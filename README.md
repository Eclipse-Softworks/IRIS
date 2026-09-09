# IRIS

<p align="center"><img src="logo/iris-logo.png" alt="IRIS" width="160"></p>

IRIS is a compiled, statically typed language for software, learning systems,
and embedded control. This release remains **1.0.0-rc1**.

Write ordinary programs with `def`, `val`, `var`, `record`, `choice`, and
explicit `return`. Use the same language for tensor differentiation, structured
concurrency, model integration, and checked evolution workflows.

```iris
def main() -> i64 {
    val answer = 6 * 7;
    assert(answer == 42);
    println(f"Hello, IRIS! answer = {answer}");
    return 0
}
```

## Start here

```powershell
cargo build --release --locked
.\target\release\iris.exe run examples/01_basics/hello.iris
.\target\release\iris.exe test projects/ledger/main.iris
```

On Linux/macOS use `./target/release/iris`. Native execution needs LLVM, a
linker, and the target system libraries. Windows uses LLVM-C and a MinGW
UCRT64 sysroot; the compiler binary does not bundle that toolchain.
See [installation](docs/getting-started.md) and
[dependencies](docs/REQUIREMENTS.md).

## Learn by running

The [example catalog](examples/README.md) progresses from bindings and functions
through types, ownership, effects, concurrency, data, ML, AIS, and integration.
Every runnable example checks its results. Examples requiring the compiler host,
a service, or a board are identified beside their commands.

The [projects](projects/README.md) combine these features into complete programs:

| Project | What you build |
| --- | --- |
| [Ledger](projects/ledger/) | Multi-file integer-money ledger with checked errors |
| [Job pipeline](projects/job_pipeline/) | Structured workers, channels, and atomic completion |
| [Learning service](projects/learning_service/) | Deterministic neural training, evaluation, registry, and health |
| [Model gateway](projects/model_gateway/) | Offline protocol checks and a configurable live LLM client |
| [Robot controller](projects/robotic_actuator_control/) | Bounded actuator control in a deterministic simulation |
| [Multimodal orchestrator](projects/multimodal_ai_orchestrator/) | Concurrent simulated modalities, tensor fusion, and AIS selection |
| [Evolution lab](projects/autonomous_evolution_lab/) | Candidate tests, transaction rollback, and audited ORC promotion |

The runnable catalog is the source of truth: [catalog.json](examples/catalog.json).
Run both execution paths with:

```powershell
python tools/verify_learning.py --iris target/release/iris.exe
```

## Current language and runtime

| Area | Current surface and scope |
| --- | --- |
| Types | Records, choices, patterns, generics, const parameters, container constructors, traits, blanket implementations, and dynamic trait dispatch |
| Errors and effects | Options, results, `?`, `try/catch`, declared effect rows, callback effect contracts, and handler replacement |
| Memory | Reference-counted runtime objects, weak references, checked integer arithmetic and indexing; source borrowing/move checks are lexical annotations |
| Concurrency | Channels, atomics, mutexes, parallel ranges, async result channels, and a bounded worker executor with cooperative task-group cancellation |
| Learning | Scalar tape AD, differentiable tensors through closures/branches/loops/traits, `std.nn` training, and external model adapters |
| AIS | Viability bounds, active inference, agent/model lifecycle, health, degradation, and decision policies; application behavior still needs application-specific validation |
| Networking and LLMs | Bounded typed TCP, HTTP status/headers/timeouts, chat/tools/embeddings; native HTTPS currently uses Windows WinHTTP |
| Metaprogramming | Typed analysis and checked source edits under the compiler host; standalone native programs must check availability |
| Evolution | ORC JIT, ABI-checked generation leases/rollback, and seven-gate `iris evolve`; unrestricted activation is an explicit host API with separate authority |
| Embedded | Allocation-free scalar component bundles for supported Cortex-M/ESP32 profiles and a restricted Uno profile; board integration and device verification are separate steps |
| Editor | Extension 1.0.5: completion, hover/effects, navigation, diagnostics, formatting, Test Explorer, and trace-based DAP debugging |

See [language guide](docs/BOOK.md), [language reference](docs/SPEC.md),
[stdlib reference](docs/stdlib-reference.md), and
[current boundaries](docs/known-issues.md).
LLVM/native is the primary execution path. Other export backends have
operation-specific coverage; exporting a graph does not train a model.

## Build, format, and debug

```text
iris run file.iris
iris build file.iris -o app
iris --emit jit file.iris
iris --strict-effects --emit ir examples/04_safety/effects.iris
iris fmt file.iris
iris fmt file.iris --check
iris test file.iris --no-color
iris docs file.iris --output api.html
```

Install the [VS Code extension](vscode-iris/README.md) and point
`iris.executablePath` at an installed compiler copy. The debugger uses interpreter
traces; native process attachment and register inspection are not provided.

## Contribute

```text
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --no-fail-fast
```

The learning catalog is also checked by `cargo test --test examples_showcase`.
Tests that depend on external toolchains report their capability requirements.
Use asserting reproductions when reporting defects.

Source: [Eclipse-Softworks/IRIS](https://github.com/Eclipse-Softworks/IRIS).
Licensed under [GPL-2.0-or-later](LICENSE).
