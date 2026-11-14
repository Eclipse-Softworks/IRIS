//! The `record` evolution ABI (records of f64/i64/bool fields in and out of
//! an evolved policy), and genetic search whose winner is promoted through
//! the same seven gates as a hand-written candidate.

use iris::codegen::llvm_orc::is_orc_jit_available;
use iris::evolution::audit::{read_verified_records, EvolutionAuditLog, EvolutionEventKind};
use iris::evolution::float_search::{self, FloatSearchConfig};
use iris::evolution::{
    compile_candidate, ControlCanaryCase, EvolutionCoordinator, EvolutionError, EvolutionGate,
    EvolutionPolicy, PolicyAbi, RecordCanaryCase, RecordData, RecordField, TrustedConstitution,
};
use serde_json::json;

const SUPPORT: &str = r#"
record CartState { position: f64, velocity: f64, step: i64 }
record Command { force: f64, brake: bool }

def test_policy() -> i64 effect throw {
    val a = policy(CartState { position: 0.5, velocity: 0.0, step: 3 });
    val b = policy(CartState { position: 0.5, velocity: 0.0, step: 3 });
    assert(a.force == b.force);
    0
}

def test_transaction_rollback() -> i64 effect alloc, transaction {
    val state: list<i64> = list()
    push(state, 10);
    transaction_begin();
    push(state, 99);
    transaction_rollback();
    if list_len(state) == 1 && list_get(state, 0) == 10 { 0 } else { 1 }
}
"#;

const BASELINE: &str = r#"
def policy(s: CartState) -> Command {
    Command { force: 0.0, brake: false }
}
"#;

const CANDIDATE: &str = r#"
def policy(s: CartState) -> Command {
    val force = -1.5 * s.position - 0.4 * s.velocity;
    Command { force: force, brake: s.step > 100 }
}
"#;

fn program(policy: &str) -> String {
    format!("{SUPPORT}\n{policy}")
}

fn record_policy() -> EvolutionPolicy {
    EvolutionPolicy {
        abi: PolicyAbi::Record,
        ..EvolutionPolicy::default()
    }
}

fn cases() -> Vec<RecordCanaryCase> {
    let mut cases = Vec::new();
    for position in [-2.0, 0.0, 1.0] {
        for (velocity, step) in [(0.0, 1), (1.0, 200)] {
            let input = json!({ "position": position, "velocity": velocity, "step": step });
            let expected =
                json!({ "force": -1.5 * position - 0.4 * velocity, "brake": step > 100 });
            cases.push(RecordCanaryCase {
                input: input.as_object().unwrap().clone(),
                expected: expected.as_object().unwrap().clone(),
            });
        }
    }
    cases
}

fn force_within(limit: f64) -> impl Fn(&RecordData, &RecordData) -> bool {
    move |_, output| matches!(output.get("force"), Some(RecordField::F64(f)) if f.abs() <= limit)
}

fn temp_audit(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "iris_record_abi_{name}_{}_audit.jsonl",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    path
}

#[test]
fn record_policy_is_promoted_served_natively_then_rolled_back() {
    if !is_orc_jit_available() {
        eprintln!("LLVM ORC is unavailable; record ABI promotion test skipped");
        return;
    }
    let baseline = compile_candidate(&program(BASELINE), "record_baseline").unwrap();
    let candidate_source = program(CANDIDATE);
    let candidate = compile_candidate(&candidate_source, "record_candidate").unwrap();
    let coordinator = EvolutionCoordinator::new(record_policy()).unwrap();
    let baseline_receipt = coordinator.install_baseline(&baseline).unwrap();

    let audit_path = temp_audit("promote");
    let mut audit = EvolutionAuditLog::open(&audit_path).unwrap();
    let constitution = TrustedConstitution::pinned(b"|force| <= 10".to_vec());
    let receipt = coordinator
        .evaluate_and_promote_record(
            &candidate_source,
            &candidate,
            &cases(),
            &constitution,
            force_within(10.0),
            Some(&mut audit),
        )
        .unwrap();
    assert_eq!(
        receipt.gates.iter().map(|g| g.gate).collect::<Vec<_>>(),
        EvolutionGate::ALL
    );
    assert_eq!(receipt.canary.candidate_exact, 6);
    assert_eq!(receipt.canary.candidate_absolute_error, 0);
    assert!(receipt.canary.baseline_absolute_error > 0);

    // The promoted generation serves native calls with records.
    let lease = coordinator.hot_swap().lease("policy").unwrap();
    let served = lease
        .call_record(&[
            RecordField::F64(2.0),
            RecordField::F64(1.0),
            RecordField::I64(500),
        ])
        .unwrap();
    assert_eq!(
        served,
        vec![RecordField::F64(-3.4), RecordField::Bool(true)]
    );
    drop(lease);

    // A non-finite field violates the constitution whatever the host says.
    let input = RecordData {
        fields: vec![
            ("position".into(), RecordField::F64(1.0)),
            ("velocity".into(), RecordField::F64(0.0)),
            ("step".into(), RecordField::I64(1)),
        ],
    };
    let output = RecordData {
        fields: vec![
            ("force".into(), RecordField::F64(f64::NAN)),
            ("brake".into(), RecordField::Bool(false)),
        ],
    };
    let rolled_back = coordinator
        .observe_or_rollback_record(&receipt, &input, &output, |_, _| true, Some(&mut audit))
        .unwrap();
    assert!(rolled_back);
    assert_eq!(
        coordinator.hot_swap().active_generation("policy"),
        Some(baseline_receipt.generation)
    );
    let records = read_verified_records(&audit_path).unwrap();
    assert_eq!(
        records.last().unwrap().event.kind,
        EvolutionEventKind::RolledBack
    );
    let _ = std::fs::remove_file(audit_path);
}

