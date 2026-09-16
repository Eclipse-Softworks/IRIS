//! Evolutionary search engine and genetic mutation operators for IRIS genomes.
//!
//! Provides genetic programming (GP) operators including point mutation, subtree
//! replacement, shrink mutation, and type-safe crossover, alongside population-level
//! tournament selection and generational search.

use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use super::fitness::{FitnessEvaluator, FitnessScore};
use super::genome::{BinaryOp, GeneNode, GeneType, UnaryOp, VecBinaryOp, VecUnaryOp};

/// Configuration for evolutionary search.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvolutionConfig {
    /// Number of individuals in the population.
    pub population_size: usize,
    /// Number of evolutionary generations.
    pub generations: usize,
    /// Probability of performing crossover between selected parents.
    pub crossover_rate: f64,
    /// Probability of mutating an individual.
    pub mutation_rate: f64,
    /// Number of candidates in each tournament selection.
    pub tournament_size: usize,
    /// Number of top elite genomes preserved unchanged per generation.
    pub elite_count: usize,
    /// Maximum AST depth allowed for generated or mutated genomes.
    pub max_depth: usize,
    /// Target loss to stop search early.
    pub target_loss: Option<f64>,
    /// Optional RNG seed for reproducible evolutionary runs.
    pub seed: Option<u64>,
    /// Whether to apply gradient-guided continuous refinement on candidate constants.
    pub gradient_refinement: bool,
}

impl Default for EvolutionConfig {
    fn default() -> Self {
        Self {
            population_size: 50,
            generations: 30,
            crossover_rate: 0.7,
            mutation_rate: 0.3,
            tournament_size: 4,
            elite_count: 2,
            max_depth: 6,
            target_loss: Some(0.005),
            seed: None,
            gradient_refinement: true,
        }
    }
}

/// Summary metrics captured at each generation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenerationSummary {
    pub generation: usize,
    pub best_loss: f64,
    pub best_mae: f64,
    pub best_exact_matches: usize,
    pub total_cases: usize,
    pub best_node_count: usize,
    pub best_tree_depth: usize,
    pub mean_loss: f64,
    pub diversity_ratio: f64,
    pub best_expression: String,
}

/// Final outcome of an evolutionary search run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvolutionSearchResult {
    pub best_genome: GeneNode,
    pub best_score: FitnessScore,
    pub generations_run: usize,
    pub history: Vec<GenerationSummary>,
    pub converged: bool,
}

/// Generator for typed random expression trees.
pub struct TreeGenerator<'a, R: Rng> {
    rng: &'a mut R,
    param_name: &'a str,
}

impl<'a, R: Rng> TreeGenerator<'a, R> {
    pub fn new(rng: &'a mut R, param_name: &'a str) -> Self {
        Self { rng, param_name }
    }

    /// Generates a random typed tree using ramped grow or full methods.
    pub fn generate(&mut self, target_type: GeneType, depth_limit: usize, full: bool) -> GeneNode {
        if depth_limit <= 1 {
            return self.generate_terminal(target_type);
        }

        if full {
            self.generate_non_terminal(target_type, depth_limit, full)
        } else {
            // In grow mode: choose terminal or non-terminal
            if self.rng.gen_bool(0.35) {
                self.generate_terminal(target_type)
            } else {
                self.generate_non_terminal(target_type, depth_limit, full)
            }
        }
    }

    fn generate_terminal(&mut self, target_type: GeneType) -> GeneNode {
        match target_type {
            GeneType::I64 => {
                let p = self.rng.gen_range(0.0..1.0);
                if p < 0.45 {
                    GeneNode::Var(self.param_name.to_string(), GeneType::I64)
                } else if p < 0.80 {
                    let candidates = [-5, -3, -2, -1, 0, 1, 2, 3, 4, 5, 10];
                    let val = *candidates.choose(self.rng).unwrap();
                    GeneNode::ConstI64(val)
                } else {
                    let reg = self.rng.gen_range(0..8);
                    GeneNode::StateRead(reg)
                }
            }
            GeneType::Bool => {
                let val = self.rng.gen_bool(0.5);
                GeneNode::ConstBool(val)
            }
            GeneType::Vec4 => {
                let p = self.rng.gen_range(0.0..1.0);
                if p < 0.40 {
                    GeneNode::Var("input_vec".to_string(), GeneType::Vec4)
                } else if p < 0.75 {
                    let reg = self.rng.gen_range(0..4);
                    GeneNode::StateVecRead(reg)
                } else {
                    let v = [
                        self.rng.gen_range(-5..=5),
                        self.rng.gen_range(-5..=5),
                        self.rng.gen_range(-5..=5),
                        self.rng.gen_range(-5..=5),
                    ];
                    GeneNode::ConstVec(v)
                }
            }
        }
    }

