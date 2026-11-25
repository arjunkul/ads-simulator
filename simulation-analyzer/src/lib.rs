//! Automated post-run simulation analyzer for `ads-marketplace-simulation` reports.
//!
//! Computes budget depletion curves & exhaustion milestones, average/daily CTR & eCPM,
//! per-campaign spend/CTR distributions & Gini concentration, top category/advertiser rollups,
//! and automated diagnostic findings.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufReader, Error, ErrorKind};
use std::path::Path;

use ad_server::RankerStrategy;
use ads_marketplace_simulation::{DAYS, MAX_CLICK_PROBABILITY, OwnedReport, Report, SCHEMA_VERSION};
use serde::{Deserialize, Serialize};

/// Theoretical random CTR when 5 of 100 categories match (5%) and matched click prob is 4%.
pub const THEORETICAL_RANDOM_CTR_PCT: f64 = 0.05 * MAX_CLICK_PROBABILITY * 100.0;
/// Theoretical upper-bound CTR when 100% of served ads match user preferences (4%).
pub const THEORETICAL_MAX_CTR_PCT: f64 = MAX_CLICK_PROBABILITY * 100.0;

/// Compiled specification of the `simulation-analyzer` crate for dashboard introspection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalyzerSpec {
    pub crate_name: String,
    pub binary_name: String,
    pub version: String,
    pub schema_version: u32,
    pub metric_groups: Vec<String>,
    pub finding_categories: Vec<String>,
}

/// Return the live compiled specification of `simulation-analyzer`.
pub fn analyzer_spec() -> AnalyzerSpec {
    AnalyzerSpec {
        crate_name: env!("CARGO_PKG_NAME").to_owned(),
        binary_name: "simulation-analyzer".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        schema_version: SCHEMA_VERSION,
        metric_groups: vec![
            "Budget Depletion & Exhaustion Milestones".into(),
            "Overall & Daily CTR / eCPM / Fill Rate".into(),
            "Campaign Spend Distribution & Gini Concentration".into(),
            "Category & Advertiser Rollups".into(),
            "Automated Diagnostic Findings".into(),
        ],
        finding_categories: vec![
            "budget_depletion".into(),
            "ctr_efficiency".into(),
            "pacing".into(),
            "supply_exhaustion".into(),
            "concentration".into(),
        ],
    }
}

/// Summary statistics over a numeric sample (all values finite, guarded for empty slices).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DistributionStats {
    pub min: f64,
    pub p25: f64,
    pub median: f64,
    pub p75: f64,
    pub max: f64,
    pub mean: f64,
    pub std_dev: f64,
}

/// One day along the 30-day budget depletion trajectory.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DailyDepletionPoint {
    pub day: usize,
    pub daily_spend_cents: u64,
    pub cumulative_spend_cents: u64,
    pub cumulative_utilization_pct: f64,
    pub active_campaigns: usize,
    pub newly_exhausted_campaigns: usize,
    pub insufficient_budget: u64,
}

/// Budget burn, depletion milestones, and supply-exhaustion analysis.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BudgetDepletionAnalysis {
    pub total_budget_cents: u64,
    pub total_spend_cents: u64,
    pub remaining_budget_cents: u64,
    pub utilization_pct: f64,
    pub total_campaigns: usize,
    pub exhausted_campaigns: usize,
    pub exhausted_pct: f64,
    pub active_campaigns_end_of_run: usize,
    pub first_exhaustion_day: Option<usize>,
    pub median_exhaustion_day: Option<usize>,
    pub full_exhaustion_day: Option<usize>,
    pub projected_days_to_full_depletion: Option<f64>,
    pub daily_burn_cents: DistributionStats,
    pub daily_burn_cv_pct: f64,
    pub insufficient_budget_opportunities: u64,
    pub unfilled_rate_pct: f64,
    pub daily_depletion: Vec<DailyDepletionPoint>,
}

/// Click-through rate, eCPM, and ad-server delivery volume metrics.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CtrPerformanceAnalysis {
    pub total_ad_requests: u64,
    pub total_impressions: u64,
    pub total_clicks: u64,
    pub avg_ads_per_request: f64,
    pub overall_ctr_pct: f64,
    pub daily_ctr_pct: DistributionStats,
    pub effective_cpm_cents: f64,
    pub effective_cpc_cents: f64,
    pub theoretical_random_ctr_pct: f64,
    pub theoretical_max_ctr_pct: f64,
    pub ctr_vs_random_baseline_ratio: f64,
}

