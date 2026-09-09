//! Allocation-free Cortex-M / ESP32 component generation and proof gates.

use iris::codegen::embedded::{
    build_embedded_bundle, validate_embedded_module, verify_hardware_report, EmbeddedTarget,
};
use iris::codegen::llvm_c_api::is_llvm_c_api_available;
use iris::codegen::{target_data_layout, target_preset_to_triple};
use iris::compile_to_module;

const SAFE_CONTROL: &str = r#"
def control_step(sensor: i64, target: i64) -> i64 {
    if sensor < target { target - sensor } else { 0 }
}

def main() -> i64 {
    var samples = [2, 4, 6, 8]
    var total = 0
    for i in 0..4 {
        total = total + samples[i]
    };
    if control_step(total, 20) == 0 { 0 } else { 1 }
}
"#;

#[test]
fn embedded_presets_have_32_bit_llvm_layouts() {
    assert_eq!(
        target_preset_to_triple("arduino-uno"),
        Some("avr-unknown-unknown")
    );
    assert_eq!(
        target_preset_to_triple("cortex-m4f"),
        Some("thumbv7em-none-eabihf")
    );
    assert_eq!(
        target_preset_to_triple("esp32-c3"),
        Some("riscv32-unknown-none-elf")
    );
    assert!(target_data_layout("thumbv7em-none-eabihf").contains("p:32:32"));
    assert!(target_data_layout("riscv32-unknown-none-elf").contains("p:32:32"));
    assert!(target_data_layout("avr-unknown-unknown").contains("p:16:8"));
}

#[test]
fn proof_accepts_stack_arrays_and_rejects_dynamic_allocation() {
    let safe = compile_to_module(SAFE_CONTROL, "embedded_safe").expect("compile safe control");
    let proof = validate_embedded_module(&safe, EmbeddedTarget::CortexM4F, "main")
        .expect("prove allocation-free graph");
    assert_eq!(proof.fixed_array_bytes, 32);
    assert_eq!(proof.reachable_functions, ["control_step", "main"]);

    let allocating = compile_to_module(
        r#"
def main() -> i64 effect alloc {
    val values: list<i64> = list()
    push(values, 1);
    len(values)
}
"#,
        "embedded_allocating",
    )
    .expect("compile allocating fixture");
    let error = validate_embedded_module(&allocating, EmbeddedTarget::Esp32C3, "main")
        .expect_err("dynamic collections must not receive an embedded proof");
    assert!(error.to_string().contains("dynamic collection"), "{error}");
}

#[test]
fn proof_rejects_recursive_stack_growth() {
    let module = compile_to_module(
        r#"
@noinline
def descend(n: i64) -> i64 {
    if n == 0 { 0 } else { 1 + descend(n - 1) }
}
def main() -> i64 { descend(3) - 3 }
"#,
        "embedded_recursion",
    )
    .expect("compile recursive fixture");
    let error = validate_embedded_module(&module, EmbeddedTarget::CortexM33, "main")
        .expect_err("recursive call graph must be rejected");
    assert!(
        error.to_string().contains("recursive call cycle"),
        "{error}"
    );
}

