//! User preference profiles and visitor click oracle (`UserActivityOracle`).
//!
//! Models how visiting shoppers respond to ads in the simulated world:
//! 1. **User preferred categories (`INTEREST_COUNT`, `PREFERENCE_RNG_STREAM`)**: assigns each
//!    visitor a unique set of preferred product categories ([`UserPreferenceProfile`]) and exposes
//!    [`score_preference`].
//! 2. **User activity oracle ([`UserActivityOracle`], `CLICK_RNG_STREAM`)**: supports selectable
//!    world models ([`WorldModel`]):
//!    - [`WorldModel::V0`]: binary category-match Bernoulli oracle (`MAX_CLICK_PROBABILITY` = 4%
//!      if the ad's category is among the user's preferred categories, `0%` otherwise).
//!    - [`WorldModel::V1`]: hierarchical noisy logit oracle injecting static CTR shifts and
//!      varying per-impression stochastic noise across each retail group, category, and
//!      `(advertiser, category)` combo, with non-zero background CTR on non-preferred categories.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::{Error, ErrorKind};
use std::str::FromStr;

use rand::seq::index;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};

use crate::marketplace::{Advertiser, Category, Item, Marketplace};

/// Number of distinct preferred categories assigned to each visitor.
pub const INTEREST_COUNT: usize = 5;
/// ChaCha8 RNG stream for the visitor [`UserActivityOracle`] click & noise draws.
pub const CLICK_RNG_STREAM: u64 = 2;
/// ChaCha8 RNG stream for generating visitor preferred categories.
pub const PREFERENCE_RNG_STREAM: u64 = 3;
/// ChaCha8 RNG stream for generating `world-model-v1` hierarchical shift and noise parameters.
pub const WORLD_MODEL_RNG_STREAM: u64 = 4;
/// Click probability when a served ad matches one of the visiting user's preferred categories.
pub const MAX_CLICK_PROBABILITY: f64 = 0.04;
/// Default baseline click probability for non-preferred categories under [`WorldModel::V1`].
pub const V1_NON_PREFERRED_BASE_CTR: f64 = 0.002;
/// Default world model used when none is specified (`world-model-v0` preserves baselines).
pub const DEFAULT_WORLD_MODEL: WorldModel = WorldModel::V0;

// Hierarchical logit-space shift std-devs and per-impression noise std-dev bounds for world-model v1.
const V1_GROUP_SHIFT_STD: f64 = 0.25;
const V1_GROUP_NOISE_MIN: f64 = 0.05;
const V1_GROUP_NOISE_MAX: f64 = 0.30;

const V1_CATEGORY_SHIFT_STD: f64 = 0.35;
const V1_CATEGORY_NOISE_MIN: f64 = 0.05;
const V1_CATEGORY_NOISE_MAX: f64 = 0.40;

const V1_ADVERTISER_SHIFT_STD: f64 = 0.20;
const V1_ADVERTISER_NOISE_MIN: f64 = 0.05;
const V1_ADVERTISER_NOISE_MAX: f64 = 0.25;

const V1_COMBO_SHIFT_STD: f64 = 0.30;
const V1_COMBO_NOISE_MIN: f64 = 0.05;
const V1_COMBO_NOISE_MAX: f64 = 0.45;

/// Click-oracle world model version governing shopper click probabilities and noise injection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WorldModel {
    /// `world-model-v0`: Binary category-match oracle (`4%` preferred / `0%` non-preferred).
    #[default]
    #[serde(
        rename = "world-model-v0",
        alias = "v0",
        alias = "V0",
        alias = "world_model_v0",
        alias = "world-model v0"
    )]
    V0,
    /// `world-model-v1`: Hierarchical logit oracle with per-(group, category, advertiser) static
    /// CTR shifts, varying per-impression stochastic noise, and noisy non-preferred background CTR.
    #[serde(
        rename = "world-model-v1",
        alias = "v1",
        alias = "V1",
        alias = "world_model_v1",
        alias = "world-model v1"
    )]
    V1,
}

