//! Function call, named-argument, method-call, taped-inlining, and `einsum` lowering.

use crate::error::LowerError;
use crate::ir::instr::{BinOp, IrInstr, ScalarUnaryOp, TensorOp};
use crate::ir::types::{DType, IrType};
use crate::ir::value::ValueId;
use crate::parser::ast::{AstExpr, AstStmt, Ident};
use crate::parser::lexer::Span;

use super::{
    builtin_arity_error, derive_einsum_result_type, get_qualified_enum_name_thir, hints,
    infer_generic_fn_subs, ir_type_dispatch_name, lower_function_with_generics_and_subs,
    mangle_param_list, method_on_builtin_type_thir, resolve_ast_type_with_subs, resolve_qualifier,
    runtime_builtin, same_constructor, synth_marker_thir_expr, taped_inline_ok_block, thir_to_ast,
    typeck, Lowerer, CURRENT_FN_ASTS, CURRENT_USER_FNS, DYN_UNCALLABLE,
};

impl<'m> Lowerer<'m> {
    pub(super) fn resolve_thir_named_args(
        &self,
        callee_name: &str,
        args: &[crate::thir::ThirExpr],
        named_args: &[(String, crate::thir::ThirExpr)],
        span: Span,
    ) -> Result<Vec<crate::thir::ThirExpr>, LowerError> {
        let params = self.fn_param_names.get(callee_name).ok_or_else(|| {
            LowerError::Rejected {
                detail: format!(
                    "named arguments are not supported when calling `{}`. They are matched against declared parameter names, which are only known for functions defined in this program -- not builtins, externs or closures. Pass the arguments positionally instead.",
                    callee_name
                ),
                span,
            }
        })?;

        if args.len() > params.len() {
            return Err(LowerError::Rejected {
                detail: format!(
                    "`{}` declares {} parameter(s) but {} positional argument(s) were given before the named ones",
                    callee_name,
                    params.len(),
                    args.len()
                ),
                span,
            });
        }

        let mut slots: Vec<Option<crate::thir::ThirExpr>> = params.iter().map(|_| None).collect();
        for (i, a) in args.iter().enumerate() {
            slots[i] = Some(a.clone());
        }

        for (name, value) in named_args {
            let idx = params.iter().position(|p| p == name).ok_or_else(|| {
                let suggestion = params
                    .iter()
                    .find(|p| {
                        let n = name.as_bytes();
                        let q = p.as_bytes();
                        !n.is_empty() && !q.is_empty() && n[0] == q[0]
                    })
                    .cloned();
                LowerError::Rejected {
                    detail: match suggestion {
                        Some(near) => format!(
                            "`{}` has no parameter named `{}` -- did you mean `{}`? It declares: {}",
                            callee_name, name, near, params.join(", ")
                        ),
                        None => format!(
                            "`{}` has no parameter named `{}`. It declares: {}",
                            callee_name, name, params.join(", ")
                        ),
                    },
                    span: value.span,
                }
            })?;
            if slots[idx].is_some() {
                return Err(LowerError::Rejected {
                    detail: format!(
                        "parameter `{}` of `{}` is supplied more than once",
                        name, callee_name
                    ),
                    span: value.span,
                });
            }
            slots[idx] = Some(value.clone());
        }

        let defaults = self.fn_defaults.get(callee_name);
        let mut out = Vec::with_capacity(params.len());
        for (idx, slot) in slots.into_iter().enumerate() {
            match slot {
                Some(e) => out.push(e),
                None => {
                    let default_expr = defaults
                        .and_then(|d| d.get(idx))
                        .and_then(|d| d.as_ref())
                        .ok_or_else(|| LowerError::Rejected {
                        detail: format!(
                            "parameter `{}` of `{}` has no default and was not supplied",
                            params[idx], callee_name
                        ),
                        span,
                    })?;
                    out.push(crate::thir::ThirExpr {
                        kind: crate::thir::ThirExprKind::Block(crate::thir::ThirBlock {
                            stmts: vec![],
                            tail: None,
                            span: default_expr.span(),
                        }),
                        ty: crate::thir::ThirType::Open,
                        span: default_expr.span(),
                    });
                }
            }
        }
        Ok(out)
    }

    pub(super) fn lower_thir_call(
        &mut self,
        callee: &str,
        args: &[crate::thir::ThirExpr],
        named_args: &[(String, crate::thir::ThirExpr)],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        let callee_name = self.resolve_unqualified_name(callee);

        // 1. Fold named arguments into positional order
        let reordered_thir: Vec<crate::thir::ThirExpr>;
        let args: &[crate::thir::ThirExpr] = if named_args.is_empty() {
            args
        } else {
            reordered_thir = self.resolve_thir_named_args(&callee_name, args, named_args, span)?;
            &reordered_thir
        };

        // 2. Check if user function shadows builtin
        let shadows_builtin = CURRENT_USER_FNS.with(|u| {
            let set = u.borrow();
            set.contains(callee) || set.contains(&callee_name)
        }) || self.generic_fns.contains_key(callee)
            || self.generic_fns.contains_key(&callee_name);

        let dispatch_name: &str = if shadows_builtin { "" } else { callee };

        // 3. Resume continuation call
        if let Some(ref rp_name) = self.resume_param_name {
            let rp_name = rp_name.clone();
            if callee == rp_name && args.len() == 1 {
                let (val, val_ty) = self.lower_thir_expr_req(&args[0])?;
                let cont_val = self.scope.get(&rp_name).map(|(v, _)| *v).ok_or_else(|| {
                    LowerError::rich(
                        "E0900",
                        crate::diagnostics::Diagnostic::error(format!(
                            "internal compiler error: continuation `{}` is not in scope",
                            rp_name
                        ))
                        .with_primary_span(span.start.0, span.end.0, None),
                    )
                })?;
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::ResumeCont {
                        cont: cont_val,
                        value: val,
                        result,
                    },
                    Some(val_ty.clone()),
                );
                return Ok((result, val_ty));
            }
        }

