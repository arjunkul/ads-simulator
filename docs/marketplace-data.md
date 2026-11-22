# Synthetic marketplace data

Run this from the repository root with Python 3.10 or later. The generator uses only the standard library.

```sh
python3 marketplace_data.py --seed 42 --output-dir /tmp/ads-marketplace
python3 -m unittest discover -s tests -v
```

The command writes `marketplace.json` to the requested directory and replaces that file on a rerun. Use the same seed and Python version to reproduce an export. To consume the data in a SimRun without a file, import `generate_marketplace(seed)`.

## Dataset contract

The top-level fields `schema_version`, `seed`, and `currency` identify the schema, random seed, and currency. All monetary amounts are integer US cents.

| Field | Contents |
|---|---|
| `categories` | 100 named product categories in ten groups. Each has a unique ID. |
| `advertisers` | 50 synthetic merchants with a specialty, objective, daily budget, and 20 catalog items each. Their 1,000 catalog items share the taxonomy and have unique IDs. |
| `users` | 1,000 synthetic shoppers with distinct IDs, five preferred categories each, and a shopping-style label. The label is descriptive in this baseline. |
| `organic_items` | 5,000 non-ad listings, 50 per category, with IDs distinct from advertiser catalog item IDs. |
| `browsing_events` | 12,000 organic listing views. Each event contains a user ID, an organic item ID, a one-based position within that user's sequence, and a reason. |

The generator models one 12-view browsing sequence per user. Each sequence contains nine distinct listings from preferred categories and three distinct listings outside those categories, shuffled into viewing order. The baseline contains views only. Shopping outcomes and marketplace evolution can be modeled by the SimRun.
