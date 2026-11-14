//! Governed evolution of whole programs.
//!
//! A program is promoted through the same seven gates as a policy function,
//! but its behaviour is judged by *scenarios*: an initial program state and a
//! sequence of calls into its gateways (the reviewed entry points of its
//! [`ProgramManifest`]), each with an optional expected result. Outputs are
//! compared as JSON, so a gateway can return anything from an integer to a
//! structured reply.
//!
//! Program state (`var` at the top level) survives promotion: at activation
//! the coordinator reads the live generation's state and writes every
//! variable the candidate also declares, with the same type, into the new
//! generation before the atomic switch. New variables keep their initial
//! values; variables the candidate no longer declares are dropped. A variable
//! whose type changes is refused at the resource-and-ABI gate, so a handoff
//! never reinterprets data.
//!
//! Candidate behaviour is exercised only in the bounded interpreter; native
//! code is materialized and has its state written, but is not otherwise run
//! by the coordinator before it becomes active.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::codegen::hot_swap::{HotSwapEngine, ProgramLease, ProgramSwapReceipt};
use crate::codegen::llvm_orc::{RecordField, StateValue};
use crate::interp::{InterpOptions, IrValue};
use crate::ir::module::{IrModule, STATE_INIT_FN};
use crate::ir::types::{DType, IrType};

use super::audit::{EvolutionAuditLog, EvolutionEventKind};
use super::manifest::{HostAbi, ProgramManifest};
use super::{
    append_event, enforce_resources, measure_resources, pass_gate, pass_gate_with_constitution,
    record_failed_gate, sha256_hex, validate_reachable_safe_subset, EvolutionError, EvolutionGate,
    GateEvidence, ResourceLimits, ResourceUsage, TrustedConstitution,
};

pub const PROGRAM_POLICY_SCHEMA: &str = "iris-evolution-program-policy/1";

/// What a program candidate must satisfy, besides its manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramPolicy {
    pub schema: String,
    /// Zero-argument `i64` functions that must return 0, deterministically.
    pub required_tests: Vec<String>,
    pub rollback_probe: String,
    pub deterministic_repetitions: usize,
    /// Scenario steps on which the candidate must beat the baseline.
    pub min_improved_steps: usize,
    pub max_interpreter_steps: usize,
    pub max_interpreter_depth: usize,
    pub resources: ResourceLimits,
}

impl Default for ProgramPolicy {
    fn default() -> Self {
        Self {
            schema: PROGRAM_POLICY_SCHEMA.to_owned(),
            required_tests: vec!["test_program".to_owned()],
            rollback_probe: "test_transaction_rollback".to_owned(),
            deterministic_repetitions: 2,
            min_improved_steps: 1,
            max_interpreter_steps: 1_000_000,
            max_interpreter_depth: 250,
            resources: ResourceLimits::default(),
        }
    }
}

/// One run of a program: a starting state, then calls into its gateways.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scenario {
    pub name: String,
    /// Starting values for state variables, by name; any variable left out
    /// starts at its declared initial value.
    #[serde(default)]
    pub state: serde_json::Map<String, Value>,
    pub steps: Vec<ScenarioStep>,
}

/// One gateway call.
///
/// For a `json_v1` gateway, `input` (any JSON) is passed as its JSON text and
/// the reply is read back as JSON (a reply that is not JSON is a JSON
/// string). A `scalar_i64_v1` gateway takes an integer `input`; a
/// `command_v1` gateway takes none.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScenarioStep {
    pub gateway: String,
    #[serde(default)]
    pub input: Value,
    /// The result this step should produce; steps without one are still run
    /// and judged by the constitution, but do not count towards the canary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<Value>,
}

/// How the candidate compared with the baseline over the scenarios.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramMetrics {
    pub scenarios: usize,
    pub steps: usize,
    /// Steps with an expected result.
    pub compared_steps: usize,
    pub baseline_exact: usize,
    pub candidate_exact: usize,
    pub improved_steps: usize,
    pub baseline_error: u128,
    pub candidate_error: u128,
}

/// What happened to each state variable at activation.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StateHandoff {
    /// Copied from the replaced generation.
    pub carried: Vec<String>,
    /// New in the candidate: initial value.
    pub initialised: Vec<String>,
    /// Declared by both with the same type, but of a type handoff does not
    /// carry (a map, an option, ...): initial value.
    pub reinitialised: Vec<String>,
    /// Declared only by the replaced generation.
    pub dropped: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ProgramPromotionReceipt {
    pub swap: ProgramSwapReceipt,
    pub candidate_hash: String,
    pub constitution_hash: String,
    pub policy_hash: String,
    pub resources: ResourceUsage,
    pub metrics: ProgramMetrics,
    pub handoff: StateHandoff,
    pub gates: Vec<GateEvidence>,
}

