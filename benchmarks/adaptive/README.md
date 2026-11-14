# IRIS Adaptive Computational Substrate: Research Benchmarks

> **"IRIS could be used to investigate how computation can become adaptive rather than remaining fixed."**

This benchmark suite establishes IRIS as an experimental computational substrate for investigating self-modifying, adaptive, and scientific machine intelligence. Instead of treating intelligence as static parameter weights fitted inside rigid compute graphs, IRIS investigates the continuous synthesis, verification, and live promotion of computational structures under environmental selection.

---

## The Paradigm: Fixed Software vs Adaptive Organisms

Traditional software follows:
$$\text{Program} + \text{Input} \longrightarrow \text{Output}$$

Modern Deep Learning mostly follows:
$$\text{Fixed Architecture} + \text{Static Weights} + \text{Input} \longrightarrow \text{Inference}$$

When the physical plant breaks or the data distribution shifts, fixed systems fail catastrophically.

IRIS drives toward:
$$\text{Program} + \text{Environment} \longrightarrow \text{Viability Detection} \longrightarrow \text{AST Synthesis} \longrightarrow \text{7-Gate Verification} \longrightarrow \text{New Program (Zero-Downtime Hot-Swap)}$$

---

## The Three Iconic Benchmarks

### 1. The Damaged Plant Experiment (`01_damaged_plant/`)
* **Problem:** Physical plant failure under continuous control (actuator inversion / broken gearbox).
* **Failure of Fixed Code:** The nominal controller assumes negative feedback ($u = -3\theta$). When the plant's actuator gain flips from $+1 \rightarrow -1$, the fixed controller induces explosive positive feedback runaway, crashing the plant at tick $t = 57$ ($|\theta| > 9 \times 10^7$).
* **The IRIS Solution:**
  1. Homeostatic viability monitor detects error divergence at $t = 52$.
  2. Candidate policy $u = 4\theta$ is synthesized and submitted to the **Seven Evolution Gates**:
     - `LanguageSafety`: Valid lexical and effect typing.
     - `ResourceAndAbi`: Conforms to `(i64) -> i64` without heap leaks.
     - `DeterministicTests`: Evaluates assertions.
     - `TransactionRollback`: Proves rollback safety.
     - `SafetyConstitution`: Enforces $|u| \le 1000$ torque bounds.
     - `ShadowCanary`: Validates on canary distribution.
     - `VerifiedActivation`: Atomically installs LLVM ORC JIT generation lease.
  3. Live zero-downtime hot-swap occurs at $t = 54$ ($\tau_{\text{adapt}} = 4$ ticks).
  4. The plant instantly restabilizes to $\theta = 7$ mrad. Viability is preserved ($0\%$ crashes).

### 2. Symbolic Conservation Discovery (`02_symbolic_discovery/`)
* **Problem:** Automated scientific discovery of physical laws from raw observations rather than black-box curve fitting.
* **The Challenges:**
  1. **Harmonic Oscillator Hamiltonian:** Given trajectory states $(x(t), v(t))$ of an unknown physical system ($\ddot{x} = -4x$), discover the invariant Hamiltonian $H(x, v)$ where $\frac{dH}{dt} \approx 0$ along orbits without human prompting.
     $$H(x, v) = v^2 + 4x^2$$
  2. **Kepler Planetary Orbit Invariant (Angular Momentum):** Given 4D state telemetry $(x, y, v_x, v_y)$ of a planet in an eccentric gravitational orbit ($\ddot{\vec{r}} = -\mu \vec{r}/r^3$), autonomously discover Kepler's Second Law:
     $$\mathcal{L}(x, y, v_x, v_y) = (x \times v_y) - (y \times v_x)$$
