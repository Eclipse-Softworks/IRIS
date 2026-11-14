//! A control loop that keeps running while its controller evolves.
//!
//! [`run_live`] hosts a plant and a controller written in IRIS. Every step it
//! leases the active generation of `policy(error, velocity) -> f64`, asks it
//! for a force, and advances the plant (`plant(input) -> state`, served
//! natively through the record ABI). While the loop runs, candidate
//! controllers are offered to the [`EvolutionCoordinator`] -- hand-written
//! ones on a schedule, and the winner of a float search running on a worker
//! thread when it finishes. Each must pass all seven gates before it is
//! hot-swapped in between two steps; a promoted generation whose output
//! breaks the constitution in the field is rolled back by the coordinator's
//! generation-conditional rollback, and the step is recomputed with the
//! restored controller. Every promotion, rejection and rollback lands in the
//! hash-chained audit log.
//!
//! The plant is trusted host code: it is compiled and run, not gated.

use std::collections::VecDeque;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::codegen::llvm_orc::{OrcJitEngine, RecordField};
use crate::ir::module::IrModule;
use crate::ir::types::{DType, IrType};

use super::audit::EvolutionAuditLog;
use super::float_search::{self, FloatSearchConfig};
use super::{
    compile_candidate, ControlCanaryCase, EvolutionCoordinator, EvolutionError, EvolutionPolicy,
    PolicyAbi, PromotionReceipt, TrustedConstitution,
};

/// Field names the plant's input and output records must have, in order.
pub const PLANT_INPUT_FIELDS: [&str; 4] = ["position", "velocity", "force", "dt"];
pub const PLANT_STATE_FIELDS: [&str; 2] = ["position", "velocity"];

pub struct LiveConfig {
    /// Compiled plant module exporting `plant(input) -> state`.
    pub plant: IrModule,
    /// The controller installed as generation 1.
    pub baseline: IrModule,
    /// Hand-written candidates `(name, source)`, offered in order.
    pub candidates: Vec<(String, String)>,
    pub cases: Vec<ControlCanaryCase>,
    pub constitution: TrustedConstitution,
    /// The constitution's actuator rating: a force must lie in `low..=high`.
    pub output_low: f64,
    pub output_high: f64,
    /// Setpoints, each held for `hold_seconds` of simulated time.
    pub setpoints: Vec<f64>,
    pub hold_seconds: f64,
    pub dt: f64,
    /// Simulated seconds between candidate offers.
    pub offer_every: f64,
    /// Also search for a control law on a worker thread and offer the winner.
    pub search: Option<FloatSearchConfig>,
    pub policy: EvolutionPolicy,
    /// Pace the loop to the wall clock (one `dt` per `dt`).
    pub realtime: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveEventKind {
    Installed,
    Offered,
    Rejected,
    Promoted,
    RolledBack,
    Saturated,
    SearchFinished,
}

impl LiveEventKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::Offered => "offered",
            Self::Rejected => "rejected",
            Self::Promoted => "promoted",
            Self::RolledBack => "rolled back",
            Self::Saturated => "saturated",
            Self::SearchFinished => "search finished",
        }
    }
}

#[derive(Debug, Clone)]
pub struct LiveEvent {
    /// Simulated time, seconds.
    pub time: f64,
    pub kind: LiveEventKind,
    /// The candidate or generation concerned.
    pub subject: String,
    pub detail: String,
}

/// How the loop tracked one setpoint.
#[derive(Debug, Clone)]
pub struct Segment {
    pub target: f64,
    pub final_error: f64,
    /// When the cart settled for good (within 2% and slow), seconds into the segment.
    pub settled_after: Option<f64>,
    pub peak_force: f64,
    /// Generation active at the end of the segment.
    pub generation: u64,
}

#[derive(Debug, Clone)]
pub struct LiveReport {
    pub events: Vec<LiveEvent>,
    pub segments: Vec<Segment>,
    pub steps: usize,
    pub active_generation: u64,
    pub mean_call_ns: u128,
    pub worst_call_ns: u128,
}

