//! Type checking on the HIR, before lowering.
//!
//! rustc types the program on a tree shaped like the source and lets the types
//! flow down into its control-flow graph; IRIS has lowered first and re-derived
//! types from the CFG afterwards, which is how a binding's annotation came to
//! be ignored (`val z: i32 = a` kept `a`'s `i64`) and an integer argument
//! reached an `f64` parameter unconverted. See docs/architecture-vs-rustc.md.
//!
//! HIR construction (`src/hir`) runs first: it resolves every value name and
//! registers every expression. This pass types that HIR and its report is
//! absorbed into it; lowering reads the result from the HIR, not from here.
//! It is conservative: what it does not model it types `Unknown`, which agrees
//! with everything, so it can miss an error but should never invent one. A
//! variable such a type touches is *opaque*: left open, its type is lowering's
//! to decide.
//!
//! It also owns the name and shape errors, judged on this tree before any IR
//! exists (§4.2): an unknown name (E0100), a call whose count of arguments
//! does not fit the callee (E0109) or which calls what is not a function
//! (E0118), a record built with a field it does not have (E0113) or without
//! one it does (E0114), a choice used with a variant it does not declare
//! (E0122), and a `when` that handles none of a choice's variants (E0210).
//! The IR `ValidatePass` and `ExhaustivePass` remain as backstops.
//!
//! Its errors are the program's type errors: reported whether or not
//! lowering accepts the program, in place of lowering's own error in the same
//! function (`lower`), with the fixes lowering's messages carry. A value
//! that is used but whose type nothing determines is E0207 here.
//!
//! Lowering consumes its types (`hint`): the parts of a type lowering cannot
//! know itself -- an empty collection's elements, the other side of `ok` and
//! `err`, a literal's width, a channel's element, a generic body's
//! parameters. A part nothing constrains at all takes `i64`, which no read
//! can observe (a read of a wholly unknown value is E0207). Lowering then
//! leaves nothing untyped (`check_typed`).
//!
//! `IRIS_TYPECK=off` turns the checker off; `report` prints what it finds,
//! `compare` checks its types against the ones lowering and inference produce
//! (see `lib.rs`), and `infer` lists every value lowering leaves untyped.

use std::collections::{HashMap, HashSet};

use crate::diagnostics::{Diagnostic, Edit, Fix};
use crate::ir::module::IrModule;
use crate::ir::types::{DType, IrType};
use crate::parser::ast::{
    AstBinOp, AstBlock, AstExpr, AstFunction, AstStmt, AstType, AstUnaryOp, AstWhenArm,
    AstWhenPattern, Ident,
};
use crate::parser::lexer::Span;

/// Names of built-ins lowering accepts in its call dispatch that
/// `lsp::builtin_function_names` and `runtime_builtin` do not list: the
/// operator-like and ML-intrinsic rewrites (`band`, `round`, `list_mean`,
/// ...), the option/call-shaping helpers (`is_err`, `eprintln`, `slice`,
/// ...), and the runtime entry points missing from the completion table
/// (`recv_timeout`, `par_map`, ...). The checker's unknown-name judgment
/// must not reject a call of one, so the list enumerates every name
/// `lower_call` dispatches on; not modelling them, their types are left to
/// lowering.
pub(crate) const LOWER_BUILTIN_FNS: &[&str] = &[
    "arange",
    "args",
    "array_to_list",
    "atomic_new",
    "backward",
    "band",
    "barrier",
    "bitnot",
    "bor",
    "bxor",
    "chan_send",
    "cross_entropy",
    "densify",
    "einsum",
    "eprintln",
    "exit",
    "find",
    "is_err",
    "is_none",
    "list_add",
    "list_axpy",
    "list_axpy_blas",
    "list_concat",
    "list_dot",
    "list_dot_blas",
    "list_insert",
    "list_max_val",
    "list_mean",
    "list_min_val",
    "list_mul_elem",
    "list_norm",
    "list_push",
    "list_relu",
    "list_remove",
    "list_scale",
    "list_scale_blas",
    "list_sigmoid",
    "list_softmax",
    "list_sort",
    "list_std",
    "list_sub",
    "log2",
    "matmul",
    "mse_loss",
    "mutex_lock",
    "mutex_new",
    "mutex_unlock",
    "nnz",
    "par_map",
    "parse_f64",
    "parse_i64",
    "println",
    "recv_timeout",
    "round",
    "sgd_step",
    "shl",
    "shr",
    "sign",
    "slice",
    "sparsify",
    "str_index",
    "str_replace",
    "tape",
    "task_group",
    "task_group_cancel",
    "task_group_join",
    "to_f64",
    "to_lower",
    "to_upper",
    "trim",
    "unwrap_err",
    "weak_ref",
    "weak_upgrade",
    "wrapping_add",
    "wrapping_mul",
    "wrapping_sub",
];

/// What lowering has registered by the time function bodies are lowered.
pub(crate) struct Registries<'a> {
    pub module: &'a IrModule,
    pub fn_sigs: &'a HashMap<String, IrType>,
    pub fn_param_types: &'a HashMap<String, Vec<IrType>>,
    /// Declared parameter names, in order, for the counts-of-arguments
    /// judgment (E0109). Functions only: impl methods have their own maps.
    pub fn_param_names: &'a HashMap<String, Vec<String>>,
    /// Each function's parameter defaults, for the same judgment: which
    /// arguments a call may leave out.
    pub fn_defaults: &'a HashMap<String, Vec<Option<AstExpr>>>,
    pub generic_fns: &'a HashMap<String, AstFunction>,
    pub const_defs: &'a HashMap<String, AstExpr>,
    pub trait_dispatch: &'a HashMap<String, Vec<(IrType, String)>>,
    /// Functions declared `-> Infer` (the REPL's): known by name though they
    /// have no signature until their bodies have been checked.
    pub inferred_fns: &'a HashSet<String>,
    /// Inherent methods' parameter types / declared names / defaults, keyed
    /// by the mangled name the registries give them (`Type__method`),
    /// `self` included. Separate from the function maps so the checker
    /// extends what it judges without changing how the lowerer types calls.
    pub method_param_types: &'a HashMap<String, Vec<IrType>>,
    pub method_param_names: &'a HashMap<String, Vec<String>>,
    pub method_param_defaults: &'a HashMap<String, Vec<Option<AstExpr>>>,
}

/// The checker's findings.
#[derive(Debug, Default, Clone)]
pub struct TypeckReport {
    /// The type of each expression whose type was fully determined, by span.
    pub types: HashMap<(u32, u32), IrType>,
    /// Definite type errors.
    pub errors: Vec<Diagnostic>,
    /// An integer literal where a float is declared (`val x: f64 = 1`):
    /// lowering accepts these without converting.
    pub literal_coercions: Vec<Diagnostic>,
    /// Expressions seen, and how many got a fully determined type.
    pub expressions: usize,
    pub typed: usize,
    /// Where expressions whose type was not fully determined start.
    pub untyped_starts: HashSet<u32>,
    /// How many errors were taken to be reported.
    pub reported: usize,
    /// What the checker solved of each expression's type, by span: what
    /// lowering cannot type alone -- literals, `list()`, `map()`, `none`,
    /// `ok`, `err`, channels, generic bodies -- and anything else it asks.
    pub(crate) hints: HashMap<(u32, u32), Hint>,
    /// The return types of functions declared `-> Infer`, as their bodies
    /// solve them.
    pub returns: HashMap<String, IrType>,
}

/// A type with holes.
#[derive(Clone, Debug, PartialEq)]
enum Ty {
    Var(u32),
    /// Not modelled: agrees with everything.
    Unknown,
    Scalar(DType),
    Str,
    /// A record or choice, by its resolved name.
    Named(String),
    Dyn(String),
    /// A type parameter of the generic function being checked.
    Param(String),
    List(Box<Ty>),
    Map(Box<Ty>, Box<Ty>),
    Option(Box<Ty>),
    Result(Box<Ty>, Box<Ty>),
    Tuple(Vec<Ty>),
    Array(Box<Ty>, usize),
    Chan(Box<Ty>),
    Task(Box<Ty>),
    Fn(Vec<Ty>, Box<Ty>),
    /// Anything else (tensors, atomics, ...): compared loosely.
    Other(IrType),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Any,
    /// An integer literal: any integer type, `i64` unless constrained.
    Int,
    /// A float literal: `f32` or `f64`, `f64` unless constrained.
    Float,
}

#[derive(Clone, Debug)]
struct Node {
    parent: u32,
    value: Option<Ty>,
    kind: Kind,
    /// Constrained by something the checker does not model (a type it
    /// leaves `Unknown`, an unmodelled call): left unsolved, its type is
    /// lowering's to decide, not a default and not an error.
    opaque: bool,
}

/// Why two types were unified: decides the code and wording of a mismatch.
#[derive(Clone, Copy, Debug)]
enum Why {
    Annotation,
    Argument,
    Return,
    Branch,
    Operand,
    Condition,
    Assignment,
    Element,
    Field,
}

struct Checker<'a> {
    reg: &'a Registries<'a>,
    nodes: Vec<Node>,
    scopes: Vec<HashMap<String, Ty>>,
    ret: Option<Ty>,
    /// Where the current function's return type is written.
    ret_span: Option<Span>,
    fn_name: String,
    type_params: HashSet<String>,
    /// Every expression and its type, solved when the program is done:
    /// what lowering asks for (`TypeckReport::hints`).
    recorded: Vec<((u32, u32), Ty)>,
    errors: Vec<Diagnostic>,
    coercions: Vec<Diagnostic>,
    const_types: HashMap<String, Ty>,
    /// Spans of expressions whose values are used (see `using`).
    used: HashSet<(u32, u32)>,
    /// Used expressions and their types, checked when the function is done.
    reads: Vec<(Span, Ty)>,
    /// The error type each enclosing `try`'s catch receives, innermost last.
    try_errs: Vec<Ty>,
    /// Functions declared `-> Infer` (the REPL's): their bodies' types.
    solved_returns: Vec<(String, Ty)>,
}

/// Check every function of `hir`. The checker runs on the HIR (stage 5):
/// HIR construction has already resolved names.
pub(crate) fn check(hir: &crate::hir::HirModule<'_>, reg: &Registries) -> TypeckReport {
    let ast = hir.source();
    let mut c = Checker {
        reg,
        nodes: Vec::new(),
        scopes: Vec::new(),
        ret: None,
        ret_span: None,
        fn_name: String::new(),
        type_params: HashSet::new(),
        recorded: Vec::new(),
        errors: Vec::new(),
        coercions: Vec::new(),
        const_types: HashMap::new(),
        used: HashSet::new(),
        reads: Vec::new(),
        try_errs: Vec::new(),
        solved_returns: Vec::new(),
    };
    // Constants first, in an empty scope: functions read them.
    let mut names: Vec<&String> = reg.const_defs.keys().collect();
    names.sort();
    for name in names {
        c.scopes.push(HashMap::new());
        let t = c.infer(&reg.const_defs[name]);
        c.scopes.pop();
        c.const_types.insert(name.clone(), t);
    }
    for f in &ast.functions {
        c.check_function(f);
    }
    for imp in &ast.impls {
        for m in &imp.methods {
            c.check_function(m);
        }
    }
    let mut report = TypeckReport {
        errors: std::mem::take(&mut c.errors),
        literal_coercions: std::mem::take(&mut c.coercions),
        ..Default::default()
    };
    // Every expression's solved type, for the HIR (and through it,
    // lowering). A span the parser synthesised (zero-width) gets none, and
    // neither does one two expressions share that solved to different types.
    let recorded = std::mem::take(&mut c.recorded);
    report.expressions = recorded.len();
    let mut ambiguous: HashSet<(u32, u32)> = HashSet::new();
    for (span, t) in recorded {
        let h = c.hint_of(&t);
        match h.known() {
            Some(ir) => {
                report.typed += 1;
                report.types.insert(span, ir);
            }
            None => {
                report.untyped_starts.insert(span.0);
            }
        }
        if span.1 <= span.0 || h == Hint::Open {
            continue;
        }
        match report.hints.get(&span) {
            Some(prev) if *prev != h => {
                ambiguous.insert(span);
            }
            _ => {
                report.hints.insert(span, h);
            }
        }
    }
    for span in ambiguous {
        report.hints.remove(&span);
    }
    for (name, t) in std::mem::take(&mut c.solved_returns) {
        if let Some(ir) = c.hint_of(&t).known() {
            report.returns.insert(name, ir);
        }
    }
    report
}

/// Whether `name` is a value the language knows without the program defining
/// it. Shared with HIR resolution, so the two cannot drift.
pub(crate) fn is_builtin_value_name(name: &str) -> bool {
    crate::lsp::builtin_function_names().contains(&name)
        || LOWER_BUILTIN_FNS.contains(&name)
        || super::runtime_builtin(name).is_some()
        || matches!(name, "true" | "false" | "none")
}

/// Whether a call of `name` reaches lowering's built-in dispatch (its
/// special cases before the general function path), which the checker does
/// not model. Shared with HIR resolution.
pub(crate) fn is_builtin_call_name(name: &str) -> bool {
    crate::lsp::builtin_function_names().contains(&name)
        || LOWER_BUILTIN_FNS.contains(&name)
        || super::runtime_builtin(name).is_some()
}

/// The name lowering resolves an unqualified function, constant or type
/// name to (see `Lowerer::resolve_unqualified_name`). HIR records this
/// resolution before the checker runs.
pub(crate) fn resolve_known_name(reg: &Registries<'_>, name: &str) -> String {
    let known = |n: &str| {
        reg.fn_sigs.contains_key(n)
            || reg.generic_fns.contains_key(n)
            || reg.const_defs.contains_key(n)
            || reg.module.struct_def(n).is_some()
            || reg.module.enum_def(n).is_some()
            || reg.module.type_alias(n).is_some()
    };
    let private = |n: &str| super::CURRENT_PRIVATE_ITEMS.with(|p| p.borrow().contains(n));
    let by_prefix = super::CURRENT_BRING_PREFIXES.with(|stack| {
        stack.borrow().last().and_then(|prefixes| {
            prefixes
                .iter()
                .map(|p| format!("{p}__{name}"))
                .find(|c| !private(c) && known(c))
        })
    });
    if let Some(n) = by_prefix {
        return n;
    }
    if known(name) {
        return name.to_string();
    }
    let suffix = format!("__{name}");
    let mut candidates: Vec<String> = reg
        .fn_sigs
        .keys()
        .chain(reg.generic_fns.keys())
        .chain(reg.const_defs.keys())
        .chain(reg.module.struct_defs.keys())
        .chain(reg.module.enum_defs.keys())
        .chain(reg.module.type_aliases.keys())
        .filter(|k| k.ends_with(&suffix) && !private(k))
        .cloned()
        .collect();
    candidates.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    candidates.dedup();
    candidates
        .into_iter()
        .next()
        .unwrap_or_else(|| name.to_string())
}

impl<'a> Checker<'a> {
    // ── Variables ──────────────────────────────────────────────────────────

    fn fresh(&mut self, kind: Kind) -> Ty {
        let id = self.nodes.len() as u32;
        self.nodes.push(Node {
            parent: id,
            value: None,
            kind,
            opaque: false,
        });
        Ty::Var(id)
    }

    /// A variable for a type lowering has not decided either.
    fn fresh_opaque(&mut self) -> Ty {
        let t = self.fresh(Kind::Any);
        self.taint(&t);
        t
    }

    /// Mark every unsolved variable in `t` opaque: something the checker
    /// does not model constrains it.
    fn taint(&mut self, t: &Ty) {
        match self.shallow(t) {
            Ty::Var(r) => self.nodes[r as usize].opaque = true,
            Ty::List(e) | Ty::Option(e) | Ty::Array(e, _) | Ty::Chan(e) | Ty::Task(e) => {
                self.taint(&e)
            }
            Ty::Map(a, b) | Ty::Result(a, b) => {
                self.taint(&a);
                self.taint(&b);
            }
            Ty::Tuple(ts) => {
                for t in &ts {
                    self.taint(t);
                }
            }
            Ty::Fn(ps, r) => {
                for p in &ps {
                    self.taint(p);
                }
                self.taint(&r);
            }
            _ => {}
        }
    }

    fn find(&mut self, v: u32) -> u32 {
        let p = self.nodes[v as usize].parent;
        if p == v {
            return v;
        }
        let root = self.find(p);
        self.nodes[v as usize].parent = root;
        root
    }

