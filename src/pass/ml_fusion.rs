//! Compile-time ML / tensor IR fusion and algebraic simplification pass.
//!
//! Performs backend-independent tensor graph optimizations at compile time
//! regardless of whether the runtime resolves ML backends via `dlopen` or
//! static linking:
//! - Eliminates identity `Transpose` operations (`axes == [0, 1, ..., n-1]`).
//! - Fuses consecutive `Transpose` operations (`Transpose(Transpose(x))`) into
//!   a single composed `Transpose`, or eliminates them completely when the
//!   composition is the identity permutation.
//! - Collapses consecutive `Reshape` operations (`Reshape(Reshape(x))`),
//!   eliminating the pair when the final type matches `x`'s type.
//! - Cancels double unary negation (`neg(neg(x)) -> x`).

use std::collections::{HashMap, HashSet};

use crate::error::PassError;
use crate::ir::function::IrFunction;
use crate::ir::instr::{IrInstr, TensorOp};
use crate::ir::module::IrModule;
use crate::ir::types::IrType;
use crate::ir::value::ValueId;
use crate::pass::Pass;

/// Compile-time ML/tensor optimization pass (`ml-fusion`).
pub struct MlFusionPass;

impl Pass for MlFusionPass {
    fn name(&self) -> &'static str {
        "ml-fusion"
    }

    fn run(&mut self, module: &mut IrModule) -> Result<(), PassError> {
        for func in &mut module.functions {
            optimize_tensor_ops(func);
        }
        Ok(())
    }
}

#[derive(Clone)]
enum TensorDef {
    Transpose { input: ValueId, axes: Vec<usize> },
    Reshape { input: ValueId },
    Unary { op: String, input: ValueId },
}

fn is_identity_permutation(axes: &[usize]) -> bool {
    !axes.is_empty() && axes.iter().enumerate().all(|(i, &a)| i == a)
}

fn compose_permutations(first: &[usize], second: &[usize]) -> Option<Vec<usize>> {
    if first.len() != second.len() {
        return None;
    }
    let mut out = Vec::with_capacity(second.len());
    for &idx in second {
        out.push(*first.get(idx)?);
    }
    Some(out)
}

fn resolve_root(mut id: ValueId, reps: &HashMap<ValueId, ValueId>) -> ValueId {
    let mut guard = 0usize;
    while let Some(&next) = reps.get(&id) {
        if next == id || guard > 64 {
            break;
        }
        id = next;
        guard += 1;
    }
    id
}

fn collect_known_types(func: &IrFunction) -> HashMap<ValueId, IrType> {
    let mut types = func.value_types.clone();
    for block in &func.blocks {
        for bp in &block.params {
            types.entry(bp.id).or_insert_with(|| bp.ty.clone());
        }
        for instr in &block.instrs {
            if let IrInstr::TensorOp {
                result, result_ty, ..
            } = instr
            {
                types.entry(*result).or_insert_with(|| result_ty.clone());
            }
        }
    }
    types
}

