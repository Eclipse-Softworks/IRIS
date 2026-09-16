#!/usr/bin/env python3
"""IRIS-CRUNCH: High-Performance Computational Tool Suite.

Harnesses IRIS native LLVM 23 binaries to execute heavy scientific,
physical, and biological simulations with zero C++ compilation dependencies.
"""

import argparse
import os
import subprocess
import sys
import time

TOOLS_DIR = os.path.dirname(os.path.abspath(__file__))
PROJECT_ROOT = os.path.abspath(os.path.join(TOOLS_DIR, "..", ".."))

WORKLOADS = {
    "galaxy": {
        "name": "N-Body Gravitational Galaxy Collision (Astrophysics)",
        "exe": os.path.join(TOOLS_DIR, "galaxy_sim.exe"),
        "desc": "Simulates 300 celestial bodies (Andromeda & Milky Way cores) across 5.4M pairwise gravitational vector calculations."
    },
    "cfd": {
        "name": "Lattice Boltzmann Aerodynamic Wind Tunnel (Fluid Dynamics)",
        "exe": os.path.join(TOOLS_DIR, "wind_tunnel.exe"),
        "desc": "Solves 2D Navier-Stokes equations for airflow past a bluff cylinder obstacle with momentum bounce-back."
    },
    "epidemic": {
        "name": "Epidemic ICU Saturation & Triage Policy Sweep (Public Health)",
        "exe": os.path.join(PROJECT_ROOT, "examples", "human_problems", "epidemic_monte_carlo.exe"),
        "desc": "Sweeps 20,000 public health policy scenarios (2.4M simulated days) to prevent hospital collapse."
    },
    "genomics": {
        "name": "Cancer Oncogene Mutation Sequence Alignment (Bioinformatics)",
        "exe": os.path.join(PROJECT_ROOT, "examples", "human_problems", "genomic_alignment.exe"),
        "desc": "Aligns 100 patient tumor biopsy reads against reference oncogenes via 1.46M dynamic programming matrix cells."
    },
    "three_body": {
        "name": "Poincaré Three-Body Gravitational Chaos & Slingshot Engine",
        "exe": os.path.join(TOOLS_DIR, "three_body.exe"),
        "desc": "Solves 3-body gravitational chaos across 10,000 symplectic steps, tracking near-singularity close encounters and slingshot ejections."
    },
    "neural_lm": {
        "name": "Native Neural Conversational Language Model Trainer (Deep Learning)",
        "exe": os.path.join(PROJECT_ROOT, "tools", "neural_dialogue", "train_lm.exe"),
        "desc": "Trains a neural conversational model on dialogue data using Backpropagation, Softmax, and Cross-Entropy in native IRIS."
    },
    "superbug": {
        "name": "De Novo Antimicrobial Drug Discovery (MAP-Elites)",
        "script": os.path.join(PROJECT_ROOT, "examples", "human_problems", "discover_superbug_cures.py"),
        "desc": "Navigates 3.28 * 10^19 peptide sequence space to discover novel antimicrobial agents targeting antibiotic resistance."
    }
}


def run_workload(key: str):
    if key not in WORKLOADS:
        print(f"[-] Unknown workload '{key}'. Available: {list(WORKLOADS.keys())}")
        return

    info = WORKLOADS[key]
    print("=" * 80)
    print(f" LAUNCHING IRIS-CRUNCH WORKLOAD: {info['name']}")
    print(f" {info['desc']}")
    print("=" * 80)

    t0 = time.perf_counter()
    if "exe" in info:
        cmd = [info["exe"]]
    else:
        cmd = [sys.executable, info["script"]]

    res = subprocess.run(cmd, text=True, capture_output=True)
    t_elapsed = (time.perf_counter() - t0) * 1000

    print(res.stdout)
    if res.stderr:
        print("[stderr]:", res.stderr)

    print("-" * 80)
    print(f" [CRUNCH METRICS] Wall-Clock Execution Time: {t_elapsed:.2f} ms")
    print("=" * 80 + "\n")


def main():
    parser = argparse.ArgumentParser(description="IRIS-CRUNCH: High-Performance Computational Tool Suite")
    parser.add_argument("workload", nargs="?", default="all", choices=["galaxy", "cfd", "epidemic", "genomics", "three_body", "neural_lm", "superbug", "all"],
                        help="The computational workload to execute (default: all)")
    args = parser.parse_args()

    if args.workload == "all":
        for w in WORKLOADS:
            run_workload(w)
    else:
        run_workload(args.workload)


if __name__ == "__main__":
    main()
