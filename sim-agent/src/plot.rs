//! Deterministic pure-Rust dual-panel SVG plotter for 30-day simulation budget spend.

use std::fmt::Write as _;

use crate::agent::{
    BudgetTimeSeries, format_u64_commas, format_u128_commas, format_usd_cents, format_usd_cents_f64,
};

const SVG_W: f64 = 1100.0;
const SVG_H: f64 = 680.0;
const PAD_X: f64 = 28.0;
const INNER_W: f64 = SVG_W - 2.0 * PAD_X; // 1044.0

const PLOT_LEFT: f64 = 124.0;
const PLOT_RIGHT: f64 = 1048.0;
const PLOT_W: f64 = PLOT_RIGHT - PLOT_LEFT; // 924.0

const TOP_Y_TOP: f64 = 186.0;
const TOP_Y_BOTTOM: f64 = 362.0;
const TOP_PLOT_H: f64 = TOP_Y_BOTTOM - TOP_Y_TOP; // 176.0

const BOT_Y_TOP: f64 = 452.0;
const BOT_Y_BOTTOM: f64 = 620.0;
const BOT_PLOT_H: f64 = BOT_Y_BOTTOM - BOT_Y_TOP; // 168.0

#[derive(Clone, Debug, PartialEq, Eq)]
struct YScale {
    max_cents: u128,
    ticks_cents: Vec<u128>,
}

