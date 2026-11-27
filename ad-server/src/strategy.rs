//! Explicit candidate scoring, budget pacing, and 5-axis strategy manifest for [`crate::AdServer`].
//!
//! Together with [`crate::RetrievalStrategy`], [`crate::BiddingStrategy`], and
//! [`crate::RankerStrategy`], [`ScoringStrategy`] and [`PacingStrategy`] form a transparent
//! 5-axis ad-serving strategy (`Retrieval × Pacing × Bidding × Scoring × Ranking`) that every
//! simulation run selects, executes, and records across all reports, APIs, and visualizations.

use std::fmt;
use std::str::FromStr;

use rand::Rng;
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};

use crate::timestamp::SECONDS_PER_DAY;
use crate::{
    BiddingStrategy, CampaignState, DEFAULT_BIDDING, DEFAULT_RETRIEVAL, R1_SLATE_SIZE,
    RankerStrategy, RetrievalStrategy,
};

/// Default candidate scoring strategy (`S0` = Uniform / Unscored).
pub const DEFAULT_SCORING: ScoringStrategy = ScoringStrategy::S0;
/// Default budget pacing strategy (`P0` = Unpaced / ASAP Hard Cap).
pub const DEFAULT_PACING: PacingStrategy = PacingStrategy::P0;

/// Candidate scoring strategy evaluated over retrieved, paced-eligible funded campaigns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ScoringStrategy {
    /// S0 (`Score-Uniform`): Uniform constant score (`1.0`); ignores historical impressions/clicks.
    #[default]
    #[serde(rename = "S0", alias = "s0", alias = "Score-Uniform", alias = "score-uniform", alias = "uniform")]
    S0,
    /// S1: Online UCB1 CTR score (`emp_ctr + c * sqrt(ln(T) / imp)`) with cold-start priority.
    #[serde(rename = "S1", alias = "s1", alias = "ucb1_ctr", alias = "ucb1")]
    S1,
    /// S2: Pure observed empirical CTR (`clicks / impressions`) with cold-start priority.
    #[serde(rename = "S2", alias = "s2", alias = "emp_ctr")]
    S2,
    /// S3: Observed expected CPM (`(clicks / impressions) * cpc_cents`) with cold-start priority.
    #[serde(rename = "S3", alias = "s3", alias = "ecpm")]
    S3,
    /// Score-R1 (`S4`): Daily cold-trained PyTorch L2 logistic regression CTR model (`ads-ml`).
    #[serde(rename = "Score-R1", alias = "score-r1", alias = "S4", alias = "s4")]
    ScoreR1,
    /// Score-XGB (`S5`): Daily cold-trained XGBoost GBDT CTR model (`ads-ml`).
    #[serde(
        rename = "Score-XGB",
        alias = "score-xgb",
        alias = "S5",
        alias = "s5",
        alias = "xgboost",
        alias = "xgb",
        alias = "gbdt"
    )]
    ScoreXgb,
}

