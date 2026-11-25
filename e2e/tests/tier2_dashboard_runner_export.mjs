/**
 * Tier 2 (Boundary / Edge / Negative) — Area 6: Dashboard Live Sim-Agent Runner & Static Export
 */

import assert from "node:assert/strict";
import path from "node:path";
import { defineTest } from "../registry.mjs";
import {
  httpGetJson,
  httpPostJson,
  httpPostRaw,
  runBinary,
  runBinaryOk,
  startDashboardServer,
  validateBudgetSvg,
  validateBudgetTimeSeries,
  validateStaticExportBundle,
} from "../helpers.mjs";

const AREA = "dashboard-runner-export";

export const tests = [
  defineTest({
    id: "T2-A6-01",
    tier: 2,
    area: AREA,
    features: ["F15"],
    title: "Malformed or empty JSON body to POST /api/sim-agent/run returns 400 JSON without crashing",
    async fn() {
      const server = await startDashboardServer();
      try {
        for (const raw of ["{not valid json", ""]) {
          const res = await httpPostRaw(`${server.url}/api/sim-agent/run`, raw, "application/json", {
            strictJson: false,
          });
          assert.strictEqual(res.status, 400, `Expected 400 for body=${JSON.stringify(raw)}, got ${res.status}`);
          assert.ok(res.contentType.includes("application/json"));
          assert.ok(
            res.data && typeof res.data.error === "string" && res.data.error.trim().length > 0,
            `Expected error message JSON, got ${res.text}`,
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
    id: "T2-A6-02",
    tier: 2,
    area: AREA,
    features: ["F15"],
    title: "Invalid simulation parameters (cpc=0, negative budget, bad type) return 400 and preserve prior state",
    async fn() {
      const server = await startDashboardServer();
      try {
        const before = await httpGetJson(`${server.url}/api/sim-agent/latest`);
        assert.strictEqual(before.status, 200);

        for (const badPayload of [
          { cpc_cents: 0 },
          { campaign_budget_cents: -100 },
          { seed: "bad" },
        ]) {
          const res = await httpPostJson(`${server.url}/api/sim-agent/run`, badPayload, {
            strictJson: false,
          });
          assert.strictEqual(
            res.status,
            400,
            `Expected 400 for payload ${JSON.stringify(badPayload)}, got ${res.status}`,
          );
          assert.ok(typeof res.data?.error === "string" && res.data.error.trim().length > 0);
        }

        const after = await httpGetJson(`${server.url}/api/sim-agent/latest`);
        assert.strictEqual(after.status, 200);
        assert.deepStrictEqual(after.data, before.data, "Failed POST runs must never clobber valid latest state");
      } finally {
        await server.stop();
      }
    },
  }),

  defineTest({
    id: "T2-A6-03",
    tier: 2,
    area: AREA,
    features: ["F15"],
    title: "Unreachable impression target via POST /api/sim-agent/run returns 400 JSON",
    async fn() {
      const server = await startDashboardServer();
      try {
        const res = await httpPostJson(
          `${server.url}/api/sim-agent/run`,
          { campaign_budget_cents: 50, cpc_cents: 100, impressions: 5000 },
          { strictJson: false },
        );
        assert.strictEqual(res.status, 400, `Expected 400 for unreachable impression target, got ${res.status}`);
        assert.ok(typeof res.data?.error === "string" && res.data.error.trim().length > 0);
        const health = await httpGetJson(`${server.url}/health`);
        assert.strictEqual(health.status, 200);
      } finally {
        await server.stop();
      }
    },
  }),

  defineTest({
    id: "T2-A6-04",
    tier: 2,
    area: AREA,
    features: ["F15"],
    title: "Zero-spend natural replay via POST /api/sim-agent/run returns 200 with zero-NaN SVG",
    async fn() {
      const server = await startDashboardServer();
      try {
        const res = await httpPostJson(`${server.url}/api/sim-agent/run`, {
          campaign_budget_cents: 50,
          cpc_cents: 100,
          impressions: null,
        });
        assert.strictEqual(res.status, 200);
        validateBudgetTimeSeries(res.data.timeseries, {
          campaign_budget_cents: 50,
          cpc_cents: 100,
          impressions: null,
        });
        assert.strictEqual(res.data.timeseries.summary.total_spend_cents, 0);
        assert.strictEqual(res.data.timeseries.summary.utilization_pct, 0);
        validateBudgetSvg(res.data.svg, res.data.timeseries);
      } finally {
        await server.stop();
      }
    },
  }),

  defineTest({
    id: "T2-A6-05",
    tier: 2,
    area: AREA,
    features: ["F15"],
    title: "Empty JSON object {} to POST /api/sim-agent/run applies documented defaults",
    async fn() {
      const server = await startDashboardServer();
      try {
        const res = await httpPostJson(`${server.url}/api/sim-agent/run`, {}, { timeoutMs: 45_000 });
        assert.strictEqual(res.status, 200);
        validateBudgetTimeSeries(res.data.timeseries, {
          sim_seed: 42,
          campaign_budget_cents: null,
          campaign_budget_min_cents: 100000,
          campaign_budget_max_cents: 1000000,
          cpc_cents: 100,
        });
        validateBudgetSvg(res.data.svg, res.data.timeseries);
      } finally {
        await server.stop();
      }
    },
  }),

  defineTest({
    id: "T2-A6-06",
    tier: 2,
    area: AREA,
    features: ["F15", "F17"],
    title: "Non-existent --sim-report fails cleanly and nested non-existent --export-dir auto-creates",
    async fn(ctx) {
      const missingRes = await runBinary(
        "ads-simulator-dashboard",
        ["--sim-report", path.join(ctx.tmpDir, "missing.json"), "--port", "0"],
        { timeoutMs: 15_000 },
      );
      assert.notStrictEqual(missingRes.code, 0, "Non-existent --sim-report must exit non-zero");
      assert.ok(missingRes.stderr.trim().length > 0, "Expected stderr error for missing --sim-report");

      const nestedExport = path.join(ctx.tmpDir, "deep", "nested", "bundle");
      await runBinaryOk("ads-simulator-dashboard", ["--export-dir", nestedExport], { timeoutMs: 30_000 });
      validateStaticExportBundle(nestedExport);
    },
  }),
];
