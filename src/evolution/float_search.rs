//! Genetic programming over `f64` control laws for the
//! [`PolicyAbi::ControlF64`](super::PolicyAbi::ControlF64) ABI.
//!
//! The search evolves expression trees over a policy's inputs and renders the
//! best one as an ordinary IRIS candidate. It only *proposes*: promotion goes
//! through the same seven gates as a hand-written candidate
//! ([`super::EvolutionCoordinator::evaluate_and_promote_control`]), so a search
//! result is never trusted more than any other untrusted source.
//!
//! Every operator is total on finite inputs: division is protected, `sqrt`
//! takes an absolute value, and `tanh` is written with `exp` so it saturates
//! instead of overflowing. The rendered code uses only scalar arithmetic, so
//! it can carry the allocation-free proof.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde::{Deserialize, Serialize};

use super::ControlCanaryCase;

/// Unary operators of a float genome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FloatUnary {
    Neg,
    Abs,
    Sin,
    Cos,
    Tanh,
    /// `sqrt(abs(x))`.
    SqrtAbs,
}

/// Binary operators of a float genome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FloatBinary {
    Add,
    Sub,
    Mul,
    /// `a / b`, or `a` when `|b|` is below [`DIV_EPSILON`].
    Div,
    Min,
    Max,
}

const UNARY: [FloatUnary; 6] = [
    FloatUnary::Neg,
    FloatUnary::Abs,
    FloatUnary::Sin,
    FloatUnary::Cos,
    FloatUnary::Tanh,
    FloatUnary::SqrtAbs,
];

const BINARY: [FloatBinary; 6] = [
    FloatBinary::Add,
    FloatBinary::Sub,
    FloatBinary::Mul,
    FloatBinary::Div,
    FloatBinary::Min,
    FloatBinary::Max,
];

/// Divisors smaller than this in magnitude leave the dividend unchanged.
pub const DIV_EPSILON: f64 = 1e-9;

/// An expression over the policy inputs `x0 .. x{n-1}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FloatGene {
    Input(usize),
    Const(f64),
    Unary(FloatUnary, Box<FloatGene>),
    Binary(FloatBinary, Box<FloatGene>, Box<FloatGene>),
    /// `if cond > 0 { then } else { otherwise }`.
    IfPositive(Box<FloatGene>, Box<FloatGene>, Box<FloatGene>),
}

impl FloatGene {
    pub fn eval(&self, inputs: &[f64]) -> f64 {
        match self {
            Self::Input(index) => inputs.get(*index).copied().unwrap_or(0.0),
            Self::Const(value) => *value,
            Self::Unary(op, operand) => {
                let x = operand.eval(inputs);
                match op {
                    FloatUnary::Neg => -x,
                    FloatUnary::Abs => x.abs(),
                    FloatUnary::Sin => x.sin(),
                    FloatUnary::Cos => x.cos(),
                    FloatUnary::Tanh => 1.0 - 2.0 / ((2.0 * x).exp() + 1.0),
                    FloatUnary::SqrtAbs => x.abs().sqrt(),
                }
            }
            Self::Binary(op, lhs, rhs) => {
                let a = lhs.eval(inputs);
                let b = rhs.eval(inputs);
                match op {
                    FloatBinary::Add => a + b,
                    FloatBinary::Sub => a - b,
                    FloatBinary::Mul => a * b,
                    FloatBinary::Div => a / if b.abs() > DIV_EPSILON { b } else { 1.0 },
                    FloatBinary::Min => {
                        if a < b {
                            a
                        } else {
                            b
                        }
                    }
                    FloatBinary::Max => {
                        if a > b {
                            a
                        } else {
                            b
                        }
                    }
                }
            }
            Self::IfPositive(cond, then, otherwise) => {
                if cond.eval(inputs) > 0.0 {
                    then.eval(inputs)
                } else {
                    otherwise.eval(inputs)
                }
            }
        }
    }

