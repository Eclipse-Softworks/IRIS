# IRIS ML Integration Guide

> ⚠️ **ARCHITECTURAL UPDATE & DEPRECATION NOTICE**
>
> **Compile-time SDK linking (`IRIS_NATIVE_ML_BACKENDS=1`, `cargo clean && cargo build`) is DEPRECATED and OBSOLETE.**
>
> IRIS has migrated to a **dynamic runtime loading (`dlopen`/`LoadLibrary`)** architecture (see [`docs/ml-backends.md`](ml-backends.md) and [`src/runtime/iris_ml_dynload.h`](../src/runtime/iris_ml_dynload.h)):
> - **Zero compile-time SDK requirement**: IRIS does not require external ML SDKs to build, and does not link against external ML libraries at compile time.
> - **Lazy dynamic resolution**: Shared libraries (`onnxruntime.dll`/`libonnxruntime.so`, `tensorflow.dll`/`libtensorflow.so`, or the `iris_torch_plugin` shared library) are discovered and loaded at runtime on first use via `dlopen`/`LoadLibrary`.
> - **Self-contained binaries**: Programs that do not use ML backends never load external ML shared libraries. If an ML library is missing when an ML function is called, IRIS returns a graceful runtime error rather than crashing or failing at start.
> - `IRIS_NATIVE_ML_BACKENDS` is obsolete and ignored by the build system.

This guide explains how to set up and test ONNX Runtime, LibTorch, and TensorFlow integration with IRIS.

## Architecture Overview

```
IRIS Source Code
    ↓
Stdlib Module: std.ml
    ↓
Native IRIS runtime bridge (`iris_mlrt_*` wrappers)
    ↓
Dynamic ML Loader (src/runtime/iris_ml_dynload.h / dlopen / LoadLibrary)
    ↓
C/C++ Shim Layer (src/runtime/onnx_shim.c, tf_shim.c, iris_torch_plugin)
    ↓
Shared Libraries at Runtime (onnxruntime.dll/.so, tensorflow.dll/.so, iris_torch_plugin)
```

### Data Flow

- **IRIS Tensors**: Represented as lists of f64 values + shape list
- **IrisTensor (C struct)**: `float* data, int64_t* shape, int32_t ndim, int64_t numel`
- **Native Tensors**: Framework-specific (OrtValue, torch::Tensor, TfTensor)

## ONNX Runtime Integration

### Windows Setup

1. **Download ONNX Runtime Shared Library**:
   - Visit: https://github.com/microsoft/onnxruntime/releases
   - Download: `onnxruntime-win-x64-1.17.0.zip` (or latest)
   - Extract to: `C:\onnxruntime` (or your preferred location)

2. **Configure Library Search Path**:
   Ensure `onnxruntime.dll` is discoverable at runtime by doing one of:
   - Placing `onnxruntime.dll` next to your compiled IRIS binary
   - Adding `C:\onnxruntime\lib` to your system `PATH`
   - Setting `$env:ONNXRUNTIME_DIR = "C:\onnxruntime"`

   *(Note: The legacy `$env:IRIS_NATIVE_ML_BACKENDS = "1"` is deprecated and obsolete; IRIS automatically resolves ONNX Runtime via `dlopen`/`LoadLibrary` at runtime).*

3. **No Compiler Rebuild Needed**:
   IRIS dynamically resolves ONNX entry points on first use via `iris_ml_dynload.h`. You do **not** need to run `cargo clean && cargo build`.

4. **Generate Test Models**:
   ```bash
   cd tests
   pip install onnx numpy
   python create_onnx_model.py
   ```

   This creates:
   - `fixtures/identity.onnx`: Identity function (output = input)
   - `fixtures/add_const.onnx`: Add constant (output = input + 2.0)
   - `fixtures/matmul.onnx`: Matrix multiplication

5. **Run Tests**:
   ```bash
   cargo test --test ml_inference -- --nocapture
   ```

### Linux Setup

