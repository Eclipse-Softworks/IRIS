//! Versioned whole-program evolution contracts.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::codegen::hot_swap::ProgramGateway;
use crate::ir::module::IrModule;
use crate::ir::types::{DType, IrType};

pub const PROGRAM_MANIFEST_SCHEMA: &str = "iris-evolution-program/2";

/// Stable host-facing ABIs. Candidate internals may use the complete IRIS
/// language, but promotion exposes only these reviewed gateway shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostAbi {
    JsonV1,
    CommandV1,
    ScalarI64V1,
}

impl HostAbi {
    pub fn signature(self) -> (&'static [IrType], IrType) {
        const NONE: &[IrType] = &[];
        match self {
            Self::JsonV1 => (&[IrType::Str], IrType::Str),
            Self::CommandV1 => (NONE, IrType::Scalar(DType::I64)),
            Self::ScalarI64V1 => (&[IrType::Scalar(DType::I64)], IrType::Scalar(DType::I64)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramEntrypoint {
    pub gateway: String,
    pub symbol: String,
    pub abi: HostAbi,
}

/// Explicit state handoff between generations. Snapshots are canonical JSON;
/// import returns zero on success and a nonzero status on rejection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateContract {
    pub schema: String,
    pub snapshot_symbol: String,
    pub import_symbol: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramManifest {
    pub schema: String,
    pub program_id: String,
    pub entrypoints: Vec<ProgramEntrypoint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<StateContract>,
}

impl ProgramManifest {
    pub fn validate(&self, module: &IrModule) -> Result<(), ManifestError> {
        if self.schema != PROGRAM_MANIFEST_SCHEMA {
            return Err(ManifestError::Schema(self.schema.clone()));
        }
        validate_name("program id", &self.program_id)?;
        if self.entrypoints.is_empty() {
            return Err(ManifestError::EmptyEntrypoints);
        }
        let mut gateways = BTreeSet::new();
        let mut symbols = BTreeSet::new();
        for entrypoint in &self.entrypoints {
            validate_name("gateway", &entrypoint.gateway)?;
            validate_name("gateway symbol", &entrypoint.symbol)?;
            if !gateways.insert(entrypoint.gateway.clone()) {
                return Err(ManifestError::DuplicateGateway(entrypoint.gateway.clone()));
            }
            if !symbols.insert(entrypoint.symbol.clone()) {
                return Err(ManifestError::DuplicateSymbol(entrypoint.symbol.clone()));
            }
            let (params, result) = entrypoint.abi.signature();
            validate_signature(module, &entrypoint.symbol, params, &result)?;
        }
        if let Some(state) = &self.state {
            if state.schema.trim().is_empty() {
                return Err(ManifestError::InvalidName("state schema"));
            }
            validate_name("state snapshot symbol", &state.snapshot_symbol)?;
            validate_name("state import symbol", &state.import_symbol)?;
            validate_signature(module, &state.snapshot_symbol, &[], &IrType::Str)?;
            validate_signature(
                module,
                &state.import_symbol,
                &[IrType::Str],
                &IrType::Scalar(DType::I64),
            )?;
        }
        Ok(())
    }

    pub fn gateways(&self) -> Vec<ProgramGateway> {
        self.entrypoints
            .iter()
            .map(|entrypoint| {
                ProgramGateway::new(entrypoint.gateway.clone(), entrypoint.symbol.clone())
            })
            .collect()
    }
}

fn validate_signature(
    module: &IrModule,
    symbol: &str,
    params: &[IrType],
    result: &IrType,
) -> Result<(), ManifestError> {
    let function = module
        .function_by_name(symbol)
        .ok_or_else(|| ManifestError::MissingSymbol(symbol.to_owned()))?;
    let actual_params: Vec<_> = function
        .params
        .iter()
        .map(|param| param.ty.clone())
        .collect();
    if actual_params != params || &function.return_ty != result {
        return Err(ManifestError::Abi {
            symbol: symbol.to_owned(),
            expected: format_signature(params, result),
            actual: format_signature(&actual_params, &function.return_ty),
        });
    }
    Ok(())
}

fn format_signature(params: &[IrType], result: &IrType) -> String {
    format!(
        "({}) -> {result}",
        params
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn validate_name(kind: &'static str, value: &str) -> Result<(), ManifestError> {
    let valid = !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(ManifestError::InvalidName(kind))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    Schema(String),
    InvalidName(&'static str),
    EmptyEntrypoints,
    DuplicateGateway(String),
    DuplicateSymbol(String),
    MissingSymbol(String),
    Abi {
        symbol: String,
        expected: String,
        actual: String,
    },
}

impl fmt::Display for ManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Schema(schema) => {
                write!(formatter, "unsupported program manifest schema '{schema}'")
            }
            Self::InvalidName(kind) => write!(formatter, "invalid {kind}"),
            Self::EmptyEntrypoints => formatter.write_str("program manifest has no entrypoints"),
            Self::DuplicateGateway(name) => write!(formatter, "duplicate gateway '{name}'"),
            Self::DuplicateSymbol(name) => write!(formatter, "duplicate gateway symbol '{name}'"),
            Self::MissingSymbol(name) => {
                write!(formatter, "program gateway symbol '{name}' is missing")
            }
            Self::Abi {
                symbol,
                expected,
                actual,
            } => write!(
                formatter,
                "program gateway '{symbol}' has ABI {actual}; expected {expected}"
            ),
        }
    }
}

impl std::error::Error for ManifestError {}
