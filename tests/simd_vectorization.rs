use iris::codegen::emit_simd;
use iris::{compile, compile_to_module, EmitKind};

#[test]
fn test_simd_emission_via_lib_compile() {
    let src = r#"
def vector_accumulate(n: i64) -> f64 {
    var total = 0.0;
    var i = 0;
    while i < n {
        total = total + (to_f64(i) * 2.0);
        i = i + 1
    };
    return total
}
"#;

    let llvm_ir = compile(src, "vec_acc", EmitKind::Simd).expect("compile simd");

    assert!(llvm_ir.contains("IRIS SIMD/Vectorization IR"));
    if cfg!(target_arch = "x86_64") {
        assert!(llvm_ir.contains("Target: x86-64-v3 (AVX2 + FMA)"));
        assert!(llvm_ir.contains(r#""target-cpu"="x86-64-v3""#));
        // AVX-512 is not part of x86-64-v3: code using it would fault on
        // the many AVX2 machines without it.
        assert!(llvm_ir.contains(r#""target-features"="+avx2,+fma""#));
        assert!(!llvm_ir.contains("avx512"));
    }

    // Every function definition carries the attribute group.
    assert!(llvm_ir.contains("define double @vector_accumulate(i64 %p.n) nounwind #0 {"));

    // The loop's back-edge has its own distinct loop node with a width hint
    // for f64: an attachment names a metadata node, never named metadata.
    let back_edge = llvm_ir
        .lines()
        .find(|l| l.contains("!llvm.loop !"))
        .expect("a back-edge with loop metadata");
    let id = back_edge.rsplit("!llvm.loop !").next().unwrap().trim();
    assert!(
        llvm_ir.contains(&format!("!{id} = distinct !{{!{id}, !")),
        "loop node !{id} must be distinct and self-referential:\n{llvm_ir}"
    );
    if cfg!(target_arch = "x86_64") {
        assert!(llvm_ir.contains(r#"!{!"llvm.loop.vectorize.width", i32 4}"#));
    }
    assert!(
        !llvm_ir.contains("!llvm.loop.vec."),
        "named metadata used as an attachment"
    );
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

    if cfg!(target_arch = "x86_64") {
        assert!(simd_ir.contains(r#""target-cpu"="x86-64-v3""#));
    }
    assert!(simd_ir.contains("@scale_vector"));
}
