//! `std.ros2` against a real ROS 2 install: rclcpp generic publishers and
//! subscriptions, services, parameters, rclcpp_lifecycle, tf2 and
//! rclcpp_action, all inside one IRIS program per test.
//!
//! These need the bridge (`python build_ros2_bridge.py`, which leaves
//! iris_ros2.dll / libiris_ros2.so in the repository root) and a ROS 2
//! environment the bridge can load from (a sourced shell, or the ROS 2 bin
//! directory on PATH). Without them every test here says so and passes.
//!
//! Each program runs as a native binary and, where LLVM ORC is available,
//! in-process through the JIT.

use std::path::{Path, PathBuf};

use iris::codegen::llvm_orc::{is_orc_jit_available, OrcJitEngine};
use iris::compile_file_to_module;

fn bridge_library() -> Option<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    ["iris_ros2.dll", "libiris_ros2.so"]
        .iter()
        .map(|name| root.join(name))
        .find(|path| path.exists())
}

#[cfg(windows)]
fn loadable(path: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryW(name: *const u16) -> *mut std::ffi::c_void;
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe { !LoadLibraryW(wide.as_ptr()).is_null() }
}

#[cfg(unix)]
fn loadable(path: &Path) -> bool {
    extern "C" {
        fn dlopen(name: *const std::os::raw::c_char, flags: i32) -> *mut std::ffi::c_void;
    }
    let name = std::ffi::CString::new(path.to_string_lossy().as_bytes()).unwrap();
    unsafe { !dlopen(name.as_ptr(), 2).is_null() }
}

/// Whether the bridge can run here; explains why not when it cannot.
fn ros2_ready(test: &str) -> bool {
    let Some(library) = bridge_library() else {
        eprintln!("{test}: skipped -- no ROS 2 bridge built (run `python build_ros2_bridge.py`)");
        return false;
    };
    if !loadable(&library) {
        eprintln!(
            "{test}: skipped -- {} does not load (source a ROS 2 environment first)",
            library.display()
        );
        return false;
    }
    true
}

/// Runs `source` as a native binary and `function` through ORC; the program's
/// asserts are the test.
///
/// The binary is built and run here rather than through `EmitKind::Eval`,
/// which falls back to the interpreter when the native program fails -- a
/// native assert failure would pass unnoticed.
fn run_both(source: &str, name: &str, function: &str) {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("iris_ros2_live_{name}_{}.iris", std::process::id()));
    std::fs::write(&path, source).expect("write ROS 2 fixture");
    let module = compile_file_to_module(&path).expect("compile ROS 2 module");

    let exe = dir.join(format!(
        "iris_ros2_live_{name}_{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    let exe = iris::codegen::build::build_binary(&module, &exe).expect("build ROS 2 program");
    // From the repository root, where the bridge library is.
    let run = std::process::Command::new(&exe)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run ROS 2 program");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        run.status.success(),
        "{name}: native run failed ({}):\n{stdout}\n{stderr}",
        run.status
    );
    let _ = std::fs::remove_file(&exe);

    if is_orc_jit_available() {
        let engine = OrcJitEngine::new().expect("create ORC session");
        let loaded = engine.load_module(&module).expect("load ROS 2 module");
        assert_eq!(
            loaded.call_i64_0(function).expect("call through ORC"),
            0,
            "{name}: ORC"
        );
        eprintln!("{name}: native and ORC");
    } else {
        eprintln!("{name}: native only -- LLVM ORC is unavailable here");
    }
    let _ = std::fs::remove_file(path);
}

const TOPICS: &str = r#"
bring std.ros2

def matched(ctx: ROS2Context, p: ROS2Publisher) -> bool {
    var tries = 0
    while publisher_matches(p) < 1 && tries < 300 {
        spin_some(ctx, 10);
        sleep_ms(10);
        tries = tries + 1
    };
    publisher_matches(p) > 0
}

