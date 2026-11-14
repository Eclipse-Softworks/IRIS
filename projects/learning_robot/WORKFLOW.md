# Learning from trajectories

IRIS now has a runnable path from sensor observations through training,
drift-triggered adaptation, held-out evaluation, recovery, and model persistence.
The original `main.iris` remains the introductory four-probe experiment.

## Run

From the repository root:

```powershell
.\target\release\iris.exe run projects/learning_robot/trajectories.iris
.\target\release\iris.exe run projects/learning_robot/benchmark.iris
python tools/verify_learning.py --iris target/release/iris.exe --filter projects/learning_robot/
```

The verifier runs each entry in a temporary working directory. It also runs
`persistence.iris`, which writes `iris-robot-checkpoint.txt` and malformed-input
fixtures. To keep a checkpoint, run that entry from a chosen output directory
using absolute compiler and source paths. It overwrites those named files.

`noisy.iris` runs five different training/selection trajectory seeds with sensor
perturbations and nonlinear drag. `export.iris` writes a learned checkpoint and
generated policy; the preparation command below compiles that generated source.

## Data and adaptation

`system.iris` contains the model, simulator, collector, trainer, controller, and
update policy. Only the simulator reads plant parameters. The trainer accepts
observations `(velocity, command, next_velocity)` from continuous exploration
trajectories, with bounded pseudorandom commands and reproducible seeds.
Adjacent observations share the same measured state; the executable checks this.

Training uses scalar autodiff and mini-batch SGD: 30 passes, eight observations
per batch, learning rate 0.3. A function boundary limits each autodiff batch's
temporary storage. The initial large inline batch implementation exhausted the
native stack; this example does not resolve the compiler's general training
memory behavior or establish bounded long-running training memory.

The detector checks six disjoint 16-observation windows. Three consecutive
windows with prediction MSE above 0.002 request one retraining job. A 96-sample
buffer is collected first; training is synchronous between episodes. Stable
data, an isolated spike, and post-adaptation observations must not retrigger it.

Training/monitoring and candidate selection use different trajectories and seeds.
Promotion requires valid parameter bounds, at least 10% lower selection prediction
MSE, and better simulated tracking on selection scenario 0. A rejected job keeps
the current model. This validation still needs a known simulation plant; it is
not a deployable unknown-robot validation procedure.

Final scenarios are never passed to training or selection. They use different
targets, and two also change dynamics, add nonlinear drag, and add deterministic
sensor perturbations. Models stay frozen throughout final evaluation.

## Results

Recorded with the existing release compiler on this Windows workspace. Each
episode has 120 steps; all tracking errors, including transients, count.
Every controller sees the same scenario and starts from rest.

| Scenario | Frozen learned MSE | Adapted MSE | Fixed feedback MSE | Saturated steps: frozen / adapted / feedback |
| --- | ---: | ---: | ---: | --- |
| Changed plant, unseen target | 1.246890 | 1.206200 | 1.274440 | 13 / 16 / 7 |
| Unseen dynamics, noise 0.01, drag 0.01 | 0.237468 | 0.197876 | 0.318808 | 0 / 12 / 0 |
| Unseen dynamics, noise 0.02, drag 0.02 | 1.360650 | 1.281370 | 1.353080 | 13 / 17 / 9 |
| Mean | 0.948337 | 0.895149 | 0.982108 | |

Mean tracking error improves about 5.6% over the frozen model and 8.9% over the
chosen feedback controller. Adaptation uses saturation more often. The feedback
baseline is a fixed PD policy with gains 0.4 and 0.8, not an optimized baseline.
These are three deterministic regression scenarios, not statistical evidence of
general superiority. Noise tests perturb velocity readings; position is exact.

Fault injection at a target transition replaces the active model. After eight
observations, a watchdog compares the current and previous models on the same
data and restores the previous parameters if prediction MSE worsened by more
than 0.002. Position and velocity continue without reset. Whole-episode MSE was
0.545610 without recovery and 0.485001 with recovery. This test has excitation at
the fault; an unexcited robot may not reveal model degradation immediately.

