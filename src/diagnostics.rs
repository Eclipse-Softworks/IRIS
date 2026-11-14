//! Source diagnostics: byte-to-line/col mapping and human-readable error rendering.
//!
//! Provides both plain-text and ANSI-colored rendering of compiler errors
//! in a rustc-style format with source excerpts, span underlines, error
//! codes, and optional help/hint notes.

use crate::error::{CodegenError, Error, InterpError, LowerError, ParseError, PassError};

// ---------------------------------------------------------------------------
// ANSI color helpers (no external dependency)
// ---------------------------------------------------------------------------

/// ANSI escape codes for terminal coloring.
mod ansi {
    pub const RESET: &str = "\x1b[0m";
    pub const BOLD: &str = "\x1b[1m";
    pub const BOLD_RED: &str = "\x1b[1;31m";
    pub const BOLD_BLUE: &str = "\x1b[1;34m";
    pub const BOLD_GREEN: &str = "\x1b[1;32m";
}

// ---------------------------------------------------------------------------
// Core line/col mapping
// ---------------------------------------------------------------------------

/// Converts a byte offset within `source` to a 1-based `(line, col)` pair.
///
/// # Examples
/// ```text
/// "abc\ndef\n", byte 4  → (2, 1)   // 'd' is first char of line 2
/// "hello",     byte 2  → (1, 3)   // 'l' at column 3 on line 1
/// ```
pub fn byte_to_line_col(source: &str, byte: u32) -> (u32, u32) {
    let byte = byte as usize;
    let mut line = 1u32;
    let mut col = 1u32;
    for (i, ch) in source.char_indices() {
        if i == byte {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Returns the 1-based `(line, col)` for the start of the given byte span.
pub fn span_to_line_col(source: &str, start_byte: u32) -> (u32, u32) {
    byte_to_line_col(source, start_byte)
}

// ---------------------------------------------------------------------------
// Error span/byte extraction
// ---------------------------------------------------------------------------

/// Extracts the starting byte offset from errors that carry location info.
///
/// Returns `None` for errors without source position (pass errors, runtime errors).
pub fn error_byte_offset(err: &Error) -> Option<u32> {
    extract_byte(err)
}

fn extract_byte(err: &Error) -> Option<u32> {
    extract_span(err).map(|(start, _)| start)
}

/// Extracts the full `(start, end)` byte span from errors that carry one.
///
/// For errors that only store a single position (e.g. `UnexpectedChar`),
/// the returned span is one byte wide: `(pos, pos + 1)`.
fn extract_span(err: &Error) -> Option<(u32, u32)> {
    match err {
        Error::Parse(pe) => match pe {
            ParseError::UnexpectedChar { pos, .. } => Some((*pos, *pos + 1)),
            ParseError::UnterminatedString { pos, .. } => Some((*pos, *pos + 1)),
            ParseError::InvalidEscape { pos, .. } => Some((*pos, *pos + 2)),
            ParseError::InvalidLiteral { span, .. } => Some((span.start.0, span.end.0)),
            ParseError::UnexpectedToken { span, .. } => Some((span.start.0, span.end.0)),
            ParseError::UnexpectedEof { .. } => None,
            ParseError::RecursionLimitExceeded { span, .. } => Some((span.start.0, span.end.0)),
            ParseError::Diag { diag, .. } => diag.primary_span(),
            ParseError::Multiple(errors) => errors
                .first()
                .and_then(|e| extract_span(&Error::Parse(e.clone()))),
        },
        Error::Lower(le) => match le {
            LowerError::UndefinedVariable { span, .. } => Some((span.start.0, span.end.0)),
            LowerError::TypeMismatch { span, .. } => Some((span.start.0, span.end.0)),
            LowerError::DuplicateFunction { span, .. } => Some((span.start.0, span.end.0)),
            LowerError::Unsupported { span, .. } => Some((span.start.0, span.end.0)),
            LowerError::Rejected { span, .. } => Some((span.start.0, span.end.0)),
            LowerError::UndefinedLayer { span, .. } => Some((span.start.0, span.end.0)),
            LowerError::DuplicateNode { span, .. } => Some((span.start.0, span.end.0)),
            LowerError::InvalidLayerParam { span, .. } => Some((span.start.0, span.end.0)),
            LowerError::UnknownOp { .. } => None,
            LowerError::Diag { diag, .. } => diag.primary_span(),
            LowerError::Multiple(errors) => errors
                .first()
                .and_then(|e| extract_span(&Error::Lower(e.clone_shallow()))),
        },
        Error::Pass(crate::error::PassError::Diag { diag, .. }) => diag.primary_span(),
        Error::Interp(InterpError::Located { byte, byte_end, .. }) => {
            Some((*byte, byte_end.unwrap_or(*byte + 1)))
        }
        Error::Codegen(CodegenError::Runtime { span, .. }) => *span,
        _ => None,
    }
}

/// Returns a contextual help note for common errors, or `None`.
fn error_hint(err: &Error) -> Option<&'static str> {
    match err {
        Error::Parse(pe) => match pe {
            ParseError::UnexpectedChar { ch: '@', .. } => {
                Some("IRIS does not use '@' — decorators are not supported")
            }
            ParseError::UnexpectedChar { ch: '#', .. } => {
                Some("comments in IRIS start with '//', not '#'")
            }
            ParseError::UnterminatedString { .. } => {
                Some("make sure every '\"' has a matching closing '\"'")
            }
            ParseError::InvalidEscape { .. } => {
                Some("valid escape sequences: \\n, \\t, \\r, \\\\, \\\"")
            }
            ParseError::UnexpectedEof { .. } => {
                Some("check for unmatched braces '{}' or parentheses '()'")
            }
            _ => None,
        },
        Error::Lower(le) => match le {
            LowerError::UndefinedVariable { name, .. } if name == "struct" => {
                Some("IRIS uses 'record' instead of 'struct'")
            }
            LowerError::UndefinedVariable { name, .. } if name == "enum" => {
                Some("IRIS uses 'choice' instead of 'enum'")
            }
            LowerError::UndefinedVariable { name, .. } if name == "match" => {
                Some("IRIS uses 'when' instead of 'match'")
            }
            LowerError::UndefinedVariable { name, .. } if name == "import" => {
                Some("IRIS uses 'bring \"file.iris\"' instead of 'import'")
            }
            LowerError::TypeMismatch { .. } => {
                Some("try adding an explicit type annotation to clarify the expected type")
            }
            LowerError::DuplicateFunction { .. } => {
                Some("rename one of the functions, or move it to a separate module")
            }
            _ => None,
        },
        Error::Pass(pe) => match pe {
            PassError::MissingTerminator { .. } => {
                Some("every block must end with a return value or branch — check for missing 'return' or 'else' clauses")
            }
            _ => None,
        },
        Error::Interp(ie) => {
            // Unwrap Located to get hints for the inner error.
            let inner = match ie {
                InterpError::Located { inner, .. } => inner.as_ref(),
                other => other,
            };
            runtime_hint(err.diagnostic_code(), &inner.to_string())
        }
        Error::Codegen(CodegenError::Runtime { code, message, .. }) => runtime_hint(code, message),
        _ => None,
    }
}

/// Help for a runtime error, keyed by its code so a failure reads the same
/// whichever backend ran the program.
fn runtime_hint(code: &str, message: &str) -> Option<&'static str> {
    match code {
        "E0402" => Some("check that the divisor is not zero before dividing"),
        "E0403" if message.contains("tensor of rank") || message.contains("for dimension") => {
            Some("index a tensor with one index per dimension, each from 0 to that dimension's length - 1, or with one flat row-major position")
        }
        "E0403" => Some("valid indices run from 0 to `len(xs) - 1`; check the index first, or loop with `for x in xs`"),
        "E0404" => Some("check that operand types match — use `to_i64()`, `to_f64()`, or `to_str()` for conversions"),
        "E0406" if message.contains("unwrap called on none") => {
            Some("check it with `is_some(x)` first, or give a default with `x.unwrap_or(default)`")
        }
        "E0406" if message.contains("unwrap called on err") => {
            Some("handle both cases with `when r { ok(v) => ..., err(e) => ... }`, or pass the error on with `?`")
        }
        "E0408" => Some("check that the recursion reaches its base case; iteration this deep is better written as a loop"),
        "E0409" => Some("integer arithmetic is checked; `wrapping_add`, `wrapping_sub` and `wrapping_mul` wrap around instead, and `f64` holds larger magnitudes"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Plain-text rendering (backward-compatible)
// ---------------------------------------------------------------------------

/// Renders a rustc-style diagnostic for `err`, with a source excerpt, span
/// underline, error code, and optional help note.
///
/// ```text
/// error[E0001]: [syntax error] unexpected character '@' …
///  --> 3:3
///   |
/// 3 |   @invalid
///   |   ^
///   = help: IRIS does not use '@' — decorators are not supported
/// ```
pub fn render_error(source: &str, err: &Error) -> String {
    render_error_inner(source, err, None, false)
}

/// Like [`render_error`] but includes the filename in the location arrow.
///
/// ```text
///  --> src/main.iris:3:3
/// ```
pub fn render_error_with_file(source: &str, err: &Error, filename: &str) -> String {
    render_error_inner(source, err, Some(filename), false)
}

/// Renders a colored (ANSI) diagnostic for terminal output.
pub fn render_error_colored(source: &str, err: &Error) -> String {
    render_error_inner(source, err, None, true)
}

/// Renders a colored (ANSI) diagnostic with filename.
pub fn render_error_colored_with_file(source: &str, err: &Error, filename: &str) -> String {
    render_error_inner(source, err, Some(filename), true)
}

// ---------------------------------------------------------------------------
// Structured Multi-Span Diagnostic Engine (rustc-style)
// ---------------------------------------------------------------------------

/// A span of at most one byte widened to the token that starts there: a
/// whole identifier, number or string literal. Errors found on the IR know
/// only where an expression starts.
pub fn token_span(source: &str, start: u32, end: u32) -> (u32, u32) {
    if end > start + 1 {
        return (start, end);
    }
    let bytes = source.as_bytes();
    let i = start as usize;
    let Some(&first) = bytes.get(i) else {
        return (start, end.max(start));
    };
    let mut j = i + 1;
    if first == b'"' {
        while j < bytes.len() && bytes[j] != b'"' && bytes[j] != b'\n' {
            if bytes[j] == b'\\' {
                j += 1;
            }
            j += 1;
        }
        if j < bytes.len() && bytes[j] == b'"' {
            j += 1;
        }
    } else if first.is_ascii_alphanumeric() || first == b'_' {
        while j < bytes.len()
            && (bytes[j].is_ascii_alphanumeric()
                || bytes[j] == b'_'
                || (first.is_ascii_digit() && bytes[j] == b'.'))
        {
            j += 1;
        }
    } else {
        return (start, end.max(start + 1));
    }
    (start, j as u32)
}

/// Severity level of a diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticLevel {
    Error,
    Warning,
    Note,
    Help,
}

impl std::fmt::Display for DiagnosticLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DiagnosticLevel::Error => write!(f, "error"),
            DiagnosticLevel::Warning => write!(f, "warning"),
            DiagnosticLevel::Note => write!(f, "note"),
            DiagnosticLevel::Help => write!(f, "help"),
        }
    }
}

