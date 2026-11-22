---
title: "The IRIS Programming Language"
author: "Moon9t"
rights: "Apache-2.0"
language: "en-US"
documentclass: book
classoption:
    - oneside
toc: true
toc-depth: 1
number-sections: false
geometry: margin=1in
colorlinks: true
linkcolor: blue
urlcolor: blue
toccolor: blue
header-includes:
    - "\\usepackage{fontspec}"
    - "\\usepackage{graphicx}"
    - "\\setmonofont[Scale=0.86]{Cascadia Mono}"
    - "\\graphicspath{{logo/}{../logo/}}"
    - "\\setcounter{secnumdepth}{-2}"
    - "\\IfFileExists{fvextra.sty}{\\usepackage{fvextra}\\DefineVerbatimEnvironment{Highlighting}{Verbatim}{breaklines,commandchars=\\\\\\{\\},fontsize=\\small}}{}"
    - "\\makeatletter"
    - "\\renewcommand{\\maketitle}{\\begin{titlepage}\\centering\\vspace*{1.4cm}\\includegraphics[width=0.30\\textwidth]{iris-logo.png}\\par\\vspace{1.1cm}{\\Huge\\bfseries\\@title\\par}\\vspace{0.5cm}{\\large\\color[HTML]{374151}Intermediate Representation for Intelligent Systems --- Version 1.0.0-rc3\\par}\\vspace{0.9cm}{\\color[HTML]{005EC3}\\rule{0.45\\textwidth}{1.6pt}}\\par\\vspace{1.0cm}{\\Large\\@author\\par}\\vspace{0.35cm}{\\normalsize\\color[HTML]{4B5563}\\@date\\par}\\vfill{\\small\\color[HTML]{6B7280}Complete Language Guide \\& Reference\\par}\\vspace{0.3cm}{\\small\\color[HTML]{6B7280}\\url{https://github.com/Eclipse-Softworks/IRIS}\\par}\\end{titlepage}}"
    - "\\makeatother"
---

# The IRIS Programming Language

**Intermediate Representation for Intelligent Systems — Version `1.0.0-rc3`**

\newpage

## Copyright and License

**The IRIS Programming Language**

Copyright © 2024–2026 Moon9t. All rights reserved.

Licensed under the Apache License, Version 2.0 (the "License"); you may not use this work except in compliance with the License.

You may obtain a copy of the License at <https://www.apache.org/licenses/LICENSE-2.0>.

Unless required by applicable law or agreed to in writing, this documentation is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.

\newpage

## Table of Contents

