//! In-process LLVM ORC/LLJIT loader.
//!
//! IRIS loads the stable LLVM-C ORC API dynamically, so the compiler does not
//! pin a build-time `llvm-sys` version. Modules are emitted to in-memory object
//! bytes and installed under an ORC resource tracker. Dropping a loaded module
//! removes its code after all owners have released it.

use std::collections::HashMap;
use std::ffi::{c_void, CStr, CString};
use std::os::raw::c_char;
use std::ptr;
use std::sync::{Arc, OnceLock};

use libloading::{Library, Symbol};

use crate::error::CodegenError;
use crate::ir::module::IrModule;
use crate::ir::types::{DType, IrType};

#[repr(C)]
struct LLVMOpaqueError;
#[repr(C)]
struct LLVMOrcOpaqueLLJIT;
#[repr(C)]
struct LLVMOrcOpaqueLLJITBuilder;
#[repr(C)]
struct LLVMOrcOpaqueJITDylib;
#[repr(C)]
struct LLVMOrcOpaqueResourceTracker;
#[repr(C)]
struct LLVMOrcOpaqueDefinitionGenerator;
#[repr(C)]
struct LLVMOrcOpaqueSymbolStringPoolEntry;
#[repr(C)]
struct LLVMOrcOpaqueMaterializationUnit;
#[repr(C)]
struct LLVMOpaqueMemoryBuffer;

type LLVMErrorRef = *mut LLVMOpaqueError;
type LLVMOrcLLJITRef = *mut LLVMOrcOpaqueLLJIT;
type LLVMOrcLLJITBuilderRef = *mut LLVMOrcOpaqueLLJITBuilder;
type LLVMOrcJITDylibRef = *mut LLVMOrcOpaqueJITDylib;
type LLVMOrcResourceTrackerRef = *mut LLVMOrcOpaqueResourceTracker;
type LLVMOrcDefinitionGeneratorRef = *mut LLVMOrcOpaqueDefinitionGenerator;
type LLVMOrcSymbolStringPoolEntryRef = *mut LLVMOrcOpaqueSymbolStringPoolEntry;
type LLVMOrcMaterializationUnitRef = *mut LLVMOrcOpaqueMaterializationUnit;
type LLVMMemoryBufferRef = *mut LLVMOpaqueMemoryBuffer;
type LLVMOrcExecutorAddress = u64;
type LLVMOrcSymbolPredicate =
    Option<unsafe extern "C" fn(*mut c_void, LLVMOrcSymbolStringPoolEntryRef) -> i32>;

type FnCreateLLJIT =
    unsafe extern "C" fn(*mut LLVMOrcLLJITRef, LLVMOrcLLJITBuilderRef) -> LLVMErrorRef;
type FnDisposeLLJIT = unsafe extern "C" fn(LLVMOrcLLJITRef) -> LLVMErrorRef;
type FnGetMainJITDylib = unsafe extern "C" fn(LLVMOrcLLJITRef) -> LLVMOrcJITDylibRef;
type FnGetGlobalPrefix = unsafe extern "C" fn(LLVMOrcLLJITRef) -> c_char;
type FnAddObjectFileWithRT = unsafe extern "C" fn(
    LLVMOrcLLJITRef,
    LLVMOrcResourceTrackerRef,
    LLVMMemoryBufferRef,
) -> LLVMErrorRef;
type FnLookup = unsafe extern "C" fn(
    LLVMOrcLLJITRef,
    *mut LLVMOrcExecutorAddress,
    *const c_char,
) -> LLVMErrorRef;
type FnCreateResourceTracker =
    unsafe extern "C" fn(LLVMOrcJITDylibRef) -> LLVMOrcResourceTrackerRef;
type FnRemoveResourceTracker = unsafe extern "C" fn(LLVMOrcResourceTrackerRef) -> LLVMErrorRef;
type FnReleaseResourceTracker = unsafe extern "C" fn(LLVMOrcResourceTrackerRef);
type FnCreateProcessGenerator = unsafe extern "C" fn(
    *mut LLVMOrcDefinitionGeneratorRef,
    c_char,
    LLVMOrcSymbolPredicate,
    *mut c_void,
) -> LLVMErrorRef;
type FnAddGenerator = unsafe extern "C" fn(LLVMOrcJITDylibRef, LLVMOrcDefinitionGeneratorRef);
type FnCreateMemoryBufferWithMemoryRangeCopy =
    unsafe extern "C" fn(*const c_char, usize, *const c_char) -> LLVMMemoryBufferRef;
