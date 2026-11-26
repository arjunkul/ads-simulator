# Project: ads-simulator

## Architecture
A pure-Rust (Edition 2024, MSRV 1.88) Cargo workspace for synthetic ad-marketplace data generation, 30-day CPC marketplace simulation, autonomous `sim-agent` execution & 30-day budget plotting, and a live/static feature & architecture dashboard — with all legacy Python code removed.

```text
ads-simulator/ (Cargo workspace root)
├── Cargo.toml                  # Workspace root + `ads-simulator` crate
├── Cargo.lock                  # Unified workspace lockfile
├── README.md                   # Project overview & quickstart for all crates
├── docs/marketplace-data.md    # Dataset contract (Rust-only, legacy Python refs removed)
├── src/                        # `ads-simulator` library & CLI (marketplace generator + specs)
├── tests/marketplace.rs        # Root Rust integration tests
├── simulation/                 # `ads-marketplace-simulation` library & CLI (30-day CPC engine)
│   ├── Cargo.toml
│   ├── README.md
│   └── src/
│       ├── lib.rs              # Library exports + SimulationSpec metadata
│       ├── main.rs             # CLI binary (`ads-marketplace-simulation`)
│       └── simulation.rs       # 30-day replay, ad selection, 4% click model, ledger report
├── sim-agent/                  # `sim-agent` library & CLI (30-day runner, auditor & SVG plotter)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── main.rs
│       ├── agent.rs            # Run pipeline, ledger audit, BudgetTimeSeries derivation
│       └── plot.rs             # Deterministic pure-Rust dual-panel SVG budget plotter
├── dashboard/                  # `ads-simulator-dashboard` library & CLI (feature dashboard + server/export)
│   ├── Cargo.toml
│   ├── assets/                 # Embedded zero-build HTML5/CSS/JS frontend
│   │   ├── index.html
│   │   ├── styles.css
│   │   └── app.js
│   └── src/
│       ├── lib.rs
│       ├── main.rs             # CLI (`--host`/`--port` server or `--export-dir` static bundle)
│       ├── catalog.rs          # Live code-derived 9-piece SimulatorCatalog
│       ├── server.rs           # std::net::TcpListener HTTP/1.1 API & static asset server
│       └── export.rs           # Self-contained static bundle generator
└── e2e/                        # Opaque-box E2E test suite (Node.js 22 native, zero Python)
    ├── runner.mjs              # Canonical E2E runner (supports --tier 1|2|3|4|all)
    ├── helpers.mjs             # Shared CLI/HTTP/SVG/browser opaque-box helpers
    └── tests/                  # Tier 1–4 test suites
```

## Code Layout & Write Ownership
| Owner | Exclusive Paths |
|---|---|
| **M1 (`cleanup-and-core-apis`)** | Delete `marketplace_data.py`, `tests/test_marketplace_data.py`, `simulation/Cargo.lock`, `simulation/.gitignore`; Edit `.gitignore`, `Cargo.toml`, `Cargo.lock`, `docs/marketplace-data.md`, `src/lib.rs`, `src/taxonomy.rs`, `tests/marketplace.rs`, `simulation/Cargo.toml`, `simulation/src/lib.rs`, `simulation/src/main.rs`, `simulation/src/simulation.rs` |
| **M2 (`sim-agent`)** | Create `sim-agent/**`; Add `"sim-agent"` to root `Cargo.toml` workspace members & update `Cargo.lock` |
| **M3 (`dashboard`)** | Create `dashboard/**`; Add `"dashboard"` to root `Cargo.toml` workspace members & update `Cargo.lock`; Update `README.md` & `simulation/README.md` |
| **E2E Testing Track (`e2e_orch`)** | Create `e2e/**`, `TEST_INFRA.md`, `TEST_READY.md` (NEVER touches Rust source files or `Cargo.toml`) |
| **M4 (`final-e2e`)** | Any bug fixes across workspace crates + Tier 5 adversarial tests after `TEST_READY.md` is published |

