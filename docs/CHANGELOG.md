# Changelog

All notable changes to the IRIS programming language are documented here.

This project follows [Keep a Changelog](https://keepachangelog.com/) conventions.

---

## [Unreleased]

### Added

- Rebuilt progressive examples and seven complete projects with explicit
  returns, assertions, per-entry execution modes, and native/interpreter
  catalog checks. Added real localhost LLM verification and Rust ORC embedding
  examples alongside separately classified SDK/board integrations.

- VS Code Test Explorer discovery plus run/debug profiles for zero-argument
  `test_*` functions, backed by a DAP named-function launch entry.
- Rich LSP hovers for functions, traits, and algebraic effects, including
  documentation, generic signatures, declared/inferred effects, and explicit
  unsafe-region status.
- Exact-range dead/unused, unreachable, unsafe, and possible-infinite-loop
  diagnostics with semantic-token modifiers and LSP unnecessary tags.
- A compiler-sourced standard-library completion registry covering the current
  AIS, ML/NN/tensor, ROS 2, LLM, reflection, metaprogramming, and networking
  modules.
- In-process LLVM ORC JIT loading and ABI-verified, lease-safe hot swapping.
- Generation-conditional rollback with deterministic SHA-256 ABI fingerprints.
- Transactional speculative state and staged filesystem rollback.
- `iris evolve`, a bounded seven-gate promotion workflow with pinned
  constitutions, shadow canaries, automatic rollback API, and a tamper-evident
  JSONL audit chain.
- Allocation-free embedded bundles and physical Arduino Uno validation.
- Differentiable tensor/NN control-flow coverage and AIS/ROS 2 v2 examples.
- V2 whole-program evolution manifests with reviewed JSON, command, and scalar
  gateways; explicit JSON state handoff; atomic multi-gateway leases; and an
  immutable content-addressed candidate artifact store.
- Bounded eight-generation hot-swap history with exact-generation rollback.
- Mandatory persisted `--audit-head` verification for `iris evolve`.
- Enforced Windows native-worker sandbox: ephemeral zero-capability LPAC,
  opt-out from broad application-package access, suspended startup before Job
  assignment, memory/CPU/process limits, bounded stdout/stderr, wall-time
  termination, token attestation, and fail-closed unsupported platforms.
- Explicitly acknowledged unrestricted whole-program activation with isolated
  authority, v2 manifest/ABI validation, ORC materialization, atomic leases,
  generation history, and exact rollback.
- Compiler-hosted typed metaprogramming via `iris meta` and `std.meta`, including
  typed/effect/ABI/CFG metadata, IR emission, and fully recompiled UTF-8 edits.
- Typed TCP stream/listener APIs with timeouts, exact writes, bounded reads,
  shutdown/close, and result-based transport errors.
- Status-bearing HTTP requests with custom headers, bodies, timeouts, bounded
  responses, Windows WinHTTP TLS/certificate validation, and JSON path queries.
- `std.llm` provider-neutral chat, tool-call, embedding and local-model APIs.
- Production `std.ml` model session/registry/health lifecycle and `std.ais`
  degraded/emergency agent lifecycle.
- Fixed-size native async executor (maximum 32 workers) shared by `spawn` and
  lowered `async def`, cooperative queue helping during nested `await`, and
  scheduler telemetry through `std.async`.
- Structured task-group cancellation queries in `std.async`:
  `cancellation_requested()` for the current task and
  `task_group_cancelled(group)` for explicit group state.

### Changed

- `iris fmt` is now comment- and literal-preserving, parse-safe, idempotence
  tested, line-width aware, and configurable by the VS Code extension.
- Supported Windows MinGW native builds emit objects through LLVM-C and link
  directly with `ld.lld`; Clang is no longer the normal fallback for that path.
- Public examples use the locked `def`/`val`/`record`/`choice`/`bring` syntax and
  the earlier 116-entrypoint collection passed an IR compilation audit. The
  replacement learning catalog now checks native execution and interpreted
  results separately; see `examples/catalog.json` for current coverage.
- Extern functions accept explicit effect contracts; the AIS/tensor/NN/ML graph
  passes strict effect checking with deterministic deduplicated diagnostics.
- The complete Rust workspace passes `cargo clippy --all-targets -- -D warnings`
  while retaining the declared Rust 1.75 MSRV.
- Collection/string `join` is inferred as allocation rather than task joining;
  concurrency primitives use the spellable `thread` effect instead of the
  reserved `spawn` keyword.
- Native task groups now share the bounded executor with `spawn`/`async def`.
  `task_group_join` closes the group, helps queued work while waiting, and
  avoids one OS thread per grouped task.

### Fixed

- Native `par_map` now carries the list element/result types through lowering
  and calls IRIS closures through generated ABI adapters. Results retain input
  order, and worker-creation failure falls back to safe inline execution.
- Channel `select` now preserves the selected channel's element type instead of
  leaking an unresolved inference type into native code.
- Nested native task-group captures use the correct runtime boxing helper, and
  Windows timed channel receive now converts the system clock epoch correctly.
- The source test runner now supports checked compile-fail directives and
  diagnostic substrings. Legacy result-returning fixtures were converted to
  zero-status assertion wrappers, and the stale syntax/known-broken backlog was
  reduced to the single import-only documentation fixture.
- LSP integration tests now distinguish error diagnostics from unused-code
  hints, preserving dead-code highlighting without classifying valid programs
  as compiler failures.
- `iris test` now preserves a program's own `main` under an internal symbol
  when running any other `test_*` function, preventing duplicate LLVM `main`
  definitions. Named native eval entries use the same collision-free wrapper,
  and native test processes now use the configured execution timeout instead
  of being able to freeze the full suite indefinitely.
- `iris bench <file> [-n N]` now reaches the benchmark runner instead of
  evaluating the file once, restoring numeric performance-gate output.
- Public compiler-to-module APIs now run lowering and optimization on the same
  guaranteed 64 MiB stack as CLI compilation, removing Linux/macOS test-thread
  stack overflows for larger integration programs.
- Parser recovery treats an empty token slice as EOF instead of indexing past
  the stream, closing the fuzz-discovered panic.
- UDP numeric-address validation no longer depends on the non-portable
  `INADDR_NONE` macro, allowing the C runtime to compile on current macOS SDKs.
- Scalar fixed-array checks now call a checked runtime boundary without
  splitting the surrounding IR block. This preserves valid LLVM phi
  predecessors for array access inside `while` and `for` loops.
- Windows CI installs the UCRT64 sysroot used by the required direct MinGW
  `ld.lld` regression test.
- Parser recovery now guarantees forward progress at every module nesting
  level, so an unexpected declaration separator or closing brace reports a
  diagnostic instead of hanging the compiler or language server.
- Native builds now discover a bundled `sqlite3.dll` beside the IRIS compiler
  before searching `PATH`, allowing database programs to compile and run from
  isolated build directories and release installations.
- Native scalar-array field aliases now preserve their element representation
  and recover their bound from the actual initializer instead of using the
  boxed-array runtime ABI.
- Named source functions used as callbacks now lower through a closure adapter,
  preserving the native calling convention across parameters and record fields.
- `std.fs.append_text` preserves line-feed bytes on Windows by opening its
  stream in binary append mode, matching `read_text` and `write_text`.

- Tape-helper inlining now permits `return` only as the callee's outer terminal
  statement; returns nested in branches stay isolated and cannot escape into a
  caller during differentiable lowering.
- Native closures correctly unbox captured records before field access instead
  of interpreting the tagged runtime wrapper as the record allocation.
- Reverse-mode tape identity now crosses loop back-edges and safe helper
  boundaries ending in `return expr`, so loop-accumulated gradients remain
  differentiable on native and interpreter backends.
- Native task-group lowering now defines every emitted SSA value, including
  programs that import `std.async` without directly using task groups.
- Native spawn trampolines now release boxed capture environments on both normal
  completion and cancelled-entry paths.
- Runtime RC cleanup now releases mutex payloads and task-group payloads,
  including cancellation/join before a task-group allocation is freed.

---

## [1.0.0-rc1] — Tooling & Editor Support

### Added

- **LSP Semantic Tokens** — registers `"semanticTokensProvider"` capability in `src/lsp.rs` and implements a full delta-encoding stream covering keywords, contextual constructs, literals, and AST-resolved user definitions for rich syntax highlighting across Neovim, Helix, Emacs, and VS Code.
- **LSP Document Highlights** — registers `"documentHighlightProvider": true` capability and wires `"textDocument/documentHighlight"` JSON-RPC handler to instantly highlight all occurrences of the identifier under the cursor.
- **LSP Call Hierarchy** — registers `"callHierarchyProvider": true` capability and implements `"textDocument/prepareCallHierarchy"`, `"callHierarchy/incomingCalls"`, and `"callHierarchy/outgoingCalls"` request handlers with full AST walks to generate incoming/outgoing call graphs.
- **DAP Conditional Breakpoints** — advertises `"supportsConditionalBreakpoints": true` in `src/dap.rs` and propagates conditions configured in `setBreakpoints` to `DebugSession` for conditional pause evaluations.

### Tests

- Target integration tests in [lsp_features_v061.rs](../tests/lsp_features_v061.rs) verifying semantic token delta-encoding, call hierarchy preparation, and document highlight reference resolution.
- Target integration tests in [debugger_conditional_breakpoints.rs](../tests/debugger_conditional_breakpoints.rs) asserting conditional breakpoint pauses with loop variable snapshots.

---

## [1.0.0-rc1] — Performance & Security

### Added (1.0.0-rc1)

- **Security audit infrastructure** (`src/security.rs`) — `SecurityPolicy` with
  per-capability allow/deny flags (fs_read, fs_write, network, ffi, process),
  allowlists and blocklists, resource limits (max_file_write_bytes, max_open_files,
  max_connections). Path validation detects traversal attacks, null byte injection,
  and Windows device name abuse. Audit logging records every security-relevant
  operation with timestamps.
- **Reference-counting GC** — C runtime implements side-table reference counting
  (`iris_retain`, `iris_release`) with deep-free semantics for strings, lists,
  maps, options, and results. `iris_gc_collect` sweeps zero-count entries.
  `iris_gc_stats_allocated` / `iris_gc_stats_freed` expose statistics.
- **Copy propagation pass** (`CopyPropPass`) — eliminates duplicate `ConstInt`
  and `ConstFloat` definitions across blocks, with transitive chain resolution.
  Reduces register pressure and enables further DCE.
- **Loop-invariant code motion** (`LicmPass`) — computes dominators, detects
  natural loops via back edges, and hoists pure loop-invariant instructions to
  the loop preheader. Full CFG analysis with iterative dataflow.
- **Benchmark suite expansion** — six new real-world benchmarks: binary search,
  tree traversal, hashmap insert/lookup, Collatz conjecture, Sieve of
  Eratosthenes, and Simpson's rule numerical integration (12 total).
- **Profiler** (`iris profile <file>`) — per-function timing, call counts,
  instruction counts, folded-stack format for flamegraph.pl / speedscope,
  built-in SVG flame graph generator, human-readable summary table.
  CLI options: `--svg`, `--folded`.
- **Sandboxed FFI** — C runtime mirrors Rust-side security policy with
  `iris_sandbox_set_policy`, `iris_sandbox_check_fs_read/write`,
  `iris_sandbox_check_network`, `iris_sandbox_check_ffi`. Default-deny when
  sandbox is active.

### Changed (Unreleased)

- **Compiler pipeline** — now includes `CopyPropPass` (after `StrengthReducePass`)
  and `LicmPass` (after `OpExpandPass`) in all pipeline paths including
  `compile_to_module`.
- **CLI** — added `profile` subcommand.

### Tests

- 36 new integration tests in `v0_6_0_performance_security.rs` covering security policy, path
  validation, audit logging, profiler lifecycle / flame graphs / edge cases,
  CopyPropPass constant dedup, LicmPass safety, full pipeline integration,
  pass manager with all v1.0.0-rc1 passes, and benchmark file existence.

---

## [1.0.0-rc1] — ML & Compute

### Added (Unreleased)

- **Real tensor runtime** — `IrisTensor` struct in C runtime with 30+ functions:
  create, reshape, transpose, element-wise ops, matrix multiply, reductions
  (sum, mean, max, min), unary ops (relu, sigmoid, tanh, exp, log, sqrt, abs),
  print, and memory management.
- **General einsum engine** — interpreter implements full Einstein summation
  notation with arbitrary subscript strings; handles dot products, matrix
  multiply, batched matmul, transpose, trace, and arbitrary contractions.
- **Tensor codegen** — LLVM IR, LLVM stub, and CUDA backends dispatch real
  tensor operations: einsum (with matmul fast-path), unary, reshape, transpose,
  reduce. SIMD-friendly loop nests for x86/ARM.
- **Reverse-mode automatic differentiation** — tape-based backpropagation via
  three new IR instructions (`TapeRecord`, `Backward`, `TapeGrad`). Supports
  17 operations: add, sub, mul, div, neg, sin, cos, exp, log, sqrt, relu,
  sigmoid, tanh, pow, abs, identity, chain rule. Full topological-sort gradient
  propagation in the interpreter.
- **Enhanced sparse tensor ops** — `Sparsify` converts both arrays and dense
  tensors to sparse (index, value) pairs; `Densify` reconstructs dense arrays
  from sparse representation. C runtime provides `iris_tensor_sparsify`,
  `iris_sparse_to_tensor`, `iris_sparse_dot`, `iris_sparse_nnz`.
- **48 new tests** — 24 tensor tests (tensor_runtime_operations.rs), 17 reverse-mode AD tests
  (reverse_mode_automatic_differentiation.rs), 7 sparse tensor tests
  (sparse_tensor_operations.rs).

### Changed

- **ONNX binary export** — already functional from prior work; verified with
  8 passing tests.
- **GPU/CUDA backend** — updated codegen dispatch for all tensor op variants.
- **SIMD codegen** — auto-vectorization paths verified for tight loops.

---

## [Unreleased] — targeting v0.3.0

### Fixed

- **Closure codegen** — rewrote lambda calling convention in LLVM backend: all
  lambdas now use uniform `(ptr %env, params...)` signature with capture
  extraction preamble at entry, fixing crashes for basic closures, captured
  variables, and higher-order function usage. Replaced stub
  `iris_call_closure` runtime function with proper `iris_closure_fn()` and
  `iris_closure_get_capture()` helpers.
- **List sort** — replaced bubble sort with stable O(n log n) merge sort in the
  C runtime.
- **Native concurrency** — three bugs fixed:
  - `ChanRecv` now properly unboxes the pointer returned by `iris_chan_recv()`
    to extract the i64 value.
  - `spawn` now passes captured variables to the trampoline function so spawned
    closures can access parent-scope bindings.
  - `println` / `print` / `eprintln` are now lowered as built-in `Print`
    instructions instead of generic calls, fixing "unresolved Infer" type errors
    when their return values were used in expressions.
- **TCP networking** — replaced stub TCP instruction handlers in the interpreter
  with real TCP calls (`TcpStream::connect`, `TcpListener::bind`, `accept`,
  `read`, `write`, `close`) via the existing `tcp_store` module.

### Added (0.2.0)

- `ROADMAP.md` with milestones v0.3.0 through v1.0.0 and beyond.
- `STABILITY.md` — feature-tier classification (Tier 1 Stable through Tier 4
  Experimental) and 12 stability milestones for v1.0 gate.
- `SPEC.md` — draft language specification covering syntax, semantics, type
  system, builtins, concurrency model, tensor ops, module system. Finalized for
  v0.3.0 (removed "Draft" label, fixed function type grammar, added
  implementation notes).
- Fuzz testing infrastructure — targets for lexer, parser, lowerer, and
  compiler; seed corpus of 18 `.iris` programs; CI job for continuous fuzzing.
- Benchmark CI — structured result collection, baseline comparison with ≥15%
  regression detection.
- **236 unit tests** for lexer, parser, IR types, IR instructions, pass manager,
  error formatting, diagnostics (byte-to-line-col, render_error, error codes,
  span underlines, colored output, help hints).
- **1050+ integration tests** across 128 test phases covering the full compiler
  pipeline from parsing through codegen and evaluation.
- VS Code extension v0.3.0 — syntax highlighting, snippets, theme,
  configuration, README.
- Built-in function return types registered in lowerer `fn_sigs` for all
  standard builtins (`println`, `sleep_ms`, `random_i64`, `random_f64`, `len`,
  `assert`, `assert_eq`, etc.).

### Improved

- **Error diagnostics** — `render_error` now includes:
  - Error codes (`[E0001]`, `[E0100]`, etc.) in every diagnostic.
  - Full span underlines (`^^^^^^^^`) instead of single-character carets.
  - Optional filename display via `render_error_with_file`.
  - ANSI-colored output via `render_error_colored` / `render_error_colored_with_file`
    with bold-red errors, bold-blue line numbers, bold-green help notes.
  - Contextual help hints for common mistakes (e.g. `@` → "decorators not
    supported", `#` → "use // for comments", `struct` → "use record", `enum`
    → "use choice", `match` → "use when", `import` → "use bring").
  - CLI automatically uses colored output when stderr is a terminal.

---

## [0.2.0] — 2026-03-03

### Added

#### Builtins (60+)

- **HTTP** — `http_get`, `http_post`
- **JSON** — `json_stringify`, `json_parse`
- **Regex** — `regex_match`, `regex_find_all`, `regex_replace`
- **DateTime** — `datetime_now`, `datetime_timestamp`, `datetime_format`
- **OS / System** — `cwd`, `list_dir`, `mkdir`, `remove_file`, `path_join`,
  `env_get`, `env_set`, `exec_cmd`, `pid`, `exit_code`, `type_of`
- **Crypto** — `sha256`, `hash`, `hex_encode`, `hex_decode`, `base64_encode`,
  `base64_decode`
- **Random** — `random`, `random_range`, `uuid`
- **Functional list ops** — `list_map`, `list_filter`, `list_reduce`,
  `list_any`, `list_all`, `list_zip`, `list_enumerate`, `list_flatten`,
  `list_unique`, `list_reverse`, `list_sorted`, `list_sum`, `list_min`,
  `list_max`
- **Collections** — deque, sorted set, bitset, heap, queue
- **String extras** — `str_pad_left`, `str_pad_right`, `str_chars`,
  `str_bytes`, `str_count`, `char_at`
- **Math constants** — `math_pi`, `math_e`, `is_nan`, `is_inf`
- **Concurrency extras** — `chan_try_recv`, `chan_len`, `select`, `timeout`,
  `thread_count`, `atomic_load`, `atomic_store`, `atomic_add`

#### SQLite

- Full-stack database operations: `db_open`, `db_exec`, `db_query`, `db_close`
  — parser, interpreter, LLVM codegen, C runtime (bundled via rusqlite).

#### FFI

- **C FFI** — `ffi_open`, `ffi_call_i64`, `ffi_call_f64`, `ffi_call_str`,
  `ffi_call_void`, `ffi_close`
- **Python FFI** — `python_eval`, `python_exec`, `python_call`,
  `python_version`
- **Rust FFI** — `rust_lib_open`, `rust_call_i64`, `rust_call_f64`,
  `rust_call_void`

#### Standard Library (25 modules)

- `math`, `string`, `fmt`, `fs`, `json`, `csv`, `http`, `time`, `crypto`,
  `ffi`, `os`, `testing`, `log`, `iter`, `set`, `queue`, `heap`, `deque`,
  `kv` (SQLite-backed), `table`, `dataset`, `dataframe`, `path`, `async`,
  `bitset`

#### Package Manager

- `iris pkg init/add/remove/install/build/run/list` — project scaffolding,
  dependency management, registry interaction.

#### LSP Enhancements

- AST-based hover (works even when compilation fails).
- Built-in and keyword hover documentation.
- Code actions: missing-semicolon quickfix, type-mismatch cast, add doc
  comment, rename to snake_case, remove redundant semicolons, wrap in
  if-condition.
- Best-practice diagnostics: BP001–BP006 (long function, missing doc, too many
  params, non-snake_case, empty body, double semicolons).
- Inlay hints, find references, rename support, diagnostic codes.

#### DAP Debugger Enhancements

- Step-back, step-over/into/out.
- Conditional breakpoints, hit counts, log-points.
- Richer stack traces with source info, loaded sources, exception info.
- Variable mutation, restart, pause.
- Debug-console completions, exception breakpoint filters.

#### REPL Enhancements

- Colored prompts, timing display, input history.
- Commands: `:ir`, `:time`, `:history`, `:clear`, `:reset`, `:bring`.

#### Tooling & Infrastructure

- Verbose `iris --version` — GCC-style output with git commit, branch, build
  date, target, host, profile, rustc version.
- Binary output naming — `hello.iris` → `hello.exe` / `./hello`.
- Incremental compilation cache infrastructure.
- Benchmark suite — factorial, fib, list, matrix, sort, string benchmarks.
- ARM64 CI — cross-platform CI on x86_64 + ARM64 (Linux, Windows, macOS).

#### Installers

- **Windows** — portable `.zip`, WiX `.msi`, Inno Setup `.exe` (bundles
  LLVM/clang, lld, MinGW ucrt64 sysroot).
- **Linux** — curl one-liner, `.deb`, `.rpm`, AppImage.
- **macOS** — curl one-liner, `.pkg`, `.dmg`.

#### VS Code Extension 0.2.0

- Status bar with version tooltip, Show Version Info command.
- Server menu (restart/stop LSP).
- LSP best-practice diagnostics and code actions.
- Inlay hint settings, timing on run.
- Updated TextMate grammar for all new builtins and types.
- New snippets for FFI, concurrency, error handling.

### Changed

- **C runtime rewrite** — now uses clang + lld exclusively (removed GCC/MSYS2
  dependency).
- **Build metadata** — `build.rs` captures git hash, branch, dirty flag, build
  date, rustc version, target/host/profile/opt-level.
- Error recovery improvements in parser.

### Changed (license)

- License changed from MIT to GPL-2.0-or-later.

---

## [0.1.0] — 2026-02-28

### Added

#### Core Language

- **Lexer** — tokenizer for `.iris` source files.
- **Parser** — recursive-descent parser producing AST.
- **SSA IR** — block-parameter SSA (MLIR-style), no phi nodes.
- **Lowerer** — AST → IR lowering with lambda-lifting for closures.
- **Pass pipeline** — Validate, TypeInfer, ConstFold, OpExpand, DCE, CSE,
  ShapeCheck, Inline, LoopUnroll, StrengthReduce, Exhaustive, GcAnnotate.
- **Tree-walking interpreter** — `iris run` / `--emit eval`.

#### Type System

- Primitives: `i32`, `i64`, `f32`, `f64`, `bool`, `str`.
- Composite: `tensor<T, shape>`, `list<T>`, `map<K,V>`, tuples, arrays.
- Records (`record`), enums (`choice`) with variant payloads.
- Generics — `def identity[T](x: T) -> T` with monomorphization.
- Traits / Impl — `trait Printable`, `impl Printable for Point`.
- Type aliases — `type Matrix = tensor<f64, [3, 3]>`.
- Function types — `fn(i64) -> i64`.
- `option<T>`, `result<T,E>`, `?` operator.

#### Control Flow & Pattern Matching

- `if/elif/else`, `for`, `while`, `break`, `continue`, `return`.
- `when` (pattern matching) with guards, range patterns, tuple destructuring.

#### Closures & Functions

- Lambda expressions — `|x: i64| x * 2`.
- Default parameters — `def greet(name: str = "world")`.
- Global constants — `const PI: f64 = 3.14`.

#### Concurrency

- `channel<T>`, `spawn`, `par for`.
- `async/await`, `atomic<T>`, `mutex<T>`.

#### ML Features

- Automatic differentiation — `grad<T>` dual numbers, `@differentiable`.
- Sparse tensors — `sparse<T>`, `sparsify`, `densify`.

#### Strings

- F-string interpolation — `f"Hello, {name}!"`.
- Builtins: `len`, `concat`, `split`, `join`, `contains`, `starts_with`,
  `ends_with`, `trim`, `to_upper`, `to_lower`, `repeat`, `find`, `slice`,
  `str_replace`, `str_reverse`.

#### Math Builtins

- `sin`, `cos`, `tan`, `exp`, `log`, `sqrt`, `abs`, `pow`, `min`, `max`,
  `clamp`, `floor`, `ceil`, `round`.

#### I/O

- `print`, `read_line`, `read_i64`, `read_f64`.
- TCP/network instruction lowering (interpreter).

#### Code Generation

- `--emit ir|llvm|eval|binary|onnx|cuda|simd` (stubs for ONNX/CUDA/SIMD).
- LLVM IR codegen — target triples, string globals, 70+ runtime declarations.
- Native binary compilation — `iris build` via clang.

#### Module System

- `bring std.math`, `pub def`, multi-file compilation.

#### FFI

- `extern def` for C function declarations.
- GC refcounting basics.

#### Tooling

- **REPL** — `:help`, `:env`, `:type`, `:quit`.
- **LSP** — hover, completions, diagnostics, go-to-definition, document
  symbols, signature help, formatting.
- **DAP** — breakpoints, step, variables, evaluate.
- **CLI** — `iris run`, `iris build`, `iris repl`, `iris lsp`, `iris dap`.

#### VS Code Extension 0.1.0

- Syntax highlighting (TextMate grammar).
- LSP integration, DAP debugger integration.
- Commands: Run File (Ctrl+F5), Build Binary, Open REPL.
- Snippets for common constructs.

---

[Unreleased]: https://github.com/Moon9t/IRIS/compare/v1.0.0-rc1...HEAD
[1.0.0-rc1]: https://github.com/Moon9t/IRIS/compare/v0.2.0...v1.0.0-rc1
[0.2.0]: https://github.com/Moon9t/IRIS/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Moon9t/IRIS/releases/tag/v0.1.0

