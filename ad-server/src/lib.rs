//! Standalone Ad-Server module.
//!
//! Accepts a `user_id` via [`AdServer::serve_ads`] and returns a set of ads from distinct
//! funded campaigns selected by a pluggable [`AdsRanker`] strategy (starting with
//! [`RankerStrategy::R1`], which selects up to 20 random campaigns with remaining budget).

pub mod ranker;

use std::collections::HashSet;
use std::io::{Error, ErrorKind};

use ads_simulator::{Item, Marketplace};
use rand::seq::IndexedRandom;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};

pub use ranker::{
    AdsRanker, R1_SLATE_SIZE, R1RandomBudgetedRanker, R2_EPSILON, R2_SLATE_SIZE,
    R2_UCB_EXPLORATION_C, R2OnlineCtrRanker, RankerStrategy,
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
    pub default_ranker: RankerStrategy,
    pub default_slate_size: usize,
    pub supported_rankers: Vec<RankerInfo>,
}

/// Metadata describing a registered ranker strategy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RankerInfo {
    pub id: String,
    pub description: String,
    pub default_slate_size: usize,
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
        default_ranker: RankerStrategy::R1,
        default_slate_size: R1_SLATE_SIZE,
        supported_rankers: RankerStrategy::all()
            .iter()
            .map(|&strategy| RankerInfo {
                id: strategy.as_str().to_owned(),
                description: strategy.description().to_owned(),
                default_slate_size: strategy.default_slate_size(),
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
    pub ranker: RankerStrategy,
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
}

impl Default for AdServerConfig {
    fn default() -> Self {
        Self {
            seed: DEFAULT_SEED,
            campaign_budget_cents: None,
            campaign_budget_min_cents: DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS,
            campaign_budget_max_cents: DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS,
            cpc_cents: DEFAULT_CPC_CENTS,
            ranker: RankerStrategy::R1,
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
    pub strategy: RankerStrategy,
    pub ads: Vec<ServedAd>,
}

/// Stateful ad-server owning campaign budgets, the O(1) active funded pool, and a pluggable
/// [`AdsRanker`] strategy.
pub struct AdServer {
    config: AdServerConfig,
    campaigns: Vec<CampaignState>,
    active_indices: Vec<usize>,
    active_pos: Vec<Option<usize>>,
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
    /// strategy specified in `config`.
    pub fn from_marketplace(
        marketplace: &Marketplace,
        config: AdServerConfig,
    ) -> Result<Self, Error> {
        let ranker = config.ranker.build(config.slate_size);
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

        let mut active_indices = Vec::with_capacity(campaigns.len());
        let mut active_pos = vec![None; campaigns.len()];
        for (idx, campaign) in campaigns.iter().enumerate() {
            if campaign.has_budget(config.cpc_cents) {
                active_pos[idx] = Some(active_indices.len());
                active_indices.push(idx);
            }
        }

        Ok(Self {
            config,
            campaigns,
            active_indices,
            active_pos,
            total_budget_cents,
            ranker,
            rng,
        })
    }

    /// Serve a set of ads from distinct funded campaigns for `user_id`.
    ///
    /// Guarantees:
    /// - Every returned ad belongs to a distinct campaign.
    /// - Every returned ad's campaign currently has `remaining_budget_cents >= cpc_cents`.
    /// - Returns at most `slate_size()` ads (20 by default under Strategy R1), or fewer if fewer
    ///   campaigns currently have sufficient budget.
    pub fn serve_ads(&mut self, user_id: &str) -> AdResponse {
        let strategy = self.ranker.strategy();
        if self.active_indices.is_empty() {
            return AdResponse {
                user_id: user_id.to_owned(),
                strategy,
                ads: Vec::new(),
            };
        }

        let max_ads = self.ranker.slate_size();
        let selected = self.ranker.select_campaigns(
            user_id,
            &self.active_indices,
            &self.campaigns,
            &mut self.rng,
        );

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
            strategy,
            ads,
        }
    }

    /// Record a delivered impression (and optional click) against `campaign_index` on `day` (1..=30).
    ///
    /// When `clicked` is `true`, deducts `cpc_cents` from remaining budget and immediately retires
    /// the campaign from the active funded pool if it can no longer afford another click.
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

        let exhausted = campaign.remaining_budget_cents < cpc;
        campaign.totals.record(cost);
        campaign.daily[day - 1].activity.record(cost);

        if exhausted {
            self.retire_campaign(campaign_index);
        }

        Ok(cost)
    }

    fn retire_campaign(&mut self, campaign_index: usize) {
        if let Some(pos) = self.active_pos[campaign_index].take() {
            let last_campaign = *self
                .active_indices
                .last()
                .expect("active_pos points into non-empty active_indices");
            self.active_indices.swap_remove(pos);
            if pos < self.active_indices.len() {
                self.active_pos[last_campaign] = Some(pos);
            }
        }
    }

    /// True if at least one campaign can currently afford a click.
    pub fn has_funded_campaigns(&self) -> bool {
        !self.active_indices.is_empty()
    }

    /// Number of campaigns that currently have remaining budget >= CPC.
    pub fn active_campaign_count(&self) -> usize {
        self.active_indices.len()
    }

    /// Configured CPC in USD cents.
    pub fn cpc_cents(&self) -> u64 {
        self.config.cpc_cents
    }

    /// Active ranker strategy.
    pub fn strategy(&self) -> RankerStrategy {
        self.ranker.strategy()
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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use ads_simulator::generate_marketplace;
    use rand_chacha::ChaCha8Rng;

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

        let budgets_a: Vec<u64> = server_a.campaigns().iter().map(|c| c.budget_cents).collect();
        let budgets_b: Vec<u64> = server_b.campaigns().iter().map(|c| c.budget_cents).collect();
        let budgets_c: Vec<u64> = server_c.campaigns().iter().map(|c| c.budget_cents).collect();

        assert_eq!(budgets_a, budgets_b);
        assert_ne!(budgets_a, budgets_c);
        assert_eq!(budgets_a.len(), 500);
        for &b in &budgets_a {
            assert!(
                (crate::DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS..=crate::DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS)
                    .contains(&b),
                "campaign budget {b} out of default [$1,000, $10,000] range"
            );
            assert_eq!(b % 100, 0, "default budget should be whole dollars");
        }
        let distinct: HashSet<u64> = budgets_a.iter().copied().collect();
        assert!(distinct.len() > 100, "expected diverse randomized campaign budgets");
        assert_eq!(
            server_a.total_budget_cents(),
            budgets_a.iter().sum::<u64>()
        );

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
    fn spec_matches_constants_and_round_trips_json() {
        let spec = ad_server_spec();
        assert_eq!(spec.crate_name, "ad-server");
        assert_eq!(spec.default_min_campaign_budget_cents, 100_000);
        assert_eq!(spec.default_max_campaign_budget_cents, 1_000_000);
        assert_eq!(spec.default_ranker, RankerStrategy::R1);
        assert_eq!(spec.default_slate_size, 20);
        assert_eq!(spec.supported_rankers.len(), 2);
        let json = serde_json::to_string(&spec).unwrap();
        let parsed: crate::AdServerSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, spec);
    }
}