type FnGetErrorMessage = unsafe extern "C" fn(LLVMErrorRef) -> *mut c_char;
type FnDisposeErrorMessage = unsafe extern "C" fn(*mut c_char);
type FnMangleAndIntern =
    unsafe extern "C" fn(LLVMOrcLLJITRef, *const c_char) -> LLVMOrcSymbolStringPoolEntryRef;
type FnAbsoluteSymbols =
    unsafe extern "C" fn(*mut LLVMOrcCSymbolMapPair, usize) -> LLVMOrcMaterializationUnitRef;
type FnJITDylibDefine =
    unsafe extern "C" fn(LLVMOrcJITDylibRef, LLVMOrcMaterializationUnitRef) -> LLVMErrorRef;
type FnDisposeMaterializationUnit = unsafe extern "C" fn(LLVMOrcMaterializationUnitRef);

#[repr(C)]
#[derive(Clone, Copy)]
struct LLVMJITSymbolFlags {
    generic_flags: u8,
    target_flags: u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct LLVMJITEvaluatedSymbol {
    address: LLVMOrcExecutorAddress,
    flags: LLVMJITSymbolFlags,
}

#[repr(C)]
struct LLVMOrcCSymbolMapPair {
    name: LLVMOrcSymbolStringPoolEntryRef,
    symbol: LLVMJITEvaluatedSymbol,
}

include!(concat!(env!("OUT_DIR"), "/jit_runtime_symbols.rs"));

extern "C" {
    fn iris_runtime_resolve(name: *const c_char) -> *mut c_void;
}

struct OrcApi {
    _lib: Library,
    create_lljit: FnCreateLLJIT,
    dispose_lljit: FnDisposeLLJIT,
    get_main_jit_dylib: FnGetMainJITDylib,
    get_global_prefix: FnGetGlobalPrefix,
    add_object_file_with_rt: FnAddObjectFileWithRT,
    lookup: FnLookup,
    create_resource_tracker: FnCreateResourceTracker,
    remove_resource_tracker: FnRemoveResourceTracker,
    release_resource_tracker: FnReleaseResourceTracker,
    create_process_generator: FnCreateProcessGenerator,
    add_generator: FnAddGenerator,
    create_memory_buffer_with_memory_range_copy: FnCreateMemoryBufferWithMemoryRangeCopy,
    get_error_message: FnGetErrorMessage,
    dispose_error_message: FnDisposeErrorMessage,
    mangle_and_intern: FnMangleAndIntern,
    absolute_symbols: FnAbsoluteSymbols,
    jit_dylib_define: FnJITDylibDefine,
    dispose_materialization_unit: FnDisposeMaterializationUnit,
}

unsafe impl Send for OrcApi {}
unsafe impl Sync for OrcApi {}

impl OrcApi {
    fn load() -> Result<Self, CodegenError> {
        let lib = load_llvm_library()?;
        macro_rules! load {
            ($ty:ty, $name:expr) => {{
                let symbol: Symbol<$ty> = unsafe {
                    lib.get($name).map_err(|error| CodegenError::Unsupported {
                        backend: "jit".into(),
                        detail: format!(
                            "LLVM ORC symbol '{}' is unavailable: {}",
                            std::str::from_utf8($name).unwrap_or("?"),
                            error
                        ),
                    })?
                };
                *symbol
            }};
        }

        Ok(Self {
            create_lljit: load!(FnCreateLLJIT, b"LLVMOrcCreateLLJIT\0"),
            dispose_lljit: load!(FnDisposeLLJIT, b"LLVMOrcDisposeLLJIT\0"),
            get_main_jit_dylib: load!(FnGetMainJITDylib, b"LLVMOrcLLJITGetMainJITDylib\0"),
            get_global_prefix: load!(FnGetGlobalPrefix, b"LLVMOrcLLJITGetGlobalPrefix\0"),
            add_object_file_with_rt: load!(
                FnAddObjectFileWithRT,
                b"LLVMOrcLLJITAddObjectFileWithRT\0"
            ),
            lookup: load!(FnLookup, b"LLVMOrcLLJITLookup\0"),
            create_resource_tracker: load!(
                FnCreateResourceTracker,
                b"LLVMOrcJITDylibCreateResourceTracker\0"
            ),
            remove_resource_tracker: load!(
                FnRemoveResourceTracker,
                b"LLVMOrcResourceTrackerRemove\0"
            ),
            release_resource_tracker: load!(
                FnReleaseResourceTracker,
                b"LLVMOrcReleaseResourceTracker\0"
            ),
            create_process_generator: load!(
                FnCreateProcessGenerator,
                b"LLVMOrcCreateDynamicLibrarySearchGeneratorForProcess\0"
            ),
            add_generator: load!(FnAddGenerator, b"LLVMOrcJITDylibAddGenerator\0"),
            create_memory_buffer_with_memory_range_copy: load!(
                FnCreateMemoryBufferWithMemoryRangeCopy,
                b"LLVMCreateMemoryBufferWithMemoryRangeCopy\0"
            ),
            get_error_message: load!(FnGetErrorMessage, b"LLVMGetErrorMessage\0"),
            dispose_error_message: load!(FnDisposeErrorMessage, b"LLVMDisposeErrorMessage\0"),
            mangle_and_intern: load!(FnMangleAndIntern, b"LLVMOrcLLJITMangleAndIntern\0"),
            absolute_symbols: load!(FnAbsoluteSymbols, b"LLVMOrcAbsoluteSymbols\0"),
            jit_dylib_define: load!(FnJITDylibDefine, b"LLVMOrcJITDylibDefine\0"),
            dispose_materialization_unit: load!(
                FnDisposeMaterializationUnit,
                b"LLVMOrcDisposeMaterializationUnit\0"
            ),
            _lib: lib,
        })
    }

