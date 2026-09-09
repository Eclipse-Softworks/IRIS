//! Length-framed, versioned protocol for the native sandbox worker.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::fmt;
use std::io::{self, Read, Write};

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerRequest {
    pub version: u16,
    pub request_id: String,
    pub operation: WorkerOperation,
}

impl WorkerRequest {
    pub fn new(request_id: impl Into<String>, operation: WorkerOperation) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            request_id: request_id.into(),
            operation,
        }
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.version != PROTOCOL_VERSION {
            return Err(ProtocolError::UnsupportedVersion(self.version));
        }
        validate_request_id(&self.request_id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkerOperation {
    ValidateArtifact {
        artifact_sha256: String,
        entrypoint: String,
    },
    Shutdown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerResponse {
    pub version: u16,
    pub request_id: String,
    pub outcome: WorkerOutcome,
    pub usage: WorkerUsage,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkerOutcome {
    Accepted { exit_code: i32 },
    Rejected { reason: String },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerUsage {
    pub wall_time_ms: u64,
    pub cpu_time_ms: u64,
    pub peak_memory_bytes: u64,
    pub output_bytes: u64,
    pub child_processes: u32,
}

pub fn write_json_frame<W: Write, T: Serialize>(
    writer: &mut W,
    message: &T,
) -> Result<(), ProtocolError> {
    let payload = serde_json::to_vec(message).map_err(ProtocolError::Json)?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge {
            announced: payload.len(),
            maximum: MAX_FRAME_BYTES,
        });
    }
    let length = u32::try_from(payload.len()).expect("frame size is bounded below u32::MAX");
    writer
        .write_all(&length.to_be_bytes())
        .map_err(ProtocolError::Io)?;
    writer.write_all(&payload).map_err(ProtocolError::Io)?;
    writer.flush().map_err(ProtocolError::Io)
}

pub fn read_json_frame<R: Read, T: DeserializeOwned>(reader: &mut R) -> Result<T, ProtocolError> {
    let mut prefix = [0_u8; 4];
    reader.read_exact(&mut prefix).map_err(ProtocolError::Io)?;
    let announced = u32::from_be_bytes(prefix) as usize;
    if announced > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge {
            announced,
            maximum: MAX_FRAME_BYTES,
        });
    }
    if announced == 0 {
        return Err(ProtocolError::EmptyFrame);
    }
    let mut payload = vec![0_u8; announced];
    reader.read_exact(&mut payload).map_err(ProtocolError::Io)?;
    serde_json::from_slice(&payload).map_err(ProtocolError::Json)
}

fn validate_request_id(request_id: &str) -> Result<(), ProtocolError> {
    let valid = !request_id.is_empty()
        && request_id.len() <= 128
        && request_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if valid {
        Ok(())
    } else {
        Err(ProtocolError::InvalidRequestId)
    }
}

#[derive(Debug)]
pub enum ProtocolError {
    Io(io::Error),
    Json(serde_json::Error),
    FrameTooLarge { announced: usize, maximum: usize },
    EmptyFrame,
    UnsupportedVersion(u16),
    InvalidRequestId,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "sandbox protocol I/O error: {error}"),
            Self::Json(error) => write!(f, "invalid sandbox protocol JSON: {error}"),
            Self::FrameTooLarge { announced, maximum } => write!(
                f,
                "sandbox frame is {announced} bytes; maximum is {maximum}"
            ),
            Self::EmptyFrame => write!(f, "sandbox protocol frame must not be empty"),
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported sandbox protocol version {version}")
            }
            Self::InvalidRequestId => write!(f, "invalid sandbox protocol request id"),
        }
    }
}

impl std::error::Error for ProtocolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            _ => None,
        }
    }
}
