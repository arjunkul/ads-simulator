//! Standalone Ad-Server module.
//!
//! Accepts a `user_id` via [`AdServer::serve_ads`] and returns a set of ads from distinct
//! funded campaigns selected by a pluggable [`AdsRanker`] strategy (starting with
//! [`RankerStrategy::R1`], which selects up to 20 random campaigns with remaining budget).

pub mod bidding;
pub mod ranker;
pub mod retrieval;
pub mod strategy;
pub use world_generator::timestamp;

use std::collections::{HashMap, HashSet};
use std::io::{Error, ErrorKind};

use ads_ml::PreparedScorer;
use rand::seq::IndexedRandom;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};
use world_generator::{AdServingTarget, Item, Marketplace};

pub use bidding::{
    AuctionWinner, Bid, BiddingStrategy, DEFAULT_BIDDING, compute_bid, run_first_price_auction,
};
pub use ranker::{
    AdsRanker, R1_SLATE_SIZE, R1RandomBudgetedRanker, R2_EPSILON, R2_SLATE_SIZE,
    R2_UCB_EXPLORATION_C, R2OnlineCtrRanker, R3_SLATE_SIZE, R3GreedyTopKRanker, RankerStrategy,
    ServingContext,
};
pub use retrieval::{
    ActiveBudgetRetriever, CampaignRetriever, DEFAULT_RETRIEVAL, RETRIEVE_V0_MAX_CANDIDATES,
    RetrievalRequest, RetrievalStrategy, build_retriever,
};
pub use strategy::{
    DEFAULT_PACING, DEFAULT_SCORING, PacingStrategy, ScoringStrategy, StrategyInfo,
    StrategyProfile, daily_budget_cents, pacing_1_unlock_second, validate_strategy,
};
pub use timestamp::{
    HOURS_PER_DAY, PstTimestamp, SECONDS_PER_DAY, SECONDS_PER_HOUR, TIMEZONE_LABEL,
    sample_pst_second_of_day,
};

pub const SCHEMA_VERSION: u32 = 1;
pub const DAYS: usize = 30;
pub const CAMPAIGNS_PER_ADVERTISER: usize = 10;
pub const DEFAULT_SEED: u64 = 42;
/// Minimum default per-campaign upfront budget in USD cents ($1,000.00).
pub const DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS: u64 = 100_000;
/// Maximum default per-campaign upfront budget in USD cents ($10,000.00).
pub const DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS: u64 = 1_000_000;
pub const DEFAULT_CPC_CENTS: u64 = 100;

/// Compiled specification and defaults of the `ad-server` module.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdServerSpec {
    pub crate_name: String,
    pub version: String,
    pub schema_version: u32,
    pub days: usize,
    pub campaigns_per_advertiser: usize,
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
    #[serde(default)]
    pub supported_retrievers: Vec<StrategyInfo>,
    pub supported_rankers: Vec<RankerInfo>,
    #[serde(default)]
    pub supported_scorers: Vec<StrategyInfo>,
    #[serde(default)]
    pub supported_pacers: Vec<StrategyInfo>,
    #[serde(default)]
    pub supported_bidders: Vec<StrategyInfo>,
}

/// Metadata describing a registered ranker strategy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RankerInfo {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub formula: String,
    pub default_slate_size: usize,
    #[serde(default)]
    pub default_scoring: ScoringStrategy,
}

/// Return the live compiled specification of the `ad-server` crate.
pub fn ad_server_spec() -> AdServerSpec {
    AdServerSpec {
        crate_name: env!("CARGO_PKG_NAME").to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        schema_version: SCHEMA_VERSION,
        days: DAYS,
        campaigns_per_advertiser: CAMPAIGNS_PER_ADVERTISER,
        default_seed: DEFAULT_SEED,
        default_min_campaign_budget_cents: DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS,
        default_max_campaign_budget_cents: DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS,
        default_cpc_cents: DEFAULT_CPC_CENTS,
        default_retrieval: DEFAULT_RETRIEVAL,
        default_ranker: RankerStrategy::R1,
        default_scoring: DEFAULT_SCORING,
        default_pacing: DEFAULT_PACING,
        default_bidding: DEFAULT_BIDDING,
        default_slate_size: R1_SLATE_SIZE,
        supported_retrievers: RetrievalStrategy::all()
            .iter()
            .map(|&ret| StrategyInfo {
                id: ret.as_str().to_owned(),
                name: ret.name().to_owned(),
                description: ret.description().to_owned(),
                formula: ret.formula().to_owned(),
            })
            .collect(),
        supported_rankers: RankerStrategy::all()
            .iter()
            .map(|&strategy| RankerInfo {
                id: strategy.as_str().to_owned(),
                name: strategy.name().to_owned(),
                description: strategy.description().to_owned(),
                formula: strategy.formula().to_owned(),
                default_slate_size: strategy.default_slate_size(),
                default_scoring: strategy.default_scoring(),
            })
            .collect(),
        supported_scorers: ScoringStrategy::all()
            .iter()
            .map(|&scoring| StrategyInfo {
                id: scoring.as_str().to_owned(),
                name: scoring.name().to_owned(),
                description: scoring.description().to_owned(),
                formula: scoring.formula().to_owned(),
            })
            .collect(),
        supported_pacers: PacingStrategy::all()
            .iter()
            .map(|&pacing| StrategyInfo {
                id: pacing.as_str().to_owned(),
                name: pacing.name().to_owned(),
                description: pacing.description().to_owned(),
                formula: pacing.formula().to_owned(),
            })
            .collect(),
        supported_bidders: BiddingStrategy::all()
            .iter()
            .map(|&bidding| StrategyInfo {
                id: bidding.as_str().to_owned(),
                name: bidding.name().to_owned(),
                description: bidding.description().to_owned(),
                formula: bidding.formula().to_owned(),
            })
            .collect(),
    }
}