    fn check(&self, error: LLVMErrorRef, operation: &str) -> Result<(), CodegenError> {
        if error.is_null() {
            return Ok(());
        }
        let message = unsafe { (self.get_error_message)(error) };
        let detail = if message.is_null() {
            "unknown LLVM ORC error".to_owned()
        } else {
            let detail = unsafe { CStr::from_ptr(message) }
                .to_string_lossy()
                .into_owned();
            unsafe { (self.dispose_error_message)(message) };
            detail
        };
        Err(CodegenError::Unsupported {
            backend: "jit".into(),
            detail: format!("{}: {}", operation, detail),
        })
    }
}

fn load_llvm_library() -> Result<Library, CodegenError> {
    #[cfg(target_os = "windows")]
    let names: &[&str] = &[
        "LLVM-C.dll",
        r"C:\llvm-20\bin\LLVM-C.dll",
        r"C:\llvm-19\bin\LLVM-C.dll",
        r"C:\llvm-18\bin\LLVM-C.dll",
        r"C:\Program Files\LLVM\bin\LLVM-C.dll",
    ];
    #[cfg(target_os = "macos")]
    let names: &[&str] = &["libLLVM.dylib"];
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let names: &[&str] = &[
        "libLLVM.so",
        "libLLVM-20.so",
        "libLLVM-19.so",
        "libLLVM-18.so",
    ];

    let mut failures = Vec::new();
    for name in names {
        match unsafe { Library::new(name) } {
            Ok(lib) => return Ok(lib),
            Err(error) => failures.push(format!("{}: {}", name, error)),
        }
    }
    Err(CodegenError::Unsupported {
        backend: "jit".into(),
        detail: format!(
            "could not load an LLVM-C library with ORC support ({})",
            failures.join("; ")
        ),
    })
}

static ORC_API: OnceLock<Result<OrcApi, String>> = OnceLock::new();

fn orc_api() -> Result<&'static OrcApi, CodegenError> {
    match ORC_API.get_or_init(|| OrcApi::load().map_err(|error| error.to_string())) {
        Ok(api) => Ok(api),
        Err(detail) => Err(CodegenError::Unsupported {
            backend: "jit".into(),
            detail: detail.clone(),
        }),
    }
}

struct OrcSession {
    api: &'static OrcApi,
    jit: LLVMOrcLLJITRef,
    main_dylib: LLVMOrcJITDylibRef,
}

unsafe impl Send for OrcSession {}
unsafe impl Sync for OrcSession {}

impl Drop for OrcSession {
    fn drop(&mut self) {
        let error = unsafe { (self.api.dispose_lljit)(self.jit) };
        let _ = self.api.check(error, "dispose LLJIT");
    }
}

