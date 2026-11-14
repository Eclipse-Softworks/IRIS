//! Lazy post-IR monomorphisation pass (`MonoPass`).
//!
//! Instantiates polymorphic IR function templates (`IrModule::generic_templates`)
//! on demand via a reachability worklist starting from concrete functions in
//! `IrModule::functions`. Unreferenced templates and specializations are never
//! materialized into `IrModule::functions`.

use std::collections::{HashMap, VecDeque};

use crate::error::PassError;
use crate::ir::function::IrFunction;
use crate::ir::instr::IrInstr;
use crate::ir::module::IrModule;
use crate::ir::types::{IrType, TraitMethodSig, TypeArena};
use crate::pass::Pass;

/// Lazily monomorphises reachable generic call sites from `generic_templates`.
pub struct MonoPass;

impl Pass for MonoPass {
    fn name(&self) -> &'static str {
        "mono"
    }

    fn run(&mut self, module: &mut IrModule) -> Result<(), PassError> {
        let mut worklist: VecDeque<usize> = (0..module.functions.len()).collect();

        while let Some(func_idx) = worklist.pop_front() {
            let calls = std::mem::take(&mut module.functions[func_idx].mono_calls);
            for ((block_idx, instr_idx), (template_name, type_args)) in calls {
                let caller_name = module.functions[func_idx].name.clone();
                let template = module
                    .generic_templates
                    .get(&template_name)
                    .cloned()
                    .ok_or_else(|| PassError::TypeError {
                        func: caller_name.clone(),
                        detail: format!(
                            "generic template '{}' referenced by '{}' not found",
                            template_name, caller_name
                        ),
                    })?;

                let expected_len = template.type_params.len() + template.hkt_params.len();
                if expected_len != type_args.len() {
                    return Err(PassError::TypeError {
                        func: caller_name,
                        detail: format!(
                            "generic template '{}' expects {} type arg(s), got {}",
                            template_name,
                            expected_len,
                            type_args.len()
                        ),
                    });
                }

                let mangled = mangle_mono_name(&template_name, &type_args);
                if !module.function_index.contains_key(&mangled) {
                    let tp_len = template.type_params.len();
                    let subs: HashMap<String, IrType> = template
                        .type_params
                        .iter()
                        .cloned()
                        .zip(type_args[..tp_len].iter().cloned())
                        .collect();
                    let hkt_subs: HashMap<String, String> = template
                        .hkt_params
                        .iter()
                        .cloned()
                        .zip(type_args[tp_len..].iter().map(extract_hkt_ctor_name))
                        .collect();
                    let specialized = instantiate_template_with_context(
                        template.func,
                        &mangled,
                        &subs,
                        &hkt_subs,
                        &module.struct_defs,
                        &module.trait_impl_methods,
                    );
                    let new_id = module.add_function(specialized).map_err(|detail| {
                        PassError::TypeError {
                            func: mangled.clone(),
                            detail,
                        }
                    })?;
                    worklist.push_back(new_id.0 as usize);
                }

                let spec_ret_ty = module.functions[module.function_index[&mangled].0 as usize]
                    .return_ty
                    .clone();
                if let Some(block) = module.functions[func_idx].blocks.get_mut(block_idx) {
                    if let Some(IrInstr::Call {
                        callee, result_ty, ..
                    }) = block.instrs.get_mut(instr_idx)
                    {
                        *callee = mangled;
                        if let Some(ret_ty) = result_ty {
                            if contains_type_param(ret_ty) {
                                *ret_ty = spec_ret_ty;
                            }
                        }
                    }
                }
            }
        }

        // Verify no uninstantiated IrType::TypeParam survives in any concrete function.
        for func in &module.functions {
            for p in &func.params {
                if contains_type_param(&p.ty) {
                    return Err(PassError::TypeError {
                        func: func.name.clone(),
                        detail: format!(
                            "unresolved type parameter '{}' in parameter '{}'",
                            p.ty, p.name
                        ),
                    });
                }
            }
            if contains_type_param(&func.return_ty) {
                return Err(PassError::TypeError {
                    func: func.name.clone(),
                    detail: format!(
                        "unresolved type parameter '{}' in return type",
                        func.return_ty
                    ),
                });
            }
            for (vid, ty) in &func.value_types {
                if contains_type_param(ty) {
                    return Err(PassError::TypeError {
                        func: func.name.clone(),
                        detail: format!("unresolved type parameter '{}' on {:?}", ty, vid),
                    });
                }
            }
        }

        Ok(())
    }
}

