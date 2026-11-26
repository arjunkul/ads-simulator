//! Pure-Rust HTTP/1.1 server for the ads-simulator feature, flowchart, ad-server & analysis dashboard.

use std::error::Error;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

use ad_server::{
    AdResponse, AdServer, AdServerConfig, DEFAULT_PACING, PacingStrategy, R1_SLATE_SIZE,
    RankerStrategy, ScoringStrategy,
};
use ads_simulator::{DEFAULT_SEED as DEFAULT_MARKETPLACE_SEED, generate_marketplace};
use serde::{Deserialize, Deserializer, Serialize};
use sim_agent::{
    ANALYSIS_JSON_FILENAME, AgentConfig, BudgetTimeSeries, DEFAULT_CPC_CENTS, DEFAULT_IMPRESSIONS,
    DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS, DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS, DEFAULT_RANKER,
    DEFAULT_SEED as DEFAULT_SIM_SEED, DEFAULT_SLATE_SIZE, SIMULATION_REPORT_FILENAME,
    load_timeseries_from_path, render_budget_svg, run_simulation,
};
use simulation_analyzer::{SimulationAnalysis, load_and_analyze};

use crate::catalog::{MarketplaceInspection, SimulatorCatalog, build_catalog, inspect_marketplace};
use crate::export::render_dashboard_html;

const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(30);

/// Shared mutable state served by the HTTP dashboard server.
pub struct DashboardState {
    pub catalog: SimulatorCatalog,
    pub default_marketplace: MarketplaceInspection,
    pub latest_timeseries: BudgetTimeSeries,
    pub latest_analysis: Option<SimulationAnalysis>,
    pub latest_svg: String,
    pub default_ad_server_sample: AdResponse,
    index_html: String,
}

