//! Fixed-array aliases must keep their native layout and bounds checks.
use iris::codegen::build::execute_binary_for_eval;
use iris::{compile, compile_to_module, EmitKind};

#[test]
fn native_record_array_loads_use_scalar_layout() {
    let source = r#"
record Buffer[T, const N: usize] { data: [T; N] }
def main() -> i64 {
    val buffer: Buffer<i64, 3> = Buffer { data: [3, 4, 5] };
    assert(buffer.data[0] == 3);
    assert(buffer.data[2] == 5);
    return 0
}
"#;
    let llvm = compile(source, "record_array_native", EmitKind::LlvmComplete).unwrap();
    assert!(!llvm.contains("= call ptr @iris_array_load("), "{llvm}");
    assert!(
        llvm.contains("call void @iris_bounds_check_abort("),
        "{llvm}"
    );
    let module = compile_to_module(source, "record_array_native").unwrap();
    execute_binary_for_eval(&module).expect("record scalar array must execute natively");
}
