//! THIR: the typed, desugared tree between HIR and the CFG.
//!
//! The pipeline is:
//!
//! ```text
//! tokens -> AST -> HIR -> THIR -> IrModule (CFG) -> passes -> codegen
//!                    ^ types   ^ explicit values, calls, fields
//!                              ^ tuple destructuring desugared
//! ```
//!
//! HIR is deliberately source-shaped: its node storage is the checked syntax
//! tree, with resolution and type tables beside it. THIR is the next stage:
//! it **owns** a second tree whose nodes say what they mean. A value carries
//! its [`Resolution`], a call carries its target, a method call carries the
//! mangled method it dispatches to, a field access carries the field's index
//! when the base type is known, a map literal carries its key/value types, and
//! tuple destructuring is rewritten into a temporary plus element accesses.
//!
//! THIR is built from the HIR after the source checker has run. Its type slots
//! are [`ThirType`]: `Known` is a solved [`IrType`], `Partial` keeps the
//! checker's structured [`Hint`] for a generic or partly-open type, and `Open`
//! is an expression the checker does not model. That is the same honest split
//! HIR reports, not a claim that every expression is fully typed.
//!
//! **Current status.** THIR is built and audited on every compile. Lowering
//! now consumes its type hints, record field indices, map key/value types, and
//! the shape of tuple destructuring, and the body walk lowers the pure value
//! forms plus binary operators, indexing, casts, if-expressions, and expression
//! blocks (with let/expr/return/assign) from THIR first; the AST body walk
//! remains for control flow, calls, when-expressions, and other statements.
//! Lowering's type queries are THIR-first:
//! a source span THIR saw is authoritative even when its hint is absent (the
//! checker left it open), and the HIR is only a fallback for
//! lowering-synthesized spans THIR never saw. The full body-walk migration is
//! still ahead. `tests/thir.rs` gates the audit.
//!
//! Limits of this slice, kept explicit rather than hidden:
//!
//! * `when` patterns are still the parser's [`AstWhenPattern`]; the variant a
//!   pattern denotes is recorded beside it, but patterns are not a second
//!   pattern tree yet.
//! * Signatures and cast targets keep their source [`AstType`]. A cast also
//!   carries a resolved [`IrType`] when it can be resolved without generic
//!   substitutions; generic casts are resolved by lowering per instance.
//! * Control flow (`if`, `when`, loops, `try`/`catch`, handlers, `select`) is
//!   kept as THIR control flow. MIR is the stage that turns it into blocks.
//! * The model/graph DSL bypasses HIR and therefore THIR; `lower_model` builds
//!   `GraphIr` directly.

// THIR is built and audited before the full lowering migration. Its fields are
// the stage's output; some are intentionally unread until then, so dead-code
// analysis would only repeat that fact for every node.
#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Mutex;

use crate::error::LowerError;
use crate::hir::{key, HirModule, HirType, Resolution, SpanKey};
use crate::ir::instr::BinOp;
use crate::ir::types::IrType;
use crate::lower::typeck::Hint;
use crate::parser::ast::{AstBinOp, AstType, AstUnaryOp, AstWhenPattern};
use crate::parser::lexer::Span;

pub(crate) mod build;

/// What THIR knows about an expression's type.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ThirType {
    /// The checker solved the whole type.
    Known(IrType),
    /// The checker solved part of it; lowering instantiates the rest.
    Partial(Hint),
    /// The checker does not model this expression; lowering determines it.
    Open,
}

impl ThirType {
    pub(crate) fn from_hir(ty: Option<&HirType>, hint: Option<&Hint>) -> Self {
        match ty {
            Some(HirType::Known(t)) => ThirType::Known(t.clone()),
            Some(HirType::Partial) => hint
                .cloned()
                .map(ThirType::Partial)
                .unwrap_or(ThirType::Open),
            Some(HirType::Open) | None => ThirType::Open,
        }
    }

    /// The whole type, when it is solved. A `Partial` hint whose parts are all
    /// known counts as solved.
    pub(crate) fn known(&self) -> Option<IrType> {
        match self {
            ThirType::Known(t) => Some(t.clone()),
            ThirType::Partial(h) => h.known(),
            ThirType::Open => None,
        }
    }

    /// The lowering-facing hint for this slot. `Open` has none, matching the
    /// HIR's hint table; `Known` is a whole type, not a partial shape.
    pub(crate) fn as_hint(&self) -> Option<Hint> {
        match self {
            ThirType::Known(t) => Some(Hint::Known(t.clone())),
            ThirType::Partial(h) => Some(h.clone()),
            ThirType::Open => None,
        }
    }

    pub(crate) fn is_open(&self) -> bool {
        matches!(self, ThirType::Open)
    }
}

/// A literal, kept apart from the AST so a THIR consumer does not need the
/// parser's node types.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ThirLiteral {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
}

/// One field initialiser in a struct literal. `index` is the field's position
/// in the record's layout when the record is known.
#[derive(Clone, Debug)]
pub(crate) struct ThirFieldInit {
    pub name: String,
    pub index: Option<usize>,
    pub value: ThirExpr,
}

/// A `when` arm. The pattern is still the parser's shape (see the module
/// limits); `variant` records the resolved `Choice.Variant` for an enum arm.
#[derive(Clone, Debug)]
pub(crate) struct ThirArm {
    pub pattern: AstWhenPattern,
    pub variant: Option<Resolution>,
    pub guard: Option<ThirExpr>,
    pub body: ThirExpr,
    pub span: Span,
}

