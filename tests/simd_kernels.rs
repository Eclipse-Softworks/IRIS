//! `--simd` builds, run: element-wise `par for` bodies execute as vector
//! kernels, give the scalar build's answers bit for bit
//! (tests/test_simd_kernels.iris checks every element), and stop with the
//! scalar build's error -- code, message and line -- when an iteration fails.

use std::path::{Path, PathBuf};
use std::process::Command;

fn iris() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_iris"));
    cmd.env_remove("IRIS_FORCE_INTERP")
        .env_remove("IRIS_ERROR_FILE");
    cmd
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("iris_simd_kernels_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

/// Build `src` natively, with or without `--simd`.
fn build(src: &Path, simd: bool, exe: &Path) {
    let mut cmd = iris();
    if simd {
        cmd.arg("--simd");
    }
    let out = cmd
        .args(["--emit", "binary", "-o"])
        .arg(exe)
        .arg(src)
        .output()
        .expect("run iris");
    assert!(
        out.status.success(),
        "build of {} failed:\n{}",
        src.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Exit code and the first two stderr lines (the error and its position).
fn run(exe: &Path) -> (Option<i32>, String, String) {
    let out = Command::new(exe)
        .env_remove("IRIS_ERROR_FILE")
        .output()
        .expect("run binary");
    let err = String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n");
    let head: Vec<&str> = err.lines().take(2).collect();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        head.join("\n"),
    )
}

#[test]
fn simd_build_runs_vector_kernels_with_the_scalar_answers() {
    let src = Path::new("tests/test_simd_kernels.iris");
    let ir = iris()
        .args(["--simd", "--emit", "llvm"])
        .arg(src)
        .output()
        .expect("emit");
    let ir = String::from_utf8_lossy(&ir.stdout);
    let kernels = ir
        .lines()
        .filter(|l| l.starts_with("define void @__par_body_") && l.contains("_simd("))
        .count();
    assert_eq!(
        kernels, 5,
        "every par for in the test is element-wise:\n{ir}"
    );
    assert_eq!(ir.matches("call void @iris_par_for_chunked(").count(), 5);

    let exe = scratch("kernels_simd");
    build(src, true, &exe);
    let (code, stdout, err) = run(&exe);
    assert_eq!(code, Some(0), "{err}");
    assert!(stdout.contains("simd kernels ok"), "{stdout}");
}

#[test]
fn an_iteration_that_fails_reports_the_scalar_builds_error() {
    for (name, expect) in [
        (
            "par_for_overflow",
            "error[E0409]: integer overflow in addition (9223372036854775797 + 11)\n --> tests/runtime_errors/par_for_overflow.iris:10:19",
        ),
        (
            "par_for_index",
            "error[E0403]: index 10 is out of bounds for length 10\n --> tests/runtime_errors/par_for_index.iris:9:18",
        ),
    ] {
        let src = PathBuf::from(format!("tests/runtime_errors/{name}.iris"));
        for simd in [false, true] {
            let exe = scratch(&format!("{name}_{simd}"));
            build(&src, simd, &exe);
            // Several workers run at once; the report is the failing one's.
            for _ in 0..5 {
                let (code, _, err) = run(&exe);
                assert_eq!(code, Some(101), "{name} (simd: {simd}): {err}");
                assert_eq!(err, expect, "{name} (simd: {simd})");
            }
        }
    }
}
