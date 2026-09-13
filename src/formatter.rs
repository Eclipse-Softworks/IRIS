use std::path::{Path, PathBuf};

use crate::parser::lexer::{Lexer, Spanned, Token};
use crate::parser::Parser;

/// Options controlling code formatting.
#[derive(Debug, Clone)]
pub struct FormatOptions {
    /// Number of spaces per indentation level.
    pub indent: usize,
    /// Maximum line width before wrapping (currently unused by the formatter).
    pub max_line_width: usize,
}

impl Default for FormatOptions {
    fn default() -> Self {
        FormatOptions {
            indent: 4,
            max_line_width: 100,
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Formats an IRIS source string according to style rules.
/// Invalid input is never rewritten: lexing or parsing errors are returned to
/// the caller so editors can leave the document untouched.
pub fn format_source(source: &str, options: &FormatOptions) -> Result<String, String> {
    let tokens = Lexer::new(source)
        .tokenize()
        .map_err(|error| format!("cannot format invalid IRIS source: {}", error))?;
    Parser::new(&tokens)
        .parse_module()
        .map_err(|error| format!("cannot format invalid IRIS source: {}", error))?;
    Ok(format_iris(source, options, &tokens))
}

/// Formats a single file.  When `check_only` is true the file is not modified;
/// returns `Ok(true)` if the file would change.
pub fn format_file(path: &Path, options: &FormatOptions, check_only: bool) -> Result<bool, String> {
    let source = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read '{}': {}", path.display(), e))?;
    let formatted = format_source(&source, options)?;
    let changed = formatted != source;
    if !check_only && changed {
        std::fs::write(path, &formatted)
            .map_err(|e| format!("cannot write '{}': {}", path.display(), e))?;
    }
    Ok(changed)
}

/// Formats every `*.iris` file in `dir` (non-recursive).
/// Returns `(total_files, changed_files)`.
pub fn format_directory(
    dir: &Path,
    options: &FormatOptions,
    check_only: bool,
) -> Result<(usize, usize), String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot read directory '{}': {}", dir.display(), e))?;
    let mut total = 0usize;
    let mut changed = 0usize;
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|ext| ext == "iris")
                && p.file_name()
                    .is_some_and(|n| n.to_str().is_some_and(|s| !s.starts_with('.')))
        })
        .collect();
    files.sort();
    for path in &files {
        total += 1;
        if format_file(path, options, check_only)? {
            changed += 1;
            if !check_only {
                eprintln!("  formatted {}", path.display());
            }
        }
    }
    Ok((total, changed))
}

// ---------------------------------------------------------------------------
// Core formatter (extracted from src/lsp.rs format_iris)
// ---------------------------------------------------------------------------

