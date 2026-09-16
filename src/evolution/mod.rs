//! Governed, measurable promotion of self-evolving IRIS policy functions.
//!
//! RC1 deliberately supports a narrow hot-swap ABI: `def policy(x: i64) ->
//! i64`. Candidates pass the compiler, resource and ABI checks, repeatable
//! tests, a transactional rollback probe, a pinned trusted constitution, a
//! shadow canary comparison, and an activation check before they become the
//! active generation.

pub mod artifact;
pub mod audit;
pub mod daemon;
pub mod ffi;
pub mod fitness;
pub mod genome;
pub mod manifest;
pub mod map_elites;
pub mod mutation;
pub mod unrestricted;

pub use daemon::run_service_daemon;
pub use fitness::{FitnessConfig, FitnessEvaluator, FitnessScore, WeightedCanaryCase};
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
        validate_policy(&policy)?;
        Ok(Self {
            swaps: HotSwapEngine::new(),
            policy,
        })
    }

    pub fn hot_swap(&self) -> &HotSwapEngine {
        &self.swaps
    }

    /// Establish the trusted baseline. A baseline is still materialized and
    /// called before installation, so unresolved symbols cannot become active.
    pub fn install_baseline(&self, module: &IrModule) -> Result<SwapReceipt, EvolutionError> {
        validate_policy_abi(module, &self.policy.function)?;
        let function = &self.policy.function;
        self.swaps
            .install_verified(module, function, |loaded| {
                loaded.call_i64_1(function, 0).map(|_| ())
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
        mut audit: Option<&mut EvolutionAuditLog>,
    ) -> Result<PromotionReceipt, EvolutionError>
    where
        F: Fn(i64, i64) -> bool,
    {
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
            .and_then(|_| validate_policy_abi(module, &self.policy.function))
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
        pass_gate(
            &mut gates,
            audit.as_deref_mut(),
            &candidate_hash,
            &policy_hash,
            EvolutionGate::ResourceAndAbi,
            &format!(
                "{} bytes, {} functions, {} blocks, {} instructions; ABI (i64) -> i64",
                resources.source_bytes,
                resources.functions,
                resources.blocks,
                resources.instructions
            ),
        )?;

        require_gate!(
            EvolutionGate::DeterministicTests,
            run_deterministic_tests(module, &self.policy)
        );
        pass_gate(
            &mut gates,
            audit.as_deref_mut(),
            &candidate_hash,
            &policy_hash,
            EvolutionGate::DeterministicTests,
            &format!(
                "{} assertion-backed tests repeated {} times",
                self.policy.required_tests.len(),
                self.policy.deterministic_repetitions
            ),
        )?;

        let rollback_result = require_gate!(
            EvolutionGate::TransactionRollback,
            interpret_i64_0(module, &self.policy.rollback_probe, &self.policy)
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
        for case in cases {
            let output = require_gate!(
                EvolutionGate::SafetyConstitution,
                interpret_i64_1(module, &self.policy.function, case.input, &self.policy)
            );
            if !constitution_allows(case.input, output) {
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
            compare_canary(&baseline, module, &self.policy, cases)
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
        mut audit: Option<&mut EvolutionAuditLog>,
    ) -> Result<bool, EvolutionError>
    where
        F: Fn(i64, i64) -> bool,
    {
        let allowed = constitution_allows(input, output);
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
    Ok(())
}

fn validate_policy_abi(module: &IrModule, function: &str) -> Result<(), EvolutionError> {
    let function_def = module
        .function_by_name(function)
        .ok_or_else(|| EvolutionError::Gate {
            gate: EvolutionGate::ResourceAndAbi,
            detail: format!("candidate does not define '{function}'"),
        })?;
    let i64_ty = IrType::Scalar(DType::I64);
    if function_def.params.len() != 1
        || function_def.params[0].ty != i64_ty
        || function_def.return_ty != i64_ty
    {
        return Err(EvolutionError::Gate {
            gate: EvolutionGate::ResourceAndAbi,
            detail: format!(
                "'{function}' must have the RC ABI (i64) -> i64, found ({}) -> {}",
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

fn run_deterministic_tests(
    module: &IrModule,
    policy: &EvolutionPolicy,
) -> Result<(), EvolutionError> {
    for test in &policy.required_tests {
        let first = interpret_i64_0(module, test, policy)?;
        if first != 0 {
            return Err(EvolutionError::Gate {
                gate: EvolutionGate::DeterministicTests,
                detail: format!("assertion-backed test '{test}' returned {first}"),
            });
        }
        for repetition in 1..policy.deterministic_repetitions {
            let next = interpret_i64_0(module, test, policy)?;
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
    cases: &[CanaryCase],
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
    for case in cases {
        let baseline_output = baseline.call_i64_1(case.input)?;
        let candidate_output = interpret_i64_1(candidate, &policy.function, case.input, policy)?;
        for repetition in 1..policy.deterministic_repetitions {
            let repeated = interpret_i64_1(candidate, &policy.function, case.input, policy)?;
            if repeated != candidate_output {
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
        let baseline_error = absolute_error(baseline_output, case.expected);
        let candidate_error = absolute_error(candidate_output, case.expected);
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

fn interpret_i64_1(
    module: &IrModule,
    function: &str,
    input: i64,
    policy: &EvolutionPolicy,
) -> Result<i64, EvolutionError> {
    interpret_i64(module, function, &[IrValue::I64(input)], policy)
}

fn interpret_i64(
    module: &IrModule,
    function: &str,
    args: &[IrValue],
    policy: &EvolutionPolicy,
) -> Result<i64, EvolutionError> {
    let function_def = module
        .function_by_name(function)
        .ok_or_else(|| EvolutionError::Runtime(format!("function '{function}' is missing")))?;
    let values = crate::interp::eval_function_in_module_opts(
        module,
        function_def,
        args,
        InterpOptions {
            max_steps: policy.max_interpreter_steps,
            max_depth: policy.max_interpreter_depth,
        },
    )
    .map_err(|error| EvolutionError::Runtime(format!("'{function}': {error}")))?;
    match values.as_slice() {
        [IrValue::I64(value)] => Ok(*value),
        other => Err(EvolutionError::Runtime(format!(
            "'{function}' returned {:?}, expected one i64",
            other
        ))),
    }
}

fn absolute_error(actual: i64, expected: i64) -> u128 {
    (i128::from(actual) - i128::from(expected)).unsigned_abs()
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
