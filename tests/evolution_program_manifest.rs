use iris::codegen::hot_swap::HotSwapEngine;
use iris::codegen::llvm_orc::is_orc_jit_available;
use iris::compile_to_module;
use iris::evolution::artifact::ArtifactStore;
use iris::evolution::manifest::{
    HostAbi, ProgramEntrypoint, ProgramManifest, StateContract, PROGRAM_MANIFEST_SCHEMA,
};
use std::sync::Mutex;

static ORC_TEST_LOCK: Mutex<()> = Mutex::new(());

fn manifest() -> ProgramManifest {
    ProgramManifest {
        schema: PROGRAM_MANIFEST_SCHEMA.to_owned(),
        program_id: "adaptive_controller".to_owned(),
        entrypoints: vec![
            ProgramEntrypoint {
                gateway: "predict".to_owned(),
                symbol: "predict".to_owned(),
                abi: HostAbi::ScalarI64V1,
            },
            ProgramEntrypoint {
                gateway: "health".to_owned(),
                symbol: "health".to_owned(),
                abi: HostAbi::CommandV1,
            },
            ProgramEntrypoint {
                gateway: "json".to_owned(),
                symbol: "json_gateway".to_owned(),
                abi: HostAbi::JsonV1,
            },
        ],
        state: Some(StateContract {
            schema: "controller-state/1".to_owned(),
            snapshot_symbol: "snapshot".to_owned(),
            import_symbol: "restore".to_owned(),
        }),
    }
}

const PROGRAM: &str = r#"
def predict(x: i64) -> i64 { x + 1 }
def health() -> i64 { 0 }
def json_gateway(request: str) -> str { request }
def snapshot() -> str { "{\"generation\":1}" }
def restore(state: str) -> i64 { if len(state) > 0 { 0 } else { 1 } }
"#;

#[test]
fn manifest_validates_reviewed_gateways_and_state_contract() {
    let module = compile_to_module(PROGRAM, "manifest_valid").unwrap();
    manifest().validate(&module).unwrap();

    let mut invalid = manifest();
    invalid.entrypoints[0].abi = HostAbi::JsonV1;
    let error = invalid.validate(&module).unwrap_err();
    assert!(error.to_string().contains("expected (str) -> str"));
}

#[test]
fn artifact_store_is_content_addressed_and_idempotent() {
    let root = std::env::temp_dir().join(format!(
        "iris_artifacts_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = ArtifactStore::new(&root);
    let first = store.persist(PROGRAM.as_bytes(), &manifest()).unwrap();
    let second = store.persist(PROGRAM.as_bytes(), &manifest()).unwrap();
    assert_eq!(first, second);
    let directory = store.artifact_dir(&first.artifact_sha256);
    assert_eq!(
        std::fs::read(directory.join("source.iris")).unwrap(),
        PROGRAM.as_bytes()
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn whole_program_manifest_installs_all_gateways_atomically() {
    let _guard = ORC_TEST_LOCK.lock().unwrap();
    if !is_orc_jit_available() {
        eprintln!("LLVM ORC is unavailable; capability test skipped");
        return;
    }
    let module = compile_to_module(PROGRAM, "manifest_install").unwrap();
    let manifest = manifest();
    manifest.validate(&module).unwrap();
    let swaps = HotSwapEngine::new();
    swaps
        .install_program_verified(&module, &manifest.program_id, &manifest.gateways(), |_| {
            Ok(())
        })
        .unwrap();
    let lease = swaps.lease_program(&manifest.program_id).unwrap();
    assert_eq!(lease.call_i64_1("predict", 41).unwrap(), 42);
    assert_eq!(lease.call_i64_0("health").unwrap(), 0);
    assert_eq!(
        lease.call_str_1("json", "{\"request\":42}").unwrap(),
        "{\"request\":42}"
    );
}
