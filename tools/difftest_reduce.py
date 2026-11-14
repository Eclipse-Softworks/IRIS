"""Shrink a program that fails the differential test, keeping the failure.

    python tools/difftest_reduce.py target/difftest/seed33.iris --native-crash
    python tools/difftest_reduce.py target/difftest/seed37.iris --compile-error "cannot find"
    python tools/difftest_reduce.py target/difftest/seed40.iris --differ

Deletes chunks of lines (halves, then quarters, ... then single lines),
keeping a deletion when the program still fails the same way, until no
single line can go. Writes <input>.min.iris.

A candidate must also still be a valid program: for --native-crash it must
run to completion in the interpreter, or deleting lines drifts toward a
different defect (a function that falls off its end, a loop whose counter
step was deleted). Runs a copy of target/debug/iris.exe taken at start, so
the compiler can be rebuilt while a reduction runs.
"""

import argparse
import os
import shutil
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
WORK = tempfile.mkdtemp(prefix="iris_reduce_")
IRIS = os.path.join(WORK, "iris.exe" if os.name == "nt" else "iris")
shutil.copy2(os.path.join(ROOT, "target", "debug", os.path.basename(IRIS)), IRIS)


def native(source):
    path = os.path.join(WORK, "candidate.iris")
    exe = os.path.join(WORK, "candidate.exe")
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        f.write(source)
    if os.path.exists(exe):
        os.remove(exe)
    build = subprocess.run([IRIS, "--emit", "binary", "-o", exe, path], capture_output=True, text=True)
    if build.returncode != 0:
        return ("build", build.stderr)
    try:
        run = subprocess.run([exe], capture_output=True, text=True, timeout=60)
    except subprocess.TimeoutExpired:
        return ("timeout", "")
    return (run.returncode, run.stdout)


def interpreted(source):
    path = os.path.join(WORK, "candidate.iris")
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        f.write(source)
    env = dict(os.environ, IRIS_FORCE_INTERP="1")
    try:
        run = subprocess.run([IRIS, "--emit", "eval", "--max-steps", "100000000", path],
                             capture_output=True, text=True, env=env, timeout=300)
    except subprocess.TimeoutExpired:
        return ("timeout", "")
    out = run.stdout.replace("\r\n", "\n")
    if out.endswith("0\n"):
        out = out[:-2]
    return (run.returncode, out + run.stderr)


def make_oracle(args):
    if args.native_crash:
        def still_fails(source):
            code, _ = native(source)
            if not (isinstance(code, int) and code not in (0, 1, 2)):
                return False
            i_code, _ = interpreted(source)
            return i_code == 0
        return still_fails
    if args.compile_error:
        def still_fails(source):
            code, err = native(source)
            return code == "build" and args.compile_error in err
        return still_fails
    if args.differ:
        def still_fails(source):
            n_code, n_out = native(source)
            if n_code != 0:
                return False
            i_code, i_out = interpreted(source)
            return i_code == 0 and i_out.replace("\r\n", "\n") != n_out.replace("\r\n", "\n")
        return still_fails
    sys.exit("choose --native-crash, --compile-error TEXT or --differ")


def reduce(lines, still_fails):
    chunk = max(1, len(lines) // 2)
    while chunk >= 1:
        i = 0
        removed_any = False
        while i < len(lines):
            candidate = lines[:i] + lines[i + chunk:]
            if candidate and still_fails("".join(candidate)):
                lines = candidate
                removed_any = True
                print(f"  {len(lines)} lines", flush=True)
            else:
                i += chunk
        if not removed_any:
            chunk //= 2
    return lines


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("program")
    parser.add_argument("--native-crash", action="store_true")
    parser.add_argument("--compile-error")
    parser.add_argument("--differ", action="store_true")
    args = parser.parse_args()
    lines = open(args.program, encoding="utf-8").read().splitlines(keepends=True)
    still_fails = make_oracle(args)
    if not still_fails("".join(lines)):
        sys.exit("the program does not fail that way")
    lines = reduce(lines, still_fails)
    out = args.program.replace(".iris", ".min.iris")
    with open(out, "w", encoding="utf-8", newline="\n") as f:
        f.write("".join(lines))
    print(out)
    print("".join(lines))


if __name__ == "__main__":
    main()
