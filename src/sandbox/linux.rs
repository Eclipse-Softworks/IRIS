//! Linux worker sandbox: Landlock + seccomp + rlimits, applied in the child
//! between `fork` and `exec`, so no candidate instruction runs unconfined.
//!
//! * **Filesystem (Landlock):** the worker may read system library and
//!   configuration directories, read and write its own scratch directory, and
//!   execute only its own staged binary and the ELF interpreter. Everything
//!   else -- the user's files, other binaries -- is inaccessible. On ABI v4+
//!   kernels TCP bind/connect are denied by Landlock as well.
//! * **System calls (seccomp-bpf):** sockets, process creation (threads are
//!   allowed), signals to other processes, tracing, namespaces, mounts, module
//!   and kexec calls, io_uring and BPF are refused.
//! * **Resources:** `RLIMIT_AS` (memory), `RLIMIT_CPU`, `RLIMIT_FSIZE`,
//!   `RLIMIT_NOFILE` and `RLIMIT_CORE`; the supervisor additionally enforces
//!   wall time, exact CPU time and combined output size, and kills the worker
//!   on any violation. `no_new_privs` and `PDEATHSIG` are always set.
//!
//! Requires Landlock (Linux 5.13+, enabled in the LSM list). Without it the
//! backend reports itself unavailable and launches nothing -- fail closed.

use super::unix::{apply_common_limits, platform_error, supervise, Sample, WorkDir};
use super::{CommandSpec, ResourceLimits, SandboxAvailability, SandboxError, SandboxRunOutput};

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

// ── Landlock ABI (linux/landlock.h); syscall numbers are the same on every
// architecture since they were added in 5.13. ────────────────────────────────
const SYS_LANDLOCK_CREATE_RULESET: libc::c_long = 444;
const SYS_LANDLOCK_ADD_RULE: libc::c_long = 445;
const SYS_LANDLOCK_RESTRICT_SELF: libc::c_long = 446;
const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1 << 0;
const LANDLOCK_RULE_PATH_BENEATH: libc::c_int = 1;

const ACCESS_FS_EXECUTE: u64 = 1 << 0;
const ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
const ACCESS_FS_READ_FILE: u64 = 1 << 2;
const ACCESS_FS_READ_DIR: u64 = 1 << 3;
const ACCESS_FS_REMOVE_DIR: u64 = 1 << 4;
const ACCESS_FS_REMOVE_FILE: u64 = 1 << 5;
const ACCESS_FS_MAKE_DIR: u64 = 1 << 7;
const ACCESS_FS_MAKE_REG: u64 = 1 << 8;
const ACCESS_FS_TRUNCATE: u64 = 1 << 14; // ABI 3
const ACCESS_NET_BIND_TCP: u64 = 1 << 0; // ABI 4
const ACCESS_NET_CONNECT_TCP: u64 = 1 << 1; // ABI 4

#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
    handled_access_net: u64,
}

#[repr(C, packed)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

/// Every filesystem right the running kernel's Landlock ABI knows, so that
/// anything not explicitly granted below is denied.
fn handled_fs_access(abi: i32) -> u64 {
    let mut access = (1 << 13) - 1; // ABI 1: EXECUTE ..= MAKE_SYM
    if abi >= 2 {
        access |= 1 << 13; // REFER
    }
    if abi >= 3 {
        access |= ACCESS_FS_TRUNCATE;
    }
    if abi >= 5 {
        access |= 1 << 15; // IOCTL_DEV
    }
    access
}

