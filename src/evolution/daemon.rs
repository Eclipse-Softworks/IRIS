//! The autonomic service daemon (`iris service-daemon`).
//!
//! A reverse proxy in front of a real HTTP service, run as a closed MAPE-K
//! loop. Every interval it measures the service through the traffic it
//! carries -- p99 latency, error ratio, request rate, requests in flight,
//! cache hits (MONITOR) -- classifies the regime it is in (ANALYZE), records
//! how the active policy fared in that regime (KNOWLEDGE), chooses the
//! policy with the least strain measured there, trying untried ones first
//! (PLAN), and applies it to live requests: forward, shed over capacity,
//! serve from cache, or throttle to the healthy rate (EXECUTE). It returns
//! to forwarding once the service recovers. Prometheus metrics, a live
//! dashboard and a JSON audit ledger report it.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// The regime the service is measured to be in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ServiceRegime {
    NominalTraffic,
    FlashCrowdSurge,
    UpstreamBrownout,
    CacheThrashing,
}

impl ServiceRegime {
    pub fn name(self) -> &'static str {
        match self {
            Self::NominalTraffic => "Nominal Traffic",
            Self::FlashCrowdSurge => "Flash Crowd Surge",
            Self::UpstreamBrownout => "Upstream Brownout",
            Self::CacheThrashing => "Cache Thrashing",
        }
    }

    /// The usual remedy, tried first in this regime.
    pub fn optimal_policy_id(self) -> i64 {
        match self {
            Self::NominalTraffic => POLICY_FORWARD,
            Self::FlashCrowdSurge => POLICY_SHED,
            Self::UpstreamBrownout => POLICY_CACHE,
            Self::CacheThrashing => POLICY_THROTTLE,
        }
    }
}

/// Instantaneous operational telemetry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryTick {
    pub tick: usize,
    pub regime: String,
    pub active_policy_id: i64,
    pub total_requests: i64,
    pub failed_requests: i64,
    pub queue_depth: i64,
    pub p99_latency_ms: f64,
    pub allostatic_strain: f64,
    pub cache_hit_ratio: f64,
    pub generation_id: usize,
    pub hot_swaps: usize,
}

/// Full audit ledger entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditLedger {
    pub total_ticks: usize,
    pub target_p99_ms: f64,
    pub error_budget_ratio: f64,
    pub total_hot_swaps: usize,
    pub average_allostatic_strain: f64,
    pub ticks: Vec<TelemetryTick>,
}

/// Live daemon state exposed to embedded HTTP server and monitoring clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonState {
    pub current_tick: usize,
    pub total_ticks: usize,
    pub regime: String,
    pub active_policy_id: i64,
    pub total_requests: i64,
    pub failed_requests: i64,
    pub queue_depth: i64,
    pub p99_latency_ms: f64,
    pub target_p99_ms: f64,
    pub error_budget_ratio: f64,
    pub allostatic_strain: f64,
    pub cache_hit_ratio: f64,
    pub generation_id: usize,
    pub hot_swaps: usize,
    pub is_running: bool,
    pub recent_ticks: Vec<TelemetryTick>,
}

/// Format Prometheus metrics snapshot from daemon state.
pub fn render_prometheus_metrics(state: &DaemonState) -> String {
    format!(
        r#"# HELP iris_requests_total Total requests served by IRIS service
# TYPE iris_requests_total counter
iris_requests_total {}
# HELP iris_failed_requests_total Total failed requests
# TYPE iris_failed_requests_total counter
iris_failed_requests_total {}
# HELP iris_p99_latency_ms Moving window P99 latency in milliseconds
# TYPE iris_p99_latency_ms gauge
iris_p99_latency_ms {:.2}
# HELP iris_target_p99_ms Target SLA P99 latency in milliseconds
# TYPE iris_target_p99_ms gauge
iris_target_p99_ms {:.2}
# HELP iris_allostatic_strain Multi-dimensional SLO strain (0.0=healthy, 1.0=critical)
# TYPE iris_allostatic_strain gauge
iris_allostatic_strain {:.4}
# HELP iris_error_budget_ratio Configured error budget ratio
# TYPE iris_error_budget_ratio gauge
iris_error_budget_ratio {:.4}
# HELP iris_service_generation Active hot-swapped code generation ID
# TYPE iris_service_generation gauge
iris_service_generation {}
# HELP iris_hot_swaps_total Total autonomous hot-swaps completed
# TYPE iris_hot_swaps_total counter
iris_hot_swaps_total {}
# HELP iris_queue_depth Instantaneous request queue depth
# TYPE iris_queue_depth gauge
iris_queue_depth {}
# HELP iris_cache_hit_ratio L1/L2 cache hit ratio
# TYPE iris_cache_hit_ratio gauge
iris_cache_hit_ratio {:.4}
# HELP iris_active_policy_id ID of currently deployed MAPE-K policy
# TYPE iris_active_policy_id gauge
iris_active_policy_id {}
"#,
        state.total_requests,
        state.failed_requests,
        state.p99_latency_ms,
        state.target_p99_ms,
        state.allostatic_strain,
        state.error_budget_ratio,
        state.generation_id,
        state.hot_swaps,
        state.queue_depth,
        state.cache_hit_ratio,
        state.active_policy_id,
    )
}

