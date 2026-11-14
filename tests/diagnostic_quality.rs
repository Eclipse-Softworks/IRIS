//! The diagnostics users see for common mistakes (tests/diagnostics/*.iris).
//!
//! Each file states, in its header, the error it should get:
//!
//! ```text
//! // expect: E0109 at 6:13      code and primary location
//! // message: takes 2 arguments substring of the message
//! // fix: <text>                a fix is offered; the fixed source contains <text>
//! // fix-compiles               and that fixed source compiles
//! // expect: ok                 the program compiles
//! ```
//!
//! Every error must also have a location, an `iris explain` entry, and no
//! compiler internals (IR value names, pass names) anywhere in what is shown.
//! The test runs the real CLI with `--error-format json` and reads the
//! diagnostics it prints, so it covers what a user and an editor receive.

use std::path::{Path, PathBuf};
use std::process::Command;

fn iris() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_iris"));
    c.env_remove("IRIS_DEBUG");
    c
}

#[derive(Default)]
struct Expect {
    ok: bool,
    code: Option<String>,
    line: u32,
    col: u32,
    messages: Vec<String>,
    fix: Option<String>,
    fix_compiles: bool,
}

fn expectations(source: &str) -> Expect {
    let mut e = Expect::default();
    for line in source.lines() {
        let Some(rest) = line.strip_prefix("// ") else {
            break;
        };
        if let Some(v) = rest.strip_prefix("expect: ") {
            if v.trim() == "ok" {
                e.ok = true;
            } else {
                let (code, at) = v.split_once(" at ").expect("expect: CODE at L:C");
                let (l, c) = at.trim().split_once(':').expect("L:C");
                e.code = Some(code.trim().to_string());
                e.line = l.parse().unwrap();
                e.col = c.parse().unwrap();
            }
        } else if let Some(v) = rest.strip_prefix("message: ") {
            e.messages.push(v.trim().to_string());
        } else if let Some(v) = rest.strip_prefix("fix: ") {
            e.fix = Some(v.trim().to_string());
        } else if rest.trim() == "fix-compiles" {
            e.fix_compiles = true;
        }
    }
    e
}

/// Text that only means something inside the compiler.
const INTERNAL: &[&str] = &[
    "verify_uses_defined",
    "verify_release_dominance",
    "[after ",
    "Br arg",
    "operand[",
    "Return value %",
    "block0",
    "SSA form",
    "[type/pass error]",
    "[compile error]",
    "[syntax error]",
    "(at byte",
    "Some('",
    "variant indices",
    "This construct is not yet supported",
];

fn leaks(text: &str) -> Vec<String> {
    let mut found: Vec<String> = INTERNAL
        .iter()
        .filter(|m| text.contains(*m))
        .map(|m| m.to_string())
        .collect();
    // IR value names: `%` followed by digits.
    let bytes = text.as_bytes();
    for i in 0..bytes.len().saturating_sub(1) {
        if bytes[i] == b'%'
            && bytes[i + 1].is_ascii_digit()
            && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric())
        {
            found.push("%<n>".into());
            break;
        }
    }
    found
}

fn apply(source: &str, edits: &[(usize, usize, String)]) -> String {
    let mut edits = edits.to_vec();
    edits.sort_by_key(|e| std::cmp::Reverse(e.0));
    let mut out = source.to_string();
    for (a, b, r) in edits {
        out.replace_range(a..b, &r);
    }
    out
}

struct Outcome {
    name: String,
    located: bool,
    fixed: bool,
    problems: Vec<String>,
}

