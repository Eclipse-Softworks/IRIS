//! Source-level warnings used by the compiler and language server.

use crate::parser::ast::{AstBlock, AstExpr, AstFunction, AstModule, AstStmt, AstWhenArm};
use crate::parser::lexer::Span;

/// A compiler warning (non-fatal diagnostic).
#[derive(Debug, Clone)]
pub struct IrWarning {
    /// Name of the function containing the warning.
    pub func: String,
    /// Human-readable warning message.
    pub message: String,
    /// Optional byte span for the warning location.
    pub span: Option<Span>,
}

use crate::error::PassError;
use crate::ir::module::IrModule;
use crate::pass::Pass;

/// IR-level Lint Pass for static analysis (e.g., unused variables).
pub struct IrLintPass;

impl Pass for IrLintPass {
    fn name(&self) -> &'static str {
        "lint"
    }

    fn run(&mut self, _module: &mut IrModule) -> Result<(), PassError> {
        // IR-level linting logic can be added here.
        // Currently, AST-level linting is primarily used.
        Ok(())
    }
}

/// Preserve the compiler's established variable-lint API. More expansive
/// editor analysis lives in [`find_source_warnings`] so callers that assert an
/// empty unused-variable set do not start receiving unrelated dead-function or
/// safety diagnostics.
pub fn find_unused_vars(module: &AstModule) -> Vec<IrWarning> {
    let mut warnings = Vec::new();
    for func in &module.functions {
        collect_unused_in_function(func, &mut warnings);
        check_potential_infinite_loops(func, &mut warnings);
    }
    warnings
}

/// Analyze an AST module for editor-facing unused, unreachable, unsafe, and
/// potentially non-terminating source constructs.
pub fn find_source_warnings(module: &AstModule) -> Vec<IrWarning> {
    let mut warnings = find_unused_vars(module);
    for func in &module.functions {
        collect_unreachable_in_block(&func.body, &func.name.name, &mut warnings);
        collect_unsafe_in_block(&func.body, &func.name.name, &mut warnings);
    }

    for func in &module.functions {
        if func.is_pub
            || func.name.name == "main"
            || func.name.name.starts_with("test_")
            || func.name.name.starts_with('_')
        {
            continue;
        }
        let used_by_function = module.functions.iter().any(|caller| {
            caller.name.name != func.name.name && block_uses_name(&caller.body, &func.name.name)
        });
        let used_by_method = module
            .impls
            .iter()
            .flat_map(|imp| imp.methods.iter())
            .any(|caller| block_uses_name(&caller.body, &func.name.name));
        if !used_by_function && !used_by_method {
            warnings.push(IrWarning {
                func: func.name.name.clone(),
                message: format!("function '{}' is never used", func.name.name),
                span: Some(func.name.span),
            });
        }
    }
    warnings
}

