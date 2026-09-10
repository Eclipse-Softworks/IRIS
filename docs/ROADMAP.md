# IRIS Roadmap

> **Current version: 1.0.0-rc1** — release-candidate working tree. The existing
> `v1.0.0-rc1` tag predates the hardening work listed below and will not be
> force-moved; a later candidate must use a new tag.

This document outlines the planned milestones for IRIS from the current release
line through the stable 1.0 release and beyond.

---

## Current State (v1.0.0-rc1)

- Core parsing, typing, ownership/borrowing, effect analysis, SSA lowering,
  interpreter, LLVM native codegen, and ORC JIT are functional on the validated
  host configuration.
- Direct Windows MinGW `ld.lld` linking and the hash-validated checked-in runtime
  object are implemented; other targets still require their documented linker
  and sysroot validation.
- The public examples and projects have been rebuilt as a progressive RC1
  learning catalog. `examples/catalog.json` classifies every IRIS file;
  `tests/examples_showcase.rs` checks native execution, interpreter execution,
  and graph export according to that contract.
- Assertion-backed focused suites cover transactions, ORC, verified hot swap,
  differentiable control flow, AIS/ROS 2, embedded allocation proofs, and the
  seven-gate evolution coordinator.
- `std.ais`, `std.rl`, `std.ml`, `std.nn`, `std.tensor`, and ROS 2 topic/QoS
  surfaces exist. External SDK bridges and physical targets have narrower
  evidence documented in `known-issues.md`.
- LSP, trace-based DAP, REPL, lossless deterministic formatter, package manager,
  installer scripts, and VS Code extension sources are present. The extension
  includes Test Explorer run/debug profiles, rich function/trait/effect hovers,
  semantic highlighting, and dead/unreachable/unsafe source diagnostics;
  publication status must still be verified against the actual release channel.
- Compiler-hosted typed metaprogramming (`iris meta`, `std.meta`), typed bounded
  TCP/HTTP, Windows system TLS, provider-neutral `std.llm`, and production
  AIS/model lifecycle records are implemented and assertion-backed.
- A separate explicitly acknowledged unrestricted whole-program activation API
  exists for autonomous hosts. It intentionally carries no behavioral safety
  claim; governed `iris evolve` remains the recommended public path.

Note: Some items below are marked as completed on `master` but are not part of
the latest tagged release yet.

---

## v0.3.0 — Hardening & Polish

**Goal:** Ship a reliable, well-documented 0.x release that outsiders can use.

| # | Task | Priority | Status |
| --- | ------ | ---------- | -------- |
| 1 | Create `CHANGELOG.md` from 0.1.0 → 0.2.0 → 0.3.0 | High | ✅ Done |
| 2 | Add ~130 unit tests to hit the ≥200 target | High | ✅ Done (249) |
| 3 | Add ~150 integration tests to hit the ≥1000 target | High | ✅ Done (1135) |
| 4 | Publish VS Code extension to marketplace | High | ✅ Done |
| 5 | Clean up stale files (`phase21_TEMP.rs`, register external stdlib) | Medium | ✅ Done |
| 6 | Finalize `SPEC.md` — remove "Draft", fill grammar gaps | Medium | ✅ Done |
| 7 | Fix concurrency in native backend (`spawn`/`channel` crash) | High | ✅ Done |
| 8 | Fuzz all Tier 1 features — expand corpus, run extended campaigns | Medium | ✅ Done |
| 9 | Implement TCP/network I/O — replace interpreter stubs | Medium | ✅ Done |
| 10 | Error message improvements — source spans, colored diagnostics | Low | ✅ Done |

---

## v0.4.0 — Ecosystem & Packaging

**Goal:** Make it easy to share and consume IRIS libraries.

