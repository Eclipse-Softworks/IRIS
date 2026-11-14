//! `src/preprocessor.rs` — Conditional compilation and object-like macros.
//!
//! Scans raw source text for `#`-directives at the start of a line and
//! either keeps or removes blocks of code based on conditions.
//!
//! Supported directives:
//!   #define NAME            -- define a symbol
//!   #define NAME VALUE      -- define a macro: later uses of NAME become VALUE
//!   #undef NAME             -- undefine a symbol or macro
//!   #if EXPR                -- conditional block start
//!   #elif EXPR              -- else-if branch
//!   #else                   -- else branch
//!   #endif                  -- conditional block end
//!   #ifdef NAME             -- if NAME is defined
//!   #ifndef NAME            -- if NAME is not defined
//!   #error "message"        -- emit compile error
//!   #warning "message"      -- emit compile warning
//!
//! `#if` expressions: `defined(NAME)` (or `defined NAME`), `NAME`, integer and
//! string literals, `==` `!=` `<` `<=` `>` `>=`, `!`, `&&`, `||` and
//! parentheses, with C's precedence. As in C, an undefined name in a
//! comparison is `0`; a bare name is true when it is defined with no value or
//! with a value other than `0`, `false` or `""`. `<` and friends compare
//! integers only.
//!
//! A macro's value replaces each later use of its name -- an identifier, not
//! text inside a string or comment -- until `#undef` or a new `#define`, as in
//! C. The replacement is made on tokens, so every token keeps a position in
//! the source as written: the value's tokens take the position of the name.
//! A symbol defined without a value is never substituted (C would erase it).
//!
//! Predefined, from the build (`--target`, `-g`, `-D NAME[=VALUE]`), not the
//! machine running the compiler:
//!   WINDOWS, LINUX, MACOS, WASM, EMBEDDED   -- the target OS (EMBEDDED: none)
//!   TARGET_WINDOWS, TARGET_LINUX, TARGET_MACOS
//!   X86_64, AARCH64, ARM, RISCV32, RISCV64, AVR, XTENSA, WASM32 -- the target architecture
//!   TARGET_OS, TARGET_ARCH                  -- the same, as strings ("linux", "x86_64")
//!   DEBUG (with -g) or RELEASE              -- the build profile
//!   IRIS_VERSION_MAJOR, IRIS_VERSION_MINOR, IRIS_VERSION_PATCH

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use crate::error::ParseError;
use crate::parser::lexer::{Lexer, Spanned, Token};

/// The build the predefined symbols describe, set by the driver.
#[derive(Clone, Debug, Default)]
struct Config {
    target: Option<String>,
    debug: bool,
    defines: Vec<String>,
}

static CONFIG: Mutex<Option<Config>> = Mutex::new(None);

/// Set the build every later `Preprocessor::new()` describes: the target
/// (preset or triple; `None` is this machine), whether it is a debug build
/// (`-g`), and the `-D NAME[=VALUE]` definitions.
pub fn configure(target: Option<&str>, debug: bool, defines: &[String]) {
    *CONFIG.lock().unwrap_or_else(|e| e.into_inner()) = Some(Config {
        target: target.map(str::to_owned),
        debug,
        defines: defines.to_vec(),
    });
}

/// Preprocessor state.
pub struct Preprocessor {
    /// Set of defined symbols (case-sensitive).
    pub defines: HashSet<String>,
    /// The values of those defined with one, as written (`"alpha"`, `10`).
    pub values: HashMap<String, String>,
}

/// A macro's value and the part of the source it applies to.
#[derive(Clone, Debug)]
struct Macro {
    name: String,
    tokens: Vec<Token>,
    /// Byte range `[from, to)` of the source where uses are replaced.
    from: u32,
    to: u32,
}

/// Preprocessed source: the text the lexer reads, with every directive and
/// inactive line blanked in place, and the macros to substitute.
pub struct Processed {
    pub text: String,
    macros: Vec<Macro>,
}

