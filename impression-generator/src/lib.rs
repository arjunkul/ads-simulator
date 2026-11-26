//! User-independent ad-impression generator.
//!
//! This module is intentionally decoupled from shopper preferences, product categories, and
//! organic browsing history. It accepts only a list of valid `user_id`s and operates an
//! [`ad_server::AdServer`] by issuing ad requests with `user_id` and delivering the returned
//! multi-campaign ad slates.

use std::collections::HashSet;
use std::io::{Error, ErrorKind};

use ad_server::{AdResponse, AdServer, ServedAd};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};

pub const DEFAULT_DAYS: usize = ad_server::DAYS;
/// Default number of ad-server requests issued per day when no impression target is specified.
/// With R1's 20-ad slate, 600 requests/day produces 12,000 impressions/day (360,000 over 30 days).
pub const DEFAULT_DAILY_REQUESTS: usize = 600;
pub const DEFAULT_SEED: u64 = 42;
pub const RNG_STREAM: u64 = 1;

/// Compiled specification of the user-independent `impression-generator` system.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImpressionGeneratorSpec {
    pub crate_name: String,
    pub binary_name: String,
    pub version: String,
    pub default_days: usize,
    pub default_daily_requests: usize,
    pub default_seed: u64,
    pub rng_stream: u64,
    pub user_independent: bool,
}

/// Return the live compiled specification of the `impression-generator` crate.
pub fn impression_generator_spec() -> ImpressionGeneratorSpec {
    ImpressionGeneratorSpec {
        crate_name: env!("CARGO_PKG_NAME").to_owned(),
        binary_name: "impression-generator".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        default_days: DEFAULT_DAYS,
        default_daily_requests: DEFAULT_DAILY_REQUESTS,
        default_seed: DEFAULT_SEED,
        rng_stream: RNG_STREAM,
        user_independent: true,
    }
}

/// Configuration for [`ImpressionGenerator`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImpressionGeneratorConfig {
    pub seed: u64,
    pub days: usize,
    pub daily_requests: usize,
    pub target_impressions: Option<u64>,
}

impl Default for ImpressionGeneratorConfig {
    fn default() -> Self {
        Self {
            seed: DEFAULT_SEED,
            days: DEFAULT_DAYS,
            daily_requests: DEFAULT_DAILY_REQUESTS,
            target_impressions: None,
        }
    }
}

/// A single ad impression delivered from an [`AdServer`] response slate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedImpression {
    pub day: usize,
    pub request_id: u64,
    pub slot_position: usize,
    pub user_id: String,
    pub ad: ServedAd,
}

/// Aggregate counters produced by running one day of impression generation.
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

/// Standalone, user-preference-independent ad-impression generator that drives [`AdServer`].
pub struct ImpressionGenerator {
    user_ids: Vec<String>,
    config: ImpressionGeneratorConfig,
    rng: ChaCha8Rng,
    next_request_id: u64,
    total_impressions_delivered: u64,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message.into())
}

impl ImpressionGenerator {
    /// Construct a new user-independent [`ImpressionGenerator`] from valid user IDs.
    ///
    /// Notice this accepts ONLY user identifiers — it has no access to user preferences,
    /// categories, or organic browsing events.
    pub fn new(user_ids: Vec<String>, config: ImpressionGeneratorConfig) -> Result<Self, Error> {
        if config.days == 0 {
            return Err(invalid("days must be positive"));
        }
        let unique: HashSet<&str> = user_ids.iter().map(String::as_str).collect();
        if unique.len() != user_ids.len() {
            return Err(invalid("duplicate user IDs supplied to impression generator"));
        }

        let mut rng = ChaCha8Rng::seed_from_u64(config.seed);
        rng.set_stream(RNG_STREAM);

        Ok(Self {
            user_ids,
            config,
            rng,
            next_request_id: 1,
            total_impressions_delivered: 0,
        })
    }

    /// Return the daily impression quota for 1-based `day` when `target_impressions` is set.
    pub fn daily_quota(&self, day: usize) -> Option<u64> {
        self.config.target_impressions.map(|target| {
            let days = self.config.days as u64;
            target / days + u64::from(day as u64 <= target % days)
        })
    }

    /// Issue a single user-independent ad request to `ad_server` and return `(request_id, response)`.
    pub fn generate_request(
        &mut self,
        ad_server: &mut AdServer,
    ) -> Result<(u64, AdResponse), Error> {
        if self.user_ids.is_empty() {
            return Err(invalid("cannot generate ad request with zero users"));
        }
        let user_index = self.rng.random_range(0..self.user_ids.len());
        let user_id = self.user_ids[user_index].clone();
        let request_id = self.next_request_id;
        self.next_request_id += 1;
        let response = ad_server.serve_ads(&user_id);
        Ok((request_id, response))
    }

