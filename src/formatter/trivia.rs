use crate::parser::{Spanned, Token};

/// Kind of source comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentKind {
    Line,
    Doc,
    Block,
}

/// A comment extracted verbatim (with `\r\n` normalized to `\n`) from source.
#[derive(Debug, Clone)]
pub struct Comment {
    pub kind: CommentKind,
    pub text: String,
    pub blank_before: bool,
}

/// Comments and whitespace metadata in the gap immediately preceding token `i`.
#[derive(Debug, Clone, Default)]
pub struct GapTrivia {
    /// Comments on the same line after token `i - 1` (line comments, doc comments,
    /// or block comments followed by a newline).
    pub eol_after_prev: Vec<Comment>,
    /// Same-line `/* ... */` block comments immediately before token `i` (no newline between them and token `i`).
    pub inline_before_next: Vec<Comment>,
    /// Own-line comments preceding token `i` (or dangling inside the current delimiter if token `i` is `}`, `)`, `]`, or `Eof`).
    pub own_line_before_next: Vec<Comment>,
    /// Whether there was a blank line after the last own-line comment before token `i` (or inline comment + token `i`).
    pub blank_after_own: bool,
    /// Whether there was at least one `\n` anywhere in this gap.
    pub had_newline: bool,
    /// Whether there was at least one blank line (`>= 2` `\n`s) in this gap before token `i`.
    pub had_blank_line: bool,
}

/// Indexed trivia gaps parallel to the non-doc-comment token slice (length `tokens.len()`).
#[derive(Debug, Clone)]
pub struct TriviaTable {
    gaps: Vec<GapTrivia>,
}

#[derive(Debug, Clone)]
struct RawComment {
    kind: CommentKind,
    text: String,
    newlines_before: usize,
    newlines_after: usize,
}

impl TriviaTable {
    /// Builds trivia for every gap before `tokens[i]` (`tokens` must not contain `Token::DocComment`
    /// and should end with `Token::Eof`).
    pub fn build(source: &str, tokens: &[Spanned<Token>]) -> Self {
        let mut gaps = Vec::with_capacity(tokens.len());
        let mut prev_end = 0usize;
        for (idx, tok) in tokens.iter().enumerate() {
            let start = (tok.span.start.0 as usize).clamp(prev_end, source.len());
            let gap_str = &source[prev_end..start];
            gaps.push(analyze_gap(idx > 0, gap_str));
            let end = (tok.span.end.0 as usize).clamp(start, source.len());
            prev_end = end;
        }
        Self { gaps }
    }

    pub fn take_eol_after_prev(&mut self, gap_idx: usize) -> Vec<Comment> {
        self.gaps
            .get_mut(gap_idx)
            .map(|g| std::mem::take(&mut g.eol_after_prev))
            .unwrap_or_default()
    }

    pub fn take_inline_before(&mut self, gap_idx: usize) -> Vec<Comment> {
        self.gaps
            .get_mut(gap_idx)
            .map(|g| std::mem::take(&mut g.inline_before_next))
            .unwrap_or_default()
    }

    pub fn take_own_line_before(&mut self, gap_idx: usize) -> (Vec<Comment>, bool) {
        match self.gaps.get_mut(gap_idx) {
            Some(g) => {
                let comments = std::mem::take(&mut g.own_line_before_next);
                let blank_after = g.blank_after_own;
                g.blank_after_own = false;
                (comments, blank_after)
            }
            None => (Vec::new(), false),
        }
    }

    pub fn has_own_line_comments(&self, gap_idx: usize) -> bool {
        self.gaps
            .get(gap_idx)
            .is_some_and(|g| !g.own_line_before_next.is_empty())
    }

    pub fn has_any_comments(&self, gap_idx: usize) -> bool {
        self.gaps.get(gap_idx).is_some_and(|g| {
            !g.eol_after_prev.is_empty()
                || !g.inline_before_next.is_empty()
                || !g.own_line_before_next.is_empty()
        })
    }

    pub fn had_newline(&self, gap_idx: usize) -> bool {
        self.gaps.get(gap_idx).is_some_and(|g| g.had_newline)
    }

    pub fn had_blank_line(&self, gap_idx: usize) -> bool {
        self.gaps.get(gap_idx).is_some_and(|g| g.had_blank_line)
    }
}

