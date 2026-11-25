use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashMap, HashSet},
    io::{Error, ErrorKind},
};

use ads_simulator::{Item, Marketplace, User, score_preference};
use rand::{
    Rng, SeedableRng,
    seq::{IndexedRandom, SliceRandom},
};
use rand_chacha::ChaCha8Rng;
use serde::Serialize;

use crate::{
    pacing::{PacingStrategy, daily_budget_cents, pacing_1_unlock_second},
    timestamp::{HOURS_PER_DAY, PstTimestamp, TIMEZONE_LABEL, sample_pst_second_of_day},
};

pub const DAYS: usize = 30;
const CAMPAIGNS_PER_ADVERTISER: usize = 10;
const MAX_CLICK_PROBABILITY: f64 = 0.04;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Config {
    pub seed: u64,
    pub campaign_budget_cents: u64,
    pub cpc_cents: u64,
    pub impressions: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opportunities: Option<u64>,
    pub pacing: PacingStrategy,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Activity {
    pub impressions: u64,
    pub clicks: u64,
    pub spend_cents: u64,
}

impl Activity {
    fn impression(&mut self, cost: Option<u64>) {
        self.impressions += 1;
        if let Some(cost) = cost {
            self.clicks += 1;
            self.spend_cents += cost;
        }
    }

    fn add_assign(&mut self, other: Self) {
        self.impressions += other.impressions;
        self.clicks += other.clicks;
        self.spend_cents += other.spend_cents;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct HourlyActivity {
    pub hour: usize,
    pub opportunities: u64,
    pub no_matching_category: u64,
    pub insufficient_budget: u64,
    pub pacing_throttled: u64,
    pub daily_budget_exhausted: u64,
    #[serde(flatten)]
    pub activity: Activity,
}

impl HourlyActivity {
    fn empty_day() -> Vec<Self> {
        (0..HOURS_PER_DAY)
            .map(|hour| Self {
                hour,
                ..Self::default()
            })
            .collect()
    }

    fn add_assign(&mut self, other: &Self) {
        self.opportunities += other.opportunities;
        self.no_matching_category += other.no_matching_category;
        self.insufficient_budget += other.insufficient_budget;
        self.pacing_throttled += other.pacing_throttled;
        self.daily_budget_exhausted += other.daily_budget_exhausted;
        self.activity.add_assign(other.activity);
    }
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Impression<'a> {
    pub timestamp: PstTimestamp,
    pub day: usize,
    pub user_id: &'a str,
    pub campaign_id: &'a str,
    pub ad_item_id: &'a str,
    pub category_id: &'a str,
    pub clicked: bool,
    pub spend_cents: u64,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct DailyActivity {
    day: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    daily_budget_cents: Option<u64>,
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
    pacing_throttled: u64,
    daily_budget_exhausted: u64,
    #[serde(flatten)]
    activity: Activity,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct Day {
    day: usize,
    date_pst: String,
    opportunities: u64,
    no_matching_category: u64,
    insufficient_budget: u64,
    pacing_throttled: u64,
    daily_budget_exhausted: u64,
    #[serde(flatten)]
    activity: Activity,
    hourly: Vec<HourlyActivity>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Report<'a> {
    schema_version: u32,
    currency: &'a str,
    timezone: &'static str,
    config: Config,
    summary: Summary,
    hourly: Vec<HourlyActivity>,
    days: Vec<Day>,
    campaigns: Vec<Campaign<'a>>,
}

#[derive(Clone, Copy, Debug)]
struct Opportunity {
    user_index: usize,
    timestamp: PstTimestamp,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message.into())
}

fn browsing_opportunities(marketplace: &Marketplace) -> Result<Vec<&User>, Error> {
    let users: HashMap<_, _> = marketplace
        .users
        .iter()
        .map(|user| (&user.id, user))
        .collect();
    let items: HashSet<_> = marketplace
        .organic_items
        .iter()
        .map(|item| &item.id)
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
            if !items.contains(&browse.organic_item_id) {
                return Err(invalid(format!(
                    "unknown browsing item {}",
                    browse.organic_item_id
                )));
            }
            Ok(*user)
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
                        daily_budget_cents: None,
                        activity: Activity::default(),
                    })
                    .collect(),
            });
        }
    }
    Ok(campaigns)
}

