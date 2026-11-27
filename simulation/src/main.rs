use std::{
    error::Error,
    fs::{self, File},
    io::{BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

use ads_marketplace_simulation::{
    BiddingStrategy, Config, DEFAULT_BIDDING, DEFAULT_CPC_CENTS, DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS,
    DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS, DEFAULT_PACING, DEFAULT_RANKER, DEFAULT_RETRIEVAL,
    DEFAULT_SEED, DEFAULT_SLATE_SIZE, PacingStrategy, RankerStrategy, Report, RetrievalStrategy,
    ScoringStrategy, run_with_log_dir,
};
use clap::Parser;
use tempfile::NamedTempFile;

#[derive(Debug, Parser)]
#[command(about = "Run a seeded 30-day CPC simulation with explicit 5-axis serving strategies")]
struct Args {
    /// Schema v1 marketplace.json file.
    #[arg(long)]
    marketplace: PathBuf,
    /// Output report path.
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value_t = DEFAULT_SEED)]
    seed: u64,
    /// Optional fixed upfront budget for each campaign in USD cents (omit to randomize $1,000-$10,000).
    #[arg(long)]
    campaign_budget_cents: Option<u64>,
    /// Minimum randomized campaign budget in USD cents (default $1,000.00).
    #[arg(long, default_value_t = DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS)]
    campaign_budget_min_cents: u64,
    /// Maximum randomized campaign budget in USD cents (default $10,000.00).
    #[arg(long, default_value_t = DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS)]
    campaign_budget_max_cents: u64,
    /// Campaign basebid / price of each click, in USD cents.
    #[arg(long, alias = "base-bid-cents", default_value_t = DEFAULT_CPC_CENTS)]
    cpc_cents: u64,
    /// Exact number of impressions to deliver across the 30 days.
    #[arg(long)]
    impressions: Option<u64>,
    /// Retrieval strategy (`Retrieve-V0`/`RT0`).
    #[arg(long, alias = "retrieval-strategy", default_value_t = DEFAULT_RETRIEVAL)]
    retrieval: RetrievalStrategy,
    /// Ads-ranker strategy (`R1`/`Rank-Uniform`, `R2`, `R3`/`Rank-Greedy`).
    #[arg(long, alias = "ranking-strategy", default_value_t = DEFAULT_RANKER)]
    ranker: RankerStrategy,
    /// Candidate scoring strategy (`S0`/`Score-Uniform`, `S1`, `S2`, `S3`, `Score-R1`; defaults from --ranker).
    #[arg(long, alias = "scoring-strategy")]
    scoring: Option<ScoringStrategy>,
    /// Budget pacing strategy (`P0`/`none`, `P1`, `P2`, `pacing-1`).
    #[arg(long, default_value_t = DEFAULT_PACING)]
    pacing: PacingStrategy,
    /// Bidding strategy (`bidding-basebid`/`B0`, `bidding-autobid-v0`/`B1`).
    #[arg(long, default_value_t = DEFAULT_BIDDING)]
    bidding: BiddingStrategy,
    /// Maximum number of distinct funded campaigns returned per ad-server request.
    #[arg(long, default_value_t = DEFAULT_SLATE_SIZE)]
    slate_size: usize,
    /// Optional directory where daily `ads-ml` JSONL impression-click logs & models are written.
    #[arg(long)]
    impression_log_dir: Option<PathBuf>,
}

fn main() -> Result<(), Box<dyn Error>> {
    run_with_args(Args::parse())
}

fn run_with_args(args: Args) -> Result<(), Box<dyn Error>> {
    let marketplace = serde_json::from_reader(BufReader::new(File::open(&args.marketplace)?))?;
    let scoring = args
        .scoring
        .unwrap_or_else(|| args.ranker.default_scoring());
    let report = run_with_log_dir(
        &marketplace,
        Config {
            seed: args.seed,
            campaign_budget_cents: args.campaign_budget_cents,
            campaign_budget_min_cents: args.campaign_budget_min_cents,
            campaign_budget_max_cents: args.campaign_budget_max_cents,
            cpc_cents: args.cpc_cents,
            impressions: args.impressions,
            retrieval: args.retrieval,
            ranker: args.ranker,
            scoring,
            pacing: args.pacing,
            bidding: args.bidding,
            slate_size: args.slate_size,
        },
        args.impression_log_dir.as_deref(),
    )?;

    write_report_with_fallback_dir(&report, &args.output, Path::new("."))
}

fn output_directory(output: &Path) -> Option<&Path> {
    output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
}

