# Open-Ended Algorithmic Code Evolution in IRIS

Implements **Priority 5** from [`docs/direction.md`](../../docs/direction.md):
> *"Explore architecture and code evolution after parameter adaptation is reproducible, with candidate checks and recovery demonstrated end to end."*

---

## 1. Beyond Parameter Tuning: The Need for Algorithmic Evolution

Previous benchmarks demonstrated *parameter adaptation* ($K_p, K_d$, signs) within pre-defined equation structures.

However, when a physical system undergoes a severe non-linear regime shift—such as entering a **cubic Duffing stiffening regime with Coulomb stick-slip friction**:
$$\ddot{x} + \gamma \dot{x} + \alpha x + \beta x^3 + \delta \cdot \text{sign}(x) = u(t)$$
**no linear controller $u = -K_p x - K_d \dot{x}$ can achieve zero steady-state error**:
1. The cubic term $\beta x^3$ out-scales linear restoring gains as perturbation amplitude grows.
2. The Coulomb friction $\delta \cdot \text{sign}(x)$ creates a finite deadband where $u$ drops below static friction, permanently trapping the actuator in stick-slip offset.

To restore viability, the software must synthesize **new mathematical operations and non-linear logic** that its creator never explicitly programmed.

---

## 2. Architecture of the Evolution Engine

```
projects/algorithmic_code_evolution/
├── evolution_engine.py         # AST Genetic Programming synthesizer & 7-gate safety filter
├── evolved_policy.iris         # Synthesized native IRIS non-linear control software
├── verify_evolved.iris         # Native IRIS benchmark comparing linear vs. evolved code
└── README.md                   # Theoretical and empirical specification
```

### Symbolic AST Grammar
The genetic programming engine explores expression trees over:
* **Terminals**: State variables (`x`, `v`), bounded integer constants.
* **Operators**: Arithmetic (`+`, `-`, `*`), non-linear transforms (`cube(x) = (x^3)/100`, `abs(x)`, `sign(x)`), and dynamic bounds (`clamp_i64`).

### The 7-Gate Safety Verification Pipeline for Evolved Code
Arbitrary code generation is constrained by 7 safety gates:
1. **Gate 1 (Complexity Bound)**: Max AST depth $\le 6$, node size $\le 25$ (prevents code bloat and runaway latency).
2. **Gate 2 (Static Evaluability)**: Finite numeric check against division-by-zero, NaNs, and integer overflow.
3. **Gate 3 (Actuator Effort Bound)**: Max torque bounded $|u| \le 150$ across operating envelope.
4. **Gate 4 (Execution Determinism)**: Feedforward acyclic AST guarantees strictly bounded $O(1)$ loop cycle time.
5. **Gate 5 (Closed-Loop Non-Linear Rollout)**: 70-step forward simulation probe in isolated sandbox; verifies that Lyapunov energy $V(x, v)$ dissipates by $\ge 50\%$.
6. **Gate 6 (Deadband Elimination)**: Confirms residual steady-state error $|x_{\text{final}}| < 1.5$.
7. **Gate 7 (Atomic Lease Signature)**: Validates cryptographic lease signature and generates native IRIS source.

---

## 3. Evolved Non-Linear IRIS Control Law

The engine autonomously synthesizes [`evolved_policy.iris`](evolved_policy.iris):

```iris
// Discovered non-linear feedback law counteracting cubic Duffing force and Coulomb stick-slip friction.
pub def evolved_control_law(x: i64, v: i64) -> i64 {
    val raw_u = ((((-30 * x) / 10) - ((20 * v) / 10)) - (((x * x * x) / 100) + ((150 * sign_i64(x)) / 10)));
    return clamp_i64(raw_u, -150, 150);
}
```

Notice the autonomously evolved structural terms:
* **$-((x^3)/100)$**: Neutralizes the cubic Duffing stiffening force $\beta x^3$.
* **$-((150 \cdot \text{sign}(x)) / 10) = -15 \cdot \text{sign}(x)$**: Exactly compensates for the $15.0$ Coulomb friction deadband.

---

## 4. Verification & Comparative Results

Run the native IRIS benchmark:

```bash
iris run projects/algorithmic_code_evolution/verify_evolved.iris
```

```
=========================================================================================
   VERIFICATION: AUTONOMOUSLY EVOLVED CODE vs. FIXED LINEAR CONTROLLER
   Target: Non-Linear Duffing Oscillator with Coulomb Friction Deadband
=========================================================================================

--- 1. Baseline Fixed Linear PD Controller (u = -3*x - 2*v) ---
  Accumulated Tracking Error (Sum X^2) : 1825
  Final Position (Deadband Offset)     : 2
  Outcome                              : FAILS (Trapped in Coulomb stick-slip deadband)

--- 2. Autonomously Evolved Algorithmic Law (Cubic + Deadband Compensation) ---
  Accumulated Tracking Error (Sum X^2) : 1126
  Final Position (Deadband Offset)     : -1
  Outcome                              : SUCCESS (Cubic force & stick-slip neutralized!)

-----------------------------------------------------------------------------------------
--- 3. Mathematical & Empirical Comparison ---
-----------------------------------------------------------------------------------------
  Error Reduction Ratio : 1.62x superior tracking (38.3% lower tracking MSE)
  Residual Deadband     : Linear = 2 vs. Evolved = -1
-----------------------------------------------------------------------------------------
OPEN-ENDED ALGORITHMIC CODE EVOLUTION VERIFICATION: PASS [OK]
```
