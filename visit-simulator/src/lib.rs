//! Visit simulator, ad slot generator, and visitor click oracle.
//!
//! This module models how a user/visitor behaves in the marketplace:
//! 1. **Visit arrival & ad slot generation (`RNG_STREAM`)**: samples visiting users uniformly
//!    without depending on preferences and issues `AdServer::serve_ads(user_id)` requests,
//!    preserving `AdServer`'s user-ID-only serving boundary.
//! 2. **Visitor click oracle (`ClickOracle`, `CLICK_RNG_STREAM`)**: models whether the visiting
//!    user clicks each served ad in the returned slot (`MAX_CLICK_PROBABILITY` = 4% if the ad's
//!    category is among the user's preferred categories, 0% otherwise) and delegates CPC billing
//!    and campaign retirement to `AdServer::record_impression`.

use std::collections::HashMap;
use std::io::{Error, ErrorKind};

use ad_server::{AdResponse, AdServer, ServedAd};
use ads_simulator::{Item, User, score_preference};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};

pub const DEFAULT_DAYS: usize = ad_server::DAYS;
/// Default number of ad-server requests issued per day when no impression target is specified.
/// With R1's 20-ad slate, 600 requests/day produces 12,000 impressions/day (360,000 over 30 days).
pub const DEFAULT_DAILY_REQUESTS: usize = 600;
pub const DEFAULT_SEED: u64 = 42;
/// ChaCha8 RNG stream for preference-independent visit arrivals and diurnal timestamps.
pub const RNG_STREAM: u64 = 1;
/// ChaCha8 RNG stream for the visitor [`ClickOracle`].
pub const CLICK_RNG_STREAM: u64 = 2;
/// Click probability when a served ad matches one of the visiting user's preferred categories.
pub const MAX_CLICK_PROBABILITY: f64 = 0.04;

/// Compiled specification of the `visit-simulator` crate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VisitSimulatorSpec {
    pub crate_name: String,
    pub binary_name: String,
    pub version: String,
    pub default_days: usize,
    pub default_daily_requests: usize,
    pub default_seed: u64,
    pub rng_stream: u64,
    pub click_rng_stream: u64,
    pub max_click_probability: f64,
    pub user_independent_arrivals: bool,
}

/// Return the live compiled specification of the `visit-simulator` crate.
pub fn visit_simulator_spec() -> VisitSimulatorSpec {
    VisitSimulatorSpec {
        crate_name: env!("CARGO_PKG_NAME").to_owned(),
        binary_name: "visit-simulator".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        default_days: DEFAULT_DAYS,
        default_daily_requests: DEFAULT_DAILY_REQUESTS,
        default_seed: DEFAULT_SEED,
        rng_stream: RNG_STREAM,
        click_rng_stream: CLICK_RNG_STREAM,
        max_click_probability: MAX_CLICK_PROBABILITY,
        user_independent_arrivals: true,
    }
}

/// Configuration for [`VisitSimulator`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct VisitSimulatorConfig {
    pub seed: u64,
    pub days: usize,
    pub daily_requests: usize,
    pub target_impressions: Option<u64>,
    pub max_click_probability: f64,
}

impl Default for VisitSimulatorConfig {
    fn default() -> Self {
        Self {
            seed: DEFAULT_SEED,
            days: DEFAULT_DAYS,
            daily_requests: DEFAULT_DAILY_REQUESTS,
            target_impressions: None,
            max_click_probability: MAX_CLICK_PROBABILITY,
        }
    }
}

/// A single ad impression delivered from an [`AdServer`] response slate along with the visitor's
/// click outcome from [`ClickOracle`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedImpression {
    pub day: usize,
    pub request_id: u64,
    pub slot_position: usize,
    pub user_id: String,
    pub ad: ServedAd,
    pub clicked: bool,
}

/// Aggregate counters produced by running one day of visit simulation, slot generation, and
/// visitor click evaluation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayGenerationStats {
    pub day: usize,
    pub ad_requests: u64,
    pub opportunities: u64,
    pub insufficient_budget: u64,
    pub impressions: u64,
    pub clicks: u64,
    pub spend_cents: u64,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message.into())
}

