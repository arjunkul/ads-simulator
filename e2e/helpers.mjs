/**
 * Shared opaque-box E2E helpers and domain validators for ads-simulator.
 *
 * Pure Node.js 22 ESM built-ins only — ZERO external npm dependencies, ZERO Python.
 */

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import fs from "node:fs";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
export const REPO_ROOT = path.resolve(__dirname, "..");

export const CHROME_CANDIDATES = [
  process.env.E2E_CHROME_BIN,
  process.env.CHROME_BIN,
  "/usr/bin/google-chrome",
  "/usr/bin/chromium",
  "/usr/bin/chromium-browser",
].filter(Boolean);

// Track live child processes so unexpected signals/exits never leave orphans.
const ACTIVE_CHILDREN = new Set();
let cleanupRegistered = false;

function registerProcessCleanup() {
  if (cleanupRegistered) return;
  cleanupRegistered = true;
  const killAll = () => {
    for (const proc of ACTIVE_CHILDREN) {
      try {
        if (proc.exitCode === null && proc.signalCode === null) {
          proc.kill("SIGKILL");
        }
      } catch {
        // Ignore already-exited processes
      }
    }
    ACTIVE_CHILDREN.clear();
  };
  process.once("exit", killAll);
  process.once("SIGINT", () => {
    killAll();
    process.exit(130);
  });
  process.once("SIGTERM", () => {
    killAll();
    process.exit(143);
  });
}

function trackChild(proc) {
  registerProcessCleanup();
  ACTIVE_CHILDREN.add(proc);
  proc.once("exit", () => ACTIVE_CHILDREN.delete(proc));
  return proc;
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

// ============================================================================
// Binary Resolution & Process Execution
// ============================================================================

export class BinaryNotFoundError extends Error {
  constructor(binaryName, searchedPaths) {
    const hint = `cargo build --workspace --release`;
    super(
      `Required workspace binary "${binaryName}" not found (searched: ${searchedPaths.join(", ")}). ` +
        `Build the workspace first with: ${hint}`,
    );
    this.name = "BinaryNotFoundError";
    this.binaryName = binaryName;
    this.searchedPaths = searchedPaths;
  }
}

/**
 * Resolve a compiled Cargo workspace binary strictly from target/release or target/debug.
 * Zero mocking backdoors: throws BinaryNotFoundError loudly if missing.
 *
 * @param {string} name
 * @param {{ profile?: 'release' | 'debug', repoRoot?: string }} [opts]
 * @returns {string}
 */
export function resolveBinary(name, opts = {}) {
  if (typeof name !== "string" || name.trim().length === 0) {
    throw new TypeError("resolveBinary: binary name must be a non-empty string");
  }
  const root = opts.repoRoot ?? REPO_ROOT;
  const requestedProfile = opts.profile ?? process.env.E2E_BIN_PROFILE ?? null;

  let profiles;
  if (requestedProfile !== null && requestedProfile !== "") {
    if (requestedProfile !== "release" && requestedProfile !== "debug") {
      throw new RangeError(
        `resolveBinary: profile must be "release" or "debug", got "${requestedProfile}"`,
      );
    }
    profiles = [requestedProfile];
  } else {
    profiles = ["release", "debug"];
  }

  const searched = [];
  for (const profile of profiles) {
    const candidate = path.join(root, "target", profile, name);
    searched.push(candidate);
    try {
      const stat = fs.statSync(candidate);
      if (stat.isFile()) {
        fs.accessSync(candidate, fs.constants.X_OK);
        return candidate;
      }
    } catch {
      // Not present or not executable
    }
  }

  throw new BinaryNotFoundError(name, searched);
}

/**
 * Run a workspace binary (by name) or explicit absolute executable path.
 * All real E2E tests pass workspace binary names so they always execute real compiled Rust binaries.
 *
 * @param {string} binOrName
 * @param {string[]} [args]
 * @param {{ cwd?: string, env?: NodeJS.ProcessEnv, timeoutMs?: number, stdin?: string, expectSuccess?: boolean, profile?: 'release' | 'debug' }} [opts]
 */
export async function runBinary(binOrName, args = [], opts = {}) {
  const binPath = path.isAbsolute(binOrName)
    ? binOrName
    : resolveBinary(binOrName, opts);
  const timeoutMs = opts.timeoutMs ?? 30_000;
  const start = Date.now();

  return await new Promise((resolve, reject) => {
    const proc = trackChild(
      spawn(binPath, args, {
        cwd: opts.cwd ?? REPO_ROOT,
        env: opts.env ?? process.env,
        stdio: ["pipe", "pipe", "pipe"],
      }),
    );

    const stdoutChunks = [];
    const stderrChunks = [];
    let settled = false;
    let killEscalation = null;

    proc.stdout.on("data", (chunk) => stdoutChunks.push(chunk));
    proc.stderr.on("data", (chunk) => stderrChunks.push(chunk));

    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      try {
        proc.kill("SIGTERM");
      } catch {
        // ignore
      }
      killEscalation = setTimeout(() => {
        try {
          if (proc.exitCode === null && proc.signalCode === null) {
            proc.kill("SIGKILL");
          }
        } catch {
          // ignore
        }
      }, 2000);
      killEscalation.unref?.();

      const partialOut = Buffer.concat(stdoutChunks).toString("utf8");
      const partialErr = Buffer.concat(stderrChunks).toString("utf8");
      reject(
        new Error(
          `Process timed out after ${timeoutMs}ms: ${binPath} ${args.join(" ")}\n` +
            `--- stdout ---\n${partialOut}\n--- stderr ---\n${partialErr}`,
        ),
      );
    }, timeoutMs);

    proc.on("error", (err) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      if (killEscalation) clearTimeout(killEscalation);
      reject(new Error(`Failed to spawn ${binPath}: ${err.message}`));
    });

    proc.on("close", (code, signal) => {
      clearTimeout(timer);
      if (killEscalation) clearTimeout(killEscalation);
      if (settled) return;
      settled = true;

      const stdout = Buffer.concat(stdoutChunks).toString("utf8");
      const stderr = Buffer.concat(stderrChunks).toString("utf8");
      const durationMs = Date.now() - start;
      const result = { code, signal, stdout, stderr, durationMs, binPath, args: [...args] };

      if (opts.expectSuccess && (code !== 0 || signal !== null)) {
        reject(
          new Error(
            `Expected exit code 0 from ${path.basename(binPath)} ${args.join(" ")}, ` +
              `got code=${code} signal=${signal} (${durationMs}ms)\n` +
              `--- stdout ---\n${stdout}\n--- stderr ---\n${stderr}`,
          ),
        );
        return;
      }

      resolve(result);
    });

    if (opts.stdin !== undefined) {
      proc.stdin.write(opts.stdin);
    }
    proc.stdin.end();
  });
}

/**
 * Convenience wrapper over runBinary that asserts exit code 0 and null signal.
 */
export async function runBinaryOk(binOrName, args = [], opts = {}) {
  return await runBinary(binOrName, args, { ...opts, expectSuccess: true });
}

/**
 * Run a cargo command in the repository root.
 */
export async function runCargo(args = [], opts = {}) {
  const cargoBin = process.env.CARGO ?? "cargo";
  const timeoutMs = opts.timeoutMs ?? 180_000;
  const start = Date.now();

  return await new Promise((resolve, reject) => {
    const proc = trackChild(
      spawn(cargoBin, args, {
        cwd: opts.cwd ?? REPO_ROOT,
        env: opts.env ?? process.env,
        stdio: ["ignore", "pipe", "pipe"],
      }),
    );

    const stdoutChunks = [];
    const stderrChunks = [];
    let settled = false;

    proc.stdout.on("data", (c) => stdoutChunks.push(c));
    proc.stderr.on("data", (c) => stderrChunks.push(c));

    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      try {
        proc.kill("SIGTERM");
      } catch {
        // ignore
      }
      setTimeout(() => {
        try {
          if (proc.exitCode === null && proc.signalCode === null) proc.kill("SIGKILL");
        } catch {
          // ignore
        }
      }, 2000).unref?.();
      reject(
        new Error(
          `cargo ${args.join(" ")} timed out after ${timeoutMs}ms\n` +
            `--- stderr ---\n${Buffer.concat(stderrChunks).toString("utf8")}`,
        ),
      );
    }, timeoutMs);

    proc.on("error", (err) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      reject(err);
    });

    proc.on("close", (code, signal) => {
      clearTimeout(timer);
      if (settled) return;
      settled = true;
      const stdout = Buffer.concat(stdoutChunks).toString("utf8");
      const stderr = Buffer.concat(stderrChunks).toString("utf8");
      const durationMs = Date.now() - start;
      if (opts.expectSuccess && (code !== 0 || signal !== null)) {
        reject(
          new Error(
            `cargo ${args.join(" ")} failed with code=${code} signal=${signal}\n` +
              `--- stdout ---\n${stdout}\n--- stderr ---\n${stderr}`,
          ),
        );
        return;
      }
      resolve({ code, signal, stdout, stderr, durationMs });
    });
  });
}

// ============================================================================
// Filesystem & Temp Directory Helpers
// ============================================================================

/**
 * Create an isolated temporary directory, invoke fn(dir), and guarantee recursive cleanup.
 *
 * @template T
 * @param {string | ((dir: string) => Promise<T> | T)} prefixOrFn
 * @param {(dir: string) => Promise<T> | T} [maybeFn]
 * @returns {Promise<T>}
 */
export async function withTempDir(prefixOrFn, maybeFn) {
  let prefix = "ads-e2e-";
  let fn = prefixOrFn;
  if (typeof prefixOrFn === "string") {
    prefix = prefixOrFn;
    fn = maybeFn;
  }
  if (typeof fn !== "function") {
    throw new TypeError("withTempDir: callback function is required");
  }
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
  try {
    return await fn(dir);
  } finally {
    if (process.env.E2E_KEEP_TMP !== "1") {
      fs.rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    }
  }
}

/**
 * Recursively list relative POSIX file paths under rootDir, pruning excluded directory names.
 *
 * @param {string} [rootDir]
 * @param {Set<string>} [pruneDirs]
 * @returns {string[]}
 */
export function listRepoFiles(
  rootDir = REPO_ROOT,
  pruneDirs = new Set([".git", ".agents", "target"]),
) {
  const results = [];
  function walk(absDir, relPrefix) {
    const entries = fs.readdirSync(absDir, { withFileTypes: true });
    for (const entry of entries) {
      if (entry.isDirectory() && pruneDirs.has(entry.name)) {
        continue;
      }
      const absPath = path.join(absDir, entry.name);
      const relPath = relPrefix ? `${relPrefix}/${entry.name}` : entry.name;
      if (entry.isDirectory()) {
        walk(absPath, relPath);
      } else {
        results.push(relPath);
      }
    }
  }
  walk(rootDir, "");
  results.sort();
  return results;
}

/**
 * Recursively list all directory relative paths under rootDir (pruning excluded dirs).
 */
export function listRepoDirs(
  rootDir = REPO_ROOT,
  pruneDirs = new Set([".git", ".agents", "target"]),
) {
  const results = [];
  function walk(absDir, relPrefix) {
    const entries = fs.readdirSync(absDir, { withFileTypes: true });
    for (const entry of entries) {
      if (!entry.isDirectory()) continue;
      if (pruneDirs.has(entry.name)) continue;
      const absPath = path.join(absDir, entry.name);
      const relPath = relPrefix ? `${relPrefix}/${entry.name}` : entry.name;
      results.push(relPath);
      walk(absPath, relPath);
    }
  }
  walk(rootDir, "");
  results.sort();
  return results;
}

export function readJsonFile(filePath) {
  const raw = fs.readFileSync(filePath, "utf8");
  return JSON.parse(raw);
}

// ============================================================================
// HTTP Helpers (Node 22 Native fetch)
// ============================================================================

/**
 * Low-level HTTP helper built on Node 22 global fetch with timeout.
 */
export async function httpRequest(url, opts = {}) {
  const timeoutMs = opts.timeoutMs ?? 15_000;
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);

  try {
    const res = await fetch(url, {
      method: opts.method ?? "GET",
      headers: opts.headers,
      body: opts.body,
      signal: controller.signal,
    });
    const text = await res.text();
    const contentType = res.headers.get("content-type") ?? "";
    return {
      status: res.status,
      ok: res.ok,
      headers: res.headers,
      contentType,
      text,
      json() {
        return JSON.parse(text);
      },
    };
  } finally {
    clearTimeout(timer);
  }
}

export async function httpGetText(url, opts = {}) {
  return await httpRequest(url, { ...opts, method: "GET" });
}

export async function httpGetJson(url, opts = {}) {
  const res = await httpRequest(url, {
    ...opts,
    method: "GET",
    headers: { Accept: "application/json", ...(opts.headers ?? {}) },
  });
  let data = null;
  let parseError = null;
  if (res.text.trim().length > 0) {
    try {
      data = JSON.parse(res.text);
    } catch (err) {
      parseError = err;
    }
  }
  if (parseError && opts.strictJson !== false && res.ok) {
    throw new Error(
      `Expected valid JSON from GET ${url} (status ${res.status}): ${parseError.message}\nBody: ${res.text.slice(0, 500)}`,
    );
  }
  return { ...res, data, parseError };
}

export async function httpPostJson(url, payload, opts = {}) {
  const body = JSON.stringify(payload);
  return await httpPostRaw(url, body, "application/json", opts);
}

export async function httpPostRaw(url, rawBody, contentType = "application/json", opts = {}) {
  const res = await httpRequest(url, {
    ...opts,
    method: "POST",
    headers: {
      "Content-Type": contentType,
      Accept: "application/json",
      ...(opts.headers ?? {}),
    },
    body: rawBody,
  });
  let data = null;
  let parseError = null;
  if (res.text.trim().length > 0) {
    try {
      data = JSON.parse(res.text);
    } catch (err) {
      parseError = err;
    }
  }
  if (parseError && opts.strictJson !== false && res.ok) {
    throw new Error(
      `Expected valid JSON from POST ${url} (status ${res.status}): ${parseError.message}\nBody: ${res.text.slice(0, 500)}`,
    );
  }
  return { ...res, data, parseError };
}

// ============================================================================
// Dashboard & Static HTTP Server Lifecycle Helpers
// ============================================================================

