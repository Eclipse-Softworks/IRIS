mod exprs;
mod items;
mod stmts;
mod types;

use crate::formatter::doc::{self, Doc};
use crate::formatter::trivia::{Comment, CommentKind, TriviaTable};
use crate::formatter::FormatOptions;
use crate::parser::parse;
use crate::parser::{Span, Spanned, Token};

/// Lossless Prettier-style IRIS surface formatter.
pub struct Formatter<'a> {
    source: &'a str,
    tokens: &'a [Spanned<Token>],
    trivia: TriviaTable,
    pos: usize,
    indent_width: usize,
    max_line_width: usize,
    suppress_next_leading_blank: bool,
    pub forbid_next_trailing_comma: bool,
    next_group_id: u32,
    /// Nesting depth of `(...)` / `[...]` expression groups (reset inside `{ ... }` blocks).
    pub(crate) expr_group_depth: usize,
}

/// How an optional comma at the end of a list item was handled.
pub enum DelimListStyle {
    /// Emit trailing comma in Break mode, omit in Flat mode (`foo(a, b)` -> multi-line `a,\n b,`).
    TrailingCommaOnBreak,
    /// Emit trailing comma unconditionally ONLY when 1 item & flagged as `must_trailing` (e.g. 1-tuple `(x,)`),
    /// otherwise `TrailingCommaOnBreak`.
    Tuple,
    /// Never add a trailing comma that wasn't there (`|a, b|`, `Tensor<...>`).
    NoTrailingComma,
}

impl<'a> Formatter<'a> {
    pub fn new(source: &'a str, tokens: &'a [Spanned<Token>], options: &FormatOptions) -> Self {
        let trivia = TriviaTable::build(source, tokens);
        Self {
            source,
            tokens,
            trivia,
            pos: 0,
            indent_width: options.indent,
            max_line_width: options.max_line_width,
            suppress_next_leading_blank: true,
            forbid_next_trailing_comma: false,
            next_group_id: 1,
            expr_group_depth: 0,
        }
    }