    fn generate_non_terminal(
        &mut self,
        target_type: GeneType,
        depth_limit: usize,
        full: bool,
    ) -> GeneNode {
        let next_depth = depth_limit.saturating_sub(1);
        match target_type {
            GeneType::I64 => {
                let choice = self.rng.gen_range(0..12);
                if choice < 2 {
                    // Unary i64
                    let op = if self.rng.gen_bool(0.5) {
                        UnaryOp::Neg
                    } else {
                        UnaryOp::Abs
                    };
                    GeneNode::Unary {
                        op,
                        child: Box::new(self.generate(GeneType::I64, next_depth, full)),
                    }
                } else if choice < 6 {
                    // Binary i64
                    let ops = [
                        BinaryOp::Add,
                        BinaryOp::Sub,
                        BinaryOp::Mul,
                        BinaryOp::DivChecked,
                        BinaryOp::ModChecked,
                        BinaryOp::Min,
                        BinaryOp::Max,
                    ];
                    let op = *ops.choose(self.rng).unwrap();
                    GeneNode::Binary {
                        op,
                        lhs: Box::new(self.generate(GeneType::I64, next_depth, full)),
                        rhs: Box::new(self.generate(GeneType::I64, next_depth, full)),
                    }
                } else if choice < 8 {
                    // Vector reductions (Vec4 -> i64)
                    let vec_red_choice = self.rng.gen_range(0..5);
                    let op = match vec_red_choice {
                        0 => VecUnaryOp::Sum,
                        1 => VecUnaryOp::Mean,
                        2 => VecUnaryOp::MinElement,
                        3 => VecUnaryOp::MaxElement,
                        _ => VecUnaryOp::ArgMax,
                    };
                    GeneNode::VecUnary {
                        op,
                        child: Box::new(self.generate(GeneType::Vec4, next_depth, full)),
                    }
                } else if choice < 10 {
                    // Dot product (Vec4 x Vec4 -> i64)
                    GeneNode::VecBinary {
                        op: VecBinaryOp::Dot,
                        lhs: Box::new(self.generate(GeneType::Vec4, next_depth, full)),
                        rhs: Box::new(self.generate(GeneType::Vec4, next_depth, full)),
                    }
                } else {
                    // If-Then-Else returning i64
                    GeneNode::IfThenElse {
                        cond: Box::new(self.generate(GeneType::Bool, next_depth, full)),
                        then_branch: Box::new(self.generate(GeneType::I64, next_depth, full)),
                        else_branch: Box::new(self.generate(GeneType::I64, next_depth, full)),
                    }
                }
            }
            GeneType::Bool => {
                if self.rng.gen_bool(0.2) {
                    GeneNode::Unary {
                        op: UnaryOp::Not,
                        child: Box::new(self.generate(GeneType::Bool, next_depth, full)),
                    }
                } else if self.rng.gen_bool(0.7) {
                    let ops = [
                        BinaryOp::Eq,
                        BinaryOp::Ne,
                        BinaryOp::Lt,
                        BinaryOp::Le,
                        BinaryOp::Gt,
                        BinaryOp::Ge,
                    ];
                    let op = *ops.choose(self.rng).unwrap();
                    GeneNode::Binary {
                        op,
                        lhs: Box::new(self.generate(GeneType::I64, next_depth, full)),
                        rhs: Box::new(self.generate(GeneType::I64, next_depth, full)),
                    }
                } else {
                    let op = if self.rng.gen_bool(0.5) {
                        BinaryOp::And
                    } else {
                        BinaryOp::Or
                    };
                    GeneNode::Binary {
                        op,
                        lhs: Box::new(self.generate(GeneType::Bool, next_depth, full)),
                        rhs: Box::new(self.generate(GeneType::Bool, next_depth, full)),
                    }
                }
            }
            GeneType::Vec4 => {
                let choice = self.rng.gen_range(0..10);
                if choice < 2 {
                    let op = if self.rng.gen_bool(0.5) {
                        VecUnaryOp::Neg
                    } else {
                        VecUnaryOp::Abs
                    };
                    GeneNode::VecUnary {
                        op,
                        child: Box::new(self.generate(GeneType::Vec4, next_depth, full)),
                    }
                } else if choice < 5 {
                    let ops = [
                        VecBinaryOp::Add,
                        VecBinaryOp::Sub,
                        VecBinaryOp::Mul,
                        VecBinaryOp::Min,
                        VecBinaryOp::Max,
                    ];
                    let op = *ops.choose(self.rng).unwrap();
                    GeneNode::VecBinary {
                        op,
                        lhs: Box::new(self.generate(GeneType::Vec4, next_depth, full)),
                        rhs: Box::new(self.generate(GeneType::Vec4, next_depth, full)),
                    }
                } else if choice < 7 {
                    let op = if self.rng.gen_bool(0.5) {
                        VecBinaryOp::Scale
                    } else {
                        VecBinaryOp::Shift
                    };
                    GeneNode::VecBinary {
                        op,
                        lhs: Box::new(self.generate(GeneType::Vec4, next_depth, full)),
                        rhs: Box::new(self.generate(GeneType::I64, next_depth, full)),
                    }
                } else if choice < 9 {
                    GeneNode::VecPack([
                        Box::new(self.generate(GeneType::I64, next_depth, full)),
                        Box::new(self.generate(GeneType::I64, next_depth, full)),
                        Box::new(self.generate(GeneType::I64, next_depth, full)),
                        Box::new(self.generate(GeneType::I64, next_depth, full)),
                    ])
                } else {
                    let reg = self.rng.gen_range(0..4);
                    GeneNode::StateVecWrite {
                        reg,
                        val: Box::new(self.generate(GeneType::Vec4, next_depth, full)),
                    }
                }
            }
        }
    }
}

