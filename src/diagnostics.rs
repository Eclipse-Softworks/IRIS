//! Source diagnostics: byte-to-line/col mapping and human-readable error rendering.
//!
//! Provides both plain-text and ANSI-colored rendering of compiler errors
//! in a rustc-style format with source excerpts, span underlines, error
//! codes, and optional help/hint notes.

use crate::error::{Error, InterpError, LowerError, ParseError, PassError};

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
        },
        Error::Interp(InterpError::Located { byte, byte_end, .. }) => {
            Some((*byte, byte_end.unwrap_or(*byte + 1)))
        }
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
            PassError::UnresolvedInfer { .. } => {
                Some("add a type annotation — the compiler cannot infer the type automatically")
            }
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
            match inner {
                InterpError::DivisionByZero => {
                    Some("check that the divisor is not zero before dividing")
                }
                InterpError::IndexOutOfBounds { .. } => {
                    Some("use 'len(list)' to check bounds before indexing")
                }
                InterpError::TypeError { .. } => {
                    Some("check that operand types match — use `to_i64()`, `to_f64()`, or `to_str()` for conversions")
                }
                InterpError::Panic { .. } => {
                    Some("use `try` blocks to catch panics, or check conditions before the panicking operation")
                }
                _ => None,
            }
        }
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

            // Sort spans by byte offset
            let mut sorted_spans = self.spans.clone();
            sorted_spans.sort_by_key(|s| s.start);

            for span in &sorted_spans {
                let (line, col) = byte_to_line_col(source, span.start);
                let source_line = source.lines().nth((line - 1) as usize).unwrap_or("");

                let span_len = (span.end.saturating_sub(span.start)).max(1) as usize;
                let max_underline = source_line
                    .len()
                    .saturating_sub((col as usize).saturating_sub(1));
                let underline_len = span_len.min(max_underline).max(1);

                let indent = (col as usize).saturating_sub(1);
                let ch = if span.is_primary { "^" } else { "-" };
                let pointer_chars = ch.repeat(underline_len);

                let label_text = if let Some(ref l) = span.label {
                    format!(" {}", l)
                } else {
                    String::new()
                };

                let line_str = line.to_string();
                let pad = " ".repeat(gutter_width.saturating_sub(line_str.len()));

                if colored {
                    out.push_str(&format!("{} {}|{}\n", gutter, ansi::BOLD_BLUE, ansi::RESET));
                    out.push_str(&format!(
                        "{}{}{} |{} {}\n",
                        pad,
                        ansi::BOLD_BLUE,
                        line_str,
                        ansi::RESET,
                        source_line
                    ));
                    let color = if span.is_primary {
                        ansi::BOLD_RED
                    } else {
                        ansi::BOLD_BLUE
                    };
                    out.push_str(&format!(
                        "{} {}|{} {}{}{}{}\n",
                        gutter,
                        ansi::BOLD_BLUE,
                        ansi::RESET,
                        " ".repeat(indent),
                        color,
                        pointer_chars,
                        label_text,
                    ));
                } else {
                    out.push_str(&format!("{}  |\n", gutter));
                    out.push_str(&format!("{}{} | {}\n", pad, line_str, source_line));
                    out.push_str(&format!(
                        "{}  | {}{}{}\n",
                        gutter,
                        " ".repeat(indent),
                        pointer_chars,
                        label_text
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
            let (l_start, c_start) = byte_to_line_col(source, span.start);
            let (l_end, c_end) = byte_to_line_col(source, span.end);
            let line_txt = source.lines().nth((l_start - 1) as usize).unwrap_or("");
            json_spans.push(JsonSpan {
                file_name: fname,
                byte_start: span.start,
                byte_end: span.end,
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
            });
        }
        for help in &self.helps {
            children.push(JsonChild {
                message: help,
                level: "help",
            });
        }
        for sugg in &self.suggestions {
            children.push(JsonChild {
                message: sugg,
                level: "help",
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
    let code = err.diagnostic_code();
    let mut diag = Diagnostic::error(format!("{}", err)).with_code(code);

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

    diag
}

fn render_error_inner(source: &str, err: &Error, filename: Option<&str>, colored: bool) -> String {
    let diag = error_to_diagnostic(err);
    diag.render(source, filename, colored)
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
    fn hint_unresolved_infer() {
        let err = Error::Pass(PassError::UnresolvedInfer {
            func: "main".into(),
        });
        let h = error_hint(&err).unwrap();
        assert!(h.contains("type annotation"));
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