fn split_daily_target(target: u64, day: usize) -> u64 {
    let days = DAYS as u64;
    target / days + u64::from(day as u64 <= target % days)
}

fn generate_day_schedule(
    day: usize,
    browsing_count: usize,
    target_count: Option<u64>,
    order: &mut [usize],
    browsing_rng: &mut ChaCha8Rng,
    time_rng: &mut ChaCha8Rng,
) -> Vec<Opportunity> {
    let count = match target_count {
        Some(target) => target as usize,
        None => browsing_count,
    };
    if count == 0 || browsing_count == 0 {
        return Vec::new();
    }

    let mut schedule = Vec::with_capacity(count);
    if target_count.is_none() {
        for user_index in 0..browsing_count {
            let second_of_day = sample_pst_second_of_day(time_rng);
            schedule.push(Opportunity {
                user_index,
                timestamp: PstTimestamp::new(day, second_of_day),
            });
        }
    } else {
        while schedule.len() < count {
            order.shuffle(browsing_rng);
            let remaining = count - schedule.len();
            for &user_index in order.iter().take(remaining) {
                let second_of_day = sample_pst_second_of_day(time_rng);
                schedule.push(Opportunity {
                    user_index,
                    timestamp: PstTimestamp::new(day, second_of_day),
                });
            }
        }
    }
    schedule.sort_by_key(|opportunity| opportunity.timestamp);
    schedule
}

pub fn run(marketplace: &Marketplace, config: Config) -> Result<Report<'_>, Error> {
    run_with_impressions(marketplace, config, |_| Ok(()))
}

