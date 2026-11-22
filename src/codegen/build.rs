//! Native binary build pipeline for IRIS.
//!
//! Phase 54 — takes an `IrModule`, emits LLVM IR text, writes the embedded C
//! runtime to a temp dir, and invokes `clang` + `lld` to produce a native
//! executable.  **No GCC installation is required** — only LLVM/clang (with
//! the bundled `ld.lld`) and MinGW sysroot headers + libraries.
//!
//! Build steps
//! -----------
//! 1. Emit LLVM IR from the module via `emit_llvm_ir`.
//! 2. Write `module.ll` to `$TMPDIR/iris_build_<PID>/`.
//! 3. Write the embedded `iris_runtime.h` + `iris_runtime.c` to the same dir.
//! 4. `clang -target x86_64-w64-windows-gnu -O2 -c iris_runtime.c -o iris_runtime.o`
//! 5. `clang -target x86_64-w64-windows-gnu -O2 -c module.ll -o module.o`
//! 6. `clang -target x86_64-w64-windows-gnu -fuse-ld=lld module.o iris_runtime.o -o <output> -lm -lpthread`
//! 7. Return the path to the output binary.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::CodegenError;
use crate::ir::module::IrModule;

/// Controls how native builds link ML runtime backends (ONNX/TF/BLAS).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MlLinkMode {
    /// Portable default: resolve ML backends dynamically at runtime via `dlopen`.
    #[default]
    DynamicDlopen,
    /// Opt-in compile-time direct linking (`IRIS_ML_STATIC_LINK=1`) for LTO/static builds.
    StaticDirect,
}

impl MlLinkMode {
    /// Reads `IRIS_ML_STATIC_LINK` from the environment.
    pub fn from_env() -> Self {
        Self::from_flag(std::env::var("IRIS_ML_STATIC_LINK").ok().as_deref())
    }

    /// Parses an explicit flag value (`"1"` / `"true"` -> [`Self::StaticDirect`]).
    pub fn from_flag(val: Option<&str>) -> Self {
        match val {
            Some(v) if v == "1" || v.eq_ignore_ascii_case("true") => Self::StaticDirect,
            _ => Self::DynamicDlopen,
        }
    }
}

/// Helper: convert a Path to &str, returning a descriptive error on non-UTF8 paths.
fn path_str(p: &Path) -> Result<&str, CodegenError> {
    p.to_str().ok_or_else(|| CodegenError::Unsupported {
        backend: "binary".into(),
        detail: format!("path contains non-UTF8 characters: {}", p.display()),
    })
}

// ---------------------------------------------------------------------------
// Embedded runtime sources (compiled into the IRIS Rust binary itself)
// ---------------------------------------------------------------------------

/// The C runtime header, embedded at compile time.
/// (updated: added time/OS, struct/tuple/closure fallback helpers)
pub const RUNTIME_H_SRC: &str = include_str!("../runtime/iris_runtime.h");

/// The C runtime implementation, embedded at compile time.
/// (updated: added iris_now_ms, iris_sleep_ms, iris_make_struct, iris_get_field,
///  iris_make_tuple, iris_get_element, iris_make_closure, etc.)
pub const RUNTIME_C_SRC: &str = include_str!("../runtime/iris_runtime.c");

/// Native ML backend shims used by generated IRIS binaries.
pub const ONNX_SHIM_H_SRC: &str = include_str!("../runtime/onnx_shim.h");
pub const ONNX_SHIM_C_SRC: &str = include_str!("../runtime/onnx_shim.c");
pub const TF_SHIM_C_SRC: &str = include_str!("../runtime/tf_shim.c");
pub const PYTORCH_SHIM_CPP_SRC: &str = include_str!("../runtime/pytorch_shim.cpp");

/// Lazy runtime loader for the ML backends. Lets the shims compile with no ML
/// SDK installed — the shared libraries are resolved via dlopen on first use.
pub const ML_DYNLOAD_H_SRC: &str = include_str!("../runtime/iris_ml_dynload.h");

// Prebuilt runtime objects embedded by the top-level build script. Defines
// `PrebuiltRuntime`, `RUNTIME_SOURCES_HASH` and `PREBUILT_RUNTIMES`. When a set
// is present for the target triple we skip the C compiler entirely.
include!(concat!(env!("OUT_DIR"), "/prebuilt_runtime.rs"));

/// Look up prebuilt runtime objects for `triple`, if this compiler embeds any.
///
/// The build script only emits entries whose recorded source hash matches the
/// current C sources, so anything returned here is known to be up to date.
fn find_prebuilt_runtime(triple: &str) -> Option<&'static PrebuiltRuntime> {
    PREBUILT_RUNTIMES.iter().find(|p| p.triple == triple)
}

/// Vendored ONNX Runtime C API header (MIT, Microsoft). Supplies the ABI layout
/// only; it creates no link-time dependency. See src/runtime/vendor/README.md.
pub const ONNXRUNTIME_C_API_H_SRC: &str = include_str!("../runtime/vendor/onnxruntime_c_api.h");

/// ML compute kernels header — convolution, pooling, losses, optimizers, etc.
pub const ML_KERNELS_H_SRC: &str = include_str!("../runtime/iris_ml_kernels.h");
/// ML compute kernels implementation.
pub const ML_KERNELS_C_SRC: &str = include_str!("../runtime/iris_ml_kernels.c");

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Compile an `IrModule` to a native executable.
///
/// `output_path` is the desired path for the final binary (e.g. `"./a.out"`).
///
/// Returns the `PathBuf` of the output binary on success, or a `CodegenError`
/// if no compiler can be found or any compilation/link step fails.
/// Additional platform linking options for standalone executables and GUI apps.
#[derive(Debug, Clone, Default)]
pub struct NativeBuildOptions {
    /// On Windows, set PE Subsystem to Windows GUI (`2`) instead of Console (`3`).
    pub gui_subsystem: bool,
    /// Extra native object or COFF resource files (`.o` / `.res`) linked into the executable.
    pub extra_objects: Vec<PathBuf>,
    /// Shared-library runtime search paths (`-rpath` on Linux/macOS).
    pub rpaths: Vec<String>,
}

/// Requires at least one zero-argument function (preferably named `main`) as the entry point.
pub fn build_binary(module: &IrModule, output_path: &Path) -> Result<PathBuf, CodegenError> {
    build_binary_with_target(module, output_path, None)
}

/// Like `build_binary` but overrides the LLVM/clang target triple.
pub fn build_binary_with_target(
    module: &IrModule,
    output_path: &Path,
    target: Option<&str>,
) -> Result<PathBuf, CodegenError> {
    build_binary_with_security(
        module,
        output_path,
        target,
        &crate::security::get_security_policy(),
    )
}

/// Like `build_binary_with_target` but validates and embeds an explicit `SecurityPolicy`
/// into the compiled binary's `@main` prologue.
pub fn build_binary_with_security(
    module: &IrModule,
    output_path: &Path,
    target: Option<&str>,
    policy: &crate::security::SecurityPolicy,
) -> Result<PathBuf, CodegenError> {
    build_binary_with_security_and_options(
        module,
        output_path,
        target,
        policy,
        &NativeBuildOptions::default(),
    )
}

/// Build a native binary using extra subsystem, resource object, and rpath options.
pub fn build_binary_with_options(
    module: &IrModule,
    output_path: &Path,
    target: Option<&str>,
    opts: &NativeBuildOptions,
) -> Result<PathBuf, CodegenError> {
    build_binary_with_security_and_options(
        module,
        output_path,
        target,
        &crate::security::get_security_policy(),
        opts,
    )
}

/// Like `build_binary_with_security` with explicit [`NativeBuildOptions`].
pub fn build_binary_with_security_and_options(
    module: &IrModule,
    output_path: &Path,
    target: Option<&str>,
    policy: &crate::security::SecurityPolicy,
    opts: &NativeBuildOptions,
) -> Result<PathBuf, CodegenError> {
    let link_libs: Vec<String> = module
        .extern_fns
        .iter()
        .filter_map(|e| e.link_lib.clone())
        .collect();
    let llvm_ir =
        crate::codegen::llvm_ir::emit_llvm_ir_for_binary_with_security(module, target, policy)?;
    build_binary_impl_with_options(llvm_ir, output_path, target, link_libs, false, false, opts)
}

/// Build a native executable whose `main` performs `calls` and prints one
/// exact `i64` line per call (see `llvm_ir::emit_llvm_ir_for_harness`). Used
/// to validate evolution candidates natively inside the worker sandbox.
pub fn build_harness_binary(
    module: &IrModule,
    calls: &[crate::codegen::llvm_ir::HarnessCall],
    output_path: &Path,
) -> Result<PathBuf, CodegenError> {
    let link_libs: Vec<String> = module
        .extern_fns
        .iter()
        .filter_map(|e| e.link_lib.clone())
        .collect();
    // Self-contained: the harness runs inside the worker sandbox, which cannot
    // load optional DLLs from elsewhere (a machine-wide OpenBLAS made every
    // sandboxed run fail with STATUS_DLL_NOT_FOUND), and scalar policies
    // never need them.
    build_binary_impl_with(
        crate::codegen::llvm_ir::emit_llvm_ir_for_harness(module, calls)?,
        output_path,
        None,
        link_libs,
        false,
        true,
    )
}

/// Like `build_binary` but uses the eval wrapper: the entry function's return
/// value is printed to stdout instead of being used as the process exit code.
/// Used by `EmitKind::Eval` so that tests get the same output as the interpreter.
pub fn build_binary_for_eval(
    module: &IrModule,
    output_path: &Path,
) -> Result<PathBuf, CodegenError> {
    build_binary_for_eval_with_target(module, output_path, None)
}

/// Like `build_binary_for_eval` but overrides the LLVM/clang target triple.
pub fn build_binary_for_eval_with_target(
    module: &IrModule,
    output_path: &Path,
    target: Option<&str>,
) -> Result<PathBuf, CodegenError> {
    use crate::codegen::llvm_ir::emit_llvm_ir_for_eval;
    let link_libs: Vec<String> = module
        .extern_fns
        .iter()
        .filter_map(|e| e.link_lib.clone())
        .collect();
    if target.is_some() {
        build_binary_impl(
            crate::codegen::llvm_ir::emit_llvm_ir_for_eval_with_target(module, target)?,
            output_path,
            target,
            link_libs,
        )
    } else {
        build_binary_impl(emit_llvm_ir_for_eval(module)?, output_path, None, link_libs)
    }
}

/// Build and execute a temporary native binary using the eval wrapper.
///
/// The entry function's return value is printed to stdout, matching the
/// observable behavior of `EmitKind::Eval`.
pub fn execute_binary_for_eval(module: &IrModule) -> Result<String, CodegenError> {
    execute_binary_for_eval_with_target(module, None)
}

/// Like `execute_binary_for_eval` but overrides the LLVM/clang target triple.
pub fn execute_binary_for_eval_with_target(
    module: &IrModule,
    target: Option<&str>,
) -> Result<String, CodegenError> {
    let output = run_binary_for_eval_entry_capture(module, None, target)?;
    // Undo the Windows CRT's text-mode translation so eval output matches the
    // interpreter: a program's `\n` reaches us as `\r\n`, and genuine `\r\n`
    // data reaches us as `\r\r\n`, which this collapses back correctly.
    //
    // Do NOT additionally map lone `\r` to `\n`. That destroyed real carriage
    // returns in program data — `http_get_request` emits a correct
    // `GET / HTTP/1.1\r\nHost: …` and eval reported `\n\n`, which looked like a
    // codegen bug for as long as it went unmeasured. The compiled binary was
    // right all along; only this capture was wrong.
    let stdout = String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n");
    if output.status.code() == Some(101) {
        let stderr = String::from_utf8_lossy(&output.stderr).replace("\r\n", "\n");
        if let Some((before, report)) = split_runtime_report(&stderr) {
            return Err(report.into_error(module.source.as_deref(), stdout, before.to_string()));
        }
    }
    if output.status.success() {
        // What the program wrote to stderr, as the interpreter would have.
        use std::io::Write;
        let mut err = std::io::stderr();
        let _ = err.write_all(&output.stderr);
        let _ = err.flush();
    }
    if !output.status.success() {
        let stderr = strip_ansi(&String::from_utf8_lossy(&output.stderr));
        // A runtime panic already says what failed; the exit code adds nothing.
        let detail = match stderr.trim() {
            "" => describe_exit(&output.status),
            text if output.status.code() == Some(101) => {
                text.trim_start_matches("panic: ").to_string()
            }
            text => format!("{text} ({})", describe_exit(&output.status)),
        };
        return Err(CodegenError::NativeRun { detail, stdout });
    }
    Ok(stdout)
}

/// A runtime error as the native runtime reports it (`iris_format_report`
/// in iris_runtime.c):
///
/// ```text
/// error[E0403]: index 5 is out of bounds for length 3
///  --> main.iris:4:5
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeReport {
    pub code: &'static str,
    pub message: String,
    /// File as messages show it, and 1-based line and column, when the
    /// runtime knew them.
    pub position: Option<(String, u32, u32)>,
}

impl RuntimeReport {
    /// As an error that renders with the source line, given the main
    /// file's lines.
    pub fn into_error(
        self,
        source: Option<&crate::ir::module::SourceLines>,
        stdout: String,
        stderr: String,
    ) -> CodegenError {
        let span = self
            .position
            .and_then(|(file, line, col)| source?.offset(&file, line, col))
            .map(|byte| (byte, byte + 1));
        CodegenError::Runtime {
            code: self.code,
            message: self.message,
            span,
            stdout,
            stderr,
        }
    }
}