- [Foreword](#foreword)
- [Part I: Foundations](#part-i-foundations)
  - [Chapter 1: Getting Started](#chapter-1-getting-started)
    - [1.1 What Is IRIS?](#11-what-is-iris)
    - [1.2 Installation and Toolchain Health](#12-installation-and-toolchain-health)
    - [1.3 Hello, World](#13-hello-world)
    - [1.4 Project Templates](#14-project-templates)
    - [1.5 Interactive REPL, Watch Mode, Testing, and IDE Setup](#15-interactive-repl-watch-mode-testing-and-ide-setup)
  - [Chapter 2: Values, Bindings, and Types](#chapter-2-values-bindings-and-types)
    - [2.1 Scalar Types](#21-scalar-types)
    - [2.2 Bindings, Constants, and Type Inference](#22-bindings-constants-and-type-inference)
    - [2.3 Literals, Escapes, F-Strings, and Explicit Casts](#23-literals-escapes-f-strings-and-explicit-casts)
    - [2.4 Checked Integer Arithmetic and Wrapping Operations](#24-checked-integer-arithmetic-and-wrapping-operations)
    - [2.5 Type Aliases](#25-type-aliases)
  - [Chapter 3: Functions and Modules](#chapter-3-functions-and-modules)
    - [3.1 Defining Functions and Explicit Returns](#31-defining-functions-and-explicit-returns)
    - [3.2 Default Parameters, Named Arguments, and Extension Calls](#32-default-parameters-named-arguments-and-extension-calls)
    - [3.3 Recursion and Tail-Call Optimization](#33-recursion-and-tail-call-optimization)
    - [3.4 First-Class Functions and Closures](#34-first-class-functions-and-closures)
    - [3.5 Modules, Visibility, and Transitive Imports](#35-modules-visibility-and-transitive-imports)
    - [3.6 Documentation Comments and HTML Docs](#36-documentation-comments-and-html-docs)
  - [Chapter 4: Control Flow](#chapter-4-control-flow)
    - [4.1 Expression-Oriented Conditionals](#41-expression-oriented-conditionals)
    - [4.2 Loops: Ranges, Collections, Conditionals, and Infinite Loops](#42-loops-ranges-collections-conditionals-and-infinite-loops)
    - [4.3 Breaking, Continuing, and Loop Labels](#43-breaking-continuing-and-loop-labels)
    - [4.4 Logical and Bitwise Operators](#44-logical-and-bitwise-operators)
    - [4.5 Tuple Destructuring and Expression Tails](#45-tuple-destructuring-and-expression-tails)
- [Part II: Data, Abstraction, and Safety](#part-ii-data-abstraction-and-safety)
  - [Chapter 5: Records, Algebraic Data Types, and Collections](#chapter-5-records-algebraic-data-types-and-collections)
    - [5.1 Records and Methods](#51-records-and-methods)
    - [5.2 Algebraic Data Types with Choice Enums](#52-algebraic-data-types-with-choice-enums)
    - [5.3 Tuples and Fixed Arrays](#53-tuples-and-fixed-arrays)
    - [5.4 Dynamic Lists and Maps](#54-dynamic-lists-and-maps)
    - [5.5 Sets, Deques, Heaps, Queues, BitSets, and Buffers](#55-sets-deques-heaps-queues-bitsets-and-buffers)
    - [5.6 Strings, Formatting, Regular Expressions, and DateTime](#56-strings-formatting-regular-expressions-and-datetime)
  - [Chapter 6: Pattern Matching with When](#chapter-6-pattern-matching-with-when)
    - [6.1 Matching Choice Variants](#61-matching-choice-variants)
    - [6.2 Matching Options and Results](#62-matching-options-and-results)
    - [6.3 Literals, Inclusive Ranges, Tuples, and Wildcards](#63-literals-inclusive-ranges-tuples-and-wildcards)
    - [6.4 Match Guards and Value-Producing Arms](#64-match-guards-and-value-producing-arms)
    - [6.5 Exhaustiveness Checking and Practical Pattern Rules](#65-exhaustiveness-checking-and-practical-pattern-rules)
  - [Chapter 7: Generics, Const Generics, and Traits](#chapter-7-generics-const-generics-and-traits)
    - [7.1 Generic Functions and Generic Records](#71-generic-functions-and-generic-records)
    - [7.2 Const Generics and Higher-Kinded Records](#72-const-generics-and-higher-kinded-records)
    - [7.3 Defining and Implementing Traits](#73-defining-and-implementing-traits)
    - [7.4 Trait Bounds and Dynamic Dispatch](#74-trait-bounds-and-dynamic-dispatch)
    - [7.5 Macros and Compile-Time Functions](#75-macros-and-compile-time-functions)
  - [Chapter 8: Error Handling, Ownership, and Memory Management](#chapter-8-error-handling-ownership-and-memory-management)
    - [8.1 Options, Results, and Error Propagation](#81-options-results-and-error-propagation)
    - [8.2 Assertions, Panics, and Runtime Diagnostics](#82-assertions-panics-and-runtime-diagnostics)
    - [8.3 Borrowing, Mutable Write-Back, and Explicit Moves](#83-borrowing-mutable-write-back-and-explicit-moves)
    - [8.4 CFG Non-Lexical Lifetimes and Explicit Drop Scopes](#84-cfg-non-lexical-lifetimes-and-explicit-drop-scopes)
    - [8.5 Two-Tier Memory Reclamation and Frame Pools](#85-two-tier-memory-reclamation-and-frame-pools)
    - [8.6 Inspecting Allocations with GC Statistics](#86-inspecting-allocations-with-gc-statistics)
- [Part III: Effects, Concurrency, and Systems](#part-iii-effects-concurrency-and-systems)
  - [Chapter 9: Static Effects, Handlers, and Speculation](#chapter-9-static-effects-handlers-and-speculation)
    - [9.1 Effect Annotations and Strict Effect Checking](#91-effect-annotations-and-strict-effect-checking)
    - [9.2 Polymorphic Effect Rows](#92-polymorphic-effect-rows)
    - [9.3 Algebraic Effect Handlers](#93-algebraic-effect-handlers)
    - [9.4 Transactional Speculation and Rollback](#94-transactional-speculation-and-rollback)
    - [9.5 Security Policies and Sandboxing](#95-security-policies-and-sandboxing)
  - [Chapter 10: Concurrency, Async/Await, and Parallelism](#chapter-10-concurrency-asyncawait-and-parallelism)
    - [10.1 Typed Channels and Spawned Tasks](#101-typed-channels-and-spawned-tasks)
    - [10.2 Structured Concurrency with Task Groups](#102-structured-concurrency-with-task-groups)
    - [10.3 Async/Await on the Cooperative Executor](#103-asyncawait-on-the-cooperative-executor)
    - [10.4 Data Parallelism with Parallel For Loops, SIMD, and GPU](#104-data-parallelism-with-parallel-for-loops-simd-and-gpu)
    - [10.5 Lock-Free Atomics and Mutexes](#105-lock-free-atomics-and-mutexes)
  - [Chapter 11: Files, Data Formats, SQLite, and Networking](#chapter-11-files-data-formats-sqlite-and-networking)
    - [11.1 Filesystem, Paths, and Operating System Queries](#111-filesystem-paths-and-operating-system-queries)
    - [11.2 JSON, CSV, Tables, and DataFrames](#112-json-csv-tables-and-dataframes)
    - [11.3 Embedded SQLite and Parameterized Queries](#113-embedded-sqlite-and-parameterized-queries)
    - [11.4 Key-Value Storage, Cryptography, and Visualization](#114-key-value-storage-cryptography-and-visualization)
    - [11.5 TCP/UDP, HTTP Client/Server, TLS, and Packet Inspection](#115-tcpudp-http-clientserver-tls-and-packet-inspection)
  - [Chapter 12: Native Compilation, FFI, and Bare-Metal Embedded](#chapter-12-native-compilation-ffi-and-bare-metal-embedded)
    - [12.1 Inside the Compiler Pipeline and SSA IR](#121-inside-the-compiler-pipeline-and-ssa-ir)
    - [12.2 Inspecting IR, LLVM, SIMD, and Profile-Guided Optimization](#122-inspecting-ir-llvm-simd-and-profile-guided-optimization)
    - [12.3 Foreign Function Interface and Bindgen](#123-foreign-function-interface-and-bindgen)
    - [12.4 Freestanding Microcontroller Bundles](#124-freestanding-microcontroller-bundles)
- [Part IV: Differentiable Programming, ML, and Autonomy](#part-iv-differentiable-programming-ml-and-autonomy)
  - [Chapter 13: Automatic Differentiation](#chapter-13-automatic-differentiation)
    - [13.1 Forward-Mode Dual Numbers and Reverse-Mode Tapes](#131-forward-mode-dual-numbers-and-reverse-mode-tapes)
    - [13.2 Reverse-Mode Scalar Autodiff with Tape, Backward, and Grad](#132-reverse-mode-scalar-autodiff-with-tape-backward-and-grad)
    - [13.3 Differentiable Tensors in Standard Tensor](#133-differentiable-tensors-in-standard-tensor)
    - [13.4 Optimization Loops: Gradient Descent and Adam](#134-optimization-loops-gradient-descent-and-adam)
  - [Chapter 14: Tensors, Neural Networks, and Model Export](#chapter-14-tensors-neural-networks-and-model-export)
    - [14.1 Typed Tensors and Compile-Time Shape Checking](#141-typed-tensors-and-compile-time-shape-checking)
    - [14.2 Neural Layers, Classical ML, Datasets, and RL](#142-neural-layers-classical-ml-datasets-and-rl)
    - [14.3 The Declarative Model DSL and Graph Fusion](#143-the-declarative-model-dsl-and-graph-fusion)
    - [14.4 ONNX, TensorRT Export, and Dynamic ML Backends](#144-onnx-tensorrt-export-and-dynamic-ml-backends)
    - [14.5 LLM Tool Calling and Structured Agents](#145-llm-tool-calling-and-structured-agents)
  - [Chapter 15: Autonomous Intelligent Systems, Robotics, and Governed Evolution](#chapter-15-autonomous-intelligent-systems-robotics-and-governed-evolution)
    - [15.1 Active Inference, Uncertainty, Adaptive Control, and Lifecycle](#151-active-inference-uncertainty-adaptive-control-and-lifecycle)
    - [15.2 ROS 2 Robotics Integration](#152-ros-2-robotics-integration)
    - [15.3 Typed Metaprogramming and Reflection](#153-typed-metaprogramming-and-reflection)
    - [15.4 Seven-Gate Governed Self-Evolution](#154-seven-gate-governed-self-evolution)
- [Part V: Native Desktop Applications](#part-v-native-desktop-applications)
  - [Chapter 16: Building Native Desktop UIs with Standard UI](#chapter-16-building-native-desktop-uis-with-standard-ui)
    - [16.1 Architecture: IRIS Immediate-Mode Trees and Retained Native Host](#161-architecture-iris-immediate-mode-trees-and-retained-native-host)
    - [16.2 Scaffolding a Desktop App and Running the Frame Loop](#162-scaffolding-a-desktop-app-and-running-the-frame-loop)
    - [16.3 Responsive Layout Containers and Breakpoints](#163-responsive-layout-containers-and-breakpoints)
    - [16.4 Built-In Themes, Vector SVG Icons, and Core Widgets](#164-built-in-themes-vector-svg-icons-and-core-widgets)
    - [16.5 Interactive State, Forms, Modals, Drawers, and Toasts](#165-interactive-state-forms-modals-drawers-and-toasts)
    - [16.6 Smooth Springs, Animations, and Frame-Pool Memory Safety](#166-smooth-springs-animations-and-frame-pool-memory-safety)
  - [Chapter 17: Custom Vector Painting, Live Camera Capture, and Standalone Packaging](#chapter-17-custom-vector-painting-live-camera-capture-and-standalone-packaging)
    - [17.1 Custom Vector Canvas Painting with DrawBuffer](#171-custom-vector-canvas-painting-with-drawbuffer)
    - [17.2 Live Native Camera Capture and Synthetic Sensor Simulation](#172-live-native-camera-capture-and-synthetic-sensor-simulation)
    - [17.3 Engineering High-Frame-Rate Native Interfaces](#173-engineering-high-frame-rate-native-interfaces)
    - [17.4 Desktop Manifest Configuration and GUI Builds](#174-desktop-manifest-configuration-and-gui-builds)
    - [17.5 Bundling, Installers, and Operating System Registration](#175-bundling-installers-and-operating-system-registration)
- [Part VI: Engineering in IRIS](#part-vi-engineering-in-iris)
  - [Chapter 18: Packages, Testing, Benchmarking, and Debugging](#chapter-18-packages-testing-benchmarking-and-debugging)
    - [18.1 Manifests, Lockfiles, and the Package Manager](#181-manifests-lockfiles-and-the-package-manager)
    - [18.2 Assertion-Backed Testing](#182-assertion-backed-testing)
    - [18.3 Benchmarking, Profiling, and Optimization](#183-benchmarking-profiling-and-optimization)
    - [18.4 Formatting, Diagnostics, JSON Output, and Bug Reports](#184-formatting-diagnostics-json-output-and-bug-reports)
    - [18.5 Dual-Mode Debugging: Time-Travel Interpreter and Native DWARF](#185-dual-mode-debugging-time-travel-interpreter-and-native-dwarf)
  - [Chapter 19: Architecture Walkthroughs of Real IRIS Applications](#chapter-19-architecture-walkthroughs-of-real-iris-applications)
    - [19.1 Checked Ledger and Command-Line Tools](#191-checked-ledger-and-command-line-tools)
    - [19.2 Concurrent Gateways and Self-Upgrading Services](#192-concurrent-gateways-and-self-upgrading-services)
    - [19.3 Differentiable Orbital Simulation: IRIS GravForge](#193-differentiable-orbital-simulation-iris-gravforge)
    - [19.4 Live Camera Vision and HUD: Aegis Target Takedown](#194-live-camera-vision-and-hud-aegis-target-takedown)
    - [19.5 Navigating the Companion Examples and Projects](#195-navigating-the-companion-examples-and-projects)
- [Appendices](#appendices)
  - [Appendix A: Complete Language Grammar and Reserved Words](#appendix-a-complete-language-grammar-and-reserved-words)
  - [Appendix B: Built-in Functions and Standard Library Map](#appendix-b-built-in-functions-and-standard-library-map)
  - [Appendix C: Type System and Operator Precedence Reference](#appendix-c-type-system-and-operator-precedence-reference)
  - [Appendix D: Complete CLI Reference](#appendix-d-complete-cli-reference)
  - [Appendix E: Compiler Diagnostic and Warning Reference](#appendix-e-compiler-diagnostic-and-warning-reference)

---

## Foreword

Modern intelligent software rarely lives inside a single tier of abstractions. A single system routinely combines numerical optimization or automatic differentiation, asynchronous networking, SQLite persistence, hardware sensor loops or live camera feeds, a responsive native graphical interface, and strict deployment safety guarantees. Historically, teams built each layer in a separate language with fragile glue between them.

**IRIS** (*Intermediate Representation for Intelligent Systems*) unifies those layers behind one statically typed language, one block-parameter SSA intermediate representation, one deterministic optimization pipeline, and one unified toolchain:

- **Native execution and JIT**: compiles through LLVM (`clang` / `ld.lld` and LLVM-C/ORC) to standalone machine-code executables, shared libraries, and desktop applications.
- **Bounded interpreter and time-travel debugging**: executes the same checked SSA IR directly for rapid prototyping, trace-based step-back debugging (`iris dap`), and safe host-governed candidate evaluation.
- **Embedded C-free cross-compilation**: lowers a verified allocation-free scalar subset directly to `.o` object bundles for `cortex-m4f`, `cortex-m33`, `esp32-c3`, `esp32`, and `arduino-uno`.
- **Differentiable computing and autonomy**: provides language-level reverse-mode autodiff (`tape`, `backward`, `grad`), differentiable tensors, a declarative `model` DSL with ONNX/TensorRT export, ROS 2 robotics integration, and 7-gate governed self-evolution.
- **Native desktop applications**: ships an embedded CPU-rendered cross-platform UI stack (`std.ui*` paired with `iris_ui_host`) with responsive flexbox/grid layout, 6 built-in themes, 99 vector SVG icons, spring animations, overlays, custom vector painting, live native camera capture, and single-command application bundling (`iris bundle`).

### Who This Book Is For

This book is written for programmers comfortable with general concepts in at least one systems, scientific, or application language (such as Rust, Python, C, C++, Go, or TypeScript). You do not need prior compiler or machine-learning experience.

- **If you are new to IRIS**, read **Part I** and **Part II** in order (Chapters 1–8). They establish the type system, algebraic data modeling, pattern matching, traits, error handling, ownership, and memory discipline.
- **If you are building services, embedded firmware, or concurrent pipelines**, read **Part III** (Chapters 9–12).
- **If you are working on optimization, neural models, robotics, or live evolution**, read **Part IV** (Chapters 13–15).
- **If you are building standalone desktop GUI applications or vision tools**, read **Part V** (Chapters 16–17).
- **For day-to-day workflows and real system designs**, use **Part VI** (Chapters 18–19) and **Appendices A–E** as your workbench reference.

Throughout this book, maturity boundaries and hardware-specific requirements are stated explicitly inline so you always know what runs identically across platforms, what uses host-specific OS integrations, and where physical hardware validation or optional third-party SDKs apply.

---

# Part I: Foundations

## Chapter 1: Getting Started

### 1.1 What Is IRIS?

IRIS is a statically typed, compiled programming language whose front end, IR optimizer, native LLVM backend, package manager, formatter, documentation generator, LSP language server, DAP debugger, and application bundler all live inside one `iris` executable.

Every `.iris` program travels through the same front-end and mid-level IR pipeline before branching to a target backend:

```text
Source (.iris) + brought std.* / local modules
       │
       ▼  Lexer ─► Parser ─► AST Lowerer ─► Type & Effect Checks
       │
       ▼  20-Pass SSA Optimizer
          (CFG NLL Borrow Check, Drop Elaboration, Shape Check,
           Inline, ConstFold, TCO, ML Fusion, GC & Frame-Pool Scope, Async)
       │
       ├──► Native AOT Executable / Library   (iris build / iris run / iris bundle)
       ├──► In-Process LLVM ORC JIT           (--emit jit / governed hot-swap)
       ├──► Bounded SSA IR Interpreter        (--emit eval / iris dap / tests)
       ├──► Bare-Metal Microcontroller Bundle (iris embedded --target ...)
       └──► Graph / Model Exports             (--emit onnx, onnx-binary, tensorrt, cuda-ptx)
```

Because both native codegen and the SSA interpreter consume the post-lowered, pass-verified IR, compile-time type errors, borrow-checker rejections (`E0382`), checked arithmetic traps (`E0409`), and out-of-bounds array diagnostics (`E0403`) behave consistently across execution modes.

---

### 1.2 Installation and Toolchain Health

#### Building from Source

To build IRIS from the Eclipse Softworks repository with Cargo (requires Rust 1.75+):

```powershell
git clone https://github.com/Eclipse-Softworks/IRIS.git
cd IRIS
cargo build --release --locked
```

On Windows, this produces `target\release\iris.exe` (and on Linux/macOS, `target/release/iris`). Place the binary in a stable directory on your `PATH` (for example `$env:USERPROFILE\.iris\bin\iris.exe` on Windows, or `~/.local/bin/iris` on Linux/macOS).

If you intend to build standalone desktop GUI applications (`std.ui*`), also compile the native UI host library in the same workspace:

```powershell
cargo build --release -p iris_ui_host
```

#### Verifying Prerequisites with `iris doctor`

IRIS parsing, `iris check`, `iris fmt`, `iris repl`, `iris lsp`, `iris pkg`, and interpreter execution are completely self-contained. Native builds (`iris build`, `iris run`, `iris bundle`, `--emit jit`) additionally require:

- A dynamically loadable **LLVM-C** shared library (release bundles ship LLVM **23.1.1**).
- A linker (`lld` / `ld.lld`, or system `clang` fallback).
- Target C system libraries (MinGW **UCRT64** sysroot on Windows, `glibc` + `libm`/`pthreads` on Linux, Xcode Command Line Tools / Apple SDK on macOS).

Run the built-in health check right after installation:

```text
iris --version
iris doctor
```

`iris doctor` compiles and runs a test program natively and in the interpreter, verifies LLVM-C, the linker, debugger adapters, and optional ROS 2 / toolchain discovery, and prints exact remediation commands if any dependency is missing. For automated toolchain provisioning on supported environments, run `iris setup`.

> **Environment Note on `--emit eval` vs `iris run`**:
> `iris run file.iris` compiles to a native binary and executes it. `iris file.iris --emit eval` runs natively when available unless `IRIS_FORCE_INTERP=1` is set (or `--emit eval` triggers explicit fallback under `IRIS_INTERP_FALLBACK=1`).

---

### 1.3 Hello, World

Create a file named `hello.iris`:

```iris
/// Entry point returning 0 on success.
def main() -> i64 {
    val language = "IRIS"
    val version = 1
    println(f"Hello from {language} v{version}!")
    assert(version == 1)
    return 0
}
```

Compile and run it natively:

```text
iris run hello.iris
```

Output:

```text
Hello from IRIS v1!
```

Or compile a standalone executable explicitly:

```text
iris build hello.iris -o hello
./hello        # or .\hello.exe on Windows
```

Three conventions appear immediately in that six-line program:

1. Programs start at `def main() -> i64` and return an exit status (`0` indicates success).
2. Immutable local bindings use `val`; the compiler infers `str` and `i64`.
3. Public and top-level functions end control paths with explicit `return`. Semicolons terminate statements when multiple statements share a block, though newline-separated single statements in clean blocks also parse cleanly across modern IRIS code. Inside book examples we consistently use clear `val`/`var` declarations and explicit `return` statements.

---

### 1.4 Project Templates

For anything beyond a single script, scaffold a project using `iris new [TEMPLATE] NAME`:

```text
iris new app my_service
cd my_service
iris run main.iris
iris test
```

IRIS ships six built-in templates:

| Template | Command | Purpose |
|---|---|---|
| `app` | `iris new app hello` | General executable project with `iris.toml`, `main.iris`, and `test_*` assertions |
| `cli` | `iris new cli mytool` | Command-line tool reading `args()` and formatted stdout/stderr |
| `server` | `iris new server api` | HTTP/TCP service skeleton built on `std.http_server` and `std.net` |
| `ros2` | `iris new ros2 rover` | ROS 2 node project importing `std.ros2` |
| `controller` | `iris new controller pid` | Allocation-free control loop structured for embedded / evolution targets |
| `ui` | `iris new ui studio` | Native desktop application with `[app]` metadata, `std.ui` loop, and bundling setup |

Every template includes a working `iris.toml` manifest and passing starter tests.

---

### 1.5 Interactive REPL, Watch Mode, Testing, and IDE Setup

A typical IRIS inner development loop relies on four fast commands:

```text
iris repl                   # Interactive expression evaluator (supports `bring std.math`, `:help`, `:quit`)
iris check                  # Fast static check across all .iris files in the workspace (types, borrows, effects)
iris watch main.iris        # Re-run on every save (use `--test` or `--check` flags)
iris test                   # Run every zero-argument `test_*` function in the workspace
```

#### VS Code and Editor Support

The official VS Code extension (`vscode-iris/`) communicates with the built-in compiler servers:

- **Language Server (`iris lsp`)**: real-time diagnostics (powered by incremental `QueryEngine` caching), hover types/docstrings/effects, rich completions (including all embedded `std.*` modules), go-to-definition, find-references, rename, semantic tokens, folding ranges, inlay hints, code actions, and document formatting via `iris fmt`.
- **Debug Adapter (`iris dap`)**: supports both trace-based interpreter debugging (including time-travel `stepBack` and `reverseContinue`) and native DWARF debugging (`gdb -i dap` / `lldb-dap`).

Set `iris.executablePath` in VS Code to a stable installed copy of `iris` (avoid pointing an active LSP server directly at a Cargo build directory you intend to overwrite during `cargo build`).

#### Common Mistakes

- **Omitting `-> i64` or `return 0` on `main()`**: IRIS entry points must declare `-> i64` and explicitly return an integer exit code.
- **Using `print` vs `println`**: `print(msg)` writes without a trailing newline; `println(msg)` writes a complete line. Passing non-strings without `to_str(x)` or an f-string fails type checking (`E0110`).
- **Passing multiple files to `iris fmt`**: `iris fmt` formats one specified file at a time, or all `*.iris` files in the current directory when given no filename.

#### Try It Yourself

1. Run `iris doctor` in your terminal and inspect which native compilation and debugger backends are active on your machine.
2. Create a new project with `iris new cli greeter`, edit `main.iris` to read command-line arguments with `args()`, and test it via `iris run main.iris -- Alice`.
3. Launch `iris watch main.iris --test` and verify that saving a broken `assert(...)` immediately reports the failing test line.

---

## Chapter 2: Values, Bindings, and Types

### 2.1 Scalar Types

Every value in IRIS has a statically known type. Scalar primitives cover signed integers, unsigned integers, IEEE 754 floating-point numbers, booleans, and immutable UTF-8 strings:

| Type | Bit Width | Description | Example Literal / Cast |
|---|---|---|---|
| `i64` | 64-bit signed | Default integer type | `42`, `-17`, `1000000` |
| `i32` | 32-bit signed | Compact/FFI signed integer | `42 to i32` |
| `i8` | 8-bit signed | Byte-sized signed integer | `7 to i8` |
| `u8` | 8-bit unsigned | Raw byte / pixel channel | `255 to u8` |
| `u32` | 32-bit unsigned | Packed color / flags / FFI | `1024 to u32` |
| `u64` | 64-bit unsigned | Large unsigned counts / hashes | `99 to u64` |
| `usize` | Pointer width | Unsigned indexing / const sizes | `16 to usize` |
| `f64` | 64-bit float | Default floating-point type | `3.14159`, `1.0e-6` |
| `f32` | 32-bit float | Neural weights / compact buffers | `0.5 to f32` |
| `bool` | 1-bit logical | Boolean truth value | `true`, `false` |
| `str` | UTF-8 sequence | Immutable heap-allocated string | `"IRIS"` |

Unannotated integer literals default to `i64`, and unannotated decimal literals default to `f64`. IRIS performs **no implicit numeric conversions**: adding an `i64` and an `f64` is a compile-time type error (`E0101`). Always make precision and signedness boundaries explicit.

---

### 2.2 Bindings, Constants, and Type Inference

IRIS separates immutable value bindings (`val`), mutable variables (`var`), and module-level constants (`const`):

```iris
pub const MAX_RETRIES: i64 = 5
pub const PI_APPROX: f64 = 3.141592653589793

def main() -> i64 {
    val threshold = 42          // immutable, inferred i64
    var attempts: i64 = 0       // mutable, explicit i64

    while attempts < MAX_RETRIES {
        attempts = attempts + 1
    }

    val ok_flag = attempts == MAX_RETRIES && threshold > 0
    assert(ok_flag)
    assert(PI_APPROX > 3.14)
    println("bindings and constants: ok")
    return 0
}
```

Use **`val` by default** and reserve `var` for accumulators, loop counters, and genuinely re-assigned local state. Attempting to reassign a `val` raises compile error `E0112` (*Assignment to an Immutable Binding*).

> **When Type Annotations Are Required**:
> Type inference works locally from initializer expressions, literal types, and function parameter signatures. However, two situations always require explicit annotations:
>
> 1. **All function parameters and return types** (IRIS requires explicit function boundaries).
> 2. **Generic constructors whose type parameter appears only in the return type**, such as empty collections:
>    `val scores: map<str, i64> = map()`, `val items: list<i64> = list()`, `val s: Set<str> = set_new()`.

---

### 2.3 Literals, Escapes, F-Strings, and Explicit Casts

#### Numeric and Scientific Notation

Floating-point literals in IRIS **must contain a decimal point**—including scientific notation! Write `1.0e-6`, `2.5E3`, or `0.000001`, rather than `1e-6` (which parses as an invalid token sequence). Underscore digit separators are not part of scalar literal syntax; write plain digits such as `1000000`.

#### String Escapes and F-Strings

Double-quoted string literals support five escape sequences:

| Escape | Meaning |
|---|---|
| `\n` | Newline (LF) |
| `\t` | Horizontal tab |
| `\r` | Carriage return |
| `\\` | Literal backslash |
| `\"` | Literal double quote |

No other escape codes (such as `\'` or `\0`) are accepted inside string literals; an unrecognized escape triggers compile error `E0003`.

String interpolation uses `f"..."` strings. At compile time, `{ident}` lowers to `to_str(ident)` concatenated cleanly with surrounding segments:

```iris
def main() -> i64 {
    val label = "orbit"
    val step = 12
    val msg = f"[{label}] step={step}\nnext=\"ready\""
    assert(contains(msg, "[orbit] step=12"))
    println(msg)
    return 0
}
```

> **F-String Rule**: `{...}` placeholders accept **simple identifiers only**. Expressions with dots or calls such as `f"{point.x}"` or `f"{len(xs)}"` are rejected by the lexer/parser. Extract them into a local `val` first:
>
> ```iris
> val px = point.x
> val text = f"x = {px}"
> ```

#### Explicit Type Conversions (`to`, `to_i64`, `to_f64`, `to_str`)

Convert between numeric widths with `expr to target_type`, or via the core conversion builtins `to_i64(x)`, `to_f64(x)`, and `to_str(x)`:

```iris
def main() -> i64 {
    val count: i64 = 7
    val total: f64 = 100.0
    val ratio = total / to_f64(count)
    val byte_val = 200 to u8
    val back_i64 = byte_val to i64
    val half_i32 = (count to i32) + (3 to i32)

    assert(ratio > 14.28 && ratio < 14.29)
    assert(back_i64 == 200)
    assert((half_i32 to i64) == 10)
    println("casts: ok")
    return 0
}
```

---

### 2.4 Checked Integer Arithmetic and Wrapping Operations

In IRIS, `+`, `-`, `*`, and `/` on `i64` and `i32` are **checked at runtime** on both native LLVM builds and the interpreter. If an operation overflows or underflows its signed range (including `i64::MIN / -1`), execution halts cleanly with diagnostic `E0409` naming the exact operation and operands instead of silently wrapping around. Division or remainder by zero halts with `E0402`.

Constant folding obeys the exact same rule: expressions that would overflow are **not** silently folded to wrapped values.

When wrap-around two's-complement arithmetic is genuinely desired—such as hash mixing, linear congruential PRNGs, or cyclic counters—use the explicit wrapping intrinsics `wrapping_add(a, b)`, `wrapping_sub(a, b)`, and `wrapping_mul(a, b)`:

```iris
def hash_mix(state: i64, value: i64) -> i64 {
    val stepped = wrapping_mul(state, 6364136223846793005)
    return wrapping_add(stepped, value)
}

def main() -> i64 {
    val near_max = 9223372036854775800
    // Regular addition would trap with E0409 if it exceeded i64 max;
    // wrapping_add wraps cleanly in two's complement:
    val wrapped = wrapping_add(near_max, 100)
    val mixed = hash_mix(17, 42)
    assert(wrapped < 0)
    assert(mixed != 0)
    println("wrapping arithmetic: ok")
    return 0
}
```

---

### 2.5 Type Aliases

Use `type Name = ExistingType` to give domain meaning to function pointers, matrices, keys, or compound tuples without runtime overhead:

```iris
pub type NodeId = i64
pub type Weight = f64
pub type Predicate = (i64) -> bool

def count_matching(items: list<NodeId>, pred: Predicate) -> i64 {
    var hits = 0
    for id in items {
        if pred(id) {
            hits = hits + 1
        }
    }
    return hits
}

def main() -> i64 {
    val ids: list<NodeId> = list(1, 4, 7, 10, 13)
    val is_even: Predicate = |n: NodeId| n % 2 == 0
    val threshold: Weight = 2.0
    assert(count_matching(ids, is_even) == 2)
    assert(threshold > 1.0)
    println("type aliases: ok")
    return 0
}
```

#### Common Mistakes

- **Writing `1e-6` without a decimal point**: write `1.0e-6`.
- **Interpolating expressions inside f-strings**: bind `val field = obj.field` before `f"{field}"`.
- **Mixing `i64` and `f64` in arithmetic or comparisons**: convert one side explicitly with `to_f64(n)` or `to_i64(x)`.
- **Using `\'` in string literals**: single quotes need no escape inside `"..."`; unsupported escapes fail with `E0003`.

#### Try It Yourself

1. Write a function `celsius_to_fahrenheit(c: f64) -> f64` and verify `0.0 -> 32.0` and `100.0 -> 212.0` with `assert`.
2. Write a tiny PRNG function `next_rand(seed: i64) -> i64` using `wrapping_mul` and `wrapping_add` that executes safely for 1,000 iterations without overflowing.
3. Experiment with forcing an intentional overflow in a small scratch file (`val x = 9223372036854775807; println(to_str(x + 1))`) and inspect the compiler's `E0409` message and `iris explain E0409`.

---

## Chapter 3: Functions and Modules

### 3.1 Defining Functions and Explicit Returns

Functions are declared with `def` (or `pub def` to export from a module). Every parameter declares `name: type`, followed by `-> ReturnType` before the function body:

```iris
def clamp_score(raw: i64, low: i64, high: i64) -> i64 {
    if raw < low {
        return low
    }
    if raw > high {
        return high
    }
    return raw
}

def main() -> i64 {
    assert(clamp_score(-5, 0, 100) == 0)
    assert(clamp_score(150, 0, 100) == 100)
    assert(clamp_score(74, 0, 100) == 74)
    println("functions: ok")
    return 0
}
```

Named functions terminate control-flow paths with explicit `return` statements (`return` without a value exits `unit`-returning procedures). Inside expression blocks (`if`/`else`, `when`, and lambdas), tail expressions without `return` remain valid and produce the block's value.

---

### 3.2 Default Parameters, Named Arguments, and Extension Calls

IRIS supports **default parameter values** and **named arguments** on user-defined functions, making multi-option APIs readable without builder boilerplate. In addition, any function whose first parameter has type `T` can be called with **UFCS dot notation** (`x.func(...)`):

```iris
def scale_and_bias(value: f64, gain: f64 = 1.0, bias: f64 = 0.0) -> f64 {
    return value * gain + bias
}

def difference(left: i64, right: i64) -> i64 {
    return left - right
}

def doubled(n: i64) -> i64 {
    return n * 2
}

def main() -> i64 {
    assert(scale_and_bias(5.0) == 5.0)
    assert(scale_and_bias(5.0, gain = 2.0, bias = 3.0) == 13.0)
    assert(difference(right = 3, left = 10) == 7)
    assert(21.doubled() == 42)
    println("named args and ufcs: ok")
    return 0
}
```

Named arguments are reordered cleanly at compile time to match the callee's signature, and missing trailing/gapped arguments with defaults are filled automatically. Builtins and `extern def` declarations do not expose parameter names, so pass their arguments positionally.

---

### 3.3 Recursion and Tail-Call Optimization

IRIS implements **IR-level tail-call optimization (`TailCallPass`)** inside the standard compiler pipeline, guaranteeing constant stack space on both the native LLVM backend and the SSA interpreter for self-recursive tail calls:

- **Self-recursion in tail expression form** lowers directly into a branch back to a fresh loop header inside SSA IR—before loop-invariant code motion (`LicmPass`), so both the native backend and the interpreter run deep self-recursion in constant stack space.
- **Limitation**: `TailCallPass` rewrites *self-recursion* whose result returns directly (such as `if cond { base } else { self_call(...) }`). Mutually recursive cycles between distinct functions are left as normal calls.

```iris
def sum_tail(n: i64, acc: i64) -> i64 {
    if n <= 0 {
        acc
    } else {
        sum_tail(n - 1, acc + n)
    }
}

def main() -> i64 {
    val total = sum_tail(50000, 0)
    assert(total == 1250025000)
    println("tail call optimization: ok")
    return 0
}
```

Running 50,000 recursive calls in `sum_tail` runs in constant stack frames on both native binaries and the interpreter without tripping the default call-depth guard (`--max-depth 250`).

---

### 3.4 First-Class Functions and Closures

Functions can be passed by name or constructed inline as **closures** with `|params| expr` or `|params| { ... }`. Closures capture outer immutable values cleanly:

```iris
def apply_twice(value: i64, f: (i64) -> i64) -> i64 {
    return f(f(value))
}

def main() -> i64 {
    val step = 5
    val advance = |x: i64| x + step
    val square = |x: i64| x * x

    assert(apply_twice(10, advance) == 20)
    assert(apply_twice(3, square) == 81)
    println("closures: ok")
    return 0
}
```

For closures whose bodies allocate, perform I/O, or accept effect-polymorphic callbacks under `--strict-effects`, use pipe-arrow syntax with an optional effect clause: `|x: i64| -> i64 effect alloc`.

---

### 3.5 Modules, Visibility, and Transitive Imports

IRIS organizes code across files and embedded standard libraries via `bring`:

```iris
bring std.math              // Embedded standard-library module
bring std.json              // Another embedded module
bring "physics.iris"        // Local sibling module relative to this file
```

Inside a library module (for instance `math_helpers.iris`), **definitions are private by default**. Only declarations marked `pub` are exported to importers:

- `pub def ...` exports a function.
- `pub record ...` and `pub choice ...` export structs and algebraic enums.
- `pub const ...` and `pub type ...` export constants and type aliases.
- `pub bring "submod.iris"` (or `pub bring std.ui_theme`) **re-exports** a brought module's public functions, records, choices, and type aliases to downstream files—just like `std.ui` re-exports `std.ui_theme`, `std.ui_protocol`, `std.ui_motion`, `std.ui_icons`, `std.ui_layout`, and `std.ui_widgets` so callers only need `bring std.ui`.

Attempting to call a private helper in another file fails at compile time with `E0100`, naming the module and instructing you to add `pub`.

> **Cross-Module Trait Limit**:
> Co-locate `impl Trait for Record` blocks in the same module as the defining `record` (and mark helper methods carefully), since traits themselves have module-global names while record visibility follows `pub`.

---

### 3.6 Documentation Comments and HTML Docs

Prefix public declarations with `///` doc comments:

```iris
/// Compute kinetic energy `0.5 * m * v * v` for a non-negative mass `m`.
pub def kinetic_energy(mass: f64, speed: f64) -> f64 {
    return 0.5 * mass * speed * speed
}
```

These doc comments surface automatically in two places:

1. **In your editor**: `iris lsp` displays `///` summaries on hover and inside autocomplete popups.
2. **Generated API documentation**: `iris docs <path>` parses `///` comments across your project and renders navigable HTML documentation.

#### Common Mistakes

- **Calling `pub` items without `pub` keyword**: forgetting `pub` on a helper record or function inside `"helpers.iris"` causes `E0100` in `main.iris`.
- **Naming a helper after a builtin by accident**: avoid shadowing core names like `len`, `min`, `max`, `split`, `trim`, `filter`, or `map` unless intentionally overriding in local scope; `std.*` modules already provide rich namespaced helpers.
- **Putting non-tail work after a recursive call**: `return 1 + sum(n - 1)` is *not* a tail call because addition happens after the call returns. Pass an accumulator parameter instead.

#### Try It Yourself

1. Split a two-file mini-project (`geometry.iris` + `main.iris`) where `geometry.iris` exports `pub record Rect`, `pub def rect_area`, and a private helper `def clamp_non_neg`. Verify that calling `clamp_non_neg` from `main.iris` fails to compile until marked `pub`.
2. Rewrite Fibonacci recursively with tail accumulators (`fib_tail(n, a, b)`) and compute `fib_tail(50, 0, 1)`.
3. Document three `pub def` functions with `///` comments and run `iris docs` to inspect the generated HTML output.

---

## Chapter 4: Control Flow

### 4.1 Expression-Oriented Conditionals

In IRIS, `if / else` is an **expression** that evaluates to a value when both branches return the same type:

```iris
def classify_temp(c: f64) -> str {
    val band = if c < 0.0 {
        "freezing"
    } else if c < 25.0 {
        "temperate"
    } else {
        "hot"
    }
    return band
}

def main() -> i64 {
    assert(classify_temp(-3.0) == "freezing")
    assert(classify_temp(18.5) == "temperate")
    assert(classify_temp(35.0) == "hot")
    println("conditionals: ok")
    return 0
}
```

When using `if` solely for side effects inside a statement (`if ready { notify(); }`), an `else` branch is optional. When using the result of `if` in a `val`, argument, or `return`, an `else` branch with a compatible type is **mandatory** (omitting it raises `E0119`, or mismatched branch types raise `E0208`).

---

### 4.2 Loops: Ranges, Collections, Conditionals, and Infinite Loops

IRIS offers four clean iteration forms:

1. **Exclusive integer range**: `for i in start..end` iterates `start, start + 1, ..., end - 1`.
2. **Stepped range**: `for i in start..end by step` advances by `step` each iteration (`by` is a contextual keyword after a range).
3. **Collection iteration**: `for item in list_val` iterates every element of a `list<T>`, fixed array `[T; N]`, or map/collection view.
4. **Conditional `while` and unconditional `loop`**: repeat while a condition holds, or until `break`.

```iris
def main() -> i64 {
    // 1. Exclusive range 1..11 (1 through 10)
    var sum = 0
    for n in 1..11 {
        sum = sum + n
    }

    // 2. Stepped range with `by`
    var evens = 0
    for k in 0..10 by 2 {
        evens = evens + k
    }

    // 3. Collection iteration
    val weights = list(3, 5, 8)
    var total_w = 0
    for w in weights {
        total_w = total_w + w
    }

    // 4. Conditional while
    var countdown = 4
    while countdown > 0 {
        countdown = countdown - 1
    }

    assert(sum == 55)
    assert(evens == 20)
    assert(total_w == 16)
    assert(countdown == 0)
    println("loops: ok")
    return 0
}
```

Notice that `while countdown > 0` explicitly mutates `countdown` inside the loop body. The compiler's lint pass warns when a `while` loop condition references variables that never change inside the loop body (a common bug when forgetting `i = i + 1`).

---

### 4.3 Breaking, Continuing, and Loop Labels

Inside `while`, `for`, or `loop`, use `continue` to skip to the next iteration and `break` to exit the innermost loop:

```iris
def find_first_product_above(limit: i64) -> i64 {
    var result = -1
    var row = 1
    while row <= 10 {
        var col = 1
        while col <= 10 {
            val prod = row * col
            if prod > limit {
                result = prod
                break
            }
            col = col + 1
        }
        if result >= 0 {
            break
        }
        row = row + 1
    }
    return result
}

def main() -> i64 {
    var odd_sum = 0
    for n in 1..10 {
        if n % 2 == 0 {
            continue
        }
        odd_sum = odd_sum + n
    }
    assert(odd_sum == 25)
    assert(find_first_product_above(45) == 50)
    println("break and continue: ok")
    return 0
}
```

`loop { ... }` also supports breaking directly when a sentinel state is reached, and early `return` immediately unwinds out of nested loops and exits the enclosing function cleanly.

---

### 4.4 Logical and Bitwise Operators

IRIS supports both symbolic boolean operators and readable keyword forms:

| Operation | Symbolic | Keyword Equivalent | Behavior |
|---|---|---|---|
| Logical AND | `a && b` | `a and b` | Short-circuits if `a` is `false` |
| Logical OR | `a \|\| b` | `a or b` | Short-circuits if `a` is `true` |
| Logical NOT | `!a` | `not a` | Inverts `bool` |

Bitwise operations on integers use explicit functions rather than `&`/`|` symbols (which are reserved for references and closure parameter lists):

```iris
def main() -> i64 {
    val flags = bor(shl(1, 3), shl(1, 1))   // bits 3 and 1 -> 0b1010 = 10
    val has_bit_3 = band(flags, 8) != 0
    val toggled = bxor(flags, 2)            // 0b1000 = 8
    val shifted = shr(flags, 1)             // 0b0101 = 5

    assert(flags == 10)
    assert(has_bit_3)
    assert(toggled == 8)
    assert(shifted == 5)
    println("logical and bitwise: ok")
    return 0
}
```

All bitwise builtins (`band`, `bor`, `bxor`, `shl`, `shr`, `bitnot`) are globally available without imports.

---

### 4.5 Tuple Destructuring and Expression Tails

Functions that return multiple values typically return a tuple `(A, B)`. Destructure tuples directly in a `val` binding or index their components with `.0`, `.1`:

```iris
def divide_with_rem(num: i64, den: i64) -> (i64, i64) {
    return (num / den, num % den)
}

def main() -> i64 {
    val (q, r) = divide_with_rem(29, 6)
    val pair = divide_with_rem(17, 5)
    assert(q == 4 && r == 5)
    assert(pair.0 == 3 && pair.1 == 2)

    val adjusted = {
        val base = q * 10
        base + r
    }
    assert(adjusted == 45)
    println("destructuring and expression tails: ok")
    return 0
}
```

#### Common Mistakes

- **Using `..=` in `for` loops**: `..=` is an inclusive range pattern for `when` arms (`1..=9`). For `for` loops, write exclusive `0..n` (so `0..10` visits `0..9`, or `0..11` visits `0..10`).
- **Omitting `else` on a value-producing `if`**: `val x = if cond { 1 }` fails with `E0119`; supply `else { 0 }`.
- **Chaining comparisons (`0 < x < 10`)**: comparisons do not chain. Write `0 < x && x < 10`.

#### Try It Yourself

1. Write `fizzbuzz_score(n: i64) -> str` using expression-oriented `if / else if / else`.
2. Sum all multiples of 3 or 5 below 100 using a single `for` loop and `continue`.
3. Write a function returning `(min_val, max_val)` for a non-empty `list<i64>` and unpack it with `val (lo, hi) = ...`.


---

# Part II: Data, Abstraction, and Safety

## Chapter 5: Records, Algebraic Data Types, and Collections

### 5.1 Records and Methods

Records group named fields into strongly typed data structures. Fields may carry default values, and methods attach cleanly via `impl` blocks (keep `impl` blocks co-located with the record and write methods inside `impl` with `def`, not `pub def`):

```iris
pub record Telemetry {
    altitude_m: f64 = 0.0,
    velocity_mps: f64 = 0.0,
    active: bool = true,
}

impl Telemetry {
    def kinetic_index(self) -> f64 {
        return self.altitude_m + 0.5 * self.velocity_mps * self.velocity_mps
    }

    def boost(self, dv: f64) -> Telemetry {
        return Telemetry {
            altitude_m: self.altitude_m,
            velocity_mps: self.velocity_mps + dv,
            active: self.active,
        }
    }
}

def main() -> i64 {
    val craft = Telemetry { altitude_m: 120.0, velocity_mps: 10.0 }
    val boosted = craft.boost(4.0)
    assert(craft.kinetic_index() == 170.0)
    assert(boosted.velocity_mps == 14.0)
    assert(boosted.active)
    println("records and methods: ok")
    return 0
}
```

When constructing a record, any field with a default value can be omitted cleanly, which keeps state-record updates concise as data schemas grow.

---

### 5.2 Algebraic Data Types with Choice Enums

Use `choice` to model values that take one of several mutually exclusive variants—optionally carrying typed payloads:

```iris
choice Alert {
    Nominal,
    Throttle(f64),
    Fault(i64, str),
}

def render_alert(a: Alert) -> str {
    return when a {
        Alert.Nominal => "OK",
        Alert.Throttle(ratio) => f"THROTTLE={ratio}",
        Alert.Fault(code, msg) => f"ERR({code}): {msg}",
    }
}

def main() -> i64 {
    val ok_state = Alert.Nominal
    val warn_state = Alert.Throttle(0.75)
    val err_state = Alert.Fault(42, "sensor timeout")

    assert(ok_state == Alert.Nominal)
    assert(render_alert(warn_state) == "THROTTLE=0.75")
    assert(render_alert(err_state) == "ERR(42): sensor timeout")
    println("choice enums: ok")
    return 0
}
```

Note that `choice` declarations represent concrete closed variant families (they do not take generic `[T]` type parameters; use `record Name[T]` or `option<T>` / `result<T, E>` when you need parametric containers). Structural `==` and `!=` work directly on `choice` values.

---

### 5.3 Tuples and Fixed Arrays

IRIS provides two stack-friendly fixed-structure aggregates:

- **Tuples `(T1, T2, ...)`**: lightweight heterogeneous grouping accessed by `.0`, `.1`, or destructured via `val (a, b) = pair`.
- **Fixed arrays `[T; N]`**: fixed-length homogeneous sequences with compile-time size `N`. Mutate elements in place via indexing when bound with `var`, and read length with `len(arr)`. Out-of-bounds access halts safely with runtime diagnostic `E0403`.

```iris
def main() -> i64 {
    var samples: [i64; 4] = [10, 20, 30, 40]
    samples[2] = 99
    assert(len(samples) == 4)
    assert(samples[2] == 99)

    val reading = ("alpha", 256, true)
    assert(reading.0 == "alpha" && reading.1 == 256 && reading.2)
    println("tuples and arrays: ok")
    return 0
}
```

---

### 5.4 Dynamic Lists and Maps

When sizes vary dynamically at runtime, reach for `list<T>` and `map<K, V>`, which are reference-counted heap collections backed by global builtins:

```iris
def main() -> i64 {
    val nums: list<i64> = list(10, 20)
    list_push(nums, 30)
    list_set(nums, 1, 25)
    assert(list_len(nums) == 3)
    assert(list_get(nums, 1) == 25)

    val popped = list_pop(nums)
    assert(popped == 30)
    assert(list_len(nums) == 2)

    val ports: map<str, i64> = map()
    map_set(ports, "http", 8080)
    map_set(ports, "metrics", 9090)
    assert(map_contains(ports, "http"))
    assert(unwrap(map_get(ports, "http")) == 8080)

    val keys = map_keys(ports)
    assert(list_len(keys) == 2)
    println("lists and maps: ok")
    return 0
}
```

Notice `map_get(m, key)` returns `option<V>` (`some(value)` or `none()`), preventing missing-key crashes. Notice also that constructing an empty `list()` or `map()` without initializers requires a binding type annotation such as `: list<i64>` or `: map<str, i64>` (`E0207` otherwise).

---

### 5.5 Sets, Deques, Heaps, Queues, BitSets, and Buffers

Beyond lists and maps, the embedded standard library supplies specialized containers tuned for distinct access patterns:

| Module | Types / Constructors | Core Operations |
|---|---|---|
| `std.set` | `Set<T>`, `set_new()`, `set_from_list(xs)` | `set_add`, `set_remove`, `set_contains`, `set_union`, `set_intersection`, `set_difference`, `set_len` |
| `std.deque` | `deque_create()` (backed by `list<i64>`) | `push_front`, `push_back`, `pop_front`, `pop_back`, `peek_front`, `peek_back`, `deque_size` |
| `std.heap` | `MinHeap<T>`, `heap_new()`, `heap_from_list(xs)` | `heap_push`, `heap_pop`, `heap_pop_val`, `heap_peek`, `heap_len`, `heap_merge` |
| `std.queue` | `Queue`, `queue_new()` | `enqueue`, `dequeue_val`, `dequeue_queue`, `queue_peek`, `queue_len` |
| `std.bitset` | `bitset_create()` | `set_bit`, `get_bit`, `clear_bit`, `toggle_bit`, `popcount`, `bitset_union`, `bitset_intersection` |
| `std.buffer` | `ByteBuffer`, `byte_buffer_from_str(s)`, `byte_buffer_from_hex(h)` | `byte_buffer_read_u16_be`, `byte_buffer_read_u32_be`, `byte_buffer_slice`, `byte_buffer_to_hex` |

Here is a compact example exercising sets and bitsets side by side:

```iris
bring std.set
bring std.bitset

def main() -> i64 {
    var tags: Set<str> = set_new()
    tags = set_add(tags, "telemetry")
    tags = set_add(tags, "vision")
    tags = set_add(tags, "telemetry")
    assert(set_len(tags) == 2)
    assert(set_contains(tags, "vision"))

    var flags = bitset_create()
    flags = set_bit(flags, 2)
    flags = set_bit(flags, 7)
    assert(get_bit(flags, 2))
    assert(!get_bit(flags, 3))
    assert(popcount(flags) == 2)

    println("specialized collections: ok")
    return 0
}
```

---

### 5.6 Strings, Formatting, Regular Expressions, and DateTime

IRIS includes UTF-8 string primitives globally, rich Unicode/text helpers in `std.string`, `std.unicode`, and `std.fmt`, regex builtins, and calendar/clock utilities in `std.time`:

```iris
bring std.string
bring std.fmt
bring std.time

def main() -> i64 {
    // Core string builtins + std.string
    val raw = "  iris,vision,control  "
    val cleaned = trim(raw)
    val parts = split(cleaned, ",")
    assert(list_len(parts) == 3)
    assert(str_join(parts, " | ") == "iris | vision | control")
    assert(pad_left("42", 5, "0") == "00042")

    // Built-in regular expressions
    assert(regex_match("\\d+", "port=8080"))
    val nums = regex_find_all("\\d+", "a12 b34 c56")
    assert(list_len(nums) == 3)
    assert(regex_replace_all("\\d+", "v1 -> v2", "X") == "vX -> vX")

    // Formatting tables / sprintf and ISO-8601 UTC date formatting
    val summary = sprintf("node=%s count=%d", list("edge-1", "7"))
    assert(summary == "node=edge-1 count=7")
    val iso = format_iso8601(0)
    assert(starts_with(iso, "1970-01-01"))

    println("strings, regex, and time: ok")
    return 0
}
```

For Unicode code-point iteration and UTF-8 scalar handling beyond ASCII byte helpers, import `std.unicode`.

#### Common Mistakes

- **Treating `set_add` or `set_bit` as mutating in place**: functional-style stdlib collection helpers (`set_add`, `heap_push`, `set_bit`, `enqueue`) return the updated value. Reassign `s = set_add(s, x)` or thread the returned value forward.
- **Writing `regex_replace` instead of `regex_replace_all`**: the built-in substitution function is `regex_replace_all(pattern, text, replacement)`.
- **Mutating fields directly on immutable record instances**: records follow value-update style—construct an updated copy cleanly (as in `boost` above), or keep mutable aggregates inside explicit handles when needed.

#### Try It Yourself

1. Model a simple order book or task queue with `record Task { id: i64, priority: i64, title: str }` and a `choice Status { Queued, Running, Done, Failed(str) }`.
2. Write a program using `MinHeap<i64>` from `std.heap` to push five out-of-order integers and pop them into ascending sorted order verified with `assert`.
3. Parse `"temp=23.5;hum=61.0;node=alpha"` with `regex_find_all` or `split` and `std.string.strip_prefix`, verifying every field with `assert`.

---

## Chapter 6: Pattern Matching with When

### 6.1 Matching Choice Variants

The `when` expression dispatches on a value's structure, binding variant payloads directly in each arm:

```iris
choice Packet {
    Ping(i64),
    Data(str, i64),
    Close,
}

def handle_packet(pkt: Packet) -> i64 {
    return when pkt {
        Packet.Ping(seq) => seq,
        Packet.Data(topic, bytes) => len(topic) + bytes,
        Packet.Close => 0,
    }
}

def main() -> i64 {
    assert(handle_packet(Packet.Ping(12)) == 12)
    assert(handle_packet(Packet.Data("imu", 64)) == 67)
    assert(handle_packet(Packet.Close) == 0)
    println("when choice matching: ok")
    return 0
}
```

---

### 6.2 Matching Options and Results

Instead of risking panics with `unwrap(...)`, handle `option<T>` and `result<T, E>` explicitly through `when`:

```iris
def parse_port(raw: str) -> result<i64, str> {
    val parsed: option<i64> = parse_i64(raw)
    return when parsed {
        some(p) => {
            if p >= 1 && p <= 65535 {
                ok(p)
            } else {
                err("port out of range")
            }
        },
        none => err("not an integer"),
    }
}

def describe_port(r: result<i64, str>) -> str {
    return when r {
        ok(port) => f"listening on {port}",
        err(reason) => f"invalid config: {reason}",
    }
}

def main() -> i64 {
    assert(describe_port(parse_port("8080")) == "listening on 8080")
    assert(describe_port(parse_port("99999")) == "invalid config: port out of range")
    assert(describe_port(parse_port("abc")) == "invalid config: not an integer")
    println("option and result matching: ok")
    return 0
}
```

Note an important IRIS standard-library rule illustrated here: `parse_i64(s)` and `parse_f64(s)` return **`option<i64>` / `option<f64>`** (not `result`), so match `some(n)` and `none`—or coalesce directly with `??`.

---

### 6.3 Literals, Inclusive Ranges, Tuples, and Wildcards

`when` also matches integers, booleans, strings, inclusive integer ranges (`lo..=hi`), flat tuples, and the catch-all wildcard `_`:

```iris
def http_category(status: i64) -> str {
    return when status {
        200 => "OK",
        201..=299 => "Success",
        400..=499 => "Client Error",
        500..=599 => "Server Error",
        _ => "Other",
    }
}

def quadrant(x_pos: bool, y_pos: bool) -> i64 {
    return when (x_pos, y_pos) {
        (true, true) => 1,
        (false, true) => 2,
        (false, false) => 3,
        (true, false) => 4,
    }
}

def main() -> i64 {
    assert(http_category(200) == "OK")
    assert(http_category(204) == "Success")
    assert(http_category(404) == "Client Error")
    assert(quadrant(true, false) == 4)
    println("ranges and tuple matching: ok")
    return 0
}
```

---

### 6.4 Match Guards and Value-Producing Arms

Add conditional precision with `if guard` clauses on any arm, or compute multi-statement blocks inside braced arms whose last expression produces the arm's result:

```iris
choice Measurement {
    Valid(f64),
    Missing,
}

def score_reading(m: Measurement, limit: f64) -> str {
    return when m {
        Measurement.Valid(v) if v < 0.0 => "negative",
        Measurement.Valid(v) if v > limit => {
            val excess = v - limit
            f"over by {excess}"
        },
        Measurement.Valid(_) => "nominal",
        Measurement.Missing => "missing",
    }
}

def main() -> i64 {
    assert(score_reading(Measurement.Valid(-1.5), 10.0) == "negative")
    assert(score_reading(Measurement.Valid(12.5), 10.0) == "over by 2.5")
    assert(score_reading(Measurement.Valid(4.0), 10.0) == "nominal")
    assert(score_reading(Measurement.Missing, 10.0) == "missing")
    println("match guards: ok")
    return 0
}
```

Notice the clean fallback `Measurement.Valid(_)` after the two guarded `Measurement.Valid(v) if ...` arms: guards do not promise exhaustiveness by themselves, so always cover the unguarded case or `_`.

---

### 6.5 Exhaustiveness Checking and Practical Pattern Rules

The compiler's `ExhaustivePass` validates `when` expressions across `choice` variants, `bool`, `option<T>`, and `result<T, E>` (reporting `E0210` if a case is unhandled). Keep two practical pattern boundaries in mind:

1. **No nested destructuring inside constructor patterns**: `when opt { some((a, b)) => ... }` is rejected; bind `some(pair)` cleanly and unpack `val (a, b) = pair` (or return a small `record` like `UiSize` instead of a nested tuple).
2. **All arms of a value-producing `when` must agree on type**: mismatched arm types trigger `E0208`.

#### Common Mistakes

- **Nesting tuple patterns inside `some(...)` or `ok(...)`**: bind `some(v)` or `ok(rec)` as a single identifier and destructure inside the arm block.
- **Using `match` instead of `when`**: `match` is reserved; IRIS uses `when` for pattern matching.
- **Forgetting a fallback arm after guarded arms**: guarded patterns may all evaluate to `false` at runtime, so include an unguarded catch-all arm.

#### Try It Yourself

1. Write a `choice Expr { Lit(i64), Add(i64, i64), Neg(i64) }` evaluator using `when`.
2. Write `classify_char_code(code: i64) -> str` using inclusive range arms `48..=57`, `65..=90`, `97..=122`, and `_`.
3. Remove one `choice` variant from a `when` block temporarily and inspect the compiler's `E0210` error message via `iris check`.

---

## Chapter 7: Generics, Const Generics, and Traits

### 7.1 Generic Functions and Generic Records

IRIS monomorphizes generic functions and generic records (`MonoPass`) into type-specialized SSA code, giving static type safety with zero dynamic boxing overhead:

```iris
record PairBox[T, U] {
    first: T,
    second: U,
}

def identity[T](value: T) -> T {
    return value
}

def swap_pair[A, B](p: PairBox<A, B>) -> PairBox<B, A> {
    return PairBox { first: p.second, second: p.first }
}

def main() -> i64 {
    val n = identity(42)
    val s = identity("IRIS")
    val p: PairBox<str, i64> = PairBox { first: "port", second: 8080 }
    val swapped = swap_pair(p)

    assert(n == 42 && s == "IRIS")
    assert(swapped.first == 8080 && swapped.second == "port")
    println("generics: ok")
    return 0
}
```

Observe the angle vs. bracket convention across IRIS definitions and call sites: declarations write `def f[T]` and `record R[T]`, whereas type annotations write `R<i64>` (just like `list<T>` and `option<T>`).

---

### 7.2 Const Generics and Higher-Kinded Records

Records and functions can also parameterize over **compile-time integer sizes (`const N: usize`)** and **higher-kinded type constructors (`F[_]`)**:

```iris
record FixedVec[T, const N: usize] {
    data: [T; N],
}

record Wrapped[F[_], A] {
    inner: F<A>,
}

record Box[T] {
    item: T,
}

def main() -> i64 {
    val vec3: FixedVec<f64, 3> = FixedVec { data: [1.0, 2.5, 4.0] }
    val wrapped: Wrapped<Box, str> = Wrapped {
        inner: Box { item: "tensor-ready" },
    }

    assert(vec3.data[1] == 2.5)
    assert(wrapped.inner.item == "tensor-ready")
    println("const generics and higher-kinded records: ok")
    return 0
}
```

Const-generic array bounds propagate directly into field metadata (`IrModule::struct_defs`) across unannotated struct literals and cross-function calls.

---

### 7.3 Defining and Implementing Traits

A `trait` declares shared behavior that any `record` can implement via `impl Trait for Type`:

```iris
trait Metric {
    def score(self) -> f64
}

record SensorNode {
    id: i64,
    confidence: f64,
}

impl Metric for SensorNode {
    def score(self) -> f64 {
        return self.confidence * 100.0
    }
}

def main() -> i64 {
    val node = SensorNode { id: 1, confidence: 0.94 }
    assert(node.score() == 94.0)
    println("traits: ok")
    return 0
}
```

Inside `impl ... { ... }`, declare methods with `def` (do not write `pub def`) and write bare `self` (or `self: RecordName`) so method bodies resolve record fields cleanly on the concrete implementing type rather than abstract `Self`. Traits may also declare effect bounds on signatures—for instance `def forward(self, input: DiffTensor) -> DiffTensor effect alloc`.

---

### 7.4 Trait Bounds and Dynamic Dispatch

IRIS supports **two complementary polymorphism mechanisms**:

1. **Static dispatch via bounded generics (`[T where T: Trait]` / blanket `impl`)**: monomorphized at compile time for maximum speed.
2. **Dynamic dispatch via trait objects (`dyn Trait`)**: stores heterogeneous record values behind a uniform interface when types only unify at runtime (such as a `list<dyn Show>` or simulation entity list).

```iris
trait Show {
    def show(self) -> str
}

trait Describe {
    def describe(self) -> str
}

record Thruster {
    name: str,
}

impl Show for Thruster {
    def show(self) -> str {
        return self.name
    }
}

impl[T where T: Show] Describe for T {
    def describe(self) -> str {
        return "unit: " + self.show()
    }
}

def main() -> i64 {
    val unit = Thruster { name: "RCS-Alpha" }
    assert(unit.describe() == "unit: RCS-Alpha")

    val obj: dyn Show = unit
    assert(obj.show() == "RCS-Alpha")
    println("trait bounds and dyn dispatch: ok")
    return 0
}
```

---

### 7.5 Macros and Compile-Time Functions

For lightweight syntactic abstraction and compile-time scalar evaluation, IRIS provides **hygienic macros (`defmacro`)** and **compile-time functions (`const def`)**:

```iris
defmacro double_expr(x) => x * 2

const def square_i64(n: i64) -> i64 {
    return n * n
}

def main() -> i64 {
    val a = double_expr!(21)
    val b = square_i64(9)
    assert(a == 42)
    assert(b == 81)
    println("macros and const def: ok")
    return 0
}
```

Macro invocations use `!` (`double_expr!(21)`), while `const def` functions evaluate deterministically during compilation when supplied constant inputs.

#### Common Mistakes

- **Writing `pub def` inside an `impl` block**: `impl` methods use plain `def`.
- **Omitting return type parameter annotations on empty generic constructors**: `val b = Box { ... }` works when fields determine `T`, whereas `set_new()` needs `val s: Set<i64> = set_new()`.
- **Separating `impl Trait for R` into another module from `R`**: keep trait implementations in the defining module alongside `record R`.

#### Try It Yourself

1. Write a generic `record RingSlot[T, const N: usize]` holding `[T; N]` and instantiate it for `i64` and `str`.
2. Define a `trait CelestialMetric { def kinetic_energy(self: Self) -> f64 }`, implement it for a `record Orbiter`, and query an instance through `dyn CelestialMetric`.
3. Write `defmacro clamp01(x)` or `const def cube(n: i64) -> i64` and assert their results natively.

---

## Chapter 8: Error Handling, Ownership, and Memory Management

### 8.1 Options, Results, and Error Propagation

IRIS represents absence and recoverable failure explicitly in the type system:

- **`option<T>`** (`some(v)` / `none()`): when a value may legitimately be absent (`map_get`, `parse_i64`, `find`).
- **`result<T, E>`** (`ok(v)` / `err(e)`): when an operation can fail with diagnostic context.

Use three ergonomic tools to compose them cleanly without deeply nested boilerplate:

1. **`?` early propagation**: inside a function returning `result<U, E>` or `option<U>`, suffixing an expression with `?` unwraps the success value or immediately returns the `err`/`none`.
2. **`??` default coalescing**: `expr ?? fallback` yields the inner value or evaluates `fallback`.
3. **`try { ... } catch reason { ... }`**: intercepts `err(reason)` propagated via `?` locally inside a block!

```iris
def safe_div(a: i64, b: i64) -> result<i64, str> {
    if b == 0 {
        return err("division by zero")
    }
    return ok(a / b)
}

def stepped(a: i64, b: i64) -> result<i64, str> {
    val q = safe_div(a, b)?
    return ok(q * 10)
}

def main() -> i64 {
    assert(unwrap(stepped(24, 4)) == 60)

    val recovered = try {
        stepped(24, 0)?
    } catch reason {
        assert(reason == "division by zero")
        -1
    }
    assert(recovered == -1)

    val port = parse_i64("bad") ?? 8080
    assert(port == 8080)
    println("error handling: ok")
    return 0
}
```

---

### 8.2 Assertions, Panics, and Runtime Diagnostics

Use `assert(condition)` for invariants and test checks, or `panic("reason")` on unrecoverable states. When a runtime check triggers (integer overflow, division by zero, out-of-bounds access, or failed `assert`/`unwrap`), both backends exit with code **101**, preserve stdout already flushed before the failure, and format a precise source-mapped diagnostic:

| Code | Meaning |
|---|---|
| `E0402` | Integer division or remainder by zero |
| `E0403` | Index out of bounds (`list`, array, `str`, or tensor rank) |
| `E0406` | `panic(...)`, failed `assert(...)`, or `unwrap` on `none`/`err` |
| `E0408` | Stack overflow / recursion depth limit exceeded |
| `E0409` | Checked integer arithmetic overflow/underflow |

Run `iris explain <code>` (for example `iris explain E0409`) to inspect detailed causes and fixes at any time.

---

### 8.3 Borrowing, Mutable Write-Back, and Explicit Moves

IRIS supports block- and CFG-checked **references (`&T`, `&mut T`)**, **dereference write-back (`*p = val`, `*p += k`)**, and **explicit `move`** transfers:

```iris
def read_plus_one(r: &i64) -> i64 {
    return *r + 1
}

def bump_in_place(p: &mut i64, delta: i64) -> i64 {
    *p += delta
    return *p
}

def main() -> i64 {
    val base = 41
    val ref_base: &i64 = &base
    assert(read_plus_one(ref_base) == 42)

    var counter: i64 = 10
    val p: &mut i64 = &mut counter
    assert(bump_in_place(p, 5) == 15)
    assert(counter == 15)

    // Explicit move invalidates the original binding at compile time (E0382 on reuse)
    val items: list<i64> = list(3, 5, 8)
    val owned = move items
    assert(list_get(owned, 2) == 8)

    println("borrowing and moves: ok")
    return 0
}
```

Dereference write-back supports conditional `&mut` branch selection (`val p = if cond { &mut a } else { &mut b }; *p = 77`), loop mutations, compound operators (`+=`, `-=`, `*=`), and projected struct/tuple targets (`(*p).field = v`, `(*p).0 = v`).

---

### 8.4 CFG Non-Lexical Lifetimes and Explicit Drop Scopes

Unlike naïve lexical borrow scopes, IRIS runs **`CfgBorrowCheckerPass`** over control-flow-graph liveness (NLL) alongside **`DropElaborationPass`**:

- **Fine-grained path tracking**: move state tracks field/tuple projections (`move pair.a` invalidates `pair.a`, leaving `pair.b` valid while rejecting moving all of `pair`).
- **Scope-driven drops**: `EnterScope` / `ExitScope` / `Drop` instructions ensure destructors run cleanly once per owned value—and skip values already moved out so a moved binding is never double-freed.
- **Opt-in strict record moves**: setting `IRIS_STRICT_RECORD_OWNERSHIP=1` turns by-value record parameter passing, record-field initialization from another record binding, and binding reassignment into implicit ownership moves (`E0382` on subsequent use of the moved source). Explicit `move` is always checked regardless of this environment variable.

---

### 8.5 Two-Tier Memory Reclamation and Frame Pools

Long-running services and interactive 60 FPS desktop GUIs need predictable, leak-free memory behavior. IRIS combines **two reclamation tiers** (see [Memory Management](memory-management.md)):

1. **Tier 1 — Compiler-Inserted Reference Counting + Cycle Collector**:
   Strings, lists, maps, options, results, channels, mutexes, atomics, closures, tensors, and native records (`IRIS_RC_NATIVE_OBJECT`) are reference-counted. `GcAnnotatePass` inserts `retain`/`release` calls at **last use**, freeing short-lived values promptly, while a Bacon–Rajan cycle collector reclaims reference cycles.
   *(Note: because `dyn Trait` wrappers hold raw pointers to wrapped records, records are reclaimed via the RC table's native-object destructor and cycle collector rather than raw last-use pointer free.)*

2. **Tier 2 — Explicit Arena Frame Pools (`ui_frame_pool`)**:
   Interactive UI frames construct fresh node trees, styles, hit-test rectangles, and draw command lists every frame. Wrapping each frame inside `ui_frame_pool_begin()` ... `ui_frame_pool_destroy(pool)` allocates transient data on a fast per-frame arena and reclaims every byte in bulk at frame end.
   **`FramePoolScopePass`** additionally verifies that any open `ui_frame_pool_begin()` has `iris_ui_pool_destroy` inserted before `Return` or `Panic` terminators so pools cannot leak on early exits.

> **Critical Rule for Persistent State Inside a Frame Pool**:
> Record field writes and struct literals allocate on whichever allocator is currently active! Always **pause the active frame pool**, **detach surviving values (`ui_state_detach`, `list`/`str` clones)** onto the main heap, mutate persistent app state while paused, and only **resume and destroy** afterward:
>
> ```iris
> // illustrative UI frame loop pattern (requires `bring std.ui`)
> val pool = ui_frame_pool_begin()
> val tree = build_ui(app, metrics)
> val buf = ui_paint(tree, ui_st, buf)
>
> // Pause pool BEFORE detaching or mutating persistent `app` state!
> val paused = ui_frame_pool_pause()
> val clean_ui_st = ui_state_detach(next_ui_st)
> app = apply_events(app, clean_ui_st, events)
> val _resumed = ui_frame_pool_resume(paused)
> val _freed = ui_frame_pool_destroy(pool)
> ```

---

### 8.6 Inspecting Allocations with GC Statistics

You can audit memory health and hunt leaks directly from IRIS code via `std.async`:

```iris
bring std.async

def main() -> i64 {
    val before = gc_get_stats()
    var sink = 0
    for i in 0..100 {
        val tmp = f"packet-{i}"
        sink = sink + len(tmp)
    }
    gc_collect_sweep()
    val after = gc_get_stats()

    val allocs = map_get(after, "allocated") ?? 0
    val freed = map_get(after, "freed") ?? 0
    val live_bytes = map_get(after, "raw_bytes_live") ?? 0
    val peak_pool = map_get(after, "pool_bytes_peak") ?? 0

    assert(sink > 0)
    assert(allocs >= 0 && freed >= 0 && live_bytes >= 0 && peak_pool >= 0)
    println(f"gc check ok: alloc={allocs} freed={freed} live_bytes={live_bytes}")
    return 0
}
```

`gc_get_stats()` reports `allocated`, `freed`, `cycles`, `weak_invalidated`, raw heap metrics (`raw_allocs`, `raw_frees`, `raw_bytes_live`, `raw_bytes_peak`), frame-pool counters (`pool_bytes_live`, `pool_bytes_peak`), and per-site counts (`site:<location>`). In a steady-state server or desktop app, `raw_bytes_live` should plateau cleanly rather than climbing monotonically.

#### Common Mistakes

- **Unwrapping `option`/`result` inside production logic with `unwrap(...)`**: prefer `?`, `??`, or exhaustive `when`.
- **Mutating persistent application records while `ui_frame_pool_begin()` is active**: if any child string, list, or nested record allocated in the pool ends up stored in long-lived state, destroying the pool triggers a use-after-free crash. Always `ui_frame_pool_pause()` + `ui_state_detach(...)` first.
- **Reusing a binding after `move x` or partial field move of the whole record**: moving `p.a` leaves `p.b` usable, but `move p` afterward is rejected by `CfgBorrowCheckerPass`.

#### Try It Yourself

1. Write a chained config parser using `parse_i64` + `?` + `try / catch` and assert both valid and malformed cases.
2. Write a function with two `var` counters, select `&mut` between them conditionally via `if`, mutate via `*ptr += 10`, and assert both final values.
3. Run a 1,000-iteration allocation loop alongside `gc_get_stats()` before and after `gc_collect_sweep()` to observe `allocated` vs. `freed`.


---

# Part III: Effects, Concurrency, and Systems

## Chapter 9: Static Effects, Handlers, and Speculation

### 9.1 Effect Annotations and Strict Effect Checking

In safety-critical software, reading a type signature like `-> i64` does not tell you whether a function touches the filesystem, allocates on the heap, invokes FFI, or sleeps on the network. IRIS makes side effects explicit directly in function and closure types via `effect` clauses:

```iris
/// Pass `--strict-effects` to enforce effect bounds statically.
def compute_pure(x: i64) -> i64 {
    return x * 2 + 1
}

def log_result(value: i64) -> i64 effect io, alloc {
    println(f"result = {value}")
    return value
}

def main() -> i64 effect io, alloc {
    val ans = compute_pure(20)
    return log_result(ans) - 41
}
```

Common tracked effect labels in IRIS and its standard library include `io`, `alloc`, `fs`, `net`, `ffi`, `thread`, `time`, `env`, `sys`, `random`, `throw`, `reflect`, and `transaction`.

By default, effect annotations document intent while migration stays frictionless. Enable **`--strict-effects`** (`iris check --strict-effects` or `iris build --strict-effects`) to enforce that every function's `effect` clause covers every effect triggered by its body and callees (`E0304` on violations). Both `iris embedded` and `iris evolve` turn on strict effect checking automatically.

---

### 9.2 Polymorphic Effect Rows

Higher-order helpers rarely know in advance which effects a caller's callback might trigger. Use an uppercase effect variable such as `effect E` to stay agnostic cleanly:

```iris
/// Run with: iris --strict-effects run effects.iris
def apply(value: i64, operation: |i64| -> i64 effect E) -> i64 effect E {
    return operation(value)
}

def increment(value: i64) -> i64 {
    return value + 1
}

def main() -> i64 effect io, alloc, throw {
    val plus_one = |value: i64| value + 1
    assert(apply(41, plus_one) == 42)
    assert(apply(41, increment) == 42)
    println("polymorphic effects: ok")
    return 0
}
```

Because `apply` quantifies over `effect E`, passing a pure function (`increment`) requires zero caller effects, whereas passing an effectful callback propagates that callback's exact effects to the caller under `--strict-effects`.

---

### 9.3 Algebraic Effect Handlers

Beyond checking effects, IRIS supports scoped interception through **`handle { ... } with { ... }`** expressions. An external declaration (`extern def`) or effectful operation can be intercepted cleanly inside a test harness or sandbox block without rewriting production callers:

```iris
extern def echo(value: str) -> str

def main() -> i64 {
    val intercepted = handle {
        echo("hello")
    } with {
        echo(value) => "handled",
    }

    assert(intercepted == "handled")
    println("effect handlers: ok")
    return 0
}
```

Notice `handle ... with` evaluates directly as an expression, so you can return its result, bind it to a `val`, or assign it to state.

---

### 9.4 Transactional Speculation and Rollback

When testing an untrusted optimization, heuristic policy, or multi-step mutation, `std.speculation` gives you **speculative evaluation and reversible thread-local transactions**:

```iris
bring std.speculation

def main() -> i64 {
    val candidate = || 500
    val fallback = || 10
    val in_bounds = |value: i64| value >= 0 && value < 100

    val chosen = speculate_with_fallback(candidate, fallback, in_bounds)
    assert(chosen == 10)

    val verified: option<i64> = speculate(|| 42, in_bounds)
    assert(unwrap(verified) == 42)

    // Transactional speculation snapshots lists, maps, atomics, and staged file writes:
    val xs: list<i64> = list(1, 2)
    val outcome = speculate_transactional(
        |n: i64| n < 0,
        || {
            list_push(xs, 999)
            999
        },
    )
    assert(!is_some(outcome))
    assert(list_len(xs) == 2)

    println("speculation: ok")
    return 0
}
```

Under `speculate_transactional`, `transaction_begin()`, `transaction_commit()`, and `transaction_rollback()` maintain nestable thread-local checkpoints over in-memory lists, maps, atomics, and staged whole-file writes (`file_write_all` staged through a synced two-phase `.new`/`.bak` WAL until commit). External network services, open file streams, and physical hardware actuators are excluded from the transactional callback effect row and are intentionally not claimed reversible.

---

### 9.5 Security Policies and Sandboxing

Defense in depth requires runtime mediation alongside static checks:

- **`--sandbox` / `IRIS_SECURITY_POLICY`**: propagates an IRIS security policy into compiled LLVM binaries and the C runtime (`iris_sandbox_set_policy_spec`), mediating C-runtime filesystem access, SQLite opens, ML model loaders, sockets (TCP/UDP/HTTP/TLS/async), processes, environment mutation, Python FFI, and dynamic shared library loads (enforcing path traversal checks, allowlists/blocklists, byte limits, and open-handle quotas). Unmediated `extern def` declarations fail closed unless explicitly allowed via `allow_ffi` / `ffi_allowlist`.
- **OS Worker Sandbox**: because arbitrary pre-linked third-party native machine code outside IRIS C-runtime mediation cannot be policed solely in-process, `iris evolve --validate sandbox` (and host sandboxing APIs) isolates candidate binaries inside OS-native kernel sandboxes: Windows AppContainer/LPAC Job objects (CPU/memory/process/pipe caps), Linux Landlock (`5.13+`) + `seccomp-bpf` + `rlimit`s, and macOS Seatbelt `sandbox-exec` + `rlimit`s. Unsupported platforms fail closed.

#### Common Mistakes

- **Declaring `extern def` without an effect clause when `--strict-effects` is enabled**: mark pure foreign declarations with `effect pure`, or list `effect ffi` / `effect io`.
- **Expecting transactional rollback to undo an HTTP request or GPIO write**: only lists, maps, atomics, and staged whole-file writes participate in `std.speculation` transactions.

#### Try It Yourself

1. Create a small program mixing pure math with `println` and check it with `iris check --strict-effects`. Observe `E0304` when an `effect io, alloc` clause is omitted, then fix the signature.
2. Wrap a mocked `extern def fetch_sensor(id: i64) -> i64` inside `handle { ... } with { fetch_sensor(id) => id * 10, }` and run both interpreter and native builds.
3. Run a test file that attempts filesystem access using `iris --sandbox --emit eval ...` to see runtime policy denial (`E0407`).

---

## Chapter 10: Concurrency, Async/Await, and Parallelism

### 10.1 Typed Channels and Spawned Tasks

IRIS schedules `spawn` tasks onto a bounded native worker executor (capped at 32 workers) and coordinates typed message passing across tasks via **`chan<T>`**:

```iris
def main() -> i64 {
    val results: chan<i64> = channel()

    spawn {
        send(results, 21 * 2)
    }

    val answer = recv(results)
    assert(answer == 42)
    println("channels and spawn: ok")
    return 0
}
```

Waiters blocked on an empty channel help execute queued work rather than spinning idle OS threads. For bounded queues, timeouts, and non-blocking polling, import `std.async` for `channel_bounded(capacity)`, `try_recv(ch)`, `recv_timeout_ms(ch, ms)`, `select_first(channels)`, and `channel_len(ch)`.

---

### 10.2 Structured Concurrency with Task Groups

Detached `spawn` blocks are easy to lose track of. When a fan-out computation must complete together—or support cooperative cancellation on deadlines—group tasks explicitly with `new_task_group()` from `std.async`:

```iris
bring std.async

def main() -> i64 {
    val completed = atomic(0)
    val workers = new_task_group()

    for _i in 0..32 {
        spawn(workers) {
            if !cancellation_requested() {
                atomic_add(completed, 1)
            }
        }
    }

    task_group_join(workers)
    assert(atomic_load(completed) == 32)

    val cancelled = new_task_group()
    task_group_cancel(cancelled)
    spawn(cancelled) {
        atomic_add(completed, 1000)
    }
    task_group_join(cancelled)

    assert(task_group_cancelled(cancelled))
    assert(atomic_load(completed) == 32)
    println("task groups: ok")
    return 0
}
```

`task_group_join(group)` closes the group to new work and blocks while helping drain queued tasks. Cancellation is **cooperative**: queued tasks skip execution on entry when cancelled, while long-running CPU loops should poll `cancellation_requested()` or `task_group_cancelled(group)` at bounded intervals.

---

### 10.3 Async/Await on the Cooperative Executor

Mark asynchronous functions with `async def`. Calling an `async def` returns a single-result awaitable (`chan<T>` under the current RC ABI) that completes cleanly with `await`:

```iris
async def square_async(value: i64) -> i64 {
    return value * value
}

def main() -> i64 {
    val a = square_async(3)
    val b = square_async(4)
    val total = await a + await b
    assert(total == 25)
    println("async await: ok")
    return 0
}
```

Under the compiler's `AsyncLowerPass` (the final pass in the standard pipeline, running *after* `GcAnnotatePass`), `async def` bodies lower cleanly into pollable state machines on both native LLVM builds and the interpreter.

> **Async Runtime Scope**:
> The cooperative executor supports non-blocking wakeups for timers, channels, `tcp_accept_async`, and `tcp_read_async`. Synchronous socket connects, writes, and `std.http` requests remain blocking calls—offload or pool them explicitly in high-concurrency servers.

---

### 10.4 Data Parallelism with Parallel For Loops, SIMD, and GPU

When a computation maps independently across an integer index range, use `par for` to unlock thread, vector, or device parallelism from a single construct:

```iris
def main() -> i64 {
    val total = atomic(0)
    par for index in 1..101 {
        atomic_add(total, index)
    }
    assert(atomic_load(total) == 5050)
    println("par for: ok")
    return 0
}
```

Notice how `par for` accepts flag-selected acceleration without changing source logic:

- **Default native (`iris build` / `iris run`)**: executes iterations concurrently across worker threads with closure captures marshaled safely.
- **`--simd` / `--emit simd`**: lowers element-wise `par for` loops into explicit vector kernels (AVX2 on x86-64, NEON on AArch64) and enables LLVM's loop vectorizer across loops.
- **`--gpu cuda` / `--emit cuda` / `--emit cuda-ptx`**: emits NVIDIA PTX device kernels for numeric and tensor `par for` loops.
  *(Verification boundary: CUDA PTX emission and CPU fallback simulation pass automated tests, while physical NVIDIA GPU kernel execution has not yet been tested on GPU hardware.)*

---

### 10.5 Lock-Free Atomics and Mutexes

For lightweight counters shared across `spawn` or `par for`, prefer lock-free atomics:

```iris
def main() -> i64 {
    val acc = atomic(10)
    atomic_add(acc, 32)
    assert(atomic_load(acc) == 42)
    atomic_store(acc, 7)
    assert(atomic_load(acc) == 7)
    println("atomics: ok")
    return 0
}
```

When coordinating shared multi-field state or serialized resource access across threads, use `mutex<T>` (`mutex`, `mutex_lock`, `mutex_unlock`) or communicate exclusively over typed `chan<T>` queues.

#### Common Mistakes

- **Calling `recv(ch)` on the main thread when nobody calls `send(ch, ...)`**: deadlocks execution until interrupted. Ensure a producer runs inside `spawn` or check non-blockingly with `std.async.try_recv`.
- **Sharing unsynchronized mutable lists across parallel workers**: protect shared state behind a `chan<T>`, `atomic`, or `mutex<T>`.
- **Expecting pre-emptive thread termination on `task_group_cancel`**: long-running compute tasks must check `cancellation_requested()`.

#### Try It Yourself

1. Build a three-worker fan-in pipeline using a shared `chan<i64>` and a `task_group` that waits for all workers to emit their partial sums.
2. Benchmark a numeric array loop under default `iris build` vs. `iris build --simd`.
3. Write an `async def fetch_pair(x: i64, y: i64)` helper that launches two `async def` tasks concurrently before `await`-ing both results.

---

## Chapter 11: Files, Data Formats, SQLite, and Networking

### 11.1 Filesystem, Paths, and Operating System Queries

IRIS separates checked file I/O (`std.fs`), portable path manipulation (`std.path`), and environment/process metadata (`std.os`):

```iris
bring std.path
bring std.os

def main() -> i64 {
    val joined = normalize_path(join_path("data/logs", "../metrics.json"))
    assert(basename(joined) == "metrics.json")
    assert(extension(joined) == "json")
    assert(stem(joined) == "metrics")

    val cpus = cpu_count()
    val pid = get_pid()
    assert(cpus >= 1 && pid > 0)
    println(f"os check: cpus={cpus} pid={pid} file={joined}")
    return 0
}
```

In `std.fs`, high-level helpers (`read_text`, `write_text`, `append_text`, `read_lines`, `copy_file`) and buffered streams (`file_open_stream`, `file_read_stream`, `file_write_stream`, `file_close_stream`) return `result<..., str>` so filesystem permission errors or missing paths never fail silently.

---

### 11.2 JSON, CSV, Tables, and DataFrames

Structured data formats live in `std.json`, `std.csv`, `std.table`, and `std.dataframe`:

```iris
bring std.json
bring std.csv
bring std.dataframe

def main() -> i64 {
    // RFC-4180 CSV rows
    val row = csv_parse_row("telemetry,42,ok")
    assert(list_len(row) == 3 && list_get(row, 1) == "42")

    // Structured JSON encoding & path query
    val doc = json_obj(
        list("service", "replicas", "healthy"),
        list(json_str("iris-node"), json_num_i64(3), json_bool(true)),
    )
    assert(json_query(doc, "service") == "iris-node")
    assert(json_query(doc, "replicas") == "3")

    // Typed numeric DataFrame
    var df = df_new(list("step", "loss"))
    df = df_add_row(df, list(1.0, 0.85))
    df = df_add_row(df, list(2.0, 0.22))
    val filtered = df_filter(df, "loss", 0.0, 0.5)
    assert(df_len(filtered) == 1)

    println("json, csv, and dataframe: ok")
    return 0
}
```

Use `json_escape(text)` before manually assembling JSON fragments from arbitrary user strings, and remember that `std.table` stores string columns (`Table`, `table_new`, `table_insert`, `table_where`, `table_to_csv`) whereas `std.dataframe` stores `f64` numeric matrices optimized for ML/scientific filtering and analytics.

---

### 11.3 Embedded SQLite and Parameterized Queries

IRIS bundles SQLite directly (`std.sql`), requiring zero external database driver installation:

```iris
bring std.sql

def main() -> i64 {
    val db = sql_open(":memory:")
    assert(db != 0)
    assert(sql_exec(db, "CREATE TABLE tasks (title TEXT, done INTEGER)") >= 0)
    assert(sql_exec_params(db, "INSERT INTO tasks VALUES (?, ?)", list("build UI", "1")) >= 0)
    assert(sql_exec_params(db, "INSERT INTO tasks VALUES (?, ?)", list("write docs", "0")) >= 0)

    val pending = sql_query_params(db, "SELECT title FROM tasks WHERE done = ?", list("0"))
    assert(list_len(pending) == 1)
    assert(list_get(list_get(pending, 0), 0) == "write docs")

    sql_close(db)
    println("sqlite: ok")
    return 0
}
```

Always keep values out of raw SQL strings using `sql_exec_params` and `sql_query_params`. Parameter binding avoids SQL injection cleanly and prevents quote-escaping bugs.

---

### 11.4 Key-Value Storage, Cryptography, and Visualization

For persistent flat configs, cryptographic checksums, HMAC authentication, and crisp charting exports, IRIS includes:

- **`std.kv`**: simple file-backed key-value store (`kv_set`, `kv_get`, `kv_delete`, `kv_keys`).
- **`std.crypto`**: `hash_sha256`, `hmac_simple`, `hmac_verify`, `timing_safe_equal`, `to_hex`/`from_hex`, `to_base64`/`from_base64`, `generate_uuid`, `websocket_accept_key`, and `aes128_encrypt_cbc` / `aes128_decrypt_cbc`.
- **`std.svg` and `std.termplot`**: programmatic SVG vector export (`svg_new`, `svg_rect`, `svg_circle`, `svg_line`, `svg_text`, `svg_save`) and instant ASCII terminal bar plots (`term_plot_bar`).

```iris
bring std.crypto

def main() -> i64 {
    val digest = hash_sha256("iris-1.0")
    assert(len(digest) == 64)

    val mac = hmac_simple("secret-key", "payload")
    assert(hmac_verify("secret-key", "payload", mac))
    assert(timing_safe_equal(mac, mac))
    println("crypto: ok")
    return 0
}
```

---

### 11.5 TCP/UDP, HTTP Client/Server, TLS, and Packet Inspection

IRIS ships a complete networking and protocol analysis suite:

- **`std.net`**: bounded, timeout-checked TCP/UDP sockets (`tcp_connect_checked`, `tcp_listen`, `tcp_accept_checked`, `tcp_read_bounded`, `tcp_read_with_timeout`, `tcp_write_all_checked`, `tcp_read_bytes`, `tcp_write_bytes`, `udp_open`, `udp_send`, `udp_recv`, `WebSocketFrame`).
- **`std.http` & `std.http_server`**: `HttpRequest`, `HttpResponse`, `http_send`, `http_tls_available()`, `http_serve`, `http_serve_mt`, routing helpers (`router_new`, `router_add`, `router_match`), and response builders (`json_ok`, `text_ok`, `bad_request`).
  *(Platform note: HTTPS via `http_send` uses OS-native WinHTTP on Windows and fails closed when `http_tls_available()` is `false` on other native targets.)*

- **`std.tls` & `std.pcap`**: binary packet parsing (`pcap_parse_global_header`, `pcap_read_packet`) and passive TLS ClientHello inspection (`tls_parse_record`, `tls_parse_client_hello`, `tls_calculate_ja3`, `tls_calculate_ja4`).

Testing failure boundaries locally is zero-dependency:

```iris
bring std.net

def main() -> i64 {
    val invalid = TcpStream { handle: -1 }
    assert(is_err(tcp_read_bounded(invalid, 0)))
    assert(is_err(tcp_connect_checked("127.0.0.1", -1, 100, 100)))
    println("checked networking: ok")
    return 0
}
```

#### Common Mistakes

- **Interpolating strings into `sql_exec` directly**: always pass user or dynamic values as parameters to `sql_exec_params` / `sql_query_params`.
- **Comparing signatures with `==` instead of `timing_safe_equal`**: use constant-time `timing_safe_equal` / `hmac_verify` for MACs and secrets.
- **Unbounded socket reads**: always prefer `tcp_read_bounded` / `tcp_read_with_timeout` so malformed peers cannot exhaust memory.

#### Try It Yourself

1. Write a short program using `std.sql` that stores five sensor readings in `:memory:`, filters readings above a threshold with `sql_query_params`, and exports matching rows as CSV via `std.csv.csv_emit_row`.
2. Hash a string with `std.crypto.hash_sha256`, verify its hex length (`64`), and test both matching and mismatched `hmac_verify` keys.
3. Build a 20-line chart with `std.svg` (`svg_new` -> `svg_circle` -> `svg_save`) and open the generated `.svg` file.

---

## Chapter 12: Native Compilation, FFI, and Bare-Metal Embedded

### 12.1 Inside the Compiler Pipeline and SSA IR

IRIS compiles source text into a block-parameter SSA IR (`IrModule`, inspired by MLIR—using basic block parameters rather than phi nodes) and transforms it through a **20-pass standard pipeline** (`build_standard_pipeline()` in `src/pass/mod.rs`):

| Order | Pass (`--dump-ir-after` name) | Responsibility |
|---|---|---|
| 1 | `ValidatePass` (`validate`) | Verifies structural SSA invariants and terminator consistency |
| 2 | `TypeInferPass` (`type-infer`) | Resolves remaining local types across blocks and expressions |
| 3 | `MonoPass` (`mono`) | Monomorphizes generic functions and const-generic records |
| 4 | `CfgBorrowCheckerPass` (`cfg_borrow_checker`) | Runs CFG non-lexical lifetime borrow, move, and projection checks |
| 5 | `DropElaborationPass` (`drop_elaboration`) | Emits explicit `EnterScope`, `ExitScope`, and `Drop` instructions |
| 6 | `ShapeCheckPass` (`shape-check`) | Verifies tensor ranks and symbolic dimensions statically |
| 7 | `InlinePass` (`inline`) | Inlines small hot functions |
| 8 | `ConstFoldPass` (`const-fold`) | Folds safe constants (deferring overflowing ops to trap with `E0409`) |
| 9 | `CopyPropPass` (`copy-prop`) | Eliminates trivial copies |
| 10 | `TailCallPass` (`tail-call`) | Lowers self-recursive tail expressions into SSA loops before LICM |
| 11 | `LicmPass` (`licm`) | Hoists loop-invariant computations (seeing new TCO loops!) |
| 12 | `LoopUnrollPass` (`loop-unroll`) | Unrolls small fixed-trip loops |
| 13 | `StrengthReducePass` (`strength-reduce`) | Simplifies expensive integer/address operations |
| 14 | `MlFusionPass` (`ml-fusion`) | Cancels double transposes/negations and collapses redundant reshapes |
| 15 | `OptPass` (`opt`) | Dead code elimination, common subexpressions, op expansion |
| 16 | `ExhaustivePass` (`exhaustive`) | Checks pattern-matching exhaustiveness (`E0210`) |
| 17 | `GcAnnotatePass` (`GcAnnotate`) | Inserts RC `retain` / `release` calls at last use |
| 18 | `FramePoolScopePass` (`frame_pool_scope`) | Guarantees `iris_ui_pool_destroy` runs on `Return`/`Panic` paths |
| 19 | `IrLintPass` (`lint`) | Emits structural IR diagnostics |
| 20 | `AsyncLowerPass` (`async-lower`) | Lowers `async def` functions into pollable state machines after GC annotation |

For native executables, IRIS lowers SSA IR to LLVM IR, compiles in-process via the dynamically loaded LLVM-C API into an object file, and links against the embedded C runtime (`src/runtime/iris_runtime.c`) using `ld.lld` directly on Windows MinGW (with system `clang` fallback where direct linking is absent).

---

### 12.2 Inspecting IR, LLVM, SIMD, and Profile-Guided Optimization

The `iris` compiler exposes every stage directly via `--emit` and optimization flags:

```text
iris prog.iris --emit ir              # Optimized SSA IR (default emit kind)
iris prog.iris --emit llvm            # Lowered LLVM IR text for the module
iris prog.iris --emit llvm-complete   # Complete LLVM IR with runtime declarations
iris prog.iris --emit simd            # Vectorized LLVM IR for par for loops
iris prog.iris --emit graph           # DOT dependency / model graph
iris prog.iris --emit jit             # In-process LLVM ORC JIT execution
iris prog.iris --emit binary -o app   # Standalone native executable
iris prog.iris --dump-ir-after licm   # Dump IR right after a named pass
```

For maximum hot-loop throughput on native builds:

1. **Vectorization (`--simd`)**: emits explicit AVX2/NEON vector kernels for `par for` and enables LLVM loop vectorization (`--emit simd` inspects the LLVM IR).
2. **Profile-Guided Optimization (`--pgo-generate` / `--pgo-use`)**:
   ```text
   iris build compute.iris --pgo-generate -o compute_inst
   ./compute_inst                    # Produces iris.profile (or IRIS_PROFILE_FILE)
   iris build compute.iris --pgo-use iris.profile -o compute_fast
   ```

---

### 12.3 Foreign Function Interface and Bindgen

IRIS integrates with C, C++, Rust, and Python cleanly across four layers:

1. **Direct C Declarations (`extern def`)**: declare C symbols and link shared or static libraries.
2. **Dynamic Loading & Out-Cells (`std.ffi`)**: open shared libraries at runtime (`lib_open`, `lib_call`, `call_i64`, `call_f64`, `call_str`, `lib_close`) and exchange primitive pointers via manual memory cells (`out_i64_cell`, `out_set_i64`, `out_i64`, `out_free`):

```iris
bring std.ffi

def main() -> i64 {
    val cell = out_i64_cell(2)
    assert(cell != 0)
    out_set_i64(cell, 0, 42)
    out_set_i64(cell, 1, 100)
    assert(out_i64(cell, 0) == 42 && out_i64(cell, 1) == 100)
    out_free(cell)
    println("ffi cells: ok")
    return 0
}
```

3. **Automated Header Binding (`iris bindgen`)**: generate `.iris` FFI wrappers directly from C/C++ headers (including C++ classes/overloads via `--cpp --shim-out shim.cpp`):
   ```text
   iris bindgen sensor_api.h -l sensor -o sensor_ffi.iris
   ```

4. **CPython Extension Export (`--emit python-ext`)**: compile an IRIS module directly into a native Python module (`.pyd` on Windows, `.so` on Unix) importable from Python:
   ```text
   iris my_kernels.iris --emit python-ext -o my_kernels.pyd
   ```

---

### 12.4 Freestanding Microcontroller Bundles

For deterministic firmware without an operating system or heap allocator, `iris embedded` compiles a strictly verified allocation-free scalar subset into relocatable object files and C board headers:

```text
iris embedded control_loop.iris --target cortex-m4f
iris embedded control_loop.iris --target cortex-m33
iris embedded control_loop.iris --target esp32-c3
iris embedded uno_button_led.iris --target arduino-uno
```

Each invocation generates:

- `iris_module.ll` and `iris_module.o` (verified to contain zero heap-allocator symbols)
- `iris_embedded_runtime.o` and `iris_embedded_harness.o`
- `iris_embedded.h` (declaring bounded GPIO, ADC, PWM, monotonic time, watchdog, and UART BSP hooks)
- `iris_embedded.json` (cryptographic proof fingerprint + static `.bss` / fixed-array footprint)

Top-level mutable scalar variables (`var presses: i64 = 0`, `f64`, or `bool`) persist across hardware control steps inside `.bss` without dynamic allocation:

```iris
// illustrative bare-metal controller pattern (compile via `iris embedded --target cortex-m4f`)
extern def iris_embedded_gpio_read(pin: i64) -> i64 effect io
extern def iris_embedded_gpio_write(pin: i64, level: i64) -> i64 effect io

var last_state: i64 = 0
var toggle_count: i64 = 0

def main() -> i64 effect io {
    val now = iris_embedded_gpio_read(2)
    if now == 1 && last_state == 0 {
        toggle_count = toggle_count + 1
    }
    last_state = now
    iris_embedded_gpio_write(13, toggle_count % 2)
    return 0
}
```

> **Embedded Target Limits**:
> - **Arduino Uno (`avr-unknown-unknown`)**: scalar control flow, top-level state, checked arithmetic, and board hooks are physically validated; cyclic CFGs and fixed arrays are rejected on AVR under LLVM 17 safety guards.
> - **Cortex-M4F, Cortex-M33, ESP32-C3**: ELF object bundles, fixed-array footprints, and host simulations are tested; physical hardware board reports (`IRIS-HW/1 ...`) remain required before claiming target silicon validation.
> - **ESP32 (Xtensa)**: requires Espressif's LLVM distribution (stock LLVM reports the missing backend cleanly).

#### Common Mistakes

- **Allocating strings, `list`, `record`, `choice`, or closures inside `iris embedded` code**: the allocation-freedom proof rejects all heap constructs cleanly at compile time.
- **Forgetting `out_free(cell)` after `out_i64_cell` / `out_new`**: manual FFI scratch buffers live outside RC tracking unless freed explicitly (though `ui_native_*` hosts reuse internal scratch cells automatically).

#### Try It Yourself

1. Inspect the SSA IR and LLVM IR emitted for one of your functions with `iris file.iris --emit ir` vs `iris file.iris --emit llvm`.
2. Dump IR right after tail-call optimization using `iris file.iris --dump-ir-after tail-call --emit ir`.
3. Compile `examples/embedded/control_loop.iris` with `iris embedded examples/embedded/control_loop.iris --target cortex-m4f -o out_m4f` and inspect `out_m4f/iris_embedded.json` and `out_m4f/iris_embedded.h`.


---

# Part IV: Differentiable Programming, ML, and Autonomy

## Chapter 13: Automatic Differentiation

### 13.1 Forward-Mode Dual Numbers and Reverse-Mode Tapes

Differential programming is a core capability of IRIS rather than an external monkey-patched runtime. IRIS exposes **two complementary automatic differentiation modes**:

1. **Forward-mode dual numbers (`grad<T>`)**: ideal when differentiating from few inputs to many outputs, carrying primal and tangent values cleanly through arithmetic expressions and `@differentiable` functions (`grad<f64>`).
2. **Reverse-mode Wengert tapes (`tape`, `backward`, `grad`)**: ideal when optimizing a scalar loss or objective with respect to arbitrarily many parameters in a single backward sweep—recording operations dynamically across functions, `if`/`else` branches, and `for`/`while` loops.

Use `tape(x)` when solving inverse problems, trajectory optimization, policy optimization, or neural training; reach for `grad<f64>` when computing exact directional derivatives on small analytic formulas.

---

### 13.2 Reverse-Mode Scalar Autodiff with Tape, Backward, and Grad

Promote any floating-point input onto the dynamic chunked tape with `tape(value)`. All standard arithmetic (`+`, `-`, `*`, `/`, unary `-`) and scalar math intrinsics (`sqrt`, `sin`, `cos`, `exp`, `log`, `abs`) record chain-rule nodes automatically—even across helper calls, branches, and loop iterations:

```iris
def squared_prediction(weight: f64, x: f64) -> f64 {
    val pred = weight * x
    return pred * pred
}

def main() -> i64 {
    val weight = tape(1.0)
    var loss = tape(0.0)

    // loss = sum_{k=1..4} (weight * k)^2 = weight^2 * (1 + 4 + 9 + 16) = 30 * weight^2
    // d(loss)/d(weight) at weight = 1.0 is 60.0
    for sample in 1..5 {
        loss = loss + squared_prediction(weight, to_f64(sample))
    }

    backward(loss)
    val slope = grad(weight)
    assert(slope == 60.0)
    println(f"scalar autodiff: dL/dw = {slope}")
    return 0
}
```

When designing differentiable physics rollouts or multi-step simulations for reverse-mode AD, keep two numerical hygiene habits in mind:

- **Guard denominators against zero**: add a small softening term before `sqrt(...)` (for example `sqrt(dx * dx + dy * dy + 0.002)`) so `1 / (2 * sqrt(r2))` never emits `NaN` during `backward(...)`.
- **Initialize step-by-step accumulators cleanly**: write `0.000001` or `1.0e-6` with a decimal point before `e` when tuning tolerances and `Adam` epsilons.

---

### 13.3 Differentiable Tensors in Standard Tensor

When optimizing vectors, matrices, or neural layers rather than scalar parameters, import `std.tensor` and build graphs with `DiffTensor`:

```iris
bring std.tensor

record SquaredLayer {
    marker: i64,
}

trait Layer {
    def forward(self, input: DiffTensor) -> DiffTensor effect alloc
}

impl Layer for SquaredLayer {
    def forward(self, input: DiffTensor) -> DiffTensor effect alloc {
        return tensor_ad_mul(input, input)
    }
}

def main() -> i64 {
    val shape: list<i64> = list(2)
    val weights = tensor_ad_parameter(tensor_ad_from_data(list(1.0, 2.0), shape))
    val factor = tensor_ad_from_data(list(2.0, 3.0), shape)

    var total = tensor_ad_from_data(list(0.0, 0.0), shape)
    for _step in 0..3 {
        total = tensor_ad_add(total, tensor_ad_mul(weights, factor))
    }

    val layer = SquaredLayer { marker: 0 }
    val output = layer.forward(total)
    tensor_ad_backward(tensor_ad_sum(output))

    val g = tensor_ad_data(tensor_ad_grad(weights))
    assert(list_get(g, 0) == 72.0)
    assert(list_get(g, 1) == 324.0)
    println("tensor autodiff: ok")
    return 0
}
```

Notice how seamlessly `DiffTensor` integrates with IRIS traits (`trait Layer`) and loops—while executing identically on the interpreter and native LLVM binaries.

---

### 13.4 Optimization Loops: Gradient Descent and Adam

Combining `tape`, `backward`, and `grad` inside an iterative loop turns any physical simulation into an inverse-problem solver. Each optimization step starts fresh variables on the tape, computes loss, calls `backward(loss)`, reads `grad(...)`, and updates ordinary plain-`f64` weights:

```iris
def target_residual(x: f64) -> f64 {
    val err_val = x - 3.0
    return err_val * err_val
}

def main() -> i64 {
    var x = 0.0
    val lr = 0.25

    for _iter in 0..20 {
        val xt = tape(x)
        val loss = target_residual(xt)
        backward(loss)
        x = x - lr * grad(xt)
    }

    assert(x > 2.999 && x < 3.001)
    println(f"gradient descent converged: x = {x}")
    return 0
}
```

For multi-parameter non-convex landscapes (like optimizing an orbital slingshot velocity vector `(vx, vy)` around multiple gravity wells in `projects/grav_forge`), swap vanilla `lr * g` for first- and second-moment Adam updates (`m = beta1 * m + (1 - beta1) * g`, `v = beta2 * v + (1 - beta2) * g * g`) alongside learning-rate schedules and gradient clipping.

#### Common Mistakes

- **Calling `grad(x)` before `backward(loss)`**: gradients populate only after running the reverse pass from the scalar `loss`.
- **Trying to feed `tape(...)` values directly across outer epochs without re-taping**: re-wrap current `f64` parameters with `tape(param)` at the start of each optimization iteration so previous epoch graphs are reclaimed cleanly.
- **Writing `sqrt(x * x + y * y)` without softening**: if `x == 0 && y == 0`, the derivative of `sqrt(0)` diverges (`Inf`/`NaN`). Add a positive `eps` inside `sqrt(...)`.

#### Try It Yourself

1. Minimize the Rosenbrock valley $f(x, y) = (1 - x)^2 + 100 (y - x^2)^2$ starting from `(-1.0, 1.0)` using `tape` + `backward` + Adam and assert final distance from `(1.0, 1.0)` is below `0.01`.
2. Differentiate `loss = sin(w * x) + 0.1 * w * w` in a multi-sample loop and assert the gradient against a finite-difference check `(f(w + eps) - f(w - eps)) / (2.0 * eps)`.
3. Train a tiny single linear layer using `std.tensor` and `std.nn.nn_ad_linear` / `nn_ad_squared_error`.

---

## Chapter 14: Tensors, Neural Networks, and Model Export

### 14.1 Typed Tensors and Compile-Time Shape Checking

Static shape safety eliminates one of the most frequent sources of runtime failures in numerical and ML code. IRIS represents dense multi-dimensional tensors with element type and symbolic dimensions in the type system:

```iris
// Compile-checked tensor multiplication: [M, K] @ [K, N] -> [M, N]
def linear_project(
    a: tensor<f32, [M, K]>,
    b: tensor<f32, [K, N]>,
) -> tensor<f32, [M, N]> {
    return a @ b
}
```

During compilation, **`ShapeCheckPass`** validates rank compatibility, contraction dimensions (`[M, K] @ [K, N]`), element-wise shape alignment, and scalar broadcasting—producing diagnostic `E0204` before a binary is ever emitted if two matrix dimensions disagree.

You can also move between dense arrays and sparse encodings via `sparsify(...)` / `densify(...)` (`sparse<T>`).

---

### 14.2 Neural Layers, Classical ML, Datasets, and RL

Beyond low-level `tensor<...>` types, the standard library equips you with higher-level training and evaluation modules:

- **`std.tensor`**: `DiffTensor`, matrix multiplication, reductions (`tensor_ad_sum`), activations, and gradient extraction.
- **`std.nn`**: differentiable layers (`nn_ad_linear`, `nn_ad_squared_error`) as well as multi-layer perceptron helpers (`mlp_create`, `mlp_forward`, `mlp_backward`).
- **`std.dataset`**: normalization, splitting, shuffling, and batching (`dataset_mean`, `dataset_std`, `dataset_normalize`, `dataset_split`, `dataset_get_batch`).
- **`std.ml`**: classical ML records, tensor pairs (`MLTensor`, `tensor_pair`), production health telemetry (`model_health_new`, `model_health_observe`, `model_health_error_rate`), and model versioning (`model_registry_new`, `model_registry_add`).
- **`std.rl`**: replay buffers (`Experience`, `ReplayBuffer`, `replay_buffer_new`, `replay_buffer_push`, `replay_buffer_sample`) and tabular Q-learning (`q_table_new`, `q_learning_update`).

Here is a runnable differentiable linear layer built cleanly on `std.tensor` and `std.nn`:

```iris
bring std.tensor
bring std.nn

def main() -> i64 {
    val input = tensor_ad_from_data(list(1.0, 2.0), list(1, 2))
    val weights = tensor_ad_parameter(tensor_ad_from_data(list(3.0, 4.0), list(2, 1)))
    val bias = tensor_ad_from_data(list(1.0), list(1, 1))
    val target = tensor_ad_from_data(list(0.0), list(1, 1))

    val pred = nn_ad_linear(input, weights, bias)
    val loss = nn_ad_squared_error(pred, target)
    tensor_ad_backward(loss)

    val g = tensor_ad_data(tensor_ad_grad(weights))
    assert(list_get(tensor_ad_data(pred), 0) == 12.0)
    assert(list_get(g, 0) == 24.0 && list_get(g, 1) == 48.0)
    println("neural layer: ok")
    return 0
}
```

---

### 14.3 The Declarative Model DSL and Graph Fusion

Neural architectures can be declared directly as top-level `model` graphs using contextual keywords (`input`, `layer`, `output`):

```iris
// Inspect graph: iris model.iris --emit graph
// Export ONNX:   iris model.iris --emit onnx-binary -o classifier.onnx
model Classifier {
    input features: tensor<f32, [1, 4]>
    layer hidden Dense(units = 8)
    layer activated ReLU(hidden)
    output activated
}
```

When compiling tensor graphs and expressions, **`MlFusionPass`** automatically cancels redundant identity transposes, fuses consecutive transposes, collapses no-op reshapes, and simplifies double negations at the IR level before code generation.

---

### 14.4 ONNX, TensorRT Export, and Dynamic ML Backends

IRIS connects seamlessly to industry inference engines in two directions:

1. **Exporting IRIS `model` definitions**:
   - `--emit graph` outputs an inspectable DOT/graph representation.
   - `--emit onnx` and `--emit onnx-binary [--weights FILE]` serialize the network directly to ONNX protobuf format (injecting trained weights from JSON `"layer.weight": [...]` or seeded initializers).
   - `--emit tensorrt [--weights FILE]` builds a serialized TensorRT engine via `trtexec`. *(Audit note: `trtexec` CLI invocation is covered by automated tests, while physical execution on hosts with NVIDIA TensorRT installed has not yet been run.)*
2. **Importing trained models at runtime (`std.ml`)**:
   - IRIS discovers **ONNX Runtime**, **TensorFlow C API**, and **LibTorch** dynamically via `dlopen` on first use (`onnx_load`/`onnx_run`, `pytorch_load`/`pytorch_run`, `torch_load`/`torch_run`), carrying zero compile-time SDK dependency when unused.
   - Pass `IRIS_ML_STATIC_LINK=1` if you explicitly want direct static linking (`MlLinkMode::StaticDirect`), and `IRIS_USE_BLAS=1` to accelerate matrix multiplication with OpenBLAS, Intel MKL, or macOS Accelerate.

---

### 14.5 LLM Tool Calling and Structured Agents

For modern agentic services talking to local runtimes (Ollama, vLLM, llama.cpp) or remote OpenAI-compatible endpoints, `std.llm` provides typed message envelopes, schema-checked tool calls, embeddings, and JSON serialization:

```iris
bring std.llm

def main() -> i64 {
    val messages: list<LlmMessage> = list(llm_message("user", "Say \"hello\""))
    val client = llm_client(
        "http://127.0.0.1:8080/v1/chat/completions",
        "http://127.0.0.1:8080/v1/embeddings",
        "local-model",
        "",
        5000,
    )

    val req = llm_request_json(client, messages, 0.0, 64, "")
    assert(contains(req, "\"model\":\"local-model\""))

    val resp = llm_response_from_json(
        200,
        "{\"choices\":[{\"message\":{\"content\":\"hello\"}}]}",
    )
    assert(is_ok(resp) && unwrap(resp).content == "hello")
    assert(is_err(llm_response_from_json(503, "unavailable")))
    println("llm protocol: ok")
    return 0
}
```

To make agent tool execution deterministic and safe, decode incoming JSON tool arguments into validated typed IRIS `record` structs before running domain logic (`projects/llm_agent`).

#### Common Mistakes

- **Assuming `model` DSL blocks define an imperative `main()` entry point**: inspect or export pure `model Name { ... }` files with `--emit graph`, `--emit onnx`, or `--emit onnx-binary`.
- **Enabling HTTPS LLM endpoints on non-Windows native builds without checking TLS**: inspect `http_tls_available()` or target a local plaintext HTTP sidecar when deploying Linux/macOS agents natively until non-Windows TLS is locked.

#### Try It Yourself

1. Save the `Classifier` `model` above to `classifier.iris` and run `iris classifier.iris --emit graph` and `iris classifier.iris --emit onnx` to inspect the generated computation graph.
2. Normalize a synthetic 1D measurement list with `std.dataset.dataset_normalize` and verify unit-scale statistics with `std.math.mean` and `std.math.std_dev`.
3. Explore `projects/learning_service/main.iris` to see deterministic training, model versioning, and health degradation tracking working together.

---

## Chapter 15: Autonomous Intelligent Systems, Robotics, and Governed Evolution

### 15.1 Active Inference, Uncertainty, Adaptive Control, and Lifecycle

IRIS was engineered for long-lived autonomous and homeostatic software. Four domain modules capture online statistics, epistemic uncertainty, adaptation, and degradation handling cleanly:

- **`std.ais`**: running Welford statistics (`RunningStats`), generative world models (`WorldModel`), viable homeostatic envelopes, genomic representation (`Genome`), Elastic Weight Consolidation (`ewc_penalty_grad`), and agent health states (`agent_runtime_new`, `agent_record_outcome`). Specific tested active-inference capabilities are demonstrated in `projects/gridworld_inference` (tabular, fully observed, seeded runs of Dirichlet exploration + expected free energy).
- **`std.uncertainty` & `std.adaptive`**: closed-form Normal-Inverse-Gamma conjugate Bayesian posteriors (`nig_prior`, `nig_update`, `nig_predictive_variance`, `nig_credible_interval`) and online risk monitors (`RiskMetrics`, `Uncertainty`).
- **`std.meta_learning`**: fast inner-loop MAML and Reptile adaptation helpers (`maml_inner_update`, `maml_eval`, `reptile_step`).

```iris
bring std.ml
bring std.ais
bring std.uncertainty

def main() -> i64 {
    // Bayesian uncertainty update over noisy telemetry
    var post = nig_default()
    post = nig_update_batch(post, list(10.0, 10.2, 9.8, 10.1))
    assert(nig_mean(post) > 9.9 && nig_mean(post) < 10.1)

    // Production agent degradation monitoring
    var agent = agent_runtime_new(2)
    agent = agent_record_outcome(agent, 1.0, -1.0, false)
    agent = agent_record_outcome(agent, 1.0, -1.0, false)
    assert(agent.state == "degraded")

    println("ais and uncertainty: ok")
    return 0
}
```

---

### 15.2 ROS 2 Robotics Integration

`std.ros2` provides direct robotics primitives—geometry transforms (`Vector3`, `Quaternion`, `Pose`, `Transform`, `transform_point`, `transform_inverse`), QoS profiles (`qos_sensor_data`, `qos_control`), and live middleware bindings to **`rclcpp`**, **`rclcpp_action`**, **`rclcpp_lifecycle`**, and **`tf2_ros`** via a thin C-ABI bridge (`iris_ros2.dll` / `libiris_ros2.so`).

Offline geometry and QoS helpers work anywhere with zero ROS installation:

```iris
bring std.ros2

def main() -> i64 {
    val tf = Transform {
        translation: Vector3 { x: 1.0, y: 2.0, z: 3.0 },
        rotation: Quaternion { x: 0.0, y: 0.0, z: 0.0, w: 1.0 },
    }
    val pt = transform_point(tf, Point { x: 1.0, y: 1.0, z: 1.0 })
    val orig = transform_point(transform_inverse(tf), pt)

    assert(pt.x == 2.0 && pt.y == 3.0 && pt.z == 4.0)
    assert(orig.x == 1.0 && qos_sensor_data().depth == 5)
    println("ros2 geometry: ok")
    return 0
}
```

> **ROS 2 Verification Boundary**:
> Live `rclcpp` topics, services, actions, parameters, lifecycle nodes, and `tf2` are verified on Windows Humble (Cyclone DDS) and Linux Humble (Fast DDS), including `projects/turtlesim_autopilot`, `projects/lifecycle_sensor_fusion`, and `projects/evolving_turtle`. macOS builds remain untested, each context uses one single-threaded executor, and tests run against simulators rather than physical robot hardware.

---

### 15.3 Typed Metaprogramming and Reflection

Programs running under the compiler/interpreter host can inspect AST/IR metadata, type-check candidate patches through the real Rust compiler pipeline, and evaluate modified fragments dynamically via **`std.meta`** and **`std.reflect`** (plus the `iris meta` subcommand):

```iris
// Run under compiler host (`IRIS_FORCE_INTERP=1 iris --emit eval ...`)
bring std.meta
bring std.reflect

def main() -> i64 {
    assert(meta_available())
    val source = "def main() -> i64 { return 0 }"
    val prog = meta_program(source)
    assert(is_ok(meta_analyze(prog)))

    val edit = MetaEdit { start_byte: 27, end_byte: 28, replacement: "7" }
    val edited = unwrap(meta_apply(prog, edit))
    assert(unwrap(reflect_eval_i64(edited)) == 7)
    println("metaprogramming: ok")
    return 0
}
```

In standalone ahead-of-time native binaries, `meta_available()` and `reflect_available()` return `false` cleanly (unless hosted via compiler/FFI evolution APIs) rather than silently shipping an unverified embedded compiler.

---

### 15.4 Seven-Gate Governed Self-Evolution

A system that updates its own logic in production cannot rely on guesswork or unguarded eval loops. IRIS's **`iris evolve`** command enforces a mandatory **seven-gate verification protocol** for policies (`i64`, `f64:N`, or `record` ABIs) and whole stateful programs (`--manifest program.json --scenarios scenarios.json`):

1. **Gate 1 — Language safety**: full parser, type, NLL borrow, strict effect, and RC-safety validation passes cleanly.
2. **Gate 2 — Resources & ABI**: source size, function count, IR block/instruction budgets, and exact binary ABI signatures match the active generation.
3. **Gate 3 — Deterministic tests**: candidate tests execute cleanly inside the bounded interpreter/sandbox without hangs or assertion traps.
4. **Gate 4 — Transactional rollback**: `test_transaction_rollback()` passes, proving speculative mutation restoration works cleanly.
5. **Gate 5 — Safety constitution**: SHA-256-pinned immutable safety rules (`--constitution` + `--constitution-sha256`) and `--min-output` / `--max-output` / `--bound` invariants evaluate every candidate input/output pair outside candidate code.
6. **Gate 6 — Shadow canary**: baseline and candidate run identical canary cases; promotion requires strictly improved pass counts and lower aggregate error.
7. **Gate 7 — Verified activation & audit chain**: LLVM ORC JIT compiles the verified candidate, atomically activates a lease-guarded generation (preserving 8 rollback generations by default via `HotSwapLease`), and appends a canonical hash-chained JSONL entry verified against an independently stored `--audit-head` checkpoint (with optional HMAC-SHA256 authentication).

Related subcommands extend governed evolution across continuous loops (`iris evolve-live`), live state-carrying HTTP services (`iris evolve-serve`), genetic AST policy search (`iris evolve-search`), SLA-monitoring autonomic daemons (`iris service-daemon`), and explicit opt-in unrestricted host replacement (`iris evolve-unrestricted`, documented separately for self-modifying host engines).

#### Common Mistakes

- **Expecting `std.meta` inside a standalone `iris build` binary**: compiler-hosted metaprogramming returns `meta_available() == false` in AOT binaries; run hosted edits with `IRIS_FORCE_INTERP=1` or drive evolution via `iris evolve` / `crates/iris_ffi`.
- **Storing `--audit` and `--audit-head` in the same untrusted directory**: for production threat models, store `--audit-head` checkpoints and HMAC keys in an isolated trust domain.

#### Try It Yourself

1. Compute conjugate posterior intervals for a noisy synthetic telemetry stream using `std.uncertainty.nig_update_batch` and `nig_credible_interval`.
2. Inspect `examples/09_evolution/typed_edit.iris` under `IRIS_FORCE_INTERP=1 iris --emit eval examples/09_evolution/typed_edit.iris`.
3. Walk through `projects/autonomous_evolution_lab/` to watch all seven evolution gates approve a superior controller and reject an unsafe candidate.


---

# Part V: Native Desktop Applications

## Chapter 16: Building Native Desktop UIs with Standard UI

### 16.1 Architecture: IRIS Immediate-Mode Trees and Retained Native Host

IRIS provides a built-in native GUI stack through **`std.ui`** backed by **`crates/iris_ui_host`**, compiled as a shared library (`iris_ui_host.dll` on Windows, `libiris_ui_host.so` on Linux, `libiris_ui_host.dylib` on macOS). Rather than embedding a browser engine or wrapping bloated external frameworks, IRIS splits responsibilities cleanly:

- **Pure IRIS layer (`std.ui*`)**: constructs immediate-mode widget trees (`UiTree`), runs iterative flexbox/grid box-constraint layout (`BoxConstraints`), hit-tests pointer/keyboard events into `UiState`, animates springs (`std.ui_motion`), and encodes compact binary vector commands (`DrawBuffer`).
- **Native Rust host (`iris_ui_host`)**: manages OS windows and DPI scaling (`winit`), presents pixels with zero-allocation framebuffer reuse (`softbuffer`), rasterizes 2D anti-aliased paths, rounded rectangles, gradients, shadows, SVG icons, arcs, and live camera frames (`tiny-skia` SIMD), shapes text cleanly (`cosmic-text` layout cache), and synchronizes screen-reader accessibility trees (`accesskit`).

Importing `bring std.ui` transitively re-exports all six UI submodules (`std.ui_theme`, `std.ui_protocol`, `std.ui_motion`, `std.ui_icons`, `std.ui_layout`, and `std.ui_widgets`). Because layout and rendering are separated cleanly from OS event pumping, you can render any UI tree straight to an **SVG string (`ui_render_svg`)**, an **HTML mirror**, or an **offscreen PNG snapshot (`--headless-snapshot`)** for automated tests without opening a visible desktop window.

---

### 16.2 Scaffolding a Desktop App and Running the Frame Loop

Start a desktop application cleanly using the `ui` project template:

```text
iris new ui mission_console
cd mission_console
cargo build --release -p iris_ui_host   # once in workspace root if building from source
iris build --gui main.iris -o mission_console
```

Even without opening a window, `std.ui` trees construct, lay out, and serialize to vector SVG cleanly:

```iris
bring std.ui

def main() -> i64 {
    val tree = ui_new()
    val header = ui_heading(tree, "hdr", "IRIS Mission Console", UiTone.Accent)
    val btn = ui_button(tree, "btn_arm", "Arm Telemetry", "act:arm")
    val card = ui_card(tree, "main_card", UiStyle { gap: 10.0 }, list(header, btn))
    val laid_out = ui_layout(ui_set_root(tree, card), 640.0, 360.0)

    val svg = ui_render_svg(laid_out)
    assert(contains(svg, "<svg"))
    assert(contains(svg, "IRIS Mission Console"))
    println("std.ui headless tree and svg: ok")
    return 0
}
```

For full interactive desktop windows, simple state machines can use `UiAppConfig` + `ui_run_app` with explicit closure parameter annotations (`|st: UiState, tree: UiTree| render_fn(st, tree)`). Multi-panel applications with custom application records drive an explicit `while ui_native_is_open(lib)` frame loop paired with `ui_frame_pool_begin()` (detailed in Section 16.6).

---

### 16.3 Responsive Layout Containers and Breakpoints

IRIS organizes widgets into nested layout containers driven by `UiStyle` constraints (`padding`, `gap`, `flex`, `align`, `min_w`, `min_h`, `max_w`, `max_h`):

| Container | Role |
|---|---|
| `ui_col(tree, key, style, children)` | Vertical flex stack |
| `ui_row(tree, key, style, children)` | Horizontal flex container |
| `ui_stack(tree, key, style, children)` | Shared-bounds layered overlay container |
| `ui_card(tree, key, style, children)` | Bordered surface container using active theme tokens |
| `ui_wrap(tree, key, style, children)` | Responsive wrapping row |
| `ui_grid(tree, key, cols, gap, children)` | Fixed/adaptive column grid |
| `ui_split_pane(tree, key, ratio, left, right)` | Proportional two-pane workspace divider |
| `ui_spacer(tree, flex, min_w, min_h)` | Elastic layout spacer |
| `ui_divider(tree, label)` | Section divider with optional inline caption |

Adapt layouts automatically across window widths with **`UiBreakpoint`** (`Compact`, `Medium`, `Expanded`, `Wide`), `ui_breakpoint(width)`, and `ui_responsive_cols(width, compact, medium, expanded)`:

```iris
bring std.ui

def main() -> i64 {
    val narrow_cols = ui_responsive_cols(520.0, 1, 2, 4)
    val wide_cols = ui_responsive_cols(1280.0, 1, 2, 4)
    assert(narrow_cols == 1)
    assert(wide_cols == 4)
    println("responsive layout helpers: ok")
    return 0
}
```

---

### 16.4 Built-In Themes, Vector SVG Icons, and Core Widgets

`std.ui_theme` ships **six curated color and elevation palettes** out of the box—every widget reads semantic theme colors (`bg`, `panel`, `border`, `text`, `muted`, `accent`, `teal`, `warn`, `danger`) so runtime theme switching requires one function swap:

- `obsidian_theme()` — canonical dark IRIS studio palette
- `daylight_theme()` — crisp high-readability light palette
- `midnight_theme()` — deep slate navy studio theme
- `nord_theme()` — cool Arctic blue-gray palette
- `emerald_studio_theme()` — high-contrast emerald command theme
- `high_contrast_theme()` — accessible high-contrast palette

You can also derive custom accents (`theme_with_accent(theme, accent)`), parse hex colors (`hex_rgb`), or blend palette stops (`mix_color`, `lighten`, `darken`, `contrast_fg`).

#### Vector SVG Icons (`std.ui_icons`)

`std.ui_icons` defines **99 crisp 24×24 vector `UiIcon` variants** (`Search`, `Plus`, `Play`, `Pause`, `Settings`, `Brain`, `Network`, `Chart`, `Bolt`, `Shield`, `Terminal`, `Rocket`, `Cpu`, `Camera`, `Sun`, `Moon`, and more) rasterized directly via `draw_cmd_svg_path` at arbitrary DPI:

- Inside widget trees: `ui_icon(tree, key, icon, size, tone)` and `ui_icon_button(tree, key, icon, text, tone, action)`.
- Directly onto custom canvases: `draw_icon`, `draw_icon_styled`, `draw_svg_icon`, or lookup by name with `ui_icon_from_name("camera")`.

---

### 16.5 Interactive State, Forms, Modals, Drawers, and Toasts

Every interactive control carries a stable string `key` and emits typed action events folded into `UiState` via `ui_process_events`. Watch the exact signatures for common widgets:

- **Buttons**: `ui_button(tree, key, text, action)` (4 args) and `ui_button_tone(tree, key, text, action, tone)` (5 args).
- **Switches & Choices**: `ui_checkbox`, `ui_toggle`, `ui_radio`, `ui_select`, `ui_segmented`, `ui_tabs`, `ui_nav_item`, `ui_accordion`, `ui_chip`, `ui_menu`, `ui_stepper`.
- **Sliders**: `ui_slider` and `ui_slider_range`.
- **Text Entry**:
  - `ui_text_input(tree, key, text, placeholder, style)` — 5th parameter is `UiStyle`!
  - `ui_text_area(tree, key, text, placeholder, min_h)` — 5th parameter is `min_h: f64`!
- **Telemetry & Visualization**: `ui_stat_card` (`StatSpec`), `ui_bar_chart` (`BarChartSpec`), `ui_line_chart` (`ChartSeries`), `ui_sparkline`, `ui_progress_bar`, `ui_progress_ring`, `ui_spinner`, `ui_avatar`, `ui_breadcrumb`, `ui_image` (`ImageSpec`), `ui_radar_scope`.
- **Overlays & Transient Notifications**: `ui_modal(tree, key, title, open, action, children)`, `ui_drawer(...)`, and floating toast notifications via `ui_state_push_toast(...)`.

---

### 16.6 Smooth Springs, Animations, and Frame-Pool Memory Safety

Animate panels, numeric badges, glow opacities, and camera crosshairs smoothly using **`std.ui_motion`** (`SpringConfig` presets: `spring_snappy()`, `spring_smooth()`, `spring_bouncy()`, `spring_gentle()`, plus `UiEase`, `motion_cubic_bezier`, and `motion_lerp_rect`) integrated right into `UiState`:

- Trigger spring motion via `ui_state_spring_from` (after event folding).
- Advance clocks each frame via `val now = ui_native_clock_secs(lib)` -> `ui_state_tick` -> `ui_state_spring_value`.
- Check `ui_state_has_active_transitions` to decide whether to poll with a short frame timer (~16 ms for 60 FPS motion) or sleep longer when idle.

#### The Mandatory `ui_frame_pool` Lifecycle

As Section 8.5 established, record field assignments (`app.score = v`) allocate on whichever allocator is currently active. Follow this five-step sequence in every interactive `std.ui` host loop:

```iris
// illustrative desktop host frame loop (requires built `iris_ui_host` library)
bring std.ui

def run_window(lib: NativeUiLib) -> i64 {
    var buf = draw_buf_new(131072)
    var ui_st = ui_state_new()
    var clicks: i64 = 0

    while ui_native_is_open(lib) {
        val pool = ui_frame_pool_begin()
        val m = ui_native_metrics(lib)
        val tree = ui_new()
        val btn = ui_button(tree, "inc", f"Clicks: {clicks}", "act:inc")
        val root = ui_card(tree, "root", UiStyle { padding: 16.0 }, list(btn))
        val laid = ui_layout_with_state(ui_set_root(tree, root), ui_st, m.width, m.height)

        buf = ui_paint(laid, ui_st, buf)
        val _sub = ui_native_submit(lib, buf)
        val events = ui_native_poll(lib, 16)
        val next_st = ui_process_events(laid, ui_st, events)
        val _cur = ui_native_set_cursor(lib, next_st.active_cursor)

        // Pause pool BEFORE detaching UiState or updating persistent app state!
        val paused = ui_frame_pool_pause()
        ui_st = ui_state_detach(next_st)
        val activated = ui_st.activated_action
        val _res = ui_frame_pool_resume(paused)
        val _dst = ui_frame_pool_destroy(pool)

        if activated == "act:inc" {
            clicks = clicks + 1
        }
    }

    val _f = draw_buf_free(buf)
    return 0
}
```

Pause -> `ui_state_detach` -> extract surviving values -> resume -> destroy -> update app state. Keeping that order prevents use-after-free bugs completely even across thousands of live frames.

#### Common Mistakes

- **Mixing up the 5th argument of `ui_text_input` vs `ui_text_area`**: `ui_text_input` expects `UiStyle` whereas `ui_text_area` expects `min_h: f64`.
- **Re-allocating `draw_buf_new` every frame inside the loop**: allocate `var buf = draw_buf_new(...)` *once* before the loop and release it *once* after the loop (`draw_buf_free(buf)`).
- **Passing un-annotated closure parameters to `ui_run_app`**: write `|s: UiState, t: UiTree| render(s, t)` rather than untyped `|s, t|`.

#### Try It Yourself

1. Build a responsive 2-card SVG telemetry layout in pure IRIS (`ui_render_svg`) that switches between 1 column and 2 columns depending on `width`.
2. Scaffold a starter app via `iris new ui hello_gui`, build `iris_ui_host` in release mode, and run it in both desktop mode and `--headless-snapshot`.
3. Add a modal dialog (`ui_modal`) and a theme switcher cycling between `obsidian_theme()`, `nord_theme()`, and `daylight_theme()`.

---

## Chapter 17: Custom Vector Painting, Live Camera Capture, and Standalone Packaging

### 17.1 Custom Vector Canvas Painting with DrawBuffer

Standard widgets compose cleanly with hardware-fast, high-precision **2D vector canvases** emitted straight into `DrawBuffer` commands and rasterized by `iris_ui_host`:

| Command | Purpose |
|---|---|
| `draw_cmd_round_rect` / `draw_cmd_round_rect_corners` | Filled/bordered rounded rects with uniform or per-corner `UiRadii` |
| `draw_cmd_circle` / `draw_cmd_arc` | Solid rings, nodes, orbital arcs, sweep radars |
| `draw_cmd_line` / `draw_cmd_polyline` | Solid or dashed lines, waveforms, orbital trajectory trails |
| `draw_cmd_linear_gradient` / `draw_cmd_radial_gradient` | Multi-stop smooth shader fills (`UiGradientStop`) |
| `draw_cmd_box_shadow` | Soft elevation drop shadows |
| `draw_cmd_path` / `draw_cmd_svg_path` | Arbitrary vector paths and SVG `d` strings |
| `draw_cmd_text` | Font-shaped `cosmic-text` labels inside custom canvases |
| `draw_cmd_image_rgba` | Blit CPU RGBA pixel grids (heatmaps, neural activations) |
| `draw_cmd_push_clip` / `draw_cmd_push_round_clip` / `draw_cmd_pop_clip` | Rectangular and rounded scissor masks |
| `draw_cmd_push_transform` / `draw_cmd_pop_transform` | Affine translation, scaling, and rotation matrices |
| `draw_cmd_push_opacity` / `draw_cmd_pop_opacity` | Grouped alpha blending layers |

Because `ui_paint` writes all widget commands into a `DrawBuffer`, you can paint custom interactive simulation viewports right inside a widget card's layout rectangle—or compose full-window canvas layers beneath crisp HUD cards!

---

### 17.2 Live Native Camera Capture and Synthetic Sensor Simulation

IRIS desktop applications can open hardware webcams (and seamlessly fall back to a deterministic synthetic sensor feed in CI, VMs, or `--sim` mode) directly through **`std.ui_protocol`** and **`crates/iris_ui_host/src/camera.rs`**:

- **`ui_native_camera_open(lib, device_index, req_w, req_h, synthetic_only)`**: opens device `device_index` using Windows Media Foundation dynamically on Windows (or activates synthetic sensor stream when `synthetic_only = true` or no hardware camera is present), returning a positive 1-based `cam_id`.
- **`ui_native_camera_info(lib, cam_id) -> UiCameraInfo`**: reports live frame `width`, `height`, `mode` (`0=Offline`, `1=Live`, `2=Synthetic`), monotonic sequence `seq`, and OS `device_name` (for example `"Lenovo EasyCamera"`).
- **`draw_cmd_camera_frame(buf, rect, cam_id, mirror, tint_mode)`**: blits the latest decoded RGBA camera frame straight onto any `UiRect` inside `DrawBuffer` with optional mirroring and optical tint modes (`0=RGB`, `1=NVG`, `2=FLIR`, `3=Slate`).
- **`ui_native_camera_sample_grid(lib, cam_id, grid_w, grid_h, mirror, out_cell)`**: downsamples the latest frame into an FFI `out_cell` byte buffer (`4..=512` resolution) for real-time IRIS computer-vision loops (frame differencing, centroid saliency, optical motion scoring) without marshaling full-HD pixels through script objects every tick!
- **`ui_native_camera_set_sim_target(...)`** & **`ui_native_camera_close(lib, cam_id)`**: inject deterministic moving synthetic targets for headless integration tests and tear down capture cleanly.

---

### 17.3 Engineering High-Frame-Rate Native Interfaces

Getting smooth 60 FPS responsiveness on a CPU vector rasterizer comes down to avoiding wasted work in both IRIS script logic and `iris_ui_host`:

1. **Always stage the release `iris_ui_host`**: debug builds of `tiny-skia` / `cosmic-text` run roughly 10× slower than release builds. Build `cargo build --release -p iris_ui_host` and pass `--release` to `iris bundle` (note: modern IRIS prefers release `iris_ui_host` automatically during release bundles).
2. **Avoid redundant rounded-clip masks**: full-screen `PushRoundClip` masks allocate and composite pixel alpha masks across the entire viewport. Keep `PushClip` rectangular for uncurved viewports and paint vector grids/trails in batched `draw_cmd_polyline` commands rather than hundreds of tiny separate rect/line primitives.
3. **Sample camera grids only when `info.seq != last_cam_seq`**: skip re-running CV feature extraction when the camera background thread has not produced a newer frame sequence number.
4. **Poll events instead of blocking unconditionally during active motion**: use `ui_native_poll(lib, 0)` or short timeouts while `ui_state_has_active_transitions(ui_st)` or live simulation/camera capture is active, and longer waits when purely idle.
5. **Re-use `DrawBuffer` and detach cleanly**: pre-allocate `draw_buf_new(...)` outside the loop and never let persistent data leak onto the frame pool.

*(Reference benchmark from this codebase: after clip-mask dirty bounding, fast scanline camera blitting, and release host staging, `projects/target_takedown` renders 36 headless frames including camera decode, CV grid analysis, HUD widgets, and PNG encode in ~1.37 s / ~38 ms per frame, down from 14.67 s debug/unoptimized).*

---

### 17.4 Desktop Manifest Configuration and GUI Builds

Configure standalone desktop properties in the `[app]` section of `iris.toml`:

```toml
[package]
name = "grav_forge"
version = "1.0.0"
entry = "main.iris"

[dependencies]

[app]
title = "IRIS GravForge"
identifier = "io.iris.grav_forge"
publisher = "Moon9t"
icon = "assets/icon.png"
gui = true
bundle_ui_host = true
window_width = 1400
window_height = 860
min_width = 920
min_height = 580
resizable = true
categories = "Science;Graphics;"
assets = ["assets/presets.json"]
```

To compile a standalone GUI binary directly on Windows without spawning a console window behind it, use:

```text
iris build --gui main.iris -o MyApp.exe
```

Under the hood, `--gui` links the executable against the Windows GUI subsystem (`SUBSYSTEM:WINDOWS` / `mainCRTStartup`), while `--console` explicitly keeps a console attached during diagnostics.

---

### 17.5 Bundling, Installers, and Operating System Registration

When you are ready to ship an installable desktop application—complete with icon conversion (`.png`, `.ico`, `.icns`), asset staging, bundled `iris_ui_host` shared library, and OS metadata—use **`iris bundle`**:

```text
# Stage a self-contained portable bundle directory (or use --format zip)
iris bundle . --format dir --release -o dist/MyApp

# Generate platform native distributions
iris bundle . --format zip -o dist/MyApp.zip         # Portable zip archive
iris bundle . --format inno -o dist/MyApp-setup.iss  # Windows Inno Setup installer script
iris bundle . --format app -o dist/MyApp.app         # macOS .app bundle
iris bundle . --format appimage -o dist/MyApp.AppDir # Linux AppDir / AppImage structure

# Install onto the host OS (copies files + creates Start Menu / Registry / .desktop entries)
iris bundle . --install --release

# Cleanly remove an installed app (pass --no-register to skip shortcut/registry registration)
iris bundle . --uninstall
```

You can override manifest values from the command line with `--title`, `--identifier`, `--icon`, `--gui` / `--console`, and `--install-dir <DIR>`. Combined with `--headless-snapshot`, a CI pipeline can build the GUI binary, verify rendered pixels into PNG, package an archive or installer script, and smoke-test install/uninstall automatically.

#### Common Mistakes

- **Shipping an application executable without `iris_ui_host.dll` / `.so` / `.dylib` beside it**: either set `bundle_ui_host = true` in `[app]` and distribute with `iris bundle`, or copy the release `iris_ui_host` library next to your binary.
- **Debugging performance issues against debug `iris_ui_host`**: always build `cargo build --release -p iris_ui_host` and pass `--release` to `iris bundle`.
- **Adding `--headless-snapshot` without closing cleanly after a deterministic frame count**: support CLI flags (`--headless-snapshot [path]`, `--sim`) in desktop apps so CI and regression scripts can verify startup and rendering without manual clicks.

#### Try It Yourself

1. Paint a custom canvas scene combining a radial gradient background, a multi-point `draw_cmd_polyline` sine wave, a circular sweep `draw_cmd_arc`, and centered text using `DrawBuffer`.
2. Add a `--headless-snapshot [path]` flag to `hello_gui` that opens `UiHostMode.Headless`, submits 3 frames, calls `ui_native_save_png(lib, path)`, and exits cleanly with `0`.
3. Package a project into a portable bundle with `iris bundle . --format dir --release -o target/my_app_bundle` and launch the standalone binary directly inside that folder.


---

# Part VI: Engineering in IRIS

## Chapter 18: Packages, Testing, Benchmarking, and Debugging

### 18.1 Manifests, Lockfiles, and the Package Manager

Every multi-file IRIS project is anchored by an **`iris.toml`** manifest declaring the package name, version, entry point, optional registry URL, dependencies, and optional `[app]` desktop metadata:

```toml
[package]
name    = "telemetry_service"
version = "0.2.0"
entry   = "main.iris"

[dependencies]
utils  = { path = "../shared/utils" }
router = { git = "https://github.com/Eclipse-Softworks/iris-router.git", tag = "v1.0.0" }
json   = { registry = "json", version = "^1.0.0" }
```

Manage dependencies and reproducible builds via `iris pkg` (and `iris install` shorthand):

| Command | Action |
|---|---|
| `iris pkg init` | Create a new `iris.toml` in the current directory |
| `iris pkg add <name> --path <p>` | Add a local path dependency |
| `iris pkg add <name> --git <url>` | Add a Git dependency (`--branch`, `--tag`, or `--rev`) |
| `iris pkg add <name> --registry <pkg> --version <req>` | Add a registry semver dependency |
| `iris pkg install [--offline]` | Fetch dependencies and generate/sync `iris.lock` |
| `iris pkg update [name]` | Refresh dependencies within version constraints |
| `iris pkg vendor` | Mirror dependencies locally into `.iris/vendor` for air-gapped builds |
| `iris pkg check` / `list` | Inspect and verify dependency resolution |
| `iris pkg build [--offline]` / `run` | Ensure dependencies, then build/run entry binary |
| `iris pkg publish [--registry R] [--dry-run]` | Publish an immutable version archive to a registry |
| `iris pkg search <text>` / `serve <dir>` | Search registries or serve a local HTTP package index |

Commit `iris.lock` alongside `iris.toml` so team builds pin exact commit SHAs and SHA-256 content checksums.

---

### 18.2 Assertion-Backed Testing

IRIS discovers and executes every zero-argument `test_*` function across your files when you run `iris test`. Every test function should return `0` after asserting real behavioral invariants—or pair built-in `assert(...)` with rich failure messages from **`std.testing`** (`assert_eq`, `assert_approx_eq`, `assert_str_eq`, `assert_true`, `assert_some`, `assert_none`, `assert_ok`, `assert_err`, `assert_list_eq`, `fail`):

```iris
bring std.testing

def normalize_gain(raw: f64) -> f64 {
    if raw < 0.0 {
        return 0.0
    }
    if raw > 10.0 {
        return 10.0
    }
    return raw
}

def test_gain_clamping() -> i64 {
    assert_approx_eq(normalize_gain(-3.0), 0.0, 0.000001, "clamps negative input")
    assert_approx_eq(normalize_gain(4.5), 4.5, 0.000001, "preserves in-range gain")
    assert_approx_eq(normalize_gain(22.0), 10.0, 0.000001, "clamps upper ceiling")
    return 0
}

def main() -> i64 {
    assert(test_gain_clamping() == 0)
    println("testing: ok")
    return 0
}
```

Run tests with optional filtering or clean CI output:

```text
iris test
iris test projects/ledger/main.iris --no-color
iris watch main.iris --test
```

Remember: avoid tests that merely print output without asserting. Every test should verify exact outputs, boundaries, and negative error paths (`assert(is_err(...))`).

---

### 18.3 Benchmarking, Profiling, and Optimization

Measure compilation and execution timings objectively with **`iris bench`** and **`iris profile`**:

- **`iris bench <file.iris> [-n ITERATIONS]`**: runs 3 warmup passes followed by `ITERATIONS` (default `10`) measured runs, reporting minimum, maximum, mean, median, and standard deviation across parse time, lower + pass-pipeline compile time, and native execution time.
- **`iris profile <file.iris>`**: instruments execution to collect per-function inclusive/exclusive timings (`total_us`, `self_us`), call counts, IR instruction counts, and folded stacks for flame graphs.
- **Native acceleration flags**: combine `--simd`, `--pgo-generate` + `--pgo-use FILE`, and (on release builds) `--release` UI host libraries after measuring baseline hotspots.

---

### 18.4 Formatting, Diagnostics, JSON Output, and Bug Reports

Keep codebases uniform and CI automation clean using IRIS's built-in developer tools:

- **`iris fmt [file]` / `iris fmt --check [file]`**: formats a single file (or all `*.iris` files in the current directory) cleanly with exit code `1` on formatting drift in `--check` mode.
- **`iris check [paths...]`**: runs lexical, parse, type, NLL borrow, exhaustiveness, lint, and optional `--strict-effects` checks without linking. Add `--error-format json` when integrating external CI bots or editors.
- **`iris explain <CODE>`**: prints an extended explanation, root cause, and suggested fix for any compiler diagnostic (`E0001`–`E0902`).
- **`iris bug-report <file.iris>`**: packages the minimal source file, compiler diagnostics, and toolchain environment metadata into a local diagnostic folder if you ever encounter an internal compiler error (nothing is uploaded automatically).

---

### 18.5 Dual-Mode Debugging: Time-Travel Interpreter and Native DWARF

`iris dap` speaks the standard Debug Adapter Protocol (`vscode-iris` configures it automatically) in two complementary modes:

1. **Trace-based interpreter DAP (`"native": false`, default)**: records deterministic execution traces supporting line/conditional/hit-count/logpoint breakpoints, expression watches, variable/local inspection, named `test_*` launches, and **time-travel debugging (`stepBack`, `reverseContinue`)**.
2. **Native DWARF DAP (`"native": true` or `"attach"` with `processId`/`pid`/`target`)**: compile with **`iris build -g main.iris`** to emit DWARF debug metadata mapping machine instructions to IRIS source lines (`-O0`) across scalars, `str`, `record` structs, `choice` enums, fixed arrays, `option`, `result`, tuples, lists, and maps across brought modules. `iris dap` relays sessions to GNU GDB (`gdb -i dap`, tested with GDB 17.1 on Windows), `lldb-dap`, or `IRIS_NATIVE_DAP`, exposing `supportsStepBack` automatically whenever the underlying native debugger provides reverse execution (e.g., `rr`).

#### Common Mistakes

- **Debugging a native release binary without `-g`**: compile with `iris build -g` when launching native GDB/LLDB DAP sessions.
- **Confusing `--emit eval` with trace DAP**: VS Code DAP defaults to trace mode for instant `stepBack`/`reverseContinue`; switch to `"native": true` when stepping compiled LLVM binaries or attaching by PID.

#### Try It Yourself

1. Initialize a package with `iris pkg init`, inspect `iris.toml`, and run `iris fmt --check`.
2. Benchmark a computational loop using `iris bench main.iris -n 20` and compare timings against `iris profile main.iris`.
3. Build a small program with `iris check --error-format json` and inspect the structured JSON diagnostic fields.

---

## Chapter 19: Architecture Walkthroughs of Real IRIS Applications

Seeing how IRIS constructs compose at scale cements every concept covered in this book. This chapter tours four concrete, verified applications included directly in the repository under `projects/`.

---

### 19.1 Checked Ledger and Command-Line Tools

#### `projects/ledger` — Multi-File Domain Modeling and Integer Money

A compact example of strict domain isolation across `"domain.iris"` and `main.iris`:

- Monetary balances are tracked exclusively in integer `cents: i64` rather than floating-point numbers, taking advantage of IRIS's checked arithmetic (`E0409`) so overflowing accumulations cannot silently corrupt balances.
- Overdrafts and negative amounts return typed `result<i64, str>` values rather than panicking, keeping `test_ledger()` fast and verifiable on both interpreter and native backends (`iris test projects/ledger/main.iris`).

#### `projects/cli_tools` — Unix-Style Binary & Text Pipelines

Four self-contained UNIX utilities (`csvq`, `logstat`, `pcapinspect`, `tlsfp`) reading arguments from `args()` (`iris run tool.iris -- ...`):

- Demonstrates `std.csv`, `std.json`, `std.buffer.ByteBuffer`, `std.pcap`, and `std.tls` JA3/JA4 TLS ClientHello fingerprinting running at native LLVM speed while verified against reference Python scripts across both backends.

---

### 19.2 Concurrent Gateways and Self-Upgrading Services

#### `projects/async_gateway` — High-Concurrency Cooperative Fan-Out

Implements an HTTP/1.1 service where every connection and supplier fan-out is an `async def` task executing on a single cooperative thread:

- `/quote/<sku>` spawns three concurrent supplier tasks bounded by a 150 ms deadline (`timeout_after`), cancelling tardy suppliers cooperatively while returning best pricing and cancellation counts.
- `/fastest/<sku>` races tasks via `async_any` + `cancel_all`.
- Verified natively under 64 simultaneous HTTP client threads and 300 concurrent self-test quotes in ~85–95 ms natively (`tests/async_gateway.rs`).

#### `projects/self_upgrading_service` — Live State-Preserving Upgrades

Driven by `iris evolve-serve` + `client.iris`: promotes upgraded inventory server versions over HTTP while carrying live stock counters cleanly across generations—rejecting unsafe/retyped versions at the seven verification gates and preserving rollback safety.

---

### 19.3 Differentiable Orbital Simulation: IRIS GravForge

Located at `projects/grav_forge`, **IRIS GravForge** brings reverse-mode AD, ADTs, traits, algebraic effects, JSON snapshots, and 60 FPS native UI together into a single interactive physics sandbox:

```text
projects/grav_forge/
├── iris.toml      # [app] window_width=1400, gui=true, bundle_ui_host=true
├── physics.iris   # BodyKind/ScenarioId/SimEvent choices, CelestialMetric trait,
│                  # algebraic effect telemetry (`grav_audit_hook`), symplectic RK-style steppers,
│                  # JSON scene codec
├── autodiff.iris  # Reverse-mode burn solver (`tape`/`backward`/`grad` + Adam)
├── canvas.iris    # Batched spacetime metric grid & multi-stop star/black-hole rendering
└── main.iris      # Interactive frame loop with `ui_frame_pool_pause` + 6 themes
```

Key architectural patterns worth adopting in your own scientific or creative software:

1. **Reverse-mode AD in the loop**: holding Shift or launching a Probe engages `autodiff.iris`, rolling out smoothed N-body gravitation (`soft2 = cfg.softening * cfg.softening + 0.0012` preventing `sqrt(0)` singularities), computing miss loss with `tape` + `backward` + `grad`, and steering `(vx, vy)` live with Adam momentum so trajectories thread curved gravity wells toward the beacon.
2. **Polymorphism across heterogeneous entities**: `trait CelestialMetric` inspects bodies uniformly, while `choice BodyKind` dispatches gravity laws and rendering styles via exhaustive `when`.
3. **Algebraic effect telemetry**: simulation milestones emit `grav_audit_hook` intercepted cleanly via `handle { ... } with { ... }` in UI event handlers.
4. **Pure IRIS scene import/export**: snapshots encode/decode JSON via `std.json` + OS clipboard (`ui_native_clipboard_set` / `get`), tested cleanly even without a GPU or display via `test_grav_forge_ad_and_physics`.

---

### 19.4 Live Camera Vision and HUD: Aegis Target Takedown

Located at `projects/target_takedown`, **IRIS Aegis-Vision** pairs live native webcam capture with a real-time computer-vision tactical HUD:

- **Zero-copy capture + fast sensor grid**: `ui_native_camera_open` captures frames via Windows Media Foundation on a background thread (or switches cleanly to deterministic synthetic bogeys via `--sim` or automatic fallback when no webcam is present). The main thread only re-runs CV detection when `info.seq != last_cam_seq`, sampling a compact `64×36` downsampled grid (`ui_native_camera_sample_grid`) into pre-allocated `VisionWorkspace` FFI cells *outside* the per-frame pool.
- **Vision & lock-on state machine**: computes adaptive running background delta + chromatic target salience across `8×6` regions, filters track centroids with an alpha-beta velocity estimator, projects lead reticles, and drives `TargetStatus` transitions (`Acquired` -> `Tracking` -> `Locked` -> `Engaging` -> `Neutralized`).
- **Tactical HUD composition**: draws live tinted video (`RGB` / `NVG` / `FLIR` / `SLATE` via `draw_cmd_camera_frame`), spring-animated reticles, lock progress rings, telemetry sparklines, and interactive controls with strict `ui_frame_pool` pause/detach safety—achieving smooth ~38 ms/frame headless throughput and real-time interactive desktop frame rates when compiled with release `iris_ui_host`.

---

### 19.5 Navigating the Companion Examples and Projects

To continue exploring by domain after finishing this book, inspect the curated index files in the repository:

- **`examples/README.md` (`examples/catalog.json`)**: progressive standalone lessons (`01_basics/` through `10_integration/`, plus `embedded/`) verified automatically across native and interpreter backends (`tools/verify_learning.py`, `cargo test --test examples_showcase`).
- **`projects/README.md`**: 30+ full multi-file applications spanning active inference (`gridworld_inference`), ROS 2 (`turtlesim_autopilot`, `lifecycle_sensor_fusion`, `evolving_turtle`), autonomous control (`quadrotor_autopilot`, `autonomous_deep_space_probe`), networking (`intrusion_detection_system`, `cyber_operations_center`), UI galleries (`ui_studio`, `ui_gallery`, `terra_pulse`, `ais_sentinel`), and embedded hardware controls (`uno_controls`).


---

# Appendices

## Appendix A: Complete Language Grammar and Reserved Words

### Reserved and Contextual Keywords

**Reserved keywords:**

```text
def      val      var      let      return   if       else     while
break    continue bring    when     match    choice   for      in
async    await    const    type     trait    impl     pub      extern
with     dyn      resume   try      catch    raise    move     unsafe
defmacro true     false
```

**Words reserved for future use:**

```text
match  enum  struct  fn  let  mut  use  self  Self  super  crate  where  as  ref  box  macro
```

**Contextual keywords** (valid as ordinary identifiers everywhere outside their dedicated syntactic position):

- `record`, `model`, `mod` (when beginning an item declaration)
- `effect` (an `effect` declaration or `-> T effect ...` function/lambda clause)
- `loop`, `defer`, `yield`, `select` (when beginning a statement in their syntactic form)
- `spawn` (statement followed by `{` or `(group) {`)
- `par` (before `for`)
- `to` (binary conversion expression `x to f64`)
- `by` (range step modifier `0..n by 2`)
- `input`, `layer`, `output` (inside `model { ... }` blocks)

### Core Grammar Summary (BNF)

```bnf
module       ::= { top_level }
top_level    ::= function_def | record_def | enum_def | const_def
               | type_alias | trait_def | impl_def | bring_decl
               | extern_def | model_def | macro_def

bring_decl   ::= [ "pub" ] "bring" ( IDENT { "." IDENT } | STRING_LIT )
function_def ::= [ "pub" ] [ "async" | "const" ] "def" IDENT [ type_params ]
                 "(" params ")" "->" type [ effect_clause ] block
type_params  ::= "[" type_param { "," type_param } "]"
params       ::= [ param { "," param } ]
param        ::= IDENT ":" type [ "=" expr ]

record_def   ::= [ "pub" ] "record" IDENT [ type_params ] "{" field_defs "}"
field_defs   ::= field_def { "," field_def } [ "," ]
field_def    ::= IDENT ":" type [ "=" expr ]

enum_def     ::= [ "pub" ] "choice" IDENT "{" variant_defs "}"
variant_defs ::= variant_def { "," variant_def } [ "," ]
variant_def  ::= IDENT [ "(" type { "," type } ")" ]

const_def    ::= [ "pub" ] "const" IDENT [ ":" type ] "=" expr
type_alias   ::= [ "pub" ] "type" IDENT "=" type
trait_def    ::= "trait" IDENT [ "[" ... "]" ] "{" { trait_method } "}"
impl_def     ::= "impl" [ "[" ... "]" ] [ IDENT "for" ] type "{" { function_def } "}"
extern_def   ::= "extern" "def" IDENT "(" params ")" "->" type [ effect_clause ]
macro_def    ::= "defmacro" IDENT "(" [ idents ] ")" "=>" expr

model_def    ::= "model" IDENT "{" { model_item } "}"
model_item   ::= "input" IDENT ":" type
               | "layer" IDENT IDENT [ "(" layer_args ")" ]
               | "output" IDENT

block        ::= "{" { stmt } [ expr ] "}"
stmt         ::= "val" ( IDENT [ ":" type ] | "(" IDENT { "," IDENT } ")" ) "=" expr [ ";" ]
               | "var" IDENT [ ":" type ] "=" expr [ ";" ]
               | expr ( "=" | "+=" | "-=" | "*=" | "/=" ) expr [ ";" ]
               | "while" expr block [ ";" ]
               | "loop" block [ ";" ]
               | "for" IDENT "in" expr [ ".." expr [ "by" expr ] ] block [ ";" ]
               | "par" "for" IDENT "in" expr ".." expr block [ ";" ]
               | "spawn" [ "(" expr ")" ] block [ ";" ]
               | "return" [ expr ] [ ";" ]
               | "break" [ ";" ]
               | "continue" [ ";" ]
               | expr [ ";" ]
```

---

## Appendix B: Built-in Functions and Standard Library Map

### Global Built-in Functions (No Import Required)

- **Core I/O & Assertions**: `print`, `println`, `read_line`, `read_i64`, `read_f64`, `assert`, `panic`, `type_of`, `args`
- **Conversions**: `to_str`, `to_i64`, `to_f64`, `format`, `parse_i64`, `parse_f64`
- **Math & Checked/Wrapping Ops**: `sin`, `cos`, `tan`, `exp`, `log`, `log2`, `sqrt`, `abs`, `floor`, `ceil`, `round`, `sign`, `pow`, `min`, `max`, `clamp`, `math_pi`, `math_e`, `math_inf`, `is_nan`, `is_inf`, `wrapping_add`, `wrapping_sub`, `wrapping_mul`
- **Bitwise**: `band`, `bor`, `bxor`, `shl`, `shr`, `bitnot`
- **Strings & Regex**: `len`, `concat`, `contains`, `starts_with`, `ends_with`, `to_upper`, `to_lower`, `trim`, `repeat`, `split`, `join`, `find`, `slice`, `str_index`, `str_replace`, `str_reverse`, `char_at`, `str_pad_left`, `str_pad_right`, `str_chars`, `str_bytes`, `str_count`, `regex_match`, `regex_find_all`, `regex_replace_all`
- **Collections**: `list`, `list_get`, `list_set`, `list_len`, `list_push`, `list_pop`, `list_map`, `list_filter`, `list_reduce`, `list_any`, `list_all`, `list_zip`, `list_enumerate`, `list_flatten`, `list_unique`, `list_reverse`, `list_sorted`, `list_sum`, `list_min`, `list_max`, `map`, `map_get`, `map_set`, `map_contains`, `map_remove`, `map_keys`, `map_values`, `map_len`, `cell`, `cell_get`, `cell_set`
- **Option / Result / Autodiff / Time**: `some`, `none`, `is_some`, `unwrap`, `ok`, `err`, `is_ok`, `is_err`, `tape`, `backward`, `grad`, `sparsify`, `densify`, `time_now_ms`, `sleep_ms`
- **Concurrency & Atomics**: `channel`, `send`, `recv`, `spawn`, `chan_try_recv`, `chan_len`, `select`, `timeout`, `thread_count`, `atomic`, `atomic_load`, `atomic_store`, `atomic_add`, `mutex`, `mutex_lock`, `mutex_unlock`

### Embedded Standard Library (`bring std.<module>`)

IRIS ships **58 embedded `.iris` modules** (**59** import names in `STDLIB_MODULE_NAMES`, where `std.tensorx` aliases `std.tensor`). See [Standard Library Reference](stdlib-reference.md) for every function signature.

| Domain | Modules |
|---|---|
| **Desktop Native UI & Media** | `ui`, `ui_theme`, `ui_protocol`, `ui_layout`, `ui_motion`, `ui_icons`, `ui_widgets` |
| **AI, ML, AD & Autonomy** | `adaptive`, `ais`, `dataset`, `llm`, `meta_learning`, `ml`, `nn`, `rl`, `stochastic`, `tensor` (`tensorx`), `uncertainty` |
| **Data, Storage & Visualization** | `csv`, `dataframe`, `json`, `kv`, `sql`, `svg`, `table`, `termplot` |
| **Collections, Strings & Core** | `bitset`, `buffer`, `collections`, `container`, `deque`, `fmt`, `heap`, `iter`, `log`, `math`, `queue`, `set`, `string`, `testing`, `time`, `unicode` |
| **Systems, Networking & Hardware** | `async`, `crypto`, `ffi`, `fs`, `http`, `http_server`, `net`, `os`, `path`, `pcap`, `ros2`, `serial`, `tls` |
| **Safety, Evolution & Reflection** | `meta`, `reflect`, `speculation` |

---

## Appendix C: Type System and Operator Precedence Reference

### Built-in Types

- **Scalars**: `i8`, `u8`, `i32`, `u32`, `i64`, `u64`, `usize`, `f32`, `f64`, `bool`, `str`, `unit`
- **Composites & Containers**:
  - Tuples `(T1, T2, ...)` and fixed stack arrays `[T; N]`
  - `list<T>`, `map<K, V>`, `tensor<DTYPE, [DIMS]>`, `sparse<T>`, `grad<T>`
  - `option<T>`, `result<T, E>`
  - `chan<T>`, `atomic<T>`, `mutex<T>`, `task_group`, `cell<T>`
  - References `&T`, `&mut T`, dynamic traits `dyn Trait`, functions `(Args) -> Ret [effect ...]`
  - Named `record` structs and `choice` algebraic enums

### Operator Precedence (Highest to Lowest)

| Precedence | Category | Operators | Notes |
|---|---|---|---|
| 1 (highest) | Postfix | `.field`, `.method()`, `.0`, `[idx]`, `?`, `!` (macro) | Left-to-right |
| 2 | Prefix unary | `-`, `!`, `not`, `&`, `&mut`, `*` | Right-to-left |
| 3 | Matrix Contraction | `@` | Left-to-right tensor matmul |
| 4 | Multiplicative | `*`, `/`, `%` | Left-to-right |
| 5 | Additive | `+`, `-` | Left-to-right |
| 6 | Coalescing / Cast | `to`, `??` | Explicit conversion / default |
| 7 | Comparison | `==`, `!=`, `<`, `<=`, `>`, `>=` | Non-chaining |
| 8 | Logical AND | `&&`, `and` | Short-circuiting |
| 9 (lowest) | Logical OR | `\|\|`, `or` | Short-circuiting |

---

## Appendix D: Complete CLI Reference

The unified `iris` binary organizes **31 subcommands**, **16 `--emit` modes**, and global compiler/execution controls:

### Subcommands (31)

- **Build & Packaging**: `build`, `bundle`, `run`, `embedded`, `new`, `pkg`, `install`, `bindgen`
- **Evolution & Services**: `evolve`, `evolve-unrestricted`, `evolve-search`, `evolve-live`, `evolve-serve`, `service-daemon`, `meta`
- **Development, Quality & Debugging**: `check`, `test`, `watch`, `fmt`, `docs`, `bench`, `profile`, `explain`, `doctor`, `bug-report`
- **Interactive & Toolchain**: `repl`, `lsp`, `dap`, `setup`, `upgrade`, `help`

### Emit Kinds (`--emit <EMIT>`)

| Mode | Purpose |
|---|---|
| `ir` (default) | Print SSA Intermediate Representation |
| `llvm` | Print lowered LLVM IR for the module |
| `llvm-complete` | Print complete LLVM IR including linked runtime declarations |
| `eval` | Execute program natively (set `IRIS_FORCE_INTERP=1` for interpreter execution) |
| `jit` | Execute in-process via LLVM ORC JIT |
| `binary` | Emit standalone native executable |
| `simd` | Emit LLVM IR with AVX2/NEON vectorized kernels |
| `pgo-instrument` / `pgo-optimize` | Emit instrumented or PGO-optimized LLVM IR (`--pgo-generate` / `--pgo-use`) |
| `cuda` / `cuda-ptx` | Emit NVIDIA CUDA LLVM IR or PTX device code (`--gpu cuda` runs numeric/tensor `par for`; PTX & CPU fallback tested, not yet run on NVIDIA GPU hardware) |
| `graph` | Export DOT dependency / model topology graph |
| `onnx` / `onnx-binary` | Export declarative `model` graph as text or ONNX protobuf (`--weights FILE`) |
| `tensorrt` | Build serialized TensorRT engine via `trtexec` (CLI invocation tested, not yet run where TensorRT is installed) |
| `python-ext` | Compile module as a CPython extension (`.pyd` / `.so`) |

### Key Global & Build Options

`--output <FILE>` (`-o`), `--error-format human|json`, `--target <TRIPLE>`, `--dump-ir-after <PASS>`, `--max-steps <N>`, `--max-depth <N>`, `--no-cache`, `--sandbox`, `--strict-effects`, `--simd`, `--pgo-generate`, `--pgo-use <FILE>`, `--weights <FILE>`, `--gpu cuda`, `--debug` (`-g`), `--define NAME[=VALUE]` (`-D`), `iris build --gui`, and `iris bundle --format auto|dir|zip|inno|app|appimage --release --install --uninstall`.

---

## Appendix E: Compiler Diagnostic and Warning Reference

Run `iris explain <CODE>` for detailed inline help and fixes on any code:

| Range | Code | Diagnostic Title |
|---|---|---|
| **Lexer & Parser** | `E0001`–`E0007` | `E0001` Unexpected Character, `E0002` Unterminated String, `E0003` Invalid Escape Sequence, `E0004` Invalid Literal, `E0005` Unexpected Token, `E0006` Unclosed Delimiter, `E0007` Nesting Too Deep |
| **Lowering & Type Resolution** | `E0100`–`E0129` | `E0100` Undefined Variable/Function, `E0101` Type Mismatch, `E0102` Duplicate Definition, `E0103` Unsupported Construct, `E0104`–`E0107` Model DSL Layer/Op Errors, `E0108` Rejected Construct, `E0109` Wrong Arg Count, `E0110` Arg Type Mismatch, `E0111` Unknown Type, `E0112` Assign to Immutable `val`, `E0113`–`E0115` Field/Module Errors, `E0116` Missing Return, `E0117`–`E0119` Method/Callable/`if`-without-`else`, `E0122`–`E0129` Variant/Loop/Format/`when`/Trait Impl Errors |
| **IR Passes & Borrow Safety** | `E0200`–`E0210`, `E0382` | `E0200` Use Before Definition, `E0201` Multiple Definition, `E0202` Pass Type Error, `E0203` Missing Terminator, `E0204` Shape Mismatch, `E0205` Unresolved Type, `E0206` Const Eval Error, `E0207` Type Annotation Needed, `E0208` Branch Type Mismatch, `E0209` Wrong Return Type, `E0210` Non-exhaustive `when`, `E0382` Use After Move |
| **Codegen & Effects** | `E0300`–`E0304` | `E0300` Codegen Failed, `E0301` No Native Toolchain, `E0302` Program Failed, `E0303` Unavailable on Target, `E0304` Effect Bound Exceeded (`--strict-effects`) |
| **Runtime Traps** | `E0400`–`E0410` | `E0400` Runtime Error, `E0401` Undefined Value, `E0402` Division By Zero, `E0403` Index Out of Bounds, `E0404` Runtime Type Error, `E0405` Runtime Unsupported, `E0406` Panic/`assert`/`unwrap`, `E0407` Security Violation, `E0408` Stack Overflow, `E0409` Integer Overflow, `E0410` GPU Error |
| **System & Internal** | `E0500`–`E0902` | `E0500` I/O Error, `E0501` Preprocessor Error, `E0900`–`E0902` Internal Compiler Errors (`iris bug-report`) |

The compiler and LSP (`find_source_warnings`) also flag unused variables, parameters, private unused functions, unreachable code, unsafe blocks, and loop conditions whose variables never mutate in-body.

---

**Edition**: IRIS `1.0.0-rc3`
**Toolchain**: Tested on Windows (LLVM 23.1.1 + MinGW UCRT64 `ld.lld`), Linux (`x86_64`), and macOS (`aarch64`)
**License**: Apache License 2.0 — see [LICENSE](../LICENSE)
**Source**: [github.com/Eclipse-Softworks/IRIS](https://github.com/Eclipse-Softworks/IRIS)