#[test]
fn emits_real_arm_and_riscv_objects_without_allocator_symbols() {
    if !is_llvm_c_api_available() {
        eprintln!("LLVM-C unavailable; embedded object emission skipped");
        return;
    }
    let module = compile_to_module(SAFE_CONTROL, "embedded_objects").expect("compile control");
    for target in [EmbeddedTarget::CortexM4F, EmbeddedTarget::Esp32C3] {
        let output = std::env::temp_dir().join(format!(
            "iris_embedded_{}_{}_{}",
            target.preset(),
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let bundle =
            build_embedded_bundle(&module, &output, target, "main").expect("emit embedded bundle");
        for object in [
            &bundle.module_object,
            &bundle.runtime_object,
            &bundle.harness_object,
        ] {
            let bytes = std::fs::read(object).expect("read embedded object");
            assert!(bytes.starts_with(b"\x7fELF"));
            assert!(!String::from_utf8_lossy(&bytes).contains("malloc"));
        }
        let manifest = std::fs::read_to_string(&bundle.manifest).expect("read manifest");
        assert!(manifest.contains("\"allocation_free\": true"));
        assert!(manifest.contains(&bundle.proof.fingerprint));
        assert!(bundle.board_support.is_none());
        std::fs::remove_dir_all(output).expect("remove isolated embedded test output");
    }

    let uno = compile_to_module(
        "def control(x: i64) -> i64 { if x == 20 { 0 } else { 1 } }\ndef main() -> i64 { control(20) }",
        "embedded_uno_object",
    )
    .expect("compile Uno scalar control");
    let output = std::env::temp_dir().join(format!("iris_embedded_uno_{}", std::process::id()));
    let bundle = build_embedded_bundle(&uno, &output, EmbeddedTarget::ArduinoUno, "main")
        .expect("emit Arduino Uno bundle");
    let board_support = bundle
        .board_support
        .as_ref()
        .expect("Uno bundle includes its allocation-free BSP");
    let source = std::fs::read_to_string(board_support).expect("read Uno BSP");
    assert!(source.contains("iris_board_validation_report"));
    std::fs::remove_dir_all(output).expect("remove isolated Uno test output");
}

#[test]
fn uno_rejects_llvm17_loop_and_fixed_array_gap() {
    let module = compile_to_module(SAFE_CONTROL, "embedded_uno_loop").expect("compile Uno loop");
    let error = validate_embedded_module(&module, EmbeddedTarget::ArduinoUno, "main")
        .expect_err("unsafe AVR loop must be rejected before LLVM codegen");
    assert!(error.to_string().contains("contains a loop"), "{error}");
}

#[test]
fn uno_button_led_demo_is_a_proven_platform_hook_program() {
    let source = include_str!("../examples/embedded/uno_button_led.iris");
    let module = compile_to_module(source, "uno_button_led").expect("compile interactive demo");
    let proof = validate_embedded_module(&module, EmbeddedTarget::ArduinoUno, "main")
        .expect("prove interactive Uno program");
    assert_eq!(proof.fixed_array_bytes, 0);
    assert_eq!(proof.reachable_functions, ["main"]);
}

#[test]
fn hardware_report_must_match_the_exact_build_and_zero_counters() {
    let module = compile_to_module(SAFE_CONTROL, "embedded_report").expect("compile control");
    let proof =
        validate_embedded_module(&module, EmbeddedTarget::Esp32C3, "main").expect("prove control");
    let line = format!(
        "IRIS-HW/1 target=esp32-c3 fingerprint={} result=0 allocations=0 faults=0",
        proof.fingerprint
    );
    let report = verify_hardware_report(&line, &proof).expect("accept board report");
    assert_eq!(report.allocations, 0);

    let bad = line.replace("allocations=0", "allocations=1");
    assert!(verify_hardware_report(&bad, &proof).is_err());
    let stale = line.replace(&proof.fingerprint, "0000000000000000");
    assert!(verify_hardware_report(&stale, &proof).is_err());
}

#[test]
fn proof_fingerprint_changes_when_reachable_code_changes() {
    let first = compile_to_module("def main() -> i64 { 0 }", "embedded_fingerprint_a")
        .expect("compile first image");
    let second = compile_to_module("def main() -> i64 { 1 }", "embedded_fingerprint_b")
        .expect("compile second image");
    let first = validate_embedded_module(&first, EmbeddedTarget::ArduinoUno, "main")
        .expect("prove first image");
    let second = validate_embedded_module(&second, EmbeddedTarget::ArduinoUno, "main")
        .expect("prove second image");
    assert_ne!(first.fingerprint, second.fingerprint);
}
