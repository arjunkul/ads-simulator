//! World generator for the ads marketplace simulator.
//!
//! Encapsulates all synthetic world generation and shopper behavior modeling:
//! - [`marketplace`]: deterministic 10x10 product taxonomy, advertisers, catalogs, organic items,
//!   shoppers, and organic browsing events.
//! - [`timestamp`]: PST diurnal traffic timestamp distribution (`sample_pst_second_of_day`).
//! - [`visit_generator`]: preference-independent shopper visit arrivals (`VisitGenerator`) driving
//!   ad-server slot requests (`AdServingTarget`).
//! - [`user_activity_oracle`]: visitor preferred categories (`UserPreferenceProfile`,
//!   `score_preference`) and the click oracle (`UserActivityOracle`).

pub mod marketplace;
mod taxonomy;
pub mod timestamp;
pub mod user_activity_oracle;
pub mod visit_generator;

pub use marketplace::{
    ADVERTISER_COUNT, AFFINITY_VIEWS, Advertiser, BrowseReason, BrowsingEvent, CATALOG_SIZE,
    CATEGORIES_PER_GROUP, CURRENCY, Category, DEFAULT_SEED, EXPLORATION_VIEWS, Item, Marketplace,
    MarketplaceSpec, ORGANIC_PER_CATEGORY, SCHEMA_VERSION, TAXONOMY_GROUP_COUNT, USER_COUNT, User,
    generate_marketplace, marketplace_spec, taxonomy,
};
pub use timestamp::{
    HOURS_PER_DAY, PstTimestamp, SECONDS_PER_DAY, SECONDS_PER_HOUR, TIMEZONE_LABEL,
    sample_pst_second_of_day,
};
pub use user_activity_oracle::{
    CLICK_RNG_STREAM, INTEREST_COUNT, MAX_CLICK_PROBABILITY, PREFERENCE_RNG_STREAM,
    UserActivityOracle, UserActivityOracleSpec, UserPreferenceProfile, generate_user_preferences,
    score_preference, user_activity_oracle_spec,
};
pub use visit_generator::{
    AdServingTarget, DEFAULT_DAILY_REQUESTS, DEFAULT_DAYS, DEFAULT_SEED as VISIT_DEFAULT_SEED,
    DayGenerationStats, GeneratedImpression, RNG_STREAM, VisitGenerator, VisitGeneratorConfig,
    VisitGeneratorSpec, visit_generator_spec,
};
