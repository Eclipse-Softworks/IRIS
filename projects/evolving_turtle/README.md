# Evolving turtle

A robot in a ROS 2 simulator keeps driving while its controller is replaced.
turtlesim's `turtle1` is the plant: [ros2_plant.iris](ros2_plant.iris)
publishes each command on `/turtle1/cmd_vel`, lets one control period of real
time pass while it reads `/turtle1/pose`, and returns what the simulator
measured. `iris evolve-live` runs the same loop it runs for the
[cart](../evolving_cart_controller/README.md):

1. every 32 ms it leases the active generation of
   `policy(error, velocity) -> f64`, asks it for a forward speed, and sends
   that speed to the turtle;
2. between steps it offers candidate controllers to the evolution
   coordinator, which hot-swaps one in only after all seven gates (tests,
   rollback probe, canary against the optimal law, constitution,
   allocation-free proof, ORC activation);
3. a promoted controller that commands more than the track's 2 units/s limit
   is rolled back in that same step, and the step is recomputed with the
   restored generation;
4. every decision is appended to a hash-chained audit log, verified at the
   end.

| File | Role |
| --- | --- |
| [ros2_plant.iris](ros2_plant.iris) | The plant: turtle1 over ROS 2 (`std.ros2`), connected on the first call and kept in program state |
| [reference.iris](reference.iris) | Derives the LQR-optimal speed law for the turtle (discrete Riccati), checks the saturated loop settles, writes `cases.json` |
| [baseline.iris](baseline.iris) | Generation 1: `0.2 * error` |
| [candidates/with_history.iris](candidates/with_history.iris) | Allocates: refused by the allocation-free proof |
| [candidates/lqr_unclamped.iris](candidates/lqr_unclamped.iris) | Optimal near the operating point, unsaturated: promoted, then rolled back |
| [candidates/lqr_saturated.iris](candidates/lqr_saturated.iris) | Optimal and saturated at 2 units/s: stays |
| [constitution.txt](constitution.txt) | The trusted rule, pinned by SHA-256 |
| [cyclonedds_local.xml](cyclonedds_local.xml) | Cyclone DDS on loopback, so Docker, WSL or VPN adapters cannot split the two processes |
| [run.ps1](run.ps1) / [run.sh](run.sh) | The one command: start turtlesim, run the loop, check the outcome, stop turtlesim |

## Run

Needs ROS 2 Humble with turtlesim, the IRIS ROS 2 bridge
(`python build_ros2_bridge.py`, see [docs/ros2-build.md](../../docs/ros2-build.md))
and an `iris` built with LLVM ORC.

```powershell
powershell -ExecutionPolicy Bypass -File projects/evolving_turtle/run.ps1
```

```bash
source /opt/ros/humble/setup.bash
projects/evolving_turtle/run.sh
```

`ROS2_DIR` names the ROS 2 install on Windows (default
`C:\dev\ros2_humble\ros2-windows`). The script exits non-zero unless the run
shows the whole story below. `tests/evolving_turtle.rs` runs the same thing
under `cargo test` wherever turtlesim and the bridge are available, and
skips with a message elsewhere.

## What was observed (2026-10-20, Windows, ROS 2 Humble, Cyclone DDS)

```text
evolve-live: 4 setpoints held 6 s each, dt 0.032 s, actuator rating [-2, 2]
[t=   0.00s] installed       generation 1: baseline controller
[t=   4.00s] offered         with_history: 664 bytes of IRIS
[t=   4.00s] rejected        with_history: evolution gate 'resource_and_abi' failed: allocation-free proof failed: ... function 'policy' uses dynamic collection which is outside the allocation-free embedded profile
[t=   8.00s] offered         lqr_unclamped: 708 bytes of IRIS
[t=   8.00s] promoted        lqr_unclamped: generation 2 after 7/7 gates (canary error 15854394240 -> 0)
[t=   8.00s] rolled back     generation 2: commanded 9.351 at error 3.893, velocity 0.786: outside [-2, 2]; generation 1 restored
[t=  12.00s] offered         lqr_saturated: 779 bytes of IRIS
[t=  12.00s] promoted        lqr_saturated: generation 3 after 7/7 gates (canary error 15854394240 -> 0)

setpoint  final error  settled        peak force  generation
   2.000      +0.4271  no                  0.400  1
   8.000      +1.1213  no                  2.000  3
   3.000      +0.0000  after 2.40 s        2.000  3
   6.000      +0.0000  after 2.02 s        2.000  3

752 steps; controller call mean 2746 ns, worst 21500 ns
2 promoted, 1 rejected, 1 rolled back; active generation 3
audit chain verified: head 23:1324d7a19cc44f21d8fa9709d0d883ec70cb78956a308e8e077f7c121e992c2a
evolving turtle: refused, promoted, rolled back, promoted, audit verified
```

The first two moves end unsettled because the sluggish baseline drives
them; the saturated optimal law, in from 12 s, settles each later move in
about two seconds. Two consecutive runs told the same story; the numbers
differ slightly because the simulator runs in real time.

The same run on Linux (2026-10-20, ROS 2 Humble on Ubuntu 22.04, Fast DDS,
turtlesim headless, in Docker via `IRIS_LINUX_STEPS=flagship
tools/ros2/linux_check.sh`): the same five events; the later moves settled
after 2.56 s and 2.37 s; `tests/evolving_turtle.rs` passed in the same
container.

## Limits

- **A simulator, not hardware.** turtlesim moves the turtle at exactly the
  commanded speed; a real robot has motor dynamics, delay and noise.
- **One axis.** The turtle drives along a straight line; heading is not
  controlled, and the controller ABI is `(error, velocity) -> speed`.
- **The plant is trusted.** It is compiled and run, not gated: only the
  controllers go through the seven gates.
- **Real time, not deterministic.** While a candidate is compiled and gated
  the loop pauses, and turtlesim keeps applying the last command (it stops
  after 1 s without one).
