//! Conformance suite test runner: cross-feature adversarial interaction tests.
//!
//! Covers tests/conformance/c01 through c21 (with c05_mod_shapes.iris as helper module).
//! Each test must contain assert(...) assertions and exit 0 under `--emit eval`.

use std::path::{Path, PathBuf};
use std::process::Command;

const SUPPORT_MODULES: &[(&str, &str)] = &[(
    "c05_mod_shapes.iris",
    "helper module imported by c05_modules_generics.iris",
)];

fn conformance_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("conformance")
}

fn conformance_tests() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(conformance_dir())
        .expect("tests/conformance/ must be readable")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".iris"))
        .collect();
    v.sort();
    v
}

fn asserts(name: &str) -> bool {
    std::fs::read_to_string(conformance_dir().join(name))
        .unwrap_or_default()
        .contains("assert(")
}

fn run(name: &str) -> (Option<i32>, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_iris"));
    cmd.args(["--emit", "eval"])
        .arg(conformance_dir().join(name));
    let out = cmd.output().expect("failed to launch the iris binary");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.code(), text)
}

#[test]
fn every_conformance_test_asserts() {
    let support: Vec<&str> = SUPPORT_MODULES.iter().map(|(f, _)| *f).collect();
    let unasserted: Vec<String> = conformance_tests()
        .into_iter()
        .filter(|f| !support.contains(&f.as_str()) && !asserts(f))
        .collect();
    assert!(
        unasserted.is_empty(),
        "these conformance tests assert nothing:\n  {}",
        unasserted.join("\n  ")
    );
}

#[test]
fn the_support_module_list_is_accurate() {
    for (file, _) in SUPPORT_MODULES {
        let p = conformance_dir().join(file);
        assert!(
            p.exists(),
            "{} is listed as support but does not exist",
            file
        );
        let src = std::fs::read_to_string(p).unwrap_or_default();
        assert!(
            !src.contains("def main("),
            "{} now has an entry point and must leave SUPPORT_MODULES",
            file
        );
    }
}

#[test]
fn every_conformance_test_runs() {
    let support: Vec<&str> = SUPPORT_MODULES.iter().map(|(f, _)| *f).collect();
    let mut failures = Vec::new();
    let tests = conformance_tests();
    assert!(
        tests.len() >= 22,
        "expected at least 22 files in tests/conformance, found {}",
        tests.len()
    );

    for f in tests {
        if support.contains(&f.as_str()) {
            continue;
        }
        match run(&f) {
            (Some(0), _) => {}
            (Some(c), out) => failures.push(format!(
                "{} exited {}: {}",
                f,
                c,
                out.lines()
                    .find(|l| l.contains("error"))
                    .unwrap_or("")
                    .trim()
            )),
            (None, _) => failures.push(format!("{} was killed by a signal (crash)", f)),
        }
    }
    assert!(
        failures.is_empty(),
        "conformance files that should run but did not:\n  {}",
        failures.join("\n  ")
    );
}
