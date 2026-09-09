# AIS / ML / ROS 2 assessment

**Updated:** 2026-08-31
**Method:** function-level inventory plus interpreter, native MinGW and ORC
execution; ROS 2 transport was exercised against the installed Humble SDK.

## Scorecard

| Module | Public functions | Rating | Current verdict |
| --- | ---: | ---: | --- |
| `std.ais` | 74 | 8/10 | Differentiated autonomy layer with explicit viability and active inference |
| `std.ml` | 87 | 7/10 | Broad learning/inference layer; robotics estimation remains incomplete |
| `std.ros2` | 57 | 6/10 | Real typed topic I/O and QoS; services/actions and wire-level tf2 remain gaps |

## `std.ais` v2

The original homeostasis, EWC, intrinsic motivation, decision strategies,
neuroevolution, constitution checks, multi-agent consensus and MAPE-K building
blocks remain. V2 adds an explicit embodied-autonomy decision path:

- `ViabilityBound` and predicted-horizon `viability_assess`;
- critical-envelope and risk-based self-preservation pre-emption;
- expected free energy split into pragmatic risk, ambiguity and epistemic
  value;
- policy selection with a confidence margin;
- `autonomy_v2_step`, which selects a corrective action before goal-seeking
  whenever predicted viability is critical.

The AIS contract is asserted in `tests/autonomy_v2_end_to_end.rs` and returns the
same decision in interpreter, direct MinGW native and ORC JIT execution.

Remaining gaps are safety-case generation, uncertainty propagation through all
policies, persistent world models, and hard real-time scheduling evidence.

## Differentiable learning layer

`std.tensor` and `std.nn` now support reverse-mode gradients through closures,
branches, loops and trait dispatch. Tensor/NN contracts run through interpreter,
native and ORC backends. This is sufficient for differentiable control programs
and small training loops; it is not yet a replacement for the optimized kernel
coverage and distributed training systems of mature frameworks.

Robotics-specific estimation and planning are still the largest ML/control
gaps: EKF/UKF, particle filtering, SLAM bindings, occupancy grids, trajectory
planning and MPC are not yet a coherent standard-library stack.

## `std.ros2` v2

Implemented and live-validated:

- context, node, publisher and subscription lifecycle;
- typed publish/wait/take for Float64, Int64, String, Vector3, Twist and Pose;
- QoS history, depth, reliability and durability profiles;
- deterministic single-threaded subscription polling;
- IRIS-side managed lifecycle state;
- quaternion operations, rigid-transform composition/inversion and a local
  direct/inverse transform buffer.

Earlier live sessions validated scalar/string and signed geometry payloads
against ROS 2 Humble. The refreshed `examples/10_integration/ros2_topics.iris`
checks a Vector3 round trip when the middleware is configured;
`ros2_geometry.iris` provides the separately executable pure geometry check.
The catalog does not treat compile-only SDK checks as fresh live validation.

This is no longer a publish-only binding: IRIS can run a closed topic loop and
consume its payload. The remaining middleware gaps are important and explicit:

- services and actions;
- parameters and simulated time;
- standard sensor/nav message families;
- `/tf` and `/tf_static` transport plus time-aware chained lookup;
- ROS-managed lifecycle protocol interoperability;
- multi-threaded and callback-driven executors.

## Safety and deployment position

IRIS now combines several useful properties in one autonomy stack:

1. static effect checking and transactional rollback for speculative actions;
2. checked integer arithmetic, scalar bounds checks, borrowing and ownership;
3. native compilation, ORC JIT and ABI-verified hot swapping with rollback;
4. differentiable tensors and active-inference/homeostasis primitives;
5. allocation-free cross-target proof and freestanding bundles for Cortex-M4F,
   Cortex-M33, ESP32-C3 and Arduino Uno.

The credible claim is a verifiable autonomy/control language with working ROS 2
topic transport—not a complete replacement for the ROS 2 client ecosystem.
Physical microcontroller execution was validated on a BDD Ultimate Starter Kit
V2 Uno clone: its ATmega328P signature, flashed image, content-bound fingerprint,
return code, allocation counter and fault counter were verified. The device was
COM9 on that host, but public instructions must use the port enumerated on the
reader's machine. The original temperature firmware was backed up and restored
during validation; the board was later flashed with the interactive IRIS
button/LED demo. This is evidence for the supported scalar/control-flow Uno
profile, not a general hard-real-time or safety certification.

## Next priorities

1. Add an AVR backend path that safely supports cyclic CFGs/fixed arrays, then
   expand physical validation to Cortex-M and ESP32-C3 boards.
2. Add ROS 2 services/actions, sensor messages and real tf2 transport.
3. Add `std.filter`, `std.spatial`, `std.control` and `std.planning` as a coherent
   estimation/control layer.
4. Run long-duration native/JIT soak, fuzz and sanitizer gates before a stable
   release claim.
