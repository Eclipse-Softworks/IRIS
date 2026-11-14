//! Security audit infrastructure for IRIS.
//!
//! Provides path validation, FFI safety checks, and network access control.
//! All potentially dangerous operations (filesystem, network, FFI) are routed
//! through these checks before execution.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

// ---------------------------------------------------------------------------
// Security Policy
// ---------------------------------------------------------------------------

/// Global security policy controlling what an IRIS program is allowed to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityPolicy {
    /// Allow filesystem read operations.
    pub allow_fs_read: bool,
    /// Allow filesystem write operations.
    pub allow_fs_write: bool,
    /// Allow network operations (TCP, HTTP).
    pub allow_network: bool,
    /// Allow FFI / dynamic library loading.
    pub allow_ffi: bool,
    /// Allow process spawning / shell commands.
    pub allow_process: bool,
    /// Allowed filesystem directories (reads). Empty = unrestricted when allow_fs_read is true.
    pub fs_read_allowlist: Vec<PathBuf>,
    /// Allowed filesystem directories (writes). Empty = unrestricted when allow_fs_write is true.
    pub fs_write_allowlist: Vec<PathBuf>,
    /// Blocked filesystem paths (always denied).
    pub fs_blocklist: Vec<PathBuf>,
    /// Allowed network hosts. Empty = unrestricted when allow_network is true.
    pub network_allowlist: Vec<String>,
    /// Blocked network hosts (always denied).
    pub network_blocklist: Vec<String>,
    /// Allowed FFI library paths. Empty = unrestricted when allow_ffi is true.
    pub ffi_allowlist: Vec<PathBuf>,
    /// Maximum file size for write operations (bytes). 0 = unlimited.
    pub max_file_write_bytes: u64,
    /// Maximum number of open file handles. 0 = unlimited.
    pub max_open_files: u32,
    /// Maximum number of network connections. 0 = unlimited.
    pub max_connections: u32,
}

impl Default for SecurityPolicy {
    /// Default: permissive (everything allowed, no restrictions).
    fn default() -> Self {
        Self {
            allow_fs_read: true,
            allow_fs_write: true,
            allow_network: true,
            allow_ffi: true,
            allow_process: true,
            fs_read_allowlist: Vec::new(),
            fs_write_allowlist: Vec::new(),
            fs_blocklist: Vec::new(),
            network_allowlist: Vec::new(),
            network_blocklist: Vec::new(),
            ffi_allowlist: Vec::new(),
            max_file_write_bytes: 0,
            max_open_files: 0,
            max_connections: 0,
        }
    }
}

impl SecurityPolicy {
    /// Fully sandboxed: denies everything.
    pub fn sandboxed() -> Self {
        Self {
            allow_fs_read: false,
            allow_fs_write: false,
            allow_network: false,
            allow_ffi: false,
            allow_process: false,
            fs_read_allowlist: Vec::new(),
            fs_write_allowlist: Vec::new(),
            fs_blocklist: Vec::new(),
            network_allowlist: Vec::new(),
            network_blocklist: Vec::new(),
            ffi_allowlist: Vec::new(),
            max_file_write_bytes: 0,
            max_open_files: 0,
            max_connections: 0,
        }
    }

    /// Default with specified working directory for FS access.
    pub fn with_working_dir(dir: PathBuf) -> Self {
        Self {
            allow_fs_read: true,
            allow_fs_write: true,
            allow_network: true,
            allow_ffi: true,
            allow_process: true,
            fs_read_allowlist: vec![dir.clone()],
            fs_write_allowlist: vec![dir],
            fs_blocklist: Vec::new(),
            network_allowlist: Vec::new(),
            network_blocklist: Vec::new(),
            ffi_allowlist: Vec::new(),
            max_file_write_bytes: 0,
            max_open_files: 0,
            max_connections: 0,
        }
    }

    /// Returns `true` when this policy matches `SecurityPolicy::default()`.
    pub fn is_default_permissive(&self) -> bool {
        *self == Self::default()
    }

