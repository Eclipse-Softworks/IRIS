//! Native, sandboxed candidate validation.
//!
//! Instead of interpreting the candidate, the coordinator compiles it to one
//! native executable whose generated `main` runs every required test, the
//! rollback probe, and the policy on every canary input -- each repeated for
//! the determinism check -- and prints one exact `i64` per call (`f64` results
//! as their bit pattern). That executable runs inside the platform worker
//! sandbox (on Windows: a zero-capability LPAC AppContainer in a Job Object
//! with memory, CPU-time, process and output limits). Untrusted code therefore
//! never executes in the coordinator's process, and the gates judge the same
//! machine code that would be promoted.
//!
//! This fails closed: on a platform without an enforced sandbox backend, the
//! native backend refuses to validate rather than run the candidate
//! unconfined.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::codegen::llvm_ir::{HarnessArg, HarnessCall, HarnessRecordField};
use crate::ir::module::IrModule;
use crate::ir::types::{DType, IrType};
use crate::sandbox::{CommandSpec, NativeSandbox, ResourceLimits};

use super::{
    record_types, EvolutionError, EvolutionGate, EvolutionPolicy, PolicyAbi, PolicyCase,
    PolicyInput, PolicyOutput, RecordData, RecordField,
};

/// Results of one sandboxed run of the validation harness.
#[derive(Debug, Clone)]
pub(super) struct NativeObservations {
    /// Probe name -> one result per repetition.
    pub probes: BTreeMap<String, Vec<i64>>,
    /// Canary case index -> one policy output per repetition.
    pub outputs: Vec<Vec<PolicyOutput>>,
    /// Human-readable resource usage for the audit trail.
    pub usage: String,
}

fn gate_error(detail: impl Into<String>) -> EvolutionError {
    EvolutionError::Gate {
        gate: EvolutionGate::DeterministicTests,
        detail: detail.into(),
    }
}

fn encode_case_args(input: &PolicyInput) -> Vec<HarnessArg> {
    match input {
        PolicyInput::I64(value) => vec![HarnessArg::I64(*value)],
        PolicyInput::F64(values) => values
            .iter()
            .map(|value| HarnessArg::F64Bits(value.to_bits()))
            .collect(),
        PolicyInput::Record(record) => {
            let fields = record
                .fields
                .iter()
                .map(|(_, field)| match field {
                    RecordField::I64(value) => HarnessRecordField::I64(*value),
                    RecordField::F64(value) => HarnessRecordField::F64Bits(value.to_bits()),
                    RecordField::Bool(value) => HarnessRecordField::Bool(*value),
                })
                .collect();
            vec![HarnessArg::Record(fields)]
        }
    }
}

fn decode_record_output(
    out_ty: &IrType,
    raw_fields: &[i64],
) -> Result<PolicyOutput, EvolutionError> {
    let IrType::Struct { fields, .. } = out_ty else {
        return Err(gate_error(format!(
            "expected record return type for native harness, found {out_ty}"
        )));
    };
    let mut converted = Vec::with_capacity(fields.len());
    for ((_, field_ty), raw) in fields.iter().zip(raw_fields.iter()) {
        let field = match field_ty {
            IrType::Scalar(DType::I64) => RecordField::I64(*raw),
            IrType::Scalar(DType::F64) => RecordField::F64(f64::from_bits(*raw as u64)),
            IrType::Scalar(DType::Bool) => RecordField::Bool(*raw != 0),
            other => {
                return Err(gate_error(format!(
                    "unsupported native record return field type {other}"
                )));
            }
        };
        converted.push(field);
    }
    RecordData::from_values(out_ty, converted)
        .map(PolicyOutput::Record)
        .map_err(gate_error)
}

