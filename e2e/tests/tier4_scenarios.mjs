/**
 * Tier 4 — Real-World End-to-End Application Scenarios (7 Multi-Step User Workflows)
 */

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { defineTest } from "../registry.mjs";
import {
  assertZeroPythonFiles,
  BROWSER_UI_HELPERS_SRC,
  formatUsdCandidates,
  httpGetJson,
  httpGetText,
  httpPostJson,
  openBrowserSession,
  readJsonFile,
  resolveBinary,
  runBinaryOk,
  startDashboardServer,
  startStaticServer,
  validateBudgetCsv,
  validateBudgetSvg,
  validateBudgetTimeSeries,
  validateMarketplaceApiResponse,
  validateMarketplaceJson,
  validateSimulationAndTimeSeriesParity,
  validateSimulationReport,
  validateSimulatorCatalog,
  validateStaticExportBundle,
} from "../helpers.mjs";

export const tests = [
  defineTest({
    id: "T4-01",
    tier: 4,
    area: "workflow",
    areas: [
      "sim-agent-plotting",
      "dashboard-catalog-api",
      "dashboard-runner-export",
      "dashboard-ui-interactivity",
    ],
    features: ["F9", "F10", "F11", "F12", "F13", "F15", "F16"],
    title: "Ad-Ops Baseline 30-Day 1M-Impression Budget Audit & Visual Dashboard Review",
    async fn(ctx) {
      const baseDir = path.join(ctx.tmpDir, "baseline");
      const res = await runBinaryOk("sim-agent", ["--output-dir", baseDir], { timeoutMs: 45_000 });
      assert.ok(res.stdout.trim().length > 0, "sim-agent must output human-readable run summary");

      const simReport = readJsonFile(path.join(baseDir, "simulation.json"));
      const ts = readJsonFile(path.join(baseDir, "budget_timeseries.json"));
      const csvText = fs.readFileSync(path.join(baseDir, "budget_timeseries.csv"), "utf8");
      const svgText = fs.readFileSync(path.join(baseDir, "budget_spent_30d.svg"), "utf8");

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
      validateSimulationAndTimeSeriesParity(simReport, ts);
      validateBudgetCsv(csvText, ts);
      validateBudgetSvg(svgText, ts);

      assert.strictEqual(ts.summary.total_budget_cents, 279898700);
      assert.strictEqual(ts.summary.total_clicks, 1976);
      assert.strictEqual(ts.summary.total_spend_cents, 197600);
      assert.strictEqual(ts.summary.remaining_budget_cents, 279701100);

      const server = await startDashboardServer([
        "--sim-report", path.join(baseDir, "budget_timeseries.json"),
        "--plot-file", path.join(baseDir, "budget_spent_30d.svg"),
      ]);
      try {
        const { data: catalog } = await httpGetJson(`${server.url}/api/catalog`);
        const session = await openBrowserSession(`${server.url}/`);
        try {
          await session.waitFor(
            `document.body.innerText.includes(${JSON.stringify(catalog.pieces[0].name)})`,
            12_000,
          );
          const bodyText = await session.eval("document.body.innerText");
          const spendCandidates = formatUsdCandidates(197600);
          assert.ok(
            spendCandidates.some((c) => bodyText.includes(c)),
            `Rendered dashboard must display baseline spend (${spendCandidates.join(" | ")})`,
          );
          const norm = bodyText.replace(/(\d),(\d)/g, "$1$2");
          assert.ok(norm.includes("1976"), "Rendered dashboard must display 1,976 clicks");
          assert.ok(norm.includes("1000000"), "Rendered dashboard must display 1,000,000 impressions");
          assert.deepStrictEqual(session.consoleErrors, []);
          assert.deepStrictEqual(session.pageErrors, []);
          assert.deepStrictEqual(session.failedRequests, []);
        } finally {
          await session.close();
        }
      } finally {
        await server.stop();
      }
    },
  }),

  defineTest({
    id: "T4-02",
    tier: 4,
    area: "workflow",
    areas: ["cpc-simulator", "sim-agent-plotting", "dashboard-runner-export"],
    features: ["F7", "F9", "F10", "F11", "F12", "F13"],
    title: "Marketplace Economist Budget-Constrained Campaign Depletion Study (Ample vs Tight vs Crunch)",
    async fn(ctx) {
      const regimes = [
        { name: "ample", budget: 10000 },
        { name: "tight", budget: 200 },
        { name: "crunch", budget: 100 },
      ];
      const results = {};
      for (const r of regimes) {
        const dir = path.join(ctx.tmpDir, r.name);
        await runBinaryOk("sim-agent", [
          "--no-impression-target",
          "--seed", "42",
          "--cpc-cents", "100",
          "--campaign-budget-cents", String(r.budget),
          "--output-dir", dir,
        ]);
        const sim = readJsonFile(path.join(dir, "simulation.json"));
        const ts = readJsonFile(path.join(dir, "budget_timeseries.json"));
        const csv = fs.readFileSync(path.join(dir, "budget_timeseries.csv"), "utf8");
        const svg = fs.readFileSync(path.join(dir, "budget_spent_30d.svg"), "utf8");
        validateSimulationReport(sim, { seed: 42, campaign_budget_cents: r.budget, cpc_cents: 100, impressions: null });
        validateBudgetTimeSeries(ts, { sim_seed: 42, campaign_budget_cents: r.budget, cpc_cents: 100, impressions: null });
        validateSimulationAndTimeSeriesParity(sim, ts);
        validateBudgetCsv(csv, ts);
        validateBudgetSvg(svg, ts);
        results[r.name] = { ts, svg };
      }

      assert.ok(
        results.ample.ts.summary.utilization_pct < results.tight.ts.summary.utilization_pct &&
          results.tight.ts.summary.utilization_pct < results.crunch.ts.summary.utilization_pct,
        "Utilization must strictly increase from Ample -> Tight -> Crunch",
      );
      assert.ok(Math.abs(results.crunch.ts.summary.utilization_pct - 100) <= 0.05);
      assert.strictEqual(results.ample.ts.summary.exhausted_campaigns, 0);
      assert.strictEqual(results.crunch.ts.summary.exhausted_campaigns, 500);
      assert.strictEqual(results.ample.ts.summary.insufficient_budget_opportunities, 0);
      assert.ok(results.crunch.ts.summary.insufficient_budget_opportunities > 0);
      assert.strictEqual(results.crunch.ts.days[29].active_campaigns_end_of_day, 0);

      // Pairwise distinct SVGs across regimes
      assert.notStrictEqual(results.ample.svg, results.tight.svg);
      assert.notStrictEqual(results.tight.svg, results.crunch.svg);
      assert.notStrictEqual(results.ample.svg, results.crunch.svg);
    },
  }),

  defineTest({
    id: "T4-03",
    tier: 4,
    area: "workflow",
    areas: [
      "dashboard-catalog-api",
      "dashboard-runner-export",
      "dashboard-ui-interactivity",
      "sim-agent-plotting",
    ],
    features: ["F14", "F15", "F16"],
    title: "Product Analyst Interactive Dashboard Exploration & Live What-If Parameter Sweep",
    async fn() {
      const server = await startDashboardServer();
      try {
        const { data: catalog } = await httpGetJson(`${server.url}/api/catalog`);
        validateSimulatorCatalog(catalog);

        const session = await openBrowserSession(`${server.url}/`);
        try {
          await session.waitFor(
            `document.body.innerText.includes(${JSON.stringify(catalog.pieces[0].name)})`,
            12_000,
          );

          // Step 1: Interactive search/filter
          const piecesJson = JSON.stringify(catalog.pieces);
          const searchCheck = await session.eval(`(async () => {
            ${BROWSER_UI_HELPERS_SRC}
            const pieces = ${piecesJson};
            const input = findSearchInput();
            if (!input) return { ok: false };
            setInputValue(input, ${JSON.stringify(catalog.pieces[8].name)});
            await new Promise((r) => setTimeout(r, 150));
            const narrowed = countVisibleCatalogPieces(pieces).count;
            setInputValue(input, '');
            await new Promise((r) => setTimeout(r, 150));
            const restored = countVisibleCatalogPieces(pieces).count;
            return { ok: true, narrowed, restored };
          })()`);
          assert.strictEqual(searchCheck.ok, true);
          assert.ok(searchCheck.narrowed >= 1 && searchCheck.narrowed < 11);
          assert.strictEqual(searchCheck.restored, 11);

          // Step 2: Live marketplace seed inspection (seed 101)
          const seed101 = await httpGetJson(`${server.url}/api/marketplace?seed=101`);
          assert.strictEqual(seed101.status, 200);
          validateMarketplaceApiResponse(seed101.data, { expectedSeed: 101 });

          // Step 3: Live What-If #1 via UI (target quota 300k)
          await session.eval(`(() => {
            ${BROWSER_UI_HELPERS_SRC}
            const c = findRunnerControls();
            if (c.naturalToggle) setCheckbox(c.naturalToggle, false);
            if (c.seed) setInputValue(c.seed, 7);
            if (c.budget) setInputValue(c.budget, 5000);
            if (c.cpc) setInputValue(c.cpc, 250);
            if (c.impressions) setInputValue(c.impressions, 300000);
            c.submitButton.click();
          })()`);
          let run1 = null;
          let deadline = Date.now() + 20_000;
          while (Date.now() < deadline) {
            const { data } = await httpGetJson(`${server.url}/api/sim-agent/latest`);
            if (data?.config?.sim_seed === 7 && data?.summary?.total_impressions === 300000) {
              run1 = data;
              break;
            }
            await new Promise((r) => setTimeout(r, 150));
          }
          assert.ok(run1, "What-If #1 must complete and update server latest");

          // Step 4: Live What-If #2 via UI (natural replay mode -> 360k)
          await session.eval(`(() => {
            ${BROWSER_UI_HELPERS_SRC}
            const c = findRunnerControls();
            if (c.seed) setInputValue(c.seed, 7);
            if (c.budget) setInputValue(c.budget, 10000);
            if (c.cpc) setInputValue(c.cpc, 100);
            if (c.naturalToggle) setCheckbox(c.naturalToggle, true);
            else if (c.impressions) setInputValue(c.impressions, '');
            c.submitButton.click();
          })()`);
          let run2 = null;
          deadline = Date.now() + 20_000;
          while (Date.now() < deadline) {
            const { data } = await httpGetJson(`${server.url}/api/sim-agent/latest`);
            if (data?.config?.sim_seed === 7 && data?.config?.impressions === null && data?.summary?.total_impressions === 360000) {
              run2 = data;
              break;
            }
            await new Promise((r) => setTimeout(r, 150));
          }
          assert.ok(run2, "What-If #2 (natural replay) must complete and update server latest");
          assert.deepStrictEqual(session.consoleErrors, []);
          assert.deepStrictEqual(session.pageErrors, []);

          // Step 5: Invalid payload resilience
          const badRes = await httpPostJson(`${server.url}/api/sim-agent/run`, { cpc_cents: 0 }, { strictJson: false });
          assert.strictEqual(badRes.status, 400);
          assert.ok(typeof badRes.data?.error === "string" && badRes.data.error.length > 0);
          const health = await httpGetJson(`${server.url}/health`);
          assert.strictEqual(health.status, 200);
          assert.deepStrictEqual(health.data, { status: "ok" });
        } finally {
          await session.close();
        }
      } finally {
        await server.stop();
      }
    },
  }),

  defineTest({
    id: "T4-04",
    tier: 4,
    area: "workflow",
    areas: ["sim-agent-plotting", "dashboard-runner-export", "dashboard-ui-interactivity"],
    features: ["F9", "F12", "F13", "F16", "F17"],
    title: "CI Static Artifact Export & Air-Gapped Offline Review over HTTP and file://",
    async fn(ctx) {
      const ciRun = path.join(ctx.tmpDir, "ci_run");
      const ciBundle = path.join(ctx.tmpDir, "ci_bundle");

      await runBinaryOk("sim-agent", [
        "--seed", "2026",
        "--impressions", "400000",
        "--output-dir", ciRun,
      ]);
      const expectedTs = readJsonFile(path.join(ciRun, "budget_timeseries.json"));

      await runBinaryOk("ads-simulator-dashboard", [
        "--sim-report", path.join(ciRun, "budget_timeseries.json"),
        "--plot-file", path.join(ciRun, "budget_spent_30d.svg"),
        "--export-dir", ciBundle,
      ]);

      const bundle = validateStaticExportBundle(ciBundle, { expectedTimeseries: expectedTs });
      assert.strictEqual(bundle.timeseries.config.sim_seed, 2026);
      assert.strictEqual(bundle.timeseries.summary.total_impressions, 400000);

      // 1. Verify via static HTTP server (zero /api/* calls)
      const staticSrv = await startStaticServer(ciBundle);
      try {
        const session = await openBrowserSession(`${staticSrv.url}/index.html`);
        try {
          await session.waitFor(
            `document.body.innerText.includes(${JSON.stringify(bundle.catalog.pieces[0].name)})`,
            12_000,
          );
          await new Promise((r) => setTimeout(r, 250));
          assert.deepStrictEqual(staticSrv.requests.filter((r) => r.path.startsWith("/api")), []);
          assert.deepStrictEqual(
            staticSrv.requests.filter((r) => r.status >= 400 && r.path !== "/favicon.ico"),
            [],
          );
          assert.deepStrictEqual(session.consoleErrors, []);
          assert.deepStrictEqual(session.pageErrors, []);
          const norm = (await session.eval("document.body.innerText")).replace(/(\d),(\d)/g, "$1$2");
          assert.ok(norm.includes("400000"), "Static HTTP view must display 400,000 impressions");
        } finally {
          await session.close();
        }
      } finally {
        await staticSrv.stop();
      }

      // 2. Verify directly over file:// protocol
      const fileSession = await openBrowserSession(pathToFileURL(path.join(ciBundle, "index.html")).href);
      try {
        await fileSession.waitFor(
          `document.body.innerText.includes(${JSON.stringify(bundle.catalog.pieces[0].name)})`,
          12_000,
        );
        const bodyText = await fileSession.eval("document.body.innerText");
        assert.deepStrictEqual(fileSession.pageErrors, []);
        for (const p of bundle.catalog.pieces) {
          assert.ok(bodyText.includes(p.name), `file:// render missing piece "${p.name}"`);
        }
      } finally {
        await fileSession.close();
      }
    },
  }),

  defineTest({
    id: "T4-05",
    tier: 4,
    area: "workflow",
    areas: ["marketplace-generator", "sim-agent-plotting", "dashboard-catalog-api"],
    features: ["F6", "F9", "F11", "F12", "F15"],
    title: "Data Scientist Multi-Seed Reproducibility & Statistical Sensitivity Study",
    async fn(ctx) {
      const dirs = {
        s42a: path.join(ctx.tmpDir, "s42_a"),
        s42b: path.join(ctx.tmpDir, "s42_b"),
        s100: path.join(ctx.tmpDir, "s100"),
        s200: path.join(ctx.tmpDir, "s200"),
      };
      await runBinaryOk("sim-agent", ["--marketplace-seed", "42", "--seed", "42", "--impressions", "300000", "--output-dir", dirs.s42a]);
      await runBinaryOk("sim-agent", ["--marketplace-seed", "42", "--seed", "42", "--impressions", "300000", "--output-dir", dirs.s42b]);
      await runBinaryOk("sim-agent", ["--marketplace-seed", "100", "--seed", "100", "--impressions", "300000", "--output-dir", dirs.s100]);
      await runBinaryOk("sim-agent", ["--marketplace-seed", "200", "--seed", "200", "--impressions", "300000", "--output-dir", dirs.s200]);

      // Byte-for-byte determinism for identical seed=42
      for (const fname of ["simulation.json", "budget_timeseries.json", "budget_timeseries.csv", "budget_spent_30d.svg"]) {
        assert.strictEqual(
          Buffer.compare(fs.readFileSync(path.join(dirs.s42a, fname)), fs.readFileSync(path.join(dirs.s42b, fname))),
          0,
          `Identical seed=42 must produce byte-identical ${fname}`,
        );
      }

      // Statistical validity across 3 distinct seeds
      const runs = [dirs.s42a, dirs.s100, dirs.s200].map((d) => ({
        ts: readJsonFile(path.join(d, "budget_timeseries.json")),
        svg: fs.readFileSync(path.join(d, "budget_spent_30d.svg"), "utf8"),
      }));
      for (const r of runs) {
        validateBudgetTimeSeries(r.ts, { impressions: 300000 });
        validateBudgetSvg(r.svg, r.ts);
        assert.ok(
          r.ts.summary.ctr_pct >= 0.05 && r.ts.summary.ctr_pct <= 0.6,
          `CTR ${r.ts.summary.ctr_pct}% outside plausible statistical band [0.05%, 0.6%]`,
        );
      }
      assert.notDeepStrictEqual(runs[0].ts.days, runs[1].ts.days);
      assert.notDeepStrictEqual(runs[1].ts.days, runs[2].ts.days);
      assert.notStrictEqual(runs[0].svg, runs[1].svg);
      assert.notStrictEqual(runs[1].svg, runs[2].svg);

      const server = await startDashboardServer();
      try {
        const m100 = await httpGetJson(`${server.url}/api/marketplace?seed=100`);
        const m200 = await httpGetJson(`${server.url}/api/marketplace?seed=200`);
        validateMarketplaceApiResponse(m100.data, { expectedSeed: 100 });
        validateMarketplaceApiResponse(m200.data, { expectedSeed: 200 });
        assert.deepStrictEqual(m100.data.taxonomy, m200.data.taxonomy);
        assert.deepStrictEqual(m100.data.counts, m200.data.counts);
      } finally {
        await server.stop();
      }
    },
  }),

  defineTest({
    id: "T4-06",
    tier: 4,
    area: "workflow",
    areas: [
      "python-removal-hygiene",
      "marketplace-generator",
      "cpc-simulator",
      "sim-agent-plotting",
      "dashboard-catalog-api",
      "dashboard-ui-interactivity",
    ],
    features: ["F1", "F5", "F6", "F7", "F9", "F12", "F14", "F15", "F16"],
    title: "Greenfield Clean-Room Pipeline (Zero-Python -> Marketplace -> Engine -> Agent -> Dashboard + Chrome)",
    async fn(ctx) {
      assertZeroPythonFiles(ctx.repoRoot);
      assert.ok(fs.existsSync(path.join(ctx.repoRoot, "Cargo.lock")));
      for (const bin of ["world-generator", "ads-marketplace-simulation", "sim-agent", "ads-simulator-dashboard"]) {
        assert.ok(fs.existsSync(resolveBinary(bin)), `Binary ${bin} must exist`);
      }

      const s1 = path.join(ctx.tmpDir, "s1");
      const s2Sim = path.join(ctx.tmpDir, "s2", "sim.json");
      const s3 = path.join(ctx.tmpDir, "s3");

      await runBinaryOk("world-generator", ["--seed", "55", "--output-dir", s1]);
      const mpPath = path.join(s1, "marketplace.json");
      validateMarketplaceJson(readJsonFile(mpPath), { expectedSeed: 55 });

      await runBinaryOk("ads-marketplace-simulation", [
        "--marketplace", mpPath, "--output", s2Sim, "--seed", "55", "--impressions", "240000",
      ]);
      const directSim = readJsonFile(s2Sim);
      validateSimulationReport(directSim, { seed: 55, impressions: 240000 });

      await runBinaryOk("sim-agent", [
        "--marketplace", mpPath, "--seed", "55", "--impressions", "240000", "--output-dir", s3,
      ]);
      const agentSim = readJsonFile(path.join(s3, "simulation.json"));
      const ts = readJsonFile(path.join(s3, "budget_timeseries.json"));
      const csv = fs.readFileSync(path.join(s3, "budget_timeseries.csv"), "utf8");
      const svg = fs.readFileSync(path.join(s3, "budget_spent_30d.svg"), "utf8");

      assert.deepStrictEqual(agentSim, directSim);
      validateBudgetTimeSeries(ts, { marketplace_seed: 55, sim_seed: 55, impressions: 240000 });
      validateBudgetCsv(csv, ts);
      validateBudgetSvg(svg, ts);

      const server = await startDashboardServer([
        "--sim-report", path.join(s3, "budget_timeseries.json"),
        "--plot-file", path.join(s3, "budget_spent_30d.svg"),
      ]);
      try {
        const health = await httpGetJson(`${server.url}/health`);
        const catalog = await httpGetJson(`${server.url}/api/catalog`);
        const mkt = await httpGetJson(`${server.url}/api/marketplace?seed=55`);
        const latest = await httpGetJson(`${server.url}/api/sim-agent/latest`);
        const plot = await httpGetText(`${server.url}/api/artifacts/budget-plot`);

        assert.deepStrictEqual(health.data, { status: "ok" });
        validateSimulatorCatalog(catalog.data, { repoRoot: ctx.repoRoot });
        validateMarketplaceApiResponse(mkt.data, { expectedSeed: 55 });
        assert.deepStrictEqual(latest.data, ts);
        assert.strictEqual(plot.text, svg);

        const session = await openBrowserSession(`${server.url}/`);
        try {
          await session.waitFor(
            `document.body.innerText.includes(${JSON.stringify(catalog.data.pieces[0].name)})`,
            12_000,
          );
          assert.deepStrictEqual(session.consoleErrors, []);
          assert.deepStrictEqual(session.pageErrors, []);
          assert.deepStrictEqual(session.failedRequests, []);
        } finally {
          await session.close();
        }
      } finally {
        await server.stop();
      }
    },
  }),

  defineTest({
    id: "T4-07",
    tier: 4,
    area: "workflow",
    areas: ["cpc-simulator", "sim-agent-plotting", "dashboard-runner-export"],
    features: ["F7", "F9", "F11", "F12", "F15"],
    title: "Simulation Researcher Natural Replay vs Exact Impression-Target Comparison Study",
    async fn(ctx) {
      const natDir = path.join(ctx.tmpDir, "natural");
      const t360Dir = path.join(ctx.tmpDir, "target360k");
      const t1mDir = path.join(ctx.tmpDir, "target1m");

      await runBinaryOk("sim-agent", [
        "--no-impression-target", "--seed", "42", "--campaign-budget-cents", "10000", "--cpc-cents", "100", "--output-dir", natDir,
      ]);
      await runBinaryOk("sim-agent", [
        "--impressions", "360000", "--seed", "42", "--campaign-budget-cents", "10000", "--cpc-cents", "100", "--output-dir", t360Dir,
      ]);
      await runBinaryOk("sim-agent", [
        "--impressions", "1000000", "--seed", "42", "--campaign-budget-cents", "10000", "--cpc-cents", "100", "--output-dir", t1mDir,
      ]);

      const natTs = readJsonFile(path.join(natDir, "budget_timeseries.json"));
      const natSvg = fs.readFileSync(path.join(natDir, "budget_spent_30d.svg"), "utf8");
      const t360Ts = readJsonFile(path.join(t360Dir, "budget_timeseries.json"));
      const t1mTs = readJsonFile(path.join(t1mDir, "budget_timeseries.json"));

      validateBudgetTimeSeries(natTs, { sim_seed: 42, impressions: null });
      validateBudgetTimeSeries(t360Ts, { sim_seed: 42, impressions: 360000 });
      validateBudgetTimeSeries(t1mTs, { sim_seed: 42, impressions: 1000000 });

      assert.strictEqual(natTs.summary.total_impressions, 360000);
      assert.strictEqual(t360Ts.summary.total_impressions, 360000);
      assert.ok(t1mTs.summary.total_spend_cents > t360Ts.summary.total_spend_cents * 2);
      assert.deepStrictEqual(
        natTs.days,
        t360Ts.days,
        "User-independent natural mode (12,000 req/day x 1 slot = 360k) and exact-target 360k mode should share identical daily spend patterns",
      );
      assert.notDeepStrictEqual(
        natTs.days,
        t1mTs.days,
        "360k natural mode and 1M exact-target mode must produce distinct daily spend patterns",
      );

      const server = await startDashboardServer();
      try {
        const post = await httpPostJson(`${server.url}/api/sim-agent/run`, {
          seed: 42,
          campaign_budget_cents: 10000,
          cpc_cents: 100,
          impressions: null,
        });
        assert.strictEqual(post.status, 200);
        assert.deepStrictEqual(post.data.timeseries, natTs);
        assert.strictEqual(post.data.svg, natSvg);
      } finally {
        await server.stop();
      }
    },
  }),
];