/// Point mutation: modifies a single node without changing tree structure.
pub fn point_mutate<R: Rng>(genome: &mut GeneNode, rng: &mut R, param_name: &str) -> bool {
    let size = genome.size();
    if size == 0 {
        return false;
    }
    let target_idx = rng.gen_range(0..size);
    if let Some(node) = genome.get_node_by_index(target_idx) {
        let mutated = match node {
            GeneNode::ConstI64(val) => {
                let deltas = [-3, -2, -1, 1, 2, 3, 5];
                let delta = *deltas.choose(rng).unwrap();
                GeneNode::ConstI64(val.wrapping_add(delta))
            }
            GeneNode::ConstBool(val) => GeneNode::ConstBool(!val),
            GeneNode::ConstVec(mut v) => {
                let lane = rng.gen_range(0..4);
                let deltas = [-3, -2, -1, 1, 2, 3, 5];
                let delta = *deltas.choose(rng).unwrap();
                v[lane] = v[lane].wrapping_add(delta);
                GeneNode::ConstVec(v)
            }
            GeneNode::Var(_, ty) => {
                if ty == GeneType::I64 && rng.gen_bool(0.4) {
                    let val = rng.gen_range(-5..=5);
                    GeneNode::ConstI64(val)
                } else {
                    GeneNode::Var(param_name.to_string(), ty)
                }
            }
            GeneNode::StateRead(reg) => GeneNode::StateRead((reg + 1) % 8),
            GeneNode::StateWrite { reg, val } => GeneNode::StateWrite {
                reg: (reg + 1) % 8,
                val,
            },
            GeneNode::StateVecRead(reg) => GeneNode::StateVecRead((reg + 1) % 4),
            GeneNode::StateVecWrite { reg, val } => GeneNode::StateVecWrite {
                reg: (reg + 1) % 4,
                val,
            },
            GeneNode::StateAccum { reg, val, factor } => {
                let new_f = match factor {
                    2 => 4,
                    4 => 8,
                    8 => 2,
                    _ => 4,
                };
                GeneNode::StateAccum {
                    reg: (reg + 1) % 8,
                    val,
                    factor: new_f,
                }
            }
            GeneNode::Unary { op, child } => {
                let new_op = match op {
                    UnaryOp::Neg => UnaryOp::Abs,
                    UnaryOp::Abs => UnaryOp::Neg,
                    UnaryOp::Not => UnaryOp::Not,
                };
                GeneNode::Unary { op: new_op, child }
            }
            GeneNode::Binary { op, lhs, rhs } => {
                let new_op = match op {
                    BinaryOp::Add => BinaryOp::Sub,
                    BinaryOp::Sub => BinaryOp::Add,
                    BinaryOp::Mul => BinaryOp::Add,
                    BinaryOp::DivChecked => BinaryOp::Mul,
                    BinaryOp::ModChecked => BinaryOp::DivChecked,
                    BinaryOp::Min => BinaryOp::Max,
                    BinaryOp::Max => BinaryOp::Min,
                    BinaryOp::Eq => BinaryOp::Ne,
                    BinaryOp::Ne => BinaryOp::Eq,
                    BinaryOp::Lt => BinaryOp::Le,
                    BinaryOp::Le => BinaryOp::Lt,
                    BinaryOp::Gt => BinaryOp::Ge,
                    BinaryOp::Ge => BinaryOp::Gt,
                    BinaryOp::And => BinaryOp::Or,
                    BinaryOp::Or => BinaryOp::And,
                };
                GeneNode::Binary {
                    op: new_op,
                    lhs,
                    rhs,
                }
            }
            GeneNode::VecUnary { op, child } => {
                let new_op = match op {
                    VecUnaryOp::Neg => VecUnaryOp::Abs,
                    VecUnaryOp::Abs => VecUnaryOp::Neg,
                    VecUnaryOp::Sum => VecUnaryOp::Mean,
                    VecUnaryOp::Mean => VecUnaryOp::Sum,
                    VecUnaryOp::MinElement => VecUnaryOp::MaxElement,
                    VecUnaryOp::MaxElement => VecUnaryOp::MinElement,
                    VecUnaryOp::ArgMax => VecUnaryOp::ArgMax,
                };
                GeneNode::VecUnary { op: new_op, child }
            }
            GeneNode::VecBinary { op, lhs, rhs } => {
                let new_op = match op {
                    VecBinaryOp::Add => VecBinaryOp::Sub,
                    VecBinaryOp::Sub => VecBinaryOp::Add,
                    VecBinaryOp::Mul => VecBinaryOp::Add,
                    VecBinaryOp::Min => VecBinaryOp::Max,
                    VecBinaryOp::Max => VecBinaryOp::Min,
                    VecBinaryOp::Scale => VecBinaryOp::Shift,
                    VecBinaryOp::Shift => VecBinaryOp::Scale,
                    VecBinaryOp::Dot => VecBinaryOp::Dot,
                };
                GeneNode::VecBinary {
                    op: new_op,
                    lhs,
                    rhs,
                }
            }
            GeneNode::VecPack(elems) => GeneNode::VecPack(elems),
            GeneNode::VecExtract { vec, index } => GeneNode::VecExtract {
                vec,
                index: (index + 1) % 4,
            },
            GeneNode::IfThenElse {
                cond,
                then_branch,
                else_branch,
            } => {
                // Swap branches
                GeneNode::IfThenElse {
                    cond,
                    then_branch: else_branch,
                    else_branch: then_branch,
                }
            }
        };
        genome.replace_node_by_index(target_idx, mutated)
    } else {
        false
    }
}

