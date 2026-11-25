//! 30-day CPC simulation orchestrator.
//!
//! Decouples ad-impression generation ([`impression_generator::ImpressionGenerator`]),
//! ad serving & ranking ([`ad_server::AdServer`] with Strategy [`RankerStrategy::R1`]),
//! and shopper click evaluation ([`ads_simulator::score_preference`] × 4%).

use std::{
    collections::HashMap,
    io::{Error, ErrorKind},
};

use ad_server::{
    AdServer, AdServerConfig, CAMPAIGNS_PER_ADVERTISER as SERVER_CAMPAIGNS_PER_ADVERTISER,
    CampaignState, DAYS as SERVER_DAYS, DEFAULT_CPC_CENTS as SERVER_DEFAULT_CPC,
    DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS as SERVER_DEFAULT_MAX_BUDGET,
    DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS as SERVER_DEFAULT_MIN_BUDGET,
    DEFAULT_SEED as SERVER_DEFAULT_SEED, R1_SLATE_SIZE, RankerStrategy,
    SCHEMA_VERSION as SERVER_SCHEMA_VERSION,
};
use ads_simulator::{Item, Marketplace, User, score_preference};
use impression_generator::{
    DEFAULT_DAILY_REQUESTS, ImpressionGenerator, ImpressionGeneratorConfig,
};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = SERVER_SCHEMA_VERSION;
pub const DAYS: usize = SERVER_DAYS;
pub const CAMPAIGNS_PER_ADVERTISER: usize = SERVER_CAMPAIGNS_PER_ADVERTISER;
pub const MAX_CLICK_PROBABILITY: f64 = 0.04;
pub const DEFAULT_SEED: u64 = SERVER_DEFAULT_SEED;
pub const DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS: u64 = SERVER_DEFAULT_MIN_BUDGET;
pub const DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS: u64 = SERVER_DEFAULT_MAX_BUDGET;
pub const DEFAULT_CPC_CENTS: u64 = SERVER_DEFAULT_CPC;
pub const DEFAULT_RANKER: RankerStrategy = RankerStrategy::R1;
pub const DEFAULT_SLATE_SIZE: usize = R1_SLATE_SIZE;
pub const CLICK_RNG_STREAM: u64 = 2;

const fn default_min_campaign_budget_cents() -> u64 {
    DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS
}

const fn default_max_campaign_budget_cents() -> u64 {
    DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS
}

