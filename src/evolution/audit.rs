//! Append-only, tamper-evident audit records for the evolution pipeline.
//!
//! Each JSONL record commits to the complete preceding record through a
//! SHA-256 hash chain.  [`EvolutionAuditLog::open_with_expected_head`] also
//! compares the verified file head with a checkpoint held outside the log;
//! that explicit checkpoint is what makes deletion of a valid tail detectable.
//!
//! The API intentionally exposes no operation that clears, rewrites, or
//! deletes the log.  It is tamper-evident, not tamper-proof against an actor
//! with unrestricted access to both the log and its external checkpoint.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const AUDIT_SCHEMA: &str = "iris-evolution-audit/1";
pub const CHECKPOINT_SCHEMA: &str = "iris-evolution-audit-head/1";
pub const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvolutionEventKind {
    CandidateReceived,
    GatePassed,
    GateFailed,
    Promoted,
    Observation,
    RolledBack,
}

/// Stable, serializable evidence for one evolution decision.
///
/// `BTreeMap` is deliberate: its deterministic key order keeps the bytes fed
/// to SHA-256 independent of hash-map iteration order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvolutionEvent {
    pub kind: EvolutionEventKind,
    pub candidate_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constitution_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub evidence: BTreeMap<String, String>,
}

impl EvolutionEvent {
    pub fn new(kind: EvolutionEventKind, candidate_hash: impl Into<String>) -> Self {
        Self {
            kind,
            candidate_hash: candidate_hash.into(),
            constitution_hash: None,
            policy_hash: None,
            gate: None,
            generation: None,
            decision: None,
            evidence: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRecord {
    pub schema: String,
    pub sequence: u64,
    pub timestamp_ms: u64,
    pub previous_hash: String,
    pub event: EvolutionEvent,
    pub hash: String,
}

/// A checkpoint that should be persisted independently of the JSONL file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditHead {
    pub sequence: u64,
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuditCheckpoint {
    schema: String,
    head: AuditHead,
}

impl AuditHead {
    pub fn genesis() -> Self {
        Self {
            sequence: 0,
            hash: GENESIS_HASH.to_owned(),
        }
    }
}

#[derive(Debug)]
pub enum AuditError {
    Io(std::io::Error),
    Json {
        line: usize,
        source: serde_json::Error,
    },
    UnsupportedSchema {
        line: usize,
        schema: String,
    },
    SequenceMismatch {
        line: usize,
        expected: u64,
        actual: u64,
    },
    PreviousHashMismatch {
        line: usize,
        expected: String,
        actual: String,
    },
    RecordHashMismatch {
        line: usize,
        expected: String,
        actual: String,
    },
    ExpectedHeadMismatch {
        expected: AuditHead,
        actual: AuditHead,
    },
    HeadChanged {
        expected: AuditHead,
        actual: AuditHead,
    },
    MissingCheckpoint {
        path: PathBuf,
        actual: AuditHead,
    },
    UnsupportedCheckpointSchema(String),
    SequenceOverflow,
}

impl fmt::Display for AuditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "evolution audit I/O error: {error}"),
            Self::Json { line, source } => {
                write!(formatter, "invalid evolution audit JSON at line {line}: {source}")
            }
            Self::UnsupportedSchema { line, schema } => write!(
                formatter,
                "unsupported evolution audit schema '{schema}' at line {line}"
            ),
            Self::SequenceMismatch {
                line,
                expected,
                actual,
            } => write!(
                formatter,
                "evolution audit sequence mismatch at line {line}: expected {expected}, found {actual}"
            ),
            Self::PreviousHashMismatch {
                line,
                expected,
                actual,
            } => write!(
                formatter,
                "evolution audit previous hash mismatch at line {line}: expected {expected}, found {actual}"
            ),
            Self::RecordHashMismatch {
                line,
                expected,
                actual,
            } => write!(
                formatter,
                "evolution audit record hash mismatch at line {line}: expected {expected}, found {actual}"
            ),
            Self::ExpectedHeadMismatch { expected, actual } => write!(
                formatter,
                "evolution audit head mismatch: expected sequence {} hash {}, found sequence {} hash {}",
                expected.sequence, expected.hash, actual.sequence, actual.hash
            ),
            Self::HeadChanged { expected, actual } => write!(
                formatter,
                "evolution audit changed after opening: expected sequence {} hash {}, found sequence {} hash {}",
                expected.sequence, expected.hash, actual.sequence, actual.hash
            ),
            Self::MissingCheckpoint { path, actual } => write!(
                formatter,
                "evolution audit already contains sequence {} (hash {}) but trusted checkpoint '{}' is missing",
                actual.sequence,
                actual.hash,
                path.display()
            ),
            Self::UnsupportedCheckpointSchema(schema) => write!(
                formatter,
                "unsupported evolution audit checkpoint schema '{schema}'"
            ),
            Self::SequenceOverflow => formatter.write_str("evolution audit sequence overflow"),
        }
    }
}