/// Highlight row for top campaigns by spend or CTR.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CampaignRankingRow {
    pub campaign_id: String,
    pub advertiser_id: String,
    pub category_id: String,
    pub impressions: u64,
    pub clicks: u64,
    pub spend_cents: u64,
    pub utilization_pct: f64,
    pub ctr_pct: f64,
}

/// Cross-campaign spend/CTR spread and concentration metrics.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CampaignDistributionAnalysis {
    pub spend_cents: DistributionStats,
    pub ctr_pct: DistributionStats,
    pub impressions: DistributionStats,
    pub gini_spend_coefficient: f64,
    pub top_10_pct_spend_share_pct: f64,
    pub zero_click_campaigns: usize,
    pub zero_impression_campaigns: usize,
    pub top_campaigns_by_spend: Vec<CampaignRankingRow>,
    pub top_campaigns_by_ctr: Vec<CampaignRankingRow>,
}

/// Aggregated performance metrics for a product category.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CategoryBreakdownRow {
    pub category_id: String,
    pub campaigns: usize,
    pub exhausted_campaigns: usize,
    pub impressions: u64,
    pub clicks: u64,
    pub spend_cents: u64,
    pub ctr_pct: f64,
}

/// Aggregated performance metrics for an advertiser.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AdvertiserBreakdownRow {
    pub advertiser_id: String,
    pub campaigns: usize,
    pub exhausted_campaigns: usize,
    pub budget_cents: u64,
    pub spend_cents: u64,
    pub utilization_pct: f64,
    pub impressions: u64,
    pub clicks: u64,
    pub ctr_pct: f64,
}

/// Severity tag for automated diagnostic findings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingSeverity {
    Info,
    Warning,
    Critical,
}

/// One automated diagnostic finding derived from a simulation run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisFinding {
    pub severity: FindingSeverity,
    pub category: String,
    pub title: String,
    pub detail: String,
}

/// Complete automated analysis of a 30-day ads-marketplace simulation run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SimulationAnalysis {
    pub schema_version: u32,
    pub currency: String,
    pub ranker: RankerStrategy,
    pub slate_size: usize,
    pub seed: u64,
    pub cpc_cents: u64,
    #[serde(default)]
    pub campaign_budget_cents: Option<u64>,
    #[serde(default = "default_min_budget_cents")]
    pub campaign_budget_min_cents: u64,
    #[serde(default = "default_max_budget_cents")]
    pub campaign_budget_max_cents: u64,
    pub impressions_target: Option<u64>,
    pub days_analyzed: usize,
    pub budget_depletion: BudgetDepletionAnalysis,
    pub ctr_performance: CtrPerformanceAnalysis,
    pub campaign_distribution: CampaignDistributionAnalysis,
    pub top_categories_by_spend: Vec<CategoryBreakdownRow>,
    pub top_advertisers_by_spend: Vec<AdvertiserBreakdownRow>,
    pub findings: Vec<AnalysisFinding>,
}

const fn default_min_budget_cents() -> u64 {
    ad_server::DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS
}

const fn default_max_budget_cents() -> u64 {
    ad_server::DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS
}

fn invalid(msg: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, msg.into())
}

fn safe_pct(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        (numerator as f64) * 100.0 / (denominator as f64)
    }
}

fn round4(v: f64) -> f64 {
    if !v.is_finite() {
        0.0
    } else {
        (v * 10_000.0).round() / 10_000.0
    }
}

fn percentile_sorted(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let idx = p.clamp(0.0, 1.0) * ((sorted.len() - 1) as f64);
    let lo = idx.floor() as usize;
    let hi = idx.ceil() as usize;
    if lo == hi {
        sorted[lo]
    } else {
        let frac = idx - (lo as f64);
        sorted[lo] + frac * (sorted[hi] - sorted[lo])
    }
}

fn compute_distribution(values: &[f64]) -> DistributionStats {
    if values.is_empty() {
        return DistributionStats::default();
    }
    let mut sorted: Vec<f64> = values
        .iter()
        .copied()
        .map(|v| if v.is_finite() { v } else { 0.0 })
        .collect();
    sorted.sort_by(|a, b| a.total_cmp(b));

    let n = sorted.len() as f64;
    let sum: f64 = sorted.iter().sum();
    let mean = sum / n;
    let variance = sorted
        .iter()
        .map(|&v| {
            let d = v - mean;
            d * d
        })
        .sum::<f64>()
        / n;

    DistributionStats {
        min: round4(sorted[0]),
        p25: round4(percentile_sorted(&sorted, 0.25)),
        median: round4(percentile_sorted(&sorted, 0.50)),
        p75: round4(percentile_sorted(&sorted, 0.75)),
        max: round4(sorted[sorted.len() - 1]),
        mean: round4(mean),
        std_dev: round4(variance.sqrt()),
    }
}

