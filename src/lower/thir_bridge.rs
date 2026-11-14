//! Bridges between THIR nodes and AST/span markers used during hybrid lowering.

use crate::parser::ast::{
    AstBlock, AstExpr, AstHandlerArm, AstParam, AstStmt, AstType, AstWhenArm, Ident,
};
use crate::parser::lexer::Span;

use super::resolve_qualifier;

/// Synthetic marker nodes for the THIR lowerer's span-addressed delegation:
/// an `AstExpr::Block` (or an `AstBlock`) with no source semantics meaning
/// "lower the THIR node I was registered with". Markers carry their THIR
/// payload through a side registry because source spans are ambiguous:
/// desugaring mints several expressions at one span (an f-string's nested
/// `to_str`/`concat` calls all share the literal's span), so span-keyed
/// lookup races THIR's span collisions and recurses forever.
pub(super) const SYNTH_MARK: u32 = u32::MAX;
const SYNTH_FLAG: u32 = 0x8000_0000;

/// The THIR payload a synthetic marker stands for.
#[derive(Clone)]
#[allow(dead_code)]
pub(super) enum SynthMarker {
    Expr(crate::thir::ThirExpr),
    Block(crate::thir::ThirBlock),
}

#[allow(dead_code)]
pub(super) fn push_synth_marker(m: SynthMarker) -> u32 {
    SYNTH_MARKERS.with(|r| {
        let mut v = r.borrow_mut();
        v.push(m);
        (v.len() - 1) as u32
    })
}

#[allow(dead_code)]
pub(super) fn synth_marker_span(id: u32) -> Span {
    // start = SYNTH_FLAG | id, end = SYNTH_MARK: unambiguous and trivially
    // distinguishable from any real source span.
    Span::new(SYNTH_FLAG | id, SYNTH_MARK)
}

pub(super) fn synth_marker_at(span: Span) -> Option<SynthMarker> {
    if span.end.0 == SYNTH_MARK && (span.start.0 & SYNTH_FLAG) != 0 {
        let id = (span.start.0 & !SYNTH_FLAG) as usize;
        SYNTH_MARKERS.with(|r| r.borrow().get(id).cloned())
    } else {
        None
    }
}

/// The THIR payload a synthetic marker stands for, when `arg` is one.
pub(super) fn synth_marker_thir_expr(arg: &AstExpr) -> Option<crate::thir::ThirExpr> {
    if let AstExpr::Block(b) = arg {
        if b.stmts.is_empty() && b.tail.is_none() {
            if let Some(SynthMarker::Expr(e)) = synth_marker_at(b.span) {
                return Some(e);
            }
        }
    }
    None
}

#[allow(dead_code)]
pub(super) fn synth_expr_marker(e: &crate::thir::ThirExpr) -> AstExpr {
    AstExpr::Block(crate::parser::ast::AstBlock {
        stmts: Vec::new(),
        tail: None,
        span: synth_marker_span(push_synth_marker(SynthMarker::Expr(e.clone()))),
    })
}

#[allow(dead_code)]
pub(super) fn synth_block_expr_marker(b: &crate::thir::ThirBlock) -> AstExpr {
    AstExpr::Block(crate::parser::ast::AstBlock {
        stmts: Vec::new(),
        tail: None,
        span: synth_marker_span(push_synth_marker(SynthMarker::Block(b.clone()))),
    })
}

#[allow(dead_code)]
pub(super) fn synth_block_ast_marker(b: &crate::thir::ThirBlock) -> crate::parser::ast::AstBlock {
    crate::parser::ast::AstBlock {
        stmts: Vec::new(),
        tail: None,
        span: synth_marker_span(push_synth_marker(SynthMarker::Block(b.clone()))),
    }
}

thread_local! {
    /// Guard against re-entering the THIR body walk from inside
    /// `try_lower_thir_expr`. Thread-local so sub-lowerers created during
    /// lambda/handler/spawn lowering inherit the guard.
    pub(super) static IN_THIR_LOWER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Registry of the THIR payloads synthetic markers stand for. Never
    /// cleared for the life of a thread; markers are per-compile garbage
    /// but the vector is bounded by the number of lowered nodes.
    static SYNTH_MARKERS: std::cell::RefCell<Vec<SynthMarker>> =
        std::cell::RefCell::new(Vec::with_capacity(64));
}