/// Returns a deterministic mangled name for `(template, type_args)`.
pub fn mangle_mono_name(template: &str, type_args: &[IrType]) -> String {
    if type_args.is_empty() {
        return template.to_owned();
    }
    let mut out = String::from(template);
    for ty in type_args {
        out.push_str("__");
        let rendered = format!("{}", ty);
        for ch in rendered.chars() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                out.push(ch);
            } else {
                out.push('_');
            }
        }
    }
    out
}

/// Returns `true` if `ty` contains any [`IrType::TypeParam`].
pub fn contains_type_param(ty: &IrType) -> bool {
    match ty {
        IrType::TypeParam(_) | IrType::HktApp { .. } => true,
        IrType::Scalar(_)
        | IrType::Infer
        | IrType::Str
        | IrType::TaskGroup
        | IrType::TapeRef
        | IrType::ConstInt(_) => false,
        IrType::Tensor { .. } | IrType::Enum { .. } => false,
        IrType::Fn { params, ret } => {
            params.iter().any(contains_type_param) || contains_type_param(ret)
        }
        IrType::Struct { fields, .. } => fields.iter().any(|(_, t)| contains_type_param(t)),
        IrType::Tuple(elems) => elems.iter().any(contains_type_param),
        IrType::Array { elem, .. }
        | IrType::Slice { elem, .. }
        | IrType::Option(elem)
        | IrType::Chan(elem)
        | IrType::Task(elem)
        | IrType::Atomic(elem)
        | IrType::Mutex(elem)
        | IrType::Grad(elem)
        | IrType::Sparse(elem)
        | IrType::List(elem)
        | IrType::WeakRef(elem) => contains_type_param(elem),
        IrType::ResultType(ok, err) | IrType::Map(ok, err) => {
            contains_type_param(ok) || contains_type_param(err)
        }
        IrType::TraitObject { methods, .. } => methods
            .iter()
            .any(|m| m.params.iter().any(contains_type_param) || contains_type_param(&m.ret)),
    }
}

fn extract_hkt_ctor_name(ty: &IrType) -> String {
    match ty {
        IrType::TypeParam(name) => name.clone(),
        IrType::List(_) => "list".to_owned(),
        IrType::Option(_) => "option".to_owned(),
        IrType::Map(_, _) => "map".to_owned(),
        IrType::ResultType(_, _) => "result".to_owned(),
        IrType::Chan(_) => "chan".to_owned(),
        IrType::Task(_) => "task".to_owned(),
        IrType::Atomic(_) => "atomic".to_owned(),
        IrType::Mutex(_) => "mutex".to_owned(),
        IrType::Grad(_) => "grad".to_owned(),
        IrType::Sparse(_) => "sparse".to_owned(),
        IrType::WeakRef(_) => "weak".to_owned(),
        IrType::Struct { name, .. } => name.split("__").next().unwrap_or(name).to_owned(),
        other => format!("{}", other),
    }
}

fn ir_type_base_name(ty: &IrType) -> String {
    match ty {
        IrType::Struct { name, .. } => name.clone(),
        IrType::Enum { name, .. } => name.clone(),
        IrType::Scalar(d) => format!("{}", d),
        IrType::Str => "str".to_owned(),
        other => format!("{}", other),
    }
}

