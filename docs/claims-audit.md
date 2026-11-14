# IRIS Evidence-Graded Claims Audit

This audit applies the grading standard in `.claude/skills/iris-claims/SKILL.md`:
- **Verified**: observed running end-to-end (including both native and interpreter backends where applicable).
- **Tested**: automated unit or integration tests exist and pass, with scope/hardware limits stated inline.
- **Present**: implemented in source, without end-to-end verification across all advertised configurations.
- **Absent**: neither implementation nor tests exist.

Every capability states its boundary **inline** in the same row.

## Capability Matrix

| Capability | Grade | Evidence & Inline Limits |
|---|---|---|
| **IR-level NLL borrow checking** | **Tested** | `CfgBorrowCheckerPass` (`src/pass/cfg_borrow_checker.rs`) runs in `build_standard_pipeline()` with dataflow liveness and loan tracking; tested in `cargo test --lib` (431 passed). Lexical/AST checks (`src/pass/borrow_checker.rs`) also remain alongside IR checks. |
| **Explicit IR drop scopes** | **Tested** | `ScopeId`, `DropScope`, `IrInstr::{EnterScope, ExitScope, Drop}`, and `DropElaborationPass` (`src/pass/drop_elaboration.rs`) are wired into `build_standard_pipeline()` and covered by `raii_drop_and_slices` and `pass_pipeline` tests. |
| **Incremental compilation & type interning** | **Tested** | `QueryEngine` (`src/query.rs`) provides SHA-256 input deduplication and AST structural early cutoff (used by `LspState`); `TypeArena`/`IrTypeId` (`src/ir/types.rs`) interns IR types per function. Separate multi-crate/cross-file incremental linking is not implemented; lazy post-IR monomorphisation remains future work. |
| **ML / tensor compile-time optimization & linking** | **Tested** | `MlFusionPass` (`src/pass/ml_fusion.rs`) eliminates identity/double transposes, fuses consecutive transposes, collapses redundant reshapes, and cancels double `neg` in `build_standard_pipeline()`. Runtime ML backends remain `dlopen`-resolved by default (`MlLinkMode::DynamicDlopen`), with opt-in `IRIS_ML_STATIC_LINK=1` (`MlLinkMode::StaticDirect` in `src/codegen/build.rs`); unit-tested in `cargo test --lib`. |
| **LSP & DAP tooling** | **Tested** | `src/lsp.rs` supports diagnostics (via `QueryEngine` cache), completion, hover, definition, references, rename, semantic tokens, document highlights, call hierarchy, formatting, code actions, inlay hints, and `textDocument/foldingRange`. `src/dap.rs` supports breakpoints, stepping, evaluation, scopes/variables, and `run_dap_server_with_io` unit tests. |
| **Tail-call optimization** | **Verified** | `TailCallPass` converts self-recursion to loops and mutually-recursive SCCs into trampolines at the IR level; limit: mutually-recursive groups must share parameter list and return type or TCO is declined cleanly. |
| **Async runtime** | **Verified** | `async def` lowers to state machines on both native and interpreter backends (`tests/test_async_tasks.iris`, `tests/async_await.rs`). Limit: cooperative per-thread executor; async wakeups cover timers, channels, and `tcp_accept_async`/`tcp_read_async`, while socket writes, connects, and HTTP still block. |
| **ROS 2 interoperability** | **Verified** | `std.ros2` verified on Windows Humble (Cyclone DDS) and Linux Humble (Fast DDS), plus simulated `projects/evolving_turtle` (`tests/ros2_live.rs`, `tests/evolving_turtle.rs`). Limit: macOS unbuilt, one single-threaded executor per context, no physical robot hardware tested. |
| **CUDA (`--gpu cuda`) & TensorRT (`--emit tensorrt`)** | **Tested** | CUDA PTX emission and CPU fallback execution are tested (`tests/cuda_*.rs`), and TensorRT `trtexec` CLI invocation is tested (`tests/tensorrt_*.rs`), **but neither has been run on NVIDIA GPU hardware or where TensorRT is installed**. |
| **Allocation-freedom proof** | **Tested** | `--strict-effects` tracks calls, methods, `dyn Trait`, closures, and function-valued parameters; `prove_allocation_free` verifies scalar-only non-allocating IR for embedded/evolution ABIs (not `--abi record`, which is heap-backed). `projects/uno_controls` is host-simulated, not physically flashed. |
| **`std.ais` active inference** | **Tested** | Avoid describing `std.ais` as "research-grade" as a whole. Claim only specific tested functions and `projects/gridworld_inference` (tabular, fully observed, single seeded run on both backends). |
| **Test suite & conformance** | **Verified** | `cargo test --lib` passes **431** unit tests; `tests/conformance.rs` passes **3/3**; `tests/v0_6_0_performance_security.rs` passes **36/36**; `tests/` contains **229** integration test files (`NEEDS_ASSERTIONS = []` in `tests/iris_corpus.rs`). Caveat: 17 files in `projects/` and 5 in `benchmarks/` still define `main()` without assertions. |

## Ground Truth Measurements (Current Workspace)

- `IrInstr` (`src/ir/instr.rs`): **141** brace-variants
- `build_standard_pipeline()` (`src/pass/mod.rs`): **26** passes
- `src/stdlib/*.iris`: **51** modules
- `cargo test --lib`: **431** unit tests (passing)
- `tests/*.rs`: **229** integration test files

## Documentation Corrections Applied

1. `docs/portability-readiness.md`: Removed `"61 functions of research-grade autonomy in std.ais"` and added inline CPU-only testing limits for CUDA.
2. `README.md`: Updated the `Memory` row from lexical annotations to IR-level CFG NLL borrow checking (`CfgBorrowCheckerPass`) and explicit IR drop scopes (`DropElaborationPass`).
3. `docs/REQUIREMENTS.md`: Updated stale test count comment (`2,157 / 411 unit tests`) to `431 library unit tests via --lib; 229 integration test files in tests/`.
4. `docs/BOOK.md`: Added inline TensorRT limit (`CLI invocation tested, not yet run where TensorRT is installed`) next to `--emit tensorrt`.
5. `docs/architecture-vs-rustc.md`: Added status note in §3.2 reflecting that `CfgBorrowCheckerPass`, `DropElaborationPass`, `QueryEngine`, and `TypeArena` are now implemented while lazy post-IR monomorphisation remains future work.
6. `.claude/skills/iris-claims/SKILL.md`: Updated ground-truth metrics (`26 standard passes`, `141 brace-variants`, `431 unit tests`, `229 integration test files`).