pub(super) fn thir_expr_uses_name(e: &crate::thir::ThirExpr, name: &str) -> bool {
    match &e.kind {
        crate::thir::ThirExprKind::Value { name: n, .. } => n == name,
        crate::thir::ThirExprKind::Literal(_) => false,
        crate::thir::ThirExprKind::UnaryOp { expr, .. } => thir_expr_uses_name(expr, name),
        crate::thir::ThirExprKind::BinOp { lhs, rhs, .. } => {
            thir_expr_uses_name(lhs, name) || thir_expr_uses_name(rhs, name)
        }
        crate::thir::ThirExprKind::Call {
            callee,
            args,
            named_args,
            ..
        } => {
            callee == name
                || args.iter().any(|a| thir_expr_uses_name(a, name))
                || named_args.iter().any(|(_, a)| thir_expr_uses_name(a, name))
        }
        crate::thir::ThirExprKind::MethodCall { receiver, args, .. } => {
            thir_expr_uses_name(receiver, name) || args.iter().any(|a| thir_expr_uses_name(a, name))
        }
        crate::thir::ThirExprKind::Field { base, .. }
        | crate::thir::ThirExprKind::TupleIndex { base, .. }
        | crate::thir::ThirExprKind::Destructure { base, .. } => thir_expr_uses_name(base, name),
        crate::thir::ThirExprKind::Index { base, indices } => {
            thir_expr_uses_name(base, name) || indices.iter().any(|i| thir_expr_uses_name(i, name))
        }
        crate::thir::ThirExprKind::Cast { expr, .. } => thir_expr_uses_name(expr, name),
        crate::thir::ThirExprKind::If {
            cond,
            then_block,
            else_block,
        } => {
            thir_expr_uses_name(cond, name)
                || thir_block_uses_name(then_block, name)
                || else_block
                    .as_ref()
                    .is_some_and(|b| thir_block_uses_name(b, name))
        }
        crate::thir::ThirExprKind::Block(b) => thir_block_uses_name(b, name),
        crate::thir::ThirExprKind::Tuple(elems) | crate::thir::ThirExprKind::Array(elems) => {
            elems.iter().any(|e| thir_expr_uses_name(e, name))
        }
        crate::thir::ThirExprKind::StructLit { fields, spread, .. } => {
            fields.iter().any(|f| thir_expr_uses_name(&f.value, name))
                || spread
                    .as_ref()
                    .is_some_and(|s| thir_expr_uses_name(s, name))
        }
        crate::thir::ThirExprKind::Map { entries, .. } => entries
            .iter()
            .any(|(k, v)| thir_expr_uses_name(k, name) || thir_expr_uses_name(v, name)),
        crate::thir::ThirExprKind::When {
            scrutinee, arms, ..
        } => {
            thir_expr_uses_name(scrutinee, name)
                || arms.iter().any(|a| {
                    a.guard
                        .as_ref()
                        .is_some_and(|g| thir_expr_uses_name(g, name))
                        || thir_expr_uses_name(&a.body, name)
                })
        }
        crate::thir::ThirExprKind::Lambda { params, body, .. } => {
            if params.iter().any(|p| p.name == name) {
                false
            } else {
                thir_expr_uses_name(body, name)
            }
        }
        crate::thir::ThirExprKind::Try(e)
        | crate::thir::ThirExprKind::Await(e)
        | crate::thir::ThirExprKind::Ref(e)
        | crate::thir::ThirExprKind::RefMut(e)
        | crate::thir::ThirExprKind::Deref(e)
        | crate::thir::ThirExprKind::Move(e)
        | crate::thir::ThirExprKind::Unsafe(e)
        | crate::thir::ThirExprKind::Splat(e) => thir_expr_uses_name(e, name),
        crate::thir::ThirExprKind::NullCoal { expr, default } => {
            thir_expr_uses_name(expr, name) || thir_expr_uses_name(default, name)
        }
        crate::thir::ThirExprKind::Mask { body, .. } => thir_block_uses_name(body, name),
        crate::thir::ThirExprKind::Handle { expr, arms, .. } => {
            thir_expr_uses_name(expr, name)
                || arms.iter().any(|a| thir_expr_uses_name(&a.body, name))
        }
        crate::thir::ThirExprKind::TryCatch {
            body, catch_body, ..
        } => thir_expr_uses_name(body, name) || thir_expr_uses_name(catch_body, name),
        crate::thir::ThirExprKind::Raise { args, .. } => {
            args.iter().any(|a| thir_expr_uses_name(a, name))
        }
        _ => false,
    }
}

pub(super) fn thir_block_uses_name(b: &crate::thir::ThirBlock, name: &str) -> bool {
    b.stmts.iter().any(|s| thir_stmt_uses_name(s, name))
        || b.tail
            .as_ref()
            .is_some_and(|t| thir_expr_uses_name(t, name))
}

pub(super) fn thir_stmt_uses_name(s: &crate::thir::ThirStmt, name: &str) -> bool {
    match &s.kind {
        crate::thir::ThirStmtKind::Let { name: n, init, .. } => {
            if n == name {
                false
            } else {
                thir_expr_uses_name(init, name)
            }
        }
        crate::thir::ThirStmtKind::Expr(e) => thir_expr_uses_name(e, name),
        crate::thir::ThirStmtKind::Return(Some(e)) => thir_expr_uses_name(e, name),
        crate::thir::ThirStmtKind::Assign { target, value, .. } => {
            thir_expr_uses_name(target, name) || thir_expr_uses_name(value, name)
        }
        crate::thir::ThirStmtKind::While { cond, body, .. } => {
            thir_expr_uses_name(cond, name) || thir_block_uses_name(body, name)
        }
        crate::thir::ThirStmtKind::Loop { body, .. } => thir_block_uses_name(body, name),
        crate::thir::ThirStmtKind::ForRange {
            start,
            end,
            body,
            step,
            ..
        } => {
            thir_expr_uses_name(start, name)
                || thir_expr_uses_name(end, name)
                || step.as_ref().is_some_and(|s| thir_expr_uses_name(s, name))
                || thir_block_uses_name(body, name)
        }
        crate::thir::ThirStmtKind::ForEach { iter, body, .. } => {
            thir_expr_uses_name(iter, name) || thir_block_uses_name(body, name)
        }
        crate::thir::ThirStmtKind::ParFor {
            start, end, body, ..
        } => {
            thir_expr_uses_name(start, name)
                || thir_expr_uses_name(end, name)
                || thir_block_uses_name(body, name)
        }
        crate::thir::ThirStmtKind::Spawn { body, group, .. } => {
            thir_block_uses_name(body, name)
                || group.as_ref().is_some_and(|g| thir_expr_uses_name(g, name))
        }
        crate::thir::ThirStmtKind::Mask { body, .. } => thir_block_uses_name(body, name),
        crate::thir::ThirStmtKind::Handle { expr, arms, .. } => {
            thir_expr_uses_name(expr, name)
                || arms.iter().any(|a| thir_expr_uses_name(&a.body, name))
        }
        crate::thir::ThirStmtKind::Defer(e) => thir_expr_uses_name(e, name),
        crate::thir::ThirStmtKind::Select { arms, default } => {
            arms.iter().any(|a| thir_block_uses_name(&a.body, name))
                || default
                    .as_ref()
                    .is_some_and(|d| thir_block_uses_name(d, name))
        }
        crate::thir::ThirStmtKind::Yield(e) => thir_expr_uses_name(e, name),
        _ => false,
    }
}