/// Recursively substitutes [`IrType::TypeParam`] and [`IrType::HktApp`] occurrences.
pub fn substitute_ir_type(ty: &IrType, subs: &HashMap<String, IrType>) -> IrType {
    let empty_hkt = HashMap::new();
    let empty_structs = HashMap::new();
    substitute_ir_type_with_hkt(ty, subs, &empty_hkt, &empty_structs)
}

/// Recursively substitutes [`IrType::TypeParam`] and [`IrType::HktApp`] using
/// `subs`, `hkt_subs`, and `struct_defs`.
pub fn substitute_ir_type_with_hkt(
    ty: &IrType,
    subs: &HashMap<String, IrType>,
    hkt_subs: &HashMap<String, String>,
    struct_defs: &HashMap<String, Vec<(String, IrType)>>,
) -> IrType {
    match ty {
        IrType::TypeParam(name) => subs.get(name).cloned().unwrap_or_else(|| ty.clone()),
        IrType::HktApp { ctor, args } => {
            let sub_args: Vec<IrType> = args
                .iter()
                .map(|a| substitute_ir_type_with_hkt(a, subs, hkt_subs, struct_defs))
                .collect();
            let resolved_ctor = hkt_subs.get(ctor).map(String::as_str).unwrap_or(ctor);
            match resolved_ctor {
                "list" if !sub_args.is_empty() => IrType::List(Box::new(sub_args[0].clone())),
                "option" if !sub_args.is_empty() => IrType::Option(Box::new(sub_args[0].clone())),
                "chan" if !sub_args.is_empty() => IrType::Chan(Box::new(sub_args[0].clone())),
                "task" if !sub_args.is_empty() => IrType::Task(Box::new(sub_args[0].clone())),
                "atomic" if !sub_args.is_empty() => IrType::Atomic(Box::new(sub_args[0].clone())),
                "mutex" if !sub_args.is_empty() => IrType::Mutex(Box::new(sub_args[0].clone())),
                "grad" if !sub_args.is_empty() => IrType::Grad(Box::new(sub_args[0].clone())),
                "sparse" if !sub_args.is_empty() => IrType::Sparse(Box::new(sub_args[0].clone())),
                "weak" if !sub_args.is_empty() => IrType::WeakRef(Box::new(sub_args[0].clone())),
                "map" if sub_args.len() >= 2 => {
                    IrType::Map(Box::new(sub_args[0].clone()), Box::new(sub_args[1].clone()))
                }
                "result" if sub_args.len() >= 2 => {
                    IrType::ResultType(Box::new(sub_args[0].clone()), Box::new(sub_args[1].clone()))
                }
                struct_ctor => {
                    let lower_mangled = format!(
                        "{}__{}",
                        struct_ctor,
                        sub_args
                            .iter()
                            .map(crate::lower::mangle_ir_type)
                            .collect::<Vec<_>>()
                            .join("_")
                    );
                    let mangled = if struct_defs.contains_key(&lower_mangled) {
                        lower_mangled
                    } else {
                        mangle_mono_name(struct_ctor, &sub_args)
                    };
                    let fields = struct_defs.get(&mangled).cloned().unwrap_or_default();
                    IrType::Struct {
                        name: mangled,
                        fields,
                    }
                }
            }
        }
        IrType::Scalar(_)
        | IrType::Tensor { .. }
        | IrType::Infer
        | IrType::Enum { .. }
        | IrType::Str
        | IrType::TaskGroup
        | IrType::TapeRef
        | IrType::ConstInt(_) => ty.clone(),
        IrType::Fn { params, ret } => IrType::Fn {
            params: params
                .iter()
                .map(|p| substitute_ir_type_with_hkt(p, subs, hkt_subs, struct_defs))
                .collect(),
            ret: Box::new(substitute_ir_type_with_hkt(
                ret,
                subs,
                hkt_subs,
                struct_defs,
            )),
        },
        IrType::Struct { name, fields } => IrType::Struct {
            name: name.clone(),
            fields: fields
                .iter()
                .map(|(n, t)| {
                    (
                        n.clone(),
                        substitute_ir_type_with_hkt(t, subs, hkt_subs, struct_defs),
                    )
                })
                .collect(),
        },
        IrType::Tuple(elems) => IrType::Tuple(
            elems
                .iter()
                .map(|t| substitute_ir_type_with_hkt(t, subs, hkt_subs, struct_defs))
                .collect(),
        ),
        IrType::Array { elem, len } => IrType::Array {
            elem: Box::new(substitute_ir_type_with_hkt(
                elem,
                subs,
                hkt_subs,
                struct_defs,
            )),
            len: *len,
        },
        IrType::Slice { elem, is_mut } => IrType::Slice {
            elem: Box::new(substitute_ir_type_with_hkt(
                elem,
                subs,
                hkt_subs,
                struct_defs,
            )),
            is_mut: *is_mut,
        },
        IrType::Option(inner) => IrType::Option(Box::new(substitute_ir_type_with_hkt(
            inner,
            subs,
            hkt_subs,
            struct_defs,
        ))),
        IrType::ResultType(ok, err) => IrType::ResultType(
            Box::new(substitute_ir_type_with_hkt(ok, subs, hkt_subs, struct_defs)),
            Box::new(substitute_ir_type_with_hkt(
                err,
                subs,
                hkt_subs,
                struct_defs,
            )),
        ),
        IrType::Chan(inner) => IrType::Chan(Box::new(substitute_ir_type_with_hkt(
            inner,
            subs,
            hkt_subs,
            struct_defs,
        ))),
        IrType::Task(inner) => IrType::Task(Box::new(substitute_ir_type_with_hkt(
            inner,
            subs,
            hkt_subs,
            struct_defs,
        ))),
        IrType::Atomic(inner) => IrType::Atomic(Box::new(substitute_ir_type_with_hkt(
            inner,
            subs,
            hkt_subs,
            struct_defs,
        ))),
        IrType::Mutex(inner) => IrType::Mutex(Box::new(substitute_ir_type_with_hkt(
            inner,
            subs,
            hkt_subs,
            struct_defs,
        ))),
        IrType::Grad(inner) => IrType::Grad(Box::new(substitute_ir_type_with_hkt(
            inner,
            subs,
            hkt_subs,
            struct_defs,
        ))),
        IrType::Sparse(inner) => IrType::Sparse(Box::new(substitute_ir_type_with_hkt(
            inner,
            subs,
            hkt_subs,
            struct_defs,
        ))),
        IrType::List(inner) => IrType::List(Box::new(substitute_ir_type_with_hkt(
            inner,
            subs,
            hkt_subs,
            struct_defs,
        ))),
        IrType::Map(k, v) => IrType::Map(
            Box::new(substitute_ir_type_with_hkt(k, subs, hkt_subs, struct_defs)),
            Box::new(substitute_ir_type_with_hkt(v, subs, hkt_subs, struct_defs)),
        ),
        IrType::WeakRef(inner) => IrType::WeakRef(Box::new(substitute_ir_type_with_hkt(
            inner,
            subs,
            hkt_subs,
            struct_defs,
        ))),
        IrType::TraitObject { name, methods } => IrType::TraitObject {
            name: name.clone(),
            methods: methods
                .iter()
                .map(|m| TraitMethodSig {
                    name: m.name.clone(),
                    params: m
                        .params
                        .iter()
                        .map(|p| substitute_ir_type_with_hkt(p, subs, hkt_subs, struct_defs))
                        .collect(),
                    ret: Box::new(substitute_ir_type_with_hkt(
                        &m.ret,
                        subs,
                        hkt_subs,
                        struct_defs,
                    )),
                })
                .collect(),
        },
    }
}

