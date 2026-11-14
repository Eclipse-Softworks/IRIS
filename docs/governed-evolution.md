# Governed Software Evolution

IRIS 1.0.0-rc1 supports a deliberately bounded self-evolution contract for
software-only policy functions:

```iris
def policy(input: i64) -> i64
```

This is the recommended promotion mechanism. A trusted host supplies the
baseline, canary expectations, resource policy, and pinned constitution.
Candidate code cannot rewrite those inputs.

IRIS also exposes a separate, explicitly unsafe unrestricted authority for
hosts whose purpose is autonomous whole-program replacement. It is not a mode
or flag on the governed coordinator and cannot weaken these seven gates.

## Seven mandatory gates

1. **Language safety** — the complete parser/type/borrow/effect/SSA pipeline
   succeeds, strict effect checking is enabled, and reachable promoted code is
   checked against the RC safe subset.
2. **Resources and ABI** — source bytes, function count, block count, and
   instruction count remain below configured limits; the function ABI is exactly
   `(i64) -> i64` and matches the active generation.
3. **Deterministic tests** — named zero-argument tests return `0` repeatedly.
   Candidate tests run in the bounded interpreter, so an assertion failure or
   step/depth exhaustion is a rejected candidate rather than a host abort.
4. **Transactional rollback** — `test_transaction_rollback()` must return `0`
   after demonstrating that speculative state was restored.
5. **Safety constitution** — SHA-256 pins trusted constitution bytes. A host
   callback evaluates every candidate input/output pair outside candidate code.
6. **Shadow canary** — baseline and candidate run on identical cases. Promotion
   requires a configured number of improved cases and lower aggregate absolute
   error.
7. **Verified activation** — LLVM ORC materializes the candidate, verifies its
   symbol/ABI, and atomically installs a new lease-safe generation.

Post-activation observations carry the generation they observed. A violation
uses generation-conditional rollback, so stale telemetry cannot roll back a
newer deployment.

## Audit model

Every candidate, gate, promotion, observation, and rollback is appended as
canonical JSONL. Each record includes the SHA-256 of the preceding record and is
flushed with `sync_data`. The API has no clear/rewrite operation.

The chain is tamper-evident, not tamper-proof. The CLI requires a separately
named `--audit-head` checkpoint, verifies it before promotion, and advances it
after every durable append. A non-empty log without its checkpoint fails closed.
Place the checkpoint in a different trust domain from the JSONL file if the
threat model includes an actor who can rewrite the audit directory.

## CLI

The complete runnable example is
[`projects/autonomous_evolution_lab`](../projects/autonomous_evolution_lab/README.md).
The core command is:

```text
iris evolve \
  --baseline baseline.iris \
  --candidate candidate.iris \
  --cases cases.json \
  --constitution constitution.txt \
  --constitution-sha256 <lowercase-sha256> \
  --audit evolution-audit.jsonl \
  --audit-head /protected/iris/evolution-audit.head.json \
  --min-output=-1000 \
  --max-output 1000
```

Canary cases are a JSON array:

```json
[
  { "input": 1, "expected": 3 },
  { "input": 4, "expected": 12 }
]
```

`--abi` selects the policy's host ABI:

| ABI | Signature | Canary case |
| --- | --- | --- |
| `i64` (default) | `def policy(x: i64) -> i64` | `{ "input": 1, "expected": 3 }` |
| `f64:N` | `def policy(a: f64, ..., n: f64) -> f64`, N = 1..8 | `{ "inputs": [0.5, 1.0], "expected": -0.9 }` |
| `record` | `def policy(s: In) -> Out`, both records of `f64`/`i64`/`bool` fields | `{ "input": {"position": 1.0, "step": 3}, "expected": {"force": -1.5, "brake": false} }` |

For `record`, `--min-output`/`--max-output` bound every numeric output field
and `--bound FIELD=MIN:MAX` (repeatable) bounds one field; a non-finite `f64`
field always fails the constitution. Record policies are validated in the
interpreter (`--validate sandbox` refuses them for now) and cannot claim
`--require-allocation-free`, because the returned record is heap-allocated. A
promoted record policy is served natively through
`HotSwapLease::call_record`.

