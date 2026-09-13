use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn run(relative: &str, mode: &str, expected: &str) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let work = std::env::temp_dir().join(format!("iris_learning_{}_{nonce}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_iris"));
    command.current_dir(&work).env_remove("IRIS_FORCE_INTERP");
    match mode {
        "native" => {
            command.arg("run");
        }
        "graph" => {
            command.args(["--emit", "graph"]);
        }
        "interpreter" => {
            command
                .args(["--emit", "eval"])
                .env("IRIS_FORCE_INTERP", "1");
        }
        _ => panic!("unknown execution mode: {mode}"),
    }
    command.arg(root.join(relative));
    let output = command.output().expect("start IRIS");
    let out = String::from_utf8_lossy(&output.stdout).to_string();
    let err = String::from_utf8_lossy(&output.stderr).to_string();
    let _ = std::fs::remove_dir_all(&work);
    assert!(
        output.status.success(),
        "{relative} [{mode}]: {:?}\nstdout:\n{out}\nstderr:\n{err}",
        output.status
    );
    assert!(
        out.contains(expected),
        "{relative} [{mode}]: expected {expected:?}\nstdout:\n{out}\nstderr:\n{err}"
    );
}
