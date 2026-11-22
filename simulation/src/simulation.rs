use std::{
    collections::{HashMap, HashSet},
    io::{Error, ErrorKind},
};

use ads_simulator::{Item, Marketplace, User, score_preference};
use rand::{
    Rng, SeedableRng,
    seq::{IndexedRandom, SliceRandom},
};
use rand_chacha::ChaCha8Rng;
use serde::Serialize;

const DAYS: usize = 30;
const CAMPAIGNS_PER_ADVERTISER: usize = 10;
const MAX_CLICK_PROBABILITY: f64 = 0.04;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Config {
    pub seed: u64,
    pub campaign_budget_cents: u64,
    pub cpc_cents: u64,
    pub impressions: Option<u64>,
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

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Report<'a> {
    schema_version: u32,
    currency: &'a str,
    config: Config,
    summary: Summary,
    days: Vec<Day>,
    campaigns: Vec<Campaign<'a>>,
}

struct Opportunity<'a> {
    category_id: &'a str,
    user: &'a User,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message.into())
}

fn browsing_opportunities(marketplace: &Marketplace) -> Result<Vec<Opportunity<'_>>, Error> {
    let users: HashMap<_, _> = marketplace
        .users
        .iter()
        .map(|user| (&user.id, user))
        .collect();
    let items: HashMap<_, _> = marketplace
        .organic_items
        .iter()
        .map(|item| (&item.id, item))
        .collect();
    if users.len() != marketplace.users.len() || items.len() != marketplace.organic_items.len() {
        return Err(invalid("duplicate user or organic item IDs"));
    }

    marketplace
        .browsing_events
        .iter()
        .map(|browse| {
            let user = users
                .get(&browse.user_id)
                .ok_or_else(|| invalid(format!("unknown browsing user {}", browse.user_id)))?;
            let item = items.get(&browse.organic_item_id).ok_or_else(|| {
                invalid(format!("unknown browsing item {}", browse.organic_item_id))
            })?;
            Ok(Opportunity {
                category_id: &item.category_id,
                user,
            })
        })
        .collect()
}

fn initialize_campaigns<'a>(
    marketplace: &'a Marketplace,
    config: Config,
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

pub fn run(marketplace: &Marketplace, config: Config) -> Result<Report<'_>, Error> {
    if marketplace.schema_version != 1 || marketplace.currency != "USD" {
        return Err(invalid("expected schema version 1 marketplace data in USD"));
    }
    if config.cpc_cents == 0 {
        return Err(invalid("CPC must be positive"));
    }

    let mut rng = ChaCha8Rng::seed_from_u64(config.seed);
    let opportunities = browsing_opportunities(marketplace)?;
    let mut campaigns = initialize_campaigns(marketplace, config, &mut rng)?;
    let budget_cents = config
        .campaign_budget_cents
        .checked_mul(campaigns.len() as u64)
        .ok_or_else(|| invalid("total campaign budget does not fit in the report"))?;
    let mut active: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, campaign) in campaigns.iter().enumerate() {
        let eligible = active.entry(campaign.category_id).or_default();
        if campaign.remaining_budget_cents >= config.cpc_cents {
            eligible.push(i);
        }
    }

    let mut days = Vec::with_capacity(DAYS);
    let mut totals = Activity::default();
    let mut opportunity_count = 0;
    let mut no_matching_category = 0;
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
        while quota != Some(daily.activity.impressions) {
            let impressions_before_pass = daily.activity.impressions;
            if quota.is_some() {
                order.shuffle(&mut rng);
            }
            for &slot in &order {
                let opportunity = &opportunities[slot];
                daily.opportunities += 1;
                let Some(eligible) = active.get_mut(opportunity.category_id) else {
                    daily.no_matching_category += 1;
                    continue;
                };
                if eligible.is_empty() {
                    daily.insufficient_budget += 1;
                    continue;
                }
                let index = rng.random_range(0..eligible.len());
                let campaign = &mut campaigns[eligible[index]];
                let score = score_preference(opportunity.user, campaign.item);
                let cost = rng
                    .random_bool(MAX_CLICK_PROBABILITY * score)
                    .then_some(config.cpc_cents);
                if let Some(cost) = cost {
                    campaign.remaining_budget_cents -= cost;
                    if campaign.remaining_budget_cents < config.cpc_cents {
                        eligible.swap_remove(index);
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
                        "no funded campaigns match the browsing opportunities ({} impressions delivered)"
                    ),
                    day, totals.impressions,
                )));
            }
        }
        opportunity_count += daily.opportunities;
        no_matching_category += daily.no_matching_category;
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
            no_matching_category,
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

    use ads_simulator::{BrowseReason, generate_marketplace};

    use super::{Activity, CAMPAIGNS_PER_ADVERTISER, Config, DAYS, run};

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
        marketplace
            .browsing_events
            .retain(|event| event.reason == BrowseReason::Exploration);
        let report = run(
            &marketplace,
            Config {
                seed: 42,
                campaign_budget_cents: 10_000,
                cpc_cents: 100,
                impressions: Some(3_000),
            },
        )
        .unwrap();

        assert_eq!(report.summary.activity.impressions, 3_000);
        assert_eq!(report.summary.activity.clicks, 0);
        assert_eq!(report.summary.activity.spend_cents, 0);
    }

    #[test]
    fn exact_target_is_reproducible_and_ledgers_balance() {
        let marketplace = generate_marketplace(42);
        let config = Config {
            seed: 42,
            campaign_budget_cents: 10_000,
            cpc_cents: 100,
            impressions: Some(1_000_000),
        };
        let report = run(&marketplace, config).unwrap();
        assert_eq!(report, run(&marketplace, config).unwrap());
        assert_eq!(report.campaigns.len(), 500);
        assert_eq!(report.days.len(), DAYS);
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
        let mut config = Config {
            seed: 42,
            campaign_budget_cents: 72,
            cpc_cents: 73,
            impressions: None,
        };
        let report = run(&marketplace, config).unwrap();
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
}
