# Adaptive Code Genomes in Non-Stationary Environments: Cybernetic Homeostasis & Continuous Autonomous Evolution

*A Technical and Theoretical Architecture for Self-Evolving Autonomous Systems in IRIS*

---

## 1. Executive Summary

Traditional evolutionary computation and genetic algorithms operate under a fundamental flaw when applied to real-world intelligence: **the assumption of a stationary fitness function**. In canonical benchmark tasks, a genome is evaluated against a fixed target equation (e.g. symbolic regression $f(x) = 3x$) until it reaches a global optimum. 

However, in the natural biosphere and in real-world autonomous systems, **perfection against a static metric is an evolutionary trap**. When the environment shifts—whether through climatic changes, resource scarcity, sensor degradation, or adversarial intrusion—an overspecialized genome suffers catastrophic failure.

This research establishes the theoretical and computational architecture for **Adaptive Code Genomes** in IRIS:
1. **From Static Fitness to Cybernetic Homeostasis**: Grounded in W. Ross Ashby's *Design for a Brain* (Ultrastable Systems) and Humberto Maturana & Francisco Varela's theory of *Autopoiesis*, the evolutionary objective is reframed from "maximizing scalar score" to **preserving continuous viability within multi-dimensional physiological envelopes**.
2. **The Dual-Loop Autonomous Lifecycle**: Decoupling execution into a **Fast Reactive Loop** (high-frequency runtime interaction using leased native code) and a **Slow Epigenetic Loop** (continuous background genetic programming triggered by allostatic strain).
3. **Atomic JIT Hot-Swapping as Ontogenetic Adaptation**: How IRIS's LLVM ORC JIT generation leases permit an organism to rewrite and recompile its own decision law while actively executing, with zero interruption to ongoing survival behavior.

---

## 2. The Theoretical Failure of Static Optimization

### 2.1 The Convergence Trap

In classical Genetic Programming (Koza, 1992), selection pressure drives a population toward convergence around a fitness peak:

$$\lim_{t \to \infty} \sigma^2(\mathcal{P}_t) \to 0$$

Where $\sigma^2(\mathcal{P}_t)$ represents the genetic variance of the population at generation $t$. While this produces highly optimal solutions for fixed benchmarks, it systematically destroys **evolutionary plasticity**.

```
Fitness Peak in Regime A                Regime Shift (A -> B)
       ▲                                       ▲
       │       [Population]                    │                       [Population]
  Fit  │          ▲▲▲                          │                            † (Extinct)
       │         ▲▲▲▲▲                         │                          ▲▲▲
       │        ▲▲▲▲▲▲▲                        │     [New Peak]          ▲▲▲▲▲
       └──────────────────► Genome Space       └───────▲▲▲──────────────▲▲▲▲▲▲▲───►
```

When Regime A transitions to Regime B, the converged population finds itself on a fitness precipice. Because mutation rates in converged populations are typically low, the species cannot traverse the fitness valley before depleting its energy reserves, resulting in **extinction by overspecialization**.

### 2.2 Ashby's Law of Requisite Variety

W. Ross Ashby formulated the foundational law of cybernetics:
> *"Only variety can absorb variety."* (Ashby, 1956)

Mathematically, if an environment $\mathcal{E}$ presents a set of disturbances $\mathcal{D}$ with entropy $H(\mathcal{D})$, and the regulator/organism $\mathcal{R}$ has a repertoire of actions $\mathcal{A}$, the entropy of the organism's essential variables $H(\mathcal{V})$ satisfies:

$$H(\mathcal{V}) \ge H(\mathcal{D}) - H(\mathcal{A})$$

To keep essential survival variables within physiological limits ($H(\mathcal{V}) \to 0$), the variety of actions available to the organism must equal or exceed the variety of environmental perturbations:

$$H(\mathcal{A}) \ge H(\mathcal{D})$$

A static code genome has fixed action entropy $H(\mathcal{A})$. As the real world introduces novel or non-stationary disturbances $H(\mathcal{D}_{\text{new}})$, a static genome inevitably experiences fatal boundary violations.

---

## 3. The Cybernetic Homeostasis Paradigm

Instead of evaluating genomes against an external scoring oracle, we formulate survival as **viability boundary regulation**.

### 3.1 The Essential Survival Vector

An autonomous organism is defined by an internal state vector $\mathbf{v}(t) \in \mathbb{R}^n$:

$$\mathbf{v}(t) = \begin{bmatrix} E(t) \\ T_{\text{core}}(t) \\ H(t) \end{bmatrix} = \begin{bmatrix} \text{Internal Metabolic Energy} \\ \text{Internal Core Temperature} \\ \text{Structural Integrity / Health} \end{bmatrix}$$

The organism survives if and only if $\mathbf{v}(t)$ remains within a compact viability envelope $\Omega \subset \mathbb{R}^n$:

$$\Omega = \left\{ \mathbf{v} \in \mathbb{R}^3 \;\middle|\; E_{\min} \le E \le E_{\max}, \; T_{\min} \le T_{\text{core}} \le T_{\text{max}}, \; H > 0 \right\}$$

If $\mathbf{v}(t) \notin \Omega$, the organism dies.

### 3.2 Environmental Regimes & Thermodynamic Coupling

The environment is modeled as a non-stationary Markov decision process whose transition dynamics change across distinct macro-regimes:

$$\mathcal{E}(t) = \langle T_{\text{env}}(t), \; R(t), \; D(t) \rangle$$

Where:
* $T_{\text{env}}(t)$ is the ambient temperature.
* $R(t) \in [0, 1]$ is the local resource density.
* $D(t) \in [0, 1]$ is the hazard disturbance probability.

The physical coupling between organism and environment is governed by:

1. **Thermodynamic Drift**:
   $$\frac{d T_{\text{core}}}{dt} = -k_{\text{thermal}}(T_{\text{core}} - T_{\text{env}}) + Q_{\text{metabolic}}(a)$$
   The core temperature naturally drifts toward ambient temperature unless countered by metabolic action $Q_{\text{metabolic}}$.

2. **Metabolic Decay**:
   $$\frac{d E}{dt} = -c_{\text{basal}} - c_{\text{action}}(a) + \Delta E_{\text{foraged}}(R, a)$$
   Energy depletes continuously via basal metabolic rate $c_{\text{basal}}$ plus action costs, replenished only through successful foraging in resource-dense zones.

3. **Integrity Degradation**:
   $$\frac{d H}{dt} = -\gamma_{\text{thermal}} \max(0, |T_{\text{core}} - T_{\text{opt}}| - \Delta T_{\text{tol}}) - \gamma_{\text{starvation}} \mathbb{I}(E \le 0) - \text{Damage}(D, a)$$

### 3.3 Allostatic Load as an Evolutionary Trigger

**Allostasis** is the process of achieving stability through physiological or behavioral change (McEwen, 1998). We define the instantaneous **Allostatic Strain** $S(t)$:

$$S(t) = w_E \left(\frac{E_{\text{opt}} - E(t)}{E_{\text{opt}}}\right)^2 + w_T \left(\frac{T_{\text{core}}(t) - T_{\text{opt}}}{\Delta T_{\text{tol}}}\right)^2 + w_H (1.0 - H(t))$$

When environmental conditions are benign and the current policy maintains equilibrium, $S(t) \approx 0$. When a regime shift occurs (e.g., sudden ice age or toxic storm), $S(t)$ surges. 

In our architecture, **surging allostatic load acts as the biological distress beacon that awakens the background evolutionary search**.

---

## 4. The Dual-Loop Autonomous Architecture

To enable uninterrupted survival alongside continuous adaptation, the system is structured into two concurrent asynchronous loops:

```
┌────────────────────────────────────────────────────────────────────────┐
│                        FAST RUNTIME LOOP (10-50 Hz)                    │
│                                                                        │
│   Sensory Inputs ────► [Active LLVM ORC Policy] ────► Motor Action     │
│   (Env & Core)         (Leased Generation N)          (Forage/Shelter) │
│                                                             │          │
│                                                             ▼          │
│   Allostatic Strain ◄─── Internal Homeostat ◄────── Environment State  │
└──────────┬─────────────────────────────────────────────────────────────┘
           │
           │ (High Strain Triggers Epigenetic Search)
           ▼
┌────────────────────────────────────────────────────────────────────────┐
│                   SLOW EPIGENETIC / EVOLUTION LOOP                     │
│                                                                        │
│   Candidate Genomes (Pop N) ──► Genetic Programming (Mutate/Crossover) │
│                                              │                         │
│                                              ▼                         │
│   Seven Gates Verification  ◄── Simulation against Recent Buffer       │
│   (ABI, Safety, Rollback)                                              │
│               │                                                        │
│               ▼ (Promote)                                              │
│   Atomic Hot-Swap ───────────► Generation N+1 Installed in Fast Loop   │
└────────────────────────────────────────────────────────────────────────┘
```