impl LiveReport {
    pub fn count(&self, kind: LiveEventKind) -> usize {
        self.events.iter().filter(|e| e.kind == kind).count()
    }
}

fn invalid(detail: impl Into<String>) -> EvolutionError {
    EvolutionError::InvalidPolicy(detail.into())
}

/// Checks the plant exports `plant(PlantInput) -> PlantState` with the field
/// names [`PLANT_INPUT_FIELDS`] and [`PLANT_STATE_FIELDS`], all `f64`.
fn check_plant(plant: &IrModule) -> Result<(), EvolutionError> {
    let function = plant
        .functions()
        .iter()
        .find(|f| f.name == "plant")
        .ok_or_else(|| invalid("the plant module has no `plant` function"))?;
    let fields_of = |ty: &IrType, what: &str, expected: &[&str]| -> Result<(), EvolutionError> {
        let IrType::Struct { name, fields } = ty else {
            return Err(invalid(format!(
                "plant's {what} must be a record, found {ty}"
            )));
        };
        let names: Vec<&str> = fields.iter().map(|(n, _)| n.as_str()).collect();
        let all_f64 = fields
            .iter()
            .all(|(_, t)| matches!(t, IrType::Scalar(DType::F64)));
        if names != expected || !all_f64 {
            return Err(invalid(format!(
                "plant's {what} record {name} must have f64 fields {}, found {}",
                expected.join(", "),
                names.join(", ")
            )));
        }
        Ok(())
    };
    let [param] = function.params.as_slice() else {
        return Err(invalid("`plant` must take exactly one record"));
    };
    fields_of(&param.ty, "input", &PLANT_INPUT_FIELDS)?;
    fields_of(&function.return_ty, "result", &PLANT_STATE_FIELDS)
}

fn as_f64(field: &RecordField) -> f64 {
    match field {
        RecordField::F64(v) => *v,
        RecordField::I64(v) => *v as f64,
        RecordField::Bool(b) => f64::from(u8::from(*b)),
    }
}

/// Runs the loop to the end of the setpoint schedule, reporting each event to
/// `on_event` as it happens.
pub fn run_live(
    config: LiveConfig,
    audit: &mut EvolutionAuditLog,
    on_event: impl FnMut(&LiveEvent),
) -> Result<LiveReport, EvolutionError> {
    run_live_with_hot_swap(
        config,
        audit,
        crate::codegen::hot_swap::HotSwapConfig::from_env(),
        on_event,
    )
}