fn instantiate_template_with_context(
    mut func: IrFunction,
    mangled_name: &str,
    subs: &HashMap<String, IrType>,
    hkt_subs: &HashMap<String, String>,
    struct_defs: &HashMap<String, Vec<(String, IrType)>>,
    trait_impl_methods: &HashMap<String, Vec<(String, String, String)>>,
) -> IrFunction {
    func.name = mangled_name.to_owned();
    for p in &mut func.params {
        p.ty = substitute_ir_type_with_hkt(&p.ty, subs, hkt_subs, struct_defs);
    }
    func.return_ty = substitute_ir_type_with_hkt(&func.return_ty, subs, hkt_subs, struct_defs);

    let mut arena = TypeArena::new();
    for p in &func.params {
        arena.intern_ref(&p.ty);
    }
    arena.intern_ref(&func.return_ty);

    let mut new_value_types = HashMap::with_capacity(func.value_types.len());
    let mut new_value_type_ids = HashMap::with_capacity(func.value_types.len());
    for (vid, ty) in &func.value_types {
        let sub = substitute_ir_type_with_hkt(ty, subs, hkt_subs, struct_defs);
        let tid = arena.intern_ref(&sub);
        new_value_types.insert(*vid, sub);
        new_value_type_ids.insert(*vid, tid);
    }
    func.value_types = new_value_types;
    func.value_type_ids = new_value_type_ids;
    func.type_arena = arena;

    for block in &mut func.blocks {
        for bp in &mut block.params {
            bp.ty = substitute_ir_type_with_hkt(&bp.ty, subs, hkt_subs, struct_defs);
        }
        for instr in &mut block.instrs {
            substitute_instr_types_with_context(
                instr,
                subs,
                hkt_subs,
                struct_defs,
                trait_impl_methods,
            );
        }
    }

    for (_, type_args) in func.mono_calls.values_mut() {
        for t in type_args.iter_mut() {
            *t = substitute_ir_type_with_hkt(t, subs, hkt_subs, struct_defs);
        }
    }

    func
}