/// The report at the end of a failed program's output, and the output
/// before it (what the program itself printed).
pub fn split_runtime_report(text: &str) -> Option<(&str, RuntimeReport)> {
    let start = text
        .match_indices("error[E04")
        .map(|(i, _)| i)
        .filter(|&i| i == 0 || text[..i].ends_with('\n'))
        .last()?;
    let mut lines = text[start..].lines();
    let (code, message) = lines.next()?.strip_prefix("error[")?.split_once("]: ")?;
    let parse_location = |loc: &str| -> Option<(String, u32, u32)> {
        let mut parts = loc.trim_end().rsplitn(3, ':');
        let col = parts.next()?.parse().ok()?;
        let line = parts.next()?.parse().ok()?;
        Some((parts.next()?.to_string(), line, col))
    };
    // An assert's message carries its own position (`... at file:4:5`).
    let position = lines
        .next()
        .and_then(|l| l.trim_start().strip_prefix("--> "))
        .and_then(parse_location)
        .or_else(|| {
            message
                .rsplit_once(" at ")
                .and_then(|(_, loc)| parse_location(loc))
        });
    let report = RuntimeReport {
        code: crate::error::runtime_code(code),
        message: message.trim_end().to_string(),
        position,
    };
    Some((&text[..start], report))
}

pub(crate) fn run_binary_for_eval_entry_capture(
    module: &IrModule,
    entry_name: Option<&str>,
    target: Option<&str>,
) -> Result<std::process::Output, CodegenError> {
    let link_libs: Vec<String> = module
        .extern_fns
        .iter()
        .filter_map(|e| e.link_lib.clone())
        .collect();
    let bin_path = if let Some(name) = entry_name {
        build_binary_impl(
            crate::codegen::llvm_ir::emit_llvm_ir_for_named_eval_with_target(
                module,
                Some(name),
                target,
            )?,
            &temp_eval_binary_path(),
            target,
            link_libs,
        )?
    } else {
        build_binary_for_eval_with_target(module, &temp_eval_binary_path(), target)?
    };
    let run_path = std::fs::canonicalize(&bin_path).unwrap_or(bin_path.clone());
    // Removed on every exit path, including the timeout below.
    let _exe_guard = TempExeGuard(run_path.clone());
    // Run the native binary with a timeout. Programs that use spawn/TCP may hang
    // indefinitely in the native runtime, so we fall back to the interpreter.
    //
    // The limit is configurable because it is wall-clock, not work: on a slow or
    // loaded machine a correct program can exceed a fixed budget and be reported
    // as a failure, which is a false negative rather than a real defect.
    let timeout_secs = native_timeout_secs();
    let output = run_with_timeout(&run_path, std::time::Duration::from_secs(timeout_secs))
        .map_err(|_| CodegenError::NativeRun {
            detail: format!(
                "no result after {}s; raise IRIS_NATIVE_TIMEOUT if it needs longer",
                timeout_secs
            ),
            stdout: String::new(),
        })?;
    Ok(output)
}

/// Write `contents` to `path` unless the build does not need it.
fn write_source(enabled: bool, path: &Path, contents: &str) -> std::io::Result<()> {
    if enabled {
        std::fs::write(path, contents)
    } else {
        Ok(())
    }
}

/// The cache directory holding `pre`'s objects, written there once. The name
/// carries the target and the hash of the runtime sources the objects were
/// built from, so a different compiler version never reuses them. `None` when
/// no cache directory can be written; the caller stages the objects itself.
fn prebuilt_object_dir(pre: &'static PrebuiltRuntime) -> Option<PathBuf> {
    let triple: String = pre
        .triple
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let dir = runtime_cache_dir().join(format!("prebuilt-{triple}-{RUNTIME_SOURCES_HASH}"));
    std::fs::create_dir_all(&dir).ok()?;
    for (name, bytes) in pre.objects {
        let path = dir.join(name);
        let current = std::fs::metadata(&path)
            .map(|m| m.len() == bytes.len() as u64)
            .unwrap_or(false);
        if current {
            continue;
        }
        // Written beside, then renamed, so a concurrent build never links a
        // half-written object.
        // Unique per thread as well as per process: two threads of one
        // process wrote the same pending file.
        let pending = dir.join(
            format!(
                "{name}.{}.{:?}.tmp",
                std::process::id(),
                std::thread::current().id()
            )
            .replace(['(', ')'], ""),
        );
        std::fs::write(&pending, bytes).ok()?;
        if std::fs::rename(&pending, &path).is_err() {
            let _ = std::fs::remove_file(&pending);
            let done = std::fs::metadata(&path)
                .map(|m| m.len() == bytes.len() as u64)
                .unwrap_or(false);
            if !done {
                return None;
            }
        }
    }
    Some(dir)
}

/// Deduplicated candidate IRIS installation roots in lookup priority order.
///
/// Supports both flat layouts (`<prefix>/iris.exe`, portable zip, Inno Setup)
/// and FHS/prefix layouts (`<prefix>/bin/iris`, `~/.iris/bin/iris`, Homebrew, Linux packages).
pub(crate) fn install_prefix_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut push_unique = |p: PathBuf| {
        if !p.as_os_str().is_empty() && !dirs.contains(&p) {
            dirs.push(p);
        }
    };

    if let Some(home) = std::env::var_os("IRIS_HOME").filter(|v| !v.is_empty()) {
        push_unique(PathBuf::from(home));
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            push_unique(dir.to_path_buf());
            if let Some(parent) = dir.parent() {
                push_unique(parent.to_path_buf());
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(lad) = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()) {
            push_unique(PathBuf::from(lad).join("Programs").join("IRIS"));
        }
        if let Some(home) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
            push_unique(PathBuf::from(home).join(".iris"));
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
            push_unique(PathBuf::from(home).join(".iris"));
        }
        #[cfg(target_os = "macos")]
        push_unique(PathBuf::from("/usr/local/share/iris"));
        #[cfg(target_os = "linux")]
        push_unique(PathBuf::from("/usr/share/iris"));
    }

    dirs
}

/// What `iris doctor` reports about the native toolchain.
pub struct ToolchainReport {
    /// A prebuilt runtime exists for the host target, so ordinary builds
    /// need no C compiler.
    pub prebuilt_runtime: bool,
    /// The clang IRIS would run (a bare `clang` when none was found).
    pub clang: String,
    pub lld: Option<String>,
    pub sysroot_include: Option<String>,
    pub sysroot_lib: Option<String>,
    pub sysroot_gcc: Option<String>,
    pub ui_host: Option<PathBuf>,
}

pub fn toolchain_report() -> ToolchainReport {
    ToolchainReport {
        prebuilt_runtime: find_prebuilt_runtime(crate::codegen::native_target_triple()).is_some(),
        clang: find_clang(),
        lld: find_lld(),
        sysroot_include: msys2_ucrt64_include(),
        sysroot_lib: msys2_ucrt64_lib(),
        sysroot_gcc: msys2_gcc_lib(),
        ui_host: crate::codegen::app_bundle::find_installed_ui_host(),
    }
}

/// `IRIS_VERBOSE=1`: report how a native build was done (prebuilt runtime,
/// which linker). Off by default, so `iris run` prints only the program.
pub fn verbose() -> bool {
    std::env::var_os("IRIS_VERBOSE").is_some_and(|v| !v.is_empty() && v != "0")
}

/// `text` without ANSI colour sequences, for embedding in a diagnostic.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// An exit status in words: the code, and on Windows what a crash code means.
pub fn describe_exit(status: &std::process::ExitStatus) -> String {
    match status.code() {
        Some(code) => {
            let unsigned = code as u32;
            let meaning = match unsigned {
                0xC0000005 => " -- access violation",
                0xC0000374 => " -- heap corruption",
                0xC0000409 => " -- aborted (failed assert, panic or fast-fail)",
                0xC00000FD => " -- stack overflow",
                0xC0000135 => " -- a DLL it needs was not found",
                _ => "",
            };
            if unsigned >= 0xC0000000 {
                format!("exit 0x{unsigned:X}{meaning}")
            } else {
                format!("exit {code}")
            }
        }
        None => "terminated by a signal".into(),
    }
}

/// Wall-clock budget for a natively built program run under `--emit eval`.
///
/// Override with `IRIS_NATIVE_TIMEOUT` (seconds). Values that do not parse, or a
/// zero, fall back to the default rather than disabling the timeout — an
/// unbounded wait would hang the whole test suite on a single spawning program.
pub(crate) fn native_timeout_secs() -> u64 {
    const DEFAULT_NATIVE_TIMEOUT_SECS: u64 = 15;
    std::env::var("IRIS_NATIVE_TIMEOUT")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_NATIVE_TIMEOUT_SECS)
}

/// Whether to skip native compilation entirely and interpret instead.
///
/// Set `IRIS_FORCE_INTERP=1` to enable. `--emit eval` otherwise compiles and
/// links a native binary per program, which dominates test-suite runtime: the
/// suite pays a clang invocation per test even though the interpreter would
/// answer in milliseconds. This is the switch that makes the suite fast enough
/// to iterate on, and it also removes native-timeout flakiness from CI.
pub fn force_interpreter() -> bool {
    matches!(
        std::env::var("IRIS_FORCE_INTERP").ok().as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

/// Run a child process and wait for it to finish, with a timeout.
fn run_with_timeout(
    bin: &std::path::Path,
    timeout: std::time::Duration,
) -> Result<std::process::Output, ()> {
    let program_args = crate::program_args().unwrap_or_default();
    let mut cmd = std::process::Command::new(bin);
    cmd.args(program_args.iter().skip(1))
        // The report goes to stderr, where this reads it back.
        .env_remove("IRIS_ERROR_FILE")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let policy = crate::security::get_security_policy();
    if !policy.is_default_permissive() {
        cmd.env("IRIS_SANDBOX", "1")
            .env("IRIS_SECURITY_POLICY", policy.to_runtime_spec());
    }
    let mut child = cmd.spawn().map_err(|_| ())?;
    // Drain both pipes while waiting: a program that prints more than a pipe
    // holds would otherwise block on write until the timeout, and `--emit eval`
    // would then fall back to the interpreter.
    let drain = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    );
    let stderr = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    );
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Ok(std::process::Output {
                    status,
                    stdout: stdout.join().unwrap_or_default(),
                    stderr: stderr.join().unwrap_or_default(),
                });
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(());
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(());
            }
        }
    }
}

pub(crate) fn run_native_test_capture(
    module: &IrModule,
    entry_name: &str,
    target: Option<&str>,
) -> Result<std::process::Output, CodegenError> {
    let link_libs: Vec<String> = module
        .extern_fns
        .iter()
        .filter_map(|e| e.link_lib.clone())
        .collect();
    let bin_path = build_binary_impl(
        crate::codegen::llvm_ir::emit_llvm_ir_for_test_entry_with_target(
            module, entry_name, target,
        )?,
        &temp_eval_binary_path(),
        target,
        link_libs,
    )?;
    let run_path = std::fs::canonicalize(&bin_path).unwrap_or(bin_path.clone());
    let _exe_guard = TempExeGuard(run_path.clone());
    let timeout_secs = native_timeout_secs();
    let output = run_with_timeout(&run_path, std::time::Duration::from_secs(timeout_secs)).map_err(
        |_| CodegenError::Unsupported {
            backend: "native-test".into(),
            detail: format!(
                "native test timed out after {timeout_secs}s; raise IRIS_NATIVE_TIMEOUT to allow a longer test"
            ),
        },
    )?;
    Ok(output)
}

fn temp_eval_binary_path() -> PathBuf {
    let pid = std::process::id();
    let tid = format!("{:?}", std::thread::current().id())
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    std::env::temp_dir().join(format!(
        "iris_eval_{}_{}_{}{}",
        pid,
        tid,
        nanos,
        std::env::consts::EXE_SUFFIX
    ))
}

fn _build_binary_from_llvm_ir(
    llvm_ir: String,
    output_path: &Path,
    target: Option<&str>,
) -> Result<PathBuf, CodegenError> {
    build_binary_impl(llvm_ir, output_path, target, vec![])
}

fn build_binary_impl(
    llvm_ir: String,
    output_path: &Path,
    target: Option<&str>,
    link_libs: Vec<String>,
) -> Result<PathBuf, CodegenError> {
    build_binary_impl_with(llvm_ir, output_path, target, link_libs, false, false)
}

/// Build a native executable carrying DWARF debug information, compiled at
/// `-O0` so stepping follows the source. `module` and `source_map` come from
/// `compile_file_to_module_debug` (or, for one in-memory source,
/// `compile_to_module_debug` with `SourceMap::single`).
pub fn build_binary_debug(
    module: &IrModule,
    source_map: &crate::compiler::SourceMap,
    output_path: &Path,
) -> Result<PathBuf, CodegenError> {
    let link_libs: Vec<String> = module
        .extern_fns
        .iter()
        .filter_map(|e| e.link_lib.clone())
        .collect();
    let built = build_binary_impl_with(
        crate::codegen::llvm_ir::emit_llvm_ir_for_binary_debug(module, source_map)?,
        output_path,
        None,
        link_libs,
        true,
        false,
    )?;
    // Apple's linker leaves DWARF in the object files, which the build
    // deletes; `dsymutil` collects it into a `.dSYM` bundle beside the binary
    // so lldb and gdb can still find it.
    if cfg!(target_os = "macos") {
        match Command::new("dsymutil").arg(&built).status() {
            Ok(status) if status.success() => {}
            Ok(status) => eprintln!(
                "iris_codegen: dsymutil exited with {status}; the binary has no line tables"
            ),
            Err(error) => eprintln!(
                "iris_codegen: dsymutil unavailable ({error}); the binary has no line tables"
            ),
        }
    }
    Ok(built)
}

/// `self_contained` leaves out optional libraries found on the machine (such
/// as OpenBLAS), so the binary depends on nothing beyond the system and its
/// own declared `link` libraries.
fn build_binary_impl_with(
    llvm_ir: String,
    output_path: &Path,
    target: Option<&str>,
    link_libs: Vec<String>,
    debug: bool,
    self_contained: bool,
) -> Result<PathBuf, CodegenError> {
    build_binary_impl_with_options(
        llvm_ir,
        output_path,
        target,
        link_libs,
        debug,
        self_contained,
        &NativeBuildOptions::default(),
    )
}

