/**
 * Tier 1 (Happy-Path) — Area 7: Dashboard Browser/UI Rendering & Interactivity
 */

import assert from "node:assert/strict";
import { defineTest } from "../registry.mjs";
import {
  BROWSER_UI_HELPERS_SRC,
  formatUsdCandidates,
  httpGetJson,
  openBrowserSession,
  startDashboardServer,
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
    id: "T1-A7-01",
    tier: 1,
    area: AREA,
    features: ["F16"],
    title: "Initial dashboard load renders KPI strip cleanly with zero console, page, or network errors",
    timeoutMs: 45_000,
    async fn() {
      const server = await startDashboardServer();
      try {
        const { data: catalog } = await httpGetJson(`${server.url}/api/catalog`);
        const session = await openBrowserSession(`${server.url}/`, { viewport: { width: 1440, height: 900 } });
        try {
          await waitForCatalogRender(session, catalog);
          const info = await session.eval(`({
            title: document.title,
            text: document.body.innerText,
          })`);
          assert.deepStrictEqual(session.consoleErrors, [], `Unexpected console errors: ${session.consoleErrors.join("; ")}`);
          assert.deepStrictEqual(session.pageErrors, [], `Unexpected page errors: ${session.pageErrors.join("; ")}`);
          assert.deepStrictEqual(session.failedRequests, [], `Unexpected failed requests: ${JSON.stringify(session.failedRequests)}`);
          assert.ok(typeof info.title === "string" && info.title.trim().length > 0, "document.title must be non-empty");

          const normText = info.text.replace(/(\d),(\d)/g, "$1$2");
          for (const token of [/\b100\b/, /\b50\b/, /\b1000\b/, /\b500\b/, /\b30\b/, /4(?:\.0+)?%|0\.04/]) {
            assert.ok(token.test(normText), `Dashboard page must surface core KPI matching ${token}`);
          }
          assert.ok(!/\b(NaN|Infinity|undefined)\b/.test(info.text), "Visible text must contain zero NaN/Infinity/undefined");
        } finally {
          await session.close();
        }
      } finally {
        await server.stop();
      }
    },
  }),

  defineTest({
    id: "T1-A7-02",
    tier: 1,
    area: AREA,
    features: ["F16"],
    title: "All 11 simulator pieces and interactive architecture pipeline render with positive geometry",
    timeoutMs: 45_000,
    async fn() {
      const server = await startDashboardServer();
      try {
        const { data: catalog } = await httpGetJson(`${server.url}/api/catalog`);
        const session = await openBrowserSession(`${server.url}/`);
        try {
          await waitForCatalogRender(session, catalog);
          const piecesJson = JSON.stringify(catalog.pieces);
          const check = await session.eval(`(() => {
            ${BROWSER_UI_HELPERS_SRC}
            const pieces = ${piecesJson};
            const bodyText = normalizeWs(document.body.innerText);
            const missingNames = pieces.filter((p) => !bodyText.includes(normalizeWs(p.name))).map((p) => p.name);
            const visibleSummaryResult = countVisibleCatalogPieces(pieces);
            const svgs = Array.from(document.querySelectorAll('svg')).map((s) => {
              const r = s.getBoundingClientRect();
              return { w: r.width, h: r.height };
            }).filter((r) => r.w > 0 && r.h > 0);
            // Click a piece / pipeline node if interactive
            const firstPieceName = pieces[0].name;
            const clickable = Array.from(document.querySelectorAll('button, a, [role="button"], svg g, [data-piece-id], [tabindex]'))
              .find((el) => normalizeWs(el.textContent).includes(normalizeWs(firstPieceName)));
            if (clickable && typeof clickable.click === 'function') clickable.click();
            return { missingNames, visibleSummaryCount: visibleSummaryResult.count, svgCount: svgs.length };
          })()`);

          assert.deepStrictEqual(check.missingNames, [], `Missing piece names in DOM: ${check.missingNames.join(", ")}`);
          assert.strictEqual(check.visibleSummaryCount, 11, `Expected all 11 piece summaries visible, got ${check.visibleSummaryCount}`);
          assert.ok(check.svgCount >= 1, "Expected at least one rendered SVG diagram/chart with positive bounding box");
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
    id: "T1-A7-03",
    tier: 1,
    area: AREA,
    features: ["F16"],
    title: "Per-piece feature explorer search narrows visible pieces and restores all 11 on clear",
    timeoutMs: 45_000,
    async fn() {
      const server = await startDashboardServer();
      try {
        const { data: catalog } = await httpGetJson(`${server.url}/api/catalog`);
        const session = await openBrowserSession(`${server.url}/`);
        try {
          await waitForCatalogRender(session, catalog);
          const piecesJson = JSON.stringify(catalog.pieces);
          const targetPiece = catalog.pieces[0];

          const res = await session.eval(`(async () => {
            ${BROWSER_UI_HELPERS_SRC}
            const pieces = ${piecesJson};
            const input = findSearchInput();
            if (!input) return { foundInput: false };
            const initial = countVisibleCatalogPieces(pieces).count;
            setInputValue(input, ${JSON.stringify(targetPiece.name)});
            await new Promise((r) => setTimeout(r, 150));
            const filtered = countVisibleCatalogPieces(pieces);
            setInputValue(input, '');
            await new Promise((r) => setTimeout(r, 150));
            const restored = countVisibleCatalogPieces(pieces).count;
            return { foundInput: true, initial, filteredCount: filtered.count, filteredMatched: filtered.matched, restored };
          })()`);

          assert.strictEqual(res.foundInput, true, "Dashboard UI must provide a visible semantic search/filter input");
          assert.strictEqual(res.initial, 11, `Expected 11 visible pieces initially, got ${res.initial}`);
          assert.ok(
            res.filteredCount >= 1 && res.filteredCount < 11,
            `Expected search for "${targetPiece.name}" to narrow pieces to 1..10, got ${res.filteredCount}`,
          );
          assert.ok(
            res.filteredMatched.includes(targetPiece.id),
            `Target piece "${targetPiece.id}" must remain visible after searching its name`,
          );
          assert.strictEqual(res.restored, 11, `Clearing search must restore all 11 pieces, got ${res.restored}`);
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
    id: "T1-A7-04",
    tier: 1,
    area: AREA,
    features: ["F16"],
    title: "Live 10x10 taxonomy & marketplace inspector renders all 10 taxonomy groups and counts",
    timeoutMs: 45_000,
    async fn() {
      const server = await startDashboardServer();
      try {
        const { data: catalog } = await httpGetJson(`${server.url}/api/catalog`);
        const session = await openBrowserSession(`${server.url}/`);
        try {
          await waitForCatalogRender(session, catalog);
          // Expand any collapsed taxonomy details or click taxonomy tab if present
          const bodyText = await session.eval(`(async () => {
            for (const d of document.querySelectorAll('details:not([open])')) {
              if (/taxonomy|marketplace|categor/i.test(d.textContent || '')) d.open = true;
            }
            const tabBtn = Array.from(document.querySelectorAll('button, [role="tab"]'))
              .find((b) => /taxonomy|marketplace/i.test(b.textContent || ''));
            if (tabBtn && typeof tabBtn.click === 'function') {
              tabBtn.click();
              await new Promise((r) => setTimeout(r, 150));
            }
            return document.body.innerText;
          })()`);

          const expectedGroups = [
            "Electronics",
            "Home",
            "Kitchen",
            "Apparel",
            "Sports",
            "Outdoors",
            "Beauty",
            "Hobbies",
            "Pets",
            "Garden and Tools",
          ];
          for (const g of expectedGroups) {
            assert.ok(bodyText.includes(g), `Dashboard DOM must display taxonomy group "${g}"`);
          }
          const norm = bodyText.replace(/(\d),(\d)/g, "$1$2");
          for (const cnt of [/\b100\b/, /\b50\b/, /\b1000\b/, /\b5000\b/, /\b12000\b/]) {
            assert.ok(cnt.test(norm), `Marketplace inspector must show count matching ${cnt}`);
          }
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
    id: "T1-A7-05",
    tier: 1,
    area: AREA,
    features: ["F16"],
    title: "30-day budget spend chart and sim-agent SVG viewer render with positive geometry and total spend",
    timeoutMs: 45_000,
    async fn() {
      const server = await startDashboardServer();
      try {
        const { data: catalog } = await httpGetJson(`${server.url}/api/catalog`);
        const { data: latest } = await httpGetJson(`${server.url}/api/sim-agent/latest`);
        const session = await openBrowserSession(`${server.url}/`);
        try {
          await waitForCatalogRender(session, catalog);
          const result = await session.eval(`(() => {
            const charts = [];
            for (const el of document.querySelectorAll('svg, img, object, canvas')) {
              const r = el.getBoundingClientRect();
              if (r.width >= 300 && r.height >= 150) {
                charts.push({ tag: el.tagName.toLowerCase(), width: r.width, height: r.height });
              }
            }
            return { charts, bodyText: document.body.innerText };
          })()`);

          assert.ok(
            result.charts.length >= 1,
            `Expected at least one rendered chart element >= 300x150, got ${JSON.stringify(result.charts)}`,
          );
          assert.ok(!/\b(NaN|Infinity|undefined)\b/.test(result.bodyText), "Page text must not contain NaN/Infinity/undefined");
          const spendCandidates = formatUsdCandidates(latest.summary.total_spend_cents);
          assert.ok(
            spendCandidates.some((c) => result.bodyText.includes(c)),
            `Dashboard must display formatted total spend (${spendCandidates.join(" | ")})`,
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
    id: "T1-A7-06",
    tier: 1,
    area: AREA,
    features: ["F15", "F16"],
    title: "Live simulation runner form submits POST /api/sim-agent/run and updates DOM metrics",
    timeoutMs: 60_000,
    async fn() {
      const server = await startDashboardServer();
      try {
        const { data: catalog } = await httpGetJson(`${server.url}/api/catalog`);
        const session = await openBrowserSession(`${server.url}/`);
        try {
          await waitForCatalogRender(session, catalog);
          const submitted = await session.eval(`(() => {
            ${BROWSER_UI_HELPERS_SRC}
            const controls = findRunnerControls();
            if (!controls) return { found: false };
            if (controls.naturalToggle) setCheckbox(controls.naturalToggle, false);
            if (controls.seed) setInputValue(controls.seed, 77);
            if (controls.budget) setInputValue(controls.budget, 20000);
            if (controls.cpc) setInputValue(controls.cpc, 250);
            if (controls.impressions) setInputValue(controls.impressions, 120000);
            controls.submitButton.click();
            return {
              found: true,
              hasSeed: Boolean(controls.seed),
              hasBudget: Boolean(controls.budget),
              hasCpc: Boolean(controls.cpc),
              hasImpressions: Boolean(controls.impressions),
            };
          })()`);

          assert.strictEqual(submitted.found, true, "Dashboard UI must provide a visible live simulation runner form");

          // Wait until server state reflects the submitted run
          let updated = null;
          const deadline = Date.now() + 20_000;
          while (Date.now() < deadline) {
            const { data } = await httpGetJson(`${server.url}/api/sim-agent/latest`);
            if (data?.config?.sim_seed === 77 && data?.config?.cpc_cents === 250 && data?.summary?.total_impressions === 120000) {
              updated = data;
              break;
            }
            await new Promise((r) => setTimeout(r, 150));
          }
          assert.ok(updated, "Submitting live runner form must trigger POST /api/sim-agent/run updating server latest state");

          const expectedSpend = formatUsdCandidates(updated.summary.total_spend_cents);
          await session.waitFor(
            `${JSON.stringify(expectedSpend)}.some((c) => document.body.innerText.includes(c))`,
            10_000,
          );
          const textAfter = await session.eval("document.body.innerText");
          assert.ok(!/\b(NaN|Infinity|undefined)\b/.test(textAfter), "Updated DOM must contain zero NaN/Infinity/undefined");
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
    id: "T1-A7-07",
    tier: 1,
    area: AREA,
    features: ["F14", "F15", "F16"],
    title: "Decoupled flowchart, Automated Simulation Analyzer, and Live Ad-Server (R1) UI panels render and interact cleanly",
    timeoutMs: 60_000,
    async fn() {
      const server = await startDashboardServer();
      try {
        const { data: catalog } = await httpGetJson(`${server.url}/api/catalog`);
        const session = await openBrowserSession(`${server.url}/`);
        try {
          await waitForCatalogRender(session, catalog);

          const initial = await session.eval(`(() => {
            const flowText = document.getElementById('pipeline-container')?.textContent || '';
            const analysisText = document.getElementById('analysis-section')?.innerText || '';
            const topRows = document.querySelectorAll('#analysis-top-campaigns-body tr').length;
            const findingCards = document.querySelectorAll('#analysis-findings > *').length;
            const slateRows = document.querySelectorAll('#ad-server-slate-body tr').length;
            const summaryText = document.getElementById('ad-server-summary')?.innerText || '';
            return { flowText, analysisText, topRows, findingCards, slateRows, summaryText };
          })()`);

          for (const token of ["Visit Simulator", "Ad-Server", "R1", "Simulation Analyzer"]) {
            assert.ok(
              initial.flowText.includes(token),
              `Component flowchart SVG must include "${token}"`,
            );
          }
          assert.ok(initial.topRows === 5, `Expected 5 top campaigns rows in analyzer table, got ${initial.topRows}`);
          assert.ok(initial.findingCards >= 2, `Expected >= 2 automated finding cards, got ${initial.findingCards}`);
          assert.ok(/CTR|eCPM|Utilization/i.test(initial.analysisText), "Analyzer section must display CTR/eCPM/Utilization metrics");
          assert.strictEqual(initial.slateRows, 20, `Expected 20 ad-server R1 slate rows, got ${initial.slateRows}`);
          assert.ok(initial.summaryText.includes("user-0001"), "Initial ad-server summary should show default user-0001");

          // Query Live Ad-Server (R1) for user-0042 with seed 99
          await session.eval(`(() => {
            const u = document.getElementById('ad-server-user-id');
            const s = document.getElementById('ad-server-seed');
            u.value = 'user-0042';
            s.value = '99';
            document.getElementById('ad-server-btn').click();
          })()`);

          await session.waitFor(
            `document.getElementById('ad-server-summary')?.innerText.includes('user-0042')`,
            10_000,
          );

          const afterQuery = await session.eval(`(() => {
            const rows = Array.from(document.querySelectorAll('#ad-server-slate-body tr'));
            const campaignIds = rows.map((tr) => tr.children[1]?.textContent?.trim()).filter(Boolean);
            return {
              rowCount: rows.length,
              uniqueCampaigns: new Set(campaignIds).size,
              summary: document.getElementById('ad-server-summary')?.innerText || '',
            };
          })()`);

          assert.strictEqual(afterQuery.rowCount, 20, "Updated ad-server R1 query must return 20 rows");
          assert.strictEqual(afterQuery.uniqueCampaigns, 20, "All 20 rendered R1 ads must be from distinct campaigns");
          assert.ok(afterQuery.summary.includes("user-0042"), "Updated ad-server summary must show user-0042");
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
];
