# ROS 2 in IRIS

`std.ros2` runs on the real ROS 2 client stack. Its calls go through the IRIS
ROS 2 bridge (`iris_ros2.dll` / `libiris_ros2.so`), a C ABI over **rclcpp**,
**rclcpp_action**, **rclcpp_lifecycle** and **tf2_ros**, built from
[`src/runtime/ros2`](../src/runtime/ros2). Nothing in it re-implements ROS 2:

- topics use rclcpp's generic (runtime-typed) publishers and subscriptions,
  and messages are the real C++ message structs, laid out by ROS 2's own C++
  introspection typesupport, so any installed message type works;
- services, clients, action servers and action clients subclass rclcpp's and
  rclcpp_action's type-erased base classes, so the protocol is rclcpp's;
- parameters are the node's rclcpp parameters, lifecycle nodes are
  rclcpp_lifecycle nodes, and transforms come from tf2 buffers fed by a
  tf2_ros TransformListener.

Validated on 2026-10-18 against ROS 2 Humble (Windows binary release,
`rmw_cyclonedds_cpp`) at `C:\dev\ros2_humble\ros2-windows`.

## What is verified

"Live" means `tests/ros2_live.rs`, run as a native binary and through ORC;
"interop" means [`tools/ros2/interop_check.py`](../tools/ros2/interop_check.py)
against the `ros2` CLI and the C++ demo nodes (16 checks, all passing).

| Capability | Grade | Evidence and limits |
|---|---|---|
| Publish/subscribe, any message type | Verified | Live (Twist, LaserScan, Imu); interop: `ros2 topic echo` hears IRIS, IRIS hears `demo_nodes_cpp talker`. Exact f64s both ways. |
| Field paths, sequences, JSON, schema | Verified | Live: `ranges[3]` append, bulk `msg_f64s`, `msg_from_json` (unknown fields rejected), `msg_schema`. |
| QoS | Verified | History, depth, reliability and durability only; deadline, lifespan and liveliness are not exposed. |
| Services and clients | Verified | Live (in-process); interop: `ros2 service call` answered by IRIS, IRIS calls `add_two_ints_server`. Replies may be deferred to a later spin. |
| Parameters | Verified | Live; interop: `ros2 param get/set/describe`, a descriptor's range rejects an out-of-range `ros2 param set`, and IRIS sees EVENT_PARAMETER for external changes. |
| Timers and clock | Verified | Live (wall timer); node clock is ROS time. `use_sim_time` is the standard parameter; **sim time is not exercised with a /clock publisher.** |
| Lifecycle nodes | Verified | Live; interop: `ros2 lifecycle get/set configure` drives an IRIS node, which sees EVENT_TRANSITION. Publishers on an inactive lifecycle node drop messages, as rclcpp_lifecycle's do. |
| tf2 | Verified | Live: chained and inverse lookups in a local buffer, `/tf_static` broadcaster to listener; interop: `tf2_echo` sees IRIS's frames, IRIS sees `static_transform_publisher`. Lookups do not block. |
| Actions | Verified | Live: goal, feedback, result, cancel; interop: `ros2 action send_goal --feedback` against IRIS, IRIS against `action_tutorials_cpp fibonacci_action_server`. |
| Original `iris_rcl_*` API and `std.ros2` functions | Verified | `examples/10_integration/ros2_topics.iris` and the bridge smoke test. |
| Linux | Verified | Built and run on ROS 2 Humble, Ubuntu 22.04 (2026-10-20, `tools/ros2/linux_check.sh` in Docker): the bridge builds with `build_ros2_bridge.py`, `tests/ros2_live.rs` passes native and through ORC, and `tools/ros2/interop_check.py` passes all 16 checks. **macOS is not built.** |
| Fast DDS | Verified (Linux) | The Linux run uses the image's default RMW, Fast DDS: the live tests and all 16 interop checks pass on it. On the Windows validation machine `rmw_fastrtps_cpp.dll` does not load (a dependency of the install is missing), so Windows was verified with Cyclone DDS only. |

## Build

```powershell
python build_ros2_bridge.py
```

The script runs CMake in a sourced ROS 2 environment (it sources
`local_setup.bat` / `setup.bash` itself when none is active), builds
`src/runtime/ros2` with the toolchain the SDK was built with (Visual Studio
2022 on Windows), and copies the library to the repository root. Overrides:

- `ROS2_DIR` -- the ROS 2 install (default: the sourced one, else
  `C:\dev\ros2_humble\ros2-windows` or `/opt/ros/humble`);
- `ROS2_PYTHON` -- the Python the Windows SDK's CMake config needs (Humble:
  3.8, default `C:\Python38`);
- `CMAKE_GENERATOR`.

`--clean` rebuilds from scratch.

## Run

Run IRIS inside a sourced ROS 2 environment, or at least with the ROS 2 `bin`
directory on `PATH` (the bridge then finds the rest of the install from where
rclcpp was loaded). The bridge library is looked up as `iris_ros2.dll`, then
`libiris_ros2.so`, in the usual library search path; `IRIS_ROS2_LIB` names it
explicitly.

```powershell
call C:\dev\ros2_humble\ros2-windows\local_setup.bat
iris run examples\10_integration\ros2_node.iris
```

