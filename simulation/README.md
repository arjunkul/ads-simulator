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

## Delivery model

- Each advertiser starts with ten campaigns. Each campaign uses one distinct ad item sampled from that advertiser's catalog with the simulation seed. In the baseline marketplace, this creates 500 campaigns.
- At setup, each campaign gets a fixed budget for the entire run, independent of the advertiser's daily budget field. Both the initial budget and the campaign's CPC are configurable in USD cents. The defaults are $100 per campaign and $1 per click.
- Each organic browse event supplies one ad opportunity. The simulator selects at most one campaign uniformly from all campaigns that can afford the CPC. The organic item and its category have no effect on which ad is shown. User preferences affect clicks only. Organic replay, ad selection, and click draws use separate streams from the same simulation seed.
- A click has probability 4% if the ad belongs to one of the user's preferred categories and 0% otherwise. On a click, the campaign pays exactly the CPC. A campaign becomes ineligible when its remaining budget cannot afford another click.
- The run lasts 30 days. With no impression target, it replays the supplied browsing events once each day in their original order. With a target, it splits the target as evenly as possible across 30 days, giving any remainder to the earliest days one impression at a time. It shuffles the browsing events for each pass and repeats as needed to meet the daily quota. If a pass delivers zero impressions before the quota is met, the run exits with an error.
- The same marketplace JSON, seed, and campaign settings reproduce the same report with the locked dependencies and same build target.

The JSON report contains the run configuration, totals, all 30 day summaries, and each campaign's ad item, upfront and remaining budgets, and daily impression, click, and spend counts. The daily and whole-run opportunity counters include both delivered impressions and skipped slots. `insufficient_budget` counts slots for which no campaign can afford the CPC. The legacy `no_matching_category` counter stays at zero.
