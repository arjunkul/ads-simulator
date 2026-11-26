//! Explicit candidate scoring and budget pacing strategies for [`crate::AdServer`].
//!
//! Together with [`crate::RankerStrategy`], [`ScoringStrategy`] and [`PacingStrategy`] form an
//! explicit, transparent 3-axis ad-serving strategy (`Ranking × Scoring × Pacing`) that every
//! simulation run selects, executes, and records across all reports and visualizations.

use std::fmt;
use std::str::FromStr;

use rand::Rng;
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};

use crate::{CampaignState, R1_SLATE_SIZE, RankerStrategy};

/// Default candidate scoring strategy (`S0` = Uniform / Unscored).
pub const DEFAULT_SCORING: ScoringStrategy = ScoringStrategy::S0;
/// Default budget pacing strategy (`P0` = Unpaced / ASAP Hard Cap).
pub const DEFAULT_PACING: PacingStrategy = PacingStrategy::P0;

/// Candidate scoring strategy evaluated over paced-eligible funded campaigns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ScoringStrategy {
    /// S0: Uniform constant score (`1.0`); ignores historical impressions and clicks.
    #[default]
    #[serde(rename = "S0", alias = "s0", alias = "uniform")]
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
}

impl ScoringStrategy {
    /// All compiled scoring strategies in registration order.
    pub const fn all() -> &'static [ScoringStrategy] {
        &[
            ScoringStrategy::S0,
            ScoringStrategy::S1,
            ScoringStrategy::S2,
            ScoringStrategy::S3,
        ]
    }

    /// Canonical short identifier (e.g. `"S0"`, `"S1"`, `"S2"`, `"S3"`).
    pub const fn as_str(self) -> &'static str {
        match self {
            ScoringStrategy::S0 => "S0",
            ScoringStrategy::S1 => "S1",
            ScoringStrategy::S2 => "S2",
            ScoringStrategy::S3 => "S3",
        }
    }

    /// Short human-readable strategy name.
    pub const fn name(self) -> &'static str {
        match self {
            ScoringStrategy::S0 => "Uniform (Unscored)",
            ScoringStrategy::S1 => "Online UCB1 CTR",
            ScoringStrategy::S2 => "Empirical CTR",
            ScoringStrategy::S3 => "Empirical eCPM",
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
        }
    }

    /// Concise scoring formula for dashboards and documentation.
    pub const fn formula(self) -> &'static str {
        match self {
            ScoringStrategy::S0 => "score(i) = 1.0",
            ScoringStrategy::S1 => "score(i) = (clicks_i / imp_i) + 0.02 * sqrt(ln(T) / imp_i)",
            ScoringStrategy::S2 => "score(i) = clicks_i / imp_i",
            ScoringStrategy::S3 => "score(i) = (clicks_i / imp_i) * cpc_cents",
        }
    }

    /// Compute candidate score for an observed (`impressions > 0`) campaign.
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
            "S0" | "UNIFORM" | "NONE" => Ok(ScoringStrategy::S0),
            "S1" | "UCB1" | "UCB1_CTR" => Ok(ScoringStrategy::S1),
            "S2" | "EMP_CTR" | "EMPIRICAL_CTR" | "CTR" => Ok(ScoringStrategy::S2),
            "S3" | "ECPM" | "EMP_ECPM" => Ok(ScoringStrategy::S3),
            other => Err(format!(
                "unknown scoring strategy '{other}' (supported: S0/uniform, S1/ucb1_ctr, S2/emp_ctr, S3/ecpm)"
            )),
        }
    }
}

/// Budget pacing strategy governing daily campaign eligibility across the 30-day flight.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PacingStrategy {
    /// P0: Unpaced (ASAP Hard Cap) — any campaign with remaining budget >= CPC is eligible every day.
    #[default]
    #[serde(rename = "P0", alias = "p0", alias = "unpaced", alias = "asap")]
    P0,
    /// P1: Even Daily Cap — caps cumulative spend through day `d` at `ceil(budget * d / 30)` with carryover.
    #[serde(rename = "P1", alias = "p1", alias = "even_daily", alias = "daily_cap")]
    P1,
    /// P2: Smooth Probabilistic Pacing — tapers inclusion probability smoothly toward the daily target.
    #[serde(rename = "P2", alias = "p2", alias = "smooth_prob", alias = "smooth")]
    P2,
}

