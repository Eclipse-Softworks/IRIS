//! Diagnostic and error-construction helpers for HIR/THIR -> IR lowering.

use crate::error::LowerError;
use crate::ir::types::IrType;
use crate::parser::ast::{AstBinOp, AstExpr, AstType, Ident};
use crate::parser::lexer::Span;

use super::{hints, Lowerer, CURRENT_FN_ASTS, CURRENT_PRIVATE_ITEMS};

/// `x[i]` on a value `[]` does not index (E0124), with what does.
pub(super) fn not_indexable(base_ty: &IrType, span: crate::parser::lexer::Span) -> LowerError {
    let (what, help) = match base_ty {
        IrType::Str => (
            "a string cannot be indexed with `[]`",
            "`char_at(s, i)` is its i-th character, `str_index(s, i)` the byte at byte offset i",
        ),
        IrType::Map(..) => (
            "a map cannot be indexed with `[]`",
            "`map_get(m, key)` looks a key up",
        ),
        _ => (
            "only arrays, slices, lists and tensors are indexed with `[]`",
            "",
        ),
    };
    let mut diag = crate::diagnostics::Diagnostic::error(format!(
        "{what}; this is `{}`",
        hints::type_name(base_ty)
    ))
    .with_primary_span(span.start.0, span.end.0, None);
    if !help.is_empty() {
        diag = diag.with_help(help);
    }
    LowerError::rich("E0124", diag)
}

/// The operands of `lhs op rhs` have different types. Says which operation,
/// and offers the explicit conversion IRIS requires.
pub(super) fn binop_mismatch(
    op: AstBinOp,
    lhs: &AstExpr,
    lhs_ty: &IrType,
    rhs: &AstExpr,
    rhs_ty: &IrType,
    span: Span,
) -> LowerError {
    LowerError::rich(
        "E0101",
        binop_mismatch_diag(op, lhs, lhs_ty, rhs, rhs_ty, span),
    )
}

pub(super) fn binop_mismatch_thir(
    op: AstBinOp,
    lhs: &crate::thir::ThirExpr,
    lhs_ty: &IrType,
    rhs: &crate::thir::ThirExpr,
    rhs_ty: &IrType,
    span: Span,
) -> LowerError {
    let to_ast = |e: &crate::thir::ThirExpr| -> AstExpr {
        match &e.kind {
            crate::thir::ThirExprKind::Literal(crate::thir::ThirLiteral::Int(value)) => {
                AstExpr::IntLit {
                    value: *value,
                    span: e.span,
                }
            }
            _ => AstExpr::Ident(Ident {
                name: String::new(),
                span: e.span,
            }),
        }
    };
    let lhs_ast = to_ast(lhs);
    let rhs_ast = to_ast(rhs);
    binop_mismatch(op, &lhs_ast, lhs_ty, &rhs_ast, rhs_ty, span)
}

