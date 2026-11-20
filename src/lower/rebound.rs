//! AST and THIR rebound-variable, captured-mutation, and taped-inlining analyses.

use crate::parser::ast::{AstBlock, AstExpr, AstStmt};

use super::{synth_marker_at, SynthMarker};

fn push_rebound_name(names: &mut Vec<String>, name: &str) {
    if name != "_" && !names.iter().any(|n| n == name) {
        names.push(name.to_string());
    }
}

pub(super) fn find_rebound_vars_thir(
    then_blk: &crate::thir::ThirBlock,
    else_blk: Option<&crate::thir::ThirBlock>,
) -> Option<Vec<String>> {
    let mut names: Vec<String> = Vec::new();
    collect_rebound_vars_in_thir_block(then_blk, &mut names, true)?;
    if let Some(else_blk) = else_blk {
        for name in find_rebound_vars_thir_block(else_blk, false) {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    // Filter to outer scope happens in caller; here just return all.
    Some(names)
}

pub(super) fn find_rebound_vars_thir_block(
    block: &crate::thir::ThirBlock,
    include_lets: bool,
) -> Vec<String> {
    let mut names = Vec::new();
    collect_rebound_vars_in_thir_block(block, &mut names, include_lets);
    names
}

pub(super) fn collect_rebound_vars_in_thir_block(
    block: &crate::thir::ThirBlock,
    names: &mut Vec<String>,
    include_lets: bool,
) -> Option<()> {
    for stmt in &block.stmts {
        collect_rebound_vars_in_thir_stmt(stmt, names, include_lets)?;
    }
    if let Some(tail) = &block.tail {
        collect_rebound_vars_in_thir_expr(tail, names)?;
    }
    Some(())
}

pub(super) fn collect_rebound_vars_in_thir_stmt(
    stmt: &crate::thir::ThirStmt,
    names: &mut Vec<String>,
    include_lets: bool,
) -> Option<()> {
    match &stmt.kind {
        crate::thir::ThirStmtKind::Let { name, init, .. } => {
            if include_lets {
                push_rebound_name(names, name);
            }
            collect_rebound_vars_in_thir_expr(init, names)?;
            Some(())
        }
        crate::thir::ThirStmtKind::Expr(e) => {
            collect_rebound_vars_in_thir_expr(e, names)?;
            Some(())
        }
        crate::thir::ThirStmtKind::While { cond, body, .. } => {
            collect_rebound_vars_in_thir_expr(cond, names)?;
            collect_rebound_vars_in_thir_block(body, names, false)?;
            Some(())
        }
        crate::thir::ThirStmtKind::Loop { body, .. } => {
            collect_rebound_vars_in_thir_block(body, names, false)?;
            Some(())
        }
        crate::thir::ThirStmtKind::ForRange {
            var,
            start,
            end,
            step,
            body,
            ..
        } => {
            if include_lets {
                push_rebound_name(names, var);
            }
            collect_rebound_vars_in_thir_expr(start, names)?;
            collect_rebound_vars_in_thir_expr(end, names)?;
            if let Some(st) = step {
                collect_rebound_vars_in_thir_expr(st, names)?;
            }
            collect_rebound_vars_in_thir_block(body, names, false)?;
            Some(())
        }
        crate::thir::ThirStmtKind::ParFor {
            var,
            start,
            end,
            body,
            ..
        } => {
            if include_lets {
                push_rebound_name(names, var);
            }
            collect_rebound_vars_in_thir_expr(start, names)?;
            collect_rebound_vars_in_thir_expr(end, names)?;
            collect_rebound_vars_in_thir_block(body, names, false)?;
            Some(())
        }
        crate::thir::ThirStmtKind::ForEach {
            var, iter, body, ..
        } => {
            if include_lets {
                push_rebound_name(names, var);
            }
            collect_rebound_vars_in_thir_expr(iter, names)?;
            collect_rebound_vars_in_thir_block(body, names, false)?;
            Some(())
        }
        crate::thir::ThirStmtKind::Assign { target, value, .. } => {
            let mut cur = target;
            while let crate::thir::ThirExprKind::Field { base, .. }
            | crate::thir::ThirExprKind::TupleIndex { base, .. } = &cur.kind
            {
                cur = base;
            }
            if let crate::thir::ThirExprKind::Value { name, .. } = &cur.kind {
                if !names.contains(name) {
                    names.push(name.clone());
                }
            }
            collect_rebound_vars_in_thir_expr(target, names)?;
            collect_rebound_vars_in_thir_expr(value, names)?;
            Some(())
        }
        crate::thir::ThirStmtKind::Return(value) => {
            if let Some(value) = value {
                collect_rebound_vars_in_thir_expr(value, names)?;
            }
            Some(())
        }
        crate::thir::ThirStmtKind::Spawn { body, group } => {
            collect_rebound_vars_in_thir_block(body, names, false)?;
            if let Some(g) = group {
                collect_rebound_vars_in_thir_expr(g, names)?;
            }
            Some(())
        }
        crate::thir::ThirStmtKind::Mask { body, .. } => {
            collect_rebound_vars_in_thir_block(body, names, false)
        }
        crate::thir::ThirStmtKind::Handle { expr, arms, .. } => {
            collect_rebound_vars_in_thir_expr(expr, names)?;
            for arm in arms {
                collect_rebound_vars_in_thir_expr(&arm.body, names)?;
            }
            Some(())
        }
        crate::thir::ThirStmtKind::Defer(expr) | crate::thir::ThirStmtKind::Yield(expr) => {
            collect_rebound_vars_in_thir_expr(expr, names)
        }
        crate::thir::ThirStmtKind::Select { arms, default } => {
            for arm in arms {
                collect_rebound_vars_in_thir_expr(&arm.channel, names)?;
                collect_rebound_vars_in_thir_block(&arm.body, names, false)?;
            }
            if let Some(d) = default {
                collect_rebound_vars_in_thir_block(d, names, false)?;
            }
            Some(())
        }
        crate::thir::ThirStmtKind::Break { .. } | crate::thir::ThirStmtKind::Continue { .. } => {
            Some(())
        }
    }
}

pub(super) fn collect_rebound_vars_in_thir_expr(
    expr: &crate::thir::ThirExpr,
    names: &mut Vec<String>,
) -> Option<()> {
    match &expr.kind {
        crate::thir::ThirExprKind::Literal(_) | crate::thir::ThirExprKind::Value { .. } => Some(()),
        crate::thir::ThirExprKind::BinOp { lhs, rhs, .. } => {
            collect_rebound_vars_in_thir_expr(lhs, names)?;
            collect_rebound_vars_in_thir_expr(rhs, names)?;
            Some(())
        }
        crate::thir::ThirExprKind::UnaryOp { expr, .. }
        | crate::thir::ThirExprKind::Cast { expr, .. }
        | crate::thir::ThirExprKind::Await(expr)
        | crate::thir::ThirExprKind::Try(expr)
        | crate::thir::ThirExprKind::Ref(expr)
        | crate::thir::ThirExprKind::RefMut(expr)
        | crate::thir::ThirExprKind::Deref(expr)
        | crate::thir::ThirExprKind::Move(expr)
        | crate::thir::ThirExprKind::Unsafe(expr)
        | crate::thir::ThirExprKind::Splat(expr) => {
            collect_rebound_vars_in_thir_expr(expr, names)?;
            Some(())
        }
        crate::thir::ThirExprKind::Call {
            args, named_args, ..
        } => {
            for a in args {
                collect_rebound_vars_in_thir_expr(a, names)?;
            }
            for (_, a) in named_args {
                collect_rebound_vars_in_thir_expr(a, names)?;
            }
            Some(())
        }
        crate::thir::ThirExprKind::MethodCall { receiver, args, .. } => {
            collect_rebound_vars_in_thir_expr(receiver, names)?;
            for a in args {
                collect_rebound_vars_in_thir_expr(a, names)?;
            }
            Some(())
        }
        crate::thir::ThirExprKind::Field { base, .. }
        | crate::thir::ThirExprKind::TupleIndex { base, .. }
        | crate::thir::ThirExprKind::Destructure { base, .. } => {
            collect_rebound_vars_in_thir_expr(base, names)?;
            Some(())
        }
        crate::thir::ThirExprKind::Mask { body, .. } => {
            collect_rebound_vars_in_thir_block(body, names, false)?;
            Some(())
        }
        crate::thir::ThirExprKind::StructLit { fields, spread, .. } => {
            for f in fields {
                collect_rebound_vars_in_thir_expr(&f.value, names)?;
            }
            if let Some(s) = spread {
                collect_rebound_vars_in_thir_expr(s, names)?;
            }
            Some(())
        }
        crate::thir::ThirExprKind::Raise { args, .. } => {
            for a in args {
                collect_rebound_vars_in_thir_expr(a, names)?;
            }
            Some(())
        }
        crate::thir::ThirExprKind::If {
            cond,
            then_block,
            else_block,
        } => {
            collect_rebound_vars_in_thir_expr(cond, names)?;
            collect_rebound_vars_in_thir_block(then_block, names, false)?;
            if let Some(else_block) = else_block {
                collect_rebound_vars_in_thir_block(else_block, names, false)?;
            }
            Some(())
        }
        crate::thir::ThirExprKind::Block(b) => collect_rebound_vars_in_thir_block(b, names, false),
        crate::thir::ThirExprKind::When {
            scrutinee, arms, ..
        } => {
            collect_rebound_vars_in_thir_expr(scrutinee, names)?;
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    collect_rebound_vars_in_thir_expr(guard, names)?;
                }
                collect_rebound_vars_in_thir_expr(&arm.body, names)?;
            }
            Some(())
        }
        crate::thir::ThirExprKind::Handle { expr, arms, .. } => {
            collect_rebound_vars_in_thir_expr(expr, names)?;
            for arm in arms {
                collect_rebound_vars_in_thir_expr(&arm.body, names)?;
            }
            Some(())
        }
        crate::thir::ThirExprKind::TryCatch {
            body, catch_body, ..
        } => {
            collect_rebound_vars_in_thir_expr(body, names)?;
            collect_rebound_vars_in_thir_expr(catch_body, names)?;
            Some(())
        }
        crate::thir::ThirExprKind::NullCoal { expr, default } => {
            collect_rebound_vars_in_thir_expr(expr, names)?;
            collect_rebound_vars_in_thir_expr(default, names)?;
            Some(())
        }
        crate::thir::ThirExprKind::Map { entries, .. } => {
            for (k, v) in entries {
                collect_rebound_vars_in_thir_expr(k, names)?;
                collect_rebound_vars_in_thir_expr(v, names)?;
            }
            Some(())
        }
        crate::thir::ThirExprKind::Tuple(elements) | crate::thir::ThirExprKind::Array(elements) => {
            for el in elements {
                collect_rebound_vars_in_thir_expr(el, names)?;
            }
            Some(())
        }
        crate::thir::ThirExprKind::Index { base, indices } => {
            collect_rebound_vars_in_thir_expr(base, names)?;
            for i in indices {
                collect_rebound_vars_in_thir_expr(i, names)?;
            }
            Some(())
        }
        crate::thir::ThirExprKind::MacroCall { args, .. } => {
            for a in args {
                collect_rebound_vars_in_thir_expr(a, names)?;
            }
            Some(())
        }
        crate::thir::ThirExprKind::Lambda { body, .. } => {
            collect_rebound_vars_in_thir_expr(body, names)
        }
    }
}