    pub fn size(&self) -> usize {
        match self {
            Self::Input(_) | Self::Const(_) => 1,
            Self::Unary(_, operand) => 1 + operand.size(),
            Self::Binary(_, lhs, rhs) => 1 + lhs.size() + rhs.size(),
            Self::IfPositive(cond, then, otherwise) => {
                1 + cond.size() + then.size() + otherwise.size()
            }
        }
    }

    pub fn depth(&self) -> usize {
        match self {
            Self::Input(_) | Self::Const(_) => 1,
            Self::Unary(_, operand) => 1 + operand.depth(),
            Self::Binary(_, lhs, rhs) => 1 + lhs.depth().max(rhs.depth()),
            Self::IfPositive(cond, then, otherwise) => {
                1 + cond.depth().max(then.depth()).max(otherwise.depth())
            }
        }
    }

    /// The IRIS expression for this gene.
    pub fn to_iris_expr(&self) -> String {
        match self {
            Self::Input(index) => format!("x{index}"),
            Self::Const(value) => float_literal(*value),
            Self::Unary(op, operand) => {
                let x = operand.to_iris_expr();
                match op {
                    FloatUnary::Neg => format!("(-{x})"),
                    FloatUnary::Abs => format!("abs({x})"),
                    FloatUnary::Sin => format!("sin({x})"),
                    FloatUnary::Cos => format!("cos({x})"),
                    FloatUnary::Tanh => format!("(1.0 - 2.0 / (exp(2.0 * {x}) + 1.0))"),
                    FloatUnary::SqrtAbs => format!("sqrt(abs({x}))"),
                }
            }
            Self::Binary(op, lhs, rhs) => {
                let a = lhs.to_iris_expr();
                let b = rhs.to_iris_expr();
                match op {
                    FloatBinary::Add => format!("({a} + {b})"),
                    FloatBinary::Sub => format!("({a} - {b})"),
                    FloatBinary::Mul => format!("({a} * {b})"),
                    FloatBinary::Div => format!(
                        "({a} / (if abs({b}) > {} {{ {b} }} else {{ 1.0 }}))",
                        float_literal(DIV_EPSILON)
                    ),
                    FloatBinary::Min => format!("(if {a} < {b} {{ {a} }} else {{ {b} }})"),
                    FloatBinary::Max => format!("(if {a} > {b} {{ {a} }} else {{ {b} }})"),
                }
            }
            Self::IfPositive(cond, then, otherwise) => format!(
                "(if {} > 0.0 {{ {} }} else {{ {} }})",
                cond.to_iris_expr(),
                then.to_iris_expr(),
                otherwise.to_iris_expr()
            ),
        }
    }

    /// A complete candidate: `def {function}(x0: f64, ...) -> f64` plus the
    /// assertion-backed `test_policy` and the `test_transaction_rollback`
    /// probe that the default [`super::EvolutionPolicy`] requires.
    pub fn to_iris_source(&self, function: &str, inputs: usize) -> String {
        let params = (0..inputs)
            .map(|i| format!("x{i}: f64"))
            .collect::<Vec<_>>()
            .join(", ");
        let zeros = vec!["0.0"; inputs].join(", ");
        let ones = vec!["1.0"; inputs].join(", ");
        format!(
            "/// Evolved by `iris evolve-search --abi f64:{inputs}`.\n\
             def {function}({params}) -> f64 {{\n    {}\n}}\n\n\
             def test_policy() -> i64 effect throw {{\n\
             \x20   // Deterministic, and finite (NaN is unequal to itself).\n\
             \x20   val zero = {function}({zeros});\n\
             \x20   assert({function}({zeros}) == zero);\n\
             \x20   val one = {function}({ones});\n\
             \x20   assert({function}({ones}) == one);\n\
             \x20   0\n}}\n\n\
             def test_transaction_rollback() -> i64 effect alloc, transaction, throw {{\n\
             \x20   val state: list<i64> = list(10);\n\
             \x20   transaction_begin();\n\
             \x20   list_push(state, 99);\n\
             \x20   transaction_rollback();\n\
             \x20   assert(transaction_depth() == 0);\n\
             \x20   assert(list_len(state) == 1);\n\
             \x20   assert(list_get(state, 0) == 10);\n\
             \x20   0\n}}\n",
            self.to_iris_expr()
        )
    }

