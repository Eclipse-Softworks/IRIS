//! Platform-independent resource policy and accounting.
//!
//! This module validates limits and accounts observations from a platform
//! backend. It does not claim to impose OS memory/CPU/process limits itself.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::{Duration, Instant};

const MAX_WALL_TIME_MS: u64 = 60 * 60 * 1000;
const MAX_CPU_TIME_MS: u64 = 60 * 60 * 1000;
const MAX_MEMORY_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const MAX_OUTPUT_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_PROCESSES: u32 = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceLimits {
    pub wall_time_ms: u64,
    pub cpu_time_ms: u64,
    pub memory_bytes: u64,
    pub output_bytes: u64,
    pub process_count: u32,
}

impl ResourceLimits {
    pub fn validate(self) -> Result<(), String> {
        validate_nonzero_bounded("wall time", self.wall_time_ms, MAX_WALL_TIME_MS)?;
        validate_nonzero_bounded("CPU time", self.cpu_time_ms, MAX_CPU_TIME_MS)?;
        validate_nonzero_bounded("memory", self.memory_bytes, MAX_MEMORY_BYTES)?;
        validate_nonzero_bounded("output", self.output_bytes, MAX_OUTPUT_BYTES)?;
        if self.process_count == 0 || self.process_count > MAX_PROCESSES {
            return Err(format!(
                "process count must be between 1 and {MAX_PROCESSES}"
            ));
        }
        Ok(())
    }
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            wall_time_ms: 5_000,
            cpu_time_ms: 3_000,
            memory_bytes: 256 * 1024 * 1024,
            output_bytes: 1024 * 1024,
            // The worker itself is the only allowed process. A Windows Job
            // backend maps this to ACTIVE_PROCESS_LIMIT = 1.
            process_count: 1,
        }
    }
}

fn validate_nonzero_bounded(name: &str, value: u64, maximum: u64) -> Result<(), String> {
    if value == 0 || value > maximum {
        Err(format!("{name} must be between 1 and {maximum}"))
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct ResourceSupervisor {
    limits: ResourceLimits,
    started_at: Instant,
    output_bytes: u64,
}

impl ResourceSupervisor {
    pub fn start(limits: ResourceLimits) -> Result<Self, String> {
        limits.validate()?;
        Ok(Self {
            limits,
            started_at: Instant::now(),
            output_bytes: 0,
        })
    }

    pub fn limits(&self) -> ResourceLimits {
        self.limits
    }

    pub fn elapsed(&self) -> Duration {
        self.started_at.elapsed()
    }

    pub fn check_wall_time(&self) -> Result<(), LimitViolation> {
        self.check_elapsed(self.elapsed())
    }

    pub fn check_elapsed(&self, elapsed: Duration) -> Result<(), LimitViolation> {
        let limit = Duration::from_millis(self.limits.wall_time_ms);
        if elapsed > limit {
            Err(LimitViolation::WallTime {
                elapsed_ms: duration_millis_saturating(elapsed),
                limit_ms: self.limits.wall_time_ms,
            })
        } else {
            Ok(())
        }
    }

    /// Account captured stdout/stderr bytes. The caller must terminate the
    /// platform job/process when this returns a violation.
    pub fn account_output(&mut self, bytes: usize) -> Result<u64, LimitViolation> {
        self.output_bytes = self
            .output_bytes
            .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
        if self.output_bytes > self.limits.output_bytes {
            Err(LimitViolation::Output {
                observed_bytes: self.output_bytes,
                limit_bytes: self.limits.output_bytes,
            })
        } else {
            Ok(self.output_bytes)
        }
    }

    pub fn output_bytes(&self) -> u64 {
        self.output_bytes
    }
}

fn duration_millis_saturating(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LimitViolation {
    WallTime {
        elapsed_ms: u64,
        limit_ms: u64,
    },
    Output {
        observed_bytes: u64,
        limit_bytes: u64,
    },
    CpuTime {
        observed_ms: u64,
        limit_ms: u64,
    },
}

impl fmt::Display for LimitViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WallTime {
                elapsed_ms,
                limit_ms,
            } => write!(f, "sandbox wall time {elapsed_ms} ms exceeds {limit_ms} ms"),
            Self::Output {
                observed_bytes,
                limit_bytes,
            } => write!(
                f,
                "sandbox output {observed_bytes} bytes exceeds {limit_bytes} bytes"
            ),
            Self::CpuTime {
                observed_ms,
                limit_ms,
            } => write!(f, "sandbox CPU time {observed_ms} ms exceeds {limit_ms} ms"),
        }
    }
}

impl std::error::Error for LimitViolation {}