def topics_contract() -> i64 {
    val ctx = context_create()
    assert(ctx.handle != 0);
    val node = node_create(ctx, "iris_live_topics", "")
    assert(node.handle != 0);

    // Typed helpers carry exact doubles.
    val tw_pub = publisher_create(node, "/iris_live/topics/twist", "geometry_msgs/msg/Twist")
    val tw_sub = subscription_create(node, "/iris_live/topics/twist", "geometry_msgs/msg/Twist")
    assert(matched(ctx, tw_pub));
    assert(publish_twist(tw_pub, make_twist(1.25, 0.0, 0.0, 0.0, 0.0, 0.1)));
    assert(wait_for_message(tw_sub, 5000));
    val got = take_twist(tw_sub)
    assert(is_some(got));
    val t = unwrap(got)
    assert(t.linear.x == 1.25 && t.angular.z == 0.1);

    // Any installed type, by field path, delivered as an event.
    val scan_pub = publisher_create_qos(node, "/iris_live/topics/scan", "sensor_msgs/msg/LaserScan", qos_sensor_data())
    val scan_sub = subscription_create_qos(node, "/iris_live/topics/scan", "sensor_msgs/msg/LaserScan", qos_sensor_data())
    assert(matched(ctx, scan_pub));
    val scan = publisher_msg(scan_pub)
    msg_set_str(scan, "header.frame_id", "laser");
    msg_set_stamp(scan, "header.stamp", 1500000007);
    val ranges: list<f64> = list()
    list_push(ranges, 0.5);
    list_push(ranges, 1.75);
    list_push(ranges, 3.0);
    msg_set_f64s(scan, "ranges", ranges);
    msg_set_f64(scan, "ranges[3]", 4.25);
    assert(publish_msg(scan_pub, scan));
    msg_free(scan);
    var seen = false
    var spins = 0
    while !seen && spins < 200 {
        val ev = spin_once(ctx, 50)
        if is_message(ev, scan_sub) {
            val m = ev.message
            val rs = msg_f64s(m, "ranges")
            assert(list_len(rs) == 4 && list_get(rs, 1) == 1.75 && list_get(rs, 3) == 4.25);
            assert(msg_str(m, "header.frame_id") == "laser");
            assert(msg_stamp(m, "header.stamp") == 1500000007);
            assert(msg_type(m) == "sensor_msgs/msg/LaserScan");
            seen = true
        };
        spins = spins + 1
    };
    assert(seen);

    // JSON in and out, and the type's schema.
    val imu = msg_from_json("sensor_msgs/msg/Imu", "{\"linear_acceleration\": {\"z\": 9.80665}}")
    assert(msg_f64(imu, "linear_acceleration.z") == 9.80665);
    assert(msg_len(imu, "orientation_covariance") == 9);
    assert(msg_get_json(imu, "angular_velocity") == "{\"x\":0.0,\"y\":0.0,\"z\":0.0}");
    msg_free(imu);
    assert(contains(msg_schema("std_msgs/msg/String"), "\"data\""));

    // A wall timer ticks through the same event loop.
    val timer = wall_timer_create(node, 20)
    var ticks = 0
    var rounds = 0
    while ticks < 2 && rounds < 200 {
        val ev = spin_once(ctx, 50)
        if is_timer(ev, timer) { ticks = ticks + 1 };
        rounds = rounds + 1
    };
    assert(ticks == 2);
    assert(timer_destroy(timer));

    assert(context_destroy(ctx));
    0
}

def main() -> i64 { topics_contract() }
"#;

const SERVICES: &str = r#"
bring std.ros2

