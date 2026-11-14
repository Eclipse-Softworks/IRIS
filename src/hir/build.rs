//! AST -> HIR: resolve every value name, register every expression.
//!
//! This is a full walk, not a sample: a function, impl method, constant,
//! state initialiser, record field default, macro body, or model layer
//! parameter the walk misses is a compile bug, which `IRIS_HIR=check`
//! reports as an incomplete HIR.

use std::collections::HashMap;

use crate::lower::typeck::{self, Registries};
use crate::parser::ast::{
    AstBlock, AstExpr, AstFunction, AstHandlerArm, AstModule, AstStmt, AstWhenPattern,
};
use crate::parser::lexer::Span;

use super::{key, CallSite, HirModule, HirStats, Resolution};

pub(crate) fn build<'a>(ast: &'a AstModule, reg: &Registries<'_>) -> HirModule<'a> {
    let mut b = Builder {
        reg,
        scopes: Vec::new(),
        module: HirModule {
            source: ast,
            seen: Default::default(),
            types: Default::default(),
            hints: Default::default(),
            returns: Default::default(),
            resolutions: Default::default(),
            calls: Vec::new(),
            typed_by_checker: false,
            stats: HirStats {
                functions: ast.functions.len()
                    + ast.impls.iter().map(|i| i.methods.len()).sum::<usize>(),
                impls: ast.impls.len(),
                consts: ast.consts.len(),
                state_vars: ast.state_vars.len(),
                macros: ast.macros.len(),
                ..HirStats::default()
            },
        },
    };

    // Constants are checked in an empty scope; their initialisers are
    // expressions of the module like any other.
    for c in &ast.consts {
        b.expr(&c.value);
    }
    for s in &ast.state_vars {
        b.expr(&s.init);
    }
    for record in &ast.structs {
        for field in &record.fields {
            if let Some(default) = &field.default {
                b.expr(default);
            }
        }
    }
    for f in &ast.functions {
        b.function(f);
    }
    for imp in &ast.impls {
        for m in &imp.methods {
            b.function(m);
        }
    }
    for m in &ast.macros {
        b.push_scope();
        for p in &m.params {
            b.bind(p);
        }
        b.expr(&m.body);
        b.pop_scope();
    }
    for model in &ast.models {
        for layer in &model.layers {
            for param in &layer.params {
                b.expr(&param.value);
            }
        }
    }

    b.module.recount();
    b.module
}

struct Builder<'a, 'r> {
    reg: &'r Registries<'r>,
    /// Lexical value scopes, innermost last.
    scopes: Vec<HashMap<String, ()>>,
    module: HirModule<'a>,
}

