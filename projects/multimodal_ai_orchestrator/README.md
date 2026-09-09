# Advanced: multimodal AIS pipeline

Three concurrent producers supply deterministic simulated vision, audio, and sensor scores. Tensor fusion computes their mean, AIS selects the best utility, and runtime health records the successful outcome.

## Run

```text
iris run projects/multimodal_ai_orchestrator/src/main.iris
```

No model SDK, ROS installation, or hardware. Expected fused score: 0.6; selected action: 1. Inputs are simulations, not neural inference.

## Verify

```text
python tools/verify_learning.py --filter projects/multimodal_ai_orchestrator/
```

The catalog runner checks the native and interpreter paths separately. Every
assertion is part of the program, so incorrect results fail the run.

## Extend

Replace one producer with a validated external-model adapter while retaining explicit timeouts and failure reporting.
