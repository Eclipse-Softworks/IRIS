# IRIS Language Extension Changelog

## 1.0.8

### New Features

- **Native debugging updates**: added `"request": "attach"` configuration schema, `launch.json` snippet (`IRIS: Attach Native Process`), and `IrisDebugConfigurationProvider` support for `"processId"`/`"pid"` and `"target"`; updated native capability descriptions for rich DWARF local/composite inspection (`str`, `record`, `enum`, arrays, `option`, `result`, `tuple`, `list`, `map`), GDB 17.1 on Windows / LLVM 23, and `supportsStepBack` forwarding.

- **Quick fixes from the compiler**: every fix the compiler attaches to an
  error (did-you-mean, `==` for `=`, `to_str(...)`, a missing field, `var` for
  a reassigned `val`, ...) is offered as a quick fix, the first one preferred;
  fixes that touch several places apply as one edit. All errors are shown --
  every parse error and one per function -- underlined across the whole
  token, with "declared here" locations as related information.
- **Stale compiler warning**: when the compiler in use is older than a build
  in the workspace (`target/debug` or `target/release`), the status bar turns
  yellow and says so, since its diagnostics can disagree with the terminal.
- **Async tasks**: `task` is highlighted as a type, and the task builtins
  (`async_sleep`, `async_yield`, `async_recv`, `async_any`, `task_done`,
  `task_cancel`, `task_cancelled`) as concurrency builtins. Hovers for
  `async`, `await` and `task` describe the compiled state-machine model and
  carry runnable examples; the builtins have signature hovers. New `tasks`
  snippet: start tasks, then await them so they run concurrently.
- **ROS 2 on rclcpp**: new `ros2` (an event-loop node with a timer-driven
  publisher and a subscription), `ros2srv`, `ros2action` and `tflookup`
  snippets for the rclcpp-backed `std.ros2`; the old `ros2` snippet called
  functions that never existed (`rcl_init`) and is replaced. The core
  `std.ros2` functions are highlighted; the removed lifecycle stand-ins
  (`lifecycle_new`, `lifecycle_transition`, `lifecycle_can_publish`) no
  longer are.

### Fixes

- **The language server follows the compiler you build.** In a workspace
  that builds IRIS itself, the newer of `target/release` and `target/debug`
  is used (a week-old release build used to answer for today's debug build,
  so the editor reported errors the compiler no longer made), and the server
  restarts on its own when `cargo build` replaces the binary. The server runs
  from a copy in the temp directory, so the build can replace the original:
  on Windows a running server used to make `cargo build` fail with "Access is
  denied". An explicit `iris.executablePath` is still used as given.

## 1.0.7

### New Features

- **Hover documentation for every keyword and type** (served by the `iris`
  language server): each of the 60 lexer keywords, the scalar types,
  `option`/`result`/`chan`, and contextual words (`handle`, `where`, `self`)
  explains its rules and pitfalls, gives the idiomatic form, and shows a short
  example. Every example is a complete program that the compiler's test suite
  runs, so hovers cannot drift from the language. Keyword, type and builtin
  completions carry the same documentation, and 17 missing keywords (`defer`,
  `yield`, `select`, `unsafe`, `defmacro`, `effect`, `with`, `dyn`, …) are now
  offered. Math builtin hovers show their real, generic signatures (they said
  `f32` only).
- **19 idiom snippets**: `try` (try/catch with `?`), `resultfn`, `iflet`,
  `whenval` (when as a value), `recorddefault`, `ext` (extension method),
  `pure` (effect-pure control function), `withpure`, `fstr`, `gen` (yield
  generator), `constdef`, `type`, `defmacro`, `mod`, `parsum`, `worker`,
  `loopbreak`, `forby`, `unsafe`. Every new or changed snippet compiles when
  expanded with its defaults.
- **Native debugging**: launch configurations accept `"native": true`; the
  new **IRIS: Debug Native Binary** command and the `iris.debug.native`
  setting build the program with DWARF debug info and debug the executable
  under GDB 14+ through `iris dap` — `.iris` line breakpoints (including in
  brought modules), stepping, call stacks and scalar arguments.
  `iris.debug.nativeAdapter` selects another adapter (e.g. `lldb-dap`).
  Requires an `iris` compiler with `-g` support.
