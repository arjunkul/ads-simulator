# `ads-ml`: Impression-Click Logging & PyTorch Logistic Regression CTR Model

Standalone ML module combining a **PyTorch** training pipeline (`ml/pytorch/train_lr.py`) with a **Rust** logging, model-loading, and fast-scoring interface (`ads-ml` crate):

- **Daily impression-click disk logging** (`ImpressionLogStore`, `DailyImpressionWriter`, `ImpressionClickRecord`, `ImpressionClickRef`): writes one JSONL file per simulation day (`day-01.jsonl`, `day-02.jsonl`, …) and manages per-day model artifacts (`model-day-01.json`, …).
- **PyTorch logistic regression trainer (`Score-R1`)** (`ml/pytorch/train_lr.py`, `PyTorchTrainer`, `LogisticRegressionModel`, `CtrModel`, `TrainingConfig`, `TrainingSummary`): cold-trains a fresh `LogisticRegressionCTR` (`torch.nn.Module`) from scratch on all available impression-click data on disk using exact sufficient statistics and deterministic `torch.optim.LBFGS` (Strong Wolfe line search). Runs either standalone via CLI or as a persistent `--serve-stdio` worker (so multi-day simulations pay `import torch` startup cost once).
- **Fast Rust candidate scoring** (`PreparedScorer`): loads PyTorch-exported weights and pre-indexes them for low-latency scoring over fixed user, category, and campaign tables while matching `CtrModel::predict_ctr` exactly.

## Setup

Install CPU-only PyTorch (or override the Python interpreter with `ADS_ML_PYTHON`):

```sh
pip install --index-url https://download.pytorch.org/whl/cpu -r ml/pytorch/requirements.txt
```

## Features

The model trains purely on observable impression-click fields (never accessing latent user preference profiles):

- Global intercept (unregularized)
- Contextual category match (`ad_category_id == organic_category_id`)
- One-hot embedding weights for `user_id`, `ad_category_id`, `advertiser_id`, `campaign_id`
- User × ad-category interaction (`(user_id, ad_category_id)`)

## Standalone CLI training

```sh
python3 ml/pytorch/train_lr.py \
  --log-dir /tmp/ads-cpc/impressions-r1 \
  --through-day 10 \
  --model-out /tmp/ads-cpc/impressions-r1/model-day-10.json \
  --checkpoint-out /tmp/ads-cpc/impressions-r1/model-day-10.pt
```

## Verification

```sh
python3 -m unittest ml/pytorch/test_train_lr.py -v
cargo test --locked --manifest-path ml/Cargo.toml
```
