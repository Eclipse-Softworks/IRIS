//! HIR -> THIR: own a typed, desugared tree for every walked expression.
//!
//! The walk mirrors `src/hir/build.rs` item for item, so the two expression
//! sets are comparable: `IRIS_THIR=check` fails if the HIR reached a span that
//! THIR did not build. Unlike the HIR, the nodes here are owned and their
//! annotations are explicit: resolved values and calls, method targets, field
//! indices, map key/value types, and a temporary-plus-element desugaring of
//! tuple destructuring. The builder also records the compact lowering facts
//! ([`ThirFacts`]) by source span, so lowering can ask THIR for a type hint,
//! a field index, a map's key/value types, or a destructuring's shape.

use std::collections::{HashMap, HashSet};

use crate::hir::{key, HirModule, Resolution};
use crate::ir::types::IrType;
use crate::lower::typeck::Registries;
use crate::parser::ast::{
    AstBlock, AstConst, AstExpr, AstFieldDef, AstFunction, AstHandlerArm, AstLayerParam,
    AstMacroDef, AstStateVar, AstStmt, AstWhenPattern,
};
use crate::parser::lexer::Span;

use super::{
    ThirArm, ThirBlock, ThirBody, ThirBodyKind, ThirDestructure, ThirDestructureKind, ThirExpr,
    ThirExprKind, ThirFacts, ThirFieldInit, ThirHandlerArm, ThirLetTuple, ThirLiteral, ThirModule,
    ThirParam, ThirSelectArm, ThirStats, ThirStmt, ThirStmtKind, ThirType,
};

pub(crate) fn build<'h, 'a, 'r>(hir: &'h HirModule<'a>, reg: &'r Registries<'r>) -> ThirModule {
    let mut b = Builder {
        hir,
        reg,
        scopes: Vec::new(),
        fresh: 0,
        module: ThirModule {
            bodies: Vec::new(),
            source_spans: HashSet::new(),
            facts: ThirFacts::default(),
            stats: ThirStats::default(),
        },
    };

    let ast = hir.source();
    // The item order is the HIR walk's order. Keeping it makes a missing item
    // visible in both reports at the same place.
    for c in &ast.consts {
        b.const_body(c);
    }
    for s in &ast.state_vars {
        b.state_body(s);
    }
    for record in &ast.structs {
        for field in &record.fields {
            if let Some(default) = &field.default {
                b.field_default_body(&record.name.name, field, default);
            }
        }
    }
    for f in &ast.functions {
        b.function(f, ThirBodyKind::Function);
    }
    for imp in &ast.impls {
        for m in &imp.methods {
            b.function(m, ThirBodyKind::Method);
        }
    }
    for m in &ast.macros {
        b.macro_body(m);
    }
    for model in &ast.models {
        for layer in &model.layers {
            for param in &layer.params {
                b.model_param_body(&model.name.name, &layer.name.name, param);
            }
        }
    }

    b.module.stats.source_expressions = b.module.source_spans.len();
    b.module
}

struct Builder<'h, 'a, 'r> {
    hir: &'h HirModule<'a>,
    reg: &'r Registries<'r>,
    /// Lexical value scopes, innermost last.
    scopes: Vec<HashMap<String, ()>>,
    /// Counter for hygienic temporaries introduced by desugaring.
    fresh: usize,
    module: ThirModule,
}

impl<'h, 'a, 'r> Builder<'h, 'a, 'r> {
    // ── Scopes and bodies ──────────────────────────────────────────────────

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

    fn fresh_name(&mut self, prefix: &str) -> String {
        loop {
            let name = format!("{prefix}_{}", self.fresh);
            self.fresh += 1;
            if !self.is_local(&name) {
                return name;
            }
        }
    }

