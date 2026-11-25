# `ads-ml`: Impression-Click Logging & Logistic Regression CTR Model

Standalone crate providing:

- **Daily impression-click disk logging** (`ImpressionLogStore`, `DailyImpressionWriter`, `ImpressionClickRecord`, `ImpressionClickRef`): writes one JSONL file per simulation day (`day-01.jsonl`, `day-02.jsonl`, …) and streams records across days `1..=d` with minimal allocations.
- **L2-regularized logistic regression CTR model** (`LogisticRegressionModel`, `CtrModel`, `TrainingConfig`, `TrainingSummary`): cold-trains from scratch on all available impression-click data using exact sufficient statistics and deterministic L-BFGS optimization.
- **Fast candidate ranking** (`PreparedRanker`): pre-indexes learned weights for repeated low-latency scoring over fixed user, category, and campaign tables while matching `CtrModel::predict_ctr` exactly.

## Features

The model trains purely on observable impression-click fields (never accessing latent user preference profiles):

- Global intercept (unregularized)
- Contextual category match (`ad_category_id == organic_category_id`)
- One-hot indicators for `user_id`, `ad_category_id`, `advertiser_id`, `campaign_id`
- User × ad-category interaction (`(user_id, ad_category_id)`)

## Verification

```sh
cargo test --locked --manifest-path ml/Cargo.toml
```