/// Configuration for initializing an [`AdServer`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdServerConfig {
    pub seed: u64,
    /// Optional fixed per-campaign budget override in cents (`None` randomizes each campaign
    /// uniformly across `[campaign_budget_min_cents, campaign_budget_max_cents]`).
    #[serde(default)]
    pub campaign_budget_cents: Option<u64>,
    #[serde(default = "default_min_campaign_budget_cents")]
    pub campaign_budget_min_cents: u64,
    #[serde(default = "default_max_campaign_budget_cents")]
    pub campaign_budget_max_cents: u64,
    pub cpc_cents: u64,
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

const fn default_min_campaign_budget_cents() -> u64 {
    DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS
}

const fn default_max_campaign_budget_cents() -> u64 {
    DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS
}

const fn default_slate_size() -> usize {
    R1_SLATE_SIZE
}

impl AdServerConfig {
    /// Return the effective `(min_cents, max_cents)` bounds applied to campaign budgets.
    pub const fn effective_budget_bounds(&self) -> (u64, u64) {
        match self.campaign_budget_cents {
            Some(fixed) => (fixed, fixed),
            None => (
                self.campaign_budget_min_cents,
                self.campaign_budget_max_cents,
            ),
        }
    }

    /// Return the explicit 5-axis [`StrategyProfile`] represented by this config.
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

impl Default for AdServerConfig {
    fn default() -> Self {
        Self {
            seed: DEFAULT_SEED,
            campaign_budget_cents: None,
            campaign_budget_min_cents: DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS,
            campaign_budget_max_cents: DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS,
            cpc_cents: DEFAULT_CPC_CENTS,
            retrieval: DEFAULT_RETRIEVAL,
            ranker: RankerStrategy::R1,
            scoring: DEFAULT_SCORING,
            pacing: DEFAULT_PACING,
            bidding: DEFAULT_BIDDING,
            slate_size: R1_SLATE_SIZE,
        }
    }
}

/// Impression, click, and spend counters for a campaign or day.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CampaignActivity {
    pub impressions: u64,
    pub clicks: u64,
    pub spend_cents: u64,
}

impl CampaignActivity {
    pub fn record(&mut self, cost: Option<u64>) {
        self.impressions += 1;
        if let Some(cost) = cost {
            self.clicks += 1;
            self.spend_cents += cost;
        }
    }
}

/// Daily counters for a campaign.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CampaignDailyActivity {
    pub day: usize,
    #[serde(flatten)]
    pub activity: CampaignActivity,
}

/// Live state and ledger for a single ad campaign owned by [`AdServer`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CampaignState {
    pub id: String,
    pub advertiser_id: String,
    pub ad_item_id: String,
    pub item: Item,
    pub category_id: String,
    pub budget_cents: u64,
    pub remaining_budget_cents: u64,
    pub totals: CampaignActivity,
    pub daily: Vec<CampaignDailyActivity>,
}

impl CampaignState {
    /// True if this campaign can currently afford at least one click at `cpc_cents`.
    pub fn has_budget(&self, cpc_cents: u64) -> bool {
        self.remaining_budget_cents >= cpc_cents
    }
}

/// One ad returned inside an [`AdResponse`] slate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServedAd {
    /// One-based rank position within the returned slate (`1..=ads.len()`).
    pub slot_position: usize,
    /// Internal index of the campaign inside [`AdServer`].
    pub campaign_index: usize,
    pub campaign_id: String,
    pub advertiser_id: String,
    pub ad_item_id: String,
    pub category_id: String,
    pub cpc_cents: u64,
}

/// Response returned by [`AdServer::serve_ads`] for a given `user_id`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdResponse {
    pub user_id: String,
    #[serde(default)]
    pub retrieval: RetrievalStrategy,
    pub strategy: RankerStrategy,
    #[serde(default)]
    pub scoring: ScoringStrategy,
    #[serde(default)]
    pub pacing: PacingStrategy,
    #[serde(default)]
    pub bidding: BiddingStrategy,
    pub ads: Vec<ServedAd>,
}

/// Stateful ad-server owning campaign budgets, an O(1) [`CampaignRetriever`] index (`Retrieve-V0`),
/// optional `ads-ml` [`PreparedScorer`] (`Score-R1`, `Score-XGB`), and explicit 5-axis serving
/// strategies (`Retrieval × Pacing × Bidding × Scoring × Ranking`).
pub struct AdServer {
    config: AdServerConfig,
    campaigns: Vec<CampaignState>,
    retriever: Box<dyn CampaignRetriever>,
    user_index_lookup: HashMap<String, usize>,
    ml_scorer: Option<PreparedScorer>,
    paced_indices: Vec<usize>,
    current_day: usize,
    current_second_of_day: u32,
    total_budget_cents: u64,
    ranker: Box<dyn AdsRanker>,
    rng: ChaCha8Rng,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message.into())
}

