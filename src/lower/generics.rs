//! Generic instantiation, type-argument inference, and monomorphised function lowering.

use std::collections::{HashMap, HashSet};

use crate::error::LowerError;
use crate::ir::block::BlockId;
use crate::ir::function::Param;
use crate::ir::instr::IrInstr;
use crate::ir::module::{IrFunctionBuilder, IrModule};
use crate::ir::types::{DType, IrType};
use crate::ir::value::ValueId;
use crate::parser::ast::{AstBlock, AstExpr, AstFunction, AstScalarKind, AstStmt, AstType};
use crate::parser::lexer::Span;

use super::{
    ast_attr_to_ir_attr, is_inferred_return, mangle_ir_type, parse_mangled_type,
    resolve_ast_type_with_subs, resolve_brought_name, resolve_mangled_struct, returns_a_value,
    synth_marker_at, thir_block_migratable, typeck, undetermined_return, Lowerer, SynthMarker,
    ASYNC_POLL_ATTR, ASYNC_POLL_PREFIX, ASYNC_TASK_PARAM,
};

pub(super) fn constructor_marker(name: &str) -> IrType {
    IrType::Struct {
        name: name.to_string(),
        fields: Vec::new(),
    }
}

pub(super) fn split_generic_param_names(
    params: &[crate::parser::ast::AstGenericParam],
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut type_params = Vec::new();
    let mut hkt_params = Vec::new();
    let mut const_params = Vec::new();
    for p in params {
        match p {
            crate::parser::ast::AstGenericParam::Type(n, _, _) => type_params.push(n.clone()),
            crate::parser::ast::AstGenericParam::Hkt(n, _, _, _) => hkt_params.push(n.clone()),
            crate::parser::ast::AstGenericParam::Const { name, .. } => {
                const_params.push(name.clone())
            }
        }
    }
    (type_params, hkt_params, const_params)
}

pub(super) fn mangle_param_list(
    params: &[crate::parser::ast::AstGenericParam],
    subs: &HashMap<String, IrType>,
) -> String {
    let mut parts = Vec::with_capacity(params.len());
    for gp in params {
        let name = match gp {
            crate::parser::ast::AstGenericParam::Type(n, _, _)
            | crate::parser::ast::AstGenericParam::Hkt(n, _, _, _)
            | crate::parser::ast::AstGenericParam::Const { name: n, .. } => n,
        };
        if let Some(ty) = subs.get(name) {
            parts.push(mangle_ir_type(ty));
        }
    }
    parts.join("_")
}