impl Processed {
    /// The text's tokens, with macro uses replaced by their values.
    pub fn tokenize(&self) -> Result<Vec<Spanned<Token>>, ParseError> {
        let tokens = Lexer::new(&self.text).tokenize()?;
        if self.macros.is_empty() {
            return Ok(tokens);
        }
        let mut out = Vec::with_capacity(tokens.len());
        let mut expanding = Vec::new();
        for tok in tokens {
            self.emit(tok, &mut expanding, &mut out);
        }
        Ok(out)
    }

    /// The macro `name` names at byte `at`, if any.
    fn active(&self, name: &str, at: u32) -> Option<&Macro> {
        self.macros
            .iter()
            .find(|m| m.name == name && m.from <= at && at < m.to)
    }

    /// Push `tok`, or, when it names a macro not already being expanded
    /// (C's rule, which ends self-reference), its value's tokens, expanded
    /// in turn, all at `tok`'s position.
    fn emit(
        &self,
        tok: Spanned<Token>,
        expanding: &mut Vec<String>,
        out: &mut Vec<Spanned<Token>>,
    ) {
        if let Token::Ident(name) = &tok.node {
            if !expanding.contains(name) {
                if let Some(m) = self.active(name, tok.span.start.0) {
                    expanding.push(name.clone());
                    for (i, node) in m.tokens.iter().enumerate() {
                        let inner = Spanned {
                            node: node.clone(),
                            span: tok.span,
                            newline_before: i == 0 && tok.newline_before,
                        };
                        self.emit(inner, expanding, out);
                    }
                    expanding.pop();
                    return;
                }
            }
        }
        out.push(tok);
    }
}

/// One open `#if`/`#ifdef`/`#ifndef` group.
struct Frame {
    /// Whether the enclosing code is kept.
    parent_active: bool,
    /// Whether one of the group's branches has been kept already.
    taken: bool,
    /// Whether the current branch is kept.
    active: bool,
    else_seen: bool,
}

impl Preprocessor {
    /// The predefined symbols of the configured build (see [`configure`]);
    /// this machine, not a debug build, by default.
    pub fn new() -> Self {
        let config = CONFIG
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_default();
        let triple = crate::codegen::build::resolve_target_triple(config.target.as_deref());
        let mut pp = Self::for_target(&triple, config.debug);
        for spec in &config.defines {
            pp.add_define(spec);
        }
        pp
    }

    /// The predefined symbols of a build for `triple`.
    pub fn for_target(triple: &str, debug: bool) -> Self {
        let mut pp = Preprocessor {
            defines: HashSet::new(),
            values: HashMap::new(),
        };
        let t = triple.to_ascii_lowercase();
        let arch = t.split('-').next().unwrap_or("");
        let wasm = arch.starts_with("wasm");
        let os = if t.contains("windows") {
            "windows"
        } else if t.contains("linux") {
            "linux"
        } else if t.contains("darwin") || t.contains("apple") || t.contains("macos") {
            "macos"
        } else if wasm && t.contains("wasi") {
            "wasi"
        } else {
            "none"
        };
        match os {
            "windows" => pp.define_all(&["WINDOWS", "TARGET_WINDOWS"]),
            "linux" => pp.define_all(&["LINUX", "TARGET_LINUX"]),
            "macos" => pp.define_all(&["MACOS", "TARGET_MACOS"]),
            _ => {}
        }
        if wasm {
            pp.define_all(&["WASM"]);
        } else if os == "none" {
            pp.define_all(&["EMBEDDED"]);
        }
        let arch_name = if arch == "x86_64" || arch == "amd64" {
            "x86_64"
        } else if arch == "aarch64" || arch == "arm64" {
            "aarch64"
        } else if arch.starts_with("arm") || arch.starts_with("thumb") {
            "arm"
        } else if arch.starts_with("riscv64") {
            "riscv64"
        } else if arch.starts_with("riscv32") {
            "riscv32"
        } else if arch == "avr" {
            "avr"
        } else if arch.starts_with("xtensa") {
            "xtensa"
        } else if arch == "wasm32" {
            "wasm32"
        } else {
            arch
        };
        if !arch_name.is_empty() {
            pp.define_all(&[&arch_name.to_ascii_uppercase()]);
        }
        pp.set_value("TARGET_OS", &format!("\"{os}\""));
        pp.set_value("TARGET_ARCH", &format!("\"{arch_name}\""));
        pp.define_all(&[if debug { "DEBUG" } else { "RELEASE" }]);
        pp.set_value("IRIS_VERSION_MAJOR", env!("CARGO_PKG_VERSION_MAJOR"));
        pp.set_value("IRIS_VERSION_MINOR", env!("CARGO_PKG_VERSION_MINOR"));
        pp.set_value("IRIS_VERSION_PATCH", env!("CARGO_PKG_VERSION_PATCH"));
        pp
    }

