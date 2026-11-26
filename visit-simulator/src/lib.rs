//! Visit simulator, user preferred categories, ad slot generator, and visitor click oracle.
//!
//! This module models how a user/visitor behaves in the marketplace:
//! 1. **User preferred categories (`INTEREST_COUNT`, `PREFERENCE_RNG_STREAM`)**: assigns each
//!    visitor a unique set of preferred product categories (`UserPreferenceProfile`) and exposes
//!    [`score_preference`].
//! 2. **Visit arrival & ad slot generation (`RNG_STREAM`)**: samples visiting users uniformly
//!    without depending on preferences and issues `AdServer::serve_ads(user_id)` requests,
//!    preserving `AdServer`'s user-ID-only serving boundary.
//! 3. **Visitor click oracle (`ClickOracle`, `CLICK_RNG_STREAM`)**: models whether the visiting
//!    user clicks each served ad in the returned slot (`MAX_CLICK_PROBABILITY` = 4% if the ad's
//!    category is among the user's preferred categories, 0% otherwise) and delegates CPC billing
//!    and campaign retirement to `AdServer::record_impression`.

use std::collections::{HashMap, HashSet};
use std::io::{Error, ErrorKind};

use ad_server::{AdResponse, AdServer, ServedAd};
use ads_simulator::Item;
use rand::seq::index;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};

pub const DEFAULT_DAYS: usize = ad_server::DAYS;
/// Default number of ad-server requests issued per day when no impression target is specified.
/// With R1's 20-ad slate, 600 requests/day produces 12,000 impressions/day (360,000 over 30 days).
pub const DEFAULT_DAILY_REQUESTS: usize = 600;
pub const DEFAULT_SEED: u64 = 42;
/// Number of distinct preferred categories assigned to each visitor.
pub const INTEREST_COUNT: usize = 5;
/// ChaCha8 RNG stream for preference-independent visit arrivals and diurnal timestamps.
pub const RNG_STREAM: u64 = 1;
/// ChaCha8 RNG stream for the visitor [`ClickOracle`].
pub const CLICK_RNG_STREAM: u64 = 2;
/// ChaCha8 RNG stream for generating visitor preferred categories.
pub const PREFERENCE_RNG_STREAM: u64 = 3;
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
    pub interest_count: usize,
    pub rng_stream: u64,
    pub click_rng_stream: u64,
    pub preference_rng_stream: u64,
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
        interest_count: INTEREST_COUNT,
        rng_stream: RNG_STREAM,
        click_rng_stream: CLICK_RNG_STREAM,
        preference_rng_stream: PREFERENCE_RNG_STREAM,
        max_click_probability: MAX_CLICK_PROBABILITY,
        user_independent_arrivals: true,
    }
}

/// A visitor's preferred product categories owned by `visit-simulator`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserPreferenceProfile {
    pub user_id: String,
    pub preferred_category_ids: Vec<String>,
}

/// Return `1.0` for an item in one of the visitor's preferred categories, or `0.0` otherwise.
pub fn score_preference(profile: &UserPreferenceProfile, item: &Item) -> f64 {
    if profile.preferred_category_ids.contains(&item.category_id) {
        1.0
    } else {
        0.0
    }
}