/// A source span with optional label within a diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DiagnosticSpan {
    pub start: u32,
    pub end: u32,
    pub label: Option<String>,
    pub is_primary: bool,
}

/// One text replacement: bytes `start..end` of the source become `replacement`
/// (an insertion when `start == end`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Edit {
    pub start: u32,
    pub end: u32,
    pub replacement: String,
}

/// A change that resolves a diagnostic, applied as a whole: the editor's quick
/// fix, `suggested_replacement` in JSON, and a preview line in the terminal.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Fix {
    pub message: String,
    pub edits: Vec<Edit>,
}

impl Fix {
    /// Replace `start..end` with `replacement`.
    pub fn replace(
        message: impl Into<String>,
        start: u32,
        end: u32,
        replacement: impl Into<String>,
    ) -> Self {
        Fix {
            message: message.into(),
            edits: vec![Edit {
                start,
                end,
                replacement: replacement.into(),
            }],
        }
    }

    /// Insert `text` at byte `at`.
    pub fn insert(message: impl Into<String>, at: u32, text: impl Into<String>) -> Self {
        Self::replace(message, at, at, text)
    }

    /// `source` with this fix applied. Edits must not overlap.
    pub fn apply(&self, source: &str) -> String {
        let mut edits = self.edits.clone();
        edits.sort_by_key(|e| std::cmp::Reverse(e.start));
        let mut out = source.to_string();
        for e in &edits {
            let (a, b) = (e.start as usize, (e.end as usize).max(e.start as usize));
            if b <= out.len() && out.is_char_boundary(a) && out.is_char_boundary(b) {
                out.replace_range(a..b, &e.replacement);
            }
        }
        out
    }

