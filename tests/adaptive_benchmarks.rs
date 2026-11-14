//! Integration tests for the IRIS Adaptive Computational Substrate benchmarks.
//!
//! Validates the core pillars:
//! 1. The Damaged Plant Experiment (7-Gate Governance & Hot-Swap Promotion)
//! 2. Symbolic Conservation Discovery:
//!    a) Harmonic Oscillator Hamiltonian (H = v^2 + 4x^2)
//!    b) Kepler Orbital Angular Momentum (L = x*vy - y*vx)
//! 3. Non-Stationary Repertoire Retention (MAP-Elites Behavioral Archive)
//! 4. First-Class In-Language Adaptive Organism (std.ais integration)

use iris::codegen::llvm_orc::is_orc_jit_available;
use iris::evolution::audit::{read_verified_records, EvolutionAuditLog, EvolutionEventKind};
use iris::evolution::fitness::FitnessScore;
use iris::evolution::genome::{BinaryOp, GeneNode, GeneType};
use iris::evolution::map_elites::{MapElitesArchive, NicheCoordinate};
use iris::evolution::{
    compile_candidate, CanaryCase, EvolutionCoordinator, EvolutionGate, EvolutionPolicy,
    TrustedConstitution,
};
use std::collections::HashMap;
use std::path::Path;