### Whole programs (`iris evolve --manifest`)

A program with several gateways and its own state is promoted the same way,
judged by scenarios instead of canary cases:

```text
iris evolve \
  --manifest program.json \
  --scenarios scenarios.json \
  --baseline baseline.iris \
  --candidate candidate.iris \
  --constitution constitution.txt \
  --constitution-sha256 <lowercase-sha256> \
  --audit evolution-audit.jsonl \
  --audit-head /protected/iris/evolution-audit.head.json \
  --min-output=-1000 --max-output 1000
```

`program.json` is an `iris-evolution-program/2` manifest naming each gateway,
its function and its ABI (`json_v1` `(str) -> str`, `command_v1` `() -> i64`,
`scalar_i64_v1` `(i64) -> i64`). A scenario starts from the program's initial
state, optionally overridden by name, and calls gateways in order:

```json
[
  { "name": "resumed", "state": { "count": 3, "total": 30.0 },
    "steps": [
      { "gateway": "mean", "expected": 10 },
      { "gateway": "status", "input": {}, "expected": { "count": 3 } }
    ] }
]
```

A `json_v1` gateway receives `input` as JSON text and its reply is compared as
JSON: numbers by distance, anything else exactly. The program's required test
is `test_program` unless `--test NAME` (repeatable) says otherwise; the
rollback probe is `test_transaction_rollback`. Every number in every result
must lie within `--min-output`/`--max-output`.

Program state is declared with `var` at the top level
(`var count: i64 = 0`). At activation the coordinator reads the live
generation's state and writes every variable the candidate also declares, with
the same type, into the new generation before the atomic switch; new variables
start at their initial value and removed ones are dropped. Changing a
variable's type is refused at the resource-and-ABI gate (rename it to start
fresh). Handoff carries `i64`, `f64`, `bool`, `str`, lists of those, and
records of `f64`/`i64`/`bool` fields. `ProgramCoordinator::rollback` returns
to the replaced generation with the state it had when it was replaced;
`rollback_carrying_state` takes the live values back with it.

### Automated Evolutionary Search (`iris evolve-search`)

IRIS provides a built-in genetic programming engine to search for optimal code genomes autonomously:

```text
iris evolve-search \
  --baseline baseline.iris \
  --cases cases.json \
  --generations 30 \
  --pop-size 50 \
  --mutation-rate 0.3 \
  --crossover-rate 0.7 \
  --parsimony-weight 0.001 \
  --out-candidate candidate_evolved.iris \
  --promote \
  --constitution constitution.txt \
  --constitution-sha256 <lowercase-sha256> \
  --audit evolution-audit.jsonl \
  --audit-head /protected/iris/evolution-audit.head.json
```

With `--abi f64:N` the search evolves control laws over N `f64` inputs
(cases as for `iris evolve --abi f64:N`). It seeds the population with the
least-squares affine law, scores each tree under linear scaling (the search
finds the shape of a law; its gain and offset are fitted), polishes the
leading trees' constants each generation, and renders the winner as pure
scalar arithmetic, so it can be promoted with the allocation-free proof.
Protected division, `sqrt(abs(x))` and an `exp`-based `tanh` keep every
expression finite on finite inputs. The winner still passes the same seven
gates as any hand-written candidate: the search proposes, the gates decide.

Key options:
- `--abi i64|f64:N`: Integer genomes (default) or float control laws.
- `--generations <N>` & `--pop-size <N>`: Control the evolutionary search horizon.
- `--mutation-rate <float>` & `--crossover-rate <float>`: Control genetic operator frequencies.
- `--parsimony-weight <float>`: Applies multi-objective pressure against AST node bloat.
- `--out-candidate <path>`: Saves the discovered genome as compile-ready IRIS source code.
- `--promote`: Automatically evaluates the winning candidate through the seven coordinator gates and hot-swaps it.

### A controller that evolves while it runs (`iris evolve-live`)

