//! Simulation execution pipeline, ledger invariant auditor, 30-day budget time-series
//! derivation, CSV rendering, and atomic artifact export.

use std::error::Error as StdError;
use std::fmt::{self, Write as _};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Write as _};
use std::path::{Path, PathBuf};

use ad_server::{
    BiddingStrategy, PacingStrategy, R1_SLATE_SIZE, RankerStrategy, RetrievalStrategy,
    ScoringStrategy, StrategyProfile,
};
use ads_marketplace_simulation::{Activity, Config, OwnedReport, Report};
use world_generator::{Marketplace, generate_marketplace};
use serde::{Deserialize, Serialize};
use simulation_analyzer::{SimulationAnalysis, analyze_report};
use tempfile::NamedTempFile;

/// Report and time-series schema version.
pub const SCHEMA_VERSION: u32 = ads_marketplace_simulation::SCHEMA_VERSION;
/// Monetary currency code (`"USD"`).
pub const CURRENCY: &str = world_generator::CURRENCY;
/// Number of simulated days (`30`).
pub const DAYS: usize = ads_marketplace_simulation::DAYS;
/// Default unsigned simulation RNG seed (`42`).
pub const DEFAULT_SEED: u64 = ads_marketplace_simulation::DEFAULT_SEED;
/// Default signed marketplace generator seed (`42`).
pub const DEFAULT_MARKETPLACE_SEED: i64 = world_generator::DEFAULT_SEED;
/// Default minimum upfront 30-day campaign budget in USD cents (`100_000` = `$1,000.00`).
pub const DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS: u64 =
    ads_marketplace_simulation::DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS;
/// Default maximum upfront 30-day campaign budget in USD cents (`1_000_000` = `$10,000.00`).
pub const DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS: u64 =
    ads_marketplace_simulation::DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS;
/// Default click price in USD cents (`100` = `$1.00`).
pub const DEFAULT_CPC_CENTS: u64 = ads_marketplace_simulation::DEFAULT_CPC_CENTS;
/// Default delivered-impression target across 30 days (`1_000_000`).
pub const DEFAULT_IMPRESSIONS: u64 = 1_000_000;
/// Default candidate retrieval strategy (`Retrieve-V0`).
pub const DEFAULT_RETRIEVAL: RetrievalStrategy = ads_marketplace_simulation::DEFAULT_RETRIEVAL;
/// Default ads-ranker strategy (`R1`).
pub const DEFAULT_RANKER: RankerStrategy = ads_marketplace_simulation::DEFAULT_RANKER;
/// Default candidate scoring strategy (`S0` = Uniform).
pub const DEFAULT_SCORING: ScoringStrategy = ads_marketplace_simulation::DEFAULT_SCORING;
/// Default budget pacing strategy (`P0` = Unpaced ASAP).
pub const DEFAULT_PACING: PacingStrategy = ads_marketplace_simulation::DEFAULT_PACING;
/// Default auction bidding strategy (`bidding-basebid`).
pub const DEFAULT_BIDDING: BiddingStrategy = ads_marketplace_simulation::DEFAULT_BIDDING;
/// Default ad-server slate size (`20` distinct funded campaigns per request).
pub const DEFAULT_SLATE_SIZE: usize = R1_SLATE_SIZE;
/// Default artifact output directory (`"output/sim-agent"`).
pub const DEFAULT_OUTPUT_DIR: &str = "output/sim-agent";

/// Filename for the raw simulator Schema v1 report JSON.
pub const SIMULATION_REPORT_FILENAME: &str = "simulation.json";
/// Filename for the derived 30-day budget time-series JSON.
pub const TIMESERIES_JSON_FILENAME: &str = "budget_timeseries.json";
/// Filename for the 30-day budget time-series CSV.
pub const TIMESERIES_CSV_FILENAME: &str = "budget_timeseries.csv";
/// Filename for the standalone dual-panel SVG budget plot.
pub const BUDGET_PLOT_SVG_FILENAME: &str = "budget_spent_30d.svg";
/// Filename for the automated post-run simulation analysis JSON.
pub const ANALYSIS_JSON_FILENAME: &str = "analysis.json";

const fn default_min_campaign_budget_cents() -> u64 {
    DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS
}

const fn default_max_campaign_budget_cents() -> u64 {
    DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS
}

const fn default_slate_size() -> usize {
    DEFAULT_SLATE_SIZE
}

/// Configuration for executing a 30-day `sim-agent` run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Optional path to a Schema v1 `marketplace.json`. When `None`, generates in memory.
    pub marketplace_path: Option<PathBuf>,
    /// Signed seed for in-memory marketplace generation (ignored when `marketplace_path` is `Some`).
    pub marketplace_seed: i64,
    /// Unsigned simulation RNG seed (default: `42`).
    pub sim_seed: u64,
    /// Optional fixed upfront 30-day budget per campaign in USD cents (`None` randomizes each
    /// campaign uniformly between `campaign_budget_min_cents` and `campaign_budget_max_cents`).
    #[serde(default)]
    pub campaign_budget_cents: Option<u64>,
    /// Minimum randomized campaign budget in USD cents (default: `100_000` = `$1,000.00`).
    #[serde(default = "default_min_campaign_budget_cents")]
    pub campaign_budget_min_cents: u64,
    /// Maximum randomized campaign budget in USD cents (default: `1_000_000` = `$10,000.00`).
    #[serde(default = "default_max_campaign_budget_cents")]
    pub campaign_budget_max_cents: u64,
    /// Cost per click in USD cents (default: `100`, must be `> 0`).
    pub cpc_cents: u64,
    /// Target delivered impressions across 30 days (`Some(1_000_000)` by default;
    /// `None` runs natural 600 requests/day user-independent mode).
    pub impressions: Option<u64>,
    /// Candidate retrieval strategy used by the ad-server (default: `Retrieve-V0`).
    #[serde(default)]
    pub retrieval: RetrievalStrategy,
    /// Ads-ranker strategy used by the ad-server (default: `R1`).
    #[serde(default)]
    pub ranker: RankerStrategy,
    /// Candidate scoring strategy used by the ad-server (default: `S0`).
    #[serde(default)]
    pub scoring: ScoringStrategy,
    /// Budget pacing strategy used by the ad-server (default: `P0`).
    #[serde(default)]
    pub pacing: PacingStrategy,
    /// Auction bidding strategy used by the ad-server (default: `bidding-basebid`).
    #[serde(default)]
    pub bidding: BiddingStrategy,
    /// Maximum distinct funded campaigns served per ad-server request (default: `20`).
    #[serde(default = "default_slate_size")]
    pub slate_size: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            marketplace_path: None,
            marketplace_seed: DEFAULT_MARKETPLACE_SEED,
            sim_seed: DEFAULT_SEED,
            campaign_budget_cents: None,
            campaign_budget_min_cents: DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS,
            campaign_budget_max_cents: DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS,
            cpc_cents: DEFAULT_CPC_CENTS,
            impressions: Some(DEFAULT_IMPRESSIONS),
            retrieval: DEFAULT_RETRIEVAL,
            ranker: DEFAULT_RANKER,
            scoring: DEFAULT_SCORING,
            pacing: DEFAULT_PACING,
            bidding: DEFAULT_BIDDING,
            slate_size: DEFAULT_SLATE_SIZE,
        }
    }
}

impl AgentConfig {
    /// Build the upstream [`ads_marketplace_simulation::Config`].
    pub fn simulation_config(&self) -> Config {
        Config {
            seed: self.sim_seed,
            campaign_budget_cents: self.campaign_budget_cents,
            campaign_budget_min_cents: self.campaign_budget_min_cents,
            campaign_budget_max_cents: self.campaign_budget_max_cents,
            cpc_cents: self.cpc_cents,
            impressions: self.impressions,
            retrieval: self.retrieval,
            ranker: self.ranker,
            scoring: self.scoring,
            pacing: self.pacing,
            bidding: self.bidding,
            slate_size: self.slate_size,
        }
    }
}

