use iris::codegen::hot_swap::{
    parse_retention_spec, AbiFingerprint, HotSwapConfig, HotSwapEngine, ProgramGateway,
};
use iris::codegen::llvm_orc::is_orc_jit_available;
use iris::compile_to_module;
use std::sync::Mutex;

// LLVM target/ORC initialization is process-global on the supported Windows
// toolchain. Serialize native module compilation so the default parallel Rust
// test harness cannot spin several LLVM initializers at once.
static ORC_TEST_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn abi_fingerprint_is_stable_and_content_addressed() {
    let first = compile_to_module("def value() -> i64 { 1 }", "fingerprint_one").unwrap();
    let second = compile_to_module("def value() -> i64 { 999 }", "fingerprint_two").unwrap();
    let first = AbiFingerprint::for_function(first.function_by_name("value").unwrap());
    let second = AbiFingerprint::for_function(second.function_by_name("value").unwrap());

    assert_eq!(first, second, "function bodies must not change the ABI");
    assert_eq!(
        first.sha256,
        "420deb23e69869fff7715beb8fb620dd8132af9e3eef86f230632a3c510bab58"
    );
    assert_eq!(first.hash, 0x420d_eb23_e698_69ff);
    assert_eq!(first.signature, "() -> i64");
}

#[test]
fn swaps_rolls_back_and_keeps_in_flight_generation_alive() {
    let _guard = ORC_TEST_LOCK.lock().unwrap();
    if !is_orc_jit_available() {
        eprintln!("LLVM ORC is unavailable; capability test skipped");
        return;
    }

    let first = compile_to_module("def value() -> i64 { 1 }", "swap_one").unwrap();
    let second = compile_to_module("def value() -> i64 { 2 }", "swap_two").unwrap();
    let swaps = HotSwapEngine::new();

    let installed = swaps
        .install_verified(&first, "value", |candidate| {
            assert_eq!(candidate.call_i64_0("value")?, 1);
            Ok(())
        })
        .expect("install first generation");
    let in_flight = swaps.lease("value").expect("lease first generation");

    let replacement = swaps
        .install_verified(&second, "value", |candidate| {
            assert_eq!(candidate.call_i64_0("value")?, 2);
            Ok(())
        })
        .expect("install second generation");
    assert_eq!(replacement.replaced_generation, Some(installed.generation));
    assert_eq!(swaps.lease("value").unwrap().call_i64_0().unwrap(), 2);
    assert_eq!(in_flight.call_i64_0().unwrap(), 1);

    let rollback = swaps
        .rollback_generation("value", replacement.generation)
        .expect("roll back matching active generation");
    assert_eq!(rollback.generation, installed.generation);
    assert_eq!(swaps.lease("value").unwrap().call_i64_0().unwrap(), 1);
}

#[test]
fn rejects_rollback_for_a_stale_generation() {
    let _guard = ORC_TEST_LOCK.lock().unwrap();
    if !is_orc_jit_available() {
        eprintln!("LLVM ORC is unavailable; capability test skipped");
        return;
    }

    let first = compile_to_module("def value() -> i64 { 1 }", "stale_one").unwrap();
    let second = compile_to_module("def value() -> i64 { 2 }", "stale_two").unwrap();
    let swaps = HotSwapEngine::new();
    let installed = swaps
        .install_verified(&first, "value", |_| Ok(()))
        .expect("install first generation");
    let replacement = swaps
        .install_verified(&second, "value", |_| Ok(()))
        .expect("install second generation");

    let error = swaps
        .rollback_generation("value", installed.generation)
        .expect_err("a stale generation must not roll back a newer install");
    let message = error.to_string();
    assert!(message.contains("refusing stale rollback"));
    assert!(message.contains(&installed.generation.to_string()));
    assert!(message.contains(&replacement.generation.to_string()));
    assert_eq!(
        swaps.active_generation("value"),
        Some(replacement.generation)
    );
    assert_eq!(swaps.lease("value").unwrap().call_i64_0().unwrap(), 2);

    let rollback = swaps
        .rollback_generation("value", replacement.generation)
        .expect("stale rejection must leave the matching rollback target intact");
    assert_eq!(rollback.generation, installed.generation);
    assert_eq!(swaps.lease("value").unwrap().call_i64_0().unwrap(), 1);
}

#[test]
fn rejects_abi_changes_and_failed_candidate_validation() {
    let _guard = ORC_TEST_LOCK.lock().unwrap();
    if !is_orc_jit_available() {
        eprintln!("LLVM ORC is unavailable; capability test skipped");
        return;
    }

    let swaps = HotSwapEngine::new();
    let active = compile_to_module("def value() -> i64 { 7 }", "active").unwrap();
    swaps
        .install_verified(&active, "value", |_| Ok(()))
        .expect("install active generation");
    let active_id = swaps.active_generation("value").unwrap();

    let bad_abi = compile_to_module("def value(x: i64) -> i64 { x }", "bad_abi").unwrap();
    let error = swaps
        .install_verified(&bad_abi, "value", |_| Ok(()))
        .expect_err("ABI change must be rejected");
    assert!(error.to_string().contains("ABI mismatch"));
    assert_eq!(swaps.active_generation("value"), Some(active_id));

    let invalid = compile_to_module("def value() -> i64 { 9 }", "invalid").unwrap();
    let error = swaps
        .install_verified(&invalid, "value", |_| {
            Err(iris::error::CodegenError::Unsupported {
                backend: "validator".into(),
                detail: "invariant failed".into(),
            })
        })
        .expect_err("failed validator must reject candidate");
    assert!(error.to_string().contains("invariant failed"));
    assert_eq!(swaps.active_generation("value"), Some(active_id));
    assert_eq!(swaps.lease("value").unwrap().call_i64_0().unwrap(), 7);
}