fn load_analysis_for_sim_report(sim_report: &Path) -> Option<SimulationAnalysis> {
    if let Ok(analysis) = load_and_analyze(sim_report) {
        return Some(analysis);
    }
    let parent = sim_report.parent()?;
    let sibling_analysis = parent.join(ANALYSIS_JSON_FILENAME);
    if let Ok(bytes) = fs::read(&sibling_analysis) {
        if let Ok(analysis) = serde_json::from_slice::<SimulationAnalysis>(&bytes) {
            return Some(analysis);
        }
    }
    let sibling_sim = parent.join(SIMULATION_REPORT_FILENAME);
    if let Ok(analysis) = load_and_analyze(&sibling_sim) {
        return Some(analysis);
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn sample_ad_server(
    user_id: &str,
    marketplace_seed: i64,
    seed: u64,
    campaign_budget_cents: Option<u64>,
    cpc_cents: u64,
    ranker: RankerStrategy,
    scoring: ScoringStrategy,
    pacing: PacingStrategy,
    slate_size: usize,
) -> Result<AdResponse, String> {
    if user_id.trim().is_empty() {
        return Err("user_id must not be empty".to_owned());
    }
    let mp = generate_marketplace(marketplace_seed);
    let mut server = AdServer::from_marketplace(
        &mp,
        AdServerConfig {
            seed,
            campaign_budget_cents,
            campaign_budget_min_cents: DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS,
            campaign_budget_max_cents: DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS,
            cpc_cents,
            ranker,
            scoring,
            pacing,
            slate_size,
        },
    )
    .map_err(|e| e.to_string())?;
    Ok(server.serve_ads(user_id))
}

impl DashboardState {
    /// Initialize dashboard state from optional precomputed `--sim-report` and `--plot-file`.
    pub fn load(
        sim_report: Option<&Path>,
        plot_file: Option<&Path>,
    ) -> Result<Self, Box<dyn Error>> {
        let catalog = build_catalog();
        let default_marketplace = inspect_marketplace(DEFAULT_MARKETPLACE_SEED);

        let (mut latest_timeseries, latest_analysis) = match sim_report {
            Some(path) => {
                let ts = load_timeseries_from_path(path)?;
                let analysis = load_analysis_for_sim_report(path);
                (ts, analysis)
            }
            None => {
                let run = run_simulation(&AgentConfig::default())?;
                (run.timeseries, Some(run.analysis))
            }
        };
        if latest_timeseries.config.marketplace_seed.is_none() {
            latest_timeseries.config.marketplace_seed =
                Some(latest_timeseries.config.sim_seed.cast_signed());
        }

        let latest_svg = match plot_file {
            Some(path) => {
                let text = fs::read_to_string(path)?;
                if !text.contains("<svg") {
                    return Err(format!(
                        "plot file '{}' does not contain an <svg> element",
                        path.display()
                    )
                    .into());
                }
                text
            }
            None => render_budget_svg(&latest_timeseries),
        };

        let default_ad_server_sample = sample_ad_server(
            "user-0001",
            DEFAULT_MARKETPLACE_SEED,
            DEFAULT_SIM_SEED,
            None,
            DEFAULT_CPC_CENTS,
            RankerStrategy::R1,
            RankerStrategy::R1.default_scoring(),
            DEFAULT_PACING,
            R1_SLATE_SIZE,
        )
        .map_err(io::Error::other)?;

        let index_html = render_dashboard_html(None);

        Ok(Self {
            catalog,
            default_marketplace,
            latest_timeseries,
            latest_analysis,
            latest_svg,
            default_ad_server_sample,
            index_html,
        })
    }
}

/// Bind a `TcpListener` on `host:port` and return the listener plus its actual bound address.
pub fn bind_listener(host: &str, port: u16) -> io::Result<(TcpListener, SocketAddr)> {
    let listener = TcpListener::bind((host, port))?;
    let addr = listener.local_addr()?;
    Ok((listener, addr))
}

/// Run the multithreaded HTTP/1.1 dashboard server loop on a bound listener.
pub fn serve(listener: TcpListener, state: Arc<RwLock<DashboardState>>) -> io::Result<()> {
    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                let state = Arc::clone(&state);
                thread::spawn(move || {
                    let _ = handle_connection(stream, &state);
                });
            }
            Err(err) => {
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
        }
    }
    Ok(())
}

struct HttpRequest {
    method: String,
    target: String,
    body: Vec<u8>,
}

struct HttpResponse {
    status_code: u16,
    reason: &'static str,
    content_type: &'static str,
    extra_headers: Vec<(&'static str, &'static str)>,
    body: Vec<u8>,
}

impl HttpResponse {
    fn json(status_code: u16, reason: &'static str, body: Vec<u8>) -> Self {
        Self {
            status_code,
            reason,
            content_type: "application/json; charset=utf-8",
            extra_headers: Vec::new(),
            body,
        }
    }

    fn json_value(status_code: u16, reason: &'static str, value: &impl Serialize) -> Self {
        match serde_json::to_vec(value) {
            Ok(bytes) => Self::json(status_code, reason, bytes),
            Err(err) => Self::error(500, "Internal Server Error", &err.to_string()),
        }
    }

    fn error(status_code: u16, reason: &'static str, message: &str) -> Self {
        #[derive(Serialize)]
        struct ErrBody<'a> {
            error: &'a str,
        }
        let body = serde_json::to_vec(&ErrBody { error: message })
            .unwrap_or_else(|_| b"{\"error\":\"internal error\"}".to_vec());
        Self::json(status_code, reason, body)
    }

    fn method_not_allowed(allow: &'static str) -> Self {
        let mut resp = Self::error(405, "Method Not Allowed", "method not allowed");
        resp.extra_headers.push(("Allow", allow));
        resp
    }

    fn write_to(&self, stream: &mut TcpStream) -> io::Result<()> {
        let mut header = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n",
            self.status_code,
            self.reason,
            self.content_type,
            self.body.len(),
        );
        for (k, v) in &self.extra_headers {
            header.push_str(k);
            header.push_str(": ");
            header.push_str(v);
            header.push_str("\r\n");
        }
        header.push_str("\r\n");
        stream.write_all(header.as_bytes())?;
        stream.write_all(&self.body)?;
        stream.flush()
    }
}

