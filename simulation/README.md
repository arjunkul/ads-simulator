# Ads Marketplace Simulation

Runs a seeded 30-day click-only CPC simulation with an explicit 5-axis ad-serving strategy stack:

1. **Retrieval (`--retrieval` / `--retrieval-strategy`)**: `Retrieve-V0` (O(1) sparse-set index of funded campaigns).
2. **Pacing (`--pacing`)**: `P0` (`none`), `P1` (even daily cap), `P2` (smooth probabilistic), `pacing-1` (24-hour PST diurnal linear schedule).
3. **Bidding (`--bidding`)**: `bidding-basebid` (`B0`), `bidding-autobid-v0` (`B1`, 1st-price auction `basebid * pCTR * pacing_factor`).
4. **Scoring (`--scoring` / `--scoring-strategy`)**: `S0` (`Score-Uniform`), `S1` (Online UCB1 CTR), `S2` (Empirical CTR), `S3` (Empirical eCPM), `Score-R1` (daily cold-trained PyTorch L2 logistic regression via `ads-ml`).
5. **Ranking (`--ranker` / `--ranking-strategy`)**: `R1` (`Rank-Uniform`), `R2` (Bandit Top-K + 10% ε-greedy), `R3` (`Rank-Greedy`).

## Run

```bash
cargo run --release -p ads-marketplace-simulation -- \
  --marketplace output/marketplace.json \
  --output output/simulation.json \
  --retrieval Retrieve-V0 \
  --ranker R2 \
  --scoring S1 \
  --pacing pacing-1 \
  --bidding bidding-autobid-v0 \
  --impressions 1000000
```
