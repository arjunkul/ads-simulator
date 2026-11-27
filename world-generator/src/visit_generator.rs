//! Visit generator and ad-slot arrival driver inside `world-generator`.
//!
//! Samples visiting users uniformly without depending on preferences (`RNG_STREAM` = 1),
//! optionally generates PST diurnal intraday timestamps (`sample_pst_second_of_day`),
//! issues user-ID-only ad requests against an [`AdServingTarget`] (such as `ad_server::AdServer`),
//! evaluates visitor click responses through [`UserActivityOracle`], and reports delivered
//! impressions and click billing back to the ad server.

use std::io::{Error, ErrorKind};

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};

use crate::marketplace::Marketplace;
use crate::timestamp::{SECONDS_PER_DAY, sample_pst_second_of_day};
use crate::user_activity_oracle::{
    CLICK_RNG_STREAM, DEFAULT_WORLD_MODEL, INTEREST_COUNT, MAX_CLICK_PROBABILITY,
    PREFERENCE_RNG_STREAM, UserActivityOracle, UserPreferenceProfile, WorldModel,
    WorldModelV1Params, generate_user_preferences,
};

pub const DEFAULT_DAYS: usize = 30;
/// Default number of ad-server requests issued per day when no impression target is specified.
/// With R1's 20-ad slate, 600 requests/day produces 12,000 impressions/day (360,000 over 30 days).
pub const DEFAULT_DAILY_REQUESTS: usize = 600;
pub const DEFAULT_SEED: u64 = 42;
/// ChaCha8 RNG stream for preference-independent visit arrivals and diurnal timestamps.
pub const RNG_STREAM: u64 = 1;

/// Compiled specification of the `visit_generator` module inside `world-generator`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VisitGeneratorSpec {
    pub crate_name: String,
    pub module_name: String,
    pub binary_name: String,
    pub version: String,
    pub default_days: usize,
    pub default_daily_requests: usize,
    pub default_seed: u64,
    pub default_world_model: WorldModel,
    pub interest_count: usize,
    pub rng_stream: u64,
    pub click_rng_stream: u64,
    pub preference_rng_stream: u64,
    pub max_click_probability: f64,
    pub user_independent_arrivals: bool,
}

/// Return the live compiled specification of the `visit_generator` module.
pub fn visit_generator_spec() -> VisitGeneratorSpec {
    VisitGeneratorSpec {
        crate_name: env!("CARGO_PKG_NAME").to_owned(),
        module_name: "visit_generator".to_owned(),
        binary_name: "visit-generator".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        default_days: DEFAULT_DAYS,
        default_daily_requests: DEFAULT_DAILY_REQUESTS,
        default_seed: DEFAULT_SEED,
        default_world_model: DEFAULT_WORLD_MODEL,
        interest_count: INTEREST_COUNT,
        rng_stream: RNG_STREAM,
        click_rng_stream: CLICK_RNG_STREAM,
        preference_rng_stream: PREFERENCE_RNG_STREAM,
        max_click_probability: MAX_CLICK_PROBABILITY,
        user_independent_arrivals: true,
    }
}

/// Interface implemented by an ad server (such as `ad_server::AdServer`) so [`VisitGenerator`]
/// can drive user-ID-only ad requests, intraday clocks, and impression/click billing without
/// creating a cyclic dependency on `ad-server`.
pub trait AdServingTarget {
    type Ad: Clone;
    type Response;

    fn set_day(&mut self, day: usize) -> Result<(), Error>;
    fn set_second_of_day(&mut self, second_of_day: u32) -> Result<(), Error>;
    fn uses_pst_diurnal_timestamps(&self) -> bool;
    fn serve_ads(&mut self, user_id: &str) -> Self::Response;
    fn into_response_parts(response: Self::Response) -> (String, Vec<Self::Ad>);
    fn ad_slot_position(ad: &Self::Ad) -> usize;
    fn ad_campaign_index(ad: &Self::Ad) -> usize;
    fn ad_category_id(ad: &Self::Ad) -> &str;
    fn ad_advertiser_id(ad: &Self::Ad) -> &str;
    fn record_impression(
        &mut self,
        day: usize,
        campaign_index: usize,
        clicked: bool,
    ) -> Result<Option<u64>, Error>;
}