fn read_http_request(stream: &TcpStream) -> Result<Option<HttpRequest>, HttpResponse> {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    let n = reader
        .read_line(&mut request_line)
        .map_err(|_| HttpResponse::error(400, "Bad Request", "failed to read request line"))?;
    if n == 0 {
        return Ok(None);
    }
    if request_line.len() > MAX_HEADER_BYTES {
        return Err(HttpResponse::error(
            400,
            "Bad Request",
            "request line too large",
        ));
    }

    let trimmed = request_line.trim_end_matches(&['\r', '\n'][..]);
    let mut parts = trimmed.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| HttpResponse::error(400, "Bad Request", "missing HTTP method"))?
        .to_owned();
    let target = parts
        .next()
        .ok_or_else(|| HttpResponse::error(400, "Bad Request", "missing request target"))?
        .to_owned();
    let version = parts
        .next()
        .ok_or_else(|| HttpResponse::error(400, "Bad Request", "missing HTTP version"))?;
    if !version.starts_with("HTTP/1.") {
        return Err(HttpResponse::error(
            400,
            "Bad Request",
            "unsupported HTTP version",
        ));
    }

    let mut total_header_bytes = request_line.len();
    let mut content_length: usize = 0;

    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .map_err(|_| HttpResponse::error(400, "Bad Request", "failed to read header"))?;
        if read == 0 {
            break;
        }
        total_header_bytes += read;
        if total_header_bytes > MAX_HEADER_BYTES {
            return Err(HttpResponse::error(
                400,
                "Bad Request",
                "request headers too large",
            ));
        }
        let header_trimmed = line.trim_end_matches(&['\r', '\n'][..]);
        if header_trimmed.is_empty() {
            break;
        }
        if let Some((name, val)) = header_trimmed.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = val.trim().parse::<usize>().map_err(|_| {
                    HttpResponse::error(400, "Bad Request", "invalid Content-Length header")
                })?;
                if content_length > MAX_BODY_BYTES {
                    return Err(HttpResponse::error(
                        413,
                        "Payload Too Large",
                        "request body exceeds maximum allowed size",
                    ));
                }
            }
        }
    }

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader
            .read_exact(&mut body)
            .map_err(|_| HttpResponse::error(400, "Bad Request", "incomplete request body"))?;
    }

    Ok(Some(HttpRequest {
        method,
        target,
        body,
    }))
}

fn handle_connection(mut stream: TcpStream, state: &Arc<RwLock<DashboardState>>) -> io::Result<()> {
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));

    let response = match read_http_request(&stream) {
        Ok(Some(req)) => dispatch(&req, state),
        Ok(None) => return Ok(()),
        Err(resp) => resp,
    };

    response.write_to(&mut stream)
}

fn has_path_traversal(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains("..") || lower.contains("%2e") || lower.contains('\\') || lower.contains('\0')
}

fn parse_query_pairs(query: Option<&str>) -> Result<Vec<(String, String)>, String> {
    let Some(q) = query else {
        return Ok(Vec::new());
    };
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let mut pairs = Vec::new();
    for pair in q.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, val) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        pairs.push((percent_decode_ascii(key)?, percent_decode_ascii(val)?));
    }
    Ok(pairs)
}

fn parse_seed_query(query: Option<&str>) -> Result<Option<i64>, String> {
    let mut seed_val: Option<i64> = None;
    for (key, val) in parse_query_pairs(query)? {
        if key == "seed" {
            if val.is_empty() {
                return Err("seed query parameter must not be empty".to_owned());
            }
            let parsed = val.parse::<i64>().map_err(|_| {
                format!("invalid seed '{val}': expected a signed 64-bit integer")
            })?;
            seed_val = Some(parsed);
        }
    }
    Ok(seed_val)
}