fn sample_campaign_budget(min_cents: u64, max_cents: u64, rng: &mut ChaCha8Rng) -> u64 {
    if min_cents == max_cents {
        return min_cents;
    }
    if min_cents % 100 == 0 && max_cents % 100 == 0 {
        rng.random_range((min_cents / 100)..=(max_cents / 100)) * 100
    } else {
        rng.random_range(min_cents..=max_cents)
    }
}

impl AdServer {
    /// Initialize an [`AdServer`] from a schema v1 marketplace's advertiser catalogs using the
    /// 5-axis strategy specified in `config`.
    pub fn from_marketplace(
        marketplace: &Marketplace,
        config: AdServerConfig,
    ) -> Result<Self, Error> {
        let ranker =
            config
                .ranker
                .build_with_scoring(config.slate_size, config.scoring, config.cpc_cents);
        Self::with_ranker(marketplace, config, ranker)
    }

    /// Initialize an [`AdServer`] with a custom [`AdsRanker`] implementation.
    pub fn with_ranker(
        marketplace: &Marketplace,
        config: AdServerConfig,
        ranker: Box<dyn AdsRanker>,
    ) -> Result<Self, Error> {
        if marketplace.schema_version != SCHEMA_VERSION || marketplace.currency != "USD" {
            return Err(invalid("expected schema version 1 marketplace data in USD"));
        }
        if config.cpc_cents == 0 {
            return Err(invalid("CPC must be positive"));
        }
        if config.slate_size == 0 || ranker.slate_size() == 0 {
            return Err(invalid("slate_size must be positive"));
        }
        validate_strategy(
            config.retrieval,
            config.ranker,
            config.scoring,
            config.pacing,
            config.bidding,
        )
        .map_err(invalid)?;
        let (min_budget_cents, max_budget_cents) = config.effective_budget_bounds();
        if min_budget_cents > max_budget_cents {
            return Err(invalid(format!(
                "campaign_budget_min_cents ({min_budget_cents}) must be <= campaign_budget_max_cents ({max_budget_cents})"
            )));
        }

        let mut rng = ChaCha8Rng::seed_from_u64(config.seed);
        let mut campaigns = Vec::new();
        let mut advertiser_ids = HashSet::new();
        let mut total_budget_cents = 0u64;

        for advertiser in &marketplace.advertisers {
            if !advertiser_ids.insert(&advertiser.id) {
                return Err(invalid(format!("duplicate advertiser {}", advertiser.id)));
            }
            let distinct: HashSet<_> = advertiser.catalog.iter().map(|item| &item.id).collect();
            if distinct.len() < CAMPAIGNS_PER_ADVERTISER
                || distinct.len() != advertiser.catalog.len()
            {
                return Err(invalid(format!(
                    "advertiser {} must have at least ten catalog items with unique IDs",
                    advertiser.id
                )));
            }

            let chosen: Vec<&Item> = advertiser
                .catalog
                .choose_multiple(&mut rng, CAMPAIGNS_PER_ADVERTISER)
                .collect();
            for (index, item) in chosen.into_iter().enumerate() {
                let budget_cents =
                    sample_campaign_budget(min_budget_cents, max_budget_cents, &mut rng);
                total_budget_cents = total_budget_cents
                    .checked_add(budget_cents)
                    .ok_or_else(|| invalid("total campaign budget does not fit in the report"))?;
                campaigns.push(CampaignState {
                    id: format!("{}-campaign-{:02}", advertiser.id, index + 1),
                    advertiser_id: advertiser.id.clone(),
                    ad_item_id: item.id.clone(),
                    item: Item {
                        id: item.id.clone(),
                        category_id: item.category_id.clone(),
                        name: item.name.clone(),
                        price_cents: item.price_cents,
                    },
                    category_id: item.category_id.clone(),
                    budget_cents,
                    remaining_budget_cents: budget_cents,
                    totals: CampaignActivity::default(),
                    daily: (1..=DAYS)
                        .map(|day| CampaignDailyActivity {
                            day,
                            activity: CampaignActivity::default(),
                        })
                        .collect(),
                });
            }
        }

        let initial_budgets: Vec<u64> = campaigns.iter().map(|c| c.budget_cents).collect();
        let retriever = build_retriever(config.retrieval, &initial_budgets, config.cpc_cents);
        let user_index_lookup: HashMap<String, usize> = marketplace
            .users
            .iter()
            .enumerate()
            .map(|(idx, u)| (u.id.clone(), idx))
            .collect();
        let paced_indices = Vec::with_capacity(campaigns.len());

        Ok(Self {
            config,
            campaigns,
            retriever,
            user_index_lookup,
            ml_scorer: None,
            paced_indices,
            current_day: 1,
            current_second_of_day: 0,
            total_budget_cents,
            ranker,
            rng,
        })
    }

