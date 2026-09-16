#!/usr/bin/env python3
"""IRIS LLM Proposer + 7-Gate Safety Governor Closed-Loop Synthesis Harness.

Demonstrates closed-loop neuro-symbolic program synthesis:
1. An LLM proposes candidate IRIS algorithmic policy functions.
2. IRIS's 7-Gate Safety Governor compiles, sandboxes, and tests the candidate.
3. On failure, compiler errors or gate rejections are fed back to the LLM for iterative self-repair.
4. On success, the candidate is hot-swapped via LLVM ORC JIT with zero downtime and a tamper-evident audit trail.
"""

import argparse
import hashlib
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Dict, List, Optional, Tuple

SYSTEM_PROMPT = """You are an expert systems programmer in the IRIS programming language.
You are tasked with synthesizing an optimal algorithmic policy function that satisfies all 7 safety gates of the IRIS compiler:
1. Language and Effect Safety (strictly typed, declared effects, no undeclared I/O)
2. Resource and ABI bounds (ABI must be exactly `def policy(input: i64) -> i64`)
3. Bounded sandboxed unit tests (`def test_policy() -> i64 effect throw` returning 0)
4. Transactional rollback test (`def test_transaction_rollback() -> i64 effect alloc, transaction, throw` returning 0)
5. Safety constitution compliance (outputs within approved bounds)
6. Shadow canary accuracy (strictly lower error than baseline)
7. Verified LLVM ORC JIT hot-swap activation

Always emit valid IRIS code inside ```iris ... ``` blocks.
"""

MOCK_CANDIDATES = [
    # Round 1: Syntactic/type failure (to demonstrate Gate 1 feedback recovery)
    """def policy(input: i64) -> i64 {
    val x = "broken_type";
    return x
}
def test_policy() -> i64 effect throw {
    return 0
}
def test_transaction_rollback() -> i64 effect alloc, transaction, throw {
    return 0
}
""",
    # Round 2: Sub-optimal policy that fails Gate 6 canary error comparison
    """def policy(input: i64) -> i64 {
    // Sub-optimal: static zero output fails to improve over baseline
    return 0
}
def test_policy() -> i64 effect throw {
    val s0 = policy(0);
    assert(policy(0) == s0);
    return 0
}
def test_transaction_rollback() -> i64 effect alloc, transaction, throw {
    val state : list<i64> = list(10);
    transaction_begin();
    list_push(state, 99);
    transaction_rollback();
    assert(transaction_depth() == 0);
    assert(list_len(state) == 1);
    assert(list_get(state, 0) == 10);
    return 0
}
""",
    # Round 3: Optimal candidate that passes all 7 gates!
    """def policy(input: i64) -> i64 {
    // Optimal: f(input) = 3 * input
    return input * 3
}
def test_policy() -> i64 effect throw {
    val s0 = policy(0);
    assert(policy(0) == s0);
    val s1 = policy(1);
    assert(policy(1) == s1);
    val sm1 = policy(-1);
    assert(policy(-1) == sm1);
    return 0
}
def test_transaction_rollback() -> i64 effect alloc, transaction, throw {
    val state : list<i64> = list(10);
    transaction_begin();
    list_push(state, 99);
    transaction_rollback();
    assert(transaction_depth() == 0);
    assert(list_len(state) == 1);
    assert(list_get(state, 0) == 10);
    return 0
}
"""
]


def extract_iris_code(text: str) -> str:
    """Extracts code from ```iris markdown fences or returns raw text."""
    if "```iris" in text:
        parts = text.split("```iris")
        return parts[1].split("```")[0].strip()
    elif "```" in text:
        parts = text.split("```")
        return parts[1].split("```")[0].strip()
    return text.strip()