/// Configuration for [`VisitGenerator`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct VisitGeneratorConfig {
    pub seed: u64,
    /// Optional seed used to generate visitor preferred categories and `world-model-v1` noise
    /// parameters (defaults to `seed`).
    pub preference_seed: Option<u64>,
    pub days: usize,
    pub daily_requests: usize,
    pub target_impressions: Option<u64>,
    pub max_click_probability: f64,
    #[serde(default)]
    pub world_model: WorldModel,
}

impl Default for VisitGeneratorConfig {
    fn default() -> Self {
        Self {
            seed: DEFAULT_SEED,
            preference_seed: None,
            days: DEFAULT_DAYS,
            daily_requests: DEFAULT_DAILY_REQUESTS,
            target_impressions: None,
            max_click_probability: MAX_CLICK_PROBABILITY,
            world_model: DEFAULT_WORLD_MODEL,
        }
    }
}

/// A single ad impression delivered from an ad-server response slate along with the visitor's
/// click outcome from [`UserActivityOracle`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedImpression<A> {
    pub day: usize,
    pub request_id: u64,
    pub slot_position: usize,
    pub user_id: String,
    pub ad: A,
    pub clicked: bool,
}

/// Aggregate counters produced by running one day of visit generation, slot serving, and
/// user activity click evaluation.
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

/// Visit generator driving preference-independent shopper arrivals, ad-slot requests against an
/// [`AdServingTarget`], and click evaluation via [`UserActivityOracle`].
pub struct VisitGenerator {
    user_activity_oracle: UserActivityOracle,
    config: VisitGeneratorConfig,
    rng: ChaCha8Rng,
    next_request_id: u64,
    total_impressions_delivered: u64,
}

impl VisitGenerator {
    /// Construct a new [`VisitGenerator`] directly from a [`Marketplace`], preserving full retail
    /// group, category, and advertiser metadata for [`WorldModel::V1`].
    pub fn from_marketplace(
        marketplace: &Marketplace,
        config: VisitGeneratorConfig,
    ) -> Result<Self, Error> {
        if config.days == 0 {
            return Err(invalid("days must be positive"));
        }
        let pref_seed = config.preference_seed.unwrap_or(config.seed);
        let user_activity_oracle = UserActivityOracle::from_marketplace(
            marketplace,
            pref_seed,
            config.seed,
            config.max_click_probability,
            config.world_model,
        )?;
        let mut rng = ChaCha8Rng::seed_from_u64(config.seed);
        rng.set_stream(RNG_STREAM);

        Ok(Self {
            user_activity_oracle,
            config,
            rng,
            next_request_id: 1,
            total_impressions_delivered: 0,
        })
    }

    /// Construct a new [`VisitGenerator`] from `user_ids` and marketplace `category_ids`,
    /// generating each visitor's preferred categories internally on `PREFERENCE_RNG_STREAM`.
    ///
    /// Visit arrivals remain preference-independent on `RNG_STREAM` and send only `user_id` to
    /// [`AdServingTarget::serve_ads`]. Visitor click decisions use the internal
    /// [`UserActivityOracle`] on `CLICK_RNG_STREAM`.
    pub fn new(
        user_ids: Vec<String>,
        category_ids: Vec<String>,
        config: VisitGeneratorConfig,
    ) -> Result<Self, Error> {
        let pref_seed = config.preference_seed.unwrap_or(config.seed);
        let profiles = generate_user_preferences(&user_ids, &category_ids, pref_seed)?;
        let v1_params = match config.world_model {
            WorldModel::V0 => None,
            WorldModel::V1 => Some(WorldModelV1Params::from_category_ids(&category_ids, pref_seed)),
        };
        Self::with_preferences_and_v1_params(profiles, config, v1_params)
    }

    /// Construct a new [`VisitGenerator`] with caller-supplied [`UserPreferenceProfile`]s.
    pub fn with_preferences(
        profiles: Vec<UserPreferenceProfile>,
        config: VisitGeneratorConfig,
    ) -> Result<Self, Error> {
        Self::with_preferences_and_v1_params(profiles, config, None)
    }

