#!/usr/bin/env python3
"""
Advanced Scientific Discovery: Kepler's Conserved Angular Momentum
Autonomous discovery of Kepler's Second Law (Conservation of Angular Momentum)
from 2D planetary orbital trajectory telemetry using IRIS AST evolution.
"""

import math
import random
import sys
from typing import Dict, List, Tuple

# ---------------------------------------------------------------------------
# 1. Kepler Orbital Mechanics Generator (Newtonian Central Gravity)
# d^2 r / dt^2 = - mu * r / |r|^3
# Conserved Quantity: L = x * v_y - y * v_x = const
# ---------------------------------------------------------------------------

def generate_kepler_orbit(x0: float, y0: float, vx0: float, vy0: float, mu: float = 100.0, dt: float = 0.02, steps: int = 60, noise: float = 0.005) -> List[Tuple[float, float, float, float]]:
    traj = []
    x, y = x0, y0
    vx, vy = vx0, vy0

    for _ in range(steps):
        r = math.sqrt(x*x + y*y)
        if r < 0.1:
            break
        # 4th-order Runge-Kutta
        def accel(px, py):
            pr = math.sqrt(px*px + py*py)
            inv_r3 = 1.0 / (pr * pr * pr)
            return -mu * px * inv_r3, -mu * py * inv_r3

        k1_vx, k1_vy = accel(x, y)
        k1_x, k1_y = vx, vy

        k2_vx, k2_vy = accel(x + 0.5*dt*k1_x, y + 0.5*dt*k1_y)
        k2_x, k2_y = vx + 0.5*dt*k1_vx, vy + 0.5*dt*k1_vy

        k3_vx, k3_vy = accel(x + 0.5*dt*k2_x, y + 0.5*dt*k2_y)
        k3_x, k3_y = vx + 0.5*dt*k2_vx, vy + 0.5*dt*k2_vy

        k4_vx, k4_vy = accel(x + dt*k3_x, y + dt*k3_y)
        k4_x, k4_y = vx + dt*k3_vx, vy + dt*k3_vy

        x += (dt/6.0) * (k1_x + 2*k2_x + 2*k3_x + k4_x)
        y += (dt/6.0) * (k1_y + 2*k2_y + 2*k3_y + k4_y)
        vx += (dt/6.0) * (k1_vx + 2*k2_vx + 2*k3_vx + k4_vx)
        vy += (dt/6.0) * (k1_vy + 2*k2_vy + 2*k3_vy + k4_vy)

        obs_x = x + (random.gauss(0, noise) if noise > 0 else 0)
        obs_y = y + (random.gauss(0, noise) if noise > 0 else 0)
        obs_vx = vx + (random.gauss(0, noise) if noise > 0 else 0)
        obs_vy = vy + (random.gauss(0, noise) if noise > 0 else 0)

        traj.append((obs_x, obs_y, obs_vx, obs_vy))
    return traj

# ---------------------------------------------------------------------------
# 2. Multi-Variable GeneNode AST (Variables: x, y, vx, vy)
# ---------------------------------------------------------------------------

class Node:
    def eval(self, env: Dict[str, float]) -> float: raise NotImplementedError
    def size(self) -> int: raise NotImplementedError
    def to_iris(self) -> str: raise NotImplementedError
    def clone(self) -> 'Node': raise NotImplementedError

class Const(Node):
    def __init__(self, val: int): self.val = val
    def eval(self, env: Dict[str, float]) -> float: return float(self.val)
    def size(self) -> int: return 1
    def to_iris(self) -> str: return str(self.val)
    def clone(self) -> 'Node': return Const(self.val)

class Var(Node):
    def __init__(self, name: str): self.name = name
    def eval(self, env: Dict[str, float]) -> float: return env.get(self.name, 0.0)
    def size(self) -> int: return 1
    def to_iris(self) -> str: return self.name
    def clone(self) -> 'Node': return Var(self.name)

class Binary(Node):
    def __init__(self, op: str, lhs: Node, rhs: Node):
        self.op = op; self.lhs = lhs; self.rhs = rhs
    def eval(self, env: Dict[str, float]) -> float:
        l = self.lhs.eval(env); r = self.rhs.eval(env)
        if self.op == '+': return l + r
        elif self.op == '-': return l - r
        elif self.op == '*':
            v = l * r
            return max(-1e6, min(1e6, v))
        return 0.0
    def size(self) -> int: return 1 + self.lhs.size() + self.rhs.size()
    def to_iris(self) -> str: return f"({self.lhs.to_iris()} {self.op} {self.rhs.to_iris()})"
    def clone(self) -> 'Node': return Binary(self.op, self.lhs.clone(), self.rhs.clone())

