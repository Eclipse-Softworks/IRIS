# IRIS 1.0.0-rc1 Current Language Lock

This document captures the current ready-to-use IRIS language surface for the 1.0.0-rc1 build. It is the reference for examples, LSP/DAP support, editor grammar, installer dependency checks, and ML pipeline examples.

## Locked Language Surface

- Entry point: `def main() -> i64`; public programs conventionally finish with
  explicit `return 0` (the trailing semicolon is optional).
- Bindings: `val` and `var` are both inferred bindings when no explicit type annotation is written. `val` is inferred immutable; `var` is inferred mutable.
- Control flow: `if/else`, `while`, `loop`, `break`, `continue`, `for i in a..b`, `for item in list`, `par for`, `return`, and expression tails.
- Pattern matching: `when` over `choice` enums, `option<T>`, `result<T,E>`, literals, tuples, ranges, and `_`.
- Declarations: `def`, `pub def`, `async def`, `record`, `choice`, `const`, `type`, `trait`, `impl`, `extern def`, `bring`, and `model`. Strict builds require extern host calls to declare `effect ...` or explicit `effect pure`.
- Types: `i64`, `i32`, `i8`, `u8`, `u32`, `u64`, `usize`, `f64`, `f32`, `bool`, `str`, `tensor<dtype, [dims]>`, `[T; N]`, tuples, `list<T>`, `map<K,V>`, `option<T>`, `result<T,E>`, `chan<T>`, `atomic<T>`, `mutex<T>`, `task_group`, `grad<T>`, `sparse<T>`, named records/enums, and function types.
- Expressions: calls, method calls, field access, indexing, tuple indexing, arrays, casts with `to`, lambdas, `await`, and `?`.
- Model DSL: `model Name { input x: T layer y Op(...) output y }` with contextual `input`, `layer`, and `output`.
- Standard library: embedded `std.math`, `std.string`, `std.fmt`, `std.fs`, `std.json`, `std.csv`, `std.net`, `std.http`, `std.llm`, `std.meta`, `std.kv`, `std.table`, `std.dataset`, `std.dataframe`, `std.iter`, `std.deque`, `std.bitset`, `std.crypto`, `std.os`, `std.ffi`, `std.async`, `std.testing`, `std.log`, `std.ml`, `std.nn`, `std.tensor`, `std.ais`, and `std.http_server`.
- Native execution: LLVM-C in-process object emission, ORC JIT execution, ABI-verified hot swapping, and direct MinGW `ld.lld` linking on the validated Windows target.
- Governed evolution: `iris evolve` promotes `policy(i64) -> i64` through seven gates and requires a persisted audit-head checkpoint. Library APIs additionally expose v2 whole-program manifests, JSON/command/scalar gateways, explicit JSON state handoff, eight-generation rollback, atomic gateway leases, and content-addressed artifacts.
- Unrestricted evolution: `UnrestrictedEvolutionAuthority` is a separate,
  explicitly acknowledged process-local API that skips behavioral/resource
  gates while retaining compiler validity, manifest/ABI checks, atomic ORC
  activation, leases, and rollback.
- Typed metaprogramming: `iris meta` and compiler-hosted `std.meta` expose typed
  source metadata, IR, and fully recompiled checked edits without self-hosting.
- Network/AI: typed bounded TCP and status-bearing HTTP are locked; HTTPS is
  verified through WinHTTP on Windows. `std.llm` supports chat/tools/embeddings
  and local backend wrappers. `std.ml`/`std.ais` include production lifecycle,
  health, degradation and emergency-action records.
- Async execution: `async def` returns a single-result `chan<T>` awaitable for
  RC1 source compatibility. Native `spawn`/async work is scheduled on a fixed
  executor capped at 32 workers, and a worker awaiting an empty result channel
  helps execute queued work before sleeping. It does not create one OS thread
  per async call. Structured `task_group` values schedule on the same executor;
  `task_group_join` closes the group and waits while helping queued work, and
  `task_group_cancel` is observed through `std.async.cancellation_requested()`
  and `std.async.task_group_cancelled(group)`. Cancellation is cooperative:
  queued tasks skip their body at entry, while running CPU tasks should poll at
  bounded intervals.

## Tooling Contract

- LSP: diagnostics, hover, rich completions, go to definition, document symbols, signature help, formatting, quick fixes, inlay hints, references, and rename.
- DAP: launch, line/conditional/log/hit-count breakpoints, continue, next, step in, step out, step back, pause, restart, loaded sources, stack trace, locals, set variable, watch/evaluate, exception info, and debug-console completions.
- VS Code extension: grammar, snippets, run/build/debug commands, inline run/debug code lenses, REPL, IR/LLVM viewers, LSP status bar, and settings for executable path, formatting, inlay hints, timing, and stop-on-entry debugging.
- ML learning path: `projects/learning_service/main.iris` trains deterministic neural weights, checks predictions, registers a version, and records health/degradation. `examples/07_ml/` separates scalar AD, tensor AD, neural-layer gradients, and graph export.

## Binary Install Dependencies

The `iris` binary itself includes the Rust compiler frontend, embedded runtime C source, and embedded IRIS stdlib sources. Running source files through user-facing commands uses the LLVM/native pipeline, so execution dependencies are:

| Use case | Required on target device |
| --- | --- |
| `iris --version`, parsing, diagnostics, LSP, DAP startup, REPL startup | `iris` binary and the OS runtime needed by that binary |
| `iris run`, `iris build`, `--emit eval`, `--emit jit`, `--emit binary` | `iris`, a loadable LLVM-C library, `lld`, and a linkable target sysroot; Clang is only a compatibility fallback where direct object/link support is unavailable |
| Windows native execution | LLVM-C/ORC plus `ld.lld` and a MinGW UCRT64 sysroot with headers, CRT objects, import libs, and GCC runtime libs |
| Linux native execution | LLVM-C/ORC plus `lld`, compatible libc development files, and system libs such as `libm` and pthreads |
| macOS native execution | Apple Command Line Tools or a bundled LLVM that can see the macOS SDK |
| Python FFI | A compatible Python installation available at runtime |
| C/Rust FFI | The user-provided shared libraries (`.dll`, `.so`, `.dylib`) and their transitive dependencies |
| Native ONNX/TensorFlow backend handoff | Backend SDK env vars plus `IRIS_NATIVE_ML_BACKENDS=1` at build/run time |
| HTTPS and remote LLM calls on Windows | WinHTTP supplied by the operating system; valid trust store and network access |
| HTTPS on other native targets | Not yet locked; `http_tls_available()` is false and HTTPS fails closed |
| Compiler-hosted `std.meta` | Run under the IRIS compiler/interpreter host; standalone native binaries feature-detect it as unavailable |
| VS Code extension | VS Code and the `iris` binary on PATH or configured through `iris.executablePath` |

## Bundling Policy

Installers can bundle dependencies. The current installer layout already supports a `toolchain/` payload:

- Windows full installers should bundle `toolchain/llvm` and `toolchain/ucrt64` so `iris run` works on a clean machine.
- Portable Windows zips can include the same `toolchain/` folder next to `iris.exe`.
- Linux and macOS packages may bundle LLVM, but should still detect or install the host sysroot/SDK through the package manager or Xcode Command Line Tools because those are platform-specific and large.
- Compact installers may install only `iris` and then print the exact missing dependency command for native execution.
