//! The `.iris` corpus: executed, and gated on asserting its results.
//!
//! Two release-gate problems are addressed here.
//!
//! **Nothing globbed `tests/*.iris`.** The corpus was never executed by
//! `cargo test`, so a file could rot indefinitely with no run noticing.
//! Sweeping it by hand with `IRIS_FORCE_INTERP=1` is not equivalent: that
//! bypasses codegen, and the first native sweep found three crashes the
//! interpreter sweep could not see (`test_methods`, `test_move`,
//! `test_pattern_guards`), plus a file that passes natively while failing
//! interpreted. These tests drive the real CLI, so codegen is exercised.
//!
//! **Most files assert nothing.** 0 of 155 print results without
//! checking them, so they pass whenever the program compiles and exits 0,
//! regardless of whether the output is right (known-issues #4). Converting them
//! is mechanical but has to be done by *running* each file and reading its real
//! values, so it happens in batches. `NEEDS_ASSERTIONS` is the shrinking record
//! of what is left, and `the_needs_assertions_list_is_accurate` fails once a
//! listed file gains assertions, so the list cannot quietly drift.

use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

static FFI_FIXTURE_DIR: OnceLock<Result<std::path::PathBuf, String>> = OnceLock::new();

/// Files the compiler is *supposed* to reject. Here, passing is the failure.
const MUST_FAIL: &[(&str, &str)] = &[
    (
        "test_borrow_error.iris",
        "the borrow checker must reject it",
    ),
    (
        "test_exhaustiveness.iris",
        "a non-exhaustive match must be rejected",
    ),
    (
        "test_exhaustiveness_simple.iris",
        "a non-exhaustive match must be rejected",
    ),
    (
        "test_move_borrow_error.iris",
        "borrow of a moved value must be rejected",
    ),
    ("test_move_error.iris", "use after move must be rejected"),
];

/// Import-only fixtures used by standalone corpus tests. These are valid IRIS
/// modules, but intentionally have no entry point and are not tests themselves.
const SUPPORT_MODULES: &[(&str, &str)] = &[
    (
        "issue9_mid.iris",
        "re-export layer imported by test_issue9_pub_bring_types.iris",
    ),
    (
        "issue9_types.iris",
        "type-definition module imported through issue9_mid.iris",
    ),
];

/// Files that do not currently run, each with why. A debt register, not a
/// permission slip: every entry names a cause, and the list should only shrink.
const KNOWN_BROKEN: &[(&str, &str)] = &[(
    "test_doc_comments.iris",
    "no zero-argument function, so there is nothing to evaluate",
)];

/// Files that do not yet assert their results. Shrinking; see #4.
const NEEDS_ASSERTIONS: &[&str] = &[];

/// Files whose two backends disagree, each with why. The gate below asserts
/// that *everything else* agrees, so a new divergence fails rather than joining
/// this list silently.
const KNOWN_DIVERGENT: &[(&str, &str)] = &[
    ("test_adaptive.iris",
     "std.adaptive is native-only; the interpreter refuses its externs by design rather than returning a zero -- not a defect, and #34 is fixed"),
    ("test_quick_wins.iris",
     "produces no output under the interpreter"),
];

fn corpus() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir("tests")
        .expect("tests/ must be readable")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".iris"))
        .collect();
    v.sort();
    v
}

fn asserts(name: &str) -> bool {
    std::fs::read_to_string(Path::new("tests").join(name))
        .unwrap_or_default()
        .contains("assert(")
}

/// Runs a corpus file through the real CLI. The exit code is `None` when the
/// process was killed by a signal, which is how a crash shows up.
fn run(name: &str) -> (Option<i32>, String) {
    run_with(name, false)
}

/// Runs a corpus file, optionally forcing the interpreter.
fn run_with(name: &str, force_interp: bool) -> (Option<i32>, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_iris"));
    cmd.args(["--emit", "eval"]).arg(corpus_path(name));
    configure_fixture_environment(&mut cmd, name);
    if force_interp {
        cmd.env("IRIS_FORCE_INTERP", "1");
    }
    let out = cmd.output().expect("failed to launch the iris binary");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.code(), text)
}