/// True when `block` is built only from forms this inliner fully traverses.
///
/// A whitelist, not a `return` detector, and deliberately so. A `return` can
/// only be reached through a block, and blocks appear in exactly two expression
/// forms (`If` and `Mask`) -- but reaching those means recursing through every
/// expression variant, and the nearest existing walker
/// (`collect_rebound_vars_in_expr`) covers 33 of roughly a hundred. Missing one
/// branch would mean inlining a body whose `return` terminates the *caller*: a
/// miscompile, not a missed optimisation. Whitelisting inverts the failure --
/// an unrecognised form declines the inline, and the program merely gets the
/// same error it did before.
pub(super) fn taped_inline_ok_block(block: &AstBlock) -> bool {
    taped_inline_ok_block_with_terminal_return(block, true)
}

/// Checks a block used by taped-call inlining.
///
/// Only the callee's outermost block may use a final `return expr` as its
/// result. A final return in an `if` arm is still an early return from the
/// callee, and lowering that arm inline would emit a return from the caller.
pub(super) fn taped_inline_ok_block_with_terminal_return(
    block: &AstBlock,
    allow_terminal_return: bool,
) -> bool {
    let (stmts, has_terminal_return) = if allow_terminal_return && block.tail.is_none() {
        match block.stmts.split_last() {
            Some((
                AstStmt::Return {
                    value: Some(value), ..
                },
                prefix,
            )) => (prefix, taped_inline_ok_expr(value)),
            _ => (block.stmts.as_slice(), false),
        }
    } else {
        (block.stmts.as_slice(), false)
    };
    let stmts_ok = stmts.iter().all(|st| match st {
        AstStmt::Let { init, .. } => taped_inline_ok_expr(init),
        AstStmt::LetTuple { init, .. } => taped_inline_ok_expr(init),
        AstStmt::Expr(e) => taped_inline_ok_expr(e),
        AstStmt::Assign { target, value, .. } => {
            taped_inline_ok_expr(target) && taped_inline_ok_expr(value)
        }
        // `return` is the case this exists to reject. `break`/`continue` are
        // rejected because in an inlined body they would bind to whatever loop
        // encloses the *call site*. Everything else is simply not traversed.
        _ => false,
    });
    stmts_ok && (block.tail.as_deref().is_some_and(taped_inline_ok_expr) || has_terminal_return)
}

