//! Governed, measurable promotion of self-evolving IRIS policy functions.
//!
//! Three reviewed hot-swap ABIs are supported: the RC1 scalar ABI
//! `def policy(x: i64) -> i64`, the control-law ABI
//! `def policy(a: f64, ..., h: f64) -> f64` with one to eight inputs, and the
//! record ABI `def policy(input: In) -> Out` where both records have only
//! `f64`, `i64` and `bool` fields. Records are heap-backed in IRIS IR, so a
//! record policy cannot carry the allocation-free proof; collections are not
//! accepted at the boundary at all. Candidates pass the compiler, resource and ABI
//! checks (optionally including that proof), repeatable tests, a
//! transactional rollback probe, a pinned trusted constitution, a shadow
//! canary comparison, and an activation check before they become the active
//! generation.

pub mod artifact;
pub mod audit;
pub mod daemon;
pub mod ffi;
pub mod fitness;
pub mod float_search;
pub mod genome;
pub mod live;
pub mod manifest;
pub mod map_elites;
pub mod mutation;
mod native;
pub mod program;
pub mod serve;
pub mod unrestricted;

pub use daemon::{run_service_daemon, DaemonConfig};
pub use fitness::{FitnessConfig, FitnessEvaluator, FitnessScore, WeightedCanaryCase};
pub use float_search::{
    FloatBinary, FloatGene, FloatGenerationSummary, FloatScore, FloatSearchConfig,
    FloatSearchResult, FloatUnary,
};
pub use genome::{BinaryOp, GeneNode, GeneType, GenomeState, UnaryOp};
pub use map_elites::{
    EliteEntry, MapElitesArchive, MapElitesConfig, MapElitesSearch, NicheCoordinate,
};
pub use mutation::{
    crossover, point_mutate, shrink_mutate, subtree_mutate, tune_constants_gradient,
    EvolutionConfig, EvolutionSearchResult, EvolutionarySearch, GenerationSummary,
};

use std::collections::{BTreeMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::codegen::hot_swap::{HotSwapEngine, SwapReceipt};
use crate::error::CodegenError;
use crate::interp::{InterpOptions, IrValue};
use crate::ir::instr::IrInstr;
use crate::ir::module::IrModule;
use crate::ir::types::{DType, IrType};

use audit::{EvolutionAuditLog, EvolutionEvent, EvolutionEventKind};
use native::NativeObservations;

pub const EVOLUTION_POLICY_SCHEMA: &str = "iris-evolution-policy/1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvolutionGate {
    LanguageSafety,
    ResourceAndAbi,
    DeterministicTests,
    TransactionRollback,
    SafetyConstitution,
    ShadowCanary,
    VerifiedActivation,
}

impl EvolutionGate {
    pub const ALL: [Self; 7] = [
        Self::LanguageSafety,
        Self::ResourceAndAbi,
        Self::DeterministicTests,
        Self::TransactionRollback,
        Self::SafetyConstitution,
        Self::ShadowCanary,
        Self::VerifiedActivation,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::LanguageSafety => "language_safety",
            Self::ResourceAndAbi => "resource_and_abi",
            Self::DeterministicTests => "deterministic_tests",
            Self::TransactionRollback => "transaction_rollback",
            Self::SafetyConstitution => "safety_constitution",
            Self::ShadowCanary => "shadow_canary",
            Self::VerifiedActivation => "verified_activation",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceLimits {
    pub max_source_bytes: usize,
    pub max_functions: usize,
    pub max_blocks: usize,
    pub max_instructions: usize,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: 512 * 1024,
            max_functions: 512,
            max_blocks: 10_000,
            max_instructions: 250_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceUsage {
    pub source_bytes: usize,
    pub functions: usize,
    pub blocks: usize,
    pub instructions: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanaryCase {
    pub input: i64,
    pub expected: i64,
}

/// The host-facing signature an evolved policy must have.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PolicyAbi {
    /// `(i64) -> i64`, the RC1 ABI.
    #[default]
    ScalarI64,
    /// `(f64 x inputs) -> f64` with `1 <= inputs <= MAX_CONTROL_INPUTS`.
    ControlF64 { inputs: usize },
    /// `(In) -> Out` where `In` and `Out` are records whose fields are all
    /// `f64`, `i64` or `bool`. The record types are the policy's own: they
    /// come from its signature, and the baseline and candidate must agree.
    Record,
}

pub const MAX_CONTROL_INPUTS: usize = 8;

impl PolicyAbi {
    fn is_scalar_i64(&self) -> bool {
        *self == Self::ScalarI64
    }

    /// The exact signature, for the scalar ABIs (a record ABI's types come
    /// from the policy itself).
    fn signature(self) -> Option<(Vec<IrType>, IrType)> {
        match self {
            Self::ScalarI64 => Some((vec![IrType::Scalar(DType::I64)], IrType::Scalar(DType::I64))),
            Self::ControlF64 { inputs } => Some((
                vec![IrType::Scalar(DType::F64); inputs],
                IrType::Scalar(DType::F64),
            )),
            Self::Record => None,
        }
    }

    pub fn describe(self) -> String {
        match self {
            Self::ScalarI64 => "(i64) -> i64".to_owned(),
            Self::ControlF64 { inputs } => {
                format!("({}) -> f64", vec!["f64"; inputs].join(", "))
            }
            Self::Record => "(record) -> record, with f64/i64/bool fields".to_owned(),
        }
    }
}

/// A canary case for the [`PolicyAbi::ControlF64`] ABI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlCanaryCase {
    pub inputs: Vec<f64>,
    pub expected: f64,
}

pub use crate::codegen::llvm_orc::RecordField;

/// A canary case for the [`PolicyAbi::Record`] ABI: field name to value, as
/// JSON. Every field of the record type must be present, and no other.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordCanaryCase {
    pub input: serde_json::Map<String, serde_json::Value>,
    pub expected: serde_json::Map<String, serde_json::Value>,
}

/// A record crossing the policy boundary: its fields, in declaration order.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordData {
    pub fields: Vec<(String, RecordField)>,
}

impl RecordData {
    pub fn get(&self, name: &str) -> Option<RecordField> {
        self.fields
            .iter()
            .find(|(field, _)| field == name)
            .map(|(_, value)| *value)
    }

    fn values(&self) -> Vec<RecordField> {
        self.fields.iter().map(|(_, value)| *value).collect()
    }

    /// Every `f64` field is finite.
    pub fn is_finite(&self) -> bool {
        self.fields
            .iter()
            .all(|(_, value)| !matches!(value, RecordField::F64(v) if !v.is_finite()))
    }