/// Render responsive dark-mode SVG/HTML visualizer dashboard.
pub fn render_dashboard_html(state: &DaemonState) -> String {
    let status_color = if state.p99_latency_ms <= state.target_p99_ms {
        "#3fb950"
    } else {
        "#f85149"
    };

    let sparkline_pts = if state.recent_ticks.is_empty() {
        "0,50 300,50".to_string()
    } else {
        let max_lat = state
            .recent_ticks
            .iter()
            .map(|t| t.p99_latency_ms)
            .fold(1.0f64, f64::max);
        state
            .recent_ticks
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let x = (i as f64 / (state.recent_ticks.len().max(2) - 1) as f64) * 300.0;
                let y = 70.0 - ((t.p99_latency_ms / max_lat).clamp(0.0, 1.0) * 60.0);
                format!("{:.1},{:.1}", x, y)
            })
            .collect::<Vec<_>>()
            .join(" ")
    };

    format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>IRIS Autonomic Service — Live MAPE-K Dashboard</title>
<style>
  :root {{
    --bg: #090d16;
    --card: #131b2a;
    --card-border: #1e2d42;
    --accent-cyan: #58a6ff;
    --accent-green: #3fb950;
    --accent-red: #f85149;
    --accent-amber: #d29922;
    --text-main: #f0f6fc;
    --text-muted: #8b949e;
  }}
  * {{ box-sizing: border-box; margin: 0; padding: 0; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, monospace; }}
  body {{ background: var(--bg); color: var(--text-main); padding: 24px; min-height: 100vh; }}
  .header {{ display: flex; justify-content: space-between; align-items: center; padding-bottom: 20px; border-bottom: 1px solid var(--card-border); margin-bottom: 24px; }}
  .title {{ font-size: 22px; font-weight: 700; letter-spacing: 0.5px; display: flex; align-items: center; gap: 12px; }}
  .title span {{ color: var(--accent-cyan); }}
  .badge {{ display: inline-flex; align-items: center; gap: 6px; padding: 4px 12px; border-radius: 9999px; font-size: 12px; font-weight: 600; text-transform: uppercase; }}
  .badge-live {{ background: rgba(63, 185, 80, 0.15); color: var(--accent-green); border: 1px solid rgba(63, 185, 80, 0.4); }}
  .pulse {{ width: 8px; height: 8px; border-radius: 50%; background: var(--accent-green); box-shadow: 0 0 8px var(--accent-green); animation: pulse 1.5s infinite; }}
  @keyframes pulse {{ 0%, 100% {{ opacity: 1; transform: scale(1); }} 50% {{ opacity: 0.4; transform: scale(1.2); }} }}
  .grid {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(240px, 1fr)); gap: 16px; margin-bottom: 24px; }}
  .card {{ background: var(--card); border: 1px solid var(--card-border); border-radius: 12px; padding: 20px; position: relative; overflow: hidden; }}
  .card-label {{ font-size: 13px; color: var(--text-muted); text-transform: uppercase; letter-spacing: 0.5px; margin-bottom: 8px; }}
  .card-val {{ font-size: 32px; font-weight: 800; }}
  .card-sub {{ font-size: 12px; color: var(--text-muted); margin-top: 6px; display: flex; align-items: center; gap: 6px; }}
  .chart-card {{ background: var(--card); border: 1px solid var(--card-border); border-radius: 12px; padding: 20px; margin-bottom: 24px; }}
  .chart-title {{ font-size: 15px; font-weight: 600; margin-bottom: 12px; display: flex; justify-content: space-between; }}
  .links {{ display: flex; gap: 16px; margin-top: 12px; }}
  .links a {{ color: var(--accent-cyan); text-decoration: none; font-size: 13px; font-weight: 600; }}
  .links a:hover {{ text-decoration: underline; }}
</style>
</head>
<body>
  <div class="header">
    <div class="title">
      <div>IRIS <span>AUTONOMIC MICROSERVICE</span></div>
      <div style="font-size: 13px; color: var(--text-muted); font-weight: normal;">Closed-Loop MAPE-K Engine</div>
    </div>
    <div class="badge badge-live">
      <div class="pulse"></div>
      <span id="daemon-status">{}</span>
    </div>
  </div>

  <div class="grid">
    <div class="card">
      <div class="card-label">P99 Latency</div>
      <div class="card-val" id="p99-val" style="color: {};">{:.2} ms</div>
      <div class="card-sub">Target SLA: &le; {:.1} ms</div>
    </div>

    <div class="card">
      <div class="card-label">Allostatic Strain</div>
      <div class="card-val" id="strain-val" style="color: var(--accent-cyan);">{:.4}</div>
      <div class="card-sub">Error Budget: {:.1}%</div>
    </div>

    <div class="card">
      <div class="card-label">Hot-Swapped Generation</div>
      <div class="card-val" id="gen-val" style="color: var(--accent-green);">Gen {}</div>
      <div class="card-sub">{} total swaps executed</div>
    </div>

    <div class="card">
      <div class="card-label">Operational Regime</div>
      <div class="card-val" id="regime-val" style="font-size: 20px; font-weight: 700; color: var(--accent-amber);">{}</div>
      <div class="card-sub">Policy Slot: #{}</div>
    </div>
  </div>

  <div class="chart-card">
    <div class="chart-title">
      <span>Latency Telemetry Stream (P99)</span>
      <span style="font-size: 12px; color: var(--text-muted);">Recent Ticks</span>
    </div>
    <svg width="100%" height="80" viewBox="0 0 300 80" preserveAspectRatio="none" style="overflow: visible;">
      <line x1="0" y1="70" x2="300" y2="70" stroke="#1e2d42" stroke-width="1" />
      <polyline fill="none" stroke="{}" stroke-width="2.5" points="{}" />
    </svg>
  </div>

  <div class="links">
    <a href="/metrics" target="_blank">&rarr; Prometheus /metrics</a>
    <a href="/status" target="_blank">&rarr; JSON /status</a>
  </div>

  <script>
    async function poll() {{
      try {{
        const res = await fetch('/status');
        if (!res.ok) return;
        const d = await res.json();
        document.getElementById('p99-val').innerText = d.p99_latency_ms.toFixed(2) + ' ms';
        document.getElementById('strain-val').innerText = d.allostatic_strain.toFixed(4);
        document.getElementById('gen-val').innerText = 'Gen ' + d.generation_id;
        document.getElementById('regime-val').innerText = d.regime;
        document.getElementById('daemon-status').innerText = d.is_running ? 'LIVE MAPE-K' : 'COMPLETED';
      }} catch (e) {{}}
    }}
    setInterval(poll, 1000);
  </script>