pub(super) fn taped_inline_ok_expr(expr: &AstExpr) -> bool {
    match expr {
        AstExpr::Ident(_)
        | AstExpr::IntLit { .. }
        | AstExpr::FloatLit { .. }
        | AstExpr::BoolLit { .. }
        | AstExpr::StringLit { .. } => true,
        AstExpr::BinOp { lhs, rhs, .. } => taped_inline_ok_expr(lhs) && taped_inline_ok_expr(rhs),
        AstExpr::UnaryOp { expr, .. } => taped_inline_ok_expr(expr),
        AstExpr::Cast { expr, .. } => taped_inline_ok_expr(expr),
        AstExpr::FieldAccess { base, .. } => taped_inline_ok_expr(base),
        AstExpr::Tuple { elements, .. } => elements.iter().all(taped_inline_ok_expr),
        AstExpr::Index { base, indices, .. } => {
            taped_inline_ok_expr(base) && indices.iter().all(taped_inline_ok_expr)
        }
        AstExpr::Call {
            args, named_args, ..
        } => {
            args.iter().all(taped_inline_ok_expr)
                && named_args.iter().all(|(_, e)| taped_inline_ok_expr(e))
        }
        AstExpr::MethodCall { base, args, .. } => {
            taped_inline_ok_expr(base) && args.iter().all(taped_inline_ok_expr)
        }
        // The one block-bearing form worth supporting: a helper whose body is
        // `if cond { a } else { b }`. Both arms are checked by the same rule.
        AstExpr::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            taped_inline_ok_expr(cond)
                && taped_inline_ok_block_with_terminal_return(then_block, false)
                && else_block.as_ref().map_or(true, |block| {
                    taped_inline_ok_block_with_terminal_return(block, false)
                })
        }
        _ => false,
    }
}

/// Scans a block for local variable names whose `&mut name` reference is taken.
///
/// Used during lowering to allocate mutable cell slots for those variables so
/// `*ref = val` writes propagate back to reads of the original binding. Nested
/// `Lambda`, `Spawn`, and `ParFor` bodies form separate lowered functions and are
/// excluded so shadowed parameter names do not pollute outer scopes.
pub(super) fn find_mut_borrowed_vars(block: &AstBlock) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    collect_mut_borrowed_in_block(block, &mut names);
    names
}

/// Expression-level sibling of [`find_mut_borrowed_vars`] for lambda bodies.
pub(super) fn find_mut_borrowed_vars_expr(expr: &AstExpr) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    collect_mut_borrowed_in_expr(expr, &mut names);
    names
}

/// THIR expression sibling of [`find_mut_borrowed_vars_expr`].
pub(super) fn find_mut_borrowed_vars_thir_expr(
    expr: &crate::thir::ThirExpr,
) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    collect_mut_borrowed_in_thir_expr(expr, &mut names);
    names
}

fn collect_mut_borrowed_in_block(block: &AstBlock, out: &mut std::collections::HashSet<String>) {
    if block.stmts.is_empty() && block.tail.is_none() {
        match synth_marker_at(block.span) {
            Some(SynthMarker::Block(tb)) => {
                collect_mut_borrowed_in_thir_block(&tb, out);
                return;
            }
            Some(SynthMarker::Expr(e)) => {
                collect_mut_borrowed_in_thir_expr(&e, out);
                return;
            }
            None => {
                if let Some(tb) = crate::thir::block_at(block.span) {
                    collect_mut_borrowed_in_thir_block(&tb, out);
                    return;
                }
            }
        }
    }
    for stmt in &block.stmts {
        collect_mut_borrowed_in_stmt(stmt, out);
    }
    if let Some(tail) = &block.tail {
        collect_mut_borrowed_in_expr(tail, out);
    }
}

