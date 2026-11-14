"""Interoperability check of the IRIS ROS 2 bridge against real ROS 2 peers.

Drives the bridge directly (through ctypes, the same C ABI std.ros2 calls)
while the `ros2` CLI and the C++ demo nodes talk to it, in both directions:
topic echo, service call, param get/set/describe, lifecycle get/set, action
send_goal and tf2_echo against IRIS; demo talker, add_two_ints_server,
action_tutorials fibonacci_action_server and static_transform_publisher
served to IRIS.

Run inside a sourced ROS 2 environment, from anywhere:

    python tools/ros2/interop_check.py

ROS2_DIR names the ROS 2 install (default: the sourced one, from
AMENT_PREFIX_PATH). Windows and Linux; the demo executables are looked up
under <ROS2_DIR>/lib/<package>/. On Linux the C++ demos come from the
ros-<distro>-demo-nodes-cpp, ros-<distro>-action-tutorials-cpp and
ros-<distro>-tf2-ros packages.
"""
import ctypes
import json
import os
import signal
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from bridge_ctypes import Bridge  # noqa: E402

REPO = os.path.dirname(os.path.dirname(HERE))
ROS = os.environ.get("ROS2_DIR") or os.environ.get("AMENT_PREFIX_PATH", "").split(os.pathsep)[0]
if not ROS:
    sys.exit("source a ROS 2 environment (or set ROS2_DIR) first")
DEFAULT_LIB = "iris_ros2.dll" if os.name == "nt" else "libiris_ros2.so"
b = Bridge(os.environ.get("IRIS_ROS2_LIB", os.path.join(REPO, DEFAULT_LIB)))


def demo(package, name):
    """A ROS 2 executable from <ROS>/lib/<package>, quoted for the shell."""
    path = os.path.join(ROS, "lib", package, name + (".exe" if os.name == "nt" else ""))
    return f'"{path}"'
failures = []
ev = (ctypes.c_int64 * 6)()


def check(cond, what, detail=None):
    print(("ok   " if cond else "FAIL ") + what, flush=True)
    if not cond:
        failures.append(what)
        if detail:
            print("     ---\n     " + detail.replace("\n", "\n     ")[:2000])


ctx = b.context_create("", -1)
node = b.node_create(ctx, "iris_interop", "", "", 0)
managed = b.node_create(ctx, "iris_managed", "", "", 1)
b.param_declare(node, "gain", 3, 1.5, json.dumps({"min": 0.0, "max": 2.0, "description": "P gain"}))
chatter = b.publisher_create(node, "/iris/chatter", "std_msgs/msg/String", 1, 10, 1, 2)
listener = b.subscription_create(node, "/chatter", "std_msgs/msg/String", 1, 10, 1, 2)
adder = b.service_create(node, "/iris/add_two_ints", "example_interfaces/srv/AddTwoInts")
fib_server = b.action_server_create(node, "/iris/fibonacci", "example_interfaces/action/Fibonacci", 0, 0, 0)
static_bc = b.tf_broadcaster_create(node, 1)
tfbuf = b.tf_buffer_create(node, 0)
pub_timer = b.timer_create(node, 100_000_000, 0)
cell7 = (ctypes.c_double * 8)(0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0)
b.tf_send(static_bc, "world", "iris_frame", 0, ctypes.addressof(cell7))

events = []          # (kind, text) of interesting events
pending_goals = {}   # goal handle -> (order, progress list, next step time)
received_chatter = []


def handle(kind):
    if kind == 2 and ev[1] == pub_timer:
        m = b.publisher_new_msg(chatter)
        b.msg_set_str(m, "data", "hello from iris")
        b.publish(chatter, m)
        b.msg_free(m)
    elif kind == 1 and ev[1] == listener:
        received_chatter.append(b.text(b.msg_get_str(ev[2], "data")))
    elif kind == 3 and ev[1] == adder:
        total = b.msg_get_i64(ev[2], "a") + b.msg_get_i64(ev[2], "b")
        b.msg_set_i64(ev[4], "sum", total)
        b.service_reply(adder, ev[3])
        events.append(("request", total))
    elif kind == 5:
        events.append(("parameter", b.text(b.event_text(ctx))))
    elif kind == 6:
        events.append(("transition", b.text(b.event_text(ctx))))
    elif kind == 7 and ev[1] == fib_server:
        order = b.msg_get_i64(ev[2], "order")
        pending_goals[ev[3]] = [order, [0, 1], time.time()]
        events.append(("goal", order))


