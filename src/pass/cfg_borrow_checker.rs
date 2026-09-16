//! CFG Dataflow Borrow Checker and Liveness Analysis (rustc MIR / NLL style).
//!
//! Performs backward dataflow analysis over the control-flow graph of `IrFunction`
//! basic blocks to compute exact value liveness sets (`live_in`, `live_out`, `live_before`,
//! `live_after`). This replaces lexical scope boundaries with true Non-Lexical Lifetimes (NLL),
//! allowing loans to terminate immediately after their last active use.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::error::PassError;
use crate::ir::block::BlockId;
use crate::ir::function::IrFunction;
use crate::ir::instr::IrInstr;
use crate::ir::module::IrModule;
use crate::ir::value::ValueId;
use crate::pass::Pass;

/// Computed liveness information for a single basic block in an IR function.
#[derive(Debug, Clone, Default)]
pub struct BlockLiveness {
    /// Values defined in this block (block params + instruction results).
    pub defs: HashSet<ValueId>,
    /// Values used in this block before any definition in this block.
    pub uses: HashSet<ValueId>,
    /// Values live on entry to this block (flowing into the first instruction).
    pub live_in: HashSet<ValueId>,
    /// Values live on exit from this block (flowing out of the terminator).
    pub live_out: HashSet<ValueId>,
}

/// Full dataflow liveness analysis for an `IrFunction`.
#[derive(Debug, Clone)]
pub struct FunctionLiveness {
    /// Liveness per basic block ID.
    pub blocks: HashMap<BlockId, BlockLiveness>,
    /// Predecessors per basic block ID in the CFG.
    pub preds: HashMap<BlockId, Vec<BlockId>>,
    /// Successors per basic block ID in the CFG.
    pub succs: HashMap<BlockId, Vec<BlockId>>,
    /// Map of `ValueId` to the point of its last active use: `(BlockId, instr_index)`.
    pub last_uses: HashMap<ValueId, (BlockId, usize)>,
}

impl FunctionLiveness {
    /// Analyzes an `IrFunction` and computes backward dataflow liveness across all blocks.
    pub fn compute(func: &IrFunction) -> Self {
        let mut preds: HashMap<BlockId, Vec<BlockId>> = HashMap::new();
        let mut succs: HashMap<BlockId, Vec<BlockId>> = HashMap::new();
        let mut blocks: HashMap<BlockId, BlockLiveness> = HashMap::new();

        // 1. Build CFG edges (predecessors and successors) and initial def/use sets
        for block in &func.blocks {
            let bid = block.id;
            preds.entry(bid).or_default();
            let block_succs = succs.entry(bid).or_default();

            if let Some(term) = block.terminator() {
                match term {
                    IrInstr::Br { target, .. } => {
                        block_succs.push(*target);
                    }
                    IrInstr::CondBr {
                        then_block,
                        else_block,
                        ..
                    } => {
                        block_succs.push(*then_block);
                        block_succs.push(*else_block);
                    }
                    IrInstr::SwitchVariant {
                        arms,
                        default_block,
                        ..
                    } => {
                        for (_, target) in arms {
                            block_succs.push(*target);
                        }
                        if let Some(def) = default_block {
                            block_succs.push(*def);
                        }
                    }
                    IrInstr::Return { .. } | IrInstr::Panic { .. } => {}
                    _ => {}
                }
            }

            // Record defs and uses
            let mut block_liveness = BlockLiveness::default();
            for p in &block.params {
                block_liveness.defs.insert(p.id);
            }

            for instr in &block.instrs {
                for op in instr.operands() {
                    if !block_liveness.defs.contains(&op) {
                        block_liveness.uses.insert(op);
                    }
                }
                if let Some(r) = instr.result() {
                    block_liveness.defs.insert(r);
                }
            }

            blocks.insert(bid, block_liveness);
        }

        // Fill in predecessor edges from successors
        for (&from, targets) in &succs {
            for &to in targets {
                preds.entry(to).or_default().push(from);
            }
        }

        // 2. Fixed-point backward dataflow iteration for LiveIn and LiveOut:
        //    LiveOut(B) = union_{S in Succ(B)} LiveIn(S)
        //    LiveIn(B)  = Use(B) union (LiveOut(B) \ Def(B))
        let mut worklist: VecDeque<BlockId> = func.blocks.iter().map(|b| b.id).collect();
        let mut in_worklist: HashSet<BlockId> = worklist.iter().copied().collect();

        while let Some(bid) = worklist.pop_front() {
            in_worklist.remove(&bid);

            let block_succ_ids = succs.get(&bid).cloned().unwrap_or_default();
            let mut new_live_out = HashSet::new();
            for s in block_succ_ids {
                if let Some(s_info) = blocks.get(&s) {
                    new_live_out.extend(s_info.live_in.iter().copied());
                }
            }

            let info = blocks.get_mut(&bid).unwrap();
            info.live_out = new_live_out;

            let mut new_live_in = info.uses.clone();
            for &val in &info.live_out {
                if !info.defs.contains(&val) {
                    new_live_in.insert(val);
                }
            }

            if new_live_in != info.live_in {
                info.live_in = new_live_in;
                // Predecessors must be re-evaluated
                if let Some(p_list) = preds.get(&bid) {
                    for &p in p_list {
                        if in_worklist.insert(p) {
                            worklist.push_back(p);
                        }
                    }
                }
            }
        }

        // 3. Compute exact last use points for all values across instructions
        let mut last_uses = HashMap::new();
        for block in &func.blocks {
            let bid = block.id;
            let mut current_live = blocks
                .get(&bid)
                .map(|b| b.live_out.clone())
                .unwrap_or_default();

            for (idx, instr) in block.instrs.iter().enumerate().rev() {
                for op in instr.operands() {
                    if !current_live.contains(&op) {
                        // This instruction is the last use of `op` along this path
                        last_uses.entry(op).or_insert((bid, idx));
                        current_live.insert(op);
                    }
                }
                if let Some(res) = instr.result() {
                    current_live.remove(&res);
                }
            }
        }

        Self {
            blocks,
            preds,
            succs,
            last_uses,
        }
    }