1. **Download ONNX Runtime**:
   ```bash
   cd /opt
   wget https://github.com/microsoft/onnxruntime/releases/download/v1.17.0/onnxruntime-linux-x64-1.17.0.tgz
   tar xzf onnxruntime-linux-x64-1.17.0.tgz
   ```

2. **Configure Library Search Path**:
   ```bash
   export LD_LIBRARY_PATH=/opt/onnxruntime-linux-x64-1.17.0/lib:$LD_LIBRARY_PATH
   ```
   Or place `libonnxruntime.so` in `/usr/local/lib`.

3. **Run Tests**:
   ```bash
   cd tests && python create_onnx_model.py
   cargo test --test ml_inference
   ```

## LibTorch Integration (PyTorch C++)

### Dynamic Plugin Architecture

Because LibTorch exposes a C++ ABI with mangled symbols, IRIS uses a standalone dynamic C-ABI plugin (`iris_torch_plugin`) built once against LibTorch (see `docs/ml-backends.md`). IRIS runtime loads this plugin via `dlopen` on demand.

### Windows Setup

1. **Download LibTorch**:
   - Visit: https://pytorch.org/get-started/locally/
   - Select: C++ → CPU → Windows
   - Download: `libtorch-win-shared-with-deps-*.zip`
   - Extract to: `C:\libtorch`

2. **Build the Plugin Once**:
   Build `iris_torch_plugin.dll` using `codegen::build::build_torch_plugin` or:
   ```powershell
   $env:LIBTORCH = "C:\libtorch"
   cargo test --test ml_inference
   ```
   Place `iris_torch_plugin.dll` and LibTorch DLLs on your `PATH` or next to your binary.

3. **Create Test Models**:
   ```python
   import torch
   model = torch.nn.Sequential(
       torch.nn.Linear(3, 4),
       torch.nn.ReLU()
   )
   scripted = torch.jit.script(model)
   torch.jit.save(scripted, 'tests/fixtures/model.pt')
   ```

4. **Run Tests**:
   ```bash
   cargo test --test ml_inference
   ```

### Linux Setup

1. Export LibTorch location:
   ```bash
   export LIBTORCH=/opt/libtorch
   export LD_LIBRARY_PATH=$LIBTORCH/lib:$LD_LIBRARY_PATH
   ```
2. Build the plugin and run tests:
   ```bash
   cargo test --test ml_inference
   ```

## TensorFlow Integration

### Windows Setup

1. **Download TensorFlow C API**:
   ```
   https://storage.googleapis.com/tensorflow/libtensorflow/libtensorflow-cpu-windows-x86_64-2.13.0.zip
   ```

2. **Extract and Add to Search Path**:
   Extract to `C:\tensorflow` and ensure `tensorflow.dll` is on `PATH` or next to your executable.

3. **Run Tests**:
   ```bash
   cargo test --test ml_inference
   ```

### Linux Setup

1. Download and extract `libtensorflow` to `/usr/local` or `/opt/tensorflow`.
2. Add to `LD_LIBRARY_PATH` if needed:
   ```bash
   export LD_LIBRARY_PATH=/opt/tensorflow/lib:$LD_LIBRARY_PATH
   ```
3. Run tests:
   ```bash
   cargo test --test ml_inference
   ```

## Testing End-to-End Model Execution

### Test a Simple ONNX Model

```rust
#[test]
fn test_onnx_add_model() {
    unsafe {
        // Load model
        let session = iris_onnx_session_create(CString::new("fixtures/add_const.onnx").unwrap().as_ptr());
        
        // Create input [2, 3] with values [1,2,3,4,5,6]
        let input_shape = [2i64, 3i64];
        let input = iris_tensor_alloc(2, input_shape.as_ptr());
        let data = vec![1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0];
        std::ptr::copy_nonoverlapping(data.as_ptr(), (*input).data, 6);
        
        // Run inference
        let mut inputs = vec![input];
        let mut outputs: *mut *mut IrisTensor = std::ptr::null_mut();
        let mut n_outputs = 0;
        
        let rc = iris_onnx_session_run(session, inputs.as_mut_ptr(), 1, &mut outputs, &mut n_outputs);
        
        if rc == 0 {
            // Success! Output should be [3,4,5,6,7,8]
            let output_data = std::slice::from_raw_parts((*(*outputs)).data, 6);
            assert_eq!(output_data, &[3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
        }
        
        // Cleanup
        iris_tensor_free(input);
        iris_tensor_free(*outputs);
        iris_onnx_session_free(session);
    }
}
```

