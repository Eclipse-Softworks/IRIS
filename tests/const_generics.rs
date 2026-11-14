use iris::codegen::build::execute_binary_for_eval;
use iris::ir::types::{DType, IrType};
use iris::{compile, compile_to_module, EmitKind};

#[test]
fn distinct_const_generic_array_instantiations_preserve_exact_field_lengths() {
    let source = r#"
record Array[T, const N: usize] {
    data: [T; N]
}

def sum3(a: Array<i64, 3>) -> i64 {
    a.data[0] + a.data[1] + a.data[2]
}

def sum5(a: Array<i64, 5>) -> i64 {
    a.data[0] + a.data[1] + a.data[2] + a.data[3] + a.data[4]
}

def sum4_f64(a: Array<f64, 4>) -> f64 {
    a.data[0] + a.data[1] + a.data[2] + a.data[3]
}

def main() -> i64 {
    val a3: Array<i64, 3> = Array { data: [10, 20, 30] }
    val a5: Array<i64, 5> = Array { data: [1, 2, 3, 4, 5] }
    val af: Array<f64, 4> = Array { data: [1.5, 2.5, 3.0, 1.0] }
    assert(sum3(a3) == 60)
    assert(sum5(a5) == 15)
    assert(sum4_f64(af) == 8.0)
    assert(len(a3.data) == 3)
    assert(len(a5.data) == 5)
    sum3(a3) + sum5(a5)
}
"#;

    let module = compile_to_module(source, "const_generic_distinct").expect("lower module");
    assert_eq!(
        module.struct_def("Array__i64_3"),
        Some(&vec![(
            "data".to_string(),
            IrType::Array {
                elem: Box::new(IrType::Scalar(DType::I64)),
                len: 3,
            },
        )]),
        "Array__i64_3 must have concrete length 3 in struct_defs"
    );
    assert_eq!(
        module.struct_def("Array__i64_5"),
        Some(&vec![(
            "data".to_string(),
            IrType::Array {
                elem: Box::new(IrType::Scalar(DType::I64)),
                len: 5,
            },
        )]),
        "Array__i64_5 must have concrete length 5 in struct_defs"
    );
    assert_eq!(
        module.struct_def("Array__f64_4"),
        Some(&vec![(
            "data".to_string(),
            IrType::Array {
                elem: Box::new(IrType::Scalar(DType::F64)),
                len: 4,
            },
        )]),
        "Array__f64_4 must have concrete length 4 in struct_defs"
    );

    let eval_out = compile(source, "const_generic_distinct", EmitKind::Eval).expect("eval");
    assert_eq!(eval_out.trim(), "75");

    execute_binary_for_eval(&module).expect("native binary must execute cleanly");
}

#[test]
fn unannotated_generic_struct_literals_infer_type_and_const_params() {
    let source = r#"
record Array[T, const N: usize] {
    data: [T; N]
}

record NamedBuf[const N: usize, T] {
    tag: T,
    items: [T; N]
}

def main() -> i64 {
    val a = Array { data: [10, 20, 30] }
    val b = Array { data: [1, 2, 3, 4, 5] }
    val nb = NamedBuf { items: [4, 5, 6], tag: 100 }
    assert(len(a.data) == 3)
    assert(len(b.data) == 5)
    assert(a.data[2] == 30)
    assert(b.data[4] == 5)
    assert(nb.tag == 100)
    assert(len(nb.items) == 3)
    assert(nb.items[1] == 5)
    a.data[0] + b.data[4] + nb.tag + nb.items[2]
}
"#;

    let module = compile_to_module(source, "const_generic_unannotated").expect("lower module");
    let eval_out = compile(source, "const_generic_unannotated", EmitKind::Eval).expect("eval");
    assert_eq!(eval_out.trim(), "121");

    execute_binary_for_eval(&module).expect("native unannotated struct literals must execute");
}

#[test]
fn generic_functions_use_const_generic_value_and_cross_function_calls() {
    let source = r#"
const GLOBAL_K: usize = 4

record Array[T, const N: usize] {
    data: [T; N]
}

def fixed_len[T, const N: usize](arr: [T; N]) -> i64 {
    assert(len(arr) == N)
    N
}

def inner_check[U, const K: usize](buf: Array<U, K>) -> i64 {
    assert(len(buf.data) == K)
    K * 2
}

def make_wrapper[T, const N: usize](arr: [T; N]) -> Array<T, N> {
    Array { data: arr }
}

def pass_arr[T, const N: usize](arr: [T; N]) -> [T; N] {
    arr
}

def wrap_and_forward[T, const N: usize](arr: [T; N]) -> i64 {
    val copy = pass_arr(arr)
    val wrapper = make_wrapper(copy)
    val n_direct = fixed_len(copy)
    val k2 = inner_check(wrapper)
    n_direct + k2 + N
}

def sum_array[const N: usize](buf: Array<i64, N>) -> i64 {
    var total = 0
    for i in 0..N {
        total = total + buf.data[i]
    }
    total
}

def main() -> i64 {
    val x3 = [10, 20, 30]
    val x4: [i64; GLOBAL_K] = [1, 2, 3, 4]
    val r3 = wrap_and_forward(x3)
    val r4 = wrap_and_forward(x4)
    val b4: Array<i64, GLOBAL_K> = Array { data: x4 }
    val s4 = sum_array(b4)
    assert(r3 == 12)
    assert(r4 == 16)
    assert(s4 == 10)
    r3 + r4 + s4
}
"#;

    let module = compile_to_module(source, "const_generic_cross_fn").expect("lower module");
    let eval_out = compile(source, "const_generic_cross_fn", EmitKind::Eval).expect("eval");
    assert_eq!(eval_out.trim(), "38");

    execute_binary_for_eval(&module).expect("native cross-function const generics must execute");
}
