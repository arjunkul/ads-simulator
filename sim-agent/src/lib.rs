//! Autonomous 30-day ad-marketplace simulation runner, ledger auditor, automated analyzer,
//! and SVG budget plotter.

pub mod agent;
pub mod plot;

use serde::{Deserialize, Serialize};

pub use ad_server::{
    BiddingStrategy, PacingStrategy, R1_SLATE_SIZE, RankerStrategy, RetrievalStrategy,
    ScoringStrategy, StrategyProfile,
};
pub use agent::{
    ANALYSIS_JSON_FILENAME, AgentConfig, BUDGET_PLOT_SVG_FILENAME, BudgetSummary,
    BudgetTimeSeries, CURRENCY, DAYS, DEFAULT_BIDDING, DEFAULT_CPC_CENTS, DEFAULT_IMPRESSIONS,
    DEFAULT_MARKETPLACE_SEED, DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS,
    DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS, DEFAULT_OUTPUT_DIR, DEFAULT_PACING, DEFAULT_RANKER,
    DEFAULT_RETRIEVAL, DEFAULT_SCORING, DEFAULT_SEED, DEFAULT_SLATE_SIZE, DEFAULT_WORLD_MODEL,
    DailyBudgetPoint, Error, RunArtifacts, RunResult, SCHEMA_VERSION, SIMULATION_REPORT_FILENAME,
    TIMESERIES_CSV_FILENAME, TIMESERIES_JSON_FILENAME, TimeSeriesConfig, audit_report,
    build_budget_timeseries, export_artifacts, load_timeseries_from_path, render_timeseries_csv,
    run_and_export, run_simulation, timeseries_from_owned_report,
};
pub use plot::render_budget_svg;
pub use simulation_analyzer::{SimulationAnalysis, analyze_report, render_text_summary};
pub use world_generator::WorldModel;

/// Compiled specification, defaults, outputs, and capabilities of the `sim-agent` crate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimAgentSpec {
    pub crate_name: String,
    pub binary_name: String,
    pub version: String,
    pub schema_version: u32,
    pub currency: String,
    pub days: usize,
    pub default_marketplace_seed: i64,
    pub default_seed: u64,
    pub default_min_campaign_budget_cents: u64,
    pub default_max_campaign_budget_cents: u64,
    pub default_cpc_cents: u64,
    pub default_impressions: u64,
    #[serde(default)]
    pub default_retrieval: RetrievalStrategy,
    pub default_ranker: RankerStrategy,
    #[serde(default)]
    pub default_scoring: ScoringStrategy,
    #[serde(default)]
    pub default_pacing: PacingStrategy,
    #[serde(default)]
    pub default_bidding: BiddingStrategy,
    pub default_slate_size: usize,
    #[serde(default)]
    pub default_world_model: WorldModel,
    pub default_output_dir: String,
    pub output_files: Vec<String>,
    pub cli_flags: Vec<String>,
    pub ledger_invariants: Vec<String>,
    pub plot_panels: Vec<String>,
}