    /// Read `json` as a record of type `ty`: every field present, of its type.
    pub fn from_json(
        ty: &IrType,
        json: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, String> {
        let IrType::Struct { name, fields } = ty else {
            return Err(format!("{ty} is not a record"));
        };
        if let Some(extra) = json
            .keys()
            .find(|key| !fields.iter().any(|(field, _)| field == *key))
        {
            return Err(format!("record {name} has no field '{extra}'"));
        }
        let mut values = Vec::with_capacity(fields.len());
        for (field, field_ty) in fields {
            let value = json
                .get(field)
                .ok_or_else(|| format!("field '{field}' of record {name} is missing"))?;
            let converted = match field_ty {
                IrType::Scalar(DType::F64) => value.as_f64().map(RecordField::F64),
                IrType::Scalar(DType::I64) => value.as_i64().map(RecordField::I64),
                IrType::Scalar(DType::Bool) => value.as_bool().map(RecordField::Bool),
                other => {
                    return Err(format!(
                        "field '{field}' of record {name} has type {other}, which cannot cross the boundary"
                    ))
                }
            }
            .ok_or_else(|| format!("field '{field}' of record {name} must be a {field_ty}, found {value}"))?;
            values.push((field.clone(), converted));
        }
        Ok(Self { fields: values })
    }

    pub(super) fn from_values(ty: &IrType, values: Vec<RecordField>) -> Result<Self, String> {
        let IrType::Struct { name, fields } = ty else {
            return Err(format!("{ty} is not a record"));
        };
        if values.len() != fields.len() {
            return Err(format!(
                "record {name} has {} fields, got {}",
                fields.len(),
                values.len()
            ));
        }
        Ok(Self {
            fields: fields
                .iter()
                .map(|(field, _)| field.clone())
                .zip(values)
                .collect(),
        })
    }
}

impl std::fmt::Display for RecordData {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("{")?;
        for (index, (field, value)) in self.fields.iter().enumerate() {
            if index > 0 {
                formatter.write_str(", ")?;
            }
            write!(formatter, "\"{field}\": {value}")?;
        }
        formatter.write_str("}")
    }
}

/// ABI-erased input used by the shared gate sequence.
#[derive(Debug, Clone, PartialEq)]
enum PolicyInput {
    I64(i64),
    F64(Vec<f64>),
    Record(RecordData),
}

#[derive(Debug, Clone, PartialEq)]
enum PolicyOutput {
    I64(i64),
    F64(f64),
    Record(RecordData),
}

impl std::fmt::Display for PolicyInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::I64(value) => write!(formatter, "{value}"),
            Self::F64(values) => write!(formatter, "{values:?}"),
            Self::Record(record) => write!(formatter, "{record}"),
        }
    }
}

impl std::fmt::Display for PolicyOutput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::I64(value) => write!(formatter, "{value}"),
            Self::F64(value) => write!(formatter, "{value:?}"),
            Self::Record(record) => write!(formatter, "{record}"),
        }
    }
}

/// The policy's input and output record types, for the record ABI.
pub(super) fn record_types(
    module: &IrModule,
    function: &str,
) -> Result<(IrType, IrType), EvolutionError> {
    let function_def = module
        .function_by_name(function)
        .ok_or_else(|| EvolutionError::Gate {
            gate: EvolutionGate::ResourceAndAbi,
            detail: format!("candidate does not define '{function}'"),
        })?;
    let [param] = function_def.params.as_slice() else {
        return Err(EvolutionError::Gate {
            gate: EvolutionGate::ResourceAndAbi,
            detail: format!("'{function}' must take exactly one record"),
        });
    };
    for ty in [&param.ty, &function_def.return_ty] {
        crate::codegen::llvm_orc::record_layout(ty).map_err(|error| EvolutionError::Gate {
            gate: EvolutionGate::ResourceAndAbi,
            detail: format!("'{function}' does not have the record ABI: {error}"),
        })?;
    }
    Ok((param.ty.clone(), function_def.return_ty.clone()))
}

fn zero_record(ty: &IrType) -> Vec<RecordField> {
    match ty {
        IrType::Struct { fields, .. } => fields
            .iter()
            .filter_map(|(_, field_ty)| RecordField::zero(field_ty))
            .collect(),
        _ => Vec::new(),
    }
}

fn record_to_ir(record: &RecordData) -> IrValue {
    IrValue::Struct(
        record
            .fields
            .iter()
            .map(|(_, value)| match value {
                RecordField::F64(v) => IrValue::F64(*v),
                RecordField::I64(v) => IrValue::I64(*v),
                RecordField::Bool(v) => IrValue::Bool(*v),
            })
            .collect(),
    )
}

fn record_from_ir(ty: &IrType, value: &IrValue) -> Result<RecordData, String> {
    let IrValue::Struct(values) = value else {
        return Err(format!("expected a record, got {value:?}"));
    };
    let fields = values
        .iter()
        .map(|value| match value {
            IrValue::F64(v) => Ok(RecordField::F64(*v)),
            IrValue::I64(v) => Ok(RecordField::I64(*v)),
            IrValue::Bool(v) => Ok(RecordField::Bool(*v)),
            other => Err(format!("record field {other:?} cannot cross the boundary")),
        })
        .collect::<Result<Vec<_>, _>>()?;
    RecordData::from_values(ty, fields)
}

struct PolicyCase {
    input: PolicyInput,
    expected: PolicyOutput,
}

/// Scale of the integer error metric for `f64` policies: errors are counted
/// in billionths so [`CanaryMetrics`] stays exact, ordered and hashable.
const F64_ERROR_SCALE: f64 = 1e9;

fn is_false(value: &bool) -> bool {
    !*value
}

/// Where candidate code runs while the gates gather evidence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ValidationBackend {
    /// The bounded IR interpreter, inside the coordinator process.
    #[default]
    Interpreter,
    /// A natively compiled harness run inside the platform worker sandbox,
    /// under these limits. Fails closed where no sandbox is enforced.
    NativeSandbox {
        limits: crate::sandbox::ResourceLimits,
    },
}