pub(super) fn extract_from_ast_type(
    ast_ty: &crate::parser::ast::AstType,
    concrete_ty: &IrType,
    type_params: &[String],
    hkt_params: &[String],
    const_params: &[String],
    module: &IrModule,
    subs: &mut HashMap<String, IrType>,
) {
    if let crate::parser::ast::AstType::Generic {
        name: gname,
        args: gargs,
        ..
    } = ast_ty
    {
        if hkt_params.contains(gname) {
            match concrete_ty {
                IrType::List(inner) => {
                    subs.entry(gname.clone())
                        .or_insert_with(|| constructor_marker("list"));
                    if let Some(a0) = gargs.first() {
                        extract_from_ast_type(
                            a0,
                            inner,
                            type_params,
                            hkt_params,
                            const_params,
                            module,
                            subs,
                        );
                    }
                    return;
                }
                IrType::Option(inner) => {
                    subs.entry(gname.clone())
                        .or_insert_with(|| constructor_marker("option"));
                    if let Some(a0) = gargs.first() {
                        extract_from_ast_type(
                            a0,
                            inner,
                            type_params,
                            hkt_params,
                            const_params,
                            module,
                            subs,
                        );
                    }
                    return;
                }
                IrType::Map(k, v) => {
                    subs.entry(gname.clone())
                        .or_insert_with(|| constructor_marker("map"));
                    if let Some(a0) = gargs.first() {
                        extract_from_ast_type(
                            a0,
                            k,
                            type_params,
                            hkt_params,
                            const_params,
                            module,
                            subs,
                        );
                    }
                    if let Some(a1) = gargs.get(1) {
                        extract_from_ast_type(
                            a1,
                            v,
                            type_params,
                            hkt_params,
                            const_params,
                            module,
                            subs,
                        );
                    }
                    return;
                }
                IrType::Struct {
                    name: sname,
                    fields,
                } => {
                    let (base, rest) = match sname.split_once("__") {
                        Some((b, r)) => (b.to_string(), Some(r.to_string())),
                        None => (sname.clone(), None),
                    };
                    subs.entry(gname.clone())
                        .or_insert_with(|| constructor_marker(&base));
                    if let Some(rest) = rest {
                        let mut remaining = rest.as_str();
                        for garg in gargs.iter() {
                            if remaining.is_empty() {
                                break;
                            }
                            let (arg_ty, next) = parse_mangled_type(remaining);
                            remaining = next;
                            let resolved = resolve_mangled_struct(arg_ty, module);
                            extract_from_ast_type(
                                garg,
                                &resolved,
                                type_params,
                                hkt_params,
                                const_params,
                                module,
                                subs,
                            );
                        }
                    } else {
                        for (garg, (_, fty)) in gargs.iter().zip(fields.iter()) {
                            extract_from_ast_type(
                                garg,
                                fty,
                                type_params,
                                hkt_params,
                                const_params,
                                module,
                                subs,
                            );
                        }
                    }
                    return;
                }
                _ => {}
            }
        }
    }
    match ast_ty {
        crate::parser::ast::AstType::Named(n, _) => {
            if !subs.contains_key(n)
                && (type_params.contains(n)
                    || (const_params.contains(n) && matches!(concrete_ty, IrType::ConstInt(_))))
            {
                subs.insert(n.clone(), concrete_ty.clone());
            }
        }
        crate::parser::ast::AstType::Array {
            elem: elem_ast,
            len_expr,
            ..
        } => {
            if let IrType::Array {
                elem: elem_concrete,
                len,
            } = concrete_ty
            {
                extract_from_ast_type(
                    elem_ast,
                    elem_concrete,
                    type_params,
                    hkt_params,
                    const_params,
                    module,
                    subs,
                );
                if let Some(expr) = len_expr {
                    if let crate::parser::ast::AstExpr::Ident(id) = expr.as_ref() {
                        if const_params.contains(&id.name) && !subs.contains_key(&id.name) {
                            subs.insert(id.name.clone(), IrType::ConstInt(*len as i64));
                        }
                    }
                }
            }
        }
        crate::parser::ast::AstType::Generic {
            name: gname,
            args: gargs,
            ..
        } => {
            if let (IrType::Task(inner), [garg]) = (concrete_ty, gargs.as_slice()) {
                if gname == "task" {
                    extract_from_ast_type(
                        garg,
                        inner,
                        type_params,
                        hkt_params,
                        const_params,
                        module,
                        subs,
                    );
                }
            } else if let IrType::Struct {
                name: sname,
                fields,
            } = concrete_ty
            {
                let base = resolve_brought_name(gname, module);
                let tail = gname.rsplit("__").next().unwrap_or(gname);
                let mut matched_prefix = false;
                for prefix in [
                    format!("{base}__"),
                    format!("{gname}__"),
                    format!("{tail}__"),
                ] {
                    if let Some(rest) = sname.strip_prefix(prefix.as_str()) {
                        let mut remaining = rest;
                        for garg in gargs.iter() {
                            if remaining.is_empty() {
                                break;
                            }
                            let (arg_ty, next) = parse_mangled_type(remaining);
                            remaining = next;
                            let resolved = resolve_mangled_struct(arg_ty, module);
                            extract_from_ast_type(
                                garg,
                                &resolved,
                                type_params,
                                hkt_params,
                                const_params,
                                module,
                                subs,
                            );
                        }
                        matched_prefix = true;
                        break;
                    }
                }
                if !matched_prefix {
                    if let Some(tmpl) = module
                        .generic_struct_asts
                        .get(&base)
                        .or_else(|| module.generic_struct_asts.get(gname))
                    {
                        let mut tmpl_subs: HashMap<String, IrType> = HashMap::new();
                        let (st_t, st_h, st_c) = split_generic_param_names(&tmpl.type_params);
                        for (tf, (_, cf)) in tmpl.fields.iter().zip(fields.iter()) {
                            extract_from_ast_type(
                                &tf.ty,
                                cf,
                                &st_t,
                                &st_h,
                                &st_c,
                                module,
                                &mut tmpl_subs,
                            );
                        }
                        for (tp, garg) in tmpl.type_params.iter().zip(gargs.iter()) {
                            let pname = match tp {
                                crate::parser::ast::AstGenericParam::Type(n, _, _)
                                | crate::parser::ast::AstGenericParam::Hkt(n, _, _, _)
                                | crate::parser::ast::AstGenericParam::Const { name: n, .. } => n,
                            };
                            if let Some(val) = tmpl_subs.get(pname.as_str()) {
                                extract_from_ast_type(
                                    garg,
                                    val,
                                    type_params,
                                    hkt_params,
                                    const_params,
                                    module,
                                    subs,
                                );
                            }
                        }
                    } else {
                        for (garg, (_, fty)) in gargs.iter().zip(fields.iter()) {
                            extract_from_ast_type(
                                garg,
                                fty,
                                type_params,
                                hkt_params,
                                const_params,
                                module,
                                subs,
                            );
                        }
                    }
                }
            }
        }
        crate::parser::ast::AstType::List(inner_ast, _) => {
            if let IrType::List(inner_concrete) = concrete_ty {
                extract_from_ast_type(
                    inner_ast,
                    inner_concrete,
                    type_params,
                    hkt_params,
                    const_params,
                    module,
                    subs,
                );
            }
        }
        crate::parser::ast::AstType::Map(k_ast, v_ast, _) => {
            if let IrType::Map(k_concrete, v_concrete) = concrete_ty {
                extract_from_ast_type(
                    k_ast,
                    k_concrete,
                    type_params,
                    hkt_params,
                    const_params,
                    module,
                    subs,
                );
                extract_from_ast_type(
                    v_ast,
                    v_concrete,
                    type_params,
                    hkt_params,
                    const_params,
                    module,
                    subs,
                );
            }
        }
        crate::parser::ast::AstType::Option(inner_ast, _) => {
            if let IrType::Option(inner_concrete) = concrete_ty {
                extract_from_ast_type(
                    inner_ast,
                    inner_concrete,
                    type_params,
                    hkt_params,
                    const_params,
                    module,
                    subs,
                );
            }
        }
        crate::parser::ast::AstType::Result(ok_ast, err_ast, _) => {
            if let IrType::ResultType(ok_c, err_c) = concrete_ty {
                extract_from_ast_type(
                    ok_ast,
                    ok_c,
                    type_params,
                    hkt_params,
                    const_params,
                    module,
                    subs,
                );
                extract_from_ast_type(
                    err_ast,
                    err_c,
                    type_params,
                    hkt_params,
                    const_params,
                    module,
                    subs,
                );
            }
        }
        crate::parser::ast::AstType::Chan(inner_ast, _) => {
            if let IrType::Chan(inner_c) = concrete_ty {
                extract_from_ast_type(
                    inner_ast,
                    inner_c,
                    type_params,
                    hkt_params,
                    const_params,
                    module,
                    subs,
                );
            }
        }
        crate::parser::ast::AstType::Tuple(elems_ast, _) => {
            if let IrType::Tuple(elems_concrete) = concrete_ty {
                for (a, c) in elems_ast.iter().zip(elems_concrete.iter()) {
                    extract_from_ast_type(
                        a,
                        c,
                        type_params,
                        hkt_params,
                        const_params,
                        module,
                        subs,
                    );
                }
            }
        }
        crate::parser::ast::AstType::Fn {
            params: params_ast,
            ret: ret_ast,
            ..
        } => {
            if let IrType::Fn {
                params: params_concrete,
                ret: ret_concrete,
            } = concrete_ty
            {
                for (a, c) in params_ast.iter().zip(params_concrete.iter()) {
                    extract_from_ast_type(
                        a,
                        c,
                        type_params,
                        hkt_params,
                        const_params,
                        module,
                        subs,
                    );
                }
                extract_from_ast_type(
                    ret_ast,
                    ret_concrete,
                    type_params,
                    hkt_params,
                    const_params,
                    module,
                    subs,
                );
            }
        }
        _ => {}
    }
}

