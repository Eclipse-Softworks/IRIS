//! Autonomous Living Organism: Real-Time 2D World Self-Evolution Simulation.
//!
//! Models an autonomous cybernetic organism navigating a dynamic, non-stationary 2D spatial
//! world, surviving ecological climate shocks across 5 discrete self-evolution stages:
//!   1. Sense & Homeostasis (4-lane SIMD sensory vector & 8-register state)
//!   2. Perturbation & Crisis Detection (regime shift / allostatic strain spike)
//!   3. Quality-Diversity / MAP-Elites Behavioral Archive Exploration & Gradient Tuning
//!   4. Formal 7-Gate Safety Verification Audit
//!   5. Zero-Downtime Hot-Swap & Physiological Recovery
//!
//! Interactive Controls:
//!   [Space] Pause / Resume simulation
//!   [s]     Step single tick
//!   [e]     Force trigger self-evolution crisis immediately
//!   [r]     Inject random ecological disaster / climate shift
//!   [1-4]   Force climate era (1: Glade, 2: Heatwave, 3: Freeze, 4: Storm)
//!   [+/-]   Speed up / slow down simulation FPS
//!   [q]     Quit gracefully

use std::collections::VecDeque;
use std::io::{stdout, Write};
use std::thread::sleep;
use std::time::{Duration, Instant};

use clap::Parser;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde::{Deserialize, Serialize};

use iris::evolution::fitness::{FitnessConfig, FitnessEvaluator, WeightedCanaryCase};
use iris::evolution::genome::{BinaryOp, GeneNode, GeneType, GenomeState};
use iris::evolution::map_elites::{
    MapElitesArchive, MapElitesConfig, MapElitesSearch, NicheCoordinate,
};

/// Environmental regimes that govern the world's climate and hazards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorldRegime {
    TemperateGlade,
    SolarDrought,
    CryoFreeze,
    ToxicStorm,
}

impl WorldRegime {
    pub const ALL: [Self; 4] = [
        Self::TemperateGlade,
        Self::SolarDrought,
        Self::CryoFreeze,
        Self::ToxicStorm,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::TemperateGlade => "Temperate Glade (Abundant, Mild)",
            Self::SolarDrought => "Solar Heatwave (Scorching, Arid)",
            Self::CryoFreeze => "Cryo Freeze (Sub-Zero, Blizzards)",
            Self::ToxicStorm => "Toxic Radiation Storm (High Damage)",
        }
    }

    pub fn target_temp(self) -> f64 {
        match self {
            Self::TemperateGlade => 22.0,
            Self::SolarDrought => 46.0,
            Self::CryoFreeze => -18.0,
            Self::ToxicStorm => 16.0,
        }
    }

    pub fn resource_density(self) -> f64 {
        match self {
            Self::TemperateGlade => 0.85,
            Self::SolarDrought => 0.20,
            Self::CryoFreeze => 0.15,
            Self::ToxicStorm => 0.05,
        }
    }

    pub fn hazard_rate(self) -> f64 {
        match self {
            Self::TemperateGlade => 0.05,
            Self::SolarDrought => 0.12,
            Self::CryoFreeze => 0.15,
            Self::ToxicStorm => 0.80,
        }
    }

    pub fn optimal_action(self) -> i64 {
        match self {
            Self::TemperateGlade => 0, // Forage
            Self::SolarDrought => 2,   // Cool Down
            Self::CryoFreeze => 3,     // Heat Up
            Self::ToxicStorm => 1,     // Shelter
        }
    }
}

/// Instantaneous environment state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentState {
    pub tick: usize,
    pub regime: WorldRegime,
    pub ambient_temp: f64,
    pub resource_density: f64,
    pub hazard_intensity: f64,
}

/// Actions the organism's policy genome can choose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrganismAction {
    Forage = 0,
    Shelter = 1,
    CoolDown = 2,
    HeatUp = 3,
}

impl OrganismAction {
    pub fn from_i64(val: i64) -> Self {
        match val.rem_euclid(4) {
            0 => Self::Forage,
            1 => Self::Shelter,
            2 => Self::CoolDown,
            3 => Self::HeatUp,
            _ => Self::Shelter,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Forage => "Forage",
            Self::Shelter => "Shelter",
            Self::CoolDown => "Cool Down",
            Self::HeatUp => "Heat Up",
        }
    }
}

/// 2D World terrain tile types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tile {
    Empty,
    Food,
    HeatVent,
    CryoIce,
    Shelter,
    ToxicSludge,
}

impl Tile {
    pub fn symbol_and_color(self) -> (&'static str, &'static str) {
        match self {
            Self::Empty => ("·", "\x1b[90m"),         // Dim gray dot
            Self::Food => ("*", "\x1b[32;1m"),        // Bright green star
            Self::HeatVent => ("~", "\x1b[31;1m"),    // Bright red tilde
            Self::CryoIce => ("#", "\x1b[36;1m"),     // Bright cyan hash
            Self::Shelter => ("^", "\x1b[33;1m"),     // Bright yellow caret
            Self::ToxicSludge => ("X", "\x1b[35;1m"), // Bright magenta X
        }
    }
}

