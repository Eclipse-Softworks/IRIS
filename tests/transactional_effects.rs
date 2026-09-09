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
