# Synthetic marketplace data

Run the canonical Rust generator from the repository root with Rust 1.88 or later.

```sh
cargo run --locked -- --seed 42 --output-dir /tmp/ads-marketplace
cargo test --locked
```

The command writes `marketplace.json` to the requested directory and replaces that file on a successful rerun. Use the same signed 64-bit seed and the locked build for reproducible data. SimRuns in Rust can consume the export or call `ads_simulator::generate_marketplace(seed)` directly.

The original Python generator (`python3 marketplace_data.py --seed 42 --output-dir /tmp/ads-marketplace`) remains available for existing callers. It exports the same baseline contract without the new preference profiles or ranking. Random realizations differ between the implementations.

## Dataset contract

The top-level fields `schema_version`, `seed`, and `currency` identify the schema, random seed, and currency. All monetary amounts are integer US cents.

| Field | Contents |
|---|---|
| `categories` | 100 named product categories in ten groups. Each has a unique ID. |
| `advertisers` | 50 synthetic merchants with a specialty, objective, daily budget, and 20 catalog items each. Their 1,000 catalog items share the taxonomy and have unique IDs. |
| `users` | 1,000 synthetic shoppers with distinct IDs, five preferred categories each, a descriptive shopping style, and an individual preference profile. |
| `organic_items` | 5,000 non-ad listings, 50 per category, with IDs distinct from advertiser catalog item IDs. |
| `browsing_events` | 12,000 organic listing views. Each event contains a user ID, an organic item ID, a one-based position within that user's sequence, and a reason. |

Rust adds an optional `preference` object to the schema v1 user record. It contains `item_budget_cents`, `price_sensitivity_pct`, and `ad_receptiveness_pct`. Generated profiles are distinct. When loading an earlier export, Rust uses a $100 item budget, 50% price sensitivity, and 50% ad receptiveness for a missing profile.

## Preference model

Call `score_preference(user, ScoredItem::Organic(item), weights)` for an organic listing or `score_preference(user, ScoredItem::Ad(item), weights)` for an advertiser catalog item. The score is a weighted average in the range 0 to 1 for ranking. It is not a calibrated probability.

| Signal | Default weight | Behavior |
|---|---:|---|
| Category affinity | 5 | One for a preferred category and zero otherwise. |
| Affordability | 3 | One up to the user's budget. Above it, the budget-to-price ratio and the user's price sensitivity determine the penalty. |
| Item variation | 2 | A reproducible user–item signal derived from the two IDs. |
| Ad receptiveness | 2 | The user's ad receptiveness, applied only when scoring an ad. |

Set weights with `PreferenceWeights`. Zero disables a signal, and a score with no enabled signals is 0.5.

Each user views nine distinct items in preferred categories and three distinct items outside them. The generator samples a slate three times the size of each group, keeps its highest-scoring organic items, and shuffles the selected 12 views into browsing order. The dataset contains organic views only. SimRuns can score ads separately.
