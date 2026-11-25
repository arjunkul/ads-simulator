# 30-day CPC simulation

From the repository root, generate the seeded marketplace and run the simulation.

```sh
mkdir -p /tmp/ads-cpc
cargo run --locked -- --seed 42 --output-dir /tmp/ads-cpc
cargo run --release --locked --manifest-path simulation/Cargo.toml -- \
  --marketplace /tmp/ads-cpc/marketplace.json \
  --output /tmp/ads-cpc/simulation.json \
  --seed 42 --campaign-budget-cents 10000 --cpc-cents 100 --impressions 1000000
cargo test --locked --manifest-path simulation/Cargo.toml
```

To run a serving strategy combining `Rank-Greedy` with `Score-R1` (daily cold-trained logistic regression CTR scoring backed by `ads-ml` and per-day impression-click JSONL logs):

```sh
cargo run --release --locked --manifest-path simulation/Cargo.toml -- \
  --marketplace /tmp/ads-cpc/marketplace.json \
  --output /tmp/ads-cpc/simulation-r1.json \
  --ranking-strategy Rank-Greedy \
  --scoring-strategy Score-R1 \
  --impression-log-dir /tmp/ads-cpc/impressions-r1 \
  --seed 42 --campaign-budget-cents 10000 --cpc-cents 100 --impressions 1000000
```

## Serving, ranking, and scoring strategies

A **Serving strategy** is a combination of a **Ranking strategy (`Rank-*`)** and a **Scoring strategy (`Score-*`)**:

- **Scoring strategies (`--scoring-strategy`):**
  - `Score-Uniform` (default): assigns a constant neutral score to all eligible campaigns with no model training.
  - `Score-R1`: logs every delivered impression and click outcome to disk as per-day JSONL (`day-01.jsonl` through `day-30.jsonl` in `--impression-log-dir`, defaulting to `<output_stem>-impressions`). After each day `d` (`1..=29`), `ads-ml` cold-trains a fresh L2-regularized logistic regression CTR model from scratch using all logged impression-click data from days `1..=d` on disk.
- **Ranking strategies (`--ranking-strategy`):**
  - `Rank-Uniform` (default): selects one campaign uniformly at random from all affordable campaigns, regardless of scores (useful as the baseline when paired with `Score-Uniform`, or for shadow training/logging when paired with `Score-R1`).
  - `Rank-Greedy`: selects the affordable campaign with the highest candidate score, breaking ties uniformly at random (including on day 1 before `Score-R1` has training data, or under `Score-Uniform`).

## Delivery model

- Each advertiser starts with ten campaigns. Each campaign uses one distinct ad item sampled from that advertiser's catalog with the simulation seed. In the baseline marketplace, this creates 500 campaigns.
- At setup, each campaign gets a fixed budget for the entire run, independent of the advertiser's daily budget field. Both the initial budget and the campaign's CPC are configurable in USD cents. The defaults are $100 per campaign and $1 per click.
- Each organic browse event supplies one ad opportunity. Under the default serving strategy (`Rank-Uniform` + `Score-Uniform`), the simulator selects at most one campaign uniformly from all campaigns that can afford the CPC, and the organic item and its category have no effect on which ad is shown. User preferences affect clicks only. Organic replay, ad selection, and click draws use separate streams from the same simulation seed.
- Passing `--impression-log-dir` logs daily JSONL files for any serving strategy without changing delivery.
- A click has probability 4% if the ad belongs to one of the user's preferred categories and 0% otherwise. On a click, the campaign pays exactly the CPC. A campaign becomes ineligible when its remaining budget cannot afford another click.
- The run lasts 30 days. With no impression target, it replays the supplied browsing events once each day in their original order. With a target, it splits the target as evenly as possible across 30 days, giving any remainder to the earliest days one impression at a time. It shuffles the browsing events for each pass and repeats as needed to meet the daily quota. If a pass delivers zero impressions before the quota is met, the run exits with an error.
- The same marketplace JSON, seed, and campaign settings reproduce the same report with the locked dependencies and same build target.

The JSON report contains the run configuration (including `serving: { ranking, scoring }`), totals, all 30 day summaries, and each campaign's ad item, upfront and remaining budgets, and daily impression, click, and spend counts. Under `Score-R1`, `training_runs` records diagnostics for each end-of-day cold-training run. The daily and whole-run opportunity counters include both delivered impressions and skipped slots. `insufficient_budget` counts slots for which no campaign can afford the CPC. The legacy `no_matching_category` counter stays at zero.
