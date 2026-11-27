use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;

use ads_marketplace_simulation::OwnedReport;
use world_generator::generate_marketplace;
use sim_agent::{
    ANALYSIS_JSON_FILENAME, BUDGET_PLOT_SVG_FILENAME, BudgetTimeSeries, SIMULATION_REPORT_FILENAME,
    SimulationAnalysis, TIMESERIES_CSV_FILENAME, TIMESERIES_JSON_FILENAME, audit_report,
    render_budget_svg,
};
use tempfile::TempDir;

const SIM_AGENT_BIN: &str = env!("CARGO_BIN_EXE_sim-agent");

fn list_filenames(dir: &Path) -> BTreeSet<String> {
    fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

fn expected_artifact_names() -> BTreeSet<String> {
    [
        SIMULATION_REPORT_FILENAME,
        TIMESERIES_JSON_FILENAME,
        TIMESERIES_CSV_FILENAME,
        BUDGET_PLOT_SVG_FILENAME,
        ANALYSIS_JSON_FILENAME,
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

#[test]
fn cli_golden_baseline_writes_atomic_artifacts_and_cross_checks_csv_json_svg() {
    let dir = TempDir::new().unwrap();
    let out_dir = dir.path().join("nested").join("deep").join("out");

    let output = Command::new(SIM_AGENT_BIN)
        .arg("--output-dir")
        .arg(&out_dir)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("$1,000.00-$10,000.00/campaign (random)"));
    assert!(stdout.contains(BUDGET_PLOT_SVG_FILENAME));
    assert!(stdout.contains(ANALYSIS_JSON_FILENAME));

    assert_eq!(list_filenames(&out_dir), expected_artifact_names());

    let sim_raw = fs::read_to_string(out_dir.join(SIMULATION_REPORT_FILENAME)).unwrap();
    let report: OwnedReport = serde_json::from_str(&sim_raw).unwrap();
    audit_report(&report).unwrap();
    assert_eq!(
        sim_raw,
        format!("{}\n", serde_json::to_string(&report).unwrap())
    );
    assert!(
        report
            .campaigns
            .iter()
            .all(|c| (100_000..=1_000_000).contains(&c.budget_cents))
    );

    let ts_raw = fs::read_to_string(out_dir.join(TIMESERIES_JSON_FILENAME)).unwrap();
    let ts: BudgetTimeSeries = serde_json::from_str(&ts_raw).unwrap();
    assert_eq!(ts.schema_version, 1);
    assert_eq!(ts.currency, "USD");
    assert_eq!(ts.config.marketplace_seed, Some(42));
    assert_eq!(ts.config.sim_seed, 42);
    assert_eq!(ts.config.campaign_budget_cents, None);
    assert_eq!(ts.config.campaign_budget_min_cents, 100_000);
    assert_eq!(ts.config.campaign_budget_max_cents, 1_000_000);
    assert!(ts.summary.total_budget_cents >= 500 * 100_000);
    assert!(ts.summary.total_budget_cents <= 500 * 1_000_000);
    assert_eq!(ts.summary.total_impressions, 1_000_000);
    assert!(ts.summary.total_clicks > 1_500 && ts.summary.total_clicks < 2_500);
    assert_eq!(ts.summary.total_spend_cents, ts.summary.total_clicks * 100);
    assert_eq!(ts.days.len(), 30);
    assert_eq!(ts.days[29].cumulative_spend_cents, ts.summary.total_spend_cents);

    let analysis_raw = fs::read_to_string(out_dir.join(ANALYSIS_JSON_FILENAME)).unwrap();
    let analysis: SimulationAnalysis = serde_json::from_str(&analysis_raw).unwrap();
    assert_eq!(
        analysis.budget_depletion.total_spend_cents,
        ts.summary.total_spend_cents
    );
    assert_eq!(analysis.ctr_performance.total_clicks, ts.summary.total_clicks);
    assert!(!analysis.findings.is_empty());

    // Cross-check every CSV row against BudgetTimeSeries.
    let csv_raw = fs::read_to_string(out_dir.join(TIMESERIES_CSV_FILENAME)).unwrap();
    let csv_lines: Vec<&str> = csv_raw.lines().collect();
    assert_eq!(csv_lines.len(), 31);
    assert_eq!(
        csv_lines[0],
        "day,impressions,clicks,daily_spend_cents,cumulative_spend_cents,remaining_budget_cents,utilization_pct,active_campaigns_end_of_day,insufficient_budget"
    );
    for (i, line) in csv_lines.iter().skip(1).enumerate() {
        let cols: Vec<&str> = line.split(',').collect();
        assert_eq!(cols.len(), 9);
        let pt = &ts.days[i];
        assert_eq!(cols[0].parse::<usize>().unwrap(), pt.day);
        assert_eq!(cols[1].parse::<u64>().unwrap(), pt.impressions);
        assert_eq!(cols[2].parse::<u64>().unwrap(), pt.clicks);
        assert_eq!(cols[3].parse::<u64>().unwrap(), pt.daily_spend_cents);
        assert_eq!(cols[4].parse::<u64>().unwrap(), pt.cumulative_spend_cents);
        assert_eq!(cols[5].parse::<u64>().unwrap(), pt.remaining_budget_cents);
        assert!((cols[6].parse::<f64>().unwrap() - pt.utilization_pct).abs() < 1e-6);
        assert_eq!(
            cols[7].parse::<usize>().unwrap(),
            pt.active_campaigns_end_of_day
        );
        assert_eq!(cols[8].parse::<u64>().unwrap(), pt.insufficient_budget);
    }

    // Cross-check SVG exact parity & integrity.
    let svg_raw = fs::read_to_string(out_dir.join(BUDGET_PLOT_SVG_FILENAME)).unwrap();
    assert_eq!(svg_raw, render_budget_svg(&ts));
    assert!(svg_raw.starts_with("<svg "));
    assert!(svg_raw.ends_with("</svg>"));
    assert!(svg_raw.contains("$1,000.00–$10,000.00/Campaign"));
    assert!(!svg_raw.contains("NaN"));
    assert!(!svg_raw.contains("Infinity"));
    assert!(!svg_raw.contains("-0.00"));
}

#[test]
fn cli_natural_replay_produces_r1_baseline_artifacts() {
    let dir = TempDir::new().unwrap();
    let out_dir = dir.path().join("natural");

    let output = Command::new(SIM_AGENT_BIN)
        .args(["--no-impression-target", "--output-dir"])
        .arg(&out_dir)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(list_filenames(&out_dir), expected_artifact_names());

    let ts: BudgetTimeSeries =
        serde_json::from_str(&fs::read_to_string(out_dir.join(TIMESERIES_JSON_FILENAME)).unwrap())
            .unwrap();
    assert_eq!(ts.config.impressions, None);
    assert_eq!(ts.summary.total_impressions, 360_000);
    assert!(ts.summary.total_clicks > 500 && ts.summary.total_clicks < 1_000);
    assert_eq!(ts.summary.total_spend_cents, ts.summary.total_clicks * 100);

    let svg = fs::read_to_string(out_dir.join(BUDGET_PLOT_SVG_FILENAME)).unwrap();
    assert!(svg.contains("Natural Replay"));
}

#[test]
fn cli_zero_spend_and_exhaustion_artifacts_are_valid() {
    let dir = TempDir::new().unwrap();

    // Zero-spend scenario.
    let zero_dir = dir.path().join("zero-spend");
    let zero_out = Command::new(SIM_AGENT_BIN)
        .args([
            "--campaign-budget-cents",
            "50",
            "--cpc-cents",
            "100",
            "--no-impression-target",
            "--output-dir",
        ])
        .arg(&zero_dir)
        .output()
        .unwrap();
    assert!(
        zero_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&zero_out.stderr)
    );
    let zero_ts: BudgetTimeSeries = serde_json::from_str(
        &fs::read_to_string(zero_dir.join(TIMESERIES_JSON_FILENAME)).unwrap(),
    )
    .unwrap();
    assert_eq!(zero_ts.summary.total_spend_cents, 0);
    assert_eq!(zero_ts.summary.exhausted_campaigns, 500);
    let zero_svg = fs::read_to_string(zero_dir.join(BUDGET_PLOT_SVG_FILENAME)).unwrap();
    assert!(zero_svg.contains("$0.00 spent across 30 days"));
    assert!(!zero_svg.contains("NaN"));
    assert!(!zero_svg.contains("Infinity"));
    assert!(!zero_svg.contains("-0.00"));

    // Budget exhaustion scenario.
    let exh_dir = dir.path().join("exhaustion");
    let exh_out = Command::new(SIM_AGENT_BIN)
        .args([
            "--campaign-budget-cents",
            "100",
            "--cpc-cents",
            "100",
            "--no-impression-target",
            "--output-dir",
        ])
        .arg(&exh_dir)
        .output()
        .unwrap();
    assert!(
        exh_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&exh_out.stderr)
    );
    let exh_ts: BudgetTimeSeries =
        serde_json::from_str(&fs::read_to_string(exh_dir.join(TIMESERIES_JSON_FILENAME)).unwrap())
            .unwrap();
    assert!(exh_ts.summary.exhausted_campaigns > 0);
    let mut prev_active = exh_ts.summary.total_campaigns;
    for day in &exh_ts.days {
        assert!(day.active_campaigns_end_of_day <= prev_active);
        prev_active = day.active_campaigns_end_of_day;
    }
    let exh_svg = fs::read_to_string(exh_dir.join(BUDGET_PLOT_SVG_FILENAME)).unwrap();
    assert!(exh_svg.contains("class=\"budget-cap-line\""));
    assert!(!exh_svg.contains("NaN"));
    assert!(!exh_svg.contains("Infinity"));
    assert!(!exh_svg.contains("-0.00"));
}

