//! Type lowering, mangling, const-generic evaluation, and AST/IR type substitution.

use std::collections::HashMap;

use crate::error::LowerError;
use crate::ir::instr::BinOp;
use crate::ir::module::IrModule;
use crate::ir::types::{DType, Dim, IrType, Shape, TraitMethodSig};
use crate::parser::ast::{AstBinOp, AstDim, AstExpr, AstScalarKind, AstType};

use super::{resolve_brought_name, type_not_lowerable};

/// `ty` with every record that is really one of `enums` (by name, with no
/// fields: what `lower_type` makes of a name it does not know) as that enum.
pub(super) fn resolve_enum_refs(ty: IrType, enums: &HashMap<String, Vec<String>>) -> IrType {
    let r = |t: Box<IrType>| Box::new(resolve_enum_refs(*t, enums));
    match ty {
        IrType::Struct { name, fields } if fields.is_empty() && enums.contains_key(&name) => {
            let variants = enums[&name].clone();
            IrType::Enum { name, variants }
        }
        IrType::List(t) => IrType::List(r(t)),
        IrType::Option(t) => IrType::Option(r(t)),
        IrType::Map(k, v) => IrType::Map(r(k), r(v)),
        IrType::ResultType(a, b) => IrType::ResultType(r(a), r(b)),
        IrType::Tuple(ts) => IrType::Tuple(
            ts.into_iter()
                .map(|t| resolve_enum_refs(t, enums))
                .collect(),
        ),
        other => other,
    }
}

/// Give every `dyn Trait` inside `ty` the trait's full method list. Record
/// types are not entered: their field lists are copies of `struct_defs`
/// entries, which are filled directly.
pub(super) fn fill_trait_object_methods(
    ty: &mut IrType,
    traits: &HashMap<String, Vec<TraitMethodSig>>,
) {
    match ty {
        IrType::TraitObject { name, methods } if methods.is_empty() => {
            if let Some(defined) = traits.get(name.as_str()) {
                *methods = defined.clone();
            }
        }
        IrType::List(inner)
        | IrType::Option(inner)
        | IrType::Chan(inner)
        | IrType::Atomic(inner)
        | IrType::Mutex(inner) => fill_trait_object_methods(inner, traits),
        IrType::ResultType(ok, err) | IrType::Map(ok, err) => {
            fill_trait_object_methods(ok, traits);
            fill_trait_object_methods(err, traits);
        }
        IrType::Tuple(elements) => {
            for element in elements {
                fill_trait_object_methods(element, traits);
            }
        }
        _ => {}
    }
}

/// Whether a function declared to return `ty` must produce a value: falling
/// off the end of such a body returned nothing, which the interpreter
/// reported as an internal error and native code read as garbage. `unit`
/// (an empty record) and `()` carry no value.
pub(super) fn returns_a_value(ty: &IrType) -> bool {
    match ty {
        IrType::Struct { fields, .. } => !fields.is_empty(),
        IrType::Tuple(elems) => !elems.is_empty(),
        IrType::Scalar(_)
        | IrType::Str
        | IrType::List(_)
        | IrType::Map(_, _)
        | IrType::Option(_)
        | IrType::ResultType(_, _)
        | IrType::Array { .. } => true,
        _ => false,
    }
}

/// True when `value` may be used where `declared` is expected: identical
/// constructors, with structurally equal parts.
pub(super) fn assignable(value: &IrType, declared: &IrType) -> bool {
    match (value, declared) {
        (IrType::List(a), IrType::List(b))
        | (IrType::Option(a), IrType::Option(b))
        | (IrType::Chan(a), IrType::Chan(b))
        | (IrType::Task(a), IrType::Task(b))
        | (IrType::Atomic(a), IrType::Atomic(b))
        | (IrType::Mutex(a), IrType::Mutex(b))
        | (IrType::Grad(a), IrType::Grad(b))
        | (IrType::Sparse(a), IrType::Sparse(b))
        | (IrType::WeakRef(a), IrType::WeakRef(b)) => assignable(a, b),
        (IrType::MutRef(a), IrType::MutRef(b)) => assignable(a, b),
        (IrType::Map(ka, va), IrType::Map(kb, vb))
        | (IrType::ResultType(ka, va), IrType::ResultType(kb, vb)) => {
            assignable(ka, kb) && assignable(va, vb)
        }
        (IrType::Tuple(a), IrType::Tuple(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| assignable(x, y))
        }
        _ => value == declared,
    }
}

