# Interactive learning scheduler

A software-only IRIS application for exploring learned job scheduling, inspecting
predictions, comparing policies, and testing model recovery. All application,
learning, scheduling, checkpoint, and CPU-worker logic is IRIS. Python only
orchestrates verification and independently samples process resources.

## Launch

From the repository root, using a built release compiler:

```powershell
New-Item -ItemType Directory -Force target/learning-scheduler | Out-Null
.\target\release\iris.exe --emit binary projects/learning_scheduler/main.iris -o target/learning-scheduler/iris-scheduler.exe
.\target\learning-scheduler\iris-scheduler.exe
```

The application reads commands from the terminal. On other platforms, use your
compiler path and omit `.exe`. Checkpoints are relative to the working directory.
`save` overwrites `iris-scheduler-model.txt`; `load` is explicit and retains the
current model if the file is absent or invalid. Blank input and EOF exit cleanly.

Try this sequence, one command at a time:

```text
compare
train
inspect 7
compare
shift
auto
compare
fault
compare
recover
jobs 32
execute
save
quit
```

Restart the app and enter `load`, then `status`, to restore saved weights.

| Command | Behavior |
| --- | --- |
| `status` | Show generation, workload settings, weights, and prediction MSE |
| `normal`, `shift`, `mixed` | Change which job class is expensive; keep the current model |
| `jobs N` | Set evaluation queue size, 8–128 |
| `seed N` | Set reproducible workload seed, 1–999000 |
| `train` | Fit a candidate and evaluate it on separate selection jobs |
| `auto` | Request training when recent prediction MSE exceeds 25 |
| `compare` | Replay FIFO, smallest-size, and learned policies on held-out jobs |
| `inspect N` | Show one evaluation job's features, predicted cost, and observed work |
| `execute` | Execute planned jobs in structured waves of at most two CPU tasks |
| `fault`, `recover` | Inject invalid weights, then run the recovery watchdog |
| `save`, `load` | Save or restore validated model weights |
| `help`, `quit` | Show commands or exit |

## Learning and scheduling

Jobs have a size, class, arrival, and observed work cost. Two learned class
coefficients and a shared bias predict cost from size and class. Scheduler scores
do not read the observed cost. Scalar autodiff trains on 48 labeled jobs for 50
epochs in batches of four. Training labels currently come from the deterministic
workload generator, not measured CPU duration.

Training, selection, and final replay use seeds `seed+11`, `seed+23`, and `seed+47`.
Promotion requires valid weights, at least 10% lower prediction MSE on selection
jobs, no worse mean turnaround, and no more than 10% worse p99 turnaround.
`auto` checks one recent batch when invoked; this is not a background service.
The mixed regime is intentionally harder for the small model and may cause a
candidate to be rejected.

Replay is a work-conserving, nonpreemptive **single-worker simulation**. Waiting
120 work units gives a job oldest-arrival priority over the policy score. This
is an aging threshold, not a promise that maximum wait is 120. All finite queues
complete, and every job contributes to reported metrics. FIFO is the fallback
when weights are invalid. Equal scores use original input order.

With the default UI seed and normal workload, one verified run reported:

| Policy | Mean turnaround | p95 | p99 | Maximum wait |
| --- | ---: | ---: | ---: | ---: |
| FIFO | 785.542 | 1561 | 1601 | 1559 |
| Size heuristic | 720.417 | 1561 | 1601 | 1559 |
| Learned after training | 695.333 | 1561 | 1601 | 1559 |

The learned policy improves mean turnaround by about 11.5% versus FIFO on this
queue. Tail latency and throughput do not improve. All policies have makespan
1742 because they perform the same work without idle time. These are reproducible
simulation results, not claims about production throughput.

## Actual CPU execution and resources

`execute` dispatches the replay order into two-task waves using task groups and
a channel with capacity two. Tasks run bounded integer computations, and the
controller joins each wave and consumes both results before launching more.
It reports completed jobs, checksum, wall milliseconds, and runtime worker count.
Simulated arrival spacing is not reproduced in this CPU run. It is an execution
check of the dispatch plan, not an actual online arrival scheduler.

```powershell
python tools/verify_learning.py --iris target/release/iris.exe --filter projects/learning_scheduler/
python tools/verify_learning_scheduler.py
```

The first command verifies the scripted native app and native/interpreter model
tests. The second builds a native executable, drives real stdin sessions, tests
restart recovery and malformed input, and writes
`target/learning-scheduler-verification.json` with complete transcripts and
resource samples. Tests run in scratch directories and preserve user checkpoints.

On this Windows workspace, the 128-job test with two executions took about
0.106 seconds wall time and 0.094 seconds process CPU time; sampled OS peak working
set was 6,447,104 bytes. These measurements include runtime, UI, training, and
execution and exclude compilation. They are environment-dependent. The monitor
uses Windows process counters or Linux `/proc`; unsupported or missed samples
are recorded as null. It does not measure allocation counts or per-job memory.
IRIS's own clock is coarse wall time; zero-ms intervals are below its resolution.

## Debugging and recovery

Use `inspect` to examine an incorrect prediction and `status` to see the active
weights. Switch to `shift`, then `auto`, to exercise model mismatch and promotion.
Use `fault`, `compare`, `recover`, and `status` to observe FIFO fallback and model
restoration. Candidate rejections print their reason. Malformed commands do not
terminate the session.

The checkpoint parser validates version, exact field count, finite bounded
weights, and numeric syntax. Restart/load and corrupted-file retention are tested
in separate native processes. The checkpoint contains weights only: queue,
generation counter, and previous-model history reset at process startup. Writes
are not atomic or crash-safe, and job resumption is not implemented.

## IRIS idioms and current limits

- Records model jobs, weights, and reports; choice types represent commands and policies.
- Pattern matching dispatches commands and handles `result`/`option`; checkpoint I/O uses `?` and declared effects.
- Scalar automatic differentiation trains the cost model.
- Typed collections represent the queue and dispatch order.
- Structured task groups and typed bounded channels execute actual CPU work.
- Assertions cover model quality, unique completion, idle periods, aging, fallback, and persistence.

The compiler now removes unused lifted spawn parameters and their capture
arguments. The worker creates tasks directly; it no longer needs a helper to
hide surrounding records from capture. Regression tests cover the previous
native heap-corruption case, shadowed variables, and nested task captures.
Rebuild the compiler from this checkout before building this version of the app;
older compiler binaries still contain the capture bug. This fix does not establish
general record-capture ownership guarantees. Large autodiff graphs and long-running
runtime resource growth remain outside this example's claims.

Next steps are importing real job observations, calibration of measured CPU
costs, live arrivals, background model training, monotonic timing, and crash-safe
state recovery. No Arduino or external service is required.