#[test]
fn cli_marketplace_file_loading_propagates_seed_and_matches_in_memory() {
    let dir = TempDir::new().unwrap();
    let mkt_path = dir.path().join("marketplace.json");
    let mkt = generate_marketplace(-123);
    fs::write(&mkt_path, serde_json::to_vec(&mkt).unwrap()).unwrap();

    let from_file_dir = dir.path().join("from-file");
    let file_out = Command::new(SIM_AGENT_BIN)
        .arg("--marketplace")
        .arg(&mkt_path)
        .args([
            "--marketplace-seed",
            "999",
            "--seed",
            "42",
            "--impressions",
            "3000",
            "--output-dir",
        ])
        .arg(&from_file_dir)
        .output()
        .unwrap();
    assert!(
        file_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&file_out.stderr)
    );

    let in_mem_dir = dir.path().join("in-memory");
    let mem_out = Command::new(SIM_AGENT_BIN)
        .args([
            "--marketplace-seed",
            "-123",
            "--seed",
            "42",
            "--impressions",
            "3000",
            "--output-dir",
        ])
        .arg(&in_mem_dir)
        .output()
        .unwrap();
    assert!(
        mem_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&mem_out.stderr)
    );

    let file_ts: BudgetTimeSeries = serde_json::from_str(
        &fs::read_to_string(from_file_dir.join(TIMESERIES_JSON_FILENAME)).unwrap(),
    )
    .unwrap();
    assert_eq!(file_ts.config.marketplace_seed, Some(-123));

    for name in expected_artifact_names() {
        let a = fs::read(from_file_dir.join(&name)).unwrap();
        let b = fs::read(in_mem_dir.join(&name)).unwrap();
        assert_eq!(a, b, "artifact mismatch for {name}");
    }
}

