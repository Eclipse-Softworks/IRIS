//! Source-level checks that need scopes, run on the AST before lowering:
//!
//! * a `val` binding is never assigned again (E0112) -- the lowerer treats
//!   every binding as mutable, so this was not enforced at all;
//! * every type name exists (E0111) -- an unknown name used to lower to an
//!   empty record, so `val x: integer = 1` compiled;
//! * an `if` without `else` is not used as a value (E0119);
//! * a function with a return type returns on every path (E0116).
//!
//! Errors carry spans and, where the intent is clear, a fix.

use std::collections::{HashMap, HashSet};

use crate::diagnostics::{Diagnostic, Fix};
use crate::lower::hints;
use crate::parser::ast::{
    AstBlock, AstExpr, AstFunction, AstGenericParam, AstModule, AstStmt, AstType,
};
use crate::parser::lexer::Span;

struct Binding {
    mutable: bool,
    decl: Span,
}

pub struct BindingChecker {
    known_types: HashSet<String>,
    known_consts: HashSet<String>,
    scopes: Vec<HashMap<String, Binding>>,
    /// The function being checked and its declared return type, when it
    /// must return a value.
    returning: Option<(String, String)>,
    pub errors: Vec<(&'static str, Diagnostic)>,
}

/// Type names that are always defined.
const ALWAYS_KNOWN: &[&str] = &[
    "Self",
    "self",
    "Infer",
    "task_group",
    "any",
    "never",
    "TaskGroup",
];

impl BindingChecker {
    pub fn new(module: &AstModule) -> Self {
        let mut known: HashSet<String> =
            hints::BUILTIN_TYPES.iter().map(|s| s.to_string()).collect();
        known.extend(ALWAYS_KNOWN.iter().map(|s| s.to_string()));
        for s in &module.structs {
            known.insert(s.name.name.clone());
            add_generic_names(&s.type_params, &mut known);
        }
        for e in &module.enums {
            known.insert(e.name.name.clone());
        }
        for a in &module.type_aliases {
            known.insert(a.name.clone());
        }
        for t in &module.traits {
            known.insert(t.name.name.clone());
            add_generic_names(&t.type_params, &mut known);
        }
        for i in &module.impls {
            add_generic_names(&i.generic_params, &mut known);
            for (name, _) in &i.assoc_type_bindings {
                known.insert(name.clone());
            }
            for m in &i.methods {
                add_generic_names(&m.type_params, &mut known);
            }
        }
        for f in &module.functions {
            add_generic_names(&f.type_params, &mut known);
        }
        for e in &module.effects {
            known.insert(e.name.name.clone());
        }
        // Brought modules are merged with their names prefixed by the module
        // (`net__TcpListener`); the bringing file writes them unprefixed.
        let unprefixed: Vec<String> = known
            .iter()
            .filter_map(|n| n.rsplit_once("__").map(|(_, short)| short.to_string()))
            .collect();
        known.extend(unprefixed);
        let mut known_consts: HashSet<String> =
            module.consts.iter().map(|c| c.name.name.clone()).collect();
        let unprefixed_consts: Vec<String> = known_consts
            .iter()
            .filter_map(|n| n.rsplit_once("__").map(|(_, short)| short.to_string()))
            .collect();
        known_consts.extend(unprefixed_consts);
        BindingChecker {
            known_types: known,
            known_consts,
            scopes: Vec::new(),
            returning: None,
            errors: Vec::new(),
        }
    }

    pub fn check_module(&mut self, module: &AstModule) {
        for bring in &module.brings {
            if let crate::parser::ast::BringPath::Stdlib(name) = &bring.path {
                let module_name = name.split('.').next().unwrap_or(name);
                if crate::stdlib::stdlib_source(module_name).is_none() {
                    self.unknown_module(module_name, bring.span);
                }
            }
        }
        for s in &module.structs {
            for f in &s.fields {
                self.check_type(&f.ty);
            }
        }
        for e in &module.enums {
            for v in &e.variants {
                for t in &v.fields {
                    self.check_type(t);
                }
            }
        }
        for a in &module.type_aliases {
            self.check_type(&a.ty);
        }
        for f in &module.functions {
            self.check_function(f);
        }
        for i in &module.impls {
            for m in &i.methods {
                self.check_function(m);
            }
        }
    }

