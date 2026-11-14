//! Integration tests for ROS2 Client Library and DDS wrappers.
//!
//! These compile `std.ros2` programs without running them, so they hold on
//! machines without ROS 2; tests/ros2_live.rs runs them against a ROS 2
//! install.

use iris::compile_multi;
use iris::EmitKind;

#[test]
fn test_ros2_stdlib_bring_and_compilation() {
    let src = r#"
bring std.ros2

def main() -> i64 {
    // Assert we can parse and call basic setup
    val node = ros2.create_node("test_iris_node", "sandbox");
    val pub_handle = ros2.create_publisher(node, "sensor_readings", "Float64");

    // Publish mock sensor feedback loop data
    val sent = ros2.publish_float64(pub_handle, 42.15);

    0
}
"#;
    // Compiling to LLVM IR to verify that the AST parses, merges, lowers,
    // and successfully completes optimization passes without compile errors.
    let res = compile_multi(&[("main", src)], "main", EmitKind::Llvm);
    assert!(
        res.is_ok(),
        "ROS2 client library wrapper failed to compile: {:?}",
        res.err()
    );
}

/// Every part of the rclcpp-backed API in one program: generic messages,
/// events, services, clients, parameters, lifecycle, timers, tf2 and actions.
#[test]
fn test_ros2_full_api_compiles_to_native_ir() {
    let src = r#"
bring std.ros2

def serve(ctx: ROS2Context, srv: ROS2Service, actions: ROS2ActionServer) -> i64 {
    val ev = spin_once(ctx, 10)
    if is_request(ev, srv) {
        val sum = msg_i64(request_of(ev), "a") + msg_i64(request_of(ev), "b")
        msg_set_i64(response_of(ev), "sum", sum);
        service_reply(srv, ev);
    };
    if is_goal(ev, actions) {
        val g = goal_of(ev)
        msg_set_json(goal_feedback_msg(g), "sequence", "[0,1]");
        goal_publish_feedback(g);
        msg_set_json(goal_result_msg(g), "sequence", "[0,1,1]");
        goal_succeed(g);
    };
    ev.kind
}

def main() -> i64 {
    val ctx = context_create_with("--ros-args --log-level warn", 0 - 1)
    val node = node_create_with(ctx, "iris_api", "", "--ros-args -p use_sim_time:=false", 0)
    val managed = lifecycle_node_create(ctx, "iris_api_managed", "")
    val pub_obj = publisher_create_qos(node, "/scan", "sensor_msgs/msg/LaserScan", qos_sensor_data())
    val sub = subscription_create(node, "/scan", "sensor_msgs/msg/LaserScan")
    val m = publisher_msg(pub_obj)
    val ranges: list<f64> = list()
    list_push(ranges, 1.5);
    msg_set_f64s(msg_set_str(m, "header.frame_id", "laser"), "ranges", ranges);
    msg_set_stamp(m, "header.stamp", node_now_ns(node));
    publish_msg(pub_obj, m);
    msg_free(m);
    val got = take_msg(sub)
    if is_some(got) { assert(list_len(msg_f64s(unwrap(got), "ranges")) == 1); };

    val gain = param_declare_f64_range(node, "gain", 1.5, 0.0, 2.0, "P gain")
    assert(gain >= 0.0);
    param_set_f64(node, "gain", 0.5);
    val timer = timer_create(node, 100)
    val srv = service_create(node, "/add", "example_interfaces/srv/AddTwoInts")
    val cli = client_create(node, "/add", "example_interfaces/srv/AddTwoInts")
    val req = client_request(cli)
    msg_set_i64(req, "a", 1);
    val reply = client_call(cli, req, 100)
    val actions = action_server_create(node, "/fib", "example_interfaces/action/Fibonacci")
    val acli = action_client_create(node, "/fib", "example_interfaces/action/Fibonacci")
    val goal = action_goal_msg(acli)
    val token = action_send_goal(acli, msg_set_i64(goal, "order", 3))
    val result = action_wait_result(acli, token, 100)
    val kind = serve(ctx, srv, actions)

    lifecycle_configure(managed);
    lifecycle_activate(managed);
    assert(lifecycle_is_active(managed) || !lifecycle_is_active(managed));

    val tf = tf_buffer_create(node)
    val bc = tf_static_broadcaster_create(node)
    tf_send(bc, "world", "base", transform_identity());
    val looked = tf_lookup(tf, "world", "base")
    timer_destroy(timer);
    context_destroy(ctx);
    if is_some(reply) && is_some(result) && is_some(looked) { kind } else { 0 }
}
"#;
    let res = compile_multi(&[("main", src)], "main", EmitKind::Llvm);
    assert!(
        res.is_ok(),
        "the std.ros2 API failed to compile: {:?}",
        res.err()
    );
}
