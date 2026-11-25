use std::{
    error::Error,
    fs::{self, File},
    io::{BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

use ads_marketplace_simulation::{
    Config, DEFAULT_CPC_CENTS, DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS,
    DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS, DEFAULT_RANKER, DEFAULT_SEED, DEFAULT_SLATE_SIZE,
    RankerStrategy, Report, run,
};
use clap::Parser;
use tempfile::NamedTempFile;

#[derive(Debug, Parser)]
#[command(about = "Run a seeded 30-day CPC simulation via user-independent impression generator + ad-server")]
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
    /// Price of each click, in USD cents.
    #[arg(long, default_value_t = DEFAULT_CPC_CENTS)]
    cpc_cents: u64,
    /// Exact number of impressions to deliver across the 30 days.
    #[arg(long)]
    impressions: Option<u64>,
    /// Ads-ranker strategy used by the ad-server (e.g. R1).
    #[arg(long, default_value_t = DEFAULT_RANKER)]
    ranker: RankerStrategy,
    /// Maximum number of distinct funded campaigns returned per ad-server request.
    #[arg(long, default_value_t = DEFAULT_SLATE_SIZE)]
    slate_size: usize,
}

fn main() -> Result<(), Box<dyn Error>> {
    run_with_args(Args::parse())
}

fn run_with_args(args: Args) -> Result<(), Box<dyn Error>> {
    let marketplace = serde_json::from_reader(BufReader::new(File::open(&args.marketplace)?))?;
    let report = run(
        &marketplace,
        Config {
            seed: args.seed,
            campaign_budget_cents: args.campaign_budget_cents,
            campaign_budget_min_cents: args.campaign_budget_min_cents,
            campaign_budget_max_cents: args.campaign_budget_max_cents,
            cpc_cents: args.cpc_cents,
            impressions: args.impressions,
            ranker: args.ranker,
            slate_size: args.slate_size,
        },
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
        Config, DAYS, DEFAULT_CPC_CENTS, DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS,
        DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS, DEFAULT_RANKER, DEFAULT_SEED, DEFAULT_SLATE_SIZE,
        OwnedReport, run,
    };
    use ads_simulator::generate_marketplace;
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
        assert_eq!(args.ranker, DEFAULT_RANKER);
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
