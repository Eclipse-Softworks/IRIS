#!/usr/bin/env python3
"""Program 1: Pure Python Genetic Programming Baseline (Controlled Experiment).

Implements traditional AST-based genetic programming in pure Python:
- Recursive expression tree with unary, binary, and conditional operators.
- Subtree crossover, point mutation, and tournament selection.
- Single-lineage generational evolution without quality-diversity archives or C-ABI acceleration.
"""

import math
import random
import time
from dataclasses import dataclass
from typing import Any, Callable, Dict, List, Optional, Tuple, Union


# ── AST Node Definitions ───────────────────────────────────────────────────

class ASTNode:
    """Base class for pure Python AST nodes."""

    def eval(self, env: List[int]) -> int:
        raise NotImplementedError

    def node_count(self) -> int:
        raise NotImplementedError

    def depth(self) -> int:
        raise NotImplementedError

    def to_code(self) -> str:
        raise NotImplementedError

    def clone(self) -> "ASTNode":
        raise NotImplementedError


class ConstNode(ASTNode):
    def __init__(self, val: int):
        self.val = val

    def eval(self, env: List[int]) -> int:
        return self.val

    def node_count(self) -> int:
        return 1

    def depth(self) -> int:
        return 1

    def to_code(self) -> str:
        return str(self.val)

    def clone(self) -> "ASTNode":
        return ConstNode(self.val)


class VarNode(ASTNode):
    def __init__(self, idx: int):
        self.idx = idx  # 0: queue_rho, 1: hazard_miss, 2: upstream_lat, 3: health_budget

    def eval(self, env: List[int]) -> int:
        if 0 <= self.idx < len(env):
            return env[self.idx]
        return 0

    def node_count(self) -> int:
        return 1

    def depth(self) -> int:
        return 1

    def to_code(self) -> str:
        names = ["x_queue", "x_hazard", "x_latency", "x_health"]
        return names[self.idx] if self.idx < len(names) else f"x_{self.idx}"

    def clone(self) -> "ASTNode":
        return VarNode(self.idx)


class BinaryOpNode(ASTNode):
    OPS = ["+", "-", "*", "//", "%", "min", "max"]

    def __init__(self, op: str, left: ASTNode, right: ASTNode):
        self.op = op
        self.left = left
        self.right = right

    def eval(self, env: List[int]) -> int:
        lv = self.left.eval(env)
        rv = self.right.eval(env)
        try:
            if self.op == "+":
                return lv + rv
            elif self.op == "-":
                return lv - rv
            elif self.op == "*":
                # Guard against integer overflow in long runs
                prod = lv * rv
                return max(-1_000_000, min(1_000_000, prod))
            elif self.op == "//":
                return 1 if rv == 0 else lv // rv
            elif self.op == "%":
                return 0 if rv == 0 else lv % rv
            elif self.op == "min":
                return min(lv, rv)
            elif self.op == "max":
                return max(lv, rv)
        except Exception:
            return 0
        return 0

    def node_count(self) -> int:
        return 1 + self.left.node_count() + self.right.node_count()

    def depth(self) -> int:
        return 1 + max(self.left.depth(), self.right.depth())

    def to_code(self) -> str:
        if self.op in ["min", "max"]:
            return f"{self.op}({self.left.to_code()}, {self.right.to_code()})"
        return f"({self.left.to_code()} {self.op} {self.right.to_code()})"

    def clone(self) -> "ASTNode":
        return BinaryOpNode(self.op, self.left.clone(), self.right.clone())


class TernaryIfNode(ASTNode):
    def __init__(self, cond: ASTNode, then_b: ASTNode, else_b: ASTNode):
        self.cond = cond
        self.then_b = then_b
        self.else_b = else_b

    def eval(self, env: List[int]) -> int:
        cv = self.cond.eval(env)
        if cv > 0:
            return self.then_b.eval(env)
        return self.else_b.eval(env)

    def node_count(self) -> int:
        return 1 + self.cond.node_count() + self.then_b.node_count() + self.else_b.node_count()

    def depth(self) -> int:
        return 1 + max(self.cond.depth(), self.then_b.depth(), self.else_b.depth())

    def to_code(self) -> str:
        return f"({self.then_b.to_code()} if ({self.cond.to_code()} > 0) else {self.else_b.to_code()})"

    def clone(self) -> "ASTNode":
        return TernaryIfNode(self.cond.clone(), self.then_b.clone(), self.else_b.clone())


