# Changelog

All notable changes to the IRIS programming language are documented here.

This project follows [Keep a Changelog](https://keepachangelog.com/) conventions.

---

## Unreleased

### Added

- **Rich DWARF debug types, native `attach`, and Windows GDB DAP stability:** `iris build -g` now emits entry-block `alloca` + `@llvm.dbg.declare` slots for locals (keeping `val`/`var` bindings alive on the stack) and DWARF composite/derived type metadata for `str`, `record` structs, `enum` variants, fixed scalar arrays, `option`, `result`, `tuple`, `list`, and `map` (with `ptr` fallback). `iris dap` supports native `"attach"` (`processId`/`pid` or `"target"`), forwards `stepBack`/`reverseContinue` when the native adapter advertises `supportsStepBack`, fixes the Windows GDB 17.1 DAP `setBreakpoints` timeout in `dap_native`, and targets LLVM 23.1.1.
- **`iris doctor` native debugger probe:** `iris doctor` reuses `dap_native`'s adapter lookup (`IRIS_NATIVE_DAP`, GDB 14+, or LLVM 23 `lldb-dap` with Windows Python DLL diagnostics).

- **Nine placeholder features made real** (details and limits in [known issues #120–#132](known-issues.md)):
  - **Tensor indexing natively.** `t[i, j, ...]` and flat `t[k]` at any rank, bounds-checked with E0403 on both backends; native `t[i]` used to read the tensor's header and `t[i, j]` returned NULL. Interpreter tensors now share storage like native ones.
  - **`--simd`.** Element-wise `par for` bodies get explicit vector kernels (AVX2/FMA on x86-64, NEON, wasm simd128) that keep every check the scalar code makes; a failing iteration is reported exactly as the scalar build reports it. Bit-identical results; a 4M-element `f32` loop ran 37× faster than scalar on the test machine. `--emit simd` output used to be rejected by LLVM.
  - **`--gpu cuda`.** Device-code `par for` loops compile to PTX (clang's NVPTX backend, libdevice for transcendental functions) and run through the CUDA driver API, with the CPU's error semantics (new code E0410 when no GPU or driver is available). Device code is tested by compiling it for the CPU and checking values and error records; **not yet run on an NVIDIA GPU**.
  - **`--pgo-generate` / `--pgo-use FILE`.** Instrumented builds count every block into `iris.profile` (accumulating across runs, also on a runtime error); optimised builds apply measured branch weights, entry counts, a profile summary and `cold`. The old PGO path counted nothing and stamped fixed weights.
  - **Python extensions that build.** `--emit python-ext -o NAME.pyd|.so` compiles and links the module; lists, maps, tuples, options, results, records, choices, tensors and arrays convert both ways, calls release the GIL, and a runtime error raises `NAME.IrisError` instead of ending Python.
  - **`iris service-daemon` is a real proxy.** It fronts an HTTP service (`--upstream`, `--listen`), measures p99, errors and load each interval, and switches between forwarding, shedding, serving from cache and throttling by measured outcome. It used to replay a fixed schedule. Plain HTTP/1.1 only, no TLS.
  - **ONNX and TensorRT.** Binary ONNX output now passes the ONNX checker and matches numpy under ONNX Runtime (layers carry initializers, from `--weights FILE` or seeded Glorot); `-o` writes bytes, not hex. `--emit tensorrt` builds a serialized engine with `trtexec`; not yet run where TensorRT is installed.
  - **A package registry.** The default registry is `registry/` in this repository (it was `example.com`); `iris pkg publish`, `iris pkg search` and `iris pkg serve` added; a registry can be a directory, `file://` or HTTP.
  - **`iris upgrade`** downloads this target's release archive from Eclipse-Softworks/IRIS, verifies its SHA-256 digest, extracts the executable from `.zip` or `.tar.gz` and orders pre-releases correctly.
- **The backends agree on more scalar operations** (#122): `f32` `pow`/`min`/`max`/`sign` build natively, NaN `!=` is true, shifts are taken modulo the width, the IR's `floordiv` floors on both backends, `bool` and unsigned values convert to floats correctly, float-to-int saturates, `sign(0.0)` is `0`.
- **A `par for` worker's runtime error names its own line** (#123): the error position is per thread.
- **Tensor `+ - * /` and `reshape` on mismatched sizes stop with E0404** natively instead of returning NULL (#132).
- **The preprocessor does what the BOOK said** (#133): `#if` compares values with C's grammar and precedence, a valued `#define` substitutes its value (on tokens, positions kept), nesting is correct, the predefined symbols follow `--target` and `-g`, and `-D NAME[=VALUE]` defines one.
- **`--emit onnx` is the real model in protobuf text format** (#135), equal to `--emit onnx-binary`'s once parsed; it was a sketch with no weights.
- **WebAssembly builds work again** (#136): the runtime compiles for `wasm32-wasip1` and the module runs (checked under Node's WASI); a program that reaches the network on WASI preview 1 is error E0303 naming each call.
- **Lists index with `[]`** (#137): `xs[i]` and `xs[i] = v` are bounds-checked list operations; `x[i] op= v` applies the operator (it stored `v`); string `+=` concatenates on variables, fields and elements; native `char_at` counts characters like the interpreter; `[]` on a string or map is E0124 with the function to use.
- Removed: the runtime's `iris_call_closure`/`iris_dyn_call` and `inference::StubBackend`, stubs nothing called (#134).
- **The source type checker is the type checker** (type-system milestones 4 and 5, #138): its errors are reported whether or not lowering accepts the program, with lowering's fixes, and lowering takes its types, leaving no value untyped (0 of the corpus's values, from 274). Newly rejected: `?` in a function that returns no result, `?`s with different error types in one `try`, lists of mixed types, `concat` with a non-string.
- **`std.ais` v3: active inference, learning and safety that compose.** Discrete active inference with exact Bayesian filtering, expected free energy computed from the model over multi-step policies (risk, ambiguity, information gain, utility, novelty), a policy posterior with precision, and Dirichlet learning of the likelihood and transitions (`Pomdp`, `AifAgent`). An online world model (`LinearModel`, recursive least squares with forgetting and a bounded covariance) and `viability_choose`, which keeps a predicted trajectory viable and breaks ties toward what the model is unsure of. A `SafetyPolicy` whose rules depend on the state (`safety_forbid_when`, `safety_require_when`) and on the model's predictions (`safety_keep_within`), with an audit trail; `aif_decide_permitted` plans only among permitted actions. `Autopilot` composes model learning, safety and viability; `software_governor_step` closes the governor's loop; `ewc_consolidate` and `ewc_penalty_grad` make EWC usable; models save and load (`pomdp_save`, `linear_model_save`). Each is demonstrated with assertions, on both backends: in the T-maze an agent that values information takes the rewarded arm 100 times in 100, one that weighs only reward 53 (`tests/test_ais_tmaze.iris`); learned likelihoods and transitions converge to the world's within 0.05, and the novelty drive covers an unexplored corridor in 5 steps, the minimum, where a random walk takes 37 (`test_ais_learning.iris`); an autopilot that starts knowing nothing keeps a tank in its band for 280 of 280 steps through a heat wave a thermostat does not survive (`test_ais_world_model.iris`); EWC keeps a first task's loss at 0.011 where plain training forgets it to 2.04 (`test_ais_ewc.iris`). `tests/ais_reference/reference.py` computes the same quantities independently, from their textbook definitions, and `test_ais_reference.iris` matches it to 1e-9. Examples: `examples/08_ais/active_inference.iris`, `examples/08_ais/autopilot.iris`.
- **Runtime errors that say what and where, the same on both backends.** A native program used to report a failed index as `iris: list index 5 out of bounds` with no position -- or not at all: an access violation, a wrong answer from a division by zero, a silent wrap on overflow, a silent death on stack overflow. Now both backends stop with exit code 101 and one diagnostic: the code (E0402 division by zero, E0403 index, E0406 panic, E0408 stack overflow, E0409 overflow), the message, the file, line and column of the expression that failed, the source line, and a help line. `iris run` and `--emit eval` render the native report with the source excerpt; a failure inside a standard-library function is reported at the line that called it, and an error in one of your own brought files in that file. What the program printed first is kept, in the interpreter too. `tests/runtime_errors.rs` holds both backends to the same code, message and position for each case in `tests/runtime_errors/`.
- **Integer arithmetic is checked** (SPEC 19.3): overflow stops with E0409 instead of wrapping silently natively, and constant folding no longer folds an overflow into a wrapped value. `wrapping_add`, `wrapping_sub` and `wrapping_mul` are the explicit wrap-around operations. About 2% on a native benchmark.
- **Clearer compile errors.** Every construct the lowerer used to reject as "unsupported expression -- ... This construct is not yet supported" now has a specific code and message: no such variant (E0122), `break`/`continue` outside a loop (E0123), an operation the value's type does not have (E0124), a wrong element count (E0125), a format string mismatch (E0126), a malformed `when` (E0127), an invalid assignment target (E0128), a value used as a trait it does not implement (E0129); E0103 now means only a genuine limitation. A type error mid-statement has a position. A keyword used as a name is one error with a fix, not a cascade. An error in a brought file is shown in that file.

- **`iris evolve-live`** ([`src/evolution/live.rs`](../src/evolution/live.rs)): a control loop whose controller evolves while it runs. The plant (`plant(PlantInput) -> PlantState`, record ABI) and the controllers (`policy(error, velocity) -> f64`) are IRIS; candidates and, with `--search`, a float search's winner are promoted through the seven gates between steps, and a promoted generation that breaks the actuator rating is rolled back in the same step. Worked example and observed run: [projects/evolving_cart_controller](../projects/evolving_cart_controller/README.md); covered by `tests/evolve_live_cli.rs`.
- **`iris evolve-serve`** ([`src/evolution/serve.rs`](../src/evolution/serve.rs)): serves a program's gateways over HTTP and promotes new versions of it while it runs. `POST /upgrade` takes a candidate's source through the seven gates against the operator's scenarios with traffic still flowing, and the promoted version takes over the live program state; `POST /rollback` restores the replaced version with the live state carried back. `ProgramCoordinator` gains `call` (serialised with every handoff, so no update is lost to a switch), `snapshot_state` and `rollback_carrying_state`, and `HotSwapEngine` gains `rollback_program_to_generation_with`. Worked example: [projects/self_upgrading_service](../projects/self_upgrading_service/README.md), an inventory service whose IRIS client runs on both backends; `tests/evolve_serve.rs` also checks a promotion under eight concurrent clients. Limits in [known issue #76](known-issues.md).
- **Differential tester** ([`tests/differential.rs`](../tests/differential.rs)): generates programs that combine features (records, enums with payloads, tuples, lists, maps, arrays, closures, floats, strings, loops with `break`, early `return`, program state) from a seed, runs each natively and in the interpreter, and requires identical output. A failing program is kept in `target/difftest/`; [`tools/difftest_reduce.py`](../tools/difftest_reduce.py) shrinks it to the lines that matter. It found twelve defects, all fixed (details in [known issues #82–#93](known-issues.md)):
  - LICM hoisted guarded divisions and indexing out of their guard (#82).
  - A parameter named like an LLVM temporary broke native builds (#83).
  - Floats printed differently on the two backends (#84); map iteration order differed between backends and runs (#85).
  - A closure over a fixed array corrupted the native heap (#86); a closure missed names used only inside an index (#87); a closure calling another closure failed type inference (#88).
  - A function could fall off its end without a value; this is now a compile error (#89).
  - A tag-only variant of a payload-carrying enum broke native builds (#90).
  - `val o: option<i64> = none()` did not take the annotation as its type (#91).
  - Strength reduction folded a float `x - x` to an integer zero (#92).
  - The interpreter stored string map keys with their quotes (#93).

  Regression tests: `tests/test_closure_capture_arrays.iris`, `tests/missing_return_value.rs`, `tests/test_differential_regressions.iris`.
- **AddressSanitizer builds:** `IRIS_SANITIZE=address` (any `-fsanitize=` value) builds native programs with the sanitizer on Linux and macOS: the runtime is compiled from source with it, generated functions are marked `sanitize_address`, and the link uses it. Leak reports are off by default (the runtime keeps some allocations for the process's life); `ASAN_OPTIONS=detect_leaks=1` turns them on. A new CI job, `sanitize`, runs the differential tester (seeds 1–60) and the corpus's both-backends check this way; [`tools/sanitize_linux.sh`](../tools/sanitize_linux.sh) runs the same job in a Docker container.
- **Standard-library reference tests:** every public stdlib function must be called by an asserting test or be listed in `tests/stdlib_untested.txt`, which may only shrink (`tests/stdlib_coverage.rs`). Before these tests 602 of 1,150 were untested; 536 are listed now. New reference tests check `std.crypto` against published vectors (MD5 from RFC 1321, SHA-1 from FIPS 180-4, AES-128 from FIPS-197 and SP 800-38A, RC4 from RFC 6229, the ChaCha20 quarter round from RFC 8439, base64 from RFC 4648, the WebSocket accept key from RFC 6455), `std.bitset` and `std.unicode` against hand-derived values, and `std.pcap` against a capture assembled byte by byte from the libpcap format and the IPv4, TCP and UDP RFCs.
- **Faster inner loop.** A small program builds natively in about 185 ms, down from about 640 ms: when the LLVM-C library is present the program is compiled in process (15 ms where starting clang cost 200), with the same `default<O3>` pipeline and AVX2/FMA features clang was given -- a 60M-iteration benchmark runs at the same speed either way -- and the prebuilt runtime objects are cached once per compiler version instead of being written, with 1 MB of C sources they did not need, into every build directory. Clang still compiles `-g` and sanitizer builds, and anything with `IRIS_USE_CLANG=1`. The interpreter is about twice as fast (a loop-and-call benchmark: 12.4 s to 6.1 s on a debug build, 1.05 s on release). `iris check` answers in about 75 ms for a 400-line program on a release build. `IRIS_VERBOSE=1` reports each build step and its time.
- **Diagnostics that teach.** Errors name the mistake in source terms, point at the exact token, and carry a fix where the intent is clear: `pritnln` -> `println`, `range(10)` -> `0..10`, `True` -> `true`, `fn` -> `def`, `if x = 2` -> `==`, `i++`, `elif`, `and`/`or`, single-quoted strings, `"n = " + 3` -> `to_str(3)`, `1 / 2.0` -> `1.0`, a misspelt or missing record field, `s.size()` -> `len(s)`, an unknown type (`integer` -> `i64`, `String` -> `str`), `bring std.mathz` -> `std.math`, a function from a standard module that is not brought. Fixes render as the corrected line, appear in `--error-format json` as `suggested_replacement`, and are quick fixes in the editor. Every error is reported -- all parse errors and one per function -- not only the first. `tests/diagnostic_quality.rs` keeps 39 common mistakes located (100%), explained (`iris explain` has an entry for every code the compiler emits, enforced by a source scan) and fixable where possible (58%), with no compiler internals shown.
- **Checks that were missing.** Calling a function with the wrong number of arguments (E0109) or an argument of the wrong type (E0110), assigning to a `val` (E0112), an unknown type name (E0111), an unknown standard module (E0115), a function that can end without returning (E0116), a `return` without a value (E0209), and an `if` without `else` used as a value (E0119) were all accepted, or failed later as an internal error. Type errors found after lowering now point at the expression (E0208 for mismatched branches, E0209 for return values) instead of naming IR values, and a non-exhaustive `when` names the variants it misses (E0210). Defects in IRIS itself are reported as "internal compiler error" (E0900) with `iris bug-report`, never as IR detail.
- **Newlines end statements; the return type is optional.** `push(xs, 1)` followed by `push(xs, 2)` on the next line is two statements -- the most common reason a correct-looking program failed to parse. A `(` at the start of a line no longer calls the value on the line before. `def f(x: i64) { ... }` returns `unit`.
- **New commands:** `iris check` (every error, no build), `iris watch` (run, test or check again on every save), `iris doctor` (the toolchain, end to end, with fixes), `iris new` (templates: app, cli, server, ros2, controller, each with passing tests), `iris bug-report`. `iris --help` is generated from the command definitions (it lacked `evolve-live` and `evolve-serve`), and `iris <command> --help` shows that command's options.
- **No silent fallback.** `--emit eval` used to rerun a program in the interpreter whenever the native build or run failed, so a native-only crash looked like a pass. It now reports the failure, and falls back only when no native toolchain exists (and says so). Removing the fallback exposed four native defects, fixed: `list_insert` stored a raw value where the runtime expects a boxed one (an access violation on every call); `gc_stats()` returned a boxed map (`map_len` read 0); an `extern def` of a C library function (`fabs`) was replaced by an aborting stub; and a list copied from an integer list (`list_slice`, `list_concat`) shared pointers into the original's storage, so freeing either corrupted the heap. Recursive choice types (`choice Nat { Zero, Succ(Nat) }`) failed natively too: their payloads were typed as records. A failed `assert` or panic now exits with code 101 instead of a crash code, and keeps the program's output; `iris run` builds into a temporary directory and prints only the program's output (`IRIS_VERBOSE=1` shows the build steps).
- **Flagship: a ROS 2 robot whose controller evolves while it drives** ([projects/evolving_turtle](../projects/evolving_turtle/README.md)). turtlesim's turtle is the plant of `iris evolve-live`: `ros2_plant.iris` publishes each command on `/turtle1/cmd_vel` and returns the next `/turtle1/pose`, keeping its ROS 2 connections in program state. In one command (`run.ps1` / `run.sh`) an allocating candidate is refused by the allocation-free proof, the unclamped LQR law is promoted and rolled back in the same step when it commands 9.4 units/s against a 2 units/s limit, the saturated law is promoted and settles the later moves in about two seconds, and the audit chain is verified. Verified on Windows (ROS 2 Humble, Cyclone DDS) and Linux (ROS 2 Humble, Fast DDS, headless turtlesim); `tests/evolving_turtle.rs` runs it wherever turtlesim and the bridge are available.
- **Awaitable sockets:** `await tcp_accept_async(listener, read_ms, write_ms)` and `await tcp_read_async(stream, max_bytes)` (`std.net`) wait for a connection or data without blocking the thread, so one thread serves many connections alongside its timers and channels. They are built on a new builtin, `async_readable(handle)`, a task that completes when the socket can be read; both executors (native and interpreter) wait on it with `poll` (`WSAPoll` on Windows). `tests/test_async_sockets.iris` serves two clients on one thread and answers the later one first, on both backends; a blocking server answers them in the other order. [projects/async_gateway](../projects/async_gateway/README.md) now runs entirely on one thread: its worker thread for accepting and reading is gone, and `tests/async_gateway.rs` still passes on both backends (64 concurrent requests, all in flight at once natively). Writes, connects and the HTTP client still block ([known issue #72](known-issues.md)).
- **Interpreter speed, measured:** the release-build interpreter runs about ten times faster than the debug build, and native code 10–25 times faster again; figures for four programs are in [known issues, capability boundaries](known-issues.md#rc1-capability-boundaries-2026-09-01). The interpreter now keeps a frame's values in a vector indexed by value id instead of a hash map: 24% faster on a tight loop, 6–15% on the others.
- **A failing `assert` says which one failed:** `assertion failed: a == 2 at tests/check.iris:4:5`, on both backends, including inside a function that was inlined. It used to say only `assertion failed`: natively with no position, interpreted at the call site of an inlined function. The parser adds the condition and position when it compiles a file; `assert(cond, message)` also takes a message of your own, evaluated only when the assert fails. Covered by `tests/assert_location.rs`.
- **Fixed:** on Linux, LLVM ORC (the JIT behind `evolve-live`, `evolve-serve` and the ORC halves of the ROS 2 tests) looked only for LLVM 23's library names, so with a distribution LLVM (Ubuntu's `libLLVM-18.so.1`) it was unavailable and those tests quietly ran natively only. It now tries the versioned names of LLVM 15 to 23, newest first, and `tests/ros2_live.rs` says when it skips ORC. Native builds on Linux no longer print "ld.lld link failed ... falling back to clang" (direct ld.lld linking is MinGW-only; clang is simply the linker there).
- **Fixed:** a native server on Windows could listen on a port another server was already listening on (`SO_REUSEADDR`), and the two split its connections; a busy port now fails to listen, as on Unix and in the interpreter ([known issue #95](known-issues.md)).
- **Contextual keywords:** `record`, `model`, `mod`, `effect`, `loop`, `spawn`, `par`, `defer`, `yield`, `select`, `to` and `by` introduce their constructs and are ordinary names everywhere else: `var record = 3`, `val to = 7`, `x.model`, `def select(xs, by: i64)`, a field called `loop`. Each was a reserved word, and a variable called `record`, `model`, `to` or `select` was a compile error that ordinary programs ran into. The rules are in [SPEC §2.3](SPEC.md#23-keywords); `tests/test_contextual_keywords.iris` uses each word as a name next to its construct, on both backends.
- **Fixed:** `unwrap` on an `err` or `none` now panics on both backends with the same message, including the error (`unwrap called on err: bad thing`). Natively it called `abort()` without flushing, so a program's earlier output was lost when stdout was a pipe; the interpreter reported "not yet supported — result_unwrap called on err".
- **Fixed:** `std.unicode` treated every byte from 0x80 as a UTF-8 continuation byte, so lead bytes were not counted and `codepoint_count("héllo")` was 4. `category` called Latin-1 letters `Lo` (À is `Lu`, é is `Ll`) and newline and tab `Zs` (they are `Cc`).
- **Fixed:** a user function named like a C library symbol (`send`, `free`, `log`) broke native linking on Windows and could replace the library's symbol for the runtime elsewhere. Native executables now name user functions `iris.<name>` ([known issue #75](known-issues.md); `tests/native_symbol_namespace.rs`).
- **Fixed:** `ProgramCoordinator::rollback` left the rolled-back generation's IR as the canary's baseline and handoff plan, so the next promotion was judged against code that was no longer running.
- **Program state on microcontrollers:** `iris embedded` accepts top-level `var`s of scalar type (`i64`, `f64`, `bool`) as globals in `.bss`, so a step function called repeatedly by the board support package can remember things between calls. The entry initialises them on its first call (bare-metal startup runs no constructors), and the proof covers that initialiser; other state types are rejected, and `prove_allocation_free` (the evolution gate) still refuses state. Worked example: [projects/uno_controls](../projects/uno_controls/README.md), with a toggle, a debounced four-mode lamp and a fixed-point PID, simulated on both backends and proven for the Uno (not yet flashed). See [docs/embedded.md](embedded.md).
- **LLVM backend fallback:** when the preferred LLVM-C library lacks a target's backend (the stock LLVM 23.1.1 Windows package has no AVR), object emission uses the first other installed LLVM-C that has it. `iris embedded --target arduino-uno` now works with LLVM 17 installed beside 23.1.1 and no `IRIS_LLVM_C_API`.
- **Fixed:** embedded objects referenced the hosted runtime's `iris_unhandled_effect_abort` through the weak fallbacks emitted for `extern` functions, so a firmware link would fail with an undefined symbol (since 2026-09-12). Bare-metal targets now get plain declarations ([known issue #77](known-issues.md)).
- [projects/async_gateway](../projects/async_gateway/README.md): an HTTP service whose requests are `async def` tasks on one thread, with per-request fan-out, deadlines (`timeout_after`), cancellation (`task_cancel`, `cancel_all`) and a first-answer race (`async_any`). `tests/async_gateway.rs` sends 64 concurrent requests to it on both backends; natively all 64 were in flight at once and answered in about 180 ms.
- `std.net`: `tcp_listen_local` and `tcp_listen_local_checked` listen on 127.0.0.1 only (runtime `iris_tcp_listen_loopback`, both backends). The `std.net` reference listed four functions that do not exist (`tcp_set_timeouts`, `tcp_write_exact`, `tcp_shutdown`, `tcp_close_stream`); it now lists the real ones, and its example runs.
- [projects/cli_tools](../projects/cli_tools/README.md): `csvq` (RFC 4180 CSV and JSON Lines: filter, select, exact grouped sums, CSV or JSON out), `logstat` (access-log analysis), `pcapinspect` (protocols, flows, top talker, SYN scans) and `tlsfp` (JA3/JA4 of every ClientHello, with an optional list of known fingerprints). `tests/cli_tools.rs` runs each on both backends and compares with an independent Python implementation (`samples/make_samples.py`), including ClientHellos captured from OpenSSL.
- **Program arguments:** `iris run tool.iris -- a b` and `iris tool.iris --emit eval -- a b` pass arguments to the program, and `args()` returns the program followed by its arguments on both backends. The interpreter used to return `iris.exe`'s own command line, and `iris run` passed nothing.
- `file_read_bytes(path) -> list<i64>`: a file's bytes, NULs included (`file_read_all` stops at the first NUL natively); empty if unreadable. `std.crypto` gains `md5_bytes`, `md5_str` and `md5_hex` (checked against the RFC 1321 vectors), for identifiers defined with MD5 such as JA3.
- `std.pcap` reads captures in either byte order and with nanosecond timestamps, adds `pcap_read_packet` and `dissect_udp`, and stops at a datagram's IPv4 total length.
- **Fixed:** `std.pcap` accepted only big-endian captures, so it rejected every capture tcpdump or Wireshark writes on x86; and short Ethernet frames' padding was read as TCP payload.
- **Fixed:** `std.tls` computed a "JA3" from three fields hashed with SHA-256, so no hash it printed could match a published JA3. It now follows Salesforce's definition (version, ciphers, extensions, groups, point formats; MD5). JA4 now takes the version from `supported_versions` (TLS 1.3 clients read as `12` before), includes the signature algorithms in part c, applies FoxIO's rule for non-alphanumeric ALPN values, and sets `d`/`i` from the SNI extension's presence. `TlsClientHello` gains `supported_groups`, `ec_point_formats`, `signature_algorithms`, `supported_versions` and `alpn_bytes`. `matched_threat` remains a coarse heuristic over the cipher and extension lists, not a lookup in a fingerprint database.
- **Fixed:** arrays of heap values (`["a", "b"]`, arrays of records) were empty in native code, so every load failed its bounds check ([known issue #79](known-issues.md)).
- **Fixed:** strings from `split` and `str_chars` were freed with their list even when a record field still held them, and the memory was reused: a log parser's `path` field read `"0"` natively ([known issue #80](known-issues.md)).
- **Fixed:** the native run behind `--emit eval` did not read the program's output until it exited, so a program printing more than a pipe holds blocked, timed out after 15 s, and silently fell back to the interpreter.
- [projects/llm_agent](../projects/llm_agent/README.md): a tool-calling agent whose tools are IRIS functions with typed arguments and results; arguments are decoded and checked into records before anything runs, and failures go back to the model as typed errors. `tests/llm_agent.rs` runs it on both backends against a mock OpenAI-compatible server and checks every request; it was also run against Mistral 7B in a local Ollama.
- `std.llm` tool calling: `LlmMessage` carries `tool_calls_json` and `tool_call_id`; `llm_tool_result`, `llm_assistant_turn` and `llm_tool_calls` (typed `LlmToolCall` with id, name and arguments) complete the loop the chat-completions protocol needs. **Fixed:** a response that only called tools reported its content as `"null"`.
- [projects/gridworld_inference](../projects/gridworld_inference/README.md): an active-inference agent that learns an unknown grid online (Dirichlet transition model with forgetting; actions by expected free energy with risk and a novelty term), reaches the best possible policy, and relearns when a wall appears mid-run. Compared with a random walk and the optimum; identical decisions on both backends.
- **Fixed:** `std.ais`'s `active_inference_step` returned action 0 for every model: its expected-free-energy loop never used the transitions or any preferences. It now computes the one-step expected free energy from the model (risk plus ambiguity), with uniform preferences; `model_expected_free_energy` and `active_inference_choose` take preferences ([known issue #81](known-issues.md); `tests/test_ais_active_inference.iris`).
- **Fixed:** global `const`s were lowered in hash order, so the IR, and with it the embedded proof fingerprint, changed between builds of the same source. They are now lowered in name order ([known issue #78](known-issues.md)).
- Projects built on the rclcpp-backed `std.ros2`: [turtlesim_autopilot](../projects/turtlesim_autopilot/README.md) (services, an action, topics, live-tunable parameters and tf2 against a running turtlesim) and [lifecycle_sensor_fusion](../projects/lifecycle_sensor_fusion/README.md) (an rclcpp_lifecycle node driven by itself or by `ros2 lifecycle`). Each has an offline `simulate.iris` checked on both backends.
- **ROS 2 on the real client stack:** `std.ros2` now runs on rclcpp, rclcpp_action, rclcpp_lifecycle and tf2_ros through a C++ bridge ([`src/runtime/ros2`](../src/runtime/ros2), built with `python build_ros2_bridge.py`). Any installed message type can be published, subscribed and built from JSON, with exact `f64`s and field paths (`"ranges[3]"`, `"header.stamp"`); services, clients, action servers and action clients run on rclcpp's own base classes; parameters are the node's rclcpp parameters (ranges and read-only descriptors enforced); lifecycle nodes are rclcpp_lifecycle nodes; tf2 buffers are fed by a TransformListener and written by tf2_ros broadcasters; timers follow the node's clock. rclcpp callbacks become events that `spin_once(ctx, timeout_ms)` returns. Verified against ROS 2 Humble on Windows with Cyclone DDS: `tests/ros2_live.rs` (native and ORC) and [`tools/ros2/interop_check.py`](../tools/ros2/interop_check.py), whose 16 checks include `ros2 topic echo/service call/param set/lifecycle set/action send_goal`, `tf2_echo`, and the C++ `talker`, `add_two_ints_server`, `fibonacci_action_server` and `static_transform_publisher`. Also built and verified on Linux (ROS 2 Humble, Ubuntu 22.04, Fast DDS) on 2026-10-20 with [`tools/ros2/linux_check.sh`](../tools/ros2/linux_check.sh): the live tests native and through ORC, and all 16 interop checks. Details and limits in [docs/ros2-build.md](ros2-build.md).
- **Program state:** `var name: Type = value` at the top level declares a variable that lives as long as the running program: initialised before `main` (natively by a constructor, in the interpreter on first use), readable and writable from every function, including record field updates (`stats.count = stats.count + 1`). Native programs keep it in LLVM globals with the GC's reference counts; the interpreter keeps it per module and process, so spawned threads share it as native code does. Covered by `tests/test_program_state.iris` on both backends.
- **Governed whole-program evolution:** `iris evolve --manifest program.json --scenarios scenarios.json` promotes a program with several gateways through the seven gates ([`src/evolution/program.rs`](../src/evolution/program.rs)). Scenarios start from the program's state (optionally overridden by name) and call gateways in order; results are compared as JSON with the replaced generation's, and the constitution judges every step. At activation the live generation's state is read natively and written into the candidate before the atomic switch, by name and type; new variables start fresh, removed ones are dropped, and a type change is refused. Rollback returns to the replaced generation with its own state. Covered by `tests/evolution_program.rs` and `tests/evolution_cli.rs`.
- **Record policies:** `iris evolve --abi record` promotes `def policy(s: In) -> Out` where both are records of `f64`, `i64` and `bool` fields; canary cases and constitution checks are field by field (`--bound force=-5:5`), and the promoted policy is served natively with `HotSwapLease::call_record`. Covered by `tests/evolution_record_abi.rs`.
- **Float search that promotes:** `iris evolve-search --abi f64:N` evolves control laws over N `f64` inputs ([`src/evolution/float_search.rs`](../src/evolution/float_search.rs)) — seeded with the least-squares affine law, scored under linear scaling, with constant polishing — and `--promote` sends the winner through the same seven gates as a hand-written candidate (it can carry the allocation-free proof). `iris evolve` and `evolve-search --promote` now share one promotion path.
- **True async:** `async def` bodies compile into resumable state machines ([`src/pass/async_lower.rs`](../src/pass/async_lower.rs)) run by a cooperative executor on the calling thread, identically under the interpreter ([`src/interp/async_exec.rs`](../src/interp/async_exec.rs)) and natively (`iris_runtime.c`). An `await` inside an `async def` suspends only that task: values live across it are kept in the task's frame, and the thread runs other tasks meanwhile. 20,000 concurrent tasks run in under a second natively. New builtins: `async_sleep(ms)` (timers complete in deadline order), `async_yield()`, `async_recv(ch)` (also what `await ch` does inside an `async def`: receive without blocking the thread), `async_any(t1, t2, ...)` or `async_any(list)` (select: the index of the first task to finish; losing timers and receives are cancelled, so a losing receive never takes a value), `task_done(t)`, `task_cancel(t)` and `task_cancelled(t)`. `std.async` adds `timeout_after[T]`, `join_all[T]` and `cancel_all[T]`. Idle executors sleep until the next timer or until any thread sends on a channel, and a blocking `recv` keeps the waiting thread's tasks running. Natively, a task is freed once the program and executor are done with it (the compiler releases a handle used exactly once, by an `await` or `async_any`): 200,000 loop iterations with three awaits each run in constant memory. `task<T>` is a type you can write (`list<task<i64>>`), and generic `async def`s infer `T` from a `task<T>` argument. Covered by `tests/test_async_tasks.iris` (same output on both backends) and `tests/async_await.rs`; limits in [known issue #72](known-issues.md).
- `control_f64` evolution ABI ([`src/evolution/mod.rs`](../src/evolution/mod.rs)): policies of the form `(f64 × N) -> f64`, N = 1..8, promoted through the same seven gates as `(i64) -> i64` via `evaluate_and_promote_control` / `observe_or_rollback_control`. Non-finite outputs always fail the constitution. Records and collections are excluded on purpose: they are heap-backed in IR and could never carry the allocation-free proof.
- `EvolutionPolicy.require_allocation_free`: the `resource_and_abi` gate now optionally requires `prove_allocation_free` ([`src/codegen/embedded.rs`](../src/codegen/embedded.rs)), the embedded backend's IR-level proof (no heap, host services or non-tail recursion), generalised to any scalar entry. New policy fields are omitted at their defaults, so existing policy hashes in audit logs are unchanged.
- `examples/evolving_controller.rs`: a cart controller evolved live under the proof — an allocating and a NaN-producing candidate are rejected, an unsaturated PD is promoted and then rolled back on a field violation, a saturated PD becomes active. Covered by `tests/evolution_control_abi.rs`.
- Native source-level debugging: `iris build -g` emits DWARF line tables, function scopes and scalar parameters mapped to `.iris` files (including brought modules, via a per-build source map) at `-O0`, debuggable with gdb/lldb. `iris dap` launch configurations accept `"native": true` to relay the session to GDB's DAP mode ([`src/dap_native.rs`](../src/dap_native.rs)); the VS Code extension has a "Debug Native Binary" snippet. The lowerer now records a line position for every instruction (`SpanTable::line_spans`), re-attached after the debug passes move code. Covered by `tests/native_debug_info.rs` and `tests/dap_native.rs`; all 40 buildable examples build with `-g` and produce the same output.
- `iris evolve` gains `--abi i64|f64:N`, `--require-allocation-free` and `--validate interpreter|sandbox`. Sandbox validation compiles one native harness (tests, rollback probe, canary, with bit-exact `f64` transport) and runs it in the zero-capability worker sandbox, failing closed where none is enforced.
- **No more silent `i64`:** a used value whose type nothing in the program determines is now error E0207 ("type annotations needed"), pointing at the statement that produced it ([architecture note §4.1](architecture-vs-rustc.md)). Unread unknown parts of compounds (`ok(5)`'s error type, an unused `list()`) stay legal. The whole corpus, every stdlib module and all projects compile unchanged; `list_pop` gained the inference constraint it was missing. `IRIS_INFER_DEFAULTS=allow|report` restores or audits the old behaviour.
- **`dyn Trait` everywhere it is expected:** a concrete record now becomes a trait object as a function's tail value, at an early `return`, in each `if` branch of a returned value, and in a record field (whose method list used to be empty because records are registered before traits). A record that does not implement the trait is a compile error (E0108).
- **Native debugger locals:** `iris build -g` describes `val`/`var` bindings, loop-carried `var`s and `str` values (shown as text), and keeps parameters visible after their last use. The preprocessor now replaces directive and inactive lines with blanks of the same byte length, so line numbers and byte offsets after `#define`/`#if` match the source (fixing both debug line tables and diagnostics).
- **Linux and macOS worker sandboxes:** Linux uses Landlock (filesystem, and TCP on ABI 4+) plus seccomp-bpf (no sockets, no new processes, no signals to other processes, no namespaces, mounts, io_uring or BPF) and rlimits, applied between `fork` and `exec`; verified in a Linux container. macOS uses a deny-default Seatbelt profile via `sandbox-exec` with rlimits and a resident-memory-sampling supervisor; it is exercised only by the macOS CI job. Both fail closed when their mechanism is unavailable. CI now installs gdb on Linux and requires the debugger and sandbox tests to run rather than skip.
- Language server: hover documentation for every keyword, scalar type and contextual word ([`src/lsp_docs.rs`](../src/lsp_docs.rs)) with explanation, idiom and an example that `cargo test` compiles and runs; completions carry the docs; 17 missing keywords added to completion; math builtin hovers show their generic signatures.
- CLI: `--sandbox`, `--no-cache`, `--strict-effects` and `--error-format` are global, so they may follow the subcommand (`iris run --sandbox file.iris`). `-o` and `--target` remain top-level only because `embedded`/`docs` define their own.
- VS Code extension 1.0.7 (see `vscode-iris/CHANGELOG.md`).
- Hot-swap leases resolve the entry address once at install: a policy call drops from ~5 µs to ~0.1 µs (debug build).
- **Fixed:** Windows native binaries imported `libwinpthread-1.dll` and so failed to start without MSYS2 on PATH (and always inside an AppContainer, `STATUS_DLL_NOT_FOUND`). winpthread is now linked statically when `libwinpthread.a` is present.
- **Fixed:** under `--strict-effects`, `effect pure` was treated as an effect tag named `pure` — it leaked into callers' rows (`main has effects alloc, pure`) and produced a spurious "declares effect `pure` that the body doesn't use" warning.
- Native hexadecimal (`0x...`) and binary (`0b...`) integer literals with optional digit separators (`_`) in [`src/parser/lexer.rs`](file:///c:/Users/Moon/Desktop/Projects/IRIS/src/parser/lexer.rs).
- Comprehensive binary byte buffer library (`std.buffer`, [`src/stdlib/buffer.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/src/stdlib/buffer.iris)): Safe, length-tracked octet buffers handling arbitrary binary protocols with embedded null bytes (`0x00`) without truncation, hex parsing (`from_hex`), string ingest (`from_str`), lossy ASCII conversion (`to_ascii_lossy`), buffer slicing (`slice`), concatenation (`concat`), binary subsequence search (`find`, `contains`), and both big-endian and little-endian integers (`u16_be`, `u32_be`, `u16_le`, `u32_le`).
- Pure IRIS Cryptography & Block Ciphers (`std.crypto`, [`src/stdlib/crypto.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/src/stdlib/crypto.iris)): Implemented native symmetric RC4 stream cipher (`rc4_crypt_bytes`, `rc4_crypt_buffer`, `rc4_crypt_str`), RFC 7539 ChaCha20 32-bit Quarter-Round transformation (`chacha20_quarter_round`, `rotl32`), side-channel resistant constant-time byte and string comparisons (`timing_safe_equal`, `bytes_timing_safe_equal`), pure IRIS SHA-1 (RFC 3174) cryptographic hash function (`sha1_bytes`, `sha1_buffer`, `sha1_str`), binary-safe Base64 encoder (`bytes_to_base64`), RFC 6455 WebSocket handshake accept key derivation (`websocket_accept_key`), and full NIST FIPS 197 AES-128 symmetric block cipher implementation with 10-round key expansion (`aes128_key_expansion`), Galois Field $GF(2^8)$ multiplication (`aes_xtime`, `aes_gf_mul`), forward/inverse S-box substitutions, ECB block encryption/decryption (`aes128_encrypt_block`, `aes128_decrypt_block`), and Cipher Block Chaining (CBC) with PKCS#7 padding and constant-time unpadding validation (`aes128_encrypt_cbc`, `aes128_decrypt_cbc`).
- Interactive WebSocket Protocol & Streaming Server (`std.net`, [`src/stdlib/net.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/src/stdlib/net.iris)): Added `tcp_read_bytes` and `tcp_write_bytes` bridging sockets directly to `ByteBuffer`. Implemented RFC 6455 HTTP Upgrade handshake extraction and server response negotiation (`ws_extract_key`, `ws_server_handshake`), RFC 6455 frame encoding/decoding (`ws_encode_frame`, `ws_decode_frame`), 4-byte XOR payload unmasking (`ws_unmask_payload`), and high-level bidirectional streaming helpers (`ws_send_text`, `ws_send_binary`, `ws_send_ping`, `ws_send_pong`).
- Offline PCAP Threat Hunter Tool (`tools/pcap_hunt.iris`, [`src/stdlib/pcap.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/src/stdlib/pcap.iris)): Standalone offline network forensics utility dissecting Libpcap 2.4 capture files, reconstructing Ethernet II frames (`dissect_ethernet`), IPv4 packets (`dissect_ipv4`), and TCP segments (`dissect_tcp`). Inspects multi-layer network streams for MITRE ATT&CK adversarial techniques, identifying SYN Port Sweeps (T1046), Log4Shell JNDI RCE (T1190), Apache Struts2 OGNL RCE (T1190), Spring4Shell ClassLoader RCE (T1190), GNU Bash Shellshock (T1059), and Apache Path Traversal (T1083), generating structured forensic audit scorecards.
- Extended TLS 1.3 / 1.2 JA4+ Fingerprinting (`std.tls`, [`src/stdlib/tls.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/src/stdlib/tls.iris)): TLS 1.2/1.3 Record and ClientHello handshake dissection (`tls_parse_record`, `tls_parse_client_hello`), Server Name Indication (SNI) extraction, ALPN protocol negotiation parsing (`0x0010`), GREASE filtering, JA3 attacker tool fingerprinting (`tls_calculate_ja3`), and advanced JA4+ transport fingerprinting (`tls_calculate_ja4`) generating 10-character protocol descriptors (`ja4_a`), sorted hex cipher hashes (`ja4_b`), and sorted extension hashes (`ja4_c`) with threat database matching against Cobalt Strike, Metasploit, Python requests, and legitimate HTTP/2 browsers.
- Live Network Server & Inline IDS Exploit Defense in Aegis-IRIS ([`projects/intrusion_detection_system/`](file:///c:/Users/Moon/Desktop/Projects/IRIS/projects/intrusion_detection_system/)): Integrated real OS loopback socket serving (`127.0.0.1:18088`), non-blocking Slowloris socket starvation timeout eviction, and Stage 0.5 `std.tls` JA3/JA4 exploit tool detection.
- Comprehensive Standard Library Maturation (`std.math`, `std.time`, `std.collections`, `std.queue`, `std.string`, `std.json`, `std.bitset`, `std.path`, `std.deque`, `std.heap`, `std.set`):
  - `std.math` ([`src/stdlib/math.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/src/stdlib/math.iris)): Added fundamental mathematical constants (`PI`, `TAU`, `E`, `SQRT2`, `LN2`, `LN10`), floating-point utilities (`deg_to_rad`, `rad_to_deg`, `atan2`, `hypot`, `sinh`, `cosh`, `tanh`, `lerp`, `inv_lerp`, `remap`, `smoothstep`, `is_close`, `round_to_decimals`), number theory and combinatorics (`is_prime`, `next_prime`, `factorial`, `permutations`, `combinations`, `is_power_of_two`, `next_power_of_two`, `integer_sqrt`, `pow_i64`), and list statistics (`sum_f64`, `mean`, `variance`, `std_dev`, `min_in_list`, `max_in_list`).
  - `std.time` ([`src/stdlib/time.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/src/stdlib/time.iris)): Added `Duration` & `DateTime` records, Gregorian civil calendar epoch conversions (`epoch_ms_to_datetime`, `datetime_to_epoch_s`), leap year/month rules, ISO 8601 formatting (`format_iso8601`), date formatting (`format_date`), and duration arithmetic.
  - `std.collections` ([`src/stdlib/collections.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/src/stdlib/collections.iris)): Added list algorithms (`list_reverse`, `list_unique`, `list_count`, `list_all_equal`, `list_sum`, `list_min`, `list_max`, `list_take`, `list_drop`, `list_swap`, `list_flatten`) and map utilities (`map_values_list`, `map_clone`, `map_has_key`).
  - `std.queue` ([`src/stdlib/queue.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/src/stdlib/queue.iris)): Added compaction (`queue_compact`), list conversions (`queue_to_list`, `queue_from_list`), and membership queries (`queue_contains`).
  - `std.string` ([`src/stdlib/string.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/src/stdlib/string.iris)): Added `capitalize`, `truncate`, `truncate_with`, `reverse_str`, ASCII character classification predicates (`is_ascii_digit`, `is_ascii_alphabetic`, etc.), dynamic programming `levenshtein_distance`, and `hamming_distance`.
  - `std.json` ([`src/stdlib/json.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/src/stdlib/json.iris)): Added RFC 8259 character escaping (`json_escape`), JSON literal formatting (`json_bool`, `json_num_i64`, `json_num_f64`, `json_null`), and typed getters/setters (`json_set_bool`, `json_set_i64`, `json_set_f64`, `json_get_bool`).
  - `std.bitset` ([`src/stdlib/bitset.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/src/stdlib/bitset.iris)): Added set algebra (`bitset_union`, `bitset_intersection`, `bitset_difference`, `bitset_xor`), `toggle_bit`, subset and intersection predicates (`bitset_is_subset`, `bitset_intersects`), clone, and bidirectional index list conversions (`bitset_from_indices`, `bitset_to_indices`, `bitset_max_bit`).
  - `std.path` ([`src/stdlib/path.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/src/stdlib/path.iris)): Added POSIX path normalization for Windows backslashes (`path_to_posix`), multi-segment joining (`path_join_all`), extension validation (`path_has_extension`), parent directory query (`path_parent`), and wildcard glob matching (`path_matches_glob`).
  - `std.deque` ([`src/stdlib/deque.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/src/stdlib/deque.iris)): Renamed `len` to `deque_size` and `is_empty` to `deque_is_empty` to avoid shadowing core builtins, added list conversions (`deque_to_list`, `deque_from_list`), `deque_contains`, `deque_clear`, and cyclic rotations (`deque_rotate_left`, `deque_rotate_right`).
  - `std.heap` ([`src/stdlib/heap.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/src/stdlib/heap.iris)): Added empty-list safe construction in `heap_from_list`, `heap_contains`, and `heap_clear`.
  - `std.set` ([`src/stdlib/set.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/src/stdlib/set.iris)): Added empty-list safe construction in `set_from_list`, `set_clone`, and `set_clear`.
  - Testbench ([`tests/test_mature_stdlibs.iris`](file:///c:/Users/Moon/Desktop/Projects/IRIS/tests/test_mature_stdlibs.iris)): Added full end-to-end unit test suite validating all 11 standard library domains.
- Continuous Background Learning & In-Flight Model Hot-Swapping ([`projects/continuous_background_learning/`](file:///c:/Users/Moon/Desktop/Projects/IRIS/projects/continuous_background_learning/)): Implemented end-to-end continuous learning architecture solving the multi-episode pause limitation from `docs/direction.md`. Features a 50 Hz real-time front-end control loop tracking reference trajectories without frame drops or halts, an asynchronous concurrent background parameter identification worker (`spawn`) ingesting streaming state transitions over channels, online Normalized LMS gradient descent with deadband noise immunity identifying physical plant drift (mass $\times 2.8$, friction $\times 3.6$, bias $+0.85$ Nm), zero-downtime model hot-swapping in-flight, a 2D ASCII real-time oscilloscope display, and mission debriefing scorecards.
- IRIS Mission Intelligence Research Prototype ([`projects/mission_intelligence/`](file:///c:/Users/Moon/Desktop/Projects/IRIS/projects/mission_intelligence/)): Autonomous scientific discovery and spacecraft decision pipeline targeted for 2029 mission readiness (Eclipse Softworks). Implements 4-stage architecture: Keplerian orbital simulation with realistic sensor noise and injected anomalies (plasma flares, thermal degradation, sensor dropouts), autonomous planetary dipole equation recovery ($B(r) = k \cdot r^p$ with $R^2 = 0.9997, p = -2.981$), 3.8-sigma residual anomaly detection, active epistemic experiment planning (+181% scientific yield advantage over fixed schedules), and an independent 4-gate hard safety governor guaranteeing zero flight hardware breaches.

### Changed

- `std.ros2`'s IRIS-side stand-ins are gone in favour of the real ROS 2 objects: `lifecycle_new` / `lifecycle_transition` / `lifecycle_can_publish` and the `LifecycleNode` record are replaced by `lifecycle_node_create` and `lifecycle_configure`/`activate`/`deactivate`/`cleanup`/`shutdown` on an rclcpp_lifecycle node, whose transition ids are lifecycle_msgs' (configure 1, cleanup 2, activate 3, deactivate 4); `TransformBuffer` is now a handle to a tf2 `BufferCore` (same functions; lookups now chain through intermediate frames). The typed `publish_*`/`take_*` helpers carry exact doubles instead of 1e8-scaled integers; the `iris_rcl_*` C functions keep their scaled contract. The old C bridge (`ros2_bridge.c`), its CRT shim and `build_ros2_dll.py`/`build_ros2.bat` are removed.
- `ffi_open` (interpreter and native runtime) no longer prepends one machine's ROS 2 install to `PATH` or forces `AMENT_PREFIX_PATH`; ROS 2 programs run in a sourced environment, or with the ROS 2 `bin` directory on `PATH`.
- Calling an `async def` returns a `task<T>`, not a `chan<T>`, and no longer runs the body on a worker thread. `await` and `recv(t)` still yield the result; a value annotated `chan<T>` (for example `list<chan<i64>>`) holding tasks is now a type error that says to write `task<T>`. `async_worker_count()` / `async_queued_tasks()` now describe only the `spawn` workers.

### Fixed

- `std.math`'s `atan2` was a polynomial approximation off by up to 0.004 rad; three Newton steps on `y cos t - x sin t = 0` now make it exact to double precision (checked against libm at 14 points on both backends).
- A brought module's parameters and locals named like one of its own functions were compiled as references to the function: the merge renamed every identifier matching a module item to `module__name`, regardless of scope, so `def tag(label: str)` beside `def label(...)` failed with E0101 ("expected 'str' but found 'fn(...) -> str'"). The rename now respects parameters, `val`/`var`, loop variables, lambda parameters, `when` bindings, `select` bindings and handler parameters ([`src/compiler.rs`](../src/compiler.rs)); a call `name(...)` still reaches the module's function unless the local holds a closure, matching the lowerer. It also reaches `for ... step` expressions and `spawn(group)` expressions it used to skip. Covered by `test_bring_file_locals_shadow_module_functions`.
- The interpreter's `ffi_open` overwrote `AMENT_PREFIX_PATH` in the C runtime's environment even when a sourced ROS 2 environment had set it, hiding overlay workspaces from anything loaded through FFI.
- Sandboxed native validation (`iris evolve --validate sandbox`) failed with `STATUS_DLL_NOT_FOUND` on machines with OpenBLAS in `C:\openblas`: every native build linked it, and the AppContainer cannot load it. Validation harnesses are now built self-contained.
- Native programs lost everything they had printed when they panicked or failed a bounds check with stdout redirected: `abort()` does not flush stdio. The runtime now flushes stdout first.
- On Windows, native `to_str` of an `i64` beyond 32 bits was truncated (`6000000000` printed as `1705032704`), as was printing a boxed `i64`; `read_i64` stored only 4 bytes, so `-5` read as `4294967291`. The runtime formatted `int64_t` as `long`, which is 32 bits there; it now uses `PRId64`/`SCNd64`.
- Native programs were quadratic in their allocation count: the registry that tells heap strings from string constants was a flat array searched linearly on every string free and every `realloc`, and the refcount table had a fixed 4,096 buckets. A loop building 200,000 strings took 25 s (24.8 s of it in exit cleanup); it now takes 0.4 s. The registry is a hash set, the refcount table grows, and exit cleanup drains it linearly.
- The MSVC build of the runtime treated `pthread_cond_timedwait` as an immediate return, so a timed channel receive never timed out (it spun until a value arrived); the shim now waits on the Win32 condition variable and reports `ETIMEDOUT`. It also gained `pthread_cond_broadcast`.
- In-place record field mutations in loops ([`src/lower/mod.rs`](file:///c:/Users/Moon/Desktop/Projects/IRIS/src/lower/mod.rs)): Unwrapped `AstExpr::FieldAccess` chains down to the root identifier in `collect_rebound_vars_in_stmt`, ensuring base records persist mutations across loop iterations. Added compound assignment (`+=`, `-=`, etc.) lowering support on record fields.
- Parser recursion depth limit ([`src/parser/parse.rs`](file:///c:/Users/Moon/Desktop/Projects/IRIS/src/parser/parse.rs)): Raised `MAX_PARSER_DEPTH` from 16 to 32, eliminating premature `E0007: recursion limit exceeded` errors on deeply nested expressions and multi-branch `if-else` cascades.
- Spawn lowering removes unused lifted parameters and capture arguments. This
  prevents unused surrounding records from entering task capture ownership and
  fixes the learning scheduler's native heap-corruption regression. Capture
  ordering remains deterministic, with tests for nested tasks and shadowing.

## [1.0.0-rc2] — 2026-09-16

### Added

- Autonomous Evolutionary Lab & MAP-Elites search engine: `iris evolve search` with multi-dimensional behavioral descriptors, quality-diversity archives, genetic mutation operators, and polyglot code generation (IRIS, C, Python).
- High-Performance Scientific Simulation Suite (`tools/crunch/`): N-Body Gravitational Dynamics (`three_body.iris`), Spiral Galaxy Particle Collision (`galaxy_sim.iris`), 2D Navier-Stokes Aerodynamic Wind Tunnel (`wind_tunnel.iris`), with full CLI telemetry and visualization runner.
- Native Neural Dialogue Model & Machine Learning Suite (`tools/neural_dialogue/`): Character-level neural dialogue model using `std.ml` and `std.dataset`, autoregressive sampling, streaming inference, and interactive conversational loop.
- Real-world Biomedical & Epidemiological modeling (`examples/human_problems/`): Monte Carlo epidemic diffusion, antimicrobial peptide biophysics, GPU/SIMD genomic sequence alignment.
- Python C-ABI bindings and cross-language genome bridge (`crates/iris_ffi`, `bindings/python`).
- LLVM 23.1.1 release toolchains, version-aware Windows dependency upgrades,
  versioned LLVM-C discovery, and the `IRIS_LLVM_C_API` override.
  catalog checks. Added real localhost LLM verification and Rust ORC embedding
  examples alongside separately classified SDK/board integrations.

- VS Code Test Explorer discovery plus run/debug profiles for zero-argument
  `test_*` functions, backed by a DAP named-function launch entry.
- Rich LSP hovers for functions, traits, and algebraic effects, including
  documentation, generic signatures, declared/inferred effects, and explicit
  unsafe-region status.
- Exact-range dead/unused, unreachable, unsafe, and possible-infinite-loop
  diagnostics with semantic-token modifiers and LSP unnecessary tags.
- A compiler-sourced standard-library completion registry covering the current
  AIS, ML/NN/tensor, ROS 2, LLM, reflection, metaprogramming, and networking
  modules.
- In-process LLVM ORC JIT loading and ABI-verified, lease-safe hot swapping.
- Generation-conditional rollback with deterministic SHA-256 ABI fingerprints.
- Transactional speculative state and staged filesystem rollback.
- `iris evolve`, a bounded seven-gate promotion workflow with pinned
  constitutions, shadow canaries, automatic rollback API, and a tamper-evident
  JSONL audit chain.
- Allocation-free embedded bundles and physical Arduino Uno validation.
- Differentiable tensor/NN control-flow coverage and AIS/ROS 2 v2 examples.
- V2 whole-program evolution manifests with reviewed JSON, command, and scalar
  gateways; explicit JSON state handoff; atomic multi-gateway leases; and an
  immutable content-addressed candidate artifact store.
- Bounded eight-generation hot-swap history with exact-generation rollback.
- Mandatory persisted `--audit-head` verification for `iris evolve`.
- Enforced Windows native-worker sandbox: ephemeral zero-capability LPAC,
  opt-out from broad application-package access, suspended startup before Job
  assignment, memory/CPU/process limits, bounded stdout/stderr, wall-time
  termination, token attestation, and fail-closed unsupported platforms.
- Explicitly acknowledged unrestricted whole-program activation with isolated
  authority, v2 manifest/ABI validation, ORC materialization, atomic leases,
  generation history, and exact rollback.
- Compiler-hosted typed metaprogramming via `iris meta` and `std.meta`, including
  typed/effect/ABI/CFG metadata, IR emission, and fully recompiled UTF-8 edits.
- Typed TCP stream/listener APIs with timeouts, exact writes, bounded reads,
  shutdown/close, and result-based transport errors.
- Status-bearing HTTP requests with custom headers, bodies, timeouts, bounded
  responses, Windows WinHTTP TLS/certificate validation, and JSON path queries.
- `std.llm` provider-neutral chat, tool-call, embedding and local-model APIs.
- Production `std.ml` model session/registry/health lifecycle and `std.ais`
  degraded/emergency agent lifecycle.
- Fixed-size native async executor (maximum 32 workers) shared by `spawn` and
  lowered `async def`, cooperative queue helping during nested `await`, and
  scheduler telemetry through `std.async`.
- Structured task-group cancellation queries in `std.async`:
  `cancellation_requested()` for the current task and
  `task_group_cancelled(group)` for explicit group state.

### Changed

- `iris fmt` is now comment- and literal-preserving, parse-safe, idempotence
  tested, line-width aware, and configurable by the VS Code extension.
- Supported Windows MinGW native builds emit objects through LLVM-C and link
  directly with `ld.lld`; Clang is no longer the normal fallback for that path.
- Public examples use the locked `def`/`val`/`record`/`choice`/`bring` syntax and
  the earlier 116-entrypoint collection passed an IR compilation audit. The
  replacement learning catalog now checks native execution and interpreted
  results separately; see `examples/catalog.json` for current coverage.
- Extern functions accept explicit effect contracts; the AIS/tensor/NN/ML graph
  passes strict effect checking with deterministic deduplicated diagnostics.
- The complete Rust workspace passes `cargo clippy --all-targets -- -D warnings`
  while retaining the declared Rust 1.75 MSRV.
- Collection/string `join` is inferred as allocation rather than task joining;
  concurrency primitives use the spellable `thread` effect instead of the
  reserved `spawn` keyword.
- Native task groups now share the bounded executor with `spawn`/`async def`.
  `task_group_join` closes the group, helps queued work while waiting, and
  avoids one OS thread per grouped task.

### Fixed

- Native `par_map` now carries the list element/result types through lowering
  and calls IRIS closures through generated ABI adapters. Results retain input
  order, and worker-creation failure falls back to safe inline execution.
- Channel `select` now preserves the selected channel's element type instead of
  leaking an unresolved inference type into native code.
- Nested native task-group captures use the correct runtime boxing helper, and
  Windows timed channel receive now converts the system clock epoch correctly.
- The source test runner now supports checked compile-fail directives and
  diagnostic substrings. Legacy result-returning fixtures were converted to
  zero-status assertion wrappers, and the stale syntax/known-broken backlog was
  reduced to the single import-only documentation fixture.
- LSP integration tests now distinguish error diagnostics from unused-code
  hints, preserving dead-code highlighting without classifying valid programs
  as compiler failures.
- `iris test` now preserves a program's own `main` under an internal symbol
  when running any other `test_*` function, preventing duplicate LLVM `main`
  definitions. Named native eval entries use the same collision-free wrapper,
  and native test processes now use the configured execution timeout instead
  of being able to freeze the full suite indefinitely.
- `iris bench <file> [-n N]` now reaches the benchmark runner instead of
  evaluating the file once, restoring numeric performance-gate output.
- Public compiler-to-module APIs now run lowering and optimization on the same
  guaranteed 64 MiB stack as CLI compilation, removing Linux/macOS test-thread
  stack overflows for larger integration programs.
- Parser recovery treats an empty token slice as EOF instead of indexing past
  the stream, closing the fuzz-discovered panic.
- UDP numeric-address validation no longer depends on the non-portable
  `INADDR_NONE` macro, allowing the C runtime to compile on current macOS SDKs.
- Scalar fixed-array checks now call a checked runtime boundary without
  splitting the surrounding IR block. This preserves valid LLVM phi
  predecessors for array access inside `while` and `for` loops.
- Windows CI installs the UCRT64 sysroot used by the required direct MinGW
  `ld.lld` regression test.
- Parser recovery now guarantees forward progress at every module nesting
  level, so an unexpected declaration separator or closing brace reports a
  diagnostic instead of hanging the compiler or language server.
- Native builds now discover a bundled `sqlite3.dll` beside the IRIS compiler
  before searching `PATH`, allowing database programs to compile and run from
  isolated build directories and release installations.
- Native scalar-array field aliases now preserve their element representation
  and recover their bound from the actual initializer instead of using the
  boxed-array runtime ABI.
- Named source functions used as callbacks now lower through a closure adapter,
  preserving the native calling convention across parameters and record fields.
- `std.fs.append_text` preserves line-feed bytes on Windows by opening its
  stream in binary append mode, matching `read_text` and `write_text`.

- Tape-helper inlining now permits `return` only as the callee's outer terminal
  statement; returns nested in branches stay isolated and cannot escape into a
  caller during differentiable lowering.
- Native closures correctly unbox captured records before field access instead
  of interpreting the tagged runtime wrapper as the record allocation.
- Reverse-mode tape identity now crosses loop back-edges and safe helper
  boundaries ending in `return expr`, so loop-accumulated gradients remain
  differentiable on native and interpreter backends.
- Native task-group lowering now defines every emitted SSA value, including
  programs that import `std.async` without directly using task groups.
- Native spawn trampolines now release boxed capture environments on both normal
  completion and cancelled-entry paths.
- Runtime RC cleanup now releases mutex payloads and task-group payloads,
  including cancellation/join before a task-group allocation is freed.

---

## [1.0.0-rc1] — Tooling & Editor Support

### Added

- **LSP Semantic Tokens** — registers `"semanticTokensProvider"` capability in `src/lsp.rs` and implements a full delta-encoding stream covering keywords, contextual constructs, literals, and AST-resolved user definitions for rich syntax highlighting across Neovim, Helix, Emacs, and VS Code.
- **LSP Document Highlights** — registers `"documentHighlightProvider": true` capability and wires `"textDocument/documentHighlight"` JSON-RPC handler to instantly highlight all occurrences of the identifier under the cursor.
- **LSP Call Hierarchy** — registers `"callHierarchyProvider": true` capability and implements `"textDocument/prepareCallHierarchy"`, `"callHierarchy/incomingCalls"`, and `"callHierarchy/outgoingCalls"` request handlers with full AST walks to generate incoming/outgoing call graphs.
- **DAP Conditional Breakpoints** — advertises `"supportsConditionalBreakpoints": true` in `src/dap.rs` and propagates conditions configured in `setBreakpoints` to `DebugSession` for conditional pause evaluations.

### Tests

- Target integration tests in [lsp_features_v061.rs](../tests/lsp_features_v061.rs) verifying semantic token delta-encoding, call hierarchy preparation, and document highlight reference resolution.
- Target integration tests in [debugger_conditional_breakpoints.rs](../tests/debugger_conditional_breakpoints.rs) asserting conditional breakpoint pauses with loop variable snapshots.

---

## [1.0.0-rc1] — Performance & Security

### Added (1.0.0-rc1)

- **Security audit infrastructure** (`src/security.rs`) — `SecurityPolicy` with
  per-capability allow/deny flags (fs_read, fs_write, network, ffi, process),
  allowlists and blocklists, resource limits (max_file_write_bytes, max_open_files,
  max_connections). Path validation detects traversal attacks, null byte injection,
  and Windows device name abuse. Audit logging records every security-relevant
  operation with timestamps.
- **Reference-counting GC** — C runtime implements side-table reference counting
  (`iris_retain`, `iris_release`) with deep-free semantics for strings, lists,
  maps, options, and results. `iris_gc_collect` sweeps zero-count entries.
  `iris_gc_stats_allocated` / `iris_gc_stats_freed` expose statistics.
- **Copy propagation pass** (`CopyPropPass`) — eliminates duplicate `ConstInt`
  and `ConstFloat` definitions across blocks, with transitive chain resolution.
  Reduces register pressure and enables further DCE.
- **Loop-invariant code motion** (`LicmPass`) — computes dominators, detects
  natural loops via back edges, and hoists pure loop-invariant instructions to
  the loop preheader. Full CFG analysis with iterative dataflow.
- **Benchmark suite expansion** — six new real-world benchmarks: binary search,
  tree traversal, hashmap insert/lookup, Collatz conjecture, Sieve of
  Eratosthenes, and Simpson's rule numerical integration (12 total).
- **Profiler** (`iris profile <file>`) — per-function timing, call counts,
  instruction counts, folded-stack format for flamegraph.pl / speedscope,
  built-in SVG flame graph generator, human-readable summary table.
  CLI options: `--svg`, `--folded`.
- **Sandboxed FFI** — C runtime mirrors Rust-side security policy with
  `iris_sandbox_set_policy`, `iris_sandbox_check_fs_read/write`,
  `iris_sandbox_check_network`, `iris_sandbox_check_ffi`. Default-deny when
  sandbox is active.

### Changed (Unreleased)

- **Compiler pipeline** — now includes `CopyPropPass` (after `StrengthReducePass`)
  and `LicmPass` (after `OpExpandPass`) in all pipeline paths including
  `compile_to_module`.
- **CLI** — added `profile` subcommand.

### Tests

- 36 new integration tests in `v0_6_0_performance_security.rs` covering security policy, path
  validation, audit logging, profiler lifecycle / flame graphs / edge cases,
  CopyPropPass constant dedup, LicmPass safety, full pipeline integration,
  pass manager with all v1.0.0-rc1 passes, and benchmark file existence.

---

## [1.0.0-rc1] — ML & Compute

### Added (Unreleased)

- **Real tensor runtime** — `IrisTensor` struct in C runtime with 30+ functions:
  create, reshape, transpose, element-wise ops, matrix multiply, reductions
  (sum, mean, max, min), unary ops (relu, sigmoid, tanh, exp, log, sqrt, abs),
  print, and memory management.
- **General einsum engine** — interpreter implements full Einstein summation
  notation with arbitrary subscript strings; handles dot products, matrix
  multiply, batched matmul, transpose, trace, and arbitrary contractions.
- **Tensor codegen** — LLVM IR, LLVM stub, and CUDA backends dispatch real
  tensor operations: einsum (with matmul fast-path), unary, reshape, transpose,
  reduce. SIMD-friendly loop nests for x86/ARM.
- **Reverse-mode automatic differentiation** — tape-based backpropagation via
  three new IR instructions (`TapeRecord`, `Backward`, `TapeGrad`). Supports
  17 operations: add, sub, mul, div, neg, sin, cos, exp, log, sqrt, relu,
  sigmoid, tanh, pow, abs, identity, chain rule. Full topological-sort gradient
  propagation in the interpreter.
- **Enhanced sparse tensor ops** — `Sparsify` converts both arrays and dense
  tensors to sparse (index, value) pairs; `Densify` reconstructs dense arrays
  from sparse representation. C runtime provides `iris_tensor_sparsify`,
  `iris_sparse_to_tensor`, `iris_sparse_dot`, `iris_sparse_nnz`.
- **48 new tests** — 24 tensor tests (tensor_runtime_operations.rs), 17 reverse-mode AD tests
  (reverse_mode_automatic_differentiation.rs), 7 sparse tensor tests
  (sparse_tensor_operations.rs).

### Changed

- **ONNX binary export** — already functional from prior work; verified with
  8 passing tests.
- **GPU/CUDA backend** — updated codegen dispatch for all tensor op variants.
- **SIMD codegen** — auto-vectorization paths verified for tight loops.

---

## [Unreleased] — targeting v0.3.0

### Fixed

- **Closure codegen** — rewrote lambda calling convention in LLVM backend: all
  lambdas now use uniform `(ptr %env, params...)` signature with capture
  extraction preamble at entry, fixing crashes for basic closures, captured
  variables, and higher-order function usage. Replaced stub
  `iris_call_closure` runtime function with proper `iris_closure_fn()` and
  `iris_closure_get_capture()` helpers.
- **List sort** — replaced bubble sort with stable O(n log n) merge sort in the
  C runtime.
- **Native concurrency** — three bugs fixed:
  - `ChanRecv` now properly unboxes the pointer returned by `iris_chan_recv()`
    to extract the i64 value.
  - `spawn` now passes captured variables to the trampoline function so spawned
    closures can access parent-scope bindings.
  - `println` / `print` / `eprintln` are now lowered as built-in `Print`
    instructions instead of generic calls, fixing "unresolved Infer" type errors
    when their return values were used in expressions.
- **TCP networking** — replaced stub TCP instruction handlers in the interpreter
  with real TCP calls (`TcpStream::connect`, `TcpListener::bind`, `accept`,
  `read`, `write`, `close`) via the existing `tcp_store` module.

### Added (0.2.0)

- `ROADMAP.md` with milestones v0.3.0 through v1.0.0 and beyond.
- `STABILITY.md` — feature-tier classification (Tier 1 Stable through Tier 4
  Experimental) and 12 stability milestones for v1.0 gate.
- `SPEC.md` — draft language specification covering syntax, semantics, type
  system, builtins, concurrency model, tensor ops, module system. Finalized for
  v0.3.0 (removed "Draft" label, fixed function type grammar, added
  implementation notes).
- Fuzz testing infrastructure — targets for lexer, parser, lowerer, and
  compiler; seed corpus of 18 `.iris` programs; CI job for continuous fuzzing.
- Benchmark CI — structured result collection, baseline comparison with ≥15%
  regression detection.
- **236 unit tests** for lexer, parser, IR types, IR instructions, pass manager,
  error formatting, diagnostics (byte-to-line-col, render_error, error codes,
  span underlines, colored output, help hints).
- **1050+ integration tests** across 128 test phases covering the full compiler
  pipeline from parsing through codegen and evaluation.
- VS Code extension v0.3.0 — syntax highlighting, snippets, theme,
  configuration, README.
- Built-in function return types registered in lowerer `fn_sigs` for all
  standard builtins (`println`, `sleep_ms`, `random_i64`, `random_f64`, `len`,
  `assert`, `assert_eq`, etc.).

### Improved

- **Error diagnostics** — `render_error` now includes:
  - Error codes (`[E0001]`, `[E0100]`, etc.) in every diagnostic.
  - Full span underlines (`^^^^^^^^`) instead of single-character carets.
  - Optional filename display via `render_error_with_file`.
  - ANSI-colored output via `render_error_colored` / `render_error_colored_with_file`
    with bold-red errors, bold-blue line numbers, bold-green help notes.
  - Contextual help hints for common mistakes (e.g. `@` → "decorators not
    supported", `#` → "use // for comments", `struct` → "use record", `enum`
    → "use choice", `match` → "use when", `import` → "use bring").
  - CLI automatically uses colored output when stderr is a terminal.

---

## [0.2.0] — 2026-03-03

### Added

#### Builtins (60+)

- **HTTP** — `http_get`, `http_post`
- **JSON** — `json_stringify`, `json_parse`
- **Regex** — `regex_match`, `regex_find_all`, `regex_replace`
- **DateTime** — `datetime_now`, `datetime_timestamp`, `datetime_format`
- **OS / System** — `cwd`, `list_dir`, `mkdir`, `remove_file`, `path_join`,
  `env_get`, `env_set`, `exec_cmd`, `pid`, `exit_code`, `type_of`
- **Crypto** — `sha256`, `hash`, `hex_encode`, `hex_decode`, `base64_encode`,
  `base64_decode`
- **Random** — `random`, `random_range`, `uuid`
- **Functional list ops** — `list_map`, `list_filter`, `list_reduce`,
  `list_any`, `list_all`, `list_zip`, `list_enumerate`, `list_flatten`,
  `list_unique`, `list_reverse`, `list_sorted`, `list_sum`, `list_min`,
  `list_max`
- **Collections** — deque, sorted set, bitset, heap, queue
- **String extras** — `str_pad_left`, `str_pad_right`, `str_chars`,
  `str_bytes`, `str_count`, `char_at`
- **Math constants** — `math_pi`, `math_e`, `is_nan`, `is_inf`
- **Concurrency extras** — `chan_try_recv`, `chan_len`, `select`, `timeout`,
  `thread_count`, `atomic_load`, `atomic_store`, `atomic_add`

#### SQLite

- Full-stack database operations: `db_open`, `db_exec`, `db_query`, `db_close`
  — parser, interpreter, LLVM codegen, C runtime (bundled via rusqlite).

#### FFI

- **C FFI** — `ffi_open`, `ffi_call_i64`, `ffi_call_f64`, `ffi_call_str`,
  `ffi_call_void`, `ffi_close`
- **Python FFI** — `python_eval`, `python_exec`, `python_call`,
  `python_version`
- **Rust FFI** — `rust_lib_open`, `rust_call_i64`, `rust_call_f64`,
  `rust_call_void`

#### Standard Library (25 modules)

- `math`, `string`, `fmt`, `fs`, `json`, `csv`, `http`, `time`, `crypto`,
  `ffi`, `os`, `testing`, `log`, `iter`, `set`, `queue`, `heap`, `deque`,
  `kv` (SQLite-backed), `table`, `dataset`, `dataframe`, `path`, `async`,
  `bitset`

#### Package Manager

- `iris pkg init/add/remove/install/build/run/list` — project scaffolding,
  dependency management, registry interaction.

#### LSP Enhancements

- AST-based hover (works even when compilation fails).
- Built-in and keyword hover documentation.
- Code actions: missing-semicolon quickfix, type-mismatch cast, add doc
  comment, rename to snake_case, remove redundant semicolons, wrap in
  if-condition.
- Best-practice diagnostics: BP001–BP006 (long function, missing doc, too many
  params, non-snake_case, empty body, double semicolons).
- Inlay hints, find references, rename support, diagnostic codes.

#### DAP Debugger Enhancements

- Step-back, step-over/into/out.
- Conditional breakpoints, hit counts, log-points.
- Richer stack traces with source info, loaded sources, exception info.
- Variable mutation, restart, pause.
- Debug-console completions, exception breakpoint filters.

#### REPL Enhancements

- Colored prompts, timing display, input history.
- Commands: `:ir`, `:time`, `:history`, `:clear`, `:reset`, `:bring`.

#### Tooling & Infrastructure

- Verbose `iris --version` — GCC-style output with git commit, branch, build
  date, target, host, profile, rustc version.
- Binary output naming — `hello.iris` → `hello.exe` / `./hello`.
- Incremental compilation cache infrastructure.
- Benchmark suite — factorial, fib, list, matrix, sort, string benchmarks.
- ARM64 CI — cross-platform CI on x86_64 + ARM64 (Linux, Windows, macOS).

#### Installers

- **Windows** — portable `.zip`, WiX `.msi`, Inno Setup `.exe` (bundles
  LLVM/clang, lld, MinGW ucrt64 sysroot).
- **Linux** — curl one-liner, `.deb`, `.rpm`, AppImage.
- **macOS** — curl one-liner, `.pkg`, `.dmg`.

#### VS Code Extension 0.2.0

- Status bar with version tooltip, Show Version Info command.
- Server menu (restart/stop LSP).
- LSP best-practice diagnostics and code actions.
- Inlay hint settings, timing on run.
- Updated TextMate grammar for all new builtins and types.
- New snippets for FFI, concurrency, error handling.

### Changed

- **C runtime rewrite** — now uses clang + lld exclusively (removed GCC/MSYS2
  dependency).
- **Build metadata** — `build.rs` captures git hash, branch, dirty flag, build
  date, rustc version, target/host/profile/opt-level.
- Error recovery improvements in parser.

### Changed (license)

- License changed from MIT to GPL-2.0-or-later.

---

## [0.1.0] — 2026-02-28

### Added

#### Core Language

- **Lexer** — tokenizer for `.iris` source files.
- **Parser** — recursive-descent parser producing AST.
- **SSA IR** — block-parameter SSA (MLIR-style), no phi nodes.
- **Lowerer** — AST → IR lowering with lambda-lifting for closures.
- **Pass pipeline** — Validate, TypeInfer, ConstFold, OpExpand, DCE, CSE,
  ShapeCheck, Inline, LoopUnroll, StrengthReduce, Exhaustive, GcAnnotate.
- **Tree-walking interpreter** — `iris run` / `--emit eval`.

#### Type System

- Primitives: `i32`, `i64`, `f32`, `f64`, `bool`, `str`.
- Composite: `tensor<T, shape>`, `list<T>`, `map<K,V>`, tuples, arrays.
- Records (`record`), enums (`choice`) with variant payloads.
- Generics — `def identity[T](x: T) -> T` with monomorphization.
- Traits / Impl — `trait Printable`, `impl Printable for Point`.
- Type aliases — `type Matrix = tensor<f64, [3, 3]>`.
- Function types — `fn(i64) -> i64`.
- `option<T>`, `result<T,E>`, `?` operator.

#### Control Flow & Pattern Matching

- `if/elif/else`, `for`, `while`, `break`, `continue`, `return`.
- `when` (pattern matching) with guards, range patterns, tuple destructuring.

#### Closures & Functions

- Lambda expressions — `|x: i64| x * 2`.
- Default parameters — `def greet(name: str = "world")`.
- Global constants — `const PI: f64 = 3.14`.

#### Concurrency

- `channel<T>`, `spawn`, `par for`.
- `async/await`, `atomic<T>`, `mutex<T>`.

#### ML Features

- Automatic differentiation — `grad<T>` dual numbers, `@differentiable`.
- Sparse tensors — `sparse<T>`, `sparsify`, `densify`.

#### Strings

- F-string interpolation — `f"Hello, {name}!"`.
- Builtins: `len`, `concat`, `split`, `join`, `contains`, `starts_with`,
  `ends_with`, `trim`, `to_upper`, `to_lower`, `repeat`, `find`, `slice`,
  `str_replace`, `str_reverse`.

#### Math Builtins

- `sin`, `cos`, `tan`, `exp`, `log`, `sqrt`, `abs`, `pow`, `min`, `max`,
  `clamp`, `floor`, `ceil`, `round`.

#### I/O

- `print`, `read_line`, `read_i64`, `read_f64`.
- TCP/network instruction lowering (interpreter).

#### Code Generation

- `--emit ir|llvm|eval|binary|onnx|cuda|simd` (stubs for ONNX/CUDA/SIMD).
- LLVM IR codegen — target triples, string globals, 70+ runtime declarations.
- Native binary compilation — `iris build` via clang.

#### Module System

- `bring std.math`, `pub def`, multi-file compilation.

#### FFI

- `extern def` for C function declarations.
- GC refcounting basics.

#### Tooling

- **REPL** — `:help`, `:env`, `:type`, `:quit`.
- **LSP** — hover, completions, diagnostics, go-to-definition, document
  symbols, signature help, formatting.
- **DAP** — breakpoints, step, variables, evaluate.
- **CLI** — `iris run`, `iris build`, `iris repl`, `iris lsp`, `iris dap`.

#### VS Code Extension 0.1.0

- Syntax highlighting (TextMate grammar).
- LSP integration, DAP debugger integration.
- Commands: Run File (Ctrl+F5), Build Binary, Open REPL.
- Snippets for common constructs.

---

[Unreleased]: https://github.com/Moon9t/IRIS/compare/v1.0.0-rc1...HEAD
[1.0.0-rc1]: https://github.com/Moon9t/IRIS/compare/v0.2.0...v1.0.0-rc1
[0.2.0]: https://github.com/Moon9t/IRIS/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Moon9t/IRIS/releases/tag/v0.1.0