impl ScoringStrategy {
    /// All compiled scoring strategies in registration order.
    pub const fn all() -> &'static [ScoringStrategy] {
        &[
            ScoringStrategy::S0,
            ScoringStrategy::S1,
            ScoringStrategy::S2,
            ScoringStrategy::S3,
            ScoringStrategy::ScoreR1,
            ScoringStrategy::ScoreXgb,
        ]
    }

    /// Returns `true` for offline-trained `ads-ml` CTR models (`Score-R1`, `Score-XGB`)
    /// evaluated via [`ads_ml::PreparedScorer`].
    pub const fn is_ml_model(self) -> bool {
        matches!(self, ScoringStrategy::ScoreR1 | ScoringStrategy::ScoreXgb)
    }

    /// Canonical short identifier (e.g. `"S0"`, `"S1"`, `"S2"`, `"S3"`, `"Score-R1"`, `"Score-XGB"`).
    pub const fn as_str(self) -> &'static str {
        match self {
            ScoringStrategy::S0 => "S0",
            ScoringStrategy::S1 => "S1",
            ScoringStrategy::S2 => "S2",
            ScoringStrategy::S3 => "S3",
            ScoringStrategy::ScoreR1 => "Score-R1",
            ScoringStrategy::ScoreXgb => "Score-XGB",
        }
    }

    /// Short human-readable strategy name.
    pub const fn name(self) -> &'static str {
        match self {
            ScoringStrategy::S0 => "Uniform (Unscored)",
            ScoringStrategy::S1 => "Online UCB1 CTR",
            ScoringStrategy::S2 => "Empirical CTR",
            ScoringStrategy::S3 => "Empirical eCPM",
            ScoringStrategy::ScoreR1 => "PyTorch Logistic Regression CTR (ads-ml)",
            ScoringStrategy::ScoreXgb => "XGBoost GBDT CTR (ads-ml)",
        }
    }

    /// Detailed human-readable description.
    pub const fn description(self) -> &'static str {
        match self {
            ScoringStrategy::S0 => {
                "Assigns equal score (1.0) to all eligible campaigns without using click history"
            }
            ScoringStrategy::S1 => {
                "Online UCB1 CTR upper confidence bound (emp_ctr + 0.02 * sqrt(ln(T) / imp)) with cold-start priority"
            }
            ScoringStrategy::S2 => {
                "Pure empirical CTR (clicks / impressions) with cold-start priority and no exploration bonus"
            }
            ScoringStrategy::S3 => {
                "Expected revenue per impression (emp_ctr * cpc_cents) with cold-start priority"
            }
            ScoringStrategy::ScoreR1 => {
                "Daily cold-trained PyTorch L2 logistic regression pCTR model over user/category/campaign features (ads-ml)"
            }
            ScoringStrategy::ScoreXgb => {
                "Daily cold-trained XGBoost gradient-boosted decision tree pCTR model over leave-one-out Bayesian CTR & entity features (ads-ml)"
            }
        }
    }

    /// Concise scoring formula for dashboards and documentation.
    pub const fn formula(self) -> &'static str {
        match self {
            ScoringStrategy::S0 => "score(i) = 1.0",
            ScoringStrategy::S1 => "score(i) = (clicks_i / imp_i) + 0.02 * sqrt(ln(T) / imp_i)",
            ScoringStrategy::S2 => "score(i) = clicks_i / imp_i",
            ScoringStrategy::S3 => "score(i) = (clicks_i / imp_i) * cpc_cents",
            ScoringStrategy::ScoreR1 => "pCTR(u, i) = sigmoid(w_0 + w_u + w_cat + w_adv + w_camp + w_{u,cat}) via PyTorch LBFGS",
            ScoringStrategy::ScoreXgb => "pCTR(u, i) = sigmoid(base_margin + sum_t tree_t(x(u, i))) via XGBoost GBDT",
        }
    }

    /// Compute candidate score for an observed (`impressions > 0`) campaign (online strategies).
    pub fn score_observed(
        self,
        campaign: &CampaignState,
        ln_total_impressions: f64,
        cpc_cents: u64,
        ucb_exploration_c: f64,
    ) -> f64 {
        let imp = campaign.totals.impressions.max(1) as f64;
        let emp_ctr = campaign.totals.clicks as f64 / imp;
        match self {
            ScoringStrategy::S0 => 1.0,
            ScoringStrategy::S1 => emp_ctr + ucb_exploration_c * (ln_total_impressions / imp).sqrt(),
            ScoringStrategy::S2 => emp_ctr,
            ScoringStrategy::S3 => emp_ctr * (cpc_cents as f64),
            ScoringStrategy::ScoreR1 | ScoringStrategy::ScoreXgb => emp_ctr,
        }
    }
}

impl fmt::Display for ScoringStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ScoringStrategy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_uppercase().as_str() {
            "S0" | "SCORE-UNIFORM" | "UNIFORM" | "NONE" => Ok(ScoringStrategy::S0),
            "S1" | "UCB1" | "UCB1_CTR" => Ok(ScoringStrategy::S1),
            "S2" | "EMP_CTR" | "EMPIRICAL_CTR" | "CTR" => Ok(ScoringStrategy::S2),
            "S3" | "ECPM" | "EMP_ECPM" => Ok(ScoringStrategy::S3),
            "SCORE-R1" | "SCORE_R1" | "S4" | "PYTORCH" | "LR" => Ok(ScoringStrategy::ScoreR1),
            "SCORE-XGB" | "SCORE_XGB" | "S5" | "XGBOOST" | "XGB" | "GBDT" => {
                Ok(ScoringStrategy::ScoreXgb)
            }
            other => Err(format!(
                "unknown scoring strategy '{other}' (supported: S0/Score-Uniform, S1/ucb1_ctr, S2/emp_ctr, S3/ecpm, Score-R1/S4, Score-XGB/S5)"
            )),
        }
    }
}

