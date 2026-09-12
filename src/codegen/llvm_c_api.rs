//! LLVM C API binding for compiling `.ll` → `.o`/`.obj` without `clang`.
//!
//! Uses `libloading` to load `LLVM-C.dll` at runtime, then calls the stable
//! LLVM C API functions to parse LLVM IR text and emit a native object file.
//!
//! Supported on Windows (LLVM-C.dll) and POSIX (libLLVM.so / libLLVM.dylib).

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::Path;
use std::ptr;
use std::sync::OnceLock;

use libloading::{Library, Symbol};

use crate::error::CodegenError;

// ---------------------------------------------------------------------------
// Opaque LLVM C API types
// ---------------------------------------------------------------------------

#[repr(C)]
struct LLVMOpaqueContext;
#[repr(C)]
struct LLVMOpaqueModule;
#[repr(C)]
struct LLVMOpaqueMemoryBuffer;
#[repr(C)]
struct LLVMOpaqueTargetMachine;
#[repr(C)]
struct LLVMOpaqueTarget;
struct LLVMOpaquePassBuilderOptions;
struct LLVMOpaqueError;

type LLVMContextRef = *mut LLVMOpaqueContext;
type LLVMModuleRef = *mut LLVMOpaqueModule;
type LLVMMemoryBufferRef = *mut LLVMOpaqueMemoryBuffer;
type LLVMTargetMachineRef = *mut LLVMOpaqueTargetMachine;
type LLVMTargetRef = *mut LLVMOpaqueTarget;
type LLVMPassBuilderOptionsRef = *mut LLVMOpaquePassBuilderOptions;
type LLVMErrorRef = *mut LLVMOpaqueError;
type LLVMBool = i32;

// ---------------------------------------------------------------------------
// Function pointer types for LLVM C API
// ---------------------------------------------------------------------------

type FnContextCreate = unsafe extern "C" fn() -> LLVMContextRef;
type FnContextDispose = unsafe extern "C" fn(LLVMContextRef);
type FnCreateMemoryBufferWithMemoryRangeCopy =
    unsafe extern "C" fn(*const c_char, usize, *const c_char) -> LLVMMemoryBufferRef;

type FnParseIRInContext = unsafe extern "C" fn(
    LLVMContextRef,
    LLVMMemoryBufferRef,
    *mut LLVMModuleRef,
    *mut *mut c_char,
) -> LLVMBool;

type FnGetTargetFromTriple =
    unsafe extern "C" fn(*const c_char, *mut LLVMTargetRef, *mut *mut c_char) -> LLVMBool;

type FnCreateTargetMachine = unsafe extern "C" fn(
    LLVMTargetRef,
    *const c_char,
    *const c_char,
    *const c_char,
    u32,
    u32,
    u32,
) -> LLVMTargetMachineRef;

type FnTargetMachineEmitToMemoryBuffer = unsafe extern "C" fn(
    LLVMTargetMachineRef,
    LLVMModuleRef,
    u32,
    *mut *mut c_char,
    *mut LLVMMemoryBufferRef,
) -> LLVMBool;

type FnGetBufferStart = unsafe extern "C" fn(LLVMMemoryBufferRef) -> *const c_char;
type FnGetBufferSize = unsafe extern "C" fn(LLVMMemoryBufferRef) -> usize;
type FnDisposeMemoryBuffer = unsafe extern "C" fn(LLVMMemoryBufferRef);
type FnDisposeModule = unsafe extern "C" fn(LLVMModuleRef);
type FnDisposeMessage = unsafe extern "C" fn(*mut c_char);
type FnDisposeTargetMachine = unsafe extern "C" fn(LLVMTargetMachineRef);
type FnSetTarget = unsafe extern "C" fn(LLVMModuleRef, *const c_char);
type FnInitializeTarget = unsafe extern "C" fn();
type FnRunPasses = unsafe extern "C" fn(
    LLVMModuleRef,
    *const c_char,
    LLVMTargetMachineRef,
    LLVMPassBuilderOptionsRef,
) -> LLVMErrorRef;
type FnCreatePassBuilderOptions = unsafe extern "C" fn() -> LLVMPassBuilderOptionsRef;
type FnDisposePassBuilderOptions = unsafe extern "C" fn(LLVMPassBuilderOptionsRef);
type FnGetErrorMessage = unsafe extern "C" fn(LLVMErrorRef) -> *mut c_char;
type FnDisposeErrorMessage = unsafe extern "C" fn(*mut c_char);