fn build_binary_impl_with_options(
    llvm_ir: String,
    output_path: &Path,
    target: Option<&str>,
    link_libs: Vec<String>,
    debug: bool,
    self_contained: bool,
    opts: &NativeBuildOptions,
) -> Result<PathBuf, CodegenError> {
    let output_path_buf = if output_path.is_relative() {
        std::env::current_dir()
            .map(|cwd| cwd.join(output_path))
            .unwrap_or_else(|_| output_path.to_path_buf())
    } else {
        output_path.to_path_buf()
    };
    let output_path = output_path_buf.as_path();
    let resolved_target = resolve_target_triple(target);

    // WASM compilation path — uses WASI sysroot + wasi-libc + compiler-rt.
    if resolved_target.contains("wasm32") {
        return build_wasm_binary_impl(&llvm_ir, output_path, &resolved_target);
    }

    if !llvm_ir.contains("define i32 @main(") {
        return Err(CodegenError::Unsupported {
            backend: "binary".into(),
            detail: "no entry point (define main() or a zero-argument function) for native binary"
                .into(),
        });
    }

    // 1. LLVM IR already emitted.

    // 2. Set up a per-call temp directory so parallel builds don't collide.
    // Derive from output_path's stem and add timestamp + pid to avoid collisions.
    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let stem = output_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("iris_build");
    let build_id = format!("{}_{}_{}_bld", stem, std::process::id(), now_nanos);
    let tmp_dir = std::env::temp_dir().join(build_id);
    std::fs::create_dir_all(&tmp_dir).map_err(|e| CodegenError::Unsupported {
        backend: "binary".into(),
        detail: format!("failed to create temp dir '{}': {}", tmp_dir.display(), e),
    })?;
    // Removes the directory on every exit path, success or error.
    let _build_dir_guard = BuildDirGuard(tmp_dir.clone());

    // `IRIS_SANITIZE=address` (or any `-fsanitize=` value) builds the
    // runtime from source and the program with that sanitizer, for Linux and
    // macOS targets. CI runs the differential tester and the corpus this way.
    let sanitize = sanitizer_from_env();
    if let Some(ref s) = sanitize {
        if resolved_target.contains("windows") {
            return Err(CodegenError::Unsupported {
                backend: "binary".into(),
                detail: format!(
                    "IRIS_SANITIZE={s} is supported for Linux and macOS targets, not {resolved_target}"
                ),
            });
        }
    }
    let llvm_ir = match sanitize {
        Some(ref s) if s.split(',').any(|k| k == "address") => mark_sanitize_address(&llvm_ir),
        _ => llvm_ir,
    };

    // 3. Write LLVM IR.
    let ll_path = tmp_dir.join("module.ll");
    std::fs::write(&ll_path, &llvm_ir).map_err(|e| CodegenError::Unsupported {
        backend: "binary".into(),
        detail: format!("failed to write LLVM IR to '{}': {}", ll_path.display(), e),
    })?;

    // Preferred path: this compiler embeds runtime objects prebuilt for the
    // target, so no C compiler is needed at all. Anything present here was
    // verified by the build script to come from the current C sources.
    let prebuilt = if sanitize.is_some() {
        None
    } else {
        find_prebuilt_runtime(&resolved_target)
    };
    // The prebuilt objects are kept in a cache keyed by the runtime sources
    // and written once, and the C sources are written only when the runtime
    // is compiled here. Staging both into every build's directory -- about
    // 1.4 MB in 14 files, each scanned on Windows -- was a third of the time of
    // building a small program.
    let prebuilt_dir = prebuilt.and_then(prebuilt_object_dir);
    let write_sources = prebuilt_dir.is_none();

    // 4. Write embedded runtime sources (only when compiling the runtime).
    let h_path = tmp_dir.join("iris_runtime.h");
    let c_path = tmp_dir.join("iris_runtime.c");
    let onnx_h_path = tmp_dir.join("onnx_shim.h");
    let onnx_c_path = tmp_dir.join("onnx_shim.c");
    let tf_c_path = tmp_dir.join("tf_shim.c");
    let pytorch_cpp_path = tmp_dir.join("pytorch_shim.cpp");
    write_source(write_sources, &h_path, RUNTIME_H_SRC).map_err(|e| CodegenError::Unsupported {
        backend: "binary".into(),
        detail: format!("failed to write runtime header: {}", e),
    })?;
    write_source(write_sources, &c_path, RUNTIME_C_SRC).map_err(|e| CodegenError::Unsupported {
        backend: "binary".into(),
        detail: format!("failed to write runtime C source: {}", e),
    })?;
    // The ML dynamic loader and the vendored ONNX C API header must sit next to
    // the shims: they are included by onnx_shim.c / tf_shim.c and resolved via
    // the `-I <tmp_dir>` already passed to every compile step below.
    let ml_dynload_h_path = tmp_dir.join("iris_ml_dynload.h");
    let ort_api_h_path = tmp_dir.join("onnxruntime_c_api.h");
    write_source(write_sources, &ml_dynload_h_path, ML_DYNLOAD_H_SRC).map_err(|e| {
        CodegenError::Unsupported {
            backend: "binary".into(),
            detail: format!("failed to write ML dynamic-loader header: {}", e),
        }
    })?;
    write_source(write_sources, &ort_api_h_path, ONNXRUNTIME_C_API_H_SRC).map_err(|e| {
        CodegenError::Unsupported {
            backend: "binary".into(),
            detail: format!("failed to write vendored ONNX C API header: {}", e),
        }
    })?;
    write_source(write_sources, &onnx_h_path, ONNX_SHIM_H_SRC).map_err(|e| {
        CodegenError::Unsupported {
            backend: "binary".into(),
            detail: format!("failed to write ONNX shim header: {}", e),
        }
    })?;
    write_source(write_sources, &onnx_c_path, ONNX_SHIM_C_SRC).map_err(|e| {
        CodegenError::Unsupported {
            backend: "binary".into(),
            detail: format!("failed to write ONNX shim source: {}", e),
        }
    })?;
    write_source(write_sources, &tf_c_path, TF_SHIM_C_SRC).map_err(|e| {
        CodegenError::Unsupported {
            backend: "binary".into(),
            detail: format!("failed to write TensorFlow shim source: {}", e),
        }
    })?;
    write_source(write_sources, &pytorch_cpp_path, PYTORCH_SHIM_CPP_SRC).map_err(|e| {
        CodegenError::Unsupported {
            backend: "binary".into(),
            detail: format!("failed to write PyTorch shim source: {}", e),
        }
    })?;

    // ML compute kernels (conv2d, softmax, losses, optimizers, blocked matmul)
    let ml_h_path = tmp_dir.join("iris_ml_kernels.h");
    let ml_c_path = tmp_dir.join("iris_ml_kernels.c");
    write_source(write_sources, &ml_h_path, ML_KERNELS_H_SRC).map_err(|e| {
        CodegenError::Unsupported {
            backend: "binary".into(),
            detail: format!("failed to write ML kernels header: {}", e),
        }
    })?;
    write_source(write_sources, &ml_c_path, ML_KERNELS_C_SRC).map_err(|e| {
        CodegenError::Unsupported {
            backend: "binary".into(),
            detail: format!("failed to write ML kernels source: {}", e),
        }
    })?;

    // Locate compiler tools.
    // clang — compiles LLVM IR (.ll) to object files AND compiles the C
    //         runtime AND links the final binary (with -fuse-ld=lld).
    //         No GCC installation is required.
    let clang = find_clang();
    let msys2_inc = msys2_ucrt64_include();
    let msys2_lib = msys2_ucrt64_lib();
    let gcc_lib = msys2_gcc_lib();

    let mut target_args = vec!["-target".to_owned(), resolved_target.clone()];
    if resolved_target.contains("apple") {
        if let Some(sdk) = macos_sdk_path() {
            target_args.push("-isysroot".to_owned());
            target_args.push(sdk);
        }
    }
    let onnx_sdk = if let Ok(dir) = std::env::var("ONNXRUNTIME_DIR") {
        Some(PathBuf::from(dir))
    } else if Path::new("C:\\onnxruntime").exists() {
        Some(PathBuf::from("C:\\onnxruntime"))
    } else {
        None
    };
    let tf_sdk = if let Ok(dir) = std::env::var("TENSORFLOW_DIR") {
        Some(PathBuf::from(dir))
    } else if Path::new("C:\\tensorflow").exists() {
        Some(PathBuf::from("C:\\tensorflow"))
    } else {
        None
    };
    let libtorch_sdk = if resolved_target.contains("msvc") {
        if let Ok(dir) = std::env::var("LIBTORCH_DIR") {
            Some(PathBuf::from(dir))
        } else if Path::new("C:\\libtorch").exists() {
            Some(PathBuf::from("C:\\libtorch"))
        } else {
            None
        }
    } else {
        None
    };
    let openblas_dir = if self_contained {
        None
    } else if let Ok(dir) = std::env::var("OPENBLAS_DIR") {
        Some(PathBuf::from(dir))
    } else if Path::new("C:\\openblas").exists() {
        Some(PathBuf::from("C:\\openblas"))
    } else {
        None
    };
    let use_blas = openblas_dir.is_some()
        || !self_contained
            && std::env::var("IRIS_USE_BLAS")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
    // NOTE: IRIS_NATIVE_ML_BACKENDS no longer affects codegen. ONNX and
    // TensorFlow are always compiled in and resolved at runtime via dlopen, and
    // LibTorch is a separately built plugin. The SDK paths below are retained
    // only so `iris build-plugin torch` can find LibTorch, and so the ONNX DLL
    // can be staged next to the output binary as a convenience.
    if std::env::var("IRIS_NATIVE_ML_BACKENDS").is_ok() {
        eprintln!(
            "iris_codegen: note — IRIS_NATIVE_ML_BACKENDS is obsolete and ignored; \
             ML backends are now loaded at runtime"
        );
    }

    // 5a. Compile iris_runtime.c → iris_runtime.o using clang (cached).
    //     Cache lives next to the compiler binary or in ~/.iris/cache/.
    //     Invalidated when the compiler binary is newer than the cached .o.
    let obj_dir = prebuilt_dir.clone().unwrap_or_else(|| tmp_dir.clone());
    let rt_obj = obj_dir.join("iris_runtime.o");
    let compile_rt = |c_path: &Path, out_obj: &Path| -> Result<(), CodegenError> {
        let mut cmd = Command::new(&clang);
        cmd.args(&target_args);
        cmd.args([
            "-O2",
            "-c",
            path_str(c_path)?,
            "-o",
            path_str(out_obj)?,
            "-I",
            path_str(&tmp_dir)?,
            "-Wno-pragma-pack",
        ]);
        if let Some(ref s) = sanitize {
            cmd.arg(format!("-fsanitize={s}"));
            cmd.args(["-fno-omit-frame-pointer", "-g", "-DIRIS_SANITIZED"]);
        }
        // No -D<BACKEND>_ENABLED flags by default: ML backends are resolved at
        // runtime via dlopen (see iris_ml_dynload.h) so prebuilt iris_runtime.o
        // is byte-identical across targets. Users who want compile-time static
        // backend linking can opt in via `IRIS_ML_STATIC_LINK=1`.
        if MlLinkMode::from_env() == MlLinkMode::StaticDirect {
            cmd.arg("-DIRIS_ML_STATIC_LINK=1");
        }
        if resolved_target.contains("windows") && !resolved_target.contains("msvc") {
            if let Some(ref inc) = msys2_inc {
                cmd.arg("-I").arg(inc);
            }
        }
        let output = cmd.output().map_err(|e| CodegenError::ToolchainMissing {
            detail: format!("the C compiler '{}' could not be started ({})", clang, e),
        })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            return Err(CodegenError::Unsupported {
                backend: "binary".into(),
                detail: format!(
                    "'{}' failed to compile iris_runtime.c (exit: {:?})\nstderr: {}\nstdout: {}",
                    clang,
                    output.status.code(),
                    stderr,
                    stdout
                ),
            });
        }
        Ok(())
    };
    if let Some(pre) = prebuilt {
        // No usable cache directory: stage the objects in this build's
        // directory, as before.
        if prebuilt_dir.is_none() {
            for (name, bytes) in pre.objects {
                std::fs::write(tmp_dir.join(name), bytes).map_err(|e| {
                    CodegenError::Unsupported {
                        backend: "binary".into(),
                        detail: format!("failed to write prebuilt object '{}': {}", name, e),
                    }
                })?;
            }
        }
        if verbose() {
            eprintln!(
                "iris_codegen: using prebuilt runtime objects for {} (no C compiler required)",
                resolved_target
            );
        }
    } else {
        let cache_dir = runtime_cache_dir();
        let cached_rt = cache_dir.join(runtime_cache_object_name(&resolved_target));
        if sanitize.is_some() {
            compile_rt(&c_path, &rt_obj)?;
        } else if cached_rt.is_file() {
            std::fs::copy(&cached_rt, &rt_obj).map_err(|e| CodegenError::Unsupported {
                backend: "binary".into(),
                detail: format!("failed to copy cached iris_runtime.o: {}", e),
            })?;
        } else {
            compile_rt(&c_path, &rt_obj)?;
            // Populate the content-addressed cache atomically (best-effort).
            // An interrupted copy must not leave an object that a later
            // compiler invocation mistakes for valid.
            if std::fs::create_dir_all(&cache_dir).is_ok() {
                let pending = cache_dir.join(format!(
                    ".iris_runtime_{}_{}.tmp",
                    std::process::id(),
                    now_nanos
                ));
                if std::fs::copy(&rt_obj, &pending).is_ok()
                    && std::fs::rename(&pending, &cached_rt).is_err()
                    && !cached_rt.is_file()
                {
                    let _ = std::fs::copy(&pending, &cached_rt);
                }
                let _ = std::fs::remove_file(pending);
            }
        }
    }

    // 5b. Compile LLVM IR → module.o using clang (only clang understands .ll).
    let mut support_objs = vec![rt_obj.clone()];
    // The ONNX and TensorFlow shims compile against the vendored/hand-declared
    // ABI in iris_ml_dynload.h and resolve their libraries via dlopen, so they
    // need no SDK include paths and no -D switches.
    //
    // pytorch_shim.cpp is deliberately absent: LibTorch is C++, so it is built
    // as a separate C-ABI plugin library rather than linked into every binary.
    // See build_torch_plugin() and iris_torch_available() in iris_runtime.c.
    for (src, obj_name, backend_name) in [
        (&onnx_c_path, "onnx_shim.o", "ONNX shim"),
        (&tf_c_path, "tf_shim.o", "TensorFlow shim"),
        (&ml_c_path, "iris_ml_kernels.o", "ML kernels"),
    ] {
        let obj = obj_dir.join(obj_name);

        // Already written from the embedded prebuilt set — nothing to compile.
        if prebuilt.is_some() {
            support_objs.push(obj);
            continue;
        }

        let mut shim_cmd = Command::new(&clang);
        shim_cmd.args(&target_args);
        shim_cmd.args([
            "-O2",
            "-c",
            path_str(src)?,
            "-o",
            path_str(&obj)?,
            "-I",
            path_str(&tmp_dir)?,
            "-Wno-pragma-pack",
        ]);
        if resolved_target.contains("windows") && !resolved_target.contains("msvc") {
            if let Some(ref inc) = msys2_inc {
                shim_cmd.arg("-I").arg(inc);
            }
        }
        if let Some(ref s) = sanitize {
            shim_cmd.arg(format!("-fsanitize={s}"));
            shim_cmd.args(["-fno-omit-frame-pointer", "-g"]);
        }
        if backend_name == "ML kernels" && use_blas {
            shim_cmd.arg("-DIRIS_USE_BLAS");
            if let Some(ref dir) = openblas_dir {
                shim_cmd.arg("-I").arg(dir.join("include"));
            }
        }
        if backend_name == "ML kernels"
            && (resolved_target.contains("x86_64")
                || (resolved_target.is_empty() && cfg!(target_arch = "x86_64")))
            && std::env::var("IRIS_NO_AVX2").is_err()
        {
            shim_cmd.args(["-mavx2", "-mfma"]);
        }

        let shim_output = shim_cmd
            .output()
            .map_err(|e| CodegenError::ToolchainMissing {
                detail: format!(
                    "the C compiler '{}' could not be started for the {} ({})",
                    clang, backend_name, e
                ),
            })?;
        if !shim_output.status.success() {
            let stderr = String::from_utf8_lossy(&shim_output.stderr);
            let stdout = String::from_utf8_lossy(&shim_output.stdout);
            return Err(CodegenError::Unsupported {
                backend: "binary".into(),
                detail: format!(
                    "'{}' failed to compile {} (exit: {:?})\nstderr: {}\nstdout: {}",
                    clang,
                    backend_name,
                    shim_output.status.code(),
                    stderr,
                    stdout
                ),
            });
        }
        support_objs.push(obj);
    }

    // Compile .ll -> .o.
    //
    // The LLVM C API is preferred: it needs no external compiler, which is what
    // lets `iris build` work on a machine with only LLVM installed. clang
    // remains the fallback.
    //
    // OPT-IN, via `IRIS_NO_CLANG=1`, and deliberately not the default.
    //
    // The handwritten binding's ABI and ownership bugs were fixed on
    // 2026-08-29, and emitted bytes are now checked for a real object header.
    // Clang stays the default until an LLVM-C host exercises the conditional
    // emission test and platform linker smoke test. See known-issues #45.
    //
    // There is a second reason for caution: the LLVM C API and TensorFlow both
    // embed LLVM, and loading both into one process can conflict.
    let mod_obj = tmp_dir.join("module.o");
    let compile_started = std::time::Instant::now();
    let no_clang = std::env::var("IRIS_NO_CLANG").is_ok() && sanitize.is_none();
    if no_clang && !crate::codegen::llvm_c_api::is_llvm_c_api_available() {
        return Err(CodegenError::Unsupported {
            backend: "binary".into(),
            detail: "IRIS_NO_CLANG is set, but no LLVM-C shared library is available".into(),
        });
    }
    // In process through LLVM-C when it is there, with the same optimisation
    // clang would apply -- about 15 ms for a small program where starting
    // clang costs 200. Clang still compiles debug (-g) and sanitizer builds,
    // and anything when IRIS_USE_CLANG is set.
    let opt_level: u8 = match std::env::var("IRIS_OPT_LEVEL").as_deref() {
        Ok("0") => 0,
        Ok("1") => 1,
        Ok("2") => 2,
        _ => 3,
    };
    let obj_cache_path = module_obj_cache_path(
        &llvm_ir,
        &resolved_target,
        opt_level,
        debug,
        sanitize.as_deref(),
    );
    let obj_cache_hit = obj_cache_path.as_ref().is_some_and(|p| {
        p.exists()
            && std::fs::metadata(p).is_ok_and(|m| m.len() > 0)
            && std::fs::copy(p, &mod_obj).is_ok()
    });
    if obj_cache_hit {
        if verbose() {
            eprintln!("iris_codegen: reused cached module.o");
        }
    } else {
        let in_process = no_clang
            || (!debug
                && sanitize.is_none()
                && std::env::var_os("IRIS_USE_CLANG").is_none()
                && crate::codegen::llvm_c_api::is_llvm_c_api_available());
        let in_process_ok = in_process
            && match crate::codegen::llvm_c_api::compile_llvm_ir_to_executable_object(
                &llvm_ir,
                &mod_obj,
                &resolved_target,
                opt_level,
            ) {
                Ok(()) => true,
                // Required: report it. Otherwise clang gets the module instead.
                Err(e) if no_clang => return Err(e),
                Err(e) => {
                    if verbose() {
                        eprintln!("iris_codegen: in-process compile failed ({e}); using clang");
                    }
                    false
                }
            };
        if in_process_ok {
            if verbose() {
                eprintln!("iris_codegen: compiled in process through LLVM-C");
            }
        } else {
            let opt_flag = match std::env::var("IRIS_OPT_LEVEL").as_deref() {
                _ if debug => "-O0",
                Ok("0") => "-O0",
                Ok("1") => "-O1",
                Ok("2") => "-O2",
                _ => "-O3",
            };
            let mut ir_cmd = Command::new(&clang);
            ir_cmd.args(&target_args);
            ir_cmd.args([
                opt_flag,
                "-fvectorize",
                "-fslp-vectorize",
                "-c",
                path_str(&ll_path)?,
                "-o",
                path_str(&mod_obj)?,
                "-Wno-override-module",
            ]);
            if (resolved_target.contains("x86_64")
                || (resolved_target.is_empty() && cfg!(target_arch = "x86_64")))
                && std::env::var("IRIS_NO_AVX2").is_err()
            {
                ir_cmd.args(["-mavx2", "-mfma"]);
            }
            if let Some(ref s) = sanitize {
                ir_cmd.arg(format!("-fsanitize={s}"));
                ir_cmd.args(["-fno-omit-frame-pointer", "-g"]);
            }
            // clang missing altogether: LLVM-C can still emit the object.
            let ir_status = match ir_cmd.status() {
                Ok(status) => status,
                Err(e) => {
                    crate::codegen::llvm_c_api::compile_llvm_ir_to_object(
                        &llvm_ir,
                        &mod_obj,
                        Some(&resolved_target),
                    )
                    .map_err(|_| CodegenError::ToolchainMissing {
                        detail: format!(
                        "neither clang ('{}': {}) nor an LLVM-C library could compile the program",
                        clang, e
                    ),
                    })?;
                    std::process::ExitStatus::default()
                }
            };
            if !ir_status.success() && sanitize.is_some() {
                return Err(CodegenError::Unsupported {
                    backend: "binary".into(),
                    detail: format!(
                        "clang could not compile the module with IRIS_SANITIZE (exit: {:?})",
                        ir_status.code()
                    ),
                });
            }
            if !ir_status.success() {
                // Clang failed; try LLVM C API as a last resort.
                if let Err(e) = crate::codegen::llvm_c_api::compile_llvm_ir_to_object(
                    &llvm_ir,
                    &mod_obj,
                    Some(&resolved_target),
                ) {
                    return Err(CodegenError::Unsupported {
                        backend: "binary".into(),
                        detail: format!(
                        "both clang and LLVM C API failed to compile LLVM IR: {}. clang exit: {:?}",
                        e,
                        ir_status.code()
                    ),
                    });
                }
            }
        }
        if let Some(ref cache_p) = obj_cache_path {
            if let Some(parent) = cache_p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::copy(&mod_obj, cache_p);
        }
    }

    // 6. Link module.o + iris_runtime.o → native binary.
    //    MinGW uses ld.lld directly with the MSYS2 UCRT startup objects and
    //    import libraries. Unix linking goes through clang because the compiler
    //    driver owns SDK/sysroot and default-library discovery.
    // `IRIS_NO_CLANG=1` requires both LLVM-C object emission and direct linking.
    // `IRIS_REQUIRE_DIRECT_LLD=1` is narrower: clang may emit objects, but must
    // not be used as the linker driver. CI uses it to ensure the direct-link
    // path cannot silently regress behind the clang fallback.
    if verbose() {
        eprintln!(
            "iris_codegen: compiled the program in {} ms",
            compile_started.elapsed().as_millis()
        );
    }
    let link_started = std::time::Instant::now();
    let require_direct_lld = no_clang || std::env::var("IRIS_REQUIRE_DIRECT_LLD").is_ok();
    let direct_mingw = resolved_target.contains("windows") && !resolved_target.contains("msvc");
    let link_result = if direct_mingw {
        if let Some(lld) = crate::codegen::build::find_lld() {
            link_with_lld(
                &lld,
                &resolved_target,
                &mod_obj,
                &support_objs,
                output_path,
                &target_args,
                &msys2_lib,
                &gcc_lib,
                &onnx_sdk,
                &tf_sdk,
                &libtorch_sdk,
                &openblas_dir,
                use_blas,
                &link_libs,
                opts,
            )
        } else {
            Err(CodegenError::Unsupported {
                backend: "binary".into(),
                detail: "ld.lld is not available for direct MinGW linking".into(),
            })
        }
    } else {
        Err(CodegenError::Unsupported {
            backend: "binary".into(),
            detail: format!("direct ld.lld is only supported for MinGW, not {resolved_target}"),
        })
    };

    let link_output = match link_result {
        Ok(()) => {
            if verbose() {
                eprintln!(
                    "iris_codegen: linked via ld.lld directly in {} ms",
                    link_started.elapsed().as_millis()
                );
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(metadata) = std::fs::metadata(output_path) {
                    let mut perms = metadata.permissions();
                    perms.set_mode(0o755);
                    let _ = std::fs::set_permissions(output_path, perms);
                }
            }
            return Ok(output_path.to_path_buf());
        }
        Err(e) => {
            if require_direct_lld {
                return Err(CodegenError::Unsupported {
                    backend: "binary".into(),
                    detail: format!("direct ld.lld linking is required but failed: {}", e),
                });
            }
            // Direct ld.lld linking is only tried for MinGW; elsewhere clang
            // is simply the linker, which is not worth a message.
            if direct_mingw && verbose() {
                eprintln!(
                    "iris_codegen: ld.lld link failed ({}), falling back to clang",
                    e
                );
            }
            // Fallback: link via clang
            let mut link_cmd = Command::new(&clang);
            link_cmd.args(&target_args);
            if cfg!(target_os = "macos") || resolved_target.contains("apple") {
                link_cmd.args(["-O2", path_str(&mod_obj)?]);
            } else {
                link_cmd.args(["-fuse-ld=lld", "-O2", path_str(&mod_obj)?]);
            }
            for obj in &support_objs {
                link_cmd.arg(path_str(obj)?);
            }
            for obj in &opts.extra_objects {
                link_cmd.arg(path_str(obj)?);
            }
            link_cmd.args(["-o", path_str(output_path)?]);
            if opts.gui_subsystem && resolved_target.contains("windows") {
                if resolved_target.contains("msvc") {
                    link_cmd.args(["-Wl,/SUBSYSTEM:WINDOWS", "-Wl,/ENTRY:mainCRTStartup"]);
                } else {
                    link_cmd.args(["-mwindows", "-Wl,-e,mainCRTStartup"]);
                }
            }
            if !resolved_target.contains("windows") {
                for rpath in &opts.rpaths {
                    link_cmd.arg(format!("-Wl,-rpath,{rpath}"));
                }
            }
            if let Some(ref s) = sanitize {
                link_cmd.arg(format!("-fsanitize={s}"));
            }
            if !resolved_target.contains("msvc") && !resolved_target.contains("apple") {
                link_cmd.args(["-lm", "-lpthread"]);
            }
            if resolved_target.contains("windows") {
                link_cmd.args(["-lws2_32", "-lwinhttp"]);
            }
            if resolved_target.contains("windows") && !resolved_target.contains("msvc") {
                if let Some(ref lib) = msys2_lib {
                    link_cmd.arg(format!("-L{}", lib));
                }
                if let Some(ref lib) = gcc_lib {
                    link_cmd.arg(format!("-L{}", lib));
                }
            }
            // ONNX Runtime, TensorFlow and LibTorch are intentionally NOT linked.
            // They are resolved at runtime (dlopen) so that a binary which never
            // calls into ML does not require the libraries to be present, and so
            // that the runtime object stays SDK-independent.
            if use_blas {
                if let Some(ref dir) = openblas_dir {
                    link_cmd.arg(format!("-L{}", dir.join("lib").display()));
                }
                link_cmd.arg("-lopenblas");
            }
            let mut seen_libs = std::collections::HashSet::new();
            for lib in link_libs.iter().filter(|lib| {
                !(resolved_target.contains("apple") && matches!(lib.as_str(), "m" | "pthread"))
            }) {
                if seen_libs.insert(lib.as_str()) {
                    append_link_lib_arg(&mut link_cmd, lib);
                }
            }
            link_cmd.output()
        }
    };

    let link_output = match link_output {
        Ok(link_output) => link_output,
        Err(e) => {
            return Err(CodegenError::ToolchainMissing {
                detail: format!("the linker driver '{}' could not be started ({})", clang, e),
            })
        }
    };
    if !link_output.status.success() {
        let stderr = String::from_utf8_lossy(&link_output.stderr);
        return Err(CodegenError::Unsupported {
            backend: "binary".into(),
            detail: format!(
                "'{}' failed to link binary (exit: {:?})\n{}",
                clang,
                link_output.status.code(),
                stderr
            ),
        });
    }

    #[cfg(target_os = "macos")]
    if resolved_target.contains("apple") {
        let _ = Command::new("codesign")
            .args(["-s", "-", "-f", path_str(output_path)?])
            .output();
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(output_path) {
            let mut perms = metadata.permissions();
            perms.set_mode(0o755);
            let _ = std::fs::set_permissions(output_path, perms);
        }
    }

    Ok(output_path.to_path_buf())
}

