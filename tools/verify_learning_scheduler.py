#!/usr/bin/env python3
"""Exercise the real terminal app and sample its process resources, without a board."""
import argparse
import ctypes
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parent.parent


class ProcessResources:
    """OS-reported high-water RSS sampled while the child is alive."""
    def __init__(self, process):
        self.process = process
        self.peak = 0
        self.cpu = None
        self.samples = 0
        self.handle = None
        if os.name == "nt":
            from ctypes import wintypes

            class Counters(ctypes.Structure):
                _fields_ = [("cb", wintypes.DWORD), ("faults", wintypes.DWORD)] + [
                    (name, ctypes.c_size_t) for name in (
                        "peak_rss", "rss", "peak_paged", "paged", "peak_nonpaged",
                        "nonpaged", "pagefile", "peak_pagefile")]

            class FileTime(ctypes.Structure):
                _fields_ = [("low", wintypes.DWORD), ("high", wintypes.DWORD)]

            self.Counters, self.FileTime = Counters, FileTime
            self.kernel = ctypes.WinDLL("kernel32", use_last_error=True)
            self.psapi = ctypes.WinDLL("psapi", use_last_error=True)
            self.kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
            self.kernel.OpenProcess.restype = wintypes.HANDLE
            self.kernel.CloseHandle.argtypes = [wintypes.HANDLE]
            self.psapi.GetProcessMemoryInfo.argtypes = [wintypes.HANDLE, ctypes.POINTER(Counters), wintypes.DWORD]
            self.kernel.GetProcessTimes.argtypes = [wintypes.HANDLE] + [ctypes.POINTER(FileTime)] * 4
            self.handle = self.kernel.OpenProcess(0x0400 | 0x0010, False, process.pid)

    def sample(self):
        if os.name == "nt" and self.handle:
            counters = self.Counters()
            counters.cb = ctypes.sizeof(counters)
            if self.psapi.GetProcessMemoryInfo(self.handle, ctypes.byref(counters), counters.cb):
                self.peak = max(self.peak, counters.peak_rss)
                self.samples += 1
            times = [self.FileTime() for _ in range(4)]
            if self.kernel.GetProcessTimes(self.handle, *(ctypes.byref(item) for item in times)):
                self.cpu = sum((item.high << 32) | item.low for item in times[2:]) / 10_000_000
        elif os.name == "posix":
            try:
                status = Path(f"/proc/{self.process.pid}/status").read_text()
                match = re.search(r"^VmHWM:\s+(\d+) kB", status, re.M)
                if match:
                    self.peak = max(self.peak, int(match[1]) * 1024)
                    self.samples += 1
                stat = Path(f"/proc/{self.process.pid}/stat").read_text().rsplit(")", 1)[1].split()
                self.cpu = (int(stat[11]) + int(stat[12])) / os.sysconf("SC_CLK_TCK")
            except (OSError, ValueError, IndexError):
                pass

    def close(self):
        if self.handle:
            self.kernel.CloseHandle(self.handle)


def session(binary, cwd, commands, env):
    started = time.perf_counter()
    process = subprocess.Popen([str(binary)], cwd=cwd, env=env, stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                               encoding="utf-8", errors="replace")
    resources = ProcessResources(process)
    done = threading.Event()
    result = {}

    def communicate():
        try:
            result["stdout"], result["stderr"] = process.communicate(commands, timeout=60)
        except subprocess.TimeoutExpired:
            process.kill()
            result["stdout"], result["stderr"] = process.communicate()
            result["timeout"] = True
        finally:
            done.set()

    reader = threading.Thread(target=communicate)
    reader.start()
    while not done.is_set():
        resources.sample()
        done.wait(0.005)
    reader.join()
    resources.sample()
    resources.close()
    result.update(wall_seconds=time.perf_counter() - started,
                  cpu_seconds=resources.cpu, sampled_peak_rss_bytes=resources.peak or None,
                  resource_samples=resources.samples, exit_code=process.returncode)
    assert not result.get("timeout"), "interactive session timed out"
    assert process.returncode == 0, result
    assert "learning scheduler: session complete" in result["stdout"], result
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--iris", default="target/release/iris.exe" if os.name == "nt" else "target/release/iris")
    parser.add_argument("--output", default="target/learning-scheduler-verification.json")
    args = parser.parse_args()
    compiler = Path(args.iris).resolve()
    output = Path(args.output).resolve()
    env = os.environ.copy()
    env.pop("IRIS_FORCE_INTERP", None)
    env.pop("IRIS_SCHEDULER_DEMO", None)
    results = {}
    with tempfile.TemporaryDirectory(prefix="iris-scheduler-") as temporary:
        work = Path(temporary)
        binary = work / ("scheduler.exe" if os.name == "nt" else "scheduler")
        build = subprocess.run([str(compiler), "--emit", "binary",
                                str(ROOT / "projects/learning_scheduler/main.iris"), "-o", str(binary)],
                               env=env, capture_output=True, text=True, timeout=120)
        assert build.returncode == 0, build.stderr
        results["interactive"] = session(binary, work,
            "train\ncompare\ninspect 7\nshift\nauto\nstatus\nfault\ncompare\nrecover\nstatus\n"
            "jobs 9\nexecute\nexecute\nsave\nquit\n", env)
        transcript = results["interactive"]["stdout"]
        assert transcript.count("Promoted generation=") == 2
        assert "Watchdog restored saved weights" in transcript
        assert "predicted_cost=" in transcript
        checksums = re.findall(r"CPU jobs complete: count=9 wall_ms=\d+ checksum=(\d+)", transcript)
        assert len(checksums) == 2 and checksums[0] == checksums[1]
        saved_weights = re.findall(r"weights: first=.*", transcript)[-1]
        results["restart"] = session(binary, work, "load\nstatus\nquit\n", env)
        assert "Loaded checkpoint" in results["restart"]["stdout"]
        assert saved_weights in results["restart"]["stdout"]
        (work / "iris-scheduler-model.txt").write_text("broken\n", encoding="utf-8")
        results["bad_checkpoint"] = session(binary, work, "load\nstatus\nquit\n", env)
        assert "Load failed; active model retained" in results["bad_checkpoint"]["stdout"]
        assert "weights: first=0.3 second=0.3 bias=0.02" in results["bad_checkpoint"]["stdout"]
        results["bad_commands"] = session(binary, work, "jobs 129\njobs nope\nunknown\nquit\n", env)
        assert results["bad_commands"]["stdout"].count("Command rejected:") == 3
        results["eof"] = session(binary, work, "", env)
        results["bounded_cpu"] = session(binary, work,
            "jobs 128\ntrain\nexecute\nexecute\ncompare\nquit\n", env)
        assert results["bounded_cpu"]["stdout"].count("CPU jobs complete: count=128") == 2
    report = {
        "scope": "Native app sessions only; compilation excluded. RSS is OS high-water memory sampled during execution; short peaks may be missed. Includes runtime, UI, training and jobs. Not allocation counts or per-job resource costs.",
        "sessions": results,
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    for name, result in results.items():
        print(f"PASS {name}: wall={result['wall_seconds']:.3f}s cpu={result['cpu_seconds']}s peak_rss={result['sampled_peak_rss_bytes']} bytes")
    print(f"Report: {output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
