//! C/C++ preprocessor support for `iris bindgen`.
//!
//! Supports both external `clang -E -dD` preprocessing (for full macro
//! expansion, `#include` resolution, and `#if` evaluation across headers) and
//! a standalone built-in preprocessor fallback for offline/no-clang usage.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{is_c_ident, BindgenOptions, ConstItem, MacroFnItem};

/// Result of preprocessing a C/C++ header.
#[derive(Debug, Clone, Default)]
pub(crate) struct PreprocessedHeader {
    /// Preprocessed source lines belonging to user headers (comments stripped,
    /// inactive `#if` branches removed, `#define`s stripped).
    pub cleaned_source: String,
    /// Evaluated `#define` constants (`#define NAME VALUE`).
    pub consts: Vec<ConstItem>,
    /// Safe pure expression function-like macros (`#define SQR(x) ((x) * (x))`).
    pub macro_fns: Vec<MacroFnItem>,
    /// Evaluated integer constant symbol table from `#define`s.
    pub int_env: HashMap<String, i64>,
}

#[derive(Debug, Clone)]
enum DefineValue {
    Int(i64),
    Float(f64),
    Str(String),
    Tokens(String),
}

#[derive(Debug, Clone, Copy)]
struct CondFrame {
    parent_active: bool,
    any_taken: bool,
    active: bool,
}

/// Find `clang++` or `clang` on PATH (or via `IRIS_CLANG`).
pub(crate) fn find_clang_binary(cpp: bool) -> Option<String> {
    if let Ok(explicit) = std::env::var("IRIS_CLANG") {
        let trimmed = explicit.trim();
        if !trimmed.is_empty() && Path::new(trimmed).exists() {
            if cpp {
                let p = Path::new(trimmed);
                if let Some(parent) = p.parent() {
                    let cpp_exe = parent.join(if cfg!(windows) {
                        "clang++.exe"
                    } else {
                        "clang++"
                    });
                    if cpp_exe.exists() {
                        return Some(cpp_exe.to_string_lossy().into_owned());
                    }
                }
            }
            return Some(trimmed.to_owned());
        }
    }

    let candidates: &[&str] = if cpp {
        &["clang++", "clang"]
    } else {
        &["clang", "clang++"]
    };
    for cand in candidates {
        if Command::new(cand)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
        {
            return Some((*cand).to_owned());
        }
    }
    None
}

/// Normalize a filesystem path string for case-insensitive Windows / Unix comparison.
pub(crate) fn normalize_path_key(path: &Path) -> String {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut s = resolved.to_string_lossy().replace('\\', "/");
    if let Some(stripped) = s.strip_prefix("//?/") {
        s = stripped.to_owned();
    }
    if cfg!(windows) {
        s.make_ascii_lowercase();
    }
    s
}

/// Check whether a file location belongs to the target header or explicit `-I` dirs.
pub(crate) fn is_user_header_path(
    candidate: &str,
    target_header: Option<&Path>,
    include_dirs: &[PathBuf],
) -> bool {
    if candidate.is_empty() || candidate == "<built-in>" || candidate == "<command line>" {
        return false;
    }
    let key = normalize_path_key(Path::new(candidate));
    if let Some(target) = target_header {
        let target_key = normalize_path_key(target);
        if key == target_key {
            return true;
        }
        if let Some(parent) = target.parent() {
            if !parent.as_os_str().is_empty() {
                let parent_key = normalize_path_key(parent);
                if key.starts_with(&parent_key) && !is_system_header_path(&key) {
                    return true;
                }
            }
        }
    }
    for inc in include_dirs {
        let inc_key = normalize_path_key(inc);
        if key.starts_with(&inc_key) && !is_system_header_path(&key) {
            return true;
        }
    }
    false
}

fn is_system_header_path(normalized_lower: &str) -> bool {
    normalized_lower.contains("/include/c++/")
        || normalized_lower.contains("/x86_64-w64-mingw32/include")
        || normalized_lower.contains("/clang/")
        || normalized_lower.starts_with("/usr/include")
        || normalized_lower.contains("windows kits")
        || normalized_lower.contains("visual studio")
}

/// Preprocess a header file using `clang -E -dD` when available, otherwise fallback.
pub(crate) fn preprocess_header(
    header_path: Option<&Path>,
    header_src: &str,
    opts: &BindgenOptions,
    cpp_mode: bool,
) -> PreprocessedHeader {
    if !opts.no_clang {
        if let Some(path) = header_path {
            if let Some(clang) = find_clang_binary(cpp_mode) {
                if let Ok(res) = preprocess_with_clang(&clang, path, opts, cpp_mode) {
                    return res;
                }
            }
        }
    }
    preprocess_standalone(header_path, header_src, opts)
}

