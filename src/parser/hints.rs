//! Parse errors as the programmer would describe the mistake.
//!
//! The parser reports what token it expected; the mistake behind it is often
//! recognisable from the tokens around it -- `=` in a condition, `fn` from
//! another language, `i++`, a forgotten `)` or `}`. `improve` rewrites those
//! errors into diagnostics that name the mistake and carry the fix.

use crate::diagnostics::{Diagnostic, Edit, Fix};
use crate::error::ParseError;
use crate::parser::lexer::{Span, Spanned, Token};

pub fn improve(
    source: &str,
    tokens: &[Spanned<Token>],
    errors: Vec<ParseError>,
) -> Vec<ParseError> {
    let mut out = Vec::with_capacity(errors.len());
    for (i, e) in errors.into_iter().enumerate() {
        out.push(improve_one(source, tokens, e, i == 0));
    }
    out
}

fn improve_one(
    source: &str,
    tokens: &[Spanned<Token>],
    error: ParseError,
    first: bool,
) -> ParseError {
    let (expected, found, span) = match &error {
        ParseError::UnexpectedToken {
            expected,
            found,
            span,
        } => (expected.clone(), found.clone(), *span),
        ParseError::UnexpectedEof { .. } => {
            return unclosed_brace(source, tokens).unwrap_or(error);
        }
        _ => return error,
    };
    if found == "<eof>" {
        return unclosed_brace(source, tokens).unwrap_or(error);
    }
    let Some(idx) = tokens.iter().position(|t| t.span.start == span.start) else {
        return error;
    };
    let tok = &tokens[idx].node;
    let prev = |n: usize| idx.checked_sub(n).map(|i| &tokens[i]);
    let expects_item = expected.contains("'record', 'def'");

    // `fn main()`, `func`, `function`: IRIS declares functions with `def`.
    if let Token::Ident(word) = tok {
        if expects_item
            && matches!(
                word.as_str(),
                "fn" | "func" | "function" | "fun" | "proc" | "sub"
            )
        {
            return rich(
                "E0005",
                Diagnostic::error(format!("IRIS declares functions with `def`, not `{word}`"))
                    .with_primary_span(span.start.0, span.end.0, None)
                    .with_fix(Fix::replace("use `def`", span.start.0, span.end.0, "def")),
            );
        }
    }

    // A statement at the top level of the file.
    if expects_item && first && starts_statement(tok) {
        return rich(
            "E0005",
            Diagnostic::error("statements must be inside a function")
                .with_primary_span(span.start.0, span.end.0, None)
                .with_help("the program starts in `def main() -> i64 { ... }`: put the statement there, or in a function it calls"),
        );
    }

    // `elif`: written just before the token the parser stopped at.
    if let Some(p) = prev(1) {
        if matches!(&p.node, Token::Ident(w) if w == "elif") {
            return rich(
                "E0005",
                Diagnostic::error("IRIS writes `elif` as `else if`")
                    .with_primary_span(p.span.start.0, p.span.end.0, None)
                    .with_fix(Fix::replace(
                        "use `else if`",
                        p.span.start.0,
                        p.span.end.0,
                        "else if",
                    )),
            );
        }
    }

    // `and` / `or` / `not`.
    if let Token::Ident(word) = tok {
        let replacement = match word.as_str() {
            "and" => Some("&&"),
            "or" => Some("||"),
            "not" => Some("!"),
            _ => None,
        };
        if let Some(r) = replacement {
            let end = if r == "!" {
                tokens
                    .get(idx + 1)
                    .map(|n| n.span.start.0)
                    .unwrap_or(span.end.0)
            } else {
                span.end.0
            };
            return rich(
                "E0005",
                Diagnostic::error(format!("IRIS writes `{word}` as `{r}`"))
                    .with_primary_span(span.start.0, span.end.0, None)
                    .with_fix(Fix::replace(format!("use `{r}`"), span.start.0, end, r)),
            );
        }
    }

    // `if x = 2`: assignment where a comparison was meant.
    if matches!(tok, Token::Eq) && expected == "'{'" && in_condition(tokens, idx) {
        return rich(
            "E0005",
            Diagnostic::error("`=` assigns a value; to compare, use `==`")
                .with_primary_span(span.start.0, span.end.0, None)
                .with_fix(Fix::replace(
                    "compare with `==`",
                    span.start.0,
                    span.end.0,
                    "==",
                )),
        );
    }

    // `i++` / `i--`.
    if let (Some(op1), Some(name)) = (prev(1), prev(2)) {
        let same = |a: &Token, b: &Token| std::mem::discriminant(a) == std::mem::discriminant(b);
        if matches!(tok, Token::Plus | Token::Minus)
            && same(tok, &op1.node)
            && op1.span.end.0 == span.start.0
        {
            if let Token::Ident(var) = &name.node {
                let sign = if matches!(tok, Token::Plus) { "+" } else { "-" };
                let replacement = format!("{var} = {var} {sign} 1");
                return rich(
                    "E0005",
                    Diagnostic::error(format!("IRIS has no `{sign}{sign}` operator"))
                        .with_primary_span(op1.span.start.0, span.end.0, None)
                        .with_fix(Fix::replace(
                            format!("write `{replacement}`"),
                            name.span.start.0,
                            span.end.0,
                            replacement,
                        )),
                );
            }
        }
    }

    // `def f(x)`: a parameter without a type.
    if expected == "':'"
        && matches!(tok, Token::RParen | Token::Comma)
        && in_def_header(tokens, idx)
    {
        if let Some(p) = prev(1) {
            if let Token::Ident(name) = &p.node {
                return rich(
                    "E0005",
                    Diagnostic::error(format!("parameter `{name}` needs a type"))
                        .with_primary_span(p.span.start.0, p.span.end.0, None)
                        .with_fix(Fix::insert(
                            format!("declare it, e.g. `{name}: i64`"),
                            p.span.end.0,
                            ": i64",
                        )),
                );
            }
        }
    }

    // A call whose `)` is missing: the parser reached `;` or the next line.
    if expected == "')'" {
        if let Some(p) = prev(1) {
            let next_line = line_of(source, span.start.0) > line_of(source, p.span.end.0);
            if matches!(tok, Token::Semi) || next_line {
                let mut diag = Diagnostic::error("this call is missing its closing `)`")
                    .with_primary_span(
                        span.start.0,
                        span.end.0,
                        Some("expected `)` before this".into()),
                    );
                if let Some(open) = unmatched_paren_before(tokens, idx) {
                    diag = diag.with_secondary_span(
                        open.start.0,
                        open.end.0,
                        Some("opened here".into()),
                    );
                }
                return rich(
                    "E0005",
                    diag.with_fix(Fix::insert("close it", p.span.end.0, ")")),
                );
            }
        }
    }

    // Anything else: keep the parser's own words, minus internal formatting.
    error
}

