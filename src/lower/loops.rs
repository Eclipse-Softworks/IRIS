//! Control-flow loop lowering (`while`, `for` ranges, `par for`, `foreach`, `loop`, `break`, `continue`).

use crate::error::LowerError;
use crate::ir::instr::{BinOp, IrInstr};
use crate::ir::module::IrFunctionBuilder;
use crate::ir::types::{DType, IrType};
use crate::ir::value::ValueId;
use crate::parser::ast::{AstBlock, AstExpr};
use crate::parser::lexer::Span;

use super::{
    find_captured_mutation_thir, find_rebound_vars, find_rebound_vars_thir_block, hints, Lowerer,
};

impl<'m> Lowerer<'m> {
    pub(super) fn lower_thir_while(
        &mut self,
        cond: &crate::thir::ThirExpr,
        body: &crate::thir::ThirBlock,
        label: &Option<String>,
        span: Span,
    ) -> Result<(), LowerError> {
        let outer_names = self.scope_names();
        let mut rebound = find_rebound_vars_thir_block(body, true);
        rebound.sort();
        rebound.dedup();

        let mut loop_vars: Vec<(String, ValueId, IrType)> = Vec::new();
        for name in &rebound {
            if let Some((val, ty)) = self.scope.get(name).cloned() {
                loop_vars.push((name.clone(), val, ty));
            }
        }

        let mut initial_vals: Vec<ValueId> = loop_vars.iter().map(|(_, v, _)| *v).collect();
        let taped_loop_vars: Vec<(usize, ValueId)> = loop_vars
            .iter()
            .enumerate()
            .filter_map(|(i, (_, v, _))| self.tape_nodes.get(v).map(|h| (i, *h)))
            .collect();
        for (_, handle) in &taped_loop_vars {
            initial_vals.push(*handle);
        }

        let header_bb = self.builder.create_block(Some("while_header"));
        let body_bb = self.builder.create_block(Some("while_body"));
        let merge_bb = self.builder.create_block(Some("while_merge"));

        let mut header_params: Vec<ValueId> = Vec::new();
        for (name, _, ty) in &loop_vars {
            let p = self
                .builder
                .add_block_param(header_bb, Some(name), ty.clone());
            header_params.push(p);
        }
        let header_handle_params: Vec<ValueId> = taped_loop_vars
            .iter()
            .map(|(i, _)| {
                let name = format!("{}$tape", loop_vars[*i].0);
                self.builder
                    .add_block_param(header_bb, Some(&name), IrType::TapeRef)
            })
            .collect();

        let mut merge_params: Vec<ValueId> = Vec::new();
        for (name, _, ty) in &loop_vars {
            let p = self
                .builder
                .add_block_param(merge_bb, Some(name), ty.clone());
            merge_params.push(p);
        }
        let merge_handle_params: Vec<ValueId> = taped_loop_vars
            .iter()
            .map(|(i, _)| {
                let name = format!("{}$tape", loop_vars[*i].0);
                self.builder
                    .add_block_param(merge_bb, Some(&name), IrType::TapeRef)
            })
            .collect();

        self.builder.push_instr(
            IrInstr::Br {
                target: header_bb,
                args: initial_vals,
            },
            None,
        );

        self.builder.set_current_block(header_bb);
        for ((name, _, ty), &param_val) in loop_vars.iter().zip(header_params.iter()) {
            self.scope.insert(name.clone(), (param_val, ty.clone()));
        }
        for ((i, _), &hp) in taped_loop_vars.iter().zip(header_handle_params.iter()) {
            self.taped_values.insert(header_params[*i]);
            self.tape_nodes.insert(header_params[*i], hp);
        }

        let (cond_val, _) = self.lower_thir_expr_req(cond)?;

        self.builder.push_instr(
            IrInstr::CondBr {
                cond: cond_val,
                then_block: body_bb,
                then_args: vec![],
                else_block: merge_bb,
                else_args: header_params
                    .iter()
                    .copied()
                    .chain(header_handle_params.iter().copied())
                    .collect(),
            },
            None,
        );

        self.builder.set_current_block(body_bb);
        let loop_var_names: Vec<String> = loop_vars.iter().map(|(n, _, _)| n.clone()).collect();
        self.loop_stack.push((
            header_bb,
            merge_bb,
            header_bb,
            loop_var_names,
            label.clone(),
            false,
        ));
        if body.tail.is_some() {
            let _ = self.lower_thir_block(body)?;
        } else {
            self.lower_thir_block_stmts(body)?;
        }
        self.loop_stack.pop();

        if !self.builder.is_current_block_terminated() {
            let mut updated_vals: Vec<ValueId> = loop_vars
                .iter()
                .map(|(name, original_val, _)| {
                    self.scope
                        .get(name)
                        .map(|(v, _)| *v)
                        .unwrap_or(*original_val)
                })
                .collect();
            for (i, incoming) in &taped_loop_vars {
                let current = updated_vals[*i];
                let handle = self.tape_nodes.get(&current).copied().unwrap_or(*incoming);
                updated_vals.push(handle);
            }
            self.builder.push_instr(
                IrInstr::Br {
                    target: header_bb,
                    args: updated_vals,
                },
                None,
            );
        }

        self.builder.set_current_block(merge_bb);
        for ((name, _, ty), &merge_val) in loop_vars.iter().zip(merge_params.iter()) {
            self.scope.insert(name.clone(), (merge_val, ty.clone()));
        }
        for ((i, _), &hp) in taped_loop_vars.iter().zip(merge_handle_params.iter()) {
            self.taped_values.insert(merge_params[*i]);
            self.tape_nodes.insert(merge_params[*i], hp);
        }

        let _ = span;
        self.drop_body_locals(&outer_names);
        Ok(())
    }

