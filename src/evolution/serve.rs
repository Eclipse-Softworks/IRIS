//! A program served over HTTP while new versions of it are promoted.
//!
//! `iris evolve-serve` installs a baseline program under a
//! [`ProgramCoordinator`] and serves its gateways natively. A new version is
//! offered as IRIS source; it runs through the seven gates against the
//! operator's scenarios while traffic keeps flowing, and on promotion takes
//! over the live program state. Calls wait only for the handoff itself (a
//! read of the old generation's state and a write into the new one), so
//! every call lands entirely in one generation and no update is lost.
//!
//! | Request | Meaning |
//! | --- | --- |
//! | `POST /call/<gateway>` | The body is the gateway's input (JSON); the reply is its result, with the serving generation in `X-Iris-Generation` |
//! | `GET /status` | Generation, request counts, live state, decisions |
//! | `POST /upgrade` | The body is a candidate's source: 200 with the receipt, 409 naming the gate that refused it, 422 if it does not compile |
//! | `POST /rollback` | Restore the generation the last promotion replaced, carrying the live state back |
//! | `POST /shutdown` | Stop accepting connections, finish those in flight |
//!
//! Limits: one request per connection; a gateway that panics on a live input
//! stops the host (native calls are not trapped); upgrades and rollbacks are
//! serialised with each other.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};

use super::audit::{EvolutionAuditLog, EvolutionEvent, EvolutionEventKind};
use super::manifest::ProgramManifest;
use super::program::{ProgramCoordinator, ProgramPolicy, ProgramPromotionReceipt, Scenario};
use super::{EvolutionError, TrustedConstitution};
use crate::ir::module::IrModule;

/// Everything `serve` needs besides the audit log.
pub struct ServeConfig {
    pub manifest: ProgramManifest,
    pub baseline: IrModule,
    pub scenarios: Vec<Scenario>,
    pub constitution: TrustedConstitution,
    /// Every number in every scenario result must lie in this range.
    pub min_output: f64,
    pub max_output: f64,
    /// Required test functions; `test_program` when empty.
    pub tests: Vec<String>,
    /// 0 picks a free port.
    pub port: u16,
    pub max_body_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServeEventKind {
    Installed,
    Promoted,
    Rejected,
    RolledBack,
}

impl ServeEventKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::Promoted => "promoted",
            Self::Rejected => "rejected",
            Self::RolledBack => "rolled back",
        }
    }
}

/// One decision, with the request count at the moment it took effect.
#[derive(Debug, Clone, Serialize)]
pub struct ServeEvent {
    pub kind: ServeEventKind,
    pub at_ms: u64,
    pub requests: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct ServeReport {
    pub requests: u64,
    /// Requests served by each generation.
    pub served_by: BTreeMap<u64, u64>,
    pub events: Vec<ServeEvent>,
    pub generation: u64,
    pub state: serde_json::Map<String, Value>,
}

struct Shared {
    coordinator: ProgramCoordinator,
    scenarios: Vec<Scenario>,
    constitution: TrustedConstitution,
    min_output: f64,
    max_output: f64,
    max_body_bytes: usize,
    started: Instant,
    /// Upgrades and rollbacks, one at a time, with the log they write.
    admin: Mutex<Admin>,
    counts: Mutex<BTreeMap<u64, u64>>,
    events: Mutex<Vec<ServeEvent>>,
    stopping: AtomicBool,
    address: SocketAddr,
    on_event: Box<dyn Fn(&ServeEvent) + Send + Sync>,
}

struct Admin {
    audit: EvolutionAuditLog,
    /// Promotions not yet rolled back, oldest first.
    promotions: Vec<ProgramPromotionReceipt>,
    upgrades: usize,
}

impl Shared {
    fn requests(&self) -> u64 {
        self.counts
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .sum()
    }

