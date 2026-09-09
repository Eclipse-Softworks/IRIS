//! Fail-closed native worker sandbox foundation.
//!
//! A native process is considered sandboxed only when the platform backend can
//! enforce both capability isolation and resource/process limits.  In
//! particular, a Windows Job Object by itself is *not* a security boundary:
//! the secure backend must also create the worker in an AppContainer with no
//! capabilities.

pub mod protocol;
pub mod supervisor;

use std::fmt;
use std::path::{Path, PathBuf};

pub use supervisor::{LimitViolation, ResourceLimits, ResourceSupervisor};

/// Report whether the calling process itself has a Windows AppContainer token.
/// This is primarily useful for worker attestation and diagnostics.
pub fn current_process_is_appcontainer() -> Result<bool, SandboxError> {
    platform::current_process_is_appcontainer()
}

/// Maximum number of arguments accepted by the worker bootstrap.
const MAX_ARGUMENTS: usize = 256;

/// Whether this build can enforce the complete native sandbox boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SandboxAvailability {
    /// Both capability isolation and resource/process limits are enforced.
    Enforced {
        capability_isolation: &'static str,
        resource_limits: &'static str,
    },
    /// The complete boundary cannot be established. Callers must fail closed.
    Unavailable { reason: String },
}

impl SandboxAvailability {
    pub fn is_enforced(&self) -> bool {
        matches!(self, Self::Enforced { .. })
    }

    pub fn require_enforced(&self) -> Result<(), SandboxError> {
        match self {
            Self::Enforced { .. } => Ok(()),
            Self::Unavailable { reason } => Err(SandboxError::Unavailable(reason.clone())),
        }
    }
}

/// A validated request to start the native sandbox worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    executable: PathBuf,
    arguments: Vec<String>,
}

impl CommandSpec {
    pub fn new(
        executable: impl Into<PathBuf>,
        arguments: Vec<String>,
    ) -> Result<Self, SandboxError> {
        let executable = executable.into();
        if !executable.is_absolute() {
            return Err(SandboxError::InvalidCommand(
                "worker executable must be an absolute path".into(),
            ));
        }
        if executable.as_os_str().is_empty() {
            return Err(SandboxError::InvalidCommand(
                "worker executable must not be empty".into(),
            ));
        }
        if arguments.len() > MAX_ARGUMENTS {
            return Err(SandboxError::InvalidCommand(format!(
                "worker argument count exceeds {MAX_ARGUMENTS}"
            )));
        }
        if arguments.iter().any(|arg| arg.contains('\0')) {
            return Err(SandboxError::InvalidCommand(
                "worker arguments must not contain NUL bytes".into(),
            ));
        }
        Ok(Self {
            executable,
            arguments,
        })
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    pub fn arguments(&self) -> &[String] {
        &self.arguments
    }
}

/// Entry point for launching native validation workers.
#[derive(Clone, Debug)]
pub struct NativeSandbox {
    availability: SandboxAvailability,
}

impl NativeSandbox {
    pub fn detect() -> Self {
        Self {
            availability: platform::detect(),
        }
    }

    pub fn availability(&self) -> &SandboxAvailability {
        &self.availability
    }

    /// Validate a launch request and refuse to start unless the complete
    /// platform boundary is active.
    ///
    /// The worker bootstrap is intentionally not implemented as an ordinary
    /// `std::process::Command`: on Windows that would create the process before
    /// it was assigned to a Job Object and would permit a child-process escape.
    /// The future backend must create it suspended with
    /// `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`, assign it to the Job, and
    /// only then resume its primary thread.
    pub fn prepare_launch(
        &self,
        command: &CommandSpec,
        limits: ResourceLimits,
    ) -> Result<PreparedLaunch, SandboxError> {
        self.availability.require_enforced()?;
        limits.validate().map_err(SandboxError::InvalidLimits)?;
        Ok(PreparedLaunch {
            command: command.clone(),
            limits,
        })
    }
}

impl Default for NativeSandbox {
    fn default() -> Self {
        Self::detect()
    }
}

/// A launch whose command and limits have passed the fail-closed checks.
#[derive(Clone, Debug)]
pub struct PreparedLaunch {
    command: CommandSpec,
    limits: ResourceLimits,
}

impl PreparedLaunch {
    pub fn command(&self) -> &CommandSpec {
        &self.command
    }

    pub fn limits(&self) -> ResourceLimits {
        self.limits
    }

    /// Execute the worker inside the enforced platform boundary.
    ///
    /// The platform backend owns process creation so the worker cannot run
    /// before capability isolation and resource limits are attached.
    pub fn run(&self) -> Result<SandboxRunOutput, SandboxError> {
        platform::run(&self.command, self.limits)
    }
}

/// Captured result and observed resource use from an enforced worker run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxRunOutput {
    pub exit_code: u32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub wall_time_ms: u64,
    pub cpu_time_ms: u64,
    pub peak_memory_bytes: u64,
    pub output_bytes: u64,
}

#[derive(Debug)]
pub enum SandboxError {
    Unavailable(String),
    InvalidCommand(String),
    InvalidLimits(String),
    Platform(String),
    LimitExceeded(LimitViolation),
}

impl fmt::Display for SandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(reason) => write!(f, "native sandbox unavailable: {reason}"),
            Self::InvalidCommand(reason) => write!(f, "invalid sandbox command: {reason}"),
            Self::InvalidLimits(reason) => write!(f, "invalid sandbox limits: {reason}"),
            Self::Platform(reason) => write!(f, "native sandbox platform error: {reason}"),
            Self::LimitExceeded(violation) => {
                write!(f, "native sandbox limit exceeded: {violation}")
            }
        }
    }
}

impl std::error::Error for SandboxError {}

#[cfg(windows)]
mod windows;

#[cfg(windows)]
mod platform {
    use super::{CommandSpec, ResourceLimits, SandboxAvailability, SandboxError, SandboxRunOutput};

    pub(super) fn detect() -> SandboxAvailability {
        super::windows::detect()
    }

    pub(super) fn run(
        command: &CommandSpec,
        limits: ResourceLimits,
    ) -> Result<SandboxRunOutput, SandboxError> {
        super::windows::run(command, limits)
    }

    pub(super) fn current_process_is_appcontainer() -> Result<bool, SandboxError> {
        super::windows::current_process_is_appcontainer()
    }
}

#[cfg(not(windows))]
mod platform {
    use super::{CommandSpec, ResourceLimits, SandboxAvailability, SandboxError, SandboxRunOutput};

    pub(super) fn detect() -> SandboxAvailability {
        SandboxAvailability::Unavailable {
            reason: "no enforced native sandbox backend is compiled for this platform".into(),
        }
    }

    pub(super) fn run(
        _command: &CommandSpec,
        _limits: ResourceLimits,
    ) -> Result<SandboxRunOutput, SandboxError> {
        Err(SandboxError::Unavailable(
            "no enforced native sandbox backend is compiled for this platform".into(),
        ))
    }

    pub(super) fn current_process_is_appcontainer() -> Result<bool, SandboxError> {
        Ok(false)
    }
}