    /// Execute one simulated day (1-based `day`), querying `ad_server.serve_ads(user_id)` and
    /// delivering each served ad to `on_impression`.
    ///
    /// `on_impression` evaluates external user click behavior and returns `true` if clicked.
    /// Budget deduction and campaign retirement are delegated back to `ad_server.record_impression`.
    pub fn run_day<F>(
        &mut self,
        day: usize,
        ad_server: &mut AdServer,
        mut on_impression: F,
    ) -> Result<DayGenerationStats, Error>
    where
        F: FnMut(&GeneratedImpression) -> bool,
    {
        if day == 0 || day > self.config.days {
            return Err(invalid(format!(
                "day {day} out of range 1..={}",
                self.config.days
            )));
        }
        ad_server.set_day(day)?;

        let quota = self.daily_quota(day);
        let mut stats = DayGenerationStats {
            day,
            ..DayGenerationStats::default()
        };

        match quota {
            None => {
                if self.user_ids.is_empty() {
                    return Ok(stats);
                }
                for _ in 0..self.config.daily_requests {
                    let (request_id, response) = self.generate_request(ad_server)?;
                    stats.ad_requests += 1;
                    if response.ads.is_empty() {
                        stats.opportunities += 1;
                        stats.insufficient_budget += 1;
                        continue;
                    }
                    for ad in response.ads {
                        stats.opportunities += 1;
                        let impression = GeneratedImpression {
                            day,
                            request_id,
                            slot_position: ad.slot_position,
                            user_id: response.user_id.clone(),
                            ad,
                        };
                        let clicked = on_impression(&impression);
                        let cost = ad_server.record_impression(
                            day,
                            impression.ad.campaign_index,
                            clicked,
                        )?;
                        stats.impressions += 1;
                        self.total_impressions_delivered += 1;
                        if let Some(cost) = cost {
                            stats.clicks += 1;
                            stats.spend_cents += cost;
                        }
                    }
                }
            }
            Some(target_for_day) => {
                while stats.impressions < target_for_day {
                    if self.user_ids.is_empty() {
                        return Err(invalid(format!(
                            "impression target cannot be reached on day {day}: no users available ({} impressions delivered)",
                            self.total_impressions_delivered
                        )));
                    }
                    let (request_id, response) = self.generate_request(ad_server)?;
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
                        let impression = GeneratedImpression {
                            day,
                            request_id,
                            slot_position: ad.slot_position,
                            user_id: response.user_id.clone(),
                            ad,
                        };
                        let clicked = on_impression(&impression);
                        let cost = ad_server.record_impression(
                            day,
                            impression.ad.campaign_index,
                            clicked,
                        )?;
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

    use ad_server::{AdServer, AdServerConfig, RankerStrategy};
    use ads_simulator::generate_marketplace;

    use crate::{
        DEFAULT_DAILY_REQUESTS, ImpressionGenerator, ImpressionGeneratorConfig,
        impression_generator_spec,
    };

    #[test]
    fn natural_mode_issues_600_requests_per_day_and_12000_impressions_with_r1() {
        let mp = generate_marketplace(42);
        let user_ids: Vec<String> = mp.users.iter().map(|u| u.id.clone()).collect();
        let mut ad_server = AdServer::from_marketplace(&mp, AdServerConfig::default()).unwrap();
        let mut generator =
            ImpressionGenerator::new(user_ids, ImpressionGeneratorConfig::default()).unwrap();

        let day1 = generator.run_day(1, &mut ad_server, |_| false).unwrap();
        assert_eq!(day1.day, 1);
        assert_eq!(day1.ad_requests, DEFAULT_DAILY_REQUESTS as u64);
        assert_eq!(day1.impressions, 12_000);
        assert_eq!(day1.opportunities, 12_000);
        assert_eq!(day1.insufficient_budget, 0);
        assert_eq!(day1.clicks, 0);
        assert_eq!(day1.spend_cents, 0);
    }

    #[test]
    fn targeted_mode_hits_exact_daily_quota_and_errors_when_budget_exhausts() {
        let mp = generate_marketplace(42);
        let user_ids: Vec<String> = mp.users.iter().map(|u| u.id.clone()).collect();
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
        let mut generator = ImpressionGenerator::new(
            user_ids.clone(),
            ImpressionGeneratorConfig {
                seed: 42,
                days: 30,
                daily_requests: DEFAULT_DAILY_REQUESTS,
                target_impressions: Some(1_000),
            },
        )
        .unwrap();

        // Day 1 quota = 34 -> requires 2 requests (20 + 14 delivered).
        let mut seen_req1 = HashSet::new();
        let day1 = generator
            .run_day(1, &mut ad_server, |imp| {
                if imp.request_id == 1 {
                    assert!(seen_req1.insert(imp.ad.campaign_id.clone()));
                }
                false
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
        let mut gen2 = ImpressionGenerator::new(
            user_ids,
            ImpressionGeneratorConfig {
                seed: 42,
                days: 30,
                daily_requests: DEFAULT_DAILY_REQUESTS,
                target_impressions: Some(30),
            },
        )
        .unwrap();
        let err = gen2.run_day(1, &mut broke_server, |_| false).unwrap_err();
        assert!(err.to_string().contains("cannot be reached on day 1"));
    }

    #[test]
    fn spec_marks_generator_user_independent() {
        let spec = impression_generator_spec();
        assert!(spec.user_independent);
        assert_eq!(spec.default_daily_requests, 600);
        assert_eq!(spec.default_days, 30);
    }
}