pub(super) fn infer_generic_fn_subs(
    generic_fn: &AstFunction,
    arg_tys: &[IrType],
    expected_ret: Option<&IrType>,
    module: &IrModule,
    callee_name: &str,
    span: Span,
) -> Result<HashMap<String, IrType>, LowerError> {
    let mut subs: HashMap<String, IrType> = HashMap::new();
    let (type_param_names, hkt_param_names, const_param_names) =
        split_generic_param_names(&generic_fn.type_params);
    // Pass 1: Direct Named type params
    for (param, arg_ty) in generic_fn.params.iter().zip(arg_tys.iter()) {
        if let crate::parser::ast::AstType::Named(n, _) = &param.ty {
            if type_param_names.contains(n) || hkt_param_names.contains(n) {
                subs.entry(n.clone()).or_insert_with(|| arg_ty.clone());
            }
        }
    }
    // Pass 2 & 3: Extract type, HKT, and const params from compound / Generic AST types
    for (param, arg_ty) in generic_fn.params.iter().zip(arg_tys.iter()) {
        extract_from_ast_type(
            &param.ty,
            arg_ty,
            &type_param_names,
            &hkt_param_names,
            &const_param_names,
            module,
            &mut subs,
        );
    }
    // Pass 4: Expected return type propagation
    if subs.len() < generic_fn.type_params.len() {
        if let Some(ret_annotation) = expected_ret {
            extract_from_ast_type(
                &generic_fn.return_ty,
                ret_annotation,
                &type_param_names,
                &hkt_param_names,
                &const_param_names,
                module,
                &mut subs,
            );
        }
    }
    let missing: Vec<&str> = generic_fn
        .type_params
        .iter()
        .filter_map(|p| match p {
            crate::parser::ast::AstGenericParam::Type(name, _, _)
            | crate::parser::ast::AstGenericParam::Hkt(name, _, _, _)
            | crate::parser::ast::AstGenericParam::Const { name, .. }
                if !subs.contains_key(name) =>
            {
                Some(name.as_str())
            }
            _ => None,
        })
        .collect();
    if !missing.is_empty() {
        return Err(LowerError::rich(
            "E0207",
            crate::diagnostics::Diagnostic::error(format!(
                "cannot infer type parameter{} `{}` for `{}` — no argument mentions {}, \
                 so annotate the binding (e.g. `val x: {}<...> = {}(...)`)",
                if missing.len() == 1 { "" } else { "s" },
                missing.join(", "),
                callee_name,
                if missing.len() == 1 { "it" } else { "them" },
                match &generic_fn.return_ty {
                    crate::parser::ast::AstType::Generic { name, .. } => name.clone(),
                    _ => "T".to_string(),
                },
                callee_name,
            ))
            .with_primary_span(span.start.0, span.end.0, None),
        ));
    }
    Ok(subs)
}

/// Lower a function with full generic/monomorphization state.
#[allow(clippy::too_many_arguments)]
pub(super) fn lower_function_with_generics(
    func: &AstFunction,
    module: &IrModule,
    fn_sigs: &HashMap<String, IrType>,
    const_defs: &std::rc::Rc<HashMap<String, AstExpr>>,
    generic_fns: std::rc::Rc<HashMap<String, AstFunction>>,
    mono_cache: std::rc::Rc<std::cell::RefCell<std::collections::HashSet<String>>>,
    mono_sigs: std::rc::Rc<std::cell::RefCell<HashMap<String, IrType>>>,
    trait_dispatch: std::rc::Rc<HashMap<String, Vec<(IrType, String)>>>,
    fn_defaults: std::rc::Rc<HashMap<String, Vec<Option<AstExpr>>>>,
    fn_param_names: std::rc::Rc<HashMap<String, Vec<String>>>,
    fn_param_types: std::rc::Rc<HashMap<String, Vec<IrType>>>,
    lambda_counter: std::rc::Rc<std::cell::Cell<u32>>,
) -> Result<
    (
        crate::ir::function::IrFunction,
        Vec<crate::ir::function::IrFunction>,
    ),
    LowerError,