/// A handler arm. Effect names are resolved by the HIR's `Resolution::Effect`
/// when the arm raises, not here; the arm keeps the written name and bindings.
#[derive(Clone, Debug)]
pub(crate) struct ThirHandlerArm {
    pub effect: String,
    pub params: Vec<String>,
    pub resume_param: Option<String>,
    pub body: ThirExpr,
    pub span: Span,
}

/// One `select` arm.
#[derive(Clone, Debug)]
pub(crate) struct ThirSelectArm {
    pub channel: ThirExpr,
    pub binding: String,
    pub body: ThirBlock,
    pub span: Span,
}

/// A lambda or function parameter. `ty` is absent for macro parameters, which
/// are untyped by construction.
#[derive(Clone, Debug)]
pub(crate) struct ThirParam {
    pub name: String,
    pub ty: Option<AstType>,
    pub default: Option<ThirExpr>,
}

/// A THIR expression: a kind, its type slot, and the source span it came from.
#[derive(Clone, Debug)]
pub(crate) struct ThirExpr {
    pub kind: ThirExprKind,
    pub ty: ThirType,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub(crate) enum ThirExprKind {
    /// A value name and what it denotes.
    Value {
        name: String,
        resolution: Resolution,
    },
    Literal(ThirLiteral),
    /// A direct call. `target` is the HIR's resolved callee.
    Call {
        callee: String,
        target: Resolution,
        args: Vec<ThirExpr>,
        named_args: Vec<(String, ThirExpr)>,
    },
    /// A method call. `target` is the mangled method when the registries know
    /// exactly one implementation, otherwise the written method name.
    MethodCall {
        receiver: Box<ThirExpr>,
        method: String,
        target: Resolution,
        args: Vec<ThirExpr>,
    },
    /// `base.field`. `index` is the field's position when `base`'s type is a
    /// known record.
    Field {
        base: Box<ThirExpr>,
        field: String,
        index: Option<usize>,
    },
    /// `base.0`: a tuple element.
    TupleIndex {
        base: Box<ThirExpr>,
        index: usize,
    },
    /// The index-th component of a tuple **or** record, used when the base's
    /// type is open. This is the desugared form of `LetTuple` for an unknown
    /// type: one node covers both shapes until the type is known.
    Destructure {
        base: Box<ThirExpr>,
        index: usize,
    },
    BinOp {
        op: AstBinOp,
        lhs: Box<ThirExpr>,
        rhs: Box<ThirExpr>,
    },
    UnaryOp {
        op: AstUnaryOp,
        expr: Box<ThirExpr>,
    },
    If {
        cond: Box<ThirExpr>,
        then_block: ThirBlock,
        else_block: Option<ThirBlock>,
    },
    Block(ThirBlock),
    Index {
        base: Box<ThirExpr>,
        indices: Vec<ThirExpr>,
    },
    Cast {
        expr: Box<ThirExpr>,
        ty: AstType,
        /// The cast target when it can be resolved without generic
        /// substitutions.
        resolved: Option<IrType>,
    },
    StructLit {
        name: String,
        target: Resolution,
        fields: Vec<ThirFieldInit>,
        spread: Option<Box<ThirExpr>>,
    },
    When {
        scrutinee: Box<ThirExpr>,
        arms: Vec<ThirArm>,
    },
    Tuple(Vec<ThirExpr>),
    Array(Vec<ThirExpr>),
    Lambda {
        params: Vec<ThirParam>,
        body: Box<ThirExpr>,
    },
    Await(Box<ThirExpr>),
    Try(Box<ThirExpr>),
    NullCoal {
        expr: Box<ThirExpr>,
        default: Box<ThirExpr>,
    },
    Mask {
        effects: Vec<String>,
        body: ThirBlock,
    },
    Handle {
        expr: Box<ThirExpr>,
        arms: Vec<ThirHandlerArm>,
        return_ty: AstType,
    },
    /// A map literal with its key and value types when known.
    Map {
        entries: Vec<(ThirExpr, ThirExpr)>,
        key_ty: Option<IrType>,
        val_ty: Option<IrType>,
    },
    Ref(Box<ThirExpr>),
    RefMut(Box<ThirExpr>),
    Deref(Box<ThirExpr>),
    Move(Box<ThirExpr>),
    Unsafe(Box<ThirExpr>),
    Splat(Box<ThirExpr>),
    TryCatch {
        body: Box<ThirExpr>,
        catch_param: String,
        catch_body: Box<ThirExpr>,
    },
    Raise {
        effect: String,
        target: Resolution,
        args: Vec<ThirExpr>,
    },
    MacroCall {
        name: String,
        args: Vec<ThirExpr>,
    },
}

/// A block of THIR statements and an optional tail expression.
#[derive(Clone, Debug)]
pub(crate) struct ThirBlock {
    /// Source-level block identity inherited from [`AstBlock`](crate::parser::ast::AstBlock).
    /// `0` for compiler-synthesized blocks. Drives `EnterBlock` / `ExitBlock` emission.
    pub scope_id: u32,
    pub stmts: Vec<ThirStmt>,
    pub tail: Option<Box<ThirExpr>>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub(crate) struct ThirStmt {
    pub kind: ThirStmtKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "THIR statements wrap short-lived AST/expr trees during lowering; boxing subfields adds unnecessary per-statement heap traffic"
)]
pub(crate) enum ThirStmtKind {
    Let {
        name: String,
        ty: Option<AstType>,
        init: ThirExpr,
        is_var: bool,
    },
    Expr(ThirExpr),
    While {
        label: Option<String>,
        cond: ThirExpr,
        body: ThirBlock,
    },
    Loop {
        label: Option<String>,
        body: ThirBlock,
    },
    Break {
        label: Option<String>,
    },
    Continue {
        label: Option<String>,
    },
    ForRange {
        label: Option<String>,
        var: String,
        start: ThirExpr,
        end: ThirExpr,
        inclusive: bool,
        step: Option<ThirExpr>,
        body: ThirBlock,
    },
    Assign {
        target: ThirExpr,
        op: Option<BinOp>,
        value: ThirExpr,
    },
    Return(Option<ThirExpr>),
    Spawn {
        body: ThirBlock,
        group: Option<ThirExpr>,
    },
    ParFor {
        label: Option<String>,
        var: String,
        start: ThirExpr,
        end: ThirExpr,
        inclusive: bool,
        body: ThirBlock,
    },
    ForEach {
        label: Option<String>,
        var: String,
        iter: ThirExpr,
        body: ThirBlock,
    },
    Mask {
        effects: Vec<String>,
        body: ThirBlock,
    },
    Handle {
        expr: ThirExpr,
        arms: Vec<ThirHandlerArm>,
        return_ty: AstType,
    },
    Defer(ThirExpr),
    Select {
        arms: Vec<ThirSelectArm>,
        default: Option<ThirBlock>,
    },
    Yield(ThirExpr),
}

/// What item a THIR body came from. Expression-only bodies (consts, state
/// initialisers, field defaults, macro bodies, model parameters, attribute
/// arguments) have no parameters and their expression is the block's tail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThirBodyKind {
    Function,
    Method,
    Const,
    State,
    FieldDefault,
    Macro,
    ModelParam,
    Attribute,
}