## Feature Inventory
Every Survey feature assigned a milestone (none unassigned).

| # | Feature | Description | Milestone | Source |
|---|---|---|---|---|
| F1 | Remove legacy `marketplace_data.py` | Delete the 198-line legacy Python marketplace generator at repo root | M1 | survey_1 |
| F2 | Remove legacy `tests/test_marketplace_data.py` | Delete legacy Python unittest suite while preserving `tests/` and `tests/marketplace.rs` | M1 | survey_1 |
| F3 | Clean Python references in `docs/marketplace-data.md` | Remove dead `python3 marketplace_data.py` command (L12) and stale Python wording (L26) while keeping Rust serde backward-compat note | M1 | survey_1 |
| F4 | Update `.gitignore` hygiene | Remove `__pycache__/`, keep `/target/`, add `/output/` for default `sim-agent` output artifacts | M1 | survey_1, survey_3 |
| F5 | Convert to unified Cargo workspace | Add `[workspace]` to root `Cargo.toml`, unify lockfile (remove `simulation/Cargo.lock` & `simulation/.gitignore`), preserve root & simulation builds/tests | M1 | survey_2, survey_3 |
| F6 | Expose `ads-simulator` live metadata & taxonomy | Export `pub fn marketplace_spec() -> MarketplaceSpec` and `pub fn taxonomy()` (and public constants) in `ads-simulator` without breaking existing API/tests | M1 | survey_2 |
| F7 | Expose `ads-marketplace-simulation` library crate & specs | Add `simulation/src/lib.rs`, expose public `Config`, `Report`, `OwnedReport`/`ReportData` (with `Serialize + Deserialize`), `Summary`, `Day`, `Campaign`, `Activity`, `DailyActivity`, `run()`, and `simulation_spec() -> SimulationSpec` without altering RNG/math | M1 | survey_2, survey_3 |
| F8 | Fix `simulation` CLI output parent directory creation | Call `fs::create_dir_all` on `--output` parent directory in `simulation/src/main.rs` before creating `NamedTempFile` | M1 | survey_3 |
| F9 | `sim-agent` real simulator execution pipeline | Library (`sim-agent/src/agent.rs`) executing `ads-simulator` (or loading `--marketplace`) and `ads-marketplace-simulation::run` over 30 days | M2 | survey_3 |
| F10 | `sim-agent` ledger invariant auditor | Programmatically verify `days.len() == 30`, daily spend sum == summary spend, campaign daily sums == day totals, `remaining + spend == budget`, `spend == clicks * cpc` | M2 | survey_3 |
| F11 | `sim-agent` 30-day `BudgetTimeSeries` derivation | Compute 30-day daily spend, cumulative spend, remaining budget, utilization %, clicks, impressions, CTR, end-of-day active campaign count, exhausted campaigns, insufficient-budget opportunities | M2 | survey_3 |
| F12 | `sim-agent` deterministic dual-panel SVG budget plot | Pure-Rust `plot.rs` generating `budget_spent_30d.svg` with KPI header, Daily Budget Spent bars (Days 1–30 + mean line + tooltips), Cumulative Budget Spent line+area (Days 1–30 + utilization/exhaustion annotations), zero-spend & XML-escaping guards | M2 | survey_3 |
| F13 | `sim-agent` CLI & atomic artifact export | CLI flags (`--output-dir`, `--marketplace`, `--marketplace-seed`, `--seed`, `--campaign-budget-cents`, `--cpc-cents`, `--impressions`, `--no-impression-target`) writing `simulation.json`, `budget_timeseries.json`, `budget_timeseries.csv`, `budget_spent_30d.svg` atomically + console summary | M2 | survey_3 |
| F14 | Code-derived 9-piece `SimulatorCatalog` | Introspect compiled specs from `ads-simulator`, `ads-marketplace-simulation`, and `sim-agent` covering all 9 simulator pieces with parameters, source paths, implemented features, unused/legacy fields, gaps, and pipeline edges | M3 | survey_2 |
| F15 | Pure-Rust HTTP API & asset server | `std::net::TcpListener` server (`GET /`, `/health`, `/api/catalog`, `/api/marketplace`, `/api/sim-agent/latest`, `POST /api/sim-agent/run`, `GET /api/artifacts/budget-plot`) with strict input validation and 400/404 handling | M3 | survey_2, survey_3 |
| F16 | Interactive Feature & Budget Dashboard UI | Embedded zero-build HTML5/CSS/JS/SVG UI: KPI strip, interactive 9-piece architecture pipeline diagram, searchable/filterable per-piece feature & gap cards, live 10×10 taxonomy & marketplace stats inspector, interactive 30-day budget spend charts + `sim-agent` SVG viewer + live simulation runner form | M3 | survey_2, survey_3 |
| F17 | Dashboard static export mode (`--export-dir`) | Write self-contained static bundle (`index.html`, `dashboard-data.json`, `budget_timeseries.json`, `budget_spent_30d.svg`) working without a running server | M3 | survey_2 |
| F18 | Workspace & Crate Documentation | Update `README.md` and `simulation/README.md` documenting workspace commands, `ads-simulator`, `simulation`, `sim-agent`, and `dashboard` | M3 | survey_1, survey_2, survey_3 |
| F19 | Opaque-box E2E test suite (Tiers 1–4) | Independent requirement-driven E2E suite in `e2e/` (Node.js 22, zero Python) with `TEST_INFRA.md` and `TEST_READY.md` | E2E | pattern |
| F20 | Final E2E Pass (Tiers 1–4) & Tier 5 Adversarial Hardening | Pass 100% of Tiers 1–4 sequentially, then run Challenger-initiated Tier 5 white-box adversarial hardening until zero gaps remain | M4 | pattern |