/// Convert an AST attribute to an IR attribute (args omitted for simplicity).
pub(super) fn ast_attr_to_ir_attr(
    attr: &crate::parser::ast::AstAttribute,
) -> crate::ir::function::IrAttribute {
    crate::ir::function::IrAttribute {
        name: attr.name.clone(),
        args: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Type lowering helpers
// ---------------------------------------------------------------------------

/// `-> Infer`: a return type the checker works out from the body.
pub(crate) fn is_inferred_return(t: &AstType) -> bool {
    matches!(t, AstType::Named(n, _) if n == "Infer")
}

/// Whether `t` names an associated type (`Self::Item`) anywhere.
pub(super) fn ast_type_mentions_assoc(t: &AstType) -> bool {
    match t {
        AstType::AssocType { .. } => true,
        AstType::Option(e, _)
        | AstType::List(e, _)
        | AstType::Chan(e, _)
        | AstType::Atomic(e, _)
        | AstType::Mutex(e, _)
        | AstType::Grad(e, _)
        | AstType::Sparse(e, _)
        | AstType::WeakRef(e, _)
        | AstType::Ref(e, _)
        | AstType::RefMut(e, _)
        | AstType::Slice(e, _) => ast_type_mentions_assoc(e),
        AstType::Array { elem, .. } => ast_type_mentions_assoc(elem),
        AstType::Result(a, b, _) | AstType::Map(a, b, _) => {
            ast_type_mentions_assoc(a) || ast_type_mentions_assoc(b)
        }
        AstType::Tuple(ts, _) => ts.iter().any(ast_type_mentions_assoc),
        AstType::Generic { args, .. } => args.iter().any(ast_type_mentions_assoc),
        AstType::Fn { params, ret, .. } => {
            params.iter().any(ast_type_mentions_assoc) || ast_type_mentions_assoc(ret)
        }
        _ => false,
    }
}

pub fn lower_type(ty: &AstType) -> Result<IrType, LowerError> {
    Ok(match ty {
        AstType::Scalar(kind, _) => IrType::Scalar(lower_dtype(*kind)),
        AstType::Tensor { dtype, dims, .. } => {
            let shape = Shape(dims.iter().map(lower_dim).collect());
            IrType::Tensor {
                dtype: lower_dtype(*dtype),
                shape,
            }
        }
        AstType::Named(name, _) if name == "Infer" => return Err(type_not_lowerable(ty)),
        AstType::Named(name, _) => {
            if name == "str" {
                IrType::Str
            } else if name == "task_group" {
                IrType::TaskGroup
            } else {
                IrType::Struct {
                    name: name.clone(),
                    fields: Vec::new(), // fields resolved at use-site
                }
            }
        }
        AstType::Tuple(elems, _) => {
            IrType::Tuple(elems.iter().map(lower_type).collect::<Result<_, _>>()?)
        }
        AstType::Array { elem, len, .. } => IrType::Array {
            elem: Box::new(lower_type(elem)?),
            len: *len,
        },
        AstType::Option(inner, _) => IrType::Option(Box::new(lower_type(inner)?)),
        AstType::Result(ok_ty, err_ty, _) => {
            IrType::ResultType(Box::new(lower_type(ok_ty)?), Box::new(lower_type(err_ty)?))
        }
        AstType::Chan(elem, _) => IrType::Chan(Box::new(lower_type(elem)?)),
        AstType::Atomic(inner, _) => IrType::Atomic(Box::new(lower_type(inner)?)),
        AstType::Mutex(inner, _) => IrType::Mutex(Box::new(lower_type(inner)?)),
        AstType::Grad(inner, _) => IrType::Grad(Box::new(lower_type(inner)?)),
        AstType::Sparse(inner, _) => IrType::Sparse(Box::new(lower_type(inner)?)),
        AstType::List(elem, _) => IrType::List(Box::new(lower_type(elem)?)),
        AstType::Map(k, v, _) => IrType::Map(Box::new(lower_type(k)?), Box::new(lower_type(v)?)),
        AstType::Fn { params, ret, .. } => IrType::Fn {
            params: params.iter().map(lower_type).collect::<Result<_, _>>()?,
            ret: Box::new(lower_type(ret)?),
        },
        AstType::WeakRef(inner, _) => IrType::WeakRef(Box::new(lower_type(inner)?)),
        // `task<T>`: the handle an `async def` returns.
        AstType::Generic { name, args, .. } if name == "task" && args.len() == 1 => {
            IrType::Task(Box::new(lower_type(&args[0])?))
        }
        AstType::Generic { name, args, .. } => IrType::Struct {
            name: format!(
                "{}__{}",
                name,
                args.iter()
                    .map(|a| format!("{:?}", a))
                    .collect::<Vec<_>>()
                    .join("_")
            ),
            fields: Vec::new(),
        },
        AstType::ConstInt(n, _) => IrType::ConstInt(*n),
        AstType::AssocType { .. } | AstType::MaskEffectType { .. } => {
            return Err(type_not_lowerable(ty))
        }
        // When lower_type (no module) sees `dyn Trait`, fall back to an empty
        // TraitObject; the full lower_type_with_structs path fills it in.
        AstType::DynTrait { trait_name, .. } => IrType::TraitObject {
            name: trait_name.clone(),
            methods: Vec::new(),
        },
        AstType::Ref(inner, _) => match inner.as_ref() {
            AstType::Slice(elem, _) => IrType::Slice {
                elem: Box::new(lower_type(elem)?),
                is_mut: false,
            },
            other => lower_type(other)?,
        },
        AstType::RefMut(inner, _) => match inner.as_ref() {
            AstType::Slice(elem, _) => IrType::Slice {
                elem: Box::new(lower_type(elem)?),
                is_mut: true,
            },
            other => IrType::MutRef(Box::new(lower_type(other)?)),
        },
        AstType::Slice(elem, _) => IrType::Slice {
            elem: Box::new(lower_type(elem)?),
            is_mut: false,
        },
    })
}

/// True when two types share a constructor and differ only where one side is
/// an unsubstituted type parameter -- `list<%T>` against `list<i64>`.
///
/// Used only to pick a trait impl for a receiver whose element type was not
/// fully settled, and only when exactly one candidate matches.
pub(super) fn same_constructor(a: &IrType, b: &IrType) -> bool {
    // A struct with no fields is the lowerer's marker for an unsubstituted type
    // parameter -- `impl[T] Show for list<T>` lowers its target to
    // `list<%T>`. Treating it as a wildcard is what lets one generic impl serve
    // every element type instead of needing one instantiation each.
    //
    // A user's genuinely empty record would also match here. That is tolerable
    // because this helper is consulted only after an exact match has failed,
    // and only when exactly one candidate matches -- so it can widen a lookup
    // that would otherwise fail outright, never silently redirect one that
    // would have succeeded.
    pub(super) fn is_param_marker(t: &IrType) -> bool {
        matches!(t, IrType::Struct { fields, .. } if fields.is_empty())
    }
    if is_param_marker(a) || is_param_marker(b) {
        return true;
    }
    match (a, b) {
        (IrType::List(x), IrType::List(y)) => same_constructor(x, y),
        (IrType::Option(x), IrType::Option(y)) => same_constructor(x, y),
        (IrType::Map(k1, v1), IrType::Map(k2, v2)) => {
            same_constructor(k1, k2) && same_constructor(v1, v2)
        }
        (IrType::Struct { name: n1, .. }, IrType::Struct { name: n2, .. }) => n1 == n2,
        _ => a == b,
    }
}

pub(super) fn type_name_to_ir_type(name: &str, module: &IrModule) -> Option<IrType> {
    Some(match name {
        "i64" => IrType::Scalar(DType::I64),
        "i32" => IrType::Scalar(DType::I32),
        "f64" => IrType::Scalar(DType::F64),
        "f32" => IrType::Scalar(DType::F32),
        "bool" => IrType::Scalar(DType::Bool),
        "u8" => IrType::Scalar(DType::U8),
        "i8" => IrType::Scalar(DType::I8),
        "u32" => IrType::Scalar(DType::U32),
        "u64" => IrType::Scalar(DType::U64),
        "usize" => IrType::Scalar(DType::USize),
        "str" => IrType::Str,
        _ => {
            if let Some(fields) = module.struct_def(name) {
                IrType::Struct {
                    name: name.to_owned(),
                    fields: fields.clone(),
                }
            } else if let Some(variants) = module.enum_def(name) {
                IrType::Enum {
                    name: name.to_owned(),
                    variants: variants.clone(),
                }
            } else {
                module.type_alias(name)?.clone()
            }
        }
    })
}

/// Returns a short string key for `ty` used to look up trait dispatch entries.
pub(super) fn ir_type_dispatch_name(ty: &IrType) -> String {
    match ty {
        IrType::Scalar(DType::I64) => "i64".to_owned(),
        IrType::Scalar(DType::I32) => "i32".to_owned(),
        IrType::Scalar(DType::F64) => "f64".to_owned(),
        IrType::Scalar(DType::F32) => "f32".to_owned(),
        IrType::Scalar(DType::Bool) => "bool".to_owned(),
        IrType::Str => "str".to_owned(),
        IrType::Struct { name, .. } => name.clone(),
        IrType::Enum { name, .. } => name.clone(),
        other => format!("{}", other),
    }
}

/// Returns an LLVM-IR-safe identifier fragment for `ty`. Used to build
/// monomorphised function names (`fn__T` -> `fn__MyStruct`) where the result
/// must be a valid C identifier. Strips the `%` prefix from struct Display
/// output and replaces whitespace/commas/brackets with `_`.
/// Parse one mangled type off the front of `s`, returning it
/// and whatever follows.
///
/// The inverse of `mangle_ir_type`, and it has to be, because
/// a type argument's own mangling contains underscores. The
/// previous decoder took a single `_`-separated token, so
/// `Box__list_i64` bound the type parameter to a bare `list`
/// and dropped the `i64`: the module then *declared*
/// `%Box__list_i64` while the use site said `%Box__list`, and
/// LLVM rejected the module with "base element of
/// getelementptr must be sized" -- an undeclared name has no
/// size. See known-issues #22.
///
/// **Residual ambiguity, stated:** mangling is not injective.
/// A struct argument whose own name contains `_` cannot be
/// told from a constructor applied to arguments, so an
/// unrecognised head consumes the entire remainder -- correct
/// when it is the last argument, which is the case that
/// occurs. The real fix is to carry a monomorphised struct's
/// type arguments alongside it instead of re-deriving them
/// from its name; this makes the derivation right for every
/// built-in constructor, which is what #22 needed.
pub(super) fn parse_mangled_type(s: &str) -> (IrType, &str) {
    let (head, rest) = match s.split_once('_') {
        Some((h, t)) => (h, t),
        None => (s, ""),
    };
    pub(super) fn one(ctor: fn(Box<IrType>) -> IrType, rest: &str) -> (IrType, &str) {
        let (inner, r) = parse_mangled_type(rest);
        (ctor(Box::new(inner)), r)
    }
    if let Some(len_str) = head.strip_prefix("arr") {
        if !len_str.is_empty() && len_str.chars().all(|c| c.is_ascii_digit()) {
            if let Ok(len) = len_str.parse::<usize>() {
                let (elem, r) = parse_mangled_type(rest);
                return (
                    IrType::Array {
                        elem: Box::new(elem),
                        len,
                    },
                    r,
                );
            }
        }
    }
    if head == "neg" {
        let (digits, tail) = match rest.split_once('_') {
            Some((d, t)) => (d, t),
            None => (rest, ""),
        };
        if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
            if let Ok(n) = digits.parse::<i64>() {
                return (IrType::ConstInt(-n), tail);
            }
        }
    }
    if !head.is_empty() && head.chars().all(|c| c.is_ascii_digit()) {
        if let Ok(n) = head.parse::<i64>() {
            return (IrType::ConstInt(n), rest);
        }
    }
    match head {
        "i64" => (IrType::Scalar(DType::I64), rest),
        "i32" => (IrType::Scalar(DType::I32), rest),
        "f64" => (IrType::Scalar(DType::F64), rest),
        "f32" => (IrType::Scalar(DType::F32), rest),
        "bool" => (IrType::Scalar(DType::Bool), rest),
        "str" => (IrType::Str, rest),
        "list" => one(IrType::List, rest),
        "opt" => one(IrType::Option, rest),
        "chan" => one(IrType::Chan, rest),
        "task" => one(IrType::Task, rest),
        "atomic" => one(IrType::Atomic, rest),
        "mutex" => one(IrType::Mutex, rest),
        "grad" => one(IrType::Grad, rest),
        "sparse" => one(IrType::Sparse, rest),
        "weakref" => one(IrType::WeakRef, rest),
        "mutref" => one(IrType::MutRef, rest),
        "map" => {
            let (k, r1) = parse_mangled_type(rest);
            let (v, r2) = parse_mangled_type(r1);
            (IrType::Map(Box::new(k), Box::new(v)), r2)
        }
        _ => (
            IrType::Struct {
                name: s.to_string(),
                fields: Vec::new(),
            },
            "",
        ),
    }
}

/// Resolve a top-level struct name produced by `parse_mangled_type` against the
/// module, so a user record comes back with its fields rather than as a bare
/// name. Mirrors what the call sites did before, which looked every argument up
/// with `lower_type_with_structs`.
pub(crate) fn resolve_mangled_struct(ty: IrType, module: &IrModule) -> IrType {
    match ty {
        IrType::Struct {
            ref name,
            ref fields,
        } if fields.is_empty() => lower_type_with_structs(
            &crate::parser::ast::AstType::Named(name.clone(), crate::parser::lexer::Span::at(0)),
            module,
        )
        // A mangled type name is always one the module declares or a scalar,
        // so the error path here is a safety net rather than a path the
        // program can observe.
        .unwrap_or_else(|_| ty.clone()),
        other => other,
    }
}

pub(crate) fn mangle_ir_type(ty: &IrType) -> String {
    let s = match ty {
        IrType::Infer => "infer".to_owned(),
        IrType::TapeRef => "taperef".to_owned(),
        IrType::ConstInt(n) => {
            if *n >= 0 {
                n.to_string()
            } else {
                format!("neg_{}", -n)
            }
        }
        IrType::Scalar(d) => format!("{}", d),
        IrType::Str => "str".to_owned(),
        IrType::Struct { name, .. } => name.clone(),
        IrType::Enum { name, .. } => name.clone(),
        IrType::TraitObject { name, .. } => format!("dyn{}", name),
        IrType::TaskGroup => "taskgroup".to_owned(),
        IrType::WeakRef(inner) => format!("weakref_{}", mangle_ir_type(inner)),
        IrType::MutRef(inner) => format!("mutref_{}", mangle_ir_type(inner)),
        IrType::Tuple(elems) => {
            let inner: Vec<String> = elems.iter().map(mangle_ir_type).collect();
            format!("tuple_{}", inner.join("_"))
        }
        IrType::Array { elem, len } => {
            format!("arr{}_{}", len, mangle_ir_type(elem))
        }
        IrType::Slice { elem, is_mut } => {
            format!(
                "slice_{}_{}",
                if *is_mut { "mut" } else { "const" },
                mangle_ir_type(elem)
            )
        }
        IrType::Option(inner) => format!("opt_{}", mangle_ir_type(inner)),
        IrType::ResultType(ok, err) => {
            format!("res_{}_{}", mangle_ir_type(ok), mangle_ir_type(err))
        }
        IrType::Chan(inner) => format!("chan_{}", mangle_ir_type(inner)),
        IrType::Task(inner) => format!("task_{}", mangle_ir_type(inner)),
        IrType::Atomic(inner) => format!("atomic_{}", mangle_ir_type(inner)),
        IrType::Mutex(inner) => format!("mutex_{}", mangle_ir_type(inner)),
        IrType::Grad(inner) => format!("grad_{}", mangle_ir_type(inner)),
        IrType::Sparse(inner) => format!("sparse_{}", mangle_ir_type(inner)),
        IrType::List(inner) => format!("list_{}", mangle_ir_type(inner)),
        IrType::Map(k, v) => format!("map_{}_{}", mangle_ir_type(k), mangle_ir_type(v)),
        IrType::Tensor { dtype, shape } => format!("tensor_{}_{}", dtype, shape),
        IrType::Fn { params, ret } => {
            let ps: Vec<String> = params.iter().map(mangle_ir_type).collect();
            format!("fn_{}_to_{}", ps.join("_"), mangle_ir_type(ret))
        }
        IrType::TypeParam(name) => name.clone(),
        IrType::HktApp { ctor, args } => {
            let ps: Vec<String> = args.iter().map(mangle_ir_type).collect();
            format!("{}_{}", ctor, ps.join("_"))
        }
    };
    s.replace(|c: char| !c.is_alphanumeric() && c != '_', "_")
}

/// Type lowering with struct/enum definition lookup from the module.
pub fn lower_type_with_structs(ty: &AstType, module: &IrModule) -> Result<IrType, LowerError> {
    Ok(match ty {
        AstType::Array {
            elem,
            len,
            len_expr,
            ..
        } => {
            let resolved_len = len_expr
                .as_ref()
                .and_then(|e| eval_const_usize_expr(e, &extract_const_from_module(module)))
                .unwrap_or(*len);
            IrType::Array {
                elem: Box::new(lower_type_with_structs(elem, module)?),
                len: resolved_len,
            }
        }
        AstType::Named(name, _) => {
            if name == "str" {
                return Ok(IrType::Str);
            }
            if name == "Infer" {
                return Err(type_not_lowerable(ty));
            }
            // Map scalar type names.
            if name == "i64" {
                return Ok(IrType::Scalar(DType::I64));
            }
            if name == "i32" {
                return Ok(IrType::Scalar(DType::I32));
            }
            if name == "f64" {
                return Ok(IrType::Scalar(DType::F64));
            }
            if name == "f32" {
                return Ok(IrType::Scalar(DType::F32));
            }
            if name == "bool" {
                return Ok(IrType::Scalar(DType::Bool));
            }
            if name == "task_group" {
                return Ok(IrType::TaskGroup);
            }
            let resolved_name = resolve_brought_name(name, module);
            // Check type aliases first.
            if let Some(aliased) = module.type_alias(&resolved_name) {
                return Ok(aliased.clone());
            }
            if let Some(fields) = module.struct_def(&resolved_name) {
                IrType::Struct {
                    name: resolved_name,
                    fields: fields.clone(),
                }
            } else if let Some(variants) = module.enum_def(&resolved_name) {
                IrType::Enum {
                    name: resolved_name,
                    variants: variants.clone(),
                }
            } else if let Some(&val) = module
                .const_ints
                .get(&resolved_name)
                .or_else(|| module.const_ints.get(name))
            {
                IrType::ConstInt(val)
            } else {
                IrType::Struct {
                    name: resolved_name,
                    fields: Vec::new(),
                }
            }
        }
        AstType::Tuple(elems, _) => IrType::Tuple(
            elems
                .iter()
                .map(|e| lower_type_with_structs(e, module))
                .collect::<Result<_, _>>()?,
        ),
        AstType::Option(inner, _) => {
            IrType::Option(Box::new(lower_type_with_structs(inner, module)?))
        }
        AstType::Result(ok_ty, err_ty, _) => IrType::ResultType(
            Box::new(lower_type_with_structs(ok_ty, module)?),
            Box::new(lower_type_with_structs(err_ty, module)?),
        ),
        AstType::Chan(elem, _) => IrType::Chan(Box::new(lower_type_with_structs(elem, module)?)),
        AstType::Atomic(inner, _) => {
            IrType::Atomic(Box::new(lower_type_with_structs(inner, module)?))
        }
        AstType::Mutex(inner, _) => {
            IrType::Mutex(Box::new(lower_type_with_structs(inner, module)?))
        }
        AstType::List(elem, _) => IrType::List(Box::new(lower_type_with_structs(elem, module)?)),
        AstType::Map(k, v, _) => IrType::Map(
            Box::new(lower_type_with_structs(k, module)?),
            Box::new(lower_type_with_structs(v, module)?),
        ),
        AstType::WeakRef(inner, _) => {
            IrType::WeakRef(Box::new(lower_type_with_structs(inner, module)?))
        }
        AstType::Fn { params, ret, .. } => IrType::Fn {
            params: params
                .iter()
                .map(|p| lower_type_with_structs(p, module))
                .collect::<Result<_, _>>()?,
            ret: Box::new(lower_type_with_structs(ret, module)?),
        },
        AstType::Grad(inner, _) => IrType::Grad(Box::new(lower_type_with_structs(inner, module)?)),
        AstType::Sparse(inner, _) => {
            IrType::Sparse(Box::new(lower_type_with_structs(inner, module)?))
        }
        // `task<T>`: the handle an `async def` returns (unless the program
        // declares its own generic record named `task`).
        AstType::Generic { name, args, .. }
            if name == "task" && args.len() == 1 && module.struct_def("task").is_none() =>
        {
            IrType::Task(Box::new(lower_type_with_structs(&args[0], module)?))
        }
        AstType::Generic { name, args, .. } => {
            let resolved_args: Vec<IrType> = args
                .iter()
                .map(|arg| lower_type_with_structs(arg, module))
                .collect::<Result<_, _>>()?;
            let mangled_name = format!(
                "{}__{}",
                name,
                resolved_args
                    .iter()
                    .map(mangle_ir_type)
                    .collect::<Vec<_>>()
                    .join("_")
            );
            let resolved_name = resolve_brought_name(&mangled_name, module);
            if let Some(fields) = module.struct_def(&resolved_name) {
                IrType::Struct {
                    name: resolved_name,
                    fields: fields.clone(),
                }
            } else if let Some(fields) =
                instantiate_struct_fields_from_ast(name, &resolved_args, module)
            {
                IrType::Struct {
                    name: resolved_name,
                    fields,
                }
            } else {
                // Fallback: use the generic struct template's registered field layout.
                let resolved_base = resolve_brought_name(name, module);
                if let Some(template_fields) = module.struct_def(&resolved_base) {
                    IrType::Struct {
                        name: resolved_name,
                        fields: template_fields.clone(),
                    }
                } else {
                    IrType::Struct {
                        name: resolved_name,
                        fields: Vec::new(),
                    }
                }
            }
        }
        AstType::ConstInt(n, _) => IrType::ConstInt(*n),
        AstType::AssocType { .. } => return Err(type_not_lowerable(ty)),
        AstType::DynTrait { trait_name, .. } => {
            let methods = module.trait_def(trait_name).cloned().unwrap_or_default();
            IrType::TraitObject {
                name: trait_name.clone(),
                methods,
            }
        }
        AstType::MaskEffectType { .. } => return Err(type_not_lowerable(ty)),
        AstType::Ref(inner, _) => match inner.as_ref() {
            AstType::Slice(elem, _) => IrType::Slice {
                elem: Box::new(lower_type_with_structs(elem, module)?),
                is_mut: false,
            },
            other => lower_type_with_structs(other, module)?,
        },
        AstType::RefMut(inner, _) => match inner.as_ref() {
            AstType::Slice(elem, _) => IrType::Slice {
                elem: Box::new(lower_type_with_structs(elem, module)?),
                is_mut: true,
            },
            other => IrType::MutRef(Box::new(lower_type_with_structs(other, module)?)),
        },
        AstType::Slice(inner, _) => IrType::Slice {
            elem: Box::new(lower_type_with_structs(inner, module)?),
            is_mut: false,
        },
        other => lower_type(other)?,
    })
}

pub(super) fn lower_dtype(kind: AstScalarKind) -> DType {
    match kind {
        AstScalarKind::F32 => DType::F32,
        AstScalarKind::F64 => DType::F64,
        AstScalarKind::I32 => DType::I32,
        AstScalarKind::I64 => DType::I64,
        AstScalarKind::Bool => DType::Bool,
        AstScalarKind::U8 => DType::U8,
        AstScalarKind::I8 => DType::I8,
        AstScalarKind::U32 => DType::U32,
        AstScalarKind::U64 => DType::U64,
        AstScalarKind::USize => DType::USize,
    }
}

pub(super) fn lower_dim(dim: &AstDim) -> Dim {
    match dim {
        AstDim::Literal(n) => Dim::Literal(*n),
        AstDim::Symbol(sym) => Dim::Symbolic(sym.name.clone()),
    }
}

pub(super) fn lower_binop(op: AstBinOp) -> BinOp {
    match op {
        AstBinOp::Add => BinOp::Add,
        AstBinOp::Sub => BinOp::Sub,
        AstBinOp::Mul => BinOp::Mul,
        AstBinOp::Div => BinOp::Div,
        AstBinOp::Mod => BinOp::Mod,
        AstBinOp::CmpEq => BinOp::CmpEq,
        AstBinOp::CmpNe => BinOp::CmpNe,
        AstBinOp::CmpLt => BinOp::CmpLt,
        AstBinOp::CmpLe => BinOp::CmpLe,
        AstBinOp::CmpGt => BinOp::CmpGt,
        AstBinOp::CmpGe => BinOp::CmpGe,
        // And/Or are handled via short-circuit lowering, never reach here.
        AstBinOp::And | AstBinOp::Or => {
            unreachable!("logical operators use short-circuit lowering")
        }
    }
}

/// Returns (trait_name, method_name) for an operator that can be overloaded,
/// or None for ops that cannot be overloaded (comparisons, logical).
/// Maps an operator to the trait method it dispatches to, if any.
///
/// The comparison operators were missing and fell into a `_ => None`, so
/// `impl Ord for V { def lt … }` was never consulted: `a < c` answered from the
/// operands' raw representation instead, giving `false` where the impl says
/// `true`. The direct call `a.lt(c)` was correct throughout, which is what made
/// it look like a dispatch problem rather than a missing mapping. See
/// known-issues #56.
///
/// Written exhaustively rather than with a catch-all: this is the seventh
/// defect in this codebase caused by a `_` arm quietly absorbing a case that
/// needed handling.
pub(super) fn op_trait_method(op: AstBinOp) -> Option<(&'static str, &'static str)> {
    match op {
        AstBinOp::Add => Some(("Add", "add")),
        AstBinOp::Sub => Some(("Sub", "sub")),
        AstBinOp::Mul => Some(("Mul", "mul")),
        AstBinOp::Div => Some(("Div", "div")),
        AstBinOp::Mod => Some(("Rem", "rem")),
        AstBinOp::CmpEq => Some(("Eq", "eq")),
        AstBinOp::CmpNe => Some(("Eq", "ne")),
        AstBinOp::CmpLt => Some(("Ord", "lt")),
        AstBinOp::CmpLe => Some(("Ord", "le")),
        AstBinOp::CmpGt => Some(("Ord", "gt")),
        AstBinOp::CmpGe => Some(("Ord", "ge")),
        // Short-circuiting logical operators are not overloadable: they must
        // not evaluate their right operand unconditionally, which a method
        // call would.
        AstBinOp::And | AstBinOp::Or => None,
    }
}

/// Derives the result type of an einsum operation from the notation string and
/// input tensor types.
///
/// For bootstrap: parses the output index string from the notation (the part
/// after "->") and infers the result shape by matching symbolic dim names.
/// `None` when the notation cannot be parsed or no input carries an element
/// type -- the caller reports what is missing.
pub(super) fn derive_einsum_result_type(notation: &str, input_tys: &[IrType]) -> Option<IrType> {
    // Extract output indices: "mk,kn->mn" → "mn"
    let pos = notation.find("->")?;
    let output_indices = &notation[pos + 2..];

    // Build a map from index character → symbolic Dim, using input shapes.
    let input_part = &notation[..notation.find("->").unwrap()];
    let input_index_strs: Vec<&str> = input_part.split(',').collect();

    let mut char_to_dim: HashMap<char, Dim> = HashMap::new();
    let mut result_dtype: Option<DType> = None;

    for (idx_str, ty) in input_index_strs.iter().zip(input_tys.iter()) {
        if let IrType::Tensor { dtype, shape } = ty {
            if result_dtype.is_none() {
                result_dtype = Some(*dtype);
            }
            for (ch, dim) in idx_str.chars().zip(shape.0.iter()) {
                char_to_dim.entry(ch).or_insert_with(|| dim.clone());
            }
        }
    }

    let dtype = result_dtype?;

    let result_dims: Vec<Dim> = output_indices
        .chars()
        .map(|ch| {
            char_to_dim
                .get(&ch)
                .cloned()
                .unwrap_or_else(|| Dim::Symbolic(ch.to_string()))
        })
        .collect();

    Some(IrType::Tensor {
        dtype,
        shape: Shape(result_dims),
    })
}

/// Evaluates a constant `usize` expression (such as an array length `[T; N]` or
/// `[T; N + 1]`) given concrete const-generic parameter bindings.
pub(super) fn eval_const_usize_expr(
    expr: &AstExpr,
    const_subs: &HashMap<String, usize>,
) -> Option<usize> {
    match expr {
        AstExpr::IntLit { value, .. } => usize::try_from(*value).ok(),
        AstExpr::Ident(id) => const_subs.get(&id.name).copied(),
        AstExpr::BinOp { op, lhs, rhs, .. } => {
            let l = eval_const_usize_expr(lhs, const_subs)?;
            let r = eval_const_usize_expr(rhs, const_subs)?;
            match op {
                crate::parser::ast::AstBinOp::Add => l.checked_add(r),
                crate::parser::ast::AstBinOp::Sub => l.checked_sub(r),
                crate::parser::ast::AstBinOp::Mul => l.checked_mul(r),
                crate::parser::ast::AstBinOp::Div => l.checked_div(r),
                crate::parser::ast::AstBinOp::Mod => l.checked_rem(r),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Extracts evaluated top-level integer constants (`module.const_ints`) as `usize`.
pub(super) fn extract_const_from_module(module: &IrModule) -> HashMap<String, usize> {
    module
        .const_ints
        .iter()
        .filter_map(|(k, &v)| usize::try_from(v).ok().map(|u| (k.clone(), u)))
        .collect()
}

/// Extracts `usize` const-generic bindings from `IrType::ConstInt` substitutions.
pub(super) fn extract_const_subs(subs: &HashMap<String, IrType>) -> HashMap<String, usize> {
    subs.iter()
        .filter_map(|(k, v)| match v {
            IrType::ConstInt(n) => usize::try_from(*n).ok().map(|u| (k.clone(), u)),
            _ => None,
        })
        .collect()
}

/// Resolves a constant generic type argument (`5` or `CONST_NAME`) to `usize`.
pub(super) fn const_arg_usize(
    arg: &AstType,
    const_defs: &HashMap<String, AstExpr>,
) -> Option<usize> {
    match arg {
        AstType::ConstInt(n, _) => usize::try_from(*n).ok(),
        AstType::Named(name, _) => const_defs
            .get(name)
            .and_then(|e| eval_const_usize_expr(e, &HashMap::new())),
        _ => None,
    }
}

/// Instantiates a generic struct's fields directly from its AST definition under
/// concrete `args`, preserving dependent array length expressions like `[T; N]`.
pub(super) fn instantiate_struct_fields_from_ast(
    base_name: &str,
    args: &[IrType],
    module: &IrModule,
) -> Option<Vec<(String, IrType)>> {
    let resolved_base = resolve_brought_name(base_name, module);
    let tmpl = module
        .generic_struct_asts
        .get(&resolved_base)
        .or_else(|| module.generic_struct_asts.get(base_name))?;
    if tmpl.type_params.len() != args.len() {
        return None;
    }
    let mut field_subs = HashMap::new();
    for (tp, arg) in tmpl.type_params.iter().zip(args.iter()) {
        let pname = match tp {
            crate::parser::ast::AstGenericParam::Type(n, _, _)
            | crate::parser::ast::AstGenericParam::Hkt(n, _, _, _)
            | crate::parser::ast::AstGenericParam::Const { name: n, .. } => n,
        };
        field_subs.insert(pname.clone(), arg.clone());
    }
    tmpl.fields
        .iter()
        .map(|f| {
            resolve_ast_type_with_subs(&f.ty, &field_subs, module)
                .ok()
                .map(|ty| (f.name.name.clone(), ty))
        })
        .collect()
}

pub fn substitute_ast_type(
    ty: &AstType,
    type_subs: &std::collections::HashMap<String, AstType>,
    constructor_subs: &std::collections::HashMap<String, String>,
) -> AstType {
    substitute_ast_type_with_consts(ty, type_subs, constructor_subs, &HashMap::new())
}

pub fn substitute_ast_type_with_consts(
    ty: &AstType,
    type_subs: &std::collections::HashMap<String, AstType>,
    constructor_subs: &std::collections::HashMap<String, String>,
    const_subs: &std::collections::HashMap<String, usize>,
) -> AstType {
    match ty {
        AstType::Scalar(k, span) => AstType::Scalar(*k, *span),
        AstType::Named(name, span) => {
            if let Some(concrete) = type_subs.get(name) {
                concrete.clone()
            } else if let Some(&val) = const_subs.get(name) {
                AstType::ConstInt(val as i64, *span)
            } else {
                AstType::Named(name.clone(), *span)
            }
        }
        AstType::Tuple(elems, span) => AstType::Tuple(
            elems
                .iter()
                .map(|e| {
                    substitute_ast_type_with_consts(e, type_subs, constructor_subs, const_subs)
                })
                .collect(),
            *span,
        ),
        AstType::Array {
            elem,
            len,
            len_expr,
            span,
        } => {
            let evaluated = len_expr
                .as_ref()
                .and_then(|e| eval_const_usize_expr(e, const_subs));
            AstType::Array {
                elem: Box::new(substitute_ast_type_with_consts(
                    elem,
                    type_subs,
                    constructor_subs,
                    const_subs,
                )),
                len: evaluated.unwrap_or(*len),
                len_expr: if evaluated.is_some() {
                    None
                } else {
                    len_expr.clone()
                },
                span: *span,
            }
        }
        AstType::Option(inner, span) => AstType::Option(
            Box::new(substitute_ast_type_with_consts(
                inner,
                type_subs,
                constructor_subs,
                const_subs,
            )),
            *span,
        ),
        AstType::Result(ok_ty, err_ty, span) => AstType::Result(
            Box::new(substitute_ast_type_with_consts(
                ok_ty,
                type_subs,
                constructor_subs,
                const_subs,
            )),
            Box::new(substitute_ast_type_with_consts(
                err_ty,
                type_subs,
                constructor_subs,
                const_subs,
            )),
            *span,
        ),
        AstType::Chan(elem, span) => AstType::Chan(
            Box::new(substitute_ast_type_with_consts(
                elem,
                type_subs,
                constructor_subs,
                const_subs,
            )),
            *span,
        ),
        AstType::Atomic(inner, span) => AstType::Atomic(
            Box::new(substitute_ast_type_with_consts(
                inner,
                type_subs,
                constructor_subs,
                const_subs,
            )),
            *span,
        ),
        AstType::Mutex(inner, span) => AstType::Mutex(
            Box::new(substitute_ast_type_with_consts(
                inner,
                type_subs,
                constructor_subs,
                const_subs,
            )),
            *span,
        ),
        AstType::Grad(inner, span) => AstType::Grad(
            Box::new(substitute_ast_type_with_consts(
                inner,
                type_subs,
                constructor_subs,
                const_subs,
            )),
            *span,
        ),
        AstType::Sparse(inner, span) => AstType::Sparse(
            Box::new(substitute_ast_type_with_consts(
                inner,
                type_subs,
                constructor_subs,
                const_subs,
            )),
            *span,
        ),
        AstType::List(elem, span) => AstType::List(
            Box::new(substitute_ast_type_with_consts(
                elem,
                type_subs,
                constructor_subs,
                const_subs,
            )),
            *span,
        ),
        AstType::Map(k, v, span) => AstType::Map(
            Box::new(substitute_ast_type_with_consts(
                k,
                type_subs,
                constructor_subs,
                const_subs,
            )),
            Box::new(substitute_ast_type_with_consts(
                v,
                type_subs,
                constructor_subs,
                const_subs,
            )),
            *span,
        ),
        AstType::WeakRef(inner, span) => AstType::WeakRef(
            Box::new(substitute_ast_type_with_consts(
                inner,
                type_subs,
                constructor_subs,
                const_subs,
            )),
            *span,
        ),
        AstType::Fn {
            params,
            ret,
            effects,
            span,
        } => AstType::Fn {
            params: params
                .iter()
                .map(|p| {
                    substitute_ast_type_with_consts(p, type_subs, constructor_subs, const_subs)
                })
                .collect(),
            ret: Box::new(substitute_ast_type_with_consts(
                ret,
                type_subs,
                constructor_subs,
                const_subs,
            )),
            effects: effects.clone(),
            span: *span,
        },
        AstType::Generic { name, args, span } => {
            let new_args: Vec<AstType> = args
                .iter()
                .map(|a| {
                    substitute_ast_type_with_consts(a, type_subs, constructor_subs, const_subs)
                })
                .collect();
            let base_name = if let Some(target) = constructor_subs.get(name) {
                target.clone()
            } else if let Some(AstType::Named(target, _)) = type_subs.get(name) {
                target.clone()
            } else {
                name.clone()
            };
            AstType::Generic {
                name: base_name,
                args: new_args,
                span: *span,
            }
        }
        AstType::ConstInt(v, span) => AstType::ConstInt(*v, *span),
        AstType::AssocType {
            base,
            assoc_name,
            span,
        } => AstType::AssocType {
            base: base.clone(),
            assoc_name: assoc_name.clone(),
            span: *span,
        },
        AstType::DynTrait { trait_name, span } => AstType::DynTrait {
            trait_name: trait_name.clone(),
            span: *span,
        },
        AstType::MaskEffectType { effects, span } => AstType::MaskEffectType {
            effects: effects.clone(),
            span: *span,
        },
        other => other.clone(),
    }
}

pub fn resolve_ast_type_with_subs(
    ty: &AstType,
    subs: &std::collections::HashMap<String, IrType>,
    module: &IrModule,
) -> Result<IrType, LowerError> {
    Ok(match ty {
        AstType::Scalar(k, _) => IrType::Scalar(lower_dtype(*k)),
        AstType::Named(name, _) => {
            if name == "str" {
                return Ok(IrType::Str);
            }
            if name == "Infer" {
                return Err(type_not_lowerable(ty));
            }
            if name == "i64" {
                return Ok(IrType::Scalar(DType::I64));
            }
            if name == "i32" {
                return Ok(IrType::Scalar(DType::I32));
            }
            if name == "f64" {
                return Ok(IrType::Scalar(DType::F64));
            }
            if name == "f32" {
                return Ok(IrType::Scalar(DType::F32));
            }
            if name == "bool" {
                return Ok(IrType::Scalar(DType::Bool));
            }
            if name == "task_group" {
                return Ok(IrType::TaskGroup);
            }
            if let Some(concrete) = subs.get(name) {
                return Ok(concrete.clone());
            }
            let resolved_name = resolve_brought_name(name, module);
            if let Some(aliased) = module.type_alias(&resolved_name) {
                return Ok(aliased.clone());
            }
            if let Some(fields) = module.struct_def(&resolved_name) {
                IrType::Struct {
                    name: resolved_name,
                    fields: fields.clone(),
                }
            } else if let Some(variants) = module.enum_def(&resolved_name) {
                IrType::Enum {
                    name: resolved_name,
                    variants: variants.clone(),
                }
            } else if let Some(&val) = module
                .const_ints
                .get(&resolved_name)
                .or_else(|| module.const_ints.get(name))
            {
                IrType::ConstInt(val)
            } else {
                IrType::Struct {
                    name: resolved_name,
                    fields: Vec::new(),
                }
            }
        }
        AstType::Tuple(elems, _) => IrType::Tuple(
            elems
                .iter()
                .map(|e| resolve_ast_type_with_subs(e, subs, module))
                .collect::<Result<_, _>>()?,
        ),
        AstType::Array {
            elem,
            len,
            len_expr,
            ..
        } => {
            let mut const_subs = extract_const_from_module(module);
            const_subs.extend(extract_const_subs(subs));
            let resolved_len = len_expr
                .as_ref()
                .and_then(|e| eval_const_usize_expr(e, &const_subs))
                .unwrap_or(*len);
            IrType::Array {
                elem: Box::new(resolve_ast_type_with_subs(elem, subs, module)?),
                len: resolved_len,
            }
        }
        AstType::Option(inner, _) => {
            IrType::Option(Box::new(resolve_ast_type_with_subs(inner, subs, module)?))
        }
        AstType::Result(ok_ty, err_ty, _) => IrType::ResultType(
            Box::new(resolve_ast_type_with_subs(ok_ty, subs, module)?),
            Box::new(resolve_ast_type_with_subs(err_ty, subs, module)?),
        ),
        AstType::Chan(elem, _) => {
            IrType::Chan(Box::new(resolve_ast_type_with_subs(elem, subs, module)?))
        }
        AstType::Atomic(inner, _) => {
            IrType::Atomic(Box::new(resolve_ast_type_with_subs(inner, subs, module)?))
        }
        AstType::Mutex(inner, _) => {
            IrType::Mutex(Box::new(resolve_ast_type_with_subs(inner, subs, module)?))
        }
        AstType::List(elem, _) => {
            IrType::List(Box::new(resolve_ast_type_with_subs(elem, subs, module)?))
        }
        AstType::Map(k, v, _) => IrType::Map(
            Box::new(resolve_ast_type_with_subs(k, subs, module)?),
            Box::new(resolve_ast_type_with_subs(v, subs, module)?),
        ),
        AstType::WeakRef(inner, _) => {
            IrType::WeakRef(Box::new(resolve_ast_type_with_subs(inner, subs, module)?))
        }
        AstType::Fn { params, ret, .. } => IrType::Fn {
            params: params
                .iter()
                .map(|p| resolve_ast_type_with_subs(p, subs, module))
                .collect::<Result<_, _>>()?,
            ret: Box::new(resolve_ast_type_with_subs(ret, subs, module)?),
        },
        AstType::Grad(inner, _) => {
            IrType::Grad(Box::new(resolve_ast_type_with_subs(inner, subs, module)?))
        }
        AstType::Sparse(inner, _) => {
            IrType::Sparse(Box::new(resolve_ast_type_with_subs(inner, subs, module)?))
        }
        // `task<T>`: the handle an `async def` returns.
        AstType::Generic { name, args, .. }
            if name == "task"
                && args.len() == 1
                && !subs.contains_key(name)
                && module.struct_def("task").is_none() =>
        {
            IrType::Task(Box::new(resolve_ast_type_with_subs(
                &args[0], subs, module,
            )?))
        }
        AstType::Generic { name, args, .. } => {
            let resolved_args: Vec<IrType> = args
                .iter()
                .map(|arg| resolve_ast_type_with_subs(arg, subs, module))
                .collect::<Result<_, _>>()?;

            if let Some(IrType::TypeParam(ctor)) = subs.get(name) {
                return Ok(IrType::HktApp {
                    ctor: ctor.clone(),
                    args: resolved_args,
                });
            }

            // `F` bound to a builtin constructor rebuilds the builtin type
            // rather than looking for a record called `list__i64`. Without this
            // the substitution silently produced a struct name that is defined
            // nowhere, which is the same failure mode as #14.
            if let Some(IrType::Struct { name: ctor, fields }) = subs.get(name) {
                if fields.is_empty() {
                    match ctor.as_str() {
                        "list" => {
                            return Ok(IrType::List(Box::new(
                                resolved_args
                                    .first()
                                    .cloned()
                                    .ok_or_else(|| type_not_lowerable(ty))?,
                            )));
                        }
                        "option" => {
                            return Ok(IrType::Option(Box::new(
                                resolved_args
                                    .first()
                                    .cloned()
                                    .ok_or_else(|| type_not_lowerable(ty))?,
                            )));
                        }
                        "map" => {
                            return Ok(IrType::Map(
                                Box::new(
                                    resolved_args
                                        .first()
                                        .cloned()
                                        .ok_or_else(|| type_not_lowerable(ty))?,
                                ),
                                Box::new(
                                    resolved_args
                                        .get(1)
                                        .cloned()
                                        .ok_or_else(|| type_not_lowerable(ty))?,
                                ),
                            ));
                        }
                        _ => {}
                    }
                }
            }

            let base_name = if let Some(concrete_ty) = subs.get(name) {
                match concrete_ty {
                    IrType::Struct { name: s_name, .. } => s_name.clone(),
                    _ => name.clone(),
                }
            } else {
                name.clone()
            };
            let mangled_name = format!(
                "{}__{}",
                base_name,
                resolved_args
                    .iter()
                    .map(mangle_ir_type)
                    .collect::<Vec<_>>()
                    .join("_")
            );
            let resolved_name = resolve_brought_name(&mangled_name, module);
            if let Some(fields) = module.struct_def(&resolved_name) {
                let resolved_fields: Vec<(String, IrType)> = fields
                    .iter()
                    .map(|(n, t)| (n.clone(), resolve_concrete_field(t, subs)))
                    .collect();
                IrType::Struct {
                    name: resolved_name,
                    fields: resolved_fields,
                }
            } else if let Some(fields) =
                instantiate_struct_fields_from_ast(name, &resolved_args, module)
            {
                IrType::Struct {
                    name: resolved_name,
                    fields,
                }
            } else {
                // Fallback: use the generic struct template's registered field layout.
                let resolved_base = resolve_brought_name(name, module);
                if let Some(template_fields) = module.struct_def(&resolved_base) {
                    let resolved_fields: Vec<(String, IrType)> = template_fields
                        .iter()
                        .map(|(n, t)| (n.clone(), resolve_concrete_field(t, subs)))
                        .collect();
                    IrType::Struct {
                        name: resolved_name,
                        fields: resolved_fields,
                    }
                } else {
                    IrType::Struct {
                        name: resolved_name,
                        fields: Vec::new(),
                    }
                }
            }
        }
        AstType::Ref(inner, _) => match inner.as_ref() {
            AstType::Slice(elem, _) => IrType::Slice {
                elem: Box::new(resolve_ast_type_with_subs(elem, subs, module)?),
                is_mut: false,
            },
            _ => resolve_ast_type_with_subs(inner, subs, module)?,
        },
        AstType::RefMut(inner, _) => match inner.as_ref() {
            AstType::Slice(elem, _) => IrType::Slice {
                elem: Box::new(resolve_ast_type_with_subs(elem, subs, module)?),
                is_mut: true,
            },
            _ => IrType::MutRef(Box::new(resolve_ast_type_with_subs(inner, subs, module)?)),
        },
        AstType::Slice(elem, _) => IrType::Slice {
            elem: Box::new(resolve_ast_type_with_subs(elem, subs, module)?),
            is_mut: false,
        },
        AstType::ConstInt(n, _) => IrType::ConstInt(*n),
        AstType::AssocType { .. } => return Err(type_not_lowerable(ty)),
        AstType::DynTrait { trait_name, .. } => {
            let methods = module.trait_def(trait_name).cloned().unwrap_or_default();
            IrType::TraitObject {
                name: trait_name.clone(),
                methods,
            }
        }
        AstType::MaskEffectType { .. } => return Err(type_not_lowerable(ty)),
        other => lower_type(other)?,
    })
}

pub(crate) fn resolve_concrete_field(ft: &IrType, subs: &HashMap<String, IrType>) -> IrType {
    match ft {
        IrType::Array { elem, len } => IrType::Array {
            elem: Box::new(resolve_concrete_field(elem, subs)),
            len: *len,
        },
        IrType::Slice { elem, is_mut } => IrType::Slice {
            elem: Box::new(resolve_concrete_field(elem, subs)),
            is_mut: *is_mut,
        },
        IrType::List(inner) => IrType::List(Box::new(resolve_concrete_field(inner, subs))),
        IrType::Map(k, v) => IrType::Map(
            Box::new(resolve_concrete_field(k, subs)),
            Box::new(resolve_concrete_field(v, subs)),
        ),
        IrType::Option(inner) => IrType::Option(Box::new(resolve_concrete_field(inner, subs))),
        IrType::ResultType(ok, err) => IrType::ResultType(
            Box::new(resolve_concrete_field(ok, subs)),
            Box::new(resolve_concrete_field(err, subs)),
        ),
        IrType::Chan(inner) => IrType::Chan(Box::new(resolve_concrete_field(inner, subs))),
        IrType::Atomic(inner) => IrType::Atomic(Box::new(resolve_concrete_field(inner, subs))),
        IrType::Mutex(inner) => IrType::Mutex(Box::new(resolve_concrete_field(inner, subs))),
        IrType::Grad(inner) => IrType::Grad(Box::new(resolve_concrete_field(inner, subs))),
        IrType::Sparse(inner) => IrType::Sparse(Box::new(resolve_concrete_field(inner, subs))),
        IrType::WeakRef(inner) => IrType::WeakRef(Box::new(resolve_concrete_field(inner, subs))),
        IrType::MutRef(inner) => IrType::MutRef(Box::new(resolve_concrete_field(inner, subs))),
        IrType::Fn { params, ret } => IrType::Fn {
            params: params
                .iter()
                .map(|p| resolve_concrete_field(p, subs))
                .collect(),
            ret: Box::new(resolve_concrete_field(ret, subs)),
        },
        IrType::Tuple(elems) => IrType::Tuple(
            elems
                .iter()
                .map(|e| resolve_concrete_field(e, subs))
                .collect(),
        ),
        IrType::Struct { name, fields } => {
            if let Some(concrete) = subs.get(name) {
                return concrete.clone();
            }
            let new_fields: Vec<(String, IrType)> = fields
                .iter()
                .map(|(n, t)| (n.clone(), resolve_concrete_field(t, subs)))
                .collect();
            IrType::Struct {
                name: name.clone(),
                fields: new_fields,
            }
        }
        other => other.clone(),
    }
}

pub fn populate_struct_fields(ty: &mut IrType, module: &IrModule) {
    match ty {
        IrType::Struct { name, fields } => {
            if fields.is_empty() {
                if let Some(def_fields) = module.struct_def(name) {
                    *fields = def_fields.clone();
                }
            }
            for (_, f_ty) in fields {
                populate_struct_fields(f_ty, module);
            }
        }
        IrType::Tuple(elems) => {
            for e in elems {
                populate_struct_fields(e, module);
            }
        }
        IrType::Array { elem, .. } | IrType::Slice { elem, .. } => {
            populate_struct_fields(elem, module)
        }
        IrType::Option(inner) => populate_struct_fields(inner, module),
        IrType::ResultType(ok, err) => {
            populate_struct_fields(ok, module);
            populate_struct_fields(err, module);
        }
        IrType::Chan(inner) => populate_struct_fields(inner, module),
        IrType::Atomic(inner) => populate_struct_fields(inner, module),
        IrType::Mutex(inner) => populate_struct_fields(inner, module),
        IrType::Grad(inner) => populate_struct_fields(inner, module),
        IrType::Sparse(inner) => populate_struct_fields(inner, module),
        IrType::List(inner) => populate_struct_fields(inner, module),
        IrType::Map(k, v) => {
            populate_struct_fields(k, module);
            populate_struct_fields(v, module);
        }
        IrType::WeakRef(inner) => populate_struct_fields(inner, module),
        IrType::MutRef(inner) => populate_struct_fields(inner, module),
        IrType::Fn { params, ret } => {
            for p in params {
                populate_struct_fields(p, module);
            }
            populate_struct_fields(ret, module);
        }
        _ => {}
    }
}