/// Return the live compiled specification of the `sim-agent` crate.
pub fn sim_agent_spec() -> SimAgentSpec {
    SimAgentSpec {
        crate_name: env!("CARGO_PKG_NAME").into(),
        binary_name: "sim-agent".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        schema_version: SCHEMA_VERSION,
        currency: CURRENCY.into(),
        days: DAYS,
        default_marketplace_seed: DEFAULT_MARKETPLACE_SEED,
        default_seed: DEFAULT_SEED,
        default_min_campaign_budget_cents: DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS,
        default_max_campaign_budget_cents: DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS,
        default_cpc_cents: DEFAULT_CPC_CENTS,
        default_impressions: DEFAULT_IMPRESSIONS,
        default_retrieval: DEFAULT_RETRIEVAL,
        default_ranker: DEFAULT_RANKER,
        default_scoring: DEFAULT_SCORING,
        default_pacing: DEFAULT_PACING,
        default_bidding: DEFAULT_BIDDING,
        default_slate_size: DEFAULT_SLATE_SIZE,
        default_world_model: DEFAULT_WORLD_MODEL,
        default_output_dir: DEFAULT_OUTPUT_DIR.into(),
        output_files: vec![
            SIMULATION_REPORT_FILENAME.into(),
            TIMESERIES_JSON_FILENAME.into(),
            TIMESERIES_CSV_FILENAME.into(),
            BUDGET_PLOT_SVG_FILENAME.into(),
            ANALYSIS_JSON_FILENAME.into(),
        ],
        cli_flags: vec![
            "--output-dir".into(),
            "--marketplace".into(),
            "--marketplace-seed".into(),
            "--seed".into(),
            "--campaign-budget-cents".into(),
            "--campaign-budget-min-cents".into(),
            "--campaign-budget-max-cents".into(),
            "--cpc-cents".into(),
            "--impressions".into(),
            "--no-impression-target".into(),
            "--retrieval".into(),
            "--ranker".into(),
            "--scoring".into(),
            "--pacing".into(),
            "--bidding".into(),
            "--slate-size".into(),
            "--world-model".into(),
        ],
        ledger_invariants: vec![
            "days.len() == 30 and sequential day 1..=30".into(),
            "sum(days.spend_cents) == summary.spend_cents".into(),
            "sum(campaigns.daily) == days for impressions, clicks, and spend_cents".into(),
            "remaining_budget_cents + spend_cents == budget_cents (summary and per-campaign)"
                .into(),
            "spend_cents == clicks * cpc_cents (summary, per-day, and per-campaign)".into(),
        ],
        plot_panels: vec![
            "KPI Summary Header".into(),
            "Daily Budget Spent (Days 1-30 Bar Chart + Mean Line)".into(),
            "Cumulative Budget Spent (Days 1-30 Curve + Area + Utilization)".into(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        ANALYSIS_JSON_FILENAME, BUDGET_PLOT_SVG_FILENAME, CURRENCY, DAYS, DEFAULT_BIDDING,
        DEFAULT_CPC_CENTS, DEFAULT_IMPRESSIONS, DEFAULT_MARKETPLACE_SEED,
        DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS, DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS, DEFAULT_OUTPUT_DIR,
        DEFAULT_PACING, DEFAULT_RANKER, DEFAULT_RETRIEVAL, DEFAULT_SCORING, DEFAULT_SEED,
        DEFAULT_SLATE_SIZE, DEFAULT_WORLD_MODEL, SCHEMA_VERSION, SIMULATION_REPORT_FILENAME,
        SimAgentSpec, TIMESERIES_CSV_FILENAME, TIMESERIES_JSON_FILENAME, sim_agent_spec,
    };

    #[test]
    fn sim_agent_spec_matches_defaults_and_round_trips_json() {
        let spec = sim_agent_spec();
        assert_eq!(spec.crate_name, "sim-agent");
        assert_eq!(spec.binary_name, "sim-agent");
        assert_eq!(spec.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(spec.schema_version, SCHEMA_VERSION);
        assert_eq!(spec.currency, CURRENCY);
        assert_eq!(spec.days, DAYS);
        assert_eq!(spec.default_marketplace_seed, DEFAULT_MARKETPLACE_SEED);
        assert_eq!(spec.default_seed, DEFAULT_SEED);
        assert_eq!(
            spec.default_min_campaign_budget_cents,
            DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS
        );
        assert_eq!(
            spec.default_max_campaign_budget_cents,
            DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS
        );
        assert_eq!(spec.default_cpc_cents, DEFAULT_CPC_CENTS);
        assert_eq!(spec.default_impressions, DEFAULT_IMPRESSIONS);
        assert_eq!(spec.default_retrieval, DEFAULT_RETRIEVAL);
        assert_eq!(spec.default_ranker, DEFAULT_RANKER);
        assert_eq!(spec.default_scoring, DEFAULT_SCORING);
        assert_eq!(spec.default_pacing, DEFAULT_PACING);
        assert_eq!(spec.default_bidding, DEFAULT_BIDDING);
        assert_eq!(spec.default_slate_size, DEFAULT_SLATE_SIZE);
        assert_eq!(spec.default_world_model, DEFAULT_WORLD_MODEL);
        assert_eq!(spec.default_output_dir, DEFAULT_OUTPUT_DIR);
        assert_eq!(
            spec.output_files,
            vec![
                SIMULATION_REPORT_FILENAME,
                TIMESERIES_JSON_FILENAME,
                TIMESERIES_CSV_FILENAME,
                BUDGET_PLOT_SVG_FILENAME,
                ANALYSIS_JSON_FILENAME,
            ]
        );
        assert_eq!(spec.cli_flags.len(), 17);
        assert_eq!(spec.ledger_invariants.len(), 5);
        assert_eq!(spec.plot_panels.len(), 3);

        let json = serde_json::to_string(&spec).unwrap();
        let parsed: SimAgentSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, spec);
    }
}