#[test]
fn retains_bounded_history_and_rolls_back_to_an_exact_generation() {
    let _guard = ORC_TEST_LOCK.lock().unwrap();
    if !is_orc_jit_available() {
        eprintln!("LLVM ORC is unavailable; capability test skipped");
        return;
    }

    let swaps = HotSwapEngine::with_history_limit(3);
    let mut generations = Vec::new();
    for value in 1..=4 {
        let module = compile_to_module(
            &format!("def value() -> i64 {{ {value} }}"),
            &format!("history_{value}"),
        )
        .unwrap();
        generations.push(
            swaps
                .install_verified(&module, "value", |_| Ok(()))
                .unwrap()
                .generation,
        );
    }

    assert_eq!(
        swaps.retained_generations("value"),
        vec![generations[3], generations[2], generations[1]]
    );
    swaps
        .rollback_to_generation("value", generations[3], generations[1])
        .unwrap();
    assert_eq!(swaps.lease("value").unwrap().call_i64_0().unwrap(), 2);
    assert!(swaps
        .rollback_to_generation("value", generations[1], generations[0])
        .is_err());
}

#[test]
fn program_gateways_switch_atomically_and_old_lease_remains_coherent() {
    let _guard = ORC_TEST_LOCK.lock().unwrap();
    if !is_orc_jit_available() {
        eprintln!("LLVM ORC is unavailable; capability test skipped");
        return;
    }

    let first = compile_to_module(
        "def left() -> i64 { 10 } def right() -> i64 { 11 }",
        "program_one",
    )
    .unwrap();
    let second = compile_to_module(
        "def left() -> i64 { 20 } def right() -> i64 { 21 }",
        "program_two",
    )
    .unwrap();
    let gateways = [
        ProgramGateway::new("left", "left"),
        ProgramGateway::new("right", "right"),
    ];
    let swaps = HotSwapEngine::new();
    swaps
        .install_program_verified(&first, "calculator", &gateways, |_| Ok(()))
        .unwrap();
    let old = swaps.lease_program("calculator").unwrap();
    let replacement = swaps
        .install_program_verified(&second, "calculator", &gateways, |_| Ok(()))
        .unwrap();
    let current = swaps.lease_program("calculator").unwrap();

    assert_eq!(old.call_i64_0("left").unwrap(), 10);
    assert_eq!(old.call_i64_0("right").unwrap(), 11);
    assert_eq!(current.call_i64_0("left").unwrap(), 20);
    assert_eq!(current.call_i64_0("right").unwrap(), 21);
    swaps
        .rollback_program_generation("calculator", replacement.generation)
        .unwrap();
    let restored = swaps.lease_program("calculator").unwrap();
    assert_eq!(restored.call_i64_0("left").unwrap(), 10);
    assert_eq!(restored.call_i64_0("right").unwrap(), 11);
}

#[test]
fn hot_swap_retention_and_external_archive_track_evictions() {
    let _guard = ORC_TEST_LOCK.lock().unwrap();
    if !is_orc_jit_available() {
        eprintln!("LLVM ORC is unavailable; retention/archive test skipped");
        return;
    }

    assert_eq!(parse_retention_spec("unlimited").unwrap(), None);
    assert_eq!(parse_retention_spec("0").unwrap(), None);
    assert_eq!(parse_retention_spec("12").unwrap(), Some(12));
    assert!(parse_retention_spec("bad").is_err());

    let mut archive_dir = std::env::temp_dir();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    archive_dir.push(format!("iris_hs_archive_{}_{}", std::process::id(), nonce));

    let swaps = HotSwapEngine::with_config(HotSwapConfig {
        history_limit: Some(2),
        archive_dir: Some(archive_dir.clone()),
    });
    for val in [10, 20, 30, 40] {
        let module = compile_to_module(
            &format!("def policy() -> i64 {{ {val} }}"),
            &format!("hs_{val}"),
        )
        .unwrap();
        swaps
            .install_verified(&module, "policy", |_| Ok(()))
            .unwrap();
    }
    assert_eq!(swaps.retained_generations("policy"), vec![4, 3]);
    assert_eq!(swaps.evicted_count(), 2);

    let archived = swaps.archived_generations().unwrap();
    assert_eq!(archived.len(), 4);
    assert_eq!(archived[0].status, "evicted");
    assert_eq!(archived[1].status, "evicted");
    assert_eq!(archived[2].status, "installed");
    assert_eq!(archived[3].status, "installed");
    let _ = std::fs::remove_dir_all(&archive_dir);

    // Unlimited retention never evicts in-memory generations.
    let unlimited = HotSwapEngine::with_config(HotSwapConfig::unlimited());
    for i in 0..10 {
        let module =
            compile_to_module(&format!("def policy() -> i64 {{ {i} }}"), &format!("u_{i}"))
                .unwrap();
        unlimited
            .install_verified(&module, "policy", |_| Ok(()))
            .unwrap();
    }
    assert_eq!(unlimited.retained_generations("policy").len(), 10);
    assert_eq!(unlimited.evicted_count(), 0);
}