/// Budget pacing strategy governing daily and intraday campaign eligibility across the 30-day flight.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PacingStrategy {
    /// P0 (`none`): Unpaced (ASAP Hard Cap) — any campaign with remaining budget >= CPC is eligible.
    #[default]
    #[serde(rename = "P0", alias = "p0", alias = "none", alias = "unpaced", alias = "asap")]
    P0,
    /// P1: Even Daily Cap — caps cumulative spend through day `d` at `ceil(budget * d / 30)` with carryover.
    #[serde(rename = "P1", alias = "p1", alias = "even_daily", alias = "daily_cap")]
    P1,
    /// P2: Smooth Probabilistic Pacing — tapers inclusion probability smoothly toward the daily target.
    #[serde(rename = "P2", alias = "p2", alias = "smooth_prob", alias = "smooth")]
    P2,
    /// Pacing-1 (`P3`): Equal daily budget split + linear 24-hour PST intraday spend schedule.
    #[serde(rename = "pacing-1", alias = "Pacing-1", alias = "P3", alias = "p3")]
    Pacing1,
}

impl PacingStrategy {
    /// All compiled pacing strategies in registration order.
    pub const fn all() -> &'static [PacingStrategy] {
        &[
            PacingStrategy::P0,
            PacingStrategy::P1,
            PacingStrategy::P2,
            PacingStrategy::Pacing1,
        ]
    }

    /// Canonical short identifier (e.g. `"P0"`, `"P1"`, `"P2"`, `"pacing-1"`).
    pub const fn as_str(self) -> &'static str {
        match self {
            PacingStrategy::P0 => "P0",
            PacingStrategy::P1 => "P1",
            PacingStrategy::P2 => "P2",
            PacingStrategy::Pacing1 => "pacing-1",
        }
    }

    /// Short human-readable strategy name.
    pub const fn name(self) -> &'static str {
        match self {
            PacingStrategy::P0 => "Unpaced (ASAP Hard Cap)",
            PacingStrategy::P1 => "Even Daily Cap",
            PacingStrategy::P2 => "Smooth Probabilistic Pacing",
            PacingStrategy::Pacing1 => "Pacing-1 (24h PST Intraday Linear)",
        }
    }

    /// Detailed human-readable description.
    pub const fn description(self) -> &'static str {
        match self {
            PacingStrategy::P0 => {
                "No daily throttle; every campaign with remaining_budget >= CPC is eligible until exhausted"
            }
            PacingStrategy::P1 => {
                "Caps each campaign's cumulative spend through day d at ceil(budget * d / 30) with unspent carryover"
            }
            PacingStrategy::P2 => {
                "Smoothly throttles per-request eligibility probability as daily spend approaches remaining_budget / days_left"
            }
            PacingStrategy::Pacing1 => {
                "Splits upfront budget evenly across 30 days and enforces a linear 24-hour PST intraday spend curve"
            }
        }
    }

    /// Concise pacing rule for dashboards and documentation.
    pub const fn formula(self) -> &'static str {
        match self {
            PacingStrategy::P0 => "eligible(i, d) <=> remaining_i >= cpc",
            PacingStrategy::P1 => "eligible(i, d) <=> spent_i < ceil(budget_i * d / 30)",
            PacingStrategy::P2 => {
                "p_pass(i, d) = clamp((daily_target_i - today_spent_i) / daily_target_i, 0, 1); 1.0 on Day 30"
            }
            PacingStrategy::Pacing1 => {
                "eligible(i, d, t) <=> daily_spent_i / daily_budget_i <= t / 86400 (PST)"
            }
        }
    }

    /// Return the campaign's pacing multiplier at `second_of_day` in `[0, SECONDS_PER_DAY)`.
    pub fn pacing_factor(
        self,
        daily_spend_cents: u64,
        daily_budget_cents_val: u64,
        remaining_budget_cents: u64,
        cpc_cents: u64,
        second_of_day: u32,
    ) -> f64 {
        if remaining_budget_cents < cpc_cents {
            return 0.0;
        }
        match self {
            Self::P0 => 1.0,
            Self::P1 => {
                if daily_spend_cents.saturating_add(cpc_cents) <= daily_budget_cents_val {
                    1.0
                } else {
                    0.0
                }
            }
            Self::P2 => {
                if daily_budget_cents_val == 0 {
                    0.0
                } else {
                    ((daily_budget_cents_val as f64 - daily_spend_cents as f64)
                        / (daily_budget_cents_val as f64))
                        .clamp(0.0, 1.0)
                }
            }
            Self::Pacing1 => {
                match pacing_1_unlock_second(
                    daily_spend_cents,
                    daily_budget_cents_val,
                    remaining_budget_cents,
                    cpc_cents,
                ) {
                    Some(unlock_second) if unlock_second <= second_of_day => 1.0,
                    _ => 0.0,
                }
            }
        }
    }

    /// Filter `retrieved_indices` (already guaranteed funded by `CampaignRetriever`) into `out`
    /// according to this pacing rule on 1-based `day` and `second_of_day` in `[0, 86_400)`.
    #[allow(clippy::too_many_arguments)]
    pub fn filter_eligible(
        self,
        day: usize,
        total_days: usize,
        second_of_day: u32,
        cpc_cents: u64,
        retrieved_indices: &[usize],
        campaigns: &[CampaignState],
        rng: &mut ChaCha8Rng,
        out: &mut Vec<usize>,
    ) {
        out.clear();
        let clamped_day = day.clamp(1, total_days.max(1));
        let days_u128 = total_days.max(1) as u128;

        match self {
            PacingStrategy::P0 => {
                out.extend_from_slice(retrieved_indices);
            }
            PacingStrategy::P1 => {
                let day_u128 = clamped_day as u128;
                for &idx in retrieved_indices {
                    if let Some(c) = campaigns.get(idx) {
                        let allowance =
                            ((c.budget_cents as u128) * day_u128 + (days_u128 - 1)) / days_u128;
                        if (c.totals.spend_cents as u128) < allowance {
                            out.push(idx);
                        }
                    }
                }
            }
            PacingStrategy::P2 => {
                if clamped_day >= total_days {
                    out.extend_from_slice(retrieved_indices);
                    return;
                }
                let days_left = (total_days + 1 - clamped_day) as f64;
                let day_idx = clamped_day - 1;
                for &idx in retrieved_indices {
                    let Some(c) = campaigns.get(idx) else {
                        continue;
                    };
                    let today_spend = c
                        .daily
                        .get(day_idx)
                        .map(|d| d.activity.spend_cents)
                        .unwrap_or(0);
                    if today_spend == 0 {
                        out.push(idx);
                        continue;
                    }
                    let start_of_day_remaining =
                        c.remaining_budget_cents.saturating_add(today_spend) as f64;
                    let daily_target = start_of_day_remaining / days_left;
                    let today_f = today_spend as f64;
                    if today_f < daily_target {
                        let p_pass = ((daily_target - today_f) / daily_target).clamp(0.0, 1.0);
                        if rng.random_bool(p_pass) {
                            out.push(idx);
                        }
                    }
                }
            }
            PacingStrategy::Pacing1 => {
                let day_idx = clamped_day - 1;
                for &idx in retrieved_indices {
                    let Some(c) = campaigns.get(idx) else {
                        continue;
                    };
                    let today_spend = c
                        .daily
                        .get(day_idx)
                        .map(|d| d.activity.spend_cents)
                        .unwrap_or(0);
                    let start_of_day_remaining =
                        c.remaining_budget_cents.saturating_add(today_spend);
                    let d_budget = daily_budget_cents(
                        c.budget_cents,
                        start_of_day_remaining,
                        clamped_day,
                        total_days,
                    );
                    if matches!(
                        pacing_1_unlock_second(
                            today_spend,
                            d_budget,
                            c.remaining_budget_cents,
                            cpc_cents,
                        ),
                        Some(unlock) if unlock <= second_of_day
                    ) {
                        out.push(idx);
                    }
                }
            }
        }
    }
}

