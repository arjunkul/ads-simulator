//! Pluggable ads-ranker strategies for [`crate::AdServer`].
//!
//! Ranking strategies govern how scored, paced-eligible funded campaigns are assembled into an
//! ordered ad slate:
//! - [`RankerStrategy::R1`]: Uniform random sampling without replacement (baseline; requires `S0`).
//! - [`RankerStrategy::R2`]: Cold-start priority + Top-K by candidate score + 10% ε-greedy exploration.
//! - [`RankerStrategy::R3`]: Cold-start priority + deterministic Greedy Top-K by candidate score (0% ε-greedy).

use std::fmt;
use std::str::FromStr;

use ads_ml::PreparedScorer;
use rand::Rng;
use rand::seq::{SliceRandom, index};
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};

use crate::{BiddingStrategy, CampaignState, DEFAULT_CPC_CENTS, ScoringStrategy, compute_bid};

/// Default number of distinct funded campaigns returned per ad-server request under Strategy R1.
pub const R1_SLATE_SIZE: usize = 20;
/// Default number of distinct funded campaigns returned per ad-server request under Strategy R2.
pub const R2_SLATE_SIZE: usize = 20;
/// Default number of distinct funded campaigns returned per ad-server request under Strategy R3.
pub const R3_SLATE_SIZE: usize = 20;
/// Per-slot ε-greedy uniform exploration probability for Strategy R2 (10%).
pub const R2_EPSILON: f64 = 0.10;
/// Low-CTR-calibrated UCB1 exploration coefficient for Scoring S1.
pub const R2_UCB_EXPLORATION_C: f64 = 0.02;

/// Optional request-time ML scoring and bidding context passed from [`crate::AdServer`].
pub struct ServingContext<'a> {
    pub user_index: Option<usize>,
    pub ml_scorer: Option<&'a PreparedScorer>,
    pub bidding: BiddingStrategy,
}

/// Supported ads-ranker (slate selection) strategies.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RankerStrategy {
    /// Strategy R1 (`Rank-Uniform`): select up to 20 distinct random campaigns that currently have budget.
    #[default]
    #[serde(rename = "R1", alias = "r1", alias = "Rank-Uniform", alias = "rank-uniform", alias = "random")]
    R1,
    /// Strategy R2: cold-start priority + Top-K by score with 10% ε-greedy exploration.
    #[serde(rename = "R2", alias = "r2", alias = "eps_greedy")]
    R2,
    /// Strategy R3 (`Rank-Greedy`): cold-start priority + pure Greedy Top-K by score (0% ε-greedy).
    #[serde(rename = "R3", alias = "r3", alias = "Rank-Greedy", alias = "rank-greedy", alias = "greedy")]
    R3,
}

