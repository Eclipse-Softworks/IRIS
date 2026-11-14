# Lifecycle-managed sensor fusion

A managed ROS 2 node, `/iris_fusion`, that fuses two range sensors into
`/fused/range` with a Kalman filter and an outlier gate. It is an
rclcpp_lifecycle node, so `ros2 lifecycle` drives it like any C++ managed
node:

| Transition | What the node does |
| --- | --- |
| configure | reads `process_noise`, `noise_a`, `noise_b`, `gate_sigma` (ranged parameters) and starts a filter |
| activate | `/fused/range` starts publishing |
| deactivate | publishing stops (the managed publisher drops readings) |
| cleanup | the filter is dropped |
| shutdown | the program ends (external mode) |

The same program also plays the robot: a driver node publishes
`/sensors/range_a` (noisy, 0.10 m) and `/sensors/range_b` (precise, 0.03 m,
with 5% multipath spikes of 0.8 m) at 20 Hz and watches `/fused/range`.

| File | Role | Needs |
| --- | --- | --- |
| [fusion.iris](fusion.iris) | Kalman filter, innovation gate, deterministic noise | -- |
| [simulate.iris](simulate.iris) | The filter on simulated sensors, asserted | nothing (both backends) |
| [main.iris](main.iris) | The lifecycle node, driver and monitor | ROS 2 and the bridge |

## Run

```powershell
iris run projects\lifecycle_sensor_fusion\main.iris
```

walks the node through every transition and checks each phase. To drive it
yourself instead:

```powershell
$env:IRIS_FUSION_EXTERNAL = "1"; iris run projects\lifecycle_sensor_fusion\main.iris
# another shell
ros2 lifecycle set /iris_fusion configure
ros2 lifecycle set /iris_fusion activate
ros2 topic echo /fused/range
ros2 param set /iris_fusion gate_sigma 5.0      # read at the next configure
ros2 lifecycle set /iris_fusion deactivate
ros2 lifecycle set /iris_fusion shutdown
```

## What was observed (2026-10-18, Windows, ROS 2 Humble, Cyclone DDS)

- Self-driven run, native and interpreted: nothing published while
  unconfigured; while inactive every fused reading was dropped; while active
  160 fused readings arrived with an RMS error of 0.013 m against 0.22 m for
  the spiky sensor; after deactivate none arrived; all 12 spikes were rejected;
  configure, activate, deactivate, cleanup and shutdown all arrived as events.
- External mode: `ros2 lifecycle set` configure, activate, deactivate and
  shutdown all reported "Transitioning successful", `ros2 lifecycle get` showed
  `inactive [2]` after deactivate, and `ros2 topic echo /fused/range --once`
  received a fused reading while active.
- `simulate.iris`: RMS 0.015 m fused against 0.10 m and 0.19 m raw; 32 of 32
  spikes rejected, no good reading rejected.

Limits: one-dimensional range; the filter's time base is the readings'
stamps, so out-of-order readings are fused as if simultaneous.