// ---------------------------------------------------------------------------
// Loaded library wrapper: stores raw function pointers
// ---------------------------------------------------------------------------

struct LlvmCApi {
    _lib: Library,

    context_create: FnContextCreate,
    context_dispose: FnContextDispose,
    create_memory_buffer_with_memory_range_copy: FnCreateMemoryBufferWithMemoryRangeCopy,
    parse_ir_in_context: FnParseIRInContext,
    get_target_from_triple: FnGetTargetFromTriple,
    create_target_machine: FnCreateTargetMachine,
    target_machine_emit_to_memory_buffer: FnTargetMachineEmitToMemoryBuffer,
    get_buffer_start: FnGetBufferStart,
    get_buffer_size: FnGetBufferSize,
    dispose_memory_buffer: FnDisposeMemoryBuffer,
    dispose_module: FnDisposeModule,
    dispose_message: FnDisposeMessage,
    dispose_target_machine: FnDisposeTargetMachine,
    set_target: FnSetTarget,
    run_passes: FnRunPasses,
    create_pass_builder_options: FnCreatePassBuilderOptions,
    dispose_pass_builder_options: FnDisposePassBuilderOptions,
    get_error_message: FnGetErrorMessage,
    dispose_error_message: FnDisposeErrorMessage,
}

unsafe impl Send for LlvmCApi {}
unsafe impl Sync for LlvmCApi {}