fn landlock_abi() -> Result<i32, std::io::Error> {
    // SAFETY: the version query reads no pointer arguments.
    let abi = unsafe {
        libc::syscall(
            SYS_LANDLOCK_CREATE_RULESET,
            std::ptr::null::<RulesetAttr>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if abi < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(abi as i32)
    }
}

pub(super) fn detect() -> SandboxAvailability {
    match landlock_abi() {
        Ok(abi) if abi >= 1 => {
            // SAFETY: PR_GET_SECCOMP takes no pointer arguments.
            if unsafe { libc::prctl(libc::PR_GET_SECCOMP, 0, 0, 0, 0) } < 0 {
                return SandboxAvailability::Unavailable {
                    reason: "seccomp is not available in this kernel".into(),
                };
            }
            SandboxAvailability::Enforced {
                capability_isolation: "Linux Landlock (filesystem, network) + seccomp-bpf",
                resource_limits: "rlimits + bounded supervisor pipes",
            }
        }
        Ok(_) => SandboxAvailability::Unavailable {
            reason: "Landlock reported an unusable ABI version".into(),
        },
        Err(error) => SandboxAvailability::Unavailable {
            reason: format!(
                "Landlock is not available ({error}); it needs Linux 5.13+ with landlock in the LSM list"
            ),
        },
    }
}

pub(super) fn current_process_is_appcontainer() -> Result<bool, SandboxError> {
    Ok(false)
}

/// A Landlock ruleset built in the parent. The child only has to enforce it,
/// which keeps the code between `fork` and `exec` allocation-free.
struct Ruleset {
    fd: OwnedFd,
}

impl Ruleset {
    fn new(abi: i32) -> Result<Self, SandboxError> {
        let attr = RulesetAttr {
            handled_access_fs: handled_fs_access(abi),
            handled_access_net: ACCESS_NET_BIND_TCP | ACCESS_NET_CONNECT_TCP,
        };
        // ABI 1-3 kernels know only the filesystem field.
        let size = if abi >= 4 {
            std::mem::size_of::<RulesetAttr>()
        } else {
            std::mem::size_of::<u64>()
        };
        // SAFETY: `attr` outlives the call and `size` does not exceed it.
        let fd = unsafe {
            libc::syscall(
                SYS_LANDLOCK_CREATE_RULESET,
                &attr as *const RulesetAttr,
                size,
                0u32,
            )
        };
        if fd < 0 {
            return Err(platform_error(
                "create Landlock ruleset",
                std::io::Error::last_os_error(),
            ));
        }
        // SAFETY: the kernel returned a fresh descriptor that we now own.
        Ok(Self {
            fd: unsafe { OwnedFd::from_raw_fd(fd as RawFd) },
        })
    }

    /// Grant `access` beneath `path`. A path that does not exist on this
    /// system is skipped; there is nothing there to grant.
    fn allow(&self, path: &Path, access: u64) -> Result<(), SandboxError> {
        let Ok(c_path) = CString::new(path.as_os_str().as_bytes()) else {
            return Ok(());
        };
        // SAFETY: valid NUL-terminated path; O_PATH opens without reading.
        let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
        if fd < 0 {
            return Ok(());
        }
        // SAFETY: just opened; closed when `owned` drops.
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        // A rule on a file may only carry file rights.
        let metadata =
            std::fs::metadata(path).map_err(|e| platform_error("stat sandbox path", e))?;
        let access = if metadata.is_dir() {
            access
        } else {
            access
                & (ACCESS_FS_EXECUTE
                    | ACCESS_FS_WRITE_FILE
                    | ACCESS_FS_READ_FILE
                    | ACCESS_FS_TRUNCATE)
        };
        let attr = PathBeneathAttr {
            allowed_access: access,
            parent_fd: owned.as_raw_fd(),
        };
        // SAFETY: `attr` is a valid packed path_beneath_attr for the call.
        let result = unsafe {
            libc::syscall(
                SYS_LANDLOCK_ADD_RULE,
                self.fd.as_raw_fd(),
                LANDLOCK_RULE_PATH_BENEATH,
                &attr as *const PathBeneathAttr,
                0u32,
            )
        };
        if result < 0 {
            return Err(platform_error(
                &format!("add Landlock rule for {}", path.display()),
                std::io::Error::last_os_error(),
            ));
        }
        Ok(())
    }
}

/// The ELF interpreter (`PT_INTERP`) of a dynamically linked binary; the
/// kernel opens it for execution too, so it needs the execute right.
fn elf_interpreter(binary: &Path) -> Option<PathBuf> {
    let bytes = std::fs::read(binary).ok()?;
    if bytes.get(..4)? != b"\x7fELF" || *bytes.get(4)? != 2 {
        return None; // not a 64-bit ELF
    }
    let u16_at = |at: usize| Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?));
    let u64_at = |at: usize| Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?));
    let program_headers = u64_at(0x20)? as usize;
    let entry_size = u16_at(0x36)? as usize;
    let entries = u16_at(0x38)? as usize;
    for index in 0..entries {
        let header = program_headers + index * entry_size;
        if u32::from_le_bytes(bytes.get(header..header + 4)?.try_into().ok()?) == 3 {
            // PT_INTERP: p_offset at +8, p_filesz at +32.
            let offset = u64_at(header + 8)? as usize;
            let size = u64_at(header + 32)? as usize;
            let raw = bytes.get(offset..offset + size)?;
            let text = raw.split(|&b| b == 0).next()?;
            return Some(PathBuf::from(std::ffi::OsStr::from_bytes(text)));
        }
    }
    None
}

// ── seccomp-bpf ──────────────────────────────────────────────────────────────

const BPF_LD_W_ABS: u16 = 0x20;
const BPF_JEQ_K: u16 = 0x15;
const BPF_JGE_K: u16 = 0x35;
const BPF_JSET_K: u16 = 0x45;
const BPF_RET_K: u16 = 0x06;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const OFFSET_NR: u32 = 0;
const OFFSET_ARCH: u32 = 4;
const OFFSET_ARG0_LOW: u32 = 16; // little-endian low word of args[0]
const CLONE_THREAD: u32 = 0x0001_0000;

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xC000_003E;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xC000_00B7;

