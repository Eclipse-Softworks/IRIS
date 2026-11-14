use crate::error::describe_location;
use crate::parser::ast::*;
use crate::parser::lexer::Span;

#[derive(Debug, Clone)]
struct Borrow {
    #[allow(dead_code)]
    var_name: String,
    is_mut: bool,
    scope_depth: usize,
    span: Span,
}

/// A borrow that escapes as the value of an expression (the tail of a
/// block, an arm of an `if`/`when`). The borrow tracker writes the loans
/// directly into `self.borrows` as it goes; this is the subset that also
/// flows outward as a value, so a following `Let` can register it against
/// the place the binding keeps borrowed.
#[derive(Debug, Clone)]
struct Loan {
    place: String,
    is_mut: bool,
    span: Span,
}

#[derive(Debug, Clone)]
pub enum BorrowError {
    MutBorrowWhileBorrowed {
        var: String,
        borrow_span: Span,
        mut_span: Span,
    },
    BorrowWhileMutBorrowed {
        var: String,
        borrow_span: Span,
        mut_span: Span,
    },
    MutateWhileBorrowed {
        var: String,
        borrow_span: Span,
        mutate_span: Span,
    },
    MoveWhileBorrowed {
        var: String,
        borrow_span: Span,
        move_span: Span,
    },
    UseAfterMove {
        var: String,
        move_span: Span,
        use_span: Span,
    },
    BorrowAfterMove {
        var: String,
        move_span: Span,
        borrow_span: Span,
    },
    MoveAfterMove {
        var: String,
        first_move: Span,
        second_move: Span,
    },
}

impl BorrowError {
    fn var_name(&self) -> &str {
        match self {
            BorrowError::MutBorrowWhileBorrowed { var, .. }
            | BorrowError::BorrowWhileMutBorrowed { var, .. }
            | BorrowError::MutateWhileBorrowed { var, .. }
            | BorrowError::MoveWhileBorrowed { var, .. }
            | BorrowError::UseAfterMove { var, .. }
            | BorrowError::BorrowAfterMove { var, .. }
            | BorrowError::MoveAfterMove { var, .. } => var,
        }
    }

    fn conflicting_span(&self) -> Span {
        match self {
            BorrowError::MutBorrowWhileBorrowed { borrow_span, .. }
            | BorrowError::BorrowWhileMutBorrowed { borrow_span, .. }
            | BorrowError::MutateWhileBorrowed { borrow_span, .. }
            | BorrowError::MoveWhileBorrowed { borrow_span, .. } => *borrow_span,
            BorrowError::UseAfterMove { move_span, .. }
            | BorrowError::BorrowAfterMove { move_span, .. } => *move_span,
            BorrowError::MoveAfterMove { first_move, .. } => *first_move,
        }
    }

    #[allow(dead_code)]
    fn offending_span(&self) -> Span {
        match self {
            BorrowError::MutBorrowWhileBorrowed { mut_span, .. } => *mut_span,
            BorrowError::BorrowWhileMutBorrowed { mut_span, .. } => *mut_span,
            BorrowError::MutateWhileBorrowed { mutate_span, .. } => *mutate_span,
            BorrowError::MoveWhileBorrowed { move_span, .. } => *move_span,
            BorrowError::UseAfterMove { use_span, .. } => *use_span,
            BorrowError::BorrowAfterMove { borrow_span, .. } => *borrow_span,
            BorrowError::MoveAfterMove { second_move, .. } => *second_move,
        }
    }