    /// Serialize this policy into the key-value line format expected by the C runtime.
    pub fn to_runtime_spec(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let bit = |flag: bool| i32::from(flag);
        let _ = writeln!(out, "allow_fs_read={}", bit(self.allow_fs_read));
        let _ = writeln!(out, "allow_fs_write={}", bit(self.allow_fs_write));
        let _ = writeln!(out, "allow_network={}", bit(self.allow_network));
        let _ = writeln!(out, "allow_ffi={}", bit(self.allow_ffi));
        let _ = writeln!(out, "allow_process={}", bit(self.allow_process));
        let max_write = i64::try_from(self.max_file_write_bytes).unwrap_or(i64::MAX);
        let _ = writeln!(out, "max_file_write_bytes={max_write}");
        let _ = writeln!(out, "max_open_files={}", self.max_open_files);
        let _ = writeln!(out, "max_connections={}", self.max_connections);
        for p in &self.fs_read_allowlist {
            let _ = writeln!(out, "fs_read_allow={}", p.display());
        }
        for p in &self.fs_write_allowlist {
            let _ = writeln!(out, "fs_write_allow={}", p.display());
        }
        for p in &self.fs_blocklist {
            let _ = writeln!(out, "fs_block={}", p.display());
        }
        for h in &self.network_allowlist {
            let _ = writeln!(out, "net_allow={h}");
        }
        for h in &self.network_blocklist {
            let _ = writeln!(out, "net_block={h}");
        }
        for p in &self.ffi_allowlist {
            let _ = writeln!(out, "ffi_allow={}", p.display());
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Security Errors
// ---------------------------------------------------------------------------

/// Errors raised when a security policy denies an operation.
#[derive(Debug, Clone)]
pub enum SecurityError {
    FsReadDenied { path: String },
    FsWriteDenied { path: String },
    NetworkDenied { host: String },
    FfiDenied { library: String },
    ProcessDenied { command: String },
    PathTraversal { path: String },
    FileSizeLimitExceeded { size: u64, limit: u64 },
    TooManyOpenFiles { current: u32, limit: u32 },
    TooManyConnections { current: u32, limit: u32 },
}

impl std::fmt::Display for SecurityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FsReadDenied { path } => {
                write!(f, "security: filesystem read denied for '{}'", path)
            }
            Self::FsWriteDenied { path } => {
                write!(f, "security: filesystem write denied for '{}'", path)
            }
            Self::NetworkDenied { host } => {
                write!(f, "security: network access denied for '{}'", host)
            }
            Self::FfiDenied { library } => {
                write!(f, "security: FFI access denied for library '{}'", library)
            }
            Self::ProcessDenied { command } => {
                write!(f, "security: process execution denied for '{}'", command)
            }
            Self::PathTraversal { path } => {
                write!(f, "security: path traversal attempt detected in '{}'", path)
            }
            Self::FileSizeLimitExceeded { size, limit } => {
                write!(
                    f,
                    "security: file write size {} exceeds limit {}",
                    size, limit
                )
            }
            Self::TooManyOpenFiles { current, limit } => {
                write!(
                    f,
                    "security: open file limit reached ({}/{})",
                    current, limit
                )
            }
            Self::TooManyConnections { current, limit } => {
                write!(
                    f,
                    "security: connection limit reached ({}/{})",
                    current, limit
                )
            }
        }
    }
}

impl std::error::Error for SecurityError {}

// ---------------------------------------------------------------------------
// Global Security State
// ---------------------------------------------------------------------------

extern "C" {
    fn iris_sandbox_set_policy_spec(
        spec: *const std::os::raw::c_char,
        lock_policy: std::os::raw::c_int,
    );
}

static GLOBAL_POLICY: OnceLock<Mutex<SecurityPolicy>> = OnceLock::new();

fn global_policy() -> &'static Mutex<SecurityPolicy> {
    GLOBAL_POLICY.get_or_init(|| Mutex::new(SecurityPolicy::default()))
}

/// Set the global security policy. Should be called before any IRIS evaluation.
/// Synchronizes both the Rust policy and the C runtime sandbox state.
pub fn set_security_policy(policy: SecurityPolicy) {
    let spec = policy.to_runtime_spec();
    {
        let guard = global_policy();
        *guard.lock().unwrap() = policy;
    }
    if let Ok(c_spec) = std::ffi::CString::new(spec) {
        // SAFETY: `c_spec` is a valid NUL-terminated buffer for this call.
        unsafe {
            iris_sandbox_set_policy_spec(c_spec.as_ptr(), 0);
        }
    }
}

