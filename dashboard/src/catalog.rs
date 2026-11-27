//! Live code-derived simulator feature catalog and marketplace inspector.

use ad_server::ad_server_spec;
use ads_marketplace_simulation::simulation_spec;
use serde::{Deserialize, Serialize};
use sim_agent::sim_agent_spec;
use simulation_analyzer::analyzer_spec;
use world_generator::{
    BrowseReason, generate_marketplace, marketplace_spec, taxonomy, user_activity_oracle_spec,
    visit_generator_spec,
};

/// Top-level catalog describing all architectural pieces of the simulator workspace.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SimulatorCatalog {
    pub simulator_version: String,
    pub schema_version: u32,
    pub currency: String,
    pub summary_counts: SummaryCounts,
    pub pipeline_edges: Vec<PipelineEdge>,
    pub pieces: Vec<SimulatorPiece>,
}

/// Aggregated counts across all simulator pieces and baseline marketplace/simulation specs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryCounts {
    pub pieces: usize,
    pub total_features: usize,
    pub implemented_features: usize,
    pub gaps_and_simplifications: usize,
    pub crates: usize,
    pub categories: usize,
    pub advertisers: usize,
    pub users: usize,
    pub campaigns: usize,
    pub simulation_days: usize,
    pub r1_slate_size: usize,
}

/// Directed dataflow edge connecting two simulator pieces.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipelineEdge {
    pub from: String,
    pub to: String,
    pub label: String,
}

/// A compiled parameter surfaced directly from Rust crate constants/specs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PieceParameter {
    pub key: String,
    pub label: String,
    pub value: serde_json::Value,
    pub unit: String,
}

/// A capability implemented by a simulator piece.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PieceFeature {
    pub name: String,
    pub status: String,
    pub description: String,
}

/// Detailed description of one simulator piece.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SimulatorPiece {
    pub id: String,
    pub name: String,
    pub crate_name: String,
    pub stage: String,
    pub source_files: Vec<String>,
    pub summary: String,
    pub parameters: Vec<PieceParameter>,
    pub features: Vec<PieceFeature>,
    pub gaps_and_simplifications: Vec<String>,
}