/**
 * Spawn `ads-simulator-dashboard` on an ephemeral localhost port (default `--host 127.0.0.1 --port 0`),
 * parse bound URL from stdout, verify GET /health returns 200 {"status":"ok"}, and return handle.
 *
 * @param {string[]} [args]
 * @param {{ binPath?: string, cwd?: string, env?: NodeJS.ProcessEnv, startupTimeoutMs?: number, verifyHealth?: boolean, profile?: 'release' | 'debug' }} [opts]
 */
export async function startDashboardServer(args = [], opts = {}) {
  const binPath = opts.binPath ?? resolveBinary("ads-simulator-dashboard", opts);
  const fullArgs = [...args];
  if (!fullArgs.includes("--port")) {
    fullArgs.unshift("--port", "0");
  }
  if (!fullArgs.includes("--host")) {
    fullArgs.unshift("--host", "127.0.0.1");
  }

  const startupTimeoutMs = opts.startupTimeoutMs ?? 15_000;
  const proc = trackChild(
    spawn(binPath, fullArgs, {
      cwd: opts.cwd ?? REPO_ROOT,
      env: opts.env ?? process.env,
      stdio: ["ignore", "pipe", "pipe"],
    }),
  );

  let stdout = "";
  let stderr = "";
  proc.stdout.on("data", (c) => {
    stdout += c.toString("utf8");
  });
  proc.stderr.on("data", (c) => {
    stderr += c.toString("utf8");
  });

  const exitPromise = new Promise((resolve) => {
    proc.once("exit", (code, signal) => resolve({ code, signal }));
  });

  async function stop() {
    if (proc.exitCode !== null || proc.signalCode !== null) {
      return;
    }
    try {
      proc.kill("SIGTERM");
    } catch {
      return;
    }
    await Promise.race([
      exitPromise,
      sleep(3000).then(() => {
        if (proc.exitCode === null && proc.signalCode === null) {
          try {
            proc.kill("SIGKILL");
          } catch {
            // ignore
          }
        }
      }),
    ]);
    await exitPromise;
  }

  // Wait for bound URL printed to stdout
  const urlRegex = /https?:\/\/(?:127\.0\.0\.1|localhost|0\.0\.0\.0|\[::1\]):(\d+)/;
  const boundUrl = await new Promise((resolve, reject) => {
    let done = false;
    const finish = (err, val) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      proc.stdout.off("data", onData);
      proc.off("exit", onExit);
      proc.off("error", onError);
      if (err) reject(err);
      else resolve(val);
    };

    const onData = () => {
      const m = stdout.match(urlRegex);
      if (m) {
        const normalized = m[0].replace("0.0.0.0", "127.0.0.1");
        finish(null, normalized);
      }
    };

    const onExit = (code, signal) => {
      finish(
        new Error(
          `Dashboard server exited prematurely before printing bound URL (code=${code}, signal=${signal})\n` +
            `Command: ${binPath} ${fullArgs.join(" ")}\n` +
            `--- stdout ---\n${stdout}\n--- stderr ---\n${stderr}`,
        ),
      );
    };

    const onError = (err) => {
      finish(new Error(`Failed to start dashboard server: ${err.message}`));
    };

    const timer = setTimeout(() => {
      finish(
        new Error(
          `Timed out after ${startupTimeoutMs}ms waiting for dashboard server bound URL on stdout.\n` +
            `--- stdout ---\n${stdout}\n--- stderr ---\n${stderr}`,
        ),
      );
    }, startupTimeoutMs);

    proc.stdout.on("data", onData);
    proc.once("exit", onExit);
    proc.once("error", onError);
    onData();
  }).catch(async (err) => {
    await stop();
    throw err;
  });

  const parsed = new URL(boundUrl);
  const url = `${parsed.protocol}//${parsed.host}`;
  const host = parsed.hostname;
  const port = Number(parsed.port);

  if (opts.verifyHealth !== false) {
    const deadline = Date.now() + 4000;
    let lastErr = null;
    let healthy = false;
    while (Date.now() < deadline) {
      if (proc.exitCode !== null || proc.signalCode !== null) {
        lastErr = new Error(
          `Dashboard server exited during health check (code=${proc.exitCode})\n--- stderr ---\n${stderr}`,
        );
        break;
      }
      try {
        const res = await httpGetJson(`${url}/health`, { timeoutMs: 2000, strictJson: false });
        if (res.status === 200 && res.data && res.data.status === "ok") {
          healthy = true;
          break;
        }
        lastErr = new Error(
          `GET ${url}/health returned status=${res.status}, body=${res.text.slice(0, 300)}`,
        );
      } catch (err) {
        lastErr = err;
      }
      await sleep(80);
    }
    if (!healthy) {
      await stop();
      throw new Error(
        `Dashboard server failed /health check at ${url}/health: ${lastErr?.message ?? "unknown"}\n` +
          `--- stdout ---\n${stdout}\n--- stderr ---\n${stderr}`,
      );
    }
  }

  return {
    url,
    baseUrl: url,
    host,
    port,
    proc,
    getStdout: () => stdout,
    getStderr: () => stderr,
    stop,
    close: stop,
  };
}

const STATIC_MIME_TYPES = {
  ".html": "text/html; charset=utf-8",
  ".htm": "text/html; charset=utf-8",
  ".js": "application/javascript; charset=utf-8",
  ".mjs": "application/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".svg": "image/svg+xml",
  ".csv": "text/csv; charset=utf-8",
  ".txt": "text/plain; charset=utf-8",
  ".png": "image/png",
  ".ico": "image/x-icon",
};

/**
 * Pure node:http static file server bound to 127.0.0.1:0.
 * Logs every request { method, path, status } and returns 404 JSON on /api/*.
 *
 * @param {string} rootDir
 */
export async function startStaticServer(rootDir) {
  const absRoot = path.resolve(rootDir);
  const requests = [];

  const server = http.createServer((req, res) => {
    const rawUrl = req.url ?? "/";
    let reqPath;
    try {
      reqPath = decodeURIComponent(new URL(rawUrl, "http://127.0.0.1").pathname);
    } catch {
      requests.push({ method: req.method ?? "GET", path: rawUrl, status: 400 });
      res.writeHead(400, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ error: "bad request" }));
      return;
    }

    if (reqPath === "/api" || reqPath.startsWith("/api/")) {
      requests.push({ method: req.method ?? "GET", path: reqPath, status: 404 });
      res.writeHead(404, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ error: "api unavailable in static mode" }));
      return;
    }

    const relPath = reqPath === "/" ? "index.html" : reqPath.replace(/^\/+/, "");
    const targetPath = path.resolve(absRoot, relPath);
    if (targetPath !== absRoot && !targetPath.startsWith(absRoot + path.sep)) {
      requests.push({ method: req.method ?? "GET", path: reqPath, status: 403 });
      res.writeHead(403, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ error: "forbidden" }));
      return;
    }

    let fileToServe = targetPath;
    try {
      const st = fs.statSync(fileToServe);
      if (st.isDirectory()) {
        fileToServe = path.join(fileToServe, "index.html");
      }
      const data = fs.readFileSync(fileToServe);
      const ext = path.extname(fileToServe).toLowerCase();
      const contentType = STATIC_MIME_TYPES[ext] ?? "application/octet-stream";
      requests.push({ method: req.method ?? "GET", path: reqPath, status: 200 });
      res.writeHead(200, { "Content-Type": contentType });
      res.end(data);
    } catch {
      requests.push({ method: req.method ?? "GET", path: reqPath, status: 404 });
      res.writeHead(404, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ error: "not found" }));
    }
  });

  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      server.off("error", reject);
      resolve();
    });
  });

  const addr = server.address();
  const port = typeof addr === "object" && addr ? addr.port : 0;
  const url = `http://127.0.0.1:${port}`;

  const stop = async () => {
    await new Promise((resolve) => server.close(() => resolve()));
  };

  return {
    url,
    baseUrl: url,
    port,
    requests,
    stop,
    close: stop,
  };
}

// ============================================================================
// Headless Chrome CDP Helpers (Pure Node 22 WebSocket + fetch)
// ============================================================================

/**
 * Locate an installed Chrome or Chromium binary. Throws loudly if none found.
 *
 * @param {{ throwIfMissing?: boolean }} [opts]
 * @returns {string | null}
 */
export function findChromeBinary(opts = {}) {
  for (const candidate of CHROME_CANDIDATES) {
    try {
      fs.accessSync(candidate, fs.constants.X_OK);
      return candidate;
    } catch {
      // continue
    }
  }
  if (opts.throwIfMissing !== false) {
    throw new Error(
      `Headless Chrome binary not found (checked: ${CHROME_CANDIDATES.join(", ")}). ` +
        `Set E2E_CHROME_BIN or install google-chrome.`,
    );
  }
  return null;
}

/**
 * Open a real headless Chrome CDP session over Node 22 native WebSocket.
 * Implements verified clean teardown: Browser.close -> wait exit (SIGKILL fallback) -> close WS -> rmSync with retries.
 *
 * @param {string | null} [initialUrl]
 * @param {{ viewport?: { width: number, height: number }, timeoutMs?: number }} [opts]
 */