/// Run the live control loop with an explicit hot-swap retention/archiving
/// configuration.
pub fn run_live_with_hot_swap(
    config: LiveConfig,
    audit: &mut EvolutionAuditLog,
    hot_swap: crate::codegen::hot_swap::HotSwapConfig,
    mut on_event: impl FnMut(&LiveEvent),
) -> Result<LiveReport, EvolutionError> {
    if !matches!(config.policy.abi, PolicyAbi::ControlF64 { inputs: 2 }) {
        return Err(invalid(
            "evolve-live drives `policy(error: f64, velocity: f64) -> f64` (--abi f64:2)",
        ));
    }
    if config.setpoints.is_empty() || config.hold_seconds <= 0.0 || config.dt <= 0.0 {
        return Err(invalid(
            "give at least one setpoint, a positive hold time and a positive dt",
        ));
    }
    check_plant(&config.plant)?;
    let engine = OrcJitEngine::new().map_err(|e| EvolutionError::Runtime(e.to_string()))?;
    let plant = engine
        .load_module(&config.plant)
        .map_err(|e| EvolutionError::Runtime(format!("plant: {e}")))?;

    let coordinator = EvolutionCoordinator::with_hot_swap_config(config.policy.clone(), hot_swap)?;
    let function = config.policy.function.clone();
    let (low, high) = (config.output_low, config.output_high);
    let within = move |_: &[f64], force: f64| force >= low && force <= high;

    let mut events = Vec::new();
    let mut emit = |events: &mut Vec<LiveEvent>, event: LiveEvent| {
        on_event(&event);
        events.push(event);
    };

    let installed = coordinator.install_baseline(&config.baseline)?;
    emit(
        &mut events,
        LiveEvent {
            time: 0.0,
            kind: LiveEventKind::Installed,
            subject: format!("generation {}", installed.generation),
            detail: "baseline controller".into(),
        },
    );

    // The search runs beside the loop and hands back the rendered winner.
    let (search_tx, search_rx) = mpsc::channel::<Result<(String, String), String>>();
    let mut searching = config.search.is_some();
    if let Some(search) = config.search.clone() {
        let cases = config.cases.clone();
        let function = function.clone();
        std::thread::spawn(move || {
            let outcome = float_search::search(&search, &cases, |_| {})
                .map(|result| {
                    let law = result.best.to_iris_expr();
                    let detail = format!(
                        "{law}  (mean |error| {:.4} over {} cases, {} generations)",
                        result.score.mean_absolute_error,
                        cases.len(),
                        result.generations_run
                    );
                    (result.best.to_iris_source(&function, 2), detail)
                })
                .map_err(|e| e.to_string());
            let _ = search_tx.send(outcome);
        });
    }

    let mut queue: VecDeque<(String, String)> = config.candidates.iter().cloned().collect();
    let mut next_offer = config.offer_every;
    let mut current: Option<PromotionReceipt> = None;

    let steps_per_segment = (config.hold_seconds / config.dt).round() as usize;
    let total_steps = steps_per_segment * config.setpoints.len();
    let (mut x, mut v) = (0.0f64, 0.0f64);
    let mut segments = Vec::new();
    let (mut settled_at, mut peak) = (None::<usize>, 0.0f64);
    let (mut total_ns, mut worst_ns) = (0u128, 0u128);
    let wall_start = Instant::now();

    for step in 0..total_steps {
        let t = step as f64 * config.dt;
        let segment = step / steps_per_segment;
        let target = config.setpoints[segment];

        // A finished search jumps the queue. Paced to the wall clock, its
        // winner arrives whenever the worker finishes; in simulated time the
        // search has no duration of its own, so the loop waits for it at the
        // first offer point -- which also makes such a run reproducible.
        let searched = if searching && !config.realtime && t >= config.offer_every {
            searching = false;
            search_rx.recv().ok()
        } else {
            search_rx.try_recv().ok()
        };
        if searched.is_some() {
            searching = false;
        }
        if let Some(outcome) = searched {
            match outcome {
                Ok((source, detail)) => {
                    emit(
                        &mut events,
                        LiveEvent {
                            time: t,
                            kind: LiveEventKind::SearchFinished,
                            subject: "float search".into(),
                            detail,
                        },
                    );
                    queue.push_front(("searched".into(), source));
                    next_offer = t;
                }
                Err(error) => emit(
                    &mut events,
                    LiveEvent {
                        time: t,
                        kind: LiveEventKind::Rejected,
                        subject: "float search".into(),
                        detail: error,
                    },
                ),
            }
        }

        if t >= next_offer {
            if let Some((name, source)) = queue.pop_front() {
                emit(
                    &mut events,
                    LiveEvent {
                        time: t,
                        kind: LiveEventKind::Offered,
                        subject: name.clone(),
                        detail: format!("{} bytes of IRIS", source.len()),
                    },
                );
                let outcome = compile_candidate(&source, &name).and_then(|module| {
                    coordinator.evaluate_and_promote_control(
                        &source,
                        &module,
                        &config.cases,
                        &config.constitution,
                        within,
                        Some(audit),
                    )
                });
                match outcome {
                    Ok(receipt) => {
                        emit(
                            &mut events,
                            LiveEvent {
                                time: t,
                                kind: LiveEventKind::Promoted,
                                subject: name,
                                detail: format!(
                                    "generation {} after {}/7 gates (canary error {} -> {})",
                                    receipt.swap.generation,
                                    receipt.gates.len(),
                                    receipt.canary.baseline_absolute_error,
                                    receipt.canary.candidate_absolute_error
                                ),
                            },
                        );
                        current = Some(receipt);
                    }
                    Err(error) => emit(
                        &mut events,
                        LiveEvent {
                            time: t,
                            kind: LiveEventKind::Rejected,
                            subject: name,
                            detail: error.to_string(),
                        },
                    ),
                }
                next_offer = t + config.offer_every;
            }
        }

        let inputs = [target - x, v];
        let mut force = {
            let lease = coordinator
                .hot_swap()
                .lease(&function)
                .map_err(|e| EvolutionError::Runtime(e.to_string()))?;
            let started = Instant::now();
            let force = lease
                .call_f64_n(&inputs)
                .map_err(|e| EvolutionError::Runtime(e.to_string()))?;
            let elapsed = started.elapsed().as_nanos();
            total_ns += elapsed;
            worst_ns = worst_ns.max(elapsed);
            force
        };
        if !(force.is_finite() && within(&inputs, force)) {
            let generation = coordinator
                .hot_swap()
                .active_generation(&function)
                .unwrap_or(0);
            let rolled_back = match current.take() {
                Some(receipt) => coordinator.observe_or_rollback_control(
                    &receipt,
                    &inputs,
                    force,
                    within,
                    Some(audit),
                )?,
                None => false,
            };
            if rolled_back {
                let restored = coordinator
                    .hot_swap()
                    .active_generation(&function)
                    .unwrap_or(0);
                emit(&mut events, LiveEvent {
                    time: t,
                    kind: LiveEventKind::RolledBack,
                    subject: format!("generation {generation}"),
                    detail: format!(
                        "commanded {force:.3} at error {:.3}, velocity {:.3}: outside [{low}, {high}]; generation {restored} restored",
                        inputs[0], inputs[1]
                    ),
                });
                let lease = coordinator
                    .hot_swap()
                    .lease(&function)
                    .map_err(|e| EvolutionError::Runtime(e.to_string()))?;
                force = lease
                    .call_f64_n(&inputs)
                    .map_err(|e| EvolutionError::Runtime(e.to_string()))?;
            }
            if !(force.is_finite() && within(&inputs, force)) {
                // The actuator saturates; the trusted bound, not the controller, decides.
                let limited = if force.is_finite() {
                    force.clamp(low, high)
                } else {
                    0.0
                };
                emit(
                    &mut events,
                    LiveEvent {
                        time: t,
                        kind: LiveEventKind::Saturated,
                        subject: format!(
                            "generation {}",
                            coordinator
                                .hot_swap()
                                .active_generation(&function)
                                .unwrap_or(0)
                        ),
                        detail: format!("commanded {force:.3}, applied {limited:.3}"),
                    },
                );
                force = limited;
            }
        }

        let next = plant
            .call_record(
                "plant",
                &[
                    RecordField::F64(x),
                    RecordField::F64(v),
                    RecordField::F64(force),
                    RecordField::F64(config.dt),
                ],
            )
            .map_err(|e| EvolutionError::Runtime(format!("plant: {e}")))?;
        x = as_f64(&next[0]);
        v = as_f64(&next[1]);
        peak = peak.max(force.abs());

        let error = target - x;
        let settled = error.abs() < 0.02 * target.abs().max(1.0) && v.abs() < 0.05;
        let in_segment = step - segment * steps_per_segment;
        settled_at = match (settled, settled_at) {
            (true, None) => Some(in_segment),
            (false, Some(_)) => None,
            (_, kept) => kept,
        };
        if in_segment + 1 == steps_per_segment {
            segments.push(Segment {
                target,
                final_error: error,
                settled_after: settled_at.map(|s| s as f64 * config.dt),
                peak_force: peak,
                generation: coordinator
                    .hot_swap()
                    .active_generation(&function)
                    .unwrap_or(0),
            });
            settled_at = None;
            peak = 0.0;
        }

        if config.realtime {
            let due = Duration::from_secs_f64((step + 1) as f64 * config.dt);
            if let Some(wait) = due.checked_sub(wall_start.elapsed()) {
                std::thread::sleep(wait);
            }
        }
    }

    Ok(LiveReport {
        events,
        segments,
        steps: total_steps,
        active_generation: coordinator
            .hot_swap()
            .active_generation(&function)
            .unwrap_or(0),
        mean_call_ns: total_ns / total_steps.max(1) as u128,
        worst_call_ns: worst_ns,
    })
}