impl RankerStrategy {
    /// All compiled ranker strategies in registration order.
    pub const fn all() -> &'static [RankerStrategy] {
        &[RankerStrategy::R1, RankerStrategy::R2, RankerStrategy::R3]
    }

    /// Canonical short identifier (e.g. `"R1"`, `"R2"`, `"R3"`).
    pub const fn as_str(self) -> &'static str {
        match self {
            RankerStrategy::R1 => "R1",
            RankerStrategy::R2 => "R2",
            RankerStrategy::R3 => "R3",
        }
    }

    /// Short human-readable strategy name.
    pub const fn name(self) -> &'static str {
        match self {
            RankerStrategy::R1 => "Random Budgeted Selector (Rank-Uniform)",
            RankerStrategy::R2 => "Bandit Top-K + ε-Greedy",
            RankerStrategy::R3 => "Greedy Top-K (Rank-Greedy)",
        }
    }

    /// Human-readable description of the strategy.
    pub const fn description(self) -> &'static str {
        match self {
            RankerStrategy::R1 => {
                "Select up to 20 random distinct campaigns with remaining budget >= CPC"
            }
            RankerStrategy::R2 => {
                "Cold-start priority, then Top-K by candidate score with 10% per-slot ε-greedy exploration"
            }
            RankerStrategy::R3 => {
                "Cold-start priority, then strictly Top-K by candidate score (0% ε-greedy exploration)"
            }
        }
    }

    /// Concise slate-selection formula for dashboards and documentation.
    pub const fn formula(self) -> &'static str {
        match self {
            RankerStrategy::R1 => {
                "Uniform sample without replacement of min(K, |eligible|) + Fisher-Yates shuffle"
            }
            RankerStrategy::R2 => {
                "Cold-start (imp=0) -> Top-(K-m) by score + m ~ Bin(K, ε=0.10) uniform explore slots"
            }
            RankerStrategy::R3 => {
                "Cold-start (imp=0) -> Top-K strictly by score (random tie-breaking, ε=0)"
            }
        }
    }

    /// Default scoring strategy paired with this ranker when `--scoring` is omitted.
    pub const fn default_scoring(self) -> ScoringStrategy {
        match self {
            RankerStrategy::R1 => ScoringStrategy::S0,
            RankerStrategy::R2 | RankerStrategy::R3 => ScoringStrategy::S1,
        }
    }

    /// Default slate size for this strategy.
    pub const fn default_slate_size(self) -> usize {
        match self {
            RankerStrategy::R1 => R1_SLATE_SIZE,
            RankerStrategy::R2 => R2_SLATE_SIZE,
            RankerStrategy::R3 => R3_SLATE_SIZE,
        }
    }

    /// Construct the boxed [`AdsRanker`] implementation for this strategy using its default scorer.
    pub fn build(self, slate_size: usize) -> Box<dyn AdsRanker> {
        self.build_with_scoring(slate_size, self.default_scoring(), DEFAULT_CPC_CENTS)
    }

    /// Construct the boxed [`AdsRanker`] implementation wired with an explicit [`ScoringStrategy`].
    pub fn build_with_scoring(
        self,
        slate_size: usize,
        scoring: ScoringStrategy,
        cpc_cents: u64,
    ) -> Box<dyn AdsRanker> {
        match self {
            RankerStrategy::R1 => Box::new(R1RandomBudgetedRanker::new(slate_size)),
            RankerStrategy::R2 => {
                Box::new(R2OnlineCtrRanker::with_scoring(slate_size, scoring, cpc_cents))
            }
            RankerStrategy::R3 => {
                Box::new(R3GreedyTopKRanker::with_scoring(slate_size, scoring, cpc_cents))
            }
        }
    }
}

impl fmt::Display for RankerStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for RankerStrategy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_uppercase().as_str() {
            "R1" | "RANK-UNIFORM" | "UNIFORM" | "RANDOM" => Ok(RankerStrategy::R1),
            "R2" | "EPS_GREEDY" | "BANDIT" => Ok(RankerStrategy::R2),
            "R3" | "RANK-GREEDY" | "GREEDY" | "TOP_K" => Ok(RankerStrategy::R3),
            other => Err(format!(
                "unknown ranker strategy '{other}' (supported: R1/Rank-Uniform, R2/eps_greedy, R3/Rank-Greedy)"
            )),
        }
    }
}

/// Pluggable ranking/selection strategy invoked by [`crate::AdServer::serve_ads`].
///
/// Implementations receive the requesting `user_id`, the list of currently eligible (retrieved &
/// paced) campaign indices, read-only campaign states, and the ad-server's deterministic RNG
/// stream, and return an ordered list of distinct campaign indices to serve.
pub trait AdsRanker: Send + Sync {
    /// Strategy identifier reported in [`crate::AdResponse`] and metadata.
    fn strategy(&self) -> RankerStrategy;

    /// Maximum number of distinct campaign ads returned per request.
    fn slate_size(&self) -> usize;

    /// Select up to `slate_size()` distinct campaign indices from `eligible_indices`.
    fn select_campaigns(
        &self,
        user_id: &str,
        eligible_indices: &[usize],
        campaigns: &[CampaignState],
        rng: &mut ChaCha8Rng,
    ) -> Vec<usize>;

    /// Select campaigns with optional request-time ML scorer (`Score-R1`) and bidding context.
    fn select_campaigns_with_context(
        &self,
        user_id: &str,
        eligible_indices: &[usize],
        campaigns: &[CampaignState],
        _ctx: &ServingContext<'_>,
        rng: &mut ChaCha8Rng,
    ) -> Vec<usize> {
        self.select_campaigns(user_id, eligible_indices, campaigns, rng)
    }
}

/// Strategy R1: selects up to `slate_size` (default 20) distinct campaigns uniformly at random
/// from the set of currently funded, paced-eligible campaigns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct R1RandomBudgetedRanker {
    slate_size: usize,
}