/// System calls refused with `EPERM`.
fn denied_syscalls() -> &'static [libc::c_long] {
    &[
        libc::SYS_socket,
        libc::SYS_socketpair,
        libc::SYS_connect,
        libc::SYS_bind,
        libc::SYS_listen,
        libc::SYS_accept4,
        #[cfg(target_arch = "x86_64")]
        libc::SYS_accept,
        #[cfg(target_arch = "x86_64")]
        libc::SYS_fork,
        #[cfg(target_arch = "x86_64")]
        libc::SYS_vfork,
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        libc::SYS_pidfd_getfd,
        libc::SYS_pidfd_send_signal,
        libc::SYS_tkill,
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_pivot_root,
        libc::SYS_chroot,
        libc::SYS_unshare,
        libc::SYS_setns,
        libc::SYS_open_tree,
        libc::SYS_move_mount,
        libc::SYS_fsopen,
        libc::SYS_fsconfig,
        libc::SYS_fsmount,
        libc::SYS_fspick,
        libc::SYS_kexec_load,
        libc::SYS_kexec_file_load,
        libc::SYS_init_module,
        libc::SYS_finit_module,
        libc::SYS_delete_module,
        libc::SYS_bpf,
        libc::SYS_perf_event_open,
        libc::SYS_keyctl,
        libc::SYS_add_key,
        libc::SYS_request_key,
        libc::SYS_userfaultfd,
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
        libc::SYS_open_by_handle_at,
        libc::SYS_name_to_handle_at,
        libc::SYS_swapon,
        libc::SYS_swapoff,
        libc::SYS_reboot,
    ]
}

const MAX_FILTER: usize = 192;

/// A seccomp program built on the stack, so it can be assembled between
/// `fork` and `exec` (it embeds the child's own pid) without allocating.
struct Filter {
    code: [libc::sock_filter; MAX_FILTER],
    len: usize,
}

impl Filter {
    fn push(&mut self, code: u16, jt: u8, jf: u8, k: u32) {
        // Bounded by construction: the program below has a fixed shape.
        self.code[self.len] = libc::sock_filter { code, jt, jf, k };
        self.len += 1;
    }

    /// The process's policy. `pid` is the worker's own pid: signals and
    /// limit changes may only target the worker itself.
    fn build(pid: u32) -> Self {
        let mut f = Filter {
            code: [libc::sock_filter {
                code: 0,
                jt: 0,
                jf: 0,
                k: 0,
            }; MAX_FILTER],
            len: 0,
        };
        let errno = |e: i32| SECCOMP_RET_ERRNO | (e as u32 & 0xffff);
        // Wrong architecture: no mapping of syscall numbers is trustworthy.
        f.push(BPF_LD_W_ABS, 0, 0, OFFSET_ARCH);
        f.push(BPF_JEQ_K, 1, 0, AUDIT_ARCH);
        f.push(BPF_RET_K, 0, 0, SECCOMP_RET_KILL_PROCESS);
        f.push(BPF_LD_W_ABS, 0, 0, OFFSET_NR);
        #[cfg(target_arch = "x86_64")]
        {
            // x32-ABI syscall numbers would bypass every check below.
            f.push(BPF_JGE_K, 0, 1, 0x4000_0000);
            f.push(BPF_RET_K, 0, 0, SECCOMP_RET_KILL_PROCESS);
        }
        for &nr in denied_syscalls() {
            f.push(BPF_JEQ_K, 0, 1, nr as u32);
            f.push(BPF_RET_K, 0, 0, errno(libc::EPERM));
        }
        // clone3 hides its flags in memory; glibc falls back to clone.
        f.push(BPF_JEQ_K, 0, 1, libc::SYS_clone3 as u32);
        f.push(BPF_RET_K, 0, 0, errno(libc::ENOSYS));
        // kill / tgkill / prlimit64: only the worker itself (pid 0 = self
        // for prlimit64).
        for (nr, allow_zero) in [
            (libc::SYS_kill, false),
            (libc::SYS_tgkill, false),
            (libc::SYS_prlimit64, true),
        ] {
            // if nr matches: load arg0; allow if == pid (or 0); else EPERM.
            f.push(BPF_JEQ_K, 0, if allow_zero { 5 } else { 4 }, nr as u32);
            f.push(BPF_LD_W_ABS, 0, 0, OFFSET_ARG0_LOW);
            if allow_zero {
                f.push(BPF_JEQ_K, 2, 0, 0);
            }
            f.push(BPF_JEQ_K, 1, 0, pid);
            f.push(BPF_RET_K, 0, 0, errno(libc::EPERM));
            f.push(BPF_RET_K, 0, 0, SECCOMP_RET_ALLOW);
        }
        // clone: threads only.
        f.push(BPF_JEQ_K, 0, 4, libc::SYS_clone as u32);
        f.push(BPF_LD_W_ABS, 0, 0, OFFSET_ARG0_LOW);
        f.push(BPF_JSET_K, 1, 0, CLONE_THREAD);
        f.push(BPF_RET_K, 0, 0, errno(libc::EPERM));
        f.push(BPF_RET_K, 0, 0, SECCOMP_RET_ALLOW);
        f.push(BPF_RET_K, 0, 0, SECCOMP_RET_ALLOW);
        f
    }
}