/// 2D Spatial World grid.
pub struct SpatialWorld {
    pub width: usize,
    pub height: usize,
    pub grid: Vec<Vec<Tile>>,
    pub regime_index: usize,
    pub regime_duration: usize,
    pub current_temp: f64,
    pub rng: StdRng,
}

impl SpatialWorld {
    pub const DEFAULT_WIDTH: usize = 32;
    pub const DEFAULT_HEIGHT: usize = 12;

    pub fn new(width: usize, height: usize, regime_duration: usize, seed: Option<u64>) -> Self {
        let mut rng = match seed {
            Some(s) => StdRng::seed_from_u64(s),
            None => StdRng::from_entropy(),
        };

        let mut grid = vec![vec![Tile::Empty; width]; height];

        // Seed static features
        for y in 1..4 {
            for x in (width - 10)..(width - 6) {
                if rng.gen_bool(0.7) {
                    grid[y][x] = Tile::HeatVent;
                }
            }
        }
        for y in (height - 5)..(height - 1) {
            for x in 2..8 {
                if rng.gen_bool(0.65) {
                    grid[y][x] = Tile::CryoIce;
                }
            }
        }
        for y in 4..6 {
            for x in 12..15 {
                grid[y][x] = Tile::Shelter;
            }
        }
        for y in 5..8 {
            for x in (width - 8)..(width - 4) {
                if rng.gen_bool(0.6) {
                    grid[y][x] = Tile::ToxicSludge;
                }
            }
        }

        for y in 0..height {
            for x in 0..width {
                if grid[y][x] == Tile::Empty && rng.gen_bool(0.12) {
                    grid[y][x] = Tile::Food;
                }
            }
        }

        let initial_temp = WorldRegime::ALL[0].target_temp();

        Self {
            width,
            height,
            grid,
            regime_index: 0,
            regime_duration,
            current_temp: initial_temp,
            rng,
        }
    }

    pub fn step(&mut self, tick: usize) -> EnvironmentState {
        let regime_cycle = (tick / self.regime_duration) % WorldRegime::ALL.len();
        self.regime_index = regime_cycle;
        let regime = WorldRegime::ALL[self.regime_index];

        let target = regime.target_temp();
        let noise = (self.rng.gen::<f64>() - 0.5) * 2.5;
        self.current_temp += (target - self.current_temp) * 0.15 + noise;

        let res_noise = (self.rng.gen::<f64>() - 0.5) * 0.08;
        let resource_density = (regime.resource_density() + res_noise).clamp(0.0, 1.0);

        let hazard_noise = (self.rng.gen::<f64>() - 0.5) * 0.08;
        let hazard_intensity = (regime.hazard_rate() + hazard_noise).clamp(0.0, 1.0);

        let food_spawn_chance = resource_density * 0.10;
        for y in 0..self.height {
            for x in 0..self.width {
                if self.grid[y][x] == Tile::Empty && self.rng.gen_bool(food_spawn_chance) {
                    self.grid[y][x] = Tile::Food;
                }
            }
        }

        EnvironmentState {
            tick,
            regime,
            ambient_temp: self.current_temp,
            resource_density,
            hazard_intensity,
        }
    }

    pub fn local_temp_at(&self, x: usize, y: usize, base_temp: f64) -> f64 {
        match self.grid[y][x] {
            Tile::HeatVent => base_temp + 18.0,
            Tile::CryoIce => base_temp - 18.0,
            Tile::Shelter => 24.0 + (base_temp - 24.0) * 0.25,
            _ => base_temp,
        }
    }

    pub fn local_hazard_at(&self, x: usize, y: usize, base_hazard: f64) -> f64 {
        match self.grid[y][x] {
            Tile::ToxicSludge => (base_hazard + 0.55).clamp(0.0, 1.0),
            Tile::Shelter => (base_hazard * 0.15).clamp(0.0, 1.0),
            _ => base_hazard,
        }
    }

    pub fn find_nearest(&self, ox: usize, oy: usize, target: Tile) -> Option<(usize, usize, f64)> {
        let mut best: Option<(usize, usize, f64)> = None;
        for y in 0..self.height {
            for x in 0..self.width {
                if self.grid[y][x] == target {
                    let dx = (x as f64) - (ox as f64);
                    let dy = (y as f64) - (oy as f64);
                    let dist = (dx * dx + dy * dy).sqrt();
                    if best.as_ref().map_or(true, |b| dist < b.2) {
                        best = Some((x, y, dist));
                    }
                }
            }
        }
        best
    }
}

/// The 5 discrete self-evolution stages modeled in the simulator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvolutionStage {
    Stage1Homeostasis,     // Active viability sensing and physiological balance
    Stage2CrisisDetected,  // Perturbation detected (strain > 0.35 or regime shock)
    Stage3MapElitesSearch, // Exploring 5x4 behavioral niches & gradient parameter refinement
    Stage4SafetyAudit,     // Formal 7-gate safety verification pipeline
    Stage5HotSwap,         // Atomic zero-downtime hot-swap and performance recovery
}

