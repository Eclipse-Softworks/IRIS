# How rustc Is Wired, and What IRIS Should Borrow

**Date**: 2026-08-05
**Sources**: [rustc-dev-guide overview](https://rustc-dev-guide.rust-lang.org/overview.html);
IRIS architecture read directly from the source tree.

The purpose here is not to imitate rustc. It is to identify *which* of rustc's
structural choices explain the specific classes of bug IRIS is currently hitting,
and which are irrelevant at IRIS's scale.

---

## 1. The rustc pipeline

| # | Stage | Operates on | Responsible for |
|---|---|---|---|
| 1 | Lexing (`rustc_lexer`) | source text | tokens |
| 2 | Parsing | tokens | AST |
| 3 | Expansion & resolution | AST | macro expansion, AST validation, **name resolution**, early lints |
| 4 | AST lowering | AST | **HIR** — desugared, name-resolved |
| 5 | Type check & inference | **HIR** | type inference, trait solving, type safety |
| 6 | THIR lowering | HIR | **THIR** — fully typed, explicit method calls and derefs |
| 7 | MIR lowering | THIR | **MIR** — control-flow graph of basic blocks |
| 8 | Borrow checking | **MIR** | dataflow-based memory safety (NLL) |
| 9 | MIR optimisation | MIR | generic, pre-monomorphisation optimisation |
| 10 | Monomorphisation collection | MIR | worklist of concrete instantiations |
| 11 | Codegen | MIR → LLVM IR | machine code |

Two cross-cutting systems: **queries** (every stage is a memoised query hanging off
`TyCtxt`, giving incremental compilation via dependency tracking) and
**diagnostics** (`Diag`; the parser accepts a superset of the grammar so it can
recover and report many errors per run).

**The critical ordering fact:** rustc types the program at stage 5, on HIR — a
tree that still mirrors the source. MIR, the CFG, is only produced at stage 7,
*after* everything is fully typed. Types flow **downhill** into the CFG; they are
never re-derived from it.

---

## 2. The IRIS pipeline as it stands

| # | Stage | Operates on | Notes |
|---|---|---|---|
| 1 | Lexer | source | ~46 keywords |
| 2 | Parser | tokens | AST, 35 expr / 18 stmt / 23 type forms |
| 3 | Preprocessor | source | `#ifdef` family, before lexing |
| 4 | AST passes | AST | `ast_exhaustive`, `variance_checker`, **`borrow_checker`**, `effect_checker` |
| 5 | HIR construction | AST → HIR | `src/hir`: resolves every value name (locals, functions, built-ins, constants, state, methods, effects, brought module qualifiers) and registers every expression; source-shaped, as rustc's HIR is |
| 6 | Source type checker | HIR | `src/lower/typeck.rs`: unifies and types the HIR **before lowering**, and owns the program's type errors and lowering's old name/shape errors (E0100/E0109/E0113/E0114/E0118/E0122/E0210); conservative -- what it does not model agrees with everything. Its report is absorbed into the HIR |
| 7 | THIR construction | HIR → THIR | `src/thir`: owns a typed, desugared tree. Values and calls carry their resolutions, method calls their targets, record fields their layout indices, map literals their key/value types, and tuple destructuring a temporary plus one element access per component. Built on every compile and audited by `IRIS_THIR=report|check`; lowering consumes its type hints, field indices, map types, and tuple-destructuring shape, while the body walk remains AST-based |
| 8 | Lowering | HIR → IR | **also performs monomorphisation** (`mono_sigs`, `generic_fns`, `resolve_generic_struct_type`); it reads solved `-> Infer` returns from the HIR and type hints from THIR (HIR fallback only for lowering-synthesized spans), and consumes THIR's explicit field/map/destructuring facts |
| 9 | `ValidatePass` | IR | SSA structural correctness; every value is already typed (a value nothing determines is E0207, raised from the checker's types right after lowering) |
| 10 | `TypeInferPass` | IR | a **consistency check** over the types lowering emitted, not inference |
| 11 | ~20 further passes | IR | const-fold, LICM, inline, DCE, CSE, GC annotate, shape check … |
| 12 | Codegen | IR | LLVM text, CUDA, SIMD, WASM, ONNX |

Mapped onto rustc: IRIS's AST ≈ rustc's AST, `src/hir` ≈ rustc's HIR (stage 4),
`src/thir` ≈ rustc's THIR (stage 6), and `IrModule` ≈ MIR (a CFG, using block
parameters instead of phi nodes — an MLIR-style choice, and a good one). The HIR
is deliberately source-shaped, as rustc's is: it is the checked node tree plus
resolution and type tables, not a second copy. It is built before type checking,
the checker runs on it, and its report is absorbed into it; lowering reads
solved `-> Infer` returns from the HIR and type hints from THIR.

THIR is a real second tree, not a second resolution pass: it is built from the
HIR after the checker, and owns explicit, desugared nodes. Lowering now consumes
THIR's type hints, record field indices, map key/value types, and the shape of
tuple destructuring, but still pattern-matches the AST for control flow and the
remaining forms. The THIR → CFG edge is therefore **partial**, not complete:
do not describe lowering as fully THIR-based until the body walk migrates.

---

## 3. The three structural differences that explain the bugs

### 3.1 Type inference used to run *after* lowering — the big one

> **Resolved (2026-10-24).** The source checker (`src/lower/typeck.rs`) types
> the HIR before lowering, and lowering consumes those types. `IrType` has no
> `Infer` variant, and `src/pass/type_infer.rs` is only a consistency check.
> The history below is why the old ordering produced bugs.

rustc types HIR (stage 5) and lowers to MIR (stage 7). IRIS used to lower first
and infer types afterwards, on the CFG.

The consequence was that type information which was obvious in the source had to
be **re-derived from IR context** once the source structure was gone. That is
exactly the shape of the defects found:

- `str` field in a record inside `result<T, E>` mis-typed as `i64`, failing LLVM
  verification (`'%v14' defined with type 'i64' but expected 'ptr'`).
- Named arguments silently evaluating to `0` — argument identity was resolved
  during lowering, before any type information existed to check it against.
- `IrType::Infer` existing at all as a lowering output.

And the fallback made it worse: **unresolved inference slots defaulted to `i64`**.
That converted "I could not determine this type" into "it is an integer", which is
precisely how a `str` became an `i64` and a wrong answer reached the user instead
of an error. `ValidatePass` then rejected only *surviving* `Infer` — but the
default had already removed them.

`MEMORY.md` records the workaround this forced: the lowerer must eagerly propagate
concrete scalar types "to avoid `Infer` at validation time", and unknown calls had
to start erroring explicitly because inference would otherwise silently resolve
them to `i64`. Those are symptoms of the ordering, not the cause.

### 3.2 Borrow checking runs on the AST, not the CFG

rustc borrow-checks MIR (stage 8) using dataflow over a control-flow graph — that
is what makes non-lexical lifetimes possible at all.

IRIS originally borrow-checked only the AST (`src/pass/borrow_checker.rs`), with scope-based
lifetimes and borrows cleared at scope exit. A tree has no notion of "this path
reaches that use", so the analysis cannot be path-sensitive. It will accept unsound
programs and reject sound ones, and no amount of work on the AST version fixes
that — the information is not present in the representation.
*(Status update: `CfgBorrowCheckerPass` in `src/pass/cfg_borrow_checker.rs` now
performs IR-level CFG NLL loan/liveness checking in `build_standard_pipeline()`,
and `DropElaborationPass` + `QueryEngine` + `TypeArena` are implemented alongside
it; lazy post-IR monomorphisation in §3.3 remains future work.)*

### 3.3 Monomorphisation happens inside the lowerer

rustc collects a monomorphisation worklist as a distinct stage (10) over already
optimised, already type-checked MIR.

IRIS monomorphises *during* lowering, which means the lowerer is simultaneously
desugaring, resolving generics, substituting types, and building SSA. `src/lower/mod.rs`
is **17,262 lines** — by far the largest file in the project. That is the direct
cost of one stage owning four responsibilities, and it is why bugs there are hard
to localise.

---

## 4. What to actually do

Ordered by value-per-unit-of-disruption. This is a refactor of *ordering*, not a
rewrite — most existing code moves rather than disappears.

### 4.1 Stop defaulting unresolved types to `i64` — do this first

> **Done (2026-10-18; completed 2026-10-24).** A value that some instruction
> *uses*, but whose whole type nothing in the program determines, is now error
> E0207 ("type annotations needed") at the statement that produced it, instead
> of a silent `i64`. An unknown *part* of a compound (`result<i64, ?>`,
> `chan<?>`, an unused `list()` element) takes `i64` when nothing constrains
> it, because no read of the compound can observe it; extracting it produces a
> wholly unknown value, which is caught. The corpus (217 files), every stdlib
> module and all projects compile unchanged; the one false positive found,
> `list_pop`, was a missing inference constraint and is fixed. The judgment
> lives in the checker (`src/lower/typeck.rs`): E0207 comes from the source
> tree, and `check_typed` asserts lowering leaves no value untyped. There is
> no env switch to restore the old defaulting.

A one-line policy change with outsized effect: make an unresolved inference slot a
**compile error** with a span, not an `i64`.

This will surface a set of latent failures immediately, which is the point — every
one of them is currently a silent wrong answer. Do it after Phase 1.2 of the
release plan (the test-assertion conversion), so the fallout is visible.

**Effort:** hours to change, days to work through the fallout.

### 4.2 Introduce typed HIR and THIR between AST and lowering

**Implemented (2026-10-24).** `src/hir/{mod,build}.rs` builds the HIR after the
AST passes and before type checking:

- **name-resolved** (a distinct resolution stage, as rustc has at stage 3):
  every identifier in value position, call callee, record constructor, `when`
  struct pattern, and brought module qualifier is looked up and recorded as a
  `Resolution` (`Local`, `Fn`, `Const`, `State`, `Builtin`, `Type`, `Module`).
  Method names, `raise` effect names, and enum variant patterns are recorded as
  `Method`/`Effect`/`Variant` at their call or arm spans, but are not checked
  against a symbol table here; the checker and lowering own those errors.
- **desugared before HIR** where the surface language has sugar: the AST passes
  flatten inline modules, inject trait default methods into impls, desugar
  `yield`, and expand macro calls; f-strings are expanded by the parser. HIR
  therefore sees the post-expansion program, as rustc's does. THIR then
  desugars tuple destructuring into a temporary plus one `TupleIndex`, `Field`
  or `Destructure` per component; map literals are explicit THIR nodes carrying
  their key/value types, while the CFG lowering still emits the map operations.
  Control-flow desugaring remains lowering/MIR.
- **typed before lowering**: every expression is registered by span and gets a
  `HirType` slot: `Known` (the whole type), `Partial` (the checker solved part
  of it; lowering substitutes generic parameters), or `Open` (the checker does
  not model the form). `absorb` fills every slot, so nothing is silently
  absent; `IRIS_HIR=report` prints the split. "Fully typed" is not literal:
  the checker is conservative by design, and the open set is explicit. On
  `tests/test_mature_stdlibs.iris` it is 5,696 expressions: 5,180 known
  (90.9%), 74 partial (1.3%), 442 open (7.8%). THIR copies the same split into
  its own nodes (`Known`/`Partial`/`Open`), not a claim that the open set is
  gone.

The checker runs on the HIR (`typeck::check(&hir, ...)`), its report is
absorbed into it, and lowering reads types and solved `-> Infer` returns only
from the HIR: `typeck::hint`/`solved_return` delegate to the installed HIR
context, and the checker's old `HINTS`/`RETURNS` thread-locals are gone.

`IRIS_HIR=check` audits completeness (every expression typed, every value name
and call resolved) after the program's own errors have been reported; all 174
valid `tests/*.iris` and 46 `examples/*.iris` pass it with zero unresolved
names or calls. `tests/hir.rs` gates this.

**THIR is in place; lowering consumption started (2026-10-25).**
`src/thir/{mod,build}.rs` owns a second tree built from the HIR after the
checker: every HIR-walked expression is rebuilt, values and calls carry their
resolutions, method calls carry their mangled targets, record fields carry
layout indices when the base type is known, map literals carry key/value types,
and tuple destructuring is desugared. `IRIS_THIR=check` audits coverage and
resolution; all valid corpus programs pass with zero unresolved values or
calls. `tests/thir.rs` gates this.

Lowering now reads THIR first for type hints, record field indices, map
key/value types, and tuple-destructuring shape, and the body walk lowers the
pure value forms plus binary operators, indexing, casts, if-expressions, and
expression blocks from THIR first, falling back to the AST for control flow,
calls, when-expressions, and statements. A
`let (a, b) = ...`
lowering fails with an internal `E0902` if THIR has no desugared entry for
it; the record field and map consumers use THIR's answer when it still matches
the lowered type, and fall back to the lowered type only for local/generic
cases THIR could not resolve. This is real consumption, but it is **not** the
full body-walk migration.

**Remaining:** the `Lowerer` body walk still pattern-matches the AST and
resolves names itself through `resolve_unqualified_name`; control flow,
lambdas, effects/handlers, async/await and taped autodiff have not moved to
THIR nodes. That migration is the next step, and it is not a rename: those
contexts must become THIR consumers one slice at a time. THIR's own limits are
explicit: `when` patterns and signatures keep their source
`AstType`/`AstWhenPattern` shapes; a generic cast target has no resolved
`IrType`; control flow is kept for MIR; and the model/graph DSL is outside the
stage (`lower_model` in `src/lower/graph.rs` builds `GraphIr` directly and
neither builds nor reads HIR/THIR; only model layer parameter values are
nominally walked by `lower()`). The checker types functions, impl methods and
constants; state initialisers, record field defaults, parameter defaults, macro
bodies, attribute arguments and model layer parameters are walked and resolved
but remain `Open` — they are lowering's to type.

The old IR-level `HmTypeInferPass` is gone. `src/pass/type_infer.rs` holds a
consistency check over what lowering emitted; the unification machinery lives
in `src/lower/typeck.rs` and operates on the HIR.

The original estimate was weeks; resolution, typing and the THIR tree are in
place, lowering consumes THIR's explicit facts, and migrating the body walk is
the remainder.

> **Milestones 1-7 done (2026-10-24).** `src/lower/typeck.rs` types the HIR before
> lowering, with unification over inference variables, from the registries
> lowering builds (records, choices, aliases, signatures, constants, impls).
> It is conservative -- what it does not model agrees with everything -- and
> it is measured, not trusted: `IRIS_TYPECK=compare` checks its types against
> the IR's for every value of every program.
>
> | Milestone | Status |
> | --- | --- |
> | 1. Types every expression, in shadow | done: 93.9% of 489k expressions in the corpus typed, 100.00% agreement with the IR on the 362k values compared (after M3) |
> | 2. Its errors are reported when lowering accepted the program | done: zero errors on the 397 valid programs; an ignored annotation (`val z: i32 = a`), an integer passed for a float parameter (which crashed natively) and an integer literal where a float is declared are errors with fixes |
> | 3. Lowering takes its types where it defaulted | done for literals' widths, empty `list()`, `map()`, `none`, `ok` and `err`: `map()` was `map<str, i64>` whatever it held |
> | 4. It replaces the IR-level type errors | done (2026-10-21): its errors are reported whether or not lowering accepts the program, in place of lowering's error in the same function, carrying lowering's fixes (`1.0`, `to_str(3)`); it reports E0207 itself. On the 51 diagnostic cases and every compile-fail program, each E0202/E0207/E0208/E0209 comes from it, and the IR passes find no type error it misses. Newly errors: `?` in a function that does not return a result, `?`s carrying different errors in one `try`, a list given values of different types, `concat` with a non-string, an option compared with a number. Name and shape errors (E0100, E0109, E0113, E0114, E0118, E0122) and `when` exhaustiveness (E0210) are its too (milestone 6) |
> | 5. Lowering consumes a typed tree and cannot produce `Infer` | done (2026-10-21): lowering takes the checker's types for empty collections, channels, `none`/`ok`/`err`, generic and async bodies (type parameters substituted per instance), and types its own desugarings from the program (a generator's list from the declared return type, `?` and `catch` from the function's result and the body's errors, handler continuations from the effect's signature, program state from its declaration). On the 341-program corpus lowering leaves 0 values, block parameters or signatures untyped (274 before). `IrType` has no `Infer` variant: the tree's types are the program's types, and a value nothing determines is E0207 from the checker. `src/pass/type_infer.rs` is a consistency check, not inference. **Limit:** what the checker does not model is `Unknown` to it and is typed by lowering from the program's declarations; a built-in whose dispatch it does not model (or a local shadowed by a built-in in call position) is left to lowering |
> | 6. Lowering's name and shape errors are the checker's | done (2026-10-24): unknown names (E0100), wrong argument counts (E0109), a record literal's unknown or missing fields (E0113/E0114), calling a value (E0118), an unknown variant (E0122) and a non-exhaustive `when` (E0210) are reported from the HIR before any IR exists. The 51-file diagnostics corpus passes byte-exact against the CLI's rendered output, and the checker reports nothing on the valid programs of the 341-program corpus |
> | 7. A resolved, typed HIR between the tree and the CFG | done (2026-10-24): `src/hir/{mod,build}.rs` walks every function, impl method, constant, state initialiser, field default, macro body and model layer parameter; every value name and call site is resolved; every expression has a `HirType` slot. The checker runs on the HIR and its report is absorbed; lowering reads types and `-> Infer` returns only from it (the checker's `HINTS`/`RETURNS` tables are gone). `IRIS_HIR=check` audits completeness, `IRIS_HIR=report` prints the counts; 174 valid `tests/*.iris` and 46 `examples/*.iris` have zero unresolved names or calls. **Limit:** node storage is source-shaped, 7.8% of the mature-stdlib corpus is explicitly `Open` (the checker's conservative cut), lowering still resolves names itself (only the type/hint flow is HIR-authoritative), and the model/graph DSL takes the separate `lower_model` path without HIR |
> | 8. A typed, desugared THIR between HIR and the CFG | built (2026-10-24), consumed in part (2026-10-25): `src/thir/{mod,build}.rs` owns a second tree. Values and calls carry resolutions, method calls carry targets, record fields carry layout indices, map literals carry key/value types, and tuple destructuring becomes a temporary plus one element access per component. `IRIS_THIR=check` audits that every HIR-walked expression was rebuilt and every value/call resolved; the valid corpus passes. `tests/thir.rs` gates this. Lowering consumes THIR's type hints, field indices, map types, and tuple-destructuring shape; **limit:** the body walk still uses the AST for control flow and the remaining forms, so THIR is not yet the CFG's only input. Patterns and signatures keep source shapes, generic casts have no resolved `IrType`, and the model/graph DSL remains outside the stage |
>
> Its rules: no implicit conversions; an integer literal takes the integer
> width its context declares and its type is fixed where it is bound; records
> yielded by different branches are left to the `dyn` coercion lowering does.

### 4.3 Move borrow checking to the IR CFG

Once the IR is reliably typed, re-target `borrow_checker.rs` from AST to
`IrModule` and make it a dataflow analysis over the block graph. IRIS already has
the CFG; it is simply not being used for this.

**Effort:** weeks. Do after 4.2.

### 4.4 Split monomorphisation out of the lowerer

Make it a distinct stage over typed IR, as rustc does. The immediate benefit is
that `src/lower/mod.rs` stops being a 17k-line file with four jobs, and generic
resolution bugs become localisable.

**Effort:** weeks. Can proceed in parallel with 4.3.

### 4.5 Adopt rustc's diagnostics discipline, not its diagnostics code

Two cheap wins already visible as problems:

- Errors should carry a **source span** and point at user syntax. The current
  `variable '%30' is used before it has been assigned` names an internal SSA value
   — meaningless to a user.
- Keep parsing after an error to report many per run. IRIS already does this via
  `parse_recovering()`; extend the same discipline to the type and lowering stages.

**Effort:** days, and it disproportionately improves the impression the compiler
makes on a first-time user.

---

## 5. What *not* to copy

- **The query system.** rustc's `TyCtxt`/query architecture exists to make
  incremental compilation viable across a huge codebase with separate crate
  compilation. IRIS is a single crate compiling single projects, and already has a
  file-level `BuildCache`. Adopting queries now would be a very large refactor
  serving a problem IRIS does not yet have. Revisit only if whole-project rebuild
  times become the binding constraint.
- **IRs beyond HIR + THIR + CFG.** IRIS now has both HIR and THIR at the user's
  request: HIR is the source-shaped checked tree, THIR the owned typed,
  desugared tree. The split is earning its keep as lowering consumes THIR's
  type hints, field indices, map types, and tuple-destructuring shape; adding
  another lowering IR, or copying rustc's query system, would multiply
  maintenance without removing a current bug class. Finish the body-walk
  migration, then stop.
- **Block parameters → phi nodes.** IRIS's MLIR-style block parameters are a
  *better* choice than LLVM-style phi nodes. Keep them.

---

## 6. Summary

IRIS's IR design is sound; its **stage ordering** is not. rustc establishes types
on a source-shaped tree and lets them flow downhill into the CFG. IRIS used to
build the CFG first and then try to recover types from it, with an `i64` default
when it could not — which turned unknown types into wrong answers rather than
errors.

The single highest-value change is **4.1** (fail instead of defaulting to `i64`),
because it is cheap and converts a class of silent corruption into visible errors.
The single most important change is **4.2** (typed layers before lowering),
because it removes the conditions that make that class possible at all. HIR and
THIR are now both in place, and lowering consumes THIR's explicit facts; the
remaining 4.2 work is migrating the body walk from the AST to THIR.

Neither is a rewrite. The lexer, parser, IR design, pass framework, and all 15
backends are unaffected.