/// Live marketplace inspection payload returned by `GET /api/marketplace?seed=<i64>`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MarketplaceInspection {
    pub seed: i64,
    pub schema_version: u32,
    pub currency: String,
    pub taxonomy: Vec<TaxonomyGroup>,
    pub counts: MarketplaceCounts,
    pub price_stats: PriceStats,
    pub browsing_split: BrowsingSplit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaxonomyGroup {
    pub group: String,
    pub categories: Vec<TaxonomyCategory>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaxonomyCategory {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketplaceCounts {
    pub categories: usize,
    pub advertisers: usize,
    pub catalog_items: usize,
    pub users: usize,
    pub organic_items: usize,
    pub browsing_events: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PriceStats {
    pub catalog: PriceBucket,
    pub organic: PriceBucket,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PriceBucket {
    pub min_cents: u32,
    pub mean_cents: f64,
    pub max_cents: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowsingSplit {
    pub affinity: usize,
    pub exploration: usize,
}

fn param(
    key: &str,
    label: &str,
    value: impl Into<serde_json::Value>,
    unit: &str,
) -> PieceParameter {
    PieceParameter {
        key: key.to_owned(),
        label: label.to_owned(),
        value: value.into(),
        unit: unit.to_owned(),
    }
}

fn feat(name: &str, description: &str) -> PieceFeature {
    PieceFeature {
        name: name.to_owned(),
        status: "implemented".to_owned(),
        description: description.to_owned(),
    }
}

fn edge(from: &str, to: &str, label: &str) -> PipelineEdge {
    PipelineEdge {
        from: from.to_owned(),
        to: to.to_owned(),
        label: label.to_owned(),
    }
}

/// Construct the live code-derived 12-piece `SimulatorCatalog` from all 7 compiled workspace crates.
pub fn build_catalog() -> SimulatorCatalog {
    let mp = marketplace_spec();
    let srv = ad_server_spec();
    let visit = visit_generator_spec();
    let oracle = user_activity_oracle_spec();
    let sim = simulation_spec();
    let analyzer = analyzer_spec();
    let agent = sim_agent_spec();

    let baseline_campaigns = mp.advertiser_count * srv.campaigns_per_advertiser;
    let baseline_min_total_budget_cents =
        (baseline_campaigns as u64).saturating_mul(srv.default_min_campaign_budget_cents);
    let baseline_max_total_budget_cents =
        (baseline_campaigns as u64).saturating_mul(srv.default_max_campaign_budget_cents);
    let max_ctr_pct = (sim.max_click_probability * 100.0 * 100.0).round() / 100.0;

    let pieces = vec![
        SimulatorPiece {
            id: "taxonomy-schema".into(),
            name: "Product Taxonomy & Schema".into(),
            crate_name: "world-generator".into(),
            stage: "World Generation".into(),
            source_files: vec![
                "world-generator/src/taxonomy.rs".into(),
                "world-generator/src/marketplace.rs".into(),
                "world-generator/src/lib.rs".into(),
            ],
            summary: "Defines the deterministic 10x10 product taxonomy, stable category IDs, USD-cent schema v1 contract, and live MarketplaceSpec metadata.".into(),
            parameters: vec![
                param("schema_version", "Schema Version", mp.schema_version, "version"),
                param("currency", "Currency", mp.currency.clone(), "ISO-4217"),
                param("taxonomy_group_count", "Taxonomy Groups", mp.taxonomy_group_count, "groups"),
                param("categories_per_group", "Categories / Group", mp.categories_per_group, "categories"),
                param("category_count", "Total Categories", mp.category_count, "categories"),
                param("default_seed", "Default Seed", mp.default_seed, "i64"),
            ],
            features: vec![
                feat("Stable 10x10 Taxonomy Ordering", "Assigns deterministic IDs category-001 through category-100 across 10 retail groups."),
                feat("Integer USD-Cent Monetary Representation", "Eliminates floating-point currency rounding drift across all catalog, organic, and simulation ledgers."),
                feat("Compiled Metadata Introspection", "Exposes marketplace_spec() and taxonomy() directly from Rust constants for downstream crates."),
                feat("Serde Schema v1 Compatibility", "Reads both canonical v1 exports and older records with extra user or advertiser attributes."),
            ],
            gaps_and_simplifications: vec![
                "Flat two-level taxonomy (group -> category) without deeper subcategories or cross-category similarity weights.".into(),
                "Single USD currency without multi-currency FX conversion.".into(),
            ],
        },
        SimulatorPiece {
            id: "advertiser-catalog".into(),
            name: "Advertiser & Catalog Generator".into(),
            crate_name: "world-generator".into(),
            stage: "World Generation".into(),
            source_files: vec!["world-generator/src/marketplace.rs".into()],
            summary: "Synthesizes 50 specialty merchants, each carrying 20 catalog offers spanning all 10 categories in its group with randomized pricing and daily budget.".into(),
            parameters: vec![
                param("advertiser_count", "Advertisers", mp.advertiser_count, "merchants"),
                param("catalog_size", "Catalog Items / Advertiser", mp.catalog_size, "items"),
                param("total_catalog_items", "Total Catalog Items", mp.total_catalog_items, "items"),
                param("price_range_cents", "Item Price Range", "$5.00 - $300.00 (500-30000 cents)", "USD cents"),
                param("daily_budget_range_cents", "Advertiser Daily Budget Range", "$10.00 - $500.00 (1000-50000 cents)", "USD cents"),
            ],
            features: vec![
                feat("Full Specialty Coverage Guarantee", "First 10 items of every merchant cover all 10 categories in its specialty before sampling 10 extra items."),
                feat("Deterministic ChaCha8 Sampling", "Reproduces identical merchant assortments, prices, and budgets from any signed 64-bit seed."),
                feat("Distinct Catalog Namespace", "IDs catalog-0001..catalog-1000 are disjoint from organic listing IDs."),
            ],
            gaps_and_simplifications: vec![
                "Advertiser daily_budget_cents is generated in marketplace metadata while campaigns receive randomized 30-day upfront budgets ($1,000-$10,000) in ad-server.".into(),
                "Item prices are uniform integer dollars ($5-$300) rather than category-specific price distributions.".into(),
            ],
        },
        SimulatorPiece {
            id: "organic-inventory".into(),
            name: "Organic Listing Inventory".into(),
            crate_name: "world-generator".into(),
            stage: "World Generation".into(),
            source_files: vec!["world-generator/src/marketplace.rs".into()],
            summary: "Generates 5,000 non-ad organic listings evenly balanced at 50 items per category to supply shopper browsing datasets.".into(),
            parameters: vec![
                param("organic_per_category", "Organic Items / Category", mp.organic_per_category, "items"),
                param("total_organic_items", "Total Organic Listings", mp.total_organic_items, "items"),
                param("category_count", "Covered Categories", mp.category_count, "categories"),
            ],
            features: vec![
                feat("Contiguous Category-Ordered Storage", "Stores 50 organic listings per category in contiguous slices enabling O(1) category-indexed sampling."),
                feat("Disjoint Organic Namespace", "IDs organic-0001..organic-5000 never collide with advertiser catalog IDs."),
            ],
            gaps_and_simplifications: vec![
                "Organic listing prices and categories do not currently influence ad ranking or contextual relevance.".into(),
            ],
        },
        SimulatorPiece {
            id: "user-browsing".into(),
            name: "Shopper Roster & Organic Browsing".into(),
            crate_name: "world-generator".into(),
            stage: "World Generation".into(),
            source_files: vec![
                "world-generator/src/marketplace.rs".into(),
                "world-generator/src/main.rs".into(),
            ],
            summary: "Creates 1,000 shopper identities (id-only) and 12,000 shuffled organic browsing events (9 affinity + 3 exploration per shopper).".into(),
            parameters: vec![
                param("user_count", "Shoppers", mp.user_count, "users"),
                param("affinity_views", "Affinity Views / User", mp.affinity_views, "events"),
                param("exploration_views", "Exploration Views / User", mp.exploration_views, "events"),
                param("browsing_events_per_user", "Events / User", mp.browsing_events_per_user, "events"),
                param("total_browsing_events", "Total Browsing Events", mp.total_browsing_events, "events"),
            ],
            features: vec![
                feat("Minimal Shopper Identity Schema", "Serializes User with id-only so visitor preference profiles remain encapsulated in world-generator::user_activity_oracle."),
                feat("75/25 Affinity-Exploration Browsing Split", "Samples 9 distinct focus-category listings and 3 outside-focus listings per shopper, then shuffles order."),
                feat("Atomic JSON CLI Export", "world-generator CLI writes marketplace.json atomically via NamedTempFile."),
            ],
            gaps_and_simplifications: vec![
                "Static shopper roster without churn, new-user acquisition, or organic session timestamps.".into(),
            ],
        },
        SimulatorPiece {
            id: "visit-generator".into(),
            name: "Visit Generator & Diurnal Arrivals".into(),
            crate_name: "world-generator".into(),
            stage: "World Generation".into(),
            source_files: vec![
                "world-generator/src/visit_generator.rs".into(),
                "world-generator/src/timestamp.rs".into(),
                "simulation/src/bin/visit_generator.rs".into(),
            ],
            summary: "Samples preference-independent shopper visits across 30 days (Stream 1) with optional PST diurnal timestamps, drives AdServer::serve_ads(user_id) via AdServingTarget, and coordinates click feedback.".into(),
            parameters: vec![
                param("user_independent_arrivals", "Preference-Independent Visit Arrivals", visit.user_independent_arrivals, "bool"),
                param("default_days", "Simulation Horizon", visit.default_days, "days"),
                param("default_daily_requests", "Default Daily Ad Requests", visit.default_daily_requests, "requests/day"),
                param("rng_stream", "Visit Arrival RNG Stream", format!("ChaCha8 Stream {}", visit.rng_stream), "stream"),
                param("default_seed", "Default Seed", visit.default_seed, "u64"),
            ],
            features: vec![
                feat("Preference-Independent Visit Arrivals & Serving Boundary", "Samples visiting users uniformly on Stream 1 and sends only user_id to AdServingTarget::serve_ads(user_id), keeping AdServer decoupled from shopper preferences."),
                feat("PST Diurnal Intraday Arrival Schedule", "Generates split-normal PST arrival timestamps peaking at 08:00 PST and tapering off around 18:00 PST when pacing-1 is active."),
                feat("Dual Request & Target Quota Modes", "Supports natural visit request streams (600 req/day = 12k impressions/day at slate 20) or exact 30-day impression quotas."),
                feat("Standalone CLI Binary & Unreachable Quota Guard", "visit-generator CLI produces standalone JSON slates directly from marketplace users/categories and AdServer, and fails cleanly when budgets deplete."),
            ],
            gaps_and_simplifications: vec![
                "Uniform random user sampling without user session frequency caps or heavy-user power-law arrival distributions.".into(),
            ],
        },
        SimulatorPiece {
            id: "user-activity-oracle".into(),
            name: "User Activity Oracle & Preferences".into(),
            crate_name: "world-generator".into(),
            stage: "World Generation".into(),
            source_files: vec!["world-generator/src/user_activity_oracle.rs".into()],
            summary: "Assigns each shopper 5 unique preferred categories (Stream 3 + score_preference) and evaluates visitor clicks via selectable world models: world-model-v0 (binary 4% / 0%) or world-model-v1 (hierarchical static shifts + varying per-impression noise per retail group, category, and advertiser combo).".into(),
            parameters: vec![
                param("default_world_model", "Default World Model", oracle.default_world_model.as_str(), "model"),
                param(
                    "supported_world_models",
                    "Supported World Models",
                    oracle
                        .supported_world_models
                        .iter()
                        .map(|m| m.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    "models",
                ),
                param("interest_count", "Preferred Categories / User", oracle.interest_count, "categories"),
                param("preference_rng_stream", "Preference RNG Stream", format!("ChaCha8 Stream {}", oracle.preference_rng_stream), "stream"),
                param("click_rng_stream", "User Activity Oracle RNG Stream", format!("ChaCha8 Stream {}", oracle.click_rng_stream), "stream"),
                param("world_model_rng_stream", "World Model v1 Param RNG Stream", format!("ChaCha8 Stream {}", oracle.world_model_rng_stream), "stream"),
                param("max_click_probability", "Preferred-Category Base Click Probability", format!("{} ({:.1}%)", oracle.max_click_probability, max_ctr_pct), "probability"),
                param("v1_non_preferred_base_ctr", "v1 Non-Preferred Base CTR", format!("{} ({:.2}%)", oracle.v1_non_preferred_base_ctr, oracle.v1_non_preferred_base_ctr * 100.0), "probability"),
            ],
            features: vec![
                feat("Unique Shopper Preferred Categories", "generate_user_preferences assigns 5 distinct preferred categories per shopper on Stream 3 with binary score_preference(profile, item)."),
                feat("World Model v0 (Binary Category Oracle)", "Baseline click oracle evaluating visitor clicks on Stream 2 (4% on preferred categories, 0% otherwise) while remaining hidden from AdServer."),
                feat("World Model v1 (Noisy Hierarchical Oracle)", "Injects seeded static logit CTR shifts and varying per-impression Gaussian logit noise per retail group, category, and (advertiser, category) combo on Stream 4 + Stream 2, with noisy non-zero background CTR on non-preferred categories."),
            ],
            gaps_and_simplifications: vec![
                "User category preferences stay static over 30 days without price sensitivity, recency, or ad fatigue.".into(),
                "No position-bias decay across slate slots 1..20 (slot 20 has the same click distribution as slot 1).".into(),
            ],
        },
        SimulatorPiece {
            id: "ad-server".into(),
            name: "Ad-Server & Campaign Budget Pool".into(),
            crate_name: "ad-server".into(),
            stage: "Ad Serving & Simulation".into(),
            source_files: vec!["ad-server/src/lib.rs".into()],
            summary: "Standalone ad-server module accepting user_id via serve_ads(user_id) to return a slate of ads from distinct funded campaigns with randomized $1,000-$10,000 budgets and O(1) retirement.".into(),
            parameters: vec![
                param("campaigns_per_advertiser", "Campaigns / Advertiser", srv.campaigns_per_advertiser, "campaigns"),
                param("baseline_campaigns", "Baseline Total Campaigns", baseline_campaigns, "campaigns"),
                param(
                    "default_campaign_budget_range_cents",
                    "Default Campaign Budget Range",
                    format!(
                        "{}-{} cents (${:.0}-${:.0})",
                        srv.default_min_campaign_budget_cents,
                        srv.default_max_campaign_budget_cents,
                        srv.default_min_campaign_budget_cents as f64 / 100.0,
                        srv.default_max_campaign_budget_cents as f64 / 100.0,
                    ),
                    "USD cents",
                ),
                param(
                    "baseline_total_budget_range_cents",
                    "Baseline Budget Pool Range",
                    format!("{}-{} cents", baseline_min_total_budget_cents, baseline_max_total_budget_cents),
                    "USD cents",
                ),
                param("default_cpc_cents", "Default CPC", format!("{} cents (${:.2})", srv.default_cpc_cents, srv.default_cpc_cents as f64 / 100.0), "USD cents"),
            ],
            features: vec![
                feat("User-ID Request Boundary", "serve_ads(user_id) accepts only a user identifier and returns AdResponse containing distinct funded campaign ads."),
                feat("Randomized Per-Campaign Budgets", "Deterministically samples each campaign's upfront budget uniformly in [$1,000, $10,000] (100,000-1,000,000 cents), with optional fixed override."),
                feat("O(1) Active Budget Pool Retirement", "Maintains active_indices + active_pos lookup table for O(1) swap-remove retirement even under multi-click slate updates."),
                feat("Defensive Serving Invariants", "Guarantees zero duplicate campaigns and remaining_budget_cents >= cpc_cents even for custom AdsRanker plugins."),
                feat("Per-Campaign Daily Ledger Ownership", "Tracks exact impressions, clicks, spend, and remaining budget across all 30 days."),
            ],
            gaps_and_simplifications: vec![
                "Uniform CPC across all campaigns without daily pacing curves or real-time auction bidding.".into(),
            ],
        },
        SimulatorPiece {
            id: "ads-ranker-r1".into(),
            name: "Ad-Serving Strategy Stack (Retrieval · Pacing · Bidding · Scoring · Ranking)".into(),
            crate_name: "ad-server".into(),
            stage: "Ad Serving & Simulation".into(),
            source_files: vec![
                "ad-server/src/retrieval.rs".into(),
                "ad-server/src/bidding.rs".into(),
                "ad-server/src/ranker.rs".into(),
                "ad-server/src/strategy.rs".into(),
                "ml/src/lib.rs".into(),
                "ml/src/model.rs".into(),
                "ml/src/xgboost.rs".into(),
            ],
            summary: "Explicit 5-axis ad-serving strategy pipeline combining Candidate Retrieval (Retrieve-V0), Budget Pacing (P0/P1/P2/pacing-1), Auction Bidding (bidding-basebid/bidding-autobid-v0), Candidate Scoring (S0–S3 + Score-R1 PyTorch LR + Score-XGB XGBoost GBDT), and Slate Ranking (R1/R2/R3) on every simulation run.".into(),
            parameters: vec![
                param("default_retrieval", "Default Retrieval Strategy", srv.default_retrieval.as_str(), "strategy"),
                param("default_ranker", "Default Ranking Strategy", srv.default_ranker.as_str(), "strategy"),
                param("default_scoring", "Default Scoring Strategy", srv.default_scoring.as_str(), "strategy"),
                param("default_pacing", "Default Pacing Strategy", srv.default_pacing.as_str(), "strategy"),
                param("default_bidding", "Default Bidding Strategy", srv.default_bidding.as_str(), "strategy"),
                param("default_slate_size", "Default Slate Size", srv.default_slate_size, "ads / request"),
                param("supported_retrievers", "Registered Retrievers", srv.supported_retrievers.iter().map(|r| r.id.as_str()).collect::<Vec<_>>().join(", "), "strategies"),
                param("supported_rankers", "Registered Rankers", srv.supported_rankers.iter().map(|r| r.id.as_str()).collect::<Vec<_>>().join(", "), "strategies"),
                param("supported_scorers", "Registered Scorers", srv.supported_scorers.iter().map(|s| s.id.as_str()).collect::<Vec<_>>().join(", "), "strategies"),
                param("supported_pacers", "Registered Pacers", srv.supported_pacers.iter().map(|p| p.id.as_str()).collect::<Vec<_>>().join(", "), "strategies"),
                param("supported_bidders", "Registered Bidders", srv.supported_bidders.iter().map(|b| b.id.as_str()).collect::<Vec<_>>().join(", "), "strategies"),
            ],
            features: vec![
                feat("Explicit 5-Axis Strategy Manifest", "Every run resolves and records concrete Retrieval, Ranking, Scoring, Pacing, and Bidding strategies across reports, analysis, SVGs, and dashboards."),
                feat("Active-Budget Retrieval (Retrieve-V0, Top-20)", "ActiveBudgetRetriever sparse-set index uniformly samples up to 20 funded campaigns per request in O(20) with O(1) swap-remove retirement."),
                feat("Pluggable Candidate Scoring (S0–S3 + Score-R1 + Score-XGB)", "Supports Uniform (S0), Online UCB1 CTR (S1), Empirical CTR (S2), Empirical eCPM (S3), daily PyTorch L2 logistic regression CTR (Score-R1), and daily XGBoost GBDT CTR (Score-XGB) via ads-ml."),
                feat("Budget Pacing Engines (P0–P2 + pacing-1)", "Supports Unpaced ASAP (P0), Even Daily Cap (P1), Smooth Probabilistic (P2), and 24-hour PST linear intraday budget schedule (pacing-1)."),
                feat("Auction Bidding & Slate Ranking (B0–B1 × R1–R3)", "Supports Base Bid (bidding-basebid) and eCPM Auto-Bid (bidding-autobid-v0) combined with Random (R1), Bandit ε-greedy (R2), and Greedy Top-K (R3)."),
            ],
            gaps_and_simplifications: vec![
                "Retrieve-V0 uniformly samples up to 20 funded campaigns without ANN/inverted-index pre-filtering; Score-R1 and Score-XGB require Python PyTorch / XGBoost runtimes for daily cold retraining.".into(),
            ],
        },
        SimulatorPiece {
            id: "delivery-click-engine".into(),
            name: "30-Day Simulation Orchestrator".into(),
            crate_name: "ads-marketplace-simulation".into(),
            stage: "Ad Serving & Simulation".into(),
            source_files: vec![
                "simulation/src/simulation.rs".into(),
                "simulation/src/lib.rs".into(),
                "simulation/src/main.rs".into(),
            ],
            summary: "Orchestrates world-generator (VisitGenerator + UserActivityOracle) and AdServer across 30 days, drives optional daily Score-R1 / Score-XGB ML training and JSONL logging, and assembles balanced Schema v1 reports.".into(),
            parameters: vec![
                param("days", "Simulation Horizon", sim.days, "days"),
                param("default_ranker", "Default Ranker", sim.default_ranker.as_str(), "strategy"),
                param("default_slate_size", "Default Slate Size", sim.default_slate_size, "ads / request"),
                param("default_retrieval", "Default Retrieval", sim.default_retrieval.as_str(), "strategy"),
                param("default_pacing", "Default Pacing", sim.default_pacing.as_str(), "strategy"),
                param("default_bidding", "Default Bidding", sim.default_bidding.as_str(), "strategy"),
            ],
            features: vec![
                feat("Isolated 4-Stream RNG Architecture", "Stream 0 (AdServer/Ranker), Stream 1 (VisitGenerator arrivals), Stream 2 (UserActivityOracle clicks), and Stream 3 (user preferences) remain strictly independent."),
                feat("Click-Only CPC Ledger Aggregation", "Aggregates VisitGenerator/UserActivityOracle delivery and click outcomes and AdServer campaign ledgers into balanced 30-day Schema v1 reports."),
                feat("Daily ML CTR Training & JSONL Logging", "Optionally persists daily impression-click logs and runs end-of-day PyTorch (Score-R1) or XGBoost (Score-XGB) CTR training via ads-ml."),
                feat("Dual Borrowed & Owned Report Types", "Exports zero-copy Report and serde-deserializable OwnedReport with ad_requests, ranker, and balanced multi-level ledgers."),
            ],
            gaps_and_simplifications: vec![
                "Legacy no_matching_category counter is retained at 0 for Schema v1 compatibility; no post-click conversion or ROAS model yet.".into(),
            ],
        },
        SimulatorPiece {
            id: "simulation-analyzer".into(),
            name: "Automated Simulation Analyzer".into(),
            crate_name: "simulation-analyzer".into(),
            stage: "Analysis & Visualization".into(),
            source_files: vec![
                "simulation-analyzer/src/lib.rs".into(),
                "simulation-analyzer/src/main.rs".into(),
            ],
            summary: "Auto-analyzes any 30-day simulation report for budget depletion milestones, average & daily CTR, eCPM, campaign Gini concentration, category/advertiser rollups, and diagnostic findings.".into(),
            parameters: vec![
                param("schema_version", "Supported Report Schema", analyzer.schema_version, "version"),
                param("metric_groups", "Metric Groups", analyzer.metric_groups.len(), "groups"),
                param("finding_categories", "Diagnostic Categories", analyzer.finding_categories.join(", "), "categories"),
                param("theoretical_random_ctr", "Random Baseline CTR", "0.20%", "CTR"),
                param("theoretical_max_ctr", "Targeted Ceiling CTR", "4.00%", "CTR"),
            ],
            features: vec![
                feat("Budget Depletion & Exhaustion Milestones", "Computes first, median (50%), and full (100%) exhaustion days, projected days to depletion, daily burn stats, and CV%."),
                feat("CTR, eCPM & Fill-Rate Analytics", "Evaluates overall and daily CTR distributions, ratio vs 0.20% random baseline, eCPM, effective CPC, and avg ads/request."),
                feat("Spend Concentration & Rollups", "Calculates campaign spend Gini coefficient, top-10% share, zero-click count, and top campaigns, categories, and advertisers."),
                feat("Automated Diagnostic Findings & CLI", "Synthesizes severity-tagged findings (info/warning/critical) and provides standalone simulation-analyzer CLI + analysis.json export."),
            ],
            gaps_and_simplifications: vec![
                "Single-run post-hoc analyzer without automated multi-strategy A/B statistical significance testing (p-values / confidence intervals) yet.".into(),
            ],
        },
        SimulatorPiece {
            id: "sim-agent-runner".into(),
            name: "Sim-Agent Runner, Auditor & 30-Day Plotter".into(),
            crate_name: "sim-agent".into(),
            stage: "Analysis & Visualization".into(),
            source_files: vec![
                "sim-agent/src/agent.rs".into(),
                "sim-agent/src/plot.rs".into(),
                "sim-agent/src/lib.rs".into(),
                "sim-agent/src/main.rs".into(),
            ],
            summary: "Autonomously runs the 30-day simulator, audits all ledger invariants, runs SimulationAnalyzer, derives 30-day budget time series, and renders deterministic dual-panel SVG plots.".into(),
            parameters: vec![
                param("days", "Plotted Horizon", agent.days, "days"),
                param("default_impressions", "Default Impression Target", agent.default_impressions, "impressions"),
                param("default_ranker", "Default Ranker", agent.default_ranker.as_str(), "strategy"),
                param("default_slate_size", "Default Slate Size", agent.default_slate_size, "ads / request"),
                param("output_files", "Exported Artifacts", agent.output_files.join(", "), "files"),
            ],
            features: vec![
                feat("End-to-End Autonomous Pipeline", "Generates/loads marketplace, runs 30-day engine with R1, audits ledgers, auto-analyzes, and exports 5 atomic artifacts."),
                feat("Programmatic Ledger Invariant Auditor", "Verifies 30 sequential days, spend == clicks * cpc, remaining + spend == budget, and campaign-to-day rollups."),
                feat("Pure-Rust Dual-Panel SVG Budget Plotter", "Renders KPI header, daily spend bars + mean line, and cumulative spend curve with zero-spend and XML-escape guards."),
            ],
            gaps_and_simplifications: vec![
                "Runs single-policy configurations without automated multi-ranker parameter sweeps or policy optimization loops.".into(),
            ],
        },
        SimulatorPiece {
            id: "dashboard-inspector".into(),
            name: "Feature, Flowchart & Analysis Dashboard".into(),
            crate_name: "ads-simulator-dashboard".into(),
            stage: "Analysis & Visualization".into(),
            source_files: vec![
                "dashboard/src/catalog.rs".into(),
                "dashboard/src/server.rs".into(),
                "dashboard/src/export.rs".into(),
                "dashboard/src/main.rs".into(),
            ],
            summary: "Serves a zero-build interactive web dashboard and pure-Rust HTTP/1.1 API featuring the Component Interaction Flowchart, all 12 pieces, live Ad-Server R1 sampler, auto-analyzer panel, and offline static export.".into(),
            parameters: vec![
                param("default_host", "Default Bind Host", "127.0.0.1", "IP"),
                param("default_port", "Default Bind Port", 8080, "TCP port (0 for ephemeral)"),
                param("catalog_pieces", "Cataloged Simulator Pieces", 12, "pieces"),
                param("export_files", "Static Export Bundle", "index.html, dashboard-data.json, budget_timeseries.json, budget_spent_30d.svg", "files"),
            ],
            features: vec![
                feat("Interactive Component Interaction Flowchart", "Visualizes all 7 crates, decision diamonds, R1 ad-server boundary, and live feedback loops with click-to-inspect trace filtering."),
                feat("Live Ad-Server (R1) Sampler", "GET /api/ad-server/sample?user_id=... lets users query AdServer::serve_ads(user_id) and inspect the 20 distinct funded ads live."),
                feat("Integrated Auto-Analyzer & Live Sim-Agent Runner", "Surfaces budget depletion, CTR, eCPM, Gini concentration, and diagnostic findings on every live POST /api/sim-agent/run."),
                feat("Self-Contained Offline Static Export", "--export-dir writes a 4-file bundle with embedded bootstrap JSON that renders cleanly over file://."),
            ],
            gaps_and_simplifications: vec![
                "In-memory latest simulation state only; does not persist multi-run comparison history across restarts.".into(),
            ],
        },
    ];

    let total_features: usize = pieces.iter().map(|p| p.features.len()).sum();
    let implemented_features: usize = pieces
        .iter()
        .flat_map(|p| p.features.iter())
        .filter(|f| f.status.eq_ignore_ascii_case("implemented"))
        .count();
    let gaps_and_simplifications: usize = pieces
        .iter()
        .map(|p| p.gaps_and_simplifications.len())
        .sum();

    let pipeline_edges = vec![
        edge("taxonomy-schema", "advertiser-catalog", "10x10 category IDs"),
        edge("taxonomy-schema", "organic-inventory", "50 listings / category"),
        edge("taxonomy-schema", "user-activity-oracle", "100 category IDs -> 5 preferred / user"),
        edge("organic-inventory", "user-browsing", "9 affinity + 3 exploration views"),
        edge("user-browsing", "visit-generator", "user_ids (1,000 shoppers)"),
        edge("advertiser-catalog", "ad-server", "500 funded CPC campaigns"),
        edge("visit-generator", "ad-server", "serve_ads(user_id) & record_impression"),
        edge("ad-server", "ads-ranker-r1", "eligible funded pool"),
        edge("ads-ranker-r1", "ad-server", "20 distinct funded campaigns"),
        edge("ad-server", "user-activity-oracle", "20-ad served slate"),
        edge("visit-generator", "user-activity-oracle", "visiting user_index"),
        edge("user-activity-oracle", "ad-server", "record_impression (click -> -CPC)"),
        edge("visit-generator", "delivery-click-engine", "delivered impressions & clicks"),
        edge("delivery-click-engine", "simulation-analyzer", "30-day Report JSON"),
        edge("delivery-click-engine", "sim-agent-runner", "audited 30-day ledger"),
        edge("simulation-analyzer", "sim-agent-runner", "analysis.json + findings"),
        edge("sim-agent-runner", "dashboard-inspector", "time series, analysis & SVG"),
    ];

    SimulatorCatalog {
        simulator_version: env!("CARGO_PKG_VERSION").to_owned(),
        schema_version: mp.schema_version,
        currency: mp.currency,
        summary_counts: SummaryCounts {
            pieces: pieces.len(),
            total_features,
            implemented_features,
            gaps_and_simplifications,
            crates: 7,
            categories: mp.category_count,
            advertisers: mp.advertiser_count,
            users: mp.user_count,
            campaigns: baseline_campaigns,
            simulation_days: sim.days,
            r1_slate_size: srv.default_slate_size,
        },
        pipeline_edges,
        pieces,
    }
}

/// Generate and summarize a marketplace for a given signed 64-bit seed.
pub fn inspect_marketplace(seed: i64) -> MarketplaceInspection {
    let mp = generate_marketplace(seed);

    let mut cat_number = 0usize;
    let taxonomy_groups = taxonomy()
        .iter()
        .map(|(group, names)| {
            let categories = names
                .iter()
                .map(|name| {
                    cat_number += 1;
                    TaxonomyCategory {
                        id: format!("category-{cat_number:03}"),
                        name: (*name).to_owned(),
                    }
                })
                .collect();
            TaxonomyGroup {
                group: (*group).to_owned(),
                categories,
            }
        })
        .collect();

    let mut catalog_count = 0usize;
    let mut catalog_min = u32::MAX;
    let mut catalog_max = 0u32;
    let mut catalog_sum = 0u64;

    for adv in &mp.advertisers {
        for item in &adv.catalog {
            catalog_count += 1;
            catalog_min = catalog_min.min(item.price_cents);
            catalog_max = catalog_max.max(item.price_cents);
            catalog_sum += u64::from(item.price_cents);
        }
    }

    let mut organic_min = u32::MAX;
    let mut organic_max = 0u32;
    let mut organic_sum = 0u64;
    for item in &mp.organic_items {
        organic_min = organic_min.min(item.price_cents);
        organic_max = organic_max.max(item.price_cents);
        organic_sum += u64::from(item.price_cents);
    }

    let mut affinity = 0usize;
    let mut exploration = 0usize;
    for ev in &mp.browsing_events {
        match ev.reason {
            BrowseReason::Affinity => affinity += 1,
            BrowseReason::Exploration => exploration += 1,
        }
    }

    let catalog_mean = if catalog_count > 0 {
        ((catalog_sum as f64 / catalog_count as f64) * 100.0).round() / 100.0
    } else {
        0.0
    };
    let organic_mean = if !mp.organic_items.is_empty() {
        ((organic_sum as f64 / mp.organic_items.len() as f64) * 100.0).round() / 100.0
    } else {
        0.0
    };

    MarketplaceInspection {
        seed: mp.seed,
        schema_version: mp.schema_version,
        currency: mp.currency,
        taxonomy: taxonomy_groups,
        counts: MarketplaceCounts {
            categories: mp.categories.len(),
            advertisers: mp.advertisers.len(),
            catalog_items: catalog_count,
            users: mp.users.len(),
            organic_items: mp.organic_items.len(),
            browsing_events: mp.browsing_events.len(),
        },
        price_stats: PriceStats {
            catalog: PriceBucket {
                min_cents: if catalog_count > 0 { catalog_min } else { 0 },
                mean_cents: catalog_mean,
                max_cents: catalog_max,
            },
            organic: PriceBucket {
                min_cents: if !mp.organic_items.is_empty() {
                    organic_min
                } else {
                    0
                },
                mean_cents: organic_mean,
                max_cents: organic_max,
            },
        },
        browsing_split: BrowsingSplit {
            affinity,
            exploration,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::Path;

    use super::{build_catalog, inspect_marketplace};

    #[test]
    fn catalog_has_twelve_connected_pieces_and_verified_rust_sources() {
        let catalog = build_catalog();
        assert_eq!(catalog.schema_version, 1);
        assert_eq!(catalog.currency, "USD");
        assert_eq!(catalog.pieces.len(), 12);
        assert_eq!(catalog.summary_counts.pieces, 12);
        assert_eq!(catalog.summary_counts.crates, 7);
        assert_eq!(catalog.summary_counts.r1_slate_size, 20);
        assert!(catalog.pipeline_edges.len() >= 10);
        assert!(catalog.summary_counts.gaps_and_simplifications >= 3);

        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let ids: HashSet<_> = catalog.pieces.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids.len(), 12);

        let mut connected = HashSet::new();
        for edge in &catalog.pipeline_edges {
            assert!(ids.contains(edge.from.as_str()));
            assert!(ids.contains(edge.to.as_str()));
            assert_ne!(edge.from, edge.to);
            connected.insert(edge.from.as_str());
            connected.insert(edge.to.as_str());
        }
        assert_eq!(connected.len(), 12);

        for piece in &catalog.pieces {
            assert!(!piece.source_files.is_empty());
            for sf in &piece.source_files {
                assert!(sf.ends_with(".rs"), "expected .rs source file, got {sf}");
                assert!(
                    repo_root.join(sf).exists(),
                    "source file missing on disk: {sf}"
                );
            }
        }
    }

    #[test]
    fn marketplace_inspection_matches_baseline_counts_and_seed_determinism() {
        let m42_a = inspect_marketplace(42);
        let m42_b = inspect_marketplace(42);
        let m99 = inspect_marketplace(99);

        assert_eq!(m42_a, m42_b);
        assert_eq!(m42_a.seed, 42);
        assert_eq!(m42_a.taxonomy.len(), 10);
        assert!(m42_a.taxonomy.iter().all(|g| g.categories.len() == 10));
        assert_eq!(m42_a.counts.categories, 100);
        assert_eq!(m42_a.counts.advertisers, 50);
        assert_eq!(m42_a.counts.catalog_items, 1_000);
        assert_eq!(m42_a.counts.users, 1_000);
        assert_eq!(m42_a.counts.organic_items, 5_000);
        assert_eq!(m42_a.counts.browsing_events, 12_000);
        assert_eq!(m42_a.browsing_split.affinity, 9_000);
        assert_eq!(m42_a.browsing_split.exploration, 3_000);
        assert_eq!(m42_a.taxonomy, m99.taxonomy);
        assert_ne!(m42_a.price_stats, m99.price_stats);
    }
}