pub(super) fn thir_expr_migratable(e: &crate::thir::ThirExpr) -> bool {
    match &e.kind {
        crate::thir::ThirExprKind::Literal(_) => true,
        crate::thir::ThirExprKind::Value { .. } => true,
        crate::thir::ThirExprKind::UnaryOp { expr, .. } => thir_expr_migratable(expr),
        crate::thir::ThirExprKind::Tuple(elements) => elements.iter().all(thir_expr_migratable),
        crate::thir::ThirExprKind::TupleIndex { base, .. } => thir_expr_migratable(base),
        crate::thir::ThirExprKind::Field { base, .. } => thir_expr_migratable(base),
        crate::thir::ThirExprKind::Destructure { base, .. } => thir_expr_migratable(base),
        crate::thir::ThirExprKind::Map { entries, .. } => entries
            .iter()
            .all(|(k, v)| thir_expr_migratable(k) && thir_expr_migratable(v)),
        crate::thir::ThirExprKind::BinOp { lhs, rhs, .. } => {
            thir_expr_migratable(lhs) && thir_expr_migratable(rhs)
        }
        crate::thir::ThirExprKind::Index { base, indices } => {
            thir_expr_migratable(base) && indices.iter().all(thir_expr_migratable)
        }
        crate::thir::ThirExprKind::Cast { expr, .. } => thir_expr_migratable(expr),
        crate::thir::ThirExprKind::Await(e) => thir_expr_migratable(e),
        crate::thir::ThirExprKind::Try(e) => thir_expr_migratable(e),
        crate::thir::ThirExprKind::Block(b) => thir_block_migratable(b),
        crate::thir::ThirExprKind::NullCoal { expr, default } => {
            thir_expr_migratable(expr) && thir_expr_migratable(default)
        }
        crate::thir::ThirExprKind::If {
            cond,
            then_block,
            else_block,
        } => {
            thir_expr_migratable(cond)
                && thir_block_migratable(then_block)
                && else_block.as_ref().map_or(true, thir_block_migratable)
        }
        crate::thir::ThirExprKind::Array(elems) => elems.iter().all(thir_expr_migratable),
        crate::thir::ThirExprKind::Mask { body, .. } => thir_block_migratable(body),
        crate::thir::ThirExprKind::StructLit { fields, spread, .. } => {
            fields.iter().all(|f| thir_expr_migratable(&f.value))
                && spread.as_ref().map_or(true, |s| thir_expr_migratable(s))
        }
        crate::thir::ThirExprKind::Raise { args, .. } => args.iter().all(thir_expr_migratable),
        crate::thir::ThirExprKind::MethodCall { receiver, args, .. } => {
            thir_expr_migratable(receiver) && args.iter().all(thir_expr_migratable)
        }
        crate::thir::ThirExprKind::Lambda { params, body, .. } => {
            params
                .iter()
                .all(|p| p.default.as_ref().map_or(true, thir_expr_migratable))
                && thir_expr_migratable(body)
        }
        crate::thir::ThirExprKind::Handle { expr, arms, .. } => {
            thir_expr_migratable(expr) && arms.iter().all(|a| thir_expr_migratable(&a.body))
        }
        crate::thir::ThirExprKind::TryCatch {
            body, catch_body, ..
        } => thir_expr_migratable(body) && thir_expr_migratable(catch_body),
        crate::thir::ThirExprKind::Call {
            args, named_args, ..
        } => {
            args.iter().all(thir_expr_migratable)
                && named_args.iter().all(|(_, e)| thir_expr_migratable(e))
        }
        crate::thir::ThirExprKind::When {
            scrutinee, arms, ..
        } => {
            thir_expr_migratable(scrutinee)
                && arms.iter().all(|a| {
                    a.guard.as_ref().map_or(true, thir_expr_migratable)
                        && thir_expr_migratable(&a.body)
                })
        }
        crate::thir::ThirExprKind::Ref(e)
        | crate::thir::ThirExprKind::RefMut(e)
        | crate::thir::ThirExprKind::Deref(e)
        | crate::thir::ThirExprKind::Move(e)
        | crate::thir::ThirExprKind::Unsafe(e)
        | crate::thir::ThirExprKind::Splat(e) => thir_expr_migratable(e),
        crate::thir::ThirExprKind::MacroCall { .. } => true,
    }
}

