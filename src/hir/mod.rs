//! HIR: the name-resolved, typed module between the syntax tree and the CFG.
//!
//! The pipeline is:
//!
//! ```text
//! tokens -> AST -> HIR -> THIR -> IrModule (CFG) -> passes -> codegen
//!                    ^ name resolution
//!                    ^ type checking
//! ```
//!
//! `build` walks the parser's AST once and produces a [`HirModule`]: every
//! value name is resolved to what it denotes (`Local`, `Fn`, `Builtin`, ...),
//! every call site records its target, and every expression is registered so
//! the type checker's answers can be attached to it. The source type checker
//! (`lower::typeck`) runs on the HIR (stage 5, as rustc's does), and
//! [`HirModule::absorb`] stores its report in the module. THIR is built from
//! the HIR and installed for lowering. Lowering reads solved `-> Infer`
//! returns from the HIR through [`return_type`] and type hints from THIR; it
//! also consumes THIR's field indices, map types, and tuple-destructuring
//! shape. The body walk still pattern-matches the AST for control flow and the
//! remaining forms. Types therefore flow **downhill**: AST -> HIR -> THIR -> CFG.
//!
//! The node storage is the checked source tree, which HIR deliberately keeps
//! source-shaped (rustc's HIR does too). What makes it HIR is that it is
//! resolved, typed, and validated, and that it is the source of THIR's types
//! and of lowering's solved `-> Infer` returns.
//!
//! ## What is resolved here
//!
//! * Every identifier in value position, including call callees, with
//!   lowering's rule that a built-in wins over a local of the same name in
//!   call position (known-issue #15).
//! * Method-call names, struct-literal constructors, enum patterns, and
//!   `raise` effect names.
//! * Every function, impl method, constant, state initialiser, record field
//!   default, and macro body is walked, so no expression can escape typing
//!   unnoticed.
//!
//! ## What is not
//!
//! * Type names in annotations are resolved by lowering's existing type
//!   tables (`resolve_brought_name`); HIR records value names.
//! * Desugaring before HIR belongs to the parser and the AST passes:
//!   f-strings are expanded by the parser; inline modules are flattened,
//!   trait default methods injected, `yield` desugared, and macros expanded
//!   before the HIR walk. THIR then desugars tuple destructuring and gives map
//!   literals explicit key/value types.
//!
//! `IRIS_HIR=report` prints what a compile's HIR holds; `IRIS_HIR=check`
//! additionally fails the compile if the module is incomplete (an expression
//! with no type entry, an unresolved name, or a walk that missed an item).

pub mod build;

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use crate::ir::types::IrType;
use crate::lower::typeck::{Hint, TypeckReport};
use crate::parser::ast::AstModule;
use crate::parser::lexer::Span;

/// A span key, as the checker records types.
pub(crate) type SpanKey = (u32, u32);

#[inline]
pub(crate) fn key(span: Span) -> SpanKey {
    (span.start.0, span.end.0)
}

/// What the type checker determined for one expression.
#[derive(Clone, Debug, PartialEq)]
pub enum HirType {
    /// The whole type is solved: lowering must not re-derive it.
    Known(IrType),
    /// Part of the type is solved (`list<_>`, a generic parameter, ...);
    /// lowering substitutes or decides the open parts.
    Partial,
    /// The checker does not model this expression (a handler body, a form
    /// only lowering understands). Lowering determines it; it is counted,
    /// not hidden.
    Open,
}

impl HirType {
    pub fn is_known(&self) -> bool {
        matches!(self, HirType::Known(_))
    }
}

/// What a name denotes after resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// A parameter, `let`, loop variable, pattern binding, or lambda
    /// parameter in scope.
    Local,
    /// A function of the program (possibly module-mangled).
    Fn(String),
    /// A constant of the program.
    Const(String),
    /// A top-level `var` state slot.
    State(String),
    /// A language or runtime built-in.
    Builtin(String),
    /// A record, choice, or alias name.
    Type(String),
    /// An enum variant: `Choice.Variant`.
    Variant { choice: String, variant: String },
    /// A method call's name (`Type__method` when the registries know it).
    Method(String),
    /// An algebraic effect named by `raise`.
    Effect(String),
    /// A brought module qualifier (`string`, `collections`, ...) used as the
    /// base of a qualified method call.
    Module(String),
    /// Nothing the program or the language defines.
    Unknown,
}