fn percent_decode_ascii(input: &str) -> Result<String, String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err("invalid percent encoding in query parameter".to_owned());
            }
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                .map_err(|_| "invalid percent encoding".to_owned())?;
            let byte = u8::from_str_radix(hex, 16)
                .map_err(|_| "invalid hex escape in query parameter".to_owned())?;
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| "non-UTF-8 query parameter".to_owned())
}

fn deserialize_optional_nullable_u64<'de, D>(
    deserializer: D,
) -> Result<Option<Option<u64>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<u64>::deserialize(deserializer).map(Some)
}

#[derive(Debug, Deserialize)]
struct SimRunRequest {
    #[serde(default)]
    marketplace_seed: Option<i64>,
    #[serde(default)]
    seed: Option<u64>,
    #[serde(default)]
    campaign_budget_cents: Option<u64>,
    #[serde(default)]
    campaign_budget_min_cents: Option<u64>,
    #[serde(default)]
    campaign_budget_max_cents: Option<u64>,
    #[serde(default)]
    cpc_cents: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_nullable_u64")]
    impressions: Option<Option<u64>>,
    #[serde(default)]
    ranker: Option<RankerStrategy>,
    #[serde(default)]
    scoring: Option<ScoringStrategy>,
    #[serde(default)]
    pacing: Option<PacingStrategy>,
    #[serde(default)]
    slate_size: Option<usize>,
}

#[derive(Serialize)]
struct SimRunResponse<'a> {
    timeseries: &'a BudgetTimeSeries,
    analysis: &'a SimulationAnalysis,
    svg: &'a str,
}

fn dispatch(req: &HttpRequest, state: &Arc<RwLock<DashboardState>>) -> HttpResponse {
    let (path, query) = match req.target.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (req.target.as_str(), None),
    };

    if has_path_traversal(path) {
        return HttpResponse::error(400, "Bad Request", "invalid request path");
    }

    match path {
        "/" | "/index.html" => {
            if req.method != "GET" {
                return HttpResponse::method_not_allowed("GET");
            }
            let guard = state.read().expect("dashboard state lock");
            HttpResponse {
                status_code: 200,
                reason: "OK",
                content_type: "text/html; charset=utf-8",
                extra_headers: Vec::new(),
                body: guard.index_html.as_bytes().to_vec(),
            }
        }
        "/health" => {
            if req.method != "GET" {
                return HttpResponse::method_not_allowed("GET");
            }
            HttpResponse::json(200, "OK", br#"{"status":"ok"}"#.to_vec())
        }
        "/api/catalog" => {
            if req.method != "GET" {
                return HttpResponse::method_not_allowed("GET");
            }
            let guard = state.read().expect("dashboard state lock");
            HttpResponse::json_value(200, "OK", &guard.catalog)
        }
        "/api/marketplace" => {
            if req.method != "GET" {
                return HttpResponse::method_not_allowed("GET");
            }
            match parse_seed_query(query) {
                Ok(None) => {
                    let guard = state.read().expect("dashboard state lock");
                    HttpResponse::json_value(200, "OK", &guard.default_marketplace)
                }
                Ok(Some(seed)) if seed == DEFAULT_MARKETPLACE_SEED => {
                    let guard = state.read().expect("dashboard state lock");
                    HttpResponse::json_value(200, "OK", &guard.default_marketplace)
                }
                Ok(Some(seed)) => {
                    let inspection = inspect_marketplace(seed);
                    HttpResponse::json_value(200, "OK", &inspection)
                }
                Err(msg) => HttpResponse::error(400, "Bad Request", &msg),
            }
        }
        "/api/ad-server/sample" => {
            if req.method != "GET" {
                return HttpResponse::method_not_allowed("GET");
            }
            handle_ad_server_sample(query, state)
        }
        "/api/sim-agent/latest" => {
            if req.method != "GET" {
                return HttpResponse::method_not_allowed("GET");
            }
            let guard = state.read().expect("dashboard state lock");
            HttpResponse::json_value(200, "OK", &guard.latest_timeseries)
        }
        "/api/sim-agent/analysis" => {
            if req.method != "GET" {
                return HttpResponse::method_not_allowed("GET");
            }
            let guard = state.read().expect("dashboard state lock");
            match &guard.latest_analysis {
                Some(analysis) => HttpResponse::json_value(200, "OK", analysis),
                None => HttpResponse::error(
                    404,
                    "Not Found",
                    "no simulation analysis available for loaded timeseries-only report",
                ),
            }
        }
        "/api/sim-agent/run" => {
            if req.method != "POST" {
                return HttpResponse::method_not_allowed("POST");
            }
            handle_sim_run(&req.body, state)
        }
        "/api/artifacts/budget-plot" => {
            if req.method != "GET" {
                return HttpResponse::method_not_allowed("GET");
            }
            let guard = state.read().expect("dashboard state lock");
            HttpResponse {
                status_code: 200,
                reason: "OK",
                content_type: "image/svg+xml; charset=utf-8",
                extra_headers: Vec::new(),
                body: guard.latest_svg.as_bytes().to_vec(),
            }
        }
        _ => HttpResponse::error(404, "Not Found", "not found"),
    }
}

