//! Native define-by-run tensor AD across ordinary IRIS control flow.

use iris::codegen::llvm_orc::{is_orc_jit_available, OrcJitEngine};
use iris::{compile_file, compile_file_to_module, EmitKind};

const CONTROL_FLOW_PROGRAM: &str = r#"
bring std.tensor

record SquaredLayer { marker: i64 }

trait TensorLayer {
    def forward(self, x: DiffTensor) -> DiffTensor effect alloc
}

impl TensorLayer for SquaredLayer {
    def forward(self, x: DiffTensor) -> DiffTensor effect alloc {
        tensor_ad_mul(x, x)
    }
}

def apply_tensor(x: DiffTensor, transform: |DiffTensor| -> DiffTensor effect alloc) -> DiffTensor effect alloc {
    transform(x)
}

def tensor_control_flow_gradient() -> i64 effect alloc {
    val shape: list<i64> = list()
    push(shape, 2);
    val weights_data: list<f64> = list()
    push(weights_data, 1.0);
    push(weights_data, 2.0);
    val factor_data: list<f64> = list()
    push(factor_data, 2.0);
    push(factor_data, 3.0);
    val zero_data: list<f64> = list()
    push(zero_data, 0.0);
    push(zero_data, 0.0);

    val weights = tensor_ad_parameter(tensor_ad_from_data(weights_data, shape))
    val factor = tensor_ad_from_data(factor_data, shape)
    var accumulated = tensor_ad_from_data(zero_data, shape)
    for i in 0..3 {
        accumulated = tensor_ad_add(accumulated, tensor_ad_mul(weights, factor))
    };

    val take_identity = true
    val choose = |value: DiffTensor| if take_identity {
        value
    } else {
        tensor_ad_mul(value, value)
    };
    val selected = apply_tensor(accumulated, choose)
    val layer = SquaredLayer { marker: 0 }
    val output = layer.forward(selected)
    val loss = tensor_ad_sum(output)
    tensor_ad_backward(loss);
    val gradient = tensor_ad_data(tensor_ad_grad(weights))
    if list_get(gradient, 0) == 72.0 && list_get(gradient, 1) == 324.0 { 0 } else { 1 }
}
"#;

const NN_PROGRAM: &str = r#"
bring std.tensor
bring std.nn

def nn_gradient() -> i64 effect alloc {
    val x_shape: list<i64> = list()
    push(x_shape, 1);
    push(x_shape, 2);
    val w_shape: list<i64> = list()
    push(w_shape, 2);
    push(w_shape, 1);
    val y_shape: list<i64> = list()
    push(y_shape, 1);
    push(y_shape, 1);

    val x_data: list<f64> = list()
    push(x_data, 1.0);
    push(x_data, 2.0);
    val w_data: list<f64> = list()
    push(w_data, 3.0);
    push(w_data, 4.0);
    val bias_data: list<f64> = list()
    push(bias_data, 1.0);
    val target_data: list<f64> = list()
    push(target_data, 0.0);

    val x = tensor_ad_from_data(x_data, x_shape)
    val weights = tensor_ad_parameter(tensor_ad_from_data(w_data, w_shape))
    val bias = tensor_ad_from_data(bias_data, y_shape)
    val target = tensor_ad_from_data(target_data, y_shape)
    val prediction = nn_ad_linear(x, weights, bias)
    val loss = nn_ad_squared_error(prediction, target)
    tensor_ad_backward(loss);
    val gradient = tensor_ad_data(tensor_ad_grad(weights))
    if list_get(gradient, 0) == 24.0 && list_get(gradient, 1) == 48.0 { 0 } else { 1 }
}
"#;

fn assert_eval_and_orc(source: &str, module_name: &str, function: &str) {
    let mut path = std::env::temp_dir();
    path.push(format!("iris_{module_name}_{}.iris", std::process::id()));
    std::fs::write(&path, source).expect("write tensor AD fixture");
    let interpreted = compile_file(&path, EmitKind::Eval).expect("interpret tensor AD");
    assert_eq!(interpreted.trim(), "0");
    if !is_orc_jit_available() {
        eprintln!("LLVM ORC unavailable; tensor native parity skipped");
        let _ = std::fs::remove_file(path);
        return;
    }
    let module = compile_file_to_module(&path).expect("compile tensor AD module");
    let engine = OrcJitEngine::new().expect("create tensor AD ORC session");
    let loaded = engine.load_module(&module).expect("load tensor AD module");
    assert_eq!(
        loaded
            .call_i64_0(function)
            .expect("call tensor AD function"),
        0
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn gradients_cross_loops_closures_branches_and_traits() {
    assert_eval_and_orc(
        CONTROL_FLOW_PROGRAM,
        "tensor_control_flow",
        "tensor_control_flow_gradient",
    );
}

#[test]
fn std_nn_linear_and_loss_are_differentiable() {
    assert_eval_and_orc(NN_PROGRAM, "tensor_nn", "nn_gradient");
}

#[test]
fn differentiable_tensor_stdlib_exports_are_registered() {
    let mut path = std::env::temp_dir();
    path.push(format!("iris_tensor_exports_{}.iris", std::process::id()));
    std::fs::write(&path, "bring std.tensor\ndef marker() -> i64 { 0 }").unwrap();
    let module = compile_file_to_module(&path).expect("compile std.tensor exports");
    let names = module
        .functions()
        .iter()
        .map(|function| function.name.as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"marker"), "functions: {names:?}");
    let _ = std::fs::remove_file(path);
}
