//! The `control_f64` evolution ABI: multi-input `f64` control laws promoted
//! through the same seven gates as the RC1 `(i64) -> i64` ABI, optionally
//! behind the IR-level allocation-free proof.

use iris::codegen::llvm_orc::is_orc_jit_available;
use iris::evolution::audit::{read_verified_records, EvolutionAuditLog, EvolutionEventKind};
use iris::evolution::{
    compile_candidate, ControlCanaryCase, EvolutionCoordinator, EvolutionGate, EvolutionPolicy,
    PolicyAbi, TrustedConstitution, ValidationBackend,
};
use iris::sandbox::{NativeSandbox, ResourceLimits};

const ROLLBACK_PROBE: &str = r#"
def test_transaction_rollback() -> i64 effect alloc, transaction {
    val state: list<i64> = list()
    push(state, 10);
    transaction_begin();
    push(state, 99);
    transaction_rollback();
    if list_len(state) == 1 && list_get(state, 0) == 10 { 0 } else { 1 }
}
"#;

/// Proportional-only baseline: ignores the error rate.
const BASELINE: &str = r#"
def policy(error: f64, rate: f64) -> f64 effect pure { 0.5 * error }
"#;

/// PD candidate split across a helper, so the proof covers more than one
/// function.
const PD_CANDIDATE: &str = r#"
def saturate(u: f64) -> f64 effect pure {
    if u > 10.0 { 10.0 } else { if u < -10.0 { -10.0 } else { u } }
}

def policy(error: f64, rate: f64) -> f64 effect pure {
    saturate(2.0 * error - 0.5 * rate)
}

def test_policy() -> i64 effect throw {
    assert(policy(1.0, 0.0) == 2.0);
    assert(policy(100.0, 0.0) == 10.0);
    0
}
"#;

fn source(candidate: &str) -> String {
    format!("{candidate}\n{ROLLBACK_PROBE}")
}

fn control_policy(require_allocation_free: bool) -> EvolutionPolicy {
    EvolutionPolicy {
        abi: PolicyAbi::ControlF64 { inputs: 2 },
        require_allocation_free,
        ..EvolutionPolicy::default()
    }
}

fn cases() -> Vec<ControlCanaryCase> {
    vec![
        ControlCanaryCase {
            inputs: vec![1.0, 0.0],
            expected: 2.0,
        },
        ControlCanaryCase {
            inputs: vec![2.0, 2.0],
            expected: 3.0,
        },
        ControlCanaryCase {
            inputs: vec![-1.0, 4.0],
            expected: -4.0,
        },
    ]
}

fn bounded(_: &[f64], output: f64) -> bool {
    (-10.0..=10.0).contains(&output)
}

fn temp_audit(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "iris_control_abi_{name}_{}_audit.jsonl",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    path
}

#[test]
fn allocation_free_pd_controller_is_promoted_then_rolled_back() {
    if !is_orc_jit_available() {
        eprintln!("LLVM ORC is unavailable; control ABI promotion test skipped");
        return;
    }
    let baseline = compile_candidate(BASELINE, "control_baseline").unwrap();
    let candidate_source = source(PD_CANDIDATE);
    let candidate = compile_candidate(&candidate_source, "control_candidate").unwrap();
    let coordinator = EvolutionCoordinator::new(control_policy(true)).unwrap();
    let baseline_receipt = coordinator.install_baseline(&baseline).unwrap();

    let audit_path = temp_audit("promote");
    let mut audit = EvolutionAuditLog::open(&audit_path).unwrap();
    let constitution = TrustedConstitution::pinned(b"actuator command within [-10, 10]".to_vec());
    let receipt = coordinator
        .evaluate_and_promote_control(
            &candidate_source,
            &candidate,
            &cases(),
            &constitution,
            bounded,
            Some(&mut audit),
        )
        .unwrap();

    assert_eq!(
        receipt.gates.iter().map(|g| g.gate).collect::<Vec<_>>(),
        EvolutionGate::ALL
    );
    let abi_evidence = &receipt.gates[1].detail;
    assert!(
        abi_evidence.contains("ABI (f64, f64) -> f64"),
        "{abi_evidence}"
    );
    assert!(
        abi_evidence.contains("allocation-free proof over 2 reachable functions"),
        "{abi_evidence}"
    );
    assert_eq!(receipt.canary.candidate_exact, 3);
    assert_eq!(receipt.canary.candidate_absolute_error, 0);
    assert!(receipt.canary.baseline_absolute_error > 0);

    // The promoted generation is now the native code behind the slot.
    let lease = coordinator.hot_swap().lease("policy").unwrap();
    assert_eq!(lease.call_f64_n(&[2.0, 2.0]).unwrap(), 3.0);
    assert_eq!(lease.call_f64_n(&[50.0, 0.0]).unwrap(), 10.0);
    drop(lease);

    // A non-finite field observation violates the constitution regardless of
    // the host callback, and rolls back to the baseline.
    let rolled_back = coordinator
        .observe_or_rollback_control(
            &receipt,
            &[1.0, 0.0],
            f64::NAN,
            |_, _| true,
            Some(&mut audit),
        )
        .unwrap();
    assert!(rolled_back);
    assert_eq!(
        coordinator.hot_swap().active_generation("policy"),
        Some(baseline_receipt.generation)
    );
    assert_eq!(
        coordinator
            .hot_swap()
            .lease("policy")
            .unwrap()
            .call_f64_n(&[2.0, 2.0])
            .unwrap(),
        1.0
    );

    let records = read_verified_records(&audit_path).unwrap();
    assert_eq!(
        records
            .iter()
            .filter(|r| r.event.kind == EvolutionEventKind::GatePassed)
            .count(),
        7
    );
    assert_eq!(
        records.last().unwrap().event.kind,
        EvolutionEventKind::RolledBack
    );
    let _ = std::fs::remove_file(audit_path);
}