/// Render a standalone, deterministic, inline-embeddable `1100x680` dual-panel SVG showing
/// 30-day KPI summary metrics, Daily Budget Spent bars, and Cumulative Budget Spent curve.
pub fn render_budget_svg(series: &BudgetTimeSeries) -> String {
    let summary = &series.summary;
    let config = &series.config;
    let n = series.days.len();

    let total_spent_str = format_usd_cents(summary.total_spend_cents);
    let total_budget_str = format_usd_cents(summary.total_budget_cents);
    let remaining_str = format_usd_cents(summary.remaining_budget_cents);
    let util_str = format_pct(summary.utilization_pct);
    let mean_daily_str = format_usd_cents_f64(summary.mean_daily_spend_cents);
    let ctr_str = format_pct(summary.ctr_pct);

    let mode_label = match config.impressions {
        Some(target) => format!("Target {} Impressions", format_u64_commas(target)),
        None => "Natural Replay (No Target)".to_string(),
    };
    let mkt_seed_label = match config.marketplace_seed {
        Some(seed) => format!("Mkt Seed {seed}"),
        None => "Loaded Report".to_string(),
    };
    let budget_label = match config.campaign_budget_cents {
        Some(fixed) => format!("{}/Campaign", format_usd_cents(fixed)),
        None => format!(
            "{}–{}/Campaign",
            format_usd_cents(config.campaign_budget_min_cents),
            format_usd_cents(config.campaign_budget_max_cents),
        ),
    };
    let subtitle = format!(
        "Seed {} · {} · {} · Ranker {} / Scoring {} / Pacing {} / {} (Slate {}) · {} Campaigns · {} · {} CPC · {}",
        config.sim_seed,
        mkt_seed_label,
        config.retrieval,
        config.ranker,
        config.scoring,
        config.pacing,
        config.bidding,
        config.slate_size,
        format_u64_commas(summary.total_campaigns as u64),
        budget_label,
        format_usd_cents(config.cpc_cents),
        mode_label,
    );
    let desc = format!(
        "30-day ads marketplace budget report: {} spent of {} total budget ({} utilization) across {} clicks and {} impressions.",
        total_spent_str,
        total_budget_str,
        util_str,
        format_u64_commas(summary.total_clicks),
        format_u64_commas(summary.total_impressions),
    );
    let currency_escaped = xml_escape(&series.currency);

    let active_end = series
        .days
        .last()
        .map(|d| d.active_campaigns_end_of_day)
        .unwrap_or_else(|| {
            summary
                .total_campaigns
                .saturating_sub(summary.exhausted_campaigns)
        });

    let mut svg = String::with_capacity(20_480);

    let _ = write!(
        svg,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1100\" height=\"680\" viewBox=\"0 0 1100 680\" role=\"img\" aria-labelledby=\"sim-budget-title sim-budget-desc\">\n"
    );
    let _ = writeln!(
        svg,
        "  <title id=\"sim-budget-title\">30-Day Ads Simulation — Budget Spent</title>"
    );
    let _ = writeln!(
        svg,
        "  <desc id=\"sim-budget-desc\">{}</desc>",
        xml_escape(&desc)
    );
    svg.push_str(
        "  <defs>\n\
         \x20   <style>\n\
         \x20     text {{ font-family: system-ui, -apple-system, 'Segoe UI', Roboto, Helvetica, Arial, sans-serif; font-variant-numeric: tabular-nums; }}\n\
         \x20   </style>\n\
         \x20   <linearGradient id=\"sim-budget-bar-grad\" x1=\"0\" y1=\"0\" x2=\"0\" y2=\"1\">\n\
         \x20     <stop offset=\"0%\" stop-color=\"#3B82F6\"/>\n\
         \x20     <stop offset=\"100%\" stop-color=\"#2563EB\"/>\n\
         \x20   </linearGradient>\n\
         \x20   <linearGradient id=\"sim-budget-bar-peak-grad\" x1=\"0\" y1=\"0\" x2=\"0\" y2=\"1\">\n\
         \x20     <stop offset=\"0%\" stop-color=\"#1D4ED8\"/>\n\
         \x20     <stop offset=\"100%\" stop-color=\"#1E40AF\"/>\n\
         \x20   </linearGradient>\n\
         \x20   <linearGradient id=\"sim-budget-cum-area-grad\" x1=\"0\" y1=\"0\" x2=\"0\" y2=\"1\">\n\
         \x20     <stop offset=\"0%\" stop-color=\"#0D9488\" stop-opacity=\"0.22\"/>\n\
         \x20     <stop offset=\"100%\" stop-color=\"#0D9488\" stop-opacity=\"0.02\"/>\n\
         \x20   </linearGradient>\n\
         \x20 </defs>\n",
    );

    // Outer rounded editorial card canvas.
    let _ = writeln!(
        svg,
        "  <rect x=\"0.50\" y=\"0.50\" width=\"{:.2}\" height=\"{:.2}\" rx=\"12\" fill=\"#F8FAFC\" stroke=\"#CBD5E1\" stroke-width=\"1\"/>",
        SVG_W - 1.0,
        SVG_H - 1.0
    );

    // BAND 1: Header.
    svg.push_str("  <g class=\"header-band\">\n");
    let _ = writeln!(
        svg,
        "    <text x=\"{PAD_X:.2}\" y=\"40.00\" font-size=\"20\" font-weight=\"700\" fill=\"#0F172A\">30-Day Ads Simulation — Budget Spent</text>"
    );
    let _ = writeln!(
        svg,
        "    <text x=\"{PAD_X:.2}\" y=\"59.00\" font-size=\"11.5\" fill=\"#475569\">{}</text>",
        xml_escape(&subtitle)
    );
    let schema_pill_text = format!("Schema v{} · {}", series.schema_version, series.currency);
    let schema_pill_w = ((schema_pill_text.len() as f64) * 6.4 + 22.0).clamp(116.0, 220.0);
    let schema_pill_x = SVG_W - PAD_X - schema_pill_w;
    let _ = writeln!(
        svg,
        "    <rect x=\"{:.2}\" y=\"24.00\" width=\"{:.2}\" height=\"22.00\" rx=\"11\" fill=\"#FFFFFF\" stroke=\"#CBD5E1\" stroke-width=\"1\"/>",
        safe_coord(schema_pill_x),
        safe_coord(schema_pill_w)
    );
    let _ = writeln!(
        svg,
        "    <text x=\"{:.2}\" y=\"38.50\" text-anchor=\"middle\" font-size=\"10.5\" font-weight=\"600\" fill=\"#475569\">Schema v{} · {}</text>",
        safe_coord(schema_pill_x + schema_pill_w / 2.0),
        series.schema_version,
        currency_escaped
    );
    svg.push_str("  </g>\n");

    // BAND 2: 6 KPI Cards.
    let missed_sub = if summary.insufficient_budget_opportunities > 0 {
        (
            format!(
                "{} missed slots",
                format_u64_commas(summary.insufficient_budget_opportunities)
            ),
            "#B91C1C",
        )
    } else {
        ("0 missed slots".to_string(), "#64748B")
    };
    let daily_range_sub = format!(
        "{} – {}",
        format_usd_compact(summary.min_daily_spend_cents),
        format_usd_compact(summary.max_daily_spend_cents)
    );
    let active_exhausted_val = format!(
        "{} / {}",
        format_u64_commas(active_end as u64),
        format_u64_commas(summary.exhausted_campaigns as u64)
    );
    let active_accent = if summary.exhausted_campaigns > 0 {
        "#DC2626"
    } else {
        "#16A34A"
    };

    let kpi_cards = [
        (
            "TOTAL SPENT",
            total_spent_str.clone(),
            format!("Rem {remaining_str}"),
            "#64748B",
            "#2563EB",
        ),
        (
            "TOTAL BUDGET",
            total_budget_str.clone(),
            format!(
                "{} campaigns",
                format_u64_commas(summary.total_campaigns as u64)
            ),
            "#64748B",
            "#475569",
        ),
        (
            "UTILIZATION",
            util_str.clone(),
            missed_sub.0,
            missed_sub.1,
            "#0D9488",
        ),
        (
            "AVG DAILY SPEND",
            mean_daily_str.clone(),
            daily_range_sub,
            "#64748B",
            "#D97706",
        ),
        (
            "CLICKS & CTR",
            format_u64_commas(summary.total_clicks),
            format!("CTR {ctr_str}"),
            "#64748B",
            "#7C3AED",
        ),
        (
            "ACTIVE / EXHAUSTED",
            active_exhausted_val,
            format!(
                "of {} total",
                format_u64_commas(summary.total_campaigns as u64)
            ),
            "#64748B",
            active_accent,
        ),
    ];

    svg.push_str("  <g class=\"kpi-strip\">\n");
    for (idx, (label, value, subtext, sub_color, accent)) in kpi_cards.iter().enumerate() {
        let card_x = PAD_X + (idx as f64) * 176.0;
        let card_w = 164.0;
        let card_y = 74.0;
        let card_h = 62.0;
        let val_font = kpi_value_font_size(value);

        let _ = writeln!(
            svg,
            "    <rect x=\"{card_x:.2}\" y=\"{card_y:.2}\" width=\"{card_w:.2}\" height=\"{card_h:.2}\" rx=\"8\" fill=\"#FFFFFF\" stroke=\"#E2E8F0\" stroke-width=\"1\"/>"
        );
        let _ = writeln!(
            svg,
            "    <rect x=\"{card_x:.2}\" y=\"{card_y:.2}\" width=\"4.00\" height=\"{card_h:.2}\" rx=\"2\" fill=\"{accent}\"/>"
        );
        let text_x = card_x + 12.0;
        let _ = writeln!(
            svg,
            "    <text x=\"{text_x:.2}\" y=\"92.00\" font-size=\"9.5\" font-weight=\"700\" letter-spacing=\"0.4\" fill=\"#64748B\">{}</text>",
            xml_escape(label)
        );
        let _ = writeln!(
            svg,
            "    <text x=\"{text_x:.2}\" y=\"113.00\" font-size=\"{val_font}\" font-weight=\"700\" fill=\"#0F172A\">{}</text>",
            xml_escape(value)
        );
        let _ = writeln!(
            svg,
            "    <text x=\"{text_x:.2}\" y=\"128.00\" font-size=\"10\" fill=\"{sub_color}\">{}</text>",
            xml_escape(subtext)
        );
    }
    svg.push_str("  </g>\n");

    // BAND 3: Top Panel — Daily Budget Spent.
    let max_daily = series
        .days
        .iter()
        .map(|d| d.daily_spend_cents)
        .max()
        .unwrap_or(0)
        .max(summary.max_daily_spend_cents);
    let top_scale = nice_y_scale(max_daily, 5);
    let top_all_whole = top_scale.ticks_cents.iter().all(|&t| t % 100 == 0);

    svg.push_str("  <g class=\"daily-spend-panel\">\n");
    let _ = writeln!(
        svg,
        "    <rect x=\"{PAD_X:.2}\" y=\"148.00\" width=\"{INNER_W:.2}\" height=\"250.00\" rx=\"10\" fill=\"#FFFFFF\" stroke=\"#E2E8F0\" stroke-width=\"1\"/>"
    );
    let _ = writeln!(
        svg,
        "    <text x=\"44.00\" y=\"169.00\" font-size=\"13\" font-weight=\"700\" fill=\"#0F172A\">Daily Budget Spent</text>"
    );
    let _ = writeln!(
        svg,
        "    <text x=\"178.00\" y=\"169.00\" font-size=\"11\" fill=\"#64748B\">USD spend per simulated day</text>"
    );

    // Top panel legend (right-aligned).
    svg.push_str(
        "    <rect x=\"752.00\" y=\"159.00\" width=\"10.00\" height=\"10.00\" rx=\"2\" fill=\"url(#sim-budget-bar-grad)\"/>\n\
         \x20   <text x=\"766.00\" y=\"168.00\" font-size=\"10.5\" fill=\"#475569\">Daily Spend</text>\n\
         \x20   <rect x=\"840.00\" y=\"159.00\" width=\"10.00\" height=\"10.00\" rx=\"2\" fill=\"url(#sim-budget-bar-peak-grad)\"/>\n\
         \x20   <text x=\"854.00\" y=\"168.00\" font-size=\"10.5\" fill=\"#475569\">Peak Day</text>\n\
         \x20   <line x1=\"918.00\" y1=\"164.00\" x2=\"938.00\" y2=\"164.00\" stroke=\"#D97706\" stroke-width=\"1.8\" stroke-dasharray=\"5,3\"/>\n\
         \x20   <text x=\"944.00\" y=\"168.00\" font-size=\"10.5\" fill=\"#475569\">Mean Daily Spend</text>\n",
    );

    // Top panel horizontal gridlines & Y ticks.
    for &tick in &top_scale.ticks_cents {
        let ratio = safe_ratio(tick as f64, top_scale.max_cents as f64);
        let y = safe_coord((TOP_Y_BOTTOM - ratio * TOP_PLOT_H).clamp(TOP_Y_TOP, TOP_Y_BOTTOM));
        let stroke = if tick == 0 { "#94A3B8" } else { "#E2E8F0" };
        let _ = writeln!(
            svg,
            "    <line x1=\"{PLOT_LEFT:.2}\" y1=\"{y:.2}\" x2=\"{PLOT_RIGHT:.2}\" y2=\"{y:.2}\" stroke=\"{stroke}\" stroke-width=\"1\"/>"
        );
        let tick_label = format_y_tick(tick, top_all_whole);
        let _ = writeln!(
            svg,
            "    <text x=\"116.00\" y=\"{:.2}\" text-anchor=\"end\" font-size=\"10.5\" fill=\"#64748B\">{}</text>",
            safe_coord(y + 3.5),
            xml_escape(&tick_label)
        );
    }

    // Y-axis baseline & rotated title.
    let _ = writeln!(
        svg,
        "    <line x1=\"{PLOT_LEFT:.2}\" y1=\"{TOP_Y_TOP:.2}\" x2=\"{PLOT_LEFT:.2}\" y2=\"{TOP_Y_BOTTOM:.2}\" stroke=\"#94A3B8\" stroke-width=\"1\"/>"
    );
    let _ = writeln!(
        svg,
        "    <text x=\"46.00\" y=\"274.00\" text-anchor=\"middle\" font-size=\"10.5\" font-weight=\"600\" fill=\"#475569\" transform=\"rotate(-90 46.00 274.00)\">Daily Spend ({currency_escaped})</text>"
    );

    // Bars & X-axis labels.
    if n > 0 {
        let slot_w = PLOT_W / (n as f64);
        let bar_w = (slot_w * 0.68).clamp(4.0, 48.0);
        let label_step = if n <= 35 {
            1
        } else {
            ((n as f64) / 30.0).ceil() as usize
        };

        for (i, pt) in series.days.iter().enumerate() {
            let center_x = safe_coord(PLOT_LEFT + (i as f64 + 0.5) * slot_w);
            let bar_x = safe_coord(center_x - bar_w / 2.0);
            let ratio = safe_ratio(pt.daily_spend_cents as f64, top_scale.max_cents as f64);
            let bar_h = safe_coord((ratio * TOP_PLOT_H).clamp(0.0, TOP_PLOT_H));
            let bar_y = safe_coord(TOP_Y_BOTTOM - bar_h);
            let rx = if bar_h >= 4.0 { "2" } else { "0" };
            let fill = if max_daily > 0 && pt.daily_spend_cents == max_daily {
                "url(#sim-budget-bar-peak-grad)"
            } else {
                "url(#sim-budget-bar-grad)"
            };
            let tooltip = format!(
                "Day {}: {} spent ({} clicks / {} impr)",
                pt.day,
                format_usd_cents(pt.daily_spend_cents),
                format_u64_commas(pt.clicks),
                format_u64_commas(pt.impressions),
            );
            let _ = writeln!(
                svg,
                "    <rect class=\"daily-bar\" data-day=\"{}\" data-spend-cents=\"{}\" x=\"{bar_x:.2}\" y=\"{bar_y:.2}\" width=\"{:.2}\" height=\"{bar_h:.2}\" rx=\"{rx}\" fill=\"{fill}\"><title>{}</title></rect>",
                pt.day,
                pt.daily_spend_cents,
                safe_coord(bar_w),
                xml_escape(&tooltip)
            );

            if i % label_step == 0 || i == n - 1 {
                let _ = writeln!(
                    svg,
                    "    <text x=\"{center_x:.2}\" y=\"378.00\" text-anchor=\"middle\" font-size=\"10\" fill=\"#64748B\">{}</text>",
                    pt.day
                );
            }
        }

        // Mean daily spend reference line + pill badge.
        if max_daily > 0 {
            let mean_cents = if summary.mean_daily_spend_cents.is_finite()
                && summary.mean_daily_spend_cents >= 0.0
            {
                summary.mean_daily_spend_cents
            } else {
                0.0
            };
            let mean_y = safe_coord(
                (TOP_Y_BOTTOM - safe_ratio(mean_cents, top_scale.max_cents as f64) * TOP_PLOT_H)
                    .clamp(TOP_Y_TOP, TOP_Y_BOTTOM),
            );
            let _ = writeln!(
                svg,
                "    <line class=\"mean-spend-line\" x1=\"{PLOT_LEFT:.2}\" y1=\"{mean_y:.2}\" x2=\"{PLOT_RIGHT:.2}\" y2=\"{mean_y:.2}\" stroke=\"#D97706\" stroke-width=\"1.5\" stroke-dasharray=\"6,4\"/>"
            );

            let mean_label = format!("Avg {}/day", format_usd_cents_f64(mean_cents));
            let pill_w = ((mean_label.len() as f64) * 6.0 + 16.0).clamp(84.0, 220.0);
            let pill_h = 18.0;
            let pill_x = safe_coord(PLOT_RIGHT - 4.0 - pill_w);
            let pill_y = safe_coord(if mean_y - (pill_h + 4.0) >= TOP_Y_TOP {
                mean_y - (pill_h + 4.0)
            } else {
                (mean_y + 4.0).min(TOP_Y_BOTTOM - pill_h - 2.0)
            });
            let _ = writeln!(
                svg,
                "    <rect x=\"{pill_x:.2}\" y=\"{pill_y:.2}\" width=\"{pill_w:.2}\" height=\"{pill_h:.2}\" rx=\"4\" fill=\"#FFFBEB\" fill-opacity=\"0.94\" stroke=\"#FDE68A\" stroke-width=\"1\"/>"
            );
            let _ = writeln!(
                svg,
                "    <text x=\"{:.2}\" y=\"{:.2}\" text-anchor=\"middle\" font-size=\"10\" font-weight=\"600\" fill=\"#B45309\">{}</text>",
                safe_coord(pill_x + pill_w / 2.0),
                safe_coord(pill_y + 12.5),
                xml_escape(&mean_label)
            );
        } else {
            let banner = zero_spend_banner_text(n);
            render_centered_banner(&mut svg, PLOT_LEFT + PLOT_W / 2.0, 274.0, &banner);
        }
    } else {
        render_centered_banner(
            &mut svg,
            PLOT_LEFT + PLOT_W / 2.0,
            274.0,
            "No daily budget data available",
        );
    }

    let _ = writeln!(
        svg,
        "    <text x=\"{:.2}\" y=\"392.00\" text-anchor=\"middle\" font-size=\"10.5\" font-weight=\"600\" fill=\"#475569\">Simulation Day</text>",
        PLOT_LEFT + PLOT_W / 2.0
    );
    svg.push_str("  </g>\n");

    // BAND 4: Bottom Panel — Cumulative Budget Spent.
    let max_cum = series
        .days
        .iter()
        .map(|d| d.cumulative_spend_cents)
        .max()
        .unwrap_or(0)
        .max(summary.total_spend_cents);

    // Zero-spend guard runs BEFORE cap-line scaling so zero-spend runs always use $0.00..$1.00.
    let (bot_scale, show_cap_line) = if max_cum == 0 {
        (nice_y_scale(0, 5), false)
    } else {
        let high_util = summary.utilization_pct.is_finite() && summary.utilization_pct >= 50.0;
        let show_cap = summary.total_budget_cents > 0 && (high_util || summary.exhausted_campaigns > 0);
        let scale_target = if show_cap {
            max_cum.max(summary.total_budget_cents)
        } else {
            max_cum
        };
        (nice_y_scale(scale_target, 5), show_cap)
    };
    let bot_all_whole = bot_scale.ticks_cents.iter().all(|&t| t % 100 == 0);

    svg.push_str("  <g class=\"cumulative-spend-panel\">\n");
    let _ = writeln!(
        svg,
        "    <rect x=\"{PAD_X:.2}\" y=\"410.00\" width=\"{INNER_W:.2}\" height=\"246.00\" rx=\"10\" fill=\"#FFFFFF\" stroke=\"#E2E8F0\" stroke-width=\"1\"/>"
    );
    let _ = writeln!(
        svg,
        "    <text x=\"44.00\" y=\"431.00\" font-size=\"13\" font-weight=\"700\" fill=\"#0F172A\">Cumulative Budget Spent</text>"
    );
    let _ = writeln!(
        svg,
        "    <text x=\"212.00\" y=\"431.00\" font-size=\"11\" fill=\"#64748B\">Running spend &amp; budget utilization</text>"
    );

    // Right-side header note or exhaustion badge.
    if max_cum == 0 && summary.exhausted_campaigns > 0 {
        let note = format!(
            "{}/{} campaigns exhausted · Cap {}",
            format_u64_commas(summary.exhausted_campaigns as u64),
            format_u64_commas(summary.total_campaigns as u64),
            total_budget_str
        );
        let pill_w = ((note.len() as f64) * 5.9 + 18.0).clamp(160.0, 380.0);
        let pill_x = safe_coord(PLOT_RIGHT - pill_w);
        let _ = writeln!(
            svg,
            "    <rect x=\"{pill_x:.2}\" y=\"417.00\" width=\"{pill_w:.2}\" height=\"20.00\" rx=\"10\" fill=\"#FEF2F2\" stroke=\"#FECACA\" stroke-width=\"1\"/>"
        );
        let _ = writeln!(
            svg,
            "    <text x=\"{:.2}\" y=\"430.50\" text-anchor=\"middle\" font-size=\"10\" font-weight=\"600\" fill=\"#B91C1C\">{}</text>",
            safe_coord(pill_x + pill_w / 2.0),
            xml_escape(&note)
        );
    } else if !show_cap_line {
        let note = format!("Total Budget: {total_budget_str} ({util_str} utilized)");
        let _ = writeln!(
            svg,
            "    <text x=\"{PLOT_RIGHT:.2}\" y=\"431.00\" text-anchor=\"end\" font-size=\"10.5\" fill=\"#64748B\">{}</text>",
            xml_escape(&note)
        );
    } else {
        svg.push_str(
            "    <line x1=\"826.00\" y1=\"427.00\" x2=\"846.00\" y2=\"427.00\" stroke=\"#0D9488\" stroke-width=\"2.5\"/>\n\
             \x20   <text x=\"852.00\" y=\"431.00\" font-size=\"10.5\" fill=\"#475569\">Cumulative</text>\n\
             \x20   <line x1=\"930.00\" y1=\"427.00\" x2=\"950.00\" y2=\"427.00\" stroke=\"#DC2626\" stroke-width=\"1.5\" stroke-dasharray=\"6,4\"/>\n\
             \x20   <text x=\"956.00\" y=\"431.00\" font-size=\"10.5\" fill=\"#475569\">Budget Cap</text>\n",
        );
    }

    // Bottom panel horizontal gridlines & Y ticks.
    for &tick in &bot_scale.ticks_cents {
        let ratio = safe_ratio(tick as f64, bot_scale.max_cents as f64);
        let y = safe_coord((BOT_Y_BOTTOM - ratio * BOT_PLOT_H).clamp(BOT_Y_TOP, BOT_Y_BOTTOM));
        let stroke = if tick == 0 { "#94A3B8" } else { "#E2E8F0" };
        let _ = writeln!(
            svg,
            "    <line x1=\"{PLOT_LEFT:.2}\" y1=\"{y:.2}\" x2=\"{PLOT_RIGHT:.2}\" y2=\"{y:.2}\" stroke=\"{stroke}\" stroke-width=\"1\"/>"
        );
        let tick_label = format_y_tick(tick, bot_all_whole);
        let _ = writeln!(
            svg,
            "    <text x=\"116.00\" y=\"{:.2}\" text-anchor=\"end\" font-size=\"10.5\" fill=\"#64748B\">{}</text>",
            safe_coord(y + 3.5),
            xml_escape(&tick_label)
        );
    }

    // Bottom Y-axis baseline & rotated title.
    let _ = writeln!(
        svg,
        "    <line x1=\"{PLOT_LEFT:.2}\" y1=\"{BOT_Y_TOP:.2}\" x2=\"{PLOT_LEFT:.2}\" y2=\"{BOT_Y_BOTTOM:.2}\" stroke=\"#94A3B8\" stroke-width=\"1\"/>"
    );
    let _ = writeln!(
        svg,
        "    <text x=\"46.00\" y=\"536.00\" text-anchor=\"middle\" font-size=\"10.5\" font-weight=\"600\" fill=\"#475569\" transform=\"rotate(-90 46.00 536.00)\">Cumulative ({currency_escaped})</text>"
    );

    // Optional budget cap reference line & left-aligned badge.
    let mut placed_boxes: Vec<(f64, f64, f64, f64)> = Vec::new();
    if show_cap_line {
        let cap_y = safe_coord(
            (BOT_Y_BOTTOM
                - safe_ratio(
                    summary.total_budget_cents as f64,
                    bot_scale.max_cents as f64,
                ) * BOT_PLOT_H)
                .clamp(BOT_Y_TOP, BOT_Y_BOTTOM),
        );
        let _ = writeln!(
            svg,
            "    <line class=\"budget-cap-line\" data-role=\"budget-cap\" x1=\"{PLOT_LEFT:.2}\" y1=\"{cap_y:.2}\" x2=\"{PLOT_RIGHT:.2}\" y2=\"{cap_y:.2}\" stroke=\"#DC2626\" stroke-width=\"1.5\" stroke-dasharray=\"6,4\"/>"
        );
        let cap_label = if summary.exhausted_campaigns > 0 {
            format!(
                "Budget Cap: {} · {}/{} exhausted",
                total_budget_str,
                format_u64_commas(summary.exhausted_campaigns as u64),
                format_u64_commas(summary.total_campaigns as u64),
            )
        } else {
            format!("Budget Cap: {total_budget_str}")
        };
        let box_w = ((cap_label.len() as f64) * 5.9 + 16.0).clamp(112.0, 340.0);
        let box_h = 18.0;
        let box_x = 132.0;
        let box_y = safe_coord(if cap_y - (box_h + 4.0) >= BOT_Y_TOP {
            cap_y - (box_h + 4.0)
        } else {
            (cap_y + 4.0).min(BOT_Y_BOTTOM - box_h - 2.0)
        });
        placed_boxes.push((box_x, box_y, box_x + box_w, box_y + box_h));
        let _ = writeln!(
            svg,
            "    <rect x=\"{box_x:.2}\" y=\"{box_y:.2}\" width=\"{box_w:.2}\" height=\"{box_h:.2}\" rx=\"4\" fill=\"#FEF2F2\" fill-opacity=\"0.95\" stroke=\"#FECACA\" stroke-width=\"1\"/>"
        );
        let _ = writeln!(
            svg,
            "    <text x=\"{:.2}\" y=\"{:.2}\" font-size=\"10\" font-weight=\"600\" fill=\"#B91C1C\">{}</text>",
            safe_coord(box_x + 8.0),
            safe_coord(box_y + 12.5),
            xml_escape(&cap_label)
        );
    }

    if n > 0 {
        let slot_w = PLOT_W / (n as f64);
        let label_step = if n <= 35 {
            1
        } else {
            ((n as f64) / 30.0).ceil() as usize
        };

        let points: Vec<(f64, f64)> = series
            .days
            .iter()
            .enumerate()
            .map(|(i, pt)| {
                let x = safe_coord(PLOT_LEFT + (i as f64 + 0.5) * slot_w);
                let ratio = safe_ratio(
                    pt.cumulative_spend_cents as f64,
                    bot_scale.max_cents as f64,
                );
                let y = safe_coord(
                    (BOT_Y_BOTTOM - ratio * BOT_PLOT_H).clamp(BOT_Y_TOP, BOT_Y_BOTTOM),
                );
                (x, y)
            })
            .collect();

        if n >= 2 {
            let mut poly_points = String::with_capacity(n * 16);
            for (i, &(x, y)) in points.iter().enumerate() {
                if i > 0 {
                    poly_points.push(' ');
                }
                let _ = write!(poly_points, "{x:.2},{y:.2}");
            }
            let first_x = points[0].0;
            let last_x = points[n - 1].0;
            let _ = writeln!(
                svg,
                "    <polygon class=\"cumulative-area\" points=\"{first_x:.2},{BOT_Y_BOTTOM:.2} {poly_points} {last_x:.2},{BOT_Y_BOTTOM:.2}\" fill=\"url(#sim-budget-cum-area-grad)\"/>"
            );
            let _ = writeln!(
                svg,
                "    <polyline class=\"cumulative-line\" fill=\"none\" stroke=\"#0D9488\" stroke-width=\"2.5\" stroke-linejoin=\"round\" stroke-linecap=\"round\" points=\"{poly_points}\"/>"
            );
        }

        let milestones = milestone_indices(n);

        for (i, (pt, &(x, y))) in series.days.iter().zip(points.iter()).enumerate() {
            let is_milestone = milestones.contains(&i);
            let r = if is_milestone { "4.5" } else { "2.8" };
            let fill = if is_milestone { "#0F766E" } else { "#FFFFFF" };
            let stroke = "#0D9488";
            let stroke_w = if is_milestone { "2" } else { "1.6" };
            let tooltip = format!(
                "Day {}: {} cumulative ({} of budget) · {} active campaigns",
                pt.day,
                format_usd_cents(pt.cumulative_spend_cents),
                format_pct(pt.utilization_pct),
                format_u64_commas(pt.active_campaigns_end_of_day as u64),
            );
            let _ = writeln!(
                svg,
                "    <circle class=\"cumulative-point\" data-day=\"{}\" data-cumulative-spend-cents=\"{}\" cx=\"{x:.2}\" cy=\"{y:.2}\" r=\"{r}\" fill=\"{fill}\" stroke=\"{stroke}\" stroke-width=\"{stroke_w}\"><title>{}</title></circle>",
                pt.day,
                pt.cumulative_spend_cents,
                xml_escape(&tooltip)
            );

            if i % label_step == 0 || i == n - 1 {
                let _ = writeln!(
                    svg,
                    "    <text x=\"{x:.2}\" y=\"636.00\" text-anchor=\"middle\" font-size=\"10\" fill=\"#64748B\">{}</text>",
                    pt.day
                );
            }
        }

        if max_cum > 0 {
            for &idx in &milestones {
                let pt = &series.days[idx];
                let (x_i, y_i) = points[idx];
                let label = format!(
                    "D{}: {} ({})",
                    pt.day,
                    format_usd_cents(pt.cumulative_spend_cents),
                    format_pct(pt.utilization_pct)
                );
                let pill_w = ((label.len() as f64) * 5.8 + 14.0).clamp(84.0, 260.0);
                let pill_h = 20.0;
                let max_x = (PLOT_RIGHT - 2.0 - pill_w).max(PLOT_LEFT + 2.0);
                let box_x = if idx == 0 {
                    (x_i + 8.0).clamp(PLOT_LEFT + 2.0, max_x)
                } else if idx == n - 1 {
                    (x_i - pill_w - 8.0).clamp(PLOT_LEFT + 2.0, max_x)
                } else {
                    (x_i - pill_w / 2.0).clamp(PLOT_LEFT + 2.0, max_x)
                };

                let min_y = BOT_Y_TOP + 2.0;
                let max_y = BOT_Y_BOTTOM - pill_h - 2.0;
                let above_y = y_i - pill_h - 8.0;
                let below_y = (y_i + 10.0).clamp(min_y, max_y);

                let candidates = [
                    above_y,
                    below_y,
                    (above_y - pill_h - 4.0).clamp(min_y, max_y),
                    (below_y + pill_h + 4.0).clamp(min_y, max_y),
                ];

                let mut chosen_y = if above_y >= min_y { above_y } else { below_y };
                for &cand_y in &candidates {
                    if cand_y < min_y || cand_y > max_y {
                        continue;
                    }
                    let rect = (box_x, cand_y, box_x + pill_w, cand_y + pill_h);
                    if !placed_boxes.iter().any(|&other| rects_overlap(rect, other)) {
                        chosen_y = cand_y;
                        break;
                    }
                }
                let box_x = safe_coord(box_x);
                let chosen_y = safe_coord(chosen_y.clamp(min_y, max_y));
                placed_boxes.push((box_x, chosen_y, box_x + pill_w, chosen_y + pill_h));

                let _ = writeln!(
                    svg,
                    "    <rect class=\"milestone-callout\" x=\"{box_x:.2}\" y=\"{chosen_y:.2}\" width=\"{pill_w:.2}\" height=\"{pill_h:.2}\" rx=\"4\" fill=\"#FFFFFF\" fill-opacity=\"0.94\" stroke=\"#99F6E4\" stroke-width=\"1\"/>"
                );
                let _ = writeln!(
                    svg,
                    "    <text x=\"{:.2}\" y=\"{:.2}\" text-anchor=\"middle\" font-size=\"10\" font-weight=\"600\" fill=\"#0F766E\">{}</text>",
                    safe_coord(box_x + pill_w / 2.0),
                    safe_coord(chosen_y + 13.5),
                    xml_escape(&label)
                );
            }
        } else {
            let banner = zero_spend_banner_text(n);
            render_centered_banner(&mut svg, PLOT_LEFT + PLOT_W / 2.0, 536.0, &banner);
        }
    } else {
        render_centered_banner(
            &mut svg,
            PLOT_LEFT + PLOT_W / 2.0,
            536.0,
            "No daily budget data available",
        );
    }

    let _ = writeln!(
        svg,
        "    <text x=\"{:.2}\" y=\"650.00\" text-anchor=\"middle\" font-size=\"10.5\" font-weight=\"600\" fill=\"#475569\">Simulation Day</text>",
        PLOT_LEFT + PLOT_W / 2.0
    );
    svg.push_str("  </g>\n");

    // BAND 5: Footer.
    let _ = writeln!(
        svg,
        "  <text x=\"{PAD_X:.2}\" y=\"670.00\" font-size=\"10\" fill=\"#94A3B8\">Generated deterministically by sim-agent (pure Rust SVG)</text>"
    );
    let _ = writeln!(
        svg,
        "  <text x=\"{:.2}\" y=\"670.00\" text-anchor=\"end\" font-size=\"10\" fill=\"#94A3B8\">All monetary figures derived from exact integer {currency_escaped} cents</text>",
        SVG_W - PAD_X
    );

    svg.push_str("</svg>");
    svg
}

