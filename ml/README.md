# `ads-ml`: Impression-Click Logging & CTR Models (`Score-R1` & `Score-XGB`)

Standalone ML module combining **PyTorch** (`ml/pytorch/train_lr.py`) and **XGBoost** (`ml/xgboost/train_xgb.py`) training pipelines with a **Rust** logging, model-loading, and fast-scoring interface (`ads-ml` crate):

- **Daily impression-click disk logging** (`ImpressionLogStore`, `DailyImpressionWriter`, `ImpressionClickRecord`, `ImpressionClickRef`): writes one JSONL file per simulation day (`day-01.jsonl`, `day-02.jsonl`, …) and manages per-day model artifacts (`model-day-01.json`, …).
- **PyTorch logistic regression trainer (`Score-R1`)** (`ml/pytorch/train_lr.py`, `PyTorchTrainer`, `LogisticRegressionModel`, `CtrModel`, `TrainingConfig`, `TrainingSummary`): cold-trains a fresh `LogisticRegressionCTR` (`torch.nn.Module`) from scratch on all available impression-click data using exact sufficient statistics and deterministic `torch.optim.LBFGS` (Strong Wolfe line search). Runs either standalone via CLI or as a persistent `--serve-stdio` worker (so multi-day simulations pay `import torch` startup cost once).
- **XGBoost GBDT trainer (`Score-XGB`)** (`ml/xgboost/train_xgb.py`, `XgboostTrainer`, `XgboostCtrModel`, `XgboostTrainingConfig`): cold-trains a `binary:logistic` gradient-boosted decision tree ensemble on leak-free leave-one-out Bayesian-smoothed CTR & exposure features plus entity indices, exporting portable flat decision trees for zero-allocation native Rust inference. Also supports standalone CLI and persistent `--serve-stdio` worker mode.
- **Fast Rust candidate scoring** (`PreparedScorer`): loads either PyTorch linear weights (`Score-R1`) or XGBoost flat decision trees (`Score-XGB`) and pre-indexes them for low-latency scoring over fixed user, category, and campaign tables while matching `CtrModel::predict_ctr` exactly.

## Setup

Install CPU-only PyTorch and/or XGBoost (override the Python interpreter with `ADS_ML_PYTHON`):

```sh
pip install --index-url https://download.pytorch.org/whl/cpu -r ml/pytorch/requirements.txt
pip install -r ml/xgboost/requirements.txt
```

## Features

Both models train purely on observable impression-click fields (never accessing latent user preference profiles):

### `Score-R1` (PyTorch L2 Logistic Regression)
- Global intercept (unregularized)
- Contextual category match (`ad_category_id == organic_category_id`)
- One-hot embedding weights for `user_id`, `ad_category_id`, `advertiser_id`, `campaign_id`
- User × ad-category interaction (`(user_id, ad_category_id)`)

### `Score-XGB` (XGBoost GBDT)
- Contextual category match (`same_category`)
- Leave-one-out Bayesian-smoothed CTR (`(clicks_loo + m * p0) / (imp_loo + m)`) and `log1p(imp_loo)` exposure counts for `(user, ad_category)`, `user`, `ad_category`, `advertiser`, and `campaign`
- Encoded `ad_category_idx` and `advertiser_idx`

## Standalone CLI training

PyTorch (`Score-R1`):

```sh
python3 ml/pytorch/train_lr.py \
  --log-dir /tmp/ads-cpc/impressions-r1 \
  --through-day 10 \
  --model-out /tmp/ads-cpc/impressions-r1/model-day-10.json \
  --checkpoint-out /tmp/ads-cpc/impressions-r1/model-day-10.pt
```

XGBoost (`Score-XGB`):

```sh
python3 ml/xgboost/train_xgb.py \
  --log-dir /tmp/ads-cpc/impressions-xgb \
  --through-day 10 \
  --model-out /tmp/ads-cpc/impressions-xgb/model-day-10.json
```

## Verification

```sh
python3 -m unittest ml/pytorch/test_train_lr.py ml/xgboost/test_train_xgb.py -v
cargo test --locked --manifest-path ml/Cargo.toml
```