    /// `t` with its outermost variable replaced by what it is bound to.
    fn shallow(&mut self, t: &Ty) -> Ty {
        match t {
            Ty::Var(v) => {
                let r = self.find(*v);
                match self.nodes[r as usize].value.clone() {
                    Some(bound) => self.shallow(&bound),
                    None => Ty::Var(r),
                }
            }
            other => other.clone(),
        }
    }

    /// `t` with every bound variable replaced; unbound literal variables
    /// take their default type.
    fn resolve(&mut self, t: &Ty, default_literals: bool) -> Ty {
        match self.shallow(t) {
            Ty::Var(r) => match (default_literals, self.nodes[r as usize].kind) {
                (true, Kind::Int) => Ty::Scalar(DType::I64),
                (true, Kind::Float) => Ty::Scalar(DType::F64),
                _ => Ty::Var(r),
            },
            Ty::List(e) => Ty::List(Box::new(self.resolve(&e, default_literals))),
            Ty::Map(k, v) => Ty::Map(
                Box::new(self.resolve(&k, default_literals)),
                Box::new(self.resolve(&v, default_literals)),
            ),
            Ty::Option(e) => Ty::Option(Box::new(self.resolve(&e, default_literals))),
            Ty::Result(a, b) => Ty::Result(
                Box::new(self.resolve(&a, default_literals)),
                Box::new(self.resolve(&b, default_literals)),
            ),
            Ty::Tuple(ts) => Ty::Tuple(
                ts.iter()
                    .map(|t| self.resolve(t, default_literals))
                    .collect(),
            ),
            Ty::Array(e, n) => Ty::Array(Box::new(self.resolve(&e, default_literals)), n),
            Ty::Chan(e) => Ty::Chan(Box::new(self.resolve(&e, default_literals))),
            Ty::Task(e) => Ty::Task(Box::new(self.resolve(&e, default_literals))),
            Ty::Fn(ps, r) => Ty::Fn(
                ps.iter()
                    .map(|t| self.resolve(t, default_literals))
                    .collect(),
                Box::new(self.resolve(&r, default_literals)),
            ),
            other => other,
        }
    }

    fn occurs(&mut self, v: u32, t: &Ty) -> bool {
        match self.shallow(t) {
            Ty::Var(r) => r == v,
            Ty::List(e) | Ty::Option(e) | Ty::Array(e, _) | Ty::Chan(e) | Ty::Task(e) => {
                self.occurs(v, &e)
            }
            Ty::Map(a, b) | Ty::Result(a, b) => self.occurs(v, &a) || self.occurs(v, &b),
            Ty::Tuple(ts) => ts.iter().any(|t| self.occurs(v, t)),
            Ty::Fn(ps, r) => ps.iter().any(|t| self.occurs(v, t)) || self.occurs(v, &r),
            _ => false,
        }
    }

    // ── Unification ────────────────────────────────────────────────────────

    /// Make `found` and `expected` the same type. A definite mismatch is an
    /// error at `span`; anything involving `Unknown`, a type parameter, or a
    /// type the checker does not model agrees.
    fn unify(&mut self, found: &Ty, expected: &Ty, span: Span, why: Why) {
        if let Err((f, e)) = self.unify_inner(found, expected) {
            self.mismatch(&f, &e, span, why);
        }
    }

    /// `unify` for a value `e` whose type is decided elsewhere: `because`
    /// points at where (an annotation, a return type, a parameter). An
    /// integer literal where a float is wanted gets the fix `1` -> `1.0`.
    fn expect(
        &mut self,
        e: &AstExpr,
        found: &Ty,
        expected: &Ty,
        why: Why,
        because: Option<(Span, String)>,
    ) {
        let before = self.errors.len();
        self.unify(found, expected, e.span(), why);
        if self.errors.len() == before {
            return;
        }
        let mut d = self.errors.pop().expect("an error was just pushed");
        if let Some((at, label)) = because {
            if at.end.0 > at.start.0 {
                d = d.with_secondary_span(at.start.0, at.end.0, Some(label));
            }
        }
        let literal = match e {
            AstExpr::IntLit { value, span } => Some((*value, *span, "")),
            AstExpr::UnaryOp {
                op: AstUnaryOp::Neg,
                expr,
                ..
            } => match expr.as_ref() {
                AstExpr::IntLit { value, span } => Some((*value, *span, "-")),
                _ => None,
            },
            _ => None,
        };
        let expected_r = self.resolve(expected, false);
        let found_r = self.resolve(found, true);
        if let (Some((value, at, sign)), Ty::Scalar(DType::F32 | DType::F64)) =
            (literal, &expected_r)
        {
            d = d.with_fix(crate::diagnostics::Fix::replace(
                format!("write it as a float: `{sign}{value}.0`"),
                at.start.0,
                at.end.0,
                format!("{value}.0"),
            ));
        } else if let (Ty::Scalar(f), Ty::Scalar(x)) = (&found_r, &expected_r) {
            // Between numeric types IRIS converts only when told: `x to T`.
            if f != x && *f != DType::Bool && *x != DType::Bool {
                let target = x.to_string();
                let sp = e.span();
                let simple = matches!(
                    e,
                    AstExpr::Ident(_)
                        | AstExpr::Call { .. }
                        | AstExpr::FieldAccess { .. }
                        | AstExpr::MethodCall { .. }
                );
                let (before, after) = if simple {
                    (String::new(), format!(" to {target}"))
                } else {
                    ("(".to_string(), format!(") to {target}"))
                };
                d = d.with_fix(crate::diagnostics::Fix {
                    message: format!("convert it with `to {target}`"),
                    edits: vec![
                        crate::diagnostics::Edit {
                            start: sp.start.0,
                            end: sp.start.0,
                            replacement: before,
                        },
                        crate::diagnostics::Edit {
                            start: sp.end.0,
                            end: sp.end.0,
                            replacement: after,
                        },
                    ],
                });
            }
        }
        self.errors.push(d);
    }

    fn unify_inner(&mut self, a: &Ty, b: &Ty) -> Result<(), (Ty, Ty)> {
        let a = self.shallow(a);
        let b = self.shallow(b);
        match (&a, &b) {
            // Within a generic body a type parameter is one definite type:
            // a variable meeting it is that type (lowering substitutes it
            // per instance).
            (Ty::Var(x), Ty::Param(_)) | (Ty::Param(_), Ty::Var(x))
                if self.nodes[*x as usize].kind == Kind::Any =>
            {
                let p = if matches!(a, Ty::Param(_)) {
                    a.clone()
                } else {
                    b.clone()
                };
                self.nodes[*x as usize].value = Some(p);
                Ok(())
            }
            (Ty::Unknown | Ty::Param(_) | Ty::Other(_), other)
            | (other, Ty::Unknown | Ty::Param(_) | Ty::Other(_)) => {
                let other = other.clone();
                self.taint(&other);
                Ok(())
            }
            (Ty::Var(x), Ty::Var(y)) => {
                if x == y {
                    return Ok(());
                }
                let (kx, ky) = (self.nodes[*x as usize].kind, self.nodes[*y as usize].kind);
                let kind = match (kx, ky) {
                    (Kind::Any, k) | (k, Kind::Any) => k,
                    (Kind::Int, Kind::Int) => Kind::Int,
                    (Kind::Float, Kind::Float) => Kind::Float,
                    _ => return Err((a.clone(), b.clone())),
                };
                self.nodes[*x as usize].parent = *y;
                self.nodes[*y as usize].kind = kind;
                if self.nodes[*x as usize].opaque {
                    self.nodes[*y as usize].opaque = true;
                }
                Ok(())
            }
            (Ty::Var(x), t) | (t, Ty::Var(x)) => {
                let kind = self.nodes[*x as usize].kind;
                let fits = match (kind, t) {
                    (Kind::Any, _) => true,
                    (Kind::Int, Ty::Scalar(d)) => is_int(*d),
                    (Kind::Float, Ty::Scalar(d)) => matches!(d, DType::F32 | DType::F64),
                    _ => false,
                };
                if !fits {
                    return Err((a.clone(), b.clone()));
                }
                if self.occurs(*x, t) {
                    return Ok(());
                }
                self.nodes[*x as usize].value = Some(t.clone());
                if self.nodes[*x as usize].opaque {
                    let t = t.clone();
                    self.taint(&t);
                }
                Ok(())
            }
            (Ty::Scalar(x), Ty::Scalar(y)) if x == y => Ok(()),
            (Ty::Str, Ty::Str) => Ok(()),
            (Ty::Named(x), Ty::Named(y)) => {
                if x == y || loose_named(x, y) {
                    Ok(())
                } else {
                    Err((a.clone(), b.clone()))
                }
            }
            // A record passed where a trait object is expected is coerced.
            (Ty::Dyn(_), Ty::Named(_)) | (Ty::Named(_), Ty::Dyn(_)) => Ok(()),
            (Ty::Dyn(x), Ty::Dyn(y)) => {
                if x == y {
                    Ok(())
                } else {
                    Err((a.clone(), b.clone()))
                }
            }
            (Ty::List(x), Ty::List(y))
            | (Ty::Option(x), Ty::Option(y))
            | (Ty::Chan(x), Ty::Chan(y))
            | (Ty::Task(x), Ty::Task(y)) => {
                self.unify_inner(x, y).map_err(|_| (a.clone(), b.clone()))
            }
            // `val xs: list<i64> = [1, 2]`: an array literal becomes a list.
            (Ty::Array(x, _), Ty::List(y)) | (Ty::List(y), Ty::Array(x, _)) => {
                self.unify_inner(x, y).map_err(|_| (a.clone(), b.clone()))
            }
            (Ty::Array(x, n), Ty::Array(y, m)) => {
                if n != m {
                    return Err((a.clone(), b.clone()));
                }
                self.unify_inner(x, y).map_err(|_| (a.clone(), b.clone()))
            }
            (Ty::Map(k1, v1), Ty::Map(k2, v2)) | (Ty::Result(k1, v1), Ty::Result(k2, v2)) => {
                self.unify_inner(k1, k2)
                    .map_err(|_| (a.clone(), b.clone()))?;
                self.unify_inner(v1, v2).map_err(|_| (a.clone(), b.clone()))
            }
            (Ty::Tuple(xs), Ty::Tuple(ys)) => {
                if xs.len() != ys.len() {
                    return Err((a.clone(), b.clone()));
                }
                for (x, y) in xs.iter().zip(ys.iter()) {
                    self.unify_inner(x, y).map_err(|_| (a.clone(), b.clone()))?;
                }
                Ok(())
            }
            (Ty::Fn(p1, r1), Ty::Fn(p2, r2)) => {
                // Signatures recorded without parameters (`fn_sigs`) agree
                // with any parameter list.
                if p1.len() == p2.len() {
                    for (x, y) in p1.iter().zip(p2.iter()) {
                        self.unify_inner(x, y).map_err(|_| (a.clone(), b.clone()))?;
                    }
                }
                self.unify_inner(r1, r2).map_err(|_| (a.clone(), b.clone()))
            }
            _ => Err((a.clone(), b.clone())),
        }
    }

    fn mismatch(&mut self, found: &Ty, expected: &Ty, span: Span, why: Why) {
        let f = self.resolve(found, false);
        let e = self.resolve(expected, false);
        // An integer literal where a float is declared: lowering accepts it
        // (without converting); keep it apart from real errors.
        let literal = |t: &Ty, nodes: &[Node]| matches!(t, Ty::Var(v) if nodes[*v as usize].kind == Kind::Int);
        let (fs, es) = (self.name(&f), self.name(&e));
        let float_expected = matches!(e, Ty::Scalar(DType::F32 | DType::F64));
        if literal(&f, &self.nodes) && float_expected {
            self.errors.push(
                Diagnostic::error(format!("expected `{es}`, found an integer"))
                    .with_code("E0202")
                    .with_primary_span(span.start.0, span.end.0, None)
                    .with_note("IRIS does not convert between types implicitly"),
            );
            return;
        }
        let (code, message) = match why {
            Why::Return => (
                "E0209",
                format!(
                    "`{}` returns `{es}`, but this value is `{fs}`",
                    self.fn_name
                ),
            ),
            Why::Branch => (
                "E0208",
                format!("this is `{fs}`, but the other branch is `{es}`"),
            ),
            Why::Argument => (
                "E0110",
                format!("this argument is `{fs}`, but the parameter is `{es}`"),
            ),
            Why::Operand => ("E0101", format!("cannot combine `{fs}` and `{es}`")),
            Why::Condition => (
                "E0202",
                format!("a condition must be `bool`, but this is `{fs}`"),
            ),
            Why::Annotation => (
                "E0202",
                format!("mismatched types: expected `{es}`, found `{fs}`"),
            ),
            Why::Assignment => (
                "E0202",
                format!("cannot assign `{fs}` to a variable of type `{es}`"),
            ),
            Why::Element => (
                "E0202",
                format!("this element is `{fs}`, but the others are `{es}`"),
            ),
            Why::Field => (
                "E0202",
                format!("this field is `{es}`, but the value is `{fs}`"),
            ),
        };
        self.errors.push(
            Diagnostic::error(message)
                .with_code(code)
                .with_primary_span(span.start.0, span.end.0, None),
        );
    }

