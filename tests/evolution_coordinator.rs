use iris::codegen::llvm_orc::is_orc_jit_available;
use iris::evolution::audit::{read_verified_records, EvolutionAuditLog, EvolutionEventKind};
use iris::evolution::{
    compile_candidate, CanaryCase, EvolutionCoordinator, EvolutionGate, EvolutionPolicy,
    TrustedConstitution,
};

const BASELINE: &str = r#"
def policy(x: i64) -> i64 { x * 2 }
"#;

const CANDIDATE: &str = r#"
def policy(x: i64) -> i64 { x * 3 }

def test_policy() -> i64 effect throw {
    assert(policy(4) == 12);
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

#[test]
fn seven_gates_promote_then_generation_safe_rollback_is_audited() {
    if !is_orc_jit_available() {
        eprintln!("LLVM ORC is unavailable; evolution coordinator capability test skipped");
        return;
    }

    let baseline = compile_candidate(BASELINE, "evolution_baseline").unwrap();
    let candidate = compile_candidate(CANDIDATE, "evolution_candidate").unwrap();
    let coordinator = EvolutionCoordinator::new(EvolutionPolicy::default()).unwrap();
    let baseline_receipt = coordinator.install_baseline(&baseline).unwrap();

    let cases = vec![
        CanaryCase {
            input: 1,
            expected: 3,
        },
        CanaryCase {
            input: 4,
            expected: 12,
        },
        CanaryCase {
            input: 9,
            expected: 27,
        },
    ];
    let constitution = TrustedConstitution::pinned(
        b"IRIS test constitution: policy outputs must remain in [-1000, 1000]".to_vec(),
    );
    let constitution_allows = |_: i64, output: i64| (-1000..=1000).contains(&output);

    let audit_path = std::env::temp_dir().join(format!(
        "iris_evolution_coordinator_{}_audit.jsonl",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&audit_path);
    let mut audit = EvolutionAuditLog::open(&audit_path).unwrap();

    let receipt = coordinator
        .evaluate_and_promote(
            CANDIDATE,
            &candidate,
            &cases,
            &constitution,
            constitution_allows,
            Some(&mut audit),
        )
        .unwrap();
    assert_eq!(
        receipt
            .gates
            .iter()
            .map(|evidence| evidence.gate)
            .collect::<Vec<_>>(),
        EvolutionGate::ALL
    );
    assert_eq!(receipt.canary.candidate_exact, cases.len());
    assert_eq!(receipt.canary.baseline_exact, 0);
    assert_eq!(
        coordinator
            .hot_swap()
            .lease("policy")
            .unwrap()
            .call_i64_1(4)
            .unwrap(),
        12
    );

    let rolled_back = coordinator
        .observe_or_rollback(&receipt, 4, 5000, constitution_allows, Some(&mut audit))
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
            .call_i64_1(4)
            .unwrap(),
        8
    );

    let verified_head = audit.verify().unwrap();
    assert_eq!(verified_head, *audit.head());
    let records = read_verified_records(&audit_path).unwrap();
    assert_eq!(
        records
            .iter()
            .filter(|record| record.event.kind == EvolutionEventKind::GatePassed)
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
fn strict_candidate_compilation_rejects_undeclared_effects() {
    let error = compile_candidate(
        r#"def policy(x: i64) -> i64 { println("unsafe"); x }"#,
        "evolution_effect_rejection",
    )
    .expect_err("strict compilation must reject undeclared I/O");
    assert!(error.to_string().contains("effect"), "{error}");
}

#[test]
fn rejected_gate_is_named_in_the_tamper_evident_audit() {
    let candidate = compile_candidate(CANDIDATE, "evolution_resource_rejection").unwrap();
    let mut policy = EvolutionPolicy::default();
    policy.resources.max_source_bytes = 1;
    let coordinator = EvolutionCoordinator::new(policy).unwrap();
    let constitution = TrustedConstitution::pinned(b"test constitution".to_vec());
    let audit_path = std::env::temp_dir().join(format!(
        "iris_evolution_rejection_{}_audit.jsonl",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&audit_path);
    let mut audit = EvolutionAuditLog::open(&audit_path).unwrap();

    let error = coordinator
        .evaluate_and_promote(
            CANDIDATE,
            &candidate,
            &[CanaryCase {
                input: 1,
                expected: 3,
            }],
            &constitution,
            |_, _| true,
            Some(&mut audit),
        )
        .expect_err("resource violation must reject the candidate");
    assert!(error.to_string().contains("resource_and_abi"), "{error}");

    let records = read_verified_records(&audit_path).unwrap();
    let failed = records.last().unwrap();
    assert_eq!(failed.event.kind, EvolutionEventKind::GateFailed);
    assert_eq!(failed.event.gate.as_deref(), Some("resource_and_abi"));
    let _ = std::fs::remove_file(audit_path);
}