fn collect_unused_in_function(func: &AstFunction, warnings: &mut Vec<IrWarning>) {
    let mut declared: Vec<(String, Option<Span>, &'static str)> = func
        .params
        .iter()
        .map(|param| (param.name.name.clone(), Some(param.name.span), "parameter"))
        .collect();
    collect_bindings_in_block(&func.body, &mut declared);

    for (name, span, kind) in declared {
        // Skip names starting with '_' — convention for intentionally unused.
        if name.starts_with('_') {
            continue;
        }
        // Check if this name is referenced anywhere in the block.
        if !block_uses_name(&func.body, &name) {
            warnings.push(IrWarning {
                func: func.name.name.clone(),
                message: format!("{} '{}' is never used", kind, name),
                span,
            });
        }
    }
}

fn collect_bindings_in_block(
    block: &AstBlock,
    out: &mut Vec<(String, Option<Span>, &'static str)>,
) {
    for stmt in &block.stmts {
        match stmt {
            AstStmt::Let { name, init, .. } => {
                out.push((name.name.clone(), Some(name.span), "variable"));
                collect_bindings_in_expr(init, out);
            }
            AstStmt::LetTuple { names, init, .. } => {
                out.extend(
                    names
                        .iter()
                        .map(|name| (name.name.clone(), Some(name.span), "variable")),
                );
                collect_bindings_in_expr(init, out);
            }
            AstStmt::While { cond, body, .. } => {
                collect_bindings_in_expr(cond, out);
                collect_bindings_in_block(body, out);
            }
            AstStmt::Loop { body, .. } | AstStmt::MaskStmt { body, .. } => {
                collect_bindings_in_block(body, out);
            }
            AstStmt::ForRange {
                var,
                start,
                end,
                step,
                body,
                ..
            } => {
                out.push((var.name.clone(), Some(var.span), "loop variable"));
                collect_bindings_in_expr(start, out);
                collect_bindings_in_expr(end, out);
                if let Some(step) = step {
                    collect_bindings_in_expr(step, out);
                }
                collect_bindings_in_block(body, out);
            }
            AstStmt::ForEach {
                var, iter, body, ..
            } => {
                out.push((var.name.clone(), Some(var.span), "loop variable"));
                collect_bindings_in_expr(iter, out);
                collect_bindings_in_block(body, out);
            }
            AstStmt::ParFor {
                var,
                start,
                end,
                body,
                ..
            } => {
                out.push((var.name.clone(), Some(var.span), "loop variable"));
                collect_bindings_in_expr(start, out);
                collect_bindings_in_expr(end, out);
                collect_bindings_in_block(body, out);
            }
            AstStmt::Assign { target, value, .. } => {
                collect_bindings_in_expr(target, out);
                collect_bindings_in_expr(value, out);
            }
            AstStmt::Expr(expr) | AstStmt::Defer { expr, .. } | AstStmt::Yield { expr, .. } => {
                collect_bindings_in_expr(expr, out)
            }
            AstStmt::Return { value, .. } => {
                if let Some(value) = value {
                    collect_bindings_in_expr(value, out);
                }
            }
            AstStmt::Spawn { body, .. } => {
                for stmt in body {
                    collect_bindings_in_stmt(stmt, out);
                }
            }
            AstStmt::HandleStmt { expr, arms, .. } => {
                collect_bindings_in_expr(expr, out);
                for arm in arms {
                    out.extend(
                        arm.params
                            .iter()
                            .map(|p| (p.name.clone(), Some(p.span), "handler parameter")),
                    );
                    if let Some(p) = &arm.resume_param {
                        out.push((p.name.clone(), Some(p.span), "resume parameter"));
                    }
                    collect_bindings_in_expr(&arm.body, out);
                }
            }
            AstStmt::Select { arms, default, .. } => {
                for arm in arms {
                    collect_bindings_in_expr(&arm.channel, out);
                    collect_bindings_in_block(&arm.body, out);
                }
                if let Some(default) = default {
                    collect_bindings_in_block(default, out);
                }
            }
            AstStmt::Break { .. } | AstStmt::Continue { .. } => {}
        }
    }
    if let Some(tail) = &block.tail {
        collect_bindings_in_expr(tail, out);
    }
}

fn collect_bindings_in_stmt(stmt: &AstStmt, out: &mut Vec<(String, Option<Span>, &'static str)>) {
    let block = AstBlock {
        stmts: vec![stmt.clone()],
        tail: None,
        span: stmt_span(stmt),
    };
    collect_bindings_in_block(&block, out);
}

fn collect_bindings_in_expr(expr: &AstExpr, out: &mut Vec<(String, Option<Span>, &'static str)>) {
    match expr {
        AstExpr::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            collect_bindings_in_expr(cond, out);
            collect_bindings_in_block(then_block, out);
            if let Some(block) = else_block {
                collect_bindings_in_block(block, out);
            }
        }
        AstExpr::Block(block) | AstExpr::Mask { body: block, .. } => {
            collect_bindings_in_block(block, out)
        }
        AstExpr::When {
            scrutinee, arms, ..
        } => {
            collect_bindings_in_expr(scrutinee, out);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    collect_bindings_in_expr(guard, out);
                }
                collect_bindings_in_expr(&arm.body, out);
            }
        }
        AstExpr::Lambda { params, body, .. } => {
            out.extend(
                params
                    .iter()
                    .map(|p| (p.name.name.clone(), Some(p.name.span), "closure parameter")),
            );
            collect_bindings_in_expr(body, out);
        }
        AstExpr::BinOp { lhs, rhs, .. } => {
            collect_bindings_in_expr(lhs, out);
            collect_bindings_in_expr(rhs, out);
        }
        AstExpr::Call {
            args, named_args, ..
        } => {
            for arg in args {
                collect_bindings_in_expr(arg, out);
            }
            for (_, arg) in named_args {
                collect_bindings_in_expr(arg, out);
            }
        }
        AstExpr::UnaryOp { expr, .. }
        | AstExpr::Cast { expr, .. }
        | AstExpr::Await { expr, .. }
        | AstExpr::Try { expr, .. }
        | AstExpr::Ref { expr, .. }
        | AstExpr::RefMut { expr, .. }
        | AstExpr::Deref { expr, .. }
        | AstExpr::Move { expr, .. }
        | AstExpr::Unsafe { body: expr, .. }
        | AstExpr::Splat { expr, .. }
        | AstExpr::FieldAccess { base: expr, .. }
        | AstExpr::TupleIndex { base: expr, .. } => collect_bindings_in_expr(expr, out),
        AstExpr::Index { base, indices, .. } => {
            collect_bindings_in_expr(base, out);
            for index in indices {
                collect_bindings_in_expr(index, out);
            }
        }
        AstExpr::StructLit { fields, spread, .. } => {
            for (_, value) in fields {
                collect_bindings_in_expr(value, out);
            }
            if let Some(spread) = spread {
                collect_bindings_in_expr(spread, out);
            }
        }
        AstExpr::Tuple { elements, .. }
        | AstExpr::ArrayLit {
            elems: elements, ..
        } => {
            for element in elements {
                collect_bindings_in_expr(element, out);
            }
        }
        AstExpr::NullCoal { expr, default, .. } => {
            collect_bindings_in_expr(expr, out);
            collect_bindings_in_expr(default, out);
        }
        AstExpr::MethodCall { base, args, .. } => {
            collect_bindings_in_expr(base, out);
            for arg in args {
                collect_bindings_in_expr(arg, out);
            }
        }
        AstExpr::Handle { expr, arms, .. } => {
            collect_bindings_in_expr(expr, out);
            for arm in arms {
                collect_bindings_in_expr(&arm.body, out);
            }
        }
        AstExpr::MapLiteral { entries, .. } => {
            for (key, value) in entries {
                collect_bindings_in_expr(key, out);
                collect_bindings_in_expr(value, out);
            }
        }
        AstExpr::TryCatch {
            body, catch_body, ..
        } => {
            collect_bindings_in_expr(body, out);
            collect_bindings_in_expr(catch_body, out);
        }
        AstExpr::Raise { args, .. } | AstExpr::MacroCall { args, .. } => {
            for arg in args {
                collect_bindings_in_expr(arg, out);
            }
        }
        AstExpr::Ident(_)
        | AstExpr::IntLit { .. }
        | AstExpr::FloatLit { .. }
        | AstExpr::BoolLit { .. }
        | AstExpr::StringLit { .. } => {}
    }
}

/// Returns true if `name` appears as an `AstExpr::Ident` anywhere in the block.
fn block_uses_name(block: &AstBlock, name: &str) -> bool {
    for stmt in &block.stmts {
        if stmt_uses_name(stmt, name) {
            return true;
        }
    }
    if let Some(tail) = &block.tail {
        if expr_uses_name(tail, name) {
            return true;
        }
    }
    false
}

fn stmt_uses_name(stmt: &AstStmt, name: &str) -> bool {
    match stmt {
        AstStmt::Let { init, .. } => expr_uses_name(init, name),
        AstStmt::Expr(e) => expr_uses_name(e, name),
        AstStmt::While { cond, body, .. } => {
            expr_uses_name(cond, name) || block_uses_name(body, name)
        }
        AstStmt::Loop { body, .. } => block_uses_name(body, name),
        AstStmt::ForRange {
            start, end, body, ..
        } => {
            expr_uses_name(start, name) || expr_uses_name(end, name) || block_uses_name(body, name)
        }
        AstStmt::ForEach { iter, body, .. } => {
            expr_uses_name(iter, name) || block_uses_name(body, name)
        }
        AstStmt::Assign { target, value, .. } => {
            expr_uses_name(target, name) || expr_uses_name(value, name)
        }
        AstStmt::LetTuple { init, .. } => expr_uses_name(init, name),
        AstStmt::Return { value, .. } => value.as_ref().is_some_and(|e| expr_uses_name(e, name)),
        AstStmt::Spawn { body, .. } => body.iter().any(|s| stmt_uses_name(s, name)),
        AstStmt::ParFor {
            start, end, body, ..
        } => {
            expr_uses_name(start, name) || expr_uses_name(end, name) || block_uses_name(body, name)
        }
        AstStmt::Break { .. } | AstStmt::Continue { .. } => false,
        AstStmt::MaskStmt { body, .. } => block_uses_name(body, name),
        AstStmt::HandleStmt { expr, .. } => expr_uses_name(expr, name),
        AstStmt::Defer { expr, .. } => expr_uses_name(expr, name),
        AstStmt::Yield { expr, .. } => expr_uses_name(expr, name),
        AstStmt::Select { arms, default, .. } => {
            arms.iter()
                .any(|arm| expr_uses_name(&arm.channel, name) || block_uses_name(&arm.body, name))
                || default.as_ref().is_some_and(|d| block_uses_name(d, name))
        }
    }
}

pub(crate) fn expr_uses_name(expr: &AstExpr, name: &str) -> bool {
    match expr {
        AstExpr::Ident(ident) => ident.name == name,
        AstExpr::IntLit { .. }
        | AstExpr::FloatLit { .. }
        | AstExpr::BoolLit { .. }
        | AstExpr::StringLit { .. } => false,
        AstExpr::BinOp { lhs, rhs, .. } => expr_uses_name(lhs, name) || expr_uses_name(rhs, name),
        AstExpr::UnaryOp { expr, .. }
        | AstExpr::Cast { expr, .. }
        | AstExpr::Await { expr, .. }
        | AstExpr::Try { expr, .. } => expr_uses_name(expr, name),
        AstExpr::Call { callee, args, .. } => {
            callee.name == name || args.iter().any(|a| expr_uses_name(a, name))
        }
        AstExpr::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            expr_uses_name(cond, name)
                || block_uses_name(then_block, name)
                || else_block
                    .as_ref()
                    .is_some_and(|b| block_uses_name(b, name))
        }
        AstExpr::Block(b) => block_uses_name(b, name),
        AstExpr::When {
            scrutinee, arms, ..
        } => expr_uses_name(scrutinee, name) || arms.iter().any(|a| arm_uses_name(a, name)),
        AstExpr::FieldAccess { base, .. }
        | AstExpr::TupleIndex { base, .. }
        | AstExpr::Index { base, .. } => expr_uses_name(base, name),
        AstExpr::ArrayLit { elems, .. } => elems.iter().any(|e| expr_uses_name(e, name)),
        AstExpr::Tuple { elements, .. } => elements.iter().any(|e| expr_uses_name(e, name)),
        AstExpr::Lambda { body, .. } => expr_uses_name(body, name),
        AstExpr::StructLit { fields, spread, .. } => {
            fields.iter().any(|(_, v)| expr_uses_name(v, name))
                || spread.as_ref().is_some_and(|s| expr_uses_name(s, name))
        }
        AstExpr::MethodCall { base, args, .. } => {
            expr_uses_name(base, name) || args.iter().any(|a| expr_uses_name(a, name))
        }
        AstExpr::Mask { body, .. } => block_uses_name(body, name),
        AstExpr::Handle { expr, .. } => expr_uses_name(expr, name),
        AstExpr::NullCoal { expr, default, .. } => {
            expr_uses_name(expr, name) || expr_uses_name(default, name)
        }
        AstExpr::MapLiteral { entries, .. } => entries
            .iter()
            .any(|(k, v)| expr_uses_name(k, name) || expr_uses_name(v, name)),
        AstExpr::Ref { expr, .. }
        | AstExpr::RefMut { expr, .. }
        | AstExpr::Deref { expr, .. }
        | AstExpr::Move { expr, .. } => expr_uses_name(expr, name),
        AstExpr::Unsafe { body, .. } => expr_uses_name(body, name),
        AstExpr::Splat { expr, .. } => expr_uses_name(expr, name),
        AstExpr::TryCatch {
            body, catch_body, ..
        } => expr_uses_name(body, name) || expr_uses_name(catch_body, name),
        AstExpr::Raise { args, .. } => args.iter().any(|a| expr_uses_name(a, name)),
        AstExpr::MacroCall { args, .. } => args.iter().any(|a| expr_uses_name(a, name)),
    }
}