impl Resolution {
    fn is_unknown(&self) -> bool {
        matches!(self, Resolution::Unknown)
    }
}

/// One call site, resolved.
#[derive(Clone, Debug)]
pub(crate) struct CallSite {
    pub span: Span,
    /// The name written at the call site.
    pub name: String,
    /// What it resolved to.
    pub target: Resolution,
    /// Positional plus named arguments.
    pub argc: usize,
    /// `base.method(...)` rather than `name(...)`.
    pub method: bool,
}

/// Counts of what a compile's HIR holds. Printed by `IRIS_HIR=report`, and
/// kept as the last compile's value for tests.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HirStats {
    pub functions: usize,
    pub impls: usize,
    pub consts: usize,
    pub state_vars: usize,
    pub macros: usize,
    pub expressions: usize,
    pub typed: usize,
    pub partial: usize,
    pub open: usize,
    pub names: usize,
    pub local_names: usize,
    pub function_names: usize,
    pub builtin_names: usize,
    pub const_names: usize,
    pub state_names: usize,
    pub type_names: usize,
    pub method_names: usize,
    pub effect_names: usize,
    pub variant_names: usize,
    pub module_names: usize,
    pub unresolved_names: usize,
    pub calls: usize,
    pub function_calls: usize,
    pub builtin_calls: usize,
    pub method_calls: usize,
    pub unknown_calls: usize,
}

/// The HIR of one compile. Borrows the parser's module for its nodes; owns
/// everything resolution and typing add.
pub(crate) struct HirModule<'a> {
    source: &'a AstModule,
    /// Every expression the walk reached, by span.
    seen: HashSet<SpanKey>,
    /// Every expression's type, by span: known, partial, or explicitly open.
    types: HashMap<SpanKey, HirType>,
    /// The checker's structured hints, which lowering instantiates for
    /// generic bodies and parts of composite types.
    hints: HashMap<SpanKey, Hint>,
    /// Solved `-> Infer` returns.
    returns: HashMap<String, IrType>,
    /// Every value name use, by the identifier's span.
    resolutions: HashMap<SpanKey, Resolution>,
    /// Every call site.
    calls: Vec<CallSite>,
    /// Whether the source checker's report was absorbed. With
    /// `IRIS_TYPECK=off` it is not: names are resolved but types are
    /// lowering's.
    typed_by_checker: bool,
    stats: HirStats,
}

