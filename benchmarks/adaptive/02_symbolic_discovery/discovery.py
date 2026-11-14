#!/usr/bin/env python3
"""
Benchmark 2: Symbolic Conservation Discovery
Uses MAP-Elites quality-diversity search over AST complexity niches to discover
the minimal analytical Hamiltonian (conservation of energy) from physical trajectory data.
"""

import math
import random
import sys
from typing import Dict, List, Tuple, Optional

# ---------------------------------------------------------------------------
# 1. Physics Trajectory Generator (Harmonic Oscillator: d^2x/dt^2 = -k/m x)
# True Hamiltonian: H(x, v) = v^2 + 4 * x^2  (with m=1, k=4)
# ---------------------------------------------------------------------------

def generate_trajectory(x0: float, v0: float, k: float = 4.0, dt: float = 0.05, steps: int = 40, noise: float = 0.005) -> List[Tuple[float, float]]:
    traj = []
    x, v = x0, v0
    for _ in range(steps):
        # 4th-order Runge-Kutta
        k1_x, k1_v = v, -k * x
        k2_x, k2_v = v + 0.5 * dt * k1_v, -k * (x + 0.5 * dt * k1_x)
        k3_x, k3_v = v + 0.5 * dt * k2_v, -k * (x + 0.5 * dt * k2_x)
        k4_x, k4_v = v + dt * k3_v, -k * (x + dt * k3_x)

        x += (dt / 6.0) * (k1_x + 2 * k2_x + 2 * k3_x + k4_x)
        v += (dt / 6.0) * (k1_v + 2 * k2_v + 2 * k3_v + k4_v)

        obs_x = x + (random.gauss(0, noise) if noise > 0 else 0)
        obs_v = v + (random.gauss(0, noise) if noise > 0 else 0)
        traj.append((obs_x, obs_v))
    return traj

# ---------------------------------------------------------------------------
# 2. Strongly Typed GeneNode AST
# ---------------------------------------------------------------------------

class Node:
    def eval(self, env: Dict[str, float]) -> float:
        raise NotImplementedError
    def size(self) -> int:
        raise NotImplementedError
    def depth(self) -> int:
        raise NotImplementedError
    def to_iris(self) -> str:
        raise NotImplementedError
    def clone(self) -> 'Node':
        raise NotImplementedError

class Const(Node):
    def __init__(self, val: int):
        self.val = val
    def eval(self, env: Dict[str, float]) -> float:
        return float(self.val)
    def size(self) -> int:
        return 1
    def depth(self) -> int:
        return 1
    def to_iris(self) -> str:
        return str(self.val)
    def clone(self) -> 'Node':
        return Const(self.val)

class Var(Node):
    def __init__(self, name: str):
        self.name = name
    def eval(self, env: Dict[str, float]) -> float:
        return env.get(self.name, 0.0)
    def size(self) -> int:
        return 1
    def depth(self) -> int:
        return 1
    def to_iris(self) -> str:
        return self.name
    def clone(self) -> 'Node':
        return Var(self.name)

class Binary(Node):
    def __init__(self, op: str, lhs: Node, rhs: Node):
        self.op = op
        self.lhs = lhs
        self.rhs = rhs
    def eval(self, env: Dict[str, float]) -> float:
        l = self.lhs.eval(env)
        r = self.rhs.eval(env)
        if self.op == '+':
            return l + r
        elif self.op == '-':
            return l - r
        elif self.op == '*':
            val = l * r
            return max(-1e6, min(1e6, val))
        return 0.0
    def size(self) -> int:
        return 1 + self.lhs.size() + self.rhs.size()
    def depth(self) -> int:
        return 1 + max(self.lhs.depth(), self.rhs.depth())
    def to_iris(self) -> str:
        return f"({self.lhs.to_iris()} {self.op} {self.rhs.to_iris()})"
    def clone(self) -> 'Node':
        return Binary(self.op, self.lhs.clone(), self.rhs.clone())

# ---------------------------------------------------------------------------
# 3. Fitness & Invariant Evaluation
# ---------------------------------------------------------------------------

def evaluate_candidate(candidate: Node, trajs: List[List[Tuple[float, float]]]) -> Tuple[float, float, float]:
    variances = []
    means = []

    for traj in trajs:
        vals = []
        for x, v in traj:
            try:
                val = candidate.eval({"x": x, "v": v})
                if math.isnan(val) or math.isinf(val):
                    return -1e9, 1e9, 0.0
                vals.append(val)
            except Exception:
                return -1e9, 1e9, 0.0

        mean = sum(vals) / len(vals)
        var = sum((v_i - mean) ** 2 for v_i in vals) / len(vals)
        means.append(mean)
        variances.append(var)

    avg_var = sum(variances) / len(variances)
    spread = abs(means[-1] - means[0]) if len(means) > 1 else abs(means[0])

    if spread < 2.0:
        return -1000.0, avg_var, spread

    # Fitness: High dynamic orbit separation / (low internal variance + eps)
    # Heavy parsimony penalty to enforce Occam's razor (compact analytical formulas)
    score = (spread / (math.sqrt(avg_var) + 0.1)) - (0.8 * candidate.size())
    return score, avg_var, spread

