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
  --impressions 1000000 --pacing pacing-1
cargo test --locked --manifest-path simulation/Cargo.toml
```

## Delivery model

- Each advertiser starts with ten campaigns. Each campaign uses one distinct ad item sampled from that advertiser's catalog with the simulation seed. In the baseline marketplace, this creates 500 campaigns.
- At setup, each campaign gets a fixed budget for the entire run, independent of the advertiser's daily budget field. Both the initial budget and the campaign's CPC are configurable in USD cents. The defaults are $100 per campaign and $1 per click.
- Each organic browse event supplies one ad opportunity. Every generated opportunity receives a PST (`UTC-08:00`) timestamp across `2026-01-01` through `2026-01-30`, sampled from a diurnal distribution that peaks around 08:00 PST and dies out around 18:00 PST (split-normal with mode `08:00`, morning `σ = 1.8h`, afternoon `σ = 4.0h`, truncated to `[00:00, 24:00)`). Opportunities within each day are processed in chronological timestamp order.
- The simulator selects at most one campaign uniformly from all campaigns eligible at that timestamp. The organic item and its category have no effect on which ad is shown. User preferences affect clicks only. Ad selection, organic replay order, click draws, and timestamp draws use separate streams from the same simulation seed.
- A click has probability 4% if the ad belongs to one of the user's preferred categories and 0% otherwise. On a click, the campaign pays exactly the CPC. A campaign becomes ineligible when its remaining budget cannot afford another click.
- The run lasts 30 days. With no target flag, each day generates one timestamped opportunity per supplied browsing event. With `--impressions` (exact delivered impression target) or `--opportunities` (exact generated opportunity count), the target is split as evenly as possible across 30 days, giving any remainder to the earliest days one at a time, using shuffled passes over the browsing events. If `--impressions` cannot be fully met on a day, the run exits with an error.
- The same marketplace JSON, seed, and campaign settings reproduce the same report and impression log with the locked dependencies and same build target.

## Pacing strategies

Select a pacing strategy with `--pacing`:

- `pacing-1` (default): divides each campaign's total budget evenly across the 30 days (`budget / 30`, giving any remainder cents to the earliest days) and enforces a linear intraday spend schedule across the 24-hour PST day. At second `t` of the day (`0..86,400`), a campaign is eligible only if it can afford another click within both its remaining lifetime budget and today's daily budget, and its cumulative daily spend does not exceed `daily_budget * (t / 86,400)`. After a click puts it ahead of pace, it is throttled until `t = ceil(daily_spend * 86,400 / daily_budget)`.
- `none`: unpaced baseline; any campaign whose remaining lifetime budget can afford a click is eligible at any time.

## Outputs

The JSON report contains the run configuration, `timezone` (`"PST"`), run totals, 24-hour PST aggregates (`hourly`), all 30 day summaries (each with `date_pst` and its own 24-hour PST `hourly` breakdown), and each campaign's ad item, upfront and remaining budgets, daily budgets (when paced), and daily impression, click, and spend counts. Opportunity counters reconcile delivered `impressions` with skipped slots: `pacing_throttled` (campaigns waiting for intraday pacing unlock), `daily_budget_exhausted` (campaigns with remaining total budget that hit today's daily budget), `insufficient_budget` (no campaign can afford the CPC), and legacy `no_matching_category` (always zero).

When `--impressions-output` is supplied, every delivered impression is also written as an NDJSON record containing its RFC 3339 PST `timestamp` (e.g. `2026-01-01T08:14:23-08:00`), `day`, `user_id`, `campaign_id`, `ad_item_id`, `category_id`, `clicked`, and `spend_cents`.
