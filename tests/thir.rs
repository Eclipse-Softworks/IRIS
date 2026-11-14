//! The THIR stage between HIR and the CFG.
//!
//! `src/thir` owns a typed, desugared tree built from the HIR: values and calls
//! carry their resolutions, method calls carry their targets, field accesses
//! carry layout indices, map literals carry key/value types, and tuple
//! destructuring is a temporary plus one access per component. These tests
//! drive the real CLI:
//!
//! * `IRIS_THIR=check` fails a compile whose THIR is incomplete -- an
//!   expression the HIR walked but THIR did not rebuild, a lowering hint the
//!   HIR solved but THIR did not copy, an unresolved value, or a call without
//!   a target.
//! * `IRIS_THIR=report` prints the counts, which must add up.
//!
//! THIR is built on every compile and lowering consumes its type hints, field
//! indices, map types, and tuple-destructuring shape. These tests prove the
//! stage's coverage and shapes; the corpus tests prove lowering runs on it.

use std::process::Command;

/// Programs whose THIR must be complete. The list mirrors the HIR sample and
/// adds the shape fixture and the map-literal test.
const SAMPLE: &[&str] = &[
    "tests/test_mature_stdlibs.iris",
    "tests/language_core.iris",
    "tests/test_ais_reference.iris",
    "tests/test_slice_patterns.iris",
    "tests/test_generic_set.iris",
    "tests/test_quick_wins.iris",
    "tests/test_blanket_impls.iris",
    "tests/test_effects_handlers.iris",
    "tests/test_dict_literal.iris",
    "tests/fixtures/thir_shapes.iris",
];

fn run(args: &[&str], envs: &[(&str, &str)]) -> (bool, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_iris"));
    cmd.args(args);
    // The tests must not inherit a developer's audit modes.
    cmd.env_remove("IRIS_TYPECK");
    cmd.env_remove("IRIS_HIR");
    cmd.env_remove("IRIS_THIR");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run iris");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn run_full(args: &[&str]) -> (bool, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_iris"));
    cmd.args(args);
    cmd.env_remove("IRIS_TYPECK");
    cmd.env_remove("IRIS_HIR");
    cmd.env_remove("IRIS_THIR");
    let out = cmd.output().expect("run iris");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn every_sampled_program_has_a_complete_thir() {
    for path in SAMPLE {
        let (ok, stderr) = run(&["check", path], &[("IRIS_THIR", "check")]);
        assert!(ok, "{path} failed to check:\n{stderr}");
        assert!(
            !stderr.contains("E0902"),
            "{path} has an incomplete THIR:\n{stderr}"
        );
    }
}

#[test]
fn the_report_accounts_for_every_expression() {
    let path = "tests/test_mature_stdlibs.iris";
    let (ok, stderr) = run(&["check", path], &[("IRIS_THIR", "report")]);
    assert!(ok, "{path} failed to check:\n{stderr}");

    let line = stderr
        .lines()
        .find(|l| l.contains(" expressions ("))
        .unwrap_or_else(|| panic!("no THIR expression report in:\n{stderr}"));
    let (total, known, partial, open, source) = parse_expression_counts(line);
    assert!(
        total > 5_000,
        "only {total} THIR expressions built:\n{stderr}"
    );
    assert_eq!(
        known + partial + open,
        total,
        "type slots do not add up: {line}"
    );
    assert!(
        known > total / 2,
        "the checker should type most expressions: {line}"
    );
    assert!(source > 5_000, "the report lost source expressions: {line}");
    assert!(
        source <= total,
        "synthetic nodes must not reduce the source count: {line}"
    );

    let values = stderr
        .lines()
        .find(|l| l.contains(" values ("))
        .unwrap_or_else(|| panic!("no THIR value report in:\n{stderr}"));
    assert!(
        values.contains("0 unresolved"),
        "a valid program has unresolved THIR values: {values}"
    );

    let calls = stderr
        .lines()
        .find(|l| l.contains(" calls ("))
        .unwrap_or_else(|| panic!("no THIR call report in:\n{stderr}"));
    assert!(
        calls.contains("0 unresolved"),
        "a valid program has unresolved THIR calls: {calls}"
    );
    assert!(
        calls.contains("method targets"),
        "the call report must separate method targets: {calls}"
    );
}