/// Split a campaign's upfront budget across `days` as evenly as possible, giving any remainder
/// cents to the earliest days one cent at a time, capped by remaining lifetime budget.
pub fn daily_budget_cents(
    total_budget_cents: u64,
    remaining_budget_cents: u64,
    day: usize,
    days: usize,
) -> u64 {
    let days = days.max(1) as u64;
    let base = total_budget_cents / days;
    let extra = u64::from((day as u64) <= total_budget_cents % days);
    (base + extra).min(remaining_budget_cents)
}

/// Return the earliest integer second of day in `[0, SECONDS_PER_DAY)` at which a campaign is
/// back on or behind the even intraday spend curve (`daily_spend / daily_budget <= t / 86_400`),
/// or `None` if it cannot afford another click today or would not unlock before midnight.
pub fn pacing_1_unlock_second(
    daily_spend_cents: u64,
    daily_budget_cents: u64,
    remaining_budget_cents: u64,
    cpc_cents: u64,
) -> Option<u32> {
    if remaining_budget_cents < cpc_cents {
        return None;
    }
    if daily_spend_cents.saturating_add(cpc_cents) > daily_budget_cents {
        return None;
    }
    if daily_spend_cents == 0 {
        return Some(0);
    }
    let unlock = (u128::from(daily_spend_cents) * u128::from(SECONDS_PER_DAY))
        .div_ceil(u128::from(daily_budget_cents));
    if unlock < u128::from(SECONDS_PER_DAY) {
        Some(unlock as u32)
    } else {
        None
    }
}