impl R1RandomBudgetedRanker {
    /// Create a new R1 ranker returning up to `slate_size` distinct funded campaigns.
    pub const fn new(slate_size: usize) -> Self {
        Self { slate_size }
    }
}

impl Default for R1RandomBudgetedRanker {
    fn default() -> Self {
        Self::new(R1_SLATE_SIZE)
    }
}

impl AdsRanker for R1RandomBudgetedRanker {
    fn strategy(&self) -> RankerStrategy {
        RankerStrategy::R1
    }

    fn slate_size(&self) -> usize {
        self.slate_size
    }

    fn select_campaigns(
        &self,
        _user_id: &str,
        eligible_indices: &[usize],
        _campaigns: &[CampaignState],
        rng: &mut ChaCha8Rng,
    ) -> Vec<usize> {
        let count = self.slate_size.min(eligible_indices.len());
        if count == 0 {
            return Vec::new();
        }
        let mut positions = index::sample(rng, eligible_indices.len(), count).into_vec();
        positions.shuffle(rng);
        positions
            .into_iter()
            .map(|pos| eligible_indices[pos])
            .collect()
    }
}

/// Strategy R2: cold-start priority + Top-K selection by [`ScoringStrategy`] (default `S1` UCB1
/// CTR) combined with per-slot ε-greedy uniform exploration (default `ε = 0.10`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct R2OnlineCtrRanker {
    slate_size: usize,
    epsilon: f64,
    exploration_c: f64,
    scoring: ScoringStrategy,
    cpc_cents: u64,
}

impl R2OnlineCtrRanker {
    /// Create a new R2 online CTR learner with default `S1` scoring, ε = 0.10, and UCB1 c = 0.02.
    pub const fn new(slate_size: usize) -> Self {
        Self {
            slate_size,
            epsilon: R2_EPSILON,
            exploration_c: R2_UCB_EXPLORATION_C,
            scoring: ScoringStrategy::S1,
            cpc_cents: DEFAULT_CPC_CENTS,
        }
    }

    /// Create an R2 ranker using an explicit [`ScoringStrategy`] and `cpc_cents`.
    pub const fn with_scoring(
        slate_size: usize,
        scoring: ScoringStrategy,
        cpc_cents: u64,
    ) -> Self {
        Self {
            slate_size,
            epsilon: R2_EPSILON,
            exploration_c: R2_UCB_EXPLORATION_C,
            scoring,
            cpc_cents,
        }
    }

    /// Create an R2 ranker with custom ε-greedy probability and UCB1 exploration coefficient.
    pub const fn with_params(slate_size: usize, epsilon: f64, exploration_c: f64) -> Self {
        Self {
            slate_size,
            epsilon,
            exploration_c,
            scoring: ScoringStrategy::S1,
            cpc_cents: DEFAULT_CPC_CENTS,
        }
    }
}

impl Default for R2OnlineCtrRanker {
    fn default() -> Self {
        Self::new(R2_SLATE_SIZE)
    }
}

/// Strategy R3: cold-start priority + deterministic Greedy Top-K selection strictly by
/// [`ScoringStrategy`] (0% ε-greedy exploration, random tie-breaking).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct R3GreedyTopKRanker {
    slate_size: usize,
    exploration_c: f64,
    scoring: ScoringStrategy,
    cpc_cents: u64,
}

impl R3GreedyTopKRanker {
    /// Create a new R3 greedy top-K ranker using default `S1` UCB1 CTR scoring.
    pub const fn new(slate_size: usize) -> Self {
        Self::with_scoring(slate_size, ScoringStrategy::S1, DEFAULT_CPC_CENTS)
    }

    /// Create an R3 greedy top-K ranker using an explicit [`ScoringStrategy`] and `cpc_cents`.
    pub const fn with_scoring(
        slate_size: usize,
        scoring: ScoringStrategy,
        cpc_cents: u64,
    ) -> Self {
        Self {
            slate_size,
            exploration_c: R2_UCB_EXPLORATION_C,
            scoring,
            cpc_cents,
        }
    }
}

impl Default for R3GreedyTopKRanker {
    fn default() -> Self {
        Self::new(R3_SLATE_SIZE)
    }
}

