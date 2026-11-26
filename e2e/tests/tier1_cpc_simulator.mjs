/**
 * Tier 1 (Happy-Path) — Area 3: 30-Day CPC Simulator CLI & Ledger Accounting
 */

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { defineTest } from "../registry.mjs";
import { readJsonFile, runBinaryOk, validateSimulationReport } from "../helpers.mjs";

const AREA = "cpc-simulator";

async function generateMarketplace(dir, seed = 42) {
  await runBinaryOk("ads-simulator", ["--seed", String(seed), "--output-dir", dir]);
  return path.join(dir, "marketplace.json");
}

export const tests = [
  defineTest({
    id: "T1-A3-01",
    tier: 1,
    area: AREA,
    features: ["F7"],
    title: "Canonical baseline (seed=42, budget=10000, cpc=100, impressions=1000000) reproduces contract totals",
    async fn(ctx) {
      const mpPath = await generateMarketplace(path.join(ctx.tmpDir, "mkt"), 42);
      const outPath = path.join(ctx.tmpDir, "baseline.json");
      await runBinaryOk("ads-marketplace-simulation", [
        "--marketplace", mpPath,
        "--output", outPath,
        "--seed", "42",
        "--campaign-budget-cents", "10000",
        "--cpc-cents", "100",
        "--impressions", "1000000",
      ]);

      const report = readJsonFile(outPath);
      validateSimulationReport(report, {
        seed: 42,
        campaign_budget_cents: 10000,
        cpc_cents: 100,
        impressions: 1000000,
      });
      assert.strictEqual(report.summary.budget_cents, 5000000);
      assert.strictEqual(report.summary.remaining_budget_cents, 4801600);
      assert.strictEqual(report.summary.opportunities, 1000000);
      assert.strictEqual(report.summary.no_matching_category, 0);
      assert.strictEqual(report.summary.insufficient_budget, 0);
      assert.strictEqual(report.summary.impressions, 1000000);
      assert.strictEqual(report.summary.clicks, 1984);
      assert.strictEqual(report.summary.spend_cents, 198400);
      assert.ok(report.summary.ad_requests >= 50000, "R1 1M impressions requires >= 50k ad-server requests");
    },
  }),

  defineTest({
    id: "T1-A3-02",
    tier: 1,
    area: AREA,
    features: ["F7"],
    title: "Full 30-day & 500-campaign ledger accounting and daily quota split invariants",
    async fn(ctx) {
      const mpPath = await generateMarketplace(path.join(ctx.tmpDir, "mkt"), 42);
      const outPath = path.join(ctx.tmpDir, "ledger.json");
      await runBinaryOk("ads-marketplace-simulation", [
        "--marketplace", mpPath,
        "--output", outPath,
        "--seed", "42",
        "--campaign-budget-cents", "10000",
        "--cpc-cents", "100",
        "--impressions", "1000000",
      ]);
      const report = readJsonFile(outPath);
      validateSimulationReport(report, {
        seed: 42,
        campaign_budget_cents: 10000,
        cpc_cents: 100,
        impressions: 1000000,
      });
      for (let i = 0; i < 10; i++) {
        assert.strictEqual(report.days[i].impressions, 33334, `Day ${i + 1} must deliver 33334 impressions`);
      }
      for (let i = 10; i < 30; i++) {
        assert.strictEqual(report.days[i].impressions, 33333, `Day ${i + 1} must deliver 33333 impressions`);
      }
      assert.strictEqual(report.days[0].clicks, 60);
      assert.strictEqual(report.days[0].spend_cents, 6000);
    },
  }),

  defineTest({
    id: "T1-A3-03",
    tier: 1,
    area: AREA,
    features: ["F7"],
    title: "Natural user-independent request mode (omitting --impressions) delivers 360k impressions across 18k R1 requests",
    async fn(ctx) {
      const mpPath = await generateMarketplace(path.join(ctx.tmpDir, "mkt"), 42);
      const outPath = path.join(ctx.tmpDir, "natural.json");
      await runBinaryOk("ads-marketplace-simulation", [
        "--marketplace", mpPath,
        "--output", outPath,
        "--seed", "42",
        "--campaign-budget-cents", "10000",
        "--cpc-cents", "100",
      ]);
      const report = readJsonFile(outPath);
      validateSimulationReport(report, {
        seed: 42,
        campaign_budget_cents: 10000,
        cpc_cents: 100,
        impressions: null,
      });
      assert.strictEqual(report.summary.budget_cents, 5000000);
      assert.strictEqual(report.summary.remaining_budget_cents, 4929900);
      assert.strictEqual(report.summary.ad_requests, 18000);
      assert.strictEqual(report.summary.opportunities, 360000);
      assert.strictEqual(report.summary.no_matching_category, 0);
      assert.strictEqual(report.summary.insufficient_budget, 0);
      assert.strictEqual(report.summary.impressions, 360000);
      assert.strictEqual(report.summary.clicks, 701);
      assert.strictEqual(report.summary.spend_cents, 70100);
      for (const d of report.days) {
        assert.strictEqual(d.ad_requests, 600);
        assert.strictEqual(d.opportunities, 12000);
        assert.strictEqual(d.impressions, 12000);
      }
    },
  }),

  defineTest({
    id: "T1-A3-04",
    tier: 1,
    area: AREA,
    features: ["F7"],
    title: "Deterministic reproducibility for same seed and divergence for distinct seeds",
    async fn(ctx) {
      const mpPath = await generateMarketplace(path.join(ctx.tmpDir, "mkt"), 42);
      const outA = path.join(ctx.tmpDir, "run-a.json");
      const outB = path.join(ctx.tmpDir, "run-b.json");
      const outC = path.join(ctx.tmpDir, "run-c.json");

      await runBinaryOk("ads-marketplace-simulation", [
        "--marketplace", mpPath, "--output", outA, "--seed", "42", "--impressions", "30000",
      ]);
      await runBinaryOk("ads-marketplace-simulation", [
        "--marketplace", mpPath, "--output", outB, "--seed", "42", "--impressions", "30000",
      ]);
      await runBinaryOk("ads-marketplace-simulation", [
        "--marketplace", mpPath, "--output", outC, "--seed", "99", "--impressions", "30000",
      ]);

      assert.strictEqual(
        Buffer.compare(fs.readFileSync(outA), fs.readFileSync(outB)),
        0,
        "Identical simulation runs must be byte-for-byte deterministic",
      );
      const repA = readJsonFile(outA);
      const repC = readJsonFile(outC);
      validateSimulationReport(repC, { seed: 99, impressions: 30000 });
      assert.notDeepStrictEqual(repA.days, repC.days, "Different seeds must yield distinct daily click outcomes");
    },
  }),

  defineTest({
    id: "T1-A3-05",
    tier: 1,
    area: AREA,
    features: ["F8"],
    title: "Automatic creation of missing parent directories for --output without temp residue",
    async fn(ctx) {
      const mpPath = await generateMarketplace(path.join(ctx.tmpDir, "mkt"), 42);
      const nestedDir = path.join(ctx.tmpDir, "nested", "a", "b");
      const outPath = path.join(nestedDir, "report.json");
      await runBinaryOk("ads-marketplace-simulation", [
        "--marketplace", mpPath,
        "--output", outPath,
        "--seed", "42",
        "--impressions", "3000",
      ]);
      assert.ok(fs.existsSync(outPath), "Nested output report.json must be created");
      assert.deepStrictEqual(fs.readdirSync(nestedDir), ["report.json"], "No leftover temporary files allowed");
      validateSimulationReport(readJsonFile(outPath), { seed: 42, impressions: 3000 });
    },
  }),

  defineTest({
    id: "T1-A3-06",
    tier: 1,
    area: AREA,
    features: ["F7"],
    title: "CLI --help documents all contract flags and exits 0",
    async fn(ctx) {
      const res = await runBinaryOk("ads-marketplace-simulation", ["--help"]);
      for (const flag of [
        "--marketplace",
        "--output",
        "--seed",
        "--campaign-budget-cents",
        "--cpc-cents",
        "--impressions",
        "--ranker",
        "--slate-size",
      ]) {
        assert.ok(res.stdout.includes(flag), `--help must document ${flag}`);
      }
    },
  }),

  defineTest({
    id: "T1-A3-07",
    tier: 1,
    area: AREA,
    features: ["F7", "F8"],
    title: "Standalone visit-simulator CLI operates ad-server Strategy R1 returning 20 distinct funded campaigns per request",
    async fn(ctx) {
      const outA = path.join(ctx.tmpDir, "slates-a.json");
      const outB = path.join(ctx.tmpDir, "slates-b.json");
      await runBinaryOk("visit-simulator", [
        "--seed", "42",
        "--requests", "15",
        "--ranker", "R1",
        "--slate-size", "20",
        "--output", outA,
      ]);
      await runBinaryOk("visit-simulator", [
        "--seed", "42",
        "--requests", "15",
        "--ranker", "R1",
        "--slate-size", "20",
        "--output", outB,
      ]);

      assert.strictEqual(
        Buffer.compare(fs.readFileSync(outA), fs.readFileSync(outB)),
        0,
        "visit-simulator CLI must be byte-for-byte deterministic for identical seeds",
      );

      const data = readJsonFile(outA);
      assert.strictEqual(data.ranker, "R1");
      assert.strictEqual(data.slate_size, 20);
      assert.strictEqual(data.total_requests, 15);
      assert.strictEqual(data.total_impressions, 300);
      assert.strictEqual(data.slates.length, 15);

      for (const rec of data.slates) {
        assert.ok(/^user-\d{4}$/.test(rec.user_id), `Expected valid user_id, got ${rec.user_id}`);
        assert.strictEqual(rec.strategy, "R1");
        assert.strictEqual(rec.ads.length, 20);
        const ids = new Set(rec.ads.map((a) => a.campaign_id));
        assert.strictEqual(ids.size, 20, "All 20 served ads in an R1 slate must come from distinct funded campaigns");
      }
    },
  }),
];
