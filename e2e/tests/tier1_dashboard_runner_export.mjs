/**
 * Tier 1 (Happy-Path) — Area 6: Dashboard Live Sim-Agent Runner & Static Export
 */

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { defineTest } from "../registry.mjs";
import {
  httpGetJson,
  httpGetText,
  httpPostJson,
  readJsonFile,
  runBinaryOk,
  startDashboardServer,
  validateBudgetSvg,
  validateBudgetTimeSeries,
  validateStaticExportBundle,
} from "../helpers.mjs";

const AREA = "dashboard-runner-export";

export const tests = [
  defineTest({
    id: "T1-A6-01",
    tier: 1,
    area: AREA,
    features: ["F15"],
    title: "POST /api/sim-agent/run with baseline 1M-impression payload returns golden timeseries and SVG",
    async fn() {
      const server = await startDashboardServer();
      try {
        const res = await httpPostJson(
          `${server.url}/api/sim-agent/run`,
          {
            marketplace_seed: 42,
            seed: 42,
            campaign_budget_cents: 10000,
            cpc_cents: 100,
            impressions: 1000000,
          },
          { timeoutMs: 45_000 },
        );
        assert.strictEqual(res.status, 200);
        assert.ok(res.contentType.includes("application/json"));
        assert.ok(res.data && res.data.timeseries && res.data.svg, "Response must contain { timeseries, svg }");

        validateBudgetTimeSeries(res.data.timeseries, {
          marketplace_seed: 42,
          sim_seed: 42,
          campaign_budget_cents: 10000,
          cpc_cents: 100,
          impressions: 1000000,
        });
        assert.strictEqual(res.data.timeseries.summary.total_clicks, 2028);
        assert.strictEqual(res.data.timeseries.summary.total_spend_cents, 202800);
        assert.ok(res.data.analysis, "POST /api/sim-agent/run should include SimulationAnalysis");
        assert.strictEqual(res.data.analysis.budget_depletion.total_spend_cents, 202800);
        assert.strictEqual(res.data.analysis.ctr_performance.total_clicks, 2028);
        validateBudgetSvg(res.data.svg, res.data.timeseries);
      } finally {
        await server.stop();
      }
    },
  }),

  defineTest({
    id: "T1-A6-02",
    tier: 1,
    area: AREA,
    features: ["F15"],
    title: "POST /api/sim-agent/run supports natural user-independent mode (impressions: null)",
    async fn() {
      const server = await startDashboardServer();
      try {
        const res = await httpPostJson(`${server.url}/api/sim-agent/run`, {
          seed: 42,
          campaign_budget_cents: 10000,
          cpc_cents: 100,
          impressions: null,
        });
        assert.strictEqual(res.status, 200);
        validateBudgetTimeSeries(res.data.timeseries, { sim_seed: 42, impressions: null });
        assert.strictEqual(res.data.timeseries.summary.total_impressions, 360000);
        assert.strictEqual(res.data.timeseries.summary.total_clicks, 744);
        assert.strictEqual(res.data.timeseries.summary.total_spend_cents, 74400);
        validateBudgetSvg(res.data.svg, res.data.timeseries);
      } finally {
        await server.stop();
      }
    },
  }),

  defineTest({
    id: "T1-A6-03",
    tier: 1,
    area: AREA,
    features: ["F15"],
    title: "POST /api/sim-agent/run updates active GET /api/sim-agent/latest and /api/artifacts/budget-plot",
    async fn() {
      const server = await startDashboardServer();
      try {
        const post = await httpPostJson(`${server.url}/api/sim-agent/run`, {
          marketplace_seed: 11,
          seed: 77,
          campaign_budget_cents: 20000,
          cpc_cents: 250,
          impressions: 120000,
        });
        assert.strictEqual(post.status, 200);
        validateBudgetTimeSeries(post.data.timeseries, {
          marketplace_seed: 11,
          sim_seed: 77,
          campaign_budget_cents: 20000,
          cpc_cents: 250,
          impressions: 120000,
        });

        const latest = await httpGetJson(`${server.url}/api/sim-agent/latest`);
        assert.strictEqual(latest.status, 200);
        assert.deepStrictEqual(latest.data, post.data.timeseries, "GET /api/sim-agent/latest must reflect latest POST run");

        const plot = await httpGetText(`${server.url}/api/artifacts/budget-plot`);
        assert.strictEqual(plot.status, 200);
        assert.ok(plot.contentType.includes("image/svg+xml"));
        assert.strictEqual(plot.text, post.data.svg, "GET /api/artifacts/budget-plot must match latest POST SVG");
      } finally {
        await server.stop();
      }
    },
  }),

  defineTest({
    id: "T1-A6-04",
    tier: 1,
    area: AREA,
    features: ["F15"],
    title: "Startup loads pre-computed --sim-report (budget_timeseries.json or simulation.json) & --plot-file",
    async fn(ctx) {
      const simDir = path.join(ctx.tmpDir, "precomp");
      await runBinaryOk("sim-agent", [
        "--seed", "123",
        "--campaign-budget-cents", "5000",
        "--cpc-cents", "200",
        "--impressions", "150000",
        "--output-dir", simDir,
      ]);
      const expectedTs = readJsonFile(path.join(simDir, "budget_timeseries.json"));
      const expectedSvg = fs.readFileSync(path.join(simDir, "budget_spent_30d.svg"), "utf8");

      // Dashboard A: budget_timeseries.json + plot-file
      const dashA = await startDashboardServer([
        "--sim-report", path.join(simDir, "budget_timeseries.json"),
        "--plot-file", path.join(simDir, "budget_spent_30d.svg"),
      ]);
      try {
        const latestA = await httpGetJson(`${dashA.url}/api/sim-agent/latest`);
        assert.strictEqual(latestA.status, 200);
        assert.deepStrictEqual(latestA.data, expectedTs);
        const plotA = await httpGetText(`${dashA.url}/api/artifacts/budget-plot`);
        assert.strictEqual(plotA.status, 200);
        assert.strictEqual(plotA.text, expectedSvg);
      } finally {
        await dashA.stop();
      }

      // Dashboard B: raw simulation.json
      const dashB = await startDashboardServer([
        "--sim-report", path.join(simDir, "simulation.json"),
      ]);
      try {
        const latestB = await httpGetJson(`${dashB.url}/api/sim-agent/latest`);
        assert.strictEqual(latestB.status, 200);
        validateBudgetTimeSeries(latestB.data, {
          sim_seed: 123,
          campaign_budget_cents: 5000,
          cpc_cents: 200,
          impressions: 150000,
        });
        assert.strictEqual(latestB.data.summary.total_spend_cents, expectedTs.summary.total_spend_cents);
        assert.strictEqual(latestB.data.summary.total_clicks, expectedTs.summary.total_clicks);
      } finally {
        await dashB.stop();
      }
    },
  }),

  defineTest({
    id: "T1-A6-05",
    tier: 1,
    area: AREA,
    features: ["F17"],
    title: "Static bundle export (--export-dir) writes complete 4-file offline bundle and exits 0",
    async fn(ctx) {
      const exportDir = path.join(ctx.tmpDir, "export-default");
      await runBinaryOk("ads-simulator-dashboard", ["--export-dir", exportDir], { timeoutMs: 30_000 });
      const bundle = validateStaticExportBundle(exportDir, { repoRoot: ctx.repoRoot });

      // Confirm index.html embeds offline bootstrap data (at least one catalog piece name)
      const firstPieceName = bundle.catalog.pieces[0].name;
      assert.ok(
        bundle.indexHtml.includes(firstPieceName) || /dashboard.?data|__BOOTSTRAP__|window\./i.test(bundle.indexHtml),
        "Exported index.html must contain embedded bootstrap data for offline rendering",
      );
    },
  }),

  defineTest({
    id: "T1-A6-06",
    tier: 1,
    area: AREA,
    features: ["F17"],
    title: "Static bundle export (--export-dir) respects custom --sim-report and --plot-file",
    async fn(ctx) {
      const simDir = path.join(ctx.tmpDir, "custom-sim");
      const exportDir = path.join(ctx.tmpDir, "custom-export");
      await runBinaryOk("sim-agent", [
        "--seed", "88",
        "--cpc-cents", "200",
        "--impressions", "90000",
        "--output-dir", simDir,
      ]);
      const expectedTs = readJsonFile(path.join(simDir, "budget_timeseries.json"));
      const expectedSvg = fs.readFileSync(path.join(simDir, "budget_spent_30d.svg"), "utf8");

      await runBinaryOk("ads-simulator-dashboard", [
        "--sim-report", path.join(simDir, "budget_timeseries.json"),
        "--plot-file", path.join(simDir, "budget_spent_30d.svg"),
        "--export-dir", exportDir,
      ]);

      const bundle = validateStaticExportBundle(exportDir, { expectedTimeseries: expectedTs });
      assert.strictEqual(bundle.svg, expectedSvg, "Exported SVG must match supplied --plot-file byte-for-byte");
    },
  }),
];