    fn check_function(&mut self, f: &AstFunction) {
        for p in &f.params {
            self.check_type(&p.ty);
        }
        self.check_type(&f.return_ty);
        self.returning = (returns_value(&f.return_ty) && !f.is_async)
            .then(|| (f.name.name.clone(), crate::lsp::ast_type_str(&f.return_ty)));
        self.scopes.clear();
        self.scopes.push(HashMap::new());
        self.block(&f.body);
        self.scopes.clear();
        // A body with no final expression is checked exactly on the control
        // flow graph after lowering. What that check cannot see is a final
        // `if` without `else`: it lowers to a value of its own, so the
        // function seemed to end in one.
        if returns_value(&f.return_ty)
            && !f.is_async
            && matches!(
                f.body.tail.as_deref(),
                Some(AstExpr::If {
                    else_block: None,
                    ..
                })
            )
            && !f.body.stmts.iter().any(stmt_leaves)
        {
            self.missing_return(f);
        }
    }

    // ── type names ─────────────────────────────────────────────────────

    fn check_type(&mut self, ty: &AstType) {
        match ty {
            AstType::Named(name, span) => self.check_type_name(name, *span),
            AstType::Generic { name, args, span } => {
                self.check_type_name(name, *span);
                for a in args {
                    if let AstType::Named(arg_name, _) = a {
                        if self.known_consts.contains(arg_name) {
                            continue;
                        }
                    }
                    self.check_type(a);
                }
            }
            AstType::Option(inner, _)
            | AstType::Chan(inner, _)
            | AstType::Atomic(inner, _)
            | AstType::Mutex(inner, _)
            | AstType::Grad(inner, _)
            | AstType::Sparse(inner, _)
            | AstType::List(inner, _)
            | AstType::Slice(inner, _)
            | AstType::WeakRef(inner, _)
            | AstType::Ref(inner, _)
            | AstType::RefMut(inner, _) => self.check_type(inner),
            AstType::Result(ok, err, _) => {
                self.check_type(ok);
                self.check_type(err);
            }
            AstType::Map(k, v, _) => {
                self.check_type(k);
                self.check_type(v);
            }
            AstType::Tuple(elems, _) => {
                for e in elems {
                    self.check_type(e);
                }
            }
            AstType::Fn { params, ret, .. } => {
                for p in params {
                    self.check_type(p);
                }
                self.check_type(ret);
            }
            AstType::Array { elem, .. } => self.check_type(elem),
            _ => {}
        }
    }

    fn check_type_name(&mut self, name: &str, span: Span) {
        // Qualified names (`json.Value`) and single capital letters (generic
        // parameters of a scope this pass does not track) are left alone.
        if self.known_types.contains(name)
            || name.contains('.')
            || name.contains("::")
            || name.contains("__")
            || (name.len() == 1 && name.chars().all(|c| c.is_ascii_uppercase()))
        {
            return;
        }
        let mut diag = Diagnostic::error(format!("unknown type `{name}`")).with_primary_span(
            span.start.0,
            span.end.0,
            Some("not a type".into()),
        );
        let suggestion = hints::type_alias(name)
            .map(str::to_string)
            .or_else(|| hints::closest(name, self.known_types.iter().map(String::as_str)));
        match suggestion {
            Some(s) if s == "list" || s == "map" || s == "option" || s == "result" => {
                diag = diag.with_help(format!("IRIS writes this as `{s}<...>`"));
            }
            Some(s) => {
                diag = diag.with_fix(Fix::replace(
                    format!("use `{s}`"),
                    span.start.0,
                    span.end.0,
                    s,
                ));
            }
            None => {
                diag = diag.with_help("the built-in types are i64, f64, bool, str, unit, list<T>, map<K, V>, option<T> and result<T, E>; records, choices and type aliases must be declared");
            }
        }
        self.errors.push(("E0111", diag));
    }

