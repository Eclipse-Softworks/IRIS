//! Strength reduction optimizer: replaces expensive ops with cheaper equivalents.
//!
//! Transformations:
//! - `x - x`   → `0`       (subtraction of identical values)
//!
//! `x * 2^n → x << n` and `x / 2^n → x >> n` were removed: the shift drops
//! the overflow check, and floors where division truncates. LLVM does both
//! reductions itself where they are sound.
//!
//! Integers only. For floats `x - x` is NaN when `x` is NaN or infinite, and
//! the zero this pass wrote was an integer constant of a float type, which
//! the LLVM backend emitted as `double 0` and clang rejected.

use crate::error::PassError;
use crate::ir::function::IrFunction;
use crate::ir::instr::{BinOp, IrInstr};
use crate::ir::module::IrModule;
use crate::ir::types::IrType;
use crate::pass::Pass;

pub struct StrengthReducePass;

impl Pass for StrengthReducePass {
    fn name(&self) -> &'static str {
        "strength-reduce"
    }

    fn run(&mut self, module: &mut IrModule) -> Result<(), PassError> {
        for func in &mut module.functions {
            strength_reduce_func(func);
        }
        Ok(())
    }
}

fn strength_reduce_func(func: &mut IrFunction) {
    for block in &mut func.blocks {
        let mut new_instrs = Vec::new();
        for instr in block.instrs.drain(..) {
            match &instr {
                IrInstr::BinOp {
                    result,
                    op,
                    lhs,
                    rhs,
                    ty,
                } => {
                    let integer = matches!(ty, IrType::Scalar(d) if d.is_integer());
                    if !integer {
                        new_instrs.push(instr);
                        continue;
                    }
                    let ty = ty.clone();
                    let result = *result;
                    let lhs = *lhs;
                    let rhs = *rhs;

                    match op {
                        BinOp::Mul => {
                            // `x * 2^n → x << n` has been removed, like the
                            // division rewrite below. Integer multiplication is
                            // checked -- an overflow stops the program with
                            // E0409 -- and a shift wraps silently, so the
                            // rewrite turned `BIG * 2` into a wrong answer
                            // whenever the factor was a visible constant.
                            // LLVM reduces a checked multiply by a constant
                            // itself.
                            new_instrs.push(IrInstr::BinOp {
                                result,
                                op: BinOp::Mul,
                                lhs,
                                rhs,
                                ty,
                            });
                        }
                        BinOp::Div => {
                            // `x / 2^n → x >> n` is NOT a valid rewrite for signed
                            // integers and has been removed.
                            //
                            // An arithmetic shift right *floors*; IRIS division
                            // truncates toward zero (`wrapping_div`, matching the
                            // interpreter and the C runtime). They differ for every
                            // negative dividend that does not divide exactly:
                            //
                            //     -7 / 2  == -3      (truncate — correct)
                            //     -7 >> 1 == -4      (floor    — what this emitted)
                            //
                            // The rewrite therefore silently changed results, and
                            // only when the divisor happened to be a visible
                            // constant. Whether it fired depended on inlining, so
                            // the same expression gave -3 through a real call and
                            // -4 once the callee was small enough to inline. It also
                            // broke the identity `(a/b)*b + (a%b) == a`, since `%`
                            // was left truncating.
                            //
                            // Recovering the optimisation would need the usual bias
                            // correction (add `(x >> 63) >>> (64 - n)` before
                            // shifting), or a proof that `x >= 0`. Neither is worth
                            // doing here: codegen emits LLVM `sdiv`, and LLVM
                            // performs this strength reduction itself, correctly,
                            // whenever it is sound.
                            new_instrs.push(IrInstr::BinOp {
                                result,
                                op: BinOp::Div,
                                lhs,
                                rhs,
                                ty,
                            });
                        }
                        BinOp::Sub => {
                            // x - x → 0
                            if lhs == rhs {
                                new_instrs.push(IrInstr::ConstInt {
                                    result,
                                    value: 0,
                                    ty,
                                });
                                continue;
                            }
                            new_instrs.push(IrInstr::BinOp {
                                result,
                                op: BinOp::Sub,
                                lhs,
                                rhs,
                                ty,
                            });
                        }
                        _ => {
                            new_instrs.push(IrInstr::BinOp {
                                result,
                                op: *op,
                                lhs,
                                rhs,
                                ty,
                            });
                        }
                    }
                }
                _ => new_instrs.push(instr),
            }
        }
        block.instrs = new_instrs;
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::module::{IrFunctionBuilder, IrModule};
    use crate::ir::types::{DType, IrType};
    use crate::pass::Pass;

    fn i64_ty() -> IrType {
        IrType::Scalar(DType::I64)
    }

    /// Build `x * rhs_const` and return the module.
    fn build_mul_module(rhs_val: i64) -> IrModule {
        let mut m = IrModule::new("test");
        let mut builder = IrFunctionBuilder::new("f", vec![], i64_ty());
        let entry = builder.create_block(Some("entry"));
        builder.set_current_block(entry);

        let x = builder.fresh_value(); // %0
        builder.push_instr(
            IrInstr::ConstInt {
                result: x,
                value: 7,
                ty: i64_ty(),
            },
            Some(i64_ty()),
        );
        let c = builder.fresh_value(); // %1
        builder.push_instr(
            IrInstr::ConstInt {
                result: c,
                value: rhs_val,
                ty: i64_ty(),
            },
            Some(i64_ty()),
        );
        let r = builder.fresh_value(); // %2
        builder.push_instr(
            IrInstr::BinOp {
                result: r,
                op: BinOp::Mul,
                lhs: x,
                rhs: c,
                ty: i64_ty(),
            },
            Some(i64_ty()),
        );
        builder.push_instr(IrInstr::Return { values: vec![r] }, None);
        m.add_function(builder.build()).unwrap();
        m
    }

    /// Build `x / rhs_const` and return the module.
    fn build_div_module(rhs_val: i64) -> IrModule {
        let mut m = IrModule::new("test");
        let mut builder = IrFunctionBuilder::new("f", vec![], i64_ty());
        let entry = builder.create_block(Some("entry"));
        builder.set_current_block(entry);

        let x = builder.fresh_value();
        builder.push_instr(
            IrInstr::ConstInt {
                result: x,
                value: 100,
                ty: i64_ty(),
            },
            Some(i64_ty()),
        );
        let c = builder.fresh_value();
        builder.push_instr(
            IrInstr::ConstInt {
                result: c,
                value: rhs_val,
                ty: i64_ty(),
            },
            Some(i64_ty()),
        );
        let r = builder.fresh_value();
        builder.push_instr(
            IrInstr::BinOp {
                result: r,
                op: BinOp::Div,
                lhs: x,
                rhs: c,
                ty: i64_ty(),
            },
            Some(i64_ty()),
        );
        builder.push_instr(IrInstr::Return { values: vec![r] }, None);
        m.add_function(builder.build()).unwrap();
        m
    }

    /// Build `x - x` and return the module.
    fn build_sub_self_module() -> IrModule {
        let mut m = IrModule::new("test");
        let mut builder = IrFunctionBuilder::new("f", vec![], i64_ty());
        let entry = builder.create_block(Some("entry"));
        builder.set_current_block(entry);

        let x = builder.fresh_value();
        builder.push_instr(
            IrInstr::ConstInt {
                result: x,
                value: 42,
                ty: i64_ty(),
            },
            Some(i64_ty()),
        );
        let r = builder.fresh_value();
        builder.push_instr(
            IrInstr::BinOp {
                result: r,
                op: BinOp::Sub,
                lhs: x,
                rhs: x,
                ty: i64_ty(),
            },
            Some(i64_ty()),
        );
        builder.push_instr(IrInstr::Return { values: vec![r] }, None);
        m.add_function(builder.build()).unwrap();
        m
    }

    #[test]
    fn pass_name() {
        let pass = StrengthReducePass;
        assert_eq!(pass.name(), "strength-reduce");
    }

    #[test]
    fn mul_by_power_of_two_is_not_reduced_to_shl() {
        // A shift would drop the overflow check: `x * 8` must stay a multiply.
        let mut m = build_mul_module(8);
        StrengthReducePass.run(&mut m).unwrap();

        let block = &m.functions()[0].blocks()[0];
        assert!(block
            .instrs
            .iter()
            .any(|i| matches!(i, IrInstr::BinOp { op: BinOp::Mul, .. })));
        assert!(!block
            .instrs
            .iter()
            .any(|i| matches!(i, IrInstr::BinOp { op: BinOp::Shl, .. })));
    }

    #[test]
    fn mul_by_non_power_of_two_not_reduced() {
        let mut m = build_mul_module(7);
        StrengthReducePass.run(&mut m).unwrap();

        let block = &m.functions()[0].blocks()[0];
        let has_mul = block
            .instrs
            .iter()
            .any(|i| matches!(i, IrInstr::BinOp { op: BinOp::Mul, .. }));
        assert!(has_mul, "mul by 7 should NOT be reduced");
    }

    #[test]
    fn mul_by_one_not_reduced() {
        let mut m = build_mul_module(1);
        StrengthReducePass.run(&mut m).unwrap();

        let block = &m.functions()[0].blocks()[0];
        assert!(!block
            .instrs
            .iter()
            .any(|i| matches!(i, IrInstr::BinOp { op: BinOp::Shl, .. })));
    }

    #[test]
    fn div_by_power_of_two_is_not_reduced_to_shr() {
        // This test previously asserted the opposite, and the behaviour it
        // asserted was a miscompilation.
        //
        // `x / 2^n → x >> n` holds only for non-negative dividends. An
        // arithmetic shift right floors; IRIS division truncates toward zero.
        // So -7 / 2 is -3, while -7 >> 1 is -4 — and because the rewrite only
        // fired when the divisor was a visible constant, whether you got the
        // right answer depended on inlining. It also broke the identity
        // (a/b)*b + (a%b) == a, since `%` was left truncating.
        //
        // Codegen emits LLVM `sdiv`, and LLVM performs this reduction itself,
        // correctly, whenever it is sound. See known-issues #19.
        let mut m = build_div_module(4); // 4 = 2^2
        StrengthReducePass.run(&mut m).unwrap();

        let block = &m.functions()[0].blocks()[0];
        let has_shr = block
            .instrs
            .iter()
            .any(|i| matches!(i, IrInstr::BinOp { op: BinOp::Shr, .. }));
        assert!(
            !has_shr,
            "signed div must NOT be rewritten to shr — it floors where division truncates"
        );

        let has_div = block
            .instrs
            .iter()
            .any(|i| matches!(i, IrInstr::BinOp { op: BinOp::Div, .. }));
        assert!(has_div, "the division must survive the pass intact");
    }

    #[test]
    fn div_by_non_power_of_two_not_reduced() {
        let mut m = build_div_module(3);
        StrengthReducePass.run(&mut m).unwrap();

        let block = &m.functions()[0].blocks()[0];
        let has_div = block
            .instrs
            .iter()
            .any(|i| matches!(i, IrInstr::BinOp { op: BinOp::Div, .. }));
        assert!(has_div, "div by 3 should NOT be reduced");
    }

    #[test]
    fn sub_self_reduced_to_zero() {
        let mut m = build_sub_self_module();
        StrengthReducePass.run(&mut m).unwrap();

        let block = &m.functions()[0].blocks()[0];
        // x - x should become ConstInt(0)
        let no_sub = !block
            .instrs
            .iter()
            .any(|i| matches!(i, IrInstr::BinOp { op: BinOp::Sub, .. }));
        assert!(no_sub, "sub self should be removed");
        // Should have a ConstInt with value 0 for the result
        let has_zero = block
            .instrs
            .iter()
            .any(|i| matches!(i, IrInstr::ConstInt { value: 0, .. }));
        assert!(has_zero, "sub self should produce const 0");
    }

    #[test]
    fn float_sub_self_is_kept() {
        // x - x is NaN for a NaN or infinite x, and the integer zero the pass
        // used to write had a float type that LLVM rejected (`double 0`).
        let f64_ty = IrType::Scalar(DType::F64);
        let mut m = IrModule::new("test");
        let mut builder = IrFunctionBuilder::new("f", vec![], f64_ty.clone());
        let entry = builder.create_block(Some("entry"));
        builder.set_current_block(entry);
        let x = builder.fresh_value();
        builder.push_instr(
            IrInstr::ConstFloat {
                result: x,
                value: f64::INFINITY,
                ty: f64_ty.clone(),
            },
            Some(f64_ty.clone()),
        );
        let r = builder.fresh_value();
        builder.push_instr(
            IrInstr::BinOp {
                result: r,
                op: BinOp::Sub,
                lhs: x,
                rhs: x,
                ty: f64_ty.clone(),
            },
            Some(f64_ty),
        );
        builder.push_instr(IrInstr::Return { values: vec![r] }, None);
        m.add_function(builder.build()).unwrap();
        StrengthReducePass.run(&mut m).unwrap();

        let block = &m.functions()[0].blocks()[0];
        assert!(block
            .instrs
            .iter()
            .any(|i| matches!(i, IrInstr::BinOp { op: BinOp::Sub, .. })));
        assert!(!block
            .instrs
            .iter()
            .any(|i| matches!(i, IrInstr::ConstInt { .. })));
    }

    #[test]
    fn strength_reduce_preserves_other_ops() {
        // Add should not be touched
        let mut m = IrModule::new("test");
        let mut builder = IrFunctionBuilder::new("f", vec![], i64_ty());
        let entry = builder.create_block(Some("entry"));
        builder.set_current_block(entry);

        let a = builder.fresh_value();
        builder.push_instr(
            IrInstr::ConstInt {
                result: a,
                value: 3,
                ty: i64_ty(),
            },
            Some(i64_ty()),
        );
        let b = builder.fresh_value();
        builder.push_instr(
            IrInstr::ConstInt {
                result: b,
                value: 4,
                ty: i64_ty(),
            },
            Some(i64_ty()),
        );
        let r = builder.fresh_value();
        builder.push_instr(
            IrInstr::BinOp {
                result: r,
                op: BinOp::Add,
                lhs: a,
                rhs: b,
                ty: i64_ty(),
            },
            Some(i64_ty()),
        );
        builder.push_instr(IrInstr::Return { values: vec![r] }, None);
        m.add_function(builder.build()).unwrap();

        StrengthReducePass.run(&mut m).unwrap();

        let block = &m.functions()[0].blocks()[0];
        let has_add = block
            .instrs
            .iter()
            .any(|i| matches!(i, IrInstr::BinOp { op: BinOp::Add, .. }));
        assert!(has_add, "add should be preserved");
    }
}