    fn define_all(&mut self, names: &[&str]) {
        for name in names {
            self.defines.insert((*name).to_owned());
        }
    }

    fn set_value(&mut self, name: &str, value: &str) {
        self.defines.insert(name.to_owned());
        self.values.insert(name.to_owned(), value.to_owned());
    }

    /// Add a define from the command line (e.g. `-D NAME` or `-D NAME=VALUE`).
    pub fn add_define(&mut self, spec: &str) {
        if let Some(idx) = spec.find('=') {
            self.set_value(spec[..idx].trim(), spec[idx + 1..].trim());
        } else {
            self.defines.insert(spec.trim().to_string());
        }
    }

    /// Process source text, returning the preprocessed text.
    /// `filename` is used for error reporting.
    pub fn process(&self, source: &str, filename: &str) -> Result<String, String> {
        self.process_full(source, filename).map(|p| p.text)
    }

    /// Process source text: the preprocessed text and its macros, ready for
    /// [`Processed::tokenize`].
    pub fn process_full(&self, source: &str, filename: &str) -> Result<Processed, String> {
        let mut defines = self.defines.clone();
        let mut values = self.values.clone();
        let mut output = String::new();
        let mut stack: Vec<Frame> = Vec::new();
        let mut macros: Vec<Macro> = Vec::new();
        for (name, value) in &self.values {
            let tokens = self.macro_tokens(name, value, filename, 0)?;
            if !tokens.is_empty() {
                macros.push(Macro {
                    name: name.clone(),
                    tokens,
                    from: 0,
                    to: u32::MAX,
                });
            }
        }
        let mut offset: u32 = 0;

        // Split with `split_inclusive` on the newline rather than `lines()`.
        //
        // `lines()` strips the carriage return of a CRLF terminator, so every
        // kept line came out one byte shorter than the input. The lexer then
        // computed spans against that shortened text while diagnostics rendered
        // against the caller's original source, and every reported position
        // drifted by one byte per preceding line — on Windows, in every file,
        // growing with file length. It is why an assertion failure named the
        // previous statement. See known-issues #20.
        //
        // Keeping the terminator makes the preprocessor byte-faithful for every
        // line it passes through, which is what span fidelity requires of it.
        for (line_no, raw_with_term) in source.split_inclusive('\n').enumerate() {
            let line_start = offset;
            offset += raw_with_term.len() as u32;
            let raw_line = raw_with_term.strip_suffix('\n').unwrap_or(raw_with_term);
            let had_newline = raw_with_term.ends_with('\n');
            let line = raw_line.trim_start();
            let active = stack.last().map_or(true, |f| f.active);
            let err = |msg: &str| self.error(filename, line_no + 1, msg);

            let emitted = 'line: {
                let Some(directive_text) = line.strip_prefix('#') else {
                    break 'line active;
                };
                let directive_end = directive_text
                    .find([' ', '\t', '\r', '\n', '('])
                    .unwrap_or(directive_text.len());
                let directive = directive_text[..directive_end].trim().to_uppercase();
                let rest = strip_comment(directive_text[directive_end..].trim());

                match directive.as_str() {
                    "DEFINE" => {
                        if active {
                            let mut parts = rest.splitn(2, char::is_whitespace);
                            let name = parts.next().unwrap_or("").trim();
                            let value = parts.next().unwrap_or("").trim();
                            if !is_identifier(name) {
                                return Err(err(&format!("`#define {rest}`: expected a name")));
                            }
                            close(&mut macros, name, line_start);
                            defines.insert(name.to_owned());
                            if value.is_empty() {
                                values.remove(name);
                            } else {
                                values.insert(name.to_owned(), value.to_owned());
                                let tokens =
                                    self.macro_tokens(name, value, filename, line_no + 1)?;
                                macros.push(Macro {
                                    name: name.to_owned(),
                                    tokens,
                                    from: offset,
                                    to: u32::MAX,
                                });
                            }
                        }
                        break 'line false;
                    }
                    "UNDEF" => {
                        if active {
                            let name = rest.trim();
                            defines.remove(name);
                            values.remove(name);
                            close(&mut macros, name, line_start);
                        }
                        break 'line false;
                    }
                    "IFDEF" | "IFNDEF" | "IF" => {
                        let cond = if !active {
                            false
                        } else if directive == "IF" {
                            eval(rest, &defines, &values).map_err(|e| err(&e))?
                        } else {
                            let name = rest.trim();
                            if !is_identifier(name) {
                                return Err(err(&format!(
                                    "`#{} {rest}`: expected a name",
                                    directive.to_lowercase()
                                )));
                            }
                            defines.contains(name) == (directive == "IFDEF")
                        };
                        stack.push(Frame {
                            parent_active: active,
                            taken: cond,
                            active: cond,
                            else_seen: false,
                        });
                        break 'line false;
                    }
                    "ELIF" => {
                        let Some(top) = stack.last_mut() else {
                            return Err(err("#elif without matching #if"));
                        };
                        if top.else_seen {
                            return Err(err("#elif after #else"));
                        }
                        if top.parent_active && !top.taken {
                            let cond = eval(rest, &defines, &values).map_err(|e| err(&e))?;
                            top.active = cond;
                            top.taken = cond;
                        } else {
                            top.active = false;
                        }
                        break 'line false;
                    }
                    "ELSE" => {
                        let Some(top) = stack.last_mut() else {
                            return Err(err("#else without matching #if"));
                        };
                        if top.else_seen {
                            return Err(err("a second #else in one #if"));
                        }
                        top.else_seen = true;
                        top.active = top.parent_active && !top.taken;
                        top.taken = true;
                        break 'line false;
                    }
                    "ENDIF" => {
                        if stack.pop().is_none() {
                            return Err(err("#endif without matching #if"));
                        }
                        break 'line false;
                    }
                    "ERROR" => {
                        if active {
                            return Err(err(rest.trim().trim_matches('"')));
                        }
                        break 'line false;
                    }
                    "WARNING" => {
                        if active {
                            let msg = rest.trim().trim_matches('"');
                            eprintln!("warning: {}:{}: {}", filename, line_no + 1, msg);
                        }
                        break 'line false;
                    }
                    // Not a directive: kept like any other line.
                    _ => active,
                }
            };
            if emitted {
                output.push_str(raw_line);
            } else {
                // A dropped line (a directive, or a line in an inactive branch)
                // becomes blanks of the same byte length, keeping its newline, so
                // every later line number and byte offset still matches the
                // caller's source. Debug line tables and diagnostics rely on it.
                output.extend(std::iter::repeat(' ').take(raw_line.len()));
            }
            if had_newline {
                output.push('\n');
            }
        }