> {
    lower_function_with_generics_and_subs(
        func,
        module,
        fn_sigs,
        const_defs,
        generic_fns,
        mono_cache,
        mono_sigs,
        HashMap::new(), // no type param subs for top-level functions
        trait_dispatch,
        fn_defaults,
        fn_param_names,
        fn_param_types,
        lambda_counter,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn lower_function_with_generics_and_subs(
    func: &AstFunction,
    module: &IrModule,
    fn_sigs: &HashMap<String, IrType>,
    const_defs: &std::rc::Rc<HashMap<String, AstExpr>>,
    generic_fns: std::rc::Rc<HashMap<String, AstFunction>>,
    mono_cache: std::rc::Rc<std::cell::RefCell<std::collections::HashSet<String>>>,
    mono_sigs: std::rc::Rc<std::cell::RefCell<HashMap<String, IrType>>>,
    type_param_subs: HashMap<String, IrType>,
    trait_dispatch: std::rc::Rc<HashMap<String, Vec<(IrType, String)>>>,
    fn_defaults: std::rc::Rc<HashMap<String, Vec<Option<AstExpr>>>>,
    fn_param_names: std::rc::Rc<HashMap<String, Vec<String>>>,
    fn_param_types: std::rc::Rc<HashMap<String, Vec<IrType>>>,
    lambda_counter: std::rc::Rc<std::cell::Cell<u32>>,
) -> Result<
    (
        crate::ir::function::IrFunction,
        Vec<crate::ir::function::IrFunction>,
    ),
    LowerError,
> {
    let resolve = |ty: &AstType| -> Result<IrType, LowerError> {
        resolve_ast_type_with_subs(ty, &type_param_subs, module)
    };

    let return_ty = if is_inferred_return(&func.return_ty) {
        // `-> Infer`: what the checker solved from the body (an async
        // function's poll body is checked under the function's name).
        let name = func
            .name
            .name
            .strip_prefix(ASYNC_POLL_PREFIX)
            .unwrap_or(&func.name.name);
        typeck::solved_return(name).ok_or_else(|| undetermined_return(func))?
    } else {
        resolve(&func.return_ty)?
    };
    let params: Vec<Param> = func
        .params
        .iter()
        .map(|p| {
            Ok(Param {
                name: p.name.name.clone(),
                ty: resolve(&p.ty)?,
            })
        })
        .collect::<Result<_, LowerError>>()?;

    if func.is_async {
        // `async def f(params) -> T` becomes
        //   * `__poll_f(task) -> T`: the body, with parameters read from the
        //     task's frame and every `await` a suspension point, turned into a
        //     resumable state machine by `pass::async_lower`;
        //   * `f(params) -> task<T>`: creates the task, stores the arguments in
        //     its frame (slots 0..n) and schedules it on this thread's executor.
        let task_ty = IrType::Task(Box::new(return_ty.clone()));
        let poll_name = format!("{ASYNC_POLL_PREFIX}{}", func.name.name);
        let mut poll_func = func.clone();
        poll_func.name.name = poll_name.clone();
        poll_func.is_async = false;
        poll_func.attrs.push(crate::parser::ast::AstAttribute {
            name: ASYNC_POLL_ATTR.to_owned(),
            args: Vec::new(),
            span: func.name.span,
        });
        let (poll_ir, mut lifted) = lower_function_with_generics_and_subs(
            &poll_func,
            module,
            fn_sigs,
            const_defs,
            generic_fns,
            mono_cache,
            mono_sigs,
            type_param_subs,
            trait_dispatch,
            fn_defaults,
            fn_param_names,
            fn_param_types,
            lambda_counter,
        )?;

        let mut builder = IrFunctionBuilder::new(&func.name.name, params.clone(), task_ty.clone());
        let entry = builder.create_block(Some("entry"));
        builder.set_current_block(entry);
        let mut arg_vals: Vec<ValueId> = Vec::with_capacity(params.len());
        for p in &params {
            arg_vals.push(builder.add_block_param(entry, Some(&p.name), p.ty.clone()));
        }
        let poll_ref = builder.fresh_value();
        builder.push_instr(
            IrInstr::ConstStr {
                result: poll_ref,
                value: poll_name,
            },
            Some(IrType::Str),
        );
        let task = builder.fresh_value();
        builder.push_instr(
            IrInstr::BuiltinCall {
                result: task,
                name: "async_task_new".to_owned(),
                args: vec![poll_ref],
                result_ty: task_ty.clone(),
            },
            Some(task_ty.clone()),
        );
        let i64_ty = IrType::Scalar(DType::I64);
        for (slot, value) in arg_vals.into_iter().enumerate() {
            let slot_val = builder.fresh_value();
            builder.push_instr(
                IrInstr::ConstInt {
                    result: slot_val,
                    value: slot as i64,
                    ty: i64_ty.clone(),
                },
                Some(i64_ty.clone()),
            );
            let done = builder.fresh_value();
            builder.push_instr(
                IrInstr::BuiltinCall {
                    result: done,
                    name: "async_frame_set".to_owned(),
                    args: vec![task, slot_val, value],
                    result_ty: i64_ty.clone(),
                },
                Some(i64_ty.clone()),
            );
        }
        let scheduled = builder.fresh_value();
        builder.push_instr(
            IrInstr::BuiltinCall {
                result: scheduled,
                name: "async_schedule".to_owned(),
                args: vec![task],
                result_ty: i64_ty.clone(),
            },
            Some(i64_ty),
        );
        builder.push_instr(IrInstr::Return { values: vec![task] }, None);
        builder.seal_unterminated_blocks();
        let constructor = builder.build();
        lifted.push(poll_ir);
        return Ok((constructor, lifted));
    }

    let is_async_poll = func.attrs.iter().any(|attr| attr.name == ASYNC_POLL_ATTR);
    let ir_params = if is_async_poll {
        vec![Param {
            name: ASYNC_TASK_PARAM.to_owned(),
            ty: IrType::Task(Box::new(return_ty.clone())),
        }]
    } else {
        params.clone()
    };
    let mut builder = IrFunctionBuilder::new(&func.name.name, ir_params.clone(), return_ty.clone());
    let entry = builder.create_block(Some("entry"));
    builder.set_current_block(entry);

    let lifted_fns = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut lowerer = Lowerer::new_generic(
        builder,
        module,
        fn_sigs,
        lambda_counter,
        lifted_fns.clone(),
        type_param_subs.clone(),
        generic_fns,
        mono_cache,
        mono_sigs,
        const_defs.clone(),
        trait_dispatch,
        fn_defaults,
        fn_param_names,
        fn_param_types,
    );
    lowerer.current_return_ty = Some(return_ty.clone());

    // Pre-register monomorphized struct defs for Generic types used in this function.
    // Walk AST param types + return type, find Generic types, resolve with subs,
    // and register the concrete struct def so field access works.
    {
        let mut local_defs = std::collections::HashMap::new();
        // Helper: walk an AST type and register monomorphized struct defs.
        fn register_generic_structs(
            ast_ty: &crate::parser::ast::AstType,
            subs: &HashMap<String, IrType>,
            module: &IrModule,
            local_defs: &mut HashMap<String, Vec<(String, IrType)>>,
        ) -> Result<(), LowerError> {
            use crate::parser::ast::AstType;
            match ast_ty {
                AstType::Generic { args, .. } => {
                    for a in args {
                        register_generic_structs(a, subs, module, local_defs)?;
                    }
                    if let IrType::Struct {
                        name: resolved,
                        fields,
                    } = resolve_ast_type_with_subs(ast_ty, subs, module)?
                    {
                        if module.struct_def(&resolved).is_none()
                            && !local_defs.contains_key(&resolved)
                            && !fields.is_empty()
                        {
                            local_defs.insert(resolved, fields);
                        }
                    }
                }
                AstType::Tuple(elems, _) => {
                    for e in elems {
                        register_generic_structs(e, subs, module, local_defs)?;
                    }
                }
                AstType::Array { elem, .. }
                | AstType::Option(elem, _)
                | AstType::Chan(elem, _)
                | AstType::Atomic(elem, _)
                | AstType::Mutex(elem, _)
                | AstType::Grad(elem, _)
                | AstType::Sparse(elem, _)
                | AstType::List(elem, _)
                | AstType::WeakRef(elem, _)
                | AstType::Ref(elem, _)
                | AstType::RefMut(elem, _)
                | AstType::Slice(elem, _) => {
                    register_generic_structs(elem, subs, module, local_defs)?;
                }
                AstType::Result(a, b, _) | AstType::Map(a, b, _) => {
                    register_generic_structs(a, subs, module, local_defs)?;
                    register_generic_structs(b, subs, module, local_defs)?;
                }
                AstType::Fn { params, ret, .. } => {
                    for p in params {
                        register_generic_structs(p, subs, module, local_defs)?;
                    }
                    register_generic_structs(ret, subs, module, local_defs)?;
                }
                _ => {}
            }
            Ok(())
        }
        for p in &func.params {
            register_generic_structs(&p.ty, &type_param_subs, module, &mut local_defs)?;
        }
        if !is_inferred_return(&func.return_ty) {
            register_generic_structs(&func.return_ty, &type_param_subs, module, &mut local_defs)?;
        }
        lowerer.local_struct_defs = local_defs;
        // Also carry over defaults for any monomorphized structs registered above.
        let mut local_defaults: HashMap<String, Vec<Option<crate::parser::ast::AstExpr>>> =
            HashMap::new();
        for mangled_name in lowerer.local_struct_defs.keys() {
            if !module.struct_defaults.contains_key(mangled_name) {
                // Find the base name by matching known base names with __ suffix.
                for (base_name, base_defaults) in &module.struct_defaults {
                    if mangled_name == base_name
                        || mangled_name.starts_with(&format!("{}__", base_name))
                    {
                        local_defaults.insert(mangled_name.clone(), base_defaults.clone());
                        break;
                    }
                }
            }
        }
        lowerer.local_struct_defaults = local_defaults;
    }

    // An async poll body receives only its task; each declared parameter is
    // read from the task's frame, where the constructor stored it.
    if is_async_poll {
        let task =
            lowerer
                .builder
                .add_block_param(entry, Some(ASYNC_TASK_PARAM), ir_params[0].ty.clone());
        lowerer.async_task = Some(task);
        let i64_ty = IrType::Scalar(DType::I64);
        for (slot, (param, ir_param)) in func.params.iter().zip(params.iter()).enumerate() {
            let slot_val = lowerer.builder.fresh_value();
            lowerer.builder.push_instr(
                IrInstr::ConstInt {
                    result: slot_val,
                    value: slot as i64,
                    ty: i64_ty.clone(),
                },
                Some(i64_ty.clone()),
            );
            let value = lowerer.builder.fresh_value();
            lowerer.builder.push_instr(
                IrInstr::BuiltinCall {
                    result: value,
                    name: "async_frame_get".to_owned(),
                    args: vec![task, slot_val],
                    result_ty: ir_param.ty.clone(),
                },
                Some(ir_param.ty.clone()),
            );
            lowerer.builder.name_value(value, &param.name.name);
            lowerer
                .scope
                .insert(param.name.name.clone(), (value, ir_param.ty.clone()));
        }
    }
    // Register function parameters as entry block params.
    for (param, ir_param) in func
        .params
        .iter()
        .zip(params.iter())
        .filter(|_| !is_async_poll)
    {
        let val =
            lowerer
                .builder
                .add_block_param(entry, Some(&param.name.name), ir_param.ty.clone());
        lowerer.bind_scope_var(param.name.name.clone(), val, ir_param.ty.clone());
        if let Some(destructor) = lowerer.find_destructor(&ir_param.ty) {
            lowerer.register_drop(
                param.name.name.clone(),
                val,
                destructor,
                ir_param.ty.clone(),
            );
        }
    }

    // Inject global constants into scope, in name order: the IR (and the
    // embedded proof fingerprint taken from it) must not depend on hash order.
    let const_defs = lowerer.const_defs.clone();
    let mut const_names: Vec<&String> = const_defs.keys().collect();
    const_names.sort();
    for name in const_names {
        let (val, ty) = lowerer.lower_expr(&const_defs[name])?;
        lowerer.scope.insert(name.clone(), (val, ty));
    }

    let const_kinds: HashMap<&str, &AstType> = func
        .type_params
        .iter()
        .filter_map(|tp| match tp {
            crate::parser::ast::AstGenericParam::Const { name, kind } => {
                Some((name.as_str(), kind.as_ref()))
            }
            _ => None,
        })
        .collect();
    let mut sub_keys: Vec<&String> = type_param_subs.keys().collect();
    sub_keys.sort();
    for name in sub_keys {
        if let Some(IrType::ConstInt(n)) = type_param_subs.get(name) {
            let const_ty = match const_kinds.get(name.as_str()).copied() {
                Some(AstType::Scalar(AstScalarKind::USize | AstScalarKind::I64, _)) | None => {
                    IrType::Scalar(DType::I64)
                }
                Some(AstType::Named(s, _)) if s == "usize" || s == "i64" => {
                    IrType::Scalar(DType::I64)
                }
                Some(other) => resolve_ast_type_with_subs(other, &type_param_subs, module)
                    .unwrap_or(IrType::Scalar(DType::I64)),
            };
            let val = lowerer.builder.fresh_value();
            lowerer.builder.push_instr(
                IrInstr::ConstInt {
                    result: val,
                    value: *n,
                    ty: const_ty.clone(),
                },
                Some(const_ty.clone()),
            );
            lowerer.scope.insert(name.clone(), (val, const_ty));
        }
    }

    let returns_dyn = matches!(return_ty, IrType::TraitObject { .. });

    let thir_block = crate::thir::body(&func.name.name)
        .map(|b| b.body)
        .or_else(|| crate::thir::block_at(func.body.span));
    let use_thir_body = thir_block
        .as_ref()
        .map(thir_block_migratable)
        .unwrap_or(false);

    let tail_val = if use_thir_body {
        let tb = thir_block.unwrap();
        if lowerer.builder.is_current_block_terminated() {
            None
        } else if tb.tail.is_some() {
            lowerer.expected_expr_ty = if returns_dyn {
                Some(return_ty.clone())
            } else {
                None
            };
            let (value, ty) = lowerer.lower_thir_block(&tb)?;
            lowerer.expected_expr_ty = None;
            Some(if returns_dyn {
                lowerer.coerce_to_trait_object(value, ty, &return_ty, func.name.span)?
            } else {
                (value, ty)
            })
        } else {
            lowerer.lower_thir_block(&tb)?;
            None
        }
    } else {
        lowerer.lower_block_stmts(&func.body)?;
        match &func.body.tail {
            Some(tail) if !lowerer.builder.is_current_block_terminated() => {
                lowerer.builder.set_line_span(tail.span().start.0);
                if returns_dyn {
                    lowerer.expected_expr_ty = Some(return_ty.clone());
                }
                let (value, ty) = lowerer.lower_expr(tail)?;
                lowerer.expected_expr_ty = None;
                Some(if returns_dyn {
                    lowerer.coerce_to_trait_object(value, ty, &return_ty, tail.span())?
                } else {
                    (value, ty)
                })
            }
            _ => None,
        }
    };

    // The block that ends the body with no value, if any: an error below
    // when the function declares a value and that block can be reached.
    let mut falls_off: Option<BlockId> = None;
    if !lowerer.builder.is_current_block_terminated() {
        let ret_values: Vec<ValueId> = match tail_val {
            Some((v, _)) => vec![v],
            None => {
                falls_off = Some(lowerer.builder.current_block());
                vec![]
            }
        };
        // Emit active destructors (Drop trait) in reverse order (LIFO) before implicit return.
        lowerer.emit_drops(&ret_values);
        lowerer.exit_drop_scope();
        // Emit deferred expressions in reverse order (LIFO) before implicit return.
        lowerer.emit_defers();
        lowerer
            .builder
            .push_instr(IrInstr::Return { values: ret_values }, None);
    }

    lowerer.builder.seal_unterminated_blocks();

    let mut ir_func = lowerer.builder.build();
    if let Some(end) = falls_off {
        if returns_a_value(&return_ty) && can_reach(&ir_func, end) {
            let ret = return_ty.to_string().trim_start_matches('%').to_string();
            return Err(LowerError::rich(
                "E0116",
                crate::diagnostics::Diagnostic::error(format!(
                    "`{}` may end without returning a value: it returns `{ret}`",
                    func.name.name
                ))
                .with_primary_span(func.name.span.start.0, func.name.span.end.0, None)
                .with_help(format!(
                    "return a `{ret}` on every path: end the body with a value or a `return`, or add an `else` branch"
                )),
            ));
        }
    }
    // Propagate AST function attributes (e.g., "kernel", "differentiable") to IR.
    ir_func.attrs = func.attrs.iter().map(ast_attr_to_ir_attr).collect();
    ir_func.is_const = func.is_const;
    let lifted = match std::rc::Rc::try_unwrap(lifted_fns) {
        Ok(cell) => cell.into_inner(),
        Err(rc) => rc.borrow().clone(),
    };
    Ok((ir_func, lifted))
}

/// Whether control can reach `target` from the entry block. A block that
/// panics goes no further, and a branch on a constant takes one edge, so
/// `while true { ... return x }` and `panic(..);` do not count as falling off.
pub(super) fn can_reach(func: &crate::ir::function::IrFunction, target: BlockId) -> bool {
    let constants: HashMap<ValueId, bool> = func
        .blocks()
        .iter()
        .flat_map(|b| b.instrs.iter())
        .filter_map(|i| match i {
            IrInstr::ConstBool { result, value } => Some((*result, *value)),
            _ => None,
        })
        .collect();
    let entry = func.entry_block().id;
    let mut seen: HashSet<BlockId> = HashSet::new();
    let mut work = vec![entry];
    while let Some(id) = work.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(block) = func.block(id) else {
            continue;
        };
        if block
            .instrs
            .iter()
            .any(|i| matches!(i, IrInstr::Panic { .. }))
        {
            continue;
        }
        if id == target {
            return true;
        }
        match block.instrs.last() {
            Some(IrInstr::Br { target, .. }) => work.push(*target),
            Some(IrInstr::CondBr {
                cond,
                then_block,
                else_block,
                ..
            }) => match constants.get(cond) {
                Some(true) => work.push(*then_block),
                Some(false) => work.push(*else_block),
                None => {
                    work.push(*then_block);
                    work.push(*else_block);
                }
            },
            Some(IrInstr::SwitchVariant {
                arms,
                default_block,
                ..
            }) => {
                work.extend(arms.iter().map(|(_, b)| *b));
                work.extend(default_block.iter().copied());
            }
            _ => {}
        }
    }
    false
}

pub fn collect_generic_apps_in_type(
    ty: &AstType,
    apps: &mut std::collections::HashSet<(String, Vec<AstType>)>,
) {
    match ty {
        AstType::Tuple(elems, _) => {
            for e in elems {
                collect_generic_apps_in_type(e, apps);
            }
        }
        AstType::Array { elem, .. } => collect_generic_apps_in_type(elem, apps),
        AstType::Option(inner, _) => collect_generic_apps_in_type(inner, apps),
        AstType::Result(ok, err, _) => {
            collect_generic_apps_in_type(ok, apps);
            collect_generic_apps_in_type(err, apps);
        }
        AstType::Chan(elem, _) => collect_generic_apps_in_type(elem, apps),
        AstType::Atomic(inner, _) => collect_generic_apps_in_type(inner, apps),
        AstType::Mutex(inner, _) => collect_generic_apps_in_type(inner, apps),
        AstType::Grad(inner, _) => collect_generic_apps_in_type(inner, apps),
        AstType::Sparse(inner, _) => collect_generic_apps_in_type(inner, apps),
        AstType::List(elem, _) => collect_generic_apps_in_type(elem, apps),
        AstType::Map(k, v, _) => {
            collect_generic_apps_in_type(k, apps);
            collect_generic_apps_in_type(v, apps);
        }
        AstType::WeakRef(inner, _) => collect_generic_apps_in_type(inner, apps),
        AstType::Fn { params, ret, .. } => {
            for p in params {
                collect_generic_apps_in_type(p, apps);
            }
            collect_generic_apps_in_type(ret, apps);
        }
        AstType::Generic { name, args, .. } => {
            apps.insert((name.clone(), args.clone()));
            for a in args {
                collect_generic_apps_in_type(a, apps);
            }
        }
        _ => {}
    }
}

pub fn collect_generic_apps_in_expr(
    expr: &AstExpr,
    apps: &mut std::collections::HashSet<(String, Vec<AstType>)>,
) {
    match expr {
        AstExpr::Ident(_) => {}
        AstExpr::IntLit { .. }
        | AstExpr::FloatLit { .. }
        | AstExpr::BoolLit { .. }
        | AstExpr::StringLit { .. } => {}
        AstExpr::BinOp { lhs, rhs, .. } => {
            collect_generic_apps_in_expr(lhs, apps);
            collect_generic_apps_in_expr(rhs, apps);
        }
        AstExpr::Call { args, .. } => {
            for a in args {
                collect_generic_apps_in_expr(a, apps);
            }
        }
        AstExpr::UnaryOp { expr: inner, .. } => {
            collect_generic_apps_in_expr(inner, apps);
        }
        AstExpr::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            collect_generic_apps_in_expr(cond, apps);
            collect_generic_apps_in_block(then_block, apps);
            if let Some(ref eb) = else_block {
                collect_generic_apps_in_block(eb, apps);
            }
        }
        AstExpr::Block(block) => collect_generic_apps_in_block(block, apps),
        AstExpr::Index { base, indices, .. } => {
            collect_generic_apps_in_expr(base, apps);
            for i in indices {
                collect_generic_apps_in_expr(i, apps);
            }
        }
        AstExpr::Cast {
            expr: inner, ty, ..
        } => {
            collect_generic_apps_in_expr(inner, apps);
            collect_generic_apps_in_type(ty, apps);
        }
        AstExpr::StructLit { fields, spread, .. } => {
            for (_, f_expr) in fields {
                collect_generic_apps_in_expr(f_expr, apps);
            }
            if let Some(s) = spread {
                collect_generic_apps_in_expr(s, apps);
            }
        }
        AstExpr::FieldAccess { base, .. } => {
            collect_generic_apps_in_expr(base, apps);
        }
        AstExpr::When {
            scrutinee, arms, ..
        } => {
            collect_generic_apps_in_expr(scrutinee, apps);
            for arm in arms {
                collect_generic_apps_in_expr(&arm.body, apps);
            }
        }
        AstExpr::Tuple { elements, .. } => {
            for e in elements {
                collect_generic_apps_in_expr(e, apps);
            }
        }
        AstExpr::TupleIndex { base, .. } => {
            collect_generic_apps_in_expr(base, apps);
        }
        AstExpr::ArrayLit { elems, .. } => {
            for e in elems {
                collect_generic_apps_in_expr(e, apps);
            }
        }
        AstExpr::Lambda { params, body, .. } => {
            for p in params {
                collect_generic_apps_in_type(&p.ty, apps);
            }
            collect_generic_apps_in_expr(body, apps);
        }
        AstExpr::Await { expr: inner, .. } => {
            collect_generic_apps_in_expr(inner, apps);
        }
        AstExpr::Try { expr: inner, .. } => {
            collect_generic_apps_in_expr(inner, apps);
        }
        AstExpr::MethodCall { base, args, .. } => {
            collect_generic_apps_in_expr(base, apps);
            for a in args {
                collect_generic_apps_in_expr(a, apps);
            }
        }
        AstExpr::Mask { body, .. } => {
            collect_generic_apps_in_block(body, apps);
        }
        AstExpr::Handle {
            expr: inner,
            arms,
            return_ty,
            ..
        } => {
            collect_generic_apps_in_expr(inner, apps);
            collect_generic_apps_in_type(return_ty, apps);
            for arm in arms {
                collect_generic_apps_in_expr(&arm.body, apps);
            }
        }
        AstExpr::NullCoal { expr, default, .. } => {
            collect_generic_apps_in_expr(expr, apps);
            collect_generic_apps_in_expr(default, apps);
        }
        AstExpr::MapLiteral { entries, .. } => {
            for (k, v) in entries {
                collect_generic_apps_in_expr(k, apps);
                collect_generic_apps_in_expr(v, apps);
            }
        }
        AstExpr::Ref { expr, .. }
        | AstExpr::RefMut { expr, .. }
        | AstExpr::Deref { expr, .. }
        | AstExpr::Move { expr, .. } => {
            collect_generic_apps_in_expr(expr, apps);
        }
        AstExpr::Unsafe { body, .. } => collect_generic_apps_in_expr(body, apps),
        AstExpr::Splat { expr, .. } => collect_generic_apps_in_expr(expr, apps),
        AstExpr::MacroCall { args, .. } => {
            for a in args {
                collect_generic_apps_in_expr(a, apps);
            }
        }
        AstExpr::TryCatch {
            body, catch_body, ..
        } => {
            collect_generic_apps_in_expr(body, apps);
            collect_generic_apps_in_expr(catch_body, apps);
        }
        AstExpr::Raise { args, .. } => {
            for a in args {
                collect_generic_apps_in_expr(a, apps);
            }
        }
    }
}