#[test]
fn allocating_policy_fails_the_allocation_free_gate() {
    let allocating = source(
        r#"
def policy(error: f64, rate: f64) -> f64 effect alloc {
    val history: list<f64> = list()
    push(history, error);
    2.0 * error - 0.5 * rate
}

def test_policy() -> i64 effect throw, alloc {
    assert(policy(1.0, 0.0) == 2.0);
    0
}
"#,
    );
    let module = compile_candidate(&allocating, "control_allocating").unwrap();
    let coordinator = EvolutionCoordinator::new(control_policy(true)).unwrap();
    let audit_path = temp_audit("alloc");
    let mut audit = EvolutionAuditLog::open(&audit_path).unwrap();
    let error = coordinator
        .evaluate_and_promote_control(
            &allocating,
            &module,
            &cases(),
            &TrustedConstitution::pinned(b"c".to_vec()),
            bounded,
            Some(&mut audit),
        )
        .expect_err("a heap-allocating policy must not receive the proof");
    let message = error.to_string();
    assert!(message.contains("resource_and_abi"), "{message}");
    assert!(
        message.contains("allocation-free proof failed"),
        "{message}"
    );
    assert!(message.contains("dynamic collection"), "{message}");

    let failed = read_verified_records(&audit_path).unwrap().pop().unwrap();
    assert_eq!(failed.event.kind, EvolutionEventKind::GateFailed);
    assert_eq!(failed.event.gate.as_deref(), Some("resource_and_abi"));
    let _ = std::fs::remove_file(audit_path);
}

/// Non-tail recursion. (A tail-recursive helper is legitimately accepted:
/// `TailCallPass` has already turned it into a loop.)
#[test]
fn recursive_policy_fails_the_allocation_free_gate() {
    let recursive = source(
        r#"
@noinline
def series(x: f64, n: i64) -> f64 effect pure {
    if n == 0 { 0.0 } else { x + series(x * 0.5, n - 1) }
}

def policy(error: f64, rate: f64) -> f64 effect pure { series(error, 2) - rate }

def test_policy() -> i64 effect throw {
    assert(policy(4.0, 0.0) == 6.0);
    0
}
"#,
    );
    let module = compile_candidate(&recursive, "control_recursive").unwrap();
    let coordinator = EvolutionCoordinator::new(control_policy(true)).unwrap();
    let error = coordinator
        .evaluate_and_promote_control(
            &recursive,
            &module,
            &cases(),
            &TrustedConstitution::pinned(b"c".to_vec()),
            bounded,
            None,
        )
        .expect_err("unbounded stack growth must not receive the proof");
    assert!(
        error.to_string().contains("recursive call cycle"),
        "{error}"
    );
}

#[test]
fn signature_must_match_the_declared_control_abi() {
    let wrong_arity = source(
        r#"
def policy(error: f64) -> f64 effect pure { 2.0 * error }
def test_policy() -> i64 effect throw { assert(policy(1.0) == 2.0); 0 }
"#,
    );
    let module = compile_candidate(&wrong_arity, "control_wrong_arity").unwrap();
    let coordinator = EvolutionCoordinator::new(control_policy(false)).unwrap();
    let error = coordinator
        .evaluate_and_promote_control(
            &wrong_arity,
            &module,
            &cases(),
            &TrustedConstitution::pinned(b"c".to_vec()),
            bounded,
            None,
        )
        .expect_err("a one-input policy must not satisfy a two-input ABI");
    assert!(error.to_string().contains("(f64, f64) -> f64"), "{error}");
}

#[test]
fn nan_output_is_rejected_by_the_constitution_gate() {
    let nan = source(
        r#"
def policy(error: f64, rate: f64) -> f64 effect pure { sqrt(error) - rate }
def test_policy() -> i64 effect throw { assert(policy(4.0, 0.0) == 2.0); 0 }
"#,
    );
    let module = compile_candidate(&nan, "control_nan").unwrap();
    let coordinator = EvolutionCoordinator::new(control_policy(false)).unwrap();
    let error = coordinator
        .evaluate_and_promote_control(
            &nan,
            &module,
            &cases(), // includes error = -1.0, so sqrt yields NaN
            &TrustedConstitution::pinned(b"c".to_vec()),
            |_, _| true,
            None,
        )
        .expect_err("NaN must never reach an actuator");
    assert!(error.to_string().contains("safety_constitution"), "{error}");
}

