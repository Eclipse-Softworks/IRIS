use iris::codegen::llvm_orc::{is_orc_jit_available, OrcJitEngine};
use iris::compile_file_to_module;

#[test]
fn executes_viability_records_in_process() {
    if !is_orc_jit_available() {
        eprintln!("skipping: LLVM ORC is unavailable");
        return;
    }

    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/ais_v2_native_repro.iris");
    let module = compile_file_to_module(&fixture).expect("compile AIS v2 fixture");
    let engine = OrcJitEngine::new().expect("create ORC session");
    let loaded = engine.load_module(&module).expect("load AIS v2 fixture");
    assert_eq!(
        loaded
            .call_i64_0("ais_v2_native_contract")
            .expect("execute AIS v2 fixture"),
        0
    );
}