/// `binop_mismatch`'s diagnostic, which the source type checker gives too.
pub(crate) fn binop_mismatch_diag(
    op: AstBinOp,
    lhs: &AstExpr,
    lhs_ty: &IrType,
    rhs: &AstExpr,
    rhs_ty: &IrType,
    span: Span,
) -> crate::diagnostics::Diagnostic {
    use crate::diagnostics::{Diagnostic, Edit, Fix};
    use hints::Kind;
    let verb = match op {
        AstBinOp::Add => "add",
        AstBinOp::Sub => "subtract",
        AstBinOp::Mul => "multiply",
        AstBinOp::Div => "divide",
        AstBinOp::Mod => "take the remainder of",
        AstBinOp::CmpEq
        | AstBinOp::CmpNe
        | AstBinOp::CmpLt
        | AstBinOp::CmpLe
        | AstBinOp::CmpGt
        | AstBinOp::CmpGe => "compare",
        _ => "combine",
    };
    let (l, r) = (hints::type_name(lhs_ty), hints::type_name(rhs_ty));
    let mut diag = Diagnostic::error(format!("cannot {verb} `{l}` and `{r}`"))
        .with_primary_span(span.start.0, span.end.0, None)
        .with_note("IRIS does not convert between types implicitly");
    let wrap = |e: &AstExpr, before: &str, after: &str| -> Vec<Edit> {
        let s = e.span();
        vec![
            Edit {
                start: s.start.0,
                end: s.start.0,
                replacement: before.to_string(),
            },
            Edit {
                start: s.end.0,
                end: s.end.0,
                replacement: after.to_string(),
            },
        ]
    };
    let to_float = |e: &AstExpr, float: &str| -> Fix {
        match e {
            AstExpr::IntLit { value, span } => Fix::replace(
                format!("write it as a float: `{value}.0`"),
                span.start.0,
                span.end.0,
                format!("{value}.0"),
            ),
            _ => Fix {
                message: format!("convert it with `to {float}`"),
                edits: wrap(e, "(", &format!(" to {float})")),
            },
        }
    };
    match (hints::kind(lhs_ty), hints::kind(rhs_ty)) {
        (Some(Kind::Str), Some(Kind::Int | Kind::Float | Kind::Bool))
            if matches!(op, AstBinOp::Add) =>
        {
            diag = diag.with_fix(Fix {
                message: "convert it to text with `to_str`".into(),
                edits: wrap(rhs, "to_str(", ")"),
            });
        }
        (Some(Kind::Int | Kind::Float | Kind::Bool), Some(Kind::Str))
            if matches!(op, AstBinOp::Add) =>
        {
            diag = diag.with_fix(Fix {
                message: "convert it to text with `to_str`".into(),
                edits: wrap(lhs, "to_str(", ")"),
            });
        }
        (Some(Kind::Int), Some(Kind::Float)) => diag = diag.with_fix(to_float(lhs, &r)),
        (Some(Kind::Float), Some(Kind::Int)) => diag = diag.with_fix(to_float(rhs, &l)),
        (Some(Kind::Option), Some(_)) | (Some(_), Some(Kind::Option)) => {
            diag = diag.with_help("an `option` may be `none`: test it with `is_some(x)` or `is_none(x)`, or take its value with `unwrap(x)` once you know it is there");
        }
        _ => {}
    }
    diag.with_code("E0101")
}