/// A supported in-process ORC JIT session.
#[derive(Clone)]
pub struct OrcJitEngine {
    session: Arc<OrcSession>,
}

impl OrcJitEngine {
    pub fn new() -> Result<Self, CodegenError> {
        crate::codegen::llvm_c_api::initialize_native_target()?;
        let api = orc_api()?;
        let mut jit = ptr::null_mut();
        let error = unsafe { (api.create_lljit)(&mut jit, ptr::null_mut()) };
        api.check(error, "create LLJIT")?;
        if jit.is_null() {
            return Err(CodegenError::Unsupported {
                backend: "jit".into(),
                detail: "LLVMOrcCreateLLJIT returned a null session".into(),
            });
        }
        let main_dylib = unsafe { (api.get_main_jit_dylib)(jit) };
        if main_dylib.is_null() {
            let error = unsafe { (api.dispose_lljit)(jit) };
            let _ = api.check(error, "dispose incomplete LLJIT");
            return Err(CodegenError::Unsupported {
                backend: "jit".into(),
                detail: "LLJIT has no main JITDylib".into(),
            });
        }

        let mut generator = ptr::null_mut();
        let prefix = unsafe { (api.get_global_prefix)(jit) };
        let error = unsafe {
            (api.create_process_generator)(&mut generator, prefix, None, ptr::null_mut())
        };
        api.check(error, "create process symbol generator")?;
        unsafe { (api.add_generator)(main_dylib, generator) };

        let engine = Self {
            session: Arc::new(OrcSession {
                api,
                jit,
                main_dylib,
            }),
        };
        engine.install_runtime_symbols()?;
        Ok(engine)
    }

    pub fn load_module(&self, module: &IrModule) -> Result<OrcJitModule, CodegenError> {
        let llvm_ir = crate::codegen::llvm_ir::emit_llvm_ir(module)?;
        let object = crate::codegen::llvm_c_api::compile_llvm_ir_to_object_bytes(
            &llvm_ir,
            Some(crate::codegen::llvm_ir::native_target_triple()),
        )?;
        self.load_object(module, &object)
    }

    fn load_object(&self, module: &IrModule, object: &[u8]) -> Result<OrcJitModule, CodegenError> {
        let api = self.session.api;
        let tracker = unsafe { (api.create_resource_tracker)(self.session.main_dylib) };
        if tracker.is_null() {
            return Err(CodegenError::Unsupported {
                backend: "jit".into(),
                detail: "failed to create an ORC resource tracker".into(),
            });
        }
        let name =
            CString::new(format!("{}.o", module.name)).map_err(|_| CodegenError::Unsupported {
                backend: "jit".into(),
                detail: "module name contains a null byte".into(),
            })?;
        let buffer = unsafe {
            (api.create_memory_buffer_with_memory_range_copy)(
                object.as_ptr() as *const c_char,
                object.len(),
                name.as_ptr(),
            )
        };
        if buffer.is_null() {
            unsafe { (api.release_resource_tracker)(tracker) };
            return Err(CodegenError::Unsupported {
                backend: "jit".into(),
                detail: "failed to create an object memory buffer".into(),
            });
        }
        let error = unsafe { (api.add_object_file_with_rt)(self.session.jit, tracker, buffer) };
        if let Err(error) = api.check(error, "add object to LLJIT") {
            unsafe { (api.release_resource_tracker)(tracker) };
            return Err(error);
        }

        let signatures = module
            .functions()
            .iter()
            .map(|function| {
                (
                    function.name.clone(),
                    JitSignature {
                        params: function
                            .params
                            .iter()
                            .map(|param| param.ty.clone())
                            .collect(),
                        result: function.return_ty.clone(),
                    },
                )
            })
            .collect();

        Ok(OrcJitModule {
            session: Arc::clone(&self.session),
            tracker,
            signatures,
        })
    }

