//! The CUDA backend's device code, run: the same device functions and
//! kernels `--gpu cuda` compiles to PTX, compiled instead for this CPU
//! (`DeviceTarget::HostTest`, each kernel running its range in turn) and
//! driven by a C harness. Results, and the error record a failing iteration
//! leaves -- code, operands, source position -- are checked.
//!
//! The PTX itself is checked to compile in `cuda_kernel_codegen.rs`; running
//! it needs an NVIDIA GPU.

use std::fmt::Write;

use iris::codegen::cuda::{build_host_test, device_module, DeviceTarget};

const PROGRAM: &str = r#"
bring std.tensor
def poly(x: f32, k: f32) -> f32 { x * x * k + (1.0 to f32) }
def main() -> i64 {
    val n = 8
    val a = tensor_ad_from_data(list(0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0), list(8))
    val out = tensor_ad_from_data(list(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0), list(8))
    val m = tensor_ad_from_data(list(0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0), list(4, 4))
    val p = tensor_ad_from_data(list(1.0, 2.0, 3.0, 4.0, 5.0, 6.0), list(6))
    val q = tensor_ad_from_data(list(1.0, 0.0, 0.0, 1.0, 1.0, 1.0), list(6))
    val k: f32 = 0.5 to f32
    val big: i64 = 9223372036854775803
    par for j in 0..n {
        out[j] = poly(a[j], k) + ((j * 3) to f32)
    }
    par for j in 0..4 {
        m[j, j] = m[j, 3 - j] * (2.0 to f32)
    }
    par for j in 0..n {
        out[j] = a[j + 1]
    }
    par for j in 0..n {
        out[j] = (big + j) to f32
    }
    par for j in 0..n {
        assert(j < 5)
        out[j] = j to f32
    }
    par for j in 0..2 {
        val r = einsum("i,i->", p, q)
        out[j] = r[0] + (j to f32)
    }
    return 0
}
"#;

/// The C value of a capture slot, by the capture's name in the program.
fn slot(name: &str, kind: char) -> String {
    match (name, kind) {
        (t, 't') => format!("(uint64_t)(uintptr_t)&{t}_h"),
        ("k", 'f') => "fbits(0.5f)".into(),
        ("big", 'l') => "(uint64_t)9223372036854775803LL".into(),
        ("n", 'l') => "8".into(),
        _ => "0".into(),
    }
}