export async function openBrowserSession(initialUrl = null, opts = {}) {
  const chromeBin = findChromeBinary({ throwIfMissing: true });
  const viewport = opts.viewport ?? { width: 1440, height: 900 };
  const startupTimeoutMs = opts.timeoutMs ?? 20_000;
  const userDataDir = fs.mkdtempSync(path.join(os.tmpdir(), "ads-chrome-"));

  const proc = trackChild(
    spawn(
      chromeBin,
      [
        "--headless=new",
        "--no-sandbox",
        "--disable-gpu",
        "--disable-dev-shm-usage",
        "--disable-extensions",
        "--no-first-run",
        "--disable-background-networking",
        "--remote-debugging-port=0",
        `--user-data-dir=${userDataDir}`,
        "about:blank",
      ],
      { stdio: ["ignore", "pipe", "pipe"] },
    ),
  );

  let stderrBuf = "";
  proc.stderr.on("data", (c) => {
    stderrBuf += c.toString("utf8");
  });

  const exitPromise = new Promise((resolve) => {
    proc.once("exit", (code, signal) => resolve({ code, signal }));
  });

  let ws = null;
  let closed = false;
  let nextCmdId = 1;
  const pending = new Map();
  const eventListeners = new Map();

  const consoleErrors = [];
  const pageErrors = [];
  const logErrors = [];
  const failedRequests = [];
  const networkRequests = [];
  const requestUrlById = new Map();

  function emitEvent(method, params) {
    const handlers = eventListeners.get(method);
    if (handlers) {
      for (const h of [...handlers]) {
        try {
          h(params);
        } catch {
          // ignore listener errors
        }
      }
    }
  }

  function onEvent(method, handler) {
    if (!eventListeners.has(method)) eventListeners.set(method, new Set());
    eventListeners.get(method).add(handler);
    return () => eventListeners.get(method)?.delete(handler);
  }

  function sendCommand(method, params = {}, timeoutMs = 15_000) {
    if (!ws || ws.readyState !== WebSocket.OPEN) {
      return Promise.reject(new Error(`CDP WebSocket is not open (command: ${method})`));
    }
    const id = nextCmdId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        pending.delete(id);
        reject(new Error(`CDP command timed out after ${timeoutMs}ms: ${method}`));
      }, timeoutMs);
      pending.set(id, { resolve, reject, timer, method });
      ws.send(JSON.stringify({ id, method, params }));
    });
  }

  async function close() {
    if (closed) return;
    closed = true;
    for (const [, p] of pending) {
      clearTimeout(p.timer);
      p.reject(new Error("CDP session closed"));
    }
    pending.clear();

    // Verified clean teardown order:
    // 1. Send Browser.close over WebSocket
    try {
      if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ id: nextCmdId++, method: "Browser.close", params: {} }));
      }
    } catch {
      // ignore
    }

    // 2. Wait for Chrome process to exit (3s timeout -> SIGKILL) and await exit
    if (proc.exitCode === null && proc.signalCode === null) {
      await Promise.race([
        exitPromise,
        sleep(3000).then(() => {
          if (proc.exitCode === null && proc.signalCode === null) {
            try {
              proc.kill("SIGKILL");
            } catch {
              // ignore
            }
          }
        }),
      ]);
      await exitPromise;
    }

    // 3. Close WebSocket
    try {
      ws?.close();
    } catch {
      // ignore
    }

    // 4. Recursively remove temp userDataDir with retries
    fs.rmSync(userDataDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }

  try {
    // Wait for DevTools listening URL on stderr
    const debugWsUrl = await new Promise((resolve, reject) => {
      let done = false;
      const finish = (err, val) => {
        if (done) return;
        done = true;
        clearTimeout(timer);
        proc.stderr.off("data", onStderr);
        proc.off("exit", onExit);
        if (err) reject(err);
        else resolve(val);
      };
      const onStderr = () => {
        const m = stderrBuf.match(/DevTools listening on (ws:\/\/[^\s]+)/);
        if (m) finish(null, m[1]);
      };
      const onExit = (code, signal) => {
        finish(
          new Error(
            `Chrome exited before DevTools became ready (code=${code}, signal=${signal})\n${stderrBuf}`,
          ),
        );
      };
      const timer = setTimeout(() => {
        finish(new Error(`Timed out waiting for Chrome DevTools URL\n${stderrBuf}`));
      }, startupTimeoutMs);

      proc.stderr.on("data", onStderr);
      proc.once("exit", onExit);
      onStderr();
    });

    const debugPort = new URL(debugWsUrl).port;
    // Discover page target
    let pageTarget = null;
    const deadline = Date.now() + 5000;
    while (Date.now() < deadline) {
      const listRes = await httpGetJson(`http://127.0.0.1:${debugPort}/json/list`, {
        timeoutMs: 3000,
      });
      if (Array.isArray(listRes.data)) {
        pageTarget = listRes.data.find((t) => t.type === "page" && t.webSocketDebuggerUrl);
        if (pageTarget) break;
      }
      await sleep(80);
    }
    if (!pageTarget) {
      throw new Error(`No Chrome page target found on DevTools port ${debugPort}`);
    }

    ws = new WebSocket(pageTarget.webSocketDebuggerUrl);
    await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error("Timed out connecting to Chrome CDP WebSocket")), 10_000);
      ws.addEventListener("open", () => {
        clearTimeout(timer);
        resolve();
      });
      ws.addEventListener("error", (ev) => {
        clearTimeout(timer);
        reject(new Error(`Chrome CDP WebSocket error: ${ev?.message ?? "connection failed"}`));
      });
    });

    ws.addEventListener("message", (event) => {
      let msg;
      try {
        msg = JSON.parse(String(event.data));
      } catch {
        return;
      }
      if (typeof msg.id === "number" && pending.has(msg.id)) {
        const entry = pending.get(msg.id);
        pending.delete(msg.id);
        clearTimeout(entry.timer);
        if (msg.error) {
          entry.reject(new Error(`CDP error in ${entry.method}: ${msg.error.message}`));
        } else {
          entry.resolve(msg.result);
        }
        return;
      }
      if (typeof msg.method === "string") {
        emitEvent(msg.method, msg.params ?? {});
      }
    });

    // Subscribe to runtime, log, and network diagnostics
    onEvent("Runtime.consoleAPICalled", (params) => {
      if (params.type === "error") {
        const text = (params.args ?? [])
          .map((a) => a.value ?? a.description ?? a.type)
          .join(" ");
        consoleErrors.push(text || "console.error");
      }
    });

    onEvent("Runtime.exceptionThrown", (params) => {
      const d = params.exceptionDetails ?? {};
      const msg = d.exception?.description || d.text || "Uncaught exception";
      pageErrors.push(msg);
    });

    onEvent("Log.entryAdded", (params) => {
      if (params.entry?.level === "error") {
        logErrors.push(params.entry.text ?? "log error");
      }
    });

    onEvent("Network.requestWillBeSent", (params) => {
      if (params.requestId && params.request?.url) {
        requestUrlById.set(params.requestId, {
          url: params.request.url,
          method: params.request.method ?? "GET",
          resourceType: params.type ?? "Other",
        });
      }
    });

    onEvent("Network.responseReceived", (params) => {
      const url = params.response?.url ?? requestUrlById.get(params.requestId)?.url ?? "";
      const method = requestUrlById.get(params.requestId)?.method ?? "GET";
      const status = params.response?.status ?? 0;
      const resourceType = params.type ?? requestUrlById.get(params.requestId)?.resourceType ?? "Other";
      networkRequests.push({ url, method, status, resourceType });
      if (status >= 400 && !url.endsWith("/favicon.ico")) {
        failedRequests.push({ url, method, status });
      }
    });

    onEvent("Network.loadingFailed", (params) => {
      if (params.canceled) return;
      const info = requestUrlById.get(params.requestId);
      const url = info?.url ?? "";
      if (url.startsWith("data:") || url.endsWith("/favicon.ico")) return;
      failedRequests.push({
        url,
        method: info?.method ?? "GET",
        errorText: params.errorText ?? "loadingFailed",
      });
    });

    await sendCommand("Page.enable");
    await sendCommand("Runtime.enable");
    await sendCommand("Log.enable");
    await sendCommand("Network.enable");
    await sendCommand("Emulation.setDeviceMetricsOverride", {
      width: viewport.width,
      height: viewport.height,
      deviceScaleFactor: 1,
      mobile: viewport.width < 600,
    });

    async function navigate(targetUrl, navOpts = {}) {
      const navTimeout = navOpts.timeoutMs ?? 15_000;
      const loadPromise = new Promise((resolve, reject) => {
        const timer = setTimeout(() => {
          off();
          reject(new Error(`Timed out waiting for Page.loadEventFired on ${targetUrl}`));
        }, navTimeout);
        const off = onEvent("Page.loadEventFired", () => {
          clearTimeout(timer);
          off();
          resolve();
        });
      });
      const navRes = await sendCommand("Page.navigate", { url: targetUrl }, navTimeout);
      if (navRes?.errorText) {
        throw new Error(`Page.navigate failed for ${targetUrl}: ${navRes.errorText}`);
      }
      await loadPromise;
      if (navOpts.settleMs) {
        await sleep(navOpts.settleMs);
      }
    }

    async function evalExpr(expression, evalOpts = {}) {
      const res = await sendCommand(
        "Runtime.evaluate",
        {
          expression,
          awaitPromise: evalOpts.awaitPromise !== false,
          returnByValue: evalOpts.returnByValue !== false,
        },
        evalOpts.timeoutMs ?? 15_000,
      );
      if (res?.exceptionDetails) {
        const msg =
          res.exceptionDetails.exception?.description ||
          res.exceptionDetails.text ||
          "JS evaluation exception";
        throw new Error(`Browser eval threw exception: ${msg}`);
      }
      return res?.result?.value;
    }

    async function waitFor(expression, waitTimeoutMs = 10_000, pollMs = 100) {
      const deadlineMs = Date.now() + waitTimeoutMs;
      let lastVal;
      let lastErr = null;
      while (Date.now() < deadlineMs) {
        try {
          lastVal = await evalExpr(expression, { timeoutMs: Math.min(5000, waitTimeoutMs) });
          if (lastVal) return lastVal;
        } catch (err) {
          lastErr = err;
        }
        await sleep(pollMs);
      }
      throw new Error(
        `Timed out after ${waitTimeoutMs}ms waiting for expression: ${expression}\n` +
          `lastVal=${JSON.stringify(lastVal)}, lastErr=${lastErr?.message ?? "none"}\n` +
          `consoleErrors=${JSON.stringify(consoleErrors)}\n` +
          `pageErrors=${JSON.stringify(pageErrors)}\n` +
          `failedRequests=${JSON.stringify(failedRequests)}`,
      );
    }

    function clearEvents() {
      consoleErrors.length = 0;
      pageErrors.length = 0;
      logErrors.length = 0;
      failedRequests.length = 0;
      networkRequests.length = 0;
    }

    const session = {
      consoleErrors,
      pageErrors,
      logErrors,
      failedRequests,
      networkRequests,
      sendCommand,
      navigate,
      eval: evalExpr,
      waitFor,
      clearEvents,
      close,
    };

    if (initialUrl) {
      await navigate(initialUrl);
    }

    return session;
  } catch (err) {
    await close();
    throw err;
  }
}

/**
 * One-shot page inspection in headless Chrome via CDP.
 *
 * @param {string} url
 * @param {{
 *   timeoutMs?: number,
 *   waitForSelector?: string,
 *   waitForExpression?: string,
 *   evaluateExpression?: string,
 *   viewport?: { width: number, height: number },
 *   settleMs?: number,
 * }} [opts]
 */
export async function inspectPageWithChrome(url, opts = {}) {
  const session = await openBrowserSession(null, {
    viewport: opts.viewport,
    timeoutMs: opts.timeoutMs ?? 20_000,
  });
  try {
    await session.navigate(url, { timeoutMs: opts.timeoutMs ?? 15_000 });
    if (opts.waitForSelector) {
      const sel = JSON.stringify(opts.waitForSelector);
      await session.waitFor(`Boolean(document.querySelector(${sel}))`, opts.timeoutMs ?? 10_000);
    }
    if (opts.waitForExpression) {
      await session.waitFor(opts.waitForExpression, opts.timeoutMs ?? 10_000);
    }
    const settleMs = opts.settleMs ?? 150;
    if (settleMs > 0) {
      await sleep(settleMs);
    }
    const title = await session.eval("document.title");
    const html = await session.eval("document.documentElement.outerHTML");
    let evalResult = undefined;
    if (opts.evaluateExpression) {
      evalResult = await session.eval(opts.evaluateExpression);
    }
    return {
      url,
      title,
      html,
      evalResult,
      consoleErrors: [...session.consoleErrors],
      pageErrors: [...session.pageErrors],
      logErrors: [...session.logErrors],
      failedRequests: [...session.failedRequests],
      networkRequests: [...session.networkRequests],
    };
  } finally {
    await session.close();
  }
}

/**
 * Lightweight DOM dump using Chrome --dump-dom.
 */
export async function dumpDomWithChrome(url, opts = {}) {
  const chromeBin = findChromeBinary({ throwIfMissing: true });
  const res = await runBinaryOk(
    chromeBin,
    ["--headless=new", "--no-sandbox", "--disable-gpu", "--disable-dev-shm-usage", "--dump-dom", url],
    { timeoutMs: opts.timeoutMs ?? 20_000 },
  );
  return res.stdout;
}

/**
 * Reusable browser-side semantic locator functions (zero private CSS class coupling).
 * Include inside a Runtime.evaluate IIFE when interacting with the dashboard UI.
 */
export const BROWSER_UI_HELPERS_SRC = `
  function isVisible(el) {
    if (!el || el.nodeType !== 1) return false;
    const style = window.getComputedStyle(el);
    if (style.display === 'none' || style.visibility === 'hidden' || Number(style.opacity) === 0) return false;
    const rect = el.getBoundingClientRect();
    return rect.width > 0 && rect.height > 0;
  }

  function getLabelText(el) {
    const parts = [];
    if (el.getAttribute('aria-label')) parts.push(el.getAttribute('aria-label'));
    if (el.getAttribute('placeholder')) parts.push(el.getAttribute('placeholder'));
    if (el.getAttribute('name')) parts.push(el.getAttribute('name'));
    if (el.id) {
      parts.push(el.id);
      const lbl = document.querySelector('label[for="' + CSS.escape(el.id) + '"]');
      if (lbl) parts.push(lbl.textContent || '');
    }
    const parentLabel = el.closest('label');
    if (parentLabel) parts.push(parentLabel.textContent || '');
    return parts.join(' ').trim();
  }

  function setInputValue(el, value) {
    const proto = el.tagName === 'SELECT'
      ? HTMLSelectElement.prototype
      : HTMLInputElement.prototype;
    const desc = Object.getOwnPropertyDescriptor(proto, 'value');
    if (desc && desc.set) desc.set.call(el, String(value));
    else el.value = String(value);
    el.dispatchEvent(new Event('input', { bubbles: true }));
    el.dispatchEvent(new Event('change', { bubbles: true }));
  }

  function setCheckbox(el, checked) {
    if (el.checked !== Boolean(checked)) {
      el.checked = Boolean(checked);
      el.dispatchEvent(new Event('input', { bubbles: true }));
      el.dispatchEvent(new Event('change', { bubbles: true }));
    }
  }

  function findSearchInput() {
    const searchType = Array.from(document.querySelectorAll('input[type="search"]')).find(isVisible);
    if (searchType) return searchType;
    const candidates = Array.from(
      document.querySelectorAll('input:not([type="hidden"]):not([type="number"]):not([type="checkbox"]):not([type="radio"]):not([type="submit"]):not([type="button"])')
    ).filter(isVisible);
    return candidates.find((el) => /search|filter|piece|feature|find/i.test(getLabelText(el))) || null;
  }

  function findRunnerControls() {
    const buttons = Array.from(document.querySelectorAll('button, input[type="submit"]')).filter(isVisible);
    const submitButton = buttons.find((b) => /run|simulate|execute/i.test((b.textContent || b.value || '').trim()));
    if (!submitButton) return null;
    const container = submitButton.closest('form') || submitButton.closest('section') || submitButton.parentElement?.parentElement || document.body;
    const inputs = Array.from(container.querySelectorAll('input, select')).filter((el) => el.type !== 'hidden' && el.type !== 'submit' && el.type !== 'button');
    const findBy = (re, excludeRe = null) => inputs.find((el) => {
      const txt = getLabelText(el);
      return re.test(txt) && (!excludeRe || !excludeRe.test(txt));
    }) || null;
    return {
      form: submitButton.closest('form'),
      container,
      submitButton,
      seed: findBy(/\\bseed\\b|sim.?seed/i, /marketplace/i),
      marketplaceSeed: findBy(/marketplace.*seed/i),
      budget: findBy(/budget/i),
      cpc: findBy(/cpc/i),
      impressions: findBy(/impression/i, /natural|replay|no.?target|single/i),
      naturalToggle: inputs.find((el) => el.type === 'checkbox' && /natural|replay|no.?target|single|unbounded/i.test(getLabelText(el))) || null,
    };
  }

  function normalizeWs(s) {
    return String(s || '').replace(/\\s+/g, ' ').trim();
  }

  function countVisibleCatalogPieces(pieces) {
    const nonSvgLeaves = Array.from(document.querySelectorAll('body *')).filter((el) => {
      if (el.closest('svg') || el.tagName === 'SCRIPT' || el.tagName === 'STYLE') return false;
      return isVisible(el);
    });
    const visibleTexts = nonSvgLeaves.map((el) => normalizeWs(el.textContent));
    let count = 0;
    const matched = [];
    for (const p of pieces) {
      const targetSummary = normalizeWs(p.summary);
      const found = visibleTexts.some((txt) => targetSummary.length > 0 && txt.includes(targetSummary));
      if (found) {
        count += 1;
        matched.push(p.id);
      }
    }
    return { count, matched };
  }
`;

// ============================================================================
// Formatting & Domain Helpers
// ============================================================================

function isPlainObject(v) {
  return v !== null && typeof v === "object" && !Array.isArray(v);
}

function assertNonNegInt(val, label) {
  assert.ok(
    typeof val === "number" && Number.isInteger(val) && val >= 0,
    `${label} must be a non-negative integer, got ${JSON.stringify(val)}`,
  );
}

function assertFiniteNonNeg(val, label) {
  assert.ok(
    typeof val === "number" && Number.isFinite(val) && val >= 0,
    `${label} must be a finite non-negative number, got ${JSON.stringify(val)}`,
  );
}

/**
 * Generate acceptable formatted USD string representations for an exact cent value.
 * Covers comma-grouped, ungrouped, whole-dollar (when cents % 100 === 0), and k-abbreviated forms.
 *
 * @param {number} cents
 * @returns {string[]}
 */
export function formatUsdCandidates(cents) {
  const dollars = cents / 100;
  const fixed2 = dollars.toFixed(2);
  const [intPart, decPart] = fixed2.split(".");
  const groupedInt = intPart.replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  const candidates = new Set([
    `$${groupedInt}.${decPart}`,
    `$${intPart}.${decPart}`,
  ]);
  if (cents % 100 === 0) {
    candidates.add(`$${groupedInt}`);
    candidates.add(`$${intPart}`);
  }
  if (dollars >= 1000) {
    const kVal = dollars / 1000;
    candidates.add(`$${kVal.toFixed(2)}k`);
    candidates.add(`$${kVal.toFixed(2)}K`);
    candidates.add(`$${kVal.toFixed(1)}k`);
    candidates.add(`$${kVal.toFixed(1)}K`);
    if (Number.isInteger(kVal)) {
      candidates.add(`$${kVal}k`);
      candidates.add(`$${kVal}K`);
    }
  }
  return [...candidates];
}