impl std::error::Error for AuditError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<std::io::Error> for AuditError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Serialize)]
struct HashMaterial<'a> {
    schema: &'a str,
    sequence: u64,
    timestamp_ms: u64,
    previous_hash: &'a str,
    event: &'a EvolutionEvent,
}

fn record_hash(record: &AuditRecord) -> Result<String, AuditError> {
    let material = HashMaterial {
        schema: &record.schema,
        sequence: record.sequence,
        timestamp_ms: record.timestamp_ms,
        previous_hash: &record.previous_hash,
        event: &record.event,
    };
    let bytes = serde_json::to_vec(&material).map_err(|source| AuditError::Json {
        line: record.sequence as usize,
        source,
    })?;
    Ok(hex_digest(Sha256::digest(bytes)))
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    let bytes = bytes.as_ref();
    let mut output = String::with_capacity(bytes.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

/// Single-writer handle for a verified evolution audit log.
pub struct EvolutionAuditLog {
    path: PathBuf,
    head: AuditHead,
    checkpoint_path: Option<PathBuf>,
}

impl EvolutionAuditLog {
    /// Open an existing log, or prepare a new empty log, after verifying every
    /// record already present.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, AuditError> {
        let path = path.into();
        let head = verify_file(&path)?;
        Ok(Self {
            path,
            head,
            checkpoint_path: None,
        })
    }

    /// Open and additionally require an externally retained head checkpoint.
    /// A valid prefix created by deleting the tail passes ordinary hash-chain
    /// verification but fails this comparison.
    pub fn open_with_expected_head(
        path: impl Into<PathBuf>,
        expected: &AuditHead,
    ) -> Result<Self, AuditError> {
        let log = Self::open(path)?;
        if &log.head != expected {
            return Err(AuditError::ExpectedHeadMismatch {
                expected: expected.clone(),
                actual: log.head,
            });
        }
        Ok(log)
    }

    /// Open a log using a mandatory independently persisted head checkpoint.
    ///
    /// A checkpoint is initialized only for a new, empty log. If a non-empty
    /// log has no checkpoint, or the stored head differs from the verified log
    /// head, opening fails closed. Put `checkpoint_path` in a separate trust
    /// domain when protection against an attacker who can rewrite the audit
    /// directory is required.
    pub fn open_with_checkpoint(
        path: impl Into<PathBuf>,
        checkpoint_path: impl Into<PathBuf>,
    ) -> Result<Self, AuditError> {
        let mut log = Self::open(path)?;
        let checkpoint_path = checkpoint_path.into();
        let checkpoint = match read_checkpoint(&checkpoint_path) {
            Ok(checkpoint) => checkpoint,
            Err(AuditError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                if log.head != AuditHead::genesis() {
                    return Err(AuditError::MissingCheckpoint {
                        path: checkpoint_path,
                        actual: log.head,
                    });
                }
                write_checkpoint(&checkpoint_path, &log.head)?;
                AuditCheckpoint {
                    schema: CHECKPOINT_SCHEMA.to_owned(),
                    head: log.head.clone(),
                }
            }
            Err(error) => return Err(error),
        };
        if checkpoint.head != log.head {
            return Err(AuditError::ExpectedHeadMismatch {
                expected: checkpoint.head,
                actual: log.head,
            });
        }
        log.checkpoint_path = Some(checkpoint_path);
        Ok(log)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn head(&self) -> &AuditHead {
        &self.head
    }

    pub fn verify(&self) -> Result<AuditHead, AuditError> {
        verify_file(&self.path)
    }

    pub fn append(&mut self, event: EvolutionEvent) -> Result<AuditRecord, AuditError> {
        self.append_at(event, now_ms())
    }

    /// Append with an explicit timestamp. This is useful when the coordinator
    /// already owns the event time and for deterministic replay fixtures.
    pub fn append_at(
        &mut self,
        event: EvolutionEvent,
        timestamp_ms: u64,
    ) -> Result<AuditRecord, AuditError> {
        let actual = verify_file(&self.path)?;
        if actual != self.head {
            return Err(AuditError::HeadChanged {
                expected: self.head.clone(),
                actual,
            });
        }

        let sequence = self
            .head
            .sequence
            .checked_add(1)
            .ok_or(AuditError::SequenceOverflow)?;
        let mut record = AuditRecord {
            schema: AUDIT_SCHEMA.to_owned(),
            sequence,
            timestamp_ms,
            previous_hash: self.head.hash.clone(),
            event,
            hash: String::new(),
        };
        record.hash = record_hash(&record)?;

        let mut bytes = serde_json::to_vec(&record).map_err(|source| AuditError::Json {
            line: sequence as usize,
            source,
        })?;
        bytes.push(b'\n');

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(&bytes)?;
        file.flush()?;
        file.sync_data()?;

        self.head = AuditHead {
            sequence,
            hash: record.hash.clone(),
        };
        if let Some(path) = &self.checkpoint_path {
            write_checkpoint(path, &self.head)?;
        }
        Ok(record)
    }
}

fn read_checkpoint(path: &Path) -> Result<AuditCheckpoint, AuditError> {
    let bytes = std::fs::read(path)?;
    let checkpoint: AuditCheckpoint =
        serde_json::from_slice(&bytes).map_err(|source| AuditError::Json { line: 0, source })?;
    if checkpoint.schema != CHECKPOINT_SCHEMA {
        return Err(AuditError::UnsupportedCheckpointSchema(checkpoint.schema));
    }
    Ok(checkpoint)
}

fn write_checkpoint(path: &Path, head: &AuditHead) -> Result<(), AuditError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let checkpoint = AuditCheckpoint {
        schema: CHECKPOINT_SCHEMA.to_owned(),
        head: head.clone(),
    };
    let mut bytes =
        serde_json::to_vec(&checkpoint).map_err(|source| AuditError::Json { line: 0, source })?;
    bytes.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)?;
    file.write_all(&bytes)?;
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

/// Verify a complete JSONL chain and return its current head.
pub fn verify_file(path: &Path) -> Result<AuditHead, AuditError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AuditHead::genesis())
        }
        Err(error) => return Err(AuditError::Io(error)),
    };
    let mut head = AuditHead::genesis();
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line_number = index + 1;
        let line = line?;
        let record: AuditRecord =
            serde_json::from_str(&line).map_err(|source| AuditError::Json {
                line: line_number,
                source,
            })?;
        if record.schema != AUDIT_SCHEMA {
            return Err(AuditError::UnsupportedSchema {
                line: line_number,
                schema: record.schema,
            });
        }
        let expected_sequence = head
            .sequence
            .checked_add(1)
            .ok_or(AuditError::SequenceOverflow)?;
        if record.sequence != expected_sequence {
            return Err(AuditError::SequenceMismatch {
                line: line_number,
                expected: expected_sequence,
                actual: record.sequence,
            });
        }
        if record.previous_hash != head.hash {
            return Err(AuditError::PreviousHashMismatch {
                line: line_number,
                expected: head.hash,
                actual: record.previous_hash,
            });
        }
        let expected_hash = record_hash(&record)?;
        if record.hash != expected_hash {
            return Err(AuditError::RecordHashMismatch {
                line: line_number,
                expected: expected_hash,
                actual: record.hash,
            });
        }
        head = AuditHead {
            sequence: record.sequence,
            hash: record.hash,
        };
    }
    Ok(head)
}

/// Read records only after verifying the complete chain.
pub fn read_verified_records(path: &Path) -> Result<Vec<AuditRecord>, AuditError> {
    verify_file(path)?;
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(AuditError::Io(error)),
    };
    BufReader::new(file)
        .lines()
        .enumerate()
        .map(|(index, line)| {
            let line = line?;
            serde_json::from_str(&line).map_err(|source| AuditError::Json {
                line: index + 1,
                source,
            })
        })
        .collect()
}