fn check(path: &Path, scratch: &Path) -> Outcome {
    let name = path.file_stem().unwrap().to_string_lossy().into_owned();
    let source = std::fs::read_to_string(path).unwrap();
    let expect = expectations(&source);
    let out = iris()
        .args(["--emit", "ir", "--error-format", "json"])
        .arg(path)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let mut o = Outcome {
        name: name.clone(),
        located: false,
        fixed: false,
        problems: Vec::new(),
    };

    if expect.ok {
        if !out.status.success() {
            o.problems.push(format!("should compile, but:\n{stderr}"));
        }
        return o;
    }
    if out.status.success() {
        o.problems.push("accepted; it should be an error".into());
        return o;
    }
    let diags: Vec<serde_json::Value> = stderr
        .lines()
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let Some(first) = diags.first() else {
        o.problems
            .push(format!("no JSON diagnostic printed:\n{stderr}"));
        return o;
    };
    for line in stderr
        .lines()
        .filter(|l| !l.starts_with('{') && !l.trim().is_empty())
    {
        o.problems
            .push(format!("stray stderr line in JSON mode: {line}"));
    }
    let rendered = first["rendered"].as_str().unwrap_or("");
    let message = first["message"].as_str().unwrap_or("");
    let code = first["code"].as_str().unwrap_or("");

    let leaked = leaks(rendered);
    if !leaked.is_empty() {
        o.problems.push(format!(
            "compiler internals shown ({}):\n{rendered}",
            leaked.join(", ")
        ));
    }
    if Some(code) != expect.code.as_deref() {
        o.problems.push(format!(
            "code {code}, expected {}",
            expect.code.clone().unwrap_or_default()
        ));
    }
    if iris::explain::explain(code).is_none() {
        o.problems
            .push(format!("`iris explain {code}` has no entry"));
    }
    let primary = first["spans"]
        .as_array()
        .and_then(|s| s.iter().find(|s| s["is_primary"] == true).or(s.first()))
        .cloned();
    match primary {
        Some(span) => {
            o.located = true;
            let (l, c) = (
                span["line_start"].as_u64().unwrap_or(0) as u32,
                span["column_start"].as_u64().unwrap_or(0) as u32,
            );
            if (l, c) != (expect.line, expect.col) {
                o.problems.push(format!(
                    "at {l}:{c}, expected {}:{}",
                    expect.line, expect.col
                ));
            }
        }
        None => o.problems.push("no location".into()),
    }
    for m in &expect.messages {
        if !message.contains(m.as_str()) {
            o.problems
                .push(format!("message does not mention {m:?}: {message}"));
        }
    }

    // The first child carrying replacements is the fix.
    let fix = first["children"].as_array().and_then(|children| {
        children.iter().find_map(|c| {
            let spans = c["spans"].as_array()?;
            let edits: Vec<(usize, usize, String)> = spans
                .iter()
                .filter_map(|s| {
                    Some((
                        s["byte_start"].as_u64()? as usize,
                        s["byte_end"].as_u64()? as usize,
                        s["suggested_replacement"].as_str()?.to_string(),
                    ))
                })
                .collect();
            (!edits.is_empty()).then_some(edits)
        })
    });
    if let Some(edits) = &fix {
        o.fixed = true;
        let fixed = apply(&source, edits);
        if let Some(want) = &expect.fix {
            if !fixed.contains(want.as_str()) {
                o.problems
                    .push(format!("fixed source does not contain {want:?}:\n{fixed}"));
            }
        }
        if expect.fix_compiles {
            let f = scratch.join(format!("{name}.fixed.iris"));
            std::fs::write(&f, &fixed).unwrap();
            let r = iris().args(["--emit", "ir"]).arg(&f).output().unwrap();
            if !r.status.success() {
                o.problems.push(format!(
                    "the fixed program does not compile:\n{fixed}\n{}",
                    String::from_utf8_lossy(&r.stderr)
                ));
            }
        }
    } else if expect.fix.is_some() || expect.fix_compiles {
        o.problems.push("no fix offered".into());
    }
    o
}

#[test]
fn common_mistakes_get_located_explained_fixable_diagnostics() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("diagnostics");
    let scratch = std::env::temp_dir().join(format!("iris_diag_quality_{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "iris"))
        .collect();
    files.sort();

    let outcomes: Vec<Outcome> = files.iter().map(|f| check(f, &scratch)).collect();
    let errors: Vec<&Outcome> = outcomes
        .iter()
        .filter(|o| {
            !expectations(&std::fs::read_to_string(dir.join(format!("{}.iris", o.name))).unwrap())
                .ok
        })
        .collect();
    let located = errors.iter().filter(|o| o.located).count();
    let fixed = errors.iter().filter(|o| o.fixed).count();
    eprintln!(
        "{} mistakes: {} located ({}%), {} with a fix ({}%)",
        errors.len(),
        located,
        located * 100 / errors.len().max(1),
        fixed,
        fixed * 100 / errors.len().max(1)
    );
    let failing: Vec<&Outcome> = outcomes.iter().filter(|o| !o.problems.is_empty()).collect();
    let report: String = failing
        .iter()
        .map(|o| format!("--- {}\n{}\n", o.name, o.problems.join("\n")))
        .collect();
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(
        failing.is_empty(),
        "{} of {} cases fail:\n{report}",
        failing.len(),
        outcomes.len()
    );
}

/// Every error code the compiler's source can emit has an `iris explain`
/// entry, so a new code cannot ship without one.
#[test]
fn every_error_code_in_the_compiler_is_explained() {
    fn scan(dir: &Path, out: &mut std::collections::BTreeSet<String>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                scan(&path, out);
            } else if path.extension().is_some_and(|x| x == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                let bytes = text.as_bytes();
                for i in 0..bytes.len().saturating_sub(7) {
                    if bytes[i] == b'"'
                        && bytes[i + 1] == b'E'
                        && bytes[i + 6] == b'"'
                        && bytes[i + 2..i + 6].iter().all(u8::is_ascii_digit)
                    {
                        out.insert(text[i + 1..i + 6].to_string());
                    }
                }
            }
        }
    }
    let mut codes = std::collections::BTreeSet::new();
    scan(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut codes,
    );
    // Codes that appear only in unit tests of the renderer and of `explain`.
    for test_only in ["E0425", "E9999"] {
        codes.remove(test_only);
    }
    let missing: Vec<&String> = codes
        .iter()
        .filter(|c| iris::explain::explain(c).is_none())
        .collect();
    assert!(
        missing.is_empty(),
        "codes without an `iris explain` entry: {missing:?}"
    );
}
