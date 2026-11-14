//! A controller that evolves while it runs, under proof.
//!
//! A host simulates a cart on a track (position, velocity, viscous drag) and
//! asks an IRIS policy `policy(error: f64, velocity: f64) -> f64` for a force
//! every 20 ms. New versions of the policy arrive as IRIS source. Each one is
//! compiled with strict effects and must pass all seven evolution gates --
//! including an IR-level proof that its whole call graph is allocation-free
//! and non-recursive -- before it is hot-swapped into the running loop. A
//! promoted version that later violates the trusted constitution in the field
//! is rolled back automatically. Every decision lands in a hash-chained audit
//! log.
//!
//! Run from the repository: cargo run --example evolving_controller

use std::time::Instant;

use iris::codegen::hot_swap::HotSwapLease;
use iris::codegen::llvm_orc::is_orc_jit_available;
use iris::evolution::audit::{read_verified_records, EvolutionAuditLog};
use iris::evolution::{
    compile_candidate, ControlCanaryCase, EvolutionCoordinator, EvolutionPolicy, PolicyAbi,
    PromotionReceipt, TrustedConstitution,
};

const MAX_FORCE: f64 = 10.0;
const DT: f64 = 0.02;
const DRAG: f64 = 0.4;

const ROLLBACK_PROBE: &str = r#"
def test_transaction_rollback() -> i64 effect alloc, transaction {
    val state: list<i64> = list()
    push(state, 10);
    transaction_begin();
    push(state, 99);
    transaction_rollback();
    return if list_len(state) == 1 && list_get(state, 0) == 10 { 0 } else { 1 }
}
"#;

/// Generation 0: a sluggish proportional controller.
const BASELINE: &str = r#"
def policy(error: f64, velocity: f64) -> f64 effect pure { return 0.5 * error }
"#;

/// Each candidate: a name, what it is meant to show, and its source.
const CANDIDATES: &[(&str, &str)] = &[
    (
        "pd+history",
        r#"
def policy(error: f64, velocity: f64) -> f64 effect alloc {
    val history: list<f64> = list()
    push(history, error);
    return 4.0 * error - 2.5 * velocity
}
def test_policy() -> i64 effect throw, alloc { assert(policy(1.0, 0.0) == 4.0); return 0 }
"#,
    ),
    (
        "sqrt-gain",
        r#"
def policy(error: f64, velocity: f64) -> f64 effect pure {
    return 4.0 * sqrt(error) - 2.5 * velocity
}
def test_policy() -> i64 effect throw { assert(policy(1.0, 0.0) == 4.0); return 0 }
"#,
    ),
    (
        "pd-unclamped",
        r#"
def policy(error: f64, velocity: f64) -> f64 effect pure {
    return 4.0 * error - 2.5 * velocity
}
def test_policy() -> i64 effect throw { assert(policy(1.0, 0.0) == 4.0); return 0 }
"#,
    ),
    (
        "pd-saturated",
        r#"
def saturate(u: f64) -> f64 effect pure {
    return if u > 10.0 { 10.0 } else { if u < -10.0 { -10.0 } else { u } }
}
def policy(error: f64, velocity: f64) -> f64 effect pure {
    return saturate(4.0 * error - 2.5 * velocity)
}
def test_policy() -> i64 effect throw {
    assert(policy(1.0, 0.0) == 4.0);
    assert(policy(50.0, 0.0) == 10.0);
    return 0
}
"#,
    ),
];

/// The trusted constitution: a finite force inside the actuator's rating.
/// (The coordinator rejects non-finite outputs before consulting it.)
fn within_rating(_: &[f64], force: f64) -> bool {
    force.abs() <= MAX_FORCE
}

/// Reference behaviour the canary scores candidates against: a damped PD law
/// that respects the actuator rating, sampled near the operating point.
fn canary_cases() -> Vec<ControlCanaryCase> {
    let mut cases = Vec::new();
    for error in [-1.0, -0.5, 0.5, 1.0] {
        for velocity in [-0.5, 0.0, 0.5] {
            let force: f64 = 4.0 * error - 2.5 * velocity;
            cases.push(ControlCanaryCase {
                inputs: vec![error, velocity],
                expected: force.clamp(-MAX_FORCE, MAX_FORCE),
            });
        }
    }
    cases
}

struct Episode {
    final_error: f64,
    settle_s: Option<f64>,
    worst_call_ns: u128,
    mean_call_ns: u128,
    violation: Option<(Vec<f64>, f64)>,
}

