//! Explicit drop-scope elaboration pass.
//!
//! Lowers explicit [`IrInstr::Drop`] instructions into concrete destructor
//! [`IrInstr::Call`] instructions and strips [`IrInstr::EnterScope`] /
//! [`IrInstr::ExitScope`] markers once `ValidatePass` and
//! `CfgBorrowCheckerPass` have inspected the CFG.

use std::collections::HashMap;

use crate::error::PassError;
use crate::ir::instr::{InstrId, IrInstr};
use crate::ir::module::IrModule;
use crate::ir::types::IrType;
use crate::ir::value::ValueDef;
use crate::pass::Pass;

/// Elaborates explicit [`IrInstr::Drop`] instructions into [`IrInstr::Call`]
/// and strips lexical scope markers (`EnterScope` / `ExitScope`).
pub struct DropElaborationPass;

impl Pass for DropElaborationPass {
    fn name(&self) -> &'static str {
        "drop_elaboration"
    }

    fn run(&mut self, module: &mut IrModule) -> Result<(), PassError> {
        let return_types: HashMap<String, IrType> = module
            .functions
            .iter()
            .map(|f| (f.name.clone(), f.return_ty.clone()))
            .collect();

        for func in &mut module.functions {
            for bi in 0..func.blocks.len() {
                let instrs = std::mem::take(&mut func.blocks[bi].instrs);
                let mut rewritten = Vec::with_capacity(instrs.len());
                for instr in instrs {
                    match instr {
                        IrInstr::EnterScope { .. } | IrInstr::ExitScope { .. } => {}
                        IrInstr::Drop {
                            value,
                            drop_fn,
                            ty: _,
                        } => {
                            let ret_ty = return_types
                                .get(&drop_fn)
                                .cloned()
                                .unwrap_or_else(|| IrType::Tuple(Vec::new()));
                            let res = func.fresh_value();
                            let tid = func.type_arena.intern_ref(&ret_ty);
                            let block_id = func.blocks[bi].id;
                            let instr_id = InstrId(rewritten.len() as u32);
                            func.value_defs.insert(
                                res,
                                ValueDef::InstrResult {
                                    block: block_id,
                                    instr: instr_id,
                                },
                            );
                            func.value_type_ids.insert(res, tid);
                            func.value_types.insert(res, ret_ty.clone());
                            rewritten.push(IrInstr::Call {
                                result: Some(res),
                                callee: drop_fn,
                                args: vec![value],
                                result_ty: Some(ret_ty),
                            });
                        }
                        other => rewritten.push(other),
                    }
                }
                func.blocks[bi].instrs = rewritten;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::block::BlockId;
    use crate::ir::function::{DropBinding, Param};
    use crate::ir::module::IrFunctionBuilder;
    use crate::ir::types::DType;

    #[test]
    fn elaborates_explicit_drop_scopes_to_calls_and_strips_markers() {
        let mut module = IrModule::new("drop_elab_test");
        let unit = IrType::Tuple(Vec::new());
        let struct_ty = IrType::Struct {
            name: "Res".to_string(),
            fields: vec![("id".to_string(), IrType::Scalar(DType::I64))],
        };

        // Destructor function
        let mut dtor = IrFunctionBuilder::new(
            "Res__drop",
            vec![Param {
                name: "self".to_string(),
                ty: struct_ty.clone(),
            }],
            unit.clone(),
        );
        let d_entry = dtor.create_block(Some("entry"));
        dtor.set_current_block(d_entry);
        let _self_v = dtor.add_block_param(d_entry, Some("self"), struct_ty.clone());
        let u = dtor.fresh_value();
        dtor.push_instr(
            IrInstr::MakeTuple {
                result: u,
                elements: vec![],
                result_ty: unit.clone(),
            },
            Some(unit.clone()),
        );
        dtor.push_instr(IrInstr::Return { values: vec![u] }, None);
        dtor.seal_unterminated_blocks();
        module.add_function(dtor.build()).unwrap();

        // Caller with explicit EnterScope / Drop / ExitScope
        let mut main_b = IrFunctionBuilder::new("main", vec![], IrType::Scalar(DType::I64));
        let entry = main_b.create_block(Some("entry"));
        main_b.set_current_block(entry);
        let scope = main_b.alloc_drop_scope(None);
        main_b.push_instr(IrInstr::EnterScope { scope }, None);
        let fld = main_b.fresh_value();
        main_b.push_instr(
            IrInstr::ConstInt {
                result: fld,
                value: 1,
                ty: IrType::Scalar(DType::I64),
            },
            Some(IrType::Scalar(DType::I64)),
        );
        let res_v = main_b.fresh_value();
        main_b.push_instr(
            IrInstr::MakeStruct {
                result: res_v,
                fields: vec![fld],
                result_ty: struct_ty.clone(),
            },
            Some(struct_ty.clone()),
        );
        main_b.register_drop_binding(
            scope,
            DropBinding {
                value: res_v,
                var_name: "r".to_string(),
                drop_fn: "Res__drop".to_string(),
                ty: struct_ty.clone(),
            },
        );
        main_b.push_instr(
            IrInstr::Drop {
                value: res_v,
                drop_fn: "Res__drop".to_string(),
                ty: struct_ty,
            },
            None,
        );
        main_b.push_instr(IrInstr::ExitScope { scope }, None);
        main_b.push_instr(IrInstr::Return { values: vec![fld] }, None);
        main_b.seal_unterminated_blocks();
        let main_id = module.add_function(main_b.build()).unwrap();

        let mut pass = DropElaborationPass;
        pass.run(&mut module).unwrap();

        let main_fn = module.function(main_id).unwrap();
        assert_eq!(main_fn.drop_scopes.len(), 1);
        let instrs = &main_fn.block(BlockId(0)).unwrap().instrs;
        assert!(!instrs.iter().any(|i| matches!(
            i,
            IrInstr::EnterScope { .. } | IrInstr::ExitScope { .. } | IrInstr::Drop { .. }
        )));
        assert!(instrs
            .iter()
            .any(|i| matches!(i, IrInstr::Call { callee, .. } if callee == "Res__drop")));
    }
}