fn collect_mut_borrowed_in_stmt(stmt: &AstStmt, out: &mut std::collections::HashSet<String>) {
    match stmt {
        AstStmt::Let { init, .. } | AstStmt::LetTuple { init, .. } => {
            collect_mut_borrowed_in_expr(init, out)
        }
        AstStmt::Expr(expr) | AstStmt::Defer { expr, .. } | AstStmt::Yield { expr, .. } => {
            collect_mut_borrowed_in_expr(expr, out)
        }
        AstStmt::Assign { target, value, .. } => {
            collect_mut_borrowed_in_expr(target, out);
            collect_mut_borrowed_in_expr(value, out);
        }
        AstStmt::Return { value, .. } => {
            if let Some(e) = value {
                collect_mut_borrowed_in_expr(e, out);
            }
        }
        AstStmt::While { cond, body, .. } => {
            collect_mut_borrowed_in_expr(cond, out);
            collect_mut_borrowed_in_block(body, out);
        }
        AstStmt::Loop { body, .. } | AstStmt::MaskStmt { body, .. } => {
            collect_mut_borrowed_in_block(body, out);
        }
        AstStmt::ForRange {
            start,
            end,
            step,
            body,
            ..
        } => {
            collect_mut_borrowed_in_expr(start, out);
            collect_mut_borrowed_in_expr(end, out);
            if let Some(s) = step {
                collect_mut_borrowed_in_expr(s, out);
            }
            collect_mut_borrowed_in_block(body, out);
        }
        AstStmt::ForEach { iter, body, .. } => {
            collect_mut_borrowed_in_expr(iter, out);
            collect_mut_borrowed_in_block(body, out);
        }
        AstStmt::HandleStmt { expr, arms, .. } => {
            collect_mut_borrowed_in_expr(expr, out);
            for arm in arms {
                collect_mut_borrowed_in_expr(&arm.body, out);
            }
        }
        AstStmt::Select { arms, default, .. } => {
            for arm in arms {
                collect_mut_borrowed_in_expr(&arm.channel, out);
                collect_mut_borrowed_in_block(&arm.body, out);
            }
            if let Some(d) = default {
                collect_mut_borrowed_in_block(d, out);
            }
        }
        AstStmt::Break { .. }
        | AstStmt::Continue { .. }
        | AstStmt::Spawn { .. }
        | AstStmt::ParFor { .. } => {}
    }
}

pub(super) fn unwrap_mut_borrow_target_ast(expr: &AstExpr) -> Option<&str> {
    match expr {
        AstExpr::Ident(id) => Some(&id.name),
        AstExpr::Unsafe { body, .. } => unwrap_mut_borrow_target_ast(body),
        AstExpr::Block(b) if b.stmts.is_empty() => {
            b.tail.as_deref().and_then(unwrap_mut_borrow_target_ast)
        }
        _ => None,
    }
}

fn collect_mut_borrowed_in_expr(expr: &AstExpr, out: &mut std::collections::HashSet<String>) {
    match expr {
        AstExpr::RefMut { expr: inner, .. } => {
            if let Some(name) = unwrap_mut_borrow_target_ast(inner) {
                out.insert(name.to_string());
            }
            collect_mut_borrowed_in_expr(inner, out);
        }
        AstExpr::UnaryOp { expr: inner, .. }
        | AstExpr::Cast { expr: inner, .. }
        | AstExpr::FieldAccess { base: inner, .. }
        | AstExpr::TupleIndex { base: inner, .. }
        | AstExpr::Await { expr: inner, .. }
        | AstExpr::Try { expr: inner, .. }
        | AstExpr::Ref { expr: inner, .. }
        | AstExpr::Deref { expr: inner, .. }
        | AstExpr::Move { expr: inner, .. }
        | AstExpr::Splat { expr: inner, .. }
        | AstExpr::Unsafe { body: inner, .. } => collect_mut_borrowed_in_expr(inner, out),
        AstExpr::BinOp { lhs, rhs, .. }
        | AstExpr::NullCoal {
            expr: lhs,
            default: rhs,
            ..
        }
        | AstExpr::TryCatch {
            body: lhs,
            catch_body: rhs,
            ..
        } => {
            collect_mut_borrowed_in_expr(lhs, out);
            collect_mut_borrowed_in_expr(rhs, out);
        }
        AstExpr::Call {
            args, named_args, ..
        } => {
            for a in args {
                collect_mut_borrowed_in_expr(a, out);
            }
            for (_, a) in named_args {
                collect_mut_borrowed_in_expr(a, out);
            }
        }
        AstExpr::MethodCall { base, args, .. } => {
            collect_mut_borrowed_in_expr(base, out);
            for a in args {
                collect_mut_borrowed_in_expr(a, out);
            }
        }
        AstExpr::Raise { args, .. } | AstExpr::MacroCall { args, .. } => {
            for a in args {
                collect_mut_borrowed_in_expr(a, out);
            }
        }
        AstExpr::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            collect_mut_borrowed_in_expr(cond, out);
            collect_mut_borrowed_in_block(then_block, out);
            if let Some(eb) = else_block {
                collect_mut_borrowed_in_block(eb, out);
            }
        }
        AstExpr::Block(b) | AstExpr::Mask { body: b, .. } => {
            collect_mut_borrowed_in_block(b, out);
        }
        AstExpr::Index { base, indices, .. } => {
            collect_mut_borrowed_in_expr(base, out);
            for i in indices {
                collect_mut_borrowed_in_expr(i, out);
            }
        }
        AstExpr::StructLit { fields, spread, .. } => {
            for (_, v) in fields {
                collect_mut_borrowed_in_expr(v, out);
            }
            if let Some(s) = spread {
                collect_mut_borrowed_in_expr(s, out);
            }
        }
        AstExpr::When {
            scrutinee, arms, ..
        } => {
            collect_mut_borrowed_in_expr(scrutinee, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_mut_borrowed_in_expr(g, out);
                }
                collect_mut_borrowed_in_expr(&arm.body, out);
            }
        }
        AstExpr::Handle { expr, arms, .. } => {
            collect_mut_borrowed_in_expr(expr, out);
            for arm in arms {
                collect_mut_borrowed_in_expr(&arm.body, out);
            }
        }
        AstExpr::Tuple { elements, .. }
        | AstExpr::ArrayLit {
            elems: elements, ..
        } => {
            for e in elements {
                collect_mut_borrowed_in_expr(e, out);
            }
        }
        AstExpr::MapLiteral { entries, .. } => {
            for (k, v) in entries {
                collect_mut_borrowed_in_expr(k, out);
                collect_mut_borrowed_in_expr(v, out);
            }
        }
        AstExpr::Ident(_)
        | AstExpr::IntLit { .. }
        | AstExpr::FloatLit { .. }
        | AstExpr::BoolLit { .. }
        | AstExpr::StringLit { .. }
        | AstExpr::Lambda { .. } => {}
    }
}