    pub(super) fn lower_thir_loop(
        &mut self,
        body: &crate::thir::ThirBlock,
        label: &Option<String>,
        span: Span,
    ) -> Result<(), LowerError> {
        let outer_names = self.scope_names();
        let mut rebound = find_rebound_vars_thir_block(body, true);
        rebound.sort();
        rebound.dedup();

        let mut loop_vars: Vec<(String, ValueId, IrType)> = Vec::new();
        for name in &rebound {
            if let Some((val, ty)) = self.scope.get(name).cloned() {
                loop_vars.push((name.clone(), val, ty));
            }
        }

        let initial_vals: Vec<ValueId> = loop_vars.iter().map(|(_, v, _)| *v).collect();

        let header_bb = self.builder.create_block(Some("loop_header"));
        let body_bb = self.builder.create_block(Some("loop_body"));
        let merge_bb = self.builder.create_block(Some("loop_merge"));

        let mut header_params: Vec<ValueId> = Vec::new();
        for (name, _, ty) in &loop_vars {
            let p = self
                .builder
                .add_block_param(header_bb, Some(name), ty.clone());
            header_params.push(p);
        }

        let mut merge_params: Vec<ValueId> = Vec::new();
        for (name, _, ty) in &loop_vars {
            let p = self
                .builder
                .add_block_param(merge_bb, Some(name), ty.clone());
            merge_params.push(p);
        }

        self.builder.push_instr(
            IrInstr::Br {
                target: header_bb,
                args: initial_vals,
            },
            None,
        );

        self.builder.set_current_block(header_bb);
        for ((name, _, ty), &param_val) in loop_vars.iter().zip(header_params.iter()) {
            self.scope.insert(name.clone(), (param_val, ty.clone()));
        }
        self.builder.push_instr(
            IrInstr::Br {
                target: body_bb,
                args: vec![],
            },
            None,
        );

        self.builder.set_current_block(body_bb);
        let loop_var_names: Vec<String> = loop_vars.iter().map(|(n, _, _)| n.clone()).collect();
        self.loop_stack.push((
            header_bb,
            merge_bb,
            header_bb,
            loop_var_names,
            label.clone(),
            false,
        ));
        if body.tail.is_some() {
            let _ = self.lower_thir_block(body)?;
        } else {
            self.lower_thir_block_stmts(body)?;
        }
        self.loop_stack.pop();

        if !self.builder.is_current_block_terminated() {
            let updated_vals: Vec<ValueId> = loop_vars
                .iter()
                .map(|(name, original_val, _)| {
                    self.scope
                        .get(name)
                        .map(|(v, _)| *v)
                        .unwrap_or(*original_val)
                })
                .collect();
            self.builder.push_instr(
                IrInstr::Br {
                    target: header_bb,
                    args: updated_vals,
                },
                None,
            );
        }

        self.builder.set_current_block(merge_bb);
        for ((name, _, ty), &merge_val) in loop_vars.iter().zip(merge_params.iter()) {
            self.scope.insert(name.clone(), (merge_val, ty.clone()));
        }

        let _ = span;
        self.drop_body_locals(&outer_names);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn lower_thir_for_range(
        &mut self,
        var: &str,
        start: &crate::thir::ThirExpr,
        end: &crate::thir::ThirExpr,
        body: &crate::thir::ThirBlock,
        inclusive: bool,
        step: Option<&crate::thir::ThirExpr>,
        label: &Option<String>,
        span: Span,
    ) -> Result<(), LowerError> {
        let (start_val, loop_var_ty) = self.lower_thir_expr_req(start)?;
        let (end_val, _) = self.lower_thir_expr_req(end)?;

        if !matches!(
            loop_var_ty,
            IrType::Scalar(
                DType::I64
                    | DType::I32
                    | DType::U64
                    | DType::U32
                    | DType::USize
                    | DType::I8
                    | DType::U8
            )
        ) {
            return Err(LowerError::TypeMismatch {
                expected: "integer scalar (for-loop range variable)".to_string(),
                found: format!("{}", loop_var_ty),
                span,
            });
        }

        let outer_names = self.scope_names();
        let mut rebound = find_rebound_vars_thir_block(body, true);
        rebound.sort();
        rebound.dedup();
        if !rebound.iter().any(|n| n == var) {
            rebound.push(var.to_string());
        }

        let mut loop_vars: Vec<(String, ValueId, IrType)> = Vec::new();
        loop_vars.push((var.to_string(), start_val, loop_var_ty.clone()));
        for name in &rebound {
            if name == var {
                continue;
            }
            if let Some((val, ty)) = self.scope.get(name).cloned() {
                loop_vars.push((name.clone(), val, ty));
            }
        }

        let mut initial_vals: Vec<ValueId> = loop_vars.iter().map(|(_, v, _)| *v).collect();
        let taped_loop_vars: Vec<(usize, ValueId)> = loop_vars
            .iter()
            .enumerate()
            .filter_map(|(i, (_, v, _))| self.tape_nodes.get(v).map(|h| (i, *h)))
            .collect();
        for (_, handle) in &taped_loop_vars {
            initial_vals.push(*handle);
        }

        let header_bb = self.builder.create_block(Some("for_header"));
        let body_bb = self.builder.create_block(Some("for_body"));
        let merge_bb = self.builder.create_block(Some("for_merge"));

        let mut header_params: Vec<ValueId> = Vec::new();
        for (name, _, ty) in &loop_vars {
            let p = self
                .builder
                .add_block_param(header_bb, Some(name), ty.clone());
            header_params.push(p);
        }
        let header_handle_params: Vec<ValueId> = taped_loop_vars
            .iter()
            .map(|(i, _)| {
                let name = format!("{}$tape", loop_vars[*i].0);
                self.builder
                    .add_block_param(header_bb, Some(&name), IrType::TapeRef)
            })
            .collect();

        let mut merge_params: Vec<ValueId> = Vec::new();
        for (name, _, ty) in &loop_vars {
            let p = self
                .builder
                .add_block_param(merge_bb, Some(name), ty.clone());
            merge_params.push(p);
        }
        let merge_handle_params: Vec<ValueId> = taped_loop_vars
            .iter()
            .map(|(i, _)| {
                let name = format!("{}$tape", loop_vars[*i].0);
                self.builder
                    .add_block_param(merge_bb, Some(&name), IrType::TapeRef)
            })
            .collect();

        self.builder.push_instr(
            IrInstr::Br {
                target: header_bb,
                args: initial_vals,
            },
            None,
        );

        self.builder.set_current_block(header_bb);
        for ((name, _, ty), &param_val) in loop_vars.iter().zip(header_params.iter()) {
            self.scope.insert(name.clone(), (param_val, ty.clone()));
        }
        for ((i, _), &hp) in taped_loop_vars.iter().zip(header_handle_params.iter()) {
            self.taped_values.insert(header_params[*i]);
            self.tape_nodes.insert(header_params[*i], hp);
        }
        let loop_var_param = header_params[0];
        let cond_result = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: cond_result,
                op: if inclusive {
                    BinOp::CmpLe
                } else {
                    BinOp::CmpLt
                },
                lhs: loop_var_param,
                rhs: end_val,
                ty: IrType::Scalar(DType::Bool),
            },
            Some(IrType::Scalar(DType::Bool)),
        );
        self.builder.push_instr(
            IrInstr::CondBr {
                cond: cond_result,
                then_block: body_bb,
                then_args: vec![],
                else_block: merge_bb,
                else_args: header_params
                    .iter()
                    .copied()
                    .chain(header_handle_params.iter().copied())
                    .collect(),
            },
            None,
        );

        self.builder.set_current_block(body_bb);
        let loop_var_names: Vec<String> = loop_vars.iter().map(|(n, _, _)| n.clone()).collect();
        self.loop_stack.push((
            header_bb,
            merge_bb,
            header_bb,
            loop_var_names,
            label.clone(),
            true,
        ));
        if body.tail.is_some() {
            let _ = self.lower_thir_block(body)?;
        } else {
            self.lower_thir_block_stmts(body)?;
        }
        self.loop_stack.pop();