fn render_centered_banner(svg: &mut String, cx: f64, cy: f64, message: &str) {
    let pill_w = ((message.len() as f64) * 6.8 + 28.0).clamp(180.0, 420.0);
    let pill_h = 28.0;
    let x = safe_coord(cx - pill_w / 2.0);
    let y = safe_coord(cy - pill_h / 2.0);
    let _ = writeln!(
        svg,
        "    <rect x=\"{x:.2}\" y=\"{y:.2}\" width=\"{pill_w:.2}\" height=\"{pill_h:.2}\" rx=\"6\" fill=\"#F8FAFC\" fill-opacity=\"0.96\" stroke=\"#CBD5E1\" stroke-width=\"1\"/>"
    );
    let _ = writeln!(
        svg,
        "    <text x=\"{:.2}\" y=\"{:.2}\" text-anchor=\"middle\" font-size=\"12\" font-weight=\"600\" fill=\"#475569\">{}</text>",
        safe_coord(cx),
        safe_coord(cy + 4.0),
        xml_escape(message)
    );
}

fn zero_spend_banner_text(n: usize) -> String {
    if n == 30 {
        "$0.00 spent across 30 days".to_string()
    } else if n == 1 {
        "$0.00 spent across 1 day".to_string()
    } else {
        format!("$0.00 spent across {n} days")
    }
}