    fn constant_count(&self) -> usize {
        match self {
            Self::Const(_) => 1,
            Self::Input(_) => 0,
            Self::Unary(_, operand) => operand.constant_count(),
            Self::Binary(_, lhs, rhs) => lhs.constant_count() + rhs.constant_count(),
            Self::IfPositive(cond, then, otherwise) => {
                cond.constant_count() + then.constant_count() + otherwise.constant_count()
            }
        }
    }

    /// The `index`-th constant in pre-order.
    fn constant_mut(&mut self, index: &mut usize) -> Option<&mut f64> {
        match self {
            Self::Const(value) => {
                if *index == 0 {
                    Some(value)
                } else {
                    *index -= 1;
                    None
                }
            }
            Self::Input(_) => None,
            Self::Unary(_, operand) => operand.constant_mut(index),
            Self::Binary(_, lhs, rhs) => lhs
                .constant_mut(index)
                .or_else(move || rhs.constant_mut(index)),
            Self::IfPositive(cond, then, otherwise) => cond
                .constant_mut(index)
                .or_else(|| then.constant_mut(index))
                .or_else(move || otherwise.constant_mut(index)),
        }
    }

    /// The `index`-th node in pre-order.
    fn node_mut(&mut self, index: &mut usize) -> Option<&mut FloatGene> {
        if *index == 0 {
            return Some(self);
        }
        *index -= 1;
        match self {
            Self::Input(_) | Self::Const(_) => None,
            Self::Unary(_, operand) => operand.node_mut(index),
            Self::Binary(_, lhs, rhs) => lhs.node_mut(index).or_else(move || rhs.node_mut(index)),
            Self::IfPositive(cond, then, otherwise) => cond
                .node_mut(index)
                .or_else(|| then.node_mut(index))
                .or_else(move || otherwise.node_mut(index)),
        }
    }

    fn node(&self, index: &mut usize) -> Option<&FloatGene> {
        if *index == 0 {
            return Some(self);
        }
        *index -= 1;
        match self {
            Self::Input(_) | Self::Const(_) => None,
            Self::Unary(_, operand) => operand.node(index),
            Self::Binary(_, lhs, rhs) => lhs.node(index).or_else(|| rhs.node(index)),
            Self::IfPositive(cond, then, otherwise) => cond
                .node(index)
                .or_else(|| then.node(index))
                .or_else(|| otherwise.node(index)),
        }
    }
}

/// An IRIS `f64` literal: always with a decimal point, never in exponent form.
fn float_literal(value: f64) -> String {
    if !value.is_finite() {
        return "0.0".to_owned();
    }
    let text = if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{value:.1}")
    } else {
        let fixed = format!("{value:.17}");
        let trimmed = fixed.trim_end_matches('0');
        if trimmed.ends_with('.') {
            format!("{trimmed}0")
        } else {
            trimmed.to_owned()
        }
    };
    if value < 0.0 {
        format!("({text})")
    } else {
        text
    }
}

/// Search parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FloatSearchConfig {
    pub inputs: usize,
    pub population_size: usize,
    pub generations: usize,
    pub crossover_rate: f64,
    pub mutation_rate: f64,
    pub tournament_size: usize,
    pub elite_count: usize,
    pub max_depth: usize,
    /// Added to the loss per tree node.
    pub parsimony_weight: f64,
    /// Stop early once the best loss is at or below this.
    pub target_loss: Option<f64>,
    pub seed: Option<u64>,
    /// Hill-climbing steps on the best tree's constants per generation.
    pub constant_steps: usize,
}

impl Default for FloatSearchConfig {
    fn default() -> Self {
        Self {
            inputs: 1,
            population_size: 200,
            generations: 40,
            crossover_rate: 0.7,
            mutation_rate: 0.3,
            tournament_size: 4,
            elite_count: 2,
            max_depth: 6,
            parsimony_weight: 1e-4,
            target_loss: None,
            seed: None,
            constant_steps: 40,
        }
    }
}

