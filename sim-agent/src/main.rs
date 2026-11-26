//! CLI entry point for `sim-agent`: runs the 30-day CPC marketplace simulator, audits all
//! ledgers, runs automated analysis, and atomically exports JSON, CSV, SVG, and analysis artifacts.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use sim_agent::{
    AgentConfig, BiddingStrategy, DEFAULT_BIDDING, DEFAULT_CPC_CENTS, DEFAULT_IMPRESSIONS,
    DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS, DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS, DEFAULT_OUTPUT_DIR,
    DEFAULT_PACING, DEFAULT_RANKER, DEFAULT_RETRIEVAL, DEFAULT_SEED, DEFAULT_SLATE_SIZE,
    PacingStrategy, RankerStrategy, RetrievalStrategy, RunArtifacts, ScoringStrategy,
    run_and_export,
};

#[derive(Debug, Parser)]
#[command(
    name = "sim-agent",
    about = "Run the 30-day CPC marketplace simulator with explicit 5-axis serving strategies"
)]
struct Args {
    /// Directory where simulation, analysis, and budget artifacts are written.
    #[arg(long, default_value = DEFAULT_OUTPUT_DIR)]
    output_dir: PathBuf,

    /// Optional Schema v1 marketplace.json file (generated in memory if omitted).
    #[arg(long)]
    marketplace: Option<PathBuf>,

    /// Signed seed for marketplace generation (defaults to --seed; ignored if --marketplace is set).
    #[arg(long, allow_hyphen_values = true)]
    marketplace_seed: Option<i64>,

    /// Unsigned simulation RNG seed.
    #[arg(long, default_value_t = DEFAULT_SEED)]
    seed: u64,

    /// Optional fixed upfront 30-day budget for each campaign in USD cents (omit to randomize $1,000-$10,000).
    #[arg(long)]
    campaign_budget_cents: Option<u64>,

    /// Minimum randomized campaign budget in USD cents (default $1,000.00).
    #[arg(long, default_value_t = DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS)]
    campaign_budget_min_cents: u64,

    /// Maximum randomized campaign budget in USD cents (default $10,000.00).
    #[arg(long, default_value_t = DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS)]
    campaign_budget_max_cents: u64,

    /// Price of each click, in USD cents (must be positive).
    #[arg(long, alias = "base-bid-cents", default_value_t = DEFAULT_CPC_CENTS)]
    cpc_cents: u64,

    /// Exact number of impressions to deliver across 30 days (defaults to 1000000 unless --no-impression-target is set).
    #[arg(long, conflicts_with = "no_impression_target")]
    impressions: Option<u64>,

    /// Disable the impression target and run 600 user-independent ad-server requests/day across 30 days.
    #[arg(long, default_value_t = false)]
    no_impression_target: bool,

    /// Campaign retrieval strategy (Retrieve-V0/RT0).
    #[arg(long, alias = "retrieval-strategy", default_value_t = DEFAULT_RETRIEVAL)]
    retrieval: RetrievalStrategy,

    /// Ads-ranker strategy used by the ad-server (R1/Rank-Uniform, R2=bandit+eps-greedy, R3/Rank-Greedy).
    #[arg(long, alias = "ranking-strategy", default_value_t = DEFAULT_RANKER)]
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

    /// Maximum distinct funded campaigns returned per ad-server request.
    #[arg(long, default_value_t = DEFAULT_SLATE_SIZE)]
    slate_size: usize,
}

impl Args {
    fn resolve_impressions(&self) -> Option<u64> {
        if self.no_impression_target {
            None
        } else {
            Some(self.impressions.unwrap_or(DEFAULT_IMPRESSIONS))
        }
    }

    fn resolve_scoring(&self) -> ScoringStrategy {
        self.scoring
            .unwrap_or_else(|| self.ranker.default_scoring())
    }

    fn into_agent_config(&self) -> AgentConfig {
        AgentConfig {
            marketplace_path: self.marketplace.clone(),
            marketplace_seed: self
                .marketplace_seed
                .unwrap_or_else(|| self.seed.cast_signed()),
            sim_seed: self.seed,
            campaign_budget_cents: self.campaign_budget_cents,
            campaign_budget_min_cents: self.campaign_budget_min_cents,
            campaign_budget_max_cents: self.campaign_budget_max_cents,
            cpc_cents: self.cpc_cents,
            impressions: self.resolve_impressions(),
            retrieval: self.retrieval,
            ranker: self.ranker,
            scoring: self.resolve_scoring(),
            pacing: self.pacing,
            bidding: self.bidding,
            slate_size: self.slate_size,
        }
    }
}

