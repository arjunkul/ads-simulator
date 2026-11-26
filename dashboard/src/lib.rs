//! Interactive feature catalog, marketplace inspector, and 30-day budget spend dashboard crate.

pub mod catalog;
pub mod export;
pub mod server;

pub use catalog::{
    BrowsingSplit, MarketplaceCounts, MarketplaceInspection, PieceFeature, PieceParameter,
    PipelineEdge, PriceBucket, PriceStats, SimulatorCatalog, SimulatorPiece, SummaryCounts,
    TaxonomyCategory, TaxonomyGroup, build_catalog, inspect_marketplace,
};
pub use export::{
    BUDGET_PLOT_SVG_FILENAME, BUDGET_TIMESERIES_FILENAME, DASHBOARD_DATA_FILENAME,
    INDEX_HTML_FILENAME, StaticExportArtifacts, export_static_bundle, render_dashboard_html,
};
pub use server::{DashboardState, bind_listener, serve};