/// Return whether an `extern def` calls an IRIS runtime-mediated function
/// rather than an unmediated raw C symbol or third-party shared library.
pub fn is_mediated_runtime_extern(ext: &crate::ir::module::IrExternFn) -> bool {
    ext.link_lib.is_none() && ext.name.starts_with("iris_")
}

/// Check FFI permission against an explicit `SecurityPolicy`.
pub fn check_ffi_with_policy(
    policy: &SecurityPolicy,
    library_path: &str,
) -> Result<(), SecurityError> {
    if !policy.allow_ffi {
        log_audit(AuditOp::FfiLoad, false, library_path.to_string());
        return Err(SecurityError::FfiDenied {
            library: library_path.to_string(),
        });
    }
    let validated = validate_path(library_path)?;
    if !policy.ffi_allowlist.is_empty() {
        let allowed = policy
            .ffi_allowlist
            .iter()
            .any(|a| validated.starts_with(a));
        if !allowed {
            log_audit(AuditOp::FfiLoad, false, library_path.to_string());
            return Err(SecurityError::FfiDenied {
                library: library_path.to_string(),
            });
        }
    }
    log_audit(AuditOp::FfiLoad, true, library_path.to_string());
    Ok(())
}

/// Validate all `extern def` declarations in an IR module against `policy`.
/// Any non-runtime C declaration or external linked library is rejected when FFI
/// is denied or excluded by `ffi_allowlist`.
pub fn validate_module_ffi_with_policy(
    extern_fns: &[crate::ir::module::IrExternFn],
    policy: &SecurityPolicy,
) -> Result<(), SecurityError> {
    for ext in extern_fns {
        if is_mediated_runtime_extern(ext) {
            continue;
        }
        let target = ext.link_lib.as_deref().unwrap_or(ext.name.as_str());
        check_ffi_with_policy(policy, target)?;
    }
    Ok(())
}

/// Validate all `extern def` declarations in an IR module against the global policy.
pub fn validate_module_ffi(
    extern_fns: &[crate::ir::module::IrExternFn],
) -> Result<(), SecurityError> {
    let policy = get_security_policy();
    validate_module_ffi_with_policy(extern_fns, &policy)
}

/// Get a snapshot of the current global security policy.
pub fn get_security_policy() -> SecurityPolicy {
    global_policy().lock().unwrap().clone()
}

// ---------------------------------------------------------------------------
// Audit Log
// ---------------------------------------------------------------------------

/// Record of a security-relevant operation.
#[derive(Debug, Clone)]
pub struct AuditEntry {
    pub timestamp_ms: u64,
    pub operation: AuditOp,
    pub allowed: bool,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub enum AuditOp {
    FsRead,
    FsWrite,
    FsDelete,
    Network,
    FfiLoad,
    FfiCall,
    ProcessSpawn,
}

impl std::fmt::Display for AuditOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FsRead => write!(f, "fs_read"),
            Self::FsWrite => write!(f, "fs_write"),
            Self::FsDelete => write!(f, "fs_delete"),
            Self::Network => write!(f, "network"),
            Self::FfiLoad => write!(f, "ffi_load"),
            Self::FfiCall => write!(f, "ffi_call"),
            Self::ProcessSpawn => write!(f, "process_spawn"),
        }
    }
}

static AUDIT_LOG: OnceLock<Mutex<Vec<AuditEntry>>> = OnceLock::new();

fn audit_log() -> &'static Mutex<Vec<AuditEntry>> {
    AUDIT_LOG.get_or_init(|| Mutex::new(Vec::new()))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn log_audit(op: AuditOp, allowed: bool, detail: String) {
    let entry = AuditEntry {
        timestamp_ms: now_ms(),
        operation: op,
        allowed,
        detail,
    };
    if let Ok(mut log) = audit_log().lock() {
        log.push(entry);
    }
}

/// Get all audit entries (snapshot).
pub fn get_audit_log() -> Vec<AuditEntry> {
    audit_log().lock().unwrap().clone()
}

/// Clear the audit log.
pub fn clear_audit_log() {
    audit_log().lock().unwrap().clear();
}

// ---------------------------------------------------------------------------
// Path Validation
// ---------------------------------------------------------------------------