#[test]
fn device_code_computes_and_reports_errors_like_the_cpu() {
    let src = std::env::temp_dir().join(format!("iris_cuda_host_{}.iris", std::process::id()));
    std::fs::write(&src, PROGRAM).unwrap();
    let module = iris::compile_file_to_module_with_opts(&src, None).expect("compiles");
    let _ = std::fs::remove_file(&src);
    let device = device_module(&module, DeviceTarget::HostTest).expect("device code");
    assert_eq!(
        device.kernels.len(),
        6,
        "every loop is device code: {:?}",
        device.host_only
    );

    let mut c = String::from(
        r#"#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <math.h>
typedef struct { float* data; int64_t* shape; int32_t ndim; int64_t numel; } dt;
typedef struct { int32_t code, dim; int64_t a, b, loc, msg; } err_t;
extern err_t __iris_dev_error;
float __nv_powf(float a, float b) { return powf(a, b); }
double __nv_pow(double a, double b) { return pow(a, b); }
float __nv_expf(float x) { return expf(x); }
float __nv_logf(float x) { return logf(x); }
float __nv_tanhf(float x) { return tanhf(x); }
static uint64_t fbits(float f) { uint32_t u; memcpy(&u, &f, 4); return u; }
float a_d[8] = {0, 1, 2, 3, 4, 5, 6, 7}; int64_t a_s[1] = {8}; dt a_h = {a_d, a_s, 1, 8};
float out_d[8]; int64_t out_s[1] = {8}; dt out_h = {out_d, out_s, 1, 8};
float m_d[16]; int64_t m_s[2] = {4, 4}; dt m_h = {m_d, m_s, 2, 16};
float p_d[6] = {1, 2, 3, 4, 5, 6}; int64_t p_s[1] = {6}; dt p_h = {p_d, p_s, 1, 6};
float q_d[6] = {1, 0, 0, 1, 1, 1}; int64_t q_s[1] = {6}; dt q_h = {q_d, q_s, 1, 6};
static void report(int k) {
    printf("k%d err %d %d %lld %lld %lld %lld out", k, __iris_dev_error.code, __iris_dev_error.dim,
           (long long)__iris_dev_error.a, (long long)__iris_dev_error.b,
           (long long)__iris_dev_error.loc, (long long)__iris_dev_error.msg);
    for (int i = 0; i < 8; i++) printf(" %g", out_d[i]);
    printf(" m");
    for (int i = 0; i < 16; i++) printf(" %g", m_d[i]);
    printf("\n");
}
"#,
    );
    for k in &device.kernels {
        writeln!(c, "extern void {}(int64_t, int64_t, void*);", k.kernel).unwrap();
    }
    c.push_str("int main(void) {\n    for (int i = 0; i < 16; i++) m_d[i] = (float)i;\n");
    let ranges = [8, 4, 8, 8, 8, 2];
    for (idx, k) in device.kernels.iter().enumerate() {
        let body = module.function_by_name(&k.body).unwrap();
        let names: Vec<&str> = body
            .params
            .iter()
            .skip(1)
            .map(|p| p.name.as_str())
            .collect();
        let slots: Vec<String> = names
            .iter()
            .zip(k.captures.chars())
            .map(|(n, kind)| slot(n, kind))
            .collect();
        writeln!(
            c,
            "    {{ uint64_t env[{}] = {{{}}};",
            slots.len().max(1),
            if slots.is_empty() {
                "0".into()
            } else {
                slots.join(", ")
            }
        )
        .unwrap();
        writeln!(c, "      memset(&__iris_dev_error, 0, sizeof __iris_dev_error); memset(out_d, 0, sizeof out_d);").unwrap();
        writeln!(
            c,
            "      {}(0, {}, env); report({idx}); }}",
            k.kernel, ranges[idx]
        )
        .unwrap();
    }
    c.push_str("    return 0;\n}\n");

    let exe = std::env::temp_dir().join(format!(
        "iris_cuda_host_{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    let (device, exe) = match build_host_test(&module, &c, &exe) {
        Ok(built) => built,
        Err(iris::error::CodegenError::ToolchainMissing { detail }) => {
            eprintln!("skipped: {detail}");
            return;
        }
        Err(e) => panic!("{e}"),
    };
    let out = std::process::Command::new(&exe).output().expect("runs");
    let _ = std::fs::remove_file(&exe);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let lines: Vec<Vec<String>> = text
        .lines()
        .map(|l| l.split_whitespace().map(str::to_owned).collect())
        .collect();
    assert_eq!(lines.len(), 6, "{text}");
    let field = |k: usize, i: usize| lines[k][i].clone();
    let outs =
        |k: usize| -> Vec<f64> { lines[k][9..17].iter().map(|v| v.parse().unwrap()).collect() };

    // 0: element-wise through a device function, checked arithmetic.
    assert_eq!(field(0, 2), "0", "{text}");
    let want: Vec<f64> = (0..8)
        .map(|j| (j * j) as f64 * 0.5 + 1.0 + 3.0 * j as f64)
        .collect();
    assert_eq!(outs(0), want, "{text}");

    // 1: two indices.
    let m: Vec<f64> = lines[1][18..34]
        .iter()
        .map(|v| v.parse().unwrap())
        .collect();
    assert_eq!(
        (m[0], m[5], m[10], m[15]),
        (6.0, 12.0, 18.0, 24.0),
        "{text}"
    );

    // 2: a[8] is out of range: E0403's record, with a position.
    assert_eq!(
        (field(2, 2), field(2, 3), field(2, 4), field(2, 5)),
        ("1".into(), "-1".into(), "8".into(), "8".into()),
        "{text}"
    );
    assert_ne!(field(2, 6), "0", "a source position: {text}");
    assert_eq!(
        &outs(2)[..7],
        &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0],
        "{text}"
    );

    // 3: big + 5 overflows (addition, the operands).
    assert_eq!(
        (field(3, 2), field(3, 3), field(3, 4), field(3, 5)),
        (
            "3".into(),
            "0".into(),
            "9223372036854775803".into(),
            "5".into()
        ),
        "{text}"
    );

    // 4: assert(j < 5) fails: a panic with the assert's message.
    assert_eq!(field(4, 2), "5", "{text}");
    let msg: usize = field(4, 7).parse().unwrap();
    assert!(
        device.panics[msg].contains("assertion failed"),
        "{:?}",
        device.panics
    );

    // 5: einsum on the device: p . q = 1 + 4 + 5 + 6 = 16, a rank-0 tensor.
    assert_eq!(field(5, 2), "0", "{text}");
    assert_eq!(&outs(5)[..2], &[16.0, 17.0], "{text}");
}