    /// Evaluates `f` inside `(...)` or `[...]` expression grouping delimiters.
    pub(crate) fn with_expr_group<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        self.expr_group_depth += 1;
        let res = f(self);
        self.expr_group_depth -= 1;
        res
    }

    /// Evaluates `f` at statement scope (`expr_group_depth = 0`) for `{ ... }` blocks.
    pub(crate) fn with_stmt_scope<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let prev = self.expr_group_depth;
        self.expr_group_depth = 0;
        let res = f(self);
        self.expr_group_depth = prev;
        res
    }

    pub fn alloc_group_id(&mut self) -> u32 {
        let id = self.next_group_id;
        self.next_group_id = self.next_group_id.wrapping_add(1);
        id
    }

    pub fn mark_forbid_trailing_comma(&mut self) {
        self.forbid_next_trailing_comma = true;
    }

    pub fn tok(&self) -> &Token {
        self.tokens
            .get(self.pos)
            .map(|t| &t.node)
            .unwrap_or(&Token::Eof)
    }

    pub fn peek(&self, offset: usize) -> &Token {
        self.tokens
            .get(self.pos + offset)
            .map(|t| &t.node)
            .unwrap_or(&Token::Eof)
    }

    pub fn at_eof(&self) -> bool {
        matches!(self.tok(), Token::Eof)
    }

    pub fn at_line_start(&self) -> bool {
        self.tokens.get(self.pos).is_some_and(|t| t.newline_before)
    }

    /// Whether the most recently emitted token was a closing brace `}`.
    pub fn prev_tok_is_rbrace(&self) -> bool {
        self.pos
            .checked_sub(1)
            .and_then(|i| self.tokens.get(i))
            .is_some_and(|t| matches!(t.node, Token::RBrace))
    }

    pub fn had_newline_before_cur(&self) -> bool {
        self.trivia.had_newline(self.pos)
    }

    pub fn had_blank_before_cur(&self) -> bool {
        self.trivia.had_blank_line(self.pos)
    }

    pub fn has_own_line_comments_before_cur(&self) -> bool {
        self.trivia.has_own_line_comments(self.pos)
    }

    pub fn raw_slice(&self, idx: usize) -> &'a str {
        let span = self.tokens.get(idx).map(|t| t.span).unwrap_or(Span::at(0));
        let start = (span.start.0 as usize).min(self.source.len());
        let end = (span.end.0 as usize).clamp(start, self.source.len());
        &self.source[start..end]
    }

    pub fn expect(&mut self, expected: &Token) -> Result<Doc, String> {
        if self.tok() == expected {
            let doc = if matches!(expected, Token::RParen | Token::RBracket | Token::RBrace) {
                self.emit_tight_tok()
            } else {
                self.emit_tok()
            };
            if matches!(expected, Token::LBrace | Token::LParen | Token::LBracket) {
                self.suppress_next_leading_blank = true;
            }
            Ok(doc)
        } else {
            Err(format!(
                "formatter expected `{expected}`, found `{}` at token {}",
                self.tok(),
                self.pos
            ))
        }
    }

    /// Emits current token verbatim along with leading own-line comments,
    /// inline block comments (placed right before token with trailing space),
    /// and trailing EOL comments attached via `LineSuffix`.
    pub fn emit_tok(&mut self) -> Doc {
        self.emit_tok_impl(InlinePos::AfterSeparator)
    }

    /// Emits closing delimiter or tight punctuation (`)`, `]`, `}`, `,`, `;`, `.`, `:`),
    /// putting any inline block comment with a leading space BEFORE the punctuation.
    pub fn emit_tight_tok(&mut self) -> Doc {
        self.emit_tok_impl(InlinePos::BeforeTightPunct)
    }

    fn emit_tok_impl(&mut self, inline_pos: InlinePos) -> Doc {
        let idx = self.pos;
        if idx >= self.tokens.len() || matches!(self.tok(), Token::Eof) {
            return Doc::Empty;
        }
        let suppress = std::mem::take(&mut self.suppress_next_leading_blank);
        self.forbid_next_trailing_comma = false;
        let own = self.take_leading_comments_doc(idx, suppress);
        let inline = self.take_inline_before_doc(idx, inline_pos);
        let text = Doc::text(self.raw_slice(idx));
        let suffix = self.take_eol_suffix_doc(idx);
        self.pos += 1;
        Doc::concat(vec![own, inline, text, suffix])
    }

    /// Expects a closing delimiter (`)`, `]`, `}`), returning `(tok_doc, eol_suffix_doc)` so
    /// trailing `// comment` on the closing delimiter stays outside the delimiter's own `Group`.
    pub fn expect_close_split_eol(&mut self, expected: &Token) -> Result<(Doc, Doc), String> {
        if self.tok() != expected {
            return Err(format!(
                "formatter expected `{expected}`, found `{}` at token {}",
                self.tok(),
                self.pos
            ));
        }
        let idx = self.pos;
        let suppress = std::mem::take(&mut self.suppress_next_leading_blank);
        self.forbid_next_trailing_comma = false;
        let own = self.take_leading_comments_doc(idx, suppress);
        let inline = self.take_inline_before_doc(idx, InlinePos::BeforeTightPunct);
        let text = Doc::text(self.raw_slice(idx));
        let suffix = self.take_eol_suffix_doc(idx);
        self.pos += 1;
        Ok((Doc::concat(vec![own, inline, text]), suffix))
    }

    /// Extracts leading own-line comments before the current token so container boundaries
    /// can place them outside an inner item/expression `Doc::group`.
    pub fn take_leading_own_line_comments_cur(&mut self, force_suppress: bool) -> Doc {
        let suppress = std::mem::take(&mut self.suppress_next_leading_blank) || force_suppress;
        self.take_leading_comments_doc(self.pos, suppress)
    }

    /// Consumes an optional `,` token cleanly, preserving any comments attached to it,
    /// returning `(comma_was_present, comments_doc_if_comma_had_eol_or_inline)`.
    /// Returns an `IfBreak(",", "")` + attached trailing suffix when desired, OR a tight
    /// `Doc` for `,` comments.
    pub fn consume_optional_trailing_comma(&mut self, emit_on_break: bool) -> (bool, Doc) {
        if !matches!(self.tok(), Token::Comma) {
            return (
                false,
                if emit_on_break {
                    Doc::if_break(Doc::text(","), Doc::Empty)
                } else {
                    Doc::Empty
                },
            );
        }
        self.forbid_next_trailing_comma = false;
        let idx = self.pos;
        let own = self.take_leading_comments_doc(idx, false);
        let inline = self.take_inline_before_doc(idx, InlinePos::BeforeTightPunct);
        let eol = self.trivia.take_eol_after_prev(idx + 1);
        let has_eol = !eol.is_empty();
        let suffix = comments_to_eol_suffix(&eol);
        self.pos += 1;

        if has_eol || !own.is_empty() || !inline.is_empty() {
            // Comment attached to or around the trailing comma guarantees the comma stays visible.
            (true, Doc::concat(vec![own, inline, Doc::text(","), suffix]))
        } else if emit_on_break {
            (true, Doc::if_break(Doc::text(","), Doc::Empty))
        } else {
            (true, Doc::Empty)
        }
    }

    /// Emits any own-line comments sitting before the current closing delimiter or `Eof`
    /// without consuming that closing token. Returns `Some(doc)` if any dangling comments
    /// were found.
    pub fn take_dangling_comments(&mut self, suppress_first_blank: bool) -> Option<Doc> {
        let idx = self.pos;
        if !self.trivia.has_own_line_comments(idx) {
            return None;
        }
        let (comments, _) = self.trivia.take_own_line_before(idx);
        if comments.is_empty() {
            return None;
        }
        Some(render_own_line_comment_list(
            &comments,
            false,
            suppress_first_blank,
        ))
    }

    /// Consumes and emits only the leading own-line comments before the current token.
    /// Useful before emitting a line-break separator or checking whether the token
    /// actually had leading comments (which forces preceding blank lines to belong above
    /// the comment rather than below it).
    pub fn take_leading_comments_doc(&mut self, idx: usize, suppress_first_blank: bool) -> Doc {
        let (comments, blank_after) = self.trivia.take_own_line_before(idx);
        if comments.is_empty() {
            return Doc::Empty;
        }
        render_own_line_comment_list(&comments, blank_after, suppress_first_blank)
    }

    fn take_inline_before_doc(&mut self, idx: usize, inline_pos: InlinePos) -> Doc {
        let comments = self.trivia.take_inline_before(idx);
        if comments.is_empty() {
            return Doc::Empty;
        }
        let mut parts = Vec::with_capacity(comments.len() * 2);
        match inline_pos {
            InlinePos::AfterSeparator => {
                for c in comments {
                    parts.push(Doc::verbatim(c.text));
                    parts.push(Doc::text(" "));
                }
            }
            InlinePos::BeforeTightPunct => {
                for c in comments {
                    parts.push(Doc::text(" "));
                    parts.push(Doc::verbatim(c.text));
                }
            }
        }
        Doc::concat(parts)
    }

    pub fn take_eol_suffix_doc(&mut self, tok_idx: usize) -> Doc {
        let comments = self.trivia.take_eol_after_prev(tok_idx + 1);
        comments_to_eol_suffix(&comments)
    }

    pub fn build_module_doc(mut self) -> Result<Doc, String> {
        let mut parts = Vec::new();
        let mut is_first_item = true;
        let mut prev_was_single_line = false;
        let mut prev_item_kind = ItemKind::Other;

        while !self.at_eof() {
            let had_blank = !is_first_item && self.had_blank_before_cur();
            let had_comments_before = self.has_own_line_comments_before_cur();
            let leading = self.take_leading_own_line_comments_cur(is_first_item);
            let kind = self.classify_top_level_item();

            let item_doc = self.fmt_top_level_item(true)?;
            let is_single_line =
                doc::renders_single_line(&item_doc, self.indent_width, self.max_line_width, 0);

            if !is_first_item {
                let want_blank = had_blank
                    || had_comments_before
                    || !prev_was_single_line
                    || !is_single_line
                    || (prev_item_kind == ItemKind::Bring && kind != ItemKind::Bring)
                    || (prev_item_kind != ItemKind::Bring && kind == ItemKind::Bring);
                if want_blank {
                    parts.push(Doc::BlankLine);
                } else {
                    parts.push(Doc::HardLine);
                }
            }

            if !leading.is_empty() {
                parts.push(leading);
            }
            parts.push(item_doc);
            is_first_item = false;
            prev_was_single_line = is_single_line;
            prev_item_kind = kind;
        }

        if let Some(eof_comments) = self.take_dangling_comments(is_first_item) {
            if !is_first_item {
                if self.had_blank_before_cur() {
                    parts.push(Doc::BlankLine);
                } else {
                    parts.push(Doc::HardLine);
                }
            }
            parts.push(eof_comments_trimmed_doc(eof_comments));
        }

        if parts.is_empty() {
            return Ok(Doc::Empty);
        }
        parts.push(Doc::HardLine);
        Ok(Doc::concat(parts))
    }

    /// Formats a delimited comma-separated list `( ... )`, `[ ... ]`, or `{ ... }`
    /// using Prettier width-aware grouping, optional trailing comma handling, and dangling
    /// comment preservation.
    pub fn fmt_delimited_list<F>(
        &mut self,
        open: &Token,
        close: &Token,
        pad_flat: bool,
        style: DelimListStyle,
        parse_item: F,
    ) -> Result<Doc, String>
    where
        F: FnMut(&mut Self) -> Result<Doc, String>,
    {
        if matches!(open, Token::LParen | Token::LBracket) {
            self.with_expr_group(|f| {
                f.fmt_delimited_list_inner(open, close, pad_flat, style, parse_item)
            })
        } else {
            self.fmt_delimited_list_inner(open, close, pad_flat, style, parse_item)
        }
    }

    fn fmt_delimited_list_inner<F>(
        &mut self,
        open: &Token,
        close: &Token,
        pad_flat: bool,
        style: DelimListStyle,
        mut parse_item: F,
    ) -> Result<Doc, String>
    where
        F: FnMut(&mut Self) -> Result<Doc, String>,
    {
        let open_doc = self.expect(open)?;
        let mut items = Vec::new();
        let mut trail_doc = Doc::Empty;
        let mut trailing_comma_seen = false;
        let mut last_forbids_trailing_comma = false;

        while self.tok() != close && !self.at_eof() {
            let leading = self.take_leading_own_line_comments_cur(items.is_empty());
            self.forbid_next_trailing_comma = false;
            let parsed_doc = parse_item(self)?;
            last_forbids_trailing_comma = self.forbid_next_trailing_comma;
            let item_doc = if leading.is_empty() {
                parsed_doc
            } else {
                Doc::concat(vec![leading, parsed_doc])
            };
            if matches!(self.tok(), Token::Comma) {
                if self.peek(1) == close {
                    let emit_on_break = !last_forbids_trailing_comma
                        && !matches!(style, DelimListStyle::NoTrailingComma);
                    let (seen, doc) = self.consume_optional_trailing_comma(emit_on_break);
                    trailing_comma_seen = seen;
                    trail_doc = doc;
                    items.push((item_doc, None));
                    break;
                } else {
                    let comma_doc = self.emit_tight_tok();
                    items.push((item_doc, Some(comma_doc)));
                }
            } else {
                items.push((item_doc, None));
                break;
            }
        }

        if items.is_empty() {
            if let Some(dangling) = self.take_dangling_comments(true) {
                let (close_doc, close_eol) = self.expect_close_split_eol(close)?;
                return Ok(Doc::concat(vec![
                    open_doc,
                    Doc::indent(Doc::concat(vec![
                        Doc::HardLine,
                        eof_comments_trimmed_doc(dangling),
                    ])),
                    Doc::HardLine,
                    close_doc,
                    close_eol,
                ]));
            }
            let (close_doc, close_eol) = self.expect_close_split_eol(close)?;
            return Ok(Doc::concat(vec![open_doc, close_doc, close_eol]));
        }

        if !trailing_comma_seen && !items.is_empty() && !last_forbids_trailing_comma {
            let emit_on_break = matches!(
                style,
                DelimListStyle::TrailingCommaOnBreak | DelimListStyle::Tuple
            );
            if emit_on_break {
                trail_doc = Doc::if_break(Doc::text(","), Doc::Empty);
            }
        }
        // For 1-element tuples `(x,)`, keep trailing comma unconditionally so it stays a tuple.
        if matches!(style, DelimListStyle::Tuple) && items.len() == 1 && trailing_comma_seen {
            trail_doc = Doc::text(",");
        }

        let dangling = self.take_dangling_comments(false);
        let (close_doc, close_eol) = self.expect_close_split_eol(close)?;

        let mut inner = Vec::new();
        let edge_break = if pad_flat { Doc::Line } else { Doc::SoftLine };
        inner.push(edge_break.clone());
        let last_idx = items.len() - 1;
        for (i, (item, comma)) in items.into_iter().enumerate() {
            inner.push(item);
            if i < last_idx {
                if let Some(c) = comma {
                    inner.push(c);
                } else {
                    inner.push(Doc::text(","));
                }
                inner.push(Doc::Line);
            } else {
                inner.push(trail_doc.clone());
            }
        }
        if let Some(dangling_doc) = dangling {
            inner.push(Doc::HardLine);
            inner.push(eof_comments_trimmed_doc(dangling_doc));
        }

        let group_doc = Doc::group(Doc::concat(vec![
            open_doc,
            Doc::indent(Doc::concat(inner)),
            edge_break,
            close_doc,
        ]));
        Ok(if close_eol.is_empty() {
            group_doc
        } else {
            Doc::concat(vec![group_doc, close_eol])
        })
    }

    pub fn is_ident_tok(tok: &Token) -> bool {
        matches!(tok, Token::Ident(_))
            || soft_keyword_name(tok).is_some()
            || reserved_noun(tok).is_some()
    }

    pub fn expect_ident(&mut self) -> Result<Doc, String> {
        if Self::is_ident_tok(self.tok()) {
            Ok(self.emit_tok())
        } else {
            Err(format!(
                "formatter expected identifier, found `{}` at token {}",
                self.tok(),
                self.pos
            ))
        }
    }

    pub fn expect_ident_or_keyword(&mut self) -> Result<Doc, String> {
        match self.tok() {
            Token::Eof => Err("formatter expected identifier, found EOF".to_owned()),
            _ => Ok(self.emit_tok()),
        }
    }

    pub fn soft_keyword_at(&self, offset: usize) -> Option<&'static str> {
        soft_keyword_name(self.peek(offset))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Bring,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlinePos {
    AfterSeparator,
    BeforeTightPunct,
}