    /// The lines this fix changes, as they read after it is applied.
    fn preview(&self, source: &str) -> Vec<(u32, String)> {
        let Some(first) = self.edits.iter().map(|e| e.start).min() else {
            return Vec::new();
        };
        let last = self
            .edits
            .iter()
            .map(|e| e.end.max(e.start))
            .max()
            .unwrap_or(first);
        let (first_line, _) = byte_to_line_col(source, first);
        let (last_line, _) = byte_to_line_col(source, last);
        let added: usize = self
            .edits
            .iter()
            .map(|e| e.replacement.matches('\n').count())
            .sum::<usize>();
        let removed: usize = self
            .edits
            .iter()
            .map(|e| {
                source
                    .get(e.start as usize..(e.end as usize).max(e.start as usize))
                    .map(|t| t.matches('\n').count())
                    .unwrap_or(0)
            })
            .sum::<usize>();
        let fixed = self.apply(source);
        let count = (last_line - first_line) as usize + 1 + added
            - removed.min(added + (last_line - first_line) as usize);
        fixed
            .lines()
            .enumerate()
            .skip(first_line as usize - 1)
            .take(count.clamp(1, 6))
            .map(|(i, l)| (i as u32 + 1, l.to_string()))
            .collect()
    }
}

/// A structured multi-span diagnostic with optional notes, helps, and JSON serialization.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Diagnostic {
    pub code: Option<String>,
    pub level: DiagnosticLevel,
    pub message: String,
    pub spans: Vec<DiagnosticSpan>,
    pub notes: Vec<String>,
    pub helps: Vec<String>,
    pub suggestions: Vec<String>,
    /// Machine-applicable changes that resolve this diagnostic.
    #[serde(default)]
    pub fixes: Vec<Fix>,
}

