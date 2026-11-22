mod simulation;

use std::{
    error::Error,
    fs::File,
    io::{BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

use clap::Parser;
use simulation::{Config, run};
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
    #[arg(long, default_value_t = 42)]
    seed: u64,
    /// Upfront budget for each campaign, in USD cents.
    #[arg(long, default_value_t = 10_000)]
    campaign_budget_cents: u64,
    /// Price of each click, in USD cents.
    #[arg(long, default_value_t = 100)]
    cpc_cents: u64,
    /// Exact number of impressions to deliver across the 30 days.
    #[arg(long)]
    impressions: Option<u64>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let marketplace = serde_json::from_reader(BufReader::new(File::open(args.marketplace)?))?;
    let report = run(
        &marketplace,
        Config {
            seed: args.seed,
            campaign_budget_cents: args.campaign_budget_cents,
            cpc_cents: args.cpc_cents,
            impressions: args.impressions,
        },
    )?;

    let directory = args
        .output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = NamedTempFile::new_in(directory)?;
    {
        let mut output = BufWriter::new(temporary.as_file_mut());
        serde_json::to_writer(&mut output, &report)?;
        writeln!(output)?;
        output.flush()?;
    }
    temporary.persist(args.output)?;
    Ok(())
}