## Milestones
| # | Name | Scope | Dependencies | Status |
|---|---|---|---|---|
| M1 | `cleanup-and-core-apis` | F1–F8: Remove old Python code & refs, unify Cargo workspace, expose `ads-simulator` & `ads-marketplace-simulation` library/metadata APIs, fix output dir creation | none | DONE (`19/19` tests, Gate PASS) |
| M2 | `sim-agent` | F9–F13: Pure-Rust `sim-agent` library + CLI running 30-day simulator, auditing ledgers, exporting JSON/CSV & deterministic dual-panel `budget_spent_30d.svg` | M1 | IN_PROGRESS |
| M3 | `dashboard` | F14–F18: Pure-Rust `dashboard` crate with live code-derived 9-piece `SimulatorCatalog`, HTTP server, zero-build interactive UI, live `sim-agent` integration, `--export-dir`, and README docs | M1, M2 | PLANNED |
| E2E | `e2e-testing-track` | F19: Independent opaque-box E2E test infrastructure & Tiers 1–4 test cases in `e2e/`, publishing `TEST_INFRA.md` & `TEST_READY.md` | none | IN_PROGRESS |
| M4 | `final-e2e` | F20: Phase 1 pass 100% E2E (Tiers 1→2→3→4) + Phase 2 Tier 5 adversarial coverage hardening | M3, E2E | PLANNED |

## Interface Contracts

### 1. `ads-simulator` ↔ Downstream Crates (`simulation`, `sim-agent`, `dashboard`)
- Existing public API preserved unchanged: `Marketplace`, `Category`, `Advertiser`, `Item`, `User`, `BrowsingEvent`, `BrowseReason`, `generate_marketplace(seed: i64) -> Marketplace`, `score_preference(user: &User, item: &Item) -> f64`.
- New read-only exports in `src/lib.rs`:
  - `pub const SCHEMA_VERSION: u32 = 1;`
  - `pub const CURRENCY: &str = "USD";`
  - `pub const DEFAULT_SEED: i64 = 42;`
  - `pub const ADVERTISER_COUNT: usize = 50;`
  - `pub const CATALOG_SIZE: usize = 20;`
  - `pub const USER_COUNT: usize = 1_000;`
  - `pub const ORGANIC_PER_CATEGORY: usize = 50;`
  - `pub const INTEREST_COUNT: usize = 5;`
  - `pub const AFFINITY_VIEWS: usize = 9;`
  - `pub const EXPLORATION_VIEWS: usize = 3;`
  - `pub const TAXONOMY_GROUP_COUNT: usize = 10;`
  - `pub const CATEGORIES_PER_GROUP: usize = 10;`
  - `pub fn taxonomy() -> &'static [(&'static str, [&'static str; 10])]`
  - `#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)] pub struct MarketplaceSpec { ... }` + `pub fn marketplace_spec() -> MarketplaceSpec`