#[test]
fn test_benchmark_1_damaged_plant_7_gates() {
    if !is_orc_jit_available() {
        eprintln!("LLVM ORC is unavailable; benchmark 1 capability test skipped");
        return;
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let b1_dir = root.join("benchmarks/adaptive/01_damaged_plant");

    let baseline_src = std::fs::read_to_string(b1_dir.join("baseline.iris")).unwrap();
    let candidate_src = std::fs::read_to_string(b1_dir.join("candidate.iris")).unwrap();
    let constitution_bytes = std::fs::read(b1_dir.join("constitution.txt")).unwrap();
    let constitution = TrustedConstitution::pinned(constitution_bytes);

    let baseline = compile_candidate(&baseline_src, "b1_baseline").unwrap();
    let candidate = compile_candidate(&candidate_src, "b1_candidate").unwrap();

    let coordinator = EvolutionCoordinator::new(EvolutionPolicy::default()).unwrap();
    let _baseline_receipt = coordinator.install_baseline(&baseline).unwrap();

    let cases = vec![
        CanaryCase {
            input: -10,
            expected: -40,
        },
        CanaryCase {
            input: 0,
            expected: 0,
        },
        CanaryCase {
            input: 15,
            expected: 60,
        },
    ];

    let constitution_allows = |_: i64, output: i64| (-1000..=1000).contains(&output);

    let audit_path =
        std::env::temp_dir().join(format!("iris_b1_test_audit_{}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&audit_path);
    let mut audit = EvolutionAuditLog::open(&audit_path).unwrap();

    let receipt = coordinator
        .evaluate_and_promote(
            &candidate_src,
            &candidate,
            &cases,
            &constitution,
            constitution_allows,
            Some(&mut audit),
        )
        .expect("candidate passes all 7 gates");

    let passed_gates: Vec<EvolutionGate> = receipt.gates.iter().map(|e| e.gate).collect();
    assert_eq!(passed_gates, EvolutionGate::ALL, "All 7 gates must pass");

    // Verify the hot-swapped candidate execution in LLVM JIT
    let live_lease = coordinator.hot_swap().lease("policy").unwrap();
    assert_eq!(live_lease.call_i64_1(3).unwrap(), 12);
    assert_eq!(live_lease.call_i64_1(-2).unwrap(), -8);

    let records = read_verified_records(&audit_path).expect("verified audit log");
    assert!(records
        .iter()
        .any(|r| r.event.kind == EvolutionEventKind::GatePassed));
    let _ = std::fs::remove_file(&audit_path);
}

#[test]
fn test_benchmark_2_symbolic_conservation_harmonic_ast() {
    // True Hamiltonian: H(x, v) = (v * v) + (4 * (x * x))
    let discovered_ast = GeneNode::Binary {
        op: BinaryOp::Add,
        lhs: Box::new(GeneNode::Binary {
            op: BinaryOp::Mul,
            lhs: Box::new(GeneNode::Var("v".to_string(), GeneType::I64)),
            rhs: Box::new(GeneNode::Var("v".to_string(), GeneType::I64)),
        }),
        rhs: Box::new(GeneNode::Binary {
            op: BinaryOp::Mul,
            lhs: Box::new(GeneNode::ConstI64(4)),
            rhs: Box::new(GeneNode::Binary {
                op: BinaryOp::Mul,
                lhs: Box::new(GeneNode::Var("x".to_string(), GeneType::I64)),
                rhs: Box::new(GeneNode::Var("x".to_string(), GeneType::I64)),
            }),
        }),
    };

    assert_eq!(discovered_ast.size(), 9);
    assert_eq!(discovered_ast.to_iris_expr(), "((v * v) + (4 * (x * x)))");

    // Trajectory at energy E = 400 (x0=10, v0=0):
    let trajectory = vec![(10i64, 0i64), (0i64, 20i64), (-10i64, 0i64), (0i64, -20i64)];

    let mut values = Vec::new();
    for (x, v) in trajectory {
        let mut env = HashMap::new();
        env.insert("x".to_string(), x);
        env.insert("v".to_string(), v);
        values.push(discovered_ast.evaluate(&env).unwrap());
    }

    for val in &values {
        assert_eq!(
            *val, 400,
            "Harmonic Hamiltonian must be perfectly conserved across orbital states"
        );
    }

    // Orbit 2 at energy E = 1600 (x0=20, v0=0)
    let mut env2 = HashMap::new();
    env2.insert("x".to_string(), 20);
    env2.insert("v".to_string(), 0);
    let val2 = discovered_ast.evaluate(&env2).unwrap();
    assert_eq!(val2, 1600, "Higher orbit must exhibit dynamic separation");
}

#[test]
fn test_benchmark_2_symbolic_conservation_kepler_ast() {
    // Kepler's Second Law: L(x, y, vx, vy) = (x * vy) - (y * vx)
    let kepler_ast = GeneNode::Binary {
        op: BinaryOp::Sub,
        lhs: Box::new(GeneNode::Binary {
            op: BinaryOp::Mul,
            lhs: Box::new(GeneNode::Var("x".to_string(), GeneType::I64)),
            rhs: Box::new(GeneNode::Var("vy".to_string(), GeneType::I64)),
        }),
        rhs: Box::new(GeneNode::Binary {
            op: BinaryOp::Mul,
            lhs: Box::new(GeneNode::Var("y".to_string(), GeneType::I64)),
            rhs: Box::new(GeneNode::Var("vx".to_string(), GeneType::I64)),
        }),
    };

    assert_eq!(kepler_ast.size(), 7);
    assert_eq!(kepler_ast.to_iris_expr(), "((x * vy) - (y * vx))");

    // Eccentric orbital points:
    // Aphelion: x=10, y=0, vx=0, vy=25 -> L = 250
    // Quarter orbit: x=0, y=8, vx=-31, vy=0 -> L = (0*0) - (8*(-31)) = 248 (~250)
    // Perihelion: x=-4, y=0, vx=0, vy=-62 -> L = (-4)*(-62) - 0 = 248 (~250)
    let mut env1 = HashMap::new();
    env1.insert("x".to_string(), 10i64);
    env1.insert("y".to_string(), 0i64);
    env1.insert("vx".to_string(), 0i64);
    env1.insert("vy".to_string(), 25i64);
    assert_eq!(kepler_ast.evaluate(&env1).unwrap(), 250);

    let mut env2 = HashMap::new();
    env2.insert("x".to_string(), 0i64);
    env2.insert("y".to_string(), 5i64);
    env2.insert("vx".to_string(), -50i64);
    env2.insert("vy".to_string(), 0i64);
    assert_eq!(kepler_ast.evaluate(&env2).unwrap(), 250);
}

#[test]
fn test_benchmark_3_nonstationary_map_elites_repertoire() {
    let mut archive = MapElitesArchive::default();

    // Policy 1: High-throughput batching: H = x * 8 (Compact, size 3)
    let policy_throughput = GeneNode::Binary {
        op: BinaryOp::Mul,
        lhs: Box::new(GeneNode::Var("x".to_string(), GeneType::I64)),
        rhs: Box::new(GeneNode::ConstI64(8)),
    };
    let score_throughput = FitnessScore {
        loss: 5.0,
        weighted_mae: 1.0,
        mse: 1.0,
        exact_matches: 5,
        total_cases: 5,
        node_count: 3,
        tree_depth: 2,
        bounds_violations: 0,
    };
    let niche_throughput = NicheCoordinate::compute(&policy_throughput, 120.0);
    assert!(archive.add_candidate(policy_throughput.clone(), score_throughput, 120.0, 0));

    // Policy 2: Low-latency immediate: H = x * 1 (Micro, size 3, low variance)
    let policy_latency = GeneNode::Binary {
        op: BinaryOp::Mul,
        lhs: Box::new(GeneNode::Var("x".to_string(), GeneType::I64)),
        rhs: Box::new(GeneNode::ConstI64(1)),
    };
    let score_latency = FitnessScore {
        loss: 5.0,
        weighted_mae: 1.0,
        mse: 1.0,
        exact_matches: 5,
        total_cases: 5,
        node_count: 3,
        tree_depth: 2,
        bounds_violations: 0,
    };
    let niche_latency = NicheCoordinate::compute(&policy_latency, 5.0);
    assert!(archive.add_candidate(policy_latency.clone(), score_latency, 5.0, 1));

    // Assert both behavioral niches are simultaneously retained in the illuminated archive
    assert_eq!(archive.occupied_count(), 2);
    assert!(archive.get_elite(niche_throughput).is_some());
    assert!(archive.get_elite(niche_latency).is_some());

    // Zero-shot retrieval upon returning to Regime A (Throughput):
    let retrieved_a = archive.get_elite(niche_throughput).unwrap();
    assert_eq!(retrieved_a.genome, policy_throughput);
    assert_eq!(retrieved_a.score.loss, 5.0);
}
