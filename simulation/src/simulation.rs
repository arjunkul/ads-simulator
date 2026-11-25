use std::{
    collections::{HashMap, HashSet},
    io::{Error, ErrorKind},
    path::PathBuf,
};

use ads_ml::{
    AdCandidate, ImpressionClickRef, ImpressionLogStore, LogisticRegressionModel, PreparedRanker,
    TrainingConfig,
};
use ads_simulator::{Item, Marketplace, User, score_preference};
use clap::ValueEnum;
use rand::{
    Rng, SeedableRng,
    seq::{IndexedRandom, SliceRandom},
};
use rand_chacha::ChaCha8Rng;
use serde::Serialize;

const DAYS: usize = 30;
const CAMPAIGNS_PER_ADVERTISER: usize = 10;
const MAX_CLICK_PROBABILITY: f64 = 0.04;
const TIE_EPSILON: f64 = 1e-12;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    #[default]
    Uniform,
    R1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Config {
    pub seed: u64,
    pub campaign_budget_cents: u64,
    pub cpc_cents: u64,
    pub impressions: Option<u64>,
    pub strategy: Strategy,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub impression_log_dir: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
struct Activity {
    impressions: u64,
    clicks: u64,
    spend_cents: u64,
}

impl Activity {
    fn impression(&mut self, cost: Option<u64>) {
        self.impressions += 1;
        if let Some(cost) = cost {
            self.clicks += 1;
            self.spend_cents += cost;
        }
    }
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct DailyActivity {
    day: usize,
    #[serde(flatten)]
    activity: Activity,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct Campaign<'a> {
    id: String,
    advertiser_id: &'a str,
    ad_item_id: &'a str,
    #[serde(skip)]
    item: &'a Item,
    category_id: &'a str,
    budget_cents: u64,
    remaining_budget_cents: u64,
    #[serde(flatten)]
    totals: Activity,
    daily: Vec<DailyActivity>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct Summary {
    budget_cents: u64,
    remaining_budget_cents: u64,
    opportunities: u64,
    no_matching_category: u64,
    insufficient_budget: u64,
    #[serde(flatten)]
    activity: Activity,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct Day {
    day: usize,
    opportunities: u64,
    no_matching_category: u64,
    insufficient_budget: u64,
    #[serde(flatten)]
    activity: Activity,
}

#[derive(Debug, PartialEq, Serialize)]
struct DailyTrainingSummary {
    trained_after_day: usize,
    impressions: u64,
    clicks: u64,
    unique_groups: usize,
    feature_count: usize,
    iterations: usize,
    converged: bool,
    mean_log_loss: f64,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Report<'a> {
    schema_version: u32,
    currency: &'a str,
    config: Config,
    summary: Summary,
    days: Vec<Day>,
    campaigns: Vec<Campaign<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    training_runs: Vec<DailyTrainingSummary>,
}

struct Opportunity<'a> {
    user: &'a User,
    user_index: usize,
    organic_item: &'a Item,
    organic_category_index: usize,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message.into())
}

fn build_category_table<'a>(
    marketplace: &'a Marketplace,
) -> (Vec<&'a str>, HashMap<&'a str, usize>) {
    let mut category_ids = Vec::new();
    let mut index_by_id = HashMap::new();
    let mut intern = |id: &'a str| {
        if !index_by_id.contains_key(id) {
            let idx = category_ids.len();
            category_ids.push(id);
            index_by_id.insert(id, idx);
        }
    };
    for category in &marketplace.categories {
        intern(&category.id);
    }
    for item in &marketplace.organic_items {
        intern(&item.category_id);
    }
    for advertiser in &marketplace.advertisers {
        for item in &advertiser.catalog {
            intern(&item.category_id);
        }
    }
    (category_ids, index_by_id)
}