- **F5 without launch.json**: a debug configuration provider debugs the active
  `.iris` file using the `iris.debug.*` settings, and offers trace and native
  configurations when creating a `launch.json`.
- **`iris: build-debug` task** and the `iris.build.debugInfo` setting for
  debuggable native binaries; **`iris.strictEffects`** setting passes
  `--strict-effects` to run/build.

### Bug Fixes

- **Run/Build with settings enabled**: `iris.target`, `iris.sandbox` and
  `iris.noCache` were passed *after* the subcommand (`iris build --target …`),
  which the compiler rejects, so **IRIS: Build Binary** and **IRIS: Run File**
  failed whenever one was set. All options now precede the subcommand. (The
  compiler also accepts `--sandbox`, `--no-cache`, `--strict-effects`,
  `--error-format` and `-g` after the subcommand in builds after 1.0.0-rc2.)
- **Stale runs**: Run/Build and CodeLens runs now wait for the file to be
  saved before invoking the compiler, instead of racing the save.
- **Snippets that inserted invalid code**: `select` (now `select! { msg = ch => …, default => … }`),
  `test` (declared `-> bool` but returned `0`; now `-> i64` with `assert`),
  `tx` (`catch` without a binding), and `defer` (now defers an expression).
- `where` is highlighted as a keyword.
- README: `iris.formatOnSave` defaults to `false` (it said `true`); the
  settings table now lists every setting.

## 1.0.6

### Language & Grammar Updates (IRIS 1.0.0-rc2)

- **Transactional Speculative Effects**: Added syntax highlighting and snippets for
  `transaction_begin()`, `transaction_commit()`, `transaction_rollback()`,
  `transaction_depth()`, and `effect transaction`.
- **Autonomy v2 & ROS 2 v2**: Added type and function highlighting for
  `ViabilityBound`, `AutonomyV2Decision`, `ExpectedFreeEnergy`, `ActiveInferenceDecision`,
  `autonomy_v2_step`, `active_inference_select`, `ROS2Node`, `LifecycleNode`, `Transform`,
  `StampedTransform`, `transform_buffer_set`, and `transform_buffer_lookup`.
- **Set Collections & Reflection**: Added syntax highlighting for `set_new`, `set_add`,
  `set_contains`, `set_remove`, `set_to_list`, `reflection_available`, `validate`,
  `eval`, and `eval_i64`.
- **Expanded Snippets**: Added completion snippets for transactional frames (`tx`),
  active inference autonomy (`autonomyv2`), ROS 2 transform buffers (`tfbuffer`),
  and set operations (`setops`).

### Bug Fixes

- **`IRIS: Build Binary` CLI argument ordering**: Fixed issue where `-o` was appended
  after `build`, causing the compiler CLI parser to reject it. `-o` is now passed
  as a global compiler option (`iris -o <output> build <file>`).

## 1.0.5

### New Features

- Native Test Explorer discovery, run, cancellation, output, duration, and
  debug profiles for zero-argument `test_*` functions.
- Named-function DAP launches so Debug Test runs the selected test instead of
  `main`.
- Rich hovers for functions, traits, algebraic effects, fields, and builtins,
  including docs, generics, declared/inferred effects, and unsafe status.
- Dead/unused, unreachable, unsafe, and possible-infinite-loop diagnostics with
  exact ranges, fading tags, and matching semantic-token modifiers.
- Complete standard-library module completions and snippets for structured
  concurrency, tensor autodiff, AIS, LLM integration, typed metaprogramming,
  checked networking, ownership, ROS 2, and assertion-backed tests.

### Formatting

- The compiler formatter now preserves line/doc/nested block comments and
  literal spelling, refuses to rewrite invalid source, is idempotence tested,
  and wraps at safe comma boundaries.
- Added configurable indentation and preferred line width settings.

### Compatibility

- Targets IRIS compiler `1.0.0-rc1`; this extension release does not rename the
  compiler release to RC2.
- Debugging remains interpreter-trace based rather than native-process attach.

## 1.0.4

### New Features

- Added workspace test and formatting commands, per-test CodeLens actions, the
  complete compiler-output picker, toolchain diagnostics, and expanded current
  syntax/stdlib snippets.