fn write_report_with_fallback_dir(
    report: &Report<'_>,
    output_path: &Path,
    fallback_dir: &Path,
) -> Result<(), Box<dyn Error>> {
    let (directory, target) = match output_directory(output_path) {
        Some(parent) => {
            fs::create_dir_all(parent)?;
            (parent, output_path.to_path_buf())
        }
        None => {
            let target = if fallback_dir == Path::new(".") {
                output_path.to_path_buf()
            } else {
                fallback_dir.join(output_path)
            };
            (fallback_dir, target)
        }
    };

    let mut temporary = NamedTempFile::new_in(directory)?;
    {
        let mut output = BufWriter::new(temporary.as_file_mut());
        serde_json::to_writer(&mut output, report)?;
        writeln!(output)?;
        output.flush()?;
    }
    temporary.persist(target)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, File},
        io::BufReader,
        path::Path,
    };

    use ads_marketplace_simulation::{
        Config, DAYS, DEFAULT_BIDDING, DEFAULT_CPC_CENTS, DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS,
        DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS, DEFAULT_PACING, DEFAULT_RANKER, DEFAULT_RETRIEVAL,
        DEFAULT_SEED, DEFAULT_SLATE_SIZE, OwnedReport, run,
    };
    use world_generator::generate_marketplace;
    use clap::Parser;
    use tempfile::TempDir;

    use super::{Args, output_directory, run_with_args, write_report_with_fallback_dir};

    #[test]
    fn cli_defaults_match_library_constants() {
        let args = Args::try_parse_from([
            "ads-marketplace-simulation",
            "--marketplace",
            "marketplace.json",
            "--output",
            "simulation.json",
        ])
        .unwrap();
        assert_eq!(args.seed, DEFAULT_SEED);
        assert_eq!(args.campaign_budget_cents, None);
        assert_eq!(
            args.campaign_budget_min_cents,
            DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS
        );
        assert_eq!(
            args.campaign_budget_max_cents,
            DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS
        );
        assert_eq!(args.cpc_cents, DEFAULT_CPC_CENTS);
        assert_eq!(args.impressions, None);
        assert_eq!(args.retrieval, DEFAULT_RETRIEVAL);
        assert_eq!(args.ranker, DEFAULT_RANKER);
        assert_eq!(args.scoring, None);
        assert_eq!(args.pacing, DEFAULT_PACING);
        assert_eq!(args.bidding, DEFAULT_BIDDING);
        assert_eq!(args.slate_size, DEFAULT_SLATE_SIZE);
    }

    #[test]
    fn output_directory_filters_empty_and_root_parents() {
        assert_eq!(output_directory(Path::new("simulation.json")), None);
        assert_eq!(output_directory(Path::new("/")), None);
        assert_eq!(
            output_directory(Path::new("./simulation.json")),
            Some(Path::new("."))
        );
        assert_eq!(
            output_directory(Path::new("nested/deep/simulation.json")),
            Some(Path::new("nested/deep"))
        );
    }

    #[test]
    fn cli_creates_missing_nested_output_parent_directories() {
        let temp = TempDir::new().unwrap();
        let marketplace_path = temp.path().join("marketplace.json");
        let marketplace = generate_marketplace(42);
        serde_json::to_writer(File::create(&marketplace_path).unwrap(), &marketplace).unwrap();

        let output_path = temp.path().join("nested/deep/dir/simulation.json");
        assert!(!output_path.parent().unwrap().exists());

        let args = Args::try_parse_from([
            "ads-marketplace-simulation",
            "--marketplace",
            marketplace_path.to_str().unwrap(),
            "--output",
            output_path.to_str().unwrap(),
            "--impressions",
            "300",
        ])
        .unwrap();
        run_with_args(args).unwrap();

        assert!(output_path.is_file());
        let written: OwnedReport =
            serde_json::from_reader(BufReader::new(File::open(&output_path).unwrap())).unwrap();
        assert_eq!(written.days.len(), DAYS);
        assert_eq!(written.summary.activity.impressions, 300);

        let entries: Vec<_> = fs::read_dir(output_path.parent().unwrap())
            .unwrap()
            .collect();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn bare_relative_output_filename_writes_atomically_without_create_dir_all_error() {
        let temp = TempDir::new().unwrap();
        let mut marketplace = generate_marketplace(42);
        marketplace.advertisers.truncate(1);
        let report = run(
            &marketplace,
            Config {
                impressions: Some(60),
                ..Config::default()
            },
        )
        .unwrap();

        write_report_with_fallback_dir(&report, Path::new("simulation.json"), temp.path()).unwrap();

        let written_path = temp.path().join("simulation.json");
        assert!(written_path.is_file());
        let written: OwnedReport =
            serde_json::from_reader(BufReader::new(File::open(&written_path).unwrap())).unwrap();
        assert_eq!(written, report.to_owned_report());
    }
}