`ros2_available()` reports whether the bridge loads; every other function
panics with instructions when it does not.

## The programming model

IRIS code cannot run inside an rclcpp callback, so callbacks become events.
Every context owns an rclcpp `SingleThreadedExecutor`; every node created in
it is added to that executor. `spin_once(ctx, timeout_ms)` runs it until
something happens and returns a `ROS2Event`:

```iris
bring std.ros2

def main() -> i64 {
    val ctx = context_create()
    val node = node_create(ctx, "adder", "")
    val srv = service_create(node, "/add_two_ints", "example_interfaces/srv/AddTwoInts")
    val ticker = timer_create(node, 500)
    var served = 0
    while served < 3 {
        val ev = spin_once(ctx, 1000)
        if is_request(ev, srv) {
            val req = request_of(ev)
            msg_set_i64(response_of(ev), "sum", msg_i64(req, "a") + msg_i64(req, "b"));
            assert(service_reply(srv, ev));
            served = served + 1
        };
        if is_timer(ev, ticker) { log_info(node, "waiting for requests"); };
    };
    assert(context_destroy(ctx));
    0
}
```

| Event | `source` | Payload |
|---|---|---|
| `EVENT_MESSAGE` | subscription | `message` |
| `EVENT_TIMER` | timer | `id`: times fired |
| `EVENT_REQUEST` | service | `message`: request, `aux`: response, `id`: token |
| `EVENT_RESPONSE` | client | `message`: response, `id`: token |
| `EVENT_PARAMETER` | node | `event_text(ctx)`: name; only changes from outside the program |
| `EVENT_TRANSITION` | node | `id`: `ON_*` callback, `aux`: what it returned |
| `EVENT_GOAL`, `EVENT_CANCEL` | action server | `id`: goal, `message`: its request |
| `EVENT_GOAL_RESPONSE`, `EVENT_FEEDBACK`, `EVENT_GOAL_STATUS`, `EVENT_RESULT`, `EVENT_CANCEL_RESPONSE` | action client | `id`: goal token |

Because the program answers after the callback has returned, decisions rclcpp
wants inside a callback are **policies set in advance**:

- a goal is accepted and executed, accepted and deferred (`goal_execute`
  later), or rejected (`action_server_create_with`, with a `max_goals` cap);
- a cancel request is accepted (the goal reads `GOAL_CANCELING` and the
  program finishes it with `goal_canceled`) or rejected;
- a lifecycle callback returns what `lifecycle_set_result` last set
  (success by default);
- a parameter set is validated by its descriptor (`param_declare_*_range`,
  read-only, JSON descriptors), not by program code.

Waiting helpers (`wait_for_message`, `client_call`, `action_wait_result`,
`executor_spin_once`) run the same executor; events they do not consume stay
queued for `spin_once`. The queue holds at most 10,000 events; older ones are
dropped (`iris_ros2_dropped_events` counts them).

## Messages

A message is a handle to a real C++ message struct of any installed type:

```iris
val scan = publisher_msg(scan_pub)                // or msg_new("sensor_msgs/msg/LaserScan")
msg_set_str(scan, "header.frame_id", "laser");
msg_set_stamp(scan, "header.stamp", node_now_ns(node));
msg_set_f64s(scan, "ranges", ranges);             // a list<f64>
msg_set_f64(scan, "ranges[3]", 2.5);              // index == length appends
assert(publish_msg(scan_pub, scan));
msg_free(scan);
```

Getters convert between numeric kinds where exact (`msg_f64`, `msg_i64`,
`msg_bool`, `msg_str`, `msg_f64s`, `msg_len`, `msg_stamp`); any field reads as
JSON with `msg_get_json` and writes with `msg_set_json`. An unknown field, a
type mismatch or an out-of-range integer panics with the path and the type's
field list.

Lifetimes: a message an event or a take delivers lives until the next
`spin_once` on its context (a take's, also until the next take on that
subscription; a synchronous call's, until the next call on that client).
`msg_clone` keeps one. Messages from `msg_new`, `msg_from_json`,
`publisher_msg`, `client_request` and `action_goal_msg` belong to the
program and are freed with `msg_free`. A stale handle is an error, never a
crash: handles index a registry and are validated on every call.

## ABI notes

The bridge exports plain C functions taking int64 slots, the only argument
class `ffi_call_i64` carries. Strings arrive as `const char *`; doubles arrive
as their bit pattern, which both IRIS backends pass for an `f64` argument, so
no value is rounded. Text comes back through a per-thread result buffer that
`std.ros2` copies into an out-cell; the bridge never returns memory IRIS would
have to free. f64 results use `ffi_call_f64` (at most two arguments, the
native dispatcher's limit).

The original `iris_rcl_*` functions remain, reimplemented on the new objects:
they still scale fractional values by 1e8, as their callers expect.

The C++ service and action typesupports are exported only as template
specialisations (`rosidl_typesupport_cpp::get_service_type_support_handle<T>`),
so the bridge resolves them by mangled name (MSVC, with an export-table
fallback, or Itanium).

rclcpp_action's `ClientBase::wait_for_action_server` (Humble) gives up at once
on a non-default context, because its wait loop consults the global context;
`action_wait_server` waits on the node's graph events instead.
