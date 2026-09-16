use iris::codegen::llvm_ir::{
    emit_llvm_ir_with_target, target_data_layout, target_preset_to_triple,
};
use iris::compile_to_module;

#[test]
fn test_wasm32_target_resolution_and_data_layout() {
    let triple_wasi = target_preset_to_triple("wasm32-wasi").expect("resolve wasm32-wasi");
    assert_eq!(triple_wasi, "wasm32-wasip1");

    let triple_wasm = target_preset_to_triple("wasm32").expect("resolve wasm32");
    assert_eq!(triple_wasm, "wasm32-wasip1");

    let triple_unknown = target_preset_to_triple("wasm32-unknown").expect("resolve wasm32-unknown");
    assert_eq!(triple_unknown, "wasm32-unknown-unknown");

    let layout = target_data_layout(triple_wasi);
    assert!(
        layout.contains("p:32:32"),
        "wasm data layout must specify 32-bit pointers"
    );
    assert!(
        layout.contains("i64:64"),
        "wasm data layout must align i64 to 64 bits"
    );
}

#[test]
fn test_wasm32_llvm_ir_emission() {
    let src = r#"
def compute_energy(mass: i64, c: i64) -> i64 {
    return mass * c * c
}
"#;

    let module = compile_to_module(src, "wasm_module").expect("compile to IR");
    let llvm_ir =
        emit_llvm_ir_with_target(&module, Some("wasm32-wasi")).expect("emit wasm32 llvm ir");

    assert!(
        llvm_ir.contains("target triple = \"wasm32-wasip1\""),
        "LLVM IR must specify wasm32-wasip1 target triple"
    );
    assert!(
        llvm_ir.contains("target datalayout ="),
        "LLVM IR must include wasm datalayout"
    );
    assert!(
        llvm_ir.contains("@compute_energy"),
        "LLVM IR must declare or define compute_energy"
    );
}
