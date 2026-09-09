//! Explicitly unsafe, unrestricted whole-program activation.
//!
//! This authority is intentionally separate from [`super::EvolutionCoordinator`].
//! It does not apply constitutions, canaries, resource budgets, effect filters,
//! audit checkpoints, transactional probes, or sandboxing. Candidate code may
//! exercise every capability granted to the host process. The compiler and
//! gateway ABI checks remain mandatory because malformed or ABI-incompatible
//! machine code cannot be installed coherently.

use std::fmt;

use sha2::{Digest, Sha256};

use crate::codegen::hot_swap::{HotSwapEngine, ProgramLease, ProgramSwapReceipt};
use crate::ir::module::IrModule;

use super::manifest::{ManifestError, ProgramManifest};

/// Exact acknowledgement required before an unrestricted authority is issued.
pub const UNRESTRICTED_ACKNOWLEDGEMENT: &str = "IRIS_UNRESTRICTED_SELF_MODIFICATION";

/// Deliberately non-`Clone` proof that the caller acknowledged unrestricted
/// code activation for this process.
#[derive(Debug)]
pub struct UnrestrictedAuthorityToken {
    _private: (),
}

/// Turn an explicit acknowledgement phrase into an authority token.
pub fn acknowledge_unrestricted(
    acknowledgement: &str,
) -> Result<UnrestrictedAuthorityToken, UnrestrictedEvolutionError> {
    if acknowledgement == UNRESTRICTED_ACKNOWLEDGEMENT {
        Ok(UnrestrictedAuthorityToken { _private: () })
    } else {
        Err(UnrestrictedEvolutionError::Acknowledgement)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnrestrictedActivationReceipt {
    pub swap: ProgramSwapReceipt,
    pub source_sha256: String,
    pub manifest: ProgramManifest,
}

/// Process-local authority for installing arbitrary, compiler-valid IRIS
/// programs into atomic whole-program dispatch slots.
pub struct UnrestrictedEvolutionAuthority {
    swaps: HotSwapEngine,
    _token: UnrestrictedAuthorityToken,
}

impl UnrestrictedEvolutionAuthority {
    pub fn new(token: UnrestrictedAuthorityToken) -> Self {
        Self {
            swaps: HotSwapEngine::new(),
            _token: token,
        }
    }

    pub fn with_history_limit(token: UnrestrictedAuthorityToken, history_limit: usize) -> Self {
        Self {
            swaps: HotSwapEngine::with_history_limit(history_limit),
            _token: token,
        }
    }

    pub fn hot_swap(&self) -> &HotSwapEngine {
        &self.swaps
    }

    /// Compile and activate source without behavioral or capability gates.
    pub fn compile_and_activate(
        &self,
        source: &str,
        module_name: &str,
        manifest: &ProgramManifest,
    ) -> Result<UnrestrictedActivationReceipt, UnrestrictedEvolutionError> {
        let module = crate::compile_to_module(source, module_name)
            .map_err(|error| UnrestrictedEvolutionError::Compile(error.to_string()))?;
        self.activate_module(source.as_bytes(), &module, manifest)
    }

    /// Activate an already compiled program. This performs only structural
    /// manifest validation, ABI compatibility, ORC materialization, and the
    /// atomic dispatch-table switch.
    pub fn activate_module(
        &self,
        source: &[u8],
        module: &IrModule,
        manifest: &ProgramManifest,
    ) -> Result<UnrestrictedActivationReceipt, UnrestrictedEvolutionError> {
        manifest
            .validate(module)
            .map_err(UnrestrictedEvolutionError::Manifest)?;
        let swap = self
            .swaps
            .install_program_verified(module, &manifest.program_id, &manifest.gateways(), |_| {
                Ok(())
            })
            .map_err(|error| UnrestrictedEvolutionError::Activation(error.to_string()))?;
        Ok(UnrestrictedActivationReceipt {
            swap,
            source_sha256: sha256_hex(source),
            manifest: manifest.clone(),
        })
    }

    pub fn lease_program(
        &self,
        program_id: &str,
    ) -> Result<ProgramLease, UnrestrictedEvolutionError> {
        self.swaps
            .lease_program(program_id)
            .map_err(|error| UnrestrictedEvolutionError::Activation(error.to_string()))
    }

    pub fn rollback(
        &self,
        receipt: &UnrestrictedActivationReceipt,
    ) -> Result<ProgramSwapReceipt, UnrestrictedEvolutionError> {
        self.swaps
            .rollback_program_generation(&receipt.swap.program_name, receipt.swap.generation)
            .map_err(|error| UnrestrictedEvolutionError::Activation(error.to_string()))
    }
}

#[derive(Debug)]
pub enum UnrestrictedEvolutionError {
    Acknowledgement,
    Compile(String),
    Manifest(ManifestError),
    Activation(String),
}

impl fmt::Display for UnrestrictedEvolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Acknowledgement => write!(
                formatter,
                "unrestricted evolution requires acknowledgement '{UNRESTRICTED_ACKNOWLEDGEMENT}'"
            ),
            Self::Compile(detail) => write!(
                formatter,
                "unrestricted candidate failed to compile: {detail}"
            ),
            Self::Manifest(error) => write!(formatter, "unrestricted manifest is invalid: {error}"),
            Self::Activation(detail) => {
                write!(formatter, "unrestricted activation failed: {detail}")
            }
        }
    }
}

impl std::error::Error for UnrestrictedEvolutionError {}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
