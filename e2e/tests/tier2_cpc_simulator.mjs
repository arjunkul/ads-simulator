/**
 * Tier 2 (Boundary / Edge / Negative) — Area 3: 30-Day CPC Simulator CLI & Ledger Accounting
 */

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { defineTest } from "../registry.mjs";
import { readJsonFile, runBinary, runBinaryOk, validateSimulationReport } from "../helpers.mjs";

const AREA = "cpc-simulator";

async function generateMarketplace(dir, seed = 42) {
  await runBinaryOk("world-generator", ["--seed", String(seed), "--output-dir", dir]);
  return path.join(dir, "marketplace.json");
}

export const tests = [
  defineTest({
    id: "T2-A3-01",
    tier: 2,
    area: AREA,
    features: ["F7"],
    title: "Zero CPC (--cpc-cents 0) is rejected cleanly without writing output",
    async fn(ctx) {
      const mpPath = await generateMarketplace(path.join(ctx.tmpDir, "mkt"), 42);
      const outPath = path.join(ctx.tmpDir, "out.json");
      const res = await runBinary("ads-marketplace-simulation", [
        "--marketplace", mpPath,
        "--output", outPath,
        "--cpc-cents", "0",
      ]);
      assert.notStrictEqual(res.code, 0, "Zero CPC must fail with non-zero exit");
      assert.ok(/cpc.*positive/i.test(res.stderr), `stderr must explain CPC must be positive, got: ${res.stderr}`);
      assert.strictEqual(fs.existsSync(outPath), false, "Output file must not be written on validation failure");
    },
  }),

  defineTest({
    id: "T2-A3-02",
    tier: 2,
    area: AREA,
    features: ["F7"],
    title: "Full budget exhaustion in natural replay mode (budget=100, cpc=100)",
    async fn(ctx) {
      const mpPath = await generateMarketplace(path.join(ctx.tmpDir, "mkt"), 42);
      const outPath = path.join(ctx.tmpDir, "exhaust.json");
      await runBinaryOk("ads-marketplace-simulation", [
        "--marketplace", mpPath,
        "--output", outPath,
        "--seed", "42",
        "--campaign-budget-cents", "100",
        "--cpc-cents", "100",
      ]);
      const report = readJsonFile(outPath);
      validateSimulationReport(report, {
        seed: 42,
        campaign_budget_cents: 100,
        cpc_cents: 100,
        impressions: null,
      });
      assert.strictEqual(report.summary.budget_cents, 50000);
      assert.strictEqual(report.summary.remaining_budget_cents, 0);
      assert.strictEqual(report.summary.clicks, 500);
      assert.strictEqual(report.summary.spend_cents, 50000);
      assert.strictEqual(report.summary.ad_requests, 360000);
      assert.ok(report.summary.insufficient_budget > 0);
      assert.ok(report.summary.impressions > 0);
      for (const c of report.campaigns) {
        assert.strictEqual(c.remaining_budget_cents, 0);
        assert.strictEqual(c.clicks, 1);
      }
    },
  }),

  defineTest({
    id: "T2-A3-03",
    tier: 2,
    area: AREA,
    features: ["F7"],
    title: "Unaffordable budget (budget < cpc) and zero budget (budget = 0) in natural mode",
    async fn(ctx) {
      const mpPath = await generateMarketplace(path.join(ctx.tmpDir, "mkt"), 42);
      const cases = [
        { name: "unaffordable", budget: 72, cpc: 73 },
        { name: "zero", budget: 0, cpc: 100 },
      ];
      for (const c of cases) {
        const outPath = path.join(ctx.tmpDir, `${c.name}.json`);
        await runBinaryOk("ads-marketplace-simulation", [
          "--marketplace", mpPath,
          "--output", outPath,
          "--seed", "42",
          "--campaign-budget-cents", String(c.budget),
          "--cpc-cents", String(c.cpc),
        ]);
        const report = readJsonFile(outPath);
        validateSimulationReport(report, {
          seed: 42,
          campaign_budget_cents: c.budget,
          cpc_cents: c.cpc,
          impressions: null,
        });
        assert.strictEqual(report.summary.impressions, 0);
        assert.strictEqual(report.summary.clicks, 0);
        assert.strictEqual(report.summary.spend_cents, 0);
        assert.strictEqual(report.summary.remaining_budget_cents, report.summary.budget_cents);
        assert.strictEqual(report.summary.ad_requests, 360000);
        assert.strictEqual(report.summary.opportunities, 360000);
        assert.strictEqual(report.summary.insufficient_budget, 360000);
        for (const d of report.days) {
          assert.strictEqual(d.impressions, 0);
          assert.strictEqual(d.insufficient_budget, 12000);
        }
      }
    },
  }),

  defineTest({
    id: "T2-A3-04",
    tier: 2,
    area: AREA,
    features: ["F7"],
    title: "Unreachable --impressions target rejected immediately on day 1 and mid-run after exhaustion",
    async fn(ctx) {
      const mpPath = await generateMarketplace(path.join(ctx.tmpDir, "mkt"), 42);
      const outA = path.join(ctx.tmpDir, "unreach-a.json");
      const resA = await runBinary("ads-marketplace-simulation", [
        "--marketplace", mpPath, "--output", outA,
        "--campaign-budget-cents", "72", "--cpc-cents", "73", "--impressions", "1",
      ]);
      assert.notStrictEqual(resA.code, 0);
      assert.ok(/impression target cannot be reached on day 1/i.test(resA.stderr), `stderr: ${resA.stderr}`);
      assert.strictEqual(fs.existsSync(outA), false);

      const outB = path.join(ctx.tmpDir, "unreach-b.json");
      const resB = await runBinary("ads-marketplace-simulation", [
        "--marketplace", mpPath, "--output", outB,
        "--campaign-budget-cents", "100", "--cpc-cents", "100", "--impressions", "1000000",
      ]);
      assert.notStrictEqual(resB.code, 0);
      assert.ok(/impression target cannot be reached on day \d+/i.test(resB.stderr), `stderr: ${resB.stderr}`);
      assert.strictEqual(fs.existsSync(outB), false);
    },
  }),

  defineTest({
    id: "T2-A3-05",
    tier: 2,
    area: AREA,
    features: ["F7"],
    title: "Zero impression target (--impressions 0) completes 30 zero-activity days cleanly",
    async fn(ctx) {
      const mpPath = await generateMarketplace(path.join(ctx.tmpDir, "mkt"), 42);
      const outPath = path.join(ctx.tmpDir, "zero-imp.json");
      await runBinaryOk("ads-marketplace-simulation", [
        "--marketplace", mpPath, "--output", outPath,
        "--campaign-budget-cents", "10000", "--cpc-cents", "100", "--impressions", "0",
      ]);
      const report = readJsonFile(outPath);
      validateSimulationReport(report, { campaign_budget_cents: 10000, cpc_cents: 100, impressions: 0 });
      assert.strictEqual(report.summary.opportunities, 0);
      assert.strictEqual(report.summary.impressions, 0);
      assert.strictEqual(report.summary.clicks, 0);
      assert.strictEqual(report.summary.spend_cents, 0);
      assert.strictEqual(report.summary.remaining_budget_cents, 5000000);
    },
  }),

  defineTest({
    id: "T2-A3-06",
    tier: 2,
    area: AREA,
    features: ["F7"],
    title: "Non-divisible budget remainder cutoff (budget=250, cpc=100) never underflows",
    async fn(ctx) {
      const mpPath = await generateMarketplace(path.join(ctx.tmpDir, "mkt"), 42);
      const outPath = path.join(ctx.tmpDir, "rem.json");
      await runBinaryOk("ads-marketplace-simulation", [
        "--marketplace", mpPath, "--output", outPath,
        "--seed", "42", "--campaign-budget-cents", "250", "--cpc-cents", "100",
      ]);
      const report = readJsonFile(outPath);
      validateSimulationReport(report, { seed: 42, campaign_budget_cents: 250, cpc_cents: 100, impressions: null });
      for (const c of report.campaigns) {
        assert.ok(c.clicks <= 2, `campaign ${c.id} exceeded max 2 clicks`);
        assert.ok(c.spend_cents <= 200, `campaign ${c.id} spent more than 200 cents`);
        assert.ok(c.remaining_budget_cents >= 50, `campaign ${c.id} underflowed remainder (< 50 cents)`);
      }
      assert.strictEqual(report.summary.spend_cents % 100, 0);
    },
  }),

  defineTest({
    id: "T2-A3-07",
    tier: 2,
    area: AREA,
    features: ["F7"],
    title: "Missing marketplace file, malformed JSON, wrong schema_version, and non-USD currency rejected",
    async fn(ctx) {
      const mpPath = await generateMarketplace(path.join(ctx.tmpDir, "mkt"), 42);
      const validMp = readJsonFile(mpPath);

      const badJsonPath = path.join(ctx.tmpDir, "bad.json");
      fs.writeFileSync(badJsonPath, "{not valid json", "utf8");

      const wrongVerPath = path.join(ctx.tmpDir, "v2.json");
      fs.writeFileSync(wrongVerPath, JSON.stringify({ ...validMp, schema_version: 2 }), "utf8");

      const wrongCurrPath = path.join(ctx.tmpDir, "eur.json");
      fs.writeFileSync(wrongCurrPath, JSON.stringify({ ...validMp, currency: "EUR" }), "utf8");

      const cases = [
        { name: "missing", path: path.join(ctx.tmpDir, "does-not-exist.json"), regex: null },
        { name: "malformed", path: badJsonPath, regex: null },
        { name: "wrong-ver", path: wrongVerPath, regex: /schema version 1.*USD/i },
        { name: "wrong-curr", path: wrongCurrPath, regex: /schema version 1.*USD/i },
      ];

      for (const c of cases) {
        const outPath = path.join(ctx.tmpDir, `out-${c.name}.json`);
        const res = await runBinary("ads-marketplace-simulation", [
          "--marketplace", c.path, "--output", outPath,
        ]);
        assert.notStrictEqual(res.code, 0, `Case ${c.name} must exit non-zero`);
        assert.ok(res.stderr.trim().length > 0, `Case ${c.name} must emit stderr message`);
        if (c.regex) {
          assert.ok(c.regex.test(res.stderr), `Case ${c.name} stderr mismatch: ${res.stderr}`);
        }
        assert.strictEqual(fs.existsSync(outPath), false, `Case ${c.name} must not create output file`);
      }
    },
  }),

  defineTest({
    id: "T2-A3-08",
    tier: 2,
    area: AREA,
    features: ["F7"],
    title: "Invalid CLI arguments (--seed -1, missing --output, missing --marketplace, unknown flag) rejected",
    async fn(ctx) {
      const mpPath = await generateMarketplace(path.join(ctx.tmpDir, "mkt"), 42);
      const outPath = path.join(ctx.tmpDir, "out.json");
      const badCalls = [
        ["--marketplace", mpPath, "--output", outPath, "--seed", "-1"],
        ["--marketplace", mpPath],
        ["--output", outPath],
        ["--marketplace", mpPath, "--output", outPath, "--bogus-flag"],
      ];
      for (const args of badCalls) {
        const res = await runBinary("ads-marketplace-simulation", args);
        assert.notStrictEqual(res.code, 0, `Expected non-zero exit for ${JSON.stringify(args)}`);
        assert.ok(res.stderr.trim().length > 0, `Expected stderr error for ${JSON.stringify(args)}`);
      }
    },
  }),
];
