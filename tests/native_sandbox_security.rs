//! End-to-end verification of native C-runtime security mediation and CLI
//! audit hardening flags.

use std::path::PathBuf;
use std::process::{Command, Output};

struct TestDir(PathBuf);

impl TestDir {
    fn new(prefix: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("iris_{prefix}_{}_{}", std::process::id(), nanos));
        std::fs::create_dir_all(&dir).expect("create temp test dir");
        Self(dir)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn iris_bin() -> &'static str {
    env!("CARGO_BIN_EXE_iris")
}

fn run_iris(args: &[&str], extra_env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(iris_bin());
    cmd.args(args);
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.output().expect("execute iris CLI")
}

fn combined_output(out: &Output) -> String {
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s
}

#[test]
fn compiled_sandbox_binary_enforces_embedded_policy_across_c_runtime_calls() {
    let dir = TestDir::new("native_sandbox_calls");
    let exe_ext = if cfg!(windows) { ".exe" } else { "" };

    // 1. Pure computation succeeds inside a compiled `--sandbox` binary.
    let pure_src = dir.0.join("pure.iris");
    let pure_exe = dir.0.join(format!("pure{exe_ext}"));
    std::fs::write(
        &pure_src,
        "def main() -> i64 effect io { println(42); 0 }\n",
    )
    .unwrap();
    let build_pure = run_iris(
        &[
            "--sandbox",
            "--emit",
            "binary",
            "-o",
            &pure_exe.to_string_lossy(),
            &pure_src.to_string_lossy(),
        ],
        &[],
    );
    assert!(
        build_pure.status.success(),
        "failed building pure sandbox binary:\n{}",
        combined_output(&build_pure)
    );
    let run_pure = Command::new(&pure_exe).output().expect("run pure binary");
    assert!(run_pure.status.success());
    assert_eq!(String::from_utf8_lossy(&run_pure.stdout).trim(), "42");

    // 2. Denied C-runtime operations inside compiled `--sandbox` binaries
    //    must fail closed with E0407 even when executed directly without CLI wrapper.
    let target_file = dir.0.join("blocked.txt");
    let esc_file = target_file.to_string_lossy().replace('\\', "\\\\");
    let cases = [
        (
            "fs_write",
            format!(
                "def main() -> i64 effect fs, alloc {{ file_write_all(\"{esc_file}\", \"pwn\"); 0 }}\n"
            ),
        ),
        (
            "fs_read",
            format!(
                "def main() -> i64 effect fs, alloc {{ val _ = file_read_all(\"{esc_file}\"); 0 }}\n"
            ),
        ),
        (
            "mkdir",
            format!("def main() -> i64 effect fs {{ val _ = mkdir(\"{esc_file}\"); 0 }}\n"),
        ),
        (
            "db_open",
            format!("def main() -> i64 effect fs {{ val _ = db_open(\"{esc_file}\"); 0 }}\n"),
        ),
        (
            "net_tcp",
            "def main() -> i64 effect net { val _ = tcp_connect(\"127.0.0.1\", 80); 0 }\n"
                .to_owned(),
        ),
        (
            "proc_exec",
            "def main() -> i64 effect io { val _ = exec_cmd(\"echo hi\"); 0 }\n".to_owned(),
        ),
        (
            "proc_env",
            "def main() -> i64 effect io { env_set(\"IRIS_TEST_ENV\", \"1\"); 0 }\n".to_owned(),
        ),
    ];

    std::fs::write(&target_file, "secret").unwrap();
    for (name, src) in cases {
        let src_path = dir.0.join(format!("{name}.iris"));
        let exe_path = dir.0.join(format!("{name}{exe_ext}"));
        std::fs::write(&src_path, src).unwrap();
        let build = run_iris(
            &[
                "--sandbox",
                "--emit",
                "binary",
                "-o",
                &exe_path.to_string_lossy(),
                &src_path.to_string_lossy(),
            ],
            &[],
        );
        assert!(
            build.status.success(),
            "build failed for '{name}':\n{}",
            combined_output(&build)
        );
        let ran = Command::new(&exe_path).output().expect("run binary");
        let text = combined_output(&ran);
        assert!(
            !ran.status.success(),
            "sandboxed binary '{name}' should have failed closed:\n{text}"
        );
        assert!(
            text.contains("E0407"),
            "expected E0407 security error for '{name}', got:\n{text}"
        );
    }
    assert_eq!(std::fs::read_to_string(&target_file).unwrap(), "secret");
}

