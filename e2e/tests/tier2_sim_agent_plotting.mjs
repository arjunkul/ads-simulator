/**
 * Tier 2 (Boundary / Edge / Negative) — Area 4: sim-agent 30-Day Execution & Budget Plotting
 */

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { defineTest } from "../registry.mjs";
import {
  readJsonFile,
  runBinary,
  runBinaryOk,
  validateBudgetCsv,
  validateBudgetSvg,
  validateBudgetTimeSeries,
} from "../helpers.mjs";

const AREA = "sim-agent-plotting";

export const tests = [
  defineTest({
    id: "T2-A4-01",
    tier: 2,
    area: AREA,
    features: ["F10", "F11", "F12"],
    title: "Zero-spend unaffordable CPC edge case (budget < cpc, natural replay) renders zero-NaN SVG",
    async fn(ctx) {
      const outDir = path.join(ctx.tmpDir, "unaffordable");
      await runBinaryOk("sim-agent", [
        "--output-dir", outDir,
        "--campaign-budget-cents", "50",
        "--cpc-cents", "100",
        "--no-impression-target",
      ]);

      const ts = readJsonFile(path.join(outDir, "budget_timeseries.json"));
      const csvText = fs.readFileSync(path.join(outDir, "budget_timeseries.csv"), "utf8");
      const svgText = fs.readFileSync(path.join(outDir, "budget_spent_30d.svg"), "utf8");

      validateBudgetTimeSeries(ts, { campaign_budget_cents: 50, cpc_cents: 100, impressions: null });
      assert.strictEqual(ts.summary.total_impressions, 0);
      assert.strictEqual(ts.summary.total_clicks, 0);
      assert.strictEqual(ts.summary.total_spend_cents, 0);
      assert.strictEqual(ts.summary.remaining_budget_cents, 25000);
      assert.strictEqual(ts.summary.utilization_pct, 0);
      assert.strictEqual(ts.summary.ctr_pct, 0);
      assert.strictEqual(ts.summary.insufficient_budget_opportunities, 360000);
      assert.strictEqual(ts.summary.exhausted_campaigns, 500);
      for (const d of ts.days) {
        assert.strictEqual(d.active_campaigns_end_of_day, 0);
      }
      validateBudgetCsv(csvText, ts);
      validateBudgetSvg(svgText, ts);
    },
  }),

  defineTest({
    id: "T2-A4-02",
    tier: 2,
    area: AREA,
    features: ["F11", "F12"],
    title: "Zero total budget (--campaign-budget-cents 0) guards division-by-zero in JSON, CSV, and SVG",
    async fn(ctx) {
      const outDir = path.join(ctx.tmpDir, "zero-budget");
      await runBinaryOk("sim-agent", [
        "--output-dir", outDir,
        "--campaign-budget-cents", "0",
        "--cpc-cents", "100",
        "--no-impression-target",
      ]);

      const ts = readJsonFile(path.join(outDir, "budget_timeseries.json"));
      const csvText = fs.readFileSync(path.join(outDir, "budget_timeseries.csv"), "utf8");
      const svgText = fs.readFileSync(path.join(outDir, "budget_spent_30d.svg"), "utf8");

      validateBudgetTimeSeries(ts, { campaign_budget_cents: 0, cpc_cents: 100, impressions: null });
      assert.strictEqual(ts.summary.total_budget_cents, 0);
      assert.strictEqual(ts.summary.total_spend_cents, 0);
      assert.strictEqual(ts.summary.utilization_pct, 0);
      for (const d of ts.days) {
        assert.strictEqual(d.utilization_pct, 0);
      }
      validateBudgetCsv(csvText, ts);
      validateBudgetSvg(svgText, ts);
    },
  }),

  defineTest({
    id: "T2-A4-03",
    tier: 2,
    area: AREA,
    features: ["F9", "F13"],
    title: "Zero CPC (--cpc-cents 0) is rejected with non-zero exit and leaves no artifacts",
    async fn(ctx) {
      const outDir = path.join(ctx.tmpDir, "zero-cpc");
      const res = await runBinary("sim-agent", ["--output-dir", outDir, "--cpc-cents", "0"]);
      assert.notStrictEqual(res.code, 0, "Zero CPC must fail");
      assert.ok(res.stderr.trim().length > 0, "Expected descriptive stderr error");
      assert.strictEqual(fs.existsSync(path.join(outDir, "budget_timeseries.json")), false);
    },
  }),

  defineTest({
    id: "T2-A4-04",
    tier: 2,
    area: AREA,
    features: ["F13"],
    title: "Conflicting CLI flags (--impressions + --no-impression-target) are rejected",
    async fn(ctx) {
      const outDir = path.join(ctx.tmpDir, "conflict");
      const res = await runBinary("sim-agent", [
        "--output-dir", outDir,
        "--impressions", "100000",
        "--no-impression-target",
      ]);
      assert.notStrictEqual(res.code, 0, "Conflicting flags must fail with non-zero exit");
      assert.ok(res.stderr.trim().length > 0, "Expected stderr error explaining flag conflict");
      assert.strictEqual(fs.existsSync(path.join(outDir, "budget_timeseries.json")), false);
    },
  }),

  defineTest({
    id: "T2-A4-05",
    tier: 2,
    area: AREA,
    features: ["F9", "F13"],
    title: "Unreachable impression target fails cleanly without partial artifacts",
    async fn(ctx) {
      const outDir = path.join(ctx.tmpDir, "unreachable");
      const res = await runBinary("sim-agent", [
        "--output-dir", outDir,
        "--campaign-budget-cents", "50",
        "--cpc-cents", "100",
        "--impressions", "1000",
      ]);
      assert.notStrictEqual(res.code, 0, "Unreachable target must fail");
      assert.ok(res.stderr.trim().length > 0, "Expected descriptive stderr message");
      assert.strictEqual(fs.existsSync(path.join(outDir, "budget_timeseries.json")), false);
    },
  }),

  defineTest({
    id: "T2-A4-06",
    tier: 2,
    area: AREA,
    features: ["F9", "F13"],
    title: "Missing or malformed --marketplace file is rejected cleanly",
    async fn(ctx) {
      const outA = path.join(ctx.tmpDir, "missing-mkt");
      const resA = await runBinary("sim-agent", [
        "--marketplace", path.join(ctx.tmpDir, "does-not-exist.json"),
        "--output-dir", outA,
      ]);
      assert.notStrictEqual(resA.code, 0, "Missing marketplace file must fail");
      assert.ok(resA.stderr.trim().length > 0);

      const badFile = path.join(ctx.tmpDir, "corrupt.json");
      fs.writeFileSync(badFile, "{not valid json", "utf8");
      const outB = path.join(ctx.tmpDir, "corrupt-mkt");
      const resB = await runBinary("sim-agent", [
        "--marketplace", badFile,
        "--output-dir", outB,
      ]);
      assert.notStrictEqual(resB.code, 0, "Malformed marketplace JSON must fail");
      assert.ok(resB.stderr.trim().length > 0);
    },
  }),

  defineTest({
    id: "T2-A4-07",
    tier: 2,
    area: AREA,
    features: ["F9", "F11", "F12", "F13"],
    title: "Negative --marketplace-seed, remainder quota split (--impressions 31), and nested --output-dir",
    async fn(ctx) {
      const nestedOut = path.join(ctx.tmpDir, "deep", "nested", "agent");
      await runBinaryOk("sim-agent", [
        "--marketplace-seed", "-42",
        "--seed", "42",
        "--impressions", "31",
        "--output-dir", nestedOut,
      ]);

      const ts = readJsonFile(path.join(nestedOut, "budget_timeseries.json"));
      const csvText = fs.readFileSync(path.join(nestedOut, "budget_timeseries.csv"), "utf8");
      const svgText = fs.readFileSync(path.join(nestedOut, "budget_spent_30d.svg"), "utf8");

      validateBudgetTimeSeries(ts, { marketplace_seed: -42, sim_seed: 42, impressions: 31 });
      assert.strictEqual(ts.days[0].impressions, 2, "Day 1 must receive remainder impression (2)");
      for (let i = 1; i < 30; i++) {
        assert.strictEqual(ts.days[i].impressions, 1, `Day ${i + 1} must receive 1 impression`);
      }
      validateBudgetCsv(csvText, ts);
      validateBudgetSvg(svgText, ts);
    },
  }),
];
