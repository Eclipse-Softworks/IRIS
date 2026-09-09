use iris::codegen::build::execute_binary_for_eval;
use iris::compile_to_module;

#[test]
fn named_function_values_use_the_closure_abi_across_parameters_and_fields() {
    let source = r#"
record Transform { apply: |i64| -> i64 }
def increment(value: i64) -> i64 { return value + 1 }
def subtract(left: i64, right: i64) -> i64 { return left - right }
def call_one(value: i64, callback: |i64| -> i64) -> i64 { return callback(value) }
def call_two(callback: |i64, i64| -> i64) -> i64 { return callback(10, 3) }
def main() -> i64 {
    assert(call_one(41, increment) == 42);
    assert(call_one(-2, increment) == -1);
    assert(call_two(subtract) == 7);
    val transform = Transform { apply: increment };
    assert(transform.apply(9) == 10);
    val lambda = |value: i64| value * 2;
    assert(call_one(21, lambda) == 42);
    return 0
}
"#;
    let module = compile_to_module(source, "named_callbacks").unwrap();
    execute_binary_for_eval(&module).expect("named and lambda callbacks must agree natively");
}
