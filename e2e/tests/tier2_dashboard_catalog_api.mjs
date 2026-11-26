/**
 * Tier 2 (Boundary / Edge / Negative) — Area 5: Dashboard Feature Catalog API & Live Introspection
 */

import assert from "node:assert/strict";
import { defineTest } from "../registry.mjs";
import {
  httpGetJson,
  httpGetText,
  httpRequest,
  startDashboardServer,
  validateBudgetSvg,
  validateBudgetTimeSeries,
  validateMarketplaceApiResponse,
  validateSimulatorCatalog,
} from "../helpers.mjs";

const AREA = "dashboard-catalog-api";

export const tests = [
  defineTest({
    id: "T2-A5-01",
    tier: 2,
    area: AREA,
    features: ["F15"],
    title: "Invalid seed query parameters on GET /api/marketplace return 400 JSON without crashing server",
    async fn() {
      const server = await startDashboardServer();
      try {
        for (const query of ["seed=not-a-number", "seed=", "seed=3.14"]) {
          const res = await httpGetJson(`${server.url}/api/marketplace?${query}`, { strictJson: false });
          assert.strictEqual(res.status, 400, `Expected 400 for ?${query}, got ${res.status}`);
          assert.ok(res.contentType.includes("application/json"), `Expected JSON error content-type for ?${query}`);
          assert.ok(
            res.data && typeof res.data.error === "string" && res.data.error.trim().length > 0,
            `Expected non-empty error string for ?${query}, got ${res.text}`,
          );
        }
        const health = await httpGetJson(`${server.url}/health`);
        assert.strictEqual(health.status, 200);
      } finally {
        await server.stop();
      }
    },
  }),

  defineTest({
    id: "T2-A5-02",
    tier: 2,
    area: AREA,
    features: ["F15"],
    title: "Extreme signed 64-bit seeds (-42, 0, i64::MAX, i64::MIN) succeed and i64 overflow returns 400",
    async fn() {
      const server = await startDashboardServer();
      try {
        const validSeeds = [
          { str: "-42", re: /"seed"\s*:\s*-42\b/ },
          { str: "0", re: /"seed"\s*:\s*0\b/ },
          { str: "9223372036854775807", re: /"seed"\s*:\s*9223372036854775807\b/ },
          { str: "-9223372036854775808", re: /"seed"\s*:\s*-9223372036854775808\b/ },
        ];
        for (const s of validSeeds) {
          const res = await httpGetJson(`${server.url}/api/marketplace?seed=${s.str}`);
          assert.strictEqual(res.status, 200, `Expected 200 for seed=${s.str}`);
          assert.ok(s.re.test(res.text), `Raw JSON must preserve exact seed ${s.str}`);
          validateMarketplaceApiResponse(res.data, { expectedSeed: null });
        }

        const overflow = await httpGetJson(
          `${server.url}/api/marketplace?seed=99999999999999999999999`,
          { strictJson: false },
        );
        assert.strictEqual(overflow.status, 400, "i64 overflow seed must return 400");
        assert.ok(typeof overflow.data?.error === "string" && overflow.data.error.length > 0);
      } finally {
        await server.stop();
      }
    },
  }),

  defineTest({
    id: "T2-A5-03",
    tier: 2,
    area: AREA,
    features: ["F15"],
    title: "Unknown API and page routes return 404 JSON {error}",
    async fn() {
      const server = await startDashboardServer();
      try {
        const calls = [
          { method: "GET", path: "/api/nonexistent" },
          { method: "GET", path: "/does-not-exist" },
          { method: "POST", path: "/api/unknown" },
        ];
        for (const c of calls) {
          const res = await httpRequest(`${server.url}${c.path}`, { method: c.method });
          assert.strictEqual(res.status, 404, `Expected 404 for ${c.method} ${c.path}, got ${res.status}`);
          assert.ok(res.contentType.includes("application/json"), `Expected JSON 404 for ${c.method} ${c.path}`);
          const parsed = res.json();
          assert.ok(typeof parsed.error === "string" && parsed.error.trim().length > 0);
        }
      } finally {
        await server.stop();
      }
    },
  }),

  defineTest({
    id: "T2-A5-04",
    tier: 2,
    area: AREA,
    features: ["F15"],
    title: "Unsupported HTTP methods on valid routes return 404 or 405 and preserve server health",
    async fn() {
      const server = await startDashboardServer();
      try {
        const calls = [
          { method: "POST", path: "/api/catalog" },
          { method: "DELETE", path: "/health" },
          { method: "GET", path: "/api/sim-agent/run" },
        ];
        for (const c of calls) {
          const res = await httpRequest(`${server.url}${c.path}`, { method: c.method });
          assert.ok(
            res.status === 404 || res.status === 405,
            `Expected 404 or 405 for ${c.method} ${c.path}, got ${res.status}`,
          );
        }
        const health = await httpGetJson(`${server.url}/health`);
        assert.strictEqual(health.status, 200);
        assert.deepStrictEqual(health.data, { status: "ok" });
      } finally {
        await server.stop();
      }
    },
  }),

  defineTest({
    id: "T2-A5-05",
    tier: 2,
    area: AREA,
    features: ["F15"],
    title: "Path traversal attempts return 400/404 and never leak repository files",
    async fn() {
      const server = await startDashboardServer();
      try {
        for (const badPath of [
          "/../../Cargo.toml",
          "/api/artifacts/../../Cargo.toml",
          "/%2e%2e/Cargo.toml",
        ]) {
          const res = await httpGetText(`${server.url}${badPath}`);
          assert.ok(
            res.status === 400 || res.status === 404,
            `Expected 400/404 for traversal ${badPath}, got ${res.status}`,
          );
          assert.ok(!res.text.includes("[workspace]"), `Path traversal leaked workspace Cargo.toml for ${badPath}`);
          assert.ok(!res.text.includes("[package]"), `Path traversal leaked Cargo.toml for ${badPath}`);
        }
      } finally {
        await server.stop();
      }
    },
  }),

  defineTest({
    id: "T2-A5-06",
    tier: 2,
    area: AREA,
    features: ["F14", "F15"],
    title: "Ephemeral --port 0 binds cleanly and handles 10 concurrent API requests without errors",
    async fn() {
      const server = await startDashboardServer();
      try {
        assert.ok(Number.isInteger(server.port) && server.port > 0, `Expected ephemeral port > 0, got ${server.port}`);
        const tasks = [
          httpGetJson(`${server.url}/health`),
          httpGetJson(`${server.url}/api/catalog`),
          httpGetJson(`${server.url}/api/marketplace?seed=1`),
          httpGetJson(`${server.url}/api/marketplace?seed=2`),
          httpGetJson(`${server.url}/api/marketplace?seed=3`),
          httpGetJson(`${server.url}/api/marketplace?seed=4`),
          httpGetJson(`${server.url}/api/marketplace?seed=5`),
          httpGetJson(`${server.url}/api/sim-agent/latest`),
          httpGetText(`${server.url}/api/artifacts/budget-plot`),
          httpGetText(`${server.url}/`),
        ];
        const responses = await Promise.all(tasks);
        for (let i = 0; i < responses.length; i++) {
          assert.strictEqual(responses[i].status, 200, `Concurrent request #${i} returned ${responses[i].status}`);
        }
        validateSimulatorCatalog(responses[1].data);
        validateMarketplaceApiResponse(responses[2].data, { expectedSeed: 1 });
        validateBudgetTimeSeries(responses[7].data);
        validateBudgetSvg(responses[8].text, responses[7].data);
      } finally {
        await server.stop();
      }
    },
  }),
];
