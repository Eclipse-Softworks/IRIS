// simd_vectorization_backend.rs — CPU SIMD / Vectorization
//
// Tests for the SIMD emitter (EmitKind::Simd):
//   - header naming the target's vector ISA
//   - the attribute group every function carries: x86-64-v3 with AVX2 + FMA
//     on x86-64, NEON on AArch64
//   - loop-vectorizer width hints on back-edges
//   - explicit vector kernels for element-wise `par for` bodies
//   - the program itself is unchanged

use iris::{compile, EmitKind};

const FLOAT_LOOP: &str = r#"
def total(n: i64) -> f64 {
    var s = 0.0
    var i = 0
    while i < n {
        s = s + to_f64(i)
        i = i + 1
    }
    s
}
"#;

// ── Test 1: SIMD header present ───────────────────────────────────────────

#[test]
fn test_simd_header() {
    let src = r#"def f(x: i64) -> i64 { x * 2 }"#;
    let ir = compile(src, "test", EmitKind::Simd).unwrap();
    assert!(
        ir.contains("SIMD/Vectorization IR"),
        "expected the SIMD header:\n{}",
        ir
    );
}

// ── Test 2: target-cpu attribute group ────────────────────────────────────

#[test]
fn test_simd_target_cpu_attribute() {
    let src = r#"def f(x: i64) -> i64 { x + 1 }"#;
    let ir = compile(src, "test", EmitKind::Simd).unwrap();
    if cfg!(target_arch = "x86_64") {
        assert!(
            ir.contains(r#""target-cpu"="x86-64-v3""#),
            "expected x86-64-v3:\n{}",
            ir
        );
    } else if cfg!(target_arch = "aarch64") {
        assert!(
            ir.contains(r#""target-features"="+neon""#),
            "expected NEON:\n{}",
            ir
        );
    }
}

// ── Test 3: target-features: AVX2 and FMA, not AVX-512 ────────────────────

#[test]
fn test_simd_avx2_features() {
    let src = r#"def f(x: f64) -> f64 { x }"#;
    let ir = compile(src, "test", EmitKind::Simd).unwrap();
    if cfg!(target_arch = "x86_64") {
        assert!(
            ir.contains(r#""target-features"="+avx2,+fma""#),
            "expected AVX2 + FMA:\n{}",
            ir
        );
        assert!(!ir.contains("avx512"), "x86-64-v3 has no AVX-512:\n{}", ir);
    }
}

// ── Test 4: a loop's back-edge carries a distinct loop node ───────────────

#[test]
fn test_simd_loop_metadata_on_back_edge() {
    let ir = compile(FLOAT_LOOP, "test", EmitKind::Simd).unwrap();
    let back_edges: Vec<&str> = ir
        .lines()
        .filter(|l| l.contains(", !llvm.loop !"))
        .collect();
    assert_eq!(back_edges.len(), 1, "one loop, one back-edge:\n{}", ir);
    assert!(
        ir.contains("= distinct !{"),
        "expected a distinct loop node:\n{}",
        ir
    );
}

// ── Test 5: vectorize.width hint for the loop's element type ──────────────

#[test]
fn test_simd_loop_vectorize_width() {
    let ir = compile(FLOAT_LOOP, "test", EmitKind::Simd).unwrap();
    assert!(
        ir.contains("llvm.loop.vectorize.width"),
        "expected a width hint:\n{}",
        ir
    );
    if cfg!(target_arch = "x86_64") {
        // f64 in 256-bit registers.
        assert!(
            ir.contains(r#"!{!"llvm.loop.vectorize.width", i32 4}"#),
            "{}",
            ir
        );
    }
}

// ── Test 6: a function without loops gets no loop metadata ────────────────

#[test]
fn test_simd_no_loop_no_metadata() {
    let src = r#"def f(x: f64) -> f64 { x * x }"#;
    let ir = compile(src, "test", EmitKind::Simd).unwrap();
    assert!(!ir.contains("!llvm.loop"), "{}", ir);
}

// ── Test 7: attribute group #0 emitted and used ───────────────────────────

#[test]
fn test_simd_attribute_group() {
    let src = r#"def f(x: i64) -> i64 { x + 1 }"#;
    let ir = compile(src, "test", EmitKind::Simd).unwrap();
    assert!(
        ir.contains("attributes #0"),
        "expected 'attributes #0' group:\n{}",
        ir
    );
    assert!(
        ir.lines()
            .any(|l| l.starts_with("define i64 @f(") && l.ends_with(" #0 {")),
        "the function carries it:\n{}",
        ir
    );
}

// ── Test 8: evaluation still correct under SIMD backend ───────────────────

#[test]
fn test_simd_eval_unchanged() {
    let src = r#"def f() -> i64 { 2 + 2 }"#;
    let result = compile(src, "test", EmitKind::Eval).unwrap();
    assert_eq!(result.trim(), "4");
    let ir = compile(src, "test", EmitKind::Simd).unwrap();
    assert!(ir.contains("@f"), "expected '@f' in SIMD IR:\n{}", ir);
}

// ── Test 9: an element-wise `par for` body becomes a vector kernel ────────

#[test]
fn test_simd_par_for_kernel() {
    let src = r#"
def scale(a: tensor<f32, [N]>, out: tensor<f32, [N]>, k: f32) -> i64 {
    par for i in 0..64 {
        out[i] = a[i] * k
    }
    0
}
"#;
    let ir = compile(src, "test", EmitKind::Simd).unwrap();
    assert!(
        ir.contains("define void @__par_body_0_simd(i64 %lo, i64 %hi, ptr %env) #0"),
        "{}",
        ir
    );
    assert!(
        ir.contains("call void @iris_par_for_chunked(ptr @__par_body_0_simd,"),
        "{}",
        ir
    );
    if cfg!(target_arch = "x86_64") {
        assert!(
            ir.contains("load <8 x float>"),
            "f32 lanes, 8 to a register:\n{}",
            ir
        );
        assert!(ir.contains("fmul <8 x float>"), "{}", ir);
    }
    // The rest of the range runs the scalar body.
    assert!(
        ir.contains("call i64 @__par_body_0(i64 %si, ptr %env)"),
        "{}",
        ir
    );
}

// ── Test 10: a body that is not element-wise stays scalar ─────────────────

#[test]
fn test_simd_non_elementwise_body_stays_scalar() {
    let src = r#"
def shift(a: tensor<f32, [N]>, out: tensor<f32, [N]>) -> i64 {
    par for i in 0..63 {
        out[i] = a[i + 1]
    }
    0
}
"#;
    let ir = compile(src, "test", EmitKind::Simd).unwrap();
    assert!(
        !ir.contains("_simd("),
        "a[i + 1] is not at the loop index:\n{}",
        ir
    );
    assert!(
        ir.contains("call void @iris_par_for(ptr @__par_body_0,"),
        "{}",
        ir
    );
}