### 2. `ads-marketplace-simulation` ↔ `sim-agent` / `dashboard`
- Crate library target (`simulation/src/lib.rs`) exports `pub mod simulation;` and re-exports `Config`, `Report`, `OwnedReport` (or `Report` with public fields + `Deserialize` owned counterpart), `Summary`, `Day`, `Campaign`, `Activity`, `DailyActivity`, `SimulationSpec`, `simulation_spec()`, `run`, and constants:
  - `pub const SCHEMA_VERSION: u32 = 1;`
  - `pub const DAYS: usize = 30;`
  - `pub const CAMPAIGNS_PER_ADVERTISER: usize = 10;`
  - `pub const MAX_CLICK_PROBABILITY: f64 = 0.04;`
  - `pub const DEFAULT_SEED: u64 = 42;`
  - `pub const DEFAULT_CAMPAIGN_BUDGET_CENTS: u64 = 10_000;`
  - `pub const DEFAULT_CPC_CENTS: u64 = 100;`
- Existing CLI binary `ads-marketplace-simulation` (`simulation/src/main.rs`) keeps all exact CLI flags (`--marketplace`, `--output`, `--seed`, `--campaign-budget-cents`, `--cpc-cents`, `--impressions`) and exact deterministic RNG/math behavior (`seed=42, budget=10000, cpc=100, impressions=1000000` => `clicks=2020`, `spend_cents=202000`), and now creates missing parent directories for `--output`.

### 3. `sim-agent` CLI & Output Contract ↔ User / `dashboard` / E2E
- Binary name: `sim-agent` (`cargo run --release -p sim-agent -- [OPTIONS]`)
- CLI flags:
  - `--output-dir <PATH>` (default: `output/sim-agent`)
  - `--marketplace <PATH>` (optional: load existing `marketplace.json`; if omitted, generates marketplace in memory)
  - `--marketplace-seed <i64>` (optional: defaults to `--seed` as `i64`; ignored if `--marketplace` is provided)
  - `--seed <u64>` (default: `42`)
  - `--campaign-budget-cents <u64>` (default: `10000`)
  - `--cpc-cents <u64>` (default: `100`)
  - `--impressions <u64>` (optional: defaults to `1000000` unless `--no-impression-target` is passed)
  - `--no-impression-target` (flag: conflicts with `--impressions`; runs natural single-pass 12k/day replay mode across 30 days)
- Output files written atomically to `<output-dir>`:
  1. `simulation.json` — Raw simulator `Report` JSON (Schema v1)
  2. `budget_timeseries.json` — Compact 30-day budget summary & daily series:
     - Top-level: `schema_version` (`1`), `currency` (`"USD"`), `config` (`{ marketplace_seed, sim_seed, campaign_budget_cents, cpc_cents, impressions }`), `summary` (`{ total_budget_cents, total_spend_cents, remaining_budget_cents, utilization_pct, total_impressions, total_clicks, ctr_pct, exhausted_campaigns, total_campaigns, insufficient_budget_opportunities, min_daily_spend_cents, mean_daily_spend_cents, max_daily_spend_cents }`), `days` (30 objects with `day` 1..=30, `impressions`, `clicks`, `daily_spend_cents`, `cumulative_spend_cents`, `remaining_budget_cents`, `utilization_pct`, `active_campaigns_end_of_day`, `insufficient_budget`).
  3. `budget_timeseries.csv` — Header row + 30 data rows matching `days`
  4. `budget_spent_30d.svg` — Well-formed standalone SVG (`<svg ...>...</svg>`) with KPI summary, Daily Budget Spent bar panel (30 bars), Cumulative Budget Spent curve panel (30 points), zero `NaN`/`Infinity`, deterministic output.
