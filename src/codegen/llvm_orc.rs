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

use crate::codegen::llvm_c_api::{llvm_library_candidates, RECOMMENDED_LLVM_VERSION};
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
    let default_name = "LLVM-C.dll";
    #[cfg(target_os = "macos")]
    let default_name = "libLLVM.dylib";
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let default_name = "libLLVM.so";

    let candidates = llvm_library_candidates(default_name);
    let mut failures = Vec::new();
    for candidate in &candidates {
        match unsafe { Library::new(candidate) } {
            Ok(lib) => return Ok(lib),
            Err(error) => failures.push(format!("{}: {}", candidate.display(), error)),
        }
    }
    Err(CodegenError::Unsupported {
        backend: "jit".into(),
        detail: format!(
            "could not load an LLVM-C library with ORC support for {}. Set IRIS_LLVM_C_API to the shared-library path. Tried: {}",
            RECOMMENDED_LLVM_VERSION,
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
        if let Err(err) = crate::security::validate_module_ffi(&module.extern_fns) {
            return Err(CodegenError::Unsupported {
                backend: "security".into(),
                detail: format!("E0407: {err}"),
            });
        }
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
            state_vars: module.state_vars().to_vec(),
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
    /// The program's state variables (`var` at the top level).
    state_vars: Vec<(String, IrType)>,
}

const MAX_F64_ARGS: usize = 8;

/// One field of a record crossing the host boundary
/// ([`OrcJitModule::call_record`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RecordField {
    F64(f64),
    I64(i64),
    Bool(bool),
}

impl RecordField {
    /// The zero value for a field of type `ty`, if records may carry it.
    pub fn zero(ty: &IrType) -> Option<Self> {
        match ty {
            IrType::Scalar(DType::F64) => Some(Self::F64(0.0)),
            IrType::Scalar(DType::I64) => Some(Self::I64(0)),
            IrType::Scalar(DType::Bool) => Some(Self::Bool(false)),
            _ => None,
        }
    }
}

impl std::fmt::Display for RecordField {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::F64(value) => write!(formatter, "{value:?}"),
            Self::I64(value) => write!(formatter, "{value}"),
            Self::Bool(value) => write!(formatter, "{value}"),
        }
    }
}

/// The native layout of a record whose fields are all `f64`, `i64` or
/// `bool`: LLVM's natural struct layout for `%Name = type { ... }`, each field
/// at its own alignment (8, 8 and 1 bytes) and the whole padded to 8.
/// Returns (offset, field type) per field and the total size.
pub fn record_layout(ty: &IrType) -> Result<(Vec<(usize, DType)>, usize), CodegenError> {
    let IrType::Struct { name, fields } = ty else {
        return Err(CodegenError::Unsupported {
            backend: "jit".into(),
            detail: format!("{ty} is not a record"),
        });
    };
    let mut layout = Vec::with_capacity(fields.len());
    let mut offset = 0usize;
    let mut align = 1usize;
    for (field, field_ty) in fields {
        let (size, field_align, dtype) = match field_ty {
            IrType::Scalar(DType::F64) => (8, 8, DType::F64),
            IrType::Scalar(DType::I64) => (8, 8, DType::I64),
            IrType::Scalar(DType::Bool) => (1, 1, DType::Bool),
            other => {
                return Err(CodegenError::Unsupported {
                    backend: "jit".into(),
                    detail: format!(
                        "field '{field}' of record {name} has type {other}; records cross the host boundary with f64, i64 and bool fields only"
                    ),
                })
            }
        };
        offset = offset.div_ceil(field_align) * field_align;
        layout.push((offset, dtype));
        offset += size;
        align = align.max(field_align);
    }
    Ok((layout, offset.div_ceil(align).max(1) * align))
}

extern "C" {
    fn iris_free_bytes(ptr: *mut c_void);
}

/// A program state value crossing the host boundary, for state handoff
/// between generations.
#[derive(Debug, Clone, PartialEq)]
pub enum StateValue {
    I64(i64),
    F64(f64),
    Bool(bool),
    Str(String),
    /// A list of `i64`, `f64`, `bool` or `str`.
    List(Vec<StateValue>),
    /// A record of `f64`, `i64` and `bool` fields, in declaration order.
    Record(Vec<RecordField>),
}