pub fn collect_generic_apps_in_stmt(
    stmt: &AstStmt,
    apps: &mut std::collections::HashSet<(String, Vec<AstType>)>,
) {
    match stmt {
        AstStmt::Let { ty, init, .. } => {
            if let Some(t) = ty {
                collect_generic_apps_in_type(t, apps);
            }
            collect_generic_apps_in_expr(init, apps);
        }
        AstStmt::Expr(expr) => collect_generic_apps_in_expr(expr, apps),
        AstStmt::While { cond, body, .. } => {
            collect_generic_apps_in_expr(cond, apps);
            collect_generic_apps_in_block(body, apps);
        }
        AstStmt::Loop { body, .. } => {
            collect_generic_apps_in_block(body, apps);
        }
        AstStmt::Break { .. } | AstStmt::Continue { .. } => {}
        AstStmt::ForRange {
            start, end, body, ..
        } => {
            collect_generic_apps_in_expr(start, apps);
            collect_generic_apps_in_expr(end, apps);
            collect_generic_apps_in_block(body, apps);
        }
        AstStmt::Assign { target, value, .. } => {
            collect_generic_apps_in_expr(target, apps);
            collect_generic_apps_in_expr(value, apps);
        }
        AstStmt::LetTuple { init, .. } => {
            collect_generic_apps_in_expr(init, apps);
        }
        AstStmt::Return { value, .. } => {
            if let Some(v) = value {
                collect_generic_apps_in_expr(v, apps);
            }
        }
        AstStmt::Spawn { body, group, .. } => {
            for s in body {
                collect_generic_apps_in_stmt(s, apps);
            }
            if let Some(g) = group {
                collect_generic_apps_in_expr(g, apps);
            }
        }
        AstStmt::ParFor {
            start, end, body, ..
        } => {
            collect_generic_apps_in_expr(start, apps);
            collect_generic_apps_in_expr(end, apps);
            collect_generic_apps_in_block(body, apps);
        }
        AstStmt::ForEach { iter, body, .. } => {
            collect_generic_apps_in_expr(iter, apps);
            collect_generic_apps_in_block(body, apps);
        }
        AstStmt::MaskStmt { body, .. } => {
            collect_generic_apps_in_block(body, apps);
        }
        AstStmt::HandleStmt {
            expr,
            arms,
            return_ty,
            ..
        } => {
            collect_generic_apps_in_expr(expr, apps);
            collect_generic_apps_in_type(return_ty, apps);
            for arm in arms {
                collect_generic_apps_in_expr(&arm.body, apps);
            }
        }
        AstStmt::Defer { expr, .. } => {
            collect_generic_apps_in_expr(expr, apps);
        }
        AstStmt::Yield { expr, .. } => {
            collect_generic_apps_in_expr(expr, apps);
        }
        AstStmt::Select { arms, default, .. } => {
            for arm in arms {
                collect_generic_apps_in_expr(&arm.channel, apps);
                collect_generic_apps_in_block(&arm.body, apps);
            }
            if let Some(d) = default {
                collect_generic_apps_in_block(d, apps);
            }
        }
    }
}