/// Delete a per-build temp directory once its outputs have been linked.
///
/// These directories hold `module.ll`, the staged C runtime sources and the
/// object files — tens of megabytes for a single build. Nothing removed them, and
/// `--emit eval` creates one per evaluated program, so a test run leaked one
/// directory per test: 2129 of them had accumulated on this machine, occupying
/// roughly 23 GB and eventually filling the disk, which fails builds in ways that
/// look nothing like a disk problem.
///
/// Best-effort by design: a build that succeeded must not be reported as failed
/// because cleanup could not run. Set `IRIS_KEEP_BUILD_DIR=1` to retain the
/// directory when inspecting generated IR or objects.
/// The sanitizer requested by `IRIS_SANITIZE` (`address`, `undefined`,
/// `address,undefined`, ...), passed to clang as `-fsanitize=<value>`.
fn sanitizer_from_env() -> Option<String> {
    std::env::var("IRIS_SANITIZE")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == ',' || c == '-')
        })
}

/// Mark every function definition `sanitize_address`: AddressSanitizer
/// instruments only functions carrying the attribute, which clang adds to C
/// functions but generated IR does not have.
fn mark_sanitize_address(ir: &str) -> String {
    let mut out = String::with_capacity(ir.len() + 4096);
    for line in ir.lines() {
        let trimmed = line.trim_end();
        if trimmed.starts_with("define ") && trimmed.ends_with('{') {
            let cut = [" section ", " personality ", " comdat"]
                .iter()
                .filter_map(|k| trimmed.find(k))
                .min()
                .unwrap_or(trimmed.len() - 1);
            out.push_str(trimmed[..cut].trim_end());
            out.push_str(" sanitize_address ");
            out.push_str(trimmed[cut..].trim_start());
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

fn cleanup_build_dir(tmp_dir: &std::path::Path) {
    if std::env::var("IRIS_KEEP_BUILD_DIR").is_ok() {
        eprintln!(
            "iris_codegen: keeping build dir {} (IRIS_KEEP_BUILD_DIR set)",
            tmp_dir.display()
        );
        return;
    }
    let _ = std::fs::remove_dir_all(tmp_dir);
}

/// Removes a build directory when it goes out of scope.
///
/// `build_binary_impl` returns early through `?` at roughly a dozen points — a
/// missing compiler, a clang failure, a link error. Cleaning up only on the
/// success paths therefore still leaked one directory per *failed* build, which a
/// test run produces plenty of. Tying removal to scope exit covers every path,
/// including the error ones, without threading cleanup through each `?`.
struct BuildDirGuard(PathBuf);

impl Drop for BuildDirGuard {
    fn drop(&mut self) {
        cleanup_build_dir(&self.0);
    }
}

/// Removes a temporary executable when it goes out of scope.
///
/// The eval path deleted its binary only after a successful run, so every
/// timeout or runtime failure left one behind — the same early-return leak as
/// the build directories, and the reason 34 stray executables survived a single
/// suite run. Honours `IRIS_KEEP_BUILD_DIR` for consistency with the directory
/// guard, since inspecting a failing binary is the same debugging task.
struct TempExeGuard(PathBuf);

impl Drop for TempExeGuard {
    fn drop(&mut self) {
        if std::env::var("IRIS_KEEP_BUILD_DIR").is_ok() {
            return;
        }
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The network calls a program built from `llvm_ir` can reach from `main`
/// (or a global constructor): `(function, runtime call)` pairs. WASI
/// preview 1 has no sockets, so on `wasm32-wasip1` these are build errors
/// rather than calls that always fail. Code nothing reaches -- a standard
/// library's unused HTTP helpers -- is not counted.
pub(crate) fn reachable_network_calls(llvm_ir: &str) -> Vec<(String, String)> {
    use std::collections::{HashMap, HashSet, VecDeque};

    /// Every `@name` an LLVM line refers to.
    fn refs(line: &str) -> Vec<String> {
        let bytes = line.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] != b'@' {
                i += 1;
                continue;
            }
            i += 1;
            if bytes.get(i) == Some(&b'"') {
                let start = i + 1;
                let end = line[start..].find('"').map_or(line.len(), |e| start + e);
                out.push(line[start..end].to_owned());
                i = end + 1;
            } else {
                let start = i;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric()
                        || matches!(bytes[i], b'_' | b'.' | b'$' | b'-'))
                {
                    i += 1;
                }
                if i > start {
                    out.push(line[start..i].to_owned());
                }
            }
        }
        out
    }

    let is_network = |name: &str| {
        (name.starts_with("iris_tcp_") || name.starts_with("iris_udp_") || name.starts_with("iris_http_"))
            // Report-only: no socket is touched (they answer "no TLS", 0).
            && name != "iris_http_tls_available"
            && name != "iris_http_last_status"
    };

    // Functions and globals, and what each refers to.
    let mut edges: HashMap<String, Vec<String>> = HashMap::new();
    let mut current: Option<String> = None;
    for line in llvm_ir.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("define ") {
            current = refs(rest).into_iter().next();
            if let Some(name) = &current {
                edges.entry(name.clone()).or_default();
            }
            continue;
        }
        if trimmed == "}" {
            current = None;
            continue;
        }
        if let Some(name) = &current {
            edges.entry(name.clone()).or_default().extend(refs(line));
        } else if trimmed.starts_with('@') {
            // A global's initializer: vtables, constructor lists.
            let mut names = refs(trimmed).into_iter();
            if let Some(global) = names.next() {
                edges.entry(global).or_default().extend(names);
            }
        }
    }

    let mut seen: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = ["main", "llvm.global_ctors"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let mut found = Vec::new();
    while let Some(name) = queue.pop_front() {
        if !seen.insert(name.clone()) {
            continue;
        }
        for callee in edges.get(&name).into_iter().flatten() {
            if is_network(callee) {
                let pair = (name.clone(), callee.clone());
                if !found.contains(&pair) {
                    found.push(pair);
                }
            } else if !seen.contains(callee) {
                queue.push_back(callee.clone());
            }
        }
    }
    found
}

/// Build a WASM binary using the WASI sysroot (wasi-libc + compiler-rt).
fn build_wasm_binary_impl(
    llvm_ir: &str,
    output_path: &Path,
    target: &str,
) -> Result<PathBuf, CodegenError> {
    // Determine P1 vs P2 target
    let is_p2 = target.contains("wasip2");
    if !is_p2 {
        let calls = reachable_network_calls(llvm_ir);
        if !calls.is_empty() {
            let list: Vec<String> = calls
                .iter()
                .map(|(f, c)| {
                    format!(
                        "`{}` calls `{c}`",
                        if f == "iris_main" { "main" } else { f }
                    )
                })
                .collect();
            return Err(CodegenError::TargetLacks {
                target: "wasm32-wasip1".into(),
                detail: format!(
                    "it uses the network, and WASI preview 1 has no sockets ({}). Build for \
                     `--target wasm32-wasip2` (WASI preview 2 has sockets), or natively",
                    list.join(", ")
                ),
            });
        }
    }

    // Locate the WASI sysroot.
    let sysroot = find_wasi_sysroot()?;
    let wasm_target = if is_p2 {
        "wasm32-wasip2"
    } else {
        "wasm32-wasip1"
    };
    let wasm_lib_subdir = if is_p2 {
        "wasm32-wasip2"
    } else {
        "wasm32-wasip1"
    };

    // Set up temp directory for build artifacts.
    let build_id = output_path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| format!("{}_wasm", s))
        .unwrap_or_else(|| format!("iris_wasm_build_{}", std::process::id()));
    let tmp_dir = std::env::temp_dir().join(build_id);
    std::fs::create_dir_all(&tmp_dir).map_err(|e| CodegenError::Unsupported {
        backend: "wasm".into(),
        detail: format!("failed to create temp dir '{}': {}", tmp_dir.display(), e),
    })?;
    // Same leak as the native path had; see BuildDirGuard.
    let _build_dir_guard = BuildDirGuard(tmp_dir.clone());

    let clang = find_clang();

    // Write LLVM IR to module.ll. Clang names a C `main(argc, argv)`
    // `__main_argc_argv` on WASI -- what wasi-libc's start code calls -- and
    // LLVM IR gets no such renaming, so without this `_start` found no `main`
    // and trapped.
    let llvm_ir = llvm_ir.replacen(
        "define i32 @main(i32 %argc, ptr %argv)",
        "define i32 @__main_argc_argv(i32 %argc, ptr %argv)",
        1,
    );
    let ll_path = tmp_dir.join("module.ll");
    std::fs::write(&ll_path, &llvm_ir).map_err(|e| CodegenError::Unsupported {
        backend: "wasm".into(),
        detail: format!("failed to write LLVM IR to '{}': {}", ll_path.display(), e),
    })?;

    // Write embedded runtime sources
    let h_path = tmp_dir.join("iris_runtime.h");
    let c_path = tmp_dir.join("iris_runtime.c");
    std::fs::write(&h_path, RUNTIME_H_SRC).map_err(|e| CodegenError::Unsupported {
        backend: "wasm".into(),
        detail: format!("failed to write runtime header: {}", e),
    })?;
    std::fs::write(&c_path, RUNTIME_C_SRC).map_err(|e| CodegenError::Unsupported {
        backend: "wasm".into(),
        detail: format!("failed to write runtime C source: {}", e),
    })?;

    // WASM compilation flags
    let wasi_defines = [
        "-D_POSIX_C_SOURCE=200809L",
        "-D_WASI_EMULATED_SIGNAL",
        "-D_WASI_EMULATED_PROCESS_CLOCKS",
        "-D_WASI_EMULATED_GETPID",
    ];
    let sysroot_opt = format!("--sysroot={}", sysroot.display());
    let inc_opt = format!("-I{}", tmp_dir.display());

    // Step 1: Compile iris_runtime.c → iris_runtime.o
    let rt_obj = tmp_dir.join("iris_runtime.o");
    let mut rt_cmd = std::process::Command::new(&clang);
    rt_cmd.args([
        "-target",
        wasm_target,
        "-O2",
        "-c",
        path_str(&c_path)?,
        "-o",
        path_str(&rt_obj)?,
        &inc_opt,
        &sysroot_opt,
    ]);
    if is_p2 {
        rt_cmd.arg("-D__wasip2__=1");
    }
    for def in &wasi_defines {
        rt_cmd.arg(def);
    }
    let rt_output = rt_cmd
        .output()
        .map_err(|e| CodegenError::ToolchainMissing {
            detail: format!(
                "the C compiler '{}' could not be started for WebAssembly ({})",
                clang, e
            ),
        })?;
    if !rt_output.status.success() {
        let stderr = String::from_utf8_lossy(&rt_output.stderr);
        return Err(CodegenError::Unsupported {
            backend: "wasm".into(),
            detail: format!(
                "'{}' failed to compile iris_runtime.c for WASM (exit: {:?})\nstderr: {}",
                clang,
                rt_output.status.code(),
                stderr,
            ),
        });
    }

    // Step 3: Link → .wasm (pass LLVM IR directly instead of pre-compiling to .o)
    let sysroot_lib = sysroot.join("lib").join(wasm_lib_subdir);
    let p1_lib_dir = sysroot.join("lib").join("wasm32-wasip1");
    let mut link_cmd = std::process::Command::new(&clang);
    link_cmd.args([
        "-target",
        wasm_target,
        "-O2",
        &sysroot_opt,
        "-nodefaultlibs",
        path_str(&ll_path)?, // LLVM IR directly (not pre-compiled)
        path_str(&rt_obj)?,  // C runtime object
    ]);
    // Add library paths
    link_cmd.arg(format!("-L{}", sysroot_lib.display()));
    if is_p2 {
        // P2 needs P1 lib dir for compiler-rt (not bundled in P2 sysroot)
        link_cmd.arg(format!("-L{}", p1_lib_dir.display()));
    }
    link_cmd.args([
        "-lc",
        "-lwasi-emulated-signal",
        "-lwasi-emulated-process-clocks",
        "-lclang_rt.builtins-wasm32",
        "-o",
        path_str(output_path)?,
    ]);
    let link_output = link_cmd.output().map_err(|e| CodegenError::Unsupported {
        backend: "wasm".into(),
        detail: format!("'{}' link step could not start: {}", clang, e),
    })?;
    if !link_output.status.success() {
        let stderr = String::from_utf8_lossy(&link_output.stderr);
        return Err(CodegenError::Unsupported {
            backend: "wasm".into(),
            detail: format!(
                "'{}' failed to link WASM binary (exit: {:?})\n{}",
                clang,
                link_output.status.code(),
                stderr,
            ),
        });
    }

    // Step 4 (P2 only): Post-process with wasm-tools → P2 component
    if is_p2 {
        let wasm_tools = find_wasm_tools()?;
        let adapter = find_wasi_p2_adapter()?;
        let p2_output = tmp_dir.join("output.p2.wasm");

        let mut component_cmd = std::process::Command::new(&wasm_tools);
        component_cmd.args([
            "component",
            "new",
            path_str(output_path)?,
            "--adapt",
            path_str(&adapter)?,
            "-o",
            path_str(&p2_output)?,
        ]);
        let comp_output = component_cmd
            .output()
            .map_err(|e| CodegenError::Unsupported {
                backend: "wasm".into(),
                detail: format!("'{}' could not start: {}", wasm_tools.display(), e),
            })?;
        if !comp_output.status.success() {
            let stderr = String::from_utf8_lossy(&comp_output.stderr);
            return Err(CodegenError::Unsupported {
                backend: "wasm".into(),
                detail: format!(
                    "'{}' failed to convert WASM to P2 component (exit: {:?})\n{}",
                    wasm_tools.display(),
                    comp_output.status.code(),
                    stderr,
                ),
            });
        }
        // Replace the output with the P2 component
        std::fs::copy(&p2_output, output_path).map_err(|e| CodegenError::Unsupported {
            backend: "wasm".into(),
            detail: format!("failed to copy P2 component to output path: {}", e),
        })?;
    }

    Ok(output_path.to_path_buf())
}

/// Locate `wasm-tools` executable by searching IRIS install prefixes and PATH.
fn find_wasm_tools() -> Result<PathBuf, CodegenError> {
    if let Ok(path) = std::env::var("IRIS_WASM_TOOLS") {
        let p = PathBuf::from(&path);
        if p.is_file() {
            return Ok(p);
        }
    }
    let exe_name = if cfg!(windows) {
        "wasm-tools.exe"
    } else {
        "wasm-tools"
    };
    for prefix in install_prefix_dirs() {
        for cand in [
            prefix.join("bin").join(exe_name),
            prefix.join("sdks").join("wasm").join(exe_name),
            prefix.join("sdks").join("wasm").join("bin").join(exe_name),
            prefix.join("toolchain").join(exe_name),
        ] {
            if cand.is_file() {
                return Ok(cand);
            }
        }
    }
    #[cfg(windows)]
    let search_cmd = "where";
    #[cfg(not(windows))]
    let search_cmd = "which";
    if let Ok(output) = std::process::Command::new(search_cmd)
        .arg("wasm-tools")
        .output()
    {
        if output.status.success() {
            let path_str = String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            if !path_str.is_empty() {
                let p = PathBuf::from(&path_str);
                if p.is_file() {
                    return Ok(p);
                }
            }
        }
    }
    Err(CodegenError::Unsupported {
        backend: "wasm".into(),
        detail: "wasm-tools not found. Install via `iris setup wasm` or `cargo install wasm-tools` or set IRIS_WASM_TOOLS".into(),
    })
}

/// Locate the WASI P1→P2 adapter module.
fn find_wasi_p2_adapter() -> Result<PathBuf, CodegenError> {
    if let Ok(path) = std::env::var("IRIS_WASI_P2_ADAPTER") {
        let p = PathBuf::from(&path);
        if p.is_file() {
            return Ok(p);
        }
    }
    let mut candidates = Vec::new();
    for prefix in install_prefix_dirs() {
        candidates.push(prefix.join("toolchain").join("wasi_snapshot_preview1.wasm"));
        candidates.push(
            prefix
                .join("sdks")
                .join("wasm")
                .join("wasi_snapshot_preview1.wasm"),
        );
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".into());
    candidates.push(
        PathBuf::from(&home)
            .join("AppData")
            .join("Local")
            .join("Temp")
            .join("wasi_snapshot_preview1.wasm"),
    );
    candidates.push(
        PathBuf::from(&home)
            .join(".iris")
            .join("toolchain")
            .join("wasi_snapshot_preview1.wasm"),
    );
    candidates.push(PathBuf::from("wasi_snapshot_preview1.wasm"));
    for c in &candidates {
        if c.is_file() {
            return Ok(c.clone());
        }
    }
    Err(CodegenError::Unsupported {
        backend: "wasm".into(),
        detail: "WASI P1→P2 adapter module not found. Download from https://github.com/bytecodealliance/wasmtime/releases and set IRIS_WASI_P2_ADAPTER".into(),
    })
}

/// Locate the WASI sysroot directory (contains wasi-libc, compiler-rt, etc.).
fn find_wasi_sysroot() -> Result<PathBuf, CodegenError> {
    let candidates: Vec<PathBuf> = {
        let mut c = Vec::new();
        if let Ok(v) = std::env::var("IRIS_WASI_SYSROOT") {
            if !v.is_empty() {
                c.push(PathBuf::from(v));
            }
        }
        for prefix in install_prefix_dirs() {
            c.push(prefix.join("toolchain").join("wasi-sysroot"));
            c.push(prefix.join("sdks").join("wasm").join("wasi-sysroot"));
            c.push(prefix.join("sdks").join("wasm"));
        }
        c
    };

    // Check each candidate for an actual sysroot version directory (e.g., wasi-sysroot-24.0)
    for base in &candidates {
        if !base.exists() {
            continue;
        }
        // Look for a versioned subdirectory or use the base itself
        if let Ok(entries) = std::fs::read_dir(base) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir()
                    && path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .is_some_and(|s| s.starts_with("wasi-sysroot"))
                {
                    // Verify it has lib/wasm32-wasip1/libc.a
                    let libc = path.join("lib/wasm32-wasip1/libc.a");
                    if libc.exists() {
                        return Ok(path);
                    }
                }
            }
            // Also check the base itself
            let libc = base.join("lib/wasm32-wasip1/libc.a");
            if libc.exists() {
                return Ok(base.to_path_buf());
            }
        }
    }

    Err(CodegenError::Unsupported {
        backend: "wasm".into(),
        detail: "WASI sysroot not found. Install the WASI SDK (wasi-sysroot) to ~/.iris/toolchain/wasi-sysroot/ or set IRIS_WASI_SYSROOT".into(),
    })
}

