# Incompressible Navier-Stokes Equations in IRIS

This project implements a complete, native Computational Fluid Dynamics (CFD) solver for the **2D Incompressible Navier-Stokes Equations** written directly in IRIS.

---

## 1. Mathematical Formulation

The motion of an incompressible Newtonian fluid is governed by the coupled momentum and continuity partial differential equations (PDEs):

### Momentum Conservation:
$$\frac{\partial \mathbf{u}}{\partial t} + (\mathbf{u} \cdot \nabla) \mathbf{u} = -\frac{1}{\rho}\nabla p + \nu \nabla^2 \mathbf{u} + \mathbf{f}$$

Component-wise in 2D Cartesian coordinates $(x, y)$:
$$\frac{\partial u}{\partial t} + u \frac{\partial u}{\partial x} + v \frac{\partial u}{\partial y} = -\frac{1}{\rho}\frac{\partial p}{\partial x} + \nu \left( \frac{\partial^2 u}{\partial x^2} + \frac{\partial^2 u}{\partial y^2} \right)$$

$$\frac{\partial v}{\partial t} + u \frac{\partial v}{\partial x} + v \frac{\partial v}{\partial y} = -\frac{1}{\rho}\frac{\partial p}{\partial y} + \nu \left( \frac{\partial^2 v}{\partial x^2} + \frac{\partial^2 v}{\partial y^2} \right)$$

### Mass Conservation (Continuity / Incompressibility):
$$\nabla \cdot \mathbf{u} = \frac{\partial u}{\partial x} + \frac{\partial v}{\partial y} = 0$$

Where:
* $\mathbf{u} = (u, v)$: Fluid velocity vector field
* $p$: Fluid pressure scalar field
* $\rho$: Fluid density ($\rho = 1.0$)
* $\nu$: Kinematic viscosity ($\nu = 0.05$, corresponding to Reynolds number $Re = 20$)

---

## 2. Numerical Method: Chorin's Projection Scheme

To decouple the velocity and pressure fields, the solver uses **Chorin's Projection Method (Fractional-Step Scheme)** on an Eulerian grid:

1. **Predictor Step (Advection & Diffusion):**  
   Computes an intermediate velocity $\mathbf{u}^* = (u^*, v^*)$ without pressure:
   $$\mathbf{u}^* = \mathbf{u}^n + \Delta t \left( -(\mathbf{u}^n \cdot \nabla)\mathbf{u}^n + \nu \nabla^2 \mathbf{u}^n \right)$$

2. **Divergence Calculation:**  
   Computes the divergence of the intermediate velocity field:
   $$b_{i,j} = \frac{\rho}{\Delta t} \nabla \cdot \mathbf{u}^* = \frac{\rho}{\Delta t} \left( \frac{u^*_{i+1,j} - u^*_{i-1,j}}{2\Delta x} + \frac{v^*_{i,j+1} - v^*_{i,j-1}}{2\Delta y} \right)$$

3. **Pressure Poisson Equation:**  
   Enforces the incompressibility constraint by solving for pressure:
   $$\nabla^2 p = b$$
   Solved iteratively via Jacobi relaxation with Neumann boundary conditions ($\frac{\partial p}{\partial n} = 0$).

4. **Projection (Correction) Step:**  
   Projects the intermediate velocity onto the divergence-free subspace:
   $$\mathbf{u}^{n+1} = \mathbf{u}^* - \frac{\Delta t}{\rho} \nabla p$$

---

## 3. Benchmark Problem: 2D Lid-Driven Cavity Flow

The benchmark models fluid trapped inside an enclosed square cavity $[0, 1] \times [0, 1]$:
* **Top Boundary ($y = 1$):** Moving solid lid with tangential velocity $u = U_{\text{lid}} = 1.0, v = 0$.
* **Left, Right, Bottom Walls:** Stationary no-slip solid walls ($u = 0, v = 0$).

As the top lid slides to the right, viscous shear drags adjacent fluid forward. When fluid hits the right wall, it is forced downwards, across the bottom floor to the left, and back up the left wall, setting up the iconic **recirculating primary central vortex** (the classic Ghia et al. 1982 benchmark).

---

## 4. Run the IRIS Native Solver

Compile and execute the native IRIS solver:

```powershell
.\target\release\iris.exe run projects/navier_stokes/cavity_flow.iris
```

### Terminal Output:
The program compiles natively with LLVM and produces an ASCII vector field showing the primary vortex circulation:

```
======================================================================
  FLOW FIELD VECTOR MAP (2D Recirculating Primary Vortex in Cavity)
  Symbols: [>] Right, [<] Left, [^] Up, [v] Down, [.] Stagnant Wall
======================================================================

 .  >  >  >  >  >  >  >  >  >  >  >  >  >  >  . 
 .  >  >  >  >  >  >  >  >  >  >  >  >  >  >  . 
 .  ^  ^  >  >  >  >  >  >  >  >  >  >  v  v  . 
 .  ^  ^  ^  ^  .  .  .  .  .  .  .  v  v  v  . 
 .  ^  ^  ^  ^  ^  .  .  .  .  .  v  v  v  v  . 
 .  ^  ^  ^  ^  <  <  <  <  <  <  v  v  v  v  . 
 .  .  ^  ^  ^  <  <  <  <  <  <  <  v  v  .  . 
 .  .  .  ^  ^  <  <  <  <  <  <  <  v  .  .  . 

Top-Center Velocity u: 0.7822 (Driven by lid to the right)
Bottom-Center Return Velocity u: -0.0331 (Recirculating to the left)
Right-Wall Downward Velocity v: -0.0690 (Downward vortex motion)
Left-Wall Upward Velocity v: 0.0745 (Upward vortex return)

[+] Navier-Stokes Incompressible Cavity Flow Successfully Solved!
[+] Mass Conservation div(u) = 0 and Recirculating Vortex Verified.
```

---

## 5. Visualizer

To generate a high-resolution 2D streamline and pressure contour plot:

```powershell
python projects/navier_stokes/visualize.py
```

Generated plot: `projects/navier_stokes/cavity_flow_streamlines.png`
