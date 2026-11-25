#!/usr/bin/env node
/**
 * Canonical opaque-box E2E test runner for ads-simulator.
 *
 * Usage:
 *   node e2e/runner.mjs [--tier 1|2|3|4|all] [--list] [--area <slug>] [--filter <substr>]
 *                       [--fail-fast] [--verbose] [--json] [--help]
 *
 * Exit codes:
 *   0 - Success (--help, --list, or executed >0 tests with 0 failures)
 *   1 - Test failure(s) or zero tests matched during execution
 *   2 - Invalid CLI arguments
 */

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import {
  AREA_LABELS,
  FEATURE_AREAS,
  summarizeRegistry,
  TIER_LABELS,
  VALID_TIERS,
} from "./registry.mjs";
import { REPO_ROOT } from "./helpers.mjs";
import { ALL_TESTS } from "./tests/index.mjs";

const AREA_SET = new Set(FEATURE_AREAS);

function printUsage(stream = process.stdout) {
  stream.write(
    `Usage: node e2e/runner.mjs [OPTIONS]\n\n` +
      `Options:\n` +
      `  --tier <1|2|3|4|all>   Select test tier (default: all)\n` +
      `  --list                 List matching tests & summary counts with zero side effects\n` +
      `  --area <slug[,slug]>   Filter by feature area slug (${FEATURE_AREAS.join(", ")})\n` +
      `  --filter <substring>   Case-insensitive substring match on test id, title, or area\n` +
      `  --fail-fast            Stop immediately after the first failing test\n` +
      `  --verbose              Print per-test diagnostic logs even on passing tests\n` +
      `  --json                 Emit machine-readable JSON output\n` +
      `  -h, --help             Print this usage message and exit 0\n`,
  );
}

function parseCliArgs(argv) {
  const opts = {
    tier: "all",
    list: false,
    areas: [],
    filter: null,
    failFast: false,
    verbose: false,
    json: false,
    help: false,
  };

  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "-h" || arg === "--help") {
      opts.help = true;
    } else if (arg === "--list") {
      opts.list = true;
    } else if (arg === "--fail-fast") {
      opts.failFast = true;
    } else if (arg === "--verbose") {
      opts.verbose = true;
    } else if (arg === "--json") {
      opts.json = true;
    } else if (arg === "--tier" || arg.startsWith("--tier=")) {
      const val = arg.includes("=") ? arg.slice("--tier=".length) : argv[++i];
      if (val === undefined || val === "") {
        throw new Error("Missing value for --tier (expected 1, 2, 3, 4, or all)");
      }
      if (val === "all") {
        opts.tier = "all";
      } else if (/^[1-4]$/.test(val)) {
        opts.tier = Number(val);
      } else {
        throw new Error(`Invalid --tier value "${val}" (expected 1, 2, 3, 4, or all)`);
      }
    } else if (arg === "--area" || arg.startsWith("--area=")) {
      const val = arg.includes("=") ? arg.slice("--area=".length) : argv[++i];
      if (val === undefined || val.trim() === "") {
        throw new Error(`Missing value for --area (valid: ${FEATURE_AREAS.join(", ")})`);
      }
      const slugs = val.split(",").map((s) => s.trim()).filter(Boolean);
      if (slugs.length === 0) {
        throw new Error(`Invalid empty --area value`);
      }
      for (const slug of slugs) {
        if (!AREA_SET.has(slug)) {
          throw new Error(`Unknown --area slug "${slug}" (valid: ${FEATURE_AREAS.join(", ")})`);
        }
        if (!opts.areas.includes(slug)) opts.areas.push(slug);
      }
    } else if (arg === "--filter" || arg.startsWith("--filter=")) {
      const val = arg.includes("=") ? arg.slice("--filter=".length) : argv[++i];
      if (val === undefined || val.trim() === "") {
        throw new Error("Missing non-empty value for --filter");
      }
      opts.filter = val.trim();
    } else {
      throw new Error(`Unknown argument: "${arg}"`);
    }
  }

  return opts;
}

