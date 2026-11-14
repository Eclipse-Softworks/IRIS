# Self-upgrading inventory service

A stateful HTTP service that is replaced by newer versions of itself while
it runs, without losing its stock or its counters. The service is IRIS;
`iris evolve-serve` hosts it:

1. it compiles `v1.iris` and serves its gateways (`restock`, `reserve`,
   `stats`) natively, at `POST /call/<gateway>`;
2. a new version arrives as IRIS source at `POST /upgrade`. While traffic
   continues, it goes through the seven gates: language safety, resources and
   ABI (including the state handoff plan), its own tests, the rollback probe,
   the constitution, a shadow canary against the running version over
   `scenarios.json`, and verified activation;
3. on promotion, the live values of every top-level `var` the new version
   also declares are written into it before the switch. Calls wait only for
   that read and write, so each call lands entirely in one version and no
   update is lost;
4. `POST /rollback` restores the replaced version, carrying the live state
   back, and every decision goes into a hash-chained audit log.

| File | Role |
| --- | --- |
| [v1.iris](v1.iris) | Version 1: accepts a reservation larger than the shelf, driving stock negative |
| [v2.iris](v2.iris) | Version 2: refuses it, reports the shortfall, counts refusals in a new `refused` variable |
| [candidates/debug_logging.iris](candidates/debug_logging.iris) | Version 2 with a `println` left in: refused at `language_safety` |
| [candidates/fractional_stock.iris](candidates/fractional_stock.iris) | Version 2 with `stock: list<f64>`: refused at `resource_and_abi`, because handoff never reinterprets data |
| [program.json](program.json) | The manifest: three `json_v1` gateways |
| [scenarios.json](scenarios.json) | What an upgrade must get right, including one scenario resumed from live-like state |
| [constitution.txt](constitution.txt) | Every number in every reply lies in [0, 1000000] (`--min-output`/`--max-output`) |
| [client.iris](client.iris) | Drives a running server through the story below, asserting every reply |

## Run

```powershell
$p = "projects/self_upgrading_service"
$h = (Get-FileHash $p/constitution.txt -Algorithm SHA256).Hash.ToLower()
iris evolve-serve --manifest $p/program.json --baseline $p/v1.iris `
  --scenarios $p/scenarios.json --constitution $p/constitution.txt `
  --constitution-sha256 $h --audit target/inventory/audit.jsonl `
  --audit-head target/inventory/audit.head.json `
  --min-output=0 --max-output 1000000 --port 8787
```

Then, from the repository root in another terminal, run `iris run
projects/self_upgrading_service/client.iris` (set `IRIS_SERVICE_PORT` for
another port). By hand:

```powershell
curl.exe -s -X POST localhost:8787/call/restock -d '{\"sku\":\"bolt\",\"qty\":10}'
curl.exe -s -X POST localhost:8787/upgrade --data-binary "@$p/v2.iris"
curl.exe -s localhost:8787/status
curl.exe -s -X POST localhost:8787/rollback
curl.exe -s -X POST localhost:8787/shutdown
```

| Request | Reply |
| --- | --- |
| `POST /call/<gateway>` | The gateway's result; `X-Iris-Generation` names the version that served it |
| `GET /status` | Generation, request counts per generation, live state, decisions |
| `POST /upgrade` | 200 with the gates and the handoff (`carried`, `initialised`, `dropped`); 409 naming the refusing gate; 422 if it does not compile |
| `POST /rollback` | 200 with the restored generation and what was carried back |
| `POST /shutdown` | Stops after the requests in flight; the host prints a summary and verifies the audit chain |

## What was observed (2026-10-19, Windows)

The client, as a native binary and under the interpreter, against a fresh
server each time; the server printed the same decisions both times:

```text
[  0.000s      0 requests] installed    baseline, serving on http://127.0.0.1:55956
[  1.311s      7 requests] rejected     upgrade_1: evolution gate 'language_safety' failed: function 'reserve' contains console output
[  1.483s      7 requests] rejected     upgrade_2: evolution gate 'resource_and_abi' failed: state 'stock' changes type from list<i64> to list<f64>; give the new variable a new name so it starts fresh
[  2.021s      8 requests] promoted     upgrade_3: generation 2 after 7/7 gates; carries skus, stock, served, reserved, initialises refused
[  2.048s     14 requests] rolled back  generation 2 replaced by generation 1; carries skus, stock, served, reserved
[  2.497s     18 requests] promoted     upgrade_4: generation 3 after 7/7 gates; carries skus, stock, served, reserved, initialises refused

22 requests (generation 1: 12, generation 2: 6, generation 3: 4)
final generation 3; state {"refused":1,"reserved":17,"served":9,"skus":["bolt","nut"],"stock":[6,3]}
audit chain verified: head 24:acb7b5c6...
```

Version 1 oversold a bolt to -2 before the upgrade. Version 2 took over with
the 5 requests already counted and refused a 25-bolt reservation (short
by 15). The rollback brought version 1 back with 7 served and 10 units on the
shelves, and the second promotion continued from 8. In the scenarios, version 2
was exact on 9/9 steps and version 1 on 3/9.

`tests/evolve_serve.rs` runs that client on both backends and also checks the
handoff under load: eight clients reserve one unit at a time while version 2 is
promoted, and afterwards `served`, `reserved` and the stock account for every
reservation on either side of the switch.

Limits ([known issue #76](../../docs/known-issues.md)): a gateway that panics
stops the host; calls are serialised; only an operator rolls back; the
gateways are fixed when the host starts; and an upgrade must improve on at
least one scenario step.