pub(super) fn thir_stmt_migratable(s: &crate::thir::ThirStmt) -> bool {
    match &s.kind {
        crate::thir::ThirStmtKind::Let { init, .. } => thir_expr_migratable(init),
        crate::thir::ThirStmtKind::Expr(e) => thir_expr_migratable(e),
        crate::thir::ThirStmtKind::Return(Some(e)) => thir_expr_migratable(e),
        crate::thir::ThirStmtKind::Return(None) => true,
        crate::thir::ThirStmtKind::Assign { target, value, .. } => {
            matches!(
                target.kind,
                crate::thir::ThirExprKind::Value { .. }
                    | crate::thir::ThirExprKind::Index { .. }
                    | crate::thir::ThirExprKind::Field { .. }
                    | crate::thir::ThirExprKind::TupleIndex { .. }
                    | crate::thir::ThirExprKind::Deref(_)
            ) && thir_expr_migratable(target)
                && thir_expr_migratable(value)
        }
        crate::thir::ThirStmtKind::While { cond, body, .. } => {
            thir_expr_migratable(cond) && thir_block_migratable(body)
        }
        crate::thir::ThirStmtKind::Loop { body, .. } => thir_block_migratable(body),
        crate::thir::ThirStmtKind::ForRange {
            start,
            end,
            body,
            step,
            ..
        } => {
            thir_expr_migratable(start)
                && thir_expr_migratable(end)
                && step.as_ref().map_or(true, thir_expr_migratable)
                && thir_block_migratable(body)
        }
        crate::thir::ThirStmtKind::ParFor {
            start, end, body, ..
        } => {
            thir_expr_migratable(start) && thir_expr_migratable(end) && thir_block_migratable(body)
        }
        crate::thir::ThirStmtKind::ForEach { iter, body, .. } => {
            thir_expr_migratable(iter) && thir_block_migratable(body)
        }
        crate::thir::ThirStmtKind::Mask { body, .. } => thir_block_migratable(body),
        crate::thir::ThirStmtKind::Defer(e) => thir_expr_migratable(e),
        crate::thir::ThirStmtKind::Yield(e) => thir_expr_migratable(e),
        crate::thir::ThirStmtKind::Break { .. } | crate::thir::ThirStmtKind::Continue { .. } => {
            true
        }
        crate::thir::ThirStmtKind::Spawn { body, group } => {
            thir_block_migratable(body) && group.as_ref().map_or(true, thir_expr_migratable)
        }
        crate::thir::ThirStmtKind::Select { arms, default } => {
            arms.iter().all(|a| thir_block_migratable(&a.body))
                && default.as_ref().map_or(true, thir_block_migratable)
        }
        crate::thir::ThirStmtKind::Handle { expr, arms, .. } => {
            thir_expr_migratable(expr) && arms.iter().all(|a| thir_expr_migratable(&a.body))
        }
    }
}

pub(super) fn thir_block_migratable(b: &crate::thir::ThirBlock) -> bool {
    b.stmts.iter().all(thir_stmt_migratable)
        && b.tail.as_ref().map_or(true, |t| thir_expr_migratable(t))
}

