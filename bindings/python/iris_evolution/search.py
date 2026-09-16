"""Evolutionary search session and genetic programming optimization in Python."""

from typing import Iterable, Tuple
try:
    from . import ffi
    from .genome import GeneNode
except (ImportError, ValueError):
    import sys
    from pathlib import Path
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
    from iris_evolution import ffi
    from iris_evolution.genome import GeneNode


class EvolutionarySearch:
    """Orchestrates genetic programming search over typed code genomes."""

    def __init__(
        self,
        pop_size: int = 30,
        generations: int = 25,
        mutation_rate: float = 0.35,
        crossover_rate: float = 0.7,
        parsimony_weight: float = 0.01,
        seed: int = 0,
    ):
        self._ptr = ffi._lib.iris_evolution_search_create(
            int(pop_size),
            int(generations),
            float(mutation_rate),
            float(crossover_rate),
            float(parsimony_weight),
            int(seed),
        )
        if not self._ptr:
            raise RuntimeError("Failed to create IRIS evolutionary search session")

    def add_case(
        self, input_val: int, expected_val: int, weight: float = 1.0
    ) -> "EvolutionarySearch":
        """Adds an input/output training observation."""
        if not self._ptr:
            raise RuntimeError("Search session has already been freed")
        ffi._lib.iris_evolution_search_add_case(
            self._ptr, int(input_val), int(expected_val), float(weight)
        )
        return self

    def add_cases(
        self, pairs: Iterable[Tuple[int, int]]
    ) -> "EvolutionarySearch":
        """Adds multiple (input, expected) pairs."""
        for inp, exp in pairs:
            self.add_case(inp, exp)
        return self

    def run(self) -> GeneNode:
        """Executes the evolutionary search and returns the best discovered code genome."""
        if not self._ptr:
            raise RuntimeError("Search session has already been freed")
        genome_ptr = ffi._lib.iris_evolution_search_run(self._ptr)
        if not genome_ptr:
            raise RuntimeError(
                "Evolutionary search failed to find a valid candidate (no cases provided?)"
            )
        return GeneNode(genome_ptr)

    def __del__(self):
        if hasattr(self, "_ptr") and self._ptr:
            ffi._lib.iris_evolution_search_free(self._ptr)
            self._ptr = None


if __name__ == "__main__":
    print("[IRIS Evolutionary Search] Starting autonomous genetic programming...")
    search = EvolutionarySearch(pop_size=30, generations=20, seed=123)
    # Search for f(x) = 2 * x + 5
    cases = [(x, 2 * x + 5) for x in range(-5, 6)]
    search.add_cases(cases)
    best = search.run()
    print(f"Target function: f(x) = 2 * x + 5")
    print(f"Evolved Python : {best.to_python()}")
    print(f"Evolved IRIS   : {best.to_iris()}")
    callable_fn = best.to_callable()
    correct = sum(1 for x, y in cases if callable_fn(x) == y)
    print(f"Accuracy       : {correct}/{len(cases)} cases passed.")