fn preprocess_with_clang(
    clang: &str,
    header_path: &Path,
    opts: &BindgenOptions,
    cpp_mode: bool,
) -> Result<PreprocessedHeader, String> {
    let mut cmd = Command::new(clang);
    cmd.arg("-E").arg("-dD").arg("-CC");
    if cpp_mode {
        cmd.arg("-x").arg("c++").arg("-std=c++17");
    } else {
        cmd.arg("-x").arg("c");
    }
    if let Some(parent) = header_path.parent() {
        if !parent.as_os_str().is_empty() {
            cmd.arg("-I").arg(parent);
        }
    }
    for dir in &opts.include_dirs {
        cmd.arg("-I").arg(dir);
    }
    for def in &opts.defines {
        cmd.arg(format!("-D{def}"));
    }
    cmd.arg(header_path);

    let out = cmd
        .output()
        .map_err(|e| format!("failed to run clang: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut current_in_user_file = true;
    let mut int_env: HashMap<String, i64> = HashMap::new();
    let mut raw_macros: HashMap<String, DefineValue> = HashMap::new();
    let mut consts = Vec::new();
    let mut seen_consts = HashSet::new();
    let mut macro_fns = Vec::new();
    let mut seen_macro_fns = HashSet::new();
    let mut cleaned = String::new();

    // Seed CLI -D defines into int_env so dependent macros evaluate too.
    seed_cli_defines(&opts.defines, &mut int_env, &mut raw_macros);

    for raw_line in text.lines() {
        let trimmed = raw_line.trim();
        if let Some(marker_file) = parse_linemarker_file(trimmed) {
            current_in_user_file =
                is_user_header_path(&marker_file, Some(header_path), &opts.include_dirs);
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("#define") {
            let rest = rest.trim_start();
            // Always update internal constant table for object-like integer macros,
            // but only emit IRIS declarations for macros defined inside user headers.
            if let Some((name, val, decl)) = parse_object_macro(rest, &int_env, &raw_macros) {
                if let DefineValue::Int(n) = &val {
                    int_env.insert(name.clone(), *n);
                }
                raw_macros.insert(name.clone(), val);
                if current_in_user_file && !name.starts_with('_') && seen_consts.insert(name) {
                    if let Some(c) = decl {
                        consts.push(c);
                    }
                }
            } else if current_in_user_file {
                if let Some(mfn) = parse_function_like_macro(rest, &int_env) {
                    if !mfn.name.starts_with('_') && seen_macro_fns.insert(mfn.name.clone()) {
                        macro_fns.push(mfn);
                    }
                }
            }
            continue;
        }
        if trimmed.starts_with('#') {
            continue;
        }
        if current_in_user_file {
            cleaned.push_str(raw_line);
            cleaned.push('\n');
        }
    }

    Ok(PreprocessedHeader {
        cleaned_source: strip_c_comments(&cleaned),
        consts,
        macro_fns,
        int_env,
    })
}

fn parse_linemarker_file(trimmed: &str) -> Option<String> {
    if !trimmed.starts_with('#') {
        return None;
    }
    let after_hash = trimmed[1..].trim_start();
    let first_digit_end = after_hash.find(|c: char| !c.is_ascii_digit())?;
    if first_digit_end == 0 {
        return None;
    }
    let after_num = after_hash[first_digit_end..].trim_start();
    let quoted = after_num.strip_prefix('"')?;
    let end_quote = quoted.find('"')?;
    Some(quoted[..end_quote].replace("\\\\", "\\"))
}

/// Standalone built-in C/C++ preprocessor fallback (`--no-clang` or clang absent).
pub(crate) fn preprocess_standalone(
    header_path: Option<&Path>,
    header_src: &str,
    opts: &BindgenOptions,
) -> PreprocessedHeader {
    let mut visited_files = HashSet::new();
    if let Some(p) = header_path {
        visited_files.insert(normalize_path_key(p));
    }

    let mut int_env: HashMap<String, i64> = HashMap::new();
    let mut raw_macros: HashMap<String, DefineValue> = HashMap::new();
    let mut text_macros: HashMap<String, String> = HashMap::new();
    seed_cli_defines(&opts.defines, &mut int_env, &mut raw_macros);
    for d in &opts.defines {
        if let Some((k, v)) = d.split_once('=') {
            text_macros.insert(k.trim().to_owned(), v.trim().to_owned());
        } else {
            text_macros.insert(d.trim().to_owned(), "1".to_owned());
        }
    }

    let mut consts = Vec::new();
    let mut seen_consts = HashSet::new();
    let mut macro_fns = Vec::new();
    let mut seen_macro_fns = HashSet::new();
    let mut cleaned = String::new();

    process_source_recursive(
        header_src,
        header_path.and_then(|p| p.parent()),
        &opts.include_dirs,
        &mut visited_files,
        &mut int_env,
        &mut raw_macros,
        &mut text_macros,
        &mut consts,
        &mut seen_consts,
        &mut macro_fns,
        &mut seen_macro_fns,
        &mut cleaned,
    );

    PreprocessedHeader {
        cleaned_source: cleaned,
        consts,
        macro_fns,
        int_env,
    }
}

#[allow(clippy::too_many_arguments)]
fn process_source_recursive(
    src: &str,
    current_dir: Option<&Path>,
    include_dirs: &[PathBuf],
    visited: &mut HashSet<String>,
    int_env: &mut HashMap<String, i64>,
    raw_macros: &mut HashMap<String, DefineValue>,
    text_macros: &mut HashMap<String, String>,
    consts: &mut Vec<ConstItem>,
    seen_consts: &mut HashSet<String>,
    macro_fns: &mut Vec<MacroFnItem>,
    seen_macro_fns: &mut HashSet<String>,
    cleaned: &mut String,
) {
    let joined = join_backslash_continuations(src);
    let no_comments = strip_c_comments(&joined);
    let mut cond_stack: Vec<CondFrame> = Vec::new();

    for line in no_comments.lines() {
        let trimmed = line.trim();
        let currently_active = matches!(
            cond_stack.last(),
            None | Some(CondFrame { active: true, .. })
        );

        if let Some(rest) = trimmed.strip_prefix('#') {
            let dir = rest.trim_start();
            if let Some(arg) = strip_kw(dir, "ifdef") {
                let is_def = is_defined_macro(arg.trim(), text_macros, raw_macros);
                let active = currently_active && is_def;
                cond_stack.push(CondFrame {
                    parent_active: currently_active,
                    any_taken: is_def,
                    active,
                });
                continue;
            }
            if let Some(arg) = strip_kw(dir, "ifndef") {
                let is_def = !is_defined_macro(arg.trim(), text_macros, raw_macros);
                let active = currently_active && is_def;
                cond_stack.push(CondFrame {
                    parent_active: currently_active,
                    any_taken: is_def,
                    active,
                });
                continue;
            }
            if let Some(expr) = strip_kw(dir, "if") {
                let taken = eval_pp_condition(expr, int_env, text_macros, raw_macros);
                let active = currently_active && taken;
                cond_stack.push(CondFrame {
                    parent_active: currently_active,
                    any_taken: taken,
                    active,
                });
                continue;
            }
            if let Some(expr) = strip_kw(dir, "elif") {
                if let Some(top) = cond_stack.last_mut() {
                    if !top.parent_active || top.any_taken {
                        top.active = false;
                    } else {
                        let taken = eval_pp_condition(expr, int_env, text_macros, raw_macros);
                        top.any_taken = taken;
                        top.active = taken;
                    }
                }
                continue;
            }
            if strip_kw(dir, "else").is_some() {
                if let Some(top) = cond_stack.last_mut() {
                    top.active = top.parent_active && !top.any_taken;
                    top.any_taken = true;
                }
                continue;
            }
            if strip_kw(dir, "endif").is_some() {
                cond_stack.pop();
                continue;
            }
            if !currently_active {
                continue;
            }
            if let Some(rest_def) = strip_kw(dir, "define") {
                if let Some((name, val, decl)) = parse_object_macro(rest_def, int_env, raw_macros) {
                    if let DefineValue::Int(n) = &val {
                        int_env.insert(name.clone(), *n);
                    }
                    text_macros.insert(
                        name.clone(),
                        match &val {
                            DefineValue::Int(n) => n.to_string(),
                            DefineValue::Float(f) => f.to_string(),
                            DefineValue::Str(s) => format!("\"{s}\""),
                            DefineValue::Tokens(t) => t.clone(),
                        },
                    );
                    raw_macros.insert(name.clone(), val);
                    if !name.starts_with('_') && seen_consts.insert(name) {
                        if let Some(c) = decl {
                            consts.push(c);
                        }
                    }
                } else {
                    // Track empty header guard `#define FOO_H` as defined
                    if is_c_ident(rest_def.trim()) {
                        text_macros.insert(rest_def.trim().to_owned(), String::new());
                    } else if let Some(mfn) = parse_function_like_macro(rest_def, int_env) {
                        if !mfn.name.starts_with('_') && seen_macro_fns.insert(mfn.name.clone()) {
                            macro_fns.push(mfn);
                        }
                    }
                }
                continue;
            }
            if let Some(undef_name) = strip_kw(dir, "undef") {
                let k = undef_name.trim();
                int_env.remove(k);
                raw_macros.remove(k);
                text_macros.remove(k);
                continue;
            }
            if let Some(inc_arg) = strip_kw(dir, "include") {
                if let Some(inc_file) = parse_quoted_include(inc_arg) {
                    if let Some(resolved) =
                        resolve_local_include(&inc_file, current_dir, include_dirs)
                    {
                        let key = normalize_path_key(&resolved);
                        if visited.insert(key) {
                            if let Ok(inc_src) = std::fs::read_to_string(&resolved) {
                                process_source_recursive(
                                    &inc_src,
                                    resolved.parent(),
                                    include_dirs,
                                    visited,
                                    int_env,
                                    raw_macros,
                                    text_macros,
                                    consts,
                                    seen_consts,
                                    macro_fns,
                                    seen_macro_fns,
                                    cleaned,
                                );
                            }
                        }
                    }
                }
                continue;
            }
            continue;
        }

        if currently_active {
            let expanded = expand_object_macros_in_line(line, text_macros);
            cleaned.push_str(&expanded);
            cleaned.push('\n');
        }
    }
}

fn seed_cli_defines(
    defines: &[String],
    int_env: &mut HashMap<String, i64>,
    raw_macros: &mut HashMap<String, DefineValue>,
) {
    for d in defines {
        if let Some((name, val)) = d.split_once('=') {
            let k = name.trim().to_owned();
            if let Some(n) = eval_int_expr(val.trim(), int_env) {
                int_env.insert(k.clone(), n);
                raw_macros.insert(k, DefineValue::Int(n));
            } else {
                raw_macros.insert(k, DefineValue::Tokens(val.trim().to_owned()));
            }
        } else {
            let k = d.trim().to_owned();
            if is_c_ident(&k) {
                int_env.insert(k.clone(), 1);
                raw_macros.insert(k, DefineValue::Int(1));
            }
        }
    }
}

fn strip_kw<'a>(line: &'a str, kw: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(kw)?;
    match rest.chars().next() {
        None => Some(""),
        Some(c) if c.is_whitespace() => Some(rest.trim_start()),
        _ => None,
    }
}

fn is_defined_macro(
    name: &str,
    text_macros: &HashMap<String, String>,
    raw_macros: &HashMap<String, DefineValue>,
) -> bool {
    let tok = name.split_whitespace().next().unwrap_or("");
    text_macros.contains_key(tok) || raw_macros.contains_key(tok)
}

fn parse_quoted_include(arg: &str) -> Option<String> {
    let t = arg.trim();
    let after = t.strip_prefix('"')?;
    let end = after.find('"')?;
    Some(after[..end].to_owned())
}

fn resolve_local_include(
    rel: &str,
    current_dir: Option<&Path>,
    include_dirs: &[PathBuf],
) -> Option<PathBuf> {
    if let Some(dir) = current_dir {
        let cand = dir.join(rel);
        if cand.is_file() {
            return Some(cand);
        }
    }
    for dir in include_dirs {
        let cand = dir.join(rel);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

fn expand_object_macros_in_line(line: &str, text_macros: &HashMap<String, String>) -> String {
    if text_macros.is_empty() {
        return line.to_owned();
    }
    // Single-pass token replacement avoiding replacement inside string/char literals
    let mut out = String::with_capacity(line.len());
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if ch == '"' || ch == '\'' {
            let quote = ch;
            out.push(ch);
            i += 1;
            while i < chars.len() {
                let c = chars[i];
                out.push(c);
                i += 1;
                if c == '\\' && i < chars.len() {
                    out.push(chars[i]);
                    i += 1;
                    continue;
                }
                if c == quote {
                    break;
                }
            }
            continue;
        }
        if ch.is_ascii_alphabetic() || ch == '_' {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let ident: String = chars[start..i].iter().collect();
            if let Some(rep) = text_macros.get(&ident) {
                if !rep.is_empty() && !rep.contains('(') && is_c_ident_or_scalar(rep) {
                    out.push_str(rep);
                    continue;
                }
            }
            out.push_str(&ident);
            continue;
        }
        out.push(ch);
        i += 1;
    }
    out
}

fn is_c_ident_or_scalar(s: &str) -> bool {
    is_c_ident(s)
        || s.chars()
            .all(|c| c.is_ascii_digit() || c == '-' || c == '.')
}

/// Join backslash-newline line continuations (`\` at end of line).
pub(crate) fn join_backslash_continuations(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut lines = src.lines().peekable();
    while let Some(line) = lines.next() {
        let mut cur = line.trim_end_matches('\r').to_owned();
        while cur.ends_with('\\') {
            cur.pop();
            if let Some(next_line) = lines.next() {
                cur.push(' ');
                cur.push_str(next_line.trim_end_matches('\r').trim_start());
            } else {
                break;
            }
        }
        out.push_str(&cur);
        out.push('\n');
    }
    out
}

/// Strip `// ...` and `/* ... */` comments while respecting string/char literals.
pub(crate) fn strip_c_comments(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'"' || b == b'\'' {
            let quote = b;
            out.push(quote as char);
            i += 1;
            while i < bytes.len() {
                let c = bytes[i];
                out.push(c as char);
                i += 1;
                if c == b'\\' && i < bytes.len() {
                    out.push(bytes[i] as char);
                    i += 1;
                    continue;
                }
                if c == quote {
                    break;
                }
            }
            continue;
        }
        if b == b'/' && i + 1 < bytes.len() {
            if bytes[i + 1] == b'/' {
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            if bytes[i + 1] == b'*' {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    if bytes[i] == b'\n' {
                        out.push('\n');
                    }
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
                continue;
            }
        }
        out.push(b as char);
        i += 1;
    }
    out
}

fn parse_object_macro(
    rest: &str,
    int_env: &HashMap<String, i64>,
    raw_macros: &HashMap<String, DefineValue>,
) -> Option<(String, DefineValue, Option<ConstItem>)> {
    let rest = rest.trim_start();
    let split_idx = rest
        .find(|c: char| c.is_whitespace() || c == '(')
        .unwrap_or(rest.len());
    let name = &rest[..split_idx];
    if !is_c_ident(name) {
        return None;
    }
    let after = &rest[split_idx..];
    if after.starts_with('(') {
        return None; // function-like macro
    }
    let val_raw = strip_c_comments(after).trim().to_owned();
    if val_raw.is_empty() {
        return None;
    }
    // 1. Quoted string literal
    if val_raw.starts_with('"') && val_raw.ends_with('"') && val_raw.len() >= 2 {
        let inner = val_raw[1..val_raw.len() - 1].to_owned();
        let decl = ConstItem {
            name: name.to_owned(),
            ty: "str".to_owned(),
            value: format!("\"{inner}\""),
        };
        return Some((name.to_owned(), DefineValue::Str(inner), Some(decl)));
    }
    // 2. Alias to another macro
    if let Some(prev) = raw_macros.get(&val_raw) {
        let decl = match prev {
            DefineValue::Int(n) => Some(ConstItem {
                name: name.to_owned(),
                ty: "i64".to_owned(),
                value: n.to_string(),
            }),
            DefineValue::Float(f) => Some(ConstItem {
                name: name.to_owned(),
                ty: "f64".to_owned(),
                value: format_float(*f),
            }),
            DefineValue::Str(s) => Some(ConstItem {
                name: name.to_owned(),
                ty: "str".to_owned(),
                value: format!("\"{s}\""),
            }),
            DefineValue::Tokens(_) => None,
        };
        return Some((name.to_owned(), prev.clone(), decl));
    }
    // 3. Evaluable integer constant expression (e.g. `(BASE * 2 + 5)`, `1 << 4`)
    if let Some(n) = eval_int_expr(&val_raw, int_env) {
        let decl = ConstItem {
            name: name.to_owned(),
            ty: "i64".to_owned(),
            value: n.to_string(),
        };
        return Some((name.to_owned(), DefineValue::Int(n), Some(decl)));
    }
    // 4. Float constant
    let clean_f = val_raw.trim_end_matches(['f', 'F']);
    if let Ok(f) = clean_f.parse::<f64>() {
        let formatted = if clean_f.contains('.') {
            clean_f.to_owned()
        } else {
            format_float(f)
        };
        let decl = ConstItem {
            name: name.to_owned(),
            ty: "f64".to_owned(),
            value: formatted,
        };
        return Some((name.to_owned(), DefineValue::Float(f), Some(decl)));
    }
    Some((name.to_owned(), DefineValue::Tokens(val_raw), None))
}

fn format_float(f: f64) -> String {
    let s = f.to_string();
    if s.contains('.') || s.contains('e') || s.contains('E') {
        s
    } else {
        format!("{s}.0")
    }
}

fn parse_function_like_macro(rest: &str, int_env: &HashMap<String, i64>) -> Option<MacroFnItem> {
    let rest = rest.trim_start();
    let lparen = rest.find('(')?;
    let name = &rest[..lparen];
    if !is_c_ident(name) || name.contains(char::is_whitespace) {
        return None;
    }
    let after_name = &rest[lparen + 1..];
    let rparen = after_name.find(')')?;
    let params_str = &after_name[..rparen];
    let body_raw = strip_c_comments(&after_name[rparen + 1..])
        .trim()
        .to_owned();
    if body_raw.is_empty()
        || body_raw.contains("...")
        || body_raw.contains("__VA_ARGS__")
        || body_raw.contains("do ")
        || body_raw.contains("do{")
        || body_raw.contains(';')
        || body_raw.contains('{')
        || body_raw.contains('#')
        || body_raw.contains('?')
    {
        return None;
    }
    let mut params = Vec::new();
    if !params_str.trim().is_empty() {
        for p in params_str.split(',') {
            let pt = p.trim();
            if !is_c_ident(pt) {
                return None;
            }
            params.push(pt.to_owned());
        }
    }
    if params.is_empty() {
        return None;
    }
    // Verify the macro body parses strictly as an integer arithmetic/bitwise expr over params + int_env
    let iris_body = validate_and_lower_macro_body(&body_raw, &params, int_env)?;
    Some(MacroFnItem {
        name: name.to_owned(),
        params,
        body_expr: iris_body,
    })
}

fn validate_and_lower_macro_body(
    body: &str,
    params: &[String],
    int_env: &HashMap<String, i64>,
) -> Option<String> {
    let tokens = tokenize_expr(body).ok()?;
    if tokens.is_empty() {
        return None;
    }
    let mut out = String::new();
    for tok in &tokens {
        match tok {
            ExprTok::Num(n) => out.push_str(&n.to_string()),
            ExprTok::Ident(id) => {
                if params.contains(id) {
                    out.push_str(id);
                } else {
                    let val = int_env.get(id)?;
                    out.push_str(&val.to_string());
                }
            }
            ExprTok::Op(op) => {
                if !matches!(
                    op.as_str(),
                    "+" | "-" | "*" | "/" | "%" | "<<" | ">>" | "&" | "|" | "^" | "(" | ")"
                ) {
                    return None;
                }
                if op == "(" || op == ")" {
                    out.push_str(op);
                } else {
                    out.push(' ');
                    out.push_str(op);
                    out.push(' ');
                }
            }
        }
    }
    // Check balanced parens & ensure parsing succeeds with dummy args = 1
    let mut dummy_env = int_env.clone();
    for p in params {
        dummy_env.insert(p.clone(), 1);
    }
    eval_int_expr(body, &dummy_env)?;
    Some(out.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Evaluate a preprocessor `#if` / `#elif` condition using `defined(...)` + integer arithmetic.
fn eval_pp_condition(
    expr: &str,
    int_env: &HashMap<String, i64>,
    text_macros: &HashMap<String, String>,
    raw_macros: &HashMap<String, DefineValue>,
) -> bool {
    let mut expanded = String::new();
    let chars: Vec<char> = expr.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_alphabetic() || chars[i] == '_' {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let ident: String = chars[start..i].iter().collect();
            if ident == "defined" {
                while i < chars.len() && chars[i].is_whitespace() {
                    i += 1;
                }
                let has_paren = i < chars.len() && chars[i] == '(';
                if has_paren {
                    i += 1;
                    while i < chars.len() && chars[i].is_whitespace() {
                        i += 1;
                    }
                }
                let mstart = i;
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let target: String = chars[mstart..i].iter().collect();
                if has_paren {
                    while i < chars.len() && chars[i].is_whitespace() {
                        i += 1;
                    }
                    if i < chars.len() && chars[i] == ')' {
                        i += 1;
                    }
                }
                expanded.push(if is_defined_macro(&target, text_macros, raw_macros) {
                    '1'
                } else {
                    '0'
                });
                continue;
            }
            if let Some(v) = int_env.get(&ident) {
                expanded.push_str(&v.to_string());
            } else {
                expanded.push('0');
            }
            continue;
        }
        expanded.push(chars[i]);
        i += 1;
    }
    eval_int_expr(&expanded, int_env).unwrap_or(0) != 0
}

#[derive(Debug, Clone, PartialEq)]
enum ExprTok {
    Num(i64),
    Ident(String),
    Op(String),
}

fn tokenize_expr(expr: &str) -> Result<Vec<ExprTok>, ()> {
    let mut toks = Vec::new();
    let b = expr.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b[i].is_ascii_digit() {
            let start = i;
            if b[i] == b'0' && i + 1 < b.len() && (b[i + 1] == b'x' || b[i + 1] == b'X') {
                i += 2;
                while i < b.len() && (b[i].is_ascii_hexdigit() || b[i] == b'\'') {
                    i += 1;
                }
                let digits: String = expr[start + 2..i].chars().filter(|c| *c != '\'').collect();
                skip_int_suffix(b, &mut i);
                let val = i64::from_str_radix(&digits, 16)
                    .or_else(|_| u64::from_str_radix(&digits, 16).map(|u| u as i64))
                    .map_err(|_| ())?;
                toks.push(ExprTok::Num(val));
                continue;
            }
            if b[i] == b'0' && i + 1 < b.len() && (b[i + 1] == b'b' || b[i + 1] == b'B') {
                i += 2;
                while i < b.len() && (b[i] == b'0' || b[i] == b'1' || b[i] == b'\'') {
                    i += 1;
                }
                let digits: String = expr[start + 2..i].chars().filter(|c| *c != '\'').collect();
                skip_int_suffix(b, &mut i);
                let val = i64::from_str_radix(&digits, 2).map_err(|_| ())?;
                toks.push(ExprTok::Num(val));
                continue;
            }
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'\'') {
                i += 1;
            }
            if i < b.len() && (b[i] == b'.' || b[i] == b'e' || b[i] == b'E') {
                return Err(()); // Float literal — handled elsewhere
            }
            let digits: String = expr[start..i].chars().filter(|c| *c != '\'').collect();
            skip_int_suffix(b, &mut i);
            let val = if digits.starts_with('0') && digits.len() > 1 {
                i64::from_str_radix(&digits[1..], 8).map_err(|_| ())?
            } else {
                digits
                    .parse::<i64>()
                    .or_else(|_| digits.parse::<u64>().map(|u| u as i64))
                    .map_err(|_| ())?
            };
            toks.push(ExprTok::Num(val));
            continue;
        }
        if b[i].is_ascii_alphabetic() || b[i] == b'_' {
            let start = i;
            i += 1;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            toks.push(ExprTok::Ident(expr[start..i].to_owned()));
            continue;
        }
        let two = if i + 1 < b.len() {
            Some(&expr[i..i + 2])
        } else {
            None
        };
        if let Some(op) =
            two.filter(|o| matches!(*o, "<<" | ">>" | "<=" | ">=" | "==" | "!=" | "&&" | "||"))
        {
            toks.push(ExprTok::Op(op.to_owned()));
            i += 2;
            continue;
        }
        let one = (b[i] as char).to_string();
        if matches!(
            one.as_str(),
            "+" | "-"
                | "*"
                | "/"
                | "%"
                | "!"
                | "~"
                | "&"
                | "^"
                | "|"
                | "<"
                | ">"
                | "("
                | ")"
                | "?"
                | ":"
        ) {
            toks.push(ExprTok::Op(one));
            i += 1;
            continue;
        }
        return Err(());
    }
    Ok(toks)
}

fn skip_int_suffix(b: &[u8], i: &mut usize) {
    while *i < b.len() && matches!(b[*i], b'u' | b'U' | b'l' | b'L') {
        *i += 1;
    }
}

/// Evaluate a constant integer C expression (recursive descent) using known `#define` / `enum` values.
pub(crate) fn eval_int_expr(expr: &str, env: &HashMap<String, i64>) -> Option<i64> {
    let toks = tokenize_expr(expr.trim()).ok()?;
    if toks.is_empty() {
        return None;
    }
    let mut parser = ExprParser {
        toks: &toks,
        pos: 0,
        env,
    };
    let val = parser.parse_ternary()?;
    if parser.pos == toks.len() {
        Some(val)
    } else {
        None
    }
}

struct ExprParser<'a> {
    toks: &'a [ExprTok],
    pos: usize,
    env: &'a HashMap<String, i64>,
}

impl<'a> ExprParser<'a> {
    fn peek_op(&self) -> Option<&str> {
        match self.toks.get(self.pos) {
            Some(ExprTok::Op(s)) => Some(s.as_str()),
            _ => None,
        }
    }

    fn eat_op(&mut self, expected: &str) -> bool {
        if self.peek_op() == Some(expected) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn parse_ternary(&mut self) -> Option<i64> {
        let cond = self.parse_log_or()?;
        if self.eat_op("?") {
            let t = self.parse_ternary()?;
            if !self.eat_op(":") {
                return None;
            }
            let f = self.parse_ternary()?;
            return Some(if cond != 0 { t } else { f });
        }
        Some(cond)
    }

    fn parse_log_or(&mut self) -> Option<i64> {
        let mut lhs = self.parse_log_and()?;
        while self.eat_op("||") {
            let rhs = self.parse_log_and()?;
            lhs = i64::from(lhs != 0 || rhs != 0);
        }
        Some(lhs)
    }

    fn parse_log_and(&mut self) -> Option<i64> {
        let mut lhs = self.parse_bit_or()?;
        while self.eat_op("&&") {
            let rhs = self.parse_bit_or()?;
            lhs = i64::from(lhs != 0 && rhs != 0);
        }
        Some(lhs)
    }

    fn parse_bit_or(&mut self) -> Option<i64> {
        let mut lhs = self.parse_bit_xor()?;
        while self.eat_op("|") {
            let rhs = self.parse_bit_xor()?;
            lhs |= rhs;
        }
        Some(lhs)
    }

    fn parse_bit_xor(&mut self) -> Option<i64> {
        let mut lhs = self.parse_bit_and()?;
        while self.eat_op("^") {
            let rhs = self.parse_bit_and()?;
            lhs ^= rhs;
        }
        Some(lhs)
    }

    fn parse_bit_and(&mut self) -> Option<i64> {
        let mut lhs = self.parse_eq()?;
        while self.eat_op("&") {
            let rhs = self.parse_eq()?;
            lhs &= rhs;
        }
        Some(lhs)
    }

    fn parse_eq(&mut self) -> Option<i64> {
        let mut lhs = self.parse_rel()?;
        loop {
            if self.eat_op("==") {
                let rhs = self.parse_rel()?;
                lhs = i64::from(lhs == rhs);
            } else if self.eat_op("!=") {
                let rhs = self.parse_rel()?;
                lhs = i64::from(lhs != rhs);
            } else {
                break;
            }
        }
        Some(lhs)
    }

    fn parse_rel(&mut self) -> Option<i64> {
        let mut lhs = self.parse_shift()?;
        loop {
            if self.eat_op("<") {
                let rhs = self.parse_shift()?;
                lhs = i64::from(lhs < rhs);
            } else if self.eat_op("<=") {
                let rhs = self.parse_shift()?;
                lhs = i64::from(lhs <= rhs);
            } else if self.eat_op(">") {
                let rhs = self.parse_shift()?;
                lhs = i64::from(lhs > rhs);
            } else if self.eat_op(">=") {
                let rhs = self.parse_shift()?;
                lhs = i64::from(lhs >= rhs);
            } else {
                break;
            }
        }
        Some(lhs)
    }

    fn parse_shift(&mut self) -> Option<i64> {
        let mut lhs = self.parse_add()?;
        loop {
            if self.eat_op("<<") {
                let rhs = self.parse_add()?;
                lhs = lhs.wrapping_shl((rhs as u32) & 63);
            } else if self.eat_op(">>") {
                let rhs = self.parse_add()?;
                lhs = lhs.wrapping_shr((rhs as u32) & 63);
            } else {
                break;
            }
        }
        Some(lhs)
    }

    fn parse_add(&mut self) -> Option<i64> {
        let mut lhs = self.parse_mul()?;
        loop {
            if self.eat_op("+") {
                lhs = lhs.checked_add(self.parse_mul()?)?;
            } else if self.eat_op("-") {
                lhs = lhs.checked_sub(self.parse_mul()?)?;
            } else {
                break;
            }
        }
        Some(lhs)
    }

    fn parse_mul(&mut self) -> Option<i64> {
        let mut lhs = self.parse_unary()?;
        loop {
            if self.eat_op("*") {
                lhs = lhs.checked_mul(self.parse_unary()?)?;
            } else if self.eat_op("/") {
                let rhs = self.parse_unary()?;
                if rhs == 0 {
                    return None;
                }
                lhs = lhs.checked_div(rhs)?;
            } else if self.eat_op("%") {
                let rhs = self.parse_unary()?;
                if rhs == 0 {
                    return None;
                }
                lhs = lhs.checked_rem(rhs)?;
            } else {
                break;
            }
        }
        Some(lhs)
    }

    fn parse_unary(&mut self) -> Option<i64> {
        if self.eat_op("+") {
            return self.parse_unary();
        }
        if self.eat_op("-") {
            return self.parse_unary()?.checked_neg();
        }
        if self.eat_op("~") {
            return Some(!self.parse_unary()?);
        }
        if self.eat_op("!") {
            return Some(i64::from(self.parse_unary()? == 0));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Option<i64> {
        if self.eat_op("(") {
            let v = self.parse_ternary()?;
            if !self.eat_op(")") {
                return None;
            }
            return Some(v);
        }
        match self.toks.get(self.pos)? {
            ExprTok::Num(n) => {
                self.pos += 1;
                Some(*n)
            }
            ExprTok::Ident(id) => {
                self.pos += 1;
                if id == "true" {
                    Some(1)
                } else if id == "false" {
                    Some(0)
                } else {
                    self.env.get(id).copied()
                }
            }
            ExprTok::Op(_) => None,
        }
    }
}
