# E2E Test Infrastructure (`TEST_INFRA.md`)

## 1. Test Philosophy

The `ads-simulator` End-to-End test suite in `e2e/` is an **independent, requirement-driven, opaque-box test suite** derived strictly from user requirements and the public CLI, HTTP API, artifact, and UI contracts in `PROJECT.md`.

Core principles:
- **Strict Opaque-Box Verification**: Tests interact exclusively through compiled workspace binaries (`ads-simulator`, `ads-marketplace-simulation`, `sim-agent`, `ads-simulator-dashboard`), HTTP/1.1 endpoints, generated on-disk artifacts (`marketplace.json`, `simulation.json`, `budget_timeseries.json`, `budget_timeseries.csv`, `budget_spent_30d.svg`, static export bundles), and real headless Chrome rendering via Chrome DevTools Protocol (CDP).
- **Zero Python & Zero External npm Packages**: Built purely on **Node.js 22 native built-ins** (`node:assert/strict`, `node:child_process`, `node:fs`, `node:http`, `node:os`, `node:path`, `node:url`, global `fetch`, global `WebSocket`) plus headless `/usr/bin/google-chrome`. No `.py` files, no `package.json`, no `node_modules/`.
- **Strict Anti-Skip / Loud Failure Policy**: There is **no `skip` primitive** anywhere in `e2e/registry.mjs` or `e2e/runner.mjs`. Missing binaries throw `BinaryNotFoundError` and fail loudly with exit code `1`. Zero-match test runs also exit `1`.
- **Deterministic Temp Isolation**: Every test receives its own isolated `tmpDir` under `os.tmpdir()`, automatically cleaned up in `finally` (or preserved for debugging with `E2E_KEEP_TMP=1`). Running the suite never mutates tracked workspace files or leaves orphan server/Chrome processes.
- **Semantic UI Testing**: Browser tests query rendered semantics (text content, headings, labels, `<form>`, `<button>`, `<svg>`, visible bounding boxes, network requests, zero console/page errors) rather than brittle private CSS class names.

---

## 2. Feature Inventory

Every user requirement and `PROJECT.md` feature (`F1`–`F20`) is mapped into **7 canonical feature areas**:

| # | Area Slug | Display Name | `PROJECT.md` Features | Primary Verification Targets |
|---|---|---|---|---|
| 1 | `python-removal-hygiene` | Legacy Python Removal & Repo Hygiene | `F1`, `F2`, `F3`, `F4`, `F5`, `F18` | Repo tree zero-`.py` scan, `.gitignore`, `docs/marketplace-data.md`, `README.md`, unified Cargo workspace & `Cargo.lock` |
| 2 | `marketplace-generator` | Marketplace Generator CLI & Determinism | `F6` | `ads-simulator` CLI, Schema v1 `marketplace.json`, 10×10 taxonomy, 9/3 browsing split, determinism, signed `i64` seeds |
| 3 | `cpc-simulator` | 30-Day CPC Simulator CLI & Ledger Accounting | `F7`, `F8` | `ads-marketplace-simulation` CLI, golden 1M-impression & natural replay ledgers, 500-campaign rollup accounting, nested `--output` dir creation |
| 4 | `sim-agent-plotting` | `sim-agent` 30-Day Execution & Budget Plotting | `F9`, `F10`, `F11`, `F12`, `F13` | `sim-agent` CLI, atomic 4-artifact export (`simulation.json`, `budget_timeseries.json`, `budget_timeseries.csv`, `budget_spent_30d.svg`), ledger audit, XML/SVG geometry |
| 5 | `dashboard-catalog-api` | Dashboard Feature Catalog API & Live Introspection | `F14`, `F15` | `ads-simulator-dashboard` HTTP server (`/health`, `/`, `/api/catalog` 9-piece graph & live constants, `/api/marketplace?seed=...`, `/api/sim-agent/latest`, `/api/artifacts/budget-plot`) |
| 6 | `dashboard-runner-export` | Dashboard Live Sim-Agent Runner & Static Export | `F15`, `F17` | `POST /api/sim-agent/run`, state persistence/isolation, `--sim-report` & `--plot-file` startup flags, `--export-dir` 4-file offline bundle |
| 7 | `dashboard-ui-interactivity` | Dashboard Browser/UI Rendering & Interactivity | `F16`, `F17` | Headless Chrome CDP: KPI strip, 9-piece pipeline & cards, live search/filter, 10×10 taxonomy inspector, 30-day SVG viewer, live runner form, `file://` & static HTTP offline modes, responsive mobile viewport, XSS safety |

*Note: `F19` is this opaque-box E2E infrastructure & Tiers 1–4 suite; `F20` is final M4 execution and Tier 5 adversarial hardening.*

