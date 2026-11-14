//! Pattern-matching (`when`, `Option`, `Result`, literal/struct/enum patterns) lowering.

use std::collections::HashMap;

use crate::error::LowerError;
use crate::ir::block::BlockId;
use crate::ir::instr::{BinOp, IrInstr, ScalarUnaryOp};
use crate::ir::types::{DType, IrType};
use crate::ir::value::ValueId;
use crate::parser::ast::{AstExpr, AstWhenArm, AstWhenPattern};
use crate::parser::lexer::Span;

use super::{collect_rebound_vars_in_expr, collect_rebound_vars_in_thir_expr, hints, Lowerer};

/// Returns true if a pattern extracts variable bindings (needs safe evaluation order with guards).
pub(super) fn pattern_has_bindings(pattern: &AstWhenPattern) -> bool {
    match pattern {
        AstWhenPattern::Binding { .. } => true,
        AstWhenPattern::OptionSome { binding: Some(_) } => true,
        AstWhenPattern::ResultOk { binding: Some(_) } => true,
        AstWhenPattern::ResultErr { binding: Some(_) } => true,
        AstWhenPattern::EnumVariant { bindings, .. } if !bindings.is_empty() => true,
        // A tuple pattern has bindings if ANY sub-pattern is an ident binding
        AstWhenPattern::Tuple(subs) => subs.iter().any(|s| pattern_has_bindings(s)
            || matches!(s, AstWhenPattern::EnumVariant { enum_name, .. } if enum_name.is_empty())),
        // A struct pattern has bindings if any field sub-pattern has bindings or is a bare ident
        AstWhenPattern::Struct { fields, .. } => fields.iter().any(|(_, f)| pattern_has_bindings(f)
            || matches!(f, AstWhenPattern::EnumVariant { enum_name, .. } if enum_name.is_empty())
            || matches!(f, AstWhenPattern::Wildcard)),
        _ => false,
    }
}

impl<'m> Lowerer<'m> {
    fn collect_pending_when_rebound_vals(
        rebound_names: &[String],
        arm_scope: &std::collections::HashMap<String, (ValueId, IrType)>,
        outer_scope: &std::collections::HashMap<String, (ValueId, IrType)>,
    ) -> Vec<ValueId> {
        rebound_names
            .iter()
            .map(|name| {
                arm_scope
                    .get(name)
                    .or_else(|| outer_scope.get(name))
                    .map(|(v, _)| *v)
                    .expect("rebound variable missing from arm and outer scope")
            })
            .collect()
    }

    fn emit_when_merge_branches(
        &mut self,
        merge_bb: BlockId,
        default_ty: &IrType,
        branches: Vec<(BlockId, ValueId, IrType, Vec<ValueId>, bool)>,
        span: Span,
    ) -> IrType {
        let result_ty = if !branches.is_empty()
            && branches
                .iter()
                .all(|(_, _, ty, _, _)| matches!(ty, IrType::MutRef(_)))
        {
            branches[0].2.clone()
        } else if let Some((_, _, ty, _, _)) = branches
            .iter()
            .find(|(_, _, ty, _, _)| !matches!(ty, IrType::MutRef(_)))
        {
            ty.clone()
        } else {
            default_ty.clone()
        };
        let any_shared_borrow = branches.iter().any(|(_, _, _, _, is_shared)| *is_shared);
        for (end_bb, val, ty, rebound_vals, _) in branches {
            self.builder.set_current_block(end_bb);
            let val = if !any_shared_borrow {
                self.coerce_branch_mut_ref(val, ty, &result_ty, span).0
            } else {
                val
            };
            let mut args = Vec::with_capacity(1 + rebound_vals.len());
            args.push(val);
            args.extend(rebound_vals);
            self.builder.push_instr(
                IrInstr::Br {
                    target: merge_bb,
                    args,
                },
                None,
            );
        }
        result_ty
    }

    pub(super) fn lower_thir_when_expr_with_keep_ref(
        &mut self,
        scrutinee: &crate::thir::ThirExpr,
        arms: &[crate::thir::ThirArm],
        span: Span,
        keep_ref: bool,
    ) -> Result<(ValueId, IrType), LowerError> {
        if arms.is_empty() {
            return Err(LowerError::rich(
                "E0127",
                crate::diagnostics::Diagnostic::error(
                    "a `when` needs at least one arm".to_string(),
                )
                .with_primary_span(span.start.0, span.end.0, None),
            ));
        }

        let (scrut_val, scrut_ty) = self.lower_thir_expr_req(scrutinee)?;

        let is_option_when = arms.iter().any(|a| {
            matches!(
                a.pattern,
                AstWhenPattern::OptionSome { .. } | AstWhenPattern::OptionNone
            )
        });
        let is_result_when = arms.iter().any(|a| {
            matches!(
                a.pattern,
                AstWhenPattern::ResultOk { .. } | AstWhenPattern::ResultErr { .. }
            )
        });

        let option_has_guards = is_option_when && arms.iter().any(|a| a.guard.is_some());
        let result_has_guards = is_result_when && arms.iter().any(|a| a.guard.is_some());

        if is_option_when && !option_has_guards {
            return self.lower_thir_option_when_with_keep_ref(
                scrut_val, &scrut_ty, arms, span, keep_ref,
            );
        }
        if is_result_when && !result_has_guards {
            return self.lower_thir_result_when_with_keep_ref(
                scrut_val, &scrut_ty, arms, span, keep_ref,
            );
        }

        let is_enum_scrut = matches!(&scrut_ty, IrType::Enum { .. });
        let has_guard = arms.iter().any(|a| a.guard.is_some());
        let needs_chain = has_guard || !is_enum_scrut;
        if needs_chain {
            return self.lower_thir_when_as_chain_with_keep_ref(
                scrut_val, &scrut_ty, arms, span, keep_ref,
            );
        }

        let (enum_name, variants) = match &scrut_ty {
            IrType::Enum { name, variants } => (name.clone(), variants.clone()),
            _ => {
                return Err(LowerError::rich(
                    "E0124",
                    crate::diagnostics::Diagnostic::error(format!(
                        "this `when` matches variants, but the value is `{}`, not a choice",
                        hints::type_name(&scrut_ty)
                    ))
                    .with_primary_span(span.start.0, span.end.0, None),
                ));
            }
        };

        let mut arm_blocks: Vec<BlockId> = Vec::new();
        let mut default_block_opt: Option<BlockId> = None;
        for (arm_idx, arm) in arms.iter().enumerate() {
            match &arm.pattern {
                AstWhenPattern::Wildcard => {
                    let bb = self
                        .builder
                        .create_block(Some(&format!("when_{}_wildcard", enum_name)));
                    default_block_opt = Some(bb);
                    arm_blocks.push(bb);
                }
                AstWhenPattern::EnumVariant { variant_name, .. } => {
                    arm_blocks.push(
                        self.builder
                            .create_block(Some(&format!("when_{}_{}", enum_name, variant_name))),
                    );
                }
                AstWhenPattern::Or(_) => {
                    arm_blocks.push(
                        self.builder
                            .create_block(Some(&format!("when_or_{}", arm_idx))),
                    );
                }
                AstWhenPattern::Binding { .. } => {
                    return self.lower_thir_when_as_chain_with_keep_ref(
                        scrut_val, &scrut_ty, arms, span, keep_ref,
                    );
                }
                _ => {
                    return Err(LowerError::rich(
                        "E0103",
                        crate::diagnostics::Diagnostic::error(
                            "this pattern cannot match a choice value: use `Choice.Variant` or `Choice.Variant(a, b)`".to_string(),
                        )
                        .with_primary_span(arm.span.start.0, arm.span.end.0, None),
                    ));
                }
            }
        }
        let merge_bb = self.builder.create_block(Some("when_merge"));

        let mut switch_arms: Vec<(usize, BlockId)> = Vec::new();
        for (arm_idx, arm) in arms.iter().enumerate() {
            match &arm.pattern {
                AstWhenPattern::EnumVariant { variant_name, .. } => {
                    let variant_idx =
                        variants
                            .iter()
                            .position(|v| v == variant_name)
                            .ok_or_else(|| {
                                LowerError::rich(
                                    "E0122",
                                    crate::diagnostics::Diagnostic::error(format!(
                                        "`{1}` has no variant `{0}`",
                                        variant_name, enum_name
                                    ))
                                    .with_primary_span(
                                        arm.span.start.0,
                                        arm.span.end.0,
                                        None,
                                    ),
                                )
                            })?;
                    switch_arms.push((variant_idx, arm_blocks[arm_idx]));
                }
                AstWhenPattern::Or(subs) => {
                    for sub in subs {
                        if let AstWhenPattern::EnumVariant { variant_name, .. } = sub {
                            let variant_idx = variants
                                .iter()
                                .position(|v| v == variant_name)
                                .ok_or_else(|| {
                                    LowerError::rich(
                                        "E0122",
                                        crate::diagnostics::Diagnostic::error(format!(
                                            "`{1}` has no variant `{0}`",
                                            variant_name, enum_name
                                        ))
                                        .with_primary_span(arm.span.start.0, arm.span.end.0, None),
                                    )
                                })?;
                            switch_arms.push((variant_idx, arm_blocks[arm_idx]));
                        } else {
                            return Err(LowerError::rich(
                                "E0103",
                                crate::diagnostics::Diagnostic::error(
                                    "this pattern is not supported inside `|` alternatives: give it its own arm".to_string(),
                                )
                                .with_primary_span(arm.span.start.0, arm.span.end.0, None),
                            ));
                        }
                    }
                }
                AstWhenPattern::Wildcard => {}
                _ => {
                    return Err(LowerError::rich(
                        "E0103",
                        crate::diagnostics::Diagnostic::error(
                            "this pattern cannot match a choice value: use `Choice.Variant` or `Choice.Variant(a, b)`".to_string(),
                        )
                        .with_primary_span(arm.span.start.0, arm.span.end.0, None),
                    ));
                }
            }
        }

        self.builder.push_instr(
            IrInstr::SwitchVariant {
                scrutinee: scrut_val,
                arms: switch_arms,
                default_block: default_block_opt,
            },
            None,
        );

        let variant_field_types: Vec<Vec<IrType>> = self
            .module
            .enum_variant_fields(&enum_name)
            .cloned()
            .unwrap_or_default();

        let outer_scope = self.scope.clone();
        let outer_cells = self.cell_vars.clone();
        let mut rebound_names: Vec<String> = Vec::new();
        for arm in arms {
            let _ = collect_rebound_vars_in_thir_expr(&arm.body, &mut rebound_names);
        }
        rebound_names.retain(|name| outer_scope.contains_key(name) && !outer_cells.contains_key(name));

        let mut fallback_ty: Option<IrType> = None;
        let mut pending_branches = Vec::new();
        for (arm, &arm_bb) in arms.iter().zip(arm_blocks.iter()) {
            self.scope = outer_scope.clone();
            self.cell_vars = outer_cells.clone();
            self.builder.set_current_block(arm_bb);

            if let AstWhenPattern::EnumVariant {
                variant_name,
                bindings,
                ..
            } = &arm.pattern
            {
                if !bindings.is_empty() {
                    let vidx = variants.iter().position(|v| v == variant_name);
                    let empty_fields: Vec<IrType> = Vec::new();
                    let field_types: &Vec<IrType> = vidx
                        .and_then(|i| variant_field_types.get(i))
                        .unwrap_or(&empty_fields);
                    let variant_idx = vidx.unwrap_or(0);
                    if bindings.len() > field_types.len() {
                        return Err(LowerError::rich(
                            "E0125",
                            crate::diagnostics::Diagnostic::error(format!(
                                "`{enum_name}.{variant_name}` has {} field{}, but the pattern binds {}",
                                field_types.len(),
                                if field_types.len() == 1 { "" } else { "s" },
                                bindings.len()
                            ))
                            .with_code("E0125")
                            .with_primary_span(arm.span.start.0, arm.span.end.0, None),
                        ));
                    }
                    for (field_idx, binding_name) in bindings.iter().enumerate() {
                        let field_ty = field_types[field_idx].clone();
                        let result = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::ExtractVariantField {
                                result,
                                operand: scrut_val,
                                variant_idx,
                                field_idx,
                                result_ty: field_ty.clone(),
                            },
                            Some(field_ty.clone()),
                        );
                        self.bind_scope_var(binding_name.clone(), result, field_ty);
                    }
                }
            }

            let (arm_val, arm_ty) = if keep_ref {
                self.lower_thir_expr_keep_ref_req(&arm.body)?
            } else {
                self.lower_thir_expr_req(&arm.body)?
            };
            if fallback_ty.is_none() {
                fallback_ty = Some(arm_ty.clone());
            }
            if !self.builder.is_current_block_terminated() {
                let rebound_vals = Self::collect_pending_when_rebound_vals(
                    &rebound_names,
                    &self.scope,
                    &outer_scope,
                );
                pending_branches.push((
                    self.builder.current_block(),
                    arm_val,
                    arm_ty,
                    rebound_vals,
                    Self::expr_is_shared_borrow_thir(&arm.body),
                ));
            }
        }
        self.scope = outer_scope;
        self.cell_vars = outer_cells;

