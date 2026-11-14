//! Governed whole-program evolution: several gateways, program state
//! (`var` at the top level), scenario-based gates, and JSON state handoff
//! from the live generation to the promoted one.

use iris::codegen::llvm_orc::is_orc_jit_available;
use iris::evolution::audit::{read_verified_records, EvolutionAuditLog, EvolutionEventKind};
use iris::evolution::manifest::{
    HostAbi, ProgramEntrypoint, ProgramManifest, PROGRAM_MANIFEST_SCHEMA,
};
use iris::evolution::program::{json_error, ProgramCoordinator, ProgramPolicy, Scenario};
use iris::evolution::{compile_candidate, EvolutionError, EvolutionGate, TrustedConstitution};
use serde_json::json;

const SUPPORT: &str = r#"
def test_program() -> i64 effect throw {
    val before = count;
    val n = add(3);
    assert(n == before + 1);
    0
}

def test_transaction_rollback() -> i64 effect alloc, transaction, throw {
    val state: list<i64> = list(10);
    transaction_begin();
    list_push(state, 99);
    transaction_rollback();
    assert(list_len(state) == 1);
    0
}

def status(request: str) -> str effect alloc, io {
    concat("{\"count\":", concat(to_str(count), "}"))
}
"#;

/// The baseline's `mean` is wrong: it returns the running total.
const BASELINE: &str = r#"
var count: i64 = 0
var total: f64 = 0.0

def add(x: i64) -> i64 {
    count = count + 1;
    total = total + (x to f64);
    count
}

def mean() -> i64 { total to i64 }
"#;

/// The candidate fixes `mean` and starts tracking the peak.
const CANDIDATE: &str = r#"
var count: i64 = 0
var total: f64 = 0.0
var peak: i64 = 0

def add(x: i64) -> i64 {
    count = count + 1;
    total = total + (x to f64);
    if x > peak { peak = x };
    count
}

def mean() -> i64 {
    if count == 0 { 0 } else { (total / (count to f64)) to i64 }
}
"#;

fn program(body: &str) -> String {
    format!("{body}\n{SUPPORT}")
}

fn manifest() -> ProgramManifest {
    ProgramManifest {
        schema: PROGRAM_MANIFEST_SCHEMA.to_owned(),
        program_id: "running_mean".to_owned(),
        entrypoints: vec![
            ProgramEntrypoint {
                gateway: "add".to_owned(),
                symbol: "add".to_owned(),
                abi: HostAbi::ScalarI64V1,
            },
            ProgramEntrypoint {
                gateway: "mean".to_owned(),
                symbol: "mean".to_owned(),
                abi: HostAbi::CommandV1,
            },
            ProgramEntrypoint {
                gateway: "status".to_owned(),
                symbol: "status".to_owned(),
                abi: HostAbi::JsonV1,
            },
        ],
        state: None,
    }
}

fn scenarios() -> Vec<Scenario> {
    serde_json::from_value(json!([
        {
            "name": "fresh",
            "steps": [
                { "gateway": "add", "input": 4, "expected": 1 },
                { "gateway": "add", "input": 6, "expected": 2 },
                { "gateway": "mean", "expected": 5 },
                { "gateway": "status", "input": {}, "expected": { "count": 2 } }
            ]
        },
        {
            "name": "resumed",
            "state": { "count": 3, "total": 30.0 },
            "steps": [
                { "gateway": "mean", "expected": 10 },
                { "gateway": "add", "input": 10, "expected": 4 },
                { "gateway": "mean", "expected": 10 }
            ]
        }
    ]))
    .unwrap()
}

/// Every numeric result stays within [-1000, 1000].
fn bounded(_: &iris::evolution::program::ScenarioStep, output: &serde_json::Value) -> bool {
    fn ok(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::Number(n) => n.as_f64().is_some_and(|v| v.abs() <= 1000.0),
            serde_json::Value::Array(items) => items.iter().all(ok),
            serde_json::Value::Object(map) => map.values().all(ok),
            _ => true,
        }
    }
    ok(output)
}

