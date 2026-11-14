//! `iris evolve-serve`: the inventory service in
//! `projects/self_upgrading_service` is upgraded while it serves traffic.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use iris::codegen::llvm_orc::is_orc_jit_available;
use iris::evolution::audit::{read_verified_records, EvolutionAuditLog, EvolutionEventKind};
use iris::evolution::program::Scenario;
use iris::evolution::serve::{serve, ServeConfig, ServeEventKind};
use iris::evolution::{compile_candidate_file, TrustedConstitution};

fn project() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("projects/self_upgrading_service")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("iris_evolve_serve_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// One HTTP/1.1 exchange: status, `X-Iris-Generation` (if any), body.
fn http(address: SocketAddr, method: &str, path: &str, body: &str) -> (u16, Option<u64>, String) {
    let mut stream = TcpStream::connect(address).expect("connect");
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    let (head, body) = response.split_once("\r\n\r\n").expect("complete response");
    let status = head[9..12].parse().expect("status code");
    let generation = head
        .lines()
        .find_map(|line| line.strip_prefix("X-Iris-Generation: "))
        .map(|value| value.trim().parse().unwrap());
    (status, generation, body.to_owned())
}

fn json(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|error| panic!("{error}: {text}"))
}

#[test]
fn an_upgrade_under_concurrent_traffic_loses_no_update() {
    if !is_orc_jit_available() {
        eprintln!("LLVM ORC is unavailable; evolve-serve test skipped");
        return;
    }
    let dir = scratch("concurrent");
    let project = project();
    let config = ServeConfig {
        manifest: serde_json::from_str(
            &std::fs::read_to_string(project.join("program.json")).unwrap(),
        )
        .unwrap(),
        baseline: compile_candidate_file(&project.join("v1.iris")).unwrap(),
        scenarios: serde_json::from_str::<Vec<Scenario>>(
            &std::fs::read_to_string(project.join("scenarios.json")).unwrap(),
        )
        .unwrap(),
        constitution: TrustedConstitution::pinned(
            std::fs::read(project.join("constitution.txt")).unwrap(),
        ),
        min_output: 0.0,
        max_output: 1_000_000.0,
        tests: Vec::new(),
        port: 0,
        max_body_bytes: 1 << 20,
    };
    let audit = EvolutionAuditLog::open(dir.join("audit.jsonl")).unwrap();
    let (ready, address) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        serve(
            config,
            audit,
            move |address| ready.send(address).unwrap(),
            |_| {},
        )
    });
    let address = address.recv().unwrap();

    let (status, _, body) = http(
        address,
        "POST",
        "/call/restock",
        r#"{"sku":"part","qty":100000}"#,
    );
    assert_eq!(status, 200, "{body}");

    // Eight clients reserve one unit at a time until each has been served
    // by the upgraded generation a few times.
    let upgraded = Arc::new(AtomicBool::new(false));
    let clients: Vec<_> = (0..8)
        .map(|_| {
            let upgraded = Arc::clone(&upgraded);
            std::thread::spawn(move || {
                let (mut calls, mut on_new) = (0u64, 0u64);
                while on_new < 5 && calls < 5_000 {
                    let (status, generation, body) = http(
                        address,
                        "POST",
                        "/call/reserve",
                        r#"{"sku":"part","qty":1}"#,
                    );
                    assert_eq!(status, 200, "{body}");
                    assert_eq!(json(&body)["ok"], true, "{body}");
                    calls += 1;
                    if upgraded.load(Ordering::SeqCst) && generation != Some(1) {
                        on_new += 1;
                    }
                }
                calls
            })
        })
        .collect();
    std::thread::sleep(std::time::Duration::from_millis(100));
    let v2 = std::fs::read_to_string(project.join("v2.iris")).unwrap();
    let (status, generation, body) = http(address, "POST", "/upgrade", &v2);
    assert_eq!(status, 200, "{body}");
    assert_eq!(generation, Some(2));
    upgraded.store(true, Ordering::SeqCst);
    let reservations: u64 = clients
        .into_iter()
        .map(|client| client.join().unwrap())
        .sum();

    // Every reservation, on either side of the switch, is in the state.
    let (_, _, stats) = http(address, "POST", "/call/stats", "{}");
    let stats = json(&stats);
    assert_eq!(
        stats["served"],
        serde_json::json!(1 + reservations),
        "{stats}"
    );
    assert_eq!(
        stats["reserved"],
        serde_json::json!(reservations),
        "{stats}"
    );
    assert_eq!(
        stats["units"],
        serde_json::json!(100_000 - reservations),
        "{stats}"
    );
    assert_eq!(stats["refused"], 0, "{stats}");

    let (status, _, _) = http(address, "POST", "/shutdown", "");
    assert_eq!(status, 200);
    let report = server.join().unwrap().expect("serve");
    assert_eq!(report.generation, 2);
    assert!(
        report.served_by.get(&1).copied().unwrap_or(0) > 1,
        "{:?}",
        report.served_by
    );
    assert!(
        report.served_by.get(&2).copied().unwrap_or(0) >= 40,
        "{:?}",
        report.served_by
    );
    assert_eq!(report.requests, 2 + reservations);
    assert_eq!(report.state["served"], serde_json::json!(1 + reservations));
    let kinds: Vec<ServeEventKind> = report.events.iter().map(|event| event.kind).collect();
    assert_eq!(
        kinds,
        vec![ServeEventKind::Installed, ServeEventKind::Promoted]
    );
    let records = read_verified_records(&dir.join("audit.jsonl")).unwrap();
    assert_eq!(
        records.last().unwrap().event.kind,
        EvolutionEventKind::Promoted
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// Start `iris evolve-serve` on a free port; the child and its port.
fn start_server(dir: &Path) -> (std::process::Child, String) {
    let project = project();
    let constitution = project.join("constitution.txt");
    let hash = iris::evolution::sha256_hex(&std::fs::read(&constitution).unwrap());
    let mut child = Command::new(env!("CARGO_BIN_EXE_iris"))
        .arg("evolve-serve")
        .arg("--manifest")
        .arg(project.join("program.json"))
        .arg("--baseline")
        .arg(project.join("v1.iris"))
        .arg("--scenarios")
        .arg(project.join("scenarios.json"))
        .arg("--constitution")
        .arg(&constitution)
        .args(["--constitution-sha256", &hash])
        .arg("--audit")
        .arg(dir.join("audit.jsonl"))
        .arg("--audit-head")
        .arg(dir.join("audit.head.json"))
        .args(["--min-output=0", "--max-output", "1000000", "--port", "0"])
        .arg("--port-file")
        .arg(dir.join("port"))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("start iris evolve-serve");
    for _ in 0..600 {
        if let Ok(port) = std::fs::read_to_string(dir.join("port")) {
            if !port.is_empty() {
                return (child, port);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("iris evolve-serve did not start");
}

#[test]
fn the_iris_client_upgrades_and_rolls_back_the_service_on_both_backends() {
    if !is_orc_jit_available() {
        eprintln!("LLVM ORC is unavailable; evolve-serve test skipped");
        return;
    }
    let root = env!("CARGO_MANIFEST_DIR");
    let client = project().join("client.iris");
    let dir = scratch("client");
    let native = dir.join(format!("client{}", std::env::consts::EXE_SUFFIX));
    let build = Command::new(env!("CARGO_BIN_EXE_iris"))
        .args(["--emit", "binary", "-o"])
        .arg(&native)
        .arg(&client)
        .output()
        .expect("build the client");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );

    for backend in ["interpreter", "native"] {
        let _ = std::fs::remove_file(dir.join("port"));
        let _ = std::fs::remove_file(dir.join("audit.jsonl"));
        let _ = std::fs::remove_file(dir.join("audit.head.json"));
        let (server, port) = start_server(&dir);
        let run = if backend == "native" {
            Command::new(&native)
                .current_dir(root)
                .env("IRIS_SERVICE_PORT", &port)
                .output()
        } else {
            Command::new(env!("CARGO_BIN_EXE_iris"))
                .current_dir(root)
                .args(["--emit", "eval"])
                .arg(&client)
                .env("IRIS_FORCE_INTERP", "1")
                .env("IRIS_SERVICE_PORT", &port)
                .output()
        }
        .expect("run the client");
        let served = server.wait_with_output().expect("server exit");
        let log = String::from_utf8_lossy(&served.stdout).into_owned()
            + &String::from_utf8_lossy(&served.stderr);
        let output = String::from_utf8_lossy(&run.stdout).into_owned()
            + &String::from_utf8_lossy(&run.stderr);
        assert!(
            run.status.success(),
            "{backend} client failed:\n{output}\nserver:\n{log}"
        );
        assert!(
            output
                .contains("self-upgrading service: upgraded, refused, rolled back, upgraded again"),
            "{backend}:\n{output}"
        );
        assert!(served.status.success(), "{log}");
        for needle in [
            "rejected     upgrade_1: evolution gate 'language_safety' failed",
            "rejected     upgrade_2: evolution gate 'resource_and_abi' failed",
            "promoted     upgrade_3: generation 2 after 7/7 gates; carries skus, stock, served, reserved, initialises refused",
            "rolled back  generation 2 replaced by generation 1",
            "promoted     upgrade_4: generation 3 after 7/7 gates",
            "22 requests (generation 1: 12, generation 2: 6, generation 3: 4)",
            "\"served\":9",
            "audit chain verified",
        ] {
            assert!(log.contains(needle), "{backend}: missing {needle:?} in\n{log}");
        }
        let records = read_verified_records(&dir.join("audit.jsonl")).unwrap();
        assert!(records
            .iter()
            .any(|record| record.event.kind == EvolutionEventKind::RolledBack));
    }
    let _ = std::fs::remove_dir_all(dir);
}
