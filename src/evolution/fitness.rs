//! Expressive multi-objective fitness evaluation for evolutionary code search.
//!
//! Evaluates candidate genomes against task performance (MAE, MSE, exact matches),
//! parsimony pressure (code complexity and tree size), execution safety,
//! and constitutional boundaries.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::genome::GeneNode;

/// A test case with an importance weight and tolerance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WeightedCanaryCase {
    pub input: i64,
    pub expected: i64,
    #[serde(default = "default_weight")]
    pub weight: f64,
    #[serde(default)]
    pub tolerance: i64,
}

fn default_weight() -> f64 {
    1.0
}

impl WeightedCanaryCase {
    pub fn new(input: i64, expected: i64) -> Self {
        Self {
            input,
            expected,
            weight: 1.0,
            tolerance: 0,
        }
    }

    pub fn with_weight(mut self, weight: f64) -> Self {
        self.weight = weight;
        self
    }
}

/// Configuration governing multi-objective fitness tradeoffs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FitnessConfig {
    /// Parsimony penalty factor per AST node: Loss += parsimony_weight * node_count.
    pub parsimony_weight: f64,
    /// Penalty factor for deep trees beyond `max_depth_threshold`.
    pub depth_penalty_weight: f64,
    /// Depth at which depth penalties begin.
    pub max_depth_threshold: usize,
    /// Reward deduction from loss for each exact match.
    pub exact_match_reward: f64,
    /// Penalty added whenever an output violates constitution min/max bounds.
    pub bounds_violation_penalty: f64,
    /// Constitutional minimum approved output.
    pub min_output: i64,
    /// Constitutional maximum approved output.
    pub max_output: i64,
}

impl Default for FitnessConfig {
    fn default() -> Self {
        Self {
            parsimony_weight: 0.001,
            depth_penalty_weight: 0.01,
            max_depth_threshold: 6,
            exact_match_reward: 0.0,
            bounds_violation_penalty: 10000.0,
            min_output: i64::MIN,
            max_output: i64::MAX,
        }
    }
}

/// Comprehensive score returned from evaluating a genome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FitnessScore {
    /// Scalar composite loss (lower is better).
    pub loss: f64,
    /// Weighted Mean Absolute Error across all canary cases.
    pub weighted_mae: f64,
    /// Mean Squared Error across all canary cases.
    pub mse: f64,
    /// Total number of test cases where candidate output matched expected (within tolerance).
    pub exact_matches: usize,
    /// Total number of test cases evaluated.
    pub total_cases: usize,
    /// Number of AST nodes in the evaluated genome.
    pub node_count: usize,
    /// Maximum depth of the evaluated genome.
    pub tree_depth: usize,
    /// Number of cases that produced outputs outside constitutional bounds.
    pub bounds_violations: usize,
}

impl FitnessScore {
    /// True if all canary cases matched their expected targets.
    pub fn is_perfect(&self) -> bool {
        self.exact_matches == self.total_cases && self.bounds_violations == 0
    }
}

/// Evaluator that computes fitness scores for candidate genomes.
pub struct FitnessEvaluator {
    cases: Vec<WeightedCanaryCase>,
    config: FitnessConfig,
}

impl FitnessEvaluator {
    pub fn new(cases: Vec<WeightedCanaryCase>, config: FitnessConfig) -> Self {
        Self { cases, config }
    }

    pub fn cases(&self) -> &[WeightedCanaryCase] {
        &self.cases
    }

    pub fn from_canary_cases(
        cases: &[crate::evolution::CanaryCase],
        config: FitnessConfig,
    ) -> Self {
        let weighted = cases
            .iter()
            .map(|c| WeightedCanaryCase::new(c.input, c.expected))
            .collect();
        Self::new(weighted, config)
    }

    /// Evaluates a genome and produces a detailed multi-objective `FitnessScore`.
    pub fn evaluate_genome(&self, genome: &GeneNode, param_name: &str) -> FitnessScore {
        let mut total_weighted_abs_error = 0.0;
        let mut total_squared_error = 0.0;
        let mut total_weight = 0.0;
        let mut exact_matches = 0;
        let mut bounds_violations = 0;

        let mut env = HashMap::new();

        for case in &self.cases {
            env.insert(param_name.to_string(), case.input);
            total_weight += case.weight;

            match genome.evaluate(&env) {
                Ok(actual) => {
                    let diff = (actual as f64) - (case.expected as f64);
                    let abs_err = diff.abs();
                    total_weighted_abs_error += case.weight * abs_err;
                    total_squared_error += diff * diff;

                    if (actual - case.expected).abs() <= case.tolerance {
                        exact_matches += 1;
                    }

                    if actual < self.config.min_output || actual > self.config.max_output {
                        bounds_violations += 1;
                    }
                }
                Err(_) => {
                    // Evaluation error (e.g. division/modulo error) is heavily penalized
                    total_weighted_abs_error += case.weight * 100_000.0;
                    total_squared_error += 1_000_000_000.0;
                    bounds_violations += 1;
                }
            }
        }

        let total_cases = self.cases.len();
        let weighted_mae = if total_weight > 0.0 {
            total_weighted_abs_error / total_weight
        } else {
            0.0
        };

        let mse = if total_cases > 0 {
            total_squared_error / (total_cases as f64)
        } else {
            0.0
        };

        let node_count = genome.size();
        let tree_depth = genome.depth();

        // Compute parsimony and depth penalties
        let parsimony_penalty = self.config.parsimony_weight * (node_count as f64);
        let depth_penalty = if tree_depth > self.config.max_depth_threshold {
            self.config.depth_penalty_weight
                * ((tree_depth - self.config.max_depth_threshold) as f64)
        } else {
            0.0
        };

        let bounds_penalty = (bounds_violations as f64) * self.config.bounds_violation_penalty;
        let exact_bonus = (exact_matches as f64) * self.config.exact_match_reward;

        let loss = (weighted_mae + parsimony_penalty + depth_penalty + bounds_penalty
            - exact_bonus)
            .max(0.0);

        FitnessScore {
            loss,
            weighted_mae,
            mse,
            exact_matches,
            total_cases,
            node_count,
            tree_depth,
            bounds_violations,
        }
    }
}