#[test]
fn cli_negative_marketplace_seed_succeeds() {
    let dir = TempDir::new().unwrap();
    let out_dir = dir.path().join("neg-seed");

    let output = Command::new(SIM_AGENT_BIN)
        .args([
            "--marketplace-seed",
            "-7",
            "--seed",
            "42",
            "--impressions",
            "3000",
            "--output-dir",
        ])
        .arg(&out_dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let ts: BudgetTimeSeries =
        serde_json::from_str(&fs::read_to_string(out_dir.join(TIMESERIES_JSON_FILENAME)).unwrap())
            .unwrap();
    assert_eq!(ts.config.marketplace_seed, Some(-7));
}

#[test]
fn cli_repeated_runs_are_byte_for_byte_deterministic_and_atomic_overwrite() {
    let dir = TempDir::new().unwrap();
    let out_a = dir.path().join("run-a");
    let out_b = dir.path().join("run-b");

    for out in [&out_a, &out_b] {
        let status = Command::new(SIM_AGENT_BIN)
            .args(["--seed", "77", "--impressions", "5000", "--output-dir"])
            .arg(out)
            .status()
            .unwrap();
        assert!(status.success());
    }

    for name in expected_artifact_names() {
        assert_eq!(
            fs::read(out_a.join(&name)).unwrap(),
            fs::read(out_b.join(&name)).unwrap(),
            "non-deterministic artifact {name}"
        );
    }

    // Overwrite run-a in place and confirm byte-for-byte equality + zero stray temp files.
    let rerun = Command::new(SIM_AGENT_BIN)
        .args(["--seed", "77", "--impressions", "5000", "--output-dir"])
        .arg(&out_a)
        .status()
        .unwrap();
    assert!(rerun.success());
    assert_eq!(list_filenames(&out_a), expected_artifact_names());
    for name in expected_artifact_names() {
        assert_eq!(
            fs::read(out_a.join(&name)).unwrap(),
            fs::read(out_b.join(&name)).unwrap(),
        );
    }
}

#[test]
fn cli_error_cases_exit_nonzero() {
    let dir = TempDir::new().unwrap();

    // Conflicting --impressions and --no-impression-target.
    let conflict = Command::new(SIM_AGENT_BIN)
        .args([
            "--impressions",
            "1000",
            "--no-impression-target",
            "--output-dir",
        ])
        .arg(dir.path().join("conflict"))
        .output()
        .unwrap();
    assert!(!conflict.status.success());

    // Zero CPC.
    let zero_cpc_dir = dir.path().join("zero-cpc");
    let zero_cpc = Command::new(SIM_AGENT_BIN)
        .args([
            "--cpc-cents",
            "0",
            "--no-impression-target",
            "--output-dir",
        ])
        .arg(&zero_cpc_dir)
        .output()
        .unwrap();
    assert!(!zero_cpc.status.success());
    assert!(!zero_cpc_dir.exists());

    // Unreachable impression target.
    let unreachable_dir = dir.path().join("unreachable");
    let unreachable = Command::new(SIM_AGENT_BIN)
        .args([
            "--campaign-budget-cents",
            "50",
            "--cpc-cents",
            "100",
            "--impressions",
            "100",
            "--output-dir",
        ])
        .arg(&unreachable_dir)
        .output()
        .unwrap();
    assert!(!unreachable.status.success());
    assert!(
        String::from_utf8_lossy(&unreachable.stderr).contains("target cannot be reached")
    );
    assert!(!unreachable_dir.exists());

    // Missing marketplace file.
    let missing_dir = dir.path().join("missing-mkt");
    let missing = Command::new(SIM_AGENT_BIN)
        .arg("--marketplace")
        .arg(dir.path().join("nonexistent.json"))
        .args(["--no-impression-target", "--output-dir"])
        .arg(&missing_dir)
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(!missing_dir.exists());

    // Output dir points to an existing regular file.
    let blocker = dir.path().join("existing-file");
    fs::write(&blocker, "not a directory").unwrap();
    let blocked = Command::new(SIM_AGENT_BIN)
        .args(["--impressions", "100", "--output-dir"])
        .arg(&blocker)
        .output()
        .unwrap();
    assert!(!blocked.status.success());

    // Incompatible strategy combination: R1 (random) + S1 (UCB1 CTR).
    let invalid_combo_dir = dir.path().join("invalid-combo");
    let invalid_combo = Command::new(SIM_AGENT_BIN)
        .args([
            "--ranker",
            "R1",
            "--scoring",
            "S1",
            "--pacing",
            "P0",
            "--impressions",
            "1000",
            "--output-dir",
        ])
        .arg(&invalid_combo_dir)
        .output()
        .unwrap();
    assert!(!invalid_combo.status.success());
    assert!(
        String::from_utf8_lossy(&invalid_combo.stderr).contains("incompatible strategy combination")
    );
    assert!(!invalid_combo_dir.exists());
}

#[test]
fn cli_explicit_ranking_scoring_pacing_recorded_across_all_artifacts() {
    let dir = TempDir::new().unwrap();
    let out_dir = dir.path().join("r2-s2-p1");

    let output = Command::new(SIM_AGENT_BIN)
        .args([
            "--retrieval",
            "Retrieve-V0",
            "--ranker",
            "R2",
            "--scoring",
            "S2",
            "--pacing",
            "P1",
            "--bidding",
            "bidding-autobid-v0",
            "--impressions",
            "6000",
            "--output-dir",
        ])
        .arg(&out_dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Strategy    : Retrieval Retrieve-V0"));
    assert!(stdout.contains("Ranking R2"));
    assert!(stdout.contains("Scoring S2"));
    assert!(stdout.contains("Pacing P1"));
    assert!(stdout.contains("Bidding bidding-autobid-v0"));

    let sim_report: ads_marketplace_simulation::OwnedReport = serde_json::from_str(
        &fs::read_to_string(out_dir.join(sim_agent::SIMULATION_REPORT_FILENAME)).unwrap(),
    )
    .unwrap();
    assert_eq!(
        sim_report.config.retrieval,
        sim_agent::RetrievalStrategy::RetrieveV0
    );
    assert_eq!(sim_report.config.ranker, sim_agent::RankerStrategy::R2);
    assert_eq!(sim_report.config.scoring, sim_agent::ScoringStrategy::S2);
    assert_eq!(sim_report.config.pacing, sim_agent::PacingStrategy::P1);
    assert_eq!(
        sim_report.config.bidding,
        sim_agent::BiddingStrategy::AutobidV0
    );
    assert_eq!(
        sim_report.strategy.retrieval,
        sim_agent::RetrievalStrategy::RetrieveV0
    );
    assert_eq!(sim_report.strategy.ranking, sim_agent::RankerStrategy::R2);
    assert_eq!(sim_report.strategy.scoring, sim_agent::ScoringStrategy::S2);
    assert_eq!(sim_report.strategy.pacing, sim_agent::PacingStrategy::P1);
    assert_eq!(
        sim_report.strategy.bidding,
        sim_agent::BiddingStrategy::AutobidV0
    );

    let ts: BudgetTimeSeries =
        serde_json::from_str(&fs::read_to_string(out_dir.join(TIMESERIES_JSON_FILENAME)).unwrap())
            .unwrap();
    assert_eq!(
        ts.config.retrieval,
        sim_agent::RetrievalStrategy::RetrieveV0
    );
    assert_eq!(ts.config.ranker, sim_agent::RankerStrategy::R2);
    assert_eq!(ts.config.scoring, sim_agent::ScoringStrategy::S2);
    assert_eq!(ts.config.pacing, sim_agent::PacingStrategy::P1);
    assert_eq!(ts.config.bidding, sim_agent::BiddingStrategy::AutobidV0);
    assert_eq!(
        ts.strategy.retrieval,
        sim_agent::RetrievalStrategy::RetrieveV0
    );
    assert_eq!(ts.strategy.scoring, sim_agent::ScoringStrategy::S2);
    assert_eq!(ts.strategy.pacing, sim_agent::PacingStrategy::P1);
    assert_eq!(ts.strategy.bidding, sim_agent::BiddingStrategy::AutobidV0);

    let analysis: sim_agent::SimulationAnalysis = serde_json::from_str(
        &fs::read_to_string(out_dir.join(sim_agent::ANALYSIS_JSON_FILENAME)).unwrap(),
    )
    .unwrap();
    assert_eq!(
        analysis.retrieval,
        sim_agent::RetrievalStrategy::RetrieveV0
    );
    assert_eq!(analysis.ranker, sim_agent::RankerStrategy::R2);
    assert_eq!(analysis.scoring, sim_agent::ScoringStrategy::S2);
    assert_eq!(analysis.pacing, sim_agent::PacingStrategy::P1);
    assert_eq!(analysis.bidding, sim_agent::BiddingStrategy::AutobidV0);
    assert_eq!(
        analysis.strategy.retrieval,
        sim_agent::RetrievalStrategy::RetrieveV0
    );
    assert_eq!(analysis.strategy.ranking, sim_agent::RankerStrategy::R2);
    assert_eq!(analysis.strategy.scoring, sim_agent::ScoringStrategy::S2);
    assert_eq!(analysis.strategy.pacing, sim_agent::PacingStrategy::P1);
    assert_eq!(
        analysis.strategy.bidding,
        sim_agent::BiddingStrategy::AutobidV0
    );

    let svg = fs::read_to_string(out_dir.join(sim_agent::BUDGET_PLOT_SVG_FILENAME)).unwrap();
    assert!(svg.contains("Retrieve-V0"));
    assert!(svg.contains("Ranker R2 / Scoring S2 / Pacing P1 / bidding-autobid-v0 (Slate 20)"));
}