pub(super) fn thir_matches_ast(thir: &crate::thir::ThirExpr, ast: &AstExpr) -> bool {
    use crate::parser::ast::AstExpr;
    use crate::thir::ThirExprKind;
    // Macro expansion can reuse/swap spans, producing THIR nodes whose
    // span does not match the AST expression being lowered. Never migrate
    // those: the AST path is the authority for them.
    if thir.span.start.0 == 0 && thir.span.end.0 == 0 {
        return false;
    }
    if thir.span.start.0 > thir.span.end.0 {
        return false;
    }
    if thir.span != ast.span() {
        return false;
    }
    // For compound forms, also require child spans to match, so a THIR node
    // that merely shares the parent span but has different children (macro
    // expansion or parser desugaring) is not mismatched.
    match (ast, &thir.kind) {
        (
            AstExpr::BinOp {
                op: aop, lhs, rhs, ..
            },
            ThirExprKind::BinOp {
                op: top,
                lhs: tl,
                rhs: tr,
            },
        ) => {
            if aop != top || tl.span != lhs.span() || tr.span != rhs.span() {
                return false;
            }
        }
        (AstExpr::UnaryOp { op: aop, expr, .. }, ThirExprKind::UnaryOp { op: top, expr: te }) => {
            if aop != top || te.span != expr.span() {
                return false;
            }
        }
        (
            AstExpr::FieldAccess {
                base, field: af, ..
            },
            ThirExprKind::Field {
                base: tb,
                field: tf,
                ..
            },
        ) => {
            if af != tf || tb.span != base.span() {
                return false;
            }
        }
        (
            AstExpr::Index { base, indices, .. },
            ThirExprKind::Index {
                base: tb,
                indices: ti,
                ..
            },
        ) => {
            if tb.span != base.span() || ti.len() != indices.len() {
                return false;
            }
            for (a, b) in indices.iter().zip(ti.iter()) {
                if b.span != a.span() {
                    return false;
                }
            }
        }
        (AstExpr::Tuple { elements, .. }, ThirExprKind::Tuple(te)) => {
            if te.len() != elements.len() {
                return false;
            }
            for (a, b) in elements.iter().zip(te.iter()) {
                if b.span != a.span() {
                    return false;
                }
            }
        }
        (AstExpr::MapLiteral { entries, .. }, ThirExprKind::Map { entries: te, .. }) => {
            if te.len() != entries.len() {
                return false;
            }
            for ((ak, av), (tk, tv)) in entries.iter().zip(te.iter()) {
                if tk.span != ak.span() || tv.span != av.span() {
                    return false;
                }
            }
        }
        (AstExpr::Block(ab), ThirExprKind::Block(tb)) => {
            if ab.stmts.len() != tb.stmts.len() || ab.tail.is_some() != tb.tail.is_some() {
                return false;
            }
        }
        (
            AstExpr::NullCoal { expr, default, .. },
            ThirExprKind::NullCoal {
                expr: te,
                default: td,
            },
        ) => {
            if te.span != expr.span() || td.span != default.span() {
                return false;
            }
        }
        (AstExpr::Cast { expr, .. }, ThirExprKind::Cast { expr: te, .. }) => {
            if te.span != expr.span() {
                return false;
            }
        }
        (AstExpr::Try { expr, .. }, ThirExprKind::Try(te)) => {
            if te.span != expr.span() {
                return false;
            }
        }
        (AstExpr::Await { expr, .. }, ThirExprKind::Await(te)) => {
            if te.span != expr.span() {
                return false;
            }
        }
        (
            AstExpr::Call {
                callee: ac,
                args: aa,
                named_args: an,
                ..
            },
            ThirExprKind::Call {
                callee: tc,
                args: ta,
                named_args: tn,
                ..
            },
        ) => {
            if ac.name != *tc || aa.len() != ta.len() || an.len() != tn.len() {
                return false;
            }
            for (a, b) in aa.iter().zip(ta.iter()) {
                if b.span != a.span() {
                    return false;
                }
            }
            for ((an_name, an_expr), (tn_name, tn_expr)) in an.iter().zip(tn.iter()) {
                if an_name != tn_name || tn_expr.span != an_expr.span() {
                    return false;
                }
            }
        }
        (
            AstExpr::MethodCall {
                base: ab,
                method: am,
                args: aa,
                ..
            },
            ThirExprKind::MethodCall {
                receiver: tr,
                method: tm,
                args: ta,
                ..
            },
        ) => {
            if am != tm || ab.span() != tr.span || aa.len() != ta.len() {
                return false;
            }
            for (a, b) in aa.iter().zip(ta.iter()) {
                if b.span != a.span() {
                    return false;
                }
            }
        }
        (
            AstExpr::When {
                scrutinee: ascr,
                arms: aarms,
                ..
            },
            ThirExprKind::When {
                scrutinee: tscr,
                arms: tarms,
                ..
            },
        ) => {
            if ascr.span() != tscr.span || aarms.len() != tarms.len() {
                return false;
            }
        }
        (
            AstExpr::If {
                cond: ac,
                then_block: at,
                else_block: ae,
                ..
            },
            ThirExprKind::If {
                cond: tc,
                then_block: tt,
                else_block: te,
            },
        ) => {
            if ac.span() != tc.span || at.span != tt.span || ae.is_some() != te.is_some() {
                return false;
            }
        }
        (AstExpr::ArrayLit { elems: ae, .. }, ThirExprKind::Array(te)) => {
            if ae.len() != te.len() {
                return false;
            }
            for (a, b) in ae.iter().zip(te.iter()) {
                if b.span != a.span() {
                    return false;
                }
            }
        }
        _ => {}
    }
    match (ast, &thir.kind) {
        (
            AstExpr::IntLit { value: av, .. },
            ThirExprKind::Literal(crate::thir::ThirLiteral::Int(tv)),
        ) => av == tv,
        (
            AstExpr::FloatLit { value: av, .. },
            ThirExprKind::Literal(crate::thir::ThirLiteral::Float(tv)),
        ) => av.to_bits() == tv.to_bits(),
        (
            AstExpr::BoolLit { value: av, .. },
            ThirExprKind::Literal(crate::thir::ThirLiteral::Bool(tv)),
        ) => av == tv,
        (
            AstExpr::StringLit { value: av, .. },
            ThirExprKind::Literal(crate::thir::ThirLiteral::Str(tv)),
        ) => av == tv,
        (AstExpr::Ident(ident), ThirExprKind::Value { name, .. }) => ident.name == *name,
        (AstExpr::UnaryOp { .. }, ThirExprKind::UnaryOp { .. }) => true,
        (AstExpr::BinOp { .. }, ThirExprKind::BinOp { .. }) => true,
        (AstExpr::Tuple { .. }, ThirExprKind::Tuple(_)) => true,
        (AstExpr::TupleIndex { index: ai, .. }, ThirExprKind::TupleIndex { index: ti, .. }) => {
            ai == ti
        }
        (AstExpr::Index { .. }, ThirExprKind::Index { .. }) => true,
        (AstExpr::FieldAccess { .. }, ThirExprKind::Field { .. }) => true,
        (AstExpr::FieldAccess { .. }, ThirExprKind::TupleIndex { .. }) => true,
        (AstExpr::Cast { .. }, ThirExprKind::Cast { .. }) => true,
        (AstExpr::Await { .. }, ThirExprKind::Await(_)) => true,
        (AstExpr::Try { .. }, ThirExprKind::Try(_)) => true,
        (AstExpr::Mask { .. }, ThirExprKind::Mask { .. }) => true,
        (AstExpr::StructLit { name: an, .. }, ThirExprKind::StructLit { name: tn, .. }) => an == tn,
        (
            AstExpr::Raise {
                effect_name: ae, ..
            },
            ThirExprKind::Raise { effect: te, .. },
        ) => ae == te,
        (AstExpr::MethodCall { .. }, ThirExprKind::MethodCall { .. }) => true,
        (AstExpr::Call { .. }, ThirExprKind::Call { .. }) => true,
        (AstExpr::When { .. }, ThirExprKind::When { .. }) => true,
        (AstExpr::If { .. }, ThirExprKind::If { .. }) => true,
        (AstExpr::ArrayLit { .. }, ThirExprKind::Array(_)) => true,
        (AstExpr::Lambda { .. }, ThirExprKind::Lambda { .. }) => true,
        (AstExpr::Handle { .. }, ThirExprKind::Handle { .. }) => true,
        (AstExpr::TryCatch { .. }, ThirExprKind::TryCatch { .. }) => true,
        (AstExpr::MapLiteral { .. }, ThirExprKind::Map { .. }) => true,
        (AstExpr::Block(_), ThirExprKind::Block(_)) => true,
        (AstExpr::NullCoal { .. }, ThirExprKind::NullCoal { .. }) => true,
        (AstExpr::Ref { expr: ae, .. }, ThirExprKind::Ref(te))
        | (AstExpr::RefMut { expr: ae, .. }, ThirExprKind::RefMut(te))
        | (AstExpr::Deref { expr: ae, .. }, ThirExprKind::Deref(te))
        | (AstExpr::Move { expr: ae, .. }, ThirExprKind::Move(te))
        | (AstExpr::Unsafe { body: ae, .. }, ThirExprKind::Unsafe(te))
        | (AstExpr::Splat { expr: ae, .. }, ThirExprKind::Splat(te)) => ae.span() == te.span,
        (AstExpr::MacroCall { name: an, .. }, ThirExprKind::MacroCall { name: tn, .. }) => {
            an.name == *tn
        }
        _ => false,
    }
}