    fn install_runtime_symbols(&self) -> Result<(), CodegenError> {
        const EXPORTED_AND_CALLABLE: u8 = (1 << 0) | (1 << 2);
        let api = self.session.api;
        let mut pairs = Vec::with_capacity(JIT_RUNTIME_SYMBOL_NAMES.len() + 2);
        for name in JIT_RUNTIME_SYMBOL_NAMES {
            let c_name = CString::new(*name).expect("generated runtime symbol contains null byte");
            let address = unsafe { iris_runtime_resolve(c_name.as_ptr()) };
            if address.is_null() {
                return Err(CodegenError::Unsupported {
                    backend: "jit".into(),
                    detail: format!(
                        "linked runtime could not resolve generated symbol '{}'",
                        name
                    ),
                });
            }
            let interned = unsafe { (api.mangle_and_intern)(self.session.jit, c_name.as_ptr()) };
            if interned.is_null() {
                return Err(CodegenError::Unsupported {
                    backend: "jit".into(),
                    detail: format!("LLVM ORC could not intern runtime symbol '{}'", name),
                });
            }
            pairs.push(LLVMOrcCSymbolMapPair {
                name: interned,
                symbol: LLVMJITEvaluatedSymbol {
                    address: address as usize as u64,
                    flags: LLVMJITSymbolFlags {
                        generic_flags: EXPORTED_AND_CALLABLE,
                        target_flags: 0,
                    },
                },
            });
        }

        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        {
            let implementation = CString::new("iris_chkstk_ms").unwrap();
            let address = unsafe { iris_runtime_resolve(implementation.as_ptr()) };
            if address.is_null() {
                return Err(CodegenError::Unsupported {
                    backend: "jit".into(),
                    detail: "linked runtime could not resolve the MinGW stack probe".into(),
                });
            }
            let external = CString::new("___chkstk_ms").unwrap();
            let interned = unsafe { (api.mangle_and_intern)(self.session.jit, external.as_ptr()) };
            if interned.is_null() {
                return Err(CodegenError::Unsupported {
                    backend: "jit".into(),
                    detail: "LLVM ORC could not intern the MinGW stack probe".into(),
                });
            }
            pairs.push(LLVMOrcCSymbolMapPair {
                name: interned,
                symbol: LLVMJITEvaluatedSymbol {
                    address: address as usize as u64,
                    flags: LLVMJITSymbolFlags {
                        generic_flags: EXPORTED_AND_CALLABLE,
                        target_flags: 0,
                    },
                },
            });

            // LLVM's MinGW backend inserts `call @__main()` into a user
            // function literally named `main`. Executables resolve that from
            // libgcc, while ORC loads an object without an archive-link phase.
            let implementation = CString::new("iris_mingw_main").unwrap();
            let address = unsafe { iris_runtime_resolve(implementation.as_ptr()) };
            if address.is_null() {
                return Err(CodegenError::Unsupported {
                    backend: "jit".into(),
                    detail: "linked runtime could not resolve the MinGW main initializer".into(),
                });
            }
            let external = CString::new("__main").unwrap();
            let interned = unsafe { (api.mangle_and_intern)(self.session.jit, external.as_ptr()) };
            if interned.is_null() {
                return Err(CodegenError::Unsupported {
                    backend: "jit".into(),
                    detail: "LLVM ORC could not intern the MinGW main initializer".into(),
                });
            }
            pairs.push(LLVMOrcCSymbolMapPair {
                name: interned,
                symbol: LLVMJITEvaluatedSymbol {
                    address: address as usize as u64,
                    flags: LLVMJITSymbolFlags {
                        generic_flags: EXPORTED_AND_CALLABLE,
                        target_flags: 0,
                    },
                },
            });
        }

        let unit = unsafe { (api.absolute_symbols)(pairs.as_mut_ptr(), pairs.len()) };
        if unit.is_null() {
            return Err(CodegenError::Unsupported {
                backend: "jit".into(),
                detail: "LLVMOrcAbsoluteSymbols returned a null materialization unit".into(),
            });
        }
        let error = unsafe { (api.jit_dylib_define)(self.session.main_dylib, unit) };
        if error.is_null() {
            Ok(())
        } else {
            unsafe { (api.dispose_materialization_unit)(unit) };
            api.check(error, "register IRIS runtime symbols")
        }
    }
}

/// Code and metadata installed in one removable ORC generation.
pub struct OrcJitModule {
    session: Arc<OrcSession>,
    tracker: LLVMOrcResourceTrackerRef,
    signatures: HashMap<String, JitSignature>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum JitValue {
    I64(i64),
    I32(i32),
    I8(i8),
    F64(f64),
    F32(f32),
    Bool(bool),
    Str(String),
    Ptr(usize),
}

impl std::fmt::Display for JitValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::I64(value) => write!(formatter, "{}", value),
            Self::I32(value) => write!(formatter, "{}", value),
            Self::I8(value) => write!(formatter, "{}", value),
            Self::F64(value) => write!(formatter, "{}", value),
            Self::F32(value) => write!(formatter, "{}", value),
            Self::Bool(value) => write!(formatter, "{}", value),
            Self::Str(value) => formatter.write_str(value),
            Self::Ptr(value) => write!(formatter, "0x{:x}", value),
        }
    }
}