/// Subtree mutation: replaces a randomly selected subtree with a fresh random typed subtree.
pub fn subtree_mutate<R: Rng>(
    genome: &mut GeneNode,
    rng: &mut R,
    param_name: &str,
    max_depth: usize,
) -> bool {
    let size = genome.size();
    if size == 0 {
        return false;
    }
    let target_idx = rng.gen_range(0..size);
    if let Some(node) = genome.get_node_by_index(target_idx) {
        let node_type = node.gene_type();
        let depth_limit = rng.gen_range(2..=max_depth.min(4));
        let full = rng.gen_bool(0.5);
        let mut generator = TreeGenerator::new(rng, param_name);
        let replacement = generator.generate(node_type, depth_limit, full);
        genome.replace_node_by_index(target_idx, replacement)
    } else {
        false
    }
}

/// Shrink mutation: replaces a compound node with one of its child nodes having the same return type.
pub fn shrink_mutate<R: Rng>(genome: &mut GeneNode, rng: &mut R) -> bool {
    let size = genome.size();
    if size <= 1 {
        return false;
    }
    let target_idx = rng.gen_range(0..size);
    if let Some(node) = genome.get_node_by_index(target_idx) {
        let target_type = node.gene_type();
        let candidate_children: Vec<GeneNode> = match &node {
            GeneNode::StateWrite { val, .. }
            | GeneNode::StateVecWrite { val, .. }
            | GeneNode::StateAccum { val, .. }
                if val.gene_type() == target_type =>
            {
                vec![*val.clone()]
            }
            GeneNode::Unary { child, .. } | GeneNode::VecUnary { child, .. }
                if child.gene_type() == target_type =>
            {
                vec![*child.clone()]
            }
            GeneNode::Binary { lhs, rhs, .. } | GeneNode::VecBinary { lhs, rhs, .. } => {
                let mut c = Vec::new();
                if lhs.gene_type() == target_type {
                    c.push(*lhs.clone());
                }
                if rhs.gene_type() == target_type {
                    c.push(*rhs.clone());
                }
                c
            }
            GeneNode::VecPack(elems) => {
                let mut c = Vec::new();
                for e in elems {
                    if e.gene_type() == target_type {
                        c.push(*e.clone());
                    }
                }
                c
            }
            GeneNode::VecExtract { vec, .. } if vec.gene_type() == target_type => {
                vec![*vec.clone()]
            }
            GeneNode::IfThenElse {
                then_branch,
                else_branch,
                ..
            } => {
                vec![*then_branch.clone(), *else_branch.clone()]
            }
            _ => Vec::new(),
        };

        if let Some(chosen_child) = candidate_children.choose(rng) {
            return genome.replace_node_by_index(target_idx, chosen_child.clone());
        }
    }
    false
}