impl fmt::Display for PacingStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PacingStrategy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_uppercase().as_str() {
            "P0" | "UNPACED" | "ASAP" | "NONE" => Ok(PacingStrategy::P0),
            "P1" | "EVEN_DAILY" | "DAILY_CAP" | "EVEN" => Ok(PacingStrategy::P1),
            "P2" | "SMOOTH_PROB" | "SMOOTH" | "PROBABILISTIC" => Ok(PacingStrategy::P2),
            "PACING-1" | "PACING_1" | "PACING1" | "P3" => Ok(PacingStrategy::Pacing1),
            other => Err(format!(
                "unknown pacing strategy '{other}' (supported: P0/none, P1/even_daily, P2/smooth_prob, pacing-1/P3)"
            )),
        }
    }
}

/// Metadata describing any registered retrieval, pacing, bidding, scoring, or ranking strategy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrategyInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub formula: String,
}

/// Complete, explicit 5-axis serving strategy manifest recorded on every simulation run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrategyProfile {
    #[serde(default)]
    pub retrieval: RetrievalStrategy,
    #[serde(default = "default_retrieval_name")]
    pub retrieval_name: String,
    pub ranking: RankerStrategy,
    pub ranking_name: String,
    pub scoring: ScoringStrategy,
    pub scoring_name: String,
    pub pacing: PacingStrategy,
    pub pacing_name: String,
    #[serde(default)]
    pub bidding: BiddingStrategy,
    #[serde(default = "default_bidding_name")]
    pub bidding_name: String,
    pub slate_size: usize,
}

fn default_retrieval_name() -> String {
    DEFAULT_RETRIEVAL.name().to_owned()
}

fn default_bidding_name() -> String {
    DEFAULT_BIDDING.name().to_owned()
}

impl StrategyProfile {
    /// Build a complete 5-axis manifest from retrieval, pacing, bidding, scoring, ranking, and slate size.
    pub fn new(
        retrieval: RetrievalStrategy,
        ranking: RankerStrategy,
        scoring: ScoringStrategy,
        pacing: PacingStrategy,
        bidding: BiddingStrategy,
        slate_size: usize,
    ) -> Self {
        Self {
            retrieval,
            retrieval_name: retrieval.name().to_owned(),
            ranking,
            ranking_name: ranking.name().to_owned(),
            scoring,
            scoring_name: scoring.name().to_owned(),
            pacing,
            pacing_name: pacing.name().to_owned(),
            bidding,
            bidding_name: bidding.name().to_owned(),
            slate_size,
        }
    }
}

