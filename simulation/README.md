# 30-day CPC simulation

From the repository root, generate the seeded marketplace, run the 30-day simulation via the user-independent `impression-generator` + `ad-server` (Ranker Strategy **R1**), and auto-analyze the run.

```sh
mkdir -p /tmp/ads-cpc
cargo run --locked -p ads-simulator -- --seed 42 --output-dir /tmp/ads-cpc
cargo run --release --locked -p ads-marketplace-simulation -- \
  --marketplace /tmp/ads-cpc/marketplace.json \
  --output /tmp/ads-cpc/simulation.json \
  --seed 42 --campaign-budget-cents 10000 --cpc-cents 100 --impressions 1000000 --ranker R1 --slate-size 20
cargo run --release --locked -p simulation-analyzer -- \
  --report /tmp/ads-cpc/simulation.json \
  --output /tmp/ads-cpc/analysis.json
cargo test --locked -p ads-marketplace-simulation
```

## Decoupled delivery model

- **Campaign pool (`ad-server`):** Each advertiser starts with ten campaigns (500 total in the baseline), each promoting a distinct catalog item sampled with the simulation seed. By default, each campaign receives a deterministically randomized 30-day upfront budget between \$1,000 and \$10,000 (`--campaign-budget-min-cents 100000 --campaign-budget-max-cents 1000000`, or pass `--campaign-budget-cents <CENTS>` for a fixed override) and a flat CPC (default \$1).
- **User-independent ad-impression generator (`impression-generator`):** Completely decoupled from shopper preferences and organic browsing events — it receives only the list of valid `user_ids`, samples a `user_id` uniformly on ChaCha8 Stream 1, and invokes `AdServer::serve_ads(user_id)`.
- **Ad-Server & Ads-Ranker Strategy `R1` (`ad-server`):** `AdServer::serve_ads(user_id)` delegates to the pluggable `AdsRanker` trait. Strategy **R1** uniformly samples up to **20 distinct campaigns** (`--slate-size 20`) from all campaigns that currently have `remaining_budget_cents >= cpc_cents` (returning fewer if fewer than 20 campaigns remain funded, or an empty slate when all budgets exhaust).
- **Shopper click evaluator (`ads-marketplace-simulation`):** For each served ad in the returned slate, a click occurs with probability **4%** if the ad belongs to one of the user's preferred categories (`score_preference == 1.0`) and **0%** otherwise (ChaCha8 Stream 2). Clicks deduct exact CPC via `AdServer::record_impression`, immediately retiring any campaign whose remaining budget drops below CPC in O(1) time.
- **30-day horizon:** With no impression target, `impression-generator` issues 600 requests/day across 30 days (12,000 impressions/day = 360,000 impressions when at least 20 campaigns remain funded). With `--impressions`, the target is split evenly across 30 days (remainder +1 to earliest days). If all budgets exhaust before a daily target is met, the run exits with an error.
- **Automated analysis (`simulation-analyzer`):** Computes budget depletion curves & exhaustion milestones, average/daily CTR & eCPM, campaign Gini concentration, top category/advertiser rollups, and diagnostic findings.
