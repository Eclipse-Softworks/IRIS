# Advanced: verified software evolution

The default IRIS program checks candidate selection. baseline.iris and candidate.iris expose policy(i64) -> i64; the target cases prefer multiplication by three. The candidate includes assertion-backed behavior and transaction rollback probes.

## Run

```text
iris run projects/autonomous_evolution_lab/main.iris
```

Default run needs no service. Actual promotion requires a loadable LLVM ORC runtime and writes an audit log plus its head checkpoint.

## Verify

```text
python tools/verify_learning.py --filter projects/autonomous_evolution_lab/
```

The catalog runner checks the native and interpreter paths separately. Every
assertion is part of the program, so incorrect results fail the run.

## Extend

Change the candidate within the declared ABI, add adversarial cases, and inspect each gate and audit record.

## Perform actual audited promotion

```powershell
.\projects\autonomous_evolution_lab\run.ps1 -Iris iris
```

The script hashes constitution.txt, runs the seven coordinator gates, and
retains audit.jsonl and its head checkpoint in a fresh temporary directory.
It prints the directory for inspection. The integration test is:

```text
cargo test --test evolution_cli
```

This CLI workflow promotes the scalar policy ABI. Whole-program gateways and
unrestricted authority are separate Rust host APIs; see
[governed evolution](../../docs/governed-evolution.md).