impl LlvmCApi {
    fn load() -> Result<Self, CodegenError> {
        #[cfg(target_os = "windows")]
        let lib_name = "LLVM-C.dll";
        #[cfg(target_os = "macos")]
        let lib_name = "libLLVM.dylib";
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        let lib_name = "libLLVM.so";

        // Try system PATH / dynamic loader first
        let lib = match unsafe { Library::new(lib_name) } {
            Ok(lib) => lib,
            Err(_) => {
                #[cfg(target_os = "windows")]
                {
                    let candidates = [
                        r"C:\llvm-20\bin\LLVM-C.dll",
                        r"C:\llvm-19\bin\LLVM-C.dll",
                        r"C:\llvm-18\bin\LLVM-C.dll",
                        r"C:\Program Files\LLVM\bin\LLVM-C.dll",
                    ];
                    let mut found = None;
                    for path in &candidates {
                        if std::path::Path::new(path).exists() {
                            match unsafe { Library::new::<&str>(path) } {
                                Ok(lib) => {
                                    found = Some(lib);
                                    break;
                                }
                                Err(e) => {
                                    return Err(CodegenError::Unsupported {
                                        backend: "llvm_c_api".into(),
                                        detail: format!("failed to load '{}': {}", path, e),
                                    });
                                }
                            }
                        }
                    }
                    if let Some(lib) = found {
                        lib
                    } else {
                        return Err(CodegenError::Unsupported {
                            backend: "llvm_c_api".into(),
                            detail: format!(
                                "failed to load '{}' via PATH or any known install path. Is LLVM installed?",
                                lib_name
                            ),
                        });
                    }
                }
                #[cfg(not(target_os = "windows"))]
                {
                    return Err(CodegenError::Unsupported {
                        backend: "llvm_c_api".into(),
                        detail: format!(
                            "failed to load '{}' via standard library paths. Is LLVM installed?",
                            lib_name
                        ),
                    });
                }
            }
        };

        // Helper to load a function symbol
        macro_rules! load {
            ($lib:expr, $fn_ty:ty, $name:expr) => {{
                let sym: Symbol<$fn_ty> = unsafe {
                    $lib.get($name).map_err(|e| CodegenError::Unsupported {
                        backend: "llvm_c_api".into(),
                        detail: format!(
                            "symbol '{}' not found: {}",
                            std::str::from_utf8($name).unwrap_or("?"),
                            e
                        ),
                    })?
                };
                *sym // Deref to get the function pointer copy
            }};
        }

        Ok(Self {
            // Load all function pointers first (borrow `lib`), THEN move `lib` into `_lib`.
            context_create: load!(lib, FnContextCreate, b"LLVMContextCreate\0"),
            context_dispose: load!(lib, FnContextDispose, b"LLVMContextDispose\0"),
            create_memory_buffer_with_memory_range_copy: load!(
                lib,
                FnCreateMemoryBufferWithMemoryRangeCopy,
                b"LLVMCreateMemoryBufferWithMemoryRangeCopy\0"
            ),
            parse_ir_in_context: load!(lib, FnParseIRInContext, b"LLVMParseIRInContext\0"),
            get_target_from_triple: load!(lib, FnGetTargetFromTriple, b"LLVMGetTargetFromTriple\0"),
            create_target_machine: load!(lib, FnCreateTargetMachine, b"LLVMCreateTargetMachine\0"),
            target_machine_emit_to_memory_buffer: load!(
                lib,
                FnTargetMachineEmitToMemoryBuffer,
                b"LLVMTargetMachineEmitToMemoryBuffer\0"
            ),
            get_buffer_start: load!(lib, FnGetBufferStart, b"LLVMGetBufferStart\0"),
            get_buffer_size: load!(lib, FnGetBufferSize, b"LLVMGetBufferSize\0"),
            dispose_memory_buffer: load!(lib, FnDisposeMemoryBuffer, b"LLVMDisposeMemoryBuffer\0"),
            dispose_module: load!(lib, FnDisposeModule, b"LLVMDisposeModule\0"),
            dispose_message: load!(lib, FnDisposeMessage, b"LLVMDisposeMessage\0"),
            dispose_target_machine: load!(
                lib,
                FnDisposeTargetMachine,
                b"LLVMDisposeTargetMachine\0"
            ),
            set_target: load!(lib, FnSetTarget, b"LLVMSetTarget\0"),
            run_passes: load!(lib, FnRunPasses, b"LLVMRunPasses\0"),
            create_pass_builder_options: load!(
                lib,
                FnCreatePassBuilderOptions,
                b"LLVMCreatePassBuilderOptions\0"
            ),
            dispose_pass_builder_options: load!(
                lib,
                FnDisposePassBuilderOptions,
                b"LLVMDisposePassBuilderOptions\0"
            ),
            get_error_message: load!(lib, FnGetErrorMessage, b"LLVMGetErrorMessage\0"),
            dispose_error_message: load!(lib, FnDisposeErrorMessage, b"LLVMDisposeErrorMessage\0"),
            _lib: lib,
        })
    }