/// How well a gene fits the canary cases.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FloatScore {
    /// Mean absolute error, plus parsimony; infinite when any output is not
    /// finite.
    pub loss: f64,
    pub mean_absolute_error: f64,
    pub nodes: usize,
}

/// One generation's best, for progress reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FloatGenerationSummary {
    pub generation: usize,
    pub best: FloatScore,
    pub expression: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FloatSearchResult {
    pub best: FloatGene,
    pub score: FloatScore,
    pub generations_run: usize,
    pub reached_target: bool,
}

pub fn score(gene: &FloatGene, cases: &[ControlCanaryCase], parsimony_weight: f64) -> FloatScore {
    let nodes = gene.size();
    let mut total = 0.0;
    for case in cases {
        let output = gene.eval(&case.inputs);
        if !output.is_finite() {
            return FloatScore {
                loss: f64::INFINITY,
                mean_absolute_error: f64::INFINITY,
                nodes,
            };
        }
        total += (output - case.expected).abs();
    }
    let mean = if cases.is_empty() {
        0.0
    } else {
        total / cases.len() as f64
    };
    FloatScore {
        loss: mean + parsimony_weight * nodes as f64,
        mean_absolute_error: mean,
        nodes,
    }
}

/// Run the search; `progress` sees each generation's best.
pub fn search(
    config: &FloatSearchConfig,
    cases: &[ControlCanaryCase],
    mut progress: impl FnMut(&FloatGenerationSummary),
) -> Result<FloatSearchResult, String> {
    if config.inputs == 0 || config.inputs > super::MAX_CONTROL_INPUTS {
        return Err(format!(
            "f64 policies take 1..={} inputs, not {}",
            super::MAX_CONTROL_INPUTS,
            config.inputs
        ));
    }
    if cases.is_empty() {
        return Err("the search needs at least one canary case".to_owned());
    }
    if let Some(case) = cases.iter().find(|case| case.inputs.len() != config.inputs) {
        return Err(format!(
            "a canary case has {} inputs; the policy takes {}",
            case.inputs.len(),
            config.inputs
        ));
    }
    if config.population_size < 2 || config.max_depth < 1 {
        return Err("the population needs at least two trees of depth at least 1".to_owned());
    }
    let mut rng = match config.seed {
        Some(seed) => StdRng::seed_from_u64(seed),
        None => StdRng::from_entropy(),
    };

    // Ramped half-and-half initialisation, plus the constant-zero policy and
    // the least-squares affine policy: control laws are often close to
    // linear, and starting from the best line lets the search spend its time
    // on the nonlinear part.
    let mut population: Vec<FloatGene> = Vec::with_capacity(config.population_size);
    population.push(FloatGene::Const(0.0));
    if let Some(seed) = linear_seed(cases, config.inputs) {
        population.push(seed);
    }
    while population.len() < config.population_size {
        let depth = rng.gen_range(1..=config.max_depth.clamp(1, 4));
        let full = rng.gen_bool(0.5);
        population.push(random_tree(&mut rng, config.inputs, depth, full));
    }
    let mut scores: Vec<Scaled> = population
        .iter()
        .map(|gene| scaled_score(gene, cases, config.parsimony_weight))
        .collect();

    let mut generations_run = 0;
    let mut reached_target = false;
    for generation in 1..=config.generations {
        generations_run = generation;
        let mut order: Vec<usize> = (0..population.len()).collect();
        order.sort_by(|&a, &b| scores[a].score.loss.total_cmp(&scores[b].score.loss));
        // Polish the leading trees' constants: GP finds structure well and
        // constants poorly.
        for &index in order.iter().take(POLISHED_PER_GENERATION) {
            let (polished, polished_score) = tune_constants(
                &population[index],
                cases,
                config.parsimony_weight,
                config.constant_steps,
                &mut rng,
            );
            if polished_score.score.loss < scores[index].score.loss {
                population[index] = polished;
                scores[index] = polished_score;
            }
        }
        order.sort_by(|&a, &b| scores[a].score.loss.total_cmp(&scores[b].score.loss));
        let best = order[0];
        progress(&FloatGenerationSummary {
            generation,
            best: scores[best].score,
            expression: scores[best].apply(&population[best]).to_iris_expr(),
        });
        if config
            .target_loss
            .is_some_and(|target| scores[best].score.loss <= target)
        {
            reached_target = true;
            break;
        }
        if generation == config.generations {
            break;
        }

        let losses: Vec<f64> = scores.iter().map(|scored| scored.score.loss).collect();
        let mut next: Vec<FloatGene> = order
            .iter()
            .take(config.elite_count.min(population.len()))
            .map(|&index| population[index].clone())
            .collect();
        while next.len() < config.population_size {
            let parent = tournament(&mut rng, &losses, config.tournament_size);
            let mut child = population[parent].clone();
            if rng.gen_bool(config.crossover_rate.clamp(0.0, 1.0)) {
                let donor = tournament(&mut rng, &losses, config.tournament_size);
                crossover(&mut rng, &mut child, &population[donor]);
            }
            if rng.gen_bool(config.mutation_rate.clamp(0.0, 1.0)) {
                mutate(&mut rng, &mut child, config.inputs, config.max_depth);
            }
            if child.depth() > config.max_depth {
                child = population[parent].clone();
            }
            next.push(child);
        }
        population = next;
        scores = population
            .iter()
            .map(|gene| scaled_score(gene, cases, config.parsimony_weight))
            .collect();
    }

    let best = (0..scores.len())
        .min_by(|&a, &b| scores[a].score.loss.total_cmp(&scores[b].score.loss))
        .unwrap_or(0);
    // The policy is the scaled tree, scored exactly as it will be rendered --
    // or the bare tree when the scaling is (numerically) the identity.
    let scaled = scores[best].apply(&population[best]);
    let scaled_score = score(&scaled, cases, config.parsimony_weight);
    let bare_score = score(&population[best], cases, config.parsimony_weight);
    let (policy, final_score) = if bare_score.loss <= scaled_score.loss {
        (population[best].clone(), bare_score)
    } else {
        (scaled, scaled_score)
    };
    Ok(FloatSearchResult {
        best: policy,
        score: final_score,
        generations_run,
        reached_target,
    })
}