/// Crossover: swaps compatible typed subtrees between two parent genomes.
pub fn crossover<R: Rng>(
    parent_a: &GeneNode,
    parent_b: &GeneNode,
    rng: &mut R,
    max_depth: usize,
) -> (GeneNode, GeneNode) {
    let target_type = if rng.gen_bool(0.70) {
        GeneType::I64
    } else if rng.gen_bool(0.50) {
        GeneType::Vec4
    } else {
        GeneType::Bool
    };

    let indices_a = parent_a.collect_indices_by_type(target_type);
    let indices_b = parent_b.collect_indices_by_type(target_type);

    if indices_a.is_empty() || indices_b.is_empty() {
        return (parent_a.clone(), parent_b.clone());
    }

    let idx_a = *indices_a.choose(rng).unwrap();
    let idx_b = *indices_b.choose(rng).unwrap();

    let node_a = parent_a.get_node_by_index(idx_a).unwrap();
    let node_b = parent_b.get_node_by_index(idx_b).unwrap();

    let mut offspring_a = parent_a.clone();
    let mut offspring_b = parent_b.clone();

    offspring_a.replace_node_by_index(idx_a, node_b);
    offspring_b.replace_node_by_index(idx_b, node_a);

    // If depth exceeds maximum, fall back to parents
    if offspring_a.depth() > max_depth {
        offspring_a = parent_a.clone();
    }
    if offspring_b.depth() > max_depth {
        offspring_b = parent_b.clone();
    }

    (offspring_a, offspring_b)
}

/// Runs tournament selection to select a parent genome from scored candidates.
pub fn tournament_select<'a, R: Rng>(
    population: &'a [(GeneNode, FitnessScore)],
    tournament_size: usize,
    rng: &mut R,
) -> &'a GeneNode {
    let k = tournament_size.clamp(1, population.len());
    let mut best: Option<&'a (GeneNode, FitnessScore)> = None;

    for _ in 0..k {
        let candidate = population.choose(rng).unwrap();
        match best {
            None => best = Some(candidate),
            Some(current) => {
                if candidate.1.loss < current.1.loss {
                    best = Some(candidate);
                }
            }
        }
    }

    &best.unwrap().0
}

/// The core Evolutionary Search Engine.
pub struct EvolutionarySearch {
    config: EvolutionConfig,
    param_name: String,
}

impl EvolutionarySearch {
    pub fn new(config: EvolutionConfig, param_name: String) -> Self {
        Self { config, param_name }
    }