    /// A type as the programmer writes it.
    fn name(&mut self, t: &Ty) -> String {
        match self.resolve(t, false) {
            Ty::Var(v) => match self.nodes[v as usize].kind {
                Kind::Int => "integer".into(),
                Kind::Float => "float".into(),
                Kind::Any => "_".into(),
            },
            Ty::Unknown => "_".into(),
            Ty::Scalar(d) => d.to_string(),
            Ty::Str => "str".into(),
            Ty::Named(n) | Ty::Dyn(n) | Ty::Param(n) => {
                n.rsplit("__").next().unwrap_or(&n).to_string()
            }
            Ty::List(e) => format!("list<{}>", self.name(&e)),
            Ty::Map(k, v) => format!("map<{}, {}>", self.name(&k), self.name(&v)),
            Ty::Option(e) => format!("option<{}>", self.name(&e)),
            Ty::Result(a, b) => format!("result<{}, {}>", self.name(&a), self.name(&b)),
            Ty::Tuple(ts) => format!(
                "({})",
                ts.iter()
                    .map(|t| self.name(t))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Ty::Array(e, n) => format!("[{}; {n}]", self.name(&e)),
            Ty::Chan(e) => format!("chan<{}>", self.name(&e)),
            Ty::Task(e) => format!("task<{}>", self.name(&e)),
            Ty::Fn(ps, r) => format!(
                "({}) -> {}",
                ps.iter()
                    .map(|t| self.name(t))
                    .collect::<Vec<_>>()
                    .join(", "),
                self.name(&r)
            ),
            Ty::Other(ir) => crate::lower::hints::type_name(&ir),
        }
    }

    /// The type of a value that is `a` or `b`, once unified: each part from
    /// whichever side has it, where the other's is unmodelled (`Unknown`)
    /// -- so `if c { some(f(x)) } else { none }`, with `f` unmodelled, is
    /// still `option<_>` with the `none`'s element, which the context (a
    /// return type, an annotation) then decides.
    fn join(&mut self, a: &Ty, b: &Ty) -> Ty {
        match (self.shallow(a), self.shallow(b)) {
            (Ty::Unknown | Ty::Other(_), other) | (other, Ty::Unknown | Ty::Other(_))
                if !matches!(other, Ty::Unknown) =>
            {
                other
            }
            (Ty::List(x), Ty::List(y)) => Ty::List(Box::new(self.join(&x, &y))),
            (Ty::Option(x), Ty::Option(y)) => Ty::Option(Box::new(self.join(&x, &y))),
            (Ty::Chan(x), Ty::Chan(y)) => Ty::Chan(Box::new(self.join(&x, &y))),
            (Ty::Task(x), Ty::Task(y)) => Ty::Task(Box::new(self.join(&x, &y))),
            (Ty::Map(k1, v1), Ty::Map(k2, v2)) => {
                Ty::Map(Box::new(self.join(&k1, &k2)), Box::new(self.join(&v1, &v2)))
            }
            (Ty::Result(o1, e1), Ty::Result(o2, e2)) => {
                Ty::Result(Box::new(self.join(&o1, &o2)), Box::new(self.join(&e1, &e2)))
            }
            (Ty::Tuple(xs), Ty::Tuple(ys)) if xs.len() == ys.len() => Ty::Tuple(
                xs.iter()
                    .zip(ys.iter())
                    .map(|(x, y)| self.join(x, y))
                    .collect(),
            ),
            (x, _) => x,
        }
    }

    /// A `?` at `at` passing on an error of type `err` (`None`: an option's
    /// absence): to the enclosing `try`'s catch, which every `?` in it must
    /// agree on, or out of the function, which must return a result (an
    /// option) with that error.
    fn propagate(&mut self, err: Option<&Ty>, at: Span) {
        if let Some(top) = self.try_errs.last().cloned() {
            if let Some(e) = err {
                if self.unify_inner(e, &top).is_err() {
                    let (en, tn) = (self.name(e), self.name(&top));
                    self.errors.push(
                        Diagnostic::error(format!("this `?` passes on a `{en}`, but an earlier `?` in this `try` passes on a `{tn}`"))
                            .with_code("E0202")
                            .with_primary_span(at.start.0, at.end.0, None)
                            .with_note("the `catch` receives the error, so every `?` in its `try` must carry the same type"),
                    );
                }
            }
            return;
        }
        let Some(ret) = self.ret.clone() else { return };
        let fn_name = self.fn_name.clone();
        match (self.resolve(&ret, false), err) {
            // Not modelled here, or a lambda: lowering's to judge.
            (Ty::Unknown | Ty::Param(_) | Ty::Var(_) | Ty::Other(_), _) => {}
            (Ty::Result(_, re), Some(e)) => {
                if self.unify_inner(e, &re).is_err() {
                    let (en, rn) = (self.name(e), self.name(&re));
                    self.errors.push(
                        Diagnostic::error(format!("this `?` returns a `{en}` error from `{fn_name}`, whose errors are `{rn}`"))
                            .with_code("E0209")
                            .with_primary_span(at.start.0, at.end.0, None),
                    );
                }
            }
            (Ty::Option(_), None) => {}
            (r, _) => {
                let (rn, what) = (
                    self.name(&r),
                    if err.is_some() {
                        "a `result`"
                    } else {
                        "an `option`"
                    },
                );
                self.errors.push(
                    Diagnostic::error(format!("`?` returns early from `{fn_name}`, which returns `{rn}`: only a function returning {what} can"))
                        .with_code("E0202")
                        .with_primary_span(at.start.0, at.end.0, None)
                        .with_help("handle the value here with `when` or `??` and a default, or inside `try { ... } catch e { ... }`"),
                );
            }
        }
    }

    /// Two different records: branches yielding them are coerced to the
    /// trait object their context expects, which lowering judges.
    fn different_records(&mut self, a: &Ty, b: &Ty) -> bool {
        match (self.resolve(a, false), self.resolve(b, false)) {
            (Ty::Named(x), Ty::Named(y)) => {
                x != y
                    && self.reg.module.struct_def(&x).is_some()
                    && self.reg.module.struct_def(&y).is_some()
            }
            _ => false,
        }
    }

    // ── Between `IrType` and `Ty` ──────────────────────────────────────────

    fn ty_from_ir(&mut self, t: &IrType) -> Ty {
        match t {
            IrType::Scalar(d) => Ty::Scalar(*d),
            IrType::Str => Ty::Str,
            IrType::Struct { name, .. } => {
                if self.type_params.contains(name) {
                    Ty::Param(name.clone())
                } else if name.is_empty() {
                    Ty::Unknown
                } else {
                    Ty::Named(name.clone())
                }
            }
            IrType::Enum { name, .. } => Ty::Named(name.clone()),
            IrType::TraitObject { name, .. } => Ty::Dyn(name.clone()),
            IrType::List(e) => Ty::List(Box::new(self.ty_from_ir(e))),
            IrType::Map(k, v) => {
                Ty::Map(Box::new(self.ty_from_ir(k)), Box::new(self.ty_from_ir(v)))
            }
            IrType::Option(e) => Ty::Option(Box::new(self.ty_from_ir(e))),
            IrType::ResultType(a, b) => {
                Ty::Result(Box::new(self.ty_from_ir(a)), Box::new(self.ty_from_ir(b)))
            }
            IrType::Tuple(ts) => Ty::Tuple(ts.iter().map(|t| self.ty_from_ir(t)).collect()),
            IrType::Array { elem, len } => Ty::Array(Box::new(self.ty_from_ir(elem)), *len),
            IrType::Chan(e) => Ty::Chan(Box::new(self.ty_from_ir(e))),
            IrType::Task(e) => Ty::Task(Box::new(self.ty_from_ir(e))),
            IrType::Fn { params, ret } => Ty::Fn(
                params.iter().map(|t| self.ty_from_ir(t)).collect(),
                Box::new(self.ty_from_ir(ret)),
            ),
            other => Ty::Other(other.clone()),
        }
    }

    /// The `IrType` of `t`, if nothing in it is unknown.
    fn ty_to_ir(&mut self, t: &Ty) -> Option<IrType> {
        self.hint_of(t).known()
    }

    /// What lowering learns of `t`: a part the checker could not type is
    /// `Open` (lowering's own typing decides it); a part nothing in the
    /// program constrains at all is `i64`, the documented default for parts
    /// no read observes (a read of a wholly unknown value is E0207); a
    /// generic body's parameter is `Param`, which lowering substitutes per
    /// instance.
    fn hint_of(&mut self, t: &Ty) -> Hint {
        let h = match self.resolve(t, true) {
            Ty::Var(r) => {
                let node = &self.nodes[r as usize];
                if node.opaque || node.kind != Kind::Any {
                    Hint::Open
                } else {
                    Hint::Known(IrType::Scalar(DType::I64))
                }
            }
            Ty::Unknown => Hint::Open,
            Ty::Param(n) => Hint::Param(n),
            Ty::Scalar(d) => Hint::Known(IrType::Scalar(d)),
            Ty::Str => Hint::Known(IrType::Str),
            // The registered types, fields and methods included: lowering
            // compares them whole.
            Ty::Named(n) => {
                if let Some(variants) = self.reg.module.enum_def(&n) {
                    Hint::Known(IrType::Enum {
                        name: n.clone(),
                        variants: variants.clone(),
                    })
                } else if let Some(fields) = self.reg.module.struct_def(&n) {
                    Hint::Known(IrType::Struct {
                        name: n.clone(),
                        fields: fields.clone(),
                    })
                } else {
                    Hint::Open
                }
            }
            Ty::Dyn(n) => match self.reg.module.trait_def(&n) {
                Some(methods) => Hint::Known(IrType::TraitObject {
                    name: n.clone(),
                    methods: methods.clone(),
                }),
                None => Hint::Open,
            },
            Ty::List(e) => Hint::List(Box::new(self.hint_of(&e))),
            Ty::Map(k, v) => Hint::Map(Box::new(self.hint_of(&k)), Box::new(self.hint_of(&v))),
            Ty::Option(e) => Hint::Option(Box::new(self.hint_of(&e))),
            Ty::Result(a, b) => {
                Hint::Result(Box::new(self.hint_of(&a)), Box::new(self.hint_of(&b)))
            }
            Ty::Tuple(ts) => Hint::Tuple(ts.iter().map(|t| self.hint_of(t)).collect()),
            Ty::Array(e, n) => Hint::Array(Box::new(self.hint_of(&e)), n),
            Ty::Chan(e) => Hint::Chan(Box::new(self.hint_of(&e))),
            Ty::Task(e) => Hint::Task(Box::new(self.hint_of(&e))),
            Ty::Fn(ps, r) => Hint::Fn(
                ps.iter().map(|t| self.hint_of(t)).collect(),
                Box::new(self.hint_of(&r)),
            ),
            Ty::Other(ir) => Hint::Known(ir),
        };
        h.normalized()
    }

    /// An annotation, as lowering reads it.
    fn ast_ty(&mut self, t: &AstType) -> Ty {
        if let AstType::Named(n, _) = t {
            if self.type_params.contains(n) {
                return Ty::Param(n.clone());
            }
            if n == "Infer" {
                return self.fresh_opaque();
            }
        }
        if self.mentions_type_param(t) {
            // `option<T>`: the structure, with `T` the parameter.
            let subs: HashMap<String, Ty> = self
                .type_params
                .iter()
                .map(|p| (p.clone(), Ty::Param(p.clone())))
                .collect();
            return self.ast_ty_subst(t, &subs).unwrap_or(Ty::Unknown);
        }
        // A type lowering will reject (it reports why) agrees with everything.
        match super::lower_type_with_structs(t, self.reg.module) {
            Ok(IrType::Infer) | Err(_) => Ty::Unknown,
            Ok(ir) => self.ty_from_ir(&ir),
        }
    }

    fn mentions_type_param(&self, t: &AstType) -> bool {
        match t {
            AstType::Named(n, _) => self.type_params.contains(n),
            AstType::Generic { name, args, .. } => {
                self.type_params.contains(name) || args.iter().any(|a| self.mentions_type_param(a))
            }
            AstType::Tuple(ts, _) => ts.iter().any(|a| self.mentions_type_param(a)),
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
            | AstType::Slice(e, _) => self.mentions_type_param(e),
            AstType::Array { elem, len_expr, .. } => {
                self.mentions_type_param(elem)
                    || len_expr
                        .as_ref()
                        .is_some_and(|e| self.type_params.iter().any(|p| expr_mentions(e, p)))
            }
            AstType::Result(a, b, _) | AstType::Map(a, b, _) => {
                self.mentions_type_param(a) || self.mentions_type_param(b)
            }
            AstType::Fn { params, ret, .. } => {
                params.iter().any(|a| self.mentions_type_param(a)) || self.mentions_type_param(ret)
            }
            _ => false,
        }
    }

    // ── Names ──────────────────────────────────────────────────────────────

    fn lookup_local(&self, name: &str) -> Option<Ty> {
        self.scopes.iter().rev().find_map(|s| s.get(name).cloned())
    }

    fn bind(&mut self, name: &str, t: Ty) {
        if let Some(s) = self.scopes.last_mut() {
            s.insert(name.to_string(), t);
        }
    }

    /// The name lowering resolves an unqualified function, constant or type
    /// name to (see `Lowerer::resolve_unqualified_name`).
    fn resolve_name(&self, name: &str) -> String {
        resolve_known_name(self.reg, name)
    }

    /// A name the program or the language knows as a value: a local in
    /// scope, a constant, a defined function, or a built-in. Judging which
    /// misspelling to suggest (`bool` for `boolean`).
    fn known_value_name(&self, name: &str) -> bool {
        self.scopes.iter().any(|s| s.contains_key(name))
            || self.reg.fn_sigs.contains_key(name)
            || self.reg.const_defs.contains_key(name)
            || self.reg.inferred_fns.contains(name)
            || is_builtin_value_name(name)
    }

    /// Whether a call of `name` reaches lowering's built-in dispatch (its
    /// special cases before the general function path), which the checker
    /// does not model.
    fn known_call_name(&self, name: &str) -> bool {
        is_builtin_call_name(name)
    }

    /// The declared types of a (possibly mangled) function's parameters --
    /// a `def`'s or an impl method's, `self` included for the latter. `[]`
    /// for one the registries do not type.
    fn params_of(&mut self, mangled: &str) -> Vec<Ty> {
        self.reg
            .fn_param_types
            .get(mangled)
            .or_else(|| self.reg.method_param_types.get(mangled))
            .map(|ps| ps.iter().map(|p| self.ty_from_ir(p)).collect())
            .unwrap_or_default()
    }

    /// The type of referring to a defined function without calling it: its
    /// signature. Parameters the registries do not type (a builtin's) are
    /// left out: a first-class use of `len` is unmodelled.
    fn fn_ty(&mut self, resolved: &str) -> Ty {
        let ps = self.params_of(resolved);
        let r = self
            .reg
            .fn_sigs
            .get(resolved)
            .cloned()
            .map(|r| self.ty_from_ir(&r))
            .unwrap_or(Ty::Unknown);
        Ty::Fn(ps, Box::new(r))
    }

    /// Lowering's argument-count error (§4.2), judged here: `callee` was
    /// given `given` arguments; it takes `required..=max`. The call's
    /// declared signature, when it is one the ASTs hold, is the note.
    fn arity_diag(
        &self,
        resolved: &str,
        callee: &str,
        required: usize,
        max: usize,
        given: usize,
        span: Span,
    ) -> Diagnostic {
        let plural = |n: usize| if n == 1 { "argument" } else { "arguments" };
        let takes = if required == max {
            format!("{required} {}", plural(max))
        } else {
            format!("{required} to {max} arguments")
        };
        let were = if given == 1 { "was" } else { "were" };
        let mut diag =
            Diagnostic::error(format!("`{callee}` takes {takes} but {given} {were} given"))
                .with_primary_span(span.start.0, span.end.0, None);
        if let Some(signature) = super::signature_of(resolved) {
            diag = diag.with_note(format!("declared as `{signature}`"));
        }
        diag.with_code("E0109")
    }

    /// Lowering's unknown-name error (§4.2), judged on this tree. `call`:
    /// the name is being called, so `range(a, b)` is rewritten to a range,
    /// and the module that defines it `pub` can be suggested.
    fn unknown_name(&self, name: &str, span: Span, call: Option<(Span, &[AstExpr])>) -> Diagnostic {
        let mut diag = Diagnostic::error(format!("cannot find `{name}` in this scope"))
            .with_primary_span(span.start.0, span.end.0, Some("not found".into()));

        // `range(n)` and `range(a, b)` are `0..n` and `a..b`.
        if let (Some((call_span, args)), "range") = (call, name) {
            if (1..=2).contains(&args.len()) {
                let first = args[0].span();
                let last = args[args.len() - 1].span();
                let mut edits = vec![Edit {
                    start: call_span.start.0,
                    end: first.start.0,
                    replacement: if args.len() == 1 {
                        "0..".into()
                    } else {
                        String::new()
                    },
                }];
                if args.len() == 2 {
                    edits.push(Edit {
                        start: first.end.0,
                        end: last.start.0,
                        replacement: "..".into(),
                    });
                }
                edits.push(Edit {
                    start: last.end.0,
                    end: call_span.end.0,
                    replacement: String::new(),
                });
                return diag
                    .with_fix(Fix {
                        message: "IRIS writes a range as `start..end`".into(),
                        edits,
                    })
                    .with_code("E0100");
            }
        }
        if let Some((replacement, why)) = super::hints::foreign_name(name, call.is_some()) {
            if self.known_value_name(replacement) {
                return diag
                    .with_note(why)
                    .with_fix(Fix::replace(
                        format!("use `{replacement}`"),
                        span.start.0,
                        span.end.0,
                        replacement,
                    ))
                    .with_code("E0100");
            }
        }
        let candidates: Vec<&str> = self
            .scopes
            .iter()
            .flat_map(|s| s.keys().map(|k| k.as_str()))
            .chain(self.reg.fn_sigs.keys().map(|s| s.as_str()))
            .chain(self.reg.const_defs.keys().map(|s| s.as_str()))
            .chain(crate::lsp::builtin_function_names().iter().copied())
            .collect();
        if let Some(near) = super::hints::closest(name, candidates) {
            return diag
                .with_fix(Fix::replace(
                    format!("did you mean `{near}`?"),
                    span.start.0,
                    span.end.0,
                    near,
                ))
                .with_code("E0100");
        }
        if call.is_some() {
            if let Some(module) = super::hints::module_exporting(name) {
                return diag
                    .with_note(format!("`{name}` is defined in `std.{module}`"))
                    .with_fix(Fix::insert(
                        format!("bring `std.{module}`"),
                        0,
                        format!("bring std.{module}\n"),
                    ))
                    .with_code("E0100");
            }
        }
        if let Some(module) = self.private_item_hint(name) {
            diag = diag.with_help(format!(
                "`{name}` is defined in module `{module}` but is not `pub`; add `pub` to its definition to use it here"
            ));
        }
        diag.with_code("E0100")
    }

    /// If `name` is unresolvable only because the matching item is not
    /// `pub`, the module that defines it (see lowering's `#13` hint).
    fn private_item_hint(&self, name: &str) -> Option<String> {
        let suffix = format!("__{name}");
        super::CURRENT_PRIVATE_ITEMS.with(|p| {
            p.borrow()
                .iter()
                .find(|m| m.ends_with(&suffix))
                .map(|m| m[..m.len() - suffix.len()].to_owned())
        })
    }

    /// Lowering's unknown and missing fields of a record literal (§4.2),
    /// judged on this tree: one field written that the record does not have
    /// (E0113), or one declared without a default and not written (E0114),
    /// with lowering's fixes.
    fn struct_literal_fields(
        &mut self,
        resolved: &str,
        declared: &[(String, IrType)],
        fields: &[(String, AstExpr)],
        field_spans: &[Span],
        has_spread: bool,
        span: Span,
    ) {
        let record = resolved.split("__").next().unwrap_or(resolved);
        let defaults = self
            .reg
            .module
            .struct_defaults(resolved)
            .or_else(|| self.reg.module.struct_defaults(record))
            .cloned();
        let has_default = |i: usize| {
            defaults
                .as_ref()
                .and_then(|d| d.get(i))
                .is_some_and(|d| d.is_some())
        };
        let missing: Vec<(usize, &(String, IrType))> = if has_spread {
            Vec::new()
        } else {
            declared
                .iter()
                .enumerate()
                .filter(|(i, (n, _))| !fields.iter().any(|(f, _)| f == n) && !has_default(*i))
                .collect()
        };
        let field_list = declared
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>()
            .join(", ");

        if let Some((idx, (unknown, value))) = fields
            .iter()
            .enumerate()
            .find(|(_, (n, _))| !declared.iter().any(|(d, _)| d == n))
        {
            let at = field_spans
                .get(idx)
                .copied()
                .unwrap_or_else(|| value.span());
            let mut diag = Diagnostic::error(format!("`{record}` has no field `{unknown}`"))
                .with_primary_span(at.start.0, at.end.0, Some("unknown field".into()))
                .with_note(format!("`{record}` has fields: {field_list}"));
            // One field unknown and one missing: almost certainly a misspelling.
            let rename = if missing.len() == 1 {
                Some(missing[0].1 .0.clone())
            } else {
                super::hints::closest(unknown, missing.iter().map(|(_, (n, _))| n.as_str()))
            };
            if let (Some(to), Some(_)) = (rename, field_spans.get(idx)) {
                diag = diag.with_fix(Fix::replace(
                    format!("rename it to `{to}`"),
                    at.start.0,
                    at.end.0,
                    to,
                ));
            }
            self.errors.push(diag.with_code("E0113"));
            return;
        }

        if !missing.is_empty() {
            let names: Vec<String> = missing.iter().map(|(_, (n, _))| format!("`{n}`")).collect();
            let message = if names.len() == 1 {
                format!("missing field {} in `{record}`", names[0])
            } else {
                format!("missing fields {} in `{record}`", names.join(", "))
            };
            let mut diag =
                Diagnostic::error(message).with_primary_span(span.start.0, span.end.0, None);
            let fill: Option<Vec<String>> = missing
                .iter()
                .map(|(_, (n, ty))| super::hints::placeholder(ty).map(|v| format!("{n}: {v}")))
                .collect();
            if let Some(fill) = fill {
                let text = fill.join(", ");
                let fix = match fields.last() {
                    Some((_, last)) => Fix::insert(
                        format!("add {}", names.join(", ")),
                        last.span().end.0,
                        format!(", {text}"),
                    ),
                    None => Fix::insert(
                        format!("add {}", names.join(", ")),
                        span.end.0.saturating_sub(1),
                        format!(" {text} "),
                    ),
                };
                diag = diag.with_fix(fix);
            }
            self.errors.push(diag.with_code("E0114"));
        }
    }

    /// Lowering's no-such-field error on a field access into a record the
    /// registries type: the access ends at the field name, so the error
    /// points there (`p.z`, not `p`).
    fn no_such_field(
        &self,
        record: &str,
        field: &str,
        fields: &[(String, IrType)],
        at: Span,
    ) -> Diagnostic {
        let record = record.split("__").next().unwrap_or(record);
        let list = fields
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let mut diag = Diagnostic::error(format!("`{record}` has no field `{field}`"))
            .with_primary_span(at.start.0, at.end.0, Some("unknown field".into()))
            .with_note(format!("`{record}` has fields: {list}"));
        if let Some(near) = super::hints::closest(field, fields.iter().map(|(n, _)| n.as_str())) {
            diag = diag.with_fix(Fix::replace(
                format!("did you mean `{near}`?"),
                at.start.0,
                at.end.0,
                near,
            ));
        }
        diag.with_code("E0113")
    }

    /// Lowering's exhaustiveness judgment (§4.2), on this tree: a `when` on
    /// a choice the registries know, whose arms are all variant patterns
    /// (no guard, no wildcard, no pattern the checker does not model),
    /// handles every variant or not (E0210). Anything else is lowering's
    /// and the pass's to judge.
    fn when_exhaustive(&mut self, arms: &[AstWhenArm], span: Span, s: &Ty) {
        let resolved = match self.resolve(s, false) {
            Ty::Named(n) => n,
            _ => return,
        };
        if arms.is_empty() {
            return;
        }
        let Some(variants) = self.reg.module.enum_def(&resolved).cloned() else {
            return;
        };
        let mut handled: HashSet<usize> = HashSet::new();
        for arm in arms {
            if arm.guard.is_some() {
                return;
            }
            let names: Vec<String> = match &arm.pattern {
                AstWhenPattern::EnumVariant { enum_name, variant_name, .. } => {
                    if enum_name != &resolved {
                        // A name the checker does not resolve the same way,
                        // or a bare binding: bundled up with it.
                        return;
                    }
                    vec![variant_name.clone()]
                }
                AstWhenPattern::Or(alts) if alts.iter().all(|p| matches!(p, AstWhenPattern::EnumVariant { enum_name, .. } if enum_name == &resolved)) => {
                    alts.iter()
                        .filter_map(|p| match p {
                            AstWhenPattern::EnumVariant { variant_name, .. } => Some(variant_name.clone()),
                            _ => None,
                        })
                        .collect()
                }
                AstWhenPattern::Wildcard
                | AstWhenPattern::OptionSome { .. }
                | AstWhenPattern::OptionNone
                | AstWhenPattern::ResultOk { .. }
                | AstWhenPattern::ResultErr { .. }
                | AstWhenPattern::Or(_)
                | AstWhenPattern::IntLit(_)
                | AstWhenPattern::FloatLit(_)
                | AstWhenPattern::BoolLit(_)
                | AstWhenPattern::StringLit(_)
                | AstWhenPattern::Tuple(_)
                | AstWhenPattern::Range { .. }
                | AstWhenPattern::Slice { .. }
                | AstWhenPattern::Binding { .. }
                | AstWhenPattern::Struct { .. } => return,
            };
            for name in &names {
                match variants.iter().position(|v| v == name) {
                    Some(i) => {
                        handled.insert(i);
                    }
                    // An arm naming what the choice has no variant of:
                    // lowering's E0122 is on its way.
                    None => return,
                }
            }
        }
        let missing: Vec<String> = variants
            .iter()
            .enumerate()
            .filter(|(i, _)| !handled.contains(i))
            .map(|(_, v)| format!("`{resolved}.{v}`"))
            .collect();
        if missing.is_empty() {
            return;
        }
        let mut diag = Diagnostic::error(format!(
            "this `when` does not handle {}",
            missing.join(", ")
        ))
        .with_help("add an arm for each, or a `_ =>` arm for the rest");
        // The `when` keyword is what the pass points at.
        let at = std::cmp::min(span.start.0 + 4, span.end.0);
        diag = diag.with_primary_span(span.start.0, at, None);
        self.errors.push(diag.with_code("E0210"));
    }

    // ── Functions ──────────────────────────────────────────────────────────

    fn check_function(&mut self, f: &AstFunction) {
        self.type_params = f
            .type_params
            .iter()
            .map(|p| match p {
                crate::parser::ast::AstGenericParam::Type(n, _, _) => n.clone(),
                crate::parser::ast::AstGenericParam::Hkt(n, _, _, _) => n.clone(),
                crate::parser::ast::AstGenericParam::Const { name, .. } => name.clone(),
            })
            .collect();
        self.fn_name = f
            .name
            .name
            .rsplit("__")
            .next()
            .unwrap_or(&f.name.name)
            .to_string();
        self.scopes = vec![HashMap::new()];
        for tp in &f.type_params {
            if let crate::parser::ast::AstGenericParam::Const { name, .. } = tp {
                let cty = self.fresh(Kind::Int);
                self.bind(name, cty);
            }
        }
        for p in &f.params {
            let t = self.ast_ty(&p.ty);
            self.bind(&p.name.name, t);
        }
        // An `async def`'s body returns its declared type; the call returns
        // a task of it (the registries say so).
        let ret = self.ast_ty(&f.return_ty);
        self.ret = Some(ret.clone());
        self.ret_span = Some(f.return_ty.span());
        if let Some(tail) = &f.body.tail {
            if returns_value(&f.return_ty) {
                self.using(tail);
            }
        }
        let body = self.infer_block(&f.body);
        let inferred_return = matches!(&f.return_ty, AstType::Named(n, _) if n == "Infer");
        if let Some(tail) = &f.body.tail {
            // A value-returning function's tail is its result. `unit`
            // functions, and generics, are lowering's to judge.
            if !matches!(ret, Ty::Unknown) && returns_value(&f.return_ty) {
                let because = Some((
                    f.return_ty.span(),
                    "the return type is declared here".to_string(),
                ));
                self.expect(tail, &body, &ret, Why::Return, because);
            }
            // `-> Infer`: the tail decides the return type.
            if inferred_return {
                self.unify(&body, &ret, tail.span(), Why::Return);
            }
        }
        if inferred_return {
            self.solved_returns.push((f.name.name.clone(), ret.clone()));
        }
        self.check_reads();
        self.scopes.clear();
        self.ret = None;
        self.ret_span = None;
        self.type_params.clear();
    }

    // ── Statements ─────────────────────────────────────────────────────────

    fn infer_block(&mut self, b: &AstBlock) -> Ty {
        self.scopes.push(HashMap::new());
        for s in &b.stmts {
            self.check_stmt(s);
        }
        let t = match &b.tail {
            Some(e) => self.infer(e),
            None => Ty::Unknown,
        };
        self.scopes.pop();
        t
    }

    fn check_stmt(&mut self, s: &AstStmt) {
        match s {
            AstStmt::Let { name, ty, init, .. } => {
                let t = self.infer(init);
                let bound = match ty {
                    Some(ann) => {
                        let a = self.ast_ty(ann);
                        let because = (
                            ann.span(),
                            "expected because of this annotation".to_string(),
                        );
                        self.expect(init, &t, &a, Why::Annotation, Some(because));
                        a
                    }
                    // A literal takes its type where it is bound -- `i64` or
                    // `f64` -- not from a later use, as lowering types it.
                    None => self.resolve(&t, true),
                };
                self.bind(&name.name, bound);
            }
            AstStmt::LetTuple { names, init, .. } => {
                let t = self.infer(init);
                let parts = match self.resolve(&t, false) {
                    Ty::Tuple(ts) if ts.len() == names.len() => ts,
                    _ => vec![Ty::Unknown; names.len()],
                };
                for (n, t) in names.iter().zip(parts) {
                    self.bind(&n.name, t);
                }
            }
            AstStmt::Expr(e) => {
                self.infer(e);
            }
            AstStmt::Assign {
                target,
                op,
                value,
                span,
            } => {
                let v = self.infer(value);
                let t = self.infer(target);
                if op.is_none() {
                    self.unify(&v, &t, value.span(), Why::Assignment);
                } else {
                    let _ = span;
                    self.unify(&v, &t, value.span(), Why::Operand);
                }
            }
            AstStmt::While { cond, body, .. } => {
                self.using(cond);
                let c = self.infer(cond);
                self.unify(&c, &Ty::Scalar(DType::Bool), cond.span(), Why::Condition);
                self.infer_block(body);
            }
            AstStmt::Loop { body, .. } => {
                self.infer_block(body);
            }
            AstStmt::ForRange {
                var,
                start,
                end,
                step,
                body,
                ..
            } => {
                let a = self.infer(start);
                let b = self.infer(end);
                self.unify(&b, &a, end.span(), Why::Operand);
                if let Some(st) = step {
                    let s = self.infer(st);
                    self.unify(&s, &a, st.span(), Why::Operand);
                }
                self.scopes.push(HashMap::new());
                self.bind(&var.name, a);
                self.infer_block(body);
                self.scopes.pop();
            }
            AstStmt::ParFor {
                var,
                start,
                end,
                body,
                ..
            } => {
                let a = self.infer(start);
                let b = self.infer(end);
                self.unify(&b, &a, end.span(), Why::Operand);
                self.scopes.push(HashMap::new());
                self.bind(&var.name, a);
                self.infer_block(body);
                self.scopes.pop();
            }
            AstStmt::ForEach {
                var, iter, body, ..
            } => {
                let it = self.infer(iter);
                let elem = match self.resolve(&it, false) {
                    Ty::List(e) | Ty::Array(e, _) => *e,
                    _ => Ty::Unknown,
                };
                self.scopes.push(HashMap::new());
                self.bind(&var.name, elem);
                self.infer_block(body);
                self.scopes.pop();
            }
            AstStmt::Return { value: Some(v), .. } => {
                self.using(v);
                let t = self.infer(v);
                if let Some(r) = self.ret.clone() {
                    let because = self
                        .ret_span
                        .map(|s| (s, "the return type is declared here".to_string()));
                    self.expect(v, &t, &r, Why::Return, because);
                }
            }
            // A bare `return` in a value-returning function is the binding
            // checker's (E0209), before lowering.
            AstStmt::Return { value: None, .. }
            | AstStmt::Break { .. }
            | AstStmt::Continue { .. } => {}
            AstStmt::Spawn { body, group, .. } => {
                if let Some(g) = group {
                    self.infer(g);
                }
                self.scopes.push(HashMap::new());
                for s in body {
                    self.check_stmt(s);
                }
                self.scopes.pop();
            }
            AstStmt::MaskStmt { body, .. } => {
                self.infer_block(body);
            }
            AstStmt::HandleStmt { expr, .. } => {
                self.infer(expr);
            }
            AstStmt::Defer { expr, .. } | AstStmt::Yield { expr, .. } => {
                self.infer(expr);
            }
            AstStmt::Select { arms, default, .. } => {
                for arm in arms {
                    self.infer(&arm.channel);
                    self.scopes.push(HashMap::new());
                    self.bind(&arm.binding, Ty::Unknown);
                    self.infer_block(&arm.body);
                    self.scopes.pop();
                }
                if let Some(d) = default {
                    self.infer_block(d);
                }
            }
        }
    }

    // ── Expressions ────────────────────────────────────────────────────────

    fn infer(&mut self, e: &AstExpr) -> Ty {
        let t = self.infer_inner(e);
        let s = e.span();
        self.recorded.push(((s.start.0, s.end.0), t.clone()));
        if self.used.remove(&(s.start.0, s.end.0)) {
            self.reads.push((s, t.clone()));
        }
        t
    }

    /// `e`'s value is used -- an operand, an argument, a condition, a
    /// returned value -- so its type must be known (E0207 otherwise).
    fn using(&mut self, e: &AstExpr) {
        let s = e.span();
        if s.end.0 > s.start.0 {
            self.used.insert((s.start.0, s.end.0));
        }
    }

    /// A used value whose type nothing in the program determines: E0207,
    /// once per unknown. Unknown *parts* of a value (`result<i64, _>`, an
    /// empty list's elements) are not reported; they take `i64`, which no
    /// read can observe.
    fn check_reads(&mut self) {
        let reads = std::mem::take(&mut self.reads);
        let mut reported: HashSet<u32> = HashSet::new();
        for (span, t) in reads {
            let Ty::Var(r) = self.resolve(&t, true) else {
                continue;
            };
            let node = &self.nodes[r as usize];
            if node.kind != Kind::Any || node.opaque || !reported.insert(r) {
                continue;
            }
            self.errors.push(
                Diagnostic::error("type annotations needed: nothing in the program determines the type of this value".to_string())
                    .with_code("E0207")
                    .with_primary_span(span.start.0, span.end.0, None)
                    .with_help("annotate where the value is created, for example `val names: list<str> = list()`"),
            );
        }
        self.used.clear();
    }

    fn infer_inner(&mut self, e: &AstExpr) -> Ty {
        match e {
            AstExpr::IntLit { .. } => self.fresh(Kind::Int),
            AstExpr::FloatLit { .. } => self.fresh(Kind::Float),
            AstExpr::BoolLit { .. } => Ty::Scalar(DType::Bool),
            AstExpr::StringLit { .. } => Ty::Str,
            AstExpr::Ident(id) => self.infer_ident(id),
            AstExpr::BinOp { op, lhs, rhs, .. } => {
                self.using(lhs);
                self.using(rhs);
                let l = self.infer(lhs);
                let r = self.infer(rhs);
                match op {
                    AstBinOp::And | AstBinOp::Or => {
                        self.unify(&l, &Ty::Scalar(DType::Bool), lhs.span(), Why::Operand);
                        self.unify(&r, &Ty::Scalar(DType::Bool), rhs.span(), Why::Operand);
                        Ty::Scalar(DType::Bool)
                    }
                    _ => {
                        // Operators on records and tensors are overloaded or
                        // elementwise: lowering's to judge.
                        let lr = self.resolve(&l, false);
                        // `"ab" * 3` repeats a string.
                        if matches!(op, AstBinOp::Mul) && lr == Ty::Str {
                            self.unify(&r, &Ty::Scalar(DType::I64), rhs.span(), Why::Operand);
                            return Ty::Str;
                        }
                        // An option, result, list, map, tuple or channel
                        // operates only on its own type (`i < 0` with `i`
                        // an `option<i64>` is the classic mistake).
                        if matches!(
                            lr,
                            Ty::Option(_)
                                | Ty::Result(..)
                                | Ty::List(_)
                                | Ty::Map(..)
                                | Ty::Tuple(_)
                                | Ty::Chan(_)
                        ) {
                            if let Err((f, x)) = self.unify_inner(&r, &l) {
                                match (self.ty_to_ir(&l), self.ty_to_ir(&r)) {
                                    (Some(lt), Some(rt)) => {
                                        self.errors.push(super::binop_mismatch_diag(
                                            *op,
                                            lhs,
                                            &lt,
                                            rhs,
                                            &rt,
                                            e.span(),
                                        ))
                                    }
                                    _ => self.mismatch(&f, &x, rhs.span(), Why::Operand),
                                }
                            }
                            return if is_comparison(*op) {
                                Ty::Scalar(DType::Bool)
                            } else {
                                Ty::Unknown
                            };
                        }
                        if !matches!(lr, Ty::Var(_) | Ty::Scalar(_) | Ty::Str) {
                            return if is_comparison(*op) {
                                Ty::Scalar(DType::Bool)
                            } else {
                                Ty::Unknown
                            };
                        }
                        if let Err((f, x)) = self.unify_inner(&r, &l) {
                            // Lowering's diagnostic, with its fix (`1.0`,
                            // `to_str(3)`); the result is not known, so no
                            // error follows from it.
                            match (self.ty_to_ir(&l), self.ty_to_ir(&r)) {
                                (Some(lt), Some(rt)) => self.errors.push(
                                    super::binop_mismatch_diag(*op, lhs, &lt, rhs, &rt, e.span()),
                                ),
                                _ => self.mismatch(&f, &x, rhs.span(), Why::Operand),
                            }
                            return if is_comparison(*op) {
                                Ty::Scalar(DType::Bool)
                            } else {
                                Ty::Unknown
                            };
                        }
                        if is_comparison(*op) {
                            Ty::Scalar(DType::Bool)
                        } else {
                            l
                        }
                    }
                }
            }
            AstExpr::UnaryOp { op, expr, .. } => {
                self.using(expr);
                let t = self.infer(expr);
                match op {
                    AstUnaryOp::Not => Ty::Scalar(DType::Bool),
                    AstUnaryOp::Neg => t,
                }
            }
            AstExpr::Call {
                callee,
                args,
                named_args,
                span,
            } => self.infer_call(callee, args, named_args, *span),
            AstExpr::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                self.using(cond);
                let c = self.infer(cond);
                self.unify(&c, &Ty::Scalar(DType::Bool), cond.span(), Why::Condition);
                let t = self.infer_block(then_block);
                match else_block {
                    Some(eb) => {
                        let f = self.infer_block(eb);
                        // Each branch's value, when both have one.
                        if then_block.tail.is_some() && eb.tail.is_some() {
                            let at = eb.tail.as_ref().map(|t| t.span()).unwrap_or(eb.span);
                            if self.different_records(&f, &t) {
                                return Ty::Unknown;
                            }
                            self.unify(&f, &t, at, Why::Branch);
                            self.join(&t, &f)
                        } else {
                            Ty::Unknown
                        }
                    }
                    None => Ty::Unknown,
                }
            }
            AstExpr::Block(b) => self.infer_block(b),
            AstExpr::Mask { body, .. } => self.infer_block(body),
            AstExpr::Index { base, indices, .. } => {
                self.using(base);
                for i in indices {
                    self.using(i);
                }
                let b = self.infer(base);
                for i in indices {
                    self.infer(i);
                }
                match self.resolve(&b, false) {
                    Ty::List(e) | Ty::Array(e, _) if indices.len() == 1 => *e,
                    Ty::Str if indices.len() == 1 => Ty::Str,
                    _ => Ty::Unknown,
                }
            }
            AstExpr::Cast { expr, ty, .. } => {
                self.infer(expr);
                self.ast_ty(ty)
            }
            AstExpr::StructLit {
                name,
                fields,
                field_spans,
                spread,
                span,
            } => {
                if let Some(s) = spread {
                    self.infer(s);
                }
                let resolved = super::resolve_brought_name(name, self.reg.module);
                let def = self.reg.module.struct_def(&resolved).cloned();
                let generic = self.reg.module.generic_struct_names.contains(&resolved);
                match def {
                    Some(def) if !generic => {
                        for (fname, fexpr) in fields {
                            let v = self.infer(fexpr);
                            if let Some((_, ft)) = def.iter().find(|(n, _)| n == fname) {
                                let ft = self.ty_from_ir(ft);
                                self.unify(&v, &ft, fexpr.span(), Why::Field);
                            }
                        }
                        // The record exists and is not generic, so its
                        // fields are judged here as lowering would (§4.2).
                        self.struct_literal_fields(
                            &resolved,
                            &def,
                            fields,
                            field_spans,
                            spread.is_some(),
                            *span,
                        );
                        Ty::Named(resolved)
                    }
                    _ => {
                        for (_, fexpr) in fields {
                            self.infer(fexpr);
                        }
                        Ty::Unknown
                    }
                }
            }
            AstExpr::FieldAccess { base, field, span } => {
                self.using(base);
                // `Color.Red`: a variant of a choice. `std.math.pi`: a
                // constant reached through a qualified spelling.
                if let AstExpr::Ident(id) = base.as_ref() {
                    if self.lookup_local(&id.name).is_none() {
                        let mangled_const =
                            format!("{}__{}", super::resolve_qualifier(&id.name), field);
                        if let Some(t) = self.const_types.get(&mangled_const) {
                            return t.clone();
                        }
                        let resolved = self.resolve_name(&id.name);
                        if let Some(variants) = self.reg.module.enum_def(&resolved) {
                            let variants = variants.clone();
                            return match variants.iter().position(|v| v == field) {
                                Some(_) => Ty::Named(resolved),
                                // Not a variant of this choice: lowering's
                                // judgment, and the same error (§4.2).
                                None => {
                                    self.errors.push(
                                        Diagnostic::error(format!(
                                            "`{1}` has no variant `{0}`",
                                            field, resolved
                                        ))
                                        .with_code("E0122")
                                        .with_primary_span(span.start.0, span.end.0, None),
                                    );
                                    Ty::Unknown
                                }
                            };
                        }
                    }
                }
                let b = self.infer(base);
                match self.resolve(&b, false) {
                    Ty::Named(n) => match self.reg.module.struct_def(&n) {
                        Some(def) if !self.reg.module.generic_struct_names.contains(&n) => {
                            match def.iter().find(|(f, _)| f == field).map(|(_, t)| t.clone()) {
                                Some(t) => self.ty_from_ir(&t),
                                // The access ends at the field name.
                                None => {
                                    let at = Span::new(
                                        span.end.0.saturating_sub(field.len() as u32),
                                        span.end.0,
                                    );
                                    self.errors.push(self.no_such_field(&n, field, def, at));
                                    Ty::Unknown
                                }
                            }
                        }
                        _ => Ty::Unknown,
                    },
                    _ => Ty::Unknown,
                }
            }
            AstExpr::When {
                scrutinee,
                arms,
                span,
            } => {
                self.using(scrutinee);
                let s = self.infer(scrutinee);
                // The variants a known choice's arms handle are judged
                // before the arms are walked (§4.2).
                self.when_exhaustive(arms, *span, &s);
                let mut result: Option<Ty> = None;
                for arm in arms {
                    self.scopes.push(HashMap::new());
                    self.bind_pattern(&arm.pattern, &s);
                    if let Some(g) = &arm.guard {
                        self.infer(g);
                    }
                    let t = self.infer(&arm.body);
                    self.scopes.pop();
                    match &result {
                        None => result = Some(t),
                        Some(r) => {
                            let r = r.clone();
                            if self.different_records(&t, &r) {
                                result = Some(Ty::Unknown);
                            } else {
                                self.unify(&t, &r, arm.body.span(), Why::Branch);
                                result = Some(self.join(&r, &t));
                            }
                        }
                    }
                }
                result.unwrap_or(Ty::Unknown)
            }
            AstExpr::Tuple { elements, .. } => {
                Ty::Tuple(elements.iter().map(|e| self.infer(e)).collect())
            }
            AstExpr::TupleIndex { base, index, .. } => {
                let b = self.infer(base);
                match self.resolve(&b, false) {
                    Ty::Tuple(ts) => ts.get(*index).cloned().unwrap_or(Ty::Unknown),
                    _ => Ty::Unknown,
                }
            }
            AstExpr::ArrayLit { elems, .. } => {
                let elem = self.fresh(Kind::Any);
                for el in elems {
                    let t = self.infer(el);
                    self.unify(&t, &elem, el.span(), Why::Element);
                }
                Ty::Array(Box::new(elem), elems.len())
            }
            AstExpr::Lambda { params, body, .. } => {
                self.scopes.push(HashMap::new());
                let ps: Vec<Ty> = params
                    .iter()
                    .map(|p| {
                        let t = self.ast_ty(&p.ty);
                        self.bind(&p.name.name, t.clone());
                        t
                    })
                    .collect();
                // A lambda's `return` (and `?`) returns from the lambda.
                let outer = self.ret.replace(Ty::Unknown);
                let outer_span = self.ret_span.take();
                let outer_try = std::mem::take(&mut self.try_errs);
                let r = self.infer(body);
                self.ret = outer;
                self.ret_span = outer_span;
                self.try_errs = outer_try;
                self.scopes.pop();
                Ty::Fn(ps, Box::new(r))
            }
            AstExpr::Await { expr, .. } => {
                let t = self.infer(expr);
                match self.resolve(&t, false) {
                    Ty::Task(inner) => *inner,
                    _ => Ty::Unknown,
                }
            }
            AstExpr::Try { expr, span } => {
                let t = self.infer(expr);
                match self.resolve(&t, false) {
                    Ty::Result(ok, e) => {
                        self.propagate(Some(&e), *span);
                        *ok
                    }
                    Ty::Option(inner) => {
                        self.propagate(None, *span);
                        *inner
                    }
                    _ => Ty::Unknown,
                }
            }
            AstExpr::NullCoal { expr, default, .. } => {
                let t = self.infer(expr);
                let d = self.infer(default);
                match self.resolve(&t, false) {
                    Ty::Result(ok, _) => {
                        self.unify(&d, &ok, default.span(), Why::Branch);
                        *ok
                    }
                    Ty::Option(inner) => {
                        self.unify(&d, &inner, default.span(), Why::Branch);
                        *inner
                    }
                    _ => Ty::Unknown,
                }
            }
            AstExpr::MethodCall {
                base,
                method,
                args,
                span,
            } => self.infer_method(base, method, args, *span),
            AstExpr::MapLiteral { entries, .. } => {
                let k = self.fresh(Kind::Any);
                let v = self.fresh(Kind::Any);
                for (ke, ve) in entries {
                    let kt = self.infer(ke);
                    let vt = self.infer(ve);
                    self.unify(&kt, &k, ke.span(), Why::Element);
                    self.unify(&vt, &v, ve.span(), Why::Element);
                }
                Ty::Map(Box::new(k), Box::new(v))
            }
            AstExpr::Ref { expr, .. }
            | AstExpr::RefMut { expr, .. }
            | AstExpr::Deref { expr, .. }
            | AstExpr::Move { expr, .. }
            | AstExpr::Splat { expr, .. } => self.infer(expr),
            AstExpr::Unsafe { body, .. } => self.infer(body),
            AstExpr::TryCatch {
                body,
                catch_param,
                catch_body,
                ..
            } => {
                // The catch receives what the body's `?`s carry.
                let err = self.fresh(Kind::Any);
                self.try_errs.push(err.clone());
                let bt = self.infer(body);
                self.try_errs.pop();
                // A body without `?` never reaches its catch (lowering
                // gives the parameter `str`).
                if let Ty::Var(_) = self.shallow(&err) {
                    self.unify(&err, &Ty::Str, catch_body.span(), Why::Assignment);
                }
                self.scopes.push(HashMap::new());
                self.bind(catch_param, err);
                let ct = self.infer(catch_body);
                self.scopes.pop();
                self.unify(&ct, &bt, catch_body.span(), Why::Branch);
                self.join(&bt, &ct)
            }
            AstExpr::Handle { expr, .. } => {
                self.infer(expr);
                Ty::Unknown
            }
            AstExpr::Raise { args, .. } | AstExpr::MacroCall { args, .. } => {
                for a in args {
                    self.infer(a);
                }
                Ty::Unknown
            }
        }
    }