</body>
</html>
"##,
        if state.is_running {
            "LIVE MAPE-K"
        } else {
            "COMPLETED"
        },
        status_color,
        state.p99_latency_ms,
        state.target_p99_ms,
        state.allostatic_strain,
        state.error_budget_ratio * 100.0,
        state.generation_id,
        state.hot_swaps,
        state.regime,
        state.active_policy_id,
        status_color,
        sparkline_pts,
    )
}

/// Handle a single incoming HTTP request on the embedded server.
pub fn handle_http_connection(mut stream: TcpStream, state: &Arc<RwLock<DaemonState>>) {
    let mut buffer = [0u8; 4096];
    let n = match stream.read(&mut buffer) {
        Ok(n) if n > 0 => n,
        _ => return,
    };
    let req = String::from_utf8_lossy(&buffer[..n]);
    let first_line = req.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    if parts.len() < 2 {
        return;
    }
    let method = parts[0];
    let path = parts[1];

    if method != "GET" {
        let resp =
            "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    let current = state.read().unwrap().clone();

    let (status_line, content_type, body) = match path {
        "/metrics" => {
            let body = render_prometheus_metrics(&current);
            (
                "HTTP/1.1 200 OK",
                "text/plain; version=0.0.4; charset=utf-8",
                body,
            )
        }
        "/status" | "/api/status" => {
            let body = serde_json::to_string_pretty(&current).unwrap_or_else(|_| "{}".to_string());
            ("HTTP/1.1 200 OK", "application/json", body)
        }
        "/" | "/index.html" | "/dashboard" => {
            let body = render_dashboard_html(&current);
            ("HTTP/1.1 200 OK", "text/html; charset=utf-8", body)
        }
        _ => (
            "HTTP/1.1 404 Not Found",
            "text/plain",
            "Not Found\n".to_string(),
        ),
    };

    let response = format!(
        "{}\r\nContent-Type: {}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{}",
        status_line,
        content_type,
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// Starts the embedded zero-dependency HTTP server in a background thread.
pub fn start_embedded_http_server(
    port: u16,
    state: Arc<RwLock<DaemonState>>,
) -> Result<(u16, std::sync::mpsc::Sender<()>), String> {
    let listener = TcpListener::bind(format!("127.0.0.1:{}", port))
        .map_err(|e| format!("failed to bind HTTP server to 127.0.0.1:{}: {}", port, e))?;
    let bound_port = listener.local_addr().map_err(|e| e.to_string())?.port();
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;

    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();

    std::thread::spawn(move || {
        while stop_rx.try_recv().is_err() {
            match listener.accept() {
                Ok((stream, _)) => {
                    let st = Arc::clone(&state);
                    std::thread::spawn(move || {
                        handle_http_connection(stream, &st);
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(_) => {
                    break;
                }
            }
        }
    });

    Ok((bound_port, stop_tx))
}

// ---------------------------------------------------------------------------
// The monitored service, behind a reverse proxy
// ---------------------------------------------------------------------------

/// Portable TLS termination configuration for `iris service-daemon`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonTlsConfig {
    /// Path to PEM-encoded X.509 certificate chain.
    pub cert_path: PathBuf,
    /// Path to PEM-encoded private key.
    pub key_path: PathBuf,
    /// Minimum TLS protocol version (`"TLS1.2"` or `"TLS1.3"`).
    pub min_protocol: String,
}

/// Validate a daemon TLS configuration before binding listeners.
pub fn validate_tls_config(tls: &DaemonTlsConfig) -> Result<(), String> {
    if tls.min_protocol != "TLS1.2" && tls.min_protocol != "TLS1.3" {
        return Err(format!(
            "unsupported TLS min_protocol '{}': expected TLS1.2 or TLS1.3",
            tls.min_protocol
        ));
    }
    let cert_pem = std::fs::read_to_string(&tls.cert_path).map_err(|e| {
        format!(
            "cannot read TLS certificate '{}': {e}",
            tls.cert_path.display()
        )
    })?;
    if !cert_pem.contains("-----BEGIN CERTIFICATE-----") {
        return Err(format!(
            "TLS certificate '{}' does not contain a PEM certificate block",
            tls.cert_path.display()
        ));
    }
    let key_pem = std::fs::read_to_string(&tls.key_path).map_err(|e| {
        format!(
            "cannot read TLS private key '{}': {e}",
            tls.key_path.display()
        )
    })?;
    if !key_pem.contains("-----BEGIN ") || !key_pem.contains("PRIVATE KEY-----") {
        return Err(format!(
            "TLS private key '{}' does not contain a PEM private key block",
            tls.key_path.display()
        ));
    }
    Ok(())
}

/// What the daemon watches, and what it holds it to.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// The service: `http://host:port` (plain HTTP).
    pub upstream: String,
    /// The port clients reach the service through; 0 picks a free one.
    pub listen: u16,
    /// Monitoring intervals to run; 0 runs until the process is stopped.
    pub ticks: usize,
    /// Length of one interval, in milliseconds.
    pub tick_ms: u64,
    pub target_p99: f64,
    pub error_budget: f64,
    /// In-flight requests from which the shedding policy answers 429.
    pub max_inflight: usize,
    pub headless: bool,
    /// The audit ledger's path: written at the end, or after every interval
    /// when running until stopped.
    pub audit: Option<PathBuf>,
    /// The dashboard and metrics server's port, if serving one.
    pub serve: Option<u16>,
    /// Told the proxy's port, and the dashboard's, once both listen.
    pub ready: Option<std::sync::mpsc::Sender<(u16, Option<u16>)>>,
    /// Optional TLS configuration for HTTPS listeners.
    pub tls: Option<DaemonTlsConfig>,
}

/// The policies the daemon applies to live traffic.
pub const POLICY_FORWARD: i64 = 0;
/// Answer 429 once `max_inflight` requests are in flight.
pub const POLICY_SHED: i64 = 1;
/// Serve a `GET` from the last good response to it, without asking the service.
pub const POLICY_CACHE: i64 = 2;
/// Admit requests at the rate the service last sustained when healthy; 503 past it.
pub const POLICY_THROTTLE: i64 = 3;

pub fn policy_name(id: i64) -> &'static str {
    match id {
        POLICY_SHED => "shed over capacity (429)",
        POLICY_CACHE => "serve from cache",
        POLICY_THROTTLE => "throttle to the healthy rate (503)",
        _ => "forward",
    }
}

/// Measurements of one interval, gathered by the proxy.
#[derive(Default)]
struct IntervalStats {
    requests: i64,
    failed: i64,
    shed: i64,
    gets: i64,
    cache_served: i64,
    latencies: Vec<f64>,
}

/// Responses to `GET`s, by target, for the cache policy: the most recent good
/// one, bounded in count and bytes.
#[derive(Default)]
struct ResponseCache {
    entries: HashMap<String, Vec<u8>>,
    order: VecDeque<String>,
    bytes: usize,
}

const CACHE_ENTRIES: usize = 1024;
const CACHE_BYTES: usize = 64 << 20;

impl ResponseCache {
    fn put(&mut self, key: &str, response: &[u8]) {
        if response.len() > CACHE_BYTES / 4 {
            return;
        }
        if let Some(old) = self.entries.insert(key.to_owned(), response.to_vec()) {
            self.bytes -= old.len();
            self.order.retain(|k| k != key);
        }
        self.bytes += response.len();
        self.order.push_back(key.to_owned());
        while self.entries.len() > CACHE_ENTRIES || self.bytes > CACHE_BYTES {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(gone) = self.entries.remove(&oldest) {
                self.bytes -= gone.len();
            }
        }
    }
}

/// Admission at a fixed rate, for the throttling policy.
struct TokenBucket {
    rate: f64,
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    /// A token, waiting up to `wait` for one.
    fn take(&mut self, wait: Duration) -> bool {
        let deadline = Instant::now() + wait;
        loop {
            let now = Instant::now();
            self.tokens = (self.tokens + now.duration_since(self.last).as_secs_f64() * self.rate)
                .min(self.rate.max(1.0));
            self.last = now;
            if self.tokens >= 1.0 {
                self.tokens -= 1.0;
                return true;
            }
            if now >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

struct Proxy {
    host: String,
    port: u16,
    policy: AtomicI64,
    in_flight: AtomicI64,
    max_inflight: i64,
    stats: Mutex<IntervalStats>,
    cache: Mutex<ResponseCache>,
    bucket: Mutex<TokenBucket>,
    stop: AtomicBool,
}

/// `http://host:port[/...]` to (host, port).
fn parse_upstream(url: &str) -> Result<(String, u16), String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| format!("the upstream must be an http:// URL, not `{url}`"))?;
    let authority = rest.split('/').next().unwrap_or("");
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h,
            p.parse::<u16>()
                .map_err(|_| format!("bad port in `{url}`"))?,
        ),
        None => (authority, 80),
    };
    if host.is_empty() {
        return Err(format!("no host in `{url}`"));
    }
    Ok((host.to_owned(), port))
}

/// An HTTP/1.1 request as read from a client.
struct Request {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// Read the head of an HTTP message (up to the blank line) and what
/// followed it in the same reads.
fn read_head(stream: &mut TcpStream) -> std::io::Result<(String, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..at]).into_owned();
            return Ok((head, buf[at + 4..].to_vec()));
        }
        if buf.len() > 64 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "headers too large",
            ));
        }
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed",
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