#[test]
fn program_is_promoted_with_its_live_state_and_rolled_back() {
    if !is_orc_jit_available() {
        eprintln!("LLVM ORC is unavailable; program evolution test skipped");
        return;
    }
    let coordinator = ProgramCoordinator::new(ProgramPolicy::default(), manifest()).unwrap();
    let baseline = compile_candidate(&program(BASELINE), "program_baseline").unwrap();
    let baseline_receipt = coordinator.install_baseline(baseline).unwrap();

    // The baseline serves for a while, building up live state.
    let lease = coordinator.lease().unwrap();
    assert_eq!(lease.call_i64_1("add", 10).unwrap(), 1);
    assert_eq!(lease.call_i64_1("add", 20).unwrap(), 2);
    assert_eq!(lease.call_i64_0("mean").unwrap(), 30);
    drop(lease);

    let audit_path = std::env::temp_dir().join(format!(
        "iris_program_evolution_{}_audit.jsonl",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&audit_path);
    let mut audit = EvolutionAuditLog::open(&audit_path).unwrap();
    let source = program(CANDIDATE);
    let candidate = compile_candidate(&source, "program_candidate").unwrap();
    let receipt = coordinator
        .evaluate_and_promote(
            &source,
            candidate,
            &scenarios(),
            &TrustedConstitution::pinned(b"|result| <= 1000".to_vec()),
            bounded,
            Some(&mut audit),
        )
        .unwrap();
    assert_eq!(
        receipt.gates.iter().map(|g| g.gate).collect::<Vec<_>>(),
        EvolutionGate::ALL
    );
    assert_eq!(receipt.metrics.compared_steps, 7);
    assert_eq!(receipt.metrics.candidate_exact, 7);
    assert!(receipt.metrics.baseline_exact < 7);
    assert_eq!(
        receipt.handoff.carried,
        vec!["count".to_owned(), "total".to_owned()]
    );
    assert_eq!(receipt.handoff.initialised, vec!["peak".to_owned()]);
    assert!(receipt.handoff.dropped.is_empty());

    // The promoted generation continues from the live state, not from the
    // scenarios' state: 2 observations totalling 30.
    let lease = coordinator.lease().unwrap();
    assert_eq!(lease.call_i64_0("mean").unwrap(), 15);
    assert_eq!(lease.call_str_1("status", "{}").unwrap(), "{\"count\":2}");
    assert_eq!(lease.call_i64_1("add", 60).unwrap(), 3);
    assert_eq!(lease.call_i64_0("mean").unwrap(), 30);
    let state = lease.read_state().unwrap();
    assert_eq!(state.len(), 3);
    drop(lease);

    // Rolling back restores the baseline generation with the state it had.
    assert_eq!(
        coordinator.rollback(&receipt).unwrap(),
        baseline_receipt.generation
    );
    let lease = coordinator.lease().unwrap();
    assert_eq!(lease.call_i64_0("mean").unwrap(), 30);
    assert_eq!(lease.call_i64_1("add", 1).unwrap(), 3);
    drop(lease);

    let records = read_verified_records(&audit_path).unwrap();
    assert_eq!(
        records.last().unwrap().event.kind,
        EvolutionEventKind::Promoted
    );
    assert!(records
        .last()
        .unwrap()
        .event
        .evidence
        .contains_key("state_snapshot_sha256"));
    let _ = std::fs::remove_file(audit_path);
}

#[test]
fn a_state_variable_may_not_change_type() {
    if !is_orc_jit_available() {
        return;
    }
    let coordinator = ProgramCoordinator::new(ProgramPolicy::default(), manifest()).unwrap();
    coordinator
        .install_baseline(compile_candidate(&program(BASELINE), "type_baseline").unwrap())
        .unwrap();
    let retyped = program(
        &CANDIDATE
            .replace("var total: f64 = 0.0", "var total: i64 = 0")
            .replace("total = total + (x to f64);", "total = total + x;")
            .replace("(total / (count to f64)) to i64", "total / count"),
    );
    let error = coordinator
        .evaluate_and_promote(
            &retyped,
            compile_candidate(&retyped, "type_candidate").unwrap(),
            &scenarios(),
            &TrustedConstitution::pinned(b"any".to_vec()),
            bounded,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(
            &error,
            EvolutionError::Gate {
                gate: EvolutionGate::ResourceAndAbi,
                ..
            }
        ),
        "{error}"
    );
    assert!(
        error.to_string().contains("changes type from f64 to i64"),
        "{error}"
    );
}

#[test]
fn the_constitution_judges_every_scenario_step() {
    if !is_orc_jit_available() {
        return;
    }
    let coordinator = ProgramCoordinator::new(ProgramPolicy::default(), manifest()).unwrap();
    coordinator
        .install_baseline(compile_candidate(&program(BASELINE), "judge_baseline").unwrap())
        .unwrap();
    let source = program(CANDIDATE);
    let error = coordinator
        .evaluate_and_promote(
            &source,
            compile_candidate(&source, "judge_candidate").unwrap(),
            &scenarios(),
            &TrustedConstitution::pinned(b"means stay below 8".to_vec()),
            |step, output| step.gateway != "mean" || output.as_i64().is_some_and(|m| m < 8),
            None,
        )
        .unwrap_err();
    assert!(
        matches!(
            &error,
            EvolutionError::Gate {
                gate: EvolutionGate::SafetyConstitution,
                ..
            }
        ),
        "{error}"
    );
    assert!(error.to_string().contains("scenario 'resumed'"), "{error}");
}

#[test]
fn json_results_are_compared_structurally() {
    assert_eq!(
        json_error(&json!({"a": 1, "b": [1, 2]}), &json!({"a": 1, "b": [1, 2]})),
        0
    );
    assert_eq!(json_error(&json!(1.5), &json!(1.0)), 500_000_000);
    assert_eq!(
        json_error(&json!({"a": 1}), &json!({"a": 1, "b": 2})),
        1_000_000_000
    );
    assert_eq!(json_error(&json!("x"), &json!("y")), 1_000_000_000);
}

#[test]
fn a_state_type_handoff_does_not_carry_is_reinitialised() {
    if !is_orc_jit_available() {
        return;
    }
    let with_map = |body: &str| program(&format!("var seen: map<str, i64> = map()\n{body}"));
    let coordinator = ProgramCoordinator::new(ProgramPolicy::default(), manifest()).unwrap();
    coordinator
        .install_baseline(compile_candidate(&with_map(BASELINE), "map_baseline").unwrap())
        .unwrap();
    let source = with_map(CANDIDATE);
    let receipt = coordinator
        .evaluate_and_promote(
            &source,
            compile_candidate(&source, "map_candidate").unwrap(),
            &scenarios(),
            &TrustedConstitution::pinned(b"|result| <= 1000".to_vec()),
            bounded,
            None,
        )
        .unwrap();
    assert_eq!(receipt.handoff.reinitialised, vec!["seen".to_owned()]);
    assert_eq!(
        receipt.handoff.carried,
        vec!["count".to_owned(), "total".to_owned()]
    );
    assert!(
        receipt.gates[1].detail.contains("re-initialises seen"),
        "{}",
        receipt.gates[1].detail
    );
}