fn arm_uses_name(arm: &AstWhenArm, name: &str) -> bool {
    arm.guard.as_ref().is_some_and(|g| expr_uses_name(g, name)) || expr_uses_name(&arm.body, name)
}

fn stmt_span(stmt: &AstStmt) -> Span {
    match stmt {
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
        AstStmt::Expr(expr) => expr.span(),
    }
}

fn collect_unreachable_in_block(block: &AstBlock, func_name: &str, warnings: &mut Vec<IrWarning>) {
    collect_unreachable_in_stmts(&block.stmts, func_name, warnings);
    if let Some(tail) = &block.tail {
        collect_unreachable_in_expr(tail, func_name, warnings);
    }
}

fn collect_unreachable_in_stmts(stmts: &[AstStmt], func_name: &str, warnings: &mut Vec<IrWarning>) {
    let mut terminated = false;
    for stmt in stmts {
        if terminated {
            warnings.push(IrWarning {
                func: func_name.to_owned(),
                message: "unreachable code after control flow exits this block".to_owned(),
                span: Some(stmt_span(stmt)),
            });
            continue;
        }
        collect_unreachable_in_stmt(stmt, func_name, warnings);
        terminated = matches!(
            stmt,
            AstStmt::Return { .. } | AstStmt::Break { .. } | AstStmt::Continue { .. }
        );
    }
}

