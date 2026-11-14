//! Profile-guided optimisation, end to end: a `--pgo-generate` build counts
//! its blocks as it runs and adds them to the profile file at exit (several
//! runs accumulate); `--pgo-use` applies the counts -- branch weights, entry
//! counts, `cold` -- and the optimised build runs the same.

use std::path::PathBuf;
use std::process::Command;

const PROGRAM: &str = r#"
def classify(x: i64) -> i64 {
    if x % 100 == 0 { 1 } else { 0 }
}
def never(x: i64) -> i64 { x * 2 }
def main() -> i64 {
    var i = 0
    var hits = 0
    while i < 10000 {
        hits = hits + classify(i)
        i = i + 1
    }
    if hits < 0 { println(to_str(never(hits))) }
    println(to_str(hits))
    assert(hits == 100)
    return 0
}
"#;

fn dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("iris_pgo_e2e_{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn iris(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_iris"))
        .args(args)
        .env_remove("IRIS_FORCE_INTERP")
        .output()
        .expect("run iris")
}

#[test]
fn a_recorded_profile_drives_the_optimised_build() {
    let d = dir();
    let src = d.join("pgo.iris");
    std::fs::write(&src, PROGRAM).unwrap();
    let src_s = src.to_str().unwrap();
    let profile = d.join("pgo.profile");
    let _ = std::fs::remove_file(&profile);

    // Record: two runs of the instrumented build.
    let gen = d.join(format!("gen{}", std::env::consts::EXE_SUFFIX));
    let out = iris(&[
        "--pgo-generate",
        "--emit",
        "binary",
        "-o",
        gen.to_str().unwrap(),
        src_s,
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for _ in 0..2 {
        let run = Command::new(&gen)
            .env("IRIS_PROFILE_FILE", &profile)
            .output()
            .unwrap();
        assert!(run.status.success());
        assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "100");
    }
    let text = std::fs::read_to_string(&profile).expect("the profile was written");
    for line in [
        "classify:entry0:20000",
        "classify:then1:200",
        "classify:else2:19800",
        "never:entry0:0",
        "main:while_body2:20000",
    ] {
        assert!(
            text.lines().any(|l| l == line),
            "expected `{line}` in:\n{text}"
        );
    }

    // Apply: the IR carries the measured weights.
    let p = profile.to_str().unwrap();
    let ir = iris(&["--pgo-use", p, "--emit", "pgo-optimize", src_s]);
    assert!(
        ir.status.success(),
        "{}",
        String::from_utf8_lossy(&ir.stderr)
    );
    let ir = String::from_utf8_lossy(&ir.stdout);
    assert!(ir.contains("profile matched 3 of 3 functions"), "{ir}");
    assert!(
        ir.contains("!{!\"branch_weights\", i32 200, i32 19800}"),
        "{ir}"
    );
    assert!(
        ir.contains("!{!\"function_entry_count\", i64 20000}"),
        "{ir}"
    );
    assert!(
        ir.lines()
            .any(|l| l.starts_with("define i64 @never(") && l.contains(" cold ")),
        "{ir}"
    );

    // The optimised build runs the same.
    let opt = d.join(format!("opt{}", std::env::consts::EXE_SUFFIX));
    let out = iris(&[
        "--pgo-use",
        p,
        "--emit",
        "binary",
        "-o",
        opt.to_str().unwrap(),
        src_s,
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&opt).output().unwrap();
    assert!(run.status.success());
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "100");
}

#[test]
fn a_run_stopped_by_a_runtime_error_still_writes_its_profile() {
    let d = dir();
    let src = d.join("pgo_err.iris");
    std::fs::write(
        &src,
        "def main() -> i64 {\n    var i = 0\n    while i < 50 { i = i + 1 }\n    val xs: list<i64> = list(1)\n    println(to_str(list_get(xs, i)))\n    return 0\n}\n",
    )
    .unwrap();
    let profile = d.join("pgo_err.profile");
    let _ = std::fs::remove_file(&profile);
    let exe = d.join(format!("err{}", std::env::consts::EXE_SUFFIX));
    let out = iris(&[
        "--pgo-generate",
        "--emit",
        "binary",
        "-o",
        exe.to_str().unwrap(),
        src.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&exe)
        .env("IRIS_PROFILE_FILE", &profile)
        .output()
        .unwrap();
    assert_eq!(run.status.code(), Some(101));
    let text = std::fs::read_to_string(&profile).expect("written on the error path");
    assert!(text.lines().any(|l| l == "main:while_body2:50"), "{text}");
}
