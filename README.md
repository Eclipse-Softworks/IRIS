# IRIS

**Intermediate Representation for Intelligent Systems** — Version `1.0.0-rc2`

[![License: GPL v2+](https://img.shields.io/badge/License-GPL%20v2%2B-blue.svg)](LICENSE)
[![Compiler](https://img.shields.io/badge/compiler-LLVM%2023.1.1%20%2B%20SSA%20IR-blueviolet.svg)](docs/REQUIREMENTS.md)
[![VS Code Extension](https://img.shields.io/badge/vscode--iris-1.0.8-007ACC.svg)](vscode-iris/README.md)

IRIS is a statically typed, dual-backend programming language designed to build, train, verify, deploy, and safely evolve intelligent, concurrent, and cyber-physical software inside a single toolchain.

---

## Purpose

Adaptive software is usually split across incompatible stacks: Python for modeling and agent loops, C or C++ for low-latency control and hardware interfaces, and ad-hoc deployment scripts that cannot statically verify safety or roll back regressions live.

IRIS unifies those layers around one frontend, one SSA IR, and explicit safety boundaries (see [docs/direction.md](docs/direction.md)):

```text
  Source (.iris)
     │
     ▼  Lexer ─► Parser ─► Type & Borrow Checker ─► Effect Checker ─► SSA IR ─► Optimizer
     │
     ├──► Native AOT / ORC JIT (LLVM 23.1.1 + clang / ld.lld)
     ├──► Freestanding Embedded Bundles (Cortex-M4F, Cortex-M33, ESP32-C3, ESP32, Arduino Uno)
     ├──► Bounded SSA IR Interpreter (deterministic sandboxing & time-travel trace debugging)
     └──► Governed Evolution (7 verification gates ─► SHA-256 constitution ─► atomic ORC hot-swap)
```

---

## What IRIS Can Do

### 1. Statically Typed Systems & Concurrent Language
- **Types & algebraic data modeling**: `i64`, `f64`, `bool`, `str`, `unit`, fixed-size arrays `[T; N]`, tuples `(A, B)`, `list<T>`, `map<K, V>`, `record` structs, `choice` enums, `option<T>` (`some`/`none`), and `result<T, E>` (`ok`/`err` with `?` propagation).
- **Control flow & abstraction**: expression-oriented `if`/`else`, `while`, `for i in a..b`, `loop`/`break`/`continue`, exhaustive `when` pattern matching, first-class closures, `trait`/`impl` polymorphism, and `bring` module imports (`std.*` or file paths).
- **Concurrency & parallelism**: `async def`/`await`, `spawn` tasks, typed `channel()` with `send`/`recv`, `par for` data-parallel loops, and lock-free atomics (`atomic`, `atomic_load`, `atomic_store`, `atomic_add`).
- **Static effect checking & transactional speculation**: `--strict-effects` enforces effect declarations (`io`, `alloc`, `ffi`, `reflect`), and `std.speculation` provides nestable thread-local transactional rollback for lists, maps, atomics, and staged whole-file writes.

### 2. Machine Learning, Autodiff & LLM Tooling
- **Language-level reverse-mode autodiff**: `tape(...)`, `backward(...)`, and `grad(...)` differentiate across functions, branches, and loops on a chunked tape.
- **Runtime-loaded ML backends** ([docs/ml-backends.md](docs/ml-backends.md)): dynamically discovers **ONNX Runtime**, **TensorFlow C API**, and **LibTorch** via `dlopen` at first use — no compile-time SDK dependency when unused.
- **Native ML, RL, AIS & LLM modules**: dense tensors (`std.tensor`), neural layers (`std.nn`), classic ML (`std.ml`), replay buffers & RL (`std.rl`), Active Inference / homeostatic control (`std.ais`), Bayesian uncertainty (`std.uncertainty`), MAML-style inner loops (`std.meta_learning`), and OpenAI-compatible chat/embeddings/tool-calling (`std.llm`).

### 3. Cyber-Physical Control, Robotics & Embedded Targets
- **Real ROS 2 client stack** ([docs/ros2-build.md](docs/ros2-build.md)): `std.ros2` binds directly to `rclcpp`, `rclcpp_action`, `rclcpp_lifecycle`, and `tf2_ros` through the C-ABI bridge (`iris_ros2.dll` / `libiris_ros2.so`), supporting runtime-typed topics, QoS profiles, services/clients, actions, typed parameters with range validation, lifecycle state machines, and `tf2` transforms.
- **Freestanding microcontroller codegen** ([docs/embedded.md](docs/embedded.md)): `iris embedded <file.iris> --target <target>` targets `cortex-m4f`, `cortex-m33`, `esp32-c3`, `esp32`, and `arduino-uno`. It statically enforces an allocation-free profile (no heap allocations, dynamic strings/collections, closures, or recursion), verifies zero allocator symbols in the emitted object, and emits `.ll`/`.o`, `iris_embedded.h`, and an `iris_embedded.json` proof manifest.
- **Hardware & protocol interop**: host-side microcontroller serial links (`std.serial`), TCP sockets (`std.net`), HTTP/1.1 client/server (`std.http`, `std.http_server`), TLS handshake & JA3/JA4 fingerprinting (`std.tls`), libpcap packet dissection (`std.pcap`), SQLite (`std.sql`), and C/C++ header binding generation (`iris bindgen`, including `extern "C"` wrapper shims).

### 4. Governed Self-Evolution & Typed Metaprogramming
- **7-gate constitutional verification** ([docs/governed-evolution.md](docs/governed-evolution.md)): evaluates candidate code through (1) full parser/type/borrow/effect/RC-safe checks, (2) resource & ABI checks (`i64`, `f64:N`, `record`, or multi-gateway manifests), (3) deterministic bounded tests, (4) `test_transaction_rollback()` verification, (5) a SHA-256-pinned external constitution, (6) shadow canary comparison against baseline, and (7) LLVM ORC activation via lease-safe `HotSwapLease` with generation-conditional rollback.
- **Tamper-evident audit chain**: appends canonical hash-chained JSONL records (`--audit`) verified against an independently stored `--audit-head` checkpoint.
- **Compiler-hosted metaprogramming & host FFI**: `iris meta` and `std.meta` (`meta_analyze`, `meta_emit_ir`, `meta_apply`) validate edits through the real Rust compiler pipeline, while [crates/iris_ffi](crates/iris_ffi/src/lib.rs) exports the evolution coordinator as a C-ABI shared library (`iris_evolution.dll` / `libiris_evolution.so`) for Python, C/C++, Go, and Node.js hosts.

### 5. Dual-Backend Execution, Code Emission & Debugging
- **16 `--emit` modes**: `binary` (default executable), `jit` (LLVM ORC), `llvm`, `llvm-complete`, `ir`, `eval` (IR interpreter), `simd`, `pgo-instrument`, `pgo-optimize`, `cuda`, `cuda-ptx`, `graph`, `onnx`, `onnx-binary`, `tensorrt`, `python-ext`.
- **IDE & Dual-Mode DAP** ([vscode-iris/README.md](vscode-iris/README.md)):
  - Built-in **LSP server** (`iris lsp`) for diagnostics, completions, hover, definitions, references, rename, code actions, formatting, and semantic tokens.
  - **Trace-based interpreter DAP** (`iris dap`, `native: false`) with conditional breakpoints, hit conditions, logpoints, variable/stack inspection, and time-travel reverse stepping (`stepBack`, `reverseContinue`).
  - **Native DWARF DAP** (`native: true`) supporting both `launch` and `attach` (`processId`/`pid`/`target`), DWARF metadata for scalars, `str`, `record`, `enum`, fixed scalar arrays, `option`, `result`, `tuple`, `list`, `map`, and pointer fallbacks, forwarding to GNU GDB (`gdb -i dap`, verified with GDB 17.1 on Windows) or LLDB (`IRIS_NATIVE_DAP` / `lldb-dap`), and passing through `supportsStepBack` when supported by the native backend.

---

## Standard Library (51 Modules)

Import any standard library module with `bring std.<module>`. Full API details are in [docs/stdlib-reference.md](docs/stdlib-reference.md).

| Domain | Modules (`src/stdlib/*.iris`) |
| --- | --- |
| **AI, ML & Autonomy** | `adaptive`, `ais`, `llm`, `meta_learning`, `ml`, `nn`, `rl`, `stochastic`, `tensor`, `uncertainty` |
| **Data, Math & Visualization** | `csv`, `dataframe`, `dataset`, `json`, `kv`, `math`, `sql`, `svg`, `table`, `termplot` |
| **Collections & Core** | `bitset`, `buffer`, `collections`, `container`, `deque`, `fmt`, `heap`, `iter`, `log`, `queue`, `set`, `string`, `testing`, `time`, `unicode` |
| **Systems, Networking & Robotics** | `async`, `crypto`, `ffi`, `fs`, `http`, `http_server`, `net`, `os`, `path`, `pcap`, `ros2`, `serial`, `tls` |
| **Reflection & Safety** | `meta`, `reflect`, `speculation` |

---

## Quick Start

### Prerequisites & Building from Source

See [docs/REQUIREMENTS.md](docs/REQUIREMENTS.md) and [docs/getting-started.md](docs/getting-started.md) for platform packages and environment setup. On Windows, IRIS uses a dynamically loaded **LLVM 23.1.1** toolchain (`clang`, `ld.lld`, LLVM-C) and a MinGW UCRT64 sysroot for native binaries; IR emission, checking, LSP, and interpreter evaluation run without LLVM.

```powershell
git clone https://github.com/Eclipse-Softworks/IRIS.git
cd IRIS
cargo build --release --locked
```

Check your local toolchain setup after building:

```powershell
.\target\release\iris.exe --version
.\target\release\iris.exe doctor
```

### Your First Project

```powershell
iris new app hello          # templates: app, cli, server, ros2, controller
cd hello
iris run main.iris
iris test
```

While developing:

```powershell
iris check                  # type, borrow, and effect checks without linking
iris watch main.iris        # re-run on save (--test, --check)
iris explain E0109          # explain any compiler diagnostic code
iris fmt --check main.iris  # verify formatting
```

### Quick Code Examples

**Reverse-mode automatic differentiation:**

```iris
def square(x: f64) -> f64 { return x * x }

def main() -> i64 {
    val x = tape(3.0)
    val loss = square(x)
    backward(loss)
    assert(grad(x) == 6.0)
    return 0
}
```

**Pattern matching & error propagation:**

```iris
choice Direction { North, South, East, West }

def parse_and_double(s: str) -> option<i64> {
    val n = parse_i64(s)?
    return some(n * 2)
}

def describe(dir: Direction) -> str {
    return when dir {
        Direction.North => "north",
        Direction.South => "south",
        _               => "east or west",
    }
}
```

**Channels & concurrent tasks:**

```iris
def main() -> i64 {
    val ch = channel()
    spawn {
        send(ch, 42)
    }
    val v = recv(ch)
    assert(v == 42)
    return 0
}
```

---

## CLI Overview

Run `iris --help` or `iris <command> --help` for full flags.

| Category | Commands |
| --- | --- |
| **Build & Execute** | `iris run <file>`, `iris build <file> -o <out>`, `iris embedded <file> --target <t>`, `iris --emit <kind> <file>`, `iris repl` |
| **Check, Test & Docs** | `iris test [file]`, `iris check [paths...]`, `iris watch <file>`, `iris fmt [file]`, `iris docs [file]`, `iris explain [code]` |
| **Analysis & Interop** | `iris meta <file> [--emit-ir]`, `iris bindgen <header>`, `iris bench <file>`, `iris profile [file]` |
| **Governed Evolution** | `iris evolve`, `iris evolve-search`, `iris evolve-live`, `iris evolve-serve`, `iris evolve-unrestricted --acknowledge-unsafe`, `iris service-daemon` |
| **Projects, Packages & Setup** | `iris new <template> [name]`, `iris pkg <init\|add\|install\|publish\|search>`, `iris lsp`, `iris dap`, `iris doctor`, `iris bug-report <file>`, `iris setup`, `iris install`, `iris upgrade` |

---

## Learning Path & Example Projects

### 10-Stage Learning Curriculum ([examples/](examples/README.md))

Work through [`examples/01_basics`](examples/01_basics) to [`examples/10_integration`](examples/10_integration) covering core syntax, functions, ADTs/traits, `--strict-effects` and `std.speculation`, channels/atomics, SQLite/HTTP/JSON, autodiff/ML, active inference, governed evolution, and external integrations. Run the automated catalog check:

```powershell
python tools/verify_learning.py --iris target/release/iris.exe
```

### Showcase Projects ([projects/](projects/README.md))

The `projects/` workspace includes 37 standalone applications and benchmarks across cyber-physical autonomy, AI/ML, networking, security, and systems:

| Domain | Highlighted Projects |
| --- | --- |
| **Self-Evolving & Adaptive Systems** | [`autonomous_deep_space_probe`](projects/autonomous_deep_space_probe/README.md), [`continuous_background_learning`](projects/continuous_background_learning/README.md), [`autonomous_evolution_lab`](projects/autonomous_evolution_lab/README.md), [`algorithmic_code_evolution`](projects/algorithmic_code_evolution/README.md), `self_upgrading_service`, `autonomous_organism` |
| **Robotics & Embedded Control** | [`quadrotor_autopilot`](projects/quadrotor_autopilot/README.md), [`turtlesim_autopilot`](projects/turtlesim_autopilot/README.md), [`robotic_actuator_control`](projects/robotic_actuator_control/README.md), [`realtime_embedded_actuator`](projects/realtime_embedded_actuator/README.md), `lifecycle_sensor_fusion`, `autonomous_drone_hunter`, `uno_controls` |
| **AI, Scientific & Simulation** | [`quantum_simulator`](projects/quantum_simulator/README.md), [`artificial_life_ecosystem`](projects/artificial_life_ecosystem/README.md), `llm_agent`, `model_gateway`, `multimodal_ai_orchestrator`, `learning_service`, `learning_scheduler`, `navier_stokes`, `chaotic_discovery` |
| **Networking, Security & Tooling** | [`intrusion_detection_system`](projects/intrusion_detection_system/README.md), [`cyber_operations_center`](projects/cyber_operations_center/README.md), [`jit_superoptimizer`](projects/jit_superoptimizer/README.md), `async_gateway`, `ledger`, `job_pipeline`, `cli_tools` |

---

## VS Code Extension (`vscode-iris`)

The companion extension in [vscode-iris/](vscode-iris/README.md) integrates syntax highlighting, LSP language intelligence, integrated commands (`Run`, `Build`, `Test`, `Check`, `Watch`, `Explain`, `Doctor`, `New Project`, `Emit IR/LLVM`, `Evolution Gates`, `Package Manager`), and dual-mode debugging (`launch` in trace or native DWARF mode, plus `attach` to running native binaries).

---

## Testing & Verification

```powershell
# Compiler unit tests
cargo test --lib

# Specific integration suites or the full workspace suite
cargo test --workspace --no-fail-fast

# Code formatting & strict lints
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

---

## Documentation

| Document | Topic |
| --- | --- |
| [docs/getting-started.md](docs/getting-started.md) | Building, `iris doctor`, project scaffolding, and editor workflow |
| [docs/direction.md](docs/direction.md) | Vision, priorities, and design rationale for IRIS |
| [docs/language-reference.md](docs/language-reference.md) | Concise syntax, types, collections, concurrency, and CLI quick reference |
| [docs/BOOK.md](docs/BOOK.md) / [docs/SPEC.md](docs/SPEC.md) | Language book and formal specification |
| [docs/stdlib-reference.md](docs/stdlib-reference.md) | Standard library modules and functions |
| [docs/governed-evolution.md](docs/governed-evolution.md) | 7 verification gates, safety constitutions, audit logs, and ORC hot-swapping |
| [docs/embedded.md](docs/embedded.md) | Allocation-free microcontroller targets and proof manifests |
| [docs/ml-backends.md](docs/ml-backends.md) | Dynamic ONNX Runtime, TensorFlow, and LibTorch loading |
| [docs/metaprogramming-networking-llm.md](docs/metaprogramming-networking-llm.md) | `std.meta`, `std.net`, `std.http`, and `std.llm` |
| [docs/ros2-build.md](docs/ros2-build.md) | Building and running the ROS 2 `rclcpp` bridge |
| [docs/REQUIREMENTS.md](docs/REQUIREMENTS.md) | Host dependencies, LLVM 23.1.1, and MinGW/Linux/macOS setup |
| [docs/STABILITY.md](docs/STABILITY.md) & [docs/known-issues.md](docs/known-issues.md) | Release-candidate stability policy and known limitations |
| [docs/CHANGELOG.md](docs/CHANGELOG.md) | Release history and unreleased changes |

---

## Contributing & License

See [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md) and [docs/CODE_OF_CONDUCT.md](docs/CODE_OF_CONDUCT.md).
IRIS is licensed under the **GNU General Public License v2.0 or later** ([LICENSE](LICENSE)).
