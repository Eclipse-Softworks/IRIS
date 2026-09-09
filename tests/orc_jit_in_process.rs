//! Assertion-backed tests for the in-process LLVM ORC backend.

use iris::codegen::llvm_orc::{is_orc_jit_available, JitValue, OrcJitEngine};
use iris::compile_to_module;

#[test]
fn executes_i64_functions_in_process() {
    if !is_orc_jit_available() {
        eprintln!("LLVM ORC is unavailable; capability test skipped");
        return;
    }

    let module = compile_to_module(
        "def answer() -> i64 { 6 * 7 }\ndef inc(x: i64) -> i64 { x + 1 }",
        "orc_scalar",
    )
    .expect("compile IRIS module");
    let engine = OrcJitEngine::new().expect("create ORC session");
    let loaded = engine.load_module(&module).expect("load ORC module");

    assert_eq!(loaded.call_i64_0("answer").expect("call answer"), 42);
    assert_eq!(loaded.call_i64_1("inc", 41).expect("call inc"), 42);
}

#[test]
fn resolves_checked_arithmetic_collections_and_strings_from_the_linked_runtime() {
    if !is_orc_jit_available() {
        eprintln!("LLVM ORC is unavailable; capability test skipped");
        return;
    }

    let module = compile_to_module(
        r#"
def collection_size() -> i64 {
    val values: list<i64> = list()
    push(values, 40 + 2);
    list_len(values)
}

def greeting() -> str { "hello from ORC" }
"#,
        "orc_runtime",
    )
    .expect("compile runtime-backed module");
    let engine = OrcJitEngine::new().expect("create ORC session");
    let loaded = engine
        .load_module(&module)
        .expect("load runtime-backed module");

    assert_eq!(
        loaded
            .call_i64_0("collection_size")
            .expect("call collection_size"),
        1
    );
    assert_eq!(
        loaded.call_zero_arg("greeting").expect("call greeting"),
        JitValue::Str("hello from ORC".into())
    );
}

#[test]
fn resource_tracker_allows_a_removed_symbol_to_be_reloaded() {
    if !is_orc_jit_available() {
        eprintln!("LLVM ORC is unavailable; capability test skipped");
        return;
    }

    let engine = OrcJitEngine::new().expect("create ORC session");
    let first = compile_to_module("def value() -> i64 { 1 }", "generation_one")
        .expect("compile first generation");
    {
        let loaded = engine.load_module(&first).expect("load first generation");
        assert_eq!(loaded.call_i64_0("value").expect("call first value"), 1);
    }

    let second = compile_to_module("def value() -> i64 { 2 }", "generation_two")
        .expect("compile second generation");
    let loaded = engine
        .load_module(&second)
        .expect("reload symbol after resource removal");
    assert_eq!(loaded.call_i64_0("value").expect("call second value"), 2);
}
