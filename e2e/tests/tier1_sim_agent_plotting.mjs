/**
 * Tier 1 (Happy-Path) — Area 4: sim-agent 30-Day Execution & Budget Plotting
 */

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { defineTest } from "../registry.mjs";
import {
  readJsonFile,
  runBinaryOk,
  validateBudgetCsv,
  validateBudgetSvg,
  validateBudgetTimeSeries,
  validateSimulationAndTimeSeriesParity,
  validateSimulationReport,
} from "../helpers.mjs";

const AREA = "sim-agent-plotting";

export const tests = [
  defineTest({
    id: "T1-A4-01",
    tier: 1,
    area: AREA,
    features: ["F9", "F10", "F11", "F12", "F13"],
    title: "Default baseline 30-day execution produces golden ledger, CSV, and dual-panel SVG",
    async fn(ctx) {
      const outDir = path.join(ctx.tmpDir, "baseline");
      const res = await runBinaryOk("sim-agent", ["--output-dir", outDir], { timeoutMs: 45_000 });
      assert.ok(res.stdout.trim().length > 0, "sim-agent stdout must print run summary");

      const simPath = path.join(outDir, "simulation.json");
      const tsPath = path.join(outDir, "budget_timeseries.json");
      const csvPath = path.join(outDir, "budget_timeseries.csv");
      const svgPath = path.join(outDir, "budget_spent_30d.svg");
      for (const p of [simPath, tsPath, csvPath, svgPath]) {
        assert.ok(fs.existsSync(p), `Missing expected sim-agent artifact: ${path.basename(p)}`);
      }

      const simReport = readJsonFile(simPath);
      const ts = readJsonFile(tsPath);
      const csvText = fs.readFileSync(csvPath, "utf8");
      const svgText = fs.readFileSync(svgPath, "utf8");

      validateSimulationReport(simReport, {
        seed: 42,
        campaign_budget_cents: null,
        campaign_budget_min_cents: 100000,
        campaign_budget_max_cents: 1000000,
        cpc_cents: 100,
        impressions: 1000000,
      });
      validateBudgetTimeSeries(ts, {
        marketplace_seed: 42,
        sim_seed: 42,
        campaign_budget_cents: null,
        campaign_budget_min_cents: 100000,
        campaign_budget_max_cents: 1000000,
        cpc_cents: 100,
        impressions: 1000000,
        total_campaigns: 500,
      });
      assert.strictEqual(ts.summary.total_budget_cents, 279898700);
      assert.strictEqual(ts.summary.total_clicks, 1968);
      assert.strictEqual(ts.summary.total_spend_cents, 196800);
      assert.strictEqual(ts.summary.remaining_budget_cents, 279701900);
      assert.strictEqual(ts.summary.exhausted_campaigns, 0);
      assert.strictEqual(ts.days[0].daily_spend_cents, 6500);
      assert.strictEqual(ts.days[7].daily_spend_cents, 6100);
      assert.strictEqual(ts.summary.min_daily_spend_cents, 4900);
      const distinctBudgets = new Set(simReport.campaigns.map((c) => c.budget_cents));
      assert.ok(distinctBudgets.size > 100, "Default run must randomize campaign budgets across [$1,000, $10,000]");
      assert.ok(fs.existsSync(path.join(outDir, "analysis.json")), "sim-agent must export analysis.json");
      const analysis = readJsonFile(path.join(outDir, "analysis.json"));
      assert.strictEqual(analysis.budget_depletion.total_spend_cents, 196800);
      assert.strictEqual(analysis.ctr_performance.total_clicks, 1968);

      validateSimulationAndTimeSeriesParity(simReport, ts);
      validateBudgetCsv(csvText, ts);
      validateBudgetSvg(svgText, ts);
    },
  }),

  defineTest({
    id: "T1-A4-02",
    tier: 1,
    area: AREA,
    features: ["F9", "F11", "F12", "F13"],
    title: "Natural user-independent mode (--no-impression-target) runs 12k/day across 30 days",
    async fn(ctx) {
      const outDir = path.join(ctx.tmpDir, "natural");
      await runBinaryOk("sim-agent", ["--output-dir", outDir, "--seed", "42", "--no-impression-target"]);

      const ts = readJsonFile(path.join(outDir, "budget_timeseries.json"));
      const csvText = fs.readFileSync(path.join(outDir, "budget_timeseries.csv"), "utf8");
      const svgText = fs.readFileSync(path.join(outDir, "budget_spent_30d.svg"), "utf8");

      validateBudgetTimeSeries(ts, { sim_seed: 42, impressions: null, total_campaigns: 500 });
      assert.strictEqual(ts.summary.total_impressions, 360000);
      assert.strictEqual(ts.summary.total_clicks, 719);
      assert.strictEqual(ts.summary.total_spend_cents, 71900);
      for (const d of ts.days) {
        assert.strictEqual(d.impressions, 12000, `Day ${d.day} must have 12000 natural impressions`);
      }
      validateBudgetCsv(csvText, ts);
      validateBudgetSvg(svgText, ts);
    },
  }),

  defineTest({
    id: "T1-A4-03",
    tier: 1,
    area: AREA,
    features: ["F9", "F10", "F11", "F12", "F13"],
    title: "Custom marketplace-seed, seed, budget, CPC, and target impressions all propagate accurately",
    async fn(ctx) {
      const outDir = path.join(ctx.tmpDir, "custom");
      await runBinaryOk("sim-agent", [
        "--output-dir", outDir,
        "--marketplace-seed", "7",
        "--seed", "99",
        "--campaign-budget-cents", "25000",
        "--cpc-cents", "250",
        "--impressions", "300000",
      ]);

      const ts = readJsonFile(path.join(outDir, "budget_timeseries.json"));
      const csvText = fs.readFileSync(path.join(outDir, "budget_timeseries.csv"), "utf8");
      const svgText = fs.readFileSync(path.join(outDir, "budget_spent_30d.svg"), "utf8");

      validateBudgetTimeSeries(ts, {
        marketplace_seed: 7,
        sim_seed: 99,
        campaign_budget_cents: 25000,
        cpc_cents: 250,
        impressions: 300000,
        total_campaigns: 500,
      });
      assert.strictEqual(ts.summary.total_budget_cents, 12500000);
      for (const d of ts.days) {
        assert.strictEqual(d.impressions, 10000);
        assert.strictEqual(d.daily_spend_cents, d.clicks * 250);
      }
      validateBudgetCsv(csvText, ts);
      validateBudgetSvg(svgText, ts);
    },
  }),

  defineTest({
    id: "T1-A4-04",
    tier: 1,
    area: AREA,
    features: ["F9", "F12", "F13"],
    title: "External --marketplace JSON file achieves exact parity with in-memory --marketplace-seed",
    async fn(ctx) {
      const mktDir = path.join(ctx.tmpDir, "mkt-123");
      const outA = path.join(ctx.tmpDir, "from-file");
      const outB = path.join(ctx.tmpDir, "in-memory");

      await runBinaryOk("ads-simulator", ["--seed", "123", "--output-dir", mktDir]);
      await runBinaryOk("sim-agent", [
        "--marketplace", path.join(mktDir, "marketplace.json"),
        "--seed", "42",
        "--impressions", "150000",
        "--output-dir", outA,
      ]);
      await runBinaryOk("sim-agent", [
        "--marketplace-seed", "123",
        "--seed", "42",
        "--impressions", "150000",
        "--output-dir", outB,
      ]);

      const tsA = readJsonFile(path.join(outA, "budget_timeseries.json"));
      const tsB = readJsonFile(path.join(outB, "budget_timeseries.json"));
      validateBudgetTimeSeries(tsA, { marketplace_seed: 123, sim_seed: 42, impressions: 150000 });
      validateBudgetTimeSeries(tsB, { marketplace_seed: 123, sim_seed: 42, impressions: 150000 });
      assert.deepStrictEqual(tsA, tsB, "External --marketplace and in-memory --marketplace-seed must produce identical BudgetTimeSeries");

      const svgA = fs.readFileSync(path.join(outA, "budget_spent_30d.svg"), "utf8");
      const svgB = fs.readFileSync(path.join(outB, "budget_spent_30d.svg"), "utf8");
      assert.strictEqual(svgA, svgB, "External --marketplace and in-memory --marketplace-seed must produce byte-identical SVG");
    },
  }),

  defineTest({
    id: "T1-A4-05",
    tier: 1,
    area: AREA,
    features: ["F9", "F12", "F13"],
    title: "Repeated identical sim-agent runs are byte-for-byte deterministic across all 4 artifacts",
    async fn(ctx) {
      const dir1 = path.join(ctx.tmpDir, "det-1");
      const dir2 = path.join(ctx.tmpDir, "det-2");
      await runBinaryOk("sim-agent", ["--seed", "42", "--impressions", "300000", "--output-dir", dir1]);
      await runBinaryOk("sim-agent", ["--seed", "42", "--impressions", "300000", "--output-dir", dir2]);

      for (const file of [
        "simulation.json",
        "budget_timeseries.json",
        "budget_timeseries.csv",
        "budget_spent_30d.svg",
        "analysis.json",
      ]) {
        const b1 = fs.readFileSync(path.join(dir1, file));
        const b2 = fs.readFileSync(path.join(dir2, file));
        assert.strictEqual(Buffer.compare(b1, b2), 0, `${file} must be byte-for-byte deterministic`);
      }
    },
  }),

  defineTest({
    id: "T1-A4-06",
    tier: 1,
    area: AREA,
    features: ["F10", "F11", "F12"],
    title: "Mid-month campaign budget exhaustion tracks active campaigns, utilization, and SVG cleanly",
    async fn(ctx) {
      const outDir = path.join(ctx.tmpDir, "exhaust");
      await runBinaryOk("sim-agent", [
        "--output-dir", outDir,
        "--seed", "42",
        "--campaign-budget-cents", "100",
        "--cpc-cents", "100",
        "--no-impression-target",
      ]);

      const ts = readJsonFile(path.join(outDir, "budget_timeseries.json"));
      const csvText = fs.readFileSync(path.join(outDir, "budget_timeseries.csv"), "utf8");
      const svgText = fs.readFileSync(path.join(outDir, "budget_spent_30d.svg"), "utf8");

      validateBudgetTimeSeries(ts, {
        campaign_budget_cents: 100,
        cpc_cents: 100,
        impressions: null,
        total_campaigns: 500,
      });
      assert.strictEqual(ts.summary.exhausted_campaigns, 500);
      assert.strictEqual(ts.days[29].active_campaigns_end_of_day, 0);
      assert.strictEqual(ts.summary.total_spend_cents, 50000);
      assert.ok(ts.summary.insufficient_budget_opportunities > 0);
      validateBudgetCsv(csvText, ts);
      validateBudgetSvg(svgText, ts);
    },
  }),

  defineTest({
    id: "T1-A4-07",
    tier: 1,
    area: AREA,
    features: ["F9", "F10", "F12"],
    title: "Standalone simulation-analyzer CLI produces auto-analysis matching sim-agent analysis.json",
    async fn(ctx) {
      const outDir = path.join(ctx.tmpDir, "run-for-analysis");
      await runBinaryOk("sim-agent", [
        "--output-dir", outDir,
        "--seed", "42",
        "--impressions", "1000000",
      ]);

      const cliOutPath = path.join(ctx.tmpDir, "cli-analysis.json");
      const res = await runBinaryOk("simulation-analyzer", [
        "--report", path.join(outDir, "simulation.json"),
        "--output", cliOutPath,
      ]);
      assert.ok(res.stdout.includes("Budget Depletion") || res.stdout.includes("Utilization"), "CLI stdout must print summary");

      const agentAnalysis = readJsonFile(path.join(outDir, "analysis.json"));
      const cliAnalysis = readJsonFile(cliOutPath);
      assert.deepStrictEqual(cliAnalysis, agentAnalysis, "simulation-analyzer CLI output must match sim-agent analysis.json");

      assert.strictEqual(cliAnalysis.ranker, "R1");
      assert.strictEqual(cliAnalysis.slate_size, 20);
      assert.strictEqual(cliAnalysis.budget_depletion.total_spend_cents, 196800);
      assert.strictEqual(cliAnalysis.budget_depletion.daily_depletion.length, 30);
      assert.ok(cliAnalysis.ctr_performance.overall_ctr_pct > 0.15 && cliAnalysis.ctr_performance.overall_ctr_pct < 0.30);
      assert.ok(cliAnalysis.ctr_performance.effective_cpm_cents > 150 && cliAnalysis.ctr_performance.effective_cpm_cents < 300);
      assert.ok(cliAnalysis.campaign_distribution.gini_spend_coefficient >= 0 && cliAnalysis.campaign_distribution.gini_spend_coefficient <= 1);
      assert.ok(Array.isArray(cliAnalysis.findings) && cliAnalysis.findings.length >= 2);
      assert.strictEqual(cliAnalysis.campaign_distribution.top_campaigns_by_spend.length, 5);
    },
  }),
];