    /// Converts this borrow error into a rich, rustc-style multi-span Diagnostic.
    pub fn to_diagnostic(&self, offending: Span) -> crate::diagnostics::Diagnostic {
        let var = self.var_name();
        let conflicting = self.conflicting_span();
        let primary_label = match self {
            BorrowError::MutBorrowWhileBorrowed { .. } => "mutable borrow occurs here",
            BorrowError::BorrowWhileMutBorrowed { .. } => "immutable borrow occurs here",
            BorrowError::MutateWhileBorrowed { .. } => "conflicting mutation occurs here",
            BorrowError::MoveWhileBorrowed { .. } => "move occurs here",
            BorrowError::UseAfterMove { .. } => "use of moved value here",
            BorrowError::BorrowAfterMove { .. } => "borrow occurs here",
            BorrowError::MoveAfterMove { .. } => "second move occurs here",
        };
        let secondary_label = match self {
            BorrowError::MutBorrowWhileBorrowed { .. } => "immutable borrow occurs here",
            BorrowError::BorrowWhileMutBorrowed { .. } => "mutable borrow occurs here",
            BorrowError::MutateWhileBorrowed { .. } => "first borrow occurs here",
            BorrowError::MoveWhileBorrowed { .. } => "first borrow occurs here",
            BorrowError::UseAfterMove { .. } => "value moved here",
            BorrowError::BorrowAfterMove { .. } => "value moved here",
            BorrowError::MoveAfterMove { .. } => "first move occurs here",
        };

        let mut spans = Vec::new();
        if conflicting.start.0 != offending.start.0 {
            spans.push(crate::diagnostics::DiagnosticSpan {
                start: conflicting.start.0,
                end: conflicting.end.0,
                label: Some(secondary_label.to_string()),
                is_primary: false,
            });
        }
        spans.push(crate::diagnostics::DiagnosticSpan {
            start: offending.start.0,
            end: offending.end.0,
            label: Some(primary_label.to_string()),
            is_primary: true,
        });

        crate::diagnostics::Diagnostic {
            code: Some("E0382".into()),
            level: crate::diagnostics::DiagnosticLevel::Error,
            message: format!("{}", self),
            spans,
            notes: vec![format!(
                "value `{}` cannot be used while borrowed or moved",
                var
            )],
            helps: vec![],
            suggestions: vec![],
            fixes: vec![],
        }
    }
}

impl std::fmt::Display for BorrowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BorrowError::MutBorrowWhileBorrowed { var, .. } => {
                write!(
                    f,
                    "cannot borrow `{}` as mutable because it is already borrowed as immutable",
                    var
                )
            }
            BorrowError::BorrowWhileMutBorrowed { var, .. } => {
                write!(
                    f,
                    "cannot borrow `{}` as immutable because it is already borrowed as mutable",
                    var
                )
            }
            BorrowError::MutateWhileBorrowed { var, .. } => {
                write!(
                    f,
                    "cannot mutate `{}` because it is currently borrowed",
                    var
                )
            }
            BorrowError::MoveWhileBorrowed { var, .. } => {
                write!(f, "cannot move `{}` because it is currently borrowed", var)
            }
            BorrowError::UseAfterMove { var, .. } => {
                write!(f, "use of moved value `{}`", var)
            }
            BorrowError::BorrowAfterMove { var, .. } => {
                write!(f, "cannot borrow `{}` because it has been moved", var)
            }
            BorrowError::MoveAfterMove { var, .. } => {
                write!(f, "cannot move `{}` because it has already been moved", var)
            }
        }
    }
}

pub struct BorrowChecker {
    borrows: std::collections::HashMap<String, Vec<Borrow>>,
    moved: std::collections::HashSet<String>,
    scope_depth: usize,
    errors: Vec<(BorrowError, Span)>,
    has_ref_types: bool,
    source: Option<String>,
}

impl Default for BorrowChecker {
    fn default() -> Self {
        Self::new()
    }
}

impl BorrowChecker {
    pub fn new() -> Self {
        Self {
            borrows: std::collections::HashMap::new(),
            moved: std::collections::HashSet::new(),
            scope_depth: 0,
            errors: Vec::new(),
            has_ref_types: false,
            source: None,
        }
    }

    pub fn with_source(mut self, source: String) -> Self {
        self.source = Some(source);
        self
    }

    pub fn check_module(&mut self, ast: &AstModule) {
        for func in &ast.functions {
            self.check_function(func);
        }
    }

    pub fn errors(&self) -> &[(BorrowError, Span)] {
        &self.errors
    }

    /// Converts all collected borrow errors into structured Diagnostics.
    pub fn diagnostics(&self) -> Vec<crate::diagnostics::Diagnostic> {
        self.errors
            .iter()
            .map(|(err, span)| err.to_diagnostic(*span))
            .collect()
    }

    pub fn has_ref_types(&self) -> bool {
        self.has_ref_types
    }

    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    fn loc(&self, byte: u32) -> String {
        describe_location(self.source.as_deref(), byte)
    }