/// Compile, sandbox, run and parse. Every failure is attributed to the
/// `deterministic_tests` gate: it is the first gate that executes candidate
/// code, and a crash anywhere in the harness means the candidate's behaviour
/// could not be established.
pub(super) fn observe(
    module: &IrModule,
    policy: &EvolutionPolicy,
    cases: &[PolicyCase],
    limits: ResourceLimits,
) -> Result<NativeObservations, EvolutionError> {
    let sandbox = NativeSandbox::detect();
    sandbox
        .availability()
        .require_enforced()
        .map_err(|error| gate_error(format!("native validation refused: {error}")))?;

    let out_record_ty = if policy.abi == PolicyAbi::Record {
        Some(record_types(module, &policy.function)?.1)
    } else {
        None
    };

    let repetitions = policy.deterministic_repetitions;
    let mut probe_names = policy.required_tests.clone();
    probe_names.push(policy.rollback_probe.clone());
    let mut calls = Vec::new();
    for probe in &probe_names {
        for _ in 0..repetitions {
            calls.push(HarnessCall {
                function: probe.clone(),
                args: Vec::new(),
            });
        }
    }
    for case in cases {
        let args = encode_case_args(&case.input);
        for _ in 0..repetitions {
            calls.push(HarnessCall {
                function: policy.function.clone(),
                args: args.clone(),
            });
        }
    }

    let work_dir = TempDir::new()?;
    let exe_name = if cfg!(windows) {
        "candidate.exe"
    } else {
        "candidate"
    };
    let binary =
        crate::codegen::build::build_harness_binary(module, &calls, &work_dir.0.join(exe_name))
            .map_err(|error| gate_error(format!("native harness build failed: {error}")))?;
    let command = CommandSpec::new(binary, Vec::new())
        .map_err(|error| gate_error(format!("native harness launch: {error}")))?;
    let output = sandbox
        .prepare_launch(&command, limits)
        .and_then(|launch| launch.run())
        .map_err(|error| gate_error(format!("sandboxed candidate run failed: {error}")))?;
    let usage = format!(
        "sandboxed native run: {} ms wall, {} ms CPU, {} bytes peak memory",
        output.wall_time_ms, output.cpu_time_ms, output.peak_memory_bytes
    );
    if output.exit_code != 0 {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(gate_error(format!(
            "candidate exited with code {:#x} in the sandbox: {}",
            output.exit_code,
            stderr.trim()
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let values = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            line.parse::<i64>()
                .map_err(|_| gate_error(format!("unparseable harness output line '{line}'")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let lines_per_case_call = match out_record_ty.as_ref() {
        Some(IrType::Struct { fields, .. }) => fields.len(),
        _ => 1,
    };
    let expected_lines =
        probe_names.len() * repetitions + cases.len() * repetitions * lines_per_case_call;
    if values.len() != expected_lines {
        return Err(gate_error(format!(
            "harness printed {} results (expected {} for {} calls); candidate output interfered with the protocol",
            values.len(),
            expected_lines,
            calls.len()
        )));
    }

    let mut values = values.into_iter();
    let mut probes = BTreeMap::new();
    for probe in probe_names {
        let results = values.by_ref().take(repetitions).collect();
        probes.insert(probe, results);
    }
    let outputs = cases
        .iter()
        .map(|case| {
            (0..repetitions)
                .map(|_| match &case.input {
                    PolicyInput::I64(_) => {
                        let value = values
                            .next()
                            .ok_or_else(|| gate_error("missing scalar policy output"))?;
                        Ok(PolicyOutput::I64(value))
                    }
                    PolicyInput::F64(_) => {
                        let value = values
                            .next()
                            .ok_or_else(|| gate_error("missing float policy output"))?;
                        Ok(PolicyOutput::F64(f64::from_bits(value as u64)))
                    }
                    PolicyInput::Record(_) => {
                        let out_ty = out_record_ty.as_ref().ok_or_else(|| {
                            gate_error("missing record return type for native harness")
                        })?;
                        let raw_fields: Vec<i64> =
                            values.by_ref().take(lines_per_case_call).collect();
                        if raw_fields.len() != lines_per_case_call {
                            return Err(gate_error("missing record field output"));
                        }
                        decode_record_output(out_ty, &raw_fields)
                    }
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(NativeObservations {
        probes,
        outputs,
        usage,
    })
}

/// A private scratch directory removed on drop. The harness binary is copied
/// into the sandbox profile by the platform backend, so this directory only
/// needs to outlive the launch.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Result<Self, EvolutionError> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "iris_evolve_native_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path)
            .map_err(|error| gate_error(format!("cannot create harness directory: {error}")))?;
        Ok(Self(path))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