pub(super) fn get_qualified_enum_name(base: &AstExpr) -> Option<String> {
    if let AstExpr::FieldAccess {
        base: inner_base,
        field: inner_field,
        ..
    } = base
    {
        if let AstExpr::Ident(inner_ident) = inner_base.as_ref() {
            let resolved_q = resolve_qualifier(&inner_ident.name);
            return Some(format!("{}__{}", resolved_q, inner_field));
        }
    }
    None
}

pub(super) fn get_qualified_enum_name_thir(base: &crate::thir::ThirExpr) -> Option<String> {
    if let crate::thir::ThirExprKind::Field {
        base: inner_base,
        field: inner_field,
        ..
    } = &base.kind
    {
        if let crate::thir::ThirExprKind::Value {
            name: inner_name, ..
        } = &inner_base.kind
        {
            let resolved_q = resolve_qualifier(inner_name);
            return Some(format!("{}__{}", resolved_q, inner_field));
        }
    }
    None
}

pub(super) fn thir_to_ast(e: &crate::thir::ThirExpr) -> AstExpr {
    match &e.kind {
        crate::thir::ThirExprKind::Value { name, .. } => AstExpr::Ident(Ident {
            name: name.clone(),
            span: e.span,
        }),
        crate::thir::ThirExprKind::Literal(lit) => match lit {
            crate::thir::ThirLiteral::Int(v) => AstExpr::IntLit {
                value: *v,
                span: e.span,
            },
            crate::thir::ThirLiteral::Float(v) => AstExpr::FloatLit {
                value: *v,
                span: e.span,
            },
            crate::thir::ThirLiteral::Bool(v) => AstExpr::BoolLit {
                value: *v,
                span: e.span,
            },
            crate::thir::ThirLiteral::Str(v) => AstExpr::StringLit {
                value: v.clone(),
                span: e.span,
            },
        },
        crate::thir::ThirExprKind::Call {
            callee,
            args,
            named_args,
            ..
        } => AstExpr::Call {
            callee: Ident {
                name: callee.clone(),
                span: e.span,
            },
            args: args.iter().map(thir_to_ast).collect(),
            named_args: named_args
                .iter()
                .map(|(n, a)| (n.clone(), thir_to_ast(a)))
                .collect(),
            span: e.span,
        },
        crate::thir::ThirExprKind::MethodCall {
            receiver,
            method,
            args,
            ..
        } => AstExpr::MethodCall {
            base: Box::new(thir_to_ast(receiver)),
            method: method.clone(),
            args: args.iter().map(thir_to_ast).collect(),
            span: e.span,
        },
        crate::thir::ThirExprKind::Field { base, field, .. } => AstExpr::FieldAccess {
            base: Box::new(thir_to_ast(base)),
            field: field.clone(),
            span: e.span,
        },
        crate::thir::ThirExprKind::TupleIndex { base, index } => AstExpr::TupleIndex {
            base: Box::new(thir_to_ast(base)),
            index: *index,
            span: e.span,
        },
        crate::thir::ThirExprKind::Destructure { base, index } => AstExpr::TupleIndex {
            base: Box::new(thir_to_ast(base)),
            index: *index,
            span: e.span,
        },
        crate::thir::ThirExprKind::BinOp { op, lhs, rhs } => AstExpr::BinOp {
            op: *op,
            lhs: Box::new(thir_to_ast(lhs)),
            rhs: Box::new(thir_to_ast(rhs)),
            span: e.span,
        },
        crate::thir::ThirExprKind::UnaryOp { op, expr } => AstExpr::UnaryOp {
            op: *op,
            expr: Box::new(thir_to_ast(expr)),
            span: e.span,
        },
        crate::thir::ThirExprKind::If {
            cond,
            then_block,
            else_block,
        } => AstExpr::If {
            cond: Box::new(thir_to_ast(cond)),
            then_block: thir_block_to_ast(then_block),
            else_block: else_block.as_ref().map(thir_block_to_ast),
            span: e.span,
        },
        crate::thir::ThirExprKind::Block(b) => AstExpr::Block(thir_block_to_ast(b)),
        crate::thir::ThirExprKind::Index { base, indices } => AstExpr::Index {
            base: Box::new(thir_to_ast(base)),
            indices: indices.iter().map(thir_to_ast).collect(),
            span: e.span,
        },
        crate::thir::ThirExprKind::Cast { expr, ty, .. } => AstExpr::Cast {
            expr: Box::new(thir_to_ast(expr)),
            ty: ty.clone(),
            span: e.span,
        },
        crate::thir::ThirExprKind::StructLit {
            name,
            fields,
            spread,
            ..
        } => AstExpr::StructLit {
            name: name.clone(),
            fields: fields
                .iter()
                .map(|f| (f.name.clone(), thir_to_ast(&f.value)))
                .collect(),
            field_spans: fields.iter().map(|f| f.value.span).collect(),
            spread: spread.as_ref().map(|s| Box::new(thir_to_ast(s))),
            span: e.span,
        },
        crate::thir::ThirExprKind::When { scrutinee, arms } => AstExpr::When {
            scrutinee: Box::new(thir_to_ast(scrutinee)),
            arms: arms.iter().map(thir_arm_to_ast).collect(),
            span: e.span,
        },
        crate::thir::ThirExprKind::Tuple(elems) => AstExpr::Tuple {
            elements: elems.iter().map(thir_to_ast).collect(),
            span: e.span,
        },
        crate::thir::ThirExprKind::Array(elems) => AstExpr::ArrayLit {
            elems: elems.iter().map(thir_to_ast).collect(),
            span: e.span,
        },
        crate::thir::ThirExprKind::Lambda { params, body } => AstExpr::Lambda {
            params: params
                .iter()
                .map(|p| AstParam {
                    name: Ident {
                        name: p.name.clone(),
                        span: e.span,
                    },
                    ty: p
                        .ty
                        .clone()
                        .unwrap_or(AstType::Named("unknown".to_string(), e.span)),
                    default: p.default.as_ref().map(thir_to_ast),
                })
                .collect(),
            body: Box::new(thir_to_ast(body)),
            span: e.span,
        },
        crate::thir::ThirExprKind::Await(inner) => AstExpr::Await {
            expr: Box::new(thir_to_ast(inner)),
            span: e.span,
        },
        crate::thir::ThirExprKind::Try(inner) => AstExpr::Try {
            expr: Box::new(thir_to_ast(inner)),
            span: e.span,
        },
        crate::thir::ThirExprKind::NullCoal { expr, default } => AstExpr::NullCoal {
            expr: Box::new(thir_to_ast(expr)),
            default: Box::new(thir_to_ast(default)),
            span: e.span,
        },
        crate::thir::ThirExprKind::Mask { effects, body } => AstExpr::Mask {
            effects: effects.clone(),
            body: thir_block_to_ast(body),
            span: e.span,
        },
        crate::thir::ThirExprKind::Handle {
            expr,
            arms,
            return_ty,
        } => AstExpr::Handle {
            expr: Box::new(thir_to_ast(expr)),
            arms: arms
                .iter()
                .map(|a| AstHandlerArm {
                    effect_name: a.effect.clone(),
                    params: a
                        .params
                        .iter()
                        .map(|p| Ident {
                            name: p.clone(),
                            span: a.span,
                        })
                        .collect(),
                    resume_param: a.resume_param.as_ref().map(|rp| Ident {
                        name: rp.clone(),
                        span: a.span,
                    }),
                    body: Box::new(thir_to_ast(&a.body)),
                    span: a.span,
                })
                .collect(),
            return_ty: Box::new(return_ty.clone()),
            span: e.span,
        },
        crate::thir::ThirExprKind::Map { entries, .. } => AstExpr::MapLiteral {
            entries: entries
                .iter()
                .map(|(k, v)| (thir_to_ast(k), thir_to_ast(v)))
                .collect(),
            span: e.span,
        },
        crate::thir::ThirExprKind::Ref(inner) => AstExpr::Ref {
            expr: Box::new(thir_to_ast(inner)),
            span: e.span,
        },
        crate::thir::ThirExprKind::RefMut(inner) => AstExpr::RefMut {
            expr: Box::new(thir_to_ast(inner)),
            span: e.span,
        },
        crate::thir::ThirExprKind::Deref(inner) => AstExpr::Deref {
            expr: Box::new(thir_to_ast(inner)),
            span: e.span,
        },
        crate::thir::ThirExprKind::Move(inner) => AstExpr::Move {
            expr: Box::new(thir_to_ast(inner)),
            span: e.span,
        },
        crate::thir::ThirExprKind::Unsafe(inner) => AstExpr::Unsafe {
            body: Box::new(thir_to_ast(inner)),
            span: e.span,
        },
        crate::thir::ThirExprKind::Splat(inner) => AstExpr::Splat {
            expr: Box::new(thir_to_ast(inner)),
            span: e.span,
        },
        crate::thir::ThirExprKind::TryCatch {
            body,
            catch_param,
            catch_body,
        } => AstExpr::TryCatch {
            body: Box::new(thir_to_ast(body)),
            catch_param: catch_param.clone(),
            catch_body: Box::new(thir_to_ast(catch_body)),
            span: e.span,
        },
        crate::thir::ThirExprKind::Raise { effect, args, .. } => AstExpr::Raise {
            effect_name: effect.clone(),
            args: args.iter().map(thir_to_ast).collect(),
            span: e.span,
        },
        crate::thir::ThirExprKind::MacroCall { name, args } => AstExpr::MacroCall {
            name: Ident {
                name: name.clone(),
                span: e.span,
            },
            args: args.iter().map(thir_to_ast).collect(),
            span: e.span,
        },
    }
}