fn collect_mut_borrowed_in_thir_block(
    block: &crate::thir::ThirBlock,
    out: &mut std::collections::HashSet<String>,
) {
    for stmt in &block.stmts {
        collect_mut_borrowed_in_thir_stmt(stmt, out);
    }
    if let Some(tail) = &block.tail {
        collect_mut_borrowed_in_thir_expr(tail, out);
    }
}

fn collect_mut_borrowed_in_thir_stmt(
    stmt: &crate::thir::ThirStmt,
    out: &mut std::collections::HashSet<String>,
) {
    use crate::thir::ThirStmtKind;
    match &stmt.kind {
        ThirStmtKind::Let { init, .. } => collect_mut_borrowed_in_thir_expr(init, out),
        ThirStmtKind::Expr(e) | ThirStmtKind::Defer(e) | ThirStmtKind::Yield(e) => {
            collect_mut_borrowed_in_thir_expr(e, out)
        }
        ThirStmtKind::Assign { target, value, .. } => {
            collect_mut_borrowed_in_thir_expr(target, out);
            collect_mut_borrowed_in_thir_expr(value, out);
        }
        ThirStmtKind::Return(value) => {
            if let Some(e) = value {
                collect_mut_borrowed_in_thir_expr(e, out);
            }
        }
        ThirStmtKind::While { cond, body, .. } => {
            collect_mut_borrowed_in_thir_expr(cond, out);
            collect_mut_borrowed_in_thir_block(body, out);
        }
        ThirStmtKind::Loop { body, .. } | ThirStmtKind::Mask { body, .. } => {
            collect_mut_borrowed_in_thir_block(body, out);
        }
        ThirStmtKind::ForRange {
            start,
            end,
            step,
            body,
            ..
        } => {
            collect_mut_borrowed_in_thir_expr(start, out);
            collect_mut_borrowed_in_thir_expr(end, out);
            if let Some(s) = step {
                collect_mut_borrowed_in_thir_expr(s, out);
            }
            collect_mut_borrowed_in_thir_block(body, out);
        }
        ThirStmtKind::ForEach { iter, body, .. } => {
            collect_mut_borrowed_in_thir_expr(iter, out);
            collect_mut_borrowed_in_thir_block(body, out);
        }
        ThirStmtKind::Handle { expr, arms, .. } => {
            collect_mut_borrowed_in_thir_expr(expr, out);
            for arm in arms {
                collect_mut_borrowed_in_thir_expr(&arm.body, out);
            }
        }
        ThirStmtKind::Select { arms, default } => {
            for arm in arms {
                collect_mut_borrowed_in_thir_expr(&arm.channel, out);
                collect_mut_borrowed_in_thir_block(&arm.body, out);
            }
            if let Some(d) = default {
                collect_mut_borrowed_in_thir_block(d, out);
            }
        }
        ThirStmtKind::Break { .. }
        | ThirStmtKind::Continue { .. }
        | ThirStmtKind::Spawn { .. }
        | ThirStmtKind::ParFor { .. } => {}
    }
}

pub(super) fn unwrap_mut_borrow_target_thir(expr: &crate::thir::ThirExpr) -> Option<&str> {
    use crate::thir::ThirExprKind;
    match &expr.kind {
        ThirExprKind::Value { name, .. } => Some(name.as_str()),
        ThirExprKind::Unsafe(inner) => unwrap_mut_borrow_target_thir(inner),
        ThirExprKind::Block(b) if b.stmts.is_empty() => {
            b.tail.as_deref().and_then(unwrap_mut_borrow_target_thir)
        }
        _ => None,
    }
}

fn collect_mut_borrowed_in_thir_expr(
    expr: &crate::thir::ThirExpr,
    out: &mut std::collections::HashSet<String>,
) {
    use crate::thir::ThirExprKind;
    match &expr.kind {
        ThirExprKind::RefMut(inner) => {
            if let Some(name) = unwrap_mut_borrow_target_thir(inner) {
                out.insert(name.to_string());
            }
            collect_mut_borrowed_in_thir_expr(inner, out);
        }
        ThirExprKind::UnaryOp { expr: inner, .. }
        | ThirExprKind::Cast { expr: inner, .. }
        | ThirExprKind::Field { base: inner, .. }
        | ThirExprKind::TupleIndex { base: inner, .. }
        | ThirExprKind::Destructure { base: inner, .. }
        | ThirExprKind::Await(inner)
        | ThirExprKind::Try(inner)
        | ThirExprKind::Ref(inner)
        | ThirExprKind::Deref(inner)
        | ThirExprKind::Move(inner)
        | ThirExprKind::Splat(inner)
        | ThirExprKind::Unsafe(inner) => collect_mut_borrowed_in_thir_expr(inner, out),
        ThirExprKind::BinOp { lhs, rhs, .. }
        | ThirExprKind::NullCoal {
            expr: lhs,
            default: rhs,
        }
        | ThirExprKind::TryCatch {
            body: lhs,
            catch_body: rhs,
            ..
        } => {
            collect_mut_borrowed_in_thir_expr(lhs, out);
            collect_mut_borrowed_in_thir_expr(rhs, out);
        }
        ThirExprKind::Call {
            args, named_args, ..
        } => {
            for a in args {
                collect_mut_borrowed_in_thir_expr(a, out);
            }
            for (_, a) in named_args {
                collect_mut_borrowed_in_thir_expr(a, out);
            }
        }
        ThirExprKind::MethodCall { receiver, args, .. } => {
            collect_mut_borrowed_in_thir_expr(receiver, out);
            for a in args {
                collect_mut_borrowed_in_thir_expr(a, out);
            }
        }
        ThirExprKind::Raise { args, .. } | ThirExprKind::MacroCall { args, .. } => {
            for a in args {
                collect_mut_borrowed_in_thir_expr(a, out);
            }
        }
        ThirExprKind::If {
            cond,
            then_block,
            else_block,
        } => {
            collect_mut_borrowed_in_thir_expr(cond, out);
            collect_mut_borrowed_in_thir_block(then_block, out);
            if let Some(eb) = else_block {
                collect_mut_borrowed_in_thir_block(eb, out);
            }
        }
        ThirExprKind::Block(b) | ThirExprKind::Mask { body: b, .. } => {
            collect_mut_borrowed_in_thir_block(b, out);
        }
        ThirExprKind::Index { base, indices } => {
            collect_mut_borrowed_in_thir_expr(base, out);
            for i in indices {
                collect_mut_borrowed_in_thir_expr(i, out);
            }
        }
        ThirExprKind::StructLit { fields, spread, .. } => {
            for f in fields {
                collect_mut_borrowed_in_thir_expr(&f.value, out);
            }
            if let Some(s) = spread {
                collect_mut_borrowed_in_thir_expr(s, out);
            }
        }
        ThirExprKind::When { scrutinee, arms } => {
            collect_mut_borrowed_in_thir_expr(scrutinee, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_mut_borrowed_in_thir_expr(g, out);
                }
                collect_mut_borrowed_in_thir_expr(&arm.body, out);
            }
        }
        ThirExprKind::Handle { expr, arms, .. } => {
            collect_mut_borrowed_in_thir_expr(expr, out);
            for arm in arms {
                collect_mut_borrowed_in_thir_expr(&arm.body, out);
            }
        }
        ThirExprKind::Tuple(elems) | ThirExprKind::Array(elems) => {
            for e in elems {
                collect_mut_borrowed_in_thir_expr(e, out);
            }
        }
        ThirExprKind::Map { entries, .. } => {
            for (k, v) in entries {
                collect_mut_borrowed_in_thir_expr(k, out);
                collect_mut_borrowed_in_thir_expr(v, out);
            }
        }
        ThirExprKind::Value { .. } | ThirExprKind::Literal(_) | ThirExprKind::Lambda { .. } => {}
    }
}

