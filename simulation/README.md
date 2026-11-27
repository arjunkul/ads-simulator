# Ads Marketplace Simulation

Runs a seeded 30-day click-only CPC simulation with an explicit 5-axis ad-serving strategy stack:

1. **Retrieval (`--retrieval` / `--retrieval-strategy`)**: `Retrieve-V0` (O(20) sparse-set index uniformly sampling up to 20 funded campaigns per request).
2. **Pacing (`--pacing`)**: `P0` (`none`), `P1` (even daily cap), `P2` (smooth probabilistic), `pacing-1` (24-hour PST diurnal linear schedule).
3. **Bidding (`--bidding`)**: `bidding-basebid` (`B0`), `bidding-autobid-v0` (`B1`, 1st-price auction `basebid * pCTR * pacing_factor`).
4. **Scoring (`--scoring` / `--scoring-strategy`)**: `S0` (`Score-Uniform`), `S1` (Online UCB1 CTR), `S2` (Empirical CTR), `S3` (Empirical eCPM), `Score-R1` (daily cold-trained PyTorch L2 logistic regression via `ads-ml`), `Score-XGB` / `S5` (daily cold-trained XGBoost GBDT via `ads-ml`).
5. **Ranking (`--ranker` / `--ranking-strategy`)**: `R1` (`Rank-Uniform`), `R2` (Bandit Top-K + 10% ε-greedy), `R3` (`Rank-Greedy`).
6. **World Model (`--world-model`)**: `world-model-v0` (`v0`, binary 4% preferred / 0% non-preferred baseline) or `world-model-v1` (`v1`, noisy hierarchical logit oracle injecting static CTR shifts and varying per-impression Gaussian noise per retail group, category, and advertiser combo, with noisy non-preferred background CTR).

## Run

```bash
cargo run --release -p ads-marketplace-simulation -- \
  --marketplace output/marketplace.json \
  --output output/simulation.json \
  --world-model world-model-v1 \
  --retrieval Retrieve-V0 \
  --ranker R2 \
  --scoring S1 \
  --pacing pacing-1 \
  --bidding bidding-autobid-v0 \
  --impressions 1000000
```
