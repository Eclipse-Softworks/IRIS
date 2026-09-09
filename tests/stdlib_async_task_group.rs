use iris::compile_to_module;
use iris::ir::types::IrType;

#[test]
fn std_async_task_group_wrapper_uses_the_builtin_nominal_type() {
    let async_stdlib = include_str!("../src/stdlib/async.iris");
    let source = format!(
        "{}\n{}",
        async_stdlib,
        r#"
def main() -> i64 {
    val group: task_group = task_group()
    task_group_join(group);
    0
}
"#
    );
    let module = compile_to_module(&source, "stdlib_async_task_group")
        .expect("std.async must compile with the task_group builtin type");

    let wrapper = module
        .function_by_name("new_task_group")
        .expect("std.async wrapper");
    assert_eq!(wrapper.return_ty, IrType::TaskGroup);
}