impl WorldModel {
    /// All registered world models in version order.
    pub const fn all() -> &'static [WorldModel] {
        &[WorldModel::V0, WorldModel::V1]
    }

    /// Canonical identifier (`"world-model-v0"` or `"world-model-v1"`).
    pub const fn as_str(self) -> &'static str {
        match self {
            WorldModel::V0 => "world-model-v0",
            WorldModel::V1 => "world-model-v1",
        }
    }

    /// Short human-readable name.
    pub const fn name(self) -> &'static str {
        match self {
            WorldModel::V0 => "World Model v0 (Binary Category Oracle)",
            WorldModel::V1 => "World Model v1 (Noisy Hierarchical Oracle)",
        }
    }

    /// Human-readable description of the oracle behavior.
    pub const fn description(self) -> &'static str {
        match self {
            WorldModel::V0 => {
                "Binary category-match Bernoulli click model: 4.0% on user preferred categories and 0.0% otherwise"
            }
            WorldModel::V1 => {
                "Hierarchical logit click model injecting static CTR shifts and varying per-impression stochastic noise per retail group, category, and advertiser combo, with non-zero background CTR on non-preferred categories"
            }
        }
    }

    /// Concise mathematical formula for dashboards and documentation.
    pub const fn formula(self) -> &'static str {
        match self {
            WorldModel::V0 => "p(click | u, c) = 0.04 if c in prefs(u) else 0.0",
            WorldModel::V1 => {
                "p(click | u, g, c, a) = sigmoid(logit(p_base(u, c)) + mu_{g,c,a} + sigma_{g,c,a} * N(0,1))"
            }
        }
    }
}

impl fmt::Display for WorldModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for WorldModel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let normalized = s.trim().to_ascii_lowercase().replace(['_', ' '], "-");
        match normalized.as_str() {
            "v0" | "world-model-v0" | "binary" => Ok(WorldModel::V0),
            "v1" | "world-model-v1" | "noisy" => Ok(WorldModel::V1),
            other => Err(format!(
                "unknown world model '{other}' (supported: world-model-v0/v0, world-model-v1/v1)"
            )),
        }
    }
}

/// Metadata describing a registered click-oracle [`WorldModel`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldModelInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub formula: String,
}

/// Compiled specification of the `user_activity_oracle` module inside `world-generator`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UserActivityOracleSpec {
    pub crate_name: String,
    pub module_name: String,
    pub version: String,
    pub default_world_model: WorldModel,
    pub supported_world_models: Vec<WorldModelInfo>,
    pub interest_count: usize,
    pub click_rng_stream: u64,
    pub preference_rng_stream: u64,
    pub world_model_rng_stream: u64,
    pub max_click_probability: f64,
    pub v1_non_preferred_base_ctr: f64,
}