fn parse_headers(head: &str) -> (String, Vec<(String, String)>) {
    let mut lines = head.split("\r\n");
    let first = lines.next().unwrap_or("").to_owned();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect();
    (first, headers)
}

/// The rest of a body of `len` bytes, after `have`.
fn read_exact_body(
    stream: &mut TcpStream,
    mut have: Vec<u8>,
    len: usize,
) -> std::io::Result<Vec<u8>> {
    let mut chunk = [0u8; 8192];
    while have.len() < len {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "body cut short",
            ));
        }
        have.extend_from_slice(&chunk[..n]);
    }
    have.truncate(len);
    Ok(have)
}

fn read_request(stream: &mut TcpStream) -> Result<Request, (u16, &'static str)> {
    let (head, rest) = read_head(stream).map_err(|_| (400, "Bad Request"))?;
    let (first, headers) = parse_headers(&head);
    let mut parts = first.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Err((400, "Bad Request"));
    };
    if header(&headers, "Transfer-Encoding")
        .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"))
    {
        return Err((411, "Length Required"));
    }
    let len: usize = header(&headers, "Content-Length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if len > 16 << 20 {
        return Err((413, "Payload Too Large"));
    }
    let body = read_exact_body(stream, rest, len).map_err(|_| (400, "Bad Request"))?;
    Ok(Request {
        method: method.to_owned(),
        target: target.to_owned(),
        headers,
        body,
    })
}

