//! Transactional speculative effects must agree in the interpreter and ORC.

use iris::codegen::llvm_orc::{is_orc_jit_available, OrcJitEngine};
use iris::{compile, compile_to_module, EmitKind};
use std::io::Write;
use std::process::Command;

const STATE_PROGRAM: &str = r#"
def transaction_state() -> i64 effect alloc, transaction {
    val values: list<i64> = list()
    push(values, 1);
    val table = map()
    map_set(table, "x", 10);
    val counter = atomic(5)

    transaction_begin();
    push(values, 2);
    transaction_begin();
    push(values, 3);
    map_set(table, "x", 99);
    atomic_add(counter, 7);
    transaction_commit();
    transaction_rollback();

    val restored = unwrap(map_get(table, "x"))
    if list_len(values) == 1 && list_get(values, 0) == 1 && restored == 10 && atomic_load(counter) == 5 && transaction_depth() == 0 {
        transaction_begin();
        push(values, 4);
        transaction_commit();
        if list_len(values) == 2 && list_get(values, 1) == 4 { 0 } else { 2 }
    } else {
        1
    }
}
"#;

#[test]
fn nested_state_rollback_matches_interpreter_and_native_jit() {
    let interpreted = compile(STATE_PROGRAM, "transaction_eval", EmitKind::Eval)
        .expect("evaluate transaction program");
    assert_eq!(interpreted.trim(), "0");

    if !is_orc_jit_available() {
        eprintln!("LLVM ORC is unavailable; native transaction parity skipped");
        return;
    }
    let module =
        compile_to_module(STATE_PROGRAM, "transaction_orc").expect("compile transaction module");
    let engine = OrcJitEngine::new().expect("create ORC session");
    let loaded = engine
        .load_module(&module)
        .expect("load transaction module");
    assert_eq!(
        loaded
            .call_i64_0("transaction_state")
            .expect("call native transaction"),
        0
    );
}

#[test]
fn staged_file_write_is_visible_inside_and_discarded_on_rollback() {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "iris_transaction_{}_rollback.txt",
        std::process::id()
    ));
    std::fs::write(&path, "before").expect("seed transaction file");
    let iris_path = path.to_string_lossy().replace('\\', "\\\\");
    let source = format!(
        r#"
def file_transaction() -> i64 effect fs, alloc, transaction {{
    transaction_begin();
    file_write_all("{iris_path}", "during");
    val visible = unwrap(file_read_all("{iris_path}"))
    transaction_rollback();
    val after = unwrap(file_read_all("{iris_path}"))
    if visible == "during" && after == "before" {{ 0 }} else {{ 1 }}
}}
"#
    );
    let interpreted = compile(&source, "transaction_file", EmitKind::Eval)
        .expect("evaluate staged file transaction");
    assert_eq!(interpreted.trim(), "0");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "before");

    if is_orc_jit_available() {
        let module = compile_to_module(&source, "transaction_file_orc")
            .expect("compile staged file transaction");
        let engine = OrcJitEngine::new().expect("create file transaction ORC session");
        let loaded = engine
            .load_module(&module)
            .expect("load file transaction module");
        assert_eq!(
            loaded
                .call_i64_0("file_transaction")
                .expect("call native file transaction"),
            0
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "before");
    }
    let _ = std::fs::remove_file(path);
}

#[test]
fn strict_effects_reject_irreversible_transaction_callback() {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "iris_transaction_effect_{}.iris",
        std::process::id()
    ));
    let source = r#"bring std.speculation
def main() -> i64 effect io, alloc, fs, transaction {
    val validator = |x: i64| x == 1;
    val candidate = || { println("not reversible"); 1 };
    val result: option<i64> = speculate_transactional(validator, candidate);
    0
}
"#;
    let mut file = std::fs::File::create(&path).expect("create strict-effects source");
    file.write_all(source.as_bytes())
        .expect("write strict-effects source");
    let output = Command::new(env!("CARGO_BIN_EXE_iris"))
        .args(["--strict-effects", "--emit", "eval"])
        .arg(&path)
        .output()
        .expect("run strict-effects compiler");
    let mut diagnostic = String::from_utf8_lossy(&output.stdout).into_owned();
    diagnostic.push_str(&String::from_utf8_lossy(&output.stderr));
    let _ = std::fs::remove_file(path);
    assert!(
        !output.status.success(),
        "irreversible callback was accepted:\n{diagnostic}"
    );
    assert!(
        diagnostic.contains("E0304"),
        "expected callback diagnostic:\n{diagnostic}"
    );
}