        // 4. Native channel / concurrency built-ins
        if dispatch_name == "task_group" {
            let result = self.builder.fresh_value();
            self.builder
                .push_instr(IrInstr::TaskGroupNew { result }, Some(IrType::TaskGroup));
            return Ok((result, IrType::TaskGroup));
        }
        if dispatch_name == "task_group_join" && !args.is_empty() {
            let (group_val, _) = self.lower_thir_expr_req(&args[0])?;
            self.builder
                .push_instr(IrInstr::TaskGroupJoin { group: group_val }, None);
            return Ok(self.emit_unit_value());
        }
        if dispatch_name == "task_group_cancel" && !args.is_empty() {
            let (group_val, _) = self.lower_thir_expr_req(&args[0])?;
            self.builder
                .push_instr(IrInstr::TaskGroupCancel { group: group_val }, None);
            return Ok(self.emit_unit_value());
        }
        if (dispatch_name == "send" || callee == "chan_send") && args.len() == 2 {
            let (chan_val, _) = self.lower_thir_expr_req(&args[0])?;
            let (val, val_ty) = self.lower_thir_expr_req(&args[1])?;
            self.chan_elem_types
                .entry(chan_val)
                .or_insert_with(|| val_ty.clone());
            self.builder.push_instr(
                IrInstr::ChanSend {
                    chan: chan_val,
                    value: val,
                },
                None,
            );
            return Ok(self.emit_unit_value());
        }
        if dispatch_name == "recv" && args.len() == 1 {
            let (chan_val, chan_ty) = self.lower_thir_expr_req(&args[0])?;
            if let IrType::Task(elem) = chan_ty {
                let elem_ty = *elem;
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::BuiltinCall {
                        result,
                        name: "async_block_on".to_owned(),
                        args: vec![chan_val],
                        result_ty: elem_ty.clone(),
                    },
                    Some(elem_ty.clone()),
                );
                return Ok((result, elem_ty));
            }
            let elem_ty = match (self.chan_elem_types.get(&chan_val), chan_ty) {
                (Some(t), _) => t.clone(),
                (None, IrType::Chan(elem)) => *elem,
                (None, other) => {
                    return Err(LowerError::TypeMismatch {
                        expected: "channel".into(),
                        found: other.to_string(),
                        span,
                    });
                }
            };
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::ChanRecv {
                    result,
                    chan: chan_val,
                    elem_ty: elem_ty.clone(),
                },
                Some(elem_ty.clone()),
            );
            return Ok((result, elem_ty));
        }

        // 5. Generic function call — monomorphize on demand
        if let Some(generic_fn) = self.generic_fns.get(&callee_name).cloned() {
            let mut arg_vals = Vec::with_capacity(args.len());
            let mut arg_tys = Vec::with_capacity(args.len());
            for arg in args {
                let (v, ty) = self.lower_thir_expr_req(arg)?;
                arg_vals.push(v);
                arg_tys.push(ty);
            }

            let expected_ret = self
                .binding_ty
                .clone()
                .or_else(|| self.expected_expr_ty.clone());
            let subs = infer_generic_fn_subs(
                &generic_fn,
                &arg_tys,
                expected_ret.as_ref(),
                self.module,
                &callee_name,
                span,
            )?;
            let mut concrete_ret =
                resolve_ast_type_with_subs(&generic_fn.return_ty, &subs, self.module)?;
            if generic_fn.is_async {
                concrete_ret = IrType::Task(Box::new(concrete_ret));
            }
            if let Some(tmpl) = self.module.generic_templates.get(&callee_name) {
                let mut mono_args =
                    Vec::with_capacity(tmpl.type_params.len() + tmpl.hkt_params.len());
                for tp in &tmpl.type_params {
                    mono_args.push(subs.get(tp).cloned().unwrap_or(IrType::Infer));
                }
                for hp in &tmpl.hkt_params {
                    let ctor_ty = match subs.get(hp) {
                        Some(IrType::Struct { name, .. }) => IrType::TypeParam(name.clone()),
                        Some(other) => other.clone(),
                        None => IrType::TypeParam(hp.clone()),
                    };
                    mono_args.push(ctor_ty);
                }
                let result = self.builder.fresh_value();
                self.builder.push_generic_call(
                    &callee_name,
                    mono_args,
                    IrInstr::Call {
                        result: Some(result),
                        callee: callee_name.clone(),
                        args: arg_vals,
                        result_ty: Some(concrete_ret.clone()),
                    },
                    Some(concrete_ret.clone()),
                );
                return Ok((result, concrete_ret));
            }
            let mangle = mangle_param_list(&generic_fn.type_params, &subs);
            let mangled = format!("{}__{}", callee_name, mangle);
            self.mono_sigs
                .borrow_mut()
                .insert(mangled.clone(), concrete_ret.clone());
            let mono_hit = self.mono_cache.borrow().contains(&mangled);
            crate::lower::mono::record_instantiation(&mangled, mono_hit);
            if !mono_hit {
                self.mono_cache.borrow_mut().insert(mangled.clone());
                let mut mono_fn = generic_fn.clone();
                mono_fn.name.name = mangled.clone();
                mono_fn.type_params = Vec::new();
                let fn_sigs_ref = self.fn_sigs;
                let (ir_func, extra_lifted) = lower_function_with_generics_and_subs(
                    &mono_fn,
                    self.module,
                    fn_sigs_ref,
                    &self.const_defs,
                    self.generic_fns.clone(),
                    self.mono_cache.clone(),
                    self.mono_sigs.clone(),
                    subs,
                    self.trait_dispatch.clone(),
                    self.fn_defaults.clone(),
                    self.fn_param_names.clone(),
                    self.fn_param_types.clone(),
                    self.lambda_counter.clone(),
                )?;
                self.lifted_fns.borrow_mut().push(ir_func);
                self.lifted_fns.borrow_mut().extend(extra_lifted);
            }
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::Call {
                    result: Some(result),
                    callee: mangled,
                    args: arg_vals,
                    result_ty: Some(concrete_ret.clone()),
                },
                Some(concrete_ret.clone()),
            );
            return Ok((result, concrete_ret));
        }

        // 6. Trait method static dispatch
        if let Some(impls) = self.trait_dispatch.get(&callee_name).cloned() {
            if !args.is_empty() {
                let (first_val, first_ty) = self.lower_thir_expr_req(&args[0])?;
                let type_key = ir_type_dispatch_name(&first_ty);
                if let Some((_, mangled)) = impls
                    .iter()
                    .find(|(dispatch_ty, _)| ir_type_dispatch_name(dispatch_ty) == type_key)
                {
                    let mangled = mangled.clone();
                    let ret_ty = self.sig_of(&mangled, span)?;
                    let mut arg_vals = vec![first_val];
                    for arg in &args[1..] {
                        let (v, _) = self.lower_thir_expr_req(arg)?;
                        arg_vals.push(v);
                    }
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::Call {
                            result: Some(result),
                            callee: mangled,
                            args: arg_vals,
                            result_ty: Some(ret_ty.clone()),
                        },
                        Some(ret_ty.clone()),
                    );
                    return Ok((result, ret_ty));
                }
            }
        }

        // 6b. Native assert lowering
        if dispatch_name == "assert" {
            if args.is_empty() || args.len() > 2 {
                return Err(LowerError::rich(
                    "E0109",
                    crate::diagnostics::Diagnostic::error(
                        "`assert` takes a condition and an optional message".to_string(),
                    )
                    .with_primary_span(span.start.0, span.end.0, None),
                ));
            }
            let (cond, _) = self.lower_thir_expr_req(&args[0])?;
            let then_block = self.builder.create_block(Some("assert_ok"));
            let panic_block = self.builder.create_block(Some("assert_fail"));
            let merge_block = self.builder.create_block(Some("assert_merge"));
            self.builder.push_instr(
                IrInstr::CondBr {
                    cond,
                    then_block,
                    then_args: vec![],
                    else_block: panic_block,
                    else_args: vec![],
                },
                None,
            );
            self.builder.set_current_block(panic_block);
            let prefix = self.builder.fresh_value();
            let fixed = match args.get(1) {
                None => Some("assertion failed".to_owned()),
                Some(arg) => match &arg.kind {
                    crate::thir::ThirExprKind::Literal(crate::thir::ThirLiteral::Str(s)) => {
                        Some(format!("assertion failed: {s}"))
                    }
                    _ => None,
                },
            };
            self.builder.push_instr(
                IrInstr::ConstStr {
                    result: prefix,
                    value: fixed.clone().unwrap_or_else(|| "assertion failed: ".into()),
                },
                Some(IrType::Str),
            );
            let msg_val = match args.get(1).filter(|_| fixed.is_none()) {
                Some(extra) => {
                    let (val, ty) = self.lower_thir_expr_req(extra)?;
                    let str_val = if ty == IrType::Str {
                        val
                    } else {
                        let converted = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::ValueToStr {
                                result: converted,
                                operand: val,
                            },
                            Some(IrType::Str),
                        );
                        converted
                    };
                    let full_msg = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::StrConcat {
                            result: full_msg,
                            lhs: prefix,
                            rhs: str_val,
                        },
                        Some(IrType::Str),
                    );
                    full_msg
                }
                None => prefix,
            };
            self.builder.push_instr(
                IrInstr::Panic {
                    msg: msg_val,
                    span_byte: Some(span.start.0),
                },
                None,
            );
            self.builder.set_current_block(then_block);
            self.builder.push_instr(
                IrInstr::Br {
                    target: merge_block,
                    args: vec![],
                },
                None,
            );
            self.builder.set_current_block(merge_block);
            return Ok(self.emit_unit_value());
        }

        // 6c. Route built-ins before fn_sigs checks so they aren't emitted as direct calls
        if !shadows_builtin
            && (typeck::is_builtin_call_name(dispatch_name) || typeck::is_builtin_call_name(callee))
        {
            let callee_ident = Ident {
                name: callee.to_string(),
                span,
            };
            let ast_args: Vec<AstExpr> = args.iter().map(thir_to_ast).collect();
            let ast_named_args: Vec<(String, AstExpr)> = named_args
                .iter()
                .map(|(n, a)| (n.clone(), thir_to_ast(a)))
                .collect();
            return self.lower_call(&callee_ident, &ast_args, &ast_named_args, span);
        }

        // 7. General user function call (if defined in fn_sigs, mono_sigs, extern_fns, or scope)
        let ret_ty = self
            .fn_sigs
            .get(&callee_name)
            .cloned()
            .or_else(|| self.mono_sigs.borrow().get(&callee_name).cloned())
            .or_else(|| {
                self.module
                    .extern_fns
                    .iter()
                    .find(|e| e.name == callee_name)
                    .map(|e| e.ret_ty.clone())
            });

        if let Some(ret_ty) = ret_ty {
            let callee_param_tys: Option<Vec<IrType>> =
                self.fn_param_types.get(&callee_name).cloned();
            if let Some(names) = self.fn_param_names.get(&callee_name) {
                let has_splat = args
                    .iter()
                    .any(|a| matches!(a.kind, crate::thir::ThirExprKind::Splat(_)));
                let required = match self.fn_defaults.get(&callee_name) {
                    Some(defs) => defs.iter().filter(|d| d.is_none()).count(),
                    None => names.len(),
                };
                if !has_splat && (args.len() < required || args.len() > names.len()) {
                    return Err(self.arity_error(
                        &callee_name,
                        callee,
                        required,
                        names.len(),
                        args.len(),
                        span,
                    ));
                }
            }

            let defaults = self.fn_defaults.get(&callee_name).cloned();
            let mut arg_vals = Vec::new();
            for arg in args {
                if let crate::thir::ThirExprKind::Splat(expr) = &arg.kind {
                    let (arr_val, arr_ty) = self.lower_thir_expr_req(expr)?;
                    let (elem_ty, arr_len) = match &arr_ty {
                        IrType::Array { elem, len } => ((**elem).clone(), *len),
                        _ => {
                            return Err(LowerError::rich(
                                "E0110",
                                crate::diagnostics::Diagnostic::error(
                                    "`..` in a call needs an array of known size".to_string(),
                                )
                                .with_primary_span(
                                    arg.span.start.0,
                                    arg.span.end.0,
                                    None,
                                ),
                            ));
                        }
                    };
                    for i in 0..arr_len {
                        let idx_val = self.builder.fresh_value();
                        let idx_ty = IrType::Scalar(DType::I64);
                        self.builder.push_instr(
                            IrInstr::ConstInt {
                                result: idx_val,
                                value: i as i64,
                                ty: idx_ty.clone(),
                            },
                            Some(idx_ty.clone()),
                        );
                        let elem_val = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::ArrayLoad {
                                result: elem_val,
                                array: arr_val,
                                index: idx_val,
                                elem_ty: elem_ty.clone(),
                            },
                            Some(elem_ty.clone()),
                        );
                        arg_vals.push(elem_val);
                    }
                } else {
                    let (v, vty) = self.lower_thir_expr_req(arg)?;
                    let slot = arg_vals.len();
                    if let Some(param_ty) = callee_param_tys.as_ref().and_then(|ts| ts.get(slot)) {
                        if hints::incompatible(&vty, param_ty) {
                            return Err(self
                                .argument_type_error_span(callee, slot, arg.span, &vty, param_ty));
                        }
                    }
                    let expected = callee_param_tys
                        .as_ref()
                        .and_then(|ts| ts.get(slot))
                        .filter(|t| matches!(t, IrType::TraitObject { .. }))
                        .cloned();
                    let v = match expected {
                        Some(exp) => self.coerce_to_trait_object(v, vty, &exp, arg.span)?.0,
                        None => v,
                    };
                    arg_vals.push(v);
                }
            }
            if let Some(ref defs) = defaults {
                for default_expr in defs.iter().skip(arg_vals.len()).flatten() {
                    let (v, _) = self.lower_expr(default_expr)?;
                    arg_vals.push(v);
                }
            }

            let is_extern = self.module.extern_fns.iter().any(|e| e.name == callee_name);
            if is_extern {
                if !crate::lower::types::returns_a_value(&ret_ty) {
                    self.builder.push_instr(
                        IrInstr::CallExtern {
                            result: None,
                            name: callee_name.clone(),
                            args: arg_vals,
                            ret_ty: ret_ty.clone(),
                        },
                        None,
                    );
                    return Ok(self.emit_unit_value());
                }
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::CallExtern {
                        result: Some(result),
                        name: callee_name.clone(),
                        args: arg_vals,
                        ret_ty: ret_ty.clone(),
                    },
                    Some(ret_ty.clone()),
                );
                return Ok((result, ret_ty));
            }

            if arg_vals.iter().any(|v| self.tape_nodes.contains_key(v)) {
                if let Some(inlined) = self.try_lower_taped_call(&callee_name, &arg_vals)? {
                    return Ok(inlined);
                }
            }

            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::Call {
                    result: Some(result),
                    callee: callee_name,
                    args: arg_vals,
                    result_ty: Some(ret_ty.clone()),
                },
                Some(ret_ty.clone()),
            );
            return Ok((result, ret_ty));
        }

        // 8. Closure call from local scope
        if let Some((closure_val, closure_ty)) = self.scope.get(&callee_name).cloned() {
            if let IrType::Fn { ret, .. } = &closure_ty {
                let mut arg_vals = Vec::with_capacity(args.len());
                for a in args {
                    let (v, _) = self.lower_thir_expr_req(a)?;
                    arg_vals.push(v);
                }
                let result = self.builder.fresh_value();
                let ret_ty = (**ret).clone();
                self.builder.push_instr(
                    IrInstr::CallClosure {
                        result: Some(result),
                        closure: closure_val,
                        args: arg_vals,
                        result_ty: ret_ty.clone(),
                        pass_env: true,
                    },
                    Some(ret_ty.clone()),
                );
                return Ok((result, ret_ty));
            }
        }

        // 9. Built-in fallback through lower_call
        let callee_ident = Ident {
            name: callee.to_string(),
            span,
        };
        let ast_args: Vec<AstExpr> = args.iter().map(thir_to_ast).collect();
        let ast_named_args: Vec<(String, AstExpr)> = named_args
            .iter()
            .map(|(n, a)| (n.clone(), thir_to_ast(a)))
            .collect();
        self.lower_call(&callee_ident, &ast_args, &ast_named_args, span)
    }

    pub(super) fn lower_thir_method_call(
        &mut self,
        base: &crate::thir::ThirExpr,
        method: &str,
        args: &[crate::thir::ThirExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        if let crate::thir::ThirExprKind::Value {
            name: base_name, ..
        } = &base.kind
        {
            let resolved_q = resolve_qualifier(base_name);
            let base_resolved_enum = self.resolve_unqualified_name(base_name);
            if !self.scope.contains_key(base_name)
                && self.module.enum_def(&base_resolved_enum).is_none()
                && self.module.struct_def(&base_resolved_enum).is_none()
            {
                let mangled_fn = format!("{}__{}", resolved_q, method);
                if self.generic_fns.contains_key(mangled_fn.as_str()) {
                    return self.lower_thir_call(&mangled_fn, args, &[], span);
                }
                let ret_ty = self
                    .fn_sigs
                    .get(mangled_fn.as_str())
                    .cloned()
                    .or_else(|| self.mono_sigs.borrow().get(mangled_fn.as_str()).cloned());
                if let Some(ret_ty) = ret_ty {
                    let mut arg_vals = Vec::with_capacity(args.len());
                    for arg in args {
                        let (v, _) = self.lower_thir_expr_req(arg)?;
                        arg_vals.push(v);
                    }
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::Call {
                            result: Some(result),
                            callee: mangled_fn,
                            args: arg_vals,
                            result_ty: Some(ret_ty.clone()),
                        },
                        Some(ret_ty.clone()),
                    );
                    return Ok((result, ret_ty));
                }

                let unqualified_mangled_fn = self.resolve_unqualified_name(method);
                if self
                    .generic_fns
                    .contains_key(unqualified_mangled_fn.as_str())
                {
                    return self.lower_thir_call(&unqualified_mangled_fn, args, &[], span);
                }
                if self.fn_sigs.contains_key(unqualified_mangled_fn.as_str())
                    || self
                        .mono_sigs
                        .borrow()
                        .contains_key(unqualified_mangled_fn.as_str())
                {
                    return self.lower_thir_call(&unqualified_mangled_fn, args, &[], span);
                }
            }
            if let Some(variants) = self.module.enum_def(&base_resolved_enum) {
                let variants = variants.clone();
                if let Some(variant_idx) = variants.iter().position(|v| v == method) {
                    let mut field_vals = Vec::with_capacity(args.len());
                    for arg in args {
                        let (v, _) = self.lower_thir_expr_req(arg)?;
                        field_vals.push(v);
                    }
                    let result_ty = IrType::Enum {
                        name: base_resolved_enum,
                        variants,
                    };
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::MakeVariant {
                            result,
                            variant_idx,
                            fields: field_vals,
                            result_ty: result_ty.clone(),
                        },
                        Some(result_ty.clone()),
                    );
                    return Ok((result, result_ty));
                }
            }
        }

        if let Some(enum_name) = get_qualified_enum_name_thir(base) {
            if let Some(variants) = self.module.enum_def(&enum_name) {
                let variants = variants.clone();
                if let Some(variant_idx) = variants.iter().position(|v| v == method) {
                    let mut field_vals = Vec::with_capacity(args.len());
                    for arg in args {
                        let (v, _) = self.lower_thir_expr_req(arg)?;
                        field_vals.push(v);
                    }
                    let result_ty = IrType::Enum {
                        name: enum_name,
                        variants,
                    };
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::MakeVariant {
                            result,
                            variant_idx,
                            fields: field_vals,
                            result_ty: result_ty.clone(),
                        },
                        Some(result_ty.clone()),
                    );
                    return Ok((result, result_ty));
                }
            }
        }

        let (base_val, base_ty) = self.lower_thir_expr_req(base)?;

        if let IrType::List(inner_elem_ty) = &base_ty.clone() {
            let elem_ty = *inner_elem_ty.clone();
            match method {
                "map" => return self.lower_thir_list_map(base_val, elem_ty, args, span),
                "filter" => return self.lower_thir_list_filter(base_val, elem_ty, args, span),
                "fold" => return self.lower_thir_list_fold(base_val, elem_ty, args, span),
                "any" => return self.lower_thir_list_any(base_val, elem_ty, args, span),
                "all" => return self.lower_thir_list_all(base_val, elem_ty, args, span),
                "len" => {
                    let result = self.builder.fresh_value();
                    let ty = IrType::Scalar(DType::I64);
                    self.builder.push_instr(
                        IrInstr::ListLen {
                            result,
                            list: base_val,
                        },
                        Some(ty.clone()),
                    );
                    return Ok((result, ty));
                }
                "push" => {
                    if args.len() != 1 {
                        return Err(builtin_arity_error(
                            "push() requires exactly 1 argument",
                            args.len(),
                            span,
                        ));
                    }
                    let (v, _) = self.lower_thir_expr_req(&args[0])?;
                    self.builder.push_instr(
                        IrInstr::ListPush {
                            list: base_val,
                            value: v,
                        },
                        None,
                    );
                    let zero = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::ConstInt {
                            result: zero,
                            value: 0,
                            ty: IrType::Scalar(DType::I64),
                        },
                        Some(IrType::Scalar(DType::I64)),
                    );
                    return Ok((zero, IrType::Scalar(DType::I64)));
                }
                "pop" => {
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::ListPop {
                            result,
                            list: base_val,
                            elem_ty: elem_ty.clone(),
                        },
                        Some(elem_ty.clone()),
                    );
                    return Ok((result, elem_ty));
                }
                "get" => {
                    if args.len() != 1 {
                        return Err(builtin_arity_error(
                            "get() requires exactly 1 argument",
                            args.len(),
                            span,
                        ));
                    }
                    let (idx, _) = self.lower_thir_expr_req(&args[0])?;
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::ListGet {
                            result,
                            list: base_val,
                            index: idx,
                            elem_ty: elem_ty.clone(),
                        },
                        Some(elem_ty.clone()),
                    );
                    return Ok((result, elem_ty));
                }
                "set" => {
                    if args.len() != 2 {
                        return Err(builtin_arity_error(
                            "set() requires exactly 2 arguments",
                            args.len(),
                            span,
                        ));
                    }
                    let (idx, _) = self.lower_thir_expr_req(&args[0])?;
                    let (v, _) = self.lower_thir_expr_req(&args[1])?;
                    self.builder.push_instr(
                        IrInstr::ListSet {
                            list: base_val,
                            index: idx,
                            value: v,
                        },
                        None,
                    );
                    let zero = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::ConstInt {
                            result: zero,
                            value: 0,
                            ty: IrType::Scalar(DType::I64),
                        },
                        Some(IrType::Scalar(DType::I64)),
                    );
                    return Ok((zero, IrType::Scalar(DType::I64)));
                }
                _ => {}
            }
        }

        if let IrType::Option(inner_ty) = &base_ty {
            let inner = (**inner_ty).clone();
            match method {
                "is_some" => {
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::IsSome {
                            result,
                            operand: base_val,
                        },
                        Some(IrType::Scalar(DType::Bool)),
                    );
                    return Ok((result, IrType::Scalar(DType::Bool)));
                }
                "is_none" => {
                    let is_some_result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::IsSome {
                            result: is_some_result,
                            operand: base_val,
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
                    return Ok((result, IrType::Scalar(DType::Bool)));
                }
                "unwrap" => {
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::OptionUnwrap {
                            result,
                            operand: base_val,
                            result_ty: inner.clone(),
                        },
                        Some(inner.clone()),
                    );
                    return Ok((result, inner));
                }
                "unwrap_or" => {
                    if args.len() != 1 {
                        return Err(builtin_arity_error(
                            "unwrap_or() requires exactly 1 argument",
                            args.len(),
                            span,
                        ));
                    }
                    let (default_val, _default_ty) = self.lower_thir_expr_req(&args[0])?;
                    let is_some_result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::IsSome {
                            result: is_some_result,
                            operand: base_val,
                        },
                        Some(IrType::Scalar(DType::Bool)),
                    );
                    let then_bb = self.builder.create_block(Some("unwrap_or_then"));
                    let else_bb = self.builder.create_block(Some("unwrap_or_else"));
                    let merge_bb = self.builder.create_block(Some("unwrap_or_merge"));
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
                            operand: base_val,
                            result_ty: inner.clone(),
                        },
                        Some(inner.clone()),
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
                        Some("unwrap_or_res"),
                        inner.clone(),
                    );
                    self.builder.set_current_block(merge_bb);
                    return Ok((merge_result, inner));
                }
                "map" => {
                    if args.len() != 1 {
                        return Err(builtin_arity_error(
                            "map() requires exactly 1 argument",
                            args.len(),
                            span,
                        ));
                    }
                    let (f_val, f_ty) = self.lower_thir_expr_req(&args[0])?;
                    let ret_ty = match &f_ty {
                        IrType::Fn { ret, .. } => (**ret).clone(),
                        other => {
                            return Err(LowerError::TypeMismatch {
                                expected: "a function".into(),
                                found: other.to_string(),
                                span,
                            })
                        }
                    };
                    let is_some_result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::IsSome {
                            result: is_some_result,
                            operand: base_val,
                        },
                        Some(IrType::Scalar(DType::Bool)),
                    );
                    let then_bb = self.builder.create_block(Some("opt_map_then"));
                    let else_bb = self.builder.create_block(Some("opt_map_else"));
                    let merge_bb = self.builder.create_block(Some("opt_map_merge"));
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
                            operand: base_val,
                            result_ty: inner.clone(),
                        },
                        Some(inner),
                    );
                    let call_result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::CallClosure {
                            result: Some(call_result),
                            closure: f_val,
                            args: vec![unwrapped],
                            result_ty: ret_ty.clone(),
                            pass_env: true,
                        },
                        Some(ret_ty.clone()),
                    );
                    let some_result = self.builder.fresh_value();
                    let some_ty = IrType::Option(Box::new(ret_ty.clone()));
                    self.builder.push_instr(
                        IrInstr::MakeSome {
                            result: some_result,
                            value: call_result,
                            result_ty: some_ty.clone(),
                        },
                        Some(some_ty),
                    );
                    self.builder.push_instr(
                        IrInstr::Br {
                            target: merge_bb,
                            args: vec![some_result],
                        },
                        None,
                    );
                    self.builder.set_current_block(else_bb);
                    let none_result = self.builder.fresh_value();
                    let none_ty = IrType::Option(Box::new(ret_ty));
                    self.builder.push_instr(
                        IrInstr::MakeNone {
                            result: none_result,
                            result_ty: none_ty.clone(),
                        },
                        Some(none_ty.clone()),
                    );
                    self.builder.push_instr(
                        IrInstr::Br {
                            target: merge_bb,
                            args: vec![none_result],
                        },
                        None,
                    );
                    let merge_result = self.builder.add_block_param(
                        merge_bb,
                        Some("opt_map_res"),
                        none_ty.clone(),
                    );
                    self.builder.set_current_block(merge_bb);
                    return Ok((merge_result, none_ty));
                }
                _ => {}
            }
        }

        if let IrType::ResultType(ok_ty, err_ty) = &base_ty {
            let ok = (**ok_ty).clone();
            let err = (**err_ty).clone();
            match method {
                "is_ok" => {
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::IsOk {
                            result,
                            operand: base_val,
                        },
                        Some(IrType::Scalar(DType::Bool)),
                    );
                    return Ok((result, IrType::Scalar(DType::Bool)));
                }
                "is_err" => {
                    let is_ok_result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::IsOk {
                            result: is_ok_result,
                            operand: base_val,
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
                    return Ok((result, IrType::Scalar(DType::Bool)));
                }
                "unwrap" => {
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::ResultUnwrap {
                            result,
                            operand: base_val,
                            result_ty: ok.clone(),
                        },
                        Some(ok.clone()),
                    );
                    return Ok((result, ok));
                }
                "unwrap_err" => {
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::ResultUnwrapErr {
                            result,
                            operand: base_val,
                            result_ty: err.clone(),
                        },
                        Some(err.clone()),
                    );
                    return Ok((result, err));
                }
                "unwrap_or" => {
                    if args.len() != 1 {
                        return Err(builtin_arity_error(
                            "unwrap_or() requires exactly 1 argument",
                            args.len(),
                            span,
                        ));
                    }
                    let (default_val, _) = self.lower_thir_expr_req(&args[0])?;
                    let is_ok_result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::IsOk {
                            result: is_ok_result,
                            operand: base_val,
                        },
                        Some(IrType::Scalar(DType::Bool)),
                    );
                    let then_bb = self.builder.create_block(Some("res_unwrap_or_then"));
                    let else_bb = self.builder.create_block(Some("res_unwrap_or_else"));
                    let merge_bb = self.builder.create_block(Some("res_unwrap_or_merge"));
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
                            operand: base_val,
                            result_ty: ok.clone(),
                        },
                        Some(ok.clone()),
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
                        Some("res_unwrap_or_res"),
                        ok.clone(),
                    );
                    self.builder.set_current_block(merge_bb);
                    return Ok((merge_result, ok));
                }
                _ => {}
            }
        }

        if matches!(base_ty, IrType::Str) {
            match method {
                "len" => {
                    let result = self.builder.fresh_value();
                    let ty = IrType::Scalar(DType::I64);
                    self.builder.push_instr(
                        IrInstr::StrLen {
                            result,
                            operand: base_val,
                        },
                        Some(ty.clone()),
                    );
                    return Ok((result, ty));
                }
                "to_upper" => {
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::StrToUpper {
                            result,
                            operand: base_val,
                        },
                        Some(IrType::Str),
                    );
                    return Ok((result, IrType::Str));
                }
                "to_lower" => {
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::StrToLower {
                            result,
                            operand: base_val,
                        },
                        Some(IrType::Str),
                    );
                    return Ok((result, IrType::Str));
                }
                "trim" => {
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::StrTrim {
                            result,
                            operand: base_val,
                        },
                        Some(IrType::Str),
                    );
                    return Ok((result, IrType::Str));
                }
                "contains" => {
                    if args.len() != 1 {
                        return Err(builtin_arity_error(
                            "contains() requires exactly 1 argument",
                            args.len(),
                            span,
                        ));
                    }
                    let (needle, _) = self.lower_thir_expr_req(&args[0])?;
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::StrContains {
                            result,
                            haystack: base_val,
                            needle,
                        },
                        Some(IrType::Scalar(DType::Bool)),
                    );
                    return Ok((result, IrType::Scalar(DType::Bool)));
                }
                "starts_with" => {
                    if args.len() != 1 {
                        return Err(builtin_arity_error(
                            "starts_with() requires exactly 1 argument",
                            args.len(),
                            span,
                        ));
                    }
                    let (prefix, _) = self.lower_thir_expr_req(&args[0])?;
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::StrStartsWith {
                            result,
                            haystack: base_val,
                            prefix,
                        },
                        Some(IrType::Scalar(DType::Bool)),
                    );
                    return Ok((result, IrType::Scalar(DType::Bool)));
                }
                "ends_with" => {
                    if args.len() != 1 {
                        return Err(builtin_arity_error(
                            "ends_with() requires exactly 1 argument",
                            args.len(),
                            span,
                        ));
                    }
                    let (suffix, _) = self.lower_thir_expr_req(&args[0])?;
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::StrEndsWith {
                            result,
                            haystack: base_val,
                            suffix,
                        },
                        Some(IrType::Scalar(DType::Bool)),
                    );
                    return Ok((result, IrType::Scalar(DType::Bool)));
                }
                "index" => {
                    if args.len() != 1 {
                        return Err(builtin_arity_error(
                            "index() requires exactly 1 argument",
                            args.len(),
                            span,
                        ));
                    }
                    let (idx, _) = self.lower_thir_expr_req(&args[0])?;
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::StrIndex {
                            result,
                            string: base_val,
                            index: idx,
                        },
                        Some(IrType::Scalar(DType::I64)),
                    );
                    return Ok((result, IrType::Scalar(DType::I64)));
                }
                "slice" => {
                    if args.len() != 2 {
                        return Err(builtin_arity_error(
                            "slice() requires exactly 2 arguments",
                            args.len(),
                            span,
                        ));
                    }
                    let (start, _) = self.lower_thir_expr_req(&args[0])?;
                    let (end, _) = self.lower_thir_expr_req(&args[1])?;
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::StrSlice {
                            result,
                            string: base_val,
                            start,
                            end,
                        },
                        Some(IrType::Str),
                    );
                    return Ok((result, IrType::Str));
                }
                "find" => {
                    if args.len() != 1 {
                        return Err(builtin_arity_error(
                            "find() requires exactly 1 argument",
                            args.len(),
                            span,
                        ));
                    }
                    let (needle, _) = self.lower_thir_expr_req(&args[0])?;
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::StrFind {
                            result,
                            haystack: base_val,
                            needle,
                        },
                        Some(IrType::Option(Box::new(IrType::Scalar(DType::I64)))),
                    );
                    return Ok((result, IrType::Option(Box::new(IrType::Scalar(DType::I64)))));
                }
                "replace" => {
                    if args.len() != 2 {
                        return Err(builtin_arity_error(
                            "replace() requires exactly 2 arguments",
                            args.len(),
                            span,
                        ));
                    }
                    let (old_s, _) = self.lower_thir_expr_req(&args[0])?;
                    let (new_s, _) = self.lower_thir_expr_req(&args[1])?;
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::StrReplace {
                            result,
                            string: base_val,
                            from: old_s,
                            to: new_s,
                        },
                        Some(IrType::Str),
                    );
                    return Ok((result, IrType::Str));
                }
                "is_empty" => {
                    let len_result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::StrLen {
                            result: len_result,
                            operand: base_val,
                        },
                        Some(IrType::Scalar(DType::I64)),
                    );
                    let zero = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::ConstInt {
                            result: zero,
                            value: 0,
                            ty: IrType::Scalar(DType::I64),
                        },
                        Some(IrType::Scalar(DType::I64)),
                    );
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::BinOp {
                            result,
                            op: BinOp::CmpEq,
                            lhs: len_result,
                            rhs: zero,
                            ty: IrType::Scalar(DType::I64),
                        },
                        Some(IrType::Scalar(DType::Bool)),
                    );
                    return Ok((result, IrType::Scalar(DType::Bool)));
                }
                "split" => {
                    if args.len() != 1 {
                        return Err(builtin_arity_error(
                            "split() requires exactly 1 argument",
                            args.len(),
                            span,
                        ));
                    }
                    let (delim, _) = self.lower_thir_expr_req(&args[0])?;
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::StrSplit {
                            result,
                            str_val: base_val,
                            delim,
                        },
                        Some(IrType::List(Box::new(IrType::Str))),
                    );
                    return Ok((result, IrType::List(Box::new(IrType::Str))));
                }
                "repeat" => {
                    if args.len() != 1 {
                        return Err(builtin_arity_error(
                            "repeat() requires exactly 1 argument",
                            args.len(),
                            span,
                        ));
                    }
                    let (count, _) = self.lower_thir_expr_req(&args[0])?;
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::StrRepeat {
                            result,
                            operand: base_val,
                            count,
                        },
                        Some(IrType::Str),
                    );
                    return Ok((result, IrType::Str));
                }
                _ => {}
            }
        }

        if let IrType::Scalar(_dt) = &base_ty {
            if method == "to_str" && args.is_empty() {
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::ValueToStr {
                        result,
                        operand: base_val,
                    },
                    Some(IrType::Str),
                );
                return Ok((result, IrType::Str));
            }
        }

        if let IrType::Map(_k_ty, v_ty) = &base_ty {
            let v = (**v_ty).clone();
            match method {
                "len" => {
                    let result = self.builder.fresh_value();
                    let ty = IrType::Scalar(DType::I64);
                    self.builder.push_instr(
                        IrInstr::MapLen {
                            result,
                            map: base_val,
                        },
                        Some(ty.clone()),
                    );
                    return Ok((result, ty));
                }
                "get" => {
                    if args.len() != 1 {
                        return Err(builtin_arity_error(
                            "get() requires exactly 1 argument",
                            args.len(),
                            span,
                        ));
                    }
                    let (k, _) = self.lower_thir_expr_req(&args[0])?;
                    let result = self.builder.fresh_value();
                    let ret_ty = IrType::Option(Box::new(v.clone()));
                    self.builder.push_instr(
                        IrInstr::MapGet {
                            result,
                            map: base_val,
                            key: k,
                            val_ty: v,
                        },
                        Some(ret_ty.clone()),
                    );
                    return Ok((result, ret_ty));
                }
                "set" => {
                    if args.len() != 2 {
                        return Err(builtin_arity_error(
                            "set() requires exactly 2 arguments",
                            args.len(),
                            span,
                        ));
                    }
                    let (k, _) = self.lower_thir_expr_req(&args[0])?;
                    let (v_val, _) = self.lower_thir_expr_req(&args[1])?;
                    self.builder.push_instr(
                        IrInstr::MapSet {
                            map: base_val,
                            key: k,
                            value: v_val,
                        },
                        None,
                    );
                    let zero = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::ConstInt {
                            result: zero,
                            value: 0,
                            ty: IrType::Scalar(DType::I64),
                        },
                        Some(IrType::Scalar(DType::I64)),
                    );
                    return Ok((zero, IrType::Scalar(DType::I64)));
                }
                "contains" => {
                    if args.len() != 1 {
                        return Err(builtin_arity_error(
                            "contains() requires exactly 1 argument",
                            args.len(),
                            span,
                        ));
                    }
                    let (k, _) = self.lower_thir_expr_req(&args[0])?;
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::MapContains {
                            result,
                            map: base_val,
                            key: k,
                        },
                        Some(IrType::Scalar(DType::Bool)),
                    );
                    return Ok((result, IrType::Scalar(DType::Bool)));
                }
                _ => {}
            }
        }

        if let IrType::Chan(inner) = &base_ty {
            let elem = (**inner).clone();
            match method {
                "send" => {
                    if args.len() != 1 {
                        return Err(builtin_arity_error(
                            "send() requires exactly 1 argument",
                            args.len(),
                            span,
                        ));
                    }
                    let (v, v_ty) = self.lower_thir_expr_req(&args[0])?;
                    self.chan_elem_types
                        .entry(base_val)
                        .or_insert_with(|| v_ty.clone());
                    self.builder.push_instr(
                        IrInstr::ChanSend {
                            chan: base_val,
                            value: v,
                        },
                        None,
                    );
                    let zero = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::ConstInt {
                            result: zero,
                            value: 0,
                            ty: IrType::Scalar(DType::I64),
                        },
                        Some(IrType::Scalar(DType::I64)),
                    );
                    return Ok((zero, IrType::Scalar(DType::I64)));
                }
                "recv" => {
                    let elem_ty = self
                        .chan_elem_types
                        .get(&base_val)
                        .cloned()
                        .unwrap_or_else(|| elem.clone());
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::ChanRecv {
                            result,
                            chan: base_val,
                            elem_ty: elem_ty.clone(),
                        },
                        Some(elem_ty.clone()),
                    );
                    return Ok((result, elem_ty));
                }
                _ => {}
            }
        }

        if self.fn_sigs.contains_key(method) || self.generic_fns.contains_key(method) {
            let mut call_args = Vec::with_capacity(args.len() + 1);
            call_args.push(base.clone());
            call_args.extend(args.iter().cloned());
            return self.lower_thir_call(method, &call_args, &[], span);
        }

        if let IrType::TraitObject {
            name: trait_name,
            methods: trait_methods,
        } = &base_ty
        {
            let method_sig = trait_methods
                .iter()
                .find(|m| m.name == *method)
                .or_else(|| {
                    self.module
                        .trait_def(trait_name)
                        .and_then(|methods| methods.iter().find(|m| m.name == *method))
                })
                .cloned()
                .ok_or_else(|| {
                    let uncallable = DYN_UNCALLABLE
                        .with(|d| d.borrow().contains(&(trait_name.clone(), method.to_string())));
                    let message = if uncallable {
                        format!(
                            "`{method}` of `{trait_name}` uses the trait's associated type, which only an impl knows, so it cannot be called through `dyn {trait_name}`"
                        )
                    } else {
                        format!("trait `{trait_name}` has no method `{method}`")
                    };
                    LowerError::rich(
                        "E0117",
                        crate::diagnostics::Diagnostic::error(message)
                            .with_primary_span(span.start.0, span.end.0, None),
                    )
                })?;
            let mut arg_vals = Vec::with_capacity(args.len());
            for arg in args {
                let (v, _) = self.lower_thir_expr_req(arg)?;
                arg_vals.push(v);
            }
            let ret_ty = *method_sig.ret.clone();
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::DynCall {
                    result,
                    obj: base_val,
                    method_name: method.to_string(),
                    args: arg_vals,
                    result_ty: ret_ty.clone(),
                },
                Some(ret_ty.clone()),
            );
            return Ok((result, ret_ty));
        }

        if !matches!(base_ty, IrType::Struct { .. }) {
            if let Some(cands) = self.trait_dispatch.get(method) {
                let exact = cands.iter().find(|(recv_ty, _)| recv_ty == &base_ty);
                let chosen = match exact {
                    Some(hit) => Some(hit),
                    None => {
                        let same_ctor: Vec<&(IrType, String)> = cands
                            .iter()
                            .filter(|(recv_ty, _)| same_constructor(recv_ty, &base_ty))
                            .collect();
                        if same_ctor.len() == 1 {
                            Some(same_ctor[0])
                        } else {
                            None
                        }
                    }
                };
                if let Some((_, fname)) = chosen {
                    let fname = fname.clone();
                    let mut arg_vals = Vec::with_capacity(args.len() + 1);
                    arg_vals.push(base_val);
                    for arg in args {
                        let (v, _) = self.lower_thir_expr_req(arg)?;
                        arg_vals.push(v);
                    }
                    let ret_ty = self.sig_of(&fname, span)?;
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::Call {
                            result: Some(result),
                            callee: fname,
                            args: arg_vals,
                            result_ty: Some(ret_ty.clone()),
                        },
                        Some(ret_ty.clone()),
                    );
                    return Ok((result, ret_ty));
                }
            }
        }

        let type_name = match &base_ty {
            IrType::Struct { name, .. } => name.clone(),
            IrType::TypeParam(tp_name) => {
                let mut arg_vals = Vec::with_capacity(args.len() + 1);
                arg_vals.push(base_val);
                for arg in args {
                    let (v, _) = self.lower_thir_expr_req(arg)?;
                    arg_vals.push(v);
                }
                let ret_ty = self
                    .trait_dispatch
                    .get(method)
                    .and_then(|cands| cands.first())
                    .and_then(|(_, fname)| self.fn_sigs.get(fname).cloned())
                    .unwrap_or(IrType::Str);
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::Call {
                        result: Some(result),
                        callee: format!("__trait_method__{tp_name}____{method}"),
                        args: arg_vals,
                        result_ty: Some(ret_ty.clone()),
                    },
                    Some(ret_ty.clone()),
                );
                return Ok((result, ret_ty));
            }
            other => {
                return Err(method_on_builtin_type_thir(other, method, base, args, span));
            }
        };

        let struct_mangled = format!("{}__{}", type_name, method);
        if !self.fn_sigs.contains_key(&struct_mangled) {
            if let Some(fields) = self.module.struct_def(&type_name) {
                if let Some((_, field_ty)) = fields.iter().find(|(name, _)| name == method) {
                    if let IrType::Fn { params: _, ret } = field_ty {
                        let field_val = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::GetField {
                                result: field_val,
                                base: base_val,
                                field_index: fields.iter().position(|(n, _)| n == method).unwrap(),
                                result_ty: field_ty.clone(),
                            },
                            Some(field_ty.clone()),
                        );
                        let mut arg_vals = Vec::with_capacity(args.len());
                        for arg in args {
                            let (v, _) = self.lower_thir_expr_req(arg)?;
                            arg_vals.push(v);
                        }
                        let ret_ty = *ret.clone();
                        let result = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::CallClosure {
                                result: Some(result),
                                closure: field_val,
                                args: arg_vals,
                                result_ty: ret_ty.clone(),
                                pass_env: false,
                            },
                            Some(ret_ty.clone()),
                        );
                        return Ok((result, ret_ty));
                    }
                }
            }
        }

        let mangled = if self.fn_sigs.contains_key(&struct_mangled) {
            struct_mangled
        } else if let Some(impls) = self.trait_dispatch.get(method) {
            let dispatch_ty = IrType::Struct {
                name: type_name.clone(),
                fields: Vec::new(),
            };
            impls
                .iter()
                .find(|(ty, _)| {
                    if let (IrType::Struct { name: n1, .. }, IrType::Struct { name: n2, .. }) =
                        (ty, &dispatch_ty)
                    {
                        n1 == n2
                    } else {
                        ty == &dispatch_ty
                    }
                })
                .map(|(_, name)| name.clone())
                .unwrap_or(struct_mangled)
        } else {
            struct_mangled
        };

        let ret_ty = self.sig_of(&mangled, span)?;

        let mut arg_vals = vec![base_val];
        for arg in args {
            let (v, _) = self.lower_thir_expr_req(arg)?;
            arg_vals.push(v);
        }

        let result = self.builder.fresh_value();
        let call_instr = IrInstr::Call {
            result: Some(result),
            callee: mangled.clone(),
            args: arg_vals,
            result_ty: Some(ret_ty.clone()),
        };
        if self.module.generic_templates.contains_key(&mangled) {
            self.builder.push_generic_call(
                &mangled,
                vec![base_ty.clone()],
                call_instr,
                Some(ret_ty.clone()),
            );
        } else {
            self.builder.push_instr(call_instr, Some(ret_ty.clone()));
        }
        Ok((result, ret_ty))
    }

    /// Resolve named arguments `f(w = 3, h = 4)` into positional order.
    ///
    /// Named arguments were parsed into `AstExpr::Call::named_args` but never
    /// read by the lowerer, so the call was lowered with only its *positional*
    /// arguments -- usually none at all. The callee then read whatever happened
    /// to be in the argument registers. No stage produced a diagnostic:
    /// `tests/test_named_args.iris` printed "All named arg tests passed!" while
    /// every value it computed was wrong. See known-issues #1.
    ///
    /// Only reachable when `named_args` is non-empty, so a purely positional
    /// call takes exactly the path it always did.
    pub(super) fn resolve_named_args(
        &self,
        callee_name: &str,
        args: &[AstExpr],
        named_args: &[(String, AstExpr)],
        span: Span,
    ) -> Result<Vec<AstExpr>, LowerError> {
        let params = self
            .fn_param_names
            .get(callee_name)
            .ok_or_else(|| LowerError::Rejected {
                detail: format!(
                    "named arguments are not supported when calling `{}`. They are matched \
                     against declared parameter names, which are only known for functions \
                     defined in this program -- not builtins, externs or closures. Pass the \
                     arguments positionally instead.",
                    callee_name
                ),
                span,
            })?;

        if args.len() > params.len() {
            return Err(LowerError::Rejected {
                detail: format!(
                    "`{}` declares {} parameter(s) but {} positional argument(s) were given \
                     before the named ones",
                    callee_name,
                    params.len(),
                    args.len()
                ),
                span,
            });
        }

        // Positional arguments fill the leading slots; named ones fill by name.
        let mut slots: Vec<Option<AstExpr>> = params.iter().map(|_| None).collect();
        for (i, a) in args.iter().enumerate() {
            slots[i] = Some(a.clone());
        }

        for (name, value) in named_args {
            let idx = params.iter().position(|p| p == name).ok_or_else(|| {
                // Nearest declared parameter by shared prefix, for a "did you mean".
                let suggestion = params
                    .iter()
                    .find(|p| {
                        let n = name.as_bytes();
                        let q = p.as_bytes();
                        !n.is_empty() && !q.is_empty() && n[0] == q[0]
                    })
                    .cloned();
                LowerError::Rejected {
                    detail: match suggestion {
                        Some(near) => format!(
                            "`{}` has no parameter named `{}` -- did you mean `{}`? It declares: {}",
                            callee_name,
                            name,
                            near,
                            params.join(", ")
                        ),
                        None => format!(
                            "`{}` has no parameter named `{}`. It declares: {}",
                            callee_name,
                            name,
                            params.join(", ")
                        ),
                    },
                    span: value.span(),
                }
            })?;
            if slots[idx].is_some() {
                return Err(LowerError::Rejected {
                    detail: format!(
                        "parameter `{}` of `{}` is supplied more than once",
                        name, callee_name
                    ),
                    span: value.span(),
                });
            }
            slots[idx] = Some(value.clone());
        }

        // Fill any gap from that parameter's default. A gap with no default is
        // an error here rather than a silently short argument list.
        let defaults = self.fn_defaults.get(callee_name);
        let mut out = Vec::with_capacity(slots.len());
        for (i, slot) in slots.into_iter().enumerate() {
            match slot {
                Some(e) => out.push(e),
                None => match defaults.and_then(|d| d.get(i)).and_then(|d| d.clone()) {
                    Some(d) => out.push(d),
                    None => {
                        return Err(LowerError::Rejected {
                            detail: format!(
                                "parameter `{}` of `{}` has no argument and no default value",
                                params[i], callee_name
                            ),
                            span,
                        });
                    }
                },
            }
        }
        Ok(out)
    }

    /// Lowers a function call. Handles the built-in `einsum` intrinsic specially.
    pub(super) fn lower_call(
        &mut self,
        callee: &Ident,
        args: &[AstExpr],
        named_args: &[(String, AstExpr)],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        let callee_name = self.resolve_unqualified_name(&callee.name);

        // Named arguments are folded into positional order before anything
        // else inspects `args`, so every path below sees a normal call.
        let reordered: Vec<AstExpr>;
        let args: &[AstExpr] = if named_args.is_empty() {
            args
        } else {
            reordered = self.resolve_named_args(&callee_name, args, named_args, span)?;
            &reordered
        };

        // A user (or stdlib) definition of this name shadows the builtin.
        //
        // The builtin used to win unconditionally and silently:
        //
        //     def band(a: i64, b: i64) -> i64 { 999 }
        //     band(12, 10)   ->  8      // the builtin ran; the user's fn never did
        //
        // No error, no warning. 203 builtin names are effectively reserved this
        // way — among them len, min, max, map, filter, get, set, push, pop —
        // and none is a keyword, so nothing stops a program choosing one. If the
        // arity differs the failure is at least loud, but it names the builtin
        // the programmer never intended to call.
        //
        // Thirteen *stdlib* functions collide with a builtin too, so their IRIS
        // implementations were dead code as well.
        //
        // Blanking the dispatch name routes the call to the general
        // user-function path, which is the one the programmer wrote.
        // See known-issues #15.
        let shadows_builtin = CURRENT_USER_FNS.with(|u| {
            let set = u.borrow();
            set.contains(&callee.name) || set.contains(&callee_name)
        }) || self.generic_fns.contains_key(&callee.name)
            || self.generic_fns.contains_key(&callee_name);
        let dispatch_name: &str = if shadows_builtin {
            ""
        } else {
            callee.name.as_str()
        };

        // Built-in: resume continuation call.
        // When lowering inside a handler arm with `has_resume`, a call to the
        // resume_param name (e.g. `v(x)`) lowers to a `ResumeCont` instruction
        // that signals the parent interpreter to return `x` from the effect
        // perform site.
        if let Some(ref rp_name) = self.resume_param_name {
            let rp_name = rp_name.clone();
            if callee.name == rp_name && args.len() == 1 {
                let (val, val_ty) = self.lower_expr(&args[0])?;
                let cont_val = self.scope.get(&rp_name).map(|(v, _)| *v).ok_or_else(|| {
                    LowerError::rich(
                        "E0900",
                        crate::diagnostics::Diagnostic::error(format!(
                            "internal compiler error: continuation `{}` is not in scope",
                            rp_name
                        ))
                        .with_primary_span(
                            (span).start.0,
                            (span).end.0,
                            None,
                        ),
                    )
                })?;
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::ResumeCont {
                        cont: cont_val,
                        value: val,
                        result,
                    },
                    Some(val_ty.clone()),
                );
                return Ok((result, val_ty));
            }
        }

        // Built-in: println(x) / print(x) → Print instruction
        if dispatch_name == "println" || callee.name == "print" || callee.name == "eprintln" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    &format!("{}() requires exactly 1 argument", callee.name),
                    args.len(),
                    span,
                ));
            }
            let (operand, _) = self.lower_expr(&args[0])?;
            self.builder.push_instr(IrInstr::Print { operand }, None);
            // Return a dummy i64 0 as the "unit" value.
            let dummy = self.builder.fresh_value();
            let dummy_ty = IrType::Scalar(DType::I64);
            self.builder.push_instr(
                IrInstr::ConstInt {
                    result: dummy,
                    value: 0,
                    ty: dummy_ty.clone(),
                },
                Some(dummy_ty.clone()),
            );
            return Ok((dummy, dummy_ty));
        }

        // Built-in: task_group() → TaskGroupNew
        if dispatch_name == "task_group" {
            let result = self.builder.fresh_value();
            self.builder
                .push_instr(IrInstr::TaskGroupNew { result }, Some(IrType::TaskGroup));
            return Ok((result, IrType::TaskGroup));
        }

        // Built-in: task_group_join(tg) → TaskGroupJoin
        if dispatch_name == "task_group_join" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "task_group_join() requires exactly 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (group_val, _) = self.lower_expr(&args[0])?;
            self.builder
                .push_instr(IrInstr::TaskGroupJoin { group: group_val }, None);
            return Ok(self.emit_unit_value());
        }

        // Built-in: task_group_cancel(tg) → TaskGroupCancel
        if dispatch_name == "task_group_cancel" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "task_group_cancel() requires exactly 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (group_val, _) = self.lower_expr(&args[0])?;
            self.builder
                .push_instr(IrInstr::TaskGroupCancel { group: group_val }, None);
            return Ok(self.emit_unit_value());
        }

        // Built-in: channel() → ChanNew
        if dispatch_name == "channel" {
            let elem_ty = match &self.binding_ty {
                Some(IrType::Chan(inner)) => (**inner).clone(),
                _ => self
                    .typeck_hint(span)
                    .and_then(|h| h.part(0))
                    .ok_or_else(|| self.undetermined(span, "what this channel carries"))?,
            };
            let chan_ty = IrType::Chan(Box::new(elem_ty.clone()));
            let capacity_val = if args.len() == 1 {
                self.lower_expr(&args[0])?.0
            } else {
                let dummy = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::ConstInt {
                        result: dummy,
                        value: -1,
                        ty: IrType::Scalar(DType::I64),
                    },
                    Some(IrType::Scalar(DType::I64)),
                );
                dummy
            };
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::ChanNew {
                    result,
                    elem_ty,
                    capacity: capacity_val,
                },
                Some(chan_ty.clone()),
            );
            return Ok((result, chan_ty));
        }

        // Built-in: send(ch, v) / chan_send(ch, v) → ChanSend (returns unit, use dummy i64 0)
        if dispatch_name == "send" || callee.name == "chan_send" {
            if args.len() != 2 {
                return Err(builtin_arity_error(
                    "send() requires exactly 2 arguments (channel, value)",
                    args.len(),
                    span,
                ));
            }
            let (chan_val, _) = self.lower_expr(&args[0])?;
            let (val, val_ty) = self.lower_expr(&args[1])?;
            // Record the concrete element type so recv() can use it.
            self.chan_elem_types
                .entry(chan_val)
                .or_insert_with(|| val_ty.clone());
            self.builder.push_instr(
                IrInstr::ChanSend {
                    chan: chan_val,
                    value: val,
                },
                None,
            );
            // Return a dummy i64 0 as the "unit" value.
            let dummy = self.builder.fresh_value();
            let dummy_ty = IrType::Scalar(DType::I64);
            self.builder.push_instr(
                IrInstr::ConstInt {
                    result: dummy,
                    value: 0,
                    ty: dummy_ty.clone(),
                },
                Some(dummy_ty.clone()),
            );
            return Ok((dummy, dummy_ty));
        }

        // Built-in: recv(ch) → ChanRecv
        if dispatch_name == "recv" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "recv() requires exactly 1 argument (channel)",
                    args.len(),
                    span,
                ));
            }
            let (chan_val, chan_ty) = self.lower_expr(&args[0])?;
            // `recv` on a task waits for its result, so code written when an
            // `async def` returned a channel keeps working.
            if let IrType::Task(elem) = chan_ty {
                let elem_ty = *elem;
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::BuiltinCall {
                        result,
                        name: "async_block_on".to_owned(),
                        args: vec![chan_val],
                        result_ty: elem_ty.clone(),
                    },
                    Some(elem_ty.clone()),
                );
                return Ok((result, elem_ty));
            }
            // Prefer the concrete element type recorded when send() was called.
            let elem_ty = match (self.chan_elem_types.get(&chan_val), chan_ty) {
                (Some(t), _) => t.clone(),
                (None, IrType::Chan(elem)) => *elem,
                (None, other) => {
                    return Err(LowerError::TypeMismatch {
                        expected: "channel".into(),
                        found: other.to_string(),
                        span,
                    })
                }
            };
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::ChanRecv {
                    result,
                    chan: chan_val,
                    elem_ty: elem_ty.clone(),
                },
                Some(elem_ty.clone()),
            );
            return Ok((result, elem_ty));
        }

        // Built-in: tape(x) → mark scalar value as a reverse-mode leaf.
        if dispatch_name == "tape" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "tape() requires exactly 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (value, ty) = self.lower_expr(&args[0])?;
            if !Self::is_reverse_diff_scalar(&ty) {
                return Err(LowerError::rich(
                    "E0110",
                    crate::diagnostics::Diagnostic::error(format!(
                        "`tape` records numbers, but this is `{}`",
                        hints::type_name(&ty)
                    ))
                    .with_primary_span((span).start.0, (span).end.0, None),
                ));
            }
            self.ensure_taped_leaf(value, &ty);
            return Ok((value, ty));
        }

        // Built-in: backward(loss) → run reverse-mode backprop from a taped scalar.
        if dispatch_name == "backward" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "backward() requires exactly 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (loss, loss_ty) = self.lower_expr(&args[0])?;
            if !Self::is_reverse_diff_scalar(&loss_ty) {
                return Err(LowerError::rich(
                    "E0110",
                    crate::diagnostics::Diagnostic::error(format!(
                        "`backward` needs a number (the loss), but this is `{}`",
                        hints::type_name(&loss_ty)
                    ))
                    .with_primary_span((span).start.0, (span).end.0, None),
                ));
            }
            let internal = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::Backward {
                    result: internal,
                    loss: self.tape_ref_for(loss),
                },
                Some(IrType::Scalar(DType::Bool)),
            );
            let dummy = self.builder.fresh_value();
            let dummy_ty = IrType::Scalar(DType::I64);
            self.builder.push_instr(
                IrInstr::ConstInt {
                    result: dummy,
                    value: 0,
                    ty: dummy_ty.clone(),
                },
                Some(dummy_ty.clone()),
            );
            return Ok((dummy, dummy_ty));
        }

        // Built-in: grad(x) → extract d(loss)/d(x) after backward(loss).
        // Only activates when `x` is a taped value (reverse-mode AD context).
        // For forward-mode dual numbers `grad(literal)` falls through to MakeGrad.
        if dispatch_name == "grad" && args.len() == 1 {
            let (value, _) = self.lower_expr(&args[0])?;
            if self.taped_values.contains(&value) {
                let result = self.builder.fresh_value();
                let result_ty = IrType::Scalar(DType::F64);
                self.builder.push_instr(
                    IrInstr::TapeGrad {
                        result,
                        tape_node: self.tape_ref_for(value),
                    },
                    Some(result_ty.clone()),
                );
                return Ok((result, result_ty));
            }
            // Not a taped value → forward-mode MakeGrad path below handles it.
            // Fall through (the MakeGrad handler will re-lower args[0], which is fine
            // for literals/simple vars with no side effects).
        }

        // Built-in: atomic(v) / atomic_new(v) → AtomicNew
        if dispatch_name == "atomic" || callee.name == "atomic_new" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "atomic_new() requires exactly 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (val, inner_ty) = self.lower_expr(&args[0])?;
            let result_ty = IrType::Atomic(Box::new(inner_ty));
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::AtomicNew {
                    result,
                    value: val,
                    result_ty: result_ty.clone(),
                },
                Some(result_ty.clone()),
            );
            return Ok((result, result_ty));
        }

        // Built-in: atomic_load(a) → AtomicLoad
        if dispatch_name == "atomic_load" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "atomic_load() requires exactly 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (val, atomic_ty) = self.lower_expr(&args[0])?;
            let inner_ty = match atomic_ty {
                IrType::Atomic(inner) => *inner,
                other => {
                    return Err(LowerError::TypeMismatch {
                        expected: "atomic<T>".into(),
                        found: other.to_string(),
                        span,
                    })
                }
            };
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::AtomicLoad {
                    result,
                    atomic: val,
                    result_ty: inner_ty.clone(),
                },
                Some(inner_ty.clone()),
            );
            return Ok((result, inner_ty));
        }

        // Built-in: atomic_store(a, v) → AtomicStore
        if dispatch_name == "atomic_store" {
            if args.len() != 2 {
                return Err(builtin_arity_error(
                    "atomic_store() requires exactly 2 arguments",
                    args.len(),
                    span,
                ));
            }
            let (a, _) = self.lower_expr(&args[0])?;
            let (v, _) = self.lower_expr(&args[1])?;
            self.builder.push_instr(
                IrInstr::AtomicStore {
                    atomic: a,
                    value: v,
                },
                None,
            );
            let dummy = self.builder.fresh_value();
            let dummy_ty = IrType::Scalar(DType::I64);
            self.builder.push_instr(
                IrInstr::ConstInt {
                    result: dummy,
                    value: 0,
                    ty: dummy_ty.clone(),
                },
                Some(dummy_ty.clone()),
            );
            return Ok((dummy, dummy_ty));
        }

        // Built-in: mutex_new(v) → MutexNew
        if dispatch_name == "mutex_new" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "mutex_new() requires exactly 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (val, inner_ty) = self.lower_expr(&args[0])?;
            let result_ty = IrType::Mutex(Box::new(inner_ty));
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::MutexNew {
                    result,
                    value: val,
                    result_ty: result_ty.clone(),
                },
                Some(result_ty.clone()),
            );
            return Ok((result, result_ty));
        }

        // Built-in: mutex_lock(m) → MutexLock
        if dispatch_name == "mutex_lock" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "mutex_lock() requires exactly 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (val, mutex_ty) = self.lower_expr(&args[0])?;
            let inner_ty = match mutex_ty {
                IrType::Mutex(inner) => *inner,
                other => {
                    return Err(LowerError::TypeMismatch {
                        expected: "mutex<T>".into(),
                        found: other.to_string(),
                        span,
                    })
                }
            };
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::MutexLock {
                    result,
                    mutex: val,
                    result_ty: inner_ty.clone(),
                },
                Some(inner_ty.clone()),
            );
            return Ok((result, inner_ty));
        }

        // Built-in: barrier() → Barrier (sync point, no-op in interpreter)
        if dispatch_name == "barrier" {
            self.builder.push_instr(IrInstr::Barrier, None);
            let dummy = self.builder.fresh_value();
            let dummy_ty = IrType::Scalar(DType::I64);
            self.builder.push_instr(
                IrInstr::ConstInt {
                    result: dummy,
                    value: 0,
                    ty: dummy_ty.clone(),
                },
                Some(dummy_ty.clone()),
            );
            return Ok((dummy, dummy_ty));
        }

        // Built-in: mutex_unlock(m) → MutexUnlock (no-op in interpreter, returns unit)
        if dispatch_name == "mutex_unlock" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "mutex_unlock() requires exactly 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (val, _) = self.lower_expr(&args[0])?;
            self.builder
                .push_instr(IrInstr::MutexUnlock { mutex: val }, None);
            let dummy = self.builder.fresh_value();
            let dummy_ty = IrType::Scalar(DType::I64);
            self.builder.push_instr(
                IrInstr::ConstInt {
                    result: dummy,
                    value: 0,
                    ty: dummy_ty.clone(),
                },
                Some(dummy_ty.clone()),
            );
            return Ok((dummy, dummy_ty));
        }

        // Built-in: atomic_add(a, v) → AtomicAdd (returns new value)
        if dispatch_name == "atomic_add" {
            if args.len() != 2 {
                return Err(builtin_arity_error(
                    "atomic_add() requires exactly 2 arguments",
                    args.len(),
                    span,
                ));
            }
            let (a, atomic_ty) = self.lower_expr(&args[0])?;
            let (v, _) = self.lower_expr(&args[1])?;
            let inner_ty = if let IrType::Atomic(inner) = atomic_ty {
                *inner
            } else {
                IrType::Scalar(DType::I64)
            };
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::AtomicAdd {
                    result,
                    atomic: a,
                    value: v,
                    result_ty: inner_ty.clone(),
                },
                Some(inner_ty.clone()),
            );
            return Ok((result, inner_ty));
        }

        // Built-in: some(v) → MakeSome
        if dispatch_name == "some" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "some() requires exactly 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (val, inner_ty) = self.lower_expr(&args[0])?;
            let result_ty = IrType::Option(Box::new(inner_ty));
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::MakeSome {
                    result,
                    value: val,
                    result_ty: result_ty.clone(),
                },
                Some(result_ty.clone()),
            );
            return Ok((result, result_ty));
        }

        // Built-in: none() → MakeNone (also handled as identifier)
        if dispatch_name == "none" {
            let elem = match &self.binding_ty {
                Some(IrType::Option(e)) => Some((**e).clone()),
                _ => self.typeck_hint(span).and_then(|h| h.part(0)),
            };
            let elem =
                elem.ok_or_else(|| self.undetermined(span, "what this `none` would hold"))?;
            let result_ty = IrType::Option(Box::new(elem));
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::MakeNone {
                    result,
                    result_ty: result_ty.clone(),
                },
                Some(result_ty.clone()),
            );
            return Ok((result, result_ty));
        }

        // Built-in: is_some(v) → IsSome
        if dispatch_name == "is_some" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "is_some() requires exactly 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (val, _) = self.lower_expr(&args[0])?;
            let result = self.builder.fresh_value();
            let ty = IrType::Scalar(DType::Bool);
            self.builder.push_instr(
                IrInstr::IsSome {
                    result,
                    operand: val,
                },
                Some(ty.clone()),
            );
            return Ok((result, ty));
        }

        // Built-in: unwrap(v) → OptionUnwrap (option<T>) or ResultUnwrap (result<T,E>)
        if dispatch_name == "unwrap" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "unwrap() requires exactly 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (val, val_ty) = self.lower_expr(&args[0])?;
            let result = self.builder.fresh_value();
            match &val_ty {
                IrType::ResultType(ok_ty, _) => {
                    let inner_ty = (**ok_ty).clone();
                    self.builder.push_instr(
                        IrInstr::ResultUnwrap {
                            result,
                            operand: val,
                            result_ty: inner_ty.clone(),
                        },
                        Some(inner_ty.clone()),
                    );
                    return Ok((result, inner_ty));
                }
                IrType::Option(inner) => {
                    let inner_ty = (**inner).clone();
                    self.builder.push_instr(
                        IrInstr::OptionUnwrap {
                            result,
                            operand: val,
                            result_ty: inner_ty.clone(),
                        },
                        Some(inner_ty.clone()),
                    );
                    return Ok((result, inner_ty));
                }
                other => {
                    return Err(LowerError::TypeMismatch {
                        expected: "an option or a result to unwrap".into(),
                        found: other.to_string(),
                        span,
                    });
                }
            }
        }

        // Built-in: ok(v) → MakeOk
        if dispatch_name == "ok" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "ok() requires exactly 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (val, inner_ty) = self.lower_expr(&args[0])?;
            // The error type the checker solved (from an annotation or a
            // use); it defaulted to `i64`.
            let err_ty = self
                .typeck_hint(span)
                .and_then(|h| h.part(1))
                .ok_or_else(|| self.undetermined(span, "the error type of this `ok`"))?;
            let result_ty = IrType::ResultType(Box::new(inner_ty), Box::new(err_ty));
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::MakeOk {
                    result,
                    value: val,
                    result_ty: result_ty.clone(),
                },
                Some(result_ty.clone()),
            );
            return Ok((result, result_ty));
        }

        // Built-in: err(v) → MakeErr
        if dispatch_name == "err" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "err() requires exactly 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (val, inner_ty) = self.lower_expr(&args[0])?;
            let ok_ty = self
                .typeck_hint(span)
                .and_then(|h| h.part(0))
                .ok_or_else(|| self.undetermined(span, "the success type of this `err`"))?;
            let result_ty = IrType::ResultType(Box::new(ok_ty), Box::new(inner_ty));
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::MakeErr {
                    result,
                    value: val,
                    result_ty: result_ty.clone(),
                },
                Some(result_ty.clone()),
            );
            return Ok((result, result_ty));
        }

        // Built-in: is_ok(v) → IsOk
        if dispatch_name == "is_ok" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "is_ok() requires exactly 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (val, _) = self.lower_expr(&args[0])?;
            let result = self.builder.fresh_value();
            let ty = IrType::Scalar(DType::Bool);
            self.builder.push_instr(
                IrInstr::IsOk {
                    result,
                    operand: val,
                },
                Some(ty.clone()),
            );
            return Ok((result, ty));
        }

        // Built-in: is_none(v) → !IsSome
        if dispatch_name == "is_none" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "is_none() requires exactly 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (val, _) = self.lower_expr(&args[0])?;
            let bool_ty = IrType::Scalar(DType::Bool);
            let is_some = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::IsSome {
                    result: is_some,
                    operand: val,
                },
                Some(bool_ty.clone()),
            );
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::UnaryOp {
                    result,
                    op: ScalarUnaryOp::Not,
                    operand: is_some,
                    ty: bool_ty.clone(),
                },
                Some(bool_ty.clone()),
            );
            return Ok((result, bool_ty));
        }

        // Built-in: is_err(v) → !IsOk
        if dispatch_name == "is_err" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "is_err() requires exactly 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (val, _) = self.lower_expr(&args[0])?;
            let bool_ty = IrType::Scalar(DType::Bool);
            let is_ok = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::IsOk {
                    result: is_ok,
                    operand: val,
                },
                Some(bool_ty.clone()),
            );
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::UnaryOp {
                    result,
                    op: ScalarUnaryOp::Not,
                    operand: is_ok,
                    ty: bool_ty.clone(),
                },
                Some(bool_ty.clone()),
            );
            return Ok((result, bool_ty));
        }

        // Built-in: unwrap_err(v) → ResultUnwrapErr
        if dispatch_name == "unwrap_err" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "unwrap_err() requires exactly 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (val, val_ty) = self.lower_expr(&args[0])?;
            let result = self.builder.fresh_value();
            let err_ty = match &val_ty {
                IrType::ResultType(_, err_ty) => (**err_ty).clone(),
                other => {
                    return Err(LowerError::TypeMismatch {
                        expected: "result<T, E>".into(),
                        found: other.to_string(),
                        span,
                    })
                }
            };
            self.builder.push_instr(
                IrInstr::ResultUnwrapErr {
                    result,
                    operand: val,
                    result_ty: err_ty.clone(),
                },
                Some(err_ty.clone()),
            );
            return Ok((result, err_ty));
        }

        // Built-in intrinsic: einsum("notation", inputs...)
        if dispatch_name == "einsum" {
            return self.lower_einsum(args, span);
        }

        // Check if the callee is a closure variable in scope.
        if let Some((closure_val, IrType::Fn { ret, .. })) = self.scope.get(&callee_name).cloned() {
            let ret_ty = *ret;
            let mut arg_vals = Vec::with_capacity(args.len());
            for arg in args {
                let (v, _) = self.lower_expr(arg)?;
                arg_vals.push(v);
            }
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::CallClosure {
                    result: Some(result),
                    closure: closure_val,
                    args: arg_vals,
                    result_ty: ret_ty.clone(),
                    pass_env: true,
                },
                Some(ret_ty.clone()),
            );
            return Ok((result, ret_ty));
        }

        // Built-in: len(s) → StrLen or ListLen depending on argument type
        if dispatch_name == "len" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "len() requires exactly 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (operand, operand_ty) = self.lower_expr(&args[0])?;
            let result = self.builder.fresh_value();
            let ty = IrType::Scalar(DType::I64);
            match &operand_ty {
                IrType::List(_) => {
                    self.builder.push_instr(
                        IrInstr::ListLen {
                            result,
                            list: operand,
                        },
                        Some(ty.clone()),
                    );
                }
                IrType::Array { len, .. } => {
                    self.builder.push_instr(
                        IrInstr::ConstInt {
                            result,
                            value: *len as i64,
                            ty: ty.clone(),
                        },
                        Some(ty.clone()),
                    );
                }
                _ => {
                    self.builder
                        .push_instr(IrInstr::StrLen { result, operand }, Some(ty.clone()));
                }
            }
            return Ok((result, ty));
        }

        // Built-in: concat(s, t) → StrConcat
        if dispatch_name == "concat" {
            if args.len() != 2 {
                return Err(builtin_arity_error(
                    "concat() requires exactly 2 arguments",
                    args.len(),
                    span,
                ));
            }
            let (lhs, _) = self.lower_expr(&args[0])?;
            let (rhs, _) = self.lower_expr(&args[1])?;
            let result = self.builder.fresh_value();
            self.builder
                .push_instr(IrInstr::StrConcat { result, lhs, rhs }, Some(IrType::Str));
            return Ok((result, IrType::Str));
        }

        // Built-in: to_str(v) → ValueToStr
        if dispatch_name == "to_str" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "to_str() requires exactly 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (operand, _) = self.lower_expr(&args[0])?;
            let result = self.builder.fresh_value();
            self.builder
                .push_instr(IrInstr::ValueToStr { result, operand }, Some(IrType::Str));
            return Ok((result, IrType::Str));
        }

        // Built-in: to_f64(v) → Cast to f64
        if dispatch_name == "to_f64" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "to_f64() requires exactly 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (operand, from_ty) = self.lower_expr(&args[0])?;
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::Cast {
                    result,
                    operand,
                    from_ty,
                    to_ty: IrType::Scalar(DType::F64),
                },
                Some(IrType::Scalar(DType::F64)),
            );
            return Ok((result, IrType::Scalar(DType::F64)));
        }

        // Built-in: format("...", args...) — split on "{}" and concat with args
        if dispatch_name == "format" {
            if args.is_empty() {
                return Err(builtin_arity_error(
                    "format() requires at least 1 argument (the format string)",
                    args.len(),
                    span,
                ));
            }
            // First arg must be a string literal.
            let fmt_str = match synth_marker_thir_expr(&args[0]).as_ref().map(|e| &e.kind) {
                Some(crate::thir::ThirExprKind::Literal(crate::thir::ThirLiteral::Str(v))) => {
                    v.clone()
                }
                Some(_) => {
                    return Err(LowerError::rich(
                        "E0126",
                        crate::diagnostics::Diagnostic::error(
                            "`format` needs a string literal as its first argument".to_string(),
                        )
                        .with_primary_span(
                            (span).start.0,
                            (span).end.0,
                            None,
                        ),
                    ))
                }
                None => match &args[0] {
                    AstExpr::StringLit { value, .. } => value.clone(),
                    _ => {
                        return Err(LowerError::rich(
                            "E0126",
                            crate::diagnostics::Diagnostic::error(
                                "`format` needs a string literal as its first argument".to_string(),
                            )
                            .with_primary_span(
                                (span).start.0,
                                (span).end.0,
                                None,
                            ),
                        ))
                    }
                },
            };
            // Split the format string on "{}" to get pieces.
            let pieces: Vec<&str> = fmt_str.split("{}").collect();
            let n_holes = pieces.len().saturating_sub(1);
            if n_holes != args.len() - 1 {
                return Err(LowerError::rich(
                    "E0126",
                    crate::diagnostics::Diagnostic::error(format!(
                        "the format string has {} holes but {} values are given",
                        n_holes,
                        args.len() - 1
                    ))
                    .with_primary_span((span).start.0, (span).end.0, None),
                ));
            }
            // Lower each argument (skip index 0, the format string).
            let mut arg_vals: Vec<ValueId> = Vec::new();
            for arg in &args[1..] {
                let (v, _) = self.lower_expr(arg)?;
                // Convert to string representation.
                let s = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::ValueToStr {
                        result: s,
                        operand: v,
                    },
                    Some(IrType::Str),
                );
                arg_vals.push(s);
            }
            // Build the concatenated string: piece[0] + arg[0] + piece[1] + arg[1] + ...
            // Start with the first piece as a ConstStr.
            let mut acc = {
                let r = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::ConstStr {
                        result: r,
                        value: pieces[0].to_owned(),
                    },
                    Some(IrType::Str),
                );
                r
            };
            for i in 0..n_holes {
                // Concat with the argument.
                let after_arg = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::StrConcat {
                        result: after_arg,
                        lhs: acc,
                        rhs: arg_vals[i],
                    },
                    Some(IrType::Str),
                );
                // Concat with the next piece.
                let next_piece = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::ConstStr {
                        result: next_piece,
                        value: pieces[i + 1].to_owned(),
                    },
                    Some(IrType::Str),
                );
                acc = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::StrConcat {
                        result: acc,
                        lhs: after_arg,
                        rhs: next_piece,
                    },
                    Some(IrType::Str),
                );
            }
            return Ok((acc, IrType::Str));
        }

        // Built-in: print(v) → Print (returns unit, we return a dummy i64 zero for now)
        if dispatch_name == "print" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "print() requires exactly 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (operand, _) = self.lower_expr(&args[0])?;
            self.builder.push_instr(IrInstr::Print { operand }, None);
            // Return a dummy i64 zero as the "unit" value.
            let dummy = self.builder.fresh_value();
            let ty = IrType::Scalar(DType::I64);
            self.builder.push_instr(
                IrInstr::ConstInt {
                    result: dummy,
                    value: 0,
                    ty: ty.clone(),
                },
                Some(ty.clone()),
            );
            return Ok((dummy, ty));
        }

        // Built-in: read_line() → ReadLine
        if dispatch_name == "read_line" {
            if !args.is_empty() {
                return Err(builtin_arity_error(
                    "read_line() takes no arguments",
                    args.len(),
                    span,
                ));
            }
            let result = self.builder.fresh_value();
            self.builder
                .push_instr(IrInstr::ReadLine { result }, Some(IrType::Str));
            return Ok((result, IrType::Str));
        }

        // Built-in: read_i64() → ReadI64
        if dispatch_name == "read_i64" {
            if !args.is_empty() {
                return Err(builtin_arity_error(
                    "read_i64() takes no arguments",
                    args.len(),
                    span,
                ));
            }
            let result = self.builder.fresh_value();
            let ty = IrType::Scalar(DType::I64);
            self.builder
                .push_instr(IrInstr::ReadI64 { result }, Some(ty.clone()));
            return Ok((result, ty));
        }

        // Built-in: read_f64() → ReadF64
        if dispatch_name == "read_f64" {
            if !args.is_empty() {
                return Err(builtin_arity_error(
                    "read_f64() takes no arguments",
                    args.len(),
                    span,
                ));
            }
            let result = self.builder.fresh_value();
            let ty = IrType::Scalar(DType::F64);
            self.builder
                .push_instr(IrInstr::ReadF64 { result }, Some(ty.clone()));
            return Ok((result, ty));
        }

        // Built-in: parse_i64(s) → ParseI64 → option<i64>
        if dispatch_name == "parse_i64" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "parse_i64() requires exactly 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (operand, _) = self.lower_expr(&args[0])?;
            let result = self.builder.fresh_value();
            let ty = IrType::Option(Box::new(IrType::Scalar(DType::I64)));
            self.builder
                .push_instr(IrInstr::ParseI64 { result, operand }, Some(ty.clone()));
            return Ok((result, ty));
        }

        // Built-in: parse_f64(s) → ParseF64 → option<f64>
        if dispatch_name == "parse_f64" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "parse_f64() requires exactly 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (operand, _) = self.lower_expr(&args[0])?;
            let result = self.builder.fresh_value();
            let ty = IrType::Option(Box::new(IrType::Scalar(DType::F64)));
            self.builder
                .push_instr(IrInstr::ParseF64 { result, operand }, Some(ty.clone()));
            return Ok((result, ty));
        }

        // Built-in: str_index(s, i) → StrIndex → i64
        if dispatch_name == "str_index" {
            if args.len() != 2 {
                return Err(builtin_arity_error(
                    "str_index() requires 2 arguments: (str, i64)",
                    args.len(),
                    span,
                ));
            }
            let (string, _) = self.lower_expr(&args[0])?;
            let (index, _) = self.lower_expr(&args[1])?;
            let result = self.builder.fresh_value();
            let ty = IrType::Scalar(DType::I64);
            self.builder.push_instr(
                IrInstr::StrIndex {
                    result,
                    string,
                    index,
                },
                Some(ty.clone()),
            );
            return Ok((result, ty));
        }

        // Built-in: slice(s, start, end) → StrSlice → str
        if dispatch_name == "slice" {
            if args.len() != 3 {
                return Err(builtin_arity_error(
                    "slice() requires 3 arguments: (str, i64, i64)",
                    args.len(),
                    span,
                ));
            }
            let (string, _) = self.lower_expr(&args[0])?;
            let (start, _) = self.lower_expr(&args[1])?;
            let (end, _) = self.lower_expr(&args[2])?;
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::StrSlice {
                    result,
                    string,
                    start,
                    end,
                },
                Some(IrType::Str),
            );
            return Ok((result, IrType::Str));
        }

        // Built-in: find(s, sub) → StrFind → option<i64>
        if dispatch_name == "find" {
            if args.len() != 2 {
                return Err(builtin_arity_error(
                    "find() requires 2 arguments: (str, str)",
                    args.len(),
                    span,
                ));
            }
            let (haystack, _) = self.lower_expr(&args[0])?;
            let (needle, _) = self.lower_expr(&args[1])?;
            let result = self.builder.fresh_value();
            let ty = IrType::Option(Box::new(IrType::Scalar(DType::I64)));
            self.builder.push_instr(
                IrInstr::StrFind {
                    result,
                    haystack,
                    needle,
                },
                Some(ty.clone()),
            );
            return Ok((result, ty));
        }

        // Built-in: str_replace(s, old, new) → StrReplace → str
        if dispatch_name == "str_replace" {
            if args.len() != 3 {
                return Err(builtin_arity_error(
                    "str_replace() requires 3 arguments: (str, str, str)",
                    args.len(),
                    span,
                ));
            }
            let (string, _) = self.lower_expr(&args[0])?;
            let (from, _) = self.lower_expr(&args[1])?;
            let (to, _) = self.lower_expr(&args[2])?;
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::StrReplace {
                    result,
                    string,
                    from,
                    to,
                },
                Some(IrType::Str),
            );
            return Ok((result, IrType::Str));
        }

        // Built-in: list() or list(a, b, c) → ListNew + optional ListPush
        if dispatch_name == "list" {
            // An annotation `list<T>` types the elements, and each element is
            // lowered against `T`: a nested `list(...)` used to see the outer
            // `list<list<T>>` and reject its own elements. Each element is
            // lowered once -- the first used to be lowered twice, once to
            // infer the type, so `list(f())` called `f` twice.
            let annotated = match &self.binding_ty {
                Some(IrType::List(inner)) => Some((**inner).clone()),
                _ => None,
            };
            let saved = std::mem::replace(&mut self.binding_ty, annotated.clone());
            let mut elements = Vec::with_capacity(args.len());
            for arg in args {
                match self.lower_expr(arg) {
                    Ok(lowered) => elements.push(lowered),
                    Err(e) => {
                        self.binding_ty = saved;
                        return Err(e);
                    }
                }
            }
            self.binding_ty = saved;
            // An empty `list()` with no annotation takes the element type
            // the checker solved from its uses (`row = list()`, an argument
            // for a `list<f64>` parameter); it was left to default to `i64`.
            let elem_ty = annotated
                .or_else(|| elements.first().map(|(_, ty)| ty.clone()))
                .or_else(|| self.typeck_hint(span).and_then(|h| h.part(0)))
                .ok_or_else(|| self.undetermined(span, "what this list holds"))?;
            let list_ty = IrType::List(Box::new(elem_ty.clone()));
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::ListNew {
                    result,
                    elem_ty: elem_ty.clone(),
                },
                Some(list_ty.clone()),
            );
            for (val, _) in elements {
                self.builder.push_instr(
                    IrInstr::ListPush {
                        list: result,
                        value: val,
                    },
                    None,
                );
            }
            return Ok((result, list_ty));
        }

        // Built-in: push(lst, val) / list_push(lst, val) → ListPush — append to list
        if dispatch_name == "push" || callee.name == "list_push" {
            if args.len() != 2 {
                return Err(builtin_arity_error(
                    "push() requires 2 arguments: (list, value)",
                    args.len(),
                    span,
                ));
            }
            let (list, list_ty) = self.lower_expr(&args[0])?;
            let (value, value_ty) = self.lower_expr(&args[1])?;
            // Pushing a concrete struct into a `list<dyn Trait>` coerces it
            // rather than failing validation with "type mismatch: dyn Trait vs
            // Struct". A heterogeneous collection is the main reason trait
            // objects exist, so refusing this made the feature close to
            // useless. See known-issues #18.
            let elem_expected = match &list_ty {
                IrType::List(elem) if matches!(**elem, IrType::TraitObject { .. }) => {
                    Some((**elem).clone())
                }
                _ => None,
            };
            let value = match elem_expected {
                Some(exp) => {
                    self.coerce_to_trait_object(value, value_ty, &exp, args[1].span())?
                        .0
                }
                None => value,
            };
            self.builder
                .push_instr(IrInstr::ListPush { list, value }, None);
            let dummy = self.builder.fresh_value();
            let ty = IrType::Scalar(DType::I64);
            self.builder.push_instr(
                IrInstr::ConstInt {
                    result: dummy,
                    value: 0,
                    ty: ty.clone(),
                },
                Some(ty.clone()),
            );
            return Ok((dummy, ty));
        }

        // Built-in: pop(lst) → ListPop → elem  (alias for list_pop)
        if dispatch_name == "pop" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "pop() requires 1 argument: (list)",
                    args.len(),
                    span,
                ));
            }
            let (list, list_ty) = self.lower_expr(&args[0])?;
            let elem_ty = if let IrType::List(inner) = &list_ty {
                *inner.clone()
            } else {
                IrType::Scalar(DType::I64)
            };
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::ListPop {
                    result,
                    list,
                    elem_ty: elem_ty.clone(),
                },
                Some(elem_ty.clone()),
            );
            return Ok((result, elem_ty));
        }

        // Built-in: list_len(lst) → ListLen → i64
        if dispatch_name == "list_len" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "list_len() requires 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (list, _) = self.lower_expr(&args[0])?;
            let result = self.builder.fresh_value();
            let ty = IrType::Scalar(DType::I64);
            self.builder
                .push_instr(IrInstr::ListLen { result, list }, Some(ty.clone()));
            return Ok((result, ty));
        }

        // Built-in: list_get(lst, i) → ListGet → elem
        if dispatch_name == "list_get" {
            if args.len() != 2 {
                return Err(builtin_arity_error(
                    "list_get() requires 2 arguments: (list, index)",
                    args.len(),
                    span,
                ));
            }
            let (list, list_ty) = self.lower_expr(&args[0])?;
            let (index, _) = self.lower_expr(&args[1])?;
            let elem_ty = if let IrType::List(inner) = &list_ty {
                *inner.clone()
            } else {
                IrType::Scalar(DType::I64)
            };
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::ListGet {
                    result,
                    list,
                    index,
                    elem_ty: elem_ty.clone(),
                },
                Some(elem_ty.clone()),
            );
            return Ok((result, elem_ty));
        }

        // Built-in: list_set(lst, i, val) → ListSet
        if dispatch_name == "list_set" {
            if args.len() != 3 {
                return Err(builtin_arity_error(
                    "list_set() requires 3 arguments: (list, index, value)",
                    args.len(),
                    span,
                ));
            }
            let (list, _) = self.lower_expr(&args[0])?;
            let (index, _) = self.lower_expr(&args[1])?;
            let (value, _) = self.lower_expr(&args[2])?;
            self.builder
                .push_instr(IrInstr::ListSet { list, index, value }, None);
            let dummy = self.builder.fresh_value();
            let ty = IrType::Scalar(DType::I64);
            self.builder.push_instr(
                IrInstr::ConstInt {
                    result: dummy,
                    value: 0,
                    ty: ty.clone(),
                },
                Some(ty.clone()),
            );
            return Ok((dummy, ty));
        }

        // Built-in: list_pop(lst) → ListPop → elem
        if dispatch_name == "list_pop" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "list_pop() requires 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (list, list_ty) = self.lower_expr(&args[0])?;
            let elem_ty = if let IrType::List(inner) = &list_ty {
                *inner.clone()
            } else {
                IrType::Scalar(DType::I64)
            };
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::ListPop {
                    result,
                    list,
                    elem_ty: elem_ty.clone(),
                },
                Some(elem_ty.clone()),
            );
            return Ok((result, elem_ty));
        }

        // Built-in: list_remove(lst, idx) → ListRemove → elem
        if dispatch_name == "list_remove" {
            if args.len() != 2 {
                return Err(builtin_arity_error(
                    "list_remove() requires 2 arguments (list, index)",
                    args.len(),
                    span,
                ));
            }
            let (list, list_ty) = self.lower_expr(&args[0])?;
            let (idx, _) = self.lower_expr(&args[1])?;
            let elem_ty = if let IrType::List(inner) = &list_ty {
                *inner.clone()
            } else {
                IrType::Scalar(DType::I64)
            };
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::BuiltinCall {
                    result,
                    name: "list_remove".to_string(),
                    args: vec![list, idx],
                    result_ty: elem_ty.clone(),
                },
                Some(elem_ty.clone()),
            );
            return Ok((result, elem_ty));
        }

        // Built-in: list_insert(lst, idx, val) → ListInsert → i64 0 (side-effecting)
        if dispatch_name == "list_insert" {
            if args.len() != 3 {
                return Err(builtin_arity_error(
                    "list_insert() requires 3 arguments (list, index, value)",
                    args.len(),
                    span,
                ));
            }
            let (list, _) = self.lower_expr(&args[0])?;
            let (idx, _) = self.lower_expr(&args[1])?;
            let (val, _) = self.lower_expr(&args[2])?;
            let dummy_ty = IrType::Scalar(DType::I64);
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::BuiltinCall {
                    result,
                    name: "list_insert".to_string(),
                    args: vec![list, idx, val],
                    result_ty: dummy_ty.clone(),
                },
                Some(dummy_ty.clone()),
            );
            return Ok((result, dummy_ty));
        }

        // Built-in: array_to_list(arr) → convert [T; N] to list<T>
        if dispatch_name == "array_to_list" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "array_to_list() requires exactly 1 argument (an array)",
                    args.len(),
                    span,
                ));
            }
            let (arr_val, arr_ty) = self.lower_expr(&args[0])?;
            let (elem_ty, arr_len) = match &arr_ty {
                IrType::Array { elem, len } => ((**elem).clone(), *len),
                _ => {
                    return Err(builtin_arity_error(
                        "array_to_list() requires an array argument",
                        args.len(),
                        span,
                    ));
                }
            };
            let list_ty = IrType::List(Box::new(elem_ty.clone()));
            let list = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::ListNew {
                    result: list,
                    elem_ty: elem_ty.clone(),
                },
                Some(list_ty.clone()),
            );
            for i in 0..arr_len {
                let idx = self.builder.fresh_value();
                let idx_ty = IrType::Scalar(DType::I64);
                self.builder.push_instr(
                    IrInstr::ConstInt {
                        result: idx,
                        value: i as i64,
                        ty: idx_ty.clone(),
                    },
                    Some(idx_ty.clone()),
                );
                let elem = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::ArrayLoad {
                        result: elem,
                        array: arr_val,
                        index: idx,
                        elem_ty: elem_ty.clone(),
                    },
                    Some(elem_ty.clone()),
                );
                self.builder
                    .push_instr(IrInstr::ListPush { list, value: elem }, None);
            }
            return Ok((list, list_ty));
        }

        // Built-in: map() → MapNew — create an empty hash map (keys: str, values: i64 default)
        if dispatch_name == "map" {
            if !args.is_empty() {
                return Err(builtin_arity_error(
                    "map() takes no arguments — it creates an empty hash map",
                    args.len(),
                    span,
                ));
            }
            // Use binding_ty from `val m: map<K, V> = map()` annotation if available.
            // Without an annotation, the types the checker solved from the
            // map's uses; `map<str, i64>` was assumed whatever it held.
            let (key_ty, val_ty) = if let Some(IrType::Map(k, v)) = &self.binding_ty {
                (*k.clone(), *v.clone())
            } else {
                let h = self.typeck_hint(span);
                let k = h
                    .as_ref()
                    .and_then(|h| h.part(0))
                    .ok_or_else(|| self.undetermined(span, "this map's key type"))?;
                let v = h
                    .as_ref()
                    .and_then(|h| h.part(1))
                    .ok_or_else(|| self.undetermined(span, "this map's value type"))?;
                (k, v)
            };
            let result = self.builder.fresh_value();
            let map_ty = IrType::Map(Box::new(key_ty.clone()), Box::new(val_ty.clone()));
            self.builder.push_instr(
                IrInstr::MapNew {
                    result,
                    key_ty,
                    val_ty,
                },
                Some(map_ty.clone()),
            );
            return Ok((result, map_ty));
        }

        // Built-in: map_set(m, k, v) → MapSet
        if dispatch_name == "map_set" {
            if args.len() != 3 {
                return Err(builtin_arity_error(
                    "map_set() requires 3 arguments: (map, key, value)",
                    args.len(),
                    span,
                ));
            }
            let (map, _) = self.lower_expr(&args[0])?;
            let (key, _) = self.lower_expr(&args[1])?;
            let (value, _) = self.lower_expr(&args[2])?;
            self.builder
                .push_instr(IrInstr::MapSet { map, key, value }, None);
            let dummy = self.builder.fresh_value();
            let ty = IrType::Scalar(DType::I64);
            self.builder.push_instr(
                IrInstr::ConstInt {
                    result: dummy,
                    value: 0,
                    ty: ty.clone(),
                },
                Some(ty.clone()),
            );
            return Ok((dummy, ty));
        }

        // Built-in: map_get(m, k) → MapGet → option<val_ty>
        if dispatch_name == "map_get" {
            if args.len() != 2 {
                return Err(builtin_arity_error(
                    "map_get() requires 2 arguments: (map, key)",
                    args.len(),
                    span,
                ));
            }
            let (map, map_ty) = self.lower_expr(&args[0])?;
            let (key, _) = self.lower_expr(&args[1])?;
            let val_ty = if let IrType::Map(_, v) = &map_ty {
                *v.clone()
            } else {
                IrType::Scalar(DType::I64)
            };
            let opt_ty = IrType::Option(Box::new(val_ty.clone()));
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::MapGet {
                    result,
                    map,
                    key,
                    val_ty,
                },
                Some(opt_ty.clone()),
            );
            return Ok((result, opt_ty));
        }

        // Built-in: map_contains(m, k) → MapContains → bool
        if dispatch_name == "map_contains" {
            if args.len() != 2 {
                return Err(builtin_arity_error(
                    "map_contains() requires 2 arguments: (map, key)",
                    args.len(),
                    span,
                ));
            }
            let (map, _) = self.lower_expr(&args[0])?;
            let (key, _) = self.lower_expr(&args[1])?;
            let result = self.builder.fresh_value();
            let ty = IrType::Scalar(DType::Bool);
            self.builder
                .push_instr(IrInstr::MapContains { result, map, key }, Some(ty.clone()));
            return Ok((result, ty));
        }

        // Built-in: map_remove(m, k) → MapRemove
        if dispatch_name == "map_remove" {
            if args.len() != 2 {
                return Err(builtin_arity_error(
                    "map_remove() requires 2 arguments: (map, key)",
                    args.len(),
                    span,
                ));
            }
            let (map, _) = self.lower_expr(&args[0])?;
            let (key, _) = self.lower_expr(&args[1])?;
            self.builder
                .push_instr(IrInstr::MapRemove { map, key }, None);
            let dummy = self.builder.fresh_value();
            let ty = IrType::Scalar(DType::I64);
            self.builder.push_instr(
                IrInstr::ConstInt {
                    result: dummy,
                    value: 0,
                    ty: ty.clone(),
                },
                Some(ty.clone()),
            );
            return Ok((dummy, ty));
        }

        // Built-in: map_len(m) → MapLen → i64
        if dispatch_name == "map_len" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "map_len() requires 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (map, _) = self.lower_expr(&args[0])?;
            let result = self.builder.fresh_value();
            let ty = IrType::Scalar(DType::I64);
            self.builder
                .push_instr(IrInstr::MapLen { result, map }, Some(ty.clone()));
            return Ok((result, ty));
        }

        // ── Phase 56: File I/O builtins ──────────────────────────────────────

        // Built-in: file_read_all(path) → FileReadAll → result<str, str>
        if dispatch_name == "file_read_all" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "file_read_all() requires 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (path, _) = self.lower_expr(&args[0])?;
            let result = self.builder.fresh_value();
            let ty = IrType::ResultType(Box::new(IrType::Str), Box::new(IrType::Str));
            self.builder
                .push_instr(IrInstr::FileReadAll { result, path }, Some(ty.clone()));
            return Ok((result, ty));
        }

        // Built-in: file_write_all(path, content) → FileWriteAll → result<i64, str>
        if dispatch_name == "file_write_all" {
            if args.len() != 2 {
                return Err(builtin_arity_error(
                    "file_write_all() requires 2 arguments",
                    args.len(),
                    span,
                ));
            }
            let (path, _) = self.lower_expr(&args[0])?;
            let (content, _) = self.lower_expr(&args[1])?;
            let result = self.builder.fresh_value();
            let ty =
                IrType::ResultType(Box::new(IrType::Scalar(DType::I64)), Box::new(IrType::Str));
            self.builder.push_instr(
                IrInstr::FileWriteAll {
                    result,
                    path,
                    content,
                },
                Some(ty.clone()),
            );
            return Ok((result, ty));
        }

        // Built-in: file_exists(path) → FileExists → bool
        if dispatch_name == "file_exists" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "file_exists() requires 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (path, _) = self.lower_expr(&args[0])?;
            let result = self.builder.fresh_value();
            let ty = IrType::Scalar(DType::Bool);
            self.builder
                .push_instr(IrInstr::FileExists { result, path }, Some(ty.clone()));
            return Ok((result, ty));
        }

        // Built-in: file_lines(path) → FileLines → list<str>
        if dispatch_name == "file_lines" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "file_lines() requires 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (path, _) = self.lower_expr(&args[0])?;
            let result = self.builder.fresh_value();
            let ty = IrType::List(Box::new(IrType::Str));
            self.builder
                .push_instr(IrInstr::FileLines { result, path }, Some(ty.clone()));
            return Ok((result, ty));
        }

        // ── Database operations ─────────────────────────────────────────────
        if dispatch_name == "db_open" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "db_open(path) requires 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (path, _) = self.lower_expr(&args[0])?;
            let result = self.builder.fresh_value();
            let ty = IrType::Scalar(DType::I64);
            self.builder
                .push_instr(IrInstr::DbOpen { result, path }, Some(ty.clone()));
            return Ok((result, ty));
        }
        if dispatch_name == "db_exec" {
            if args.len() != 2 {
                return Err(builtin_arity_error(
                    "db_exec(db, sql) requires 2 arguments",
                    args.len(),
                    span,
                ));
            }
            let (db, _) = self.lower_expr(&args[0])?;
            let (sql, _) = self.lower_expr(&args[1])?;
            let result = self.builder.fresh_value();
            let ty = IrType::Scalar(DType::I64);
            self.builder
                .push_instr(IrInstr::DbExec { result, db, sql }, Some(ty.clone()));
            return Ok((result, ty));
        }
        if dispatch_name == "db_exec_params" {
            if args.len() != 3 {
                return Err(builtin_arity_error(
                    "db_exec_params(db, sql, params) requires 3 arguments",
                    args.len(),
                    span,
                ));
            }
            let (db, _) = self.lower_expr(&args[0])?;
            let (sql, _) = self.lower_expr(&args[1])?;
            let (params, _) = self.lower_expr(&args[2])?;
            let result = self.builder.fresh_value();
            let ty = IrType::Scalar(DType::I64);
            self.builder.push_instr(
                IrInstr::DbExecParams {
                    result,
                    db,
                    sql,
                    params,
                },
                Some(ty.clone()),
            );
            return Ok((result, ty));
        }
        if dispatch_name == "db_query" {
            if args.len() != 2 {
                return Err(builtin_arity_error(
                    "db_query(db, sql) requires 2 arguments",
                    args.len(),
                    span,
                ));
            }
            let (db, _) = self.lower_expr(&args[0])?;
            let (sql, _) = self.lower_expr(&args[1])?;
            let result = self.builder.fresh_value();
            let ty = IrType::List(Box::new(IrType::List(Box::new(IrType::Str))));
            self.builder
                .push_instr(IrInstr::DbQuery { result, db, sql }, Some(ty.clone()));
            return Ok((result, ty));
        }
        if dispatch_name == "db_query_params" {
            if args.len() != 3 {
                return Err(builtin_arity_error(
                    "db_query_params(db, sql, params) requires 3 arguments",
                    args.len(),
                    span,
                ));
            }
            let (db, _) = self.lower_expr(&args[0])?;
            let (sql, _) = self.lower_expr(&args[1])?;
            let (params, _) = self.lower_expr(&args[2])?;
            let result = self.builder.fresh_value();
            let ty = IrType::List(Box::new(IrType::List(Box::new(IrType::Str))));
            self.builder.push_instr(
                IrInstr::DbQueryParams {
                    result,
                    db,
                    sql,
                    params,
                },
                Some(ty.clone()),
            );
            return Ok((result, ty));
        }
        if dispatch_name == "db_close" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "db_close(db) requires 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (db, _) = self.lower_expr(&args[0])?;
            let result = self.builder.fresh_value();
            let ty = IrType::Scalar(DType::I64);
            self.builder
                .push_instr(IrInstr::DbClose { result, db }, Some(ty.clone()));
            return Ok((result, ty));
        }

        // ── Phase 89: Mutable cell (for closure captures) ───────────────────
        // cell(v) → list containing one element (shared via Rc)
        // cell_get(c) → read element 0
        // cell_set(c, v) → write element 0

        if dispatch_name == "cell" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "cell(v) requires 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (val_v, val_ty) = self.lower_expr(&args[0])?;
            let list_ty = IrType::List(Box::new(val_ty.clone()));
            let list = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::ListNew {
                    result: list,
                    elem_ty: val_ty.clone(),
                },
                Some(list_ty.clone()),
            );
            self.builder
                .push_instr(IrInstr::ListPush { list, value: val_v }, None);
            return Ok((list, list_ty));
        }
        if dispatch_name == "cell_get" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "cell_get(c) requires 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (cell, cell_ty) = self.lower_expr(&args[0])?;
            let elem_ty = match &cell_ty {
                IrType::List(inner) => *inner.clone(),
                other => {
                    return Err(LowerError::TypeMismatch {
                        expected: "a cell".into(),
                        found: other.to_string(),
                        span,
                    })
                }
            };
            let zero = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::ConstInt {
                    result: zero,
                    value: 0,
                    ty: IrType::Scalar(DType::I64),
                },
                Some(IrType::Scalar(DType::I64)),
            );
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::ListGet {
                    result,
                    list: cell,
                    index: zero,
                    elem_ty: elem_ty.clone(),
                },
                Some(elem_ty.clone()),
            );
            return Ok((result, elem_ty));
        }
        if dispatch_name == "cell_set" {
            if args.len() != 2 {
                return Err(builtin_arity_error(
                    "cell_set(c, v) requires 2 arguments",
                    args.len(),
                    span,
                ));
            }
            let (cell, _) = self.lower_expr(&args[0])?;
            let (new_val, _) = self.lower_expr(&args[1])?;
            let zero = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::ConstInt {
                    result: zero,
                    value: 0,
                    ty: IrType::Scalar(DType::I64),
                },
                Some(IrType::Scalar(DType::I64)),
            );
            self.builder.push_instr(
                IrInstr::ListSet {
                    list: cell,
                    index: zero,
                    value: new_val,
                },
                None,
            );
            let unit = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::ConstInt {
                    result: unit,
                    value: 0,
                    ty: IrType::Scalar(DType::I64),
                },
                Some(IrType::Scalar(DType::I64)),
            );
            return Ok((unit, IrType::Scalar(DType::I64)));
        }

        // ── Phase 88: TCP network I/O ────────────────────────────────────────

        if dispatch_name == "tcp_connect" {
            if args.len() != 2 {
                return Err(builtin_arity_error(
                    "tcp_connect(host, port) requires 2 args",
                    args.len(),
                    span,
                ));
            }
            let (host, _) = self.lower_expr(&args[0])?;
            let (port, _) = self.lower_expr(&args[1])?;
            let result = self.builder.fresh_value();
            let ty = IrType::Scalar(DType::I64);
            self.builder
                .push_instr(IrInstr::TcpConnect { result, host, port }, Some(ty.clone()));
            return Ok((result, ty));
        }
        if dispatch_name == "tcp_listen" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "tcp_listen(port) requires 1 arg",
                    args.len(),
                    span,
                ));
            }
            let (port, _) = self.lower_expr(&args[0])?;
            let result = self.builder.fresh_value();
            let ty = IrType::Scalar(DType::I64);
            self.builder
                .push_instr(IrInstr::TcpListen { result, port }, Some(ty.clone()));
            return Ok((result, ty));
        }
        if dispatch_name == "tcp_accept" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "tcp_accept(listener) requires 1 arg",
                    args.len(),
                    span,
                ));
            }
            let (listener, _) = self.lower_expr(&args[0])?;
            let result = self.builder.fresh_value();
            let ty = IrType::Scalar(DType::I64);
            self.builder
                .push_instr(IrInstr::TcpAccept { result, listener }, Some(ty.clone()));
            return Ok((result, ty));
        }
        if dispatch_name == "tcp_read" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "tcp_read(conn) requires 1 arg",
                    args.len(),
                    span,
                ));
            }
            let (conn, _) = self.lower_expr(&args[0])?;
            let result = self.builder.fresh_value();
            let ty = IrType::Str;
            self.builder
                .push_instr(IrInstr::TcpRead { result, conn }, Some(ty.clone()));
            return Ok((result, ty));
        }
        if dispatch_name == "tcp_write" {
            if args.len() != 2 {
                return Err(builtin_arity_error(
                    "tcp_write(conn, data) requires 2 args",
                    args.len(),
                    span,
                ));
            }
            let (conn, _) = self.lower_expr(&args[0])?;
            let (data, _) = self.lower_expr(&args[1])?;
            let unit = self.builder.fresh_value();
            self.builder
                .push_instr(IrInstr::TcpWrite { conn, data }, None);
            // Return a dummy unit value.
            self.builder.push_instr(
                IrInstr::ConstInt {
                    result: unit,
                    value: 0,
                    ty: IrType::Scalar(DType::I64),
                },
                Some(IrType::Scalar(DType::I64)),
            );
            return Ok((unit, IrType::Scalar(DType::I64)));
        }
        if dispatch_name == "tcp_close" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "tcp_close(conn) requires 1 arg",
                    args.len(),
                    span,
                ));
            }
            let (conn, _) = self.lower_expr(&args[0])?;
            let unit = self.builder.fresh_value();
            self.builder.push_instr(IrInstr::TcpClose { conn }, None);
            self.builder.push_instr(
                IrInstr::ConstInt {
                    result: unit,
                    value: 0,
                    ty: IrType::Scalar(DType::I64),
                },
                Some(IrType::Scalar(DType::I64)),
            );
            return Ok((unit, IrType::Scalar(DType::I64)));
        }

        // ── Phase 58: Extended collection builtins ───────────────────────────

        // Built-in: list_contains(list, val) → ListContains → bool
        if dispatch_name == "list_contains" {
            if args.len() != 2 {
                return Err(builtin_arity_error(
                    "list_contains() requires 2 arguments",
                    args.len(),
                    span,
                ));
            }
            let (list, _) = self.lower_expr(&args[0])?;
            let (value, _) = self.lower_expr(&args[1])?;
            let result = self.builder.fresh_value();
            let ty = IrType::Scalar(DType::Bool);
            self.builder.push_instr(
                IrInstr::ListContains {
                    result,
                    list,
                    value,
                },
                Some(ty.clone()),
            );
            return Ok((result, ty));
        }

        // Built-in: list_sort(list) → ListSort (side-effecting, returns unit-like dummy)
        if dispatch_name == "list_sort" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "list_sort() requires 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (list, _) = self.lower_expr(&args[0])?;
            self.builder.push_instr(IrInstr::ListSort { list }, None);
            let dummy = self.builder.fresh_value();
            let ty = IrType::Scalar(DType::I64);
            self.builder.push_instr(
                IrInstr::ConstInt {
                    result: dummy,
                    value: 0,
                    ty: ty.clone(),
                },
                Some(ty.clone()),
            );
            return Ok((dummy, ty));
        }

        // Built-in: map_keys(map) → MapKeys → list<str>
        if dispatch_name == "map_keys" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "map_keys() requires 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (map, _) = self.lower_expr(&args[0])?;
            let result = self.builder.fresh_value();
            let ty = IrType::List(Box::new(IrType::Str));
            self.builder
                .push_instr(IrInstr::MapKeys { result, map }, Some(ty.clone()));
            return Ok((result, ty));
        }

        // Built-in: map_values(map) → MapValues → list<?>
        if dispatch_name == "map_values" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "map_values() requires 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (map, map_ty) = self.lower_expr(&args[0])?;
            let result = self.builder.fresh_value();
            let val_ty = if let IrType::Map(_, v) = &map_ty {
                *v.clone()
            } else {
                IrType::Scalar(DType::I64)
            };
            let ty = IrType::List(Box::new(val_ty));
            self.builder
                .push_instr(IrInstr::MapValues { result, map }, Some(ty.clone()));
            return Ok((result, ty));
        }

        // Built-in: list_concat(a, b) → ListConcat → list
        if dispatch_name == "list_concat" {
            if args.len() != 2 {
                return Err(builtin_arity_error(
                    "list_concat() requires 2 arguments",
                    args.len(),
                    span,
                ));
            }
            let (lhs, lhs_ty) = self.lower_expr(&args[0])?;
            let (rhs, _) = self.lower_expr(&args[1])?;
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::ListConcat { result, lhs, rhs },
                Some(lhs_ty.clone()),
            );
            return Ok((result, lhs_ty));
        }

        // Built-in: list_slice(list, start, end) → ListSlice → list
        if dispatch_name == "list_slice" {
            if args.len() != 3 {
                return Err(builtin_arity_error(
                    "list_slice() requires 3 arguments",
                    args.len(),
                    span,
                ));
            }
            let (list, list_ty) = self.lower_expr(&args[0])?;
            let (start, _) = self.lower_expr(&args[1])?;
            let (end, _) = self.lower_expr(&args[2])?;
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::ListSlice {
                    result,
                    list,
                    start,
                    end,
                },
                Some(list_ty.clone()),
            );
            return Ok((result, list_ty));
        }

        // ── Phase 59: Process / environment builtins ─────────────────────────

        // Built-in: exit(code) → ProcessExit (does not return)
        if dispatch_name == "exit" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "exit() requires 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (code, _) = self.lower_expr(&args[0])?;
            self.builder.push_instr(IrInstr::ProcessExit { code }, None);
            let dummy = self.builder.fresh_value();
            let ty = IrType::Scalar(DType::I64);
            self.builder.push_instr(
                IrInstr::ConstInt {
                    result: dummy,
                    value: 0,
                    ty: ty.clone(),
                },
                Some(ty.clone()),
            );
            return Ok((dummy, ty));
        }

        // Built-in: args() → ProcessArgs → list<str>
        if dispatch_name == "args" {
            if !args.is_empty() {
                return Err(builtin_arity_error(
                    "args() takes no arguments",
                    args.len(),
                    span,
                ));
            }
            let result = self.builder.fresh_value();
            let ty = IrType::List(Box::new(IrType::Str));
            self.builder
                .push_instr(IrInstr::ProcessArgs { result }, Some(ty.clone()));
            return Ok((result, ty));
        }

        // Built-in: env_var(name) → EnvVar → option<str>
        if dispatch_name == "env_var" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "env_var() requires 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (name, _) = self.lower_expr(&args[0])?;
            let result = self.builder.fresh_value();
            let ty = IrType::Option(Box::new(IrType::Str));
            self.builder
                .push_instr(IrInstr::EnvVar { result, name }, Some(ty.clone()));
            return Ok((result, ty));
        }

        // Built-in: panic(msg) → Panic (terminator; does not return)
        if dispatch_name == "panic" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "panic() requires exactly 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (msg, _) = self.lower_expr(&args[0])?;
            if !self.builder.is_current_block_terminated() {
                self.builder.push_instr(
                    IrInstr::Panic {
                        msg,
                        span_byte: Some(span.start.0),
                    },
                    None,
                );
            }
            // Return a dummy value so the type-checker is happy.
            let dummy = self.builder.fresh_value();
            let ty = IrType::Scalar(DType::I64);
            return Ok((dummy, ty));
        }

        // Built-in: assert(cond) — lowers to: if cond { continue } else { panic("assertion failed") }
        if dispatch_name == "assert" {
            if args.is_empty() || args.len() > 2 {
                return Err(LowerError::rich(
                    "E0109",
                    crate::diagnostics::Diagnostic::error(
                        "`assert` takes a condition and an optional message".to_string(),
                    )
                    .with_primary_span((span).start.0, (span).end.0, None),
                ));
            }
            let (cond, _) = self.lower_expr(&args[0])?;
            let then_block = self.builder.create_block(Some("assert_ok"));
            let panic_block = self.builder.create_block(Some("assert_fail"));
            let merge_block = self.builder.create_block(Some("assert_merge"));
            // CondBr: if cond → then_block, else → panic_block
            self.builder.push_instr(
                IrInstr::CondBr {
                    cond,
                    then_block,
                    then_args: vec![],
                    else_block: panic_block,
                    else_args: vec![],
                },
                None,
            );
            // panic_block: emit panic message + unreachable return (ValidatePass needs a terminator)
            self.builder.set_current_block(panic_block);
            let prefix = self.builder.fresh_value();
            // A literal message (the condition and position the parser adds
            // for a file) is joined here, so the failure path stays one
            // constant: no concatenation for the allocation-free proof to
            // reject in embedded and evolved code.
            let fixed = match args.get(1) {
                None => Some("assertion failed".to_owned()),
                Some(AstExpr::StringLit { value, .. }) => {
                    Some(format!("assertion failed: {value}"))
                }
                Some(_) => None,
            };
            self.builder.push_instr(
                IrInstr::ConstStr {
                    result: prefix,
                    value: fixed.clone().unwrap_or_else(|| "assertion failed: ".into()),
                },
                Some(IrType::Str),
            );
            // Any other message (`assert(cond, "n is " + to_str(n))`) is
            // evaluated only when the assert fails.
            let msg_val = match args.get(1).filter(|_| fixed.is_none()) {
                Some(extra) => {
                    let (text, text_ty) = self.lower_expr(extra)?;
                    let text = if text_ty == IrType::Str {
                        text
                    } else {
                        let converted = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::ValueToStr {
                                result: converted,
                                operand: text,
                            },
                            Some(IrType::Str),
                        );
                        converted
                    };
                    let joined = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::StrConcat {
                            result: joined,
                            lhs: prefix,
                            rhs: text,
                        },
                        Some(IrType::Str),
                    );
                    joined
                }
                None => prefix,
            };
            // Attach the assert's own source position to the Panic.
            //
            // Without this the failure reported the *previous* statement — a line
            // that succeeded, which is worse than no location at all. The span
            // recorded by `lower_stmt` is consumed by the first instruction the
            // condition emits, and when the condition is const-foldable that
            // instruction is then deleted by DCE. `span_table` is keyed by
            // (block_id, instr_idx) and no pass maintains it, so the entry is
            // simply lost and the interpreter's sticky `last_byte` still holds
            // the previous statement's position.
            //
            // Recording it here is robust because everything in `assert_fail`
            // is live: the message and the Panic that consumes it.
            // See known-issues #20 for the general span-table staleness, which
            // this does not fix.
            self.builder.push_instr(
                IrInstr::Panic {
                    msg: msg_val,
                    span_byte: Some(span.start.0),
                },
                None,
            );
            // then_block: jump to merge
            self.builder.set_current_block(then_block);
            self.builder.push_instr(
                IrInstr::Br {
                    target: merge_block,
                    args: vec![],
                },
                None,
            );
            // merge_block: continue with dummy zero
            self.builder.set_current_block(merge_block);
            let dummy = self.builder.fresh_value();
            let ty = IrType::Scalar(DType::I64);
            self.builder.push_instr(
                IrInstr::ConstInt {
                    result: dummy,
                    value: 0,
                    ty: ty.clone(),
                },
                Some(ty.clone()),
            );
            return Ok((dummy, ty));
        }

        // Built-in: grad(v) → MakeGrad(value=v, tangent=1.0)
        if dispatch_name == "grad" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "grad() requires exactly 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (val, inner_ty) = self.lower_expr(&args[0])?;
            let result_ty = IrType::Grad(Box::new(inner_ty));
            // tangent = 1.0 (seeding the derivative)
            let one = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::ConstFloat {
                    result: one,
                    value: 1.0,
                    ty: IrType::Scalar(DType::F64),
                },
                Some(IrType::Scalar(DType::F64)),
            );
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::MakeGrad {
                    result,
                    value: val,
                    tangent: one,
                    ty: result_ty.clone(),
                },
                Some(result_ty.clone()),
            );
            return Ok((result, result_ty));
        }

        // Built-in: grad_of(closure, x) → numerical derivative via central finite differences
        // Returns (f(x+h) - f(x-h)) / (2*h)  where h = 1e-7
        if dispatch_name == "grad_of" {
            if args.len() != 2 {
                return Err(builtin_arity_error(
                    "grad_of() requires exactly 2 arguments: grad_of(closure, x)",
                    args.len(),
                    span,
                ));
            }
            let (closure_val, closure_ty) = self.lower_expr(&args[0])?;
            let (mut x_val, mut x_ty) = self.lower_expr(&args[1])?;
            // Prefer the closure's param type for arithmetic and calls so
            // CallClosure args/results match the closure signature.
            let (param_ty, ret_ty): (IrType, IrType) = match &closure_ty {
                IrType::Fn { params, ret } if !params.is_empty() => {
                    (params[0].clone(), (*ret).as_ref().clone())
                }
                _ => (x_ty.clone(), x_ty.clone()),
            };
            // If the provided x has a different type, cast it to the closure param type.
            if x_ty != param_ty {
                let cast = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::Cast {
                        result: cast,
                        operand: x_val,
                        from_ty: x_ty.clone(),
                        to_ty: param_ty.clone(),
                    },
                    Some(param_ty.clone()),
                );
                x_val = cast;
                x_ty = param_ty.clone();
            }
            // h = 1e-3 (step for central finite difference; large enough for f32 precision)
            let h_val = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::ConstFloat {
                    result: h_val,
                    value: 1e-3,
                    ty: x_ty.clone(),
                },
                Some(x_ty.clone()),
            );
            // x_plus = x + h
            let x_plus = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::BinOp {
                    result: x_plus,
                    op: BinOp::Add,
                    lhs: x_val,
                    rhs: h_val,
                    ty: x_ty.clone(),
                },
                Some(x_ty.clone()),
            );
            // x_minus = x - h
            let x_minus = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::BinOp {
                    result: x_minus,
                    op: BinOp::Sub,
                    lhs: x_val,
                    rhs: h_val,
                    ty: x_ty.clone(),
                },
                Some(x_ty.clone()),
            );
            // f_plus = closure(x_plus)
            let f_plus = self.builder.fresh_value();
            // Cast arg to closure param type if needed, call closure.
            let f_plus_call_arg = if x_ty != param_ty {
                let cast_arg = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::Cast {
                        result: cast_arg,
                        operand: x_plus,
                        from_ty: x_ty.clone(),
                        to_ty: param_ty.clone(),
                    },
                    Some(param_ty.clone()),
                );
                cast_arg
            } else {
                x_plus
            };
            self.builder.push_instr(
                IrInstr::CallClosure {
                    result: Some(f_plus),
                    closure: closure_val,
                    args: vec![f_plus_call_arg],
                    result_ty: ret_ty.clone(),
                    pass_env: true,
                },
                Some(ret_ty.clone()),
            );
            // f_minus = closure(x_minus)
            let f_minus = self.builder.fresh_value();
            let f_minus_call_arg = if x_ty != param_ty {
                let cast_arg = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::Cast {
                        result: cast_arg,
                        operand: x_minus,
                        from_ty: x_ty.clone(),
                        to_ty: param_ty.clone(),
                    },
                    Some(param_ty.clone()),
                );
                cast_arg
            } else {
                x_minus
            };
            self.builder.push_instr(
                IrInstr::CallClosure {
                    result: Some(f_minus),
                    closure: closure_val,
                    args: vec![f_minus_call_arg],
                    result_ty: ret_ty.clone(),
                    pass_env: true,
                },
                Some(ret_ty.clone()),
            );
            // Perform arithmetic in the closure's return type (conservative fix).
            // diff = f_plus - f_minus
            let diff = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::BinOp {
                    result: diff,
                    op: BinOp::Sub,
                    lhs: f_plus,
                    rhs: f_minus,
                    ty: ret_ty.clone(),
                },
                Some(ret_ty.clone()),
            );
            // two_h = 2.0 * h (compute in return type)
            let two = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::ConstFloat {
                    result: two,
                    value: 2.0,
                    ty: ret_ty.clone(),
                },
                Some(ret_ty.clone()),
            );
            // Cast h (which is in param type x_ty) to return type if needed
            // so multiplication happens in the same type.
            let h_val_ret = if x_ty != ret_ty {
                let cf = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::Cast {
                        result: cf,
                        operand: h_val,
                        from_ty: x_ty.clone(),
                        to_ty: ret_ty.clone(),
                    },
                    Some(ret_ty.clone()),
                );
                cf
            } else {
                h_val
            };
            let two_h = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::BinOp {
                    result: two_h,
                    op: BinOp::Mul,
                    lhs: two,
                    rhs: h_val_ret,
                    ty: ret_ty.clone(),
                },
                Some(ret_ty.clone()),
            );
            // result = diff / two_h (in return type)
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::BinOp {
                    result,
                    op: BinOp::Div,
                    lhs: diff,
                    rhs: two_h,
                    ty: ret_ty.clone(),
                },
                Some(ret_ty.clone()),
            );
            return Ok((result, ret_ty));
        }

        // Built-in: sparsify(arr) → Sparsify (convert dense array to sparse representation)
        if dispatch_name == "sparsify" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "sparsify() requires exactly 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (val, inner_ty) = self.lower_expr(&args[0])?;
            let result_ty = IrType::Sparse(Box::new(inner_ty));
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::Sparsify {
                    result,
                    operand: val,
                    ty: result_ty.clone(),
                },
                Some(result_ty.clone()),
            );
            return Ok((result, result_ty));
        }

        // Built-in: densify(sparse) → Densify (reconstruct the dense collection)
        //
        // This returns dense data, as the name says and as the README, the LSP
        // hover signature and the runtime's `iris_densify` all describe. It used
        // to return the non-zero count as an i64 — a shortcut that contradicted
        // every one of those, and that the native backend did not implement the
        // same way, so a program's meaning depended on how it was run. The count
        // is now `nnz(s)` below.
        if dispatch_name == "densify" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "densify() requires exactly 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (val, sparse_ty) = self.lower_expr(&args[0])?;
            // The runtime reconstructs an IrisList, so the result is a list of the
            // sparse value's element type.
            let elem_ty = match &sparse_ty {
                IrType::Sparse(inner) => match &**inner {
                    IrType::Array { elem, .. } => (**elem).clone(),
                    IrType::List(elem) => (**elem).clone(),
                    other => other.clone(),
                },
                other => other.clone(),
            };
            let result_ty = IrType::List(Box::new(elem_ty));
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::Densify {
                    result,
                    operand: val,
                    ty: result_ty.clone(),
                },
                Some(result_ty.clone()),
            );
            return Ok((result, result_ty));
        }

        // Built-in: nnz(sparse) → count of stored non-zero elements.
        if dispatch_name == "nnz" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "nnz() requires exactly 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (val, _) = self.lower_expr(&args[0])?;
            let result_ty = IrType::Scalar(DType::I64);
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::SparseNnz {
                    result,
                    operand: val,
                },
                Some(result_ty.clone()),
            );
            return Ok((result, result_ty));
        }

        // Built-in: split(s, delim) → list<str>
        if dispatch_name == "split" {
            if args.len() != 2 {
                return Err(builtin_arity_error(
                    "split() requires exactly 2 arguments",
                    args.len(),
                    span,
                ));
            }
            let (str_val, _) = self.lower_expr(&args[0])?;
            let (delim, _) = self.lower_expr(&args[1])?;
            let result = self.builder.fresh_value();
            let ret_ty = IrType::List(Box::new(IrType::Str));
            self.builder.push_instr(
                IrInstr::StrSplit {
                    result,
                    str_val,
                    delim,
                },
                Some(ret_ty.clone()),
            );
            return Ok((result, ret_ty));
        }

        // Built-in: join(lst, delim) → str
        if dispatch_name == "join" {
            if args.len() != 2 {
                return Err(builtin_arity_error(
                    "join() requires exactly 2 arguments",
                    args.len(),
                    span,
                ));
            }
            let (list_val, _) = self.lower_expr(&args[0])?;
            let (delim, _) = self.lower_expr(&args[1])?;
            let result = self.builder.fresh_value();
            let ret_ty = IrType::Str;
            self.builder.push_instr(
                IrInstr::StrJoin {
                    result,
                    list_val,
                    delim,
                },
                Some(ret_ty.clone()),
            );
            return Ok((result, ret_ty));
        }

        // Phase 97: time_now_ms() -> i64
        if dispatch_name == "time_now_ms" {
            let result = self.builder.fresh_value();
            let ty = IrType::Scalar(DType::I64);
            self.builder
                .push_instr(IrInstr::NowMs { result }, Some(ty.clone()));
            return Ok((result, ty));
        }

        // Phase 97: sleep_ms(n: i64) -> i64
        if dispatch_name == "sleep_ms" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "sleep_ms() requires exactly 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (ms, _) = self.lower_expr(&args[0])?;
            let result = self.builder.fresh_value();
            let ty = IrType::Scalar(DType::I64);
            self.builder
                .push_instr(IrInstr::SleepMs { result, ms }, Some(ty.clone()));
            return Ok((result, ty));
        }

        // Built-in string predicates: contains(s, sub), starts_with(s, p), ends_with(s, p)
        {
            let str_pred: Option<fn(ValueId, ValueId, ValueId) -> IrInstr> = match dispatch_name {
                "contains" => Some(|result, haystack, needle| IrInstr::StrContains {
                    result,
                    haystack,
                    needle,
                }),
                "starts_with" => Some(|result, haystack, prefix| IrInstr::StrStartsWith {
                    result,
                    haystack,
                    prefix,
                }),
                "ends_with" => Some(|result, haystack, suffix| IrInstr::StrEndsWith {
                    result,
                    haystack,
                    suffix,
                }),
                _ => None,
            };
            if let Some(mk) = str_pred {
                if args.len() != 2 {
                    return Err(builtin_arity_error(
                        &format!("{}() requires exactly 2 arguments", callee.name),
                        args.len(),
                        span,
                    ));
                }
                let (haystack, _) = self.lower_expr(&args[0])?;
                let (second, _) = self.lower_expr(&args[1])?;
                let result = self.builder.fresh_value();
                let ret_ty = IrType::Scalar(DType::Bool);
                self.builder
                    .push_instr(mk(result, haystack, second), Some(ret_ty.clone()));
                return Ok((result, ret_ty));
            }
        }

        // Built-in string transforms: to_upper(s), to_lower(s), trim(s)
        {
            let str_xform: Option<fn(ValueId, ValueId) -> IrInstr> = match dispatch_name {
                "to_upper" => Some(|result, operand| IrInstr::StrToUpper { result, operand }),
                "to_lower" => Some(|result, operand| IrInstr::StrToLower { result, operand }),
                "trim" => Some(|result, operand| IrInstr::StrTrim { result, operand }),
                _ => None,
            };
            if let Some(mk) = str_xform {
                if args.len() != 1 {
                    return Err(builtin_arity_error(
                        &format!("{}() requires exactly 1 argument", callee.name),
                        args.len(),
                        span,
                    ));
                }
                let (operand, _) = self.lower_expr(&args[0])?;
                let result = self.builder.fresh_value();
                let ret_ty = IrType::Str;
                self.builder
                    .push_instr(mk(result, operand), Some(ret_ty.clone()));
                return Ok((result, ret_ty));
            }
        }

        // Built-in: repeat(s, n) → StrRepeat
        if dispatch_name == "repeat" {
            if args.len() != 2 {
                return Err(builtin_arity_error(
                    "repeat() requires exactly 2 arguments",
                    args.len(),
                    span,
                ));
            }
            let (operand, _) = self.lower_expr(&args[0])?;
            let (count, _) = self.lower_expr(&args[1])?;
            let result = self.builder.fresh_value();
            let ret_ty = IrType::Str;
            self.builder.push_instr(
                IrInstr::StrRepeat {
                    result,
                    operand,
                    count,
                },
                Some(ret_ty.clone()),
            );
            return Ok((result, ret_ty));
        }

        // Built-in bitwise binary: band(a,b), bor(a,b), bxor(a,b), shl(a,b),
        // shr(a,b); and wrap-around arithmetic, wrapping_add(a,b) etc.
        {
            let bitbin: Option<BinOp> = match dispatch_name {
                "band" => Some(BinOp::BitAnd),
                "bor" => Some(BinOp::BitOr),
                "bxor" => Some(BinOp::BitXor),
                "shl" => Some(BinOp::Shl),
                "shr" => Some(BinOp::Shr),
                "wrapping_add" => Some(BinOp::WrapAdd),
                "wrapping_sub" => Some(BinOp::WrapSub),
                "wrapping_mul" => Some(BinOp::WrapMul),
                _ => None,
            };
            if let Some(op) = bitbin {
                if args.len() != 2 {
                    return Err(builtin_arity_error(
                        &format!("{}() requires exactly 2 arguments", callee.name),
                        args.len(),
                        span,
                    ));
                }
                let (lhs, lhs_ty) = self.lower_expr(&args[0])?;
                let (rhs, _) = self.lower_expr(&args[1])?;
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::BinOp {
                        result,
                        op,
                        lhs,
                        rhs,
                        ty: lhs_ty.clone(),
                    },
                    Some(lhs_ty.clone()),
                );
                return Ok((result, lhs_ty));
            }
        }

        // Built-in bitwise unary: bitnot(x)
        if dispatch_name == "bitnot" {
            if args.len() != 1 {
                return Err(builtin_arity_error(
                    "bitnot() requires exactly 1 argument",
                    args.len(),
                    span,
                ));
            }
            let (operand, op_ty) = self.lower_expr(&args[0])?;
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::UnaryOp {
                    result,
                    op: ScalarUnaryOp::BitNot,
                    operand,
                    ty: op_ty.clone(),
                },
                Some(op_ty.clone()),
            );
            return Ok((result, op_ty));
        }

        // Built-in math unary: sqrt, abs, floor, ceil, sin, cos, tan, exp, log, log2, round, sign
        {
            let math_unary: Option<ScalarUnaryOp> = match dispatch_name {
                "sqrt" => Some(ScalarUnaryOp::Sqrt),
                "abs" => Some(ScalarUnaryOp::Abs),
                "floor" => Some(ScalarUnaryOp::Floor),
                "ceil" => Some(ScalarUnaryOp::Ceil),
                "sin" => Some(ScalarUnaryOp::Sin),
                "cos" => Some(ScalarUnaryOp::Cos),
                "tan" => Some(ScalarUnaryOp::Tan),
                "exp" => Some(ScalarUnaryOp::Exp),
                "log" => Some(ScalarUnaryOp::Log),
                "log2" => Some(ScalarUnaryOp::Log2),
                "round" => Some(ScalarUnaryOp::Round),
                "sign" => Some(ScalarUnaryOp::Sign),
                _ => None,
            };
            if let Some(op) = math_unary {
                if args.len() != 1 {
                    return Err(builtin_arity_error(
                        &format!("{}() requires exactly 1 argument", callee.name),
                        args.len(),
                        span,
                    ));
                }
                let (operand, op_ty) = self.lower_expr(&args[0])?;
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::UnaryOp {
                        result,
                        op,
                        operand,
                        ty: op_ty.clone(),
                    },
                    Some(op_ty.clone()),
                );
                let tape_op = match op {
                    ScalarUnaryOp::Sqrt => Some("sqrt"),
                    ScalarUnaryOp::Abs => Some("abs"),
                    ScalarUnaryOp::Sin => Some("sin"),
                    ScalarUnaryOp::Cos => Some("cos"),
                    ScalarUnaryOp::Exp => Some("exp"),
                    ScalarUnaryOp::Log => Some("log"),
                    _ => None,
                };
                if let Some(tape_op) = tape_op {
                    self.maybe_record_tape_result(result, &op_ty, tape_op, &[operand]);
                }
                return Ok((result, op_ty));
            }
        }

        // clamp(x, lo, hi) → min(max(x, lo), hi)
        if dispatch_name == "clamp" {
            if args.len() != 3 {
                return Err(builtin_arity_error(
                    "clamp() requires exactly 3 arguments",
                    args.len(),
                    span,
                ));
            }
            let (x, x_ty) = self.lower_expr(&args[0])?;
            let (lo, _) = self.lower_expr(&args[1])?;
            let (hi, _) = self.lower_expr(&args[2])?;
            let inner = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::BinOp {
                    result: inner,
                    op: BinOp::Max,
                    lhs: x,
                    rhs: lo,
                    ty: x_ty.clone(),
                },
                Some(x_ty.clone()),
            );
            let outer = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::BinOp {
                    result: outer,
                    op: BinOp::Min,
                    lhs: inner,
                    rhs: hi,
                    ty: x_ty.clone(),
                },
                Some(x_ty.clone()),
            );
            return Ok((outer, x_ty));
        }

        // Built-in math binary: pow(base, exp), min(a, b), max(a, b)
        {
            let math_bin: Option<BinOp> = match dispatch_name {
                "pow" => Some(BinOp::Pow),
                "min" => Some(BinOp::Min),
                "max" => Some(BinOp::Max),
                _ => None,
            };
            if let Some(op) = math_bin {
                if args.len() != 2 {
                    return Err(builtin_arity_error(
                        &format!("{}() requires exactly 2 arguments", callee.name),
                        args.len(),
                        span,
                    ));
                }
                let (lhs, lhs_ty) = self.lower_expr(&args[0])?;
                let (rhs, _) = self.lower_expr(&args[1])?;
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::BinOp {
                        result,
                        op,
                        lhs,
                        rhs,
                        ty: lhs_ty.clone(),
                    },
                    Some(lhs_ty.clone()),
                );
                if matches!(op, BinOp::Pow) {
                    self.maybe_record_tape_result(result, &lhs_ty, "pow", &[lhs, rhs]);
                }
                return Ok((result, lhs_ty));
            }
        }

        // ── Functional list operations: expand inline instead of BuiltinCall ──
        // These use CallClosure which the codegen handles natively, so they work
        // with both the interpreter and the binary/LLVM backend.
        if matches!(
            callee.name.as_str(),
            "list_map" | "list_filter" | "list_reduce" | "list_any" | "list_all"
        ) {
            match dispatch_name {
                "list_map" => {
                    // list_map(list, closure)
                    if args.len() != 2 {
                        return Err(builtin_arity_error(
                            "list_map() requires 2 arguments: (list, closure)",
                            args.len(),
                            span,
                        ));
                    }
                    let (base_val, base_ty) = self.lower_expr(&args[0])?;
                    let elem_ty = match &base_ty {
                        IrType::List(inner) => *inner.clone(),
                        _ => IrType::Scalar(DType::I64),
                    };
                    return self.lower_list_map(base_val, elem_ty, &args[1..], span);
                }
                "list_filter" => {
                    // list_filter(list, closure)
                    if args.len() != 2 {
                        return Err(builtin_arity_error(
                            "list_filter() requires 2 arguments: (list, closure)",
                            args.len(),
                            span,
                        ));
                    }
                    let (base_val, base_ty) = self.lower_expr(&args[0])?;
                    let elem_ty = match &base_ty {
                        IrType::List(inner) => *inner.clone(),
                        _ => IrType::Scalar(DType::I64),
                    };
                    return self.lower_list_filter(base_val, elem_ty, &args[1..], span);
                }
                "list_reduce" => {
                    // list_reduce(list, initial, closure)
                    if args.len() != 3 {
                        return Err(builtin_arity_error(
                            "list_reduce() requires 3 arguments: (list, initial, closure)",
                            args.len(),
                            span,
                        ));
                    }
                    let (base_val, base_ty) = self.lower_expr(&args[0])?;
                    let elem_ty = match &base_ty {
                        IrType::List(inner) => *inner.clone(),
                        _ => IrType::Scalar(DType::I64),
                    };
                    return self.lower_list_fold(base_val, elem_ty, &args[1..], span);
                }
                "list_any" => {
                    // list_any(list, closure)
                    if args.len() != 2 {
                        return Err(builtin_arity_error(
                            "list_any() requires 2 arguments: (list, closure)",
                            args.len(),
                            span,
                        ));
                    }
                    let (base_val, base_ty) = self.lower_expr(&args[0])?;
                    let elem_ty = match &base_ty {
                        IrType::List(inner) => *inner.clone(),
                        _ => IrType::Scalar(DType::I64),
                    };
                    return self.lower_list_any(base_val, elem_ty, &args[1..], span);
                }
                "list_all" => {
                    // list_all(list, closure)
                    if args.len() != 2 {
                        return Err(builtin_arity_error(
                            "list_all() requires 2 arguments: (list, closure)",
                            args.len(),
                            span,
                        ));
                    }
                    let (base_val, base_ty) = self.lower_expr(&args[0])?;
                    let elem_ty = match &base_ty {
                        IrType::List(inner) => *inner.clone(),
                        _ => IrType::Scalar(DType::I64),
                    };
                    return self.lower_list_all(base_val, elem_ty, &args[1..], span);
                }
                _ => unreachable!(),
            }
        }

        // ── Phase 104: New runtime builtins (HTTP, JSON, Regex, DateTime, OS, etc.) ──
        // NOTE: set_*, json_parse, path_exists are NOT here — they are stdlib .iris functions.
        {
            let builtin_info: Option<(&str, IrType)> = runtime_builtin(dispatch_name);

            // Special handling: chan_try_recv — use the channel's element type.
            if dispatch_name == "chan_try_recv" {
                if args.len() != 1 {
                    return Err(builtin_arity_error(
                        "chan_try_recv() requires exactly 1 argument",
                        args.len(),
                        span,
                    ));
                }
                let (chan_val, chan_ty) = self.lower_expr(&args[0])?;
                let elem_ty = match &chan_ty {
                    IrType::Chan(elem) => (**elem).clone(),
                    other => {
                        return Err(LowerError::TypeMismatch {
                            expected: "channel".into(),
                            found: format!("{}", other),
                            span,
                        })
                    }
                };
                let ret_ty = IrType::Option(Box::new(elem_ty));
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::BuiltinCall {
                        result,
                        name: "chan_try_recv".to_string(),
                        args: vec![chan_val],
                        result_ty: ret_ty.clone(),
                    },
                    Some(ret_ty.clone()),
                );
                return Ok((result, ret_ty));
            }

            // Special handling: recv_timeout(ch, ms) — returns option<elem_ty>
            if dispatch_name == "recv_timeout" {
                if args.len() != 2 {
                    return Err(builtin_arity_error(
                        "recv_timeout() requires 2 arguments (channel, timeout_ms)",
                        args.len(),
                        span,
                    ));
                }
                let (chan_val, chan_ty) = self.lower_expr(&args[0])?;
                let (timeout_ms, _) = self.lower_expr(&args[1])?;
                let elem_ty = match (self.chan_elem_types.get(&chan_val), &chan_ty) {
                    (Some(t), _) => t.clone(),
                    (None, IrType::Chan(elem)) => (**elem).clone(),
                    (None, other) => {
                        return Err(LowerError::TypeMismatch {
                            expected: "channel".into(),
                            found: other.to_string(),
                            span,
                        })
                    }
                };
                let ret_ty = IrType::Option(Box::new(elem_ty));
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::BuiltinCall {
                        result,
                        name: "recv_timeout".to_string(),
                        args: vec![chan_val, timeout_ms],
                        result_ty: ret_ty.clone(),
                    },
                    Some(ret_ty.clone()),
                );
                return Ok((result, ret_ty));
            }

            // async_recv(ch): a task completed with the channel's next value.
            if dispatch_name == "async_recv" {
                if args.len() != 1 {
                    return Err(builtin_arity_error(
                        "async_recv() requires exactly 1 argument (channel)",
                        args.len(),
                        span,
                    ));
                }
                let (chan_val, chan_ty) = self.lower_expr(&args[0])?;
                let ret_ty = match chan_ty {
                    IrType::Chan(elem) => IrType::Task(elem),
                    other => {
                        return Err(LowerError::TypeMismatch {
                            expected: "chan<T>".to_owned(),
                            found: other.to_string(),
                            span,
                        })
                    }
                };
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::BuiltinCall {
                        result,
                        name: "async_recv".to_string(),
                        args: vec![chan_val],
                        result_ty: ret_ty.clone(),
                    },
                    Some(ret_ty.clone()),
                );
                return Ok((result, ret_ty));
            }

            // Special handling: weak_ref(v) — remember what it points at, so
            // `weak_upgrade` can hand back a correctly typed payload (#55).
            if dispatch_name == "weak_ref" {
                if args.len() != 1 {
                    return Err(builtin_arity_error(
                        "weak_ref() requires exactly 1 argument",
                        args.len(),
                        span,
                    ));
                }
                let (target, target_ty) = self.lower_expr(&args[0])?;
                let ret_ty = IrType::WeakRef(Box::new(target_ty));
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::BuiltinCall {
                        result,
                        name: "weak_ref".to_string(),
                        args: vec![target],
                        result_ty: ret_ty.clone(),
                    },
                    Some(ret_ty.clone()),
                );
                return Ok((result, ret_ty));
            }

            // Special handling: weak_upgrade(w) — returns option<referent>
            if dispatch_name == "weak_upgrade" {
                if args.len() != 1 {
                    return Err(builtin_arity_error(
                        "weak_upgrade() requires exactly 1 argument",
                        args.len(),
                        span,
                    ));
                }
                let (weak_val, weak_ty) = self.lower_expr(&args[0])?;
                // Carry the referent's type through, rather than `Infer`. With
                // `Infer` the downstream `unwrap` defaulted to `i64`, so a
                // weak-referenced list came back as an integer and
                // `iris_list_len(ptr %v)` was handed an `i64` -- invalid IR.
                // See known-issues #55.
                let inner = match &weak_ty {
                    IrType::WeakRef(t) => (**t).clone(),
                    other => {
                        return Err(LowerError::TypeMismatch {
                            expected: "weak_ref<T>".into(),
                            found: other.to_string(),
                            span,
                        })
                    }
                };
                let ret_ty = IrType::Option(Box::new(inner));
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::BuiltinCall {
                        result,
                        name: "weak_upgrade".to_string(),
                        args: vec![weak_val],
                        result_ty: ret_ty.clone(),
                    },
                    Some(ret_ty.clone()),
                );
                return Ok((result, ret_ty));
            }

            // `par_map(list<T>, fn(T) -> U)` preserves both generic element
            // types in IR. The old builtin table returned `list<Infer>`, so
            // native code could not generate a correctly typed callback
            // adapter and consumers guessed at the result ABI.
            if dispatch_name == "par_map" {
                if args.len() != 2 {
                    return Err(LowerError::rich(
                        "E0109",
                        crate::diagnostics::Diagnostic::error(
                            "`par_map` takes a list and a function".to_string(),
                        )
                        .with_primary_span(
                            (span).start.0,
                            (span).end.0,
                            None,
                        ),
                    ));
                }
                let (list, list_ty) = self.lower_expr(&args[0])?;
                let (mapper, mapper_ty) = self.lower_expr(&args[1])?;
                let input_ty = match &list_ty {
                    IrType::List(elem) => (**elem).clone(),
                    other => {
                        return Err(LowerError::TypeMismatch {
                            expected: "list".into(),
                            found: format!("{}", other),
                            span,
                        })
                    }
                };
                let output_ty = match &mapper_ty {
                    IrType::Fn { params, ret } => {
                        if let Some(parameter_ty) = params.last() {
                            if parameter_ty != &input_ty {
                                return Err(LowerError::TypeMismatch {
                                    expected: format!("fn({}) -> _", input_ty),
                                    found: format!("{}", mapper_ty),
                                    span,
                                });
                            }
                        }
                        (**ret).clone()
                    }
                    other => {
                        return Err(LowerError::TypeMismatch {
                            expected: format!("fn({}) -> _", input_ty),
                            found: format!("{}", other),
                            span,
                        })
                    }
                };
                let ret_ty = IrType::List(Box::new(output_ty));
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::BuiltinCall {
                        result,
                        name: "par_map".to_owned(),
                        args: vec![list, mapper],
                        result_ty: ret_ty.clone(),
                    },
                    Some(ret_ty.clone()),
                );
                return Ok((result, ret_ty));
            }

            if let Some((rt_name, ret_ty)) = builtin_info {
                let mut arg_vals = Vec::with_capacity(args.len());
                let mut arg_tys = Vec::with_capacity(args.len());
                for arg in args {
                    let (v, ty) = self.lower_expr(arg)?;
                    arg_vals.push(v);
                    arg_tys.push(ty);
                }
                if rt_name == "async_any" {
                    let tasks_ok = match arg_tys.as_slice() {
                        [] => false,
                        [IrType::List(elem)] => matches!(**elem, IrType::Task(_)),
                        tys => tys.iter().all(|ty| matches!(ty, IrType::Task(_))),
                    };
                    if !tasks_ok {
                        return Err(LowerError::TypeMismatch {
                            expected: "one or more task<T> arguments, or a list<task<T>>"
                                .to_owned(),
                            found: arg_tys
                                .iter()
                                .map(|t| t.to_string())
                                .collect::<Vec<_>>()
                                .join(", "),
                            span,
                        });
                    }
                }
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::BuiltinCall {
                        result,
                        name: rt_name.to_string(),
                        args: arg_vals,
                        result_ty: ret_ty.clone(),
                    },
                    Some(ret_ty.clone()),
                );
                return Ok((result, ret_ty));
            }
        }

        // Const function call — evaluate at compile time if all args are literals.
        if let Some(const_fn) = self.generic_fns.get(&callee_name).cloned() {
            if const_fn.is_const {
                if let Some(result_val) = self.try_eval_const_fn(&const_fn, args) {
                    let ret_ty = self.resolve_ty(&const_fn.return_ty)?;
                    match &ret_ty {
                        IrType::Scalar(DType::I64) => {
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::ConstInt {
                                    result,
                                    value: result_val,
                                    ty: ret_ty.clone(),
                                },
                                Some(ret_ty.clone()),
                            );
                            return Ok((result, ret_ty));
                        }
                        IrType::Scalar(DType::F64) => {
                            let result = self.builder.fresh_value();
                            let bits = result_val as u64;
                            let float_val = f64::from_bits(bits);
                            self.builder.push_instr(
                                IrInstr::ConstFloat {
                                    result,
                                    value: float_val,
                                    ty: ret_ty.clone(),
                                },
                                Some(ret_ty.clone()),
                            );
                            return Ok((result, ret_ty));
                        }
                        IrType::Scalar(DType::Bool) => {
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::ConstBool {
                                    result,
                                    value: result_val != 0,
                                },
                                Some(ret_ty.clone()),
                            );
                            return Ok((result, ret_ty));
                        }
                        _ => {} // Fall through to normal call for non-scalar return types
                    }
                }
            }
        }

        // Generic function call — monomorphize on demand.
        if let Some(generic_fn) = self.generic_fns.get(&callee_name).cloned() {
            // Lower each argument and collect concrete types.
            let mut arg_vals = Vec::with_capacity(args.len());
            let mut arg_tys = Vec::with_capacity(args.len());
            for arg in args {
                let (v, ty) = self.lower_expr(arg)?;
                arg_vals.push(v);
                arg_tys.push(ty);
            }

            let expected_ret = self
                .binding_ty
                .clone()
                .or_else(|| self.expected_expr_ty.clone());
            let subs = infer_generic_fn_subs(
                &generic_fn,
                &arg_tys,
                expected_ret.as_ref(),
                self.module,
                &callee_name,
                span,
            )?;

            // Resolve the concrete return type using resolve_ast_type_with_subs
            // which handles both Named("T") and Generic { name, args } with subs.
            let mut concrete_ret =
                resolve_ast_type_with_subs(&generic_fn.return_ty, &subs, self.module)?;
            if generic_fn.is_async {
                concrete_ret = IrType::Task(Box::new(concrete_ret));
            }
            if let Some(tmpl) = self.module.generic_templates.get(&callee_name) {
                let mut mono_args =
                    Vec::with_capacity(tmpl.type_params.len() + tmpl.hkt_params.len());
                for tp in &tmpl.type_params {
                    mono_args.push(subs.get(tp).cloned().unwrap_or(IrType::Infer));
                }
                for hp in &tmpl.hkt_params {
                    let ctor_ty = match subs.get(hp) {
                        Some(IrType::Struct { name, .. }) => IrType::TypeParam(name.clone()),
                        Some(other) => other.clone(),
                        None => IrType::TypeParam(hp.clone()),
                    };
                    mono_args.push(ctor_ty);
                }
                let result = self.builder.fresh_value();
                self.builder.push_generic_call(
                    &callee_name,
                    mono_args,
                    IrInstr::Call {
                        result: Some(result),
                        callee: callee_name.clone(),
                        args: arg_vals,
                        result_ty: Some(concrete_ret.clone()),
                    },
                    Some(concrete_ret.clone()),
                );
                return Ok((result, concrete_ret));
            }

            // Generate mangled name: e.g. `max_val__i64` for T=i64.
            let mangle = mangle_param_list(&generic_fn.type_params, &subs);
            let mangled = format!("{}__{}", callee_name, mangle);

            // Register the return type for the mangled name.
            self.mono_sigs
                .borrow_mut()
                .insert(mangled.clone(), concrete_ret.clone());

            // Monomorphize if not already done.
            let mono_hit = self.mono_cache.borrow().contains(&mangled);
            crate::lower::mono::record_instantiation(&mangled, mono_hit);
            if !mono_hit {
                self.mono_cache.borrow_mut().insert(mangled.clone());

                // Build a renamed copy of the generic function.
                let mut mono_fn = generic_fn.clone();
                mono_fn.name.name = mangled.clone();
                mono_fn.type_params = Vec::new(); // no longer generic

                // Lower the specialized function.
                let fn_sigs_ref = self.fn_sigs;
                let (ir_func, extra_lifted) = lower_function_with_generics_and_subs(
                    &mono_fn,
                    self.module,
                    fn_sigs_ref,
                    &self.const_defs,
                    self.generic_fns.clone(),
                    self.mono_cache.clone(),
                    self.mono_sigs.clone(),
                    subs,
                    self.trait_dispatch.clone(),
                    self.fn_defaults.clone(),
                    self.fn_param_names.clone(),
                    self.fn_param_types.clone(),
                    self.lambda_counter.clone(),
                )?;

                self.lifted_fns.borrow_mut().push(ir_func);
                self.lifted_fns.borrow_mut().extend(extra_lifted);
            }

            // Emit the call to the specialized function.
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::Call {
                    result: Some(result),
                    callee: mangled,
                    args: arg_vals,
                    result_ty: Some(concrete_ret.clone()),
                },
                Some(concrete_ret.clone()),
            );
            return Ok((result, concrete_ret));
        }

        // Trait method dispatch — static dispatch based on first arg's concrete type.
        if let Some(impls) = self.trait_dispatch.get(&callee_name).cloned() {
            if !args.is_empty() {
                let (first_val, first_ty) = self.lower_expr(&args[0])?;
                let type_key = ir_type_dispatch_name(&first_ty);
                if let Some((_, mangled)) = impls
                    .iter()
                    .find(|(dispatch_ty, _)| ir_type_dispatch_name(dispatch_ty) == type_key)
                {
                    let mangled = mangled.clone();
                    let ret_ty = self.sig_of(&mangled, span)?;
                    let mut arg_vals = vec![first_val];
                    for arg in &args[1..] {
                        let (v, _) = self.lower_expr(arg)?;
                        arg_vals.push(v);
                    }
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::Call {
                            result: Some(result),
                            callee: mangled,
                            args: arg_vals,
                            result_ty: Some(ret_ty.clone()),
                        },
                        Some(ret_ty.clone()),
                    );
                    return Ok((result, ret_ty));
                }
            }
        }

        // ML/AI intrinsics (Phases 77–80)
        match dispatch_name {
            "zeros" => return self.lower_ml_zeros(args, span),
            "ones" => return self.lower_ml_ones(args, span),
            "fill" => return self.lower_ml_fill(args, span),
            "linspace" => return self.lower_ml_linspace(args, span),
            "arange" => return self.lower_ml_arange(args, span),
            "list_sum" => return self.lower_ml_list_sum(args, span),
            "list_mean" => return self.lower_ml_list_mean(args, span),
            "list_max_val" => return self.lower_ml_list_max_val(args, span),
            "list_min_val" => return self.lower_ml_list_min_val(args, span),
            "list_std" => return self.lower_ml_list_std(args, span),
            "list_norm" => return self.lower_ml_list_norm(args, span),
            "list_dot" => return self.lower_ml_list_dot(args, span),
            "list_add" => return self.lower_ml_list_binop(args, span, BinOp::Add),
            "list_sub" => return self.lower_ml_list_binop(args, span, BinOp::Sub),
            "list_mul_elem" => return self.lower_ml_list_binop(args, span, BinOp::Mul),
            "list_scale" => return self.lower_ml_list_scale(args, span),
            "list_relu" => return self.lower_ml_list_relu(args, span),
            "list_sigmoid" => return self.lower_ml_list_sigmoid(args, span),
            "list_softmax" => return self.lower_ml_list_softmax(args, span),
            "mse_loss" => return self.lower_ml_mse_loss(args, span),
            "cross_entropy" => return self.lower_ml_cross_entropy(args, span),
            "list_axpy" => return self.lower_ml_list_axpy(args, span),
            "sgd_step" => return self.lower_ml_sgd_step(args, span),
            // Phase 82: BLAS-named bindings
            "list_dot_blas" => return self.lower_ml_list_dot(args, span),
            "list_axpy_blas" => return self.lower_ml_list_axpy(args, span),
            "list_scale_blas" => return self.lower_ml_list_scale(args, span),
            "matmul" => return self.lower_ml_matmul(args, span),
            _ => {}
        }

        // General function call — look up the callee's return type from
        // pre-collected signatures so the result has a concrete type.
        let ret_ty = self
            .fn_sigs
            .get(&callee_name)
            .cloned()
            .or_else(|| self.mono_sigs.borrow().get(&callee_name).cloned())
            .or_else(|| {
                self.module
                    .extern_fns
                    .iter()
                    .find(|e| e.name == callee_name)
                    .map(|e| e.ret_ty.clone())
            });

        // No signature: the callee is not defined anywhere.
        let Some(ret_ty) = ret_ty else {
            // A variable of that name exists: it is being called, not missing.
            if let Some((_, ty)) = self.scope.get(&callee_name) {
                let ty = hints::type_name(ty);
                return Err(LowerError::rich(
                    "E0118",
                    crate::diagnostics::Diagnostic::error(format!(
                        "`{}` is not a function: it has type `{ty}`",
                        callee.name
                    ))
                    .with_primary_span(
                        span.start.0,
                        span.end.0,
                        Some("called here".into()),
                    ),
                ));
            }
            return Err(self.unknown_name(&callee.name, callee.span, Some((span, args))));
        };

        // Declared parameter types, so a concrete struct passed where a
        // `dyn Trait` is expected becomes a trait object here. Without this it
        // reached `DynCall` as a bare struct and failed at *runtime* with
        // "DynCall on non-trait-object" -- a mistake knowable at compile time,
        // deferred to execution. See known-issues #18.
        let callee_param_tys: Option<Vec<IrType>> = self.fn_param_types.get(&callee_name).cloned();

        // The number of arguments must fit the parameters. A missing argument
        // used to reach the IR as an undefined value and surface, unlocated,
        // as an internal verifier failure.
        if let Some(names) = self.fn_param_names.get(&callee_name) {
            let has_splat = args.iter().any(|a| matches!(a, AstExpr::Splat { .. }));
            let required = match self.fn_defaults.get(&callee_name) {
                Some(defs) => defs.iter().filter(|d| d.is_none()).count(),
                None => names.len(),
            };
            if !has_splat && (args.len() < required || args.len() > names.len()) {
                return Err(self.arity_error(
                    &callee_name,
                    &callee.name,
                    required,
                    names.len(),
                    args.len(),
                    span,
                ));
            }
        }

        // Build argument list, expanding splat args (`..expr`).
        let defaults = self.fn_defaults.get(&callee_name).cloned();
        let mut arg_vals = Vec::new();
        for arg in args {
            if let AstExpr::Splat { expr, .. } = arg {
                let (arr_val, arr_ty) = self.lower_expr(expr)?;
                let (elem_ty, arr_len) = match &arr_ty {
                    IrType::Array { elem, len } => ((**elem).clone(), *len),
                    _ => {
                        return Err(LowerError::rich(
                            "E0110",
                            crate::diagnostics::Diagnostic::error(
                                "`..` in a call needs an array of known size".to_string(),
                            )
                            .with_primary_span(
                                (arg.span()).start.0,
                                (arg.span()).end.0,
                                None,
                            ),
                        ));
                    }
                };
                for i in 0..arr_len {
                    let idx_val = self.builder.fresh_value();
                    let idx_ty = IrType::Scalar(DType::I64);
                    self.builder.push_instr(
                        IrInstr::ConstInt {
                            result: idx_val,
                            value: i as i64,
                            ty: idx_ty.clone(),
                        },
                        Some(idx_ty.clone()),
                    );
                    let elem_val = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::ArrayLoad {
                            result: elem_val,
                            array: arr_val,
                            index: idx_val,
                            elem_ty: elem_ty.clone(),
                        },
                        Some(elem_ty.clone()),
                    );
                    arg_vals.push(elem_val);
                }
            } else {
                let (v, vty) = self.lower_expr(arg)?;
                let slot = arg_vals.len();
                if let Some(param_ty) = callee_param_tys.as_ref().and_then(|ts| ts.get(slot)) {
                    if hints::incompatible(&vty, param_ty) {
                        return Err(self.argument_type_error(
                            &callee.name,
                            slot,
                            arg,
                            &vty,
                            param_ty,
                        ));
                    }
                }
                let expected = callee_param_tys
                    .as_ref()
                    .and_then(|ts| ts.get(slot))
                    .filter(|t| matches!(t, IrType::TraitObject { .. }))
                    .cloned();
                let v = match expected {
                    Some(exp) => self.coerce_to_trait_object(v, vty, &exp, arg.span())?.0,
                    None => v,
                };
                arg_vals.push(v);
            }
        }
        if let Some(ref defs) = defaults {
            for default_expr in defs.iter().skip(arg_vals.len()).flatten() {
                let (v, _) = self.lower_expr(default_expr)?;
                arg_vals.push(v);
            }
        }

        // Check if this is an extern (C-linkage) function.
        let is_extern = self.module.extern_fns.iter().any(|e| e.name == callee_name);
        if is_extern {
            if !crate::lower::types::returns_a_value(&ret_ty) {
                self.builder.push_instr(
                    IrInstr::CallExtern {
                        result: None,
                        name: callee_name.clone(),
                        args: arg_vals,
                        ret_ty: ret_ty.clone(),
                    },
                    None,
                );
                return Ok(self.emit_unit_value());
            }
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::CallExtern {
                    result: Some(result),
                    name: callee_name.clone(),
                    args: arg_vals,
                    ret_ty: ret_ty.clone(),
                },
                Some(ret_ty.clone()),
            );
            return Ok((result, ret_ty));
        }

        // A call carrying a taped argument is lowered inline, so the tape graph
        // continues through the callee's body instead of stopping at its
        // signature. Without this, `sq(x)` on a taped `x` produced an untaped
        // result and `backward` was rejected outright (#49).
        if arg_vals.iter().any(|v| self.tape_nodes.contains_key(v)) {
            if let Some(inlined) = self.try_lower_taped_call(&callee_name, &arg_vals)? {
                return Ok(inlined);
            }
        }

        let result = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::Call {
                result: Some(result),
                callee: callee_name,
                args: arg_vals,
                result_ty: Some(ret_ty.clone()),
            },
            Some(ret_ty.clone()),
        );
        Ok((result, ret_ty))
    }

    /// Lowers `callee`'s body directly into the current function, with its
    /// parameters bound to `arg_vals`.
    ///
    /// Returns `Ok(None)` when the callee is not a candidate, in which case the
    /// caller emits an ordinary call and the handle is lost as before -- a
    /// rejected program rather than a wrong gradient.
    ///
    /// Declined deliberately when:
    /// * the body uses any form outside the inliner's whitelist -- notably
    ///   `return`, which would terminate the *caller*;
    /// * the function is already being inlined on this path -- a recursive taped
    ///   call would otherwise expand forever;
    /// * the arity does not match, so defaults and splats keep the normal path.
    pub(super) fn try_lower_taped_call(
        &mut self,
        callee_name: &str,
        arg_vals: &[ValueId],
    ) -> Result<Option<(ValueId, IrType)>, LowerError> {
        if self.taped_inline_stack.iter().any(|n| n == callee_name) {
            return Ok(None);
        }
        let Some(func) = CURRENT_FN_ASTS.with(|m| m.borrow().get(callee_name).cloned()) else {
            return Ok(None);
        };
        if func.params.len() != arg_vals.len() || !taped_inline_ok_block(&func.body) {
            return Ok(None);
        }

        let saved_scope = self.scope.clone();
        self.taped_inline_stack.push(callee_name.to_owned());
        for (param, val) in func.params.iter().zip(arg_vals.iter()) {
            // Bind the parameter to the caller's *value*, not a copy. That is
            // what carries `tape_nodes` across: the body sees the very ValueId
            // whose tape node the caller recorded.
            let ty = self.resolve_ty(&param.ty)?;
            self.scope.insert(param.name.name.clone(), (*val, ty));
        }
        // A final `return expr` is equivalent to a tail expression for this
        // deliberately small inliner. Lower every preceding statement, then
        // lower the returned expression without emitting `IrInstr::Return` --
        // that instruction would otherwise return from the caller. Returns in
        // any other position remain outside the whitelist below.
        let lowered = if func.body.tail.is_none() {
            match func.body.stmts.split_last() {
                Some((
                    AstStmt::Return {
                        value: Some(value), ..
                    },
                    prefix,
                )) => {
                    let mut error = None;
                    for stmt in prefix {
                        if let Err(err) = self.lower_stmt(stmt) {
                            error = Some(err);
                            break;
                        }
                    }
                    match error {
                        Some(err) => Err(err),
                        None => self.lower_expr(value).map(Some),
                    }
                }
                _ => self.lower_block(&func.body),
            }
        } else {
            self.lower_block(&func.body)
        };
        self.taped_inline_stack.pop();
        self.scope = saved_scope;

        lowered
    }

    pub(super) fn lower_einsum(
        &mut self,
        args: &[AstExpr],
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        if args.is_empty() {
            return Err(builtin_arity_error(
                "einsum requires at least one argument (the notation string)",
                args.len(),
                span,
            ));
        }

        // First arg must be a string literal (the einsum notation).
        let notation = match synth_marker_thir_expr(&args[0]).as_ref().map(|e| &e.kind) {
            Some(crate::thir::ThirExprKind::Literal(crate::thir::ThirLiteral::Str(v))) => v.clone(),
            Some(_) => {
                return Err(LowerError::rich(
                    "E0126",
                    crate::diagnostics::Diagnostic::error(
                        "`einsum` needs a string literal as its first argument".to_string(),
                    )
                    .with_primary_span((span).start.0, (span).end.0, None),
                ));
            }
            None => match &args[0] {
                AstExpr::StringLit { value, .. } => value.clone(),
                other => {
                    return Err(LowerError::rich(
                        "E0126",
                        crate::diagnostics::Diagnostic::error(
                            "`einsum` needs a string literal as its first argument".to_string(),
                        )
                        .with_primary_span(
                            (other.span()).start.0,
                            (other.span()).end.0,
                            None,
                        ),
                    ));
                }
            },
        };

        // Remaining args are tensor inputs.
        let mut input_vals = Vec::new();
        let mut input_tys = Vec::new();
        for arg in &args[1..] {
            let (v, ty) = self.lower_expr(arg)?;
            input_vals.push(v);
            input_tys.push(ty);
        }

        // Derive result type from the einsum notation and input shapes.
        let result_ty = derive_einsum_result_type(&notation, &input_tys).ok_or_else(|| {
            self.undetermined(span, "the element type of this einsum's tensor inputs")
        })?;

        let result = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::TensorOp {
                result,
                op: TensorOp::Einsum {
                    notation: notation.clone(),
                },
                inputs: input_vals.clone(),
                result_ty: result_ty.clone(),
            },
            Some(result_ty.clone()),
        );
        self.maybe_record_tape_result(result, &result_ty, "einsum", &input_vals);
        Ok((result, result_ty))
    }
}