#[test]
fn unmediated_extern_c_calls_are_blocked_by_sandbox_and_runtime_policy_spec_enforces_limits() {
    let dir = TestDir::new("native_sandbox_extern");
    let exe_ext = if cfg!(windows) { ".exe" } else { "" };

    // 1. Unmediated `extern def` is rejected under `--sandbox` (`allow_ffi = false`).
    let extern_src = dir.0.join("unmediated.iris");
    std::fs::write(
        &extern_src,
        "extern def puts(s: str) -> i64\ndef main() -> i64 effect io { val _ = puts(\"hi\"); 0 }\n",
    )
    .unwrap();
    for mode in ["binary", "llvm", "llvm-complete", "jit", "eval"] {
        let out = run_iris(
            &["--sandbox", "--emit", mode, &extern_src.to_string_lossy()],
            &[],
        );
        let text = combined_output(&out);
        assert!(
            !out.status.success(),
            "--sandbox --emit {mode} must reject unmediated extern def:\n{text}"
        );
        assert!(
            text.contains("E0407"),
            "expected E0407 for unmediated extern in {mode}:\n{text}"
        );
    }

    // 2. Permissively compiled binary still enforces runtime `IRIS_SECURITY_POLICY`
    //    (allowlist prefix, write-size limit, path traversal rejection).
    let writer_src = dir.0.join("writer.iris");
    let writer_exe = dir.0.join(format!("writer{exe_ext}"));
    let ok_file = dir.0.join("ok.txt");
    let big_file = dir.0.join("big.txt");
    let esc_ok = ok_file.to_string_lossy().replace('\\', "\\\\");
    let esc_big = big_file.to_string_lossy().replace('\\', "\\\\");
    let traversal = format!("{}\\..\\escape.txt", dir.0.to_string_lossy()).replace('\\', "\\\\");

    std::fs::write(
        &writer_src,
        format!(
            r#"
def main() -> i64 effect fs, alloc, io {{
    val mode = env_get("IRIS_MODE")
    if mode == "ok" {{
        val _ = file_write_all("{esc_ok}", "hi");
        println("ok");
        0
    }} else {{
        if mode == "big" {{
            val _ = file_write_all("{esc_big}", "123456789");
            println("unexpected_big");
            0
        }} else {{
            val _ = file_write_all("{traversal}", "no");
            println("unexpected_trav");
            0
        }}
    }}
}}
"#
        ),
    )
    .unwrap();

    let build = run_iris(
        &[
            "--emit",
            "binary",
            "-o",
            &writer_exe.to_string_lossy(),
            &writer_src.to_string_lossy(),
        ],
        &[],
    );
    assert!(build.status.success(), "{}", combined_output(&build));

    let allow_prefix = format!("{}/", dir.0.to_string_lossy().replace('\\', "/"));
    let policy_spec = format!(
        "allow_fs_read=1\nallow_fs_write=1\nallow_network=0\nallow_ffi=0\nallow_process=0\nmax_file_write_bytes=4\nmax_open_files=0\nmax_connections=0\nfs_write_allow={allow_prefix}\n"
    );

    let ok_run = Command::new(&writer_exe)
        .env("IRIS_MODE", "ok")
        .env("IRIS_SECURITY_POLICY", &policy_spec)
        .output()
        .unwrap();
    assert!(
        ok_run.status.success(),
        "small write inside allowlist must succeed:\n{}",
        combined_output(&ok_run)
    );
    assert_eq!(std::fs::read_to_string(&ok_file).unwrap(), "hi");

    let big_run = Command::new(&writer_exe)
        .env("IRIS_MODE", "big")
        .env("IRIS_SECURITY_POLICY", &policy_spec)
        .output()
        .unwrap();
    let big_text = combined_output(&big_run);
    assert!(
        !big_run.status.success(),
        "oversized write must fail:\n{big_text}"
    );
    assert!(big_text.contains("E0407"), "expected E0407:\n{big_text}");

    let trav_run = Command::new(&writer_exe)
        .env("IRIS_MODE", "trav")
        .env("IRIS_SECURITY_POLICY", &policy_spec)
        .output()
        .unwrap();
    let trav_text = combined_output(&trav_run);
    assert!(
        !trav_run.status.success(),
        "path traversal write must fail:\n{trav_text}"
    );
    assert!(trav_text.contains("E0407"), "expected E0407:\n{trav_text}");
}