    fn infer_ident(&mut self, id: &Ident) -> Ty {
        let name = &id.name;
        if let Some(t) = self.lookup_local(name) {
            return t;
        }
        if name == "none" {
            let e = self.fresh(Kind::Any);
            return Ty::Option(Box::new(e));
        }
        if let Some((_, t)) = self.reg.module.state_slot(name) {
            let t = t.clone();
            return self.ty_from_ir(&t);
        }
        let resolved = self.resolve_name(name);
        if let Some(t) = self.const_types.get(&resolved).cloned() {
            return t;
        }
        // The REPL's `-> Infer` wrappers and generic functions have no
        // first-class type; what they are used as is unmodelled.
        if self.reg.inferred_fns.contains(&resolved) || self.reg.generic_fns.contains_key(&resolved)
        {
            return self.fresh_opaque();
        }
        // A function is a value: its signature, for first-class use.
        if self.reg.fn_sigs.contains_key(&resolved) {
            return self.fn_ty(&resolved);
        }
        // Lowering's judgment (§4.2): the name is not a value the program
        // or a built-in defines.
        self.errors.push(self.unknown_name(name, id.span, None));
        Ty::Unknown
    }

    /// A call of `name`: a closure in scope, a polymorphic builtin, a
    /// function of the program, or a runtime builtin.
    fn infer_call(
        &mut self,
        callee: &Ident,
        args: &[AstExpr],
        named: &[(String, AstExpr)],
        span: Span,
    ) -> Ty {
        let name = &callee.name;
        for a in args.iter().chain(named.iter().map(|(_, e)| e)) {
            self.using(a);
        }
        // A closure held in a variable.
        if let Some(t) = self.lookup_local(name) {
            let arg_tys: Vec<Ty> = args.iter().map(|a| self.infer(a)).collect();
            for (_, e) in named {
                self.infer(e);
            }
            return match self.resolve(&t, false) {
                Ty::Fn(ps, r) => {
                    // Named arguments and defaults rearrange the positions:
                    // check only a plain positional call of the full arity.
                    if named.is_empty()
                        && !args.iter().any(|a| matches!(a, AstExpr::Splat { .. }))
                        && ps.len() == arg_tys.len()
                    {
                        for ((a, p), e) in arg_tys.iter().zip(ps.iter()).zip(args) {
                            self.unify(a, p, e.span(), Why::Argument);
                        }
                    } else {
                        for a in &arg_tys {
                            self.taint(a);
                        }
                    }
                    *r
                }
                // Not a function: a definite type called (§4.2). An unknown
                // one is unmodelled and agrees with everything.
                Ty::Unknown | Ty::Var(_) | Ty::Param(_) => {
                    for a in &arg_tys {
                        self.taint(a);
                    }
                    Ty::Unknown
                }
                // A builtin beats a local of the same name in call position:
                // lowering finds its dispatch before it ever looks at scopes,
                // so `val len = ...; len(line)` still lowers the `len`
                // built-in (known-issue #15). Not a judgment of the source.
                _ if self.known_call_name(name) => {
                    for a in &arg_tys {
                        self.taint(a);
                    }
                    Ty::Unknown
                }
                other => {
                    let tname = self.name(&other);
                    self.errors.push(
                        Diagnostic::error(format!(
                            "`{}` is not a function: it has type `{}`",
                            name, tname
                        ))
                        .with_code("E0118")
                        .with_primary_span(
                            span.start.0,
                            span.end.0,
                            Some("called here".into()),
                        ),
                    );
                    for a in &arg_tys {
                        self.taint(a);
                    }
                    Ty::Unknown
                }
            };
        }
        if let Some(t) = self.infer_builtin(name, args) {
            for (_, e) in named {
                self.infer(e);
            }
            return t;
        }
        let resolved = self.resolve_name(name);
        let arg_tys: Vec<Ty> = args.iter().map(|a| self.infer(a)).collect();
        for (_, e) in named {
            self.infer(e);
        }
        // A generic function: its parameters' types, with a fresh variable
        // for each type parameter, are checked against the arguments.
        if let Some(g) = self.reg.generic_fns.get(&resolved).cloned() {
            if g.type_params.is_empty() || g.is_const && g.type_params.is_empty() {
                // `const def` without type parameters: an ordinary signature.
            } else {
                return self.instantiate_call(&g, &arg_tys, args, named.is_empty());
            }
        }
        if let Some(ret) = self.reg.fn_sigs.get(&resolved).cloned() {
            let params = self.reg.fn_param_types.get(&resolved).cloned();
            // The count of arguments must fit the declared parameters (the
            // same judgment lowering makes, §4.2). Only a plain positional
            // call of a fully declared function; named arguments and `..`
            // splats are lowering's to judge.
            if named.is_empty() {
                if let Some(names) = self.reg.fn_param_names.get(&resolved) {
                    let has_splat = args.iter().any(|a| matches!(a, AstExpr::Splat { .. }));
                    let required = match self.reg.fn_defaults.get(&resolved) {
                        Some(defs) => defs.iter().filter(|d| d.is_none()).count(),
                        None => names.len(),
                    };
                    if !has_splat && (args.len() < required || args.len() > names.len()) {
                        self.errors.push(self.arity_diag(
                            &resolved,
                            name,
                            required,
                            names.len(),
                            args.len(),
                            span,
                        ));
                    }
                }
            }
            if let Some(params) = params {
                // Named arguments and defaults rearrange the positions:
                // check only a plain positional call of the full arity.
                if !(named.is_empty() && params.len() == arg_tys.len()) {
                    for a in &arg_tys {
                        self.taint(a);
                    }
                }
                if named.is_empty() && params.len() == arg_tys.len() {
                    let decl = super::CURRENT_FN_ASTS
                        .with(|m| m.borrow().get(&resolved).map(|f| f.params.clone()));
                    for (i, ((a, p), e)) in arg_tys.iter().zip(params.iter()).zip(args).enumerate()
                    {
                        let p = self.ty_from_ir(p);
                        let because = decl.as_ref().and_then(|ps| ps.get(i)).map(|param| {
                            let at = param.name.span.merge(param.ty.span());
                            (
                                at,
                                format!("parameter `{}` is declared here", param.name.name),
                            )
                        });
                        self.expect(e, a, &p, Why::Argument, because);
                    }
                }
            }
            return self.ty_from_ir(&ret);
        }
        // A runtime builtin takes its arguments as they are: nothing
        // constrains them, here or in inference after lowering.
        if let Some((_, ret)) = super::runtime_builtin(name) {
            return self.ty_from_ir(&ret);
        }
        // A built-in whose dispatch the checker does not model: what it
        // does with its arguments is lowering's to judge.
        if self.known_call_name(name) {
            for a in &arg_tys {
                self.taint(a);
            }
            return Ty::Unknown;
        }
        // The REPL's `-> Infer` wrappers have no signature yet, so the fall
        // through above does not see them; what the call returns is what the
        // checker solved from the callee's body.
        if self.reg.inferred_fns.contains(&resolved) {
            // The call's result is the return type solved from the
            // callee's body when the checker got there; a wrapper declared
            // after its caller is unmodelled until solved.
            return self
                .solved_returns
                .iter()
                .rev()
                .find(|(n, _)| *n == resolved)
                .map(|(_, t)| t.clone())
                .unwrap_or_else(|| self.fresh_opaque());
        }
        // Lowering's judgment (§4.2): the caller is not a value of the
        // program or a built-in; the error points from the name.
        self.errors
            .push(self.unknown_name(name, callee.span, Some((span, args))));
        for a in &arg_tys {
            self.taint(a);
        }
        Ty::Unknown
    }