/// Drive the cart from 0 to `target` for `seconds`, calling whichever policy
/// generation is active through a fresh lease every step.
fn run_episode(coordinator: &EvolutionCoordinator, target: f64, seconds: f64) -> Episode {
    let (mut x, mut v) = (0.0f64, 0.0f64);
    let steps = (seconds / DT) as usize;
    let (mut worst, mut total) = (0u128, 0u128);
    let mut settle_s = None;
    for step in 0..steps {
        let lease: HotSwapLease = coordinator.hot_swap().lease("policy").unwrap();
        let inputs = [target - x, v];
        let started = Instant::now();
        let force = lease.call_f64_n(&inputs).unwrap();
        let elapsed = started.elapsed().as_nanos();
        worst = worst.max(elapsed);
        total += elapsed;
        if !within_rating(&inputs, force) || !force.is_finite() {
            return Episode {
                final_error: target - x,
                settle_s,
                worst_call_ns: worst,
                mean_call_ns: total / (step as u128 + 1),
                violation: Some((inputs.to_vec(), force)),
            };
        }
        v += (force - DRAG * v) * DT;
        x += v * DT;
        let settled = (target - x).abs() < 0.02 * target.abs().max(1.0) && v.abs() < 0.05;
        match (settled, settle_s) {
            (true, None) => settle_s = Some(step as f64 * DT),
            (false, Some(_)) => settle_s = None,
            _ => {}
        }
    }
    Episode {
        final_error: target - x,
        settle_s,
        worst_call_ns: worst,
        mean_call_ns: total / steps as u128,
        violation: None,
    }
}

fn report(label: &str, episode: &Episode) {
    let settle = episode
        .settle_s
        .map_or("not settled".to_owned(), |s| format!("settled at {s:.2} s"));
    println!(
        "    {label:<24} final error {:+.4}  {settle:<18} policy call mean {} ns, worst {} ns",
        episode.final_error, episode.mean_call_ns, episode.worst_call_ns
    );
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if !is_orc_jit_available() {
        println!("LLVM ORC JIT is unavailable on this machine; nothing to demonstrate.");
        return Ok(());
    }

    let policy = EvolutionPolicy {
        abi: PolicyAbi::ControlF64 { inputs: 2 },
        require_allocation_free: true,
        ..EvolutionPolicy::default()
    };
    let coordinator = EvolutionCoordinator::new(policy)?;
    let constitution =
        TrustedConstitution::pinned(b"cart force: finite and within +/-10 N".to_vec());
    let audit_path = std::env::temp_dir().join(format!(
        "iris_evolving_controller_{}.jsonl",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&audit_path);
    let mut audit = EvolutionAuditLog::open(&audit_path)?;
    let cases = canary_cases();

    let gen0 = coordinator.install_baseline(&compile_candidate(BASELINE, "gen0")?)?;
    println!(
        "generation {}: baseline P controller installed",
        gen0.generation
    );
    report("step to 1.0 m", &run_episode(&coordinator, 1.0, 8.0));

    let mut promoted: Vec<(&str, PromotionReceipt)> = Vec::new();
    for (name, body) in CANDIDATES {
        let source = format!("{body}\n{ROLLBACK_PROBE}");
        println!("\ncandidate {name}");
        let module = match compile_candidate(&source, name) {
            Ok(module) => module,
            Err(error) => {
                println!("    rejected at compile time: {error}");
                continue;
            }
        };
        match coordinator.evaluate_and_promote_control(
            &source,
            &module,
            &cases,
            &constitution,
            within_rating,
            Some(&mut audit),
        ) {
            Ok(receipt) => {
                println!(
                    "    promoted to generation {} (canary error {} -> {} nano-units)",
                    receipt.swap.generation,
                    receipt.canary.baseline_absolute_error,
                    receipt.canary.candidate_absolute_error
                );
                println!("    {}", receipt.gates[1].detail);
                promoted.push((name, receipt));
            }
            Err(error) => {
                println!("    rejected: {error}");
                continue;
            }
        }

        // Field operation under the new generation: a small step, then a
        // large setpoint change that pushes the actuator to its limit.
        let receipt = &promoted.last().unwrap().1;
        report("step to 1.0 m", &run_episode(&coordinator, 1.0, 8.0));
        let large = run_episode(&coordinator, 5.0, 10.0);
        if let Some((inputs, force)) = &large.violation {
            println!(
                "    step to 5.0 m: commanded {force:.2} N at state {inputs:?} -- outside the rating"
            );
            let rolled_back = coordinator.observe_or_rollback_control(
                receipt,
                inputs,
                *force,
                within_rating,
                Some(&mut audit),
            )?;
            assert!(rolled_back);
            println!(
                "    constitution violated in the field: rolled back to generation {}",
                coordinator.hot_swap().active_generation("policy").unwrap()
            );
        } else {
            report("step to 5.0 m", &large);
        }
    }

    let names = promoted.iter().map(|(name, _)| *name).collect::<Vec<_>>();
    assert_eq!(names, ["pd-unclamped", "pd-saturated"]);
    let active = coordinator.hot_swap().active_generation("policy").unwrap();
    assert_eq!(active, promoted[1].1.swap.generation);

    let records = read_verified_records(&audit_path)?;
    audit.verify()?;
    println!(
        "\nactive generation {active}; audit log holds {} hash-chained records, chain verified",
        records.len()
    );
    println!("audit log: {}", audit_path.display());
    Ok(())
}