const fn default_slate_size() -> usize {
    DEFAULT_SLATE_SIZE
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SimulationSpec {
    pub schema_version: u32,
    pub days: usize,
    pub campaigns_per_advertiser: usize,
    pub max_click_probability: f64,
    pub default_seed: u64,
    pub default_min_campaign_budget_cents: u64,
    pub default_max_campaign_budget_cents: u64,
    pub default_cpc_cents: u64,
    pub default_ranker: RankerStrategy,
    pub default_slate_size: usize,
    pub default_daily_requests: usize,
}

/// Return the compiled simulation constants and default CLI parameters.
pub const fn simulation_spec() -> SimulationSpec {
    SimulationSpec {
        schema_version: SCHEMA_VERSION,
        days: DAYS,
        campaigns_per_advertiser: CAMPAIGNS_PER_ADVERTISER,
        max_click_probability: MAX_CLICK_PROBABILITY,
        default_seed: DEFAULT_SEED,
        default_min_campaign_budget_cents: DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS,
        default_max_campaign_budget_cents: DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS,
        default_cpc_cents: DEFAULT_CPC_CENTS,
        default_ranker: DEFAULT_RANKER,
        default_slate_size: DEFAULT_SLATE_SIZE,
        default_daily_requests: DEFAULT_DAILY_REQUESTS,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub seed: u64,
    /// Optional fixed per-campaign budget in cents (`None` randomizes each campaign across
    /// `[campaign_budget_min_cents, campaign_budget_max_cents]`).
    #[serde(default)]
    pub campaign_budget_cents: Option<u64>,
    #[serde(default = "default_min_campaign_budget_cents")]
    pub campaign_budget_min_cents: u64,
    #[serde(default = "default_max_campaign_budget_cents")]
    pub campaign_budget_max_cents: u64,
    pub cpc_cents: u64,
    pub impressions: Option<u64>,
    #[serde(default)]
    pub ranker: RankerStrategy,
    #[serde(default = "default_slate_size")]
    pub slate_size: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            seed: DEFAULT_SEED,
            campaign_budget_cents: None,
            campaign_budget_min_cents: DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS,
            campaign_budget_max_cents: DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS,
            cpc_cents: DEFAULT_CPC_CENTS,
            impressions: None,
            ranker: DEFAULT_RANKER,
            slate_size: DEFAULT_SLATE_SIZE,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activity {
    pub impressions: u64,
    pub clicks: u64,
    pub spend_cents: u64,
}

impl Activity {
    #[cfg(test)]
    fn impression(&mut self, cost: Option<u64>) {
        self.impressions += 1;
        if let Some(cost) = cost {
            self.clicks += 1;
            self.spend_cents += cost;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DailyActivity {
    pub day: usize,
    #[serde(flatten)]
    pub activity: Activity,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Campaign<'a> {
    pub id: String,
    pub advertiser_id: &'a str,
    pub ad_item_id: &'a str,
    #[serde(skip)]
    pub item: &'a Item,
    pub category_id: &'a str,
    pub budget_cents: u64,
    pub remaining_budget_cents: u64,
    #[serde(flatten)]
    pub totals: Activity,
    pub daily: Vec<DailyActivity>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedCampaign {
    pub id: String,
    pub advertiser_id: String,
    pub ad_item_id: String,
    pub category_id: String,
    pub budget_cents: u64,
    pub remaining_budget_cents: u64,
    #[serde(flatten)]
    pub totals: Activity,
    pub daily: Vec<DailyActivity>,
}

impl Campaign<'_> {
    pub fn to_owned_campaign(&self) -> OwnedCampaign {
        OwnedCampaign::from(self)
    }
}

impl From<&Campaign<'_>> for OwnedCampaign {
    fn from(campaign: &Campaign<'_>) -> Self {
        Self {
            id: campaign.id.clone(),
            advertiser_id: campaign.advertiser_id.to_owned(),
            ad_item_id: campaign.ad_item_id.to_owned(),
            category_id: campaign.category_id.to_owned(),
            budget_cents: campaign.budget_cents,
            remaining_budget_cents: campaign.remaining_budget_cents,
            totals: campaign.totals,
            daily: campaign.daily.clone(),
        }
    }
}

impl From<Campaign<'_>> for OwnedCampaign {
    fn from(campaign: Campaign<'_>) -> Self {
        Self {
            id: campaign.id,
            advertiser_id: campaign.advertiser_id.to_owned(),
            ad_item_id: campaign.ad_item_id.to_owned(),
            category_id: campaign.category_id.to_owned(),
            budget_cents: campaign.budget_cents,
            remaining_budget_cents: campaign.remaining_budget_cents,
            totals: campaign.totals,
            daily: campaign.daily,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub budget_cents: u64,
    pub remaining_budget_cents: u64,
    #[serde(default)]
    pub ad_requests: u64,
    pub opportunities: u64,
    pub no_matching_category: u64,
    pub insufficient_budget: u64,
    #[serde(flatten)]
    pub activity: Activity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Day {
    pub day: usize,
    #[serde(default)]
    pub ad_requests: u64,
    pub opportunities: u64,
    pub no_matching_category: u64,
    pub insufficient_budget: u64,
    #[serde(flatten)]
    pub activity: Activity,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Report<'a> {
    pub schema_version: u32,
    pub currency: &'a str,
    pub config: Config,
    pub summary: Summary,
    pub days: Vec<Day>,
    pub campaigns: Vec<Campaign<'a>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedReport {
    pub schema_version: u32,
    pub currency: String,
    pub config: Config,
    pub summary: Summary,
    pub days: Vec<Day>,
    pub campaigns: Vec<OwnedCampaign>,
}

impl Report<'_> {
    pub fn to_owned_report(&self) -> OwnedReport {
        OwnedReport::from(self)
    }

    pub fn into_owned_report(self) -> OwnedReport {
        OwnedReport::from(self)
    }
}

impl From<&Report<'_>> for OwnedReport {
    fn from(report: &Report<'_>) -> Self {
        Self {
            schema_version: report.schema_version,
            currency: report.currency.to_owned(),
            config: report.config,
            summary: report.summary,
            days: report.days.clone(),
            campaigns: report.campaigns.iter().map(OwnedCampaign::from).collect(),
        }
    }
}

impl From<Report<'_>> for OwnedReport {
    fn from(report: Report<'_>) -> Self {
        Self {
            schema_version: report.schema_version,
            currency: report.currency.to_owned(),
            config: report.config,
            summary: report.summary,
            days: report.days,
            campaigns: report
                .campaigns
                .into_iter()
                .map(OwnedCampaign::from)
                .collect(),
        }
    }
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message.into())
}

fn build_user_lookup(marketplace: &Marketplace) -> Result<(Vec<String>, HashMap<&str, &User>), Error> {
    let mut user_ids = Vec::with_capacity(marketplace.users.len());
    let mut users = HashMap::with_capacity(marketplace.users.len());
    for user in &marketplace.users {
        if users.insert(user.id.as_str(), user).is_some() {
            return Err(invalid(format!("duplicate user ID {}", user.id)));
        }
        user_ids.push(user.id.clone());
    }
    Ok((user_ids, users))
}

fn build_item_lookup(marketplace: &Marketplace) -> HashMap<(&str, &str), (&str, &Item)> {
    let mut lookup = HashMap::new();
    for adv in &marketplace.advertisers {
        for item in &adv.catalog {
            lookup.insert((adv.id.as_str(), item.id.as_str()), (adv.id.as_str(), item));
        }
    }
    lookup
}

fn map_campaigns<'a>(
    states: Vec<CampaignState>,
    item_lookup: &HashMap<(&'a str, &'a str), (&'a str, &'a Item)>,
) -> Result<Vec<Campaign<'a>>, Error> {
    states
        .into_iter()
        .map(|state| {
            let &(advertiser_id, item) = item_lookup
                .get(&(state.advertiser_id.as_str(), state.ad_item_id.as_str()))
                .ok_or_else(|| {
                    invalid(format!(
                        "campaign '{}' references unknown catalog item '{}'",
                        state.id, state.ad_item_id
                    ))
                })?;
            Ok(Campaign {
                id: state.id,
                advertiser_id,
                ad_item_id: item.id.as_str(),
                item,
                category_id: item.category_id.as_str(),
                budget_cents: state.budget_cents,
                remaining_budget_cents: state.remaining_budget_cents,
                totals: Activity {
                    impressions: state.totals.impressions,
                    clicks: state.totals.clicks,
                    spend_cents: state.totals.spend_cents,
                },
                daily: state
                    .daily
                    .into_iter()
                    .map(|d| DailyActivity {
                        day: d.day,
                        activity: Activity {
                            impressions: d.activity.impressions,
                            clicks: d.activity.clicks,
                            spend_cents: d.activity.spend_cents,
                        },
                    })
                    .collect(),
            })
        })
        .collect()
}

/// Run the 30-day CPC simulation by wiring:
/// 1. [`ImpressionGenerator`] (user-independent request generator operating only on `user_ids`),
/// 2. [`AdServer`] (stateful budget pool + pluggable [`RankerStrategy`], defaulting to R1 = 20
///    random distinct funded campaigns per `serve_ads(user_id)` call),
/// 3. External shopper click model (`score_preference(user, ad_item) * 0.04`).
pub fn run(marketplace: &Marketplace, config: Config) -> Result<Report<'_>, Error> {
    if marketplace.schema_version != SCHEMA_VERSION || marketplace.currency != "USD" {
        return Err(invalid("expected schema version 1 marketplace data in USD"));
    }
    if config.cpc_cents == 0 {
        return Err(invalid("CPC must be positive"));
    }
    if config.slate_size == 0 {
        return Err(invalid("slate_size must be positive"));
    }

    let (user_ids, user_lookup) = build_user_lookup(marketplace)?;
    let item_lookup = build_item_lookup(marketplace);

    let mut ad_server = AdServer::from_marketplace(
        marketplace,
        AdServerConfig {
            seed: config.seed,
            campaign_budget_cents: config.campaign_budget_cents,
            campaign_budget_min_cents: config.campaign_budget_min_cents,
            campaign_budget_max_cents: config.campaign_budget_max_cents,
            cpc_cents: config.cpc_cents,
            ranker: config.ranker,
            slate_size: config.slate_size,
        },
    )?;
    let budget_cents = ad_server.total_budget_cents();

    let mut impression_generator = ImpressionGenerator::new(
        user_ids,
        ImpressionGeneratorConfig {
            seed: config.seed,
            days: DAYS,
            daily_requests: DEFAULT_DAILY_REQUESTS,
            target_impressions: config.impressions,
        },
    )?;

    let mut click_rng = ChaCha8Rng::seed_from_u64(config.seed);
    click_rng.set_stream(CLICK_RNG_STREAM);

    let mut days = Vec::with_capacity(DAYS);
    let mut totals = Activity::default();
    let mut total_ad_requests = 0u64;
    let mut opportunity_count = 0u64;
    let mut insufficient_budget = 0u64;

    for day in 1..=DAYS {
        let day_stats = impression_generator.run_day(day, &mut ad_server, |impression| {
            let user = user_lookup
                .get(impression.user_id.as_str())
                .expect("impression generator only emits known user IDs");
            let &(_, item) = item_lookup
                .get(&(
                    impression.ad.advertiser_id.as_str(),
                    impression.ad.ad_item_id.as_str(),
                ))
                .expect("ad server only emits known catalog items");
            let score = score_preference(user, item);
            click_rng.random_bool(MAX_CLICK_PROBABILITY * score)
        })?;

        let daily = Day {
            day,
            ad_requests: day_stats.ad_requests,
            opportunities: day_stats.opportunities,
            no_matching_category: 0,
            insufficient_budget: day_stats.insufficient_budget,
            activity: Activity {
                impressions: day_stats.impressions,
                clicks: day_stats.clicks,
                spend_cents: day_stats.spend_cents,
            },
        };

        total_ad_requests += daily.ad_requests;
        opportunity_count += daily.opportunities;
        insufficient_budget += daily.insufficient_budget;
        totals.impressions += daily.activity.impressions;
        totals.clicks += daily.activity.clicks;
        totals.spend_cents += daily.activity.spend_cents;
        days.push(daily);
    }

    let campaigns = map_campaigns(ad_server.into_campaigns(), &item_lookup)?;

    Ok(Report {
        schema_version: SCHEMA_VERSION,
        currency: &marketplace.currency,
        config,
        summary: Summary {
            budget_cents,
            remaining_budget_cents: budget_cents - totals.spend_cents,
            ad_requests: total_ad_requests,
            opportunities: opportunity_count,
            no_matching_category: 0,
            insufficient_budget,
            activity: totals,
        },
        days,
        campaigns,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use ad_server::{R1_SLATE_SIZE, RankerStrategy};
    use ads_simulator::generate_marketplace;

    use super::{
        Activity, CAMPAIGNS_PER_ADVERTISER, Config, DAYS, DEFAULT_CPC_CENTS,
        DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS, DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS, DEFAULT_SEED,
        OwnedReport, run,
    };

    #[test]
    fn nonclick_impressions_do_not_spend_budget() {
        let mut activity = Activity::default();
        activity.impression(None);
        activity.impression(None);
        assert_eq!(activity.impressions, 2);
        assert_eq!(activity.clicks, 0);
        assert_eq!(activity.spend_cents, 0);
        activity.impression(Some(73));
        assert_eq!(activity.clicks, 1);
        assert_eq!(activity.spend_cents, 73);
    }

    #[test]
    fn ads_outside_preferred_categories_never_receive_clicks() {
        let mut marketplace = generate_marketplace(42);
        let config = Config {
            seed: 42,
            campaign_budget_cents: Some(10_000),
            cpc_cents: 100,
            impressions: Some(3_000),
            ranker: RankerStrategy::R1,
            slate_size: R1_SLATE_SIZE,
            ..Config::default()
        };
        let initial = run(&marketplace, config).unwrap();
        assert!(initial.summary.activity.clicks > 0);
        assert!(
            initial
                .campaigns
                .iter()
                .all(|campaign| campaign.remaining_budget_cents >= 100)
        );
        let initial_deliveries: Vec<_> = initial
            .campaigns
            .iter()
            .map(|campaign| campaign.totals.impressions)
            .collect();

        for user in &mut marketplace.users {
            user.preferred_category_ids = vec!["no-campaign-category".to_owned()];
        }
        let report = run(&marketplace, config).unwrap();
        assert_eq!(report.summary.activity.impressions, 3_000);
        assert_eq!(report.summary.activity.clicks, 0);
        assert_eq!(report.summary.activity.spend_cents, 0);
        assert!(
            report
                .campaigns
                .iter()
                .zip(initial_deliveries)
                .all(|(campaign, expected)| campaign.totals.impressions == expected)
        );
    }

    #[test]
    fn changing_organic_items_or_browsing_events_does_not_change_ad_delivery() {
        let mut marketplace = generate_marketplace(42);
        let config = Config {
            seed: 42,
            campaign_budget_cents: Some(10_000),
            cpc_cents: 100,
            impressions: Some(600),
            ranker: RankerStrategy::R1,
            slate_size: R1_SLATE_SIZE,
            ..Config::default()
        };
        let baseline = run(&marketplace, config).unwrap();
        let baseline_json = serde_json::to_value(&baseline).unwrap();

        for item in &mut marketplace.organic_items {
            item.category_id = "different-organic-category".to_owned();
        }
        marketplace.browsing_events.clear();

        let decoupled = run(&marketplace, config).unwrap();
        assert_eq!(decoupled.summary.activity.impressions, 600);
        assert_eq!(decoupled.summary.no_matching_category, 0);
        assert_eq!(serde_json::to_value(decoupled).unwrap(), baseline_json);
    }

    #[test]
    fn exact_target_is_reproducible_and_ledgers_balance() {
        let marketplace = generate_marketplace(42);
        let config = Config {
            seed: 42,
            campaign_budget_cents: None,
            campaign_budget_min_cents: DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS,
            campaign_budget_max_cents: DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS,
            cpc_cents: 100,
            impressions: Some(1_000_000),
            ranker: RankerStrategy::R1,
            slate_size: R1_SLATE_SIZE,
        };
        let report = run(&marketplace, config).unwrap();
        assert_eq!(report, run(&marketplace, config).unwrap());
        assert_eq!(report.campaigns.len(), 500);
        assert_eq!(report.days.len(), DAYS);
        let summary = &report.summary;
        assert_eq!(summary.activity.impressions, 1_000_000);
        assert!(summary.activity.clicks > 0);
        assert!(summary.ad_requests >= 50_000);
        let expected_total_budget: u64 = report.campaigns.iter().map(|c| c.budget_cents).sum();
        assert_eq!(summary.budget_cents, expected_total_budget);
        assert_eq!(
            summary.remaining_budget_cents + summary.activity.spend_cents,
            summary.budget_cents
        );
        assert_eq!(
            summary.activity.spend_cents,
            summary.activity.clicks * config.cpc_cents
        );

        let mut ads_by_advertiser = HashMap::<_, HashSet<_>>::new();
        for campaign in &report.campaigns {
            assert!(
                ads_by_advertiser
                    .entry(campaign.advertiser_id)
                    .or_default()
                    .insert(campaign.ad_item_id)
            );
            assert!(
                (DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS..=DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS)
                    .contains(&campaign.budget_cents)
            );
            assert_eq!(
                campaign.remaining_budget_cents + campaign.totals.spend_cents,
                campaign.budget_cents
            );
            assert_eq!(
                campaign.totals.spend_cents,
                campaign.totals.clicks * config.cpc_cents
            );
            assert_eq!(campaign.daily.len(), DAYS);
        }
        assert_eq!(ads_by_advertiser.len(), 50);
        assert!(
            ads_by_advertiser
                .values()
                .all(|ads| ads.len() == CAMPAIGNS_PER_ADVERTISER)
        );

        for (i, day) in report.days.iter().enumerate() {
            assert_eq!(day.day, i + 1);
            let quota = if i < 10 { 33_334 } else { 33_333 };
            assert_eq!(day.activity.impressions, quota);
            assert_eq!(
                day.activity.impressions + day.insufficient_budget + day.no_matching_category,
                day.opportunities
            );
            let mut from_campaigns = Activity::default();
            for campaign in &report.campaigns {
                let campaign_day = &campaign.daily[i];
                assert_eq!(campaign_day.day, day.day);
                from_campaigns.impressions += campaign_day.activity.impressions;
                from_campaigns.clicks += campaign_day.activity.clicks;
                from_campaigns.spend_cents += campaign_day.activity.spend_cents;
            }
            assert_eq!(from_campaigns, day.activity);
        }
    }

    #[test]
    fn unaffordable_campaigns_leave_budgets_intact_and_cannot_meet_a_target() {
        let marketplace = generate_marketplace(42);
        let mut config = Config {
            seed: 42,
            campaign_budget_cents: Some(72),
            cpc_cents: 73,
            impressions: None,
            ranker: RankerStrategy::R1,
            slate_size: R1_SLATE_SIZE,
            ..Config::default()
        };
        let report = run(&marketplace, config).unwrap();
        assert_eq!(report.summary.ad_requests, (600 * DAYS) as u64);
        assert_eq!(report.summary.opportunities, (600 * DAYS) as u64);
        assert_eq!(report.summary.activity, Activity::default());
        assert_eq!(
            report.summary.remaining_budget_cents,
            report.summary.budget_cents
        );
        assert_eq!(report.summary.insufficient_budget, (600 * DAYS) as u64);

        config.impressions = Some(1);
        let error = run(&marketplace, config).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("target cannot be reached on day 1")
        );
    }

    #[test]
    fn config_default_matches_exported_defaults() {
        assert_eq!(
            Config::default(),
            Config {
                seed: DEFAULT_SEED,
                campaign_budget_cents: None,
                campaign_budget_min_cents: DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS,
                campaign_budget_max_cents: DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS,
                cpc_cents: DEFAULT_CPC_CENTS,
                impressions: None,
                ranker: RankerStrategy::R1,
                slate_size: R1_SLATE_SIZE,
            }
        );
    }

    #[test]
    fn owned_report_round_trips_natural_mode_json() {
        let marketplace = generate_marketplace(42);
        let report = run(&marketplace, Config::default()).unwrap();
        assert_eq!(report.summary.ad_requests, 18_000);
        assert_eq!(report.summary.activity.impressions, 360_000);
        let borrowed_json = serde_json::to_string(&report).unwrap();
        let owned = report.to_owned_report();
        let owned_json = serde_json::to_string(&owned).unwrap();
        assert_eq!(borrowed_json, owned_json);

        let decoded: OwnedReport = serde_json::from_str(&borrowed_json).unwrap();
        assert_eq!(decoded, owned);
        assert_eq!(report.into_owned_report(), owned);
    }
}