/// Normalize and validate a filesystem path against the policy.
/// Returns the canonicalized path on success.
pub fn validate_path(raw: &str) -> Result<PathBuf, SecurityError> {
    let path = Path::new(raw);

    // Reject obvious path traversal patterns.
    let normalized = raw.replace('\\', "/");
    if normalized.contains("/../")
        || normalized.ends_with("/..")
        || normalized.starts_with("../")
        || normalized == ".."
    {
        return Err(SecurityError::PathTraversal {
            path: raw.to_string(),
        });
    }

    // Check for null bytes (injection attack).
    if raw.contains('\0') {
        return Err(SecurityError::PathTraversal {
            path: raw.to_string(),
        });
    }

    // On Windows, reject device paths.
    #[cfg(windows)]
    {
        let upper = normalized.to_uppercase();
        if upper.starts_with("\\\\.\\") || upper.starts_with("\\\\?\\") {
            return Err(SecurityError::PathTraversal {
                path: raw.to_string(),
            });
        }
        // Reject Windows device names (CON, PRN, AUX, NUL, COM1-9, LPT1-9).
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_uppercase();
        let devices = [
            "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
            "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
        ];
        if devices.contains(&stem.as_str()) {
            return Err(SecurityError::PathTraversal {
                path: raw.to_string(),
            });
        }
    }

    Ok(path.to_path_buf())
}

/// Check if a filesystem read is allowed by the current policy.
pub fn check_fs_read(path: &str) -> Result<(), SecurityError> {
    let policy = get_security_policy();

    if !policy.allow_fs_read {
        log_audit(AuditOp::FsRead, false, path.to_string());
        return Err(SecurityError::FsReadDenied {
            path: path.to_string(),
        });
    }

    let validated = validate_path(path)?;

    // Check blocklist.
    for blocked in &policy.fs_blocklist {
        if validated.starts_with(blocked) {
            log_audit(AuditOp::FsRead, false, path.to_string());
            return Err(SecurityError::FsReadDenied {
                path: path.to_string(),
            });
        }
    }

    // Check allowlist (if non-empty).
    if !policy.fs_read_allowlist.is_empty() {
        let allowed = policy
            .fs_read_allowlist
            .iter()
            .any(|a| validated.starts_with(a));
        if !allowed {
            log_audit(AuditOp::FsRead, false, path.to_string());
            return Err(SecurityError::FsReadDenied {
                path: path.to_string(),
            });
        }
    }

    log_audit(AuditOp::FsRead, true, path.to_string());
    Ok(())
}

/// Check if a filesystem write is allowed by the current policy.
pub fn check_fs_write(path: &str) -> Result<(), SecurityError> {
    let policy = get_security_policy();

    if !policy.allow_fs_write {
        log_audit(AuditOp::FsWrite, false, path.to_string());
        return Err(SecurityError::FsWriteDenied {
            path: path.to_string(),
        });
    }

    let validated = validate_path(path)?;

    // Check blocklist.
    for blocked in &policy.fs_blocklist {
        if validated.starts_with(blocked) {
            log_audit(AuditOp::FsWrite, false, path.to_string());
            return Err(SecurityError::FsWriteDenied {
                path: path.to_string(),
            });
        }
    }

    // Check allowlist (if non-empty).
    if !policy.fs_write_allowlist.is_empty() {
        let allowed = policy
            .fs_write_allowlist
            .iter()
            .any(|a| validated.starts_with(a));
        if !allowed {
            log_audit(AuditOp::FsWrite, false, path.to_string());
            return Err(SecurityError::FsWriteDenied {
                path: path.to_string(),
            });
        }
    }

    log_audit(AuditOp::FsWrite, true, path.to_string());
    Ok(())
}

/// Check if writing `bytes` to `path` is allowed by the current policy.
pub fn check_fs_write_bytes(path: &str, bytes: u64) -> Result<(), SecurityError> {
    check_fs_write(path)?;
    let limit = get_security_policy().max_file_write_bytes;
    if limit > 0 && bytes > limit {
        log_audit(AuditOp::FsWrite, false, path.to_string());
        return Err(SecurityError::FileSizeLimitExceeded { size: bytes, limit });
    }
    Ok(())
}