struct TestDir(std::path::PathBuf);

impl TestDir {
    fn new(prefix: &str) -> Self {
        let mut dir = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        dir.push(format!("iris_{prefix}_{}_{}", std::process::id(), nanos));
        std::fs::create_dir_all(&dir).expect("create test dir");
        Self(dir)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn dir_has_wal_artifacts(dir: &std::path::Path) -> bool {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.contains(".iris-txn-")
        })
}

extern "C" {
    fn iris_transaction_recover(dir: *const std::os::raw::c_char) -> i64;
}

#[test]
fn multi_file_commit_and_failure_rollback_match_in_interpreter_and_native() {
    let dir = TestDir::new("wal_multi");
    let a = dir.0.join("a.txt");
    let b = dir.0.join("b.txt");
    let c_new = dir.0.join("c_new.txt");
    let bad_dir = dir.0.join("existing_dir");
    std::fs::create_dir_all(&bad_dir).unwrap();
    std::fs::write(&a, "orig_a").unwrap();
    std::fs::write(&b, "orig_b").unwrap();

    let esc = |p: &std::path::Path| p.to_string_lossy().replace('\\', "\\\\");
    let ok_program = format!(
        r#"
def multi_ok() -> i64 effect fs, alloc, transaction {{
    transaction_begin();
    file_write_all("{}", "next_a");
    file_write_all("{}", "next_b");
    file_write_all("{}", "created_c");
    if transaction_commit() == 1 {{ 0 }} else {{ 1 }}
}}
"#,
        esc(&a),
        esc(&b),
        esc(&c_new)
    );
    let interp_ok = compile(&ok_program, "multi_ok_eval", EmitKind::Eval).unwrap();
    assert_eq!(interp_ok.trim(), "0");
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "next_a");
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "next_b");
    assert_eq!(std::fs::read_to_string(&c_new).unwrap(), "created_c");
    assert!(!dir_has_wal_artifacts(&dir.0));

    // Reset files and fail when a staged path points to a directory.
    std::fs::write(&a, "orig_a").unwrap();
    std::fs::write(&b, "orig_b").unwrap();
    let _ = std::fs::remove_file(&c_new);
    let fail_program = format!(
        r#"
def multi_fail() -> i64 effect fs, alloc, transaction {{
    val items: list<i64> = list()
    push(items, 10);
    transaction_begin();
    push(items, 20);
    file_write_all("{}", "mutated_a");
    file_write_all("{}", "mutated_b");
    file_write_all("{}", "mutated_new");
    file_write_all("{}", "cannot_overwrite_dir");
    val committed = transaction_commit();
    if committed == 0 && list_len(items) == 1 {{ 0 }} else {{ 1 }}
}}
"#,
        esc(&a),
        esc(&b),
        esc(&c_new),
        esc(&bad_dir)
    );
    let interp_fail = compile(&fail_program, "multi_fail_eval", EmitKind::Eval).unwrap();
    assert_eq!(interp_fail.trim(), "0");
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "orig_a");
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "orig_b");
    assert!(!c_new.exists());
    assert!(!dir_has_wal_artifacts(&dir.0));

    if is_orc_jit_available() {
        let mod_ok = compile_to_module(&ok_program, "multi_ok_orc").unwrap();
        let engine = OrcJitEngine::new().unwrap();
        assert_eq!(
            engine
                .load_module(&mod_ok)
                .unwrap()
                .call_i64_0("multi_ok")
                .unwrap(),
            0
        );
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "next_a");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "next_b");
        assert_eq!(std::fs::read_to_string(&c_new).unwrap(), "created_c");
        assert!(!dir_has_wal_artifacts(&dir.0));

        std::fs::write(&a, "orig_a").unwrap();
        std::fs::write(&b, "orig_b").unwrap();
        let _ = std::fs::remove_file(&c_new);
        let mod_fail = compile_to_module(&fail_program, "multi_fail_orc").unwrap();
        let engine_fail = OrcJitEngine::new().unwrap();
        assert_eq!(
            engine_fail
                .load_module(&mod_fail)
                .unwrap()
                .call_i64_0("multi_fail")
                .unwrap(),
            0
        );
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "orig_a");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "orig_b");
        assert!(!c_new.exists());
        assert!(!dir_has_wal_artifacts(&dir.0));
    }
}

