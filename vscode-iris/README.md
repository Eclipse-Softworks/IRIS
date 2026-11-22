![IRIS](icon.png)

# IRIS Language for Visual Studio Code

Full-featured IDE support for the [IRIS programming language](https://github.com/Eclipse-Softworks/IRIS) — syntax highlighting, Language Server Protocol, debugging, REPL, and more.

![VS Code](https://img.shields.io/badge/VS%20Code-1.85+-blue?logo=visualstudiocode)
![License](https://img.shields.io/badge/license-Apache--2.0-blue)
![Extension](https://img.shields.io/badge/extension-1.0.10-orange)
![Compiler](https://img.shields.io/badge/compiler-1.0.0--rc3-purple)

---

## Features

### Syntax Highlighting

Rich TextMate grammar for `.iris` files — preprocessor directives (`#ifdef`/`#define`), comments (`///`, `//`, `/* ... */`), strings, f-strings, char literals, attributes, model DSL blocks, keywords (including logical `and`/`or`/`not`, `mut`, `self`, `Self`), macros (`name!`), all built-in types and compiler builtins, and all 58 embedded stdlib modules (including `std.ui*` native GUI, camera, SQLite, crypto, ROS 2, AIS, and ML modules).

### Language Server Protocol (LSP)

Powered by the IRIS compiler's built-in language server:

- **Hover** — full signatures, generic parameters, docs, declared/inferred effects, and unsafe-region status for functions, traits, effects, fields, and builtins; every keyword and type explains its rules, the idiomatic form, and a runnable example (each example is compiled and run by the compiler's test suite)
- **Completions** — context-aware functions, fields, types, traits, keywords, and every registered standard-library module; keyword, type and builtin items show their documentation
- **Diagnostics** — exact-range compiler errors plus dead/unused, unreachable, unsafe, possible-infinite-loop, and best-practice warnings; unnecessary code is faded by VS Code
- **Go to Definition** / **Peek Definition**
- **Document Symbols** — outline view and breadcrumbs
- **Signature Help** — parameter hints as you type
- **Formatting** — deterministic, comment/literal-preserving, parse-safe format on save with configurable indentation and line width
- **Inlay Hints** — inline inferred binding hints on `val` / `var`
- **Code Actions** — auto-fix missing semicolons, type casts, naming conventions, and more

### Debug Adapter Protocol (DAP)

Step-through debugging with the built-in IRIS debugger:

- Breakpoints (line, conditional, logpoint, and hit-count)
- Step In / Step Over / Step Out / Step Back / Continue / Restart / Pause
- Variables inspector (locals)
- Watch expressions
- Debug Console evaluation and completions
- Named-function launch, used to debug the selected `test_*` function

By default the debugger records and replays interpreter traces.

**Native debugging** (`"native": true` in the launch configuration, the
**IRIS: Debug Native Binary** command, or the `iris.debug.native` setting)
builds the program with DWARF debug information (`iris build -g`, `-O0`) and
runs it under GDB 14+'s DAP mode through the same `iris` adapter:

- Breakpoints on `.iris` lines, including in brought modules
- Step In / Over / Out, Continue, Pause (plus `stepBack` / `reverseContinue` when supported by the native adapter)
- Call stacks with IRIS function names and lines
- Function arguments, `val`/`var` locals, and loop-carried `var`s with DWARF types for scalars, `str`, `record`, `enum`, fixed scalar arrays, `option`, `result`, `tuple`, `list`, `map`, and `ptr` fallback
- Native `attach` (`"request": "attach"` with `"processId"` / `"pid"` or `"target"`, e.g. `"localhost:3333"`) to binaries built with `iris -g build` (or the **iris: build-debug** task)

Point `iris.debug.nativeAdapter` at another adapter (for example `lldb-dap`) to use it instead of GDB.

### Test Explorer

The native VS Code Testing view discovers zero-argument `test_*` functions as
files change. Run or debug one test, a file, or the whole workspace. Test output,
duration, cancellation, and failures are reported through the standard Testing
UI; CodeLens actions use the same compiler and named-entry debugger.

### Commands

| Command | Keybinding | Description |
|---------|------------|-------------|
| **IRIS: Run File** | `Ctrl+F5` | Run the current `.iris` file |
| **IRIS: Build Binary** | — | Compile to a native executable |
| **IRIS: Build GUI Binary** | — | Compile with `--gui` (Windows GUI subsystem, no console window) |
| **IRIS: Bundle Standalone Application...** | — | Package icons, assets, and `iris_ui_host` via `iris bundle` (`dir`/`zip`/`inno`/`app`/`appimage`/`--install`) |
| **IRIS: Debug File** | `F5` | Launch the current `.iris` file under the trace-based DAP debugger (or natively, with `iris.debug.native`) |
| **IRIS: Debug Native Binary** | — | Build with debug info and debug the native executable under GDB |
| **IRIS: Check Project** | — | Run static type, borrow, and effect checks without linking (`iris check`) |
| **IRIS: Watch File...** | — | Re-run, test, or check on every save (`iris watch`) |
| **IRIS: New Project from Template...** | — | Scaffold a new project (`ui`, `app`, `cli`, `server`, `ros2`, `controller`) |
| **IRIS: Run Tests in File** | — | Run every `test_*` function in the file |
| **IRIS: Run All Workspace Tests** | — | Run the workspace test suite |
| **IRIS: Format Workspace** | — | Format workspace `.iris` files |
| **IRIS: Check Formatting** | — | Verify formatting with `iris fmt --check` |
| **IRIS: Run Benchmarks** | — | Run `iris bench` on the active `.iris` file |
| **IRIS: Profile File** | — | Instrument and profile per-function timing (`iris profile`) |
| **IRIS: Generate Documentation** | — | Generate HTML API documentation from `///` comments (`iris docs`) |
| **IRIS: Build Embedded Bundle...** | — | Emit allocation-free bare-metal bundles (`cortex-m4f`, `cortex-m33`, `esp32-c3`, `esp32`, `arduino-uno`) |
| **IRIS: Generate FFI Bindings...** | — | Generate `.iris` bindings from C/C++ `.h`/`.hpp` headers (`iris bindgen`) |
| **IRIS: Package Manager...** | — | Run `iris pkg` commands (`init`, `add`, `remove`, `install`, `update`, `vendor`, `publish`, ...) |
| **IRIS: Run Toolchain Doctor** | — | Diagnose compiler, LLVM-C, linker, debugger, and SDK setup (`iris doctor`) |
| **IRIS: Collect Bug Report** | — | Package minimal compiler diagnostics locally (`iris bug-report`) |
| **IRIS: Open REPL** | — | Launch an interactive IRIS session |
| **IRIS: Restart Language Server** | — | Restart the LSP server |
| **IRIS: Stop Language Server** | — | Stop the LSP server |
| **IRIS: Show IR Output** | — | Display the compiler's SSA IR |
| **IRIS: Show LLVM IR Output** | — | Display the generated LLVM IR |
| **IRIS: Show Graph / CUDA / SIMD / Compiler Output...** | — | Inspect any `--emit` output side-by-side |
| **IRIS: Explain Error Code** | — | Look up `E0xxx` explanations with `iris explain` |
| **IRIS: Show Version Info** | — | Show compiler version, git commit, build date, and target |

### Snippets

Scaffold current language patterns including native desktop UI apps (`uiapp`, `uisvg`, `uicard`, `uimodal`, `uispring`, `uicamera`, `uicanvas`), ownership/borrowing (`move`, `ref`, `refmut`, `derefmut`), const generics (`constgen`), traits and trait objects (`trait`, `traitg`, `impl`, `dyn`), effects/handlers (`handle`, `handleresume`, `pure`, `withpure`), speculation (`speculate`, `tx`), wrapping arithmetic (`wrap`), async/await and structured task groups, differentiable tensors, SQLite/JSON/HTTP (`sql`, `json`, `httpserv`), uncertainty (`uncertainty`), preprocessor (`ifdef`), ROS 2, LLM clients, typed metaprogramming, checked networking, FFI, embedded firmware (`embedded`), GC inspection (`gcstats`), and assertion-backed tests.

### Code Lens & Status Bar

- **Status bar** shows the active IRIS compiler version with a rich tooltip (git commit, branch, build date, target triple, rustc version).

---

## Requirements

- **VS Code** ≥ 1.85
- **IRIS compiler** installed and available on `PATH` (or configure `iris.executablePath`)

Install IRIS from [github.com/Eclipse-Softworks/IRIS/releases](https://github.com/Eclipse-Softworks/IRIS/releases) or build from source:

```bash
git clone https://github.com/Eclipse-Softworks/IRIS.git
cd IRIS
cargo build --release
```

---

## Extension Settings

| Setting | Default | Description |
|---------|---------|-------------|
| `iris.executablePath` | `"iris"` | Path to the `iris` binary |
| `iris.maxNumberOfProblems` | `100` | Maximum diagnostics shown in the Problems panel |
| `iris.formatOnSave` | `false` | Auto-format `.iris` files on save |
| `iris.format.indentSize` | `4` | Formatter indentation width |
| `iris.format.maxLineWidth` | `100` | Preferred line width for safe comma-boundary wrapping |
| `iris.trace.server` | `"off"` | Trace LSP communication (`off` / `messages` / `verbose`) |
| `iris.inlayHints.enabled` | `true` | Enable inlay hints |
| `iris.inlayHints.typeHints` | `true` | Show inferred binding hints on `val` / `var` bindings |
| `iris.debug.stopOnEntry` | `false` | Stop at the first executable trace point when debugging |
| `iris.debug.native` | `false` | Make **IRIS: Debug File** debug the native binary under GDB |
| `iris.debug.nativeAdapter` | `""` | Native DAP adapter command (sets `IRIS_NATIVE_DAP`), e.g. `lldb-dap` |
| `iris.build.debugInfo` | `false` | **IRIS: Build Binary** emits DWARF debug info (`-g`) |
| `iris.showTimingOnRun` | `true` | Show compile + run elapsed time |
| `iris.sandbox` | `false` | Pass `--sandbox` (deny filesystem, network, FFI, process) |
| `iris.noCache` | `false` | Pass `--no-cache` |
| `iris.strictEffects` | `false` | Pass `--strict-effects` |
| `iris.target` | `""` | Target preset/triple for **IRIS: Build Binary** |
| `iris.simd` | `false` | Pass `--simd` (AVX2/NEON vector kernels & loop vectorization) |
| `iris.gpu` | `""` | Pass `--gpu <api>` (`cuda` or empty) |
| `iris.build.gui` | `false` | Pass `--gui` on **IRIS: Build Binary** |
| `iris.bundle.release` | `true` | Prefer release `iris_ui_host` when bundling apps |

---

## Quick Start

1. Install the IRIS compiler and ensure `iris` is on your PATH.
2. Install this extension from the VS Code Marketplace.
3. Open any `.iris` file — the language server starts automatically.
4. Press `Ctrl+F5` to run, or use `F5` to debug.

```iris
// hello.iris
def main() -> i64 {
    print("Hello, IRIS!");
    return 0
}
```

---

## IRIS Language at a Glance

```iris
// Records, generics, and pattern matching
record Point { x: f64, y: f64 }

choice Shape {
    Circle(f64),
    Rect(f64, f64),
}

def area(s: Shape) -> f64 {
    return when s {
        Shape.Circle(r) => 3.14159 * r * r,
        Shape.Rect(w, h) => w * h,
    }
}

def main() -> i64 {
    val c = Shape.Circle(5.0);
    val r = Shape.Rect(3.0, 4.0);
    print(concat("Circle area: ", to_str(area(c))));
    print(concat("Rect area: ", to_str(area(r))));
    return 0
}
```

IRIS 1.0.0-rc2 includes strong static typing, ownership/borrowing, effect checking,
algebraic data types, closures, traits and trait objects, generics, pattern
matching, structured concurrency, reverse-mode autodiff/tensors, native and ORC
JIT execution, metaprogramming, governed evolution, networking, ROS 2, AIS/ML,
multi-module projects, and a broad standard library.

---

## Known Limits

- Native debug sessions need GDB 14+ (tested on Windows with GDB 17.1) or
  another adapter via `iris.debug.nativeAdapter`. Reverse stepping natively
  requires an adapter/target that advertises `supportsStepBack` (e.g. `gdb`
  record mode). On Windows, LLVM `lldb-dap` requires its Python DLL on `PATH`.
- Native toolchain requirements depend on the target. Supported Windows MinGW
  builds link directly with `ld.lld`; consult the repository requirements and
  portability documentation for other targets.
- External ML SDKs, ROS 2 installations, and physical boards require their
  corresponding deployment dependencies and validation.

---

## Contributing

Contributions welcome! See [CONTRIBUTING.md](../docs/CONTRIBUTING.md).

---

## License

[Apache-2.0](../LICENSE)
