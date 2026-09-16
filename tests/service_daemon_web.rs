use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, RwLock};

use iris::evolution::daemon::{
    render_dashboard_html, render_prometheus_metrics, run_service_daemon,
    start_embedded_http_server, DaemonState, TelemetryTick,
};

fn http_get(port: u16, path: &str) -> String {
    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port))
        .unwrap_or_else(|e| panic!("failed to connect to 127.0.0.1:{}: {}", port, e));
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
        path, port
    );
    stream
        .write_all(request.as_bytes())
        .expect("write HTTP request");

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("read HTTP response");
    response
}

#[test]
fn test_embedded_http_server_endpoints() {
    let state = Arc::new(RwLock::new(DaemonState {
        current_tick: 42,
        total_ticks: 100,
        regime: "Nominal Traffic".to_string(),
        active_policy_id: 0,
        total_requests: 12500,
        failed_requests: 3,
        queue_depth: 2,
        p99_latency_ms: 4.85,
        target_p99_ms: 15.0,
        error_budget_ratio: 0.01,
        allostatic_strain: 0.035,
        cache_hit_ratio: 0.985,
        generation_id: 4,
        hot_swaps: 3,
        is_running: true,
        recent_ticks: vec![TelemetryTick {
            tick: 42,
            regime: "Nominal Traffic".to_string(),
            active_policy_id: 0,
            total_requests: 12500,
            failed_requests: 3,
            queue_depth: 2,
            p99_latency_ms: 4.85,
            allostatic_strain: 0.035,
            cache_hit_ratio: 0.985,
            generation_id: 4,
            hot_swaps: 3,
        }],
    }));

    // Bind to port 0 (OS allocates free ephemeral port)
    let (bound_port, _stop_tx) =
        start_embedded_http_server(0, Arc::clone(&state)).expect("start embedded HTTP server");
    assert!(bound_port > 0, "bound port must be non-zero");

    // 1. Test /metrics (Prometheus)
    let metrics_resp = http_get(bound_port, "/metrics");
    assert!(metrics_resp.contains("HTTP/1.1 200 OK"));
    assert!(metrics_resp.contains("Content-Type: text/plain"));
    assert!(metrics_resp.contains("iris_requests_total 12500"));
    assert!(metrics_resp.contains("iris_p99_latency_ms 4.85"));
    assert!(metrics_resp.contains("iris_allostatic_strain 0.0350"));
    assert!(metrics_resp.contains("iris_service_generation 4"));
    assert!(metrics_resp.contains("iris_hot_swaps_total 3"));

    // 2. Test /status (JSON API)
    let status_resp = http_get(bound_port, "/status");
    assert!(status_resp.contains("HTTP/1.1 200 OK"));
    assert!(status_resp.contains("Content-Type: application/json"));
    assert!(status_resp.contains(r#""current_tick": 42"#));
    assert!(status_resp.contains(r#""regime": "Nominal Traffic""#));
    assert!(status_resp.contains(r#""generation_id": 4"#));
    assert!(status_resp.contains(r#""is_running": true"#));

    // 3. Test / (Live Dashboard HTML)
    let dash_resp = http_get(bound_port, "/");
    assert!(dash_resp.contains("HTTP/1.1 200 OK"));
    assert!(dash_resp.contains("Content-Type: text/html"));
    assert!(dash_resp.contains("IRIS"));
    assert!(dash_resp.contains("AUTONOMIC MICROSERVICE"));
    assert!(dash_resp.contains("MAPE-K"));
    assert!(dash_resp.contains("<svg"));

    // 4. Test 404 on unknown endpoint
    let not_found_resp = http_get(bound_port, "/nonexistent");
    assert!(not_found_resp.contains("HTTP/1.1 404 Not Found"));
}

#[test]
fn test_render_prometheus_metrics_unit() {
    let state = DaemonState {
        current_tick: 10,
        total_ticks: 20,
        regime: "Test".to_string(),
        active_policy_id: 1,
        total_requests: 1000,
        failed_requests: 0,
        queue_depth: 0,
        p99_latency_ms: 2.5,
        target_p99_ms: 10.0,
        error_budget_ratio: 0.02,
        allostatic_strain: 0.01,
        cache_hit_ratio: 0.99,
        generation_id: 2,
        hot_swaps: 1,
        is_running: false,
        recent_ticks: vec![],
    };

    let text = render_prometheus_metrics(&state);
    assert!(text.contains("iris_requests_total 1000"));
    assert!(text.contains("iris_p99_latency_ms 2.50"));
    assert!(text.contains("iris_target_p99_ms 10.00"));
}

#[test]
fn test_render_dashboard_html_unit() {
    let state = DaemonState {
        current_tick: 5,
        total_ticks: 10,
        regime: "Nominal Traffic".to_string(),
        active_policy_id: 0,
        total_requests: 500,
        failed_requests: 0,
        queue_depth: 1,
        p99_latency_ms: 3.1,
        target_p99_ms: 15.0,
        error_budget_ratio: 0.01,
        allostatic_strain: 0.02,
        cache_hit_ratio: 0.98,
        generation_id: 1,
        hot_swaps: 0,
        is_running: true,
        recent_ticks: vec![],
    };

    let html = render_dashboard_html(&state);
    assert!(html.contains("LIVE MAPE-K"));
    assert!(html.contains("3.10 ms"));
    assert!(html.contains("Gen 1"));
}

#[test]
fn test_run_service_daemon_with_serve() {
    // Run a short 5-tick headless daemon simulation with serve = true on port 0
    let res = run_service_daemon(5, 15.0, 0.01, true, None, true, 0);
    assert!(
        res.is_ok(),
        "daemon execution with serve enabled must succeed"
    );
}