fn seed_crashed_committing_wal(
    dir: &std::path::Path,
) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let a = dir.join("a.txt");
    let b = dir.join("b.txt");
    let c_new = dir.join("c_created.txt");
    let a_new = dir.join(".a.txt.iris-txn-new-999-1-0");
    let b_new = dir.join(".b.txt.iris-txn-new-999-1-1");
    let c_temp = dir.join(".c_created.txt.iris-txn-new-999-1-2");
    let a_bak = dir.join(".a.txt.iris-txn-bak-999-1-0");
    let b_bak = dir.join(".b.txt.iris-txn-bak-999-1-1");
    let c_bak = dir.join(".c_created.txt.iris-txn-bak-999-1-2");

    // Simulate crash after a.txt and c_created.txt were overwritten, before b.txt was replaced.
    std::fs::write(&a, "partial_new_a").unwrap();
    std::fs::write(&b, "original_b").unwrap();
    std::fs::write(&c_new, "partial_new_c").unwrap();
    std::fs::write(&a_bak, "original_a").unwrap();
    std::fs::write(&b_bak, "original_b").unwrap();
    std::fs::write(&b_new, "new_b_unapplied").unwrap();

    let wal = iris::tx_journal::WalTransaction {
        state: iris::tx_journal::WalState::Committing,
        entries: vec![
            iris::tx_journal::WalEntry {
                existed: true,
                dest: a.clone(),
                new_path: a_new,
                bak_path: a_bak,
            },
            iris::tx_journal::WalEntry {
                existed: true,
                dest: b.clone(),
                new_path: b_new,
                bak_path: b_bak,
            },
            iris::tx_journal::WalEntry {
                existed: false,
                dest: c_new.clone(),
                new_path: c_temp,
                bak_path: c_bak,
            },
        ],
    };
    let wal_path = dir.join(".iris-txn-999-1.wal");
    std::fs::write(&wal_path, iris::tx_journal::serialize_wal(&wal)).unwrap();
    (a, b, c_new)
}

#[test]
fn crash_recovery_rolls_back_committing_wal_and_cleans_committed_in_rust_and_c() {
    // 1. Rust recovery on COMMITTING crash
    let rust_dir = TestDir::new("wal_recover_rust");
    let (a, b, c_new) = seed_crashed_committing_wal(&rust_dir.0);
    let recovered = iris::tx_journal::recover_incomplete_transactions(&rust_dir.0).unwrap();
    assert_eq!(recovered, 1);
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "original_a");
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "original_b");
    assert!(!c_new.exists());
    assert!(!dir_has_wal_artifacts(&rust_dir.0));

    // 2. C runtime recovery on the exact same WAL format
    let c_dir = TestDir::new("wal_recover_c");
    let (ca, cb, cc_new) = seed_crashed_committing_wal(&c_dir.0);
    let c_dir_str = std::ffi::CString::new(c_dir.0.to_string_lossy().into_owned()).unwrap();
    let c_recovered = unsafe { iris_transaction_recover(c_dir_str.as_ptr()) };
    assert_eq!(c_recovered, 1);
    assert_eq!(std::fs::read_to_string(&ca).unwrap(), "original_a");
    assert_eq!(std::fs::read_to_string(&cb).unwrap(), "original_b");
    assert!(!cc_new.exists());
    assert!(!dir_has_wal_artifacts(&c_dir.0));

    // 3. COMMITTED crash cleanup preserves new files in both Rust and C
    for use_c in [false, true] {
        let dir = TestDir::new("wal_recover_committed");
        let f = dir.0.join("final.txt");
        let f_new = dir.0.join(".final.txt.iris-txn-new-77-1-0");
        let f_bak = dir.0.join(".final.txt.iris-txn-bak-77-1-0");
        std::fs::write(&f, "committed_contents").unwrap();
        std::fs::write(&f_bak, "old_contents").unwrap();
        let wal = iris::tx_journal::WalTransaction {
            state: iris::tx_journal::WalState::Committed,
            entries: vec![iris::tx_journal::WalEntry {
                existed: true,
                dest: f.clone(),
                new_path: f_new,
                bak_path: f_bak,
            }],
        };
        std::fs::write(
            dir.0.join(".iris-txn-77-1.wal"),
            iris::tx_journal::serialize_wal(&wal),
        )
        .unwrap();
        let count = if use_c {
            let cs = std::ffi::CString::new(dir.0.to_string_lossy().into_owned()).unwrap();
            unsafe { iris_transaction_recover(cs.as_ptr()) as usize }
        } else {
            iris::tx_journal::recover_incomplete_transactions(&dir.0).unwrap()
        };
        assert_eq!(count, 1);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "committed_contents");
        assert!(!dir_has_wal_artifacts(&dir.0));
    }
}
