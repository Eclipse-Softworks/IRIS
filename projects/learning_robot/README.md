# Learning robot: models to control in IRIS

**Start with the [trajectory learning workflow](WORKFLOW.md)** for collected
observations, automatic drift detection, controller comparisons, checkpointing,
timing, and Uno preparation. The following describes the original introductory
experiment, retained as a small executable baseline.

A first executable milestone toward building, training, and running learning
systems in one language. Simulation, automatic differentiation, gradient descent,
evaluation, and bounded control are all application code in `main.iris`.

## Run and verify

From the repository root, with a built compiler:

```powershell
.\target\release\iris.exe run projects/learning_robot/main.iris
python tools/verify_learning.py --iris target/release/iris.exe --filter projects/learning_robot/
```

The catalog runner verifies native and interpreter execution separately.

## Experiment

A one-axis actuator follows `v_next = a * v + b * command`, then
`position_next = position + v_next`. Learned parameters represent velocity
retention and actuator response. A controller uses the model to approach a
target, keeping every command within [-1, 1]. `std.ais` supplies the clamp;
the application implements evaluation and update decisions.

1. Train two parameters on four calibration probes with scalar autodiff and
   batch gradient descent. Observations come from the simulated plant.
2. Change the plant from `(a=0.5, b=1.0)` to `(a=0.8, b=0.25)`.
3. Detect prediction error on ten different input pairs and retrain.
4. Require valid parameter bounds, lower prediction error, and lower closed-loop
   tracking error before installing the candidate.
5. Reject invalid, worse, and unchanged candidates. Inject a bad installed model
   and verify that restoring the saved model restores tracking performance.

Tracking comparisons start from rest, run 60 discrete steps, and use the same
target sequence: position 2 for 30 steps, then -1 for 30 steps. The baseline
keeps the model learned on the original plant. Mean squared tracking error
includes every step and all transients.

The verified native run produced approximately 0.413814 baseline tracking MSE
and 0.371738 after retraining (10.2% lower). Fault injection raised MSE to
0.453514; restoration returned it to 0.371738. Assertions check improvement,
prediction accuracy, command bounds, rejection, and restoration.

## Boundaries

This is a scripted parameter-learning experiment in a deterministic, noise-free linear simulation.
Calibration can query independent plant states. Training and evaluation run in
separate phases; rollback replays an episode from rest. The tracking scenario
is used for candidate selection, so improvement does not demonstrate
generalization to unseen tasks. There is no physical robot, concurrent online
training, architecture/code evolution, persistent checkpoint, or real-time
deadline guarantee yet.

Next steps: collected trajectories, separate selection and unseen evaluation
scenarios, noisy sensors, nonlinear dynamics, model persistence, and measured
control-loop timing. See the [project direction](../../docs/direction.md).
