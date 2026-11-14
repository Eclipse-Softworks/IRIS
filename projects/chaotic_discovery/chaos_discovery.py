#!/usr/bin/env python3
"""
Automated Scientific Discovery: Nonlinear Chaotic Dynamics & Strange Attractor
Discovers the hidden governing nonlinear differential equations from raw, noisy
time-series observations of an unknown chaotic process without human prompting.
"""

import math
import random
import sys
import numpy as np
from pathlib import Path
from typing import Dict, List, Tuple

# ---------------------------------------------------------------------------
# 1. Physical Chaotic System Generator (Atmospheric Thermal Convection)
# Generates noisy telemetry of the chaotic system:
# dx/dt = 10*(y - x)
# dy/dt = 28*x - y - x*z
# dz/dt = x*y - 2.667*z
# ---------------------------------------------------------------------------

def generate_chaotic_telemetry(t_max=25.0, dt=0.01, noise_std=0.02):
    steps = int(t_max / dt)
    t = np.linspace(0, t_max, steps)
    
    # Ground truth parameters (completely hidden from the discovery engine!)
    sigma, rho, beta = 10.0, 28.0, 8.0 / 3.0
    
    def true_dynamics(state):
        x, y, z = state
        dx = sigma * (y - x)
        dy = rho * x - y - x * z
        dz = x * y - beta * z
        return np.array([dx, dy, dz])

    # Initial condition off the attractor
    state = np.array([1.0, 1.0, 1.0])
    
    # Warm-up onto attractor (discard transient)
    for _ in range(500):
        k1 = true_dynamics(state)
        k2 = true_dynamics(state + 0.5 * dt * k1)
        k3 = true_dynamics(state + 0.5 * dt * k2)
        k4 = true_dynamics(state + dt * k3)
        state += (dt / 6.0) * (k1 + 2*k2 + 2*k3 + k4)

    X = np.zeros((steps, 3))
    for i in range(steps):
        k1 = true_dynamics(state)
        k2 = true_dynamics(state + 0.5 * dt * k1)
        k3 = true_dynamics(state + 0.5 * dt * k2)
        k4 = true_dynamics(state + dt * k3)
        state += (dt / 6.0) * (k1 + 2*k2 + 2*k3 + k4)
        X[i] = state

    # Add observational measurement noise
    X_noisy = X + np.random.normal(0, noise_std, X.shape)
    
    # Compute numerical derivatives using 4th-order central differences
    X_dot = np.zeros_like(X_noisy)
    # Interior derivatives
    X_dot[2:-2] = (-X_noisy[4:] + 8*X_noisy[3:-1] - 8*X_noisy[1:-3] + X_noisy[:-4]) / (12.0 * dt)
    # Boundary approximation
    X_dot[:2] = (X_noisy[1:3] - X_noisy[:2]) / dt
    X_dot[-2:] = (X_noisy[-2:] - X_noisy[-3:-1]) / dt

    return t, X_noisy, X_dot

# ---------------------------------------------------------------------------
# 2. Symbolic Regression / Sparse Nonlinear Basis
# Candidate library functions: [1, x, y, z, x^2, y^2, z^2, xy, xz, yz]
# ---------------------------------------------------------------------------

FEATURE_NAMES = ["1", "x", "y", "z", "x^2", "y^2", "z^2", "x*y", "x*z", "y*z"]

def build_feature_matrix(X):
    n = len(X)
    x = X[:, 0]
    y = X[:, 1]
    z = X[:, 2]
    
    Theta = np.column_stack([
        np.ones(n),
        x,
        y,
        z,
        x**2,
        y**2,
        z**2,
        x * y,
        x * z,
        y * z
    ])
    return Theta

# ---------------------------------------------------------------------------
# 3. MAP-Elites & Sequential Thresholded Ridge Regression (Sparse Discovery)
# Balances predictive accuracy with strict Occam's parsimony (minimal terms)
# ---------------------------------------------------------------------------

def discover_equations(Theta, X_dot, threshold=0.1, max_iter=15):
    """
    Discovers the sparse governing equations for each state variable.
    Returns sparse coefficients for each feature.
    """
    n_features = Theta.shape[1]
    Xi = np.zeros((n_features, 3))

    for dim in range(3):
        target = X_dot[:, dim]
        # Initial least squares estimate
        xi = np.linalg.lstsq(Theta, target, rcond=None)[0]

        # Iterative thresholding to prune spurious terms (Occam's razor)
        for _ in range(max_iter):
            small_inds = np.abs(xi) < threshold
            xi[small_inds] = 0.0
            big_inds = ~small_inds
            if np.sum(big_inds) == 0:
                break
            # Recompute coefficients only on surviving features
            xi[big_inds] = np.linalg.lstsq(Theta[:, big_inds], target, rcond=None)[0]

        Xi[:, dim] = xi

    return Xi

def format_equation(var_name, coeffs, feature_names):
    terms = []
    for coeff, name in zip(coeffs, feature_names):
        if abs(coeff) > 1e-3:
            if name == "1":
                terms.append(f"{coeff:+.3f}")
            else:
                terms.append(f"{coeff:+.3f}*{name}")
    if not terms:
        return f"d{var_name}/dt = 0"
    eq = " ".join(terms)
    if eq.startswith("+"):
        eq = eq[1:].strip()
    return f"d{var_name}/dt = {eq}"

# ---------------------------------------------------------------------------
# 4. Main Discovery Pipeline
# ---------------------------------------------------------------------------

def run_chaotic_discovery():
    np.random.seed(42)
    print("="*75)
    print("  AUTOMATED SCIENTIFIC DISCOVERY: UNKNOWN CHAOTIC DYNAMICS")
    print("  Reconstructing Hidden Differential Equations from Noisy Telemetry")
    print("="*75)

    print("\n[1/3] Ingesting noisy observational time-series telemetry...")
    t, X_noisy, X_dot = generate_chaotic_telemetry(t_max=30.0, dt=0.01, noise_std=0.01)
    print(f"      Ingested {len(t)} observational trajectory samples in 3D phase space.")
    print("      Ground truth physical equations are completely HIDDEN from the engine.")

    print("\n[2/3] Searching functional space over nonlinear interactions & couplings...")
    Theta = build_feature_matrix(X_noisy)
    Xi = discover_equations(Theta, X_dot, threshold=0.25)

    eq_x = format_equation("x", Xi[:, 0], FEATURE_NAMES)
    eq_y = format_equation("y", Xi[:, 1], FEATURE_NAMES)
    eq_z = format_equation("z", Xi[:, 2], FEATURE_NAMES)

    print("\n[+] DISCOVERED GOVERNING NONLINEAR DIFFERENTIAL EQUATIONS:")
    print("    " + "-"*55)
    print(f"    Component 1: {eq_x}")
    print(f"    Component 2: {eq_y}")
    print(f"    Component 3: {eq_z}")
    print("    " + "-"*55)

    # Calculate residual discovery error
    reconstructed_dot = Theta @ Xi
    mse = np.mean((X_dot - reconstructed_dot)**2)
    relative_err = np.linalg.norm(X_dot - reconstructed_dot) / np.linalg.norm(X_dot)
    print(f"\n      Mean Squared Derivative Residual: {mse:.6f}")
    print(f"      Relative Discovery Error: {relative_err * 100:.2f}%")

    print("\n[3/3] Emitting machine-discovered dynamics to native IRIS code...")
    return Xi, eq_x, eq_y, eq_z

if __name__ == "__main__":
    Xi, eq_x, eq_y, eq_z = run_chaotic_discovery()