fn handle_ad_server_sample(
    query: Option<&str>,
    state: &Arc<RwLock<DashboardState>>,
) -> HttpResponse {
    let pairs = match parse_query_pairs(query) {
        Ok(p) => p,
        Err(msg) => return HttpResponse::error(400, "Bad Request", &msg),
    };
    if pairs.is_empty() {
        let guard = state.read().expect("dashboard state lock");
        return HttpResponse::json_value(200, "OK", &guard.default_ad_server_sample);
    }

    let mut user_id = "user-0001".to_owned();
    let mut seed = DEFAULT_SIM_SEED;
    let mut budget: Option<u64> = None;
    let mut cpc = DEFAULT_CPC_CENTS;
    let mut ranker = RankerStrategy::R1;
    let mut scoring: Option<ScoringStrategy> = None;
    let mut pacing = DEFAULT_PACING;
    let mut slate_size = R1_SLATE_SIZE;

    for (k, v) in pairs {
        match k.as_str() {
            "user_id" => user_id = v,
            "seed" => match v.parse::<u64>() {
                Ok(n) => seed = n,
                Err(_) => return HttpResponse::error(400, "Bad Request", "invalid seed parameter"),
            },
            "budget" => match v.parse::<u64>() {
                Ok(n) => budget = Some(n),
                Err(_) => {
                    return HttpResponse::error(400, "Bad Request", "invalid budget parameter");
                }
            },
            "cpc" => match v.parse::<u64>() {
                Ok(n) => cpc = n,
                Err(_) => return HttpResponse::error(400, "Bad Request", "invalid cpc parameter"),
            },
            "ranker" => match v.parse::<RankerStrategy>() {
                Ok(r) => ranker = r,
                Err(msg) => return HttpResponse::error(400, "Bad Request", &msg),
            },
            "scoring" => match v.parse::<ScoringStrategy>() {
                Ok(s) => scoring = Some(s),
                Err(msg) => return HttpResponse::error(400, "Bad Request", &msg),
            },
            "pacing" => match v.parse::<PacingStrategy>() {
                Ok(p) => pacing = p,
                Err(msg) => return HttpResponse::error(400, "Bad Request", &msg),
            },
            "slate_size" => match v.parse::<usize>() {
                Ok(n) => slate_size = n,
                Err(_) => {
                    return HttpResponse::error(400, "Bad Request", "invalid slate_size parameter");
                }
            },
            _ => {}
        }
    }

    let resolved_scoring = scoring.unwrap_or_else(|| ranker.default_scoring());
    match sample_ad_server(
        &user_id,
        DEFAULT_MARKETPLACE_SEED,
        seed,
        budget,
        cpc,
        ranker,
        resolved_scoring,
        pacing,
        slate_size,
    ) {
        Ok(resp) => HttpResponse::json_value(200, "OK", &resp),
        Err(msg) => HttpResponse::error(400, "Bad Request", &msg),
    }
}