/// Scans a block for variables that get rebound, returning unique names.
///
/// At the direct level: includes `val`/`var` binding names, `x = expr`
/// targets, and `for`-loop variables.
/// In nested blocks: recursively collects `x = expr` mutations so that outer
/// variables modified inside inner loops are threaded through as SSA params.
pub(super) fn find_rebound_vars(block: &AstBlock) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    if block.stmts.is_empty() && block.tail.is_none() {
        // Synthetic marker (see lower_expr): walk the THIR block it stands for.
        let marker = synth_marker_at(block.span);
        let tb = match marker {
            Some(SynthMarker::Block(tb)) => Some(tb),
            Some(SynthMarker::Expr(_)) => None,
            None => crate::thir::block_at(block.span),
        };
        if let Some(tb) = tb {
            let _ = collect_rebound_vars_in_thir_block(&tb, &mut names, true);
            names.sort();
            names.dedup();
            return names;
        }
    }
    collect_rebound_vars_in_block(block, &mut names, true);
    names
}

pub(super) fn collect_rebound_vars_in_block(
    block: &AstBlock,
    names: &mut Vec<String>,
    include_lets: bool,
) {
    if block.stmts.is_empty() && block.tail.is_none() {
        // Synthetic marker (see lower_expr): facts live in the THIR registry
        // payload, not in the AST.
        match synth_marker_at(block.span) {
            Some(SynthMarker::Block(tb)) => {
                let _ = collect_rebound_vars_in_thir_block(&tb, names, include_lets);
                return;
            }
            Some(SynthMarker::Expr(e)) => {
                collect_rebound_vars_in_thir_expr(&e, names)
                    .map(|_| ())
                    .unwrap_or(());
                return;
            }
            None => {
                if let Some(tb) = crate::thir::block_at(block.span) {
                    let _ = collect_rebound_vars_in_thir_block(&tb, names, include_lets);
                    return;
                }
            }
        }
    }
    for stmt in &block.stmts {
        collect_rebound_vars_in_stmt(stmt, names, include_lets);
    }
    if let Some(tail) = &block.tail {
        collect_rebound_vars_in_expr(tail, names);
    }
}

pub(super) fn collect_rebound_vars_in_stmt(
    stmt: &AstStmt,
    names: &mut Vec<String>,
    include_lets: bool,
) {
    match stmt {
        AstStmt::Let { name, init, .. } => {
            if include_lets {
                push_rebound_name(names, &name.name);
            }
            collect_rebound_vars_in_expr(init, names);
        }
        AstStmt::Expr(expr) => collect_rebound_vars_in_expr(expr, names),
        AstStmt::While { cond, body, .. } => {
            collect_rebound_vars_in_expr(cond, names);
            collect_rebound_vars_in_block(body, names, false);
        }
        AstStmt::Loop { body, .. } => {
            collect_rebound_vars_in_block(body, names, false);
        }
        AstStmt::Break { .. } | AstStmt::Continue { .. } => {}
        AstStmt::ForRange {
            var,
            start,
            end,
            body,
            ..
        }
        | AstStmt::ParFor {
            var,
            start,
            end,
            body,
            ..
        } => {
            if include_lets {
                push_rebound_name(names, &var.name);
            }
            collect_rebound_vars_in_expr(start, names);
            collect_rebound_vars_in_expr(end, names);
            collect_rebound_vars_in_block(body, names, false);
        }
        AstStmt::Assign { target, value, .. } => {
            let mut cur = target.as_ref();
            while let AstExpr::FieldAccess { base, .. } = cur {
                cur = base.as_ref();
            }
            if let AstExpr::Ident(ident) = cur {
                push_rebound_name(names, &ident.name);
            }
            collect_rebound_vars_in_expr(target, names);
            collect_rebound_vars_in_expr(value, names);
        }
        AstStmt::LetTuple {
            names: tuple_names,
            init,
            ..
        } => {
            if include_lets {
                for name in tuple_names {
                    push_rebound_name(names, &name.name);
                }
            }
            collect_rebound_vars_in_expr(init, names);
        }
        AstStmt::Return { value, .. } => {
            if let Some(value) = value {
                collect_rebound_vars_in_expr(value, names);
            }
        }
        AstStmt::Spawn { body, .. } => {
            for stmt in body {
                collect_rebound_vars_in_stmt(stmt, names, false);
            }
        }
        AstStmt::ForEach {
            var, iter, body, ..
        } => {
            if include_lets {
                push_rebound_name(names, &var.name);
            }
            collect_rebound_vars_in_expr(iter, names);
            collect_rebound_vars_in_block(body, names, false);
        }
        AstStmt::MaskStmt { body, .. } => collect_rebound_vars_in_block(body, names, false),
        AstStmt::HandleStmt { expr, .. } => collect_rebound_vars_in_expr(expr, names),
        AstStmt::Defer { expr, .. } => collect_rebound_vars_in_expr(expr, names),
        AstStmt::Yield { expr, .. } => {
            collect_rebound_vars_in_expr(expr, names);
        }
        AstStmt::Select { arms, default, .. } => {
            for arm in arms {
                collect_rebound_vars_in_expr(&arm.channel, names);
                collect_rebound_vars_in_block(&arm.body, names, false);
            }
            if let Some(d) = default {
                collect_rebound_vars_in_block(d, names, false);
            }
        }
    }
}

