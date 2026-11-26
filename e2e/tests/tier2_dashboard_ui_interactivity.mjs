/**
 * Tier 2 (Boundary / Edge / Negative) — Area 7: Dashboard Browser/UI Rendering & Interactivity
 */

import assert from "node:assert/strict";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { defineTest } from "../registry.mjs";
import {
  BROWSER_UI_HELPERS_SRC,
  formatUsdCandidates,
  httpGetJson,
  openBrowserSession,
  runBinaryOk,
  startDashboardServer,
  validateStaticExportBundle,
} from "../helpers.mjs";

const AREA = "dashboard-ui-interactivity";

async function waitForCatalogRender(session, catalog) {
  const firstName = JSON.stringify(catalog.pieces[0].name);
  await session.waitFor(
    `document.body && document.body.innerText && document.body.innerText.includes(${firstName})`,
    12_000,
  );
}

export const tests = [
  defineTest({
    id: "T2-A7-01",
    tier: 2,
    area: AREA,
    features: ["F16"],
    title: "Non-matching feature search shows graceful 0-match state and recovers on clear",
    timeoutMs: 45_000,
    async fn() {
      const server = await startDashboardServer();
      try {
        const { data: catalog } = await httpGetJson(`${server.url}/api/catalog`);
        const session = await openBrowserSession(`${server.url}/`);
        try {
          await waitForCatalogRender(session, catalog);
          const piecesJson = JSON.stringify(catalog.pieces);
          const res = await session.eval(`(async () => {
            ${BROWSER_UI_HELPERS_SRC}
            const pieces = ${piecesJson};
            const input = findSearchInput();
            if (!input) return { foundInput: false };
            setInputValue(input, 'zzzz_no_such_feature_xyz');
            await new Promise((r) => setTimeout(r, 150));
            const zeroCount = countVisibleCatalogPieces(pieces).count;
            setInputValue(input, '');
            await new Promise((r) => setTimeout(r, 150));
            const restoredCount = countVisibleCatalogPieces(pieces).count;
            return { foundInput: true, zeroCount, restoredCount };
          })()`);

          assert.strictEqual(res.foundInput, true, "Search input must exist");
          assert.strictEqual(res.zeroCount, 0, `Expected 0 visible pieces for nonsense search, got ${res.zeroCount}`);
          assert.strictEqual(res.restoredCount, 11, `Clearing search must restore 11 pieces, got ${res.restoredCount}`);
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
    id: "T2-A7-02",
    tier: 2,
    area: AREA,
    features: ["F15", "F16"],
    title: "Submitting invalid simulation params (cpc=0) displays graceful error and recovers on valid rerun",
    timeoutMs: 60_000,
    async fn() {
      const server = await startDashboardServer();
      try {
        const { data: catalog } = await httpGetJson(`${server.url}/api/catalog`);
        const session = await openBrowserSession(`${server.url}/`);
        try {
          await waitForCatalogRender(session, catalog);
          // Submit invalid cpc=0 (set novalidate on form so server 400 or custom JS error handler runs)
          const invRes = await session.eval(`(() => {
            ${BROWSER_UI_HELPERS_SRC}
            const c = findRunnerControls();
            if (!c) return { found: false };
            if (c.form) c.form.setAttribute('novalidate', 'novalidate');
            if (c.cpc) setInputValue(c.cpc, 0);
            c.submitButton.click();
            return { found: true };
          })()`);
          assert.strictEqual(invRes.found, true, "Runner form controls must exist");

          await session.waitFor(
            `/error|invalid|cpc|positive|greater than 0|must be/i.test(document.body.innerText)`,
            8_000,
          );
          const errText = await session.eval("document.body.innerText");
          assert.ok(!/\b(NaN|undefined)\b/.test(errText), "Error state must not inject NaN/undefined");
          assert.deepStrictEqual(session.pageErrors, [], "Invalid submission must not throw uncaught JS exceptions");

          // Recover with valid submission
          await session.eval(`(() => {
            ${BROWSER_UI_HELPERS_SRC}
            const c = findRunnerControls();
            if (c.naturalToggle) setCheckbox(c.naturalToggle, false);
            if (c.seed) setInputValue(c.seed, 51);
            if (c.budget) setInputValue(c.budget, 10000);
            if (c.cpc) setInputValue(c.cpc, 100);
            if (c.impressions) setInputValue(c.impressions, 60000);
            c.submitButton.click();
          })()`);

          let recovered = false;
          const deadline = Date.now() + 20_000;
          while (Date.now() < deadline) {
            const { data } = await httpGetJson(`${server.url}/api/sim-agent/latest`);
            if (data?.config?.sim_seed === 51 && data?.summary?.total_impressions === 60000) {
              recovered = true;
              break;
            }
            await new Promise((r) => setTimeout(r, 150));
          }
          assert.strictEqual(recovered, true, "UI must recover and run valid simulation after error");
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
    id: "T2-A7-03",
    tier: 2,
    area: AREA,
    features: ["F15", "F16"],
    title: "Zero-spend simulation triggered via UI renders $0.00 cleanly with valid chart geometry",
    timeoutMs: 60_000,
    async fn() {
      const server = await startDashboardServer();
      try {
        const { data: catalog } = await httpGetJson(`${server.url}/api/catalog`);
        const session = await openBrowserSession(`${server.url}/`);
        try {
          await waitForCatalogRender(session, catalog);
          const triggered = await session.eval(`(() => {
            ${BROWSER_UI_HELPERS_SRC}
            const c = findRunnerControls();
            if (!c) return false;
            if (c.budget) setInputValue(c.budget, 50);
            if (c.cpc) setInputValue(c.cpc, 100);
            if (c.naturalToggle) {
              setCheckbox(c.naturalToggle, true);
            } else if (c.impressions) {
              setInputValue(c.impressions, '');
            }
            c.submitButton.click();
            return true;
          })()`);
          assert.strictEqual(triggered, true, "Runner controls must exist");

          let zeroRun = null;
          const deadline = Date.now() + 20_000;
          while (Date.now() < deadline) {
            const { data } = await httpGetJson(`${server.url}/api/sim-agent/latest`);
            if (data?.summary?.total_spend_cents === 0 && data?.config?.campaign_budget_cents === 50) {
              zeroRun = data;
              break;
            }
            await new Promise((r) => setTimeout(r, 150));
          }
          assert.ok(zeroRun, "Zero-spend natural replay must complete via UI");

          const zeroCandidates = formatUsdCandidates(0);
          await session.waitFor(
            `${JSON.stringify(zeroCandidates)}.some((c) => document.body.innerText.includes(c))`,
            8_000,
          );
          const domState = await session.eval(`({
            bodyText: document.body.innerText,
            hasChart: Array.from(document.querySelectorAll('svg, img, object, canvas')).some((el) => {
              const r = el.getBoundingClientRect();
              return r.width >= 300 && r.height >= 150;
            }),
          })`);
          assert.ok(!/\b(NaN|Infinity|undefined)\b/.test(domState.bodyText), "Zero-spend DOM must have zero NaN/Infinity");
          assert.strictEqual(domState.hasChart, true, "Zero-spend run must still render chart with valid bounding box");
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
    id: "T2-A7-04",
    tier: 2,
    area: AREA,
    features: ["F16", "F17"],
    title: "Static exported index.html renders all 11 pieces and budget chart directly over file://",
    timeoutMs: 45_000,
    async fn(ctx) {
      const exportDir = path.join(ctx.tmpDir, "file-export");
      await runBinaryOk("ads-simulator-dashboard", ["--export-dir", exportDir]);
      const bundle = validateStaticExportBundle(exportDir);

      const fileUrl = pathToFileURL(path.join(exportDir, "index.html")).href;
      const session = await openBrowserSession(fileUrl);
      try {
        await waitForCatalogRender(session, bundle.catalog);
        const state = await session.eval(`({
          bodyText: document.body.innerText,
          hasChart: Array.from(document.querySelectorAll('svg, img, object, canvas')).some((el) => {
            const r = el.getBoundingClientRect();
            return r.width >= 300 && r.height >= 150;
          }),
        })`);

        assert.deepStrictEqual(session.pageErrors, [], `Uncaught exceptions on file:// load: ${session.pageErrors.join("; ")}`);
        for (const piece of bundle.catalog.pieces) {
          assert.ok(state.bodyText.includes(piece.name), `Offline file:// render missing piece "${piece.name}"`);
        }
        assert.strictEqual(state.hasChart, true, "Offline file:// render must display budget chart >= 300x150");
        const spendCandidates = formatUsdCandidates(bundle.timeseries.summary.total_spend_cents);
        assert.ok(
          spendCandidates.some((c) => state.bodyText.includes(c)),
          `Offline file:// render must show total spend (${spendCandidates.join(" | ")})`,
        );
      } finally {
        await session.close();
      }
    },
  }),

  defineTest({
    id: "T2-A7-05",
    tier: 2,
    area: AREA,
    features: ["F16"],
    title: "Responsive mobile viewport (375x812) renders cleanly without severe horizontal overflow",
    timeoutMs: 45_000,
    async fn() {
      const server = await startDashboardServer();
      try {
        const { data: catalog } = await httpGetJson(`${server.url}/api/catalog`);
        const session = await openBrowserSession(`${server.url}/`, { viewport: { width: 375, height: 812 } });
        try {
          await waitForCatalogRender(session, catalog);
          const layout = await session.eval(`({
            scrollWidth: document.documentElement.scrollWidth,
            innerWidth: window.innerWidth,
            bodyText: document.body.innerText,
          })`);
          assert.deepStrictEqual(session.consoleErrors, []);
          assert.deepStrictEqual(session.pageErrors, []);
          assert.deepStrictEqual(session.failedRequests, []);
          assert.ok(
            layout.scrollWidth <= 375 + 24,
            `Mobile layout overflow detected: scrollWidth=${layout.scrollWidth} > 399`,
          );
          for (const p of catalog.pieces) {
            assert.ok(layout.bodyText.includes(p.name), `Mobile view missing piece "${p.name}"`);
          }
        } finally {
          await session.close();
        }
      } finally {
        await server.stop();
      }
    },
  }),

  defineTest({
    id: "T2-A7-06",
    tier: 2,
    area: AREA,
    features: ["F16"],
    title: "XSS / HTML-injection payload in interactive search input never executes scripts",
    timeoutMs: 45_000,
    async fn() {
      const server = await startDashboardServer();
      try {
        const { data: catalog } = await httpGetJson(`${server.url}/api/catalog`);
        const session = await openBrowserSession(`${server.url}/`);
        try {
          await waitForCatalogRender(session, catalog);
          const piecesJson = JSON.stringify(catalog.pieces);
          const res = await session.eval(`(async () => {
            ${BROWSER_UI_HELPERS_SRC}
            const pieces = ${piecesJson};
            const input = findSearchInput();
            if (!input) return { foundInput: false };
            setInputValue(input, '<img src=x onerror="window.__e2e_xss=true">');
            await new Promise((r) => setTimeout(r, 300));
            const xssTriggered = Boolean(window.__e2e_xss);
            setInputValue(input, '');
            await new Promise((r) => setTimeout(r, 150));
            const restored = countVisibleCatalogPieces(pieces).count;
            return { foundInput: true, xssTriggered, restored };
          })()`);

          assert.strictEqual(res.foundInput, true, "Search input must exist");
          assert.strictEqual(res.xssTriggered, false, "XSS payload must NEVER execute in search filter");
          assert.strictEqual(res.restored, 11, "Clearing search input must restore all 11 pieces");
          assert.deepStrictEqual(session.pageErrors, []);
        } finally {
          await session.close();
        }
      } finally {
        await server.stop();
      }
    },
  }),
];