`iris evolve-live` keeps a control loop running while its controller is
replaced. The plant and every controller are IRIS:

```text
iris evolve-live --plant plant.iris --baseline baseline.iris \
  --candidate a.iris --candidate b.iris [--search --seed 7] \
  --cases cases.json --constitution constitution.txt --constitution-sha256 <sha256> \
  --audit audit.jsonl --audit-head audit.head.json \
  --min-output=-10 --max-output 10 --setpoints 1,5,-2,3 --hold 8 --offer-every 4 \
  [--require-allocation-free] [--realtime]
```

Each control period the loop leases the active generation of
`policy(error: f64, velocity: f64) -> f64` and steps `plant(PlantInput) ->
PlantState` (f64 fields `position, velocity, force, dt` in,
`position, velocity` out), which is served natively through the record ABI.
Candidates are offered every `--offer-every` simulated seconds and must pass
the seven gates before they are swapped in between two steps. With
`--search`, a float search runs on a worker thread; its winner jumps the
queue (in simulated time the loop waits for it at the first offer point, so
the run is reproducible; with `--realtime` it is offered whenever it
finishes). A promoted generation whose force leaves the
`--min-output`/`--max-output` rating is rolled back in that step through the
coordinator's generation-conditional rollback, and the step is recomputed
with the restored generation; the baseline saturates instead. The run ends
with a per-setpoint table and a verified audit chain. The library form is
`iris::evolution::live::run_live`. Worked example:
[projects/evolving_cart_controller](../projects/evolving_cart_controller/README.md);
covered by `tests/evolve_live_cli.rs`.

### A service upgraded while it serves (`iris evolve-serve`)

`iris evolve-serve` hosts a whole program, as `iris evolve --manifest`
describes it, over HTTP on 127.0.0.1 and promotes new versions of it in
place:

```text
iris evolve-serve --manifest program.json --baseline v1.iris \
  --scenarios scenarios.json --constitution constitution.txt \
  --constitution-sha256 <sha256> --audit audit.jsonl --audit-head audit.head.json \
  [--min-output=0 --max-output 1000000] [--test NAME] [--port 8787 | --port 0 --port-file F]
```