/// Deterministically generate [`UserPreferenceProfile`]s assigning [`INTEREST_COUNT`] distinct
/// preferred categories (or all available categories if fewer than [`INTEREST_COUNT`]) to each
/// user ID on [`PREFERENCE_RNG_STREAM`].
pub fn generate_user_preferences(
    user_ids: &[String],
    category_ids: &[String],
    seed: u64,
) -> Result<Vec<UserPreferenceProfile>, Error> {
    let mut seen_users = HashSet::with_capacity(user_ids.len());
    for uid in user_ids {
        if !seen_users.insert(uid.as_str()) {
            return Err(invalid(format!("duplicate user ID {uid}")));
        }
    }

    let mut seen_categories = HashSet::with_capacity(category_ids.len());
    for cid in category_ids {
        if !seen_categories.insert(cid.as_str()) {
            return Err(invalid(format!("duplicate category ID {cid}")));
        }
    }

    if user_ids.is_empty() {
        return Ok(Vec::new());
    }
    if category_ids.is_empty() {
        return Err(invalid(
            "cannot generate user preferences with zero categories",
        ));
    }

    let k = INTEREST_COUNT.min(category_ids.len());
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    rng.set_stream(PREFERENCE_RNG_STREAM);

    let mut preference_sets = HashSet::with_capacity(user_ids.len());
    let mut profiles = Vec::with_capacity(user_ids.len());

    for uid in user_ids {
        let mut attempts = 0usize;
        let indices = loop {
            let mut sample = index::sample(&mut rng, category_ids.len(), k).into_vec();
            sample.sort_unstable();
            attempts += 1;
            if preference_sets.insert(sample.clone()) || attempts >= 64 {
                break sample;
            }
        };
        let preferred_category_ids = indices
            .into_iter()
            .map(|i| category_ids[i].clone())
            .collect();
        profiles.push(UserPreferenceProfile {
            user_id: uid.clone(),
            preferred_category_ids,
        });
    }

    Ok(profiles)
}

/// Configuration for [`VisitSimulator`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct VisitSimulatorConfig {
    pub seed: u64,
    /// Optional seed used to generate visitor preferred categories (defaults to `seed`).
    pub preference_seed: Option<u64>,
    pub days: usize,
    pub daily_requests: usize,
    pub target_impressions: Option<u64>,
    pub max_click_probability: f64,
}

