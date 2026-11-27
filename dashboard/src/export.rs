//! Embedded HTML rendering and atomic static offline bundle export (`--export-dir`).

use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use ad_server::AdResponse;
use serde::Serialize;
use sim_agent::BudgetTimeSeries;
use simulation_analyzer::SimulationAnalysis;
use tempfile::NamedTempFile;

use crate::catalog::{MarketplaceInspection, SimulatorCatalog};

pub const INDEX_HTML_FILENAME: &str = "index.html";
pub const DASHBOARD_DATA_FILENAME: &str = "dashboard-data.json";
pub const BUDGET_TIMESERIES_FILENAME: &str = "budget_timeseries.json";
pub const BUDGET_PLOT_SVG_FILENAME: &str = "budget_spent_30d.svg";

const INDEX_HTML_TEMPLATE: &str = include_str!("../assets/index.html");
const STYLES_CSS: &str = include_str!("../assets/styles.css");
const APP_JS: &str = include_str!("../assets/app.js");

/// Paths produced by [`export_static_bundle`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaticExportArtifacts {
    pub index_html: PathBuf,
    pub dashboard_data_json: PathBuf,
    pub budget_timeseries_json: PathBuf,
    pub budget_plot_svg: PathBuf,
}

#[derive(Serialize)]
struct DashboardDataFile<'a> {
    catalog: &'a SimulatorCatalog,
    marketplace: &'a MarketplaceInspection,
    timeseries: &'a BudgetTimeSeries,
    analysis: Option<&'a SimulationAnalysis>,
    ad_server_sample: &'a AdResponse,
}

#[derive(Serialize)]
struct StaticBootstrapPayload<'a> {
    catalog: &'a SimulatorCatalog,
    marketplace: &'a MarketplaceInspection,
    timeseries: &'a BudgetTimeSeries,
    analysis: Option<&'a SimulationAnalysis>,
    ad_server_sample: &'a AdResponse,
    svg: &'a str,
    #[serde(rename = "staticMode")]
    static_mode: bool,
}

/// Escape sequences that could close a `<script>` tag or start an HTML comment inside inline JSON.
fn escape_json_for_script_tag(json: &str) -> String {
    json.replace("</", "<\\/")
        .replace("<!--", "<\\!--")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

/// Render the self-contained dashboard HTML with inlined CSS/JS and optional offline bootstrap JSON.
pub fn render_dashboard_html(bootstrap_json: Option<&str>) -> String {
    let bootstrap = match bootstrap_json {
        Some(json) => escape_json_for_script_tag(json),
        None => "null".to_owned(),
    };
    INDEX_HTML_TEMPLATE
        .replace("/*__INLINE_CSS__*/", STYLES_CSS)
        .replace("/*__BOOTSTRAP_JSON__*/", &bootstrap)
        .replace("/*__INLINE_JS__*/", APP_JS)
}

fn write_bytes_atomic(dir: &Path, filename: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    let target = dir.join(filename);
    let mut temp = NamedTempFile::new_in(dir)?;
    {
        let mut writer = BufWriter::new(temp.as_file_mut());
        writer.write_all(bytes)?;
        writer.flush()?;
    }
    temp.persist(&target).map_err(|err| err.error)?;
    Ok(target)
}

/// Atomically export the 4-file self-contained static dashboard bundle to `export_dir`.
pub fn export_static_bundle(
    export_dir: &Path,
    catalog: &SimulatorCatalog,
    marketplace: &MarketplaceInspection,
    timeseries: &BudgetTimeSeries,
    analysis: Option<&SimulationAnalysis>,
    ad_server_sample: &AdResponse,
    svg: &str,
) -> io::Result<StaticExportArtifacts> {
    if export_dir.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "export directory path must not be empty",
        ));
    }
    fs::create_dir_all(export_dir)?;

    let mut data_bytes = serde_json::to_vec_pretty(&DashboardDataFile {
        catalog,
        marketplace,
        timeseries,
        analysis,
        ad_server_sample,
    })
    .map_err(io::Error::other)?;
    data_bytes.push(b'\n');

    let mut ts_bytes = serde_json::to_vec_pretty(timeseries).map_err(io::Error::other)?;
    ts_bytes.push(b'\n');

    let bootstrap_json = serde_json::to_string(&StaticBootstrapPayload {
        catalog,
        marketplace,
        timeseries,
        analysis,
        ad_server_sample,
        svg,
        static_mode: true,
    })
    .map_err(io::Error::other)?;
    let html = render_dashboard_html(Some(&bootstrap_json));

    let dashboard_data_json =
        write_bytes_atomic(export_dir, DASHBOARD_DATA_FILENAME, &data_bytes)?;
    let budget_timeseries_json =
        write_bytes_atomic(export_dir, BUDGET_TIMESERIES_FILENAME, &ts_bytes)?;
    let budget_plot_svg = write_bytes_atomic(export_dir, BUDGET_PLOT_SVG_FILENAME, svg.as_bytes())?;
    let index_html = write_bytes_atomic(export_dir, INDEX_HTML_FILENAME, html.as_bytes())?;

    Ok(StaticExportArtifacts {
        index_html,
        dashboard_data_json,
        budget_timeseries_json,
        budget_plot_svg,
    })
}

#[cfg(test)]
mod tests {
    use ad_server::{AdServer, AdServerConfig};
    use world_generator::generate_marketplace;
    use sim_agent::{AgentConfig, render_budget_svg, run_simulation};
    use tempfile::tempdir;

    use crate::catalog::{build_catalog, inspect_marketplace};
    use crate::export::{export_static_bundle, render_dashboard_html};

    #[test]
    fn static_export_writes_all_four_files_with_embedded_bootstrap() {
        let dir = tempdir().unwrap();
        let catalog = build_catalog();
        let marketplace = inspect_marketplace(42);
        let run = run_simulation(&AgentConfig {
            impressions: Some(6_000),
            ..AgentConfig::default()
        })
        .unwrap();
        let svg = render_budget_svg(&run.timeseries);
        let mp = generate_marketplace(42);
        let mut server = AdServer::from_marketplace(&mp, AdServerConfig::default()).unwrap();
        let sample = server.serve_ads("user-0001");

        let artifacts = export_static_bundle(
            dir.path(),
            &catalog,
            &marketplace,
            &run.timeseries,
            Some(&run.analysis),
            &sample,
            &svg,
        )
        .unwrap();

        assert!(artifacts.index_html.exists());
        assert!(artifacts.dashboard_data_json.exists());
        assert!(artifacts.budget_timeseries_json.exists());
        assert!(artifacts.budget_plot_svg.exists());

        let html = std::fs::read_to_string(&artifacts.index_html).unwrap();
        assert!(html.contains("<html"));
        assert!(html.contains(&catalog.pieces[0].name));
        assert!(!html.contains("/*__INLINE_CSS__*/"));
        assert!(!html.contains("/*__BOOTSTRAP_JSON__*/"));
    }

    #[test]
    fn render_html_escapes_script_closing_tags_in_bootstrap() {
        let html = render_dashboard_html(Some(r#"{"note":"</script><!--test-->"}"#));
        assert!(!html.contains("</script><!--test-->"));
        assert!(html.contains(r#"<\/script><\!--test-->"#));
    }
}