/// Token-stream based IRIS formatter.  Normalises indentation and spacing.
fn format_iris(source: &str, options: &FormatOptions, spanned_tokens: &[Spanned<Token>]) -> String {
    let comments = scan_comments(source);
    let mut comment_index = 0usize;

    let indent_width = options.indent;
    let indent_str = |depth: usize| " ".repeat(indent_width * depth);

    let mut out = String::with_capacity(source.len() + 64);
    let mut indent = 0usize;
    let mut at_line_start = true;
    let mut prev_was_newline = false;
    let mut last_emitted_end = 0usize;
    let mut prev_tok_was_pub = false;

    let is_top_level_kw = |t: &Token| {
        matches!(
            t,
            Token::Def
                | Token::Record
                | Token::Choice
                | Token::Model
                | Token::Const
                | Token::Type
                | Token::Extern
                | Token::Trait
                | Token::Impl
                | Token::Mod
                | Token::Pub
        )
    };

    let is_stmt_kw = |t: &Token| {
        matches!(
            t,
            Token::Val
                | Token::Var
                | Token::For
                | Token::While
                | Token::Loop
                | Token::Return
                | Token::Break
                | Token::Continue
                | Token::Spawn
                | Token::Par
        )
    };

    let mut skip_next = false;
    for (idx, spanned) in spanned_tokens.iter().enumerate() {
        if skip_next {
            skip_next = false;
            last_emitted_end = spanned.span.end.0 as usize;
            continue;
        }

        let tok = &spanned.node;
        while comment_index < comments.len()
            && comments[comment_index].start < spanned.span.start.0 as usize
        {
            emit_comment(
                &mut out,
                &comments[comment_index],
                indent,
                indent_width,
                &mut at_line_start,
            );
            last_emitted_end = comments[comment_index].start + comments[comment_index].text.len();
            comment_index += 1;
        }
        let tok_str = token_to_str(tok, source, spanned.span.start.0, spanned.span.end.0);
        if tok_str.is_empty() {
            continue;
        }

        if is_top_level_kw(tok)
            && indent == 0
            && !out.is_empty()
            && !(matches!(tok, Token::Def) && prev_tok_was_pub)
        {
            if !out.ends_with("\n\n") {
                if out.ends_with('\n') {
                    out.push('\n');
                } else {
                    out.push_str("\n\n");
                }
            }
            at_line_start = true;
        } else if idx > 0 && at_line_start && !out.ends_with("\n\n") && !out.is_empty() {
            let curr_start = spanned.span.start.0 as usize;
            if curr_start >= last_emitted_end
                && curr_start <= source.len()
                && has_blank_line(&source[last_emitted_end..curr_start])
            {
                out.push('\n');
            }
        }
        last_emitted_end = spanned.span.end.0 as usize;

        if indent > 0 && !at_line_start && is_stmt_kw(tok) {
            out.push('\n');
            at_line_start = true;
        }

        if at_line_start {
            let ind = indent_str(indent);
            out.push_str(&ind);
            at_line_start = false;
        }

        if tok_str == "{" {
            let next_is_rbrace = spanned_tokens
                .get(idx + 1)
                .is_some_and(|next| matches!(next.node, Token::RBrace));
            if next_is_rbrace {
                if !out.ends_with(' ') && !out.ends_with('\n') {
                    out.push(' ');
                }
                out.push('{');
                out.push('}');
                skip_next = true;
                prev_was_newline = false;
                continue;
            }
            if !out.ends_with(' ') && !out.ends_with('\n') {
                out.push(' ');
            }
            out.push('{');
            indent += 1;
            out.push('\n');
            at_line_start = true;
            prev_was_newline = true;
            continue;
        }

        if tok_str == "}" {
            indent = indent.saturating_sub(1);
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&indent_str(indent));
            out.push('}');
            let joins_next = spanned_tokens
                .get(idx + 1)
                .is_some_and(|next| matches!(next.node, Token::Else | Token::Catch | Token::With));
            if joins_next {
                out.push(' ');
                at_line_start = false;
            } else {
                out.push('\n');
                at_line_start = true;
            }
            prev_was_newline = true;
            continue;
        }

        if tok_str == ";" {
            if out.ends_with(' ') {
                out.pop();
            }
            out.push(';');
            out.push('\n');
            at_line_start = true;
            prev_was_newline = false;
            continue;
        }

        if tok_str == ":" {
            if out.ends_with(' ') {
                out.pop();
            }
            out.push(':');
            out.push(' ');
            prev_was_newline = false;
            continue;
        }

        if tok_str == "::" {
            if out.ends_with(' ') {
                out.pop();
            }
            out.push_str("::");
            prev_was_newline = false;
            continue;
        }

        if tok_str == "." {
            if out.ends_with(' ') {
                out.pop();
            }
            out.push('.');
            prev_was_newline = false;
            continue;
        }

        if tok_str == "?" {
            if out.ends_with(' ') {
                out.pop();
            }
            out.push('?');
            prev_was_newline = false;
            continue;
        }

        if tok_str == ".." || tok_str == "..=" {
            if out.ends_with(' ') {
                out.pop();
            }
            out.push_str(&tok_str);
            prev_was_newline = false;
            continue;
        }

        if tok_str == "," {
            if out.ends_with(' ') {
                out.pop();
            }
            out.push(',');
            let user_broke_line = if idx + 1 < spanned_tokens.len() {
                let comma_end = spanned.span.end.0 as usize;
                let next_start = spanned_tokens[idx + 1].span.start.0 as usize;
                if next_start >= comma_end && next_start <= source.len() {
                    source[comma_end..next_start].contains('\n')
                } else {
                    false
                }
            } else {
                false
            };
            let projected_width = projected_group_width(spanned_tokens, idx + 1, source);
            if user_broke_line
                || current_line_width(&out).saturating_add(projected_width)
                    >= options.max_line_width
            {
                out.push('\n');
                out.push_str(&indent_str(indent));
                at_line_start = false;
            } else {
                out.push(' ');
            }
            prev_was_newline = false;
            continue;
        }

        if tok_str == "[" || tok_str == "(" {
            out.push_str(&tok_str);
            let user_broke = if idx + 1 < spanned_tokens.len() {
                let end = spanned.span.end.0 as usize;
                let next_start = spanned_tokens[idx + 1].span.start.0 as usize;
                if next_start >= end && next_start <= source.len() {
                    source[end..next_start].contains('\n')
                } else {
                    false
                }
            } else {
                false
            };
            if user_broke {
                indent += 1;
                out.push('\n');
                out.push_str(&indent_str(indent));
                at_line_start = false;
            }
            prev_was_newline = false;
            continue;
        }

        if tok_str == "]" || tok_str == ")" {
            let user_broke = if idx > 0 {
                let prev_end = spanned_tokens[idx - 1].span.end.0 as usize;
                let start = spanned.span.start.0 as usize;
                if start >= prev_end && start <= source.len() {
                    source[prev_end..start].contains('\n')
                } else {
                    false
                }
            } else {
                false
            };
            if user_broke {
                indent = indent.saturating_sub(1);
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str(&indent_str(indent));
            } else if out.ends_with(' ') {
                out.pop();
            }
            out.push_str(&tok_str);
            prev_was_newline = false;
            continue;
        }

        if tok_str == "<" {
            let is_generic = idx > 0
                && spanned_tokens[idx - 1].span.end.0 == spanned.span.start.0
                && matches!(
                    spanned_tokens[idx - 1].node,
                    Token::Ident(_)
                        | Token::Str
                        | Token::I64
                        | Token::I32
                        | Token::U64
                        | Token::F64
                        | Token::Bool
                        | Token::Tensor
                        | Token::RAngle
                );
            if is_generic {
                if out.ends_with(' ') {
                    out.pop();
                }
                out.push('<');
            } else {
                if !out.ends_with(' ') && !out.ends_with('\n') {
                    out.push(' ');
                }
                out.push('<');
                out.push(' ');
            }
            prev_was_newline = false;
            continue;
        }

        if tok_str == ">" {
            let is_generic_close = idx > 0
                && matches!(
                    spanned_tokens[idx - 1].node,
                    Token::Ident(_)
                        | Token::Str
                        | Token::I64
                        | Token::I32
                        | Token::I8
                        | Token::U8
                        | Token::U32
                        | Token::U64
                        | Token::Usize
                        | Token::F64
                        | Token::F32
                        | Token::Bool
                        | Token::Tensor
                        | Token::RAngle
                )
                && (idx + 1 >= spanned_tokens.len()
                    || matches!(
                        spanned_tokens[idx + 1].node,
                        Token::Comma
                            | Token::Semi
                            | Token::RParen
                            | Token::RBracket
                            | Token::RBrace
                            | Token::RAngle
                            | Token::Arrow
                            | Token::FatArrow
                            | Token::Eq
                            | Token::LBrace
                    ));
            if is_generic_close {
                if out.ends_with(' ') {
                    out.pop();
                }
                out.push('>');
            } else {
                if !out.ends_with(' ') && !out.ends_with('\n') {
                    out.push(' ');
                }
                out.push('>');
                out.push(' ');
            }
            prev_was_newline = false;
            continue;
        }

        let needs_space = matches!(
            tok_str.as_str(),
            "=" | "=="
                | "!="
                | "<="
                | ">="
                | "+="
                | "-="
                | "*="
                | "/="
                | "%="
                | "+"
                | "-"
                | "*"
                | "/"
                | "%"
                | "&&"
                | "||"
                | "->"
                | "=>"
                | "to"
        );

        if needs_space {
            if !out.ends_with(' ') && !out.ends_with('\n') {
                out.push(' ');
            }
            out.push_str(&tok_str);
            out.push(' ');
        } else {
            let last = out.chars().last();
            let needs_sep = matches!(last, Some(c) if c.is_alphanumeric() || c == '_' || c == '"');
            if needs_sep && !tok_str.starts_with(['.', '(', '[']) {
                out.push(' ');
            }
            out.push_str(&tok_str);
        }

        let _ = (idx, prev_was_newline, spanned.span);
        prev_was_newline = false;
        prev_tok_was_pub = matches!(tok, Token::Pub);
    }

    while comment_index < comments.len() {
        emit_comment(
            &mut out,
            &comments[comment_index],
            indent,
            indent_width,
            &mut at_line_start,
        );
        comment_index += 1;
    }

    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn has_blank_line(text: &str) -> bool {
    let mut saw_newline = false;
    for b in text.bytes() {
        if b == b'\n' {
            if saw_newline {
                return true;
            }
            saw_newline = true;
        } else if b != b' ' && b != b'\t' && b != b'\r' {
            saw_newline = false;
        }
    }
    false
}

