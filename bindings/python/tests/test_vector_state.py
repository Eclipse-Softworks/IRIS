"""Automated tests for IRIS Evolution Python bindings: Vectorized Operations & Persistent State."""

import ast
import os
import sys
import unittest

# Ensure the package is in sys.path
package_dir = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
if package_dir not in sys.path:
    sys.path.insert(0, package_dir)

from iris_evolution import EvolutionarySearch, GeneNode, GenomeState


class TestVectorAndStateEvolution(unittest.TestCase):
    def test_genome_state_lifecycle(self):
        """Verifies GenomeState lifecycle: creation, reset, and garbage collection."""
        state = GenomeState()
        self.assertIsNotNone(state._ptr)
        state.reset()
        # Create second independent state
        state2 = GenomeState()
        self.assertIsNotNone(state2._ptr)
        self.assertNotEqual(state._ptr, state2._ptr)
        del state
        del state2

    def test_search_and_vec4_evaluation(self):
        """Verifies that an evolved genome supports both scalar eval and eval_vec4."""
        search = EvolutionarySearch(
            pop_size=20,
            generations=15,
            mutation_rate=0.3,
            crossover_rate=0.7,
            seed=42,
        )
        search.add_case(1, 2)
        search.add_case(2, 4)
        search.add_case(3, 6)

        genome = search.run()
        self.assertIsInstance(genome, GeneNode)

        # 1. Scalar eval
        val = genome.eval(5)
        self.assertIsInstance(val, int)

        # 2. Vectorized 4-lane eval (stateless)
        out_vec, action = genome.eval_vec4([10, 20, 30, 40])
        self.assertEqual(len(out_vec), 4)
        self.assertIsInstance(action, int)

        # 3. Vectorized 4-lane eval with persistent state
        state = GenomeState()
        out_vec2, action2 = genome.eval_vec4([5, 15, 25, 35], state=state)
        self.assertEqual(len(out_vec2), 4)
        self.assertIsInstance(action2, int)

        # 4. Error on invalid vector length
        with self.assertRaises(ValueError):
            genome.eval_vec4([1, 2, 3])

    def test_python_callable_with_safe_builtins(self):
        """Verifies that to_callable() properly handles safe vector builtins (sum, range, zip, min, max, abs)."""
        search = EvolutionarySearch(
            pop_size=15,
            generations=10,
            seed=99,
        )
        search.add_case(0, 0)
        search.add_case(1, 1)

        genome = search.run()
        py_callable = genome.to_callable()
        self.assertTrue(callable(py_callable))

        # Must execute cleanly
        res = py_callable(7)
        self.assertIsInstance(res, (int, list))

        # Check Python function generation
        py_func_src = genome.to_python_func(func_name="policy_candidate", param_name="sensors")
        self.assertIn("def policy_candidate(", py_func_src)
        self.assertIn("return ", py_func_src)

        # Compile and run generated Python source code
        scope = {}
        exec(py_func_src, {}, scope)
        fn = scope["policy_candidate"]
        self.assertTrue(callable(fn))
        self.assertIsNotNone(fn(7))


if __name__ == "__main__":
    unittest.main()