/// How many of each generation's best trees get their constants polished.
const POLISHED_PER_GENERATION: usize = 3;

/// A tree's score under Keijzer's linear scaling: the tree `f` stands for
/// `offset + gain * f`, both fitted to the targets by least squares, so the
/// search only has to find the shape of a law, not its gain and offset.
#[derive(Debug, Clone, Copy)]
struct Scaled {
    score: FloatScore,
    offset: f64,
    gain: f64,
}

impl Scaled {
    fn apply(&self, gene: &FloatGene) -> FloatGene {
        if self.gain == 0.0 {
            return FloatGene::Const(self.offset);
        }
        let scaled = if self.gain == 1.0 {
            gene.clone()
        } else {
            FloatGene::Binary(
                FloatBinary::Mul,
                Box::new(FloatGene::Const(self.gain)),
                Box::new(gene.clone()),
            )
        };
        if self.offset == 0.0 {
            scaled
        } else {
            FloatGene::Binary(
                FloatBinary::Add,
                Box::new(scaled),
                Box::new(FloatGene::Const(self.offset)),
            )
        }
    }
}

fn scaled_score(gene: &FloatGene, cases: &[ControlCanaryCase], parsimony_weight: f64) -> Scaled {
    let nodes = gene.size();
    let infinite = Scaled {
        score: FloatScore {
            loss: f64::INFINITY,
            mean_absolute_error: f64::INFINITY,
            nodes,
        },
        offset: 0.0,
        gain: 1.0,
    };
    let mut outputs = Vec::with_capacity(cases.len());
    for case in cases {
        let output = gene.eval(&case.inputs);
        if !output.is_finite() {
            return infinite;
        }
        outputs.push(output);
    }
    let count = cases.len() as f64;
    let mean_output = outputs.iter().sum::<f64>() / count;
    let mean_target = cases.iter().map(|case| case.expected).sum::<f64>() / count;
    let mut covariance = 0.0;
    let mut variance = 0.0;
    for (output, case) in outputs.iter().zip(cases) {
        covariance += (output - mean_output) * (case.expected - mean_target);
        variance += (output - mean_output).powi(2);
    }
    let (offset, gain) = if variance < 1e-12 {
        (mean_target, 0.0)
    } else {
        let gain = covariance / variance;
        (mean_target - gain * mean_output, gain)
    };
    if !offset.is_finite() || !gain.is_finite() {
        return infinite;
    }
    let error = outputs
        .iter()
        .zip(cases)
        .map(|(output, case)| (offset + gain * output - case.expected).abs())
        .sum::<f64>()
        / count;
    if !error.is_finite() {
        return infinite;
    }
    Scaled {
        score: FloatScore {
            loss: error + parsimony_weight * nodes as f64,
            mean_absolute_error: error,
            nodes,
        },
        offset,
        gain,
    }
}

