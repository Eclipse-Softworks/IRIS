//! Runtime errors read the same on both backends (tests/runtime_errors/*.iris).
//!
//! Each file states the error it must stop with:
//!
//! ```text
//! // expect: E0403 at 7:20     code and the position of the failing expression
//! // message: index 5 ...      a substring of the message
//! // output: before            a line the program printed first, which is kept
//! ```
//!
//! Both the native binary and the interpreter must exit with 101 and report
//! the same code, message and position, as a full diagnostic. Natively the
//! runtime reports `error[E0403]: ...` with a position, and the driver turns
//! that into the same diagnostic the interpreter renders.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Default, Debug)]
struct Expect {
    code: String,
    line: u32,
    col: u32,
    messages: Vec<String>,
    output: Vec<String>,
}

fn expectations(source: &str) -> Expect {
    let mut e = Expect::default();
    for line in source.lines() {
        let Some(rest) = line.strip_prefix("// ") else {
            break;
        };
        if let Some(v) = rest.strip_prefix("expect: ") {
            let (code, at) = v.split_once(" at ").expect("expect: CODE at L:C");
            let (l, c) = at.trim().split_once(':').expect("L:C");
            e.code = code.trim().to_string();
            e.line = l.parse().unwrap();
            e.col = c.parse().unwrap();
        } else if let Some(v) = rest.strip_prefix("message: ") {
            e.messages.push(v.trim().to_string());
        } else if let Some(v) = rest.strip_prefix("output: ") {
            e.output.push(v.trim().to_string());
        }
    }
    e
}

/// The first diagnostic's code, message and position, and the program's
/// stdout.
fn run(path: &Path, interpreted: bool) -> (Option<i32>, String, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_iris"));
    cmd.args(["--emit", "eval"]).arg(path);
    cmd.env_remove("IRIS_DEBUG");
    if interpreted {
        cmd.env("IRIS_FORCE_INTERP", "1");
    } else {
        cmd.env_remove("IRIS_FORCE_INTERP");
    }
    let out = cmd.output().expect("run iris");
    let stderr = String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n");
    let stdout = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    let header = stderr
        .lines()
        .find(|l| l.starts_with("error["))
        .unwrap_or("")
        .to_string();
    let location = stderr
        .lines()
        .find_map(|l| l.trim_start().strip_prefix("--> "))
        .unwrap_or("")
        .to_string();
    (
        out.status.code(),
        header,
        location,
        format!("{stdout}\n--- stderr ---\n{stderr}"),
    )
}

fn files() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir("tests/runtime_errors")
        .expect("tests/runtime_errors")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "iris"))
        .collect();
    v.sort();
    v
}

#[test]
fn runtime_errors_are_coded_located_and_the_same_on_both_backends() {
    let files = files();
    assert!(
        files.len() >= 10,
        "runtime error corpus: {} files",
        files.len()
    );
    let mut failures = Vec::new();
    for path in &files {
        let source = std::fs::read_to_string(path).unwrap();
        let e = expectations(&source);
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let mut reports = Vec::new();
        for interpreted in [false, true] {
            let backend = if interpreted { "interpreter" } else { "native" };
            let (code, header, location, text) = run(path, interpreted);
            let want_header = format!("error[{}]: ", e.code);
            let mut problems = Vec::new();
            if code != Some(101) {
                problems.push(format!("exit code {code:?}, want 101"));
            }
            if !header.starts_with(&want_header) {
                problems.push(format!("header `{header}`, want `{want_header}...`"));
            }
            for m in &e.messages {
                if !header.contains(m.as_str()) {
                    problems.push(format!("message lacks `{m}`"));
                }
            }
            if !location.ends_with(&format!(":{}:{}", e.line, e.col)) || !location.contains(&name) {
                problems.push(format!(
                    "location `{location}`, want {name}:{}:{}",
                    e.line, e.col
                ));
            }
            for line in &e.output {
                if !text
                    .split("--- stderr ---")
                    .next()
                    .unwrap_or("")
                    .contains(line.as_str())
                {
                    problems.push(format!("output lost `{line}`"));
                }
            }
            if !text.contains(" |") {
                problems.push("no source excerpt".into());
            }
            if !problems.is_empty() {
                failures.push(format!(
                    "{name} ({backend}): {}\n{text}",
                    problems.join("; ")
                ));
            }
            reports.push((header, location));
        }
        if reports[0] != reports[1] {
            failures.push(format!(
                "{name}: the backends differ: native {:?}, interpreter {:?}",
                reports[0], reports[1]
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} problem(s):\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}