Five noisy-training trials, evaluated on a separate plant and target, produced
mean MSE 0.205318 versus frozen-model MSE 0.252019 (18.5% lower). Individual
adapted scores ranged from 0.205245 to 0.205469. Noise is deterministic and the
evaluation episode is shared across trials; this tests sensitivity to training
trajectories, not a statistical sample of real sensor behavior.

## Checkpoints and timing

`checkpoint.iris` stores a versioned three-line text file and validates exact
field count, version, numeric parsing, and model bounds on load. Missing files,
invalid values, NaN/infinity, and failed writes are exercised. Decimal parameters
may round, so persistence tests check resulting prediction and control behavior.
Writes are not atomic or crash-safe. The caller retains the active model on a
load error; this is not yet a persistent running-service lifecycle.

`benchmark.iris` separates training from 20 measured batches of 200,000
control-plus-simulation steps and prints a checksum to keep the work observable.
One run reported total 83 ms, batch minimum 0 ms, maximum 17 ms, and mean 4.15 ms.
Training reported 0 ms. Zeros mean the clock could not resolve the interval.
The clock uses wall time with coarse platform resolution: these results do not
measure inference alone, per-tick jitter, deadline misses, or allocations.

## Uno kit milestone

`uno_policy.iris` prepares a scalar fixed-point controller for the documented
Uno R3 / BDD kit. Its scale is 1000; commands saturate at +/-1000. It rejects
invalid parameters and out-of-range inputs and returns status codes from six
self-checks. It performs no GPIO or actuator writes. It is a reference-policy
self-test; the learned artifact is generated separately below.

The learned-policy preparation command trains on noisy observations in IRIS,
saves and reloads the checkpoint, quantizes its coefficients, checks 243 states
against floating-point commands, and emits `learned_policy.iris`. A Python
orchestrator handles temporary files and compiler invocations; it implements no
learning or control logic. The host and exported policies share `uno_control.iris`.

```powershell
python tools/prepare_learning_robot.py --llvm 'C:/Program Files/LLVM/bin/LLVM-C.dll'
```

The default LLVM library on this machine lacked `LLVMInitializeAVRTargetInfo`.
Use an AVR-enabled LLVM library on other machines. The prepared output is in
`target/learning-robot-export/`: checkpoint, generated policy, shared kernel,
AVR bundle, and `verification.json` containing command logs and source hashes.
Preparation publishes these after the generated policy's desktop self-test and
AVR compilation pass. It does not link/upload firmware or operate a board.

The learned coefficients were 799/1000 and 250/1000. Maximum command error on
the 243-state grid was 0.002968 (rounded up), below the 0.01 test bound. This is
a finite grid check, not a proof over all states. The AVR bundle has zero fixed
array bytes; its manifest records the exact content fingerprint.
A bundle is not a hardware validation result. Follow
[the embedded guide](../../docs/embedded.md) for the existing board report
protocol and require the new bundle's matching report.

The first physical milestone is this policy self-test and sensor telemetry.
Then add hardware-in-the-loop replay before actuator control. The existing BSP
clock is a placeholder and its PWM fallback needs replacement for timed control.
Training stays on the host; the Uno profile
excludes heap-backed AD, records, collections, and cyclic control flow.

## Remaining work

- Streaming observation buffers and background retraining while control runs.
- Broader noise/dynamics experiments and tuned feedback baselines.
- Monotonic high-resolution timing, allocation measurement, and deadline checks.
- Crash-safe checkpoint promotion and restart recovery.
- Hardware-in-the-loop replay and actual Uno execution.
- Architecture/code evolution after the parameter-learning workflow is reliable.

The existing assertions cover this milestone. They do not establish hardware
safety, real-time behavior, or arbitrary model self-evolution.