/// One function or expression body in THIR.
#[derive(Clone, Debug)]
pub(crate) struct ThirBody {
    pub name: String,
    pub kind: ThirBodyKind,
    pub params: Vec<ThirParam>,
    pub ret_ty: Option<AstType>,
    pub body: ThirBlock,
}

/// Counts of what a compile's THIR holds. Printed by `IRIS_THIR=report`, and
/// kept as the last compile's value for tests.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThirStats {
    pub bodies: usize,
    pub functions: usize,
    pub methods: usize,
    pub consts: usize,
    pub state_vars: usize,
    pub field_defaults: usize,
    pub macros: usize,
    pub model_params: usize,
    pub attributes: usize,
    /// THIR expression nodes, including the temporary and element accesses
    /// tuple destructuring introduces.
    pub expressions: usize,
    /// Distinct source expression spans. A complete THIR has one of these for
    /// every span the HIR walk reached.
    pub source_expressions: usize,
    pub statements: usize,
    pub known: usize,
    pub partial: usize,
    pub open: usize,
    pub values: usize,
    pub unresolved_values: usize,
    pub calls: usize,
    pub method_calls: usize,
    pub method_targets: usize,
    pub unresolved_calls: usize,
    pub maps: usize,
    pub maps_with_key: usize,
    pub maps_with_value: usize,
    /// Destructured components (`let (a, b) = ...` counts two).
    pub destructures: usize,
    pub tuple_destructures: usize,
    pub struct_destructures: usize,
    pub open_destructures: usize,
    pub field_accesses: usize,
    pub field_indices: usize,
    pub casts: usize,
    pub resolved_casts: usize,
}

/// A tuple destructuring as THIR desugared it: a temporary plus one
/// component per bound name. Lowering uses the component shape instead of
/// re-deriving tuple-versus-record from the source.
#[derive(Clone, Debug)]
pub(crate) struct ThirLetTuple {
    pub temp: String,
    pub init_ty: ThirType,
    pub components: Vec<ThirDestructure>,
}

/// One component of a desugared tuple destructuring.
#[derive(Clone, Debug)]
pub(crate) struct ThirDestructure {
    pub name: String,
    /// Position in the tuple or record.
    pub index: usize,
    /// Record field name, when the component is a record field.
    pub field: Option<String>,
    pub kind: ThirDestructureKind,
    pub ty: ThirType,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThirDestructureKind {
    Tuple,
    Field,
    Open,
}

/// The compact view of THIR that lowering consumes, indexed by source span.
/// It is not a second tree: types and component shapes are cloned, while the
/// bodies stay owned by [`ThirModule`].
#[derive(Default)]
pub(crate) struct ThirFacts {
    /// The THIR type slot of every source expression.
    pub types: HashMap<SpanKey, ThirType>,
    /// The checker's lowering-facing hint, copied from HIR so THIR is the
    /// first place lowering asks.
    pub hints: HashMap<SpanKey, Hint>,
    /// Record field layout for a source field access whose base type was known
    /// when THIR was built.
    pub field_indices: HashMap<SpanKey, usize>,
    /// Key/value types for a source map literal, when THIR knows them.
    pub map_types: HashMap<SpanKey, (Option<IrType>, Option<IrType>)>,
    /// Desugared tuple destructuring, keyed by the statement's span.
    pub let_tuples: HashMap<SpanKey, ThirLetTuple>,
}

/// The THIR of one compile. Unlike HIR, it owns its nodes; it does not borrow
/// the parser's tree.
pub(crate) struct ThirModule {
    pub(crate) bodies: Vec<ThirBody>,
    /// Every source expression span the builder reached. Compared with the
    /// HIR's set by [`validate`](ThirModule::validate).
    pub(crate) source_spans: HashSet<SpanKey>,
    /// Lowering-facing facts, indexed by source span.
    pub(crate) facts: ThirFacts,
    pub(crate) stats: ThirStats,
}

impl ThirModule {
    pub(crate) fn stats(&self) -> &ThirStats {
        &self.stats
    }