    fn push_body(
        &mut self,
        name: String,
        kind: ThirBodyKind,
        params: Vec<ThirParam>,
        ret_ty: Option<crate::parser::ast::AstType>,
        body: ThirBlock,
    ) {
        self.module.stats.bodies += 1;
        match kind {
            ThirBodyKind::Function => self.module.stats.functions += 1,
            ThirBodyKind::Method => self.module.stats.methods += 1,
            ThirBodyKind::Const => self.module.stats.consts += 1,
            ThirBodyKind::State => self.module.stats.state_vars += 1,
            ThirBodyKind::FieldDefault => self.module.stats.field_defaults += 1,
            ThirBodyKind::Macro => self.module.stats.macros += 1,
            ThirBodyKind::ModelParam => self.module.stats.model_params += 1,
            ThirBodyKind::Attribute => self.module.stats.attributes += 1,
        }
        self.module.bodies.push(ThirBody {
            name,
            kind,
            params,
            ret_ty,
            body,
        });
    }

    fn expr_block(&self, expr: ThirExpr) -> ThirBlock {
        let span = expr.span;
        ThirBlock {
            stmts: Vec::new(),
            tail: Some(Box::new(expr)),
            span,
        }
    }

    fn expr_body(&mut self, name: String, kind: ThirBodyKind, expr: ThirExpr) {
        let body = self.expr_block(expr);
        self.push_body(name, kind, Vec::new(), None, body);
    }

    // ── Items ──────────────────────────────────────────────────────────────

    fn const_body(&mut self, c: &AstConst) {
        self.push_scope();
        let e = self.expr(&c.value);
        self.pop_scope();
        self.expr_body(c.name.name.clone(), ThirBodyKind::Const, e);
    }

    fn state_body(&mut self, s: &AstStateVar) {
        self.push_scope();
        let e = self.expr(&s.init);
        self.pop_scope();
        self.expr_body(s.name.name.clone(), ThirBodyKind::State, e);
    }

    fn field_default_body(&mut self, record: &str, field: &AstFieldDef, default: &AstExpr) {
        self.push_scope();
        let e = self.expr(default);
        self.pop_scope();
        self.expr_body(
            format!("{record}.{}", field.name.name),
            ThirBodyKind::FieldDefault,
            e,
        );
    }

    fn macro_body(&mut self, m: &AstMacroDef) {
        self.push_scope();
        for p in &m.params {
            self.bind(p);
        }
        let e = self.expr(&m.body);
        self.pop_scope();
        let params = m
            .params
            .iter()
            .map(|p| ThirParam {
                name: p.clone(),
                ty: None,
                default: None,
            })
            .collect();
        let body = self.expr_block(e);
        self.push_body(m.name.name.clone(), ThirBodyKind::Macro, params, None, body);
    }

    fn model_param_body(&mut self, model: &str, layer: &str, param: &AstLayerParam) {
        self.push_scope();
        let e = self.expr(&param.value);
        self.pop_scope();
        self.expr_body(
            format!("{model}.{layer}.{}", param.key.name),
            ThirBodyKind::ModelParam,
            e,
        );
    }

    fn function(&mut self, f: &AstFunction, kind: ThirBodyKind) {
        self.push_scope();
        // Attribute arguments are expressions of the item, as in the HIR.
        for attr in &f.attrs {
            for a in &attr.args {
                let e = self.expr(a);
                let name = format!("{}@{}({})", f.name.name, attr.name, attr.span.start.0);
                self.expr_body(name, ThirBodyKind::Attribute, e);
            }
        }
        // Defaults are lowered at call sites, not in the function's scope.
        let mut params = Vec::new();
        for p in &f.params {
            let default = p.default.as_ref().map(|e| self.expr(e));
            params.push(ThirParam {
                name: p.name.name.clone(),
                ty: Some(p.ty.clone()),
                default,
            });
        }
        for p in &f.params {
            self.bind(&p.name.name);
        }
        let body = self.block(&f.body);
        self.pop_scope();
        self.push_body(
            f.name.name.clone(),
            kind,
            params,
            Some(f.return_ty.clone()),
            body,
        );
    }

    // ── Blocks and statements ──────────────────────────────────────────────