    fn record(&self, kind: ServeEventKind, generation: Option<u64>, detail: String) -> ServeEvent {
        let event = ServeEvent {
            kind,
            at_ms: self.started.elapsed().as_millis() as u64,
            requests: self.requests(),
            generation,
            detail,
        };
        (self.on_event)(&event);
        self.events
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(event.clone());
        event
    }
}

/// Serve `config.baseline` until `POST /shutdown`. `on_ready` receives the
/// bound address once the listener is up; `on_event` sees every decision.
pub fn serve(
    config: ServeConfig,
    audit: EvolutionAuditLog,
    on_ready: impl FnOnce(SocketAddr),
    on_event: impl Fn(&ServeEvent) + Send + Sync + 'static,
) -> Result<ServeReport, EvolutionError> {
    serve_with_hot_swap(
        config,
        audit,
        crate::codegen::hot_swap::HotSwapConfig::from_env(),
        on_ready,
        on_event,
    )
}

/// Serve `config.baseline` with an explicit hot-swap retention/archiving policy.
pub fn serve_with_hot_swap(
    config: ServeConfig,
    audit: EvolutionAuditLog,
    hot_swap: crate::codegen::hot_swap::HotSwapConfig,
    on_ready: impl FnOnce(SocketAddr),
    on_event: impl Fn(&ServeEvent) + Send + Sync + 'static,
) -> Result<ServeReport, EvolutionError> {
    let mut policy = ProgramPolicy::default();
    if !config.tests.is_empty() {
        policy.required_tests = config.tests;
    }
    let coordinator = ProgramCoordinator::with_hot_swap_config(policy, config.manifest, hot_swap)?;
    let installed = coordinator.install_baseline(config.baseline)?;
    let listener = TcpListener::bind(("127.0.0.1", config.port))
        .map_err(|error| EvolutionError::Runtime(format!("cannot listen: {error}")))?;
    let address = listener
        .local_addr()
        .map_err(|error| EvolutionError::Runtime(error.to_string()))?;
    let shared = Arc::new(Shared {
        coordinator,
        scenarios: config.scenarios,
        constitution: config.constitution,
        min_output: config.min_output,
        max_output: config.max_output,
        max_body_bytes: config.max_body_bytes,
        started: Instant::now(),
        admin: Mutex::new(Admin {
            audit,
            promotions: Vec::new(),
            upgrades: 0,
        }),
        counts: Mutex::new(BTreeMap::new()),
        events: Mutex::new(Vec::new()),
        stopping: AtomicBool::new(false),
        address,
        on_event: Box::new(on_event),
    });
    shared.record(
        ServeEventKind::Installed,
        Some(installed.generation),
        format!("baseline, serving on http://{address}"),
    );
    on_ready(address);

    let mut workers = Vec::new();
    for stream in listener.incoming() {
        if shared.stopping.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = stream else { continue };
        let shared = Arc::clone(&shared);
        // Compiling and gating a candidate recurses deeply.
        let worker = std::thread::Builder::new()
            .stack_size(64 << 20)
            .spawn(move || handle(stream, &shared))
            .map_err(|error| EvolutionError::Runtime(error.to_string()))?;
        workers.push(worker);
        workers.retain(|worker| !worker.is_finished());
    }
    for worker in workers {
        let _ = worker.join();
    }

    let (generation, state) = shared.coordinator.snapshot_state()?;
    let served_by = shared
        .counts
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    let events = shared
        .events
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    Ok(ServeReport {
        requests: served_by.values().sum(),
        served_by,
        events,
        generation,
        state,
    })
}

struct Request {
    method: String,
    path: String,
    body: Vec<u8>,
}

struct Response {
    status: u16,
    body: Value,
    generation: Option<u64>,
}

fn reply(status: u16, body: Value) -> Response {
    Response {
        status,
        body,
        generation: None,
    }
}

fn handle(mut stream: TcpStream, shared: &Shared) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let response = match read_request(&mut stream, shared.max_body_bytes) {
        Ok(request) => route(request, shared),
        Err((status, detail)) => reply(status, json!({ "error": detail })),
    };
    let body = response.body.to_string();
    let reason = match response.status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        409 => "Conflict",
        413 => "Payload Too Large",
        422 => "Unprocessable Entity",
        _ => "Internal Server Error",
    };
    let generation = response
        .generation
        .map(|generation| format!("X-Iris-Generation: {generation}\r\n"))
        .unwrap_or_default();
    let head = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{generation}Connection: close\r\n\r\n",
        response.status,
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

