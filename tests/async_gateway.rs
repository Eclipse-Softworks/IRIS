//! `projects/async_gateway`: the gateway answers many concurrent HTTP
//! requests from one async thread, with deadlines and cancellation, on both
//! backends.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn project() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("projects/async_gateway")
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// One request; the status code and body.
fn get(port: u16, method: &str, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect to the gateway");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 0\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("read the response");
    let status = response
        .get(9..12)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned())
        .unwrap_or_default();
    (status, body)
}

/// The gateway process; killed if the test fails before shutting it down.
struct Server(Option<std::process::Child>);

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn wait_until_listening(port: u16, server: &mut std::process::Child) {
    for _ in 0..300 {
        if let Some(status) = server.try_wait().unwrap() {
            panic!("the gateway exited before listening: {status}");
        }
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("the gateway did not listen on {port}");
}

fn exercise(server: std::process::Child, port: u16, backend: &str) {
    let mut server = Server(Some(server));
    wait_until_listening(port, server.0.as_mut().unwrap());

    // 64 clients at once: quotes, races to the fastest supplier, and
    // requests whose upstream never answers.
    let started = Instant::now();
    let clients: Vec<_> = (0..64)
        .map(|i| {
            std::thread::spawn(move || {
                let path = match i % 8 {
                    0 => "/slow".to_owned(),
                    1 => format!("/fastest/part{i}"),
                    2 => format!("/quote/rare-{i}"),
                    _ => format!("/quote/part{}", i % 16),
                };
                let (status, body) = get(port, "GET", &path);
                (path, status, body)
            })
        })
        .collect();
    let replies: Vec<_> = clients
        .into_iter()
        .map(|client| client.join().unwrap())
        .collect();
    let elapsed = started.elapsed();
    // A race cancels the calls still running when the winner arrives: two,
    // unless a slow timer let another finish too.
    let mut race_cancelled = 0;
    for (path, status, body) in &replies {
        if path == "/slow" {
            assert_eq!(*status, 504, "{backend} {path}: {body}");
        } else {
            assert_eq!(*status, 200, "{backend} {path}: {body}");
        }
        if path.starts_with("/quote/rare-") {
            assert!(
                body.contains("\"answered\":2,\"cancelled\":1"),
                "{backend} {path}: {body}"
            );
        } else if path.starts_with("/quote/") {
            assert!(
                body.contains("\"answered\":3,\"cancelled\":0"),
                "{backend} {path}: {body}"
            );
        } else if path.starts_with("/fastest/") {
            let reply: serde_json::Value = serde_json::from_str(body).unwrap();
            let cancelled = reply["cancelled"].as_i64().unwrap();
            assert!((0..=2).contains(&cancelled), "{backend} {path}: {body}");
            race_cancelled += cancelled;
        }
    }

    let (status, stats) = get(port, "GET", "/stats");
    assert_eq!(status, 200);
    let stats: serde_json::Value = serde_json::from_str(&stats).unwrap();
    eprintln!(
        "{backend}: 64 requests in {} ms; {stats}",
        elapsed.as_millis()
    );
    assert_eq!(stats["served"], 64, "{stats}");
    assert_eq!(stats["timeouts"], 8, "{stats}");
    // 8 hung calls under /slow, 8 under rare quotes, and the race losers.
    assert_eq!(stats["cancelled_calls"], 8 + 8 + race_cancelled, "{stats}");
    // The server itself saw the requests overlap on its one async thread.
    assert!(stats["peak_in_flight"].as_i64().unwrap() >= 8, "{stats}");

    let (status, _) = get(port, "POST", "/shutdown");
    assert_eq!(status, 200);
    let output = server
        .0
        .take()
        .unwrap()
        .wait_with_output()
        .expect("gateway exit");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{backend}: {stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("async gateway: stopped after 65 requests"),
        "{backend}: {stdout}"
    );
}

#[test]
fn the_gateway_serves_concurrent_requests_with_deadlines_on_both_backends() {
    let main = project().join("main.iris");

    let port = free_port();
    let interpreted = Command::new(env!("CARGO_BIN_EXE_iris"))
        .args(["--emit", "eval"])
        .arg(&main)
        .env("IRIS_FORCE_INTERP", "1")
        .env("IRIS_GATEWAY_PORT", port.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the interpreted gateway");
    exercise(interpreted, port, "interpreter");

    let exe = std::env::temp_dir().join(format!(
        "iris_async_gateway_{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    let build = Command::new(env!("CARGO_BIN_EXE_iris"))
        .args(["--emit", "binary", "-o"])
        .arg(&exe)
        .arg(&main)
        .output()
        .expect("build the gateway");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let port = free_port();
    let native = Command::new(&exe)
        .env("IRIS_GATEWAY_PORT", port.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the native gateway");
    exercise(native, port, "native");
    let _ = std::fs::remove_file(exe);
}
