"""Automated tests for IRIS Evolution Python bindings."""

import ast
import os
import sys
import unittest

# Ensure the package is in sys.path
package_dir = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
if package_dir not in sys.path:
    sys.path.insert(0, package_dir)

from iris_evolution import EvolutionarySearch, GeneNode, GenomeState


class TestIrisEvolution(unittest.TestCase):
    def test_search_and_polyglot_python_callable(self):
        """Verifies that an evolutionary search generates an AST that compiles

        into a valid, directly executable Python callable.
        """
        search = EvolutionarySearch(
            pop_size=35,
            generations=25,
            mutation_rate=0.35,
            crossover_rate=0.7,
            parsimony_weight=0.01,
            seed=123,
        )

        # Target: identity f(x) = x
        search.add_case(0, 0)
        search.add_case(1, 1)
        search.add_case(5, 5)
        search.add_case(-4, -4)

        genome = search.run()
        self.assertIsInstance(genome, GeneNode)

        # 1. Bare-metal C-ABI evaluation
        self.assertEqual(genome.eval(0), 0)
        self.assertEqual(genome.eval(1), 1)
        self.assertEqual(genome.eval(5), 5)
        self.assertEqual(genome.eval(-4), -4)

        # 2. Syntax validation of generated Python expression
        py_expr = genome.to_python()
        self.assertTrue(len(py_expr) > 0)
        parsed_ast = ast.parse(py_expr, mode="eval")
        self.assertIsNotNone(parsed_ast)

        # 3. Execution as a first-class native Python callable
        py_callable = genome.to_callable()
        self.assertTrue(callable(py_callable))
        self.assertEqual(py_callable(0), 0)
        self.assertEqual(py_callable(1), 1)
        self.assertEqual(py_callable(5), 5)
        self.assertEqual(py_callable(10), 10)

        # 4. Typed Python function generation
        py_func_src = genome.to_python_func(func_name="my_policy", param_name="v")
        self.assertIn("def my_policy(v: int) -> int:", py_func_src)
        self.assertIn("return ", py_func_src)

        # Execute the generated Python function code
        scope = {}
        exec(py_func_src, {}, scope)
        my_policy = scope["my_policy"]
        self.assertEqual(my_policy(42), 42)

    def test_genome_state_and_vector_evaluation(self):
        """Verifies stateful register containers and SIMD vector evaluations."""
        search = EvolutionarySearch(
            pop_size=20,
            generations=15,
            seed=42,
        )
        search.add_case(1, 1)
        genome = search.run()
        self.assertIsInstance(genome, GeneNode)

        # Stateless vec4 eval
        out_vec, action = genome.eval_vec4([10, 20, 30, 40])
        self.assertEqual(len(out_vec), 4)
        self.assertIn(action, [0, 1, 2, 3])

        # Stateful vec4 eval
        state = GenomeState()
        out_vec2, action2 = genome.eval_vec4([10, 20, 30, 40], state=state)
        self.assertEqual(len(out_vec2), 4)
        self.assertIn(action2, [0, 1, 2, 3])
        state.reset()


if __name__ == "__main__":
    unittest.main()