/// Visitor click oracle modeling whether a visiting shopper clicks a served ad.
///
/// Uses a dedicated ChaCha8 RNG stream (`CLICK_RNG_STREAM`) independent from visit arrivals
/// (`RNG_STREAM`) and ad-server ranking (Stream 0). An ad in one of the user's preferred
/// categories clicks with probability `max_click_probability` (default `4%`), and `0%` otherwise.
pub struct ClickOracle {
    users: Vec<User>,
    user_index: HashMap<String, usize>,
    max_click_probability: f64,
    rng: ChaCha8Rng,
}

impl ClickOracle {
    /// Construct a new [`ClickOracle`] with [`MAX_CLICK_PROBABILITY`] (`4%` preferred / `0%` else).
    pub fn new(users: Vec<User>, seed: u64) -> Result<Self, Error> {
        Self::with_max_click_probability(users, seed, MAX_CLICK_PROBABILITY)
    }

    /// Construct a new [`ClickOracle`] with an explicit preferred-category click probability.
    pub fn with_max_click_probability(
        users: Vec<User>,
        seed: u64,
        max_click_probability: f64,
    ) -> Result<Self, Error> {
        if !max_click_probability.is_finite() || !(0.0..=1.0).contains(&max_click_probability) {
            return Err(invalid(
                "max_click_probability must be finite and within [0.0, 1.0]",
            ));
        }

        let mut user_index = HashMap::with_capacity(users.len());
        for (idx, user) in users.iter().enumerate() {
            if user_index.insert(user.id.clone(), idx).is_some() {
                return Err(invalid(format!("duplicate user ID {}", user.id)));
            }
        }

        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        rng.set_stream(CLICK_RNG_STREAM);

        Ok(Self {
            users,
            user_index,
            max_click_probability,
            rng,
        })
    }

    /// Configured click probability for ads in a user's preferred category.
    pub fn max_click_probability(&self) -> f64 {
        self.max_click_probability
    }

    /// Deterministic click probability for `user_id` on an ad in `category_id`.
    pub fn click_probability(&self, user_id: &str, category_id: &str) -> Result<f64, Error> {
        let &idx = self
            .user_index
            .get(user_id)
            .ok_or_else(|| invalid(format!("unknown user ID {user_id}")))?;
        Ok(self.click_probability_for_index(idx, category_id))
    }

    /// Sample whether `user_id` clicks `ad` using this oracle's isolated click RNG stream.
    pub fn evaluate_click(&mut self, user_id: &str, ad: &ServedAd) -> Result<bool, Error> {
        let &idx = self
            .user_index
            .get(user_id)
            .ok_or_else(|| invalid(format!("unknown user ID {user_id}")))?;
        Ok(self.evaluate_click_for_index(idx, &ad.category_id))
    }

    /// Sample whether `user_id` clicks catalog `item` via [`ads_simulator::score_preference`].
    pub fn evaluate_item_click(&mut self, user_id: &str, item: &Item) -> Result<bool, Error> {
        let &idx = self
            .user_index
            .get(user_id)
            .ok_or_else(|| invalid(format!("unknown user ID {user_id}")))?;
        let score = score_preference(&self.users[idx], item);
        Ok(self.rng.random_bool(self.max_click_probability * score))
    }

    fn user_count(&self) -> usize {
        self.users.len()
    }

    fn user_id(&self, idx: usize) -> &str {
        self.users[idx].id.as_str()
    }

    fn click_probability_for_index(&self, idx: usize, category_id: &str) -> f64 {
        if self.users[idx]
            .preferred_category_ids
            .iter()
            .any(|preferred| preferred == category_id)
        {
            self.max_click_probability
        } else {
            0.0
        }
    }

    fn evaluate_click_for_index(&mut self, idx: usize, category_id: &str) -> bool {
        let prob = self.click_probability_for_index(idx, category_id);
        self.rng.random_bool(prob)
    }
}

/// Standalone visit simulator, ad slot generator, and visitor click oracle driving [`AdServer`].
pub struct VisitSimulator {
    click_oracle: ClickOracle,
    config: VisitSimulatorConfig,
    rng: ChaCha8Rng,
    next_request_id: u64,
    total_impressions_delivered: u64,
}