pub fn soft_keyword_name(tok: &Token) -> Option<&'static str> {
    Some(match tok {
        Token::Record => "record",
        Token::Model => "model",
        Token::Mod => "mod",
        Token::Effect => "effect",
        Token::Loop => "loop",
        Token::Spawn => "spawn",
        Token::Par => "par",
        Token::Defer => "defer",
        Token::Yield => "yield",
        Token::Select => "select",
        Token::To => "to",
        Token::By => "by",
        _ => return None,
    })
}

pub fn reserved_noun(tok: &Token) -> Option<&'static str> {
    Some(match tok {
        Token::Choice => "choice",
        Token::Type => "type",
        Token::Trait => "trait",
        Token::Impl => "impl",
        Token::Match => "match",
        Token::Const => "const",
        Token::Extern => "extern",
        _ => return None,
    })
}

pub fn continues_a_name(tok: &Token) -> bool {
    matches!(
        tok,
        Token::Eq
            | Token::PlusEq
            | Token::MinusEq
            | Token::StarEq
            | Token::SlashEq
            | Token::PercentEq
            | Token::Dot
            | Token::LBracket
            | Token::Semi
            | Token::Comma
            | Token::RParen
            | Token::RBrace
            | Token::Plus
            | Token::Star
            | Token::Slash
            | Token::Percent
            | Token::EqEq
            | Token::NotEq
            | Token::LtEq
            | Token::GtEq
            | Token::AmpAmp
            | Token::PipePipe
            | Token::Question
            | Token::QuestionQuestion
            | Token::LAngle
            | Token::RAngle
            | Token::To
            | Token::Eof
    )
}

