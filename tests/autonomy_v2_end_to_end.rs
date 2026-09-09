//! AIS v2 self-preservation/active inference and ROS 2 v2 pure semantics.

use iris::codegen::llvm_orc::{is_orc_jit_available, OrcJitEngine};
use iris::{compile_file, compile_file_to_module, EmitKind};

const AIS_V2: &str = r#"
bring std.ais

def ais_v2_contract() -> i64 {
    val bounds: list<ViabilityBound> = list()
    push(bounds, viability_bound_new("energy", 0.4, 0.8, 0.2, 0.9, 1.0, 7));

    val unsafe_values: list<f64> = list()
    push(unsafe_values, 0.1);
    val zero_drift: list<f64> = list()
    push(zero_drift, 0.0);

    val preferences: list<f64> = list()
    push(preferences, 1.0);
    push(preferences, 0.0);
    val outcome0: list<f64> = list()
    push(outcome0, 0.9);
    push(outcome0, 0.1);
    val outcome1: list<f64> = list()
    push(outcome1, 0.2);
    push(outcome1, 0.8);
    val outcomes: list<list<f64>> = list()
    push(outcomes, outcome0);
    push(outcomes, outcome1);
    val ambiguity: list<f64> = list()
    push(ambiguity, 0.1);
    push(ambiguity, 0.05);
    val epistemic: list<f64> = list()
    push(epistemic, 0.05);
    push(epistemic, 0.4);

    val endangered = autonomy_v2_step(
        bounds, unsafe_values, zero_drift, 1.0,
        outcomes, preferences, ambiguity, epistemic, 99
    )
    val safe_values: list<f64> = list()
    push(safe_values, 0.6);
    val regulated = autonomy_v2_step(
        bounds, safe_values, zero_drift, 1.0,
        outcomes, preferences, ambiguity, epistemic, 99
    )
    val selected = active_inference_select(outcomes, preferences, ambiguity, epistemic)

    if endangered.emergency && endangered.action == 7 &&
       !regulated.emergency && regulated.action == 0 && regulated.viable &&
       selected.policy == 0 && selected.expected_free_energy < 0.08 {
        0
    } else { 1 }
}
"#;

const ROS2_V2: &str = r#"
bring std.ros2

def ros2_v2_contract() -> i64 {
    val sensor_qos = qos_sensor_data()
    val control_qos = qos_control()
    val node = ROS2Node { handle: 1, ctx: 2 }
    val unconfigured = lifecycle_new(node)
    val inactive = lifecycle_transition(unconfigured, 1)
    val active = lifecycle_transition(inactive, 2)

    val transform = Transform {
        translation: Vector3 { x: 1.0, y: 2.0, z: 3.0 },
        rotation: Quaternion { x: 0.0, y: 0.0, z: 0.0, w: 1.0 }
    }
    val point = transform_point(transform, Point { x: 1.0, y: 1.0, z: 1.0 })
    val recovered = transform_point(transform_inverse(transform), point)
    var buffer = transform_buffer_new()
    buffer = transform_buffer_set(buffer, StampedTransform {
        parent_frame: "map", child_frame: "base_link", stamp_ns: 10, transform: transform
    })
    val direct = transform_buffer_lookup(buffer, "map", "base_link")
    val reverse = transform_buffer_lookup(buffer, "base_link", "map")

    if sensor_qos.reliability == 2 && sensor_qos.depth == 5 &&
       control_qos.reliability == 1 && control_qos.depth == 1 &&
       lifecycle_can_publish(active) && active.transitions == 2 &&
       point.x == 2.0 && point.y == 3.0 && point.z == 4.0 &&
       recovered.x == 1.0 && recovered.y == 1.0 && recovered.z == 1.0 &&
       is_some(direct) && is_some(reverse) {
        0
    } else { 1 }
}
"#;

fn assert_eval_and_orc(source: &str, name: &str, function: &str) {
    let path = std::env::temp_dir().join(format!("iris_{name}_{}.iris", std::process::id()));
    std::fs::write(&path, source).expect("write autonomy fixture");
    eprintln!("{name}: evaluating native/interpreter contract");
    let interpreted = compile_file(&path, EmitKind::Eval).expect("interpret autonomy fixture");
    assert_eq!(interpreted.trim(), "0");
    if is_orc_jit_available() {
        eprintln!("{name}: compiling ORC module");
        let module = compile_file_to_module(&path).expect("compile autonomy module");
        let engine = OrcJitEngine::new().expect("create ORC session");
        let loaded = engine.load_module(&module).expect("load autonomy module");
        eprintln!("{name}: entering ORC function");
        assert_eq!(
            loaded.call_i64_0(function).expect("call autonomy function"),
            0
        );
        eprintln!("{name}: ORC function returned");
    }
    let _ = std::fs::remove_file(path);
}

#[test]
fn ais_v2_prioritizes_viability_then_minimizes_expected_free_energy() {
    assert_eval_and_orc(AIS_V2, "ais_v2", "ais_v2_contract");
}

#[test]
fn ros2_v2_qos_lifecycle_and_transform_semantics_match_backends() {
    assert_eval_and_orc(ROS2_V2, "ros2_v2", "ros2_v2_contract");
}