    fn instantiate_call(
        &mut self,
        g: &AstFunction,
        arg_tys: &[Ty],
        args: &[AstExpr],
        positional: bool,
    ) -> Ty {
        let mut subs: HashMap<String, Ty> = HashMap::new();
        for p in &g.type_params {
            if let crate::parser::ast::AstGenericParam::Type(n, _, _) = p {
                let v = self.fresh_opaque();
                subs.insert(n.clone(), v);
            } else {
                return Ty::Unknown;
            }
        }
        if positional && g.params.len() == arg_tys.len() {
            for ((p, a), e) in g.params.iter().zip(arg_tys).zip(args) {
                if let Some(pt) = self.ast_ty_subst(&p.ty, &subs) {
                    self.unify(a, &pt, e.span(), Why::Argument);
                }
            }
        }
        self.ast_ty_subst(&g.return_ty, &subs)
            .unwrap_or(Ty::Unknown)
    }

    /// `t` with type parameters replaced by `subs`; `None` for a form not
    /// modelled (a generic record, a higher-kinded parameter).
    fn ast_ty_subst(&mut self, t: &AstType, subs: &HashMap<String, Ty>) -> Option<Ty> {
        Some(match t {
            AstType::Named(n, _) if subs.contains_key(n) => subs[n].clone(),
            AstType::List(e, _) => Ty::List(Box::new(self.ast_ty_subst(e, subs)?)),
            AstType::Option(e, _) => Ty::Option(Box::new(self.ast_ty_subst(e, subs)?)),
            AstType::Chan(e, _) => Ty::Chan(Box::new(self.ast_ty_subst(e, subs)?)),
            AstType::Map(k, v, _) => Ty::Map(
                Box::new(self.ast_ty_subst(k, subs)?),
                Box::new(self.ast_ty_subst(v, subs)?),
            ),
            AstType::Result(a, b, _) => Ty::Result(
                Box::new(self.ast_ty_subst(a, subs)?),
                Box::new(self.ast_ty_subst(b, subs)?),
            ),
            AstType::Tuple(ts, _) => Ty::Tuple(
                ts.iter()
                    .map(|t| self.ast_ty_subst(t, subs))
                    .collect::<Option<Vec<_>>>()?,
            ),
            AstType::Fn { params, ret, .. } => Ty::Fn(
                params
                    .iter()
                    .map(|t| self.ast_ty_subst(t, subs))
                    .collect::<Option<Vec<_>>>()?,
                Box::new(self.ast_ty_subst(ret, subs)?),
            ),
            AstType::Generic { .. } => return None,
            other => {
                if subs.keys().any(|k| ast_mentions(other, k)) {
                    return None;
                }
                match super::lower_type_with_structs(other, self.reg.module) {
                    Ok(IrType::Infer) | Err(_) => Ty::Unknown,
                    Ok(ir) => self.ty_from_ir(&ir),
                }
            }
        })
    }

