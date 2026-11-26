use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashMap, HashSet},
    fmt,
    io::{Error, ErrorKind},
    path::PathBuf,
};

use ads_ml::{
    AdCandidate, ImpressionClickRef, ImpressionLogStore, PreparedScorer, PyTorchTrainer,
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

use crate::{
    bidding::{AuctionWinner, BiddingStrategy, compute_bid, run_first_price_auction},
    pacing::{PacingStrategy, daily_budget_cents, pacing_1_unlock_second},
    ranking::{MAX_CLICK_PROBABILITY, RankingModel},
    retrieval::{RetrievalRequest, RetrievalStrategy, build_retriever},
    timestamp::{HOURS_PER_DAY, PstTimestamp, TIMEZONE_LABEL, sample_pst_second_of_day},
};

pub const DAYS: usize = 30;
const CAMPAIGNS_PER_ADVERTISER: usize = 10;
const TIE_EPSILON: f64 = 1e-12;
const NEUTRAL_PCTR: f64 = 1.0;

/// Ranking strategy (`Rank-*`): turns candidate scores into the selected ad.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, ValueEnum)]
pub enum RankingStrategy {
    #[default]
    #[serde(rename = "Rank-Uniform")]
    #[value(name = "Rank-Uniform", alias = "rank-uniform", alias = "uniform")]
    RankUniform,
    #[serde(rename = "Rank-Greedy")]
    #[value(name = "Rank-Greedy", alias = "rank-greedy", alias = "greedy")]
    RankGreedy,
}

impl fmt::Display for RankingStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RankUniform => f.write_str("Rank-Uniform"),
            Self::RankGreedy => f.write_str("Rank-Greedy"),
        }
    }
}

/// Scoring strategy (`Score-*`): assigns a numeric score to each candidate ad.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, ValueEnum)]
pub enum ScoringStrategy {
    #[default]
    #[serde(rename = "Score-Uniform")]
    #[value(name = "Score-Uniform", alias = "score-uniform", alias = "uniform")]
    ScoreUniform,
    #[serde(rename = "Score-R1")]
    #[value(name = "Score-R1", alias = "score-r1", alias = "r1")]
    ScoreR1,
}

impl fmt::Display for ScoringStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ScoreUniform => f.write_str("Score-Uniform"),
            Self::ScoreR1 => f.write_str("Score-R1"),
        }
    }
}

/// Serving strategy: a combination of a `Retrieve-*`, `Rank-*`, and `Score-*` strategy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ServingStrategy {
    pub retrieval: RetrievalStrategy,
    pub ranking: RankingStrategy,
    pub scoring: ScoringStrategy,
}

impl ServingStrategy {
    pub const fn new(
        retrieval: RetrievalStrategy,
        ranking: RankingStrategy,
        scoring: ScoringStrategy,
    ) -> Self {
        Self {
            retrieval,
            ranking,
            scoring,
        }
    }

    #[cfg(test)]
    pub const fn uniform() -> Self {
        Self::new(
            RetrievalStrategy::RetrieveV0,
            RankingStrategy::RankUniform,
            ScoringStrategy::ScoreUniform,
        )
    }

    #[cfg(test)]
    pub const fn r1() -> Self {
        Self::new(
            RetrievalStrategy::RetrieveV0,
            RankingStrategy::RankGreedy,
            ScoringStrategy::ScoreR1,
        )
    }
}

impl fmt::Display for ServingStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}+{}+{}", self.retrieval, self.ranking, self.scoring)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Config {
    pub seed: u64,
    pub campaign_budget_cents: u64,
    pub cpc_cents: u64,
    pub impressions: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opportunities: Option<u64>,
    pub serving: ServingStrategy,
    pub pacing: PacingStrategy,
    pub bidding: BiddingStrategy,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub impression_log_dir: Option<PathBuf>,
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

#[derive(Debug, PartialEq, Serialize)]
pub struct Impression<'a> {
    pub timestamp: PstTimestamp,
    pub day: usize,
    pub user_id: &'a str,
    pub campaign_id: &'a str,
    pub ad_item_id: &'a str,
    pub category_id: &'a str,
    pub base_bid_cents: u64,
    pub pctr: f64,
    pub pacing_factor: f64,
    pub final_bid_cents: f64,
    pub click_price_cents: u64,
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

#[derive(Debug, PartialEq, Serialize)]
struct DailyTrainingSummary {
    trained_after_day: usize,
    trainer: String,
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
    timezone: &'static str,
    config: Config,
    summary: Summary,
    hourly: Vec<HourlyActivity>,
    days: Vec<Day>,
    campaigns: Vec<Campaign<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    training_runs: Vec<DailyTrainingSummary>,
}