        if !stack.is_empty() {
            return Err(self.error(
                filename,
                source.lines().count(),
                "unterminated #if/#ifdef/#ifndef (missing #endif)",
            ));
        }

        Ok(Processed {
            text: output,
            macros,
        })
    }

    /// A macro value's tokens; a value that is not valid IRIS is an error at
    /// its `#define` (line 0: a predefined or `-D` value).
    fn macro_tokens(
        &self,
        name: &str,
        value: &str,
        filename: &str,
        line: usize,
    ) -> Result<Vec<Token>, String> {
        let tokens = Lexer::new(value).tokenize().map_err(|e| {
            let msg = format!("the value of `{name}`, `{value}`, is not valid IRIS: {e}");
            if line == 0 {
                format!("preprocessor error: {msg}")
            } else {
                self.error(filename, line, &msg)
            }
        })?;
        Ok(tokens
            .into_iter()
            .map(|t| t.node)
            .filter(|t| *t != Token::Eof)
            .collect())
    }

    fn error(&self, filename: &str, line: usize, msg: &str) -> String {
        format!("preprocessor error in {}:{}: {}", filename, line, msg)
    }
}

impl Default for Preprocessor {
    fn default() -> Self {
        Self::new()
    }
}

/// End the open macro named `name` at `at`.
fn close(macros: &mut [Macro], name: &str, at: u32) {
    for m in macros.iter_mut() {
        if m.name == name && m.to == u32::MAX {
            m.to = at;
        }
    }
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A directive's text without a trailing `//` comment (outside strings).
fn strip_comment(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut in_str = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if in_str => i += 1,
            b'"' => in_str = !in_str,
            b'/' if !in_str && bytes.get(i + 1) == Some(&b'/') => return s[..i].trim_end(),
            _ => {}
        }
        i += 1;
    }
    s
}