    /// The builtins whose type depends on their arguments' types.
    fn infer_builtin(&mut self, name: &str, args: &[AstExpr]) -> Option<Ty> {
        self.infer_builtin_inner(name, args)
    }

    /// The element type of the channel `ch` (a channel of a fresh element
    /// type, when `ch`'s type is not known yet); `Unknown` for anything else.
    fn chan_elem(&mut self, ch: &Ty) -> Ty {
        match self.shallow(ch) {
            Ty::Chan(e) => *e,
            Ty::Var(_) => {
                let e = self.fresh(Kind::Any);
                let chan = Ty::Chan(Box::new(e.clone()));
                match self.unify_inner(ch, &chan) {
                    Ok(()) => e,
                    Err(_) => Ty::Unknown,
                }
            }
            _ => Ty::Unknown,
        }
    }

    fn infer_builtin_inner(&mut self, name: &str, args: &[AstExpr]) -> Option<Ty> {
        let i64t = Ty::Scalar(DType::I64);
        let boolt = Ty::Scalar(DType::Bool);
        let f64t = Ty::Scalar(DType::F64);
        // User definitions shadow builtins.
        if super::CURRENT_USER_FNS.with(|u| u.borrow().contains(name)) {
            return None;
        }
        let n = args.len();
        Some(match (name, n) {
            ("list", _) => {
                let e = self.fresh(Kind::Any);
                for a in args {
                    let t = self.infer(a);
                    self.unify(&t, &e, a.span(), Why::Element);
                }
                Ty::List(Box::new(e))
            }
            ("channel", 0 | 1) => {
                if let Some(cap) = args.first() {
                    let t = self.infer(cap);
                    self.unify(&t, &i64t, cap.span(), Why::Argument);
                }
                Ty::Chan(Box::new(self.fresh(Kind::Any)))
            }
            ("send" | "chan_send", 2) => {
                let ch = self.infer(&args[0]);
                let v = self.infer(&args[1]);
                let e = self.chan_elem(&ch);
                self.unify(&v, &e, args[1].span(), Why::Element);
                // Lowering's unit stand-in.
                i64t
            }
            ("recv" | "chan_recv", 1) => {
                let ch = self.infer(&args[0]);
                // `recv` on a task waits for its result.
                if let Ty::Task(e) = self.shallow(&ch) {
                    *e
                } else {
                    self.chan_elem(&ch)
                }
            }
            ("async_recv", 1) => {
                let ch = self.infer(&args[0]);
                Ty::Task(Box::new(self.chan_elem(&ch)))
            }
            ("par_map", 2) => {
                let l = self.infer(&args[0]);
                let f = self.infer(&args[1]);
                let e = match self.shallow(&l) {
                    Ty::List(e) => *e,
                    _ => {
                        self.taint(&l);
                        Ty::Unknown
                    }
                };
                let r = self.fresh(Kind::Any);
                self.unify(
                    &f,
                    &Ty::Fn(vec![e], Box::new(r.clone())),
                    args[1].span(),
                    Why::Argument,
                );
                Ty::List(Box::new(r))
            }
            ("chan_try_recv" | "try_recv", 1) => {
                let ch = self.infer(&args[0]);
                Ty::Option(Box::new(self.chan_elem(&ch)))
            }
            ("recv_timeout", 2) => {
                let ch = self.infer(&args[0]);
                let ms = self.infer(&args[1]);
                self.unify(&ms, &i64t, args[1].span(), Why::Argument);
                Ty::Option(Box::new(self.chan_elem(&ch)))
            }
            ("push" | "list_push", 2) => {
                let l = self.infer(&args[0]);
                let v = self.infer(&args[1]);
                if let Ty::List(e) = self.shallow(&l) {
                    // A list holds values of one type. (Lowering accepted
                    // an unannotated list of mixed values, typed by its
                    // first, and later code read them as that type.)
                    if let Err((f, x)) = self.unify_inner(&v, &e) {
                        self.mismatch(&f, &x, args[1].span(), Why::Element);
                    }
                }
                i64t
            }
            ("list_get", 2) => {
                let l = self.infer(&args[0]);
                let i = self.infer(&args[1]);
                self.unify(&i, &i64t, args[1].span(), Why::Argument);
                match self.resolve(&l, false) {
                    Ty::List(e) => *e,
                    _ => Ty::Unknown,
                }
            }
            ("list_set", 3) => {
                let l = self.infer(&args[0]);
                let i = self.infer(&args[1]);
                let v = self.infer(&args[2]);
                self.unify(&i, &i64t, args[1].span(), Why::Argument);
                if let Ty::List(e) = self.resolve(&l, false) {
                    self.unify(&v, &e, args[2].span(), Why::Element);
                }
                Ty::Unknown
            }
            ("list_len" | "len" | "str_len", 1) => {
                self.infer(&args[0]);
                i64t
            }
            ("some", 1) => {
                let t = self.infer(&args[0]);
                Ty::Option(Box::new(t))
            }
            ("none", 0) => {
                let e = self.fresh(Kind::Any);
                Ty::Option(Box::new(e))
            }
            ("ok", 1) => {
                let t = self.infer(&args[0]);
                let e = self.fresh(Kind::Any);
                Ty::Result(Box::new(t), Box::new(e))
            }
            ("err", 1) => {
                let t = self.infer(&args[0]);
                let o = self.fresh(Kind::Any);
                Ty::Result(Box::new(o), Box::new(t))
            }
            ("unwrap", 1) => {
                let t = self.infer(&args[0]);
                match self.resolve(&t, false) {
                    Ty::Option(e) => *e,
                    Ty::Result(o, _) => *o,
                    _ => Ty::Unknown,
                }
            }
            ("is_some" | "is_none" | "is_ok" | "is_err", 1) => {
                self.infer(&args[0]);
                boolt
            }
            ("concat", 2) => {
                for a in args {
                    let t = self.infer(a);
                    self.unify(&t, &Ty::Str, a.span(), Why::Argument);
                }
                Ty::Str
            }
            // String functions, as lowering types them.
            ("slice", 3) | ("str_replace" | "replace", 3) | ("repeat", 2) => {
                let s = self.infer(&args[0]);
                self.unify(&s, &Ty::Str, args[0].span(), Why::Argument);
                for a in &args[1..] {
                    self.infer(a);
                }
                Ty::Str
            }
            ("to_upper" | "to_lower" | "trim" | "str_reverse", 1) => {
                let s = self.infer(&args[0]);
                self.unify(&s, &Ty::Str, args[0].span(), Why::Argument);
                Ty::Str
            }
            ("char_at", 2) | ("str_index" | "str_count", 2) => {
                let s = self.infer(&args[0]);
                self.unify(&s, &Ty::Str, args[0].span(), Why::Argument);
                let i = self.infer(&args[1]);
                if name == "str_count" {
                    self.unify(&i, &Ty::Str, args[1].span(), Why::Argument);
                    i64t
                } else {
                    self.unify(&i, &i64t, args[1].span(), Why::Argument);
                    if name == "char_at" {
                        Ty::Str
                    } else {
                        i64t
                    }
                }
            }
            ("starts_with" | "ends_with", 2) | ("find", 2) | ("split", 2) => {
                for a in args {
                    let t = self.infer(a);
                    self.unify(&t, &Ty::Str, a.span(), Why::Argument);
                }
                match name {
                    "find" => Ty::Option(Box::new(i64t)),
                    "split" => Ty::List(Box::new(Ty::Str)),
                    _ => boolt,
                }
            }
            ("join", 2) => {
                let l = self.infer(&args[0]);
                self.unify(
                    &l,
                    &Ty::List(Box::new(Ty::Str)),
                    args[0].span(),
                    Why::Argument,
                );
                let d = self.infer(&args[1]);
                self.unify(&d, &Ty::Str, args[1].span(), Why::Argument);
                Ty::Str
            }
            ("str_chars", 1) => {
                let s = self.infer(&args[0]);
                self.unify(&s, &Ty::Str, args[0].span(), Why::Argument);
                Ty::List(Box::new(Ty::Str))
            }
            ("parse_i64" | "parse_f64", 1) => {
                let s = self.infer(&args[0]);
                self.unify(&s, &Ty::Str, args[0].span(), Why::Argument);
                Ty::Option(Box::new(if name == "parse_i64" { i64t } else { f64t }))
            }
            // List and map functions, threading the element types.
            ("list_pop", 1) => {
                let l = self.infer(&args[0]);
                match self.shallow(&l) {
                    Ty::List(e) => *e,
                    _ => {
                        self.taint(&l);
                        Ty::Unknown
                    }
                }
            }
            ("list_contains" | "list_index_of", 2) => {
                let l = self.infer(&args[0]);
                let v = self.infer(&args[1]);
                if let Ty::List(e) = self.shallow(&l) {
                    self.unify(&v, &e, args[1].span(), Why::Argument);
                } else {
                    self.taint(&l);
                    self.taint(&v);
                }
                if name == "list_contains" {
                    boolt
                } else {
                    i64t
                }
            }
            ("map_keys" | "map_values" | "map_len", 1) => {
                let m = self.infer(&args[0]);
                match (name, self.shallow(&m)) {
                    ("map_len", _) => i64t,
                    ("map_keys", Ty::Map(k, _)) => Ty::List(k),
                    ("map_values", Ty::Map(_, v)) => Ty::List(v),
                    _ => {
                        self.taint(&m);
                        Ty::Unknown
                    }
                }
            }
            ("map_contains" | "map_remove", 2) => {
                let m = self.infer(&args[0]);
                let k = self.infer(&args[1]);
                if let Ty::Map(kt, _) = self.shallow(&m) {
                    self.unify(&k, &kt, args[1].span(), Why::Argument);
                } else {
                    self.taint(&m);
                    self.taint(&k);
                }
                if name == "map_contains" {
                    boolt
                } else {
                    i64t
                }
            }
            // Any value at all, which it does not constrain.
            ("json_stringify", 1) => {
                self.infer(&args[0]);
                Ty::Str
            }
            ("to_str", 1) => {
                self.infer(&args[0]);
                Ty::Str
            }
            ("to_f64", 1) => {
                self.infer(&args[0]);
                f64t
            }
            ("to_i64", 1) => {
                self.infer(&args[0]);
                i64t
            }
            ("println" | "print" | "eprintln" | "eprint", _) => {
                for a in args {
                    self.infer(a);
                }
                i64t
            }
            ("abs", 1) => self.infer(&args[0]),
            ("min" | "max", 2) => {
                let a = self.infer(&args[0]);
                let b = self.infer(&args[1]);
                self.unify(&b, &a, args[1].span(), Why::Argument);
                a
            }
            ("sqrt" | "sin" | "cos" | "tan" | "exp" | "log" | "floor" | "ceil", 1) => {
                let a = self.infer(&args[0]);
                self.unify(&a, &f64t, args[0].span(), Why::Argument);
                f64t
            }
            ("map", 0) => {
                let k = self.fresh(Kind::Any);
                let v = self.fresh(Kind::Any);
                Ty::Map(Box::new(k), Box::new(v))
            }
            ("map_get", 2) => {
                let m = self.infer(&args[0]);
                let k = self.infer(&args[1]);
                match self.resolve(&m, false) {
                    Ty::Map(mk, mv) => {
                        self.unify(&k, &mk, args[1].span(), Why::Argument);
                        Ty::Option(mv)
                    }
                    _ => Ty::Unknown,
                }
            }
            ("map_set", 3) => {
                let m = self.infer(&args[0]);
                let k = self.infer(&args[1]);
                let v = self.infer(&args[2]);
                if let Ty::Map(mk, mv) = self.resolve(&m, false) {
                    self.unify(&k, &mk, args[1].span(), Why::Argument);
                    self.unify(&v, &mv, args[2].span(), Why::Argument);
                }
                Ty::Unknown
            }
            ("assert", _) | ("panic", _) => {
                for a in args {
                    self.infer(a);
                }
                if name == "assert" {
                    if let Some(c) = args.first() {
                        // The condition was typed above.
                        let _ = c;
                    }
                }
                i64t
            }
            _ => return None,
        })
    }