impl VisitSimulator {
    /// Construct a new [`VisitSimulator`] from marketplace [`User`] profiles.
    ///
    /// Visit arrivals remain preference-independent on `RNG_STREAM` and send only `user_id` to
    /// [`AdServer::serve_ads`]. Visitor click decisions use the internal [`ClickOracle`] on
    /// `CLICK_RNG_STREAM`.
    pub fn new(users: Vec<User>, config: VisitSimulatorConfig) -> Result<Self, Error> {
        if config.days == 0 {
            return Err(invalid("days must be positive"));
        }
        let click_oracle = ClickOracle::with_max_click_probability(
            users,
            config.seed,
            config.max_click_probability,
        )?;

        let mut rng = ChaCha8Rng::seed_from_u64(config.seed);
        rng.set_stream(RNG_STREAM);

        Ok(Self {
            click_oracle,
            config,
            rng,
            next_request_id: 1,
            total_impressions_delivered: 0,
        })
    }

    /// Borrow the underlying visitor [`ClickOracle`].
    pub fn click_oracle(&self) -> &ClickOracle {
        &self.click_oracle
    }

    /// Mutably borrow the underlying visitor [`ClickOracle`].
    pub fn click_oracle_mut(&mut self) -> &mut ClickOracle {
        &mut self.click_oracle
    }

    /// Return the daily impression quota for 1-based `day` when `target_impressions` is set.
    pub fn daily_quota(&self, day: usize) -> Option<u64> {
        self.config.target_impressions.map(|target| {
            let days = self.config.days as u64;
            target / days + u64::from(day as u64 <= target % days)
        })
    }

    fn generate_request_indexed(
        &mut self,
        ad_server: &mut AdServer,
    ) -> Result<(u64, usize, AdResponse), Error> {
        let user_count = self.click_oracle.user_count();
        if user_count == 0 {
            return Err(invalid("cannot generate ad request with zero users"));
        }
        let user_index = self.rng.random_range(0..user_count);
        let user_id = self.click_oracle.user_id(user_index).to_owned();
        let request_id = self.next_request_id;
        self.next_request_id += 1;
        let response = ad_server.serve_ads(&user_id);
        Ok((request_id, user_index, response))
    }

    /// Issue a single preference-independent ad request to `ad_server` and return `(request_id, response)`.
    pub fn generate_request(
        &mut self,
        ad_server: &mut AdServer,
    ) -> Result<(u64, AdResponse), Error> {
        let (request_id, _, response) = self.generate_request_indexed(ad_server)?;
        Ok((request_id, response))
    }

