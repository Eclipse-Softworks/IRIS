use iris::codegen::emit_simd;
use iris::{compile, compile_to_module, EmitKind};

#[test]
fn test_simd_emission_via_lib_compile() {
    let src = r#"
def vector_accumulate(n: i64) -> i64 {
    var total = 0;
    var i = 0;
    while i < n {
        total = total + (i * 2);
        i = i + 1
    };
    return total
}
"#;

    let llvm_ir = compile(src, "vec_acc", EmitKind::Simd).expect("compile simd");

    // Verify SIMD preamble
    assert!(llvm_ir.contains("IRIS SIMD/Vectorization IR — phase 51"));
    assert!(llvm_ir.contains("Target: x86-64-v3 (AVX2 + FMA)"));

    // Verify target-cpu and target-features attributes
    assert!(llvm_ir.contains(r#""target-cpu"="x86-64-v3""#));
    assert!(llvm_ir.contains(r#""target-features"="+avx2,+fma,+avx512f""#));

    // Verify function definition is tagged with attribute group #0
    assert!(llvm_ir.contains("define i64 @vector_accumulate(i64 %n)"));
    assert!(llvm_ir.contains("#0 {"));

    // Verify loop vectorizer hints
    assert!(llvm_ir.contains(r#"!{!"llvm.loop.vectorize.enable", i1 true}"#));
    assert!(llvm_ir.contains(r#"!{!"llvm.loop.vectorize.width", i32 8}"#));
    assert!(llvm_ir.contains(r#"!{!"llvm.loop.unroll.count", i32 4}"#));
    assert!(llvm_ir.contains("!llvm.loop.vec.f32"));
    assert!(llvm_ir.contains("!llvm.loop.vec.f64"));

    // Verify back-edge metadata injection
    assert!(llvm_ir.contains("!llvm.loop !llvm.loop.vec.f64"));
}

#[test]
fn test_simd_emission_direct() {
    let src = r#"
def scale_vector(x: f64) -> f64 {
    return x * 3.141592653589793
}
"#;

    let module = compile_to_module(src, "vec_scale").expect("compile to module");
    let simd_ir = emit_simd(&module).expect("emit simd");

    assert!(simd_ir.contains(r#""target-cpu"="x86-64-v3""#));
    assert!(simd_ir.contains("@scale_vector"));
}
