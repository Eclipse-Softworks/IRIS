"""Quality-Diversity (MAP-Elites) algorithm for Python code genome evolution."""

import random
from dataclasses import dataclass
from typing import Dict, List, Optional, Tuple

try:
    from . import ffi
    from .genome import GeneNode
    from .search import EvolutionarySearch
except (ImportError, ValueError):
    import sys
    from pathlib import Path
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
    from iris_evolution import ffi
    from iris_evolution.genome import GeneNode
    from iris_evolution.search import EvolutionarySearch


@dataclass(frozen=True)
class NicheCoordinate:
    """Represents a 2D phenotypic niche: (complexity_bin, variance_bin)."""

    complexity_bin: int  # 0: Micro (1-3), 1: Compact (4-7), 2: Moderate (8-15), 3: Complex (16-30), 4: Deep (31+)
    variance_bin: int    # 0: Inactive (<0.1), 1: Low (<10), 2: Medium (<100), 3: High (>=100)

    @classmethod
    def compute(cls, expr_str: str, outputs: List[float]) -> "NicheCoordinate":
        # Estimate size from token count
        tokens = expr_str.replace("(", " ").replace(")", " ").split()
        size = max(1, len(tokens))

        if size <= 3:
            c_bin = 0
        elif size <= 7:
            c_bin = 1
        elif size <= 15:
            c_bin = 2
        elif size <= 30:
            c_bin = 3
        else:
            c_bin = 4

        if len(outputs) >= 2:
            mean = sum(outputs) / len(outputs)
            var = sum((x - mean) ** 2 for x in outputs) / len(outputs)
        else:
            var = 0.0

        if var < 0.1:
            v_bin = 0
        elif var < 10.0:
            v_bin = 1
        elif var < 100.0:
            v_bin = 2
        else:
            v_bin = 3

        return cls(complexity_bin=c_bin, variance_bin=v_bin)


@dataclass
class MapEliteEntry:
    """An elite code genome occupying a specific behavioral niche."""

    expr: str
    callable_fn: object
    loss: float
    mae: float
    exact_matches: int
    niche: NicheCoordinate
    generation: int


class MapElitesArchive:
    """Maintains a multi-dimensional grid of phenotypic elites."""

    def __init__(self, target_cases: List[Tuple[int, int]]):
        self.cases = target_cases
        self.grid: Dict[NicheCoordinate, MapEliteEntry] = {}
        self.total_evaluations = 0

    @property
    def occupied_count(self) -> int:
        return len(self.grid)

    @property
    def coverage(self) -> float:
        return len(self.grid) / 20.0  # 5 complexity * 4 variance niches

    def evaluate_candidate(self, py_expr: str, generation: int = 0) -> Optional[MapEliteEntry]:
        """Evaluates an expression and places it into its niche if competitive."""
        self.total_evaluations += 1
        try:
            safe_builtins = {"min": min, "max": max, "abs": abs}
            fn = eval(f"lambda x, _r=[0,0,0,0]: {py_expr}", {"__builtins__": safe_builtins}, {})
            outputs = []
            total_err = 0.0
            exact = 0
            for inp, exp in self.cases:
                pred = fn(inp)
                outputs.append(float(pred))
                err = abs(pred - exp)
                total_err += err
                if pred == exp:
                    exact += 1

            mae = total_err / max(1, len(self.cases))
            tokens = py_expr.replace("(", " ").replace(")", " ").split()
            parsimony_penalty = 0.001 * len(tokens)
            loss = mae + parsimony_penalty

            niche = NicheCoordinate.compute(py_expr, outputs)
            entry = MapEliteEntry(
                expr=py_expr,
                callable_fn=fn,
                loss=loss,
                mae=mae,
                exact_matches=exact,
                niche=niche,
                generation=generation,
            )

            if niche not in self.grid or loss < self.grid[niche].loss:
                self.grid[niche] = entry
                return entry
            return None
        except Exception:
            return None

    def best_overall(self) -> Optional[MapEliteEntry]:
        """Returns the single best elite with the lowest loss."""
        if not self.grid:
            return None
        return min(self.grid.values(), key=lambda e: e.loss)

    def sample_random_elite(self) -> Optional[MapEliteEntry]:
        """Uniformly samples an elite from an occupied niche."""
        if not self.grid:
            return None
        return random.choice(list(self.grid.values()))


if __name__ == "__main__":
    print("[MAP-Elites] Initializing Quality-Diversity Archive...")
    target_cases = [(x, 3 * x + 2) for x in range(-5, 6)]
    archive = MapElitesArchive(target_cases)

    # Seed with diverse expressions
    seeds = [
        "x",
        "(x + 2)",
        "(x * 3)",
        "((x * 3) + 2)",
        "max(x, (3 * x))",
        "abs(x)",
        "(x * 0)",
        "min((x * 3), (x + 2))",
    ]
    for expr in seeds:
        archive.evaluate_candidate(expr, generation=0)

    print(f"Archive coverage: {archive.coverage * 100:.1f}% ({archive.occupied_count}/20 niches occupied)")
    best = archive.best_overall()
    if best:
        print(f"Best Elite : {best.expr} (MAE={best.mae:.2f}, Exact={best.exact_matches}/{len(target_cases)})")
        print(f"Niche      : Complexity Bin {best.niche.complexity_bin}, Variance Bin {best.niche.variance_bin}")