fn milestone_indices(n: usize) -> Vec<usize> {
    match n {
        0 => Vec::new(),
        1 => vec![0],
        2..=4 => vec![0, n - 1],
        _ => vec![0, (n - 1) / 2, n - 1],
    }
}

fn rects_overlap(a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)) -> bool {
    let pad = 2.0;
    a.0 < b.2 + pad && a.2 + pad > b.0 && a.1 < b.3 + pad && a.3 + pad > b.1
}

fn kpi_value_font_size(val: &str) -> &'static str {
    let len = val.chars().count();
    if len <= 13 {
        "16"
    } else if len <= 17 {
        "13"
    } else {
        "11"
    }
}

fn nice_y_scale(max_cents: u64, target_intervals: usize) -> YScale {
    if max_cents == 0 {
        return YScale {
            max_cents: 100,
            ticks_cents: vec![0, 25, 50, 75, 100],
        };
    }

    let intervals = target_intervals.max(1) as f64;
    let rough_step = (max_cents as f64) / intervals;
    let exp = rough_step.log10().floor();
    let mag = 10f64.powf(exp);
    let residual = rough_step / mag;
    let factor = if residual <= 1.0 {
        1.0
    } else if residual <= 2.0 {
        2.0
    } else if residual <= 2.5 && mag >= 10.0 {
        2.5
    } else if residual <= 5.0 {
        5.0
    } else {
        10.0
    };
    let step = ((factor * mag).round().max(1.0)) as u128;
    let n_steps = u128::from(max_cents).div_ceil(step).max(1);
    let top = step * n_steps;
    let ticks_cents = (0..=n_steps).map(|k| k * step).collect();

    YScale {
        max_cents: top,
        ticks_cents,
    }
}