    /// Set the current 1-based simulated day (`1..=30`) used by daily pacing strategies.
    pub fn set_day(&mut self, day: usize) -> Result<(), Error> {
        if day == 0 || day > DAYS {
            return Err(invalid(format!("day {day} out of range 1..={DAYS}")));
        }
        self.current_day = day;
        Ok(())
    }

    /// Current 1-based simulated day (`1..=30`).
    pub fn current_day(&self) -> usize {
        self.current_day
    }

    /// Set the current PST second-of-day (`0..86_400`) used by intraday pacing (`pacing-1`).
    pub fn set_second_of_day(&mut self, second_of_day: u32) -> Result<(), Error> {
        if second_of_day >= SECONDS_PER_DAY {
            return Err(invalid(format!(
                "second_of_day {second_of_day} out of range 0..{SECONDS_PER_DAY}"
            )));
        }
        self.current_second_of_day = second_of_day;
        Ok(())
    }

    /// Current PST second-of-day (`0..86_400`).
    pub fn current_second_of_day(&self) -> u32 {
        self.current_second_of_day
    }

    /// Install or clear an `ads-ml` [`PreparedScorer`] for ML CTR scoring (`Score-R1`, `Score-XGB`).
    pub fn set_ml_scorer(&mut self, scorer: Option<PreparedScorer>) {
        self.ml_scorer = scorer;
    }

    /// Convenience helper setting `day` (`1..=30`) before serving ads for `user_id`.
    pub fn serve_ads_for_day(&mut self, day: usize, user_id: &str) -> Result<AdResponse, Error> {
        self.set_day(day)?;
        Ok(self.serve_ads(user_id))
    }

    /// Serve a set of ads from distinct retrieved, paced, scored, bid, and ranked campaigns for `user_id`.
    ///
    /// Guarantees:
    /// - Stage 1 (Retrieval): O(20) lookup via [`CampaignRetriever`] (`Retrieve-V0`, up to 20 funded campaigns).
    /// - Stage 2 (Pacing): Applies [`PacingStrategy`] (`P0`/`P1`/`P2`/`pacing-1`) for `(day, second_of_day)`.
    /// - Stage 3–5 (Scoring, Bidding, Ranking): Scores & bids candidates and selects up to `slate_size()`
    ///   distinct funded campaigns.
    pub fn serve_ads(&mut self, user_id: &str) -> AdResponse {
        let retrieval = self.config.retrieval;
        let strategy = self.ranker.strategy();
        let scoring = self.config.scoring;
        let pacing = self.config.pacing;
        let bidding = self.config.bidding;
        let user_index = self.user_index_lookup.get(user_id).copied();
        let req = RetrievalRequest {
            day: self.current_day,
            user_id,
            user_index: user_index.unwrap_or(0),
            organic_category_id: "",
            organic_category_index: 0,
        };

        let retrieved = self.retriever.retrieve(&req, &mut self.rng);
        if retrieved.is_empty() {
            return AdResponse {
                user_id: user_id.to_owned(),
                retrieval,
                strategy,
                scoring,
                pacing,
                bidding,
                ads: Vec::new(),
            };
        }

        let max_ads = self.ranker.slate_size();
        let ctx = ServingContext {
            user_index,
            ml_scorer: self.ml_scorer.as_ref(),
            bidding,
        };
        let selected = if pacing == PacingStrategy::P0 {
            self.ranker.select_campaigns_with_context(
                user_id,
                retrieved,
                &self.campaigns,
                &ctx,
                &mut self.rng,
            )
        } else {
            pacing.filter_eligible(
                self.current_day,
                DAYS,
                self.current_second_of_day,
                self.config.cpc_cents,
                retrieved,
                &self.campaigns,
                &mut self.rng,
                &mut self.paced_indices,
            );
            if self.paced_indices.is_empty() {
                return AdResponse {
                    user_id: user_id.to_owned(),
                    retrieval,
                    strategy,
                    scoring,
                    pacing,
                    bidding,
                    ads: Vec::new(),
                };
            }
            self.ranker.select_campaigns_with_context(
                user_id,
                &self.paced_indices,
                &self.campaigns,
                &ctx,
                &mut self.rng,
            )
        };

        let mut seen = HashSet::with_capacity(selected.len().min(max_ads));
        let mut ads = Vec::with_capacity(selected.len().min(max_ads));
        for campaign_index in selected {
            if ads.len() >= max_ads {
                break;
            }
            let Some(campaign) = self.campaigns.get(campaign_index) else {
                continue;
            };
            if !campaign.has_budget(self.config.cpc_cents) {
                continue;
            }
            if !seen.insert(campaign_index) {
                continue;
            }
            ads.push(ServedAd {
                slot_position: ads.len() + 1,
                campaign_index,
                campaign_id: campaign.id.clone(),
                advertiser_id: campaign.advertiser_id.clone(),
                ad_item_id: campaign.ad_item_id.clone(),
                category_id: campaign.category_id.clone(),
                cpc_cents: self.config.cpc_cents,
            });
        }

        AdResponse {
            user_id: user_id.to_owned(),
            retrieval,
            strategy,
            scoring,
            pacing,
            bidding,
            ads,
        }
    }

