//! Public learning material is an executable, classified release contract.
#[path = "support/learning.rs"]
mod learning;

use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;

fn catalog() -> Vec<Value> {
    serde_json::from_str::<Value>(include_str!("../examples/catalog.json"))
        .expect("valid learning catalog")["entries"]
        .as_array()
        .unwrap()
        .clone()
}

fn iris_files(dir: &Path, root: &Path, found: &mut BTreeSet<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            iris_files(&path, root, found);
        } else if path.extension().is_some_and(|ext| ext == "iris") {
            found.insert(
                path.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
}

#[test]
fn every_public_source_has_an_explicit_execution_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let entries = catalog();
    let mut actual = BTreeSet::new();
    iris_files(&root.join("examples"), root, &mut actual);
    iris_files(&root.join("projects"), root, &mut actual);
    let mut indexed = BTreeSet::new();
    for entry in entries {
        let path = entry["path"].as_str().unwrap();
        assert!(
            indexed.insert(path.to_owned()),
            "duplicate catalog path: {path}"
        );
        let mode = entry["mode"].as_str().unwrap();
        assert!(
            ["dual", "native", "host", "module", "embedded", "manual", "graph"].contains(&mode)
        );
        let source = std::fs::read_to_string(root.join(path)).unwrap();
        if ["dual", "native", "host", "manual"].contains(&mode) {
            assert!(source.contains("assert("), "{path} has no assertions");
            assert!(
                source.contains("return 0"),
                "{path} has no explicit success return"
            );
        }
    }
    assert_eq!(
        actual, indexed,
        "catalog must classify every public IRIS source"
    );
}

#[test]
fn public_programs_run_natively_without_interpreter_fallback() {
    for entry in catalog() {
        if ["dual", "native"].contains(&entry["mode"].as_str().unwrap()) {
            learning::run(
                entry["path"].as_str().unwrap(),
                "native",
                entry["expected"].as_str().unwrap(),
            );
        } else if entry["mode"] == "graph" {
            learning::run(
                entry["path"].as_str().unwrap(),
                "graph",
                entry["expected"].as_str().unwrap(),
            );
        }
    }
}

#[test]
fn public_programs_and_host_tools_run_in_the_interpreter() {
    for entry in catalog() {
        if ["dual", "host"].contains(&entry["mode"].as_str().unwrap()) {
            learning::run(
                entry["path"].as_str().unwrap(),
                "interpreter",
                entry["expected"].as_str().unwrap(),
            );
        }
    }
}

#[test]
fn service_and_sdk_entries_are_compile_checked() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for entry in catalog() {
        if entry["mode"] == "manual" {
            let path = entry["path"].as_str().unwrap();
            iris::compile_file_to_module(&root.join(path))
                .unwrap_or_else(|error| panic!("{path}: {error}"));
        }
    }
}