# ── Pure Python Genetic Programming Engine ────────────────────────────────

class PurePythonGP:
    """Standard genetic programming engine implemented in pure Python."""

    def __init__(
        self,
        num_vars: int = 4,
        pop_size: int = 40,
        max_depth: int = 6,
        tournament_size: int = 4,
        crossover_prob: float = 0.75,
        mutation_prob: float = 0.25,
        seed: int = 42,
    ):
        self.num_vars = num_vars
        self.pop_size = pop_size
        self.max_depth = max_depth
        self.tournament_size = tournament_size
        self.crossover_prob = crossover_prob
        self.mutation_prob = mutation_prob
        self.rng = random.Random(seed)

    def random_leaf(self) -> ASTNode:
        if self.rng.random() < 0.5:
            idx = self.rng.randint(0, self.num_vars - 1)
            return VarNode(idx)
        else:
            val = self.rng.choice([-10, -1, 0, 1, 2, 5, 10, 25, 50, 80, 100])
            return ConstNode(val)

    def random_tree(self, max_d: int) -> ASTNode:
        if max_d <= 1 or (self.rng.random() < 0.25 and max_d < self.max_depth):
            return self.random_leaf()

        r = self.rng.random()
        if r < 0.65:
            op = self.rng.choice(BinaryOpNode.OPS)
            return BinaryOpNode(op, self.random_tree(max_d - 1), self.random_tree(max_d - 1))
        else:
            return TernaryIfNode(
                self.random_tree(max_d - 1),
                self.random_tree(max_d - 1),
                self.random_tree(max_d - 1),
            )

    def mutate(self, tree: ASTNode) -> ASTNode:
        """Point mutation or subtree replacement."""
        if self.rng.random() < 0.2:
            return self.random_tree(max_d=3)

        if isinstance(tree, ConstNode):
            return ConstNode(tree.val + self.rng.choice([-5, -1, 1, 5]))
        elif isinstance(tree, VarNode):
            return VarNode(self.rng.randint(0, self.num_vars - 1))
        elif isinstance(tree, BinaryOpNode):
            if self.rng.random() < 0.3:
                return BinaryOpNode(self.rng.choice(BinaryOpNode.OPS), tree.left.clone(), tree.right.clone())
            elif self.rng.random() < 0.5:
                return BinaryOpNode(tree.op, self.mutate(tree.left), tree.right.clone())
            else:
                return BinaryOpNode(tree.op, tree.left.clone(), self.mutate(tree.right))
        elif isinstance(tree, TernaryIfNode):
            pick = self.rng.random()
            if pick < 0.33:
                return TernaryIfNode(self.mutate(tree.cond), tree.then_b.clone(), tree.else_b.clone())
            elif pick < 0.66:
                return TernaryIfNode(tree.cond.clone(), self.mutate(tree.then_b), tree.else_b.clone())
            else:
                return TernaryIfNode(tree.cond.clone(), tree.then_b.clone(), self.mutate(tree.else_b))
        return tree.clone()

    def crossover(self, parent1: ASTNode, parent2: ASTNode) -> ASTNode:
        """Subtree crossover swapping branch between parents."""
        if self.rng.random() < 0.2 or isinstance(parent1, (ConstNode, VarNode)):
            return parent2.clone()

        if isinstance(parent1, BinaryOpNode):
            if self.rng.random() < 0.5:
                return BinaryOpNode(parent1.op, parent2.clone(), parent1.right.clone())
            else:
                return BinaryOpNode(parent1.op, parent1.left.clone(), parent2.clone())
        elif isinstance(parent1, TernaryIfNode):
            if self.rng.random() < 0.5:
                return TernaryIfNode(parent1.cond.clone(), parent2.clone(), parent1.else_b.clone())
            else:
                return TernaryIfNode(parent1.cond.clone(), parent1.then_b.clone(), parent2.clone())
        return parent1.clone()

    def evolve(
        self,
        training_cases: List[Tuple[List[int], int]],
        generations: int = 15,
    ) -> Tuple[ASTNode, float, float]:
        """Evolves population against training cases. Returns (best_tree, best_loss, elapsed_sec)."""
        t0 = time.perf_counter()
        population = [self.random_tree(max_d=4) for _ in range(self.pop_size)]

        def evaluate(ind: ASTNode) -> float:
            total_err = 0.0
            for env, target in training_cases:
                pred = ind.eval(env)
                # Action is mapped modulo 4
                chosen_action = abs(pred) % 4
                if chosen_action != target:
                    total_err += 1.0
            # Weak parsimony penalty to avoid infinite recursion
            return total_err + 0.001 * ind.node_count()

        best_ind = population[0]
        best_fitness = evaluate(best_ind)

        for _ in range(generations):
            scores = [evaluate(ind) for ind in population]
            for ind, score in zip(population, scores):
                if score < best_fitness:
                    best_fitness = score
                    best_ind = ind.clone()

            # Tournament selection and next generation
            next_pop = [best_ind.clone()]  # Elitism
            while len(next_pop) < self.pop_size:
                # Tournament 1
                t1 = self.rng.sample(range(self.pop_size), self.tournament_size)
                p1 = population[min(t1, key=lambda i: scores[i])]
                # Tournament 2
                t2 = self.rng.sample(range(self.pop_size), self.tournament_size)
                p2 = population[min(t2, key=lambda i: scores[i])]

                if self.rng.random() < self.crossover_prob:
                    child = self.crossover(p1, p2)
                else:
                    child = p1.clone()

                if self.rng.random() < self.mutation_prob:
                    child = self.mutate(child)

                # Cap max tree depth to prevent runaway stack overflows
                if child.depth() > self.max_depth + 4:
                    child = self.random_leaf()

                next_pop.append(child)

            population = next_pop

        elapsed = time.perf_counter() - t0
        return best_ind, best_fitness, elapsed