pub(super) fn thir_stmt_to_ast(s: &crate::thir::ThirStmt) -> AstStmt {
    match &s.kind {
        crate::thir::ThirStmtKind::Let {
            name,
            ty,
            init,
            is_var,
        } => AstStmt::Let {
            name: Ident {
                name: name.clone(),
                span: s.span,
            },
            ty: ty.clone(),
            init: Box::new(thir_to_ast(init)),
            is_var: *is_var,
            span: s.span,
        },
        crate::thir::ThirStmtKind::Assign { target, op, value } => AstStmt::Assign {
            target: Box::new(thir_to_ast(target)),
            op: *op,
            value: Box::new(thir_to_ast(value)),
            span: s.span,
        },
        crate::thir::ThirStmtKind::Expr(e) => AstStmt::Expr(Box::new(thir_to_ast(e))),
        crate::thir::ThirStmtKind::Return(value) => AstStmt::Return {
            value: value.as_ref().map(|v| Box::new(thir_to_ast(v))),
            span: s.span,
        },
        crate::thir::ThirStmtKind::Break { label } => AstStmt::Break {
            label: label.clone(),
            span: s.span,
        },
        crate::thir::ThirStmtKind::Continue { label } => AstStmt::Continue {
            label: label.clone(),
            span: s.span,
        },
        crate::thir::ThirStmtKind::While { label, cond, body } => AstStmt::While {
            label: label.clone(),
            cond: Box::new(thir_to_ast(cond)),
            body: thir_block_to_ast(body),
            span: s.span,
        },
        crate::thir::ThirStmtKind::Loop { label, body } => AstStmt::Loop {
            label: label.clone(),
            body: thir_block_to_ast(body),
            span: s.span,
        },
        crate::thir::ThirStmtKind::ForRange {
            label,
            var,
            start,
            end,
            inclusive,
            step,
            body,
        } => AstStmt::ForRange {
            label: label.clone(),
            var: Ident {
                name: var.clone(),
                span: s.span,
            },
            start: Box::new(thir_to_ast(start)),
            end: Box::new(thir_to_ast(end)),
            inclusive: *inclusive,
            step: step.as_ref().map(|st| Box::new(thir_to_ast(st))),
            body: thir_block_to_ast(body),
            span: s.span,
        },
        crate::thir::ThirStmtKind::ForEach {
            label,
            var,
            iter,
            body,
        } => AstStmt::ForEach {
            label: label.clone(),
            var: Ident {
                name: var.clone(),
                span: s.span,
            },
            iter: Box::new(thir_to_ast(iter)),
            body: thir_block_to_ast(body),
            span: s.span,
        },
        crate::thir::ThirStmtKind::ParFor {
            label,
            var,
            start,
            end,
            inclusive,
            body,
        } => AstStmt::ParFor {
            label: label.clone(),
            var: Ident {
                name: var.clone(),
                span: s.span,
            },
            start: Box::new(thir_to_ast(start)),
            end: Box::new(thir_to_ast(end)),
            inclusive: *inclusive,
            body: thir_block_to_ast(body),
            span: s.span,
        },
        crate::thir::ThirStmtKind::Defer(body) => AstStmt::Defer {
            expr: Box::new(thir_to_ast(body)),
            span: s.span,
        },
        crate::thir::ThirStmtKind::Yield(value) => AstStmt::Yield {
            expr: Box::new(thir_to_ast(value)),
            span: s.span,
        },
        crate::thir::ThirStmtKind::Spawn { body, group } => AstStmt::Spawn {
            body: thir_block_to_ast(body).stmts,
            span: s.span,
            group: group.as_ref().map(|g| Box::new(thir_to_ast(g))),
        },
        crate::thir::ThirStmtKind::Mask { effects, body } => AstStmt::MaskStmt {
            effects: effects.clone(),
            body: thir_block_to_ast(body),
            span: s.span,
        },
        crate::thir::ThirStmtKind::Handle {
            expr,
            arms,
            return_ty,
        } => AstStmt::HandleStmt {
            expr: Box::new(thir_to_ast(expr)),
            arms: arms
                .iter()
                .map(|a| AstHandlerArm {
                    effect_name: a.effect.clone(),
                    params: a
                        .params
                        .iter()
                        .map(|p| Ident {
                            name: p.clone(),
                            span: a.span,
                        })
                        .collect(),
                    resume_param: a.resume_param.as_ref().map(|rp| Ident {
                        name: rp.clone(),
                        span: a.span,
                    }),
                    body: Box::new(thir_to_ast(&a.body)),
                    span: a.span,
                })
                .collect(),
            return_ty: Box::new(return_ty.clone()),
            span: s.span,
        },
        crate::thir::ThirStmtKind::Select { arms, default } => AstStmt::Select {
            arms: arms
                .iter()
                .map(|a| crate::parser::ast::SelectArm {
                    channel: thir_to_ast(&a.channel),
                    binding: a.binding.clone(),
                    body: thir_block_to_ast(&a.body),
                    span: a.span,
                })
                .collect(),
            default: default.as_ref().map(|d| Box::new(thir_block_to_ast(d))),
            span: s.span,
        },
    }
}

pub(super) fn thir_block_to_ast(b: &crate::thir::ThirBlock) -> AstBlock {
    AstBlock {
        stmts: b.stmts.iter().map(thir_stmt_to_ast).collect(),
        tail: b.tail.as_ref().map(|t| Box::new(thir_to_ast(t))),
        span: b.span,
    }
}

pub(super) fn thir_arm_to_ast(a: &crate::thir::ThirArm) -> AstWhenArm {
    AstWhenArm {
        pattern: a.pattern.clone(),
        guard: a.guard.as_ref().map(|g| Box::new(thir_to_ast(g))),
        body: Box::new(thir_to_ast(&a.body)),
        span: a.span,
        enum_name: String::new(),
        variant_name: String::new(),
    }
}
