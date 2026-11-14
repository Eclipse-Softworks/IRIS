//! Supervision shared by the Unix sandbox backends: staging the worker in a
//! private scratch directory, resource limits applied before `exec`, bounded
//! output capture, and the wall/CPU/memory/output enforcement loop.

use super::{LimitViolation, ResourceLimits, SandboxError, SandboxRunOutput};

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(5);

pub(super) fn platform_error(context: &str, error: impl std::fmt::Display) -> SandboxError {
    SandboxError::Platform(format!("{context}: {error}"))
}

/// A private scratch directory holding the staged worker, removed on drop.
pub(super) struct WorkDir {
    pub path: PathBuf,
    pub worker: PathBuf,
}

impl WorkDir {
    /// Create a 0700 directory under the system temp directory and copy the
    /// worker into it as the only executable the sandbox will run.
    pub fn stage(executable: &Path) -> Result<Self, SandboxError> {
        let source = executable
            .canonicalize()
            .map_err(|error| platform_error("canonicalize worker executable", error))?;
        if !source.is_file() {
            return Err(SandboxError::InvalidCommand(
                "worker executable is not a regular file".into(),
            ));
        }
        static NEXT_RUN: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "iris-sandbox-{}-{}",
            std::process::id(),
            NEXT_RUN.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).map_err(|e| platform_error("create sandbox directory", e))?;
        // Canonical so that path-based policies (Landlock rules, Seatbelt
        // subpaths) match what the kernel sees, e.g. /private/var on macOS.
        let path = path
            .canonicalize()
            .map_err(|e| platform_error("canonicalize sandbox directory", e))?;
        let dir = Self {
            worker: path.join("iris-sandbox-worker"),
            path,
        };
        std::fs::set_permissions(&dir.path, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| platform_error("restrict sandbox directory", e))?;
        std::fs::copy(&source, &dir.worker).map_err(|e| platform_error("stage worker", e))?;
        std::fs::set_permissions(&dir.worker, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| platform_error("mark worker executable", e))?;
        Ok(dir)
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub(super) type Resource = libc::__rlimit_resource_t;
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub(super) type Resource = libc::c_int;

/// `setrlimit`, callable between `fork` and `exec` (no allocation).
pub(super) fn set_limit(resource: Resource, soft: u64, hard: u64) -> std::io::Result<()> {
    let limit = libc::rlimit {
        rlim_cur: soft as libc::rlim_t,
        rlim_max: hard as libc::rlim_t,
    };
    // SAFETY: `limit` is a valid rlimit structure.
    if unsafe { libc::setrlimit(resource, &limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// The limits every Unix backend applies in the child before `exec`.
/// `memory` is applied where the platform enforces `RLIMIT_AS` (Linux); the
/// supervisor enforces it by sampling elsewhere.
pub(super) fn apply_common_limits(limits: &ResourceLimits) -> std::io::Result<()> {
    let cpu_seconds = limits.cpu_time_ms.div_ceil(1000).max(1);
    set_limit(libc::RLIMIT_CPU, cpu_seconds, cpu_seconds + 1)?;
    set_limit(libc::RLIMIT_FSIZE, limits.output_bytes, limits.output_bytes)?;
    set_limit(libc::RLIMIT_NOFILE, 64, 64)?;
    set_limit(libc::RLIMIT_CORE, 0, 0)?;
    #[cfg(target_os = "linux")]
    set_limit(libc::RLIMIT_AS, limits.memory_bytes, limits.memory_bytes)?;
    Ok(())
}

/// Live usage of a running worker, sampled by the supervisor.
pub(super) struct Sample {
    pub cpu_ms: Option<u64>,
    pub resident_bytes: Option<u64>,
}

/// Run `child` to completion under `limits`, killing it on any violation.
pub(super) fn supervise(
    mut child: Child,
    limits: ResourceLimits,
    started: Instant,
    sample: impl Fn(libc::pid_t) -> Sample,
) -> Result<SandboxRunOutput, SandboxError> {
    let pid = child.id() as libc::pid_t;
    let total_output = Arc::new(AtomicU64::new(0));
    let output_exceeded = Arc::new(AtomicBool::new(false));
    let stdout_reader = spawn_reader(
        child.stdout.take().expect("piped stdout"),
        Arc::clone(&total_output),
        Arc::clone(&output_exceeded),
        limits.output_bytes,
    );
    let stderr_reader = spawn_reader(
        child.stderr.take().expect("piped stderr"),
        Arc::clone(&total_output),
        Arc::clone(&output_exceeded),
        limits.output_bytes,
    );

    let mut violation = None;
    let mut status: libc::c_int = 0;
    // SAFETY: rusage is plain data that wait4 fills in.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let mut peak_sampled = 0u64;
    loop {
        // SAFETY: `pid` is our unreaped child; status/usage are valid.
        let reaped = unsafe { libc::wait4(pid, &mut status, libc::WNOHANG, &mut usage) };
        if reaped == pid {
            break;
        }
        if reaped < 0 {
            let error = std::io::Error::last_os_error();
            let _ = child.kill();
            return Err(platform_error("wait for sandboxed worker", error));
        }
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let now = sample(pid);
        if let Some(resident) = now.resident_bytes {
            peak_sampled = peak_sampled.max(resident);
        }
        if elapsed_ms > limits.wall_time_ms {
            violation = Some(LimitViolation::WallTime {
                elapsed_ms,
                limit_ms: limits.wall_time_ms,
            });
        } else if output_exceeded.load(Ordering::Acquire) {
            violation = Some(LimitViolation::Output {
                observed_bytes: total_output.load(Ordering::Acquire),
                limit_bytes: limits.output_bytes,
            });
        } else if let Some(cpu_ms) = now.cpu_ms.filter(|&ms| ms > limits.cpu_time_ms) {
            violation = Some(LimitViolation::CpuTime {
                observed_ms: cpu_ms,
                limit_ms: limits.cpu_time_ms,
            });
        } else if let Some(resident) = now.resident_bytes.filter(|&b| b > limits.memory_bytes) {
            violation = Some(LimitViolation::Memory {
                observed_bytes: resident,
                limit_bytes: limits.memory_bytes,
            });
        }
        if violation.is_some() {
            let _ = child.kill();
            // SAFETY: as above; blocking reap after SIGKILL.
            unsafe { libc::wait4(pid, &mut status, 0, &mut usage) };
            break;
        }
        thread::sleep(POLL_INTERVAL);
    }

    let to_ms = |time: libc::timeval| time.tv_sec as u64 * 1000 + time.tv_usec as u64 / 1000;
    let cpu_time_ms = to_ms(usage.ru_utime) + to_ms(usage.ru_stime);
    // ru_maxrss is in KiB on Linux and in bytes on macOS.
    #[cfg(target_os = "linux")]
    let max_rss = usage.ru_maxrss.max(0) as u64 * 1024;
    #[cfg(not(target_os = "linux"))]
    let max_rss = usage.ru_maxrss.max(0) as u64;
    let peak_memory_bytes = max_rss.max(peak_sampled);
    let stdout = join_reader(stdout_reader, "stdout")?;
    let stderr = join_reader(stderr_reader, "stderr")?;
    let output_bytes = total_output.load(Ordering::Acquire);

    let signaled = libc::WIFSIGNALED(status);
    if violation.is_none() && signaled && libc::WTERMSIG(status) == libc::SIGXCPU {
        violation = Some(LimitViolation::CpuTime {
            observed_ms: cpu_time_ms,
            limit_ms: limits.cpu_time_ms,
        });
    }
    if violation.is_none() && signaled && libc::WTERMSIG(status) == libc::SIGXFSZ {
        violation = Some(LimitViolation::Output {
            observed_bytes: limits.output_bytes.saturating_add(1),
            limit_bytes: limits.output_bytes,
        });
    }
    if let Some(violation) = violation {
        return Err(SandboxError::LimitExceeded(violation));
    }
    if output_exceeded.load(Ordering::Acquire) {
        return Err(SandboxError::LimitExceeded(LimitViolation::Output {
            observed_bytes: output_bytes,
            limit_bytes: limits.output_bytes,
        }));
    }
    let exit_code = if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status) as u32
    } else {
        128 + libc::WTERMSIG(status) as u32
    };
    Ok(SandboxRunOutput {
        exit_code,
        stdout,
        stderr,
        wall_time_ms: started.elapsed().as_millis() as u64,
        cpu_time_ms,
        peak_memory_bytes,
        output_bytes,
    })
}

fn spawn_reader(
    mut stream: impl Read + Send + 'static,
    total: Arc<AtomicU64>,
    exceeded: Arc<AtomicBool>,
    limit: u64,
) -> JoinHandle<std::io::Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut captured = Vec::new();
        let mut chunk = [0_u8; 8 * 1024];
        loop {
            let read = stream.read(&mut chunk)?;
            if read == 0 {
                break;
            }
            let read_u64 = read as u64;
            let previous = total.fetch_add(read_u64, Ordering::AcqRel);
            let keep = limit.saturating_sub(previous).min(read_u64) as usize;
            captured.extend_from_slice(&chunk[..keep]);
            if previous.saturating_add(read_u64) > limit {
                exceeded.store(true, Ordering::Release);
            }
        }
        Ok(captured)
    })
}

fn join_reader(
    reader: JoinHandle<std::io::Result<Vec<u8>>>,
    stream: &str,
) -> Result<Vec<u8>, SandboxError> {
    reader
        .join()
        .map_err(|_| SandboxError::Platform(format!("sandbox {stream} reader panicked")))?
        .map_err(|error| platform_error(&format!("read sandbox {stream}"), error))
}