impl Diagnostic {
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            code: None,
            level: DiagnosticLevel::Error,
            message: message.into(),
            spans: Vec::new(),
            notes: Vec::new(),
            helps: Vec::new(),
            suggestions: Vec::new(),
            fixes: Vec::new(),
        }
    }

    pub fn warning(message: impl Into<String>) -> Self {
        Self {
            code: None,
            level: DiagnosticLevel::Warning,
            message: message.into(),
            spans: Vec::new(),
            notes: Vec::new(),
            helps: Vec::new(),
            suggestions: Vec::new(),
            fixes: Vec::new(),
        }
    }

    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    pub fn with_primary_span(mut self, start: u32, end: u32, label: Option<String>) -> Self {
        self.spans.push(DiagnosticSpan {
            start,
            end,
            label,
            is_primary: true,
        });
        self
    }

    pub fn with_secondary_span(mut self, start: u32, end: u32, label: Option<String>) -> Self {
        self.spans.push(DiagnosticSpan {
            start,
            end,
            label,
            is_primary: false,
        });
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.helps.push(help.into());
        self
    }

    pub fn with_suggestion(mut self, suggestion: impl Into<String>) -> Self {
        self.suggestions.push(suggestion.into());
        self
    }

    pub fn with_fix(mut self, fix: Fix) -> Self {
        self.fixes.push(fix);
        self
    }

    /// The primary span, or the first span, as `(start, end)`.
    pub fn primary_span(&self) -> Option<(u32, u32)> {
        self.spans
            .iter()
            .find(|s| s.is_primary)
            .or_else(|| self.spans.first())
            .map(|s| (s.start, s.end))
    }

    /// Renders this diagnostic to a human-readable text string with source excerpts and underlines.
    pub fn render(&self, source: &str, filename: Option<&str>, colored: bool) -> String {
        let mut out = if colored {
            let (level_color, level_str) = match self.level {
                DiagnosticLevel::Error => (ansi::BOLD_RED, "error"),
                DiagnosticLevel::Warning => (ansi::BOLD_GREEN, "warning"),
                DiagnosticLevel::Note => (ansi::BOLD_BLUE, "note"),
                DiagnosticLevel::Help => (ansi::BOLD_GREEN, "help"),
            };
            if let Some(ref code) = self.code {
                format!(
                    "{}{}[{}]{}: {}{}{}\n",
                    level_color,
                    level_str,
                    code,
                    ansi::RESET,
                    ansi::BOLD,
                    self.message,
                    ansi::RESET,
                )
            } else {
                format!(
                    "{}{}{}: {}{}{}\n",
                    level_color,
                    level_str,
                    ansi::RESET,
                    ansi::BOLD,
                    self.message,
                    ansi::RESET,
                )
            }
        } else if let Some(ref code) = self.code {
            format!("{}[{}]: {}\n", self.level, code, self.message)
        } else {
            format!("{}: {}\n", self.level, self.message)
        };

        if !self.spans.is_empty() {
            // Find primary span or fallback to first span for header location
            let primary = self
                .spans
                .iter()
                .find(|s| s.is_primary)
                .unwrap_or(&self.spans[0]);
            let (primary_line, primary_col) = byte_to_line_col(source, primary.start);
            let loc = if let Some(f) = filename {
                format!("{}:{}:{}", f, primary_line, primary_col)
            } else {
                format!("{}:{}", primary_line, primary_col)
            };

            let max_line_num = self
                .spans
                .iter()
                .map(|s| byte_to_line_col(source, s.start).0)
                .max()
                .unwrap_or(primary_line);
            let gutter_width = max_line_num.to_string().len().max(2);
            let gutter = " ".repeat(gutter_width);

            if colored {
                out.push_str(&format!(" {}-->{} {}\n", ansi::BOLD_BLUE, ansi::RESET, loc));
            } else {
                out.push_str(&format!(" --> {}\n", loc));
            }

            // Spans grouped by line, in order: a line is shown once, with
            // every span on it underlined, as rustc does. A span's label sits
            // after its underline when it is the line's last; an earlier one
            // hangs below its underline on a `|`.
            let mut placed: Vec<(u32, usize, usize, Option<String>, bool)> = Vec::new();
            for span in &self.spans {
                let (start, end) = token_span(source, span.start, span.end);
                let (line, col) = byte_to_line_col(source, start);
                let source_line = source.lines().nth((line - 1) as usize).unwrap_or("");
                let indent = (col as usize).saturating_sub(1);
                let max_len = source_line.len().saturating_sub(indent).max(1);
                let len = ((end.saturating_sub(start)) as usize).clamp(1, max_len);
                placed.push((line, indent, len, span.label.clone(), span.is_primary));
            }
            placed.sort_by_key(|p| (p.0, p.1));
            let mut i = 0;
            while i < placed.len() {
                let line = placed[i].0;
                let mut group: Vec<&(u32, usize, usize, Option<String>, bool)> = Vec::new();
                while i < placed.len() && placed[i].0 == line {
                    group.push(&placed[i]);
                    i += 1;
                }
                let source_line = source.lines().nth((line - 1) as usize).unwrap_or("");
                let line_str = line.to_string();
                let pad = " ".repeat(gutter_width.saturating_sub(line_str.len()));
                let (blue, red, reset) = if colored {
                    (ansi::BOLD_BLUE, ansi::BOLD_RED, ansi::RESET)
                } else {
                    ("", "", "")
                };
                out.push_str(&format!(
                    "{gutter} {blue}|{reset}
"
                ));
                out.push_str(&format!(
                    "{pad}{blue}{line_str} |{reset} {source_line}
"
                ));
                // The underline row.
                let mut row = String::new();
                let mut width = 0usize;
                for (_, indent, len, _, primary) in &group {
                    if *indent < width {
                        continue;
                    }
                    row.push_str(&" ".repeat(indent - width));
                    let (ch, color) = if *primary { ("^", red) } else { ("-", blue) };
                    row.push_str(&format!("{color}{}{reset}", ch.repeat(*len)));
                    width = indent + len;
                }
                let last = group.last().expect("a group has a span");
                if let Some(label) = &last.3 {
                    let color = if last.4 { red } else { blue };
                    row.push_str(&format!(" {color}{label}{reset}"));
                }
                out.push_str(&format!(
                    "{gutter} {blue}|{reset} {row}
"
                ));
                // Earlier labels, rightmost first, each below its underline.
                let earlier: Vec<&&(u32, usize, usize, Option<String>, bool)> = group
                    [..group.len() - 1]
                    .iter()
                    .filter(|p| p.3.is_some())
                    .collect();
                for (k, p) in earlier.iter().enumerate().rev() {
                    let bars = |upto: usize| -> String {
                        let mut r = String::new();
                        let mut w = 0usize;
                        for q in &earlier[..upto] {
                            if q.1 >= w {
                                r.push_str(&" ".repeat(q.1 - w));
                                r.push_str(&format!("{blue}|{reset}"));
                                w = q.1 + 1;
                            }
                        }
                        r
                    };
                    let connector = bars(k + 1);
                    out.push_str(&format!(
                        "{gutter} {blue}|{reset} {connector}
"
                    ));
                    let lead = bars(k);
                    let lead_width = earlier[..k].iter().map(|q| q.1 + 1).max().unwrap_or(0);
                    let color = if p.4 { red } else { blue };
                    let label = p.3.as_deref().unwrap_or("");
                    out.push_str(&format!(
                        "{gutter} {blue}|{reset} {lead}{}{color}{label}{reset}
",
                        " ".repeat(p.1.saturating_sub(lead_width))
                    ));
                }
            }
        }

        // Render notes
        for note in &self.notes {
            if colored {
                out.push_str(&format!(
                    "   {}= note:{} {}\n",
                    ansi::BOLD_BLUE,
                    ansi::RESET,
                    note
                ));
            } else {
                out.push_str(&format!("   = note: {}\n", note));
            }
        }

        // Render helps
        for help in &self.helps {
            if colored {
                out.push_str(&format!(
                    "   {}= help:{} {}\n",
                    ansi::BOLD_GREEN,
                    ansi::RESET,
                    help
                ));
            } else {
                out.push_str(&format!("   = help: {}\n", help));
            }
        }

        // Render suggestions
        for suggestion in &self.suggestions {
            if colored {
                out.push_str(&format!(
                    "   {}= suggestion:{} {}\n",
                    ansi::BOLD_GREEN,
                    ansi::RESET,
                    suggestion
                ));
            } else {
                out.push_str(&format!("   = suggestion: {}\n", suggestion));
            }
        }

        // Render fixes: the message, then the changed lines as they would read.
        for fix in &self.fixes {
            if colored {
                out.push_str(&format!(
                    "{}help{}: {}\n",
                    ansi::BOLD_GREEN,
                    ansi::RESET,
                    fix.message
                ));
            } else {
                out.push_str(&format!("help: {}\n", fix.message));
            }
            let lines = fix.preview(source);
            let width = lines
                .iter()
                .map(|(n, _)| n.to_string().len())
                .max()
                .unwrap_or(1)
                .max(2);
            if !lines.is_empty() {
                out.push_str(&format!("{} |\n", " ".repeat(width)));
            }
            for (n, text) in lines {
                let num = n.to_string();
                if colored {
                    out.push_str(&format!(
                        "{}{}{} |{} {}{}{}\n",
                        " ".repeat(width - num.len()),
                        ansi::BOLD_BLUE,
                        num,
                        ansi::RESET,
                        ansi::BOLD_GREEN,
                        text,
                        ansi::RESET
                    ));
                } else {
                    out.push_str(&format!(
                        "{}{} | {}\n",
                        " ".repeat(width - num.len()),
                        num,
                        text
                    ));
                }
            }
        }

        out
    }

    /// Renders this diagnostic to a rustc-compatible JSON string.
    pub fn render_json(&self, source: &str, filename: Option<&str>) -> String {
        #[derive(serde::Serialize)]
        struct JsonSpan<'a> {
            file_name: &'a str,
            byte_start: u32,
            byte_end: u32,
            line_start: u32,
            line_end: u32,
            column_start: u32,
            column_end: u32,
            is_primary: bool,
            label: Option<&'a str>,
            text: Vec<JsonLine<'a>>,
        }

        #[derive(serde::Serialize)]
        struct JsonLine<'a> {
            text: &'a str,
            highlight_start: u32,
            highlight_end: u32,
        }

        #[derive(serde::Serialize)]
        struct JsonChild<'a> {
            message: &'a str,
            level: &'a str,
            #[serde(skip_serializing_if = "Vec::is_empty")]
            spans: Vec<JsonReplacement<'a>>,
        }

        #[derive(serde::Serialize)]
        struct JsonReplacement<'a> {
            file_name: &'a str,
            byte_start: u32,
            byte_end: u32,
            line_start: u32,
            line_end: u32,
            column_start: u32,
            column_end: u32,
            is_primary: bool,
            suggested_replacement: &'a str,
            suggestion_applicability: &'a str,
        }

        #[derive(serde::Serialize)]
        struct JsonDiagnostic<'a> {
            message: &'a str,
            code: Option<&'a str>,
            level: &'a str,
            spans: Vec<JsonSpan<'a>>,
            children: Vec<JsonChild<'a>>,
            rendered: String,
        }

        let fname = filename.unwrap_or("<unknown>");
        let mut json_spans = Vec::new();

        for span in &self.spans {
            let (start, end) = token_span(source, span.start, span.end);
            let (l_start, c_start) = byte_to_line_col(source, start);
            let (l_end, c_end) = byte_to_line_col(source, end);
            let line_txt = source.lines().nth((l_start - 1) as usize).unwrap_or("");
            json_spans.push(JsonSpan {
                file_name: fname,
                byte_start: start,
                byte_end: end,
                line_start: l_start,
                line_end: l_end,
                column_start: c_start,
                column_end: c_end,
                is_primary: span.is_primary,
                label: span.label.as_deref(),
                text: vec![JsonLine {
                    text: line_txt,
                    highlight_start: c_start,
                    highlight_end: c_end.max(c_start + 1),
                }],
            });
        }

        let mut children = Vec::new();
        for note in &self.notes {
            children.push(JsonChild {
                message: note,
                level: "note",
                spans: Vec::new(),
            });
        }
        for help in &self.helps {
            children.push(JsonChild {
                message: help,
                level: "help",
                spans: Vec::new(),
            });
        }
        for sugg in &self.suggestions {
            children.push(JsonChild {
                message: sugg,
                level: "help",
                spans: Vec::new(),
            });
        }
        for fix in &self.fixes {
            let spans = fix
                .edits
                .iter()
                .map(|e| {
                    let (line_start, column_start) = byte_to_line_col(source, e.start);
                    let (line_end, column_end) = byte_to_line_col(source, e.end.max(e.start));
                    JsonReplacement {
                        file_name: fname,
                        byte_start: e.start,
                        byte_end: e.end.max(e.start),
                        line_start,
                        line_end,
                        column_start,
                        column_end,
                        is_primary: true,
                        suggested_replacement: &e.replacement,
                        suggestion_applicability: "MachineApplicable",
                    }
                })
                .collect();
            children.push(JsonChild {
                message: &fix.message,
                level: "help",
                spans,
            });
        }

        let rendered = self.render(source, filename, false);

        let diag = JsonDiagnostic {
            message: &self.message,
            code: self.code.as_deref(),
            level: match self.level {
                DiagnosticLevel::Error => "error",
                DiagnosticLevel::Warning => "warning",
                DiagnosticLevel::Note => "note",
                DiagnosticLevel::Help => "help",
            },
            spans: json_spans,
            children,
            rendered,
        };

        serde_json::to_string(&diag).unwrap_or_else(|_| "{}".into())
    }
}