// ============================================================================
// Opaque-Box Validators
// ============================================================================

/**
 * Assert zero legacy/rogue .py files exist in repoRoot (excluding .git, .agents, target,
 * and the sanctioned ml/pytorch/ PyTorch CTR training subsystem introduced by ads-ml).
 */
export function assertZeroPythonFiles(repoRoot = REPO_ROOT) {
  const files = listRepoFiles(repoRoot, new Set([".git", ".agents", "target"]));
  const pyFiles = files.filter(
    (f) => f.toLowerCase().endsWith(".py") && !f.startsWith("ml/pytorch/"),
  );
  assert.deepStrictEqual(
    pyFiles,
    [],
    `Expected zero legacy .py files outside ml/pytorch/ in ${repoRoot}, found: ${pyFiles.join(", ")}`,
  );
  return files;
}

/**
 * Validate complete Schema v1 marketplace.json invariants.
 *
 * @param {unknown} mp
 * @param {{ expectedSeed?: number | null }} [opts]
 */
export function validateMarketplaceJson(mp, opts = {}) {
  assert.ok(isPlainObject(mp), "marketplace must be a JSON object");
  assert.strictEqual(mp.schema_version, 1, `marketplace.schema_version must be 1, got ${mp.schema_version}`);
  assert.strictEqual(mp.currency, "USD", `marketplace.currency must be "USD", got ${mp.currency}`);
  assert.strictEqual(typeof mp.seed, "number", `marketplace.seed must be a number, got ${typeof mp.seed}`);

  if (opts.expectedSeed !== undefined && opts.expectedSeed !== null) {
    assert.strictEqual(mp.seed, opts.expectedSeed, `marketplace.seed mismatch`);
  }

  // Categories: 100 total, 10 groups x 10 categories
  assert.ok(Array.isArray(mp.categories), "marketplace.categories must be an array");
  assert.strictEqual(mp.categories.length, 100, `expected 100 categories, got ${mp.categories.length}`);
  const categoryIds = new Set();
  const groupCounts = new Map();
  for (const cat of mp.categories) {
    assert.ok(isPlainObject(cat), "category entry must be an object");
    assert.ok(typeof cat.id === "string" && cat.id.length > 0, "category.id must be non-empty string");
    assert.ok(typeof cat.name === "string" && cat.name.length > 0, "category.name must be non-empty string");
    assert.ok(typeof cat.group === "string" && cat.group.length > 0, "category.group must be non-empty string");
    assert.ok(!categoryIds.has(cat.id), `duplicate category id: ${cat.id}`);
    categoryIds.add(cat.id);
    groupCounts.set(cat.group, (groupCounts.get(cat.group) ?? 0) + 1);
  }
  assert.strictEqual(groupCounts.size, 10, `expected 10 taxonomy groups, got ${groupCounts.size}`);
  for (const [grp, cnt] of groupCounts) {
    assert.strictEqual(cnt, 10, `taxonomy group "${grp}" must have 10 categories, got ${cnt}`);
  }

  // Advertisers: 50 total, 20 catalog items each
  assert.ok(Array.isArray(mp.advertisers), "marketplace.advertisers must be an array");
  assert.strictEqual(mp.advertisers.length, 50, `expected 50 advertisers, got ${mp.advertisers.length}`);
  const advertiserIds = new Set();
  const allItemIds = new Set();
  const coveredCatalogCats = new Set();

  for (const adv of mp.advertisers) {
    assert.ok(isPlainObject(adv), "advertiser must be an object");
    assert.ok(typeof adv.id === "string" && adv.id.length > 0, "advertiser.id must be non-empty");
    assert.ok(!advertiserIds.has(adv.id), `duplicate advertiser id: ${adv.id}`);
    advertiserIds.add(adv.id);
    assert.strictEqual(Object.hasOwn(adv, "objective"), false, `advertiser ${adv.id} must not have dropped objective field`);
    assertNonNegInt(adv.daily_budget_cents, `advertiser ${adv.id}.daily_budget_cents`);
    assert.ok(
      adv.daily_budget_cents >= 1000 && adv.daily_budget_cents <= 50000 && adv.daily_budget_cents % 100 === 0,
      `advertiser ${adv.id}.daily_budget_cents out of [1000, 50000] step 100: ${adv.daily_budget_cents}`,
    );
    assert.ok(Array.isArray(adv.catalog) && adv.catalog.length === 20, `advertiser ${adv.id} must have 20 catalog items`);
    for (const item of adv.catalog) {
      assert.ok(typeof item.id === "string" && item.id.length > 0, "catalog item id missing");
      assert.ok(!allItemIds.has(item.id), `duplicate item id: ${item.id}`);
      allItemIds.add(item.id);
      assert.ok(categoryIds.has(item.category_id), `unknown catalog item category_id: ${item.category_id}`);
      coveredCatalogCats.add(item.category_id);
      assertNonNegInt(item.price_cents, `item ${item.id}.price_cents`);
      assert.ok(
        item.price_cents >= 500 && item.price_cents <= 30000 && item.price_cents % 100 === 0,
        `item ${item.id}.price_cents out of [500, 30000] step 100: ${item.price_cents}`,
      );
    }
  }
  assert.strictEqual(coveredCatalogCats.size, 100, `advertiser catalogs must cover all 100 categories, covered ${coveredCatalogCats.size}`);

  // Organic items: 5000 total (50 per category)
  assert.ok(Array.isArray(mp.organic_items), "marketplace.organic_items must be an array");
  assert.strictEqual(mp.organic_items.length, 5000, `expected 5000 organic_items, got ${mp.organic_items.length}`);
  const organicById = new Map();
  const organicPerCat = new Map();
  for (const item of mp.organic_items) {
    assert.ok(typeof item.id === "string" && item.id.length > 0, "organic item id missing");
    assert.ok(!allItemIds.has(item.id), `duplicate item id across catalog/organic: ${item.id}`);
    allItemIds.add(item.id);
    assert.ok(categoryIds.has(item.category_id), `unknown organic category_id: ${item.category_id}`);
    assertNonNegInt(item.price_cents, `organic ${item.id}.price_cents`);
    assert.ok(
      item.price_cents >= 500 && item.price_cents <= 30000 && item.price_cents % 100 === 0,
      `organic ${item.id}.price_cents out of [500, 30000] step 100: ${item.price_cents}`,
    );
    organicById.set(item.id, item);
    organicPerCat.set(item.category_id, (organicPerCat.get(item.category_id) ?? 0) + 1);
  }
  assert.strictEqual(allItemIds.size, 6000, `expected 6000 unique item IDs, got ${allItemIds.size}`);
  for (const catId of categoryIds) {
    assert.strictEqual(organicPerCat.get(catId), 50, `category ${catId} must have 50 organic items`);
  }

  // Users: 1000 total with unique IDs (preferred categories moved to visit-simulator)
  assert.ok(Array.isArray(mp.users), "marketplace.users must be an array");
  assert.strictEqual(mp.users.length, 1000, `expected 1000 users, got ${mp.users.length}`);
  const userIdsSeen = new Set();
  for (const u of mp.users) {
    assert.ok(isPlainObject(u), "user must be an object");
    assert.ok(typeof u.id === "string" && u.id.length > 0, "user.id missing");
    assert.ok(!userIdsSeen.has(u.id), `duplicate user id: ${u.id}`);
    userIdsSeen.add(u.id);
  }

  // Browsing events: 12000 total (12 per user: 9 affinity + 3 exploration)
  assert.ok(Array.isArray(mp.browsing_events), "marketplace.browsing_events must be an array");
  assert.strictEqual(mp.browsing_events.length, 12000, `expected 12000 browsing_events, got ${mp.browsing_events.length}`);
  const eventsByUser = new Map();
  let totalAffinity = 0;
  let totalExploration = 0;
  for (const ev of mp.browsing_events) {
    assert.ok(userIdsSeen.has(ev.user_id), `browsing_event references unknown user_id: ${ev.user_id}`);
    const organic = organicById.get(ev.organic_item_id);
    assert.ok(organic, `browsing_event references unknown organic_item_id: ${ev.organic_item_id}`);
    if (ev.reason === "affinity") {
      totalAffinity += 1;
    } else if (ev.reason === "exploration") {
      totalExploration += 1;
    } else {
      assert.fail(`invalid browsing_event reason: ${ev.reason}`);
    }
    if (!eventsByUser.has(ev.user_id)) eventsByUser.set(ev.user_id, []);
    eventsByUser.get(ev.user_id).push(ev);
  }
  assert.strictEqual(totalAffinity, 9000, `expected 9000 affinity events, got ${totalAffinity}`);
  assert.strictEqual(totalExploration, 3000, `expected 3000 exploration events, got ${totalExploration}`);
  assert.strictEqual(eventsByUser.size, 1000, `expected events for all 1000 users`);
  for (const [uid, evs] of eventsByUser) {
    assert.strictEqual(evs.length, 12, `user ${uid} must have 12 browsing events`);
    const itemSet = new Set(evs.map((e) => e.organic_item_id));
    assert.strictEqual(itemSet.size, 12, `user ${uid} must browse 12 distinct organic items`);
    const positions = evs.map((e) => e.position).sort((a, b) => a - b);
    assert.deepStrictEqual(positions, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12], `user ${uid} positions must be 1..=12`);
    const aff = evs.filter((e) => e.reason === "affinity");
    const exp = evs.filter((e) => e.reason === "exploration");
    assert.strictEqual(aff.length, 9, `user ${uid} must have 9 affinity events, got ${aff.length}`);
    assert.strictEqual(exp.length, 3, `user ${uid} must have 3 exploration events, got ${exp.length}`);
    const affCats = new Set(aff.map((e) => organicById.get(e.organic_item_id).category_id));
    const expCats = new Set(exp.map((e) => organicById.get(e.organic_item_id).category_id));
    assert.ok(affCats.size <= 5, `user ${uid} affinity events must span <= 5 focus categories`);
    for (const cid of expCats) {
      assert.ok(!affCats.has(cid), `user ${uid} exploration category ${cid} must not overlap affinity categories`);
    }
  }
}

/**
 * Validate complete 30-day simulation Report (simulation.json) ledger accounting invariants.
 *
 * @param {unknown} report
 * @param {{ seed?: number, campaign_budget_cents?: number, cpc_cents?: number, impressions?: number | null }} [expectedConfig]
 */
