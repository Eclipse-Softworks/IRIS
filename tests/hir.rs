//! The HIR stage between the syntax tree and the CFG.
//!
//! `src/hir` builds a name-resolved module and registers every expression
//! before the source checker runs on it; the checker's report is absorbed
//! into the HIR, and lowering reads types and solved `-> Infer` returns only
//! from it (`lower::typeck::hint`/`solved_return` are HIR lookups). These
//! tests drive the real CLI:
//!
//! * `IRIS_HIR=check` fails a compile whose HIR is incomplete -- an
//!   expression the walk missed, a value name that resolved to nothing, or a
//!   call to an unknown target.
//! * `IRIS_HIR=report` prints the counts, which must add up: every
//!   expression has a type slot, and no valid program has unresolved names.
//!
//! The corpus sample covers functions, impls and methods, effects and
//! handlers, slice/pattern bindings, brought module qualifiers, generics,
//! and the merged stdlib.

use std::process::Command;

/// Programs whose HIR must be complete. Each was chosen for a different
/// shape: the merged stdlib, core language forms, AIS (heavy built-in use),
/// slice bindings, qualified brought calls, generics, blanket impls, and
/// algebraic effects.
const SAMPLE: &[&str] = &[
    "tests/test_mature_stdlibs.iris",
    "tests/language_core.iris",
    "tests/test_ais_reference.iris",
    "tests/test_slice_patterns.iris",
    "tests/test_generic_set.iris",
    "tests/test_quick_wins.iris",
    "tests/test_blanket_impls.iris",
    "tests/test_effects_handlers.iris",
];

fn run(args: &[&str], envs: &[(&str, &str)]) -> (bool, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_iris"));
    cmd.args(args);
    // The tests must not inherit a developer's checker/HIR mode.
    cmd.env_remove("IRIS_TYPECK");
    cmd.env_remove("IRIS_HIR");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run iris");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn every_sampled_program_has_a_complete_hir() {
    for path in SAMPLE {
        let (ok, stderr) = run(&["check", path], &[("IRIS_HIR", "check")]);
        assert!(ok, "{path} failed to check:\n{stderr}");
        assert!(
            !stderr.contains("E0901"),
            "{path} has an incomplete HIR:\n{stderr}"
        );
    }
}

#[test]
fn the_report_accounts_for_every_expression() {
    let path = "tests/test_mature_stdlibs.iris";
    let (ok, stderr) = run(&["check", path], &[("IRIS_HIR", "report")]);
    assert!(ok, "{path} failed to check:\n{stderr}");

    let line = stderr
        .lines()
        .find(|l| l.contains(" expressions ("))
        .unwrap_or_else(|| panic!("no HIR expression report in:\n{stderr}"));
    let (total, known, partial, open) = parse_expression_counts(line);
    assert!(total > 1_000, "only {total} expressions walked:\n{stderr}");
    assert_eq!(
        known + partial + open,
        total,
        "type slots do not add up: {line}"
    );
    assert!(
        known > total / 2,
        "the checker should type most expressions: {line}"
    );

    let names = stderr
        .lines()
        .find(|l| l.contains(" names ("))
        .unwrap_or_else(|| panic!("no HIR name report in:\n{stderr}"));
    assert!(
        names.ends_with("0 unresolved"),
        "a valid program has unresolved value names: {names}"
    );

    let calls = stderr
        .lines()
        .find(|l| l.contains(" calls ("))
        .unwrap_or_else(|| panic!("no HIR call report in:\n{stderr}"));
    assert!(
        calls.ends_with("0 unresolved"),
        "a valid program has unresolved calls: {calls}"
    );
}

#[test]
fn hir_check_does_not_mask_a_real_diagnostic() {
    // A program the checker must reject: its own error is reported, not an
    // internal incomplete-HIR error.
    let (ok, stderr) = run(
        &["check", "tests/diagnostics/annotation_mismatch.iris"],
        &[("IRIS_HIR", "check")],
    );
    assert!(!ok, "an invalid program must not check");
    assert!(
        !stderr.contains("E0901"),
        "HIR validation masked the real error:\n{stderr}"
    );
}

/// `hir m: 5696 expressions (5180 known, 74 partial, 442 open)` ->
/// `(5696, 5180, 74, 442)`.
fn parse_expression_counts(line: &str) -> (usize, usize, usize, usize) {
    let (head, tail) = line
        .split_once(" expressions (")
        .unwrap_or_else(|| panic!("bad expression report: {line}"));
    let total = head
        .split_whitespace()
        .last()
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("bad expression total: {line}"));
    let inner = tail.trim_end_matches(')');
    let nums: Vec<usize> = inner
        .split(',')
        .map(|part| {
            part.split_whitespace()
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or_else(|| panic!("bad count in: {line}"))
        })
        .collect();
    assert_eq!(nums.len(), 3, "expected known/partial/open: {line}");
    (total, nums[0], nums[1], nums[2])
}