    fn block(&mut self, b: &AstBlock) -> ThirBlock {
        self.push_scope();
        let mut stmts = Vec::new();
        for s in &b.stmts {
            stmts.extend(self.stmt(s));
        }
        let tail = b.tail.as_ref().map(|e| Box::new(self.expr(e)));
        self.pop_scope();
        ThirBlock {
            stmts,
            tail,
            span: b.span,
        }
    }

    fn stmts_block(&mut self, stmts: &[AstStmt], span: Span) -> ThirBlock {
        self.push_scope();
        let mut out = Vec::new();
        for s in stmts {
            out.extend(self.stmt(s));
        }
        self.pop_scope();
        ThirBlock {
            stmts: out,
            tail: None,
            span,
        }
    }

    fn stmt(&mut self, s: &AstStmt) -> Vec<ThirStmt> {
        match s {
            AstStmt::LetTuple {
                names,
                init,
                is_var,
                span,
            } => self.let_tuple(names, init, *is_var, *span),
            _ => {
                let stmt = self.simple_stmt(s);
                self.module.stats.statements += 1;
                vec![stmt]
            }
        }
    }

    fn simple_stmt(&mut self, s: &AstStmt) -> ThirStmt {
        let span = stmt_span(s);
        let kind = match s {
            AstStmt::Let {
                name,
                ty,
                init,
                is_var,
                ..
            } => {
                let init = self.expr(init);
                self.bind(&name.name);
                ThirStmtKind::Let {
                    name: name.name.clone(),
                    ty: ty.clone(),
                    init,
                    is_var: *is_var,
                }
            }
            AstStmt::Expr(e) => ThirStmtKind::Expr(self.expr(e)),
            AstStmt::While {
                label, cond, body, ..
            } => ThirStmtKind::While {
                label: label.clone(),
                cond: self.expr(cond),
                body: self.block(body),
            },
            AstStmt::Loop { label, body, .. } => ThirStmtKind::Loop {
                label: label.clone(),
                body: self.block(body),
            },
            AstStmt::Break { label, .. } => ThirStmtKind::Break {
                label: label.clone(),
            },
            AstStmt::Continue { label, .. } => ThirStmtKind::Continue {
                label: label.clone(),
            },
            AstStmt::ForRange {
                label,
                var,
                start,
                end,
                inclusive,
                step,
                body,
                ..
            } => {
                let start = self.expr(start);
                let end = self.expr(end);
                let step = step.as_ref().map(|e| self.expr(e));
                self.push_scope();
                self.bind(&var.name);
                let body = self.block(body);
                self.pop_scope();
                ThirStmtKind::ForRange {
                    label: label.clone(),
                    var: var.name.clone(),
                    start,
                    end,
                    inclusive: *inclusive,
                    step,
                    body,
                }
            }
            AstStmt::Assign {
                target, op, value, ..
            } => ThirStmtKind::Assign {
                target: self.expr(target),
                op: *op,
                value: self.expr(value),
            },
            AstStmt::Return { value, .. } => {
                ThirStmtKind::Return(value.as_ref().map(|e| self.expr(e)))
            }
            AstStmt::Spawn { body, group, .. } => {
                let group = group.as_ref().map(|e| self.expr(e));
                let body = self.stmts_block(body, span);
                ThirStmtKind::Spawn { body, group }
            }
            AstStmt::ParFor {
                label,
                var,
                start,
                end,
                inclusive,
                body,
                ..
            } => {
                let start = self.expr(start);
                let end = self.expr(end);
                self.push_scope();
                self.bind(&var.name);
                let body = self.block(body);
                self.pop_scope();
                ThirStmtKind::ParFor {
                    label: label.clone(),
                    var: var.name.clone(),
                    start,
                    end,
                    inclusive: *inclusive,
                    body,
                }
            }
            AstStmt::ForEach {
                label,
                var,
                iter,
                body,
                ..
            } => {
                let iter = self.expr(iter);
                self.push_scope();
                self.bind(&var.name);
                let body = self.block(body);
                self.pop_scope();
                ThirStmtKind::ForEach {
                    label: label.clone(),
                    var: var.name.clone(),
                    iter,
                    body,
                }
            }
            AstStmt::MaskStmt { effects, body, .. } => ThirStmtKind::Mask {
                effects: effects.clone(),
                body: self.block(body),
            },
            AstStmt::HandleStmt {
                expr,
                arms,
                return_ty,
                ..
            } => ThirStmtKind::Handle {
                expr: self.expr(expr),
                arms: self.handler_arms(arms),
                return_ty: (**return_ty).clone(),
            },
            AstStmt::Defer { expr, .. } => ThirStmtKind::Defer(self.expr(expr)),
            AstStmt::Select { arms, default, .. } => {
                let mut out = Vec::new();
                for arm in arms {
                    let channel = self.expr(&arm.channel);
                    self.push_scope();
                    self.bind(&arm.binding);
                    let body = self.block(&arm.body);
                    self.pop_scope();
                    out.push(ThirSelectArm {
                        channel,
                        binding: arm.binding.clone(),
                        body,
                        span: arm.span,
                    });
                }
                let default = default.as_ref().map(|b| self.block(b));
                ThirStmtKind::Select { arms: out, default }
            }
            AstStmt::Yield { expr, .. } => ThirStmtKind::Yield(self.expr(expr)),
            AstStmt::LetTuple { .. } => {
                unreachable!("LetTuple is desugared by stmt()")
            }
        };
        ThirStmt { kind, span }
    }

