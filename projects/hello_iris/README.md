# hello_iris

A starter project created using the **IRIS CLI Package Manager** (`iris pkg`).

---

## 1. Project Creation

This project was initialized directly using the IRIS command-line tool:

```bash
# 1. Create a new project folder
mkdir projects/hello_iris

# 2. Initialize project manifest and scaffold
cd projects/hello_iris
iris pkg init
```

The `iris pkg init` command automatically scaffolds:
- [`iris.toml`](file:///c:/Users/Moon/Desktop/Projects/IRIS/projects/hello_iris/iris.toml): The project package manifest defining name, version, entry file, and dependencies.
- [`main.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/projects/hello_iris/main.iris): The default executable entry point function `pub def main() -> i64`.
- `.iris/`: The local project metadata directory for dependency resolution and lockfiles.

---

## 2. Manifest Configuration (`iris.toml`)

```toml
[package]
name = "hello_iris"
version = "0.1.0"
entry = "main.iris"

[dependencies]
```

---

## 3. CLI Commands Reference

From inside the project directory:

| Command | Action |
|---|---|
| `iris pkg run` (or `iris pkg r`) | Resolves dependencies, builds the binary, and executes it immediately. |
| `iris pkg build` (or `iris pkg b`) | Compiles the project binary to `hello_iris.exe`. |
| `iris pkg list` (or `iris pkg ls`) | Displays declared and locked dependencies. |
| `iris pkg check` | Verifies that all required dependencies are installed. |
| `iris pkg add <name> --path <p>` | Adds a local path dependency to `iris.toml`. |
| `iris pkg add <name> --git <url>` | Adds a Git repository dependency. |
| `iris pkg remove <name>` | Removes a dependency from the manifest and lockfile. |
| `iris run main.iris` | Directly compiles and runs the IRIS source file. |

---

## 4. Running the Project

```bash
iris pkg run
```

### Output:
```
no dependencies to install
iris_codegen: using prebuilt runtime objects for x86_64-pc-windows-gnu (no C compiler required)
iris_codegen: linked via ld.lld directly
wrote binary: C:\Users\Moon\Desktop\Projects\IRIS\projects\hello_iris\hello_iris.exe
=== hello_iris (v0.1.0) ===
Author: IRIS Developer
Tasks Completed: 42
Circle (r=5): Area = 78.5398
Loop iterations: 1000 in 0 ms

Project initialized and running successfully with the IRIS CLI!
```
