//! The service daemon against a real service: it proxies a local HTTP server,
//! notices from the traffic itself when the server starts failing, switches
//! to serving cached responses (clients keep getting answers), and goes back
//! to forwarding once the server recovers. The audit ledger and the metrics
//! endpoint report what happened.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iris::evolution::{run_service_daemon, DaemonConfig};

/// A service answering `hello` -- or, while `failing` is set, a 500 after a
/// short delay.
fn start_service(failing: Arc<AtomicU8>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let failing = Arc::clone(&failing);
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let mut got = Vec::new();
                while !got.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => got.extend_from_slice(&buf[..n]),
                    }
                }
                let reply: &[u8] = if failing.load(Ordering::Relaxed) == 1 {
                    std::thread::sleep(Duration::from_millis(30));
                    b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 4\r\nConnection: close\r\n\r\nboom"
                } else {
                    b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello"
                };
                let _ = stream.write_all(reply);
            });
        }
    });
    port
}

/// GET through a port: (status, whole response).
fn get(port: u16, path: &str) -> (u16, String) {
    let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)) else {
        return (0, String::new());
    };
    s.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let _ = write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    );
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    let status = out
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    (status, out)
}

/// GET until `want` holds for a response, for at most `secs`.
fn until(port: u16, secs: u64, want: impl Fn(u16, &str) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        let (code, text) = get(port, "/data");
        if want(code, &text) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    false
}

#[test]
fn the_daemon_rides_out_a_failing_service_and_recovers() {
    let failing = Arc::new(AtomicU8::new(0));
    let service = start_service(Arc::clone(&failing));
    let ledger_path =
        std::env::temp_dir().join(format!("iris_daemon_ledger_{}.json", std::process::id()));
    let (tx, rx) = std::sync::mpsc::channel();
    let config = DaemonConfig {
        upstream: format!("http://127.0.0.1:{service}"),
        listen: 0,
        ticks: 40,
        tick_ms: 200,
        target_p99: 25.0,
        error_budget: 0.05,
        max_inflight: 32,
        headless: true,
        audit: Some(ledger_path.clone()),
        serve: Some(0),
        ready: Some(tx),
        tls: None,
    };
    let daemon = std::thread::spawn(move || run_service_daemon(config));
    let (proxy, dashboard) = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the daemon starts");
    let dashboard = dashboard.expect("serving the dashboard");

    // Healthy: answers come from the service.
    assert!(until(proxy, 3, |c, t| c == 200
        && t.ends_with("hello")
        && !t.contains("X-Iris-Cache")));
    for _ in 0..20 {
        assert_eq!(get(proxy, "/data").0, 200);
        std::thread::sleep(Duration::from_millis(10));
    }

    // The service fails: the daemon must notice and serve from its cache.
    failing.store(1, Ordering::Relaxed);
    assert!(
        until(proxy, 4, |c, t| c == 200
            && t.contains("X-Iris-Cache: hit")
            && t.ends_with("hello")),
        "the daemon never served from cache while the service failed"
    );

    // Metrics while running.
    let (code, metrics) = get(dashboard, "/metrics");
    assert_eq!(code, 200);
    assert!(metrics.contains("iris_active_policy_id 2"), "{metrics}");

    // The service recovers: back to forwarding.
    failing.store(0, Ordering::Relaxed);
    assert!(
        until(proxy, 5, |c, t| c == 200
            && !t.contains("X-Iris-Cache")
            && t.ends_with("hello")),
        "the daemon never went back to forwarding"
    );

    let ledger = daemon.join().unwrap().expect("the daemon ran");
    assert_eq!(ledger.total_ticks, 40);
    assert!(
        ledger.total_hot_swaps >= 2,
        "{} policy changes",
        ledger.total_hot_swaps
    );
    assert!(ledger.ticks.iter().any(|t| t.regime == "Upstream Brownout"));
    assert!(ledger.ticks.iter().any(|t| t.active_policy_id == 2));
    assert!(ledger.ticks.last().unwrap().failed_requests > 0);
    let written = std::fs::read_to_string(&ledger_path).expect("the ledger was written");
    assert!(written.contains("\"total_hot_swaps\""));
    let _ = std::fs::remove_file(&ledger_path);
}
