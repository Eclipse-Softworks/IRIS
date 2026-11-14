//! A failing `assert` names its condition and position on both backends,
//! even inside a function that was inlined. It used to say only "assertion
//! failed": natively with no position, interpreted at the call site.

use std::path::PathBuf;
use std::process::Command;

fn program(name: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("iris_assert_location_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, source).unwrap();
    path
}

/// stderr from running the program natively and interpreted.
fn both_stderr(path: &PathBuf) -> (String, String) {
    let exe = path.with_extension(if cfg!(windows) { "exe" } else { "out" });
    let build = Command::new(env!("CARGO_BIN_EXE_iris"))
        .args(["--emit", "binary", "-o"])
        .arg(&exe)
        .arg(path)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&exe).output().unwrap();
    assert!(
        !native.status.success(),
        "the native program should fail its assert"
    );
    let interp = Command::new(env!("CARGO_BIN_EXE_iris"))
        .args(["--emit", "eval"])
        .arg(path)
        .env("IRIS_FORCE_INTERP", "1")
        .output()
        .unwrap();
    assert!(
        !interp.status.success(),
        "the interpreted program should fail its assert"
    );
    (
        String::from_utf8_lossy(&native.stderr).into_owned(),
        String::from_utf8_lossy(&interp.stderr).into_owned(),
    )
}

#[test]
fn a_failing_assert_names_its_condition_and_line() {
    let path = program(
        "check.iris",
        "def check() -> i64 {\n    val a = 1;\n    assert(a == 1);\n    assert(a ==   2);\n    0\n}\n\ndef main() -> i64 {\n    check();\n    return 0\n}\n",
    );
    let (native, interp) = both_stderr(&path);
    for (backend, text) in [("native", &native), ("interpreted", &interp)] {
        assert!(
            text.contains("assertion failed: a == 2 at ") && text.contains("check.iris:4:5"),
            "{backend} stderr does not name the assert:\n{text}"
        );
    }
}

#[test]
fn an_assert_can_carry_its_own_message() {
    let path = program(
        "message.iris",
        "def main() -> i64 {\n    val n = 3;\n    assert(n > 5, \"n is \" + to_str(n));\n    return 0\n}\n",
    );
    let (native, interp) = both_stderr(&path);
    for (backend, text) in [("native", &native), ("interpreted", &interp)] {
        assert!(
            text.contains("assertion failed: n is 3"),
            "{backend} stderr:\n{text}"
        );
    }
}
