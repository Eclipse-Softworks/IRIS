# In-Memory Evolutionary JIT Superoptimizer

An algebraic expression optimizer and semantic equivalence proof engine written in native IRIS.

The superoptimizer ingests micro-op computational graphs, applies cost-guided peephole rewrites (strength reduction, shift coalescing, dead identity elimination), and formally proves zero-regression semantic equivalence against randomized and boundary test vectors before emitting machine-optimized code.

---

## Superoptimization Transformations

1. **Strength Reduction**:
   - Replaces high-latency multiplication nodes (`MUL(x, 2^k)`) with single-cycle bitwise shifts (`SHL(x, k)`), reducing execution latency from 4 cycles to 1 cycle ($75\%$ speedup).
2. **Algebraic Identity Folding**:
   - Eliminates redundant operations: `x ^ x` $\rightarrow 0$, `x * 1` $\rightarrow x$, `x + 0` $\rightarrow x$, collapsing entire multi-instruction subtrees to zero-latency identity variables.
3. **Sequential Shift Coalescing**:
   - Folds cascaded shifts: `(x << a) << b` $\rightarrow \text{SHL}(x, a + b)$, halving instruction dispatch overhead.
4. **Formal Equivalence Verification**:
   - Asserts exact bit-for-bit mathematical equivalence across exhaustive corner cases and thousands of randomized fuzzing inputs before accepting transformed candidates.

---

## Running the Superoptimizer

Execute the multi-kernel optimization and formal equivalence verification suite:

```powershell
.\target\release\iris.exe run projects/jit_superoptimizer/main.iris
```