    fn with_preferences_and_v1_params(
        profiles: Vec<UserPreferenceProfile>,
        config: VisitGeneratorConfig,
        v1_params: Option<WorldModelV1Params>,
    ) -> Result<Self, Error> {
        if config.days == 0 {
            return Err(invalid("days must be positive"));
        }
        let user_activity_oracle = UserActivityOracle::with_world_model(
            profiles,
            config.seed,
            config.max_click_probability,
            config.world_model,
            v1_params,
        )?;

        let mut rng = ChaCha8Rng::seed_from_u64(config.seed);
        rng.set_stream(RNG_STREAM);

        Ok(Self {
            user_activity_oracle,
            config,
            rng,
            next_request_id: 1,
            total_impressions_delivered: 0,
        })
    }

    /// Active [`WorldModel`] used by the internal [`UserActivityOracle`].
    pub fn world_model(&self) -> WorldModel {
        self.user_activity_oracle.world_model()
    }

    /// All visitor preference profiles owned by this generator's oracle.
    pub fn user_preferences(&self) -> &[UserPreferenceProfile] {
        self.user_activity_oracle.preferences()
    }

    /// Borrow the underlying [`UserActivityOracle`].
    pub fn user_activity_oracle(&self) -> &UserActivityOracle {
        &self.user_activity_oracle
    }

    /// Mutably borrow the underlying [`UserActivityOracle`].
    pub fn user_activity_oracle_mut(&mut self) -> &mut UserActivityOracle {
        &mut self.user_activity_oracle
    }

    /// Return the daily impression quota for 1-based `day` when `target_impressions` is set.
    pub fn daily_quota(&self, day: usize) -> Option<u64> {
        self.config.target_impressions.map(|target| {
            let days = self.config.days as u64;
            target / days + u64::from(day as u64 <= target % days)
        })
    }

    fn generate_request_indexed<T: AdServingTarget>(
        &mut self,
        ad_server: &mut T,
    ) -> Result<(u64, usize, T::Response), Error> {
        let user_count = self.user_activity_oracle.user_count();
        if user_count == 0 {
            return Err(invalid("cannot generate ad request with zero users"));
        }
        let user_index = self.rng.random_range(0..user_count);
        let user_id = self.user_activity_oracle.user_id(user_index).to_owned();
        let request_id = self.next_request_id;
        self.next_request_id += 1;
        let response = ad_server.serve_ads(&user_id);
        Ok((request_id, user_index, response))
    }

    /// Issue a single preference-independent ad request to `ad_server` and return `(request_id, response)`.
    pub fn generate_request<T: AdServingTarget>(
        &mut self,
        ad_server: &mut T,
    ) -> Result<(u64, T::Response), Error> {
        let (request_id, _, response) = self.generate_request_indexed(ad_server)?;
        Ok((request_id, response))
    }