pub(crate) fn resolve_target_triple(target: Option<&str>) -> String {
    match target {
        Some(t) => crate::codegen::llvm_ir::target_preset_to_triple(t).unwrap_or(t),
        None => crate::codegen::llvm_ir::native_target_triple(),
    }
    .to_owned()
}

/// Find clang — required for compiling LLVM IR, C code, and linking.
/// Search order: IRIS_CLANG env var, next to iris binary (bundled),
/// Inno Setup install dir, system LLVM, PATH.
pub(crate) fn find_clang() -> String {
    // Env-var override takes precedence over all automatic detection.
    if let Ok(v) = std::env::var("IRIS_CLANG") {
        if !v.is_empty() {
            return v;
        }
    }

    let mut candidates: Vec<String> = Vec::new();

    // 1. Installed / bundled IRIS prefixes (<prefix>/toolchain/llvm/bin/clang[.exe])
    let clang_name = if cfg!(target_os = "windows") {
        "clang.exe"
    } else {
        "clang"
    };
    for prefix in install_prefix_dirs() {
        candidates.push(
            prefix
                .join("toolchain")
                .join("llvm")
                .join("bin")
                .join(clang_name)
                .to_string_lossy()
                .into_owned(),
        );
    }

    #[cfg(target_os = "windows")]
    {
        // 2. Standalone LLVM version directories, newest supported first.
        candidates.push(r"C:\llvm-23.1.1\bin\clang.exe".into());
        candidates.push(r"C:\llvm-23\bin\clang.exe".into());
        candidates.push(r"C:\llvm-22\bin\clang.exe".into());
        candidates.push(r"C:\llvm-21\bin\clang.exe".into());
        candidates.push(r"C:\llvm-20\bin\clang.exe".into());
        candidates.push(r"C:\llvm-19\bin\clang.exe".into());
        candidates.push(r"C:\llvm-18\bin\clang.exe".into());

        // 4. System-wide LLVM installs (standard Inno Setup path)
        candidates.push(r"C:\Program Files\LLVM\bin\clang.exe".into());
        candidates.push(r"C:\Program Files (x86)\LLVM\bin\clang.exe".into());

        // 5. Legacy user-local fallback
        if let Ok(home) = std::env::var("USERPROFILE") {
            candidates.push(format!(r"{}\.iris\llvm\bin\clang.exe", home));
        }

        // 5. MSYS2-style paths (from MSYS2/MINGW shells)
        candidates.push("/c/Program Files/LLVM/bin/clang.exe".into());
    }

    #[cfg(target_os = "macos")]
    {
        // macOS: package-installed toolchain, Homebrew LLVM, Xcode CLT
        candidates.push("/usr/local/share/iris/toolchain/llvm/bin/clang".into());
        candidates.push("/opt/homebrew/opt/llvm/bin/clang".into());
        candidates.push("/usr/local/opt/llvm/bin/clang".into());
        candidates.push("/usr/bin/clang".into());
        if let Ok(home) = std::env::var("HOME") {
            candidates.push(format!("{}/.iris/toolchain/llvm/bin/clang", home));
            candidates.push(format!("{}/.iris/llvm/bin/clang", home));
        }
    }

    #[cfg(target_os = "linux")]
    {
        // Linux: package-installed toolchain, then common distribution paths
        candidates.push("/usr/share/iris/toolchain/llvm/bin/clang".into());
        candidates.push("/usr/lib/llvm-23/bin/clang".into());
        candidates.push("/usr/lib/llvm-22/bin/clang".into());
        candidates.push("/usr/lib/llvm-21/bin/clang".into());
        candidates.push("/usr/lib/llvm-18/bin/clang".into());
        candidates.push("/usr/lib/llvm-17/bin/clang".into());
        candidates.push("/usr/bin/clang".into());
        if let Ok(home) = std::env::var("HOME") {
            candidates.push(format!("{}/.iris/toolchain/llvm/bin/clang", home));
            candidates.push(format!("{}/.iris/llvm/bin/clang", home));
        }
    }

    for p in &candidates {
        if std::path::Path::new(p).exists() {
            return p.clone();
        }
    }
    // Fall back to PATH lookup.
    "clang".to_owned()
}

