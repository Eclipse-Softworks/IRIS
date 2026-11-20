# Memory Management in IRIS (and How to Keep GUI Programs Lean)

IRIS uses two tiers of memory reclamation. Long-running programs — especially
GUIs — must understand both or they will creep upward in memory every frame.

## Tier 1: reference counting for heap values

Strings, lists, maps, options, results, channels, mutexes, atomics, closures and
tensors are reference-counted. The compiler inserts `retain` at creation and
releases each value at its **last use** (see `src/pass/gc_annotate.rs`). This
makes short-lived temporaries (f-strings, parsed records kept on the stack,
channel payloads) free promptly without any per-scope bookkeeping.

You normally write nothing for this. It means:

* Values that cross a function boundary carry their reference with them.
  Returning a `list<T>` to a caller transfers ownership; the caller's drop is
  what frees it.
* Cycles between heap values are reclaimed by a Bacon–Rajan cycle collector
  that runs alongside the reference count table.

## Tier 2: frame pools for transient arenas

Records are native heap allocations (`iris_alloc_bytes`) but **are** tracked by
the reference-count table as `IRIS_RC_NATIVE_OBJECT`. The compiler registers a
per-type destructor for each record, and the last release calls that destructor
and frees the block (`src/runtime/iris_runtime.c`, `rc_deep_free_by_kind`).
`GcAnnotatePass` does not treat `IrType::Struct` as a compiler-managed heap
type: a trait object (`dyn Trait`) holds a raw pointer to its wrapped record
and releasing the wrapper does not release that payload, so automatically
releasing records at last use would free a record still referenced by a trait
object in a `list<dyn Trait>`. Records return to main-heap cleanup only through
the RC table's native-object path and the cycle collector.

Frame pools remain useful as bulk arenas for allocations that are created and
discarded together within one frame. They are not the only reclamation path:

```iris
val pool = ui_frame_pool_begin()
val root = build_screen_tree(state, tree)      // any transient work
tree = ui_layout(ui_set_root(tree, root), w, h)
val buf = ui_paint(tree, ui_st, buf)
val _ = ui_frame_pool_destroy(pool)            // every byte tracked above is freed now
ui_native_submit(lib, buf)
```

Rules of thumb:

1. **All transient UI work happens inside `ui_frame_pool_begin` / `ui_frame_pool_destroy`.**
   Trees, layouts, paints, hit-tests, event folds — anything created fresh per frame.
2. **Values that must survive the pool are detached onto the main heap while the
   pool is paused.** See `ui_state_detach`, `clone_snapshot_detached`,
   `clone_str_detached` in the standard library and in `projects/terra_pulse`.
3. **The frame loop itself should not rebuild its context record when nothing changed.**
   Reuse the previous context even though records are reference-counted: a
   needless rebuild per frame still allocates, retains and releases work that
   the frame did not need.
4. **Poll/drain channels and filesystem in the background, not the UI thread, and
   pool the temporaries the worker creates** (`ui_frame_pool_begin` is legal on
   worker threads too — see `run_stage1_in_pool`).
5. Records that form long-lived application state (e.g. `TelemetryGuiState`) are
   effectively permanent by design. Keep them small; put big payloads in lists
   of detached clones that are replaced wholesale and re-pooled next frame.

## Verifying you are not leaking

`bring std.async` exposes `gc_get_stats()` as a `map<str, i64>` and
`gc_collect_sweep()` forces a GC pass. Keys now include the original
`allocated`, `freed`, `cycles`, `weak_invalidated`, plus raw-allocation
accounting (`raw_allocs`, `raw_frees`, `raw_bytes_live`, `raw_bytes_peak`),
frame-pool bytes (`pool_bytes_live`, `pool_bytes_peak`), and per-source-site
allocation counts under `site:<packed location>`. In a steady-state GUI,
`freed` should track `allocated` and `raw_bytes_live` should plateau. For
wall-clock evidence, watch the process working set while idle.

`FramePoolScopePass` makes a `ui_frame_pool_begin` compiler-managed: any begin
still open at a `Return` or `Panic` gets an automatic
`iris_ui_pool_destroy` inserted before that terminator, so a missing destroy
on an ordinary exit path is impossible. The interpreter prunes weak-reference
targets once their last `Weak` is gone and drops finished task handles before
adding new ones; the native runtime now unlinks a weak ref from its target when
the weak ref is freed.

## Native side (iris_ui_host)

`crates/iris_ui_host` reuses its framebuffer, font system, clip mask and image
premultiplication scratch across frames, and only re-sends the accessibility
tree to the platform adapter when the widget tree actually changes. Frames that
are visually identical cost almost no allocation.

## Compile-time ownership checks for records

`CfgBorrowCheckerPass` tracks moves and loans over the CFG with non-lexical
lifetimes. Move state is keyed by a place (binding slot plus field/tuple
projection path), so `move p.a` invalidates only `p.a`: `p.b` stays usable, while
moving the whole `p` after `p.a` is rejected. Reads of a field are recorded with
their projection path as well, so a partial move does not poison unrelated
fields. A whole-binding reassignment restores initialization.

With `IRIS_STRICT_RECORD_OWNERSHIP=1`, by-value record uses become implicit
moves: passing a record to a parameter of record type, initializing a record
field from a record binding, and binding a record to a new `val` all transfer
ownership. The callee's parameter type is the ownership signature — no extra
annotation is needed — and a later use of the source binding is E0382. This mode
is opt-in because existing IRIS code commonly passes record-typed values to
read-only helpers and reuses them. Explicit `move` is checked in both modes.
Borrow parameters (`&T` / `&mut T`) are not moves; the existing NLL loan checks
cover their lifetime at the call site.

Destructor emission skips a value that has been moved out of its binding, so a
transfer gives the destructor obligation to the new owner rather than running it
twice. Fixtures: `tests/test_partial_field_move.iris`,
`tests/test_record_call_move.iris` (positive) and
`tests/test_partial_field_move_error.iris`, `tests/test_record_call_move_error.iris`,
`tests/test_record_binding_move_error.iris` (compile-fail), plus the unit tests
`test_nll_partial_field_moves_are_independent` and
`test_raii_explicit_move_drops_new_owner_once`.

## How this differs from Rust

Rust compilers fully staticize ownership: every allocation has a unique owner,
duplicated freely (clones) or moved, and `Drop` runs deterministically — that is
how a Rust desktop app can sit near ~300 MB indefinitely. IRIS cannot match
Rust's static discipline for records yet, so its GUI idiom is:

* reference counting for heap values (close to `Rc<T>`/`Arc<T>` semantics), and
* explicit frame pools instead of full borrow-checked move semantics.

The practical discipline above gets the same steady-state behavior Rust apps
enjoy, as long as every transient lives inside a pool and long-lived state is
kept shallow.