fn collect_unreachable_in_stmt(stmt: &AstStmt, func_name: &str, warnings: &mut Vec<IrWarning>) {
    match stmt {
        AstStmt::While { cond, body, .. } => {
            collect_unreachable_in_expr(cond, func_name, warnings);
            collect_unreachable_in_block(body, func_name, warnings);
        }
        AstStmt::Loop { body, .. } | AstStmt::MaskStmt { body, .. } => {
            collect_unreachable_in_block(body, func_name, warnings)
        }
        AstStmt::ForRange {
            start,
            end,
            step,
            body,
            ..
        } => {
            collect_unreachable_in_expr(start, func_name, warnings);
            collect_unreachable_in_expr(end, func_name, warnings);
            if let Some(step) = step {
                collect_unreachable_in_expr(step, func_name, warnings);
            }
            collect_unreachable_in_block(body, func_name, warnings);
        }
        AstStmt::ForEach { iter, body, .. } => {
            collect_unreachable_in_expr(iter, func_name, warnings);
            collect_unreachable_in_block(body, func_name, warnings);
        }
        AstStmt::ParFor {
            start, end, body, ..
        } => {
            collect_unreachable_in_expr(start, func_name, warnings);
            collect_unreachable_in_expr(end, func_name, warnings);
            collect_unreachable_in_block(body, func_name, warnings);
        }
        AstStmt::Assign { target, value, .. } => {
            collect_unreachable_in_expr(target, func_name, warnings);
            collect_unreachable_in_expr(value, func_name, warnings);
        }
        AstStmt::Let { init, .. } | AstStmt::LetTuple { init, .. } => {
            collect_unreachable_in_expr(init, func_name, warnings)
        }
        AstStmt::Expr(expr) | AstStmt::Defer { expr, .. } | AstStmt::Yield { expr, .. } => {
            collect_unreachable_in_expr(expr, func_name, warnings)
        }
        AstStmt::Return { value, .. } => {
            if let Some(value) = value {
                collect_unreachable_in_expr(value, func_name, warnings);
            }
        }
        AstStmt::Spawn { body, .. } => collect_unreachable_in_stmts(body, func_name, warnings),
        AstStmt::HandleStmt { expr, arms, .. } => {
            collect_unreachable_in_expr(expr, func_name, warnings);
            for arm in arms {
                collect_unreachable_in_expr(&arm.body, func_name, warnings);
            }
        }
        AstStmt::Select { arms, default, .. } => {
            for arm in arms {
                collect_unreachable_in_expr(&arm.channel, func_name, warnings);
                collect_unreachable_in_block(&arm.body, func_name, warnings);
            }
            if let Some(default) = default {
                collect_unreachable_in_block(default, func_name, warnings);
            }
        }
        AstStmt::Break { .. } | AstStmt::Continue { .. } => {}
    }
}