impl Default for VisitSimulatorConfig {
    fn default() -> Self {
        Self {
            seed: DEFAULT_SEED,
            preference_seed: None,
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
/// Owns each visitor's [`UserPreferenceProfile`] and uses a dedicated ChaCha8 RNG stream
/// (`CLICK_RNG_STREAM`) independent from visit arrivals (`RNG_STREAM`), preference generation
/// (`PREFERENCE_RNG_STREAM`), and ad-server ranking (Stream 0). An ad in one of the user's
/// preferred categories clicks with probability `max_click_probability` (default `4%`), and `0%`
/// otherwise.
pub struct ClickOracle {
    profiles: Vec<UserPreferenceProfile>,
    user_index: HashMap<String, usize>,
    max_click_probability: f64,
    rng: ChaCha8Rng,
}

impl ClickOracle {
    /// Construct a new [`ClickOracle`] with [`MAX_CLICK_PROBABILITY`] (`4%` preferred / `0%` else).
    pub fn new(profiles: Vec<UserPreferenceProfile>, seed: u64) -> Result<Self, Error> {
        Self::with_max_click_probability(profiles, seed, MAX_CLICK_PROBABILITY)
    }

    /// Construct a new [`ClickOracle`] with an explicit preferred-category click probability.
    pub fn with_max_click_probability(
        profiles: Vec<UserPreferenceProfile>,
        seed: u64,
        max_click_probability: f64,
    ) -> Result<Self, Error> {
        if !max_click_probability.is_finite() || !(0.0..=1.0).contains(&max_click_probability) {
            return Err(invalid(
                "max_click_probability must be finite and within [0.0, 1.0]",
            ));
        }

        let mut user_index = HashMap::with_capacity(profiles.len());
        for (idx, profile) in profiles.iter().enumerate() {
            if user_index.insert(profile.user_id.clone(), idx).is_some() {
                return Err(invalid(format!("duplicate user ID {}", profile.user_id)));
            }
        }

        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        rng.set_stream(CLICK_RNG_STREAM);

        Ok(Self {
            profiles,
            user_index,
            max_click_probability,
            rng,
        })
    }

    /// Generate visitor preferred categories from `user_ids` and `category_ids` and build a
    /// [`ClickOracle`].
    pub fn from_ids(
        user_ids: &[String],
        category_ids: &[String],
        preference_seed: u64,
        click_seed: u64,
        max_click_probability: f64,
    ) -> Result<Self, Error> {
        let profiles = generate_user_preferences(user_ids, category_ids, preference_seed)?;
        Self::with_max_click_probability(profiles, click_seed, max_click_probability)
    }

    /// All visitor preference profiles managed by this oracle.
    pub fn preferences(&self) -> &[UserPreferenceProfile] {
        &self.profiles
    }

    /// Preferred category IDs for `user_id`.
    pub fn preferred_categories(&self, user_id: &str) -> Result<&[String], Error> {
        let &idx = self
            .user_index
            .get(user_id)
            .ok_or_else(|| invalid(format!("unknown user ID {user_id}")))?;
        Ok(&self.profiles[idx].preferred_category_ids)
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

    /// Sample whether `user_id` clicks catalog `item` via [`score_preference`].
    pub fn evaluate_item_click(&mut self, user_id: &str, item: &Item) -> Result<bool, Error> {
        let &idx = self
            .user_index
            .get(user_id)
            .ok_or_else(|| invalid(format!("unknown user ID {user_id}")))?;
        let score = score_preference(&self.profiles[idx], item);
        Ok(self.rng.random_bool(self.max_click_probability * score))
    }

    fn user_count(&self) -> usize {
        self.profiles.len()
    }

    fn user_id(&self, idx: usize) -> &str {
        self.profiles[idx].user_id.as_str()
    }

    fn click_probability_for_index(&self, idx: usize, category_id: &str) -> f64 {
        if self.profiles[idx]
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

/// Standalone visit simulator, user preference model, ad slot generator, and visitor click oracle
/// driving [`AdServer`].
pub struct VisitSimulator {
    click_oracle: ClickOracle,
    config: VisitSimulatorConfig,
    rng: ChaCha8Rng,
    next_request_id: u64,
    total_impressions_delivered: u64,
}

impl VisitSimulator {
    /// Construct a new [`VisitSimulator`] from `user_ids` and marketplace `category_ids`,
    /// generating each visitor's preferred categories internally on `PREFERENCE_RNG_STREAM`.
    ///
    /// Visit arrivals remain preference-independent on `RNG_STREAM` and send only `user_id` to
    /// [`AdServer::serve_ads`]. Visitor click decisions use the internal [`ClickOracle`] on
    /// `CLICK_RNG_STREAM`.
    pub fn new(
        user_ids: Vec<String>,
        category_ids: Vec<String>,
        config: VisitSimulatorConfig,
    ) -> Result<Self, Error> {
        let pref_seed = config.preference_seed.unwrap_or(config.seed);
        let profiles = generate_user_preferences(&user_ids, &category_ids, pref_seed)?;
        Self::with_preferences(profiles, config)
    }

    /// Construct a new [`VisitSimulator`] with caller-supplied [`UserPreferenceProfile`]s.
    pub fn with_preferences(
        profiles: Vec<UserPreferenceProfile>,
        config: VisitSimulatorConfig,
    ) -> Result<Self, Error> {
        if config.days == 0 {
            return Err(invalid("days must be positive"));
        }
        let click_oracle = ClickOracle::with_max_click_probability(
            profiles,
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

    /// All visitor preference profiles owned by this simulator.
    pub fn user_preferences(&self) -> &[UserPreferenceProfile] {
        self.click_oracle.preferences()
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
    use ads_simulator::{Item, generate_marketplace};

    use crate::{
        CLICK_RNG_STREAM, ClickOracle, DEFAULT_DAILY_REQUESTS, INTEREST_COUNT,
        MAX_CLICK_PROBABILITY, PREFERENCE_RNG_STREAM, RNG_STREAM, UserPreferenceProfile,
        VisitSimulator, VisitSimulatorConfig, generate_user_preferences, score_preference,
        visit_simulator_spec,
    };

    #[test]
    fn generates_unique_five_category_preferences_per_user_and_scores_by_category() {
        let mp = generate_marketplace(42);
        let user_ids: Vec<String> = mp.users.iter().map(|u| u.id.clone()).collect();
        let category_ids: Vec<String> = mp.categories.iter().map(|c| c.id.clone()).collect();
        let valid_categories: HashSet<&str> = category_ids.iter().map(String::as_str).collect();

        let prefs_a = generate_user_preferences(&user_ids, &category_ids, 42).unwrap();
        let prefs_b = generate_user_preferences(&user_ids, &category_ids, 42).unwrap();
        let prefs_c = generate_user_preferences(&user_ids, &category_ids, 99).unwrap();

        assert_eq!(prefs_a, prefs_b);
        assert_ne!(prefs_a, prefs_c);
        assert_eq!(prefs_a.len(), user_ids.len());

        let mut unique_sets = HashSet::new();
        for profile in &prefs_a {
            assert_eq!(profile.preferred_category_ids.len(), INTEREST_COUNT);
            let distinct: HashSet<&str> = profile
                .preferred_category_ids
                .iter()
                .map(String::as_str)
                .collect();
            assert_eq!(distinct.len(), INTEREST_COUNT);
            assert!(distinct.iter().all(|c| valid_categories.contains(c)));
            assert!(unique_sets.insert(profile.preferred_category_ids.clone()));
        }

        let reader = UserPreferenceProfile {
            user_id: "user-0001".into(),
            preferred_category_ids: vec!["books".into()],
        };
        let book = Item {
            id: "catalog-0001".into(),
            category_id: "books".into(),
            name: "Book".into(),
            price_cents: 1_000,
        };
        let game = Item {
            id: "catalog-0002".into(),
            category_id: "games".into(),
            name: "Game".into(),
            price_cents: 1_000,
        };
        assert_eq!(score_preference(&reader, &book), 1.0);
        assert_eq!(score_preference(&reader, &game), 0.0);
    }

    #[test]
    fn natural_mode_issues_600_requests_per_day_and_12000_impressions_with_r1() {
        let mp = generate_marketplace(42);
        let user_ids: Vec<String> = mp.users.iter().map(|u| u.id.clone()).collect();
        let category_ids: Vec<String> = mp.categories.iter().map(|c| c.id.clone()).collect();
        let mut ad_server = AdServer::from_marketplace(&mp, AdServerConfig::default()).unwrap();
        let mut simulator = VisitSimulator::new(
            user_ids,
            category_ids,
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
        let profiles = vec![UserPreferenceProfile {
            user_id: "user-0001".to_owned(),
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

        let mut oracle_a = ClickOracle::new(profiles.clone(), 42).unwrap();
        let mut oracle_b = ClickOracle::new(profiles, 42).unwrap();
        assert_eq!(
            oracle_a
                .click_probability("user-0001", "category-001")
                .unwrap(),
            MAX_CLICK_PROBABILITY
        );
        assert_eq!(
            oracle_a
                .click_probability("user-0001", "category-099")
                .unwrap(),
            0.0
        );

        let mut pref_clicks_a = 0u64;
        let mut pref_clicks_b = 0u64;
        for _ in 0..5_000 {
            assert!(
                !oracle_a
                    .evaluate_click("user-0001", &non_preferred_ad)
                    .unwrap()
            );
            assert!(
                !oracle_b
                    .evaluate_click("user-0001", &non_preferred_ad)
                    .unwrap()
            );
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
        let mut simulator = VisitSimulator::new(
            user_ids.clone(),
            category_ids.clone(),
            VisitSimulatorConfig {
                seed: 42,
                preference_seed: None,
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
            user_ids,
            category_ids,
            VisitSimulatorConfig {
                seed: 42,
                preference_seed: None,
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
    fn spec_reports_visit_preference_and_click_oracle_constants() {
        let spec = visit_simulator_spec();
        assert!(spec.user_independent_arrivals);
        assert_eq!(spec.interest_count, INTEREST_COUNT);
        assert_eq!(spec.rng_stream, RNG_STREAM);
        assert_eq!(spec.click_rng_stream, CLICK_RNG_STREAM);
        assert_eq!(spec.preference_rng_stream, PREFERENCE_RNG_STREAM);
        assert_eq!(spec.max_click_probability, MAX_CLICK_PROBABILITY);
        assert_eq!(spec.default_daily_requests, 600);
        assert_eq!(spec.default_days, 30);
    }
}