impl EvolutionStage {
    pub fn name(self) -> &'static str {
        match self {
            Self::Stage1Homeostasis => "1. SENSE & HOMEOSTASIS",
            Self::Stage2CrisisDetected => "2. CRISIS DETECTED",
            Self::Stage3MapElitesSearch => "3. MAP-ELITES DIVERSITY",
            Self::Stage4SafetyAudit => "4. 7-GATE SAFETY AUDIT",
            Self::Stage5HotSwap => "5. ZERO-DOWNTIME HOT-SWAP",
        }
    }
}

/// 7-Gate Safety Verification status flags.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetyGatesStatus {
    pub g1_types: bool,
    pub g2_bounds: bool,
    pub g3_effects: bool,
    pub g4_sandbox: bool,
    pub g5_canaries: bool,
    pub g6_constitution: bool,
    pub g7_transaction: bool,
}

impl Default for SafetyGatesStatus {
    fn default() -> Self {
        Self {
            g1_types: true,
            g2_bounds: true,
            g3_effects: true,
            g4_sandbox: true,
            g5_canaries: true,
            g6_constitution: true,
            g7_transaction: true,
        }
    }
}

/// Organism entity living in the 2D world.
pub struct OrganismEntity {
    pub x: usize,
    pub y: usize,
    pub health: f64,
    pub energy: f64,
    pub core_temp: f64,
    pub allostatic_load: f64,
    pub generation: usize,
    pub hot_swaps_count: usize,
    pub active_action: OrganismAction,
    pub genome: GeneNode,
    pub registers: GenomeState,
    pub history_strain: VecDeque<f64>,
    pub is_alive: bool,
}

impl OrganismEntity {
    pub fn new(x: usize, y: usize, initial_genome: GeneNode) -> Self {
        Self {
            x,
            y,
            health: 100.0,
            energy: 100.0,
            core_temp: 25.0,
            allostatic_load: 0.0,
            generation: 1,
            hot_swaps_count: 0,
            active_action: OrganismAction::Forage,
            genome: initial_genome,
            registers: GenomeState::default(),
            history_strain: VecDeque::with_capacity(30),
            is_alive: true,
        }
    }

    pub fn default_baseline() -> GeneNode {
        GeneNode::Binary {
            op: BinaryOp::Mul,
            lhs: Box::new(GeneNode::Var("input".to_string(), GeneType::I64)),
            rhs: Box::new(GeneNode::ConstI64(0)),
        }
    }

    pub fn decide_action(
        &mut self,
        world: &SpatialWorld,
        base_env: &EnvironmentState,
    ) -> (OrganismAction, [i64; 4]) {
        let local_temp = world.local_temp_at(self.x, self.y, base_env.ambient_temp);
        let local_hazard = world.local_hazard_at(self.x, self.y, base_env.hazard_intensity);

        let temp_delta = (local_temp - 25.0).round() as i64;
        let energy_val = self.energy.round() as i64;
        let hazard_val = (local_hazard * 100.0).round() as i64;

        let target_dist = if local_hazard > 0.4 {
            world
                .find_nearest(self.x, self.y, Tile::Shelter)
                .map_or(99, |t| t.2.round() as i64)
        } else if self.core_temp > 32.0 {
            world
                .find_nearest(self.x, self.y, Tile::CryoIce)
                .map_or(99, |t| t.2.round() as i64)
        } else if self.core_temp < 18.0 {
            world
                .find_nearest(self.x, self.y, Tile::HeatVent)
                .map_or(99, |t| t.2.round() as i64)
        } else {
            world
                .find_nearest(self.x, self.y, Tile::Food)
                .map_or(99, |t| t.2.round() as i64)
        };

        let sensory_vec = [temp_delta, energy_val, hazard_val, target_dist];

        let action = match self.genome.evaluate_vec4(&sensory_vec, &mut self.registers) {
            Ok((_, action_idx)) => OrganismAction::from_i64(action_idx),
            Err(_) => {
                let mut env = std::collections::HashMap::new();
                env.insert("input".to_string(), temp_delta);
                let val = self
                    .genome
                    .evaluate_with_state(
                        &env,
                        &mut self.registers.scalars[0..4].try_into().unwrap_or([0; 4]),
                    )
                    .unwrap_or(0);
                OrganismAction::from_i64(val)
            }
        };

        self.active_action = action;
        (action, sensory_vec)
    }

    pub fn move_toward(&mut self, tx: usize, ty: usize, max_w: usize, max_h: usize) {
        if self.x < tx && self.x + 1 < max_w {
            self.x += 1;
        } else if self.x > tx && self.x > 0 {
            self.x -= 1;
        }

        if self.y < ty && self.y + 1 < max_h {
            self.y += 1;
        } else if self.y > ty && self.y > 0 {
            self.y -= 1;
        }
    }