        let default_ty = fallback_ty.unwrap_or(IrType::Scalar(DType::I64));
        let result_ty = self.emit_when_merge_branches(merge_bb, &default_ty, pending_branches, span);
        let result = self
            .builder
            .add_block_param(merge_bb, Some("when_result"), result_ty.clone());
        let mut rebound_params = Vec::new();
        for name in &rebound_names {
            let Some((_, ty)) = self.scope.get(name).cloned() else {
                continue;
            };
            let param = self
                .builder
                .add_block_param(merge_bb, Some(name), ty.clone());
            rebound_params.push((name.clone(), param, ty));
        }
        self.builder.set_current_block(merge_bb);
        for (name, param, ty) in rebound_params {
            self.scope.insert(name, (param, ty));
        }

        Ok((result, result_ty))
    }

    pub(super) fn lower_thir_option_when_with_keep_ref(
        &mut self,
        scrut_val: ValueId,
        scrut_ty: &IrType,
        arms: &[crate::thir::ThirArm],
        span: Span,
        keep_ref: bool,
    ) -> Result<(ValueId, IrType), LowerError> {
        let inner_ty = match scrut_ty {
            IrType::Option(inner) => (**inner).clone(),
            other => {
                return Err(LowerError::TypeMismatch {
                    expected: "an option, for `some`/`none` arms".into(),
                    found: other.to_string(),
                    span,
                })
            }
        };
        let some_arm = arms
            .iter()
            .find(|a| matches!(a.pattern, AstWhenPattern::OptionSome { .. }));
        let none_arm = arms
            .iter()
            .find(|a| matches!(a.pattern, AstWhenPattern::OptionNone))
            .or_else(|| {
                arms.iter()
                    .find(|a| matches!(a.pattern, AstWhenPattern::Wildcard))
            });

        if some_arm.is_none() && none_arm.is_none() {
            return Err(LowerError::rich(
                "E0127",
                crate::diagnostics::Diagnostic::error(
                    "a `when` on an option needs `some(...)` and `none` arms".to_string(),
                )
                .with_primary_span(span.start.0, span.end.0, None),
            ));
        }

        let is_some_result = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::IsSome {
                result: is_some_result,
                operand: scrut_val,
            },
            Some(IrType::Scalar(DType::Bool)),
        );

        let some_bb = self.builder.create_block(Some("option_some"));
        let none_bb = self.builder.create_block(Some("option_none"));
        let merge_bb = self.builder.create_block(Some("option_merge"));

        let outer_scope = self.scope.clone();
        let outer_cells = self.cell_vars.clone();
        let mut rebound_names = Vec::new();
        for arm in arms {
            let _ = collect_rebound_vars_in_thir_expr(&arm.body, &mut rebound_names);
        }
        rebound_names.retain(|name| outer_scope.contains_key(name) && !outer_cells.contains_key(name));

        let unit_ty = IrType::Scalar(DType::I64);
        let partial = some_arm.is_none() || none_arm.is_none();

        let unit_val = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: unit_val,
                value: 0,
                ty: unit_ty.clone(),
            },
            Some(unit_ty.clone()),
        );

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

        let mut pending_branches = Vec::new();
        self.builder.set_current_block(some_bb);
        self.scope = outer_scope.clone();
        self.cell_vars = outer_cells.clone();
        let (some_val, mut first_ty): (ValueId, Option<IrType>) = if let Some(arm) = some_arm {
            if let AstWhenPattern::OptionSome {
                binding: Some(ref bind_name),
            } = arm.pattern
            {
                let unwrapped = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::OptionUnwrap {
                        result: unwrapped,
                        operand: scrut_val,
                        result_ty: inner_ty.clone(),
                    },
                    Some(inner_ty.clone()),
                );
                self.bind_scope_var(bind_name.clone(), unwrapped, inner_ty.clone());
            }
            let (v, ty) = if keep_ref {
                self.lower_thir_expr_keep_ref_req(&arm.body)?
            } else {
                self.lower_thir_expr_req(&arm.body)?
            };
            (v, Some(ty))
        } else {
            (unit_val, Some(unit_ty.clone()))
        };
        if !self.builder.is_current_block_terminated() {
            let rebound_vals =
                Self::collect_pending_when_rebound_vals(&rebound_names, &self.scope, &outer_scope);
            pending_branches.push((
                self.builder.current_block(),
                some_val,
                first_ty.clone().unwrap_or_else(|| unit_ty.clone()),
                rebound_vals,
                some_arm.is_some_and(|arm| Self::expr_is_shared_borrow_thir(&arm.body)),
            ));
        }

        self.builder.set_current_block(none_bb);
        self.scope = outer_scope.clone();
        self.cell_vars = outer_cells.clone();
        let (none_val, none_ty) = if let Some(arm) = none_arm {
            let (v, ty) = if keep_ref {
                self.lower_thir_expr_keep_ref_req(&arm.body)?
            } else {
                self.lower_thir_expr_req(&arm.body)?
            };
            if first_ty.is_none() {
                first_ty = Some(ty.clone());
            }
            (v, ty)
        } else if partial {
            let def_ty = match first_ty.clone() {
                Some(IrType::MutRef(inner)) => *inner,
                Some(t) => t,
                None => unit_ty.clone(),
            };
            (self.emit_default_value(&def_ty), def_ty)
        } else {
            (unit_val, unit_ty.clone())
        };
        if !self.builder.is_current_block_terminated() {
            let rebound_vals =
                Self::collect_pending_when_rebound_vals(&rebound_names, &self.scope, &outer_scope);
            pending_branches.push((
                self.builder.current_block(),
                none_val,
                none_ty,
                rebound_vals,
                none_arm.is_some_and(|arm| Self::expr_is_shared_borrow_thir(&arm.body)),
            ));
        }

        self.scope = outer_scope;
        self.cell_vars = outer_cells;
        let default_ty = first_ty.unwrap_or(unit_ty);
        let result_ty = self.emit_when_merge_branches(merge_bb, &default_ty, pending_branches, span);
        let result =
            self.builder
                .add_block_param(merge_bb, Some("option_result"), result_ty.clone());
        let mut rebound_params = Vec::new();
        for name in &rebound_names {
            let Some((_, ty)) = self.scope.get(name) else {
                continue;
            };
            let param = self
                .builder
                .add_block_param(merge_bb, Some(name), ty.clone());
            rebound_params.push((name.clone(), param, ty.clone()));
        }
        self.builder.set_current_block(merge_bb);
        for (name, param, ty) in rebound_params {
            self.scope.insert(name, (param, ty));
        }
        Ok((result, result_ty))
    }

    pub(super) fn lower_thir_result_when_with_keep_ref(
        &mut self,
        scrut_val: ValueId,
        scrut_ty: &IrType,
        arms: &[crate::thir::ThirArm],
        span: Span,
        keep_ref: bool,
    ) -> Result<(ValueId, IrType), LowerError> {
        let (ok_inner_ty, err_inner_ty) = match scrut_ty {
            IrType::ResultType(ok, err) => ((**ok).clone(), (**err).clone()),
            other => {
                return Err(LowerError::TypeMismatch {
                    expected: "a result, for `ok`/`err` arms".into(),
                    found: other.to_string(),
                    span,
                })
            }
        };
        let ok_arm = arms
            .iter()
            .find(|a| matches!(a.pattern, AstWhenPattern::ResultOk { .. }));
        let err_arm = arms
            .iter()
            .find(|a| matches!(a.pattern, AstWhenPattern::ResultErr { .. }))
            .or_else(|| {
                arms.iter()
                    .find(|a| matches!(a.pattern, AstWhenPattern::Wildcard))
            });

        if ok_arm.is_none() && err_arm.is_none() {
            return Err(LowerError::rich(
                "E0127",
                crate::diagnostics::Diagnostic::error(
                    "a `when` on a result needs `ok(...)` and `err(...)` arms".to_string(),
                )
                .with_primary_span(span.start.0, span.end.0, None),
            ));
        }

        let is_ok_result = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::IsOk {
                result: is_ok_result,
                operand: scrut_val,
            },
            Some(IrType::Scalar(DType::Bool)),
        );

        let ok_bb = self.builder.create_block(Some("result_ok"));
        let err_bb = self.builder.create_block(Some("result_err"));
        let merge_bb = self.builder.create_block(Some("result_merge"));

        let outer_scope = self.scope.clone();
        let outer_cells = self.cell_vars.clone();
        let mut rebound_names = Vec::new();
        for arm in arms {
            let _ = collect_rebound_vars_in_thir_expr(&arm.body, &mut rebound_names);
        }
        rebound_names.retain(|name| outer_scope.contains_key(name) && !outer_cells.contains_key(name));

        let unit_ty = IrType::Scalar(DType::I64);
        let partial = ok_arm.is_none() || err_arm.is_none();

        let unit_val = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: unit_val,
                value: 0,
                ty: unit_ty.clone(),
            },
            Some(unit_ty.clone()),
        );

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

        let mut pending_branches = Vec::new();
        self.builder.set_current_block(ok_bb);
        self.scope = outer_scope.clone();
        self.cell_vars = outer_cells.clone();
        let (ok_val, mut first_ty): (ValueId, Option<IrType>) = if let Some(arm) = ok_arm {
            if let AstWhenPattern::ResultOk {
                binding: Some(ref bind_name),
            } = arm.pattern
            {
                let unwrapped = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::ResultUnwrap {
                        result: unwrapped,
                        operand: scrut_val,
                        result_ty: ok_inner_ty.clone(),
                    },
                    Some(ok_inner_ty.clone()),
                );
                self.bind_scope_var(bind_name.clone(), unwrapped, ok_inner_ty.clone());
            }
            let (v, ty) = if keep_ref {
                self.lower_thir_expr_keep_ref_req(&arm.body)?
            } else {
                self.lower_thir_expr_req(&arm.body)?
            };
            (v, Some(ty))
        } else {
            (unit_val, Some(unit_ty.clone()))
        };
        if !self.builder.is_current_block_terminated() {
            let rebound_vals =
                Self::collect_pending_when_rebound_vals(&rebound_names, &self.scope, &outer_scope);
            pending_branches.push((
                self.builder.current_block(),
                ok_val,
                first_ty.clone().unwrap_or_else(|| unit_ty.clone()),
                rebound_vals,
                ok_arm.is_some_and(|arm| Self::expr_is_shared_borrow_thir(&arm.body)),
            ));
        }

        self.builder.set_current_block(err_bb);
        self.scope = outer_scope.clone();
        self.cell_vars = outer_cells.clone();
        let (err_val, err_ty) = if let Some(arm) = err_arm {
            if let AstWhenPattern::ResultErr {
                binding: Some(ref bind_name),
            } = arm.pattern
            {
                let unwrapped = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::ResultUnwrapErr {
                        result: unwrapped,
                        operand: scrut_val,
                        result_ty: err_inner_ty.clone(),
                    },
                    Some(err_inner_ty.clone()),
                );
                self.bind_scope_var(bind_name.clone(), unwrapped, err_inner_ty.clone());
            }
            let (v, ty) = if keep_ref {
                self.lower_thir_expr_keep_ref_req(&arm.body)?
            } else {
                self.lower_thir_expr_req(&arm.body)?
            };
            if first_ty.is_none() {
                first_ty = Some(ty.clone());
            }
            (v, ty)
        } else if partial {
            let def_ty = match first_ty.clone() {
                Some(IrType::MutRef(inner)) => *inner,
                Some(t) => t,
                None => unit_ty.clone(),
            };
            (self.emit_default_value(&def_ty), def_ty)
        } else {
            (unit_val, unit_ty.clone())
        };
        if !self.builder.is_current_block_terminated() {
            let rebound_vals =
                Self::collect_pending_when_rebound_vals(&rebound_names, &self.scope, &outer_scope);
            pending_branches.push((
                self.builder.current_block(),
                err_val,
                err_ty,
                rebound_vals,
                err_arm.is_some_and(|arm| Self::expr_is_shared_borrow_thir(&arm.body)),
            ));
        }

        self.scope = outer_scope;
        self.cell_vars = outer_cells;
        let default_ty = first_ty.unwrap_or(unit_ty);
        let result_ty = self.emit_when_merge_branches(merge_bb, &default_ty, pending_branches, span);
        let result =
            self.builder
                .add_block_param(merge_bb, Some("result_result"), result_ty.clone());
        let mut rebound_params = Vec::new();
        for name in &rebound_names {
            let Some((_, ty)) = self.scope.get(name) else {
                continue;
            };
            let param = self
                .builder
                .add_block_param(merge_bb, Some(name), ty.clone());
            rebound_params.push((name.clone(), param, ty.clone()));
        }
        self.builder.set_current_block(merge_bb);
        for (name, param, ty) in rebound_params {
            self.scope.insert(name, (param, ty));
        }
        Ok((result, result_ty))
    }

    pub(super) fn lower_thir_when_as_chain_with_keep_ref(
        &mut self,
        scrut_val: ValueId,
        scrut_ty: &IrType,
        arms: &[crate::thir::ThirArm],
        span: Span,
        keep_ref: bool,
    ) -> Result<(ValueId, IrType), LowerError> {
        let merge_bb = self.builder.create_block(Some("when_merge"));
        let no_match_bb = self.builder.create_block(Some("when_no_match"));
        let _no_match_scrut = self
            .builder
            .add_block_param(no_match_bb, None, scrut_ty.clone());

        let outer_scope = self.scope.clone();
        let outer_cells = self.cell_vars.clone();
        let mut rebound_names = Vec::new();
        for arm in arms {
            let _ = collect_rebound_vars_in_thir_expr(&arm.body, &mut rebound_names);
        }
        rebound_names
            .retain(|name| outer_scope.contains_key(name) && !outer_cells.contains_key(name));

        let mut pending_branches: Vec<(BlockId, ValueId, IrType, Vec<ValueId>, bool)> = Vec::new();

        let (enum_name_opt, enum_variants_opt): (Option<String>, Option<Vec<String>>) =
            if let IrType::Enum { name, variants } = scrut_ty {
                (Some(name.clone()), Some(variants.clone()))
            } else {
                (None, None)
            };
        let enum_variant_field_types: Vec<Vec<IrType>> = if let Some(ref ename) = enum_name_opt {
            self.module
                .enum_variant_fields(ename)
                .cloned()
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        let mut current_check_bb = self.builder.current_block();
        let mut current_scrut_val = scrut_val;

        for (arm_idx, arm) in arms.iter().enumerate() {
            let is_last = arm_idx == arms.len() - 1;
            let has_guard_with_bindings = arm.guard.is_some() && pattern_has_bindings(&arm.pattern);

            let (bind_guard_bb_opt, guard_bb_scrut_opt) = if has_guard_with_bindings {
                let bb = self
                    .builder
                    .create_block(Some(&format!("when_guard_{}", arm_idx)));
                let scrut_param = self.builder.add_block_param(bb, None, scrut_ty.clone());
                (Some(bb), Some(scrut_param))
            } else {
                (None, None)
            };

            let arm_body_bb = self
                .builder
                .create_block(Some(&format!("when_arm_{}", arm_idx)));
            let arm_bb_scrut = self
                .builder
                .add_block_param(arm_body_bb, None, scrut_ty.clone());

            let (next_check_bb, next_bb_scrut_opt) = if is_last {
                (no_match_bb, None)
            } else {
                let bb = self
                    .builder
                    .create_block(Some(&format!("when_check_{}", arm_idx + 1)));
                let scrut_param = self.builder.add_block_param(bb, None, scrut_ty.clone());
                (bb, Some(scrut_param))
            };

            self.builder.set_current_block(current_check_bb);
            self.scope = outer_scope.clone();
            self.cell_vars = outer_cells.clone();

            let pat_cond = self.emit_pattern_condition(
                current_scrut_val,
                scrut_ty,
                &arm.pattern,
                &enum_name_opt,
                &enum_variants_opt,
                span,
            )?;

            let (body_scope, body_cells) = if let Some(bind_guard_bb) = bind_guard_bb_opt {
                let guard_bb_scrut = guard_bb_scrut_opt.unwrap_or(current_scrut_val);
                self.builder.push_instr(
                    IrInstr::CondBr {
                        cond: pat_cond,
                        then_block: bind_guard_bb,
                        then_args: vec![current_scrut_val],
                        else_block: next_check_bb,
                        else_args: vec![current_scrut_val],
                    },
                    None,
                );

                self.builder.set_current_block(bind_guard_bb);
                self.scope = outer_scope.clone();
                self.cell_vars = outer_cells.clone();
                self.bind_pattern_vars(
                    guard_bb_scrut,
                    scrut_ty,
                    &arm.pattern,
                    &enum_variants_opt,
                    &enum_variant_field_types,
                    arm.span,
                )?;
                let guard_expr = arm.guard.as_ref().unwrap();
                let (guard_val, _) = self.lower_thir_expr_req(guard_expr)?;
                self.builder.push_instr(
                    IrInstr::CondBr {
                        cond: guard_val,
                        then_block: arm_body_bb,
                        then_args: vec![guard_bb_scrut],
                        else_block: next_check_bb,
                        else_args: vec![current_scrut_val],
                    },
                    None,
                );

                (self.scope.clone(), self.cell_vars.clone())
            } else {
                let final_cond = if let Some(ref guard_expr) = arm.guard {
                    let (guard_val, _) = self.lower_thir_expr_req(guard_expr)?;
                    let and_result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::BinOp {
                            result: and_result,
                            op: BinOp::BitAnd,
                            lhs: pat_cond,
                            rhs: guard_val,
                            ty: IrType::Scalar(DType::Bool),
                        },
                        Some(IrType::Scalar(DType::Bool)),
                    );
                    and_result
                } else {
                    pat_cond
                };

                self.builder.push_instr(
                    IrInstr::CondBr {
                        cond: final_cond,
                        then_block: arm_body_bb,
                        then_args: vec![current_scrut_val],
                        else_block: next_check_bb,
                        else_args: vec![current_scrut_val],
                    },
                    None,
                );

                (outer_scope.clone(), outer_cells.clone())
            };

            self.builder.set_current_block(arm_body_bb);
            self.scope = body_scope;
            self.cell_vars = body_cells;

            if arm.guard.is_none() || !pattern_has_bindings(&arm.pattern) {
                self.bind_pattern_vars(
                    arm_bb_scrut,
                    scrut_ty,
                    &arm.pattern,
                    &enum_variants_opt,
                    &enum_variant_field_types,
                    arm.span,
                )?;
            }

            let (arm_val, arm_ty) = if keep_ref {
                self.lower_thir_expr_keep_ref_req(&arm.body)?
            } else {
                self.lower_thir_expr_req(&arm.body)?
            };
            if !self.builder.is_current_block_terminated() {
                let arm_scope = self.scope.clone();
                let rebound_vals = Self::collect_pending_when_rebound_vals(
                    &rebound_names,
                    &arm_scope,
                    &outer_scope,
                );
                pending_branches.push((
                    self.builder.current_block(),
                    arm_val,
                    arm_ty,
                    rebound_vals,
                    Self::expr_is_shared_borrow_thir(&arm.body),
                ));
            }

            current_scrut_val = next_bb_scrut_opt.unwrap_or(scrut_val);
            current_check_bb = next_check_bb;
        }

        self.builder.set_current_block(no_match_bb);
        self.scope = outer_scope.clone();
        self.cell_vars = outer_cells.clone();
        let panic_msg = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstStr {
                result: panic_msg,
                value: "when: no pattern matched".to_string(),
            },
            Some(IrType::Str),
        );
        self.builder.push_instr(
            IrInstr::Panic {
                msg: panic_msg,
                span_byte: None,
            },
            None,
        );

        self.scope = outer_scope;
        self.cell_vars = outer_cells;
        let result_ty = self.emit_when_merge_branches(
            merge_bb,
            &IrType::Scalar(DType::I64),
            pending_branches,
            span,
        );
        let result = self
            .builder
            .add_block_param(merge_bb, Some("when_result"), result_ty.clone());
        let mut rebound_params = Vec::new();
        for name in &rebound_names {
            let Some((_, ty)) = self.scope.get(name) else {
                continue;
            };
            let param = self
                .builder
                .add_block_param(merge_bb, Some(name), ty.clone());
            rebound_params.push((name.clone(), param, ty.clone()));
        }
        self.builder.set_current_block(merge_bb);
        for (name, param, ty) in rebound_params {
            self.scope.insert(name, (param, ty));
        }
        Ok((result, result_ty))
    }

    /// Lowers `when scrutinee { EnumName.Variant => expr, ... }` to SSA.
    ///
    /// Emits a `SwitchVariant` terminator that dispatches to one block per arm,
    /// each of which produces a value and jumps to a merge block.
    pub(super) fn lower_when_expr_with_keep_ref(
        &mut self,
        scrutinee: &AstExpr,
        arms: &[AstWhenArm],
        span: Span,
        keep_ref: bool,
    ) -> Result<(ValueId, IrType), LowerError> {
        if arms.is_empty() {
            return Err(LowerError::rich(
                "E0127",
                crate::diagnostics::Diagnostic::error(
                    "a `when` needs at least one arm".to_string(),
                )
                .with_primary_span((span).start.0, (span).end.0, None),
            ));
        }

        // 1. Evaluate the scrutinee.
        let (scrut_val, scrut_ty) = self.lower_expr(scrutinee)?;

        // Check if this is an option or result pattern match.
        let is_option_when = arms.iter().any(|a| {
            matches!(
                a.pattern,
                AstWhenPattern::OptionSome { .. } | AstWhenPattern::OptionNone
            )
        });
        let is_result_when = arms.iter().any(|a| {
            matches!(
                a.pattern,
                AstWhenPattern::ResultOk { .. } | AstWhenPattern::ResultErr { .. }
            )
        });

        // If any option/result arm has a guard, use chain lowering so that
        // guards are evaluated correctly with bindings in scope.
        let option_has_guards = is_option_when && arms.iter().any(|a| a.guard.is_some());
        let result_has_guards = is_result_when && arms.iter().any(|a| a.guard.is_some());

        if is_option_when && !option_has_guards {
            return self
                .lower_option_when_with_keep_ref(scrut_val, &scrut_ty, arms, span, keep_ref);
        }
        if is_result_when && !result_has_guards {
            return self
                .lower_result_when_with_keep_ref(scrut_val, &scrut_ty, arms, span, keep_ref);
        }

        // Check if any arm has guards or non-enum patterns (wildcard, literal).
        // For enum matches with wildcards, use SwitchVariant with default_block.
        // Only use chain if there are guards or non-enum literal patterns.
        let is_enum_scrut = matches!(&scrut_ty, IrType::Enum { .. });
        let has_guard = arms.iter().any(|a| a.guard.is_some());

        // Use chain only if: has guard OR not enum type OR has literal patterns (non-wildcard)
        let needs_chain = has_guard || !is_enum_scrut;
        if needs_chain {
            return self
                .lower_when_as_chain_with_keep_ref(scrut_val, &scrut_ty, arms, span, keep_ref);
        }

        // 2. Verify it is an enum type and extract variants.
        let (enum_name, variants) = match &scrut_ty {
            IrType::Enum { name, variants } => (name.clone(), variants.clone()),
            _ => {
                return Err(LowerError::rich(
                    "E0124",
                    crate::diagnostics::Diagnostic::error(format!(
                        "this `when` matches variants, but the value is `{}`, not a choice",
                        hints::type_name(&scrut_ty)
                    ))
                    .with_primary_span((span).start.0, (span).end.0, None),
                ));
            }
        };

        // 3. Allocate one block per arm and a merge block.
        let mut arm_blocks: Vec<BlockId> = Vec::new();
        let mut default_block_opt: Option<BlockId> = None;
        for (arm_idx, arm) in arms.iter().enumerate() {
            match &arm.pattern {
                AstWhenPattern::Wildcard => {
                    // Wildcard becomes the default block
                    let bb = self
                        .builder
                        .create_block(Some(&format!("when_{}_wildcard", enum_name)));
                    default_block_opt = Some(bb);
                    arm_blocks.push(bb);
                }
                AstWhenPattern::EnumVariant { .. } => {
                    // Regular variant arm
                    arm_blocks.push(
                        self.builder.create_block(Some(&format!(
                            "when_{}_{}",
                            enum_name, arm.variant_name
                        ))),
                    );
                }
                AstWhenPattern::Or(_) => {
                    // Or-pattern: allocate one shared body block
                    arm_blocks.push(
                        self.builder
                            .create_block(Some(&format!("when_or_{}", arm_idx))),
                    );
                }
                AstWhenPattern::Binding { .. } => {
                    // Binding pattern: delegate to chain lowering
                    return self.lower_when_as_chain_with_keep_ref(
                        scrut_val, &scrut_ty, arms, span, keep_ref,
                    );
                }
                _ => {
                    return Err(LowerError::rich("E0103", crate::diagnostics::Diagnostic::error("this pattern cannot match a choice value: use `Choice.Variant` or `Choice.Variant(a, b)`".to_string()).with_primary_span((arm.span).start.0, (arm.span).end.0, None)));
                }
            }
        }
        let merge_bb = self.builder.create_block(Some("when_merge"));

        // 4. Build the arms list for SwitchVariant (skip wildcard patterns).
        let mut switch_arms: Vec<(usize, BlockId)> = Vec::new();
        for (arm_idx, arm) in arms.iter().enumerate() {
            match &arm.pattern {
                AstWhenPattern::EnumVariant { variant_name, .. } => {
                    let variant_idx =
                        variants
                            .iter()
                            .position(|v| v == variant_name)
                            .ok_or_else(|| {
                                LowerError::rich(
                                    "E0122",
                                    crate::diagnostics::Diagnostic::error(format!(
                                        "`{1}` has no variant `{0}`",
                                        variant_name, enum_name
                                    ))
                                    .with_primary_span(
                                        (arm.span).start.0,
                                        (arm.span).end.0,
                                        None,
                                    ),
                                )
                            })?;
                    switch_arms.push((variant_idx, arm_blocks[arm_idx]));
                }
                AstWhenPattern::Or(subs) => {
                    // Or-pattern: each sub-pattern maps to the shared body block
                    for sub in subs {
                        if let AstWhenPattern::EnumVariant { variant_name, .. } = sub {
                            let variant_idx = variants
                                .iter()
                                .position(|v| v == variant_name)
                                .ok_or_else(|| {
                                    LowerError::rich(
                                        "E0122",
                                        crate::diagnostics::Diagnostic::error(format!(
                                            "`{1}` has no variant `{0}`",
                                            variant_name, enum_name
                                        ))
                                        .with_primary_span(
                                            (arm.span).start.0,
                                            (arm.span).end.0,
                                            None,
                                        ),
                                    )
                                })?;
                            switch_arms.push((variant_idx, arm_blocks[arm_idx]));
                        } else {
                            return Err(LowerError::rich("E0103", crate::diagnostics::Diagnostic::error("this pattern is not supported inside `|` alternatives: give it its own arm".to_string()).with_primary_span((arm.span).start.0, (arm.span).end.0, None)));
                        }
                    }
                }
                AstWhenPattern::Wildcard => {
                    // Skip - handled as default_block
                }
                _ => {
                    return Err(LowerError::rich("E0103", crate::diagnostics::Diagnostic::error("this pattern cannot match a choice value: use `Choice.Variant` or `Choice.Variant(a, b)`".to_string()).with_primary_span((arm.span).start.0, (arm.span).end.0, None)));
                }
            }
        }

        // 5. Emit SwitchVariant terminator in the current block.
        self.builder.push_instr(
            IrInstr::SwitchVariant {
                scrutinee: scrut_val,
                arms: switch_arms,
                default_block: default_block_opt,
            },
            None,
        );

        // 6. Lower each arm body.
        // Get the variant field types for this enum so we can emit ExtractVariantField.
        let variant_field_types: Vec<Vec<IrType>> = self
            .module
            .enum_variant_fields(&enum_name)
            .cloned()
            .unwrap_or_default();

        let outer_scope = self.scope.clone();
        let outer_cells = self.cell_vars.clone();

        // Variables the arms assign to must travel to the merge block as block
        // arguments, exactly as the option and result paths already do. Without
        // this the assignment was simply *lost*: `when c { C.X => { a = 1; } }`
        // restored the outer scope afterwards, so `a` kept its old value and the
        // program silently computed the wrong answer. See known-issues #3.
        let mut rebound_names: Vec<String> = Vec::new();
        for arm in arms {
            collect_rebound_vars_in_expr(&arm.body, &mut rebound_names);
        }
        rebound_names
            .retain(|name| outer_scope.contains_key(name) && !outer_cells.contains_key(name));

        let mut pending_branches: Vec<(BlockId, ValueId, IrType, Vec<ValueId>, bool)> = Vec::new();
        for (arm, &arm_bb) in arms.iter().zip(arm_blocks.iter()) {
            self.scope = outer_scope.clone();
            self.cell_vars = outer_cells.clone();
            self.builder.set_current_block(arm_bb);

            // Emit ExtractVariantField instructions for pattern bindings.
            if let AstWhenPattern::EnumVariant {
                variant_name,
                bindings,
                ..
            } = &arm.pattern
            {
                if !bindings.is_empty() {
                    // Find the variant index for field type lookup.
                    let vidx = variants.iter().position(|v| v == variant_name);
                    let empty_fields: Vec<IrType> = Vec::new();
                    let field_types: &Vec<IrType> = vidx
                        .and_then(|i| variant_field_types.get(i))
                        .unwrap_or(&empty_fields);
                    let variant_idx = vidx.unwrap_or(0);
                    if bindings.len() > field_types.len() {
                        return Err(LowerError::rich(
                            "E0125",
                            crate::diagnostics::Diagnostic::error(format!(
                                "`{enum_name}.{variant_name}` has {} field{}, but the pattern binds {}",
                                field_types.len(),
                                if field_types.len() == 1 { "" } else { "s" },
                                bindings.len()
                            ))
                            .with_code("E0125")
                            .with_primary_span(arm.span.start.0, arm.span.end.0, None),
                        ));
                    }
                    for (field_idx, binding_name) in bindings.iter().enumerate() {
                        let field_ty = field_types[field_idx].clone();
                        let result = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::ExtractVariantField {
                                result,
                                operand: scrut_val,
                                variant_idx,
                                field_idx,
                                result_ty: field_ty.clone(),
                            },
                            Some(field_ty.clone()),
                        );
                        self.bind_scope_var(binding_name.clone(), result, field_ty);
                    }
                }
            }

            let (arm_val, arm_ty) = if keep_ref {
                self.lower_expr_keep_ref(&arm.body)?
            } else {
                self.lower_expr(&arm.body)?
            };
            if !self.builder.is_current_block_terminated() {
                let arm_scope = self.scope.clone();
                let rebound_vals = Self::collect_pending_when_rebound_vals(
                    &rebound_names,
                    &arm_scope,
                    &outer_scope,
                );
                pending_branches.push((
                    self.builder.current_block(),
                    arm_val,
                    arm_ty,
                    rebound_vals,
                    Self::expr_is_shared_borrow_ast(&arm.body),
                ));
            }
        }
        self.scope = outer_scope;
        self.cell_vars = outer_cells;

        let result_ty = self.emit_when_merge_branches(
            merge_bb,
            &IrType::Scalar(DType::I64),
            pending_branches,
            span,
        );

        // 7. Merge block receives the result, then the rebound variables.
        let result = self
            .builder
            .add_block_param(merge_bb, Some("when_result"), result_ty.clone());
        let mut rebound_params = Vec::new();
        for name in &rebound_names {
            let Some((_, ty)) = self.scope.get(name).cloned() else {
                continue;
            };
            let param = self
                .builder
                .add_block_param(merge_bb, Some(name), ty.clone());
            rebound_params.push((name.clone(), param, ty));
        }
        self.builder.set_current_block(merge_bb);
        for (name, param, ty) in rebound_params {
            self.scope.insert(name, (param, ty));
        }

        Ok((result, result_ty))
    }

    /// Lowers `when opt_val { some(x) => body, none => body }` for option types.
    pub(super) fn lower_option_when_with_keep_ref(
        &mut self,
        scrut_val: ValueId,
        scrut_ty: &IrType,
        arms: &[AstWhenArm],
        span: Span,
        keep_ref: bool,
    ) -> Result<(ValueId, IrType), LowerError> {
        // Extract inner type from option type.
        let inner_ty = match scrut_ty {
            IrType::Option(inner) => (**inner).clone(),
            other => {
                return Err(LowerError::TypeMismatch {
                    expected: "an option, for `some`/`none` arms".into(),
                    found: other.to_string(),
                    span,
                })
            }
        };
        // Find the some and none arms.
        let some_arm = arms
            .iter()
            .find(|a| matches!(a.pattern, AstWhenPattern::OptionSome { .. }));
        let none_arm = arms
            .iter()
            .find(|a| matches!(a.pattern, AstWhenPattern::OptionNone))
            .or_else(|| {
                arms.iter()
                    .find(|a| matches!(a.pattern, AstWhenPattern::Wildcard))
            });

        if some_arm.is_none() && none_arm.is_none() {
            return Err(LowerError::rich(
                "E0127",
                crate::diagnostics::Diagnostic::error(
                    "a `when` on an option needs `some(...)` and `none` arms".to_string(),
                )
                .with_primary_span((span).start.0, (span).end.0, None),
            ));
        }

        // Emit IsSome test.
        let is_some_result = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::IsSome {
                result: is_some_result,
                operand: scrut_val,
            },
            Some(IrType::Scalar(DType::Bool)),
        );

        let some_bb = self.builder.create_block(Some("option_some"));
        let none_bb = self.builder.create_block(Some("option_none"));
        let merge_bb = self.builder.create_block(Some("option_merge"));

        let outer_scope = self.scope.clone();
        let outer_cells = self.cell_vars.clone();
        let mut rebound_names = Vec::new();
        for arm in arms {
            collect_rebound_vars_in_expr(&arm.body, &mut rebound_names);
        }
        rebound_names
            .retain(|name| outer_scope.contains_key(name) && !outer_cells.contains_key(name));

        let unit_ty = IrType::Scalar(DType::I64);

        // When only one arm is present the whole expression evaluates to unit (i64 0).
        // When both arms are present the result type comes from the arm bodies.
        let partial = some_arm.is_none() || none_arm.is_none();

        // Pre-compute a unit value in the current (pre-branch) block BEFORE the CondBr
        // terminates the block, so it's accessible from both successor arms.
        let unit_val = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: unit_val,
                value: 0,
                ty: unit_ty.clone(),
            },
            Some(unit_ty.clone()),
        );

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

        let mut pending_branches: Vec<(BlockId, ValueId, IrType, Vec<ValueId>, bool)> = Vec::new();

        // Some branch.
        self.builder.set_current_block(some_bb);
        self.scope = outer_scope.clone();
        self.cell_vars = outer_cells.clone();
        let (some_val, some_ty, some_is_shared, result_ty): (
            ValueId,
            IrType,
            bool,
            Option<IrType>,
        ) = if let Some(arm) = some_arm {
            // Bind the inner value if a name was given.
            if let AstWhenPattern::OptionSome {
                binding: Some(ref bind_name),
            } = arm.pattern
            {
                // Unwrap the option to get the inner value.
                let unwrapped = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::OptionUnwrap {
                        result: unwrapped,
                        operand: scrut_val,
                        result_ty: inner_ty.clone(),
                    },
                    Some(inner_ty.clone()),
                );
                self.bind_scope_var(bind_name.clone(), unwrapped, inner_ty.clone());
            }
            let (v, ty) = if keep_ref {
                self.lower_expr_keep_ref(&arm.body)?
            } else {
                self.lower_expr(&arm.body)?
            };
            // The covered arm keeps its value even when the match is partial.
            // Discarding it here is what made `when o { some(x) => x }` return
            // 0 for every input (#54).
            (
                v,
                ty.clone(),
                Self::expr_is_shared_borrow_ast(&arm.body),
                Some(ty),
            )
        } else {
            (unit_val, unit_ty.clone(), false, Some(unit_ty.clone()))
        };
        if !self.builder.is_current_block_terminated() {
            let some_scope = self.scope.clone();
            let rebound_vals = Self::collect_pending_when_rebound_vals(
                &rebound_names,
                &some_scope,
                &outer_scope,
            );
            pending_branches.push((
                self.builder.current_block(),
                some_val,
                some_ty,
                rebound_vals,
                some_is_shared,
            ));
        }

        // None branch.
        self.builder.set_current_block(none_bb);
        self.scope = outer_scope.clone();
        self.cell_vars = outer_cells.clone();
        let (none_val, none_ty, none_is_shared) = if let Some(arm) = none_arm {
            let (v, ty) = if keep_ref {
                self.lower_expr_keep_ref(&arm.body)?
            } else {
                self.lower_expr(&arm.body)?
            };
            (v, ty, Self::expr_is_shared_borrow_ast(&arm.body))
        } else if partial {
            // No `none` arm. The branch still needs a value of the same type as
            // the arm that *is* present, so the merge stays well-typed.
            let def_ty = match result_ty.clone() {
                Some(IrType::MutRef(inner)) => *inner,
                Some(t) => t,
                None => unit_ty.clone(),
            };
            (self.emit_default_value(&def_ty), def_ty, false)
        } else {
            (unit_val, unit_ty.clone(), false)
        };
        if !self.builder.is_current_block_terminated() {
            let none_scope = self.scope.clone();
            let rebound_vals = Self::collect_pending_when_rebound_vals(
                &rebound_names,
                &none_scope,
                &outer_scope,
            );
            pending_branches.push((
                self.builder.current_block(),
                none_val,
                none_ty,
                rebound_vals,
                none_is_shared,
            ));
        }

        self.scope = outer_scope;
        self.cell_vars = outer_cells;
        let result_ty =
            self.emit_when_merge_branches(merge_bb, &unit_ty, pending_branches, span);
        let result =
            self.builder
                .add_block_param(merge_bb, Some("option_result"), result_ty.clone());
        let mut rebound_params = Vec::new();
        for name in &rebound_names {
            let Some((_, ty)) = self.scope.get(name) else {
                continue;
            };
            let param = self
                .builder
                .add_block_param(merge_bb, Some(name), ty.clone());
            rebound_params.push((name.clone(), param, ty.clone()));
        }
        self.builder.set_current_block(merge_bb);
        for (name, param, ty) in rebound_params {
            self.scope.insert(name, (param, ty));
        }
        Ok((result, result_ty))
    }

    /// Lowers `when res_val { ok(x) => body, err(e) => body }` for result types.
    pub(super) fn lower_result_when_with_keep_ref(
        &mut self,
        scrut_val: ValueId,
        scrut_ty: &IrType,
        arms: &[AstWhenArm],
        span: Span,
        keep_ref: bool,
    ) -> Result<(ValueId, IrType), LowerError> {
        let (ok_inner_ty, err_inner_ty) = match scrut_ty {
            IrType::ResultType(ok, err) => ((**ok).clone(), (**err).clone()),
            other => {
                return Err(LowerError::TypeMismatch {
                    expected: "a result, for `ok`/`err` arms".into(),
                    found: other.to_string(),
                    span,
                })
            }
        };
        let ok_arm = arms
            .iter()
            .find(|a| matches!(a.pattern, AstWhenPattern::ResultOk { .. }));
        let err_arm = arms
            .iter()
            .find(|a| matches!(a.pattern, AstWhenPattern::ResultErr { .. }))
            .or_else(|| {
                arms.iter()
                    .find(|a| matches!(a.pattern, AstWhenPattern::Wildcard))
            });

        if ok_arm.is_none() && err_arm.is_none() {
            return Err(LowerError::rich(
                "E0127",
                crate::diagnostics::Diagnostic::error(
                    "a `when` on a result needs `ok(...)` and `err(...)` arms".to_string(),
                )
                .with_primary_span((span).start.0, (span).end.0, None),
            ));
        }

        // Emit IsOk test.
        let is_ok_result = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::IsOk {
                result: is_ok_result,
                operand: scrut_val,
            },
            Some(IrType::Scalar(DType::Bool)),
        );

        let ok_bb = self.builder.create_block(Some("result_ok"));
        let err_bb = self.builder.create_block(Some("result_err"));
        let merge_bb = self.builder.create_block(Some("result_merge"));

        let outer_scope = self.scope.clone();
        let outer_cells = self.cell_vars.clone();
        let mut rebound_names = Vec::new();
        for arm in arms {
            collect_rebound_vars_in_expr(&arm.body, &mut rebound_names);
        }
        rebound_names
            .retain(|name| outer_scope.contains_key(name) && !outer_cells.contains_key(name));

        let unit_ty = IrType::Scalar(DType::I64);

        // When only one arm is present the whole expression evaluates to unit (i64 0).
        let partial = ok_arm.is_none() || err_arm.is_none();

        // Pre-compute unit value BEFORE the CondBr terminates the block,
        // so it's accessible from both successor arms.
        let unit_val = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: unit_val,
                value: 0,
                ty: unit_ty.clone(),
            },
            Some(unit_ty.clone()),
        );

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

        let mut pending_branches: Vec<(BlockId, ValueId, IrType, Vec<ValueId>, bool)> = Vec::new();

        // Ok branch.
        self.builder.set_current_block(ok_bb);
        self.scope = outer_scope.clone();
        self.cell_vars = outer_cells.clone();
        let (ok_val, ok_ty, ok_is_shared, result_ty): (
            ValueId,
            IrType,
            bool,
            Option<IrType>,
        ) = if let Some(arm) = ok_arm {
            if let AstWhenPattern::ResultOk {
                binding: Some(ref bind_name),
            } = arm.pattern
            {
                let unwrapped = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::ResultUnwrap {
                        result: unwrapped,
                        operand: scrut_val,
                        result_ty: ok_inner_ty.clone(),
                    },
                    Some(ok_inner_ty.clone()),
                );
                self.bind_scope_var(bind_name.clone(), unwrapped, ok_inner_ty.clone());
            }
            let (v, ty) = if keep_ref {
                self.lower_expr_keep_ref(&arm.body)?
            } else {
                self.lower_expr(&arm.body)?
            };
            // The covered arm keeps its value even when the match is partial;
            // see the same fix in `lower_option_when` (#54).
            (
                v,
                ty.clone(),
                Self::expr_is_shared_borrow_ast(&arm.body),
                Some(ty),
            )
        } else {
            (unit_val, unit_ty.clone(), false, Some(unit_ty.clone()))
        };
        if !self.builder.is_current_block_terminated() {
            let ok_scope = self.scope.clone();
            let rebound_vals = Self::collect_pending_when_rebound_vals(
                &rebound_names,
                &ok_scope,
                &outer_scope,
            );
            pending_branches.push((
                self.builder.current_block(),
                ok_val,
                ok_ty,
                rebound_vals,
                ok_is_shared,
            ));
        }

        // Err branch.
        self.builder.set_current_block(err_bb);
        self.scope = outer_scope.clone();
        self.cell_vars = outer_cells.clone();
        let (err_val, err_ty, err_is_shared) = if let Some(arm) = err_arm {
            if let AstWhenPattern::ResultErr {
                binding: Some(ref bind_name),
            } = arm.pattern
            {
                let unwrapped = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::ResultUnwrapErr {
                        result: unwrapped,
                        operand: scrut_val,
                        result_ty: err_inner_ty.clone(),
                    },
                    Some(err_inner_ty.clone()),
                );
                self.bind_scope_var(bind_name.clone(), unwrapped, err_inner_ty.clone());
            }
            let (v, ty) = if keep_ref {
                self.lower_expr_keep_ref(&arm.body)?
            } else {
                self.lower_expr(&arm.body)?
            };
            (v, ty, Self::expr_is_shared_borrow_ast(&arm.body))
        } else if partial {
            // No `err` arm: the branch still needs a value of the arm's type so
            // the merge stays well-typed.
            let def_ty = match result_ty.clone() {
                Some(IrType::MutRef(inner)) => *inner,
                Some(t) => t,
                None => unit_ty.clone(),
            };
            (self.emit_default_value(&def_ty), def_ty, false)
        } else {
            (unit_val, unit_ty.clone(), false)
        };
        if !self.builder.is_current_block_terminated() {
            let err_scope = self.scope.clone();
            let rebound_vals = Self::collect_pending_when_rebound_vals(
                &rebound_names,
                &err_scope,
                &outer_scope,
            );
            pending_branches.push((
                self.builder.current_block(),
                err_val,
                err_ty,
                rebound_vals,
                err_is_shared,
            ));
        }

        self.scope = outer_scope;
        self.cell_vars = outer_cells;
        let result_ty =
            self.emit_when_merge_branches(merge_bb, &unit_ty, pending_branches, span);
        let result =
            self.builder
                .add_block_param(merge_bb, Some("result_result"), result_ty.clone());
        let mut rebound_params = Vec::new();
        for name in &rebound_names {
            let Some((_, ty)) = self.scope.get(name) else {
                continue;
            };
            let param = self
                .builder
                .add_block_param(merge_bb, Some(name), ty.clone());
            rebound_params.push((name.clone(), param, ty.clone()));
        }
        self.builder.set_current_block(merge_bb);
        for (name, param, ty) in rebound_params {
            self.scope.insert(name, (param, ty));
        }
        Ok((result, result_ty))
    }

    /// Lowers a `when` expression as an if-else chain.
    ///
    /// Used for:
    /// - Arms with guards (`pattern if cond =>`)
    /// - Wildcard patterns (`_`)
    /// - Literal patterns (integer, bool, string)
    /// - Enum patterns when guards or wildcards are mixed in
    pub(super) fn lower_when_as_chain_with_keep_ref(
        &mut self,
        scrut_val: ValueId,
        scrut_ty: &IrType,
        arms: &[AstWhenArm],
        span: Span,
        keep_ref: bool,
    ) -> Result<(ValueId, IrType), LowerError> {
        // Create a merge block that all arms jump to with their result value.
        let merge_bb = self.builder.create_block(Some("when_merge"));

        // Pre-allocate: we'll build the chain from first arm to last.
        // We need a "no-match" fallback block (panic or unreachable) for non-exhaustive matches.
        // But we emit a runtime panic for safety.
        let no_match_bb = self.builder.create_block(Some("when_no_match"));
        let _no_match_scrut = self
            .builder
            .add_block_param(no_match_bb, None, scrut_ty.clone());

        let outer_scope = self.scope.clone();
        let outer_cells = self.cell_vars.clone();
        let mut rebound_names = Vec::new();
        for arm in arms {
            collect_rebound_vars_in_expr(&arm.body, &mut rebound_names);
        }
        rebound_names
            .retain(|name| outer_scope.contains_key(name) && !outer_cells.contains_key(name));

        let mut pending_branches: Vec<(BlockId, ValueId, IrType, Vec<ValueId>, bool)> = Vec::new();

        // Extract enum variant info if scrutinee is an enum.
        let (enum_name_opt, enum_variants_opt): (Option<String>, Option<Vec<String>>) =
            if let IrType::Enum { name, variants } = scrut_ty {
                (Some(name.clone()), Some(variants.clone()))
            } else {
                (None, None)
            };
        let enum_variant_field_types: Vec<Vec<IrType>> = if let Some(ref ename) = enum_name_opt {
            self.module
                .enum_variant_fields(ename)
                .cloned()
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        // We chain arms: for each arm, emit:
        //   current_bb: cond = (pattern_matches && guard?)
        //               condBr cond -> arm_body_bb, next_check_bb
        //   arm_body_bb: bind vars, lower body, br merge_bb
        //   next_check_bb: (next iteration's current_bb)
        let mut current_check_bb = self.builder.current_block();
        // The initial scrutinee value (from entry block or expression result)
        let mut current_scrut_val = scrut_val;

        for (arm_idx, arm) in arms.iter().enumerate() {
            let is_last = arm_idx == arms.len() - 1;
            let has_guard_with_bindings = arm.guard.is_some() && pattern_has_bindings(&arm.pattern);

            // IMPORTANT: create bind_guard_bb BEFORE arm_body_bb so the validator's
            // linear block scan sees value definitions before their uses.
            let (bind_guard_bb_opt, guard_bb_scrut_opt) = if has_guard_with_bindings {
                let bb = self
                    .builder
                    .create_block(Some(&format!("when_guard_{}", arm_idx)));
                let scrut_param = self.builder.add_block_param(bb, None, scrut_ty.clone());
                (Some(bb), Some(scrut_param))
            } else {
                (None, None)
            };

            // Create the arm body block.
            let arm_body_bb = self
                .builder
                .create_block(Some(&format!("when_arm_{}", arm_idx)));
            let arm_bb_scrut = self
                .builder
                .add_block_param(arm_body_bb, None, scrut_ty.clone());

            // Create the next-check block (reuse no_match_bb for last arm).
            let (next_check_bb, next_bb_scrut_opt) = if is_last {
                (no_match_bb, None)
            } else {
                let bb = self
                    .builder
                    .create_block(Some(&format!("when_check_{}", arm_idx + 1)));
                let scrut_param = self.builder.add_block_param(bb, None, scrut_ty.clone());
                (bb, Some(scrut_param))
            };

            // Emit the pattern match condition into current_check_bb.
            self.builder.set_current_block(current_check_bb);
            self.scope = outer_scope.clone();
            self.cell_vars = outer_cells.clone();

            // Compute the pattern condition (tag check only, no extraction).
            let pat_cond = self.emit_pattern_condition(
                current_scrut_val,
                scrut_ty,
                &arm.pattern,
                &enum_name_opt,
                &enum_variants_opt,
                span,
            )?;

            // If guard is present AND pattern has extractable bindings, use a 3-block approach:
            //   check_bb → (pat matches?) → bind_guard_bb → (guard?) → arm_body_bb
            // This ensures bindings are available to the guard expression.
            let (body_scope, body_cells) = if let Some(bind_guard_bb) = bind_guard_bb_opt {
                let guard_bb_scrut = guard_bb_scrut_opt.unwrap_or(current_scrut_val);
                // In check_bb: branch on pattern condition.
                self.builder.push_instr(
                    IrInstr::CondBr {
                        cond: pat_cond,
                        then_block: bind_guard_bb,
                        then_args: vec![current_scrut_val],
                        else_block: next_check_bb,
                        else_args: vec![current_scrut_val],
                    },
                    None,
                );

                // In bind_guard_bb: emit bindings, evaluate guard.
                self.builder.set_current_block(bind_guard_bb);
                self.scope = outer_scope.clone();
                self.cell_vars = outer_cells.clone();
                self.bind_pattern_vars(
                    guard_bb_scrut,
                    scrut_ty,
                    &arm.pattern,
                    &enum_variants_opt,
                    &enum_variant_field_types,
                    arm.span,
                )?;
                let guard_expr = arm.guard.as_ref().unwrap();
                let (guard_val, _) = self.lower_expr(guard_expr)?;
                self.builder.push_instr(
                    IrInstr::CondBr {
                        cond: guard_val,
                        then_block: arm_body_bb,
                        then_args: vec![guard_bb_scrut],
                        else_block: next_check_bb,
                        else_args: vec![current_scrut_val],
                    },
                    None,
                );

                // Body scope carries the bindings from bind_guard_bb.
                (self.scope.clone(), self.cell_vars.clone())
            } else {
                // Simple case: combine pat_cond + optional guard in single block.
                let final_cond = if let Some(ref guard_expr) = arm.guard {
                    let (guard_val, _) = self.lower_expr(guard_expr)?;
                    let and_result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::BinOp {
                            result: and_result,
                            op: BinOp::BitAnd,
                            lhs: pat_cond,
                            rhs: guard_val,
                            ty: IrType::Scalar(DType::Bool),
                        },
                        Some(IrType::Scalar(DType::Bool)),
                    );
                    and_result
                } else {
                    pat_cond
                };

                self.builder.push_instr(
                    IrInstr::CondBr {
                        cond: final_cond,
                        then_block: arm_body_bb,
                        then_args: vec![current_scrut_val],
                        else_block: next_check_bb,
                        else_args: vec![current_scrut_val],
                    },
                    None,
                );

                (outer_scope.clone(), outer_cells.clone())
            };

            // Emit arm body block.
            self.builder.set_current_block(arm_body_bb);
            self.scope = body_scope;
            self.cell_vars = body_cells;

            // Bind pattern variables (no-op if already bound by bind_guard_bb path).
            if arm.guard.is_none() || !pattern_has_bindings(&arm.pattern) {
                self.bind_pattern_vars(
                    arm_bb_scrut,
                    scrut_ty,
                    &arm.pattern,
                    &enum_variants_opt,
                    &enum_variant_field_types,
                    arm.span,
                )?;
            }

            let (arm_val, arm_ty) = if keep_ref {
                self.lower_expr_keep_ref(&arm.body)?
            } else {
                self.lower_expr(&arm.body)?
            };
            if !self.builder.is_current_block_terminated() {
                let arm_scope = self.scope.clone();
                let rebound_vals = Self::collect_pending_when_rebound_vals(
                    &rebound_names,
                    &arm_scope,
                    &outer_scope,
                );
                pending_branches.push((
                    self.builder.current_block(),
                    arm_val,
                    arm_ty,
                    rebound_vals,
                    Self::expr_is_shared_borrow_ast(&arm.body),
                ));
            }

            // Update current scrutinee value for next iteration (from next_check_bb's block parameter)
            current_scrut_val = next_bb_scrut_opt.unwrap_or(scrut_val);
            current_check_bb = next_check_bb;
        }

        // Emit the no-match block (runtime panic).
        self.builder.set_current_block(no_match_bb);
        self.scope = outer_scope.clone();
        self.cell_vars = outer_cells.clone();
        let panic_msg = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstStr {
                result: panic_msg,
                value: "when: no pattern matched".to_string(),
            },
            Some(IrType::Str),
        );
        self.builder.push_instr(
            IrInstr::Panic {
                msg: panic_msg,
                span_byte: None,
            },
            None,
        );
        // Panic is now a terminator; we do not need a dummy branch to merge_bb.

        self.scope = outer_scope;
        self.cell_vars = outer_cells;
        let result_ty = self.emit_when_merge_branches(
            merge_bb,
            &IrType::Scalar(DType::I64),
            pending_branches,
            span,
        );
        let result = self
            .builder
            .add_block_param(merge_bb, Some("when_result"), result_ty.clone());
        let mut rebound_params = Vec::new();
        for name in &rebound_names {
            let Some((_, ty)) = self.scope.get(name) else {
                continue;
            };
            let param = self
                .builder
                .add_block_param(merge_bb, Some(name), ty.clone());
            rebound_params.push((name.clone(), param, ty.clone()));
        }
        self.builder.set_current_block(merge_bb);
        for (name, param, ty) in rebound_params {
            self.scope.insert(name, (param, ty));
        }
        Ok((result, result_ty))
    }

    /// Emits instructions computing a bool condition for whether `scrut_val` matches `pattern`.
    pub(super) fn emit_pattern_condition(
        &mut self,
        scrut_val: ValueId,
        scrut_ty: &IrType,
        pattern: &AstWhenPattern,
        enum_name_opt: &Option<String>,
        enum_variants_opt: &Option<Vec<String>>,
        span: Span,
    ) -> Result<ValueId, LowerError> {
        match pattern {
            AstWhenPattern::Binding { pattern: inner, .. } => self.emit_pattern_condition(
                scrut_val,
                scrut_ty,
                inner,
                enum_name_opt,
                enum_variants_opt,
                span,
            ),
            AstWhenPattern::Struct {
                struct_name,
                fields,
                ..
            } => {
                // Struct pattern: look up the struct def, extract field indices, check sub-patterns.
                let bool_ty = IrType::Scalar(DType::Bool);
                let def = self.module.struct_def(struct_name).ok_or_else(|| {
                    LowerError::rich(
                        "E0111",
                        crate::diagnostics::Diagnostic::error(format!(
                            "unknown record `{}` in this pattern",
                            struct_name
                        ))
                        .with_primary_span(
                            (span).start.0,
                            (span).end.0,
                            None,
                        ),
                    )
                })?;
                let field_map: HashMap<String, (usize, IrType)> = def
                    .iter()
                    .enumerate()
                    .map(|(i, (n, t))| (n.clone(), (i, t.clone())))
                    .collect();
                let mut all_ok = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::ConstBool {
                        result: all_ok,
                        value: true,
                    },
                    Some(bool_ty.clone()),
                );
                for (field_name, sub_pat) in fields {
                    let (field_idx, field_ty) = field_map.get(field_name).ok_or_else(|| {
                        LowerError::rich(
                            "E0113",
                            crate::diagnostics::Diagnostic::error(format!(
                                "`{1}` has no field `{0}`",
                                field_name, struct_name
                            ))
                            .with_primary_span(
                                (span).start.0,
                                (span).end.0,
                                None,
                            ),
                        )
                    })?;
                    let field_val = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::GetField {
                            result: field_val,
                            base: scrut_val,
                            field_index: *field_idx,
                            result_ty: field_ty.clone(),
                        },
                        Some(field_ty.clone()),
                    );
                    let sub_cond = match sub_pat {
                        AstWhenPattern::EnumVariant { enum_name, .. } if enum_name.is_empty() => {
                            let t = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::ConstBool {
                                    result: t,
                                    value: true,
                                },
                                Some(bool_ty.clone()),
                            );
                            t
                        }
                        AstWhenPattern::Wildcard => {
                            let t = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::ConstBool {
                                    result: t,
                                    value: true,
                                },
                                Some(bool_ty.clone()),
                            );
                            t
                        }
                        other => self.emit_pattern_condition(
                            field_val, field_ty, other, &None, &None, span,
                        )?,
                    };
                    let new_ok = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::BinOp {
                            result: new_ok,
                            op: BinOp::BitAnd,
                            lhs: all_ok,
                            rhs: sub_cond,
                            ty: bool_ty.clone(),
                        },
                        Some(bool_ty.clone()),
                    );
                    all_ok = new_ok;
                }
                Ok(all_ok)
            }
            AstWhenPattern::Wildcard => {
                // Always matches: emit `true`.
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::ConstBool {
                        result,
                        value: true,
                    },
                    Some(IrType::Scalar(DType::Bool)),
                );
                Ok(result)
            }
            AstWhenPattern::IntLit(n) => {
                // scrutinee == n
                let lit_val = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::ConstInt {
                        result: lit_val,
                        value: *n,
                        ty: scrut_ty.clone(),
                    },
                    Some(scrut_ty.clone()),
                );
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::BinOp {
                        result,
                        op: BinOp::CmpEq,
                        lhs: scrut_val,
                        rhs: lit_val,
                        ty: scrut_ty.clone(),
                    },
                    Some(IrType::Scalar(DType::Bool)),
                );
                Ok(result)
            }
            AstWhenPattern::FloatLit(f) => {
                // scrutinee == f (float comparison)
                let f64_ty = IrType::Scalar(DType::F64);
                let lit_val = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::ConstFloat {
                        result: lit_val,
                        value: *f,
                        ty: f64_ty.clone(),
                    },
                    Some(f64_ty.clone()),
                );
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::BinOp {
                        result,
                        op: BinOp::CmpEq,
                        lhs: scrut_val,
                        rhs: lit_val,
                        ty: f64_ty.clone(),
                    },
                    Some(IrType::Scalar(DType::Bool)),
                );
                Ok(result)
            }
            AstWhenPattern::BoolLit(b) => {
                // scrutinee == b
                let lit_val = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::ConstBool {
                        result: lit_val,
                        value: *b,
                    },
                    Some(IrType::Scalar(DType::Bool)),
                );
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::BinOp {
                        result,
                        op: BinOp::CmpEq,
                        lhs: scrut_val,
                        rhs: lit_val,
                        ty: scrut_ty.clone(),
                    },
                    Some(IrType::Scalar(DType::Bool)),
                );
                Ok(result)
            }
            AstWhenPattern::StringLit(s) => {
                // StrEq(scrutinee, s)
                let str_val = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::ConstStr {
                        result: str_val,
                        value: s.clone(),
                    },
                    Some(IrType::Str),
                );
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::StrEq {
                        result,
                        lhs: scrut_val,
                        rhs: str_val,
                    },
                    Some(IrType::Scalar(DType::Bool)),
                );
                Ok(result)
            }
            AstWhenPattern::Tuple(subs) => {
                // For a tuple pattern (a, b, ...): extract each element and check each sub-pattern.
                // Pure bindings (EnumVariant with empty enum_name) always succeed.
                // Literal sub-patterns (IntLit, BoolLit, StringLit) emit a check.
                // All checks are AND-ed together.
                let bool_ty = IrType::Scalar(DType::Bool);
                let tuple_elems = match scrut_ty {
                    IrType::Tuple(ref elems) => elems.clone(),
                    _ => {
                        return Err(LowerError::rich(
                            "E0124",
                            crate::diagnostics::Diagnostic::error(format!(
                                "a tuple pattern cannot match `{}`",
                                hints::type_name(scrut_ty)
                            ))
                            .with_primary_span(
                                (span).start.0,
                                (span).end.0,
                                None,
                            ),
                        ))
                    }
                };
                let mut all_ok = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::ConstBool {
                        result: all_ok,
                        value: true,
                    },
                    Some(bool_ty.clone()),
                );
                for (i, sub) in subs.iter().enumerate() {
                    let elem_ty = tuple_elems
                        .get(i)
                        .cloned()
                        .unwrap_or(IrType::Scalar(DType::I64));
                    let elem_val = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::GetElement {
                            result: elem_val,
                            base: scrut_val,
                            index: i,
                            result_ty: elem_ty.clone(),
                        },
                        Some(elem_ty.clone()),
                    );
                    // Check sub-pattern
                    let sub_ok = match sub {
                        // Binding or wildcard: always true
                        AstWhenPattern::EnumVariant {
                            enum_name,
                            variant_name,
                            ..
                        } if enum_name.is_empty() => {
                            // Bind this element under variant_name (handled separately in bindings)
                            let _ = variant_name;
                            let t = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::ConstBool {
                                    result: t,
                                    value: true,
                                },
                                Some(bool_ty.clone()),
                            );
                            t
                        }
                        AstWhenPattern::Wildcard => {
                            let t = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::ConstBool {
                                    result: t,
                                    value: true,
                                },
                                Some(bool_ty.clone()),
                            );
                            t
                        }
                        other => self.emit_pattern_condition(
                            elem_val, &elem_ty, other, &None, &None, span,
                        )?,
                    };
                    let new_all = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::BinOp {
                            result: new_all,
                            op: BinOp::BitAnd,
                            lhs: all_ok,
                            rhs: sub_ok,
                            ty: bool_ty.clone(),
                        },
                        Some(bool_ty.clone()),
                    );
                    all_ok = new_all;
                }
                Ok(all_ok)
            }
            AstWhenPattern::OptionNone => {
                // !IsSome(scrutinee)
                let is_some_result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::IsSome {
                        result: is_some_result,
                        operand: scrut_val,
                    },
                    Some(IrType::Scalar(DType::Bool)),
                );
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::UnaryOp {
                        result,
                        op: ScalarUnaryOp::Not,
                        operand: is_some_result,
                        ty: IrType::Scalar(DType::Bool),
                    },
                    Some(IrType::Scalar(DType::Bool)),
                );
                Ok(result)
            }
            AstWhenPattern::OptionSome { .. } => {
                // IsSome(scrutinee)
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::IsSome {
                        result,
                        operand: scrut_val,
                    },
                    Some(IrType::Scalar(DType::Bool)),
                );
                Ok(result)
            }
            AstWhenPattern::ResultOk { .. } => {
                // IsOk(scrutinee)
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::IsOk {
                        result,
                        operand: scrut_val,
                    },
                    Some(IrType::Scalar(DType::Bool)),
                );
                Ok(result)
            }
            AstWhenPattern::ResultErr { .. } => {
                // !IsOk(scrutinee)
                let is_ok_result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::IsOk {
                        result: is_ok_result,
                        operand: scrut_val,
                    },
                    Some(IrType::Scalar(DType::Bool)),
                );
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::UnaryOp {
                        result,
                        op: ScalarUnaryOp::Not,
                        operand: is_ok_result,
                        ty: IrType::Scalar(DType::Bool),
                    },
                    Some(IrType::Scalar(DType::Bool)),
                );
                Ok(result)
            }
            AstWhenPattern::EnumVariant {
                variant_name,
                enum_name,
                ..
            } => {
                // If enum_name is empty, this is a plain identifier binding (always matches).
                if enum_name.is_empty() {
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::ConstBool {
                            result,
                            value: true,
                        },
                        Some(IrType::Scalar(DType::Bool)),
                    );
                    return Ok(result);
                }
                // GetVariantTag(scrutinee) == variant_idx_const
                let variants = enum_variants_opt.as_ref().ok_or_else(|| {
                    LowerError::rich(
                        "E0124",
                        crate::diagnostics::Diagnostic::error(
                            "a variant pattern can only match a choice value".to_string(),
                        )
                        .with_primary_span(
                            (span).start.0,
                            (span).end.0,
                            None,
                        ),
                    )
                })?;
                let variant_idx =
                    variants
                        .iter()
                        .position(|v| v == variant_name)
                        .ok_or_else(|| {
                            LowerError::rich(
                                "E0122",
                                crate::diagnostics::Diagnostic::error(format!(
                                    "`{1}` has no variant `{0}`",
                                    variant_name,
                                    enum_name_opt.as_deref().unwrap_or("?")
                                ))
                                .with_primary_span(
                                    (span).start.0,
                                    (span).end.0,
                                    None,
                                ),
                            )
                        })?;
                let tag_val = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::GetVariantTag {
                        result: tag_val,
                        operand: scrut_val,
                    },
                    Some(IrType::Scalar(DType::I64)),
                );
                let idx_val = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::ConstInt {
                        result: idx_val,
                        value: variant_idx as i64,
                        ty: IrType::Scalar(DType::I64),
                    },
                    Some(IrType::Scalar(DType::I64)),
                );
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::BinOp {
                        result,
                        op: BinOp::CmpEq,
                        lhs: tag_val,
                        rhs: idx_val,
                        ty: IrType::Scalar(DType::I64),
                    },
                    Some(IrType::Scalar(DType::Bool)),
                );
                Ok(result)
            }
            AstWhenPattern::Range { lo, hi } => {
                // lo <= scrutinee && scrutinee <= hi
                let lo_val = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::ConstInt {
                        result: lo_val,
                        value: *lo,
                        ty: scrut_ty.clone(),
                    },
                    Some(scrut_ty.clone()),
                );
                let hi_val = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::ConstInt {
                        result: hi_val,
                        value: *hi,
                        ty: scrut_ty.clone(),
                    },
                    Some(scrut_ty.clone()),
                );
                let lo_ok = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::BinOp {
                        result: lo_ok,
                        op: BinOp::CmpLe,
                        lhs: lo_val,
                        rhs: scrut_val,
                        ty: scrut_ty.clone(),
                    },
                    Some(IrType::Scalar(DType::Bool)),
                );
                let hi_ok = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::BinOp {
                        result: hi_ok,
                        op: BinOp::CmpLe,
                        lhs: scrut_val,
                        rhs: hi_val,
                        ty: scrut_ty.clone(),
                    },
                    Some(IrType::Scalar(DType::Bool)),
                );
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::BinOp {
                        result,
                        op: BinOp::BitAnd,
                        lhs: lo_ok,
                        rhs: hi_ok,
                        ty: IrType::Scalar(DType::Bool),
                    },
                    Some(IrType::Scalar(DType::Bool)),
                );
                Ok(result)
            }
            AstWhenPattern::Or(sub_pats) => {
                let mut cond_val = None;
                for p in sub_pats {
                    let sub_cond = self.emit_pattern_condition(
                        scrut_val,
                        scrut_ty,
                        p,
                        enum_name_opt,
                        enum_variants_opt,
                        span,
                    )?;
                    if let Some(prev) = cond_val {
                        let combined = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::BinOp {
                                result: combined,
                                op: BinOp::BitOr,
                                lhs: prev,
                                rhs: sub_cond,
                                ty: IrType::Scalar(DType::Bool),
                            },
                            Some(IrType::Scalar(DType::Bool)),
                        );
                        cond_val = Some(combined);
                    } else {
                        cond_val = Some(sub_cond);
                    }
                }
                Ok(cond_val.unwrap_or_else(|| {
                    let res = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::ConstBool {
                            result: res,
                            value: true,
                        },
                        Some(IrType::Scalar(DType::Bool)),
                    );
                    res
                }))
            }
            AstWhenPattern::Slice { prefix, rest } => {
                let bool_ty = IrType::Scalar(DType::Bool);
                let i64_ty = IrType::Scalar(DType::I64);
                let prefix_len = prefix.len() as i64;

                // Get list length
                let len_val = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::ListLen {
                        result: len_val,
                        list: scrut_val,
                    },
                    Some(i64_ty.clone()),
                );

                // Check length: if no rest, len == prefix.len(); if rest, len >= prefix.len()
                let prefix_len_val = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::ConstInt {
                        result: prefix_len_val,
                        value: prefix_len,
                        ty: i64_ty.clone(),
                    },
                    Some(i64_ty.clone()),
                );
                let len_check = if rest.is_some() {
                    let cmp = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::BinOp {
                            result: cmp,
                            op: BinOp::CmpGe,
                            lhs: len_val,
                            rhs: prefix_len_val,
                            ty: i64_ty.clone(),
                        },
                        Some(bool_ty.clone()),
                    );
                    cmp
                } else {
                    let cmp = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::BinOp {
                            result: cmp,
                            op: BinOp::CmpEq,
                            lhs: len_val,
                            rhs: prefix_len_val,
                            ty: i64_ty.clone(),
                        },
                        Some(bool_ty.clone()),
                    );
                    cmp
                };

                // If no prefix elements, length check is the only condition
                if prefix.is_empty() {
                    return Ok(len_check);
                }

                // Restructure with CondBr: check length first, only then check elements.
                // This avoids ListGet out-of-bounds panics when length is insufficient.
                let check_elems_bb = self.builder.create_block(Some("check_elems"));
                let no_match_bb = self.builder.create_block(Some("no_match"));
                let merge_bb = self.builder.create_block(Some("merge"));

                self.builder.push_instr(
                    IrInstr::CondBr {
                        cond: len_check,
                        then_block: check_elems_bb,
                        then_args: vec![],
                        else_block: no_match_bb,
                        else_args: vec![],
                    },
                    None,
                );

                // no_match: length was insufficient — false → merge
                self.builder.set_current_block(no_match_bb);
                let false_val = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::ConstBool {
                        result: false_val,
                        value: false,
                    },
                    Some(bool_ty.clone()),
                );
                self.builder.push_instr(
                    IrInstr::Br {
                        target: merge_bb,
                        args: vec![false_val],
                    },
                    None,
                );

                // check_elems: length is sufficient — check each prefix element via ListGet
                self.builder.set_current_block(check_elems_bb);
                let elem_ty = match scrut_ty {
                    IrType::List(inner) => (**inner).clone(),
                    _ => return Err(self.undetermined(span, "the list this slice pattern matches")),
                };

                let mut all_ok = len_check;
                for (i, sub) in prefix.iter().enumerate() {
                    let idx_val = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::ConstInt {
                            result: idx_val,
                            value: i as i64,
                            ty: i64_ty.clone(),
                        },
                        Some(i64_ty.clone()),
                    );
                    let elem_val = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::ListGet {
                            result: elem_val,
                            list: scrut_val,
                            index: idx_val,
                            elem_ty: elem_ty.clone(),
                        },
                        Some(elem_ty.clone()),
                    );
                    let sub_ok =
                        self.emit_pattern_condition(elem_val, &elem_ty, sub, &None, &None, span)?;
                    let new_all = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::BinOp {
                            result: new_all,
                            op: BinOp::BitAnd,
                            lhs: all_ok,
                            rhs: sub_ok,
                            ty: bool_ty.clone(),
                        },
                        Some(bool_ty.clone()),
                    );
                    all_ok = new_all;
                }
                self.builder.push_instr(
                    IrInstr::Br {
                        target: merge_bb,
                        args: vec![all_ok],
                    },
                    None,
                );

                // merge: result = phi from no_match(false) or check_elems(all_ok)
                let result =
                    self.builder
                        .add_block_param(merge_bb, Some("slice_match"), bool_ty.clone());
                self.builder.set_current_block(merge_bb);
                Ok(result)
            }
        }
    }

    /// Binds pattern variable names into the current scope.
    pub(super) fn bind_pattern_vars(
        &mut self,
        scrut_val: ValueId,
        scrut_ty: &IrType,
        pattern: &AstWhenPattern,
        enum_variants_opt: &Option<Vec<String>>,
        enum_variant_field_types: &[Vec<IrType>],
        span: Span,
    ) -> Result<(), LowerError> {
        match pattern {
            AstWhenPattern::Binding {
                name,
                pattern: inner,
                ..
            } => {
                self.bind_scope_var(name.clone(), scrut_val, scrut_ty.clone());
                self.bind_pattern_vars(
                    scrut_val,
                    scrut_ty,
                    inner,
                    enum_variants_opt,
                    enum_variant_field_types,
                    span,
                )?;
            }
            AstWhenPattern::OptionSome {
                binding: Some(bind_name),
            } => {
                let inner_ty = match scrut_ty {
                    IrType::Option(inner) => (**inner).clone(),
                    _ => return Err(self.undetermined(span, "what this `some` holds")),
                };
                let unwrapped = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::OptionUnwrap {
                        result: unwrapped,
                        operand: scrut_val,
                        result_ty: inner_ty.clone(),
                    },
                    Some(inner_ty.clone()),
                );
                self.bind_scope_var(bind_name.clone(), unwrapped, inner_ty);
            }
            AstWhenPattern::ResultOk {
                binding: Some(bind_name),
            } => {
                let ok_ty = match scrut_ty {
                    IrType::ResultType(ok, _) => (**ok).clone(),
                    _ => return Err(self.undetermined(span, "the success type of this `ok`")),
                };
                let unwrapped = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::ResultUnwrap {
                        result: unwrapped,
                        operand: scrut_val,
                        result_ty: ok_ty.clone(),
                    },
                    Some(ok_ty.clone()),
                );
                self.bind_scope_var(bind_name.clone(), unwrapped, ok_ty);
            }
            AstWhenPattern::ResultErr {
                binding: Some(bind_name),
            } => {
                let err_ty = match scrut_ty {
                    IrType::ResultType(_, err) => (**err).clone(),
                    _ => return Err(self.undetermined(span, "the error type of this `err`")),
                };
                let unwrapped = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::ResultUnwrapErr {
                        result: unwrapped,
                        operand: scrut_val,
                        result_ty: err_ty.clone(),
                    },
                    Some(err_ty.clone()),
                );
                self.bind_scope_var(bind_name.clone(), unwrapped, err_ty);
            }
            AstWhenPattern::EnumVariant {
                variant_name,
                bindings,
                ..
            } => {
                if !bindings.is_empty() {
                    if let Some(variants) = enum_variants_opt {
                        let vidx = variants.iter().position(|v| v == variant_name);
                        let empty_fields: Vec<IrType> = Vec::new();
                        let field_types: &Vec<IrType> = vidx
                            .and_then(|i| enum_variant_field_types.get(i))
                            .unwrap_or(&empty_fields);
                        let variant_idx = vidx.unwrap_or(0);
                        for (field_idx, binding_name) in bindings.iter().enumerate() {
                            let field_ty = match field_types.get(field_idx) {
                                Some(ty) => ty.clone(),
                                None => {
                                    return Err(self.undetermined(
                                        span,
                                        "the type of the field this pattern binds",
                                    ))
                                }
                            };
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::ExtractVariantField {
                                    result,
                                    operand: scrut_val,
                                    variant_idx,
                                    field_idx,
                                    result_ty: field_ty.clone(),
                                },
                                Some(field_ty.clone()),
                            );
                            self.bind_scope_var(binding_name.clone(), result, field_ty);
                        }
                    }
                }
            }
            AstWhenPattern::Struct {
                struct_name,
                fields,
                ..
            } => {
                let def = self
                    .module
                    .struct_def(struct_name)
                    .cloned()
                    .unwrap_or_default();
                let field_map: HashMap<String, (usize, IrType)> = def
                    .into_iter()
                    .enumerate()
                    .map(|(i, (n, t))| (n, (i, t)))
                    .collect();
                for (field_name, sub_pat) in fields {
                    if let Some((field_idx, field_ty)) = field_map.get(field_name) {
                        let field_val = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::GetField {
                                result: field_val,
                                base: scrut_val,
                                field_index: *field_idx,
                                result_ty: field_ty.clone(),
                            },
                            Some(field_ty.clone()),
                        );
                        match sub_pat {
                            AstWhenPattern::EnumVariant {
                                enum_name,
                                variant_name,
                                ..
                            } if enum_name.is_empty() => {
                                self.bind_scope_var(
                                    variant_name.clone(),
                                    field_val,
                                    field_ty.clone(),
                                );
                            }
                            AstWhenPattern::Binding { name, .. } => {
                                self.bind_scope_var(name.clone(), field_val, field_ty.clone());
                            }
                            _ => {
                                self.bind_pattern_vars(
                                    field_val,
                                    field_ty,
                                    sub_pat,
                                    enum_variants_opt,
                                    enum_variant_field_types,
                                    span,
                                )?;
                            }
                        }
                    }
                }
            }
            // Tuple pattern: bind each element to its name (sub-patterns that are ident bindings).
            AstWhenPattern::Tuple(subs) => {
                let tuple_elems = if let IrType::Tuple(ref elems) = scrut_ty {
                    elems.clone()
                } else {
                    vec![]
                };
                for (i, sub) in subs.iter().enumerate() {
                    if let AstWhenPattern::EnumVariant {
                        enum_name,
                        variant_name,
                        ..
                    } = sub
                    {
                        if enum_name.is_empty() {
                            // This is an ident binding
                            let elem_ty = tuple_elems
                                .get(i)
                                .cloned()
                                .unwrap_or(IrType::Scalar(DType::I64));
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::GetElement {
                                    result,
                                    base: scrut_val,
                                    index: i,
                                    result_ty: elem_ty.clone(),
                                },
                                Some(elem_ty.clone()),
                            );
                            self.bind_scope_var(variant_name.clone(), result, elem_ty);
                        }
                    }
                }
            }
            // Slice pattern: bind prefix elements and optional rest sub-list.
            AstWhenPattern::Slice { prefix, rest } => {
                let i64_ty = IrType::Scalar(DType::I64);
                let elem_ty = match scrut_ty {
                    IrType::List(inner) => (**inner).clone(),
                    _ => return Err(self.undetermined(span, "the list this slice pattern matches")),
                };
                // Bind each prefix element
                for (i, sub) in prefix.iter().enumerate() {
                    if let AstWhenPattern::EnumVariant {
                        enum_name,
                        variant_name,
                        ..
                    } = sub
                    {
                        if enum_name.is_empty() {
                            let idx_val = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::ConstInt {
                                    result: idx_val,
                                    value: i as i64,
                                    ty: i64_ty.clone(),
                                },
                                Some(i64_ty.clone()),
                            );
                            let elem_val = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::ListGet {
                                    result: elem_val,
                                    list: scrut_val,
                                    index: idx_val,
                                    elem_ty: elem_ty.clone(),
                                },
                                Some(elem_ty.clone()),
                            );
                            self.bind_scope_var(
                                variant_name.clone(),
                                elem_val,
                                elem_ty.clone(),
                            );
                        }
                    }
                }
                // Bind rest variable as sub-list via ListSlice(prefix.len, len)
                if let Some(rest_name) = rest {
                    let prefix_len_val = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::ConstInt {
                            result: prefix_len_val,
                            value: prefix.len() as i64,
                            ty: i64_ty.clone(),
                        },
                        Some(i64_ty.clone()),
                    );
                    let len_val = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::ListLen {
                            result: len_val,
                            list: scrut_val,
                        },
                        Some(i64_ty.clone()),
                    );
                    let rest_val = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::ListSlice {
                            result: rest_val,
                            list: scrut_val,
                            start: prefix_len_val,
                            end: len_val,
                        },
                        Some(scrut_ty.clone()),
                    );
                    self.bind_scope_var(rest_name.clone(), rest_val, scrut_ty.clone());
                }
            }
            // No bindings for wildcard, literals, none.
            _ => {}
        }
        Ok(())
    }
}