/// Configuration metadata recorded inside [`BudgetTimeSeries`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeSeriesConfig {
    /// Marketplace seed used for the run (`Some` on all `sim-agent` runs, `None` when
    /// reconstructed from a standalone `simulation.json` without marketplace metadata).
    #[serde(default)]
    pub marketplace_seed: Option<i64>,
    pub sim_seed: u64,
    #[serde(default)]
    pub campaign_budget_cents: Option<u64>,
    #[serde(default = "default_min_campaign_budget_cents")]
    pub campaign_budget_min_cents: u64,
    #[serde(default = "default_max_campaign_budget_cents")]
    pub campaign_budget_max_cents: u64,
    pub cpc_cents: u64,
    pub impressions: Option<u64>,
    #[serde(default)]
    pub retrieval: RetrievalStrategy,
    #[serde(default)]
    pub ranker: RankerStrategy,
    #[serde(default)]
    pub scoring: ScoringStrategy,
    #[serde(default)]
    pub pacing: PacingStrategy,
    #[serde(default)]
    pub bidding: BiddingStrategy,
    #[serde(default = "default_slate_size")]
    pub slate_size: usize,
}

/// Aggregate 30-day budget and performance summary metrics.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BudgetSummary {
    pub total_budget_cents: u64,
    pub total_spend_cents: u64,
    pub remaining_budget_cents: u64,
    pub utilization_pct: f64,
    pub total_impressions: u64,
    pub total_clicks: u64,
    pub ctr_pct: f64,
    pub exhausted_campaigns: usize,
    pub total_campaigns: usize,
    pub insufficient_budget_opportunities: u64,
    pub min_daily_spend_cents: u64,
    pub mean_daily_spend_cents: f64,
    pub max_daily_spend_cents: u64,
}

/// Single day's budget spend, cumulative utilization, and active-campaign metrics.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DailyBudgetPoint {
    pub day: usize,
    pub impressions: u64,
    pub clicks: u64,
    pub daily_spend_cents: u64,
    pub cumulative_spend_cents: u64,
    pub remaining_budget_cents: u64,
    pub utilization_pct: f64,
    pub active_campaigns_end_of_day: usize,
    pub insufficient_budget: u64,
}

/// Compact 30-day budget summary and daily spend time-series (`budget_timeseries.json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BudgetTimeSeries {
    pub schema_version: u32,
    pub currency: String,
    pub config: TimeSeriesConfig,
    #[serde(default)]
    pub strategy: StrategyProfile,
    pub summary: BudgetSummary,
    pub days: Vec<DailyBudgetPoint>,
}

/// In-memory output of a validated 30-day simulation run.
#[derive(Clone, Debug, PartialEq)]
pub struct RunResult {
    pub report: OwnedReport,
    pub timeseries: BudgetTimeSeries,
    pub analysis: SimulationAnalysis,
}

/// Paths and in-memory results produced by [`run_and_export`] or [`export_artifacts`].
#[derive(Clone, Debug, PartialEq)]
pub struct RunArtifacts {
    pub result: RunResult,
    pub output_dir: PathBuf,
    pub simulation_json: PathBuf,
    pub timeseries_json: PathBuf,
    pub timeseries_csv: PathBuf,
    pub budget_plot_svg: PathBuf,
    pub analysis_json: PathBuf,
    pub svg: String,
}

/// Error returned by `sim-agent` operations.
#[derive(Debug)]
pub enum Error {
    /// Failed to open or read a marketplace file.
    MarketplaceIo { path: PathBuf, source: io::Error },
    /// Failed to parse a marketplace JSON file.
    MarketplaceParse {
        path: PathBuf,
        source: serde_json::Error,
    },
    /// Upstream simulator rejected the configuration or marketplace.
    Simulation(io::Error),
    /// Ledger invariant audit detected an inconsistency in a report.
    LedgerAudit(String),
    /// Failed to create directories or read/write artifact files.
    ArtifactIo { path: PathBuf, source: io::Error },
    /// Failed to serialize or deserialize JSON artifacts.
    SerializeJson {
        path: PathBuf,
        source: serde_json::Error,
    },
}

impl Error {
    /// Return `true` when the failure stems from invalid caller parameters, simulator input
    /// rejection, or ledger invariant validation failure.
    pub fn is_invalid_input(&self) -> bool {
        matches!(self, Self::Simulation(_) | Self::LedgerAudit(_))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MarketplaceIo { path, source } => {
                write!(
                    f,
                    "failed to read marketplace '{}': {source}",
                    path.display()
                )
            }
            Self::MarketplaceParse { path, source } => {
                write!(
                    f,
                    "failed to parse marketplace JSON '{}': {source}",
                    path.display()
                )
            }
            Self::Simulation(source) => write!(f, "simulation failed: {source}"),
            Self::LedgerAudit(reason) => write!(f, "ledger audit failed: {reason}"),
            Self::ArtifactIo { path, source } => {
                write!(f, "artifact I/O error for '{}': {source}", path.display())
            }
            Self::SerializeJson { path, source } => {
                write!(f, "JSON error for '{}': {source}", path.display())
            }
        }
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::MarketplaceIo { source, .. }
            | Self::Simulation(source)
            | Self::ArtifactIo { source, .. } => Some(source),
            Self::MarketplaceParse { source, .. } | Self::SerializeJson { source, .. } => {
                Some(source)
            }
            Self::LedgerAudit(_) => None,
        }
    }
}

/// Execute the real 30-day marketplace simulator, audit every ledger invariant, and derive
/// the 30-day [`BudgetTimeSeries`].
pub fn run_simulation(config: &AgentConfig) -> Result<RunResult, Error> {
    let marketplace = match &config.marketplace_path {
        Some(path) => load_marketplace(path)?,
        None => generate_marketplace(config.marketplace_seed),
    };
    let effective_marketplace_seed = marketplace.seed;

    let report = ads_marketplace_simulation::run(&marketplace, config.simulation_config())
        .map_err(Error::Simulation)?
        .into_owned_report();

    let timeseries = timeseries_from_owned_report(&report, Some(effective_marketplace_seed))?;
    let analysis = analyze_report(&report).map_err(|err| Error::LedgerAudit(err.to_string()))?;
    Ok(RunResult {
        report,
        timeseries,
        analysis,
    })
}

/// Execute [`run_simulation`] and atomically export all 5 artifacts into `output_dir`.
///
/// The simulation and in-memory formatting run before `output_dir` is created so failed runs
/// leave no empty directories or partial files behind.
pub fn run_and_export(config: &AgentConfig, output_dir: &Path) -> Result<RunArtifacts, Error> {
    validate_output_dir(output_dir)?;
    let result = run_simulation(config)?;
    export_artifacts(result, output_dir)
}

