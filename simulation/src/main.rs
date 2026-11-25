mod pacing;
mod simulation;
mod timestamp;

use std::{
    error::Error,
    fs::File,
    io::{BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

use clap::Parser;
use pacing::PacingStrategy;
use simulation::{Config, run, run_with_impressions};
use tempfile::NamedTempFile;

#[derive(Parser)]
#[command(about = "Run a seeded 30-day CPC simulation from marketplace data")]
struct Args {
    /// Schema v1 marketplace.json file.
    #[arg(long)]
    marketplace: PathBuf,
    /// Output report path.
    #[arg(long)]
    output: PathBuf,
    /// Optional NDJSON output path for timestamped delivered impression events (PST).
    #[arg(long)]
    impressions_output: Option<PathBuf>,
    #[arg(long, default_value_t = 42)]
    seed: u64,
    /// Upfront budget for each campaign, in USD cents.
    #[arg(long, default_value_t = 10_000)]
    campaign_budget_cents: u64,
    /// Price of each click, in USD cents.
    #[arg(long, default_value_t = 100)]
    cpc_cents: u64,
    /// Exact number of impressions to deliver across the 30 days.
    #[arg(long, conflicts_with = "opportunities")]
    impressions: Option<u64>,
    /// Exact number of timestamped ad opportunities to generate across the 30 days.
    #[arg(long, conflicts_with = "impressions")]
    opportunities: Option<u64>,
    /// Budget pacing strategy.
    #[arg(long, value_enum, default_value_t = PacingStrategy::Pacing1)]
    pacing: PacingStrategy,
}

fn parent_dir(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let marketplace = serde_json::from_reader(BufReader::new(File::open(args.marketplace)?))?;
    let config = Config {
        seed: args.seed,
        campaign_budget_cents: args.campaign_budget_cents,
        cpc_cents: args.cpc_cents,
        impressions: args.impressions,
        opportunities: args.opportunities,
        pacing: args.pacing,
    };

    let report = if let Some(impressions_path) = args.impressions_output {
        let mut temp_impressions = NamedTempFile::new_in(parent_dir(&impressions_path))?;
        let report = {
            let mut writer = BufWriter::new(temp_impressions.as_file_mut());
            let report = run_with_impressions(&marketplace, config, |impression| {
                serde_json::to_writer(&mut writer, impression).map_err(std::io::Error::other)?;
                writer.write_all(b"\n")
            })?;
            writer.flush()?;
            report
        };
        temp_impressions.persist(impressions_path)?;
        report
    } else {
        run(&marketplace, config)?
    };

    let mut temporary = NamedTempFile::new_in(parent_dir(&args.output))?;
    {
        let mut output = BufWriter::new(temporary.as_file_mut());
        serde_json::to_writer(&mut output, &report)?;
        writeln!(output)?;
        output.flush()?;
    }
    temporary.persist(args.output)?;
    Ok(())
}