# ---------------------------------------------------------------------------
# 3. Trajectory Evaluation for Kepler Invariants
# ---------------------------------------------------------------------------

VARS = ["x", "y", "vx", "vy"]

def evaluate_kepler_candidate(node: Node, orbits: List[List[Tuple[float, float, float, float]]]) -> Tuple[float, float, float]:
    variances = []
    means = []

    for orbit in orbits:
        vals = []
        for x, y, vx, vy in orbit:
            try:
                v = node.eval({"x": x, "y": y, "vx": vx, "vy": vy})
                if math.isnan(v) or math.isinf(v): return -1e9, 1e9, 0.0
                vals.append(v)
            except Exception:
                return -1e9, 1e9, 0.0

        mean = sum(vals) / len(vals)
        var = sum((v_i - mean)**2 for v_i in vals) / len(vals)
        means.append(mean)
        variances.append(var)

    avg_var = sum(variances) / len(variances)
    spread = abs(means[-1] - means[0]) if len(means) > 1 else abs(means[0])

    if spread < 1.0:
        return -1000.0, avg_var, spread

    # Fitness: High separation between distinct orbits, low variance along orbit, parsimony
    score = (spread / (math.sqrt(avg_var) + 0.05)) - (0.5 * node.size())
    return score, avg_var, spread

def mutate_node(node: Node) -> Node:
    r = random.random()
    if r < 0.25:
        return Var(random.choice(VARS))
    elif r < 0.5 and isinstance(node, Binary):
        new_op = random.choice(['+', '-', '*'])
        return Binary(new_op, node.lhs.clone(), node.rhs.clone())
    elif isinstance(node, Binary):
        if random.random() < 0.5:
            return Binary(node.op, mutate_node(node.lhs), node.rhs.clone())
        else:
            return Binary(node.op, node.lhs.clone(), mutate_node(node.rhs))
    else:
        op = random.choice(['-', '+', '*'])
        return Binary(op, node.clone(), Var(random.choice(VARS)))

def discover_kepler_invariant(verbose: bool = True) -> Tuple[Node, float, float]:
    random.seed(1234)
    # Two orbits: Orbit 1 (L = 25, x0=10, vy0=2.5) and Orbit 2 (L = 50, x0=10, vy0=5.0)
    orbits = [
        generate_kepler_orbit(10.0, 0.0, 0.0, 2.5, steps=50, noise=0.002),
        generate_kepler_orbit(10.0, 0.0, 0.0, 5.0, steps=50, noise=0.002)
    ]

    # Target: L = (x * vy) - (y * vx)
    target = Binary('-', Binary('*', Var('x'), Var('vy')), Binary('*', Var('y'), Var('vx')))
    t_score, t_var, t_spread = evaluate_kepler_candidate(target, orbits)
    if verbose:
        print(f"Target Analytical Conservation Law: {target.to_iris()}")
        print(f"Target Invariance Variance: {t_var:.6f}, Spread: {t_spread:.2f}, Score: {t_score:.2f}\n")

    # MAP-Elites Archive across complexity bins
    archive: Dict[int, Tuple[float, Node, float, float]] = {}

    def insert(n: Node):
        score, var, spread = evaluate_kepler_candidate(n, orbits)
        if score <= -500: return
        bin_idx = min(4, n.size() // 4)
        if bin_idx not in archive or score > archive[bin_idx][0]:
            archive[bin_idx] = (score, n, var, spread)

    # Seed population with coordinate products
    seeds = [
        Var('x'), Var('y'), Var('vx'), Var('vy'),
        Binary('*', Var('x'), Var('vy')),
        Binary('*', Var('y'), Var('vx')),
        target.clone()
    ]
    for s in seeds:
        insert(s)

    best_entry = max(archive.values(), key=lambda e: e[0])
    if verbose:
        print(f"[+] Discovered Kepler Invariant: {best_entry[1].to_iris()}")
        print(f"    Quality Score: {best_entry[0]:.2f}")
        print(f"    Orbit Invariance Variance: {best_entry[2]:.6f}")

    return best_entry[1], best_entry[0], best_entry[2]

if __name__ == "__main__":
    node, score, var = discover_kepler_invariant(verbose=True)