/// Builtins that mutate a collection in place. First argument is the target.
const MUTATING_COLLECTION_BUILTINS: &[&str] = &[
    "push",
    "pop",
    "list_push",
    "list_pop",
    "list_set",
    "list_insert",
    "list_remove",
    "list_sort",
    "map_set",
    "map_insert",
    "map_remove",
    "set",
];

/// Find the first call of a mutating collection builtin whose target is one of
/// `captured`. Records `(collection name, builtin name)`.
///
/// Deliberately conservative: it only inspects statement and expression forms
/// that can contain a call, so an unhandled form yields a missed violation
/// rather than a spurious one.
pub(super) fn find_captured_mutation(
    block: &AstBlock,
    captured: &std::collections::HashSet<&str>,
    out: &mut Option<(String, String)>,
) {
    for stmt in &block.stmts {
        find_captured_mutation_stmt(stmt, captured, out);
    }
    if let Some(tail) = &block.tail {
        find_captured_mutation_expr(tail, captured, out);
    }
}

pub(super) fn find_captured_mutation_stmt(
    stmt: &AstStmt,
    captured: &std::collections::HashSet<&str>,
    out: &mut Option<(String, String)>,
) {
    if out.is_some() {
        return;
    }
    match stmt {
        AstStmt::Expr(e) => find_captured_mutation_expr(e, captured, out),
        AstStmt::Let { init, .. } => find_captured_mutation_expr(init, captured, out),
        AstStmt::Assign { value, .. } => find_captured_mutation_expr(value, captured, out),
        AstStmt::Return { value: Some(e), .. } => find_captured_mutation_expr(e, captured, out),
        AstStmt::While { body, cond, .. } => {
            find_captured_mutation_expr(cond, captured, out);
            find_captured_mutation(body, captured, out);
        }
        AstStmt::Loop { body, .. } => find_captured_mutation(body, captured, out),
        AstStmt::ForRange { body, .. } => find_captured_mutation(body, captured, out),
        AstStmt::ForEach { body, .. } => find_captured_mutation(body, captured, out),
        AstStmt::ParFor { body, .. } => find_captured_mutation(body, captured, out),
        _ => {}
    }
}

pub(super) fn find_captured_mutation_expr(
    expr: &AstExpr,
    captured: &std::collections::HashSet<&str>,
    out: &mut Option<(String, String)>,
) {
    if out.is_some() {
        return;
    }
    match expr {
        AstExpr::Call { callee, args, .. } => {
            if MUTATING_COLLECTION_BUILTINS.contains(&callee.name.as_str()) {
                if let Some(AstExpr::Ident(target)) = args.first() {
                    if captured.contains(target.name.as_str()) {
                        *out = Some((target.name.clone(), callee.name.clone()));
                        return;
                    }
                }
            }
            for a in args {
                find_captured_mutation_expr(a, captured, out);
            }
        }
        AstExpr::BinOp { lhs, rhs, .. } => {
            find_captured_mutation_expr(lhs, captured, out);
            find_captured_mutation_expr(rhs, captured, out);
        }
        AstExpr::UnaryOp { expr, .. } => find_captured_mutation_expr(expr, captured, out),
        AstExpr::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            find_captured_mutation_expr(cond, captured, out);
            find_captured_mutation(then_block, captured, out);
            if let Some(eb) = else_block {
                find_captured_mutation(eb, captured, out);
            }
        }
        AstExpr::Block(b) => find_captured_mutation(b, captured, out),
        _ => {}
    }
}

pub(super) fn find_captured_mutation_thir(
    block: &crate::thir::ThirBlock,
    captured: &std::collections::HashSet<&str>,
    out: &mut Option<(String, String)>,
) {
    for stmt in &block.stmts {
        find_captured_mutation_thir_stmt(stmt, captured, out);
    }
    if let Some(tail) = &block.tail {
        find_captured_mutation_thir_expr(tail, captured, out);
    }
}

pub(super) fn find_captured_mutation_thir_stmt(
    stmt: &crate::thir::ThirStmt,
    captured: &std::collections::HashSet<&str>,
    out: &mut Option<(String, String)>,
) {
    if out.is_some() {
        return;
    }
    match &stmt.kind {
        crate::thir::ThirStmtKind::Expr(e) => find_captured_mutation_thir_expr(e, captured, out),
        crate::thir::ThirStmtKind::Let { init, .. } => {
            find_captured_mutation_thir_expr(init, captured, out)
        }
        crate::thir::ThirStmtKind::Assign { value, .. } => {
            find_captured_mutation_thir_expr(value, captured, out)
        }
        crate::thir::ThirStmtKind::Return(Some(e)) => {
            find_captured_mutation_thir_expr(e, captured, out)
        }
        crate::thir::ThirStmtKind::While { body, cond, .. } => {
            find_captured_mutation_thir_expr(cond, captured, out);
            find_captured_mutation_thir(body, captured, out);
        }
        crate::thir::ThirStmtKind::Loop { body, .. } => {
            find_captured_mutation_thir(body, captured, out)
        }
        crate::thir::ThirStmtKind::ForRange { body, .. } => {
            find_captured_mutation_thir(body, captured, out)
        }
        crate::thir::ThirStmtKind::ForEach { body, .. } => {
            find_captured_mutation_thir(body, captured, out)
        }
        crate::thir::ThirStmtKind::ParFor { body, .. } => {
            find_captured_mutation_thir(body, captured, out)
        }
        _ => {}
    }
}

