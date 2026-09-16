#!/usr/bin/env python3
"""Head-to-Head Comparative Benchmark: Pure Python GP vs. IRIS Evolution Library.

Simulates an Autonomous System / Living Gateway over 1,000 operational cycles
experiencing 5 non-stationary environmental shock regimes:
1. Nominal Equilibrium (Ticks 0 - 199)
2. Flash Crowd Saturation Surge (Ticks 200 - 399)
3. Upstream Latency Brownout (Ticks 400 - 599)
4. Adversarial Toxic Attack Storm (Ticks 600 - 799)
5. Post-Shock Recovery / Return to Nominal (Ticks 800 - 999)

Collects and contrasts:
- Survival accuracy and allostatic strain across each phase
- Catastrophic forgetting upon returning to nominal regime
- Decision evaluation latency (microseconds per tick)
- Evolution/synthesis compute time
- Genome compactness, syntactic bloat, and code elegance
"""

import os
import random
import sys
import time
from dataclasses import dataclass
from typing import Dict, List, Tuple

# Ensure current dir and bindings/python are in sys.path
script_dir = os.path.dirname(os.path.abspath(__file__))
if script_dir not in sys.path:
    sys.path.insert(0, script_dir)

from genome_pure_python import PurePythonAgent
from genome_iris_evolution import IrisEvolutionAgent


# ── Environment Simulation ─────────────────────────────────────────────────

@dataclass
class EnvironmentalTick:
    tick_id: int
    regime_id: int
    regime_name: str
    sensory_vector: List[int]  # [queue_rho, hazard_defect, upstream_latency, health_budget]
    optimal_action: int
    composite_scalar: int      # scalar proxy for GP training


def compute_ground_truth_action(sensory_vec: List[int]) -> int:
    """Ground-truth optimal action under physical/SLA constraints."""
    queue, hazard, latency, health = sensory_vec
    if health < 35 or hazard > 80:
        return 3  # Shield / Quarantine / Heat Up
    elif latency > 70:
        return 2  # Circuit Break / Fallback Cache / Cool Down
    elif queue > 70:
        return 1  # Shed / Backpressure
    else:
        return 0  # Normal Serve / Forage