#[allow(clippy::too_many_arguments)]
fn select_scored_slate(
    slate_size: usize,
    epsilon: f64,
    exploration_c: f64,
    scoring: ScoringStrategy,
    cpc_cents: u64,
    eligible_indices: &[usize],
    campaigns: &[CampaignState],
    ctx: Option<&ServingContext<'_>>,
    rng: &mut ChaCha8Rng,
) -> Vec<usize> {
    let target = slate_size.min(eligible_indices.len());
    if target == 0 {
        return Vec::new();
    }

    let bidding = ctx.map(|c| c.bidding).unwrap_or(BiddingStrategy::Basebid);
    let mut selected = Vec::with_capacity(target);

    // When Score-R1 (PyTorch logistic regression) is active, score all eligible campaigns directly
    // via the daily trained PreparedScorer (or uniform cold-start on Day 1 before any model exists).
    let (observed, total_impressions) = if scoring == ScoringStrategy::ScoreR1 {
        (eligible_indices.to_vec(), 1u64)
    } else {
        let mut cold_start = Vec::new();
        let mut observed = Vec::new();
        let mut total_impressions = 0u64;

        for &idx in eligible_indices {
            let imp = campaigns
                .get(idx)
                .map(|c| c.totals.impressions)
                .unwrap_or(0);
            total_impressions = total_impressions.saturating_add(imp);
            if imp == 0 {
                cold_start.push(idx);
            } else {
                observed.push(idx);
            }
        }

        // 1. Cold-start pass: uniformly explore unvisited funded campaigns first.
        if !cold_start.is_empty() {
            let take_cold = target.min(cold_start.len());
            let mut positions = index::sample(rng, cold_start.len(), take_cold).into_vec();
            positions.shuffle(rng);
            for pos in positions {
                selected.push(cold_start[pos]);
            }
        }

        (observed, total_impressions)
    };

    let remaining_slots = target.saturating_sub(selected.len());
    if remaining_slots == 0 || observed.is_empty() {
        return selected;
    }

    // 2. Score observed funded campaigns via the active ScoringStrategy + BiddingStrategy.
    let ln_t = (total_impressions.max(1) as f64).ln().max(0.0);
    let ml_pair = ctx.and_then(|c| c.ml_scorer.zip(c.user_index));
    let mut scored: Vec<(usize, f64, u64)> = observed
        .into_iter()
        .map(|idx| {
            let raw_score = if scoring == ScoringStrategy::ScoreR1 {
                match ml_pair {
                    Some((scorer, user_idx)) => scorer.score_ctr(user_idx, 0, idx),
                    None => 1.0,
                }
            } else {
                scoring.score_observed(&campaigns[idx], ln_t, cpc_cents, exploration_c)
            };
            let rank_score = match bidding {
                BiddingStrategy::Basebid => raw_score,
                BiddingStrategy::AutobidV0 => {
                    compute_bid(BiddingStrategy::AutobidV0, cpc_cents, raw_score, 1.0)
                        .final_bid_cents
                }
            };
            let tie_breaker: u64 = rng.random();
            (idx, rank_score, tie_breaker)
        })
        .collect();

    // 3. Split remaining slots into ε-greedy explore slots vs Top-K exploit slots.
    let mut explore_slots = 0usize;
    if epsilon > 0.0 {
        let clamped_eps = epsilon.clamp(0.0, 1.0);
        for _ in 0..remaining_slots {
            if rng.random_bool(clamped_eps) {
                explore_slots += 1;
            }
        }
    }
    let exploit_slots = remaining_slots - explore_slots;

    scored.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| a.0.cmp(&b.0))
    });

    let exploit_take = exploit_slots.min(scored.len());
    for entry in scored.iter().take(exploit_take) {
        selected.push(entry.0);
    }

    let pool = &scored[exploit_take..];
    let explore_take = explore_slots.min(pool.len());
    if explore_take > 0 {
        let mut positions = index::sample(rng, pool.len(), explore_take).into_vec();
        positions.shuffle(rng);
        for pos in positions {
            selected.push(pool[pos].0);
        }
    }

    selected
}

impl AdsRanker for R2OnlineCtrRanker {
    fn strategy(&self) -> RankerStrategy {
        RankerStrategy::R2
    }

    fn slate_size(&self) -> usize {
        self.slate_size
    }

    fn select_campaigns(
        &self,
        _user_id: &str,
        eligible_indices: &[usize],
        campaigns: &[CampaignState],
        rng: &mut ChaCha8Rng,
    ) -> Vec<usize> {
        select_scored_slate(
            self.slate_size,
            self.epsilon,
            self.exploration_c,
            self.scoring,
            self.cpc_cents,
            eligible_indices,
            campaigns,
            None,
            rng,
        )
    }