    /// `let (a, b) = init` becomes a temporary plus one access per component.
    /// The temporary guarantees `init` is evaluated once. A known tuple uses
    /// `TupleIndex`; a known record uses `Field` with its layout index; an
    /// open type uses `Destructure`, which keeps the tuple-or-record choice
    /// for lowering.
    fn let_tuple(
        &mut self,
        names: &[crate::parser::ast::Ident],
        init: &AstExpr,
        is_var: bool,
        span: Span,
    ) -> Vec<ThirStmt> {
        let init = self.expr(init);
        let init_span = init.span;
        let init_ty = init.ty.clone();
        let known = init_ty.known();

        let temp = self.fresh_name("__thir_tmp");
        let temp_name = temp.clone();
        self.bind(&temp);
        let temp_value = ThirExpr {
            kind: ThirExprKind::Value {
                name: temp.clone(),
                resolution: Resolution::Local,
            },
            ty: init_ty.clone(),
            span: init_span,
        };
        self.count_expr(&temp_value);
        let mut out = vec![ThirStmt {
            kind: ThirStmtKind::Let {
                name: temp,
                ty: None,
                init,
                is_var: false,
            },
            span,
        }];
        let mut components = Vec::with_capacity(names.len());

        match known {
            Some(IrType::Tuple(elems)) if elems.len() == names.len() => {
                self.module.stats.tuple_destructures += names.len();
                for (i, name) in names.iter().enumerate() {
                    let elem_ty = elems[i].clone();
                    let value = ThirExpr {
                        kind: ThirExprKind::TupleIndex {
                            base: Box::new(temp_value.clone()),
                            index: i,
                        },
                        ty: ThirType::Known(elem_ty),
                        span: name.span,
                    };
                    components.push(ThirDestructure {
                        name: name.name.clone(),
                        index: i,
                        field: None,
                        kind: ThirDestructureKind::Tuple,
                        ty: value.ty.clone(),
                    });
                    self.count_expr(&value);
                    out.push(ThirStmt {
                        kind: ThirStmtKind::Let {
                            name: name.name.clone(),
                            ty: None,
                            init: value,
                            is_var,
                        },
                        span: name.span,
                    });
                    self.bind(&name.name);
                }
            }
            Some(IrType::Struct { fields, .. }) if fields.len() == names.len() => {
                self.module.stats.struct_destructures += names.len();
                for (i, name) in names.iter().enumerate() {
                    let (field, field_ty) = fields[i].clone();
                    let value = ThirExpr {
                        kind: ThirExprKind::Field {
                            base: Box::new(temp_value.clone()),
                            field: field.clone(),
                            index: Some(i),
                        },
                        ty: ThirType::Known(field_ty),
                        span: name.span,
                    };
                    components.push(ThirDestructure {
                        name: name.name.clone(),
                        index: i,
                        field: Some(field),
                        kind: ThirDestructureKind::Field,
                        ty: value.ty.clone(),
                    });
                    self.count_expr(&value);
                    out.push(ThirStmt {
                        kind: ThirStmtKind::Let {
                            name: name.name.clone(),
                            ty: None,
                            init: value,
                            is_var,
                        },
                        span: name.span,
                    });
                    self.bind(&name.name);
                }
            }
            _ => {
                self.module.stats.open_destructures += names.len();
                for (i, name) in names.iter().enumerate() {
                    let value = ThirExpr {
                        kind: ThirExprKind::Destructure {
                            base: Box::new(temp_value.clone()),
                            index: i,
                        },
                        ty: ThirType::Open,
                        span: name.span,
                    };
                    components.push(ThirDestructure {
                        name: name.name.clone(),
                        index: i,
                        field: None,
                        kind: ThirDestructureKind::Open,
                        ty: value.ty.clone(),
                    });
                    self.count_expr(&value);
                    out.push(ThirStmt {
                        kind: ThirStmtKind::Let {
                            name: name.name.clone(),
                            ty: None,
                            init: value,
                            is_var,
                        },
                        span: name.span,
                    });
                    self.bind(&name.name);
                }
            }
        }
        self.module.facts.let_tuples.insert(
            key(span),
            ThirLetTuple {
                temp: temp_name,
                init_ty,
                components,
            },
        );
        self.module.stats.destructures += names.len();
        self.module.stats.statements += out.len();
        out
    }