def services_contract() -> i64 {
    val ctx = context_create()
    val node = node_create(ctx, "iris_live_services", "")
    assert(node.handle != 0);

    val srv = service_create(node, "/iris_live/services/add", "example_interfaces/srv/AddTwoInts")
    val cli = client_create(node, "/iris_live/services/add", "example_interfaces/srv/AddTwoInts")
    assert(client_wait_service(cli, 5000));
    val req = client_request(cli)
    msg_set_i64(req, "a", 40);
    msg_set_i64(req, "b", 2);
    val token = client_send(cli, req)
    msg_free(req);
    var answered = false
    var tries = 0
    while !answered && tries < 200 {
        val ev = spin_once(ctx, 50)
        if is_request(ev, srv) {
            val sum = msg_i64(request_of(ev), "a") + msg_i64(request_of(ev), "b")
            msg_set_i64(response_of(ev), "sum", sum);
            assert(service_reply(srv, ev));
            answered = true
        };
        tries = tries + 1
    };
    assert(answered);
    val resp = client_wait_response(cli, token, 5000)
    assert(is_some(resp) && msg_i64(unwrap(resp), "sum") == 42);

    // Parameters are rclcpp's: descriptors are enforced.
    assert(param_declare_f64_range(node, "gain", 1.5, 0.0, 2.0, "P gain") == 1.5);
    assert(!param_set_f64(node, "gain", 3.0));
    assert(param_f64(node, "gain") == 1.5);
    assert(param_set_f64(node, "gain", 0.25) && param_f64(node, "gain") == 0.25);
    assert(param_declare_i64_range(node, "rate", 50, 1, 100, "Hz") == 50);
    assert(!param_set_i64(node, "rate", 500));
    assert(param_declare_str(node, "frame", "base_link") == "base_link");
    assert(param_declare_bool(node, "enabled", true));
    assert(param_declare_json(node, "weights", "[0.5, 1.5]", "") == "[0.5,1.5]");
    assert(param_type(node, "weights") == PARAM_DOUBLE_ARRAY);
    assert(param_has(node, "use_sim_time"));
    assert(!node_uses_sim_time(node));

    assert(context_destroy(ctx));
    0
}

def main() -> i64 { services_contract() }
"#;

const LIFECYCLE_TF: &str = r#"
bring std.ros2

def lifecycle_tf_contract() -> i64 {
    val ctx = context_create()

    val managed = lifecycle_node_create(ctx, "iris_live_managed", "")
    assert(managed.handle != 0);
    assert(lifecycle_state(managed) == LIFECYCLE_UNCONFIGURED);
    val lpub = publisher_create(managed, "/iris_live/lifecycle/out", "std_msgs/msg/String")
    assert(!publish_string(lpub, "dropped while inactive"));
    assert(lifecycle_configure(managed) == LIFECYCLE_INACTIVE);
    assert(lifecycle_activate(managed) == LIFECYCLE_ACTIVE);
    assert(publish_string(lpub, "published while active"));
    assert(lifecycle_set_result(managed, ON_DEACTIVATE, CALLBACK_FAILURE));
    assert(lifecycle_deactivate(managed) == LIFECYCLE_ACTIVE);
    var configured = false
    var activated = false
    var rounds = 0
    while rounds < 20 {
        val ev = spin_once(ctx, 0)
        if is_transition(ev, managed) && ev.id == ON_CONFIGURE { configured = true };
        if is_transition(ev, managed) && ev.id == ON_ACTIVATE { activated = true };
        rounds = rounds + 1
    };
    assert(configured && activated);

    // tf2's own buffer: chains and inverses.
    var buffer = transform_buffer_new()
    val base = Transform {
        translation: Vector3 { x: 1.0, y: 2.0, z: 3.0 },
        rotation: Quaternion { x: 0.0, y: 0.0, z: 0.0, w: 1.0 }
    }
    val laser = Transform {
        translation: Vector3 { x: 0.5, y: 0.0, z: 0.0 },
        rotation: Quaternion { x: 0.0, y: 0.0, z: 0.0, w: 1.0 }
    }
    buffer = transform_buffer_set(buffer, StampedTransform { parent_frame: "map", child_frame: "base_link", stamp_ns: 10, transform: base });
    buffer = transform_buffer_set(buffer, StampedTransform { parent_frame: "base_link", child_frame: "laser", stamp_ns: 10, transform: laser });
    val chained = transform_buffer_lookup(buffer, "map", "laser")
    assert(is_some(chained) && unwrap(chained).translation.x == 1.5);
    val reverse = transform_buffer_lookup(buffer, "laser", "map")
    assert(is_some(reverse) && unwrap(reverse).translation.x == 0.0 - 1.5);
    assert(is_none(transform_buffer_lookup(buffer, "map", "nowhere")));

    // /tf_static from a broadcaster to a listener, over DDS.
    val node = node_create(ctx, "iris_live_tf", "")
    val listener = tf_buffer_create(node)
    val bc = tf_static_broadcaster_create(node)
    assert(tf_send(bc, "iris_live_world", "iris_live_robot", base));
    var found = false
    var waits = 0
    while !found && waits < 200 {
        spin_once(ctx, 25);
        found = tf_can_transform(listener, "iris_live_world", "iris_live_robot");
        waits = waits + 1
    };
    assert(found);
    val heard = tf_lookup(listener, "iris_live_world", "iris_live_robot")
    assert(is_some(heard) && unwrap(heard).translation.z == 3.0);

    assert(context_destroy(ctx));
    0
}