/// Converts any standard IRIS [`Error`] into a structured [`Diagnostic`].
pub fn error_to_diagnostic(err: &Error) -> Diagnostic {
    if let Some(diag) = rich_diagnostic(err) {
        return diag;
    }
    if let Some(diag) = internal_error_diagnostic(err) {
        return diag;
    }
    let code = err.diagnostic_code();
    let mut diag = Diagnostic::error(err.message()).with_code(code);

    if let Some((start_byte, end_byte)) = extract_span(err) {
        diag.spans.push(DiagnosticSpan {
            start: start_byte,
            end: end_byte,
            label: None,
            is_primary: true,
        });
    }

    if let Some(hint) = error_hint(err) {
        diag.helps.push(hint.to_string());
    }
    if let Error::Interp(ie) = err {
        let inner = match ie {
            InterpError::Located { inner, .. } => inner.as_ref(),
            other => other,
        };
        if let InterpError::StackOverflow { limit } = inner {
            diag.notes.push(format!(
                "the interpreter stops at a call depth of {limit}; `--max-depth N` raises it"
            ));
        }
    }

    diag
}

/// Every diagnostic `err` carries: one per error when the compiler collected
/// several (one per function), otherwise one.
pub fn error_to_diagnostics(err: &Error) -> Vec<Diagnostic> {
    match err {
        Error::Lower(LowerError::Multiple(errors)) => errors
            .iter()
            .map(|e| error_to_diagnostic(&Error::Lower(e.clone_shallow())))
            .collect(),
        Error::Parse(ParseError::Multiple(errors)) => errors
            .iter()
            .map(|e| error_to_diagnostic(&Error::Parse(e.clone())))
            .collect(),
        _ => vec![error_to_diagnostic(err)],
    }
}

