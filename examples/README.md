# IRIS RC1 examples

Read the folders in order, then build the [complete projects](../projects/README.md).
Programs use current `def`/`val`/`var`/`record`/`choice` syntax, explicit function
returns, and assertions. Expression tails remain useful inside closures and
control-flow expressions.

| Folder | Level | Coverage |
| --- | --- | --- |
| [01_basics](01_basics/) | Beginner | Bindings, control flow, collections, and strings |
| [02_functions](02_functions/) | Beginner | Functions, named arguments, recursion, closures, and modules |
| [03_types](03_types/) | Intermediate | Records, patterns, generics, const parameters, traits, and macros |
| [04_safety](04_safety/) | Intermediate | Ownership, weak references, effect contracts, and handlers |
| [05_concurrency](05_concurrency/) | Intermediate | Channels, async, parallel ranges, task groups, and runtime soak |
| [06_data](06_data/) | Intermediate | JSON, CSV, files, and parameterized SQLite |
| [07_ml](07_ml/) | Advanced | Scalar/tensor differentiation, neural layers, and model graphs |
| [08_ais](08_ais/) | Advanced | Viability, active inference, and production lifecycle |
| [09_evolution](09_evolution/) | Advanced | Transactions, speculation, and typed compiler-host edits |
| [10_integration](10_integration/) | Advanced | LLM protocols, checked networking, FFI, and ROS geometry |
| [embedded](embedded/) | Hardware | Restricted allocation-free components and Uno GPIO |

## Run and verify

```text
iris run examples/01_basics/hello.iris
iris test projects/ledger/main.iris
python tools/verify_learning.py --iris target/release/iris.exe
```

On Unix use `target/release/iris`. The verifier runs in temporary working
directories, enforces timeouts, and checks expected output and exit status.
`dual` means both native `iris run` and forced interpreter execution are tested;
`host` requires `IRIS_FORCE_INTERP=1`. It never silently accepts native failure
through interpreter fallback. Native programs require LLVM and target libraries.

## Runnable catalog

The machine-readable [catalog.json](catalog.json) also includes project modules.