`POST /call/<gateway>` calls a gateway natively; the reply carries the serving
generation in `X-Iris-Generation`. `POST /upgrade` takes a candidate's source
and runs it through the seven gates against the scenarios while calls
continue; on promotion the live state is handed over as above. Calls go
through `ProgramCoordinator::call`, which is serialised with the handoff, so
each call lands entirely in one generation and the next generation starts
from the state that call left. `POST /rollback` restores the generation the
last promotion replaced with the live state carried back
(`ProgramCoordinator::rollback_carrying_state`); `GET /status` shows the
generation, request counts, state and decisions; `POST /shutdown` ends the run
with a summary and a verified audit chain. The library form is
`iris::evolution::serve::serve`. Worked example:
[projects/self_upgrading_service](../projects/self_upgrading_service/README.md);
covered by `tests/evolve_serve.rs`; limits in
[known issue #76](known-issues.md).

### Autonomic service daemon (`iris service-daemon`)

`iris service-daemon` runs a MAPE-K (Monitor-Analyze-Plan-Execute-Knowledge)
loop around a real HTTP service. It is a reverse proxy: clients reach the
service through `--listen`, and every request is measured on its way.

```text
iris service-daemon \
  --upstream http://127.0.0.1:8080 \
  --listen 8088 \
  --target-p99 15.0 \
  --error-budget 0.01 \
  --audit daemon-audit.json \
  --serve --port 9090
```

- **Monitor**: each interval (`--tick-ms`, default 1000) it measures p99
  latency, the error ratio, the request rate, requests in flight and cache
  hits from the traffic that passed through.
- **Analyze**: it classifies the interval -- nominal traffic, a flash crowd
  (over three times the healthy rate, with latency or load up), an upstream
  brownout (errors over `--error-budget`, or p99 over `--target-p99`), cache
  thrashing -- and computes the strain against those targets.
- **Plan**: it records the strain the active policy produced in that regime.
  When strain is above 0.15 it tries the policies it has not yet measured
  there, the regime's usual remedy first, and after that picks the one with
  the least measured strain: forward everything; shed (answer 429 once
  `--max-inflight` requests are in flight); serve the last good response to
  a `GET` from cache; throttle to the rate the service last handled while
  healthy.
- **Execute**: the proxy applies the policy from the next request on, and
  returns to forwarding after three good intervals. Each policy change is a
  new generation; every interval -- its measurements, regime, policy and
  generation -- goes into the audit ledger (`--audit`).

`--ticks N` stops after N intervals; 0 (the default) runs until stopped.
With `--serve`, `http://localhost:9090/` shows the live dashboard,
`/metrics` exports Prometheus gauges and counters (`iris_requests_total`,
`iris_p99_latency_ms`, `iris_allostatic_strain`, `iris_service_generation`,
`iris_cache_hit_ratio`, ...) and `/status` the same as JSON.

`tests/service_daemon_proxy.rs` fails a local service mid-run: the daemon
serves from cache within an interval and forwards again after the service
recovers. **Limits:** plain HTTP/1.1 (no TLS), one request per connection,
and a cached response is the last good one for that request target, whatever its age.

## Current boundary

- Candidate validation is bounded by interpreter steps and call depth.
- Reachable native policy/test code may not use FFI, files, network, process,
  clock, concurrency, effect-opaque closure calls, or dynamic dispatch.
- The activation gate materializes and installs native code but does not execute
  untrusted native code inside the coordinator process during validation.
- AIS/ML/tensor/NN imports compile under strict effects. Extern declarations
  require explicit contracts; bundled stdlib bodies use deterministic inferred
  transitive summaries, while user and package functions require clauses.
- The v2 program manifest supports reviewed `(str)->str` JSON, `()->i64`
  command, and legacy `(i64)->i64` gateways. All gateways in a module switch
  atomically. Governed program promotion (`iris evolve --manifest`) judges
  scenarios in the interpreter and hands program state (`var` at the top
  level) to the new generation automatically. Calls still in flight on the
  replaced generation while it is promoted can change its state after the
  snapshot; quiesce writers first when every update must carry over.
- Eight generations are retained by default. Candidate source and manifests can
  be written to the immutable content-addressed artifact store.
- Native worker framing and resource policies are connected to a validated
  Windows LPAC + Job Object launcher. Workers have zero capabilities, opt out
  of broad application-package access, start suspended, and are resumed only
  after memory/CPU/process limits and bounded output pipes are active.
  Unsupported platforms fail closed. The scalar coordinator continues to use
  bounded interpreter execution during its validation gates.

These constraints are what make the current claim testable: IRIS can build and
promote bounded self-evolving software policies with measurable evidence. It is
also capable of general compiler-valid whole-program activation through the
unsafe authority below, but that mechanism makes no safety claim about candidate
behavior.

## Unrestricted whole-program authority

`UnrestrictedEvolutionAuthority` accepts any compiler-valid IRIS module whose
declared gateways match an `iris-evolution-program/2` manifest. It deliberately
does not run constitutions, canaries, effect filters, resource checks,
transaction probes, audit logging, or sandboxing. Candidate code inherits all
filesystem, network, FFI, process, hardware, and other privileges held by its
host process.

Issuing the authority requires the exact acknowledgement
`IRIS_UNRESTRICTED_SELF_MODIFICATION`. The token is non-cloneable and the unsafe
authority owns an isolated hot-swap table, so it cannot silently weaken an
existing governed coordinator. Compiler validity, manifest structure, ABI
fingerprints, ORC materialization, atomic gateway switching, generation leases,
bounded history, and exact-generation rollback remain enforced because they are
required for coherent execution, not policy controls.

Use `iris evolve-unrestricted --help` for the one-process CLI demonstration.
Long-running autonomous software embeds the Rust library API so the authority,
leases, observations, and rollback decisions remain alive with the host.