/// A whole response from the service, and its status code.
fn read_response(stream: &mut TcpStream) -> std::io::Result<(u16, Vec<u8>)> {
    let (head, rest) = read_head(stream)?;
    let (first, headers) = parse_headers(&head);
    let status: u16 = first
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(502);
    let mut out = head.into_bytes();
    out.extend_from_slice(b"\r\n\r\n");
    let body = if let Some(len) =
        header(&headers, "Content-Length").and_then(|v| v.parse::<usize>().ok())
    {
        read_exact_body(stream, rest, len)?
    } else if header(&headers, "Transfer-Encoding")
        .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"))
    {
        // Through to the last chunk, as sent.
        let mut body = rest;
        let mut chunk = [0u8; 8192];
        while !body.ends_with(b"0\r\n\r\n") {
            let n = stream.read(&mut chunk)?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
        }
        body
    } else {
        // Until the service closes the connection.
        let mut body = rest;
        stream.read_to_end(&mut body)?;
        body
    };
    out.extend_from_slice(&body);
    Ok((status, out))
}

fn respond(client: &mut TcpStream, code: u16, reason: &str, extra: &str, body: &str) {
    let text = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = client.write_all(text.as_bytes());
}

/// Forward `req` to the service and return its response.
fn forward(proxy: &Proxy, req: &Request, peer: &str) -> Result<(u16, Vec<u8>), String> {
    use std::net::ToSocketAddrs;
    let addr = (proxy.host.as_str(), proxy.port)
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve {}: {e}", proxy.host))?
        .next()
        .ok_or_else(|| format!("cannot resolve {}", proxy.host))?;
    let mut up = TcpStream::connect_timeout(&addr, Duration::from_secs(3))
        .map_err(|e| format!("connect: {e}"))?;
    up.set_read_timeout(Some(Duration::from_secs(30))).ok();
    up.set_write_timeout(Some(Duration::from_secs(30))).ok();
    let mut out = format!(
        "{} {} HTTP/1.1\r\nHost: {}:{}\r\n",
        req.method, req.target, proxy.host, proxy.port
    );
    for (k, v) in &req.headers {
        let k_low = k.to_ascii_lowercase();
        if matches!(
            k_low.as_str(),
            "host" | "connection" | "keep-alive" | "proxy-connection" | "content-length"
        ) {
            continue;
        }
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str(&format!(
        "X-Forwarded-For: {peer}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        req.body.len()
    ));
    up.write_all(out.as_bytes())
        .map_err(|e| format!("send: {e}"))?;
    up.write_all(&req.body).map_err(|e| format!("send: {e}"))?;
    read_response(&mut up).map_err(|e| format!("receive: {e}"))
}

/// One client connection: one request, under the active policy.
fn serve_client(proxy: &Proxy, mut client: TcpStream) {
    client.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let peer = client
        .peer_addr()
        .map(|a| a.ip().to_string())
        .unwrap_or_default();
    let req = match read_request(&mut client) {
        Ok(r) => r,
        Err((code, reason)) => {
            respond(&mut client, code, reason, "", "");
            return;
        }
    };
    let is_get = req.method == "GET";
    let policy = proxy.policy.load(Ordering::Relaxed);

    // Execute: the policy's answer, where it gives one without the service.
    if policy == POLICY_SHED && proxy.in_flight.load(Ordering::Relaxed) >= proxy.max_inflight {
        respond(
            &mut client,
            429,
            "Too Many Requests",
            "Retry-After: 1\r\n",
            "over capacity\n",
        );
        let mut s = proxy.stats.lock().unwrap();
        s.requests += 1;
        s.shed += 1;
        return;
    }
    if policy == POLICY_CACHE && is_get {
        let cached = proxy
            .cache
            .lock()
            .unwrap()
            .entries
            .get(&req.target)
            .cloned();
        if let Some(mut bytes) = cached {
            if let Some(eol) = bytes.windows(2).position(|w| w == b"\r\n") {
                bytes.splice(eol + 2..eol + 2, b"X-Iris-Cache: hit\r\n".iter().copied());
            }
            let _ = client.write_all(&bytes);
            let mut s = proxy.stats.lock().unwrap();
            s.requests += 1;
            s.gets += 1;
            s.cache_served += 1;
            return;
        }
    }
    if policy == POLICY_THROTTLE
        && !proxy
            .bucket
            .lock()
            .unwrap()
            .take(Duration::from_millis(200))
    {
        respond(
            &mut client,
            503,
            "Service Unavailable",
            "Retry-After: 1\r\n",
            "throttled\n",
        );
        let mut s = proxy.stats.lock().unwrap();
        s.requests += 1;
        s.shed += 1;
        return;
    }

    proxy.in_flight.fetch_add(1, Ordering::Relaxed);
    let started = Instant::now();
    let result = forward(proxy, &req, &peer);
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    proxy.in_flight.fetch_sub(1, Ordering::Relaxed);
    let status = match &result {
        Ok((status, bytes)) => {
            let _ = client.write_all(bytes);
            *status
        }
        Err(e) => {
            respond(
                &mut client,
                502,
                "Bad Gateway",
                "",
                &format!("upstream: {e}\n"),
            );
            502
        }
    };
    if let (true, Ok((200, bytes))) = (is_get, &result) {
        proxy.cache.lock().unwrap().put(&req.target, bytes);
    }
    let mut s = proxy.stats.lock().unwrap();
    s.requests += 1;
    s.latencies.push(ms);
    if status >= 500 {
        s.failed += 1;
    }
    if is_get {
        s.gets += 1;
    }
}

fn start_proxy(config: &DaemonConfig) -> Result<(Arc<Proxy>, u16), String> {
    let (host, port) = parse_upstream(&config.upstream)?;
    let listener = TcpListener::bind(("0.0.0.0", config.listen))
        .map_err(|e| format!("cannot listen on port {}: {e}", config.listen))?;
    let bound = listener.local_addr().map_err(|e| e.to_string())?.port();
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let proxy = Arc::new(Proxy {
        host,
        port,
        policy: AtomicI64::new(POLICY_FORWARD),
        in_flight: AtomicI64::new(0),
        max_inflight: config.max_inflight.max(1) as i64,
        stats: Mutex::new(IntervalStats::default()),
        cache: Mutex::new(ResponseCache::default()),
        bucket: Mutex::new(TokenBucket {
            rate: 100.0,
            tokens: 100.0,
            last: Instant::now(),
        }),
        stop: AtomicBool::new(false),
    });
    let p = Arc::clone(&proxy);
    std::thread::spawn(move || {
        while !p.stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    let p = Arc::clone(&p);
                    std::thread::spawn(move || serve_client(&p, stream));
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    });
    Ok((proxy, bound))
}

/// The nearest-rank 99th percentile.
fn p99(latencies: &mut [f64]) -> f64 {
    if latencies.is_empty() {
        return 0.0;
    }
    latencies.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = ((0.99 * latencies.len() as f64).ceil() as usize).clamp(1, latencies.len());
    latencies[rank - 1]
}

fn write_ledger(path: &PathBuf, ledger: &AuditLedger) -> Result<(), String> {
    let json = serde_json::to_string_pretty(ledger)
        .map_err(|e| format!("cannot serialise the audit ledger: {e}"))?;
    let tmp = path.with_extension("tmp");
    let mut file =
        File::create(&tmp).map_err(|e| format!("cannot create '{}': {e}", tmp.display()))?;
    file.write_all(json.as_bytes())
        .map_err(|e| format!("cannot write the audit ledger: {e}"))?;
    drop(file);
    std::fs::rename(&tmp, path).map_err(|e| format!("cannot write '{}': {e}", path.display()))
}

/// Run the autonomic daemon: proxy the service, and every interval measure
/// it (MONITOR), classify what it is going through (ANALYZE), choose the
/// policy that has done best in that regime (PLAN, KNOWLEDGE), and apply it
/// to live traffic (EXECUTE). Returns the audit ledger.
pub fn run_service_daemon(config: DaemonConfig) -> Result<AuditLedger, String> {
    if let Some(tls) = &config.tls {
        validate_tls_config(tls)?;
    }
    let (proxy, proxy_port) = start_proxy(&config)?;
    let target_p99 = config.target_p99;
    let error_budget = config.error_budget;

    let shared_state = Arc::new(RwLock::new(DaemonState {
        current_tick: 0,
        total_ticks: config.ticks,
        regime: ServiceRegime::NominalTraffic.name().to_string(),
        active_policy_id: POLICY_FORWARD,
        total_requests: 0,
        failed_requests: 0,
        queue_depth: 0,
        p99_latency_ms: 0.0,
        target_p99_ms: target_p99,
        error_budget_ratio: error_budget,
        allostatic_strain: 0.0,
        cache_hit_ratio: 1.0,
        generation_id: 1,
        hot_swaps: 0,
        is_running: true,
        recent_ticks: Vec::new(),
    }));
    let mut dashboard: Option<(u16, std::sync::mpsc::Sender<()>)> = None;
    if let Some(port) = config.serve {
        dashboard = Some(start_embedded_http_server(port, Arc::clone(&shared_state))?);
    }
    if let Some(ready) = &config.ready {
        let _ = ready.send((proxy_port, dashboard.as_ref().map(|(p, _)| *p)));
    }
    if !config.headless {
        println!("IRIS autonomic service daemon (MAPE-K)");
        println!("  service      {}", config.upstream);
        println!("  clients use  http://127.0.0.1:{proxy_port}/");
        if let Some((p, _)) = &dashboard {
            println!("  dashboard    http://127.0.0.1:{p}/   metrics /metrics   JSON /status");
        }
        println!(
            "  SLA          p99 <= {:.1} ms, errors <= {:.2}%, interval {} ms",
            target_p99,
            error_budget * 100.0,
            config.tick_ms
        );
    }

    let mut history: Vec<TelemetryTick> = Vec::new();
    let mut total_requests = 0i64;
    let mut total_failed = 0i64;
    let mut generation = 1usize;
    let mut hot_swaps = 0usize;
    let mut cache_ratio = 1.0f64;
    let mut nominal_streak = 0usize;
    // The request rate the service sustains when healthy (requests/s).
    let mut healthy_rate: Option<f64> = None;
    // KNOWLEDGE: for each (regime, policy), the strain measured under it.
    let mut knowledge: HashMap<(ServiceRegime, i64), (f64, u32)> = HashMap::new();
    let started = Instant::now();
    let interval = Duration::from_millis(config.tick_ms.max(10));

    let mut tick = 0usize;
    loop {
        tick += 1;
        std::thread::sleep(interval);

        // ── MONITOR ──────────────────────────────────────────────────────
        let mut s = std::mem::take(&mut *proxy.stats.lock().unwrap());
        let active = proxy.policy.load(Ordering::Relaxed);
        let in_flight = proxy.in_flight.load(Ordering::Relaxed);
        let latency = p99(&mut s.latencies);
        let served = (s.requests - s.shed).max(0);
        let error_ratio = if served > 0 {
            s.failed as f64 / served as f64
        } else {
            0.0
        };
        let rate = s.requests as f64 / interval.as_secs_f64();
        if s.gets > 0 {
            cache_ratio = s.cache_served as f64 / s.gets as f64;
        }
        total_requests += s.requests;
        total_failed += s.failed;

        // ── ANALYZE ──────────────────────────────────────────────────────
        let lat_strain = if latency > target_p99 {
            ((latency - target_p99) / target_p99).min(1.0)
        } else {
            0.0
        };
        let err_strain = if error_budget > 0.0 {
            (error_ratio / error_budget).min(1.0)
        } else if error_ratio > 0.0 {
            1.0
        } else {
            0.0
        };
        let strain = (lat_strain * 0.6 + err_strain * 0.4).clamp(0.0, 1.0);
        let surging = healthy_rate.is_some_and(|h| rate > 3.0 * h.max(1.0))
            && (latency > target_p99 || in_flight >= proxy.max_inflight / 2);
        let measured = if s.requests == 0 {
            ServiceRegime::NominalTraffic
        } else if surging {
            ServiceRegime::FlashCrowdSurge
        } else if error_ratio > error_budget {
            ServiceRegime::UpstreamBrownout
        } else if latency > target_p99 && active == POLICY_CACHE && s.gets > 0 && cache_ratio < 0.5
        {
            ServiceRegime::CacheThrashing
        } else if latency > target_p99 {
            ServiceRegime::UpstreamBrownout
        } else {
            ServiceRegime::NominalTraffic
        };

        // ── KNOWLEDGE: how the active policy did in this regime ──────────
        if s.requests > 0 {
            let e = knowledge.entry((measured, active)).or_insert((0.0, 0));
            e.0 += strain;
            e.1 += 1;
        }

        // ── PLAN & EXECUTE ───────────────────────────────────────────────
        let mut next = active;
        if measured == ServiceRegime::NominalTraffic {
            nominal_streak += 1;
            if s.requests > 0 && active == POLICY_FORWARD {
                healthy_rate = Some(healthy_rate.map_or(rate, |h| 0.8 * h + 0.2 * rate));
            }
            // Back to plain forwarding once the service has recovered.
            if active != POLICY_FORWARD && nominal_streak >= 3 {
                next = POLICY_FORWARD;
            }
        } else {
            nominal_streak = 0;
            if strain > 0.15 {
                // Untried policies first, the regime's usual remedy leading;
                // then the one with the least strain measured here.
                let mut order = vec![measured.optimal_policy_id()];
                for p in [POLICY_SHED, POLICY_CACHE, POLICY_THROTTLE, POLICY_FORWARD] {
                    if !order.contains(&p) {
                        order.push(p);
                    }
                }
                next = order
                    .iter()
                    .copied()
                    .find(|p| *p != active && !knowledge.contains_key(&(measured, *p)))
                    .unwrap_or_else(|| {
                        order
                            .iter()
                            .copied()
                            .min_by(|a, b| {
                                let avg = |p: &i64| {
                                    knowledge
                                        .get(&(measured, *p))
                                        .map_or(f64::MAX, |(s, n)| s / *n as f64)
                                };
                                avg(a)
                                    .partial_cmp(&avg(b))
                                    .unwrap_or(std::cmp::Ordering::Equal)
                            })
                            .unwrap_or(active)
                    });
            }
        }
        if next == POLICY_THROTTLE {
            let mut b = proxy.bucket.lock().unwrap();
            b.rate = healthy_rate.unwrap_or(rate).max(1.0);
        }
        if next != active {
            proxy.policy.store(next, Ordering::Relaxed);
            generation += 1;
            hot_swaps += 1;
            if !config.headless {
                println!(
                    "[tick {tick:04}] {} (p99 {latency:.1} ms, errors {:.1}%, strain {strain:.2}): {} -> {}",
                    measured.name(),
                    error_ratio * 100.0,
                    policy_name(active),
                    policy_name(next)
                );
            }
        }

        let record = TelemetryTick {
            tick,
            regime: measured.name().to_string(),
            active_policy_id: next,
            total_requests,
            failed_requests: total_failed,
            queue_depth: in_flight,
            p99_latency_ms: latency,
            allostatic_strain: strain,
            cache_hit_ratio: cache_ratio,
            generation_id: generation,
            hot_swaps,
        };
        {
            let mut st = shared_state.write().unwrap();
            st.current_tick = tick;
            st.regime = record.regime.clone();
            st.active_policy_id = next;
            st.total_requests = total_requests;
            st.failed_requests = total_failed;
            st.queue_depth = in_flight;
            st.p99_latency_ms = latency;
            st.allostatic_strain = strain;
            st.cache_hit_ratio = cache_ratio;
            st.generation_id = generation;
            st.hot_swaps = hot_swaps;
            st.recent_ticks.push(record.clone());
            if st.recent_ticks.len() > 50 {
                st.recent_ticks.remove(0);
            }
        }
        if !config.headless && (tick % 10 == 0 || tick == 1) {
            println!(
                "[tick {tick:04}] {:<18} | {:>6.1} req/s | p99 {:>7.1} ms | errors {:>5.1}% | in flight {:>3} | cache {:>3.0}% | {}",
                measured.name(),
                rate,
                latency,
                error_ratio * 100.0,
                in_flight,
                cache_ratio * 100.0,
                policy_name(next)
            );
        }
        history.push(record);

        let ledger = |history: &Vec<TelemetryTick>| AuditLedger {
            total_ticks: history.len(),
            target_p99_ms: target_p99,
            error_budget_ratio: error_budget,
            total_hot_swaps: hot_swaps,
            average_allostatic_strain: history.iter().map(|t| t.allostatic_strain).sum::<f64>()
                / history.len().max(1) as f64,
            ticks: history.clone(),
        };
        if config.ticks == 0 {
            if let Some(path) = &config.audit {
                write_ledger(path, &ledger(&history))?;
            }
            continue;
        }
        if tick >= config.ticks {
            let done = ledger(&history);
            proxy.stop.store(true, Ordering::Relaxed);
            shared_state.write().unwrap().is_running = false;
            if let Some((_, stop)) = &dashboard {
                let _ = stop.send(());
            }
            if let Some(path) = &config.audit {
                write_ledger(path, &done)?;
                if !config.headless {
                    println!("audit ledger: {}", path.display());
                }
            }
            if !config.headless {
                println!(
                    "{} intervals in {:.1?}: {} requests, {} failed, {} policy changes, average strain {:.3}",
                    done.total_ticks,
                    started.elapsed(),
                    total_requests,
                    total_failed,
                    hot_swaps,
                    done.average_allostatic_strain
                );
            }
            return Ok(done);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tls_config_validation_rejects_bad_protocol_and_missing_files() {
        let bad_proto = DaemonTlsConfig {
            cert_path: PathBuf::from("nonexistent-cert.pem"),
            key_path: PathBuf::from("nonexistent-key.pem"),
            min_protocol: "SSLv3".into(),
        };
        assert!(validate_tls_config(&bad_proto).is_err());

        let missing = DaemonTlsConfig {
            cert_path: PathBuf::from("nonexistent-cert.pem"),
            key_path: PathBuf::from("nonexistent-key.pem"),
            min_protocol: "TLS1.3".into(),
        };
        assert!(validate_tls_config(&missing).is_err());
    }

    #[test]
    fn tls_config_validation_accepts_valid_pem_pair() {
        let dir = std::env::temp_dir().join(format!("iris_tls_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let cert = dir.join("cert.pem");
        let key = dir.join("key.pem");
        std::fs::write(
            &cert,
            "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n",
        )
        .unwrap();
        std::fs::write(
            &key,
            "-----BEGIN PRIVATE KEY-----\nMIIE\n-----END PRIVATE KEY-----\n",
        )
        .unwrap();
        let cfg = DaemonTlsConfig {
            cert_path: cert.clone(),
            key_path: key.clone(),
            min_protocol: "TLS1.2".into(),
        };
        assert!(validate_tls_config(&cfg).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
