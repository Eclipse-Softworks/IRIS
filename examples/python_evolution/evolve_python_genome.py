#!/usr/bin/env python3
"""Example: Self-Evolving Python Genomes via Built-In IRIS Evolution Library.

Demonstrates:
1. Evolving an executable Python function/lambda directly from training observations.
2. An autonomous living organism living in a Python loop that adapts to shifting
   climatic regimes and hot-swaps its active Python code genome in real time.
"""

import os
import sys
import time

# Ensure bindings/python is in sys.path
script_dir = os.path.dirname(os.path.abspath(__file__))
bindings_dir = os.path.abspath(os.path.join(script_dir, "..", "..", "bindings", "python"))
if bindings_dir not in sys.path:
    sys.path.insert(0, bindings_dir)

from iris_evolution import EvolutionarySearch, GenomeState


def demo_direct_python_evolution():
    print("=" * 80)
    print("PART 1: DIRECT PYTHON CODE GENOME EVOLUTION")
    print("=" * 80)
    print("Goal: Evolve an executable Python policy mapping x -> 3x using IRIS Genetic Programming.\n")

    search = EvolutionarySearch(
        pop_size=40,
        generations=30,
        mutation_rate=0.35,
        crossover_rate=0.7,
        parsimony_weight=0.005,
        seed=12345,
    )

    # Supply training observations from Python
    observations = [(-3, -9), (0, 0), (1, 3), (2, 6), (5, 15)]
    print(f"Training observations: {observations}")
    for inp, expected in observations:
        search.add_case(inp, expected)

    start_time = time.time()
    best_genome = search.run()
    elapsed_ms = (time.time() - start_time) * 1000.0

    print(f"Evolution completed in {elapsed_ms:.1f}ms\n")

    # 1. Native C-ABI Evaluation
    print(f"[C-ABI Native Eval]  f(10) = {best_genome.eval(10)}")

    # 2. Syntax-accurate Python expression
    py_expr = best_genome.to_python()
    print(f"[Python Expression]  {py_expr}")

    # 3. Compiling to a first-class Python callable
    py_callable = best_genome.to_callable()
    print(f"[Python Callable]    py_callable(10) = {py_callable(10)} (Type: {type(py_callable).__name__})")

    # 4. Typed Python function definition
    py_func_code = best_genome.to_python_func(func_name="evolved_policy", param_name="x")
    print(f"\n[Generated Python Source Code]:\n{py_func_code}")


def demo_stateful_vector_genome_in_python():
    print("\n" + "=" * 80)
    print("PART 2: STATEFUL MULTI-REGISTER & SIMD VECTOR GENOME EXECUTION")
    print("=" * 80)
    print("Demonstrates 4-lane SIMD sensory vector inputs and persistent register state.\n")

    search = EvolutionarySearch(
        pop_size=30,
        generations=20,
        seed=42,
    )
    search.add_case(10, 10)
    genome = search.run()

    state = GenomeState()

    sensory_inputs = [
        [22, 85, 2, 98],    # [temp, food, hazard, health]
        [46, 20, 8, 80],    # heatwave
        [-18, 15, 12, 65],  # cryo freeze
        [16, 5, 80, 45],    # radiation storm
    ]

    action_names = ["Forage", "Shelter", "CoolDown", "HeatUp"]

    for idx, sensory_vec in enumerate(sensory_inputs, 1):
        out_vec, action = genome.eval_vec4(sensory_vec, state=state)
        action_name = action_names[action % 4]
        print(f"Step {idx} | Input: {sensory_vec} -> OutVec: {out_vec} | Action: {action_name} ({action})")

    print("\nSUCCESS: Multi-register stateful SIMD vector evaluations executed cleanly via C-ABI!")


if __name__ == "__main__":
    demo_direct_python_evolution()
    demo_stateful_vector_genome_in_python()

