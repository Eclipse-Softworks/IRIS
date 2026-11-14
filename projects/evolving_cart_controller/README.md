# Evolving cart controller

A cart that keeps moving while its controller is replaced. The plant, every
controller and the reference derivation are IRIS; `iris evolve-live` runs
the loop:

1. every 20 ms it leases the active generation of
   `policy(error, velocity) -> f64`, asks it for a force, and steps the plant
   (`plant.iris`, served natively through the record ABI);
2. between steps it offers candidate controllers to the evolution
   coordinator: hand-written ones on a schedule, and, with `--search`, the
   winner of a float search running beside the loop. A candidate is
   hot-swapped in only after all seven gates (tests, rollback probe, canary
   against the optimal law, constitution, allocation-free proof, ORC
   activation);
3. a promoted controller that commands a force outside the motor's ±10 N
   rating is rolled back in that same step, and the step is recomputed with
   the restored generation;
4. every decision is appended to a hash-chained audit log.

| File | Role |
| --- | --- |
| [plant.iris](plant.iris) | 1.2 kg cart with viscous drag |
| [reference.iris](reference.iris) | Derives the LQR-optimal gains from the plant (discrete Riccati), checks the saturated loop settles, writes `cases.json` |
| [baseline.iris](baseline.iris) | Generation 1: `0.5 * error` |
| [candidates/pd_with_history.iris](candidates/pd_with_history.iris) | Allocates: refused by the allocation-free proof |
| [candidates/lqr_unclamped.iris](candidates/lqr_unclamped.iris) | Optimal near the operating point, unsaturated: promoted, then rolled back |
| [candidates/lqr_saturated.iris](candidates/lqr_saturated.iris) | Optimal and saturated: stays |
| [constitution.txt](constitution.txt) | The trusted rule, pinned by SHA-256 |

## Run

```powershell
$p = "projects/evolving_cart_controller"
$h = (Get-FileHash $p/constitution.txt -Algorithm SHA256).Hash.ToLower()
iris evolve-live --plant $p/plant.iris --baseline $p/baseline.iris `
  --candidate $p/candidates/pd_with_history.iris `
  --candidate $p/candidates/lqr_unclamped.iris `
  --candidate $p/candidates/lqr_saturated.iris `
  --cases $p/cases.json --constitution $p/constitution.txt --constitution-sha256 $h `
  --audit target/cart/audit.jsonl --audit-head target/cart/audit.head.json `
  --min-output=-10 --max-output 10 --setpoints 1,5,-2,3 --hold 8 --offer-every 4 `
  --require-allocation-free
```

Add `--search --seed 7` to let a float search propose its own law, and
`--realtime` to pace the loop to the wall clock (32 s). Regenerate the cases
with `IRIS_WRITE_CASES=projects/evolving_cart_controller/cases.json iris run
projects/evolving_cart_controller/reference.iris`.

## What was observed (2026-10-18, Windows)

```text
[t=   0.00s] installed       generation 1: baseline controller
[t=   4.00s] rejected        pd_with_history: evolution gate 'resource_and_abi' failed: allocation-free proof failed ...
[t=   8.00s] promoted        lqr_unclamped: generation 2 after 7/7 gates (canary error 52066538418 -> 0)
[t=   8.00s] rolled back     generation 2: commanded 17.979 at error 4.008, velocity -0.169: outside [-10, 10]; generation 1 restored
[t=  12.00s] promoted        lqr_saturated: generation 3 after 7/7 gates (canary error 52066538418 -> 0)

setpoint  final error  settled        peak force  generation
   1.000      +0.0078  no                  0.500  1
   5.000      +0.0038  after 6.62 s        8.519  3
  -2.000      -0.0000  after 3.18 s       10.000  3
   3.000      +0.0000  after 2.98 s       10.000  3

1600 steps; controller call mean 234 ns, worst 2300 ns
audit chain verified: head 23:e94e2570...
```

With `--search --seed 7 --realtime`, the search recovered the LQR law from
the 25 canary cases in 40 ms, `4.34017662287 * x0 - 3.46534329407 * x1`
(reference.iris: 4.340176623413, 3.465343295175), and it passed the seven
gates as generation 2. `tests/evolve_live_cli.rs` checks both runs.

Limits: one plant contract (position, velocity, force, dt); the constitution
is an output range; the canary measures closeness to a reference law, so a
candidate can only be "better" relative to that reference.