/// Return the live compiled specification of the `user_activity_oracle` module.
pub fn user_activity_oracle_spec() -> UserActivityOracleSpec {
    UserActivityOracleSpec {
        crate_name: env!("CARGO_PKG_NAME").to_owned(),
        module_name: "user_activity_oracle".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        default_world_model: DEFAULT_WORLD_MODEL,
        supported_world_models: WorldModel::all()
            .iter()
            .map(|&wm| WorldModelInfo {
                id: wm.as_str().to_owned(),
                name: wm.name().to_owned(),
                description: wm.description().to_owned(),
                formula: wm.formula().to_owned(),
            })
            .collect(),
        interest_count: INTEREST_COUNT,
        click_rng_stream: CLICK_RNG_STREAM,
        preference_rng_stream: PREFERENCE_RNG_STREAM,
        world_model_rng_stream: WORLD_MODEL_RNG_STREAM,
        max_click_probability: MAX_CLICK_PROBABILITY,
        v1_non_preferred_base_ctr: V1_NON_PREFERRED_BASE_CTR,
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

/// Inspectable breakdown of `world-model-v1` static logit shifts and per-impression noise std-devs
/// for a `(retail group, category, advertiser)` combination.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ComboNoiseProfile {
    pub group: String,
    pub category_id: String,
    pub advertiser_id: String,
    pub group_shift: f64,
    pub category_shift: f64,
    pub advertiser_shift: f64,
    pub combo_shift: f64,
    pub total_shift: f64,
    pub group_noise_std: f64,
    pub category_noise_std: f64,
    pub advertiser_noise_std: f64,
    pub combo_noise_std: f64,
    pub total_noise_std: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct EffectPair {
    shift: f64,
    noise_std: f64,
}

/// Deterministic hierarchical static-shift and noise-variance tables for [`WorldModel::V1`].
#[derive(Clone, Debug)]
pub struct WorldModelV1Params {
    category_to_group: HashMap<String, String>,
    group_effects: HashMap<String, EffectPair>,
    category_effects: HashMap<String, EffectPair>,
    advertiser_effects: HashMap<String, EffectPair>,
    combo_effects: HashMap<(String, String), EffectPair>,
}

impl WorldModelV1Params {
    /// Generate deterministic `world-model-v1` shift and noise tables across all retail groups,
    /// categories, advertisers, and `(advertiser, category)` catalog combos on
    /// [`WORLD_MODEL_RNG_STREAM`] (Stream 4).
    pub fn from_categories_and_advertisers(
        categories: &[Category],
        advertisers: &[Advertiser],
        seed: u64,
    ) -> Self {
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        rng.set_stream(WORLD_MODEL_RNG_STREAM);

        let mut group_order = Vec::new();
        let mut seen_groups = HashSet::new();
        let mut category_to_group = HashMap::with_capacity(categories.len());
        for cat in categories {
            category_to_group.insert(cat.id.clone(), cat.group.clone());
            if seen_groups.insert(cat.group.as_str()) {
                group_order.push(cat.group.clone());
            }
        }

        let mut group_effects = HashMap::with_capacity(group_order.len());
        for group in &group_order {
            let shift = sample_standard_normal(&mut rng) * V1_GROUP_SHIFT_STD;
            let noise_std = rng.random_range(V1_GROUP_NOISE_MIN..=V1_GROUP_NOISE_MAX);
            group_effects.insert(group.clone(), EffectPair { shift, noise_std });
        }

        let mut category_effects = HashMap::with_capacity(categories.len());
        for cat in categories {
            let shift = sample_standard_normal(&mut rng) * V1_CATEGORY_SHIFT_STD;
            let noise_std = rng.random_range(V1_CATEGORY_NOISE_MIN..=V1_CATEGORY_NOISE_MAX);
            category_effects.insert(cat.id.clone(), EffectPair { shift, noise_std });
        }

        let mut advertiser_effects = HashMap::with_capacity(advertisers.len());
        let mut combo_effects = HashMap::new();
        for adv in advertisers {
            let adv_shift = sample_standard_normal(&mut rng) * V1_ADVERTISER_SHIFT_STD;
            let adv_noise_std = rng.random_range(V1_ADVERTISER_NOISE_MIN..=V1_ADVERTISER_NOISE_MAX);
            advertiser_effects.insert(
                adv.id.clone(),
                EffectPair {
                    shift: adv_shift,
                    noise_std: adv_noise_std,
                },
            );

            let mut seen_cats = HashSet::new();
            for item in &adv.catalog {
                if seen_cats.insert(item.category_id.as_str()) {
                    let shift = sample_standard_normal(&mut rng) * V1_COMBO_SHIFT_STD;
                    let noise_std = rng.random_range(V1_COMBO_NOISE_MIN..=V1_COMBO_NOISE_MAX);
                    combo_effects.insert(
                        (adv.id.clone(), item.category_id.clone()),
                        EffectPair { shift, noise_std },
                    );
                }
            }
        }

        Self {
            category_to_group,
            group_effects,
            category_effects,
            advertiser_effects,
            combo_effects,
        }
    }

    /// Generate `world-model-v1` tables directly from a [`Marketplace`].
    pub fn from_marketplace(marketplace: &Marketplace, seed: u64) -> Self {
        Self::from_categories_and_advertisers(&marketplace.categories, &marketplace.advertisers, seed)
    }

    /// Generate `world-model-v1` tables from category IDs alone (grouping every 10 categories into
    /// synthetic retail groups when full marketplace metadata is unavailable).
    pub fn from_category_ids(category_ids: &[String], seed: u64) -> Self {
        let synthetic_categories: Vec<Category> = category_ids
            .iter()
            .enumerate()
            .map(|(idx, id)| Category {
                id: id.clone(),
                group: format!("group-{:02}", (idx / 10) + 1),
                name: id.clone(),
            })
            .collect();
        Self::from_categories_and_advertisers(&synthetic_categories, &[], seed)
    }

    /// Inspect the decomposed static shift and noise std-dev for a `(category_id, advertiser_id)` combo.
    pub fn combo_profile(&self, category_id: &str, advertiser_id: &str) -> ComboNoiseProfile {
        let group = self
            .category_to_group
            .get(category_id)
            .cloned()
            .unwrap_or_else(|| "unknown".to_owned());
        let g = self
            .group_effects
            .get(&group)
            .copied()
            .unwrap_or(EffectPair {
                shift: 0.0,
                noise_std: 0.0,
            });
        let c = self
            .category_effects
            .get(category_id)
            .copied()
            .unwrap_or(EffectPair {
                shift: 0.0,
                noise_std: 0.0,
            });
        let a = self
            .advertiser_effects
            .get(advertiser_id)
            .copied()
            .unwrap_or(EffectPair {
                shift: 0.0,
                noise_std: 0.0,
            });
        let ac = self
            .combo_effects
            .get(&(advertiser_id.to_owned(), category_id.to_owned()))
            .copied()
            .unwrap_or(EffectPair {
                shift: 0.0,
                noise_std: 0.0,
            });

        let total_shift = g.shift + c.shift + a.shift + ac.shift;
        let total_noise_std = (g.noise_std * g.noise_std
            + c.noise_std * c.noise_std
            + a.noise_std * a.noise_std
            + ac.noise_std * ac.noise_std)
            .sqrt();

        ComboNoiseProfile {
            group,
            category_id: category_id.to_owned(),
            advertiser_id: advertiser_id.to_owned(),
            group_shift: g.shift,
            category_shift: c.shift,
            advertiser_shift: a.shift,
            combo_shift: ac.shift,
            total_shift,
            group_noise_std: g.noise_std,
            category_noise_std: c.noise_std,
            advertiser_noise_std: a.noise_std,
            combo_noise_std: ac.noise_std,
            total_noise_std,
        }
    }

    #[inline]
    fn combined_effect(&self, category_id: &str, advertiser_id: &str) -> EffectPair {
        let mut shift = 0.0;
        let mut noise_var = 0.0;

        if let Some(group) = self.category_to_group.get(category_id)
            && let Some(e) = self.group_effects.get(group)
        {
            shift += e.shift;
            noise_var += e.noise_std * e.noise_std;
        }
        if let Some(e) = self.category_effects.get(category_id) {
            shift += e.shift;
            noise_var += e.noise_std * e.noise_std;
        }
        if !advertiser_id.is_empty() {
            if let Some(e) = self.advertiser_effects.get(advertiser_id) {
                shift += e.shift;
                noise_var += e.noise_std * e.noise_std;
            }
            // Fast lookup via temporary key only when advertiser_id is present and combos exist.
            if !self.combo_effects.is_empty()
                && let Some(e) = self
                    .combo_effects
                    .get(&(advertiser_id.to_owned(), category_id.to_owned()))
            {
                shift += e.shift;
                noise_var += e.noise_std * e.noise_std;
            }
        }

        EffectPair {
            shift,
            noise_std: noise_var.sqrt(),
        }
    }
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message.into())
}

#[inline]
fn logit(p: f64) -> f64 {
    let clamped = p.clamp(1e-9, 1.0 - 1e-9);
    (clamped / (1.0 - clamped)).ln()
}

#[inline]
fn sigmoid(z: f64) -> f64 {
    if z >= 0.0 {
        1.0 / (1.0 + (-z).exp())
    } else {
        let ez = z.exp();
        ez / (1.0 + ez)
    }
}

#[inline]
fn sample_standard_normal(rng: &mut impl Rng) -> f64 {
    let u1 = (1.0 - rng.random::<f64>()).max(f64::MIN_POSITIVE);
    let u2 = rng.random::<f64>();
    (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
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
/// (`PREFERENCE_RNG_STREAM`), world-model parameter generation (`WORLD_MODEL_RNG_STREAM`), and
/// ad-server ranking (Stream 0).
///
/// - Under [`WorldModel::V0`], an ad in one of the user's preferred categories clicks with
///   probability `max_click_probability` (default `4%`), and `0%` otherwise.
/// - Under [`WorldModel::V1`], each retail group, category, and `(advertiser, category)` combo
///   has a seeded static logit shift and noise std-dev; every impression draws fresh Gaussian
///   logit noise scaled by that combo's noise level, and non-preferred categories have a noisy
///   non-zero background CTR (`V1_NON_PREFERRED_BASE_CTR` = `0.2%`).
pub struct UserActivityOracle {
    profiles: Vec<UserPreferenceProfile>,
    user_index: HashMap<String, usize>,
    max_click_probability: f64,
    world_model: WorldModel,
    v1_params: Option<WorldModelV1Params>,
    rng: ChaCha8Rng,
}

impl UserActivityOracle {
    /// Construct a new [`UserActivityOracle`] under [`WorldModel::V0`] with
    /// [`MAX_CLICK_PROBABILITY`] (`4%` preferred / `0%` else).
    pub fn new(profiles: Vec<UserPreferenceProfile>, seed: u64) -> Result<Self, Error> {
        Self::with_max_click_probability(profiles, seed, MAX_CLICK_PROBABILITY)
    }

    /// Construct a new [`UserActivityOracle`] under [`WorldModel::V0`] with an explicit
    /// preferred-category click probability.
    pub fn with_max_click_probability(
        profiles: Vec<UserPreferenceProfile>,
        seed: u64,
        max_click_probability: f64,
    ) -> Result<Self, Error> {
        Self::with_world_model(profiles, seed, max_click_probability, WorldModel::V0, None)
    }

    /// Construct a new [`UserActivityOracle`] with an explicit [`WorldModel`] and optional
    /// precomputed [`WorldModelV1Params`].
    pub fn with_world_model(
        profiles: Vec<UserPreferenceProfile>,
        seed: u64,
        max_click_probability: f64,
        world_model: WorldModel,
        v1_params: Option<WorldModelV1Params>,
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

        let resolved_v1_params = match world_model {
            WorldModel::V0 => None,
            WorldModel::V1 => Some(v1_params.unwrap_or_else(|| {
                let mut cats: Vec<String> = profiles
                    .iter()
                    .flat_map(|p| p.preferred_category_ids.iter().cloned())
                    .collect();
                cats.sort();
                cats.dedup();
                WorldModelV1Params::from_category_ids(&cats, seed)
            })),
        };

        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        rng.set_stream(CLICK_RNG_STREAM);

        Ok(Self {
            profiles,
            user_index,
            max_click_probability,
            world_model,
            v1_params: resolved_v1_params,
            rng,
        })
    }

    /// Generate visitor preferred categories from `user_ids` and `category_ids` and build a
    /// [`UserActivityOracle`] under [`WorldModel::V0`].
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

    /// Build a [`UserActivityOracle`] directly from a [`Marketplace`] using the requested
    /// [`WorldModel`]. When [`WorldModel::V1`] is selected, hierarchical shift and noise tables
    /// are seeded from `preference_seed` across all retail groups, categories, and advertisers.
    pub fn from_marketplace(
        marketplace: &Marketplace,
        preference_seed: u64,
        click_seed: u64,
        max_click_probability: f64,
        world_model: WorldModel,
    ) -> Result<Self, Error> {
        let user_ids: Vec<String> = marketplace.users.iter().map(|u| u.id.clone()).collect();
        let category_ids: Vec<String> = marketplace
            .categories
            .iter()
            .map(|c| c.id.clone())
            .collect();
        let profiles = generate_user_preferences(&user_ids, &category_ids, preference_seed)?;
        let v1_params = match world_model {
            WorldModel::V0 => None,
            WorldModel::V1 => Some(WorldModelV1Params::from_marketplace(
                marketplace,
                preference_seed,
            )),
        };
        Self::with_world_model(
            profiles,
            click_seed,
            max_click_probability,
            world_model,
            v1_params,
        )
    }

    /// Active [`WorldModel`] governing this oracle.
    pub fn world_model(&self) -> WorldModel {
        self.world_model
    }

    /// Inspect the `world-model-v1` shift and noise breakdown for `(category_id, advertiser_id)`,
    /// or `None` when running under [`WorldModel::V0`].
    pub fn combo_noise_profile(
        &self,
        category_id: &str,
        advertiser_id: &str,
    ) -> Option<ComboNoiseProfile> {
        self.v1_params
            .as_ref()
            .map(|params| params.combo_profile(category_id, advertiser_id))
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

    /// Configured base click probability for ads in a user's preferred category.
    pub fn max_click_probability(&self) -> f64 {
        self.max_click_probability
    }

    /// Deterministic base/median click probability for `user_id` on an ad in `category_id`
    /// (without advertiser-specific shift).
    pub fn click_probability(&self, user_id: &str, category_id: &str) -> Result<f64, Error> {
        self.click_probability_for_ad(user_id, category_id, "")
    }

    /// Deterministic base/median click probability for `user_id` on an ad from `advertiser_id`
    /// in `category_id` (before per-impression stochastic noise under [`WorldModel::V1`]).
    pub fn click_probability_for_ad(
        &self,
        user_id: &str,
        category_id: &str,
        advertiser_id: &str,
    ) -> Result<f64, Error> {
        let &idx = self
            .user_index
            .get(user_id)
            .ok_or_else(|| invalid(format!("unknown user ID {user_id}")))?;
        Ok(self.median_click_probability_for_index(idx, category_id, advertiser_id))
    }

    /// Sample whether `user_id` clicks an ad in `category_id` using this oracle's isolated click
    /// RNG stream (with empty advertiser context).
    pub fn evaluate_click(&mut self, user_id: &str, category_id: &str) -> Result<bool, Error> {
        self.evaluate_ad_click(user_id, category_id, "")
    }

    /// Sample whether `user_id` clicks an ad from `advertiser_id` in `category_id` using this
    /// oracle's isolated click RNG stream.
    pub fn evaluate_ad_click(
        &mut self,
        user_id: &str,
        category_id: &str,
        advertiser_id: &str,
    ) -> Result<bool, Error> {
        let &idx = self
            .user_index
            .get(user_id)
            .ok_or_else(|| invalid(format!("unknown user ID {user_id}")))?;
        Ok(self.evaluate_click_for_index(idx, category_id, advertiser_id))
    }

    /// Sample whether `user_id` clicks catalog `item` via [`score_preference`] (under `V0`) or
    /// the active world model.
    pub fn evaluate_item_click(&mut self, user_id: &str, item: &Item) -> Result<bool, Error> {
        let &idx = self
            .user_index
            .get(user_id)
            .ok_or_else(|| invalid(format!("unknown user ID {user_id}")))?;
        match self.world_model {
            WorldModel::V0 => {
                let score = score_preference(&self.profiles[idx], item);
                Ok(self.rng.random_bool(self.max_click_probability * score))
            }
            WorldModel::V1 => Ok(self.evaluate_click_for_index(idx, &item.category_id, "")),
        }
    }

    pub(crate) fn user_count(&self) -> usize {
        self.profiles.len()
    }

    pub(crate) fn user_id(&self, idx: usize) -> &str {
        self.profiles[idx].user_id.as_str()
    }

    fn is_preferred(&self, idx: usize, category_id: &str) -> bool {
        self.profiles[idx]
            .preferred_category_ids
            .iter()
            .any(|preferred| preferred == category_id)
    }

    fn v1_base_probability(&self, preferred: bool) -> f64 {
        if self.max_click_probability <= 0.0 {
            return 0.0;
        }
        if preferred {
            self.max_click_probability
        } else {
            (V1_NON_PREFERRED_BASE_CTR * (self.max_click_probability / MAX_CLICK_PROBABILITY))
                .clamp(0.0, 1.0)
        }
    }

    fn median_click_probability_for_index(
        &self,
        idx: usize,
        category_id: &str,
        advertiser_id: &str,
    ) -> f64 {
        let preferred = self.is_preferred(idx, category_id);
        match self.world_model {
            WorldModel::V0 => {
                if preferred {
                    self.max_click_probability
                } else {
                    0.0
                }
            }
            WorldModel::V1 => {
                let base_p = self.v1_base_probability(preferred);
                if base_p <= 0.0 {
                    return 0.0;
                }
                if base_p >= 1.0 {
                    return 1.0;
                }
                let effect = self
                    .v1_params
                    .as_ref()
                    .map(|p| p.combined_effect(category_id, advertiser_id))
                    .unwrap_or(EffectPair {
                        shift: 0.0,
                        noise_std: 0.0,
                    });
                sigmoid(logit(base_p) + effect.shift)
            }
        }
    }

    /// Sample the per-impression click probability (with stochastic noise applied under
    /// [`WorldModel::V1`]) for testing and diagnostics.
    pub fn sample_impression_click_probability(
        &mut self,
        user_id: &str,
        category_id: &str,
        advertiser_id: &str,
    ) -> Result<f64, Error> {
        let &idx = self
            .user_index
            .get(user_id)
            .ok_or_else(|| invalid(format!("unknown user ID {user_id}")))?;
        Ok(self.draw_impression_probability(idx, category_id, advertiser_id))
    }

    #[inline]
    fn draw_impression_probability(
        &mut self,
        idx: usize,
        category_id: &str,
        advertiser_id: &str,
    ) -> f64 {
        let preferred = self.is_preferred(idx, category_id);
        match self.world_model {
            WorldModel::V0 => {
                if preferred {
                    self.max_click_probability
                } else {
                    0.0
                }
            }
            WorldModel::V1 => {
                let base_p = self.v1_base_probability(preferred);
                if base_p <= 0.0 {
                    return 0.0;
                }
                if base_p >= 1.0 {
                    return 1.0;
                }
                let effect = self
                    .v1_params
                    .as_ref()
                    .map(|p| p.combined_effect(category_id, advertiser_id))
                    .unwrap_or(EffectPair {
                        shift: 0.0,
                        noise_std: 0.0,
                    });
                let eps = sample_standard_normal(&mut self.rng);
                sigmoid(logit(base_p) + effect.shift + effect.noise_std * eps)
            }
        }
    }

    pub(crate) fn evaluate_click_for_index(
        &mut self,
        idx: usize,
        category_id: &str,
        advertiser_id: &str,
    ) -> bool {
        let prob = self.draw_impression_probability(idx, category_id, advertiser_id);
        self.rng.random_bool(prob)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::str::FromStr;

    use crate::marketplace::{Item, generate_marketplace};
    use crate::user_activity_oracle::{
        CLICK_RNG_STREAM, DEFAULT_WORLD_MODEL, INTEREST_COUNT, MAX_CLICK_PROBABILITY,
        PREFERENCE_RNG_STREAM, UserActivityOracle, UserPreferenceProfile,
        V1_NON_PREFERRED_BASE_CTR, WORLD_MODEL_RNG_STREAM, WorldModel, generate_user_preferences,
        score_preference, user_activity_oracle_spec,
    };

    #[test]
    fn world_model_parsing_display_and_json_round_trip() {
        for (raw, expected) in [
            ("v0", WorldModel::V0),
            ("V0", WorldModel::V0),
            ("world-model-v0", WorldModel::V0),
            ("world-model v0", WorldModel::V0),
            ("v1", WorldModel::V1),
            ("V1", WorldModel::V1),
            ("world-model-v1", WorldModel::V1),
            ("world-model v1", WorldModel::V1),
        ] {
            assert_eq!(WorldModel::from_str(raw).unwrap(), expected);
        }
        assert!(WorldModel::from_str("v99").is_err());
        assert_eq!(WorldModel::V0.to_string(), "world-model-v0");
        assert_eq!(WorldModel::V1.to_string(), "world-model-v1");
        assert_eq!(WorldModel::default(), DEFAULT_WORLD_MODEL);

        for wm in WorldModel::all() {
            let json = serde_json::to_string(wm).unwrap();
            let parsed: WorldModel = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, *wm);
        }
        assert_eq!(
            serde_json::from_str::<WorldModel>("\"v1\"").unwrap(),
            WorldModel::V1
        );
    }

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
    fn user_activity_oracle_v0_clicks_only_preferred_categories_and_is_deterministic() {
        let profiles = vec![UserPreferenceProfile {
            user_id: "user-0001".to_owned(),
            preferred_category_ids: vec!["category-001".to_owned()],
        }];

        let mut oracle_a = UserActivityOracle::new(profiles.clone(), 42).unwrap();
        let mut oracle_b = UserActivityOracle::new(profiles, 42).unwrap();
        assert_eq!(oracle_a.world_model(), WorldModel::V0);
        assert!(
            oracle_a
                .combo_noise_profile("category-001", "advertiser-01")
                .is_none()
        );
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
    fn world_model_v1_varies_shifts_and_noise_across_groups_categories_and_advertisers() {
        let mp = generate_marketplace(42);
        let mut oracle_a = UserActivityOracle::from_marketplace(
            &mp,
            42,
            42,
            MAX_CLICK_PROBABILITY,
            WorldModel::V1,
        )
        .unwrap();
        let mut oracle_b = UserActivityOracle::from_marketplace(
            &mp,
            42,
            42,
            MAX_CLICK_PROBABILITY,
            WorldModel::V1,
        )
        .unwrap();
        let oracle_diff_seed = UserActivityOracle::from_marketplace(
            &mp,
            99,
            99,
            MAX_CLICK_PROBABILITY,
            WorldModel::V1,
        )
        .unwrap();

        assert_eq!(oracle_a.world_model(), WorldModel::V1);

        // Two advertisers in the same specialty (Electronics: advertiser-01 and advertiser-11)
        // share category-001 (Phones), while advertiser-02 is in Home (category-011).
        let p_elec_a1_c1 = oracle_a
            .combo_noise_profile("category-001", "advertiser-01")
            .unwrap();
        let p_elec_a11_c1 = oracle_a
            .combo_noise_profile("category-001", "advertiser-11")
            .unwrap();
        let p_elec_a1_c2 = oracle_a
            .combo_noise_profile("category-002", "advertiser-01")
            .unwrap();
        let p_home_a2_c11 = oracle_a
            .combo_noise_profile("category-011", "advertiser-02")
            .unwrap();

        // Same retail group shares group effect; different retail groups differ.
        assert_eq!(p_elec_a1_c1.group, "Electronics");
        assert_eq!(p_home_a2_c11.group, "Home");
        assert_eq!(p_elec_a1_c1.group_shift, p_elec_a1_c2.group_shift);
        assert_eq!(p_elec_a1_c1.group_noise_std, p_elec_a1_c2.group_noise_std);
        assert_ne!(p_elec_a1_c1.group_shift, p_home_a2_c11.group_shift);
        assert_ne!(p_elec_a1_c1.group_noise_std, p_home_a2_c11.group_noise_std);

        // Same category across different advertisers shares category effect, but advertiser + combo differ.
        assert_eq!(p_elec_a1_c1.category_shift, p_elec_a11_c1.category_shift);
        assert_ne!(p_elec_a1_c1.advertiser_shift, p_elec_a11_c1.advertiser_shift);
        assert_ne!(p_elec_a1_c1.combo_shift, p_elec_a11_c1.combo_shift);
        assert_ne!(p_elec_a1_c1.total_noise_std, p_elec_a11_c1.total_noise_std);

        // Same advertiser across different categories shares advertiser effect, but category + combo differ.
        assert_eq!(p_elec_a1_c1.advertiser_shift, p_elec_a1_c2.advertiser_shift);
        assert_ne!(p_elec_a1_c1.category_shift, p_elec_a1_c2.category_shift);
        assert_ne!(p_elec_a1_c1.combo_shift, p_elec_a1_c2.combo_shift);

        // Different seed produces different noise profile.
        let p_other_seed = oracle_diff_seed
            .combo_noise_profile("category-001", "advertiser-01")
            .unwrap();
        assert_ne!(p_elec_a1_c1.total_shift, p_other_seed.total_shift);
        assert_ne!(p_elec_a1_c1.total_noise_std, p_other_seed.total_noise_std);

        // Per-impression stochastic noise produces varying probabilities for the same (user, cat, adv)
        // and is deterministic across identical seeds.
        let pref_cat = oracle_a
            .preferred_categories("user-0001")
            .unwrap()[0]
            .clone();
        let non_pref_cat = mp
            .categories
            .iter()
            .find(|c| {
                !oracle_a
                    .preferred_categories("user-0001")
                    .unwrap()
                    .contains(&c.id)
            })
            .unwrap()
            .id
            .clone();
        let pref_adv = mp
            .advertisers
            .iter()
            .find(|a| a.catalog.iter().any(|it| it.category_id == pref_cat))
            .unwrap()
            .id
            .clone();
        let non_pref_adv = mp
            .advertisers
            .iter()
            .find(|a| a.catalog.iter().any(|it| it.category_id == non_pref_cat))
            .unwrap()
            .id
            .clone();

        let mut sampled_probs = Vec::with_capacity(200);
        for _ in 0..200 {
            let pa = oracle_a
                .sample_impression_click_probability("user-0001", &pref_cat, &pref_adv)
                .unwrap();
            let pb = oracle_b
                .sample_impression_click_probability("user-0001", &pref_cat, &pref_adv)
                .unwrap();
            assert_eq!(pa, pb);
            assert!(pa > 0.0 && pa < 1.0);
            sampled_probs.push(pa);
        }
        let min_p = sampled_probs.iter().copied().fold(f64::INFINITY, f64::min);
        let max_p = sampled_probs
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(
            max_p - min_p > 0.005,
            "per-impression noise must vary click probabilities across impressions (min={min_p}, max={max_p})"
        );

        // Non-preferred categories have non-zero noisy CTR, still significantly below preferred CTR.
        let pref_median = oracle_a
            .click_probability_for_ad("user-0001", &pref_cat, &pref_adv)
            .unwrap();
        let non_pref_median = oracle_a
            .click_probability_for_ad("user-0001", &non_pref_cat, &non_pref_adv)
            .unwrap();
        assert!(non_pref_median > 0.0 && non_pref_median < pref_median);

        let mut non_pref_clicks = 0u64;
        let mut pref_clicks = 0u64;
        for _ in 0..10_000 {
            if oracle_a
                .evaluate_ad_click("user-0001", &non_pref_cat, &non_pref_adv)
                .unwrap()
            {
                non_pref_clicks += 1;
            }
            if oracle_a
                .evaluate_ad_click("user-0001", &pref_cat, &pref_adv)
                .unwrap()
            {
                pref_clicks += 1;
            }
        }
        assert!(
            non_pref_clicks > 0,
            "world-model-v1 must allow non-zero noisy clicks on non-preferred categories"
        );
        assert!(
            pref_clicks > non_pref_clicks * 2,
            "preferred clicks ({pref_clicks}) should substantially exceed non-preferred clicks ({non_pref_clicks})"
        );

        // Zero max_click_probability still produces strictly zero clicks under v1.
        let mut zero_oracle =
            UserActivityOracle::from_marketplace(&mp, 42, 42, 0.0, WorldModel::V1).unwrap();
        for _ in 0..500 {
            assert!(
                !zero_oracle
                    .evaluate_ad_click("user-0001", &pref_cat, &pref_adv)
                    .unwrap()
            );
            assert!(
                !zero_oracle
                    .evaluate_ad_click("user-0001", &non_pref_cat, &non_pref_adv)
                    .unwrap()
            );
        }
    }

    #[test]
    fn spec_reports_user_activity_oracle_constants_and_world_models() {
        let spec = user_activity_oracle_spec();
        assert_eq!(spec.crate_name, "world-generator");
        assert_eq!(spec.module_name, "user_activity_oracle");
        assert_eq!(spec.default_world_model, DEFAULT_WORLD_MODEL);
        assert_eq!(spec.supported_world_models.len(), 2);
        assert_eq!(spec.supported_world_models[0].id, "world-model-v0");
        assert_eq!(spec.supported_world_models[1].id, "world-model-v1");
        assert_eq!(spec.interest_count, INTEREST_COUNT);
        assert_eq!(spec.click_rng_stream, CLICK_RNG_STREAM);
        assert_eq!(spec.preference_rng_stream, PREFERENCE_RNG_STREAM);
        assert_eq!(spec.world_model_rng_stream, WORLD_MODEL_RNG_STREAM);
        assert_eq!(spec.max_click_probability, MAX_CLICK_PROBABILITY);
        assert_eq!(spec.v1_non_preferred_base_ctr, V1_NON_PREFERRED_BASE_CTR);
    }
}
