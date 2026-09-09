#!/usr/bin/env python3
"""Run the public learning catalog with bounded execution and assertion checks."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--iris", default="target/debug/iris.exe" if os.name == "nt" else "target/debug/iris")
    parser.add_argument("--mode", choices=("all", "native", "interpreter"), default="all")
    parser.add_argument("--filter", default="")
    parser.add_argument("--timeout", type=int, default=120)
    args = parser.parse_args()
    compiler = str(Path(args.iris).resolve())
    catalog = json.loads((ROOT / "examples/catalog.json").read_text(encoding="utf-8"))
    failures = []
    count = 0
    for entry in catalog["entries"]:
        path = ROOT / entry["path"]
        kind = entry["mode"]
        if args.filter not in entry["path"] or kind in ("module", "embedded", "manual"):
            continue
        source = path.read_text(encoding="utf-8")
        if kind != "graph" and ("assert(" not in source or "return 0" not in source):
            failures.append(entry["path"] + ": missing assertions or explicit return 0")
            continue
        modes = ["native", "interpreter"] if kind == "dual" else ["interpreter" if kind == "host" else "native"]
        for mode in modes:
            if args.mode != "all" and mode != args.mode:
                continue
            env = os.environ.copy()
            env.pop("IRIS_FORCE_INTERP", None)
            env.update(entry.get("env", {}))
            if mode == "interpreter":
                env["IRIS_FORCE_INTERP"] = "1"
            command = [compiler, "--emit", "eval", str(path)] if mode == "interpreter" else [compiler, "run", str(path)]
            if kind == "graph":
                command = [compiler, "--emit", "graph", str(path)]
            start = time.monotonic()
            try:
                # Each program gets a scratch working directory for its own files.
                with tempfile.TemporaryDirectory(prefix="iris-learning-") as work:
                    result = subprocess.run(command, cwd=work, env=env, capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=args.timeout)
                expected = entry.get("expected", "")
                if result.returncode or (expected and expected not in result.stdout):
                    failures.append(f"{entry['path']} [{mode}]\n{result.stdout}\n{result.stderr}")
                    print(f"FAIL {mode}: {entry['path']}", flush=True)
                else:
                    print(f"PASS {mode}: {entry['path']} ({time.monotonic() - start:.1f}s)", flush=True)
            except (subprocess.TimeoutExpired, OSError) as error:
                failures.append(f"{entry['path']} [{mode}]: {error}")
            count += 1
    for failure in failures:
        print(failure, file=sys.stderr)
    print(f"{count} executions, {len(failures)} failures", flush=True)
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
