//! Seeded 30-day click-only CPC ad marketplace simulation library.
//!
//! Wires [`impression_generator::ImpressionGenerator`] (user-independent ad-request stream),
//! [`ad_server::AdServer`] (campaign budget pool + pluggable [`ad_server::RankerStrategy`],
//! starting with `R1` = 20 random budgeted campaigns), and shopper click evaluation.

pub mod simulation;

pub use ad_server::{
    AdResponse, AdServer, AdServerConfig, AdServerSpec, AdsRanker, R1_SLATE_SIZE,
    R1RandomBudgetedRanker, R2_EPSILON, R2_SLATE_SIZE, R2_UCB_EXPLORATION_C, R2OnlineCtrRanker,
    RankerStrategy, ServedAd, ad_server_spec,
};
pub use impression_generator::{
    DEFAULT_DAILY_REQUESTS, DayGenerationStats, GeneratedImpression, ImpressionGenerator,
    ImpressionGeneratorConfig, ImpressionGeneratorSpec, impression_generator_spec,
};
pub use simulation::{
    Activity, CAMPAIGNS_PER_ADVERTISER, Campaign, Config, DAYS, DEFAULT_CPC_CENTS,
    DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS, DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS, DEFAULT_RANKER,
    DEFAULT_SEED, DEFAULT_SLATE_SIZE, DailyActivity, Day, MAX_CLICK_PROBABILITY, OwnedCampaign,
    OwnedReport, Report, SCHEMA_VERSION, SimulationSpec, Summary, run, simulation_spec,
};

#[cfg(test)]
mod tests {
    use ads_simulator::generate_marketplace;

    use crate::{
        CAMPAIGNS_PER_ADVERTISER, Config, DAYS, DEFAULT_CPC_CENTS, DEFAULT_DAILY_REQUESTS,
        DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS, DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS, DEFAULT_RANKER,
        DEFAULT_SEED, DEFAULT_SLATE_SIZE, MAX_CLICK_PROBABILITY, OwnedReport, RankerStrategy,
        SCHEMA_VERSION, SimulationSpec, run, simulation_spec,
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
        assert_eq!(spec.default_ranker, DEFAULT_RANKER);
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
            ranker: RankerStrategy::R1,
            slate_size: DEFAULT_SLATE_SIZE,
        };
        let report = run(&marketplace, config).unwrap();

        assert_eq!(report.schema_version, SCHEMA_VERSION);
        assert_eq!(report.currency, "USD");
        assert_eq!(report.config, config);
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
