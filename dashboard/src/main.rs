//! CLI entry point for `ads-simulator-dashboard`: serves the interactive web UI + JSON API
//! or exports a self-contained static offline bundle via `--export-dir`.

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, RwLock};

use ads_simulator_dashboard::{DashboardState, bind_listener, export_static_bundle, serve};
use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "ads-simulator-dashboard",
    about = "Serve the ads-simulator feature & 30-day budget dashboard or export a static bundle"
)]
struct Args {
    /// Host IP address to bind when serving.
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// TCP port to bind when serving (0 selects an ephemeral OS-assigned port).
    #[arg(long, default_value_t = 8080)]
    port: u16,

    /// Optional precomputed sim-agent budget_timeseries.json or simulation.json report.
    #[arg(long)]
    sim_report: Option<PathBuf>,

    /// Optional precomputed sim-agent budget_spent_30d.svg plot file.
    #[arg(long)]
    plot_file: Option<PathBuf>,

    /// Export a self-contained static bundle to this directory and exit without starting the server.
    #[arg(long)]
    export_dir: Option<PathBuf>,
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("Error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let state = DashboardState::load(args.sim_report.as_deref(), args.plot_file.as_deref())?;

    if let Some(export_dir) = args.export_dir {
        let artifacts = export_static_bundle(
            &export_dir,
            &state.catalog,
            &state.default_marketplace,
            &state.latest_timeseries,
            state.latest_analysis.as_ref(),
            &state.default_ad_server_sample,
            &state.latest_svg,
        )?;
        println!(
            "Exported static dashboard bundle to {}",
            export_dir.display()
        );
        println!("  - {}", artifacts.index_html.display());
        println!("  - {}", artifacts.dashboard_data_json.display());
        println!("  - {}", artifacts.budget_timeseries_json.display());
        println!("  - {}", artifacts.budget_plot_svg.display());
        return Ok(());
    }

    let (listener, bound_addr) = bind_listener(&args.host, args.port)?;
    let url = format!("http://{}:{}", bound_addr.ip(), bound_addr.port());
    println!("{url}");
    io::stdout().flush()?;

    serve(listener, Arc::new(RwLock::new(state)))?;
    Ok(())
}