/// The least-squares affine policy `w . x + c` over the cases, as a balanced
/// sum tree (so its depth grows with log2 of the input count); `None` when the
/// normal equations are singular.
fn linear_seed(cases: &[ControlCanaryCase], inputs: usize) -> Option<FloatGene> {
    let n = inputs + 1;
    // Augmented normal equations [X^T X | X^T y], with a whisker of ridge.
    let mut a = vec![vec![0.0; n + 1]; n];
    for case in cases {
        let row: Vec<f64> = case
            .inputs
            .iter()
            .copied()
            .chain(std::iter::once(1.0))
            .collect();
        for i in 0..n {
            for j in 0..n {
                a[i][j] += row[i] * row[j];
            }
            a[i][n] += row[i] * case.expected;
        }
    }
    for (i, row) in a.iter_mut().enumerate() {
        row[i] += 1e-9;
    }
    // Gauss-Jordan elimination with partial pivoting.
    for col in 0..n {
        let pivot = (col..n).max_by(|&r, &s| a[r][col].abs().total_cmp(&a[s][col].abs()))?;
        if a[pivot][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, pivot);
        for r in 0..n {
            if r != col {
                let factor = a[r][col] / a[col][col];
                let (pivot_row, target_row) = if r < col {
                    let (lo, hi) = a.split_at_mut(col);
                    (&hi[0], &mut lo[r])
                } else {
                    let (lo, hi) = a.split_at_mut(r);
                    (&lo[col], &mut hi[0])
                };
                for (dst, &src) in target_row[col..=n].iter_mut().zip(&pivot_row[col..=n]) {
                    *dst -= factor * src;
                }
            }
        }
    }
    let weights: Vec<f64> = (0..n).map(|i| a[i][n] / a[i][i]).collect();
    if weights.iter().any(|w| !w.is_finite()) {
        return None;
    }
    let mut terms: Vec<FloatGene> = (0..inputs)
        .map(|i| {
            FloatGene::Binary(
                FloatBinary::Mul,
                Box::new(FloatGene::Const(weights[i])),
                Box::new(FloatGene::Input(i)),
            )
        })
        .collect();
    terms.push(FloatGene::Const(weights[inputs]));
    while terms.len() > 1 {
        terms = terms
            .chunks(2)
            .map(|pair| match pair {
                [a, b] => {
                    FloatGene::Binary(FloatBinary::Add, Box::new(a.clone()), Box::new(b.clone()))
                }
                [a] => a.clone(),
                _ => unreachable!("chunks(2) yields one or two"),
            })
            .collect();
    }
    terms.pop()
}

fn tournament(rng: &mut StdRng, losses: &[f64], size: usize) -> usize {
    let mut best = rng.gen_range(0..losses.len());
    for _ in 1..size.max(1) {
        let challenger = rng.gen_range(0..losses.len());
        if losses[challenger] < losses[best] {
            best = challenger;
        }
    }
    best
}

fn random_terminal(rng: &mut StdRng, inputs: usize) -> FloatGene {
    if rng.gen_bool(0.6) {
        FloatGene::Input(rng.gen_range(0..inputs))
    } else {
        // Small "round" constants are easier for the constant tuner to refine.
        FloatGene::Const((rng.gen_range(-40..=40) as f64) / 10.0)
    }
}

