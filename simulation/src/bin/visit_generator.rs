//! Standalone CLI for the user-independent visit generator operating `ad-server`.

use std::error::Error;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ad_server::{
    AdResponse, AdServer, AdServerConfig, BiddingStrategy, DEFAULT_BIDDING, DEFAULT_CPC_CENTS,
    DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS, DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS, DEFAULT_PACING,
    DEFAULT_RETRIEVAL, DEFAULT_SEED, PacingStrategy, R1_SLATE_SIZE, RankerStrategy,
    RetrievalStrategy, ScoringStrategy, StrategyProfile,
};
use clap::Parser;
use serde::Serialize;
use tempfile::NamedTempFile;
use world_generator::{
    DEFAULT_SEED as DEFAULT_MARKETPLACE_SEED, Marketplace, VisitGenerator, VisitGeneratorConfig,
    generate_marketplace,
};

#[derive(Debug, Parser)]
#[command(
    name = "visit-generator",
    about = "Generate user-independent visits and ad slot slates against ad-server with explicit 5-axis serving strategies"
)]
struct Args {
    /// Optional Schema v1 marketplace.json file (generated in memory if omitted).
    #[arg(long)]
    marketplace: Option<PathBuf>,

    /// Signed seed for in-memory marketplace generation (ignored when --marketplace is set).
    #[arg(long, default_value_t = DEFAULT_MARKETPLACE_SEED, allow_hyphen_values = true)]
    marketplace_seed: i64,

    /// Output JSON path for the generated impression slates.
    #[arg(long)]
    output: PathBuf,

    /// RNG seed for ad-server and visit generator.
    #[arg(long, default_value_t = DEFAULT_SEED)]
    seed: u64,

    /// Optional fixed upfront campaign budget override in USD cents (omit to randomize $1,000-$10,000).
    #[arg(long)]
    campaign_budget_cents: Option<u64>,

    /// Minimum randomized campaign budget in USD cents (default $1,000.00).
    #[arg(long, default_value_t = DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS)]
    campaign_budget_min_cents: u64,

    /// Maximum randomized campaign budget in USD cents (default $10,000.00).
    #[arg(long, default_value_t = DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS)]
    campaign_budget_max_cents: u64,

    /// Price per click in USD cents.
    #[arg(long, default_value_t = DEFAULT_CPC_CENTS)]
    cpc_cents: u64,

    /// Campaign retrieval strategy (Retrieve-V0/RT0).
    #[arg(long, alias = "retrieval-strategy", default_value_t = DEFAULT_RETRIEVAL)]
    retrieval: RetrievalStrategy,

    /// Ads-ranker strategy (R1/Rank-Uniform, R2=bandit+eps-greedy, R3/Rank-Greedy).
    #[arg(long, alias = "ranking-strategy", default_value_t = RankerStrategy::R1)]
    ranker: RankerStrategy,

    /// Candidate scoring strategy (S0/Score-Uniform, S1=ucb1_ctr, S2=emp_ctr, S3=ecpm, Score-R1; defaults from --ranker).
    #[arg(long, alias = "scoring-strategy")]
    scoring: Option<ScoringStrategy>,

    /// Budget pacing strategy (P0/none, P1=even_daily, P2=smooth_prob, pacing-1).
    #[arg(long, default_value_t = DEFAULT_PACING)]
    pacing: PacingStrategy,

    /// Bidding strategy (bidding-basebid/B0, bidding-autobid-v0/B1).
    #[arg(long, default_value_t = DEFAULT_BIDDING)]
    bidding: BiddingStrategy,

    /// Maximum distinct funded campaigns served per request.
    #[arg(long, default_value_t = R1_SLATE_SIZE)]
    slate_size: usize,

    /// Number of user-independent ad requests to issue to ad-server.
    #[arg(long, default_value_t = 25)]
    requests: usize,
}

#[derive(Serialize)]
struct RequestRecord {
    request_id: u64,
    #[serde(flatten)]
    response: AdResponse,
}

#[derive(Serialize)]
struct GeneratorOutput {
    schema_version: u32,
    retrieval: RetrievalStrategy,
    ranker: RankerStrategy,
    scoring: ScoringStrategy,
    pacing: PacingStrategy,
    bidding: BiddingStrategy,
    strategy: StrategyProfile,
    slate_size: usize,
    seed: u64,
    total_requests: usize,
    total_impressions: usize,
    slates: Vec<RequestRecord>,
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(args) {
        Ok(path) => {
            println!("{}", path.display());
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("Error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> Result<PathBuf, Box<dyn Error>> {
    let marketplace: Marketplace = match &args.marketplace {
        Some(path) => serde_json::from_reader(BufReader::new(File::open(path)?))?,
        None => generate_marketplace(args.marketplace_seed),
    };

    let scoring = args
        .scoring
        .unwrap_or_else(|| args.ranker.default_scoring());
    let strategy = StrategyProfile::new(
        args.retrieval,
        args.ranker,
        scoring,
        args.pacing,
        args.bidding,
        args.slate_size,
    );
    let mut ad_server = AdServer::from_marketplace(
        &marketplace,
        AdServerConfig {
            seed: args.seed,
            campaign_budget_cents: args.campaign_budget_cents,
            campaign_budget_min_cents: args.campaign_budget_min_cents,
            campaign_budget_max_cents: args.campaign_budget_max_cents,
            cpc_cents: args.cpc_cents,
            retrieval: args.retrieval,
            ranker: args.ranker,
            scoring,
            pacing: args.pacing,
            bidding: args.bidding,
            slate_size: args.slate_size,
        },
    )?;
    let user_ids: Vec<String> = marketplace.users.iter().map(|u| u.id.clone()).collect();
    let category_ids: Vec<String> = marketplace
        .categories
        .iter()
        .map(|c| c.id.clone())
        .collect();
    let mut generator = VisitGenerator::new(
        user_ids,
        category_ids,
        VisitGeneratorConfig {
            seed: args.seed,
            preference_seed: Some(marketplace.seed.cast_unsigned()),
            ..VisitGeneratorConfig::default()
        },
    )?;

    let mut slates = Vec::with_capacity(args.requests);
    let mut total_impressions = 0usize;
    for _ in 0..args.requests {
        let (request_id, mut response) = generator.generate_request(&mut ad_server)?;
        response.ads.truncate(generator.slots_per_visit());
        total_impressions += response.ads.len();
        slates.push(RequestRecord {
            request_id,
            response,
        });
    }

    let payload = GeneratorOutput {
        schema_version: ad_server::SCHEMA_VERSION,
        retrieval: args.retrieval,
        ranker: args.ranker,
        scoring,
        pacing: args.pacing,
        bidding: args.bidding,
        strategy,
        slate_size: args.slate_size,
        seed: args.seed,
        total_requests: slates.len(),
        total_impressions,
        slates,
    };

    let dir = args
        .output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir)?;
    let mut temp = NamedTempFile::new_in(dir)?;
    {
        let mut writer = BufWriter::new(temp.as_file_mut());
        serde_json::to_writer_pretty(&mut writer, &payload)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }
    temp.persist(&args.output)?;
    Ok(args.output)
}