export function validateSimulationReport(report, expectedConfig = undefined) {
  assert.ok(isPlainObject(report), "simulation report must be an object");
  assert.strictEqual(report.schema_version, 1, `report.schema_version must be 1, got ${report.schema_version}`);
  assert.strictEqual(report.currency, "USD", `report.currency must be "USD", got ${report.currency}`);
  assert.ok(isPlainObject(report.config), "report.config must be an object");
  assert.ok(isPlainObject(report.summary), "report.summary must be an object");

  const cfg = report.config;
  assertNonNegInt(cfg.seed, "config.seed");
  assert.ok(
    cfg.campaign_budget_cents === null || (Number.isInteger(cfg.campaign_budget_cents) && cfg.campaign_budget_cents >= 0),
    `config.campaign_budget_cents must be null or non-negative integer, got ${JSON.stringify(cfg.campaign_budget_cents)}`,
  );
  assertNonNegInt(cfg.campaign_budget_min_cents, "config.campaign_budget_min_cents");
  assertNonNegInt(cfg.campaign_budget_max_cents, "config.campaign_budget_max_cents");
  assert.ok(
    cfg.campaign_budget_min_cents <= cfg.campaign_budget_max_cents,
    `config.campaign_budget_min_cents (${cfg.campaign_budget_min_cents}) must be <= max (${cfg.campaign_budget_max_cents})`,
  );
  assertNonNegInt(cfg.cpc_cents, "config.cpc_cents");
  assert.ok(cfg.cpc_cents > 0, `config.cpc_cents must be > 0, got ${cfg.cpc_cents}`);
  assert.ok(
    cfg.impressions === null || (Number.isInteger(cfg.impressions) && cfg.impressions >= 0),
    `config.impressions must be null or non-negative integer, got ${cfg.impressions}`,
  );

  if (expectedConfig) {
    for (const k of [
      "seed",
      "campaign_budget_cents",
      "campaign_budget_min_cents",
      "campaign_budget_max_cents",
      "cpc_cents",
      "impressions",
    ]) {
      if (Object.hasOwn(expectedConfig, k)) {
        assert.strictEqual(cfg[k], expectedConfig[k], `report.config.${k} mismatch`);
      }
    }
  }

  assert.ok(Array.isArray(report.days) && report.days.length === 30, `report.days must have length 30, got ${report.days?.length}`);
  assert.ok(Array.isArray(report.campaigns) && report.campaigns.length === 500, `report.campaigns must have length 500, got ${report.campaigns?.length}`);

  const cpc = cfg.cpc_cents;
  const s = report.summary;
  for (const field of [
    "budget_cents",
    "remaining_budget_cents",
    "opportunities",
    "no_matching_category",
    "insufficient_budget",
    "impressions",
    "clicks",
    "spend_cents",
  ]) {
    assertNonNegInt(s[field], `summary.${field}`);
  }

  if (cfg.campaign_budget_cents !== null) {
    assert.strictEqual(s.budget_cents, report.campaigns.length * cfg.campaign_budget_cents, "summary.budget_cents must equal campaigns * campaign_budget_cents");
  } else {
    assert.ok(
      s.budget_cents >= report.campaigns.length * cfg.campaign_budget_min_cents &&
        s.budget_cents <= report.campaigns.length * cfg.campaign_budget_max_cents,
      `summary.budget_cents (${s.budget_cents}) out of expected randomized bounds`,
    );
  }
  assert.strictEqual(s.remaining_budget_cents + s.spend_cents, s.budget_cents, "summary remaining + spend must equal budget");
  assert.strictEqual(s.spend_cents, s.clicks * cpc, "summary.spend_cents must equal clicks * cpc_cents");
  assert.strictEqual(s.impressions + s.no_matching_category + s.insufficient_budget, s.opportunities, "summary opportunities accounting mismatch");
  assert.ok(s.clicks <= s.impressions, "summary.clicks cannot exceed summary.impressions");

  // Sum days
  let sumOpp = 0;
  let sumNoCat = 0;
  let sumInsuff = 0;
  let sumImp = 0;
  let sumClicks = 0;
  let sumSpend = 0;

  for (let i = 0; i < 30; i++) {
    const d = report.days[i];
    assert.strictEqual(d.day, i + 1, `days[${i}].day must be ${i + 1}`);
    for (const f of ["opportunities", "no_matching_category", "insufficient_budget", "impressions", "clicks", "spend_cents"]) {
      assertNonNegInt(d[f], `days[${i}].${f}`);
    }
    assert.strictEqual(d.impressions + d.no_matching_category + d.insufficient_budget, d.opportunities, `day ${d.day} opportunities accounting mismatch`);
    assert.strictEqual(d.spend_cents, d.clicks * cpc, `day ${d.day} spend_cents !== clicks * cpc`);
    assert.ok(d.clicks <= d.impressions, `day ${d.day} clicks > impressions`);
    if (cfg.impressions !== null) {
      const expectedQuota = Math.floor(cfg.impressions / 30) + (d.day <= cfg.impressions % 30 ? 1 : 0);
      assert.strictEqual(d.impressions, expectedQuota, `day ${d.day} impressions quota mismatch`);
    }
    sumOpp += d.opportunities;
    sumNoCat += d.no_matching_category;
    sumInsuff += d.insufficient_budget;
    sumImp += d.impressions;
    sumClicks += d.clicks;
    sumSpend += d.spend_cents;
  }

  assert.strictEqual(sumOpp, s.opportunities, "sum(days.opportunities) !== summary.opportunities");
  assert.strictEqual(sumNoCat, s.no_matching_category, "sum(days.no_matching_category) !== summary.no_matching_category");
  assert.strictEqual(sumInsuff, s.insufficient_budget, "sum(days.insufficient_budget) !== summary.insufficient_budget");
  assert.strictEqual(sumImp, s.impressions, "sum(days.impressions) !== summary.impressions");
  assert.strictEqual(sumClicks, s.clicks, "sum(days.clicks) !== summary.clicks");
  assert.strictEqual(sumSpend, s.spend_cents, "sum(days.spend_cents) !== summary.spend_cents");

  // Reconcile 500 campaigns and per-day campaign rollups
  const dayRollupImp = new Array(30).fill(0);
  const dayRollupClicks = new Array(30).fill(0);
  const dayRollupSpend = new Array(30).fill(0);
  const advCounts = new Map();
  const advItems = new Map();
  let campSumBudget = 0;
  let campSumRem = 0;
  let campSumImp = 0;
  let campSumClicks = 0;
  let campSumSpend = 0;

  for (const c of report.campaigns) {
    assert.ok(typeof c.id === "string" && c.id.length > 0, "campaign.id missing");
    assert.ok(typeof c.advertiser_id === "string" && c.advertiser_id.length > 0, "campaign.advertiser_id missing");
    assert.ok(typeof c.ad_item_id === "string" && c.ad_item_id.length > 0, "campaign.ad_item_id missing");
    advCounts.set(c.advertiser_id, (advCounts.get(c.advertiser_id) ?? 0) + 1);
    if (!advItems.has(c.advertiser_id)) advItems.set(c.advertiser_id, new Set());
    assert.ok(!advItems.get(c.advertiser_id).has(c.ad_item_id), `duplicate ad_item_id ${c.ad_item_id} for ${c.advertiser_id}`);
    advItems.get(c.advertiser_id).add(c.ad_item_id);

    assertNonNegInt(c.budget_cents, `campaign ${c.id}.budget_cents`);
    if (cfg.campaign_budget_cents !== null) {
      assert.strictEqual(c.budget_cents, cfg.campaign_budget_cents, `campaign ${c.id} budget_cents mismatch`);
    } else {
      assert.ok(
        c.budget_cents >= cfg.campaign_budget_min_cents && c.budget_cents <= cfg.campaign_budget_max_cents,
        `campaign ${c.id} budget_cents (${c.budget_cents}) out of [${cfg.campaign_budget_min_cents}, ${cfg.campaign_budget_max_cents}]`,
      );
    }
    assertNonNegInt(c.remaining_budget_cents, `campaign ${c.id}.remaining_budget_cents`);
    assertNonNegInt(c.impressions, `campaign ${c.id}.impressions`);
    assertNonNegInt(c.clicks, `campaign ${c.id}.clicks`);
    assertNonNegInt(c.spend_cents, `campaign ${c.id}.spend_cents`);
    assert.strictEqual(c.remaining_budget_cents + c.spend_cents, c.budget_cents, `campaign ${c.id} remaining + spend !== budget`);
    assert.strictEqual(c.spend_cents, c.clicks * cpc, `campaign ${c.id} spend_cents !== clicks * cpc`);
    assert.ok(Array.isArray(c.daily) && c.daily.length === 30, `campaign ${c.id}.daily must have 30 rows`);

    let cImp = 0;
    let cClicks = 0;
    let cSpend = 0;
    for (let i = 0; i < 30; i++) {
      const cd = c.daily[i];
      assert.strictEqual(cd.day, i + 1, `campaign ${c.id} daily[${i}].day mismatch`);
      assertNonNegInt(cd.impressions, `campaign ${c.id} daily[${i}].impressions`);
      assertNonNegInt(cd.clicks, `campaign ${c.id} daily[${i}].clicks`);
      assertNonNegInt(cd.spend_cents, `campaign ${c.id} daily[${i}].spend_cents`);
      assert.strictEqual(cd.spend_cents, cd.clicks * cpc, `campaign ${c.id} day ${cd.day} spend !== clicks * cpc`);
      cImp += cd.impressions;
      cClicks += cd.clicks;
      cSpend += cd.spend_cents;
      dayRollupImp[i] += cd.impressions;
      dayRollupClicks[i] += cd.clicks;
      dayRollupSpend[i] += cd.spend_cents;
    }
    assert.strictEqual(cImp, c.impressions, `campaign ${c.id} daily impressions sum mismatch`);
    assert.strictEqual(cClicks, c.clicks, `campaign ${c.id} daily clicks sum mismatch`);
    assert.strictEqual(cSpend, c.spend_cents, `campaign ${c.id} daily spend sum mismatch`);

    campSumBudget += c.budget_cents;
    campSumRem += c.remaining_budget_cents;
    campSumImp += c.impressions;
    campSumClicks += c.clicks;
    campSumSpend += c.spend_cents;
  }

  assert.strictEqual(advCounts.size, 50, `expected 50 advertisers in campaigns, got ${advCounts.size}`);
  for (const [advId, cnt] of advCounts) {
    assert.strictEqual(cnt, 10, `advertiser ${advId} must have 10 campaigns, got ${cnt}`);
  }
  assert.strictEqual(campSumBudget, s.budget_cents, "campaign budget sum !== summary.budget_cents");
  assert.strictEqual(campSumRem, s.remaining_budget_cents, "campaign remaining sum !== summary.remaining_budget_cents");
  assert.strictEqual(campSumImp, s.impressions, "campaign impressions sum !== summary.impressions");
  assert.strictEqual(campSumClicks, s.clicks, "campaign clicks sum !== summary.clicks");
  assert.strictEqual(campSumSpend, s.spend_cents, "campaign spend sum !== summary.spend_cents");

  for (let i = 0; i < 30; i++) {
    assert.strictEqual(dayRollupImp[i], report.days[i].impressions, `day ${i + 1} campaign rollup impressions mismatch`);
    assert.strictEqual(dayRollupClicks[i], report.days[i].clicks, `day ${i + 1} campaign rollup clicks mismatch`);
    assert.strictEqual(dayRollupSpend[i], report.days[i].spend_cents, `day ${i + 1} campaign rollup spend mismatch`);
  }
}

/**
 * Deep structural and mathematical validator for sim-agent's BudgetTimeSeries JSON.
 *
 * @param {unknown} ts
 * @param {{
 *   marketplace_seed?: number,
 *   sim_seed?: number,
 *   campaign_budget_cents?: number | null,
 *   campaign_budget_min_cents?: number,
 *   campaign_budget_max_cents?: number,
 *   cpc_cents?: number,
 *   impressions?: number | null,
 *   total_campaigns?: number,
 * } | null} [expectedConfig]
 */
export function validateBudgetTimeSeries(ts, expectedConfig = null) {
  assert.ok(isPlainObject(ts), "BudgetTimeSeries must be a plain object");
  assert.strictEqual(ts.schema_version, 1, `BudgetTimeSeries.schema_version must be 1, got ${ts.schema_version}`);
  assert.strictEqual(ts.currency, "USD", `BudgetTimeSeries.currency must be "USD", got ${ts.currency}`);
  assert.ok(isPlainObject(ts.config), "BudgetTimeSeries.config must be an object");
  assert.ok(isPlainObject(ts.summary), "BudgetTimeSeries.summary must be an object");

  const cfg = ts.config;
  assert.ok(
    typeof cfg.marketplace_seed === "number" && Number.isInteger(cfg.marketplace_seed),
    `config.marketplace_seed must be an integer, got ${JSON.stringify(cfg.marketplace_seed)}`,
  );
  assertNonNegInt(cfg.sim_seed, "config.sim_seed");
  assert.ok(
    cfg.campaign_budget_cents === null || (Number.isInteger(cfg.campaign_budget_cents) && cfg.campaign_budget_cents >= 0),
    `config.campaign_budget_cents must be null or non-negative integer, got ${JSON.stringify(cfg.campaign_budget_cents)}`,
  );
  assertNonNegInt(cfg.campaign_budget_min_cents, "config.campaign_budget_min_cents");
  assertNonNegInt(cfg.campaign_budget_max_cents, "config.campaign_budget_max_cents");
  assert.ok(
    cfg.campaign_budget_min_cents <= cfg.campaign_budget_max_cents,
    `config.campaign_budget_min_cents (${cfg.campaign_budget_min_cents}) must be <= max (${cfg.campaign_budget_max_cents})`,
  );
  assertNonNegInt(cfg.cpc_cents, "config.cpc_cents");
  assert.ok(cfg.cpc_cents > 0, `config.cpc_cents must be > 0, got ${cfg.cpc_cents}`);
  assert.ok(
    cfg.impressions === null || (Number.isInteger(cfg.impressions) && cfg.impressions >= 0),
    `config.impressions must be null or non-negative integer, got ${JSON.stringify(cfg.impressions)}`,
  );

  if (expectedConfig) {
    for (const key of [
      "marketplace_seed",
      "sim_seed",
      "campaign_budget_cents",
      "campaign_budget_min_cents",
      "campaign_budget_max_cents",
      "cpc_cents",
      "impressions",
    ]) {
      if (Object.hasOwn(expectedConfig, key)) {
        assert.strictEqual(cfg[key], expectedConfig[key], `BudgetTimeSeries.config.${key} mismatch`);
      }
    }
    if (expectedConfig.total_campaigns !== undefined) {
      assert.strictEqual(ts.summary.total_campaigns, expectedConfig.total_campaigns, "summary.total_campaigns mismatch");
    }
  }

  const s = ts.summary;
  for (const field of [
    "total_budget_cents",
    "total_spend_cents",
    "remaining_budget_cents",
    "total_impressions",
    "total_clicks",
    "exhausted_campaigns",
    "total_campaigns",
    "insufficient_budget_opportunities",
    "min_daily_spend_cents",
    "max_daily_spend_cents",
  ]) {
    assertNonNegInt(s[field], `summary.${field}`);
  }
  for (const field of ["utilization_pct", "ctr_pct", "mean_daily_spend_cents"]) {
    assertFiniteNonNeg(s[field], `summary.${field}`);
  }

  assert.ok(Array.isArray(ts.days) && ts.days.length === 30, `BudgetTimeSeries.days must have length 30, got ${ts.days?.length}`);

  const cpc = expectedConfig?.cpc_cents ?? cfg.cpc_cents;
  let runningSpend = 0;
  let sumImpressions = 0;
  let sumClicks = 0;
  let sumInsufficient = 0;
  let minDaily = Infinity;
  let maxDaily = -Infinity;

  for (let i = 0; i < 30; i++) {
    const d = ts.days[i];
    assert.ok(isPlainObject(d), `days[${i}] must be an object`);
    assert.strictEqual(d.day, i + 1, `days[${i}].day must be ${i + 1}, got ${d.day}`);
    for (const f of [
      "impressions",
      "clicks",
      "daily_spend_cents",
      "cumulative_spend_cents",
      "remaining_budget_cents",
      "active_campaigns_end_of_day",
      "insufficient_budget",
    ]) {
      assertNonNegInt(d[f], `days[${i}].${f}`);
    }
    assertFiniteNonNeg(d.utilization_pct, `days[${i}].utilization_pct`);
    assert.ok(d.clicks <= d.impressions, `day ${d.day}: clicks (${d.clicks}) > impressions (${d.impressions})`);
    assert.strictEqual(d.daily_spend_cents, d.clicks * cpc, `day ${d.day}: daily_spend_cents !== clicks * cpc`);

    runningSpend += d.daily_spend_cents;
    assert.strictEqual(d.cumulative_spend_cents, runningSpend, `day ${d.day}: cumulative_spend_cents mismatch`);
    if (i > 0) {
      assert.ok(
        d.cumulative_spend_cents >= ts.days[i - 1].cumulative_spend_cents,
        `day ${d.day}: cumulative_spend_cents must be monotonically non-decreasing`,
      );
      assert.ok(
        d.active_campaigns_end_of_day <= ts.days[i - 1].active_campaigns_end_of_day,
        `day ${d.day}: active_campaigns_end_of_day must be monotonically non-increasing`,
      );
    }

    assert.strictEqual(
      d.remaining_budget_cents + d.cumulative_spend_cents,
      s.total_budget_cents,
      `day ${d.day}: remaining_budget_cents + cumulative_spend_cents !== total_budget_cents`,
    );

    if (s.total_budget_cents === 0) {
      assert.strictEqual(d.utilization_pct, 0, `day ${d.day}: utilization_pct must be 0 when total_budget_cents === 0`);
    } else {
      const expectedUtil = (d.cumulative_spend_cents / s.total_budget_cents) * 100;
      assert.ok(
        Math.abs(d.utilization_pct - expectedUtil) <= 0.05,
        `day ${d.day}: utilization_pct ${d.utilization_pct} deviates from expected ${expectedUtil.toFixed(4)}`,
      );
    }

    assert.ok(
      d.active_campaigns_end_of_day <= s.total_campaigns,
      `day ${d.day}: active_campaigns_end_of_day (${d.active_campaigns_end_of_day}) > total_campaigns (${s.total_campaigns})`,
    );
    if (d.insufficient_budget > 0) {
      assert.strictEqual(
        d.active_campaigns_end_of_day,
        0,
        `day ${d.day}: active_campaigns_end_of_day must be 0 when insufficient_budget > 0`,
      );
    }

    if (cfg.impressions !== null) {
      const expectedQuota = Math.floor(cfg.impressions / 30) + (d.day <= cfg.impressions % 30 ? 1 : 0);
      assert.strictEqual(d.impressions, expectedQuota, `day ${d.day}: impressions quota mismatch`);
    }

    sumImpressions += d.impressions;
    sumClicks += d.clicks;
    sumInsufficient += d.insufficient_budget;
    if (d.daily_spend_cents < minDaily) minDaily = d.daily_spend_cents;
    if (d.daily_spend_cents > maxDaily) maxDaily = d.daily_spend_cents;
  }

  assert.strictEqual(runningSpend, s.total_spend_cents, "sum(daily_spend_cents) !== summary.total_spend_cents");
  assert.strictEqual(ts.days[29].cumulative_spend_cents, s.total_spend_cents, "day 30 cumulative_spend_cents !== summary.total_spend_cents");
  assert.strictEqual(s.remaining_budget_cents + s.total_spend_cents, s.total_budget_cents, "summary remaining + total_spend !== total_budget");
  assert.strictEqual(ts.days[29].remaining_budget_cents, s.remaining_budget_cents, "day 30 remaining !== summary.remaining");
  if (cfg.campaign_budget_cents !== null) {
    assert.strictEqual(s.total_budget_cents, s.total_campaigns * cfg.campaign_budget_cents, "summary.total_budget_cents !== total_campaigns * campaign_budget_cents");
  } else {
    assert.ok(
      s.total_budget_cents >= s.total_campaigns * cfg.campaign_budget_min_cents &&
        s.total_budget_cents <= s.total_campaigns * cfg.campaign_budget_max_cents,
      `summary.total_budget_cents (${s.total_budget_cents}) out of expected randomized bounds`,
    );
  }
  assert.strictEqual(s.total_spend_cents, s.total_clicks * cpc, "summary.total_spend_cents !== total_clicks * cpc");
  assert.strictEqual(sumImpressions, s.total_impressions, "sum(days.impressions) !== summary.total_impressions");
  assert.strictEqual(sumClicks, s.total_clicks, "sum(days.clicks) !== summary.total_clicks");
  assert.strictEqual(sumInsufficient, s.insufficient_budget_opportunities, "sum(days.insufficient_budget) !== summary.insufficient_budget_opportunities");
  assert.ok(s.exhausted_campaigns <= s.total_campaigns, "exhausted_campaigns > total_campaigns");
  assert.strictEqual(
    s.total_campaigns - s.exhausted_campaigns,
    ts.days[29].active_campaigns_end_of_day,
    "total_campaigns - exhausted_campaigns !== day 30 active_campaigns_end_of_day",
  );

  if (s.total_budget_cents === 0) {
    assert.strictEqual(s.utilization_pct, 0, "summary.utilization_pct must be 0 when total_budget_cents === 0");
  } else {
    const expectedUtil = (s.total_spend_cents / s.total_budget_cents) * 100;
    assert.ok(
      Math.abs(s.utilization_pct - expectedUtil) <= 0.05,
      `summary.utilization_pct ${s.utilization_pct} deviates from expected ${expectedUtil.toFixed(4)}`,
    );
  }

  if (s.total_impressions === 0) {
    assert.strictEqual(s.ctr_pct, 0, "summary.ctr_pct must be 0 when total_impressions === 0");
  } else {
    const expectedCtr = (s.total_clicks / s.total_impressions) * 100;
    assert.ok(
      Math.abs(s.ctr_pct - expectedCtr) <= 0.05,
      `summary.ctr_pct ${s.ctr_pct} deviates from expected ${expectedCtr.toFixed(4)}`,
    );
  }

  assert.strictEqual(s.min_daily_spend_cents, minDaily, "summary.min_daily_spend_cents mismatch");
  assert.strictEqual(s.max_daily_spend_cents, maxDaily, "summary.max_daily_spend_cents mismatch");
  assert.ok(
    s.min_daily_spend_cents <= s.mean_daily_spend_cents && s.mean_daily_spend_cents <= s.max_daily_spend_cents,
    `mean_daily_spend_cents (${s.mean_daily_spend_cents}) outside [min=${s.min_daily_spend_cents}, max=${s.max_daily_spend_cents}]`,
  );
  assert.ok(
    Math.abs(s.mean_daily_spend_cents - s.total_spend_cents / 30) <= 1.0,
    `mean_daily_spend_cents (${s.mean_daily_spend_cents}) deviates from total_spend_cents / 30 (${(s.total_spend_cents / 30).toFixed(2)})`,
  );

  if (cfg.impressions !== null) {
    assert.strictEqual(s.total_impressions, cfg.impressions, "summary.total_impressions !== config.impressions");
  }
}

