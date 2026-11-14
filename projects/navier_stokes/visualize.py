#!/usr/bin/env python3
"""
Navier-Stokes Cavity Flow Visualizer
Generates high-resolution streamline and pressure contour visualizations
for the 2D incompressible Navier-Stokes cavity flow solved by IRIS.
"""

import numpy as np
import matplotlib.pyplot as plt
from pathlib import Path

def solve_navier_stokes_cavity(nx=41, ny=41, steps=300, dt=0.001, nu=0.05, rho=1.0, u_lid=1.0):
    dx = 1.0 / (nx - 1)
    dy = 1.0 / (ny - 1)

    u = np.zeros((ny, nx))
    v = np.zeros((ny, nx))
    p = np.zeros((ny, nx))
    b = np.zeros((ny, nx))

    # Initial condition: moving top lid
    u[-1, :] = u_lid

    dx2 = dx * dx
    dy2 = dy * dy

    for step in range(steps):
        # 1. Intermediate velocity step (Advection-Diffusion)
        u_star = u.copy()
        v_star = v.copy()

        # Interior spatial derivatives
        du_dx = (u[1:-1, 2:] - u[1:-1, :-2]) / (2.0 * dx)
        du_dy = (u[2:, 1:-1] - u[:-2, 1:-1]) / (2.0 * dy)
        lap_u = (u[1:-1, 2:] - 2*u[1:-1, 1:-1] + u[1:-1, :-2]) / dx2 + \
                (u[2:, 1:-1] - 2*u[1:-1, 1:-1] + u[:-2, 1:-1]) / dy2

        u_star[1:-1, 1:-1] = u[1:-1, 1:-1] + dt * (
            -(u[1:-1, 1:-1] * du_dx + v[1:-1, 1:-1] * du_dy) + nu * lap_u
        )

        dv_dx = (v[1:-1, 2:] - v[1:-1, :-2]) / (2.0 * dx)
        dv_dy = (v[2:, 1:-1] - v[:-2, 1:-1]) / (2.0 * dy)
        lap_v = (v[1:-1, 2:] - 2*v[1:-1, 1:-1] + v[1:-1, :-2]) / dx2 + \
                (v[2:, 1:-1] - 2*v[1:-1, 1:-1] + v[:-2, 1:-1]) / dy2

        v_star[1:-1, 1:-1] = v[1:-1, 1:-1] + dt * (
            -(u[1:-1, 1:-1] * dv_dx + v[1:-1, 1:-1] * dv_dy) + nu * lap_v
        )

        # Boundary conditions on intermediate velocity
        u_star[0, :] = 0.0; u_star[-1, :] = u_lid
        u_star[:, 0] = 0.0; u_star[:, -1] = 0.0
        v_star[0, :] = 0.0; v_star[-1, :] = 0.0
        v_star[:, 0] = 0.0; v_star[:, -1] = 0.0

        # 2. Pressure Poisson source term
        div_star = (u_star[1:-1, 2:] - u_star[1:-1, :-2]) / (2.0 * dx) + \
                   (v_star[2:, 1:-1] - v_star[:-2, 1:-1]) / (2.0 * dy)
        b[1:-1, 1:-1] = (rho / dt) * div_star

        # 3. Pressure Poisson equation relaxation
        denom = 2.0 * (dx2 + dy2)
        for _ in range(30):
            p_old = p.copy()
            p[1:-1, 1:-1] = (
                (p_old[1:-1, 2:] + p_old[1:-1, :-2]) * dy2 +
                (p_old[2:, 1:-1] + p_old[:-2, 1:-1]) * dx2 -
                b[1:-1, 1:-1] * dx2 * dy2
            ) / denom

            # Neumann boundary conditions
            p[:, 0] = p[:, 1]
            p[:, -1] = p[:, -2]
            p[0, :] = p[1, :]
            p[-1, :] = 0.0

        # 4. Projection step
        dp_dx = (p[1:-1, 2:] - p[1:-1, :-2]) / (2.0 * dx)
        dp_dy = (p[2:, 1:-1] - p[:-2, 1:-1]) / (2.0 * dy)

        u[1:-1, 1:-1] = u_star[1:-1, 1:-1] - (dt / rho) * dp_dx
        v[1:-1, 1:-1] = v_star[1:-1, 1:-1] - (dt / rho) * dp_dy

        # Re-apply boundary conditions
        u[0, :] = 0.0; u[-1, :] = u_lid
        u[:, 0] = 0.0; u[:, -1] = 0.0
        v[0, :] = 0.0; v[-1, :] = 0.0
        v[:, 0] = 0.0; v[:, -1] = 0.0

    return u, v, p, dx, dy

def plot_solution():
    print("Computing high-resolution solution for visualization...")
    u, v, p, dx, dy = solve_navier_stokes_cavity(nx=41, ny=41, steps=250, dt=0.001, nu=0.05)

    ny, nx = u.shape
    x = np.linspace(0, 1, nx)
    y = np.linspace(0, 1, ny)
    X, Y = np.meshgrid(x, y)

    speed = np.sqrt(u**2 + v**2)

    fig, axes = plt.subplots(1, 2, figsize=(14, 6))

    # 1. Streamline plot
    ax1 = axes[0]
    strm = ax1.streamplot(X, Y, u, v, color=speed, cmap='plasma', density=1.5, linewidth=1.2, arrowsize=1.2)
    fig.colorbar(strm.lines, ax=ax1, label='Velocity Magnitude |u|')
    ax1.set_title("2D Navier-Stokes Cavity Flow: Streamlines & Primary Vortex", fontsize=12)
    ax1.set_xlabel("x (Normalized Width)")
    ax1.set_ylabel("y (Normalized Height)")
    ax1.set_xlim(0, 1)
    ax1.set_ylim(0, 1)

    # 2. Pressure contours
    ax2 = axes[1]
    cp = ax2.contourf(X, Y, p, levels=25, cmap='viridis')
    fig.colorbar(cp, ax=ax2, label='Pressure p')
    ax2.quiver(X[::2, ::2], Y[::2, ::2], u[::2, ::2], v[::2, ::2], color='white', scale=15, alpha=0.7)
    ax2.set_title("Pressure Field & Velocity Vector Grid", fontsize=12)
    ax2.set_xlabel("x (Normalized Width)")
    ax2.set_ylabel("y (Normalized Height)")

    plt.tight_layout()
    output_path = Path(__file__).resolve().parent / "cavity_flow_streamlines.png"
    plt.savefig(output_path, dpi=200)
    plt.close()
    print(f"[+] Saved high-resolution CFD plot to: {output_path}")

if __name__ == "__main__":
    plot_solution()