### Using ML from IRIS Code

```iris
import std.ml;

def test_onnx_inference() {
    // Load ONNX model
    let model_id = onnx_load("model.onnx");
    
    // Create input tensor: [2, 3]
    let input = [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]];
    
    // Run inference (shape is inferred)
    let output = onnx_run(model_id, input);
    
    // Use output
    print(output);
    
    // Cleanup
    onnx_free(model_id);
}
```

## Troubleshooting

### "ONNX Runtime shared library not found at runtime"

**Cause**: `onnxruntime.dll` (Windows) or `libonnxruntime.so` (Linux) is not on the search path when an ML function is called.

**Solution**:
- On Windows: Place `onnxruntime.dll` in the same directory as the binary, add its location to `PATH`, or set `ONNXRUNTIME_DIR`.
- On Linux: Add its parent `lib` directory to `LD_LIBRARY_PATH` (or copy to `/usr/local/lib`).
- Note: IRIS does not require rebuilding with `cargo build`.

### "LibTorch plugin not found"

**Cause**: `iris_torch_plugin.dll` / `libiris_torch_plugin.so` is missing.

**Solution**:
Compile the dynamic C-ABI bridge plugin once:
```sh
LIBTORCH_DIR=/path/to/libtorch cargo run --release -- <build_command>
```
Place `iris_torch_plugin` and LibTorch libraries on your system library path.

### Test models not found

**Cause**: `tests/fixtures/` directory missing model files.

**Solution**:
```bash
cd tests
pip install onnx numpy torch
python create_onnx_model.py
```

## Platform-Specific Notes

### Windows Specifics

- Shared library: `onnxruntime.dll`, `tensorflow.dll`, `iris_torch_plugin.dll`.
- Place libraries in the executable directory or on `PATH`.

### Linux Specifics

- Shared library: `libonnxruntime.so`, `libtensorflow.so`, `libiris_torch_plugin.so`.
- Set `LD_LIBRARY_PATH` if libraries are in a custom directory:
  ```bash
  export LD_LIBRARY_PATH=/opt/onnxruntime/lib:$LD_LIBRARY_PATH
  ```

### macOS Specifics

- Shared library: `libonnxruntime.dylib`, `libtensorflow.dylib`, `libiris_torch_plugin.dylib`.
- Set `DYLD_LIBRARY_PATH` if needed:
  ```bash
  export DYLD_LIBRARY_PATH=/opt/onnxruntime/lib:$DYLD_LIBRARY_PATH
  ```

## Runtime Dynamic Loading Architecture

Unlike early prototype versions that attempted compile-time static linking:

1. **SDK-Independent Runtime Object**: The precompiled `iris_runtime.o` contains no compile-time SDK dependencies.
2. **Lazy Dynamic Binding**: Entry points are dynamically bound via `dlopen`/`LoadLibrary` in `src/runtime/iris_ml_dynload.h` only when ML functions are invoked.
3. **API Downward Negotiation**: IRIS negotiates API versions downward at load time so older shared libraries remain compatible.
4. **Obsolete Flags**: `IRIS_NATIVE_ML_BACKENDS` is deprecated, obsolete, and ignored.

## Next Steps

1. Obtain external shared libraries (e.g. `onnxruntime.dll` / `libonnxruntime.so`)
2. Ensure they are placed on your system library search path
3. Generate test models in `tests/fixtures/`
4. Run inference tests: `cargo test --test ml_inference -- --nocapture`

For questions or issues, check the build output logs in `target/debug/build/iris-*/output/`.