#[derive(Debug)]
struct SourceComment {
    start: usize,
    text: String,
    inline: bool,
}

fn scan_comments(source: &str) -> Vec<SourceComment> {
    let bytes = source.as_bytes();
    let mut comments = Vec::new();
    let mut i = 0usize;
    let mut line_has_code = false;

    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                line_has_code = false;
                i += 1;
            }
            b' ' | b'\t' | b'\r' => i += 1,
            b'"' | b'\'' => {
                let quote = bytes[i];
                line_has_code = true;
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i = (i + 2).min(bytes.len());
                    } else if bytes[i] == quote {
                        i += 1;
                        break;
                    } else {
                        i += 1;
                    }
                }
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                let start = i;
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                // Documentation comments are real lexer tokens and must not be
                // emitted a second time as trivia.
                if bytes.get(start + 2) != Some(&b'/') {
                    comments.push(SourceComment {
                        start,
                        text: source[start..i].to_owned(),
                        inline: line_has_code,
                    });
                }
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                let start = i;
                let inline = line_has_code;
                i += 2;
                let mut depth = 1usize;
                while i < bytes.len() && depth > 0 {
                    if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
                        depth += 1;
                        i += 2;
                    } else if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                comments.push(SourceComment {
                    start,
                    text: source[start..i].to_owned(),
                    inline,
                });
                line_has_code = true;
            }
            _ => {
                line_has_code = true;
                i += 1;
            }
        }
    }

    comments
}

