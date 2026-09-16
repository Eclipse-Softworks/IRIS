//! Quality-Diversity (MAP-Elites) Evolutionary Search Engine for IRIS Genomes.
//!
//! Maintains a multi-dimensional archive of phenotypic elites rather than a simple 1D population.
//! Preserves diverse stepping stones across structural complexity and behavioral variance niches,
//! preventing premature convergence and enabling the discovery of counter-intuitive algorithms.

use std::collections::HashMap;

use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use serde::{Deserialize, Serialize};

use super::fitness::{FitnessEvaluator, FitnessScore};
use super::genome::GeneNode;
use super::mutation::{
    crossover, point_mutate, shrink_mutate, subtree_mutate, tune_constants_gradient,
};

/// Behavioral descriptor defining an individual's phenotypic niche in the archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NicheCoordinate {
    /// Structural complexity bin (0: Micro, 1: Compact, 2: Moderate, 3: Complex, 4: Deep).
    pub complexity_bin: usize,
    /// Behavioral output variance bin (0: Inactive/Flat, 1: Low, 2: Medium, 3: High/Dynamic).
    pub variance_bin: usize,
}

impl NicheCoordinate {
    pub const NUM_COMPLEXITY_BINS: usize = 5;
    pub const NUM_VARIANCE_BINS: usize = 4;
    pub const TOTAL_CELLS: usize = Self::NUM_COMPLEXITY_BINS * Self::NUM_VARIANCE_BINS;

    /// Computes the niche coordinate for a given genome and its evaluation output variance.
    pub fn compute(genome: &GeneNode, output_variance: f64) -> Self {
        let size = genome.size();
        let complexity_bin = match size {
            1..=3 => 0,   // Micro
            4..=7 => 1,   // Compact
            8..=15 => 2,  // Moderate
            16..=30 => 3, // Complex
            _ => 4,       // Deep
        };

        let variance_bin = if output_variance < 0.1 {
            0 // Inactive / Flat
        } else if output_variance < 10.0 {
            1 // Low variance
        } else if output_variance < 100.0 {
            2 // Medium variance
        } else {
            3 // High / Dynamic
        };

        Self {
            complexity_bin,
            variance_bin,
        }
    }
}

/// An elite occupant of a single behavioral niche cell.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EliteEntry {
    pub genome: GeneNode,
    pub score: FitnessScore,
    pub niche: NicheCoordinate,
    pub generation_discovered: usize,
}

/// Multi-dimensional Archive of Phenotypic Elites.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapElitesArchive {
    cells: HashMap<NicheCoordinate, EliteEntry>,
    total_evaluations: usize,
    insertions: usize,
    replacements: usize,
}

impl Default for MapElitesArchive {
    fn default() -> Self {
        Self::new()
    }
}

impl MapElitesArchive {
    pub fn new() -> Self {
        Self {
            cells: HashMap::new(),
            total_evaluations: 0,
            insertions: 0,
            replacements: 0,
        }
    }

    /// Number of occupied niche cells.
    pub fn occupied_count(&self) -> usize {
        self.cells.len()
    }

    /// Archive coverage ratio (occupied / total possible niches).
    pub fn coverage(&self) -> f64 {
        self.occupied_count() as f64 / NicheCoordinate::TOTAL_CELLS as f64
    }

    /// Attempts to add a candidate genome to the archive.
    /// Returns true if the candidate either occupied a new niche or replaced a less-fit elite.
    pub fn add_candidate(
        &mut self,
        genome: GeneNode,
        score: FitnessScore,
        output_variance: f64,
        generation: usize,
    ) -> bool {
        self.total_evaluations += 1;
        let niche = NicheCoordinate::compute(&genome, output_variance);

        match self.cells.get_mut(&niche) {
            Some(existing) => {
                // Replacement rule: lower loss is more fit
                if score.loss < existing.score.loss {
                    *existing = EliteEntry {
                        genome,
                        score,
                        niche,
                        generation_discovered: generation,
                    };
                    self.replacements += 1;
                    true
                } else {
                    false
                }
            }
            None => {
                self.cells.insert(
                    niche,
                    EliteEntry {
                        genome,
                        score,
                        niche,
                        generation_discovered: generation,
                    },
                );
                self.insertions += 1;
                true
            }
        }
    }

    /// Returns a reference to the single best elite overall (lowest loss).
    pub fn best_elite(&self) -> Option<&EliteEntry> {
        self.cells
            .values()
            .min_by(|a, b| a.score.loss.partial_cmp(&b.score.loss).unwrap())
    }

    /// Uniformly samples a random elite from the occupied cells.
    pub fn sample_random_elite<R: Rng>(&self, rng: &mut R) -> Option<&EliteEntry> {
        let entries: Vec<&EliteEntry> = self.cells.values().collect();
        entries.choose(rng).copied()
    }

