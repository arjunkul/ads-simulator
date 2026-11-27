//! User preference profiles and visitor click oracle (`UserActivityOracle`).
//!
//! Models how visiting shoppers respond to ads in the simulated world:
//! 1. **User preferred categories (`INTEREST_COUNT`, `PREFERENCE_RNG_STREAM`)**: assigns each
//!    visitor a unique set of preferred product categories ([`UserPreferenceProfile`]) and exposes
//!    [`score_preference`].
//! 2. **User activity oracle ([`UserActivityOracle`], `CLICK_RNG_STREAM`)**: evaluates whether a
//!    visiting user clicks each served ad (`MAX_CLICK_PROBABILITY` = 4% if the ad's category is
//!    among the user's preferred categories, 0% otherwise).

use std::collections::{HashMap, HashSet};
use std::io::{Error, ErrorKind};

use rand::seq::index;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};

use crate::marketplace::Item;

/// Number of distinct preferred categories assigned to each visitor.
pub const INTEREST_COUNT: usize = 5;
/// ChaCha8 RNG stream for the visitor [`UserActivityOracle`].
pub const CLICK_RNG_STREAM: u64 = 2;
/// ChaCha8 RNG stream for generating visitor preferred categories.
pub const PREFERENCE_RNG_STREAM: u64 = 3;
/// Click probability when a served ad matches one of the visiting user's preferred categories.
pub const MAX_CLICK_PROBABILITY: f64 = 0.04;

/// Compiled specification of the `user_activity_oracle` module inside `world-generator`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UserActivityOracleSpec {
    pub crate_name: String,
    pub module_name: String,
    pub version: String,
    pub interest_count: usize,
    pub click_rng_stream: u64,
    pub preference_rng_stream: u64,
    pub max_click_probability: f64,
}

/// Return the live compiled specification of the `user_activity_oracle` module.
pub fn user_activity_oracle_spec() -> UserActivityOracleSpec {
    UserActivityOracleSpec {
        crate_name: env!("CARGO_PKG_NAME").to_owned(),
        module_name: "user_activity_oracle".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        interest_count: INTEREST_COUNT,
        click_rng_stream: CLICK_RNG_STREAM,
        preference_rng_stream: PREFERENCE_RNG_STREAM,
        max_click_probability: MAX_CLICK_PROBABILITY,
    }
}

/// A visitor's preferred product categories owned by `world-generator::user_activity_oracle`.
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

fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message.into())
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

/// User activity oracle modeling whether a visiting shopper clicks a served ad.
///
/// Owns each visitor's [`UserPreferenceProfile`] and uses a dedicated ChaCha8 RNG stream
/// (`CLICK_RNG_STREAM`) independent from visit generation (`RNG_STREAM`), preference generation
/// (`PREFERENCE_RNG_STREAM`), and ad-server ranking (Stream 0). An ad in one of the user's
/// preferred categories clicks with probability `max_click_probability` (default `4%`), and `0%`
/// otherwise.
pub struct UserActivityOracle {
    profiles: Vec<UserPreferenceProfile>,
    user_index: HashMap<String, usize>,
    max_click_probability: f64,
    rng: ChaCha8Rng,
}

impl UserActivityOracle {
    /// Construct a new [`UserActivityOracle`] with [`MAX_CLICK_PROBABILITY`] (`4%` preferred / `0%` else).
    pub fn new(profiles: Vec<UserPreferenceProfile>, seed: u64) -> Result<Self, Error> {
        Self::with_max_click_probability(profiles, seed, MAX_CLICK_PROBABILITY)
    }

    /// Construct a new [`UserActivityOracle`] with an explicit preferred-category click probability.
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
    /// [`UserActivityOracle`].
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

    /// Sample whether `user_id` clicks an ad in `category_id` using this oracle's isolated click RNG stream.
    pub fn evaluate_click(&mut self, user_id: &str, category_id: &str) -> Result<bool, Error> {
        let &idx = self
            .user_index
            .get(user_id)
            .ok_or_else(|| invalid(format!("unknown user ID {user_id}")))?;
        Ok(self.evaluate_click_for_index(idx, category_id))
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

    pub(crate) fn user_count(&self) -> usize {
        self.profiles.len()
    }

    pub(crate) fn user_id(&self, idx: usize) -> &str {
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

    pub(crate) fn evaluate_click_for_index(&mut self, idx: usize, category_id: &str) -> bool {
        let prob = self.click_probability_for_index(idx, category_id);
        self.rng.random_bool(prob)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::marketplace::{Item, generate_marketplace};
    use crate::user_activity_oracle::{
        CLICK_RNG_STREAM, INTEREST_COUNT, MAX_CLICK_PROBABILITY, PREFERENCE_RNG_STREAM,
        UserActivityOracle, UserPreferenceProfile, generate_user_preferences, score_preference,
        user_activity_oracle_spec,
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
    fn user_activity_oracle_clicks_only_preferred_categories_and_is_deterministic() {
        let profiles = vec![UserPreferenceProfile {
            user_id: "user-0001".to_owned(),
            preferred_category_ids: vec!["category-001".to_owned()],
        }];

        let mut oracle_a = UserActivityOracle::new(profiles.clone(), 42).unwrap();
        let mut oracle_b = UserActivityOracle::new(profiles, 42).unwrap();
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
            assert!(!oracle_a.evaluate_click("user-0001", "category-099").unwrap());
            assert!(!oracle_b.evaluate_click("user-0001", "category-099").unwrap());
            let ca = oracle_a.evaluate_click("user-0001", "category-001").unwrap();
            let cb = oracle_b.evaluate_click("user-0001", "category-001").unwrap();
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
    fn spec_reports_user_activity_oracle_constants() {
        let spec = user_activity_oracle_spec();
        assert_eq!(spec.crate_name, "world-generator");
        assert_eq!(spec.module_name, "user_activity_oracle");
        assert_eq!(spec.interest_count, INTEREST_COUNT);
        assert_eq!(spec.click_rng_stream, CLICK_RNG_STREAM);
        assert_eq!(spec.preference_rng_stream, PREFERENCE_RNG_STREAM);
        assert_eq!(spec.max_click_probability, MAX_CLICK_PROBABILITY);
    }
}
