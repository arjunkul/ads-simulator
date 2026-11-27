//! Seeded 30-day click-only CPC ad marketplace simulation library.
//!
//! Orchestrates [`world_generator::VisitGenerator`] (visit arrivals, ad slot generation, and
//! [`world_generator::UserActivityOracle`]), [`ad_server::AdServer`] (campaign budget pool + explicit
//! 5-axis `Retrieval × Pacing × Bidding × Scoring × Ranking` strategy pipeline), and `ads-ml`
//! PyTorch daily CTR training (`Score-R1`).

pub mod simulation;

pub use ad_server::{
    ActiveBudgetRetriever, AdResponse, AdServer, AdServerConfig, AdServerSpec, AdsRanker,
    AuctionWinner, Bid, BiddingStrategy, CampaignRetriever, DEFAULT_BIDDING, DEFAULT_RETRIEVAL,
    HOURS_PER_DAY, PacingStrategy, PstTimestamp, R1_SLATE_SIZE, R1RandomBudgetedRanker, R2_EPSILON,
    R2_SLATE_SIZE, R2_UCB_EXPLORATION_C, R2OnlineCtrRanker, R3_SLATE_SIZE, R3GreedyTopKRanker,
    RankerStrategy, RetrievalRequest, RetrievalStrategy, SECONDS_PER_DAY, SECONDS_PER_HOUR,
    ScoringStrategy, ServedAd, StrategyInfo, StrategyProfile, TIMEZONE_LABEL, ad_server_spec,
    build_retriever, compute_bid, daily_budget_cents, pacing_1_unlock_second,
    run_first_price_auction, sample_pst_second_of_day, validate_strategy,
};
pub use simulation::{
    Activity, CAMPAIGNS_PER_ADVERTISER, Campaign, Config, DAYS, DEFAULT_CPC_CENTS,
    DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS, DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS, DEFAULT_PACING,
    DEFAULT_RANKER, DEFAULT_SCORING, DEFAULT_SEED, DEFAULT_SLATE_SIZE, DailyActivity, Day,
    MAX_CLICK_PROBABILITY, OwnedCampaign, OwnedReport, Report, SCHEMA_VERSION, SimulationSpec,
    Summary, run, run_with_log_dir, simulation_spec,
};
pub use world_generator::{
    CLICK_RNG_STREAM, DEFAULT_DAILY_REQUESTS, DayGenerationStats, GeneratedImpression,
    INTEREST_COUNT, PREFERENCE_RNG_STREAM, UserActivityOracle, UserActivityOracleSpec,
    UserPreferenceProfile, VisitGenerator, VisitGeneratorConfig, VisitGeneratorSpec,
    generate_user_preferences, score_preference, user_activity_oracle_spec, visit_generator_spec,
};

#[cfg(test)]
mod tests {
    use world_generator::generate_marketplace;

    use crate::{
        BiddingStrategy, CAMPAIGNS_PER_ADVERTISER, Config, DAYS, DEFAULT_BIDDING,
        DEFAULT_CPC_CENTS, DEFAULT_DAILY_REQUESTS, DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS,
        DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS, DEFAULT_PACING, DEFAULT_RANKER, DEFAULT_RETRIEVAL,
        DEFAULT_SCORING, DEFAULT_SEED, DEFAULT_SLATE_SIZE, MAX_CLICK_PROBABILITY, OwnedReport,
        PacingStrategy, RankerStrategy, RetrievalStrategy, SCHEMA_VERSION, ScoringStrategy,
        SimulationSpec, run, simulation_spec,
    };

    #[test]
    fn simulation_spec_matches_constants_and_round_trips_json() {
        let spec = simulation_spec();
        assert_eq!(spec.schema_version, SCHEMA_VERSION);
        assert_eq!(spec.days, DAYS);
        assert_eq!(spec.campaigns_per_advertiser, CAMPAIGNS_PER_ADVERTISER);
        assert_eq!(spec.max_click_probability, MAX_CLICK_PROBABILITY);
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
        assert_eq!(spec.default_retrieval, DEFAULT_RETRIEVAL);
        assert_eq!(spec.default_ranker, DEFAULT_RANKER);
        assert_eq!(spec.default_scoring, DEFAULT_SCORING);
        assert_eq!(spec.default_pacing, DEFAULT_PACING);
        assert_eq!(spec.default_bidding, DEFAULT_BIDDING);
        assert_eq!(spec.default_slate_size, DEFAULT_SLATE_SIZE);
        assert_eq!(spec.default_daily_requests, DEFAULT_DAILY_REQUESTS);

        let json = serde_json::to_string(&spec).unwrap();
        let parsed: SimulationSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, spec);
    }

    #[test]
    fn public_library_api_and_owned_report_round_trip_r1_baseline() {
        let marketplace = generate_marketplace(42);
        let config = Config {
            seed: DEFAULT_SEED,
            campaign_budget_cents: None,
            campaign_budget_min_cents: DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS,
            campaign_budget_max_cents: DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS,
            cpc_cents: DEFAULT_CPC_CENTS,
            impressions: Some(1_000_000),
            retrieval: RetrievalStrategy::RetrieveV0,
            ranker: RankerStrategy::R1,
            scoring: ScoringStrategy::S0,
            pacing: PacingStrategy::P0,
            bidding: BiddingStrategy::Basebid,
            slate_size: DEFAULT_SLATE_SIZE,
        };
        let report = run(&marketplace, config).unwrap();

        assert_eq!(report.schema_version, SCHEMA_VERSION);
        assert_eq!(report.currency, "USD");
        assert_eq!(report.config, config);
        assert_eq!(report.strategy.retrieval, RetrievalStrategy::RetrieveV0);
        assert_eq!(report.strategy.ranking, RankerStrategy::R1);
        assert_eq!(report.strategy.scoring, ScoringStrategy::S0);
        assert_eq!(report.strategy.pacing, PacingStrategy::P0);
        assert_eq!(report.strategy.bidding, BiddingStrategy::Basebid);
        assert_eq!(report.days.len(), DAYS);
        assert_eq!(report.campaigns.len(), 500);
        let summed_budget: u64 = report.campaigns.iter().map(|c| c.budget_cents).sum();
        assert_eq!(report.summary.budget_cents, summed_budget);
        assert!(
            report.campaigns.iter().all(|c| {
                (DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS..=DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS)
                    .contains(&c.budget_cents)
            })
        );
        assert_eq!(report.summary.activity.impressions, 1_000_000);
        assert!(report.summary.activity.clicks > 0);
        assert_eq!(
            report.summary.activity.spend_cents,
            report.summary.activity.clicks * DEFAULT_CPC_CENTS
        );
        assert_eq!(
            report.summary.remaining_budget_cents + report.summary.activity.spend_cents,
            report.summary.budget_cents
        );

        let report_json = serde_json::to_string(&report).unwrap();
        let owned_from_ref = report.to_owned_report();
        let owned_json = serde_json::to_string(&owned_from_ref).unwrap();
        assert_eq!(report_json, owned_json);

        let parsed_str: OwnedReport = serde_json::from_str(&report_json).unwrap();
        assert_eq!(parsed_str, owned_from_ref);
    }
}
