//! Every public standard-library function must be exercised by a test that
//! checks its result, or be listed in `tests/stdlib_untested.txt`.
//!
//! "Exercised" is measured cheaply: the function is called in a file under
//! `tests/` or `projects/` that asserts something. That is a floor, not
//! proof of correctness; a reference test (known answers from a published
//! vector, an independent implementation, or a hand-derived value) is the
//! standard to write to. The list may only shrink: a new public function
//! needs a test, and a listed function that gains one must leave the list.
//!
//! `IRIS_UPDATE_STDLIB_UNTESTED=1 cargo test --test stdlib_coverage`
//! rewrites the list from the current tree.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// `module.function` for every `pub def` in `src/stdlib/*.iris`.
fn public_functions() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let dir = root().join("src/stdlib");
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("iris") {
            continue;
        }
        let module = path.file_stem().unwrap().to_string_lossy().into_owned();
        let source = std::fs::read_to_string(&path).unwrap();
        for line in source.lines() {
            if let Some(rest) = line.strip_prefix("pub def ") {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() {
                    out.insert(format!("{module}.{name}"));
                }
            }
        }
    }
    out
}

/// An asserting source file: its text, and whether it is Rust (whose own
/// method calls, `.len()`, must not count as calls to the stdlib).
struct Source {
    text: String,
    rust: bool,
}

fn collect(dir: &Path, out: &mut Vec<Source>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if path.is_dir() {
            collect(&path, out);
        } else if (ext == "iris" || ext == "rs") && !path.ends_with("stdlib_coverage.rs") {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if text.contains("assert") {
                    out.push(Source {
                        text,
                        rust: ext == "rs",
                    });
                }
            }
        }
    }
}

/// Whether `name(` occurs as a call, not as the tail of a longer name. In
/// IRIS `x.name()` is a call too (extension methods); in Rust it is not ours.
fn calls(source: &Source, name: &str) -> bool {
    let text = source.text.as_str();
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(at) = text[from..].find(name) {
        let start = from + at;
        let end = start + name.len();
        let before_ok = start == 0
            || !(bytes[start - 1].is_ascii_alphanumeric()
                || bytes[start - 1] == b'_'
                || (source.rust && bytes[start - 1] == b'.'));
        let rest = text[end..].trim_start_matches([' ', '\t']);
        if before_ok && rest.starts_with('(') {
            return true;
        }
        from = end;
    }
    false
}

fn untested_now() -> BTreeSet<String> {
    let mut texts = Vec::new();
    collect(&root().join("tests"), &mut texts);
    collect(&root().join("projects"), &mut texts);
    public_functions()
        .into_iter()
        .filter(|qualified| {
            let name = qualified.split_once('.').unwrap().1;
            !texts.iter().any(|t| calls(t, name))
        })
        .collect()
}

fn listed() -> BTreeSet<String> {
    std::fs::read_to_string(root().join("tests/stdlib_untested.txt"))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

#[test]
fn every_public_stdlib_function_is_tested_or_listed() {
    let now = untested_now();
    if std::env::var("IRIS_UPDATE_STDLIB_UNTESTED").is_ok() {
        let mut text = String::from(
            "# Public stdlib functions no asserting test calls yet. This list may only\n\
             # shrink; see tests/stdlib_coverage.rs.\n",
        );
        for f in &now {
            text.push_str(f);
            text.push('\n');
        }
        std::fs::write(root().join("tests/stdlib_untested.txt"), text).unwrap();
        return;
    }
    let listed = listed();
    let new: Vec<_> = now.difference(&listed).collect();
    assert!(
        new.is_empty(),
        "public stdlib functions with no asserting test (add a test that checks the result):\n  {}",
        new.iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}

#[test]
fn the_untested_list_is_accurate() {
    let now = untested_now();
    let stale: Vec<_> = listed().difference(&now).cloned().collect();
    assert!(
        stale.is_empty(),
        "these are now tested or no longer exist; remove them from tests/stdlib_untested.txt:\n  {}",
        stale.join("\n  ")
    );
}
