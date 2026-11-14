# IRIS System Requirements

> **IRIS**
> Comprehensive platform requirements for running, building, and deploying IRIS programs.

---

## Quick Start

```sh
# Verify installation
iris --version

# Evaluate a program (runs natively by default; reports E0302 on native failure, set IRIS_INTERP_FALLBACK=1 for interpreter fallback)
iris --emit eval hello.iris

# Build a native binary
iris build hello.iris
./hello
```

---

## Minimum Requirements (Interpreter / IR Emission)

These are the requirements to run `iris <file.iris>` for IR analysis, or
`iris repl`, `iris lsp`, `iris dap`, and `iris pkg`.

| Component     | Requirement                                    |
|---------------|------------------------------------------------|
| **OS**        | Windows 10+, macOS 12+, Linux (glibc 2.17+)   |
| **Arch**      | x86_64 (AMD64) or ARM64 (aarch64)             |
| **RAM**       | 256 MB minimum                                 |
| **Disk**      | 50 MB (IRIS binary + stdlib)                   |
| **Dependencies** | **None** — fully self-contained             |

---

## Native Compilation Requirements

Native object emission and ORC JIT execution use a dynamically loaded LLVM-C
library. Native binaries additionally need a target linker and compatible
sysroot. The validated Windows MinGW path links directly with `ld.lld`; Clang is
an optional compatibility fallback for targets without a direct linker path.

| Component     | Requirement                                              |
|---------------|----------------------------------------------------------|
| **LLVM-C**    | LLVM 23.1.1 shared library with target and ORC APIs         |
| **Linker**    | `lld` (bundled with LLVM) or system linker               |
| **C library** | Windows: MinGW UCRT64 · Linux: glibc · macOS: system SDK |
| **RAM**       | 512 MB minimum, 2 GB recommended for large projects      |
| **Disk**      | 500 MB (IRIS + bundled LLVM + sysroot)                   |

### Installing LLVM and lld

**Ubuntu / Debian:**
```sh
sudo apt update
sudo apt install clang lld
# Or install the version used by IRIS release bundles:
sudo apt install clang-23 lld-23
```

**Fedora / RHEL:**
```sh
sudo dnf install clang lld
```

**Arch Linux:**
```sh
sudo pacman -S clang lld
```

**macOS (Homebrew):**
```sh
brew install llvm
# Add to PATH:
export PATH="$(brew --prefix llvm)/bin:$PATH"
```

**Windows:**
- The **Full installer** (`.exe` / `.msi`) bundles LLVM and MinGW UCRT64 — no extra steps needed.
- IRIS release bundles and the dependency downloader use LLVM **23.1.1**.
- For **portable** installs: download LLVM from https://releases.llvm.org/ and install MSYS2 UCRT64:
  ```powershell
  # Install MSYS2 from https://www.msys2.org/
  # Then in MSYS2 UCRT64 shell:
  pacman -S mingw-w64-ucrt-x86_64-gcc mingw-w64-ucrt-x86_64-headers-git
  ```

---

## ML Workload Requirements (Optional)

These are optional dependencies that unlock accelerated ML performance.

| Component           | Purpose                      | Install                              |
|---------------------|------------------------------|--------------------------------------|
| **OpenBLAS**        | Accelerated matmul (10-50×)  | `apt install libopenblas-dev`        |
| **Intel MKL**       | Accelerated matmul (Intel CPUs) | Via Intel oneAPI toolkit          |
| **NVIDIA driver**   | Running `--gpu cuda` programs (loaded at run time) | https://www.nvidia.com/drivers |
| **CUDA Toolkit 12+** | libdevice, for `--gpu cuda` code that uses `sin`, `exp`, `pow` and the like (`CUDA_PATH`) | https://developer.nvidia.com/cuda |
| **TensorRT**        | `--emit tensorrt` engines (`trtexec`, via `TENSORRT_ROOT` or `PATH`) | https://developer.nvidia.com/tensorrt |
| **ONNX Runtime**    | ONNX model inference         | https://onnxruntime.ai/              |
| **LibTorch**        | PyTorch model inference      | https://pytorch.org/cppdocs/         |
| **TensorFlow C API** | TensorFlow model inference  | https://www.tensorflow.org/install/lang_c |
| **RAM**             | 4 GB+ for training workloads | —                                    |
| **GPU VRAM**        | 4 GB+ for CUDA workloads    | —                                    |

### Enabling BLAS Acceleration

```sh
# Linux (OpenBLAS)
sudo apt install libopenblas-dev
export IRIS_USE_BLAS=1
iris build my_ml_program.iris    # Links against OpenBLAS

# macOS (Accelerate framework is always available)
# BLAS is available via the system Accelerate framework — no extra install needed.

# Windows
# Install OpenBLAS from https://github.com/OpenMathLib/OpenBLAS/releases
# Set OPENBLAS_DIR environment variable
```

### Enabling ML Backend Inference