    /// Execute one simulated day (1-based `day`), simulating user visits, querying
    /// `ad_server.serve_ads(user_id)`, evaluating visitor clicks via [`ClickOracle`], recording
    /// outcomes back to `ad_server.record_impression`, and notifying `on_impression` for each
    /// delivered impression.
    pub fn run_day<F>(
        &mut self,
        day: usize,
        ad_server: &mut AdServer,
        mut on_impression: F,
    ) -> Result<DayGenerationStats, Error>
    where
        F: FnMut(&GeneratedImpression),
    {
        if day == 0 || day > self.config.days {
            return Err(invalid(format!(
                "day {day} out of range 1..={}",
                self.config.days
            )));
        }
        ad_server.set_day(day)?;
        let use_pst_diurnal = ad_server.pacing() == ad_server::PacingStrategy::Pacing1;
        if !use_pst_diurnal {
            ad_server.set_second_of_day(0)?;
        }

        let quota = self.daily_quota(day);
        let mut stats = DayGenerationStats {
            day,
            ..DayGenerationStats::default()
        };

        match quota {
            None => {
                if self.click_oracle.user_count() == 0 {
                    return Ok(stats);
                }
                let pst_seconds: Vec<u32> = if use_pst_diurnal {
                    let mut v: Vec<u32> = (0..self.config.daily_requests)
                        .map(|_| ad_server::sample_pst_second_of_day(&mut self.rng))
                        .collect();
                    v.sort_unstable();
                    v
                } else {
                    Vec::new()
                };
                for req_idx in 0..self.config.daily_requests {
                    if let Some(&sec) = pst_seconds.get(req_idx) {
                        ad_server.set_second_of_day(sec)?;
                    }
                    let (request_id, user_index, response) =
                        self.generate_request_indexed(ad_server)?;
                    stats.ad_requests += 1;
                    if response.ads.is_empty() {
                        stats.opportunities += 1;
                        stats.insufficient_budget += 1;
                        continue;
                    }
                    for ad in response.ads {
                        stats.opportunities += 1;
                        let clicked = self
                            .click_oracle
                            .evaluate_click_for_index(user_index, &ad.category_id);
                        let cost =
                            ad_server.record_impression(day, ad.campaign_index, clicked)?;
                        let impression = GeneratedImpression {
                            day,
                            request_id,
                            slot_position: ad.slot_position,
                            user_id: response.user_id.clone(),
                            ad,
                            clicked,
                        };
                        on_impression(&impression);
                        stats.impressions += 1;
                        self.total_impressions_delivered += 1;
                        if let Some(cost) = cost {
                            stats.clicks += 1;
                            stats.spend_cents += cost;
                        }
                    }
                }
                let _ = pst_seconds.len();
            }
            Some(target_for_day) => {
                while stats.impressions < target_for_day {
                    if self.click_oracle.user_count() == 0 {
                        return Err(invalid(format!(
                            "impression target cannot be reached on day {day}: no users available ({} impressions delivered)",
                            self.total_impressions_delivered
                        )));
                    }
                    if use_pst_diurnal && target_for_day > 0 {
                        let frac = (stats.impressions as f64) / (target_for_day as f64);
                        let sec = ((frac * f64::from(ad_server::SECONDS_PER_DAY)).floor() as u32)
                            .min(ad_server::SECONDS_PER_DAY - 1);
                        ad_server.set_second_of_day(sec)?;
                    }
                    let (request_id, user_index, response) =
                        self.generate_request_indexed(ad_server)?;
                    stats.ad_requests += 1;
                    if response.ads.is_empty() {
                        return Err(invalid(format!(
                            "impression target cannot be reached on day {day}: no campaigns that can afford the CPC ({} impressions delivered)",
                            self.total_impressions_delivered
                        )));
                    }
                    for ad in response.ads {
                        if stats.impressions >= target_for_day {
                            break;
                        }
                        stats.opportunities += 1;
                        let clicked = self
                            .click_oracle
                            .evaluate_click_for_index(user_index, &ad.category_id);
                        let cost =
                            ad_server.record_impression(day, ad.campaign_index, clicked)?;
                        let impression = GeneratedImpression {
                            day,
                            request_id,
                            slot_position: ad.slot_position,
                            user_id: response.user_id.clone(),
                            ad,
                            clicked,
                        };
                        on_impression(&impression);
                        stats.impressions += 1;
                        self.total_impressions_delivered += 1;
                        if let Some(cost) = cost {
                            stats.clicks += 1;
                            stats.spend_cents += cost;
                        }
                    }
                }
            }
        }

        Ok(stats)
    }