    /// Record a delivered impression (and optional click) against `campaign_index` on `day` (1..=30).
    ///
    /// When `clicked` is `true`, deducts `cpc_cents` from remaining budget and updates the
    /// [`CampaignRetriever`] index in O(1) so exhausted campaigns immediately stop being retrieved.
    pub fn record_impression(
        &mut self,
        day: usize,
        campaign_index: usize,
        clicked: bool,
    ) -> Result<Option<u64>, Error> {
        if day == 0 || day > DAYS {
            return Err(invalid(format!("day {day} out of range 1..={DAYS}")));
        }
        let cpc = self.config.cpc_cents;
        let campaign = self
            .campaigns
            .get_mut(campaign_index)
            .ok_or_else(|| invalid(format!("unknown campaign index {campaign_index}")))?;

        let cost = if clicked {
            if campaign.remaining_budget_cents < cpc {
                return Err(invalid(format!(
                    "campaign '{}' cannot afford click CPC ({} < {})",
                    campaign.id, campaign.remaining_budget_cents, cpc
                )));
            }
            campaign.remaining_budget_cents -= cpc;
            Some(cpc)
        } else {
            None
        };

        let remaining = campaign.remaining_budget_cents;
        campaign.totals.record(cost);
        campaign.daily[day - 1].activity.record(cost);

        if cost.is_some() {
            self.retriever.on_budget_update(campaign_index, remaining);
        }

        Ok(cost)
    }

    /// True if at least one campaign can currently afford a click.
    pub fn has_funded_campaigns(&self) -> bool {
        self.retriever.active_count() > 0
    }

    /// Number of campaigns that currently have remaining budget >= CPC.
    pub fn active_campaign_count(&self) -> usize {
        self.retriever.active_count()
    }

    /// Configured CPC in USD cents.
    pub fn cpc_cents(&self) -> u64 {
        self.config.cpc_cents
    }

    /// Active retrieval strategy.
    pub fn retrieval(&self) -> RetrievalStrategy {
        self.config.retrieval
    }

    /// Active ranker strategy.
    pub fn strategy(&self) -> RankerStrategy {
        self.ranker.strategy()
    }

    /// Active candidate scoring strategy.
    pub fn scoring(&self) -> ScoringStrategy {
        self.config.scoring
    }

    /// Active budget pacing strategy.
    pub fn pacing(&self) -> PacingStrategy {
        self.config.pacing
    }

    /// Active bidding strategy.
    pub fn bidding(&self) -> BiddingStrategy {
        self.config.bidding
    }

    /// Explicit 5-axis strategy manifest for this server instance.
    pub fn strategy_profile(&self) -> StrategyProfile {
        StrategyProfile::new(
            self.config.retrieval,
            self.ranker.strategy(),
            self.config.scoring,
            self.config.pacing,
            self.config.bidding,
            self.ranker.slate_size(),
        )
    }

    /// Configured maximum slate size per request.
    pub fn slate_size(&self) -> usize {
        self.ranker.slate_size()
    }

    /// Initial total budget pool across all campaigns.
    pub fn total_budget_cents(&self) -> u64 {
        self.total_budget_cents
    }

    /// Read-only view of a single campaign by index.
    pub fn campaign(&self, campaign_index: usize) -> Option<&CampaignState> {
        self.campaigns.get(campaign_index)
    }

    /// Read-only view of all campaigns owned by the ad-server.
    pub fn campaigns(&self) -> &[CampaignState] {
        &self.campaigns
    }

    /// Consume the ad-server and return all campaign states with their recorded ledgers.
    pub fn into_campaigns(self) -> Vec<CampaignState> {
        self.campaigns
    }
}

impl AdServingTarget for AdServer {
    type Ad = ServedAd;
    type Response = AdResponse;

    fn set_day(&mut self, day: usize) -> Result<(), Error> {
        AdServer::set_day(self, day)
    }

    fn set_second_of_day(&mut self, second_of_day: u32) -> Result<(), Error> {
        AdServer::set_second_of_day(self, second_of_day)
    }

    fn uses_pst_diurnal_timestamps(&self) -> bool {
        self.pacing() == PacingStrategy::Pacing1
    }

    fn serve_ads(&mut self, user_id: &str) -> Self::Response {
        AdServer::serve_ads(self, user_id)
    }

    fn into_response_parts(response: Self::Response) -> (String, Vec<Self::Ad>) {
        (response.user_id, response.ads)
    }

    fn ad_slot_position(ad: &Self::Ad) -> usize {
        ad.slot_position
    }

    fn ad_campaign_index(ad: &Self::Ad) -> usize {
        ad.campaign_index
    }

    fn ad_category_id(ad: &Self::Ad) -> &str {
        &ad.category_id
    }

    fn ad_advertiser_id(ad: &Self::Ad) -> &str {
        &ad.advertiser_id
    }