function selectTests(allTests, opts) {
  return allTests.filter((t) => {
    if (opts.tier !== "all" && t.tier !== opts.tier) return false;
    if (opts.areas.length > 0) {
      const matchesArea = opts.areas.some((slug) => t.area === slug || t.areas.includes(slug));
      if (!matchesArea) return false;
    }
    if (opts.filter) {
      const needle = opts.filter.toLowerCase();
      const hay = `${t.id} ${t.title} ${t.area} ${t.areas.join(" ")}`.toLowerCase();
      if (!hay.includes(needle)) return false;
    }
    return true;
  });
}

function printListing(selected, allTests, opts) {
  const fullSummary = summarizeRegistry(allTests);
  const selectedSummary = summarizeRegistry(selected);

  if (opts.json) {
    process.stdout.write(
      JSON.stringify(
        {
          tier: opts.tier,
          selectedCount: selected.length,
          summary: selectedSummary,
          registrySummary: fullSummary,
          tests: selected.map((t) => ({
            id: t.id,
            tier: t.tier,
            area: t.area,
            areas: t.areas,
            features: t.features,
            title: t.title,
            timeoutMs: t.timeoutMs,
          })),
        },
        null,
        2,
      ) + "\n",
    );
    return;
  }

  console.log(`ads-simulator E2E Test Registry (tier=${opts.tier}, matched=${selected.length})`);
  console.log("");

  for (const tier of VALID_TIERS) {
    const tierTests = selected.filter((t) => t.tier === tier);
    if (tierTests.length === 0) continue;
    console.log(`=== ${TIER_LABELS[tier]} (${tierTests.length} tests) ===`);
    if (tier === 1 || tier === 2) {
      for (const slug of FEATURE_AREAS) {
        const areaTests = tierTests.filter((t) => t.area === slug);
        if (areaTests.length === 0) continue;
        console.log(`  [${slug}] — ${AREA_LABELS[slug]} (${areaTests.length} tests)`);
        for (const t of areaTests) {
          console.log(`    ${t.id.padEnd(10)} ${t.title}  [${t.features.join(",")}]`);
        }
      }
    } else {
      const groupLabel = tier === 3 ? "cross-feature" : "workflow";
      console.log(`  [${groupLabel}] (${tierTests.length} tests)`);
      for (const t of tierTests) {
        const meta = tier === 3 ? t.areas.join(" x ") : t.features.join(",");
        console.log(`    ${t.id.padEnd(10)} ${t.title}  [${meta}]`);
      }
    }
    console.log("");
  }

  const fmtAreaCounts = (map) => FEATURE_AREAS.map((a) => `${a}=${map[a]}`).join(", ");
  console.log("Summary Counts (selected):");
  console.log(`  Tier 1: ${selectedSummary.byTier[1]} (${fmtAreaCounts(selectedSummary.tier1ByArea)})`);
  console.log(`  Tier 2: ${selectedSummary.byTier[2]} (${fmtAreaCounts(selectedSummary.tier2ByArea)})`);
  console.log(`  Tier 3: ${selectedSummary.byTier[3]}`);
  console.log(`  Tier 4: ${selectedSummary.byTier[4]}`);
  console.log(`  Total:  ${selectedSummary.total}`);
}

async function runSingleTest(test) {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), `ads-e2e-${test.id.toLowerCase()}-`));
  const logs = [];
  const start = Date.now();
  const ctx = {
    repoRoot: REPO_ROOT,
    tmpDir,
    log(msg) {
      logs.push(String(msg));
    },
  };

  let timer = null;
  try {
    const timeoutPromise = new Promise((_, reject) => {
      timer = setTimeout(() => {
        reject(new Error(`Test ${test.id} timed out after ${test.timeoutMs}ms`));
      }, test.timeoutMs);
    });
    await Promise.race([Promise.resolve().then(() => test.fn(ctx)), timeoutPromise]);
    return {
      id: test.id,
      tier: test.tier,
      area: test.area,
      title: test.title,
      passed: true,
      durationMs: Date.now() - start,
      logs,
      error: null,
    };
  } catch (err) {
    return {
      id: test.id,
      tier: test.tier,
      area: test.area,
      title: test.title,
      passed: false,
      durationMs: Date.now() - start,
      logs,
      error: err instanceof Error ? err : new Error(String(err)),
    };
  } finally {
    if (timer) clearTimeout(timer);
    if (process.env.E2E_KEEP_TMP !== "1") {
      fs.rmSync(tmpDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    }
  }
}