/// A diagnostic built where the error was found, with its own spans, notes
/// and fixes.
fn rich_diagnostic(err: &Error) -> Option<Diagnostic> {
    let (code, diag) = match err {
        Error::Lower(LowerError::Diag { code, diag }) => (*code, diag),
        Error::Lower(LowerError::Multiple(errors)) => {
            return errors
                .first()
                .map(|e| error_to_diagnostic(&Error::Lower(e.clone_shallow())))
        }
        Error::Pass(PassError::Diag { code, diag }) => (*code, diag),
        Error::Parse(ParseError::Diag { code, diag }) => (*code, diag),
        Error::Parse(ParseError::Multiple(errors)) => {
            return errors
                .first()
                .map(|e| error_to_diagnostic(&Error::Parse(e.clone())))
        }
        _ => return None,
    };
    let mut d = (**diag).clone();
    d.code = Some(code.to_string());
    Some(d)
}

/// Whether `IRIS_DEBUG` asks for compiler-internal detail in diagnostics.
fn debug_detail() -> bool {
    std::env::var_os("IRIS_DEBUG").is_some_and(|v| !v.is_empty() && v != "0")
}

/// An error that is a defect in IRIS rather than in the program -- an IR
/// invariant the compiler broke -- rendered as such. The raw detail names IR
/// values and passes, which mean nothing to the reader; it is shown only with
/// `IRIS_DEBUG=1` (which `iris bug-report` sets).
fn internal_error_diagnostic(err: &Error) -> Option<Diagnostic> {
    let (function, detail) = match err {
        Error::Pass(PassError::TypeError { func, detail })
            if func == "verify_uses_defined" || func == "verify_release_dominance" =>
        {
            // "[after inline] main: block0 ..." names the function being built.
            let name = detail
                .split_once("] ")
                .map(|(_, rest)| rest)
                .and_then(|rest| rest.split(':').next())
                .unwrap_or("")
                .to_string();
            (name, detail.clone())
        }
        Error::Pass(PassError::UseBeforeDef { func, .. })
        | Error::Pass(PassError::MultipleDefinition { func, .. })
        | Error::Pass(PassError::MissingTerminator { func, .. }) => (func.clone(), err.message()),
        Error::Interp(InterpError::UndefinedValue { .. }) => (String::new(), err.message()),
        _ => return None,
    };
    let message = if function.is_empty() || function.starts_with("__") {
        "internal compiler error: IRIS generated invalid code for this program".to_string()
    } else {
        format!("internal compiler error: IRIS generated invalid code for `{function}`")
    };
    let mut diag = Diagnostic::error(message)
        .with_code("E0900")
        .with_note("this is a bug in IRIS, not in your program")
        .with_help("`iris bug-report <file.iris>` collects what is needed to report it; rewriting the expression the compiler stopped on often avoids it meanwhile");
    if debug_detail() {
        diag = diag.with_note(format!("compiler detail: {detail}"));
    }
    Some(diag)
}

fn render_error_inner(source: &str, err: &Error, filename: Option<&str>, colored: bool) -> String {
    error_to_diagnostics(err)
        .iter()
        .map(|diag| diag.render_in(source, filename, colored))
        .collect::<Vec<_>>()
        .join("\n")
}

impl Diagnostic {
    /// [`render`](Self::render) for a compile of `main`: in the brought file
    /// the diagnostic is in, when it is not in `main` itself.
    pub fn render_in(&self, source: &str, main: Option<&str>, colored: bool) -> String {
        match main.and_then(|m| localize(self, m)) {
            Some((path, text, local)) => local.render(&text, Some(&path), colored),
            None => self.render(source, main, colored),
        }
    }

    /// [`render_json`](Self::render_json), in the file the diagnostic is in.
    pub fn render_json_in(&self, source: &str, main: Option<&str>) -> String {
        match main.and_then(|m| localize(self, m)) {
            Some((path, text, local)) => local.render_json(&text, Some(&path)),
            None => self.render_json(source, main),
        }
    }
}