fn rich(code: &'static str, diag: Diagnostic) -> ParseError {
    ParseError::rich(code, diag)
}

fn starts_statement(tok: &Token) -> bool {
    matches!(
        tok,
        Token::Ident(_)
            | Token::Val
            | Token::Var
            | Token::If
            | Token::While
            | Token::For
            | Token::Return
    )
}

fn line_of(source: &str, byte: u32) -> usize {
    source.as_bytes()[..(byte as usize).min(source.len())]
        .iter()
        .filter(|b| **b == b'\n')
        .count()
}

/// Whether `idx` is inside the condition of an `if` or `while` on its line.
fn in_condition(tokens: &[Spanned<Token>], idx: usize) -> bool {
    tokens[..idx]
        .iter()
        .rev()
        .take_while(|t| !matches!(t.node, Token::LBrace | Token::RBrace | Token::Semi))
        .any(|t| matches!(t.node, Token::If | Token::While))
}

/// Whether `idx` is inside the parameter list of a `def` header.
fn in_def_header(tokens: &[Spanned<Token>], idx: usize) -> bool {
    tokens[..idx]
        .iter()
        .rev()
        .take_while(|t| !matches!(t.node, Token::LBrace | Token::RBrace | Token::Semi))
        .any(|t| matches!(t.node, Token::Def))
}

/// The `(` left open before `idx`.
fn unmatched_paren_before(tokens: &[Spanned<Token>], idx: usize) -> Option<Span> {
    let mut depth = 0i32;
    for t in tokens[..idx].iter().rev() {
        match t.node {
            Token::RParen => depth += 1,
            Token::LParen if depth == 0 => return Some(t.span),
            Token::LParen => depth -= 1,
            Token::LBrace | Token::RBrace => return None,
            _ => {}
        }
    }
    None
}

/// The file ended with a `{` open. Indentation tells which one: a `}` that
/// is indented less than the line of the `{` it closes belongs to an outer
/// block, so that `{` is the one left open.
fn unclosed_brace(source: &str, tokens: &[Spanned<Token>]) -> Option<ParseError> {
    let indent = |byte: u32| -> usize {
        let start = source[..(byte as usize).min(source.len())]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        source[start..]
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .count()
    };
    let mut stack: Vec<Span> = Vec::new();
    let mut suspect: Option<Span> = None;
    for t in tokens {
        match t.node {
            Token::LBrace => stack.push(t.span),
            Token::RBrace => {
                if let Some(open) = stack.pop() {
                    if suspect.is_none() && indent(t.span.start.0) < indent(open.start.0) {
                        suspect = Some(open);
                    }
                }
            }
            _ => {}
        }
    }
    if stack.is_empty() {
        return None;
    }
    let open = suspect.or_else(|| stack.last().copied())?;
    Some(rich(
        "E0006",
        Diagnostic::error("this `{` is never closed")
            .with_primary_span(open.start.0, open.end.0, None)
            .with_help("add the `}` that ends this block; indentation suggests where it belongs"),
    ))
}

/// A string written with single quotes: `'hi'`. Returns the error for the
/// lexer, which sees the quote characters directly.
pub fn single_quoted_string(source: &str, start: u32) -> Option<ParseError> {
    let rest = &source[start as usize..];
    let line_end = rest.find('\n').unwrap_or(rest.len());
    let close = rest[1..line_end].find('\'')? + 1;
    let close = start + close as u32;
    Some(rich(
        "E0002",
        Diagnostic::error("strings are written in double quotes; single quotes hold one character")
            .with_primary_span(start, close + 1, None)
            .with_fix(Fix {
                message: "use double quotes".into(),
                edits: vec![
                    Edit {
                        start,
                        end: start + 1,
                        replacement: "\"".into(),
                    },
                    Edit {
                        start: close,
                        end: close + 1,
                        replacement: "\"".into(),
                    },
                ],
            }),
    ))
}
