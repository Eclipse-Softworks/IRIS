use iris::meta::{analyze_source, apply_checked_edit, emit_ir, META_ANALYSIS_SCHEMA};

#[test]
fn analysis_reports_typed_functions_effects_records_and_choices() {
    let source = r#"
pub record Point { x: f64, y: f64 }
pub choice Reading { Missing, Value(f64) }

pub def score[T](value: T, fallback: i64) -> i64 effect io {
    println("meta");
    return fallback
}

def main() -> i64 effect io { return score(1, 0) }
"#;
    let analysis = analyze_source(source, "meta_fixture");
    assert_eq!(analysis.schema, META_ANALYSIS_SCHEMA);
    assert!(analysis.valid, "{:?}", analysis.diagnostics);
    let score = analysis
        .functions
        .iter()
        .find(|function| function.name == "score")
        .unwrap();
    assert_eq!(score.return_type, "i64");
    assert_eq!(score.effects, ["io"]);
    assert_eq!(score.type_parameters, ["T"]);
    assert!(score.abi_sha256.is_some());
    assert_eq!(analysis.records[0].fields[1].ty, "f64");
    assert_eq!(analysis.choices[0].variants[1].payload, ["f64"]);
}

#[test]
fn invalid_program_returns_structured_diagnostic() {
    let analysis = analyze_source("def main() -> i64 { return missing }", "bad_meta");
    assert!(!analysis.valid);
    assert!(analysis
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.phase == "compile"));
}

#[test]
fn meta_ir_uses_the_real_compiler_pipeline() {
    let ir = emit_ir("def main() -> i64 { return 40 + 2 }", "meta_ir").unwrap();
    assert!(ir.contains("def main"), "{ir}");
    assert!(ir.contains("42"), "{ir}");
}

#[test]
fn checked_edit_returns_only_compiler_valid_source() {
    let source = "def main() -> i64 { return 0 }";
    let zero = source.find('0').unwrap();
    let edited = apply_checked_edit(source, zero, zero + 1, "42", "meta_edit").unwrap();
    assert!(edited.contains("return 42"));

    let error = apply_checked_edit(source, zero, zero + 1, "missing", "meta_bad_edit")
        .expect_err("invalid edit passed the compiler gate");
    assert!(error.to_string().contains("failed verification"));
}