    fn record_impression(
        &mut self,
        day: usize,
        campaign_index: usize,
        clicked: bool,
    ) -> Result<Option<u64>, Error> {
        AdServer::record_impression(self, day, campaign_index, clicked)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use rand_chacha::ChaCha8Rng;
    use world_generator::{
        DEFAULT_DAILY_REQUESTS, VisitGenerator, VisitGeneratorConfig, generate_marketplace,
    };

    use crate::{
        AdServer, AdServerConfig, AdsRanker, CampaignState, R1_SLATE_SIZE, RankerStrategy,
        ad_server_spec,
    };

    #[test]
    fn serve_ads_r1_returns_20_distinct_funded_campaigns_and_is_deterministic() {
        let mp = generate_marketplace(42);
        let mut server_a = AdServer::from_marketplace(&mp, AdServerConfig::default()).unwrap();
        let mut server_b = AdServer::from_marketplace(&mp, AdServerConfig::default()).unwrap();

        let resp_a = server_a.serve_ads("user-0001");
        let resp_b = server_b.serve_ads("user-0001");
        assert_eq!(resp_a, resp_b);
        assert_eq!(resp_a.user_id, "user-0001");
        assert_eq!(resp_a.strategy, RankerStrategy::R1);
        assert_eq!(resp_a.ads.len(), R1_SLATE_SIZE);

        let mut seen = HashSet::new();
        for (i, ad) in resp_a.ads.iter().enumerate() {
            assert_eq!(ad.slot_position, i + 1);
            assert!(seen.insert(ad.campaign_id.clone()));
            let camp = server_a.campaign(ad.campaign_index).unwrap();
            assert!(camp.remaining_budget_cents >= server_a.cpc_cents());
        }
    }

    #[test]
    fn exhausted_campaigns_retire_immediately_and_slate_shrinks_gracefully() {
        let mp = generate_marketplace(42);
        let mut server = AdServer::from_marketplace(
            &mp,
            AdServerConfig {
                seed: 42,
                campaign_budget_cents: Some(100),
                cpc_cents: 100,
                ranker: RankerStrategy::R1,
                slate_size: 20,
                ..AdServerConfig::default()
            },
        )
        .unwrap();

        assert_eq!(server.active_campaign_count(), 500);

        // Exhaust all 500 campaigns in 25 batches of 20 distinct campaigns.
        let mut retired = HashSet::new();
        for batch in 0..25 {
            let resp = server.serve_ads(&format!("user-{:04}", batch + 1));
            assert_eq!(resp.ads.len(), 20);
            for ad in &resp.ads {
                assert!(
                    !retired.contains(&ad.campaign_id),
                    "retired campaign {} was served again",
                    ad.campaign_id
                );
                let cost = server
                    .record_impression(1, ad.campaign_index, true)
                    .unwrap();
                assert_eq!(cost, Some(100));
                retired.insert(ad.campaign_id.clone());
            }
        }

        assert_eq!(retired.len(), 500);
        assert_eq!(server.active_campaign_count(), 0);
        assert!(!server.has_funded_campaigns());
        assert!(server.serve_ads("user-0001").ads.is_empty());
    }

    #[test]
    fn partial_pool_below_20_returns_exact_remaining_funded_campaigns() {
        let mp = generate_marketplace(42);
        let mut server = AdServer::from_marketplace(
            &mp,
            AdServerConfig {
                seed: 7,
                campaign_budget_cents: Some(100),
                cpc_cents: 100,
                ranker: RankerStrategy::R1,
                slate_size: 20,
                ..AdServerConfig::default()
            },
        )
        .unwrap();

        // Exhaust 493 campaigns, leaving exactly 7 active.
        for idx in 0..493 {
            server.record_impression(1, idx, true).unwrap();
        }
        assert_eq!(server.active_campaign_count(), 7);

        let resp = server.serve_ads("user-0042");
        assert_eq!(resp.ads.len(), 7);
        let ids: HashSet<_> = resp.ads.iter().map(|a| a.campaign_index).collect();
        assert_eq!(ids, (493..500).collect::<HashSet<_>>());
    }

    #[test]
    fn defensive_guard_filters_duplicates_and_unfunded_from_custom_ranker() {
        struct BadRanker;
        impl AdsRanker for BadRanker {
            fn strategy(&self) -> RankerStrategy {
                RankerStrategy::R1
            }
            fn slate_size(&self) -> usize {
                3
            }
            fn select_campaigns(
                &self,
                _user_id: &str,
                _eligible_indices: &[usize],
                _campaigns: &[CampaignState],
                _rng: &mut ChaCha8Rng,
            ) -> Vec<usize> {
                // Includes exhausted campaign 0, duplicate 1, out-of-bounds 9999, and valid 2, 3, 4.
                vec![0, 1, 1, 9999, 2, 3, 4]
            }
        }

        let mp = generate_marketplace(42);
        let mut server = AdServer::with_ranker(
            &mp,
            AdServerConfig {
                seed: 42,
                campaign_budget_cents: Some(100),
                cpc_cents: 100,
                ranker: RankerStrategy::R1,
                slate_size: 3,
                ..AdServerConfig::default()
            },
            Box::new(BadRanker),
        )
        .unwrap();
        server.record_impression(1, 0, true).unwrap();

        let resp = server.serve_ads("user-0001");
        let picked: Vec<_> = resp.ads.iter().map(|a| a.campaign_index).collect();
        assert_eq!(picked, vec![1, 2, 3]);
    }

    #[test]
    fn default_budgets_are_randomized_between_1000_and_10000_dollars() {
        let mp = generate_marketplace(42);
        let server_a = AdServer::from_marketplace(&mp, AdServerConfig::default()).unwrap();
        let server_b = AdServer::from_marketplace(&mp, AdServerConfig::default()).unwrap();
        let server_c = AdServer::from_marketplace(
            &mp,
            AdServerConfig {
                seed: 99,
                ..AdServerConfig::default()
            },
        )
        .unwrap();

        let budgets_a: Vec<u64> = server_a
            .campaigns()
            .iter()
            .map(|c| c.budget_cents)
            .collect();
        let budgets_b: Vec<u64> = server_b
            .campaigns()
            .iter()
            .map(|c| c.budget_cents)
            .collect();
        let budgets_c: Vec<u64> = server_c
            .campaigns()
            .iter()
            .map(|c| c.budget_cents)
            .collect();

        assert_eq!(budgets_a, budgets_b);
        assert_ne!(budgets_a, budgets_c);
        assert_eq!(budgets_a.len(), 500);
        for &b in &budgets_a {
            assert!(
                (crate::DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS
                    ..=crate::DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS)
                    .contains(&b),
                "campaign budget {b} out of default [$1,000, $10,000] range"
            );
            assert_eq!(b % 100, 0, "default budget should be whole dollars");
        }
        let distinct: HashSet<u64> = budgets_a.iter().copied().collect();
        assert!(
            distinct.len() > 100,
            "expected diverse randomized campaign budgets"
        );
        assert_eq!(server_a.total_budget_cents(), budgets_a.iter().sum::<u64>());

        let bad = AdServer::from_marketplace(
            &mp,
            AdServerConfig {
                campaign_budget_cents: None,
                campaign_budget_min_cents: 200_000,
                campaign_budget_max_cents: 100_000,
                ..AdServerConfig::default()
            },
        );
        assert!(bad.is_err(), "min > max budget range must be rejected");
    }

    #[test]
    fn p1_even_daily_pacing_caps_day1_spend_and_resumes_on_day2() {
        let mut mp = generate_marketplace(42);
        mp.advertisers.truncate(1); // 10 campaigns
        let mut server = AdServer::from_marketplace(
            &mp,
            AdServerConfig {
                seed: 42,
                campaign_budget_cents: Some(3_000), // $30 total => $1/day allowance
                cpc_cents: 100,
                ranker: RankerStrategy::R1,
                scoring: crate::ScoringStrategy::S0,
                pacing: crate::PacingStrategy::P1,
                slate_size: 10,
                ..AdServerConfig::default()
            },
        )
        .unwrap();

        let resp1 = server.serve_ads_for_day(1, "user-0001").unwrap();
        assert_eq!(resp1.ads.len(), 10);
        assert_eq!(resp1.pacing, crate::PacingStrategy::P1);
        for ad in &resp1.ads {
            server
                .record_impression(1, ad.campaign_index, true)
                .unwrap();
        }
        // All 10 campaigns hit Day 1 cap ($1.00 = 100 cents), so Day 1 serves 0 more ads.
        let resp1_after = server.serve_ads_for_day(1, "user-0001").unwrap();
        assert!(resp1_after.ads.is_empty());

        // Day 2 unlocks the next $1.00 allowance for all 10 campaigns.
        let resp2 = server.serve_ads_for_day(2, "user-0001").unwrap();
        assert_eq!(resp2.ads.len(), 10);
    }

    #[test]
    fn p2_smooth_pacing_throttles_spent_campaigns_and_unlocks_on_day30() {
        let mut mp = generate_marketplace(42);
        mp.advertisers.truncate(1); // 10 campaigns
        let mut server = AdServer::from_marketplace(
            &mp,
            AdServerConfig {
                seed: 42,
                campaign_budget_cents: Some(3_000), // $30 total => $1/day target on Day 1
                cpc_cents: 100,
                ranker: RankerStrategy::R2,
                scoring: crate::ScoringStrategy::S1,
                pacing: crate::PacingStrategy::P2,
                slate_size: 10,
                ..AdServerConfig::default()
            },
        )
        .unwrap();

        // Spend $1 (100% of Day 1 daily target) on all 10 campaigns.
        for idx in 0..10 {
            server.record_impression(1, idx, true).unwrap();
        }
        assert!(
            server
                .serve_ads_for_day(1, "user-0001")
                .unwrap()
                .ads
                .is_empty()
        );
        // Day 30 unlocks all remaining budget regardless of daily spend.
        let day30 = server.serve_ads_for_day(30, "user-0001").unwrap();
        assert_eq!(day30.ads.len(), 10);
    }

    #[test]
    fn rejects_incompatible_r1_with_non_uniform_scoring() {
        let mp = generate_marketplace(42);
        let err = AdServer::from_marketplace(
            &mp,
            AdServerConfig {
                ranker: RankerStrategy::R1,
                scoring: crate::ScoringStrategy::S1,
                ..AdServerConfig::default()
            },
        );
        assert!(err.is_err());
    }

    #[test]
    fn pacing_1_intraday_unlocks_at_pst_second() {
        let mut mp = generate_marketplace(42);
        mp.advertisers.truncate(1); // 10 campaigns
        let mut server = AdServer::from_marketplace(
            &mp,
            AdServerConfig {
                seed: 42,
                campaign_budget_cents: Some(9_000), // $90 total => $3/day => unlocks at 00:00, 08:00, 16:00 PST
                cpc_cents: 100,
                retrieval: crate::RetrievalStrategy::RetrieveV0,
                ranker: RankerStrategy::R1,
                scoring: crate::ScoringStrategy::S0,
                pacing: crate::PacingStrategy::Pacing1,
                bidding: crate::BiddingStrategy::Basebid,
                slate_size: 10,
                ..AdServerConfig::default()
            },
        )
        .unwrap();

        server.set_day(1).unwrap();
        server.set_second_of_day(0).unwrap();
        let r0 = server.serve_ads("user-0001");
        assert_eq!(r0.ads.len(), 10);
        assert_eq!(r0.retrieval, crate::RetrievalStrategy::RetrieveV0);
        assert_eq!(r0.pacing, crate::PacingStrategy::Pacing1);
        for ad in &r0.ads {
            server
                .record_impression(1, ad.campaign_index, true)
                .unwrap();
        }
        // At 07:59:59 PST (28_799s), all 10 campaigns are still throttled.
        server.set_second_of_day(28_799).unwrap();
        assert!(server.serve_ads("user-0001").ads.is_empty());
        // At 08:00:00 PST (28_800s), all 10 unlock!
        server.set_second_of_day(28_800).unwrap();
        assert_eq!(server.serve_ads("user-0001").ads.len(), 10);
    }

    #[test]
    fn spec_matches_constants_and_round_trips_json() {
        let spec = ad_server_spec();
        assert_eq!(spec.crate_name, "ad-server");
        assert_eq!(spec.default_min_campaign_budget_cents, 100_000);
        assert_eq!(spec.default_max_campaign_budget_cents, 1_000_000);
        assert_eq!(spec.default_retrieval, crate::RetrievalStrategy::RetrieveV0);
        assert_eq!(spec.default_ranker, RankerStrategy::R1);
        assert_eq!(spec.default_scoring, crate::ScoringStrategy::S0);
        assert_eq!(spec.default_pacing, crate::PacingStrategy::P0);
        assert_eq!(spec.default_bidding, crate::BiddingStrategy::Basebid);
        assert_eq!(spec.default_slate_size, 20);
        assert_eq!(spec.supported_retrievers.len(), 1);
        assert_eq!(spec.supported_rankers.len(), 3);
        assert_eq!(spec.supported_scorers.len(), 6);
        assert_eq!(spec.supported_pacers.len(), 4);
        assert_eq!(spec.supported_bidders.len(), 2);
        let json = serde_json::to_string(&spec).unwrap();
        let parsed: crate::AdServerSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, spec);
    }