/// The single authority that can activate or roll back generations of one
/// program.
pub struct ProgramCoordinator {
    swaps: HotSwapEngine,
    policy: ProgramPolicy,
    manifest: ProgramManifest,
    /// The active generation's IR: the shadow canary replays it.
    active: Mutex<Option<Arc<IrModule>>>,
    /// The IR of every retained generation, by id, so a rollback restores
    /// the canary's baseline along with the machine code.
    modules: Mutex<BTreeMap<u64, Arc<IrModule>>>,
    /// Serialises [`Self::call`] with itself and with every state handoff,
    /// so no update a call makes to program state is lost to a switch.
    calls: Mutex<()>,
}

fn gate(gate: EvolutionGate, detail: impl Into<String>) -> EvolutionError {
    EvolutionError::Gate {
        gate,
        detail: detail.into(),
    }
}

impl ProgramCoordinator {
    pub fn new(policy: ProgramPolicy, manifest: ProgramManifest) -> Result<Self, EvolutionError> {
        Self::with_hot_swap_config(
            policy,
            manifest,
            crate::codegen::hot_swap::HotSwapConfig::from_env(),
        )
    }

    pub fn with_hot_swap_config(
        policy: ProgramPolicy,
        manifest: ProgramManifest,
        config: crate::codegen::hot_swap::HotSwapConfig,
    ) -> Result<Self, EvolutionError> {
        if policy.schema != PROGRAM_POLICY_SCHEMA {
            return Err(EvolutionError::InvalidPolicy(format!(
                "expected schema {PROGRAM_POLICY_SCHEMA}, found {}",
                policy.schema
            )));
        }
        if policy.required_tests.is_empty()
            || policy.rollback_probe.is_empty()
            || policy.deterministic_repetitions < 2
            || policy.max_interpreter_steps == 0
            || policy.max_interpreter_depth == 0
        {
            return Err(EvolutionError::InvalidPolicy(
                "tests, a rollback probe, and at least two repetitions are required".into(),
            ));
        }
        Ok(Self {
            swaps: HotSwapEngine::with_config(config),
            policy,
            manifest,
            active: Mutex::new(None),
            modules: Mutex::new(BTreeMap::new()),
            calls: Mutex::new(()),
        })
    }

    pub fn hot_swap(&self) -> &HotSwapEngine {
        &self.swaps
    }

    pub fn manifest(&self) -> &ProgramManifest {
        &self.manifest
    }

    /// A lease on the active generation, for calling its gateways.
    pub fn lease(&self) -> Result<ProgramLease, EvolutionError> {
        self.swaps
            .lease_program(&self.manifest.program_id)
            .map_err(EvolutionError::from)
    }

    fn options(&self) -> InterpOptions {
        InterpOptions {
            max_steps: self.policy.max_interpreter_steps,
            max_depth: self.policy.max_interpreter_depth,
        }
    }

    /// Establish the trusted baseline: validate the manifest, then install it
    /// with its state initialised.
    pub fn install_baseline(&self, module: IrModule) -> Result<ProgramSwapReceipt, EvolutionError> {
        self.manifest
            .validate(&module)
            .map_err(|error| EvolutionError::InvalidPolicy(error.to_string()))?;
        let receipt = self
            .swaps
            .install_program_verified(
                &module,
                &self.manifest.program_id,
                &self.manifest.gateways(),
                |loaded| loaded.init_state(),
            )
            .map_err(EvolutionError::from)?;
        self.retain_module(receipt.generation, Arc::new(module));
        Ok(receipt)
    }

    /// Call `gateway` on the active generation, natively. A `json_v1`
    /// gateway receives `input` as JSON text and its reply is parsed back
    /// (a reply that is not JSON is a JSON string); `scalar_i64_v1` takes an
    /// integer `input`; `command_v1` ignores it. Returns the generation that
    /// served the call, and its result.
    ///
    /// Calls made here are serialised with one another and with the state
    /// handoff of a promotion or [`Self::rollback_carrying_state`]: every
    /// call lands entirely in one generation, and the next generation starts
    /// from the state that call left. Calls made through a raw
    /// [`Self::lease`] have no such guarantee.
    pub fn call(&self, gateway: &str, input: &Value) -> Result<(u64, Value), EvolutionError> {
        let entry = self
            .manifest
            .entrypoints
            .iter()
            .find(|entry| entry.gateway == gateway)
            .ok_or_else(|| {
                EvolutionError::Runtime(format!("the manifest has no gateway '{gateway}'"))
            })?;
        let _calls = self.calls.lock().unwrap_or_else(|p| p.into_inner());
        let lease = self.lease()?;
        let result = match entry.abi {
            HostAbi::JsonV1 => {
                let reply = lease.call_str_1(gateway, &input.to_string())?;
                serde_json::from_str(&reply).unwrap_or(Value::String(reply))
            }
            HostAbi::CommandV1 => Value::from(lease.call_i64_0(gateway)?),
            HostAbi::ScalarI64V1 => {
                let value = input.as_i64().ok_or_else(|| {
                    EvolutionError::Runtime(format!("gateway '{gateway}' takes an integer"))
                })?;
                Value::from(lease.call_i64_1(gateway, value)?)
            }
        };
        Ok((lease.generation(), result))
    }