# ── Controlled Pure Python Agent ───────────────────────────────────────────

class PurePythonAgent:
    """Autonomous organism/gateway driven by pure Python genetic programming."""

    def __init__(self, seed: int = 100):
        self.gp = PurePythonGP(num_vars=4, pop_size=30, max_depth=5, seed=seed)
        # Seed with initial default tree: Forage / Normal serve (0)
        self.current_genome: ASTNode = ConstNode(0)
        self.total_evolutions = 0
        self.evolution_time_total = 0.0
        self.eval_time_total = 0.0
        self.eval_count = 0

    def decide_action(self, sensory_vector: List[int]) -> int:
        """Evaluates sensory vector and maps output to one of 4 actions."""
        t0 = time.perf_counter()
        raw_output = self.current_genome.eval(sensory_vector)
        action = abs(raw_output) % 4
        self.eval_time_total += (time.perf_counter() - t0)
        self.eval_count += 1
        return action

    def adapt_to_crisis(self, observations: List[Tuple[List[int], int]], generations: int = 15):
        """Triggers a pure Python generational GP evolution cycle."""
        best_tree, _, elapsed = self.gp.evolve(observations, generations=generations)
        self.current_genome = best_tree
        self.total_evolutions += 1
        self.evolution_time_total += elapsed

    def get_code(self) -> str:
        return self.current_genome.to_code()

    def get_stats(self) -> Dict[str, Any]:
        avg_eval_us = (self.eval_time_total / max(1, self.eval_count)) * 1_000_000
        return {
            "nodes": self.current_genome.node_count(),
            "depth": self.current_genome.depth(),
            "evolutions": self.total_evolutions,
            "total_evo_sec": self.evolution_time_total,
            "avg_eval_us": avg_eval_us,
            "code_sample": self.get_code(),
        }


if __name__ == "__main__":
    print("[Program 1: Pure Python GP] Initializing baseline test...")
    agent = PurePythonAgent(seed=42)
    sample_cases = [
        ([10, 5, 2, 95], 0),   # Normal
        ([88, 70, 10, 40], 1), # Saturation -> Shed
        ([20, 20, 95, 30], 2), # Latency spike -> Circuit break
        ([50, 90, 80, 20], 3), # Attack -> Defensive shield
    ]
    agent.adapt_to_crisis(sample_cases, generations=20)
    stats = agent.get_stats()
    print(f"Evolved AST Code : {stats['code_sample']}")
    print(f"Node count       : {stats['nodes']} (Depth: {stats['depth']})")
    print(f"Evo Time         : {stats['total_evo_sec']:.4f}s")
    for vec, expected in sample_cases:
        act = agent.decide_action(vec)
        print(f"Input: {vec} -> Action: {act} (Target: {expected})")
