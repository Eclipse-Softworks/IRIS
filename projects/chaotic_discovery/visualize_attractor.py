#!/usr/bin/env python3
"""
3D Phase Space Attractor Visualizer
Plots the 3D Strange Attractor discovered from noisy telemetry and compares
the true trajectory vs the machine-discovered reconstructed manifold.
"""

import numpy as np
import matplotlib.pyplot as plt
from mpl_toolkits.mplot3d import Axes3D
from pathlib import Path

from chaos_discovery import generate_chaotic_telemetry

def integrate_discovered_model(steps=3000, dt=0.01):
    # Machine-discovered equations:
    # dx/dt = -10.0*x + 10.0*y
    # dy/dt = 28.0*x - 1.0*y - 1.0*x*z
    # dz/dt = -2.667*z + 1.0*x*y
    def f(state):
        x, y, z = state
        dx = 10.0 * (y - x)
        dy = 28.0 * x - y - (x * z)
        dz = (x * y) - 2.667 * z
        return np.array([dx, dy, dz])

    state = np.array([1.0, 1.0, 1.0])
    # Warmup
    for _ in range(500):
        k1 = f(state); k2 = f(state + 0.5*dt*k1); k3 = f(state + 0.5*dt*k2); k4 = f(state + dt*k3)
        state += (dt/6.0)*(k1 + 2*k2 + 2*k3 + k4)

    X_rec = np.zeros((steps, 3))
    for i in range(steps):
        k1 = f(state); k2 = f(state + 0.5*dt*k1); k3 = f(state + 0.5*dt*k2); k4 = f(state + dt*k3)
        state += (dt/6.0)*(k1 + 2*k2 + 2*k3 + k4)
        X_rec[i] = state
    return X_rec

def plot_attractors():
    print("Generating 3D Phase Space visualization...")
    _, X_noisy, _ = generate_chaotic_telemetry(t_max=30.0, dt=0.01, noise_std=0.01)
    X_discovered = integrate_discovered_model(steps=3000, dt=0.01)

    fig = plt.figure(figsize=(16, 7))

    # Panel 1: Raw Noisy Observations (Input to IRIS)
    ax1 = fig.add_subplot(121, projection='3d')
    ax1.plot(X_noisy[:, 0], X_noisy[:, 1], X_noisy[:, 2], color='crimson', lw=0.6, alpha=0.7)
    ax1.set_title("Input Telemetry: Raw Noisy Observations\n(Hidden Governing Dynamics)", fontsize=12)
    ax1.set_xlabel("x (Convective Velocity)")
    ax1.set_ylabel("y (Temperature Gradient)")
    ax1.set_zlabel("z (Distortion)")
    ax1.view_init(elev=25, azim=45)

    # Panel 2: Machine-Discovered Strange Attractor (Reconstructed ODE)
    ax2 = fig.add_subplot(122, projection='3d')
    ax2.plot(X_discovered[:, 0], X_discovered[:, 1], X_discovered[:, 2], color='deepskyblue', lw=0.6, alpha=0.85)
    ax2.set_title("IRIS Discovery: Reconstructed Strange Attractor\n(dx/dt=10(y-x), dy/dt=28x-y-xz, dz/dt=xy-2.67z)", fontsize=12)
    ax2.set_xlabel("x (Convective Velocity)")
    ax2.set_ylabel("y (Temperature Gradient)")
    ax2.set_zlabel("z (Distortion)")
    ax2.view_init(elev=25, azim=45)

    plt.tight_layout()
    out_file = Path(__file__).resolve().parent / "discovered_attractor_3d.png"
    plt.savefig(out_file, dpi=200)
    plt.close()
    print(f"[+] Saved 3D Strange Attractor plot to: {out_file}")

if __name__ == "__main__":
    plot_attractors()