def step_goals():
    now = time.time()
    for g, state in list(pending_goals.items()):
        order, seq, due = state
        if now < due:
            continue
        if len(seq) <= order:
            seq.append(seq[-1] + seq[-2])
            fb = b.goal_feedback(g)
            b.msg_set_json(fb, "sequence", json.dumps(seq))
            b.goal_publish_feedback(g)
            state[2] = now + 0.1
        else:
            b.msg_set_json(b.goal_result(g), "sequence", json.dumps(seq))
            b.goal_finish(g, 4)
            del pending_goals[g]


def kill_tree(proc):
    if proc.poll() is None:
        if os.name == "nt":
            subprocess.run(f"taskkill /T /F /PID {proc.pid}", stdout=subprocess.DEVNULL,
                           stderr=subprocess.DEVNULL)
        else:
            # Started in its own session: end the shell and everything it ran.
            try:
                os.killpg(proc.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        proc.wait()


def start(cmd):
    """Start a background command whose output goes to a temp file."""
    log = tempfile.TemporaryFile()
    proc = subprocess.Popen(cmd, stdout=log, stderr=subprocess.STDOUT, shell=True,
                            start_new_session=(os.name != "nt"))
    proc.log = log
    return proc


def output_of(proc):
    proc.log.seek(0)
    return proc.log.read().decode(errors="replace")


def cli(args, timeout=30, attempts=2):
    print("  ..", args[:90], flush=True)
    for attempt in range(attempts):
        proc = start(args)
        deadline = time.time() + timeout
        while proc.poll() is None and time.time() < deadline:
            handle(b.spin_once(ctx, 20, ctypes.addressof(ev)))
            step_goals()
        kill_tree(proc)
        out = output_of(proc)
        # `--no-daemon` CLI calls discover the graph from scratch and can
        # give up before they see a node that is there (seen once on Linux
        # with Fast DDS); that is the CLI's discovery, not the bridge, so
        # try once more.
        if "Node not found" not in out or attempt + 1 == attempts:
            return out
        print("     (node not discovered yet; retrying)", flush=True)
    return out


def spin_for(seconds):
    deadline = time.time() + seconds
    while time.time() < deadline:
        handle(b.spin_once(ctx, 20, ctypes.addressof(ev)))
        step_goals()


spin_for(1.0)

# ---- IRIS serves, the ROS 2 CLI consumes ------------------------------------------
out = cli("ros2 topic echo /iris/chatter std_msgs/msg/String --once")
check("data: hello from iris" in out, "ros2 topic echo receives IRIS's publications", out)

out = cli('ros2 service call /iris/add_two_ints example_interfaces/srv/AddTwoInts "{a: 2, b: 40}"')
check("sum=42" in out, "ros2 service call is answered by IRIS", out)

out = cli("ros2 param get --no-daemon /iris_interop gain")
check("Double value is: 1.5" in out, "ros2 param get reads an IRIS parameter", out)
out = cli("ros2 param set --no-daemon /iris_interop gain 0.75")
spin_for(0.3)
check("Set parameter successful" in out and abs(b.param_get_f64(node, "gain") - 0.75) < 1e-12,
      "ros2 param set changes it", out)
check(("parameter", "gain") in events, "IRIS sees EVENT_PARAMETER for the external change")
out = cli("ros2 param set --no-daemon /iris_interop gain 5.0")
check("Setting parameter failed" in out and abs(b.param_get_f64(node, "gain") - 0.75) < 1e-12,
      "the descriptor's range rejects 5.0", out)
out = cli("ros2 param describe --no-daemon /iris_interop gain")
check("P gain" in out and "Min value: 0.0" in out, "ros2 param describe shows the descriptor", out)

out = cli("ros2 lifecycle get /iris_managed")
check("unconfigured [1]" in out, "ros2 lifecycle get", out)
out = cli("ros2 lifecycle set /iris_managed configure")
spin_for(0.3)
check("Transitioning successful" in out and b.lifecycle_state(managed) == 2,
      "ros2 lifecycle set configure drives the IRIS lifecycle node", out)
check(("transition", "configure") in events, "IRIS sees EVENT_TRANSITION configure")

out = cli('ros2 action send_goal /iris/fibonacci example_interfaces/action/Fibonacci "{order: 5}" --feedback', 60)
check("Goal finished with status: SUCCEEDED" in out and "Feedback:" in out,
      "ros2 action send_goal: IRIS action server with feedback", out)

out = cli(f'{demo("tf2_ros", "tf2_echo")} world iris_frame', 6)
check("Translation: [0.000, 0.000, 1.000]" in out, "tf2_echo sees IRIS's /tf_static", out)

# ---- C++ demo nodes serve, IRIS consumes ------------------------------------------------
print("  .. talker", flush=True)
talker = start(f'{demo("demo_nodes_cpp", "talker")}')
deadline = time.time() + 15
while not received_chatter and time.time() < deadline:
    handle(b.spin_once(ctx, 50, ctypes.addressof(ev)))
kill_tree(talker)
check(any(t.startswith("Hello World") for t in received_chatter),
      f"IRIS subscription hears demo_nodes_cpp talker ({received_chatter[:2]})", output_of(talker))

print("  .. add_two_ints_server", flush=True)
server = start(f'{demo("demo_nodes_cpp", "add_two_ints_server")}')
client = b.client_create(node, "/add_two_ints", "example_interfaces/srv/AddTwoInts")
ready = b.client_wait_service(client, 10000)
req = b.client_new_request(client)
b.msg_set_i64(req, "a", 20)
b.msg_set_i64(req, "b", 22)
tok = b.client_send(client, req)
resp = b.client_wait_response(client, tok, 10000)
total = b.msg_get_i64(resp, "sum") if resp > 0 else None
kill_tree(server)
check(ready == 1 and total == 42,
      f"IRIS client calls demo_nodes_cpp add_two_ints_server (ready {ready}, sum {total})",
      output_of(server))

print("  .. fibonacci_action_server", flush=True)
fib = start(f'{demo("action_tutorials_cpp", "fibonacci_action_server")}')
acli = b.action_client_create(node, "fibonacci", "action_tutorials_interfaces/action/Fibonacci")
ready = b.action_wait_server(acli, 10000)
goal = b.action_new_goal(acli)
b.msg_set_i64(goal, "order", 5)
tok = b.action_send_goal(acli, goal)
feedbacks = 0
result = None
deadline = time.time() + 30
while result is None and time.time() < deadline:
    k = b.spin_once(ctx, 50, ctypes.addressof(ev))
    if k == 10 and ev[1] == acli:
        feedbacks += 1
    elif k == 11 and ev[1] == acli:
        result = (ev[4], b.text(b.msg_json(ev[2], "sequence")))
    else:
        handle(k)
kill_tree(fib)
check(ready == 1 and result == (4, "[0,1,1,2,3,5]") and feedbacks > 0,
      f"IRIS action client drives action_tutorials_cpp server (result {result}, {feedbacks} feedback)",
      output_of(fib))

print("  .. static_transform_publisher", flush=True)
stp = start(
    f'{demo("tf2_ros", "static_transform_publisher")} --x 1 --y 2 --z 3 --frame-id world --child-frame-id cli_frame')
out7 = (ctypes.c_double * 8)()
found = 0
deadline = time.time() + 10
while not found and time.time() < deadline:
    handle(b.spin_once(ctx, 50, ctypes.addressof(ev)))
    found = b.tf_lookup(tfbuf, "world", "cli_frame", 0, ctypes.addressof(out7))
kill_tree(stp)
check(found == 1 and list(out7[:3]) == [1.0, 2.0, 3.0],
      f"IRIS tf2 listener sees static_transform_publisher ({list(out7[:3])})", output_of(stp))

print("  .. context_destroy", flush=True)
b.context_destroy(ctx)
print("  .. ros2 daemon stop", flush=True)
subprocess.run("ros2 daemon stop", shell=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
               timeout=60)
print()
print("FAILURES:", failures if failures else "none")
sys.exit(1 if failures else 0)
