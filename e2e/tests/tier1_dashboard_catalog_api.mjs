/**
 * Tier 1 (Happy-Path) — Area 5: Dashboard Feature Catalog API & Live Introspection
 */

import assert from "node:assert/strict";
import { defineTest } from "../registry.mjs";
import {
  httpGetJson,
  httpGetText,
  startDashboardServer,
  validateBudgetSvg,
  validateBudgetTimeSeries,
  validateMarketplaceApiResponse,
  validateSimulatorCatalog,
} from "../helpers.mjs";

const AREA = "dashboard-catalog-api";

export const tests = [
  defineTest({
    id: "T1-A5-01",
    tier: 1,
    area: AREA,
    features: ["F15"],
    title: "GET /health returns 200 {status:'ok'} and GET / serves HTML dashboard shell",
    async fn() {
      const server = await startDashboardServer();
      try {
        const health = await httpGetJson(`${server.url}/health`);
        assert.strictEqual(health.status, 200);
        assert.ok(health.contentType.includes("application/json"), `Expected JSON content-type, got ${health.contentType}`);
        assert.deepStrictEqual(health.data, { status: "ok" });

        const root = await httpGetText(`${server.url}/`);
        assert.strictEqual(root.status, 200);
        assert.ok(root.contentType.includes("text/html"), `Expected HTML content-type, got ${root.contentType}`);
        assert.ok(root.text.length > 500, "Dashboard HTML response must be substantial (> 500 bytes)");
        assert.ok(/<html\b/i.test(root.text) && /<\/html>/i.test(root.text), "Root response must contain <html>...</html>");
      } finally {
        await server.stop();
      }
    },
  }),

  defineTest({
    id: "T1-A5-02",
    tier: 1,
    area: AREA,
    features: ["F14", "F15"],
    title: "GET /api/catalog returns valid 9-piece SimulatorCatalog with verified Rust source paths",
    async fn(ctx) {
      const server = await startDashboardServer();
      try {
        const res = await httpGetJson(`${server.url}/api/catalog`);
        assert.strictEqual(res.status, 200);
        assert.ok(res.contentType.includes("application/json"));
        validateSimulatorCatalog(res.data, { repoRoot: ctx.repoRoot });
      } finally {
        await server.stop();
      }
    },
  }),

  defineTest({
    id: "T1-A5-03",
    tier: 1,
    area: AREA,
    features: ["F14", "F15"],
    title: "SimulatorCatalog surfaces live compiled simulator constants accurately across pieces",
    async fn() {
      const server = await startDashboardServer();
      try {
        const res = await httpGetJson(`${server.url}/api/catalog`);
        assert.strictEqual(res.status, 200);
        validateSimulatorCatalog(res.data);

        // Flatten all parameter representations across the 9 pieces (normalizing numeric commas)
        const paramBlob = JSON.stringify(res.data.pieces.map((p) => p.parameters)).replace(/(\d),(\d)/g, "$1$2");

        const requiredPatterns = [
          { label: "100 categories / CPC", re: /\b100\b/ },
          { label: "50 advertisers / organic per category", re: /\b50\b/ },
          { label: "20 catalog items", re: /\b20\b/ },
          { label: "1000 users", re: /\b1000\b/ },
          { label: "5 preferred interests", re: /\b5\b/ },
          { label: "9 affinity views", re: /\b9\b/ },
          { label: "3 exploration views", re: /\b3\b/ },
          { label: "30 simulation days", re: /\b30\b/ },
          { label: "10 taxonomy groups / campaigns per advertiser", re: /\b10\b/ },
          { label: "4% / 0.04 max click probability", re: /(?:0\.04|\b4(?:\.0+)?%)/ },
          { label: "100000-1000000 cents / $1000-$10000 default budget range", re: /(?:\b100000\b|\$1000\b)/ },
        ];
        for (const req of requiredPatterns) {
          assert.ok(
            req.re.test(paramBlob),
            `Catalog parameters must surface live constant for ${req.label}; observed params: ${paramBlob.slice(0, 600)}`,
          );
        }
      } finally {
        await server.stop();
      }
    },
  }),

  defineTest({
    id: "T1-A5-04",
    tier: 1,
    area: AREA,
    features: ["F15"],
    title: "GET /api/marketplace (default seed=42) returns complete 10x10 taxonomy and entity stats",
    async fn() {
      const server = await startDashboardServer();
      try {
        const res = await httpGetJson(`${server.url}/api/marketplace`);
        assert.strictEqual(res.status, 200);
        assert.ok(res.contentType.includes("application/json"));
        validateMarketplaceApiResponse(res.data, { expectedSeed: 42 });
      } finally {
        await server.stop();
      }
    },
  }),

  defineTest({
    id: "T1-A5-05",
    tier: 1,
    area: AREA,
    features: ["F15"],
    title: "GET /api/marketplace?seed=12345 is deterministic and differs in seeded metrics from seed=42",
    async fn() {
      const server = await startDashboardServer();
      try {
        const resA = await httpGetJson(`${server.url}/api/marketplace?seed=12345`);
        const resB = await httpGetJson(`${server.url}/api/marketplace?seed=12345`);
        const res42 = await httpGetJson(`${server.url}/api/marketplace?seed=42`);

        assert.strictEqual(resA.status, 200);
        assert.strictEqual(resB.status, 200);
        assert.strictEqual(res42.status, 200);

        validateMarketplaceApiResponse(resA.data, { expectedSeed: 12345 });
        assert.deepStrictEqual(resA.data, resB.data, "Same seed must produce identical /api/marketplace response");
        assert.deepStrictEqual(resA.data.taxonomy, res42.data.taxonomy, "Taxonomy must be invariant across seeds");
        assert.deepStrictEqual(resA.data.counts, res42.data.counts, "Entity counts must be invariant across seeds");
        assert.deepStrictEqual(resA.data.browsing_split, res42.data.browsing_split, "Browsing split must be invariant");
        assert.ok(
          JSON.stringify(resA.data.price_stats) !== JSON.stringify(res42.data.price_stats),
          "Distinct seeds should differ in seeded marketplace metrics",
        );
      } finally {
        await server.stop();
      }
    },
  }),

  defineTest({
    id: "T1-A5-06",
    tier: 1,
    area: AREA,
    features: ["F15"],
    title: "Baseline GET /api/sim-agent/latest and GET /api/artifacts/budget-plot return valid data & SVG",
    async fn() {
      const server = await startDashboardServer();
      try {
        const latest = await httpGetJson(`${server.url}/api/sim-agent/latest`);
        assert.strictEqual(latest.status, 200);
        assert.ok(latest.contentType.includes("application/json"));
        validateBudgetTimeSeries(latest.data);

        const plot = await httpGetText(`${server.url}/api/artifacts/budget-plot`);
        assert.strictEqual(plot.status, 200);
        assert.ok(plot.contentType.includes("image/svg+xml"), `Expected image/svg+xml, got ${plot.contentType}`);
        validateBudgetSvg(plot.text, latest.data);
      } finally {
        await server.stop();
      }
    },
  }),

  defineTest({
    id: "T1-A5-07",
    tier: 1,
    area: AREA,
    features: ["F14", "F15"],
    title: "GET /api/ad-server/sample returns 20 distinct funded R1 ads and GET /api/sim-agent/analysis returns auto-analysis",
    async fn() {
      const server = await startDashboardServer();
      try {
        const sample1 = await httpGetJson(`${server.url}/api/ad-server/sample?user_id=user-0005&seed=42`);
        const sample2 = await httpGetJson(`${server.url}/api/ad-server/sample?user_id=user-0005&seed=42`);
        assert.strictEqual(sample1.status, 200);
        assert.deepStrictEqual(sample1.data, sample2.data, "GET /api/ad-server/sample must be deterministic");
        assert.strictEqual(sample1.data.user_id, "user-0005");
        assert.strictEqual(sample1.data.strategy, "R1");
        assert.strictEqual(sample1.data.ads.length, 20);
        assert.strictEqual(
          new Set(sample1.data.ads.map((a) => a.campaign_id)).size,
          20,
          "All 20 ads returned by /api/ad-server/sample must be from distinct funded campaigns",
        );

        const diffSeed = await httpGetJson(`${server.url}/api/ad-server/sample?user_id=user-0005&seed=99`);
        assert.strictEqual(diffSeed.status, 200);
        assert.notDeepStrictEqual(
          sample1.data.ads.map((a) => a.campaign_id),
          diffSeed.data.ads.map((a) => a.campaign_id),
          "Different ad-server seeds should sample distinct R1 campaign slates",
        );

        const analysis = await httpGetJson(`${server.url}/api/sim-agent/analysis`);
        assert.strictEqual(analysis.status, 200);
        assert.strictEqual(analysis.data.ranker, "R1");
        assert.strictEqual(analysis.data.slate_size, 20);
        assert.strictEqual(analysis.data.budget_depletion.daily_depletion.length, 30);
        assert.ok(analysis.data.ctr_performance.overall_ctr_pct > 0);
        assert.ok(Array.isArray(analysis.data.findings) && analysis.data.findings.length >= 1);
      } finally {
        await server.stop();
      }
    },
  }),
];