fn collect_unreachable_in_expr(expr: &AstExpr, func_name: &str, warnings: &mut Vec<IrWarning>) {
    match expr {
        AstExpr::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            collect_unreachable_in_expr(cond, func_name, warnings);
            collect_unreachable_in_block(then_block, func_name, warnings);
            if let Some(block) = else_block {
                collect_unreachable_in_block(block, func_name, warnings);
            }
        }
        AstExpr::Block(block) | AstExpr::Mask { body: block, .. } => {
            collect_unreachable_in_block(block, func_name, warnings)
        }
        AstExpr::BinOp { lhs, rhs, .. } => {
            collect_unreachable_in_expr(lhs, func_name, warnings);
            collect_unreachable_in_expr(rhs, func_name, warnings);
        }
        AstExpr::Call {
            args, named_args, ..
        } => {
            for arg in args {
                collect_unreachable_in_expr(arg, func_name, warnings);
            }
            for (_, arg) in named_args {
                collect_unreachable_in_expr(arg, func_name, warnings);
            }
        }
        AstExpr::UnaryOp { expr, .. }
        | AstExpr::Cast { expr, .. }
        | AstExpr::Await { expr, .. }
        | AstExpr::Try { expr, .. }
        | AstExpr::Ref { expr, .. }
        | AstExpr::RefMut { expr, .. }
        | AstExpr::Deref { expr, .. }
        | AstExpr::Move { expr, .. }
        | AstExpr::Unsafe { body: expr, .. }
        | AstExpr::Splat { expr, .. }
        | AstExpr::FieldAccess { base: expr, .. }
        | AstExpr::TupleIndex { base: expr, .. } => {
            collect_unreachable_in_expr(expr, func_name, warnings)
        }
        AstExpr::Index { base, indices, .. } => {
            collect_unreachable_in_expr(base, func_name, warnings);
            for index in indices {
                collect_unreachable_in_expr(index, func_name, warnings);
            }
        }
        AstExpr::StructLit { fields, spread, .. } => {
            for (_, value) in fields {
                collect_unreachable_in_expr(value, func_name, warnings);
            }
            if let Some(spread) = spread {
                collect_unreachable_in_expr(spread, func_name, warnings);
            }
        }
        AstExpr::When {
            scrutinee, arms, ..
        } => {
            collect_unreachable_in_expr(scrutinee, func_name, warnings);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    collect_unreachable_in_expr(guard, func_name, warnings);
                }
                collect_unreachable_in_expr(&arm.body, func_name, warnings);
            }
        }
        AstExpr::Tuple { elements, .. }
        | AstExpr::ArrayLit {
            elems: elements, ..
        } => {
            for element in elements {
                collect_unreachable_in_expr(element, func_name, warnings);
            }
        }
        AstExpr::Lambda { body, .. } => collect_unreachable_in_expr(body, func_name, warnings),
        AstExpr::NullCoal { expr, default, .. } => {
            collect_unreachable_in_expr(expr, func_name, warnings);
            collect_unreachable_in_expr(default, func_name, warnings);
        }
        AstExpr::MethodCall { base, args, .. } => {
            collect_unreachable_in_expr(base, func_name, warnings);
            for arg in args {
                collect_unreachable_in_expr(arg, func_name, warnings);
            }
        }
        AstExpr::Handle { expr, arms, .. } => {
            collect_unreachable_in_expr(expr, func_name, warnings);
            for arm in arms {
                collect_unreachable_in_expr(&arm.body, func_name, warnings);
            }
        }
        AstExpr::MapLiteral { entries, .. } => {
            for (key, value) in entries {
                collect_unreachable_in_expr(key, func_name, warnings);
                collect_unreachable_in_expr(value, func_name, warnings);
            }
        }
        AstExpr::TryCatch {
            body, catch_body, ..
        } => {
            collect_unreachable_in_expr(body, func_name, warnings);
            collect_unreachable_in_expr(catch_body, func_name, warnings);
        }
        AstExpr::Raise { args, .. } | AstExpr::MacroCall { args, .. } => {
            for arg in args {
                collect_unreachable_in_expr(arg, func_name, warnings);
            }
        }
        AstExpr::Ident(_)
        | AstExpr::IntLit { .. }
        | AstExpr::FloatLit { .. }
        | AstExpr::BoolLit { .. }
        | AstExpr::StringLit { .. } => {}
    }
}

