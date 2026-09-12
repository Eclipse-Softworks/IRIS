# IRIS Expressiveness Audit

**Updated:** 2026-09-10

**Release:** 1.0.0-rc1

This audit separates syntax that merely exists from behavior exercised through
the public compiler. Its evidence is the checked-in Rust integration suite and
all 155 `.iris` corpus files. The corpus gate invokes the real CLI, requires
positive tests to assert their results, verifies intended compile failures, and
checks interpreter/native agreement except for explicitly documented
backend-specific fixtures.

## Current release evidence

- `iris test tests --no-color`: **237 passed, 0 failed, 0 ignored**. This total
  includes five source files that must fail compilation with their expected
  diagnostics.
- `cargo test --all-targets --no-fail-fast`: every target passed after updating
  the LSP test to distinguish unused-code hints from compiler errors.
- The corpus has no positive test left on the non-asserting backlog.
- The only non-runnable corpus entry is an import/documentation fixture with no
  zero-argument entry function. Two intentional backend differences are
  recorded and gated in `tests/iris_corpus.rs`.

These gates establish repeatable RC1 behavior. They do not prove that arbitrary
programs are correct, that all combinations have been explored, or that a
generated model or autonomous controller is safe in a real deployment.

## Expressive surface

| Area | Verified RC1 capability | Boundary |
|---|---|---|
| Data and types | Records, choices, tuples, aliases, generics, const parameters, higher-kinded parameters, associated types, traits, blanket implementations, and dynamic trait objects | Predicate refinement aliases are not part of the RC1 grammar |
| Functions | Closures, named/default arguments, extension methods, operator overloads, compile-time functions, macros, and explicit `return` | Typed metaprogramming is compiler-hosted; IRIS is not yet self-hosting |
| Patterns | Record, choice, tuple, slice, or/range patterns, guards, `if let`, `while let`, refutable bindings, and exhaustiveness diagnostics | Compile-fail behavior is tested explicitly rather than treated as a runnable positive test |
| Control flow | Expression-valued branches, loops, labelled break/continue, `defer`, `try`/`catch`, results/options and `?` | Deep native recursion still depends on target stack unless a tested tail-call transformation applies |
| Effects | Declared and inferred effect rows, polymorphic callback effects, handlers, masks, resumptions, and strict-effect checking | Effect declarations describe and constrain behavior; they do not replace application authorization or sandboxing |
| Concurrency | Bounded executor, `spawn`, `async def`/`await`, typed channels, timed receive, `select`, task groups, cancellation, atomics, mutexes, parallel ranges, and typed `par_map` | This is a bounded native task runtime, not an OS-independent network reactor |
| Memory and safety | Reference counting, cycle collection, weak references, lexical ownership/borrowing/moves, checked integer arithmetic, checked indexing, and bounded native execution | Borrow analysis is conservative and lexical, not a formal proof of whole-program memory safety |
| Differentiation and ML | Reverse-mode scalar tape, tensor/NN operations, gradients across calls, closures, branches, loops and trait dispatch, model adapters and registry/health APIs | External framework availability and model quality remain deployment concerns |
| AIS and evolution | Homeostatic/active-inference building blocks, lifecycle and safety policies, checked source edits, ORC JIT, ABI-verified hot swap, leases, history, audit chain and rollback | The language supplies mechanisms and gates; it cannot guarantee that an autonomous policy is beneficial or physically safe |
| Networking and LLM | Typed TCP, bounded reads/writes/timeouts, status-bearing HTTP, JSON paths, provider-neutral chat/tool/embedding/local-model APIs | HTTPS support and provider behavior vary by host platform and installed service |
| Embedded | Allocation-free scalar bundles for supported Cortex-M/ESP32 profiles plus a restricted Arduino Uno workflow | Board wiring, toolchains and physical timing/electrical validation remain target-specific |
| Tooling | Formatter, HTML docs, package installation, benchmark runner, LSP navigation/hover/completion/diagnostics, semantic dead/unsafe highlighting, Test Explorer and trace DAP | Native attach/register debugging is not supplied by the RC1 extension |

## Important combinations exercised end to end

The suite deliberately covers interactions that commonly reveal compiler bugs:

- native and interpreter differentiation through closures, branches, loops,
  helper calls and traits;
- source programs that define their own `main` while `iris test` emits a named
  native test wrapper;
- nested task-group captures, cancellation, timed receive, channel `select`,
  and typed parallel mapping through closure ABI adapters;
- generic records and choices across imports, patterns and trait dispatch;
- native MinGW linking without the former Clang fallback;
- transactional speculative effects, ABI-checked in-process JIT promotion,
  generation leases and exact rollback;
- the rewritten progressive examples and complete projects in both catalogued
  execution modes.

## Honest release framing

IRIS RC1 is expressive enough to build ordinary software, concurrent services,
learning-backed systems, compiler-hosted evolving programs, and bounded embedded
controllers. “Self-evolving” means candidate generation plus explicit tests,
policy checks, sandboxing, audit, versioned activation and rollback. It does not
mean that unrestricted self-modification is intrinsically correct or safe.

The public claim should remain: **broad, tested feature coverage with explicit
deployment boundaries**. For current operational limits see
[`known-issues.md`](known-issues.md); for syntax and semantics see
[`SPEC.md`](SPEC.md).
