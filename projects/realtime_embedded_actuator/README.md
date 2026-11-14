# Hard Real-Time Embedded Actuator & Deadline Benchmark

Implements and validates **Priority 3 & 4** from [`docs/direction.md`](../../docs/direction.md):
- Real-time discrete control step latency and jitter measurement
- Zero heap allocations in the critical control loop
- 100% hard deadline satisfaction under physical perturbations
- Sub-microsecond policy hot-swapping overhead
- Bare-metal allocation-free component bundles targeting **ARM Cortex-M4F** and **RISC-V ESP32-C3** via `iris embedded`

---

## 1. Embedded Component Architecture

The actuator kernel [`controller.iris`](controller.iris) is implemented within the restricted IRIS embedded compiler profile:
- **Zero dynamic memory**: Stack and register scalar variables only (`i64`).
- **Fixed-point arithmetic**: Gains scaled by $256$ to avoid floating-point emulation overhead on microcontrollers without hardware FPUs.
- **Actuator limits**: Bounded effort output $\text{clamp}(u, -100, 100)$.

### Cross-Compilation Targets

```bash
# Compile bare-metal ARM Cortex-M4F bundle
iris embedded projects/realtime_embedded_actuator/controller.iris --target cortex-m4f

# Compile bare-metal RISC-V ESP32-C3 bundle
iris embedded projects/realtime_embedded_actuator/controller.iris --target esp32-c3
```

### Compiler Proof Fingerprints

| Target Architecture | LLVM Triple | Proof Fingerprint | Fixed-Array Footprint | Heap Allocations |
| :--- | :--- | :--- | :--- | :--- |
| **ARM Cortex-M4F** | `thumbv7em-none-eabihf` | `911b5e527d621cb0` | 32 bytes | **0 bytes** |
| **RISC-V ESP32-C3** | `riscv32-unknown-none-elf` | `07b77a781d4c869b` | 32 bytes | **0 bytes** |

---

## 2. Hard Real-Time Deadline & Jitter Verification

Run the high-resolution timing harness across 10,000 steps at a 1 kHz periodic deadline budget ($1,000.0\,\mu\text{s}$):

```bash
python projects/realtime_embedded_actuator/benchmark_deadlines.py
```

### Empirical Results Summary

| Metric | Measured Value | Budget / Invariant | Status |
| :--- | :--- | :--- | :--- |
| **Periodic Deadline Budget** | $1,000.0\,\mu\text{s}$ (1 kHz) | Hard real-time threshold | Target |
| **Deadline Misses** | **0 / 10,000 (0.00%)** | 0 misses allowed | **PASS** |
| **Median ($p50$) Latency** | $0.70\,\mu\text{s}$ | $< 50.0\,\mu\text{s}$ | **PASS** |
| **Mean Execution Time** | $0.68\,\mu\text{s}$ | $< 50.0\,\mu\text{s}$ | **PASS** |
| **99th Percentile ($p99$) Latency** | $0.90\,\mu\text{s}$ | $< 100.0\,\mu\text{s}$ | **PASS** |
| **Worst-Case Peak Latency** | $33.10\,\mu\text{s}$ | $< 1,000.0\,\mu\text{s}$ | **PASS** |
| **Timing Jitter ($\sigma$)** | **$0.59\,\mu\text{s}$** | Sub-microsecond deterministic | **PASS** |
| **Policy Hot-Swap Overhead** | **$0.60\,\mu\text{s}$** | $< 25.0\,\mu\text{s}$ | **PASS** |
| **Heap Memory Allocations** | **0 bytes** | Allocation-free | **PASS** |
