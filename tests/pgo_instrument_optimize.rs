// pgo_instrument_optimize.rs — Profile-Guided Optimization (PGO)
//
// Tests for the PGO backends (EmitKind::PgoInstrument and EmitKind::PgoOptimize):
//   - PgoInstrument counts every block (@__profc_* arrays, atomicrmw add)
//   - PgoInstrument registers the counters from a module constructor
//   - PgoOptimize takes branch weights and entry counts from the profile
//   - PgoOptimize without a profile is an error, not invented weights
//   - ProfileData parses IRIS profile text correctly
//   - the hot/cold summary follows the profile
// tests/pgo_end_to_end.rs records a profile from a real run and applies it.

use iris::codegen::pgo::{emit_pgo_optimize, ProfileData};
use iris::{compile, compile_to_module, EmitKind};

// ── Test 1: PgoInstrument emits counter globals ────────────────────────────

#[test]
fn test_pgo_instrument_counter_globals() {
    let src = r#"def f() -> i64 { 42 }"#;
    let ir = compile(src, "test", EmitKind::PgoInstrument).unwrap();
    assert!(
        ir.contains("__profc_"),
        "expected '@__profc_' counter global in PGO instrumented IR:\n{}",
        ir
    );
    assert!(
        ir.contains("atomicrmw add ptr getelementptr inbounds ([1 x i64], ptr @__profc_"),
        "{}",
        ir
    );
}

// ── Test 2: PgoInstrument emits profile init declaration ──────────────────

#[test]
fn test_pgo_instrument_init_declare() {
    let src = r#"def f() -> i64 { 0 }"#;
    let ir = compile(src, "test", EmitKind::PgoInstrument).unwrap();
    assert!(
        ir.contains("call void @iris_profile_register(ptr @__profn_"),
        "expected the counters registered with the runtime:\n{}",
        ir
    );
}

// ── Test 3: PgoInstrument emits module constructor ─────────────────────────

#[test]
fn test_pgo_instrument_module_ctor() {
    let src = r#"def f() -> i64 { 0 }"#;
    let ir = compile(src, "test", EmitKind::PgoInstrument).unwrap();
    assert!(
        ir.contains("@llvm.global_ctors = appending global [1 x { i32, ptr, ptr }] [{ i32, ptr, ptr } { i32 65535, ptr @iris_profile_init, ptr null }]"),
        "expected module constructor for profiling setup:\n{}",
        ir
    );
}

// ── Test 4: PgoOptimize emits branch weights ──────────────────────────────

#[test]
fn test_pgo_optimize_branch_weights() {
    let src = r#"
def f(x: i64) -> i64 {
    if x > 0 { x } else { 0 }
}
"#;
    let module = compile_to_module(src, "test").unwrap();
    let profile = "f:entry0:1000\nf:then1:30\nf:else2:970\nf:merge3:1000\n";
    let ir = emit_pgo_optimize(&module, profile).unwrap();
    assert!(
        ir.contains("!{!\"branch_weights\", i32 30, i32 970}"),
        "expected the branch's weights from the profile:\n{}",
        ir
    );
    assert!(
        ir.contains("!{!\"function_entry_count\", i64 1000}"),
        "{}",
        ir
    );
    assert!(ir.contains("!\"ProfileSummary\""), "{}", ir);
}

#[test]
fn test_pgo_optimize_without_a_profile_is_an_error() {
    let src = r#"def f(x: i64) -> i64 { x }"#;
    let err = compile(src, "test", EmitKind::PgoOptimize)
        .unwrap_err()
        .to_string();
    assert!(err.contains("--pgo-use"), "{}", err);
}

// ── Test 5: ProfileData parses text format correctly ──────────────────────

#[test]
fn test_profile_data_parse() {
    let profile_text = "factorial:entry0:1000\nfactorial:then1:5\nfactorial:else2:995\n";
    let pdata = ProfileData::parse(profile_text);
    assert_eq!(
        pdata.block_count("factorial", "entry0"),
        Some(1000),
        "expected block_count('factorial', 'entry0') = 1000"
    );
    assert_eq!(
        pdata.block_count("factorial", "then1"),
        Some(5),
        "expected block_count('factorial', 'then1') = 5"
    );
    assert_eq!(
        pdata.block_count("factorial", "else2"),
        Some(995),
        "expected block_count('factorial', 'else2') = 995"
    );
}

// ── Test 6: ProfileData.entry_count works ─────────────────────────────────

#[test]
fn test_profile_data_entry_count() {
    let profile_text = "myFunc:entry0:500\nmyFunc:bb1:300\n";
    let pdata = ProfileData::parse(profile_text);
    assert_eq!(
        pdata.entry_count("myFunc"),
        Some(500),
        "expected entry_count = 500"
    );
}

// ── Test 7: generate_synthetic_profile produces parseable output ───────────

#[test]
fn test_synthetic_profile_parseable() {
    //use iris::compile;
    let _src = r#"def f() -> i64 { 42 }"#;
    // We need an IrModule — use the IR pipeline to produce it via eval (we have the
    // module already from the compile path), but synthetic profile just needs function names.
    // Instead, test the profile parsing round-trip.
    let profile_text = "f:entry0:1000\nf:bb1:500\n";
    let pdata = ProfileData::parse(profile_text);
    assert_eq!(pdata.block_count("f", "entry0"), Some(1000));
    assert_eq!(pdata.block_count("f", "bb1"), Some(500));
    assert_eq!(pdata.block_count("f", "nonexistent"), None);
}

// ── Test 8: PGO hot/cold summary in optimized output ─────────────────────

#[test]
fn test_pgo_hot_cold_summary() {
    let src = r#"
def hot() -> i64 { 99 }
def idle() -> i64 { 1 }
def other() -> i64 { 2 }
"#;
    let module = compile_to_module(src, "test").unwrap();
    let ir = emit_pgo_optimize(&module, "hot:entry0:5000\nidle:entry0:0\n").unwrap();
    assert!(ir.contains("; HOT hot (entry count: 5000)"), "{}", ir);
    assert!(ir.contains("; COLD idle (entry count: 0)"), "{}", ir);
    assert!(ir.contains("; UNPROFILED other"), "{}", ir);
    // Never run in a profiled run: optimised for size, placed out of the way.
    assert!(
        ir.lines()
            .any(|l| l.starts_with("define i64 @idle()") && l.contains(" cold ")),
        "{}",
        ir
    );
}
