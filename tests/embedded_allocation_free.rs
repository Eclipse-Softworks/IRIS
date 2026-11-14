//! Allocation-free Cortex-M / ESP32 component generation and proof gates.

use iris::codegen::embedded::{
    build_embedded_bundle, validate_embedded_module, verify_hardware_report, EmbeddedTarget,
};
use iris::codegen::llvm_c_api::{is_llvm_c_api_available, is_llvm_target_available};
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

    if !is_llvm_target_available(EmbeddedTarget::ArduinoUno.triple()) {
        eprintln!("LLVM-C has no AVR backend; Arduino Uno object emission skipped");
        return;
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

fn uno_control(name: &str) -> iris::IrModule {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("projects/uno_controls")
        .join(format!("uno_{name}.iris"));
    iris::compile_file_to_module(&path).unwrap_or_else(|error| panic!("{name}: {error}"))
}

#[test]
fn uno_programs_keep_scalar_state_between_steps() {
    for name in ["toggle", "pid", "modes"] {
        let proof =
            validate_embedded_module(&uno_control(name), EmbeddedTarget::ArduinoUno, "main")
                .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(
            proof
                .reachable_functions
                .iter()
                .any(|f| f == "__iris_state_init"),
            "{name}"
        );
        assert_eq!(proof.fixed_array_bytes, 0);
        // Constants are lowered in name order, so a rebuild proves the same image.
        let again =
            validate_embedded_module(&uno_control(name), EmbeddedTarget::ArduinoUno, "main")
                .expect("prove again");
        assert_eq!(proof.fingerprint, again.fingerprint, "{name}");
    }
    if !is_llvm_target_available(EmbeddedTarget::ArduinoUno.triple()) {
        eprintln!("no installed LLVM-C has an AVR backend; Uno state emission skipped");
        return;
    }
    for name in ["toggle", "pid", "modes"] {
        let output =
            std::env::temp_dir().join(format!("iris_uno_state_{name}_{}", std::process::id()));
        let bundle = build_embedded_bundle(
            &uno_control(name),
            &output,
            EmbeddedTarget::ArduinoUno,
            "main",
        )
        .unwrap_or_else(|error| panic!("{name}: {error}"));
        let ir = std::fs::read_to_string(&bundle.module_ir).expect("read module IR");
        // The entry initialises state on its first call: bare-metal startup
        // code runs no constructors.
        assert!(
            ir.contains("define i64 @iris_embedded_entry_main()"),
            "{name}"
        );
        assert!(ir.contains("call i64 @__iris_state_init()"), "{name}");
        assert!(!ir.contains("@llvm.global_ctors"), "{name}");
        // Board hooks come from the BSP; no hosted fallback is linked in.
        let object = std::fs::read(&bundle.module_object).expect("read module object");
        let abort = b"iris_unhandled_effect_abort";
        assert!(
            !object.windows(abort.len()).any(|window| window == abort),
            "{name}"
        );
        std::fs::remove_dir_all(output).expect("remove Uno state test output");
    }
}

#[test]
fn embedded_state_must_be_scalar_and_stays_out_of_the_hosted_proof() {
    let module = compile_to_module(
        "var history: list<i64> = list()\ndef main() -> i64 { push(history, 1); 0 }",
        "embedded_list_state",
    )
    .expect("compile list state");
    let error = validate_embedded_module(&module, EmbeddedTarget::CortexM4F, "main")
        .expect_err("list state needs the heap");
    assert!(error.to_string().contains("scalar state only"), "{error}");

    // The target-free proof that the evolution gate relies on still refuses
    // program state.
    let counter = compile_to_module(
        "var count: i64 = 0\ndef step() -> i64 { count = count + 1; count }\ndef main() -> i64 { step() }",
        "hosted_scalar_state",
    )
    .expect("compile scalar state");
    assert!(iris::codegen::embedded::prove_allocation_free(&counter, "step").is_err());
    assert!(validate_embedded_module(&counter, EmbeddedTarget::CortexM4F, "main").is_ok());
}