fn format_y_tick(cents: u128, all_whole_dollars: bool) -> String {
    if cents >= 100_000_000_000_000 {
        let trillions = (cents as f64) / 100_000_000_000_000.0;
        format!("${trillions:.1}T")
    } else if cents >= 100_000_000_000 {
        let billions = (cents as f64) / 100_000_000_000.0;
        format!("${billions:.1}B")
    } else if all_whole_dollars {
        format!("${}", format_u128_commas(cents / 100))
    } else {
        format!("${}.{:02}", format_u128_commas(cents / 100), cents % 100)
    }
}

fn format_usd_compact(cents: u64) -> String {
    if cents >= 100_000_000_000 {
        format!("${:.1}B", (cents as f64) / 100_000_000_000.0)
    } else if cents >= 100_000_000 {
        format!("${:.1}M", (cents as f64) / 100_000_000.0)
    } else if cents >= 1_000_000 {
        format!("${:.1}K", (cents as f64) / 100_000.0)
    } else if cents.is_multiple_of(100) {
        format!("${}", format_u64_commas(cents / 100))
    } else {
        format_usd_cents(cents)
    }
}

fn format_pct(pct: f64) -> String {
    let clean = if !pct.is_finite() || pct.abs() < 0.005 {
        0.0
    } else {
        pct
    };
    format!("{clean:.2}%")
}