impl<'a, 'r> Builder<'a, 'r> {
    // ── Scopes ─────────────────────────────────────────────────────────────

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn bind(&mut self, name: &str) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string(), ());
        }
    }

    fn is_local(&self, name: &str) -> bool {
        self.scopes.iter().rev().any(|s| s.contains_key(name))
    }

    fn saw(&mut self, span: Span) {
        self.module.saw(span);
    }

    fn resolve_at(&mut self, span: Span, resolution: Resolution) {
        self.module.resolutions.insert(key(span), resolution);
    }

    // ── Resolution ─────────────────────────────────────────────────────────

    /// A name used as a value: a local, `none`, a state slot, a constant, a
    /// function, a type, or a built-in.
    fn resolve_value(&self, name: &str) -> Resolution {
        if self.is_local(name) {
            return Resolution::Local;
        }
        if name == "none" {
            return Resolution::Builtin(name.to_string());
        }
        if self.reg.module.state_slot(name).is_some() {
            return Resolution::State(name.to_string());
        }
        if crate::lower::is_bring_qualifier(name) {
            return Resolution::Module(name.to_string());
        }
        let resolved = typeck::resolve_known_name(self.reg, name);
        self.classify(&resolved, name)
    }

    /// A callee: built-ins win over locals in call position, as they do in
    /// lowering (known-issue #15); otherwise a closure in scope, a function
    /// of the program, or a constructor.
    fn resolve_callee(&self, name: &str) -> Resolution {
        if typeck::is_builtin_call_name(name) {
            return Resolution::Builtin(name.to_string());
        }
        if self.is_local(name) {
            return Resolution::Local;
        }
        if crate::lower::is_bring_qualifier(name) {
            return Resolution::Module(name.to_string());
        }
        let resolved = typeck::resolve_known_name(self.reg, name);
        self.classify(&resolved, name)
    }

    fn classify(&self, resolved: &str, written: &str) -> Resolution {
        if self.reg.const_defs.contains_key(resolved) {
            return Resolution::Const(resolved.to_string());
        }
        if self.reg.fn_sigs.contains_key(resolved)
            || self.reg.generic_fns.contains_key(resolved)
            || self.reg.inferred_fns.contains(resolved)
        {
            return Resolution::Fn(resolved.to_string());
        }
        if self.reg.module.struct_def(resolved).is_some()
            || self.reg.module.enum_def(resolved).is_some()
            || self.reg.module.type_alias(resolved).is_some()
        {
            return Resolution::Type(resolved.to_string());
        }
        if typeck::is_builtin_value_name(written) {
            return Resolution::Builtin(written.to_string());
        }
        Resolution::Unknown
    }

    fn resolve_type_name(&self, name: &str) -> Resolution {
        let resolved = crate::lower::resolve_brought_name(name, self.reg.module);
        if self.reg.module.struct_def(&resolved).is_some()
            || self.reg.module.enum_def(&resolved).is_some()
            || self.reg.module.type_alias(&resolved).is_some()
        {
            Resolution::Type(resolved)
        } else {
            Resolution::Unknown
        }
    }

    /// A method name, to the mangled `Type__method` the registries know when
    /// exactly one impl declares it.
    fn resolve_method(&self, name: &str) -> Resolution {
        let suffix = format!("__{name}");
        let mut matches: Vec<&String> = self
            .reg
            .method_param_types
            .keys()
            .filter(|k| k.as_str().ends_with(&suffix))
            .collect();
        matches.sort();
        matches.dedup();
        match matches.as_slice() {
            [only] => Resolution::Method((*only).clone()),
            _ => Resolution::Method(name.to_string()),
        }
    }

    // ── Functions and blocks ───────────────────────────────────────────────

    fn function(&mut self, f: &AstFunction) {
        self.push_scope();
        // Attribute arguments (`@adaptive(0.01)`) are expressions of the
        // item; they are walked so none escapes typing.
        for attr in &f.attrs {
            for a in &attr.args {
                self.expr(a);
            }
        }
        // Defaults are lowered at call sites; they are not in the function's
        // value scope.
        for p in &f.params {
            if let Some(default) = &p.default {
                self.expr(default);
            }
        }
        for p in &f.params {
            self.bind(&p.name.name);
        }
        self.block(&f.body);
        self.pop_scope();
    }

    fn block(&mut self, b: &AstBlock) {
        self.push_scope();
        for s in &b.stmts {
            self.stmt(s);
        }
        if let Some(tail) = &b.tail {
            self.expr(tail);
        }
        self.pop_scope();
    }

    fn stmts(&mut self, stmts: &[AstStmt]) {
        self.push_scope();
        for s in stmts {
            self.stmt(s);
        }
        self.pop_scope();
    }

    fn stmt(&mut self, s: &AstStmt) {
        match s {
            AstStmt::Let { name, init, .. } => {
                self.expr(init);
                self.bind(&name.name);
            }
            AstStmt::LetTuple { names, init, .. } => {
                self.expr(init);
                for n in names {
                    self.bind(&n.name);
                }
            }
            AstStmt::Expr(e) => self.expr(e),
            AstStmt::While { cond, body, .. } => {
                self.expr(cond);
                self.block(body);
            }
            AstStmt::Loop { body, .. } => self.block(body),
            AstStmt::Break { .. } | AstStmt::Continue { .. } => {}
            AstStmt::ForRange {
                var,
                start,
                end,
                step,
                body,
                ..
            } => {
                self.expr(start);
                self.expr(end);
                if let Some(step) = step {
                    self.expr(step);
                }
                self.push_scope();
                self.bind(&var.name);
                self.block(body);
                self.pop_scope();
            }
            AstStmt::Assign { target, value, .. } => {
                self.expr(target);
                self.expr(value);
            }
            AstStmt::Return { value, .. } => {
                if let Some(v) = value {
                    self.expr(v);
                }
            }
            AstStmt::Spawn { body, group, .. } => {
                if let Some(g) = group {
                    self.expr(g);
                }
                self.stmts(body);
            }
            AstStmt::ParFor {
                var,
                start,
                end,
                body,
                ..
            } => {
                self.expr(start);
                self.expr(end);
                self.push_scope();
                self.bind(&var.name);
                self.block(body);
                self.pop_scope();
            }
            AstStmt::ForEach {
                var, iter, body, ..
            } => {
                self.expr(iter);
                self.push_scope();
                self.bind(&var.name);
                self.block(body);
                self.pop_scope();
            }
            AstStmt::MaskStmt { body, .. } => self.block(body),
            AstStmt::HandleStmt { expr, arms, .. } => {
                self.expr(expr);
                self.handler_arms(arms);
            }
            AstStmt::Defer { expr, .. } => self.expr(expr),
            AstStmt::Select { arms, default, .. } => {
                for arm in arms {
                    self.expr(&arm.channel);
                    self.push_scope();
                    self.bind(&arm.binding);
                    self.block(&arm.body);
                    self.pop_scope();
                }
                if let Some(default) = default {
                    self.block(default);
                }
            }
            AstStmt::Yield { expr, .. } => self.expr(expr),
        }
    }

    fn handler_arms(&mut self, arms: &[AstHandlerArm]) {
        for arm in arms {
            self.push_scope();
            for p in &arm.params {
                self.bind(&p.name);
            }
            if let Some(r) = &arm.resume_param {
                self.bind(&r.name);
            }
            self.expr(&arm.body);
            self.pop_scope();
        }
    }

    // ── Expressions ────────────────────────────────────────────────────────

    fn expr(&mut self, e: &AstExpr) {
        self.saw(e.span());
        match e {
            AstExpr::Ident(id) => {
                let r = self.resolve_value(&id.name);
                self.resolve_at(id.span, r);
            }
            AstExpr::IntLit { .. }
            | AstExpr::FloatLit { .. }
            | AstExpr::BoolLit { .. }
            | AstExpr::StringLit { .. } => {}
            AstExpr::BinOp { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            AstExpr::Call {
                callee,
                args,
                named_args,
                span,
            } => {
                let target = self.resolve_callee(&callee.name);
                self.resolve_at(callee.span, target.clone());
                self.module.calls.push(CallSite {
                    span: *span,
                    name: callee.name.clone(),
                    target,
                    argc: args.len() + named_args.len(),
                    method: false,
                });
                for a in args {
                    self.expr(a);
                }
                for (_, a) in named_args {
                    self.expr(a);
                }
            }
            AstExpr::UnaryOp { expr, .. } => self.expr(expr),
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
            AstExpr::Index { base, indices, .. } => {
                self.expr(base);
                for i in indices {
                    self.expr(i);
                }
            }
            AstExpr::Cast { expr, .. } => self.expr(expr),
            AstExpr::StructLit {
                name,
                fields,
                spread,
                span,
                ..
            } => {
                let r = self.resolve_type_name(name);
                self.resolve_at(*span, r);
                for (_, e) in fields {
                    self.expr(e);
                }
                if let Some(s) = spread {
                    self.expr(s);
                }
            }
            AstExpr::FieldAccess { base, .. } => self.expr(base),
            AstExpr::When {
                scrutinee, arms, ..
            } => {
                self.expr(scrutinee);
                for arm in arms {
                    // `Choice.Variant` in a pattern is a name use. The arm
                    // span is the only span the pattern carries.
                    if let AstWhenPattern::EnumVariant {
                        enum_name,
                        variant_name,
                        ..
                    } = &arm.pattern
                    {
                        if !enum_name.is_empty() {
                            self.resolve_at(
                                arm.span,
                                Resolution::Variant {
                                    choice: enum_name.clone(),
                                    variant: variant_name.clone(),
                                },
                            );
                        }
                    }
                    self.push_scope();
                    self.bind_pattern(&arm.pattern);
                    if let Some(g) = &arm.guard {
                        self.expr(g);
                    }
                    self.expr(&arm.body);
                    self.pop_scope();
                }
            }
            AstExpr::Tuple { elements, .. } => {
                for e in elements {
                    self.expr(e);
                }
            }
            AstExpr::TupleIndex { base, .. } => self.expr(base),
            AstExpr::ArrayLit { elems, .. } => {
                for e in elems {
                    self.expr(e);
                }
            }
            AstExpr::Lambda { params, body, .. } => {
                for p in params {
                    if let Some(default) = &p.default {
                        self.expr(default);
                    }
                }
                self.push_scope();
                for p in params {
                    self.bind(&p.name.name);
                }
                self.expr(body);
                self.pop_scope();
            }
            AstExpr::Await { expr, .. } | AstExpr::Try { expr, .. } => self.expr(expr),
            AstExpr::NullCoal { expr, default, .. } => {
                self.expr(expr);
                self.expr(default);
            }
            AstExpr::MethodCall {
                base,
                method,
                args,
                span,
            } => {
                self.expr(base);
                for a in args {
                    self.expr(a);
                }
                self.module.calls.push(CallSite {
                    span: *span,
                    name: method.clone(),
                    target: self.resolve_method(method),
                    argc: args.len(),
                    method: true,
                });
            }
            AstExpr::Mask { body, .. } => self.block(body),
            AstExpr::Handle { expr, arms, .. } => {
                self.expr(expr);
                self.handler_arms(arms);
            }
            AstExpr::MapLiteral { entries, .. } => {
                for (k, v) in entries {
                    self.expr(k);
                    self.expr(v);
                }
            }
            AstExpr::Ref { expr, .. }
            | AstExpr::RefMut { expr, .. }
            | AstExpr::Deref { expr, .. }
            | AstExpr::Move { expr, .. }
            | AstExpr::Unsafe { body: expr, .. }
            | AstExpr::Splat { expr, .. } => self.expr(expr),
            AstExpr::TryCatch {
                body,
                catch_param,
                catch_body,
                ..
            } => {
                self.expr(body);
                self.push_scope();
                self.bind(catch_param);
                self.expr(catch_body);
                self.pop_scope();
            }
            AstExpr::Raise {
                effect_name,
                args,
                span,
            } => {
                self.resolve_at(*span, Resolution::Effect(effect_name.clone()));
                for a in args {
                    self.expr(a);
                }
            }
            AstExpr::MacroCall { args, .. } => {
                for a in args {
                    self.expr(a);
                }
            }
        }
    }

    // ── Patterns ───────────────────────────────────────────────────────────

    fn bind_pattern(&mut self, pat: &AstWhenPattern) {
        match pat {
            AstWhenPattern::EnumVariant {
                enum_name,
                variant_name,
                bindings,
            } => {
                if enum_name.is_empty() {
                    // A bare identifier is a binding pattern, not a variant.
                    self.bind(variant_name);
                }
                for b in bindings {
                    self.bind(b);
                }
            }
            AstWhenPattern::OptionSome { binding } | AstWhenPattern::ResultOk { binding } => {
                if let Some(b) = binding {
                    self.bind(b);
                }
            }
            AstWhenPattern::ResultErr { binding } => {
                if let Some(b) = binding {
                    self.bind(b);
                }
            }
            AstWhenPattern::Tuple(parts) | AstWhenPattern::Or(parts) => {
                for p in parts {
                    self.bind_pattern(p);
                }
            }
            AstWhenPattern::Slice { prefix, rest } => {
                for p in prefix {
                    self.bind_pattern(p);
                }
                if let Some(r) = rest {
                    self.bind(r);
                }
            }
            AstWhenPattern::Binding { name, pattern, .. } => {
                self.bind(name);
                self.bind_pattern(pattern);
            }
            AstWhenPattern::Struct {
                struct_name,
                fields,
                span,
            } => {
                let r = self.resolve_type_name(struct_name);
                self.resolve_at(*span, r);
                for (_, p) in fields {
                    self.bind_pattern(p);
                }
            }
            AstWhenPattern::OptionNone
            | AstWhenPattern::Wildcard
            | AstWhenPattern::IntLit(_)
            | AstWhenPattern::FloatLit(_)
            | AstWhenPattern::BoolLit(_)
            | AstWhenPattern::StringLit(_)
            | AstWhenPattern::Range { .. } => {}
        }
    }
}
