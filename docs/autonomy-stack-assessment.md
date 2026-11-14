# AIS / ML / ROS 2 assessment

**Updated:** 2026-10-20
**Method:** function-level inventory plus interpreter, native MinGW and ORC
execution; ROS 2 transport was exercised against the installed Humble SDK.

## Scorecard

| Module | Public functions | Rating | Current verdict |
| --- | ---: | ---: | --- |
| `std.ais` | 136 | 8.5/10 | Active inference with learning, an online world model, a safety policy and an autopilot that compose, each demonstrated on both backends |
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

## `std.ais` v3

v2 described the computations; several of its parts took the hard terms as
arguments (ambiguity and epistemic value were supplied by the caller), and the
constitution checked names. v3 computes them from models, learns the models,
and composes the parts. What each does, and the evidence:

| Capability | Implementation | Evidence (both backends) |
| --- | --- | --- |
| Belief over hidden state | exact Bayesian filtering: predict through the transition model, update on the observation | `test_ais_reference.iris`: posterior matches an independent Python implementation |
| Choosing by expected free energy | multi-step policies; risk, ambiguity, information gain (mutual information), expected utility and parameter novelty computed from the model; policy posterior softmax(-gamma G) | 16 T-maze policies match the reference to 1e-9, both as -utility - information gain and as KL risk + ambiguity |
| Information-seeking behaviour | the epistemic term | T-maze: 100/100 trials in the rewarded arm against 53/100 for a reward-only agent (`test_ais_tmaze.iris`) |
| Learning the model | Dirichlet counts for likelihood and transitions, updated by the posterior | learned likelihood and transitions within 0.03 of the world's; the novelty drive covers an unexplored corridor in the minimum 5 steps against 37 for a random walk (`test_ais_learning.iris`) |
| Continuous world model | recursive least squares with forgetting and a bounded covariance | identifies a plant to 1e-7; matches the closed-form ridge solution (`test_ais_world_model.iris`, `test_ais_reference.iris`) |
| Viability under changing dynamics | `viability_choose` over model rollouts, with an uncertainty bonus that only breaks near-ties | an autopilot that starts knowing nothing keeps a tank in band for 280 of 280 steps through a heat wave; a thermostat is critical for 110 (`test_ais_world_model.iris`, `examples/08_ais/autopilot.iris`) |
| Safety constraints | rules over the state and over the model's predictions, required actions, an audit trail; planning restricted to permitted actions | `test_ais_safety.iris` |
| Autonomic governor | `software_governor_step`: rules over named metrics, constitution clearance, homeostat health in units of tolerance | `test_ais_safety.iris` |
| Continual learning | EWC with Fisher information from per-sample gradients | a first task's loss kept at 0.011 where plain training forgets it to 2.04 (`test_ais_ewc.iris`) |
| Persistence | save and load learned models | reloaded models identical and deciding identically (`test_ais_persistence.iris`) |

Limits: discrete models are flat (one state factor, one observation
modality), so a large problem must be encoded as their product; policies are
enumerated, so horizon times branching must stay small; the world model is
linear in its features; the safety policy's prediction rules are only as good
as the model; nothing here is a certified safety case.

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

This is no longer a publish-only binding: `std.ros2` provides a comprehensive rclcpp bridge covering topics of any message type, services, actions, parameters, lifecycle nodes, and time-aware tf2 transform lookup (matching `docs/ros2-build.md` and verified on Windows and Linux). The remaining middleware boundaries are explicit:

- macOS remains unbuilt (Linux Humble Fast DDS and Windows Humble Cyclone DDS verified);
- multi-threaded and callback-driven executors (executor is strictly single-threaded per context);
- decisions are preset policies rather than arbitrary callbacks inside rclcpp.

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
2. Complete multi-threaded callback executor and native macOS build for `std.ros2` (services, actions, parameters, lifecycle, and tf2 transport verified on Linux and Windows).
3. Add `std.filter`, `std.spatial`, `std.control` and `std.planning` as a coherent
   estimation/control layer.
4. Run long-duration native/JIT soak, fuzz and sanitizer gates before a stable
   release claim.