    /// `bring std.name` where there is no such module.
    fn unknown_module(&mut self, name: &str, bring: Span) {
        // The module name ends the `bring` (selective items aside).
        let at_end = bring.end.0.saturating_sub(name.len() as u32);
        let path_start = at_end.saturating_sub(4); // `std.`
        let mut diag = Diagnostic::error(format!("there is no module `std.{name}`"));
        diag = diag.with_primary_span(path_start, bring.end.0, None);
        match hints::closest(name, crate::stdlib::STDLIB_MODULE_NAMES.iter().copied()) {
            Some(near) => {
                diag = diag.with_fix(Fix::replace(
                    format!("did you mean `std.{near}`?"),
                    at_end,
                    bring.end.0,
                    near,
                ));
            }
            None => {
                diag = diag.with_help("docs/stdlib-reference.md lists the standard modules");
            }
        }
        self.errors.push(("E0115", diag));
    }

    // ── bindings ───────────────────────────────────────────────────────

    fn bind(&mut self, name: &str, mutable: bool, decl: Span) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string(), Binding { mutable, decl });
        }
    }

    fn lookup(&self, name: &str) -> Option<&Binding> {
        self.scopes.iter().rev().find_map(|s| s.get(name))
    }

    fn block(&mut self, b: &AstBlock) {
        self.scopes.push(HashMap::new());
        for s in &b.stmts {
            self.stmt(s);
        }
        if let Some(t) = &b.tail {
            self.expr(t);
        }
        self.scopes.pop();
    }

    fn stmt(&mut self, s: &AstStmt) {
        match s {
            AstStmt::Let {
                name,
                ty,
                init,
                is_var,
                span,
            } => {
                if let Some(t) = ty {
                    self.check_type(t);
                }
                self.value(init);
                self.bind(&name.name, *is_var, *span);
            }
            AstStmt::LetTuple {
                names,
                init,
                is_var,
                span,
                ..
            } => {
                self.value(init);
                for n in names {
                    self.bind(&n.name, *is_var, *span);
                }
            }
            AstStmt::Assign {
                target,
                value,
                span,
                ..
            } => {
                self.value(value);
                if let AstExpr::Ident(id) = target.as_ref() {
                    if let Some(b) = self.lookup(&id.name) {
                        if !b.mutable {
                            let decl = b.decl;
                            self.errors.push((
                                "E0112",
                                Diagnostic::error(format!(
                                    "cannot assign to `{}`: it was declared with `val`",
                                    id.name
                                ))
                                .with_primary_span(
                                    span.start.0,
                                    span.end.0,
                                    Some("assigned here".into()),
                                )
                                .with_secondary_span(
                                    decl.start.0,
                                    decl.end.0,
                                    Some("declared here".into()),
                                )
                                .with_fix(Fix::replace(
                                    format!("make `{}` a `var` so it can change", id.name),
                                    decl.start.0,
                                    decl.start.0 + 3,
                                    "var",
                                )),
                            ));
                        }
                    }
                } else {
                    self.expr(target);
                }
            }
            AstStmt::Expr(e) => self.expr(e),
            AstStmt::Return { value, span } => match value {
                Some(v) => self.value(v),
                None => {
                    if let Some((func, ret)) = self.returning.clone() {
                        self.errors.push((
                            "E0209",
                            Diagnostic::error(format!(
                                "`return` needs a value: `{func}` returns `{ret}`"
                            ))
                            .with_primary_span(
                                span.start.0,
                                span.end.0,
                                None,
                            ),
                        ));
                    }
                }
            },
            AstStmt::While { cond, body, .. } => {
                self.expr(cond);
                self.block(body);
            }
            AstStmt::Loop { body, .. } => self.block(body),
            AstStmt::ForRange {
                var,
                start,
                end,
                body,
                ..
            } => {
                self.expr(start);
                self.expr(end);
                self.scopes.push(HashMap::new());
                self.bind(&var.name, true, var.span);
                self.block(body);
                self.scopes.pop();
            }
            AstStmt::ForEach { iter, body, .. } => {
                self.expr(iter);
                // The loop variables are bound by the lowerer; treat them as
                // unknown (neither val nor var) here.
                self.scopes.push(HashMap::new());
                self.block(body);
                self.scopes.pop();
            }
            AstStmt::ParFor {
                start, end, body, ..
            } => {
                self.expr(start);
                self.expr(end);
                self.block(body);
            }
            AstStmt::Spawn { body, group, .. } => {
                self.scopes.push(HashMap::new());
                for s in body {
                    self.stmt(s);
                }
                self.scopes.pop();
                if let Some(g) = group {
                    self.expr(g);
                }
            }
            AstStmt::MaskStmt { body, .. } => self.block(body),
            AstStmt::HandleStmt { expr, arms, .. } => {
                self.expr(expr);
                for arm in arms {
                    self.expr(&arm.body);
                }
            }
            AstStmt::Defer { expr, .. } | AstStmt::Yield { expr, .. } => self.expr(expr),
            AstStmt::Select { arms, default, .. } => {
                for arm in arms {
                    self.expr(&arm.channel);
                    self.block(&arm.body);
                }
                if let Some(d) = default {
                    self.block(d);
                }
            }
            AstStmt::Break { .. } | AstStmt::Continue { .. } => {}
        }
    }

    /// An expression whose value is used: an `if` must have an `else`.
    fn value(&mut self, e: &AstExpr) {
        if let AstExpr::If {
            else_block: None,
            span,
            ..
        } = e
        {
            self.errors.push((
                "E0119",
                Diagnostic::error(
                    "this `if` has no `else`, so it has no value when its condition is false",
                )
                .with_primary_span(span.start.0, span.end.0, None)
                .with_help("add an `else` branch with the value for the other case"),
            ));
        }
        self.expr(e);
    }

    fn expr(&mut self, e: &AstExpr) {
        match e {
            AstExpr::BinOp { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            AstExpr::UnaryOp { expr, .. }
            | AstExpr::Ref { expr, .. }
            | AstExpr::RefMut { expr, .. }
            | AstExpr::Deref { expr, .. }
            | AstExpr::Move { expr, .. }
            | AstExpr::Await { expr, .. }
            | AstExpr::Try { expr, .. }
            | AstExpr::Splat { expr, .. } => self.expr(expr),
            AstExpr::Cast { expr, ty, .. } => {
                self.expr(expr);
                self.check_type(ty);
            }
            AstExpr::Call {
                args, named_args, ..
            } => {
                for a in args {
                    self.expr(a);
                }
                for (_, a) in named_args {
                    self.expr(a);
                }
            }
            AstExpr::MethodCall { base, args, .. } => {
                self.expr(base);
                for a in args {
                    self.expr(a);
                }
            }
            AstExpr::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                self.expr(cond);
                self.block(then_block);
                if let Some(b) = else_block {
                    self.block(b);
                }
            }
            AstExpr::Block(b) => self.block(b),
            AstExpr::Mask { body, .. } => self.block(body),
            AstExpr::Lambda { params, body, .. } => {
                self.scopes.push(HashMap::new());
                for p in params {
                    self.check_type(&p.ty);
                }
                self.expr(body);
                self.scopes.pop();
            }
            AstExpr::Index { base, indices, .. } => {
                self.expr(base);
                for i in indices {
                    self.expr(i);
                }
            }
            AstExpr::StructLit { fields, spread, .. } => {
                for (_, v) in fields {
                    self.expr(v);
                }
                if let Some(s) = spread {
                    self.expr(s);
                }
            }
            AstExpr::FieldAccess { base, .. } | AstExpr::TupleIndex { base, .. } => self.expr(base),
            AstExpr::When {
                scrutinee, arms, ..
            } => {
                self.expr(scrutinee);
                for arm in arms {
                    self.scopes.push(HashMap::new());
                    self.expr(&arm.body);
                    self.scopes.pop();
                }
            }
            AstExpr::Tuple { elements, .. } => {
                for x in elements {
                    self.expr(x);
                }
            }
            AstExpr::ArrayLit { elems, .. } => {
                for x in elems {
                    self.expr(x);
                }
            }
            AstExpr::MapLiteral { entries, .. } => {
                for (k, v) in entries {
                    self.expr(k);
                    self.expr(v);
                }
            }
            AstExpr::Handle { expr, arms, .. } => {
                self.expr(expr);
                for arm in arms {
                    self.expr(&arm.body);
                }
            }
            AstExpr::NullCoal { expr, default, .. } => {
                self.expr(expr);
                self.expr(default);
            }
            AstExpr::TryCatch {
                body, catch_body, ..
            } => {
                self.expr(body);
                self.expr(catch_body);
            }
            AstExpr::Raise { args, .. } | AstExpr::MacroCall { args, .. } => {
                for a in args {
                    self.expr(a);
                }
            }
            AstExpr::Unsafe { body, .. } => self.expr(body),
            AstExpr::Ident(_)
            | AstExpr::IntLit { .. }
            | AstExpr::FloatLit { .. }
            | AstExpr::BoolLit { .. }
            | AstExpr::StringLit { .. } => {}
        }
    }

    // ── returns ────────────────────────────────────────────────────────

    fn missing_return(&mut self, f: &AstFunction) {
        let ret = crate::lsp::ast_type_str(&f.return_ty);
        let mut diag = Diagnostic::error(format!(
            "`{}` may end without returning a value: it returns `{ret}`",
            f.name.name
        ))
        .with_primary_span(f.name.span.start.0, f.name.span.end.0, None);
        if let Some(AstExpr::If {
            span,
            else_block: None,
            ..
        }) = f.body.tail.as_deref()
        {
            diag = diag.with_secondary_span(
                span.start.0,
                span.end.0,
                Some("this `if` has no `else`: when its condition is false, the function reaches its end".into()),
            );
        }
        diag = diag.with_help(format!(
            "return a `{ret}` on every path: add a final `return`, or an `else` branch"
        ));
        self.errors.push(("E0116", diag));
    }
}

