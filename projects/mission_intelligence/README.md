# IRIS Mission Intelligence — Simulated Mission Benchmark

**Research Prototype -- Eclipse Softworks -- Simulated Mission Benchmark**

> **Mission:** Give scientific instruments and autonomous robotic systems the ability to discover patterns, detect unexpected conditions, and make better-informed decisions from their observations -- while guaranteeing flight hardware safety through an independent, verified governor.

---

## 1. Architectural Overview

```
 +-----------------------------------------------------------------------------------+
 |                             SPACE MISSION ENVIRONMENT                             |
 |  Europa Orbit (a=1850km, e=0.12, T=8834.4s) | Induction | Power | Thermal | Vault |
 +-----------------------------------------+-----------------------------------------+
                                           | Telemetry & Noisy Observations
                                           v
 +-----------------------------------------------------------------------------------+
 |                            IRIS SCIENTIFIC INFERENCE                              |
 |   * 3-Stage Independent Split: Train (50%) | Val (25%) | Untouched Test (25%)     |
 |   * Experiment 2A: Discrete Hypothesis Selection (Time-Varying Ocean Induction)   |
 |   * Experiment 2B: Continuous Parameter Recovery (Blind recovery of A_ind = 0.981)|
 |   * Multi-Condition Anomaly Benchmark: 5 stress conditions (drifts, compounds)   |
 |   * Epistemic Uncertainty Mapping: 12-sector Bayesian orbital variance U(nu)      |
 +-----------------------------------------+-----------------------------------------+
                                           | Inferred Models & Uncertainty
                                           v
 +-----------------------------------------------------------------------------------+
 |                        DECISION & EXPERIMENT PLANNING                             |
 |   * Active Experiment Selection: Maximizing Expected Information Gain (EIG)       |
 |   * Target Prioritization: Periapsis passes, anomalous plasma flares, high uncert |
 |   * Proposes Actions: SPECTRAL_SURVEY, MAG_SOUNDING, IDLE_MONITOR, PURGE, DESAT   |
 +-----------------------------------------+-----------------------------------------+
                                           | Proposed Actions (Ambitious / Unverified)
                                           v
 +-----------------------------------------------------------------------------------+
 |                       INDEPENDENT HARD SAFETY GOVERNOR                            |
 |   [Gate 1] Optical Sun-Pointing Exclusion Zone (Boresight > 35.0 deg)             |
 |   [Gate 2] Battery Reserve & Bus Overcurrent Ceiling (SoC >= 30.0%, P <= 75.0 W)  |
 |   [Gate 3] Bus Thermal Excursion Ceiling (T <= +52.0 C)                           |
 |   [Gate 4] Reaction Wheel Momentum Saturation Limit (|h_rw| <= 11.0 Nms)          |
 |   [Gate 5] Pathological / Corrupted Telemetry Filtering (Fail-Closed Safety)      |
 +-----------------------------------------+-----------------------------------------+
                                           | Verified / Clamped Executable Actions
                                           v
                                  [Flight Actuators]
```

### The Core Architectural Design Decision
IRIS is permitted to propose **ambitious, unconstrained scientific discoveries and exploratory decisions**, while an **independently verified safety governor** has strict veto and clamping authority over which actions are physically dispatched to flight actuators.

This decoupling allows the science AI to aggressively hunt high-value transient events (e.g. attempting to stare into an active Jovian flux tube near the Sun vector) without risking optical focal plane burnout, bus undervoltage, or loss of attitude control authority.

---

## 2. Core Modules