fn browsing_opportunities<'a>(
    marketplace: &'a Marketplace,
    category_index: &HashMap<&'a str, usize>,
) -> Result<(Vec<&'a str>, Vec<Opportunity<'a>>), Error> {
    let mut user_ids = Vec::with_capacity(marketplace.users.len());
    let mut users: HashMap<&str, (usize, &User)> = HashMap::with_capacity(marketplace.users.len());
    for (idx, user) in marketplace.users.iter().enumerate() {
        user_ids.push(user.id.as_str());
        users.insert(user.id.as_str(), (idx, user));
    }
    let items: HashMap<&str, &Item> = marketplace
        .organic_items
        .iter()
        .map(|item| (item.id.as_str(), item))
        .collect();
    if users.len() != marketplace.users.len() || items.len() != marketplace.organic_items.len() {
        return Err(invalid("duplicate user or organic item IDs"));
    }

    let opportunities = marketplace
        .browsing_events
        .iter()
        .map(|browse| {
            let &(user_index, user) = users
                .get(browse.user_id.as_str())
                .ok_or_else(|| invalid(format!("unknown browsing user {}", browse.user_id)))?;
            let &organic_item = items.get(browse.organic_item_id.as_str()).ok_or_else(|| {
                invalid(format!("unknown browsing item {}", browse.organic_item_id))
            })?;
            let organic_category_index = category_index[organic_item.category_id.as_str()];
            Ok(Opportunity {
                user,
                user_index,
                organic_item,
                organic_category_index,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;

    Ok((user_ids, opportunities))
}

fn initialize_campaigns<'a>(
    marketplace: &'a Marketplace,
    config: &Config,
    rng: &mut ChaCha8Rng,
) -> Result<Vec<Campaign<'a>>, Error> {
    let mut campaigns = Vec::new();
    let mut advertiser_ids = HashSet::new();
    for advertiser in &marketplace.advertisers {
        if !advertiser_ids.insert(&advertiser.id) {
            return Err(invalid(format!("duplicate advertiser {}", advertiser.id)));
        }
        let distinct: HashSet<_> = advertiser.catalog.iter().map(|item| &item.id).collect();
        if distinct.len() < CAMPAIGNS_PER_ADVERTISER || distinct.len() != advertiser.catalog.len() {
            return Err(invalid(format!(
                "advertiser {} must have at least ten catalog items with unique IDs",
                advertiser.id
            )));
        }

        for (index, item) in advertiser
            .catalog
            .choose_multiple(rng, CAMPAIGNS_PER_ADVERTISER)
            .enumerate()
        {
            campaigns.push(Campaign {
                id: format!("{}-campaign-{:02}", advertiser.id, index + 1),
                advertiser_id: &advertiser.id,
                ad_item_id: &item.id,
                item,
                category_id: &item.category_id,
                budget_cents: config.campaign_budget_cents,
                remaining_budget_cents: config.campaign_budget_cents,
                totals: Activity::default(),
                daily: (1..=DAYS)
                    .map(|day| DailyActivity {
                        day,
                        activity: Activity::default(),
                    })
                    .collect(),
            });
        }
    }
    Ok(campaigns)
}

fn select_ranked_campaign(
    ranker: &PreparedRanker,
    user_index: usize,
    organic_category_index: usize,
    active: &[usize],
    rng: &mut ChaCha8Rng,
) -> usize {
    let mut best_pos = 0;
    let mut best_score = f64::NEG_INFINITY;
    let mut tie_count = 0u64;

    for (pos, &campaign_index) in active.iter().enumerate() {
        let score = ranker.score_logit(user_index, organic_category_index, campaign_index);
        if score > best_score + TIE_EPSILON {
            best_score = score;
            best_pos = pos;
            tie_count = 1;
        } else if (score - best_score).abs() <= TIE_EPSILON {
            tie_count += 1;
            if rng.random_range(0..tie_count) == 0 {
                best_pos = pos;
            }
        }
    }
    best_pos
}

pub fn run(marketplace: &Marketplace, config: Config) -> Result<Report<'_>, Error> {
    if marketplace.schema_version != 1 || marketplace.currency != "USD" {
        return Err(invalid("expected schema version 1 marketplace data in USD"));
    }
    if config.cpc_cents == 0 {
        return Err(invalid("CPC must be positive"));
    }

    let mut ads_rng = ChaCha8Rng::seed_from_u64(config.seed);
    let mut browsing_rng = ads_rng.clone();
    browsing_rng.set_stream(1);
    let mut click_rng = ads_rng.clone();
    click_rng.set_stream(2);

    let (category_ids, category_index) = build_category_table(marketplace);
    let (user_ids, opportunities) = browsing_opportunities(marketplace, &category_index)?;
    let mut campaigns = initialize_campaigns(marketplace, &config, &mut ads_rng)?;
    let budget_cents = config
        .campaign_budget_cents
        .checked_mul(campaigns.len() as u64)
        .ok_or_else(|| invalid("total campaign budget does not fit in the report"))?;
    let mut active: Vec<_> = campaigns
        .iter()
        .enumerate()
        .filter(|(_, campaign)| campaign.remaining_budget_cents >= config.cpc_cents)
        .map(|(index, _)| index)
        .collect();

    let (_temp_log_dir, log_store) = if let Some(ref dir) = config.impression_log_dir {
        let store = ImpressionLogStore::new(dir.clone());
        store.clear()?;
        (None, Some(store))
    } else if config.strategy == Strategy::R1 {
        let temp_dir = tempfile::tempdir()?;
        let store = ImpressionLogStore::new(temp_dir.path());
        store.clear()?;
        (Some(temp_dir), Some(store))
    } else {
        (None, None)
    };

    let mut days = Vec::with_capacity(DAYS);
    let mut training_runs = Vec::new();
    let mut ranker: Option<PreparedRanker> = None;
    let mut totals = Activity::default();
    let mut opportunity_count = 0;
    let mut insufficient_budget = 0;
    let mut order: Vec<_> = (0..opportunities.len()).collect();

    for day in 1..=DAYS {
        let quota = config.impressions.map(|target| {
            let days = DAYS as u64;
            target / days + u64::from(day as u64 <= target % days)
        });
        let mut daily = Day {
            day,
            opportunities: 0,
            no_matching_category: 0,
            insufficient_budget: 0,
            activity: Activity::default(),
        };
        let mut day_writer = log_store
            .as_ref()
            .map(|store| store.create_day_writer(day))
            .transpose()?;

        while quota != Some(daily.activity.impressions) {
            let impressions_before_pass = daily.activity.impressions;
            if quota.is_some() {
                order.shuffle(&mut browsing_rng);
            }
            for &slot in &order {
                daily.opportunities += 1;
                if active.is_empty() {
                    daily.insufficient_budget += 1;
                    continue;
                }
                let opportunity = &opportunities[slot];
                let index = match (config.strategy, ranker.as_ref()) {
                    (Strategy::R1, Some(prepared)) => select_ranked_campaign(
                        prepared,
                        opportunity.user_index,
                        opportunity.organic_category_index,
                        &active,
                        &mut ads_rng,
                    ),
                    _ => ads_rng.random_range(0..active.len()),
                };
                let campaign = &mut campaigns[active[index]];
                let score = score_preference(opportunity.user, campaign.item);
                let cost = click_rng
                    .random_bool(MAX_CLICK_PROBABILITY * score)
                    .then_some(config.cpc_cents);
                if let Some(writer) = day_writer.as_mut() {
                    writer.write_ref(&ImpressionClickRef {
                        day,
                        user_id: &opportunity.user.id,
                        organic_item_id: &opportunity.organic_item.id,
                        organic_category_id: &opportunity.organic_item.category_id,
                        campaign_id: &campaign.id,
                        advertiser_id: campaign.advertiser_id,
                        ad_item_id: campaign.ad_item_id,
                        ad_category_id: campaign.category_id,
                        ad_price_cents: campaign.item.price_cents,
                        cpc_cents: config.cpc_cents,
                        clicked: cost.is_some(),
                    })?;
                }
                if let Some(cost) = cost {
                    campaign.remaining_budget_cents -= cost;
                    if campaign.remaining_budget_cents < config.cpc_cents {
                        active.swap_remove(index);
                    }
                }
                campaign.totals.impression(cost);
                campaign.daily[day - 1].activity.impression(cost);
                daily.activity.impression(cost);
                totals.impression(cost);
                if quota == Some(daily.activity.impressions) {
                    break;
                }
            }
            if quota.is_none() {
                break;
            }
            if daily.activity.impressions == impressions_before_pass {
                return Err(invalid(format!(
                    concat!(
                        "impression target cannot be reached on day {}: ",
                        "no browsing opportunities or no campaigns that can afford the CPC ({} impressions delivered)"
                    ),
                    day, totals.impressions,
                )));
            }
        }
        if let Some(writer) = day_writer {
            writer.finish()?;
        }

        if config.strategy == Strategy::R1
            && day < DAYS
            && let Some(store) = log_store.as_ref()
        {
            let model = LogisticRegressionModel::train_from_log_store(
                store,
                day,
                TrainingConfig::default(),
            )?;
            let summary = model.summary();
            training_runs.push(DailyTrainingSummary {
                trained_after_day: day,
                impressions: summary.impressions,
                clicks: summary.clicks,
                unique_groups: summary.unique_groups,
                feature_count: summary.feature_count,
                iterations: summary.iterations,
                converged: summary.converged,
                mean_log_loss: summary.mean_log_loss,
            });
            let candidates: Vec<AdCandidate<'_>> = campaigns
                .iter()
                .map(|campaign| AdCandidate {
                    campaign_id: campaign.id.as_str(),
                    advertiser_id: campaign.advertiser_id,
                    ad_item_id: campaign.ad_item_id,
                    ad_category_id: campaign.category_id,
                    ad_price_cents: campaign.item.price_cents,
                })
                .collect();
            ranker = Some(model.prepare_ranker(&user_ids, &category_ids, &candidates));
        }

        opportunity_count += daily.opportunities;
        insufficient_budget += daily.insufficient_budget;
        days.push(daily);
    }

    Ok(Report {
        schema_version: 1,
        currency: &marketplace.currency,
        config,
        summary: Summary {
            budget_cents,
            remaining_budget_cents: budget_cents - totals.spend_cents,
            opportunities: opportunity_count,
            no_matching_category: 0,
            insufficient_budget,
            activity: totals,
        },
        days,
        campaigns,
        training_runs,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use ads_ml::ImpressionLogStore;
    use ads_simulator::generate_marketplace;

    use super::{Activity, CAMPAIGNS_PER_ADVERTISER, Config, DAYS, Strategy, run};

    fn uniform_config(
        seed: u64,
        campaign_budget_cents: u64,
        cpc_cents: u64,
        impressions: Option<u64>,
    ) -> Config {
        Config {
            seed,
            campaign_budget_cents,
            cpc_cents,
            impressions,
            strategy: Strategy::Uniform,
            impression_log_dir: None,
        }
    }

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
        let config = uniform_config(42, 10_000, 100, Some(3_000));
        let initial = run(&marketplace, config.clone()).unwrap();
        assert!(initial.summary.activity.clicks > 0);
        // No campaign can exhaust its budget at this scale for this seed.
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
    fn changing_the_organic_category_does_not_change_ad_delivery() {
        let mut marketplace = generate_marketplace(42);
        marketplace.advertisers.truncate(1);
        let ad_category = marketplace.advertisers[0].catalog[0].category_id.clone();
        for item in &mut marketplace.advertisers[0].catalog {
            item.category_id.clone_from(&ad_category);
        }
        for item in &mut marketplace.organic_items {
            item.category_id.clone_from(&ad_category);
        }
        let config = uniform_config(42, 10_000, 100, Some(300));
        let matching = run(&marketplace, config.clone()).unwrap();
        let matching_json = serde_json::to_value(matching).unwrap();

        for item in &mut marketplace.organic_items {
            item.category_id = "different-organic-category".to_owned();
        }
        let mismatched = run(&marketplace, config).unwrap();
        assert_eq!(mismatched.summary.activity.impressions, 300);
        assert_eq!(mismatched.summary.no_matching_category, 0);
        assert_eq!(serde_json::to_value(mismatched).unwrap(), matching_json);
    }

    #[test]
    fn exact_target_is_reproducible_and_ledgers_balance() {
        let marketplace = generate_marketplace(42);
        let config = uniform_config(42, 10_000, 100, Some(1_000_000));
        let report = run(&marketplace, config.clone()).unwrap();
        assert_eq!(report, run(&marketplace, config.clone()).unwrap());
        assert_eq!(report.campaigns.len(), 500);
        assert_eq!(report.days.len(), DAYS);
        assert!(report.training_runs.is_empty());
        let summary = &report.summary;
        assert_eq!(summary.activity.impressions, 1_000_000);
        assert!(summary.activity.clicks > 0);
        assert_eq!(summary.budget_cents, 500 * config.campaign_budget_cents);
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
            assert_eq!(campaign.budget_cents, config.campaign_budget_cents);
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
        let mut config = uniform_config(42, 72, 73, None);
        let report = run(&marketplace, config.clone()).unwrap();
        assert_eq!(
            report.summary.opportunities,
            (marketplace.browsing_events.len() * DAYS) as u64
        );
        assert_eq!(report.summary.activity, Activity::default());
        assert_eq!(
            report.summary.remaining_budget_cents,
            report.summary.budget_cents
        );
        assert!(report.summary.insufficient_budget > 0);

        config.impressions = Some(1);
        let error = run(&marketplace, config).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("target cannot be reached on day 1")
        );
    }

    #[test]
    fn uniform_logs_impressions_to_disk_without_changing_delivery() {
        let marketplace = generate_marketplace(42);
        let base_config = uniform_config(42, 10_000, 100, Some(3_000));
        let baseline = run(&marketplace, base_config).unwrap();

        let temp = tempfile::tempdir().unwrap();
        let logged_config = Config {
            impression_log_dir: Some(temp.path().to_path_buf()),
            ..uniform_config(42, 10_000, 100, Some(3_000))
        };
        let logged = run(&marketplace, logged_config).unwrap();

        assert_eq!(logged.summary, baseline.summary);
        assert_eq!(logged.days, baseline.days);
        assert_eq!(logged.campaigns, baseline.campaigns);

        let store = ImpressionLogStore::new(temp.path());
        assert_eq!(
            store.available_days().unwrap(),
            (1..=DAYS).collect::<Vec<_>>()
        );
        let all_records = store.read_through_day(DAYS).unwrap();
        assert_eq!(
            all_records.len() as u64,
            logged.summary.activity.impressions
        );
        let logged_clicks = all_records.iter().filter(|record| record.clicked).count() as u64;
        assert_eq!(logged_clicks, logged.summary.activity.clicks);
    }

    #[test]
    fn r1_logs_daily_impressions_cold_trains_from_disk_and_outperforms_uniform() {
        let marketplace = generate_marketplace(42);
        let impressions = Some(30_000);
        let uniform_report =
            run(&marketplace, uniform_config(42, 10_000, 100, impressions)).unwrap();

        let temp = tempfile::tempdir().unwrap();
        let r1_config = Config {
            seed: 42,
            campaign_budget_cents: 10_000,
            cpc_cents: 100,
            impressions,
            strategy: Strategy::R1,
            impression_log_dir: Some(temp.path().to_path_buf()),
        };
        let r1_report = run(&marketplace, r1_config.clone()).unwrap();

        // Day 1 has no prior data, so R1 matches Uniform on day 1.
        assert_eq!(r1_report.days[0], uniform_report.days[0]);

        // R1 cold-trains after each day 1..=29 on all cumulative logged impressions so far.
        assert_eq!(r1_report.training_runs.len(), DAYS - 1);
        let mut cumulative_impressions = 0;
        let mut cumulative_clicks = 0;
        for (idx, run_summary) in r1_report.training_runs.iter().enumerate() {
            let day = idx + 1;
            cumulative_impressions += r1_report.days[idx].activity.impressions;
            cumulative_clicks += r1_report.days[idx].activity.clicks;
            assert_eq!(run_summary.trained_after_day, day);
            assert_eq!(run_summary.impressions, cumulative_impressions);
            assert_eq!(run_summary.clicks, cumulative_clicks);
            assert!(run_summary.converged);
        }

        // All 30 daily JSONL log files match the daily report counters.
        let store = ImpressionLogStore::new(temp.path());
        assert_eq!(
            store.available_days().unwrap(),
            (1..=DAYS).collect::<Vec<_>>()
        );
        for day in 1..=DAYS {
            let records = store.read_day(day).unwrap();
            let day_summary = &r1_report.days[day - 1];
            assert_eq!(records.len() as u64, day_summary.activity.impressions);
            let clicks = records.iter().filter(|record| record.clicked).count() as u64;
            assert_eq!(clicks, day_summary.activity.clicks);
            assert!(records.iter().all(|record| record.day == day));
        }

        // Budgets and ledgers balance and R1 achieves substantially higher CTR than Uniform.
        assert_eq!(r1_report.summary.activity.impressions, 30_000);
        assert_eq!(
            r1_report.summary.remaining_budget_cents + r1_report.summary.activity.spend_cents,
            r1_report.summary.budget_cents
        );
        assert_eq!(
            r1_report.summary.activity.spend_cents,
            r1_report.summary.activity.clicks * r1_config.cpc_cents
        );
        assert!(
            r1_report.summary.activity.clicks > uniform_report.summary.activity.clicks * 5,
            "expected R1 ({} clicks) to beat Uniform ({} clicks) by >5x",
            r1_report.summary.activity.clicks,
            uniform_report.summary.activity.clicks,
        );

        // Re-running with the same seed and log directory reproduces the exact report.
        let repeat_dir = tempfile::tempdir().unwrap();
        let repeat_config = Config {
            impressions: Some(6_000),
            impression_log_dir: Some(repeat_dir.path().to_path_buf()),
            ..r1_config
        };
        let first = run(&marketplace, repeat_config.clone()).unwrap();
        let second = run(&marketplace, repeat_config).unwrap();
        assert_eq!(first, second);
    }
}