/// Runtime functions for reading and building state values. They are the
/// same functions the JIT's generated code links against.
mod state_ffi {
    use crate::runtime_bindings::{IrisList, IrisVal};
    use std::ffi::c_void;
    use std::os::raw::{c_char, c_int};
    extern "C" {
        pub fn iris_alloc_bytes(bytes: u64) -> *mut c_void;
        // Typed as in `runtime_bindings`, which declares them too.
        pub fn iris_list_new() -> *mut IrisList;
        pub fn iris_list_push(list: *mut IrisList, value: *mut IrisVal);
        pub fn iris_list_push_i64(list: *mut c_void, value: i64);
        pub fn iris_list_push_f64(list: *mut c_void, value: f64);
        pub fn iris_list_len(list: *mut c_void) -> i64;
        pub fn iris_list_get(list: *mut c_void, index: i64) -> *mut c_void;
        pub fn iris_unbox_i64(value: *mut c_void) -> i64;
        pub fn iris_unbox_f64(value: *mut c_void) -> f64;
        pub fn iris_unbox_bool(value: *mut c_void) -> c_int;
        pub fn iris_unbox_str(value: *mut c_void) -> *mut c_char;
        pub fn iris_box_str(text: *const c_char) -> *mut c_void;
        pub fn iris_box_bool(value: c_int) -> *mut c_void;
        pub fn iris_drop_box(value: *mut c_void);
        pub fn iris_retain_kind(ptr: *mut c_void, kind: i32);
        pub fn iris_release_kind(ptr: *mut c_void, kind: i32);
    }
}

/// Whether state handoff can carry a variable of type `ty`.
pub fn state_type_carried(ty: &IrType) -> bool {
    match ty {
        IrType::Scalar(DType::I64 | DType::F64 | DType::Bool) | IrType::Str => true,
        IrType::List(elem) => matches!(
            elem.as_ref(),
            IrType::Scalar(DType::I64 | DType::F64 | DType::Bool) | IrType::Str
        ),
        IrType::Struct { .. } => record_layout(ty).is_ok(),
        _ => false,
    }
}

// Refcount kinds (`IRIS_RC_*`) of the GC-tracked state types.
const RC_STR: i32 = 1;
const RC_LIST: i32 = 2;

fn unsupported_state(name: &str, ty: &IrType) -> CodegenError {
    CodegenError::Unsupported {
        backend: "jit".into(),
        detail: format!(
            "state variable '{name}' has type {ty}; state handoff carries i64, f64, bool, str, \
             lists of those, and records of f64/i64/bool fields"
        ),
    }
}

/// Read the value of type `ty` stored at `at`.
///
/// # Safety
/// `at` must be the address of a program state global of type `ty`.
unsafe fn read_state_value(
    at: *mut u8,
    ty: &IrType,
    name: &str,
) -> Result<StateValue, CodegenError> {
    use state_ffi::*;
    Ok(match ty {
        IrType::Scalar(DType::I64) => StateValue::I64(at.cast::<i64>().read()),
        IrType::Scalar(DType::F64) => StateValue::F64(at.cast::<f64>().read()),
        IrType::Scalar(DType::Bool) => StateValue::Bool(at.read() & 1 == 1),
        IrType::Str => {
            let text = at.cast::<*const c_char>().read();
            StateValue::Str(if text.is_null() {
                String::new()
            } else {
                CStr::from_ptr(text).to_string_lossy().into_owned()
            })
        }
        IrType::List(elem) => {
            let list = at.cast::<*mut c_void>().read();
            let mut items = Vec::new();
            if !list.is_null() {
                for index in 0..iris_list_len(list) {
                    let item = iris_list_get(list, index);
                    items.push(match elem.as_ref() {
                        IrType::Scalar(DType::I64) => StateValue::I64(iris_unbox_i64(item)),
                        IrType::Scalar(DType::F64) => StateValue::F64(iris_unbox_f64(item)),
                        IrType::Scalar(DType::Bool) => StateValue::Bool(iris_unbox_bool(item) != 0),
                        IrType::Str => {
                            let text = iris_unbox_str(item);
                            StateValue::Str(if text.is_null() {
                                String::new()
                            } else {
                                CStr::from_ptr(text).to_string_lossy().into_owned()
                            })
                        }
                        _ => return Err(unsupported_state(name, ty)),
                    });
                }
            }
            StateValue::List(items)
        }
        IrType::Struct { .. } => {
            let (layout, _) = record_layout(ty).map_err(|_| unsupported_state(name, ty))?;
            let record = at.cast::<*mut u8>().read();
            if record.is_null() {
                return Err(CodegenError::Unsupported {
                    backend: "jit".into(),
                    detail: format!("state variable '{name}' has no value yet"),
                });
            }
            StateValue::Record(
                layout
                    .iter()
                    .map(|&(offset, dtype)| {
                        let field = record.add(offset);
                        match dtype {
                            DType::F64 => RecordField::F64(field.cast::<f64>().read_unaligned()),
                            DType::I64 => RecordField::I64(field.cast::<i64>().read_unaligned()),
                            _ => RecordField::Bool(field.read() & 1 == 1),
                        }
                    })
                    .collect(),
            )
        }
        _ => return Err(unsupported_state(name, ty)),
    })
}