fn scan_comments(gap: &str) -> (Vec<RawComment>, usize) {
    let bytes = gap.as_bytes();
    let mut i = 0usize;
    let mut comments = Vec::new();
    let mut newlines_since = 0usize;

    while i < bytes.len() {
        let rest = &gap[i..];
        if rest.starts_with("///") {
            let start = i;
            i += 3;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            let text = gap[start..i].trim_end_matches('\r').to_owned();
            if let Some(prev) = comments.last_mut() {
                let prev_ref: &mut RawComment = prev;
                prev_ref.newlines_after = newlines_since;
            }
            comments.push(RawComment {
                kind: CommentKind::Doc,
                text,
                newlines_before: newlines_since,
                newlines_after: 0,
            });
            newlines_since = 0;
        } else if rest.starts_with("//") {
            let start = i;
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            let text = gap[start..i].trim_end_matches('\r').to_owned();
            if let Some(prev) = comments.last_mut() {
                let prev_ref: &mut RawComment = prev;
                prev_ref.newlines_after = newlines_since;
            }
            comments.push(RawComment {
                kind: CommentKind::Line,
                text,
                newlines_before: newlines_since,
                newlines_after: 0,
            });
            newlines_since = 0;
        } else if rest.starts_with("/*") {
            let start = i;
            i += 2;
            let mut depth = 1usize;
            while i < bytes.len() && depth > 0 {
                let inner = &gap[i..];
                if inner.starts_with("/*") {
                    depth += 1;
                    i += 2;
                } else if inner.starts_with("*/") {
                    depth -= 1;
                    i += 2;
                } else if let Some(ch) = inner.chars().next() {
                    i += ch.len_utf8();
                } else {
                    break;
                }
            }
            let text = gap[start..i].replace("\r\n", "\n").replace('\r', "\n");
            if let Some(prev) = comments.last_mut() {
                let prev_ref: &mut RawComment = prev;
                prev_ref.newlines_after = newlines_since;
            }
            comments.push(RawComment {
                kind: CommentKind::Block,
                text,
                newlines_before: newlines_since,
                newlines_after: 0,
            });
            newlines_since = 0;
        } else {
            if bytes[i] == b'\n' {
                newlines_since += 1;
            }
            i += 1;
        }
    }

    if let Some(last) = comments.last_mut() {
        last.newlines_after = newlines_since;
    }

    (comments, newlines_since)
}

fn analyze_gap(has_prev_token: bool, gap: &str) -> GapTrivia {
    let had_newline = gap.as_bytes().contains(&b'\n');
    let (raw, trailing_newlines) = scan_comments(gap);

    if raw.is_empty() {
        return GapTrivia {
            had_newline,
            had_blank_line: trailing_newlines >= 2,
            ..Default::default()
        };
    }

    // Partition out EOL trailing comments attached to `prev_token`:
    // These are the initial run of comments with zero `\n` before the first comment
    // and zero `\n` between consecutive comments on that line, provided that line
    // actually ends in a `//`/`///` comment OR is followed by at least one `\n`.
    let mut eol_count = 0usize;
    if has_prev_token && raw[0].newlines_before == 0 {
        let mut idx = 0usize;
        while idx < raw.len() && (idx == 0 || raw[idx].newlines_before == 0) {
            idx += 1;
        }
        let has_line_comment = raw[..idx]
            .iter()
            .any(|c| matches!(c.kind, CommentKind::Line | CommentKind::Doc));
        let ends_line = has_line_comment || raw[idx - 1].newlines_after > 0;
        if ends_line {
            eol_count = idx;
        }
    }

    let mut eol_after_prev = Vec::with_capacity(eol_count);
    for c in &raw[..eol_count] {
        eol_after_prev.push(Comment {
            kind: c.kind,
            text: c.text.clone(),
            blank_before: false,
        });
    }

    let rest = &raw[eol_count..];
    if rest.is_empty() {
        let newlines_after_eol = raw[eol_count - 1].newlines_after;
        return GapTrivia {
            eol_after_prev,
            had_newline,
            had_blank_line: newlines_after_eol >= 2,
            ..Default::default()
        };
    }

    // Comments at the very end of `rest` that are `Block` comments and have NO `\n`
    // after them before `next_token` sit inline right before `next_token`.
    let mut inline_start = rest.len();
    if trailing_newlines == 0 {
        let mut idx = rest.len();
        while idx > 0 && rest[idx - 1].kind == CommentKind::Block {
            if idx < rest.len() && rest[idx - 1].newlines_after > 0 {
                break;
            }
            idx -= 1;
        }
        // Only treat the suffix as inline if either:
        // - it had no `\n` before it (true inline mid-line comment), or
        // - it appears on the same line immediately preceding the next token.
        inline_start = idx;
    }

    let own_raw = &rest[..inline_start];
    let inline_raw = &rest[inline_start..];

    let own_line_before_next: Vec<Comment> = own_raw
        .iter()
        .enumerate()
        .map(|(k, c)| Comment {
            kind: c.kind,
            text: c.text.clone(),
            blank_before: if k == 0 && eol_count == 0 && !has_prev_token {
                // At the very top of file, suppress leading blank line before comment 0.
                false
            } else {
                c.newlines_before >= 2
            },
        })
        .collect();

    let inline_before_next: Vec<Comment> = inline_raw
        .iter()
        .map(|c| Comment {
            kind: c.kind,
            text: c.text.clone(),
            blank_before: false,
        })
        .collect();

    let blank_after_own = if let Some(last_own) = own_raw.last() {
        last_own.newlines_after >= 2
    } else {
        false
    };

    let had_blank_line = own_line_before_next.first().is_some_and(|c| c.blank_before)
        || (own_line_before_next.is_empty()
            && rest
                .first()
                .is_some_and(|c| (has_prev_token || eol_count > 0) && c.newlines_before >= 2))
        || blank_after_own;

    GapTrivia {
        eol_after_prev,
        inline_before_next,
        own_line_before_next,
        blank_after_own,
        had_newline,
        had_blank_line,
    }
}
