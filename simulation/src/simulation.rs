//! 30-day CPC simulation orchestrator.
//!
//! Orchestrates [`world_generator::VisitGenerator`] (user visit arrivals with optional PST diurnal
//! timestamps for `pacing-1`, ad slot generation, and [`world_generator::UserActivityOracle`]),
//! [`ad_server::AdServer`] (explicit 5-axis `Retrieval × Pacing × Bidding × Scoring × Ranking`
//! strategy pipeline, including `Retrieve-V0`, `Pacing-1`, `bidding-autobid-v0`, and daily
//! `Score-R1` PyTorch LR / `Score-XGB` XGBoost GBDT training via `ads-ml`), and balanced Schema v1
//! report aggregation.

use std::{
    collections::HashMap,
    io::{Error, ErrorKind},
    path::Path,
};

use ad_server::{
    AdServer, AdServerConfig, BiddingStrategy,
    CAMPAIGNS_PER_ADVERTISER as SERVER_CAMPAIGNS_PER_ADVERTISER, CampaignState,
    DAYS as SERVER_DAYS, DEFAULT_BIDDING as SERVER_DEFAULT_BIDDING,
    DEFAULT_CPC_CENTS as SERVER_DEFAULT_CPC,
    DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS as SERVER_DEFAULT_MAX_BUDGET,
    DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS as SERVER_DEFAULT_MIN_BUDGET,
    DEFAULT_PACING as SERVER_DEFAULT_PACING, DEFAULT_RETRIEVAL as SERVER_DEFAULT_RETRIEVAL,
    DEFAULT_SCORING as SERVER_DEFAULT_SCORING, DEFAULT_SEED as SERVER_DEFAULT_SEED, PacingStrategy,
    R1_SLATE_SIZE, RankerStrategy, RetrievalStrategy, SCHEMA_VERSION as SERVER_SCHEMA_VERSION,
    ScoringStrategy, StrategyProfile,
};
use ads_ml::{
    AdCandidate, ImpressionClickRef, ImpressionLogStore, PreparedScorer, PyTorchTrainer,
    SufficientStatsPayload, TrainingConfig, XgboostTrainer, XgboostTrainingConfig,
};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;
use world_generator::{
    CLICK_RNG_STREAM as WORLD_CLICK_RNG_STREAM, DEFAULT_DAILY_REQUESTS, DEFAULT_SLOTS_PER_VISIT,
    DEFAULT_WORLD_MODEL as WORLD_DEFAULT_WORLD_MODEL, Item,
    MAX_CLICK_PROBABILITY as WORLD_MAX_CLICK_PROBABILITY, Marketplace, VisitGenerator,
    VisitGeneratorConfig, WorldModel,
};

pub const SCHEMA_VERSION: u32 = SERVER_SCHEMA_VERSION;
pub const DAYS: usize = SERVER_DAYS;
pub const CAMPAIGNS_PER_ADVERTISER: usize = SERVER_CAMPAIGNS_PER_ADVERTISER;
pub const MAX_CLICK_PROBABILITY: f64 = WORLD_MAX_CLICK_PROBABILITY;
pub const DEFAULT_SEED: u64 = SERVER_DEFAULT_SEED;
pub const DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS: u64 = SERVER_DEFAULT_MIN_BUDGET;
pub const DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS: u64 = SERVER_DEFAULT_MAX_BUDGET;
pub const DEFAULT_CPC_CENTS: u64 = SERVER_DEFAULT_CPC;
pub const DEFAULT_RETRIEVAL: RetrievalStrategy = SERVER_DEFAULT_RETRIEVAL;
pub const DEFAULT_RANKER: RankerStrategy = RankerStrategy::R1;
pub const DEFAULT_SCORING: ScoringStrategy = SERVER_DEFAULT_SCORING;
pub const DEFAULT_PACING: PacingStrategy = SERVER_DEFAULT_PACING;
pub const DEFAULT_BIDDING: BiddingStrategy = SERVER_DEFAULT_BIDDING;
pub const DEFAULT_SLATE_SIZE: usize = R1_SLATE_SIZE;
pub const DEFAULT_WORLD_MODEL: WorldModel = WORLD_DEFAULT_WORLD_MODEL;
pub const CLICK_RNG_STREAM: u64 = WORLD_CLICK_RNG_STREAM;

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
    pub default_daily_requests: usize,
    #[serde(default)]
    pub default_world_model: WorldModel,
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
        default_retrieval: DEFAULT_RETRIEVAL,
        default_ranker: DEFAULT_RANKER,
        default_scoring: DEFAULT_SCORING,
        default_pacing: DEFAULT_PACING,
        default_bidding: DEFAULT_BIDDING,
        default_slate_size: DEFAULT_SLATE_SIZE,
        default_daily_requests: DEFAULT_DAILY_REQUESTS,
        default_world_model: DEFAULT_WORLD_MODEL,
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
    #[serde(default)]
    pub world_model: WorldModel,
}