fn collect_unsafe_in_block(block: &AstBlock, func_name: &str, warnings: &mut Vec<IrWarning>) {
    walk_block_exprs(block, &mut |expr| {
        if let AstExpr::Unsafe { span, .. } = expr {
            warnings.push(IrWarning {
                func: func_name.to_owned(),
                message: "unsafe block marks an explicitly unsafe region; review its operations and invariants"
                    .to_owned(),
                span: Some(*span),
            });
        }
    });
}

fn walk_block_exprs(block: &AstBlock, visit: &mut impl FnMut(&AstExpr)) {
    for stmt in &block.stmts {
        walk_stmt_exprs(stmt, visit);
    }
    if let Some(tail) = &block.tail {
        walk_expr_tree(tail, visit);
    }
}

fn walk_stmt_exprs(stmt: &AstStmt, visit: &mut impl FnMut(&AstExpr)) {
    match stmt {
        AstStmt::Let { init, .. } | AstStmt::LetTuple { init, .. } => walk_expr_tree(init, visit),
        AstStmt::Expr(expr) | AstStmt::Defer { expr, .. } | AstStmt::Yield { expr, .. } => {
            walk_expr_tree(expr, visit)
        }
        AstStmt::While { cond, body, .. } => {
            walk_expr_tree(cond, visit);
            walk_block_exprs(body, visit);
        }
        AstStmt::Loop { body, .. } | AstStmt::MaskStmt { body, .. } => {
            walk_block_exprs(body, visit)
        }
        AstStmt::ForRange {
            start,
            end,
            step,
            body,
            ..
        } => {
            walk_expr_tree(start, visit);
            walk_expr_tree(end, visit);
            if let Some(step) = step {
                walk_expr_tree(step, visit);
            }
            walk_block_exprs(body, visit);
        }
        AstStmt::ForEach { iter, body, .. } => {
            walk_expr_tree(iter, visit);
            walk_block_exprs(body, visit);
        }
        AstStmt::ParFor {
            start, end, body, ..
        } => {
            walk_expr_tree(start, visit);
            walk_expr_tree(end, visit);
            walk_block_exprs(body, visit);
        }
        AstStmt::Assign { target, value, .. } => {
            walk_expr_tree(target, visit);
            walk_expr_tree(value, visit);
        }
        AstStmt::Return { value, .. } => {
            if let Some(value) = value {
                walk_expr_tree(value, visit);
            }
        }
        AstStmt::Spawn { body, .. } => {
            for stmt in body {
                walk_stmt_exprs(stmt, visit);
            }
        }
        AstStmt::HandleStmt { expr, arms, .. } => {
            walk_expr_tree(expr, visit);
            for arm in arms {
                walk_expr_tree(&arm.body, visit);
            }
        }
        AstStmt::Select { arms, default, .. } => {
            for arm in arms {
                walk_expr_tree(&arm.channel, visit);
                walk_block_exprs(&arm.body, visit);
            }
            if let Some(default) = default {
                walk_block_exprs(default, visit);
            }
        }
        AstStmt::Break { .. } | AstStmt::Continue { .. } => {}
    }
}

