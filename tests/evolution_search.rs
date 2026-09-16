use iris::evolution::compile_candidate;
use iris::evolution::fitness::{FitnessConfig, FitnessEvaluator, WeightedCanaryCase};
use iris::evolution::genome::{BinaryOp, GeneNode, GeneType};
use iris::evolution::mutation::{
    crossover, point_mutate, shrink_mutate, subtree_mutate, EvolutionConfig, EvolutionarySearch,
};
use rand::rngs::StdRng;
use rand::SeedableRng;

#[test]
fn test_genome_evaluation_and_metrics() {
    // Construct (input * 3) + 2
    let genome = GeneNode::Binary {
        op: BinaryOp::Add,
        lhs: Box::new(GeneNode::Binary {
            op: BinaryOp::Mul,
            lhs: Box::new(GeneNode::Var("input".to_string(), GeneType::I64)),
            rhs: Box::new(GeneNode::ConstI64(3)),
        }),
        rhs: Box::new(GeneNode::ConstI64(2)),
    };

    assert_eq!(genome.gene_type(), GeneType::I64);
    assert_eq!(genome.size(), 5);
    assert_eq!(genome.depth(), 3);

    let mut env = std::collections::HashMap::new();
    env.insert("input".to_string(), 4);
    assert_eq!(genome.evaluate(&env).unwrap(), 14);

    env.insert("input".to_string(), -2);
    assert_eq!(genome.evaluate(&env).unwrap(), -4);

    let expr = genome.to_iris_expr();
    assert_eq!(expr, "((input * 3) + 2)");

    let source = genome.to_iris_source("policy", "input");
    assert!(source.contains("def policy(input: i64) -> i64 {"));
    assert!(source.contains("return ((input * 3) + 2)"));
    assert!(source.contains("def test_policy() -> i64 effect throw {"));
    assert!(source
        .contains("def test_transaction_rollback() -> i64 effect alloc, transaction, throw {"));

    // Verify emitted source compiles strictly with IRIS compiler
    let module = compile_candidate(&source, "test_eval_module");
    assert!(
        module.is_ok(),
        "Emitted source failed to compile: {:?}",
        module.err()
    );
}

#[test]
fn test_genetic_operators() {
    let mut rng = StdRng::seed_from_u64(42);

    let mut genome = GeneNode::Binary {
        op: BinaryOp::Add,
        lhs: Box::new(GeneNode::Var("input".to_string(), GeneType::I64)),
        rhs: Box::new(GeneNode::ConstI64(1)),
    };

    // Point mutation
    let mutated = point_mutate(&mut genome, &mut rng, "input");
    assert!(mutated);
    assert_eq!(genome.gene_type(), GeneType::I64);

    // Subtree mutation
    let sub_mutated = subtree_mutate(&mut genome, &mut rng, "input", 4);
    assert!(sub_mutated);
    assert_eq!(genome.gene_type(), GeneType::I64);

    // Shrink mutation
    let _ = shrink_mutate(&mut genome, &mut rng);
    assert_eq!(genome.gene_type(), GeneType::I64);

    // Crossover
    let parent_a = GeneNode::Binary {
        op: BinaryOp::Mul,
        lhs: Box::new(GeneNode::Var("input".to_string(), GeneType::I64)),
        rhs: Box::new(GeneNode::ConstI64(5)),
    };
    let parent_b = GeneNode::Binary {
        op: BinaryOp::Sub,
        lhs: Box::new(GeneNode::ConstI64(100)),
        rhs: Box::new(GeneNode::Var("input".to_string(), GeneType::I64)),
    };

    let (off_a, off_b) = crossover(&parent_a, &parent_b, &mut rng, 6);
    assert_eq!(off_a.gene_type(), GeneType::I64);
    assert_eq!(off_b.gene_type(), GeneType::I64);
}

#[test]
fn test_fitness_evaluator_parsimony() {
    let cases = vec![
        WeightedCanaryCase::new(1, 3),
        WeightedCanaryCase::new(2, 6),
        WeightedCanaryCase::new(3, 9),
    ];

    let config = FitnessConfig {
        parsimony_weight: 0.1,
        ..Default::default()
    };
    let evaluator = FitnessEvaluator::new(cases, config);

    // Candidate 1: Compact (input * 3) - 3 nodes
    let compact = GeneNode::Binary {
        op: BinaryOp::Mul,
        lhs: Box::new(GeneNode::Var("input".to_string(), GeneType::I64)),
        rhs: Box::new(GeneNode::ConstI64(3)),
    };

    // Candidate 2: Bloated ((input * 3) + 0) - 5 nodes
    let bloated = GeneNode::Binary {
        op: BinaryOp::Add,
        lhs: Box::new(compact.clone()),
        rhs: Box::new(GeneNode::ConstI64(0)),
    };

    let score_compact = evaluator.evaluate_genome(&compact, "input");
    let score_bloated = evaluator.evaluate_genome(&bloated, "input");

    assert_eq!(score_compact.exact_matches, 3);
    assert_eq!(score_bloated.exact_matches, 3);
    assert_eq!(score_compact.weighted_mae, 0.0);
    assert_eq!(score_bloated.weighted_mae, 0.0);

    // Parsimony pressure favors the compact genome
    assert!(
        score_compact.loss < score_bloated.loss,
        "Compact loss ({}) should be strictly less than bloated loss ({})",
        score_compact.loss,
        score_bloated.loss
    );
}

#[test]
fn test_evolutionary_search_convergence() {
    let cases = vec![
        WeightedCanaryCase::new(-3, -9),
        WeightedCanaryCase::new(0, 0),
        WeightedCanaryCase::new(1, 3),
        WeightedCanaryCase::new(4, 12),
        WeightedCanaryCase::new(9, 27),
    ];

    let fitness_config = FitnessConfig {
        parsimony_weight: 0.001,
        ..Default::default()
    };
    let evaluator = FitnessEvaluator::new(cases, fitness_config);

    let evo_config = EvolutionConfig {
        population_size: 40,
        generations: 35,
        crossover_rate: 0.7,
        mutation_rate: 0.35,
        tournament_size: 4,
        elite_count: 2,
        max_depth: 5,
        target_loss: Some(0.01),
        seed: Some(12345),
        gradient_refinement: true,
    };

    let search = EvolutionarySearch::new(evo_config, "input".to_string());
    let result = search.run(&evaluator, None, |_| {});

    assert!(
        result.best_score.exact_matches >= 4,
        "Evolutionary search should find at least 4/5 exact matches, found {}/{}",
        result.best_score.exact_matches,
        result.best_score.total_cases
    );

    let candidate_src = result.best_genome.to_iris_source("policy", "input");
    let module = compile_candidate(&candidate_src, "evolved_candidate_test");
    assert!(
        module.is_ok(),
        "Best evolved candidate failed compiler: {:?}",
        module.err()
    );
}