    fn handler_arms(&mut self, arms: &[AstHandlerArm]) -> Vec<ThirHandlerArm> {
        let mut out = Vec::new();
        for arm in arms {
            self.push_scope();
            for p in &arm.params {
                self.bind(&p.name);
            }
            if let Some(r) = &arm.resume_param {
                self.bind(&r.name);
            }
            let body = self.expr(&arm.body);
            self.pop_scope();
            out.push(ThirHandlerArm {
                effect: arm.effect_name.clone(),
                params: arm.params.iter().map(|p| p.name.clone()).collect(),
                resume_param: arm.resume_param.as_ref().map(|r| r.name.clone()),
                body,
                span: arm.span,
            });
        }
        out
    }

    // ── Expressions ────────────────────────────────────────────────────────

    fn expr(&mut self, e: &AstExpr) -> ThirExpr {
        let span = e.span();
        if self.module.source_spans.insert(key(span)) {
            self.module.stats.source_expressions += 1;
        }
        let kind = match e {
            AstExpr::Ident(id) => ThirExprKind::Value {
                name: id.name.clone(),
                resolution: self
                    .hir
                    .resolution_at(id.span)
                    .cloned()
                    .unwrap_or(Resolution::Unknown),
            },
            AstExpr::IntLit { value, .. } => ThirExprKind::Literal(ThirLiteral::Int(*value)),
            AstExpr::FloatLit { value, .. } => ThirExprKind::Literal(ThirLiteral::Float(*value)),
            AstExpr::BoolLit { value, .. } => ThirExprKind::Literal(ThirLiteral::Bool(*value)),
            AstExpr::StringLit { value, .. } => {
                ThirExprKind::Literal(ThirLiteral::Str(value.clone()))
            }
            AstExpr::BinOp { op, lhs, rhs, .. } => ThirExprKind::BinOp {
                op: *op,
                lhs: Box::new(self.expr(lhs)),
                rhs: Box::new(self.expr(rhs)),
            },
            AstExpr::Call {
                callee,
                args,
                named_args,
                span,
            } => {
                let target = self
                    .hir
                    .call_at(*span)
                    .map(|c| c.target.clone())
                    .unwrap_or(Resolution::Unknown);
                ThirExprKind::Call {
                    callee: callee.name.clone(),
                    target,
                    args: args.iter().map(|a| self.expr(a)).collect(),
                    named_args: named_args
                        .iter()
                        .map(|(n, a)| (n.clone(), self.expr(a)))
                        .collect(),
                }
            }
            AstExpr::UnaryOp { op, expr, .. } => ThirExprKind::UnaryOp {
                op: *op,
                expr: Box::new(self.expr(expr)),
            },
            AstExpr::If {
                cond,
                then_block,
                else_block,
                ..
            } => ThirExprKind::If {
                cond: Box::new(self.expr(cond)),
                then_block: self.block(then_block),
                else_block: else_block.as_ref().map(|b| self.block(b)),
            },
            AstExpr::Block(b) => ThirExprKind::Block(self.block(b)),
            AstExpr::Index { base, indices, .. } => ThirExprKind::Index {
                base: Box::new(self.expr(base)),
                indices: indices.iter().map(|i| self.expr(i)).collect(),
            },
            AstExpr::Cast { expr, ty, .. } => {
                let resolved =
                    crate::lower::resolve_ast_type_with_subs(ty, &HashMap::new(), self.reg.module)
                        .ok();
                ThirExprKind::Cast {
                    expr: Box::new(self.expr(expr)),
                    ty: ty.clone(),
                    resolved,
                }
            }
            AstExpr::StructLit {
                name,
                fields,
                spread,
                span,
                ..
            } => {
                let target = self
                    .hir
                    .resolution_at(*span)
                    .cloned()
                    .unwrap_or(Resolution::Unknown);
                let indices: Vec<Option<usize>> = match &target {
                    Resolution::Type(resolved) => self
                        .reg
                        .module
                        .struct_def(resolved)
                        .map(|def| {
                            fields
                                .iter()
                                .map(|(n, _)| def.iter().position(|(f, _)| f == n))
                                .collect()
                        })
                        .unwrap_or_else(|| vec![None; fields.len()]),
                    _ => vec![None; fields.len()],
                };
                let fields = fields
                    .iter()
                    .zip(indices)
                    .map(|((n, e), index)| ThirFieldInit {
                        name: n.clone(),
                        index,
                        value: self.expr(e),
                    })
                    .collect();
                ThirExprKind::StructLit {
                    name: name.clone(),
                    target,
                    fields,
                    spread: spread.as_ref().map(|s| Box::new(self.expr(s))),
                }
            }
            AstExpr::FieldAccess { base, field, .. } => {
                let base = self.expr(base);
                let index = base.ty.known().and_then(|ty| match ty {
                    IrType::Struct { fields, .. } => fields.iter().position(|(n, _)| n == field),
                    _ => None,
                });
                ThirExprKind::Field {
                    base: Box::new(base),
                    field: field.clone(),
                    index,
                }
            }
            AstExpr::When {
                scrutinee, arms, ..
            } => {
                let scrutinee = self.expr(scrutinee);
                let mut out = Vec::new();
                for arm in arms {
                    let variant = self.hir.resolution_at(arm.span).cloned();
                    self.push_scope();
                    self.bind_pattern(&arm.pattern);
                    let guard = arm.guard.as_ref().map(|g| self.expr(g));
                    let body = self.expr(&arm.body);
                    self.pop_scope();
                    out.push(ThirArm {
                        pattern: arm.pattern.clone(),
                        variant,
                        guard,
                        body,
                        span: arm.span,
                    });
                }
                ThirExprKind::When {
                    scrutinee: Box::new(scrutinee),
                    arms: out,
                }
            }
            AstExpr::Tuple { elements, .. } => {
                ThirExprKind::Tuple(elements.iter().map(|e| self.expr(e)).collect())
            }
            AstExpr::TupleIndex { base, index, .. } => ThirExprKind::TupleIndex {
                base: Box::new(self.expr(base)),
                index: *index,
            },
            AstExpr::ArrayLit { elems, .. } => {
                ThirExprKind::Array(elems.iter().map(|e| self.expr(e)).collect())
            }
            AstExpr::Lambda { params, body, .. } => {
                let mut out_params = Vec::new();
                for p in params {
                    let default = p.default.as_ref().map(|e| self.expr(e));
                    out_params.push(ThirParam {
                        name: p.name.name.clone(),
                        ty: Some(p.ty.clone()),
                        default,
                    });
                }
                self.push_scope();
                for p in params {
                    self.bind(&p.name.name);
                }
                let body = self.expr(body);
                self.pop_scope();
                ThirExprKind::Lambda {
                    params: out_params,
                    body: Box::new(body),
                }
            }
            AstExpr::Await { expr, .. } => ThirExprKind::Await(Box::new(self.expr(expr))),
            AstExpr::Try { expr, .. } => ThirExprKind::Try(Box::new(self.expr(expr))),
            AstExpr::NullCoal { expr, default, .. } => ThirExprKind::NullCoal {
                expr: Box::new(self.expr(expr)),
                default: Box::new(self.expr(default)),
            },
            AstExpr::MethodCall {
                base,
                method,
                args,
                span,
            } => {
                let target = self
                    .hir
                    .call_at(*span)
                    .map(|c| c.target.clone())
                    .unwrap_or(Resolution::Unknown);
                ThirExprKind::MethodCall {
                    receiver: Box::new(self.expr(base)),
                    method: method.clone(),
                    target,
                    args: args.iter().map(|a| self.expr(a)).collect(),
                }
            }
            AstExpr::Mask { effects, body, .. } => ThirExprKind::Mask {
                effects: effects.clone(),
                body: self.block(body),
            },
            AstExpr::Handle {
                expr,
                arms,
                return_ty,
                ..
            } => ThirExprKind::Handle {
                expr: Box::new(self.expr(expr)),
                arms: self.handler_arms(arms),
                return_ty: (**return_ty).clone(),
            },
            AstExpr::MapLiteral { entries, span } => {
                let (key_ty, val_ty) = match self.ty(*span).known() {
                    Some(IrType::Map(k, v)) => (Some(*k), Some(*v)),
                    _ => match self.hir.hint_at(*span) {
                        Some(h) => (h.part(0), h.part(1)),
                        None => (None, None),
                    },
                };
                ThirExprKind::Map {
                    entries: entries
                        .iter()
                        .map(|(k, v)| (self.expr(k), self.expr(v)))
                        .collect(),
                    key_ty,
                    val_ty,
                }
            }
            AstExpr::Ref { expr, .. } => ThirExprKind::Ref(Box::new(self.expr(expr))),
            AstExpr::RefMut { expr, .. } => ThirExprKind::RefMut(Box::new(self.expr(expr))),
            AstExpr::Deref { expr, .. } => ThirExprKind::Deref(Box::new(self.expr(expr))),
            AstExpr::Move { expr, .. } => ThirExprKind::Move(Box::new(self.expr(expr))),
            AstExpr::Unsafe { body, .. } => ThirExprKind::Unsafe(Box::new(self.expr(body))),
            AstExpr::Splat { expr, .. } => ThirExprKind::Splat(Box::new(self.expr(expr))),
            AstExpr::TryCatch {
                body,
                catch_param,
                catch_body,
                ..
            } => {
                let body = self.expr(body);
                self.push_scope();
                self.bind(catch_param);
                let catch_body = self.expr(catch_body);
                self.pop_scope();
                ThirExprKind::TryCatch {
                    body: Box::new(body),
                    catch_param: catch_param.clone(),
                    catch_body: Box::new(catch_body),
                }
            }
            AstExpr::Raise {
                effect_name,
                args,
                span,
            } => ThirExprKind::Raise {
                effect: effect_name.clone(),
                target: self
                    .hir
                    .resolution_at(*span)
                    .cloned()
                    .unwrap_or(Resolution::Unknown),
                args: args.iter().map(|a| self.expr(a)).collect(),
            },
            AstExpr::MacroCall { name, args, .. } => ThirExprKind::MacroCall {
                name: name.name.clone(),
                args: args.iter().map(|a| self.expr(a)).collect(),
            },
        };

        let expr = ThirExpr {
            kind,
            ty: self.ty(span),
            span,
        };
        self.record_facts(&expr);
        self.count_expr(&expr);
        expr
    }

