# IRIS Language for Visual Studio Code

Full-featured IDE support for the [IRIS programming language](https://github.com/moon9t/iris) — syntax highlighting, Language Server Protocol, debugging, REPL, and more.

![VS Code](https://img.shields.io/badge/VS%20Code-1.85+-blue?logo=visualstudiocode)
![License](https://img.shields.io/badge/license-GPL--2.0--or--later-green)
![Extension](https://img.shields.io/badge/extension-1.0.5-orange)
![Compiler](https://img.shields.io/badge/compiler-1.0.0--rc1-purple)

---

## Features

### Syntax Highlighting

Rich TextMate grammar for `.iris` files — keywords, types, strings, f-strings, comments, operators, and builtins are all highlighted accurately.

### Language Server Protocol (LSP)

Powered by the IRIS compiler's built-in language server:

- **Hover** — full signatures, generic parameters, docs, declared/inferred effects, and unsafe-region status for functions, traits, effects, fields, and builtins
- **Completions** — context-aware functions, fields, types, traits, keywords, and every registered standard-library module
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

The debugger records and replays interpreter traces. It does not currently
attach to a native executable or expose machine instructions/registers.

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
| **IRIS: Debug File** | `F5` | Launch the current `.iris` file under the trace-based DAP debugger |
| **IRIS: Run Tests in File** | — | Run every `test_*` function in the file |
| **IRIS: Run All Workspace Tests** | — | Run the workspace test suite |
| **IRIS: Format Workspace** | — | Format workspace `.iris` files |
| **IRIS: Open REPL** | — | Launch an interactive IRIS session |
| **IRIS: Restart Language Server** | — | Restart the LSP server |
| **IRIS: Stop Language Server** | — | Stop the LSP server |
| **IRIS: Show IR Output** | — | Display the compiler's SSA IR |
| **IRIS: Show LLVM IR Output** | — | Display the generated LLVM IR |
| **IRIS: Show Version Info** | — | Show compiler version, git commit, build date, and target |

### Snippets

Scaffold current language patterns including ownership/borrowing, traits and
trait objects, effects/handlers, true async and structured task groups,
differentiable tensors, AIS, ROS 2, LLM clients, typed metaprogramming, checked
networking, FFI, and assertion-backed tests.

### Code Lens & Status Bar

- **Status bar** shows the active IRIS compiler version with a rich tooltip (git commit, branch, build date, target triple, rustc version).

---

## Requirements

- **VS Code** ≥ 1.85
- **IRIS compiler** installed and available on `PATH` (or configure `iris.executablePath`)

Install IRIS from [github.com/moon9t/iris/releases](https://github.com/moon9t/iris/releases) or build from source:

```bash
git clone https://github.com/moon9t/iris.git
cd iris
cargo build --release
```

---

## Extension Settings

| Setting | Default | Description |
|---------|---------|-------------|
| `iris.executablePath` | `"iris"` | Path to the `iris` binary |
| `iris.maxNumberOfProblems` | `100` | Maximum diagnostics shown in the Problems panel |
| `iris.formatOnSave` | `true` | Auto-format `.iris` files on save |
| `iris.format.indentSize` | `4` | Formatter indentation width |
| `iris.format.maxLineWidth` | `100` | Preferred line width for safe comma-boundary wrapping |
| `iris.trace.server` | `"off"` | Trace LSP communication (`off` / `messages` / `verbose`) |
| `iris.inlayHints.enabled` | `true` | Enable inlay hints |
| `iris.inlayHints.typeHints` | `true` | Show inferred binding hints on `val` / `var` bindings |
| `iris.debug.stopOnEntry` | `false` | Stop at the first executable trace point when debugging |
| `iris.showTimingOnRun` | `true` | Show compile + run elapsed time |

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
    when s {
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

IRIS RC1 includes strong static typing, ownership/borrowing, effect checking,
algebraic data types, closures, traits and trait objects, generics, pattern
matching, structured concurrency, reverse-mode autodiff/tensors, native and ORC
JIT execution, metaprogramming, governed evolution, networking, ROS 2, AIS/ML,
multi-module projects, and a broad standard library.

---

## Known Limits

- DAP is an interpreter-trace debugger, not native attach/debugging.
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

[GPL-2.0-or-later](../LICENSE)