| Module | Description |
|---|---|
| [`types.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/projects/mission_intelligence/types.iris) | Domain contracts: `SpacecraftState`, `RawObservation`, `AgentTelemetry`, `EnvironmentTimelineStep`, `TrajectoryPoint`, `HypothesisResult`, `EquationCandidate`, `ConfusionMatrix`, `AnomalyStressScenario`, `AdversarialSafetyResult`, `MonteCarloSummary`, `MissionDebrief`. |
| [`environment.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/projects/mission_intelligence/environment.iris) | Realistic Europa orbital mechanics ($R_p=1560.8\text{ km}$, $\mu=3202.73\text{ km}^3/\text{s}^2$, $a=1850.0\text{ km}$, $e=0.120$, $T=8834.405\text{ s}$), analytical Kepler conic solver via Newton-Raphson, time-varying electromagnetic ocean induction physics (11.1h synodic oscillation, subsurface saline ocean induced dipole), precomputed environment timeline generator, and NASA Europa Clipper 9.2 mm Al-Zn vault radiation model. |
| [`inference.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/projects/mission_intelligence/inference.iris) | 3-stage partitioned hypothesis competition (50% train, 25% validation, 25% untouched test) evaluating 6 competing models ($H_0 \dots H_5$) via MSE, $R^2$, and conventional Train BIC; continuous parameter recovery of ocean induction response amplitude $A_{\text{ind}}$; 5-scenario multi-condition anomaly stress benchmark; and 12-sector epistemic uncertainty. |
| [`planner.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/projects/mission_intelligence/planner.iris) | Epistemic experiment planner selecting observations by Expected Information Gain (EIG) and opportunistic sounding of transient phenomena (purely telemetry-driven; no access to ground truth labels). |
| [`safety.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/projects/mission_intelligence/safety.iris) | Separately verified 5-gate safety governor with fail-closed architecture and an expanded 25-vector adversarial test suite across 6 risk categories with 4-quadrant arbitration accounting. |
| [`display.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/projects/mission_intelligence/display.iris) | Real-time orbital situation radar, telemetry oscilloscope, numerical vs analytical reference trajectory audit table, hypothesis competition table, multi-condition stress table, Monte Carlo trial table, radiation budget report, and debriefing scorecard renderer (100% pure ASCII). |
| [`main.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/projects/mission_intelligence/main.iris) | End-to-end 100-step mission testbench, numerical vs analytical reference comparison, 25-vector adversarial audit, 20-trial matched Monte Carlo experiment suite with precomputed timelines and withheld anomaly labels, and automated export of audit CSV and JSON artifacts. |

---

## 3. Five Methodological Evidential Standards

### 3.1 Orbital Physics, Integration & Independent Reference Validation
The central planetary body is calibrated to the Jovian moon **Europa**:
- **Planetary Radius:** $R_p = 1560.8\text{ km}$
- **Gravitational Parameter:** $\mu = 3202.73\text{ km}^3/\text{s}^2$
- **Derived Mean Planetary Density:**
  $$\rho = \frac{3 \mu}{4 \pi G R_p^3} \approx 3012.9\text{ kg/m}^3 \quad (\text{silicate mantle + water-ice crust})$$
- **Orbit Geometry:** $a = 1850.0\text{ km}$, $e = 0.120$
  $$r_p = a(1 - e) = 1628.0\text{ km} \implies h_p = 67.2\text{ km}$$
  $$r_a = a(1 + e) = 2072.0\text{ km} \implies h_a = 511.2\text{ km}$$
- **Directly Derived Orbital Period (Kepler's 3rd Law):**
  $$T = 2\pi \sqrt{\frac{a^3}{\mu}} = 2\pi \sqrt{\frac{(1850)^3}{3202.73}} = 8834.40484\dots\text{ s} \approx 8834.405\text{ s} \quad (2.454\text{ hours})$$
- **Propagation Methodology & Independent Reference Comparison:**
  - **Flight Numerical Integrator:** Explicit 2nd-order Runge-Kutta (midpoint) numerical stepping of differential equation $\frac{d\nu}{dt} = \frac{\sqrt{\mu p}}{r^2}$ with fixed timestep $\Delta t = 88.3440\text{ s}$ ($100$ steps per revolution).
  - **State Reconstruction Method:** True anomaly $\nu(t)$ is integrated along the orbit. Radial distance and orbital velocity magnitude are reconstructed directly via the conic geometric invariants:
    $$r(\nu) = \frac{p}{1 + e \cos \nu}, \qquad v(\nu) = \sqrt{\mu \left(\frac{2}{r} - \frac{1}{a}\right)}$$
  - **Energy Conservation Qualification:** Because $r(\nu)$ and $v(\nu)$ are reconstructed algebraically from the conic orbital equations, specific orbital energy $\mathcal{E} = \frac{v^2}{2} - \frac{\mu}{r} = -\frac{\mu}{2a}$ is constant along the conic manifold by construction ($|\Delta\mathcal{E}/\mathcal{E}| < 10^{-6}$, $0.000\text{ ppm}$). Therefore, energy conservation is an algebraic consequence of the parameterization, NOT an independent validation of general-purpose orbital dynamics integration.
  - **Independent Trajectory Validation:** The genuine independent test of the numerical integrator is its trajectory divergence over time against the analytical Kepler conic solution ($|r_{\text{num}}(t) - r_{\text{ref}}(t)|$), solved via Newton-Raphson iteration of Kepler's equation.
  - **Numerical Midpoint Closure Residual:** $|\Delta r_{\text{closure}}| = |r(100) - r(0)| = 2.12 \times 10^{-6}\text{ km}$ ($2.12\text{ mm}$ residual across an $11,624\text{ km}$ trajectory).
  - **Analytical Conic Closure:** Exactly $0.00\text{ mm}$ (mathematical periodicity).
  - **Peak Trajectory Divergence:** $30.50\text{ m}$ ($30,501\text{ mm}$), representing the peak accumulated truncation error of the fixed-step midpoint integrator midway through the orbit before re-closing at periapsis.
  - **Velocity Closure Residual:** $|\Delta v_{\text{closure}}| = |v(100) - v(0)| = 1.7272 \times 10^{-9}\text{ km/s}$.
- **Telemetry vs Ground Truth Clarification:** Step 000 in the telemetry oscilloscope reports measured radar altimeter telemetry ($68.0\text{ km}$, incorporating $\pm 0.6\text{ km}$ sensor noise); true Keplerian periapsis altitude is strictly $67.2\text{ km}$ ($r_0 - R_p = 1628.0 - 1560.8\text{ km}$).

#### Trajectory Checkpoint Audit Log (Numerical Midpoint vs Analytical Conic Reference)
```
Step | Time (s) | Num Rad (km) | Ref Rad (km) | Diff (mm) | Num Vel (km/s) | Ref Vel (km/s) | Energy Drift
-----+----------+--------------+--------------+-----------+----------------+----------------+-------------
000  | 0.0      | 1628.0       | 1628.0       | 0.00 mm   | 1.4844         | 1.4844         | 0.000 ppm
010  | 883.4    | 1681.1       | 1681.1       | 4075.58 mm| 1.4419         | 1.4419         | 0.000 ppm
020  | 1766.9   | 1806.7       | 1806.7       | 2980.96 mm| 1.3469         | 1.3469         | 0.000 ppm
030  | 2650.3   | 1941.2       | 1941.2       | 3212.94 mm| 1.2524         | 1.2524         | 0.000 ppm
040  | 3533.8   | 2037.6       | 2037.6       | 4566.73 mm| 1.1885         | 1.1885         | 0.000 ppm
050  | 4417.2   | 2072.0       | 2072.0       | 0.33 mm   | 1.1663         | 1.1663         | 0.000 ppm
060  | 5300.6   | 2037.6       | 2037.6       | 8652.44 mm| 1.1884         | 1.1885         | 0.000 ppm
070  | 6184.1   | 1941.3       | 1941.2       | 20328.8 mm| 1.2524         | 1.2524         | 0.000 ppm
080  | 7067.5   | 1806.7       | 1806.7       | 30016.2 mm| 1.3469         | 1.3469         | 0.000 ppm
090  | 7951.0   | 1681.1       | 1681.1       | 23289.6 mm| 1.4419         | 1.4419         | 0.000 ppm
100  | 8834.4   | 1628.0       | 1628.0       | 2.12 mm   | 1.4844         | 1.4844         | 0.000 ppm
```

#### Timestep Refinement & Asymptotic Integrator Convergence Study
To rigorously validate the numerical integrator and confirm that the $30.50\text{ m}$ peak discrepancy represents expected finite-timestep truncation error rather than formulation defect, we execute a systematic timestep refinement study ($N = 50, 100, 200, 400, 800$ steps) against the analytical Kepler conic solution:

| Steps ($N$) | $\Delta t$ (s) | Max Pos Err (m) | Pos Ratio | Pos Order ($\alpha$) | Max Vel Err (mm/s) | Vel Ratio | Vel Order ($\alpha$) | Closure Err (mm) |
|---|---|---|---|---|---|---|---|---|
| 50 | 176.688 | 121.754 | --- | --- | 90.508 | --- | --- | 33.9123 |
| 100 | 88.344 | 30.501 | 3.992 | 1.997 | 22.711 | 3.985 | 1.995 | 2.1216 |
| 200 | 44.172 | 7.633 | 3.996 | 1.999 | 5.685 | 3.995 | 1.998 | 0.1326 |
| 400 | 22.086 | 1.909 | 3.997 | 1.999 | 1.422 | 3.997 | 1.999 | 0.0083 |
| 800 | 11.043 | 0.477 | 3.999 | 2.000 | 0.356 | 3.999 | 2.000 | 0.0005 |

- **Quadratic Convergence Confirmation:** As $\Delta t$ is repeatedly halved, both maximum position error and maximum velocity error decrease by an exact factor of $\approx 4.00$, yielding an empirical convergence order of $\alpha = \frac{\ln(\text{Ratio})}{\ln(2)} \approx 2.000$. This confirms textbook $\mathcal{O}(\Delta t^2)$ quadratic convergence for the 2nd-order Runge-Kutta midpoint method.
- **Independent Validation Principle:** As noted in the qualification above, energy conservation is algebraic; the independent empirical demonstration of numerical integrator validity is this asymptotic convergence of trajectory error toward zero against the independent Kepler conic reference.
- **Closure Discrepancy:** The full-orbit closure error decays even faster, dropping below $0.001\text{ mm}$ for $N \ge 800$.

---

### 3.2 Methodological Distinction: Model Selection vs Parameter Recovery in Realistic Europa Induction Physics

Observations are strictly partitioned into three independent subsets:
1. **Stage 1: Training Set (50%, $N_{\text{tr}} = 35$):** Used exclusively to fit parameters and compute conventional Training BIC ($\text{BIC}_{\text{tr}} = N_{\text{tr}} \ln(\text{MSE}_{\text{tr}}) + k \ln N_{\text{tr}}$).
2. **Stage 2: Validation Set (25%, $N_{\text{val}} = 17$):** Used exclusively to select the winning discrete hypothesis via Validation MSE and predictive score.
3. **Stage 3: Final Test Set (25%, $N_{\text{te}} = 17$):** Completely untouched during fitting and selection. The winning model is evaluated once on this final partition to report true generalization.

#### Physical Modeling of Europa Subsurface Ocean Induction
NASA explains that Jupiter's changing magnetic field induces an electrical response in Europa, likely via induction currents in its subsurface conducting saline ocean. Europa Clipper's magnetic measurements (ECM) are designed to characterize the thickness and conductivity of this ocean and icy shell.

- **Coordinate System:** Europa-Centered Orbital Coordinates ($\hat{x}$ along orbital velocity / corotation direction, $\hat{y}$ radially toward Jupiter, $\hat{z}$ normal to Europa's orbital plane / north).
- **Shared Expressive Jovian Background Field ($B_J(t)$):**
  Accounting for both the $\sim 11.1\text{ h}$ synodic rotation and $\sim 85.2\text{ h}$ orbital revolution periods:
  $$B_J(t) = B_{J0} + B_1 \cos(\omega_{\text{syn}} t) + B_2 \cos(\omega_{\text{orb}} t)$$
  where:
  - $B_{J0} = 210.0\text{ nT}$ is the static equatorial background field at Europa's distance ($9.38\text{ }R_J$).
  - $B_1 = 85.0\text{ nT}$ is the primary synodic oscillation amplitude driven by Jupiter's $9.6^\circ$ magnetic dipole tilt at $T_{\text{syn}} \approx 11.11\text{ hours} \approx 40,000\text{ s}$ ($\omega_{\text{syn}} \approx 1.5708 \times 10^{-4}\text{ rad/s}$).
  - $B_2 = 12.0\text{ nT}$ is the secondary orbital oscillation amplitude driven by Europa's orbital revolution around Jupiter at $T_{\text{orb}} \approx 85.23\text{ hours} \approx 306,828\text{ s}$ ($\omega_{\text{orb}} \approx 2.0478 \times 10^{-5}\text{ rad/s}$).
- **Initial Magnetic-Field Decomposition at Step 000 ($t = 0$):**
  At periapsis ($r = 1628.0\text{ km}$, true anomaly $\nu = 0$), the oscilloscope reports $B_{\text{meas}}(0) = 376.9\text{ nT}$, which exactly traces to:
  $$\begin{aligned}
  B_J(0) &= B_{J0} + B_1 \cos(0) + B_2 \cos(0) = 210.0 + 85.0 + 12.0 = 307.0\text{ nT} \\
  B_{\text{ind}}(0) &= A_{\text{ind}} \cdot B_1 \cos(-\phi) \cdot \left(\frac{R_{\text{ocean}}}{r_p}\right)^3 = 0.980 \times 85.0 \times \cos(-0.10) \times \left(\frac{1530}{1628}\right)^3 = 68.8\text{ nT} \\
  \text{noise}_{\text{mag}}(0) &= 1.5 \sin(0) + 1.2 \cos(0) = +1.2\text{ nT} \\
  B_{\text{meas}}(0) &= 307.0 + 68.8 + 1.2 = 377.0\text{ nT} \approx 376.9\text{ nT}
  \end{aligned}$$
- **Induced Saline Ocean Dipole Response:**
  For a conducting spherical shell of radius $R_{\text{ocean}} = 1530\text{ km}$, Faraday induction induces eddy currents generating a secondary induced dipole moment with inductive amplitude $A_{\text{ind}} \approx 0.980$ and phase lag $\phi \approx 0.10\text{ rad}$:
  $$B(r, t) = B_J(t) + A_{\text{ind}} \cdot B_1 \cos(\omega_{\text{syn}} t - \phi) \cdot \left(\frac{R_{\text{ocean}}}{r}\right)^3$$
- **Corrected Phase-Lag Arithmetic:**
  At synodic period $T_{\text{syn}} = 40,000\text{ s}$ ($11.11\text{ h}$), an inductive phase lag of $\phi = 0.10\text{ rad}$ corresponds to an exact time delay of:
  $$\Delta t = \frac{\phi}{2\pi} T_{\text{syn}} = \frac{0.10}{2\pi} \times 40,000\text{ s} = 636.62\text{ s} \approx 10.61\text{ minutes}$$
  *(Corrected from a previous arithmetic error stating 18 minutes; an 18-minute delay would require $\phi \approx 0.170\text{ rad}$ at this period).*
- **Physical Signal Magnitude & MSE Explanation:**
  Along Europa Clipper's trajectory ($r = 1628 - 2072\text{ km}$), the ratio $(R_{\text{ocean}}/r)^3$ ranges from $0.4036$ (apoapsis) to $0.8300$ (periapsis). The physical ocean induction term oscillates between $33.6\text{ nT}$ and $69.1\text{ nT}$, with an orbital RMS amplitude of $\approx 42.9\text{ nT}$.
  In the controlled ablation below, omitting this physical term ($A_{\text{ind}} = 0$ in $H_4$) leaves $(42.9)^2 \approx 1840.4\text{ nT}^2$ in unmodeled variance ($\text{Val MSE} = 1840.4\text{ nT}^2$). Freely fitting $A_{\text{ind}}$ ($H_5$, $\hat{A}_{\text{ind}} = 0.981$) resolves the signal, dropping MSE by $99.95\%$ to the $0.91\text{ nT}^2$ instrument noise floor.
- **Scientific Evidence Qualification:**
  NASA Galileo magnetometer observations independently established the presence of Europa's subsurface ocean. This benchmark demonstrates autonomous algorithmic model selection and parameter recovery on simulated induction physics.

#### Experiment 2A: Controlled Model Ablation & Selection Competition (3-Stage Partition)

| Hypothesis ID | Model Family | Formulation | Free Params ($k$) | Train BIC | Val MSE | Val Pred Score | Test MSE | Test $R^2$ | Status |
|---|---|---|---|---|---|---|---|---|---|
| $H_0$ | Constant Static Field | $B(r) = c_0$ | 1 | 256.6 | 1,287.5 | 124.6 | 1,345.4 | -0.0014 | Rejected |
| $H_1$ | Linear Spatial Decay | $B(r) = c_0 + c_1 r$ | 2 | 260.0 | 1,267.1 | 127.1 | 1,351.7 | -0.0061 | Rejected |
| $H_2$ | Static Inverse-Square | $B(r) = c / r^2$ | 1 | 296.9 | 3,705.7 | 142.5 | 4,376.2 | -2.2573 | Rejected |
| $H_3$ | Static Radial Dipole | $B(r) = c / r^3$ | 1 | 321.5 | 7,566.2 | 154.7 | 8,642.1 | -5.4325 | Rejected |
| $H_4$ | **Ablation Null Model (Shared $B_J(t)$, $A=0$)** | $B_J(t) + 0 \cdot \frac{R_o^3}{r^3}$ | 1 | 265.9 | 1,840.4 | 130.6 | 1,729.0 | -0.2870 | Rejected |
| **$H_5$** | **Ocean Induction (Shared $B_J(t)$, $A$ fitted)** | $B_J(t) + A_{\text{ind}} B_1\cos(\omega t - \phi) \frac{R_o^3}{r^3}$ | 2 | **15.6** | **0.91** | **5.7** | **1.48** | **0.9989** | **Selected Winner** |

- **Strict Controlled Ablation:** Competing models $H_4$ and $H_5$ share the exact same expressive Jovian background field $B_J(t)$ ($11.1\text{ h}$ synodic and $85.2\text{ h}$ orbital terms). The models differ **exclusively** by the physical ocean induction term.
- **Negative Test-Set $R^2$ Unclamped:** Out-of-sample $R^2$ values are truthfully reported without artificial clamping to zero. Because $H_0 - H_4$ predict worse than the test-set mean, their test-set $R^2$ values are properly negative.
- **Winning Model:** **$H_5$ (Coupled Ocean Induction)** decisively wins discrete model selection by achieving the lowest Validation MSE ($0.91\text{ nT}^2$) and lowest Train BIC ($15.6$).

#### Experiment 2B: Decoupled Continuous Parameter Recovery & Multi-Seed Robustness
- **Continuous Least-Squares Task:** Least-squares estimation of unknown ocean induction amplitude $A_{\text{ind}}$ on the Training partition under realistic sensor noise ($\pm 1.5\text{ nT}$):
  - **Recovered Amplitude:** $\hat{A}_{\text{ind}} = 0.981$ (Theoretical Ground Truth: $0.980$, Relative Error: $0.08\%$).
  - **Ocean Boundary:** $R_{\text{ocean}} = 1530.0\text{ km}$.
  - **Untouched Final Test Fit:** $R^2 = 0.9990$ on held-out orbital radius/time samples.
- **Multi-Seed Robustness & Explicit Uncertainty Decomposition:**
  Across 20 independent environmental noise seeds and flight trajectories:
  - **Sample Mean:** $\bar{A}_{\text{ind}} = 0.9798$
  - **Sample Standard Deviation:** $s_A = 0.0017$ ($0.17\%$ variation across trajectories)
  - **Standard Error of the Mean:** $\text{SE} = \frac{s_A}{\sqrt{20}} = 0.00039$
  - **95% Confidence Interval for $\mu_A$:** $[0.9790, 0.9806]$ (half-width $\pm 0.0008$, $t_{0.975, 19} = 2.093$)
  - **99% Confidence Interval for $\mu_A$:** $[0.9787, 0.9809]$ (half-width $\pm 0.0011$, $t_{0.995, 19} = 2.861$)
  - **Single-Trial Least-Squares Parameter Standard Error:**
    $$\sigma(\hat{A}) = \sqrt{\frac{\sigma_{\epsilon}^2}{\sum_{i=1}^{N_{\text{tr}}} x_i^2}} \approx \sqrt{\frac{(1.5\text{ nT})^2}{101,400\text{ nT}^2}} \approx 0.0047$$
  *(Note: The previously reported $\pm 0.0008$ was the 95% Confidence Interval half-width $t_{0.975} \times \text{SE}$, not the sample standard deviation $s_A = 0.0017$).*

---

### 3.3 Multi-Condition Anomaly Stress Benchmark & Proportional Safety

#### 1. In-Flight Single-Orbit Confusion Matrix
Evaluated purely on analog physical telemetry without simulator truth flags:
- Actual Injected Anomalies: 31 steps | Actual Nominal: 69 steps
- True Positives ($TP$): 31 | False Positives ($FP$): 0
- True Negatives ($TN$): 69 | False Negatives ($FN$): 0
- **Recall:** $100.0\%$ | **Precision:** $100.0\%$ | **$F_1$ Score:** $1.000$ | **Detection Lag:** $0.0$ steps.

#### 2. Multi-Condition Anomaly Stress Benchmark Suite (5 Scenarios)
To test whether performance generalizes beyond easily separable anomalies, the detector is evaluated against 5 diverse stress conditions:

| ID | Scenario Description | TP | FP | TN | FN | Lag | Recall | Precision | $F_1$ Score | Characteristic |
|---|---|---|---|---|---|---|---|---|---|---|
| 01 | Subtle Gradual Thermal Drift ($+0.12^\circ\text{C/step}$) | 28 | 0 | 15 | 7 | 7 | 80.0% | 100.0% | 0.889 | Subtle drift exhibits 7-step lag before $48.0^\circ\text{C}$ threshold crossing |
| 02 | Compound Dual Fault (Mag Surge + Optical Dropout) | 18 | 0 | 32 | 0 | 0 | 100.0% | 100.0% | 1.000 | Multi-sensor fusion isolates simultaneous optical & magnetic compound event |
| 03 | Intermittent Chattering Sensor Dropout | 12 | 1 | 35 | 2 | 1 | 85.7% | 92.3% | 0.889 | High-frequency boundary chattering produces occasional borderline dropouts |
| 04 | Heavy-Tailed Jovian Field Turbulence (Nominal) | 0 | 1 | 49 | 0 | 0 | N/A | 0.0% | 0.000 | 2.0% false alarm rate (FPR 1/50, 98% specificity, 0 actual faults) |
| 05 | Abrupt Momentum Step Surge (Valve Leak) | 15 | 0 | 35 | 0 | 0 | 100.0% | 100.0% | 1.000 | Abrupt step momentum surge flagged on first step with zero detection latency |

- **Scenario 4 Note:** In Scenario 4, there are 0 actual positive anomaly cases ($TP + FN = 0$). Mathematically, recall $\frac{TP}{TP + FN} = \frac{0}{0}$ is undefined; it is reported as `N/A`, with $2.0\%$ false alarm rate ($1/50$) and $98.0\%$ specificity.

#### 3. Expanded 25-Vector Adversarial Hard Safety Governor Audit
Safety claims are evaluated across 25 comprehensive boundary, corrupted, and multi-fault vectors:

```
                           Command Actually Unsafe       Command Actually Safe
Governor Rejected (Veto)     True Rejections: 17/17        False Vetoes: 0/8
Governor Approved (Pass)     Hardware Breaches: 0/17       True Approvals: 8/8
```

- **Total Test Vectors Evaluated:** 25 vectors across 6 risk categories
- **Unsafe Correctly Rejected:** 17 / 17 (Oracle: `[PASS: BLOCKED]`)
- **Safe Correctly Approved:** 8 / 8 (Oracle: `[PASS: ALLOWED]` across legal margins)
- **Hardware Breaches Recorded:** 0 breaches across tested envelope boundaries
- **Overall Arbitration Accuracy:** 100.0% accuracy on comprehensive 25-vector test suite
- **Fail-Closed Reliability:** 100.0% on corrupted, unphysical, or stale telemetry ($SoC = -10.0\%$, $SoC = 108.0\%$, $T = -60^\circ\text{C}$, $T = +120^\circ\text{C}$)
- **Evaluated Categories:**
  1. Solar Boresight Boundary (0.0 deg, 24.0 deg, 34.9 deg, 35.0 deg blocked; 35.1 deg, 90.0 deg, 180.0 deg allowed)
  2. Battery Reserve Floor (15.0% SoC, 31.8% SoC, 31.99% SoC blocked for 45W; 32.0% SoC allowed for 16W; 32.5% SoC, 90.0% SoC allowed for 45W)
  3. Electrical Bus Overcurrent Ceiling (95W peak draw > 75W ceiling blocked)
  4. Thermal Envelope Ceiling (+58.0 C, +52.0 C blocked; +51.8 C allowed)
  5. Reaction Wheel Momentum Ceiling (13.5 Nms, 11.0 Nms blocked; 10.9 Nms allowed)
  6. Compound Multi-Fault Combinations (+56.0 C heat + 20% SoC + 18 deg Sun hazard simultaneously blocked)

---

### 3.4 Matched Monte Carlo Repeated Experiments ($N = 20$)

#### Methodological Verification: Independent Environment Generation & Withheld Labels
1. **Precomputed Timeline:** For each seed ($1001 \dots 1020$), the complete sequence of physical states, field values, sensor noise realizations, and transient anomalies is generated into an immutable timeline **before either policy runs**.
2. **Blinded Policy Execution:** During execution, both policies (Adaptive Exploration and Balanced Baseline) receive an observation containing only noisy telemetry readings. **Ground truth anomaly labels and future states are strictly withheld from the policies.**
3. **Objective Evaluation Harness:** Science yield is evaluated externally by the scoring harness using the identical objective formula applied to both policies.
4. **Matched Operational Budgets:** Both approaches operate under identical constraints:
   - Energy Storage: $250\text{ W}\cdot\text{h}$ battery, initial charge $95\%$, minimum survival floor $30\%$.
   - Solar Generation: $88\text{ W}$ in sunlight, $0\text{ W}$ in eclipse.
   - Base Bus Draw: $22\text{ W}$.
   - Instrument Draws: `SPECTRAL_SURVEY` = $48\text{ W}$, `MAG_SOUNDING` = $16\text{ W}$, `IDLE_MONITOR` = $4\text{ W}$.

#### Definition of Discovery Yield Unit ($Y$)
Integrated science return accumulated over an orbit:
$$Y = \sum_{t=1}^{100} \Delta Y_t$$
where:
- **Multi-Spectral Survey (`SPECTRAL_SURVEY`, 48W):**
  $$\Delta Y_t = 2.10 \times \left(\frac{\text{SNR}_t}{40.0}\right) \times \beta_t$$
  where $\beta_t = 2.45$ if actively observing an in-progress physical plasma flare, and $\beta_t = 1.05$ if surveying nominal planetary surface background.
- **Magnetometer Sounding (`MAG_SOUNDING`, 16W):**
  $$\Delta Y_t = 0.90 + 0.35 \times \left(\frac{\text{SNR}_t}{40.0}\right)$$
- **Housekeeping / Standby (`IDLE_MONITOR`, 4W):**
  $$\Delta Y_t = 0.18$$

#### Trial-by-Trial Audit Table ($N = 20$ Matched Trials)
```
Trial | Int Seed | Adaptive Yield | Baseline Yield | Paired Diff | Advantage (%)
------+----------+----------------+----------------+-------------+--------------
01    | 1001     | 183.551        | 104.547        | +79.004     | +75.57%
02    | 1002     | 187.208        | 105.738        | +81.470     | +77.05%
03    | 1003     | 170.650        | 101.086        | +69.564     | +68.82%
04    | 1004     | 169.765        | 104.286        | +65.480     | +62.79%
05    | 1005     | 172.129        | 105.831        | +66.297     | +62.64%
06    | 1006     | 171.542        | 103.423        | +68.119     | +65.86%
07    | 1007     | 176.654        | 104.448        | +72.206     | +69.13%
08    | 1008     | 178.703        | 105.322        | +73.381     | +69.67%
09    | 1009     | 184.168        | 103.948        | +80.219     | +77.17%
10    | 1010     | 164.856        | 104.315        | +60.541     | +58.04%
11    | 1011     | 165.183        | 106.321        | +58.862     | +55.36%
12    | 1012     | 175.601        | 103.876        | +71.725     | +69.05%
13    | 1013     | 180.094        | 100.946        | +79.148     | +78.41%
14    | 1014     | 173.214        | 105.561        | +67.653     | +64.09%
15    | 1015     | 172.126        | 104.310        | +67.816     | +65.01%
16    | 1016     | 186.834        | 103.391        | +83.443     | +80.71%
17    | 1017     | 172.945        | 106.377        | +66.568     | +62.58%
18    | 1018     | 161.403        | 100.483        | +60.920     | +60.63%
19    | 1019     | 172.176        | 100.972        | +71.205     | +70.52%
20    | 1020     | 180.776        | 105.972        | +74.804     | +70.59%
```

#### Statistical Estimation & Multi-Baseline Evaluation
- **Adaptive Exploration Yield:** $\text{Mean} = 174.979$ ($\text{Sample SD} = 7.243$, $\text{SE} = 1.620$)
- **Baseline 1 (Fixed-Cadence Scheduled Survey):** $\text{Mean} = 104.058$ ($\text{Sample SD} = 1.868$, $\text{SE} = 0.418$)
- **Baseline 2 (Uniform Conservative Sounding):** $\text{Mean} = 71.531$ ($\text{Sample SD} = 0.019$, $\text{SE} = 0.004$)

##### Hypothesis Test 1: Adaptive vs Baseline 1 (Fixed Cadence)
- **Metric Tested:** $\Delta Y_1 = Y_{\text{adaptive}} - Y_{\text{fixed}}$ (evaluating $H_0: \mu_{\Delta Y_1} = 0$)
- **Sample Paired Yield Difference:** $\text{Mean} = +70.921\text{ units}$ ($\text{Sample SD} = 7.124$, $\text{SE} = 1.593$)
- **99% Student's $t$ Confidence Interval on Raw Difference:** $[+66.36, +75.48]\text{ units}$ ($df = 19$, $t_{\text{crit}} = 2.861$)
- **Paired Two-Tailed Student's $t$-Statistic:** $t = 44.52$ ($df = 19$, $p < 10^{-12}$, highly statistically significant)
- **Derived Relative Percentage Advantage:** $+68.184\%$ ($\text{Sample SD} = 7.028\text{ pp}$, $\text{SE} = 1.572\text{ pp}$, 99% CI: $[+63.69\%, +72.68\%]$)

##### Hypothesis Test 2: Adaptive vs Baseline 2 (Conservative Sounding)
- **Metric Tested:** $\Delta Y_2 = Y_{\text{adaptive}} - Y_{\text{conservative}}$ (evaluating $H_0: \mu_{\Delta Y_2} = 0$)
- **Sample Paired Yield Difference:** $\text{Mean} = +103.448\text{ units}$ ($\text{Sample SD} = 7.243$, $\text{SE} = 1.620$)
- **99% Student's $t$ Confidence Interval on Raw Difference:** $[+98.81, +108.09]\text{ units}$ ($df = 19$, $t_{\text{crit}} = 2.861$)
- **Paired Two-Tailed Student's $t$-Statistic:** $t = 63.81$ ($df = 19$, $p < 10^{-12}$, highly statistically significant)
- **Derived Relative Percentage Advantage:** $+144.62\%$ relative advantage over uniform conservative sounding

##### Methodological Controls & Explanation of Sample Standard Deviations ($0.019$ vs $7.243$)
- **Identical Environmental Timelines:** For each trial, the environmental timeline (magnetic field, plasma density, solar flux, spectrometer SNR, flare timing, and sensor noise) is precomputed **before either policy runs**. All three policies experience the exact same underlying physical events.
- **Strict Information Barrier:** Ground truth anomaly labels (`is_plasma_flare`) and future states are withheld from all policies. Policies decide actions strictly using observable analog sensor telemetry (measured altitude, measured magnetic field, spectrometer SNR, battery SoC, wheel momentum). The ground truth labels are accessed solely by the external benchmark scoring engine.
- **Why Conservative Baseline SD is $0.019$:** The conservative policy deterministically alternates between `MAG_SOUNDING` (16W, reward $0.90 + 0.35 \times (\text{SNR}/40) \approx 1.25 \pm 0.01$) and `IDLE_MONITOR` (4W, reward $0.18$). Because it never commands high-power surveys, it never captures flare bonus multipliers ($2.45\times$). Its yield variance across seeds is driven purely by minor spectrometer Gaussian sensor noise ($\pm 0.01$ on SNR), resulting in a microscopic standard deviation of $0.019$.
- **Why Adaptive Policy SD is $7.243$:** The adaptive agent actively detects transient plasma surges from observable SNR rises and dynamically bursts high-power surveys during flare windows. Because flare onset step (steps 28-34) and flare duration (8-12 steps) vary stochastically across seeds, the number of captured flare-boosted survey steps varies per trial, naturally generating a standard deviation of $7.243$.
- **Narrow Conclusion & Operational Scope:** The conclusion remains strictly narrow: adaptive exploration outperformed both the fixed-cadence and conservative baselines under this particular simulated reward function. The result does not yet establish that an operational spacecraft would acquire 68% more scientifically valuable data across all real flight missions without domain payload tuning. Stronger operational evidence requires multi-trajectory sweeps, noise sensitivity trials, physical parameter sweeps, and testing against flight instrument telemetry.

---

### 3.5 Europa Environmental Realism & Parametric Radiation Scenario

- **Jovian Trapped Radiation Reference:** GIRE2 (Galileo Interim Radiation Electron model) at Europa ($9.38\text{ }R_J$).
- **Exterior Unshielded Dose Rate:** $\sim 0.150\text{ krad / hour}$ ($\approx 3.60\text{ krad / Earth day}$).
- **Spacecraft Armored Vault Geometry:** Modeled after **NASA Europa Clipper's 9.2 mm (360-mil) aluminum-zinc alloy vault** (Al 7075-T7351).
- **Assumed Shielding Attenuation Factor:** $\sim 36\times$ parametric attenuation factor for trapped Jovian electron/bremsstrahlung spectrum.
- **Vault Interior Dose Rate:** $\sim 0.00417\text{ krad / hour}$ ($\approx 0.100\text{ krad / Earth day}$).
- **Single-Orbit Trajectory Total Dose:** Exterior $= 0.368\text{ krad}$ | Shielded Vault $= 0.0102\text{ krad}$ ($10.2\text{ rad}$).
- **Radiation Hardening Baseline:** $150.0\text{ krad(Si)}$ Total Ionizing Dose (TID) benchmark (Europa Clipper electronics vault design baseline).
- **Particle-Transport Calculation Qualification:** NASA confirms that Europa Clipper's electronics vault uses aluminum-zinc alloy walls up to $9.2\text{ mm}$ thick. However, that specification alone does not establish an assumed 36-fold attenuation. The interior vault dose is reported as a scenario estimate based on an assumed 36-fold shielding factor—not as a NASA-validated particle-transport (e.g. GEANT4 or SHIELDOSE2) prediction.
- **Assumed Rate vs Trajectory-Integrated Dose Note:** Instantaneous Jovian belt rates ($\sim 150\text{ rad/h}$) are distinguished from trajectory-integrated mission totals. Europa Clipper avoids orbital capture around Europa, flying high-eccentricity Jupiter orbits with brief flybys and an armored vault to survive a multi-year mission.

---

## 4. Automated Audit Artifacts

The benchmark exports all underlying raw data upon execution to enable independent verification:
- [`trajectory_audit.csv`](file:///c:/Users/Moon/Desktop/Projects/IRIS/projects/mission_intelligence/trajectory_audit.csv): 101 trajectory checkpoints comparing numerical midpoint radius/velocity against the independent analytical Kepler conic reference, with position differences in mm, velocity differences in mm/s, and energy drift in ppm.
- [`monte_carlo_trials.csv`](file:///c:/Users/Moon/Desktop/Projects/IRIS/projects/mission_intelligence/monte_carlo_trials.csv): All 20 trials with exact integer seeds, adaptive yields, baseline yields, paired differences, and percentage advantages.
- [`safety_audit.csv`](file:///c:/Users/Moon/Desktop/Projects/IRIS/projects/mission_intelligence/safety_audit.csv): 25 adversarial vectors across 6 risk categories with explicit safe/unsafe tags, test oracle expectations, and arbitrated verdicts.
- [`downlink_telemetry.json`](file:///c:/Users/Moon/Desktop/Projects/IRIS/projects/mission_intelligence/downlink_telemetry.json): Structured JSON payload exporting closure residuals, reference comparisons, 9.2mm vault dose, winning induction model ($H_5$), recovered amplitude ($A_{\text{ind}} = 0.981$), dual CIs, yield formula, and budget parity statements.

---

## 5. Execution & Verification

Run the verified benchmark using the native IRIS compiler:
```bash
cargo run --release -- run projects/mission_intelligence/main.iris
```
All assertions pass and exit with status code 0.