#[test]
fn record_constitution_judges_fields() {
    if !is_orc_jit_available() {
        return;
    }
    let baseline = compile_candidate(&program(BASELINE), "record_baseline_2").unwrap();
    let candidate_source = program(CANDIDATE);
    let candidate = compile_candidate(&candidate_source, "record_candidate_2").unwrap();
    let coordinator = EvolutionCoordinator::new(record_policy()).unwrap();
    coordinator.install_baseline(&baseline).unwrap();
    let error = coordinator
        .evaluate_and_promote_record(
            &candidate_source,
            &candidate,
            &cases(),
            &TrustedConstitution::pinned(b"|force| <= 1".to_vec()),
            force_within(1.0),
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
    // The rejected output is reported field by field.
    assert!(
        error.to_string().contains("output {\"force\": 3.0"),
        "{error}"
    );
}

#[test]
fn record_fields_must_cross_the_boundary() {
    let with_text = program(
        r#"
record Labelled { force: f64, label: str }
def policy(s: CartState) -> Labelled {
    Labelled { force: s.position, label: "x" }
}
"#,
    );
    let module = compile_candidate(&with_text, "record_with_text").unwrap();
    let coordinator = EvolutionCoordinator::new(record_policy()).unwrap();
    let error = coordinator.install_baseline(&module).unwrap_err();
    assert!(
        error.to_string().contains("f64, i64 and bool fields only"),
        "{error}"
    );
}

#[test]
fn record_policies_cannot_claim_the_allocation_free_proof() {
    let error = EvolutionCoordinator::new(EvolutionPolicy {
        abi: PolicyAbi::Record,
        require_allocation_free: true,
        ..EvolutionPolicy::default()
    })
    .err()
    .expect("rejected");
    assert!(error.to_string().contains("allocation-free"), "{error}");
}

#[test]
fn record_abi_serializes_as_a_tagged_kind() {
    let policy = serde_json::to_value(record_policy()).unwrap();
    assert_eq!(policy["abi"], json!({ "kind": "record" }));
}

#[test]
fn search_winner_is_promoted_through_the_gates() {
    if !is_orc_jit_available() {
        eprintln!("LLVM ORC is unavailable; search promotion test skipped");
        return;
    }
    // Target law: u = -1.5 e - 0.4 r.
    let cases: Vec<ControlCanaryCase> = (-4..=4)
        .flat_map(|i| (-2..=2).map(move |j| (i as f64 * 0.5, j as f64 * 0.7)))
        .map(|(e, r)| ControlCanaryCase {
            inputs: vec![e, r],
            expected: -1.5 * e - 0.4 * r,
        })
        .collect();
    let result = float_search::search(
        &FloatSearchConfig {
            inputs: 2,
            population_size: 120,
            generations: 20,
            seed: Some(5),
            ..FloatSearchConfig::default()
        },
        &cases,
        |_| {},
    )
    .unwrap();
    assert!(result.score.mean_absolute_error < 1e-6, "{result:?}");

    let source = result.best.to_iris_source("policy", 2);
    let candidate = compile_candidate(&source, "search_winner").unwrap();
    let baseline = compile_candidate(
        "def policy(e: f64, r: f64) -> f64 { 0.0 }",
        "search_baseline",
    )
    .unwrap();
    let coordinator = EvolutionCoordinator::new(EvolutionPolicy {
        abi: PolicyAbi::ControlF64 { inputs: 2 },
        // The rendered control law is pure scalar arithmetic.
        require_allocation_free: true,
        ..EvolutionPolicy::default()
    })
    .unwrap();
    coordinator.install_baseline(&baseline).unwrap();
    let receipt = coordinator
        .evaluate_and_promote_control(
            &source,
            &candidate,
            &cases,
            &TrustedConstitution::pinned(b"|u| <= 10".to_vec()),
            |_, u| u.abs() <= 10.0,
            None,
        )
        .unwrap();
    assert_eq!(receipt.gates.len(), 7);
    assert_eq!(receipt.canary.candidate_exact, cases.len());
    let lease = coordinator.hot_swap().lease("policy").unwrap();
    assert!((lease.call_f64_n(&[1.0, 1.0]).unwrap() + 1.9).abs() < 1e-9);
}