### 4.1 Fast Runtime Loop
* Operates at high frequency.
* Evaluates the current active generation $G_N$ compiled to native machine code via LLVM ORC JIT.
* Holds an atomic generation lease ([`HotSwapLease`](file:///c:/Users/Moon/Desktop/Projects/IRIS/src/codegen/hot_swap.rs)):
  ```rust
  let lease = hot_swap.lease("policy")?;
  let action = lease.call_i64_1("policy", sensor_state)?;
  ```
* Ensures zero latency jitter and deterministic real-time control.

### 4.2 Slow Epigenetic Loop
* Runs asynchronously in the background.
* Maintains a rolling buffer of recent environmental observations and forecasted regime shocks.
* Generates candidate gene trees using grammar-guided Genetic Programming (`GeneNode`).
* Bounded simulation evaluates candidate fitness based on **survival duration and allostatic recovery**:
  $$\text{Fitness}(\mathcal{G}) = \int_0^{T_{\text{sim}}} \left( H(t) - \lambda S(t) - \mu \cdot \text{NodeCount}(\mathcal{G}) \right) dt$$
* When a candidate genome outperforms the active baseline by a certified margin, it enters the **7-gate verification pipeline**.
* Upon passing, LLVM ORC materializes the candidate, verifies symbols, and executes an **atomic pointer swap**.
* Existing worker threads finish on $G_N$; subsequent ticks immediately execute $G_{N+1}$ without thread interruption.

---

## 5. Mathematical Formulation of Action Genes

A policy genome takes composite sensory signals and outputs discrete survival behaviors:

$$\mathcal{A} \in \{ 0: \text{Forage}, \; 1: \text{Shelter}, \; 2: \text{CoolDown}, \; 3: \text{HeatUp} \}$$

| Action ID | Action Name | Energy Cost | Thermal Effect | Environmental Exposure | Primary Utility |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **0** | **Forage** | High ($-3.0$) | None | $100\%$ | Essential in resource abundance; lethal in storms. |
| **1** | **Shelter** | Minimal ($-0.5$) | $50\%$ Insulation | $20\%$ | Defends against storms & thermal spikes; zero food gain. |
| **2** | **CoolDown** | Moderate ($-1.5$) | Active cooling ($-2.5^\circ C$) | $40\%$ | Prevents hyperthermia in solar droughts. |
| **3** | **HeatUp** | High ($-2.5$) | Active heating ($+3.0^\circ C$) | $40\%$ | Prevents hypothermia in cryo-freezes. |

Because the policy is encoded as an AST gene tree with conditionals (`IfThenElse`) and relational logic, the genome can synthesize complex piecewise survival strategies:
```text
(if temp_delta < -10 { 3 } else { (if hazard > 60 { 1 } else { 0 }) })
```
Translation: *"If severely freezing, burn calories to heat up; otherwise, if hazard is severe, take shelter; else forage for food."*

---

## 6. The Real-Time Evolutionary Dynamics

When observing this system live across 4 alternating regimes:

```
Regime 1: Spring Glade  (T=22°C, R=0.9, D=0.05) -> Forage-dominant policies thrive.
Regime 2: Solar Drought (T=48°C, R=0.2, D=0.10) -> Heat-shedding & conservation evolve.
Regime 3: Cryo Freeze   (T=-18°C, R=0.1, D=0.15)-> Active metabolic heating evolves.
Regime 4: Toxic Storm   (T=15°C, R=0.05, D=0.85)-> Defensive sheltering evolves.
```

### Emergent Phenomena Observed:
1. **Dynamic Punctured Equilibrium**: The organism maintains a stable policy during steady seasons, followed by bursts of rapid mutation and hot-swapping when climate boundaries are crossed (Gould & Eldredge, 1972).
2. **Parsimony Compression**: Through the multi-objective parsimony weight, redundant branches are naturally pruned during evolutionary search, keeping the executable decision trees concise and verifiable.
3. **Generational Memory / Re-adaptation**: As regimes cycle back (e.g. from Freeze back to Glade), the background population rapidly rediscovers or re-activates latent foraging alleles, demonstrating adaptive resilience.

---

## 7. Conclusion

By shifting from static target optimization to **cybernetic homeostatic viability**, and coupling genetic AST expression with **live LLVM ORC JIT hot-swapping**, IRIS demonstrates that code genomes are not restricted to toy benchmarks. They can function as living, continuously adapting computational organisms capable of surviving open-ended, non-stationary worlds.