    /// The active generation and its state, as JSON, read between calls.
    /// Variables of a type handoff does not carry are left out.
    pub fn snapshot_state(&self) -> Result<(u64, serde_json::Map<String, Value>), EvolutionError> {
        let _calls = self.calls.lock().unwrap_or_else(|p| p.into_inner());
        let lease = self.lease()?;
        let vars: Vec<(String, IrType)> = lease
            .state_vars()
            .iter()
            .filter(|(_, ty)| crate::codegen::llvm_orc::state_type_carried(ty))
            .cloned()
            .collect();
        let names: Vec<String> = vars.iter().map(|(name, _)| name.clone()).collect();
        let mut state = serde_json::Map::new();
        for (name, value) in lease.read_state_of(&names)? {
            if let Some((_, ty)) = vars.iter().find(|(var, _)| *var == name) {
                state.insert(name, state_to_json(ty, &value));
            }
        }
        Ok((lease.generation(), state))
    }

    /// Record `module` as the IR of `generation` and make it the active IR,
    /// forgetting the IR of generations the engine no longer retains.
    fn retain_module(&self, generation: u64, module: Arc<IrModule>) {
        let retained = self
            .swaps
            .retained_program_generations(&self.manifest.program_id);
        let mut modules = self.modules.lock().unwrap_or_else(|p| p.into_inner());
        modules.insert(generation, Arc::clone(&module));
        modules.retain(|id, _| retained.contains(id));
        drop(modules);
        self.set_active(module);
    }