impl<'a> HirModule<'a> {
    /// The checked module's tree. The type checker and lowering both read
    /// this; it is the HIR's node storage.
    pub(crate) fn source(&self) -> &'a AstModule {
        self.source
    }

    /// Attach the type checker's report: its solved types, hints, and
    /// `-> Infer` returns become the HIR's. Expressions the checker did not
    /// model are recorded as `Open`, never left absent.
    pub(crate) fn absorb(&mut self, report: &TypeckReport) {
        self.typed_by_checker = true;
        for (span, ty) in &report.types {
            self.types.insert(*span, HirType::Known(ty.clone()));
        }
        for (span, hint) in &report.hints {
            self.hints.insert(*span, hint.clone());
            self.types.entry(*span).or_insert(HirType::Partial);
        }
        self.returns = report.returns.clone();
        for span in &self.seen {
            self.types.entry(*span).or_insert(HirType::Open);
        }
        self.recount();
    }

    /// Record an expression the walk reached.
    fn saw(&mut self, span: Span) {
        self.seen.insert(key(span));
    }

    /// The type of the expression at `span`, if it was walked.
    pub(crate) fn type_at(&self, span: Span) -> Option<&HirType> {
        self.types.get(&key(span))
    }

    /// Every span the checker gave a lowering hint. THIR copies these into
    /// its lowering-facing facts and audits the copy.
    pub(crate) fn hint_spans(&self) -> impl Iterator<Item = SpanKey> + '_ {
        self.hints.keys().copied()
    }

    /// The checker's structured hint for the expression at `span`, if it
    /// solved a part of the type. THIR keeps the partial shape; lowering
    /// instantiates it per generic instance.
    pub(crate) fn hint_at(&self, span: Span) -> Option<&Hint> {
        self.hints.get(&key(span))
    }

    /// What the value name at `span` denotes.
    pub(crate) fn resolution_at(&self, span: Span) -> Option<&Resolution> {
        self.resolutions.get(&key(span))
    }

    /// The call site whose whole-expression span is `span`.
    pub(crate) fn call_at(&self, span: Span) -> Option<&CallSite> {
        self.calls.iter().find(|c| key(c.span) == key(span))
    }

    /// Every expression span the HIR walk reached. THIR uses this to prove
    /// its own walk did not miss a node.
    pub(crate) fn seen_spans(&self) -> impl Iterator<Item = SpanKey> + '_ {
        self.seen.iter().copied()
    }

    pub(crate) fn stats(&self) -> &HirStats {
        &self.stats
    }

    pub(crate) fn calls(&self) -> &[CallSite] {
        &self.calls
    }

    /// Recompute counts from the tables. Shape counts (functions, impls,
    /// ...) are the builder's.
    fn recount(&mut self) {
        let shape = (
            self.stats.functions,
            self.stats.impls,
            self.stats.consts,
            self.stats.state_vars,
            self.stats.macros,
        );
        let mut s = HirStats {
            functions: shape.0,
            impls: shape.1,
            consts: shape.2,
            state_vars: shape.3,
            macros: shape.4,
            expressions: self.seen.len(),
            ..HirStats::default()
        };
        // Count the walked expressions, not the type table: a type entry the
        // checker recorded for an expression the walk missed must not inflate
        // the split (it is a validation error instead).
        for span in &self.seen {
            match self.types.get(span) {
                Some(HirType::Known(_)) => s.typed += 1,
                Some(HirType::Partial) => s.partial += 1,
                _ => s.open += 1,
            }
        }
        s.names = self.resolutions.len();
        for r in self.resolutions.values() {
            match r {
                Resolution::Local => s.local_names += 1,
                Resolution::Fn(_) => s.function_names += 1,
                Resolution::Const(_) => s.const_names += 1,
                Resolution::State(_) => s.state_names += 1,
                Resolution::Builtin(_) => s.builtin_names += 1,
                Resolution::Type(_) => s.type_names += 1,
                Resolution::Method(_) => s.method_names += 1,
                Resolution::Effect(_) => s.effect_names += 1,
                Resolution::Variant { .. } => s.variant_names += 1,
                Resolution::Module(_) => s.module_names += 1,
                Resolution::Unknown => s.unresolved_names += 1,
            }
        }
        s.calls = self.calls.len();
        for c in &self.calls {
            match &c.target {
                Resolution::Method(_) => s.method_calls += 1,
                Resolution::Builtin(_) => s.builtin_calls += 1,
                Resolution::Fn(_)
                | Resolution::Const(_)
                | Resolution::Local
                | Resolution::Type(_) => s.function_calls += 1,
                Resolution::Unknown => s.unknown_calls += 1,
                _ => s.function_calls += 1,
            }
        }
        self.stats = s;
    }

    /// Everything that makes the HIR incomplete, for `IRIS_HIR=check`.
    ///
    /// A valid program must have no unresolved value names and no calls to
    /// unknown targets. With the checker on, every walked expression must
    /// have a type entry (`absorb` fills them all) and every type entry must
    /// belong to a walked expression. With `IRIS_TYPECK=off` there are no
    /// checker types, so only the coverage and resolution checks apply.
    pub(crate) fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if self.typed_by_checker {
            for span in &self.seen {
                if self.type_at(Span::new(span.0, span.1)).is_none() {
                    problems.push(format!(
                        "expression at bytes {span:?} has no HIR type entry"
                    ));
                }
            }
        }
        for span in self.types.keys() {
            if !self.seen.contains(span) {
                problems.push(format!(
                    "checker typed an expression at bytes {span:?} that the HIR walk did not register"
                ));
            }
        }
        for (span, r) in &self.resolutions {
            if r.is_unknown() {
                problems.push(format!("unresolved value name at bytes {span:?}"));
            }
        }
        for c in self.calls() {
            if c.target.is_unknown() {
                problems.push(format!(
                    "call to unknown `{}` ({} argument{}) at bytes ({}, {})",
                    c.name,
                    c.argc,
                    if c.argc == 1 { "" } else { "s" },
                    c.span.start.0,
                    c.span.end.0
                ));
            }
            // `resolve_method` is a catch-all: a method may be a builtin, a
            // trait method, or an extension. This guards the shape only;
            // method-name errors belong to the checker and lowering.
            if c.method && !matches!(c.target, Resolution::Method(_)) {
                problems.push(format!(
                    "method call `{}` at bytes ({}, {}) did not resolve to a method",
                    c.name, c.span.start.0, c.span.end.0
                ));
            }
        }
        if self.stats.functions > 0 && self.seen.is_empty() {
            problems.push("no expressions were walked".to_string());
        }
        problems
    }

    /// `IRIS_HIR=report` output.
    pub(crate) fn report(&self, module: &str) {
        let s = self.stats();
        eprintln!(
            "hir {module}: {} functions, {} impls, {} consts, {} state vars, {} macros",
            s.functions, s.impls, s.consts, s.state_vars, s.macros
        );
        eprintln!(
            "hir {module}: {} expressions ({} known, {} partial, {} open)",
            s.expressions, s.typed, s.partial, s.open
        );
        eprintln!(
            "hir {module}: {} names ({} local, {} fn, {} builtin, {} const, {} state, {} type, {} method, {} effect, {} variant, {} module), {} unresolved",
            s.names,
            s.local_names,
            s.function_names,
            s.builtin_names,
            s.const_names,
            s.state_names,
            s.type_names,
            s.method_names,
            s.effect_names,
            s.variant_names,
            s.module_names,
            s.unresolved_names
        );
        eprintln!(
            "hir {module}: {} calls ({} function, {} builtin, {} method), {} unresolved",
            s.calls, s.function_calls, s.builtin_calls, s.method_calls, s.unknown_calls
        );
    }
}

