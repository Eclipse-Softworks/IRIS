# IRIS direction

IRIS aims to let developers build learning systems from mathematical models to
real-time control in one language, with checked adaptation as they run.

The intended user builds ML-powered scientific software, autonomous systems,
or robotics applications and needs expressive mathematics and low-level control.
The central question is whether they can express, differentiate, train, evaluate,
and deploy a model without rewriting application kernels or control logic in
another language. Compiler and platform dependencies are separate from that
application-language boundary.

## First evidence

The [interactive learning scheduler](../projects/learning_scheduler/README.md)
provides a software-only application: terminal controls, cost-model training,
held-out scheduling comparisons, bounded CPU execution, model fallback, and
checkpoint save/load across process restarts. An external verification tool
records process CPU time and sampled peak memory. Its queue metrics are simulated
and its training labels are generated work costs; live arrivals and measured-cost
learning remain future work.

The [learning robot](../projects/learning_robot/README.md) joins simulated physics,
autodiff, training, control, candidate evaluation, and model restoration in one
executable IRIS program. Its documented limitations define the next work; it
does not establish broad performance advantages or hardware readiness.

The [trajectory workflow](../projects/learning_robot/WORKFLOW.md) now adds
observation-only training, sustained-error retraining triggers, separate candidate
selection and held-out scenarios, fixed feedback comparisons, continuous-state
recovery, validated checkpoints, and coarse batch timing. Native and interpreter
checks cover the learning and persistence workflows, including five noisy-data
training trials. Learned coefficients pass a fixed-point command comparison,
export to a shared scalar kernel, and cross-compile for Uno. The generated policy
passes desktop self-tests but has not been executed on the kit.

Training still runs between episodes. Streaming/background updates, precise
timing and allocation measurements, crash-safe persistence, and hardware
validation remain open. Software-only systems are the immediate focus; hardware
is optional. The documented Uno kit remains a possible hardware target, with
host-side learning followed by scalar board-side control.

## Priorities

1. Make the complete learning workflow reliable: numerical operations,
   differentiation, training, evaluation, and understandable diagnostics.
2. Train from collected trajectories and evaluate on unseen dynamics and targets,
   including noise and failures. Compare with fixed-model and conventional
   feedback controllers on identical episodes.
3. Save and restore model state; measure allocation, inference time, and control
   jitter with training separated from the control loop.
4. Integrate a physical actuator with explicit operating limits and a fallback
   controller, measuring deadline misses and tracking error.
5. Explore architecture and code evolution after parameter adaptation is
   reproducible, with candidate checks and recovery demonstrated end to end.

New features should support a measured need in this workflow. AIS provides a
place for monitoring, decision policies, and adaptation; application-specific
evaluation remains necessary. General-purpose language features support this
direction, while service demonstrations remain supporting experiments.

Success means a developer can build and debug a useful learning system entirely
in IRIS application code, with measured accuracy, resource use, and recovery
behavior. This is the project's direction, not a claim that every part exists.
