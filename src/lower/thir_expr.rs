//! Structured THIR expression and statement lowering.

use std::collections::HashSet;

use crate::error::LowerError;
use crate::ir::block::BlockId;
use crate::ir::function::Param;
use crate::ir::instr::{BinOp, IrInstr, ScalarUnaryOp};
use crate::ir::module::IrFunctionBuilder;
use crate::ir::types::{DType, IrType};
use crate::ir::value::ValueId;
use crate::parser::ast::{AstBinOp, AstType};
use crate::parser::lexer::Span;

use super::rebound::{
    collect_rebound_vars_in_thir_block, find_rebound_vars_thir, unwrap_mut_borrow_target_thir,
};
use super::{
    assignable, binop_mismatch_thir, get_qualified_enum_name_thir, hints, is_inferred_return,
    no_such_field, not_indexable, resolve_qualifier, thir_block_migratable, thir_expr_migratable,
    thir_expr_uses_name, Lowerer, CURRENT_FN_ASTS, IN_THIR_LOWER,
};

impl<'m> Lowerer<'m> {
    /// Try to lower a THIR expression structurally, auto-dereferencing `MutRef(T)`
    /// to `T`. Returns `None` when the form is not migrated yet (caller falls
    /// back to the AST), and `Some` with the result or a real error otherwise.
    pub(super) fn try_lower_thir_expr(
        &mut self,
        e: &crate::thir::ThirExpr,
    ) -> Option<Result<(ValueId, IrType), LowerError>> {
        let (val, ty) = match self.try_lower_thir_expr_keep_ref(e)? {
            Ok(r) => r,
            Err(err) => return Some(Err(err)),
        };
        Some(Ok(self.auto_deref_if_mut_ref(val, ty, e.span)))
    }