- Library exports (`sim_agent`): `AgentConfig`, `RunResult`, `BudgetTimeSeries`, `DailyBudgetPoint`, `BudgetSummary`, `run_simulation(&AgentConfig) -> Result<RunResult, Error>`, `run_and_export(&AgentConfig, &Path) -> Result<RunArtifacts, Error>`, `render_budget_svg(&BudgetTimeSeries) -> String`.

### 4. `dashboard` CLI & HTTP API Contract ↔ User / E2E
- Binary name: `ads-simulator-dashboard` (`cargo run --release -p ads-simulator-dashboard -- [OPTIONS]`)
- CLI flags:
  - `--host <IP>` (default: `127.0.0.1`)
  - `--port <u16>` (default: `8080`; supports `0` for OS-assigned ephemeral port, printing the bound URL `http://<host>:<actual_port>` to stdout immediately when ready)
  - `--sim-report <PATH>` (optional: path to `sim-agent`'s `budget_timeseries.json` or `simulation.json`)
  - `--plot-file <PATH>` (optional: path to `sim-agent`'s `budget_spent_30d.svg`)
  - `--export-dir <PATH>` (optional: if specified, exports static bundle `index.html`, `dashboard-data.json`, `budget_timeseries.json`, `budget_spent_30d.svg` and exits 0 without starting HTTP server)
- HTTP Endpoints (when serving):
  - `GET /` → `200 text/html; charset=utf-8` (Dashboard web UI)
  - `GET /health` → `200 application/json` (`{"status":"ok"}`)
  - `GET /api/catalog` → `200 application/json` (`SimulatorCatalog`: `simulator_version`, `schema_version`, `currency`, `summary_counts`, `pipeline_edges`, `pieces` [9 pieces with `id`, `name`, `crate_name`, `stage`, `source_files`, `summary`, `parameters`, `features`, `gaps_and_simplifications`])
  - `GET /api/marketplace?seed=<i64>` → `200 application/json` (default seed `42`; returns `seed`, `taxonomy` [10 groups × 10 categories], `counts`, `advertiser_objectives`, `price_stats`, `browsing_split`; invalid seed → `400`)
  - `GET /api/sim-agent/latest` → `200 application/json` (`BudgetTimeSeries` from loaded report or default baseline `sim-agent` run)
  - `POST /api/sim-agent/run` → Request JSON `{ marketplace_seed?, seed?, campaign_budget_cents?, cpc_cents?, impressions? }` (where `impressions: null` selects natural replay mode); Response `200 application/json` `{ timeseries: BudgetTimeSeries, svg: "<svg ...>" }`; Invalid JSON or invalid simulation params (e.g., `cpc_cents == 0` or unreachable impression target) → `400 application/json` `{ "error": "..." }`
  - `GET /api/artifacts/budget-plot` → `200 image/svg+xml` (loaded `--plot-file` or rendered SVG from active `sim-agent` run)
  - Unknown route → `404 application/json` `{ "error": "not found" }`
- Static Export (`--export-dir <DIR>`):
  - Writes `index.html` (with embedded bootstrap data so it renders cleanly even opened from `file://` or static HTTP server), `dashboard-data.json`, `budget_timeseries.json`, and `budget_spent_30d.svg`.

### 5. Canonical Verification Commands
- Zero legacy Python check: `find . -path './.git' -prune -o -path './.agents' -prune -o -path './target' -prune -o -name '*.py' -print` returns empty output.
- Workspace tests: `cargo test --workspace --locked` passes with 0 failures.
- E2E suite: `node e2e/runner.mjs --tier all` passes with exit code 0.