/// A builtin was called with the wrong number of arguments. `detail` is the
/// builtin's own description (`send() requires exactly 2 arguments (channel,
/// value)`); the diagnostic says it the same way every other arity error does.
pub(super) fn builtin_arity_error(detail: &str, given: usize, span: Span) -> LowerError {
    use crate::diagnostics::Diagnostic;
    let name: String = detail
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    // The number it takes: the first number after "exactly", "requires",
    // "expects", "takes" or "at least".
    let lower = detail.to_ascii_lowercase();
    let at_least = lower.contains("at least");
    let count = ["exactly ", "at least ", "requires ", "expects ", "takes "]
        .iter()
        .find_map(|w| {
            let at = lower.find(w)? + w.len();
            let digits: String = lower[at..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            digits.parse::<usize>().ok()
        });
    // How it is called, when the description says: `name(a, b)` or a
    // trailing `(a, b)` / `: (a, b)`.
    let params = match detail.find('(') {
        Some(open) if detail[open..].starts_with("()") => {
            detail.rfind('(').filter(|p| *p > open + 1).and_then(|p| {
                detail[p..]
                    .find(')')
                    .map(|q| detail[p + 1..p + q].to_string())
            })
        }
        Some(open) => detail[open..]
            .find(')')
            .map(|q| detail[open + 1..open + q].to_string()),
        None => None,
    }
    .filter(|p| !p.trim().is_empty());
    let were = if given == 1 { "was" } else { "were" };
    let message = match count {
        Some(n) => format!(
            "`{name}` takes {}{n} argument{} but {given} {were} given",
            if at_least { "at least " } else { "" },
            if n == 1 { "" } else { "s" }
        ),
        None => format!(
            "`{name}` was given {given} argument{}: {detail}",
            if given == 1 { "" } else { "s" }
        ),
    };
    let mut diag = Diagnostic::error(message).with_primary_span(span.start.0, span.end.0, None);
    if let Some(p) = params {
        diag = diag.with_note(format!("call it as `{name}({p})`"));
    }
    LowerError::rich("E0109", diag)
}

/// `record` (a type, as written) has no field `field`.
pub(super) fn no_such_field(
    record: &str,
    field: &str,
    fields: &[(String, IrType)],
    at: Span,
) -> LowerError {
    use crate::diagnostics::{Diagnostic, Fix};
    let record = record.split("__").next().unwrap_or(record);
    let list = fields
        .iter()
        .map(|(n, _)| n.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let mut diag = Diagnostic::error(format!("`{record}` has no field `{field}`"))
        .with_primary_span(at.start.0, at.end.0, Some("unknown field".into()))
        .with_note(format!("`{record}` has fields: {list}"));
    if let Some(near) = hints::closest(field, fields.iter().map(|(n, _)| n.as_str())) {
        diag = diag.with_fix(Fix::replace(
            format!("did you mean `{near}`?"),
            at.start.0,
            at.end.0,
            near,
        ));
    }
    LowerError::rich("E0113", diag)
}

/// `value.method(args)` where the value is a built-in type, which has no
/// methods: point at the function that does it, with the call rewritten.
pub(super) fn method_on_builtin_type(
    ty: &IrType,
    method: &str,
    base: &AstExpr,
    args: &[AstExpr],
    span: Span,
) -> LowerError {
    use crate::diagnostics::{Diagnostic, Edit, Fix};
    let ty_name = hints::type_name(ty);
    let function = match method {
        "size" | "length" | "len" | "count" => Some("len"),
        "push" | "append" | "push_back" | "add" => Some("push"),
        "to_string" | "toString" | "to_str" => Some("to_str"),
        other if crate::lsp::builtin_function_names().contains(&other) => Some(other),
        _ => None,
    };
    let method_span = Span::new(base.span().end.0, span.end.0);
    let mut diag = Diagnostic::error(format!("`{ty_name}` has no method `{method}`"))
        .with_primary_span(method_span.start.0 + 1, method_span.start.0 + 1 + method.len() as u32, None)
        .with_note("built-in types have no methods: their operations are functions, called as `f(value, ...)`");
    if let Some(f) = function {
        let base_span = base.span();
        // `s.size()` -> `len(s)`; `xs.push(1)` -> `push(xs, 1)`.
        let mut edits = vec![Edit {
            start: base_span.start.0,
            end: base_span.start.0,
            replacement: format!("{f}("),
        }];
        match args.first() {
            Some(first) => edits.push(Edit {
                start: base_span.end.0,
                end: first.span().start.0,
                replacement: ", ".into(),
            }),
            None => edits.push(Edit {
                start: base_span.end.0,
                end: span.end.0,
                replacement: ")".into(),
            }),
        }
        diag = diag.with_fix(Fix {
            message: format!("call `{f}`"),
            edits,
        });
    }
    LowerError::rich("E0117", diag)
}

pub(super) fn method_on_builtin_type_thir(
    ty: &IrType,
    method: &str,
    base: &crate::thir::ThirExpr,
    args: &[crate::thir::ThirExpr],
    span: Span,
) -> LowerError {
    use crate::diagnostics::{Diagnostic, Edit, Fix};
    let ty_name = hints::type_name(ty);
    let function = match method {
        "size" | "length" | "len" | "count" => Some("len"),
        "push" | "append" | "push_back" | "add" => Some("push"),
        "to_string" | "toString" | "to_str" => Some("to_str"),
        other if crate::lsp::builtin_function_names().contains(&other) => Some(other),
        _ => None,
    };
    let method_span = Span::new(base.span.end.0, span.end.0);
    let mut diag = Diagnostic::error(format!("`{ty_name}` has no method `{method}`"))
        .with_primary_span(
            method_span.start.0 + 1,
            method_span.start.0 + 1 + method.len() as u32,
            None,
        )
        .with_note(
            "built-in types have no methods: their operations are functions, called as `f(value, ...)`",
        );
    if let Some(f) = function {
        let base_span = base.span;
        let mut edits = vec![Edit {
            start: base_span.start.0,
            end: base_span.start.0,
            replacement: format!("{f}("),
        }];
        match args.first() {
            Some(first) => edits.push(Edit {
                start: base_span.end.0,
                end: first.span.start.0,
                replacement: ", ".into(),
            }),
            None => edits.push(Edit {
                start: base_span.end.0,
                end: span.end.0,
                replacement: ")".into(),
            }),
        }
        diag = diag.with_fix(Fix {
            message: format!("call `{f}`"),
            edits,
        });
    }
    LowerError::rich("E0117", diag)
}

/// `name(a: i64, b: str) -> bool`, as declared, for notes in diagnostics.
pub(super) fn signature_of(name: &str) -> Option<String> {
    CURRENT_FN_ASTS.with(|m| {
        let map = m.borrow();
        let f = map.get(name)?;
        let params: Vec<String> = f
            .params
            .iter()
            .map(|p| format!("{}: {}", p.name.name, crate::lsp::ast_type_str(&p.ty)))
            .collect();
        Some(format!(
            "def {}({}) -> {}",
            f.name.name,
            params.join(", "),
            crate::lsp::ast_type_str(&f.return_ty)
        ))
    })
}

/// E0207 for a function declared `-> Infer` whose body does not determine
/// its return type.
pub(super) fn undetermined_return(func: &crate::parser::ast::AstFunction) -> LowerError {
    LowerError::rich(
        "E0207",
        crate::diagnostics::Diagnostic::error(format!(
            "type annotations needed: nothing in `{}` determines its return type",
            func.name.name
        ))
        .with_code("E0207")
        .with_primary_span(func.name.span.start.0, func.name.span.end.0, None)
        .with_help("declare it: `def f(...) -> T`"),
    )
}

/// A type that cannot be an IR type where it is written: the error at its
/// position. IR types have no "not yet known", so these stop the build.
pub(super) fn type_not_lowerable(ty: &AstType) -> LowerError {
    use crate::diagnostics::Diagnostic;
    let span = ty.span();
    let (code, message, help) = match ty {
        AstType::Named(name, _) if name == "Infer" => (
            "E0207",
            "type annotations needed: `Infer` asks the compiler to work the type out, which it does only for a function's return type".to_string(),
            "write the type here",
        ),
        AstType::AssocType { base, assoc_name, .. } => (
            "E0111",
            format!("`{base}::{assoc_name}` is known only inside an impl of its trait, where it names a type"),
            "write the type it stands for",
        ),
        AstType::Generic { name, .. } => (
            "E0111",
            format!("`{name}` needs its type arguments, for example `{name}<i64>`"),
            "",
        ),
        _ => (
            "E0900",
            "internal compiler error: a type form lowering does not handle reached it".to_string(),
            "",
        ),
    };
    let mut d = Diagnostic::error(message)
        .with_code(code)
        .with_primary_span(span.start.0, span.end.0, None);
    if !help.is_empty() {
        d = d.with_help(help);
    }
    LowerError::rich(code, d)
}

/// E0111 for a type name written as `impl Trait for Name` that is not a type
/// of this module: neither a scalar, a record, a choice nor an alias.
pub(super) fn unknown_impl_type(name: &str, span: crate::parser::lexer::Span) -> LowerError {
    LowerError::rich(
        "E0111",
        crate::diagnostics::Diagnostic::error(format!("unknown type `{name}`"))
            .with_code("E0111")
            .with_primary_span(span.start.0, span.end.0, None)
            .with_help("records, choices and type aliases must be declared before their impl"),
    )
}

impl<'m> Lowerer<'m> {
    /// If `name` is unresolvable only because the matching item is not `pub`,
    /// returns the module that defines it. Turns "cannot find 'secret'" into a
    /// message that names the actual cause. See #13.
    pub(super) fn private_item_hint(&self, name: &str) -> Option<String> {
        let suffix = format!("__{}", name);
        CURRENT_PRIVATE_ITEMS.with(|p| {
            p.borrow()
                .iter()
                .find(|m| m.ends_with(&suffix))
                .map(|m| m[..m.len() - suffix.len()].to_owned())
        })
    }

    /// `callee` was given `given` arguments; it takes `required..=max`.
    pub(super) fn arity_error(
        &self,
        resolved: &str,
        callee: &str,
        required: usize,
        max: usize,
        given: usize,
        span: Span,
    ) -> LowerError {
        use crate::diagnostics::Diagnostic;
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
        if let Some(signature) = signature_of(resolved) {
            diag = diag.with_note(format!("declared as `{signature}`"));
        }
        LowerError::rich("E0109", diag)
    }

    /// Argument `slot` of `callee` has type `found` where `expected` is declared.
    pub(super) fn argument_type_error(
        &self,
        callee: &str,
        slot: usize,
        arg: &AstExpr,
        found: &IrType,
        expected: &IrType,
    ) -> LowerError {
        self.argument_type_error_span(callee, slot, arg.span(), found, expected)
    }

    pub(super) fn argument_type_error_span(
        &self,
        callee: &str,
        slot: usize,
        span: Span,
        found: &IrType,
        expected: &IrType,
    ) -> LowerError {
        use crate::diagnostics::{Diagnostic, Edit, Fix};
        let (found_name, expected_name) = (hints::type_name(found), hints::type_name(expected));
        let mut diag = Diagnostic::error(format!(
            "argument {} of `{callee}` must be `{expected_name}`, but this is `{found_name}`",
            slot + 1
        ))
        .with_primary_span(
            span.start.0,
            span.end.0,
            Some(format!("this is `{found_name}`")),
        );
        if let Some(signature) = signature_of(&self.resolve_unqualified_name(callee)) {
            diag = diag.with_note(format!("declared as `{signature}`"));
        }
        if matches!(hints::kind(expected), Some(hints::Kind::Str))
            && matches!(
                hints::kind(found),
                Some(hints::Kind::Int | hints::Kind::Float | hints::Kind::Bool)
            )
        {
            diag = diag.with_fix(Fix {
                message: "convert it with `to_str`".into(),
                edits: vec![
                    Edit {
                        start: span.start.0,
                        end: span.start.0,
                        replacement: "to_str(".into(),
                    },
                    Edit {
                        start: span.end.0,
                        end: span.end.0,
                        replacement: ")".into(),
                    },
                ],
            });
        }
        LowerError::rich("E0110", diag)
    }

    /// "cannot find `name`", with the fix most likely meant: a name from
    /// another language, a near-miss of a name in scope, or the standard
    /// module that exports it. `call` is the whole call and its arguments
    /// when `name` is called.
    pub(super) fn unknown_name(
        &self,
        name: &str,
        span: Span,
        call: Option<(Span, &[AstExpr])>,
    ) -> LowerError {
        use crate::diagnostics::{Diagnostic, Edit, Fix};
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
                let fix = Fix {
                    message: "IRIS writes a range as `start..end`".into(),
                    edits,
                };
                return LowerError::rich("E0100", diag.with_fix(fix));
            }
        }
        if let Some((replacement, why)) = hints::foreign_name(name, call.is_some()) {
            let known = self.scope.contains_key(replacement)
                || self.fn_sigs.contains_key(replacement)
                || crate::lsp::builtin_function_names().contains(&replacement)
                || matches!(replacement, "true" | "false" | "none");
            if known {
                return LowerError::rich(
                    "E0100",
                    diag.with_note(why).with_fix(Fix::replace(
                        format!("use `{replacement}`"),
                        span.start.0,
                        span.end.0,
                        replacement,
                    )),
                );
            }
        }
        let candidates: Vec<&str> = self
            .scope
            .keys()
            .map(|s| s.as_str())
            .chain(self.fn_sigs.keys().map(|s| s.as_str()))
            .chain(crate::lsp::builtin_function_names().iter().copied())
            .collect();
        if let Some(near) = hints::closest(name, candidates) {
            return LowerError::rich(
                "E0100",
                diag.with_fix(Fix::replace(
                    format!("did you mean `{near}`?"),
                    span.start.0,
                    span.end.0,
                    near,
                )),
            );
        }
        if call.is_some() {
            if let Some(module) = hints::module_exporting(name) {
                return LowerError::rich(
                    "E0100",
                    diag.with_note(format!("`{name}` is defined in `std.{module}`"))
                        .with_fix(Fix::insert(
                            format!("bring `std.{module}`"),
                            0,
                            format!("bring std.{module}\n"),
                        )),
                );
            }
        }
        if let Some(module) = self.private_item_hint(name) {
            diag = diag.with_help(format!(
                "`{name}` is defined in module `{module}` but is not `pub`; add `pub` to its definition to use it here"
            ));
        }
        LowerError::rich("E0100", diag)
    }

    pub(super) fn unknown_name_thir(
        &self,
        name: &str,
        span: Span,
        call: Option<(Span, &[crate::thir::ThirExpr])>,
    ) -> LowerError {
        use crate::diagnostics::{Diagnostic, Edit, Fix};
        let mut diag = Diagnostic::error(format!("cannot find `{name}` in this scope"))
            .with_primary_span(span.start.0, span.end.0, Some("not found".into()));

        if let (Some((call_span, args)), "range") = (call, name) {
            if (1..=2).contains(&args.len()) {
                let first = args[0].span;
                let last = args[args.len() - 1].span;
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
                let fix = Fix {
                    message: "IRIS writes a range as `start..end`".into(),
                    edits,
                };
                return LowerError::rich("E0100", diag.with_fix(fix));
            }
        }
        if let Some((replacement, why)) = hints::foreign_name(name, call.is_some()) {
            let known = self.scope.contains_key(replacement)
                || self.fn_sigs.contains_key(replacement)
                || crate::lsp::builtin_function_names().contains(&replacement)
                || matches!(replacement, "true" | "false" | "none");
            if known {
                return LowerError::rich(
                    "E0100",
                    diag.with_note(why).with_fix(Fix::replace(
                        format!("use `{replacement}`"),
                        span.start.0,
                        span.end.0,
                        replacement,
                    )),
                );
            }
        }
        let candidates: Vec<&str> = self
            .scope
            .keys()
            .map(|s| s.as_str())
            .chain(self.fn_sigs.keys().map(|s| s.as_str()))
            .chain(crate::lsp::builtin_function_names().iter().copied())
            .collect();
        if let Some(near) = hints::closest(name, candidates) {
            return LowerError::rich(
                "E0100",
                diag.with_fix(Fix::replace(
                    format!("did you mean `{near}`?"),
                    span.start.0,
                    span.end.0,
                    near,
                )),
            );
        }
        if call.is_some() {
            if let Some(module) = hints::module_exporting(name) {
                return LowerError::rich(
                    "E0100",
                    diag.with_note(format!("`{name}` is defined in `std.{module}`"))
                        .with_fix(Fix::insert(
                            format!("bring `std.{module}`"),
                            0,
                            format!("bring std.{module}\n"),
                        )),
                );
            }
        }
        if let Some(module) = self.private_item_hint(name) {
            diag = diag.with_help(format!(
                "`{name}` is defined in module `{module}` but is not `pub`; add `pub` to its definition to use it here"
            ));
        }
        LowerError::rich("E0100", diag)
    }

    /// E0207 at `span`: neither the checker nor lowering can say what
    /// `what` is. IR types have no "unknown", so this is where an
    /// undetermined type stops the build.
    pub(super) fn undetermined(&self, span: crate::parser::lexer::Span, what: &str) -> LowerError {
        LowerError::rich(
            "E0207",
            crate::diagnostics::Diagnostic::error(format!(
                "type annotations needed: nothing in the program determines {what}"
            ))
            .with_code("E0207")
            .with_primary_span(span.start.0, span.end.0, None)
            .with_help(
                "annotate the binding it is assigned to, for example `val xs: list<str> = list()`",
            ),
        )
    }
}