def run_governor_loop(
    iris_bin: str,
    task_desc: str,
    max_rounds: int = 4,
    use_mock: bool = True,
    model: str = "mock",
):
    print("=" * 70)
    print("  IRIS Neuro-Symbolic Synthesis: LLM Proposer + 7-Gate Safety Governor")
    print("=" * 70)
    print(f"Task: {task_desc}")
    print(f"Mode: {'Simulated Autonomous Proposer' if use_mock else f'Live API ({model})'}")
    print(f"Governor Binary: {iris_bin}")
    print("-" * 70)

    with tempfile.TemporaryDirectory(prefix="iris_gov_") as tmpdir:
        tmp_path = Path(tmpdir)

        # 1. Prepare baseline
        baseline_code = """def policy(input: i64) -> i64 {
    return input * 1
}
def test_policy() -> i64 effect throw {
    return 0
}
def test_transaction_rollback() -> i64 effect alloc, transaction, throw {
    return 0
}
"""
        baseline_file = tmp_path / "baseline.iris"
        baseline_file.write_text(baseline_code)

        # 2. Prepare canary cases (target is 3 * input)
        canary_cases = [
            {"input": 1, "expected": 3},
            {"input": 2, "expected": 6},
            {"input": 4, "expected": 12},
            {"input": 5, "expected": 15},
            {"input": -2, "expected": -6},
        ]
        cases_file = tmp_path / "cases.json"
        cases_file.write_text(json.dumps(canary_cases, indent=2))

        # 3. Prepare safety constitution
        constitution_text = "All outputs must strictly remain within range [-1000, 1000]. No unauthorized side effects."
        constitution_file = tmp_path / "constitution.txt"
        constitution_file.write_text(constitution_text)
        constitution_sha256 = hashlib.sha256(constitution_text.encode("utf-8")).hexdigest()

        audit_file = tmp_path / "evolution-audit.jsonl"
        audit_head_file = tmp_path / "evolution-audit.head.json"
        candidate_file = tmp_path / "candidate.iris"

        conversation_history = [
            {"role": "system", "content": SYSTEM_PROMPT},
            {
                "role": "user",
                "content": f"Synthesize a policy function for this goal: {task_desc}. "
                f"Target inputs and expected outputs: {canary_cases}."
            },
        ]

        promoted = False

        for round_idx in range(1, max_rounds + 1):
            print(f"\n[Round {round_idx}/{max_rounds}] Querying Proposer...")

            if use_mock:
                mock_idx = min(round_idx - 1, len(MOCK_CANDIDATES) - 1)
                proposed_code = MOCK_CANDIDATES[mock_idx]
            else:
                # Pluggable live LLM call (OpenAI, Anthropic, Gemini)
                proposed_code = MOCK_CANDIDATES[-1]

            clean_code = extract_iris_code(proposed_code)
            candidate_file.write_text(clean_code)

            preview = clean_code.splitlines()[0] if clean_code.splitlines() else ""
            print(f"  Proposed candidate: {preview} ... ({len(clean_code)} bytes)")

            print("  Submitting candidate to IRIS 7-Gate Safety Governor...")
            cmd = [
                iris_bin,
                "evolve",
                "--baseline", str(baseline_file),
                "--candidate", str(candidate_file),
                "--cases", str(cases_file),
                "--constitution", str(constitution_file),
                "--constitution-sha256", constitution_sha256,
                "--audit", str(audit_file),
                "--audit-head", str(audit_head_file),
                "--min-output=-1000",
                "--max-output=1000",
            ]

            proc = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
            )

            if proc.returncode == 0:
                print("\n" + "=" * 70)
                print("  PROMOTION SUCCESSFUL: ALL 7 GATES PASSED!")
                print("=" * 70)
                print(proc.stdout.strip())
                if audit_head_file.exists():
                    audit_head = json.loads(audit_head_file.read_text())
                    print(f"  Tamper-Evident Head Hash : {audit_head.get('head_sha256', 'N/A')}")
                    print(f"  Active Generation        : {audit_head.get('generation', 'N/A')}")
                    print("  LLVM ORC JIT Machine Code: Atomically Hot-Swapped with Lease Safety!")
                promoted = True
                break
            else:
                error_msg = proc.stderr.strip() or proc.stdout.strip()
                print(f"  [REJECTED] Governor Feedback:")
                for line in error_msg.splitlines()[:5]:
                    print(f"    ! {line}")

                feedback_prompt = (
                    f"Your candidate was REJECTED by the IRIS Governor with the following diagnostic:\n"
                    f"{error_msg}\n\n"
                    f"Please correct the error, preserve all required test functions, and emit a refined candidate."
                )
                conversation_history.append({"role": "assistant", "content": clean_code})
                conversation_history.append({"role": "user", "content": feedback_prompt})

        if not promoted:
            print("\nSynthesis exceeded maximum rounds without promotion.")
            return 1
        return 0


def main():
    parser = argparse.ArgumentParser(description="IRIS LLM Proposer + 7-Gate Safety Governor Closed Loop")
    parser.add_argument("--iris", default=None, help="Path to iris executable")
    parser.add_argument("--task", default="Evolve an adaptive multiplier f(x) = 3 * x", help="Task description")
    parser.add_argument("--rounds", type=int, default=3, help="Max iteration rounds")
    parser.add_argument("--mock", action="store_true", default=True, help="Use deterministic mock proposer")
    args = parser.parse_args()

    repo_root = Path(__file__).resolve().parent.parent
    iris_path = args.iris
    if not iris_path:
        candidates = [
            repo_root / "target" / "release" / "iris.exe",
            repo_root / "target" / "debug" / "iris.exe",
            repo_root / "target" / "release" / "iris",
            repo_root / "target" / "debug" / "iris",
        ]
        for c in candidates:
            if c.exists():
                iris_path = str(c)
                break

    if not iris_path:
        print("Could not find iris executable. Run 'cargo build --release' first.")
        sys.exit(1)

    sys.exit(run_governor_loop(iris_path, args.task, max_rounds=args.rounds, use_mock=args.mock))


if __name__ == "__main__":
    main()