fn resolve_blanket_trait_call(
    callee: &str,
    subs: &HashMap<String, IrType>,
    trait_impl_methods: &HashMap<String, Vec<(String, String, String)>>,
) -> Option<String> {
    let rest = callee.strip_prefix("__trait_method__")?;
    let mut parts = rest.splitn(3, "__");
    let tp_name = parts.next()?;
    let bound_trait = parts.next()?;
    let method_name = parts.next()?;
    let concrete_ty = subs.get(tp_name)?;
    let concrete_name = ir_type_base_name(concrete_ty);

    if !bound_trait.is_empty() {
        if let Some(entries) = trait_impl_methods.get(bound_trait) {
            if let Some((_, _, mangled)) = entries
                .iter()
                .find(|(cty, mname, _)| cty == &concrete_name && mname == method_name)
            {
                return Some(mangled.clone());
            }
        }
    }
    for entries in trait_impl_methods.values() {
        if let Some((_, _, mangled)) = entries
            .iter()
            .find(|(cty, mname, _)| cty == &concrete_name && mname == method_name)
        {
            return Some(mangled.clone());
        }
    }
    None
}

fn substitute_instr_types_with_context(
    instr: &mut IrInstr,
    subs: &HashMap<String, IrType>,
    hkt_subs: &HashMap<String, String>,
    struct_defs: &HashMap<String, Vec<(String, IrType)>>,
    trait_impl_methods: &HashMap<String, Vec<(String, String, String)>>,
) {
    match instr {
        IrInstr::ConstInt { ty, .. }
        | IrInstr::ConstFloat { ty, .. }
        | IrInstr::UnaryOp { ty, .. }
        | IrInstr::BinOp { ty, .. } => {
            *ty = substitute_ir_type_with_hkt(ty, subs, hkt_subs, struct_defs);
        }
        IrInstr::TensorOp { result_ty, .. }
        | IrInstr::BuiltinCall { result_ty, .. }
        | IrInstr::DynCall { result_ty, .. } => {
            *result_ty = substitute_ir_type_with_hkt(result_ty, subs, hkt_subs, struct_defs);
        }
        IrInstr::Call {
            callee, result_ty, ..
        } => {
            if let Some(resolved) = resolve_blanket_trait_call(callee, subs, trait_impl_methods) {
                *callee = resolved;
            }
            if let Some(t) = result_ty {
                *t = substitute_ir_type_with_hkt(t, subs, hkt_subs, struct_defs);
            }
        }
        IrInstr::Cast { from_ty, to_ty, .. } => {
            *from_ty = substitute_ir_type_with_hkt(from_ty, subs, hkt_subs, struct_defs);
            *to_ty = substitute_ir_type_with_hkt(to_ty, subs, hkt_subs, struct_defs);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::function::Param;
    use crate::ir::module::IrFunctionBuilder;
    use crate::ir::types::DType;

    fn build_id_template() -> IrFunction {
        let t_param = IrType::TypeParam("T".to_owned());
        let mut b = IrFunctionBuilder::new(
            "id",
            vec![Param {
                name: "x".to_owned(),
                ty: t_param.clone(),
            }],
            t_param.clone(),
        );
        let entry = b.create_block(Some("entry"));
        b.set_current_block(entry);
        let x = b.add_block_param(entry, Some("x"), t_param);
        b.push_instr(IrInstr::Return { values: vec![x] }, None);
        b.build()
    }

    fn build_wrap_template() -> IrFunction {
        let u_param = IrType::TypeParam("U".to_owned());
        let mut b = IrFunctionBuilder::new(
            "wrap",
            vec![Param {
                name: "v".to_owned(),
                ty: u_param.clone(),
            }],
            u_param.clone(),
        );
        let entry = b.create_block(Some("entry"));
        b.set_current_block(entry);
        let v = b.add_block_param(entry, Some("v"), u_param.clone());
        let res = b.fresh_value();
        b.push_generic_call(
            "id",
            vec![u_param.clone()],
            IrInstr::Call {
                result: Some(res),
                callee: "id".to_owned(),
                args: vec![v],
                result_ty: Some(u_param.clone()),
            },
            Some(u_param),
        );
        b.push_instr(IrInstr::Return { values: vec![res] }, None);
        b.build()
    }

    #[test]
    fn lazy_mono_instantiates_reachable_and_transitive_specializations_only() {
        let i64_ty = IrType::Scalar(DType::I64);
        let mut main_b = IrFunctionBuilder::new("main", vec![], i64_ty.clone());
        let entry = main_b.create_block(Some("entry"));
        main_b.set_current_block(entry);
        let c = main_b.fresh_value();
        main_b.push_instr(
            IrInstr::ConstInt {
                result: c,
                value: 42,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let r = main_b.fresh_value();
        main_b.push_generic_call(
            "wrap",
            vec![i64_ty.clone()],
            IrInstr::Call {
                result: Some(r),
                callee: "wrap".to_owned(),
                args: vec![c],
                result_ty: Some(i64_ty.clone()),
            },
            Some(i64_ty),
        );
        main_b.push_instr(IrInstr::Return { values: vec![r] }, None);

        let mut module = IrModule::new("lazy_mono_test");
        module.add_function(main_b.build()).unwrap();
        module.add_generic_template("id", vec!["T".to_owned()], build_id_template());
        module.add_generic_template("wrap", vec!["U".to_owned()], build_wrap_template());
        module.add_generic_template("unused", vec!["Z".to_owned()], build_id_template());

        MonoPass.run(&mut module).unwrap();

        // `main`, `wrap__i64`, and transitive `id__i64` exist; `unused` was never instantiated.
        assert!(module.function_index.contains_key("wrap__i64"));
        assert!(module.function_index.contains_key("id__i64"));
        assert_eq!(module.functions.len(), 3);

        // Call sites were rewritten to mangled concrete names and no TypeParam survives.
        let main_fn = &module.functions[module.function_index["main"].0 as usize];
        assert!(matches!(
            &main_fn.blocks[0].instrs[1],
            IrInstr::Call { callee, .. } if callee == "wrap__i64"
        ));
        let wrap_fn = &module.functions[module.function_index["wrap__i64"].0 as usize];
        assert!(matches!(
            &wrap_fn.blocks[0].instrs[0],
            IrInstr::Call { callee, .. } if callee == "id__i64"
        ));
    }

    #[test]
    fn rejects_unresolved_type_param_in_concrete_function() {
        let t_param = IrType::TypeParam("T".to_owned());
        let mut b = IrFunctionBuilder::new(
            "bad",
            vec![Param {
                name: "x".to_owned(),
                ty: t_param.clone(),
            }],
            t_param.clone(),
        );
        let entry = b.create_block(Some("entry"));
        b.set_current_block(entry);
        let x = b.add_block_param(entry, Some("x"), t_param);
        b.push_instr(IrInstr::Return { values: vec![x] }, None);

        let mut module = IrModule::new("bad_test");
        module.add_function(b.build()).unwrap();

        let err = MonoPass.run(&mut module).unwrap_err();
        assert!(err.to_string().contains("unresolved type parameter"));
    }

    #[test]
    fn standard_pipeline_and_llvm_emit_succeed_with_lazy_mono() {
        let i64_ty = IrType::Scalar(DType::I64);
        let mut main_b = IrFunctionBuilder::new("main", vec![], i64_ty.clone());
        let entry = main_b.create_block(Some("entry"));
        main_b.set_current_block(entry);
        let c = main_b.fresh_value();
        main_b.push_instr(
            IrInstr::ConstInt {
                result: c,
                value: 7,
                ty: i64_ty.clone(),
            },
            Some(i64_ty.clone()),
        );
        let r = main_b.fresh_value();
        main_b.push_generic_call(
            "wrap",
            vec![i64_ty.clone()],
            IrInstr::Call {
                result: Some(r),
                callee: "wrap".to_owned(),
                args: vec![c],
                result_ty: Some(i64_ty.clone()),
            },
            Some(i64_ty),
        );
        main_b.push_instr(IrInstr::Return { values: vec![r] }, None);

        let mut module = IrModule::new("pipeline_mono_test");
        module.add_function(main_b.build()).unwrap();
        module.add_generic_template("id", vec!["T".to_owned()], build_id_template());
        module.add_generic_template("wrap", vec!["U".to_owned()], build_wrap_template());

        let mut pm = crate::pass::build_standard_pipeline();
        pm.run(&mut module).unwrap();
        let llvm_ir = crate::codegen::llvm_ir::emit_llvm_ir(&module).unwrap();
        assert!(llvm_ir.contains("define i64 @main"));
    }

    #[test]
    fn hkt_template_instantiates_builtin_constructor() {
        let f_of_t = IrType::HktApp {
            ctor: "F".to_owned(),
            args: vec![IrType::TypeParam("T".to_owned())],
        };
        let mut hkt_b = IrFunctionBuilder::new(
            "ident",
            vec![Param {
                name: "x".to_owned(),
                ty: f_of_t.clone(),
            }],
            f_of_t.clone(),
        );
        let entry = hkt_b.create_block(Some("entry"));
        let x = hkt_b.add_block_param(entry, Some("x"), f_of_t.clone());
        hkt_b.set_current_block(entry);
        hkt_b.push_instr(IrInstr::Return { values: vec![x] }, None);

        let list_i64 = IrType::List(Box::new(IrType::Scalar(DType::I64)));
        let mut main_b = IrFunctionBuilder::new("main", vec![], list_i64.clone());
        let m_entry = main_b.create_block(Some("entry"));
        main_b.set_current_block(m_entry);
        let lst = main_b.fresh_value();
        main_b.push_instr(
            IrInstr::BuiltinCall {
                result: lst,
                name: "list_new".to_owned(),
                args: vec![],
                result_ty: list_i64.clone(),
            },
            Some(list_i64.clone()),
        );
        let r = main_b.fresh_value();
        main_b.push_generic_call(
            "ident",
            vec![
                IrType::Scalar(DType::I64),
                IrType::TypeParam("list".to_owned()),
            ],
            IrInstr::Call {
                result: Some(r),
                callee: "ident".to_owned(),
                args: vec![lst],
                result_ty: Some(f_of_t),
            },
            Some(list_i64.clone()),
        );
        main_b.push_instr(IrInstr::Return { values: vec![r] }, None);

        let mut module = IrModule::new("hkt_mono_test");
        module.add_function(main_b.build()).unwrap();
        module.add_generic_template_with_hkt(
            "ident",
            vec!["T".to_owned()],
            vec!["F".to_owned()],
            hkt_b.build(),
        );

        MonoPass.run(&mut module).unwrap();
        let spec_name = mangle_mono_name(
            "ident",
            &[
                IrType::Scalar(DType::I64),
                IrType::TypeParam("list".to_owned()),
            ],
        );
        assert!(module.function_index.contains_key(&spec_name));
        let spec_fn = &module.functions[module.function_index[&spec_name].0 as usize];
        assert_eq!(spec_fn.return_ty, list_i64);
    }

    #[test]
    fn blanket_impl_template_resolves_trait_method_dispatch() {
        let t_param = IrType::TypeParam("T".to_owned());
        let mut tmpl = IrFunctionBuilder::new(
            "Describable__blanket_T__describe",
            vec![Param {
                name: "self".to_owned(),
                ty: t_param.clone(),
            }],
            IrType::Str,
        );
        let e = tmpl.create_block(Some("entry"));
        let s = tmpl.add_block_param(e, Some("self"), t_param.clone());
        tmpl.set_current_block(e);
        let r = tmpl.fresh_value();
        tmpl.push_instr(
            IrInstr::Call {
                result: Some(r),
                callee: "__trait_method__T__Show__show".to_owned(),
                args: vec![s],
                result_ty: Some(IrType::Str),
            },
            Some(IrType::Str),
        );
        tmpl.push_instr(IrInstr::Return { values: vec![r] }, None);

        let point_ty = IrType::Struct {
            name: "Point".to_owned(),
            fields: vec![],
        };
        let mut main_b = IrFunctionBuilder::new("main", vec![], IrType::Str);
        let me = main_b.create_block(Some("entry"));
        main_b.set_current_block(me);
        let p = main_b.fresh_value();
        main_b.push_instr(
            IrInstr::MakeStruct {
                result: p,
                fields: vec![],
                result_ty: point_ty.clone(),
            },
            Some(point_ty.clone()),
        );
        let res = main_b.fresh_value();
        main_b.push_generic_call(
            "Describable__blanket_T__describe",
            vec![point_ty.clone()],
            IrInstr::Call {
                result: Some(res),
                callee: "Describable__blanket_T__describe".to_owned(),
                args: vec![p],
                result_ty: Some(IrType::Str),
            },
            Some(IrType::Str),
        );
        main_b.push_instr(IrInstr::Return { values: vec![res] }, None);

        let mut module = IrModule::new("blanket_mono_test");
        module.add_trait_impl_method("Show", "Point", "show", "Show__Point__show");
        module.add_function(main_b.build()).unwrap();
        module.add_generic_template(
            "Describable__blanket_T__describe",
            vec!["T".to_owned()],
            tmpl.build(),
        );

        MonoPass.run(&mut module).unwrap();
        let spec = &module.functions
            [module.function_index["Describable__blanket_T__describe___Point"].0 as usize];
        assert!(matches!(
            &spec.blocks[0].instrs[0],
            IrInstr::Call { callee, .. } if callee == "Show__Point__show"
        ));
    }
}