---

## 3. Test Architecture

### 3.1 Directory Layout

```text
ads-simulator/
├── TEST_INFRA.md                                    # This document
└── e2e/
    ├── runner.mjs                                   # Canonical CLI test runner
    ├── registry.mjs                                 # Side-effect-free declarative registry (no skip)
    ├── helpers.mjs                                  # Binary/HTTP/Chrome CDP helpers & strict validators
    └── tests/
        ├── index.mjs                                # Aggregates & validates all 16 test modules
        ├── tier1_python_removal_hygiene.mjs         # Area 1, Tier 1 (6 tests)
        ├── tier1_marketplace_generator.mjs          # Area 2, Tier 1 (6 tests)
        ├── tier1_cpc_simulator.mjs                  # Area 3, Tier 1 (6 tests)
        ├── tier1_sim_agent_plotting.mjs             # Area 4, Tier 1 (6 tests)
        ├── tier1_dashboard_catalog_api.mjs          # Area 5, Tier 1 (6 tests)
        ├── tier1_dashboard_runner_export.mjs        # Area 6, Tier 1 (6 tests)
        ├── tier1_dashboard_ui_interactivity.mjs     # Area 7, Tier 1 (6 tests)
        ├── tier2_python_removal_hygiene.mjs         # Area 1, Tier 2 (6 tests)
        ├── tier2_marketplace_generator.mjs          # Area 2, Tier 2 (7 tests)
        ├── tier2_cpc_simulator.mjs                  # Area 3, Tier 2 (8 tests)
        ├── tier2_sim_agent_plotting.mjs             # Area 4, Tier 2 (7 tests)
        ├── tier2_dashboard_catalog_api.mjs          # Area 5, Tier 2 (6 tests)
        ├── tier2_dashboard_runner_export.mjs        # Area 6, Tier 2 (6 tests)
        ├── tier2_dashboard_ui_interactivity.mjs     # Area 7, Tier 2 (6 tests)
        ├── tier3_combinations.mjs                   # Tier 3 cross-feature combinations (12 tests)
        └── tier4_scenarios.mjs                      # Tier 4 real-world user workflows (7 scenarios)
```

### 3.2 Runner CLI Contract (`e2e/runner.mjs`)

```bash
# Build all workspace binaries first
cargo build --workspace --release --locked

# List all registered tests without side effects (no binary/server/Chrome calls)
node e2e/runner.mjs --list

# Run sequentially tier by tier (recommended M4 gate order)
node e2e/runner.mjs --tier 1
node e2e/runner.mjs --tier 2
node e2e/runner.mjs --tier 3
node e2e/runner.mjs --tier 4

# Run entire E2E suite
node e2e/runner.mjs --tier all

# Useful filtering & diagnostic flags
node e2e/runner.mjs --tier 1 --area sim-agent-plotting --verbose
node e2e/runner.mjs --filter T3-04 --fail-fast
node e2e/runner.mjs --tier all --json
```

#### CLI Options & Exit Codes

| Flag | Description |
|---|---|
| `--tier 1\|2\|3\|4\|all` | Select tier(s) to list or execute (default: `all`). Invalid values exit `2`. |
| `--list` | Print grouped tests and per-tier / per-area summary counts with zero side effects; exits `0`. |
| `--area <slug[,slug]>` | Filter by one or more of the 7 canonical feature area slugs. Unknown slug exits `2`. |
| `--filter <substring>` | Case-insensitive substring filter against test ID, title, or area. |
| `--fail-fast` | Halt immediately on first failing test and exit `1`. |
| `--verbose` | Stream per-test `ctx.log` output even for passing tests and stack excerpts on failures. |
| `--json` | Emit structured JSON summary (`{ ok, tier, durationMs, counts, results }`). |
| `-h`, `--help` | Print CLI usage and exit `0`. |

| Exit Code | Meaning |
|---|---|
| `0` | `--help`, `--list`, or test run with `selected > 0` and `failed === 0`. |
| `1` | One or more test failures, missing workspace binaries, timeouts, or 0 tests matched during execution. |
| `2` | Invalid CLI arguments (invalid `--tier`, unknown `--area`, missing flag value, unknown flag). |

Environment variables:
- `E2E_BIN_PROFILE=release|debug` — Restrict binary resolution strictly to `target/release/` or `target/debug/` (default checks `release`, then `debug`).
- `E2E_CHROME_BIN=/path/to/chrome` — Override headless Chrome binary path (default auto-discovers `/usr/bin/google-chrome`).
- `E2E_KEEP_TMP=1` — Preserve per-test temp directories after completion for debugging.

### 3.3 Helpers & Opaque-Box Validators (`e2e/helpers.mjs`)