    fn infer_method(&mut self, base: &AstExpr, method: &str, args: &[AstExpr], span: Span) -> Ty {
        self.using(base);
        for a in args {
            self.using(a);
        }
        if let AstExpr::Ident(id) = base {
            if self.lookup_local(&id.name).is_none()
                && self.reg.module.state_slot(&id.name).is_none()
            {
                // `Shape.Circle(r)`: a variant with a payload.
                let resolved = self.resolve_name(&id.name);
                if let (Some(variants), Some(fields)) = (
                    self.reg.module.enum_def(&resolved).cloned(),
                    self.reg.module.enum_variant_fields(&resolved).cloned(),
                ) {
                    if let Some(i) = variants.iter().position(|v| v == method) {
                        let ft = fields.get(i).cloned().unwrap_or_default();
                        let arg_tys: Vec<Ty> = args.iter().map(|a| self.infer(a)).collect();
                        if ft.len() == arg_tys.len() {
                            for ((a, f), e) in arg_tys.iter().zip(ft.iter()).zip(args) {
                                let f = self.ty_from_ir(f);
                                self.unify(a, &f, e.span(), Why::Argument);
                            }
                        }
                        return Ty::Named(resolved);
                    }
                }
                // `module.function(args)`.
                if self.reg.module.struct_def(&resolved).is_none()
                    && self.reg.module.enum_def(&resolved).is_none()
                {
                    let q = super::resolve_qualifier(&id.name);
                    let mangled = format!("{q}__{method}");
                    if self.reg.fn_sigs.contains_key(&mangled)
                        || self.reg.generic_fns.contains_key(&mangled)
                    {
                        let callee = Ident {
                            name: mangled,
                            span,
                        };
                        return self.infer_call(&callee, args, &[], span);
                    }
                    for a in args {
                        let t = self.infer(a);
                        self.taint(&t);
                    }
                    return Ty::Unknown;
                }
            }
        }
        let b = self.infer(base);
        let arg_tys: Vec<Ty> = args.iter().map(|a| self.infer(a)).collect();
        match self.resolve(&b, false) {
            Ty::Named(n) => {
                // An inherent method, `Type__method`, its first parameter `self`.
                let inherent = format!("{n}__{method}");
                if let Some(ret) = self.reg.fn_sigs.get(&inherent).cloned() {
                    // The count of arguments must fit the declared
                    // parameters, `self` included (§4.2).
                    let callee = method_display(&inherent, method);
                    self.method_arity(&inherent, &callee, args.len(), span);
                    let params = self.params_of(&inherent);
                    if params.len() == arg_tys.len() + 1 {
                        for ((a, p), e) in arg_tys.iter().zip(params.iter().skip(1)).zip(args) {
                            self.unify(a, p, e.span(), Why::Argument);
                        }
                    }
                    return self.ty_from_ir(&ret);
                }
                // A trait method implemented for this record.
                let implemented = self.reg.trait_dispatch.get(method).and_then(|impls| {
                    impls
                        .iter()
                        .find(|(t, _)| matches!(t, IrType::Struct { name, .. } | IrType::Enum { name, .. } if *name == n))
                        .map(|(_, mangled)| mangled.clone())
                });
                if let Some(ret) = implemented
                    .as_ref()
                    .and_then(|m| self.reg.fn_sigs.get(m).cloned())
                {
                    if let Some(mangled) = &implemented {
                        // The same judgment on the implemented method,
                        // `self` included (§4.2).
                        let callee = trait_method_display(mangled, method);
                        self.method_arity(mangled, &callee, args.len(), span);
                        let params = self.params_of(mangled);
                        if params.len() == arg_tys.len() + 1 {
                            for ((a, p), e) in arg_tys.iter().zip(params.iter().skip(1)).zip(args) {
                                self.unify(a, p, e.span(), Why::Argument);
                            }
                        }
                    }
                    return self.ty_from_ir(&ret);
                }
                for a in &arg_tys {
                    self.taint(a);
                }
                Ty::Unknown
            }
            resolved => {
                if let Some(t) = self.builtin_method(&resolved, method, &arg_tys, args) {
                    return t;
                }
                // Closure combinators, extension methods: lowering's to type.
                self.taint(&b);
                for a in &arg_tys {
                    self.taint(a);
                }
                Ty::Unknown
            }
        }
    }

    /// Lowering's argument-count judgment for a call of an impl method
    /// (`Type__method` or `Trait__Type__method`, `self` first in the
    /// registries): a method whose parameter names are known, of a plain
    /// positional call. The message reads the parameters without `self`,
    /// as the call sees them.
    fn method_arity(&mut self, mangled: &str, callee: &str, given: usize, span: Span) {
        let Some(names) = self.reg.method_param_names.get(mangled) else {
            return;
        };
        let required = match self.reg.method_param_defaults.get(mangled) {
            Some(defs) => defs.iter().filter(|d| d.is_none()).count(),
            None => names.len(),
        };
        if given + 1 < required || given + 1 > names.len() {
            self.errors.push(self.arity_diag(
                mangled,
                callee,
                required.saturating_sub(1),
                names.len().saturating_sub(1),
                given,
                span,
            ));
        }
    }

    /// The methods lowering gives lists, options, results, strings, maps
    /// and channels, with the types it gives them; `None` for others.
    fn builtin_method(
        &mut self,
        base: &Ty,
        method: &str,
        arg_tys: &[Ty],
        args: &[AstExpr],
    ) -> Option<Ty> {
        let i64t = Ty::Scalar(DType::I64);
        let boolt = Ty::Scalar(DType::Bool);
        let arg = |i: usize| args.get(i).map(|a| a.span()).unwrap_or(Span::at(0));
        Some(match (base, method, arg_tys.len()) {
            (Ty::List(_), "len", 0) => i64t,
            // The closure combinators: the closure takes the elements.
            (Ty::List(e), "map", 1) => {
                let r = self.fresh(Kind::Any);
                self.unify(
                    &arg_tys[0],
                    &Ty::Fn(vec![(**e).clone()], Box::new(r.clone())),
                    arg(0),
                    Why::Argument,
                );
                Ty::List(Box::new(r))
            }
            (Ty::List(e), "filter" | "any" | "all", 1) => {
                self.unify(
                    &arg_tys[0],
                    &Ty::Fn(vec![(**e).clone()], Box::new(boolt.clone())),
                    arg(0),
                    Why::Argument,
                );
                if method == "filter" {
                    Ty::List(e.clone())
                } else {
                    boolt
                }
            }
            (Ty::List(e), "fold", 2) => {
                let acc = arg_tys[0].clone();
                self.unify(
                    &arg_tys[1],
                    &Ty::Fn(vec![acc.clone(), (**e).clone()], Box::new(acc.clone())),
                    arg(1),
                    Why::Argument,
                );
                acc
            }
            (Ty::Option(e), "map", 1) => {
                let r = self.fresh(Kind::Any);
                self.unify(
                    &arg_tys[0],
                    &Ty::Fn(vec![(**e).clone()], Box::new(r.clone())),
                    arg(0),
                    Why::Argument,
                );
                Ty::Option(Box::new(r))
            }
            (Ty::List(e), "push", 1) => {
                self.unify(&arg_tys[0], e, arg(0), Why::Element);
                i64t
            }
            (Ty::List(e), "pop", 0) => (**e).clone(),
            (Ty::List(e), "get", 1) => {
                self.unify(&arg_tys[0], &i64t, arg(0), Why::Argument);
                (**e).clone()
            }
            (Ty::List(e), "set", 2) => {
                self.unify(&arg_tys[0], &i64t, arg(0), Why::Argument);
                self.unify(&arg_tys[1], e, arg(1), Why::Element);
                i64t
            }
            (Ty::Option(_), "is_some" | "is_none", 0) => boolt,
            (Ty::Option(e), "unwrap", 0) => (**e).clone(),
            (Ty::Option(e), "unwrap_or", 1) => {
                self.unify(&arg_tys[0], e, arg(0), Why::Argument);
                (**e).clone()
            }
            (Ty::Result(..), "is_ok" | "is_err", 0) => boolt,
            (Ty::Result(o, _), "unwrap", 0) => (**o).clone(),
            (Ty::Result(_, e), "unwrap_err", 0) => (**e).clone(),
            (Ty::Result(o, _), "unwrap_or", 1) => {
                self.unify(&arg_tys[0], o, arg(0), Why::Argument);
                (**o).clone()
            }
            (Ty::Str, "len" | "index", _) => i64t,
            (Ty::Str, "to_upper" | "to_lower" | "trim" | "slice" | "replace" | "repeat", _) => {
                Ty::Str
            }
            (Ty::Str, "contains" | "starts_with" | "ends_with" | "is_empty", _) => boolt,
            (Ty::Str, "find", _) => Ty::Option(Box::new(i64t)),
            (Ty::Str, "split", _) => Ty::List(Box::new(Ty::Str)),
            (Ty::Map(..), "len", 0) => i64t,
            (Ty::Map(k, v), "get", 1) => {
                self.unify(&arg_tys[0], k, arg(0), Why::Argument);
                Ty::Option(v.clone())
            }
            (Ty::Map(k, v), "set", 2) => {
                self.unify(&arg_tys[0], k, arg(0), Why::Argument);
                self.unify(&arg_tys[1], v, arg(1), Why::Element);
                i64t
            }
            (Ty::Map(k, _), "contains", 1) => {
                self.unify(&arg_tys[0], k, arg(0), Why::Argument);
                boolt
            }
            (Ty::Chan(e), "send", 1) => {
                self.unify(&arg_tys[0], e, arg(0), Why::Element);
                i64t
            }
            (Ty::Chan(e), "recv", 0) => (**e).clone(),
            _ => return None,
        })
    }

    // ── Patterns ───────────────────────────────────────────────────────────

    fn bind_pattern(&mut self, p: &AstWhenPattern, scrutinee: &Ty) {
        let s = self.resolve(scrutinee, false);
        match p {
            AstWhenPattern::OptionSome { binding: Some(b) } => {
                let t = match s {
                    Ty::Option(e) => *e,
                    _ => Ty::Unknown,
                };
                self.bind(b, t);
            }
            AstWhenPattern::ResultOk { binding: Some(b) } => {
                let t = match s {
                    Ty::Result(o, _) => *o,
                    _ => Ty::Unknown,
                };
                self.bind(b, t);
            }
            AstWhenPattern::ResultErr { binding: Some(b) } => {
                let t = match s {
                    Ty::Result(_, e) => *e,
                    _ => Ty::Unknown,
                };
                self.bind(b, t);
            }
            AstWhenPattern::EnumVariant {
                enum_name,
                variant_name,
                bindings,
            } => {
                // A bare name with no enum is a binding of the whole value.
                if enum_name.is_empty() && bindings.is_empty() {
                    if let Ty::Named(n) = &s {
                        if self
                            .reg
                            .module
                            .enum_def(n)
                            .is_some_and(|vs| vs.iter().any(|v| v == variant_name))
                        {
                            return;
                        }
                    }
                    self.bind(variant_name, s.clone());
                    return;
                }
                let fields = match &s {
                    Ty::Named(n) => self.reg.module.enum_def(n).cloned().and_then(|vs| {
                        let i = vs.iter().position(|v| v == variant_name)?;
                        self.reg
                            .module
                            .enum_variant_fields(n)
                            .and_then(|fs| fs.get(i).cloned())
                    }),
                    _ => None,
                };
                for (i, b) in bindings.iter().enumerate() {
                    let t = match fields.as_ref().and_then(|fs| fs.get(i)).cloned() {
                        Some(f) => self.ty_from_ir(&f),
                        None => Ty::Unknown,
                    };
                    self.bind(b, t);
                }
            }
            AstWhenPattern::Tuple(subs) => {
                let parts = match s {
                    Ty::Tuple(ts) if ts.len() == subs.len() => ts,
                    _ => vec![Ty::Unknown; subs.len()],
                };
                for (sp, t) in subs.iter().zip(parts) {
                    self.bind_pattern(sp, &t);
                }
            }
            AstWhenPattern::Binding { name, pattern, .. } => {
                self.bind(name, s.clone());
                self.bind_pattern(pattern, &s);
            }
            AstWhenPattern::Or(alts) => {
                for a in alts {
                    self.bind_pattern(a, &s);
                }
            }
            AstWhenPattern::Struct { fields, .. } => {
                let def = match &s {
                    Ty::Named(n) => self.reg.module.struct_def(n).cloned(),
                    _ => None,
                };
                for (fname, fp) in fields {
                    let t = match def
                        .as_ref()
                        .and_then(|d| d.iter().find(|(n, _)| n == fname))
                        .map(|(_, t)| t.clone())
                    {
                        Some(ft) => self.ty_from_ir(&ft),
                        None => Ty::Unknown,
                    };
                    self.bind_pattern(fp, &t);
                }
            }
            AstWhenPattern::Slice { prefix, rest } => {
                let elem = match &s {
                    Ty::List(e) | Ty::Array(e, _) => (**e).clone(),
                    _ => Ty::Unknown,
                };
                for sp in prefix {
                    self.bind_pattern(sp, &elem);
                }
                if let Some(r) = rest {
                    let t = match &s {
                        Ty::List(_) => s.clone(),
                        _ => Ty::Unknown,
                    };
                    self.bind(r, t);
                }
            }
            _ => {}
        }
    }
}

/// `Type.method` from a mangled method name, as the call reads it: the
/// type's own name without the mangle (`Box__i64` reads `Box`).
fn method_display(mangled: &str, method: &str) -> String {
    let shown = mangled.split("__").next().unwrap_or(mangled);
    format!("{shown}.{method}")
}

/// The same, for a trait-impl mangle (`Trait__Type__method`): the type
/// after the trait, which is what the call is written against.
fn trait_method_display(mangled: &str, method: &str) -> String {
    let mut parts = mangled.split("__");
    let _trait = parts.next().unwrap_or(mangled);
    let shown = parts.next().unwrap_or(mangled);
    format!("{shown}.{method}")
}