fn random_tree(rng: &mut StdRng, inputs: usize, depth: usize, full: bool) -> FloatGene {
    if depth <= 1 || (!full && rng.gen_bool(0.3)) {
        return random_terminal(rng, inputs);
    }
    let roll: f64 = rng.gen();
    if roll < 0.25 {
        let op = UNARY[rng.gen_range(0..UNARY.len())];
        FloatGene::Unary(op, Box::new(random_tree(rng, inputs, depth - 1, full)))
    } else if roll < 0.9 {
        let op = BINARY[rng.gen_range(0..BINARY.len())];
        FloatGene::Binary(
            op,
            Box::new(random_tree(rng, inputs, depth - 1, full)),
            Box::new(random_tree(rng, inputs, depth - 1, full)),
        )
    } else {
        FloatGene::IfPositive(
            Box::new(random_tree(rng, inputs, depth - 1, full)),
            Box::new(random_tree(rng, inputs, depth - 1, full)),
            Box::new(random_tree(rng, inputs, depth - 1, full)),
        )
    }
}

fn crossover(rng: &mut StdRng, child: &mut FloatGene, donor: &FloatGene) {
    let mut donor_index = rng.gen_range(0..donor.size());
    let Some(graft) = donor.node(&mut donor_index).cloned() else {
        return;
    };
    let mut target_index = rng.gen_range(0..child.size());
    if let Some(slot) = child.node_mut(&mut target_index) {
        *slot = graft;
    }
}

fn mutate(rng: &mut StdRng, child: &mut FloatGene, inputs: usize, max_depth: usize) {
    let mut index = rng.gen_range(0..child.size());
    let Some(slot) = child.node_mut(&mut index) else {
        return;
    };
    match rng.gen_range(0..3) {
        // Replace a subtree.
        0 => {
            let depth = rng.gen_range(1..=max_depth.min(3));
            *slot = random_tree(rng, inputs, depth, false);
        }
        // Change one operator or terminal, keeping the shape.
        1 => match slot {
            FloatGene::Input(input) => *input = rng.gen_range(0..inputs),
            FloatGene::Const(value) => *value += gaussian(rng) * 0.5,
            FloatGene::Unary(op, _) => *op = UNARY[rng.gen_range(0..UNARY.len())],
            FloatGene::Binary(op, _, _) => *op = BINARY[rng.gen_range(0..BINARY.len())],
            FloatGene::IfPositive(..) => {}
        },
        // Shrink: hoist a child into this position.
        _ => {
            let replacement = match slot {
                FloatGene::Unary(_, operand) => Some((**operand).clone()),
                FloatGene::Binary(_, lhs, rhs) => Some(if rng.gen_bool(0.5) {
                    (**lhs).clone()
                } else {
                    (**rhs).clone()
                }),
                FloatGene::IfPositive(_, then, otherwise) => Some(if rng.gen_bool(0.5) {
                    (**then).clone()
                } else {
                    (**otherwise).clone()
                }),
                _ => None,
            };
            if let Some(replacement) = replacement {
                *slot = replacement;
            }
        }
    }
}

/// Stochastic hill climbing on a tree's constants.
fn tune_constants(
    gene: &FloatGene,
    cases: &[ControlCanaryCase],
    parsimony_weight: f64,
    steps: usize,
    rng: &mut StdRng,
) -> (FloatGene, Scaled) {
    let mut best = gene.clone();
    let mut best_score = scaled_score(&best, cases, parsimony_weight);
    let mut step_size = 0.5;
    let constants = gene.constant_count();
    if constants == 0 {
        return (best, best_score);
    }
    for _ in 0..steps {
        let mut trial = best.clone();
        let mut pick = rng.gen_range(0..constants);
        let noise = gaussian(rng) * step_size;
        if let Some(value) = trial.constant_mut(&mut pick) {
            *value += noise;
        }
        let trial_score = scaled_score(&trial, cases, parsimony_weight);
        if trial_score.score.loss < best_score.score.loss {
            best = trial;
            best_score = trial_score;
        } else {
            step_size = (step_size * 0.9).max(1e-6);
        }
    }
    (best, best_score)
}