#[test]
fn evolve_cli_enforces_audit_hmac_mirrors_and_separate_trust_domain() {
    let dir = TestDir::new("evolve_cli_audit_trust");
    let baseline = dir.0.join("baseline.iris");
    let candidate = dir.0.join("candidate.iris");
    let cases = dir.0.join("cases.json");
    let constitution = dir.0.join("constitution.txt");
    let logs = dir.0.join("logs");
    let trusted = dir.0.join("trusted");
    let mirror = dir.0.join("mirror");
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::create_dir_all(&trusted).unwrap();
    std::fs::create_dir_all(&mirror).unwrap();

    std::fs::write(
        &baseline,
        r#"
def policy(x: i64) -> i64 { x }
def test_policy() -> i64 { if policy(2) == 2 { 0 } else { 1 } }
def test_transaction_rollback() -> i64 effect transaction {
    transaction_begin();
    transaction_rollback();
    0
}
"#,
    )
    .unwrap();
    std::fs::write(
        &candidate,
        r#"
def policy(x: i64) -> i64 { x * 2 }
def test_policy() -> i64 { if policy(2) == 4 { 0 } else { 1 } }
def test_transaction_rollback() -> i64 effect transaction {
    transaction_begin();
    transaction_rollback();
    0
}
"#,
    )
    .unwrap();
    std::fs::write(&cases, r#"[{"input": 3, "expected": 6}]"#).unwrap();
    std::fs::write(&constitution, "constitution-v1\n").unwrap();

    let sha_out = iris::evolution::sha256_hex(b"constitution-v1\n");
    let audit_log = logs.join("audit.jsonl");
    let same_head = logs.join("audit.head.json");
    let sep_head = trusted.join("audit.head.json");
    let mirror_head = mirror.join("audit.head.json");
    let key_file = dir.0.join("hmac.key");
    std::fs::write(&key_file, "cli-secret-key\n").unwrap();

    // 1. Same-directory checkpoint must be rejected when --require-separate-audit-domain is set.
    let same_dir_out = run_iris(
        &[
            "evolve",
            "--baseline",
            &baseline.to_string_lossy(),
            "--candidate",
            &candidate.to_string_lossy(),
            "--cases",
            &cases.to_string_lossy(),
            "--constitution",
            &constitution.to_string_lossy(),
            "--constitution-sha256",
            &sha_out,
            "--audit",
            &audit_log.to_string_lossy(),
            "--audit-head",
            &same_head.to_string_lossy(),
            "--require-separate-audit-domain",
            "--validate",
            "interpreter",
        ],
        &[],
    );
    assert!(!same_dir_out.status.success());

    // 2. Separate directories + HMAC key file + mirror succeed cleanly.
    let ok_out = run_iris(
        &[
            "evolve",
            "--baseline",
            &baseline.to_string_lossy(),
            "--candidate",
            &candidate.to_string_lossy(),
            "--cases",
            &cases.to_string_lossy(),
            "--constitution",
            &constitution.to_string_lossy(),
            "--constitution-sha256",
            &sha_out,
            "--audit",
            &audit_log.to_string_lossy(),
            "--audit-head",
            &sep_head.to_string_lossy(),
            "--audit-head-mirror",
            &mirror_head.to_string_lossy(),
            "--audit-hmac-key-file",
            &key_file.to_string_lossy(),
            "--require-separate-audit-domain",
            "--validate",
            "interpreter",
        ],
        &[],
    );
    assert!(
        ok_out.status.success(),
        "evolve with separate audit domain & HMAC failed:\n{}",
        combined_output(&ok_out)
    );
    let head_json = std::fs::read_to_string(&sep_head).unwrap();
    let mirror_json = std::fs::read_to_string(&mirror_head).unwrap();
    assert_eq!(head_json, mirror_json);
    assert!(head_json.contains("iris-evolution-audit-head/2"));
}