/// A diagnostic whose primary span is in a file `main` brought, rather than
/// in `main` itself: that file's path as messages show it, its text, and the
/// diagnostic with its spans made relative to that text. Spans and fixes in
/// other files are dropped.
pub fn localize(diag: &Diagnostic, main: &str) -> Option<(String, String, Diagnostic)> {
    let primary = diag
        .spans
        .iter()
        .find(|s| s.is_primary)
        .or(diag.spans.first())?;
    let map = crate::compiler::source_map_for(std::path::Path::new(main))?;
    let (file, _) = map.lookup(primary.start)?;
    if file.base == 0 {
        return None;
    }
    let (base, end) = (file.base, file.base + file.text.len() as u32);
    let inside = |start: u32, stop: u32| start >= base && stop <= end + 1;
    let mut local = diag.clone();
    local.spans.retain(|s| inside(s.start, s.end));
    for span in &mut local.spans {
        span.start -= base;
        span.end -= base;
    }
    local
        .fixes
        .retain(|fix| fix.edits.iter().all(|e| inside(e.start, e.end)));
    for fix in &mut local.fixes {
        for edit in &mut fix.edits {
            edit.start -= base;
            edit.end -= base;
        }
    }
    Some((
        crate::compiler::display_path(&file.path),
        file.text.clone(),
        local,
    ))
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ParseError;
    use crate::parser::lexer::{BytePos, Span};

    // -- byte_to_line_col -----------------------------------------------------

    #[test]
    fn byte_to_line_col_start_of_file() {
        assert_eq!(byte_to_line_col("hello", 0), (1, 1));
    }

    #[test]
    fn byte_to_line_col_first_line() {
        assert_eq!(byte_to_line_col("hello", 3), (1, 4));
    }

    #[test]
    fn byte_to_line_col_second_line() {
        assert_eq!(byte_to_line_col("abc\ndef", 4), (2, 1));
    }

    #[test]
    fn byte_to_line_col_second_line_middle() {
        assert_eq!(byte_to_line_col("abc\ndef", 5), (2, 2));
    }

    #[test]
    fn byte_to_line_col_third_line() {
        assert_eq!(byte_to_line_col("a\nb\nc", 4), (3, 1));
    }

    #[test]
    fn byte_to_line_col_empty_string() {
        assert_eq!(byte_to_line_col("", 0), (1, 1));
    }

    #[test]
    fn byte_to_line_col_newline_only() {
        assert_eq!(byte_to_line_col("\n", 1), (2, 1));
    }

    // -- span_to_line_col -----------------------------------------------------

    #[test]
    fn span_to_line_col_delegates() {
        assert_eq!(
            span_to_line_col("abc\ndef", 4),
            byte_to_line_col("abc\ndef", 4)
        );
    }

    // -- error_byte_offset ----------------------------------------------------

    #[test]
    fn error_byte_offset_parse_unexpected_char() {
        let err = Error::Parse(ParseError::UnexpectedChar { ch: '@', pos: 42 });
        assert_eq!(error_byte_offset(&err), Some(42));
    }

    #[test]
    fn error_byte_offset_parse_eof() {
        let err = Error::Parse(ParseError::UnexpectedEof {
            context: "test".into(),
        });
        assert_eq!(error_byte_offset(&err), None);
    }

    #[test]
    fn error_byte_offset_lower_undefined() {
        let span = Span {
            start: BytePos(10),
            end: BytePos(15),
        };
        let err = Error::Lower(crate::error::LowerError::UndefinedVariable {
            name: "x".into(),
            span,
            suggestion: None,
        });
        assert_eq!(error_byte_offset(&err), Some(10));
    }

    #[test]
    fn error_byte_offset_pass_none() {
        let err = Error::Pass(crate::error::PassError::UseBeforeDef {
            func: "f".into(),
            value: "v".into(),
        });
        assert_eq!(error_byte_offset(&err), None);
    }

    #[test]
    fn error_byte_offset_interp_none() {
        let err = Error::Interp(crate::error::InterpError::DivisionByZero);
        assert_eq!(error_byte_offset(&err), None);
    }

    // -- render_error ---------------------------------------------------------

    #[test]
    fn render_error_with_location() {
        let src = "def main() {\n  @invalid\n}";
        let err = Error::Parse(ParseError::UnexpectedChar { ch: '@', pos: 15 });
        let rendered = render_error(src, &err);
        assert!(rendered.contains("error[E0001]"));
        assert!(rendered.contains("-->"));
        assert!(rendered.contains("^"));
    }

    #[test]
    fn render_error_without_location() {
        let src = "def main() {}";
        let err = Error::Parse(ParseError::UnexpectedEof {
            context: "test".into(),
        });
        let rendered = render_error(src, &err);
        assert!(rendered.contains("error[E0006]"));
        assert!(!rendered.contains("-->"));
    }

    #[test]
    fn render_error_line_number_correct() {
        let src = "line1\nline2\nline3";
        // byte 12 = start of "line3"
        let err = Error::Parse(ParseError::UnexpectedChar { ch: 'x', pos: 12 });
        let rendered = render_error(src, &err);
        assert!(rendered.contains("3 |"));
    }

    // -- error codes in output ------------------------------------------------

    #[test]
    fn render_error_includes_error_code() {
        let src = "@bad";
        let err = Error::Parse(ParseError::UnexpectedChar { ch: '@', pos: 0 });
        let rendered = render_error(src, &err);
        assert!(
            rendered.contains("error[E0001]"),
            "expected error code in output:\n{}",
            rendered
        );
    }

    // -- span underline -------------------------------------------------------

    #[test]
    fn render_error_underlines_span() {
        let src = "val x = badtoken";
        let span = Span {
            start: BytePos(8),
            end: BytePos(16), // "badtoken" = 8 chars
        };
        let err = Error::Parse(ParseError::InvalidLiteral {
            text: "badtoken".into(),
            span,
        });
        let rendered = render_error(src, &err);
        // Should contain "^^^^^^^^" (8 carets)
        assert!(
            rendered.contains("^^^^^^^^"),
            "expected multi-char underline in:\n{}",
            rendered
        );
    }

    #[test]
    fn render_error_single_char_underline() {
        let src = "@x";
        let err = Error::Parse(ParseError::UnexpectedChar { ch: '@', pos: 0 });
        let rendered = render_error(src, &err);
        assert!(
            rendered.contains("^"),
            "expected single caret in:\n{}",
            rendered
        );
    }

    // -- filename in location -------------------------------------------------

    #[test]
    fn render_error_with_filename() {
        let src = "@bad";
        let err = Error::Parse(ParseError::UnexpectedChar { ch: '@', pos: 0 });
        let rendered = render_error_with_file(src, &err, "test.iris");
        assert!(
            rendered.contains("test.iris:1:1"),
            "expected filename in location:\n{}",
            rendered
        );
    }

    // -- help hints -----------------------------------------------------------

    #[test]
    fn render_error_help_hint_at_sign() {
        let src = "@bad";
        let err = Error::Parse(ParseError::UnexpectedChar { ch: '@', pos: 0 });
        let rendered = render_error(src, &err);
        assert!(
            rendered.contains("= help:"),
            "expected help note for '@':\n{}",
            rendered
        );
        assert!(rendered.contains("decorators"));
    }

    #[test]
    fn render_error_help_hint_hash() {
        let src = "# comment";
        let err = Error::Parse(ParseError::UnexpectedChar { ch: '#', pos: 0 });
        let rendered = render_error(src, &err);
        assert!(rendered.contains("= help:"));
        assert!(rendered.contains("//"));
    }

    #[test]
    fn render_error_help_hint_unterminated_string() {
        let src = "\"hello";
        let err = Error::Parse(ParseError::UnterminatedString { pos: 0 });
        let rendered = render_error(src, &err);
        assert!(rendered.contains("= help:"));
    }

    // -- colored output -------------------------------------------------------

    #[test]
    fn render_error_colored_contains_ansi() {
        let src = "@bad";
        let err = Error::Parse(ParseError::UnexpectedChar { ch: '@', pos: 0 });
        let rendered = render_error_colored(src, &err);
        assert!(
            rendered.contains("\x1b["),
            "expected ANSI escape codes in colored output:\n{}",
            rendered
        );
    }

    #[test]
    fn render_error_colored_with_filename() {
        let src = "@bad";
        let err = Error::Parse(ParseError::UnexpectedChar { ch: '@', pos: 0 });
        let rendered = render_error_colored_with_file(src, &err, "main.iris");
        assert!(rendered.contains("main.iris:1:1"));
        assert!(rendered.contains("\x1b["));
    }

    // -- extract_span ---------------------------------------------------------

    #[test]
    fn extract_span_parse_char() {
        let err = Error::Parse(ParseError::UnexpectedChar { ch: 'x', pos: 5 });
        assert_eq!(extract_span(&err), Some((5, 6)));
    }

    #[test]
    fn extract_span_lower_variable() {
        let span = Span {
            start: BytePos(10),
            end: BytePos(15),
        };
        let err = Error::Lower(LowerError::UndefinedVariable {
            name: "foo".into(),
            span,
            suggestion: None,
        });
        assert_eq!(extract_span(&err), Some((10, 15)));
    }

    #[test]
    fn extract_span_pass_returns_none() {
        let err = Error::Pass(PassError::UseBeforeDef {
            func: "f".into(),
            value: "v".into(),
        });
        assert_eq!(extract_span(&err), None);
    }

    // -- error_hint -----------------------------------------------------------

    #[test]
    fn hint_division_by_zero() {
        let err = Error::Interp(InterpError::DivisionByZero);
        assert!(error_hint(&err).is_some());
    }

    #[test]
    fn hint_index_out_of_bounds() {
        let err = Error::Interp(InterpError::IndexOutOfBounds { idx: 5, len: 3 });
        assert!(error_hint(&err).is_some());
    }

    #[test]
    fn hint_missing_terminator() {
        let err = Error::Pass(PassError::MissingTerminator {
            func: "main".into(),
            block: "bb0".into(),
        });
        assert!(error_hint(&err).is_some());
    }

    #[test]
    fn hint_type_mismatch() {
        let span = Span {
            start: BytePos(0),
            end: BytePos(1),
        };
        let err = Error::Lower(LowerError::TypeMismatch {
            expected: "i64".into(),
            found: "str".into(),
            span,
        });
        assert!(error_hint(&err).is_some());
    }

    #[test]
    fn hint_none_for_generic_error() {
        let err = Error::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "test"));
        assert!(error_hint(&err).is_none());
    }

    // -- Multi-span and JSON diagnostic tests ---------------------------------

    #[test]
    fn multi_span_diagnostic_renders_secondary_underlines_and_labels() {
        let source = "val mut x = 10\nval r = &x\nx = 20\n";
        let diag = Diagnostic::error("cannot mutate `x` because it is currently borrowed")
            .with_code("E0382")
            .with_secondary_span(23, 25, Some("immutable borrow occurs here".into()))
            .with_primary_span(26, 27, Some("conflicting mutation occurs here".into()))
            .with_note("borrows must end before mutation can occur");

        let rendered = diag.render(source, Some("main.iris"), false);
        assert!(
            rendered.contains("error[E0382]: cannot mutate `x` because it is currently borrowed")
        );
        assert!(rendered.contains("--> main.iris:3:1"));
        assert!(rendered.contains("-- immutable borrow occurs here"));
        assert!(rendered.contains("^ conflicting mutation occurs here"));
        assert!(rendered.contains("= note: borrows must end before mutation can occur"));
    }

    #[test]
    fn json_diagnostic_produces_valid_rustc_schema() {
        let source = "val x = bad\n";
        let diag = Diagnostic::error("cannot find `bad` in scope")
            .with_code("E0425")
            .with_primary_span(8, 11, Some("not found in this scope".into()))
            .with_help("check spelling or declare `bad` before use");

        let json_str = diag.render_json(source, Some("test.iris"));
        let parsed: serde_json::Value =
            serde_json::from_str(&json_str).expect("must be valid JSON");

        assert_eq!(parsed["message"], "cannot find `bad` in scope");
        assert_eq!(parsed["code"], "E0425");
        assert_eq!(parsed["level"], "error");
        assert_eq!(parsed["spans"][0]["file_name"], "test.iris");
        assert_eq!(parsed["spans"][0]["line_start"], 1);
        assert_eq!(parsed["spans"][0]["column_start"], 9);
        assert_eq!(parsed["spans"][0]["is_primary"], true);
        assert_eq!(parsed["spans"][0]["label"], "not found in this scope");
        assert_eq!(parsed["children"][0]["level"], "help");
        assert!(parsed["rendered"]
            .as_str()
            .unwrap()
            .contains("error[E0425]"));
    }
}