/// Locate the macOS SDK sysroot for clang when targeting Apple platforms.
fn macos_sdk_path() -> Option<String> {
    if let Ok(sdk) = std::env::var("SDKROOT") {
        let trimmed = sdk.trim();
        if !trimmed.is_empty() && Path::new(trimmed).is_dir() {
            return Some(trimmed.to_owned());
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = Command::new("xcrun").args(["--show-sdk-path"]).output() {
            if output.status.success() {
                let sdk = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !sdk.is_empty() && Path::new(&sdk).is_dir() {
                    return Some(sdk);
                }
            }
        }
        for candidate in [
            "/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk",
            "/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk",
        ] {
            if Path::new(candidate).is_dir() {
                return Some(candidate.to_owned());
            }
        }
    }
    None
}

/// Find `ld.lld` (LLVM linker) — used as a clang-free alternative for linking.
/// Search order mirrors `find_clang`: env var, exe-relative, Inno Setup, system LLVM, PATH.
pub(crate) fn find_lld() -> Option<String> {
    if let Ok(v) = std::env::var("IRIS_LD") {
        if !v.is_empty() && std::path::Path::new(&v).exists() {
            return Some(v);
        }
    }

    let exe_name = if cfg!(target_os = "windows") {
        "ld.lld.exe"
    } else {
        "ld.lld"
    };

    let mut candidates: Vec<String> = Vec::new();

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(exe_name).to_string_lossy().into_owned());
        }
    }
    for prefix in install_prefix_dirs() {
        candidates.push(
            prefix
                .join("toolchain")
                .join("llvm")
                .join("bin")
                .join(exe_name)
                .to_string_lossy()
                .into_owned(),
        );
    }

    #[cfg(target_os = "windows")]
    {
        // Standalone LLVM version directories, newest supported first.
        candidates.push(format!(r"C:\llvm-23.1.1\bin\{}", exe_name));
        candidates.push(format!(r"C:\llvm-23\bin\{}", exe_name));
        candidates.push(format!(r"C:\llvm-22\bin\{}", exe_name));
        candidates.push(format!(r"C:\llvm-21\bin\{}", exe_name));
        candidates.push(format!(r"C:\llvm-20\bin\{}", exe_name));
        candidates.push(format!(r"C:\llvm-19\bin\{}", exe_name));
        candidates.push(format!(r"C:\llvm-18\bin\{}", exe_name));
        // System-wide LLVM (standard Inno Setup path)
        candidates.push(format!(r"C:\Program Files\LLVM\bin\{}", exe_name));
        candidates.push(format!(r"C:\Program Files (x86)\LLVM\bin\{}", exe_name));
        if let Ok(home) = std::env::var("USERPROFILE") {
            candidates.push(format!(r"{}\.iris\toolchain\llvm\bin\{}", home, exe_name));
        }
        candidates.push(format!("/c/Program Files/LLVM/bin/{}", exe_name));
    }

    #[cfg(not(target_os = "windows"))]
    {
        candidates.push(format!(
            "/usr/local/share/iris/toolchain/llvm/bin/{}",
            exe_name
        ));
        candidates.push(format!("/opt/homebrew/opt/llvm/bin/{}", exe_name));
        candidates.push(format!("/usr/local/opt/llvm/bin/{}", exe_name));
        candidates.push(format!("/usr/lib/llvm-23/bin/{}", exe_name));
        candidates.push(format!("/usr/lib/llvm-22/bin/{}", exe_name));
        candidates.push(format!("/usr/lib/llvm-21/bin/{}", exe_name));
        candidates.push(format!("/usr/bin/{}", exe_name));
        if let Ok(home) = std::env::var("HOME") {
            candidates.push(format!("{}/.iris/toolchain/llvm/bin/{}", home, exe_name));
        }
    }

    for p in &candidates {
        if std::path::Path::new(p).exists() {
            return Some(p.clone());
        }
    }
    None
}

