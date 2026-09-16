# Getting started with IRIS RC1

## Build or install

Build the compiler with Rust and Cargo:

```text
git clone https://github.com/Eclipse-Softworks/IRIS.git
cd IRIS
cargo build --release --locked
```

The executable is `target/release/iris.exe` on Windows or
`target/release/iris` elsewhere. Keep it in a stable directory on PATH.
On Windows the recommended per-user location is
`$env:USERPROFILE\.iris\bin\iris.exe`. Restart existing terminals after
changing the user PATH.

Compiler/LSP startup needs the executable and its OS runtime. Native builds also
need a loadable LLVM-C library, a linker, and target system libraries. IRIS
release bundles and installers use LLVM 23.1.1.
On Windows install LLVM and a MinGW UCRT64 sysroot. See
[requirements](REQUIREMENTS.md) for discovery settings and platform dependencies.

## First program

```iris
def main() -> i64 {
    val answer: i64 = 42;
    assert(answer == 42);
    println(f"answer = {answer}");
    return 0
}
```

Save as `hello.iris`, then use:

```text
iris run hello.iris
iris build hello.iris -o hello
iris fmt hello.iris
```

On Windows execute the output as `.\hello.exe`; on Unix use `./hello`.
`return` exits the current function. Expression tails remain valid, especially
inside `if`, `when`, and lambda bodies.

## Guided progression

Begin with [01_basics](../examples/01_basics/), then follow
[the catalog](../examples/README.md). Start complete programs with
[the ledger](../projects/ledger/), then progress to
[neural training](../projects/learning_service/) and
[the evolution lab](../projects/autonomous_evolution_lab/).

The collection includes standalone examples, reusable project modules,
compiler-host examples, and board/service integrations.
The [manifest](../examples/catalog.json) records which execution path applies.

## Editor and tests

Install [the VS Code extension](../vscode-iris/README.md).
Set `iris.executablePath` to your installed copy. Avoid pointing a running
language server at a Cargo build output that is being relinked.

Use Format Document for `iris fmt` formatting, hover for types/docs/effects,
and Test Explorer for zero-argument `test_*` functions. For example:

```text
iris test projects/ledger/main.iris --no-color
iris --strict-effects --emit ir examples/04_safety/effects.iris
```

DAP debugging is interpreter trace-based. It does not attach to native processes.

## Reproduce the learning checks

```text
python tools/verify_learning.py --iris target/release/iris.exe
cargo test --test examples_showcase
```

Use `--iris target/release/iris` on Unix. The runner uses temporary working
directories and a per-program timeout. Compiler-host examples run with
`IRIS_FORCE_INTERP=1`; hardware and live-service programs have separate commands
and are never counted as a successful hardware/service test merely for compiling.