- Added editor-title run/debug actions and safer process-based command launch.

## 1.0.3

### Bug Fixes

- **Language server no longer fails with an opaque "exited with code 1".** The
  extension picked its executable by checking only that a file existed, so a
  stale global install at `~/.iris/bin/iris.exe` was chosen even though it could
  not load — an older build still statically imported `onnxruntime.dll`,
  `tensorflow.dll`, `c10.dll` and `torch_cpu.dll`, so Windows failed it at image
  load with `STATUS_DLL_NOT_FOUND` (0xC0000135) before `main` ran. Candidates are
  now probed with `--version` and the loader status is decoded and reported.
- **A workspace build is now preferred** over a global install, so working on the
  compiler uses the binary matching the sources being edited.
- **`$iris` problem matcher was referenced by every task but never declared**, so
  compiler errors from tasks never reached the Problems panel. It is now defined
  against the real diagnostic format (`error[E0005]: …` / ` --> file:line:col`).
- **`TaskGroup.Run` does not exist** in the VS Code API; the run task set its
  group to `undefined`. esbuild does not typecheck, which is how this shipped.
- **Run/build no longer go through a shell**, so paths containing spaces are
  passed verbatim instead of being re-parsed.
- **Settings now take effect.** `iris.executablePath` and the `iris.inlayHints.*`
  flags are only read at server startup, and the change handler was an empty
  stub; both now trigger a restart.

### New Features

- **Syntax highlighting completed against `src/parser/lexer.rs`.** Added the 23
  reserved words the grammar was missing: `match`, `let`, `mod`, `by`, `effect`,
  `handle`, `resume`, `with`, `raise`, `try`, `catch`, `defer`, `select`, `yield`,
  `move`, `unsafe`, `dyn`, `defmacro`. Removed `and`, `or` and `not`, which are
  not IRIS keywords, and `layer`, `input`, `output` and `where`, which are
  contextual identifiers rather than reserved words and so mis-coloured ordinary
  variables. Loop labels are now highlighted in `break`/`continue`.
- **New commands**: Explain Error Code (seeded from the diagnostic under the
  cursor), Check Formatting, Run Tests in File, Run Benchmarks, Generate
  Documentation, Package Manager, Diagnose Toolchain, and Show Compiler Output
  for all eleven text emit kinds (`graph`, `cuda`, `cuda-ptx`, `simd`,
  `llvm-complete`, `onnx`, `tensorrt`, `pgo-*`) — previously only `ir` and `llvm`
  were reachable.
- **New settings**: `iris.sandbox`, `iris.noCache`, `iris.target`.
- **New snippets** for `trait`, `impl`, effect operations, `handle` blocks with
  and without `resume`, `defer`, `select`, labelled loops, and `assert`.

## 1.0.2

### New Features

- **Syntax highlighting**: Added support for Higher-Kinded Types (HKT) generic parameter placeholders `[_]`.
- **Language Specifications**: Highlighting for `where` clauses and angle bracket generic arguments.

## 1.0.1

### Bug Fixes

- **Syntax highlighting**: Fixed `-` arithmetic operator incorrectly matching inside `->` arrow type annotations
- **Syntax highlighting**: Fixed `!` logical-NOT operator incorrectly matching inside `!=` comparison operators
- **Grammar**: Added AIS/MAPE-K stdlib functions (`mapek_agent_init`, `mapek_step`, `mapek_run`, `homeostasis_*`, `active_inf_*`, `epistemic_*`, `safety_check`, etc.)
- **Grammar**: Added concurrency primitives (`atomic_cas`, `mutex_new`, `mutex_lock`, `mutex_unlock`, `par_for`, `par_map`, `par_reduce`)
- **Rebuild**: Clean production build of bundled extension JS

## 1.0.0

### New Features

- **Standard Library Hardening & Standardization** — Integrated actual working code of all stubs/fallbacks across `std.ais` (Agent loops, Q-learning, decision strategies) and `std.rl` (Experience replay buffers using O(1) RingReplayBuffer).
- **Generic SavedModel session runner in TensorFlow C API** — Rewrote TensorFlow dynamic shims to query SavedModel graph operations generically without hardcoded node names.
- **Flawless Ecosystem Integration** — Standardized C runtime, ML compute engines (PyTorch, TensorFlow, ONNX Runtime), SIMD/BLAS optimizations, and 34 standard library modules.
- **Passes All Tests** — Passed all 1,400+ unit and integration tests successfully with zero errors.