pub fn comments_to_eol_suffix(comments: &[Comment]) -> Doc {
    if comments.is_empty() {
        return Doc::Empty;
    }
    let mut parts = Vec::new();
    let mut needs_break = false;
    for c in comments {
        parts.push(Doc::text(" "));
        parts.push(Doc::verbatim(c.text.clone()));
        if matches!(c.kind, CommentKind::Line | CommentKind::Doc) {
            needs_break = true;
        }
    }
    let suffix = Doc::line_suffix(Doc::concat(parts));
    if needs_break {
        Doc::concat(vec![suffix, Doc::BreakParent])
    } else {
        suffix
    }
}

pub fn render_own_line_comment_list(
    comments: &[Comment],
    blank_after: bool,
    suppress_first_blank: bool,
) -> Doc {
    let mut parts = Vec::new();
    for (i, c) in comments.iter().enumerate() {
        if c.blank_before && !(suppress_first_blank && i == 0) {
            parts.push(Doc::BlankLine);
        }
        parts.push(Doc::verbatim(c.text.clone()));
        parts.push(Doc::HardLine);
    }
    if blank_after {
        parts.push(Doc::BlankLine);
    }
    Doc::concat(parts)
}

/// Drops one trailing `HardLine` produced by `render_own_line_comment_list` so
/// dangling comments at the end of a container or file sit directly before the
/// container's own closing line break.
pub fn eof_comments_trimmed_doc(doc: Doc) -> Doc {
    match doc {
        Doc::Concat(mut items) => {
            while matches!(items.last(), Some(Doc::HardLine | Doc::BlankLine)) {
                items.pop();
            }
            Doc::concat(items)
        }
        Doc::HardLine | Doc::BlankLine => Doc::Empty,
        other => other,
    }
}

/// Validates using the compiler's own parser just to guarantee rejected syntax stays Err.
pub fn validate_iris_syntax(tokens: &[Spanned<Token>]) -> Result<(), String> {
    parse::Parser::new(tokens)
        .parse_module()
        .map(|_| ())
        .map_err(|e| format!("parse error: {e}"))
}
