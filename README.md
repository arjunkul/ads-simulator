# Ads Marketplace Simulator

An ads marketplace simulator workspace modeling shopper behavior, advertiser campaigns, user-independent visit simulation and ad slot generation, a pluggable 5-axis `ad-server` (**Retrieval × Pacing × Bidding × Scoring × Ranking**), daily CTR model training via `ads-ml` (`Score-R1`), automated post-run simulation analysis, an autonomous `sim-agent` 30-day budget plotter, and an interactive feature/flowchart/analysis dashboard.

## Workspace Crates

| Crate | Path | Binary | Purpose |
|---|---|---|---|
| `ads-simulator` | `.` ([src/lib.rs](src/lib.rs)) | `ads-simulator` | Deterministic synthetic marketplace generator (10×10 taxonomy, 50 advertisers, 1,000 shoppers, 5,000 organic items, 12,000 browsing events). |
| `ads-ml` | `ml/` ([ml/src/lib.rs](ml/src/lib.rs)) | *(library)* | Daily JSONL impression/click log store (`ImpressionLogStore`), L2-regularized logistic regression CTR model (`Score-R1` via PyTorch worker), and pre-indexed `PreparedScorer`. |
| `ad-server` | `ad-server/` ([ad-server/src/lib.rs](ad-server/src/lib.rs)) | *(library)* | Standalone ad-server module accepting `user_id` via `serve_ads(user_id)` with explicit 5-axis strategies: Retrieval (`Retrieve-V0`), Pacing (`P0`–`P2`, `pacing-1` 24h PST intraday schedule), Bidding (`bidding-basebid`, `bidding-autobid-v0`), Scoring (`S0`–`S3`, `Score-R1`), and Ranking (`R1`–`R3`). |
| `visit-simulator` | `visit-simulator/` ([visit-simulator/src/lib.rs](visit-simulator/src/lib.rs)) | `visit-simulator` | Models visitor preferred categories (5 unique categories/user + `score_preference` on Stream 3), preference-independent user visits & slot generation (Stream 1) operating `AdServer::serve_ads(user_id)` across 30 days with optional PST diurnal timestamps, and the visitor `ClickOracle` (Stream 2: `4%` preferred / `0%` non-preferred). |
| `ads-marketplace-simulation` | `simulation/` ([simulation/src/simulation.rs](simulation/src/simulation.rs)) | `ads-marketplace-simulation` | 30-day click-only CPC simulation orchestrator wiring `VisitSimulator` (visits + `ClickOracle`) → `AdServer` → Schema v1 JSON report (plus daily `Score-R1` training when selected). |
| `simulation-analyzer` | `simulation-analyzer/` ([simulation-analyzer/src/lib.rs](simulation-analyzer/src/lib.rs)) | `simulation-analyzer` | Automated post-run analyzer computing budget depletion milestones, overall & daily CTR, eCPM, campaign spend Gini concentration, category/advertiser rollups, and diagnostic findings. |
| `sim-agent` | `sim-agent/` ([sim-agent/src/agent.rs](sim-agent/src/agent.rs)) | `sim-agent` | Autonomous 30-day simulator runner, ledger auditor, automated analyzer, JSON/CSV exporter, and deterministic pure-Rust dual-panel SVG budget plotter. |
| `ads-simulator-dashboard` | `dashboard/` ([dashboard/src/server.rs](dashboard/src/server.rs)) | `ads-simulator-dashboard` | Interactive web dashboard and HTTP/1.1 API featuring the Component Interaction Flowchart, 11-piece catalog, live Ad-Server sampler, Automated Simulation Analyzer view, live 5-axis runner, and offline `--export-dir` static bundles. |

## Quickstart

Requires Rust 1.88+ (Edition 2024).

### 1. Run workspace tests
```sh
cargo test --workspace --locked
```

### 2. Generate synthetic marketplace data
```sh
cargo run --release --locked -p ads-simulator -- --seed 42 --output-dir /tmp/ads-cpc
```

### 3. Run the standalone user-independent visit simulator against `ad-server` (R1)
```sh
cargo run --release --locked -p visit-simulator -- \
  --marketplace /tmp/ads-cpc/marketplace.json \
  --output /tmp/ads-cpc/slates.json \
  --seed 42 --ranker R1 --slate-size 20 --requests 25
```

### 4. Run the 30-day CPC simulation & auto-analyze it
```sh
cargo run --release --locked -p ads-marketplace-simulation -- \
  --marketplace /tmp/ads-cpc/marketplace.json \
  --output /tmp/ads-cpc/simulation.json \
  --seed 42 --campaign-budget-cents 10000 --cpc-cents 100 --impressions 1000000 --ranker R1 --slate-size 20

cargo run --release --locked -p simulation-analyzer -- \
  --report /tmp/ads-cpc/simulation.json \
  --output /tmp/ads-cpc/analysis.json
```

### 5. Run `sim-agent` (simulate + audit + auto-analyze + plot 30-day budget spend)
```sh
cargo run --release --locked -p sim-agent -- \
  --seed 42 --campaign-budget-cents 10000 --cpc-cents 100 --impressions 1000000 --ranker R1 \
  --output-dir output/sim-agent
```
Outputs written atomically to `output/sim-agent/`:
- `simulation.json` — Raw Schema v1 simulator report
- `budget_timeseries.json` — Compact 30-day budget summary & daily metrics
- `budget_timeseries.csv` — 30-row CSV of daily & cumulative spend
- `budget_spent_30d.svg` — Standalone dual-panel 30-day budget spend SVG plot
- `analysis.json` — Full `SimulationAnalysis` (depletion milestones, CTR, eCPM, Gini, findings)

Pass `--no-impression-target` to run the natural user-independent request stream (600 requests/day × 20 ads/req = 12,000 impressions/day across 30 days).

### 6. Launch the Feature, Flowchart & Analyzer Dashboard (or export static bundle)
```sh
# Live server mode (use --port 0 for an OS-assigned port)
cargo run --release --locked -p ads-simulator-dashboard -- --host 127.0.0.1 --port 8080

# Offline static bundle mode
cargo run --release --locked -p ads-simulator-dashboard -- --export-dir output/dashboard
```

## Documentation
- [docs/marketplace-data.md](docs/marketplace-data.md) — Schema v1 synthetic marketplace dataset contract
- [simulation/README.md](simulation/README.md) — 30-day CPC delivery, `ad-server` (R1), and user-independent visit simulator model