fn add_generic_names(params: &[AstGenericParam], known: &mut HashSet<String>) {
    for p in params {
        match p {
            AstGenericParam::Type(name, _, _) => {
                known.insert(name.clone());
            }
            AstGenericParam::Hkt(name, inner, _, _) => {
                known.insert(name.clone());
                add_generic_names(inner, known);
            }
            AstGenericParam::Const { name, .. } => {
                known.insert(name.clone());
            }
        }
    }
}

/// Whether a function declared with this return type must produce a value.
fn returns_value(ty: &AstType) -> bool {
    match ty {
        AstType::Named(n, _) => !matches!(n.as_str(), "unit" | "Infer" | "never"),
        AstType::Tuple(elems, _) => !elems.is_empty(),
        _ => true,
    }
}

/// `b` always leaves the function (returns, panics, loops forever).
fn block_leaves(b: &AstBlock) -> bool {
    b.stmts.iter().any(stmt_leaves) || b.tail.as_deref().is_some_and(expr_leaves)
}

fn stmt_leaves(s: &AstStmt) -> bool {
    match s {
        AstStmt::Return { .. } => true,
        AstStmt::Expr(e) => expr_leaves(e),
        AstStmt::Loop { body, .. } => !contains_break(body),
        AstStmt::While { cond, body, .. } => {
            matches!(cond.as_ref(), AstExpr::BoolLit { value: true, .. }) && !contains_break(body)
        }
        _ => false,
    }
}