fn emit_comment(
    out: &mut String,
    comment: &SourceComment,
    indent: usize,
    indent_width: usize,
    at_line_start: &mut bool,
) {
    let is_line = comment.text.starts_with("//");
    let is_single_line_block = comment.text.starts_with("/*") && !comment.text.contains('\n');

    if comment.inline && is_single_line_block && !*at_line_start {
        if !out.ends_with(' ') && !out.ends_with('\n') {
            out.push(' ');
        }
        out.push_str(comment.text.trim_end());
        out.push(' ');
        *at_line_start = false;
        return;
    }

    if !*at_line_start {
        if comment.inline && is_line {
            out.push(' ');
        } else {
            out.push('\n');
        }
    }
    if out.ends_with('\n') || out.is_empty() {
        out.push_str(&" ".repeat(indent * indent_width));
    }

    let mut lines = comment.text.lines().peekable();
    while let Some(line) = lines.next() {
        out.push_str(line.trim_end());
        if lines.peek().is_some() {
            out.push('\n');
            out.push_str(&" ".repeat(indent * indent_width));
        }
    }
    out.push('\n');
    *at_line_start = true;
}

fn current_line_width(text: &str) -> usize {
    text.rsplit_once('\n')
        .map(|(_, line)| line.chars().count())
        .unwrap_or_else(|| text.chars().count())
}

/// Estimate the width of the next comma-delimited group. This lets the
/// formatter break before adding an argument that would exceed the configured
/// line width instead of waiting until the line is already too long.
fn projected_group_width(tokens: &[Spanned<Token>], start: usize, source: &str) -> usize {
    let mut width = 1usize;
    for spanned in tokens.iter().skip(start) {
        if matches!(
            spanned.node,
            Token::Comma | Token::RParen | Token::RBracket | Token::RBrace | Token::Eof
        ) {
            break;
        }
        let token = token_to_str(
            &spanned.node,
            source,
            spanned.span.start.0,
            spanned.span.end.0,
        );
        width = width.saturating_add(token.chars().count() + 1);
    }
    width
}

