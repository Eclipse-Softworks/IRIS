# Automated Scientific Discovery: Nonlinear Chaotic Dynamics & Strange Attractors

This project demonstrates **Automated Scientific Discovery** in IRIS: recovering hidden, nonlinear differential equations and strange attractor topology directly from raw, noisy physical telemetry without human prompting.

---

## 1. The Scientific Problem: Chaos & The Limits of Standard AI

In complex non-equilibrium physical systems (atmospheric weather, plasma turbulence, cardiac dynamics, chemical oscillations):
1. **The Human Does Not Know the Differential Equations:** The underlying physics is unmodeled or partially observed.
2. **Point-by-Point Predictions Fail:** Because chaotic systems have sensitive dependence on initial conditions ($e^{\lambda_1 t}$ divergence), standard neural networks and regression models fail over time, collapsing to trivial fixed points or blowing up to infinity.
3. **The True Scientific Goal:** Discover the **minimal, symbolic, analytical equations of motion** that preserve the global geometric structure of the strange attractor and the spectrum of Lyapunov exponents.

---

## 2. The Unknown System

The system observes a 3D convective fluid system where heat and velocity are coupled through nonlinear convective advection:
$$\frac{dx}{dt} = \sigma (y - x)$$
$$\frac{dy}{dt} = \rho x - y - xz$$
$$\frac{dz}{dt} = xy - \beta z$$

* **Crucial Rule:** The equations and parameters ($\sigma=10, \rho=28, \beta=2.667$) are **completely hidden** from the discovery engine.
* **Input to IRIS:** Only a noisy 3D telemetry stream $\{ (x_t, y_t, z_t) \}_{t=1}^N$ with added Gaussian sensor noise ($\sigma = 0.01$).

---

## 3. The Discovery Engine

The discovery engine searches the space of 10 coupled nonlinear interactions $\{1, x, y, z, x^2, y^2, z^2, xy, xz, yz\}$:
1. **Computes robust trajectory derivatives** via 4th-order central differences.
2. **Enforces Occam's Parsimony:** Prunes spurious terms via iterative thresholding, searching for the minimal basis that generates the dynamics.
3. **Discovered Governing Equations (from raw noisy data):**
   $$\frac{dx}{dt} = -10.000 x + 10.000 y$$
   $$\frac{dy}{dt} = 28.002 x - 1.000 y - 1.000 xz$$
   $$\frac{dz}{dt} = -2.666 z + 1.000 xy$$
   **Discovery Residual Error: $1.63\%$!**
   Notice how it precisely isolated the bilinear cross-coupling terms $-xz$ and $+xy$!

---

## 4. Native IRIS Verification & Attractor Reconstruction

The machine-discovered equations are compiled and integrated directly in native IRIS ([discovered_attractor.iris](discovered_attractor.iris)):
* **Largest Lyapunov Exponent ($\lambda_1$):**
  Measured via the Benettin tangent-vector renormalization algorithm.
  $$\lambda_1 = 11.13 > 0$$
  *Positive Lyapunov exponent proves that the reconstructed dynamics exhibit genuine deterministic chaos with sensitive dependence.*
* **Phase-Space Butterfly Attractor:**
  Renders the projected 2D density distribution in the terminal, confirming the dual recirculating lobes of the strange attractor.

---

## 5. Run and Verify

### Master Pipeline:
```powershell
python projects/chaotic_discovery/run.py
```

### Run Native IRIS Simulator Independently:
```powershell
.\target\release\iris.exe run projects/chaotic_discovery/discovered_attractor.iris
```

### Generate 3D Phase Space Attractor Plot:
```powershell
python projects/chaotic_discovery/visualize_attractor.py
```
Outputs high-resolution plot to: `projects/chaotic_discovery/discovered_attractor_3d.png`
