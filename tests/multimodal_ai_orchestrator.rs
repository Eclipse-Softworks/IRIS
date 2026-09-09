//! The public orchestrator defaults to deterministic simulated inputs.
#[path = "support/learning.rs"]
mod learning;

#[test]
fn multimodal_project_fuses_real_values_without_external_sdks() {
    learning::run(
        "projects/multimodal_ai_orchestrator/src/main.iris",
        "native",
        "multimodal orchestrator: 3 producers, tensor fusion, AIS action=1",
    );
}