/// Atomically export `simulation.json`, `budget_timeseries.json`, `budget_timeseries.csv`,
/// `budget_spent_30d.svg`, and `analysis.json` for an existing [`RunResult`].
pub fn export_artifacts(result: RunResult, output_dir: &Path) -> Result<RunArtifacts, Error> {
    validate_output_dir(output_dir)?;

    let simulation_json = output_dir.join(SIMULATION_REPORT_FILENAME);
    let timeseries_json = output_dir.join(TIMESERIES_JSON_FILENAME);
    let timeseries_csv = output_dir.join(TIMESERIES_CSV_FILENAME);
    let budget_plot_svg = output_dir.join(BUDGET_PLOT_SVG_FILENAME);
    let analysis_json = output_dir.join(ANALYSIS_JSON_FILENAME);

    let mut simulation_bytes =
        serde_json::to_vec(&result.report).map_err(|source| Error::SerializeJson {
            path: simulation_json.clone(),
            source,
        })?;
    simulation_bytes.push(b'\n');

    let mut timeseries_bytes =
        serde_json::to_vec_pretty(&result.timeseries).map_err(|source| Error::SerializeJson {
            path: timeseries_json.clone(),
            source,
        })?;
    timeseries_bytes.push(b'\n');

    let mut analysis_bytes =
        serde_json::to_vec_pretty(&result.analysis).map_err(|source| Error::SerializeJson {
            path: analysis_json.clone(),
            source,
        })?;
    analysis_bytes.push(b'\n');

    let csv = render_timeseries_csv(&result.timeseries);
    let svg = crate::plot::render_budget_svg(&result.timeseries);

    fs::create_dir_all(output_dir).map_err(|source| Error::ArtifactIo {
        path: output_dir.to_path_buf(),
        source,
    })?;

    write_atomic_bytes(output_dir, &simulation_json, &simulation_bytes)?;
    write_atomic_bytes(output_dir, &timeseries_json, &timeseries_bytes)?;
    write_atomic_bytes(output_dir, &timeseries_csv, csv.as_bytes())?;
    write_atomic_bytes(output_dir, &budget_plot_svg, svg.as_bytes())?;
    write_atomic_bytes(output_dir, &analysis_json, &analysis_bytes)?;

    Ok(RunArtifacts {
        result,
        output_dir: output_dir.to_path_buf(),
        simulation_json,
        timeseries_json,
        timeseries_csv,
        budget_plot_svg,
        analysis_json,
        svg,
    })
}

