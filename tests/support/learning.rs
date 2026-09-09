use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub fn run(relative: &str, mode: &str, expected: &str) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let work = std::env::temp_dir().join(format!("iris_learning_{}_{nonce}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let stdout = work.join("stdout.txt");
    let stderr = work.join("stderr.txt");
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
    command.stdout(Stdio::from(File::create(&stdout).unwrap()));
    command.stderr(Stdio::from(File::create(&stderr).unwrap()));
    let mut child = command.spawn().expect("start IRIS");
    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let out = std::fs::read_to_string(stdout).unwrap_or_default();
    let err = std::fs::read_to_string(stderr).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&work);
    assert!(
        status.is_some_and(|s| s.success()),
        "{relative} [{mode}]: {status:?}\n{out}\n{err}"
    );
    assert!(
        out.contains(expected),
        "{relative} [{mode}]: expected {expected:?}\n{out}\n{err}"
    );
}