    pub(super) fn try_lower_thir_expr_keep_ref(
        &mut self,
        e: &crate::thir::ThirExpr,
    ) -> Option<Result<(ValueId, IrType), LowerError>> {
        struct InThirGuard(bool);
        impl Drop for InThirGuard {
            fn drop(&mut self) {
                IN_THIR_LOWER.with(|c| c.set(self.0));
            }
        }
        let _in_thir_guard = InThirGuard(IN_THIR_LOWER.with(|c| c.replace(true)));
        let result = match &e.kind {
            crate::thir::ThirExprKind::Literal(lit) => Some(self.lower_thir_literal(lit, e.span)),
            crate::thir::ThirExprKind::Value { name, .. } => {
                if name == "none" {
                    let elem = match &self.binding_ty {
                        Some(IrType::Option(el)) => Some((**el).clone()),
                        _ => self.typeck_hint(e.span).and_then(|h| h.part(0)),
                    };
                    let elem = match elem {
                        Some(el) => el,
                        None => {
                            return Some(Err(
                                self.undetermined(e.span, "what this `none` would hold")
                            ))
                        }
                    };
                    let result_ty = IrType::Option(Box::new(elem));
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::MakeNone {
                            result,
                            result_ty: result_ty.clone(),
                        },
                        Some(result_ty.clone()),
                    );
                    return Some(Ok((result, result_ty)));
                }
                let resolved_name = self.resolve_unqualified_name(name);
                if !self.scope.contains_key(&resolved_name) {
                    if let Some((_, ty)) = self.module.state_slot(name) {
                        let ty = ty.clone();
                        return Some(Ok(self.state_get(name, ty)));
                    }
                    if let Some(function) =
                        CURRENT_FN_ASTS.with(|asts| asts.borrow().get(&resolved_name).cloned())
                    {
                        let params: Vec<crate::thir::ThirParam> = function
                            .params
                            .iter()
                            .enumerate()
                            .map(|(index, parameter)| crate::thir::ThirParam {
                                name: format!("__fnref_arg_{index}"),
                                ty: Some(parameter.ty.clone()),
                                default: None,
                            })
                            .collect();
                        let body = crate::thir::ThirExpr {
                            kind: crate::thir::ThirExprKind::Call {
                                callee: resolved_name.clone(),
                                target: crate::hir::Resolution::Unknown,
                                args: params
                                    .iter()
                                    .map(|p| crate::thir::ThirExpr {
                                        kind: crate::thir::ThirExprKind::Value {
                                            name: p.name.clone(),
                                            resolution: crate::hir::Resolution::Local,
                                        },
                                        ty: crate::thir::ThirType::Open,
                                        span: e.span,
                                    })
                                    .collect(),
                                named_args: vec![],
                            },
                            ty: crate::thir::ThirType::Open,
                            span: e.span,
                        };
                        return Some(self.lower_thir_lambda(&params, &body, e.span));
                    }
                    if let Some(ret_ty) = self.fn_sigs.get(&resolved_name).cloned() {
                        let fn_ty = IrType::Fn {
                            params: vec![],
                            ret: Box::new(ret_ty),
                        };
                        let result = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::MakeClosure {
                                result,
                                fn_name: resolved_name,
                                captures: vec![],
                                result_ty: fn_ty.clone(),
                            },
                            Some(fn_ty.clone()),
                        );
                        return Some(Ok((result, fn_ty)));
                    }
                }
                let r = self
                    .load_scope_var(&resolved_name)
                    .ok_or_else(|| self.unknown_name_thir(name, e.span, None));
                if r.is_ok() {
                    self.record_read_place(name, e.span);
                }
                Some(r)
            }
            crate::thir::ThirExprKind::UnaryOp { op, expr: inner } => {
                match self.try_lower_thir_expr(inner) {
                    Some(Ok((v, t))) => Some(self.lower_thir_unary(*op, v, t, e.span)),
                    Some(Err(err)) => Some(Err(err)),
                    None => None,
                }
            }
            crate::thir::ThirExprKind::Tuple(elements) => {
                let mut vals = Vec::with_capacity(elements.len());
                let mut tys = Vec::with_capacity(elements.len());
                for el in elements {
                    match self.try_lower_thir_expr(el) {
                        Some(Ok((v, t))) => {
                            vals.push(v);
                            tys.push(t);
                        }
                        Some(Err(err)) => return Some(Err(err)),
                        None => return None,
                    }
                }
                let result_ty = IrType::Tuple(tys);
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::MakeTuple {
                        result,
                        elements: vals,
                        result_ty: result_ty.clone(),
                    },
                    Some(result_ty.clone()),
                );
                Some(Ok((result, result_ty)))
            }
            crate::thir::ThirExprKind::TupleIndex { base, index } => {
                match self.try_lower_thir_expr(base) {
                    Some(Ok((base_val, base_ty))) => {
                        let elem_types =
                            match &base_ty {
                                IrType::Tuple(elems) => elems.clone(),
                                _ => {
                                    return Some(Err(LowerError::rich(
                                        "E0124",
                                        crate::diagnostics::Diagnostic::error(format!(
                                            "`.N` takes an element of a tuple, but this is `{}`",
                                            hints::type_name(&base_ty)
                                        ))
                                        .with_primary_span(e.span.start.0, e.span.end.0, None),
                                    )));
                                }
                            };
                        if *index >= elem_types.len() {
                            return Some(Err(LowerError::rich(
                                "E0125",
                                crate::diagnostics::Diagnostic::error(format!(
                                    "tuple index {} is out of range: the tuple has {} elements",
                                    index,
                                    elem_types.len()
                                ))
                                .with_primary_span(
                                    e.span.start.0,
                                    e.span.end.0,
                                    None,
                                ),
                            )));
                        }
                        let result_ty = elem_types[*index].clone();
                        let result = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::GetElement {
                                result,
                                base: base_val,
                                index: *index,
                                result_ty: result_ty.clone(),
                            },
                            Some(result_ty.clone()),
                        );
                        Some(Ok((result, result_ty)))
                    }
                    Some(Err(err)) => Some(Err(err)),
                    None => None,
                }
            }
            crate::thir::ThirExprKind::Field { base, field, index } => {
                if let crate::thir::ThirExprKind::Value {
                    name: base_name, ..
                } = &base.as_ref().kind
                {
                    if !self.scope.contains_key(base_name) {
                        let resolved_q = resolve_qualifier(base_name);
                        let mangled_const = format!("{}__{}", resolved_q, field);
                        if let Some(val_ty) = self.scope.get(&mangled_const).cloned() {
                            return Some(Ok(val_ty));
                        }
                    }
                    let resolved_enum = self.resolve_unqualified_name(base_name);
                    if let Some(variants) = self.module.enum_def(&resolved_enum) {
                        let variants = variants.clone();
                        let variant_idx =
                            match variants.iter().position(|v| v == field) {
                                Some(idx) => idx,
                                None => {
                                    return Some(Err(LowerError::rich(
                                        "E0122",
                                        crate::diagnostics::Diagnostic::error(format!(
                                            "`{1}` has no variant `{0}`",
                                            field, resolved_enum
                                        ))
                                        .with_primary_span(e.span.start.0, e.span.end.0, None),
                                    )));
                                }
                            };
                        let result_ty = IrType::Enum {
                            name: resolved_enum,
                            variants,
                        };
                        let result = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::MakeVariant {
                                result,
                                variant_idx,
                                fields: vec![],
                                result_ty: result_ty.clone(),
                            },
                            Some(result_ty.clone()),
                        );
                        return Some(Ok((result, result_ty)));
                    }
                }
                if let Some(enum_name) = get_qualified_enum_name_thir(base.as_ref()) {
                    if let Some(variants) = self.module.enum_def(&enum_name) {
                        let variants = variants.clone();
                        let variant_idx =
                            match variants.iter().position(|v| v == field) {
                                Some(idx) => idx,
                                None => {
                                    return Some(Err(LowerError::rich(
                                        "E0122",
                                        crate::diagnostics::Diagnostic::error(format!(
                                            "`{1}` has no variant `{0}`",
                                            field, enum_name
                                        ))
                                        .with_primary_span(e.span.start.0, e.span.end.0, None),
                                    )));
                                }
                            };
                        let result_ty = IrType::Enum {
                            name: enum_name,
                            variants,
                        };
                        let result = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::MakeVariant {
                                result,
                                variant_idx,
                                fields: vec![],
                                result_ty: result_ty.clone(),
                            },
                            Some(result_ty.clone()),
                        );
                        return Some(Ok((result, result_ty)));
                    }
                }
                match self.try_lower_thir_expr(base) {
                    Some(Ok((base_val, base_ty))) => {
                        if let IrType::Grad(inner) = &base_ty {
                            let inner_ty = *inner.clone();
                            let result = self.builder.fresh_value();
                            let (instr, ret_ty) =
                                if field == "value" {
                                    (
                                        IrInstr::GradValue {
                                            result,
                                            operand: base_val,
                                            ty: inner_ty.clone(),
                                        },
                                        inner_ty,
                                    )
                                } else if field == "grad" || field == "tangent" {
                                    (
                                        IrInstr::GradTangent {
                                            result,
                                            operand: base_val,
                                            ty: inner_ty.clone(),
                                        },
                                        inner_ty,
                                    )
                                } else {
                                    return Some(Err(LowerError::rich(
                                        "E0113",
                                        crate::diagnostics::Diagnostic::error(format!(
                                            "`grad<T>` has no field `{}`: use `.value` or `.grad`",
                                            field
                                        ))
                                        .with_primary_span(e.span.start.0, e.span.end.0, None),
                                    )));
                                };
                            self.builder.push_instr(instr, Some(ret_ty.clone()));
                            Some(Ok((result, ret_ty)))
                        } else {
                            let struct_fields = match &base_ty {
                                IrType::Struct {
                                    name: s_name,
                                    fields,
                                } => {
                                    if let Some(def_fields) = self.local_struct_defs.get(s_name) {
                                        def_fields.clone()
                                    } else if let Some(def_fields) = self.module.struct_def(s_name)
                                    {
                                        def_fields.clone()
                                    } else {
                                        let resolved = self.resolve_unqualified_name(s_name);
                                        if let Some(def_fields) = self.module.struct_def(&resolved)
                                        {
                                            def_fields.clone()
                                        } else {
                                            fields.clone()
                                        }
                                    }
                                }
                                _ => {
                                    return Some(Err(LowerError::rich(
                                        "E0124",
                                        crate::diagnostics::Diagnostic::error(format!(
                                            "`{}` has no fields: only records do",
                                            hints::type_name(&base_ty)
                                        ))
                                        .with_primary_span(e.span.start.0, e.span.end.0, None),
                                    )));
                                }
                            };
                            let field_index = match index {
                                Some(i)
                                    if struct_fields
                                        .get(*i)
                                        .map(|(n, _)| n == field)
                                        .unwrap_or(false) =>
                                {
                                    *i
                                }
                                _ => match struct_fields.iter().position(|(n, _)| n == field) {
                                    Some(i) => i,
                                    None => {
                                        let at = Span::new(
                                            e.span.end.0.saturating_sub(field.len() as u32),
                                            e.span.end.0,
                                        );
                                        return Some(Err(no_such_field(
                                            &hints::type_name(&base_ty),
                                            field,
                                            &struct_fields,
                                            at,
                                        )));
                                    }
                                },
                            };
                            let result_ty = struct_fields[field_index].1.clone();
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::GetField {
                                    result,
                                    base: base_val,
                                    field_index,
                                    result_ty: result_ty.clone(),
                                },
                                Some(result_ty.clone()),
                            );
                            Some(Ok((result, result_ty)))
                        }
                    }
                    Some(Err(err)) => Some(Err(err)),
                    None => None,
                }
            }
            crate::thir::ThirExprKind::Map {
                entries,
                key_ty,
                val_ty,
            } => {
                let (key_ty, val_ty) = if let Some(IrType::Map(k, v)) = &self.binding_ty {
                    (*k.clone(), *v.clone())
                } else {
                    let k = key_ty
                        .clone()
                        .or_else(|| self.typeck_hint(e.span).and_then(|h| h.part(0)))
                        .or_else(|| {
                            entries
                                .first()
                                .and_then(|(k, _)| self.infer_thir_expr_type_simple(k))
                        });
                    let v = val_ty
                        .clone()
                        .or_else(|| self.typeck_hint(e.span).and_then(|h| h.part(1)))
                        .or_else(|| {
                            entries
                                .first()
                                .and_then(|(_, v)| self.infer_thir_expr_type_simple(v))
                        });
                    match (k, v) {
                        (Some(k), Some(v)) => (k, v),
                        _ => {
                            return Some(Err(
                                self.undetermined(e.span, "the key and value types of this map")
                            ));
                        }
                    }
                };
                let map_ty = IrType::Map(Box::new(key_ty.clone()), Box::new(val_ty.clone()));
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::MapNew {
                        result,
                        key_ty: key_ty.clone(),
                        val_ty: val_ty.clone(),
                    },
                    Some(map_ty.clone()),
                );
                for (k, v) in entries {
                    let (key_id, _) = match self.try_lower_thir_expr(k) {
                        Some(Ok(r)) => r,
                        Some(Err(err)) => return Some(Err(err)),
                        None => return None,
                    };
                    let (val_id, _) = match self.try_lower_thir_expr(v) {
                        Some(Ok(r)) => r,
                        Some(Err(err)) => return Some(Err(err)),
                        None => return None,
                    };
                    self.builder.push_instr(
                        IrInstr::MapSet {
                            map: result,
                            key: key_id,
                            value: val_id,
                        },
                        None,
                    );
                }
                Some(Ok((result, map_ty)))
            }
            crate::thir::ThirExprKind::BinOp { op, lhs, rhs } => {
                if matches!(op, AstBinOp::And | AstBinOp::Or) {
                    return Some(self.lower_thir_short_circuit(*op, lhs, rhs, e.span));
                }
                let (lhs_val, lhs_ty) = match self.try_lower_thir_expr(lhs) {
                    Some(Ok(r)) => r,
                    Some(Err(e)) => return Some(Err(e)),
                    None => return None,
                };
                let (rhs_val, rhs_ty) = match self.try_lower_thir_expr(rhs) {
                    Some(Ok(r)) => r,
                    Some(Err(e)) => return Some(Err(e)),
                    None => return None,
                };
                let is_mul = matches!(op, AstBinOp::Mul);
                let special = matches!(
                    (&lhs_ty, &rhs_ty),
                    (IrType::Str, IrType::Str)
                        | (IrType::Scalar(DType::F32), IrType::Scalar(DType::F64))
                        | (IrType::Scalar(DType::F64), IrType::Scalar(DType::F32))
                        | (IrType::Grad(_), IrType::Scalar(_))
                        | (IrType::Scalar(_), IrType::Grad(_))
                ) || (is_mul
                    && matches!(
                        (&lhs_ty, &rhs_ty),
                        (IrType::Str, _)
                            | (_, IrType::Str)
                            | (IrType::Sparse(_), IrType::Scalar(_))
                            | (IrType::Scalar(_), IrType::Sparse(_))
                    ));
                if lhs_ty != rhs_ty && !special {
                    return Some(Err(binop_mismatch_thir(
                        *op, lhs, &lhs_ty, rhs, &rhs_ty, e.span,
                    )));
                }
                Some(self.lower_binop_expr(*op, lhs_val, lhs_ty, rhs_val, rhs_ty, e.span))
            }
            crate::thir::ThirExprKind::Index { base, indices } => {
                let (base_val, base_ty) = match self.try_lower_thir_expr(base) {
                    Some(Ok(r)) => r,
                    Some(Err(e)) => return Some(Err(e)),
                    None => return None,
                };
                if let IrType::Array { elem, .. } | IrType::Slice { elem, .. } = &base_ty {
                    let elem_ty = (**elem).clone();
                    if indices.len() != 1 {
                        return Some(Err(LowerError::rich(
                            "E0125",
                            crate::diagnostics::Diagnostic::error(
                                "an array or slice takes exactly 1 index".to_string(),
                            )
                            .with_primary_span(
                                e.span.start.0,
                                e.span.end.0,
                                None,
                            ),
                        )));
                    }
                    let (idx_val, _) = match self.try_lower_thir_expr(&indices[0]) {
                        Some(Ok(r)) => r,
                        Some(Err(e)) => return Some(Err(e)),
                        None => return None,
                    };
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::ArrayLoad {
                            result,
                            array: base_val,
                            index: idx_val,
                            elem_ty: elem_ty.clone(),
                        },
                        Some(elem_ty.clone()),
                    );
                    return Some(Ok((result, elem_ty)));
                }
                if let IrType::List(elem) = &base_ty {
                    let elem_ty = (**elem).clone();
                    if indices.len() != 1 {
                        return Some(Err(LowerError::rich(
                            "E0125",
                            crate::diagnostics::Diagnostic::error(
                                "a list takes exactly 1 index".to_string(),
                            )
                            .with_primary_span(
                                e.span.start.0,
                                e.span.end.0,
                                None,
                            ),
                        )));
                    }
                    let (index, _) = match self.try_lower_thir_expr(&indices[0]) {
                        Some(Ok(r)) => r,
                        Some(Err(e)) => return Some(Err(e)),
                        None => return None,
                    };
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::ListGet {
                            result,
                            list: base_val,
                            index,
                            elem_ty: elem_ty.clone(),
                        },
                        Some(elem_ty.clone()),
                    );
                    return Some(Ok((result, elem_ty)));
                }
                let elem_ty = match &base_ty {
                    IrType::Tensor { dtype, .. } => IrType::Scalar(*dtype),
                    other => {
                        return Some(Err(not_indexable(other, e.span)));
                    }
                };
                let mut idx_vals = Vec::new();
                for idx in indices {
                    let (iv, _) = match self.try_lower_thir_expr(idx) {
                        Some(Ok(r)) => r,
                        Some(Err(e)) => return Some(Err(e)),
                        None => return None,
                    };
                    idx_vals.push(iv);
                }
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::Load {
                        result,
                        tensor: base_val,
                        indices: idx_vals,
                        result_ty: elem_ty.clone(),
                    },
                    Some(elem_ty.clone()),
                );
                Some(Ok((result, elem_ty)))
            }
            crate::thir::ThirExprKind::Cast {
                expr: inner,
                ty,
                resolved,
            } => {
                let (operand_val, from_ty) = match self.try_lower_thir_expr(inner) {
                    Some(Ok(r)) => r,
                    Some(Err(e)) => return Some(Err(e)),
                    None => return None,
                };
                let to_ty = if !self.type_param_subs.is_empty() {
                    match self.resolve_ty(ty) {
                        Ok(t) => t,
                        Err(e) => return Some(Err(e)),
                    }
                } else {
                    match resolved {
                        Some(t) => t.clone(),
                        None => match self.resolve_ty(ty) {
                            Ok(t) => t,
                            Err(e) => return Some(Err(e)),
                        },
                    }
                };
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::Cast {
                        result,
                        operand: operand_val,
                        from_ty: from_ty.clone(),
                        to_ty: to_ty.clone(),
                    },
                    Some(to_ty.clone()),
                );
                Some(Ok((result, to_ty)))
            }
            crate::thir::ThirExprKind::Block(b) => {
                if !thir_block_migratable(b) {
                    return None;
                }
                Some(self.lower_thir_block_with_keep_ref(b, true))
            }
            crate::thir::ThirExprKind::If {
                cond,
                then_block,
                else_block,
            } => {
                if !thir_expr_migratable(cond) {
                    return None;
                }
                if !thir_block_migratable(then_block) {
                    return None;
                }
                if let Some(eb) = else_block {
                    if !thir_block_migratable(eb) {
                        return None;
                    }
                }
                let rebound = find_rebound_vars_thir(then_block, else_block.as_ref())?;
                Some(self.lower_thir_if_expr_with_keep_ref(
                    cond,
                    then_block,
                    else_block.as_ref(),
                    rebound,
                    e.span,
                    true,
                ))
            }
            crate::thir::ThirExprKind::Ref(inner) => {
                if let Some(name) = unwrap_mut_borrow_target_thir(inner) {
                    let resolved = self.resolve_unqualified_name(name);
                    if self.cell_vars.contains_key(&resolved) {
                        let (_, _, slot_proj) = match self.resolve_thir_place(inner)? {
                            Ok(r) => r,
                            Err(err) => return Some(Err(err)),
                        };
                        let (loaded, elem_ty) = self
                            .load_scope_var(&resolved)
                            .expect("checked in cell_vars");
                        return Some(Ok(
                            self.emit_borrow_marker(loaded, elem_ty, false, slot_proj, e.span)
                        ));
                    }
                }
                match self.resolve_thir_place(inner) {
                    Some(Ok((val, ty, slot_proj))) => Some(Ok(
                        self.emit_borrow_marker(val, ty, false, slot_proj, e.span)
                    )),
                    Some(Err(err)) => Some(Err(err)),
                    None => None,
                }
            }
            crate::thir::ThirExprKind::RefMut(inner) => {
                if let Some(name) = unwrap_mut_borrow_target_thir(inner) {
                    let resolved = self.resolve_unqualified_name(name);
                    if let Some((cell_val, elem_ty)) = self.cell_vars.get(&resolved).cloned() {
                        let (_, _, slot_proj) = match self.resolve_thir_place(inner)? {
                            Ok(r) => r,
                            Err(err) => return Some(Err(err)),
                        };
                        let mut_ref_ty = IrType::MutRef(Box::new(elem_ty));
                        return Some(Ok(
                            self.emit_borrow_marker(cell_val, mut_ref_ty, true, slot_proj, e.span)
                        ));
                    }
                }
                if let crate::thir::ThirExprKind::Deref(ptr_inner) = &inner.kind {
                    match self.try_lower_thir_expr_keep_ref(ptr_inner) {
                        Some(Ok((inner_val, inner_ty)))
                            if matches!(inner_ty, IrType::MutRef(_)) =>
                        {
                            return Some(Ok(
                                self.emit_borrow_marker(inner_val, inner_ty, true, None, e.span)
                            ));
                        }
                        Some(Err(err)) => return Some(Err(err)),
                        _ => {}
                    }
                }
                match self.resolve_thir_place(inner) {
                    Some(Ok((val, ty, slot_proj))) => {
                        if matches!(ty, IrType::Slice { .. } | IrType::MutRef(_)) {
                            Some(Ok(self.emit_borrow_marker(val, ty, true, slot_proj, e.span)))
                        } else {
                            let cell = self.builder.fresh_value();
                            let mut_ref_ty = IrType::MutRef(Box::new(ty.clone()));
                            self.builder.push_instr(
                                IrInstr::CellNew {
                                    result: cell,
                                    init: val,
                                    elem_ty: ty,
                                },
                                Some(mut_ref_ty.clone()),
                            );
                            Some(Ok(self.emit_borrow_marker(
                                cell, mut_ref_ty, true, slot_proj, e.span,
                            )))
                        }
                    }
                    Some(Err(err)) => Some(Err(err)),
                    None => None,
                }
            }
            crate::thir::ThirExprKind::Move(inner) => match self.resolve_thir_place(inner) {
                Some(Ok((val, ty, slot_proj))) => Some(Ok(self.emit_move_marker(
                    val,
                    ty,
                    slot_proj.map(|(s, _)| s),
                    e.span,
                ))),
                Some(Err(err)) => Some(Err(err)),
                None => None,
            },
            crate::thir::ThirExprKind::Deref(inner) => {
                match self.try_lower_thir_expr_keep_ref(inner) {
                    Some(Ok((val, ty))) => {
                        Some(Ok(self.auto_deref_if_mut_ref(val, ty, inner.span)))
                    }
                    other => other,
                }
            }
            crate::thir::ThirExprKind::Unsafe(inner) | crate::thir::ThirExprKind::Splat(inner) => {
                self.try_lower_thir_expr_keep_ref(inner)
            }
            crate::thir::ThirExprKind::Array(elems) => {
                if elems.is_empty() {
                    return Some(Err(LowerError::rich(
                        "E0103",
                        crate::diagnostics::Diagnostic::error(
                            "an empty array literal `[]` is not supported: use `list()` with a type annotation, e.g. `val xs: list<i64> = list()`".to_string(),
                        )
                        .with_primary_span(e.span.start.0, e.span.end.0, None),
                    )));
                }
                let mut elem_vals = Vec::with_capacity(elems.len());
                let mut elem_tys = Vec::with_capacity(elems.len());
                for e in elems {
                    match self.try_lower_thir_expr(e) {
                        Some(Ok((v, t))) => {
                            elem_vals.push(v);
                            elem_tys.push(t);
                        }
                        Some(Err(err)) => return Some(Err(err)),
                        None => return None,
                    }
                }
                let elem_ty = elem_tys.pop().expect("non-empty checked above");
                let size = elem_vals.len();
                let result_ty = IrType::Array {
                    elem: Box::new(elem_ty.clone()),
                    len: size,
                };
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::AllocArray {
                        result,
                        elem_ty: elem_ty.clone(),
                        size,
                        init: elem_vals,
                    },
                    Some(result_ty.clone()),
                );
                Some(Ok((result, result_ty)))
            }
            crate::thir::ThirExprKind::Await(inner) => {
                let (val, ty) = match self.try_lower_thir_expr(inner) {
                    Some(Ok(r)) => r,
                    Some(Err(e)) => return Some(Err(e)),
                    None => return None,
                };
                match ty {
                    IrType::Task(elem) => {
                        let elem_ty = (*elem).clone();
                        let name = if self.async_task.is_some() {
                            "async_await"
                        } else {
                            "async_block_on"
                        };
                        let result = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::BuiltinCall {
                                result,
                                name: name.to_owned(),
                                args: vec![val],
                                result_ty: elem_ty.clone(),
                            },
                            Some(elem_ty.clone()),
                        );
                        Some(Ok((result, elem_ty)))
                    }
                    IrType::Chan(elem) if self.async_task.is_some() => {
                        let elem_ty = (*elem).clone();
                        let receive = self.builder.fresh_value();
                        let receive_ty = IrType::Task(Box::new(elem_ty.clone()));
                        self.builder.push_instr(
                            IrInstr::BuiltinCall {
                                result: receive,
                                name: "async_recv".to_owned(),
                                args: vec![val],
                                result_ty: receive_ty.clone(),
                            },
                            Some(receive_ty),
                        );
                        let result = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::BuiltinCall {
                                result,
                                name: "async_await".to_owned(),
                                args: vec![receive],
                                result_ty: elem_ty.clone(),
                            },
                            Some(elem_ty.clone()),
                        );
                        Some(Ok((result, elem_ty)))
                    }
                    IrType::Chan(elem) => {
                        let elem_ty = (*elem).clone();
                        let result = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::ChanRecv {
                                result,
                                chan: val,
                                elem_ty: elem_ty.clone(),
                            },
                            Some(elem_ty.clone()),
                        );
                        Some(Ok((result, elem_ty)))
                    }
                    other => Some(Err(LowerError::TypeMismatch {
                        expected: "task<T> or chan<T>".to_owned(),
                        found: format!("{}", other),
                        span: e.span,
                    })),
                }
            }
            crate::thir::ThirExprKind::Try(inner) => {
                let (val, res_ty) = match self.try_lower_thir_expr(inner) {
                    Some(Ok(r)) => r,
                    Some(Err(e)) => return Some(Err(e)),
                    None => return None,
                };
                // Determine if this is a result or option type and branch accordingly.
                if let IrType::ResultType(ok, err) = &res_ty {
                    let ok_ty = (**ok).clone();
                    let err_ty = (**err).clone();
                    let is_ok_result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::IsOk {
                            result: is_ok_result,
                            operand: val,
                        },
                        Some(IrType::Scalar(DType::Bool)),
                    );
                    let ok_bb = self.builder.create_block(Some("try_ok"));
                    let err_bb = self.builder.create_block(Some("try_err"));
                    let cont_bb = self.builder.create_block(Some("try_cont"));
                    self.builder.push_instr(
                        IrInstr::CondBr {
                            cond: is_ok_result,
                            then_block: ok_bb,
                            then_args: vec![],
                            else_block: err_bb,
                            else_args: vec![],
                        },
                        None,
                    );
                    self.builder.set_current_block(ok_bb);
                    let ok_unwrapped = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::ResultUnwrap {
                            result: ok_unwrapped,
                            operand: val,
                            result_ty: ok_ty.clone(),
                        },
                        Some(ok_ty.clone()),
                    );
                    self.builder.push_instr(
                        IrInstr::Br {
                            target: cont_bb,
                            args: vec![ok_unwrapped],
                        },
                        None,
                    );
                    self.builder.set_current_block(err_bb);
                    let err_unwrapped = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::ResultUnwrapErr {
                            result: err_unwrapped,
                            operand: val,
                            result_ty: err_ty.clone(),
                        },
                        Some(err_ty.clone()),
                    );
                    if let (Some(catch_bb), Some(_cont_bb), Some(ref _catch_param)) = (
                        self.try_catch_catch_bb,
                        self.try_catch_cont_bb,
                        self.try_catch_param.clone(),
                    ) {
                        match &self.try_catch_err_ty {
                            None => self.try_catch_err_ty = Some(err_ty.clone()),
                            Some(prev)
                                if !assignable(&err_ty, prev) && !assignable(prev, &err_ty) =>
                            {
                                return Some(Err(LowerError::TypeMismatch {
                                    expected: format!(
                                        "{prev} (the error an earlier `?` in this `try` carries)"
                                    ),
                                    found: err_ty.to_string(),
                                    span: e.span,
                                }));
                            }
                            Some(_) => {}
                        }
                        self.builder.push_instr(
                            IrInstr::Br {
                                target: catch_bb,
                                args: vec![err_unwrapped],
                            },
                            None,
                        );
                    } else {
                        let err_result = self.builder.fresh_value();
                        let ok_side = match &self.current_return_ty {
                            Some(IrType::ResultType(ok, _)) => (**ok).clone(),
                            other => {
                                return Some(Err(LowerError::TypeMismatch {
                                    expected:
                                        "a function returning `result` for `?` to return from"
                                            .into(),
                                    found: other
                                        .as_ref()
                                        .map_or("no return type".to_string(), |t| t.to_string()),
                                    span: e.span,
                                }));
                            }
                        };
                        let err_ret_ty =
                            IrType::ResultType(Box::new(ok_side), Box::new(err_ty.clone()));
                        self.builder.push_instr(
                            IrInstr::MakeErr {
                                result: err_result,
                                value: err_unwrapped,
                                result_ty: err_ret_ty.clone(),
                            },
                            Some(err_ret_ty.clone()),
                        );
                        self.emit_drops(&[err_result]);
                        self.emit_defers();
                        self.builder.push_instr(
                            IrInstr::Return {
                                values: vec![err_result],
                            },
                            None,
                        );
                    }
                    self.builder.set_current_block(cont_bb);
                    let result =
                        self.builder
                            .add_block_param(cont_bb, Some("try_result"), ok_ty.clone());
                    self.builder.set_current_block(cont_bb);
                    Some(Ok((result, ok_ty)))
                } else if let IrType::Option(inner) = &res_ty {
                    let inner_ty = (**inner).clone();
                    let is_some_result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::IsSome {
                            result: is_some_result,
                            operand: val,
                        },
                        Some(IrType::Scalar(DType::Bool)),
                    );
                    let some_bb = self.builder.create_block(Some("try_some"));
                    let none_bb = self.builder.create_block(Some("try_none"));
                    let cont_bb = self.builder.create_block(Some("try_cont"));
                    self.builder.push_instr(
                        IrInstr::CondBr {
                            cond: is_some_result,
                            then_block: some_bb,
                            then_args: vec![],
                            else_block: none_bb,
                            else_args: vec![],
                        },
                        None,
                    );
                    self.builder.set_current_block(some_bb);
                    let some_unwrapped = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::OptionUnwrap {
                            result: some_unwrapped,
                            operand: val,
                            result_ty: inner_ty.clone(),
                        },
                        Some(inner_ty.clone()),
                    );
                    self.builder.push_instr(
                        IrInstr::Br {
                            target: cont_bb,
                            args: vec![some_unwrapped],
                        },
                        None,
                    );
                    self.builder.set_current_block(none_bb);
                    let none_val = self.builder.fresh_value();
                    let none_ret_ty = match &self.current_return_ty {
                        Some(t @ IrType::Option(_)) => t.clone(),
                        _ => IrType::Option(Box::new(inner_ty.clone())),
                    };
                    self.builder.push_instr(
                        IrInstr::MakeNone {
                            result: none_val,
                            result_ty: none_ret_ty.clone(),
                        },
                        Some(none_ret_ty),
                    );
                    // Early return with none
                    self.emit_drops(&[none_val]);
                    self.emit_defers();
                    self.builder.push_instr(
                        IrInstr::Return {
                            values: vec![none_val],
                        },
                        None,
                    );
                    self.builder.set_current_block(cont_bb);
                    let result =
                        self.builder
                            .add_block_param(cont_bb, Some("try_result"), inner_ty.clone());
                    Some(Ok((result, inner_ty)))
                } else {
                    Some(Err(LowerError::TypeMismatch {
                        expected: "result<T,E> or option<T>".to_owned(),
                        found: format!("{}", res_ty),
                        span: e.span,
                    }))
                }
            }
            crate::thir::ThirExprKind::Mask {
                effects: _effects,
                body,
            } => {
                if !thir_block_migratable(body) {
                    return None;
                }
                Some(self.lower_thir_mask_block(body, e.span))
            }
            crate::thir::ThirExprKind::Raise { effect, args, .. } => {
                if !args.iter().all(thir_expr_migratable) {
                    return None;
                }
                let mut lowered_args = Vec::new();
                for a in args {
                    match self.try_lower_thir_expr(a) {
                        Some(Ok((v, _))) => lowered_args.push(v),
                        Some(Err(err)) => return Some(Err(err)),
                        None => return None,
                    }
                }
                let effect_name = effect.clone();
                let ret_ty = self
                    .module
                    .extern_fns
                    .iter()
                    .find(|ef| ef.name == effect_name)
                    .map(|ef| ef.ret_ty.clone())
                    .ok_or_else(|| {
                        LowerError::rich(
                            "E0100",
                            crate::diagnostics::Diagnostic::error(format!(
                                "no effect operation `{}` is declared",
                                effect_name
                            ))
                            .with_code("E0100")
                            .with_primary_span(e.span.start.0, e.span.end.0, None)
                            .with_help(format!(
                                "declare it: `extern def {}(...) -> T`",
                                effect_name
                            )),
                        )
                    });
                match ret_ty {
                    Ok(rt) => {
                        let result = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::CallExtern {
                                result: Some(result),
                                name: effect_name,
                                args: lowered_args,
                                ret_ty: rt.clone(),
                            },
                            Some(rt.clone()),
                        );
                        Some(Ok((result, rt)))
                    }
                    Err(err) => Some(Err(err)),
                }
            }
            crate::thir::ThirExprKind::StructLit {
                name,
                fields,
                spread,
                ..
            } => {
                if !fields.iter().all(|f| thir_expr_migratable(&f.value)) {
                    return None;
                }
                if let Some(s) = spread {
                    if !thir_expr_migratable(s) {
                        return None;
                    }
                }
                Some(self.lower_thir_struct_lit(name, fields, spread, e.span))
            }
            crate::thir::ThirExprKind::TryCatch {
                body,
                catch_param,
                catch_body,
            } => {
                let prev_catch = self.try_catch_catch_bb.take();
                let prev_cont = self.try_catch_cont_bb.take();
                let prev_param = self.try_catch_param.take();
                let prev_err_ty = self.try_catch_err_ty.take();

                let catch_bb = self.builder.create_block(Some("catch"));
                let cont_bb = self.builder.create_block(Some("try_cont"));

                self.try_catch_catch_bb = Some(catch_bb);
                self.try_catch_cont_bb = Some(cont_bb);
                self.try_catch_param = Some(catch_param.clone());

                let (mut body_val, mut body_ty) = match self.try_lower_thir_expr_keep_ref(body) {
                    Some(Ok(r)) => r,
                    Some(Err(err)) => return Some(Err(err)),
                    None => {
                        return Some(Err(LowerError::rich(
                            "E0902",
                            crate::diagnostics::Diagnostic::error(
                                "internal compiler error: unmigratable THIR trycatch body"
                                    .to_string(),
                            )
                            .with_code("E0902"),
                        )))
                    }
                };
                let body_end_bb = self.builder.current_block();
                let body_terminated = self.builder.is_current_block_terminated();

                self.builder.set_current_block(catch_bb);
                let catch_ty = self.try_catch_err_ty.take().unwrap_or(IrType::Str);
                let catch_param_val =
                    self.builder
                        .add_block_param(catch_bb, Some(catch_param), catch_ty.clone());
                self.scope
                    .insert(catch_param.clone(), (catch_param_val, catch_ty));
                let (mut catch_val, mut catch_val_ty) =
                    match self.try_lower_thir_expr_keep_ref(catch_body) {
                        Some(Ok(r)) => r,
                        Some(Err(err)) => return Some(Err(err)),
                        None => {
                            return Some(Err(LowerError::rich(
                                "E0902",
                                crate::diagnostics::Diagnostic::error(
                                    "internal compiler error: unmigratable THIR trycatch catch"
                                        .to_string(),
                                )
                                .with_code("E0902"),
                            )))
                        }
                    };
                let catch_end_bb = self.builder.current_block();
                let catch_terminated = self.builder.is_current_block_terminated();

                let result_ty = Self::unify_branch_ref_types(
                    !body_terminated,
                    &body_ty,
                    !catch_terminated,
                    &catch_val_ty,
                )
                .unwrap_or_else(|| body_ty.clone());
                let any_shared_borrow = Self::expr_is_shared_borrow_thir(body)
                    || Self::expr_is_shared_borrow_thir(catch_body);
                if !any_shared_borrow {
                    if !body_terminated {
                        self.builder.set_current_block(body_end_bb);
                        (body_val, body_ty) =
                            self.coerce_branch_mut_ref(body_val, body_ty, &result_ty, e.span);
                    }
                    if !catch_terminated {
                        self.builder.set_current_block(catch_end_bb);
                        (catch_val, catch_val_ty) =
                            self.coerce_branch_mut_ref(catch_val, catch_val_ty, &result_ty, e.span);
                    }
                }
                let _ = (body_ty, catch_val_ty);

                if !body_terminated {
                    self.builder.set_current_block(body_end_bb);
                    self.builder.push_instr(
                        IrInstr::Br {
                            target: cont_bb,
                            args: vec![body_val],
                        },
                        None,
                    );
                }
                if !catch_terminated {
                    self.builder.set_current_block(catch_end_bb);
                    self.builder.push_instr(
                        IrInstr::Br {
                            target: cont_bb,
                            args: vec![catch_val],
                        },
                        None,
                    );
                }

                self.builder.set_current_block(cont_bb);
                let result =
                    self.builder
                        .add_block_param(cont_bb, Some("try_result"), result_ty.clone());

                self.try_catch_catch_bb = prev_catch;
                self.try_catch_cont_bb = prev_cont;
                self.try_catch_param = prev_param;
                self.try_catch_err_ty = prev_err_ty;

                Some(Ok((result, result_ty)))
            }
            crate::thir::ThirExprKind::NullCoal { expr, default } => {
                let (val, val_ty) = match self.try_lower_thir_expr(expr) {
                    Some(Ok(r)) => r,
                    Some(Err(err)) => return Some(Err(err)),
                    None => return None,
                };
                let (default_val, _default_ty) = match self.try_lower_thir_expr(default) {
                    Some(Ok(r)) => r,
                    Some(Err(err)) => return Some(Err(err)),
                    None => return None,
                };
                match &val_ty {
                    IrType::Option(inner) => {
                        let result_type = (**inner).clone();
                        let is_some_result = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::IsSome {
                                result: is_some_result,
                                operand: val,
                            },
                            Some(IrType::Scalar(DType::Bool)),
                        );
                        let then_bb = self.builder.create_block(Some("nullcoal_some"));
                        let else_bb = self.builder.create_block(Some("nullcoal_none"));
                        let merge_bb = self.builder.create_block(Some("nullcoal_merge"));
                        self.builder.push_instr(
                            IrInstr::CondBr {
                                cond: is_some_result,
                                then_block: then_bb,
                                then_args: vec![],
                                else_block: else_bb,
                                else_args: vec![],
                            },
                            None,
                        );
                        self.builder.set_current_block(then_bb);
                        let unwrapped = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::OptionUnwrap {
                                result: unwrapped,
                                operand: val,
                                result_ty: result_type.clone(),
                            },
                            Some(result_type.clone()),
                        );
                        self.builder.push_instr(
                            IrInstr::Br {
                                target: merge_bb,
                                args: vec![unwrapped],
                            },
                            None,
                        );
                        self.builder.set_current_block(else_bb);
                        self.builder.push_instr(
                            IrInstr::Br {
                                target: merge_bb,
                                args: vec![default_val],
                            },
                            None,
                        );
                        let merge_result = self.builder.add_block_param(
                            merge_bb,
                            Some("nullcoal_res"),
                            result_type.clone(),
                        );
                        self.builder.set_current_block(merge_bb);
                        Some(Ok((merge_result, result_type)))
                    }
                    IrType::ResultType(ok, _) => {
                        let result_type = (**ok).clone();
                        let is_ok_result = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::IsOk {
                                result: is_ok_result,
                                operand: val,
                            },
                            Some(IrType::Scalar(DType::Bool)),
                        );
                        let then_bb = self.builder.create_block(Some("nullcoal_ok"));
                        let else_bb = self.builder.create_block(Some("nullcoal_err"));
                        let merge_bb = self.builder.create_block(Some("nullcoal_merge"));
                        self.builder.push_instr(
                            IrInstr::CondBr {
                                cond: is_ok_result,
                                then_block: then_bb,
                                then_args: vec![],
                                else_block: else_bb,
                                else_args: vec![],
                            },
                            None,
                        );
                        self.builder.set_current_block(then_bb);
                        let unwrapped = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::ResultUnwrap {
                                result: unwrapped,
                                operand: val,
                                result_ty: result_type.clone(),
                            },
                            Some(result_type.clone()),
                        );
                        self.builder.push_instr(
                            IrInstr::Br {
                                target: merge_bb,
                                args: vec![unwrapped],
                            },
                            None,
                        );
                        self.builder.set_current_block(else_bb);
                        self.builder.push_instr(
                            IrInstr::Br {
                                target: merge_bb,
                                args: vec![default_val],
                            },
                            None,
                        );
                        let merge_result = self.builder.add_block_param(
                            merge_bb,
                            Some("nullcoal_res"),
                            result_type.clone(),
                        );
                        self.builder.set_current_block(merge_bb);
                        Some(Ok((merge_result, result_type)))
                    }
                    _ => Some(Ok((val, val_ty))),
                }
            }
            crate::thir::ThirExprKind::MacroCall { name, .. } => {
                Some(Err(crate::error::LowerError::rich(
                    "E0100",
                    crate::diagnostics::Diagnostic::error(format!("no macro named `{}`", name))
                        .with_primary_span(e.span.start.0, e.span.end.0, None),
                )))
            }
            crate::thir::ThirExprKind::Handle {
                expr,
                arms,
                return_ty,
            } => Some(self.lower_thir_handle(expr, arms, return_ty)),
            crate::thir::ThirExprKind::MethodCall {
                receiver,
                method,
                args,
                ..
            } => Some(self.lower_thir_method_call(receiver, method, args, e.span)),
            crate::thir::ThirExprKind::When {
                scrutinee, arms, ..
            } => Some(self.lower_thir_when_expr_with_keep_ref(scrutinee, arms, e.span, true)),
            crate::thir::ThirExprKind::Call {
                callee,
                args,
                named_args,
                ..
            } => Some(self.lower_thir_call(callee, args, named_args, e.span)),
            crate::thir::ThirExprKind::Destructure { base, index } => {
                let (base_val, base_ty) = match self.try_lower_thir_expr(base) {
                    Some(Ok(r)) => r,
                    Some(Err(e)) => return Some(Err(e)),
                    None => return None,
                };
                match &base_ty {
                    IrType::Tuple(elems) => {
                        if *index >= elems.len() {
                            return Some(Err(LowerError::rich(
                                "E0125",
                                crate::diagnostics::Diagnostic::error(format!(
                                    "tuple index {} is out of range: the tuple has {} elements",
                                    index,
                                    elems.len()
                                ))
                                .with_primary_span(
                                    e.span.start.0,
                                    e.span.end.0,
                                    None,
                                ),
                            )));
                        }
                        let result_ty = elems[*index].clone();
                        let result = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::GetElement {
                                result,
                                base: base_val,
                                index: *index,
                                result_ty: result_ty.clone(),
                            },
                            Some(result_ty.clone()),
                        );
                        Some(Ok((result, result_ty)))
                    }
                    IrType::Struct { fields, .. } => {
                        if *index >= fields.len() {
                            return Some(Err(LowerError::rich(
                                "E0125",
                                crate::diagnostics::Diagnostic::error(format!(
                                    "record index {} is out of range",
                                    index
                                ))
                                .with_primary_span(
                                    e.span.start.0,
                                    e.span.end.0,
                                    None,
                                ),
                            )));
                        }
                        let result_ty = fields[*index].1.clone();
                        let result = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::GetField {
                                result,
                                base: base_val,
                                field_index: *index,
                                result_ty: result_ty.clone(),
                            },
                            Some(result_ty.clone()),
                        );
                        Some(Ok((result, result_ty)))
                    }
                    _ => None,
                }
            }
            crate::thir::ThirExprKind::Lambda { params, body, .. } => {
                Some(self.lower_thir_lambda(params, body, e.span))
            }
        };
        if let Some(Ok((v, _))) = &result {
            self.builder.note_value_span(*v, e.span.start.0);
        }
        result
    }

    pub(super) fn lower_thir_expr_req(
        &mut self,
        e: &crate::thir::ThirExpr,
    ) -> Result<(ValueId, IrType), LowerError> {
        match self.try_lower_thir_expr(e) {
            Some(r) => r,
            None => Err(LowerError::rich(
                "E0902",
                crate::diagnostics::Diagnostic::error(
                    "internal compiler error: unmigratable THIR expression".to_string(),
                )
                .with_code("E0902"),
            )),
        }
    }

    pub(super) fn lower_thir_expr_keep_ref_req(
        &mut self,
        e: &crate::thir::ThirExpr,
    ) -> Result<(ValueId, IrType), LowerError> {
        match self.try_lower_thir_expr_keep_ref(e) {
            Some(r) => r,
            None => Err(LowerError::rich(
                "E0902",
                crate::diagnostics::Diagnostic::error(
                    "internal compiler error: unmigratable THIR expression".to_string(),
                )
                .with_code("E0902"),
            )),
        }
    }

    pub(super) fn emit_defers(&mut self) {
        let thir_defers: Vec<_> = self.thir_defer_stack.clone();
        for expr in thir_defers.iter().rev() {
            let _ = self.try_lower_thir_expr(expr);
        }
        let defers: Vec<_> = self.defer_stack.clone();
        for expr in defers.iter().rev() {
            let _ = self.lower_expr(expr);
        }
    }

    pub(super) fn lower_thir_block_opt_with_keep_ref(
        &mut self,
        b: &crate::thir::ThirBlock,
        keep_ref: bool,
    ) -> Result<Option<(ValueId, IrType)>, LowerError> {
        for stmt in &b.stmts {
            if self.builder.is_current_block_terminated() {
                break;
            }
            match self.lower_thir_stmt(stmt) {
                Some(Ok(())) => {}
                Some(Err(e)) => return Err(e),
                None => {
                    return Err(LowerError::rich(
                        "E0902",
                        crate::diagnostics::Diagnostic::error(
                            "internal compiler error: unmigratable THIR statement".to_string(),
                        )
                        .with_code("E0902"),
                    ))
                }
            }
        }
        if let Some(tail) = &b.tail {
            self.builder.set_line_span(tail.span.start.0);
            if self.builder.is_current_block_terminated() {
                return Ok(None);
            }
            let lowered = if keep_ref {
                self.try_lower_thir_expr_keep_ref(tail)
            } else {
                self.try_lower_thir_expr(tail)
            };
            match lowered {
                Some(Ok(r)) => return Ok(Some(r)),
                Some(Err(e)) => return Err(e),
                None => {
                    return Err(LowerError::rich(
                        "E0902",
                        crate::diagnostics::Diagnostic::error(
                            "internal compiler error: unmigratable THIR expr".to_string(),
                        )
                        .with_code("E0902"),
                    ))
                }
            }
        }
        Ok(None)
    }

    pub(super) fn lower_thir_block(
        &mut self,
        b: &crate::thir::ThirBlock,
    ) -> Result<(ValueId, IrType), LowerError> {
        self.lower_thir_block_with_keep_ref(b, false)
    }

    pub(super) fn lower_thir_block_with_keep_ref(
        &mut self,
        b: &crate::thir::ThirBlock,
        keep_ref: bool,
    ) -> Result<(ValueId, IrType), LowerError> {
        if let Some(v) = self.lower_thir_block_opt_with_keep_ref(b, keep_ref)? {
            Ok(v)
        } else if let Some((param_id, param_ty)) = self.builder.current_block_first_param() {
            Ok((param_id, param_ty))
        } else {
            Ok(self.emit_unit_value())
        }
    }

    pub(super) fn lower_thir_short_circuit(
        &mut self,
        op: AstBinOp,
        lhs: &crate::thir::ThirExpr,
        rhs: &crate::thir::ThirExpr,
        _span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        let bool_ty = IrType::Scalar(DType::Bool);
        let (lhs_val, _) = self.lower_thir_expr_req(lhs)?;

        let rhs_bb = self.builder.create_block(Some("sc_rhs"));
        let merge_bb = self.builder.create_block(Some("sc_merge"));

        let short_val = self.builder.fresh_value();
        let short_bool = matches!(op, AstBinOp::Or);
        self.builder.push_instr(
            IrInstr::ConstBool {
                result: short_val,
                value: short_bool,
            },
            Some(bool_ty.clone()),
        );

        match op {
            AstBinOp::And => {
                self.builder.push_instr(
                    IrInstr::CondBr {
                        cond: lhs_val,
                        then_block: rhs_bb,
                        then_args: vec![],
                        else_block: merge_bb,
                        else_args: vec![short_val],
                    },
                    None,
                );
            }
            AstBinOp::Or => {
                self.builder.push_instr(
                    IrInstr::CondBr {
                        cond: lhs_val,
                        then_block: merge_bb,
                        then_args: vec![short_val],
                        else_block: rhs_bb,
                        else_args: vec![],
                    },
                    None,
                );
            }
            _ => unreachable!(),
        }

        self.builder.set_current_block(rhs_bb);
        let (rhs_val, _) = self.lower_thir_expr_req(rhs)?;
        if !self.builder.is_current_block_terminated() {
            self.builder.push_instr(
                IrInstr::Br {
                    target: merge_bb,
                    args: vec![rhs_val],
                },
                None,
            );
        }

        let result = self
            .builder
            .add_block_param(merge_bb, Some("sc_result"), bool_ty.clone());
        self.builder.set_current_block(merge_bb);
        Ok((result, bool_ty))
    }

    pub(super) fn lower_thir_mask_block(
        &mut self,
        body: &crate::thir::ThirBlock,
        _span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        self.lower_thir_block(body)
    }

    pub(super) fn lower_thir_struct_lit(
        &mut self,
        name: &str,
        fields: &[crate::thir::ThirFieldInit],
        spread: &Option<Box<crate::thir::ThirExpr>>,
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        let (mut resolved_name, mut struct_fields) = self.resolve_struct_lit_header(
            name,
            |this| {
                fields
                    .iter()
                    .map(|f| (f.name.clone(), this.typeck_type(f.value.span)))
                    .collect()
            },
            span,
        )?;

        let spread_val = if let Some(spread_expr) = spread {
            match self.try_lower_thir_expr(spread_expr) {
                Some(Ok(r)) => Some(r),
                Some(Err(e)) => return Err(e),
                None => {
                    return Err(LowerError::rich(
                        "E0902",
                        crate::diagnostics::Diagnostic::error(
                            "internal compiler error: unmigratable THIR spread".to_string(),
                        )
                        .with_code("E0902"),
                    ))
                }
            }
        } else {
            None
        };

        let field_defaults = self
            .local_struct_defaults
            .get(&resolved_name)
            .or_else(|| self.module.struct_defaults.get(&resolved_name))
            .cloned();

        // Reuse original field validation with synthesized AstExpr placeholders.
        let synth_fields: Vec<(String, crate::parser::ast::AstExpr)> = fields
            .iter()
            .map(|f| {
                (
                    f.name.clone(),
                    crate::parser::ast::AstExpr::Ident(crate::parser::ast::Ident {
                        name: String::new(),
                        span: f.value.span,
                    }),
                )
            })
            .collect();
        let synth_spans: Vec<Span> = fields.iter().map(|f| f.value.span).collect();
        self.check_struct_literal_fields(
            name,
            &struct_fields,
            &synth_fields,
            &synth_spans,
            spread.is_some(),
            field_defaults.as_ref(),
            span,
        )?;

        let mut field_vals = Vec::with_capacity(struct_fields.len());
        let mut field_actual_tys = Vec::with_capacity(struct_fields.len());
        for (field_idx, (field_name, field_ty)) in struct_fields.iter().enumerate() {
            if let Some(provided) = fields.iter().find(|f| f.name == *field_name) {
                let prev_binding_ty = self.binding_ty.take();
                self.binding_ty = Some(field_ty.clone());
                let (val, actual_ty) = match self.try_lower_thir_expr(&provided.value) {
                    Some(Ok(r)) => r,
                    Some(Err(e)) => return Err(e),
                    None => {
                        return Err(LowerError::rich(
                            "E0902",
                            crate::diagnostics::Diagnostic::error(
                                "internal compiler error: unmigratable THIR field".to_string(),
                            )
                            .with_code("E0902"),
                        ))
                    }
                };
                self.binding_ty = prev_binding_ty;
                let (val, actual_ty) =
                    self.coerce_to_trait_object(val, actual_ty, field_ty, provided.value.span)?;
                field_vals.push(val);
                field_actual_tys.push(actual_ty);
            } else if let Some((spread_val_id, spread_ty)) = &spread_val {
                let spread_struct_fields = match spread_ty {
                    IrType::Struct { fields, .. } => fields.clone(),
                    _ => {
                        return Err(LowerError::rich(
                            "E0124",
                            crate::diagnostics::Diagnostic::error(
                                "the value after `..` must be a record of the same type"
                                    .to_string(),
                            )
                            .with_primary_span(
                                span.start.0,
                                span.end.0,
                                None,
                            ),
                        ));
                    }
                };
                let field_index = spread_struct_fields
                    .iter()
                    .position(|(n, _)| n == field_name)
                    .ok_or_else(|| {
                        LowerError::rich(
                            "E0113",
                            crate::diagnostics::Diagnostic::error(format!(
                                "the record after `..` has no field `{}`",
                                field_name
                            ))
                            .with_primary_span(
                                span.start.0,
                                span.end.0,
                                None,
                            ),
                        )
                    })?;
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::GetField {
                        result,
                        base: *spread_val_id,
                        field_index,
                        result_ty: field_ty.clone(),
                    },
                    Some(field_ty.clone()),
                );
                field_vals.push(result);
                field_actual_tys.push(field_ty.clone());
            } else if let Some(defaults) = &field_defaults {
                if let Some(Some(default_expr)) = defaults.get(field_idx) {
                    let prev_binding_ty = self.binding_ty.take();
                    self.binding_ty = Some(field_ty.clone());
                    let (val, actual_ty) = self.lower_expr(default_expr)?;
                    self.binding_ty = prev_binding_ty;
                    let (val, actual_ty) =
                        self.coerce_to_trait_object(val, actual_ty, field_ty, default_expr.span())?;
                    field_vals.push(val);
                    field_actual_tys.push(actual_ty);
                } else {
                    return Err(LowerError::rich(
                        "E0114",
                        crate::diagnostics::Diagnostic::error(format!(
                            "missing field `{}`",
                            field_name
                        ))
                        .with_primary_span(span.start.0, span.end.0, None),
                    ));
                }
            } else {
                return Err(LowerError::rich(
                    "E0114",
                    crate::diagnostics::Diagnostic::error(format!(
                        "missing field `{}`",
                        field_name
                    ))
                    .with_primary_span(span.start.0, span.end.0, None),
                ));
            }
        }

        self.finish_generic_struct_lit(
            name,
            &mut resolved_name,
            &mut struct_fields,
            &field_actual_tys,
            spread.is_some(),
        );

        let result_ty = IrType::Struct {
            name: resolved_name,
            fields: struct_fields,
        };
        let result = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::MakeStruct {
                result,
                fields: field_vals,
                result_ty: result_ty.clone(),
            },
            Some(result_ty.clone()),
        );
        Ok((result, result_ty))
    }

    pub(super) fn lower_thir_stmt(
        &mut self,
        s: &crate::thir::ThirStmt,
    ) -> Option<Result<(), LowerError>> {
        self.builder.set_span_byte(s.span.start.0);
        match &s.kind {
            crate::thir::ThirStmtKind::Let { name, ty, init, is_var } => {
                let mut ann_ty: Option<IrType> = None;
                if let Some(ast_ty) = ty {
                    let ann = match self.resolve_ty(ast_ty) {
                        Ok(a) => a,
                        Err(e) => return Some(Err(e)),
                    };
                    ann_ty = Some(ann.clone());
                    self.binding_ty = Some(ann.clone());
                    self.expected_expr_ty = Some(ann);
                }
                let keep_ref =
                    ty.is_none() || ann_ty.as_ref().is_some_and(|a| matches!(a, IrType::MutRef(_)));
                let lowered = if keep_ref {
                    self.try_lower_thir_expr_keep_ref(init)
                } else {
                    self.try_lower_thir_expr(init)
                };
                let (mut val, mut tyval) = match lowered {
                    Some(Ok(r)) => r,
                    Some(Err(e)) => return Some(Err(e)),
                    None => return None,
                };
                self.binding_ty = None;
                self.expected_expr_ty = None;
                if let (Some(ast_ty), Some(ann)) = (ty, ann_ty.as_ref()) {
                    let (nv, nt) =
                        match self.coerce_to_trait_object(val, tyval.clone(), ann, ast_ty.span()) {
                            Ok(r) => r,
                            Err(e) => return Some(Err(e)),
                        };
                    val = nv;
                    tyval = nt;
                }
                if let Some(drop_fn) = self.find_destructor(&tyval) {
                    self.register_drop(name.clone(), val, drop_fn, tyval.clone());
                }
                if !name.starts_with('_') {
                    self.builder.name_value(val, name);
                }
                self.bind_scope_var(name.clone(), val, tyval);
                let _ = is_var;
                Some(Ok(()))
            }
            crate::thir::ThirStmtKind::Expr(e) => {
                match self.try_lower_thir_expr(e) {
                    Some(Ok(_)) => Some(Ok(())),
                    Some(Err(err)) => Some(Err(err)),
                    None => None,
                }
            }
            crate::thir::ThirStmtKind::Return(value) => {
                let ret_values = if let Some(expr) = value {
                    if self.expected_expr_ty.is_none() {
                        self.expected_expr_ty = self.current_return_ty.clone();
                    }
                    let keep_ref = self
                        .current_return_ty
                        .as_ref()
                        .is_some_and(|t| matches!(t, IrType::MutRef(_)));
                    let lowered = if keep_ref {
                        self.try_lower_thir_expr_keep_ref(expr)
                    } else {
                        self.try_lower_thir_expr(expr)
                    };
                    let (val, ty) = match lowered {
                        Some(Ok(r)) => r,
                        Some(Err(e)) => return Some(Err(e)),
                        None => return None,
                    };
                    self.expected_expr_ty = None;
                    let (val, _ty) = if let Some(expected) = self.current_return_ty.clone() {
                        match self.coerce_to_trait_object(val, ty, &expected, expr.span) {
                            Ok(r) => r,
                            Err(e) => return Some(Err(e)),
                        }
                    } else {
                        (val, ty)
                    };
                    vec![val]
                } else {
                    vec![]
                };
                self.emit_drops(&ret_values);
                self.emit_defers();
                self.builder
                    .push_instr(IrInstr::Return { values: ret_values }, None);
                Some(Ok(()))
            }
            crate::thir::ThirStmtKind::Assign { target, op, value } => {
                match &target.kind {
                    crate::thir::ThirExprKind::Value { name, .. } => {
                        let resolved = self.resolve_unqualified_name(name);
                        if !self.scope.contains_key(name) && !self.scope.contains_key(&resolved) {
                            if let Some((_, state_ty)) = self.module.state_slot(name) {
                                let state_ty = state_ty.clone();
                                let (value_val, value_ty) = if let Some(bin_op) = op {
                                    let (current, _) = self.state_get(name, state_ty.clone());
                                    let (rhs_val, _) = match self.try_lower_thir_expr(value) {
                                        Some(Ok(r)) => r,
                                        Some(Err(e)) => return Some(Err(e)),
                                        None => return None,
                                    };
                                    (self.compound(*bin_op, current, &state_ty, rhs_val), state_ty.clone())
                                } else {
                                    let prev_binding_ty = self.binding_ty.replace(state_ty.clone());
                                    let lowered = self.try_lower_thir_expr(value);
                                    self.binding_ty = prev_binding_ty;
                                    match lowered {
                                        Some(Ok((v, t))) => (v, t),
                                        Some(Err(e)) => return Some(Err(e)),
                                        None => return None,
                                    }
                                };
                                if !assignable(&value_ty, &state_ty) {
                                    return Some(Err(LowerError::TypeMismatch {
                                        expected: format!("{state_ty} (the type of program state '{}')", name),
                                        found: value_ty.to_string(),
                                        span: s.span,
                                    }));
                                }
                                self.state_set(name, value_val);
                                return Some(Ok(()));
                            }
                        }
                        if let Some((cell, elem_ty)) = self.cell_vars.get(&resolved).cloned() {
                            let new_val = if let Some(bin_op) = op {
                                let (lhs_val, lhs_ty) = self
                                    .load_scope_var(&resolved)
                                    .expect("checked in cell_vars");
                                let (rhs_val, _) = match self.try_lower_thir_expr(value) {
                                    Some(Ok(r)) => r,
                                    Some(Err(e)) => return Some(Err(e)),
                                    None => return None,
                                };
                                self.compound(*bin_op, lhs_val, &lhs_ty, rhs_val)
                            } else {
                                let prev_binding_ty = self.binding_ty.replace(elem_ty.clone());
                                let lowered =
                                    self.lower_thir_expr_for_expected_ty(value, Some(&elem_ty));
                                self.binding_ty = prev_binding_ty;
                                let (val, val_ty) = match lowered {
                                    Ok(r) => r,
                                    Err(e) => return Some(Err(e)),
                                };
                                if !assignable(&val_ty, &elem_ty) {
                                    return Some(Err(LowerError::TypeMismatch {
                                        expected: elem_ty.to_string(),
                                        found: val_ty.to_string(),
                                        span: s.span,
                                    }));
                                }
                                val
                            };
                            self.builder.name_value(new_val, name);
                            self.record_assign_place(name, None, new_val, target.span);
                            self.builder.push_instr(
                                IrInstr::CellStore {
                                    cell,
                                    value: new_val,
                                },
                                None,
                            );
                            return Some(Ok(()));
                        }
                        let new_val = if let Some(bin_op) = op {
                            let (lhs_val, lhs_ty) = match self
                                .scope
                                .get(name)
                                .cloned()
                                .or_else(|| self.scope.get(&resolved).cloned())
                            {
                                Some(r) => r,
                                None => {
                                    return Some(Err(LowerError::rich(
                                        "E0100",
                                        crate::diagnostics::Diagnostic::error(format!(
                                            "cannot find `{}` in this scope",
                                            name
                                        ))
                                        .with_primary_span(s.span.start.0, s.span.end.0, None),
                                    )))
                                }
                            };
                            let (rhs_val, _) = match self.try_lower_thir_expr(value) {
                                Some(Ok(r)) => r,
                                Some(Err(e)) => return Some(Err(e)),
                                None => return None,
                            };
                            (self.compound(*bin_op, lhs_val, &lhs_ty, rhs_val), lhs_ty)
                        } else {
                            let expected_ty = self
                                .scope
                                .get(name)
                                .or_else(|| self.scope.get(&resolved))
                                .map(|(_, ty)| ty.clone());
                            match self.lower_thir_expr_for_expected_ty(value, expected_ty.as_ref()) {
                                Ok(r) => r,
                                Err(e) => return Some(Err(e)),
                            }
                        };
                        self.builder.name_value(new_val.0, name);
                        self.record_assign_place(name, None, new_val.0, target.span);
                        self.scope.insert(name.clone(), (new_val.0, new_val.1));
                        Some(Ok(()))
                    }
                    crate::thir::ThirExprKind::Deref(ptr_expr) => {
                        Some(self.lower_thir_deref_assign(ptr_expr, *op, value, s.span))
                    }
                    crate::thir::ThirExprKind::Index { base, indices } => {
                        let (base_val, base_ty) = match self.try_lower_thir_expr(base) {
                            Some(Ok(r)) => r,
                            Some(Err(e)) => return Some(Err(e)),
                            None => return None,
                        };
                        let (single, elem_ty) = match &base_ty {
                            IrType::Array { elem, .. }
                            | IrType::Slice { elem, .. }
                            | IrType::List(elem) => (true, (**elem).clone()),
                            IrType::Tensor { dtype, .. } => (false, IrType::Scalar(*dtype)),
                            other => return Some(Err(not_indexable(other, target.span))),
                        };
                        if single && indices.len() != 1 {
                            return Some(Err(LowerError::rich(
                                "E0125",
                                crate::diagnostics::Diagnostic::error(format!(
                                    "`{}` takes exactly 1 index",
                                    hints::type_name(&base_ty)
                                ))
                                .with_primary_span(target.span.start.0, target.span.end.0, None),
                            )));
                        }
                        let mut idx_vals = Vec::with_capacity(indices.len());
                        for idx in indices {
                            let (iv, _) = match self.try_lower_thir_expr(idx) {
                                Some(Ok(r)) => r,
                                Some(Err(e)) => return Some(Err(e)),
                                None => return None,
                            };
                            idx_vals.push(iv);
                        }
                        let (mut value_val, _) = match self.try_lower_thir_expr(value) {
                            Some(Ok(r)) => r,
                            Some(Err(e)) => return Some(Err(e)),
                            None => return None,
                        };
                        if let Some(bin_op) = op {
                            let current = self.builder.fresh_value();
                            let load = match &base_ty {
                                IrType::Array { .. } | IrType::Slice { .. } => IrInstr::ArrayLoad {
                                    result: current,
                                    array: base_val,
                                    index: idx_vals[0],
                                    elem_ty: elem_ty.clone(),
                                },
                                IrType::List(_) => IrInstr::ListGet {
                                    result: current,
                                    list: base_val,
                                    index: idx_vals[0],
                                    elem_ty: elem_ty.clone(),
                                },
                                _ => IrInstr::Load {
                                    result: current,
                                    tensor: base_val,
                                    indices: idx_vals.clone(),
                                    result_ty: elem_ty.clone(),
                                },
                            };
                            self.builder.push_instr(load, Some(elem_ty.clone()));
                            value_val = self.compound(*bin_op, current, &elem_ty, value_val);
                        }
                        let store = match &base_ty {
                            IrType::Array { .. } | IrType::Slice { .. } => IrInstr::ArrayStore {
                                array: base_val,
                                index: idx_vals[0],
                                value: value_val,
                            },
                            IrType::List(_) => IrInstr::ListSet {
                                list: base_val,
                                index: idx_vals[0],
                                value: value_val,
                            },
                            _ => IrInstr::Store {
                                tensor: base_val,
                                indices: idx_vals,
                                value: value_val,
                            },
                        };
                        self.builder.push_instr(store, None);
                        Some(Ok(()))
                    }
                    crate::thir::ThirExprKind::Field { base, field, .. } => {
                        let value_val = match self.try_lower_thir_assign_rhs(*op, target, value) {
                            Some(Ok(v)) => v,
                            Some(Err(e)) => return Some(Err(e)),
                            None => return None,
                        };
                        Some(self.lower_thir_field_assignment(base, field, value_val, target.span))
                    }
                    crate::thir::ThirExprKind::TupleIndex { base, index } => {
                        let value_val = match self.try_lower_thir_assign_rhs(*op, target, value) {
                            Some(Ok(v)) => v,
                            Some(Err(e)) => return Some(Err(e)),
                            None => return None,
                        };
                        Some(self.lower_thir_tuple_assignment(base, *index, value_val, target.span))
                    }
                    _ => Some(Err(LowerError::rich(
                        "E0128",
                        crate::diagnostics::Diagnostic::error(
                            "cannot assign to this: the left of `=` must be a variable, an index, a field or a tuple element".to_string(),
                        )
                        .with_primary_span(s.span.start.0, s.span.end.0, None),
                    ))),
                }
            }
            crate::thir::ThirStmtKind::While { label, cond, body } => {
                Some(self.lower_thir_while(cond, body, label, s.span))
            }
            crate::thir::ThirStmtKind::Loop { label, body } => {
                Some(self.lower_thir_loop(body, label, s.span))
            }
            crate::thir::ThirStmtKind::ForRange {
                label,
                var,
                start,
                end,
                inclusive,
                step,
                body,
            } => Some(self.lower_thir_for_range(
                var,
                start,
                end,
                body,
                *inclusive,
                step.as_ref(),
                label,
                s.span,
            )),
            crate::thir::ThirStmtKind::ParFor {
                label: _,
                var,
                start,
                end,
                inclusive,
                body,
            } => Some(self.lower_thir_par_for(var, start, end, body, *inclusive, s.span)),
            crate::thir::ThirStmtKind::ForEach {
                label,
                var,
                iter,
                body,
            } => Some(self.lower_thir_foreach(var, iter, body, label, s.span)),
            crate::thir::ThirStmtKind::Mask { effects: _, body } => {
                Some(self.lower_thir_block(body).map(|_| ()))
            }
            crate::thir::ThirStmtKind::Defer(expr) => {
                self.thir_defer_stack.push(expr.clone());
                Some(Ok(()))
            }
            crate::thir::ThirStmtKind::Yield(expr) => {
                Some(self.lower_thir_expr_req(expr).map(|_| ()))
            }
            crate::thir::ThirStmtKind::Select { arms, default } => {
                Some(self.lower_thir_select(arms, default.as_ref(), s.span))
            }
            crate::thir::ThirStmtKind::Handle { expr, arms, return_ty } => {
                Some(self.lower_thir_handle(expr, arms, return_ty).map(|_| ()))
            }
            crate::thir::ThirStmtKind::Spawn { body, group } => {
                Some(self.lower_thir_spawn(body, group.as_ref(), s.span))
            }
            crate::thir::ThirStmtKind::Break { label } => {
                Some(self.lower_break(label, s.span).map(|_| ()))
            }
            crate::thir::ThirStmtKind::Continue { label } => {
                Some(self.lower_continue(label, s.span).map(|_| ()))
            }
        }
    }

    pub(super) fn lower_thir_deref_assign(
        &mut self,
        ptr_expr: &crate::thir::ThirExpr,
        op: Option<BinOp>,
        value: &crate::thir::ThirExpr,
        span: Span,
    ) -> Result<(), LowerError> {
        let (ptr_val, ptr_ty) = self.lower_thir_expr_keep_ref_req(ptr_expr)?;
        let IrType::MutRef(inner_box) = ptr_ty else {
            return Err(Self::deref_assign_error(&ptr_ty, span));
        };
        let elem_ty = *inner_box;
        let new_val = if let Some(bin_op) = op {
            let cur_val = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::CellLoad {
                    result: cur_val,
                    cell: ptr_val,
                    result_ty: elem_ty.clone(),
                },
                Some(elem_ty.clone()),
            );
            self.builder.note_value_span(cur_val, span.start.0);
            let (rhs_val, _) = self.lower_thir_expr_req(value)?;
            self.compound(bin_op, cur_val, &elem_ty, rhs_val)
        } else {
            let prev_binding_ty = self.binding_ty.replace(elem_ty.clone());
            let lowered = self.lower_thir_expr_for_expected_ty(value, Some(&elem_ty));
            self.binding_ty = prev_binding_ty;
            let (val, val_ty) = lowered?;
            if !assignable(&val_ty, &elem_ty) {
                return Err(LowerError::TypeMismatch {
                    expected: elem_ty.to_string(),
                    found: val_ty.to_string(),
                    span,
                });
            }
            val
        };
        self.builder.push_instr(
            IrInstr::CellStore {
                cell: ptr_val,
                value: new_val,
            },
            None,
        );
        Ok(())
    }

    pub(super) fn try_lower_thir_assign_rhs(
        &mut self,
        op: Option<crate::ir::instr::BinOp>,
        target: &crate::thir::ThirExpr,
        value: &crate::thir::ThirExpr,
    ) -> Option<Result<ValueId, LowerError>> {
        if let Some(bin_op) = op {
            let (lhs_val, lhs_ty) = match self.try_lower_thir_expr(target) {
                Some(Ok(r)) => r,
                Some(Err(e)) => return Some(Err(e)),
                None => return None,
            };
            let (rhs_val, _) = match self.try_lower_thir_expr(value) {
                Some(Ok(r)) => r,
                Some(Err(e)) => return Some(Err(e)),
                None => return None,
            };
            Some(Ok(self.compound(bin_op, lhs_val, &lhs_ty, rhs_val)))
        } else {
            match self.try_lower_thir_expr(value) {
                Some(Ok((v, _))) => Some(Ok(v)),
                Some(Err(e)) => Some(Err(e)),
                None => None,
            }
        }
    }

    pub(super) fn lower_thir_field_assignment(
        &mut self,
        base: &crate::thir::ThirExpr,
        field: &str,
        value_val: ValueId,
        span: Span,
    ) -> Result<(), LowerError> {
        let (base_val, base_ty) = self.lower_thir_expr_req(base)?;
        let struct_fields = match &base_ty {
            IrType::Struct { fields, .. } => fields.clone(),
            _ => {
                return Err(LowerError::rich(
                    "E0124",
                    crate::diagnostics::Diagnostic::error(format!(
                        "cannot assign a field of `{}`: only records have fields",
                        hints::type_name(&base_ty)
                    ))
                    .with_primary_span(span.start.0, span.end.0, None),
                ));
            }
        };
        let field_index = struct_fields
            .iter()
            .position(|(n, _)| n == field)
            .ok_or_else(|| {
                LowerError::rich(
                    "E0113",
                    crate::diagnostics::Diagnostic::error(format!(
                        "this record has no field `{}`",
                        field
                    ))
                    .with_primary_span(span.start.0, span.end.0, None),
                )
            })?;

        let mut new_fields = Vec::with_capacity(struct_fields.len());
        for (i, field_info) in struct_fields.iter().enumerate() {
            if i == field_index {
                new_fields.push(value_val);
            } else {
                let f_ty = field_info.1.clone();
                let f_val = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::GetField {
                        result: f_val,
                        base: base_val,
                        field_index: i,
                        result_ty: f_ty.clone(),
                    },
                    Some(f_ty),
                );
                new_fields.push(f_val);
            }
        }
        let result_ty = base_ty.clone();
        let result = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::MakeStruct {
                result,
                fields: new_fields,
                result_ty: result_ty.clone(),
            },
            Some(result_ty.clone()),
        );

        self.store_aggregate_to_thir_base(
            base,
            field_index,
            result,
            result_ty,
            span,
            "cannot assign to this field: it must belong to a variable",
        )
    }

    pub(super) fn lower_thir_tuple_assignment(
        &mut self,
        base: &crate::thir::ThirExpr,
        index: usize,
        value_val: ValueId,
        span: Span,
    ) -> Result<(), LowerError> {
        let (base_val, base_ty) = self.lower_thir_expr_req(base)?;
        let elem_types = match &base_ty {
            IrType::Tuple(elems) => elems.clone(),
            _ => {
                return Err(LowerError::rich(
                    "E0124",
                    crate::diagnostics::Diagnostic::error(format!(
                        "`.N` takes an element of a tuple, but this is `{}`",
                        hints::type_name(&base_ty)
                    ))
                    .with_primary_span(span.start.0, span.end.0, None),
                ));
            }
        };
        if index >= elem_types.len() {
            return Err(LowerError::rich(
                "E0125",
                crate::diagnostics::Diagnostic::error(format!(
                    "tuple index {} is out of range: the tuple has {} elements",
                    index,
                    elem_types.len()
                ))
                .with_primary_span(span.start.0, span.end.0, None),
            ));
        }

        let mut new_elements = Vec::with_capacity(elem_types.len());
        for (i, elem_ty) in elem_types.iter().enumerate() {
            if i == index {
                new_elements.push(value_val);
            } else {
                let f_ty = elem_ty.clone();
                let f_val = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::GetElement {
                        result: f_val,
                        base: base_val,
                        index: i,
                        result_ty: f_ty.clone(),
                    },
                    Some(f_ty),
                );
                new_elements.push(f_val);
            }
        }
        let result_ty = base_ty.clone();
        let result = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::MakeTuple {
                result,
                elements: new_elements,
                result_ty: result_ty.clone(),
            },
            Some(result_ty.clone()),
        );

        self.store_aggregate_to_thir_base(
            base,
            index,
            result,
            result_ty,
            span,
            "cannot assign to this tuple element: the tuple must be a variable",
        )
    }

    fn store_aggregate_to_thir_base(
        &mut self,
        base: &crate::thir::ThirExpr,
        place_idx: usize,
        result: ValueId,
        result_ty: IrType,
        span: Span,
        not_var_msg: &str,
    ) -> Result<(), LowerError> {
        match &base.kind {
            crate::thir::ThirExprKind::Value { name, .. }
                if !self.scope.contains_key(name) && self.module.state_slot(name).is_some() =>
            {
                self.state_set(name, result);
                Ok(())
            }
            crate::thir::ThirExprKind::Value { name, .. } => {
                let resolved = self.resolve_unqualified_name(name);
                if let Some((cell, _)) = self.cell_vars.get(&resolved).cloned() {
                    self.builder.push_instr(
                        IrInstr::CellStore {
                            cell,
                            value: result,
                        },
                        None,
                    );
                    return Ok(());
                }
                if let Some((cell, IrType::MutRef(_))) = self.load_scope_var(&resolved) {
                    self.builder.push_instr(
                        IrInstr::CellStore {
                            cell,
                            value: result,
                        },
                        None,
                    );
                    return Ok(());
                }
                self.record_assign_place(name, Some(place_idx), result, span);
                self.scope.insert(name.clone(), (result, result_ty));
                Ok(())
            }
            crate::thir::ThirExprKind::Deref(ptr_expr) => {
                let (ptr_val, ptr_ty) = self.lower_thir_expr_keep_ref_req(ptr_expr)?;
                if !matches!(ptr_ty, IrType::MutRef(_)) {
                    return Err(Self::deref_assign_error(&ptr_ty, span));
                }
                self.builder.push_instr(
                    IrInstr::CellStore {
                        cell: ptr_val,
                        value: result,
                    },
                    None,
                );
                Ok(())
            }
            crate::thir::ThirExprKind::Field {
                base: parent_base,
                field: parent_field,
                ..
            } => self.lower_thir_field_assignment(parent_base, parent_field, result, base.span),
            crate::thir::ThirExprKind::TupleIndex {
                base: parent_base,
                index: parent_index,
            } => self.lower_thir_tuple_assignment(parent_base, *parent_index, result, base.span),
            _ => {
                Err(LowerError::rich(
                    "E0128",
                    crate::diagnostics::Diagnostic::error(not_var_msg.to_string())
                        .with_primary_span(span.start.0, span.end.0, None),
                ))
            }
        }
    }

    pub(super) fn lower_thir_block_stmts(
        &mut self,
        b: &crate::thir::ThirBlock,
    ) -> Result<(), LowerError> {
        for stmt in &b.stmts {
            if self.builder.is_current_block_terminated() {
                break;
            }
            match self.lower_thir_stmt(stmt) {
                Some(Ok(())) => {}
                Some(Err(e)) => return Err(e),
                None => {
                    return Err(LowerError::rich(
                        "E0902",
                        crate::diagnostics::Diagnostic::error(
                            "internal compiler error: unmigratable THIR statement".to_string(),
                        )
                        .with_code("E0902"),
                    ))
                }
            }
        }
        Ok(())
    }

    pub(super) fn lower_thir_select(
        &mut self,
        arms: &[crate::thir::ThirSelectArm],
        default: Option<&crate::thir::ThirBlock>,
        _span: Span,
    ) -> Result<(), LowerError> {
        let outer_scope = self.scope.clone();
        let outer_cells = self.cell_vars.clone();
        let mut rebound_names: Vec<String> = Vec::new();
        for arm in arms {
            let _ = collect_rebound_vars_in_thir_block(&arm.body, &mut rebound_names, false);
        }
        if let Some(d) = default {
            let _ = collect_rebound_vars_in_thir_block(d, &mut rebound_names, false);
        }
        rebound_names
            .retain(|name| outer_scope.contains_key(name) && !outer_cells.contains_key(name));

        let loop_bb = self.builder.create_block(Some("select_loop"));
        let merge_bb = self.builder.create_block(Some("select_merge"));

        let mut rebound_params: Vec<(String, ValueId, IrType)> = Vec::new();
        for name in &rebound_names {
            let Some((_, ty)) = outer_scope.get(name) else {
                continue;
            };
            let param = self
                .builder
                .add_block_param(merge_bb, Some(name), ty.clone());
            rebound_params.push((name.clone(), param, ty.clone()));
        }

        self.builder.push_instr(
            IrInstr::Br {
                target: loop_bb,
                args: vec![],
            },
            None,
        );

        self.builder.set_current_block(loop_bb);
        let channels: Vec<(ValueId, IrType)> = arms
            .iter()
            .map(|arm| self.lower_thir_expr_req(&arm.channel))
            .collect::<Result<Vec<_>, LowerError>>()?;
        let channel_values: Vec<ValueId> = channels.iter().map(|(value, _)| *value).collect();

        let select_result = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BuiltinCall {
                result: select_result,
                name: "select".to_string(),
                args: channel_values,
                result_ty: IrType::Scalar(DType::I64),
            },
            Some(IrType::Scalar(DType::I64)),
        );

        let next_check_bb = self.builder.create_block(Some("select_no_match"));
        for (i, arm) in arms.iter().enumerate() {
            let idx_val = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::ConstInt {
                    result: idx_val,
                    value: i as i64,
                    ty: IrType::Scalar(DType::I64),
                },
                Some(IrType::Scalar(DType::I64)),
            );
            let cmp_val = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::BinOp {
                    result: cmp_val,
                    op: BinOp::CmpEq,
                    lhs: select_result,
                    rhs: idx_val,
                    ty: IrType::Scalar(DType::Bool),
                },
                Some(IrType::Scalar(DType::Bool)),
            );
            let arm_body_bb = self
                .builder
                .create_block(Some(&format!("select_arm_{}", i)));
            let fall_bb = if i < arms.len() - 1 {
                self.builder
                    .create_block(Some(&format!("select_check_{}", i + 1)))
            } else {
                next_check_bb
            };
            self.builder.push_instr(
                IrInstr::CondBr {
                    cond: cmp_val,
                    then_block: arm_body_bb,
                    else_block: fall_bb,
                    then_args: vec![],
                    else_args: vec![],
                },
                None,
            );

            self.builder.set_current_block(arm_body_bb);
            self.scope = outer_scope.clone();
            self.cell_vars = outer_cells.clone();
            let (channel, channel_ty) = &channels[i];
            let elem_ty = match self.chan_elem_types.get(channel) {
                Some(ty) => ty.clone(),
                None => match channel_ty {
                    IrType::Chan(elem) => (**elem).clone(),
                    _ => {
                        return Err(self.undetermined(arm.channel.span, "what this channel carries"))
                    }
                },
            };
            let recv_result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::ChanRecv {
                    result: recv_result,
                    chan: *channel,
                    elem_ty: elem_ty.clone(),
                },
                Some(elem_ty.clone()),
            );
            self.scope
                .insert(arm.binding.clone(), (recv_result, elem_ty));
            self.lower_thir_block_stmts(&arm.body)?;
            if !self.builder.is_current_block_terminated() {
                let mut merge_args: Vec<ValueId> = Vec::new();
                for name in &rebound_names {
                    let val = self
                        .scope
                        .get(name)
                        .or_else(|| outer_scope.get(name))
                        .map(|(v, _)| *v)
                        .expect("rebound variable missing from select arm scope");
                    merge_args.push(val);
                }
                self.builder.push_instr(
                    IrInstr::Br {
                        target: merge_bb,
                        args: merge_args,
                    },
                    None,
                );
            }

            if i < arms.len() - 1 {
                self.builder.set_current_block(fall_bb);
            }
        }

        self.builder.set_current_block(next_check_bb);
        self.scope = outer_scope.clone();
        self.cell_vars = outer_cells.clone();
        if let Some(default_block) = default {
            self.lower_thir_block_stmts(default_block)?;
            if !self.builder.is_current_block_terminated() {
                let mut merge_args: Vec<ValueId> = Vec::new();
                for name in &rebound_names {
                    let val = self
                        .scope
                        .get(name)
                        .or_else(|| outer_scope.get(name))
                        .map(|(v, _)| *v)
                        .expect("rebound variable missing from select default scope");
                    merge_args.push(val);
                }
                self.builder.push_instr(
                    IrInstr::Br {
                        target: merge_bb,
                        args: merge_args,
                    },
                    None,
                );
            }
        } else {
            let ms_val = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::ConstInt {
                    result: ms_val,
                    value: 1,
                    ty: IrType::Scalar(DType::I64),
                },
                Some(IrType::Scalar(DType::I64)),
            );
            let sleep_result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::SleepMs {
                    result: sleep_result,
                    ms: ms_val,
                },
                Some(IrType::Scalar(DType::I64)),
            );
            self.builder.push_instr(
                IrInstr::Br {
                    target: loop_bb,
                    args: vec![],
                },
                None,
            );
        }

        self.scope = outer_scope;
        self.cell_vars = outer_cells;
        for (name, param, ty) in rebound_params {
            self.scope.insert(name, (param, ty));
        }
        self.builder.set_current_block(merge_bb);
        Ok(())
    }

    pub(super) fn lower_thir_handle(
        &mut self,
        expr: &crate::thir::ThirExpr,
        arms: &[crate::thir::ThirHandlerArm],
        return_ty: &AstType,
    ) -> Result<(ValueId, IrType), LowerError> {
        let mut handler_arms = Vec::with_capacity(arms.len());
        for arm in arms {
            let handler_arm = self.lower_thir_handler_arm(arm)?;
            handler_arms.push(handler_arm);
        }

        self.builder
            .push_instr(IrInstr::PushHandler { arms: handler_arms }, None);

        let (body_val, body_ty) = self.lower_thir_expr_req(expr)?;

        self.builder.push_instr(IrInstr::PopHandler, None);

        let result_ty = if is_inferred_return(return_ty) {
            body_ty
        } else {
            self.resolve_ty(return_ty)?
        };
        Ok((body_val, result_ty))
    }

    pub(super) fn lower_thir_handler_arm(
        &mut self,
        arm: &crate::thir::ThirHandlerArm,
    ) -> Result<crate::ir::instr::HandlerArm, LowerError> {
        let counter = self.lambda_counter.get();
        self.lambda_counter.set(counter + 1);
        let fn_name = format!("__handler_{}", counter);

        let extern_fn = self
            .module
            .extern_fns
            .iter()
            .find(|ef| ef.name == arm.effect)
            .ok_or_else(|| {
                LowerError::rich(
                    "E0100",
                    crate::diagnostics::Diagnostic::error(format!(
                        "no effect operation `{}` is declared, so this handler has nothing to handle",
                        arm.effect
                    ))
                    .with_code("E0100")
                    .with_primary_span(arm.span.start.0, arm.span.end.0, None)
                    .with_help(format!("declare it: `extern def {}(...) -> T`", arm.effect)),
                )
            })?;
        if arm.params.len() > extern_fn.param_types.len() {
            return Err(LowerError::rich(
                "E0109",
                crate::diagnostics::Diagnostic::error(format!(
                    "the handler for `{}` takes {} values, but the operation passes {}",
                    arm.effect,
                    arm.params.len(),
                    extern_fn.param_types.len()
                ))
                .with_code("E0109")
                .with_primary_span(arm.span.start.0, arm.span.end.0, None),
            ));
        }
        let mut lifted_params: Vec<Param> = Vec::new();
        for (p, ty) in arm.params.iter().zip(&extern_fn.param_types) {
            lifted_params.push(Param {
                name: p.clone(),
                ty: ty.clone(),
            });
        }
        let cont_param_name = if let Some(ref rp) = arm.resume_param {
            rp.clone()
        } else {
            "__cont".to_string()
        };
        let cont_ty = IrType::WeakRef(Box::new(extern_fn.ret_ty.clone()));
        lifted_params.insert(
            0,
            Param {
                name: cont_param_name,
                ty: cont_ty.clone(),
            },
        );

        let temp_builder =
            IrFunctionBuilder::new_return_decided_by_body(&fn_name, lifted_params.clone());
        let mut handler_lowerer = self.new_sub_lowerer(temp_builder);
        handler_lowerer.resume_param_name = arm.resume_param.clone();

        let entry = handler_lowerer.builder.create_block(Some("entry"));
        handler_lowerer.builder.set_current_block(entry);

        if let Some(ref rp) = arm.resume_param {
            let val = handler_lowerer
                .builder
                .add_block_param(entry, Some(rp), cont_ty.clone());
            handler_lowerer.scope.insert(rp.clone(), (val, cont_ty));
        }
        for (p, ty) in arm.params.iter().zip(&extern_fn.param_types) {
            let ty = ty.clone();
            let val = handler_lowerer
                .builder
                .add_block_param(entry, Some(p), ty.clone());
            handler_lowerer.scope.insert(p.clone(), (val, ty));
        }

        let (ret_val, ret_ty) = handler_lowerer.lower_thir_expr_req(&arm.body)?;
        handler_lowerer.builder.push_instr(
            IrInstr::Return {
                values: vec![ret_val],
            },
            None,
        );
        handler_lowerer.builder.seal_unterminated_blocks();

        let mut ir_func = handler_lowerer.builder.build_returning(ret_ty);
        ir_func.capture_count = 0;

        self.lifted_fns.borrow_mut().push(ir_func);

        Ok(crate::ir::instr::HandlerArm {
            effect_name: arm.effect.clone(),
            func_name: fn_name,
            num_args: arm.params.len(),
            has_resume: arm.resume_param.is_some(),
        })
    }

    pub(super) fn lower_thir_spawn(
        &mut self,
        body: &crate::thir::ThirBlock,
        group: Option<&crate::thir::ThirExpr>,
        span: Span,
    ) -> Result<(), LowerError> {
        let counter = self.lambda_counter.get();
        self.lambda_counter.set(counter + 1);
        let fn_name = format!("__spawn_{}", counter);

        let mut capture_names: Vec<String> = self.scope.keys().cloned().collect();
        capture_names.sort();
        let mut captures: Vec<(String, ValueId, IrType)> = Vec::new();
        let mut capture_scope_ids: Vec<(ValueId, ValueId)> = Vec::new();
        for name in &capture_names {
            let Some(orig_vid) = self.scope.get(name).map(|(v, _)| *v) else {
                continue;
            };
            if let Some((loaded_vid, ty)) = self.load_scope_var(name) {
                captures.push((name.clone(), loaded_vid, ty));
                capture_scope_ids.push((orig_vid, loaded_vid));
            }
        }

        let lifted_params: Vec<crate::ir::function::Param> = captures
            .iter()
            .map(|(name, _, ty)| crate::ir::function::Param {
                name: name.clone(),
                ty: ty.clone(),
            })
            .collect();

        let temp_builder =
            IrFunctionBuilder::new(&fn_name, lifted_params, IrType::Scalar(DType::I64));
        let mut spawn_lowerer = self.new_sub_lowerer(temp_builder);
        let entry = spawn_lowerer.builder.create_block(Some("entry"));
        spawn_lowerer.builder.set_current_block(entry);
        let mut capture_val_map: Vec<(ValueId, ValueId)> = Vec::new();
        for ((name, outer_val, ty), (orig_vid, _)) in captures.iter().zip(capture_scope_ids.iter())
        {
            let inner_val = spawn_lowerer
                .builder
                .add_block_param(entry, Some(name), ty.clone());
            spawn_lowerer
                .scope
                .insert(name.clone(), (inner_val, ty.clone()));
            capture_val_map.push((*orig_vid, inner_val));
            if *outer_val != *orig_vid {
                capture_val_map.push((*outer_val, inner_val));
            }
        }
        for (outer_val, inner_val) in &capture_val_map {
            if let Some(elem_ty) = self.chan_elem_types.get(outer_val) {
                spawn_lowerer
                    .chan_elem_types
                    .insert(*inner_val, elem_ty.clone());
            }
        }
        if body.tail.is_some() {
            let _ = spawn_lowerer.lower_thir_block(body)?;
        } else {
            spawn_lowerer.lower_thir_block_stmts(body)?;
        }
        for (outer_val, inner_val) in &capture_val_map {
            if let Some(elem_ty) = spawn_lowerer.chan_elem_types.get(inner_val) {
                self.chan_elem_types
                    .entry(*outer_val)
                    .or_insert_with(|| elem_ty.clone());
            }
        }
        let dummy_ret = spawn_lowerer.builder.fresh_value();
        spawn_lowerer.builder.push_instr(
            IrInstr::ConstInt {
                result: dummy_ret,
                value: 0,
                ty: IrType::Scalar(DType::I64),
            },
            Some(IrType::Scalar(DType::I64)),
        );
        spawn_lowerer.builder.push_instr(
            IrInstr::Return {
                values: vec![dummy_ret],
            },
            None,
        );
        spawn_lowerer.builder.seal_unterminated_blocks();
        let mut ir_func = spawn_lowerer.builder.build();
        let used: HashSet<ValueId> = ir_func
            .blocks
            .iter()
            .flat_map(|block| block.all_operands())
            .collect();
        let keep: Vec<bool> = ir_func.blocks[0]
            .params
            .iter()
            .map(|param| used.contains(&param.id))
            .collect();
        let removed: HashSet<ValueId> = ir_func.blocks[0]
            .params
            .iter()
            .filter(|param| !used.contains(&param.id))
            .map(|param| param.id)
            .collect();
        ir_func.params = ir_func
            .params
            .into_iter()
            .zip(&keep)
            .filter_map(|(param, keep)| keep.then_some(param))
            .collect();
        captures = captures
            .into_iter()
            .zip(&keep)
            .filter_map(|(capture, keep)| keep.then_some(capture))
            .collect();
        ir_func.blocks[0]
            .params
            .retain(|param| used.contains(&param.id));
        ir_func.value_defs.retain(|id, _| !removed.contains(id));
        ir_func.value_types.retain(|id, _| !removed.contains(id));
        self.lifted_fns.borrow_mut().push(ir_func);

        let capture_vals: Vec<ValueId> = captures.iter().map(|(_, v, _)| *v).collect();
        if let Some(group_expr) = group {
            let (group_val, _) = self.lower_thir_expr_req(group_expr)?;
            self.builder.push_instr(
                IrInstr::TaskGroupSpawn {
                    group: group_val,
                    body_fn: fn_name,
                    args: capture_vals,
                },
                None,
            );
        } else {
            self.builder.push_instr(
                IrInstr::Spawn {
                    body_fn: fn_name,
                    args: capture_vals,
                },
                None,
            );
        }
        let _ = span;
        Ok(())
    }

    pub(super) fn lower_thir_lambda(
        &mut self,
        params: &[crate::thir::ThirParam],
        body: &crate::thir::ThirExpr,
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        // Convert THIR params to AST params for the existing resolver path.
        let mut ast_params = Vec::with_capacity(params.len());
        for p in params {
            let ty = p.ty.clone().ok_or_else(|| {
                LowerError::rich(
                    "E0100",
                    crate::diagnostics::Diagnostic::error(
                        "lambda parameter needs a type".to_string(),
                    )
                    .with_primary_span(span.start.0, span.end.0, None),
                )
            })?;
            ast_params.push(crate::parser::ast::AstParam {
                name: crate::parser::ast::Ident {
                    name: p.name.clone(),
                    span,
                },
                ty,
                default: None,
            });
        }

        let counter = self.lambda_counter.get();
        self.lambda_counter.set(counter + 1);
        let fn_name = format!("__lambda_{}", counter);

        let param_names: std::collections::HashSet<String> =
            ast_params.iter().map(|p| p.name.name.clone()).collect();

        let mut captures: Vec<(String, ValueId, IrType, Option<IrType>)> = self
            .scope
            .iter()
            .filter(|(name, _)| !param_names.contains(*name) && thir_expr_uses_name(body, name))
            .map(|(name, (vid, ty))| {
                if let Some((cell_vid, elem_ty)) = self.cell_vars.get(name) {
                    (
                        name.clone(),
                        *cell_vid,
                        IrType::MutRef(Box::new(elem_ty.clone())),
                        Some(elem_ty.clone()),
                    )
                } else {
                    (name.clone(), *vid, ty.clone(), None)
                }
            })
            .collect();
        captures.sort_by(|a, b| a.0.cmp(&b.0));

        let mut lifted_params: Vec<Param> = captures
            .iter()
            .map(|(name, _, ty, _)| Param {
                name: name.clone(),
                ty: ty.clone(),
            })
            .collect();
        for p in &ast_params {
            lifted_params.push(Param {
                name: p.name.name.clone(),
                ty: self.resolve_ty(&p.ty)?,
            });
        }

        let temp_builder =
            IrFunctionBuilder::new_return_decided_by_body(&fn_name, lifted_params.clone());
        let mut lambda_lowerer = self.new_sub_lowerer(temp_builder);
        lambda_lowerer.mut_borrowed_vars = super::find_mut_borrowed_vars_thir_expr(body);

        let entry = lambda_lowerer.builder.create_block(Some("entry"));
        lambda_lowerer.builder.set_current_block(entry);

        for (name, _, ty, cell_elem_ty) in &captures {
            let val = lambda_lowerer
                .builder
                .add_block_param(entry, Some(name), ty.clone());
            if let Some(elem_ty) = cell_elem_ty {
                lambda_lowerer
                    .cell_vars
                    .insert(name.clone(), (val, elem_ty.clone()));
                lambda_lowerer
                    .scope
                    .insert(name.clone(), (val, elem_ty.clone()));
            } else {
                lambda_lowerer.scope.insert(name.clone(), (val, ty.clone()));
            }
        }
        for p in &ast_params {
            let ty = self.resolve_ty(&p.ty)?;
            let val = lambda_lowerer
                .builder
                .add_block_param(entry, Some(&p.name.name), ty.clone());
            lambda_lowerer.bind_scope_var(p.name.name.clone(), val, ty);
        }

        let (ret_val, ret_ty) = match lambda_lowerer.try_lower_thir_expr(body) {
            Some(Ok(r)) => r,
            Some(Err(e)) => return Err(e),
            None => {
                return Err(LowerError::rich(
                    "E0902",
                    crate::diagnostics::Diagnostic::error(
                        "internal compiler error: unmigratable THIR lambda body".to_string(),
                    )
                    .with_code("E0902"),
                ))
            }
        };
        lambda_lowerer.builder.push_instr(
            IrInstr::Return {
                values: vec![ret_val],
            },
            None,
        );
        lambda_lowerer.builder.seal_unterminated_blocks();

        let mut ir_func = lambda_lowerer.builder.build_returning(ret_ty.clone());
        ir_func.capture_count = captures.len();
        self.lifted_fns.borrow_mut().push(ir_func);

        let capture_vals: Vec<ValueId> = captures.iter().map(|(_, v, _, _)| *v).collect();
        let closure_ty = IrType::Fn {
            params: lifted_params.iter().map(|p| p.ty.clone()).collect(),
            ret: Box::new(ret_ty),
        };
        let result = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::MakeClosure {
                result,
                fn_name: fn_name.clone(),
                captures: capture_vals,
                result_ty: closure_ty.clone(),
            },
            Some(closure_ty.clone()),
        );
        let _ = span;
        Ok((result, closure_ty))
    }

    pub(super) fn lower_thir_if_expr_with_keep_ref(
        &mut self,
        cond: &crate::thir::ThirExpr,
        then_blk: &crate::thir::ThirBlock,
        else_blk: Option<&crate::thir::ThirBlock>,
        rebound_names: Vec<String>,
        span: Span,
        keep_ref: bool,
    ) -> Result<(ValueId, IrType), LowerError> {
        let (cond_val, _) = match self.try_lower_thir_expr(cond) {
            Some(Ok(r)) => r,
            Some(Err(e)) => return Err(e),
            None => {
                return Err(LowerError::rich(
                    "E0902",
                    crate::diagnostics::Diagnostic::error(
                        "internal compiler error: unmigratable THIR if".to_string(),
                    )
                    .with_code("E0902"),
                ))
            }
        };
        let expected_ty = self.expected_expr_ty.take();
        let outer_scope = self.scope.clone();
        let outer_slots = self.scope_slots.clone();
        let outer_cells = self.cell_vars.clone();
        let mut rebound_names = rebound_names;
        rebound_names
            .retain(|name| outer_scope.contains_key(name) && !outer_cells.contains_key(name));

        if let Some(else_blk) = else_blk {
            let unit_ty = IrType::Scalar(DType::I64);
            let unit_val = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::ConstInt {
                    result: unit_val,
                    value: 0,
                    ty: unit_ty.clone(),
                },
                Some(unit_ty.clone()),
            );

            let then_bb = self.builder.create_block(Some("then"));
            let else_bb = self.builder.create_block(Some("else"));
            let merge_bb = self.builder.create_block(Some("merge"));

            self.builder.push_instr(
                IrInstr::CondBr {
                    cond: cond_val,
                    then_block: then_bb,
                    then_args: vec![],
                    else_block: else_bb,
                    else_args: vec![],
                },
                None,
            );

            self.builder.set_current_block(then_bb);
            self.scope = outer_scope.clone();
            self.scope_slots = outer_slots.clone();
            self.cell_vars = outer_cells.clone();
            let mut then_result = self.lower_thir_block_opt_with_keep_ref(then_blk, keep_ref)?;
            if let (Some(ref expected), Some((tv, tt))) = (&expected_ty, then_result.clone()) {
                let (tv, tt) = self.coerce_branch_mut_ref(tv, tt, expected, span);
                let (cv, ct) = self.coerce_to_trait_object(tv, tt, expected, span)?;
                then_result = Some((cv, ct));
            }
            let then_scope = self.scope.clone();
            let then_end_bb = self.builder.current_block();
            let then_terminated = self.builder.is_current_block_terminated();
            self.scope = outer_scope.clone();
            self.scope_slots = outer_slots.clone();
            self.cell_vars = outer_cells.clone();

            self.builder.set_current_block(else_bb);
            self.scope = outer_scope.clone();
            self.scope_slots = outer_slots.clone();
            self.cell_vars = outer_cells.clone();
            let mut else_result = self.lower_thir_block_opt_with_keep_ref(else_blk, keep_ref)?;
            if let (Some(ref expected), Some((ev, et))) = (&expected_ty, else_result.clone()) {
                let (ev, et) = self.coerce_branch_mut_ref(ev, et, expected, span);
                let (cv, ct) = self.coerce_to_trait_object(ev, et, expected, span)?;
                else_result = Some((cv, ct));
            }
            let else_scope = self.scope.clone();
            let else_end_bb = self.builder.current_block();
            let else_terminated = self.builder.is_current_block_terminated();
            self.scope = outer_scope.clone();
            self.scope_slots = outer_slots.clone();
            self.cell_vars = outer_cells.clone();

            let then_eff_ty = then_result
                .as_ref()
                .map(|(_, ty)| ty.clone())
                .unwrap_or_else(|| unit_ty.clone());
            let else_eff_ty = else_result
                .as_ref()
                .map(|(_, ty)| ty.clone())
                .unwrap_or_else(|| unit_ty.clone());
            let result_ty = Self::unify_branch_ref_types(
                !then_terminated,
                &then_eff_ty,
                !else_terminated,
                &else_eff_ty,
            )
            .unwrap_or_else(|| unit_ty.clone());

            let any_shared_borrow = Self::block_tail_is_shared_borrow_thir(then_blk)
                || Self::block_tail_is_shared_borrow_thir(else_blk);
            if !any_shared_borrow {
                if !then_terminated {
                    if let Some((tv, tt)) = then_result.take() {
                        self.builder.set_current_block(then_end_bb);
                        then_result = Some(self.coerce_branch_mut_ref(tv, tt, &result_ty, span));
                    }
                }
                if !else_terminated {
                    if let Some((ev, et)) = else_result.take() {
                        self.builder.set_current_block(else_end_bb);
                        else_result = Some(self.coerce_branch_mut_ref(ev, et, &result_ty, span));
                    }
                }
            }

            let mut then_br_block: Option<BlockId> = None;
            let mut else_br_block: Option<BlockId> = None;

            if !then_terminated {
                self.builder.set_current_block(then_end_bb);
                let mut then_args = vec![then_result
                    .as_ref()
                    .map(|(then_val, _)| *then_val)
                    .unwrap_or(unit_val)];
                for name in &rebound_names {
                    let rebound_val = then_scope
                        .get(name)
                        .or_else(|| outer_scope.get(name))
                        .map(|(val, _)| *val)
                        .expect("rebound variable missing from then-scope");
                    then_args.push(rebound_val);
                }
                then_br_block = Some(then_end_bb);
                self.builder.push_instr(
                    IrInstr::Br {
                        target: merge_bb,
                        args: then_args,
                    },
                    None,
                );
            }

            if !else_terminated {
                self.builder.set_current_block(else_end_bb);
                let mut else_args = vec![else_result
                    .as_ref()
                    .map(|(else_val, _)| *else_val)
                    .unwrap_or(unit_val)];
                for name in &rebound_names {
                    let rebound_val = else_scope
                        .get(name)
                        .or_else(|| outer_scope.get(name))
                        .map(|(val, _)| *val)
                        .expect("rebound variable missing from else-scope");
                    else_args.push(rebound_val);
                }
                else_br_block = Some(else_end_bb);
                self.builder.push_instr(
                    IrInstr::Br {
                        target: merge_bb,
                        args: else_args,
                    },
                    None,
                );
            }

            let result =
                self.builder
                    .add_block_param(merge_bb, Some("if_result"), result_ty.clone());
            let mut rebound_params = Vec::new();
            for name in &rebound_names {
                let Some((_, ty)) = outer_scope.get(name) else {
                    continue;
                };
                let param = self
                    .builder
                    .add_block_param(merge_bb, Some(name), ty.clone());
                rebound_params.push((name.clone(), param, ty.clone()));
            }
            let then_handle = then_result
                .as_ref()
                .and_then(|(v, _)| self.tape_nodes.get(v).copied());
            let else_handle = else_result
                .as_ref()
                .and_then(|(v, _)| self.tape_nodes.get(v).copied());
            if let (Some(th), Some(eh), Some(tb), Some(eb)) =
                (then_handle, else_handle, then_br_block, else_br_block)
            {
                let handle_param =
                    self.builder
                        .add_block_param(merge_bb, Some("if_result$tape"), IrType::TapeRef);
                let patched_then = self.builder.append_br_arg(tb, merge_bb, th);
                let patched_else = self.builder.append_br_arg(eb, merge_bb, eh);
                if patched_then && patched_else {
                    self.taped_values.insert(result);
                    self.tape_nodes.insert(result, handle_param);
                }
            }

            self.builder.set_current_block(merge_bb);
            self.cell_vars = outer_cells;
            for (name, param, ty) in rebound_params {
                if let Some(&slot) = self.scope_slots.get(&name) {
                    self.builder.map_value_slot_if_unset(param, slot);
                }
                self.scope.insert(name, (param, ty));
            }
            Ok((result, result_ty))
        } else {
            let unit_ty = IrType::Scalar(DType::I64);
            let unit_val = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::ConstInt {
                    result: unit_val,
                    value: 0,
                    ty: unit_ty.clone(),
                },
                Some(unit_ty.clone()),
            );

            let then_bb = self.builder.create_block(Some("then"));
            let merge_bb = self.builder.create_block(Some("merge"));

            self.builder.push_instr(
                IrInstr::CondBr {
                    cond: cond_val,
                    then_block: then_bb,
                    then_args: vec![],
                    else_block: merge_bb,
                    else_args: std::iter::once(unit_val)
                        .chain(
                            rebound_names
                                .iter()
                                .filter_map(|name| outer_scope.get(name).map(|(val, _)| *val)),
                        )
                        .collect(),
                },
                None,
            );

            self.builder.set_current_block(then_bb);
            self.scope = outer_scope.clone();
            self.scope_slots = outer_slots.clone();
            self.cell_vars = outer_cells.clone();
            let _then_result = self.lower_thir_block(then_blk)?;
            let then_scope = self.scope.clone();
            if !self.builder.is_current_block_terminated() {
                let mut then_args = vec![unit_val];
                for name in &rebound_names {
                    let rebound_val = then_scope
                        .get(name)
                        .or_else(|| outer_scope.get(name))
                        .map(|(val, _)| *val)
                        .expect("rebound variable missing from then-scope");
                    then_args.push(rebound_val);
                }
                self.builder.push_instr(
                    IrInstr::Br {
                        target: merge_bb,
                        args: then_args,
                    },
                    None,
                );
            }
            self.scope = outer_scope.clone();
            self.scope_slots = outer_slots.clone();
            self.cell_vars = outer_cells;

            let merge_param =
                self.builder
                    .add_block_param(merge_bb, Some("if_result"), unit_ty.clone());
            let mut rebound_params = Vec::new();
            for name in &rebound_names {
                let Some((_, ty)) = outer_scope.get(name) else {
                    continue;
                };
                let param = self
                    .builder
                    .add_block_param(merge_bb, Some(name), ty.clone());
                rebound_params.push((name.clone(), param, ty.clone()));
            }
            self.builder.set_current_block(merge_bb);
            for (name, param, ty) in rebound_params {
                if let Some(&slot) = self.scope_slots.get(&name) {
                    self.builder.map_value_slot_if_unset(param, slot);
                }
                self.scope.insert(name, (param, ty));
            }
            Ok((merge_param, unit_ty))
        }
    }

    pub(super) fn lower_thir_literal(
        &mut self,
        lit: &crate::thir::ThirLiteral,
        span: crate::parser::lexer::Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        match lit {
            crate::thir::ThirLiteral::Int(value) => {
                let result = self.builder.fresh_value();
                let ty = match self.typeck_type(span) {
                    Some(IrType::Scalar(d)) if d != DType::I64 && d.is_integer() => {
                        IrType::Scalar(d)
                    }
                    _ => IrType::Scalar(DType::I64),
                };
                self.builder.push_instr(
                    IrInstr::ConstInt {
                        result,
                        value: *value,
                        ty: ty.clone(),
                    },
                    Some(ty.clone()),
                );
                Ok((result, ty))
            }
            crate::thir::ThirLiteral::Float(value) => {
                let result = self.builder.fresh_value();
                let ty = match self.typeck_type(span) {
                    Some(IrType::Scalar(DType::F32)) => IrType::Scalar(DType::F32),
                    _ => IrType::Scalar(DType::F64),
                };
                self.builder.push_instr(
                    IrInstr::ConstFloat {
                        result,
                        value: *value,
                        ty: ty.clone(),
                    },
                    Some(ty.clone()),
                );
                Ok((result, ty))
            }
            crate::thir::ThirLiteral::Bool(value) => {
                let result = self.builder.fresh_value();
                let ty = IrType::Scalar(DType::Bool);
                self.builder.push_instr(
                    IrInstr::ConstBool {
                        result,
                        value: *value,
                    },
                    Some(ty.clone()),
                );
                Ok((result, ty))
            }
            crate::thir::ThirLiteral::Str(value) => {
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::ConstStr {
                        result,
                        value: value.clone(),
                    },
                    Some(IrType::Str),
                );
                Ok((result, IrType::Str))
            }
        }
    }

    pub(super) fn lower_thir_unary(
        &mut self,
        op: crate::parser::ast::AstUnaryOp,
        val: ValueId,
        ty: IrType,
        span: crate::parser::lexer::Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        let result = self.builder.fresh_value();
        let ir_op = match op {
            crate::parser::ast::AstUnaryOp::Neg => ScalarUnaryOp::Neg,
            crate::parser::ast::AstUnaryOp::Not => ScalarUnaryOp::Not,
        };
        self.builder.push_instr(
            IrInstr::UnaryOp {
                result,
                op: ir_op,
                operand: val,
                ty: ty.clone(),
            },
            Some(ty.clone()),
        );
        if matches!(ir_op, ScalarUnaryOp::Neg) {
            self.maybe_record_tape_result(result, &ty, "neg", &[val]);
        }
        let _ = span;
        Ok((result, ty))
    }
}