    /// The THIR type slot for a source expression.
    pub(crate) fn type_at(&self, span: Span) -> Option<&ThirType> {
        self.facts.types.get(&key(span))
    }

    /// The checker hint for a source expression, as lowering sees it.
    pub(crate) fn hint_at(&self, span: Span) -> Option<Hint> {
        self.facts.hints.get(&key(span)).cloned()
    }

    /// The record layout index THIR resolved for a source field access.
    pub(crate) fn field_index_at(&self, span: Span) -> Option<usize> {
        self.facts.field_indices.get(&key(span)).copied()
    }

    /// The key/value types THIR resolved for a source map literal.
    pub(crate) fn map_types_at(&self, span: Span) -> Option<(Option<IrType>, Option<IrType>)> {
        self.facts.map_types.get(&key(span)).cloned()
    }

    /// The desugared form of a source tuple destructuring.
    pub(crate) fn let_tuple_at(&self, span: Span) -> Option<&ThirLetTuple> {
        self.facts.let_tuples.get(&key(span))
    }

    /// The THIR expression built for a source span, if one was. Lowering uses
    /// this to consume THIR structurally rather than re-walking the AST.
    pub(crate) fn expr_at(&self, span: Span) -> Option<ThirExpr> {
        self.find_expr(span).cloned()
    }

    /// The THIR block built for this source span, if one was. Standalone
    /// statement blocks (loop bodies, select arms, handler tails, ...) are
    /// not expressions, so `expr_at` cannot see them; lowering uses this to
    /// find them.
    pub(crate) fn block_at(&self, span: Span) -> Option<ThirBlock> {
        find_block_in_module(&self.bodies, key(span))
    }

    fn find_expr(&self, span: Span) -> Option<&ThirExpr> {
        let target = key(span);
        for body in &self.bodies {
            if let Some(e) = find_expr_in_block(&body.body, target) {
                return Some(e);
            }
        }
        None
    }

    /// The body with this name, if there is one. Duplicate method names across
    /// impls are possible; the first match is returned.
    pub(crate) fn body(&self, name: &str) -> Option<&ThirBody> {
        self.bodies.iter().find(|b| b.name == name)
    }

    /// Everything that makes THIR incomplete, for `IRIS_THIR=check`.
    ///
    /// A valid program must have a THIR node for every expression the HIR
    /// walk reached, every HIR lowering hint copied into THIR's facts, no
    /// unresolved value names, and no calls without a target. Types may be
    /// `Open`: that is a modeled limit, not a missing node.
    pub(crate) fn validate(&self, hir: &HirModule<'_>) -> Vec<String> {
        let mut problems = Vec::new();
        let hir_spans: HashSet<SpanKey> = hir.seen_spans().collect();
        for span in &self.source_spans {
            if !hir_spans.contains(span) {
                problems.push(format!(
                    "THIR expression at bytes {span:?} is not in the HIR"
                ));
            }
        }
        for span in &hir_spans {
            if !self.source_spans.contains(span) {
                problems.push(format!(
                    "HIR expression at bytes {span:?} was not built into THIR"
                ));
            }
        }
        for span in hir.hint_spans() {
            if !self.facts.hints.contains_key(&span) {
                problems.push(format!(
                    "HIR lowering hint at bytes {span:?} was not copied into THIR"
                ));
            }
        }
        if self.stats.unresolved_values > 0 {
            problems.push(format!(
                "{} THIR value name(s) did not resolve",
                self.stats.unresolved_values
            ));
        }
        if self.stats.unresolved_calls > 0 {
            problems.push(format!(
                "{} THIR call(s) have no target",
                self.stats.unresolved_calls
            ));
        }
        if self.stats.method_calls > self.stats.method_targets {
            problems.push(format!(
                "{} of {} THIR method call(s) did not resolve to a method",
                self.stats.method_calls - self.stats.method_targets,
                self.stats.method_calls
            ));
        }
        if self.stats.bodies == 0 && !hir_spans.is_empty() {
            problems.push("THIR has no bodies but the HIR has expressions".to_string());
        }
        problems
    }