    /// Total impressions delivered across all days run so far.
    pub fn total_impressions_delivered(&self) -> u64 {
        self.total_impressions_delivered
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use ad_server::{AdServer, AdServerConfig, RankerStrategy, ServedAd};
    use ads_simulator::{User, generate_marketplace};

    use crate::{
        CLICK_RNG_STREAM, ClickOracle, DEFAULT_DAILY_REQUESTS, MAX_CLICK_PROBABILITY, RNG_STREAM,
        VisitSimulator, VisitSimulatorConfig, visit_simulator_spec,
    };

    #[test]
    fn natural_mode_issues_600_requests_per_day_and_12000_impressions_with_r1() {
        let mp = generate_marketplace(42);
        let mut ad_server = AdServer::from_marketplace(&mp, AdServerConfig::default()).unwrap();
        let mut simulator = VisitSimulator::new(
            mp.users,
            VisitSimulatorConfig {
                max_click_probability: 0.0,
                ..VisitSimulatorConfig::default()
            },
        )
        .unwrap();

        let day1 = simulator.run_day(1, &mut ad_server, |_| {}).unwrap();
        assert_eq!(day1.day, 1);
        assert_eq!(day1.ad_requests, DEFAULT_DAILY_REQUESTS as u64);
        assert_eq!(day1.impressions, 12_000);
        assert_eq!(day1.opportunities, 12_000);
        assert_eq!(day1.insufficient_budget, 0);
        assert_eq!(day1.clicks, 0);
        assert_eq!(day1.spend_cents, 0);
    }

    #[test]
    fn click_oracle_clicks_only_preferred_categories_and_is_deterministic() {
        let users = vec![User {
            id: "user-0001".to_owned(),
            preferred_category_ids: vec!["category-001".to_owned()],
        }];
        let preferred_ad = ServedAd {
            slot_position: 1,
            campaign_index: 0,
            campaign_id: "campaign-01".to_owned(),
            advertiser_id: "advertiser-01".to_owned(),
            ad_item_id: "catalog-0001".to_owned(),
            category_id: "category-001".to_owned(),
            cpc_cents: 100,
        };
        let non_preferred_ad = ServedAd {
            category_id: "category-099".to_owned(),
            ..preferred_ad.clone()
        };

        let mut oracle_a = ClickOracle::new(users.clone(), 42).unwrap();
        let mut oracle_b = ClickOracle::new(users, 42).unwrap();
        assert_eq!(
            oracle_a.click_probability("user-0001", "category-001").unwrap(),
            MAX_CLICK_PROBABILITY
        );
        assert_eq!(
            oracle_a.click_probability("user-0001", "category-099").unwrap(),
            0.0
        );

        let mut pref_clicks_a = 0u64;
        let mut pref_clicks_b = 0u64;
        for _ in 0..5_000 {
            assert!(!oracle_a.evaluate_click("user-0001", &non_preferred_ad).unwrap());
            assert!(!oracle_b.evaluate_click("user-0001", &non_preferred_ad).unwrap());
            let ca = oracle_a.evaluate_click("user-0001", &preferred_ad).unwrap();
            let cb = oracle_b.evaluate_click("user-0001", &preferred_ad).unwrap();
            assert_eq!(ca, cb);
            if ca {
                pref_clicks_a += 1;
            }
            if cb {
                pref_clicks_b += 1;
            }
        }
        assert_eq!(pref_clicks_a, pref_clicks_b);
        assert!(
            (130..=270).contains(&pref_clicks_a),
            "expected ~200 clicks at 4% over 5,000 draws, got {pref_clicks_a}"
        );
    }

    #[test]
    fn targeted_mode_hits_exact_daily_quota_and_errors_when_budget_exhausts() {
        let mp = generate_marketplace(42);
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
        let mut simulator = VisitSimulator::new(
            mp.users.clone(),
            VisitSimulatorConfig {
                seed: 42,
                days: 30,
                daily_requests: DEFAULT_DAILY_REQUESTS,
                target_impressions: Some(1_000),
                max_click_probability: 0.0,
            },
        )
        .unwrap();

        // Day 1 quota = 34 -> requires 2 requests (20 + 14 delivered).
        let mut seen_req1 = HashSet::new();
        let day1 = simulator
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
        let mut sim2 = VisitSimulator::new(
            mp.users,
            VisitSimulatorConfig {
                seed: 42,
                days: 30,
                daily_requests: DEFAULT_DAILY_REQUESTS,
                target_impressions: Some(30),
                ..VisitSimulatorConfig::default()
            },
        )
        .unwrap();
        let err = sim2.run_day(1, &mut broke_server, |_| {}).unwrap_err();
        assert!(err.to_string().contains("cannot be reached on day 1"));
    }

    #[test]
    fn spec_reports_visit_and_click_oracle_constants() {
        let spec = visit_simulator_spec();
        assert!(spec.user_independent_arrivals);
        assert_eq!(spec.rng_stream, RNG_STREAM);
        assert_eq!(spec.click_rng_stream, CLICK_RNG_STREAM);
        assert_eq!(spec.max_click_probability, MAX_CLICK_PROBABILITY);
        assert_eq!(spec.default_daily_requests, 600);
        assert_eq!(spec.default_days, 30);
    }
}
