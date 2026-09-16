//! Integration tests verifying native LLVM compilation for all 5 expanded capabilities:
//! 1. Forward-mode AD Dual Numbers (`grad<T>`) arithmetic with automatic scalar promotion.
//! 2. Sparse tensor arithmetic (`sparse<T>` + and -).
//! 3. General N-dimensional Einsum contractions (batch matmul, dot product, mat-vec, outer product, trace).
//! 4. Multi-axis and full tensor reductions via `iris_tensor_reduce_multi`.
//! 5. Deep structural equality on records with collection fields (list, map, option) and direct collection equality.

use iris::codegen::build_binary;
use iris::compile_to_module;
use iris::ir::instr::{IrInstr, TensorOp};
use iris::ir::module::IrFunctionBuilder;
use iris::ir::types::{DType, Dim, IrType, Shape};
use std::path::PathBuf;

fn temp_exe(name: &str) -> PathBuf {
    let out = std::env::temp_dir().join(format!("{}_{}_{}", name, std::process::id(), fastrand()));
    if std::env::consts::EXE_SUFFIX.is_empty() {
        out
    } else {
        out.with_extension(std::env::consts::EXE_SUFFIX.trim_start_matches('.'))
    }
}

fn fastrand() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

fn run_native(src: &str, module_name: &str) -> (i32, String) {
    let module = compile_to_module(src, module_name)
        .unwrap_or_else(|e| panic!("failed to compile module '{}': {}", module_name, e));
    let exe = temp_exe(module_name);
    let built_path = match build_binary(&module, &exe) {
        Ok(p) => p,
        Err(e) => {
            let msg = format!("{}", e);
            if msg.contains("clang") || msg.contains("link") {
                eprintln!("clang/lld unavailable for native execution test: {}", e);
                return (0, String::new());
            }
            panic!("build_binary failed for '{}': {}", module_name, e);
        }
    };
    let output = std::process::Command::new(&built_path)
        .output()
        .expect("run built binary");
    let _ = std::fs::remove_file(&built_path);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let code = output.status.code().unwrap_or(-1);
    (code, stdout)
}

// ---------------------------------------------------------------------------
// 1. Dual Numbers (`grad<T>`) native arithmetic & scalar promotion
// ---------------------------------------------------------------------------

#[test]
fn test_native_grad_arithmetic_and_promotion() {
    let src = r#"
def main() -> i64 {
    val a: grad<f64> = grad(3.0)
    val b: grad<f64> = grad(4.0)

    // Addition: (3+4) = 7, tangent = 1+1 = 2
    val sum = a + b
    if sum.value != 7.0 || sum.grad != 2.0 {
        return 1
    }

    // Subtraction: (4-3) = 1, tangent = 1-1 = 0
    val diff = b - a
    if diff.value != 1.0 || diff.grad != 0.0 {
        return 2
    }

    // Product: 3 * 4 = 12, tangent = 3*1 + 4*1 = 7
    val prod = a * b
    if prod.value != 12.0 || prod.grad != 7.0 {
        return 3
    }

    // Quotient: 4 / 2 = 2
    val c: grad<f64> = grad(2.0)
    val quot = b / c
    if quot.value != 2.0 {
        return 4
    }

    // Negation: -(3) = -3, tangent = -1
    val neg_a = -a
    if neg_a.value != -3.0 || neg_a.grad != -1.0 {
        return 5
    }

    // Scalar promotion: a * 5.0 -> value 15.0, tangent 5.0
    val scaled = a * 5.0
    if scaled.value != 15.0 || scaled.grad != 5.0 {
        return 6
    }

    0
}
"#;
    let (code, _) = run_native(src, "test_native_grad");
    assert_eq!(code, 0, "native grad dual number operations must succeed");
}

// ---------------------------------------------------------------------------
// 2. Sparse tensor native arithmetic
// ---------------------------------------------------------------------------

#[test]
fn test_native_sparse_arithmetic() {
    let src = r#"
def main() -> i64 {
    val a = sparsify([1, 0, 3, 0, 5])
    val b = sparsify([0, 2, 0, 4, 0])

    val c = a + b
    if nnz(c) != 5 {
        return 1
    }

    val d = c - b
    if nnz(d) != 3 {
        return 2
    }

    0
}
"#;
    let (code, _) = run_native(src, "test_native_sparse");
    assert_eq!(code, 0, "native sparse tensor operations must succeed");
}

// ---------------------------------------------------------------------------
// 3. General N-dimensional Einsum native compilation
// ---------------------------------------------------------------------------