    #[test]
    fn visit_generator_drives_ad_server_in_natural_and_targeted_modes() {
        let mp = generate_marketplace(42);
        let user_ids: Vec<String> = mp.users.iter().map(|u| u.id.clone()).collect();
        let category_ids: Vec<String> = mp.categories.iter().map(|c| c.id.clone()).collect();

        let mut ad_server = AdServer::from_marketplace(
            &mp,
            AdServerConfig {
                seed: 42,
                campaign_budget_cents: Some(100),
                cpc_cents: 100,
                ranker: RankerStrategy::R1,
                slate_size: 20,
                ..AdServerConfig::default()
            },
        )
        .unwrap();
        let mut generator = VisitGenerator::new(
            user_ids.clone(),
            category_ids.clone(),
            VisitGeneratorConfig {
                seed: 42,
                preference_seed: None,
                days: 30,
                daily_requests: DEFAULT_DAILY_REQUESTS,
                target_impressions: Some(1_000),
                max_click_probability: 0.0,
                ..VisitGeneratorConfig::default()
            },
        )
        .unwrap();

        let mut seen_req1 = HashSet::new();
        let day1 = generator
            .run_day(1, &mut ad_server, |imp| {
                assert!(!imp.clicked);
                if imp.request_id == 1 {
                    assert!(seen_req1.insert(imp.ad.campaign_id.clone()));
                }
            })
            .unwrap();
        assert_eq!(day1.impressions, 34);
        assert_eq!(day1.ad_requests, 2);
        assert_eq!(seen_req1.len(), 20);

        // Unaffordable budget cannot meet a target.
        let mut broke_server = AdServer::from_marketplace(
            &mp,
            AdServerConfig {
                seed: 42,
                campaign_budget_cents: Some(50),
                cpc_cents: 100,
                ranker: RankerStrategy::R1,
                slate_size: 20,
                ..AdServerConfig::default()
            },
        )
        .unwrap();
        let mut gen2 = VisitGenerator::new(
            user_ids,
            category_ids,
            VisitGeneratorConfig {
                seed: 42,
                preference_seed: None,
                days: 30,
                daily_requests: DEFAULT_DAILY_REQUESTS,
                target_impressions: Some(30),
                ..VisitGeneratorConfig::default()
            },
        )
        .unwrap();
        let err = gen2.run_day(1, &mut broke_server, |_| {}).unwrap_err();
        assert!(err.to_string().contains("cannot be reached on day 1"));
    }
}