| # | Task | Priority | Status |
| --- | ------ | ---------- | -------- |
| 1 | Package registry — central registry for `iris pkg add` | High | ✅ Done |
| 2 | Dependency resolution — semver solver, lockfile (`iris.lock`) | High | ✅ Done |
| 3 | `iris doc` — auto-generate HTML docs from comments | Medium | ✅ Done |
| 4 | `iris fmt` — standalone formatter (currently LSP-only) | Medium | ✅ Done |
| 5 | `iris lint` — standalone linter with BP001–BP006 rules | Medium | ✅ Done |
| 6 | Cross-compilation — `iris build --target aarch64-linux` | Medium | ✅ Done |
| 7 | Incremental compilation — use cache infrastructure for faster rebuilds | Low | ✅ Done |

---

## v1.0.0-rc1 — ML & Compute

**Goal:** Make the ML headline features real, not stubs.

Release status: historical RC milestone; current hardening is ahead of the existing tag.

| # | Task | Priority | Status |
| --- | ------ | ---------- | -------- |
| 1 | Tensor runtime — replace shape-tracking stubs with real compute | Critical | ✅ Done |
| 2 | `einsum` codegen — generate loop nests or dispatch to BLAS | High | ✅ Done |
| 3 | ONNX export — binary protobuf, not just text format | High | ✅ Done |
| 4 | GPU backend — CUDA or Vulkan compute shaders for tensor ops | Medium | ✅ Done |
| 5 | SIMD codegen — auto-vectorize tight loops on x86/ARM | Medium | ✅ Done |
| 6 | Automatic differentiation v2 — reverse-mode AD | Medium | ✅ Done |
| 7 | Sparse tensor ops — CSR/COO kernels, sparse matmul | Low | ✅ Done |
| 8 | ML compute kernels — conv2d, maxpool, batchnorm, softmax, GELU | Critical | ✅ Done |
| 9 | Loss functions — MSE, cross-entropy, binary CE in C runtime | Critical | ✅ Done |
| 10 | Optimizers — SGD, Adam with state management | Critical | ✅ Done |
| 11 | BLAS dispatch — OpenBLAS/MKL via `-DIRIS_USE_BLAS` | High | ✅ Done |
| 12 | Blocked matmul — 32×32 tiled for cache friendliness | High | ✅ Done |
| 13 | AIS framework (`std.ais`) — agent loop, perception, decision | High | ✅ Done |
| 14 | RL module (`std.rl`) — Q-learning, SARSA, policy gradient, replay | High | ✅ Done |
| 15 | System requirements documentation (`REQUIREMENTS.md`) | Medium | ✅ Done |
| 16 | Nightly CI workflow — extended fuzz, smoke tests, benchmarks | Medium | ✅ Done |
| 17 | User IR optimization bump — `-O1` → `-O2` (LLVM 18) | Medium | ✅ Done |

---

## v1.0.0-rc1 — Performance & Security

**Goal:** Production-grade performance and trustworthy FFI.

Release status: historical RC milestone; current hardening is ahead of the existing tag.

| # | Task | Priority | Status |
| --- | ------ | ---------- | -------- |
| 1 | Security audit — FFI surface, filesystem ops, network I/O | Critical | ✅ Done |
| 2 | GC / memory management — refcounting or tracing GC for native backend | High | ✅ Done |
| 3 | Optimization passes — DCE, constant folding, inlining, loop unrolling | High | ✅ Done |
| 4 | Benchmark suite expansion — real-world workloads | Medium | ✅ Done |
| 5 | Profiler — `iris profile` with flame graphs | Medium | ✅ Done |
| 6 | Sandboxed FFI — restrict filesystem/network for untrusted packages | Low | ✅ Done |

---

## v1.0.0 — Stable Release

**Goal:** Freeze Tier 1 features, commit to backward compatibility.

| # | Gate | Criteria |
| --- | ------ | ---------- |
| 1 | All 12 STABILITY.md milestones met | Including ≥3 external contributors |
| 2 | Zero known crashers in fuzz campaign | 72-hour clean run |
| 3 | SPEC.md finalized and versioned | No "Draft" label |
| 4 | CHANGELOG complete | Every breaking change since 0.1.0 |
| 5 | Tier 1 features frozen | Syntax, builtins, CLI flags locked |
| 6 | GC implemented | No memory leaks in long-running programs |
| 7 | Tensor ops functional | At least matmul, elementwise, reduce on CPU |
| 8 | ML readiness | Loss functions, optimizers, conv/pooling layers functional |
| 9 | AIS framework | Agent lifecycle, decision strategies, RL primitives tested |
| 10 | Published artifacts | VS Code extension, package registry, 3-platform installers |