class NonStationaryEnvironment:
    """Generates non-stationary environmental regimes with dynamic continuous noise."""

    REGIMES = [
        (0, "Nominal Operations"),       # Predominantly Action 0
        (1, "Flash Crowd Surge"),        # Predominantly Action 1
        (2, "Upstream Brownout"),        # Predominantly Action 2
        (3, "Adversarial Storm"),        # Predominantly Action 3
        (0, "Post-Shock Recovery"),      # Return to Nominal (Action 0)
    ]

    def __init__(self, seed: int = 12345):
        self.rng = random.Random(seed)

    def generate_tick(self, tick: int) -> EnvironmentalTick:
        phase_idx = min(4, tick // 200)
        regime_id, regime_name = self.REGIMES[phase_idx]

        if phase_idx == 0 or phase_idx == 4:
            # Nominal / Recovery: mostly low queue, low latency, high health
            # 10% transient jitter
            q_mean = 20 if self.rng.random() > 0.10 else 76
            h_mean = 8 if self.rng.random() > 0.05 else 85
            lat_mean = 12 if self.rng.random() > 0.08 else 75
            queue = int(max(0, min(100, self.rng.gauss(q_mean, 6))))
            hazard = int(max(0, min(100, self.rng.gauss(h_mean, 3))))
            latency = int(max(0, min(100, self.rng.gauss(lat_mean, 4))))
            health = int(max(0, min(100, self.rng.gauss(92, 4))))
        elif phase_idx == 1:
            # Flash crowd: queue heavily saturated, occasional normal dip
            q_mean = 86 if self.rng.random() > 0.12 else 55
            queue = int(max(0, min(100, self.rng.gauss(q_mean, 6))))
            hazard = int(max(0, min(100, self.rng.gauss(25, 5))))
            latency = int(max(0, min(100, self.rng.gauss(20, 5))))
            health = int(max(0, min(100, self.rng.gauss(65, 6))))
        elif phase_idx == 2:
            # Upstream Brownout: latency leaps
            lat_mean = 90 if self.rng.random() > 0.10 else 40
            queue = int(max(0, min(100, self.rng.gauss(42, 6))))
            hazard = int(max(0, min(100, self.rng.gauss(18, 4))))
            latency = int(max(0, min(100, self.rng.gauss(lat_mean, 5))))
            health = int(max(0, min(100, self.rng.gauss(48, 6))))
        else:
            # Adversarial Storm: hazard spike & health drain
            queue = int(max(0, min(100, self.rng.gauss(60, 6))))
            hazard = int(max(0, min(100, self.rng.gauss(93, 3))))
            latency = int(max(0, min(100, self.rng.gauss(50, 6))))
            health = int(max(0, min(100, self.rng.gauss(25, 4))))

        sensory_vec = [queue, hazard, latency, health]
        optimal_act = compute_ground_truth_action(sensory_vec)
        composite = queue * 2 + hazard * 2 + latency * 3 - health

        return EnvironmentalTick(
            tick_id=tick,
            regime_id=regime_id,
            regime_name=regime_name,
            sensory_vector=sensory_vec,
            optimal_action=optimal_act,
            composite_scalar=composite,
        )


# ── Comparison Runner ──────────────────────────────────────────────────────

@dataclass
class AgentTrack:
    name: str
    actions: List[int]
    strain: List[float]
    phase_correct: Dict[int, int]
    phase_totals: Dict[int, int]


def run_experiment(total_ticks: int = 1000):
    print("=" * 80)
    print("        IRIS vs. PURE PYTHON CODE GENOME COMPARATIVE EXPERIMENT")
    print("=" * 80)
    print(f"Simulation Horizon : {total_ticks} Operational Cycles (5 Non-Stationary Regimes)")
    print(f"Sensory Inputs     : 4-Lane Continuous Vector [Queue, Hazard, Latency, SLA Health]")
    print(f"Autonomic Control  : Closed-Loop MAPE-K with Rolling Allostatic Strain Sensing\n")

    env = NonStationaryEnvironment(seed=999)

    agent_py = PurePythonAgent(seed=42)
    agent_iris = IrisEvolutionAgent(seed=42)

    track_py = AgentTrack("Pure Python GP", [], [], {p: 0 for p in range(5)}, {p: 0 for p in range(5)})
    track_iris = AgentTrack("IRIS Evolution", [], [], {p: 0 for p in range(5)}, {p: 0 for p in range(5)})

    # Rolling window for autonomic crisis detection (last 15 ticks)
    window_py: List[Tuple[List[int], int, bool]] = []
    window_iris: List[Tuple[int, int, bool]] = []
    WINDOW_SIZE = 15

    for tick in range(total_ticks):
        current_phase = tick // 200
        env_state = env.generate_tick(tick)
        optimal = env_state.optimal_action

        # 1. Autonomous Decisions
        act_py = agent_py.decide_action(env_state.sensory_vector)
        act_iris = agent_iris.decide_action(env_state.sensory_vector)

        track_py.actions.append(act_py)
        track_iris.actions.append(act_iris)

        track_py.phase_totals[current_phase] += 1
        track_iris.phase_totals[current_phase] += 1

        # Accuracy & Errors
        ok_py = (act_py == optimal)
        ok_iris = (act_iris == optimal)

        if ok_py:
            track_py.phase_correct[current_phase] += 1
        if ok_iris:
            track_iris.phase_correct[current_phase] += 1

        # Allostatic strain penalty
        severity = (env_state.sensory_vector[0] + env_state.sensory_vector[1] + env_state.sensory_vector[2]) / 300.0
        strain_py = 0.05 * severity if ok_py else (1.0 + severity)
        strain_iris = 0.05 * severity if ok_iris else (1.0 + severity)

        track_py.strain.append(strain_py)
        track_iris.strain.append(strain_iris)

        # Append to rolling observation window
        window_py.append((env_state.sensory_vector, optimal, ok_py))
        window_iris.append((env_state.composite_scalar, optimal, ok_iris))
        if len(window_py) > WINDOW_SIZE:
            window_py.pop(0)
        if len(window_iris) > WINDOW_SIZE:
            window_iris.pop(0)

        # Autonomic trigger: Check rolling error rate
        if len(window_py) == WINDOW_SIZE:
            err_py = sum(1 for _, _, ok in window_py if not ok) / WINDOW_SIZE
            if err_py >= 0.35:  # Over 35% errors in rolling window -> Crisis detected!
                py_cases = [(vec, opt) for vec, opt, _ in window_py]
                agent_py.adapt_to_crisis(py_cases, generations=15)
                window_py.clear()

        if len(window_iris) == WINDOW_SIZE:
            err_iris = sum(1 for _, _, ok in window_iris if not ok) / WINDOW_SIZE
            if err_iris >= 0.35:
                iris_cases = [(comp, opt) for comp, opt, _ in window_iris]
                agent_iris.adapt_to_crisis(env_state.regime_id, iris_cases, generations=20)
                window_iris.clear()


    # ── Comparative Analytics ──────────────────────────────────────────────

    stats_py = agent_py.get_stats()
    stats_iris = agent_iris.get_stats()

    print("\n" + "=" * 80)
    print("                    PHASE-BY-PHASE SURVIVAL & ADAPTATION")
    print("=" * 80)
    print(f"{'Phase / Environmental Regime':<35} | {'Pure Python Accuracy':<20} | {'IRIS Engine Accuracy':<20}")
    print("-" * 80)

    for p in range(5):
        _, r_name = env.REGIMES[p]
        p_name = f"Phase {p+1}: {r_name}"
        acc_py = (track_py.phase_correct[p] / track_py.phase_totals[p]) * 100.0
        acc_iris = (track_iris.phase_correct[p] / track_iris.phase_totals[p]) * 100.0
        print(f"{p_name:<35} | {acc_py:>18.1f}% | {acc_iris:>18.1f}%")

    # Catastrophic Forgetting metric (Phase 1 vs Phase 5 recovery)
    p1_py = (track_py.phase_correct[0] / track_py.phase_totals[0]) * 100.0
    p5_py = (track_py.phase_correct[4] / track_py.phase_totals[4]) * 100.0
    forget_py = p1_py - p5_py

    p1_iris = (track_iris.phase_correct[0] / track_iris.phase_totals[0]) * 100.0
    p5_iris = (track_iris.phase_correct[4] / track_iris.phase_totals[4]) * 100.0
    forget_iris = p1_iris - p5_iris

    print("-" * 80)
    print(f"{'Catastrophic Forgetting Drop':<35} | {forget_py:>17.1f}% drop | {forget_iris:>17.1f}% drop")

    print("\n" + "=" * 80)
    print("                  PERFORMANCE & COMPUTATIONAL EFFICIENCY")
    print("=" * 80)
    total_strain_py = sum(track_py.strain)
    total_strain_iris = sum(track_iris.strain)

    print(f"{'Metric':<35} | {'Pure Python Baseline':<20} | {'IRIS Evolution Engine':<20}")
    print("-" * 80)
    print(f"{'Cumulative Allostatic Strain':<35} | {total_strain_py:>20.1f} | {total_strain_iris:>20.1f}")
    print(f"{'Average Decision Latency':<35} | {stats_py['avg_eval_us']:>18.2f} us | {stats_iris['avg_eval_us']:>18.2f} us")
    speedup = stats_py['avg_eval_us'] / max(0.001, stats_iris['avg_eval_us'])
    print(f"{'Decision Evaluation Speedup':<35} | {'Baseline (1.0x)':>20} | {f'{speedup:.1f}x Faster':>20}")
    print(f"{'Total Evolution Search Time':<35} | {stats_py['total_evo_sec']:>18.4f} s | {stats_iris['total_evo_sec']:>18.4f} s")
    iris_arch_desc = f"{stats_iris['archive_size']} niches ({stats_iris['map_coverage']})"
    py_node_desc = f"{stats_py['nodes']} nodes (depth {stats_py['depth']})"
    iris_node_desc = f"{stats_iris['tokens']} tokens (compact)"
    print(f"{'Zero-Cost Hot-Swaps Performed':<35} | {'0 (Full Re-Evo)':>20} | {stats_iris['hotswaps']:>20}")
    print(f"{'Phenotypic MAP-Elites Archive':<35} | {'None (Stateless)':>20} | {iris_arch_desc:>20}")
    print(f"{'AST Node Count / Complexity':<35} | {py_node_desc:>20} | {iris_node_desc:>20}")

    print("\n" + "=" * 80)
    print("                      GENOME CODE COMPARISON")
    print("=" * 80)
    print("\n[Program 1: Pure Python Evolved AST Expression]:")
    print(f"  {stats_py['code_sample']}")

    print("\n[Program 2: IRIS Evolved Typed Python Code (Polyglot Output)]:")
    print(stats_iris['code_sample'])
    print("=" * 80)


if __name__ == "__main__":
    run_experiment(total_ticks=1000)
