//! Whole-tensor operations in GPU code, run: each kernel applies one
//! operation (unary, reduction, transpose, reshape, element-wise arithmetic)
//! to a tensor and copies its element `i` out. Built for this CPU
//! (`DeviceTarget::HostTest`), as `cuda_device_code.rs` does, and checked
//! against the host runtime's definitions.

use std::fmt::Write;

use iris::codegen::cuda::build_host_test;
use iris::ir::function::Param;
use iris::ir::instr::{BinOp, IrInstr, TensorOp};
use iris::ir::module::{IrFunctionBuilder, IrModule};
use iris::ir::types::{DType, Dim, IrType, Shape};

fn tensor(dims: &[u64]) -> IrType {
    IrType::Tensor {
        dtype: DType::F32,
        shape: Shape(dims.iter().map(|d| Dim::Literal(*d)).collect()),
    }
}

enum Op {
    Tensor(TensorOp, Vec<u64>),
    Bin(BinOp),
}

/// `__par_body_{n}(i, a, b, out)`: `r = op(a[, b])`, `out[i] = r[i]`.
fn body(n: usize, op: &Op) -> iris::ir::function::IrFunction {
    let f32t = IrType::Scalar(DType::F32);
    let i64t = IrType::Scalar(DType::I64);
    let params = vec![
        Param {
            name: "i".into(),
            ty: i64t.clone(),
        },
        Param {
            name: "a".into(),
            ty: tensor(&[2, 3]),
        },
        Param {
            name: "b".into(),
            ty: tensor(&[2, 3]),
        },
        Param {
            name: "out".into(),
            ty: tensor(&[6]),
        },
    ];
    let mut f = IrFunctionBuilder::new(format!("__par_body_{n}"), params, i64t.clone());
    let entry = f.create_block(Some("entry"));
    let i = f.add_block_param(entry, Some("i"), i64t.clone());
    let a = f.add_block_param(entry, Some("a"), tensor(&[2, 3]));
    let b = f.add_block_param(entry, Some("b"), tensor(&[2, 3]));
    let out = f.add_block_param(entry, Some("out"), tensor(&[6]));
    f.set_current_block(entry);
    let r = f.fresh_value();
    match op {
        Op::Tensor(op, dims) => {
            let ty = tensor(dims);
            f.push_instr(
                IrInstr::TensorOp {
                    result: r,
                    op: op.clone(),
                    inputs: vec![a],
                    result_ty: ty.clone(),
                },
                Some(ty),
            );
        }
        Op::Bin(op) => {
            let ty = tensor(&[2, 3]);
            f.push_instr(
                IrInstr::BinOp {
                    result: r,
                    op: *op,
                    lhs: a,
                    rhs: b,
                    ty: ty.clone(),
                },
                Some(ty),
            );
        }
    }
    let x = f.fresh_value();
    f.push_instr(
        IrInstr::Load {
            result: x,
            tensor: r,
            indices: vec![i],
            result_ty: f32t.clone(),
        },
        Some(f32t),
    );
    f.push_instr(
        IrInstr::Store {
            tensor: out,
            indices: vec![i],
            value: x,
        },
        None,
    );
    let zero = f.fresh_value();
    f.push_instr(
        IrInstr::ConstInt {
            result: zero,
            value: 0,
            ty: i64t.clone(),
        },
        Some(i64t),
    );
    f.push_instr(IrInstr::Return { values: vec![zero] }, None);
    f.build()
}