/// Link object files into a native binary using `ld.lld` directly (no clang).
/// This is a best-effort function: if linking fails, the caller falls back to clang.
#[allow(clippy::too_many_arguments)]
fn link_with_lld(
    lld_path: &str,
    target_triple: &str,
    main_obj: &Path,
    support_objs: &[PathBuf],
    output: &Path,
    _target_args: &[String],
    msys2_lib: &Option<String>,
    gcc_lib: &Option<String>,
    _onnx_sdk: &Option<PathBuf>,
    _tf_sdk: &Option<PathBuf>,
    _libtorch_sdk: &Option<PathBuf>,
    openblas_dir: &Option<PathBuf>,
    use_blas: bool,
    link_libs: &[String],
    opts: &NativeBuildOptions,
) -> Result<(), CodegenError> {
    let mut cmd = std::process::Command::new(lld_path);

    // Target-specific linking
    if target_triple.contains("windows") && !target_triple.contains("msvc") {
        // MinGW uses GNU-driver syntax. Startup and termination objects are
        // order-sensitive: crt2/crtbegin precede user objects and crtend is last.
        cmd.args(["-m", "i386pep", "-Bdynamic"]);
        if opts.gui_subsystem {
            cmd.args(["--subsystem", "windows", "-e", "mainCRTStartup"]);
        }
        cmd.arg("-o").arg(path_str(output)?);

        if msys2_lib.is_none() || gcc_lib.is_none() {
            return Err(CodegenError::Unsupported {
                backend: "binary".into(),
                detail: "direct MinGW linking requires the MSYS2 UCRT64 libraries and GCC runtime"
                    .into(),
            });
        }

        if let Some(ref lib) = msys2_lib {
            cmd.arg(format!("-L{}", lib));
            if let Some(root) = Path::new(lib).parent() {
                cmd.arg(format!(
                    "-L{}",
                    root.join("x86_64-w64-mingw32").join("lib").display()
                ));
                cmd.arg(format!(
                    "-L{}",
                    root.join("x86_64-w64-mingw32")
                        .join("mingw")
                        .join("lib")
                        .display()
                ));
            }
        }
        if let Some(ref lib) = gcc_lib {
            cmd.arg(format!("-L{}", lib));
        }

        if let Some(ref lib) = msys2_lib {
            let crt2 = Path::new(lib).join("crt2.o");
            if !crt2.exists() {
                return Err(CodegenError::Unsupported {
                    backend: "binary".into(),
                    detail: format!("MinGW startup object '{}' is missing", crt2.display()),
                });
            }
            cmd.arg(path_str(&crt2)?);
        }
        if let Some(ref gcc) = gcc_lib {
            let crt_begin = Path::new(gcc).join("crtbegin.o");
            if !crt_begin.exists() {
                return Err(CodegenError::Unsupported {
                    backend: "binary".into(),
                    detail: format!("MinGW startup object '{}' is missing", crt_begin.display()),
                });
            }
            cmd.arg(path_str(&crt_begin)?);
        }

        cmd.arg(path_str(main_obj)?);
        for obj in support_objs {
            cmd.arg(path_str(obj)?);
        }
        for obj in &opts.extra_objects {
            cmd.arg(path_str(obj)?);
        }
        append_optional_lld_libraries(&mut cmd, openblas_dir, use_blas, link_libs);
        append_mingw_default_libraries(&mut cmd, msys2_lib);

        if let Some(ref gcc) = gcc_lib {
            let crt_end = Path::new(gcc).join("crtend.o");
            if !crt_end.exists() {
                return Err(CodegenError::Unsupported {
                    backend: "binary".into(),
                    detail: format!(
                        "MinGW termination object '{}' is missing",
                        crt_end.display()
                    ),
                });
            }
            cmd.arg(path_str(&crt_end)?);
        }
    } else if target_triple.contains("msvc") {
        cmd.arg(path_str(main_obj)?);
        for obj in support_objs {
            cmd.arg(path_str(obj)?);
        }
        for obj in &opts.extra_objects {
            cmd.arg(path_str(obj)?);
        }
        cmd.arg("-o").arg(path_str(output)?);
        if opts.gui_subsystem {
            cmd.args(["/SUBSYSTEM:WINDOWS", "/ENTRY:mainCRTStartup"]);
        }
        // MSVC target — use lld-link compatible flags
        cmd.arg("/defaultlib:libcmt");
        cmd.arg("/defaultlib:oldnames");
    } else {
        cmd.arg(path_str(main_obj)?);
        for obj in support_objs {
            cmd.arg(path_str(obj)?);
        }
        for obj in &opts.extra_objects {
            cmd.arg(path_str(obj)?);
        }
        cmd.arg("-o").arg(path_str(output)?);
        for rpath in &opts.rpaths {
            cmd.args(["-rpath", rpath]);
        }
        // Linux/macOS: standard system libraries
        cmd.arg("-lm");
        cmd.arg("-lpthread");
        append_optional_lld_libraries(&mut cmd, openblas_dir, use_blas, link_libs);
    }

    let output_result = cmd.output().map_err(|e| CodegenError::Unsupported {
        backend: "binary".into(),
        detail: format!("ld.lld could not start: {}", e),
    })?;

    if !output_result.status.success() {
        let stderr = String::from_utf8_lossy(&output_result.stderr);
        return Err(CodegenError::Unsupported {
            backend: "binary".into(),
            detail: format!("ld.lld failed: {}", stderr),
        });
    }

    if !output.is_file() || std::fs::metadata(output).map(|m| m.len()).unwrap_or(0) == 0 {
        return Err(CodegenError::Unsupported {
            backend: "binary".into(),
            detail: format!(
                "ld.lld reported success but did not create a non-empty binary at '{}'",
                output.display()
            ),
        });
    }

    Ok(())
}

fn is_direct_link_input(lib: &str) -> bool {
    lib.contains('/')
        || lib.contains('\\')
        || lib.ends_with(".a")
        || lib.ends_with(".lib")
        || lib.ends_with(".o")
        || lib.ends_with(".obj")
        || lib.ends_with(".so")
        || lib.ends_with(".dylib")
}

fn append_link_lib_arg(cmd: &mut std::process::Command, lib: &str) {
    if is_direct_link_input(lib) {
        cmd.arg(lib);
    } else {
        cmd.arg(format!("-l{}", lib));
    }
}

fn append_optional_lld_libraries(
    cmd: &mut std::process::Command,
    openblas_dir: &Option<PathBuf>,
    use_blas: bool,
    link_libs: &[String],
) {
    if use_blas {
        if let Some(dir) = openblas_dir {
            cmd.arg(format!("-L{}", dir.join("lib").display()));
        }
        cmd.arg("-lopenblas");
    }
    let mut seen_libs = std::collections::HashSet::new();
    for lib in link_libs {
        if seen_libs.insert(lib.as_str()) {
            append_link_lib_arg(cmd, lib);
        }
    }
}

fn append_mingw_default_libraries(cmd: &mut std::process::Command, msys2_lib: &Option<String>) {
    let has_ucrt = msys2_lib
        .as_ref()
        .map(|lib| Path::new(lib).join("libucrt.a").is_file())
        .unwrap_or(false);
    let crt_lib = if has_ucrt { "ucrt" } else { "msvcrt" };
    // Link winpthread statically when its archive is present. A dynamic
    // `libwinpthread-1.dll` dependency means the binary only starts where
    // MSYS2 is on PATH -- and never inside a zero-capability AppContainer,
    // which cannot read that directory (STATUS_DLL_NOT_FOUND).
    let static_pthread = msys2_lib
        .as_ref()
        .map(|lib| Path::new(lib).join("libwinpthread.a").is_file())
        .unwrap_or(false);
    let pthread = if static_pthread {
        ":libwinpthread.a"
    } else {
        "pthread"
    };
    // The second MinGW/GCC group resolves dependencies introduced by the
    // Windows import libraries in the first group, matching clang's driver.
    for lib in [
        "mingw32", "gcc", "gcc_eh", "moldname", "mingwex", crt_lib, "advapi32", "shell32",
        "user32", "kernel32", "ws2_32", "winhttp", pthread, "m", "mingw32", "gcc", "gcc_eh",
        "moldname", "mingwex", crt_lib, "kernel32",
    ] {
        cmd.arg(format!("-l{}", lib));
    }
}

/// Return the MinGW ucrt64 include path if it exists.
/// Windows-only: needed for cross-compiling to the windows-gnu target.
/// On Linux/macOS, system headers are used via clang's built-in paths.
pub(crate) fn msys2_ucrt64_include() -> Option<String> {
    #[cfg(not(target_os = "windows"))]
    {
        None
    }

    #[cfg(target_os = "windows")]
    {
        let mut candidates: Vec<String> = Vec::new();

        for prefix in install_prefix_dirs() {
            candidates.push(
                prefix
                    .join("toolchain")
                    .join("ucrt64")
                    .join("include")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        candidates.push(r"C:\msys64\ucrt64\include".into());
        if let Ok(home) = std::env::var("USERPROFILE") {
            candidates.push(format!(r"{}\.iris\ucrt64\include", home));
        }
        candidates.push("/c/msys64/ucrt64/include".into());

        for p in &candidates {
            if std::path::Path::new(p.as_str()).exists() {
                return Some(p.clone());
            }
        }
        None
    }
}

/// Return the MinGW ucrt64 lib path if it exists (Windows-only).
pub(crate) fn msys2_ucrt64_lib() -> Option<String> {
    #[cfg(not(target_os = "windows"))]
    {
        None
    }

    #[cfg(target_os = "windows")]
    {
        let mut candidates: Vec<String> = Vec::new();

        for prefix in install_prefix_dirs() {
            candidates.push(
                prefix
                    .join("toolchain")
                    .join("ucrt64")
                    .join("lib")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        candidates.push(r"C:\msys64\ucrt64\lib".into());
        if let Ok(home) = std::env::var("USERPROFILE") {
            candidates.push(format!(r"{}\.iris\ucrt64\lib", home));
        }
        candidates.push("/c/msys64/ucrt64/lib".into());

        for p in &candidates {
            if std::path::Path::new(p.as_str()).exists() {
                return Some(p.clone());
            }
        }
        None
    }
}

/// Return the GCC internal lib path (contains CRT start files like crtbegin.o,
/// libgcc.a) inside the MinGW ucrt64 tree (Windows-only).
pub(crate) fn msys2_gcc_lib() -> Option<String> {
    #[cfg(not(target_os = "windows"))]
    {
        None
    }

    #[cfg(target_os = "windows")]
    {
        let triple = "x86_64-w64-mingw32";
        let mut base_dirs: Vec<String> = Vec::new();

        for prefix in install_prefix_dirs() {
            base_dirs.push(
                prefix
                    .join("toolchain")
                    .join("ucrt64")
                    .join("lib")
                    .join("gcc")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        // System MSYS2
        base_dirs.push(r"C:\msys64\ucrt64\lib\gcc".into());
        // Legacy user-local
        if let Ok(home) = std::env::var("USERPROFILE") {
            base_dirs.push(format!(r"{}\.iris\ucrt64\lib\gcc", home));
        }
        base_dirs.push("/c/msys64/ucrt64/lib/gcc".into());

        for base in &base_dirs {
            let version_root = Path::new(base).join(triple);
            let Ok(entries) = std::fs::read_dir(&version_root) else {
                continue;
            };
            let mut versions: Vec<PathBuf> = entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.is_dir() && path.join("crtbegin.o").exists())
                .collect();
            versions.sort_by_key(|path| mingw_gcc_version_key(path));
            if let Some(path) = versions.into_iter().next_back() {
                return Some(path.to_string_lossy().into_owned());
            }
        }
        None
    }
}

#[cfg(any(target_os = "windows", test))]
fn mingw_gcc_version_key(path: &Path) -> Vec<u32> {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .split('.')
        .map(|part| part.parse::<u32>().unwrap_or(0))
        .collect()
}

/// Return a persistent cache directory for compiled C runtime objects.
/// Prefers a directory next to the compiler binary; falls back to ~/.iris/cache/.
fn runtime_cache_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("iris_cache");
            if candidate.exists() || std::fs::create_dir_all(&candidate).is_ok() {
                return candidate;
            }
        }
    }
    if let Ok(home) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
        let candidate = PathBuf::from(home).join(".iris").join("cache");
        if candidate.exists() || std::fs::create_dir_all(&candidate).is_ok() {
            return candidate;
        }
    }
    std::env::temp_dir().join("iris_cache")
}

/// Name a cached runtime object by both target and exact embedded C sources.
///
/// The former `iris_runtime.o` cache was shared by every target and considered
/// valid solely from filesystem mtimes. Restored CI caches could therefore
/// feed an older or wrong-architecture object into a native link. The source
/// hash is generated by the same build script that validates prebuilts.
fn runtime_cache_object_name(target: &str) -> String {
    let target = target
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect::<String>();
    format!("iris_runtime_{target}_{RUNTIME_SOURCES_HASH}.o")
}

/// Build the optional LibTorch plugin (`iris_torch_plugin`).
///
/// LibTorch exposes a C++ API, so unlike ONNX/TensorFlow it cannot be `dlopen`ed
/// directly from the runtime. Instead `pytorch_shim.cpp` — whose entry points are
/// already `extern "C"` — is compiled into a standalone shared library that links
/// LibTorch, and `iris_torch_available()` in the runtime resolves its four
/// symbols at first use.
///
/// This keeps `iris_runtime.o` free of any LibTorch reference, which is what
/// allows a single prebuilt runtime object to be shipped per target triple.
///
/// `libtorch_dir` defaults to `$LIBTORCH_DIR`, then `C:\libtorch`.
/// Returns the path to the produced shared library.
pub fn build_torch_plugin(
    libtorch_dir: Option<&Path>,
    output_dir: &Path,
) -> Result<PathBuf, CodegenError> {
    let sdk: PathBuf = match libtorch_dir {
        Some(p) => p.to_path_buf(),
        None => std::env::var("LIBTORCH_DIR")
            .map(PathBuf::from)
            .ok()
            .or_else(|| {
                let d = PathBuf::from(r"C:\libtorch");
                d.exists().then_some(d)
            })
            .ok_or_else(|| CodegenError::Unsupported {
                backend: "torch-plugin".into(),
                detail: "LibTorch not found — set LIBTORCH_DIR to your install".into(),
            })?,
    };

    if !sdk.join("include").is_dir() {
        return Err(CodegenError::Unsupported {
            backend: "torch-plugin".into(),
            detail: format!(
                "'{}' does not look like a LibTorch install (no include/)",
                sdk.display()
            ),
        });
    }

    std::fs::create_dir_all(output_dir).map_err(CodegenError::Io)?;

    // Stage the sources the plugin needs: the shim itself plus the runtime
    // header it includes for IrisTensor.
    let src = output_dir.join("pytorch_shim.cpp");
    std::fs::write(&src, PYTORCH_SHIM_CPP_SRC).map_err(CodegenError::Io)?;
    std::fs::write(output_dir.join("iris_runtime.h"), RUNTIME_H_SRC).map_err(CodegenError::Io)?;

    let lib_name = if cfg!(target_os = "windows") {
        "iris_torch_plugin.dll"
    } else if cfg!(target_os = "macos") {
        "libiris_torch_plugin.dylib"
    } else {
        "libiris_torch_plugin.so"
    };
    let out = output_dir.join(lib_name);

    let clang = find_clang();
    let mut cmd = Command::new(&clang);
    cmd.args(["-shared", "-O2", "-x", "c++", "-std=c++17"]);
    cmd.arg("-DLIBTORCH_ENABLED");
    cmd.arg(path_str(&src)?);
    cmd.args(["-o", path_str(&out)?]);
    cmd.arg("-I").arg(output_dir);
    cmd.arg("-I").arg(sdk.join("include"));
    cmd.arg("-I")
        .arg(sdk.join("include/torch/csrc/api/include"));
    cmd.arg(format!("-L{}", sdk.join("lib").display()));
    cmd.args(["-ltorch", "-ltorch_cpu", "-lc10"]);
    if !cfg!(target_os = "windows") {
        cmd.arg("-lstdc++");
    }

    let output = cmd.output().map_err(|e| CodegenError::Unsupported {
        backend: "torch-plugin".into(),
        detail: format!("'{}' could not start: {}", clang, e),
    })?;
    if !output.status.success() {
        return Err(CodegenError::Unsupported {
            backend: "torch-plugin".into(),
            detail: format!(
                "failed to build LibTorch plugin (exit {:?})\n{}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            ),
        });
    }

    Ok(out)
}

/// Emit LLVM IR text suitable for native binary compilation.
///
/// This is identical to `emit_llvm_ir` but provides a clear name for the
/// binary code-generation path.
pub fn emit_binary_ir(module: &IrModule) -> Result<String, CodegenError> {
    crate::codegen::llvm_ir::emit_llvm_ir(module)
}

/// Returns the embedded C runtime source as a static string.
///
/// Useful for writing the runtime to disk in build scripts or tests.
pub fn runtime_c_source() -> &'static str {
    RUNTIME_C_SRC
}

/// Returns the embedded C runtime header as a static string.
pub fn runtime_h_source() -> &'static str {
    RUNTIME_H_SRC
}

