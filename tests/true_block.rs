//! End-to-end tests for the structured "true block" IR construct.
//!
//! A source-level `{ ... }` block must survive lowering as real
//! `EnterBlock` / `ExitBlock` markers, round-trip through binary IR
//! serialization, and be observed by the interpreter.

use iris::interp::{eval_function, IrValue};
use iris::ir::function::ScopeId;
use iris::ir::instr::IrInstr;
use iris::ir::module::IrFunctionBuilder;
use iris::ir::types::{DType, IrType};
use iris::{
    compile, compile_to_module, deserialize_module, eval_ir_module, serialize_module, EmitKind,
};

const NESTED_BLOCKS: &str = r#"
def main() -> i64 {
    val x = { val a = 10; a }
    val y = { val b = 20; { val c = 12; b + c } }
    x + y
}
"#;

#[test]
fn block_expressions_emit_structured_scope_markers() {
    let ir = compile(NESTED_BLOCKS, "true_block_ir", EmitKind::Ir)
        .expect("compilation with block expressions must succeed");
    let enters = ir.matches("enter_block").count();
    let exits = ir.matches("exit_block").count();
    assert!(enters >= 3, "expected 3+ enter_block markers, got {enters}");
    assert_eq!(enters, exits, "block scope markers must be balanced");
}

#[test]
fn block_markers_survive_serialization_round_trip() {
    let module = compile_to_module(NESTED_BLOCKS, "true_block_serde").expect("compile");
    let original = iris::codegen::printer::emit_ir_text(&module).expect("print original");
    let bytes = serialize_module(&module);
    let restored = deserialize_module(&bytes).expect("deserialize");
    let after = iris::codegen::printer::emit_ir_text(&restored).expect("print restored");

    assert_eq!(
        original.matches("enter_block").count(),
        after.matches("enter_block").count(),
        "enter_block markers must survive serialization"
    );
    assert_eq!(
        original.matches("exit_block").count(),
        after.matches("exit_block").count(),
        "exit_block markers must survive serialization"
    );
}

#[test]
fn block_eval_matches_after_round_trip() {
    let module = compile_to_module(NESTED_BLOCKS, "true_block_eval").expect("compile");
    let expected = eval_ir_module(&module).expect("eval original");
    assert_eq!(expected.trim(), "42", "nested block values must compose");

    let bytes = serialize_module(&module);
    let restored = deserialize_module(&bytes).expect("deserialize");
    let actual = eval_ir_module(&restored).expect("eval restored");
    assert_eq!(actual.trim(), expected.trim());
}

#[test]
fn interpreter_rejects_unmatched_exit_block() {
    // Hand-build IR that exits a block scope without ever entering it. This is
    // malformed IR; the interpreter must catch it rather than silently accept
    // an unbalanced structured block.
    let mut builder = IrFunctionBuilder::new("bad", vec![], IrType::Scalar(DType::I64));
    let entry = builder.create_block(Some("entry"));
    builder.set_current_block(entry);
    builder.push_instr(IrInstr::ExitBlock { scope: ScopeId(7) }, None);
    let zero = builder.fresh_value();
    builder.push_instr(
        IrInstr::ConstInt {
            result: zero,
            value: 0,
            ty: IrType::Scalar(DType::I64),
        },
        Some(IrType::Scalar(DType::I64)),
    );
    builder.push_instr(IrInstr::Return { values: vec![zero] }, None);
    let func = builder.build();

    let result = eval_function(&func, &[] as &[IrValue]);
    assert!(
        result.is_err(),
        "an ExitBlock without a matching EnterBlock must be rejected"
    );
}