/// Store `value` (of type `ty`) at `at`, taking a reference on a GC-tracked
/// value and releasing the one it replaces, as `state_set` does.
///
/// # Safety
/// `at` must be the address of a program state global of type `ty`.
unsafe fn write_state_value(
    at: *mut u8,
    ty: &IrType,
    name: &str,
    value: &StateValue,
) -> Result<(), CodegenError> {
    use state_ffi::*;
    let mismatch = || CodegenError::Unsupported {
        backend: "jit".into(),
        detail: format!("state variable '{name}' is {ty}; the value given is {value:?}"),
    };
    let new_str = |text: &str| -> Result<*mut c_void, CodegenError> {
        let text = CString::new(text).map_err(|_| CodegenError::Unsupported {
            backend: "jit".into(),
            detail: format!("state variable '{name}' holds a string with a NUL byte"),
        })?;
        Ok(iris_box_str(text.as_ptr()))
    };
    match (ty, value) {
        (IrType::Scalar(DType::I64), StateValue::I64(v)) => at.cast::<i64>().write(*v),
        (IrType::Scalar(DType::F64), StateValue::F64(v)) => at.cast::<f64>().write(*v),
        (IrType::Scalar(DType::Bool), StateValue::Bool(v)) => at.write(u8::from(*v)),
        (IrType::Str, StateValue::Str(text)) => {
            // The box copies the string and owns one reference; take the
            // state's own, then drop the box.
            let boxed = new_str(text)?;
            let copy = iris_unbox_str(boxed).cast::<c_void>();
            iris_retain_kind(copy, RC_STR);
            iris_drop_box(boxed);
            let old = at.cast::<*mut c_void>().read();
            at.cast::<*mut c_void>().write(copy);
            iris_release_kind(old, RC_STR);
        }
        (IrType::List(elem), StateValue::List(items)) => {
            let typed_list = iris_list_new();
            let list = typed_list.cast::<c_void>();
            for item in items {
                match (elem.as_ref(), item) {
                    (IrType::Scalar(DType::I64), StateValue::I64(v)) => {
                        iris_list_push_i64(list, *v)
                    }
                    (IrType::Scalar(DType::F64), StateValue::F64(v)) => {
                        iris_list_push_f64(list, *v)
                    }
                    (IrType::Scalar(DType::Bool), StateValue::Bool(v)) => iris_list_push(
                        typed_list,
                        iris_box_bool(std::os::raw::c_int::from(*v)).cast(),
                    ),
                    (IrType::Str, StateValue::Str(text)) => {
                        iris_list_push(typed_list, new_str(text)?.cast())
                    }
                    _ => return Err(mismatch()),
                }
            }
            iris_retain_kind(list, RC_LIST);
            let old = at.cast::<*mut c_void>().read();
            at.cast::<*mut c_void>().write(list);
            iris_release_kind(old, RC_LIST);
        }
        (IrType::Struct { .. }, StateValue::Record(fields)) => {
            let (layout, size) = record_layout(ty).map_err(|_| unsupported_state(name, ty))?;
            if layout.len() != fields.len() {
                return Err(mismatch());
            }
            // Records are never freed, so the one this replaces stays valid
            // for anything still holding it.
            let record = iris_alloc_bytes(size as u64).cast::<u8>();
            for (&(offset, dtype), field) in layout.iter().zip(fields) {
                let at_field = record.add(offset);
                match (dtype, field) {
                    (DType::F64, RecordField::F64(v)) => at_field.cast::<f64>().write_unaligned(*v),
                    (DType::I64, RecordField::I64(v)) => at_field.cast::<i64>().write_unaligned(*v),
                    (DType::Bool, RecordField::Bool(v)) => at_field.write(u8::from(*v)),
                    _ => return Err(mismatch()),
                }
            }
            at.cast::<*mut u8>().write(record);
        }
        _ => return Err(mismatch()),
    }
    Ok(())
}