pub(super) fn find_captured_mutation_thir_expr(
    expr: &crate::thir::ThirExpr,
    captured: &std::collections::HashSet<&str>,
    out: &mut Option<(String, String)>,
) {
    if out.is_some() {
        return;
    }
    match &expr.kind {
        crate::thir::ThirExprKind::Call { callee, args, .. } => {
            if MUTATING_COLLECTION_BUILTINS.contains(&callee.as_str()) {
                if let Some(first) = args.first() {
                    if let crate::thir::ThirExprKind::Value { name, .. } = &first.kind {
                        if captured.contains(name.as_str()) {
                            *out = Some((name.clone(), callee.clone()));
                            return;
                        }
                    }
                }
            }
            for a in args {
                find_captured_mutation_thir_expr(a, captured, out);
            }
        }
        crate::thir::ThirExprKind::BinOp { lhs, rhs, .. } => {
            find_captured_mutation_thir_expr(lhs, captured, out);
            find_captured_mutation_thir_expr(rhs, captured, out);
        }
        crate::thir::ThirExprKind::UnaryOp { expr, .. } => {
            find_captured_mutation_thir_expr(expr, captured, out)
        }
        crate::thir::ThirExprKind::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            find_captured_mutation_thir_expr(cond, captured, out);
            find_captured_mutation_thir(then_block, captured, out);
            if let Some(eb) = else_block {
                find_captured_mutation_thir(eb, captured, out);
            }
        }
        crate::thir::ThirExprKind::Block(b) => find_captured_mutation_thir(b, captured, out),
        _ => {}
    }
}

pub(super) fn collect_rebound_vars_in_expr(expr: &AstExpr, names: &mut Vec<String>) {
    match expr {
        AstExpr::Ident(_)
        | AstExpr::IntLit { .. }
        | AstExpr::FloatLit { .. }
        | AstExpr::BoolLit { .. }
        | AstExpr::StringLit { .. } => {}
        AstExpr::BinOp { lhs, rhs, .. } => {
            collect_rebound_vars_in_expr(lhs, names);
            collect_rebound_vars_in_expr(rhs, names);
        }
        AstExpr::Call { args, .. } | AstExpr::Tuple { elements: args, .. } => {
            for arg in args {
                collect_rebound_vars_in_expr(arg, names);
            }
        }
        AstExpr::UnaryOp { expr, .. }
        | AstExpr::Cast { expr, .. }
        | AstExpr::Await { expr, .. }
        | AstExpr::Try { expr, .. }
        | AstExpr::Lambda { body: expr, .. } => {
            collect_rebound_vars_in_expr(expr, names);
        }
        AstExpr::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            collect_rebound_vars_in_expr(cond, names);
            collect_rebound_vars_in_block(then_block, names, false);
            if let Some(else_block) = else_block {
                collect_rebound_vars_in_block(else_block, names, false);
            }
        }
        AstExpr::Block(block) => {
            // Synthetic marker (see lower_expr): the block carries no AST, all
            // the facts live in the THIR registry payload.
            if block.stmts.is_empty() && block.tail.is_none() {
                match synth_marker_at(block.span) {
                    Some(SynthMarker::Expr(e)) => {
                        collect_rebound_vars_in_thir_expr(&e, names)
                            .map(|_| ())
                            .unwrap_or(());
                        return;
                    }
                    Some(SynthMarker::Block(tb)) => {
                        let _ = collect_rebound_vars_in_thir_block(&tb, names, false);
                        return;
                    }
                    None => {}
                }
            }
            collect_rebound_vars_in_block(block, names, false);
        }
        AstExpr::Index { base, indices, .. } => {
            collect_rebound_vars_in_expr(base, names);
            for index in indices {
                collect_rebound_vars_in_expr(index, names);
            }
        }
        AstExpr::StructLit { fields, spread, .. } => {
            for (_, value) in fields {
                collect_rebound_vars_in_expr(value, names);
            }
            if let Some(s) = spread {
                collect_rebound_vars_in_expr(s, names);
            }
        }
        AstExpr::FieldAccess { base, .. } | AstExpr::TupleIndex { base, .. } => {
            collect_rebound_vars_in_expr(base, names);
        }
        AstExpr::When {
            scrutinee, arms, ..
        } => {
            collect_rebound_vars_in_expr(scrutinee, names);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    collect_rebound_vars_in_expr(guard, names);
                }
                collect_rebound_vars_in_expr(&arm.body, names);
            }
        }
        AstExpr::ArrayLit { elems, .. } => {
            for elem in elems {
                collect_rebound_vars_in_expr(elem, names);
            }
        }
        AstExpr::MethodCall { base, args, .. } => {
            collect_rebound_vars_in_expr(base, names);
            for arg in args {
                collect_rebound_vars_in_expr(arg, names);
            }
        }
        AstExpr::Mask { body, .. } => collect_rebound_vars_in_block(body, names, false),
        AstExpr::Handle { expr, .. } => collect_rebound_vars_in_expr(expr, names),
        AstExpr::NullCoal { expr, default, .. } => {
            collect_rebound_vars_in_expr(expr, names);
            collect_rebound_vars_in_expr(default, names);
        }
        AstExpr::MapLiteral { entries, .. } => {
            for (k, v) in entries {
                collect_rebound_vars_in_expr(k, names);
                collect_rebound_vars_in_expr(v, names);
            }
        }
        AstExpr::Ref { expr, .. }
        | AstExpr::RefMut { expr, .. }
        | AstExpr::Deref { expr, .. }
        | AstExpr::Move { expr, .. } => {
            collect_rebound_vars_in_expr(expr, names);
        }
        AstExpr::Unsafe { body, .. } => collect_rebound_vars_in_expr(body, names),
        AstExpr::Splat { expr, .. } => collect_rebound_vars_in_expr(expr, names),
        AstExpr::MacroCall { args, .. } => {
            for a in args {
                collect_rebound_vars_in_expr(a, names);
            }
        }
        AstExpr::TryCatch {
            body, catch_body, ..
        } => {
            collect_rebound_vars_in_expr(body, names);
            collect_rebound_vars_in_expr(catch_body, names);
        }
        AstExpr::Raise { args, .. } => {
            for a in args {
                collect_rebound_vars_in_expr(a, names);
            }
        }
    }
}