    /// Execute one simulated day (1-based `day`), generating user visits, querying
    /// `ad_server.serve_ads(user_id)`, evaluating visitor clicks via [`UserActivityOracle`],
    /// recording outcomes back to `ad_server.record_impression`, and notifying `on_impression`
    /// for each delivered impression.
    pub fn run_day<T, F>(
        &mut self,
        day: usize,
        ad_server: &mut T,
        mut on_impression: F,
    ) -> Result<DayGenerationStats, Error>
    where
        T: AdServingTarget,
        F: FnMut(&GeneratedImpression<T::Ad>),
    {
        if day == 0 || day > self.config.days {
            return Err(invalid(format!(
                "day {day} out of range 1..={}",
                self.config.days
            )));
        }
        ad_server.set_day(day)?;
        let use_pst_diurnal = ad_server.uses_pst_diurnal_timestamps();
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
                if self.user_activity_oracle.user_count() == 0 {
                    return Ok(stats);
                }
                let pst_seconds: Vec<u32> = if use_pst_diurnal {
                    let mut v: Vec<u32> = (0..self.config.daily_requests)
                        .map(|_| sample_pst_second_of_day(&mut self.rng))
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
                    let (resp_user_id, ads) = T::into_response_parts(response);
                    if ads.is_empty() {
                        stats.opportunities += 1;
                        stats.insufficient_budget += 1;
                        continue;
                    }
                    for ad in ads {
                        stats.opportunities += 1;
                        let clicked = self.user_activity_oracle.evaluate_click_for_index(
                            user_index,
                            T::ad_category_id(&ad),
                            T::ad_advertiser_id(&ad),
                        );
                        let cost =
                            ad_server.record_impression(day, T::ad_campaign_index(&ad), clicked)?;
                        let impression = GeneratedImpression {
                            day,
                            request_id,
                            slot_position: T::ad_slot_position(&ad),
                            user_id: resp_user_id.clone(),
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
                    if self.user_activity_oracle.user_count() == 0 {
                        return Err(invalid(format!(
                            "impression target cannot be reached on day {day}: no users available ({} impressions delivered)",
                            self.total_impressions_delivered
                        )));
                    }
                    if use_pst_diurnal && target_for_day > 0 {
                        let frac = (stats.impressions as f64) / (target_for_day as f64);
                        let sec = ((frac * f64::from(SECONDS_PER_DAY)).floor() as u32)
                            .min(SECONDS_PER_DAY - 1);
                        ad_server.set_second_of_day(sec)?;
                    }
                    let (request_id, user_index, response) =
                        self.generate_request_indexed(ad_server)?;
                    stats.ad_requests += 1;
                    let (resp_user_id, ads) = T::into_response_parts(response);
                    if ads.is_empty() {
                        return Err(invalid(format!(
                            "impression target cannot be reached on day {day}: no campaigns that can afford the CPC ({} impressions delivered)",
                            self.total_impressions_delivered
                        )));
                    }
                    for ad in ads {
                        if stats.impressions >= target_for_day {
                            break;
                        }
                        stats.opportunities += 1;
                        let clicked = self.user_activity_oracle.evaluate_click_for_index(
                            user_index,
                            T::ad_category_id(&ad),
                            T::ad_advertiser_id(&ad),
                        );
                        let cost =
                            ad_server.record_impression(day, T::ad_campaign_index(&ad), clicked)?;
                        let impression = GeneratedImpression {
                            day,
                            request_id,
                            slot_position: T::ad_slot_position(&ad),
                            user_id: resp_user_id.clone(),
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
    use std::io::Error;

    use crate::marketplace::generate_marketplace;
    use crate::user_activity_oracle::{
        CLICK_RNG_STREAM, DEFAULT_WORLD_MODEL, INTEREST_COUNT, MAX_CLICK_PROBABILITY,
        PREFERENCE_RNG_STREAM, WorldModel,
    };
    use crate::visit_generator::{
        AdServingTarget, DEFAULT_DAILY_REQUESTS, RNG_STREAM, VisitGenerator, VisitGeneratorConfig,
        visit_generator_spec,
    };

    #[derive(Clone, Debug)]
    struct MockAd {
        slot_position: usize,
        campaign_index: usize,
        category_id: String,
        advertiser_id: String,
    }

    struct MockAdServer {
        slate_size: usize,
        cpc_cents: u64,
        empty: bool,
    }

    impl AdServingTarget for MockAdServer {
        type Ad = MockAd;
        type Response = (String, Vec<MockAd>);

        fn set_day(&mut self, _day: usize) -> Result<(), Error> {
            Ok(())
        }

        fn set_second_of_day(&mut self, _second_of_day: u32) -> Result<(), Error> {
            Ok(())
        }

        fn uses_pst_diurnal_timestamps(&self) -> bool {
            false
        }

        fn serve_ads(&mut self, user_id: &str) -> Self::Response {
            if self.empty {
                return (user_id.to_owned(), Vec::new());
            }
            let ads = (0..self.slate_size)
                .map(|i| MockAd {
                    slot_position: i + 1,
                    campaign_index: i,
                    category_id: format!("category-{:03}", (i % 100) + 1),
                    advertiser_id: format!("advertiser-{:02}", (i % 50) + 1),
                })
                .collect();
            (user_id.to_owned(), ads)
        }

        fn into_response_parts(response: Self::Response) -> (String, Vec<Self::Ad>) {
            response
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
            _day: usize,
            _campaign_index: usize,
            clicked: bool,
        ) -> Result<Option<u64>, Error> {
            Ok(clicked.then_some(self.cpc_cents))
        }
    }

    #[test]
    fn natural_mode_issues_600_requests_per_day_and_12000_impressions() {
        let mp = generate_marketplace(42);
        let user_ids: Vec<String> = mp.users.iter().map(|u| u.id.clone()).collect();
        let category_ids: Vec<String> = mp.categories.iter().map(|c| c.id.clone()).collect();
        let mut ad_server = MockAdServer {
            slate_size: 20,
            cpc_cents: 100,
            empty: false,
        };
        let mut generator = VisitGenerator::new(
            user_ids,
            category_ids,
            VisitGeneratorConfig {
                max_click_probability: 0.0,
                ..VisitGeneratorConfig::default()
            },
        )
        .unwrap();

        let day1 = generator.run_day(1, &mut ad_server, |_| {}).unwrap();
        assert_eq!(day1.day, 1);
        assert_eq!(day1.ad_requests, DEFAULT_DAILY_REQUESTS as u64);
        assert_eq!(day1.impressions, 12_000);
        assert_eq!(day1.opportunities, 12_000);
        assert_eq!(day1.insufficient_budget, 0);
        assert_eq!(day1.clicks, 0);
        assert_eq!(day1.spend_cents, 0);
    }

    #[test]
    fn targeted_mode_hits_exact_daily_quota_and_errors_when_empty() {
        let mp = generate_marketplace(42);
        let user_ids: Vec<String> = mp.users.iter().map(|u| u.id.clone()).collect();
        let category_ids: Vec<String> = mp.categories.iter().map(|c| c.id.clone()).collect();
        let mut ad_server = MockAdServer {
            slate_size: 20,
            cpc_cents: 100,
            empty: false,
        };
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
                world_model: WorldModel::V0,
            },
        )
        .unwrap();

        let day1 = generator.run_day(1, &mut ad_server, |_| {}).unwrap();
        assert_eq!(day1.impressions, 34);
        assert_eq!(day1.ad_requests, 2);

        let mut empty_server = MockAdServer {
            slate_size: 20,
            cpc_cents: 100,
            empty: true,
        };
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
        let err = gen2.run_day(1, &mut empty_server, |_| {}).unwrap_err();
        assert!(err.to_string().contains("cannot be reached on day 1"));
    }

    #[test]
    fn from_marketplace_with_world_model_v1_runs_deterministically_with_clicks() {
        let mp = generate_marketplace(42);
        let mut server_a = MockAdServer {
            slate_size: 20,
            cpc_cents: 100,
            empty: false,
        };
        let mut server_b = MockAdServer {
            slate_size: 20,
            cpc_cents: 100,
            empty: false,
        };
        let cfg = VisitGeneratorConfig {
            seed: 42,
            preference_seed: Some(42),
            world_model: WorldModel::V1,
            ..VisitGeneratorConfig::default()
        };
        let mut gen_a = VisitGenerator::from_marketplace(&mp, cfg).unwrap();
        let mut gen_b = VisitGenerator::from_marketplace(&mp, cfg).unwrap();
        assert_eq!(gen_a.world_model(), WorldModel::V1);

        let stats_a = gen_a.run_day(1, &mut server_a, |_| {}).unwrap();
        let stats_b = gen_b.run_day(1, &mut server_b, |_| {}).unwrap();
        assert_eq!(stats_a, stats_b);
        assert_eq!(stats_a.impressions, 12_000);
        assert!(stats_a.clicks > 0);
    }

    #[test]
    fn spec_reports_visit_generator_constants() {
        let spec = visit_generator_spec();
        assert_eq!(spec.crate_name, "world-generator");
        assert_eq!(spec.module_name, "visit_generator");
        assert_eq!(spec.binary_name, "visit-generator");
        assert_eq!(spec.default_world_model, DEFAULT_WORLD_MODEL);
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