fn expr_leaves(e: &AstExpr) -> bool {
    match e {
        AstExpr::Call { callee, .. } => matches!(
            callee.name.as_str(),
            "panic" | "exit" | "unreachable" | "fail" | "abort"
        ),
        AstExpr::Raise { .. } => true,
        AstExpr::If {
            then_block,
            else_block: Some(else_block),
            ..
        } => block_leaves(then_block) && block_leaves(else_block),
        AstExpr::Block(b) => block_leaves(b),
        // A `when` that is used as a statement: assume its arms are total.
        AstExpr::When { arms, .. } => !arms.is_empty() && arms.iter().all(|a| expr_leaves(&a.body)),
        _ => false,
    }
}

/// Whether a `break` (of any loop, conservatively) appears in `b`.
fn contains_break(b: &AstBlock) -> bool {
    fn s(st: &AstStmt) -> bool {
        match st {
            AstStmt::Break { .. } => true,
            AstStmt::Expr(e) => x(e),
            AstStmt::While { body, .. }
            | AstStmt::Loop { body, .. }
            | AstStmt::ForRange { body, .. }
            | AstStmt::ForEach { body, .. } => contains_break(body),
            _ => false,
        }
    }
    fn x(e: &AstExpr) -> bool {
        match e {
            AstExpr::If {
                then_block,
                else_block,
                ..
            } => contains_break(then_block) || else_block.as_ref().is_some_and(contains_break),
            AstExpr::Block(b) => contains_break(b),
            AstExpr::When { arms, .. } => arms.iter().any(|a| x(&a.body)),
            _ => false,
        }
    }
    b.stmts.iter().any(s) || b.tail.as_deref().is_some_and(x)
}