fn walk_expr_tree(expr: &AstExpr, visit: &mut impl FnMut(&AstExpr)) {
    visit(expr);
    match expr {
        AstExpr::BinOp { lhs, rhs, .. } => {
            walk_expr_tree(lhs, visit);
            walk_expr_tree(rhs, visit);
        }
        AstExpr::Call {
            args, named_args, ..
        } => {
            for arg in args {
                walk_expr_tree(arg, visit);
            }
            for (_, arg) in named_args {
                walk_expr_tree(arg, visit);
            }
        }
        AstExpr::UnaryOp { expr, .. }
        | AstExpr::Cast { expr, .. }
        | AstExpr::Await { expr, .. }
        | AstExpr::Try { expr, .. }
        | AstExpr::Ref { expr, .. }
        | AstExpr::RefMut { expr, .. }
        | AstExpr::Deref { expr, .. }
        | AstExpr::Move { expr, .. }
        | AstExpr::Unsafe { body: expr, .. }
        | AstExpr::Splat { expr, .. }
        | AstExpr::FieldAccess { base: expr, .. }
        | AstExpr::TupleIndex { base: expr, .. } => walk_expr_tree(expr, visit),
        AstExpr::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            walk_expr_tree(cond, visit);
            walk_block_exprs(then_block, visit);
            if let Some(block) = else_block {
                walk_block_exprs(block, visit);
            }
        }
        AstExpr::Block(block) | AstExpr::Mask { body: block, .. } => walk_block_exprs(block, visit),
        AstExpr::Index { base, indices, .. } => {
            walk_expr_tree(base, visit);
            for index in indices {
                walk_expr_tree(index, visit);
            }
        }
        AstExpr::StructLit { fields, spread, .. } => {
            for (_, value) in fields {
                walk_expr_tree(value, visit);
            }
            if let Some(spread) = spread {
                walk_expr_tree(spread, visit);
            }
        }
        AstExpr::When {
            scrutinee, arms, ..
        } => {
            walk_expr_tree(scrutinee, visit);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    walk_expr_tree(guard, visit);
                }
                walk_expr_tree(&arm.body, visit);
            }
        }
        AstExpr::Tuple { elements, .. }
        | AstExpr::ArrayLit {
            elems: elements, ..
        } => {
            for element in elements {
                walk_expr_tree(element, visit);
            }
        }
        AstExpr::Lambda { body, .. } => walk_expr_tree(body, visit),
        AstExpr::NullCoal { expr, default, .. } => {
            walk_expr_tree(expr, visit);
            walk_expr_tree(default, visit);
        }
        AstExpr::MethodCall { base, args, .. } => {
            walk_expr_tree(base, visit);
            for arg in args {
                walk_expr_tree(arg, visit);
            }
        }
        AstExpr::Handle { expr, arms, .. } => {
            walk_expr_tree(expr, visit);
            for arm in arms {
                walk_expr_tree(&arm.body, visit);
            }
        }
        AstExpr::MapLiteral { entries, .. } => {
            for (key, value) in entries {
                walk_expr_tree(key, visit);
                walk_expr_tree(value, visit);
            }
        }
        AstExpr::TryCatch {
            body, catch_body, ..
        } => {
            walk_expr_tree(body, visit);
            walk_expr_tree(catch_body, visit);
        }
        AstExpr::Raise { args, .. } | AstExpr::MacroCall { args, .. } => {
            for arg in args {
                walk_expr_tree(arg, visit);
            }
        }
        AstExpr::Ident(_)
        | AstExpr::IntLit { .. }
        | AstExpr::FloatLit { .. }
        | AstExpr::BoolLit { .. }
        | AstExpr::StringLit { .. } => {}
    }
}

// ---------------------------------------------------------------------------
// Infinite loop detection
// ---------------------------------------------------------------------------

fn check_potential_infinite_loops(func: &AstFunction, warnings: &mut Vec<IrWarning>) {
    check_infinite_loops_in_block(&func.body, &func.name.name, warnings);
}

fn check_infinite_loops_in_block(block: &AstBlock, func_name: &str, warnings: &mut Vec<IrWarning>) {
    for stmt in &block.stmts {
        match stmt {
            AstStmt::While {
                cond, body, span, ..
            } => {
                let cond_vars = collect_idents_in_expr(cond);
                if !cond_vars.is_empty() {
                    let mutated = cond_vars.iter().any(|v| body_assigns_var(body, v));
                    let exits = body_has_exit(body);
                    if !mutated && !exits {
                        warnings.push(IrWarning {
                            func: func_name.to_string(),
                            message: format!(
                                "possible infinite loop: '{}' is never modified in the loop body",
                                cond_vars.join("', '")
                            ),
                            span: Some(*span),
                        });
                    }
                }
                check_infinite_loops_in_block(body, func_name, warnings);
            }
            AstStmt::ForRange { body, .. }
            | AstStmt::ForEach { body, .. }
            | AstStmt::ParFor { body, .. }
            | AstStmt::Loop { body, .. } => {
                check_infinite_loops_in_block(body, func_name, warnings);
            }
            _ => {}
        }
    }
}

