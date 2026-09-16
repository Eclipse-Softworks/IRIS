//! C-ABI Foreign Function Interface (FFI) for IRIS Evolution Engine.
//!
//! Exposes opaque handles and C-callable functions allowing other programming
//! languages (e.g. Python via `ctypes`, C, C++, Go) to evolve, execute, and
//! observe code genomes autonomously.

#![allow(clippy::missing_safety_doc)]

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::panic::catch_unwind;

use super::fitness::{FitnessConfig, FitnessEvaluator, WeightedCanaryCase};
use super::genome::GeneNode;
use super::mutation::{EvolutionConfig, EvolutionarySearch};

/// Opaque handle to an evolutionary search session.
pub struct IrisSearchSession {
    evo_cfg: EvolutionConfig,
    fitness_cfg: FitnessConfig,
    cases: Vec<WeightedCanaryCase>,
    var_name: String,
}

/// Opaque handle to an evolved code genome.
pub struct IrisGenome {
    pub node: GeneNode,
}

fn string_to_c_char(s: String) -> *mut c_char {
    match CString::new(s) {
        Ok(c) => c.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

// ── Search Session API ──────────────────────────────────────────────────────

/// Creates a new evolutionary search session.
#[no_mangle]
pub unsafe extern "C" fn iris_evolution_search_create(
    pop_size: u32,
    generations: u32,
    mutation_rate: f64,
    crossover_rate: f64,
    parsimony_weight: f64,
    seed: u64,
) -> *mut IrisSearchSession {
    let result = catch_unwind(|| {
        let evo_cfg = EvolutionConfig {
            population_size: (pop_size as usize).max(4),
            generations: (generations as usize).max(1),
            crossover_rate: crossover_rate.clamp(0.0, 1.0),
            mutation_rate: mutation_rate.clamp(0.0, 1.0),
            tournament_size: 3,
            elite_count: 2,
            max_depth: 4,
            target_loss: Some(0.001),
            seed: if seed > 0 { Some(seed) } else { None },
            gradient_refinement: true,
        };

        let fitness_cfg = FitnessConfig {
            parsimony_weight,
            ..Default::default()
        };

        Box::into_raw(Box::new(IrisSearchSession {
            evo_cfg,
            fitness_cfg,
            cases: Vec::new(),
            var_name: "input".to_string(),
        }))
    });

    result.unwrap_or(std::ptr::null_mut())
}

/// Adds a weighted canary training case to the search session.
#[no_mangle]
pub unsafe extern "C" fn iris_evolution_search_add_case(
    session: *mut IrisSearchSession,
    input: i64,
    expected: i64,
    weight: f64,
) {
    if session.is_null() {
        return;
    }
    let _ = catch_unwind(|| {
        let session = &mut *session;
        session.cases.push(WeightedCanaryCase {
            input,
            expected,
            weight: if weight > 0.0 { weight } else { 1.0 },
            tolerance: 0,
        });
    });
}

/// Runs the evolutionary search and returns the best discovered genome.
#[no_mangle]
pub unsafe extern "C" fn iris_evolution_search_run(
    session: *mut IrisSearchSession,
) -> *mut IrisGenome {
    if session.is_null() {
        return std::ptr::null_mut();
    }

    let result = catch_unwind(|| {
        let session = &*session;
        if session.cases.is_empty() {
            return std::ptr::null_mut();
        }

        let evaluator = FitnessEvaluator::new(session.cases.clone(), session.fitness_cfg.clone());
        let search = EvolutionarySearch::new(session.evo_cfg.clone(), session.var_name.clone());
        let result = search.run(&evaluator, None, |_| {});

        Box::into_raw(Box::new(IrisGenome {
            node: result.best_genome,
        }))
    });

    result.unwrap_or(std::ptr::null_mut())
}

/// Frees an evolutionary search session handle.
#[no_mangle]
pub unsafe extern "C" fn iris_evolution_search_free(session: *mut IrisSearchSession) {
    if !session.is_null() {
        let _ = catch_unwind(|| {
            drop(Box::from_raw(session));
        });
    }
}

// ── Genome API ──────────────────────────────────────────────────────────────

/// Evaluates the genome on an integer input value.
#[no_mangle]
pub unsafe extern "C" fn iris_genome_eval(genome: *const IrisGenome, input: i64) -> i64 {
    if genome.is_null() {
        return 0;
    }
    let result = catch_unwind(|| {
        let genome = &*genome;
        let mut env = HashMap::new();
        env.insert("input".to_string(), input);
        genome.node.evaluate(&env).unwrap_or(0)
    });
    result.unwrap_or(0)
}

/// Returns the genome formatted as an IRIS expression (caller must free with `iris_string_free`).
#[no_mangle]
pub unsafe extern "C" fn iris_genome_to_iris_expr(genome: *const IrisGenome) -> *mut c_char {
    if genome.is_null() {
        return std::ptr::null_mut();
    }
    let result = catch_unwind(|| {
        let genome = &*genome;
        string_to_c_char(genome.node.to_iris_expr())
    });
    result.unwrap_or(std::ptr::null_mut())
}

/// Returns the genome formatted as a valid Python expression (caller must free with `iris_string_free`).
#[no_mangle]
pub unsafe extern "C" fn iris_genome_to_python_expr(genome: *const IrisGenome) -> *mut c_char {
    if genome.is_null() {
        return std::ptr::null_mut();
    }
    let result = catch_unwind(|| {
        let genome = &*genome;
        string_to_c_char(genome.node.to_python_expr())
    });
    result.unwrap_or(std::ptr::null_mut())
}

/// Returns the genome formatted as a typed Python function definition (caller must free with `iris_string_free`).
#[no_mangle]
pub unsafe extern "C" fn iris_genome_to_python_func(
    genome: *const IrisGenome,
    func_name: *const c_char,
    param_name: *const c_char,
) -> *mut c_char {
    if genome.is_null() {
        return std::ptr::null_mut();
    }
    let result = catch_unwind(|| {
        let genome = &*genome;
        let fn_name = if func_name.is_null() {
            "policy"
        } else {
            CStr::from_ptr(func_name).to_str().unwrap_or("policy")
        };
        let p_name = if param_name.is_null() {
            "x"
        } else {
            CStr::from_ptr(param_name).to_str().unwrap_or("x")
        };
        string_to_c_char(genome.node.to_python_func(fn_name, p_name))
    });
    result.unwrap_or(std::ptr::null_mut())
}

/// Returns the genome formatted as a Python lambda expression (caller must free with `iris_string_free`).
#[no_mangle]
pub unsafe extern "C" fn iris_genome_to_python_lambda(
    genome: *const IrisGenome,
    param_name: *const c_char,
) -> *mut c_char {
    if genome.is_null() {
        return std::ptr::null_mut();
    }
    let result = catch_unwind(|| {
        let genome = &*genome;
        let p_name = if param_name.is_null() {
            "x"
        } else {
            CStr::from_ptr(param_name).to_str().unwrap_or("x")
        };
        string_to_c_char(genome.node.to_python_lambda(p_name))
    });
    result.unwrap_or(std::ptr::null_mut())
}

/// Frees an allocated genome handle.
#[no_mangle]
pub unsafe extern "C" fn iris_genome_free(genome: *mut IrisGenome) {
    if !genome.is_null() {
        let _ = catch_unwind(|| {
            drop(Box::from_raw(genome));
        });
    }
}

/// Creates an allocated persistent GenomeState handle.
#[no_mangle]
pub unsafe extern "C" fn iris_genome_state_create() -> *mut super::genome::GenomeState {
    Box::into_raw(Box::new(super::genome::GenomeState::default()))
}

/// Frees an allocated GenomeState handle.
#[no_mangle]
pub unsafe extern "C" fn iris_genome_state_free(state: *mut super::genome::GenomeState) {
    if !state.is_null() {
        let _ = catch_unwind(|| {
            drop(Box::from_raw(state));
        });
    }
}

/// Resets an allocated GenomeState handle to all zeros.
#[no_mangle]
pub unsafe extern "C" fn iris_genome_state_reset(state: *mut super::genome::GenomeState) {
    if !state.is_null() {
        let _ = catch_unwind(|| {
            (*state).reset();
        });
    }
}

/// Evaluates a genome with a 4-element integer vector input `in_vec4[4]`
/// and populates `out_vec4[4]` with the vector output, returning the argmax scalar action.
#[no_mangle]
pub unsafe extern "C" fn iris_genome_eval_vec4(
    genome: *const IrisGenome,
    in_vec4: *const i64,
    out_vec4: *mut i64,
) -> i64 {
    if genome.is_null() || in_vec4.is_null() {
        return 0;
    }
    let result = catch_unwind(|| {
        let genome = &*genome;
        let mut in_arr = [0i64; 4];
        std::ptr::copy_nonoverlapping(in_vec4, in_arr.as_mut_ptr(), 4);
        let mut state = super::genome::GenomeState::default();
        match genome.node.evaluate_vec4(&in_arr, &mut state) {
            Ok((v, action)) => {
                if !out_vec4.is_null() {
                    std::ptr::copy_nonoverlapping(v.as_ptr(), out_vec4, 4);
                }
                action
            }
            Err(_) => 0,
        }
    });
    result.unwrap_or(0)
}

/// Evaluates a genome with a persistent GenomeState and 4-element input vector.
#[no_mangle]
pub unsafe extern "C" fn iris_genome_eval_stateful_vec4(
    genome: *const IrisGenome,
    state: *mut super::genome::GenomeState,
    in_vec4: *const i64,
    out_vec4: *mut i64,
) -> i64 {
    if genome.is_null() || in_vec4.is_null() {
        return 0;
    }
    let result = catch_unwind(|| {
        let genome = &*genome;
        let mut in_arr = [0i64; 4];
        std::ptr::copy_nonoverlapping(in_vec4, in_arr.as_mut_ptr(), 4);
        let mut default_state = super::genome::GenomeState::default();
        let state_ref = if state.is_null() {
            &mut default_state
        } else {
            &mut *state
        };
        match genome.node.evaluate_vec4(&in_arr, state_ref) {
            Ok((v, action)) => {
                if !out_vec4.is_null() {
                    std::ptr::copy_nonoverlapping(v.as_ptr(), out_vec4, 4);
                }
                action
            }
            Err(_) => 0,
        }
    });
    result.unwrap_or(0)
}

/// Frees a string allocated by this library.
#[no_mangle]
pub unsafe extern "C" fn iris_string_free(s: *mut c_char) {
    if !s.is_null() {
        let _ = catch_unwind(|| {
            drop(CString::from_raw(s));
        });
    }
}
