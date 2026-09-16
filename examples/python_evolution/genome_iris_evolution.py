#!/usr/bin/env python3
"""Program 2: IRIS Evolution Library Engine (Experimental System).

Implements an autonomous self-evolving genome using IRIS's C-ABI dynamic library:
- LLVM 23-compiled bare-metal execution via C-ABI and persistent multi-register SIMD state.
- Quality-Diversity (MAP-Elites) archive preventing catastrophic forgetting across non-stationary regimes.
- Formal parsimony regularized search producing compact, mathematically sound ASTs.
- Automatic polyglot translation to clean, typed, idiomatic Python code.
"""

import os
import sys
import time
from typing import Any, Dict, List, Optional, Tuple

# Ensure bindings/python is in sys.path
script_dir = os.path.dirname(os.path.abspath(__file__))
bindings_dir = os.path.abspath(os.path.join(script_dir, "..", "..", "bindings", "python"))
if bindings_dir not in sys.path:
    sys.path.insert(0, bindings_dir)

from iris_evolution import EvolutionarySearch, GeneNode, GenomeState
from iris_evolution.map_elites import MapElitesArchive, NicheCoordinate


# ── IRIS Evolution Agent ───────────────────────────────────────────────────

class IrisEvolutionAgent:
    """Autonomous organism/gateway powered by the IRIS Evolution Engine."""

    def __init__(self, seed: int = 100):
        self.seed = seed
        self.state = GenomeState()
        # Initialize an initial default search to bootstrap a baseline genome
        search = EvolutionarySearch(
            pop_size=20,
            generations=15,
            parsimony_weight=0.01,
            seed=seed,
        )
        search.add_case(0, 0)
        search.add_case(1, 0)
        self.current_genome: GeneNode = search.run()
        self.current_callable = self.current_genome.to_callable()

        # Regime-specialized MAP-Elites archives to store and hot-swap policy niches
        # Keys: regime_id (0: Nominal, 1: Flash Surge, 2: Latency Brownout, 3: Toxic Storm)
        self.regime_archives: Dict[int, GeneNode] = {0: self.current_genome}
        self.global_map_archive = MapElitesArchive(target_cases=[(0, 0), (1, 1), (2, 2), (3, 3)])
        self.global_map_archive.evaluate_candidate(self.current_genome.to_python(), generation=0)

        self.total_evolutions = 0
        self.total_hotswaps = 0
        self.evolution_time_total = 0.0
        self.eval_time_total = 0.0
        self.eval_count = 0

    def decide_action(self, sensory_vector: List[int]) -> int:
        """Evaluates sensory vector using the ultra-fast compiled Python callable emitted by IRIS."""
        composite = sensory_vector[0] * 2 + sensory_vector[1] * 2 + sensory_vector[2] * 3 - sensory_vector[3]
        t0 = time.perf_counter()
        res = self.current_callable(composite)
        action = abs(int(res)) % 4
        self.eval_time_total += (time.perf_counter() - t0)
        self.eval_count += 1
        return action

    def decide_action_c_abi(self, sensory_vector: List[int]) -> int:
        """Evaluates sensory vector directly in bare-metal C-ABI machine code via ctypes."""
        composite = sensory_vector[0] * 2 + sensory_vector[1] * 2 + sensory_vector[2] * 3 - sensory_vector[3]
        t0 = time.perf_counter()
        raw_output = self.current_genome.eval(composite)
        action = abs(raw_output) % 4
        self.eval_time_total += (time.perf_counter() - t0)
        self.eval_count += 1
        return action

    def adapt_to_crisis(
        self,
        regime_id: int,
        observations: List[Tuple[int, int]],
        generations: int = 25,
    ):
        """Adapts to an environmental crisis: hot-swaps verified elite or evolves a new one."""
        if not observations:
            return

        # 1. Filter observations to dominant failure pattern (eliminate boundary transition noise)
        from collections import Counter
        target_counts = Counter(exp for _, exp in observations)
        dominant_target, _ = target_counts.most_common(1)[0]
        filtered_cases = [(inp, exp) for inp, exp in observations if exp == dominant_target]
        if not filtered_cases:
            filtered_cases = observations

        # 2. Check if a proven elite exists in the archive and verify it on current observations
        if regime_id in self.regime_archives:
            candidate = self.regime_archives[regime_id]
            correct = sum(1 for inp, exp in filtered_cases if (abs(candidate.eval(inp)) % 4) == exp)
            if (correct / len(filtered_cases)) >= 0.70:
                self.current_genome = candidate
                self.current_callable = candidate.to_callable()
                self.total_hotswaps += 1
                return

        # 3. Synthesize a new parsimonious genome via IRIS C-ABI Genetic Programming
        t0 = time.perf_counter()
        search = EvolutionarySearch(
            pop_size=35,
            generations=generations,
            mutation_rate=0.35,
            crossover_rate=0.70,
            parsimony_weight=0.015,
            seed=self.seed + self.total_evolutions * 7,
        )
        for inp, exp in filtered_cases:
            search.add_case(inp, exp)

        new_genome = search.run()
        elapsed = time.perf_counter() - t0

        # 4. Verify candidate fitness before archiving
        v_correct = sum(1 for inp, exp in filtered_cases if (abs(new_genome.eval(inp)) % 4) == exp)
        if (v_correct / len(filtered_cases)) >= 0.70:
            self.regime_archives[regime_id] = new_genome
            self.global_map_archive.evaluate_candidate(
                new_genome.to_python(), generation=self.total_evolutions + 1
            )

        self.current_genome = new_genome
        self.current_callable = new_genome.to_callable()
        self.total_evolutions += 1
        self.evolution_time_total += elapsed

    def get_python_code(self, func_name: str = "iris_evolved_policy") -> str:
        """Emits a complete, typed, idiomatic Python function definition."""
        return self.current_genome.to_python_func(func_name=func_name, param_name="x")

    def get_python_expr(self) -> str:
        """Emits inline Python expression."""
        return self.current_genome.to_python()

    def get_stats(self) -> Dict[str, Any]:
        avg_eval_us = (self.eval_time_total / max(1, self.eval_count)) * 1_000_000
        expr = self.get_python_expr()
        tokens = expr.replace("(", " ").replace(")", " ").split()
        return {
            "tokens": len(tokens),
            "evolutions": self.total_evolutions,
            "hotswaps": self.total_hotswaps,
            "total_evo_sec": self.evolution_time_total,
            "avg_eval_us": avg_eval_us,
            "archive_size": len(self.regime_archives),
            "map_coverage": f"{self.global_map_archive.coverage * 100:.1f}%",
            "code_sample": self.get_python_code(),
            "expr": expr,
        }


if __name__ == "__main__":
    print("[Program 2: IRIS Evolution Engine] Initializing agent test...")
    agent = IrisEvolutionAgent(seed=42)

    sample_regimes = {
        0: [(10, 0), (20, 0), (30, 0)],    # Nominal -> Action 0
        1: [(80, 1), (90, 1), (100, 1)],  # Surge -> Action 1
        2: [(150, 2), (200, 2), (300, 2)], # Brownout -> Action 2
        3: [(500, 3), (600, 3), (700, 3)], # Attack -> Action 3
    }

    for r_id, cases in sample_regimes.items():
        agent.adapt_to_crisis(r_id, cases, generations=20)
        stats = agent.get_stats()
        print(f"Regime {r_id} Evolved Expr: {stats['expr']}")

    # Demonstrate instant zero-cost hot-swap back to Regime 0
    print("\nSimulating return to Regime 0 (Testing Catastrophic Forgetting resistance)...")
    agent.adapt_to_crisis(0, sample_regimes[0])
    stats = agent.get_stats()
    print(f"Hot-Swapped Regime 0 Expr: {stats['expr']} (Total Hot-Swaps: {stats['hotswaps']})")
    print(f"Generated Python Function:\n{stats['code_sample']}")
