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

Key options:
- `--generations <N>` & `--pop-size <N>`: Control the evolutionary search horizon.
- `--mutation-rate <float>` & `--crossover-rate <float>`: Control genetic operator frequencies.
- `--parsimony-weight <float>`: Applies multi-objective pressure against AST node bloat.
- `--out-candidate <path>`: Saves the discovered genome as compile-ready IRIS source code.
- `--promote`: Automatically evaluates the winning candidate through the seven coordinator gates and hot-swaps it.

### Autonomic Microservice Daemon & Live Web Dashboard (`iris service-daemon`)

IRIS includes a closed-loop MAPE-K (Monitor-Analyze-Plan-Execute-Knowledge) autonomic daemon runtime with an embedded zero-dependency HTTP server, Prometheus metrics exporter, and real-time SVG web visualizer:

```text
iris service-daemon \
  --ticks 120 \
  --target-p99 15.0 \
  --error-budget 0.01 \
  --serve \
  --port 9090
```

- **Live Web Dashboard**: Navigate to `http://localhost:9090/` to visualize real-time P99 latency sparklines, allostatic strain gauges, and hot-swap policy generations.
- **Prometheus Metrics**: `GET http://localhost:9090/metrics` exports gauges and counters (`iris_requests_total`, `iris_p99_latency_ms`, `iris_allostatic_strain`, `iris_service_generation`, etc.).
- **JSON Telemetry API**: `GET http://localhost:9090/status` returns structured real-time operational status.

## Current boundary

- Candidate validation is bounded by interpreter steps and call depth.
- Reachable native policy/test code may not use FFI, files, network, process,
  clock, concurrency, effect-opaque closure calls, or dynamic dispatch.
- The activation gate materializes and installs native code but does not execute
  untrusted native code inside the coordinator process during validation.
- AIS/ML/tensor/NN imports compile under strict effects. Extern declarations
  require explicit contracts; bundled stdlib bodies use deterministic inferred
  transitive summaries, while user and package functions require clauses.
- The library-level v2 program manifest supports reviewed `(str)->str` JSON,
  `()->i64` command, and legacy `(i64)->i64` gateways, plus explicit JSON
  snapshot/import state migration. All gateways in a module switch atomically.
  The public CLI's seven-gate canary scoring remains scalar for RC1.
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