    fn initialize_target(&self, triple: &str) -> Result<(), CodegenError> {
        let component = if triple.starts_with("x86_64") || triple.starts_with("i386") {
            "X86"
        } else if triple.starts_with("avr") {
            "AVR"
        } else if triple.starts_with("aarch64") {
            "AArch64"
        } else if triple.starts_with("arm") || triple.starts_with("thumb") {
            "ARM"
        } else if triple.starts_with("wasm") {
            "WebAssembly"
        } else if triple.starts_with("riscv") {
            "RISCV"
        } else if triple.starts_with("xtensa") {
            // Available in Espressif's LLVM distribution.  Stock LLVM builds
            // return a normal missing-symbol diagnostic here.
            "Xtensa"
        } else {
            return Err(CodegenError::Unsupported {
                backend: "llvm_c_api".into(),
                detail: format!(
                    "LLVM-C target initialization is not implemented for '{}'",
                    triple
                ),
            });
        };

        for suffix in ["TargetInfo", "Target", "TargetMC", "AsmPrinter"] {
            let symbol_name = format!("LLVMInitialize{}{}", component, suffix);
            let mut symbol_bytes = symbol_name.as_bytes().to_vec();
            symbol_bytes.push(0);
            let initializer: Symbol<FnInitializeTarget> = unsafe {
                self._lib
                    .get(&symbol_bytes)
                    .map_err(|error| CodegenError::Unsupported {
                        backend: "llvm_c_api".into(),
                        detail: format!("symbol '{}' not found: {}", symbol_name, error),
                    })?
            };
            unsafe { initializer() };
        }
        Ok(())
    }
}

static LLVM_C_API: OnceLock<Result<LlvmCApi, String>> = OnceLock::new();

