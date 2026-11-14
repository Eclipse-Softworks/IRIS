//! The source type checker (src/lower/typeck.rs) against the corpus.
//!
//! It types the program before lowering, and is held to what lowering and
//! inference produce: every value of every program in `tests/` and
//! `examples/` is compared (`IRIS_TYPECK=compare`), and a disagreement is a
//! bug in one of the two. It must also report nothing on a valid program --
//! its errors stop the build.

use std::path::{Path, PathBuf};
use std::process::Command;

fn corpus() -> Vec<PathBuf> {
    let mut files = Vec::new();
    for dir in ["tests", "examples"] {
        collect(Path::new(dir), &mut files);
    }
    files.sort();
    files
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // The diagnostics corpus holds programs that must not compile.
            if !path.ends_with("diagnostics") && !path.ends_with("runtime_errors") {
                collect(&path, out);
            }
        } else if path.extension().is_some_and(|e| e == "iris") {
            // So does a test marked to fail compilation.
            let compile_fail = std::fs::read_to_string(&path)
                .is_ok_and(|s| s.contains("// iris-test: compile-fail"));
            if !compile_fail {
                out.push(path);
            }
        }
    }
}

#[derive(Default)]
struct Totals {
    compiled: usize,
    expressions: usize,
    typed: usize,
    agree: usize,
    disagree: usize,
    errors: Vec<String>,
    differs: Vec<String>,
}

fn number_after(line: &str, word: &str) -> usize {
    line.split_whitespace()
        .skip_while(|w| *w != word)
        .nth(1)
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

#[test]
fn the_source_type_checker_agrees_with_lowering_on_the_corpus() {
    let files = corpus();
    assert!(files.len() > 200, "corpus of {} files", files.len());
    let chunks: Vec<&[PathBuf]> = files.chunks(files.len().div_ceil(6)).collect();
    let outputs: Vec<(PathBuf, String)> = std::thread::scope(|scope| {
        let handles: Vec<_> = chunks
            .iter()
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|path| {
                            let out = Command::new(env!("CARGO_BIN_EXE_iris"))
                                .arg("check")
                                .arg(path)
                                .env("IRIS_TYPECK", "compare")
                                .output()
                                .expect("run iris check");
                            (
                                path.clone(),
                                String::from_utf8_lossy(&out.stderr).into_owned(),
                            )
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("worker"))
            .collect()
    });
    let mut t = Totals::default();
    for (path, err) in &outputs {
        let name = path.display().to_string();
        for line in err.lines() {
            if line.starts_with("typeck-stats:") {
                t.compiled += 1;
                t.expressions += number_after(line, "expressions");
                t.typed += number_after(line, "typed");
                if number_after(line, "reported") > 0 {
                    t.errors.push(format!("{name}: {line}"));
                }
            } else if line.starts_with("typeck-compare:") {
                t.agree += number_after(line, "agree");
                t.disagree += number_after(line, "disagree");
            } else if line.starts_with("typeck-error:") {
                t.errors.push(format!("{name}: {line}"));
            } else if line.starts_with("typeck-differ:") {
                t.differs.push(format!("{name}: {line}"));
            }
        }
    }
    let typed = 100.0 * t.typed as f64 / t.expressions.max(1) as f64;
    let agreement = 100.0 * t.agree as f64 / (t.agree + t.disagree).max(1) as f64;
    println!(
        "{} programs compiled; {typed:.1}% of {} expressions typed; agreement {agreement:.3}% ({} disagree)",
        t.compiled, t.expressions, t.disagree
    );
    // 242 of tests/ and examples/ compile today (the rest are meant to fail,
    // or need hardware); far fewer means the run itself broke.
    assert!(t.compiled > 200, "only {} programs compiled", t.compiled);
    assert!(
        t.errors.is_empty(),
        "the checker reported errors on valid programs:\n{}",
        t.errors.join("\n")
    );
    assert!(typed >= 90.0, "typed {typed:.1}% of expressions");
    assert!(
        agreement >= 99.9,
        "agreement {agreement:.3}%; disagreements:\n{}",
        t.differs
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
