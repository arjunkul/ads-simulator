mod simulation;

use std::{
    error::Error,
    fs::File,
    io::{BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

use clap::Parser;
use simulation::{Config, RankingStrategy, ScoringStrategy, ServingStrategy, run};
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
    /// Ranking strategy (`Rank-*`) used to turn candidate scores into the served ad.
    #[arg(long, value_enum, default_value_t = RankingStrategy::RankUniform)]
    ranking_strategy: RankingStrategy,
    /// Scoring strategy (`Score-*`) used to score eligible candidate ads.
    #[arg(long, value_enum, default_value_t = ScoringStrategy::ScoreUniform)]
    scoring_strategy: ScoringStrategy,
    /// Directory where daily impression-click JSONL logs (`day-01.jsonl`, ...) are written.
    /// Defaults to a sibling `<output_stem>-impressions` directory when `--scoring-strategy Score-R1` is used.
    #[arg(long)]
    impression_log_dir: Option<PathBuf>,
}

fn default_log_dir(output: &Path) -> PathBuf {
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let stem = output
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("simulation");
    parent.join(format!("{stem}-impressions"))
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let marketplace = serde_json::from_reader(BufReader::new(File::open(args.marketplace)?))?;
    let serving = ServingStrategy::new(args.ranking_strategy, args.scoring_strategy);
    let impression_log_dir = match (serving.scoring, args.impression_log_dir) {
        (_, Some(dir)) => Some(dir),
        (ScoringStrategy::ScoreR1, None) => Some(default_log_dir(&args.output)),
        (ScoringStrategy::ScoreUniform, None) => None,
    };
    let report = run(
        &marketplace,
        Config {
            seed: args.seed,
            campaign_budget_cents: args.campaign_budget_cents,
            cpc_cents: args.cpc_cents,
            impressions: args.impressions,
            serving,
            impression_log_dir,
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