pub fn collect_generic_apps_in_block(
    block: &AstBlock,
    apps: &mut std::collections::HashSet<(String, Vec<AstType>)>,
) {
    if block.stmts.is_empty() && block.tail.is_none() {
        // Synthetic marker (see lower_expr): facts live in the THIR registry.
        match synth_marker_at(block.span) {
            Some(SynthMarker::Block(tb)) => {
                collect_generic_apps_in_thir_block(&tb, apps);
                return;
            }
            Some(SynthMarker::Expr(e)) => {
                collect_generic_apps_in_thir_expr(&e, apps);
                return;
            }
            None => {
                if let Some(tb) = crate::thir::block_at(block.span) {
                    collect_generic_apps_in_thir_block(&tb, apps);
                    return;
                }
            }
        }
    }
    for stmt in &block.stmts {
        collect_generic_apps_in_stmt(stmt, apps);
    }
}

pub(super) fn collect_generic_apps_in_thir_expr(
    e: &crate::thir::ThirExpr,
    apps: &mut std::collections::HashSet<(String, Vec<AstType>)>,
) {
    match &e.kind {
        crate::thir::ThirExprKind::Call {
            args, named_args, ..
        } => {
            for a in args {
                collect_generic_apps_in_thir_expr(a, apps);
            }
            for (_, a) in named_args {
                collect_generic_apps_in_thir_expr(a, apps);
            }
        }
        crate::thir::ThirExprKind::MethodCall { receiver, args, .. } => {
            collect_generic_apps_in_thir_expr(receiver, apps);
            for a in args {
                collect_generic_apps_in_thir_expr(a, apps);
            }
        }
        crate::thir::ThirExprKind::Field { base, .. }
        | crate::thir::ThirExprKind::TupleIndex { base, .. }
        | crate::thir::ThirExprKind::Destructure { base, .. }
        | crate::thir::ThirExprKind::UnaryOp { expr: base, .. }
        | crate::thir::ThirExprKind::Await(base)
        | crate::thir::ThirExprKind::Try(base)
        | crate::thir::ThirExprKind::Ref(base)
        | crate::thir::ThirExprKind::RefMut(base)
        | crate::thir::ThirExprKind::Deref(base)
        | crate::thir::ThirExprKind::Move(base)
        | crate::thir::ThirExprKind::Unsafe(base)
        | crate::thir::ThirExprKind::Splat(base) => {
            collect_generic_apps_in_thir_expr(base, apps);
        }
        crate::thir::ThirExprKind::Cast { expr, ty, .. } => {
            collect_generic_apps_in_thir_expr(expr, apps);
            collect_generic_apps_in_type(ty, apps);
        }
        crate::thir::ThirExprKind::BinOp { lhs, rhs, .. } => {
            collect_generic_apps_in_thir_expr(lhs, apps);
            collect_generic_apps_in_thir_expr(rhs, apps);
        }
        crate::thir::ThirExprKind::Index { base, indices } => {
            collect_generic_apps_in_thir_expr(base, apps);
            for idx in indices {
                collect_generic_apps_in_thir_expr(idx, apps);
            }
        }
        crate::thir::ThirExprKind::If {
            cond,
            then_block,
            else_block,
        } => {
            collect_generic_apps_in_thir_expr(cond, apps);
            collect_generic_apps_in_thir_block(then_block, apps);
            if let Some(eb) = else_block {
                collect_generic_apps_in_thir_block(eb, apps);
            }
        }
        crate::thir::ThirExprKind::Block(b) => {
            collect_generic_apps_in_thir_block(b, apps);
        }
        crate::thir::ThirExprKind::NullCoal { expr, default } => {
            collect_generic_apps_in_thir_expr(expr, apps);
            collect_generic_apps_in_thir_expr(default, apps);
        }
        crate::thir::ThirExprKind::Tuple(elements) | crate::thir::ThirExprKind::Array(elements) => {
            for el in elements {
                collect_generic_apps_in_thir_expr(el, apps);
            }
        }
        crate::thir::ThirExprKind::Map {
            entries,
            key_ty: _,
            val_ty: _,
        } => {
            for (k, v) in entries {
                collect_generic_apps_in_thir_expr(k, apps);
                collect_generic_apps_in_thir_expr(v, apps);
            }
        }
        crate::thir::ThirExprKind::StructLit { fields, spread, .. } => {
            for f in fields {
                collect_generic_apps_in_thir_expr(&f.value, apps);
            }
            if let Some(s) = spread {
                collect_generic_apps_in_thir_expr(s, apps);
            }
        }
        crate::thir::ThirExprKind::When {
            scrutinee, arms, ..
        } => {
            collect_generic_apps_in_thir_expr(scrutinee, apps);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_generic_apps_in_thir_expr(g, apps);
                }
                collect_generic_apps_in_thir_expr(&arm.body, apps);
            }
        }
        crate::thir::ThirExprKind::Lambda { params, body, .. } => {
            for p in params {
                if let Some(t) = &p.ty {
                    collect_generic_apps_in_type(t, apps);
                }
            }
            collect_generic_apps_in_thir_expr(body, apps);
        }
        crate::thir::ThirExprKind::Mask { body, .. } => {
            collect_generic_apps_in_thir_block(body, apps);
        }
        crate::thir::ThirExprKind::Handle {
            expr,
            arms,
            return_ty,
        } => {
            collect_generic_apps_in_thir_expr(expr, apps);
            for arm in arms {
                collect_generic_apps_in_thir_expr(&arm.body, apps);
            }
            collect_generic_apps_in_type(return_ty, apps);
        }
        crate::thir::ThirExprKind::TryCatch {
            body, catch_body, ..
        } => {
            collect_generic_apps_in_thir_expr(body, apps);
            collect_generic_apps_in_thir_expr(catch_body, apps);
        }
        crate::thir::ThirExprKind::Raise { args, .. }
        | crate::thir::ThirExprKind::MacroCall { args, .. } => {
            for a in args {
                collect_generic_apps_in_thir_expr(a, apps);
            }
        }
        _ => {}
    }
}