fn is_int(d: DType) -> bool {
    matches!(
        d,
        DType::I64 | DType::I32 | DType::I8 | DType::U8 | DType::U32 | DType::U64 | DType::USize
    )
}

fn is_comparison(op: AstBinOp) -> bool {
    matches!(
        op,
        AstBinOp::CmpEq
            | AstBinOp::CmpNe
            | AstBinOp::CmpLt
            | AstBinOp::CmpLe
            | AstBinOp::CmpGt
            | AstBinOp::CmpGe
    )
}

/// Monomorphised and generic record names (`Box__i64`, `Box`) agree with
/// each other: instantiation is lowering's.
fn loose_named(a: &str, b: &str) -> bool {
    let base = |s: &str| s.split("__").next().unwrap_or(s).to_string();
    a.contains("__") && b.contains("__") && base(a) == base(b)
        || a.starts_with(&format!("{b}__"))
        || b.starts_with(&format!("{a}__"))
}

fn returns_value(t: &AstType) -> bool {
    !matches!(t, AstType::Named(n, _) if n == "Infer" || n == "unit" || n == "()")
        && !matches!(t, AstType::Tuple(ts, _) if ts.is_empty())
}

fn expr_mentions(e: &crate::parser::ast::AstExpr, name: &str) -> bool {
    use crate::parser::ast::AstExpr;
    match e {
        AstExpr::Ident(id) => id.name == name,
        AstExpr::BinOp { lhs, rhs, .. } => expr_mentions(lhs, name) || expr_mentions(rhs, name),
        AstExpr::UnaryOp { expr, .. } => expr_mentions(expr, name),
        _ => false,
    }
}

fn ast_mentions(t: &AstType, name: &str) -> bool {
    match t {
        AstType::Named(n, _) => n == name,
        AstType::Generic { name: n, args, .. } => {
            n == name || args.iter().any(|a| ast_mentions(a, name))
        }
        AstType::Tuple(ts, _) => ts.iter().any(|a| ast_mentions(a, name)),
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
        | AstType::Slice(e, _) => ast_mentions(e, name),
        AstType::Array { elem, len_expr, .. } => {
            ast_mentions(elem, name) || len_expr.as_ref().is_some_and(|e| expr_mentions(e, name))
        }
        AstType::Result(a, b, _) | AstType::Map(a, b, _) => {
            ast_mentions(a, name) || ast_mentions(b, name)
        }
        AstType::Fn { params, ret, .. } => {
            params.iter().any(|a| ast_mentions(a, name)) || ast_mentions(ret, name)
        }
        _ => false,
    }
}

// ── Instrumentation (milestone 1) ─────────────────────────────────────────

thread_local! {
    /// The report of the compile in progress, taken after the passes.
    static REPORT: std::cell::RefCell<Option<TypeckReport>> = const { std::cell::RefCell::new(None) };
}

/// What the checker solved of an expression's type, for lowering: the
/// parts it knows, the parts it leaves to lowering (`Open`: a type it does
/// not model touches them), and a generic body's type parameters (`Param`),
/// which lowering substitutes per instance. Normalized: a type with every
/// part known is `Known`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Hint {
    Known(IrType),
    Open,
    Param(String),
    List(Box<Hint>),
    Option(Box<Hint>),
    Result(Box<Hint>, Box<Hint>),
    Map(Box<Hint>, Box<Hint>),
    Chan(Box<Hint>),
    Task(Box<Hint>),
    Tuple(Vec<Hint>),
    Array(Box<Hint>, usize),
    Fn(Vec<Hint>, Box<Hint>),
}

impl Hint {
    /// The whole type, when every part of it is known.
    pub(crate) fn known(&self) -> Option<IrType> {
        Some(match self {
            Hint::Known(t) => t.clone(),
            Hint::Open | Hint::Param(_) => return None,
            Hint::List(e) => IrType::List(Box::new(e.known()?)),
            Hint::Option(e) => IrType::Option(Box::new(e.known()?)),
            Hint::Result(o, e) => IrType::ResultType(Box::new(o.known()?), Box::new(e.known()?)),
            Hint::Map(k, v) => IrType::Map(Box::new(k.known()?), Box::new(v.known()?)),
            Hint::Chan(e) => IrType::Chan(Box::new(e.known()?)),
            Hint::Task(e) => IrType::Task(Box::new(e.known()?)),
            Hint::Tuple(ts) => IrType::Tuple(ts.iter().map(Hint::known).collect::<Option<_>>()?),
            Hint::Array(e, n) => IrType::Array {
                elem: Box::new(e.known()?),
                len: *n,
            },
            Hint::Fn(ps, r) => IrType::Fn {
                params: ps.iter().map(Hint::known).collect::<Option<_>>()?,
                ret: Box::new(r.known()?),
            },
        })
    }

    /// Part `i`, when known: the element of a list, option, channel or task
    /// (0); a result's success (0) and error (1) sides; a map's key (0) and
    /// value (1).
    pub(crate) fn part(&self, i: usize) -> Option<IrType> {
        match self {
            Hint::Known(t) => match (t, i) {
                (IrType::List(e) | IrType::Option(e) | IrType::Chan(e) | IrType::Task(e), 0) => {
                    Some((**e).clone())
                }
                (IrType::ResultType(a, _) | IrType::Map(a, _), 0) => Some((**a).clone()),
                (IrType::ResultType(_, b) | IrType::Map(_, b), 1) => Some((**b).clone()),
                _ => None,
            },
            Hint::List(e) | Hint::Option(e) | Hint::Chan(e) | Hint::Task(e) if i == 0 => e.known(),
            Hint::Result(a, b) | Hint::Map(a, b) => match i {
                0 => a.known(),
                1 => b.known(),
                _ => None,
            },
            _ => None,
        }
    }

    /// With each type parameter replaced by `subs`' type for it (open when
    /// `subs` has none).
    pub(crate) fn instantiate(&self, subs: &HashMap<String, IrType>) -> Hint {
        let sub = |h: &Hint| Box::new(h.instantiate(subs));
        let h = match self {
            Hint::Param(n) => return subs.get(n).cloned().map_or(Hint::Open, Hint::Known),
            Hint::Known(_) | Hint::Open => return self.clone(),
            Hint::List(e) => Hint::List(sub(e)),
            Hint::Option(e) => Hint::Option(sub(e)),
            Hint::Result(a, b) => Hint::Result(sub(a), sub(b)),
            Hint::Map(a, b) => Hint::Map(sub(a), sub(b)),
            Hint::Chan(e) => Hint::Chan(sub(e)),
            Hint::Task(e) => Hint::Task(sub(e)),
            Hint::Tuple(ts) => Hint::Tuple(ts.iter().map(|t| t.instantiate(subs)).collect()),
            Hint::Array(e, n) => Hint::Array(sub(e), *n),
            Hint::Fn(ps, r) => Hint::Fn(ps.iter().map(|t| t.instantiate(subs)).collect(), sub(r)),
        };
        h.normalized()
    }

    fn normalized(self) -> Hint {
        match self.known() {
            Some(t) => Hint::Known(t),
            None => self,
        }
    }
}

/// `IRIS_TYPECK`: `report` prints what the checker finds, `compare` also
/// checks its types against the compiled module's. `off`: the checker does
/// not run.
pub(crate) fn mode() -> Option<String> {
    std::env::var("IRIS_TYPECK")
        .ok()
        .filter(|m| m == "report" || m == "compare")
}

/// Every value lowering makes is typed when it is made -- `IrType` has no
/// "not yet known" -- but lowering could still push a value and record no
/// type for it: its own defect, an internal error.
pub(crate) fn check_typed(module: &IrModule) -> Result<(), crate::error::LowerError> {
    for func in module.functions() {
        for block in func.blocks() {
            for instr in &block.instrs {
                let Some(result) = instr.result() else {
                    continue;
                };
                if func.value_type(result).is_none() {
                    let mut d = Diagnostic::error(format!(
                        "internal compiler error: lowering left a value in `{}` without a type",
                        func.name
                    ))
                    .with_code("E0900");
                    if let Some(byte) = func.span_table.get_for_val(result).filter(|b| *b > 0) {
                        d = d.with_primary_span(byte, byte + 1, None);
                    }
                    return Err(crate::error::LowerError::rich("E0900", d));
                }
            }
        }
    }
    Ok(())
}

/// Whether the checker runs: always, unless `IRIS_TYPECK=off`.
pub(crate) fn enabled() -> bool {
    std::env::var("IRIS_TYPECK").map_or(true, |m| m != "off")
}

/// The checker's errors as lowering errors, to report when lowering
/// accepted the program.
pub(crate) fn take_errors() -> Vec<crate::error::LowerError> {
    // `report` and `compare` observe: the errors are printed after the
    // passes instead, so a corpus run sees every program's findings.
    if mode().is_some() {
        return Vec::new();
    }
    REPORT.with(|r| {
        let mut slot = r.borrow_mut();
        let Some(report) = slot.as_mut() else {
            return Vec::new();
        };
        let errors = std::mem::take(&mut report.errors);
        // Keep the report for the instrumentation, which counts them.
        report.reported = errors.len();
        if std::env::var("IRIS_TYPECK_ORIGIN").is_ok() {
            for d in &errors {
                eprintln!(
                    "typeck-reported: [{}] at byte {:?}",
                    d.code.as_deref().unwrap_or("?"),
                    d.primary_span()
                );
            }
        }
        errors
            .into_iter()
            .map(|d| {
                let code: &'static str = match d.code.as_deref() {
                    Some("E0100") => "E0100",
                    Some("E0101") => "E0101",
                    Some("E0109") => "E0109",
                    Some("E0110") => "E0110",
                    Some("E0113") => "E0113",
                    Some("E0114") => "E0114",
                    Some("E0118") => "E0118",
                    Some("E0122") => "E0122",
                    Some("E0207") => "E0207",
                    Some("E0208") => "E0208",
                    Some("E0209") => "E0209",
                    Some("E0210") => "E0210",
                    _ => "E0202",
                };
                crate::error::LowerError::rich(code, d)
            })
            .collect()
    })
}

/// Store the checker's report for error reporting. Its types and hints are
/// already in the HIR (`HirModule::absorb`), which is the only thing lowering
/// reads them from.
pub(crate) fn publish(report: TypeckReport) {
    REPORT.with(|r| *r.borrow_mut() = Some(report));
}

/// What the checker solved of the type of the expression at `span` (see
/// `Hint`): an empty `list()`'s elements, the other side of an `ok` or
/// `err`, a literal's width -- what lowering cannot know alone. Read from
/// the HIR: the checker's report is absorbed there before lowering starts.
pub(crate) fn hint(span: Span) -> Option<Hint> {
    crate::hir::hint(span)
}

/// The return type the checker solved for `name`, declared `-> Infer`.
pub(crate) fn solved_return(name: &str) -> Option<IrType> {
    crate::hir::return_type(name)
}

/// Forget the previous compile's HIR when a new one starts.
pub(crate) fn clear_hints() {
    crate::hir::clear();
}

fn position(module: &IrModule, byte: u32) -> String {
    match module.source.as_deref().and_then(|s| {
        s.locate(byte)
            .map(|(f, l, c)| (s.files[f].name.clone(), l, c))
    }) {
        Some((f, l, c)) => format!("{f}:{l}:{c}"),
        None => format!("byte {byte}"),
    }
}

/// Right after lowering, with `compare`: check the checker's types against
/// the module's -- before inlining and async lowering give many values one
/// position.
pub(crate) fn compare_after_lowering(module: &IrModule) {
    if mode().as_deref() != Some("compare") {
        return;
    }
    let Some(report) = REPORT.with(|r| r.borrow().clone()) else {
        return;
    };
    let m = module;
    let mut by_start: HashMap<u32, Vec<IrType>> = HashMap::new();
    for ((start, _), t) in &report.types {
        by_start.entry(*start).or_default().push(t.clone());
    }
    let (mut agree, mut disagree, mut unmatched) = (0usize, 0usize, 0usize);
    let mut shown = 0usize;
    for func in m.functions() {
        for block in func.blocks() {
            for instr in &block.instrs {
                let Some(result) = instr.result() else {
                    continue;
                };
                // A string constant's type is never in doubt, and lowering
                // makes many of its own (panic messages) at a statement's
                // position.
                if matches!(instr, crate::ir::instr::IrInstr::ConstStr { .. }) {
                    continue;
                }
                let Some(byte) = func.span_table.get_for_val(result) else {
                    continue;
                };
                // Position 0: code the parser synthesised (generators).
                if byte == 0 {
                    continue;
                }
                let Some(ir) = func.value_type(result) else {
                    continue;
                };
                let candidates = by_start.get(&byte);
                // Several expressions start at one byte (`a` and `a + b`):
                // the value agrees with any of them, and is not compared when
                // one of them was not typed.
                // A function passed as a value is wrapped in a lambda whose
                // call to it keeps the name's position: its result is the
                // function's.
                let wrapper_call = |c: &IrType| {
                    func.name.starts_with("__lambda_")
                        && matches!(c, IrType::Fn { ret, .. } if same_type(ret, ir))
                };
                if candidates
                    .is_some_and(|cs| cs.iter().any(|c| same_type(c, ir) || wrapper_call(c)))
                {
                    agree += 1;
                } else if candidates.is_none() || report.untyped_starts.contains(&byte) {
                    unmatched += 1;
                } else {
                    disagree += 1;
                    if shown < 60 {
                        shown += 1;
                        let ours: Vec<String> = candidates
                            .into_iter()
                            .flatten()
                            .map(|c| c.to_string())
                            .collect();
                        eprintln!(
                            "typeck-differ: {}: ir `{}`, typeck `{}` (in {})",
                            position(m, byte),
                            ir,
                            ours.join("` or `"),
                            func.name
                        );
                    }
                }
            }
        }
    }
    eprintln!("typeck-compare: agree {agree} disagree {disagree} unmatched {unmatched}");
}

/// After the pass pipeline: print the checker's findings. Lines are
/// prefixed `typeck-` so a harness can read them.
pub(crate) fn after_passes(module: &IrModule) {
    let Some(report) = REPORT.with(|r| r.borrow_mut().take()) else {
        return;
    };
    if mode().is_none() {
        return;
    }
    for d in &report.errors {
        let b = d.spans.first().map(|s| s.start).unwrap_or(0);
        eprintln!(
            "typeck-error: {}: [{}] {}",
            position(module, b),
            d.code.as_deref().unwrap_or("?"),
            d.message
        );
    }
    for d in &report.literal_coercions {
        let b = d.spans.first().map(|s| s.start).unwrap_or(0);
        eprintln!("typeck-doubtful: {}: {}", position(module, b), d.message);
    }
    eprintln!(
        "typeck-stats: expressions {} typed {} reported {}",
        report.expressions, report.typed, report.reported
    );
}

/// Equal as the programmer sees them: records, choices and trait objects
/// by name, function types by result.
fn same_type(a: &IrType, b: &IrType) -> bool {
    match (a, b) {
        (IrType::Struct { name: x, .. }, IrType::Struct { name: y, .. }) => {
            x == y || loose_named(x, y)
        }
        (IrType::Enum { name: x, .. }, IrType::Enum { name: y, .. }) => x == y,
        (IrType::Struct { name: x, .. }, IrType::Enum { name: y, .. })
        | (IrType::Enum { name: x, .. }, IrType::Struct { name: y, .. }) => x == y,
        (IrType::TraitObject { name: x, .. }, IrType::TraitObject { name: y, .. }) => x == y,
        (IrType::Fn { ret: r1, .. }, IrType::Fn { ret: r2, .. }) => same_type(r1, r2),
        (IrType::List(x), IrType::List(y))
        | (IrType::Option(x), IrType::Option(y))
        | (IrType::Chan(x), IrType::Chan(y))
        | (IrType::Task(x), IrType::Task(y)) => same_type(x, y),
        (IrType::Map(a1, b1), IrType::Map(a2, b2))
        | (IrType::ResultType(a1, b1), IrType::ResultType(a2, b2)) => {
            same_type(a1, a2) && same_type(b1, b2)
        }
        (IrType::Tuple(xs), IrType::Tuple(ys)) => {
            xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| same_type(x, y))
        }
        (IrType::Array { elem: e1, len: l1 }, IrType::Array { elem: e2, len: l2 }) => {
            l1 == l2 && same_type(e1, e2)
        }
        _ => a == b,
    }
}