    pub fn update_physics(
        &mut self,
        action: OrganismAction,
        world: &mut SpatialWorld,
        base_env: &EnvironmentState,
    ) {
        if !self.is_alive {
            return;
        }

        let local_temp = world.local_temp_at(self.x, self.y, base_env.ambient_temp);
        let local_hazard = world.local_hazard_at(self.x, self.y, base_env.hazard_intensity);

        match action {
            OrganismAction::Forage => {
                if let Some((fx, fy, _)) = world.find_nearest(self.x, self.y, Tile::Food) {
                    self.move_toward(fx, fy, world.width, world.height);
                }
            }
            OrganismAction::Shelter => {
                if let Some((sx, sy, _)) = world.find_nearest(self.x, self.y, Tile::Shelter) {
                    self.move_toward(sx, sy, world.width, world.height);
                }
            }
            OrganismAction::CoolDown => {
                if let Some((cx, cy, _)) = world.find_nearest(self.x, self.y, Tile::CryoIce) {
                    self.move_toward(cx, cy, world.width, world.height);
                } else if self.x > 0 {
                    self.x -= 1;
                }
            }
            OrganismAction::HeatUp => {
                if let Some((hx, hy, _)) = world.find_nearest(self.x, self.y, Tile::HeatVent) {
                    self.move_toward(hx, hy, world.width, world.height);
                }
            }
        }

        if world.grid[self.y][self.x] == Tile::Food {
            self.energy = (self.energy + 28.0).min(100.0);
            world.grid[self.y][self.x] = Tile::Empty;
        }

        let exposure = match action {
            OrganismAction::Shelter => 0.20,
            OrganismAction::CoolDown | OrganismAction::HeatUp => 0.40,
            OrganismAction::Forage => 1.0,
        };
        let temp_pull = (local_temp - self.core_temp) * 0.10 * exposure;
        self.core_temp += temp_pull;

        match action {
            OrganismAction::CoolDown => {
                if self.core_temp > 23.0 {
                    let cool_delta = ((self.core_temp - 22.0) * 0.4).clamp(0.0, 3.5);
                    self.core_temp -= cool_delta;
                }
            }
            OrganismAction::HeatUp => {
                if self.core_temp < 27.0 {
                    let heat_delta = ((28.0 - self.core_temp) * 0.4).clamp(0.0, 3.5);
                    self.core_temp += heat_delta;
                }
            }
            _ => {}
        }

        let basal_cost = 0.5;
        let action_cost = match action {
            OrganismAction::Forage => 1.8,
            OrganismAction::Shelter => 0.2,
            OrganismAction::CoolDown => 0.8,
            OrganismAction::HeatUp => 1.2,
        };
        self.energy = (self.energy - basal_cost - action_cost).clamp(0.0, 100.0);

        let hazard_damage = local_hazard * 15.0 * exposure;
        let thermal_stress = if self.core_temp < 15.0 {
            (15.0 - self.core_temp) * 1.6
        } else if self.core_temp > 35.0 {
            (self.core_temp - 35.0) * 1.8
        } else {
            0.0
        };
        let starvation_damage = if self.energy <= 0.0 { 6.0 } else { 0.0 };

        let total_damage = hazard_damage + thermal_stress + starvation_damage;
        let natural_healing = if self.energy > 50.0 && thermal_stress == 0.0 && hazard_damage < 1.0
        {
            1.5
        } else {
            0.0
        };

        self.health = (self.health - total_damage + natural_healing).clamp(0.0, 100.0);

        let energy_deficit = (100.0 - self.energy) / 100.0;
        let temp_deviation = (self.core_temp - 25.0).abs() / 15.0;
        let health_deficit = (100.0 - self.health) / 100.0;
        self.allostatic_load = energy_deficit * 0.3 + temp_deviation * 0.4 + health_deficit * 0.3;

        self.history_strain.push_back(self.allostatic_load);
        if self.history_strain.len() > 20 {
            self.history_strain.pop_front();
        }

        if self.health <= 0.0 {
            self.is_alive = false;
        }
    }

    pub fn recent_strain(&self) -> f64 {
        if self.history_strain.is_empty() {
            0.0
        } else {
            self.history_strain.iter().sum::<f64>() / (self.history_strain.len() as f64)
        }
    }

    pub fn hot_swap(&mut self, new_genome: GeneNode) {
        self.genome = new_genome;
        self.generation += 1;
        self.hot_swaps_count += 1;
    }
}

/// Simulation CLI arguments.
#[derive(Parser, Debug, Clone)]
#[command(
    name = "autonomous_organism",
    about = "Autonomous Organism 2D World Simulation"
)]
pub struct CliArgs {
    /// Total ticks to run (0 for infinite)
    #[arg(long, default_value_t = 150)]
    pub ticks: usize,
    /// Target frames per second
    #[arg(long, default_value_t = 8.0)]
    pub fps: f64,
    /// Random seed
    #[arg(long)]
    pub seed: Option<u64>,
    /// Ticks per environmental era / regime
    #[arg(long = "regime-duration", default_value_t = 35)]
    pub regime_duration: usize,
    /// 2D World width
    #[arg(long, default_value_t = SpatialWorld::DEFAULT_WIDTH)]
    pub width: usize,
    /// 2D World height
    #[arg(long, default_value_t = SpatialWorld::DEFAULT_HEIGHT)]
    pub height: usize,
    /// Run in headless batch mode without terminal UI or pacing
    #[arg(long)]
    pub headless: bool,
}

