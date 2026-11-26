//! Standalone CLI for `simulation-analyzer`: auto-analyzes any Schema v1 `simulation.json`
//! report, prints a structured summary, and optionally writes `analysis.json` atomically.

use std::error::Error;
use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use simulation_analyzer::{load_and_analyze, render_text_summary};
use tempfile::NamedTempFile;

#[derive(Debug, Parser)]
#[command(
    name = "simulation-analyzer",
    about = "Auto-analyze a 30-day ads-marketplace simulation report (budget depletion, CTR, concentration, findings)"
)]
struct Args {
    /// Path to a Schema v1 simulation.json report.
    #[arg(long)]
    report: PathBuf,

    /// Optional output path to write the full SimulationAnalysis JSON.
    #[arg(long)]
    output: Option<PathBuf>,
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

fn run(args: Args) -> Result<(), Box<dyn Error>> {
    let analysis = load_and_analyze(&args.report)?;
    print!("{}", render_text_summary(&analysis));

    if let Some(output_path) = args.output {
        let dir = output_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(dir)?;
        let mut temp = NamedTempFile::new_in(dir)?;
        {
            let mut writer = BufWriter::new(temp.as_file_mut());
            serde_json::to_writer_pretty(&mut writer, &analysis)?;
            writer.write_all(b"\n")?;
            writer.flush()?;
        }
        temp.persist(&output_path)?;
        println!("Wrote analysis JSON to {}", output_path.display());
    }

    Ok(())
}