    fn source_line(&self, span: Span) -> Option<String> {
        let src = self.source.as_ref()?;
        let start = span.start.0 as usize;
        let end = std::cmp::min(span.end.0 as usize, src.len());
        if start >= src.len() {
            return None;
        }
        let line_start = src[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let line_end = src[end..].find('\n').map(|i| end + i).unwrap_or(src.len());
        let line_text = src[line_start..line_end].trim_end();
        if line_text.is_empty() {
            return None;
        }
        let col = start - line_start;
        let len = end - start;
        let line_num = src[..start].bytes().filter(|&b| b == b'\n').count() + 1;
        let underline = format!(
            "{: >width$}{:^^len$}",
            "",
            "",
            width = col,
            len = len.max(1)
        );
        Some(format!(
            "  --> {}:{}\n       |\n {: >4} | {}\n       | {}",
            line_num,
            col + 1,
            line_num,
            line_text,
            underline
        ))
    }

    fn format_error(&self, err: &BorrowError, offending: Span) -> String {
        let var = err.var_name();
        let conflicting = err.conflicting_span();

        let primary_msg = format!("{}", err);
        let primary_loc = self.loc(offending.start.0);
        let conflicting_loc = self.loc(conflicting.start.0);

        let mut out = format!("[borrow error] {} (at {})", primary_msg, primary_loc);

        if conflicting_loc != primary_loc {
            let kind = match err {
                BorrowError::MutBorrowWhileBorrowed { .. } => "immutable borrow",
                BorrowError::BorrowWhileMutBorrowed { .. } => "mutable borrow",
                BorrowError::MutateWhileBorrowed { .. } => "first borrow",
                BorrowError::MoveWhileBorrowed { .. } => "first borrow",
                BorrowError::UseAfterMove { .. } => "move",
                BorrowError::BorrowAfterMove { .. } => "move",
                BorrowError::MoveAfterMove { .. } => "first move",
            };
            out.push_str(&format!(
                "\n  note: {} of `{}` created here (at {})",
                kind, var, conflicting_loc
            ));
        }

        if let Some(ctx) = self.source_line(conflicting) {
            out.push_str(&format!("\n{}", ctx));
        }
        if let Some(ctx) = self.source_line(offending) {
            out.push_str(&format!("\n{}", ctx));
        }

        out
    }

    pub fn print_errors(&self) {
        for (err, span) in &self.errors {
            eprintln!("{}", self.format_error(err, *span));
        }
    }

    fn check_function(&mut self, func: &AstFunction) {
        self.scope_depth = 0;
        self.borrows.clear();
        self.moved.clear();

        for param in &func.params {
            self.scan_type_for_refs(&param.ty);
            self.add_var(&param.name.name);
        }

        self.scan_type_for_refs(&func.return_ty);
        let _ = self.check_block(&func.body);
    }

    fn scan_type_for_refs(&mut self, ty: &AstType) {
        match ty {
            AstType::Ref(inner, _) | AstType::RefMut(inner, _) => {
                self.has_ref_types = true;
                self.scan_type_for_refs(inner);
            }
            AstType::Option(inner, _)
            | AstType::Chan(inner, _)
            | AstType::Atomic(inner, _)
            | AstType::Mutex(inner, _)
            | AstType::Grad(inner, _)
            | AstType::Sparse(inner, _)
            | AstType::List(inner, _)
            | AstType::Slice(inner, _)
            | AstType::WeakRef(inner, _) => self.scan_type_for_refs(inner),
            AstType::Result(ok, err, _) => {
                self.scan_type_for_refs(ok);
                self.scan_type_for_refs(err);
            }
            AstType::Map(k, v, _) => {
                self.scan_type_for_refs(k);
                self.scan_type_for_refs(v);
            }
            AstType::Tuple(elems, _) => {
                for e in elems {
                    self.scan_type_for_refs(e);
                }
            }
            AstType::Fn { params, ret, .. } => {
                for p in params {
                    self.scan_type_for_refs(p);
                }
                self.scan_type_for_refs(ret);
            }
            AstType::Array { elem, .. } => self.scan_type_for_refs(elem),
            AstType::Generic { args, .. } => {
                for a in args {
                    self.scan_type_for_refs(a);
                }
            }
            _ => {}
        }
    }

    fn check_block(&mut self, block: &AstBlock) -> Vec<Loan> {
        self.scope_depth += 1;
        for stmt in &block.stmts {
            let _ = self.check_stmt(stmt);
        }
        let mut loans = Vec::new();
        if let Some(result) = &block.tail {
            loans = self.check_expr(result);
        }
        self.scope_depth -= 1;
        self.clear_scope_borrows();
        loans
    }

    fn check_stmt(&mut self, stmt: &AstStmt) -> Vec<Loan> {
        match stmt {
            AstStmt::Let { name, init, ty, .. } => {
                if let Some(t) = ty {
                    self.scan_type_for_refs(t);
                }
                let loans = self.check_expr(init);
                for loan in loans {
                    // The binding keeps the borrow alive: register it so a
                    // later mutable use of the borrowed place is caught.
                    self.borrows
                        .entry(loan.place.clone())
                        .or_default()
                        .push(Borrow {
                            var_name: loan.place,
                            is_mut: loan.is_mut,
                            scope_depth: self.scope_depth,
                            span: loan.span,
                        });
                }
                self.add_var(&name.name);
            }
            AstStmt::Expr(e) => {
                let _ = self.check_expr(e);
            }
            AstStmt::Assign { target, value, .. } => {
                let _ = self.check_expr(value);
                if let AstExpr::Ident(ident) = target.as_ref() {
                    self.check_use_after_move(&ident.name, target.span());
                    if let Some(borrows) = self.borrows.get(&ident.name) {
                        if !borrows.is_empty() {
                            self.errors.push((
                                BorrowError::MutateWhileBorrowed {
                                    var: ident.name.clone(),
                                    borrow_span: borrows[0].span,
                                    mutate_span: target.span(),
                                },
                                target.span(),
                            ));
                        }
                    }
                }
            }
            AstStmt::Return { value, .. } => {
                if let Some(v) = value {
                    let _ = self.check_expr(v);
                }
            }
            AstStmt::While { cond, body, .. } => {
                let _ = self.check_expr(cond);
                let _ = self.check_block(body);
            }
            AstStmt::Loop { body, .. } => {
                let _ = self.check_block(body);
            }
            AstStmt::ForRange {
                start, end, body, ..
            } => {
                let _ = self.check_expr(start);
                let _ = self.check_expr(end);
                let _ = self.check_block(body);
            }
            AstStmt::ForEach { iter, body, .. } => {
                let _ = self.check_expr(iter);
                let _ = self.check_block(body);
            }
            AstStmt::ParFor {
                start, end, body, ..
            } => {
                let _ = self.check_expr(start);
                let _ = self.check_expr(end);
                let _ = self.check_block(body);
            }
            AstStmt::Spawn { body, group, .. } => {
                for s in body {
                    let _ = self.check_stmt(s);
                }
                if let Some(g) = group {
                    let _ = self.check_expr(g);
                }
            }
            AstStmt::LetTuple { init, .. } => {
                let _ = self.check_expr(init);
            }
            AstStmt::MaskStmt { body, .. } => {
                let _ = self.check_block(body);
            }
            AstStmt::HandleStmt { expr, arms, .. } => {
                let _ = self.check_expr(expr);
                for arm in arms {
                    let _ = self.check_expr(&arm.body);
                }
            }
            AstStmt::Break { .. } | AstStmt::Continue { .. } => {}
            AstStmt::Defer { expr, .. } => {
                let _ = self.check_expr(expr);
            }
            AstStmt::Yield { expr, .. } => {
                let _ = self.check_expr(expr);
            }
            AstStmt::Select { arms, default, .. } => {
                for arm in arms {
                    let _ = self.check_expr(&arm.channel);
                    let _ = self.check_block(&arm.body);
                }
                if let Some(d) = default {
                    let _ = self.check_block(d);
                }
            }
        }
        Vec::new()
    }

    fn check_expr(&mut self, expr: &AstExpr) -> Vec<Loan> {
        match expr {
            AstExpr::Ref { expr: inner, span } => {
                self.has_ref_types = true;
                if let AstExpr::Ident(ident) = inner.as_ref() {
                    self.check_borrow_after_move(&ident.name, *span);
                    if let Some(borrows) = self.borrows.get(&ident.name) {
                        if borrows.iter().any(|b| b.is_mut) {
                            self.errors.push((
                                BorrowError::BorrowWhileMutBorrowed {
                                    var: ident.name.clone(),
                                    borrow_span: borrows.iter().find(|b| b.is_mut).unwrap().span,
                                    mut_span: *span,
                                },
                                *span,
                            ));
                        }
                    }
                    self.add_borrow(ident.name.clone(), false, *span);
                    vec![Loan {
                        place: ident.name.clone(),
                        is_mut: false,
                        span: *span,
                    }]
                } else {
                    self.check_expr(inner)
                }
            }
            AstExpr::RefMut { expr: inner, span } => {
                self.has_ref_types = true;
                if let AstExpr::Ident(ident) = inner.as_ref() {
                    self.check_borrow_after_move(&ident.name, *span);
                    if let Some(borrows) = self.borrows.get(&ident.name) {
                        if !borrows.is_empty() {
                            self.errors.push((
                                BorrowError::MutBorrowWhileBorrowed {
                                    var: ident.name.clone(),
                                    borrow_span: borrows[0].span,
                                    mut_span: *span,
                                },
                                *span,
                            ));
                        }
                    }
                    self.add_borrow(ident.name.clone(), true, *span);
                    vec![Loan {
                        place: ident.name.clone(),
                        is_mut: true,
                        span: *span,
                    }]
                } else {
                    self.check_expr(inner)
                }
            }
            AstExpr::Deref { expr: inner, .. } => self.check_expr(inner),
            AstExpr::Move { expr: inner, span } => {
                // Check for borrow conflicts first
                if let AstExpr::Ident(ident) = inner.as_ref() {
                    if let Some(borrows) = self.borrows.get(&ident.name) {
                        if !borrows.is_empty() {
                            self.errors.push((
                                BorrowError::MoveWhileBorrowed {
                                    var: ident.name.clone(),
                                    borrow_span: borrows[0].span,
                                    move_span: *span,
                                },
                                *span,
                            ));
                        }
                    }
                    self.mark_moved(&ident.name, *span);
                } else {
                    let _ = self.check_expr(inner);
                }
                Vec::new()
            }
            AstExpr::Ident(ident) => {
                self.check_use_after_move(&ident.name, ident.span);
                Vec::new()
            }
            AstExpr::IntLit { .. }
            | AstExpr::FloatLit { .. }
            | AstExpr::BoolLit { .. }
            | AstExpr::StringLit { .. } => Vec::new(),
            AstExpr::BinOp { lhs, rhs, .. } => {
                let _ = self.check_expr(lhs);
                let _ = self.check_expr(rhs);
                Vec::new()
            }
            AstExpr::UnaryOp { expr, .. } => {
                let _ = self.check_expr(expr);
                Vec::new()
            }
            AstExpr::Call {
                args, named_args, ..
            } => {
                for arg in args {
                    let _ = self.check_expr(arg);
                }
                for (_, arg) in named_args {
                    let _ = self.check_expr(arg);
                }
                Vec::new()
            }
            AstExpr::MethodCall { base, args, .. } => {
                let _ = self.check_expr(base);
                for arg in args {
                    let _ = self.check_expr(arg);
                }
                Vec::new()
            }
            AstExpr::If {
                cond,
                then_block,
                else_block,
                span: _,
            } => {
                let _ = self.check_expr(cond);
                let then_loans = self.check_block(then_block);
                let mut loans = then_loans.clone();
                if let Some(eb) = else_block {
                    let else_loans = self.check_block(eb);
                    self.check_arm_loans(&then_loans, &else_loans);
                    loans.extend(else_loans);
                }
                loans
            }
            AstExpr::Block(block) => self.check_block(block),
            AstExpr::Lambda { body, .. } => {
                let _ = self.check_expr(body);
                Vec::new()
            }
            AstExpr::Index { base, indices, .. } => {
                let _ = self.check_expr(base);
                for idx in indices {
                    let _ = self.check_expr(idx);
                }
                Vec::new()
            }
            AstExpr::Cast { expr, .. } => {
                let _ = self.check_expr(expr);
                Vec::new()
            }
            AstExpr::StructLit { fields, spread, .. } => {
                for (_, v) in fields {
                    let _ = self.check_expr(v);
                }
                if let Some(s) = spread {
                    let _ = self.check_expr(s);
                }
                Vec::new()
            }
            AstExpr::FieldAccess { base, .. } => {
                let _ = self.check_expr(base);
                Vec::new()
            }
            AstExpr::When {
                scrutinee, arms, ..
            } => {
                let _ = self.check_expr(scrutinee);
                let mut loans: Vec<Loan> = Vec::new();
                for arm in arms {
                    let arm_loans = self.check_expr(&arm.body);
                    self.check_arm_loans(&loans, &arm_loans);
                    loans.extend(arm_loans);
                }
                loans
            }
            AstExpr::Tuple { elements, .. } => {
                for e in elements {
                    let _ = self.check_expr(e);
                }
                Vec::new()
            }
            AstExpr::TupleIndex { base, .. } => {
                let _ = self.check_expr(base);
                Vec::new()
            }
            AstExpr::ArrayLit { elems, .. } => {
                for e in elems {
                    let _ = self.check_expr(e);
                }
                Vec::new()
            }
            AstExpr::MapLiteral { entries, .. } => {
                for (k, v) in entries {
                    let _ = self.check_expr(k);
                    let _ = self.check_expr(v);
                }
                Vec::new()
            }
            AstExpr::Await { expr, .. } => {
                let _ = self.check_expr(expr);
                Vec::new()
            }
            AstExpr::Try { expr, .. } => {
                let _ = self.check_expr(expr);
                Vec::new()
            }
            AstExpr::Mask { body, .. } => {
                let _ = self.check_block(body);
                Vec::new()
            }
            AstExpr::Handle { expr, arms, .. } => {
                let _ = self.check_expr(expr);
                for arm in arms {
                    let _ = self.check_expr(&arm.body);
                }
                Vec::new()
            }
            AstExpr::NullCoal { expr, default, .. } => {
                let lhs = self.check_expr(expr);
                let rhs = self.check_expr(default);
                self.check_arm_loans(&lhs, &rhs);
                let mut loans = lhs;
                loans.extend(rhs);
                loans
            }
            AstExpr::TryCatch {
                body, catch_body, ..
            } => {
                let _ = self.check_expr(body);
                let _ = self.check_expr(catch_body);
                Vec::new()
            }
            AstExpr::Raise { args, .. } => {
                for a in args {
                    let _ = self.check_expr(a);
                }
                Vec::new()
            }
            AstExpr::Unsafe { body, .. } => {
                let _ = self.check_expr(body);
                Vec::new()
            }
            AstExpr::Splat { expr, .. } => {
                let _ = self.check_expr(expr);
                Vec::new()
            }
            AstExpr::MacroCall { args, .. } => {
                for a in args {
                    let _ = self.check_expr(a);
                }
                Vec::new()
            }
        }
    }

    /// Cross-arm compatibility for `&x` in one arm and `&mut x` in another
    /// arm of the same expression: the value cannot keep both. Conflicts are
    /// reported on the mutable arm (the error rustc reports at).
    fn check_arm_loans(&mut self, a: &[Loan], b: &[Loan]) {
        for x in a {
            for y in b {
                if x.place == y.place && x.is_mut != y.is_mut {
                    let (ref_loan, mut_loan) = if x.is_mut { (y, x) } else { (x, y) };
                    self.errors.push((
                        BorrowError::MutBorrowWhileBorrowed {
                            var: x.place.clone(),
                            borrow_span: ref_loan.span,
                            mut_span: mut_loan.span,
                        },
                        mut_loan.span,
                    ));
                }
            }
        }
    }

    fn add_var(&mut self, name: &str) {
        self.borrows.entry(name.to_string()).or_default();
    }

    fn add_borrow(&mut self, var_name: String, is_mut: bool, span: Span) {
        self.borrows
            .entry(var_name.clone())
            .or_default()
            .push(Borrow {
                var_name,
                is_mut,
                scope_depth: self.scope_depth,
                span,
            });
    }

    fn clear_scope_borrows(&mut self) {
        for borrows in self.borrows.values_mut() {
            borrows.retain(|b| b.scope_depth < self.scope_depth);
        }
    }

    #[allow(dead_code)]
    fn is_moved(&self, name: &str) -> bool {
        self.moved.contains(name)
    }

    fn mark_moved(&mut self, name: &str, span: Span) {
        if self.moved.contains(name) {
            self.errors.push((
                BorrowError::MoveAfterMove {
                    var: name.to_string(),
                    first_move: span,
                    second_move: span,
                },
                span,
            ));
        } else {
            self.moved.insert(name.to_string());
        }
    }

    fn check_use_after_move(&mut self, name: &str, span: Span) {
        if self.moved.contains(name) {
            self.errors.push((
                BorrowError::UseAfterMove {
                    var: name.to_string(),
                    move_span: span,
                    use_span: span,
                },
                span,
            ));
        }
    }

    fn check_borrow_after_move(&mut self, name: &str, span: Span) {
        if self.moved.contains(name) {
            self.errors.push((
                BorrowError::BorrowAfterMove {
                    var: name.to_string(),
                    move_span: span,
                    borrow_span: span,
                },
                span,
            ));
        }
    }
}