    fn module_of(&self, generation: u64) -> Option<Arc<IrModule>> {
        self.modules
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&generation)
            .cloned()
    }

    fn set_active(&self, module: Arc<IrModule>) {
        let mut active = self.active.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(previous) = active.take() {
            crate::interp::reset_program_state(&previous);
        }
        *active = Some(module);
    }

    fn active_module(&self) -> Option<Arc<IrModule>> {
        self.active
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Run `module` through the seven gates against `scenarios` and, if every
    /// gate passes, hand the live generation's state to it and activate it.
    /// `constitution_allows(step, output)` is trusted host code judging each
    /// scenario step's result.
    pub fn evaluate_and_promote<F>(
        &self,
        source: &str,
        module: IrModule,
        scenarios: &[Scenario],
        constitution: &TrustedConstitution,
        constitution_allows: F,
        mut audit: Option<&mut EvolutionAuditLog>,
    ) -> Result<ProgramPromotionReceipt, EvolutionError>
    where
        F: Fn(&ScenarioStep, &Value) -> bool,
    {
        let candidate_hash = sha256_hex(source.as_bytes());
        let policy_hash = sha256_hex(
            &serde_json::to_vec(&(&self.policy, &self.manifest))
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
        let module = Arc::new(module);
        // The candidate's interpreter state must not outlive this call.
        struct ResetOnDrop(Arc<IrModule>);
        impl Drop for ResetOnDrop {
            fn drop(&mut self) {
                crate::interp::reset_program_state(&self.0);
            }
        }
        let _reset = ResetOnDrop(Arc::clone(&module));
        let mut gates = Vec::with_capacity(EvolutionGate::ALL.len());

        // 1. Language safety: everything reachable from the gateways, the
        //    state contract, the tests and the probe.
        let mut roots: Vec<String> = self
            .manifest
            .entrypoints
            .iter()
            .map(|entry| entry.symbol.clone())
            .collect();
        if module.function_by_name(STATE_INIT_FN).is_some() {
            roots.push(STATE_INIT_FN.to_owned());
        }
        if let Some(state) = &self.manifest.state {
            roots.push(state.snapshot_symbol.clone());
            roots.push(state.import_symbol.clone());
        }
        roots.extend(self.policy.required_tests.iter().cloned());
        roots.push(self.policy.rollback_probe.clone());
        require_gate!(
            EvolutionGate::LanguageSafety,
            validate_reachable_safe_subset(&module, &roots)
        );
        pass_gate(
            &mut gates,
            audit.as_deref_mut(),
            &candidate_hash,
            &policy_hash,
            EvolutionGate::LanguageSafety,
            &format!(
                "full compiler pipeline, strict effects, and safe-subset reachability from {} roots passed",
                roots.len()
            ),
        )?;

        // 2. Resources, manifest ABI, and a state handoff that never changes
        //    a variable's type.
        let resources = measure_resources(source, &module);
        require_gate!(
            EvolutionGate::ResourceAndAbi,
            enforce_resources(&resources, &self.policy.resources)
        );
        require_gate!(
            EvolutionGate::ResourceAndAbi,
            self.manifest
                .validate(&module)
                .map_err(|error| gate(EvolutionGate::ResourceAndAbi, error.to_string()))
        );
        let active = self.active_module();
        let handoff = require_gate!(
            EvolutionGate::ResourceAndAbi,
            plan_handoff(active.as_deref(), &module)
        );
        pass_gate(
            &mut gates,
            audit.as_deref_mut(),
            &candidate_hash,
            &policy_hash,
            EvolutionGate::ResourceAndAbi,
            &format!(
                "{} bytes, {} functions, {} blocks, {} instructions; {} gateways; state carries {}, initialises {}, re-initialises {} (type not carried), drops {}",
                resources.source_bytes,
                resources.functions,
                resources.blocks,
                resources.instructions,
                self.manifest.entrypoints.len(),
                list_or_none(&handoff.carried),
                list_or_none(&handoff.initialised),
                list_or_none(&handoff.reinitialised),
                list_or_none(&handoff.dropped)
            ),
        )?;

        // 3. Deterministic tests, each from freshly initialised state.
        let options = self.options();
        for test in &self.policy.required_tests {
            let runs = require_gate!(
                EvolutionGate::DeterministicTests,
                (0..self.policy.deterministic_repetitions)
                    .map(|_| run_probe(&module, test, options))
                    .collect::<Result<Vec<_>, _>>()
            );
            if let Some((index, &bad)) = runs.iter().enumerate().find(|(_, &result)| result != 0) {
                let error = gate(
                    EvolutionGate::DeterministicTests,
                    format!("test '{test}' returned {bad} on repetition {}", index + 1),
                );
                record_failed_gate(
                    audit.as_deref_mut(),
                    &candidate_hash,
                    &policy_hash,
                    EvolutionGate::DeterministicTests,
                    &error.to_string(),
                );
                return Err(error);
            }
        }
        // ...and the scenarios replay identically.
        let mut candidate_runs = Vec::with_capacity(self.policy.deterministic_repetitions);
        for _ in 0..self.policy.deterministic_repetitions {
            candidate_runs.push(require_gate!(
                EvolutionGate::DeterministicTests,
                run_scenarios(&module, &self.manifest, scenarios, options)
            ));
        }
        if let Some(changed) = candidate_runs
            .iter()
            .skip(1)
            .position(|run| run != &candidate_runs[0])
        {
            let error = gate(
                EvolutionGate::DeterministicTests,
                format!(
                    "scenario results changed between repetitions 1 and {}",
                    changed + 2
                ),
            );
            record_failed_gate(
                audit.as_deref_mut(),
                &candidate_hash,
                &policy_hash,
                EvolutionGate::DeterministicTests,
                &error.to_string(),
            );
            return Err(error);
        }
        let candidate_outputs = candidate_runs.swap_remove(0);
        pass_gate(
            &mut gates,
            audit.as_deref_mut(),
            &candidate_hash,
            &policy_hash,
            EvolutionGate::DeterministicTests,
            &format!(
                "{} assertion-backed tests and {} scenarios repeated {} times; bounded interpreter",
                self.policy.required_tests.len(),
                scenarios.len(),
                self.policy.deterministic_repetitions
            ),
        )?;

        // 4. Transaction rollback probe.
        let probe = require_gate!(
            EvolutionGate::TransactionRollback,
            run_probe(&module, &self.policy.rollback_probe, options)
        );
        if probe != 0 {
            let error = gate(
                EvolutionGate::TransactionRollback,
                format!(
                    "probe '{}' returned {probe} instead of 0",
                    self.policy.rollback_probe
                ),
            );
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

        // 5. The pinned constitution judges every step's result.
        let constitution_hash =
            require_gate!(EvolutionGate::SafetyConstitution, constitution.verify());
        for (scenario, outputs) in scenarios.iter().zip(&candidate_outputs) {
            for (index, (step, output)) in scenario.steps.iter().zip(outputs).enumerate() {
                if !constitution_allows(step, output) {
                    let error = gate(
                        EvolutionGate::SafetyConstitution,
                        format!(
                            "trusted constitution rejected scenario '{}' step {} ({} with input {}): {}",
                            scenario.name,
                            index + 1,
                            step.gateway,
                            step.input,
                            output
                        ),
                    );
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
        }
        pass_gate_with_constitution(
            &mut gates,
            audit.as_deref_mut(),
            &candidate_hash,
            &policy_hash,
            &constitution_hash,
            EvolutionGate::SafetyConstitution,
            &format!(
                "pinned constitution accepted {} steps",
                candidate_outputs.iter().map(Vec::len).sum::<usize>()
            ),
        )?;

        // 6. Shadow canary: the active generation's IR replays the same
        //    scenarios, and the candidate must do better on the expectations.
        let baseline = require_gate!(
            EvolutionGate::ShadowCanary,
            active.ok_or_else(|| gate(
                EvolutionGate::ShadowCanary,
                "no trusted baseline generation is installed"
            ))
        );
        let baseline_outputs = require_gate!(
            EvolutionGate::ShadowCanary,
            run_scenarios(&baseline, &self.manifest, scenarios, options)
        );
        let metrics = compare_outputs(scenarios, &baseline_outputs, &candidate_outputs);
        if metrics.compared_steps == 0
            || metrics.improved_steps < self.policy.min_improved_steps
            || metrics.candidate_error >= metrics.baseline_error
        {
            let error = gate(
                EvolutionGate::ShadowCanary,
                format!(
                    "candidate error {} did not improve baseline error {} on at least {} of {} compared steps",
                    metrics.candidate_error,
                    metrics.baseline_error,
                    self.policy.min_improved_steps,
                    metrics.compared_steps
                ),
            );
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
                "exact {}/{} vs baseline {}/{}; error {} vs {}",
                metrics.candidate_exact,
                metrics.compared_steps,
                metrics.baseline_exact,
                metrics.compared_steps,
                metrics.candidate_error,
                metrics.baseline_error
            ),
        )?;

        // 7. Verified activation with state handoff: read the live
        //    generation's state, write the carried variables into the
        //    candidate before it becomes reachable, then switch atomically.
        //    Calls through `call` wait from the read to the switch.
        let calls = self.calls.lock().unwrap_or_else(|p| p.into_inner());
        let carried: Vec<(String, StateValue)> =
            match self.swaps.lease_program(&self.manifest.program_id) {
                Ok(lease) => require_gate!(
                    EvolutionGate::VerifiedActivation,
                    lease.read_state_of(&handoff.carried).map_err(|error| gate(
                        EvolutionGate::VerifiedActivation,
                        error.to_string()
                    ))
                ),
                Err(_) => Vec::new(),
            };
        let snapshot_hash = sha256_hex(format!("{carried:?}").as_bytes());
        let swap = require_gate!(
            EvolutionGate::VerifiedActivation,
            self.swaps
                .install_program_verified(
                    &module,
                    &self.manifest.program_id,
                    &self.manifest.gateways(),
                    |loaded| {
                        loaded.init_state()?;
                        for (name, value) in &carried {
                            loaded.write_state(name, value)?;
                        }
                        Ok(())
                    },
                )
                .map_err(|error| gate(EvolutionGate::VerifiedActivation, error.to_string()))
        );
        let active_generation = self
            .swaps
            .active_program_generation(&self.manifest.program_id);
        if active_generation != Some(swap.generation) {
            let _ = self
                .swaps
                .rollback_program_generation(&self.manifest.program_id, swap.generation);
            let error = gate(
                EvolutionGate::VerifiedActivation,
                format!(
                    "active generation changed during activation: expected {}, found {:?}",
                    swap.generation, active_generation
                ),
            );
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
                "generation {} materialized with {} carried state variable{} (snapshot {snapshot_hash}) and installed in every gateway",
                swap.generation,
                handoff.carried.len(),
                if handoff.carried.len() == 1 { "" } else { "s" }
            ),
        )?;
        let mut evidence = BTreeMap::new();
        evidence.insert("generation".to_owned(), swap.generation.to_string());
        evidence.insert("state_snapshot_sha256".to_owned(), snapshot_hash);
        append_event(
            audit,
            EvolutionEventKind::Promoted,
            &candidate_hash,
            Some(swap.generation),
            Some(&constitution_hash),
            &policy_hash,
            evidence,
        )?;
        drop(calls);
        drop(_reset);
        self.retain_module(swap.generation, module);
        Ok(ProgramPromotionReceipt {
            swap,
            candidate_hash,
            constitution_hash,
            policy_hash,
            resources,
            metrics,
            handoff,
            gates,
        })
    }

    /// Roll back to the generation `receipt` replaced. That generation still
    /// holds the state it had when it was replaced.
    pub fn rollback(&self, receipt: &ProgramPromotionReceipt) -> Result<u64, EvolutionError> {
        let generation = self
            .swaps
            .rollback_program_generation(&self.manifest.program_id, receipt.swap.generation)
            .map(|swap| swap.generation)
            .map_err(EvolutionError::from)?;
        if let Some(module) = self.module_of(generation) {
            self.set_active(module);
        }
        Ok(generation)
    }

    /// Roll back to the generation `receipt` replaced, carrying the live
    /// state back with it: every variable both generations declare (with
    /// the same, carried type) takes the value the rolled-back generation
    /// left. In the returned handoff, `initialised` lists the variables only
    /// the restored generation declares; they keep the values they had when
    /// it was replaced.
    pub fn rollback_carrying_state(
        &self,
        receipt: &ProgramPromotionReceipt,
    ) -> Result<(u64, StateHandoff), EvolutionError> {
        let target = receipt.swap.replaced_generation.ok_or_else(|| {
            EvolutionError::Runtime("the promotion replaced no generation".into())
        })?;
        let missing = |generation: u64| {
            EvolutionError::Runtime(format!("generation {generation} is no longer retained"))
        };
        let current_ir = self
            .module_of(receipt.swap.generation)
            .ok_or_else(|| missing(receipt.swap.generation))?;
        let target_ir = self.module_of(target).ok_or_else(|| missing(target))?;
        let handoff = plan_handoff(Some(&current_ir), &target_ir)?;
        let _calls = self.calls.lock().unwrap_or_else(|p| p.into_inner());
        let lease = self.lease()?;
        if lease.generation() != receipt.swap.generation {
            return Err(EvolutionError::Runtime(format!(
                "generation {} is active, not {}",
                lease.generation(),
                receipt.swap.generation
            )));
        }
        let carried = lease.read_state_of(&handoff.carried)?;
        drop(lease);
        let swap = self
            .swaps
            .rollback_program_to_generation_with(
                &self.manifest.program_id,
                receipt.swap.generation,
                target,
                |restored| {
                    for (name, value) in &carried {
                        restored.write_state(name, value)?;
                    }
                    Ok(())
                },
            )
            .map_err(EvolutionError::from)?;
        self.set_active(target_ir);
        Ok((swap.generation, handoff))
    }
}

fn list_or_none(names: &[String]) -> String {
    if names.is_empty() {
        "nothing".to_owned()
    } else {
        names.join(", ")
    }
}

/// Which state variables move from `active` to `candidate`. A variable both
/// declare must keep its type.
fn plan_handoff(
    active: Option<&IrModule>,
    candidate: &IrModule,
) -> Result<StateHandoff, EvolutionError> {
    let mut handoff = StateHandoff::default();
    let old: BTreeMap<&str, &IrType> = active
        .map(|module| {
            module
                .state_vars()
                .iter()
                .map(|(name, ty)| (name.as_str(), ty))
                .collect()
        })
        .unwrap_or_default();
    let new_names: BTreeSet<&str> = candidate
        .state_vars()
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    for (name, ty) in candidate.state_vars() {
        match old.get(name.as_str()) {
            Some(old_ty) if *old_ty == ty && crate::codegen::llvm_orc::state_type_carried(ty) => {
                handoff.carried.push(name.clone())
            }
            Some(old_ty) if *old_ty == ty => handoff.reinitialised.push(name.clone()),
            Some(old_ty) => {
                return Err(gate(
                    EvolutionGate::ResourceAndAbi,
                    format!(
                        "state '{name}' changes type from {old_ty} to {ty}; give the new variable a new name so it starts fresh"
                    ),
                ))
            }
            None => handoff.initialised.push(name.clone()),
        }
    }
    handoff.dropped = old
        .keys()
        .filter(|name| !new_names.contains(*name))
        .map(|name| (*name).to_owned())
        .collect();
    Ok(handoff)
}

fn interp_error(context: &str, error: impl std::fmt::Display) -> EvolutionError {
    EvolutionError::Runtime(format!("{context}: {error}"))
}

/// A zero-argument test or probe, from freshly initialised state.
fn run_probe(module: &IrModule, name: &str, options: InterpOptions) -> Result<i64, EvolutionError> {
    let function = module
        .function_by_name(name)
        .ok_or_else(|| EvolutionError::Runtime(format!("function '{name}' is missing")))?;
    crate::interp::init_program_state(module, options)
        .map_err(|error| interp_error("initialising program state", error))?;
    match crate::interp::eval_function_in_module_opts(module, function, &[], options)
        .map_err(|error| interp_error(name, error))?
        .as_slice()
    {
        [IrValue::I64(value)] => Ok(*value),
        other => Err(EvolutionError::Runtime(format!(
            "'{name}' returned {other:?}, expected one i64"
        ))),
    }
}

/// Every scenario's step results, each scenario from its own starting state.
fn run_scenarios(
    module: &IrModule,
    manifest: &ProgramManifest,
    scenarios: &[Scenario],
    options: InterpOptions,
) -> Result<Vec<Vec<Value>>, EvolutionError> {
    let mut results = Vec::with_capacity(scenarios.len());
    for scenario in scenarios {
        crate::interp::init_program_state(module, options)
            .map_err(|error| interp_error("initialising program state", error))?;
        for (name, value) in &scenario.state {
            let (_, ty) = module.state_slot(name).ok_or_else(|| {
                EvolutionError::InvalidPolicy(format!(
                    "scenario '{}' sets state '{name}', which the program does not declare",
                    scenario.name
                ))
            })?;
            let value = state_from_json(ty, value).map_err(|error| {
                EvolutionError::InvalidPolicy(format!(
                    "scenario '{}' state '{name}': {error}",
                    scenario.name
                ))
            })?;
            crate::interp::set_program_state(module, name, ir_from_state(&value), options)
                .map_err(|error| interp_error("setting scenario state", error))?;
        }
        let mut outputs = Vec::with_capacity(scenario.steps.len());
        for (index, step) in scenario.steps.iter().enumerate() {
            let context = format!(
                "scenario '{}' step {} ({})",
                scenario.name,
                index + 1,
                step.gateway
            );
            outputs.push(call_gateway(module, manifest, step, options, &context)?);
        }
        results.push(outputs);
    }
    crate::interp::reset_program_state(module);
    Ok(results)
}

fn call_gateway(
    module: &IrModule,
    manifest: &ProgramManifest,
    step: &ScenarioStep,
    options: InterpOptions,
    context: &str,
) -> Result<Value, EvolutionError> {
    let entry = manifest
        .entrypoints
        .iter()
        .find(|entry| entry.gateway == step.gateway)
        .ok_or_else(|| {
            EvolutionError::InvalidPolicy(format!("{context}: the manifest has no such gateway"))
        })?;
    let function = module.function_by_name(&entry.symbol).ok_or_else(|| {
        EvolutionError::Runtime(format!("{context}: '{}' is missing", entry.symbol))
    })?;
    let args = match entry.abi {
        HostAbi::JsonV1 => vec![IrValue::Str(step.input.to_string())],
        HostAbi::CommandV1 => Vec::new(),
        HostAbi::ScalarI64V1 => vec![IrValue::I64(step.input.as_i64().ok_or_else(|| {
            EvolutionError::InvalidPolicy(format!("{context}: input must be an integer"))
        })?)],
    };
    let result = crate::interp::eval_function_in_module_opts(module, function, &args, options)
        .map_err(|error| interp_error(context, error))?;
    match (entry.abi, result.as_slice()) {
        (HostAbi::JsonV1, [IrValue::Str(reply)]) => {
            Ok(serde_json::from_str(reply).unwrap_or_else(|_| Value::String(reply.clone())))
        }
        (HostAbi::CommandV1 | HostAbi::ScalarI64V1, [IrValue::I64(value)]) => {
            Ok(Value::from(*value))
        }
        (_, other) => Err(EvolutionError::Runtime(format!(
            "{context}: unexpected result {other:?}"
        ))),
    }
}

/// Scale of the error between numbers: billionths, so the metric stays an
/// exact, ordered integer.
const ERROR_SCALE: f64 = 1e9;

/// Distance between a result and its expectation: numeric leaves by
/// difference, any other mismatch (a missing key, a different string) as one
/// whole unit.
pub fn json_error(actual: &Value, expected: &Value) -> u128 {
    const UNIT: u128 = ERROR_SCALE as u128;
    match (actual, expected) {
        (Value::Number(a), Value::Number(b)) => {
            if a == b {
                return 0;
            }
            match (a.as_f64(), b.as_f64()) {
                (Some(a), Some(b)) => {
                    let scaled = (a - b).abs() * ERROR_SCALE;
                    if scaled.is_finite() && scaled < u128::MAX as f64 {
                        scaled.round() as u128
                    } else {
                        u128::MAX
                    }
                }
                _ => UNIT,
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            let shared: u128 = a
                .iter()
                .zip(b)
                .map(|(x, y)| json_error(x, y))
                .fold(0, u128::saturating_add);
            let missing = a.len().abs_diff(b.len()) as u128;
            shared.saturating_add(missing.saturating_mul(UNIT))
        }
        (Value::Object(a), Value::Object(b)) => {
            let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
            keys.into_iter()
                .map(|key| match (a.get(key), b.get(key)) {
                    (Some(x), Some(y)) => json_error(x, y),
                    _ => UNIT,
                })
                .fold(0, u128::saturating_add)
        }
        _ if actual == expected => 0,
        _ => UNIT,
    }
}

fn compare_outputs(
    scenarios: &[Scenario],
    baseline: &[Vec<Value>],
    candidate: &[Vec<Value>],
) -> ProgramMetrics {
    let mut metrics = ProgramMetrics {
        scenarios: scenarios.len(),
        steps: 0,
        compared_steps: 0,
        baseline_exact: 0,
        candidate_exact: 0,
        improved_steps: 0,
        baseline_error: 0,
        candidate_error: 0,
    };
    for ((scenario, base), cand) in scenarios.iter().zip(baseline).zip(candidate) {
        for ((step, base), cand) in scenario.steps.iter().zip(base).zip(cand) {
            metrics.steps += 1;
            let Some(expected) = &step.expected else {
                continue;
            };
            metrics.compared_steps += 1;
            let base_error = json_error(base, expected);
            let cand_error = json_error(cand, expected);
            metrics.baseline_error = metrics.baseline_error.saturating_add(base_error);
            metrics.candidate_error = metrics.candidate_error.saturating_add(cand_error);
            metrics.baseline_exact += usize::from(base_error == 0);
            metrics.candidate_exact += usize::from(cand_error == 0);
            metrics.improved_steps += usize::from(cand_error < base_error);
        }
    }
    metrics
}

/// A state value of type `ty` from JSON.
pub fn state_from_json(ty: &IrType, value: &Value) -> Result<StateValue, String> {
    let wrong = || format!("expected {ty}, found {value}");
    Ok(match ty {
        IrType::Scalar(DType::I64) => StateValue::I64(value.as_i64().ok_or_else(wrong)?),
        IrType::Scalar(DType::F64) => StateValue::F64(value.as_f64().ok_or_else(wrong)?),
        IrType::Scalar(DType::Bool) => StateValue::Bool(value.as_bool().ok_or_else(wrong)?),
        IrType::Str => StateValue::Str(value.as_str().ok_or_else(wrong)?.to_owned()),
        IrType::List(elem) => StateValue::List(
            value
                .as_array()
                .ok_or_else(wrong)?
                .iter()
                .map(|item| match elem.as_ref() {
                    IrType::Scalar(DType::I64 | DType::F64 | DType::Bool) | IrType::Str => {
                        state_from_json(elem, item)
                    }
                    other => Err(format!("lists of {other} are not carried by state handoff")),
                })
                .collect::<Result<_, _>>()?,
        ),
        IrType::Struct { name, fields } => {
            let object = value.as_object().ok_or_else(wrong)?;
            StateValue::Record(
                fields
                    .iter()
                    .map(|(field, field_ty)| {
                        let item = object
                            .get(field)
                            .ok_or_else(|| format!("field '{field}' of {name} is missing"))?;
                        match field_ty {
                            IrType::Scalar(DType::F64) => item.as_f64().map(RecordField::F64),
                            IrType::Scalar(DType::I64) => item.as_i64().map(RecordField::I64),
                            IrType::Scalar(DType::Bool) => item.as_bool().map(RecordField::Bool),
                            _ => None,
                        }
                        .ok_or_else(|| {
                            format!("field '{field}' of {name}: expected {field_ty}, found {item}")
                        })
                    })
                    .collect::<Result<_, _>>()?,
            )
        }
        other => return Err(format!("{other} is not carried by state handoff")),
    })
}

/// The JSON form of a state value of type `ty`.
pub fn state_to_json(ty: &IrType, value: &StateValue) -> Value {
    json_of_state(Some(ty), value)
}

/// `state_to_json` where the declared type may not say (a list's elements
/// when the declaration is not a list): records then take index names.
fn json_of_state(ty: Option<&IrType>, value: &StateValue) -> Value {
    match value {
        StateValue::I64(v) => Value::from(*v),
        StateValue::F64(v) => Value::from(*v),
        StateValue::Bool(v) => Value::from(*v),
        StateValue::Str(v) => Value::from(v.clone()),
        StateValue::List(items) => {
            let elem = match ty {
                Some(IrType::List(elem)) => Some(elem.as_ref()),
                _ => None,
            };
            Value::Array(items.iter().map(|item| json_of_state(elem, item)).collect())
        }
        StateValue::Record(fields) => {
            let names: Vec<String> = match ty {
                Some(IrType::Struct { fields, .. }) => {
                    fields.iter().map(|(name, _)| name.clone()).collect()
                }
                _ => (0..fields.len()).map(|index| index.to_string()).collect(),
            };
            Value::Object(
                names
                    .into_iter()
                    .zip(fields)
                    .map(|(name, field)| {
                        let value = match field {
                            RecordField::F64(v) => Value::from(*v),
                            RecordField::I64(v) => Value::from(*v),
                            RecordField::Bool(v) => Value::from(*v),
                        };
                        (name, value)
                    })
                    .collect(),
            )
        }
    }
}

/// An interpreter value for a state value.
fn ir_from_state(value: &StateValue) -> IrValue {
    match value {
        StateValue::I64(v) => IrValue::I64(*v),
        StateValue::F64(v) => IrValue::F64(*v),
        StateValue::Bool(v) => IrValue::Bool(*v),
        StateValue::Str(v) => IrValue::Str(v.clone()),
        StateValue::List(items) => IrValue::List(Arc::new(std::sync::Mutex::new(
            items.iter().map(ir_from_state).collect(),
        ))),
        StateValue::Record(fields) => IrValue::Struct(
            fields
                .iter()
                .map(|field| match field {
                    RecordField::F64(v) => IrValue::F64(*v),
                    RecordField::I64(v) => IrValue::I64(*v),
                    RecordField::Bool(v) => IrValue::Bool(*v),
                })
                .collect(),
        ),
    }
}
