//! Type consistency checking pass.
//!
//! For bootstrap: validates that type constraints are locally consistent
//! without doing full unification. A full unification-based inference engine
//! is the natural next step once the IR stabilizes.

use crate::error::PassError;
use crate::ir::instr::{IrInstr, ScalarUnaryOp, TensorOp};
use crate::ir::module::IrModule;
use crate::ir::types::{DType, IrType};
use crate::pass::Pass;

fn same_nominal_type(a: &IrType, b: &IrType) -> bool {
    match (a, b) {
        (IrType::Struct { name: na, .. }, IrType::Struct { name: nb, .. }) => na == nb,
        (IrType::Enum { name: na, .. }, IrType::Enum { name: nb, .. }) => na == nb,
        (IrType::TraitObject { name: na, .. }, IrType::TraitObject { name: nb, .. }) => na == nb,
        _ => false,
    }
}

/// Checks that tensor operation result types are consistent with their inputs,
/// and that binary operations do not mix incompatible types: a consistency
/// check of what lowering produced (every value already typed), not inference.
pub struct TypeInferPass;

impl Pass for TypeInferPass {
    fn name(&self) -> &'static str {
        "type-infer"
    }

    fn run(&mut self, module: &mut IrModule) -> Result<(), PassError> {
        for func in module.functions() {
            for (bi, block) in func.blocks().iter().enumerate() {
                for (ii, instr) in block.instrs.iter().enumerate() {
                    let at = |message: String| located(func, bi, ii, instr, message);
                    match instr {
                        IrInstr::BinOp { lhs, rhs, .. } => {
                            let lhs_ty = func.value_type(*lhs);
                            let rhs_ty = func.value_type(*rhs);
                            match (lhs_ty, rhs_ty) {
                                (Some(l), Some(r)) if l != r => {
                                    let is_grad_mixed = (matches!(l, IrType::Grad(_))
                                        && matches!(r, IrType::Scalar(_)))
                                        || (matches!(l, IrType::Scalar(_))
                                            && matches!(r, IrType::Grad(_)));
                                    let is_sparse_mixed = (matches!(l, IrType::Sparse(_))
                                        && matches!(r, IrType::Scalar(_)))
                                        || (matches!(l, IrType::Scalar(_))
                                            && matches!(r, IrType::Sparse(_)));
                                    if !is_grad_mixed && !is_sparse_mixed {
                                        return Err(at(format!(
                                            "cannot combine `{}` and `{}`",
                                            src(l),
                                            src(r)
                                        )));
                                    }
                                }
                                _ => {}
                            }
                        }

                        IrInstr::UnaryOp { op, operand, .. } => {
                            if let Some(ty) = func.value_type(*operand) {
                                match op {
                                    ScalarUnaryOp::Neg => {
                                        if !matches!(
                                            ty,
                                            IrType::Scalar(
                                                DType::F32
                                                    | DType::F64
                                                    | DType::I32
                                                    | DType::I64
                                                    | DType::I8
                                            ) | IrType::Grad(_)
                                                | IrType::Tensor { .. }
                                        ) {
                                            return Err(at(format!(
                                                "`-` needs a number, but this is `{}`",
                                                src(ty)
                                            )));
                                        }
                                    }
                                    ScalarUnaryOp::Not => {
                                        if !matches!(ty, IrType::Scalar(DType::Bool)) {
                                            return Err(at(format!(
                                                "`!` needs a `bool`, but this is `{}`",
                                                src(ty)
                                            )));
                                        }
                                    }
                                    // Math builtins — accept any numeric scalar.
                                    ScalarUnaryOp::Sqrt
                                    | ScalarUnaryOp::Abs
                                    | ScalarUnaryOp::Floor
                                    | ScalarUnaryOp::Ceil
                                    | ScalarUnaryOp::Sin
                                    | ScalarUnaryOp::Cos
                                    | ScalarUnaryOp::Tan
                                    | ScalarUnaryOp::Exp
                                    | ScalarUnaryOp::Log
                                    | ScalarUnaryOp::Log2
                                    | ScalarUnaryOp::Round
                                    | ScalarUnaryOp::Sign => {
                                        if !matches!(
                                            ty,
                                            IrType::Scalar(
                                                DType::F32
                                                    | DType::F64
                                                    | DType::I32
                                                    | DType::I64
                                                    | DType::U8
                                                    | DType::I8
                                                    | DType::U32
                                                    | DType::U64
                                                    | DType::USize
                                            )
                                        ) {
                                            return Err(at(format!(
                                                "`{}` needs a number, but this is `{}`",
                                                format!("{:?}", op).to_lowercase(),
                                                src(ty)
                                            )));
                                        }
                                    }
                                    // BitNot — integer scalars only.
                                    ScalarUnaryOp::BitNot => {
                                        if !matches!(
                                            ty,
                                            IrType::Scalar(
                                                DType::I32
                                                    | DType::I64
                                                    | DType::U8
                                                    | DType::I8
                                                    | DType::U32
                                                    | DType::U64
                                                    | DType::USize
                                            )
                                        ) {
                                            return Err(at(format!(
                                                "`~` needs an integer, but this is `{}`",
                                                src(ty)
                                            )));
                                        }
                                    }
                                }
                            }
                        }

                        IrInstr::TensorOp {
                            op: TensorOp::Einsum { notation: _ },
                            inputs,
                            result_ty,
                            ..
                        } => {
                            // Validate all inputs are tensor types.
                            for &input in inputs {
                                if let Some(ty) = func.value_type(input) {
                                    if !matches!(ty, IrType::Tensor { .. }) {
                                        return Err(at(format!(
                                            "`einsum` input {} must be a tensor, but it is `{}`",
                                            input,
                                            src(ty)
                                        )));
                                    }
                                }
                            }

                            // Result must also be a tensor.
                            if !matches!(result_ty, IrType::Tensor { .. }) {
                                return Err(at(format!(
                                    "`einsum` must produce a tensor, not `{}`",
                                    src(result_ty)
                                )));
                            }
                        }

                        IrInstr::Load { result_ty, .. } => {
                            // Load result must be a scalar.
                            if !matches!(result_ty, IrType::Scalar(_)) {
                                return Err(at(format!(
                                    "loading this element gives `{}`, not a number",
                                    src(result_ty)
                                )));
                            }
                        }

                        IrInstr::Cast { to_ty, .. } => {
                            // Cast result must be a scalar type.
                            if !matches!(to_ty, IrType::Scalar(_)) {
                                return Err(at(format!(
                                    "`to` converts between number types, not to `{}`",
                                    src(to_ty)
                                )));
                            }
                        }

                        IrInstr::GetField {
                            base, field_index, ..
                        } => {
                            if let Some(ty) = func.value_type(*base) {
                                if !matches!(ty, IrType::Struct { .. }) {
                                    return Err(at(format!(
                                        "`{}` has no fields: only records do",
                                        src(ty)
                                    )));
                                }
                            }
                            let _ = field_index;
                        }

                        IrInstr::ArrayLoad { array, .. } => {
                            if let Some(ty) = func.value_type(*array) {
                                if !matches!(ty, IrType::Array { .. } | IrType::Slice { .. }) {
                                    return Err(at(format!("only arrays and slices are indexed this way, but this is `{}`", src(ty))));
                                }
                            }
                        }

                        IrInstr::ArrayStore { array, .. } => {
                            if let Some(ty) = func.value_type(*array) {
                                if !matches!(ty, IrType::Array { .. } | IrType::Slice { .. }) {
                                    return Err(at(format!("only arrays and slices are indexed this way, but this is `{}`", src(ty))));
                                }
                            }
                        }

                        IrInstr::CellLoad { cell, .. } | IrInstr::CellStore { cell, .. } => {
                            if let Some(ty) = func.value_type(*cell) {
                                if !matches!(ty, IrType::MutRef(_) | IrType::Infer) {
                                    return Err(at(format!(
                                        "cannot dereference `{}`: expected `&mut`",
                                        src(ty)
                                    )));
                                }
                            }
                        }

                        IrInstr::GetElement { base, .. } => {
                            if let Some(ty) = func.value_type(*base) {
                                if !matches!(ty, IrType::Tuple(_) | IrType::Struct { .. }) {
                                    return Err(at(format!(
                                        "`.N` takes an element of a tuple, but this is `{}`",
                                        src(ty)
                                    )));
                                }
                            }
                        }

                        IrInstr::Return { values } => {
                            // Check single-value returns match the declared return type.
                            if values.len() == 1 {
                                if let (Some(val_ty), ret_ty) =
                                    (func.value_type(values[0]), &func.return_ty)
                                {
                                    if val_ty != ret_ty && !same_nominal_type(val_ty, ret_ty) {
                                        return Err(at(format!(
                                            "`{}` returns `{}`, but this value is `{}`",
                                            func.name,
                                            src(ret_ty),
                                            src(val_ty)
                                        )));
                                    }
                                }
                            }
                        }

                        _ => {}
                    }
                }
            }
        }
        Ok(())
    }
}

/// A type as written in source: `Point`, not `%Point`.
fn src(ty: &IrType) -> String {
    ty.to_string().replace('%', "").replace("enum.", "")
}

/// A type error at instruction `ii` of block `bi`, located by the span table
/// (the instruction's own position, its result's, or an operand's).
fn located(
    func: &crate::ir::function::IrFunction,
    bi: usize,
    ii: usize,
    instr: &IrInstr,
    message: String,
) -> PassError {
    let byte = func
        .get_instr_span(bi as u32, ii)
        .or_else(|| {
            instr
                .operands()
                .iter()
                .find_map(|v| func.span_table.get_for_val(*v))
        })
        .or_else(|| func.debug_span(bi as u32, ii));
    let mut diag = crate::diagnostics::Diagnostic::error(message);
    if let Some(b) = byte {
        diag = diag.with_primary_span(b, b + 1, None);
    }
    PassError::rich("E0202", diag)
}