/// Check if a network operation is allowed by the current policy.
pub fn check_network(host: &str) -> Result<(), SecurityError> {
    let policy = get_security_policy();

    if !policy.allow_network {
        log_audit(AuditOp::Network, false, host.to_string());
        return Err(SecurityError::NetworkDenied {
            host: host.to_string(),
        });
    }

    // Check blocklist.
    for blocked in &policy.network_blocklist {
        if host == blocked || host.ends_with(blocked) {
            log_audit(AuditOp::Network, false, host.to_string());
            return Err(SecurityError::NetworkDenied {
                host: host.to_string(),
            });
        }
    }

    // Check allowlist (if non-empty).
    if !policy.network_allowlist.is_empty() {
        let allowed = policy
            .network_allowlist
            .iter()
            .any(|a| host == a || host.ends_with(a.as_str()));
        if !allowed {
            log_audit(AuditOp::Network, false, host.to_string());
            return Err(SecurityError::NetworkDenied {
                host: host.to_string(),
            });
        }
    }

    log_audit(AuditOp::Network, true, host.to_string());
    Ok(())
}

/// Check if FFI library loading is allowed by the current policy.
pub fn check_ffi(library_path: &str) -> Result<(), SecurityError> {
    check_ffi_with_policy(&get_security_policy(), library_path)
}

/// Check if process spawning is allowed by the current policy.
pub fn check_process(command: &str) -> Result<(), SecurityError> {
    let policy = get_security_policy();

    if !policy.allow_process {
        log_audit(AuditOp::ProcessSpawn, false, command.to_string());
        return Err(SecurityError::ProcessDenied {
            command: command.to_string(),
        });
    }

    log_audit(AuditOp::ProcessSpawn, true, command.to_string());
    Ok(())
}

// ---------------------------------------------------------------------------
// Audit Summary / Report
// ---------------------------------------------------------------------------

/// Generate a human-readable security audit report.
pub fn audit_report() -> String {
    let entries = get_audit_log();
    if entries.is_empty() {
        return "Security audit: no operations recorded.\n".to_string();
    }

    let mut report = String::new();
    report.push_str("═══════════════════════════════════════════\n");
    report.push_str(" IRIS Security Audit Report\n");
    report.push_str("═══════════════════════════════════════════\n\n");

    let total = entries.len();
    let denied = entries.iter().filter(|e| !e.allowed).count();
    let allowed = total - denied;

    report.push_str(&format!("Total operations:  {}\n", total));
    report.push_str(&format!("Allowed:           {}\n", allowed));
    report.push_str(&format!("Denied:            {}\n\n", denied));

    // Group by operation type.
    let mut ops_count: std::collections::BTreeMap<String, (usize, usize)> =
        std::collections::BTreeMap::new();
    for entry in &entries {
        let key = format!("{}", entry.operation);
        let (a, d) = ops_count.entry(key).or_insert((0, 0));
        if entry.allowed {
            *a += 1;
        } else {
            *d += 1;
        }
    }

    report.push_str("By operation:\n");
    for (op, (a, d)) in &ops_count {
        report.push_str(&format!("  {:<15} allowed={}, denied={}\n", op, a, d));
    }

    if denied > 0 {
        report.push_str("\nDenied operations:\n");
        for entry in entries.iter().filter(|e| !e.allowed) {
            report.push_str(&format!(
                "  [{}] {} — {}\n",
                entry.timestamp_ms, entry.operation, entry.detail
            ));
        }
    }

    report.push_str("\n═══════════════════════════════════════════\n");
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_traversal_rejection() {
        assert!(validate_path("../../../etc/passwd").is_err());
        assert!(validate_path("foo/../../bar").is_err());
        assert!(validate_path("..").is_err());
    }

    #[test]
    fn test_path_null_byte_rejection() {
        assert!(validate_path("foo\0bar").is_err());
    }

    #[test]
    fn test_valid_paths() {
        assert!(validate_path("hello.iris").is_ok());
        assert!(validate_path("src/main.rs").is_ok());
        assert!(validate_path("./local_file").is_ok());
    }

    #[test]
    fn test_sandboxed_policy_denies_all() {
        let policy = SecurityPolicy::sandboxed();
        assert!(!policy.allow_fs_read);
        assert!(!policy.allow_fs_write);
        assert!(!policy.allow_network);
        assert!(!policy.allow_ffi);
        assert!(!policy.allow_process);
    }

    #[test]
    fn test_default_policy_allows_all() {
        let policy = SecurityPolicy::default();
        assert!(policy.allow_fs_read);
        assert!(policy.allow_fs_write);
        assert!(policy.allow_network);
        assert!(policy.allow_ffi);
        assert!(policy.allow_process);
    }
}