fn safe_ratio(num: f64, den: f64) -> f64 {
    if !num.is_finite() || !den.is_finite() || den <= 0.0 {
        0.0
    } else {
        (num / den).clamp(0.0, 1.0)
    }
}

fn safe_coord(val: f64) -> f64 {
    if !val.is_finite() || val.abs() < 1e-9 {
        0.0
    } else {
        val
    }
}

fn xml_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::agent::{
        AgentConfig, BudgetSummary, BudgetTimeSeries, DailyBudgetPoint, TimeSeriesConfig,
        format_u64_commas, format_usd_cents, format_usd_cents_f64, run_simulation,
    };
    use crate::plot::{
        format_pct, format_y_tick, nice_y_scale, render_budget_svg, xml_escape,
    };

    fn assert_no_bad_floats(svg: &str) {
        assert!(!svg.contains("NaN"), "SVG contained NaN");
        assert!(!svg.contains("Infinity"), "SVG contained Infinity");
        assert!(!svg.contains("-0.00"), "SVG contained -0.00");
    }

    #[test]
    fn render_svg_golden_baseline_geometry_kpis_and_determinism() {
        let ts = run_simulation(&AgentConfig::default()).unwrap().timeseries;
        let svg = render_budget_svg(&ts);
        let svg_again = render_budget_svg(&ts);

        assert_eq!(svg, svg_again);
        assert!(svg.starts_with("<svg "));
        assert!(svg.ends_with("</svg>"));
        assert!(svg.contains("xmlns=\"http://www.w3.org/2000/svg\""));
        assert!(svg.contains("width=\"1100\" height=\"680\" viewBox=\"0 0 1100 680\""));
        assert!(svg.contains("30-Day Ads Simulation — Budget Spent"));
        assert!(svg.contains("Mkt Seed 42"));
        assert!(svg.contains("Retrieve-V0"));
        assert!(svg.contains("Ranker R1 / Scoring S0 / Pacing P0 / bidding-basebid (Slate 20)"));
        assert!(svg.contains("$1,000.00–$10,000.00/Campaign"));

        // Verify KPI values match the computed timeseries.
        assert!(svg.contains(&format_usd_cents(ts.summary.total_spend_cents)));
        assert!(svg.contains(&format_usd_cents(ts.summary.total_budget_cents)));
        assert!(svg.contains(&format_pct(ts.summary.utilization_pct)));
        assert!(svg.contains(&format_usd_cents_f64(ts.summary.mean_daily_spend_cents)));
        assert!(svg.contains(&format!(">{:<}<", format_u64_commas(ts.summary.total_clicks))));
        assert!(svg.contains("500 / 0"));

        // 30 daily bars & 30 cumulative points + tooltips + mean line.
        assert_eq!(svg.matches("class=\"daily-bar\"").count(), 30);
        assert_eq!(svg.matches("class=\"cumulative-point\"").count(), 30);
        assert!(svg.contains(&format!(
            "Day 1: {} spent",
            format_usd_cents(ts.days[0].daily_spend_cents)
        )));
        assert!(svg.contains("class=\"mean-spend-line\""));
        assert!(svg.contains(&format!(
            "D1: {}",
            format_usd_cents(ts.days[0].cumulative_spend_cents)
        )));
        assert!(svg.contains(&format!(
            "D15: {}",
            format_usd_cents(ts.days[14].cumulative_spend_cents)
        )));
        assert!(svg.contains(&format!(
            "D30: {}",
            format_usd_cents(ts.days[29].cumulative_spend_cents)
        )));

        assert_no_bad_floats(&svg);
    }

    #[test]
    fn render_svg_zero_spend_uses_fallback_scale_and_banner() {
        let ts = run_simulation(&AgentConfig {
            campaign_budget_cents: Some(50),
            cpc_cents: 100,
            impressions: None,
            ..AgentConfig::default()
        })
        .unwrap()
        .timeseries;

        let svg = render_budget_svg(&ts);
        assert!(svg.starts_with("<svg "));
        assert!(svg.ends_with("</svg>"));
        assert!(svg.contains("$0.00 spent across 30 days"));
        assert!(svg.contains("$1.00"));
        assert_eq!(svg.matches("class=\"daily-bar\"").count(), 30);
        assert_eq!(svg.matches("class=\"cumulative-point\"").count(), 30);
        assert!(!svg.contains("class=\"budget-cap-line\""));
        assert_no_bad_floats(&svg);
    }

    #[test]
    fn render_svg_exhaustion_renders_budget_cap_and_exhaustion_label() {
        let ts = run_simulation(&AgentConfig {
            campaign_budget_cents: Some(100),
            cpc_cents: 100,
            impressions: None,
            ..AgentConfig::default()
        })
        .unwrap()
        .timeseries;

        let svg = render_budget_svg(&ts);
        assert!(svg.contains("class=\"budget-cap-line\""));
        assert!(svg.contains("Budget Cap: $500.00"));
        assert!(svg.contains("500/500 exhausted"));
        assert_no_bad_floats(&svg);
    }

    #[test]
    fn render_svg_large_spend_thousands_separators_and_no_overflow() {
        let days: Vec<DailyBudgetPoint> = (1..=30)
            .map(|day| {
                let daily = if day == 30 { 4_166_680 } else { 4_166_666 };
                let cum = (day as u64) * 4_166_666 + if day == 30 { 14 } else { 0 };
                DailyBudgetPoint {
                    day,
                    impressions: 500_000,
                    clicks: 10_000,
                    daily_spend_cents: daily,
                    cumulative_spend_cents: cum,
                    remaining_budget_cents: 200_000_000 - cum,
                    utilization_pct: (cum as f64) * 100.0 / 200_000_000.0,
                    active_campaigns_end_of_day: 500,
                    insufficient_budget: 0,
                }
            })
            .collect();

        let series = BudgetTimeSeries {
            schema_version: 1,
            currency: "USD".to_string(),
            config: TimeSeriesConfig {
                marketplace_seed: None,
                sim_seed: 99,
                campaign_budget_cents: Some(400_000),
                campaign_budget_min_cents: 400_000,
                campaign_budget_max_cents: 400_000,
                cpc_cents: 416,
                impressions: Some(15_000_000),
                retrieval: crate::DEFAULT_RETRIEVAL,
                ranker: crate::DEFAULT_RANKER,
                scoring: crate::DEFAULT_SCORING,
                pacing: crate::DEFAULT_PACING,
                bidding: crate::DEFAULT_BIDDING,
                slate_size: crate::DEFAULT_SLATE_SIZE,
            },
            strategy: crate::StrategyProfile::default(),
            summary: BudgetSummary {
                total_budget_cents: 200_000_000,
                total_spend_cents: 125_000_000,
                remaining_budget_cents: 75_000_000,
                utilization_pct: 62.5,
                total_impressions: 15_000_000,
                total_clicks: 300_000,
                ctr_pct: 2.0,
                exhausted_campaigns: 0,
                total_campaigns: 500,
                insufficient_budget_opportunities: 0,
                min_daily_spend_cents: 4_166_666,
                mean_daily_spend_cents: 125_000_000.0 / 30.0,
                max_daily_spend_cents: 4_166_680,
            },
            days,
        };

        let svg = render_budget_svg(&series);
        assert!(svg.contains("Loaded Report"));
        assert!(svg.contains("$1,250,000.00"));
        assert!(svg.contains("$2,000,000.00"));
        assert!(svg.contains("class=\"budget-cap-line\""));
        assert_no_bad_floats(&svg);
    }

    #[test]
    fn render_svg_defensive_empty_single_short_and_long_series() {
        for count in [0usize, 1, 7, 60] {
            let days: Vec<DailyBudgetPoint> = (1..=count)
                .map(|day| {
                    let daily = (day as u64) * 500;
                    let cum = (1..=day).map(|d| (d as u64) * 500).sum::<u64>();
                    DailyBudgetPoint {
                        day,
                        impressions: 1_000,
                        clicks: day as u64 * 5,
                        daily_spend_cents: daily,
                        cumulative_spend_cents: cum,
                        remaining_budget_cents: 10_000_000u64.saturating_sub(cum),
                        utilization_pct: (cum as f64) * 100.0 / 10_000_000.0,
                        active_campaigns_end_of_day: 500,
                        insufficient_budget: 0,
                    }
                })
                .collect();
            let total_spend = days.last().map(|d| d.cumulative_spend_cents).unwrap_or(0);
            let max_daily = days.iter().map(|d| d.daily_spend_cents).max().unwrap_or(0);
            let min_daily = days.iter().map(|d| d.daily_spend_cents).min().unwrap_or(0);

            let series = BudgetTimeSeries {
                schema_version: 1,
                currency: "USD".to_string(),
                config: TimeSeriesConfig {
                    marketplace_seed: Some(42),
                    sim_seed: 42,
                    campaign_budget_cents: Some(20_000),
                    campaign_budget_min_cents: 20_000,
                    campaign_budget_max_cents: 20_000,
                    cpc_cents: 100,
                    impressions: None,
                    retrieval: crate::DEFAULT_RETRIEVAL,
                    ranker: crate::DEFAULT_RANKER,
                    scoring: crate::DEFAULT_SCORING,
                    pacing: crate::DEFAULT_PACING,
                    bidding: crate::DEFAULT_BIDDING,
                    slate_size: crate::DEFAULT_SLATE_SIZE,
                },
                strategy: crate::StrategyProfile::default(),
                summary: BudgetSummary {
                    total_budget_cents: 10_000_000,
                    total_spend_cents: total_spend,
                    remaining_budget_cents: 10_000_000u64.saturating_sub(total_spend),
                    utilization_pct: (total_spend as f64) * 100.0 / 10_000_000.0,
                    total_impressions: (count as u64) * 1_000,
                    total_clicks: total_spend / 100,
                    ctr_pct: 1.0,
                    exhausted_campaigns: 0,
                    total_campaigns: 500,
                    insufficient_budget_opportunities: 0,
                    min_daily_spend_cents: min_daily,
                    mean_daily_spend_cents: if count == 0 {
                        0.0
                    } else {
                        (total_spend as f64) / (count as f64)
                    },
                    max_daily_spend_cents: max_daily,
                },
                days,
            };

            let svg = render_budget_svg(&series);
            assert!(svg.starts_with("<svg "));
            assert!(svg.ends_with("</svg>"));
            assert_eq!(svg.matches("class=\"daily-bar\"").count(), count);
            assert_eq!(svg.matches("class=\"cumulative-point\"").count(), count);
            if count == 0 {
                assert!(svg.contains("No daily budget data available"));
            }
            assert_no_bad_floats(&svg);
        }
    }

    #[test]
    fn render_svg_xml_escapes_text_and_sanitizes_non_finite_floats() {
        let mut ts = run_simulation(&AgentConfig {
            impressions: None,
            ..AgentConfig::default()
        })
        .unwrap()
        .timeseries;
        ts.currency = "U&S<D>\"'".to_string();
        ts.summary.utilization_pct = f64::NAN;
        ts.summary.ctr_pct = f64::INFINITY;
        ts.summary.mean_daily_spend_cents = -0.0;
        ts.days[0].utilization_pct = f64::NEG_INFINITY;

        let svg = render_budget_svg(&ts);
        assert!(svg.contains("U&amp;S&lt;D&gt;&quot;&apos;"));
        assert!(!svg.contains("<D>"));
        assert_no_bad_floats(&svg);
    }

    #[test]
    fn scale_and_format_helpers_cover_boundaries() {
        assert_eq!(format_u64_commas(0), "0");
        assert_eq!(format_u64_commas(999), "999");
        assert_eq!(format_u64_commas(1_000), "1,000");
        assert_eq!(format_u64_commas(1_234_567_890), "1,234,567,890");

        assert_eq!(format_usd_cents(0), "$0.00");
        assert_eq!(format_usd_cents(5), "$0.05");
        assert_eq!(format_usd_cents(202_000), "$2,020.00");
        assert_eq!(format_usd_cents_f64(-0.0), "$0.00");
        assert_eq!(format_usd_cents_f64(f64::NAN), "$0.00");
        assert_eq!(format_usd_cents_f64(f64::INFINITY), "$0.00");
        assert_eq!(format_usd_cents_f64(202_000.0 / 30.0), "$67.33");

        assert_eq!(format_pct(f64::NAN), "0.00%");
        assert_eq!(format_pct(-0.001), "0.00%");
        assert_eq!(format_pct(4.04), "4.04%");

        assert_eq!(xml_escape("a&b<c>d\"e'f"), "a&amp;b&lt;c&gt;d&quot;e&apos;f");

        let zero_scale = nice_y_scale(0, 5);
        assert_eq!(zero_scale.max_cents, 100);
        assert_eq!(zero_scale.ticks_cents, vec![0, 25, 50, 75, 100]);

        let golden_daily = nice_y_scale(8_800, 5);
        assert_eq!(golden_daily.max_cents, 10_000);
        assert_eq!(
            golden_daily.ticks_cents,
            vec![0, 2_000, 4_000, 6_000, 8_000, 10_000]
        );

        let golden_cum = nice_y_scale(202_000, 5);
        assert_eq!(golden_cum.max_cents, 250_000);
        assert_eq!(
            golden_cum.ticks_cents,
            vec![0, 50_000, 100_000, 150_000, 200_000, 250_000]
        );

        let huge = nice_y_scale(u64::MAX / 4, 5);
        assert!(huge.max_cents >= u128::from(u64::MAX / 4));
        assert!(format_y_tick(huge.max_cents, true).ends_with('T'));
    }
}