/// The program exit status and stdout, with compiler diagnostics excluded.
///
/// The divergence check must compare what the *program* printed. The native
/// path legitimately writes build notices to stderr that the interpreter never
/// emits, so comparing combined output reports every file as divergent.
fn outcome_of(name: &str, force_interp: bool) -> (Option<i32>, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_iris"));
    cmd.args(["--emit", "eval"]).arg(corpus_path(name));
    configure_fixture_environment(&mut cmd, name);
    if force_interp {
        cmd.env("IRIS_FORCE_INTERP", "1");
    }
    let out = cmd.output().expect("failed to launch the iris binary");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn corpus_path(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(name)
}

fn configure_fixture_environment(command: &mut Command, name: &str) {
    if name != "test_ffi_full.iris" {
        return;
    }
    let fixture_dir = FFI_FIXTURE_DIR
        .get_or_init(build_ffi_fixture)
        .as_ref()
        .unwrap_or_else(|error| panic!("could not build the FFI corpus fixture: {error}"));
    command.current_dir(fixture_dir);
}

/// Build the C fixture that `test_ffi_full.iris` dynamically loads.
///
/// The old corpus gate accidentally depended on an untracked DLL in one
/// developer checkout. Producing the fixture in an isolated process directory
/// makes the native/interpreter agreement test portable across CI hosts.
fn build_ffi_fixture() -> Result<std::path::PathBuf, String> {
    let fixture_dir = std::env::temp_dir().join(format!("iris_ffi_fixture_{}", std::process::id()));
    std::fs::create_dir_all(&fixture_dir)
        .map_err(|error| format!("create {}: {error}", fixture_dir.display()))?;
    let output_path = fixture_dir.join("iris_ffitest.dll");
    let source_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("ffitest.c");
    let compiler = ffi_fixture_compiler();
    let mut command = Command::new(&compiler);
    if cfg!(target_os = "macos") {
        command.arg("-dynamiclib");
    } else {
        command.arg("-shared");
    }
    if cfg!(unix) {
        command.arg("-fPIC");
    }
    let result = command
        .arg(&source_path)
        .arg("-o")
        .arg(&output_path)
        .output()
        .map_err(|error| format!("start '{compiler}': {error}"))?;
    if !result.status.success() {
        return Err(format!(
            "'{compiler}' exited {}: {}",
            result.status,
            String::from_utf8_lossy(&result.stderr).trim()
        ));
    }
    Ok(fixture_dir)
}

fn ffi_fixture_compiler() -> String {
    if let Ok(compiler) = std::env::var("IRIS_FFI_CC") {
        if !compiler.is_empty() {
            return compiler;
        }
    }
    if cfg!(target_os = "windows") {
        // The CI image's clang defaults to the MSVC driver and cannot always
        // find a usable link.exe. CI installs this UCRT toolchain for IRIS's
        // direct MinGW gate, so use its compiler for the portable fixture too.
        let mingw = r"C:\msys64\ucrt64\bin\gcc.exe";
        if Path::new(mingw).is_file() {
            return mingw.to_owned();
        }
    }
    std::env::var("CC")
        .or_else(|_| std::env::var("IRIS_CLANG"))
        .unwrap_or_else(|_| "clang".to_owned())
}

// -- The assertion gate (#4) ----------------------------------------------

/// Every corpus file must assert its results, or be listed as not yet doing so.
/// A *new* file that asserts nothing fails here instead of joining the backlog
/// unnoticed.
#[test]
fn every_iris_test_asserts_or_is_listed() {
    // A file that never runs cannot assert anything, so negative tests and
    // known breakages are exempt -- they are tracked by their own lists, each
    // with its own accuracy check.
    let never_runs: Vec<&str> = MUST_FAIL
        .iter()
        .map(|(f, _)| *f)
        .chain(KNOWN_BROKEN.iter().map(|(f, _)| *f))
        .chain(SUPPORT_MODULES.iter().map(|(f, _)| *f))
        .collect();
    let unlisted: Vec<String> = corpus()
        .into_iter()
        .filter(|f| {
            !asserts(f)
                && !NEEDS_ASSERTIONS.contains(&f.as_str())
                && !never_runs.contains(&f.as_str())
        })
        .collect();
    assert!(
        unlisted.is_empty(),
        "these .iris tests assert nothing and are not on NEEDS_ASSERTIONS. A test \
         that only prints cannot fail. Add assert(...) to each:\n  {}",
        unlisted.join("\n  ")
    );
}

/// The backlog must stay honest: a file that has gained assertions comes off the
/// list, or the remaining count stops meaning anything.
#[test]
fn the_needs_assertions_list_is_accurate() {
    let stale: Vec<&str> = NEEDS_ASSERTIONS
        .iter()
        .copied()
        .filter(|f| asserts(f))
        .collect();
    assert!(
        stale.is_empty(),
        "these files now assert and must be removed from NEEDS_ASSERTIONS:\n  {}",
        stale.join("\n  ")
    );

    let gone: Vec<&str> = NEEDS_ASSERTIONS
        .iter()
        .copied()
        .filter(|f| !Path::new("tests").join(f).exists())
        .collect();
    assert!(
        gone.is_empty(),
        "these files no longer exist and must be removed from NEEDS_ASSERTIONS:\n  {}",
        gone.join("\n  ")
    );
}

#[test]
fn the_support_module_list_is_accurate() {
    let wrong: Vec<String> = SUPPORT_MODULES
        .iter()
        .filter_map(|(file, _)| {
            let path = Path::new("tests").join(file);
            if !path.exists() {
                return Some(format!("{} is listed as support but does not exist", file));
            }
            let source = std::fs::read_to_string(path).unwrap_or_default();
            if source.contains("def main(") {
                return Some(format!(
                    "{} now has an entry point and must leave SUPPORT_MODULES",
                    file
                ));
            }
            None
        })
        .collect();
    assert!(wrong.is_empty(), "{}", wrong.join("\n  "));
}

// -- The execution gate ---------------------------------------------------

/// Every corpus file must run, unless it is a negative test or listed breakage.
/// This is what puts the corpus into CI at all.
#[test]
fn every_iris_test_runs() {
    let broken: Vec<&str> = KNOWN_BROKEN.iter().map(|(f, _)| *f).collect();
    let must_fail: Vec<&str> = MUST_FAIL.iter().map(|(f, _)| *f).collect();
    let support: Vec<&str> = SUPPORT_MODULES.iter().map(|(f, _)| *f).collect();

    let mut failures = Vec::new();
    for f in corpus() {
        if broken.contains(&f.as_str())
            || must_fail.contains(&f.as_str())
            || support.contains(&f.as_str())
        {
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
        "corpus files that should run but did not:\n  {}",
        failures.join("\n  ")
    );
}

/// A negative test that starts passing is as much a defect as a positive test
/// that starts failing: it means the check it guards has been lost.
#[test]
fn negative_tests_still_fail() {
    let mut wrong = Vec::new();
    for (f, why) in MUST_FAIL {
        if !Path::new("tests").join(f).exists() {
            wrong.push(format!("{} is listed in MUST_FAIL but does not exist", f));
            continue;
        }
        if let (Some(0), _) = run(f) {
            wrong.push(format!("{} compiled and ran, but {}", f, why));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n  "));
}

/// The two backends must compute the same answer.
///
/// The gate used to run only the native path, so an interpreter-only failure
/// escaped CI entirely -- and the backends genuinely disagree in both
/// directions (known-issues #52). Asserting they *agree* is a stronger and
/// cheaper statement than asserting each passes separately: it catches a
/// regression in either one, and it caught `to_str` on an option printing a raw
/// address natively while the interpreter printed `some(6)`.
///
/// Only stdout is compared. Diagnostics go to stderr -- which is itself
/// something this check forced: `iris_codegen:` progress lines were being
/// written to stdout, so every single file "diverged" until they were moved.
#[test]
fn the_two_backends_agree() {
    let exempt: Vec<&str> = MUST_FAIL
        .iter()
        .map(|(f, _)| *f)
        .chain(KNOWN_BROKEN.iter().map(|(f, _)| *f))
        .chain(KNOWN_DIVERGENT.iter().map(|(f, _)| *f))
        .chain(SUPPORT_MODULES.iter().map(|(f, _)| *f))
        .collect();

    let mut disagree = Vec::new();
    for f in corpus() {
        if exempt.contains(&f.as_str()) {
            continue;
        }
        let native = outcome_of(&f, false);
        let interp = outcome_of(&f, true);
        if native != interp {
            disagree.push(format!(
                "{}
      native (exit {:?}): {}
      interp (exit {:?}): {}",
                f,
                native.0,
                native.1.lines().last().unwrap_or("(no output)"),
                interp.0,
                interp.1.lines().last().unwrap_or("(no output)")
            ));
        }
    }
    assert!(
        disagree.is_empty(),
        "these files produce different output on the two backends:
  {}",
        disagree.join(
            "
  "
        )
    );
}

/// The divergence list must stay honest, like the others.
#[test]
fn the_known_divergent_list_is_accurate() {
    let mut wrong = Vec::new();
    for (f, _) in KNOWN_DIVERGENT {
        if !Path::new("tests").join(f).exists() {
            wrong.push(format!("{} is listed as divergent but does not exist", f));
            continue;
        }
        let native = outcome_of(f, false);
        let interp = outcome_of(f, true);
        if native == interp {
            wrong.push(format!(
                "{} now agrees and must come off KNOWN_DIVERGENT",
                f
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{}",
        wrong.join(
            "
  "
        )
    );
}

/// The debt register must describe reality: a file that has been fixed comes off
/// KNOWN_BROKEN, or the list stops being a to-do list.
#[test]
fn the_known_broken_list_is_accurate() {
    let mut wrong = Vec::new();
    for (f, _) in KNOWN_BROKEN {
        if !Path::new("tests").join(f).exists() {
            wrong.push(format!("{} is listed as broken but does not exist", f));
            continue;
        }
        if let (Some(0), _) = run(f) {
            wrong.push(format!("{} now runs and must come off KNOWN_BROKEN", f));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n  "));
}