#[test]
fn tuple_maps_and_fields_are_desugared() {
    let path = "tests/fixtures/thir_shapes.iris";
    let (ok, stderr) = run(&["check", path], &[("IRIS_THIR", "report")]);
    assert!(ok, "{path} failed to check:\n{stderr}");

    let line = stderr
        .lines()
        .find(|l| l.contains(" expressions ("))
        .unwrap_or_else(|| panic!("no THIR expression report in:\n{stderr}"));
    let (total, known, partial, open, source) = parse_expression_counts(line);
    assert_eq!(
        known + partial + open,
        total,
        "type slots do not add up: {line}"
    );
    assert!(
        total > source,
        "tuple destructuring should add synthetic nodes: {line}"
    );

    let shapes = stderr
        .lines()
        .find(|l| l.contains(" maps ("))
        .unwrap_or_else(|| panic!("no THIR shape report in:\n{stderr}"));
    assert!(
        shapes.contains("2 maps (2 key-typed, 2 value-typed)"),
        "a map literal lost its key/value types: {shapes}"
    );
    assert!(
        shapes.contains("2 destructures (2 tuple, 0 struct, 0 open)"),
        "the tuple destructuring was not desugared: {shapes}"
    );
    assert!(
        shapes.contains("2 field accesses (2 indexed)"),
        "record field indices were not resolved: {shapes}"
    );

    let calls = stderr
        .lines()
        .find(|l| l.contains(" calls ("))
        .unwrap_or_else(|| panic!("no THIR call report in:\n{stderr}"));
    assert!(
        calls.contains("4 method, 4 method targets"),
        "method calls did not resolve to targets: {calls}"
    );
}

#[test]
fn the_shape_fixture_still_runs() {
    let (ok, stdout, stderr) = run_full(&["--emit", "eval", "tests/fixtures/thir_shapes.iris"]);
    assert!(ok, "the THIR shape fixture failed to run:\n{stderr}");
    assert_eq!(stdout.trim(), "0", "unexpected fixture output:\n{stdout}");
}

#[test]
fn thir_check_does_not_mask_a_real_diagnostic() {
    let (ok, stderr) = run(
        &["check", "tests/diagnostics/annotation_mismatch.iris"],
        &[("IRIS_THIR", "check")],
    );
    assert!(!ok, "an invalid program must not check");
    assert!(
        !stderr.contains("E0902"),
        "THIR validation masked the real error:\n{stderr}"
    );
}

/// `thir m: 5748 expressions (5232 known, 74 partial, 442 open; 5696 source
/// spans), 1262 statements` -> `(5748, 5232, 74, 442, 5696)`.
fn parse_expression_counts(line: &str) -> (usize, usize, usize, usize, usize) {
    let (head, tail) = line
        .split_once(" expressions (")
        .unwrap_or_else(|| panic!("bad expression report: {line}"));
    let total = head
        .split_whitespace()
        .last()
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("bad expression total: {line}"));
    let inner = tail
        .split(')')
        .next()
        .unwrap_or_else(|| panic!("bad expression tail: {line}"));
    let mut parts = inner.split(';');
    let types = parts
        .next()
        .unwrap_or_else(|| panic!("bad type split: {line}"));
    let nums: Vec<usize> = types
        .split(',')
        .map(|part| {
            part.split_whitespace()
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or_else(|| panic!("bad count in: {line}"))
        })
        .collect();
    assert_eq!(nums.len(), 3, "expected known/partial/open: {line}");
    let source = parts
        .next()
        .unwrap_or_else(|| panic!("no source-span count: {line}"))
        .split_whitespace()
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("bad source-span count: {line}"));
    (total, nums[0], nums[1], nums[2], source)
}