impl Default for StrategyProfile {
    fn default() -> Self {
        Self::new(
            DEFAULT_RETRIEVAL,
            RankerStrategy::R1,
            DEFAULT_SCORING,
            DEFAULT_PACING,
            DEFAULT_BIDDING,
            R1_SLATE_SIZE,
        )
    }
}

impl fmt::Display for StrategyProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Retrieval {} ({}) · Pacing {} ({}) · Bidding {} ({}) · Scoring {} ({}) · Ranking {} ({}) · Slate {}",
            self.retrieval,
            self.retrieval_name,
            self.pacing,
            self.pacing_name,
            self.bidding,
            self.bidding_name,
            self.scoring,
            self.scoring_name,
            self.ranking,
            self.ranking_name,
            self.slate_size,
        )
    }
}

/// Validate that the selected 5-axis strategy tuple is internally coherent.
pub fn validate_strategy(
    _retrieval: RetrievalStrategy,
    ranking: RankerStrategy,
    scoring: ScoringStrategy,
    _pacing: PacingStrategy,
    _bidding: BiddingStrategy,
) -> Result<(), String> {
    if ranking == RankerStrategy::R1 && scoring != ScoringStrategy::S0 {
        return Err(format!(
            "incompatible strategy combination: ranker R1 ({}) ignores candidate scores and requires scoring S0 ({}); select ranker R2 or R3 to use scoring {} ({})",
            ranking.name(),
            ScoringStrategy::S0.name(),
            scoring,
            scoring.name(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{
        PacingStrategy, ScoringStrategy, StrategyProfile, daily_budget_cents,
        pacing_1_unlock_second, validate_strategy,
    };
    use crate::timestamp::SECONDS_PER_DAY;
    use crate::{BiddingStrategy, RankerStrategy, RetrievalStrategy};

    #[test]
    fn scoring_and_pacing_parse_display_and_json_round_trip() {
        for (raw, expected) in [
            ("S0", ScoringStrategy::S0),
            ("Score-Uniform", ScoringStrategy::S0),
            ("uniform", ScoringStrategy::S0),
            ("S1", ScoringStrategy::S1),
            ("ucb1_ctr", ScoringStrategy::S1),
            ("S2", ScoringStrategy::S2),
            ("emp_ctr", ScoringStrategy::S2),
            ("S3", ScoringStrategy::S3),
            ("ecpm", ScoringStrategy::S3),
            ("Score-R1", ScoringStrategy::ScoreR1),
            ("S4", ScoringStrategy::ScoreR1),
            ("Score-XGB", ScoringStrategy::ScoreXgb),
            ("S5", ScoringStrategy::ScoreXgb),
            ("xgboost", ScoringStrategy::ScoreXgb),
            ("xgb", ScoringStrategy::ScoreXgb),
            ("gbdt", ScoringStrategy::ScoreXgb),
        ] {
            assert_eq!(ScoringStrategy::from_str(raw).unwrap(), expected);
            let json = serde_json::to_string(&expected).unwrap();
            let parsed: ScoringStrategy = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, expected);
        }
        assert!(ScoringStrategy::ScoreR1.is_ml_model());
        assert!(ScoringStrategy::ScoreXgb.is_ml_model());
        assert!(!ScoringStrategy::S0.is_ml_model());
        assert!(!ScoringStrategy::S1.is_ml_model());
        assert!(ScoringStrategy::from_str("S99").is_err());

        for (raw, expected) in [
            ("P0", PacingStrategy::P0),
            ("none", PacingStrategy::P0),
            ("unpaced", PacingStrategy::P0),
            ("P1", PacingStrategy::P1),
            ("even_daily", PacingStrategy::P1),
            ("P2", PacingStrategy::P2),
            ("smooth_prob", PacingStrategy::P2),
            ("pacing-1", PacingStrategy::Pacing1),
            ("P3", PacingStrategy::Pacing1),
        ] {
            assert_eq!(PacingStrategy::from_str(raw).unwrap(), expected);
            let json = serde_json::to_string(&expected).unwrap();
            let parsed: PacingStrategy = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, expected);
        }
        assert!(PacingStrategy::from_str("P99").is_err());
    }

    #[test]
    fn validate_strategy_rejects_r1_with_non_uniform_scoring() {
        assert!(validate_strategy(
            RetrievalStrategy::RetrieveV0,
            RankerStrategy::R1,
            ScoringStrategy::S0,
            PacingStrategy::P0,
            BiddingStrategy::Basebid
        )
        .is_ok());
        assert!(validate_strategy(
            RetrievalStrategy::RetrieveV0,
            RankerStrategy::R1,
            ScoringStrategy::S1,
            PacingStrategy::P0,
            BiddingStrategy::Basebid
        )
        .is_err());
        assert!(validate_strategy(
            RetrievalStrategy::RetrieveV0,
            RankerStrategy::R1,
            ScoringStrategy::ScoreXgb,
            PacingStrategy::P0,
            BiddingStrategy::Basebid
        )
        .is_err());
        assert!(validate_strategy(
            RetrievalStrategy::RetrieveV0,
            RankerStrategy::R3,
            ScoringStrategy::ScoreR1,
            PacingStrategy::Pacing1,
            BiddingStrategy::AutobidV0
        )
        .is_ok());
        assert!(validate_strategy(
            RetrievalStrategy::RetrieveV0,
            RankerStrategy::R3,
            ScoringStrategy::ScoreXgb,
            PacingStrategy::Pacing1,
            BiddingStrategy::AutobidV0
        )
        .is_ok());

        let profile = StrategyProfile::new(
            RetrievalStrategy::RetrieveV0,
            RankerStrategy::R2,
            ScoringStrategy::S1,
            PacingStrategy::Pacing1,
            BiddingStrategy::AutobidV0,
            20,
        );
        let text = profile.to_string();
        assert!(text.contains("Retrieval Retrieve-V0"));
        assert!(text.contains("Pacing pacing-1"));
        assert!(text.contains("Bidding bidding-autobid-v0"));
        assert!(text.contains("Scoring S1"));
        assert!(text.contains("Ranking R2"));
    }

    #[test]
    fn daily_budget_splits_evenly_with_earliest_day_remainder() {
        let budgets: Vec<_> = (1..=30)
            .map(|day| daily_budget_cents(10_000, 10_000, day, 30))
            .collect();
        assert_eq!(budgets.iter().sum::<u64>(), 10_000);
        assert!(budgets[..10].iter().all(|&b| b == 334));
        assert!(budgets[10..].iter().all(|&b| b == 333));
    }

    #[test]
    fn pacing_1_unlocks_clicks_evenly_across_the_day_and_handles_boundaries() {
        assert_eq!(pacing_1_unlock_second(0, 300, 9_000, 100), Some(0));
        assert_eq!(pacing_1_unlock_second(100, 300, 8_900, 100), Some(8 * 3_600));
        assert_eq!(pacing_1_unlock_second(200, 300, 8_800, 100), Some(16 * 3_600));
        assert_eq!(pacing_1_unlock_second(300, 300, 8_700, 100), None);
        assert_eq!(PacingStrategy::Pacing1.pacing_factor(100, 300, 8_900, 100, 28_799), 0.0);
        assert_eq!(PacingStrategy::Pacing1.pacing_factor(100, 300, 8_900, 100, 28_800), 1.0);
        assert_eq!(PacingStrategy::P0.pacing_factor(300, 300, 8_700, 100, 0), 1.0);
        let t1 = pacing_1_unlock_second(100, 333, 10_000, 100).unwrap();
        assert!(100 * u64::from(SECONDS_PER_DAY) <= 333 * u64::from(t1));
        assert!(100 * u64::from(SECONDS_PER_DAY) > 333 * u64::from(t1 - 1));
        assert_eq!(pacing_1_unlock_second(99_999, 100_000, 100_000, 1), None);
        assert_eq!(pacing_1_unlock_second(0, 33, 1_000, 100), None);
    }
}