fn handle_sim_run(body: &[u8], state: &Arc<RwLock<DashboardState>>) -> HttpResponse {
    if body.is_empty() || body.iter().all(|b| b.is_ascii_whitespace()) {
        return HttpResponse::error(400, "Bad Request", "request body must be a JSON object");
    }

    let parsed: SimRunRequest = match serde_json::from_slice(body) {
        Ok(req) => req,
        Err(err) => {
            return HttpResponse::error(400, "Bad Request", &format!("invalid JSON payload: {err}"));
        }
    };

    let sim_seed = parsed.seed.unwrap_or(DEFAULT_SIM_SEED);
    let marketplace_seed = parsed
        .marketplace_seed
        .unwrap_or_else(|| sim_seed.cast_signed());
    let campaign_budget_cents = parsed.campaign_budget_cents;
    let campaign_budget_min_cents = parsed
        .campaign_budget_min_cents
        .unwrap_or(DEFAULT_MIN_CAMPAIGN_BUDGET_CENTS);
    let campaign_budget_max_cents = parsed
        .campaign_budget_max_cents
        .unwrap_or(DEFAULT_MAX_CAMPAIGN_BUDGET_CENTS);
    let cpc_cents = parsed.cpc_cents.unwrap_or(DEFAULT_CPC_CENTS);
    let impressions = match parsed.impressions {
        None => Some(DEFAULT_IMPRESSIONS),
        Some(explicit) => explicit,
    };
    let ranker = parsed.ranker.unwrap_or(DEFAULT_RANKER);
    let scoring = parsed.scoring.unwrap_or_else(|| ranker.default_scoring());
    let pacing = parsed.pacing.unwrap_or(DEFAULT_PACING);
    let slate_size = parsed.slate_size.unwrap_or(DEFAULT_SLATE_SIZE);

    let config = AgentConfig {
        marketplace_path: None,
        marketplace_seed,
        sim_seed,
        campaign_budget_cents,
        campaign_budget_min_cents,
        campaign_budget_max_cents,
        cpc_cents,
        impressions,
        ranker,
        scoring,
        pacing,
        slate_size,
    };

    let run_result = match run_simulation(&config) {
        Ok(res) => res,
        Err(err) => return HttpResponse::error(400, "Bad Request", &err.to_string()),
    };

    let svg = render_budget_svg(&run_result.timeseries);
    let response = HttpResponse::json_value(
        200,
        "OK",
        &SimRunResponse {
            timeseries: &run_result.timeseries,
            analysis: &run_result.analysis,
            svg: &svg,
        },
    );

    if let Ok(mut guard) = state.write() {
        guard.latest_timeseries = run_result.timeseries;
        guard.latest_analysis = Some(run_result.analysis);
        guard.latest_svg = svg;
    }

    response
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, RwLock};

    use ad_server::{AdResponse, AdServer, AdServerConfig};
    use ads_simulator::generate_marketplace;
    use sim_agent::{AgentConfig, render_budget_svg, run_simulation};
    use simulation_analyzer::SimulationAnalysis;

    use crate::catalog::{build_catalog, inspect_marketplace};
    use crate::export::render_dashboard_html;
    use crate::server::{DashboardState, HttpRequest, dispatch, parse_seed_query};

    fn test_state() -> Arc<RwLock<DashboardState>> {
        let run = run_simulation(&AgentConfig {
            impressions: Some(6_000),
            ..AgentConfig::default()
        })
        .unwrap();
        let svg = render_budget_svg(&run.timeseries);
        let mp = generate_marketplace(42);
        let mut server = AdServer::from_marketplace(&mp, AdServerConfig::default()).unwrap();
        let sample = server.serve_ads("user-0001");
        Arc::new(RwLock::new(DashboardState {
            catalog: build_catalog(),
            default_marketplace: inspect_marketplace(42),
            latest_timeseries: run.timeseries,
            latest_analysis: Some(run.analysis),
            latest_svg: svg,
            default_ad_server_sample: sample,
            index_html: render_dashboard_html(None),
        }))
    }

    #[test]
    fn seed_query_parser_handles_extremes_and_rejects_invalid() {
        assert_eq!(parse_seed_query(None).unwrap(), None);
        assert_eq!(parse_seed_query(Some("seed=42")).unwrap(), Some(42));
        assert_eq!(parse_seed_query(Some("seed=-42")).unwrap(), Some(-42));
        assert_eq!(
            parse_seed_query(Some("seed=9223372036854775807")).unwrap(),
            Some(i64::MAX)
        );
        assert_eq!(
            parse_seed_query(Some("seed=-9223372036854775808")).unwrap(),
            Some(i64::MIN)
        );
        assert!(parse_seed_query(Some("seed=")).is_err());
        assert!(parse_seed_query(Some("seed=abc")).is_err());
        assert!(parse_seed_query(Some("seed=3.14")).is_err());
        assert!(parse_seed_query(Some("seed=99999999999999999999999")).is_err());
    }

    #[test]
    fn router_handles_health_analysis_ad_server_sample_404_405_and_invalid_post() {
        let state = test_state();

        let health = dispatch(
            &HttpRequest {
                method: "GET".into(),
                target: "/health".into(),
                body: Vec::new(),
            },
            &state,
        );
        assert_eq!(health.status_code, 200);
        assert_eq!(health.body, br#"{"status":"ok"}"#);

        let analysis_resp = dispatch(
            &HttpRequest {
                method: "GET".into(),
                target: "/api/sim-agent/analysis".into(),
                body: Vec::new(),
            },
            &state,
        );
        assert_eq!(analysis_resp.status_code, 200);
        let parsed_analysis: SimulationAnalysis =
            serde_json::from_slice(&analysis_resp.body).unwrap();
        assert_eq!(parsed_analysis.ctr_performance.total_impressions, 6_000);

        let ad_sample = dispatch(
            &HttpRequest {
                method: "GET".into(),
                target: "/api/ad-server/sample?user_id=user-0007&seed=9".into(),
                body: Vec::new(),
            },
            &state,
        );
        assert_eq!(ad_sample.status_code, 200);
        let parsed_ads: AdResponse = serde_json::from_slice(&ad_sample.body).unwrap();
        assert_eq!(parsed_ads.user_id, "user-0007");
        assert_eq!(parsed_ads.ads.len(), 20);

        let not_found = dispatch(
            &HttpRequest {
                method: "GET".into(),
                target: "/unknown".into(),
                body: Vec::new(),
            },
            &state,
        );
        assert_eq!(not_found.status_code, 404);

        let wrong_method = dispatch(
            &HttpRequest {
                method: "DELETE".into(),
                target: "/health".into(),
                body: Vec::new(),
            },
            &state,
        );
        assert_eq!(wrong_method.status_code, 405);

        let traversal = dispatch(
            &HttpRequest {
                method: "GET".into(),
                target: "/../../Cargo.toml".into(),
                body: Vec::new(),
            },
            &state,
        );
        assert_eq!(traversal.status_code, 400);

        let bad_cpc = dispatch(
            &HttpRequest {
                method: "POST".into(),
                target: "/api/sim-agent/run".into(),
                body: br#"{"cpc_cents":0}"#.to_vec(),
            },
            &state,
        );
        assert_eq!(bad_cpc.status_code, 400);
        assert_eq!(
            state
                .read()
                .unwrap()
                .latest_timeseries
                .summary
                .total_impressions,
            6_000
        );
    }
}