        if !self.builder.is_current_block_terminated() {
            let cur_loop_var = self
                .scope
                .get(var)
                .map(|(v, _)| *v)
                .unwrap_or(loop_var_param);
            let step_val = if let Some(step_expr) = step {
                self.lower_thir_expr_req(step_expr)?.0
            } else {
                let one = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::ConstInt {
                        result: one,
                        value: 1,
                        ty: loop_var_ty.clone(),
                    },
                    Some(loop_var_ty.clone()),
                );
                one
            };
            let incremented = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::BinOp {
                    result: incremented,
                    op: BinOp::Add,
                    lhs: cur_loop_var,
                    rhs: step_val,
                    ty: loop_var_ty.clone(),
                },
                Some(loop_var_ty.clone()),
            );
            self.scope
                .insert(var.to_string(), (incremented, loop_var_ty));

            let mut updated_vals: Vec<ValueId> = loop_vars
                .iter()
                .map(|(name, original_val, _)| {
                    self.scope
                        .get(name)
                        .map(|(v, _)| *v)
                        .unwrap_or(*original_val)
                })
                .collect();
            for (i, incoming) in &taped_loop_vars {
                let current = updated_vals[*i];
                let handle = self.tape_nodes.get(&current).copied().unwrap_or(*incoming);
                updated_vals.push(handle);
            }
            self.builder.push_instr(
                IrInstr::Br {
                    target: header_bb,
                    args: updated_vals,
                },
                None,
            );
        }

        self.builder.set_current_block(merge_bb);
        for ((name, _, ty), &merge_val) in loop_vars.iter().zip(merge_params.iter()) {
            if name == var {
                self.scope.remove(name);
            } else {
                self.scope.insert(name.clone(), (merge_val, ty.clone()));
            }
        }
        for ((i, _), &hp) in taped_loop_vars.iter().zip(merge_handle_params.iter()) {
            if loop_vars[*i].0 != var {
                self.taped_values.insert(merge_params[*i]);
                self.tape_nodes.insert(merge_params[*i], hp);
            }
        }

        let _ = span;
        self.drop_body_locals(&outer_names);
        Ok(())
    }

    pub(super) fn lower_thir_par_for(
        &mut self,
        var: &str,
        start: &crate::thir::ThirExpr,
        end: &crate::thir::ThirExpr,
        body: &crate::thir::ThirBlock,
        inclusive: bool,
        span: Span,
    ) -> Result<(), LowerError> {
        let counter = self.lambda_counter.get();
        self.lambda_counter.set(counter + 1);
        let fn_name = format!("__par_body_{}", counter);

        let mut captures: Vec<(String, ValueId, IrType)> = self
            .scope
            .iter()
            .filter(|(name, _)| name.as_str() != var)
            .map(|(name, (vid, ty))| (name.clone(), *vid, ty.clone()))
            .collect();
        captures.sort_by(|a, b| a.0.cmp(&b.0));

        {
            let captured_names: std::collections::HashSet<&str> =
                captures.iter().map(|(n, _, _)| n.as_str()).collect();
            let mut offender: Option<(String, String)> = None;
            find_captured_mutation_thir(body, &captured_names, &mut offender);
            if let Some((coll, op)) = offender {
                return Err(LowerError::Rejected {
                    detail: format!(
                        "`par for` body mutates the captured collection `{}` via `{}`.\nIterations run concurrently, so the result depends on thread scheduling.\nUse `atomic` for a shared counter, or build a per-iteration value and combine after the loop.",
                        coll, op
                    ),
                    span,
                });
            }
        }

        let mut params = vec![crate::ir::function::Param {
            name: var.to_string(),
            ty: IrType::Scalar(DType::I64),
        }];
        for (name, _, ty) in &captures {
            params.push(crate::ir::function::Param {
                name: name.clone(),
                ty: ty.clone(),
            });
        }

        let temp_builder = IrFunctionBuilder::new(&fn_name, params, IrType::Scalar(DType::I64));
        let mut body_lowerer = self.new_sub_lowerer(temp_builder);
        let entry = body_lowerer.builder.create_block(Some("entry"));
        body_lowerer.builder.set_current_block(entry);
        let var_val =
            body_lowerer
                .builder
                .add_block_param(entry, Some(var), IrType::Scalar(DType::I64));
        body_lowerer
            .scope
            .insert(var.to_string(), (var_val, IrType::Scalar(DType::I64)));
        for (name, _, ty) in &captures {
            let inner_val = body_lowerer
                .builder
                .add_block_param(entry, Some(name), ty.clone());
            body_lowerer
                .scope
                .insert(name.clone(), (inner_val, ty.clone()));
        }
        if body.tail.is_some() {
            let _ = body_lowerer.lower_thir_block(body)?;
        } else {
            body_lowerer.lower_thir_block_stmts(body)?;
        }
        let dummy_ret = body_lowerer.builder.fresh_value();
        body_lowerer.builder.push_instr(
            IrInstr::ConstInt {
                result: dummy_ret,
                value: 0,
                ty: IrType::Scalar(DType::I64),
            },
            Some(IrType::Scalar(DType::I64)),
        );
        body_lowerer.builder.push_instr(
            IrInstr::Return {
                values: vec![dummy_ret],
            },
            None,
        );
        body_lowerer.builder.seal_unterminated_blocks();
        let ir_func = body_lowerer.builder.build();
        self.lifted_fns.borrow_mut().push(ir_func);

        let (start_val, _) = self.lower_thir_expr_req(start)?;
        let (end_val, _) = self.lower_thir_expr_req(end)?;
        let var_id = self.builder.fresh_value();
        let capture_vals: Vec<ValueId> = captures.iter().map(|(_, v, _)| *v).collect();
        self.builder.set_span_byte(span.start.0);
        self.builder.push_instr(
            IrInstr::ParFor {
                var: var_id,
                start: start_val,
                end: end_val,
                inclusive,
                body_fn: fn_name,
                args: capture_vals,
            },
            None,
        );
        Ok(())
    }

    pub(super) fn lower_thir_foreach(
        &mut self,
        var: &str,
        iter: &crate::thir::ThirExpr,
        body: &crate::thir::ThirBlock,
        label: &Option<String>,
        span: Span,
    ) -> Result<(), LowerError> {
        let i64_ty = IrType::Scalar(DType::I64);
        let (iter_val, iter_ty) = self.lower_thir_expr_req(iter)?;
        let iter_is_str = matches!(iter_ty, IrType::Str);

        if matches!(iter_ty, IrType::Scalar(_)) {
            return Err(LowerError::rich(
                "E0124",
                crate::diagnostics::Diagnostic::error(format!(
                    "`for` cannot iterate `{}`: it needs a list, a range or a str",
                    hints::type_name(&iter_ty)
                ))
                .with_primary_span(span.start.0, span.end.0, None),
            ));
        }

        let len_val = self.builder.fresh_value();
        if iter_is_str {
            self.builder.push_instr(
                IrInstr::StrLen {
                    result: len_val,
                    operand: iter_val,
                },
                Some(i64_ty.clone()),
            );
        } else {
            self.builder.push_instr(
                IrInstr::ListLen {
                    result: len_val,
                    list: iter_val,
                },
                Some(i64_ty.clone()),
            );
        }

        let idx_init = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: idx_init,
                value: 0,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );

        let outer_names = self.scope_names();
        let mut rebound = find_rebound_vars_thir_block(body, true);
        rebound.sort();
        rebound.dedup();
        let idx_name = format!("__foreach_idx_{}", span.start.0);
        if !rebound.contains(&idx_name) {
            rebound.push(idx_name.clone());
        }

        let mut loop_vars: Vec<(String, ValueId, IrType)> = Vec::new();
        loop_vars.push((idx_name.clone(), idx_init, i64_ty.clone()));
        for name in &rebound {
            if name == &idx_name {
                continue;
            }
            if let Some((val, ty)) = self.scope.get(name).cloned() {
                loop_vars.push((name.clone(), val, ty));
            }
        }

        let initial_vals: Vec<ValueId> = loop_vars.iter().map(|(_, v, _)| *v).collect();

        let header_bb = self.builder.create_block(Some("foreach_header"));
        let body_bb = self.builder.create_block(Some("foreach_body"));
        let merge_bb = self.builder.create_block(Some("foreach_merge"));

        let mut header_params: Vec<ValueId> = Vec::new();
        for (name, _, ty) in &loop_vars {
            let p = self
                .builder
                .add_block_param(header_bb, Some(name), ty.clone());
            header_params.push(p);
        }

        let mut merge_params: Vec<ValueId> = Vec::new();
        for (name, _, ty) in &loop_vars {
            let p = self
                .builder
                .add_block_param(merge_bb, Some(name), ty.clone());
            merge_params.push(p);
        }

        self.builder.push_instr(
            IrInstr::Br {
                target: header_bb,
                args: initial_vals,
            },
            None,
        );

        self.builder.set_current_block(header_bb);
        let idx_param = header_params[0];
        for ((name, _, ty), &param_val) in loop_vars.iter().zip(header_params.iter()) {
            self.scope.insert(name.clone(), (param_val, ty.clone()));
        }

        let cond_val = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: cond_val,
                op: BinOp::CmpLt,
                lhs: idx_param,
                rhs: len_val,
                ty: i64_ty.clone(),
            },
            Some(IrType::Scalar(DType::Bool)),
        );
        self.builder.push_instr(
            IrInstr::CondBr {
                cond: cond_val,
                then_block: body_bb,
                then_args: vec![],
                else_block: merge_bb,
                else_args: header_params.clone(),
            },
            None,
        );

        self.builder.set_current_block(body_bb);
        let elem_ty = match &iter_ty {
            IrType::List(inner) => *inner.clone(),
            IrType::Str => i64_ty.clone(),
            _ => return Err(self.undetermined(span, "the type of this `for`'s loop variable")),
        };
        let elem_val = self.builder.fresh_value();
        if iter_is_str {
            self.builder.push_instr(
                IrInstr::StrIndex {
                    result: elem_val,
                    string: iter_val,
                    index: idx_param,
                },
                Some(i64_ty.clone()),
            );
        } else {
            self.builder.push_instr(
                IrInstr::ListGet {
                    result: elem_val,
                    list: iter_val,
                    index: idx_param,
                    elem_ty: elem_ty.clone(),
                },
                Some(elem_ty.clone()),
            );
        }
        self.scope.insert(var.to_string(), (elem_val, elem_ty));

        let loop_var_names: Vec<String> = loop_vars.iter().map(|(n, _, _)| n.clone()).collect();
        self.loop_stack.push((
            header_bb,
            merge_bb,
            header_bb,
            loop_var_names,
            label.clone(),
            false,
        ));
        if body.tail.is_some() {
            let _ = self.lower_thir_block(body)?;
        } else {
            self.lower_thir_block_stmts(body)?;
        }
        self.loop_stack.pop();

        if !self.builder.is_current_block_terminated() {
            let cur_idx = self
                .scope
                .get(&idx_name)
                .map(|(v, _)| *v)
                .unwrap_or(idx_param);
            let one = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::ConstInt {
                    result: one,
                    value: 1,
                    ty: i64_ty.clone(),
                },
                Some(i64_ty.clone()),
            );
            let next_idx = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::BinOp {
                    result: next_idx,
                    op: BinOp::Add,
                    lhs: cur_idx,
                    rhs: one,
                    ty: i64_ty.clone(),
                },
                Some(i64_ty.clone()),
            );
            self.scope
                .insert(idx_name.clone(), (next_idx, i64_ty.clone()));

            let updated_vals: Vec<ValueId> = loop_vars
                .iter()
                .map(|(name, original_val, _)| {
                    self.scope
                        .get(name)
                        .map(|(v, _)| *v)
                        .unwrap_or(*original_val)
                })
                .collect();
            self.builder.push_instr(
                IrInstr::Br {
                    target: header_bb,
                    args: updated_vals,
                },
                None,
            );
        }

        self.builder.set_current_block(merge_bb);
        for ((name, _, ty), &merge_val) in loop_vars.iter().zip(merge_params.iter()) {
            if name != &idx_name {
                self.scope.insert(name.clone(), (merge_val, ty.clone()));
            }
        }
        self.scope.remove(&idx_name);
        self.scope.remove(var);

        let _ = span;
        self.drop_body_locals(&outer_names);
        Ok(())
    }

    /// Lowers a `while cond { body }` loop using SSA block parameters.
    /// Drop the bindings a loop body introduced, restoring the enclosing scope.
    ///
    /// The lowerer keeps one flat scope per function, so a `val` declared
    /// inside a loop body stayed visible after the loop ended:
    /// `for i in 0..2 { val r = 1.5; }; to_str(r)` compiled and printed 1.5.
    /// That is wrong on its own, and it is the substrate for known-issues #27.
    ///
    /// The leaked binding is found by the *next* loop's rebound-variable scan
    /// -- `find_rebound_vars` collects `Let` statements, since without block
    /// scoping a `val` in a body behaves like an assignment -- and that scan
    /// types the new loop's block parameter from the stale outer binding. Two
    /// sibling loops each declaring `val r`, one f64 and one i64, therefore
    /// failed with "type mismatch: i64 vs f64" on a block argument, naming
    /// values that appear nowhere in the source, with nothing to suggest the
    /// two loops were related.
    ///
    /// Call with the scope's key set captured *before* the body is lowered.
    /// Names the loop legitimately carries out -- outer variables assigned in
    /// the body -- were in scope beforehand and so are kept.
    pub(super) fn drop_body_locals(&mut self, outer: &std::collections::HashSet<String>) {
        let introduced: Vec<String> = self
            .scope
            .keys()
            .filter(|n| !outer.contains(*n))
            .cloned()
            .collect();
        for n in introduced {
            self.scope.remove(&n);
        }
    }

    /// Snapshot the names currently in scope, for `drop_body_locals`.
    pub(super) fn scope_names(&self) -> std::collections::HashSet<String> {
        self.scope.keys().cloned().collect()
    }

    pub(super) fn lower_while(
        &mut self,
        cond: &AstExpr,
        body: &AstBlock,
        label: &Option<String>,
        span: Span,
    ) -> Result<(), LowerError> {
        // Pre-scan body to find which variables get rebound.
        let outer_names = self.scope_names();
        let rebound = find_rebound_vars(body);

        // Collect the loop variables that exist in the current scope.
        let mut loop_vars: Vec<(String, ValueId, IrType)> = Vec::new();
        for name in &rebound {
            if let Some((val, ty)) = self.scope.get(name).cloned() {
                loop_vars.push((name.clone(), val, ty));
            }
        }

        let mut initial_vals: Vec<ValueId> = loop_vars.iter().map(|(_, v, _)| *v).collect();

        // A loop variable holding a taped value carries *two* things across the
        // back-edge: the primal and the handle to its tape node. Only the primal
        // used to be threaded, so on the second iteration the handle was gone and
        // `backward` was rejected -- a loop-accumulated loss could not be written
        // at all (#49). The handles ride along as extra block parameters,
        // appended after the primals so the existing indices are undisturbed.
        let taped_loop_vars: Vec<(usize, ValueId)> = loop_vars
            .iter()
            .enumerate()
            .filter_map(|(i, (_, v, _))| self.tape_nodes.get(v).map(|h| (i, *h)))
            .collect();
        for (_, handle) in &taped_loop_vars {
            initial_vals.push(*handle);
        }

        // Create the three blocks.
        let header_bb = self.builder.create_block(Some("while_header"));
        let body_bb = self.builder.create_block(Some("while_body"));
        let merge_bb = self.builder.create_block(Some("while_merge"));

        // Add block params to header (one per loop variable).
        let mut header_params: Vec<ValueId> = Vec::new();
        for (name, _, ty) in &loop_vars {
            let p = self
                .builder
                .add_block_param(header_bb, Some(name), ty.clone());
            header_params.push(p);
        }
        let header_handle_params: Vec<ValueId> = taped_loop_vars
            .iter()
            .map(|(i, _)| {
                let name = format!("{}$tape", loop_vars[*i].0);
                self.builder
                    .add_block_param(header_bb, Some(&name), IrType::TapeRef)
            })
            .collect();

        // Add block params to merge (receive exit values from header's else path).
        let mut merge_params: Vec<ValueId> = Vec::new();
        for (name, _, ty) in &loop_vars {
            let p = self
                .builder
                .add_block_param(merge_bb, Some(name), ty.clone());
            merge_params.push(p);
        }
        let merge_handle_params: Vec<ValueId> = taped_loop_vars
            .iter()
            .map(|(i, _)| {
                let name = format!("{}$tape", loop_vars[*i].0);
                self.builder
                    .add_block_param(merge_bb, Some(&name), IrType::TapeRef)
            })
            .collect();

        // From the current block, branch to header with initial values.
        self.builder.push_instr(
            IrInstr::Br {
                target: header_bb,
                args: initial_vals,
            },
            None,
        );

        // Lower condition in header block.
        self.builder.set_current_block(header_bb);
        for ((name, _, ty), &param_val) in loop_vars.iter().zip(header_params.iter()) {
            self.scope.insert(name.clone(), (param_val, ty.clone()));
        }
        // Re-point the tape mapping at this iteration's parameters, so a taped
        // value read inside the body resolves to the handle that came round the
        // back-edge rather than to the one from before the loop.
        for ((i, _), &hp) in taped_loop_vars.iter().zip(header_handle_params.iter()) {
            self.taped_values.insert(header_params[*i]);
            self.tape_nodes.insert(header_params[*i], hp);
        }

        let (cond_val, _) = self.lower_expr(cond)?;

        // Emit CondBr: true → body (no args), false → merge (current header params).
        self.builder.push_instr(
            IrInstr::CondBr {
                cond: cond_val,
                then_block: body_bb,
                then_args: vec![],
                else_block: merge_bb,
                else_args: header_params
                    .iter()
                    .copied()
                    .chain(header_handle_params.iter().copied())
                    .collect(),
            },
            None,
        );

        // Lower body block.
        self.builder.set_current_block(body_bb);
        let loop_var_names: Vec<String> = loop_vars.iter().map(|(n, _, _)| n.clone()).collect();
        self.loop_stack.push((
            header_bb,
            merge_bb,
            header_bb,
            loop_var_names.clone(),
            label.clone(),
            false,
        ));
        let _ = self.lower_block(body)?;
        self.loop_stack.pop();

        // Emit back-edge Br if the body wasn't terminated by break/continue.
        if !self.builder.is_current_block_terminated() {
            let mut updated_vals: Vec<ValueId> = loop_vars
                .iter()
                .map(|(name, original_val, _)| {
                    self.scope
                        .get(name)
                        .map(|(v, _)| *v)
                        .unwrap_or(*original_val)
                })
                .collect();
            // Whatever the body last assigned to a taped loop variable has its
            // own tape node; that handle is what the next iteration must see.
            for (i, incoming) in &taped_loop_vars {
                let current = updated_vals[*i];
                let handle = self.tape_nodes.get(&current).copied().unwrap_or(*incoming);
                updated_vals.push(handle);
            }
            self.builder.push_instr(
                IrInstr::Br {
                    target: header_bb,
                    args: updated_vals,
                },
                None,
            );
        }

        // Move to merge block and update scope with loop var final values.
        self.builder.set_current_block(merge_bb);
        for ((name, _, ty), &merge_val) in loop_vars.iter().zip(merge_params.iter()) {
            self.scope.insert(name.clone(), (merge_val, ty.clone()));
        }
        // The whole point: after the loop, the accumulated value still has a
        // handle, so `backward(acc)` sees a tape node rather than a bare f64.
        for ((i, _), &hp) in taped_loop_vars.iter().zip(merge_handle_params.iter()) {
            self.taped_values.insert(merge_params[*i]);
            self.tape_nodes.insert(merge_params[*i], hp);
        }

        let _ = span;
        // A `val` declared in the body is local to it (#27).
        self.drop_body_locals(&outer_names);
        Ok(())
    }

    /// Lowers `for <var> in <start>..<end> { body }` to SSA block-param loop.
    ///
    /// The loop variable is incremented by 1 after each body execution.
    /// Semantics: `start` and `end` are evaluated once before the loop.
    // Loop lowering necessarily receives every syntactic component plus the
    // active builder; grouping them would obscure the AST-to-SSA mapping.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn lower_for_range(
        &mut self,
        var: &crate::parser::ast::Ident,
        start: &AstExpr,
        end: &AstExpr,
        body: &AstBlock,
        inclusive: bool,
        step: Option<&AstExpr>,
        label: &Option<String>,
        span: Span,
    ) -> Result<(), LowerError> {
        // 1. Evaluate start and end once in the current (pre-loop) block.
        let (start_val, loop_var_ty) = self.lower_expr(start)?;
        let (end_val, _) = self.lower_expr(end)?;

        // Defensive: the loop variable must be an integer scalar.  A non-integer
        // type here usually means the start expression was something unexpected
        // (e.g. a bare side-effecting call before the loop that the parser
        // attached to start).  Fail clearly rather than propagating a bad type
        // through all the block params.
        if !matches!(
            loop_var_ty,
            IrType::Scalar(
                DType::I64
                    | DType::I32
                    | DType::U64
                    | DType::U32
                    | DType::USize
                    | DType::I8
                    | DType::U8
            )
        ) {
            return Err(LowerError::TypeMismatch {
                expected: "integer scalar (for-loop range variable)".to_string(),
                found: format!("{}", loop_var_ty),
                span,
            });
        }

        // 2. Pre-scan body for rebounded variables; loop var is always rebound.
        let outer_names = self.scope_names();
        let mut rebound = find_rebound_vars(body);
        if !rebound.contains(&var.name) {
            rebound.push(var.name.clone());
        }

        // 3. Collect loop variables: loop var first, then other rebound outer vars.
        let mut loop_vars: Vec<(String, ValueId, IrType)> = Vec::new();
        loop_vars.push((var.name.clone(), start_val, loop_var_ty.clone()));
        for name in &rebound {
            if name == &var.name {
                continue;
            }
            if let Some((val, ty)) = self.scope.get(name).cloned() {
                loop_vars.push((name.clone(), val, ty));
            }
        }

        let mut initial_vals: Vec<ValueId> = loop_vars.iter().map(|(_, v, _)| *v).collect();

        // A taped loop variable carries its tape handle across the back-edge as
        // well as its primal; see the same treatment in `lower_while` (#49).
        let taped_loop_vars: Vec<(usize, ValueId)> = loop_vars
            .iter()
            .enumerate()
            .filter_map(|(i, (_, v, _))| self.tape_nodes.get(v).map(|h| (i, *h)))
            .collect();
        for (_, handle) in &taped_loop_vars {
            initial_vals.push(*handle);
        }

        // 4. Create blocks.
        let header_bb = self.builder.create_block(Some("for_header"));
        let body_bb = self.builder.create_block(Some("for_body"));
        let merge_bb = self.builder.create_block(Some("for_merge"));

        // 5. Header block params (one per loop variable).
        let mut header_params: Vec<ValueId> = Vec::new();
        for (name, _, ty) in &loop_vars {
            let p = self
                .builder
                .add_block_param(header_bb, Some(name), ty.clone());
            header_params.push(p);
        }
        let header_handle_params: Vec<ValueId> = taped_loop_vars
            .iter()
            .map(|(i, _)| {
                let name = format!("{}$tape", loop_vars[*i].0);
                self.builder
                    .add_block_param(header_bb, Some(&name), IrType::TapeRef)
            })
            .collect();

        // 6. Merge block params (receive final values on loop exit).
        let mut merge_params: Vec<ValueId> = Vec::new();
        for (name, _, ty) in &loop_vars {
            let p = self
                .builder
                .add_block_param(merge_bb, Some(name), ty.clone());
            merge_params.push(p);
        }
        let merge_handle_params: Vec<ValueId> = taped_loop_vars
            .iter()
            .map(|(i, _)| {
                let name = format!("{}$tape", loop_vars[*i].0);
                self.builder
                    .add_block_param(merge_bb, Some(&name), IrType::TapeRef)
            })
            .collect();

        // 7. Branch from current block to header with initial values.
        self.builder.push_instr(
            IrInstr::Br {
                target: header_bb,
                args: initial_vals,
            },
            None,
        );

        // 8. Header: update scope with params, emit loop_var < end (or <=) condition.
        self.builder.set_current_block(header_bb);
        for ((name, _, ty), &param_val) in loop_vars.iter().zip(header_params.iter()) {
            self.scope.insert(name.clone(), (param_val, ty.clone()));
        }
        for ((i, _), &hp) in taped_loop_vars.iter().zip(header_handle_params.iter()) {
            self.taped_values.insert(header_params[*i]);
            self.tape_nodes.insert(header_params[*i], hp);
        }
        let loop_var_param = header_params[0]; // first param is always the loop var
        let cond_result = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: cond_result,
                op: if inclusive {
                    BinOp::CmpLe
                } else {
                    BinOp::CmpLt
                },
                lhs: loop_var_param,
                rhs: end_val,
                ty: IrType::Scalar(DType::Bool),
            },
            Some(IrType::Scalar(DType::Bool)),
        );
        self.builder.push_instr(
            IrInstr::CondBr {
                cond: cond_result,
                then_block: body_bb,
                then_args: vec![],
                else_block: merge_bb,
                else_args: header_params
                    .iter()
                    .copied()
                    .chain(header_handle_params.iter().copied())
                    .collect(),
            },
            None,
        );

        // 9. Body block.
        self.builder.set_current_block(body_bb);
        let loop_var_names: Vec<String> = loop_vars.iter().map(|(n, _, _)| n.clone()).collect();
        self.loop_stack.push((
            header_bb,
            merge_bb,
            header_bb,
            loop_var_names,
            label.clone(),
            true,
        ));
        // Use lower_block (not lower_block_stmts) so tail expressions like `print(x)` without `;`
        // are also evaluated as side-effecting statements.
        self.lower_block(body)?;
        self.loop_stack.pop();

        // 10. Emit increment and back-edge (if body not terminated by break/continue).
        if !self.builder.is_current_block_terminated() {
            let cur_loop_var = self
                .scope
                .get(&var.name)
                .map(|(v, _)| *v)
                .unwrap_or(loop_var_param);
            let step_val = if let Some(step_expr) = step {
                self.lower_expr(step_expr)?.0
            } else {
                let one = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::ConstInt {
                        result: one,
                        value: 1,
                        ty: loop_var_ty.clone(),
                    },
                    Some(loop_var_ty.clone()),
                );
                one
            };
            let incremented = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::BinOp {
                    result: incremented,
                    op: BinOp::Add,
                    lhs: cur_loop_var,
                    rhs: step_val,
                    ty: loop_var_ty.clone(),
                },
                Some(loop_var_ty.clone()),
            );
            self.scope
                .insert(var.name.clone(), (incremented, loop_var_ty));

            let mut updated_vals: Vec<ValueId> = loop_vars
                .iter()
                .map(|(name, original_val, _)| {
                    self.scope
                        .get(name)
                        .map(|(v, _)| *v)
                        .unwrap_or(*original_val)
                })
                .collect();
            for (i, incoming) in &taped_loop_vars {
                let current = updated_vals[*i];
                let handle = self.tape_nodes.get(&current).copied().unwrap_or(*incoming);
                updated_vals.push(handle);
            }
            self.builder.push_instr(
                IrInstr::Br {
                    target: header_bb,
                    args: updated_vals,
                },
                None,
            );
        }

        // 11. Move to merge block; update scope with final values of rebound outer
        //     variables, but remove the loop variable (it's no longer in scope).
        self.builder.set_current_block(merge_bb);
        for ((name, _, ty), &merge_val) in loop_vars.iter().zip(merge_params.iter()) {
            if name == &var.name {
                // Loop variable goes out of scope at the end of the for loop.
                self.scope.remove(name);
            } else {
                self.scope.insert(name.clone(), (merge_val, ty.clone()));
            }
        }
        for ((i, _), &hp) in taped_loop_vars.iter().zip(merge_handle_params.iter()) {
            if loop_vars[*i].0 != var.name {
                self.taped_values.insert(merge_params[*i]);
                self.tape_nodes.insert(merge_params[*i], hp);
            }
        }

        let _ = span;
        // A `val` declared in the body is local to it (#27).
        self.drop_body_locals(&outer_names);
        Ok(())
    }

    /// Lowers `for <var> in <list_expr> { body }` to SSA block-param loop.
    ///
    /// Desugars to:
    /// ```text
    /// val __iter_N = lower(iter_expr)
    /// var __idx_N  = 0
    /// val __len_N  = list_len(__iter_N)
    /// while __idx_N < __len_N {
    ///     val <var> = list_get(__iter_N, __idx_N)
    ///     lower(body)
    ///     __idx_N = __idx_N + 1
    /// }
    /// ```
    pub(super) fn lower_foreach(
        &mut self,
        var: &crate::parser::ast::Ident,
        iter: &AstExpr,
        body: &AstBlock,
        span: Span,
    ) -> Result<(), LowerError> {
        let i64_ty = IrType::Scalar(DType::I64);

        // Evaluate the list expression once.
        let (iter_val, iter_ty) = self.lower_expr(iter)?;

        // `for b in s` over a `str` iterates its bytes. This used to fall
        // through to the list path, so the loop called `list_len` on a string
        // and died at runtime with "list_len: not a list" -- accepted by the
        // compiler, broken at execution. See known-issues #63.
        let iter_is_str = matches!(iter_ty, IrType::Str);

        // A scalar is not iterable at all, and treating it as one produced the
        // same shape of failure. Reject it here rather than emitting a
        // `list_len` that cannot succeed.
        if matches!(iter_ty, IrType::Scalar(_)) {
            return Err(LowerError::rich(
                "E0124",
                crate::diagnostics::Diagnostic::error(format!(
                    "`for` cannot iterate `{}`: it needs a list, a range or a str",
                    hints::type_name(&iter_ty)
                ))
                .with_primary_span((span).start.0, (span).end.0, None),
            ));
        }

        // Compute length once before loop.
        let len_val = self.builder.fresh_value();
        if iter_is_str {
            self.builder.push_instr(
                IrInstr::StrLen {
                    result: len_val,
                    operand: iter_val,
                },
                Some(i64_ty.clone()),
            );
        } else {
            self.builder.push_instr(
                IrInstr::ListLen {
                    result: len_val,
                    list: iter_val,
                },
                Some(i64_ty.clone()),
            );
        }

        // Initial index = 0.
        let idx_init = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstInt {
                result: idx_init,
                value: 0,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );

        // Pre-scan body for rebound outer vars (the loop index is always rebound).
        let outer_names = self.scope_names();
        let mut rebound = find_rebound_vars(body);
        let idx_name = format!("__foreach_idx_{}", var.span.start.0);
        if !rebound.contains(&idx_name) {
            rebound.push(idx_name.clone());
        }

        // Collect loop variables: index first, then other rebound outer vars.
        let mut loop_vars: Vec<(String, ValueId, IrType)> = Vec::new();
        loop_vars.push((idx_name.clone(), idx_init, i64_ty.clone()));
        for name in &rebound {
            if name == &idx_name {
                continue;
            }
            if let Some((val, ty)) = self.scope.get(name).cloned() {
                loop_vars.push((name.clone(), val, ty));
            }
        }

        let initial_vals: Vec<ValueId> = loop_vars.iter().map(|(_, v, _)| *v).collect();

        // Create blocks.
        let header_bb = self.builder.create_block(Some("foreach_header"));
        let body_bb = self.builder.create_block(Some("foreach_body"));
        let merge_bb = self.builder.create_block(Some("foreach_merge"));

        // Header block params.
        let mut header_params: Vec<ValueId> = Vec::new();
        for (name, _, ty) in &loop_vars {
            let p = self
                .builder
                .add_block_param(header_bb, Some(name), ty.clone());
            header_params.push(p);
        }

        // Merge block params.
        let mut merge_params: Vec<ValueId> = Vec::new();
        for (name, _, ty) in &loop_vars {
            let p = self
                .builder
                .add_block_param(merge_bb, Some(name), ty.clone());
            merge_params.push(p);
        }

        // Branch from current block to header.
        self.builder.push_instr(
            IrInstr::Br {
                target: header_bb,
                args: initial_vals,
            },
            None,
        );

        // Header block: check index < len.
        self.builder.set_current_block(header_bb);
        let idx_param = header_params[0];
        for ((name, _, ty), &param_val) in loop_vars.iter().zip(header_params.iter()) {
            self.scope.insert(name.clone(), (param_val, ty.clone()));
        }

        let cond_val = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result: cond_val,
                op: BinOp::CmpLt,
                lhs: idx_param,
                rhs: len_val,
                ty: i64_ty.clone(),
            },
            Some(IrType::Scalar(DType::Bool)),
        );
        self.builder.push_instr(
            IrInstr::CondBr {
                cond: cond_val,
                then_block: body_bb,
                then_args: vec![],
                else_block: merge_bb,
                else_args: header_params.clone(),
            },
            None,
        );

        // Body block.
        self.builder.set_current_block(body_bb);

        // Bind loop variable: list_get(iter_val, idx_param), or the byte at
        // that index when iterating a str (#63).
        let elem_ty = match &iter_ty {
            IrType::List(inner) => *inner.clone(),
            IrType::Str => i64_ty.clone(),
            _ => return Err(self.undetermined(span, "the type of this `for`'s loop variable")),
        };
        let elem_val = self.builder.fresh_value();
        if iter_is_str {
            self.builder.push_instr(
                IrInstr::StrIndex {
                    result: elem_val,
                    string: iter_val,
                    index: idx_param,
                },
                Some(i64_ty.clone()),
            );
        } else {
            self.builder.push_instr(
                IrInstr::ListGet {
                    result: elem_val,
                    list: iter_val,
                    index: idx_param,
                    elem_ty: elem_ty.clone(),
                },
                Some(elem_ty.clone()),
            );
        }
        self.scope.insert(var.name.clone(), (elem_val, elem_ty));

        let loop_var_names: Vec<String> = loop_vars.iter().map(|(n, _, _)| n.clone()).collect();
        self.loop_stack.push((
            header_bb,
            merge_bb,
            header_bb,
            loop_var_names.clone(),
            None,
            false,
        ));
        let _ = self.lower_block(body)?;
        self.loop_stack.pop();

        // Emit back-edge Br if body was not terminated.
        if !self.builder.is_current_block_terminated() {
            // Increment index.
            let cur_idx = self
                .scope
                .get(&idx_name)
                .map(|(v, _)| *v)
                .unwrap_or(idx_param);
            let one = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::ConstInt {
                    result: one,
                    value: 1,
                    ty: i64_ty.clone(),
                },
                Some(i64_ty.clone()),
            );
            let next_idx = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::BinOp {
                    result: next_idx,
                    op: BinOp::Add,
                    lhs: cur_idx,
                    rhs: one,
                    ty: i64_ty.clone(),
                },
                Some(i64_ty.clone()),
            );
            self.scope
                .insert(idx_name.clone(), (next_idx, i64_ty.clone()));

            let updated_vals: Vec<ValueId> = loop_vars
                .iter()
                .map(|(name, original_val, _)| {
                    self.scope
                        .get(name)
                        .map(|(v, _)| *v)
                        .unwrap_or(*original_val)
                })
                .collect();
            self.builder.push_instr(
                IrInstr::Br {
                    target: header_bb,
                    args: updated_vals,
                },
                None,
            );
        }

        // Move to merge block; restore outer rebound vars.
        self.builder.set_current_block(merge_bb);
        for ((name, _, ty), &merge_val) in loop_vars.iter().zip(merge_params.iter()) {
            if name != &idx_name {
                self.scope.insert(name.clone(), (merge_val, ty.clone()));
            }
        }
        // Remove the synthetic index name from scope if it leaked in.
        self.scope.remove(&idx_name);
        // Loop iteration variable is not in scope after the loop.
        self.scope.remove(&var.name);

        let _ = span;
        // A `val` declared in the body is local to it (#27).
        self.drop_body_locals(&outer_names);
        Ok(())
    }

    /// Lowers a `loop { body }` (infinite loop). `break` exits to merge_bb.
    pub(super) fn lower_loop(
        &mut self,
        body: &AstBlock,
        label: &Option<String>,
        span: Span,
    ) -> Result<(), LowerError> {
        // Pre-scan body to find which variables get rebound inside the loop.
        let outer_names = self.scope_names();
        let rebound = find_rebound_vars(body);

        // Collect the loop variables that exist in the current scope.
        let mut loop_vars: Vec<(String, ValueId, IrType)> = Vec::new();
        for name in &rebound {
            if let Some((val, ty)) = self.scope.get(name).cloned() {
                loop_vars.push((name.clone(), val, ty));
            }
        }

        let initial_vals: Vec<ValueId> = loop_vars.iter().map(|(_, v, _)| *v).collect();

        // Create header/body/merge blocks with SSA block params for loop vars.
        let header_bb = self.builder.create_block(Some("loop_header"));
        let body_bb = self.builder.create_block(Some("loop_body"));
        let merge_bb = self.builder.create_block(Some("loop_merge"));

        // Header block params (one per loop variable).
        let mut header_params: Vec<ValueId> = Vec::new();
        for (name, _, ty) in &loop_vars {
            let p = self
                .builder
                .add_block_param(header_bb, Some(name), ty.clone());
            header_params.push(p);
        }

        // Merge block params (receive final values on loop exit).
        let mut merge_params: Vec<ValueId> = Vec::new();
        for (name, _, ty) in &loop_vars {
            let p = self
                .builder
                .add_block_param(merge_bb, Some(name), ty.clone());
            merge_params.push(p);
        }

        // Branch from current block to header with initial values.
        self.builder.push_instr(
            IrInstr::Br {
                target: header_bb,
                args: initial_vals,
            },
            None,
        );

        // Header: update scope with params and jump to body.
        self.builder.set_current_block(header_bb);
        for ((name, _, ty), &param_val) in loop_vars.iter().zip(header_params.iter()) {
            self.scope.insert(name.clone(), (param_val, ty.clone()));
        }
        self.builder.push_instr(
            IrInstr::Br {
                target: body_bb,
                args: vec![],
            },
            None,
        );

        // Lower body block.
        self.builder.set_current_block(body_bb);
        let loop_var_names: Vec<String> = loop_vars.iter().map(|(n, _, _)| n.clone()).collect();
        self.loop_stack.push((
            header_bb,
            merge_bb,
            header_bb,
            loop_var_names.clone(),
            label.clone(),
            false,
        ));
        let _ = self.lower_block(body)?;
        self.loop_stack.pop();

        // Emit back-edge Br if the body wasn't terminated by break/continue.
        if !self.builder.is_current_block_terminated() {
            let updated_vals: Vec<ValueId> = loop_vars
                .iter()
                .map(|(name, original_val, _)| {
                    self.scope
                        .get(name)
                        .map(|(v, _)| *v)
                        .unwrap_or(*original_val)
                })
                .collect();
            self.builder.push_instr(
                IrInstr::Br {
                    target: header_bb,
                    args: updated_vals,
                },
                None,
            );
        }

        // Move to merge block and update scope with final values.
        self.builder.set_current_block(merge_bb);
        for ((name, _, ty), &merge_val) in loop_vars.iter().zip(merge_params.iter()) {
            self.scope.insert(name.clone(), (merge_val, ty.clone()));
        }

        let _ = span;
        // A `val` declared in the body is local to it (#27).
        self.drop_body_locals(&outer_names);
        Ok(())
    }

    /// Lowers `break [label]` — jumps to the merge block of the (optionally labeled) loop.
    pub(super) fn lower_break(
        &mut self,
        label: &Option<String>,
        span: Span,
    ) -> Result<(), LowerError> {
        let (_, merge_bb, _, loop_var_names, _, _) = if let Some(target_label) = label {
            self.loop_stack
                .iter()
                .rev()
                .find(|(_, _, _, _, lbl, _)| lbl.as_ref() == Some(target_label))
                .cloned()
                .ok_or_else(|| {
                    LowerError::rich(
                        "E0123",
                        crate::diagnostics::Diagnostic::error(format!(
                            "`break {}`: no enclosing loop has that label",
                            target_label
                        ))
                        .with_primary_span(
                            (span).start.0,
                            (span).end.0,
                            None,
                        ),
                    )
                })?
        } else {
            self.loop_stack.last().cloned().ok_or_else(|| {
                LowerError::rich(
                    "E0123",
                    crate::diagnostics::Diagnostic::error("`break` outside a loop".to_string())
                        .with_primary_span((span).start.0, (span).end.0, None),
                )
            })?
        };

        let mut args = Vec::with_capacity(loop_var_names.len());
        for name in &loop_var_names {
            let Some((value, _)) = self.scope.get(name) else {
                return Err(LowerError::rich(
                    "E0900",
                    crate::diagnostics::Diagnostic::error(format!(
                        "internal compiler error: loop variable `{}` was not tracked",
                        name
                    ))
                    .with_primary_span((span).start.0, (span).end.0, None),
                ));
            };
            args.push(*value);
        }

        self.builder.push_instr(
            IrInstr::Br {
                target: merge_bb,
                args,
            },
            None,
        );
        Ok(())
    }

    /// Lowers `continue [label]` — jumps to the continue target of the (optionally labeled) loop.
    pub(super) fn lower_continue(
        &mut self,
        label: &Option<String>,
        span: Span,
    ) -> Result<(), LowerError> {
        let (header_bb, _, _, loop_var_names, _, is_for_range) = if let Some(target_label) = label {
            self.loop_stack
                .iter()
                .rev()
                .find(|(_, _, _, _, lbl, _)| lbl.as_ref() == Some(target_label))
                .cloned()
                .ok_or_else(|| {
                    LowerError::rich(
                        "E0123",
                        crate::diagnostics::Diagnostic::error(format!(
                            "`continue {}`: no enclosing loop has that label",
                            target_label
                        ))
                        .with_primary_span(
                            (span).start.0,
                            (span).end.0,
                            None,
                        ),
                    )
                })?
        } else {
            self.loop_stack.last().cloned().ok_or_else(|| {
                LowerError::rich(
                    "E0123",
                    crate::diagnostics::Diagnostic::error("`continue` outside a loop".to_string())
                        .with_primary_span((span).start.0, (span).end.0, None),
                )
            })?
        };

        if is_for_range {
            // For-range: increment the loop var inline, then branch to header with all
            // updated values. Save and restore scope so the increment doesn't leak.
            let saved_scope: Vec<_> = self
                .scope
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();

            let (cur_loop_var, loop_var_ty) = self
                .scope
                .get(&loop_var_names[0])
                .cloned()
                .unwrap_or((ValueId(0), IrType::Scalar(DType::I64)));
            let one = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::ConstInt {
                    result: one,
                    value: 1,
                    ty: loop_var_ty.clone(),
                },
                Some(loop_var_ty.clone()),
            );
            let incremented = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::BinOp {
                    result: incremented,
                    op: BinOp::Add,
                    lhs: cur_loop_var,
                    rhs: one,
                    ty: loop_var_ty.clone(),
                },
                Some(loop_var_ty.clone()),
            );
            self.scope
                .insert(loop_var_names[0].clone(), (incremented, loop_var_ty));

            let mut args = Vec::with_capacity(loop_var_names.len());
            for name in &loop_var_names {
                let Some((value, _)) = self.scope.get(name) else {
                    return Err(LowerError::rich(
                        "E0900",
                        crate::diagnostics::Diagnostic::error(format!(
                            "internal compiler error: loop variable `{}` was not tracked",
                            name
                        ))
                        .with_primary_span(
                            (span).start.0,
                            (span).end.0,
                            None,
                        ),
                    ));
                };
                args.push(*value);
            }

            // Restore scope so subsequent code isn't affected.
            self.scope.clear();
            for (k, v) in saved_scope {
                self.scope.insert(k, v);
            }

            self.builder.push_instr(
                IrInstr::Br {
                    target: header_bb,
                    args,
                },
                None,
            );
        } else {
            // While/loop: just pass current values of loop vars to header params.
            let mut args = Vec::with_capacity(loop_var_names.len());
            for name in &loop_var_names {
                let Some((value, _)) = self.scope.get(name) else {
                    return Err(LowerError::rich(
                        "E0900",
                        crate::diagnostics::Diagnostic::error(format!(
                            "internal compiler error: loop variable `{}` was not tracked",
                            name
                        ))
                        .with_primary_span(
                            (span).start.0,
                            (span).end.0,
                            None,
                        ),
                    ));
                };
                args.push(*value);
            }
            self.builder.push_instr(
                IrInstr::Br {
                    target: header_bb,
                    args,
                },
                None,
            );
        }
        Ok(())
    }
}