async function main() {
  let opts;
  try {
    opts = parseCliArgs(process.argv.slice(2));
  } catch (err) {
    process.stderr.write(`Error: ${err.message}\n\n`);
    printUsage(process.stderr);
    process.exit(2);
  }

  if (opts.help) {
    printUsage(process.stdout);
    process.exit(0);
  }

  const selected = selectTests(ALL_TESTS, opts);

  if (opts.list) {
    printListing(selected, ALL_TESTS, opts);
    process.exit(0);
  }

  if (selected.length === 0) {
    const msg = `No tests matched the given filters (tier=${opts.tier}, area=${opts.areas.join(",") || "any"}, filter=${opts.filter ?? "none"}).`;
    if (opts.json) {
      process.stdout.write(JSON.stringify({ ok: false, error: msg, counts: { selected: 0, passed: 0, failed: 0 }, results: [] }, null, 2) + "\n");
    } else {
      process.stderr.write(`FAIL: ${msg}\n`);
    }
    process.exit(1);
  }

  const startAll = Date.now();
  const results = [];
  let passed = 0;
  let failed = 0;

  if (!opts.json) {
    console.log(`Running ${selected.length} opaque-box E2E test(s) (tier=${opts.tier})...`);
  }

  for (const test of selected) {
    const res = await runSingleTest(test);
    results.push(res);

    if (res.passed) {
      passed += 1;
      if (!opts.json) {
        console.log(`  PASS  ${res.id.padEnd(10)} (${res.durationMs}ms) ${res.title}`);
        if (opts.verbose && res.logs.length > 0) {
          for (const line of res.logs) console.log(`        [log] ${line}`);
        }
      }
    } else {
      failed += 1;
      if (!opts.json) {
        console.log(`  FAIL  ${res.id.padEnd(10)} (${res.durationMs}ms) ${res.title}`);
        console.log(`        ${res.error?.name ?? "Error"}: ${res.error?.message ?? "unknown failure"}`);
        if (res.logs.length > 0) {
          for (const line of res.logs) console.log(`        [log] ${line}`);
        }
        if (opts.verbose && res.error?.stack) {
          for (const sline of res.error.stack.split("\n").slice(1, 6)) {
            console.log(`        ${sline.trim()}`);
          }
        }
      }
      if (opts.failFast) {
        break;
      }
    }
  }

  const durationMs = Date.now() - startAll;
  const ok = selected.length > 0 && failed === 0 && passed === selected.length;

  if (opts.json) {
    process.stdout.write(
      JSON.stringify(
        {
          ok,
          tier: opts.tier,
          durationMs,
          counts: {
            selected: selected.length,
            executed: results.length,
            passed,
            failed,
          },
          results: results.map((r) => ({
            id: r.id,
            tier: r.tier,
            area: r.area,
            title: r.title,
            passed: r.passed,
            durationMs: r.durationMs,
            error: r.error ? `${r.error.name}: ${r.error.message}` : null,
          })),
        },
        null,
        2,
      ) + "\n",
    );
  } else {
    console.log("");
    console.log(
      `E2E Summary: ${passed} passed, ${failed} failed, ${results.length}/${selected.length} executed in ${durationMs}ms`,
    );
  }

  process.exit(ok ? 0 : 1);
}

main().catch((err) => {
  process.stderr.write(`Fatal runner error: ${err?.stack ?? err}\n`);
  process.exit(1);
});