    /// Returns an immutable iterator over all occupied niche elites.
    pub fn iter_elites(&self) -> impl Iterator<Item = &EliteEntry> {
        self.cells.values()
    }

    /// Returns the elite occupant of a specific niche coordinate, if occupied.
    pub fn get_elite(&self, niche: NicheCoordinate) -> Option<&EliteEntry> {
        self.cells.get(&niche)
    }
}

/// Configuration for MAP-Elites evolutionary search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapElitesConfig {
    pub initial_random_samples: usize,
    pub iterations: usize,
    pub batch_size: usize,
    pub crossover_rate: f64,
    pub mutation_rate: f64,
    pub gradient_refinement: bool,
    pub max_depth: usize,
    pub seed: Option<u64>,
}

impl Default for MapElitesConfig {
    fn default() -> Self {
        Self {
            initial_random_samples: 50,
            iterations: 100,
            batch_size: 10,
            crossover_rate: 0.7,
            mutation_rate: 0.4,
            gradient_refinement: true,
            max_depth: 6,
            seed: None,
        }
    }
}

/// MAP-Elites search orchestrator.
pub struct MapElitesSearch {
    config: MapElitesConfig,
    param_name: String,
}

impl MapElitesSearch {
    pub fn new(config: MapElitesConfig, param_name: &str) -> Self {
        Self {
            config,
            param_name: param_name.to_string(),
        }
    }

    /// Computes variance of outputs produced by a candidate across canary cases.
    fn compute_output_variance(
        genome: &GeneNode,
        evaluator: &FitnessEvaluator,
        param_name: &str,
    ) -> f64 {
        let mut outputs = Vec::new();
        for case in evaluator.cases() {
            let mut env = HashMap::new();
            env.insert(param_name.to_string(), case.input);
            if let Ok(val) = genome.evaluate(&env) {
                outputs.push(val as f64);
            }
        }
        if outputs.len() < 2 {
            return 0.0;
        }
        let mean = outputs.iter().sum::<f64>() / outputs.len() as f64;
        outputs.iter().map(|&x| (x - mean).powi(2)).sum::<f64>() / outputs.len() as f64
    }

    /// Executes MAP-Elites quality-diversity search loop.
    pub fn run(&self, evaluator: &FitnessEvaluator) -> (MapElitesArchive, Option<EliteEntry>) {
        let mut rng: StdRng = match self.config.seed {
            Some(s) => StdRng::seed_from_u64(s),
            None => StdRng::from_entropy(),
        };

        let mut archive = MapElitesArchive::new();

        // 1. Initial random population seeding
        for _ in 0..self.config.initial_random_samples {
            let depth = rng.gen_range(2..=self.config.max_depth.min(4));
            let mut generator = super::mutation::TreeGenerator::new(&mut rng, &self.param_name);
            let mut candidate = generator.generate(super::genome::GeneType::I64, depth, false);

            if self.config.gradient_refinement {
                tune_constants_gradient(&mut candidate, evaluator, &self.param_name, 2);
            }

            let score = evaluator.evaluate_genome(&candidate, &self.param_name);
            let variance = Self::compute_output_variance(&candidate, evaluator, &self.param_name);
            archive.add_candidate(candidate, score, variance, 0);
        }

        // 2. Main quality-diversity optimization loop
        for iter in 1..=self.config.iterations {
            for _ in 0..self.config.batch_size {
                if archive.occupied_count() == 0 {
                    break;
                }

                // Sample parent(s) from archive
                let parent_a = archive
                    .sample_random_elite(&mut rng)
                    .unwrap()
                    .genome
                    .clone();
                let parent_b = archive
                    .sample_random_elite(&mut rng)
                    .unwrap()
                    .genome
                    .clone();

                let mut child = if rng.gen_bool(self.config.crossover_rate) {
                    let (c1, _) = crossover(&parent_a, &parent_b, &mut rng, self.config.max_depth);
                    c1
                } else {
                    parent_a
                };

                // Apply mutation
                if rng.gen_bool(self.config.mutation_rate) {
                    let mut_choice = rng.gen_range(0..10);
                    if mut_choice < 4 {
                        point_mutate(&mut child, &mut rng, &self.param_name);
                    } else if mut_choice < 8 {
                        subtree_mutate(
                            &mut child,
                            &mut rng,
                            &self.param_name,
                            self.config.max_depth,
                        );
                    } else {
                        shrink_mutate(&mut child, &mut rng);
                    }
                }

                // Continuous parameter gradient tuning
                if self.config.gradient_refinement {
                    tune_constants_gradient(&mut child, evaluator, &self.param_name, 2);
                }

                let score = evaluator.evaluate_genome(&child, &self.param_name);
                let variance = Self::compute_output_variance(&child, evaluator, &self.param_name);
                archive.add_candidate(child, score, variance, iter);
            }
        }

        let best = archive.best_elite().cloned();
        (archive, best)
    }
}
