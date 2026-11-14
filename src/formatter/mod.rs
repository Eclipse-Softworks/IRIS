mod doc;
mod syntax;
mod trivia;

use std::path::{Path, PathBuf};

use crate::parser::lexer::{Lexer, Spanned, Token};

/// Options controlling code formatting.
#[derive(Debug, Clone)]
pub struct FormatOptions {
    /// Number of spaces per indentation level.
    pub indent: usize,
    /// Maximum target line width before Prettier-style wrapping kicks in.
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
    let normalized = if source.contains('\r') {
        source.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        source.to_owned()
    };

    let tokens = Lexer::new(&normalized)
        .tokenize()
        .map_err(|error| format!("cannot format invalid IRIS source: {}", error))?;
    syntax::validate_iris_syntax(&tokens)
        .map_err(|error| format!("cannot format invalid IRIS source: {}", error))?;

    let non_doc_tokens: Vec<Spanned<Token>> = tokens
        .into_iter()
        .filter(|t| !matches!(t.node, Token::DocComment(_)))
        .collect();

    let document =
        syntax::Formatter::new(&normalized, &non_doc_tokens, options).build_module_doc()?;
    Ok(doc::render(
        &document,
        options.indent,
        options.max_line_width,
    ))
}

/// Formats a single file. When `check_only` is true the file is not modified;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;

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
        assert!(formatted.contains("val a = 1;\n\n    val b = 2;"));
        assert!(formatted.contains("}\n\ndef bar()"));
    }

    #[test]
    fn empty_blocks_kept_compact() {
        let source = "record Empty {}\n";
        let formatted = format_source(source, &FormatOptions::default()).unwrap();
        assert!(formatted.contains("record Empty {}"));
    }

    #[test]
    fn formats_unary_and_binary_operators_cleanly() {
        let source = "def f(flag: bool, x: i64) -> bool {\n    val y = -x + 1 * -2;\n    val z = !flag && (y > -10);\n    return z\n}\n";
        let formatted = format_source(source, &FormatOptions::default()).unwrap();
        assert!(formatted.contains("val y = -x + 1 * -2;"));
        assert!(formatted.contains("val z = !flag && (y > -10);"));
        assert!(!formatted.contains("- x"));
        assert!(!formatted.contains("! flag"));
    }

    #[test]
    fn formats_nested_generics_and_tensor_types_tightly() {
        let source = "def f[T, U](m: list<map<str, option<i64>>>, t: tensor<f32, [4, 8]>) -> option<U> {\n    return none\n}\n";
        let formatted = format_source(source, &FormatOptions::default()).unwrap();
        assert!(formatted.contains(
            "def f[T, U](m: list<map<str, option<i64>>>, t: tensor<f32, [4, 8]>) -> option<U>"
        ));
        assert!(!formatted.contains("> >"));
    }

    #[test]
    fn collapses_short_multiline_calls_and_wraps_long_calls() {
        let short = "def main() -> i64 {\n    val s = add(\n        1,\n        2,\n    );\n    return s\n}\n";
        let short_fmt = format_source(short, &FormatOptions::default()).unwrap();
        assert!(
            short_fmt.contains("val s = add(1, 2);"),
            "got:\n{short_fmt}"
        );

        let long = "def main() -> i64 {\n    val total = compute_weighted_summary_value(first_very_long_argument_name, second_very_long_argument_name, third_very_long_argument_name);\n    return total\n}\n";
        let long_fmt = format_source(long, &FormatOptions::default()).unwrap();
        assert!(
            long_fmt.contains("compute_weighted_summary_value(\n"),
            "got:\n{long_fmt}"
        );
        assert!(
            long_fmt.contains("third_very_long_argument_name,\n"),
            "got:\n{long_fmt}"
        );
    }

    #[test]
    fn hugs_trailing_closure_callback_arguments() {
        let source = "def main() -> i64 {\n    val ys = items.map(|x: i64| {\n        val doubled = x * 2;\n        doubled + 1\n    });\n    return 0\n}\n";
        let formatted = format_source(source, &FormatOptions::default()).unwrap();
        assert!(
            formatted.contains("items.map(|x: i64| {\n"),
            "got:\n{formatted}"
        );
        assert!(formatted.contains("\n    });\n"), "got:\n{formatted}");
    }

    #[test]
    fn wraps_long_method_chains_before_dot() {
        let options = FormatOptions {
            indent: 4,
            max_line_width: 45,
        };
        let source = "def main() -> i64 {\n    val out = numbers.filter(|x: i64| x > 0).map(|x: i64| x * 2).sum();\n    return out\n}\n";
        let formatted = format_source(source, &options).unwrap();
        assert!(
            formatted.contains("\n        .filter("),
            "got:\n{formatted}"
        );
        assert!(formatted.contains("\n        .map("), "got:\n{formatted}");
        assert!(formatted.contains("\n        .sum()"), "got:\n{formatted}");
        // Re-parses cleanly because `.` stays valid across newlines
        let twice = format_source(&formatted, &options).unwrap();
        assert_eq!(formatted, twice);
    }

    #[test]
    fn preserves_numeric_and_string_literal_spellings() {
        let source = r#"def main() -> i64 {
    val hex = 0xff;
    val bin = 0b1010_0101;
    val big = 1_000_000;
    val flt = 1.0;
    val sci = 1.5e-4;
    val ch = 'a';
    val fstr = f"v={hex}";
    val raw = r"a\nb";
    val uni = "日本語"; // unicode comment: café
    return big
}
"#;
        let formatted = format_source(source, &FormatOptions::default()).unwrap();
        assert!(formatted.contains("0xff"));
        assert!(formatted.contains("0b1010_0101"));
        assert!(formatted.contains("1_000_000"));
        assert!(formatted.contains("1.0"));
        assert!(formatted.contains("1.5e-4"));
        assert!(formatted.contains("'a'"));
        assert!(formatted.contains(r#"f"v={hex}""#));
        assert!(formatted.contains(r#"r"a\nb""#));
        assert!(formatted.contains(r#""日本語""#));
        assert!(formatted.contains("// unicode comment: café"));
    }

    #[test]
    fn produces_no_trailing_whitespace_even_on_blank_lines() {
        let source = "def main() -> i64 {\n    val a = 1;   \n    \n    val b = 2;  // note  \n    return a + b\n}\n";
        let formatted = format_source(source, &FormatOptions::default()).unwrap();
        for (idx, line) in formatted.lines().enumerate() {
            assert_eq!(
                line,
                line.trim_end(),
                "line {} has trailing whitespace: {:?}",
                idx + 1,
                line
            );
        }
        assert!(formatted.ends_with('\n'));
        assert!(!formatted.ends_with("\n\n"));
    }

    #[test]
    fn omits_trailing_comma_after_fn_type_effect_row() {
        let source = "pub def run_tx[T](validator: |T| -> bool, candidate: || -> T effect alloc, fs) -> option<T> effect transaction, alloc, fs { none() }\n";
        let formatted = format_source(source, &FormatOptions::default()).unwrap();
        assert!(
            formatted.contains("candidate: || -> T effect alloc, fs\n"),
            "got:\n{formatted}"
        );
        assert!(
            !formatted.contains("effect alloc, fs,"),
            "got:\n{formatted}"
        );
        let twice = format_source(&formatted, &FormatOptions::default()).unwrap();
        assert_eq!(formatted, twice);
    }

    #[test]
    fn leading_and_eol_comments_do_not_force_compact_constructs_to_break() {
        let source = "// header comment\nrecord Cat { name: str }\nrecord Dog { name: str } // inline\ndef id(x: i64) -> i64 { x } // keep flat\n";
        let formatted = format_source(source, &FormatOptions::default()).unwrap();
        assert!(formatted.contains("// header comment\nrecord Cat { name: str }\nrecord Dog { name: str } // inline\ndef id(x: i64) -> i64 { x } // keep flat\n"), "got:\n{formatted}");
        let twice = format_source(&formatted, &FormatOptions::default()).unwrap();
        assert_eq!(formatted, twice);
    }

    #[test]
    fn keeps_if_followed_by_line_start_deref_or_neg_separate_and_wraps_binop_safely() {
        let separate = "def f(c: i64) -> i64 {\n    val y = if c == 2 { &mut c } else { &mut c }\n    *y = 7\n    0\n}\n";
        let fmt_sep = format_source(separate, &FormatOptions::default()).unwrap();
        assert!(fmt_sep.contains("}\n    *y = 7\n"), "got:\n{fmt_sep}");
        assert_eq!(
            fmt_sep,
            format_source(&fmt_sep, &FormatOptions::default()).unwrap()
        );

        let wrapped = "def g(cond: bool, alpha: i64, beta: i64) -> i64 {\n    val total = if cond { alpha + 1 } else { beta + 2 } - very_long_offset_identifier(alpha, beta)\n    total\n}\n";
        let narrow = FormatOptions {
            indent: 4,
            max_line_width: 60,
        };
        let fmt_wrap = format_source(wrapped, &narrow).unwrap();
        assert!(
            fmt_wrap.contains("} -\n"),
            "expected trailing '-', got:\n{fmt_wrap}"
        );
        assert_eq!(fmt_wrap, format_source(&fmt_wrap, &narrow).unwrap());
    }

    #[test]
    fn corpus_roundtrip_and_idempotence_across_repo_iris_files() {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        for sub in ["src/stdlib", "examples", "tests"] {
            let dir = manifest_dir.join(sub);
            if !dir.is_dir() {
                continue;
            }
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().is_some_and(|e| e == "iris") {
                    files.push(path);
                }
            }
        }
        files.sort();
        assert!(!files.is_empty(), "expected repo .iris files to test");

        let opts = FormatOptions::default();
        let mut checked = 0usize;
        let mut failures = Vec::new();
        for path in &files {
            let src = std::fs::read_to_string(path).unwrap();
            let normalized = src.replace("\r\n", "\n");
            let Ok(toks) = Lexer::new(&normalized).tokenize() else {
                // Skip negative lexer test fixtures and preprocessor (#ifdef/#define) fixtures.
                continue;
            };
            if Parser::new(&toks).parse_module().is_err() {
                // Skip intentional parse-error negative test fixtures.
                continue;
            }
            checked += 1;

            let formatted = match format_source(&src, &opts) {
                Ok(f) => f,
                Err(e) => {
                    failures.push(format!("{}: format error: {e}", path.display()));
                    continue;
                }
            };
            let reformatted = match format_source(&formatted, &opts) {
                Ok(f) => f,
                Err(e) => {
                    failures.push(format!(
                        "{}: formatted output failed re-parse: {e}\n--- FIRST 40 LINES ---\n{}",
                        path.display(),
                        formatted.lines().take(40).collect::<Vec<_>>().join("\n")
                    ));
                    continue;
                }
            };
            if formatted != reformatted {
                let first_diff = formatted
                    .lines()
                    .zip(reformatted.lines())
                    .enumerate()
                    .find(|(_, (a, b))| a != b)
                    .map(|(idx, (a, b))| format!("line {}: {:?} vs {:?}", idx + 1, a, b))
                    .unwrap_or_else(|| {
                        format!(
                            "line count {} vs {}",
                            formatted.lines().count(),
                            reformatted.lines().count()
                        )
                    });
                failures.push(format!("{}: not idempotent ({first_diff})", path.display()));
                continue;
            }
            for (idx, line) in formatted.lines().enumerate() {
                if line != line.trim_end() {
                    failures.push(format!(
                        "{}:{}: trailing whitespace: {:?}",
                        path.display(),
                        idx + 1,
                        line
                    ));
                    break;
                }
            }
        }
        assert!(
            failures.is_empty(),
            "corpus failures ({}/{}):\n{}",
            failures.len(),
            checked,
            failures.join("\n")
        );
        assert!(
            checked >= 20,
            "expected at least 20 valid corpus files, got {checked}"
        );
    }
}
