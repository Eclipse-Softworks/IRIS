//! `projects/cli_tools`: the command-line tools, run with real arguments on
//! both backends, against the answers `samples/make_samples.py` computed.

use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn tool(name: &str) -> PathBuf {
    root()
        .join("projects/cli_tools")
        .join(format!("{name}.iris"))
}

fn sample(name: &str) -> String {
    root()
        .join("projects/cli_tools/samples")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

fn expected() -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(sample("expected.json")).unwrap()).unwrap()
}

/// Build `name` natively once per test run.
fn native(name: &str, dir: &Path) -> PathBuf {
    let exe = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    let build = Command::new(env!("CARGO_BIN_EXE_iris"))
        .args(["--emit", "binary", "-o"])
        .arg(&exe)
        .arg(tool(name))
        .output()
        .expect("build");
    assert!(
        build.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    exe
}

/// Stdout of the tool under each backend; they must be identical.
fn run_both(name: &str, exe: &Path, args: &[&str]) -> String {
    let native = Command::new(exe)
        .current_dir(root())
        .args(args)
        .output()
        .expect("run native");
    let interpreted = Command::new(env!("CARGO_BIN_EXE_iris"))
        .current_dir(root())
        .args(["--emit", "eval"])
        .arg(tool(name))
        .arg("--")
        .args(args)
        .env("IRIS_FORCE_INTERP", "1")
        .output()
        .expect("run interpreter");
    let native_out = String::from_utf8_lossy(&native.stdout).replace("\r\n", "\n");
    let interp_out = String::from_utf8_lossy(&interpreted.stdout).replace("\r\n", "\n");
    assert!(
        native.status.success(),
        "{name} native: {native_out}{}",
        String::from_utf8_lossy(&native.stderr)
    );
    assert!(
        interpreted.status.success(),
        "{name} interpreter: {interp_out}{}",
        String::from_utf8_lossy(&interpreted.stderr)
    );
    // `--emit eval` ends with main's return value.
    let interp_out = interp_out
        .strip_suffix("0\n")
        .unwrap_or(&interp_out)
        .to_owned();
    assert_eq!(
        native_out, interp_out,
        "{name} {args:?}: the backends disagree"
    );
    native_out
}

#[test]
fn the_self_test_matches_the_reference_on_both_backends() {
    let dir = std::env::temp_dir().join(format!("iris_cli_selftest_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let exe = native("selftest", &dir);
    let out = run_both("selftest", &exe, &[]);
    assert!(
        out.contains(
            "cli tools: csv, json lines, logs, pcap and TLS fingerprints match the reference"
        ),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn each_tool_runs_with_arguments_on_both_backends() {
    let expected = expected();
    let dir = std::env::temp_dir().join(format!("iris_cli_tools_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    // csvq: grouping with exact sums, filtering, JSON output, JSON Lines.
    let csvq = native("csvq", &dir);
    let orders = sample("orders.csv");
    let grouped = run_both(
        "csvq",
        &csvq,
        &[&orders, "--group-by", "region", "--sum", "amount"],
    );
    let lines: Vec<&str> = grouped.lines().collect();
    let want: Vec<&str> = expected["orders"]["group_by_region_sum_amount"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(lines, want);
    let eu = run_both(
        "csvq",
        &csvq,
        &[
            &orders,
            "--where",
            "region=EU",
            "--select",
            "order_id,amount",
        ],
    );
    assert_eq!(
        eu.lines().count() as u64,
        1 + expected["orders"]["eu_rows"].as_u64().unwrap()
    );
    assert_eq!(
        eu.lines().nth(1).unwrap(),
        expected["orders"]["eu_first"].as_str().unwrap()
    );
    let json = run_both("csvq", &csvq, &[&orders, "--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("csvq --json is valid JSON");
    let rows = parsed.as_array().unwrap();
    assert_eq!(rows.len(), 40);
    assert_eq!(rows[1]["note"], expected["orders"]["quoted_note"]);
    assert_eq!(rows[2]["note"], "Leave at door\nside gate");
    let events = run_both(
        "csvq",
        &csvq,
        &[
            &sample("events.jsonl"),
            "--jsonl",
            "--select",
            "user,action,ms",
            "--where",
            "ms>=500",
        ],
    );
    assert_eq!(
        events.lines().count() as u64,
        1 + expected["events"]["slow"].as_u64().unwrap()
    );
    assert_eq!(
        events.lines().nth(1).unwrap(),
        expected["events"]["first_slow"].as_str().unwrap()
    );

    // logstat
    let logstat = native("logstat", &dir);
    let report = run_both("logstat", &logstat, &[&sample("access.log")]);
    let log = &expected["log"];
    assert!(
        report.contains(&format!(
            "lines {}, parsed {}, unparsed {}",
            log["lines"], log["parsed"], log["unparsed"]
        )),
        "{report}"
    );
    assert!(
        report.contains(&format!(
            "top path {} ({} requests)",
            log["top_path"].as_str().unwrap(),
            log["top_path_hits"]
        )),
        "{report}"
    );
    assert!(
        report.contains(&format!(
            "404 scanner {}",
            log["scanners"][0].as_str().unwrap()
        )),
        "{report}"
    );

    // pcapinspect
    let pcapinspect = native("pcapinspect", &dir);
    let capture = sample("capture.pcap");
    let report = run_both("pcapinspect", &pcapinspect, &[&capture]);
    let cap = &expected["capture"];
    assert!(
        report.contains(&format!(
            "packets {}, bytes {}",
            cap["packets"], cap["bytes"]
        )),
        "{report}"
    );
    assert!(
        report.contains(&format!("flows {}", cap["flows"])),
        "{report}"
    );
    assert!(
        report.contains(&format!("syn scan {}", cap["scans"][0].as_str().unwrap())),
        "{report}"
    );

    // tlsfp, with a list of known fingerprints: one JA3 and one JA4.
    let tlsfp = native("tlsfp", &dir);
    let hellos = expected["hellos"].as_array().unwrap();
    let known = dir.join("known.csv");
    std::fs::write(
        &known,
        format!(
            "fingerprint,label\n{},python openssl\n{},browser-like test client\n",
            hellos[0]["ja3_md5"].as_str().unwrap(),
            hellos[2]["ja4"].as_str().unwrap()
        ),
    )
    .unwrap();
    let known = known.to_string_lossy().into_owned();
    let report = run_both("tlsfp", &tlsfp, &[&capture, "--known", &known]);
    for hello in hellos {
        assert!(
            report.contains(&format!("  ja3 {}", hello["ja3_md5"].as_str().unwrap())),
            "{report}"
        );
        assert!(
            report.contains(&format!("  ja4 {}", hello["ja4"].as_str().unwrap())),
            "{report}"
        );
    }
    assert!(report.contains("known: python openssl"), "{report}");
    assert!(
        report.contains("known: browser-like test client"),
        "{report}"
    );
    assert!(report.contains("5 client hellos, 2 known"), "{report}");

    // Bad input is reported, not crashed on.
    let missing = Command::new(&pcapinspect)
        .arg(dir.join("nope.pcap"))
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stdout).contains("cannot read"));
    let not_pcap = Command::new(&pcapinspect).arg(&orders).output().unwrap();
    assert_eq!(not_pcap.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&not_pcap.stdout).contains("not a libpcap capture"));
    let _ = std::fs::remove_dir_all(dir);
}