    /// Initializes a diverse population using ramped half-and-half tree generation.
    pub fn initialize_population<R: Rng>(
        &self,
        rng: &mut R,
        seed_baseline: Option<&GeneNode>,
    ) -> Vec<GeneNode> {
        let mut population = Vec::with_capacity(self.config.population_size);

        if let Some(baseline) = seed_baseline {
            population.push(baseline.clone());
            // Add slight point mutations of baseline
            for _ in 0..self.config.elite_count.max(2) {
                let mut mutated = baseline.clone();
                point_mutate(&mut mutated, rng, &self.param_name);
                population.push(mutated);
            }
        }

        let remaining = self.config.population_size.saturating_sub(population.len());
        let min_depth = 2;
        let max_depth = self.config.max_depth.max(min_depth);

        for i in 0..remaining {
            let depth = min_depth + (i % (max_depth - min_depth + 1));
            let full = (i % 2) == 0;
            let mut generator = TreeGenerator::new(rng, &self.param_name);
            let individual = generator.generate(GeneType::I64, depth, full);
            population.push(individual);
        }

        population
    }

    /// Executes the full evolutionary search loop.
    pub fn run<F>(
        &self,
        evaluator: &FitnessEvaluator,
        seed_baseline: Option<&GeneNode>,
        mut on_generation: F,
    ) -> EvolutionSearchResult
    where
        F: FnMut(&GenerationSummary),
    {
        let mut rng: StdRng = match self.config.seed {
            Some(seed) => StdRng::seed_from_u64(seed),
            None => StdRng::from_entropy(),
        };

        let mut population = self.initialize_population(&mut rng, seed_baseline);
        let mut history = Vec::with_capacity(self.config.generations);
        let mut best_overall: Option<(GeneNode, FitnessScore)> = None;

        for gen in 1..=self.config.generations {
            // Evaluate all individuals
            let mut scored: Vec<(GeneNode, FitnessScore)> = population
                .into_iter()
                .map(|g| {
                    let score = evaluator.evaluate_genome(&g, &self.param_name);
                    (g, score)
                })
                .collect();

            // Sort by loss ascending (lowest loss = most fit)
            scored.sort_by(|a, b| {
                a.1.loss
                    .partial_cmp(&b.1.loss)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            let best_this_gen = &scored[0];
            let mean_loss: f64 =
                scored.iter().map(|s| s.1.loss).sum::<f64>() / (scored.len() as f64);

            let mut unique_hashes = HashSet::new();
            for item in &scored {
                unique_hashes.insert(item.0.to_iris_expr());
            }
            let diversity_ratio = (unique_hashes.len() as f64) / (scored.len() as f64);

            let summary = GenerationSummary {
                generation: gen,
                best_loss: best_this_gen.1.loss,
                best_mae: best_this_gen.1.weighted_mae,
                best_exact_matches: best_this_gen.1.exact_matches,
                total_cases: best_this_gen.1.total_cases,
                best_node_count: best_this_gen.1.node_count,
                best_tree_depth: best_this_gen.1.tree_depth,
                mean_loss,
                diversity_ratio,
                best_expression: best_this_gen.0.to_iris_expr(),
            };

            on_generation(&summary);
            history.push(summary);

            match &best_overall {
                None => best_overall = Some(best_this_gen.clone()),
                Some(current) => {
                    if best_this_gen.1.loss < current.1.loss {
                        best_overall = Some(best_this_gen.clone());
                    }
                }
            }

            // Early stopping check
            let current_best = best_overall.as_ref().unwrap();
            let target_reached = match self.config.target_loss {
                Some(target) => current_best.1.loss <= target,
                None => false,
            };

            if current_best.1.is_perfect() || target_reached || gen == self.config.generations {
                return EvolutionSearchResult {
                    best_genome: current_best.0.clone(),
                    best_score: current_best.1.clone(),
                    generations_run: gen,
                    history,
                    converged: current_best.1.is_perfect() || target_reached,
                };
            }

            // Breeding next generation
            let mut next_generation = Vec::with_capacity(self.config.population_size);

            // Elitism: carry over elite_count best individuals unchanged
            let elite_count = self.config.elite_count.min(scored.len());
            for (genome, _) in scored.iter().take(elite_count) {
                next_generation.push(genome.clone());
            }

            // Fill remainder with crossover and mutations
            while next_generation.len() < self.config.population_size {
                let parent_a = tournament_select(&scored, self.config.tournament_size, &mut rng);
                let parent_b = tournament_select(&scored, self.config.tournament_size, &mut rng);

                let (mut off_a, mut off_b) = if rng.gen_bool(self.config.crossover_rate) {
                    crossover(parent_a, parent_b, &mut rng, self.config.max_depth)
                } else {
                    (parent_a.clone(), parent_b.clone())
                };

                // Mutation
                if rng.gen_bool(self.config.mutation_rate) {
                    let mut_choice = rng.gen_range(0..10);
                    if mut_choice < 4 {
                        point_mutate(&mut off_a, &mut rng, &self.param_name);
                    } else if mut_choice < 8 {
                        subtree_mutate(
                            &mut off_a,
                            &mut rng,
                            &self.param_name,
                            self.config.max_depth,
                        );
                    } else {
                        shrink_mutate(&mut off_a, &mut rng);
                    }
                }

                if rng.gen_bool(self.config.mutation_rate) {
                    let mut_choice = rng.gen_range(0..10);
                    if mut_choice < 4 {
                        point_mutate(&mut off_b, &mut rng, &self.param_name);
                    } else if mut_choice < 8 {
                        subtree_mutate(
                            &mut off_b,
                            &mut rng,
                            &self.param_name,
                            self.config.max_depth,
                        );
                    } else {
                        shrink_mutate(&mut off_b, &mut rng);
                    }
                }

                if self.config.gradient_refinement {
                    tune_constants_gradient(&mut off_a, evaluator, &self.param_name, 2);
                }

                next_generation.push(off_a);
                if next_generation.len() < self.config.population_size {
                    if self.config.gradient_refinement {
                        tune_constants_gradient(&mut off_b, evaluator, &self.param_name, 2);
                    }
                    next_generation.push(off_b);
                }
            }

            population = next_generation;
        }

        let final_best = best_overall.unwrap();
        EvolutionSearchResult {
            best_genome: final_best.0,
            best_score: final_best.1,
            generations_run: self.config.generations,
            history,
            converged: false,
        }
    }
}

/// Continuous parameter optimization via numerical sensitivity / local gradient descent.
/// Fine-tunes ConstI64 numerical parameters in the candidate AST against canary cases.
pub fn tune_constants_gradient(
    genome: &mut GeneNode,
    evaluator: &FitnessEvaluator,
    param_name: &str,
    steps: usize,
) -> bool {
    let const_indices = genome.collect_indices_by_variant_const_i64();
    if const_indices.is_empty() {
        return false;
    }

    let mut current_loss = evaluator.evaluate_genome(genome, param_name).loss;
    let mut any_improved = false;

    for _ in 0..steps {
        let mut step_improved = false;
        for &idx in &const_indices {
            if let Some(GeneNode::ConstI64(orig_val)) = genome.get_node_by_index(idx) {
                let mut best_val = orig_val;
                let mut best_loss = current_loss;

                for delta in [-3, -2, -1, 1, 2, 3] {
                    let cand = orig_val.saturating_add(delta);
                    let mut trial = genome.clone();
                    trial.replace_node_by_index(idx, GeneNode::ConstI64(cand));
                    let loss = evaluator.evaluate_genome(&trial, param_name).loss;
                    if loss < best_loss {
                        best_loss = loss;
                        best_val = cand;
                    }
                }

                if best_val != orig_val {
                    genome.replace_node_by_index(idx, GeneNode::ConstI64(best_val));
                    current_loss = best_loss;
                    step_improved = true;
                    any_improved = true;
                }
            } else if let Some(GeneNode::ConstVec(orig_vec)) = genome.get_node_by_index(idx) {
                let mut best_vec = orig_vec;
                let mut best_loss = current_loss;
                for lane in 0..4 {
                    for delta in [-2, -1, 1, 2] {
                        let mut cand_vec = orig_vec;
                        cand_vec[lane] = cand_vec[lane].saturating_add(delta);
                        let mut trial = genome.clone();
                        trial.replace_node_by_index(idx, GeneNode::ConstVec(cand_vec));
                        let loss = evaluator.evaluate_genome(&trial, param_name).loss;
                        if loss < best_loss {
                            best_loss = loss;
                            best_vec = cand_vec;
                        }
                    }
                }
                if best_vec != orig_vec {
                    genome.replace_node_by_index(idx, GeneNode::ConstVec(best_vec));
                    current_loss = best_loss;
                    step_improved = true;
                    any_improved = true;
                }
            }
        }
        if !step_improved {
            break;
        }
    }

    any_improved
}