IRIS uses a **dynamic runtime loading (`dlopen`)** architecture (see `docs/ml-backends.md` and `src/runtime/iris_ml_dynload.h`). ML backends are resolved dynamically at runtime on first use without compile-time linking; the legacy `IRIS_NATIVE_ML_BACKENDS=1` flag is deprecated and obsolete.

Ensure the shared library is available on your library search path:

```sh
# ONNX Runtime (onnxruntime.dll / libonnxruntime.so)
export LD_LIBRARY_PATH=/path/to/onnxruntime/lib:$LD_LIBRARY_PATH
iris build projects/learning_service/main.iris

# PyTorch (LibTorch) — requires standalone iris_torch_plugin (see docs/ml-backends.md)
export LD_LIBRARY_PATH=/path/to/libtorch/lib:$LD_LIBRARY_PATH

# TensorFlow (tensorflow.dll / libtensorflow.so)
export LD_LIBRARY_PATH=/path/to/tensorflow/lib:$LD_LIBRARY_PATH
```

---

## Declared Platform Targets

The repository workflows are the authority for the matrix exercised by a given
commit. Do not infer that every installer format below was produced or physically
run merely because a target is declared.

| Platform          | Architecture | Binary | Installers                  | CI Runner          |
|-------------------|-------------|--------|-----------------------------|-------------------|
| **Windows 10/11** | x86_64      | ✅     | `.exe`, `.zip`, `.msi`      | `windows-latest`  |
| **Windows 11**    | ARM64       | ✅     | `.zip`                      | `windows-latest` (cross) |
| **Ubuntu 20.04+** | x86_64      | ✅     | `.deb`, `.rpm`, `.AppImage` | `ubuntu-latest`   |
| **Ubuntu 22.04+** | ARM64       | ✅     | `.deb`, `.rpm`              | `ubuntu-24.04-arm` |
| **macOS 12+**     | x86_64      | ✅     | `.pkg`, `.dmg`              | `macos-latest`    |
| **macOS 14+**     | ARM64       | ✅     | `.pkg`, `.dmg`              | `macos-14`        |

---

## Environment Variables

| Variable                  | Purpose                                              | Default |
|---------------------------|------------------------------------------------------|---------|
| `IRIS_CLANG`              | Optional Clang compatibility fallback override       | `clang` |
| `IRIS_LLVM_C_API`         | Exact LLVM-C shared-library path                      | auto-detected |
| `IRIS_USE_CLANG`          | Compile programs with clang instead of in process through LLVM-C (`-g` and sanitizer builds always use clang) | off |
| `IRIS_OPT_LEVEL`          | Optimisation level of native builds, `0`–`3`         | `3`     |
| `IRIS_VERBOSE`            | Report each native build step and its time           | off     |
| `IRIS_INTERP_FALLBACK`    | Rerun `--emit eval` in the interpreter when the native build or run fails | off |
| `IRIS_PANIC_ABORT`        | A failed `assert` or panic calls `abort()` (for a debugger) instead of exiting with 101 | off |
| `IRIS_FREE_AT_EXIT`       | Free every live object at exit (for leak hunting)    | off     |
| `IRIS_DEBUG`              | Show compiler-internal detail in diagnostics          | off     |
| `IRIS_USE_BLAS`           | Enable BLAS-accelerated tensor ops (`1` to enable)   | off     |
| `IRIS_NATIVE_ML_BACKENDS` | *(Deprecated)* Obsolete compile-time link flag; ML backends now use dynamic `dlopen` loading (see `docs/ml-backends.md`) | off     |
| `ONNXRUNTIME_DIR`         | Path to ONNX Runtime SDK                             | —       |
| `LIBTORCH`                | Path to LibTorch (PyTorch C++ API)                   | —       |
| `TENSORFLOW_DIR`          | Path to TensorFlow C API                             | —       |
| `OPENBLAS_DIR`            | Path to OpenBLAS installation                        | —       |
| `IRIS_LOG`                | Log level: `error`, `warn`, `info`, `debug`, `trace` | `warn`  |

---

## Building from Source

Requires **Rust 1.75+** (stable). No C compiler needed to build IRIS itself —
only for the programs IRIS compiles.

```sh
git clone https://github.com/Eclipse-Softworks/IRIS
cd iris

# Debug build
cargo build

# Release build (optimized)
cargo build --release

# Run tests (431 library unit tests via --lib; 229 integration test files in tests/)
cargo test

# Install globally
cargo install --path .
```

---

## Verification

After installation, verify everything works:

```sh
# Check version
iris --version

# Run a simple program
echo 'def main() -> i64 { print("Hello from IRIS!"); 0 }' > hello.iris
iris run hello.iris

# Run the ML pipeline example
iris run projects/learning_service/main.iris

# Run the AIS agent example
iris run examples/08_ais/viability.iris

# Build a native binary
iris build hello.iris
./hello      # Linux/macOS
hello.exe    # Windows
```
