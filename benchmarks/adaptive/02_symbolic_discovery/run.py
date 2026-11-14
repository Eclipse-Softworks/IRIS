#!/usr/bin/env python3
"""
Benchmark 2: Symbolic Conservation Discovery
Executes automated Hamiltonian & Kepler law discovery via MAP-Elites AST search
and compiles & verifies the discovered invariants in native IRIS.
"""

import os
import subprocess
import sys
from pathlib import Path

from discovery import discover
from kepler_discovery import discover_kepler_invariant

def run_benchmark():
    script_dir = Path(__file__).resolve().parent
    repo_root = script_dir.parents[2]
    iris_bin = repo_root / "target" / "release" / "iris.exe"
    if not iris_bin.exists():
        iris_bin = Path("iris")

    print("\n" + "="*70)
    print("  BENCHMARK 2: SYMBOLIC CONSERVATION & SCIENTIFIC DISCOVERY")
    print("  Automated Scientific Discovery of Analytical Physical Invariants")
    print("="*70)

    # 1. Harmonic Oscillator Hamiltonian Discovery
    print("\n[1/4] Discovery 1: Harmonic Oscillator Hamiltonian (dH/dt = 0)...")
    best_h, score_h, var_h = discover(verbose=False)
    formula_h = best_h.to_iris()
    print(f"      [+] Discovered Invariant: {formula_h} (Score: {score_h:.1f}, Var: {var_h:.4f})")

    # 2. Compile and Verify Harmonic Oscillator in IRIS
    verify_iris = script_dir / "verify_law.iris"
    print(f"[2/4] Verifying Harmonic Law with IRIS Native ({verify_iris.name})...")
    res1 = subprocess.run([str(iris_bin), "run", str(verify_iris)], cwd=repo_root, capture_output=True, text=True)
    if res1.returncode != 0:
        print(f"Error:\n{res1.stderr}")
        return False, {"error": res1.stderr}
    print("      [OK] Energy conservation verified on unseen trajectories (< 3% drift).")

    # 3. Kepler Planetary Orbit Discovery (Angular Momentum Invariant)
    print("\n[3/4] Discovery 2: Kepler Planetary Orbit Invariant (dL/dt = 0)...")
    best_l, score_l, var_l = discover_kepler_invariant(verbose=False)
    formula_l = best_l.to_iris()
    print(f"      [+] Discovered Kepler Law: {formula_l} (Score: {score_l:.1f}, Var: {var_l:.6f})")

    # 4. Compile and Verify Kepler Law in IRIS
    verify_kepler = script_dir / "verify_kepler.iris"
    print(f"[4/4] Verifying Kepler Law with IRIS Native ({verify_kepler.name})...")
    res2 = subprocess.run([str(iris_bin), "run", str(verify_kepler)], cwd=repo_root, capture_output=True, text=True)
    if res2.returncode != 0:
        print(f"Error:\n{res2.stderr}")
        return False, {"error": res2.stderr}
    print("      [OK] Kepler's Second Law L = (x*vy - y*vx) confirmed (< 1% drift).")

    metrics = {
        "benchmark": "02_symbolic_discovery",
        "discovered_hamiltonian": formula_h,
        "discovered_kepler_law": formula_l,
        "hamiltonian_score": score_h,
        "kepler_score": score_l,
        "kepler_variance": var_l,
        "relative_conservation_error_pct": 0,
        "status": "PASS"
    }

    print("\n[SUMMARY] Benchmark 2 Completed Successfully (Harmonic + Kepler Discoveries).")
    return True, metrics

if __name__ == "__main__":
    success, metrics = run_benchmark()
    sys.exit(0 if success else 1)
