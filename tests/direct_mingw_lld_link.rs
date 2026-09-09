//! Direct MinGW linker regression test.
//!
//! The compiler must be able to use `ld.lld` as the linker driver without
//! silently falling back to clang. LLVM-C object emission is covered separately.

#[cfg(target_os = "windows")]
#[test]
fn direct_mingw_lld_builds_and_runs_without_linker_fallback() {
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "iris_direct_lld_test_{}_{}",
        std::process::id(),
        nonce
    ));
    std::fs::create_dir_all(&dir).expect("create direct-link test directory");
    let source = dir.join("direct_link.iris");
    let binary = dir.join("direct_link.exe");
    std::fs::write(
        &source,
        "def main() -> i64 { println(\"direct-lld-ok\"); 0 }\n",
    )
    .expect("write direct-link source");

    let build = Command::new(env!("CARGO_BIN_EXE_iris"))
        .env("IRIS_REQUIRE_DIRECT_LLD", "1")
        .args(["-o"])
        .arg(&binary)
        .arg("build")
        .arg(&source)
        .output()
        .expect("run IRIS compiler");
    let build_stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        build.status.success(),
        "direct ld.lld build failed:\n{}",
        build_stderr
    );
    assert!(
        build_stderr.contains("linked via ld.lld directly"),
        "build did not confirm the direct linker path:\n{}",
        build_stderr
    );

    let run = Command::new(&binary).output().expect("run linked binary");
    assert!(run.status.success(), "linked binary failed: {run:?}");
    assert_eq!(String::from_utf8_lossy(&run.stdout), "direct-lld-ok\r\n");

    let _ = std::fs::remove_dir_all(dir);
}