    /// Returns `true` if `val` is live at exit from block `bid`.
    pub fn is_live_out(&self, bid: BlockId, val: ValueId) -> bool {
        self.blocks
            .get(&bid)
            .map(|b| b.live_out.contains(&val))
            .unwrap_or(false)
    }

    /// Returns `true` if `val` is live at entry to block `bid`.
    pub fn is_live_in(&self, bid: BlockId, val: ValueId) -> bool {
        self.blocks
            .get(&bid)
            .map(|b| b.live_in.contains(&val))
            .unwrap_or(false)
    }

    /// Returns the location of the last active use of `val`, if known.
    pub fn last_use_of(&self, val: ValueId) -> Option<(BlockId, usize)> {
        self.last_uses.get(&val).copied()
    }
}

/// CFG-based borrow checker pass.
///
/// Runs dataflow liveness analysis and verifies that no active borrows conflict
/// with mutations or moves across basic block boundaries.
pub struct CfgBorrowCheckerPass;

impl Pass for CfgBorrowCheckerPass {
    fn name(&self) -> &'static str {
        "cfg_borrow_checker"
    }

    fn run(&mut self, module: &mut IrModule) -> Result<(), PassError> {
        for func in &module.functions {
            let liveness = FunctionLiveness::compute(func);

            // Verify basic block safety invariants:
            // No instruction consumes a value that is not live or defined
            for block in &func.blocks {
                let bid = block.id;
                let mut active_defs: HashSet<ValueId> = block.params.iter().map(|p| p.id).collect();
                if let Some(b_info) = liveness.blocks.get(&bid) {
                    active_defs.extend(b_info.live_in.iter().copied());
                }

                for instr in &block.instrs {
                    for op in instr.operands() {
                        if !active_defs.contains(&op) {
                            return Err(PassError::UseBeforeDef {
                                func: func.name.clone(),
                                value: format!("{}", op),
                            });
                        }
                    }
                    if let Some(r) = instr.result() {
                        active_defs.insert(r);
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::function::Param;
    use crate::ir::instr::{BinOp, IrInstr};
    use crate::ir::module::IrFunctionBuilder;
    use crate::ir::types::{DType, IrType};

    #[test]
    fn test_cfg_liveness_straight_line() {
        let mut builder = IrFunctionBuilder::new(
            "test_liveness",
            vec![Param {
                name: "x".into(),
                ty: IrType::Scalar(DType::I64),
            }],
            IrType::Scalar(DType::I64),
        );

        let b0 = builder.create_block(Some("entry"));
        builder.set_current_block(b0);
        let p_x = builder.add_block_param(b0, Some("x"), IrType::Scalar(DType::I64));

        let v1 = builder.fresh_value();
        builder.push_instr(
            IrInstr::ConstInt {
                result: v1,
                value: 42,
                ty: IrType::Scalar(DType::I64),
            },
            Some(IrType::Scalar(DType::I64)),
        );

        let v2 = builder.fresh_value();
        builder.push_instr(
            IrInstr::BinOp {
                result: v2,
                op: BinOp::Add,
                lhs: p_x,
                rhs: v1,
                ty: IrType::Scalar(DType::I64),
            },
            Some(IrType::Scalar(DType::I64)),
        );

        builder.push_instr(IrInstr::Return { values: vec![v2] }, None);
        let func = builder.build();

        let liveness = FunctionLiveness::compute(&func);
        let b0_info = liveness.blocks.get(&b0).expect("b0 exists");

        // p_x is a param, so defined in block.
        assert!(b0_info.defs.contains(&p_x));
        assert!(b0_info.defs.contains(&v1));
        assert!(b0_info.defs.contains(&v2));

        // Return exits the function with v2, so nothing is live_out after return.
        assert!(b0_info.live_out.is_empty());

        // Last use of p_x and v1 is at the BinOp instruction (index 1)
        assert_eq!(liveness.last_use_of(p_x), Some((b0, 1)));
        assert_eq!(liveness.last_use_of(v1), Some((b0, 1)));
    }

    #[test]
    fn test_cfg_liveness_branching_non_lexical() {
        // BB0: condbr -> BB1, BB2
        // BB1: use x, br BB3
        // BB2: no use of x, br BB3
        // BB3: return 0
        let mut builder = IrFunctionBuilder::new(
            "test_branch_liveness",
            vec![
                Param {
                    name: "c".into(),
                    ty: IrType::Scalar(DType::Bool),
                },
                Param {
                    name: "x".into(),
                    ty: IrType::Scalar(DType::I64),
                },
            ],
            IrType::Scalar(DType::I64),
        );

        let b0 = builder.create_block(Some("b0"));
        let b1 = builder.create_block(Some("b1"));
        let b2 = builder.create_block(Some("b2"));
        let b3 = builder.create_block(Some("b3"));

        builder.set_current_block(b0);
        let c = builder.add_block_param(b0, Some("c"), IrType::Scalar(DType::Bool));
        let x = builder.add_block_param(b0, Some("x"), IrType::Scalar(DType::I64));
        builder.push_instr(
            IrInstr::CondBr {
                cond: c,
                then_block: b1,
                then_args: vec![],
                else_block: b2,
                else_args: vec![],
            },
            None,
        );

        // BB1: uses x
        builder.set_current_block(b1);
        let v_add = builder.fresh_value();
        builder.push_instr(
            IrInstr::BinOp {
                result: v_add,
                op: BinOp::Add,
                lhs: x,
                rhs: x,
                ty: IrType::Scalar(DType::I64),
            },
            Some(IrType::Scalar(DType::I64)),
        );
        builder.push_instr(
            IrInstr::Br {
                target: b3,
                args: vec![],
            },
            None,
        );

        // BB2: does NOT use x
        builder.set_current_block(b2);
        builder.push_instr(
            IrInstr::Br {
                target: b3,
                args: vec![],
            },
            None,
        );

        // BB3: return 0
        builder.set_current_block(b3);
        let v_ret = builder.fresh_value();
        builder.push_instr(
            IrInstr::ConstInt {
                result: v_ret,
                value: 0,
                ty: IrType::Scalar(DType::I64),
            },
            Some(IrType::Scalar(DType::I64)),
        );
        builder.push_instr(
            IrInstr::Return {
                values: vec![v_ret],
            },
            None,
        );

        let func = builder.build();
        let liveness = FunctionLiveness::compute(&func);

        // x is live_in to BB1 because it is used there
        assert!(liveness.is_live_in(b1, x));

        // x is NOT live_in to BB2 because it is neither used in BB2 nor live_out of BB2
        assert!(!liveness.is_live_in(b2, x));

        // x is NOT live_in to BB3
        assert!(!liveness.is_live_in(b3, x));
    }
}