// ── #if expressions ─────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    Int(i64),
    LParen,
    RParen,
    Not,
    And,
    Or,
    Cmp(&'static str),
}

/// What an operand stands for.
#[derive(Clone, Debug)]
enum Val {
    Undefined,
    /// Defined without a value.
    Defined,
    Int(i64),
    Str(String),
}

impl Val {
    /// A value as written: `"alpha"` is a string, `10` an integer, anything
    /// else (`fast`) a string of itself.
    fn parse(text: &str) -> Val {
        let t = text.trim();
        if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
            Val::Str(t[1..t.len() - 1].replace("\\\"", "\""))
        } else if let Ok(n) = t.parse::<i64>() {
            Val::Int(n)
        } else {
            Val::Str(t.to_owned())
        }
    }

    fn truthy(&self) -> bool {
        match self {
            Val::Undefined => false,
            Val::Defined => true,
            Val::Int(n) => *n != 0,
            Val::Str(s) => !s.is_empty() && s != "false" && s != "0",
        }
    }
}

fn lex_expr(s: &str) -> Result<Vec<Tok>, String> {
    let chars: Vec<char> = s.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match c {
            ' ' | '\t' | '\r' | '\n' => i += 1,
            '(' => {
                toks.push(Tok::LParen);
                i += 1
            }
            ')' => {
                toks.push(Tok::RParen);
                i += 1
            }
            '&' if next == Some('&') => {
                toks.push(Tok::And);
                i += 2
            }
            '|' if next == Some('|') => {
                toks.push(Tok::Or);
                i += 2
            }
            '=' if next == Some('=') => {
                toks.push(Tok::Cmp("=="));
                i += 2
            }
            '!' if next == Some('=') => {
                toks.push(Tok::Cmp("!="));
                i += 2
            }
            '<' | '>' if next == Some('=') => {
                toks.push(Tok::Cmp(if c == '<' { "<=" } else { ">=" }));
                i += 2
            }
            '<' => {
                toks.push(Tok::Cmp("<"));
                i += 1
            }
            '>' => {
                toks.push(Tok::Cmp(">"));
                i += 1
            }
            '!' => {
                toks.push(Tok::Not);
                i += 1
            }
            '"' => {
                let mut text = String::new();
                i += 1;
                loop {
                    match chars.get(i) {
                        None => return Err("unterminated string".into()),
                        Some('"') => break,
                        Some('\\') if chars.get(i + 1).is_some() => {
                            text.push(chars[i + 1]);
                            i += 2;
                        }
                        Some(ch) => {
                            text.push(*ch);
                            i += 1;
                        }
                    }
                }
                i += 1;
                toks.push(Tok::Str(text));
            }
            c if c.is_ascii_digit() || (c == '-' && next.is_some_and(|d| d.is_ascii_digit())) => {
                let start = i;
                i += 1;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                let text: String = chars[start..i].iter().collect();
                toks.push(Tok::Int(
                    text.parse()
                        .map_err(|_| format!("`{text}` is not an integer"))?,
                ));
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                toks.push(Tok::Ident(chars[start..i].iter().collect()));
            }
            other => return Err(format!("unexpected `{other}` in #if expression")),
        }
    }
    Ok(toks)
}