    fn select_campaigns_with_context(
        &self,
        _user_id: &str,
        eligible_indices: &[usize],
        campaigns: &[CampaignState],
        ctx: &ServingContext<'_>,
        rng: &mut ChaCha8Rng,
    ) -> Vec<usize> {
        select_scored_slate(
            self.slate_size,
            self.epsilon,
            self.exploration_c,
            self.scoring,
            self.cpc_cents,
            eligible_indices,
            campaigns,
            Some(ctx),
            rng,
        )
    }
}

impl AdsRanker for R3GreedyTopKRanker {
    fn strategy(&self) -> RankerStrategy {
        RankerStrategy::R3
    }

    fn slate_size(&self) -> usize {
        self.slate_size
    }

    fn select_campaigns(
        &self,
        _user_id: &str,
        eligible_indices: &[usize],
        campaigns: &[CampaignState],
        rng: &mut ChaCha8Rng,
    ) -> Vec<usize> {
        select_scored_slate(
            self.slate_size,
            0.0,
            self.exploration_c,
            self.scoring,
            self.cpc_cents,
            eligible_indices,
            campaigns,
            None,
            rng,
        )
    }

    fn select_campaigns_with_context(
        &self,
        _user_id: &str,
        eligible_indices: &[usize],
        campaigns: &[CampaignState],
        ctx: &ServingContext<'_>,
        rng: &mut ChaCha8Rng,
    ) -> Vec<usize> {
        select_scored_slate(
            self.slate_size,
            0.0,
            self.exploration_c,
            self.scoring,
            self.cpc_cents,
            eligible_indices,
            campaigns,
            Some(ctx),
            rng,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::str::FromStr;

    use world_generator::generate_marketplace;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    use super::{
        AdsRanker, R1_SLATE_SIZE, R1RandomBudgetedRanker, R2_SLATE_SIZE, R2OnlineCtrRanker,
        R3_SLATE_SIZE, R3GreedyTopKRanker, RankerStrategy,
    };
    use crate::{AdServer, AdServerConfig, PacingStrategy, ScoringStrategy};

    #[test]
    fn strategy_parsing_display_and_json_round_trip() {
        assert_eq!(RankerStrategy::from_str("R1").unwrap(), RankerStrategy::R1);
        assert_eq!(RankerStrategy::from_str("r1").unwrap(), RankerStrategy::R1);
        assert_eq!(RankerStrategy::from_str("R2").unwrap(), RankerStrategy::R2);
        assert_eq!(RankerStrategy::from_str("r2").unwrap(), RankerStrategy::R2);
        assert_eq!(RankerStrategy::from_str("R3").unwrap(), RankerStrategy::R3);
        assert_eq!(RankerStrategy::from_str("greedy").unwrap(), RankerStrategy::R3);
        assert!(RankerStrategy::from_str("R99").is_err());
        assert_eq!(RankerStrategy::R1.to_string(), "R1");
        assert_eq!(RankerStrategy::R2.to_string(), "R2");
        assert_eq!(RankerStrategy::R3.to_string(), "R3");
        assert_eq!(RankerStrategy::R1.default_slate_size(), R1_SLATE_SIZE);
        assert_eq!(RankerStrategy::R2.default_slate_size(), R2_SLATE_SIZE);
        assert_eq!(RankerStrategy::R3.default_slate_size(), R3_SLATE_SIZE);
        assert_eq!(RankerStrategy::R1.default_scoring(), ScoringStrategy::S0);
        assert_eq!(RankerStrategy::R2.default_scoring(), ScoringStrategy::S1);
        assert_eq!(RankerStrategy::R3.default_scoring(), ScoringStrategy::S1);

        for strat in [RankerStrategy::R1, RankerStrategy::R2, RankerStrategy::R3] {
            let json = serde_json::to_string(&strat).unwrap();
            let parsed: RankerStrategy = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, strat);
        }
    }

    #[test]
    fn r1_selects_twenty_distinct_eligible_campaigns_and_caps_when_fewer_exist() {
        let ranker = R1RandomBudgetedRanker::default();
        assert_eq!(ranker.strategy(), RankerStrategy::R1);
        assert_eq!(ranker.slate_size(), 20);

        let eligible: Vec<usize> = (0..500).collect();
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let picked = ranker.select_campaigns("user-0001", &eligible, &[], &mut rng);
        assert_eq!(picked.len(), 20);
        let unique: HashSet<_> = picked.iter().copied().collect();
        assert_eq!(unique.len(), 20);

        let small: Vec<usize> = vec![3, 17, 42, 99, 105];
        let picked_small = ranker.select_campaigns("user-0001", &small, &[], &mut rng);
        assert_eq!(picked_small.len(), 5);
        assert_eq!(
            picked_small.iter().copied().collect::<HashSet<_>>(),
            small.iter().copied().collect::<HashSet<_>>()
        );

        let empty = ranker.select_campaigns("user-0001", &[], &[], &mut rng);
        assert!(empty.is_empty());
    }

    #[test]
    fn r2_explores_cold_start_then_exploits_higher_ctr_campaigns() {
        let mp = generate_marketplace(42);
        let mut server = AdServer::from_marketplace(
            &mp,
            AdServerConfig {
                seed: 42,
                ranker: RankerStrategy::R2,
                scoring: ScoringStrategy::S1,
                pacing: PacingStrategy::P0,
                slate_size: 20,
                ..AdServerConfig::default()
            },
        )
        .unwrap();

        // Give every campaign 100 baseline impressions; boost campaigns 10..20 to 5% CTR.
        for idx in 0..500 {
            for step in 0..100 {
                let clicked = (10..20).contains(&idx) && step < 5;
                server.record_impression(1, idx, clicked).unwrap();
            }
        }

        let resp = server.serve_ads("user-0001");
        assert_eq!(resp.strategy, RankerStrategy::R2);
        assert_eq!(resp.scoring, ScoringStrategy::S1);
        assert_eq!(resp.pacing, PacingStrategy::P0);
        assert_eq!(resp.ads.len(), 20);
        let unique: HashSet<_> = resp.ads.iter().map(|a| a.campaign_index).collect();
        assert_eq!(unique.len(), 20);

        // Most of the 10 boosted campaigns should be selected in the exploited top slots.
        let boosted_hits = (10..20).filter(|idx| unique.contains(idx)).count();
        assert!(
            boosted_hits >= 8,
            "R2 should exploit high-CTR campaigns, got only {boosted_hits}/10 boosted campaigns"
        );
    }

    #[test]
    fn r3_greedy_with_emp_ctr_picks_all_ten_boosted_campaigns_deterministically() {
        let mp = generate_marketplace(42);
        let mut server = AdServer::from_marketplace(
            &mp,
            AdServerConfig {
                seed: 42,
                ranker: RankerStrategy::R3,
                scoring: ScoringStrategy::S2,
                pacing: PacingStrategy::P0,
                slate_size: 20,
                ..AdServerConfig::default()
            },
        )
        .unwrap();

        for idx in 0..500 {
            for step in 0..100 {
                let clicked = (10..20).contains(&idx) && step < 5;
                server.record_impression(1, idx, clicked).unwrap();
            }
        }

        let resp = server.serve_ads("user-0001");
        assert_eq!(resp.strategy, RankerStrategy::R3);
        assert_eq!(resp.scoring, ScoringStrategy::S2);
        let top10: HashSet<_> = resp.ads.iter().take(10).map(|a| a.campaign_index).collect();
        assert_eq!(top10, (10..20).collect::<HashSet<_>>());
        assert_eq!(R3GreedyTopKRanker::default().slate_size(), 20);
    }

    #[test]
    fn r2_ranker_is_deterministic_for_identical_seed() {
        let mp = generate_marketplace(42);
        let mut a = AdServer::from_marketplace(
            &mp,
            AdServerConfig {
                seed: 7,
                ranker: RankerStrategy::R2,
                scoring: ScoringStrategy::S1,
                ..AdServerConfig::default()
            },
        )
        .unwrap();
        let mut b = AdServer::from_marketplace(
            &mp,
            AdServerConfig {
                seed: 7,
                ranker: RankerStrategy::R2,
                scoring: ScoringStrategy::S1,
                ..AdServerConfig::default()
            },
        )
        .unwrap();

        let resp_a = a.serve_ads("user-0001");
        let resp_b = b.serve_ads("user-0001");
        assert_eq!(resp_a, resp_b);
        assert_eq!(R2OnlineCtrRanker::default().slate_size(), 20);
    }
}
