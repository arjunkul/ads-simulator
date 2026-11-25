/**
 * Tier 3 — Cross-Feature Pairwise Combination Tests (12 Tests)
 *
 * Exercises real multi-crate pipelines, CLI-to-HTTP parity, live browser updates,
 * offline static serving, and workspace verification across all 7 feature areas.
 */

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
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
  runBinaryOk,
  runCargo,
  startDashboardServer,
  startStaticServer,
  validateBudgetCsv,
  validateBudgetSvg,
  validateBudgetTimeSeries,
  validateMarketplaceApiResponse,
  validateMarketplaceJson,
  validateSimulationReport,
  validateSimulatorCatalog,
  validateStaticExportBundle,
} from "../helpers.mjs";

export const tests = [
  defineTest({
    id: "T3-01",
    tier: 3,
    area: "cross-feature",
    areas: ["marketplace-generator", "sim-agent-plotting", "dashboard-catalog-api", "dashboard-runner-export"],
    features: ["F6", "F9", "F12", "F13", "F15"],
    title: "3-stage CLI pipeline (ads-simulator -> sim-agent --marketplace -> dashboard --sim-report --plot-file)",
    async fn(ctx) {
      const mktDir = path.join(ctx.tmpDir, "mkt");
      const runDir = path.join(ctx.tmpDir, "run");

      await runBinaryOk("ads-simulator", ["--seed", "77", "--output-dir", mktDir]);
      const mpPath = path.join(mktDir, "marketplace.json");
      validateMarketplaceJson(readJsonFile(mpPath), { expectedSeed: 77 });

      await runBinaryOk("sim-agent", [
        "--marketplace", mpPath,
        "--seed", "77",
        "--campaign-budget-cents", "10000",
        "--cpc-cents", "100",
        "--impressions", "500000",
        "--output-dir", runDir,
      ]);
      const simReport = readJsonFile(path.join(runDir, "simulation.json"));
      const ts = readJsonFile(path.join(runDir, "budget_timeseries.json"));
      const csv = fs.readFileSync(path.join(runDir, "budget_timeseries.csv"), "utf8");
      const svg = fs.readFileSync(path.join(runDir, "budget_spent_30d.svg"), "utf8");

      validateSimulationReport(simReport, { seed: 77, impressions: 500000 });
      validateBudgetTimeSeries(ts, { marketplace_seed: 77, sim_seed: 77, impressions: 500000 });
      validateBudgetCsv(csv, ts);
      validateBudgetSvg(svg, ts);

      const server = await startDashboardServer([
        "--sim-report", path.join(runDir, "budget_timeseries.json"),
        "--plot-file", path.join(runDir, "budget_spent_30d.svg"),
      ]);
      try {
        const latest = await httpGetJson(`${server.url}/api/sim-agent/latest`);
        assert.strictEqual(latest.status, 200);
        assert.deepStrictEqual(latest.data, ts);

        const plot = await httpGetText(`${server.url}/api/artifacts/budget-plot`);
        assert.strictEqual(plot.status, 200);
        assert.ok(plot.contentType.includes("image/svg+xml"));
        assert.strictEqual(plot.text, svg);
      } finally {
        await server.stop();
      }
    },
  }),

  defineTest({
    id: "T3-02",
    tier: 3,
    area: "cross-feature",
    areas: ["cpc-simulator", "sim-agent-plotting"],
    features: ["F7", "F9", "F10", "F11"],
    title: "Engine parity: ads-marketplace-simulation CLI matches sim-agent simulation.json and daily series",
    async fn(ctx) {
      const mktDir = path.join(ctx.tmpDir, "mkt");
      const directOut = path.join(ctx.tmpDir, "direct_sim.json");
      const agentDir = path.join(ctx.tmpDir, "agent");

      await runBinaryOk("ads-simulator", ["--seed", "42", "--output-dir", mktDir]);
      const mpPath = path.join(mktDir, "marketplace.json");

      await runBinaryOk("ads-marketplace-simulation", [
        "--marketplace", mpPath,
        "--output", directOut,
        "--seed", "42",
        "--campaign-budget-cents", "10000",
        "--cpc-cents", "100",
        "--impressions", "1000000",
      ]);
      await runBinaryOk("sim-agent", [
        "--marketplace", mpPath,
        "--seed", "42",
        "--campaign-budget-cents", "10000",
        "--cpc-cents", "100",
        "--impressions", "1000000",
        "--output-dir", agentDir,
      ]);

      const directSim = readJsonFile(directOut);
      const agentSim = readJsonFile(path.join(agentDir, "simulation.json"));
      const ts = readJsonFile(path.join(agentDir, "budget_timeseries.json"));

      assert.deepStrictEqual(agentSim, directSim, "sim-agent simulation.json must match ads-marketplace-simulation CLI exactly");
      assert.strictEqual(directSim.summary.impressions, 1000000);
      assert.strictEqual(directSim.summary.clicks, 1984);
      assert.strictEqual(directSim.summary.spend_cents, 198400);
      assert.strictEqual(directSim.summary.remaining_budget_cents, 4801600);

      let running = 0;
      for (let i = 0; i < 30; i++) {
        running += directSim.days[i].spend_cents;
        assert.strictEqual(ts.days[i].daily_spend_cents, directSim.days[i].spend_cents);
        assert.strictEqual(ts.days[i].clicks, directSim.days[i].clicks);
        assert.strictEqual(ts.days[i].impressions, directSim.days[i].impressions);
        assert.strictEqual(ts.days[i].cumulative_spend_cents, running);
      }
      assert.strictEqual(running, 198400);
    },
  }),

  defineTest({
    id: "T3-03",
    tier: 3,
    area: "cross-feature",
    areas: ["sim-agent-plotting", "dashboard-runner-export"],
    features: ["F9", "F11", "F13", "F17"],
    title: "Natural replay mode (sim-agent --no-impression-target) exported into dashboard --export-dir bundle",
    async fn(ctx) {
      const natDir = path.join(ctx.tmpDir, "natural");
      const exportDir = path.join(ctx.tmpDir, "export");

      await runBinaryOk("sim-agent", [
        "--no-impression-target",
        "--seed", "42",
        "--campaign-budget-cents", "10000",
        "--cpc-cents", "100",
        "--output-dir", natDir,
      ]);
      const natTs = readJsonFile(path.join(natDir, "budget_timeseries.json"));
      const natSvg = fs.readFileSync(path.join(natDir, "budget_spent_30d.svg"), "utf8");
      assert.strictEqual(natTs.config.impressions, null);
      assert.strictEqual(natTs.summary.total_impressions, 360000);

      await runBinaryOk("ads-simulator-dashboard", [
        "--sim-report", path.join(natDir, "budget_timeseries.json"),
        "--plot-file", path.join(natDir, "budget_spent_30d.svg"),
        "--export-dir", exportDir,
      ]);

      const bundle = validateStaticExportBundle(exportDir, { expectedTimeseries: natTs });
      assert.strictEqual(bundle.svg, natSvg);
    },
  }),

  defineTest({
    id: "T3-04",
    tier: 3,
    area: "cross-feature",
    areas: ["sim-agent-plotting", "dashboard-runner-export"],
    features: ["F9", "F12", "F15"],
    title: "Live HTTP POST /api/sim-agent/run achieves exact JSON & byte-for-byte SVG parity with sim-agent CLI",
    async fn(ctx) {
      const cliDir = path.join(ctx.tmpDir, "cli");
      await runBinaryOk("sim-agent", [
        "--marketplace-seed", "99",
        "--seed", "123",
        "--campaign-budget-cents", "8000",
        "--cpc-cents", "200",
        "--impressions", "250000",
        "--output-dir", cliDir,
      ]);
      const cliTs = readJsonFile(path.join(cliDir, "budget_timeseries.json"));
      const cliSvg = fs.readFileSync(path.join(cliDir, "budget_spent_30d.svg"), "utf8");

      const server = await startDashboardServer();
      try {
        const post = await httpPostJson(`${server.url}/api/sim-agent/run`, {
          marketplace_seed: 99,
          seed: 123,
          campaign_budget_cents: 8000,
          cpc_cents: 200,
          impressions: 250000,
        });
        assert.strictEqual(post.status, 200);
        assert.deepStrictEqual(post.data.timeseries, cliTs, "HTTP timeseries must match sim-agent CLI");
        assert.strictEqual(post.data.svg, cliSvg, "HTTP SVG must match sim-agent CLI byte-for-byte");

        const latest = await httpGetJson(`${server.url}/api/sim-agent/latest`);
        const plot = await httpGetText(`${server.url}/api/artifacts/budget-plot`);
        assert.deepStrictEqual(latest.data, cliTs);
        assert.strictEqual(plot.text, cliSvg);
      } finally {
        await server.stop();
      }
    },
  }),

  defineTest({
    id: "T3-05",
    tier: 3,
    area: "cross-feature",
    areas: ["dashboard-catalog-api", "marketplace-generator", "cpc-simulator", "python-removal-hygiene"],
    features: ["F6", "F7", "F14", "F15"],
    title: "Code-derived /api/catalog constants agree with live /api/marketplace, /api/sim-agent/latest, and Rust source paths",
    async fn(ctx) {
      const server = await startDashboardServer();
      try {
        const [catRes, mktRes, latestRes] = await Promise.all([
          httpGetJson(`${server.url}/api/catalog`),
          httpGetJson(`${server.url}/api/marketplace?seed=42`),
          httpGetJson(`${server.url}/api/sim-agent/latest`),
        ]);
        assert.strictEqual(catRes.status, 200);
        assert.strictEqual(mktRes.status, 200);
        assert.strictEqual(latestRes.status, 200);

        validateSimulatorCatalog(catRes.data, { repoRoot: ctx.repoRoot });
        validateMarketplaceApiResponse(mktRes.data, { expectedSeed: 42 });
        validateBudgetTimeSeries(latestRes.data);

        assert.strictEqual(latestRes.data.days.length, 30);
        assert.strictEqual(latestRes.data.summary.total_campaigns, 500);
        assert.strictEqual(mktRes.data.counts.categories, 100);
        assert.strictEqual(mktRes.data.counts.advertisers, 50);
        assert.strictEqual(mktRes.data.counts.users, 1000);
      } finally {
        await server.stop();
      }
    },
  }),

  defineTest({
    id: "T3-06",
    tier: 3,
    area: "cross-feature",
    areas: ["marketplace-generator", "dashboard-catalog-api"],
    features: ["F6", "F15"],
    title: "GET /api/marketplace?seed=1337 matches exact stats computed from ads-simulator --seed 1337 CLI",
    async fn(ctx) {
      const mktDir = path.join(ctx.tmpDir, "mkt-1337");
      await runBinaryOk("ads-simulator", ["--seed", "1337", "--output-dir", mktDir]);
      const mp = readJsonFile(path.join(mktDir, "marketplace.json"));
      validateMarketplaceJson(mp, { expectedSeed: 1337 });

      // Independently compute expected catalog & organic price stats from CLI JSON
      const catalogPrices = mp.advertisers.flatMap((a) => a.catalog.map((i) => i.price_cents));
      const expectedCatMin = Math.min(...catalogPrices);
      const expectedCatMax = Math.max(...catalogPrices);
      const expectedCatMean = Math.round((catalogPrices.reduce((s, p) => s + p, 0) / catalogPrices.length) * 100) / 100;

      const server = await startDashboardServer();
      try {
        const res = await httpGetJson(`${server.url}/api/marketplace?seed=1337`);
        assert.strictEqual(res.status, 200);
        validateMarketplaceApiResponse(res.data, { expectedSeed: 1337 });

        assert.strictEqual(res.data.price_stats.catalog.min_cents, expectedCatMin);
        assert.strictEqual(res.data.price_stats.catalog.max_cents, expectedCatMax);
        assert.strictEqual(res.data.price_stats.catalog.mean_cents, expectedCatMean);
      } finally {
        await server.stop();
      }
    },
  }),

  defineTest({
    id: "T3-07",
    tier: 3,
    area: "cross-feature",
    areas: ["dashboard-ui-interactivity", "dashboard-runner-export", "sim-agent-plotting"],
    features: ["F12", "F15", "F16"],
    title: "Live POST /api/sim-agent/run via browser form updates rendered SVG and KPI metrics in headless Chrome",
    async fn() {
      const server = await startDashboardServer();
      try {
        const { data: catalog } = await httpGetJson(`${server.url}/api/catalog`);
        const session = await openBrowserSession(`${server.url}/`);
        try {
          const firstPieceName = JSON.stringify(catalog.pieces[0].name);
          await session.waitFor(`document.body.innerText.includes(${firstPieceName})`, 12_000);
          const beforePlot = (await httpGetText(`${server.url}/api/artifacts/budget-plot`)).text;

          const clicked = await session.eval(`(() => {
            ${BROWSER_UI_HELPERS_SRC}
            const c = findRunnerControls();
            if (!c) return false;
            if (c.naturalToggle) setCheckbox(c.naturalToggle, false);
            if (c.seed) setInputValue(c.seed, 7);
            if (c.budget) setInputValue(c.budget, 5000);
            if (c.cpc) setInputValue(c.cpc, 250);
            if (c.impressions) setInputValue(c.impressions, 300000);
            c.submitButton.click();
            return true;
          })()`);
          assert.strictEqual(clicked, true, "Runner controls must be present");

          // Wait for POST /api/sim-agent/run 200 in CDP network requests
          const deadline = Date.now() + 25_000;
          let postSeen = false;
          while (Date.now() < deadline) {
            if (
              session.networkRequests.some(
                (r) => r.method === "POST" && r.url.includes("/api/sim-agent/run") && r.status === 200,
              )
            ) {
              postSeen = true;
              break;
            }
            await new Promise((r) => setTimeout(r, 150));
          }
          assert.strictEqual(postSeen, true, "Expected POST /api/sim-agent/run returning 200 from browser");

          const afterLatest = (await httpGetJson(`${server.url}/api/sim-agent/latest`)).data;
          const afterPlot = (await httpGetText(`${server.url}/api/artifacts/budget-plot`)).text;
          assert.notStrictEqual(afterPlot, beforePlot, "Live run must update the rendered budget SVG");
          validateBudgetSvg(afterPlot, afterLatest);

          const spendCandidates = formatUsdCandidates(afterLatest.summary.total_spend_cents);
          await session.waitFor(
            `${JSON.stringify(spendCandidates)}.some((c) => document.body.innerText.includes(c))`,
            10_000,
          );
          const normBody = (await session.eval("document.body.innerText")).replace(/(\d),(\d)/g, "$1$2");
          assert.ok(normBody.includes("300000"), "Updated DOM must show 300,000 impressions");
          assert.deepStrictEqual(session.consoleErrors, []);
          assert.deepStrictEqual(session.pageErrors, []);
        } finally {
          await session.close();
        }
      } finally {
        await server.stop();
      }
    },
  }),

  defineTest({
    id: "T3-08",
    tier: 3,
    area: "cross-feature",
    areas: ["cpc-simulator", "sim-agent-plotting", "dashboard-catalog-api"],
    features: ["F7", "F10", "F11", "F12", "F15"],
    title: "Full 100% budget exhaustion run served through dashboard API and SVG plot",
    async fn(ctx) {
      const exhaustDir = path.join(ctx.tmpDir, "exhaust");
      await runBinaryOk("sim-agent", [
        "--no-impression-target",
        "--seed", "42",
        "--campaign-budget-cents", "100",
        "--cpc-cents", "100",
        "--output-dir", exhaustDir,
      ]);
      const diskSvg = fs.readFileSync(path.join(exhaustDir, "budget_spent_30d.svg"), "utf8");

      const server = await startDashboardServer([
        "--sim-report", path.join(exhaustDir, "budget_timeseries.json"),
        "--plot-file", path.join(exhaustDir, "budget_spent_30d.svg"),
      ]);
      try {
        const { data: ts } = await httpGetJson(`${server.url}/api/sim-agent/latest`);
        const { text: svg } = await httpGetText(`${server.url}/api/artifacts/budget-plot`);

        validateBudgetTimeSeries(ts, { campaign_budget_cents: 100, cpc_cents: 100, impressions: null });
        assert.strictEqual(ts.summary.total_budget_cents, 50000);
        assert.strictEqual(ts.summary.total_spend_cents, 50000);
        assert.strictEqual(ts.summary.remaining_budget_cents, 0);
        assert.strictEqual(ts.summary.total_clicks, 500);
        assert.ok(Math.abs(ts.summary.utilization_pct - 100) <= 0.05);
        assert.strictEqual(ts.summary.exhausted_campaigns, 500);
        assert.strictEqual(ts.summary.total_campaigns, 500);
        assert.ok(ts.summary.insufficient_budget_opportunities > 0);
        assert.strictEqual(ts.days[29].active_campaigns_end_of_day, 0);
        assert.strictEqual(svg, diskSvg);
        validateBudgetSvg(svg, ts);
      } finally {
        await server.stop();
      }
    },
  }),

  defineTest({
    id: "T3-09",
    tier: 3,
    area: "cross-feature",
    areas: ["marketplace-generator", "sim-agent-plotting", "dashboard-runner-export"],
    features: ["F6", "F9", "F13", "F17"],
    title: "Negative marketplace seed (-42) flows end-to-end from sim-agent to dashboard static export",
    async fn(ctx) {
      const mktNeg = path.join(ctx.tmpDir, "mkt-neg");
      const runNeg = path.join(ctx.tmpDir, "run-neg");
      const runPos = path.join(ctx.tmpDir, "run-pos");
      const exportNeg = path.join(ctx.tmpDir, "export-neg");

      await runBinaryOk("ads-simulator", ["--seed", "-42", "--output-dir", mktNeg]);
      assert.strictEqual(readJsonFile(path.join(mktNeg, "marketplace.json")).seed, -42);

      await runBinaryOk("sim-agent", [
        "--marketplace-seed", "-42", "--seed", "42", "--impressions", "120000", "--output-dir", runNeg,
      ]);
      await runBinaryOk("sim-agent", [
        "--marketplace-seed", "42", "--seed", "42", "--impressions", "120000", "--output-dir", runPos,
      ]);

      const tsNeg = readJsonFile(path.join(runNeg, "budget_timeseries.json"));
      const tsPos = readJsonFile(path.join(runPos, "budget_timeseries.json"));
      validateBudgetTimeSeries(tsNeg, { marketplace_seed: -42, sim_seed: 42, impressions: 120000 });
      assert.notDeepStrictEqual(tsNeg.days, tsPos.days, "Negative seed -42 must produce distinct realization from +42");

      await runBinaryOk("ads-simulator-dashboard", [
        "--sim-report", path.join(runNeg, "budget_timeseries.json"),
        "--plot-file", path.join(runNeg, "budget_spent_30d.svg"),
        "--export-dir", exportNeg,
      ]);
      const bundle = validateStaticExportBundle(exportNeg, { expectedTimeseries: tsNeg });
      assert.strictEqual(bundle.timeseries.config.marketplace_seed, -42);
    },
  }),

  defineTest({
    id: "T3-10",
    tier: 3,
    area: "cross-feature",
    areas: ["python-removal-hygiene", "marketplace-generator", "cpc-simulator", "sim-agent-plotting", "dashboard-catalog-api"],
    features: ["F1", "F2", "F3", "F4", "F5", "F18"],
    title: "Python-free repository hygiene paired with unified cargo test --workspace --locked",
    timeoutMs: 180_000,
    async fn(ctx) {
      assertZeroPythonFiles(ctx.repoRoot);
      assert.strictEqual(fs.existsSync(path.join(ctx.repoRoot, "simulation", "Cargo.lock")), false);
      assert.strictEqual(fs.existsSync(path.join(ctx.repoRoot, "simulation", ".gitignore")), false);
      assert.ok(fs.existsSync(path.join(ctx.repoRoot, "Cargo.lock")));

      const res = await runCargo(["test", "--workspace", "--locked"], {
        cwd: ctx.repoRoot,
        timeoutMs: 160_000,
      });
      assert.strictEqual(
        res.code,
        0,
        `cargo test --workspace --locked failed (code=${res.code}):\n${res.stdout}\n${res.stderr}`,
      );
    },
  }),

  defineTest({
    id: "T3-11",
    tier: 3,
    area: "cross-feature",
    areas: ["cpc-simulator", "sim-agent-plotting", "dashboard-runner-export"],
    features: ["F8", "F13", "F15"],
    title: "Nested output dirs + raw simulation.json --sim-report paired with distinct --plot-file passthrough",
    async fn(ctx) {
      const mktDir = path.join(ctx.tmpDir, "deep", "mkt", "dir");
      const rawSimPath = path.join(ctx.tmpDir, "a", "b", "c", "raw_simulation.json");
      const agentDir = path.join(ctx.tmpDir, "x", "y", "z", "agent_out");

      await runBinaryOk("ads-simulator", ["--seed", "42", "--output-dir", mktDir]);
      await runBinaryOk("ads-marketplace-simulation", [
        "--marketplace", path.join(mktDir, "marketplace.json"),
        "--output", rawSimPath,
        "--seed", "42",
        "--impressions", "90000",
      ]);
      await runBinaryOk("sim-agent", [
        "--seed", "99",
        "--impressions", "90000",
        "--output-dir", agentDir,
      ]);

      const rawSim = readJsonFile(rawSimPath);
      const customSvg = fs.readFileSync(path.join(agentDir, "budget_spent_30d.svg"), "utf8");

      const server = await startDashboardServer([
        "--sim-report", rawSimPath,
        "--plot-file", path.join(agentDir, "budget_spent_30d.svg"),
      ]);
      try {
        const latest = await httpGetJson(`${server.url}/api/sim-agent/latest`);
        assert.strictEqual(latest.status, 200);
        validateBudgetTimeSeries(latest.data, { sim_seed: 42, impressions: 90000 });
        assert.strictEqual(latest.data.summary.total_spend_cents, rawSim.summary.spend_cents);
        assert.strictEqual(latest.data.summary.total_clicks, rawSim.summary.clicks);

        const plot = await httpGetText(`${server.url}/api/artifacts/budget-plot`);
        assert.strictEqual(plot.status, 200);
        assert.strictEqual(plot.text, customSvg, "--plot-file must pass through exact provided SVG");
      } finally {
        await server.stop();
      }
    },
  }),

  defineTest({
    id: "T3-12",
    tier: 3,
    area: "cross-feature",
    areas: ["dashboard-ui-interactivity", "dashboard-runner-export", "sim-agent-plotting"],
    features: ["F12", "F16", "F17"],
    title: "Static bundle served via pure-Node HTTP server renders in Chrome with zero /api/* requests",
    async fn(ctx) {
      const exportDir = path.join(ctx.tmpDir, "static-bundle");
      await runBinaryOk("ads-simulator-dashboard", ["--export-dir", exportDir]);
      const bundle = validateStaticExportBundle(exportDir);

      const staticSrv = await startStaticServer(exportDir);
      try {
        const session = await openBrowserSession(`${staticSrv.url}/index.html`);
        try {
          const firstPiece = JSON.stringify(bundle.catalog.pieces[0].name);
          await session.waitFor(`document.body.innerText.includes(${firstPiece})`, 12_000);
          await new Promise((r) => setTimeout(r, 300));

          const apiReqsServer = staticSrv.requests.filter((r) => r.path.startsWith("/api"));
          const errorReqsServer = staticSrv.requests.filter(
            (r) => r.status >= 400 && r.path !== "/favicon.ico",
          );
          const apiReqsChrome = session.networkRequests.filter((r) => new URL(r.url).pathname.startsWith("/api"));

          assert.deepStrictEqual(apiReqsServer, [], `Static bundle must make zero /api/* requests; server saw: ${JSON.stringify(apiReqsServer)}`);
          assert.deepStrictEqual(errorReqsServer, [], `Static server saw 4xx/5xx responses: ${JSON.stringify(errorReqsServer)}`);
          assert.deepStrictEqual(apiReqsChrome, [], `Chrome network log saw /api/* requests: ${JSON.stringify(apiReqsChrome)}`);
          assert.deepStrictEqual(session.consoleErrors, []);
          assert.deepStrictEqual(session.pageErrors, []);
          assert.deepStrictEqual(session.failedRequests, []);

          const dom = await session.eval(`({
            bodyText: document.body.innerText,
            hasChart: Array.from(document.querySelectorAll('svg, img, object, canvas')).some((el) => {
              const r = el.getBoundingClientRect();
              return r.width >= 300 && r.height >= 150;
            }),
          })`);
          for (const piece of bundle.catalog.pieces) {
            assert.ok(dom.bodyText.includes(piece.name), `Static HTTP render missing piece "${piece.name}"`);
          }
          assert.strictEqual(dom.hasChart, true, "Static HTTP render must include visible chart >= 300x150");
        } finally {
          await session.close();
        }
      } finally {
        await staticSrv.stop();
      }
    },
  }),
];