fn optimize_tensor_ops(func: &mut IrFunction) {
    let value_types = collect_known_types(func);
    let mut replacements: HashMap<ValueId, ValueId> = HashMap::new();
    let mut mutated = false;

    for block in &mut func.blocks {
        // Track tensor definitions within this basic block so dominance is guaranteed.
        let mut defs: HashMap<ValueId, TensorDef> = HashMap::new();

        for instr in &mut block.instrs {
            let IrInstr::TensorOp {
                result,
                op,
                inputs,
                result_ty,
            } = instr
            else {
                continue;
            };

            if inputs.len() != 1 {
                continue;
            }
            let src = resolve_root(inputs[0], &replacements);
            inputs[0] = src;

            match op {
                TensorOp::Transpose { axes } => {
                    if is_identity_permutation(axes) {
                        replacements.insert(*result, src);
                        continue;
                    }
                    if let Some(TensorDef::Transpose {
                        input: inner_src,
                        axes: inner_axes,
                    }) = defs.get(&src).cloned()
                    {
                        if let Some(composed) = compose_permutations(&inner_axes, axes) {
                            let root_src = resolve_root(inner_src, &replacements);
                            if is_identity_permutation(&composed) {
                                replacements.insert(*result, root_src);
                                continue;
                            }
                            inputs[0] = root_src;
                            *axes = composed.clone();
                            mutated = true;
                            defs.insert(
                                *result,
                                TensorDef::Transpose {
                                    input: root_src,
                                    axes: composed,
                                },
                            );
                            continue;
                        }
                    }
                    defs.insert(
                        *result,
                        TensorDef::Transpose {
                            input: src,
                            axes: axes.clone(),
                        },
                    );
                }
                TensorOp::Reshape => {
                    if let Some(TensorDef::Reshape { input: inner_src }) = defs.get(&src).cloned() {
                        let root_src = resolve_root(inner_src, &replacements);
                        if value_types
                            .get(&root_src)
                            .is_some_and(|orig_ty| orig_ty == result_ty)
                        {
                            replacements.insert(*result, root_src);
                            continue;
                        }
                        inputs[0] = root_src;
                        mutated = true;
                        defs.insert(*result, TensorDef::Reshape { input: root_src });
                        continue;
                    }
                    defs.insert(*result, TensorDef::Reshape { input: src });
                }
                TensorOp::Unary { op: unary_name } => {
                    if unary_name == "neg" {
                        if let Some(TensorDef::Unary {
                            op: inner_op,
                            input: inner_src,
                        }) = defs.get(&src).cloned()
                        {
                            if inner_op == "neg" {
                                let root_src = resolve_root(inner_src, &replacements);
                                replacements.insert(*result, root_src);
                                continue;
                            }
                        }
                    }
                    defs.insert(
                        *result,
                        TensorDef::Unary {
                            op: unary_name.clone(),
                            input: src,
                        },
                    );
                }
                TensorOp::Einsum { .. } | TensorOp::Reduce { .. } => {}
            }
        }
    }

    if replacements.is_empty() && !mutated {
        return;
    }

    // Resolve transitive replacement chains.
    let keys: Vec<ValueId> = replacements.keys().copied().collect();
    for k in keys {
        let root = resolve_root(k, &replacements);
        replacements.insert(k, root);
    }

    // Rewrite uses across the whole function, then prune replaced pure tensor ops
    // as well as any intermediate tensor ops left dead in the same block.
    let mut used: HashSet<ValueId> = HashSet::new();
    for block in &mut func.blocks {
        for instr in &mut block.instrs {
            crate::pass::opt::apply_replacements(instr, &replacements);
        }
        block.instrs.retain(|instr| {
            instr
                .result()
                .map_or(true, |res| !replacements.contains_key(&res))
        });
        for instr in &block.instrs {
            for op in instr.operands() {
                used.insert(op);
            }
        }
    }
    for block in &mut func.blocks {
        block.instrs.retain(|instr| match instr {
            IrInstr::TensorOp { result, .. } => used.contains(result),
            _ => true,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::function::Param;
    use crate::ir::module::IrFunctionBuilder;
    use crate::ir::types::{DType, Dim, Shape};

    fn tensor_ty(dims: Vec<u64>) -> IrType {
        IrType::Tensor {
            dtype: DType::F32,
            shape: Shape(dims.into_iter().map(Dim::Literal).collect()),
        }
    }

    #[test]
    fn eliminates_double_transpose_and_identity_transpose() {
        let ty = tensor_ty(vec![2, 3]);
        let ty_t = tensor_ty(vec![3, 2]);
        let mut b = IrFunctionBuilder::new(
            "f",
            vec![Param {
                name: "x".to_owned(),
                ty: ty.clone(),
            }],
            ty.clone(),
        );
        let entry = b.create_block(Some("entry"));
        b.set_current_block(entry);
        let x = b.add_block_param(entry, Some("x"), ty.clone());
        let t1 = b.fresh_value();
        b.push_instr(
            IrInstr::TensorOp {
                result: t1,
                op: TensorOp::Transpose { axes: vec![1, 0] },
                inputs: vec![x],
                result_ty: ty_t.clone(),
            },
            Some(ty_t),
        );
        let t2 = b.fresh_value();
        b.push_instr(
            IrInstr::TensorOp {
                result: t2,
                op: TensorOp::Transpose { axes: vec![1, 0] },
                inputs: vec![t1],
                result_ty: ty.clone(),
            },
            Some(ty),
        );
        b.push_instr(IrInstr::Return { values: vec![t2] }, None);

        let mut m = IrModule::new("test");
        m.add_function(b.build()).unwrap();

        MlFusionPass.run(&mut m).unwrap();
        let instrs = &m.functions[0].blocks[0].instrs;
        assert_eq!(instrs.len(), 1);
        assert!(matches!(&instrs[0], IrInstr::Return { values } if values == &[x]));
    }

    #[test]
    fn composes_consecutive_transposes() {
        let ty = tensor_ty(vec![2, 3, 4]);
        let mut b = IrFunctionBuilder::new(
            "f",
            vec![Param {
                name: "x".to_owned(),
                ty: ty.clone(),
            }],
            ty.clone(),
        );
        let entry = b.create_block(Some("entry"));
        b.set_current_block(entry);
        let x = b.add_block_param(entry, Some("x"), ty.clone());
        let t1 = b.fresh_value();
        b.push_instr(
            IrInstr::TensorOp {
                result: t1,
                op: TensorOp::Transpose {
                    axes: vec![1, 2, 0],
                },
                inputs: vec![x],
                result_ty: ty.clone(),
            },
            Some(ty.clone()),
        );
        let t2 = b.fresh_value();
        b.push_instr(
            IrInstr::TensorOp {
                result: t2,
                op: TensorOp::Transpose {
                    axes: vec![0, 2, 1],
                },
                inputs: vec![t1],
                result_ty: ty.clone(),
            },
            Some(ty),
        );
        b.push_instr(IrInstr::Return { values: vec![t2] }, None);

        let mut m = IrModule::new("test");
        m.add_function(b.build()).unwrap();

        MlFusionPass.run(&mut m).unwrap();
        let instrs = &m.functions[0].blocks[0].instrs;
        assert_eq!(instrs.len(), 2);
        match &instrs[0] {
            IrInstr::TensorOp {
                result,
                op: TensorOp::Transpose { axes },
                inputs,
                ..
            } => {
                assert_eq!(*result, t2);
                assert_eq!(inputs, &[x]);
                assert_eq!(axes, &[1, 0, 2]);
            }
            other => panic!("unexpected instruction: {:?}", other),
        }
    }

    #[test]
    fn collapses_redundant_reshape_to_original_type() {
        let ty_2x3 = tensor_ty(vec![2, 3]);
        let ty_6 = tensor_ty(vec![6]);
        let mut b = IrFunctionBuilder::new(
            "f",
            vec![Param {
                name: "x".to_owned(),
                ty: ty_2x3.clone(),
            }],
            ty_2x3.clone(),
        );
        let entry = b.create_block(Some("entry"));
        b.set_current_block(entry);
        let x = b.add_block_param(entry, Some("x"), ty_2x3.clone());
        let r1 = b.fresh_value();
        b.push_instr(
            IrInstr::TensorOp {
                result: r1,
                op: TensorOp::Reshape,
                inputs: vec![x],
                result_ty: ty_6.clone(),
            },
            Some(ty_6),
        );
        let r2 = b.fresh_value();
        b.push_instr(
            IrInstr::TensorOp {
                result: r2,
                op: TensorOp::Reshape,
                inputs: vec![r1],
                result_ty: ty_2x3.clone(),
            },
            Some(ty_2x3),
        );
        b.push_instr(IrInstr::Return { values: vec![r2] }, None);

        let mut m = IrModule::new("test");
        m.add_function(b.build()).unwrap();

        MlFusionPass.run(&mut m).unwrap();
        let instrs = &m.functions[0].blocks[0].instrs;
        assert_eq!(instrs.len(), 1);
        assert!(matches!(&instrs[0], IrInstr::Return { values } if values == &[x]));
    }

    #[test]
    fn eliminates_double_neg_unary() {
        let ty = tensor_ty(vec![4]);
        let mut b = IrFunctionBuilder::new(
            "f",
            vec![Param {
                name: "x".to_owned(),
                ty: ty.clone(),
            }],
            ty.clone(),
        );
        let entry = b.create_block(Some("entry"));
        b.set_current_block(entry);
        let x = b.add_block_param(entry, Some("x"), ty.clone());
        let u1 = b.fresh_value();
        b.push_instr(
            IrInstr::TensorOp {
                result: u1,
                op: TensorOp::Unary {
                    op: "neg".to_owned(),
                },
                inputs: vec![x],
                result_ty: ty.clone(),
            },
            Some(ty.clone()),
        );
        let u2 = b.fresh_value();
        b.push_instr(
            IrInstr::TensorOp {
                result: u2,
                op: TensorOp::Unary {
                    op: "neg".to_owned(),
                },
                inputs: vec![u1],
                result_ty: ty.clone(),
            },
            Some(ty),
        );
        b.push_instr(IrInstr::Return { values: vec![u2] }, None);

        let mut m = IrModule::new("test");
        m.add_function(b.build()).unwrap();

        MlFusionPass.run(&mut m).unwrap();
        let instrs = &m.functions[0].blocks[0].instrs;
        assert_eq!(instrs.len(), 1);
        assert!(matches!(&instrs[0], IrInstr::Return { values } if values == &[x]));
    }
}