impl ValidationBackend {
    fn is_interpreter(&self) -> bool {
        *self == Self::Interpreter
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvolutionPolicy {
    pub schema: String,
    pub function: String,
    pub required_tests: Vec<String>,
    pub rollback_probe: String,
    pub deterministic_repetitions: usize,
    pub min_improved_cases: usize,
    pub max_interpreter_steps: usize,
    pub max_interpreter_depth: usize,
    pub resources: ResourceLimits,
    /// Omitted from serialization at its default so the hashes of existing
    /// policies, already recorded in audit logs, do not change.
    #[serde(default, skip_serializing_if = "PolicyAbi::is_scalar_i64")]
    pub abi: PolicyAbi,
    /// Require an IR-level proof that the policy's whole call graph performs
    /// no heap allocation, host service call or recursion. The proof reads
    /// compiled IR, not effect annotations.
    #[serde(default, skip_serializing_if = "is_false")]
    pub require_allocation_free: bool,
    #[serde(default, skip_serializing_if = "ValidationBackend::is_interpreter")]
    pub validation: ValidationBackend,
}

impl Default for EvolutionPolicy {
    fn default() -> Self {
        Self {
            schema: EVOLUTION_POLICY_SCHEMA.to_owned(),
            function: "policy".to_owned(),
            required_tests: vec!["test_policy".to_owned()],
            rollback_probe: "test_transaction_rollback".to_owned(),
            deterministic_repetitions: 2,
            min_improved_cases: 1,
            max_interpreter_steps: 1_000_000,
            max_interpreter_depth: 250,
            resources: ResourceLimits::default(),
            abi: PolicyAbi::ScalarI64,
            require_allocation_free: false,
            validation: ValidationBackend::Interpreter,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedConstitution {
    pub source: Vec<u8>,
    pub expected_sha256: String,
}

impl TrustedConstitution {
    pub fn pinned(source: impl Into<Vec<u8>>) -> Self {
        let source = source.into();
        let expected_sha256 = sha256_hex(&source);
        Self {
            source,
            expected_sha256,
        }
    }

    pub fn verify(&self) -> Result<String, EvolutionError> {
        let actual = sha256_hex(&self.source);
        if actual != self.expected_sha256 {
            return Err(EvolutionError::Gate {
                gate: EvolutionGate::SafetyConstitution,
                detail: format!(
                    "trusted constitution hash mismatch: expected {}, found {}",
                    self.expected_sha256, actual
                ),
            });
        }
        Ok(actual)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanaryMetrics {
    pub cases: usize,
    pub baseline_exact: usize,
    pub candidate_exact: usize,
    pub improved_cases: usize,
    pub baseline_absolute_error: u128,
    pub candidate_absolute_error: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateEvidence {
    pub gate: EvolutionGate,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionReceipt {
    pub swap: SwapReceipt,
    pub candidate_hash: String,
    pub constitution_hash: String,
    pub policy_hash: String,
    pub resources: ResourceUsage,
    pub canary: CanaryMetrics,
    pub gates: Vec<GateEvidence>,
}

#[derive(Debug)]
pub enum EvolutionError {
    InvalidPolicy(String),
    Compile(String),
    Runtime(String),
    Audit(String),
    Gate { gate: EvolutionGate, detail: String },
}

impl std::fmt::Display for EvolutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPolicy(detail) => write!(formatter, "invalid evolution policy: {detail}"),
            Self::Compile(detail) => write!(formatter, "candidate compilation failed: {detail}"),
            Self::Runtime(detail) => write!(formatter, "candidate runtime failed: {detail}"),
            Self::Audit(detail) => write!(formatter, "evolution audit failed: {detail}"),
            Self::Gate { gate, detail } => {
                write!(
                    formatter,
                    "evolution gate '{}' failed: {detail}",
                    gate.name()
                )
            }
        }
    }
}

impl std::error::Error for EvolutionError {}

impl From<CodegenError> for EvolutionError {
    fn from(error: CodegenError) -> Self {
        Self::Runtime(error.to_string())
    }
}

/// Compiles an untrusted candidate with the complete compiler pipeline and
/// request-local strict effect checking.
pub fn compile_candidate(source: &str, module_name: &str) -> Result<IrModule, EvolutionError> {
    crate::compile_to_module_strict(source, module_name)
        .map_err(|error| EvolutionError::Compile(error.to_string()))
}

/// Compile a candidate file with imports resolved and strict effects enabled.
pub fn compile_candidate_file(path: &std::path::Path) -> Result<IrModule, EvolutionError> {
    crate::compile_file_to_module_strict(path)
        .map_err(|error| EvolutionError::Compile(error.to_string()))
}

/// The single authority that can activate or roll back evolved generations.
pub struct EvolutionCoordinator {
    swaps: HotSwapEngine,
    policy: EvolutionPolicy,
}

impl EvolutionCoordinator {
    pub fn new(policy: EvolutionPolicy) -> Result<Self, EvolutionError> {
        Self::with_hot_swap_config(policy, crate::codegen::hot_swap::HotSwapConfig::from_env())
    }

    pub fn with_hot_swap_config(
        policy: EvolutionPolicy,
        config: crate::codegen::hot_swap::HotSwapConfig,
    ) -> Result<Self, EvolutionError> {
        validate_policy(&policy)?;
        Ok(Self {
            swaps: HotSwapEngine::with_config(config),
            policy,
        })
    }

    pub fn hot_swap(&self) -> &HotSwapEngine {
        &self.swaps
    }

    /// Establish the trusted baseline. A baseline is still materialized and
    /// called before installation, so unresolved symbols cannot become active.
    pub fn install_baseline(&self, module: &IrModule) -> Result<SwapReceipt, EvolutionError> {
        validate_policy_abi(module, &self.policy)?;
        let function = &self.policy.function;
        let abi = self.policy.abi;
        let zero_input = match abi {
            PolicyAbi::Record => zero_record(&record_types(module, function)?.0),
            _ => Vec::new(),
        };
        self.swaps
            .install_verified(module, function, |loaded| match abi {
                PolicyAbi::ScalarI64 => loaded.call_i64_1(function, 0).map(|_| ()),
                PolicyAbi::ControlF64 { inputs } => {
                    loaded.call_f64_n(function, &vec![0.0; inputs]).map(|_| ())
                }
                PolicyAbi::Record => loaded.call_record(function, &zero_input).map(|_| ()),
            })
            .map_err(EvolutionError::from)
    }

    /// Evaluate and atomically activate a candidate after all seven gates pass.
    /// The constitution callback belongs to trusted host code; candidate code
    /// cannot replace it. Returning `false` rejects that input/output pair.
    pub fn evaluate_and_promote<F>(
        &self,
        source: &str,
        module: &IrModule,
        cases: &[CanaryCase],
        constitution: &TrustedConstitution,
        constitution_allows: F,
        audit: Option<&mut EvolutionAuditLog>,
    ) -> Result<PromotionReceipt, EvolutionError>
    where
        F: Fn(i64, i64) -> bool,
    {
        self.require_abi(PolicyAbi::ScalarI64, "scalar_i64")?;
        let cases = cases
            .iter()
            .map(|case| PolicyCase {
                input: PolicyInput::I64(case.input),
                expected: PolicyOutput::I64(case.expected),
            })
            .collect::<Vec<_>>();
        self.promote(
            source,
            module,
            &cases,
            constitution,
            &|input, output| match (input, output) {
                (PolicyInput::I64(input), PolicyOutput::I64(output)) => {
                    constitution_allows(*input, *output)
                }
                _ => false,
            },
            audit,
        )
    }

    /// [`Self::evaluate_and_promote`] for the [`PolicyAbi::ControlF64`] ABI.
    /// A non-finite candidate output fails the constitution gate before the
    /// host callback is consulted.
    pub fn evaluate_and_promote_control<F>(
        &self,
        source: &str,
        module: &IrModule,
        cases: &[ControlCanaryCase],
        constitution: &TrustedConstitution,
        constitution_allows: F,
        audit: Option<&mut EvolutionAuditLog>,
    ) -> Result<PromotionReceipt, EvolutionError>
    where
        F: Fn(&[f64], f64) -> bool,
    {
        let PolicyAbi::ControlF64 { inputs } = self.policy.abi else {
            return Err(self.abi_mismatch("control_f64"));
        };
        let mut converted = Vec::with_capacity(cases.len());
        for case in cases {
            if case.inputs.len() != inputs {
                return Err(EvolutionError::InvalidPolicy(format!(
                    "canary case has {} inputs, policy ABI takes {inputs}",
                    case.inputs.len()
                )));
            }
            converted.push(PolicyCase {
                input: PolicyInput::F64(case.inputs.clone()),
                expected: PolicyOutput::F64(case.expected),
            });
        }
        self.promote(
            source,
            module,
            &converted,
            constitution,
            &|input, output| match (input, output) {
                (PolicyInput::F64(input), PolicyOutput::F64(output)) => {
                    output.is_finite() && constitution_allows(input, *output)
                }
                _ => false,
            },
            audit,
        )
    }

    /// [`Self::evaluate_and_promote`] for the [`PolicyAbi::Record`] ABI. Case
    /// fields are read with the candidate's record types. A non-finite `f64`
    /// output field fails the constitution gate before the host callback is
    /// consulted.
    pub fn evaluate_and_promote_record<F>(
        &self,
        source: &str,
        module: &IrModule,
        cases: &[RecordCanaryCase],
        constitution: &TrustedConstitution,
        constitution_allows: F,
        audit: Option<&mut EvolutionAuditLog>,
    ) -> Result<PromotionReceipt, EvolutionError>
    where
        F: Fn(&RecordData, &RecordData) -> bool,
    {
        self.require_abi(PolicyAbi::Record, "record")?;
        let (input_ty, output_ty) = record_types(module, &self.policy.function)?;
        let mut converted = Vec::with_capacity(cases.len());
        for (index, case) in cases.iter().enumerate() {
            let invalid = |error: String| {
                EvolutionError::InvalidPolicy(format!("canary case {index}: {error}"))
            };
            converted.push(PolicyCase {
                input: PolicyInput::Record(
                    RecordData::from_json(&input_ty, &case.input).map_err(invalid)?,
                ),
                expected: PolicyOutput::Record(
                    RecordData::from_json(&output_ty, &case.expected).map_err(invalid)?,
                ),
            });
        }
        self.promote(
            source,
            module,
            &converted,
            constitution,
            &|input, output| match (input, output) {
                (PolicyInput::Record(input), PolicyOutput::Record(output)) => {
                    output.is_finite() && constitution_allows(input, output)
                }
                _ => false,
            },
            audit,
        )
    }

    fn require_abi(&self, expected: PolicyAbi, called: &str) -> Result<(), EvolutionError> {
        if self.policy.abi == expected {
            Ok(())
        } else {
            Err(self.abi_mismatch(called))
        }
    }

    fn abi_mismatch(&self, called: &str) -> EvolutionError {
        EvolutionError::InvalidPolicy(format!(
            "{called} entry point called on a policy whose ABI is {}",
            self.policy.abi.describe()
        ))
    }

    /// The seven-gate sequence, shared by every ABI.
    fn promote(
        &self,
        source: &str,
        module: &IrModule,
        cases: &[PolicyCase],
        constitution: &TrustedConstitution,
        constitution_allows: &dyn Fn(&PolicyInput, &PolicyOutput) -> bool,
        mut audit: Option<&mut EvolutionAuditLog>,
    ) -> Result<PromotionReceipt, EvolutionError> {
        let candidate_hash = sha256_hex(source.as_bytes());
        let policy_hash = sha256_hex(
            &serde_json::to_vec(&self.policy)
                .map_err(|error| EvolutionError::InvalidPolicy(error.to_string()))?,
        );
        append_event(
            audit.as_deref_mut(),
            EvolutionEventKind::CandidateReceived,
            &candidate_hash,
            None,
            None,
            &policy_hash,
            BTreeMap::new(),
        )?;

        macro_rules! require_gate {
            ($gate:expr, $result:expr) => {
                match $result {
                    Ok(value) => value,
                    Err(error) => {
                        record_failed_gate(
                            audit.as_deref_mut(),
                            &candidate_hash,
                            &policy_hash,
                            $gate,
                            &error.to_string(),
                        );
                        return Err(error);
                    }
                }
            };
        }

        let mut gates = Vec::with_capacity(EvolutionGate::ALL.len());
        let roots = validation_roots(&self.policy);
        validate_reachable_safe_subset(module, &roots)
            .and_then(|_| validate_policy_abi(module, &self.policy))
            .map_err(|error| {
                record_failed_gate(
                    audit.as_deref_mut(),
                    &candidate_hash,
                    &policy_hash,
                    EvolutionGate::LanguageSafety,
                    &error.to_string(),
                );
                error
            })?;
        pass_gate(
            &mut gates,
            audit.as_deref_mut(),
            &candidate_hash,
            &policy_hash,
            EvolutionGate::LanguageSafety,
            "full compiler pipeline, strict effects, and safe-subset reachability passed",
        )?;

        let resources = measure_resources(source, module);
        require_gate!(
            EvolutionGate::ResourceAndAbi,
            enforce_resources(&resources, &self.policy.resources)
        );
        let allocation_proof = if self.policy.require_allocation_free {
            let proof = require_gate!(
                EvolutionGate::ResourceAndAbi,
                crate::codegen::embedded::prove_allocation_free(module, &self.policy.function)
                    .map_err(|error| EvolutionError::Gate {
                        gate: EvolutionGate::ResourceAndAbi,
                        detail: format!("allocation-free proof failed: {error}"),
                    })
            );
            format!(
                "; allocation-free proof over {} reachable function{} ({} fixed-array bytes)",
                proof.reachable_functions.len(),
                if proof.reachable_functions.len() == 1 {
                    ""
                } else {
                    "s"
                },
                proof.fixed_array_bytes
            )
        } else {
            String::new()
        };
        pass_gate(
            &mut gates,
            audit.as_deref_mut(),
            &candidate_hash,
            &policy_hash,
            EvolutionGate::ResourceAndAbi,
            &format!(
                "{} bytes, {} functions, {} blocks, {} instructions; ABI {}{}",
                resources.source_bytes,
                resources.functions,
                resources.blocks,
                resources.instructions,
                self.policy.abi.describe(),
                allocation_proof
            ),
        )?;

        let native = match self.policy.validation {
            ValidationBackend::Interpreter => None,
            ValidationBackend::NativeSandbox { limits } => Some(require_gate!(
                EvolutionGate::DeterministicTests,
                native::observe(module, &self.policy, cases, limits)
            )),
        };
        require_gate!(
            EvolutionGate::DeterministicTests,
            run_deterministic_tests(module, &self.policy, native.as_ref())
        );
        pass_gate(
            &mut gates,
            audit.as_deref_mut(),
            &candidate_hash,
            &policy_hash,
            EvolutionGate::DeterministicTests,
            &format!(
                "{} assertion-backed tests repeated {} times; {}",
                self.policy.required_tests.len(),
                self.policy.deterministic_repetitions,
                native
                    .as_ref()
                    .map_or("bounded interpreter", |observed| observed.usage.as_str())
            ),
        )?;

        let rollback_result = require_gate!(
            EvolutionGate::TransactionRollback,
            probe_runs(
                module,
                &self.policy.rollback_probe,
                &self.policy,
                native.as_ref(),
                1
            )
            .map(|runs| runs[0])
        );
        if rollback_result != 0 {
            let error = EvolutionError::Gate {
                gate: EvolutionGate::TransactionRollback,
                detail: format!(
                    "probe '{}' returned {} instead of 0",
                    self.policy.rollback_probe, rollback_result
                ),
            };
            record_failed_gate(
                audit.as_deref_mut(),
                &candidate_hash,
                &policy_hash,
                EvolutionGate::TransactionRollback,
                &error.to_string(),
            );
            return Err(error);
        }
        pass_gate(
            &mut gates,
            audit.as_deref_mut(),
            &candidate_hash,
            &policy_hash,
            EvolutionGate::TransactionRollback,
            "speculative state probe returned 0 after rollback",
        )?;

        let constitution_hash =
            require_gate!(EvolutionGate::SafetyConstitution, constitution.verify());
        for (index, case) in cases.iter().enumerate() {
            let output = match &native {
                Some(observed) => observed.outputs[index][0].clone(),
                None => require_gate!(
                    EvolutionGate::SafetyConstitution,
                    interpret_policy(module, &self.policy, &case.input)
                ),
            };
            if !constitution_allows(&case.input, &output) {
                let error = EvolutionError::Gate {
                    gate: EvolutionGate::SafetyConstitution,
                    detail: format!(
                        "trusted constitution rejected input {} output {}",
                        case.input, output
                    ),
                };
                record_failed_gate(
                    audit.as_deref_mut(),
                    &candidate_hash,
                    &policy_hash,
                    EvolutionGate::SafetyConstitution,
                    &error.to_string(),
                );
                return Err(error);
            }
        }
        pass_gate_with_constitution(
            &mut gates,
            audit.as_deref_mut(),
            &candidate_hash,
            &policy_hash,
            &constitution_hash,
            EvolutionGate::SafetyConstitution,
            &format!("pinned constitution accepted {} cases", cases.len()),
        )?;

        let baseline = require_gate!(
            EvolutionGate::ShadowCanary,
            self.swaps
                .lease(&self.policy.function)
                .map_err(|_| EvolutionError::Gate {
                    gate: EvolutionGate::ShadowCanary,
                    detail: "no trusted baseline generation is installed".to_owned(),
                })
        );
        let canary = require_gate!(
            EvolutionGate::ShadowCanary,
            compare_canary(&baseline, module, &self.policy, cases, native.as_ref())
        );
        if canary.improved_cases < self.policy.min_improved_cases
            || canary.candidate_absolute_error >= canary.baseline_absolute_error
        {
            let error = EvolutionError::Gate {
                gate: EvolutionGate::ShadowCanary,
                detail: format!(
                    "candidate error {} did not improve baseline error {} across at least {} cases",
                    canary.candidate_absolute_error,
                    canary.baseline_absolute_error,
                    self.policy.min_improved_cases
                ),
            };
            record_failed_gate(
                audit.as_deref_mut(),
                &candidate_hash,
                &policy_hash,
                EvolutionGate::ShadowCanary,
                &error.to_string(),
            );
            return Err(error);
        }
        pass_gate(
            &mut gates,
            audit.as_deref_mut(),
            &candidate_hash,
            &policy_hash,
            EvolutionGate::ShadowCanary,
            &format!(
                "exact {}/{} vs baseline {}/{}; absolute error {} vs {}",
                canary.candidate_exact,
                canary.cases,
                canary.baseline_exact,
                canary.cases,
                canary.candidate_absolute_error,
                canary.baseline_absolute_error
            ),
        )?;

        let function = &self.policy.function;
        let swap = require_gate!(
            EvolutionGate::VerifiedActivation,
            self.swaps
                .install_verified(module, function, |_| Ok(()))
                .map_err(|error| EvolutionError::Gate {
                    gate: EvolutionGate::VerifiedActivation,
                    detail: error.to_string(),
                })
        );

        // Candidate behavior was exercised in the bounded interpreter above.
        // Activation verifies that ORC materialized the candidate and that the
        // stable dispatch slot points at this exact generation. It deliberately
        // does not execute untrusted native code inside the coordinator process.
        let active_generation = self.swaps.active_generation(function);
        if active_generation != Some(swap.generation) {
            let _ = self.swaps.rollback_generation(function, swap.generation);
            let error = EvolutionError::Gate {
                gate: EvolutionGate::VerifiedActivation,
                detail: format!(
                    "active generation changed during activation: expected {}, found {:?}",
                    swap.generation, active_generation
                ),
            };
            record_failed_gate(
                audit.as_deref_mut(),
                &candidate_hash,
                &policy_hash,
                EvolutionGate::VerifiedActivation,
                &error.to_string(),
            );
            return Err(error);
        }
        pass_gate(
            &mut gates,
            audit.as_deref_mut(),
            &candidate_hash,
            &policy_hash,
            EvolutionGate::VerifiedActivation,
            &format!(
                "generation {} materialized and installed in the verified dispatch slot",
                swap.generation
            ),
        )?;

        let mut evidence = BTreeMap::new();
        evidence.insert("generation".to_owned(), swap.generation.to_string());
        append_event(
            audit,
            EvolutionEventKind::Promoted,
            &candidate_hash,
            Some(swap.generation),
            Some(&constitution_hash),
            &policy_hash,
            evidence,
        )?;

        Ok(PromotionReceipt {
            swap,
            candidate_hash,
            constitution_hash,
            policy_hash,
            resources,
            canary,
            gates,
        })
    }

    /// Record a post-activation observation and automatically roll back the
    /// observed generation when it violates the trusted constitution.
    pub fn observe_or_rollback<F>(
        &self,
        receipt: &PromotionReceipt,
        input: i64,
        output: i64,
        constitution_allows: F,
        audit: Option<&mut EvolutionAuditLog>,
    ) -> Result<bool, EvolutionError>
    where
        F: Fn(i64, i64) -> bool,
    {
        self.require_abi(PolicyAbi::ScalarI64, "scalar_i64")?;
        let allowed = constitution_allows(input, output);
        self.record_observation(
            receipt,
            &PolicyInput::I64(input),
            PolicyOutput::I64(output),
            allowed,
            audit,
        )
    }

    /// [`Self::observe_or_rollback`] for the [`PolicyAbi::ControlF64`] ABI.
    /// A non-finite output always violates the constitution.
    pub fn observe_or_rollback_control<F>(
        &self,
        receipt: &PromotionReceipt,
        inputs: &[f64],
        output: f64,
        constitution_allows: F,
        audit: Option<&mut EvolutionAuditLog>,
    ) -> Result<bool, EvolutionError>
    where
        F: Fn(&[f64], f64) -> bool,
    {
        if !matches!(self.policy.abi, PolicyAbi::ControlF64 { .. }) {
            return Err(self.abi_mismatch("control_f64"));
        }
        let allowed = output.is_finite() && constitution_allows(inputs, output);
        self.record_observation(
            receipt,
            &PolicyInput::F64(inputs.to_vec()),
            PolicyOutput::F64(output),
            allowed,
            audit,
        )
    }

    /// [`Self::observe_or_rollback`] for the [`PolicyAbi::Record`] ABI. A
    /// non-finite `f64` output field always violates the constitution.
    pub fn observe_or_rollback_record<F>(
        &self,
        receipt: &PromotionReceipt,
        input: &RecordData,
        output: &RecordData,
        constitution_allows: F,
        audit: Option<&mut EvolutionAuditLog>,
    ) -> Result<bool, EvolutionError>
    where
        F: Fn(&RecordData, &RecordData) -> bool,
    {
        self.require_abi(PolicyAbi::Record, "record")?;
        let allowed = output.is_finite() && constitution_allows(input, output);
        self.record_observation(
            receipt,
            &PolicyInput::Record(input.clone()),
            PolicyOutput::Record(output.clone()),
            allowed,
            audit,
        )
    }

    fn record_observation(
        &self,
        receipt: &PromotionReceipt,
        input: &PolicyInput,
        output: PolicyOutput,
        allowed: bool,
        mut audit: Option<&mut EvolutionAuditLog>,
    ) -> Result<bool, EvolutionError> {
        let mut evidence = BTreeMap::new();
        evidence.insert("input".to_owned(), input.to_string());
        evidence.insert("output".to_owned(), output.to_string());
        evidence.insert("allowed".to_owned(), allowed.to_string());
        append_event(
            audit.as_deref_mut(),
            EvolutionEventKind::Observation,
            &receipt.candidate_hash,
            Some(receipt.swap.generation),
            Some(&receipt.constitution_hash),
            &receipt.policy_hash,
            evidence,
        )?;
        if allowed {
            return Ok(false);
        }
        let rollback = self
            .swaps
            .rollback_generation(&self.policy.function, receipt.swap.generation)?;
        let mut evidence = BTreeMap::new();
        evidence.insert(
            "active_generation".to_owned(),
            rollback.generation.to_string(),
        );
        append_event(
            audit,
            EvolutionEventKind::RolledBack,
            &receipt.candidate_hash,
            Some(receipt.swap.generation),
            Some(&receipt.constitution_hash),
            &receipt.policy_hash,
            evidence,
        )?;
        Ok(true)
    }
}

fn validate_policy(policy: &EvolutionPolicy) -> Result<(), EvolutionError> {
    if policy.schema != EVOLUTION_POLICY_SCHEMA {
        return Err(EvolutionError::InvalidPolicy(format!(
            "expected schema {}, found {}",
            EVOLUTION_POLICY_SCHEMA, policy.schema
        )));
    }
    if policy.function.is_empty()
        || policy.required_tests.is_empty()
        || policy.rollback_probe.is_empty()
        || policy.deterministic_repetitions < 2
        || policy.max_interpreter_steps == 0
        || policy.max_interpreter_depth == 0
    {
        return Err(EvolutionError::InvalidPolicy(
            "function, tests, rollback probe, and at least two repetitions are required".into(),
        ));
    }
    if let PolicyAbi::ControlF64 { inputs } = policy.abi {
        if !(1..=MAX_CONTROL_INPUTS).contains(&inputs) {
            return Err(EvolutionError::InvalidPolicy(format!(
                "control_f64 ABI takes 1..={MAX_CONTROL_INPUTS} inputs, found {inputs}"
            )));
        }
    }
    if policy.abi == PolicyAbi::Record && policy.require_allocation_free {
        return Err(EvolutionError::InvalidPolicy(
            "a record policy allocates the record it returns, so it cannot carry the \
             allocation-free proof"
                .into(),
        ));
    }
    Ok(())
}

fn validate_policy_abi(module: &IrModule, policy: &EvolutionPolicy) -> Result<(), EvolutionError> {
    let function = &policy.function;
    let function_def = module
        .function_by_name(function)
        .ok_or_else(|| EvolutionError::Gate {
            gate: EvolutionGate::ResourceAndAbi,
            detail: format!("candidate does not define '{function}'"),
        })?;
    let Some((params, result)) = policy.abi.signature() else {
        return record_types(module, function).map(|_| ());
    };
    if function_def.params.len() != params.len()
        || function_def
            .params
            .iter()
            .zip(&params)
            .any(|(param, expected)| &param.ty != expected)
        || function_def.return_ty != result
    {
        return Err(EvolutionError::Gate {
            gate: EvolutionGate::ResourceAndAbi,
            detail: format!(
                "'{function}' must have the ABI {}, found ({}) -> {}",
                policy.abi.describe(),
                function_def
                    .params
                    .iter()
                    .map(|param| param.ty.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
                function_def.return_ty
            ),
        });
    }
    Ok(())
}

fn validation_roots(policy: &EvolutionPolicy) -> Vec<String> {
    let mut roots = Vec::with_capacity(policy.required_tests.len() + 2);
    roots.push(policy.function.clone());
    roots.extend(policy.required_tests.iter().cloned());
    roots.push(policy.rollback_probe.clone());
    roots
}

fn validate_reachable_safe_subset(
    module: &IrModule,
    roots: &[String],
) -> Result<(), EvolutionError> {
    let mut queue = VecDeque::from(roots.to_vec());
    let mut visited = HashSet::new();
    while let Some(name) = queue.pop_front() {
        if !visited.insert(name.clone()) {
            continue;
        }
        let function = module
            .function_by_name(&name)
            .ok_or_else(|| EvolutionError::Gate {
                gate: EvolutionGate::LanguageSafety,
                detail: format!("required reachable function '{name}' is missing"),
            })?;
        for block in function.blocks() {
            for instruction in &block.instrs {
                if let IrInstr::Call { callee, .. } = instruction {
                    if module.function_by_name(callee).is_some() {
                        queue.push_back(callee.clone());
                    }
                }
                if let Some(reason) = forbidden_reason(instruction) {
                    return Err(EvolutionError::Gate {
                        gate: EvolutionGate::LanguageSafety,
                        detail: format!("function '{name}' contains {reason}"),
                    });
                }
            }
        }
    }
    Ok(())
}

fn forbidden_reason(instruction: &IrInstr) -> Option<String> {
    match instruction {
        IrInstr::CallExtern { name, .. } => Some(format!("external call '{name}'")),
        IrInstr::CallClosure { .. } => Some("an effect-opaque closure call".into()),
        IrInstr::DynCall { .. } => Some("effect-opaque dynamic dispatch".into()),
        IrInstr::Print { .. } => Some("console output".into()),
        IrInstr::ReadLine { .. } | IrInstr::ReadI64 { .. } | IrInstr::ReadF64 { .. } => {
            Some("interactive input".into())
        }
        IrInstr::FileReadAll { .. }
        | IrInstr::FileWriteAll { .. }
        | IrInstr::FileExists { .. }
        | IrInstr::FileLines { .. } => Some("filesystem access".into()),
        IrInstr::DbOpen { .. }
        | IrInstr::DbExec { .. }
        | IrInstr::DbExecParams { .. }
        | IrInstr::DbQuery { .. }
        | IrInstr::DbQueryParams { .. }
        | IrInstr::DbClose { .. } => Some("database access".into()),
        IrInstr::ProcessExit { .. } | IrInstr::ProcessArgs { .. } | IrInstr::EnvVar { .. } => {
            Some("process or environment access".into())
        }
        IrInstr::TcpConnect { .. }
        | IrInstr::TcpListen { .. }
        | IrInstr::TcpAccept { .. }
        | IrInstr::TcpRead { .. }
        | IrInstr::TcpWrite { .. }
        | IrInstr::TcpClose { .. } => Some("network access".into()),
        IrInstr::NowMs { .. } | IrInstr::SleepMs { .. } => Some("wall-clock access".into()),
        IrInstr::Spawn { .. }
        | IrInstr::TaskGroupNew { .. }
        | IrInstr::TaskGroupSpawn { .. }
        | IrInstr::TaskGroupJoin { .. }
        | IrInstr::TaskGroupCancel { .. }
        | IrInstr::ChanNew { .. }
        | IrInstr::ChanSend { .. }
        | IrInstr::ChanRecv { .. }
        | IrInstr::ParFor { .. } => Some("concurrent execution".into()),
        IrInstr::BuiltinCall { name, .. }
            if !matches!(
                name.as_str(),
                "transaction_begin"
                    | "transaction_commit"
                    | "transaction_rollback"
                    | "transaction_depth"
                    // The program's own state: deterministic, and handed
                    // over between generations by the coordinator.
                    | "state_get"
                    | "state_set"
            ) =>
        {
            Some(format!("unapproved runtime builtin '{name}'"))
        }
        _ => None,
    }
}

fn measure_resources(source: &str, module: &IrModule) -> ResourceUsage {
    ResourceUsage {
        source_bytes: source.len(),
        functions: module.functions().len(),
        blocks: module
            .functions()
            .iter()
            .map(|function| function.blocks().len())
            .sum(),
        instructions: module
            .functions()
            .iter()
            .flat_map(|function| function.blocks())
            .map(|block| block.instrs.len())
            .sum(),
    }
}

fn enforce_resources(usage: &ResourceUsage, limits: &ResourceLimits) -> Result<(), EvolutionError> {
    let checks = [
        ("source bytes", usage.source_bytes, limits.max_source_bytes),
        ("functions", usage.functions, limits.max_functions),
        ("blocks", usage.blocks, limits.max_blocks),
        ("instructions", usage.instructions, limits.max_instructions),
    ];
    for (name, actual, maximum) in checks {
        if actual > maximum {
            return Err(EvolutionError::Gate {
                gate: EvolutionGate::ResourceAndAbi,
                detail: format!("{name} {actual} exceeds configured maximum {maximum}"),
            });
        }
    }
    Ok(())
}

/// `count` results of a zero-argument probe: read from the sandboxed run when
/// there is one, otherwise interpreted now.
fn probe_runs(
    module: &IrModule,
    probe: &str,
    policy: &EvolutionPolicy,
    native: Option<&NativeObservations>,
    count: usize,
) -> Result<Vec<i64>, EvolutionError> {
    match native {
        Some(observed) => observed
            .probes
            .get(probe)
            .filter(|runs| runs.len() >= count)
            .map(|runs| runs[..count].to_vec())
            .ok_or_else(|| EvolutionError::Runtime(format!("no native result for '{probe}'"))),
        None => (0..count)
            .map(|_| interpret_i64_0(module, probe, policy))
            .collect(),
    }
}

fn run_deterministic_tests(
    module: &IrModule,
    policy: &EvolutionPolicy,
    native: Option<&NativeObservations>,
) -> Result<(), EvolutionError> {
    for test in &policy.required_tests {
        let runs = probe_runs(
            module,
            test,
            policy,
            native,
            policy.deterministic_repetitions,
        )?;
        let first = runs[0];
        if first != 0 {
            return Err(EvolutionError::Gate {
                gate: EvolutionGate::DeterministicTests,
                detail: format!("assertion-backed test '{test}' returned {first}"),
            });
        }
        for (repetition, &next) in runs.iter().enumerate().skip(1) {
            if next != first {
                return Err(EvolutionError::Gate {
                    gate: EvolutionGate::DeterministicTests,
                    detail: format!(
                        "test '{test}' changed from {first} to {next} at repetition {}",
                        repetition + 1
                    ),
                });
            }
        }
    }
    Ok(())
}

fn compare_canary(
    baseline: &crate::codegen::hot_swap::HotSwapLease,
    candidate: &IrModule,
    policy: &EvolutionPolicy,
    cases: &[PolicyCase],
    native: Option<&NativeObservations>,
) -> Result<CanaryMetrics, EvolutionError> {
    if cases.is_empty() {
        return Err(EvolutionError::Gate {
            gate: EvolutionGate::ShadowCanary,
            detail: "at least one canary case is required".into(),
        });
    }
    let mut metrics = CanaryMetrics {
        cases: cases.len(),
        baseline_exact: 0,
        candidate_exact: 0,
        improved_cases: 0,
        baseline_absolute_error: 0,
        candidate_absolute_error: 0,
    };
    for (index, case) in cases.iter().enumerate() {
        let baseline_output = match (&case.input, &case.expected) {
            (PolicyInput::I64(input), _) => PolicyOutput::I64(baseline.call_i64_1(*input)?),
            (PolicyInput::F64(inputs), _) => PolicyOutput::F64(baseline.call_f64_n(inputs)?),
            (PolicyInput::Record(input), PolicyOutput::Record(expected)) => {
                let values = baseline.call_record(&input.values())?;
                if values.len() != expected.fields.len() {
                    return Err(EvolutionError::Gate {
                        gate: EvolutionGate::ShadowCanary,
                        detail: "the baseline returns a different record type".into(),
                    });
                }
                PolicyOutput::Record(RecordData {
                    fields: expected
                        .fields
                        .iter()
                        .map(|(field, _)| field.clone())
                        .zip(values)
                        .collect(),
                })
            }
            (PolicyInput::Record(_), _) => {
                return Err(EvolutionError::Runtime(
                    "internal error: a record case expects a non-record output".into(),
                ))
            }
        };
        let runs = match native {
            Some(observed) => observed.outputs[index].clone(),
            None => (0..policy.deterministic_repetitions)
                .map(|_| interpret_policy(candidate, policy, &case.input))
                .collect::<Result<Vec<_>, _>>()?,
        };
        let candidate_output = runs[0].clone();
        for (repetition, repeated) in runs.iter().enumerate().skip(1) {
            if !same_output(repeated, &candidate_output) {
                return Err(EvolutionError::Gate {
                    gate: EvolutionGate::DeterministicTests,
                    detail: format!(
                        "policy output for input {} changed from {} to {} at repetition {}",
                        case.input,
                        candidate_output,
                        repeated,
                        repetition + 1
                    ),
                });
            }
        }
        let baseline_error = absolute_error(&baseline_output, &case.expected);
        let candidate_error = absolute_error(&candidate_output, &case.expected);
        metrics.baseline_absolute_error += baseline_error;
        metrics.candidate_absolute_error += candidate_error;
        metrics.baseline_exact += usize::from(baseline_error == 0);
        metrics.candidate_exact += usize::from(candidate_error == 0);
        metrics.improved_cases += usize::from(candidate_error < baseline_error);
    }
    Ok(metrics)
}

fn interpret_i64_0(
    module: &IrModule,
    function: &str,
    policy: &EvolutionPolicy,
) -> Result<i64, EvolutionError> {
    interpret_i64(module, function, &[], policy)
}

fn interpret_policy(
    module: &IrModule,
    policy: &EvolutionPolicy,
    input: &PolicyInput,
) -> Result<PolicyOutput, EvolutionError> {
    let function = &policy.function;
    match input {
        PolicyInput::I64(value) => {
            interpret_i64(module, function, &[IrValue::I64(*value)], policy).map(PolicyOutput::I64)
        }
        PolicyInput::F64(values) => {
            let args = values.iter().copied().map(IrValue::F64).collect::<Vec<_>>();
            match interpret_values(module, function, &args, policy)?.as_slice() {
                [IrValue::F64(value)] => Ok(PolicyOutput::F64(*value)),
                other => Err(EvolutionError::Runtime(format!(
                    "'{function}' returned {:?}, expected one f64",
                    other
                ))),
            }
        }
        PolicyInput::Record(record) => {
            let (_, output_ty) = record_types(module, function)?;
            match interpret_values(module, function, &[record_to_ir(record)], policy)?.as_slice() {
                [value] => record_from_ir(&output_ty, value)
                    .map(PolicyOutput::Record)
                    .map_err(|error| EvolutionError::Runtime(format!("'{function}': {error}"))),
                other => Err(EvolutionError::Runtime(format!(
                    "'{function}' returned {:?}, expected one record",
                    other
                ))),
            }
        }
    }
}

/// Bitwise equality, so a policy that returns the same NaN twice still
/// counts as deterministic. (The constitution rejects NaN separately.)
fn same_output(left: &PolicyOutput, right: &PolicyOutput) -> bool {
    match (left, right) {
        (PolicyOutput::I64(left), PolicyOutput::I64(right)) => left == right,
        (PolicyOutput::F64(left), PolicyOutput::F64(right)) => left.to_bits() == right.to_bits(),
        (PolicyOutput::Record(left), PolicyOutput::Record(right)) => {
            left.fields.len() == right.fields.len()
                && left
                    .fields
                    .iter()
                    .zip(&right.fields)
                    .all(|((_, a), (_, b))| match (a, b) {
                        (RecordField::F64(a), RecordField::F64(b)) => a.to_bits() == b.to_bits(),
                        _ => a == b,
                    })
        }
        _ => false,
    }
}

fn interpret_values(
    module: &IrModule,
    function: &str,
    args: &[IrValue],
    policy: &EvolutionPolicy,
) -> Result<Vec<IrValue>, EvolutionError> {
    let function_def = module
        .function_by_name(function)
        .ok_or_else(|| EvolutionError::Runtime(format!("function '{function}' is missing")))?;
    crate::interp::eval_function_in_module_opts(
        module,
        function_def,
        args,
        InterpOptions {
            max_steps: policy.max_interpreter_steps,
            max_depth: policy.max_interpreter_depth,
        },
    )
    .map_err(|error| EvolutionError::Runtime(format!("'{function}': {error}")))
}

fn interpret_i64(
    module: &IrModule,
    function: &str,
    args: &[IrValue],
    policy: &EvolutionPolicy,
) -> Result<i64, EvolutionError> {
    match interpret_values(module, function, args, policy)?.as_slice() {
        [IrValue::I64(value)] => Ok(*value),
        other => Err(EvolutionError::Runtime(format!(
            "'{function}' returned {:?}, expected one i64",
            other
        ))),
    }
}

fn absolute_error(actual: &PolicyOutput, expected: &PolicyOutput) -> u128 {
    fn f64_error(actual: f64, expected: f64) -> u128 {
        let scaled = (actual - expected).abs() * F64_ERROR_SCALE;
        // NaN and overflow saturate to the worst error, never the best.
        if scaled.is_finite() && scaled < u128::MAX as f64 {
            scaled.round() as u128
        } else {
            u128::MAX
        }
    }
    match (actual, expected) {
        (PolicyOutput::I64(actual), PolicyOutput::I64(expected)) => {
            (i128::from(*actual) - i128::from(*expected)).unsigned_abs()
        }
        (PolicyOutput::F64(actual), PolicyOutput::F64(expected)) => f64_error(*actual, *expected),
        // The sum over fields: an f64 field in the f64 scale, an i64 field
        // exactly, and a wrong bool as one whole unit in the f64 scale.
        (PolicyOutput::Record(actual), PolicyOutput::Record(expected)) => {
            if actual.fields.len() != expected.fields.len() {
                return u128::MAX;
            }
            actual
                .fields
                .iter()
                .zip(&expected.fields)
                .map(|((_, a), (_, b))| match (a, b) {
                    (RecordField::F64(a), RecordField::F64(b)) => f64_error(*a, *b),
                    (RecordField::I64(a), RecordField::I64(b)) => {
                        (i128::from(*a) - i128::from(*b)).unsigned_abs()
                    }
                    (RecordField::Bool(a), RecordField::Bool(b)) => {
                        if a == b {
                            0
                        } else {
                            F64_ERROR_SCALE as u128
                        }
                    }
                    _ => u128::MAX,
                })
                .fold(0u128, u128::saturating_add)
        }
        _ => u128::MAX,
    }
}

fn pass_gate(
    gates: &mut Vec<GateEvidence>,
    audit: Option<&mut EvolutionAuditLog>,
    candidate_hash: &str,
    policy_hash: &str,
    gate: EvolutionGate,
    detail: &str,
) -> Result<(), EvolutionError> {
    pass_gate_with_constitution(gates, audit, candidate_hash, policy_hash, "", gate, detail)
}

fn pass_gate_with_constitution(
    gates: &mut Vec<GateEvidence>,
    audit: Option<&mut EvolutionAuditLog>,
    candidate_hash: &str,
    policy_hash: &str,
    constitution_hash: &str,
    gate: EvolutionGate,
    detail: &str,
) -> Result<(), EvolutionError> {
    gates.push(GateEvidence {
        gate,
        detail: detail.to_owned(),
    });
    let mut evidence = BTreeMap::new();
    evidence.insert("gate".to_owned(), gate.name().to_owned());
    evidence.insert("detail".to_owned(), detail.to_owned());
    append_event(
        audit,
        EvolutionEventKind::GatePassed,
        candidate_hash,
        None,
        (!constitution_hash.is_empty()).then_some(constitution_hash),
        policy_hash,
        evidence,
    )
}

fn record_failed_gate(
    audit: Option<&mut EvolutionAuditLog>,
    candidate_hash: &str,
    policy_hash: &str,
    gate: EvolutionGate,
    detail: &str,
) {
    let mut evidence = BTreeMap::new();
    evidence.insert("gate".to_owned(), gate.name().to_owned());
    evidence.insert("detail".to_owned(), detail.to_owned());
    let _ = append_event(
        audit,
        EvolutionEventKind::GateFailed,
        candidate_hash,
        None,
        None,
        policy_hash,
        evidence,
    );
}

fn append_event(
    audit: Option<&mut EvolutionAuditLog>,
    kind: EvolutionEventKind,
    candidate_hash: &str,
    generation: Option<u64>,
    constitution_hash: Option<&str>,
    policy_hash: &str,
    evidence: BTreeMap<String, String>,
) -> Result<(), EvolutionError> {
    let Some(audit) = audit else {
        return Ok(());
    };
    let mut event = EvolutionEvent::new(kind, candidate_hash);
    event.generation = generation;
    event.constitution_hash = constitution_hash.map(ToOwned::to_owned);
    event.policy_hash = Some(policy_hash.to_owned());
    event.gate = evidence.get("gate").cloned();
    event.evidence = evidence;
    audit
        .append(event)
        .map(|_| ())
        .map_err(|error| EvolutionError::Audit(error.to_string()))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(output, "{byte:02x}");
    }
    output
}