#[test]
fn abi_entry_points_cannot_be_mixed() {
    let coordinator = EvolutionCoordinator::new(EvolutionPolicy::default()).unwrap();
    let module = compile_candidate(BASELINE, "control_mixed").unwrap();
    let error = coordinator
        .evaluate_and_promote_control(
            BASELINE,
            &module,
            &cases(),
            &TrustedConstitution::pinned(b"c".to_vec()),
            bounded,
            None,
        )
        .expect_err("a scalar_i64 policy has no control entry point");
    assert!(error.to_string().contains("(i64) -> i64"), "{error}");

    let mut policy = control_policy(false);
    policy.abi = PolicyAbi::ControlF64 { inputs: 9 };
    assert!(EvolutionCoordinator::new(policy).is_err());
}

fn sandboxed(mut policy: EvolutionPolicy) -> Option<EvolutionPolicy> {
    if !NativeSandbox::detect().availability().is_enforced() || !is_orc_jit_available() {
        eprintln!("enforced native sandbox or ORC unavailable; sandboxed validation test skipped");
        return None;
    }
    policy.validation = ValidationBackend::NativeSandbox {
        limits: ResourceLimits::default(),
    };
    Some(policy)
}

#[test]
fn sandboxed_native_validation_promotes_the_same_controller() {
    let Some(policy) = sandboxed(control_policy(true)) else {
        return;
    };
    let baseline = compile_candidate(BASELINE, "sandbox_baseline").unwrap();
    let candidate_source = source(PD_CANDIDATE);
    let candidate = compile_candidate(&candidate_source, "sandbox_candidate").unwrap();
    let coordinator = EvolutionCoordinator::new(policy).unwrap();
    coordinator.install_baseline(&baseline).unwrap();
    let receipt = coordinator
        .evaluate_and_promote_control(
            &candidate_source,
            &candidate,
            &cases(),
            &TrustedConstitution::pinned(b"c".to_vec()),
            bounded,
            None,
        )
        .unwrap();
    let tests_evidence = &receipt.gates[2].detail;
    assert!(
        tests_evidence.contains("sandboxed native run"),
        "{tests_evidence}"
    );
    // The bit-exact transport means native outputs match the expected values
    // exactly, just as the interpreter's did.
    assert_eq!(receipt.canary.candidate_exact, 3);
    assert_eq!(receipt.canary.candidate_absolute_error, 0);
}

#[test]
fn sandboxed_native_validation_rejects_a_failing_native_test() {
    let Some(policy) = sandboxed(control_policy(false)) else {
        return;
    };
    let failing = source(
        r#"
def policy(error: f64, rate: f64) -> f64 effect pure { 2.0 * error - 0.5 * rate }
def test_policy() -> i64 effect throw { assert(policy(1.0, 0.0) == 3.0); 0 }
"#,
    );
    let module = compile_candidate(&failing, "sandbox_failing").unwrap();
    let coordinator = EvolutionCoordinator::new(policy).unwrap();
    let error = coordinator
        .evaluate_and_promote_control(
            &failing,
            &module,
            &cases(),
            &TrustedConstitution::pinned(b"c".to_vec()),
            bounded,
            None,
        )
        .expect_err("a failing assertion in native code must reject the candidate");
    let message = error.to_string();
    assert!(message.contains("deterministic_tests"), "{message}");
    assert!(message.contains("in the sandbox"), "{message}");
    // The rejection must come from the failed assertion itself, not from the
    // harness failing to start (e.g. STATUS_DLL_NOT_FOUND = 0xc0000135).
    assert!(message.contains("assert"), "{message}");
    assert!(!message.contains("0xc0000135"), "{message}");
}

#[test]
fn default_policy_serialization_is_unchanged() {
    // Existing audit logs record a hash of the serialized policy; the new
    // fields must not alter it for policies that do not use them.
    let json = serde_json::to_string(&EvolutionPolicy::default()).unwrap();
    assert!(!json.contains("abi"), "{json}");
    assert!(!json.contains("require_allocation_free"), "{json}");
    let round_trip: EvolutionPolicy = serde_json::from_str(&json).unwrap();
    assert_eq!(round_trip, EvolutionPolicy::default());

    let control = serde_json::to_string(&control_policy(true)).unwrap();
    assert!(
        control.contains(r#""abi":{"kind":"control_f64","inputs":2}"#),
        "{control}"
    );
    assert_eq!(
        serde_json::from_str::<EvolutionPolicy>(&control).unwrap(),
        control_policy(true)
    );
}