/// Native signatures a [`ResolvedEntry`] can be called through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    I64Nullary,
    I64ToI64,
    /// `(f64 x n) -> f64`, `1 <= n <= 8`.
    F64(usize),
    /// Any other signature; callable only through the name-based methods.
    Other,
}

/// A function address resolved and signature-checked once by
/// [`OrcJitModule::resolve_entry`].
///
/// It holds no reference to its module: the caller must keep the owning
/// [`OrcJitModule`] alive for as long as it calls the entry, which is why
/// the call methods are `unsafe`.
#[derive(Debug, Clone, Copy)]
pub struct ResolvedEntry {
    address: usize,
    kind: EntryKind,
}

impl ResolvedEntry {
    pub fn kind(&self) -> EntryKind {
        self.kind
    }

    fn mismatch(&self, name: &str, requested: &str) -> CodegenError {
        CodegenError::Unsupported {
            backend: "jit".into(),
            detail: format!(
                "function '{name}' has native signature {:?}, which does not match the requested host ABI {requested}",
                self.kind
            ),
        }
    }

    /// # Safety
    /// The module this entry was resolved from must still be alive.
    pub unsafe fn call_i64_0(&self, name: &str) -> Result<i64, CodegenError> {
        if self.kind != EntryKind::I64Nullary {
            return Err(self.mismatch(name, "() -> i64"));
        }
        let function: extern "C" fn() -> i64 = std::mem::transmute(self.address);
        Ok(function())
    }

    /// # Safety
    /// The module this entry was resolved from must still be alive.
    pub unsafe fn call_i64_1(&self, name: &str, arg: i64) -> Result<i64, CodegenError> {
        if self.kind != EntryKind::I64ToI64 {
            return Err(self.mismatch(name, "(i64) -> i64"));
        }
        let function: extern "C" fn(i64) -> i64 = std::mem::transmute(self.address);
        Ok(function(arg))
    }