def main() -> i64 { lifecycle_tf_contract() }
"#;

const ACTIONS: &str = r#"
bring std.ros2

def actions_contract() -> i64 {
    val ctx = context_create()
    val node = node_create(ctx, "iris_live_actions", "")
    val server = action_server_create(node, "/iris_live/fibonacci", "example_interfaces/action/Fibonacci")
    val client = action_client_create(node, "/iris_live/fibonacci", "example_interfaces/action/Fibonacci")
    assert(action_wait_server(client, 5000));

    // Goal, feedback, result.
    val goal = action_goal_msg(client)
    msg_set_i64(goal, "order", 4);
    val token = action_send_goal(client, goal)
    msg_free(goal);
    var feedbacks = 0
    var accepted = false
    var finished = false
    var rounds = 0
    while !finished && rounds < 400 {
        val ev = spin_once(ctx, 25)
        if is_goal(ev, server) {
            val g = goal_of(ev)
            assert(msg_i64(goal_request(g), "order") == 4);
            msg_set_json(goal_feedback_msg(g), "sequence", "[0,1,1]");
            assert(goal_publish_feedback(g));
            msg_set_json(goal_result_msg(g), "sequence", "[0,1,1,2,3]");
            assert(goal_succeed(g))
        };
        if ev.kind == EVENT_GOAL_RESPONSE && ev.source == client.handle { accepted = ev.aux == 1 };
        if ev.kind == EVENT_FEEDBACK && ev.source == client.handle { feedbacks = feedbacks + 1 };
        if ev.kind == EVENT_RESULT && ev.source == client.handle {
            assert(ev.id == token && ev.aux == GOAL_SUCCEEDED);
            assert(msg_get_json(ev.message, "sequence") == "[0,1,1,2,3]");
            finished = true
        };
        rounds = rounds + 1
    };
    assert(finished && accepted && feedbacks == 1);

    // Cancellation: the server sees CANCELING and finishes the goal canceled.
    val long_goal = action_goal_msg(client)
    msg_set_i64(long_goal, "order", 40);
    val long_token = action_send_goal(client, long_goal)
    msg_free(long_goal);
    var cancel_code = 0 - 1
    var outcome = 0
    var steps = 0
    while outcome == 0 && steps < 400 {
        val ev = spin_once(ctx, 25)
        if is_goal(ev, server) { assert(action_cancel(client, long_token)) };
        if is_cancel(ev, server) {
            val g = goal_of(ev)
            assert(goal_is_canceling(g));
            assert(goal_canceled(g))
        };
        if ev.kind == EVENT_CANCEL_RESPONSE && ev.source == client.handle { cancel_code = ev.aux };
        if ev.kind == EVENT_RESULT && ev.source == client.handle { outcome = ev.aux };
        steps = steps + 1
    };
    assert(cancel_code == 0 && outcome == GOAL_CANCELED);
    assert(action_status(client, long_token) == GOAL_CANCELED);

    assert(context_destroy(ctx));
    0
}

def main() -> i64 { actions_contract() }
"#;

#[test]
fn ros2_live_topics_any_type_json_and_timers() {
    if ros2_ready("ros2_live_topics") {
        run_both(TOPICS, "topics", "topics_contract");
    }
}

#[test]
fn ros2_live_services_and_parameters() {
    if ros2_ready("ros2_live_services") {
        run_both(SERVICES, "services", "services_contract");
    }
}

#[test]
fn ros2_live_lifecycle_and_tf2() {
    if ros2_ready("ros2_live_lifecycle_tf") {
        run_both(LIFECYCLE_TF, "lifecycle_tf", "lifecycle_tf_contract");
    }
}

#[test]
fn ros2_live_actions_with_feedback_and_cancel() {
    if ros2_ready("ros2_live_actions") {
        run_both(ACTIONS, "actions", "actions_contract");
    }
}