/// Evaluate a `#if`/`#elif` expression.
fn eval(
    expr: &str,
    defines: &HashSet<String>,
    values: &HashMap<String, String>,
) -> Result<bool, String> {
    let toks = lex_expr(expr)?;
    if toks.is_empty() {
        return Err("#if needs an expression".into());
    }
    let mut p = ExprParser {
        toks,
        pos: 0,
        defines,
        values,
    };
    let v = p.or()?;
    if p.pos != p.toks.len() {
        return Err(format!(
            "unexpected `{}` in #if expression",
            describe(&p.toks[p.pos])
        ));
    }
    Ok(v)
}

fn describe(t: &Tok) -> String {
    match t {
        Tok::Ident(s) => s.clone(),
        Tok::Str(s) => format!("\"{s}\""),
        Tok::Int(n) => n.to_string(),
        Tok::LParen => "(".into(),
        Tok::RParen => ")".into(),
        Tok::Not => "!".into(),
        Tok::And => "&&".into(),
        Tok::Or => "||".into(),
        Tok::Cmp(op) => (*op).into(),
    }
}

struct ExprParser<'a> {
    toks: Vec<Tok>,
    pos: usize,
    defines: &'a HashSet<String>,
    values: &'a HashMap<String, String>,
}

impl ExprParser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    // Both sides are evaluated, so an error anywhere in the expression is
    // reported whatever the left side's value.
    fn or(&mut self) -> Result<bool, String> {
        let mut v = self.and()?;
        while self.peek() == Some(&Tok::Or) {
            self.pos += 1;
            let r = self.and()?;
            v = v || r;
        }
        Ok(v)
    }

    fn and(&mut self) -> Result<bool, String> {
        let mut v = self.unary()?;
        while self.peek() == Some(&Tok::And) {
            self.pos += 1;
            let r = self.unary()?;
            v = v && r;
        }
        Ok(v)
    }

    fn unary(&mut self) -> Result<bool, String> {
        if self.peek() == Some(&Tok::Not) {
            self.pos += 1;
            return Ok(!self.unary()?);
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<bool, String> {
        match self.peek() {
            Some(Tok::LParen) => {
                self.pos += 1;
                let v = self.or()?;
                if self.next() != Some(Tok::RParen) {
                    return Err("missing `)` in #if expression".into());
                }
                Ok(v)
            }
            Some(Tok::Ident(name)) if name == "defined" => {
                self.pos += 1;
                let paren = self.peek() == Some(&Tok::LParen);
                if paren {
                    self.pos += 1;
                }
                let Some(Tok::Ident(name)) = self.next() else {
                    return Err("`defined` takes a name".into());
                };
                if paren && self.next() != Some(Tok::RParen) {
                    return Err("missing `)` after `defined(NAME`".into());
                }
                Ok(self.defines.contains(&name))
            }
            _ => {
                let (lhs, lhs_text) = self.operand()?;
                let Some(Tok::Cmp(op)) = self.peek().cloned() else {
                    return Ok(lhs.truthy());
                };
                self.pos += 1;
                let (rhs, rhs_text) = self.operand()?;
                compare(op, (lhs, &lhs_text), (rhs, &rhs_text))
            }
        }
    }

    fn operand(&mut self) -> Result<(Val, String), String> {
        match self.next() {
            Some(Tok::Ident(name)) => {
                let v = if !self.defines.contains(&name) {
                    Val::Undefined
                } else {
                    self.values
                        .get(&name)
                        .map_or(Val::Defined, |v| Val::parse(v))
                };
                Ok((v, format!("`{name}`")))
            }
            Some(Tok::Str(s)) => Ok((Val::Str(s.clone()), format!("\"{s}\""))),
            Some(Tok::Int(n)) => Ok((Val::Int(n), n.to_string())),
            Some(t) => Err(format!("unexpected `{}` in #if expression", describe(&t))),
            None => Err("#if expression ends early".into()),
        }
    }
}

fn compare(op: &str, (l, lt): (Val, &str), (r, rt): (Val, &str)) -> Result<bool, String> {
    // As in C, an undefined name is 0; a name defined without a value has
    // nothing to compare.
    let norm = |v: Val, text: &str| match v {
        Val::Undefined => Ok(Val::Int(0)),
        Val::Defined => Err(format!(
            "{text} is defined without a value, so `{op}` has nothing to compare"
        )),
        v => Ok(v),
    };
    let (l, r) = (norm(l, lt)?, norm(r, rt)?);
    if let (Val::Int(a), Val::Int(b)) = (&l, &r) {
        return Ok(match op {
            "==" => a == b,
            "!=" => a != b,
            "<" => a < b,
            "<=" => a <= b,
            ">" => a > b,
            _ => a >= b,
        });
    }
    let text = |v: &Val| match v {
        Val::Int(n) => n.to_string(),
        Val::Str(s) => s.clone(),
        _ => String::new(),
    };
    match op {
        "==" => Ok(text(&l) == text(&r)),
        "!=" => Ok(text(&l) != text(&r)),
        _ => {
            let (bad, v) = if matches!(l, Val::Str(_)) {
                (lt, &l)
            } else {
                (rt, &r)
            };
            Err(format!(
                "`{op}` compares integers, and {bad} is \"{}\"",
                text(v)
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pp() -> Preprocessor {
        Preprocessor::for_target("x86_64-unknown-linux-gnu", false)
    }

    #[test]
    fn test_ifdef_basic() {
        let mut pp = Preprocessor::new();
        pp.defines.insert("FOO".into());
        let src = r#"
#ifdef FOO
keep this
#endif
outside
"#;
        let out = pp.process(src, "test.iris").unwrap();
        assert!(out.contains("keep this"));
        assert!(out.contains("outside"));
    }

    #[test]
    fn test_ifdef_skipped() {
        let pp = Preprocessor::new();
        let src = r#"
#ifdef MISSING
should be removed
#endif
outside
"#;
        let out = pp.process(src, "test.iris").unwrap();
        assert!(!out.contains("should be removed"));
        assert!(out.contains("outside"));
    }

    #[test]
    fn test_if_else() {
        let mut pp = Preprocessor::new();
        let src = r#"
#if defined(FOO)
branch_a
#else
branch_b
#endif
"#;
        pp.defines.insert("FOO".into());
        let out = pp.process(src, "test.iris").unwrap();
        assert!(out.contains("branch_a"));
        assert!(!out.contains("branch_b"));
    }

    #[test]
    fn test_unterminated_if() {
        let pp = Preprocessor::new();
        let src = r#"
#ifdef FOO
unterminated
"#;
        assert!(pp.process(src, "test.iris").is_err());
    }

    #[test]
    fn dropped_lines_keep_every_position() {
        let mut pp = Preprocessor::new();
        pp.add_define("FAST");
        let src = "#define LIMIT 10\r\n#ifdef FAST\r\nfast()\r\n#else\r\nslow() // é\r\n#endif\r\ntail()\n";
        let out = pp.process(src, "test.iris").unwrap();
        assert_eq!(out.len(), src.len(), "byte length must be preserved");
        assert_eq!(out.lines().count(), src.lines().count());
        assert!(!out.contains("slow"));
        // Every surviving line sits at its original byte offset.
        for kept in ["fast()", "tail()"] {
            assert_eq!(out.find(kept), src.find(kept), "{kept} moved");
        }
    }

    #[test]
    fn test_define_and_use() {
        let mut pp = Preprocessor::new();
        pp.add_define("MY_FLAG");
        let src = r#"
#ifdef MY_FLAG
inside
#endif
"#;
        let out = pp.process(src, "test.iris").unwrap();
        assert!(out.contains("inside"));
    }

    fn kept(pp: &Preprocessor, src: &str) -> Vec<String> {
        pp.process(src, "t.iris")
            .unwrap()
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn if_compares_values() {
        let src = "#define BUILD \"alpha\"\n#define LEVEL 3\n\
                   #if BUILD == \"beta\"\nbeta\n#elif BUILD == \"alpha\" && LEVEL >= 3\nalpha3\n#else\nother\n#endif\n\
                   #if LEVEL != 3 || BUILD != \"alpha\"\nwrong\n#endif\n\
                   #if UNDEFINED_NAME == 0\nundefined_is_zero\n#endif\n";
        assert_eq!(kept(&pp(), src), ["alpha3", "undefined_is_zero"]);
    }

    #[test]
    fn if_has_c_precedence_and_parentheses() {
        // `A || B && C` is `A || (B && C)`.
        let src = "#define A 1\n#if A || UNDEF && UNDEF\nyes\n#endif\n\
                   #if (A || UNDEF) && UNDEF\nno\n#endif\n#if !(UNDEF) && !defined UNDEF\nnot\n#endif\n";
        assert_eq!(kept(&pp(), src), ["yes", "not"]);
    }

    #[test]
    fn a_true_branch_nested_in_a_skipped_one_stays_skipped() {
        let src = "#define PRESENT\n#ifdef ABSENT\n#ifdef PRESENT\ninner\n#endif\nafter_inner\n#else\nelse_branch\n#endif\nend\n";
        assert_eq!(kept(&pp(), src), ["else_branch", "end"]);
    }

    #[test]
    fn misplaced_branches_are_errors() {
        for src in [
            "#if 1\n#else\n#else\n#endif\n",
            "#if 1\n#else\n#elif 1\n#endif\n",
            "#if LEVEL < \"x\"\n#endif\n",
            "#define X\n#if X == 1\n#endif\n",
        ] {
            assert!(pp().process(src, "t.iris").is_err(), "{src}");
        }
    }

    #[test]
    fn macros_are_substituted_as_tokens_at_the_name() {
        let src = "#define LIMIT 10\n#define NAME \"iris\" // the name\nval a = LIMIT + 1\nval s = \"LIMIT\"\n#undef LIMIT\nval b = LIMIT\n";
        let processed = pp().process_full(src, "t.iris").unwrap();
        let tokens = processed.tokenize().unwrap();
        let limit_use = src.find("LIMIT + 1").unwrap() as u32;
        let ten = tokens
            .iter()
            .find(|t| t.node == Token::IntLit(10))
            .expect("LIMIT became 10");
        assert_eq!(ten.span.start.0, limit_use, "at the name's position");
        // Not inside a string, and not after #undef.
        assert!(tokens
            .iter()
            .any(|t| matches!(&t.node, Token::StringLit(s) if s == "LIMIT")));
        assert!(tokens
            .iter()
            .any(|t| matches!(&t.node, Token::Ident(s) if s == "LIMIT")));
    }

    #[test]
    fn the_target_decides_the_predefined_symbols() {
        let linux = Preprocessor::for_target("aarch64-unknown-linux-gnu", true);
        assert!(
            linux.defines.contains("LINUX")
                && linux.defines.contains("AARCH64")
                && linux.defines.contains("DEBUG")
        );
        assert!(
            !linux.defines.contains("WINDOWS")
                && !linux.defines.contains("WASM")
                && !linux.defines.contains("RELEASE")
        );
        let wasm = Preprocessor::for_target("wasm32-wasip1", false);
        assert!(wasm.defines.contains("WASM") && wasm.defines.contains("RELEASE"));
        assert_eq!(wasm.values["TARGET_OS"], "\"wasi\"");
        let mcu = Preprocessor::for_target("thumbv7em-none-eabihf", false);
        assert!(mcu.defines.contains("EMBEDDED") && mcu.defines.contains("ARM"));
    }
}