/// Returns the source text for a token (for formatting).
fn token_to_str(tok: &Token, source: &str, start: u32, end: u32) -> String {
    match tok {
        Token::Def => "def".into(),
        Token::DefMacro => "defmacro".into(),
        Token::Val => "val".into(),
        Token::Var => "var".into(),
        Token::Let => "let".into(),
        Token::If => "if".into(),
        Token::Else => "else".into(),
        Token::Match => "match".into(),
        Token::When => "when".into(),
        Token::For => "for".into(),
        Token::While => "while".into(),
        Token::Loop => "loop".into(),
        Token::Break => "break".into(),
        Token::Continue => "continue".into(),
        Token::Return => "return".into(),
        Token::Record => "record".into(),
        Token::Choice => "choice".into(),
        Token::Model => "model".into(),
        Token::Layer => "layer".into(),
        Token::Input => "input".into(),
        Token::Output => "output".into(),
        Token::Const => "const".into(),
        Token::Type => "type".into(),
        Token::Extern => "extern".into(),
        Token::Trait => "trait".into(),
        Token::Impl => "impl".into(),
        Token::Mod => "mod".into(),
        Token::Pub => "pub".into(),
        Token::Bring => "bring".into(),
        Token::Async => "async".into(),
        Token::Await => "await".into(),
        Token::Spawn => "spawn".into(),
        Token::Par => "par".into(),
        Token::In => "in".into(),
        Token::To => "to".into(),
        Token::BoolLit(b) => {
            if *b {
                "true".into()
            } else {
                "false".into()
            }
        }
        Token::I64 => "i64".into(),
        Token::I32 => "i32".into(),
        Token::I8 => "i8".into(),
        Token::U8 => "u8".into(),
        Token::U32 => "u32".into(),
        Token::U64 => "u64".into(),
        Token::Usize => "usize".into(),
        Token::F64 => "f64".into(),
        Token::F32 => "f32".into(),
        Token::Bool => "bool".into(),
        Token::Str => "str".into(),
        Token::Tensor => "tensor".into(),
        Token::LBrace => "{".into(),
        Token::RBrace => "}".into(),
        Token::LParen => "(".into(),
        Token::RParen => ")".into(),
        Token::LBracket => "[".into(),
        Token::RBracket => "]".into(),
        Token::LAngle => "<".into(),
        Token::RAngle => ">".into(),
        Token::Comma => ",".into(),
        Token::Semi => ";".into(),
        Token::Colon => ":".into(),
        Token::DoubleColon => "::".into(),
        Token::Dot => ".".into(),
        Token::DotDot => "..".into(),
        Token::DotDotEq => "..=".into(),
        Token::Arrow => "->".into(),
        Token::FatArrow => "=>".into(),
        Token::Eq => "=".into(),
        Token::EqEq => "==".into(),
        Token::NotEq => "!=".into(),
        Token::LtGt => "<>".into(),
        Token::LtEq => "<=".into(),
        Token::GtEq => ">=".into(),
        Token::PlusEq => "+=".into(),
        Token::MinusEq => "-=".into(),
        Token::StarEq => "*=".into(),
        Token::SlashEq => "/=".into(),
        Token::PercentEq => "%=".into(),
        Token::Plus => "+".into(),
        Token::Minus => "-".into(),
        Token::Star => "*".into(),
        Token::Slash => "/".into(),
        Token::Percent => "%".into(),
        Token::Pipe => "|".into(),
        Token::AmpAmp => "&&".into(),
        Token::PipePipe => "||".into(),
        Token::Bang => "!".into(),
        Token::At => "@".into(),
        Token::Question => "?".into(),
        Token::QuestionQuestion => "??".into(),
        Token::Ident(s) => s.clone(),
        Token::IntLit(n) => n.to_string(),
        Token::FloatLit(f) => {
            if f.fract() == 0.0 {
                format!("{:.1}", f)
            } else {
                f.to_string()
            }
        }
        Token::StringLit(_) | Token::CharLit(_) | Token::FStringLit(_) => source
            .get(start as usize..end as usize)
            .unwrap_or_default()
            .to_owned(),
        Token::Eof => String::new(),
        Token::Effect => "effect".to_owned(),
        Token::With => "with".to_owned(),
        Token::Yield => "yield".to_owned(),
        Token::Dyn => "dyn".to_owned(),
        Token::Resume => "resume".to_owned(),
        Token::By => "by".to_owned(),
        Token::Defer => "defer".to_owned(),
        Token::Try => "try".to_owned(),
        Token::Catch => "catch".to_owned(),
        Token::Raise => "raise".to_owned(),
        Token::Amp => "&".to_owned(),
        Token::Move => "move".to_owned(),
        Token::Unsafe => "unsafe".to_owned(),
        Token::Select => "select".to_owned(),
        // The lexer deliberately trims documentation comment whitespace and
        // advances past the newline, so its span is not a faithful source
        // slice. Reconstruct only this comment token from its preserved text.
        Token::DocComment(text) => {
            if text.is_empty() {
                "///".to_owned()
            } else {
                format!("/// {}", text)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_all_comment_forms_and_literal_spelling() {
        let source = r#"// leading
/// docs
def main() -> i64 {
    val url = "https://example.test/a//b"; // trailing
    /* outer /* nested */ block */
    val quote = "a\\n\\\"b";
    assert(len(url) > 0);
    assert(len(quote) > 0);
    return 0
}
"#;
        let formatted = format_source(source, &FormatOptions::default()).unwrap();
        assert!(formatted.contains("// leading"));
        assert!(formatted.contains("/// docs"));
        assert!(formatted.contains("// trailing"));
        assert!(formatted.contains("/* outer /* nested */ block */"));
        assert!(formatted.contains(r#""https://example.test/a//b""#));
        assert!(formatted.contains(r#""a\\n\\\"b""#));
    }

    #[test]
    fn formatting_is_idempotent() {
        let source = "def main()->i64{/* keep */ val x=1;assert(x==1);return 0}\n";
        let once = format_source(source, &FormatOptions::default()).unwrap();
        let twice = format_source(&once, &FormatOptions::default()).unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn invalid_source_is_not_rewritten() {
        assert!(format_source("def broken( -> {", &FormatOptions::default()).is_err());
    }

    #[test]
    fn configured_line_width_wraps_at_safe_comma_boundaries() {
        let options = FormatOptions {
            indent: 4,
            max_line_width: 32,
        };
        let source =
            "def f(first_parameter: i64, second_parameter: i64) -> i64 { first_parameter }\n";
        let formatted = format_source(source, &options).unwrap();
        assert!(formatted.contains(",\n"));
    }

    #[test]
    fn formats_colons_without_leading_space() {
        let source =
            "def add(x: i64, y: i64) -> i64 {\n    val a: i64 = 1;\n    return a + x + y;\n}\n";
        let formatted = format_source(source, &FormatOptions::default()).unwrap();
        assert!(formatted.contains("x: i64, y: i64"));
        assert!(formatted.contains("val a: i64 = 1;"));
        assert!(!formatted.contains("x : i64"));
        assert!(!formatted.contains("val a : i64"));
    }

    #[test]
    fn formats_double_colons_and_dots_without_spaces() {
        let source =
            "def test() -> i64 {\n    val p = Point.new();\n    val x = p.x;\n    return 0;\n}\n";
        let formatted = format_source(source, &FormatOptions::default()).unwrap();
        assert!(formatted.contains("Point.new()"));
        assert!(formatted.contains("p.x"));
        assert!(!formatted.contains("Point .new()"));
        assert!(!formatted.contains("p .x"));
    }

    #[test]
    fn formats_ranges_compactly() {
        let source = "def test() -> i64 {\n    for i in 0..10 {\n        assert(i >= 0);\n    }\n    return 0;\n}\n";
        let formatted = format_source(source, &FormatOptions::default()).unwrap();
        assert!(formatted.contains("0..10"));
        assert!(!formatted.contains("0 .. 10"));
    }

    #[test]
    fn preserves_user_newlines_and_top_level_separation() {
        let source = "def foo() -> i64 {\n    val a = 1;\n\n    val b = 2;\n    return a + b;\n}\n\ndef bar() -> i64 {\n    return 0;\n}\n";
        let formatted = format_source(source, &FormatOptions::default()).unwrap();
        // Preserves blank line inside function
        assert!(formatted.contains("val a = 1;\n\n    val b = 2;"));
        // Preserves blank line between top-level functions
        assert!(formatted.contains("}\n\ndef bar()"));
    }

    #[test]
    fn empty_blocks_kept_compact() {
        let source = "record Empty {}\n";
        let formatted = format_source(source, &FormatOptions::default()).unwrap();
        assert!(formatted.contains("record Empty {}"));
    }
}
