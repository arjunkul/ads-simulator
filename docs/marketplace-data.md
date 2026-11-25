# Synthetic marketplace data

Run the canonical Rust generator from the repository root with Rust 1.88 or later.

```sh
cargo run --locked -- --seed 42 --output-dir /tmp/ads-marketplace
cargo test --locked
```

The command writes `marketplace.json` to the requested directory and replaces that file on a successful rerun. Use the same signed 64-bit seed and the locked build for reproducible data. SimRuns in Rust can consume the export or call `ads_simulator::generate_marketplace(seed)` directly.

## Dataset contract

The top-level fields `schema_version`, `seed`, and `currency` identify the schema, random seed, and currency. All monetary amounts are integer US cents.

| Field | Contents |
|---|---|
| `categories` | 100 named product categories in ten groups. Each has a unique ID. |
| `advertisers` | 50 synthetic merchants with a specialty, daily budget, and 20 catalog items each. Their 1,000 catalog items share the taxonomy and have unique IDs. |
| `users` | 1,000 synthetic shoppers, each with an ID and a unique set of five preferred categories. |
| `organic_items` | 5,000 non-ad listings, 50 per category, with IDs distinct from advertiser catalog item IDs. |
| `browsing_events` | 12,000 organic listing views. Each event contains a user ID, an organic item ID, a one-based position within that user's sequence, and a reason. |

Older exports that include additional user or advertiser fields (such as legacy `objective`) remain readable by Rust. Newly generated user records contain only `id` and `preferred_category_ids`.

## Preference model

Use `score_preference(user, item)` for either an organic listing or an advertiser catalog item. An item in one of the user's preferred categories scores 1. All other items score 0.

Each user's browsing sequence contains nine distinct items sampled from preferred categories and three distinct items sampled from other categories. The generator shuffles the 12 selected items into browsing order using the same seed.
