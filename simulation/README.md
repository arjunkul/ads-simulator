# 30-day CPC simulation

From the repository root, generate the seeded marketplace and run the simulation.

```sh
mkdir -p /tmp/ads-cpc
cargo run --locked -- --seed 42 --output-dir /tmp/ads-cpc
cargo run --release --locked --manifest-path simulation/Cargo.toml -- \
  --marketplace /tmp/ads-cpc/marketplace.json \
  --output /tmp/ads-cpc/simulation.json \
  --impressions-output /tmp/ads-cpc/impressions.jsonl \
  --seed 42 --campaign-budget-cents 10000 --cpc-cents 100 \
  --opportunities 1000000 --pacing pacing-1 --bidding bidding-autobid-v0
cargo test --locked --manifest-path simulation/Cargo.toml
```

## Delivery model

- Each advertiser starts with ten campaigns. Each campaign uses one distinct ad item sampled from that advertiser's catalog with the simulation seed. In the baseline marketplace, this creates 500 campaigns.
- At setup, each campaign gets a fixed budget for the entire run, independent of the advertiser's daily budget field. Both the initial budget and the campaign's basebid / CPC (`--cpc-cents` / `--base-bid-cents`) are configurable in USD cents. The defaults are $100 per campaign and $1 basebid.
- Each organic browse event supplies one ad opportunity. Every generated opportunity receives a PST (`UTC-08:00`) timestamp across `2026-01-01` through `2026-01-30`, sampled from a diurnal distribution that peaks around 08:00 PST and dies out around 18:00 PST (split-normal with mode `08:00`, morning `σ = 1.8h`, afternoon `σ = 4.0h`, truncated to `[00:00, 24:00)`). Opportunities within each day are processed in chronological timestamp order.
- The simulator runs a 1st-price auction across eligible campaigns according to the selected bidding strategy, breaking ties uniformly at random. The organic item and its category have no effect on which ad is shown. Ad selection, organic replay order, click draws, and timestamp draws use separate streams from the same simulation seed.
- A click has probability 4% if the ad belongs to one of the user's preferred categories and 0% otherwise. On a click, the winning campaign pays its 1st-price click bid (`basebid * pacing_factor`, which is `$1` for `bidding-basebid` and binary `Pacing-1`). A campaign becomes ineligible when its remaining budget cannot afford another click.
- The run lasts 30 days. With no target flag, each day generates one timestamped opportunity per supplied browsing event. With `--impressions` (exact delivered impression target) or `--opportunities` (exact generated opportunity count), the target is split as evenly as possible across 30 days, giving any remainder to the earliest days one at a time, using shuffled passes over the browsing events. If `--impressions` cannot be fully met on a day, the run exits with an error.
- The same marketplace JSON, seed, and campaign settings reproduce the same report and impression log with the locked dependencies and same build target.

## Bidding strategies

Select a bidding strategy with `--bidding`:

- `bidding-basebid` (default): baseline bidder where every eligible campaign bids only its flat `basebid` (default `$1`, `100` cents), ignoring pCTR. Because all eligible campaigns share the same basebid, the 1st-price auction selects uniformly among eligible campaigns and charges `basebid` on clicks.
- `bidding-autobid-v0`: each campaign starts from its `basebid` (default `$1`) and submits a 1st-price auction bid of `final_bid = basebid * pCTR * pacing_factor`, where `pCTR` comes from the ranking model (`DefaultRankingModel` predicts `4%` for preferred categories and `0%` otherwise) and `pacing_factor` comes from the pacing strategy. Only strictly positive bids can win; the highest bidder wins (ties broken uniformly) and pays `basebid * pacing_factor` on clicks.

## Pacing strategies

Select a pacing strategy with `--pacing`:

- `pacing-1` (default): divides each campaign's total budget evenly across the 30 days (`budget / 30`, giving any remainder cents to the earliest days) and enforces a linear intraday spend schedule across the 24-hour PST day. `pacing_factor` is binary: `1.0` when the campaign can afford another click within both its remaining lifetime budget and today's daily budget and its cumulative daily spend does not exceed `daily_budget * (t / 86,400)`, and `0.0` (throttled until `t = ceil(daily_spend * 86,400 / daily_budget)` or the next day) otherwise.
- `none`: unpaced baseline (`pacing_factor = 1.0` whenever remaining lifetime budget can afford a click, `0.0` otherwise).

## Outputs

The JSON report contains the run configuration, `timezone` (`"PST"`), run totals, 24-hour PST aggregates (`hourly`), all 30 day summaries (each with `date_pst` and its own 24-hour PST `hourly` breakdown), and each campaign's ad item, upfront and remaining budgets, daily budgets (when paced), and daily impression, click, and spend counts. Opportunity counters reconcile delivered `impressions` with skipped slots: `pacing_throttled` (candidate campaigns waiting for intraday pacing unlock), `daily_budget_exhausted` (candidate campaigns with remaining total budget that hit today's daily budget), `insufficient_budget` (no candidate campaign can afford the CPC), and `no_matching_category` (under `bidding-autobid-v0`, no campaign in the marketplace had positive pCTR for the user).

When `--impressions-output` is supplied, every delivered impression is also written as an NDJSON record containing its RFC 3339 PST `timestamp` (e.g. `2026-01-01T08:14:23-08:00`), `day`, `user_id`, `campaign_id`, `ad_item_id`, `category_id`, `base_bid_cents`, `pctr`, `pacing_factor`, `final_bid_cents`, `click_price_cents`, `clicked`, and `spend_cents`.