/// A standard normal sample (Box-Muller).
fn gaussian(rng: &mut StdRng) -> f64 {
    let u1: f64 = rng.gen_range(f64::EPSILON..1.0);
    let u2: f64 = rng.gen();
    (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cases(f: impl Fn(f64, f64) -> f64) -> Vec<ControlCanaryCase> {
        let mut cases = Vec::new();
        for i in -4..=4 {
            for j in -2..=2 {
                let (a, b) = (i as f64 * 0.5, j as f64 * 0.7);
                cases.push(ControlCanaryCase {
                    inputs: vec![a, b],
                    expected: f(a, b),
                });
            }
        }
        cases
    }

    #[test]
    fn finds_a_linear_control_law() {
        let cases = cases(|position, velocity| -1.5 * position - 0.4 * velocity);
        let config = FloatSearchConfig {
            inputs: 2,
            population_size: 300,
            generations: 60,
            seed: Some(7),
            target_loss: Some(1e-3),
            ..FloatSearchConfig::default()
        };
        let result = search(&config, &cases, |_| {}).unwrap();
        assert!(
            result.score.mean_absolute_error < 0.05,
            "best {} with error {}",
            result.best.to_iris_expr(),
            result.score.mean_absolute_error
        );
    }

    #[test]
    fn improves_on_the_line_for_a_saturating_law() {
        let cases = cases(|position, _| -(1.0 - 2.0 / ((4.0 * position).exp() + 1.0)));
        let line = linear_seed(&cases, 2).unwrap();
        let line_error = score(&line, &cases, 0.0).mean_absolute_error;
        let config = FloatSearchConfig {
            inputs: 2,
            population_size: 300,
            generations: 60,
            seed: Some(11),
            ..FloatSearchConfig::default()
        };
        let result = search(&config, &cases, |_| {}).unwrap();
        assert!(
            result.score.mean_absolute_error < line_error / 4.0,
            "best {} with error {} vs the line's {line_error}",
            result.best.to_iris_expr(),
            result.score.mean_absolute_error
        );
    }

    #[test]
    fn rendered_candidates_compile_and_agree_with_the_tree() {
        let gene = FloatGene::Binary(
            FloatBinary::Div,
            Box::new(FloatGene::Unary(
                FloatUnary::Tanh,
                Box::new(FloatGene::Input(0)),
            )),
            Box::new(FloatGene::IfPositive(
                Box::new(FloatGene::Input(1)),
                Box::new(FloatGene::Const(-2.5)),
                Box::new(FloatGene::Unary(
                    FloatUnary::SqrtAbs,
                    Box::new(FloatGene::Input(1)),
                )),
            )),
        );
        let source = gene.to_iris_source("policy", 2);
        let module = crate::evolution::compile_candidate(&source, "float_gene").unwrap();
        let function = module.function_by_name("policy").unwrap();
        for inputs in [[0.3, 1.0], [-2.0, -4.0], [5.0, 0.0]] {
            let args: Vec<crate::interp::IrValue> = inputs
                .iter()
                .map(|x| crate::interp::IrValue::F64(*x))
                .collect();
            let output = crate::interp::eval_function_in_module_opts(
                &module,
                function,
                &args,
                crate::interp::InterpOptions::default(),
            )
            .unwrap();
            let crate::interp::IrValue::F64(output) = output[0] else {
                panic!("expected an f64, got {:?}", output[0]);
            };
            let expected = gene.eval(&inputs);
            assert!(
                (output - expected).abs() < 1e-12,
                "{inputs:?}: interpreter {output}, tree {expected}"
            );
        }
    }

    #[test]
    fn float_literals_are_valid_iris() {
        assert_eq!(float_literal(3.0), "3.0");
        assert_eq!(float_literal(-0.25), "(-0.25)");
        assert_eq!(float_literal(1e-9), "0.000000001");
        assert!(!float_literal(123456.789).contains('e'));
    }
}
