//! macOS worker sandbox: a deny-by-default Seatbelt profile applied by
//! `/usr/bin/sandbox-exec`, plus rlimits and a sampling supervisor.
//!
//! * **Capabilities (Seatbelt):** everything is denied except reading the
//!   system libraries and the dyld shared cache, reading and writing the
//!   worker's own scratch directory, executing the staged worker binary, and
//!   reading sysctls. Network access, process creation and every other file
//!   are denied. `sandbox-exec` execs the worker in place, so the policy is
//!   in force before the worker's first instruction.
//! * **Resources:** `RLIMIT_CPU`, `RLIMIT_FSIZE`, `RLIMIT_NOFILE` and
//!   `RLIMIT_CORE` (CPU time is therefore enforced at one-second
//!   granularity). macOS does not enforce `RLIMIT_AS`, so the supervisor
//!   samples resident memory with `proc_pidinfo` and kills the worker on a
//!   violation, alongside the wall-time and output checks.
//!
//! `sandbox-exec` is deprecated by Apple but present on every current macOS;
//! if it is missing the backend reports itself unavailable (fail closed).

use super::unix::{apply_common_limits, platform_error, supervise, Sample, WorkDir};
use super::{CommandSpec, ResourceLimits, SandboxAvailability, SandboxError, SandboxRunOutput};

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

pub(super) fn detect() -> SandboxAvailability {
    if Path::new(SANDBOX_EXEC).is_file() {
        SandboxAvailability::Enforced {
            capability_isolation: "macOS Seatbelt (deny-default profile via sandbox-exec)",
            resource_limits: "rlimits + sampling supervisor + bounded pipes",
        }
    } else {
        SandboxAvailability::Unavailable {
            reason: format!("{SANDBOX_EXEC} is not present on this system"),
        }
    }
}

pub(super) fn current_process_is_appcontainer() -> Result<bool, SandboxError> {
    Ok(false)
}

/// Quote a path as a Seatbelt (TinyScheme) string literal.
fn sb_string(path: &Path) -> String {
    let text = path.to_string_lossy();
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The worker's policy: deny everything except what a statically known,
/// dynamically linked command-line program needs to start and run.
fn profile(work: &WorkDir) -> String {
    let dir = sb_string(&work.path);
    let worker = sb_string(&work.worker);
    format!(
        r#"(version 1)
(deny default)
(allow process-exec (literal {worker}))
(allow file-read-metadata)
(allow file-read*
    (literal "/")
    (subpath "/usr/lib")
    (subpath "/usr/share")
    (subpath "/System/Library")
    (subpath "/System/Volumes/Preboot/Cryptexes")
    (subpath "/Library/Apple/System")
    (subpath "/private/var/db/dyld")
    (subpath "/private/var/db/timezone")
    (literal "/dev/null")
    (literal "/dev/urandom")
    (literal "/dev/random")
    (subpath {dir}))
(allow file-write*
    (literal "/dev/null")
    (subpath {dir}))
(allow sysctl-read)
(allow signal (target self))
(allow process-info* (target self))
"#
    )
}

pub(super) fn run(
    command: &CommandSpec,
    limits: ResourceLimits,
) -> Result<SandboxRunOutput, SandboxError> {
    limits.validate().map_err(SandboxError::InvalidLimits)?;
    if !Path::new(SANDBOX_EXEC).is_file() {
        return Err(SandboxError::Unavailable(format!(
            "{SANDBOX_EXEC} is not present on this system"
        )));
    }
    let work = WorkDir::stage(command.executable())?;
    let policy = profile(&work);

    let mut process = Command::new(SANDBOX_EXEC);
    process
        .arg("-p")
        .arg(&policy)
        .arg(&work.worker)
        .args(command.arguments())
        .current_dir(&work.path)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: runs in the forked child before exec; only setrlimit calls.
    unsafe {
        process.pre_exec(move || apply_common_limits(&limits));
    }
    let started = Instant::now();
    let child = process
        .spawn()
        .map_err(|error| platform_error("start sandboxed worker", error))?;
    let result = supervise(child, limits, started, task_sample);
    drop(work);
    result
}

#[repr(C)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

extern "C" {
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
}

/// CPU time (via `mach_timebase_info` scaling Mach ticks) and resident memory
/// of `pid` from `proc_pidinfo`. Enables sub-second polling enforcement in
/// `unix::supervise`, supplementing the whole-second `RLIMIT_CPU` kernel backstop.
fn task_sample(pid: libc::pid_t) -> Sample {
    // SAFETY: proc_taskinfo is plain data filled in by the kernel.
    let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
    // SAFETY: `info` is a valid buffer of `size` bytes.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTASKINFO,
            0,
            &mut info as *mut libc::proc_taskinfo as *mut libc::c_void,
            size,
        )
    };
    if read != size {
        return Sample {
            cpu_ms: None,
            resident_bytes: None,
        };
    }
    let mut timebase = MachTimebaseInfo { numer: 1, denom: 1 };
    // SAFETY: `timebase` is plain old data populated by libSystem.
    let ok = unsafe { mach_timebase_info(&mut timebase) };
    let cpu_ms = if ok == 0 && timebase.denom > 0 {
        let total_ticks =
            (info.pti_total_user as u128).saturating_add(info.pti_total_system as u128);
        let ns = total_ticks.saturating_mul(timebase.numer as u128) / (timebase.denom as u128);
        Some((ns / 1_000_000) as u64)
    } else {
        None
    };
    Sample {
        cpu_ms,
        resident_bytes: Some(info.pti_resident_size),
    }
}