impl Config {
    /// Return the explicit 5-axis [`StrategyProfile`] specified by this configuration.
    pub fn strategy_profile(&self) -> StrategyProfile {
        StrategyProfile::new(
            self.retrieval,
            self.ranker,
            self.scoring,
            self.pacing,
            self.bidding,
            self.slate_size,
        )
    }
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
            retrieval: DEFAULT_RETRIEVAL,
            ranker: DEFAULT_RANKER,
            scoring: DEFAULT_SCORING,
            pacing: DEFAULT_PACING,
            bidding: DEFAULT_BIDDING,
            slate_size: DEFAULT_SLATE_SIZE,
            world_model: DEFAULT_WORLD_MODEL,
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
    pub strategy: StrategyProfile,
    pub summary: Summary,
    pub days: Vec<Day>,
    pub campaigns: Vec<Campaign<'a>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedReport {
    pub schema_version: u32,
    pub currency: String,
    pub config: Config,
    #[serde(default)]
    pub strategy: StrategyProfile,
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
            strategy: report.strategy.clone(),
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
            strategy: report.strategy,
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

enum DailyMlTrainer {
    PyTorch(PyTorchTrainer),
    Xgboost(XgboostTrainer),
}

impl DailyMlTrainer {
    fn spawn_for(scoring: ScoringStrategy) -> Result<Option<Self>, Error> {
        match scoring {
            ScoringStrategy::ScoreR1 => Ok(Some(Self::PyTorch(PyTorchTrainer::spawn()?))),
            ScoringStrategy::ScoreXgb => Ok(Some(Self::Xgboost(XgboostTrainer::spawn()?))),
            _ => Ok(None),
        }
    }