fn llvm_c_api() -> Result<&'static LlvmCApi, CodegenError> {
    match LLVM_C_API.get_or_init(|| LlvmCApi::load().map_err(|error| error.to_string())) {
        Ok(api) => Ok(api),
        Err(detail) => Err(CodegenError::Unsupported {
            backend: "llvm_c_api".into(),
            detail: detail.clone(),
        }),
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Compile an LLVM IR string (`.ll` text) to a native object file.
///
/// `source_text` — the complete LLVM IR module as a string.
/// `output_path` — where to write the `.o` / `.obj` file.
/// `target_triple` — e.g. `"x86_64-w64-windows-gnu"`.
///   If `None`, detects the host target.
pub fn compile_llvm_ir_to_object(
    source_text: &str,
    output_path: &Path,
    target_triple: Option<&str>,
) -> Result<(), CodegenError> {
    let data = compile_llvm_ir_to_object_bytes(source_text, target_triple)?;
    std::fs::write(output_path, data).map_err(|e| CodegenError::Unsupported {
        backend: "llvm_c_api".into(),
        detail: format!("failed to write object file: {}", e),
    })
}

/// Compile LLVM IR into an in-memory native object.
///
/// This is the common object-emission path for both ahead-of-time builds and
/// ORC JIT loading. Keeping it in memory prevents the JIT from creating a
/// temporary object file or launching an external compiler process.
pub fn compile_llvm_ir_to_object_bytes(
    source_text: &str,
    target_triple: Option<&str>,
) -> Result<Vec<u8>, CodegenError> {
    compile_llvm_ir_to_object_bytes_with_pipeline(source_text, target_triple, None)
}

/// Compile LLVM IR after running a new-pass-manager pipeline such as
/// `default<O1>`.  Cross-target backends use this to canonicalize generic IR
/// before instruction selection; LLVM 17's ARM selector can otherwise crash on
/// legal but uncanonicalized loop/array IR instead of returning an error.
pub fn compile_llvm_ir_to_object_bytes_optimized(
    source_text: &str,
    target_triple: Option<&str>,
    pipeline: &str,
) -> Result<Vec<u8>, CodegenError> {
    compile_llvm_ir_to_object_bytes_with_pipeline(source_text, target_triple, Some(pipeline))
}

fn compile_llvm_ir_to_object_bytes_with_pipeline(
    source_text: &str,
    target_triple: Option<&str>,
    pipeline: Option<&str>,
) -> Result<Vec<u8>, CodegenError> {
    // Keep the dynamically loaded LLVM library alive for the process lifetime.
    // LLVM's global context and target registry retain code/data owned by the
    // library; unloading it after a single emission can crash at process exit.
    let api = llvm_c_api()?;

    let triple = target_triple.unwrap_or({
        if cfg!(target_os = "windows") {
            "x86_64-pc-windows-msvc"
        } else if cfg!(target_os = "macos") {
            "x86_64-apple-darwin"
        } else {
            "x86_64-unknown-linux-gnu"
        }
    });

    let context = unsafe { (api.context_create)() };
    if context.is_null() {
        return Err(CodegenError::Unsupported {
            backend: "llvm_c_api".into(),
            detail: "LLVMContextCreate returned null".into(),
        });
    }
    let module = match parse_ir(api, context, source_text) {
        Ok(module) => module,
        Err(error) => {
            unsafe { (api.context_dispose)(context) };
            return Err(error);
        }
    };

    // Set target triple
    let triple_c = CString::new(triple).map_err(|_| CodegenError::Unsupported {
        backend: "llvm_c_api".into(),
        detail: format!("invalid target triple: '{}'", triple),
    })?;
    unsafe {
        (api.set_target)(module, triple_c.as_ptr());
    }

    if let Err(error) = api.initialize_target(triple) {
        unsafe {
            (api.dispose_module)(module);
            (api.context_dispose)(context);
        }
        return Err(error);
    }

    // Get target
    let mut target: LLVMTargetRef = ptr::null_mut();
    let mut target_error: *mut c_char = ptr::null_mut();
    let failed =
        unsafe { (api.get_target_from_triple)(triple_c.as_ptr(), &mut target, &mut target_error) };
    if failed != 0 || target.is_null() {
        let err = if !target_error.is_null() {
            unsafe { CStr::from_ptr(target_error).to_string_lossy().into_owned() }
        } else {
            "unknown target".to_owned()
        };
        if !target_error.is_null() {
            unsafe { (api.dispose_message)(target_error) };
        }
        unsafe {
            (api.dispose_module)(module);
            (api.context_dispose)(context);
        }
        return Err(CodegenError::Unsupported {
            backend: "llvm_c_api".into(),
            detail: format!("unknown target triple '{}': {}", triple, err),
        });
    }

    // Create target machine
    let cpu_name = if triple.starts_with("avr") {
        "atmega328p"
    } else {
        "generic"
    };
    let cpu = CString::new(cpu_name).unwrap();
    let features = CString::new("").unwrap();
    // LLVMRelocPIC (2) is required for the ASLR-enabled executables produced by
    // the MinGW linker. LLVMRelocStatic (1) can emit absolute references that
    // happen to work under a debugger (which commonly fixes the image base) but
    // access-violate when Windows relocates the image at normal process start.
    const LLVM_CODEGEN_LEVEL_DEFAULT: u32 = 2;
    const LLVM_RELOC_STATIC: u32 = 1;
    const LLVM_RELOC_PIC: u32 = 2;
    const LLVM_CODE_MODEL_DEFAULT: u32 = 0;
    let machine = unsafe {
        (api.create_target_machine)(
            target,
            triple_c.as_ptr(),
            cpu.as_ptr(),
            features.as_ptr(),
            LLVM_CODEGEN_LEVEL_DEFAULT,
            if triple.starts_with("avr") {
                LLVM_RELOC_STATIC
            } else {
                LLVM_RELOC_PIC
            },
            LLVM_CODE_MODEL_DEFAULT,
        )
    };
    if machine.is_null() {
        unsafe {
            (api.dispose_module)(module);
            (api.context_dispose)(context);
        }
        return Err(CodegenError::Unsupported {
            backend: "llvm_c_api".into(),
            detail: format!("failed to create target machine for '{}'", triple),
        });
    }

    if let Some(pipeline) = pipeline {
        let pipeline_c = CString::new(pipeline).map_err(|_| CodegenError::Unsupported {
            backend: "llvm_c_api".into(),
            detail: "LLVM pass pipeline contains a null byte".into(),
        })?;
        let options = unsafe { (api.create_pass_builder_options)() };
        if options.is_null() {
            unsafe {
                (api.dispose_target_machine)(machine);
                (api.dispose_module)(module);
                (api.context_dispose)(context);
            }
            return Err(CodegenError::Unsupported {
                backend: "llvm_c_api".into(),
                detail: "LLVMCreatePassBuilderOptions returned null".into(),
            });
        }
        let pass_error = unsafe { (api.run_passes)(module, pipeline_c.as_ptr(), machine, options) };
        unsafe { (api.dispose_pass_builder_options)(options) };
        if !pass_error.is_null() {
            let message = unsafe { (api.get_error_message)(pass_error) };
            let detail = if message.is_null() {
                format!("LLVM pass pipeline '{}' failed", pipeline)
            } else {
                let detail = unsafe { CStr::from_ptr(message).to_string_lossy().into_owned() };
                unsafe { (api.dispose_error_message)(message) };
                detail
            };
            unsafe {
                (api.dispose_target_machine)(machine);
                (api.dispose_module)(module);
                (api.context_dispose)(context);
            }
            return Err(CodegenError::Unsupported {
                backend: "llvm_c_api".into(),
                detail: format!("LLVM pass pipeline '{}' failed: {}", pipeline, detail),
            });
        }
    }

    // Emit object file to memory buffer
    let mut emit_error: *mut c_char = ptr::null_mut();
    let mut obj_buf: LLVMMemoryBufferRef = ptr::null_mut();
    let emit_failed = unsafe {
        (api.target_machine_emit_to_memory_buffer)(
            machine,
            module,
            1,
            &mut emit_error,
            &mut obj_buf,
        )
    };
    if emit_failed != 0 || obj_buf.is_null() {
        let err = if !emit_error.is_null() {
            unsafe { CStr::from_ptr(emit_error).to_string_lossy().into_owned() }
        } else {
            "unknown error".to_owned()
        };
        if !emit_error.is_null() {
            unsafe { (api.dispose_message)(emit_error) };
        }
        unsafe {
            (api.dispose_target_machine)(machine);
            (api.dispose_module)(module);
            (api.context_dispose)(context);
        }
        return Err(CodegenError::Unsupported {
            backend: "llvm_c_api".into(),
            detail: format!("failed to emit object code: {}", err),
        });
    }

    // Write object data to file
    let data_ptr = unsafe { (api.get_buffer_start)(obj_buf) };
    let data_len = unsafe { (api.get_buffer_size)(obj_buf) };
    let data = unsafe { std::slice::from_raw_parts(data_ptr as *const u8, data_len) }.to_vec();

    unsafe {
        (api.dispose_memory_buffer)(obj_buf);
        (api.dispose_target_machine)(machine);
        (api.dispose_module)(module);
        (api.context_dispose)(context);
    }

    validate_object_bytes(&data, triple)?;
    Ok(data)
}

/// Check if the LLVM C API library is available on this system.
pub fn is_llvm_c_api_available() -> bool {
    llvm_c_api().is_ok()
}

/// Return whether the loaded LLVM distribution contains the backend for a
/// specific target triple.
///
/// LLVM packages are commonly built with only a subset of targets. In
/// particular, the stock Windows package exposes LLVM-C but omits AVR. A
/// library-level availability check cannot distinguish that case.
pub fn is_llvm_target_available(triple: &str) -> bool {
    llvm_c_api()
        .and_then(|api| api.initialize_target(triple))
        .is_ok()
}

/// Initialize the LLVM target components needed for the host JIT.
pub(crate) fn initialize_native_target() -> Result<(), CodegenError> {
    llvm_c_api()?.initialize_target(crate::codegen::llvm_ir::native_target_triple())
}

// ---------------------------------------------------------------------------
// Internal: parse LLVM IR text into a module
// ---------------------------------------------------------------------------

fn parse_ir(
    api: &LlvmCApi,
    context: LLVMContextRef,
    source: &str,
) -> Result<LLVMModuleRef, CodegenError> {
    let c_source = CString::new(source).map_err(|_| CodegenError::Unsupported {
        backend: "llvm_c_api".into(),
        detail: "LLVM IR source contains null byte".to_owned(),
    })?;
    let name = CString::new("module.ll").unwrap();

    let mem_buf = unsafe {
        (api.create_memory_buffer_with_memory_range_copy)(
            c_source.as_ptr(),
            source.len(),
            name.as_ptr(),
        )
    };
    if mem_buf.is_null() {
        return Err(CodegenError::Unsupported {
            backend: "llvm_c_api".into(),
            detail: "failed to create memory buffer".to_owned(),
        });
    }

    let mut module: LLVMModuleRef = ptr::null_mut();
    let mut parse_error: *mut c_char = ptr::null_mut();

    // LLVMParseIRInContext consumes MemBuf on both success and failure. Calling
    // LLVMDisposeMemoryBuffer here was a double-free (the newer
    // LLVMParseIRInContext2 has the opposite ownership rule).
    let failed =
        unsafe { (api.parse_ir_in_context)(context, mem_buf, &mut module, &mut parse_error) };

    if failed != 0 {
        let err = if !parse_error.is_null() {
            unsafe { CStr::from_ptr(parse_error).to_string_lossy().into_owned() }
        } else {
            "parse failed".to_owned()
        };
        if !parse_error.is_null() {
            unsafe { (api.dispose_message)(parse_error) };
        }
        return Err(CodegenError::Unsupported {
            backend: "llvm_c_api".into(),
            detail: format!("failed to parse LLVM IR: {}", err),
        });
    }

    Ok(module)
}

fn validate_object_bytes(data: &[u8], triple: &str) -> Result<(), CodegenError> {
    let valid = if triple.contains("windows") {
        matches!(
            data.get(0..2),
            Some([0x64, 0x86] | [0x4c, 0x01] | [0x64, 0xaa])
        )
    } else if triple.contains("darwin") || triple.contains("apple") {
        matches!(
            data.get(0..4),
            Some([0xcf, 0xfa, 0xed, 0xfe] | [0xfe, 0xed, 0xfa, 0xcf])
        )
    } else if triple.starts_with("wasm32") {
        data.starts_with(b"\0asm")
    } else {
        data.starts_with(b"\x7fELF")
    };

    if valid {
        Ok(())
    } else {
        Err(CodegenError::Unsupported {
            backend: "llvm_c_api".into(),
            detail: format!(
                "LLVM reported successful object emission for '{}' but returned {} bytes with an invalid object header",
                triple,
                data.len()
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{compile_llvm_ir_to_object, is_llvm_c_api_available, validate_object_bytes};

    #[test]
    fn validates_native_object_container_magic() {
        assert!(validate_object_bytes(&[0x64, 0x86, 0, 0], "x86_64-pc-windows-gnu").is_ok());
        assert!(validate_object_bytes(b"\x7fELFrest", "x86_64-unknown-linux-gnu").is_ok());
        assert!(
            validate_object_bytes(&[0xcf, 0xfa, 0xed, 0xfe, 0, 0], "aarch64-apple-darwin").is_ok()
        );
        assert!(validate_object_bytes(b"\0asmrest", "wasm32-wasip1").is_ok());
    }

    #[test]
    fn rejects_success_with_non_object_bytes() {
        let err = validate_object_bytes(b"not an object", "x86_64-pc-windows-gnu")
            .expect_err("garbage must not be accepted as a COFF object");
        assert!(err.to_string().contains("invalid object header"));
    }

    #[test]
    fn emits_a_real_object_when_llvm_c_is_installed() {
        if !is_llvm_c_api_available() {
            return;
        }
        let unique = format!(
            "iris_llvm_c_api_{}_{}.o",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique);
        compile_llvm_ir_to_object("define i32 @iris_probe() { ret i32 7 }\n", &path, None)
            .expect("LLVM C API object emission");
        let bytes = std::fs::read(&path).expect("read emitted object");
        assert!(bytes.len() > 16, "emitted object was unexpectedly small");
        let _ = std::fs::remove_file(path);
    }
}