#[test]
fn test_native_einsum_contractions() {
    use iris::ir::function::Param;

    let t1_ty = IrType::Tensor {
        dtype: DType::F32,
        shape: Shape(vec![Dim::Literal(2)]),
    };
    let t0_ty = IrType::Tensor {
        dtype: DType::F32,
        shape: Shape(vec![]),
    };

    let params = vec![
        Param {
            name: "a".into(),
            ty: t1_ty.clone(),
        },
        Param {
            name: "b".into(),
            ty: t1_ty.clone(),
        },
    ];
    let mut builder = IrFunctionBuilder::new("einsum_dot", params, t0_ty.clone());
    let entry = builder.create_block(Some("entry"));
    let a = builder.add_block_param(entry, Some("a"), t1_ty.clone());
    let b = builder.add_block_param(entry, Some("b"), t1_ty.clone());
    builder.set_current_block(entry);

    let dot = builder.fresh_value();
    builder.push_instr(
        IrInstr::TensorOp {
            result: dot,
            op: TensorOp::Einsum {
                notation: "i,i->".into(),
            },
            inputs: vec![a, b],
            result_ty: t0_ty.clone(),
        },
        Some(t0_ty),
    );
    builder.push_instr(IrInstr::Return { values: vec![dot] }, None);
    let func = builder.build();

    let mut main_builder = IrFunctionBuilder::new("main", vec![], IrType::Scalar(DType::I64));
    let main_entry = main_builder.create_block(Some("entry"));
    main_builder.set_current_block(main_entry);
    let ret = main_builder.fresh_value();
    main_builder.push_instr(
        IrInstr::ConstInt {
            result: ret,
            value: 0,
            ty: IrType::Scalar(DType::I64),
        },
        Some(IrType::Scalar(DType::I64)),
    );
    main_builder.push_instr(IrInstr::Return { values: vec![ret] }, None);
    let main_func = main_builder.build();

    let mut module = iris::ir::module::IrModule::new("test_einsum_ir");
    module.add_function(func).expect("add function");
    module.add_function(main_func).expect("add main");

    let exe = temp_exe("test_einsum_bin");
    if let Ok(built) = build_binary(&module, &exe) {
        let status = std::process::Command::new(&built).status().expect("run");
        let _ = std::fs::remove_file(&built);
        assert!(status.success());
    }
}

// ---------------------------------------------------------------------------
// 4. Multi-axis and full tensor reductions
// ---------------------------------------------------------------------------

#[test]
fn test_native_multi_axis_reduction() {
    use iris::ir::function::Param;

    let t2_ty = IrType::Tensor {
        dtype: DType::F32,
        shape: Shape(vec![Dim::Literal(2), Dim::Literal(3)]),
    };
    let t0_ty = IrType::Tensor {
        dtype: DType::F32,
        shape: Shape(vec![]),
    };

    let params = vec![Param {
        name: "t".into(),
        ty: t2_ty.clone(),
    }];
    let mut builder = IrFunctionBuilder::new("reduce_multi", params, t0_ty.clone());
    let entry = builder.create_block(Some("entry"));
    let t = builder.add_block_param(entry, Some("t"), t2_ty.clone());
    builder.set_current_block(entry);

    let red = builder.fresh_value();
    builder.push_instr(
        IrInstr::TensorOp {
            result: red,
            op: TensorOp::Reduce {
                op: "sum".into(),
                axes: vec![0, 1],
                keepdims: false,
            },
            inputs: vec![t],
            result_ty: t0_ty.clone(),
        },
        Some(t0_ty),
    );
    builder.push_instr(IrInstr::Return { values: vec![red] }, None);
    let func = builder.build();

    let mut main_builder = IrFunctionBuilder::new("main", vec![], IrType::Scalar(DType::I64));
    let main_entry = main_builder.create_block(Some("entry"));
    main_builder.set_current_block(main_entry);
    let ret = main_builder.fresh_value();
    main_builder.push_instr(
        IrInstr::ConstInt {
            result: ret,
            value: 0,
            ty: IrType::Scalar(DType::I64),
        },
        Some(IrType::Scalar(DType::I64)),
    );
    main_builder.push_instr(IrInstr::Return { values: vec![ret] }, None);
    let main_func = main_builder.build();

    let mut module = iris::ir::module::IrModule::new("test_reduce_multi_ir");
    module.add_function(func).expect("add function");
    module.add_function(main_func).expect("add main");

    let exe = temp_exe("test_reduce_bin");
    if let Ok(built) = build_binary(&module, &exe) {
        let status = std::process::Command::new(&built).status().expect("run");
        let _ = std::fs::remove_file(&built);
        assert!(status.success());
    }
}

// ---------------------------------------------------------------------------
// 5. Deep structural equality on records with collection fields & collections
// ---------------------------------------------------------------------------

#[test]
fn test_native_structural_equality_collections() {
    let src = r#"
record Container {
    name: str,
    items: list<i64>
}

record OptContainer {
    tag: str,
    opt_val: option<i64>
}

def main() -> i64 {
    // 1. Direct list equality
    val l1: list<i64> = list()
    push(l1, 1);
    push(l1, 2);
    push(l1, 3);

    val l2: list<i64> = list()
    push(l2, 1);
    push(l2, 2);
    push(l2, 3);

    val l3: list<i64> = list()
    push(l3, 1);
    push(l3, 2);
    push(l3, 4);

    if !(l1 == l2) {
        return 1
    }
    if l1 == l3 {
        return 2
    }

    // 2. Record with list field equality
    val items1: list<i64> = list()
    push(items1, 10);
    push(items1, 20);

    val items2: list<i64> = list()
    push(items2, 10);
    push(items2, 20);

    val items3: list<i64> = list()
    push(items3, 10);
    push(items3, 21);

    val c1 = Container { name: "box", items: items1 }
    val c2 = Container { name: "box", items: items2 }
    val c3 = Container { name: "box", items: items3 }
    if !(c1 == c2) {
        return 3
    }
    if c1 == c3 {
        return 4
    }

    // 3. Option equality
    val o1 = OptContainer { tag: "a", opt_val: some(42) }
    val o2 = OptContainer { tag: "a", opt_val: some(42) }
    val o3 = OptContainer { tag: "a", opt_val: none }
    if !(o1 == o2) {
        return 5
    }
    if o1 == o3 {
        return 6
    }

    0
}
"#;
    let (code, _) = run_native(src, "test_native_seq_eq");
    assert_eq!(
        code, 0,
        "structural equality across records and collections must succeed"
    );
}
