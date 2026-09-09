# Intermediate: structured job pipeline

Eight workers calculate squares, send results through a channel, and update an atomic completion counter. The owning scope joins the task group before consuming the known number of results.

## Run

```text
iris run projects/job_pipeline/main.iris
```

No service or credentials. Expected checksum: 204; completed jobs: 8. Result arrival order is intentionally unspecified.

## Verify

```text
python tools/verify_learning.py --filter projects/job_pipeline/
```

The catalog runner checks the native and interpreter paths separately. Every
assertion is part of the program, so incorrect results fail the run.

## Extend

Add jobs with result types and ensure each scheduled job reports either success or failure before joining.