    /// # Safety
    /// The module this entry was resolved from must still be alive.
    pub unsafe fn call_f64_n(&self, name: &str, args: &[f64]) -> Result<f64, CodegenError> {
        if self.kind != EntryKind::F64(args.len()) {
            return Err(self.mismatch(name, &format!("({} x f64) -> f64", args.len())));
        }
        let address = self.address;
        macro_rules! call {
            ($($arg:ident),+) => {{
                let function: extern "C" fn($($arg: f64),+) -> f64 =
                    std::mem::transmute(address);
                let [$($arg),+] = args else { unreachable!() };
                function($(*$arg),+)
            }};
        }
        Ok(match args.len() {
            1 => call!(a),
            2 => call!(a, b),
            3 => call!(a, b, c),
            4 => call!(a, b, c, d),
            5 => call!(a, b, c, d, e),
            6 => call!(a, b, c, d, e, f),
            7 => call!(a, b, c, d, e, f, g),
            8 => call!(a, b, c, d, e, f, g, h),
            _ => unreachable!("EntryKind::F64 arity is bounded at resolution"),
        })
    }
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
        let entry = self.resolve_entry(name)?;
        // SAFETY: `entry` was resolved from `self`, which is borrowed for the
        // whole call, so its code cannot be removed underneath it.
        unsafe { entry.call_i64_1(name, arg) }
    }

    /// Call the control-law ABI `(f64, ..., f64) -> f64` with 1..=8 inputs.
    ///
    /// Scalar `f64` arguments and results travel in registers under the C ABI
    /// on every supported host, so no marshalling buffer is involved.
    pub fn call_f64_n(&self, name: &str, args: &[f64]) -> Result<f64, CodegenError> {
        let entry = self.resolve_entry(name)?;
        // SAFETY: as in `call_i64_1`.
        unsafe { entry.call_f64_n(name, args) }
    }

    /// Resolve `name` once: check it belongs to this generation, materialize
    /// it, and classify its signature. Hot paths keep the result next to the
    /// module that owns the code and skip the ORC symbol lookup (a C-string
    /// allocation plus a JIT session query) on every call.
    pub fn resolve_entry(&self, name: &str) -> Result<ResolvedEntry, CodegenError> {
        let signature = self
            .signatures
            .get(name)
            .ok_or_else(|| CodegenError::Unsupported {
                backend: "jit".into(),
                detail: format!("function '{}' is not part of this JIT generation", name),
            })?;
        let i64_ty = IrType::Scalar(DType::I64);
        let f64_ty = IrType::Scalar(DType::F64);
        let kind = if signature.result == i64_ty && signature.params.is_empty() {
            EntryKind::I64Nullary
        } else if signature.result == i64_ty && signature.params == [i64_ty.clone()] {
            EntryKind::I64ToI64
        } else if signature.result == f64_ty
            && (1..=MAX_F64_ARGS).contains(&signature.params.len())
            && signature.params.iter().all(|param| *param == f64_ty)
        {
            EntryKind::F64(signature.params.len())
        } else {
            EntryKind::Other
        };
        let address = unsafe { self.lookup_address(name)? };
        Ok(ResolvedEntry { address, kind })
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

    /// The program's state variables, in declaration order.
    pub fn state_vars(&self) -> &[(String, IrType)] {
        &self.state_vars
    }

    /// Give every program state variable its initial value (run
    /// `__iris_state_init`), as a native binary does before `main`.
    pub fn init_state(&self) -> Result<(), CodegenError> {
        if self
            .signatures
            .contains_key(crate::ir::module::STATE_INIT_FN)
        {
            self.call_i64_0(crate::ir::module::STATE_INIT_FN)?;
        }
        Ok(())
    }

    fn state_address(&self, name: &str) -> Result<*mut u8, CodegenError> {
        let symbol = CString::new(format!(
            "{}{name}",
            crate::codegen::llvm_ir::STATE_GLOBAL_PREFIX
        ))
        .map_err(|_| CodegenError::Unsupported {
            backend: "jit".into(),
            detail: "state variable name contains a null byte".into(),
        })?;
        let mut address = 0;
        // SAFETY: the JIT session outlives this module, and `symbol` is a
        // NUL-terminated name.
        let error =
            unsafe { (self.session.api.lookup)(self.session.jit, &mut address, symbol.as_ptr()) };
        self.session.api.check(error, "look up program state")?;
        if address == 0 {
            return Err(CodegenError::Unsupported {
                backend: "jit".into(),
                detail: format!("LLVM ORC returned a null address for state '{name}'"),
            });
        }
        Ok(address as usize as *mut u8)
    }

    /// The current value of every state variable.
    pub fn read_state(&self) -> Result<Vec<(String, StateValue)>, CodegenError> {
        let names: Vec<String> = self
            .state_vars
            .iter()
            .map(|(name, _)| name.clone())
            .collect();
        self.read_state_of(&names)
    }

    /// The current values of the named state variables.
    pub fn read_state_of(
        &self,
        names: &[String],
    ) -> Result<Vec<(String, StateValue)>, CodegenError> {
        self.state_vars
            .iter()
            .filter(|(name, _)| names.contains(name))
            .map(|(name, ty)| {
                let at = self.state_address(name)?;
                // SAFETY: `at` is this generation's global for `name`, of `ty`.
                let value = unsafe { read_state_value(at, ty, name)? };
                Ok((name.clone(), value))
            })
            .collect()
    }

    /// Set state variable `name` to `value`.
    pub fn write_state(&self, name: &str, value: &StateValue) -> Result<(), CodegenError> {
        let ty = self
            .state_vars
            .iter()
            .find(|(var, _)| var == name)
            .map(|(_, ty)| ty)
            .ok_or_else(|| CodegenError::Unsupported {
                backend: "jit".into(),
                detail: format!("the program has no state variable '{name}'"),
            })?;
        let at = self.state_address(name)?;
        // SAFETY: as in `read_state`.
        unsafe { write_state_value(at, ty, name, value) }
    }

    /// Call the record ABI `(record) -> record`, where both records have only
    /// `f64`, `i64` and `bool` fields. The input is laid out exactly as the
    /// native `%Name` type in a host buffer (the callee only reads it); the
    /// returned record, which the callee allocated, is copied out and freed
    /// unless it is the input itself.
    pub fn call_record(
        &self,
        name: &str,
        input: &[RecordField],
    ) -> Result<Vec<RecordField>, CodegenError> {
        let signature = self
            .signatures
            .get(name)
            .ok_or_else(|| CodegenError::Unsupported {
                backend: "jit".into(),
                detail: format!("function '{}' is not part of this JIT generation", name),
            })?;
        let [param] = signature.params.as_slice() else {
            return Err(CodegenError::Unsupported {
                backend: "jit".into(),
                detail: format!("function '{name}' does not take exactly one record"),
            });
        };
        let (input_layout, input_size) = record_layout(param)?;
        let (output_layout, _) = record_layout(&signature.result)?;
        if input.len() != input_layout.len() {
            return Err(CodegenError::Unsupported {
                backend: "jit".into(),
                detail: format!(
                    "function '{name}' takes a record of {} fields, given {}",
                    input_layout.len(),
                    input.len()
                ),
            });
        }
        // u64 storage keeps every field naturally aligned.
        let mut buffer = vec![0u64; input_size.div_ceil(8)];
        let base = buffer.as_mut_ptr().cast::<u8>();
        for (index, (&(offset, dtype), value)) in input_layout.iter().zip(input).enumerate() {
            // SAFETY: `record_layout` keeps every field inside `input_size`.
            unsafe {
                let at = base.add(offset);
                match (dtype, value) {
                    (DType::F64, RecordField::F64(v)) => at.cast::<f64>().write(*v),
                    (DType::I64, RecordField::I64(v)) => at.cast::<i64>().write(*v),
                    (DType::Bool, RecordField::Bool(v)) => at.write(u8::from(*v)),
                    _ => {
                        return Err(CodegenError::Unsupported {
                            backend: "jit".into(),
                            detail: format!(
                                "input field {index} of '{name}' is {dtype:?}, given {value:?}"
                            ),
                        })
                    }
                }
            }
        }
        let address = unsafe { self.lookup_address(name)? };
        let function: unsafe extern "C" fn(*mut u8) -> *mut u8 =
            unsafe { std::mem::transmute(address) };
        let result = unsafe { function(base) };
        if result.is_null() {
            return Err(CodegenError::Unsupported {
                backend: "jit".into(),
                detail: format!("'{name}' returned a null record"),
            });
        }
        let fields = output_layout
            .iter()
            .map(|&(offset, dtype)| unsafe {
                // SAFETY: the callee returned a `%Output` record, whose layout
                // `record_layout` reproduces.
                let at = result.add(offset);
                match dtype {
                    DType::F64 => RecordField::F64(at.cast::<f64>().read_unaligned()),
                    DType::I64 => RecordField::I64(at.cast::<i64>().read_unaligned()),
                    _ => RecordField::Bool(at.read() & 1 == 1),
                }
            })
            .collect();
        if result != base {
            // Records are allocated with `iris_alloc_bytes` and owned by
            // whoever holds them; the caller of a policy is the only holder.
            unsafe { iris_free_bytes(result.cast::<c_void>()) };
        }
        drop(buffer);
        Ok(fields)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orc_loader_tracks_recommended_llvm_23_candidates() {
        #[cfg(target_os = "windows")]
        let default_name = "LLVM-C.dll";
        #[cfg(target_os = "macos")]
        let default_name = "libLLVM.dylib";
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        let default_name = "libLLVM.so";

        let candidates = llvm_library_candidates(default_name);
        assert!(!candidates.is_empty());
        let rendered = candidates
            .iter()
            .map(|path| path.to_string_lossy())
            .collect::<Vec<_>>();
        let llvm_23 = rendered.iter().position(|path| path.contains("23"));
        assert!(llvm_23.is_some(), "candidates should include LLVM 23 paths");
    }
}