* **The IRIS Solution:**
  - Uses typed `GeneNode` ASTs over state variables and algebraic operators $\{+, -, \times, \text{const}\}$.
  - Employs **MAP-Elites** quality-diversity search across structural complexity niches ($0$: Micro, $1$: Compact, $2$: Moderate, $3$: Complex).
  - Rewards orbit energy separation while penalizing intra-orbit variance and code bloat (Occam's razor).
  - Automatically discovers both exact analytical invariants.
  - Emits pure IRIS source code ([verify_law.iris](verify_law.iris) and [verify_kepler.iris](verify_kepler.iris)), compiles natively with LLVM, and verifies $< 1\%$ drift over full orbits.

### 3. Non-Stationary Generalization (`03_nonstationary_generalization/`)
* **Problem:** Out-of-distribution environmental drift across distinct regimes ($A \rightarrow B \rightarrow C \rightarrow A$) and the catastrophic forgetting of continual gradient descent.
* **The Challenge:** Routing queue workload cycling between:
  - *Regime A:* High-throughput batching.
  - *Regime B:* Low-latency deadline critical ($\le 2$ ticks).
  - *Regime C:* Severe thermal / resource throttling.
  - *Regime A (Return):* Testing retention of previously learned capability.
* **Empirical Comparison:**
  | Contender | Regime A | Regime B | Regime C | Regime A (Return) | Retention Ratio ($\rho$) | Re-adaptation Latency |
  | :--- | :---: | :---: | :---: | :---: | :---: | :---: |
  | **Fixed Baseline** | 95.0 | 10.0 (FAIL) | 25.0 (FAIL) | 95.0 | N/A | $\infty$ (Cannot adapt) |
  | **Continual Gradient** | 95.0 | 95.0 | 90.0 | 10.0 $\rightarrow$ 95.0 | **10.5%** (Severe forgetting) | 25 ticks (Slow re-training) |
  | **IRIS MAP-Elites** | **95.0** | **95.0** | **90.0** | **95.0** | **100.0%** (Zero forgetting) | **0 ticks** (Zero-shot swap) |

---

## Running the Benchmarks

### 1. Master Suite Execution (Python Polyglot Runner)
Run all three benchmarks with end-to-end execution, native IRIS compilation, and JSON reporting:

```powershell
python benchmarks/adaptive/runner.py
```

Results are saved to [results.json](results.json).

### 2. Native Rust Integration Test Suite
Run the benchmarks directly against the IRIS compiler, MAP-Elites archive, and LLVM ORC JIT hot-swap engine:

```powershell
cargo test --test adaptive_benchmarks
```

### 3. Running Individual Benchmarks
Each benchmark can also be executed independently:

```powershell
# Benchmark 1: Damaged Plant
python benchmarks/adaptive/01_damaged_plant/run.py
.\target\release\iris.exe run benchmarks/adaptive/01_damaged_plant/sim.iris
.\target\release\iris.exe run projects/native_adaptive_plant/main.iris

# Benchmark 2: Symbolic Discovery (Harmonic + Kepler)
python benchmarks/adaptive/02_symbolic_discovery/run.py
.\target\release\iris.exe run benchmarks/adaptive/02_symbolic_discovery/verify_law.iris
.\target\release\iris.exe run benchmarks/adaptive/02_symbolic_discovery/verify_kepler.iris

# Benchmark 3: Non-Stationary Generalization
python benchmarks/adaptive/03_nonstationary_generalization/run.py
.\target\release\iris.exe run benchmarks/adaptive/03_nonstationary_generalization/workload.iris
```

---

## Repository Structure

```
benchmarks/adaptive/
├── README.md                                 # This document
├── runner.py                                 # Master test runner
├── results.json                              # Machine-readable evaluation results
├── 01_damaged_plant/
│   ├── baseline.iris                         # Nominal policy: negative feedback
│   ├── candidate.iris                        # Adapted policy with 7-gate test harness
│   ├── cases.json                            # Canary cases for shadow evaluation
│   ├── constitution.txt                      # Inviolable actuator safety constitution
│   ├── sim.iris                              # Native IRIS physical dynamical simulation
│   └── run.py                                # End-to-end simulation & 7-gate hot-swap runner
├── 02_symbolic_discovery/
│   ├── discovery.py                          # MAP-Elites AST search for conserved Hamiltonian
│   ├── verify_law.iris                       # Native IRIS symplectic verification of discovered law
│   └── run.py                                # Discovery + native verification runner
└── 03_nonstationary_generalization/
    ├── bench.py                              # 4-Regime drift simulation comparing 3 paradigms
    ├── workload.iris                         # Native IRIS workload validation
    └── run.py                                # Generalization runner & retention reporter
```