/// Programmatically audit all accounting and structural invariants of an [`OwnedReport`].
///
/// Uses checked arithmetic throughout so tampered or malformed reports always return
/// `Err(Error::LedgerAudit(_))` rather than panicking or silently wrapping.
pub fn audit_report(report: &OwnedReport) -> Result<(), Error> {
    if report.schema_version != SCHEMA_VERSION {
        return Err(Error::LedgerAudit(format!(
            "unsupported report schema_version: expected {SCHEMA_VERSION}, got {}",
            report.schema_version
        )));
    }
    if report.currency != CURRENCY {
        return Err(Error::LedgerAudit(format!(
            "unsupported report currency: expected '{CURRENCY}', got '{}'",
            report.currency
        )));
    }
    let cpc = report.config.cpc_cents;
    if cpc == 0 {
        return Err(Error::LedgerAudit(
            "config.cpc_cents must be greater than zero".to_string(),
        ));
    }

    if report.days.len() != DAYS {
        return Err(Error::LedgerAudit(format!(
            "days length mismatch: expected {DAYS}, got {}",
            report.days.len()
        )));
    }

    let mut sum_day_opportunities: u64 = 0;
    let mut sum_day_no_matching: u64 = 0;
    let mut sum_day_insufficient_budget: u64 = 0;
    let mut sum_day_impressions: u64 = 0;
    let mut sum_day_clicks: u64 = 0;
    let mut sum_day_spend_cents: u64 = 0;

    for (idx, day) in report.days.iter().enumerate() {
        let expected_day = idx + 1;
        if day.day != expected_day {
            return Err(Error::LedgerAudit(format!(
                "days[{idx}] has out-of-order day number: expected {expected_day}, got {}",
                day.day
            )));
        }
        if day.activity.clicks > day.activity.impressions {
            return Err(Error::LedgerAudit(format!(
                "day {} clicks ({}) exceed impressions ({})",
                day.day, day.activity.clicks, day.activity.impressions
            )));
        }
        let expected_spend = day.activity.clicks.checked_mul(cpc).ok_or_else(|| {
            Error::LedgerAudit(format!(
                "day {} clicks ({}) * cpc ({cpc}) overflowed u64",
                day.day, day.activity.clicks
            ))
        })?;
        if day.activity.spend_cents != expected_spend {
            return Err(Error::LedgerAudit(format!(
                "day {} spend_cents mismatch: expected clicks * cpc = {expected_spend}, got {}",
                day.day, day.activity.spend_cents
            )));
        }

        let day_opp_sum = day
            .activity
            .impressions
            .checked_add(day.insufficient_budget)
            .and_then(|s| s.checked_add(day.no_matching_category))
            .ok_or_else(|| {
                Error::LedgerAudit(format!("day {} opportunity sum overflowed u64", day.day))
            })?;
        if day_opp_sum != day.opportunities {
            return Err(Error::LedgerAudit(format!(
                "day {} opportunities mismatch: impressions + insufficient_budget + no_matching_category = {day_opp_sum}, got {}",
                day.day, day.opportunities
            )));
        }

        sum_day_opportunities = checked_add(
            sum_day_opportunities,
            day.opportunities,
            "sum(days.opportunities)",
        )?;
        sum_day_no_matching = checked_add(
            sum_day_no_matching,
            day.no_matching_category,
            "sum(days.no_matching_category)",
        )?;
        sum_day_insufficient_budget = checked_add(
            sum_day_insufficient_budget,
            day.insufficient_budget,
            "sum(days.insufficient_budget)",
        )?;
        sum_day_impressions = checked_add(
            sum_day_impressions,
            day.activity.impressions,
            "sum(days.impressions)",
        )?;
        sum_day_clicks = checked_add(sum_day_clicks, day.activity.clicks, "sum(days.clicks)")?;
        sum_day_spend_cents = checked_add(
            sum_day_spend_cents,
            day.activity.spend_cents,
            "sum(days.spend_cents)",
        )?;
    }

    check_eq(
        "summary.opportunities vs sum(days.opportunities)",
        sum_day_opportunities,
        report.summary.opportunities,
    )?;
    check_eq(
        "summary.no_matching_category vs sum(days.no_matching_category)",
        sum_day_no_matching,
        report.summary.no_matching_category,
    )?;
    check_eq(
        "summary.insufficient_budget vs sum(days.insufficient_budget)",
        sum_day_insufficient_budget,
        report.summary.insufficient_budget,
    )?;
    check_eq(
        "summary.impressions vs sum(days.impressions)",
        sum_day_impressions,
        report.summary.activity.impressions,
    )?;
    check_eq(
        "summary.clicks vs sum(days.clicks)",
        sum_day_clicks,
        report.summary.activity.clicks,
    )?;
    check_eq(
        "summary.spend_cents vs sum(days.spend_cents)",
        sum_day_spend_cents,
        report.summary.activity.spend_cents,
    )?;

    let mut campaign_day_totals = vec![Activity::default(); DAYS];
    let mut sum_campaign_budget_cents: u64 = 0;
    let mut sum_campaign_remaining_cents: u64 = 0;

    for campaign in &report.campaigns {
        if campaign.daily.len() != DAYS {
            return Err(Error::LedgerAudit(format!(
                "campaign '{}' daily length mismatch: expected {DAYS}, got {}",
                campaign.id,
                campaign.daily.len()
            )));
        }

        let mut camp_impressions: u64 = 0;
        let mut camp_clicks: u64 = 0;
        let mut camp_spend: u64 = 0;

        for (d, entry) in campaign.daily.iter().enumerate() {
            let expected_day = d + 1;
            if entry.day != expected_day {
                return Err(Error::LedgerAudit(format!(
                    "campaign '{}' daily[{d}] has out-of-order day: expected {expected_day}, got {}",
                    campaign.id, entry.day
                )));
            }
            if entry.activity.clicks > entry.activity.impressions {
                return Err(Error::LedgerAudit(format!(
                    "campaign '{}' day {} clicks ({}) exceed impressions ({})",
                    campaign.id, entry.day, entry.activity.clicks, entry.activity.impressions
                )));
            }
            let expected_entry_spend = entry.activity.clicks.checked_mul(cpc).ok_or_else(|| {
                Error::LedgerAudit(format!(
                    "campaign '{}' day {} clicks * cpc overflowed u64",
                    campaign.id, entry.day
                ))
            })?;
            if entry.activity.spend_cents != expected_entry_spend {
                return Err(Error::LedgerAudit(format!(
                    "campaign '{}' day {} spend_cents mismatch: expected {expected_entry_spend}, got {}",
                    campaign.id, entry.day, entry.activity.spend_cents
                )));
            }

            camp_impressions = checked_add(
                camp_impressions,
                entry.activity.impressions,
                "campaign daily impressions",
            )?;
            camp_clicks = checked_add(camp_clicks, entry.activity.clicks, "campaign daily clicks")?;
            camp_spend = checked_add(
                camp_spend,
                entry.activity.spend_cents,
                "campaign daily spend_cents",
            )?;

            campaign_day_totals[d].impressions = checked_add(
                campaign_day_totals[d].impressions,
                entry.activity.impressions,
                "campaign_day_totals.impressions",
            )?;
            campaign_day_totals[d].clicks = checked_add(
                campaign_day_totals[d].clicks,
                entry.activity.clicks,
                "campaign_day_totals.clicks",
            )?;
            campaign_day_totals[d].spend_cents = checked_add(
                campaign_day_totals[d].spend_cents,
                entry.activity.spend_cents,
                "campaign_day_totals.spend_cents",
            )?;
        }

        if camp_impressions != campaign.totals.impressions {
            return Err(Error::LedgerAudit(format!(
                "campaign '{}' totals.impressions mismatch: expected sum(daily) = {camp_impressions}, got {}",
                campaign.id, campaign.totals.impressions
            )));
        }
        if camp_clicks != campaign.totals.clicks {
            return Err(Error::LedgerAudit(format!(
                "campaign '{}' totals.clicks mismatch: expected sum(daily) = {camp_clicks}, got {}",
                campaign.id, campaign.totals.clicks
            )));
        }
        if camp_spend != campaign.totals.spend_cents {
            return Err(Error::LedgerAudit(format!(
                "campaign '{}' totals.spend_cents mismatch: expected sum(daily) = {camp_spend}, got {}",
                campaign.id, campaign.totals.spend_cents
            )));
        }

        let expected_camp_spend = campaign.totals.clicks.checked_mul(cpc).ok_or_else(|| {
            Error::LedgerAudit(format!(
                "campaign '{}' totals.clicks * cpc overflowed u64",
                campaign.id
            ))
        })?;
        if campaign.totals.spend_cents != expected_camp_spend {
            return Err(Error::LedgerAudit(format!(
                "campaign '{}' totals.spend_cents mismatch: expected clicks * cpc = {expected_camp_spend}, got {}",
                campaign.id, campaign.totals.spend_cents
            )));
        }

        let camp_budget_sum = campaign
            .remaining_budget_cents
            .checked_add(campaign.totals.spend_cents)
            .ok_or_else(|| {
                Error::LedgerAudit(format!(
                    "campaign '{}' remaining_budget_cents + spend_cents overflowed u64",
                    campaign.id
                ))
            })?;
        if camp_budget_sum != campaign.budget_cents {
            return Err(Error::LedgerAudit(format!(
                "campaign '{}' budget balance mismatch: remaining ({}) + spend ({}) = {camp_budget_sum}, expected budget_cents = {}",
                campaign.id,
                campaign.remaining_budget_cents,
                campaign.totals.spend_cents,
                campaign.budget_cents
            )));
        }

        sum_campaign_budget_cents = checked_add(
            sum_campaign_budget_cents,
            campaign.budget_cents,
            "sum(campaigns.budget_cents)",
        )?;
        sum_campaign_remaining_cents = checked_add(
            sum_campaign_remaining_cents,
            campaign.remaining_budget_cents,
            "sum(campaigns.remaining_budget_cents)",
        )?;
    }

    for (d, day) in report.days.iter().enumerate() {
        if campaign_day_totals[d].impressions != day.activity.impressions {
            return Err(Error::LedgerAudit(format!(
                "day {} impressions mismatch between campaigns ({}) and days ({})",
                day.day, campaign_day_totals[d].impressions, day.activity.impressions
            )));
        }
        if campaign_day_totals[d].clicks != day.activity.clicks {
            return Err(Error::LedgerAudit(format!(
                "day {} clicks mismatch between campaigns ({}) and days ({})",
                day.day, campaign_day_totals[d].clicks, day.activity.clicks
            )));
        }
        if campaign_day_totals[d].spend_cents != day.activity.spend_cents {
            return Err(Error::LedgerAudit(format!(
                "day {} spend_cents mismatch between campaigns ({}) and days ({})",
                day.day, campaign_day_totals[d].spend_cents, day.activity.spend_cents
            )));
        }
    }

    check_eq(
        "summary.budget_cents vs sum(campaigns.budget_cents)",
        sum_campaign_budget_cents,
        report.summary.budget_cents,
    )?;
    check_eq(
        "summary.remaining_budget_cents vs sum(campaigns.remaining_budget_cents)",
        sum_campaign_remaining_cents,
        report.summary.remaining_budget_cents,
    )?;

    let summary_budget_sum = report
        .summary
        .remaining_budget_cents
        .checked_add(report.summary.activity.spend_cents)
        .ok_or_else(|| {
            Error::LedgerAudit(
                "summary.remaining_budget_cents + summary.spend_cents overflowed u64".to_string(),
            )
        })?;
    check_eq(
        "summary.remaining_budget_cents + summary.spend_cents vs summary.budget_cents",
        report.summary.budget_cents,
        summary_budget_sum,
    )?;

    let expected_summary_spend = report
        .summary
        .activity
        .clicks
        .checked_mul(cpc)
        .ok_or_else(|| {
            Error::LedgerAudit("summary.clicks * cpc_cents overflowed u64".to_string())
        })?;
    check_eq(
        "summary.spend_cents vs summary.clicks * cpc_cents",
        expected_summary_spend,
        report.summary.activity.spend_cents,
    )?;

    let summary_opp_sum = report
        .summary
        .activity
        .impressions
        .checked_add(report.summary.insufficient_budget)
        .and_then(|s| s.checked_add(report.summary.no_matching_category))
        .ok_or_else(|| Error::LedgerAudit("summary opportunity sum overflowed u64".to_string()))?;
    check_eq(
        "summary.opportunities vs impressions + insufficient_budget + no_matching_category",
        report.summary.opportunities,
        summary_opp_sum,
    )?;

    if let Some(target_impressions) = report.config.impressions {
        check_eq(
            "summary.impressions vs config.impressions target",
            target_impressions,
            report.summary.activity.impressions,
        )?;
    }

    Ok(())
}

/// Audit a borrowed simulator [`Report`] and derive its 30-day [`BudgetTimeSeries`].
pub fn build_budget_timeseries(
    report: &Report<'_>,
    marketplace_seed: Option<i64>,
) -> Result<BudgetTimeSeries, Error> {
    let owned = report.to_owned_report();
    timeseries_from_owned_report(&owned, marketplace_seed)
}

