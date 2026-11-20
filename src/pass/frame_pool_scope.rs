//! Compiler-enforced frame-pool cleanup.
//!
//! `ui_frame_pool_begin` starts a thread-local arena whose allocations live
//! until `ui_frame_pool_destroy`. Forgetting the destroy on a return path kept
//! every pooled object alive for the rest of the process. This pass treats a
//! begin/destroy pair as a compiler-managed scope: at every return or panic it
//! inserts destroys for any begin whose handle is still open on that path.

use crate::error::PassError;
use crate::ir::instr::IrInstr;
use crate::ir::module::IrModule;
use crate::ir::types::{DType, IrType};
use crate::ir::value::ValueId;
use crate::pass::Pass;

const POOL_BEGIN: &str = "iris_ui_pool_begin";
const POOL_DESTROY: &str = "iris_ui_pool_destroy";

/// Insert missing `iris_ui_pool_destroy` calls at function exits.
pub struct FramePoolScopePass;

impl Pass for FramePoolScopePass {
    fn name(&self) -> &'static str {
        "frame_pool_scope"
    }

    fn run(&mut self, module: &mut IrModule) -> Result<(), PassError> {
        for func in &mut module.functions {
            for bi in 0..func.blocks.len() {
                let instrs = std::mem::take(&mut func.blocks[bi].instrs);
                let mut out = Vec::with_capacity(instrs.len() + 4);
                let mut open: Vec<ValueId> = Vec::new();

                for instr in instrs {
                    let exits_function =
                        matches!(instr, IrInstr::Return { .. } | IrInstr::Panic { .. });
                    if exits_function {
                        while let Some(handle) = open.pop() {
                            out.push(IrInstr::CallExtern {
                                result: None,
                                name: POOL_DESTROY.to_owned(),
                                args: vec![handle],
                                ret_ty: IrType::Scalar(DType::I64),
                            });
                        }
                    }
                    match &instr {
                        IrInstr::CallExtern {
                            name,
                            result: Some(handle),
                            ..
                        } if name == POOL_BEGIN => open.push(*handle),
                        IrInstr::CallExtern { name, args, .. } if name == POOL_DESTROY => {
                            if let Some(&handle) = args.first() {
                                if let Some(pos) = open.iter().rposition(|h| *h == handle) {
                                    open.remove(pos);
                                }
                            }
                        }
                        _ => {}
                    }
                    out.push(instr);
                }
                func.blocks[bi].instrs = out;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::function::Param;
    use crate::ir::module::IrFunctionBuilder;

    fn begin_call(result: ValueId) -> IrInstr {
        IrInstr::CallExtern {
            result: Some(result),
            name: POOL_BEGIN.to_owned(),
            args: vec![],
            ret_ty: IrType::Scalar(DType::I64),
        }
    }

    #[test]
    fn inserts_destroy_before_return_when_begin_is_unmatched() {
        let mut module = IrModule::new("pool_scope_test");
        let mut builder = IrFunctionBuilder::new(
            "frame",
            vec![Param {
                name: "unused".to_owned(),
                ty: IrType::Scalar(DType::I64),
            }],
            IrType::Scalar(DType::I64),
        );
        let entry = builder.create_block(Some("entry"));
        builder.set_current_block(entry);
        let handle = builder.fresh_value();
        builder.push_instr(begin_call(handle), Some(IrType::Scalar(DType::I64)));
        builder.push_instr(
            IrInstr::Return {
                values: vec![handle],
            },
            None,
        );
        module.add_function(builder.build()).expect("add function");

        FramePoolScopePass.run(&mut module).expect("pass runs");
        let func = &module.functions()[0];
        let instrs = &func.blocks()[0].instrs;
        let destroy_pos = instrs
            .iter()
            .position(|i| matches!(i, IrInstr::CallExtern { name, .. } if name == POOL_DESTROY));
        let return_pos = instrs
            .iter()
            .position(|i| matches!(i, IrInstr::Return { .. }));
        assert!(destroy_pos.is_some(), "a missing destroy must be inserted");
        assert!(
            destroy_pos < return_pos,
            "the inserted destroy must run before the return"
        );
    }
}