| Example | Execution | Expected result |
| --- | --- | --- |
| [01_basics/hello.iris](01_basics/hello.iris) | dual | Hello, IRIS! answer = 42 |
| [01_basics/bindings.iris](01_basics/bindings.iris) | dual | bindings: ok |
| [01_basics/control_flow.iris](01_basics/control_flow.iris) | dual | control flow: ok |
| [01_basics/collections.iris](01_basics/collections.iris) | dual | collections: ok |
| [01_basics/strings.iris](01_basics/strings.iris) | dual | strings: ok |
| [02_functions/functions.iris](02_functions/functions.iris) | dual | functions: ok |
| [02_functions/closures.iris](02_functions/closures.iris) | dual | closures: ok |
| [02_functions/modules.iris](02_functions/modules.iris) | dual | modules: ok |
| [03_types/records_patterns.iris](03_types/records_patterns.iris) | dual | records and patterns: ok |
| [03_types/generics.iris](03_types/generics.iris) | dual | generics: ok |
| [03_types/traits.iris](03_types/traits.iris) | dual | traits: ok |
| [03_types/errors.iris](03_types/errors.iris) | dual | errors: ok |
| [04_safety/ownership.iris](04_safety/ownership.iris) | dual | ownership: ok |
| [04_safety/effects.iris](04_safety/effects.iris) | dual | effects: ok |
| [04_safety/handlers.iris](04_safety/handlers.iris) | dual | handlers: ok |
| [05_concurrency/channels.iris](05_concurrency/channels.iris) | dual | channels: ok |
| [05_concurrency/async.iris](05_concurrency/async.iris) | dual | async: ok |
| [05_concurrency/task_groups.iris](05_concurrency/task_groups.iris) | dual | task groups: ok |
| [06_data/json_csv.iris](06_data/json_csv.iris) | dual | json and csv: ok |
| [06_data/stdlib_pipeline.iris](06_data/stdlib_pipeline.iris) | dual | stdlib pipeline: ok |
| [07_ml/scalar_autodiff.iris](07_ml/scalar_autodiff.iris) | dual | scalar autodiff: ok |
| [07_ml/tensor_autodiff.iris](07_ml/tensor_autodiff.iris) | dual | tensor autodiff: ok |
| [07_ml/neural_layer.iris](07_ml/neural_layer.iris) | dual | neural layer: ok |
| [08_ais/viability.iris](08_ais/viability.iris) | dual | viability: ok |
| [08_ais/lifecycle.iris](08_ais/lifecycle.iris) | dual | lifecycle: ok |
| [09_evolution/transactions.iris](09_evolution/transactions.iris) | dual | transactions: ok |
| [09_evolution/speculation.iris](09_evolution/speculation.iris) | dual | speculation: ok |
| [09_evolution/typed_edit.iris](09_evolution/typed_edit.iris) | host | typed edit: ok |
| [10_integration/llm_protocol.iris](10_integration/llm_protocol.iris) | dual | llm protocol: ok |
| [10_integration/checked_network.iris](10_integration/checked_network.iris) | dual | checked network: ok |
| [10_integration/ros2_geometry.iris](10_integration/ros2_geometry.iris) | dual | ros2 geometry: ok |
| [embedded/uno_button_led.iris](embedded/uno_button_led.iris) | embedded | See embedded instructions |
| [embedded/control_loop.iris](embedded/control_loop.iris) | embedded | See embedded instructions |
| [03_types/macros.iris](03_types/macros.iris) | dual | macros: ok |
| [04_safety/weak_references.iris](04_safety/weak_references.iris) | dual | weak references: ok |
| [05_concurrency/parallel.iris](05_concurrency/parallel.iris) | dual | parallel: ok |
| [05_concurrency/runtime_soak.iris](05_concurrency/runtime_soak.iris) | native | runtime soak: ok |
| [06_data/files.iris](06_data/files.iris) | dual | files: ok |
| [06_data/database.iris](06_data/database.iris) | dual | database: ok |
| [10_integration/ffi_cells.iris](10_integration/ffi_cells.iris) | dual | ffi cells: ok |
| [07_ml/model_graph.iris](07_ml/model_graph.iris) | graph export | Dense-to-ReLU graph structure |

For typed editing in PowerShell:

```powershell
$env:IRIS_FORCE_INTERP = '1'
iris --emit eval examples/09_evolution/typed_edit.iris
Remove-Item Env:IRIS_FORCE_INTERP
```

`files.iris` writes `iris-example-note.txt` in its working directory. Run it in a
scratch directory if you want to keep outputs separate. `runtime_soak.iris`
defaults to one second and accepts `IRIS_SOAK_DURATION_MS`, `IRIS_SOAK_WIDTH`,
and `IRIS_SOAK_REPORT_MS`.

[hot_swap.rs](hot_swap.rs) is a Rust embedding example, run with
`cargo run --example hot_swap`. It requires LLVM ORC and verifies that in-flight
leases retain the old generation during a replacement and rollback.

[unrestricted_evolution.rs](unrestricted_evolution.rs) demonstrates the separate
explicit host authority API. Run `cargo run --example unrestricted_evolution`;
it skips behavioral/resource gates and grants candidates the host's capabilities.
Compiler validity, manifest/ABI checks, generation leases and rollback remain.

[local_model.iris](10_integration/local_model.iris) and
[ros2_topics.iris](10_integration/ros2_topics.iris) require external runtimes;
see [integration setup](10_integration/README.md). Their `manual` catalog entries
are compile-checked and are not counted as successful external-service runs.

The [embedded README](embedded/README.md) covers board wiring and device proof
reports. Assertions are deliberately replaced by status returns inside the
embedded subset, which cannot allocate panic messages.