// ── Running the worker ───────────────────────────────────────────────────────

pub(super) fn run(
    command: &CommandSpec,
    limits: ResourceLimits,
) -> Result<SandboxRunOutput, SandboxError> {
    limits.validate().map_err(SandboxError::InvalidLimits)?;
    let abi = landlock_abi().map_err(|error| {
        SandboxError::Unavailable(format!("Landlock is not available: {error}"))
    })?;
    // The only place the worker may write, and the only binary it may run.
    let work = WorkDir::stage(command.executable())?;

    let ruleset = Ruleset::new(abi)?;
    let read = ACCESS_FS_READ_FILE | ACCESS_FS_READ_DIR;
    for dir in [
        "/usr",
        "/lib",
        "/lib64",
        "/lib32",
        "/etc",
        "/sys/devices/system/cpu",
    ] {
        ruleset.allow(Path::new(dir), read)?;
    }
    ruleset.allow(
        Path::new("/dev/null"),
        ACCESS_FS_READ_FILE | ACCESS_FS_WRITE_FILE,
    )?;
    ruleset.allow(Path::new("/dev/urandom"), ACCESS_FS_READ_FILE)?;
    let mut scratch = read
        | ACCESS_FS_WRITE_FILE
        | ACCESS_FS_REMOVE_FILE
        | ACCESS_FS_REMOVE_DIR
        | ACCESS_FS_MAKE_REG
        | ACCESS_FS_MAKE_DIR;
    if abi >= 3 {
        scratch |= ACCESS_FS_TRUNCATE;
    }
    ruleset.allow(&work.path, scratch)?;
    ruleset.allow(&work.worker, ACCESS_FS_EXECUTE | ACCESS_FS_READ_FILE)?;
    if let Some(interpreter) = elf_interpreter(&work.worker) {
        ruleset.allow(&interpreter, ACCESS_FS_EXECUTE | ACCESS_FS_READ_FILE)?;
    }
    let ruleset_fd = ruleset.fd.as_raw_fd();

    let mut process = Command::new(&work.worker);
    process
        .args(command.arguments())
        .current_dir(&work.path)
        .env_clear()
        // glibc reserves a 64 MiB arena per thread by default, which would
        // exhaust RLIMIT_AS long before real memory use does.
        .env("MALLOC_ARENA_MAX", "2")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: the closure runs in the forked child before exec. It only
    // issues system calls and fills a stack-allocated filter: no allocation,
    // no locks.
    unsafe {
        process.pre_exec(move || {
            apply_common_limits(&limits)?;
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
                || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            if libc::syscall(SYS_LANDLOCK_RESTRICT_SELF, ruleset_fd, 0u32) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let filter = Filter::build(libc::getpid() as u32);
            let program = libc::sock_fprog {
                len: filter.len as u16,
                filter: filter.code.as_ptr() as *mut libc::sock_filter,
            };
            if libc::prctl(
                libc::PR_SET_SECCOMP,
                libc::SECCOMP_MODE_FILTER as libc::c_ulong,
                &program as *const libc::sock_fprog,
                0,
                0,
            ) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let started = Instant::now();
    let child = process
        .spawn()
        .map_err(|error| platform_error("start sandboxed worker", error))?;
    drop(ruleset);
    // SAFETY: plain sysconf query.
    let ticks_per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
    let result = supervise(child, limits, started, |pid| Sample {
        cpu_ms: cpu_time_ms(pid, ticks_per_second),
        // RLIMIT_AS bounds memory in the kernel.
        resident_bytes: None,
    });
    drop(work);
    result
}

/// User + system CPU time of a running process, from `/proc/<pid>/stat`.
fn cpu_time_ms(pid: libc::pid_t, ticks_per_second: u64) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Fields after the parenthesised command name; utime and stime are the
    // 14th and 15th fields overall.
    let rest = stat.rsplit_once(')')?.1;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some((utime + stime) * 1000 / ticks_per_second)
}
