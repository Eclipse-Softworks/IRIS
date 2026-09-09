//! Immutable, content-addressed candidate source artifacts.

use std::fmt;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::manifest::ProgramManifest;

pub const ARTIFACT_SCHEMA: &str = "iris-evolution-artifact/1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceArtifact {
    pub schema: String,
    pub artifact_sha256: String,
    pub source_sha256: String,
    pub manifest_sha256: String,
}

impl SourceArtifact {
    pub fn build(source: &[u8], manifest: &ProgramManifest) -> Result<Self, ArtifactError> {
        let manifest_bytes = serde_json::to_vec(manifest).map_err(ArtifactError::Json)?;
        let source_sha256 = digest(source);
        let manifest_sha256 = digest(&manifest_bytes);
        let mut material = Vec::with_capacity(source.len() + manifest_bytes.len() + 32);
        material.extend_from_slice(b"iris-evolution-artifact-v1\0");
        material.extend_from_slice(&(source.len() as u64).to_be_bytes());
        material.extend_from_slice(source);
        material.extend_from_slice(&(manifest_bytes.len() as u64).to_be_bytes());
        material.extend_from_slice(&manifest_bytes);
        Ok(Self {
            schema: ARTIFACT_SCHEMA.to_owned(),
            artifact_sha256: digest(&material),
            source_sha256,
            manifest_sha256,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ArtifactStore {
    root: PathBuf,
}

impl ArtifactStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn persist(
        &self,
        source: &[u8],
        manifest: &ProgramManifest,
    ) -> Result<SourceArtifact, ArtifactError> {
        let artifact = SourceArtifact::build(source, manifest)?;
        let directory = self.root.join(&artifact.artifact_sha256);
        std::fs::create_dir_all(&directory)?;
        write_immutable(&directory.join("source.iris"), source)?;
        let manifest_bytes = serde_json::to_vec_pretty(manifest).map_err(ArtifactError::Json)?;
        write_immutable(&directory.join("manifest.json"), &manifest_bytes)?;
        let record = serde_json::to_vec_pretty(&artifact).map_err(ArtifactError::Json)?;
        write_immutable(&directory.join("artifact.json"), &record)?;
        Ok(artifact)
    }

    pub fn artifact_dir(&self, sha256: &str) -> PathBuf {
        self.root.join(sha256)
    }
}

fn write_immutable(path: &Path, bytes: &[u8]) -> Result<(), ArtifactError> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            file.write_all(bytes)?;
            file.flush()?;
            file.sync_all()?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if std::fs::read(path)? == bytes {
                Ok(())
            } else {
                Err(ArtifactError::Collision(path.to_owned()))
            }
        }
        Err(error) => Err(ArtifactError::Io(error)),
    }
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Debug)]
pub enum ArtifactError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Collision(PathBuf),
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "artifact store I/O error: {error}"),
            Self::Json(error) => write!(formatter, "artifact JSON error: {error}"),
            Self::Collision(path) => write!(
                formatter,
                "content-addressed artifact path '{}' contains different bytes",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ArtifactError {}

impl From<std::io::Error> for ArtifactError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