/// Collect all simple identifier names referenced in an expression.
fn collect_idents_in_expr(expr: &AstExpr) -> Vec<String> {
    let mut out = Vec::new();
    collect_idents_recursive(expr, &mut out);
    out
}

fn collect_idents_recursive(expr: &AstExpr, out: &mut Vec<String>) {
    match expr {
        AstExpr::Ident(id) => out.push(id.name.clone()),
        AstExpr::BinOp { lhs, rhs, .. } => {
            collect_idents_recursive(lhs, out);
            collect_idents_recursive(rhs, out);
        }
        AstExpr::UnaryOp { expr, .. }
        | AstExpr::Cast { expr, .. }
        | AstExpr::Await { expr, .. }
        | AstExpr::Try { expr, .. } => collect_idents_recursive(expr, out),
        AstExpr::Call { args, .. } => args.iter().for_each(|a| collect_idents_recursive(a, out)),
        AstExpr::MethodCall { base, args, .. } => {
            collect_idents_recursive(base, out);
            args.iter().for_each(|a| collect_idents_recursive(a, out));
        }
        AstExpr::FieldAccess { base, .. }
        | AstExpr::TupleIndex { base, .. }
        | AstExpr::Index { base, .. } => collect_idents_recursive(base, out),
        AstExpr::TryCatch {
            body, catch_body, ..
        } => {
            collect_idents_recursive(body, out);
            collect_idents_recursive(catch_body, out);
        }
        AstExpr::Raise { args, .. } => args.iter().for_each(|a| collect_idents_recursive(a, out)),
        _ => {}
    }
}

/// Returns true if `name` is assigned (mutated) anywhere in `block`.
fn body_assigns_var(block: &AstBlock, name: &str) -> bool {
    block.stmts.iter().any(|s| stmt_assigns_var(s, name))
}

fn stmt_assigns_var(stmt: &AstStmt, name: &str) -> bool {
    match stmt {
        AstStmt::Assign { target, .. } => expr_uses_name(target, name),
        AstStmt::Let { name: n, .. } => n.name == name,
        AstStmt::While { body, .. }
        | AstStmt::Loop { body, .. }
        | AstStmt::ForRange { body, .. }
        | AstStmt::ForEach { body, .. }
        | AstStmt::ParFor { body, .. } => body_assigns_var(body, name),
        AstStmt::Spawn { body, .. } => body.iter().any(|s| stmt_assigns_var(s, name)),
        // Assignments can live inside if/else or block expressions.
        AstStmt::Expr(e) => expr_assigns_var(e, name),
        _ => false,
    }
}

/// Returns true if `expr` contains an assignment to `name` (recursing into
/// if/else branches and blocks, which is where conditional mutations live).
fn expr_assigns_var(expr: &AstExpr, name: &str) -> bool {
    match expr {
        AstExpr::If {
            then_block,
            else_block,
            ..
        } => {
            body_assigns_var(then_block, name)
                || else_block
                    .as_ref()
                    .is_some_and(|b| body_assigns_var(b, name))
        }
        AstExpr::Block(b) => body_assigns_var(b, name),
        AstExpr::When { arms, .. } => arms.iter().any(|a| expr_assigns_var(&a.body, name)),
        _ => false,
    }
}

/// Returns true if `block` contains a `break` or `return` at the top level
/// (not inside a nested loop, which would break the inner loop instead).
fn body_has_exit(block: &AstBlock) -> bool {
    block.stmts.iter().any(stmt_has_exit)
}

fn stmt_has_exit(stmt: &AstStmt) -> bool {
    match stmt {
        AstStmt::Break { .. } | AstStmt::Return { .. } => true,
        // An if-expression may contain a break — check both branches.
        AstStmt::Expr(e) => expr_has_exit(e),
        // Nested loops own their own break — don't propagate.
        AstStmt::While { .. }
        | AstStmt::Loop { .. }
        | AstStmt::ForRange { .. }
        | AstStmt::ForEach { .. }
        | AstStmt::ParFor { .. } => false,
        _ => false,
    }
}

fn expr_has_exit(expr: &AstExpr) -> bool {
    match expr {
        AstExpr::If {
            then_block,
            else_block,
            ..
        } => body_has_exit(then_block) || else_block.as_ref().is_some_and(body_has_exit),
        AstExpr::Block(b) => body_has_exit(b),
        _ => false,
    }
}