impl PacingStrategy {
    /// All compiled pacing strategies in registration order.
    pub const fn all() -> &'static [PacingStrategy] {
        &[PacingStrategy::P0, PacingStrategy::P1, PacingStrategy::P2]
    }

    /// Canonical short identifier (e.g. `"P0"`, `"P1"`, `"P2"`).
    pub const fn as_str(self) -> &'static str {
        match self {
            PacingStrategy::P0 => "P0",
            PacingStrategy::P1 => "P1",
            PacingStrategy::P2 => "P2",
        }
    }

    /// Short human-readable strategy name.
    pub const fn name(self) -> &'static str {
        match self {
            PacingStrategy::P0 => "Unpaced (ASAP Hard Cap)",
            PacingStrategy::P1 => "Even Daily Cap",
            PacingStrategy::P2 => "Smooth Probabilistic Pacing",
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
        }
    }

    /// Filter `active_indices` (already guaranteed funded) into `out` according to this pacing rule
    /// on 1-based `day` within a `total_days` flight.
    pub fn filter_eligible(
        self,
        day: usize,
        total_days: usize,
        active_indices: &[usize],
        campaigns: &[CampaignState],
        rng: &mut ChaCha8Rng,
        out: &mut Vec<usize>,
    ) {
        out.clear();
        let clamped_day = day.clamp(1, total_days.max(1));
        let days_u128 = total_days.max(1) as u128;

        match self {
            PacingStrategy::P0 => {
                out.extend_from_slice(active_indices);
            }
            PacingStrategy::P1 => {
                let day_u128 = clamped_day as u128;
                for &idx in active_indices {
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
                    out.extend_from_slice(active_indices);
                    return;
                }
                let days_left = (total_days + 1 - clamped_day) as f64;
                let day_idx = clamped_day - 1;
                for &idx in active_indices {
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
        }
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
            other => Err(format!(
                "unknown pacing strategy '{other}' (supported: P0/unpaced, P1/even_daily, P2/smooth_prob)"
            )),
        }
    }
}

/// Metadata describing any registered ranking, scoring, or pacing strategy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrategyInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub formula: String,
}

/// Complete, explicit 3-axis serving strategy manifest recorded on every simulation run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrategyProfile {
    pub ranking: RankerStrategy,
    pub ranking_name: String,
    pub scoring: ScoringStrategy,
    pub scoring_name: String,
    pub pacing: PacingStrategy,
    pub pacing_name: String,
    pub slate_size: usize,
}

impl StrategyProfile {
    /// Build a complete manifest from the selected ranking, scoring, pacing, and slate size.
    pub fn new(
        ranking: RankerStrategy,
        scoring: ScoringStrategy,
        pacing: PacingStrategy,
        slate_size: usize,
    ) -> Self {
        Self {
            ranking,
            ranking_name: ranking.name().to_owned(),
            scoring,
            scoring_name: scoring.name().to_owned(),
            pacing,
            pacing_name: pacing.name().to_owned(),
            slate_size,
        }
    }
}

impl Default for StrategyProfile {
    fn default() -> Self {
        Self::new(
            RankerStrategy::R1,
            DEFAULT_SCORING,
            DEFAULT_PACING,
            R1_SLATE_SIZE,
        )
    }
}

impl fmt::Display for StrategyProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Ranking {} ({}) · Scoring {} ({}) · Pacing {} ({}) · Slate {}",
            self.ranking,
            self.ranking_name,
            self.scoring,
            self.scoring_name,
            self.pacing,
            self.pacing_name,
            self.slate_size,
        )
    }
}

/// Validate that the selected `(ranking, scoring, pacing)` triple is internally coherent.
pub fn validate_strategy(
    ranking: RankerStrategy,
    scoring: ScoringStrategy,
    _pacing: PacingStrategy,
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
        PacingStrategy, ScoringStrategy, StrategyProfile, validate_strategy,
    };
    use crate::RankerStrategy;

    #[test]
    fn scoring_and_pacing_parse_display_and_json_round_trip() {
        for (raw, expected) in [
            ("S0", ScoringStrategy::S0),
            ("uniform", ScoringStrategy::S0),
            ("S1", ScoringStrategy::S1),
            ("ucb1_ctr", ScoringStrategy::S1),
            ("S2", ScoringStrategy::S2),
            ("emp_ctr", ScoringStrategy::S2),
            ("S3", ScoringStrategy::S3),
            ("ecpm", ScoringStrategy::S3),
        ] {
            assert_eq!(ScoringStrategy::from_str(raw).unwrap(), expected);
            let json = serde_json::to_string(&expected).unwrap();
            let parsed: ScoringStrategy = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, expected);
        }
        assert!(ScoringStrategy::from_str("S99").is_err());

        for (raw, expected) in [
            ("P0", PacingStrategy::P0),
            ("unpaced", PacingStrategy::P0),
            ("P1", PacingStrategy::P1),
            ("even_daily", PacingStrategy::P1),
            ("P2", PacingStrategy::P2),
            ("smooth_prob", PacingStrategy::P2),
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
        assert!(validate_strategy(RankerStrategy::R1, ScoringStrategy::S0, PacingStrategy::P0).is_ok());
        assert!(validate_strategy(RankerStrategy::R1, ScoringStrategy::S0, PacingStrategy::P1).is_ok());
        assert!(validate_strategy(RankerStrategy::R1, ScoringStrategy::S1, PacingStrategy::P0).is_err());
        assert!(validate_strategy(RankerStrategy::R2, ScoringStrategy::S1, PacingStrategy::P2).is_ok());
        assert!(validate_strategy(RankerStrategy::R3, ScoringStrategy::S2, PacingStrategy::P1).is_ok());

        let profile = StrategyProfile::new(
            RankerStrategy::R2,
            ScoringStrategy::S1,
            PacingStrategy::P1,
            20,
        );
        let text = profile.to_string();
        assert!(text.contains("Ranking R2"));
        assert!(text.contains("Scoring S1"));
        assert!(text.contains("Pacing P1"));
    }
}