/// Compile `llvm_ir` (from `emit_llvm_ir_for_library`), C sources and the
/// runtime into a shared library for the native target: a DLL (`.pyd`) on
/// Windows, `.so` / `.dylib` elsewhere. `c_sources` are (file name, text)
/// pairs compiled with `include_dirs` and `defines` beside the runtime's own
/// headers; `link_args` are added to the link.
pub(crate) fn build_shared_library(
    llvm_ir: &str,
    c_sources: &[(&str, &str)],
    include_dirs: &[PathBuf],
    defines: &[String],
    link_args: &[String],
    output: &Path,
) -> Result<PathBuf, CodegenError> {
    build_native_artifact(
        llvm_ir,
        c_sources,
        include_dirs,
        defines,
        link_args,
        output,
        true,
    )
}

/// `build_shared_library`, or, when not `shared`, an executable whose `main`
/// one of `c_sources` defines.
pub(crate) fn build_native_artifact(
    llvm_ir: &str,
    c_sources: &[(&str, &str)],
    include_dirs: &[PathBuf],
    defines: &[String],
    link_args: &[String],
    output: &Path,
    shared: bool,
) -> Result<PathBuf, CodegenError> {
    let fail = |detail: String| CodegenError::Unsupported {
        backend: "shared library".into(),
        detail,
    };
    let output = if output.is_relative() {
        std::env::current_dir()
            .map(|cwd| cwd.join(output))
            .unwrap_or_else(|_| output.to_path_buf())
    } else {
        output.to_path_buf()
    };
    let target = resolve_target_triple(None);
    let windows_gnu = target.contains("windows") && !target.contains("msvc");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!("iris_shlib_{}_{}", std::process::id(), stamp));
    std::fs::create_dir_all(&tmp)
        .map_err(|e| fail(format!("cannot create {}: {e}", tmp.display())))?;
    let _guard = BuildDirGuard(tmp.clone());

    let mut files: Vec<(&str, &str)> = vec![
        ("iris_runtime.h", RUNTIME_H_SRC),
        ("iris_runtime.c", RUNTIME_C_SRC),
        ("iris_ml_dynload.h", ML_DYNLOAD_H_SRC),
        ("onnxruntime_c_api.h", ONNXRUNTIME_C_API_H_SRC),
        ("onnx_shim.h", ONNX_SHIM_H_SRC),
        ("onnx_shim.c", ONNX_SHIM_C_SRC),
        ("tf_shim.c", TF_SHIM_C_SRC),
        ("iris_ml_kernels.h", ML_KERNELS_H_SRC),
        ("iris_ml_kernels.c", ML_KERNELS_C_SRC),
    ];
    files.extend_from_slice(c_sources);
    for (name, text) in &files {
        std::fs::write(tmp.join(name), text)
            .map_err(|e| fail(format!("cannot write {name}: {e}")))?;
    }
    let ll = tmp.join("module.ll");
    std::fs::write(&ll, llvm_ir).map_err(|e| fail(format!("cannot write module.ll: {e}")))?;

    let clang = find_clang();
    let mut target_args = vec!["-target".to_owned(), target.clone()];
    if target.contains("apple") {
        if let Some(sdk) = macos_sdk_path() {
            target_args.push("-isysroot".to_owned());
            target_args.push(sdk);
        }
    }
    let pic = !target.contains("windows");
    let x86 = target.contains("x86_64") && std::env::var("IRIS_NO_AVX2").is_err();
    let run = |cmd: &mut Command, what: &str| -> Result<(), CodegenError> {
        let out = cmd.output().map_err(|e| CodegenError::ToolchainMissing {
            detail: format!("the C compiler '{clang}' could not be started for {what} ({e})"),
        })?;
        if !out.status.success() {
            return Err(fail(format!(
                "{what} failed:\n{}",
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        Ok(())
    };

    let mut objects = Vec::new();
    for (name, _) in files.iter().filter(|(n, _)| n.ends_with(".c")) {
        let obj = tmp.join(format!("{name}.o"));
        let mut cmd = Command::new(&clang);
        cmd.args(&target_args)
            .args([
                "-O2",
                "-c",
                "-Wno-pragma-pack",
                "-Wno-deprecated-declarations",
            ])
            .arg(tmp.join(name))
            .arg("-o")
            .arg(&obj)
            .arg("-I")
            .arg(&tmp);
        if pic {
            cmd.arg("-fPIC");
        }
        if windows_gnu {
            if let Some(inc) = msys2_ucrt64_include() {
                cmd.arg("-I").arg(inc);
            }
        }
        if *name == "iris_ml_kernels.c" && x86 {
            cmd.args(["-mavx2", "-mfma"]);
        }
        for dir in include_dirs {
            cmd.arg("-I").arg(dir);
        }
        cmd.args(defines);
        run(&mut cmd, &format!("compiling {name}"))?;
        objects.push(obj);
    }
    let module_obj = tmp.join("module.o");
    let mut cmd = Command::new(&clang);
    cmd.args(&target_args)
        .args(["-O2", "-c", "-Wno-override-module"])
        .arg(&ll)
        .arg("-o")
        .arg(&module_obj);
    if pic {
        cmd.arg("-fPIC");
    }
    run(&mut cmd, "compiling the IRIS module")?;
    objects.insert(0, module_obj);

    if windows_gnu {
        // A DLL: MinGW's DLL start-up object and entry point.
        let lld = find_lld().ok_or_else(|| CodegenError::ToolchainMissing {
            detail: "ld.lld, to link a DLL".into(),
        })?;
        let msys2_lib = msys2_ucrt64_lib();
        let gcc_lib = msys2_gcc_lib();
        let (Some(lib), Some(gcc)) = (&msys2_lib, &gcc_lib) else {
            return Err(CodegenError::ToolchainMissing {
                detail: "the MSYS2 UCRT64 libraries and GCC runtime, to link a DLL".into(),
            });
        };
        let mut cmd = Command::new(&lld);
        if shared {
            cmd.args([
                "-m",
                "i386pep",
                "--shared",
                "-Bdynamic",
                "-e",
                "DllMainCRTStartup",
            ]);
        } else {
            cmd.args(["-m", "i386pep", "-Bdynamic"]);
        }
        cmd.arg("-o").arg(&output);
        cmd.arg(format!("-L{lib}"));
        if let Some(root) = Path::new(lib).parent() {
            cmd.arg(format!(
                "-L{}",
                root.join("x86_64-w64-mingw32").join("lib").display()
            ));
        }
        cmd.arg(format!("-L{gcc}"));
        cmd.arg(Path::new(lib).join(if shared { "dllcrt2.o" } else { "crt2.o" }));
        cmd.arg(Path::new(gcc).join("crtbegin.o"));
        cmd.args(&objects);
        cmd.args(link_args);
        append_mingw_default_libraries(&mut cmd, &msys2_lib);
        cmd.arg(Path::new(gcc).join("crtend.o"));
        let out = cmd
            .output()
            .map_err(|e| fail(format!("ld.lld could not start: {e}")))?;
        if !out.status.success() {
            return Err(fail(format!(
                "linking the DLL failed:\n{}",
                String::from_utf8_lossy(&out.stderr)
            )));
        }
    } else {
        let mut cmd = Command::new(&clang);
        cmd.args(&target_args);
        if shared {
            cmd.arg("-shared");
        }
        if !target.contains("apple") {
            cmd.arg("-fuse-ld=lld");
        }
        cmd.args(&objects).arg("-o").arg(&output).args(link_args);
        if !target.contains("apple") {
            cmd.args(["-lm", "-lpthread"]);
        }
        run(&mut cmd, "linking the shared library")?;
    }
    Ok(output)
}

fn module_obj_cache_key(
    llvm_ir: &str,
    target: &str,
    opt_level: u8,
    debug: bool,
    sanitize: Option<&str>,
) -> String {
    let input = format!(
        "target={target};opt={opt_level};debug={debug};san={};\n{llvm_ir}",
        sanitize.unwrap_or("")
    );
    crate::cache::sha256_hex(input.as_bytes())
}

fn module_obj_cache_path(
    llvm_ir: &str,
    target: &str,
    opt_level: u8,
    debug: bool,
    sanitize: Option<&str>,
) -> Option<PathBuf> {
    if std::env::var_os("IRIS_NO_CACHE").is_some() {
        return None;
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let key = module_obj_cache_key(llvm_ir, target, opt_level, debug, sanitize);
    Some(
        PathBuf::from(home)
            .join(".iris")
            .join("cache")
            .join("objs")
            .join(format!("{key}.o")),
    )
}

#[cfg(test)]
mod direct_link_tests {
    use super::{mingw_gcc_version_key, runtime_cache_object_name, RUNTIME_SOURCES_HASH};
    use std::path::Path;

    #[test]
    fn gcc_version_discovery_uses_numeric_order() {
        assert!(
            mingw_gcc_version_key(Path::new("15.2.0")) > mingw_gcc_version_key(Path::new("9.4.0"))
        );
        assert!(
            mingw_gcc_version_key(Path::new("14.10.1"))
                > mingw_gcc_version_key(Path::new("14.2.0"))
        );
    }

    #[test]
    fn runtime_cache_is_target_and_source_specific() {
        let windows = runtime_cache_object_name("x86_64-pc-windows-gnu");
        let linux = runtime_cache_object_name("x86_64-unknown-linux-gnu");
        assert_ne!(windows, linux);
        assert!(windows.contains(RUNTIME_SOURCES_HASH));
        assert!(!windows.contains('-'));
    }

    #[test]
    fn ml_link_mode_defaults_to_dynamic_and_supports_static_opt_in() {
        use super::MlLinkMode;
        assert_eq!(MlLinkMode::default(), MlLinkMode::DynamicDlopen);
        assert_eq!(MlLinkMode::from_flag(None), MlLinkMode::DynamicDlopen);
        assert_eq!(MlLinkMode::from_flag(Some("0")), MlLinkMode::DynamicDlopen);
        assert_eq!(MlLinkMode::from_flag(Some("1")), MlLinkMode::StaticDirect);
        assert_eq!(
            MlLinkMode::from_flag(Some("true")),
            MlLinkMode::StaticDirect
        );
    }

    #[test]
    fn module_obj_cache_key_varies_by_ir_target_and_opt_level() {
        use super::module_obj_cache_key;
        let k1 = module_obj_cache_key(
            "define i64 @main() { ret i64 0 }",
            "x86_64-pc-windows-gnu",
            3,
            false,
            None,
        );
        let k2 = module_obj_cache_key(
            "define i64 @main() { ret i64 1 }",
            "x86_64-pc-windows-gnu",
            3,
            false,
            None,
        );
        let k3 = module_obj_cache_key(
            "define i64 @main() { ret i64 0 }",
            "aarch64-unknown-linux-gnu",
            3,
            false,
            None,
        );
        let k4 = module_obj_cache_key(
            "define i64 @main() { ret i64 0 }",
            "x86_64-pc-windows-gnu",
            0,
            false,
            None,
        );
        assert_ne!(k1, k2);
        assert_ne!(k1, k3);
        assert_ne!(k1, k4);
        assert_eq!(
            k1,
            module_obj_cache_key(
                "define i64 @main() { ret i64 0 }",
                "x86_64-pc-windows-gnu",
                3,
                false,
                None
            )
        );
    }
}