/// Audit an [`OwnedReport`] and derive its 30-day [`BudgetTimeSeries`].
pub fn timeseries_from_owned_report(
    report: &OwnedReport,
    marketplace_seed: Option<i64>,
) -> Result<BudgetTimeSeries, Error> {
    audit_report(report)?;

    let cpc = report.config.cpc_cents;
    let total_budget_cents = report.summary.budget_cents;
    let mut campaign_remaining: Vec<u64> = report
        .campaigns
        .iter()
        .map(|campaign| campaign.budget_cents)
        .collect();

    let mut cumulative_spend_cents: u64 = 0;
    let mut days = Vec::with_capacity(DAYS);

    for (d, day) in report.days.iter().enumerate() {
        for (idx, campaign) in report.campaigns.iter().enumerate() {
            campaign_remaining[idx] = campaign_remaining[idx]
                .checked_sub(campaign.daily[d].activity.spend_cents)
                .ok_or_else(|| {
                    Error::LedgerAudit(format!(
                        "campaign '{}' overspent budget on day {}",
                        campaign.id, day.day
                    ))
                })?;
        }

        let active_campaigns_end_of_day = campaign_remaining
            .iter()
            .filter(|&&remaining| remaining >= cpc)
            .count();

        cumulative_spend_cents = checked_add(
            cumulative_spend_cents,
            day.activity.spend_cents,
            "cumulative_spend_cents",
        )?;
        let remaining_budget_cents = total_budget_cents
            .checked_sub(cumulative_spend_cents)
            .ok_or_else(|| {
                Error::LedgerAudit(format!(
                    "cumulative spend ({cumulative_spend_cents}) exceeded total budget ({total_budget_cents}) on day {}",
                    day.day
                ))
            })?;

        days.push(DailyBudgetPoint {
            day: day.day,
            impressions: day.activity.impressions,
            clicks: day.activity.clicks,
            daily_spend_cents: day.activity.spend_cents,
            cumulative_spend_cents,
            remaining_budget_cents,
            utilization_pct: pct(cumulative_spend_cents, total_budget_cents),
            active_campaigns_end_of_day,
            insufficient_budget: day.insufficient_budget,
        });
    }

    let exhausted_campaigns = report
        .campaigns
        .iter()
        .filter(|campaign| campaign.remaining_budget_cents < cpc)
        .count();
    let total_campaigns = report.campaigns.len();
    let min_daily_spend_cents = days.iter().map(|d| d.daily_spend_cents).min().unwrap_or(0);
    let max_daily_spend_cents = days.iter().map(|d| d.daily_spend_cents).max().unwrap_or(0);
    let mean_daily_spend_cents = (report.summary.activity.spend_cents as f64) / (DAYS as f64);

    let summary = BudgetSummary {
        total_budget_cents,
        total_spend_cents: report.summary.activity.spend_cents,
        remaining_budget_cents: report.summary.remaining_budget_cents,
        utilization_pct: pct(report.summary.activity.spend_cents, total_budget_cents),
        total_impressions: report.summary.activity.impressions,
        total_clicks: report.summary.activity.clicks,
        ctr_pct: pct(
            report.summary.activity.clicks,
            report.summary.activity.impressions,
        ),
        exhausted_campaigns,
        total_campaigns,
        insufficient_budget_opportunities: report.summary.insufficient_budget,
        min_daily_spend_cents,
        mean_daily_spend_cents,
        max_daily_spend_cents,
    };

    let strategy = StrategyProfile::new(
        report.config.retrieval,
        report.config.ranker,
        report.config.scoring,
        report.config.pacing,
        report.config.bidding,
        report.config.slate_size,
    );

    Ok(BudgetTimeSeries {
        schema_version: report.schema_version,
        currency: report.currency.clone(),
        config: TimeSeriesConfig {
            marketplace_seed,
            sim_seed: report.config.seed,
            campaign_budget_cents: report.config.campaign_budget_cents,
            campaign_budget_min_cents: report.config.campaign_budget_min_cents,
            campaign_budget_max_cents: report.config.campaign_budget_max_cents,
            cpc_cents: report.config.cpc_cents,
            impressions: report.config.impressions,
            retrieval: report.config.retrieval,
            ranker: report.config.ranker,
            scoring: report.config.scoring,
            pacing: report.config.pacing,
            bidding: report.config.bidding,
            slate_size: report.config.slate_size,
        },
        strategy,
        summary,
        days,
    })
}

/// Load a [`BudgetTimeSeries`] from either a `budget_timeseries.json` file or a raw
/// `simulation.json` ([`OwnedReport`]) file.
pub fn load_timeseries_from_path(path: &Path) -> Result<BudgetTimeSeries, Error> {
    let bytes = fs::read(path).map_err(|source| Error::ArtifactIo {
        path: path.to_path_buf(),
        source,
    })?;

    match serde_json::from_slice::<BudgetTimeSeries>(&bytes) {
        Ok(series) => {
            if series.schema_version != SCHEMA_VERSION {
                return Err(Error::LedgerAudit(format!(
                    "unsupported budget_timeseries schema_version: expected {SCHEMA_VERSION}, got {}",
                    series.schema_version
                )));
            }
            if series.days.len() != DAYS {
                return Err(Error::LedgerAudit(format!(
                    "budget_timeseries days length mismatch: expected {DAYS}, got {}",
                    series.days.len()
                )));
            }
            Ok(series)
        }
        Err(ts_err) => match serde_json::from_slice::<OwnedReport>(&bytes) {
            Ok(report) => timeseries_from_owned_report(&report, None),
            Err(report_err) => {
                let is_report_shape = serde_json::from_slice::<serde_json::Value>(&bytes)
                    .ok()
                    .and_then(|v| v.as_object().map(|obj| obj.contains_key("campaigns")))
                    .unwrap_or(false);
                Err(Error::SerializeJson {
                    path: path.to_path_buf(),
                    source: if is_report_shape { report_err } else { ts_err },
                })
            }
        },
    }
}

/// Render a [`BudgetTimeSeries`] as deterministic CSV with `\n` line endings.
pub fn render_timeseries_csv(timeseries: &BudgetTimeSeries) -> String {
    let mut csv = String::with_capacity(256 + timeseries.days.len() * 64);
    csv.push_str(
        "day,impressions,clicks,daily_spend_cents,cumulative_spend_cents,remaining_budget_cents,utilization_pct,active_campaigns_end_of_day,insufficient_budget\n",
    );
    for point in &timeseries.days {
        let _ = writeln!(
            csv,
            "{},{},{},{},{},{},{:.6},{},{}",
            point.day,
            point.impressions,
            point.clicks,
            point.daily_spend_cents,
            point.cumulative_spend_cents,
            point.remaining_budget_cents,
            sanitize_pct(point.utilization_pct),
            point.active_campaigns_end_of_day,
            point.insufficient_budget,
        );
    }
    csv
}

fn load_marketplace(path: &Path) -> Result<Marketplace, Error> {
    let file = File::open(path).map_err(|source| Error::MarketplaceIo {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_reader(BufReader::new(file)).map_err(|source| Error::MarketplaceParse {
        path: path.to_path_buf(),
        source,
    })
}

fn validate_output_dir(output_dir: &Path) -> Result<(), Error> {
    if output_dir.as_os_str().is_empty() {
        return Err(Error::ArtifactIo {
            path: output_dir.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::InvalidInput,
                "output directory path must not be empty",
            ),
        });
    }
    Ok(())
}

fn write_atomic_bytes(output_dir: &Path, target: &Path, bytes: &[u8]) -> Result<(), Error> {
    let mut temporary = NamedTempFile::new_in(output_dir).map_err(|source| Error::ArtifactIo {
        path: target.to_path_buf(),
        source,
    })?;
    {
        let mut writer = BufWriter::new(temporary.as_file_mut());
        writer
            .write_all(bytes)
            .and_then(|()| writer.flush())
            .map_err(|source| Error::ArtifactIo {
                path: target.to_path_buf(),
                source,
            })?;
    }
    temporary.persist(target).map_err(|err| Error::ArtifactIo {
        path: target.to_path_buf(),
        source: err.error,
    })?;
    Ok(())
}

fn checked_add(lhs: u64, rhs: u64, context: &str) -> Result<u64, Error> {
    lhs.checked_add(rhs)
        .ok_or_else(|| Error::LedgerAudit(format!("{context} overflowed u64")))
}

fn check_eq(label: &str, expected: u64, actual: u64) -> Result<(), Error> {
    if expected != actual {
        return Err(Error::LedgerAudit(format!(
            "{label} mismatch: expected {expected}, got {actual}"
        )));
    }
    Ok(())
}

fn pct(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        (numerator as f64) * 100.0 / (denominator as f64)
    }
}