## 0.4.0

### New Features

- Bumped extension metadata for the IRIS 0.4.0 language release.
- Kept editor support aligned with the updated examples and ML pipeline docs.

## 0.3.0

### New Features

- **Native binary compilation** — `iris build file.iris -o output` compiles to a standalone executable via LLVM/clang; no external toolchain required when installed via the Windows installer
- **Closure support** — first-class closures with capture, higher-order functions, `list_map`, `list_filter`, `list_reduce` all invoke closures correctly
- **Merge sort and binary search** — standard library additions
- **Expanded fuzz corpus** — arithmetic, closures, concurrency, data structures, edge cases, lists, maps, methods, ML ops, pattern matching, strings, traits
- **Benchmark suite** — `benches/` directory with binary search, Collatz, hashmap, numerical, sieve, and tree benchmarks
- **Security hardening** — input validation and bounds checks in runtime
- **Profiler** — built-in performance profiling (`iris run --profile file.iris`)
- **Expanded stdlib** — ML/NN helpers (`src/stdlib/ml.iris`, `src/stdlib/nn.iris`)
- **Windows installer** — full self-contained installer bundles LLVM/clang + MinGW sysroot + VC++ runtime; installs to PATH automatically

### Improvements

- 1,384 tests passing (249 unit + 1,135 integration)
- Strength reduction and copy propagation passes
- Loop-invariant code motion (LICM) pass
- Improved LLVM IR emission: cleaner phi nodes, no double terminators
- Better diagnostics with span information
- LSP: inlay hints, code actions, and diagnostics improvements

### Bug Fixes

- Fixed phi predecessor handling after `Panic` blocks in LLVM IR (no double terminators at -O2)
- Fixed `for`/`while` body tail expression being silently dropped
- Fixed `spawn { }`, `while { }`, `for { }`, `loop { }` accepting optional trailing `;`
- Fixed `atomic(v)` alias for `atomic_new(v)`
- Fixed `Option`/`Result` unboxing in LLVM codegen

## 0.2.0

### New Features

- **Status bar** now shows real version from `iris --version` with rich tooltip (version, git commit, branch, build date, target, rustc)
- **Show Version Info** command and server-menu action — displays full GCC-style compiler info in the output panel
- **LSP best-practice diagnostics**: BP001 (long function), BP002 (missing doc comment), BP003 (too many params), BP004 (non-snake_case), BP005 (empty body), BP006 (double semicolons)
- **LSP code actions / auto-fix**: missing semicolons, type-mismatch casts, add doc comment, rename to snake_case, remove redundant semicolons, wrap in if-condition
- **C / Python / Rust FFI** builtins: `ffi_call_i64`, `ffi_call_f64`, `ffi_call_str`, `ffi_call_void`, `python_eval`, `python_exec`, `python_call`, `python_version`, `rust_lib_open`, `rust_call_i64`, `rust_call_f64`, `rust_call_void`
- **60+ new builtins** (Phase 105): async/concurrency, deque, sorted collections, bitset, OS/system, crypto/UUID, string extras, math constants, functional list operations
- Binary output now named after the source file (e.g., `hello.iris` → `hello.exe`)
- Verbose `iris --version` output: git commit, branch, build date, target, host, profile, rustc version

### Improvements

- Updated syntax grammar with all Phase 104/105/106 builtins and new types
- New snippets for FFI, error handling, concurrency, and more
- LSP completions and hover docs for all new builtins
- InlayHint and code-lens improvements
- Better error diagnostics from build/run output

### Bug Fixes

- `list_map`, `list_filter`, `list_reduce` now properly invoke closures (were stubs)
- Status bar correctly reads version from the installed iris binary

## 0.1.0

- Initial release
- Syntax highlighting for .iris files
- Language Server Protocol: hover, completions, diagnostics, goto-definition, document symbols, signature help, formatting
- Debug Adapter Protocol: breakpoints, step, variables, evaluate
- Commands: Run File (Ctrl+F5), Build Binary, Open REPL
- Snippets for common patterns