/**
 * Verify cent-for-cent parity between raw simulation.json and budget_timeseries.json.
 */
export function validateSimulationAndTimeSeriesParity(rawReport, ts) {
  assert.strictEqual(rawReport.schema_version, ts.schema_version, "schema_version parity mismatch");
  assert.strictEqual(rawReport.currency, ts.currency, "currency parity mismatch");
  assert.strictEqual(rawReport.summary.budget_cents, ts.summary.total_budget_cents, "total_budget_cents parity mismatch");
  assert.strictEqual(rawReport.summary.spend_cents, ts.summary.total_spend_cents, "total_spend_cents parity mismatch");
  assert.strictEqual(rawReport.summary.remaining_budget_cents, ts.summary.remaining_budget_cents, "remaining_budget_cents parity mismatch");
  assert.strictEqual(rawReport.summary.impressions, ts.summary.total_impressions, "total_impressions parity mismatch");
  assert.strictEqual(rawReport.summary.clicks, ts.summary.total_clicks, "total_clicks parity mismatch");
  assert.strictEqual(rawReport.summary.insufficient_budget, ts.summary.insufficient_budget_opportunities, "insufficient_budget parity mismatch");
  assert.strictEqual(rawReport.campaigns.length, ts.summary.total_campaigns, "total_campaigns parity mismatch");
  assert.strictEqual(rawReport.days.length, 30, "rawReport.days.length must be 30");

  for (let i = 0; i < 30; i++) {
    const r = rawReport.days[i];
    const d = ts.days[i];
    assert.strictEqual(r.day, d.day, `day ${i + 1}: day parity mismatch`);
    assert.strictEqual(r.impressions, d.impressions, `day ${i + 1}: impressions parity mismatch`);
    assert.strictEqual(r.clicks, d.clicks, `day ${i + 1}: clicks parity mismatch`);
    assert.strictEqual(r.spend_cents, d.daily_spend_cents, `day ${i + 1}: spend parity mismatch`);
    assert.strictEqual(r.insufficient_budget, d.insufficient_budget, `day ${i + 1}: insufficient_budget parity mismatch`);
  }
}

function parseCsvLine(line) {
  const fields = [];
  let cur = "";
  let inQuotes = false;
  for (let i = 0; i < line.length; i++) {
    const ch = line[i];
    if (inQuotes) {
      if (ch === '"') {
        if (line[i + 1] === '"') {
          cur += '"';
          i += 1;
        } else {
          inQuotes = false;
        }
      } else {
        cur += ch;
      }
    } else if (ch === '"') {
      inQuotes = true;
    } else if (ch === ",") {
      fields.push(cur.trim());
      cur = "";
    } else {
      cur += ch;
    }
  }
  fields.push(cur.trim());
  return fields;
}

/**
 * Validate budget_timeseries.csv against BudgetTimeSeries JSON.
 */
export function validateBudgetCsv(csvText, ts) {
  assert.ok(typeof csvText === "string" && csvText.trim().length > 0, "CSV text must be non-empty string");
  const lines = csvText.replace(/\r\n/g, "\n").trim().split("\n").filter((l) => l.trim().length > 0);
  assert.strictEqual(lines.length, 31, `budget_timeseries.csv must have 31 lines (header + 30 days), got ${lines.length}`);

  const headers = parseCsvLine(lines[0]).map((h) => h.toLowerCase());
  const headerSet = new Set(headers);
  assert.strictEqual(headerSet.size, headers.length, `duplicate CSV headers: ${headers.join(", ")}`);

  const requiredCols = [
    "day",
    "impressions",
    "clicks",
    "daily_spend_cents",
    "cumulative_spend_cents",
    "remaining_budget_cents",
    "utilization_pct",
    "active_campaigns_end_of_day",
    "insufficient_budget",
  ];
  for (const col of requiredCols) {
    assert.ok(headerSet.has(col), `CSV missing required column "${col}" (found: ${headers.join(", ")})`);
  }

  const intCols = [
    "day",
    "impressions",
    "clicks",
    "daily_spend_cents",
    "cumulative_spend_cents",
    "remaining_budget_cents",
    "active_campaigns_end_of_day",
    "insufficient_budget",
  ];

  for (let i = 0; i < 30; i++) {
    const cells = parseCsvLine(lines[i + 1]);
    assert.strictEqual(cells.length, headers.length, `CSV row ${i + 1} column count (${cells.length}) !== header count (${headers.length})`);
    const row = {};
    for (let c = 0; c < headers.length; c++) {
      const raw = cells[c];
      assert.ok(
        !/^(nan|infinity|-infinity|null|undefined|)$/i.test(raw),
        `CSV row ${i + 1} column "${headers[c]}" contains invalid value: ${JSON.stringify(raw)}`,
      );
      row[headers[c]] = raw;
    }

    const expected = ts.days[i];
    for (const col of intCols) {
      const parsed = Number(row[col]);
      assert.strictEqual(parsed, expected[col], `CSV row ${i + 1} "${col}" (${parsed}) !== JSON (${expected[col]})`);
    }
    const util = Number(row.utilization_pct);
    assert.ok(
      Number.isFinite(util) && Math.abs(util - expected.utilization_pct) <= 0.05,
      `CSV row ${i + 1} "utilization_pct" (${util}) deviates from JSON (${expected.utilization_pct})`,
    );
  }
}

// ============================================================================
// Pure-JS XML Well-Formedness & SVG Geometry Validator
// ============================================================================