    /// `IRIS_THIR=report` output.
    pub(crate) fn report(&self, module: &str) {
        let s = &self.stats;
        eprintln!(
            "thir {module}: {} bodies ({} functions, {} methods, {} consts, {} state, {} field defaults, {} macros, {} model params, {} attributes)",
            s.bodies,
            s.functions,
            s.methods,
            s.consts,
            s.state_vars,
            s.field_defaults,
            s.macros,
            s.model_params,
            s.attributes
        );
        eprintln!(
            "thir {module}: {} expressions ({} known, {} partial, {} open; {} source spans), {} statements",
            s.expressions, s.known, s.partial, s.open, s.source_expressions, s.statements
        );
        eprintln!(
            "thir {module}: {} values ({} unresolved), {} calls ({} method, {} method targets, {} unresolved)",
            s.values, s.unresolved_values, s.calls, s.method_calls, s.method_targets, s.unresolved_calls
        );
        eprintln!(
            "thir {module}: {} maps ({} key-typed, {} value-typed), {} destructures ({} tuple, {} struct, {} open), {} field accesses ({} indexed), {} casts ({} resolved)",
            s.maps,
            s.maps_with_key,
            s.maps_with_value,
            s.destructures,
            s.tuple_destructures,
            s.struct_destructures,
            s.open_destructures,
            s.field_accesses,
            s.field_indices,
            s.casts,
            s.resolved_casts
        );
    }
}

/// The last compile's THIR statistics, for tests. A compile may run on a
/// spawned thread, so this is process-wide.
static LAST_STATS: Mutex<Option<ThirStats>> = Mutex::new(None);

thread_local! {
    /// The THIR of the compile currently lowering on this thread. Lowering's
    /// sub-lowerers (lambdas, handlers, spawn bodies) share it, so the facts
    /// do not have to be threaded through every constructor.
    static CONTEXT: RefCell<Option<Rc<ThirModule>>> = const { RefCell::new(None) };
}

/// Install the compile's THIR for lowering on this thread. Called after the
/// HIR checker has run and before the first function is lowered.
pub(crate) fn install(module: Rc<ThirModule>) {
    if let Ok(mut last) = LAST_STATS.lock() {
        *last = Some(module.stats.clone());
    }
    CONTEXT.with(|ctx| *ctx.borrow_mut() = Some(module));
}

/// Remove the compile's THIR from this thread, allowing it to be freed once
/// lowering no longer needs it.
/// Remove the compile's THIR from this thread, allowing it to be freed once
/// lowering no longer needs it.
pub(crate) fn uninstall() {
    CONTEXT.with(|ctx| *ctx.borrow_mut() = None);
}

fn with_context<R>(f: impl FnOnce(&ThirModule) -> R) -> Option<R> {
    CONTEXT.with(|ctx| ctx.borrow().as_ref().map(|module| f(module)))
}

/// The checker hint for a source expression, from THIR. `None` means THIR has
/// no hint for the span (an open type or a lowering-synthesized node).
pub(crate) fn hint(span: Span) -> Option<Hint> {
    with_context(|module| module.hint_at(span)).flatten()
}

/// Whether THIR built a node for this source span. A span THIR saw is
/// authoritative even when its hint is `None`: that is an open type, not a
/// reason to ask the HIR again. Spans THIR never saw are lowering-synthesized
/// nodes and may use the HIR fallback.
pub(crate) fn is_source_span(span: Span) -> bool {
    with_context(|module| module.source_spans.contains(&key(span))).unwrap_or(false)
}

/// The record layout index THIR resolved for a source field access.
pub(crate) fn field_index(span: Span) -> Option<usize> {
    with_context(|module| module.field_index_at(span)).flatten()
}

/// The key/value types THIR resolved for a source map literal.
pub(crate) fn map_types(span: Span) -> Option<(Option<IrType>, Option<IrType>)> {
    with_context(|module| module.map_types_at(span)).flatten()
}

/// The desugared form of a source tuple destructuring.
pub(crate) fn let_tuple(span: Span) -> Option<ThirLetTuple> {
    with_context(|module| module.let_tuple_at(span).cloned()).flatten()
}

/// The THIR expression built for a source span, if one was.
pub(crate) fn expr_at(span: Span) -> Option<ThirExpr> {
    with_context(|module| module.expr_at(span)).flatten()
}

/// The THIR body with this name, if there is one.
pub(crate) fn body(name: &str) -> Option<ThirBody> {
    with_context(|module| module.body(name).cloned()).flatten()
}

/// The THIR block built for this source span, if one was.
pub(crate) fn block_at(span: Span) -> Option<ThirBlock> {
    with_context(|module| module.block_at(span)).flatten()
}

fn find_expr_in_block(block: &ThirBlock, target: SpanKey) -> Option<&ThirExpr> {
    for stmt in &block.stmts {
        if let Some(e) = find_expr_in_stmt(stmt, target) {
            return Some(e);
        }
    }
    if let Some(tail) = &block.tail {
        if let Some(e) = find_expr_in_expr(tail, target) {
            return Some(e);
        }
    }
    None
}

fn find_expr_in_stmt(stmt: &ThirStmt, target: SpanKey) -> Option<&ThirExpr> {
    match &stmt.kind {
        ThirStmtKind::Let { init, .. } => find_expr_in_expr(init, target),
        ThirStmtKind::Expr(e) => find_expr_in_expr(e, target),
        ThirStmtKind::While { cond, body, .. } => {
            find_expr_in_expr(cond, target).or_else(|| find_expr_in_block(body, target))
        }
        ThirStmtKind::Loop { body, .. } => find_expr_in_block(body, target),
        ThirStmtKind::ForRange {
            start,
            end,
            step,
            body,
            ..
        } => find_expr_in_expr(start, target)
            .or_else(|| find_expr_in_expr(end, target))
            .or_else(|| step.as_ref().and_then(|s| find_expr_in_expr(s, target)))
            .or_else(|| find_expr_in_block(body, target)),
        ThirStmtKind::Assign {
            target: t, value, ..
        } => find_expr_in_expr(t, target).or_else(|| find_expr_in_expr(value, target)),
        ThirStmtKind::Return(value) => value.as_ref().and_then(|e| find_expr_in_expr(e, target)),
        ThirStmtKind::Spawn { body, group } => find_expr_in_block(body, target)
            .or_else(|| group.as_ref().and_then(|e| find_expr_in_expr(e, target))),
        ThirStmtKind::ParFor {
            start, end, body, ..
        } => find_expr_in_expr(start, target)
            .or_else(|| find_expr_in_expr(end, target))
            .or_else(|| find_expr_in_block(body, target)),
        ThirStmtKind::ForEach { iter, body, .. } => {
            find_expr_in_expr(iter, target).or_else(|| find_expr_in_block(body, target))
        }
        ThirStmtKind::Mask { body, .. } => find_expr_in_block(body, target),
        ThirStmtKind::Handle { expr, arms, .. } => find_expr_in_expr(expr, target)
            .or_else(|| arms.iter().find_map(|a| find_expr_in_expr(&a.body, target))),
        ThirStmtKind::Defer(e) => find_expr_in_expr(e, target),
        ThirStmtKind::Select { arms, default } => arms
            .iter()
            .find_map(|a| {
                find_expr_in_expr(&a.channel, target)
                    .or_else(|| find_expr_in_block(&a.body, target))
            })
            .or_else(|| default.as_ref().and_then(|b| find_expr_in_block(b, target))),
        ThirStmtKind::Yield(e) => find_expr_in_expr(e, target),
        ThirStmtKind::Break { .. } | ThirStmtKind::Continue { .. } => None,
    }
}

fn find_block_in_expr(expr: &ThirExpr, target: SpanKey) -> Option<&ThirBlock> {
    match &expr.kind {
        ThirExprKind::Block(b) => {
            if key(b.span) == target {
                Some(b)
            } else {
                find_block_in_block(b, target)
            }
        }
        ThirExprKind::Mask { body, .. } => find_block_in_block(body, target),
        ThirExprKind::If {
            cond,
            then_block,
            else_block,
            ..
        } => find_block_in_expr(cond, target)
            .or_else(|| find_block_in_block(then_block, target))
            .or_else(|| {
                else_block
                    .as_ref()
                    .and_then(|b| find_block_in_block(b, target))
            }),
        ThirExprKind::BinOp { lhs, rhs, .. } => {
            find_block_in_expr(lhs, target).or_else(|| find_block_in_expr(rhs, target))
        }
        ThirExprKind::UnaryOp { expr, .. }
        | ThirExprKind::Cast { expr, .. }
        | ThirExprKind::Await(expr)
        | ThirExprKind::Try(expr)
        | ThirExprKind::Ref(expr)
        | ThirExprKind::RefMut(expr)
        | ThirExprKind::Deref(expr)
        | ThirExprKind::Move(expr)
        | ThirExprKind::Unsafe(expr)
        | ThirExprKind::Splat(expr) => find_block_in_expr(expr, target),
        ThirExprKind::TryCatch {
            body, catch_body, ..
        } => find_block_in_expr(body, target).or_else(|| find_block_in_expr(catch_body, target)),
        ThirExprKind::Call {
            args, named_args, ..
        } => {
            for a in args {
                if let Some(b) = find_block_in_expr(a, target) {
                    return Some(b);
                }
            }
            for (_, a) in named_args {
                if let Some(b) = find_block_in_expr(a, target) {
                    return Some(b);
                }
            }
            None
        }
        ThirExprKind::MethodCall { receiver, args, .. } => find_block_in_expr(receiver, target)
            .or_else(|| args.iter().find_map(|a| find_block_in_expr(a, target))),
        ThirExprKind::Field { base, .. }
        | ThirExprKind::TupleIndex { base, .. }
        | ThirExprKind::Destructure { base, .. } => find_block_in_expr(base, target),
        ThirExprKind::Index { base, indices, .. } => find_block_in_expr(base, target)
            .or_else(|| indices.iter().find_map(|i| find_block_in_expr(i, target))),
        ThirExprKind::Tuple(elements) | ThirExprKind::Array(elements) => {
            elements.iter().find_map(|e| find_block_in_expr(e, target))
        }
        ThirExprKind::Map { entries, .. } => entries.iter().find_map(|(k, v)| {
            find_block_in_expr(k, target).or_else(|| find_block_in_expr(v, target))
        }),
        ThirExprKind::StructLit { fields, spread, .. } => fields
            .iter()
            .find_map(|f| find_block_in_expr(&f.value, target))
            .or_else(|| spread.as_ref().and_then(|s| find_block_in_expr(s, target))),
        ThirExprKind::When {
            arms, scrutinee, ..
        } => find_block_in_expr(scrutinee, target).or_else(|| {
            arms.iter().find_map(|a| {
                find_block_in_expr(&a.body, target)
                    .or_else(|| a.guard.as_ref().and_then(|g| find_block_in_expr(g, target)))
            })
        }),
        ThirExprKind::Lambda { body, .. } => find_block_in_expr(body, target),
        ThirExprKind::Handle { expr, arms, .. } => find_block_in_expr(expr, target).or_else(|| {
            arms.iter()
                .find_map(|a| find_block_in_expr(&a.body, target))
        }),
        ThirExprKind::Raise { args, .. } | ThirExprKind::MacroCall { args, .. } => {
            args.iter().find_map(|a| find_block_in_expr(a, target))
        }
        ThirExprKind::NullCoal { expr, default } => {
            find_block_in_expr(expr, target).or_else(|| find_block_in_expr(default, target))
        }
        _ => None,
    }
}

fn find_block_in_module(bodies: &[ThirBody], target: SpanKey) -> Option<ThirBlock> {
    for body in bodies {
        if let Some(b) = find_block_in_block(&body.body, target) {
            return Some(b.clone());
        }
    }
    None
}

fn find_block_in_block(block: &ThirBlock, target: SpanKey) -> Option<&ThirBlock> {
    if key(block.span) == target {
        return Some(block);
    }
    for stmt in &block.stmts {
        if let Some(b) = find_block_in_stmt(stmt, target) {
            return Some(b);
        }
    }
    block
        .tail
        .as_ref()
        .and_then(|t| find_block_in_expr(t, target))
}

fn find_block_in_stmt(stmt: &ThirStmt, target: SpanKey) -> Option<&ThirBlock> {
    match &stmt.kind {
        ThirStmtKind::Let { init, .. } => find_block_in_expr(init, target),
        ThirStmtKind::Expr(e) => find_block_in_expr(e, target),
        ThirStmtKind::While { cond, body, .. } => {
            find_block_in_expr(cond, target).or_else(|| find_block_in_block(body, target))
        }
        ThirStmtKind::Loop { body, .. } => find_block_in_block(body, target),
        ThirStmtKind::ForRange {
            start,
            end,
            step,
            body,
            ..
        } => find_block_in_expr(start, target)
            .or_else(|| find_block_in_expr(end, target))
            .or_else(|| step.as_ref().and_then(|s| find_block_in_expr(s, target)))
            .or_else(|| find_block_in_block(body, target)),
        ThirStmtKind::Assign {
            target: t, value, ..
        } => find_block_in_expr(t, target).or_else(|| find_block_in_expr(value, target)),
        ThirStmtKind::Return(value) => value.as_ref().and_then(|e| find_block_in_expr(e, target)),
        ThirStmtKind::Spawn { body, group } => find_block_in_block(body, target)
            .or_else(|| group.as_ref().and_then(|e| find_block_in_expr(e, target))),
        ThirStmtKind::ParFor {
            start, end, body, ..
        } => find_block_in_expr(start, target)
            .or_else(|| find_block_in_expr(end, target))
            .or_else(|| find_block_in_block(body, target)),
        ThirStmtKind::ForEach { iter, body, .. } => {
            find_block_in_expr(iter, target).or_else(|| find_block_in_block(body, target))
        }
        ThirStmtKind::Mask { body, .. } => find_block_in_block(body, target),
        ThirStmtKind::Handle { expr, arms, .. } => find_block_in_expr(expr, target).or_else(|| {
            arms.iter()
                .find_map(|a| find_block_in_expr(&a.body, target))
        }),
        ThirStmtKind::Defer(e) => find_block_in_expr(e, target),
        ThirStmtKind::Select { arms, default } => arms
            .iter()
            .find_map(|a| {
                find_block_in_expr(&a.channel, target)
                    .or_else(|| find_block_in_block(&a.body, target))
            })
            .or_else(|| {
                default
                    .as_ref()
                    .and_then(|b| find_block_in_block(b, target))
            }),
        ThirStmtKind::Yield(e) => find_block_in_expr(e, target),
        ThirStmtKind::Break { .. } | ThirStmtKind::Continue { .. } => None,
    }
}

fn find_expr_in_expr(expr: &ThirExpr, target: SpanKey) -> Option<&ThirExpr> {
    if key(expr.span) == target {
        return Some(expr);
    }
    match &expr.kind {
        ThirExprKind::BinOp { lhs, rhs, .. } => {
            find_expr_in_expr(lhs, target).or_else(|| find_expr_in_expr(rhs, target))
        }
        ThirExprKind::UnaryOp { expr: e, .. } => find_expr_in_expr(e, target),
        ThirExprKind::Call {
            args, named_args, ..
        } => {
            for a in args {
                if let Some(e) = find_expr_in_expr(a, target) {
                    return Some(e);
                }
            }
            for (_, a) in named_args {
                if let Some(e) = find_expr_in_expr(a, target) {
                    return Some(e);
                }
            }
            None
        }
        ThirExprKind::MethodCall { receiver, args, .. } => {
            if let Some(e) = find_expr_in_expr(receiver, target) {
                return Some(e);
            }
            for a in args {
                if let Some(e) = find_expr_in_expr(a, target) {
                    return Some(e);
                }
            }
            None
        }
        ThirExprKind::Field { base, .. } => find_expr_in_expr(base, target),
        ThirExprKind::TupleIndex { base, .. } => find_expr_in_expr(base, target),
        ThirExprKind::Destructure { base, .. } => find_expr_in_expr(base, target),
        ThirExprKind::If {
            cond,
            then_block,
            else_block,
        } => find_expr_in_expr(cond, target)
            .or_else(|| find_expr_in_block(then_block, target))
            .or_else(|| {
                else_block
                    .as_ref()
                    .and_then(|b| find_expr_in_block(b, target))
            }),
        ThirExprKind::Block(b) => find_expr_in_block(b, target),
        ThirExprKind::Index { base, indices } => {
            if let Some(e) = find_expr_in_expr(base, target) {
                return Some(e);
            }
            for i in indices {
                if let Some(e) = find_expr_in_expr(i, target) {
                    return Some(e);
                }
            }
            None
        }
        ThirExprKind::Cast { expr: e, .. } => find_expr_in_expr(e, target),
        ThirExprKind::Tuple(elements) | ThirExprKind::Array(elements) => {
            elements.iter().find_map(|e| find_expr_in_expr(e, target))
        }
        ThirExprKind::Map { entries, .. } => entries.iter().find_map(|(k, v)| {
            find_expr_in_expr(k, target).or_else(|| find_expr_in_expr(v, target))
        }),
        ThirExprKind::StructLit { fields, spread, .. } => fields
            .iter()
            .find_map(|f| find_expr_in_expr(&f.value, target))
            .or_else(|| spread.as_ref().and_then(|s| find_expr_in_expr(s, target))),
        ThirExprKind::When {
            scrutinee, arms, ..
        } => find_expr_in_expr(scrutinee, target).or_else(|| {
            arms.iter().find_map(|a| {
                a.guard
                    .as_ref()
                    .and_then(|g| find_expr_in_expr(g, target))
                    .or_else(|| find_expr_in_expr(&a.body, target))
            })
        }),
        ThirExprKind::Lambda { params, body, .. } => params
            .iter()
            .find_map(|p| {
                p.default
                    .as_ref()
                    .and_then(|d| find_expr_in_expr(d, target))
            })
            .or_else(|| find_expr_in_expr(body, target)),
        ThirExprKind::Handle { expr, arms, .. } => find_expr_in_expr(expr, target)
            .or_else(|| arms.iter().find_map(|a| find_expr_in_expr(&a.body, target))),
        ThirExprKind::TryCatch {
            body, catch_body, ..
        } => find_expr_in_expr(body, target).or_else(|| find_expr_in_expr(catch_body, target)),
        ThirExprKind::NullCoal { expr, default } => {
            find_expr_in_expr(expr, target).or_else(|| find_expr_in_expr(default, target))
        }
        ThirExprKind::Await(e)
        | ThirExprKind::Try(e)
        | ThirExprKind::Ref(e)
        | ThirExprKind::RefMut(e)
        | ThirExprKind::Deref(e)
        | ThirExprKind::Move(e)
        | ThirExprKind::Unsafe(e)
        | ThirExprKind::Splat(e) => find_expr_in_expr(e, target),
        ThirExprKind::Mask { body, .. } => find_expr_in_block(body, target),
        ThirExprKind::MacroCall { args, .. } => {
            args.iter().find_map(|a| find_expr_in_expr(a, target))
        }
        ThirExprKind::Raise { args, .. } => args.iter().find_map(|a| find_expr_in_expr(a, target)),
        ThirExprKind::Literal(_) | ThirExprKind::Value { .. } => None,
    }
}

/// Statistics of the last compile's THIR on this process, if one has run.
pub fn last_stats() -> Option<ThirStats> {
    LAST_STATS.lock().ok().and_then(|s| s.clone())
}

/// `IRIS_THIR`: `report` prints the THIR, `check` also fails an incomplete one.
pub(crate) fn mode() -> Option<String> {
    std::env::var("IRIS_THIR")
        .ok()
        .filter(|m| m == "report" || m == "check")
}

/// The error `IRIS_THIR=check` reports for an incomplete THIR.
pub(crate) fn incomplete(problems: Vec<String>) -> LowerError {
    let detail = problems.join("; ");
    LowerError::rich(
        "E0902",
        crate::diagnostics::Diagnostic::error(format!(
            "internal compiler error: incomplete THIR: {detail}"
        ))
        .with_code("E0902"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::DType;

    #[test]
    fn installed_context_serves_lowering_facts() {
        let span = Span::new(1, 4);
        let mut facts = ThirFacts::default();
        facts.hints.insert(key(span), Hint::Known(IrType::Str));
        facts.field_indices.insert(key(span), 2);
        facts.map_types.insert(
            key(span),
            (Some(IrType::Str), Some(IrType::Scalar(DType::I64))),
        );
        facts.let_tuples.insert(
            key(span),
            ThirLetTuple {
                temp: "__tmp".to_string(),
                init_ty: ThirType::Known(IrType::Tuple(vec![
                    IrType::Str,
                    IrType::Scalar(DType::I64),
                ])),
                components: vec![ThirDestructure {
                    name: "x".to_string(),
                    index: 1,
                    field: None,
                    kind: ThirDestructureKind::Tuple,
                    ty: ThirType::Known(IrType::Scalar(DType::I64)),
                }],
            },
        );
        install(Rc::new(ThirModule {
            bodies: Vec::new(),
            source_spans: HashSet::from([key(span)]),
            facts,
            stats: ThirStats::default(),
        }));

        assert!(is_source_span(span));
        assert!(!is_source_span(Span::new(5, 8)));
        assert!(matches!(hint(span), Some(Hint::Known(IrType::Str))));
        assert_eq!(field_index(span), Some(2));
        assert_eq!(
            map_types(span),
            Some((Some(IrType::Str), Some(IrType::Scalar(DType::I64))))
        );
        let tuple = let_tuple(span).expect("installed let tuple");
        assert_eq!(tuple.components[0].index, 1);
        assert_eq!(tuple.components[0].name, "x");
    }
}