    fn ty(&self, span: Span) -> ThirType {
        ThirType::from_hir(self.hir.type_at(span), self.hir.hint_at(span))
    }

    /// Record the facts lowering asks for. This is deliberately done here,
    /// not in `count_expr`, so synthetic desugaring nodes (which have source
    /// spans copied from a name) never overwrite a source expression's facts.
    fn record_facts(&mut self, e: &ThirExpr) {
        let k = key(e.span);
        self.module.facts.types.insert(k, e.ty.clone());
        if let Some(hint) = self.hir.hint_at(e.span) {
            self.module.facts.hints.insert(k, hint.clone());
        }
        match &e.kind {
            ThirExprKind::Field {
                index: Some(index), ..
            } => {
                self.module.facts.field_indices.insert(k, *index);
            }
            ThirExprKind::Map { key_ty, val_ty, .. } => {
                self.module
                    .facts
                    .map_types
                    .insert(k, (key_ty.clone(), val_ty.clone()));
            }
            _ => {}
        }
    }

    fn count_expr(&mut self, e: &ThirExpr) {
        self.module.stats.expressions += 1;
        match &e.ty {
            ThirType::Known(_) => self.module.stats.known += 1,
            ThirType::Partial(_) => self.module.stats.partial += 1,
            ThirType::Open => self.module.stats.open += 1,
        }
        match &e.kind {
            ThirExprKind::Value { resolution, .. } => {
                self.module.stats.values += 1;
                if matches!(resolution, Resolution::Unknown) {
                    self.module.stats.unresolved_values += 1;
                }
            }
            ThirExprKind::Call { target, .. } => {
                self.module.stats.calls += 1;
                if matches!(target, Resolution::Unknown) {
                    self.module.stats.unresolved_calls += 1;
                }
            }
            ThirExprKind::MethodCall { target, .. } => {
                self.module.stats.calls += 1;
                self.module.stats.method_calls += 1;
                if matches!(target, Resolution::Method(_)) {
                    self.module.stats.method_targets += 1;
                } else if matches!(target, Resolution::Unknown) {
                    self.module.stats.unresolved_calls += 1;
                }
            }
            ThirExprKind::Map { key_ty, val_ty, .. } => {
                self.module.stats.maps += 1;
                if key_ty.is_some() {
                    self.module.stats.maps_with_key += 1;
                }
                if val_ty.is_some() {
                    self.module.stats.maps_with_value += 1;
                }
            }
            ThirExprKind::Field { index, .. } => {
                self.module.stats.field_accesses += 1;
                if index.is_some() {
                    self.module.stats.field_indices += 1;
                }
            }
            ThirExprKind::Cast { resolved, .. } => {
                self.module.stats.casts += 1;
                if resolved.is_some() {
                    self.module.stats.resolved_casts += 1;
                }
            }
            ThirExprKind::Raise { target, .. } => {
                if matches!(target, Resolution::Unknown) {
                    self.module.stats.unresolved_values += 1;
                }
            }
            _ => {}
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
            AstWhenPattern::Struct { fields, .. } => {
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

fn stmt_span(s: &AstStmt) -> Span {
    match s {
        AstStmt::Let { span, .. }
        | AstStmt::While { span, .. }
        | AstStmt::Loop { span, .. }
        | AstStmt::Break { span, .. }
        | AstStmt::Continue { span, .. }
        | AstStmt::ForRange { span, .. }
        | AstStmt::Assign { span, .. }
        | AstStmt::LetTuple { span, .. }
        | AstStmt::Return { span, .. }
        | AstStmt::Spawn { span, .. }
        | AstStmt::ParFor { span, .. }
        | AstStmt::ForEach { span, .. }
        | AstStmt::MaskStmt { span, .. }
        | AstStmt::HandleStmt { span, .. }
        | AstStmt::Defer { span, .. }
        | AstStmt::Select { span, .. }
        | AstStmt::Yield { span, .. } => *span,
        AstStmt::Expr(e) => e.span(),
    }
}