fn compute_gini(values: &[u64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let total: u64 = values.iter().copied().sum();
    if total == 0 {
        return 0.0;
    }
    let mut sorted: Vec<f64> = values.iter().map(|&v| v as f64).collect();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let n = sorted.len() as f64;
    let weighted_sum: f64 = sorted
        .iter()
        .enumerate()
        .map(|(i, &x)| ((i + 1) as f64) * x)
        .sum();
    let gini = (2.0 * weighted_sum) / (n * (total as f64)) - (n + 1.0) / n;
    round4(gini.clamp(0.0, 1.0))
}

/// Analyze a borrowed [`Report`] from an in-memory simulation run.
pub fn analyze_borrowed_report(report: &Report<'_>) -> Result<SimulationAnalysis, Error> {
    analyze_report(&report.to_owned_report())
}

/// Load a `simulation.json` ([`OwnedReport`]) from disk and analyze it.
pub fn load_and_analyze(path: &Path) -> Result<SimulationAnalysis, Error> {
    let file = File::open(path)?;
    let report: OwnedReport = serde_json::from_reader(BufReader::new(file))
        .map_err(|err| invalid(format!("failed to parse simulation report: {err}")))?;
    analyze_report(&report)
}

/// Auto-analyze a completed 30-day [`OwnedReport`].
pub fn analyze_report(report: &OwnedReport) -> Result<SimulationAnalysis, Error> {
    if report.schema_version != SCHEMA_VERSION {
        return Err(invalid(format!(
            "unsupported schema_version {} (expected {SCHEMA_VERSION})",
            report.schema_version
        )));
    }
    if report.days.is_empty() {
        return Err(invalid("report contains zero days"));
    }

    let cpc = report.config.cpc_cents;
    let num_days = report.days.len();
    let total_campaigns = report.campaigns.len();
    let total_budget = report.summary.budget_cents;
    let total_spend = report.summary.activity.spend_cents;
    let remaining_budget = report.summary.remaining_budget_cents;
    let utilization_pct = round4(safe_pct(total_spend, total_budget));

    // Track exact end-of-day active campaign counts across all days.
    let mut active_by_day = vec![0usize; num_days];
    let mut running_remaining: Vec<u64> = report.campaigns.iter().map(|c| c.budget_cents).collect();
    let initially_active = running_remaining.iter().filter(|&&b| b >= cpc).count();

    for (d_idx, active_slot) in active_by_day.iter_mut().enumerate() {
        let mut active_count = 0usize;
        for (c_idx, campaign) in report.campaigns.iter().enumerate() {
            if let Some(day_entry) = campaign.daily.get(d_idx) {
                running_remaining[c_idx] =
                    running_remaining[c_idx].saturating_sub(day_entry.activity.spend_cents);
            }
            if running_remaining[c_idx] >= cpc {
                active_count += 1;
            }
        }
        *active_slot = active_count;
    }

    let exhausted_campaigns = report
        .campaigns
        .iter()
        .filter(|c| c.remaining_budget_cents < cpc)
        .count();
    let active_end = total_campaigns.saturating_sub(exhausted_campaigns);
    let exhausted_pct = round4(safe_pct(exhausted_campaigns as u64, total_campaigns as u64));

    let mut first_exhaustion_day = None;
    let mut median_exhaustion_day = None;
    let mut full_exhaustion_day = None;
    let mut cumulative_spend = 0u64;
    let mut prev_active = initially_active;
    let mut daily_depletion = Vec::with_capacity(num_days);
    let mut daily_burns = Vec::with_capacity(num_days);
    let mut daily_ctrs = Vec::with_capacity(num_days);

    for (i, day) in report.days.iter().enumerate() {
        cumulative_spend = cumulative_spend.saturating_add(day.activity.spend_cents);
        let active_now = active_by_day[i];
        let exhausted_now = total_campaigns.saturating_sub(active_now);
        let newly_exhausted = prev_active.saturating_sub(active_now);
        prev_active = active_now;

        if first_exhaustion_day.is_none() && exhausted_now > 0 {
            first_exhaustion_day = Some(day.day);
        }
        if median_exhaustion_day.is_none()
            && total_campaigns > 0
            && exhausted_now * 2 >= total_campaigns
        {
            median_exhaustion_day = Some(day.day);
        }
        if full_exhaustion_day.is_none() && total_campaigns > 0 && active_now == 0 {
            full_exhaustion_day = Some(day.day);
        }

        daily_burns.push(day.activity.spend_cents as f64);
        daily_ctrs.push(safe_pct(day.activity.clicks, day.activity.impressions));
        daily_depletion.push(DailyDepletionPoint {
            day: day.day,
            daily_spend_cents: day.activity.spend_cents,
            cumulative_spend_cents: cumulative_spend,
            cumulative_utilization_pct: round4(safe_pct(cumulative_spend, total_budget)),
            active_campaigns: active_now,
            newly_exhausted_campaigns: newly_exhausted,
            insufficient_budget: day.insufficient_budget,
        });
    }

    let daily_burn_stats = compute_distribution(&daily_burns);
    let daily_burn_cv_pct = if daily_burn_stats.mean > 0.0 {
        round4((daily_burn_stats.std_dev / daily_burn_stats.mean) * 100.0)
    } else {
        0.0
    };
    let projected_days_to_full_depletion = if daily_burn_stats.mean > 0.0 && total_budget > 0 {
        Some(round4((total_budget as f64) / daily_burn_stats.mean))
    } else {
        None
    };

    let unfilled_rate_pct = round4(safe_pct(
        report.summary.insufficient_budget,
        report.summary.opportunities,
    ));

    let budget_depletion = BudgetDepletionAnalysis {
        total_budget_cents: total_budget,
        total_spend_cents: total_spend,
        remaining_budget_cents: remaining_budget,
        utilization_pct,
        total_campaigns,
        exhausted_campaigns,
        exhausted_pct,
        active_campaigns_end_of_run: active_end,
        first_exhaustion_day,
        median_exhaustion_day,
        full_exhaustion_day,
        projected_days_to_full_depletion,
        daily_burn_cents: daily_burn_stats,
        daily_burn_cv_pct,
        insufficient_budget_opportunities: report.summary.insufficient_budget,
        unfilled_rate_pct,
        daily_depletion,
    };

    // CTR & eCPM metrics
    let total_impressions = report.summary.activity.impressions;
    let total_clicks = report.summary.activity.clicks;
    let total_ad_requests = report.summary.ad_requests;
    let avg_ads_per_request = if total_ad_requests > 0 {
        round4((total_impressions as f64) / (total_ad_requests as f64))
    } else {
        0.0
    };
    let overall_ctr_pct = round4(safe_pct(total_clicks, total_impressions));
    let daily_ctr_stats = compute_distribution(&daily_ctrs);
    let effective_cpm_cents = if total_impressions > 0 {
        round4((total_spend as f64) * 1_000.0 / (total_impressions as f64))
    } else {
        0.0
    };
    let effective_cpc_cents = if total_clicks > 0 {
        round4((total_spend as f64) / (total_clicks as f64))
    } else {
        0.0
    };
    let ctr_vs_random_baseline_ratio = if THEORETICAL_RANDOM_CTR_PCT > 0.0 {
        round4(overall_ctr_pct / THEORETICAL_RANDOM_CTR_PCT)
    } else {
        0.0
    };

    let ctr_performance = CtrPerformanceAnalysis {
        total_ad_requests,
        total_impressions,
        total_clicks,
        avg_ads_per_request,
        overall_ctr_pct,
        daily_ctr_pct: daily_ctr_stats,
        effective_cpm_cents,
        effective_cpc_cents,
        theoretical_random_ctr_pct: round4(THEORETICAL_RANDOM_CTR_PCT),
        theoretical_max_ctr_pct: round4(THEORETICAL_MAX_CTR_PCT),
        ctr_vs_random_baseline_ratio,
    };

    // Per-campaign distributions, Gini concentration, category & advertiser rollups
    let camp_spends_u64: Vec<u64> = report
        .campaigns
        .iter()
        .map(|c| c.totals.spend_cents)
        .collect();
    let camp_spends_f64: Vec<f64> = camp_spends_u64.iter().map(|&v| v as f64).collect();
    let camp_ctrs_f64: Vec<f64> = report
        .campaigns
        .iter()
        .map(|c| safe_pct(c.totals.clicks, c.totals.impressions))
        .collect();
    let camp_impr_f64: Vec<f64> = report
        .campaigns
        .iter()
        .map(|c| c.totals.impressions as f64)
        .collect();

    let gini_spend_coefficient = compute_gini(&camp_spends_u64);
    let mut spends_desc = camp_spends_u64.clone();
    spends_desc.sort_unstable_by(|a, b| b.cmp(a));
    let top_n = ((total_campaigns as f64) * 0.10).ceil() as usize;
    let top_n = top_n.clamp(0, spends_desc.len());
    let top_10_sum: u64 = spends_desc.iter().take(top_n).copied().sum();
    let top_10_pct_spend_share_pct = round4(safe_pct(top_10_sum, total_spend));

    let zero_click_campaigns = report
        .campaigns
        .iter()
        .filter(|c| c.totals.clicks == 0)
        .count();
    let zero_impression_campaigns = report
        .campaigns
        .iter()
        .filter(|c| c.totals.impressions == 0)
        .count();

    let mut campaign_rows: Vec<CampaignRankingRow> = report
        .campaigns
        .iter()
        .map(|c| CampaignRankingRow {
            campaign_id: c.id.clone(),
            advertiser_id: c.advertiser_id.clone(),
            category_id: c.category_id.clone(),
            impressions: c.totals.impressions,
            clicks: c.totals.clicks,
            spend_cents: c.totals.spend_cents,
            utilization_pct: round4(safe_pct(c.totals.spend_cents, c.budget_cents)),
            ctr_pct: round4(safe_pct(c.totals.clicks, c.totals.impressions)),
        })
        .collect();

    campaign_rows.sort_by(|a, b| {
        b.spend_cents
            .cmp(&a.spend_cents)
            .then_with(|| a.campaign_id.cmp(&b.campaign_id))
    });
    let top_campaigns_by_spend: Vec<CampaignRankingRow> =
        campaign_rows.iter().take(5).cloned().collect();

    let mut ctr_rows: Vec<CampaignRankingRow> = campaign_rows
        .into_iter()
        .filter(|r| r.impressions > 0)
        .collect();
    ctr_rows.sort_by(|a, b| {
        b.ctr_pct
            .total_cmp(&a.ctr_pct)
            .then_with(|| b.clicks.cmp(&a.clicks))
            .then_with(|| a.campaign_id.cmp(&b.campaign_id))
    });
    let top_campaigns_by_ctr: Vec<CampaignRankingRow> = ctr_rows.into_iter().take(5).collect();

    let campaign_distribution = CampaignDistributionAnalysis {
        spend_cents: compute_distribution(&camp_spends_f64),
        ctr_pct: compute_distribution(&camp_ctrs_f64),
        impressions: compute_distribution(&camp_impr_f64),
        gini_spend_coefficient,
        top_10_pct_spend_share_pct,
        zero_click_campaigns,
        zero_impression_campaigns,
        top_campaigns_by_spend,
        top_campaigns_by_ctr,
    };

    // Category rollup
    let mut cat_map: BTreeMap<String, (usize, usize, u64, u64, u64)> = BTreeMap::new();
    let mut adv_map: BTreeMap<String, (usize, usize, u64, u64, u64, u64)> = BTreeMap::new();
    for c in &report.campaigns {
        let exhausted = usize::from(c.remaining_budget_cents < cpc);
        let entry = cat_map.entry(c.category_id.clone()).or_default();
        entry.0 += 1;
        entry.1 += exhausted;
        entry.2 += c.totals.impressions;
        entry.3 += c.totals.clicks;
        entry.4 += c.totals.spend_cents;

        let a = adv_map.entry(c.advertiser_id.clone()).or_default();
        a.0 += 1;
        a.1 += exhausted;
        a.2 += c.budget_cents;
        a.3 += c.totals.spend_cents;
        a.4 += c.totals.impressions;
        a.5 += c.totals.clicks;
    }

    let mut top_categories_by_spend: Vec<CategoryBreakdownRow> = cat_map
        .into_iter()
        .map(
            |(category_id, (campaigns, exhausted_campaigns, impressions, clicks, spend_cents))| {
                CategoryBreakdownRow {
                    category_id,
                    campaigns,
                    exhausted_campaigns,
                    impressions,
                    clicks,
                    spend_cents,
                    ctr_pct: round4(safe_pct(clicks, impressions)),
                }
            },
        )
        .collect();
    top_categories_by_spend.sort_by(|a, b| {
        b.spend_cents
            .cmp(&a.spend_cents)
            .then_with(|| a.category_id.cmp(&b.category_id))
    });
    top_categories_by_spend.truncate(10);

    let mut top_advertisers_by_spend: Vec<AdvertiserBreakdownRow> = adv_map
        .into_iter()
        .map(
            |(
                advertiser_id,
                (campaigns, exhausted_campaigns, budget_cents, spend_cents, impressions, clicks),
            )| {
                AdvertiserBreakdownRow {
                    advertiser_id,
                    campaigns,
                    exhausted_campaigns,
                    budget_cents,
                    spend_cents,
                    utilization_pct: round4(safe_pct(spend_cents, budget_cents)),
                    impressions,
                    clicks,
                    ctr_pct: round4(safe_pct(clicks, impressions)),
                }
            },
        )
        .collect();
    top_advertisers_by_spend.sort_by(|a, b| {
        b.spend_cents
            .cmp(&a.spend_cents)
            .then_with(|| a.advertiser_id.cmp(&b.advertiser_id))
    });
    top_advertisers_by_spend.truncate(10);

    let findings = synthesize_findings(
        report.config.ranker,
        report.config.slate_size,
        &budget_depletion,
        &ctr_performance,
        &campaign_distribution,
    );

    Ok(SimulationAnalysis {
        schema_version: report.schema_version,
        currency: report.currency.clone(),
        ranker: report.config.ranker,
        slate_size: report.config.slate_size,
        seed: report.config.seed,
        cpc_cents: report.config.cpc_cents,
        campaign_budget_cents: report.config.campaign_budget_cents,
        campaign_budget_min_cents: report.config.campaign_budget_min_cents,
        campaign_budget_max_cents: report.config.campaign_budget_max_cents,
        impressions_target: report.config.impressions,
        days_analyzed: num_days,
        budget_depletion,
        ctr_performance,
        campaign_distribution,
        top_categories_by_spend,
        top_advertisers_by_spend,
        findings,
    })
}

fn synthesize_findings(
    ranker: RankerStrategy,
    slate_size: usize,
    dep: &BudgetDepletionAnalysis,
    ctr: &CtrPerformanceAnalysis,
    dist: &CampaignDistributionAnalysis,
) -> Vec<AnalysisFinding> {
    let mut findings = Vec::new();

    if dep.total_spend_cents == 0 {
        findings.push(AnalysisFinding {
            severity: FindingSeverity::Warning,
            category: "budget_depletion".into(),
            title: "Zero budget spent across run".into(),
            detail: format!(
                "Spent $0.00 of {} ({} insufficient-budget opportunities, {} impressions).",
                format_usd(dep.total_budget_cents),
                dep.insufficient_budget_opportunities,
                ctr.total_impressions
            ),
        });
    } else if dep.full_exhaustion_day.is_some() {
        let day = dep.full_exhaustion_day.unwrap();
        findings.push(AnalysisFinding {
            severity: FindingSeverity::Critical,
            category: "supply_exhaustion".into(),
            title: format!("Complete budget exhaustion reached on Day {day}"),
            detail: format!(
                "All {} campaigns depleted ({:.2}% total utilization), leaving {} unfilled opportunities ({:.2}% unfilled rate).",
                dep.total_campaigns,
                dep.utilization_pct,
                dep.insufficient_budget_opportunities,
                dep.unfilled_rate_pct
            ),
        });
    } else if dep.exhausted_campaigns > 0 {
        findings.push(AnalysisFinding {
            severity: FindingSeverity::Warning,
            category: "budget_depletion".into(),
            title: format!(
                "{} of {} campaigns exhausted ({:.1}%)",
                dep.exhausted_campaigns, dep.total_campaigns, dep.exhausted_pct
            ),
            detail: format!(
                "First campaign exhausted on Day {}; {} campaigns remained funded through Day {DAYS} ({:.2}% pool utilization).",
                dep.first_exhaustion_day.unwrap_or(1),
                dep.active_campaigns_end_of_run,
                dep.utilization_pct
            ),
        });
    } else if dep.utilization_pct < 15.0 {
        let proj = dep
            .projected_days_to_full_depletion
            .map(|d| format!(" (~{d:.0} days to full depletion at current burn)"))
            .unwrap_or_default();
        findings.push(AnalysisFinding {
            severity: FindingSeverity::Warning,
            category: "budget_depletion".into(),
            title: format!("Low 30-day budget utilization ({:.2}%)", dep.utilization_pct),
            detail: format!(
                "Spent {} of {}{proj}. Consider targeted ranking beyond {} to lift CTR and budget delivery.",
                format_usd(dep.total_spend_cents),
                format_usd(dep.total_budget_cents),
                ranker
            ),
        });
    } else {
        findings.push(AnalysisFinding {
            severity: FindingSeverity::Info,
            category: "budget_depletion".into(),
            title: format!("Healthy budget pacing ({:.2}% utilized)", dep.utilization_pct),
            detail: format!(
                "Spent {} of {} with zero exhausted campaigns and mean burn {}/day.",
                format_usd(dep.total_spend_cents),
                format_usd(dep.total_budget_cents),
                format_usd(dep.daily_burn_cents.mean.round() as u64)
            ),
        });
    }

    if ctr.total_impressions > 0 {
        let headroom = ctr.theoretical_max_ctr_pct / ctr.overall_ctr_pct.max(0.0001);
        findings.push(AnalysisFinding {
            severity: if ctr.overall_ctr_pct < 0.35 {
                FindingSeverity::Info
            } else {
                FindingSeverity::Info
            },
            category: "ctr_efficiency".into(),
            title: format!(
                "Average CTR {:.3}% ({:.2}x random baseline) · eCPM {}",
                ctr.overall_ctr_pct,
                ctr.ctr_vs_random_baseline_ratio,
                format_usd(ctr.effective_cpm_cents.round() as u64)
            ),
            detail: format!(
                "Ranker {} (slate {}) delivered {:.2} ads/request across {} requests. Theoretical targeted ceiling is {:.2}% ({:.1}x current CTR).",
                ranker,
                slate_size,
                ctr.avg_ads_per_request,
                ctr.total_ad_requests,
                ctr.theoretical_max_ctr_pct,
                headroom.min(999.0)
            ),
        });
    }

    if dep.daily_burn_cv_pct > 35.0 && dep.total_spend_cents > 0 {
        findings.push(AnalysisFinding {
            severity: FindingSeverity::Warning,
            category: "pacing".into(),
            title: format!("High daily spend volatility (CV {:.1}%)", dep.daily_burn_cv_pct),
            detail: format!(
                "Daily spend ranged from {} to {} (std dev {}).",
                format_usd(dep.daily_burn_cents.min.round() as u64),
                format_usd(dep.daily_burn_cents.max.round() as u64),
                format_usd(dep.daily_burn_cents.std_dev.round() as u64)
            ),
        });
    }

    if dist.gini_spend_coefficient >= 0.35 && dep.total_spend_cents > 0 {
        findings.push(AnalysisFinding {
            severity: FindingSeverity::Info,
            category: "concentration".into(),
            title: format!(
                "Spend dispersion across campaigns (Gini {:.3})",
                dist.gini_spend_coefficient
            ),
            detail: format!(
                "Top 10% of campaigns accounted for {:.1}% of total spend; {} campaigns recorded zero clicks.",
                dist.top_10_pct_spend_share_pct, dist.zero_click_campaigns
            ),
        });
    }

    findings
}

fn format_usd(cents: u64) -> String {
    let dollars = cents / 100;
    let rem = cents % 100;
    let digits = dollars.to_string();
    let len = digits.len();
    let mut out = String::with_capacity(len + len / 3 + 4);
    out.push('$');
    if len <= 3 {
        out.push_str(&digits);
    } else {
        let first = if len % 3 == 0 { 3 } else { len % 3 };
        out.push_str(&digits[..first]);
        for chunk in digits[first..].as_bytes().chunks(3) {
            out.push(',');
            out.push_str(std::str::from_utf8(chunk).expect("ASCII digits"));
        }
    }
    let _ = write!(out, ".{rem:02}");
    out
}

/// Format a concise human-readable terminal report for a [`SimulationAnalysis`].
pub fn render_text_summary(analysis: &SimulationAnalysis) -> String {
    let dep = &analysis.budget_depletion;
    let ctr = &analysis.ctr_performance;
    let dist = &analysis.campaign_distribution;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Simulation Analysis (Ranker {} · Slate {} · Seed {} · {} days)",
        analysis.ranker, analysis.slate_size, analysis.seed, analysis.days_analyzed
    );
    let _ = writeln!(
        out,
        "  Budget Depletion : {} / {} ({:.2}% utilized) · {} / {} campaigns exhausted",
        format_usd(dep.total_spend_cents),
        format_usd(dep.total_budget_cents),
        dep.utilization_pct,
        dep.exhausted_campaigns,
        dep.total_campaigns
    );
    let _ = writeln!(
        out,
        "  Daily Burn       : min {}, median {}, mean {}, max {} (CV {:.1}%)",
        format_usd(dep.daily_burn_cents.min.round() as u64),
        format_usd(dep.daily_burn_cents.median.round() as u64),
        format_usd(dep.daily_burn_cents.mean.round() as u64),
        format_usd(dep.daily_burn_cents.max.round() as u64),
        dep.daily_burn_cv_pct
    );
    let _ = writeln!(
        out,
        "  CTR & Delivery   : {:.3}% overall CTR (daily mean {:.3}%, std {:.3}%) · eCPM {} · {:.2} ads/req",
        ctr.overall_ctr_pct,
        ctr.daily_ctr_pct.mean,
        ctr.daily_ctr_pct.std_dev,
        format_usd(ctr.effective_cpm_cents.round() as u64),
        ctr.avg_ads_per_request
    );
    let _ = writeln!(
        out,
        "  Concentration    : Gini {:.3} · top-10% share {:.1}% · {} zero-click campaigns",
        dist.gini_spend_coefficient, dist.top_10_pct_spend_share_pct, dist.zero_click_campaigns
    );
    if !analysis.findings.is_empty() {
        let _ = writeln!(out, "  Findings         :");
        for f in &analysis.findings {
            let _ = writeln!(out, "    - [{:?}] {}: {}", f.severity, f.title, f.detail);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use ad_server::{R1_SLATE_SIZE, RankerStrategy};
    use ads_marketplace_simulation::{Config, run};
    use ads_simulator::generate_marketplace;

    use crate::{
        FindingSeverity, SimulationAnalysis, analyze_borrowed_report, analyzer_spec,
        render_text_summary,
    };

    #[test]
    fn analyzes_baseline_r1_run_with_valid_ctr_depletion_and_findings() {
        let mp = generate_marketplace(42);
        let report = run(
            &mp,
            Config {
                seed: 42,
                campaign_budget_cents: None,
                cpc_cents: 100,
                impressions: Some(1_000_000),
                ranker: RankerStrategy::R1,
                slate_size: R1_SLATE_SIZE,
                ..Config::default()
            },
        )
        .unwrap();

        let analysis = analyze_borrowed_report(&report).unwrap();
        assert_eq!(analysis.ranker, RankerStrategy::R1);
        assert_eq!(analysis.slate_size, 20);
        assert_eq!(analysis.days_analyzed, 30);
        assert_eq!(analysis.budget_depletion.daily_depletion.len(), 30);
        assert_eq!(
            analysis.budget_depletion.total_spend_cents,
            report.summary.activity.spend_cents
        );
        assert!(analysis.ctr_performance.overall_ctr_pct > 0.10);
        assert!(analysis.ctr_performance.overall_ctr_pct < 0.40);
        assert!(analysis.ctr_performance.avg_ads_per_request > 19.0);
        assert!(analysis.campaign_distribution.gini_spend_coefficient >= 0.0);
        assert!(analysis.campaign_distribution.gini_spend_coefficient <= 1.0);
        assert_eq!(analysis.top_categories_by_spend.len(), 10);
        assert_eq!(analysis.top_advertisers_by_spend.len(), 10);
        assert!(!analysis.findings.is_empty());

        let summary_text = render_text_summary(&analysis);
        assert!(summary_text.contains("Ranker R1"));
        assert!(summary_text.contains("Budget Depletion"));

        let json = serde_json::to_string(&analysis).unwrap();
        let parsed: SimulationAnalysis = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, analysis);
    }

    #[test]
    fn detects_full_budget_exhaustion_day_and_critical_finding() {
        let mp = generate_marketplace(42);
        let report = run(
            &mp,
            Config {
                seed: 42,
                campaign_budget_cents: Some(100),
                cpc_cents: 100,
                impressions: None,
                ranker: RankerStrategy::R1,
                slate_size: 20,
                ..Config::default()
            },
        )
        .unwrap();

        let analysis = analyze_borrowed_report(&report).unwrap();
        assert!(analysis.budget_depletion.exhausted_campaigns > 0);
        assert!(analysis.budget_depletion.first_exhaustion_day.is_some());
        if analysis.budget_depletion.full_exhaustion_day.is_some() {
            assert!(
                analysis
                    .findings
                    .iter()
                    .any(|f| f.severity == FindingSeverity::Critical)
            );
        }
    }

    #[test]
    fn handles_zero_spend_run_without_nan_or_panic() {
        let mp = generate_marketplace(42);
        let report = run(
            &mp,
            Config {
                seed: 42,
                campaign_budget_cents: Some(50),
                cpc_cents: 100,
                impressions: None,
                ranker: RankerStrategy::R1,
                slate_size: 20,
                ..Config::default()
            },
        )
        .unwrap();

        let analysis = analyze_borrowed_report(&report).unwrap();
        assert_eq!(analysis.budget_depletion.total_spend_cents, 0);
        assert_eq!(analysis.ctr_performance.overall_ctr_pct, 0.0);
        assert_eq!(analysis.campaign_distribution.gini_spend_coefficient, 0.0);
        let json = serde_json::to_string(&analysis).unwrap();
        assert!(!json.contains("NaN"));
        assert!(!json.contains("Infinity"));
    }

    #[test]
    fn analyzer_spec_round_trips_json() {
        let spec = analyzer_spec();
        assert_eq!(spec.crate_name, "simulation-analyzer");
        assert!(!spec.metric_groups.is_empty());
        let json = serde_json::to_string(&spec).unwrap();
        let parsed: crate::AnalyzerSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, spec);
    }
}
