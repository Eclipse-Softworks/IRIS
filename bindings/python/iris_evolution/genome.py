"""Evolved code genome representations and Python callable conversion."""

import ctypes
from typing import Callable
try:
    from . import ffi
except (ImportError, ValueError):
    import sys
    from pathlib import Path
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
    from iris_evolution import ffi


class GenomeState:
    """Persistent multi-register state container for stateful genome execution."""

    def __init__(self):
        self._ptr = ffi._lib.iris_genome_state_create()
        if not self._ptr:
            raise RuntimeError("Failed to allocate GenomeState")

    def reset(self):
        """Resets all scalar and vector registers to zero."""
        if self._ptr:
            ffi._lib.iris_genome_state_reset(self._ptr)

    def __del__(self):
        if hasattr(self, "_ptr") and self._ptr:
            ffi._lib.iris_genome_state_free(self._ptr)
            self._ptr = None


class GeneNode:
    """Represents an evolved typed code genome AST."""

    def __init__(self, ptr: int):
        if not ptr:
            raise ValueError("Cannot construct GeneNode from NULL pointer")
        self._ptr = ptr

    def eval(self, input_val: int) -> int:
        """Evaluates the genome directly in bare-metal machine code via C-ABI."""
        if not self._ptr:
            raise RuntimeError("GeneNode has already been freed")
        return ffi._lib.iris_genome_eval(self._ptr, int(input_val))

    def eval_vec4(
        self,
        input_vec: list[int] | tuple[int, ...],
        state: GenomeState | None = None,
    ) -> tuple[list[int], int]:
        """Evaluates a 4-element vector input via C-ABI SIMD vector operations.

        Returns (output_vec4, argmax_action).
        """
        if not self._ptr:
            raise RuntimeError("GeneNode has already been freed")
        if len(input_vec) != 4:
            raise ValueError(f"Expected 4-element vector, got length {len(input_vec)}")

        c_in = (ctypes.c_int64 * 4)(*[int(x) for x in input_vec])
        c_out = (ctypes.c_int64 * 4)()

        if state is not None and state._ptr:
            action = ffi._lib.iris_genome_eval_stateful_vec4(
                self._ptr, state._ptr, c_in, c_out
            )
        else:
            action = ffi._lib.iris_genome_eval_vec4(self._ptr, c_in, c_out)

        return list(c_out), int(action)

    def __call__(self, input_val: int | list[int]) -> int | tuple[list[int], int]:
        """Enables direct invocation: `genome(5)` or `genome([1, 2, 3, 4])`."""
        if isinstance(input_val, (list, tuple)):
            return self.eval_vec4(input_val)
        return self.eval(input_val)

    def to_python(self) -> str:
        """Emits a valid, idiomatic Python expression string."""
        if not self._ptr:
            raise RuntimeError("GeneNode has already been freed")
        raw_ptr = ffi._lib.iris_genome_to_python_expr(self._ptr)
        return ffi.ptr_to_str(raw_ptr)

    def to_python_func(self, func_name: str = "policy", param_name: str = "x") -> str:
        """Emits a complete, typed Python function definition."""
        if not self._ptr:
            raise RuntimeError("GeneNode has already been freed")
        raw_ptr = ffi._lib.iris_genome_to_python_func(
            self._ptr,
            func_name.encode("utf-8"),
            param_name.encode("utf-8"),
        )
        return ffi.ptr_to_str(raw_ptr)

    def to_python_lambda(self, param_name: str = "x") -> str:
        """Emits an inline Python lambda expression."""
        if not self._ptr:
            raise RuntimeError("GeneNode has already been freed")
        raw_ptr = ffi._lib.iris_genome_to_python_lambda(
            self._ptr,
            param_name.encode("utf-8"),
        )
        return ffi.ptr_to_str(raw_ptr)

    def to_callable(self, param_name: str = "x") -> Callable:
        """Compiles the evolved Python expression into a native Python callable."""
        py_lambda = self.to_python_lambda(param_name)
        safe_builtins = {"min": min, "max": max, "abs": abs, "sum": sum, "range": range, "zip": zip}
        return eval(py_lambda, {"__builtins__": safe_builtins}, {})

    def to_iris(self) -> str:
        """Emits the equivalent IRIS expression string."""
        if not self._ptr:
            raise RuntimeError("GeneNode has already been freed")
        raw_ptr = ffi._lib.iris_genome_to_iris_expr(self._ptr)
        return ffi.ptr_to_str(raw_ptr)

    def __repr__(self) -> str:
        return f"GeneNode(expr={self.to_python()!r})"

    def __del__(self):
        if hasattr(self, "_ptr") and self._ptr:
            ffi._lib.iris_genome_free(self._ptr)
            self._ptr = None


if __name__ == "__main__":
    print("[IRIS Genome] Demonstrating genome creation and evaluation...")
    from iris_evolution import EvolutionarySearch
    search = EvolutionarySearch(pop_size=20, generations=10, seed=42)
    search.add_cases([(-2, -6), (0, 0), (2, 6), (5, 15)])
    g = search.run()
    print(f"Discovered Python expr : {g.to_python()}")
    print(f"Discovered IRIS expr   : {g.to_iris()}")
    py_func = g.to_callable()
    print(f"Evaluated with input=10: {py_func(10)} (Expected: 30)")