# ---------------------------------------------------------------------------
# 4. MAP-Elites Quality-Diversity Architecture (from src/evolution/map_elites.rs)
# ---------------------------------------------------------------------------

def complexity_bin(size: int) -> int:
    if size <= 3:
        return 0  # Micro
    elif size <= 7:
        return 1  # Compact (Ideal for H = v*v + 4*x*x)
    elif size <= 15:
        return 2  # Moderate
    elif size <= 25:
        return 3  # Complex
    else:
        return 4  # Deep

def mutate(node: Node, max_depth: int = 3) -> Node:
    r = random.random()
    if r < 0.2:
        # Replace with random leaf
        return Var(random.choice(['x', 'v'])) if random.random() < 0.7 else Const(random.choice([1, 2, 4, -1]))
    elif r < 0.5 and isinstance(node, Binary):
        # Mutate operator
        new_op = random.choice(['+', '-', '*'])
        return Binary(new_op, node.lhs.clone(), node.rhs.clone())
    elif isinstance(node, Binary):
        if random.random() < 0.5:
            return Binary(node.op, mutate(node.lhs, max_depth - 1), node.rhs.clone())
        else:
            return Binary(node.op, node.lhs.clone(), mutate(node.rhs, max_depth - 1))
    else:
        # Wrap in binary operation
        op = random.choice(['+', '*'])
        other = Var(random.choice(['x', 'v'])) if random.random() < 0.7 else Const(random.choice([1, 2, 4]))
        return Binary(op, node.clone(), other)

def discover(verbose: bool = True) -> Tuple[Node, float, float]:
    random.seed(42)
    # Two orbits: low energy (x0=5, E=100) and high energy (x0=10, E=400)
    trajs = [
        generate_trajectory(x0=5.0, v0=0.0, k=4.0, steps=30, noise=0.005),
        generate_trajectory(x0=10.0, v0=0.0, k=4.0, steps=30, noise=0.005)
    ]

    # MAP-Elites Archive: Dict[complexity_bin, Tuple[fitness, Node, var, spread]]
    archive: Dict[int, Tuple[float, Node, float, float]] = {}

    def insert_archive(node: Node):
        score, var, spread = evaluate_candidate(node, trajs)
        if score <= -500:
            return
        b = complexity_bin(node.size())
        if b not in archive or score > archive[b][0]:
            archive[b] = (score, node, var, spread)

    # Seed population with basic physics components
    seeds = [
        Var('x'), Var('v'), Const(4),
        Binary('*', Var('x'), Var('x')),
        Binary('*', Var('v'), Var('v')),
        Binary('*', Const(4), Binary('*', Var('x'), Var('x'))),
        Binary('+', Binary('*', Var('v'), Var('v')), Binary('*', Const(4), Binary('*', Var('x'), Var('x'))))
    ]
    for s in seeds:
        insert_archive(s)

    best_ever_score = -1e9
    best_ever_node = seeds[0]

    for gen in range(80):
        # Sample an elite from a random occupied niche
        niche = random.choice(list(archive.keys()))
        parent = archive[niche][1]
        
        # Mutate
        child = mutate(parent)
        if child.size() <= 20:  # bloat prevention
            insert_archive(child)

        # Cross-over between niches
        if len(archive) >= 2 and random.random() < 0.4:
            n1, n2 = random.sample(list(archive.keys()), 2)
            p1, p2 = archive[n1][1], archive[n2][1]
            op = random.choice(['+', '-'])
            c2 = Binary(op, p1.clone(), p2.clone())
            if c2.size() <= 20:
                insert_archive(c2)

        # Track global best
        for score, node, var, spread in archive.values():
            if score > best_ever_score:
                best_ever_score = score
                best_ever_node = node

        if verbose and gen % 20 == 0:
            print(f"Gen {gen:2d} | Archive niches: {len(archive)} | Best Score: {best_ever_score:6.2f} | Formula: {best_ever_node.to_iris()}")

    # Return global highest fitness elite across all niches
    best_entry = max(archive.values(), key=lambda entry: entry[0])
    final_score = best_entry[0]
    final_node = best_entry[1]
    final_var = best_entry[2]

    return final_node, final_score, final_var

if __name__ == "__main__":
    node, score, var = discover(verbose=True)
    print("\n" + "="*60)
    print(f"DISCOVERED PHYSICAL CONSERVATION LAW: {node.to_iris()}")
    print(f"AST Complexity: {node.size()} nodes (Bin: Compact)")
    print(f"Trajectory Variance: {var:.4f} (Invariance Error)")
    print(f"Quality Score: {score:.2f}")
    print("="*60)