#[test]
fn whole_tensor_operations_on_the_device() {
    let unary = [
        "neg", "relu", "sigmoid", "tanh", "exp", "log", "sqrt", "abs",
    ];
    let mut ops: Vec<(Op, usize)> = unary
        .iter()
        .map(|u| {
            (
                Op::Tensor(TensorOp::Unary { op: u.to_string() }, vec![2, 3]),
                6,
            )
        })
        .collect();
    ops.push((
        Op::Tensor(
            TensorOp::Reduce {
                op: "sum".into(),
                axes: vec![1],
                keepdims: false,
            },
            vec![2],
        ),
        2,
    ));
    ops.push((
        Op::Tensor(
            TensorOp::Reduce {
                op: "max".into(),
                axes: vec![0],
                keepdims: true,
            },
            vec![1, 3],
        ),
        3,
    ));
    ops.push((
        Op::Tensor(
            TensorOp::Reduce {
                op: "mean".into(),
                axes: vec![],
                keepdims: false,
            },
            vec![],
        ),
        1,
    ));
    ops.push((
        Op::Tensor(
            TensorOp::Reduce {
                op: "min".into(),
                axes: vec![1],
                keepdims: false,
            },
            vec![2],
        ),
        2,
    ));
    ops.push((
        Op::Tensor(
            TensorOp::Reduce {
                op: "prod".into(),
                axes: vec![0],
                keepdims: false,
            },
            vec![3],
        ),
        3,
    ));
    ops.push((
        Op::Tensor(TensorOp::Transpose { axes: vec![] }, vec![3, 2]),
        6,
    ));
    ops.push((
        Op::Tensor(TensorOp::Transpose { axes: vec![1, 0] }, vec![3, 2]),
        6,
    ));
    ops.push((Op::Tensor(TensorOp::Reshape, vec![3, 2]), 6));
    ops.push((Op::Bin(BinOp::Add), 6));
    ops.push((Op::Bin(BinOp::Mul), 6));
    ops.push((Op::Bin(BinOp::Div), 6));

    // A module whose `main` launches each body over its range.
    let mut module = IrModule::new("tensors");
    let i64t = IrType::Scalar(DType::I64);
    let mut main = IrFunctionBuilder::new("main", vec![], i64t.clone());
    let entry = main.create_block(Some("entry"));
    main.set_current_block(entry);
    for (n, (op, len)) in ops.iter().enumerate() {
        module.add_function(body(n, op)).unwrap();
        let (s, e, var) = (main.fresh_value(), main.fresh_value(), main.fresh_value());
        main.push_instr(
            IrInstr::ConstInt {
                result: s,
                value: 0,
                ty: i64t.clone(),
            },
            Some(i64t.clone()),
        );
        main.push_instr(
            IrInstr::ConstInt {
                result: e,
                value: *len as i64,
                ty: i64t.clone(),
            },
            Some(i64t.clone()),
        );
        main.push_instr(
            IrInstr::ParFor {
                var,
                start: s,
                end: e,
                inclusive: false,
                body_fn: format!("__par_body_{n}"),
                args: vec![],
            },
            None,
        );
    }
    let z = main.fresh_value();
    main.push_instr(
        IrInstr::ConstInt {
            result: z,
            value: 0,
            ty: i64t.clone(),
        },
        Some(i64t),
    );
    main.push_instr(IrInstr::Return { values: vec![z] }, None);
    module.add_function(main.build()).unwrap();

    let device =
        iris::codegen::cuda::device_module(&module, iris::codegen::cuda::DeviceTarget::HostTest)
            .unwrap();
    assert_eq!(device.kernels.len(), ops.len(), "{:?}", device.host_only);

    let mut c = String::from(
        r#"#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <math.h>
typedef struct { float* data; int64_t* shape; int32_t ndim; int64_t numel; } dt;
typedef struct { int32_t code, dim; int64_t a, b, loc, msg; } err_t;
extern err_t __iris_dev_error;
float __nv_expf(float x) { return expf(x); }
float __nv_logf(float x) { return logf(x); }
float __nv_tanhf(float x) { return tanhf(x); }
float a_d[6] = {-2, -1, 0, 1, 2, 3}; int64_t a_s[2] = {2, 3}; dt a_h = {a_d, a_s, 2, 6};
float b_d[6] = {1, 2, 0, 4, 0, 6}; int64_t b_s[2] = {2, 3}; dt b_h = {b_d, b_s, 2, 6};
float o_d[6]; int64_t o_s[1] = {6}; dt o_h = {o_d, o_s, 1, 6};
"#,
    );
    for k in &device.kernels {
        writeln!(c, "extern void {}(int64_t, int64_t, void*);", k.kernel).unwrap();
    }
    c.push_str("int main(void) {\n    uint64_t env[3] = {(uint64_t)(uintptr_t)&a_h, (uint64_t)(uintptr_t)&b_h, (uint64_t)(uintptr_t)&o_h};\n");
    for (n, (k, (_, len))) in device.kernels.iter().zip(&ops).enumerate() {
        writeln!(c, "    memset(&__iris_dev_error, 0, sizeof __iris_dev_error); memset(o_d, 0, sizeof o_d);").unwrap();
        writeln!(c, "    {}(0, {len}, env);", k.kernel).unwrap();
        writeln!(c, "    printf(\"{n} %d\", __iris_dev_error.code); for (int i = 0; i < {len}; i++) printf(\" %.6g\", o_d[i]); printf(\"\\n\");").unwrap();
    }
    c.push_str("    return 0;\n}\n");

    let exe = std::env::temp_dir().join(format!(
        "iris_cuda_tensors_{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    let exe = match build_host_test(&module, &c, &exe) {
        Ok((_, exe)) => exe,
        Err(iris::error::CodegenError::ToolchainMissing { detail }) => {
            eprintln!("skipped: {detail}");
            return;
        }
        Err(e) => panic!("{e}"),
    };
    let out = std::process::Command::new(&exe).output().expect("runs");
    let _ = std::fs::remove_file(&exe);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let rows: Vec<Vec<f64>> = text
        .lines()
        .map(|l| {
            let mut w = l.split_whitespace().skip(1);
            assert_eq!(w.next(), Some("0"), "no error: {l}");
            w.map(|v| {
                if v == "-inf" {
                    f64::NEG_INFINITY
                } else {
                    v.parse().unwrap()
                }
            })
            .collect()
        })
        .collect();
    assert_eq!(rows.len(), ops.len(), "{text}");

    let a = [-2.0f32, -1.0, 0.0, 1.0, 2.0, 3.0];
    let b = [1.0f32, 2.0, 0.0, 4.0, 0.0, 6.0];
    let close = |got: &[f64], want: &[f32], what: &str| {
        assert_eq!(got.len(), want.len(), "{what}");
        for (g, w) in got.iter().zip(want) {
            let w = *w as f64;
            assert!(
                (g - w).abs() <= 1e-5 * w.abs().max(1.0) || (g.is_infinite() && w.is_infinite()),
                "{what}: {got:?} vs {want:?}"
            );
        }
    };
    // The host runtime's definitions (tensor_unary in iris_runtime.c).
    let defs: [fn(f32) -> f32; 8] = [
        |x| -x,
        |x| if x > 0.0 { x } else { 0.0 },
        |x| 1.0 / (1.0 + (-x).exp()),
        |x| x.tanh(),
        |x| x.exp(),
        |x| if x > 0.0 { x.ln() } else { f32::NEG_INFINITY },
        |x| if x >= 0.0 { x.sqrt() } else { 0.0 },
        |x| x.abs(),
    ];
    for (n, f) in defs.iter().enumerate() {
        let want: Vec<f32> = a.iter().map(|x| f(*x)).collect();
        close(&rows[n], &want, unary[n]);
    }
    close(&rows[8], &[-3.0, 6.0], "sum over axis 1");
    close(&rows[9], &[1.0, 2.0, 3.0], "max over axis 0, kept");
    close(&rows[10], &[0.5], "mean of all");
    close(&rows[11], &[-2.0, 1.0], "min over axis 1");
    close(&rows[12], &[-2.0, -2.0, 0.0], "prod over axis 0");
    close(&rows[13], &[-2.0, 1.0, -1.0, 2.0, 0.0, 3.0], "transpose");
    close(
        &rows[14],
        &[-2.0, 1.0, -1.0, 2.0, 0.0, 3.0],
        "transpose by axes",
    );
    close(&rows[15], &a, "reshape");
    let add: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x + y).collect();
    let mul: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x * y).collect();
    let div: Vec<f32> = a
        .iter()
        .zip(&b)
        .map(|(x, y)| if *y != 0.0 { x / y } else { 0.0 })
        .collect();
    close(&rows[16], &add, "add");
    close(&rows[17], &mul, "mul");
    close(&rows[18], &div, "div (x / 0 = 0)");
}