fn main() -> ExitCode {
    let args = Args::parse();
    let config = args.into_agent_config();

    match run_and_export(&config, &args.output_dir) {
        Ok(artifacts) => {
            print_summary(&config, &artifacts);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("Error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn print_summary(config: &AgentConfig, artifacts: &RunArtifacts) {
    let ts = &artifacts.result.timeseries;
    let s = &ts.summary;
    let analysis = &artifacts.result.analysis;
    let active_end = ts
        .days
        .last()
        .map(|d| d.active_campaigns_end_of_day)
        .unwrap_or_else(|| s.total_campaigns.saturating_sub(s.exhausted_campaigns));

    let marketplace_desc = match (&config.marketplace_path, ts.config.marketplace_seed) {
        (Some(path), Some(seed)) => format!("file '{}' (seed {seed})", path.display()),
        (Some(path), None) => format!("file '{}'", path.display()),
        (None, Some(seed)) => format!("generated in memory (seed {seed})"),
        (None, None) => "generated in memory".to_string(),
    };
    let mode_desc = match ts.config.impressions {
        Some(target) => format!("target {} impressions", format_u64_commas(target)),
        None => "natural request stream (no impression target)".to_string(),
    };
    let budget_desc = match ts.config.campaign_budget_cents {
        Some(fixed) => format!("{}/campaign", format_usd_cents(fixed)),
        None => format!(
            "{}-{}/campaign (random)",
            format_usd_cents(ts.config.campaign_budget_min_cents),
            format_usd_cents(ts.config.campaign_budget_max_cents),
        ),
    };

    println!("sim-agent 30-day simulation complete");
    println!("  Marketplace : {marketplace_desc}");
    println!("  Strategy    : {}", ts.strategy);
    println!(
        "  Config      : sim_seed={}, retrieval={}, ranking={}, scoring={}, pacing={}, bidding={}, slate={}, budget={}, cpc={}, mode={mode_desc}",
        ts.config.sim_seed,
        ts.config.retrieval,
        ts.config.ranker,
        ts.config.scoring,
        ts.config.pacing,
        ts.config.bidding,
        ts.config.slate_size,
        budget_desc,
        format_usd_cents(ts.config.cpc_cents),
    );
    println!(
        "  Budget      : spent {} of {} ({:.2}% utilized, remaining {})",
        format_usd_cents(s.total_spend_cents),
        format_usd_cents(s.total_budget_cents),
        s.utilization_pct,
        format_usd_cents(s.remaining_budget_cents),
    );
    println!(
        "  Daily Spend : min {}, mean {}, max {} (CV {:.1}%)",
        format_usd_cents(s.min_daily_spend_cents),
        format_usd_cents_f64(s.mean_daily_spend_cents),
        format_usd_cents(s.max_daily_spend_cents),
        analysis.budget_depletion.daily_burn_cv_pct,
    );
    println!(
        "  Delivery    : {} impressions, {} clicks ({:.3}% CTR, eCPM {}, {:.2} ads/req)",
        format_u64_commas(s.total_impressions),
        format_u64_commas(s.total_clicks),
        s.ctr_pct,
        format_usd_cents_f64(analysis.ctr_performance.effective_cpm_cents),
        analysis.ctr_performance.avg_ads_per_request,
    );
    println!(
        "  Campaigns   : {} active / {} exhausted (of {} total, Gini {:.3}, {} insufficient-budget slots)",
        format_u64_commas(active_end as u64),
        format_u64_commas(s.exhausted_campaigns as u64),
        format_u64_commas(s.total_campaigns as u64),
        analysis.campaign_distribution.gini_spend_coefficient,
        format_u64_commas(s.insufficient_budget_opportunities),
    );
    println!("  Artifacts   :");
    println!("    - {}", artifacts.simulation_json.display());
    println!("    - {}", artifacts.timeseries_json.display());
    println!("    - {}", artifacts.timeseries_csv.display());
    println!("    - {}", artifacts.budget_plot_svg.display());
    println!("    - {}", artifacts.analysis_json.display());
}

fn format_u64_commas(value: u64) -> String {
    let digits = value.to_string();
    let len = digits.len();
    if len <= 3 {
        return digits;
    }
    let mut out = String::with_capacity(len + (len - 1) / 3);
    let first = len % 3;
    let first = if first == 0 { 3 } else { first };
    out.push_str(&digits[..first]);
    for chunk in digits[first..].as_bytes().chunks(3) {
        out.push(',');
        out.push_str(std::str::from_utf8(chunk).expect("ASCII digits"));
    }
    out
}

fn format_usd_cents(cents: u64) -> String {
    format!("${}.{:02}", format_u64_commas(cents / 100), cents % 100)
}

fn format_usd_cents_f64(cents: f64) -> String {
    if !cents.is_finite() || cents <= 0.0 {
        return "$0.00".to_string();
    }
    format_usd_cents(cents.round() as u64)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use clap::Parser;
    use sim_agent::{
        BiddingStrategy, DEFAULT_BIDDING, DEFAULT_CPC_CENTS, DEFAULT_IMPRESSIONS,
        DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS, DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS, DEFAULT_OUTPUT_DIR,
        DEFAULT_PACING, DEFAULT_RANKER, DEFAULT_RETRIEVAL, DEFAULT_SCORING, DEFAULT_SEED,
        DEFAULT_SLATE_SIZE, PacingStrategy, RankerStrategy, RetrievalStrategy, ScoringStrategy,
    };

    use crate::Args;

    #[test]
    fn cli_defaults_match_contract() {
        let args = Args::try_parse_from(["sim-agent"]).unwrap();
        assert_eq!(args.output_dir, PathBuf::from(DEFAULT_OUTPUT_DIR));
        assert_eq!(args.marketplace, None);
        assert_eq!(args.marketplace_seed, None);
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
        assert!(!args.no_impression_target);
        assert_eq!(args.retrieval, DEFAULT_RETRIEVAL);
        assert_eq!(args.ranker, DEFAULT_RANKER);
        assert_eq!(args.scoring, None);
        assert_eq!(args.pacing, DEFAULT_PACING);
        assert_eq!(args.bidding, DEFAULT_BIDDING);
        assert_eq!(args.slate_size, DEFAULT_SLATE_SIZE);
        assert_eq!(args.resolve_impressions(), Some(DEFAULT_IMPRESSIONS));

        let cfg = args.into_agent_config();
        assert_eq!(cfg.marketplace_seed, 42);
        assert_eq!(cfg.sim_seed, 42);
        assert_eq!(cfg.campaign_budget_cents, None);
        assert_eq!(
            cfg.campaign_budget_min_cents,
            DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS
        );
        assert_eq!(
            cfg.campaign_budget_max_cents,
            DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS
        );
        assert_eq!(cfg.impressions, Some(1_000_000));
        assert_eq!(cfg.retrieval, DEFAULT_RETRIEVAL);
        assert_eq!(cfg.ranker, DEFAULT_RANKER);
        assert_eq!(cfg.scoring, DEFAULT_SCORING);
        assert_eq!(cfg.pacing, DEFAULT_PACING);
        assert_eq!(cfg.bidding, DEFAULT_BIDDING);
        assert_eq!(cfg.slate_size, DEFAULT_SLATE_SIZE);

        // Selecting R2 without --scoring resolves to S1 (Online UCB1 CTR).
        let r2_default = Args::try_parse_from(["sim-agent", "--ranker", "R2"])
            .unwrap()
            .into_agent_config();
        assert_eq!(r2_default.retrieval, RetrievalStrategy::RetrieveV0);
        assert_eq!(r2_default.ranker, RankerStrategy::R2);
        assert_eq!(r2_default.scoring, ScoringStrategy::S1);
        assert_eq!(r2_default.pacing, PacingStrategy::P0);
        assert_eq!(r2_default.bidding, BiddingStrategy::Basebid);

        // Explicit retrieval, ranking, scoring, pacing, and bidding override defaults cleanly.
        let custom = Args::try_parse_from([
            "sim-agent",
            "--retrieval",
            "Retrieve-V0",
            "--ranker",
            "R3",
            "--scoring",
            "S2",
            "--pacing",
            "pacing-1",
            "--bidding",
            "bidding-autobid-v0",
        ])
        .unwrap()
        .into_agent_config();
        assert_eq!(custom.retrieval, RetrievalStrategy::RetrieveV0);
        assert_eq!(custom.ranker, RankerStrategy::R3);
        assert_eq!(custom.scoring, ScoringStrategy::S2);
        assert_eq!(custom.pacing, PacingStrategy::Pacing1);
        assert_eq!(custom.bidding, BiddingStrategy::AutobidV0);
    }

    #[test]
    fn cli_rejects_impressions_and_no_impression_target_conflict() {
        assert!(
            Args::try_parse_from([
                "sim-agent",
                "--impressions",
                "1000",
                "--no-impression-target",
            ])
            .is_err()
        );
    }

    #[test]
    fn cli_accepts_negative_marketplace_seed_and_extreme_seeds() {
        let neg = Args::try_parse_from(["sim-agent", "--marketplace-seed", "-7"]).unwrap();
        assert_eq!(neg.marketplace_seed, Some(-7));
        assert_eq!(neg.into_agent_config().marketplace_seed, -7);

        let min_seed = Args::try_parse_from([
            "sim-agent",
            "--marketplace-seed",
            "-9223372036854775808",
        ])
        .unwrap();
        assert_eq!(min_seed.marketplace_seed, Some(i64::MIN));
        assert_eq!(min_seed.into_agent_config().marketplace_seed, i64::MIN);

        let max_u64 =
            Args::try_parse_from(["sim-agent", "--seed", "18446744073709551615"]).unwrap();
        assert_eq!(max_u64.seed, u64::MAX);
        assert_eq!(max_u64.into_agent_config().marketplace_seed, -1);
    }

    #[test]
    fn cli_resolves_natural_replay_and_explicit_target() {
        let natural = Args::try_parse_from(["sim-agent", "--no-impression-target"]).unwrap();
        assert_eq!(natural.resolve_impressions(), None);
        assert_eq!(natural.into_agent_config().impressions, None);

        let explicit = Args::try_parse_from(["sim-agent", "--impressions", "5000"]).unwrap();
        assert_eq!(explicit.resolve_impressions(), Some(5_000));
        assert_eq!(explicit.into_agent_config().impressions, Some(5_000));

        let zero_target = Args::try_parse_from(["sim-agent", "--impressions", "0"]).unwrap();
        assert_eq!(zero_target.resolve_impressions(), Some(0));
    }
}