// ── The lowering-facing context ───────────────────────────────────────────

/// The part of the HIR lowering can still read while it walks a body. Type
/// hints are normally served by THIR; this context remains the fallback for
/// lowering-synthesized spans and the source of solved `-> Infer` returns. It
/// is installed once per compile (see [`install`]) so the deep, generic-heavy
/// lowering path does not have to thread a borrowed module through every
/// helper -- the same reason the checker used thread-local hints before HIR
/// existed.
#[derive(Clone, Default)]
struct HirContext {
    hints: HashMap<SpanKey, Hint>,
    returns: HashMap<String, IrType>,
}

thread_local! {
    static CONTEXT: std::cell::RefCell<Option<HirContext>> =
        const { std::cell::RefCell::new(None) };
}

/// The last compile's HIR statistics, for tests. A compile may run on a
/// spawned thread, so this is process-wide rather than thread-local.
static LAST_STATS: Mutex<Option<HirStats>> = Mutex::new(None);

/// Make `module` the HIR lowering reads. Replaces any previous compile's.
pub(crate) fn install(module: &HirModule<'_>) {
    let ctx = HirContext {
        hints: module.hints.clone(),
        returns: module.returns.clone(),
    };
    CONTEXT.with(|c| *c.borrow_mut() = Some(ctx));
    if let Ok(mut last) = LAST_STATS.lock() {
        *last = Some(module.stats.clone());
    }
}

/// Forget the previous compile's HIR (called at the start of lowering).
pub(crate) fn clear() {
    CONTEXT.with(|c| *c.borrow_mut() = None);
}

/// The HIR's solved hint for the expression at `span`. Lowering asks THIR
/// first; this is the fallback for spans THIR never saw.
pub(crate) fn hint(span: Span) -> Option<Hint> {
    CONTEXT.with(|c| {
        c.borrow()
            .as_ref()
            .and_then(|ctx| ctx.hints.get(&key(span)).cloned())
    })
}

/// The HIR's solved `-> Infer` return for `name`.
pub(crate) fn return_type(name: &str) -> Option<IrType> {
    CONTEXT.with(|c| {
        c.borrow()
            .as_ref()
            .and_then(|ctx| ctx.returns.get(name).cloned())
    })
}

/// Statistics of the last compile's HIR on this process, if one has run.
pub fn last_stats() -> Option<HirStats> {
    LAST_STATS.lock().ok().and_then(|s| s.clone())
}

/// `IRIS_HIR`: `report` prints the HIR, `check` also fails an incomplete one.
pub(crate) fn mode() -> Option<String> {
    std::env::var("IRIS_HIR")
        .ok()
        .filter(|m| m == "report" || m == "check")
}

/// The error `IRIS_HIR=check` reports for an incomplete HIR.
pub(crate) fn incomplete(problems: Vec<String>) -> crate::error::LowerError {
    let detail = problems.join("; ");
    crate::error::LowerError::rich(
        "E0901",
        crate::diagnostics::Diagnostic::error(format!(
            "internal compiler error: incomplete HIR: {detail}"
        ))
        .with_code("E0901"),
    )
}