unsafe impl Send for OrcJitModule {}
unsafe impl Sync for OrcJitModule {}

#[derive(Clone)]
struct JitSignature {
    params: Vec<IrType>,
    result: IrType,
}

impl OrcJitModule {
    /// Force all requested symbols to materialize before a containing program
    /// generation can be published. This is intentionally all-or-nothing from
    /// the dispatcher's perspective: an error leaves the module private.
    pub fn materialize_symbols<'a>(
        &self,
        names: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), CodegenError> {
        for name in names {
            if !self.signatures.contains_key(name) {
                return Err(CodegenError::Unsupported {
                    backend: "jit".into(),
                    detail: format!("function '{name}' is not part of this JIT generation"),
                });
            }
            unsafe { self.lookup_address(name)? };
        }
        Ok(())
    }

    pub fn call_zero_arg(&self, name: &str) -> Result<JitValue, CodegenError> {
        let signature = self
            .signatures
            .get(name)
            .ok_or_else(|| CodegenError::Unsupported {
                backend: "jit".into(),
                detail: format!("function '{}' is not part of this JIT generation", name),
            })?;
        if !signature.params.is_empty() {
            return Err(CodegenError::Unsupported {
                backend: "jit".into(),
                detail: format!("function '{}' requires arguments", name),
            });
        }
        let address = unsafe { self.lookup_address(name)? };
        match &signature.result {
            IrType::Scalar(DType::I64 | DType::U64 | DType::USize) => {
                let function: extern "C" fn() -> i64 = unsafe { std::mem::transmute(address) };
                Ok(JitValue::I64(function()))
            }
            IrType::Scalar(DType::I32 | DType::U32) => {
                let function: extern "C" fn() -> i32 = unsafe { std::mem::transmute(address) };
                Ok(JitValue::I32(function()))
            }
            IrType::Scalar(DType::I8 | DType::U8) => {
                let function: extern "C" fn() -> i8 = unsafe { std::mem::transmute(address) };
                Ok(JitValue::I8(function()))
            }
            IrType::Scalar(DType::F64) => {
                let function: extern "C" fn() -> f64 = unsafe { std::mem::transmute(address) };
                Ok(JitValue::F64(function()))
            }
            IrType::Scalar(DType::F32) => {
                let function: extern "C" fn() -> f32 = unsafe { std::mem::transmute(address) };
                Ok(JitValue::F32(function()))
            }
            IrType::Scalar(DType::Bool) => {
                let function: extern "C" fn() -> bool = unsafe { std::mem::transmute(address) };
                Ok(JitValue::Bool(function()))
            }
            IrType::Str => {
                let function: extern "C" fn() -> *const c_char =
                    unsafe { std::mem::transmute(address) };
                let value = function();
                if value.is_null() {
                    Ok(JitValue::Str(String::new()))
                } else {
                    Ok(JitValue::Str(
                        unsafe { CStr::from_ptr(value) }
                            .to_string_lossy()
                            .into_owned(),
                    ))
                }
            }
            _ => {
                let function: extern "C" fn() -> *mut c_void =
                    unsafe { std::mem::transmute(address) };
                Ok(JitValue::Ptr(function() as usize))
            }
        }
    }

    /// Look up a raw callable address. Calling it is unsafe unless the caller
    /// exactly matches the IRIS function's native ABI.
    ///
    /// # Safety
    ///
    /// The returned address must only be cast to and called through a function
    /// pointer whose calling convention, parameters, and return type exactly
    /// match the compiled IRIS function signature.
    pub unsafe fn lookup_address(&self, name: &str) -> Result<usize, CodegenError> {
        if !self.signatures.contains_key(name) {
            return Err(CodegenError::Unsupported {
                backend: "jit".into(),
                detail: format!("function '{}' is not part of this JIT generation", name),
            });
        }
        let name = CString::new(name).map_err(|_| CodegenError::Unsupported {
            backend: "jit".into(),
            detail: "function name contains a null byte".into(),
        })?;
        let mut address = 0;
        let error = (self.session.api.lookup)(self.session.jit, &mut address, name.as_ptr());
        self.session.api.check(error, "look up JIT function")?;
        if address == 0 {
            return Err(CodegenError::Unsupported {
                backend: "jit".into(),
                detail: "LLVM ORC returned a null function address".into(),
            });
        }
        Ok(address as usize)
    }

    pub fn call_i64_0(&self, name: &str) -> Result<i64, CodegenError> {
        self.require_signature(name, &[], &IrType::Scalar(DType::I64))?;
        let address = unsafe { self.lookup_address(name)? };
        let function: extern "C" fn() -> i64 = unsafe { std::mem::transmute(address) };
        Ok(function())
    }

    pub fn call_i64_1(&self, name: &str, arg: i64) -> Result<i64, CodegenError> {
        let i64_ty = IrType::Scalar(DType::I64);
        self.require_signature(name, std::slice::from_ref(&i64_ty), &i64_ty)?;
        let address = unsafe { self.lookup_address(name)? };
        let function: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(address) };
        Ok(function(arg))
    }

    /// Call the stable IRIS string gateway ABI `(str) -> str`.
    ///
    /// Gateway inputs may not contain interior NUL bytes. The returned string
    /// is copied before this method returns, so it does not borrow JIT memory.
    pub fn call_str_1(&self, name: &str, arg: &str) -> Result<String, CodegenError> {
        self.require_signature(name, &[IrType::Str], &IrType::Str)?;
        let arg = CString::new(arg).map_err(|_| CodegenError::Unsupported {
            backend: "jit".into(),
            detail: "string gateway input contains an interior NUL byte".into(),
        })?;
        let address = unsafe { self.lookup_address(name)? };
        let function: unsafe extern "C" fn(*const c_char) -> *const c_char =
            unsafe { std::mem::transmute(address) };
        let result = unsafe { function(arg.as_ptr()) };
        if result.is_null() {
            return Ok(String::new());
        }
        Ok(unsafe { CStr::from_ptr(result) }
            .to_string_lossy()
            .into_owned())
    }

    pub fn call_f64_0(&self, name: &str) -> Result<f64, CodegenError> {
        self.require_signature(name, &[], &IrType::Scalar(DType::F64))?;
        let address = unsafe { self.lookup_address(name)? };
        let function: extern "C" fn() -> f64 = unsafe { std::mem::transmute(address) };
        Ok(function())
    }

    pub fn call_f64_1(&self, name: &str, arg: f64) -> Result<f64, CodegenError> {
        let f64_ty = IrType::Scalar(DType::F64);
        self.require_signature(name, std::slice::from_ref(&f64_ty), &f64_ty)?;
        let address = unsafe { self.lookup_address(name)? };
        let function: extern "C" fn(f64) -> f64 = unsafe { std::mem::transmute(address) };
        Ok(function(arg))
    }

    fn require_signature(
        &self,
        name: &str,
        params: &[IrType],
        result: &IrType,
    ) -> Result<(), CodegenError> {
        let signature = self
            .signatures
            .get(name)
            .ok_or_else(|| CodegenError::Unsupported {
                backend: "jit".into(),
                detail: format!("function '{}' is not part of this JIT generation", name),
            })?;
        if signature.params != params || &signature.result != result {
            return Err(CodegenError::Unsupported {
                backend: "jit".into(),
                detail: format!(
                    "function '{}' has signature ({}) -> {}, which does not match the requested host ABI",
                    name,
                    signature
                        .params
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", "),
                    signature.result
                ),
            });
        }
        Ok(())
    }
}

impl Drop for OrcJitModule {
    fn drop(&mut self) {
        if self.tracker.is_null() {
            return;
        }
        let error = unsafe { (self.session.api.remove_resource_tracker)(self.tracker) };
        let _ = self.session.api.check(error, "remove JIT generation");
        unsafe { (self.session.api.release_resource_tracker)(self.tracker) };
        self.tracker = ptr::null_mut();
    }
}

pub fn is_orc_jit_available() -> bool {
    // Apple Silicon enforces W^X memory restrictions that cause in-process ORC
    // JIT execution to hang without codesigning entitlements / pthread_jit_write_protect_np.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        return false;
    }
    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    {
        crate::codegen::llvm_c_api::initialize_native_target().is_ok() && orc_api().is_ok()
    }
}