function validateXmlWellFormed(xmlText) {
  let s = xmlText.trim();
  // Strip XML declaration if present at top
  s = s.replace(/^<\?xml\b[^?]*\?>\s*/, "");
  // Verify comments are closed and strip them
  while (s.includes("<!--")) {
    const start = s.indexOf("<!--");
    const end = s.indexOf("-->", start + 4);
    assert.ok(end !== -1, "SVG contains unclosed XML comment");
    s = s.slice(0, start) + s.slice(end + 3);
  }
  // Verify CDATA blocks are closed and strip them
  while (s.includes("<![CDATA[")) {
    const start = s.indexOf("<![CDATA[");
    const end = s.indexOf("]]>", start + 9);
    assert.ok(end !== -1, "SVG contains unclosed CDATA section");
    s = s.slice(0, start) + s.slice(end + 3);
  }

  // Verify no raw unescaped ampersands
  const badAmp = s.match(/&(?!(?:amp|lt|gt|quot|apos|#\d+|#x[0-9a-fA-F]+);)/);
  assert.ok(!badAmp, `SVG contains unescaped ampersand near: ${s.slice(Math.max(0, (badAmp?.index ?? 0) - 10), (badAmp?.index ?? 0) + 20)}`);

  const stack = [];
  let rootCount = 0;
  let rootName = null;
  let i = 0;
  const n = s.length;

  while (i < n) {
    const lt = s.indexOf("<", i);
    if (lt === -1) break;
    // Ensure stray '>' doesn't occur before '<' — wait: '>' is technically legal in XML text, so skip text
    if (s.startsWith("</", lt)) {
      const gt = s.indexOf(">", lt + 2);
      assert.ok(gt !== -1, "SVG contains unclosed closing tag");
      const name = s.slice(lt + 2, gt).trim();
      assert.ok(/^[A-Za-z_][\w:.-]*$/.test(name), `Invalid XML closing tag name: "${name}"`);
      assert.ok(stack.length > 0, `Unexpected closing tag </${name}> with empty tag stack`);
      const top = stack.pop();
      assert.strictEqual(top, name, `Mismatched XML closing tag: expected </${top}>, got </${name}>`);
      i = gt + 1;
      continue;
    }

    if (s.startsWith("<!", lt) || s.startsWith("<?", lt)) {
      const gt = s.indexOf(">", lt + 2);
      assert.ok(gt !== -1, "Unclosed XML declaration/directive");
      i = gt + 1;
      continue;
    }

    // Opening or self-closing tag: scan while respecting quoted attributes
    let j = lt + 1;
    let inQuote = null;
    while (j < n) {
      const ch = s[j];
      if (inQuote) {
        if (ch === inQuote) inQuote = null;
      } else if (ch === '"' || ch === "'") {
        inQuote = ch;
      } else if (ch === ">") {
        break;
      } else if (ch === "<") {
        assert.fail(`Unclosed XML tag starting near: ${s.slice(lt, lt + 40)}`);
      }
      j += 1;
    }
    assert.ok(j < n && s[j] === ">", `Unclosed XML opening tag starting near: ${s.slice(lt, lt + 40)}`);
    assert.strictEqual(inQuote, null, `Unclosed attribute quote inside tag near: ${s.slice(lt, j + 1)}`);

    const rawTag = s.slice(lt + 1, j).trim();
    const selfClosing = rawTag.endsWith("/");
    const body = selfClosing ? rawTag.slice(0, -1).trim() : rawTag;
    const nameMatch = body.match(/^([A-Za-z_][\w:.-]*)(?:\s+([\s\S]*))?$/);
    assert.ok(nameMatch, `Malformed XML tag: <${rawTag}>`);

    const tagName = nameMatch[1];
    const attrPart = (nameMatch[2] ?? "").trim();

    // Validate attributes are strictly name="val" or name='val'
    if (attrPart.length > 0) {
      const attrRegex = /([A-Za-z_][\w:.-]*)\s*=\s*(?:"([^"]*)"|'([^']*)')/g;
      let consumed = 0;
      let m;
      while ((m = attrRegex.exec(attrPart)) !== null) {
        const gap = attrPart.slice(consumed, m.index).trim();
        assert.strictEqual(gap, "", `Malformed attribute syntax in <${tagName}> near: "${gap}"`);
        consumed = attrRegex.lastIndex;
      }
      const trailing = attrPart.slice(consumed).trim();
      assert.strictEqual(trailing, "", `Malformed trailing attribute syntax in <${tagName}> near: "${trailing}"`);
    }

    if (stack.length === 0) {
      rootCount += 1;
      rootName = tagName;
    }
    if (!selfClosing) {
      stack.push(tagName);
    }
    i = j + 1;
  }

  assert.strictEqual(stack.length, 0, `Unclosed XML tags remaining: ${stack.join(", ")}`);
  assert.strictEqual(rootCount, 1, `SVG must have exactly 1 root element, got ${rootCount}`);
  assert.ok(rootName === "svg" || rootName === "svg:svg", `Root XML element must be <svg>, got <${rootName}>`);
}

function parseAttributes(tagText) {
  const attrs = {};
  const attrRegex = /([A-Za-z_][\w:.-]*)\s*=\s*(?:"([^"]*)"|'([^']*)')/g;
  let m;
  while ((m = attrRegex.exec(tagText)) !== null) {
    attrs[m[1]] = m[2] ?? m[3] ?? "";
  }
  return attrs;
}

function parseNumAttr(val) {
  if (val === undefined) return null;
  const cleaned = String(val).trim().replace(/px$/i, "");
  if (cleaned.length === 0) return NaN;
  return Number(cleaned);
}

function extractPathPoints(d) {
  const tokens = d.match(/[a-zA-Z]|[-+]?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?/g);
  if (!tokens) return [];
  const points = [];
  let i = 0;
  let cmd = null;
  let cx = 0;
  let cy = 0;
  let sx = 0;
  let sy = 0;

  const nextNum = () => {
    if (i >= tokens.length || /^[a-zA-Z]$/.test(tokens[i])) return null;
    return Number(tokens[i++]);
  };

  while (i < tokens.length) {
    if (/^[a-zA-Z]$/.test(tokens[i])) {
      cmd = tokens[i++];
      if (cmd === "Z" || cmd === "z") {
        cx = sx;
        cy = sy;
        continue;
      }
    }
    if (!cmd) break;
    const upper = cmd.toUpperCase();
    const rel = cmd !== upper;

    if (upper === "M" || upper === "L" || upper === "T") {
      const x = nextNum();
      const y = nextNum();
      if (x === null || y === null) break;
      cx = rel ? cx + x : x;
      cy = rel ? cy + y : y;
      if (upper === "M") {
        sx = cx;
        sy = cy;
        cmd = rel ? "l" : "L";
      }
      points.push([cx, cy]);
    } else if (upper === "H") {
      const x = nextNum();
      if (x === null) break;
      cx = rel ? cx + x : x;
      points.push([cx, cy]);
    } else if (upper === "V") {
      const y = nextNum();
      if (y === null) break;
      cy = rel ? cy + y : y;
      points.push([cx, cy]);
    } else if (upper === "Q" || upper === "S") {
      const x1 = nextNum();
      const y1 = nextNum();
      const x = nextNum();
      const y = nextNum();
      if (x1 === null || y1 === null || x === null || y === null) break;
      cx = rel ? cx + x : x;
      cy = rel ? cy + y : y;
      points.push([cx, cy]);
    } else if (upper === "C") {
      const x1 = nextNum();
      const y1 = nextNum();
      const x2 = nextNum();
      const y2 = nextNum();
      const x = nextNum();
      const y = nextNum();
      if (x1 === null || y1 === null || x2 === null || y2 === null || x === null || y === null) break;
      cx = rel ? cx + x : x;
      cy = rel ? cy + y : y;
      points.push([cx, cy]);
    } else if (upper === "A") {
      const rx = nextNum();
      const ry = nextNum();
      const rot = nextNum();
      const la = nextNum();
      const sw = nextNum();
      const x = nextNum();
      const y = nextNum();
      if (rx === null || ry === null || rot === null || la === null || sw === null || x === null || y === null) break;
      cx = rel ? cx + x : x;
      cy = rel ? cy + y : y;
      points.push([cx, cy]);
    } else {
      break;
    }
  }
  return points;
}

function unescapeXmlEntities(text) {
  return text
    .replace(/&amp;/g, "&")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&apos;/g, "'")
    .replace(/&#39;/g, "'");
}

/**
 * Opaque-box structural, XML well-formedness, numeric-integrity, and visual-content validator
 * for budget_spent_30d.svg and GET /api/artifacts/budget-plot.
 *
 * @param {string} svgText
 * @param {object | null} [ts]
 */
export function validateBudgetSvg(svgText, ts = null) {
  assert.ok(typeof svgText === "string" && svgText.trim().length > 0, "SVG must be a non-empty string");
  const trimmed = svgText.trim();
  assert.ok(
    trimmed.startsWith("<?xml") || trimmed.startsWith("<svg"),
    `SVG must start with <?xml or <svg, got: ${trimmed.slice(0, 60)}`,
  );
  assert.ok(trimmed.endsWith("</svg>"), `SVG must end with </svg>`);
  assert.ok(
    /<svg\b[^>]*xmlns=["']http:\/\/www\.w3\.org\/2000\/svg["']/.test(trimmed),
    "SVG root must declare xmlns=\"http://www.w3.org/2000/svg\"",
  );

  // Zero NaN / Infinity / undefined tokens anywhere
  assert.ok(!/\b(NaN|Infinity|undefined)\b/.test(svgText), "SVG contains forbidden NaN/Infinity/undefined token");
  const attrValRegex = /=\s*(?:"([^"]*)"|'([^']*)')/g;
  let attrMatch;
  while ((attrMatch = attrValRegex.exec(svgText)) !== null) {
    const val = attrMatch[1] ?? attrMatch[2] ?? "";
    assert.ok(
      !/\b(nan|infinity|undefined|null)\b/i.test(val),
      `SVG attribute contains degenerate token: ${JSON.stringify(val)}`,
    );
  }

  // XML well-formedness check
  validateXmlWellFormed(trimmed);

  // Extract <svg> dimensions
  const svgOpenMatch = trimmed.match(/<svg\b([^>]*)>/);
  assert.ok(svgOpenMatch, "Could not parse <svg> opening tag");
  const svgAttrs = parseAttributes(svgOpenMatch[1]);
  let viewWidth = null;
  let viewHeight = null;
  if (svgAttrs.viewBox) {
    const nums = svgAttrs.viewBox.trim().split(/[\s,]+/).map(Number);
    assert.strictEqual(nums.length, 4, `Invalid SVG viewBox: ${svgAttrs.viewBox}`);
    assert.ok(nums.every(Number.isFinite), `Non-finite SVG viewBox numbers: ${svgAttrs.viewBox}`);
    viewWidth = nums[2];
    viewHeight = nums[3];
  } else {
    viewWidth = parseNumAttr(svgAttrs.width);
    viewHeight = parseNumAttr(svgAttrs.height);
  }
  assert.ok(
    Number.isFinite(viewWidth) && Number.isFinite(viewHeight) && viewWidth >= 400 && viewHeight >= 250,
    `SVG dimensions must be >= 400x250, got ${viewWidth}x${viewHeight}`,
  );

  // Geometric sanity on <rect>, <circle>, <line>
  const rects = [];
  const rectRegex = /<rect\b([^>]*?)\/?\s*>/g;
  let rm;
  while ((rm = rectRegex.exec(trimmed)) !== null) {
    const a = parseAttributes(rm[1]);
    const w = parseNumAttr(a.width);
    const h = parseNumAttr(a.height);
    const x = parseNumAttr(a.x);
    const y = parseNumAttr(a.y);
    if (w !== null) {
      assert.ok(Number.isFinite(w) && w >= 0, `SVG <rect> has invalid width: ${a.width}`);
    }
    if (h !== null) {
      assert.ok(Number.isFinite(h) && h >= 0, `SVG <rect> has invalid height: ${a.height}`);
    }
    if (x !== null) assert.ok(Number.isFinite(x), `SVG <rect> has non-finite x: ${a.x}`);
    if (y !== null) assert.ok(Number.isFinite(y), `SVG <rect> has non-finite y: ${a.y}`);
    if (w !== null && h !== null) {
      rects.push({ x: x ?? 0, y: y ?? 0, width: w, height: h });
    }
  }

  const circleRegex = /<circle\b([^>]*?)\/?\s*>/g;
  let cm;
  while ((cm = circleRegex.exec(trimmed)) !== null) {
    const a = parseAttributes(cm[1]);
    for (const k of ["cx", "cy", "r"]) {
      const v = parseNumAttr(a[k]);
      if (v !== null) {
        assert.ok(Number.isFinite(v), `SVG <circle> has non-finite ${k}: ${a[k]}`);
        if (k === "r") assert.ok(v >= 0, `SVG <circle> has negative radius: ${a[k]}`);
      }
    }
  }

  const lineRegex = /<line\b([^>]*?)\/?\s*>/g;
  let lm;
  while ((lm = lineRegex.exec(trimmed)) !== null) {
    const a = parseAttributes(lm[1]);
    for (const k of ["x1", "y1", "x2", "y2"]) {
      const v = parseNumAttr(a[k]);
      if (v !== null) {
        assert.ok(Number.isFinite(v), `SVG <line> has non-finite ${k}: ${a[k]}`);
      }
    }
  }

  // Daily Spend Bars check (Panel 1 — >= 30 bars)
  const barCandidates = rects.filter((r) => r.width > 0 && r.width <= viewWidth / 15);
  // Find largest width-consistent bar cluster with distinct x coordinates
  let bestBarCluster = [];
  for (const cand of barCandidates) {
    const cluster = barCandidates.filter((r) => Math.abs(r.width - cand.width) <= 2.0);
    const distinctX = new Set(cluster.map((r) => Math.round(r.x * 10)));
    if (distinctX.size > bestBarCluster.length) {
      bestBarCluster = cluster;
    }
  }
  const distinctClusterX = new Set(bestBarCluster.map((r) => Math.round(r.x * 10))).size;
  assert.ok(
    distinctClusterX >= 30,
    `Expected at least 30 daily spend bar <rect> elements with distinct x positions, found ${distinctClusterX} (total bar-sized rects=${barCandidates.length})`,
  );
  if (ts && ts.summary?.total_spend_cents > 0) {
    const positiveBars = bestBarCluster.filter((r) => r.height > 0);
    assert.ok(
      positiveBars.length > 0,
      "Expected at least one daily spend bar with height > 0 for non-zero spend run",
    );
  }

  // Cumulative Spend Curve check (Panel 2 — >= 30 coordinate pairs)
  const curveSeries = [];
  const polyRegex = /<polyline\b([^>]*?)\/?\s*>/g;
  let pm;
  while ((pm = polyRegex.exec(trimmed)) !== null) {
    const a = parseAttributes(pm[1]);
    if (!a.points) continue;
    const nums = a.points.trim().split(/[\s,]+/).filter(Boolean).map(Number);
    assert.ok(nums.every(Number.isFinite), `SVG <polyline> has non-finite points`);
    const pts = [];
    for (let i = 0; i + 1 < nums.length; i += 2) {
      pts.push([nums[i], nums[i + 1]]);
    }
    if (pts.length >= 30) curveSeries.push(pts);
  }

  const pathRegex = /<path\b([^>]*?)\/?\s*>/g;
  let pathM;
  while ((pathM = pathRegex.exec(trimmed)) !== null) {
    const a = parseAttributes(pathM[1]);
    if (!a.d) continue;
    const pts = extractPathPoints(a.d);
    if (pts.length >= 30) curveSeries.push(pts);
  }

  assert.ok(
    curveSeries.length >= 1,
    "Expected at least one <polyline> or <path> with >= 30 coordinate pairs for the 30-day cumulative curve",
  );

  if (ts && ts.summary?.total_spend_cents > 0) {
    const hasNonFlatCurve = curveSeries.some((pts) => {
      const ys = pts.map((p) => p[1]);
      return Math.max(...ys) - Math.min(...ys) > 0.5;
    });
    assert.ok(
      hasNonFlatCurve,
      "Expected non-flat cumulative spend curve (varying Y coordinates) for non-zero spend run",
    );
  }

  // Text & KPI Labels
  const textEntries = [];
  const textRegex = /<text\b[^>]*>([\s\S]*?)<\/text>/g;
  let tm;
  while ((tm = textRegex.exec(trimmed)) !== null) {
    const inner = unescapeXmlEntities(tm[1].replace(/<[^>]+>/g, " ")).replace(/\s+/g, " ").trim();
    if (inner.length > 0) textEntries.push(inner);
  }
  assert.ok(
    textEntries.length >= 8,
    `Expected at least 8 non-empty <text> labels in SVG, found ${textEntries.length}`,
  );
  const allText = textEntries.join(" | ");
  assert.ok(/daily/i.test(allText), `SVG text must mention daily spend; text="${allText.slice(0, 300)}"`);
  assert.ok(/cumulative/i.test(allText), `SVG text must mention cumulative spend; text="${allText.slice(0, 300)}"`);
  assert.ok(/budget|spend|spent/i.test(allText), `SVG text must mention budget/spend`);
  assert.ok(/\b1\b/.test(allText) && /\b30\b/.test(allText), `SVG text must reference day range 1..30`);

  if (ts && ts.summary && typeof ts.summary.total_spend_cents === "number") {
    const candidates = formatUsdCandidates(ts.summary.total_spend_cents);
    assert.ok(
      candidates.some((c) => allText.includes(c)),
      `SVG text must include formatted total spend (one of ${candidates.join(", ")}); observed text="${allText.slice(0, 400)}"`,
    );
  }
}

/**
 * Validate GET /api/catalog (SimulatorCatalog) against PROJECT.md §4.
 *
 * @param {unknown} catalog
 * @param {{ repoRoot?: string | null }} [opts]
 */
export function validateSimulatorCatalog(catalog, opts = {}) {
  assert.ok(isPlainObject(catalog), "SimulatorCatalog must be a plain object");
  assert.strictEqual(catalog.schema_version, 1, `catalog.schema_version must be 1, got ${catalog.schema_version}`);
  assert.strictEqual(catalog.currency, "USD", `catalog.currency must be "USD", got ${catalog.currency}`);
  assert.ok(
    typeof catalog.simulator_version === "string" && catalog.simulator_version.trim().length > 0,
    "catalog.simulator_version must be a non-empty string",
  );

  assert.ok(
    Array.isArray(catalog.pieces) && catalog.pieces.length === 11,
    `catalog.pieces must contain exactly 11 pieces, got ${catalog.pieces?.length}`,
  );

  const seenIds = new Set();
  const cratesSeen = new Set();
  let totalFeatures = 0;
  let implementedFeatures = 0;
  let totalGaps = 0;

  for (const p of catalog.pieces) {
    assert.ok(isPlainObject(p), "catalog piece must be an object");
    for (const k of ["id", "name", "crate_name", "stage", "summary"]) {
      assert.ok(
        typeof p[k] === "string" && p[k].trim().length > 0,
        `catalog piece "${p.id ?? "unknown"}" missing non-empty string field "${k}"`,
      );
    }
    assert.ok(!seenIds.has(p.id), `duplicate catalog piece id: ${p.id}`);
    seenIds.add(p.id);
    cratesSeen.add(p.crate_name.replace(/_/g, "-"));

    assert.ok(
      Array.isArray(p.source_files) && p.source_files.length >= 1,
      `piece "${p.id}" must have at least 1 source_files entry`,
    );
    for (const sf of p.source_files) {
      assert.ok(typeof sf === "string" && sf.trim().length > 0, `piece "${p.id}" has empty source_file`);
      assert.ok(!sf.toLowerCase().endsWith(".py"), `piece "${p.id}" references forbidden legacy Python file: ${sf}`);
      assert.ok(sf.toLowerCase().endsWith(".rs"), `piece "${p.id}" source_file must be a .rs file, got: ${sf}`);
      if (opts.repoRoot) {
        const fullPath = path.join(opts.repoRoot, sf);
        assert.ok(fs.existsSync(fullPath), `piece "${p.id}" source_file does not exist on disk: ${sf}`);
      }
    }

    // Parameters (array of objects or plain key-value object)
    if (Array.isArray(p.parameters)) {
      for (const param of p.parameters) {
        assert.ok(isPlainObject(param), `piece "${p.id}" parameter entry must be an object`);
        const keyStr = param.key ?? param.name ?? param.label;
        assert.ok(typeof keyStr === "string" && keyStr.trim().length > 0, `piece "${p.id}" parameter missing key/name`);
        const val = param.value !== undefined ? param.value : param.default;
        assert.ok(val !== undefined && val !== null, `piece "${p.id}" parameter "${keyStr}" missing value`);
      }
    } else if (isPlainObject(p.parameters)) {
      for (const [k, v] of Object.entries(p.parameters)) {
        assert.ok(k.trim().length > 0 && v !== undefined && v !== null, `piece "${p.id}" invalid parameter "${k}"`);
      }
    } else {
      assert.fail(`piece "${p.id}" must define parameters array or object`);
    }

    assert.ok(Array.isArray(p.features) && p.features.length >= 1, `piece "${p.id}" must have >= 1 feature`);
    for (const feat of p.features) {
      if (typeof feat === "string") {
        assert.ok(feat.trim().length > 0, `piece "${p.id}" has empty feature string`);
        totalFeatures += 1;
        implementedFeatures += 1;
      } else {
        assert.ok(isPlainObject(feat), `piece "${p.id}" feature entry must be string or object`);
        const fName = feat.name ?? feat.title;
        assert.ok(typeof fName === "string" && fName.trim().length > 0, `piece "${p.id}" feature missing name`);
        totalFeatures += 1;
        if (!feat.status || String(feat.status).toLowerCase() === "implemented") {
          implementedFeatures += 1;
        }
      }
    }

    assert.ok(Array.isArray(p.gaps_and_simplifications), `piece "${p.id}" must include gaps_and_simplifications array`);
    for (const gap of p.gaps_and_simplifications) {
      if (typeof gap === "string") {
        assert.ok(gap.trim().length > 0, `piece "${p.id}" has empty gap string`);
      } else {
        assert.ok(isPlainObject(gap), `piece "${p.id}" gap entry must be string or object`);
        const gText = gap.name ?? gap.title ?? gap.notes ?? gap.description ?? gap.summary;
        assert.ok(typeof gText === "string" && gText.trim().length > 0, `piece "${p.id}" gap object missing description`);
      }
      totalGaps += 1;
    }
  }

  for (const requiredCrate of [
    "ads-simulator",
    "ad-server",
    "visit-simulator",
    "ads-marketplace-simulation",
    "simulation-analyzer",
    "sim-agent",
    "ads-simulator-dashboard",
  ]) {
    assert.ok(
      cratesSeen.has(requiredCrate),
      `catalog.pieces must cover crate "${requiredCrate}" (observed crates: ${[...cratesSeen].join(", ")})`,
    );
  }
  assert.ok(totalGaps >= 3, `catalog must honestly report at least 3 gaps_and_simplifications across the 11 pieces, got ${totalGaps}`);

  // Pipeline edges
  assert.ok(
    Array.isArray(catalog.pipeline_edges) && catalog.pipeline_edges.length >= 10,
    `catalog.pipeline_edges must have >= 10 edges, got ${catalog.pipeline_edges?.length}`,
  );
  const connectedNodes = new Set();
  for (const edge of catalog.pipeline_edges) {
    assert.ok(isPlainObject(edge), "pipeline_edges entry must be an object");
    const from = edge.from ?? edge.source;
    const to = edge.to ?? edge.target;
    assert.ok(typeof from === "string" && seenIds.has(from), `pipeline edge has unknown source piece: ${from}`);
    assert.ok(typeof to === "string" && seenIds.has(to), `pipeline edge has unknown target piece: ${to}`);
    assert.notStrictEqual(from, to, `pipeline edge cannot be a self-loop on "${from}"`);
    connectedNodes.add(from);
    connectedNodes.add(to);
  }
  assert.strictEqual(
    connectedNodes.size,
    11,
    `All 11 pieces must be connected in pipeline_edges; connected=${connectedNodes.size}`,
  );

  // Summary counts
  assert.ok(isPlainObject(catalog.summary_counts), "catalog.summary_counts must be an object");
  const scEntries = Object.entries(catalog.summary_counts);
  assert.ok(scEntries.length >= 1, "catalog.summary_counts must not be empty");
  for (const [k, v] of scEntries) {
    assertNonNegInt(v, `catalog.summary_counts.${k}`);
  }
  if (Object.hasOwn(catalog.summary_counts, "pieces")) {
    assert.strictEqual(catalog.summary_counts.pieces, 11, "summary_counts.pieces must equal 11");
  }
  if (Object.hasOwn(catalog.summary_counts, "total_features")) {
    assert.strictEqual(catalog.summary_counts.total_features, totalFeatures, "summary_counts.total_features mismatch");
  }
  if (Object.hasOwn(catalog.summary_counts, "implemented_features")) {
    assert.strictEqual(catalog.summary_counts.implemented_features, implementedFeatures, "summary_counts.implemented_features mismatch");
  }
  if (Object.hasOwn(catalog.summary_counts, "gaps_and_simplifications")) {
    assert.strictEqual(catalog.summary_counts.gaps_and_simplifications, totalGaps, "summary_counts.gaps_and_simplifications mismatch");
  }
}

/**
 * Validate GET /api/marketplace?seed=<i64> response against PROJECT.md §4.
 *
 * @param {unknown} apiObj
 * @param {{ expectedSeed?: number | null }} [opts]
 */
export function validateMarketplaceApiResponse(apiObj, opts = {}) {
  assert.ok(isPlainObject(apiObj), "marketplace API response must be an object");
  assert.strictEqual(typeof apiObj.seed, "number", `marketplace API seed must be numeric, got ${typeof apiObj.seed}`);
  if (opts.expectedSeed !== undefined && opts.expectedSeed !== null) {
    assert.strictEqual(apiObj.seed, opts.expectedSeed, `marketplace API seed mismatch`);
  }

  // Taxonomy: 10 groups x 10 categories (supports array of objects, array of tuples, or map object)
  let groups = [];
  if (Array.isArray(apiObj.taxonomy)) {
    groups = apiObj.taxonomy.map((entry, idx) => {
      if (Array.isArray(entry) && entry.length === 2) {
        return { name: entry[0], categories: entry[1] };
      }
      if (isPlainObject(entry)) {
        return {
          name: entry.group ?? entry.name ?? entry.title,
          categories: entry.categories ?? entry.items,
        };
      }
      assert.fail(`Unsupported taxonomy entry at index ${idx}`);
    });
  } else if (isPlainObject(apiObj.taxonomy)) {
    groups = Object.entries(apiObj.taxonomy).map(([name, categories]) => ({ name, categories }));
  } else {
    assert.fail("marketplace API taxonomy must be an array or object");
  }

  assert.strictEqual(groups.length, 10, `expected 10 taxonomy groups, got ${groups.length}`);
  const seenGroupNames = new Set();
  const seenCatNames = new Set();
  for (const g of groups) {
    assert.ok(typeof g.name === "string" && g.name.trim().length > 0, "taxonomy group name must be non-empty");
    assert.ok(!seenGroupNames.has(g.name), `duplicate taxonomy group: ${g.name}`);
    seenGroupNames.add(g.name);
    assert.ok(Array.isArray(g.categories) && g.categories.length === 10, `taxonomy group "${g.name}" must have 10 categories, got ${g.categories?.length}`);
    for (const c of g.categories) {
      const cname = typeof c === "string" ? c : c?.name ?? c?.id;
      assert.ok(typeof cname === "string" && cname.trim().length > 0, `invalid category in group "${g.name}"`);
      seenCatNames.add(`${g.name}::${cname}`);
    }
  }
  assert.strictEqual(seenCatNames.size, 100, `expected 100 unique taxonomy categories, got ${seenCatNames.size}`);

  // Counts
  assert.ok(isPlainObject(apiObj.counts), "marketplace API counts must be an object");
  assert.strictEqual(apiObj.counts.categories, 100, "counts.categories must be 100");
  assert.strictEqual(apiObj.counts.advertisers, 50, "counts.advertisers must be 50");
  assert.strictEqual(apiObj.counts.users, 1000, "counts.users must be 1000");
  assert.strictEqual(apiObj.counts.organic_items, 5000, "counts.organic_items must be 5000");
  assert.strictEqual(apiObj.counts.browsing_events, 12000, "counts.browsing_events must be 12000");
  if (Object.hasOwn(apiObj.counts, "catalog_items")) {
    assert.strictEqual(apiObj.counts.catalog_items, 1000, "counts.catalog_items must be 1000");
  }

  // Advertiser objective field has been dropped from schema
  assert.strictEqual(
    Object.hasOwn(apiObj, "advertiser_objectives"),
    false,
    "marketplace API must not include dropped advertiser_objectives field",
  );

  // Price stats (supports top-level min_cents/max_cents/mean_cents or nested organic/catalog)
  assert.ok(isPlainObject(apiObj.price_stats), "marketplace API price_stats must be an object");
  const checkPriceBucket = (bucket, label) => {
    const min = bucket.min_cents ?? bucket.min;
    const max = bucket.max_cents ?? bucket.max;
    const mean = bucket.mean_cents ?? bucket.avg_cents ?? bucket.mean;
    assert.ok(typeof min === "number" && Number.isFinite(min), `${label}.min must be finite number`);
    assert.ok(typeof max === "number" && Number.isFinite(max), `${label}.max must be finite number`);
    assert.ok(typeof mean === "number" && Number.isFinite(mean), `${label}.mean must be finite number`);
    assert.ok(min >= 500 && min <= mean && mean <= max && max <= 30000, `${label} prices out of [500, 30000] ordering: min=${min}, mean=${mean}, max=${max}`);
  };
  if (apiObj.price_stats.min_cents !== undefined || apiObj.price_stats.min !== undefined) {
    checkPriceBucket(apiObj.price_stats, "price_stats");
  } else {
    const nestedKeys = Object.keys(apiObj.price_stats).filter((k) => isPlainObject(apiObj.price_stats[k]));
    assert.ok(nestedKeys.length >= 1, "price_stats must provide min/mean/max or nested stat buckets");
    for (const nk of nestedKeys) {
      checkPriceBucket(apiObj.price_stats[nk], `price_stats.${nk}`);
    }
  }

  // Browsing split
  assert.ok(isPlainObject(apiObj.browsing_split), "marketplace API browsing_split must be an object");
  const affinity = apiObj.browsing_split.affinity ?? apiObj.browsing_split.affinity_count;
  const exploration = apiObj.browsing_split.exploration ?? apiObj.browsing_split.exploration_count;
  assert.strictEqual(affinity, 9000, `browsing_split.affinity must be 9000, got ${affinity}`);
  assert.strictEqual(exploration, 3000, `browsing_split.exploration must be 3000, got ${exploration}`);
}

/**
 * Validate a dashboard static export bundle created by `--export-dir`.
 *
 * @param {string} dirPath
 * @param {{ expectedTimeseries?: object, repoRoot?: string | null }} [opts]
 */
export function validateStaticExportBundle(dirPath, opts = {}) {
  const requiredFiles = [
    "index.html",
    "dashboard-data.json",
    "budget_timeseries.json",
    "budget_spent_30d.svg",
  ];
  for (const name of requiredFiles) {
    const full = path.join(dirPath, name);
    assert.ok(fs.existsSync(full), `Static export bundle missing required file: ${name} (in ${dirPath})`);
  }

  const indexHtml = fs.readFileSync(path.join(dirPath, "index.html"), "utf8");
  assert.ok(indexHtml.length > 500, `Static export index.html too small (${indexHtml.length} bytes)`);
  assert.ok(/<html\b/i.test(indexHtml) && /<\/html>/i.test(indexHtml), "Static export index.html missing <html>...</html>");

  const ts = readJsonFile(path.join(dirPath, "budget_timeseries.json"));
  validateBudgetTimeSeries(ts);
  if (opts.expectedTimeseries) {
    assert.deepStrictEqual(ts, opts.expectedTimeseries, "Static export budget_timeseries.json mismatch");
  }

  const svg = fs.readFileSync(path.join(dirPath, "budget_spent_30d.svg"), "utf8");
  validateBudgetSvg(svg, ts);

  const dashData = readJsonFile(path.join(dirPath, "dashboard-data.json"));
  assert.ok(isPlainObject(dashData), "dashboard-data.json must be a JSON object");
  const catalog = dashData.catalog ?? dashData;
  validateSimulatorCatalog(catalog, { repoRoot: opts.repoRoot ?? null });

  return { indexHtml, dashData, catalog, timeseries: ts, svg };
}