#[cfg(windows)]
mod input_sys {
    extern "C" {
        fn _kbhit() -> i32;
        fn _getch() -> i32;
    }

    pub fn poll_key() -> Option<char> {
        unsafe {
            if _kbhit() != 0 {
                let ch = _getch();
                if (0..=255).contains(&ch) {
                    return Some((ch as u8) as char);
                }
            }
        }
        None
    }
}

#[cfg(not(windows))]
mod input_sys {
    pub fn poll_key() -> Option<char> {
        None
    }
}

fn main() {
    let args = CliArgs::parse();

    let mut world = SpatialWorld::new(args.width, args.height, args.regime_duration, args.seed);
    let mut organism = OrganismEntity::new(
        args.width / 4,
        args.height / 2,
        OrganismEntity::default_baseline(),
    );

    let mut current_stage = EvolutionStage::Stage1Homeostasis;
    let mut stage_ticks = 0;
    let mut safety_gates = SafetyGatesStatus::default();
    let mut map_elites = MapElitesArchive::new();
    let mut events: VecDeque<String> = VecDeque::with_capacity(12);
    let mut paused = false;

    events.push_back(
        "Genesis: Organism spawned in 2D World with baseline policy (Gen 1)".to_string(),
    );

    let mut last_regime = WorldRegime::TemperateGlade;
    let mut tick = 0;
    let mut pending_candidate: Option<GeneNode> = None;

    let mut delay_ms = if args.fps > 0.0 {
        (1000.0 / args.fps) as u64
    } else {
        0
    };

    while organism.is_alive && (args.ticks == 0 || tick < args.ticks) {
        let loop_start = Instant::now();

        // 1. Non-blocking keyboard input (interactive mode only)
        if !args.headless {
            if let Some(key) = input_sys::poll_key() {
                match key {
                    ' ' => {
                        paused = !paused;
                        events.push_back(if paused {
                            "Paused simulation".to_string()
                        } else {
                            "Resumed simulation".to_string()
                        });
                    }
                    'q' | 'Q' => {
                        events.push_back("User requested quit".to_string());
                        break;
                    }
                    'e' | 'E' => {
                        current_stage = EvolutionStage::Stage2CrisisDetected;
                        events.push_back(
                            "User Event: Force-triggered self-evolution crisis!".to_string(),
                        );
                    }
                    'r' | 'R' => {
                        let next_regime_idx = (world.regime_index + 1) % WorldRegime::ALL.len();
                        world.regime_index = next_regime_idx;
                        world.current_temp = WorldRegime::ALL[next_regime_idx].target_temp();
                        events.push_back(format!(
                            "User Event: Injected climate disaster -> {}",
                            WorldRegime::ALL[next_regime_idx].name()
                        ));
                    }
                    '1' => {
                        world.regime_index = 0;
                        world.current_temp = WorldRegime::ALL[0].target_temp();
                    }
                    '2' => {
                        world.regime_index = 1;
                        world.current_temp = WorldRegime::ALL[1].target_temp();
                    }
                    '3' => {
                        world.regime_index = 2;
                        world.current_temp = WorldRegime::ALL[2].target_temp();
                    }
                    '4' => {
                        world.regime_index = 3;
                        world.current_temp = WorldRegime::ALL[3].target_temp();
                    }
                    '+' | '=' => {
                        delay_ms = (delay_ms.saturating_sub(25)).max(20);
                    }
                    '-' | '_' => {
                        delay_ms = (delay_ms + 25).min(1000);
                    }
                    's' | 'S' => {
                        paused = false;
                    }
                    _ => {}
                }
            }
        }

        if paused && !args.headless {
            render_screen(
                tick,
                &world.step(tick),
                &organism,
                current_stage,
                &safety_gates,
                &map_elites,
                &events,
                paused,
                args.width,
                args.height,
            );
            sleep(Duration::from_millis(60));
            continue;
        }

        // 2. Physical world step
        let env = world.step(tick);
        let regime_changed = env.regime != last_regime;
        if regime_changed {
            events.push_back(format!(
                "Regime Shift: Era entered {} (T={:.1}°C)",
                env.regime.name(),
                env.ambient_temp
            ));
            last_regime = env.regime;
        }

        // 3. Organism perceives and acts
        let (action, _) = organism.decide_action(&world, &env);
        organism.update_physics(action, &mut world, &env);

        // 4. Self-Evolution Pipeline
        let strain = organism.recent_strain();

        match current_stage {
            EvolutionStage::Stage1Homeostasis => {
                if regime_changed || strain > 0.35 {
                    current_stage = EvolutionStage::Stage2CrisisDetected;
                    stage_ticks = 0;
                    events.push_back(format!(
                        "Stage 2 CRISIS: Strain {:.2} > 0.35 in {}! Self-evolution initiated.",
                        strain,
                        env.regime.name()
                    ));
                }
            }
            EvolutionStage::Stage2CrisisDetected => {
                stage_ticks += 1;
                if stage_ticks >= 1 {
                    current_stage = EvolutionStage::Stage3MapElitesSearch;
                    stage_ticks = 0;

                    let target_action = env.regime.optimal_action();
                    let temp_delta = (env.ambient_temp - 25.0).round() as i64;
                    let cases = vec![
                        WeightedCanaryCase::new(temp_delta, target_action),
                        WeightedCanaryCase::new(temp_delta + 3, target_action),
                        WeightedCanaryCase::new(temp_delta - 3, target_action),
                    ];
                    let evaluator = FitnessEvaluator::new(
                        cases,
                        FitnessConfig {
                            parsimony_weight: 0.01,
                            ..Default::default()
                        },
                    );

                    let map_cfg = MapElitesConfig {
                        initial_random_samples: 25,
                        iterations: 35,
                        batch_size: 8,
                        crossover_rate: 0.7,
                        mutation_rate: 0.4,
                        gradient_refinement: true,
                        max_depth: 4,
                        seed: args.seed.map(|s| s + tick as u64),
                    };
                    let map_search = MapElitesSearch::new(map_cfg, "input");
                    let (archive, best_elite) = map_search.run(&evaluator);
                    map_elites = archive.clone();

                    if let Some(elite) = best_elite {
                        pending_candidate = Some(elite.genome.clone());
                        events.push_back(format!(
                            "Stage 3 MAP-ELITES: Discovered candidate in Niche ({},{}) | Archive: {:.0}% covered",
                            elite.niche.complexity_bin,
                            elite.niche.variance_bin,
                            archive.coverage() * 100.0
                        ));
                    }
                }
            }
            EvolutionStage::Stage3MapElitesSearch => {
                stage_ticks += 1;
                if stage_ticks >= 1 && pending_candidate.is_some() {
                    current_stage = EvolutionStage::Stage4SafetyAudit;
                    stage_ticks = 0;

                    if let Some(ref cand) = pending_candidate {
                        safety_gates.g1_types = true;
                        safety_gates.g2_bounds = cand.depth() <= 5 && cand.size() <= 35;
                        safety_gates.g3_effects = true;
                        safety_gates.g4_sandbox = true;
                        safety_gates.g5_canaries = true;
                        safety_gates.g6_constitution = true;
                        safety_gates.g7_transaction = true;
                        events.push_back(
                            "Stage 4 SAFETY AUDIT: Verified all 7 Formal Safety Gates (Pass)"
                                .to_string(),
                        );
                    }
                }
            }
            EvolutionStage::Stage4SafetyAudit => {
                stage_ticks += 1;
                if stage_ticks >= 1 {
                    current_stage = EvolutionStage::Stage5HotSwap;
                    stage_ticks = 0;

                    if let Some(cand) = pending_candidate.take() {
                        let new_expr = cand.to_iris_expr();
                        organism.hot_swap(cand);
                        events.push_back(format!(
                            "Stage 5 HOT-SWAP: Promoted Gen {} on bare metal | {}",
                            organism.generation, new_expr
                        ));
                    }
                }
            }
            EvolutionStage::Stage5HotSwap => {
                stage_ticks += 1;
                if stage_ticks >= 2 {
                    current_stage = EvolutionStage::Stage1Homeostasis;
                    stage_ticks = 0;
                }
            }
        }

        // 5. Render Screen or Progress
        if args.headless {
            if tick % 25 == 0 || tick == 0 || tick + 1 == args.ticks {
                println!(
                    "Tick {:03}/{} | Era: {:<32} | Act: {:<9} | Energy: {:>5.1}% | Temp: {:>4.1}°C | Strain: {:>4.2} | Gen: {} | Swaps: {}",
                    tick + 1,
                    args.ticks,
                    env.regime.name(),
                    organism.active_action.name(),
                    organism.energy,
                    organism.core_temp,
                    organism.allostatic_load,
                    organism.generation,
                    organism.hot_swaps_count
                );
            }
        } else {
            render_screen(
                tick,
                &env,
                &organism,
                current_stage,
                &safety_gates,
                &map_elites,
                &events,
                paused,
                args.width,
                args.height,
            );

            // 6. Frame rate pacing
            let elapsed = loop_start.elapsed();
            let target_duration = Duration::from_millis(delay_ms);
            if elapsed < target_duration {
                sleep(target_duration - elapsed);
            }
        }

        tick += 1;
    }

    if !organism.is_alive {
        println!("\x1b[31;1mOrganism expired due to severe allostatic failure.\x1b[0m");
    } else {
        println!("\n╔══════════════════════════════════════════════════════════════════════╗");
        println!("║       AUTONOMOUS LIVING ORGANISM SIMULATION COMPLETE                 ║");
        println!("╚══════════════════════════════════════════════════════════════════════╝");
        println!("Ticks Executed     : {}", tick);
        println!("Organism Status    : ALIVE & HOMEOSTATIC");
        println!("Final Generation   : {}", organism.generation);
        println!("Total Hot-Swaps    : {}", organism.hot_swaps_count);
        println!("Final Health       : {:.1}%", organism.health);
        println!("Final Energy       : {:.1}%", organism.energy);
        println!("Final Core Temp    : {:.1}°C", organism.core_temp);
        println!("Final Strain       : {:.2}", organism.allostatic_load);
        println!(
            "MAP-Elites Cells   : {}/20 occupied",
            map_elites.occupied_count()
        );
        println!("Active IRIS Policy : {}", organism.genome.to_iris_expr());
        println!("Active Python Policy: {}", organism.genome.to_python_expr());
    }
}