- **Process & Binary Lifecycle**: `resolveBinary` (throws `BinaryNotFoundError` loudly; zero mocking backdoors), `runBinary`, `runBinaryOk`, `runCargo`, `withTempDir`.
- **Servers**:
  - `startDashboardServer`: launches `ads-simulator-dashboard --host 127.0.0.1 --port 0`, parses bound URL from stdout, verifies `GET /health` → `200 {"status":"ok"}`, guarantees clean `SIGTERM`→`SIGKILL` teardown.
  - `startStaticServer`: pure `node:http` static bundle server bound to `127.0.0.1:0`, logs `{ method, path, status }` in `requests[]`, and returns `404` JSON for any `/api/*` leak.
- **HTTP Client**: `httpRequest`, `httpGetText`, `httpGetJson`, `httpPostJson`, `httpPostRaw` over Node 22 native `fetch`.
- **Headless Chrome CDP**: `findChromeBinary`, `openBrowserSession`, `inspectPageWithChrome`, `dumpDomWithChrome` over Node 22 native `WebSocket`. Captures `consoleErrors`, `pageErrors`, `logErrors`, `failedRequests`, `networkRequests`, and tears down cleanly via `Browser.close` → process exit wait (`SIGKILL` fallback) → WebSocket close → `rmSync(..., { maxRetries: 5, retryDelay: 100 })`.
- **Domain Validators**:
  - `assertZeroPythonFiles` — Verifies zero `.py` files across the workspace.
  - `validateMarketplaceJson` — Audits Schema v1 `marketplace.json` (10×10 taxonomy, 50 advertisers × 20 items, 5,000 organic items, 6,000 unique IDs, 1,000 distinct 5-interest users, 12,000 browsing events with exact 9/3 affinity/exploration split).
  - `validateSimulationReport` — Audits 30-day, 500-campaign `simulation.json` ledger conservation, daily quota splits, and campaign-to-day rollups.
  - `validateBudgetTimeSeries` & `validateSimulationAndTimeSeriesParity` — Audits `budget_timeseries.json` monotonicity, utilization/CTR math, zero-budget guards, active campaign decay, and cent-for-cent parity with `simulation.json`.
  - `validateBudgetCsv` — Verifies 1 header + 30 day rows matching `budget_timeseries.json`.
  - `validateBudgetSvg` — Pure-JS XML well-formedness parser + numeric sanity (`zero NaN/Infinity/undefined`) + `viewBox`/dimensions ≥400×250 + non-negative geometry + ≥30 daily spend bar `<rect>` cluster + ≥30-point cumulative `<polyline>`/`<path>` (non-flat when spend > 0) + daily/cumulative/day-range/formatted-USD KPI text verification.
  - `validateSimulatorCatalog` — Verifies 9 unique pieces spanning `ads-simulator`, `ads-marketplace-simulation`, `sim-agent`, `.rs`-only on-disk `source_files`, connected `pipeline_edges` covering all 9 pieces, ≥3 honest `gaps_and_simplifications`, and `summary_counts` consistency.
  - `validateMarketplaceApiResponse` — Verifies `GET /api/marketplace` taxonomy, counts, advertiser objectives, price stats, and 9k/3k browsing split.
  - `validateStaticExportBundle` — Verifies all 4 required static export files (`index.html`, `dashboard-data.json`, `budget_timeseries.json`, `budget_spent_30d.svg`) and validates their contents.

---

## 4. Tier 4 Real-World Application Scenarios

All 7 end-to-end workflows in `e2e/tests/tier4_scenarios.mjs` simulate realistic multi-step user journeys:

1. **`T4-01` — Ad-Ops Baseline 30-Day 1M-Impression Budget Audit & Visual Dashboard Review**: Runs `sim-agent` with defaults, independently cross-audits all 4 artifacts against golden totals (`1,000,000` impressions, `2,020` clicks, `$2,020.00` spend, `$47,980.00` remaining), boots `ads-simulator-dashboard --sim-report --plot-file`, and verifies rendered `$2,020` / `2,020` / `1,000,000` metrics in headless Chrome with zero runtime errors.
2. **`T4-02` — Marketplace Economist Budget-Constrained Campaign Depletion Study**: Runs `sim-agent --no-impression-target` across three economic regimes — Ample (`$100`/campaign), Tight (`$2`/campaign), and Crunch (`$1`/campaign) — validating all 12 generated artifacts, strict utilization ordering (`Ample < Tight < Crunch == 100%`), campaign exhaustion monotonicity (`0` active campaigns at day 30 in Crunch), and 3 pairwise-distinct valid SVGs.
3. **`T4-03` — Product Analyst Interactive Dashboard Exploration & Live What-If Parameter Sweep**: Opens live `ads-simulator-dashboard` in headless Chrome, tests interactive feature explorer search narrowing and clearing, inspects custom marketplace `seed=101`, triggers two live What-If simulations via the UI form (300k target quota, then natural single-pass replay), and verifies HTTP `400` resilience on `{ cpc_cents: 0 }` followed by healthy `/health`.
4. **`T4-04` — CI Static Artifact Export & Air-Gapped Offline Review (`http://` + `file://`)**: Runs `sim-agent --seed 2026 --impressions 400000`, exports a static dashboard bundle via `--export-dir`, serves it over pure-Node `startStaticServer` proving zero `/api/*` requests and zero 4xx/5xx responses in Chrome, and opens the exported `index.html` directly over `file://` verifying all 9 pieces render cleanly without an HTTP server.
5. **`T4-05` — Data Scientist Multi-Seed Reproducibility & Statistical Sensitivity Study**: Confirms byte-for-byte determinism across duplicate `seed=42` runs across all 4 `sim-agent` files, compares alternate seeds (`100`, `200`) for plausible CTR (`0.05%–0.6%`) and distinct spend/SVG outputs, and verifies `/api/marketplace?seed=100` vs `seed=200` taxonomy invariance.
6. **`T4-06` — Greenfield Clean-Room Pipeline (Zero-Python → Marketplace → Engine → Agent → Dashboard + Chrome)**: Verifies zero `.py` files and all 4 compiled workspace binaries, then chains `ads-simulator --seed 55` → `ads-marketplace-simulation` → `sim-agent --marketplace` (proving exact `simulation.json` parity) → `ads-simulator-dashboard` API & headless Chrome DOM inspection.
7. **`T4-07` — Simulation Researcher Natural Replay vs Exact Impression-Target Comparison Study**: Compares `--no-impression-target` (360k sequential replay) vs `--impressions 360000` (shuffled exact quota) vs `--impressions 1000000` (1M quota), confirming distinct daily realizations and exact JSON + byte-for-byte SVG parity with `POST /api/sim-agent/run` (`impressions: null`).

---

## 5. Coverage Thresholds

| Tier | Description | Required Minimum | Implemented | Status |
|---|---|---|---|---|
| **Tier 1** | Isolated Happy-Path (≥5 per each of 7 areas) | ≥ 35 (≥ 5/area) | **42** (6/area across all 7 areas) | Exceeds threshold |
| **Tier 2** | Boundary, Edge & Negative (≥5 per each of 7 areas) | ≥ 35 (≥ 5/area) | **46** (6–8/area across all 7 areas) | Exceeds threshold |
| **Tier 3** | Cross-Feature Pairwise Combinations | ≥ 10 | **12** | Exceeds threshold |
| **Tier 4** | Real-World End-to-End Scenarios | ≥ 6 | **7** | Exceeds threshold |
| **Total** | Complete Opaque-Box E2E Suite | ≥ 86 | **107** | Exceeds threshold |

### Per-Area Tier 1 & Tier 2 Breakdown

| Feature Area Slug | Tier 1 | Tier 2 | Tier 3 Participation | Tier 4 Participation |
|---|---:|---:|---:|---:|
| `python-removal-hygiene` | 6 | 6 | 2 (`T3-05`, `T3-10`) | 1 (`T4-06`) |
| `marketplace-generator` | 6 | 7 | 5 (`T3-01`, `T3-05`, `T3-06`, `T3-09`, `T3-10`) | 2 (`T4-05`, `T4-06`) |
| `cpc-simulator` | 6 | 8 | 5 (`T3-02`, `T3-05`, `T3-08`, `T3-10`, `T3-11`) | 3 (`T4-02`, `T4-06`, `T4-07`) |
| `sim-agent-plotting` | 6 | 7 | 10 (`T3-01`–`T3-04`, `T3-07`–`T3-12`) | 7 (`T4-01`–`T4-07`) |
| `dashboard-catalog-api` | 6 | 6 | 5 (`T3-01`, `T3-05`, `T3-06`, `T3-08`, `T3-10`) | 4 (`T4-01`, `T4-03`, `T4-05`, `T4-06`) |
| `dashboard-runner-export` | 6 | 6 | 7 (`T3-01`, `T3-03`, `T3-04`, `T3-07`, `T3-09`, `T3-11`, `T3-12`) | 5 (`T4-01`, `T4-02`, `T4-03`, `T4-04`, `T4-07`) |
| `dashboard-ui-interactivity` | 6 | 6 | 2 (`T3-07`, `T3-12`) | 4 (`T4-01`, `T4-03`, `T4-04`, `T4-06`) |