---

## Post-1.0 — Future Directions & Self-Evolving Software (2026+)

For the complete architectural design and multi-phase implementation roadmap on in-process verified reflection, speculative effect isolation, continuous-discrete neuro-symbolic algorithms, and homeostatic active inference, see:
See the [Self-Evolving Software & AI/ML Architecture Roadmap](self-evolving-software-and-ai-roadmap.md).

- **In-Process Reflection & Safe JIT** (`std.reflect`) — ✅ LLVM-C object emission, ORC execution, ABI validation, atomic whole-program gateway leases, eight-generation history, exact rollback and content-addressed artifacts
- **Speculative Effect Isolation** (`std.speculation`) — ✅ nested transactional list/map/atomic rollback and staged filesystem commit/rollback across interpreter and native/JIT backends
- **Differentiable Control Flow & Tensors** (`std.tensor`, `std.nn`) — ✅ end-to-end reverse-mode autodiff across closures, branches, loops, traits and neural-network helpers
- **Autonomous Intelligent Systems v2** (`std.ais`, `std.ros2`) — ✅ viability prediction, homeostatic pre-emption, expected-free-energy policy selection, QoS endpoints, typed receive, deterministic executor, lifecycle state and tf2-compatible local transforms; services/actions and wire-level tf2 remain future work
- **Embedded Microcontroller Targets** — ✅ allocation-free proof gate and freestanding object bundles for Cortex-M4F/M33, ESP32-C3 and Arduino Uno; ATmega328P/CH340 hardware execution verified on the BDD Ultimate Starter Kit V2 with an exact content fingerprint and zero allocation/fault counters. LLVM 17 AVR loops/fixed arrays are rejected pending an upstream-safe backend path.
- **Governed Evolution Coordinator** — ✅ scalar `iris evolve` gates plus mandatory persisted audit-head verification; ✅ v2 JSON/command/scalar manifest and state contracts as library APIs; generalized CLI canary metrics remain open
- **Native Candidate Sandbox** — ✅ Windows LPAC + Job Object launch is implemented and token-attested, with suspended startup, zero capabilities, bounded output, memory/CPU/process limits, and fail-closed unsupported hosts; scalar `iris evolve` remains interpreter-validated
- **Self-Hosting Compiler** — IRIS compiler and toolchain written natively in IRIS
- **WebAssembly & Distributed Compute** — `iris build --target wasm32` and multi-node tensor parallelism

---

## RC1 capability additions completed after the original roadmap

- **Unrestricted whole-program evolution:** explicit non-cloneable authority
  token, compiler/manifest/ABI validation, ORC materialization, atomic
  multi-gateway installation, leases and rollback. Policy, audit, sandbox and
  resource gates are intentionally absent.
- **Typed metaprogramming (`std.meta`, `iris meta`):** compiler-hosted
  typed/effect/ABI/CFG inspection, IR emission and compiler-verified UTF-8 source
  edits without a self-hosted compiler. Embedding the compiler service in
  standalone native applications remains future work.
- **Networking and LLMs (`std.net`, `std.http`, `std.llm`):** bounded typed TCP,
  exact writes, timeout/shutdown, status-bearing HTTP, custom headers, JSON
  queries, remote chat/tools/embeddings and local model wrappers. Windows system
  TLS is verified; other system-TLS adapters remain future work.
- **Production AIS/ML lifecycle (`std.ais`, `std.ml`):** versioned model
  registry/session APIs, batch inference/train/close, health telemetry and
  degraded/emergency agent behavior. Each external SDK/model combination still
  requires deployment validation.

*Last updated: 2026-09-09*