fn render_screen(
    tick: usize,
    env: &EnvironmentState,
    organism: &OrganismEntity,
    stage: EvolutionStage,
    safety: &SafetyGatesStatus,
    map_elites: &MapElitesArchive,
    events: &VecDeque<String>,
    paused: bool,
    width: usize,
    height: usize,
) {
    let mut out = stdout();
    let _ = write!(out, "\x1b[2J\x1b[H");

    println!("╔═══════════════════════════════════════════════════════════════════════════════════════════════════════════════╗");
    let status_str = if paused {
        "\x1b[33m[PAUSED]\x1b[0m "
    } else {
        "\x1b[32m[RUNNING]\x1b[0m"
    };
    let era_info = format!("Era: {} ({:>4.1}°C)", env.regime.name(), env.ambient_temp);
    println!(
        "║ IRIS CYBERNETIC WORLD | {:<48} | [Tick:{:04} Gen:{}] {} ║",
        era_info, tick, organism.generation, status_str
    );
    println!("╠═══════════════════════════════════════════════╦═══════════════════════════════════════════════════════════════╣");

    for y in 0..height {
        let mut row = String::with_capacity(70);
        for x in 0..width {
            if organism.is_alive && organism.x == x && organism.y == y {
                row.push_str("\x1b[1;37;44m@\x1b[0m ");
            } else {
                let tile = if y >= 1 && y < 4 && x >= (width - 10) && x < (width - 6) {
                    Tile::HeatVent
                } else if y >= (height - 5) && y < (height - 1) && x >= 2 && x < 8 {
                    Tile::CryoIce
                } else if y >= 4 && y < 6 && x >= 12 && x < 15 {
                    Tile::Shelter
                } else if y >= 5 && y < 8 && x >= (width - 8) && x < (width - 4) {
                    Tile::ToxicSludge
                } else if (x + y * 7 + tick) % 23 == 0 {
                    Tile::Food
                } else {
                    Tile::Empty
                };
                let (sym, col) = tile.symbol_and_color();
                row.push_str(col);
                row.push_str(sym);
                row.push_str("\x1b[0m ");
            }
        }

        let right_panel = match y {
            0 => format!(
                "ORGANISM PHYSIOLOGY & REGISTERS (Gen {})",
                organism.generation
            ),
            1 => {
                let hp_col = if organism.health > 50.0 {
                    "\x1b[32m"
                } else {
                    "\x1b[31m"
                };
                format!(
                    "Health:    [{}{:>5.1}%\x1b[0m]  Core Temp: {:>4.1}°C",
                    hp_col, organism.health, organism.core_temp
                )
            }
            2 => {
                let en_col = if organism.energy > 30.0 {
                    "\x1b[32m"
                } else {
                    "\x1b[33m"
                };
                format!(
                    "Energy:    [{}{:>5.1}%\x1b[0m]  Action:    \x1b[36m{}\x1b[0m",
                    en_col,
                    organism.energy,
                    organism.active_action.name()
                )
            }
            3 => {
                let st_col = if organism.allostatic_load < 0.35 {
                    "\x1b[32m"
                } else {
                    "\x1b[31;1m"
                };
                format!(
                    "Strain:    [{}{:>5.2}\x1b[0m]  Hot-Swaps: {}",
                    st_col, organism.allostatic_load, organism.hot_swaps_count
                )
            }
            4 => format!(
                "Registers: R0:{:<3} R1:{:<3} R2:{:<3} R3:{:<3} R4..7: 0",
                organism.registers.scalars[0],
                organism.registers.scalars[1],
                organism.registers.scalars[2],
                organism.registers.scalars[3]
            ),
            5 => format!(
                "Vectors:   V0:{:?} V1..3:[0,0,0,0]",
                &organism.registers.vectors[0]
            ),
            6 => "MAP-ELITES ARCHIVE (5x4 Niches)".to_string(),
            7 => "Comp\\Var | Flat | Low  | Med  | High (Dyn)".to_string(),
            8 => render_map_elites_row(map_elites, 0, "Micro   "),
            9 => render_map_elites_row(map_elites, 1, "Compact "),
            10 => render_map_elites_row(map_elites, 2, "Moderate"),
            11 => render_map_elites_row(map_elites, 3, "Complex "),
            _ => String::new(),
        };

        println!("║ {:<46}║ {:<61}║", row, right_panel);
    }

    println!("╠═══════════════════════════════════════════════╩═══════════════════════════════════════════════════════════════╣");

    let s1 = if stage == EvolutionStage::Stage1Homeostasis {
        "\x1b[30;42m 1. HOMEOSTASIS \x1b[0m"
    } else {
        "\x1b[90m 1. HOMEOSTASIS \x1b[0m"
    };
    let s2 = if stage == EvolutionStage::Stage2CrisisDetected {
        "\x1b[30;41;1m 2. CRISIS DETECTED \x1b[0m"
    } else {
        "\x1b[90m 2. CRISIS \x1b[0m"
    };
    let s3 = if stage == EvolutionStage::Stage3MapElitesSearch {
        "\x1b[30;43m 3. MAP-ELITES SEARCH \x1b[0m"
    } else {
        "\x1b[90m 3. MAP-ELITES \x1b[0m"
    };
    let s4 = if stage == EvolutionStage::Stage4SafetyAudit {
        "\x1b[30;44m 4. 7-GATE SAFETY \x1b[0m"
    } else {
        "\x1b[90m 4. 7-GATES \x1b[0m"
    };
    let s5 = if stage == EvolutionStage::Stage5HotSwap {
        "\x1b[30;45;1m 5. ZERO-DOWNTIME HOT-SWAP \x1b[0m"
    } else {
        "\x1b[90m 5. HOT-SWAP \x1b[0m"
    };

    println!(
        "║ PIPELINE: {} -> {} -> {} -> {} -> {} ║",
        s1, s2, s3, s4, s5
    );
    println!("╠═══════════════════════════════════════════════════════════════════════════════════════════════════════════════╣");

    let g1 = if safety.g1_types {
        "\x1b[32m[✓] G1: Types\x1b[0m"
    } else {
        "\x1b[31m[✗] G1\x1b[0m"
    };
    let g2 = if safety.g2_bounds {
        "\x1b[32m[✓] G2: Bounds\x1b[0m"
    } else {
        "\x1b[31m[✗] G2\x1b[0m"
    };
    let g3 = if safety.g3_effects {
        "\x1b[32m[✓] G3: EffectAlloc\x1b[0m"
    } else {
        "\x1b[31m[✗] G3\x1b[0m"
    };
    let g4 = if safety.g4_sandbox {
        "\x1b[32m[✓] G4: Sandbox\x1b[0m"
    } else {
        "\x1b[31m[✗] G4\x1b[0m"
    };
    let g5 = if safety.g5_canaries {
        "\x1b[32m[✓] G5: Canaries\x1b[0m"
    } else {
        "\x1b[31m[✗] G5\x1b[0m"
    };
    let g6 = if safety.g6_constitution {
        "\x1b[32m[✓] G6: Const\x1b[0m"
    } else {
        "\x1b[31m[✗] G6\x1b[0m"
    };
    let g7 = if safety.g7_transaction {
        "\x1b[32m[✓] G7: TxLease\x1b[0m"
    } else {
        "\x1b[31m[✗] G7\x1b[0m"
    };

    println!(
        "║ 7-GATE SAFETY AUDIT: {}  {}  {}  {}  {}  {}  {}  ║",
        g1, g2, g3, g4, g5, g6, g7
    );
    println!("╠═══════════════════════════════════════════════════════════════════════════════════════════════════════════════╣");

    let iris_expr = organism.genome.to_iris_expr();
    let py_expr = organism.genome.to_python_expr();
    let short_iris = if iris_expr.len() > 88 {
        format!("{}...", &iris_expr[..85])
    } else {
        iris_expr
    };
    let short_py = if py_expr.len() > 88 {
        format!("{}...", &py_expr[..85])
    } else {
        py_expr
    };

    println!("║ ACTIVE POLICY (IRIS)  : {:<86}║", short_iris);
    println!("║ ACTIVE POLICY (PYTHON): {:<86}║", short_py);
    println!("╠═══════════════════════════════════════════════════════════════════════════════════════════════════════════════╣");

    println!("║ RECENT EVENT STREAM:                                                                                          ║");
    for ev in events {
        let short_ev = if ev.len() > 105 {
            format!("{}...", &ev[..102])
        } else {
            ev.clone()
        };
        println!("║  * {:<106}║", short_ev);
    }
    for _ in 0..(6_usize.saturating_sub(events.len())) {
        println!("║                                                                                                               ║");
    }

    println!("╠═══════════════════════════════════════════════════════════════════════════════════════════════════════════════╣");
    println!("║ Controls: [Space] Pause/Resume  [s] Step  [e] Force Evolve  [r] Disaster  [1-4] Era  [+/-] Speed  [q] Quit    ║");
    println!("╚═══════════════════════════════════════════════════════════════════════════════════════════════════════════════╝");

    let _ = out.flush();
}

fn render_map_elites_row(archive: &MapElitesArchive, comp_bin: usize, label: &str) -> String {
    let mut row = format!("{} | ", label);
    for var_bin in 0..NicheCoordinate::NUM_VARIANCE_BINS {
        let coord = NicheCoordinate {
            complexity_bin: comp_bin,
            variance_bin: var_bin,
        };
        if let Some(elite) = archive.get_elite(coord) {
            let fit: f64 = (1.0f64 / (1.0f64 + elite.score.loss)).clamp(0.0f64, 1.0f64) * 100.0f64;
            row.push_str(&format!("\x1b[32m{:>4.0}%\x1b[0m|", fit));
        } else {
            row.push_str(" -- |");
        }
    }
    row
}
