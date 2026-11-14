#!/usr/bin/env python3
"""Train in IRIS, check quantization, and prepare an unflashed Uno bundle."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent.parent
PROJECT = ROOT / "projects" / "learning_robot"


def run(command, work, env):
    result = subprocess.run(command, cwd=work, env=env, capture_output=True,
                            text=True, encoding="utf-8", errors="replace", timeout=120)
    print(result.stdout, end="", flush=True)
    if result.returncode:
        raise RuntimeError(f"Command failed ({result.returncode}): {command}\n{result.stderr}")
    return {"command": [str(item) for item in command],
            "stdout": result.stdout, "stderr": result.stderr, "exit_code": 0}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--iris", default="target/release/iris.exe" if os.name == "nt" else "target/release/iris")
    parser.add_argument("--llvm", help="AVR-enabled LLVM shared library, for the embedded build")
    parser.add_argument("--output", default="target/learning-robot-export")
    args = parser.parse_args()
    compiler = Path(args.iris).resolve()
    output = Path(args.output).resolve()
    if not compiler.is_file():
        parser.error(f"compiler not found: {compiler}")
    # Generated .iris files must not enter the exhaustive public source catalog.
    if output == ROOT or any(output.is_relative_to(ROOT / name) for name in ("projects", "examples")):
        parser.error("choose an output directory outside projects/ and examples/, such as target/learning-robot-export")
    env = os.environ.copy()
    env.pop("IRIS_FORCE_INTERP", None)
    embedded_env = env.copy()
    if args.llvm:
        library = Path(args.llvm).resolve()
        if not library.is_file():
            parser.error(f"LLVM library not found: {library}")
        embedded_env["IRIS_LLVM_C_API"] = str(library)
    try:
        with tempfile.TemporaryDirectory(prefix="iris-robot-export-") as scratch:
            work = Path(scratch)
            shutil.copy2(PROJECT / "uno_control.iris", work / "uno_control.iris")
            checks = [run([str(compiler), "--version"], work, env)]
            checks.append(run([str(compiler), "run", str(PROJECT / "export.iris")], work, env))
            checks.append(run([str(compiler), "run", str(work / "learned_policy.iris")], work, env))
            checks.append(run([str(compiler), "embedded", str(work / "learned_policy.iris"),
                               "--target", "arduino-uno", "-o", str(work / "bundle")], work, embedded_env))
            names = ("learned_policy.iris", "uno_control.iris", "iris-robot-learned.txt")
            report = {
                "hardware_validated": False,
                "checks": checks,
                "sha256": {name: hashlib.sha256((work / name).read_bytes()).hexdigest() for name in names},
                "note": "Native self-test and AVR cross-compilation passed. No board was flashed or exercised.",
            }
            # Publish only after all checks pass. No directory deletion or flashing.
            output.mkdir(parents=True, exist_ok=True)
            for name in names:
                shutil.copy2(work / name, output / name)
            shutil.copytree(work / "bundle", output / "bundle", dirs_exist_ok=True)
            (output / "verification.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
            print(f"Prepared {output}\nHardware validation remains pending.")
    except (OSError, subprocess.TimeoutExpired, RuntimeError) as error:
        print(str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