pub fn run_with_impressions<'a, F>(
    marketplace: &'a Marketplace,
    config: Config,
    mut on_impression: F,
) -> Result<Report<'a>, Error>
where
    F: FnMut(&Impression<'_>) -> Result<(), Error>,
{
    if marketplace.schema_version != 1 || marketplace.currency != "USD" {
        return Err(invalid("expected schema version 1 marketplace data in USD"));
    }
    if config.cpc_cents == 0 {
        return Err(invalid("CPC must be positive"));
    }
    if config.impressions.is_some() && config.opportunities.is_some() {
        return Err(invalid(
            "impressions and opportunities targets cannot both be set",
        ));
    }

    let mut ads_rng = ChaCha8Rng::seed_from_u64(config.seed);
    let mut browsing_rng = ads_rng.clone();
    browsing_rng.set_stream(1);
    let mut click_rng = ads_rng.clone();
    click_rng.set_stream(2);
    let mut time_rng = ads_rng.clone();
    time_rng.set_stream(3);

    let browsing_users = browsing_opportunities(marketplace)?;
    let mut campaigns = initialize_campaigns(marketplace, config, &mut ads_rng)?;
    let budget_cents = config
        .campaign_budget_cents
        .checked_mul(campaigns.len() as u64)
        .ok_or_else(|| invalid("total campaign budget does not fit in the report"))?;

    let mut days = Vec::with_capacity(DAYS);
    let mut hourly_totals = HourlyActivity::empty_day();
    let mut totals = Activity::default();
    let mut opportunity_count = 0;
    let mut insufficient_budget = 0;
    let mut pacing_throttled = 0;
    let mut daily_budget_exhausted = 0;
    let mut order: Vec<_> = (0..browsing_users.len()).collect();

    for day in 1..=DAYS {
        let impression_quota = config
            .impressions
            .map(|target| split_daily_target(target, day));
        let opportunity_target = config
            .opportunities
            .or(config.impressions)
            .map(|target| split_daily_target(target, day));

        let schedule = generate_day_schedule(
            day,
            browsing_users.len(),
            opportunity_target,
            &mut order,
            &mut browsing_rng,
            &mut time_rng,
        );

        let mut affordable_campaigns = 0usize;
        let mut eligible = Vec::with_capacity(campaigns.len());
        let mut throttled: BinaryHeap<Reverse<(u32, usize)>> = BinaryHeap::new();

        for (index, campaign) in campaigns.iter_mut().enumerate() {
            let can_afford_total = campaign.remaining_budget_cents >= config.cpc_cents;
            if can_afford_total {
                affordable_campaigns += 1;
            }
            match config.pacing {
                PacingStrategy::None => {
                    campaign.daily[day - 1].daily_budget_cents = None;
                    if can_afford_total {
                        eligible.push(index);
                    }
                }
                PacingStrategy::Pacing1 => {
                    let daily_budget = daily_budget_cents(
                        campaign.budget_cents,
                        campaign.remaining_budget_cents,
                        day,
                        DAYS,
                    );
                    campaign.daily[day - 1].daily_budget_cents = Some(daily_budget);
                    if pacing_1_unlock_second(
                        0,
                        daily_budget,
                        campaign.remaining_budget_cents,
                        config.cpc_cents,
                    ) == Some(0)
                    {
                        eligible.push(index);
                    }
                }
            }
        }

        let mut daily = Day {
            day,
            date_pst: PstTimestamp::date_string(day),
            opportunities: 0,
            no_matching_category: 0,
            insufficient_budget: 0,
            pacing_throttled: 0,
            daily_budget_exhausted: 0,
            activity: Activity::default(),
            hourly: HourlyActivity::empty_day(),
        };

        for opportunity in schedule {
            let second_of_day = opportunity.timestamp.second_of_day;
            let hour = opportunity.timestamp.hour();

            while let Some(&Reverse((unlock_second, campaign_idx))) = throttled.peek() {
                if unlock_second <= second_of_day {
                    throttled.pop();
                    eligible.push(campaign_idx);
                } else {
                    break;
                }
            }

            daily.opportunities += 1;
            daily.hourly[hour].opportunities += 1;

            if eligible.is_empty() {
                if !throttled.is_empty() {
                    daily.pacing_throttled += 1;
                    daily.hourly[hour].pacing_throttled += 1;
                } else if affordable_campaigns > 0 {
                    daily.daily_budget_exhausted += 1;
                    daily.hourly[hour].daily_budget_exhausted += 1;
                } else {
                    daily.insufficient_budget += 1;
                    daily.hourly[hour].insufficient_budget += 1;
                }
                continue;
            }

            let eligible_index = ads_rng.random_range(0..eligible.len());
            let campaign_index = eligible[eligible_index];
            let campaign = &mut campaigns[campaign_index];
            let user = browsing_users[opportunity.user_index];
            let score = score_preference(user, campaign.item);
            let cost = click_rng
                .random_bool(MAX_CLICK_PROBABILITY * score)
                .then_some(config.cpc_cents);

            campaign.totals.impression(cost);
            campaign.daily[day - 1].activity.impression(cost);
            daily.activity.impression(cost);
            daily.hourly[hour].activity.impression(cost);
            totals.impression(cost);

            if let Some(cost) = cost {
                campaign.remaining_budget_cents -= cost;
                if campaign.remaining_budget_cents < config.cpc_cents {
                    affordable_campaigns -= 1;
                    eligible.swap_remove(eligible_index);
                } else if config.pacing == PacingStrategy::Pacing1 {
                    let daily_budget = campaign.daily[day - 1].daily_budget_cents.unwrap_or(0);
                    let daily_spend = campaign.daily[day - 1].activity.spend_cents;
                    match pacing_1_unlock_second(
                        daily_spend,
                        daily_budget,
                        campaign.remaining_budget_cents,
                        config.cpc_cents,
                    ) {
                        Some(unlock_second) if unlock_second <= second_of_day => {}
                        Some(unlock_second) => {
                            eligible.swap_remove(eligible_index);
                            throttled.push(Reverse((unlock_second, campaign_index)));
                        }
                        None => {
                            eligible.swap_remove(eligible_index);
                        }
                    }
                }
            }

            let impression = Impression {
                timestamp: opportunity.timestamp,
                day,
                user_id: &user.id,
                campaign_id: &campaign.id,
                ad_item_id: campaign.ad_item_id,
                category_id: campaign.category_id,
                clicked: cost.is_some(),
                spend_cents: cost.unwrap_or(0),
            };
            on_impression(&impression)?;
        }

        if let Some(quota) = impression_quota
            && daily.activity.impressions < quota
        {
            return Err(invalid(format!(
                concat!(
                    "impression target cannot be reached on day {}: ",
                    "delivered {} of {} impressions (insufficient_budget={}, pacing_throttled={}, daily_budget_exhausted={})"
                ),
                day,
                daily.activity.impressions,
                quota,
                daily.insufficient_budget,
                daily.pacing_throttled,
                daily.daily_budget_exhausted,
            )));
        }

        opportunity_count += daily.opportunities;
        insufficient_budget += daily.insufficient_budget;
        pacing_throttled += daily.pacing_throttled;
        daily_budget_exhausted += daily.daily_budget_exhausted;
        for (total_hour, day_hour) in hourly_totals.iter_mut().zip(&daily.hourly) {
            total_hour.add_assign(day_hour);
        }
        days.push(daily);
    }

    Ok(Report {
        schema_version: 1,
        currency: &marketplace.currency,
        timezone: TIMEZONE_LABEL,
        config,
        summary: Summary {
            budget_cents,
            remaining_budget_cents: budget_cents - totals.spend_cents,
            opportunities: opportunity_count,
            no_matching_category: 0,
            insufficient_budget,
            pacing_throttled,
            daily_budget_exhausted,
            activity: totals,
        },
        hourly: hourly_totals,
        days,
        campaigns,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use ads_simulator::generate_marketplace;

    use super::{Activity, CAMPAIGNS_PER_ADVERTISER, Config, DAYS, run, run_with_impressions};
    use crate::{
        pacing::PacingStrategy,
        timestamp::{HOURS_PER_DAY, PstTimestamp, SECONDS_PER_DAY},
    };

    fn base_config(pacing: PacingStrategy) -> Config {
        Config {
            seed: 42,
            campaign_budget_cents: 10_000,
            cpc_cents: 100,
            impressions: Some(1_000_000),
            opportunities: None,
            pacing,
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
        let config = Config {
            seed: 42,
            campaign_budget_cents: 10_000,
            cpc_cents: 100,
            impressions: Some(3_000),
            opportunities: None,
            pacing: PacingStrategy::None,
        };
        let initial = run(&marketplace, config).unwrap();
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
        let config = Config {
            seed: 42,
            campaign_budget_cents: 10_000,
            cpc_cents: 100,
            impressions: Some(300),
            opportunities: None,
            pacing: PacingStrategy::Pacing1,
        };
        let matching = run(&marketplace, config).unwrap();
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
        for pacing in [PacingStrategy::None, PacingStrategy::Pacing1] {
            let config = base_config(pacing);
            let report = run(&marketplace, config).unwrap();
            assert_eq!(report, run(&marketplace, config).unwrap());
            assert_eq!(report.timezone, "PST");
            assert_eq!(report.campaigns.len(), 500);
            assert_eq!(report.days.len(), DAYS);
            assert_eq!(report.hourly.len(), HOURS_PER_DAY);
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

            // Peak hour across the 30-day run is 08:00 PST, dying out around 18:00 PST.
            let peak_hour = report
                .hourly
                .iter()
                .max_by_key(|hour| hour.activity.impressions)
                .unwrap()
                .hour;
            assert_eq!(peak_hour, 8);
            let evening_impressions: u64 = report.hourly[18..]
                .iter()
                .map(|hour| hour.activity.impressions)
                .sum();
            assert!(evening_impressions * 100 < summary.activity.impressions * 2);

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
                if pacing == PacingStrategy::Pacing1 {
                    let daily_budget_sum: u64 = campaign
                        .daily
                        .iter()
                        .map(|day| day.daily_budget_cents.unwrap())
                        .sum();
                    assert_eq!(daily_budget_sum, campaign.budget_cents);
                    for day in &campaign.daily {
                        assert!(day.activity.spend_cents <= day.daily_budget_cents.unwrap());
                    }
                }
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
                    day.activity.impressions
                        + day.insufficient_budget
                        + day.no_matching_category
                        + day.pacing_throttled
                        + day.daily_budget_exhausted,
                    day.opportunities
                );
                assert_eq!(day.hourly.len(), HOURS_PER_DAY);
                let mut from_hours = Activity::default();
                for hour in &day.hourly {
                    assert_eq!(
                        hour.activity.impressions
                            + hour.insufficient_budget
                            + hour.no_matching_category
                            + hour.pacing_throttled
                            + hour.daily_budget_exhausted,
                        hour.opportunities
                    );
                    from_hours.add_assign(hour.activity);
                }
                assert_eq!(from_hours, day.activity);

                let mut from_campaigns = Activity::default();
                for campaign in &report.campaigns {
                    let campaign_day = &campaign.daily[i];
                    assert_eq!(campaign_day.day, day.day);
                    from_campaigns.add_assign(campaign_day.activity);
                }
                assert_eq!(from_campaigns, day.activity);
            }
        }
    }

    #[test]
    fn impressions_have_monotonic_pst_timestamps_peaking_at_8am_and_dying_out_around_6pm() {
        let marketplace = generate_marketplace(42);
        let config = Config {
            seed: 42,
            campaign_budget_cents: 10_000,
            cpc_cents: 100,
            impressions: Some(60_000),
            opportunities: None,
            pacing: PacingStrategy::Pacing1,
        };
        let mut previous: Option<PstTimestamp> = None;
        let mut by_hour = [0u64; HOURS_PER_DAY];
        let mut count = 0u64;

        let report = run_with_impressions(&marketplace, config, |impression| {
            let formatted = impression.timestamp.to_string();
            assert!(
                formatted.ends_with("-08:00"),
                "expected PST offset: {formatted}"
            );
            if let Some(prev) = previous {
                assert!(
                    impression.timestamp >= prev,
                    "timestamps must be monotonic: {} < {}",
                    impression.timestamp,
                    prev
                );
            }
            previous = Some(impression.timestamp);
            by_hour[impression.timestamp.hour()] += 1;
            count += 1;
            Ok(())
        })
        .unwrap();

        assert_eq!(count, 60_000);
        assert_eq!(report.summary.activity.impressions, 60_000);
        let peak_hour = by_hour
            .iter()
            .enumerate()
            .max_by_key(|(_, c)| *c)
            .unwrap()
            .0;
        assert_eq!(peak_hour, 8);
        let evening: u64 = by_hour[18..].iter().sum();
        assert!(evening * 100 < count * 2);
    }

    #[test]
    fn pacing_1_enforces_daily_budget_and_even_intraday_spend_curve() {
        // Construct a single-advertiser marketplace where every user prefers the
        // ad category so campaigns generate heavy click demand across the day.
        let mut marketplace = generate_marketplace(42);
        marketplace.advertisers.truncate(1);
        let target_category = marketplace.advertisers[0].catalog[0].category_id.clone();
        for item in &mut marketplace.advertisers[0].catalog {
            item.category_id.clone_from(&target_category);
        }
        for user in &mut marketplace.users {
            user.preferred_category_ids = vec![target_category.clone()];
        }

        // $300 total budget over 30 days -> $10/day (10 clicks/day at $1 CPC).
        // With 12,000 daily opportunities across 10 campaigns (1,200 opps/campaign/day
        // at 4% CTR = ~48 expected clicks/campaign/day), unpaced burns the entire
        // 30-day budget in the first week and blows daily spend during the morning peak,
        // whereas Pacing-1 spreads spend across all 30 days and evenly through each day.
        let unpaced = run(
            &marketplace,
            Config {
                seed: 42,
                campaign_budget_cents: 30_000,
                cpc_cents: 100,
                impressions: None,
                opportunities: None,
                pacing: PacingStrategy::None,
            },
        )
        .unwrap();
        assert_eq!(unpaced.days[29].activity.impressions, 0);
        assert!(unpaced.summary.insufficient_budget > 0);
        assert_eq!(unpaced.summary.pacing_throttled, 0);

        let mut click_events: Vec<(usize, String, u32, u64)> = Vec::new();
        let paced = run_with_impressions(
            &marketplace,
            Config {
                seed: 42,
                campaign_budget_cents: 30_000,
                cpc_cents: 100,
                impressions: None,
                opportunities: None,
                pacing: PacingStrategy::Pacing1,
            },
            |impression| {
                if impression.clicked {
                    click_events.push((
                        impression.day,
                        impression.campaign_id.to_owned(),
                        impression.timestamp.second_of_day,
                        impression.spend_cents,
                    ));
                }
                Ok(())
            },
        )
        .unwrap();

        // Pacing-1 delivers impressions on every single day (all 30 days).
        assert!(paced.days.iter().all(|day| day.activity.impressions > 0));
        assert!(paced.summary.pacing_throttled > 0);

        // Verify per-campaign daily budget cap and exact linear intraday pacing invariant:
        // before every click at second `t`, cumulative daily spend <= daily_budget * t / 86_400.
        let mut spend_before_click: HashMap<(usize, String), u64> = HashMap::new();
        for (day, campaign_id, second_of_day, spend_cents) in click_events {
            let entry = spend_before_click.entry((day, campaign_id)).or_insert(0);
            let daily_budget = 1_000u64; // 30_000 / 30
            assert!(
                u128::from(*entry) * u128::from(SECONDS_PER_DAY)
                    <= u128::from(daily_budget) * u128::from(second_of_day),
                "click fired ahead of linear pace on day {day} at t={second_of_day}: prior_spend={entry}"
            );
            *entry += spend_cents;
            assert!(*entry <= daily_budget);
        }

        // Because traffic dies out around 6pm PST while Pacing-1 reserves budget
        // evenly across 24 hours, Pacing-1 delivers spend into the afternoon (12:00–18:00 PST)
        // whereas on Day 1 unpaced burns far more heavily in the morning peak.
        let paced_afternoon_spend: u64 = paced.hourly[12..18]
            .iter()
            .map(|h| h.activity.spend_cents)
            .sum();
        assert!(paced_afternoon_spend > 0);
        // Under Pacing-1 with 10 clicks/day ($10/day), at most 8 clicks can unlock
        // before 18:00 PST (since click 9 unlocks at 19:12 PST), so daily spend per
        // campaign reflects even 24h pacing rather than exhausting at 8am.
        let paced_morning_share = paced.hourly[..12]
            .iter()
            .map(|h| h.activity.spend_cents)
            .sum::<u64>() as f64
            / paced.summary.activity.spend_cents as f64;
        let unpaced_morning_share = unpaced.hourly[..12]
            .iter()
            .map(|h| h.activity.spend_cents)
            .sum::<u64>() as f64
            / unpaced.summary.activity.spend_cents as f64;
        assert!(
            paced_morning_share < unpaced_morning_share,
            "expected Pacing-1 morning spend share ({paced_morning_share:.3}) < unpaced ({unpaced_morning_share:.3})"
        );
    }

    #[test]
    fn unaffordable_campaigns_leave_budgets_intact_and_cannot_meet_a_target() {
        let marketplace = generate_marketplace(42);
        for pacing in [PacingStrategy::None, PacingStrategy::Pacing1] {
            let mut config = Config {
                seed: 42,
                campaign_budget_cents: 72,
                cpc_cents: 73,
                impressions: None,
                opportunities: None,
                pacing,
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
}