fn read_request(stream: &mut TcpStream, max_body: usize) -> Result<Request, (u16, String)> {
    let bad = |detail: &str| (400, detail.to_owned());
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(at) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break at;
        }
        if buffer.len() > 16 * 1024 {
            return Err(bad("request headers exceed 16 KiB"));
        }
        let read = stream
            .read(&mut chunk)
            .map_err(|error| bad(&format!("cannot read the request: {error}")))?;
        if read == 0 {
            return Err(bad("the connection closed before the headers ended"));
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next().unwrap_or_default().split_whitespace();
    let method = request_line.next().unwrap_or_default().to_owned();
    let path = request_line.next().unwrap_or_default().to_owned();
    if method.is_empty() || !path.starts_with('/') {
        return Err(bad("malformed request line"));
    }
    let mut length = 0usize;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                length = value
                    .trim()
                    .parse()
                    .map_err(|_| bad("Content-Length is not a number"))?;
            }
        }
    }
    if length > max_body {
        return Err((413, format!("the body exceeds {max_body} bytes")));
    }
    let mut body = buffer[header_end + 4..].to_vec();
    while body.len() < length {
        let read = stream
            .read(&mut chunk)
            .map_err(|error| bad(&format!("cannot read the body: {error}")))?;
        if read == 0 {
            return Err(bad("the connection closed before the body ended"));
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(length);
    Ok(Request { method, path, body })
}

fn route(request: Request, shared: &Shared) -> Response {
    let path = request.path.split('?').next().unwrap_or_default();
    match (request.method.as_str(), path) {
        ("POST", path) if path.starts_with("/call/") => {
            call(&path["/call/".len()..], &request.body, shared)
        }
        ("GET", "/status") => status(shared),
        ("POST", "/upgrade") => upgrade(&request.body, shared),
        ("POST", "/rollback") => rollback(shared),
        ("POST", "/shutdown") => {
            shared.stopping.store(true, Ordering::SeqCst);
            // Wake the accept loop so it sees the flag.
            let _ = TcpStream::connect(shared.address);
            reply(200, json!({ "stopping": true }))
        }
        _ => reply(
            404,
            json!({ "error": format!("no route for {} {}", request.method, request.path) }),
        ),
    }
}

fn call(gateway: &str, body: &[u8], shared: &Shared) -> Response {
    let text = String::from_utf8_lossy(body);
    let input = if text.trim().is_empty() {
        Value::Null
    } else {
        match serde_json::from_str::<Value>(&text) {
            Ok(input) => input,
            Err(error) => {
                return reply(
                    400,
                    json!({ "error": format!("the body is not JSON: {error}") }),
                )
            }
        }
    };
    match shared.coordinator.call(gateway, &input) {
        Ok((generation, result)) => {
            *shared
                .counts
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .entry(generation)
                .or_insert(0) += 1;
            Response {
                status: 200,
                body: result,
                generation: Some(generation),
            }
        }
        Err(error) => reply(500, json!({ "error": error.to_string() })),
    }
}

fn status(shared: &Shared) -> Response {
    let (generation, state) = match shared.coordinator.snapshot_state() {
        Ok(snapshot) => snapshot,
        Err(error) => return reply(500, json!({ "error": error.to_string() })),
    };
    let served_by: serde_json::Map<String, Value> = shared
        .counts
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .map(|(generation, count)| (generation.to_string(), Value::from(*count)))
        .collect();
    let events = shared
        .events
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    Response {
        status: 200,
        body: json!({
            "program": shared.coordinator.manifest().program_id,
            "generation": generation,
            "requests": shared.requests(),
            "served_by": served_by,
            "state": state,
            "events": events,
        }),
        generation: Some(generation),
    }
}

/// Every number in `value` lies in `[low, high]`.
fn within(value: &Value, low: f64, high: f64) -> bool {
    match value {
        Value::Number(number) => number.as_f64().is_some_and(|v| v >= low && v <= high),
        Value::Array(items) => items.iter().all(|item| within(item, low, high)),
        Value::Object(map) => map.values().all(|item| within(item, low, high)),
        _ => true,
    }
}

fn upgrade(body: &[u8], shared: &Shared) -> Response {
    let Ok(source) = std::str::from_utf8(body) else {
        return reply(400, json!({ "error": "the candidate is not UTF-8" }));
    };
    let mut admin = shared.admin.lock().unwrap_or_else(|p| p.into_inner());
    admin.upgrades += 1;
    let name = format!("upgrade_{}", admin.upgrades);
    let module = match super::compile_candidate(source, &name) {
        Ok(module) => module,
        Err(error) => {
            let event = shared.record(ServeEventKind::Rejected, None, format!("{name}: {error}"));
            return reply(
                422,
                json!({ "promoted": false, "gate": "compile", "detail": event.detail }),
            );
        }
    };
    let (low, high) = (shared.min_output, shared.max_output);
    let outcome = shared.coordinator.evaluate_and_promote(
        source,
        module,
        &shared.scenarios,
        &shared.constitution,
        |_, output| within(output, low, high),
        Some(&mut admin.audit),
    );
    match outcome {
        Ok(receipt) => {
            let handoff = &receipt.handoff;
            let detail = format!(
                "{name}: generation {} after {}/7 gates; carries {}{}{}",
                receipt.swap.generation,
                receipt.gates.len(),
                list(&handoff.carried),
                if handoff.initialised.is_empty() {
                    String::new()
                } else {
                    format!(", initialises {}", list(&handoff.initialised))
                },
                if handoff.dropped.is_empty() {
                    String::new()
                } else {
                    format!(", drops {}", list(&handoff.dropped))
                },
            );
            shared.record(
                ServeEventKind::Promoted,
                Some(receipt.swap.generation),
                detail,
            );
            let body = json!({
                "promoted": true,
                "generation": receipt.swap.generation,
                "replaced": receipt.swap.replaced_generation,
                "gates": receipt.gates.iter().map(|evidence| json!({
                    "gate": evidence.gate.name(),
                    "detail": evidence.detail,
                })).collect::<Vec<_>>(),
                "carried": handoff.carried,
                "initialised": handoff.initialised,
                "reinitialised": handoff.reinitialised,
                "dropped": handoff.dropped,
                "canary": {
                    "compared": receipt.metrics.compared_steps,
                    "candidate_exact": receipt.metrics.candidate_exact,
                    "baseline_exact": receipt.metrics.baseline_exact,
                },
            });
            let generation = receipt.swap.generation;
            admin.promotions.push(receipt);
            Response {
                status: 200,
                body,
                generation: Some(generation),
            }
        }
        Err(error) => {
            let gate = match &error {
                EvolutionError::Gate { gate, .. } => gate.name(),
                EvolutionError::Compile(_) => "compile",
                _ => "runtime",
            };
            let event = shared.record(ServeEventKind::Rejected, None, format!("{name}: {error}"));
            reply(
                409,
                json!({ "promoted": false, "gate": gate, "detail": event.detail }),
            )
        }
    }
}

fn rollback(shared: &Shared) -> Response {
    let mut admin = shared.admin.lock().unwrap_or_else(|p| p.into_inner());
    let Some(receipt) = admin.promotions.pop() else {
        return reply(409, json!({ "error": "no promotion to roll back" }));
    };
    match shared.coordinator.rollback_carrying_state(&receipt) {
        Ok((generation, handoff)) => {
            let mut event =
                EvolutionEvent::new(EvolutionEventKind::RolledBack, &receipt.candidate_hash);
            event.generation = Some(generation);
            event.policy_hash = Some(receipt.policy_hash.clone());
            event.decision = Some("operator rollback".into());
            event.evidence.insert(
                "rolled_back_generation".into(),
                receipt.swap.generation.to_string(),
            );
            event
                .evidence
                .insert("carried".into(), handoff.carried.join(","));
            if let Err(error) = admin.audit.append(event) {
                return reply(500, json!({ "error": format!("audit: {error}") }));
            }
            shared.record(
                ServeEventKind::RolledBack,
                Some(generation),
                format!(
                    "generation {} replaced by generation {generation}; carries {}",
                    receipt.swap.generation,
                    list(&handoff.carried)
                ),
            );
            Response {
                status: 200,
                body: json!({
                    "generation": generation,
                    "rolled_back": receipt.swap.generation,
                    "carried": handoff.carried,
                    "kept": handoff.initialised,
                }),
                generation: Some(generation),
            }
        }
        Err(error) => {
            admin.promotions.push(receipt);
            reply(500, json!({ "error": error.to_string() }))
        }
    }
}

fn list(names: &[String]) -> String {
    if names.is_empty() {
        "nothing".to_owned()
    } else {
        names.join(", ")
    }
}
