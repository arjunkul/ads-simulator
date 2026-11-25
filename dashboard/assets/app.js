(() => {
  "use strict";

  const state = {
    catalog: null,
    marketplace: null,
    timeseries: null,
    analysis: null,
    adServerSample: null,
    svg: null,
    staticMode: false,
    searchQuery: "",
    activeStage: "All",
    selectedPieceId: null,
    diagramMode: "flowchart",
    activeFlow: "all",
    inspectedNodeId: null,
  };

  function el(tag, attrs, children) {
    const node = document.createElement(tag);
    if (attrs) {
      for (const [k, v] of Object.entries(attrs)) {
        if (v === undefined || v === null) continue;
        if (k === "className") node.className = String(v);
        else if (k === "textContent") node.textContent = String(v);
        else node.setAttribute(k, String(v));
      }
    }
    if (children) {
      for (const child of children) {
        if (child === null || child === undefined) continue;
        node.appendChild(typeof child === "string" ? document.createTextNode(child) : child);
      }
    }
    return node;
  }

  function svgEl(tag, attrs, children) {
    const node = document.createElementNS("http://www.w3.org/2000/svg", tag);
    if (attrs) {
      for (const [k, v] of Object.entries(attrs)) {
        if (v === undefined || v === null) continue;
        node.setAttribute(k, String(v));
      }
    }
    if (children) {
      for (const child of children) {
        if (child === null || child === undefined) continue;
        node.appendChild(typeof child === "string" ? document.createTextNode(child) : child);
      }
    }
    return node;
  }

  function fmtInt(n) {
    const num = Number(n);
    if (!Number.isFinite(num)) return "0";
    return Math.round(num).toLocaleString("en-US");
  }

  function fmtUsdCents(cents) {
    const num = Number(cents);
    if (!Number.isFinite(num)) return "$0.00";
    const dollars = num / 100;
    return "$" + dollars.toLocaleString("en-US", {
      minimumFractionDigits: 2,
      maximumFractionDigits: 2,
    });
  }

  function fmtPct(pct, digits = 2) {
    const num = Number(pct);
    if (!Number.isFinite(num)) return "0.00%";
    return num.toFixed(digits) + "%";
  }

  function clearNode(node) {
    while (node && node.firstChild) {
      node.removeChild(node.firstChild);
    }
  }

  function readBootstrap() {
    const script = document.getElementById("dashboard-bootstrap");
    if (!script) return null;
    const raw = (script.textContent || "").trim();
    if (!raw || raw === "null") return null;
    try {
      return JSON.parse(raw);
    } catch {
      return null;
    }
  }

  function renderHeroAndKpis() {
    const catalog = state.catalog;
    const ts = state.timeseries;
    if (!catalog) return;

    const versionBadge = document.getElementById("badge-version");
    const schemaBadge = document.getElementById("badge-schema");
    const rankerBadge = document.getElementById("badge-ranker");
    const modeBadge = document.getElementById("badge-mode");
    versionBadge.textContent = "v" + catalog.simulator_version;
    schemaBadge.textContent = "Schema v" + catalog.schema_version + " · " + catalog.currency;
    if (rankerBadge && ts && ts.config) {
      rankerBadge.textContent = `Ranker ${ts.config.ranker || "R1"} · ${ts.config.slate_size || 20} Ads/Req`;
    }
    modeBadge.textContent = state.staticMode ? "Static Offline Bundle" : "Live Rust Server";

    const sc = catalog.summary_counts;
    const kpiStrip = document.getElementById("kpi-strip");
    clearNode(kpiStrip);

    const spendText = ts ? fmtUsdCents(ts.summary.total_spend_cents) : "$0.00";
    const utilText = ts ? fmtPct(ts.summary.utilization_pct, 2) + " of " + fmtUsdCents(ts.summary.total_budget_cents) : "Pending run";
    const clicksText = ts ? fmtInt(ts.summary.total_clicks) + " clicks" : "0 clicks";
    const imprText = ts ? fmtInt(ts.summary.total_impressions) + " impressions" : "0 impressions";

    const cards = [
      { label: "Simulator Pieces", value: fmtInt(sc.pieces), sub: fmtInt(sc.crates) + " crates · " + fmtInt(sc.implemented_features) + " features" },
      { label: "Categories", value: fmtInt(sc.categories), sub: "10 groups x 10 categories" },
      { label: "Advertisers", value: fmtInt(sc.advertisers), sub: "20 catalog items each" },
      { label: "Shoppers", value: fmtInt(sc.users), sub: "12,000 browse events" },
      { label: "Ad-Server & R1", value: fmtInt(sc.campaigns) + " campaigns", sub: fmtInt(sc.simulation_days) + " days · " + fmtInt(sc.r1_slate_size || 20) + " ads/req · 4.0% max CTR" },
      { label: "30-Day Budget Spent", value: spendText, sub: utilText },
      { label: "Delivered Volume", value: clicksText, sub: imprText },
    ];

    for (const c of cards) {
      kpiStrip.appendChild(
        el("div", { className: "kpi-card" }, [
          el("div", { className: "kpi-label", textContent: c.label }),
          el("div", { className: "kpi-value", textContent: c.value }),
          el("div", { className: "kpi-sub", textContent: c.sub }),
        ]),
      );
    }
  }

  const FLOW_TRACES = [
    { id: "all", label: "All Flows" },
    { id: "marketplace", label: "1. Marketplace Data" },
    { id: "ad-serving", label: "2. Impression Gen -> Ad-Server (R1)" },
    { id: "cpc-loop", label: "3. Click & CPC Loop" },
    { id: "analysis", label: "4. Analyzer & Plot" },
    { id: "live-ui", label: "5. Live Dashboard" },
  ];

  function getFlowchartDefinition(catalog) {
    const pieceMap = new Map((catalog ? catalog.pieces : []).map((p) => [p.id, p]));
    const pieceName = (id, fallback) => (pieceMap.get(id) ? pieceMap.get(id).name : fallback);
    const pieceSummary = (id, fallback) => (pieceMap.get(id) ? pieceMap.get(id).summary : fallback);

    const lanes = [
      { id: "lane-mp", title: "1. ads-simulator", subtitle: "Marketplace & Preferences", x: 12, y: 14, w: 234, h: 616, color: "#38bdf8" },
      { id: "lane-serve", title: "2. impression-generator + ad-server", subtitle: "User-Independent Gen -> Ad-Server (R1)", x: 262, y: 14, w: 344, h: 616, color: "#818cf8" },
      { id: "lane-sim", title: "3. simulation + analyzer", subtitle: "Click Evaluator & Auto-Analysis", x: 622, y: 14, w: 274, h: 616, color: "#34d399" },
      { id: "lane-ui", title: "4. sim-agent + dashboard", subtitle: "SVG Plot, Live API & Export", x: 912, y: 14, w: 236, h: 616, color: "#fbbf24" },
    ];

    const nodes = [
      // Lane 1: ads-simulator
      {
        id: "taxonomy-schema",
        pieceId: "taxonomy-schema",
        kind: "process",
        crate: "ads-simulator",
        title: pieceName("taxonomy-schema", "Product Taxonomy & Schema"),
        sub: "10 groups x 10 categories · Schema v1",
        desc: pieceSummary("taxonomy-schema", "Defines 10x10 taxonomy and Schema v1 contract."),
        x: 24, y: 66, w: 210, h: 64, color: "#38bdf8",
        flows: ["marketplace", "live-ui"],
      },
      {
        id: "advertiser-catalog",
        pieceId: "advertiser-catalog",
        kind: "process",
        crate: "ads-simulator",
        title: pieceName("advertiser-catalog", "Advertiser & Catalog Generator"),
        sub: "50 merchants x 20 offers (1,000 ads)",
        desc: pieceSummary("advertiser-catalog", "Generates 50 merchants and 1,000 catalog offers."),
        x: 24, y: 170, w: 210, h: 64, color: "#38bdf8",
        flows: ["marketplace", "ad-serving"],
      },
      {
        id: "organic-inventory",
        pieceId: "organic-inventory",
        kind: "process",
        crate: "ads-simulator",
        title: pieceName("organic-inventory", "Organic Listing Inventory"),
        sub: "5,000 non-ad listings (50/cat)",
        desc: pieceSummary("organic-inventory", "Generates 5,000 non-ad listings across 100 categories."),
        x: 24, y: 274, w: 210, h: 64, color: "#38bdf8",
        flows: ["marketplace"],
      },
      {
        id: "user-browsing",
        pieceId: "user-browsing",
        kind: "process",
        crate: "ads-simulator",
        title: pieceName("user-browsing", "Shopper Personas & Preference Model"),
        sub: "1,000 users · 5 prefs · score_preference()",
        desc: pieceSummary("user-browsing", "1,000 users with unique 5-category preferences and score_preference(u, item)."),
        x: 24, y: 378, w: 210, h: 72, color: "#38bdf8",
        flows: ["marketplace", "ad-serving", "cpc-loop"],
      },
      {
        id: "marketplace-json",
        pieceId: null,
        kind: "artifact",
        crate: "ads-simulator",
        title: "marketplace.json",
        sub: "ChaCha8 seeded Schema v1",
        desc: "Deterministic synthetic marketplace snapshot loaded via file or generated in memory.",
        x: 34, y: 500, w: 190, h: 52, color: "#38bdf8",
        flows: ["marketplace"],
      },

      // Lane 2: impression-generator + ad-server
      {
        id: "impression-generator",
        pieceId: "impression-generator",
        kind: "process",
        crate: "impression-generator",
        title: pieceName("impression-generator", "User-Independent Ad-Impression Generator"),
        sub: "user_ids ONLY · Stream 1 · 600 req/d or quota",
        desc: pieceSummary("impression-generator", "User-independent request generator issuing serve_ads(user_id) calls without seeing preferences."),
        x: 278, y: 66, w: 312, h: 70, color: "#818cf8",
        flows: ["ad-serving", "cpc-loop"],
      },
      {
        id: "ad-server",
        pieceId: "ad-server",
        kind: "process",
        crate: "ad-server",
        title: pieceName("ad-server", "Ad-Server & Campaign Budget Pool"),
        sub: "serve_ads(user_id) -> distinct funded ads · O(1) pool",
        desc: pieceSummary("ad-server", "Owns 500 campaign budgets and enforces distinct funded campaign selection."),
        x: 278, y: 176, w: 312, h: 70, color: "#818cf8",
        flows: ["ad-serving", "cpc-loop", "live-ui"],
      },
      {
        id: "active-check",
        pieceId: null,
        kind: "decision",
        crate: "ad-server",
        title: "Funded campaigns?",
        sub: "rem >= CPC",
        desc: "Checks AdServer's active campaign pool. Empty pool returns [] and increments insufficient_budget.",
        cx: 404, cy: 300, rx: 88, ry: 32, color: "#fbbf24",
        flows: ["ad-serving", "cpc-loop"],
      },
      {
        id: "skip-slot",
        pieceId: null,
        kind: "artifact",
        crate: "ad-server",
        title: "Empty Slate []",
        sub: "insufficient_budget++",
        desc: "No campaign can afford CPC; AdServer returns empty slate and caller records insufficient_budget.",
        x: 508, y: 275, w: 88, h: 50, color: "#fb7185",
        flows: ["ad-serving", "cpc-loop"],
      },
      {
        id: "ads-ranker-r1",
        pieceId: "ads-ranker-r1",
        kind: "process",
        crate: "ad-server",
        title: pieceName("ads-ranker-r1", "Ads-Ranker Strategy R1"),
        sub: "Select 20 random distinct funded campaigns",
        desc: pieceSummary("ads-ranker-r1", "Pluggable AdsRanker Strategy R1 uniformly sampling up to 20 distinct funded campaigns."),
        x: 278, y: 368, w: 312, h: 70, color: "#a78bfa",
        flows: ["ad-serving", "cpc-loop", "live-ui"],
      },
      {
        id: "served-slate",
        pieceId: null,
        kind: "artifact",
        crate: "ad-server",
        title: "AdResponse (up to 20 ServedAds)",
        sub: "Distinct campaign_ids · verified budget",
        desc: "Set of up to 20 ads from different funded campaigns returned by AdServer::serve_ads(user_id).",
        x: 294, y: 486, w: 280, h: 54, color: "#a78bfa",
        flows: ["ad-serving", "cpc-loop"],
      },

      // Lane 3: simulation + simulation-analyzer
      {
        id: "delivery-click-engine",
        pieceId: "delivery-click-engine",
        kind: "process",
        crate: "ads-marketplace-simulation",
        title: pieceName("delivery-click-engine", "30-Day Simulation & Click Evaluator"),
        sub: "Per ad: click p = 4% * score_preference(u, ad)",
        desc: pieceSummary("delivery-click-engine", "Evaluates shopper click behavior on Stream 2 and reports clicks back to AdServer."),
        x: 638, y: 112, w: 242, h: 76, color: "#34d399",
        flows: ["cpc-loop"],
      },
      {
        id: "click-check",
        pieceId: null,
        kind: "decision",
        crate: "ads-marketplace-simulation",
        title: "Click drawn?",
        sub: "4% pref / 0% else",
        desc: "If clicked, calls ad_server.record_impression(..., true) deducting CPC and retiring exhausted campaigns.",
        cx: 759, cy: 254, rx: 86, ry: 32, color: "#fbbf24",
        flows: ["cpc-loop"],
      },
      {
        id: "report-json",
        pieceId: null,
        kind: "artifact",
        crate: "ads-marketplace-simulation",
        title: "30-Day Report JSON",
        sub: "simulation.json (30d x 500 campaigns)",
        desc: "Balanced multi-level Schema v1 report including ad_requests, ranker R1, and campaign daily ledgers.",
        x: 648, y: 334, w: 222, h: 56, color: "#34d399",
        flows: ["cpc-loop", "analysis"],
      },
      {
        id: "simulation-analyzer",
        pieceId: "simulation-analyzer",
        kind: "process",
        crate: "simulation-analyzer",
        title: pieceName("simulation-analyzer", "Automated Simulation Analyzer"),
        sub: "Depletion · CTR · eCPM · Gini · Findings",
        desc: pieceSummary("simulation-analyzer", "Auto-analyzes budget depletion, average & daily CTR, spend concentration, and findings."),
        x: 638, y: 444, w: 242, h: 80, color: "#34d399",
        flows: ["analysis", "live-ui"],
      },

      // Lane 4: sim-agent + dashboard
      {
        id: "sim-agent-runner",
        pieceId: "sim-agent-runner",
        kind: "process",
        crate: "sim-agent",
        title: pieceName("sim-agent-runner", "Sim-Agent Runner, Auditor & 30-Day Plotter"),
        sub: "Audit + JSON/CSV + analysis.json + SVG",
        desc: pieceSummary("sim-agent-runner", "Runs 30-day simulation, audits ledgers, auto-analyzes, and plots budget_spent_30d.svg."),
        x: 928, y: 112, w: 204, h: 82, color: "#fbbf24",
        flows: ["analysis", "live-ui"],
      },
      {
        id: "svg-artifacts",
        pieceId: null,
        kind: "artifact",
        crate: "sim-agent",
        title: "5 Atomic Artifacts",
        sub: "sim + ts + csv + svg + analysis",
        desc: "Deterministic outputs written atomically to output/sim-agent/.",
        x: 936, y: 242, w: 188, h: 56, color: "#fbbf24",
        flows: ["analysis"],
      },
      {
        id: "dashboard-inspector",
        pieceId: "dashboard-inspector",
        kind: "process",
        crate: "ads-simulator-dashboard",
        title: pieceName("dashboard-inspector", "Feature, Flowchart & Analysis Dashboard"),
        sub: "HTTP/1.1 APIs + R1 Sampler + Static Export",
        desc: pieceSummary("dashboard-inspector", "Interactive web dashboard serving live catalog, flowchart, R1 ad-server sampler, analyzer, and static export."),
        x: 928, y: 356, w: 204, h: 84, color: "#fbbf24",
        flows: ["live-ui"],
      },
      {
        id: "static-bundle",
        pieceId: null,
        kind: "artifact",
        crate: "ads-simulator-dashboard",
        title: "Live UI / Static Bundle",
        sub: "GET / · --export-dir (file://)",
        desc: "Zero-build embedded HTML5/CSS/JS frontend working over live HTTP or air-gapped file://.",
        x: 936, y: 494, w: 188, h: 56, color: "#fbbf24",
        flows: ["live-ui", "analysis"],
      },
    ];

    const edges = [
      // Marketplace generation
      { from: "taxonomy-schema", to: "advertiser-catalog", label: "10x10 IDs", d: "M 129 130 L 129 170", lx: 162, ly: 152, flows: ["marketplace"] },
      { from: "taxonomy-schema", to: "organic-inventory", label: "50 / cat", d: "M 28 130 C 14 185, 14 252, 24 300", lx: 50, ly: 256, flows: ["marketplace"] },
      { from: "organic-inventory", to: "user-browsing", label: "9 aff + 3 exp", d: "M 129 338 L 129 378", lx: 168, ly: 360, flows: ["marketplace"] },
      { from: "user-browsing", to: "marketplace-json", label: "snapshot", d: "M 129 450 L 129 500", lx: 164, ly: 476, flows: ["marketplace"] },

      // Cross-lane: ads-simulator -> impression-generator + ad-server + click evaluator
      { from: "user-browsing", to: "impression-generator", label: "user_ids ONLY", d: "M 234 396 C 254 396, 254 101, 278 101", lx: 256, ly: 254, color: "#38bdf8", flows: ["ad-serving"] },
      { from: "advertiser-catalog", to: "ad-server", label: "500 campaigns", d: "M 234 202 C 254 202, 254 211, 278 211", lx: 256, ly: 192, flows: ["ad-serving"] },
      { from: "user-browsing", to: "delivery-click-engine", label: "score_preference()", d: "M 234 432 C 256 604, 610 604, 638 156", lx: 438, ly: 598, dashed: true, color: "#38bdf8", flows: ["cpc-loop"] },

      // Impression generator -> Ad-Server -> R1 -> Served Slate
      { from: "impression-generator", to: "ad-server", label: "serve_ads(user_id)", d: "M 434 136 L 434 176", lx: 492, ly: 158, color: "#818cf8", flows: ["ad-serving", "cpc-loop"] },
      { from: "ad-server", to: "active-check", label: "check pool", d: "M 404 246 L 404 268", lx: 446, ly: 258, flows: ["ad-serving", "cpc-loop"] },
      { from: "active-check", to: "skip-slot", label: "No", d: "M 492 300 L 508 300", lx: 499, ly: 288, color: "#fb7185", flows: ["ad-serving", "cpc-loop"] },
      { from: "active-check", to: "ads-ranker-r1", label: "Yes", d: "M 404 332 L 404 368", lx: 424, ly: 350, color: "#34d399", flows: ["ad-serving", "cpc-loop"] },
      { from: "ads-ranker-r1", to: "served-slate", label: "20 random funded", d: "M 434 438 L 434 486", lx: 496, ly: 462, color: "#a78bfa", flows: ["ad-serving", "cpc-loop"] },

      // Served Slate -> Click Evaluator -> CPC Retirement Loop + Report
      { from: "served-slate", to: "delivery-click-engine", label: "deliver slate", d: "M 574 513 C 612 513, 612 138, 638 138", lx: 610, ly: 356, flows: ["cpc-loop"] },
      { from: "delivery-click-engine", to: "click-check", label: "per ad draw", d: "M 759 188 L 759 222", lx: 798, ly: 206, flows: ["cpc-loop"] },
      { from: "click-check", to: "ad-server", label: "record_impression (-CPC)", d: "M 673 254 C 608 254, 608 211, 590 211", lx: 614, ly: 226, dashed: true, color: "#fbbf24", flows: ["cpc-loop"] },
      { from: "click-check", to: "report-json", label: "record day", d: "M 759 286 L 759 334", lx: 794, ly: 310, flows: ["cpc-loop", "analysis"] },

      // Report -> Simulation Analyzer & Sim-Agent
      { from: "report-json", to: "simulation-analyzer", label: "analyze_report()", d: "M 759 390 L 759 444", lx: 810, ly: 418, color: "#34d399", flows: ["analysis"] },
      { from: "report-json", to: "sim-agent-runner", label: "audit & series", d: "M 870 362 C 902 362, 902 146, 928 146", lx: 902, ly: 244, flows: ["analysis"] },
      { from: "simulation-analyzer", to: "sim-agent-runner", label: "analysis.json", d: "M 880 484 C 908 484, 908 172, 928 172", lx: 906, ly: 326, color: "#34d399", flows: ["analysis", "live-ui"] },

      // Sim-Agent -> Artifacts & Dashboard
      { from: "sim-agent-runner", to: "svg-artifacts", label: "atomic export", d: "M 1030 194 L 1030 242", lx: 1076, ly: 218, flows: ["analysis"] },
      { from: "svg-artifacts", to: "dashboard-inspector", label: "SVG + analysis", d: "M 1030 298 L 1030 356", lx: 1078, ly: 326, flows: ["live-ui"] },
      { from: "dashboard-inspector", to: "static-bundle", label: "GET / & export", d: "M 1030 440 L 1030 494", lx: 1080, ly: 468, flows: ["live-ui"] },

      // Live feedback loops
      { from: "dashboard-inspector", to: "sim-agent-runner", label: "POST /api/sim-agent/run", d: "M 1132 390 C 1152 390, 1152 146, 1132 146", lx: 1094, ly: 88, dashed: true, color: "#fbbf24", flows: ["live-ui"] },
      { from: "dashboard-inspector", to: "ad-server", label: "/api/ad-server/sample", d: "M 928 388 C 884 584, 594 584, 590 228", lx: 760, ly: 568, dashed: true, color: "#a78bfa", flows: ["live-ui"] },
      { from: "taxonomy-schema", to: "dashboard-inspector", label: "Compiled specs (all 7 crates)", d: "M 129 66 C 129 26, 1030 26, 1030 112", lx: 580, ly: 24, dashed: true, color: "#38bdf8", flows: ["live-ui"] },
    ];

    return { lanes, nodes, edges };
  }

  function renderFlowTracePills() {
    const bar = document.getElementById("flowchart-trace-bar");
    const pills = document.getElementById("flowchart-trace-pills");
    if (!pills || !bar) return;
    bar.classList.toggle("hidden", state.diagramMode !== "flowchart");
    clearNode(pills);
    for (const tr of FLOW_TRACES) {
      const btn = el("button", {
        type: "button",
        className: "pill-btn" + (state.activeFlow === tr.id ? " active" : ""),
        textContent: tr.label,
      });
      btn.addEventListener("click", () => {
        state.activeFlow = tr.id;
        renderFlowTracePills();
        renderPipeline();
      });
      pills.appendChild(btn);
    }
  }

  function renderFlowchartInspector(def) {
    const titleEl = document.getElementById("inspector-title");
    const crateEl = document.getElementById("inspector-crate");
    const descEl = document.getElementById("inspector-desc");
    const ioEl = document.getElementById("inspector-io");
    const jumpBtn = document.getElementById("inspector-jump-btn");
    if (!titleEl || !descEl || !ioEl || !jumpBtn || !crateEl) return;

    const targetId = state.inspectedNodeId || state.selectedPieceId;
    const node = def ? def.nodes.find((n) => n.id === targetId) : null;

    if (!node) {
      titleEl.textContent = "Select any component or decision node in the flowchart";
      crateEl.classList.add("hidden");
      descEl.textContent =
        "Click any box or decision diamond above to trace incoming/outgoing connections, data contracts, and jump directly to its compiled parameters and features.";
      clearNode(ioEl);
      ioEl.classList.add("hidden");
      jumpBtn.classList.add("hidden");
      return;
    }

    titleEl.textContent = node.title;
    crateEl.textContent = node.crate;
    crateEl.classList.remove("hidden");
    descEl.textContent = node.desc;

    const incoming = def.edges.filter((e) => e.to === node.id);
    const outgoing = def.edges.filter((e) => e.from === node.id);
    const nodeById = new Map(def.nodes.map((n) => [n.id, n]));

    clearNode(ioEl);
    for (const e of incoming) {
      const src = nodeById.get(e.from);
      ioEl.appendChild(
        el("span", {
          className: "io-chip io-chip-in",
          textContent: `IN <- ${src ? src.title : e.from} (${e.label})`,
        }),
      );
    }
    for (const e of outgoing) {
      const dst = nodeById.get(e.to);
      ioEl.appendChild(
        el("span", {
          className: "io-chip io-chip-out",
          textContent: `OUT -> ${dst ? dst.title : e.to} (${e.label})`,
        }),
      );
    }
    ioEl.classList.toggle("hidden", incoming.length === 0 && outgoing.length === 0);

    if (node.pieceId) {
      jumpBtn.classList.remove("hidden");
      jumpBtn.onclick = () => {
        state.selectedPieceId = node.pieceId;
        renderPieces();
        const card = document.querySelector('[data-piece-card="' + node.pieceId + '"]');
        if (card) card.scrollIntoView({ behavior: "smooth", block: "nearest" });
      };
    } else {
      jumpBtn.classList.add("hidden");
      jumpBtn.onclick = null;
    }
  }

  function wrapSvgLabel(text, maxChars) {
    if (text.length <= maxChars) return [text];
    const words = text.split(" ");
    const lines = [];
    let current = "";
    for (const w of words) {
      const next = current ? current + " " + w : w;
      if (next.length > maxChars && current) {
        lines.push(current);
        current = w;
        if (lines.length === 1) break;
      } else {
        current = next;
      }
    }
    if (current) {
      if (lines.length === 1) {
        const rest = text.slice(lines[0].length).trim();
        lines.push(rest.length > maxChars + 3 ? rest.slice(0, maxChars) + "..." : rest);
      } else {
        lines.push(current);
      }
    }
    return lines.slice(0, 2);
  }

  function renderInteractionFlowchart(container, catalog) {
    const def = getFlowchartDefinition(catalog);
    const activeNodeId = state.inspectedNodeId;

    const connectedToActive = new Set();
    if (activeNodeId) {
      connectedToActive.add(activeNodeId);
      for (const e of def.edges) {
        if (e.from === activeNodeId || e.to === activeNodeId) {
          connectedToActive.add(e.from);
          connectedToActive.add(e.to);
        }
      }
    }

    const nodeMatchesTrace = (n) =>
      state.activeFlow === "all" || (n.flows && n.flows.includes(state.activeFlow));
    const edgeMatchesTrace = (e) =>
      state.activeFlow === "all" || (e.flows && e.flows.includes(state.activeFlow));

    const svg = svgEl("svg", {
      class: "pipeline-svg",
      viewBox: "0 0 1160 644",
      role: "img",
      "aria-label": "Component interaction flowchart across all 7 simulator workspace crates",
    });

    const makeMarker = (id, color) =>
      svgEl(
        "marker",
        {
          id,
          viewBox: "0 0 10 10",
          refX: "8.5",
          refY: "5",
          markerWidth: "6",
          markerHeight: "6",
          orient: "auto-start-reverse",
        },
        [svgEl("path", { d: "M 0 1 L 10 5 L 0 9 z", fill: color })],
      );

    svg.appendChild(
      svgEl("defs", {}, [
        makeMarker("fc-arrow-default", "#64748b"),
        makeMarker("fc-arrow-sky", "#38bdf8"),
        makeMarker("fc-arrow-indigo", "#818cf8"),
        makeMarker("fc-arrow-violet", "#a78bfa"),
        makeMarker("fc-arrow-emerald", "#34d399"),
        makeMarker("fc-arrow-amber", "#fbbf24"),
        makeMarker("fc-arrow-rose", "#fb7185"),
      ]),
    );

    for (const lane of def.lanes) {
      svg.appendChild(
        svgEl("rect", {
          x: String(lane.x),
          y: String(lane.y),
          width: String(lane.w),
          height: String(lane.h),
          rx: "12",
          fill: "rgba(15, 23, 42, 0.55)",
          stroke: lane.color,
          "stroke-opacity": "0.35",
          "stroke-width": "1.2",
        }),
      );
      svg.appendChild(
        svgEl(
          "text",
          {
            x: String(lane.x + 12),
            y: String(lane.y + 22),
            fill: lane.color,
            "font-size": "11.5",
            "font-weight": "700",
          },
          [lane.title],
        ),
      );
      svg.appendChild(
        svgEl(
          "text",
          {
            x: String(lane.x + 12),
            y: String(lane.y + 38),
            fill: "#94a3b8",
            "font-size": "10",
          },
          [lane.subtitle],
        ),
      );
    }

    for (const edge of def.edges) {
      const traceOn = edgeMatchesTrace(edge);
      const isFocused = activeNodeId && (edge.from === activeNodeId || edge.to === activeNodeId);
      const dimmed = !traceOn || (activeNodeId && !isFocused);

      let stroke = edge.color || "#64748b";
      let markerId = "fc-arrow-default";
      if (isFocused) {
        stroke = edge.to === activeNodeId ? "#34d399" : "#38bdf8";
        markerId = edge.to === activeNodeId ? "fc-arrow-emerald" : "fc-arrow-sky";
      } else if (edge.color === "#38bdf8") markerId = "fc-arrow-sky";
      else if (edge.color === "#818cf8") markerId = "fc-arrow-indigo";
      else if (edge.color === "#a78bfa") markerId = "fc-arrow-violet";
      else if (edge.color === "#34d399") markerId = "fc-arrow-emerald";
      else if (edge.color === "#fbbf24") markerId = "fc-arrow-amber";
      else if (edge.color === "#fb7185") markerId = "fc-arrow-rose";

      const edgeGroup = svgEl("g", { class: "fc-edge" + (dimmed ? " fc-dimmed" : "") });
      const pathAttrs = {
        d: edge.d,
        fill: "none",
        stroke,
        "stroke-width": isFocused ? "2.4" : "1.55",
        "marker-end": `url(#${markerId})`,
      };
      if (edge.dashed) pathAttrs["stroke-dasharray"] = "5 4";
      edgeGroup.appendChild(svgEl("path", pathAttrs));

      if (edge.label) {
        const labelW = Math.max(36, edge.label.length * 5.5 + 10);
        edgeGroup.appendChild(
          svgEl("rect", {
            x: String(edge.lx - labelW / 2),
            y: String(edge.ly - 9),
            width: String(labelW),
            height: "16",
            rx: "4",
            fill: "#0b1120",
            "fill-opacity": "0.9",
            stroke: isFocused ? stroke : "#1e293b",
            "stroke-width": "0.8",
          }),
        );
        edgeGroup.appendChild(
          svgEl(
            "text",
            {
              x: String(edge.lx),
              y: String(edge.ly + 2.5),
              "text-anchor": "middle",
              fill: isFocused ? "#f8fafc" : "#cbd5e1",
              "font-size": "9.3",
              "font-weight": isFocused ? "700" : "500",
            },
            [edge.label],
          ),
        );
      }

      svg.appendChild(edgeGroup);
    }

    for (const node of def.nodes) {
      const traceOn = nodeMatchesTrace(node);
      const dimmed = !traceOn || (activeNodeId && !connectedToActive.has(node.id));
      const isSelected = activeNodeId === node.id || (node.pieceId && state.selectedPieceId === node.pieceId);

      const gAttrs = {
        class: "fc-node" + (isSelected ? " active" : "") + (dimmed ? " fc-dimmed" : ""),
        role: "button",
        tabindex: "0",
        "aria-label": "Inspect " + node.title,
      };
      if (node.pieceId) gAttrs["data-piece-id"] = node.pieceId;

      const g = svgEl("g", gAttrs);

      if (node.kind === "decision") {
        const pts = [
          `${node.cx},${node.cy - node.ry}`,
          `${node.cx + node.rx},${node.cy}`,
          `${node.cx},${node.cy + node.ry}`,
          `${node.cx - node.rx},${node.cy}`,
        ].join(" ");
        g.appendChild(
          svgEl("polygon", {
            points: pts,
            fill: "#1c1f2e",
            stroke: isSelected ? "#f8fafc" : node.color,
            "stroke-width": isSelected ? "2.3" : "1.5",
          }),
        );
        g.appendChild(
          svgEl(
            "text",
            {
              x: String(node.cx),
              y: String(node.cy - 2),
              "text-anchor": "middle",
              fill: "#fef3c7",
              "font-size": "10.8",
              "font-weight": "700",
            },
            [node.title],
          ),
        );
        g.appendChild(
          svgEl(
            "text",
            {
              x: String(node.cx),
              y: String(node.cy + 12),
              "text-anchor": "middle",
              fill: "#cbd5e1",
              "font-size": "9.3",
            },
            [node.sub],
          ),
        );
      } else {
        const isArtifact = node.kind === "artifact";
        const rectAttrs = {
          x: String(node.x),
          y: String(node.y),
          width: String(node.w),
          height: String(node.h),
          rx: isArtifact ? "14" : "9",
          fill: isArtifact ? "#0f1c2e" : "#162033",
          stroke: isSelected ? "#f8fafc" : node.color,
          "stroke-width": isSelected ? "2.3" : "1.3",
        };
        if (isArtifact) rectAttrs["stroke-dasharray"] = "4 2";
        g.appendChild(svgEl("rect", rectAttrs));

        if (!isArtifact) {
          g.appendChild(
            svgEl("rect", {
              x: String(node.x + 1),
              y: String(node.y + 8),
              width: "3.5",
              height: String(node.h - 16),
              rx: "2",
              fill: node.color,
            }),
          );
        }

        const titleY = node.h >= 70 ? node.y + 23 : node.y + 21;
        const maxChars = Math.floor((node.w - 20) / 6.2);
        const titleLines = wrapSvgLabel(node.title, maxChars);
        titleLines.forEach((line, idx) => {
          g.appendChild(
            svgEl(
              "text",
              {
                x: String(node.x + 12),
                y: String(titleY + idx * 14),
                fill: "#f8fafc",
                "font-size": isArtifact ? "10.8" : "11.2",
                "font-weight": "700",
              },
              [line],
            ),
          );
        });

        const subY = titleY + titleLines.length * 14 + 3;
        g.appendChild(
          svgEl(
            "text",
            {
              x: String(node.x + 12),
              y: String(Math.min(subY, node.y + node.h - 9)),
              fill: "#94a3b8",
              "font-size": "9.5",
            },
            [node.sub],
          ),
        );
      }

      const onActivate = () => {
        state.inspectedNodeId = state.inspectedNodeId === node.id ? null : node.id;
        if (node.pieceId) {
          state.selectedPieceId = node.pieceId;
          renderPieces();
        }
        renderPipeline();
      };

      g.addEventListener("click", onActivate);
      g.addEventListener("keydown", (e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onActivate();
        }
      });

      svg.appendChild(g);
    }

    container.appendChild(svg);
    renderFlowchartInspector(def);
  }

  function renderPieceDataflow(container, catalog) {
    const stages = [
      { name: "Marketplace Generation", color: "#38bdf8", colX: 30 },
      { name: "Ad Serving & Simulation", color: "#818cf8", colX: 405 },
      { name: "Analysis & Visualization", color: "#34d399", colX: 780 },
    ];

    const boxW = 310;
    const boxH = 58;
    const rowGap = 76;
    const topPad = 48;

    const coords = new Map();
    const stageCounts = new Map();
    for (const piece of catalog.pieces) {
      const stageIdx = Math.max(0, stages.findIndex((s) => s.name === piece.stage));
      const rowIdx = stageCounts.get(stageIdx) || 0;
      stageCounts.set(stageIdx, rowIdx + 1);
      const x = stages[stageIdx].colX;
      const y = topPad + rowIdx * rowGap;
      coords.set(piece.id, { x, y, cx: x + boxW / 2, cy: y + boxH / 2, stage: stages[stageIdx] });
    }

    const maxRows = Math.max(1, ...Array.from(stageCounts.values()));
    const svgHeight = topPad + maxRows * rowGap + 24;
    const svg = svgEl("svg", {
      class: "pipeline-svg",
      viewBox: "0 0 1120 " + svgHeight,
      role: "img",
      "aria-label": "11-piece simulator architecture dataflow pipeline",
    });

    svg.appendChild(
      svgEl("defs", {}, [
        svgEl(
          "marker",
          {
            id: "arrow-head",
            viewBox: "0 0 10 10",
            refX: "8",
            refY: "5",
            markerWidth: "6",
            markerHeight: "6",
            orient: "auto-start-reverse",
          },
          [svgEl("path", { d: "M 0 1 L 10 5 L 0 9 z", fill: "#64748b" })],
        ),
      ]),
    );

    for (const st of stages) {
      svg.appendChild(
        svgEl(
          "text",
          {
            x: String(st.colX),
            y: "26",
            fill: st.color,
            "font-size": "13",
            "font-weight": "700",
            "letter-spacing": "0.04em",
          },
          [st.name.toUpperCase()],
        ),
      );
    }

    for (const edge of catalog.pipeline_edges) {
      const a = coords.get(edge.from);
      const b = coords.get(edge.to);
      if (!a || !b) continue;
      let pathD;
      if (a.x === b.x) {
        const x = a.x + boxW - 14;
        const y1 = a.y + boxH;
        const y2 = b.y;
        pathD = `M ${x} ${y1} C ${x + 26} ${(y1 + y2) / 2}, ${x + 26} ${(y1 + y2) / 2}, ${x} ${y2}`;
      } else {
        const x1 = a.x + boxW;
        const y1 = a.cy;
        const x2 = b.x;
        const y2 = b.cy;
        const mx = (x1 + x2) / 2;
        pathD = `M ${x1} ${y1} C ${mx} ${y1}, ${mx} ${y2}, ${x2} ${y2}`;
      }
      svg.appendChild(
        svgEl("path", {
          d: pathD,
          fill: "none",
          stroke: "#475569",
          "stroke-width": "1.8",
          "marker-end": "url(#arrow-head)",
        }),
      );
    }

    for (const piece of catalog.pieces) {
      const pos = coords.get(piece.id);
      if (!pos) continue;
      const g = svgEl("g", {
        class: "pipeline-node" + (state.selectedPieceId === piece.id ? " active" : ""),
        "data-piece-id": piece.id,
        role: "button",
        tabindex: "0",
        "aria-label": "Inspect " + piece.name,
      });

      g.appendChild(
        svgEl("rect", {
          x: String(pos.x),
          y: String(pos.y),
          width: String(boxW),
          height: String(boxH),
          rx: "10",
          fill: "#162033",
          stroke: state.selectedPieceId === piece.id ? "#38bdf8" : pos.stage.color,
          "stroke-width": state.selectedPieceId === piece.id ? "2.4" : "1.2",
        }),
      );
      g.appendChild(
        svgEl(
          "text",
          {
            x: String(pos.x + 14),
            y: String(pos.y + 24),
            fill: "#f8fafc",
            "font-size": "13",
            "font-weight": "700",
          },
          [piece.name],
        ),
      );
      g.appendChild(
        svgEl(
          "text",
          {
            x: String(pos.x + 14),
            y: String(pos.y + 44),
            fill: "#94a3b8",
            "font-size": "11",
          },
          [piece.crate_name + " · " + piece.features.length + " features · " + piece.gaps_and_simplifications.length + " gaps"],
        ),
      );

      const onActivate = () => {
        state.selectedPieceId = piece.id;
        state.inspectedNodeId = piece.id;
        renderPipeline();
        renderPieces();
        const card = document.querySelector('[data-piece-card="' + piece.id + '"]');
        if (card) card.scrollIntoView({ behavior: "smooth", block: "nearest" });
      };
      g.addEventListener("click", onActivate);
      g.addEventListener("keydown", (e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onActivate();
        }
      });

      svg.appendChild(g);
    }

    container.appendChild(svg);
    renderFlowchartInspector(getFlowchartDefinition(catalog));
  }

  function renderPipeline() {
    const catalog = state.catalog;
    const container = document.getElementById("pipeline-container");
    clearNode(container);
    if (!catalog) return;

    renderFlowTracePills();
    if (state.diagramMode === "pipeline") {
      renderPieceDataflow(container, catalog);
    } else {
      renderInteractionFlowchart(container, catalog);
    }
  }

  function renderStageFilters() {
    const catalog = state.catalog;
    const container = document.getElementById("stage-filters");
    clearNode(container);
    if (!catalog) return;

    const stages = ["All", ...Array.from(new Set(catalog.pieces.map((p) => p.stage)))];
    for (const stage of stages) {
      const btn = el("button", {
        type: "button",
        className: "pill-btn" + (state.activeStage === stage ? " active" : ""),
        textContent: stage,
      });
      btn.addEventListener("click", () => {
        state.activeStage = stage;
        renderStageFilters();
        renderPieces();
      });
      container.appendChild(btn);
    }
  }

  function pieceMatchesFilter(piece) {
    if (state.activeStage !== "All" && piece.stage !== state.activeStage) {
      return false;
    }
    const q = state.searchQuery.trim().toLowerCase();
    if (!q) return true;
    const haystack = [
      piece.id,
      piece.name,
      piece.crate_name,
      piece.stage,
      piece.summary,
      ...(piece.source_files || []),
      ...(piece.parameters || []).map((p) => `${p.key} ${p.label} ${p.value} ${p.unit}`),
      ...(piece.features || []).map((f) => `${f.name} ${f.description}`),
      ...(piece.gaps_and_simplifications || []),
    ]
      .join(" ")
      .toLowerCase();
    return haystack.includes(q);
  }

  function renderPieces() {
    const catalog = state.catalog;
    const grid = document.getElementById("pieces-grid");
    const empty = document.getElementById("pieces-empty");
    const countDesc = document.getElementById("pieces-count-desc");
    clearNode(grid);
    if (!catalog) return;

    const matching = catalog.pieces.filter(pieceMatchesFilter);
    countDesc.textContent = `Showing ${matching.length} of ${catalog.pieces.length} simulator pieces`;

    if (matching.length === 0) {
      empty.classList.remove("hidden");
      return;
    }
    empty.classList.add("hidden");

    for (const piece of matching) {
      const card = el("article", {
        className: "piece-card" + (state.selectedPieceId === piece.id ? " highlighted" : ""),
        "data-piece-card": piece.id,
        "data-piece-id": piece.id,
      });

      const header = el("div", { className: "piece-card-header" }, [
        el("h3", { className: "piece-title", textContent: piece.name }),
        el("div", { className: "piece-meta" }, [
          el("span", { className: "badge", textContent: piece.crate_name }),
          el("span", { className: "badge badge-indigo", textContent: piece.stage }),
        ]),
      ]);
      card.appendChild(header);

      card.appendChild(el("p", { className: "piece-summary", textContent: piece.summary }));

      const filesRow = el(
        "div",
        { className: "chip-list" },
        piece.source_files.map((sf) => el("span", { className: "file-chip", textContent: sf })),
      );
      card.appendChild(filesRow);

      const paramGrid = el("div", { className: "param-list" });
      for (const p of piece.parameters) {
        paramGrid.appendChild(
          el("div", { className: "param-item" }, [
            el("div", { className: "param-label", textContent: p.label }),
            el("div", { className: "param-val", textContent: String(p.value) }),
          ]),
        );
      }
      card.appendChild(paramGrid);

      const featSection = el("div", {}, [
        el("div", { className: "sub-heading", textContent: `Implemented Features (${piece.features.length})` }),
        el(
          "ul",
          { className: "feature-list" },
          piece.features.map((f) =>
            el("li", {}, [
              el("strong", { textContent: f.name + ": " }),
              document.createTextNode(f.description),
            ]),
          ),
        ),
      ]);
      card.appendChild(featSection);

      if (piece.gaps_and_simplifications && piece.gaps_and_simplifications.length > 0) {
        const gapSection = el("div", {}, [
          el("div", {
            className: "sub-heading",
            textContent: `Current Simplifications & Gaps (${piece.gaps_and_simplifications.length})`,
          }),
          el(
            "ul",
            { className: "gap-list" },
            piece.gaps_and_simplifications.map((g) => el("li", { textContent: g })),
          ),
        ]);
        card.appendChild(gapSection);
      }

      grid.appendChild(card);
    }
  }

  function syncRunnerFormFromTimeseries(ts) {
    if (!ts || !ts.config) return;
    const mpSeed = document.getElementById("runner-marketplace-seed");
    const simSeed = document.getElementById("runner-seed");
    const budget = document.getElementById("runner-budget");
    const cpc = document.getElementById("runner-cpc");
    const impr = document.getElementById("runner-impressions");
    const ranker = document.getElementById("runner-ranker");
    const slateSize = document.getElementById("runner-slate-size");
    const nat = document.getElementById("runner-natural");

    if (mpSeed && ts.config.marketplace_seed !== null && ts.config.marketplace_seed !== undefined) {
      mpSeed.value = String(ts.config.marketplace_seed);
    }
    if (simSeed) simSeed.value = String(ts.config.sim_seed);
    if (budget) {
      budget.value = ts.config.campaign_budget_cents === null || ts.config.campaign_budget_cents === undefined
        ? ""
        : String(ts.config.campaign_budget_cents);
    }
    if (cpc) cpc.value = String(ts.config.cpc_cents);
    if (ranker && ts.config.ranker) ranker.value = String(ts.config.ranker);
    if (slateSize && ts.config.slate_size) slateSize.value = String(ts.config.slate_size);
    if (ts.config.impressions === null || ts.config.impressions === undefined) {
      if (nat) nat.checked = true;
      if (impr) {
        impr.value = "";
        impr.disabled = true;
      }
    } else {
      if (nat) nat.checked = false;
      if (impr) {
        impr.disabled = false;
        impr.value = String(ts.config.impressions);
      }
    }
  }

  function renderBudgetSection() {
    const ts = state.timeseries;
    if (!ts) return;

    const meta = document.getElementById("sim-run-meta");
    const modeLabel = ts.config.impressions === null || ts.config.impressions === undefined
      ? "Natural user-independent requests (600 req/day)"
      : `Target ${fmtInt(ts.config.impressions)} impressions`;
    const budgetLabel = ts.config.campaign_budget_cents === null || ts.config.campaign_budget_cents === undefined
      ? `${fmtUsdCents(ts.config.campaign_budget_min_cents ?? 100000)}–${fmtUsdCents(ts.config.campaign_budget_max_cents ?? 1000000)}/campaign (random)`
      : `${fmtUsdCents(ts.config.campaign_budget_cents)}/campaign`;
    meta.textContent =
      `Seed ${ts.config.sim_seed} · Ranker ${ts.config.ranker || "R1"} (Slate ${ts.config.slate_size || 20}) · ` +
      `Budget ${budgetLabel} · CPC ${fmtUsdCents(ts.config.cpc_cents)} · ${modeLabel}`;

    const kpis = document.getElementById("budget-kpis");
    clearNode(kpis);
    const s = ts.summary;
    const activeEnd = ts.days.length > 0 ? ts.days[ts.days.length - 1].active_campaigns_end_of_day : 0;

    const cards = [
      { label: "Total Budget Spent", value: fmtUsdCents(s.total_spend_cents), sub: `of ${fmtUsdCents(s.total_budget_cents)} (${fmtPct(s.utilization_pct)})` },
      { label: "Remaining Budget", value: fmtUsdCents(s.remaining_budget_cents), sub: `${fmtInt(s.exhausted_campaigns)} exhausted / ${fmtInt(activeEnd)} active` },
      { label: "Daily Spend (Mean)", value: fmtUsdCents(s.mean_daily_spend_cents), sub: `min ${fmtUsdCents(s.min_daily_spend_cents)} · max ${fmtUsdCents(s.max_daily_spend_cents)}` },
      { label: "Impressions & Clicks", value: fmtInt(s.total_clicks) + " clicks", sub: `${fmtInt(s.total_impressions)} impr (${fmtPct(s.ctr_pct, 3)} CTR)` },
    ];
    for (const c of cards) {
      kpis.appendChild(
        el("div", { className: "kpi-card" }, [
          el("div", { className: "kpi-label", textContent: c.label }),
          el("div", { className: "kpi-value", textContent: c.value }),
          el("div", { className: "kpi-sub", textContent: c.sub }),
        ]),
      );
    }

    const plotImg = document.getElementById("budget-svg-img");
    if (plotImg && state.svg) {
      plotImg.src = "data:image/svg+xml;charset=utf-8," + encodeURIComponent(state.svg);
    }

    const tbody = document.getElementById("daily-table-body");
    clearNode(tbody);
    for (const d of ts.days) {
      tbody.appendChild(
        el("tr", {}, [
          el("td", { textContent: "Day " + d.day }),
          el("td", { textContent: fmtInt(d.impressions) }),
          el("td", { textContent: fmtInt(d.clicks) }),
          el("td", { textContent: fmtUsdCents(d.daily_spend_cents) }),
          el("td", { textContent: fmtUsdCents(d.cumulative_spend_cents) }),
          el("td", { textContent: fmtUsdCents(d.remaining_budget_cents) }),
          el("td", { textContent: fmtPct(d.utilization_pct, 2) }),
          el("td", { textContent: fmtInt(d.active_campaigns_end_of_day) }),
          el("td", { textContent: fmtInt(d.insufficient_budget) }),
        ]),
      );
    }
  }

  function renderAnalysisSection() {
    const section = document.getElementById("analysis-section");
    const kpis = document.getElementById("analysis-kpis");
    const findingsBox = document.getElementById("analysis-findings");
    const tbody = document.getElementById("analysis-top-campaigns-body");
    const meta = document.getElementById("analysis-meta");
    if (!section || !kpis || !findingsBox || !tbody) return;

    const a = state.analysis;
    if (!a) {
      section.classList.add("hidden");
      return;
    }
    section.classList.remove("hidden");

    const dep = a.budget_depletion;
    const ctr = a.ctr_performance;
    const dist = a.campaign_distribution;

    if (meta) {
      meta.textContent = `Ranker ${a.ranker} (Slate ${a.slate_size}) · ${a.days_analyzed} days analyzed · ${fmtInt(ctr.total_ad_requests)} Ad-Server requests (${ctr.avg_ads_per_request.toFixed(2)} avg ads/req)`;
    }

    clearNode(kpis);
    const depletionText = dep.full_exhaustion_day
      ? `Full Day ${dep.full_exhaustion_day}`
      : dep.first_exhaustion_day
        ? `1st Day ${dep.first_exhaustion_day}`
        : dep.projected_days_to_full_depletion
          ? `~${Math.round(dep.projected_days_to_full_depletion)}d projected`
          : "No spend";

    const analysisCards = [
      {
        label: "Budget Depletion",
        value: fmtPct(dep.utilization_pct, 2),
        sub: `${fmtInt(dep.exhausted_campaigns)}/${fmtInt(dep.total_campaigns)} exhausted · ${depletionText}`,
      },
      {
        label: "Average CTR",
        value: fmtPct(ctr.overall_ctr_pct, 3),
        sub: `${ctr.ctr_vs_random_baseline_ratio.toFixed(2)}x random (0.20%) · max 4.00%`,
      },
      {
        label: "Daily CTR & eCPM",
        value: fmtUsdCents(ctr.effective_cpm_cents) + " eCPM",
        sub: `Daily CTR mean ${fmtPct(ctr.daily_ctr_pct.mean, 3)} (std ${fmtPct(ctr.daily_ctr_pct.std_dev, 3)})`,
      },
      {
        label: "Pacing Stability (CV)",
        value: fmtPct(dep.daily_burn_cv_pct, 1),
        sub: `Median burn ${fmtUsdCents(dep.daily_burn_cents.median)}/day · unfilled ${fmtPct(dep.unfilled_rate_pct, 2)}`,
      },
      {
        label: "Spend Concentration",
        value: "Gini " + dist.gini_spend_coefficient.toFixed(3),
        sub: `Top 10% share ${fmtPct(dist.top_10_pct_spend_share_pct, 1)} · ${fmtInt(dist.zero_click_campaigns)} zero-click`,
      },
      {
        label: "Ad-Server Fill",
        value: ctr.avg_ads_per_request.toFixed(2) + " ads/req",
        sub: `${fmtInt(ctr.total_ad_requests)} serve_ads() calls`,
      },
    ];

    for (const c of analysisCards) {
      kpis.appendChild(
        el("div", { className: "kpi-card" }, [
          el("div", { className: "kpi-label", textContent: c.label }),
          el("div", { className: "kpi-value", textContent: c.value }),
          el("div", { className: "kpi-sub", textContent: c.sub }),
        ]),
      );
    }

    clearNode(findingsBox);
    for (const f of a.findings || []) {
      const sevClass = f.severity === "critical"
        ? " finding-critical"
        : f.severity === "warning"
          ? " finding-warning"
          : "";
      findingsBox.appendChild(
        el("div", { className: "finding-card" + sevClass }, [
          el("div", { className: "finding-title", textContent: `[${String(f.severity || "info").toUpperCase()}] ${f.title}` }),
          el("p", { className: "finding-detail", textContent: f.detail }),
        ]),
      );
    }

    clearNode(tbody);
    for (const row of dist.top_campaigns_by_spend || []) {
      tbody.appendChild(
        el("tr", {}, [
          el("td", { textContent: row.campaign_id }),
          el("td", { textContent: row.advertiser_id }),
          el("td", { textContent: row.category_id }),
          el("td", { textContent: fmtInt(row.impressions) }),
          el("td", { textContent: fmtInt(row.clicks) }),
          el("td", { textContent: fmtPct(row.ctr_pct, 3) }),
          el("td", { textContent: fmtUsdCents(row.spend_cents) }),
          el("td", { textContent: fmtPct(row.utilization_pct, 2) }),
        ]),
      );
    }
  }

  function renderAdServerSection() {
    const sample = state.adServerSample;
    const summaryEl = document.getElementById("ad-server-summary");
    const tbody = document.getElementById("ad-server-slate-body");
    if (!tbody || !sample) return;

    if (summaryEl) {
      summaryEl.textContent =
        `Returned ${sample.ads.length} distinct funded campaign ads for user_id="${sample.user_id}" using Ranker Strategy ${sample.strategy}.`;
    }
    clearNode(tbody);
    for (const ad of sample.ads || []) {
      tbody.appendChild(
        el("tr", {}, [
          el("td", { textContent: "#" + ad.slot_position }),
          el("td", { textContent: ad.campaign_id }),
          el("td", { textContent: ad.advertiser_id }),
          el("td", { textContent: ad.ad_item_id }),
          el("td", { textContent: ad.category_id }),
          el("td", { textContent: fmtUsdCents(ad.cpc_cents) }),
        ]),
      );
    }
  }

  function renderMarketplaceSection() {
    const mp = state.marketplace;
    if (!mp) return;

    const statsRow = document.getElementById("marketplace-stats");
    clearNode(statsRow);

    const statCards = [
      { label: "Seed", value: String(mp.seed), sub: `${fmtInt(mp.counts.categories)} categories` },
      { label: "Advertisers & Catalog", value: fmtInt(mp.counts.advertisers), sub: `${fmtInt(mp.counts.catalog_items)} catalog items` },
      { label: "Shoppers & Organic", value: fmtInt(mp.counts.users) + " users", sub: `${fmtInt(mp.counts.organic_items)} organic items` },
      { label: "Browsing Events", value: fmtInt(mp.counts.browsing_events), sub: `${fmtInt(mp.browsing_split.affinity)} affinity / ${fmtInt(mp.browsing_split.exploration)} exploration` },
      {
        label: "Catalog Price Stats",
        value: fmtUsdCents(mp.price_stats.catalog.mean_cents),
        sub: `${fmtUsdCents(mp.price_stats.catalog.min_cents)} - ${fmtUsdCents(mp.price_stats.catalog.max_cents)}`,
      },
      {
        label: "Organic Price Stats",
        value: fmtUsdCents(mp.price_stats.organic.mean_cents),
        sub: `${fmtUsdCents(mp.price_stats.organic.min_cents)} - ${fmtUsdCents(mp.price_stats.organic.max_cents)}`,
      },
    ];

    for (const c of statCards) {
      statsRow.appendChild(
        el("div", { className: "kpi-card" }, [
          el("div", { className: "kpi-label", textContent: c.label }),
          el("div", { className: "kpi-value", textContent: c.value }),
          el("div", { className: "kpi-sub", textContent: c.sub }),
        ]),
      );
    }

    const taxGrid = document.getElementById("taxonomy-grid");
    clearNode(taxGrid);
    for (const grp of mp.taxonomy || []) {
      const tags = el(
        "div",
        { className: "taxonomy-tags" },
        (grp.categories || []).map((cat) =>
          el("span", { className: "taxonomy-tag", textContent: typeof cat === "string" ? cat : cat.name }),
        ),
      );
      taxGrid.appendChild(
        el("div", { className: "taxonomy-card" }, [
          el("h3", { textContent: grp.group }),
          tags,
        ]),
      );
    }
  }

  function showRunnerError(msg) {
    const box = document.getElementById("runner-error");
    if (!box) return;
    if (!msg) {
      box.textContent = "";
      box.classList.add("hidden");
    } else {
      box.textContent = "Error: " + msg;
      box.classList.remove("hidden");
    }
  }

  function showMarketplaceError(msg) {
    const box = document.getElementById("marketplace-error");
    if (!box) return;
    if (!msg) {
      box.textContent = "";
      box.classList.add("hidden");
    } else {
      box.textContent = "Error: " + msg;
      box.classList.remove("hidden");
    }
  }

  function showAdServerError(msg) {
    const box = document.getElementById("ad-server-error");
    if (!box) return;
    if (!msg) {
      box.textContent = "";
      box.classList.add("hidden");
    } else {
      box.textContent = "Error: " + msg;
      box.classList.remove("hidden");
    }
  }

  function wireControls() {
    const viewFlowchartBtn = document.getElementById("view-flowchart-btn");
    const viewPipelineBtn = document.getElementById("view-pipeline-btn");
    const setDiagramMode = (mode) => {
      state.diagramMode = mode;
      if (viewFlowchartBtn) {
        viewFlowchartBtn.classList.toggle("active", mode === "flowchart");
        viewFlowchartBtn.setAttribute("aria-selected", String(mode === "flowchart"));
      }
      if (viewPipelineBtn) {
        viewPipelineBtn.classList.toggle("active", mode === "pipeline");
        viewPipelineBtn.setAttribute("aria-selected", String(mode === "pipeline"));
      }
      renderPipeline();
    };
    if (viewFlowchartBtn) {
      viewFlowchartBtn.addEventListener("click", () => setDiagramMode("flowchart"));
    }
    if (viewPipelineBtn) {
      viewPipelineBtn.addEventListener("click", () => setDiagramMode("pipeline"));
    }

    const searchInput = document.getElementById("piece-search");
    const clearBtn = document.getElementById("clear-filters-btn");
    const resetEmptyBtn = document.getElementById("reset-empty-btn");

    const resetFilters = () => {
      state.searchQuery = "";
      state.activeStage = "All";
      state.selectedPieceId = null;
      state.inspectedNodeId = null;
      state.activeFlow = "all";
      if (searchInput) searchInput.value = "";
      renderStageFilters();
      renderPipeline();
      renderPieces();
    };

    if (searchInput) {
      searchInput.addEventListener("input", (e) => {
        state.searchQuery = e.target.value || "";
        renderPieces();
      });
    }
    if (clearBtn) clearBtn.addEventListener("click", resetFilters);
    if (resetEmptyBtn) resetEmptyBtn.addEventListener("click", resetFilters);

    const naturalToggle = document.getElementById("runner-natural");
    const imprInput = document.getElementById("runner-impressions");
    if (naturalToggle && imprInput) {
      naturalToggle.addEventListener("change", () => {
        imprInput.disabled = naturalToggle.checked;
        if (!naturalToggle.checked && !imprInput.value) {
          imprInput.value = "1000000";
        }
      });
    }

    const runnerForm = document.getElementById("sim-runner-form");
    const submitBtn = document.getElementById("runner-submit");
    if (runnerForm) {
      runnerForm.addEventListener("submit", async (e) => {
        e.preventDefault();
        if (state.staticMode) {
          showRunnerError("Static offline bundle cannot execute new simulations. Run the dashboard server for live runs.");
          return;
        }
        showRunnerError("");

        const mpSeedRaw = (document.getElementById("runner-marketplace-seed").value || "").trim();
        const seedRaw = (document.getElementById("runner-seed").value || "").trim();
        const budgetRaw = (document.getElementById("runner-budget").value || "").trim();
        const cpcRaw = (document.getElementById("runner-cpc").value || "").trim();
        const imprRaw = (document.getElementById("runner-impressions").value || "").trim();
        const rankerEl = document.getElementById("runner-ranker");
        const slateEl = document.getElementById("runner-slate-size");
        const natural = Boolean(document.getElementById("runner-natural").checked);

        const parseStrictInt = (raw) => {
          if (!/^-?\d+$/.test(raw)) return null;
          const n = Number(raw);
          return Number.isSafeInteger(n) ? n : null;
        };

        const payload = {};
        if (mpSeedRaw !== "") {
          const v = parseStrictInt(mpSeedRaw);
          if (v === null) {
            showRunnerError("Marketplace seed must be an integer.");
            return;
          }
          payload.marketplace_seed = v;
        }
        if (seedRaw !== "") {
          const v = parseStrictInt(seedRaw);
          if (v === null || v < 0) {
            showRunnerError("Simulation seed must be a non-negative integer.");
            return;
          }
          payload.seed = v;
        }
        if (budgetRaw !== "") {
          const v = parseStrictInt(budgetRaw);
          if (v === null || v < 0) {
            showRunnerError("Campaign budget must be a non-negative integer in cents.");
            return;
          }
          payload.campaign_budget_cents = v;
        }
        if (cpcRaw !== "") {
          const v = parseStrictInt(cpcRaw);
          if (v === null || v <= 0) {
            showRunnerError("CPC must be a positive integer greater than 0 cents.");
            return;
          }
          payload.cpc_cents = v;
        }
        if (rankerEl && rankerEl.value) {
          payload.ranker = rankerEl.value;
        }
        if (slateEl && slateEl.value.trim() !== "") {
          const v = parseStrictInt(slateEl.value.trim());
          if (v === null || v <= 0) {
            showRunnerError("Slate size must be a positive integer.");
            return;
          }
          payload.slate_size = v;
        }
        if (natural || imprRaw === "") {
          payload.impressions = null;
        } else {
          const v = parseStrictInt(imprRaw);
          if (v === null || v < 0) {
            showRunnerError("Target impressions must be a non-negative integer.");
            return;
          }
          payload.impressions = v;
        }

        if (submitBtn) {
          submitBtn.disabled = true;
          submitBtn.textContent = "Running...";
        }
        try {
          const res = await fetch("/api/sim-agent/run", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify(payload),
          });
          const data = await res.json().catch(() => ({ error: "Invalid server response" }));
          if (!res.ok) {
            showRunnerError(data.error || `Simulation failed with HTTP ${res.status}`);
            return;
          }
          state.timeseries = data.timeseries;
          state.analysis = data.analysis || null;
          state.svg = data.svg;
          renderHeroAndKpis();
          renderBudgetSection();
          renderAnalysisSection();
        } catch (err) {
          showRunnerError(err && err.message ? err.message : "Failed to run simulation.");
        } finally {
          if (submitBtn) {
            submitBtn.disabled = false;
            submitBtn.textContent = "Run Simulation";
          }
        }
      });
    }

    const adServerForm = document.getElementById("ad-server-form");
    if (adServerForm) {
      adServerForm.addEventListener("submit", async (e) => {
        e.preventDefault();
        if (state.staticMode) {
          showAdServerError("Live Ad-Server sampling requires the dashboard server.");
          return;
        }
        showAdServerError("");
        const userId = (document.getElementById("ad-server-user-id").value || "").trim();
        const seed = (document.getElementById("ad-server-seed").value || "42").trim();
        try {
          const res = await fetch(
            "/api/ad-server/sample?user_id=" + encodeURIComponent(userId) + "&seed=" + encodeURIComponent(seed),
          );
          const data = await res.json().catch(() => ({ error: "Invalid server response" }));
          if (!res.ok) {
            showAdServerError(data.error || `Ad-Server request failed (${res.status})`);
            return;
          }
          state.adServerSample = data;
          renderAdServerSection();
        } catch (err) {
          showAdServerError(err && err.message ? err.message : "Failed to query Ad-Server.");
        }
      });
    }

    const mpForm = document.getElementById("marketplace-seed-form");
    if (mpForm) {
      mpForm.addEventListener("submit", async (e) => {
        e.preventDefault();
        if (state.staticMode) {
          showMarketplaceError("Live marketplace seed inspection requires the dashboard server.");
          return;
        }
        showMarketplaceError("");
        const raw = (document.getElementById("marketplace-inspect-seed").value || "").trim();
        try {
          const res = await fetch("/api/marketplace?seed=" + encodeURIComponent(raw));
          const data = await res.json().catch(() => ({ error: "Invalid server response" }));
          if (!res.ok) {
            showMarketplaceError(data.error || `Marketplace request failed (${res.status})`);
            return;
          }
          state.marketplace = data;
          renderMarketplaceSection();
        } catch (err) {
          showMarketplaceError(err && err.message ? err.message : "Failed to inspect marketplace.");
        }
      });
    }
  }

  async function init() {
    wireControls();
    const bootstrap = readBootstrap();
    if (bootstrap && bootstrap.catalog) {
      state.staticMode = Boolean(bootstrap.staticMode);
      state.catalog = bootstrap.catalog;
      state.marketplace = bootstrap.marketplace;
      state.timeseries = bootstrap.timeseries;
      state.analysis = bootstrap.analysis || null;
      state.adServerSample = bootstrap.ad_server_sample || null;
      state.svg = bootstrap.svg;
      if (state.staticMode) {
        const note = document.getElementById("static-runner-note");
        if (note) note.classList.remove("hidden");
      }
      renderAll();
      syncRunnerFormFromTimeseries(state.timeseries);
      return;
    }

    try {
      const [catRes, mpRes, latestRes, plotRes, analysisRes, adSampleRes] = await Promise.all([
        fetch("/api/catalog"),
        fetch("/api/marketplace?seed=42"),
        fetch("/api/sim-agent/latest"),
        fetch("/api/artifacts/budget-plot"),
        fetch("/api/sim-agent/analysis"),
        fetch("/api/ad-server/sample"),
      ]);
      if (!catRes.ok || !mpRes.ok || !latestRes.ok || !plotRes.ok) {
        throw new Error("Failed to load dashboard API payloads");
      }
      const [catalog, marketplace, timeseries, svg] = await Promise.all([
        catRes.json(),
        mpRes.json(),
        latestRes.json(),
        plotRes.text(),
      ]);
      const analysis = analysisRes.ok ? await analysisRes.json() : null;
      const adServerSample = adSampleRes.ok ? await adSampleRes.json() : null;

      state.catalog = catalog;
      state.marketplace = marketplace;
      state.timeseries = timeseries;
      state.analysis = analysis;
      state.adServerSample = adServerSample;
      state.svg = svg;
      renderAll();
      syncRunnerFormFromTimeseries(state.timeseries);
    } catch (err) {
      showRunnerError(err && err.message ? err.message : "Unable to load dashboard data.");
    }
  }

  function renderAll() {
    renderHeroAndKpis();
    renderStageFilters();
    renderPipeline();
    renderPieces();
    renderBudgetSection();
    renderAnalysisSection();
    renderAdServerSection();
    renderMarketplaceSection();
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();