fn sanitize_pct(value: f64) -> f64 {
    if !value.is_finite() || value == 0.0 {
        0.0
    } else {
        value
    }
}

/// Format an unsigned integer with comma thousands separators.
pub(crate) fn format_u64_commas(value: u64) -> String {
    format_u128_commas(u128::from(value))
}

/// Format a `u128` integer with comma thousands separators.
pub(crate) fn format_u128_commas(value: u128) -> String {
    let digits = value.to_string();
    let len = digits.len();
    if len <= 3 {
        return digits;
    }
    let mut out = String::with_capacity(len + (len - 1) / 3);
    let first = len % 3;
    let first = if first == 0 { 3 } else { first };
    out.push_str(&digits[..first]);
    for chunk in digits[first..].as_bytes().chunks(3) {
        out.push(',');
        // SAFETY: `digits` contains only ASCII digits `'0'..='9'`.
        out.push_str(std::str::from_utf8(chunk).expect("ASCII digits"));
    }
    out
}

/// Format USD cents as exact `$X,XXX.XX` without floating-point rounding drift.
pub(crate) fn format_usd_cents(cents: u64) -> String {
    format!("${}.{:02}", format_u64_commas(cents / 100), cents % 100)
}

/// Format fractional USD cents safely as `$X,XXX.XX` (clamping non-finite or `<= 0` to `$0.00`).
pub(crate) fn format_usd_cents_f64(cents: f64) -> String {
    if !cents.is_finite() || cents <= 0.0 {
        return "$0.00".to_string();
    }
    let rounded = cents.round();
    let whole_cents = if rounded >= (u128::MAX as f64) {
        u128::MAX
    } else {
        rounded as u128
    };
    format!(
        "${}.{:02}",
        format_u128_commas(whole_cents / 100),
        whole_cents % 100
    )
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;
    use std::fs;
    use std::path::Path;

    use ads_marketplace_simulation::OwnedReport;
    use world_generator::generate_marketplace;
    use tempfile::TempDir;

    use crate::agent::{
        AgentConfig, BudgetTimeSeries, DAYS, Error, audit_report, build_budget_timeseries,
        load_timeseries_from_path, render_timeseries_csv, run_and_export, run_simulation,
        timeseries_from_owned_report,
    };

    #[test]
    fn golden_baseline_1m_impressions_exact_metrics_and_milestones() {
        let result = run_simulation(&AgentConfig::default()).unwrap();
        let ts = &result.timeseries;

        assert_eq!(ts.schema_version, 1);
        assert_eq!(ts.currency, "USD");
        assert_eq!(ts.config.marketplace_seed, Some(42));
        assert_eq!(ts.config.sim_seed, 42);
        assert_eq!(ts.config.campaign_budget_cents, None);
        assert_eq!(ts.config.campaign_budget_min_cents, 100_000);
        assert_eq!(ts.config.campaign_budget_max_cents, 1_000_000);
        assert_eq!(ts.config.cpc_cents, 100);
        assert_eq!(ts.config.impressions, Some(1_000_000));
        assert_eq!(ts.config.ranker, crate::DEFAULT_RANKER);
        assert_eq!(ts.config.slate_size, crate::DEFAULT_SLATE_SIZE);

        assert_eq!(ts.summary.total_campaigns, 500);
        assert!(ts.summary.total_budget_cents >= 500 * 100_000);
        assert!(ts.summary.total_budget_cents <= 500 * 1_000_000);
        assert_eq!(
            ts.summary.total_spend_cents + ts.summary.remaining_budget_cents,
            ts.summary.total_budget_cents
        );
        assert_eq!(ts.summary.total_impressions, 1_000_000);
        assert!(ts.summary.total_clicks > 1_500 && ts.summary.total_clicks < 2_500);
        assert_eq!(ts.summary.total_spend_cents, ts.summary.total_clicks * 100);
        assert_eq!(ts.summary.exhausted_campaigns, 0);
        assert_eq!(ts.summary.insufficient_budget_opportunities, 0);

        assert_eq!(ts.days.len(), DAYS);
        assert_eq!(ts.days[0].day, 1);
        assert_eq!(ts.days[0].impressions, 33_334);
        assert_eq!(ts.days[0].active_campaigns_end_of_day, 500);
        assert_eq!(ts.days[0].insufficient_budget, 0);

        assert_eq!(ts.days[29].day, 30);
        assert_eq!(ts.days[29].cumulative_spend_cents, ts.summary.total_spend_cents);
        assert_eq!(ts.days[29].remaining_budget_cents, ts.summary.remaining_budget_cents);

        // Verify all 500 campaigns received randomized budgets in [$1,000, $10,000].
        assert!(
            result
                .report
                .campaigns
                .iter()
                .all(|c| (100_000..=1_000_000).contains(&c.budget_cents))
        );
        let distinct_budgets: std::collections::HashSet<u64> =
            result.report.campaigns.iter().map(|c| c.budget_cents).collect();
        assert!(distinct_budgets.len() > 100);

        // Verify integrated SimulationAnalysis agrees with summary.
        assert_eq!(
            result.analysis.budget_depletion.total_spend_cents,
            ts.summary.total_spend_cents
        );
        assert_eq!(
            result.analysis.ctr_performance.total_clicks,
            ts.summary.total_clicks
        );
        assert!(!result.analysis.findings.is_empty());

        let mut prev_cum = 0;
        for (i, point) in ts.days.iter().enumerate() {
            assert_eq!(point.day, i + 1);
            assert!(point.cumulative_spend_cents >= prev_cum);
            prev_cum = point.cumulative_spend_cents;
            assert_eq!(
                point.cumulative_spend_cents + point.remaining_budget_cents,
                ts.summary.total_budget_cents
            );
            assert_eq!(point.active_campaigns_end_of_day, 500);
        }
    }

    #[test]
    fn natural_replay_matches_360k_impressions_and_r1_baseline_clicks() {
        let cfg = AgentConfig {
            impressions: None,
            ..AgentConfig::default()
        };
        let result = run_simulation(&cfg).unwrap();
        let ts = &result.timeseries;

        assert_eq!(ts.config.impressions, None);
        assert_eq!(ts.days.len(), DAYS);
        assert_eq!(ts.summary.total_impressions, 360_000);
        assert!(ts.summary.total_clicks > 500 && ts.summary.total_clicks < 1_000);
        assert_eq!(ts.summary.total_spend_cents, ts.summary.total_clicks * 100);
        assert_eq!(
            ts.summary.remaining_budget_cents + ts.summary.total_spend_cents,
            ts.summary.total_budget_cents
        );
        assert_eq!(ts.summary.exhausted_campaigns, 0);
        for day in &ts.days {
            assert_eq!(day.impressions, 12_000);
        }
    }

    #[test]
    fn zero_spend_and_zero_budget_edge_cases_have_no_nan_and_zero_active_campaigns() {
        let zero_spend_cfg = AgentConfig {
            campaign_budget_cents: Some(50),
            cpc_cents: 100,
            impressions: None,
            ..AgentConfig::default()
        };
        let ts = run_simulation(&zero_spend_cfg).unwrap().timeseries;
        assert_eq!(ts.summary.total_budget_cents, 25_000);
        assert_eq!(ts.summary.total_spend_cents, 0);
        assert_eq!(ts.summary.remaining_budget_cents, 25_000);
        assert_eq!(ts.summary.utilization_pct, 0.0);
        assert_eq!(ts.summary.total_impressions, 0);
        assert_eq!(ts.summary.total_clicks, 0);
        assert_eq!(ts.summary.ctr_pct, 0.0);
        assert_eq!(ts.summary.exhausted_campaigns, 500);
        assert_eq!(ts.summary.insufficient_budget_opportunities, 18_000);
        for day in &ts.days {
            assert_eq!(day.active_campaigns_end_of_day, 0);
            assert_eq!(day.daily_spend_cents, 0);
            assert_eq!(day.utilization_pct, 0.0);
        }

        let zero_budget_cfg = AgentConfig {
            campaign_budget_cents: Some(0),
            cpc_cents: 100,
            impressions: None,
            ..AgentConfig::default()
        };
        let zero_ts = run_simulation(&zero_budget_cfg).unwrap().timeseries;
        assert_eq!(zero_ts.summary.total_budget_cents, 0);
        assert_eq!(zero_ts.summary.utilization_pct, 0.0);
        assert!(!zero_ts.summary.utilization_pct.is_nan());
        assert!(!zero_ts.summary.ctr_pct.is_nan());
        assert_eq!(zero_ts.summary.exhausted_campaigns, 500);
        for day in &zero_ts.days {
            assert_eq!(day.utilization_pct, 0.0);
            assert!(!day.utilization_pct.is_nan());
        }
    }

    #[test]
    fn budget_exhaustion_tracks_monotonic_active_campaigns_and_cap() {
        let cfg = AgentConfig {
            campaign_budget_cents: Some(100),
            cpc_cents: 100,
            impressions: None,
            ..AgentConfig::default()
        };
        let ts = run_simulation(&cfg).unwrap().timeseries;

        assert_eq!(ts.summary.total_budget_cents, 50_000);
        assert_eq!(ts.summary.exhausted_campaigns, 500);
        assert_eq!(ts.summary.remaining_budget_cents, 0);
        assert_eq!(ts.summary.utilization_pct, 100.0);
        assert!(ts.summary.insufficient_budget_opportunities > 0);
        assert_eq!(ts.days[29].active_campaigns_end_of_day, 0);

        let mut prev_active = ts.summary.total_campaigns;
        let mut prev_cum = 0;
        for day in &ts.days {
            assert!(day.active_campaigns_end_of_day <= prev_active);
            prev_active = day.active_campaigns_end_of_day;
            assert!(day.cumulative_spend_cents >= prev_cum);
            prev_cum = day.cumulative_spend_cents;
        }
        assert_eq!(
            ts.days[29].active_campaigns_end_of_day + ts.summary.exhausted_campaigns,
            ts.summary.total_campaigns
        );
    }

    #[test]
    fn partial_exhaustion_tracks_remaining_active_campaigns() {
        let cfg = AgentConfig {
            campaign_budget_cents: Some(200),
            cpc_cents: 100,
            impressions: None,
            ..AgentConfig::default()
        };
        let ts = run_simulation(&cfg).unwrap().timeseries;

        assert!(ts.summary.exhausted_campaigns > 0);
        assert!(ts.summary.exhausted_campaigns < ts.summary.total_campaigns);
        assert!(ts.summary.remaining_budget_cents > 0);
        assert!(ts.summary.utilization_pct >= 50.0);
        assert_eq!(
            ts.days[29].active_campaigns_end_of_day + ts.summary.exhausted_campaigns,
            ts.summary.total_campaigns
        );

        let mut prev_active = ts.summary.total_campaigns;
        for day in &ts.days {
            assert!(day.active_campaigns_end_of_day <= prev_active);
            prev_active = day.active_campaigns_end_of_day;
        }
    }

    fn sample_report() -> OwnedReport {
        run_simulation(&AgentConfig {
            impressions: None,
            ..AgentConfig::default()
        })
        .unwrap()
        .report
    }

    #[test]
    fn audit_rejects_wrong_day_count_and_out_of_order_days() {
        let mut missing_day = sample_report();
        missing_day.days.pop();
        let err = audit_report(&missing_day).unwrap_err();
        assert!(matches!(err, Error::LedgerAudit(_)));
        assert!(err.is_invalid_input());

        let mut swapped_days = sample_report();
        swapped_days.days.swap(0, 1);
        assert!(matches!(
            audit_report(&swapped_days),
            Err(Error::LedgerAudit(_))
        ));
    }

    #[test]
    fn audit_rejects_header_and_cpc_violations() {
        let mut bad_schema = sample_report();
        bad_schema.schema_version = 99;
        assert!(matches!(
            audit_report(&bad_schema),
            Err(Error::LedgerAudit(_))
        ));

        let mut bad_currency = sample_report();
        bad_currency.currency = "EUR".to_string();
        assert!(matches!(
            audit_report(&bad_currency),
            Err(Error::LedgerAudit(_))
        ));

        let mut zero_cpc = sample_report();
        zero_cpc.config.cpc_cents = 0;
        assert!(matches!(
            audit_report(&zero_cpc),
            Err(Error::LedgerAudit(_))
        ));
    }

    #[test]
    fn audit_rejects_daily_spend_sum_and_opportunity_mismatches() {
        let mut spend_mismatch = sample_report();
        spend_mismatch.days[0].activity.spend_cents += 100;
        assert!(matches!(
            audit_report(&spend_mismatch),
            Err(Error::LedgerAudit(_))
        ));

        let mut opp_mismatch = sample_report();
        opp_mismatch.days[0].opportunities += 1;
        assert!(matches!(
            audit_report(&opp_mismatch),
            Err(Error::LedgerAudit(_))
        ));

        let mut summary_mismatch = sample_report();
        summary_mismatch.summary.activity.spend_cents += 100;
        assert!(matches!(
            audit_report(&summary_mismatch),
            Err(Error::LedgerAudit(_))
        ));
    }

    #[test]
    fn audit_rejects_campaign_daily_and_budget_balance_mismatches() {
        let mut camp_daily_mismatch = sample_report();
        let cpc = camp_daily_mismatch.config.cpc_cents;
        camp_daily_mismatch.campaigns[0].daily[0]
            .activity
            .impressions += 1;
        camp_daily_mismatch.campaigns[0].daily[0].activity.clicks += 1;
        camp_daily_mismatch.campaigns[0].daily[0]
            .activity
            .spend_cents += cpc;
        camp_daily_mismatch.campaigns[0].totals.impressions += 1;
        camp_daily_mismatch.campaigns[0].totals.clicks += 1;
        camp_daily_mismatch.campaigns[0].totals.spend_cents += cpc;
        camp_daily_mismatch.campaigns[0].remaining_budget_cents -= cpc;
        assert!(matches!(
            audit_report(&camp_daily_mismatch),
            Err(Error::LedgerAudit(_))
        ));

        let mut summary_balance = sample_report();
        summary_balance.summary.remaining_budget_cents += 1;
        assert!(matches!(
            audit_report(&summary_balance),
            Err(Error::LedgerAudit(_))
        ));

        let mut camp_balance = sample_report();
        camp_balance.campaigns[0].remaining_budget_cents += 1;
        assert!(matches!(
            audit_report(&camp_balance),
            Err(Error::LedgerAudit(_))
        ));

        let mut camp_days_len = sample_report();
        camp_days_len.campaigns[0].daily.pop();
        assert!(matches!(
            audit_report(&camp_days_len),
            Err(Error::LedgerAudit(_))
        ));
    }

    #[test]
    fn audit_rejects_spend_clicks_cpc_mismatch_target_mismatch_and_overflow() {
        let mut clicks_mismatch = sample_report();
        clicks_mismatch.summary.activity.clicks += 1;
        assert!(matches!(
            audit_report(&clicks_mismatch),
            Err(Error::LedgerAudit(_))
        ));

        let mut target_mismatch = sample_report();
        target_mismatch.config.impressions = Some(123_456);
        assert!(matches!(
            audit_report(&target_mismatch),
            Err(Error::LedgerAudit(_))
        ));

        // Checked arithmetic must return Err without panicking on overflow.
        let mut mul_overflow = sample_report();
        mul_overflow.days[0].activity.impressions = u64::MAX;
        mul_overflow.days[0].activity.clicks = u64::MAX;
        assert!(matches!(
            audit_report(&mul_overflow),
            Err(Error::LedgerAudit(_))
        ));

        let mut add_overflow = sample_report();
        add_overflow.campaigns[0].remaining_budget_cents = u64::MAX;
        add_overflow.campaigns[0].totals.spend_cents = 100;
        add_overflow.campaigns[0].totals.clicks = 1;
        add_overflow.campaigns[0].daily[0].activity.impressions = 1;
        add_overflow.campaigns[0].daily[0].activity.clicks = 1;
        add_overflow.campaigns[0].daily[0].activity.spend_cents = 100;
        for entry in add_overflow.campaigns[0].daily.iter_mut().skip(1) {
            entry.activity.impressions = 0;
            entry.activity.clicks = 0;
            entry.activity.spend_cents = 0;
        }
        add_overflow.campaigns[0].totals.impressions = 1;
        assert!(matches!(
            audit_report(&add_overflow),
            Err(Error::LedgerAudit(_))
        ));
    }

    #[test]
    fn owned_report_and_borrowed_report_timeseries_round_trip_parity() {
        let marketplace = generate_marketplace(42);
        let sim_cfg = ads_marketplace_simulation::Config {
            seed: 42,
            campaign_budget_cents: None,
            cpc_cents: 100,
            impressions: None,
            ..ads_marketplace_simulation::Config::default()
        };
        let borrowed_report = ads_marketplace_simulation::run(&marketplace, sim_cfg).unwrap();
        let ts_from_borrowed = build_budget_timeseries(&borrowed_report, Some(42)).unwrap();

        let run = run_simulation(&AgentConfig {
            impressions: None,
            ..AgentConfig::default()
        })
        .unwrap();
        assert_eq!(ts_from_borrowed, run.timeseries);

        let report_json = serde_json::to_string(&run.report).unwrap();
        let parsed_owned: OwnedReport = serde_json::from_str(&report_json).unwrap();
        let ts_from_owned = timeseries_from_owned_report(&parsed_owned, Some(42)).unwrap();
        assert_eq!(ts_from_owned, run.timeseries);

        // Standalone OwnedReport without marketplace_seed sets marketplace_seed = None.
        let ts_no_mkt_seed = timeseries_from_owned_report(&parsed_owned, None).unwrap();
        assert_eq!(ts_no_mkt_seed.config.marketplace_seed, None);
        assert_eq!(ts_no_mkt_seed.summary, run.timeseries.summary);
        assert_eq!(ts_no_mkt_seed.days, run.timeseries.days);
        let no_seed_json = serde_json::to_string(&ts_no_mkt_seed).unwrap();
        assert!(no_seed_json.contains("\"marketplace_seed\":null"));
        let reparsed: BudgetTimeSeries = serde_json::from_str(&no_seed_json).unwrap();
        assert_eq!(reparsed, ts_no_mkt_seed);
    }

    #[test]
    fn load_timeseries_from_path_supports_both_timeseries_and_simulation_report() {
        let dir = TempDir::new().unwrap();
        let artifacts = run_and_export(
            &AgentConfig {
                impressions: None,
                ..AgentConfig::default()
            },
            dir.path(),
        )
        .unwrap();

        let from_ts = load_timeseries_from_path(&artifacts.timeseries_json).unwrap();
        assert_eq!(from_ts, artifacts.result.timeseries);

        let from_sim = load_timeseries_from_path(&artifacts.simulation_json).unwrap();
        assert_eq!(from_sim.config.marketplace_seed, None);
        assert_eq!(from_sim.summary, artifacts.result.timeseries.summary);
        assert_eq!(from_sim.days, artifacts.result.timeseries.days);

        let bad_file = dir.path().join("bad.json");
        fs::write(&bad_file, "{not valid json").unwrap();
        assert!(matches!(
            load_timeseries_from_path(&bad_file),
            Err(Error::SerializeJson { .. })
        ));
    }

    #[test]
    fn negative_marketplace_seed_and_file_seed_precedence() {
        let neg = run_simulation(&AgentConfig {
            marketplace_seed: -7,
            impressions: Some(3_000),
            ..AgentConfig::default()
        })
        .unwrap();
        let pos = run_simulation(&AgentConfig {
            marketplace_seed: 7,
            impressions: Some(3_000),
            ..AgentConfig::default()
        })
        .unwrap();
        assert_eq!(neg.timeseries.config.marketplace_seed, Some(-7));
        assert_eq!(pos.timeseries.config.marketplace_seed, Some(7));
        assert_ne!(neg.report, pos.report);

        // Loaded --marketplace file seed takes precedence over config.marketplace_seed.
        let dir = TempDir::new().unwrap();
        let mkt_path = dir.path().join("marketplace.json");
        let mkt = generate_marketplace(-9);
        fs::write(&mkt_path, serde_json::to_vec(&mkt).unwrap()).unwrap();

        let loaded = run_simulation(&AgentConfig {
            marketplace_path: Some(mkt_path),
            marketplace_seed: 999,
            impressions: Some(3_000),
            ..AgentConfig::default()
        })
        .unwrap();
        assert_eq!(loaded.timeseries.config.marketplace_seed, Some(-9));
    }

    #[test]
    fn error_handling_cpc_zero_unreachable_target_missing_marketplace_and_empty_dir() {
        let dir = TempDir::new().unwrap();
        let nonexistent_out = dir.path().join("should-not-be-created");

        let cpc_err = run_and_export(
            &AgentConfig {
                cpc_cents: 0,
                impressions: None,
                ..AgentConfig::default()
            },
            &nonexistent_out,
        )
        .unwrap_err();
        assert!(matches!(cpc_err, Error::Simulation(_)));
        assert!(cpc_err.is_invalid_input());
        assert!(cpc_err.source().is_some());
        assert!(!nonexistent_out.exists());

        let target_err = run_and_export(
            &AgentConfig {
                campaign_budget_cents: Some(50),
                cpc_cents: 100,
                impressions: Some(10),
                ..AgentConfig::default()
            },
            &nonexistent_out,
        )
        .unwrap_err();
        assert!(matches!(target_err, Error::Simulation(_)));
        assert!(target_err.to_string().contains("target cannot be reached"));
        assert!(!nonexistent_out.exists());

        let missing_err = run_simulation(&AgentConfig {
            marketplace_path: Some(dir.path().join("does-not-exist.json")),
            impressions: None,
            ..AgentConfig::default()
        })
        .unwrap_err();
        assert!(matches!(missing_err, Error::MarketplaceIo { .. }));
        assert!(!missing_err.is_invalid_input());
        assert!(missing_err.source().is_some());

        let empty_dir_err = run_and_export(&AgentConfig::default(), Path::new("")).unwrap_err();
        assert!(matches!(empty_dir_err, Error::ArtifactIo { .. }));
    }

    #[test]
    fn csv_rendering_matches_header_and_row_count() {
        let run = run_simulation(&AgentConfig {
            impressions: None,
            ..AgentConfig::default()
        })
        .unwrap();
        let csv = render_timeseries_csv(&run.timeseries);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 31);
        assert_eq!(
            lines[0],
            "day,impressions,clicks,daily_spend_cents,cumulative_spend_cents,remaining_budget_cents,utilization_pct,active_campaigns_end_of_day,insufficient_budget"
        );
        assert!(csv.ends_with('\n'));
        assert!(!csv.contains('\r'));
    }
}
