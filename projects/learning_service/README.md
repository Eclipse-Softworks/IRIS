# Advanced: software-only learning service

A deterministic one-input sigmoid network learns a binary mapping using std.nn backpropagation and SGD. It checks predictions, registers the model, records health, and verifies degraded agent state after failures.

## Run

```text
iris run projects/learning_service/main.iris
```

No hardware, datasets, or external model runtime. Initial prediction is 0.5; trained low/high predictions must be below 0.2 and above 0.8.

## Verify

```text
python tools/verify_learning.py --filter projects/learning_service/
```

The catalog runner checks the native and interpreter paths separately. Every
assertion is part of the program, so incorrect results fail the run.

## Extend

Add a held-out dataset and serialization for weights; evaluate before promoting a new model version.