    fn train_and_prepare(
        &mut self,
        payload: &SufficientStatsPayload<'_>,
        stats_file: &Path,
        model_out: &Path,
        user_refs: &[&str],
        cat_refs: &[&str],
        candidates: &[AdCandidate<'_>],
    ) -> Result<PreparedScorer, Error> {
        match self {
            Self::PyTorch(pt) => {
                let model = pt.train_from_sufficient_stats(
                    payload,
                    stats_file,
                    model_out,
                    TrainingConfig::default(),
                )?;
                Ok(model.prepare_scorer(user_refs, cat_refs, candidates))
            }
            Self::Xgboost(xgb) => {
                let model = xgb.train_from_sufficient_stats(
                    payload,
                    stats_file,
                    model_out,
                    XgboostTrainingConfig::default(),
                )?;
                Ok(model.prepare_scorer(user_refs, cat_refs, candidates))
            }
        }
    }
}

/// Run the 30-day CPC simulation with default temporary ML log storage when an `ads-ml` scoring
/// model (`Score-R1` or `Score-XGB`) is active.
pub fn run(marketplace: &Marketplace, config: Config) -> Result<Report<'_>, Error> {
    run_with_log_dir(marketplace, config, None)
}

/// Run the 30-day CPC simulation, optionally persisting daily `ads-ml` JSONL impression-click
/// logs and trained ML model artifacts (`Score-R1` or `Score-XGB`) to `impression_log_dir`.
pub fn run_with_log_dir<'a>(
    marketplace: &'a Marketplace,
    config: Config,
    impression_log_dir: Option<&Path>,
) -> Result<Report<'a>, Error> {
    if marketplace.schema_version != SCHEMA_VERSION || marketplace.currency != "USD" {
        return Err(invalid("expected schema version 1 marketplace data in USD"));
    }
    if config.cpc_cents == 0 {
        return Err(invalid("CPC must be positive"));
    }
    if config.slate_size == 0 {
        return Err(invalid("slate_size must be positive"));
    }

    let user_ids: Vec<String> = marketplace.users.iter().map(|u| u.id.clone()).collect();
    let item_lookup = build_item_lookup(marketplace);

    let mut ad_server = AdServer::from_marketplace(
        marketplace,
        AdServerConfig {
            seed: config.seed,
            campaign_budget_cents: config.campaign_budget_cents,
            campaign_budget_min_cents: config.campaign_budget_min_cents,
            campaign_budget_max_cents: config.campaign_budget_max_cents,
            cpc_cents: config.cpc_cents,
            retrieval: config.retrieval,
            ranker: config.ranker,
            scoring: config.scoring,
            pacing: config.pacing,
            bidding: config.bidding,
            slate_size: config.slate_size,
        },
    )?;
    let budget_cents = ad_server.total_budget_cents();
    let strategy = ad_server.strategy_profile();

    let use_ml_scoring = config.scoring.is_ml_model();
    let write_daily_jsonl = impression_log_dir.is_some();
    let _temp_log_dir: Option<TempDir>;
    let log_store: Option<ImpressionLogStore> = if let Some(dir) = impression_log_dir {
        _temp_log_dir = None;
        let store = ImpressionLogStore::new(dir);
        store.clear()?;
        Some(store)
    } else if use_ml_scoring {
        let tmp = TempDir::new()?;
        let store = ImpressionLogStore::new(tmp.path());
        _temp_log_dir = Some(tmp);
        Some(store)
    } else {
        _temp_log_dir = None;
        None
    };

    let mut trainer = DailyMlTrainer::spawn_for(config.scoring)?;

    // Pre-index users and campaigns for O(1) exact sufficient-statistics aggregation when
    // an offline-trained ads-ml scorer (Score-R1 or Score-XGB) is active, avoiding O(D^2)
    // Python JSONL re-parsing on 100M-impression runs.
    let num_users = user_ids.len();
    let num_campaigns = ad_server.campaigns().len();
    let mut user_idx_map: HashMap<&str, usize> = HashMap::new();
    let mut camp_idx_map: HashMap<String, usize> = HashMap::new();
    let mut ml_cat_vocab: Vec<String> = Vec::new();
    let mut ml_adv_vocab: Vec<String> = Vec::new();
    let mut ml_camp_vocab: Vec<String> = Vec::new();
    let mut ml_camp_meta: Vec<(usize, usize, usize)> = Vec::new();
    let mut ml_counts: Vec<(u64, u64)> = Vec::new();

    if use_ml_scoring {
        user_idx_map.reserve(num_users);
        for (u_idx, uid) in user_ids.iter().enumerate() {
            user_idx_map.insert(uid.as_str(), u_idx);
        }
        let mut cat_map: HashMap<String, usize> = HashMap::new();
        let mut adv_map: HashMap<String, usize> = HashMap::new();
        camp_idx_map.reserve(num_campaigns);
        ml_camp_vocab.reserve(num_campaigns);
        ml_camp_meta.reserve(num_campaigns);
        for (k_idx, camp) in ad_server.campaigns().iter().enumerate() {
            camp_idx_map.insert(camp.id.clone(), k_idx);
            ml_camp_vocab.push(camp.id.clone());
            let c_idx = match cat_map.get(&camp.category_id) {
                Some(&idx) => idx,
                None => {
                    let idx = ml_cat_vocab.len();
                    ml_cat_vocab.push(camp.category_id.clone());
                    cat_map.insert(camp.category_id.clone(), idx);
                    idx
                }
            };
            let a_idx = match adv_map.get(&camp.advertiser_id) {
                Some(&idx) => idx,
                None => {
                    let idx = ml_adv_vocab.len();
                    ml_adv_vocab.push(camp.advertiser_id.clone());
                    adv_map.insert(camp.advertiser_id.clone(), idx);
                    idx
                }
            };
            ml_camp_meta.push((c_idx, a_idx, k_idx));
        }
        ml_counts = vec![(0u64, 0u64); num_users * num_campaigns];
    }

    let mut visit_generator = VisitGenerator::from_marketplace(
        marketplace,
        VisitGeneratorConfig {
            seed: config.seed,
            preference_seed: Some(marketplace.seed.cast_unsigned()),
            days: DAYS,
            slots_per_visit: DEFAULT_SLOTS_PER_VISIT,
            daily_requests: DEFAULT_DAILY_REQUESTS,
            target_impressions: config.impressions,
            max_click_probability: MAX_CLICK_PROBABILITY,
            world_model: config.world_model,
        },
    )?;

    let mut days = Vec::with_capacity(DAYS);
    let mut totals = Activity::default();
    let mut total_ad_requests = 0u64;
    let mut opportunity_count = 0u64;
    let mut insufficient_budget = 0u64;

    for day in 1..=DAYS {
        let mut day_writer = if write_daily_jsonl {
            match &log_store {
                Some(store) => Some(store.create_day_writer(day)?),
                None => None,
            }
        } else {
            None
        };
        let mut write_err: Option<Error> = None;

        let day_stats = visit_generator.run_day(day, &mut ad_server, |impression| {
            if use_ml_scoring {
                if let (Some(&u_idx), Some(&k_idx)) = (
                    user_idx_map.get(impression.user_id.as_str()),
                    camp_idx_map.get(impression.ad.campaign_id.as_str()),
                ) {
                    let slot = &mut ml_counts[u_idx * num_campaigns + k_idx];
                    slot.0 += 1;
                    if impression.clicked {
                        slot.1 += 1;
                    }
                }
            }
            if let Some(writer) = day_writer.as_mut() {
                if write_err.is_none() {
                    let &(_, item) = item_lookup
                        .get(&(
                            impression.ad.advertiser_id.as_str(),
                            impression.ad.ad_item_id.as_str(),
                        ))
                        .expect("ad server only emits known catalog items");
                    let rec = ImpressionClickRef {
                        day,
                        user_id: impression.user_id.as_str(),
                        organic_item_id: "",
                        organic_category_id: "",
                        campaign_id: impression.ad.campaign_id.as_str(),
                        advertiser_id: impression.ad.advertiser_id.as_str(),
                        ad_item_id: impression.ad.ad_item_id.as_str(),
                        ad_category_id: impression.ad.category_id.as_str(),
                        ad_price_cents: item.price_cents,
                        cpc_cents: impression.ad.cpc_cents,
                        clicked: impression.clicked,
                    };
                    if let Err(err) = writer.write_ref(&rec) {
                        write_err = Some(err);
                    }
                }
            }
        })?;

        if let Some(err) = write_err {
            return Err(err);
        }
        if let Some(writer) = day_writer {
            writer.finish()?;
        }

        if use_ml_scoring && day < DAYS {
            if let (Some(store), Some(ml_trainer)) = (log_store.as_ref(), trainer.as_mut()) {
                let model_out = store.model_file_path(day);
                let stats_file = store.root().join(format!("stats-day-{day:02}.json"));
                let user_refs: Vec<&str> = user_ids.iter().map(String::as_str).collect();
                let cat_vocab_refs: Vec<&str> = ml_cat_vocab.iter().map(String::as_str).collect();
                let adv_vocab_refs: Vec<&str> = ml_adv_vocab.iter().map(String::as_str).collect();
                let camp_vocab_refs: Vec<&str> = ml_camp_vocab.iter().map(String::as_str).collect();
                let mut groups: Vec<(usize, usize, usize, usize, u8, u64, u64)> = Vec::new();
                for (flat_idx, &(imp, clk)) in ml_counts.iter().enumerate() {
                    if imp > 0 {
                        let u_idx = flat_idx / num_campaigns;
                        let k_idx = flat_idx % num_campaigns;
                        let (c_idx, a_idx, _) = ml_camp_meta[k_idx];
                        groups.push((u_idx, c_idx, a_idx, k_idx, 0, imp, clk));
                    }
                }
                let payload = SufficientStatsPayload {
                    user_vocab: &user_refs,
                    ad_category_vocab: &cat_vocab_refs,
                    advertiser_vocab: &adv_vocab_refs,
                    campaign_vocab: &camp_vocab_refs,
                    groups: &groups,
                };
                let mut cat_refs: Vec<&str> = Vec::with_capacity(ml_cat_vocab.len() + 1);
                cat_refs.push("");
                cat_refs.extend(ml_cat_vocab.iter().map(String::as_str));
                let candidates: Vec<AdCandidate<'_>> = ad_server
                    .campaigns()
                    .iter()
                    .map(|c| AdCandidate {
                        campaign_id: c.id.as_str(),
                        advertiser_id: c.advertiser_id.as_str(),
                        ad_item_id: c.ad_item_id.as_str(),
                        ad_category_id: c.category_id.as_str(),
                        ad_price_cents: c.item.price_cents,
                    })
                    .collect();
                let scorer = ml_trainer.train_and_prepare(
                    &payload,
                    &stats_file,
                    &model_out,
                    &user_refs,
                    &cat_refs,
                    &candidates,
                )?;
                ad_server.set_ml_scorer(Some(scorer));
            }
        }

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
        strategy,
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

    use ad_server::{
        BiddingStrategy, PacingStrategy, R1_SLATE_SIZE, RankerStrategy, RetrievalStrategy,
        ScoringStrategy,
    };
    use world_generator::generate_marketplace;

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

        for (idx, category) in marketplace.categories.iter_mut().enumerate() {
            category.id = format!("no-campaign-category-{:03}", idx + 1);
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
            retrieval: RetrievalStrategy::RetrieveV0,
            ranker: RankerStrategy::R1,
            scoring: ScoringStrategy::S0,
            pacing: PacingStrategy::P0,
            bidding: BiddingStrategy::Basebid,
            slate_size: R1_SLATE_SIZE,
            world_model: super::DEFAULT_WORLD_MODEL,
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
    fn pacing_1_and_autobid_v0_run_end_to_end_and_balance_ledgers() {
        let marketplace = generate_marketplace(42);
        let config = Config {
            seed: 42,
            campaign_budget_cents: Some(30_000),
            cpc_cents: 100,
            impressions: Some(60_000),
            retrieval: RetrievalStrategy::RetrieveV0,
            ranker: RankerStrategy::R2,
            scoring: ScoringStrategy::S1,
            pacing: PacingStrategy::Pacing1,
            bidding: BiddingStrategy::AutobidV0,
            slate_size: R1_SLATE_SIZE,
            ..Config::default()
        };
        let report = run(&marketplace, config).unwrap();
        assert_eq!(report.summary.activity.impressions, 60_000);
        assert_eq!(report.strategy.retrieval, RetrievalStrategy::RetrieveV0);
        assert_eq!(report.strategy.pacing, PacingStrategy::Pacing1);
        assert_eq!(report.strategy.bidding, BiddingStrategy::AutobidV0);
        assert_eq!(
            report.summary.remaining_budget_cents + report.summary.activity.spend_cents,
            report.summary.budget_cents
        );
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
        let expected_requests = (super::DEFAULT_DAILY_REQUESTS * DAYS) as u64;
        assert_eq!(report.summary.ad_requests, expected_requests);
        assert_eq!(report.summary.opportunities, expected_requests);
        assert_eq!(report.summary.activity, Activity::default());
        assert_eq!(
            report.summary.remaining_budget_cents,
            report.summary.budget_cents
        );
        assert_eq!(report.summary.insufficient_budget, expected_requests);

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
                retrieval: RetrievalStrategy::RetrieveV0,
                ranker: RankerStrategy::R1,
                scoring: ScoringStrategy::S0,
                pacing: PacingStrategy::P0,
                bidding: BiddingStrategy::Basebid,
                slate_size: R1_SLATE_SIZE,
                world_model: super::DEFAULT_WORLD_MODEL,
            }
        );
    }

    #[test]
    fn owned_report_round_trips_natural_mode_json() {
        let marketplace = generate_marketplace(42);
        let report = run(&marketplace, Config::default()).unwrap();
        assert_eq!(report.summary.ad_requests, 360_000);
        assert_eq!(report.summary.activity.impressions, 360_000);
        let borrowed_json = serde_json::to_string(&report).unwrap();
        let owned = report.to_owned_report();
        let owned_json = serde_json::to_string(&owned).unwrap();
        assert_eq!(borrowed_json, owned_json);

        let decoded: OwnedReport = serde_json::from_str(&borrowed_json).unwrap();
        assert_eq!(decoded, owned);
        assert_eq!(report.into_owned_report(), owned);
    }

    #[test]
    fn score_xgb_runs_end_to_end_exports_models_and_balances_ledgers() {
        use super::run_with_log_dir;
        use tempfile::TempDir;

        let marketplace = generate_marketplace(42);
        let log_dir = TempDir::new().unwrap();
        let config = Config {
            seed: 42,
            campaign_budget_cents: Some(30_000),
            cpc_cents: 100,
            impressions: Some(15_000),
            retrieval: RetrievalStrategy::RetrieveV0,
            ranker: RankerStrategy::R3,
            scoring: ScoringStrategy::ScoreXgb,
            pacing: PacingStrategy::P0,
            bidding: BiddingStrategy::Basebid,
            slate_size: 10,
            ..Config::default()
        };
        let report_a = run_with_log_dir(&marketplace, config, Some(log_dir.path())).unwrap();
        let report_b = run(&marketplace, config).unwrap();

        assert_eq!(report_a, report_b);
        assert_eq!(report_a.strategy.scoring, ScoringStrategy::ScoreXgb);
        assert_eq!(report_a.summary.activity.impressions, 15_000);
        assert!(report_a.summary.activity.clicks > 0);
        assert_eq!(
            report_a.summary.remaining_budget_cents + report_a.summary.activity.spend_cents,
            report_a.summary.budget_cents
        );

        let model_day_1: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(log_dir.path().join("model-day-01.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(model_day_1["trainer"], "xgboost");
        let model_day_29: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(log_dir.path().join("model-day-29.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(model_day_29["trainer"], "xgboost");
        assert!(
            model_day_29["trees"]
                .as_array()
                .is_some_and(|trees| !trees.is_empty())
        );
    }

    #[test]
    fn world_model_v1_runs_deterministically_balances_ledgers_and_allows_non_preferred_clicks() {
        let mut marketplace = generate_marketplace(42);
        let cfg_v0 = Config {
            seed: 42,
            impressions: Some(60_000),
            world_model: super::WorldModel::V0,
            ..Config::default()
        };
        let cfg_v1 = Config {
            world_model: super::WorldModel::V1,
            ..cfg_v0
        };

        let report_v0 = run(&marketplace, cfg_v0).unwrap();
        let report_v1_a = run(&marketplace, cfg_v1).unwrap();
        let report_v1_b = run(&marketplace, cfg_v1).unwrap();

        assert_eq!(report_v1_a.to_owned_report(), report_v1_b.to_owned_report());
        assert_eq!(report_v1_a.config.world_model, super::WorldModel::V1);
        assert_eq!(report_v1_a.summary.activity.impressions, 60_000);
        assert_eq!(
            report_v1_a.summary.activity.spend_cents,
            report_v1_a.summary.activity.clicks * DEFAULT_CPC_CENTS
        );
        assert_eq!(
            report_v1_a.summary.remaining_budget_cents + report_v1_a.summary.activity.spend_cents,
            report_v1_a.summary.budget_cents
        );
        // Because v1 injects noisy background CTR on non-preferred categories (~0.2%) plus
        // hierarchical shifts & noise, total clicks under random R1 should clearly exceed v0.
        assert!(
            report_v1_a.summary.activity.clicks > report_v0.summary.activity.clicks,
            "v1 clicks ({}) should exceed v0 clicks ({}) due to noisy non-preferred background CTR",
            report_v1_a.summary.activity.clicks,
            report_v0.summary.activity.clicks
        );

        // Even when no served ad matches a user's preferred categories, v1 still produces noisy clicks.
        for (idx, category) in marketplace.categories.iter_mut().enumerate() {
            category.id = format!("no-campaign-category-{:03}", idx + 1);
        }
        let non_pref_v1 = run(
            &marketplace,
            Config {
                seed: 42,
                impressions: Some(15_000),
                world_model: super::WorldModel::V1,
                ..Config::default()
            },
        )
        .unwrap();
        assert!(
            non_pref_v1.summary.activity.clicks > 0,
            "world-model-v1 should produce noisy clicks even when all served ads are outside preferred categories"
        );
    }
}