pub(super) fn collect_generic_apps_in_thir_stmt(
    s: &crate::thir::ThirStmt,
    apps: &mut std::collections::HashSet<(String, Vec<AstType>)>,
) {
    match &s.kind {
        crate::thir::ThirStmtKind::Let { ty, init, .. } => {
            if let Some(t) = ty {
                collect_generic_apps_in_type(t, apps);
            }
            collect_generic_apps_in_thir_expr(init, apps)
        }
        crate::thir::ThirStmtKind::Expr(e) => collect_generic_apps_in_thir_expr(e, apps),
        crate::thir::ThirStmtKind::Return(Some(e)) => collect_generic_apps_in_thir_expr(e, apps),
        crate::thir::ThirStmtKind::Return(None) => {}
        crate::thir::ThirStmtKind::Assign { target, value, .. } => {
            collect_generic_apps_in_thir_expr(target, apps);
            collect_generic_apps_in_thir_expr(value, apps);
        }
        crate::thir::ThirStmtKind::While { cond, body, .. } => {
            collect_generic_apps_in_thir_expr(cond, apps);
            collect_generic_apps_in_thir_block(body, apps);
        }
        crate::thir::ThirStmtKind::Loop { body, .. } => {
            collect_generic_apps_in_thir_block(body, apps)
        }
        crate::thir::ThirStmtKind::ForRange {
            start,
            end,
            body,
            step,
            ..
        } => {
            collect_generic_apps_in_thir_expr(start, apps);
            collect_generic_apps_in_thir_expr(end, apps);
            if let Some(st) = step {
                collect_generic_apps_in_thir_expr(st, apps);
            }
            collect_generic_apps_in_thir_block(body, apps);
        }
        crate::thir::ThirStmtKind::ForEach { iter, body, .. } => {
            collect_generic_apps_in_thir_expr(iter, apps);
            collect_generic_apps_in_thir_block(body, apps);
        }
        crate::thir::ThirStmtKind::ParFor {
            start, end, body, ..
        } => {
            collect_generic_apps_in_thir_expr(start, apps);
            collect_generic_apps_in_thir_expr(end, apps);
            collect_generic_apps_in_thir_block(body, apps);
        }
        crate::thir::ThirStmtKind::Mask { body, .. } => {
            collect_generic_apps_in_thir_block(body, apps)
        }
        crate::thir::ThirStmtKind::Handle { expr, arms, .. } => {
            collect_generic_apps_in_thir_expr(expr, apps);
            for arm in arms {
                collect_generic_apps_in_thir_expr(&arm.body, apps);
            }
        }
        crate::thir::ThirStmtKind::Defer(e) => collect_generic_apps_in_thir_expr(e, apps),
        crate::thir::ThirStmtKind::Yield(e) => collect_generic_apps_in_thir_expr(e, apps),
        crate::thir::ThirStmtKind::Select { arms, default } => {
            for arm in arms {
                collect_generic_apps_in_thir_expr(&arm.channel, apps);
                collect_generic_apps_in_thir_block(&arm.body, apps);
            }
            if let Some(d) = default {
                collect_generic_apps_in_thir_block(d, apps);
            }
        }
        crate::thir::ThirStmtKind::Spawn { body, group } => {
            collect_generic_apps_in_thir_block(body, apps);
            if let Some(g) = group {
                collect_generic_apps_in_thir_expr(g, apps);
            }
        }
        _ => {}
    }
}

pub(super) fn collect_generic_apps_in_thir_block(
    b: &crate::thir::ThirBlock,
    apps: &mut std::collections::HashSet<(String, Vec<AstType>)>,
) {
    for stmt in &b.stmts {
        collect_generic_apps_in_thir_stmt(stmt, apps);
    }
    if let Some(tail) = &b.tail {
        collect_generic_apps_in_thir_expr(tail, apps);
    }
}