struct BrowsingOpportunity<'a> {
    user: &'a User,
    user_index: usize,
    organic_item: &'a Item,
    organic_category_index: usize,
}

#[derive(Clone, Copy, Debug)]
struct ScheduledOpportunity {
    browsing_index: usize,
    timestamp: PstTimestamp,
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
) -> Result<(Vec<&'a str>, Vec<BrowsingOpportunity<'a>>), Error> {
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
            Ok(BrowsingOpportunity {
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
) -> Vec<ScheduledOpportunity> {
    let count = match target_count {
        Some(target) => target as usize,
        None => browsing_count,
    };
    if count == 0 || browsing_count == 0 {
        return Vec::new();
    }

    let mut schedule = Vec::with_capacity(count);
    if target_count.is_none() {
        for browsing_index in 0..browsing_count {
            let second_of_day = sample_pst_second_of_day(time_rng);
            schedule.push(ScheduledOpportunity {
                browsing_index,
                timestamp: PstTimestamp::new(day, second_of_day),
            });
        }
    } else {
        while schedule.len() < count {
            order.shuffle(browsing_rng);
            let remaining = count - schedule.len();
            for &browsing_index in order.iter().take(remaining) {
                let second_of_day = sample_pst_second_of_day(time_rng);
                schedule.push(ScheduledOpportunity {
                    browsing_index,
                    timestamp: PstTimestamp::new(day, second_of_day),
                });
            }
        }
    }
    schedule.sort_by_key(|opportunity| opportunity.timestamp);
    schedule
}

fn add_paced_eligible(
    paced_eligible: &mut Vec<usize>,
    paced_pos: &mut [Option<usize>],
    campaign_index: usize,
) {
    debug_assert!(paced_pos[campaign_index].is_none());
    paced_pos[campaign_index] = Some(paced_eligible.len());
    paced_eligible.push(campaign_index);
}

fn remove_paced_eligible(
    paced_eligible: &mut Vec<usize>,
    paced_pos: &mut [Option<usize>],
    campaign_index: usize,
) {
    if let Some(pos) = paced_pos[campaign_index].take() {
        paced_eligible.swap_remove(pos);
        if let Some(&swapped) = paced_eligible.get(pos) {
            paced_pos[swapped] = Some(pos);
        }
    }
}

fn select_greedy_campaign(
    scorer: &PreparedScorer,
    user_index: usize,
    organic_category_index: usize,
    candidates: &[usize],
    rng: &mut ChaCha8Rng,
) -> usize {
    let mut best_campaign = candidates[0];
    let mut best_score = f64::NEG_INFINITY;
    let mut tie_count = 0u64;

    for &campaign_index in candidates {
        let score = scorer.score_logit(user_index, organic_category_index, campaign_index);
        if score > best_score + TIE_EPSILON {
            best_score = score;
            best_campaign = campaign_index;
            tie_count = 1;
        } else if (score - best_score).abs() <= TIE_EPSILON {
            tie_count += 1;
            if rng.random_range(0..tie_count) == 0 {
                best_campaign = campaign_index;
            }
        }
    }
    best_campaign
}

pub fn run(marketplace: &Marketplace, config: Config) -> Result<Report<'_>, Error> {
    run_with_impressions(marketplace, config, |_| Ok(()))
}

pub fn run_with_impressions<'a, F>(
    marketplace: &'a Marketplace,
    config: Config,
    on_impression: F,
) -> Result<Report<'a>, Error>
where
    F: FnMut(&Impression<'_>) -> Result<(), Error>,
{
    run_internal::<dyn RankingModel, _>(marketplace, config, None, on_impression)
}

#[cfg(test)]
pub fn run_with_ranking_and_impressions<'a, R, F>(
    marketplace: &'a Marketplace,
    config: Config,
    ranking_model: &R,
    on_impression: F,
) -> Result<Report<'a>, Error>
where
    R: RankingModel + ?Sized,
    F: FnMut(&Impression<'_>) -> Result<(), Error>,
{
    run_internal(marketplace, config, Some(ranking_model), on_impression)
}

fn run_internal<'a, R, F>(
    marketplace: &'a Marketplace,
    config: Config,
    ranking_override: Option<&R>,
    mut on_impression: F,
) -> Result<Report<'a>, Error>
where
    R: RankingModel + ?Sized,
    F: FnMut(&Impression<'_>) -> Result<(), Error>,
{
    if marketplace.schema_version != 1 || marketplace.currency != "USD" {
        return Err(invalid("expected schema version 1 marketplace data in USD"));
    }
    if config.cpc_cents == 0 {
        return Err(invalid("CPC / basebid must be positive"));
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

    let (category_ids, category_index) = build_category_table(marketplace);
    let (user_ids, browsing) = browsing_opportunities(marketplace, &category_index)?;
    let mut campaigns = initialize_campaigns(marketplace, &config, &mut ads_rng)?;
    let budget_cents = config
        .campaign_budget_cents
        .checked_mul(campaigns.len() as u64)
        .ok_or_else(|| invalid("total campaign budget does not fit in the report"))?;
    let initial_budgets: Vec<u64> = campaigns
        .iter()
        .map(|campaign| campaign.remaining_budget_cents)
        .collect();
    let mut retriever =
        build_retriever(config.serving.retrieval, &initial_budgets, config.cpc_cents);

    // Optional precomputed positive-pCTR candidates when an explicit RankingModel override is passed.
    let override_pctr_by_user: Vec<Vec<(usize, f64)>> = if let Some(model) = ranking_override {
        marketplace
            .users
            .iter()
            .map(|user| {
                campaigns
                    .iter()
                    .enumerate()
                    .filter_map(|(campaign_index, campaign)| {
                        let pctr = model.predict_pctr(user, campaign.item);
                        (pctr.is_finite() && pctr > 0.0).then_some((campaign_index, pctr))
                    })
                    .collect()
            })
            .collect()
    } else {
        Vec::new()
    };

    let needs_training_logs = config.serving.scoring == ScoringStrategy::ScoreR1;
    let (_temp_log_dir, log_store) = if let Some(ref dir) = config.impression_log_dir {
        let store = ImpressionLogStore::new(dir.clone());
        store.clear()?;
        (None, Some(store))
    } else if needs_training_logs {
        let temp_dir = tempfile::tempdir()?;
        let store = ImpressionLogStore::new(temp_dir.path());
        store.clear()?;
        (Some(temp_dir), Some(store))
    } else {
        (None, None)
    };

    let mut days = Vec::with_capacity(DAYS);
    let mut hourly_totals = HourlyActivity::empty_day();
    let mut training_runs = Vec::new();
    let mut pytorch_trainer = if needs_training_logs {
        Some(PyTorchTrainer::spawn()?)
    } else {
        None
    };
    let mut scorer: Option<PreparedScorer> = None;
    let mut totals = Activity::default();
    let mut opportunity_count = 0;
    let mut no_matching_category = 0;
    let mut insufficient_budget = 0;
    let mut pacing_throttled = 0;
    let mut daily_budget_exhausted = 0;
    let mut order: Vec<_> = (0..browsing.len()).collect();
    let mut paced_pos: Vec<Option<usize>> = vec![None; campaigns.len()];
    let mut is_throttled: Vec<bool> = vec![false; campaigns.len()];

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
            browsing.len(),
            opportunity_target,
            &mut order,
            &mut browsing_rng,
            &mut time_rng,
        );

        let mut paced_eligible = Vec::with_capacity(campaigns.len());
        let mut throttled: BinaryHeap<Reverse<(u32, usize)>> = BinaryHeap::new();
        paced_pos.fill(None);
        is_throttled.fill(false);

        for (index, campaign) in campaigns.iter_mut().enumerate() {
            let can_afford_total = campaign.remaining_budget_cents >= config.cpc_cents;
            match config.pacing {
                PacingStrategy::None => {
                    campaign.daily[day - 1].daily_budget_cents = None;
                    if can_afford_total {
                        add_paced_eligible(&mut paced_eligible, &mut paced_pos, index);
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
                        add_paced_eligible(&mut paced_eligible, &mut paced_pos, index);
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
        let mut day_writer = log_store
            .as_ref()
            .map(|store| store.create_day_writer(day))
            .transpose()?;

        for scheduled in schedule {
            let second_of_day = scheduled.timestamp.second_of_day;
            let hour = scheduled.timestamp.hour();

            while let Some(&Reverse((unlock_second, campaign_idx))) = throttled.peek() {
                if unlock_second <= second_of_day {
                    throttled.pop();
                    is_throttled[campaign_idx] = false;
                    add_paced_eligible(&mut paced_eligible, &mut paced_pos, campaign_idx);
                } else {
                    break;
                }
            }

            daily.opportunities += 1;
            daily.hourly[hour].opportunities += 1;

            let opportunity = &browsing[scheduled.browsing_index];
            let retrieval_request = RetrievalRequest {
                day,
                user_id: &opportunity.user.id,
                user_index: opportunity.user_index,
                organic_category_id: &opportunity.organic_item.category_id,
                organic_category_index: opportunity.organic_category_index,
            };
            let retrieved = retriever.retrieve(&retrieval_request);
            let eligible_slice: &[usize] = match config.pacing {
                PacingStrategy::None => retrieved,
                PacingStrategy::Pacing1 => &paced_eligible,
            };

            let winner: Option<AuctionWinner> = match config.bidding {
                BiddingStrategy::Basebid => {
                    if eligible_slice.is_empty() {
                        None
                    } else {
                        let campaign_index = match (config.serving.ranking, scorer.as_ref()) {
                            (RankingStrategy::RankGreedy, Some(prepared)) => {
                                select_greedy_campaign(
                                    prepared,
                                    opportunity.user_index,
                                    opportunity.organic_category_index,
                                    eligible_slice,
                                    &mut ads_rng,
                                )
                            }
                            (RankingStrategy::RankUniform, _)
                            | (RankingStrategy::RankGreedy, None) => {
                                eligible_slice[ads_rng.random_range(0..eligible_slice.len())]
                            }
                        };
                        let campaign = &campaigns[campaign_index];
                        let daily_budget = campaign.daily[day - 1].daily_budget_cents.unwrap_or(0);
                        let daily_spend = campaign.daily[day - 1].activity.spend_cents;
                        let pacing_factor = config.pacing.pacing_factor(
                            daily_spend,
                            daily_budget,
                            campaign.remaining_budget_cents,
                            config.cpc_cents,
                            second_of_day,
                        );
                        let pctr = if let Some(model) = ranking_override {
                            model.predict_pctr(opportunity.user, campaign.item)
                        } else if let Some(prepared) = scorer.as_ref() {
                            prepared.score_ctr(
                                opportunity.user_index,
                                opportunity.organic_category_index,
                                campaign_index,
                            )
                        } else {
                            NEUTRAL_PCTR
                        };
                        let bid = compute_bid(
                            BiddingStrategy::Basebid,
                            config.cpc_cents,
                            pctr,
                            pacing_factor,
                        );
                        Some(AuctionWinner {
                            candidate_index: campaign_index,
                            bid,
                        })
                    }
                }
                BiddingStrategy::AutobidV0 => {
                    if ranking_override.is_some() {
                        let candidates = &override_pctr_by_user[opportunity.user_index];
                        run_first_price_auction(
                            candidates.iter().filter_map(|&(campaign_index, pctr)| {
                                paced_pos[campaign_index]?;
                                let campaign = &campaigns[campaign_index];
                                let daily_budget =
                                    campaign.daily[day - 1].daily_budget_cents.unwrap_or(0);
                                let daily_spend = campaign.daily[day - 1].activity.spend_cents;
                                let pacing_factor = config.pacing.pacing_factor(
                                    daily_spend,
                                    daily_budget,
                                    campaign.remaining_budget_cents,
                                    config.cpc_cents,
                                    second_of_day,
                                );
                                let bid = compute_bid(
                                    BiddingStrategy::AutobidV0,
                                    config.cpc_cents,
                                    pctr,
                                    pacing_factor,
                                );
                                (bid.click_price_cents <= campaign.remaining_budget_cents)
                                    .then_some((campaign_index, bid))
                            }),
                            &mut ads_rng,
                        )
                    } else if eligible_slice.is_empty() {
                        None
                    } else if let Some(prepared) = scorer.as_ref() {
                        run_first_price_auction(
                            eligible_slice.iter().filter_map(|&campaign_index| {
                                let campaign = &campaigns[campaign_index];
                                let pctr = prepared.score_ctr(
                                    opportunity.user_index,
                                    opportunity.organic_category_index,
                                    campaign_index,
                                );
                                let daily_budget =
                                    campaign.daily[day - 1].daily_budget_cents.unwrap_or(0);
                                let daily_spend = campaign.daily[day - 1].activity.spend_cents;
                                let pacing_factor = config.pacing.pacing_factor(
                                    daily_spend,
                                    daily_budget,
                                    campaign.remaining_budget_cents,
                                    config.cpc_cents,
                                    second_of_day,
                                );
                                let bid = compute_bid(
                                    BiddingStrategy::AutobidV0,
                                    config.cpc_cents,
                                    pctr,
                                    pacing_factor,
                                );
                                (bid.click_price_cents <= campaign.remaining_budget_cents)
                                    .then_some((campaign_index, bid))
                            }),
                            &mut ads_rng,
                        )
                    } else {
                        // Neutral scoring model (Score-Uniform or Day 1 of Score-R1): all eligible
                        // campaigns share the same neutral pCTR and pacing_factor=1.0, so uniform
                        // tie-breaking selects fairly in O(1).
                        let campaign_index =
                            eligible_slice[ads_rng.random_range(0..eligible_slice.len())];
                        let campaign = &campaigns[campaign_index];
                        let daily_budget = campaign.daily[day - 1].daily_budget_cents.unwrap_or(0);
                        let daily_spend = campaign.daily[day - 1].activity.spend_cents;
                        let pacing_factor = config.pacing.pacing_factor(
                            daily_spend,
                            daily_budget,
                            campaign.remaining_budget_cents,
                            config.cpc_cents,
                            second_of_day,
                        );
                        let bid = compute_bid(
                            BiddingStrategy::AutobidV0,
                            config.cpc_cents,
                            NEUTRAL_PCTR,
                            pacing_factor,
                        );
                        (bid.final_bid_cents > 0.0
                            && bid.click_price_cents <= campaign.remaining_budget_cents)
                            .then_some(AuctionWinner {
                                candidate_index: campaign_index,
                                bid,
                            })
                    }
                }
            };

            let Some(winner) = winner else {
                if config.bidding == BiddingStrategy::AutobidV0 && ranking_override.is_some() {
                    let candidates = &override_pctr_by_user[opportunity.user_index];
                    if candidates.is_empty() {
                        daily.no_matching_category += 1;
                        daily.hourly[hour].no_matching_category += 1;
                    } else if candidates.iter().any(|&(c_idx, _)| is_throttled[c_idx]) {
                        daily.pacing_throttled += 1;
                        daily.hourly[hour].pacing_throttled += 1;
                    } else if candidates.iter().any(|&(c_idx, _)| {
                        campaigns[c_idx].remaining_budget_cents >= config.cpc_cents
                    }) {
                        daily.daily_budget_exhausted += 1;
                        daily.hourly[hour].daily_budget_exhausted += 1;
                    } else {
                        daily.insufficient_budget += 1;
                        daily.hourly[hour].insufficient_budget += 1;
                    }
                } else if !throttled.is_empty() {
                    daily.pacing_throttled += 1;
                    daily.hourly[hour].pacing_throttled += 1;
                } else if !retrieved.is_empty() {
                    daily.daily_budget_exhausted += 1;
                    daily.hourly[hour].daily_budget_exhausted += 1;
                } else {
                    daily.insufficient_budget += 1;
                    daily.hourly[hour].insufficient_budget += 1;
                }
                continue;
            };

            let campaign_index = winner.candidate_index;
            let campaign = &mut campaigns[campaign_index];
            let score = score_preference(opportunity.user, campaign.item);
            let cost = click_rng
                .random_bool(MAX_CLICK_PROBABILITY * score)
                .then_some(winner.bid.click_price_cents);

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
                    cpc_cents: winner.bid.click_price_cents,
                    clicked: cost.is_some(),
                })?;
            }

            campaign.totals.impression(cost);
            campaign.daily[day - 1].activity.impression(cost);
            daily.activity.impression(cost);
            daily.hourly[hour].activity.impression(cost);
            totals.impression(cost);

            if let Some(cost) = cost {
                campaign.remaining_budget_cents -= cost;
                retriever.on_budget_update(campaign_index, campaign.remaining_budget_cents);
                if campaign.remaining_budget_cents < config.cpc_cents {
                    remove_paced_eligible(&mut paced_eligible, &mut paced_pos, campaign_index);
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
                            remove_paced_eligible(
                                &mut paced_eligible,
                                &mut paced_pos,
                                campaign_index,
                            );
                            is_throttled[campaign_index] = true;
                            throttled.push(Reverse((unlock_second, campaign_index)));
                        }
                        None => {
                            remove_paced_eligible(
                                &mut paced_eligible,
                                &mut paced_pos,
                                campaign_index,
                            );
                        }
                    }
                }
            }

            let impression = Impression {
                timestamp: scheduled.timestamp,
                day,
                user_id: &opportunity.user.id,
                campaign_id: &campaign.id,
                ad_item_id: campaign.ad_item_id,
                category_id: campaign.category_id,
                base_bid_cents: winner.bid.base_bid_cents,
                pctr: winner.bid.pctr,
                pacing_factor: winner.bid.pacing_factor,
                final_bid_cents: winner.bid.final_bid_cents,
                click_price_cents: winner.bid.click_price_cents,
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
                    "delivered {} of {} impressions (insufficient_budget={}, pacing_throttled={}, daily_budget_exhausted={}, no_matching_category={})"
                ),
                day,
                daily.activity.impressions,
                quota,
                daily.insufficient_budget,
                daily.pacing_throttled,
                daily.daily_budget_exhausted,
                daily.no_matching_category,
            )));
        }

        if let Some(writer) = day_writer {
            writer.finish()?;
        }

        if config.serving.scoring == ScoringStrategy::ScoreR1
            && day < DAYS
            && let (Some(store), Some(trainer)) = (log_store.as_ref(), pytorch_trainer.as_mut())
        {
            let model_path = store.model_file_path(day);
            let model =
                trainer.train_from_log_store(store, day, &model_path, TrainingConfig::default())?;
            let summary = model.summary();
            training_runs.push(DailyTrainingSummary {
                trained_after_day: day,
                trainer: model.trainer().to_owned(),
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
            scorer = Some(model.prepare_scorer(&user_ids, &category_ids, &candidates));
        }

        opportunity_count += daily.opportunities;
        no_matching_category += daily.no_matching_category;
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
            no_matching_category,
            insufficient_budget,
            pacing_throttled,
            daily_budget_exhausted,
            activity: totals,
        },
        hourly: hourly_totals,
        days,
        campaigns,
        training_runs,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use ads_ml::ImpressionLogStore;
    use ads_simulator::{Item, User, generate_marketplace};

    use super::{
        Activity, CAMPAIGNS_PER_ADVERTISER, Config, DAYS, RankingStrategy, RetrievalStrategy,
        ScoringStrategy, ServingStrategy, run, run_with_impressions,
        run_with_ranking_and_impressions,
    };
    use crate::{
        bidding::BiddingStrategy,
        pacing::PacingStrategy,
        ranking::{DefaultRankingModel, RankingModel},
        timestamp::{HOURS_PER_DAY, PstTimestamp, SECONDS_PER_DAY},
    };

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
            opportunities: None,
            serving: ServingStrategy::uniform(),
            pacing: PacingStrategy::None,
            bidding: BiddingStrategy::Basebid,
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
        let config = Config {
            pacing: PacingStrategy::Pacing1,
            ..uniform_config(42, 10_000, 100, Some(300))
        };
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
        for pacing in [PacingStrategy::None, PacingStrategy::Pacing1] {
            let config = Config {
                pacing,
                ..uniform_config(42, 10_000, 100, Some(1_000_000))
            };
            let report = run(&marketplace, config.clone()).unwrap();
            assert_eq!(report, run(&marketplace, config.clone()).unwrap());
            assert_eq!(report.timezone, "PST");
            assert_eq!(report.campaigns.len(), 500);
            assert_eq!(report.days.len(), DAYS);
            assert_eq!(report.hourly.len(), HOURS_PER_DAY);
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
            pacing: PacingStrategy::Pacing1,
            ..uniform_config(42, 10_000, 100, Some(60_000))
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
        let mut marketplace = generate_marketplace(42);
        marketplace.advertisers.truncate(1);
        let target_category = marketplace.advertisers[0].catalog[0].category_id.clone();
        for item in &mut marketplace.advertisers[0].catalog {
            item.category_id.clone_from(&target_category);
        }
        for user in &mut marketplace.users {
            user.preferred_category_ids = vec![target_category.clone()];
        }

        let unpaced = run(&marketplace, uniform_config(42, 30_000, 100, None)).unwrap();
        assert_eq!(unpaced.days[29].activity.impressions, 0);
        assert!(unpaced.summary.insufficient_budget > 0);
        assert_eq!(unpaced.summary.pacing_throttled, 0);

        let mut click_events: Vec<(usize, String, u32, u64)> = Vec::new();
        let paced = run_with_impressions(
            &marketplace,
            Config {
                pacing: PacingStrategy::Pacing1,
                ..uniform_config(42, 30_000, 100, None)
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

        assert!(paced.days.iter().all(|day| day.activity.impressions > 0));
        assert!(paced.summary.pacing_throttled > 0);

        let mut spend_before_click: HashMap<(usize, String), u64> = HashMap::new();
        for (day, campaign_id, second_of_day, spend_cents) in click_events {
            let entry = spend_before_click.entry((day, campaign_id)).or_insert(0);
            let daily_budget = 1_000u64;
            assert!(
                u128::from(*entry) * u128::from(SECONDS_PER_DAY)
                    <= u128::from(daily_budget) * u128::from(second_of_day),
                "click fired ahead of linear pace on day {day} at t={second_of_day}: prior_spend={entry}"
            );
            *entry += spend_cents;
            assert!(*entry <= daily_budget);
        }

        let paced_afternoon_spend: u64 = paced.hourly[12..18]
            .iter()
            .map(|h| h.activity.spend_cents)
            .sum();
        assert!(paced_afternoon_spend > 0);
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
    fn autobid_v0_selects_higher_pctr_ads_and_outperforms_basebid_ctr() {
        let marketplace = generate_marketplace(42);
        let base = run(
            &marketplace,
            Config {
                opportunities: Some(120_000),
                pacing: PacingStrategy::Pacing1,
                bidding: BiddingStrategy::Basebid,
                ..uniform_config(42, 10_000, 100, None)
            },
        )
        .unwrap();

        let mut seen_impressions = 0u64;
        let autobid = run_with_ranking_and_impressions(
            &marketplace,
            Config {
                opportunities: Some(120_000),
                pacing: PacingStrategy::Pacing1,
                bidding: BiddingStrategy::AutobidV0,
                ..uniform_config(42, 10_000, 100, None)
            },
            &DefaultRankingModel,
            |impression| {
                assert_eq!(impression.base_bid_cents, 100);
                assert_eq!(impression.pacing_factor, 1.0);
                assert!((impression.pctr - 0.04).abs() < 1e-12);
                assert!((impression.final_bid_cents - 4.0).abs() < 1e-12);
                assert_eq!(impression.click_price_cents, 100);
                seen_impressions += 1;
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(seen_impressions, autobid.summary.activity.impressions);
        assert_eq!(autobid.summary.opportunities, 120_000);
        assert_eq!(
            autobid.summary.activity.impressions
                + autobid.summary.no_matching_category
                + autobid.summary.insufficient_budget
                + autobid.summary.pacing_throttled
                + autobid.summary.daily_budget_exhausted,
            autobid.summary.opportunities
        );

        let base_ctr =
            base.summary.activity.clicks as f64 / base.summary.activity.impressions as f64;
        let autobid_ctr =
            autobid.summary.activity.clicks as f64 / autobid.summary.activity.impressions as f64;
        assert!(base_ctr < 0.005, "unexpected base CTR: {base_ctr}");
        assert!(
            autobid_ctr > 0.035,
            "expected ~4% autobid CTR, got {autobid_ctr}"
        );
        assert_eq!(
            autobid.summary.activity.spend_cents,
            autobid.summary.activity.clicks * 100
        );
    }

    #[test]
    fn autobid_v0_uses_ranking_model_pctr_in_first_price_auction() {
        struct ItemSpecificRankingModel {
            high_item_id: String,
        }
        impl RankingModel for ItemSpecificRankingModel {
            fn predict_pctr(&self, _user: &User, item: &Item) -> f64 {
                if item.id == self.high_item_id {
                    0.08
                } else {
                    0.02
                }
            }
        }

        let mut marketplace = generate_marketplace(42);
        marketplace.advertisers.truncate(1);
        let preview = run(&marketplace, uniform_config(42, 10_000, 100, Some(30))).unwrap();
        let top_campaign_id = preview.campaigns[0].id.clone();
        let top_item_id = preview.campaigns[0].ad_item_id.to_owned();

        let ranking_model = ItemSpecificRankingModel {
            high_item_id: top_item_id,
        };
        let mut winning_campaigns = HashSet::new();
        let report = run_with_ranking_and_impressions(
            &marketplace,
            Config {
                bidding: BiddingStrategy::AutobidV0,
                ..uniform_config(42, 10_000, 100, Some(300))
            },
            &ranking_model,
            |impression| {
                winning_campaigns.insert(impression.campaign_id.to_owned());
                assert!((impression.final_bid_cents - 8.0).abs() < 1e-12);
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(report.summary.activity.impressions, 300);
        assert_eq!(winning_campaigns.len(), 1);
        assert!(winning_campaigns.contains(&top_campaign_id));
    }

    #[test]
    fn unaffordable_campaigns_leave_budgets_intact_and_cannot_meet_a_target() {
        let marketplace = generate_marketplace(42);
        for pacing in [PacingStrategy::None, PacingStrategy::Pacing1] {
            for bidding in [BiddingStrategy::Basebid, BiddingStrategy::AutobidV0] {
                let mut config = Config {
                    pacing,
                    bidding,
                    ..uniform_config(42, 72, 73, None)
                };
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
        }
    }

    #[test]
    fn logging_impressions_does_not_alter_uniform_delivery() {
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
            serving: ServingStrategy::r1(),
            impression_log_dir: Some(temp.path().to_path_buf()),
            ..uniform_config(42, 10_000, 100, impressions)
        };
        let r1_report = run(&marketplace, r1_config.clone()).unwrap();

        // Day 1 has no prior data, so R1 matches Uniform on day 1.
        assert_eq!(r1_report.days[0], uniform_report.days[0]);

        // R1 cold-trains after each day 1..=29 on all cumulative logged impressions so far.
        assert_eq!(r1_report.training_runs.len(), DAYS - 1);
        let mut cumulative_impressions = 0;
        let mut cumulative_clicks = 0;
        let store = ImpressionLogStore::new(temp.path());
        assert_eq!(
            store.available_days().unwrap(),
            (1..=DAYS).collect::<Vec<_>>()
        );
        for (idx, run_summary) in r1_report.training_runs.iter().enumerate() {
            let day = idx + 1;
            cumulative_impressions += r1_report.days[idx].activity.impressions;
            cumulative_clicks += r1_report.days[idx].activity.clicks;
            assert_eq!(run_summary.trained_after_day, day);
            assert_eq!(run_summary.trainer, "pytorch");
            assert_eq!(run_summary.impressions, cumulative_impressions);
            assert_eq!(run_summary.clicks, cumulative_clicks);
            assert!(run_summary.converged);
            assert!(store.model_file_path(day).exists());
        }
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

    #[test]
    fn serving_strategy_composes_retrieval_rank_and_score_independently() {
        let marketplace = generate_marketplace(42);
        let impressions = Some(3_000);
        let baseline = run(&marketplace, uniform_config(42, 10_000, 100, impressions)).unwrap();

        let shadow_serving = ServingStrategy::new(
            RetrievalStrategy::RetrieveV0,
            RankingStrategy::RankUniform,
            ScoringStrategy::ScoreR1,
        );
        assert_eq!(
            shadow_serving.to_string(),
            "Retrieve-V0+Rank-Uniform+Score-R1"
        );
        let shadow = run(
            &marketplace,
            Config {
                serving: shadow_serving,
                ..uniform_config(42, 10_000, 100, impressions)
            },
        )
        .unwrap();
        assert_eq!(shadow.summary, baseline.summary);
        assert_eq!(shadow.days, baseline.days);
        assert_eq!(shadow.training_runs.len(), DAYS - 1);
        let shadow_json = serde_json::to_value(shadow.config.serving).unwrap();
        assert_eq!(
            shadow_json,
            serde_json::json!({
                "retrieval": "Retrieve-V0",
                "ranking": "Rank-Uniform",
                "scoring": "Score-R1"
            })
        );

        let greedy_uniform = run(
            &marketplace,
            Config {
                serving: ServingStrategy::new(
                    RetrievalStrategy::RetrieveV0,
                    RankingStrategy::RankGreedy,
                    ScoringStrategy::ScoreUniform,
                ),
                ..uniform_config(42, 10_000, 100, impressions)
            },
        )
        .unwrap();
        assert_eq!(greedy_uniform.summary, baseline.summary);
        assert!(greedy_uniform.training_runs.is_empty());
    }

    #[test]
    fn retrieve_v0_excludes_depleted_campaigns_from_subsequent_impressions() {
        let marketplace = generate_marketplace(42);
        let report = run(&marketplace, uniform_config(42, 100, 100, None)).unwrap();
        assert!(report.summary.activity.clicks > 0);
        for campaign in &report.campaigns {
            assert!(campaign.totals.clicks <= 1);
            if campaign.remaining_budget_cents == 0 {
                let depletion_day = campaign
                    .daily
                    .iter()
                    .find(|d| d.activity.clicks > 0)
                    .expect("zero-budget campaign must have received a click")
                    .day;
                for later in &campaign.daily[depletion_day..] {
                    assert_eq!(
                        later.activity.impressions, 0,
                        "campaign {} received impressions on day {} after depleting on day {}",
                        campaign.id, later.day, depletion_day
                    );
                }
            }
        }
    }

    #[test]
    fn autobid_v0_integrates_with_score_r1_pctr_predictions() {
        let marketplace = generate_marketplace(42);
        let impressions = Some(30_000);
        let uniform_report =
            run(&marketplace, uniform_config(42, 10_000, 100, impressions)).unwrap();

        let temp = tempfile::tempdir().unwrap();
        let autobid_r1 = run(
            &marketplace,
            Config {
                serving: ServingStrategy::new(
                    RetrievalStrategy::RetrieveV0,
                    RankingStrategy::RankUniform,
                    ScoringStrategy::ScoreR1,
                ),
                pacing: PacingStrategy::Pacing1,
                bidding: BiddingStrategy::AutobidV0,
                impression_log_dir: Some(temp.path().to_path_buf()),
                ..uniform_config(42, 10_000, 100, impressions)
            },
        )
        .unwrap();

        assert_eq!(autobid_r1.training_runs.len(), DAYS - 1);
        assert_eq!(autobid_r1.summary.activity.impressions, 30_000);
        assert!(
            autobid_r1.summary.activity.clicks > uniform_report.summary.activity.clicks * 5,
            "expected autobid-v0 + Score-R1 ({} clicks) to beat Uniform ({} clicks) by >5x",
            autobid_r1.summary.activity.clicks,
            uniform_report.summary.activity.clicks,
        );
    }
}
