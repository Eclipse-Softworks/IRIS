//! HIR → IR lowering.
//!
//! The HIR (src/hir) is built from the parser's AST after the AST passes: it
//! resolves every value name and registers every expression, then the source
//! checker types it and its report is absorbed into it. THIR (src/thir) is
//! built from the HIR after the checker and owns the typed, desugared tree.
//! Lowering reads solved `-> Infer` returns from the HIR and type hints from
//! THIR (the HIR is a fallback only for lowering-synthesized spans), and it
//! consumes THIR's record field indices, map key/value types, and
//! tuple-destructuring shape. The body walk still pattern-matches the AST for
//! control flow and the remaining forms, so types flow downhill into the CFG
//! without being re-derived from it.
//!
//! The lowerer walks the checked AST (the HIR's node storage) and constructs
//! an `IrModule` using `IrFunctionBuilder`. Each function is lowered
//! independently. Variable bindings are tracked in a lexical scope map
//! (name → ValueId).
//!
//! Type propagation: for scalar operations where operand types are fully known
//! at construction time, the concrete type is used immediately, and `IrType`
//! has no "unknown" variant: nothing lowers to an untyped value (§4.2).

mod calls;
mod errors;
mod generics;
pub mod graph;
pub(crate) mod hints;
pub mod ir_from_graph;
mod list_hofs;
mod loops;
mod ml_builtins;
pub(crate) mod mono;
mod patterns;
mod rebound;
mod thir_bridge;
mod thir_expr;
pub(crate) mod typeck;
mod types;
pub use self::generics::{
    collect_generic_apps_in_block, collect_generic_apps_in_expr, collect_generic_apps_in_stmt,
    collect_generic_apps_in_type,
};
use self::generics::{
    extract_from_ast_type, infer_generic_fn_subs, lower_function_with_generics,
    lower_function_with_generics_and_subs, mangle_param_list, split_generic_param_names,
};
use self::types::{
    assignable, ast_attr_to_ir_attr, ast_type_mentions_assoc, const_arg_usize,
    derive_einsum_result_type, eval_const_usize_expr, fill_trait_object_methods,
    ir_type_dispatch_name, lower_binop, op_trait_method, parse_mangled_type, resolve_enum_refs,
    returns_a_value, same_constructor, type_name_to_ir_type,
};
pub(crate) use self::types::{
    is_inferred_return, mangle_ir_type, resolve_concrete_field, resolve_mangled_struct,
};
pub use self::types::{
    lower_type, lower_type_with_structs, populate_struct_fields, resolve_ast_type_with_subs,
    substitute_ast_type, substitute_ast_type_with_consts,
};
pub use graph::lower_model;
pub use ir_from_graph::lower_graph_to_ir;

pub(crate) use self::errors::binop_mismatch_diag;
use self::errors::{
    binop_mismatch, binop_mismatch_thir, builtin_arity_error, method_on_builtin_type,
    method_on_builtin_type_thir, no_such_field, not_indexable, signature_of, type_not_lowerable,
    undetermined_return, unknown_impl_type,
};
use self::rebound::{
    collect_rebound_vars_in_block, collect_rebound_vars_in_expr, collect_rebound_vars_in_thir_expr,
    find_captured_mutation, find_captured_mutation_thir, find_rebound_vars,
    find_rebound_vars_thir_block, taped_inline_ok_block,
};
use self::thir_bridge::{
    get_qualified_enum_name, get_qualified_enum_name_thir, synth_marker_at, synth_marker_thir_expr,
    thir_block_migratable, thir_expr_migratable, thir_expr_uses_name, thir_matches_ast,
    thir_to_ast, SynthMarker, IN_THIR_LOWER, SYNTH_MARK,
};

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

/// Runtime builtins lowered to a call of the C runtime: the runtime's name
/// for each, and the type it returns. Shared by lowering and the source
/// type checker, so the two cannot disagree about a builtin's result.
pub(crate) fn runtime_builtin(name: &str) -> Option<(&'static str, IrType)> {
    match name {
        // HTTP
        "http_get" => Some(("http_get", IrType::Str)),
        "http_post" => Some(("http_post", IrType::Str)),
        // JSON (json_parse is in stdlib; json_stringify is a new builtin)
        "json_stringify" => Some(("json_stringify", IrType::Str)),
        // Regex
        "regex_match" => Some(("regex_match", IrType::Scalar(DType::Bool))),
        "regex_find_all" => Some(("regex_find_all", IrType::List(Box::new(IrType::Str)))),
        "regex_replace" => Some(("regex_replace", IrType::Str)),
        "regex_replace_all" => Some(("regex_replace_all", IrType::Str)),
        // DateTime
        "datetime_now" => Some(("datetime_now", IrType::Str)),
        "datetime_timestamp" => Some(("datetime_timestamp", IrType::Scalar(DType::F64))),
        "datetime_format" => Some(("datetime_format", IrType::Str)),
        // OS / Path (path_exists is in stdlib fs.iris)
        "cwd" => Some(("cwd", IrType::Str)),
        "list_dir" => Some(("listdir", IrType::List(Box::new(IrType::Str)))),
        "path_join" => Some(("path_join", IrType::Str)),
        "mkdir" => Some(("mkdir", IrType::Scalar(DType::Bool))),
        "remove_file" => Some(("remove_file", IrType::Scalar(DType::Bool))),
        // Type introspection
        "type_of" => Some(("type_of", IrType::Str)),
        // Random
        "random" => Some(("random", IrType::Scalar(DType::F64))),
        "random_range" => Some(("random_range", IrType::Scalar(DType::I64))),
        // Seeding. `seed(n)` returns n so a run can log the seed it
        // used; `random_seed()` reports the seed in effect, generating
        // one if none was set. Together they make an evolved system
        // reproducible -- print the seed, feed it back, get the same
        // system. Without that, a self-evolving program cannot be
        // audited or replayed after a failure.
        "seed" => Some(("seed", IrType::Scalar(DType::I64))),
        "random_seed" => Some(("random_seed", IrType::Scalar(DType::I64))),
        // Hashing / Encoding
        "hash" => Some(("hash", IrType::Scalar(DType::I64))),
        "base64_encode" => Some(("base64_encode", IrType::Str)),
        "base64_decode" => Some(("base64_decode", IrType::Str)),
        // String extras
        "char_at" => Some(("char_at", IrType::Str)),
        "str_reverse" => Some(("str_reverse", IrType::Str)),

        // ── Phase 105: Async/Concurrency extensions ──
        "chan_len" => Some(("chan_len", IrType::Scalar(DType::I64))),
        "select" => Some(("select", IrType::Scalar(DType::I64))),
        "timeout" => Some(("timeout", IrType::Scalar(DType::Bool))),
        "thread_count" => Some(("thread_count", IrType::Scalar(DType::I64))),
        // `par_map`, `weak_ref` and `async_recv` take their result types from
        // their arguments: each has its own lowering, not an entry here.

        // ── Phase 105: Deque (double-ended queue) ──
        "deque_new" => Some((
            "deque_new",
            IrType::List(Box::new(IrType::Scalar(DType::I64))),
        )),
        "deque_push_front" => Some((
            "deque_push_front",
            IrType::List(Box::new(IrType::Scalar(DType::I64))),
        )),
        "deque_push_back" => Some((
            "deque_push_back",
            IrType::List(Box::new(IrType::Scalar(DType::I64))),
        )),
        "deque_pop_front" => Some(("deque_pop_front", IrType::Scalar(DType::I64))),
        "deque_pop_back" => Some(("deque_pop_back", IrType::Scalar(DType::I64))),
        "deque_len" => Some(("deque_len", IrType::Scalar(DType::I64))),
        "deque_front" => Some(("deque_front", IrType::Scalar(DType::I64))),
        "deque_back" => Some(("deque_back", IrType::Scalar(DType::I64))),

        // ── Phase 105: Sorted collection helpers ──
        "sorted_keys" => Some(("sorted_keys", IrType::List(Box::new(IrType::Str)))),

        // ── Phase 105: BitSet ──
        "bitset_new" => Some((
            "bitset_new",
            IrType::List(Box::new(IrType::Scalar(DType::I64))),
        )),
        "bitset_set" => Some((
            "bitset_set",
            IrType::List(Box::new(IrType::Scalar(DType::I64))),
        )),
        "bitset_get" => Some(("bitset_get", IrType::Scalar(DType::Bool))),
        "bitset_count" => Some(("bitset_count", IrType::Scalar(DType::I64))),
        "bitset_clear" => Some((
            "bitset_clear",
            IrType::List(Box::new(IrType::Scalar(DType::I64))),
        )),

        // ── Phase 105: FFI (dynamic library loading) ──
        "ffi_open" => Some(("ffi_open", IrType::Scalar(DType::I64))),
        "ffi_call" => Some(("ffi_call", IrType::Scalar(DType::I64))),
        "ffi_close" => Some(("ffi_close", IrType::Scalar(DType::Bool))),

        // ── Phase 106: Expanded FFI — C / Python / Rust ──
        // C FFI with typed arguments
        "ffi_call_i64" => Some(("ffi_call_i64", IrType::Scalar(DType::I64))),
        "ffi_call_f64" => Some(("ffi_call_f64", IrType::Scalar(DType::F64))),
        "ffi_call_str" => Some(("ffi_call_str", IrType::Str)),
        "ffi_call_void" => Some(("ffi_call_void", IrType::Scalar(DType::I64))),
        "ffi_call_args" => Some(("ffi_call_args", IrType::Scalar(DType::I64))),
        // FFI out-parameter cells. A pointer fits in the int64 slot the
        // dispatcher already passes, so these need no new calling
        // convention — only a way for IRIS to own memory and name its
        // address. Without them every C function returning through a
        // pointer was unreachable.
        "ffi_out_new" => Some(("ffi_out_new", IrType::Scalar(DType::I64))),
        "ffi_out_free" => Some(("ffi_out_free", IrType::Scalar(DType::I64))),
        "ffi_out_get_f64" => Some(("ffi_out_get_f64", IrType::Scalar(DType::F64))),
        "ffi_out_get_i64" => Some(("ffi_out_get_i64", IrType::Scalar(DType::I64))),
        "ffi_out_get_str" => Some(("ffi_out_get_str", IrType::Str)),
        "ffi_out_set_f64" => Some(("ffi_out_set_f64", IrType::Scalar(DType::I64))),
        "ffi_out_set_i64" => Some(("ffi_out_set_i64", IrType::Scalar(DType::I64))),
        // Python FFI
        "python_eval" => Some(("python_eval", IrType::Str)),
        "python_exec" => Some(("python_exec", IrType::Scalar(DType::I64))),
        "python_call" => Some(("python_call", IrType::Str)),
        "python_version" => Some(("python_version", IrType::Str)),
        // Rust FFI (cdylib — uses same dlopen mechanism as C)
        "rust_lib_open" => Some(("rust_lib_open", IrType::Scalar(DType::I64))),
        "rust_call_i64" => Some(("rust_call_i64", IrType::Scalar(DType::I64))),
        "rust_call_f64" => Some(("rust_call_f64", IrType::Scalar(DType::F64))),
        "rust_call_void" => Some(("rust_call_void", IrType::Scalar(DType::I64))),

        // ── Phase 105: OS / System (env, exec, pid) ──
        "env_get" => Some(("env_get", IrType::Str)),
        "env_set" => Some(("env_set", IrType::Scalar(DType::Bool))),
        "exit_code" => Some(("exit_code", IrType::Scalar(DType::I64))),
        "exec_cmd" => Some(("exec_cmd", IrType::Str)),
        "pid" => Some(("pid", IrType::Scalar(DType::I64))),

        // ── Phase 105: Crypto / UUID ──
        "uuid" => Some(("uuid", IrType::Str)),
        "sha256" => Some(("sha256", IrType::Str)),
        "hex_encode" => Some(("hex_encode", IrType::Str)),

        // ── File Stream API ──
        "file_open" => Some(("file_open", IrType::Scalar(DType::I64))),
        "file_close" => Some(("file_close", IrType::Scalar(DType::Bool))),
        "file_read" => Some(("file_read", IrType::Str)),
        "file_write" => Some(("file_write", IrType::Scalar(DType::Bool))),
        "hex_decode" => Some(("hex_decode", IrType::Str)),

        // ── Phase 105: String extras ──
        "str_pad_left" => Some(("str_pad_left", IrType::Str)),
        "str_pad_right" => Some(("str_pad_right", IrType::Str)),
        "str_chars" => Some(("str_chars", IrType::List(Box::new(IrType::Str)))),
        "str_bytes" => Some((
            "str_bytes",
            IrType::List(Box::new(IrType::Scalar(DType::I64))),
        )),
        // Binary-safe: every byte, NULs included; empty if unreadable.
        "file_read_bytes" => Some((
            "file_read_bytes",
            IrType::List(Box::new(IrType::Scalar(DType::I64))),
        )),
        "str_count" => Some(("str_count", IrType::Scalar(DType::I64))),

        // ── Phase 105: Math constants and predicates ──
        "math_pi" => Some(("math_pi", IrType::Scalar(DType::F64))),
        "math_e" => Some(("math_e", IrType::Scalar(DType::F64))),
        "math_inf" => Some(("math_inf", IrType::Scalar(DType::F64))),
        "is_nan" => Some(("is_nan", IrType::Scalar(DType::Bool))),
        "is_inf" => Some(("is_inf", IrType::Scalar(DType::Bool))),

        // ── Phase 105: Functional list operations ──
        "list_map" => Some((
            "list_map",
            IrType::List(Box::new(IrType::Scalar(DType::I64))),
        )),
        "list_filter" => Some((
            "list_filter",
            IrType::List(Box::new(IrType::Scalar(DType::I64))),
        )),
        "list_reduce" => Some(("list_reduce", IrType::Scalar(DType::I64))),
        "list_any" => Some(("list_any", IrType::Scalar(DType::Bool))),
        "list_all" => Some(("list_all", IrType::Scalar(DType::Bool))),
        "list_zip" => Some((
            "list_zip",
            IrType::List(Box::new(IrType::Scalar(DType::I64))),
        )),
        "list_enumerate" => Some((
            "list_enumerate",
            IrType::List(Box::new(IrType::Scalar(DType::I64))),
        )),
        "list_flatten" => Some((
            "list_flatten",
            IrType::List(Box::new(IrType::Scalar(DType::I64))),
        )),
        "list_unique" => Some((
            "list_unique",
            IrType::List(Box::new(IrType::Scalar(DType::I64))),
        )),
        "list_reverse" => Some((
            "list_reverse",
            IrType::List(Box::new(IrType::Scalar(DType::I64))),
        )),
        "list_sorted" => Some((
            "list_sorted",
            IrType::List(Box::new(IrType::Scalar(DType::I64))),
        )),
        "list_sum" => Some(("list_sum", IrType::Scalar(DType::F64))),
        "list_min" => Some(("list_min", IrType::Scalar(DType::I64))),
        "list_max" => Some(("list_max", IrType::Scalar(DType::I64))),
        "list_index_of" => Some(("list_index_of", IrType::Scalar(DType::I64))),
        "list_count" => Some(("list_count", IrType::Scalar(DType::I64))),
        "list_take" => Some((
            "list_take",
            IrType::List(Box::new(IrType::Scalar(DType::I64))),
        )),
        "list_drop" => Some((
            "list_drop",
            IrType::List(Box::new(IrType::Scalar(DType::I64))),
        )),

        // ── Terminal / Interactive Input ──
        "read_key" => Some(("read_key", IrType::Scalar(DType::I64))),
        "read_password" => Some(("read_password", IrType::Str)),
        "term_clear" => Some(("term_clear", IrType::Scalar(DType::I64))),
        "term_cursor" => Some(("term_cursor", IrType::Scalar(DType::I64))),
        "term_show_cursor" => Some(("term_show_cursor", IrType::Scalar(DType::I64))),
        "term_set_color" => Some(("term_set_color", IrType::Scalar(DType::I64))),
        "term_reset" => Some(("term_reset", IrType::Scalar(DType::I64))),
        "term_rows" => Some(("term_rows", IrType::Scalar(DType::I64))),
        "term_cols" => Some(("term_cols", IrType::Scalar(DType::I64))),

        // ── UDP Networking ──
        "udp_open" => Some(("udp_open", IrType::Scalar(DType::I64))),
        "udp_send" => Some(("udp_send", IrType::Scalar(DType::I64))),
        "udp_recv" => Some(("udp_recv", IrType::Str)),
        "udp_close" => Some(("udp_close", IrType::Scalar(DType::I64))),

        // ── HTTP extended ──
        "http_request" => Some(("http_request", IrType::Str)),
        "http_post_json" => Some(("http_post_json", IrType::Str)),

        // ── Weak references ──
        "weak_alive" => Some(("weak_alive", IrType::Scalar(DType::Bool))),

        // ── GC ──
        "gc_stats" => Some((
            "gc_stats_map",
            IrType::Map(Box::new(IrType::Str), Box::new(IrType::Scalar(DType::I64))),
        )),
        "gc_collect" => Some(("gc_collect_call", IrType::Scalar(DType::Bool))),

        // ── Map extras ──
        "map_entries" => Some(("map_entries", IrType::List(Box::new(IrType::Str)))),

        // ── Async tasks (see pass::async_lower) ──
        // Leaf tasks the executor completes itself; `await` them.
        "async_sleep" => Some((
            "async_sleep",
            IrType::Task(Box::new(IrType::Scalar(DType::I64))),
        )),
        "async_yield" => Some((
            "async_yield",
            IrType::Task(Box::new(IrType::Scalar(DType::I64))),
        )),
        // Completes when a socket can be read without blocking.
        "async_readable" => Some((
            "async_readable",
            IrType::Task(Box::new(IrType::Scalar(DType::I64))),
        )),
        // Completes when a socket can be written without blocking.
        "async_writable" => Some((
            "async_writable",
            IrType::Task(Box::new(IrType::Scalar(DType::I64))),
        )),
        "tcp_connect_async" => Some((
            "tcp_connect_async",
            IrType::Task(Box::new(IrType::Scalar(DType::I64))),
        )),
        "tcp_write_async" => Some((
            "tcp_write_async",
            IrType::Task(Box::new(IrType::Scalar(DType::I64))),
        )),
        "http_get_async" => Some(("http_get_async", IrType::Task(Box::new(IrType::Str)))),
        "task_done" => Some(("async_is_done", IrType::Scalar(DType::Bool))),
        // The index of the first of several tasks (or of a list of
        // tasks) to finish; checked below.
        "async_any" => Some((
            "async_any",
            IrType::Task(Box::new(IrType::Scalar(DType::I64))),
        )),
        "task_cancel" => Some(("async_cancel", IrType::Scalar(DType::I64))),
        "task_cancelled" => Some(("async_is_cancelled", IrType::Scalar(DType::Bool))),

        // Transaction status/depth use i64 for a stable native ABI.
        "transaction_begin" => Some(("transaction_begin", IrType::Scalar(DType::I64))),
        "transaction_commit" => Some(("transaction_commit", IrType::Scalar(DType::I64))),
        "transaction_rollback" => Some(("transaction_rollback", IrType::Scalar(DType::I64))),
        "transaction_depth" => Some(("transaction_depth", IrType::Scalar(DType::I64))),

        // Native differentiable tensors. Shape is runtime-owned; the
        // symbolic dimension keeps the value pointer-typed through IR.
        "dtensor" => Some((
            "tensor_from_lists",
            IrType::Tensor {
                dtype: DType::F32,
                shape: crate::ir::types::Shape(vec![crate::ir::types::Dim::Symbolic("N".into())]),
            },
        )),
        "dtensor_tape" => Some((
            "tensor_tape",
            IrType::Tensor {
                dtype: DType::F32,
                shape: crate::ir::types::Shape(vec![crate::ir::types::Dim::Symbolic("N".into())]),
            },
        )),
        "dtensor_add" => Some((
            "tensor_add",
            IrType::Tensor {
                dtype: DType::F32,
                shape: crate::ir::types::Shape(vec![crate::ir::types::Dim::Symbolic("N".into())]),
            },
        )),
        "dtensor_sub" => Some((
            "tensor_sub",
            IrType::Tensor {
                dtype: DType::F32,
                shape: crate::ir::types::Shape(vec![crate::ir::types::Dim::Symbolic("N".into())]),
            },
        )),
        "dtensor_mul" => Some((
            "tensor_mul",
            IrType::Tensor {
                dtype: DType::F32,
                shape: crate::ir::types::Shape(vec![crate::ir::types::Dim::Symbolic("N".into())]),
            },
        )),
        "dtensor_div" => Some((
            "tensor_div",
            IrType::Tensor {
                dtype: DType::F32,
                shape: crate::ir::types::Shape(vec![crate::ir::types::Dim::Symbolic("N".into())]),
            },
        )),
        "dtensor_matmul" => Some((
            "tensor_matmul",
            IrType::Tensor {
                dtype: DType::F32,
                shape: crate::ir::types::Shape(vec![crate::ir::types::Dim::Symbolic("N".into())]),
            },
        )),
        "dtensor_relu" => Some((
            "tensor_relu",
            IrType::Tensor {
                dtype: DType::F32,
                shape: crate::ir::types::Shape(vec![crate::ir::types::Dim::Symbolic("N".into())]),
            },
        )),
        "dtensor_sigmoid" => Some((
            "tensor_sigmoid",
            IrType::Tensor {
                dtype: DType::F32,
                shape: crate::ir::types::Shape(vec![crate::ir::types::Dim::Symbolic("N".into())]),
            },
        )),
        "dtensor_tanh" => Some((
            "tensor_tanh_act",
            IrType::Tensor {
                dtype: DType::F32,
                shape: crate::ir::types::Shape(vec![crate::ir::types::Dim::Symbolic("N".into())]),
            },
        )),
        "dtensor_sum" => Some((
            "tensor_sum_all",
            IrType::Tensor {
                dtype: DType::F32,
                shape: crate::ir::types::Shape(vec![crate::ir::types::Dim::Symbolic("N".into())]),
            },
        )),
        "dtensor_grad" => Some((
            "tensor_grad",
            IrType::Tensor {
                dtype: DType::F32,
                shape: crate::ir::types::Shape(vec![crate::ir::types::Dim::Symbolic("N".into())]),
            },
        )),
        "dtensor_backward" => Some(("tensor_backward", IrType::Scalar(DType::I64))),
        "dtensor_item" => Some(("tensor_item", IrType::Scalar(DType::F64))),
        "dtensor_to_list" => Some((
            "tensor_to_list",
            IrType::List(Box::new(IrType::Scalar(DType::F64))),
        )),

        // ── Reflection & Dynamic Evaluation ──
        "iris_eval" | "reflect_eval" => Some((
            "eval",
            IrType::ResultType(Box::new(IrType::Str), Box::new(IrType::Str)),
        )),
        "iris_eval_i64" | "reflect_eval_i64" => Some((
            "eval_i64",
            IrType::ResultType(Box::new(IrType::Scalar(DType::I64)), Box::new(IrType::Str)),
        )),
        "iris_validate" | "reflect_validate" => Some(("validate", IrType::Scalar(DType::Bool))),
        "iris_reflection_available" | "reflect_available" => {
            Some(("reflection_available", IrType::Scalar(DType::Bool)))
        }

        _ => None,
    }
}

thread_local! {
    /// Function names defined by the source currently being lowered.
    ///
    /// `fn_sigs` cannot answer "did the user write this?" because it is
    /// pre-populated with builtin return types (println, print, sleep_ms, ...)
    /// so call sites get concrete types. Deciding whether a definition shadows
    /// a builtin needs the set the *source* actually declares. See #15.
    static CURRENT_USER_FNS: RefCell<std::collections::HashSet<String>> =
        RefCell::new(std::collections::HashSet::new());
    /// Every non-generic function's AST, so a call carrying a reverse-mode tape
    /// value can be lowered inline and keep its handle. A tape handle cannot be
    /// passed as an ordinary argument -- it has no place in an `f64` parameter --
    /// and multi-value returns are not supported, so the handle cannot come back
    /// out either. Inlining at lowering time sidesteps both: `tape_nodes` is
    /// keyed by `ValueId`, so binding the callee's parameters to the caller's
    /// argument values carries the mapping across for free. See #49.
    static CURRENT_FN_ASTS: RefCell<HashMap<String, crate::parser::ast::AstFunction>> =
        RefCell::new(HashMap::new());
    /// `(trait, method)` pairs left out of the trait's `dyn` signature: their
    /// types name the trait's associated types, which only an impl knows.
    static DYN_UNCALLABLE: RefCell<HashSet<(String, String)>> = RefCell::new(HashSet::new());
    /// Mangled names of non-`pub` items of brought modules, published from the
    /// merged AST at the start of lowering. It has to be set here rather than
    /// where the mangling happens, because compilation runs on a spawned thread
    /// and the two are not the same thread. See #13.
    static CURRENT_PRIVATE_ITEMS: RefCell<std::collections::HashSet<String>> =
        RefCell::new(std::collections::HashSet::new());
    static CURRENT_BRING_PREFIXES: RefCell<Vec<Vec<String>>> = const { RefCell::new(Vec::new()) };
    static CURRENT_BRING_MAPPINGS: RefCell<Vec<HashMap<String, String>>> = const { RefCell::new(Vec::new()) };
}

fn set_current_brings(brings: &[crate::parser::ast::AstBring]) {
    let mut prefixes = Vec::new();
    let mut mappings = HashMap::new();
    for bring in brings {
        match &bring.path {
            crate::parser::ast::BringPath::File(rel_path) => {
                let path_obj = std::path::Path::new(rel_path);
                let stem = path_obj
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("module")
                    .replace(['.', '-'], "_");
                if !prefixes.contains(&stem) {
                    prefixes.push(stem.clone());
                }
                mappings.insert(stem.clone(), stem);
            }
            crate::parser::ast::BringPath::Stdlib(name) => {
                let mangled_prefix = name.replace(['.', '-'], "_");
                if !prefixes.contains(&mangled_prefix) {
                    prefixes.push(mangled_prefix.clone());
                }
                let qualifier = if let Some(last_dot) = name.rfind('.') {
                    &name[last_dot + 1..]
                } else {
                    name.as_str()
                };
                if !prefixes.contains(&qualifier.to_string()) {
                    prefixes.push(qualifier.to_string());
                }
                mappings.insert(qualifier.to_string(), mangled_prefix);
            }
        }
    }
    CURRENT_BRING_PREFIXES.with(|p| {
        p.borrow_mut().push(prefixes);
    });
    CURRENT_BRING_MAPPINGS.with(|m| {
        m.borrow_mut().push(mappings);
    });
}

fn clear_current_brings() {
    CURRENT_BRING_PREFIXES.with(|p| {
        p.borrow_mut().pop();
    });
    CURRENT_BRING_MAPPINGS.with(|m| {
        m.borrow_mut().pop();
    });
}

fn resolve_qualifier(qualifier: &str) -> String {
    CURRENT_BRING_MAPPINGS.with(|m| {
        m.borrow()
            .last()
            .and_then(|map| map.get(qualifier))
            .cloned()
            .unwrap_or_else(|| qualifier.to_string())
    })
}

/// Whether `name` is a brought module qualifier (`string`, `collections`,
/// ...) rather than a value. HIR resolution records these so a qualified
/// call's base is not an unresolved name.
pub(crate) fn is_bring_qualifier(name: &str) -> bool {
    CURRENT_BRING_MAPPINGS.with(|m| m.borrow().last().is_some_and(|map| map.contains_key(name)))
        || CURRENT_BRING_PREFIXES.with(|p| {
            p.borrow()
                .last()
                .is_some_and(|prefixes| prefixes.iter().any(|prefix| prefix == name))
        })
}

pub(crate) fn resolve_brought_name(name: &str, module: &IrModule) -> String {
    let resolved = CURRENT_BRING_PREFIXES.with(|prefixes_stack| {
        if let Some(prefixes) = prefixes_stack.borrow().last() {
            for prefix in prefixes.iter() {
                let candidate = format!("{}__{}", prefix, name);
                if CURRENT_PRIVATE_ITEMS.with(|p| p.borrow().contains(&candidate)) {
                    continue;
                }
                if module.struct_def(&candidate).is_some()
                    || module.enum_def(&candidate).is_some()
                    || module.type_alias(&candidate).is_some()
                {
                    return Some(candidate);
                }
            }
        }
        None
    });
    if let Some(res) = resolved {
        return res;
    }
    if module.struct_def(name).is_some()
        || module.enum_def(name).is_some()
        || module.type_alias(name).is_some()
    {
        return name.to_string();
    }
    // Fallback: scan all type registries for any mangled candidate matching `*__name`.
    let suffix = format!("__{}", name);
    let mut candidates: Vec<String> = Vec::new();
    for key in module.struct_defs.keys() {
        if key.ends_with(&suffix) {
            candidates.push(key.clone());
        }
    }
    for key in module.enum_defs.keys() {
        if key.ends_with(&suffix) {
            candidates.push(key.clone());
        }
    }
    for key in module.type_aliases.keys() {
        if key.ends_with(&suffix) {
            candidates.push(key.clone());
        }
    }
    CURRENT_PRIVATE_ITEMS.with(|p| {
        let priv_set = p.borrow();
        candidates.retain(|c| !priv_set.contains(c));
    });
    if !candidates.is_empty() {
        candidates.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        return candidates.remove(0);
    }
    name.to_string()
}

use crate::error::LowerError;
use crate::ir::block::BlockId;
use crate::ir::function::Param;
use crate::ir::instr::{BinOp, IrInstr, ScalarUnaryOp};
use crate::ir::module::{IrFunctionBuilder, IrModule};
use crate::ir::types::{DType, IrType, TraitMethodSig};
use crate::ir::value::ValueId;
use crate::parser::ast::{
    AstBinOp, AstBlock, AstExpr, AstHandlerArm, AstModule, AstStmt, AstType, AstUnaryOp, Ident,
};
use crate::parser::lexer::Span;

/// Lower an `AstModule` to an `IrModule`.
pub fn lower(ast: &AstModule, module_name: &str) -> Result<IrModule, LowerError> {
    typeck::clear_hints();
    set_current_brings(&ast.brings);
    CURRENT_PRIVATE_ITEMS.with(|p| {
        *p.borrow_mut() = ast.private_items.clone();
    });
    struct ScopeGuard;
    impl Drop for ScopeGuard {
        fn drop(&mut self) {
            DYN_UNCALLABLE.with(|d| d.borrow_mut().clear());
            clear_current_brings();
            CURRENT_PRIVATE_ITEMS.with(|p| {
                p.borrow_mut().clear();
            });
        }
    }
    let _guard = ScopeGuard;

    let mut module = IrModule::new(module_name);

    // 0. Register type aliases so structs/functions can reference them.
    for alias in &ast.type_aliases {
        let ir_ty = lower_type(&alias.ty)?;
        module
            .add_type_alias(alias.name.clone(), ir_ty)
            .map_err(|_| LowerError::DuplicateFunction {
                name: alias.name.clone(),
                span: alias.span,
            })?;
    }

    // 1. Register enum definitions so functions can reference them.
    //
    // A payload may name an enum -- its own (`choice Nat { Zero, Succ(Nat) }`)
    // or one declared later. `lower_type` knows no enums and made that an
    // empty record, so native code unboxed the payload as a record and
    // failed with "unbox_native_object type mismatch (tag=18)".
    let enum_variants: HashMap<String, Vec<String>> = ast
        .enums
        .iter()
        .map(|e| {
            (
                e.name.name.clone(),
                e.variants.iter().map(|v| v.name.name.clone()).collect(),
            )
        })
        .collect();
    for e in &ast.enums {
        let variants: Vec<String> = e.variants.iter().map(|v| v.name.name.clone()).collect();
        let variant_fields: Vec<Vec<IrType>> = e
            .variants
            .iter()
            .map(|v| {
                v.fields
                    .iter()
                    .map(|t| Ok(resolve_enum_refs(lower_type(t)?, &enum_variants)))
                    .collect::<Result<Vec<_>, LowerError>>()
            })
            .collect::<Result<_, _>>()?;
        module
            .add_enum_def(e.name.name.clone(), variants, variant_fields)
            .map_err(|_| LowerError::DuplicateFunction {
                name: e.name.name.clone(),
                span: e.name.span,
            })?;
    }

    // 2. Register struct definitions so functions can reference them.
    // Also, pre-collect and monomorphize generic struct applications on demand.
    let mut generic_apps = std::collections::HashSet::new();
    for s in &ast.structs {
        for f in &s.fields {
            collect_generic_apps_in_type(&f.ty, &mut generic_apps);
        }
    }
    for f in &ast.functions {
        for p in &f.params {
            collect_generic_apps_in_type(&p.ty, &mut generic_apps);
        }
        collect_generic_apps_in_type(&f.return_ty, &mut generic_apps);
        collect_generic_apps_in_block(&f.body, &mut generic_apps);
    }
    for i in &ast.impls {
        for m in &i.methods {
            for p in &m.params {
                collect_generic_apps_in_type(&p.ty, &mut generic_apps);
            }
            collect_generic_apps_in_type(&m.return_ty, &mut generic_apps);
            collect_generic_apps_in_block(&m.body, &mut generic_apps);
        }
    }

    let const_defs_map: HashMap<String, AstExpr> = ast
        .consts
        .iter()
        .map(|c| (c.name.name.clone(), c.value.clone()))
        .collect();
    let mut const_ints_tmp: HashMap<String, usize> = HashMap::new();
    for _ in 0..=ast.consts.len() {
        let mut progressed = false;
        for c in &ast.consts {
            if !const_ints_tmp.contains_key(&c.name.name) {
                if let Some(val) = eval_const_usize_expr(&c.value, &const_ints_tmp) {
                    const_ints_tmp.insert(c.name.name.clone(), val);
                    if let Ok(iv) = i64::try_from(val) {
                        module.const_ints.insert(c.name.name.clone(), iv);
                    }
                    progressed = true;
                }
            }
        }
        if !progressed {
            break;
        }
    }

    let mut generic_struct_templates = std::collections::HashMap::new();
    for s in &ast.structs {
        if !s.type_params.is_empty() {
            generic_struct_templates.insert(s.name.name.clone(), s.clone());
            module.generic_struct_names.insert(s.name.name.clone());
            module
                .generic_struct_asts
                .insert(s.name.name.clone(), s.clone());
        }
    }

    // Pre-register all struct names so forward references and brought module
    // record field references can be resolved by resolve_brought_name.
    for s in &ast.structs {
        let _ = module.add_struct_def(s.name.name.clone(), Vec::new());
    }

    // Register non-generic struct definitions.
    for s in &ast.structs {
        if s.type_params.is_empty() {
            let fields: Vec<(String, IrType)> = s
                .fields
                .iter()
                .map(|f| {
                    Ok((
                        f.name.name.clone(),
                        lower_type_with_structs(&f.ty, &module)?,
                    ))
                })
                .collect::<Result<_, LowerError>>()?;
            let defaults: Vec<Option<AstExpr>> =
                s.fields.iter().map(|f| f.default.clone()).collect();
            module.struct_defs.insert(s.name.name.clone(), fields);
            if defaults.iter().any(|d| d.is_some()) {
                module.struct_defaults.insert(s.name.name.clone(), defaults);
            }
        }
    }

    // Register generic struct templates so field access works inside generic
    // functions.  We use `lower_type_with_structs` (without Infer substitution)
    // so type params appear as `Struct { name: "T", fields: [] }` — a marker
    // that can be substituted with the concrete type at monomorphization time.
    // Sorted, because `generic_struct_templates` is a `HashMap` and the order
    // in which templates are registered decides the order their monomorphised
    // instantiations are created. With a nested generic that changed the
    // resulting IR between runs of the same program. See known-issues #21.
    let mut template_names: Vec<&String> = generic_struct_templates.keys().collect();
    template_names.sort();
    for name in template_names {
        let template = &generic_struct_templates[name];
        let fields: Vec<(String, IrType)> = template
            .fields
            .iter()
            .map(|f| {
                Ok((
                    f.name.name.clone(),
                    lower_type_with_structs(&f.ty, &module)?,
                ))
            })
            .collect::<Result<_, LowerError>>()?;
        let defaults: Vec<Option<AstExpr>> =
            template.fields.iter().map(|f| f.default.clone()).collect();
        // The first registration pass deliberately inserted an empty
        // placeholder for every struct name. Populate that placeholder here;
        // `add_struct_def` would reject it as a duplicate and silently leave
        // generic templates with zero fields, breaking every later `.field`
        // access on a monomorphized record.
        module.struct_defs.insert(name.clone(), fields);
        if defaults.iter().any(|d| d.is_some()) {
            module.struct_defaults.insert(name.clone(), defaults);
        }
    }

    let mut worklist: Vec<(String, Vec<AstType>)> = generic_apps.into_iter().collect();
    let mut processed_apps = std::collections::HashSet::new();

    while let Some((base_name, type_args)) = worklist.pop() {
        if !processed_apps.insert((base_name.clone(), type_args.clone())) {
            continue;
        }
        if let Some(template) = generic_struct_templates.get(&base_name).cloned() {
            let mut type_subs = std::collections::HashMap::new();
            let mut constructor_subs = std::collections::HashMap::new();
            let mut const_subs = std::collections::HashMap::new();
            for (p, arg) in template.type_params.iter().zip(type_args.iter()) {
                match p {
                    crate::parser::ast::AstGenericParam::Type(p_name, _, _) => {
                        type_subs.insert(p_name.clone(), arg.clone());
                    }
                    crate::parser::ast::AstGenericParam::Hkt(p_name, _, _, _) => {
                        let target_constructor = match arg {
                            AstType::Named(n, _) => n.clone(),
                            AstType::Generic { name: n, .. } => n.clone(),
                            _ => "".to_string(),
                        };
                        constructor_subs.insert(p_name.clone(), target_constructor);
                    }
                    crate::parser::ast::AstGenericParam::Const { name: p_name, .. } => {
                        if let Some(val) = const_arg_usize(arg, &const_defs_map) {
                            const_subs.insert(p_name.clone(), val);
                        }
                    }
                }
            }

            let mut substituted_fields = Vec::new();
            let mut local_apps = std::collections::HashSet::new();
            for f in &template.fields {
                let sub_ty = substitute_ast_type_with_consts(
                    &f.ty,
                    &type_subs,
                    &constructor_subs,
                    &const_subs,
                );
                collect_generic_apps_in_type(&sub_ty, &mut local_apps);
                substituted_fields.push((f.name.name.clone(), sub_ty));
            }

            for app in local_apps {
                if !processed_apps.contains(&app) {
                    worklist.push(app.clone());
                }
            }

            // Lower the fields of the monomorphized struct.
            let mut lowered_fields = Vec::new();
            for (f_name, f_ty) in substituted_fields {
                lowered_fields.push((f_name, lower_type_with_structs(&f_ty, &module)?));
            }

            // Register the monomorphized struct definition.
            let mangled_name = format!(
                "{}__{}",
                base_name,
                template
                    .type_params
                    .iter()
                    .zip(type_args.iter())
                    .map(|(p, arg)| match p {
                        crate::parser::ast::AstGenericParam::Const { .. } => {
                            if let Some(val) = const_arg_usize(arg, &const_defs_map) {
                                Ok(val.to_string())
                            } else {
                                lower_type_with_structs(arg, &module).map(|t| mangle_ir_type(&t))
                            }
                        }
                        _ => lower_type_with_structs(arg, &module).map(|t| mangle_ir_type(&t)),
                    })
                    .collect::<Result<Vec<_>, _>>()?
                    .join("_")
            );
            module
                .add_struct_def(mangled_name.clone(), lowered_fields)
                .ok();
            // Carry over defaults from the template (defaults are typically literals).
            if let Some(template_defaults) = module.struct_defaults.get(&base_name).cloned() {
                module
                    .struct_defaults
                    .insert(mangled_name, template_defaults);
            }
        }
    }

    // Perform field population sweep to resolve any empty forward-referenced struct fields.
    let mut updated_defs = module.struct_defs.clone();
    for fields in updated_defs.values_mut() {
        for (_, f_ty) in fields {
            populate_struct_fields(f_ty, &module);
        }
    }
    module.struct_defs = updated_defs;

    // 2b. Register trait definitions so `dyn Trait` IR types can be resolved
    // and vtables can be emitted at codegen. Generic traits (`trait Container[T]`,
    // `trait Mappable[F[_]]`) also record their type/HKT parameters.
    for t in &ast.traits {
        let mut trait_subs: HashMap<String, IrType> = HashMap::new();
        let mut trait_type_params: Vec<String> = Vec::new();
        let mut trait_hkt_params: Vec<String> = Vec::new();
        for gp in &t.type_params {
            match gp {
                crate::parser::ast::AstGenericParam::Type(name, _, _) => {
                    trait_type_params.push(name.clone());
                    trait_subs.insert(name.clone(), IrType::TypeParam(name.clone()));
                }
                crate::parser::ast::AstGenericParam::Hkt(name, _, _, _) => {
                    trait_hkt_params.push(name.clone());
                    trait_subs.insert(name.clone(), IrType::TypeParam(name.clone()));
                }
                crate::parser::ast::AstGenericParam::Const { name, .. } => {
                    trait_type_params.push(name.clone());
                    trait_subs.insert(name.clone(), IrType::TypeParam(name.clone()));
                }
            }
        }
        let mut methods: Vec<TraitMethodSig> = Vec::new();
        for m in &t.methods {
            // A method whose types name an associated type (`Self::Item`)
            // has types only an impl fixes: it is not callable through `dyn`.
            let mentions_assoc = std::iter::once(&m.return_ty)
                .chain(m.params.iter().map(|p| &p.ty))
                .any(ast_type_mentions_assoc);
            if mentions_assoc {
                DYN_UNCALLABLE.with(|d| {
                    d.borrow_mut()
                        .insert((t.name.name.clone(), m.name.name.clone()))
                });
                continue;
            }
            methods.push(TraitMethodSig {
                name: m.name.name.clone(),
                params: m
                    .params
                    .iter()
                    .map(|p| resolve_ast_type_with_subs(&p.ty, &trait_subs, &module))
                    .collect::<Result<_, _>>()?,
                ret: Box::new(resolve_ast_type_with_subs(
                    &m.return_ty,
                    &trait_subs,
                    &module,
                )?),
            });
        }
        if !trait_type_params.is_empty() || !trait_hkt_params.is_empty() {
            module.add_generic_trait_def(
                t.name.name.clone(),
                trait_type_params,
                trait_hkt_params,
                methods.clone(),
            );
        }
        module.add_trait_def(t.name.name.clone(), methods);
    }
    // Records were registered before traits, so a `dyn Trait` field type was
    // built while no trait existed and carries an empty method list. Fill it
    // in now; vtable layout and `DynCall` lowering read it.
    let traits = module.trait_defs().clone();
    for fields in module.struct_defs.values_mut() {
        for (_, field_ty) in fields.iter_mut() {
            fill_trait_object_methods(field_ty, &traits);
        }
    }
    // 2c. Register impl methods so trait_object codegen can find the concrete
    // mangled function name for each `(trait, concrete_struct, method)` triple.
    for impl_def in &ast.impls {
        if impl_def.trait_name.is_empty() {
            continue;
        }
        // Skip blanket impls — they're monomorphized later based on known concrete types.
        if !impl_def.generic_params.is_empty() {
            continue;
        }
        let trait_name = impl_def.trait_name.clone();
        for method in &impl_def.methods {
            let mangled = format!(
                "{}__{}__{}",
                impl_def.trait_name, impl_def.type_name, method.name.name
            );
            module.add_trait_impl_method(
                trait_name.clone(),
                impl_def.type_name.clone(),
                method.name.name.clone(),
                mangled,
            );
        }
    }

    // 3. Pre-collect function return types so call sites get concrete types.
    // Generic functions (with type_params) are excluded from fn_sigs; they're
    // monomorphized on demand during lower_call.
    let mut fn_sigs: HashMap<String, IrType> = HashMap::new();
    // Functions declared `-> Infer`, typed once the checker has solved them.
    let mut inferred_returns: Vec<crate::parser::ast::AstFunction> = Vec::new();
    // Names this program actually defines.
    //
    // Distinct from `fn_sigs`, which is pre-populated below with builtin return
    // types (println, print, sleep_ms, ...) so call sites get concrete types.
    // That makes `fn_sigs` useless for answering "did the user write this?",
    // which is what deciding a builtin shadow requires.
    let mut user_fn_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut generic_fn_map: HashMap<String, crate::parser::ast::AstFunction> = HashMap::new();
    let mut fn_defaults_map: HashMap<String, Vec<Option<crate::parser::ast::AstExpr>>> =
        HashMap::new();
    // Program state: registered before any function is lowered, so every
    // read and write of a state variable lowers to the state builtins.
    let state_init = register_program_state(ast, &mut module)?;

    // Declared parameter names, in order, for every function in the program.
    // Used to resolve named arguments `f(w = 3, h = 4)` to positional slots.
    // Kept separate from `fn_defaults`, which is only populated for functions
    // that actually have defaults.
    let mut fn_param_names_map: HashMap<String, Vec<String>> = HashMap::new();
    // Declared parameter types, used to coerce a concrete value to a trait
    // object at a call site. Built from the AST rather than read back from
    // `IrModule`, because a callee defined later in the file has not been
    // lowered yet when its caller is.
    let mut fn_param_types_map: HashMap<String, Vec<IrType>> = HashMap::new();
    for func in ast.functions.iter().chain(state_init.iter()) {
        user_fn_names.insert(func.name.name.clone());
        if func.type_params.is_empty() {
            // `-> Infer` (the REPL's wrappers): the checker solves it from
            // the body, below; until then the function has no signature.
            if is_inferred_return(&func.return_ty) {
                inferred_returns.push(func.clone());
            } else {
                let mut ret_ty = lower_type_with_structs(&func.return_ty, &module)?;
                if func.is_async {
                    ret_ty = IrType::Task(Box::new(ret_ty));
                }
                fn_sigs.insert(func.name.name.clone(), ret_ty);
            }
        } else {
            generic_fn_map.insert(func.name.name.clone(), func.clone());
        }
        // Also store const fn bodies so we can evaluate calls at compile time.
        if func.is_const {
            generic_fn_map.insert(func.name.name.clone(), func.clone());
        }
        fn_param_names_map.insert(
            func.name.name.clone(),
            func.params.iter().map(|p| p.name.name.clone()).collect(),
        );
        fn_param_types_map.insert(
            func.name.name.clone(),
            func.params
                .iter()
                .map(|p| lower_type_with_structs(&p.ty, &module))
                .collect::<Result<_, _>>()?,
        );
        if func.params.iter().any(|p| p.default.is_some()) {
            fn_defaults_map.insert(
                func.name.name.clone(),
                func.params.iter().map(|p| p.default.clone()).collect(),
            );
        }
    }
    let generic_fns = std::rc::Rc::new(generic_fn_map);
    CURRENT_USER_FNS.with(|u| {
        *u.borrow_mut() = user_fn_names;
    });
    CURRENT_PRIVATE_ITEMS.with(|p| {
        *p.borrow_mut() = ast.private_items.clone();
    });
    CURRENT_FN_ASTS.with(|m| {
        let mut map = m.borrow_mut();
        map.clear();
        for func in &ast.functions {
            if func.type_params.is_empty() {
                map.insert(func.name.name.clone(), func.clone());
            }
        }
    });
    let fn_defaults = std::rc::Rc::new(fn_defaults_map);
    let fn_param_names = std::rc::Rc::new(fn_param_names_map);
    let fn_param_types = std::rc::Rc::new(fn_param_types_map);
    // The REPL's `-> Infer` wrappers: known by name to the checker though
    // they have no signature until their bodies have been checked.
    let inferred_fns = std::rc::Rc::new(
        inferred_returns
            .iter()
            .map(|f| f.name.name.clone())
            .collect::<std::collections::HashSet<_>>(),
    );

    // Pre-populate built-in / runtime function return types so call sites
    // get concrete types.
    fn_sigs
        .entry("println".into())
        .or_insert(IrType::Scalar(DType::I64));
    fn_sigs
        .entry("print".into())
        .or_insert(IrType::Scalar(DType::I64));
    fn_sigs
        .entry("eprintln".into())
        .or_insert(IrType::Scalar(DType::I64));

    fn_sigs
        .entry("eprint".into())
        .or_insert(IrType::Scalar(DType::I64));
    fn_sigs
        .entry("sleep_ms".into())
        .or_insert(IrType::Scalar(DType::I64));
    fn_sigs
        .entry("random_i64".into())
        .or_insert(IrType::Scalar(DType::I64));
    fn_sigs
        .entry("random_f64".into())
        .or_insert(IrType::Scalar(DType::F64));
    fn_sigs
        .entry("time_ms".into())
        .or_insert(IrType::Scalar(DType::I64));
    fn_sigs
        .entry("exit".into())
        .or_insert(IrType::Scalar(DType::I64));
    fn_sigs
        .entry("len".into())
        .or_insert(IrType::Scalar(DType::I64));
    fn_sigs
        .entry("str_len".into())
        .or_insert(IrType::Scalar(DType::I64));
    fn_sigs
        .entry("assert".into())
        .or_insert(IrType::Scalar(DType::I64));
    fn_sigs
        .entry("assert_eq".into())
        .or_insert(IrType::Scalar(DType::I64));
    fn_sigs
        .entry("iris_eval".into())
        .or_insert(IrType::ResultType(
            Box::new(IrType::Str),
            Box::new(IrType::Str),
        ));
    fn_sigs
        .entry("reflect_eval".into())
        .or_insert(IrType::ResultType(
            Box::new(IrType::Str),
            Box::new(IrType::Str),
        ));
    fn_sigs
        .entry("iris_eval_i64".into())
        .or_insert(IrType::ResultType(
            Box::new(IrType::Scalar(DType::I64)),
            Box::new(IrType::Str),
        ));
    fn_sigs
        .entry("reflect_eval_i64".into())
        .or_insert(IrType::ResultType(
            Box::new(IrType::Scalar(DType::I64)),
            Box::new(IrType::Str),
        ));
    fn_sigs
        .entry("iris_validate".into())
        .or_insert(IrType::Scalar(DType::Bool));
    fn_sigs
        .entry("reflect_validate".into())
        .or_insert(IrType::Scalar(DType::Bool));
    fn_sigs
        .entry("iris_reflection_available".into())
        .or_insert(IrType::Scalar(DType::Bool));
    fn_sigs
        .entry("reflect_available".into())
        .or_insert(IrType::Scalar(DType::Bool));

    // 3b. Share global const definitions prepared above with function lowerers.
    let const_defs = std::rc::Rc::new(const_defs_map);

    // 3c. Process impl blocks — register mangled method names in fn_sigs and build
    // the trait dispatch table (method_name → [(dispatch_type, mangled_fn_name)]).
    // Mangling:
    //   - `impl Trait for Type { def method }` → `Trait__Type__method`
    //   - `impl Type { def method }` (trait_name == "") → `Type__method`
    let mut trait_dispatch_map: HashMap<String, Vec<(IrType, String)>> = HashMap::new();
    let mut struct_method_map: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut impl_fns: Vec<crate::parser::ast::AstFunction> = Vec::new();
    let mut blanket_templates: Vec<(String, String, crate::parser::ast::AstFunction)> = Vec::new();
    // Impl methods' declared parameters, keyed by the same mangled names:
    // the source checker judges a method call's count of arguments from
    // these, `self` included (§4.2). Types a param's type cannot resolve to
    // are lowering's to reject; those methods are not seeded.
    let mut impl_method_param_types: HashMap<String, Vec<IrType>> = HashMap::new();
    let mut impl_method_param_names: HashMap<String, Vec<String>> = HashMap::new();
    let mut impl_method_param_defaults: HashMap<String, Vec<Option<crate::parser::ast::AstExpr>>> =
        HashMap::new();
    for impl_def in &ast.impls {
        // A generic *target* -- `impl[T] Show for list<T>` -- is not a blanket
        // impl. A blanket impl enumerates the concrete types satisfying a bound
        // and emits one copy per type; here the parameter belongs to the target
        // container, and the method body is usually indifferent to it
        // (`list_len(self)` works for any element type). Registering it once
        // against `list<_>` and letting dispatch match on the constructor is
        // both correct and far cheaper than an instantiation per element type.
        //
        // Without this the blanket path caught it, searched for concrete types
        // satisfying a bound it does not have, found none, and emitted nothing.
        // See known-issues #38.
        let target_is_generic = impl_def.target_ty.is_some() && !impl_def.generic_params.is_empty();

        // Handle blanket impls: monomorphize for each known concrete type.
        if !impl_def.generic_params.is_empty()
            && !impl_def.trait_name.is_empty()
            && !target_is_generic
        {
            // Find the type param name (e.g., "T" in `impl[T where T: Show]`).
            if let crate::parser::ast::AstGenericParam::Type(ref tp_name, ref bounds, _) =
                &impl_def.generic_params[0]
            {
                // Find all concrete types that satisfy the required trait bounds.
                // We use trait_impl_methods to find types implementing each bound.
                let mut candidate_types: Option<Vec<String>> = None;
                for bound_trait in bounds {
                    let types_for_bound: Vec<String> = module
                        .trait_impl_methods()
                        .get(bound_trait)
                        .map(|entries| {
                            entries
                                .iter()
                                .map(|(cty, _, _)| cty.clone())
                                .collect::<std::collections::HashSet<_>>()
                                .into_iter()
                                .collect()
                        })
                        .unwrap_or_default();
                    candidate_types = Some(match candidate_types {
                        Some(prev) => prev
                            .into_iter()
                            .filter(|t| types_for_bound.contains(t))
                            .collect(),
                        None => types_for_bound,
                    });
                }
                // If no bounds specified, skip (can't determine concrete types).
                let concrete_types = candidate_types.unwrap_or_default();
                // If bounds exist but no concrete types found yet, also add all types in the module.
                let concrete_types = if concrete_types.is_empty() && !bounds.is_empty() {
                    // Fallback: try all known struct/enum types.
                    module.struct_defs.keys().cloned().collect::<Vec<_>>()
                } else {
                    concrete_types
                };

                for method in &impl_def.methods {
                    let template_name = format!(
                        "{}__blanket_{}__{}",
                        impl_def.trait_name, tp_name, method.name.name
                    );
                    let mut template_subs: HashMap<String, IrType> = HashMap::new();
                    template_subs.insert(tp_name.clone(), IrType::TypeParam(tp_name.clone()));
                    template_subs.insert("Self".to_owned(), IrType::TypeParam(tp_name.clone()));
                    template_subs.insert("self".to_owned(), IrType::TypeParam(tp_name.clone()));

                    let mut ret_ty =
                        resolve_ast_type_with_subs(&method.return_ty, &template_subs, &module)?;
                    if method.is_async {
                        ret_ty = IrType::Task(Box::new(ret_ty));
                    }
                    fn_sigs.insert(template_name.clone(), ret_ty);

                    let mut template_ast = method.clone();
                    template_ast.name.name = template_name.clone();
                    template_ast.type_params.clear();
                    for param in &mut template_ast.params {
                        if param.name.name == "self" {
                            if let crate::parser::ast::AstType::Named(ref n, _) = param.ty {
                                if n == tp_name || n == "self" || n == "Self" {
                                    param.ty = crate::parser::ast::AstType::Named(
                                        tp_name.clone(),
                                        param.ty.span(),
                                    );
                                }
                            }
                        }
                    }

                    impl_method_param_names.insert(
                        template_name.clone(),
                        template_ast
                            .params
                            .iter()
                            .map(|p| p.name.name.clone())
                            .collect(),
                    );
                    if template_ast.params.iter().any(|p| p.default.is_some()) {
                        impl_method_param_defaults.insert(
                            template_name.clone(),
                            template_ast
                                .params
                                .iter()
                                .map(|p| p.default.clone())
                                .collect(),
                        );
                    }
                    let lowered_params: Option<Vec<IrType>> = template_ast
                        .params
                        .iter()
                        .map(|p| resolve_ast_type_with_subs(&p.ty, &template_subs, &module).ok())
                        .collect();
                    if let Some(types) = lowered_params {
                        impl_method_param_types.insert(template_name.clone(), types);
                    }
                    blanket_templates.push((template_name.clone(), tp_name.clone(), template_ast));

                    for concrete_ty_name in &concrete_types {
                        let concrete_ir = type_name_to_ir_type(concrete_ty_name, &module)
                            .ok_or_else(|| unknown_impl_type(concrete_ty_name, impl_def.span))?;
                        module.add_trait_impl_method(
                            impl_def.trait_name.clone(),
                            concrete_ty_name.clone(),
                            method.name.name.clone(),
                            template_name.clone(),
                        );
                        trait_dispatch_map
                            .entry(method.name.name.clone())
                            .or_default()
                            .push((concrete_ir, template_name.clone()));
                    }
                }
                continue; // Skip the normal (non-generic) path below.
            }
        }
        // Normal (non-blanket) impl processing.
        //
        // If the trait itself declares generic parameters (`trait Container[T]`,
        // `trait Mappable[F[_]]`), validate `trait_args` arity and instantiate a
        // concrete `Trait__Args` definition in `module.trait_defs`.
        if let Some(gtrait) = module
            .generic_trait_defs()
            .get(&impl_def.trait_name)
            .cloned()
        {
            let expected = gtrait.type_params.len() + gtrait.hkt_params.len();
            if !impl_def.trait_args.is_empty() && impl_def.trait_args.len() != expected {
                return Err(LowerError::TypeMismatch {
                    expected: format!(
                        "{} generic argument(s) for trait '{}'",
                        expected, impl_def.trait_name
                    ),
                    found: format!("{}", impl_def.trait_args.len()),
                    span: impl_def.span,
                });
            }
            if impl_def.trait_args.len() == expected && expected > 0 {
                let mut t_subs: HashMap<String, IrType> = HashMap::new();
                let mut h_subs: HashMap<String, String> = HashMap::new();
                let mut mangled_args: Vec<String> = Vec::new();
                for (i, arg_ast) in impl_def.trait_args.iter().enumerate() {
                    let arg_ir = lower_type_with_structs(arg_ast, &module)?;
                    mangled_args.push(mangle_ir_type(&arg_ir));
                    if i < gtrait.type_params.len() {
                        t_subs.insert(gtrait.type_params[i].clone(), arg_ir);
                    } else {
                        let h_idx = i - gtrait.type_params.len();
                        let ctor_name = match arg_ast {
                            AstType::Named(n, _) => n.clone(),
                            AstType::List(_, _) => "list".to_owned(),
                            AstType::Option(_, _) => "option".to_owned(),
                            AstType::Map(_, _, _) => "map".to_owned(),
                            _ => mangle_ir_type(&arg_ir),
                        };
                        h_subs.insert(gtrait.hkt_params[h_idx].clone(), ctor_name);
                    }
                }
                let concrete_trait = format!("{}__{}", impl_def.trait_name, mangled_args.join("_"));
                let concrete_sigs: Vec<TraitMethodSig> = gtrait
                    .methods
                    .iter()
                    .map(|sig| TraitMethodSig {
                        name: sig.name.clone(),
                        params: sig
                            .params
                            .iter()
                            .map(|p| {
                                crate::pass::mono::substitute_ir_type_with_hkt(
                                    p,
                                    &t_subs,
                                    &h_subs,
                                    &module.struct_defs,
                                )
                            })
                            .collect(),
                        ret: Box::new(crate::pass::mono::substitute_ir_type_with_hkt(
                            &sig.ret,
                            &t_subs,
                            &h_subs,
                            &module.struct_defs,
                        )),
                    })
                    .collect();
                module.add_trait_def(concrete_trait.clone(), concrete_sigs);
                for method in &impl_def.methods {
                    let mangled = format!(
                        "{}__{}__{}",
                        impl_def.trait_name, impl_def.type_name, method.name.name
                    );
                    module.add_trait_impl_method(
                        concrete_trait.clone(),
                        impl_def.type_name.clone(),
                        method.name.name.clone(),
                        mangled,
                    );
                }
            }
        }
        //
        // A target carrying type arguments dispatches on the whole type, so
        // `impl Show for list<i64>` registers `IrType::List(I64)` rather than
        // dispatching on the bare name `list`.
        let dispatch_ty = match &impl_def.target_ty {
            Some(t) => lower_type_with_structs(t, &module)?,
            None => type_name_to_ir_type(&impl_def.type_name, &module)
                .ok_or_else(|| unknown_impl_type(&impl_def.type_name, impl_def.span))?,
        };
        // The mangled name has to include the arguments, or two impls of one
        // trait at different element types would collide on a single symbol --
        // `Show__list__fmt` for both `list<i64>` and `list<str>`.
        let impl_type_key = match &impl_def.target_ty {
            Some(_) => format!("{}_{}", impl_def.type_name, mangle_ir_type(&dispatch_ty)),
            None => impl_def.type_name.clone(),
        };
        for method in &impl_def.methods {
            let mangled = if impl_def.trait_name.is_empty() {
                // Standalone struct method: `TypeName__method`
                format!("{}__{}", impl_type_key, method.name.name)
            } else {
                // Trait impl: `TraitName__TypeName__method`
                format!(
                    "{}__{}__{}",
                    impl_def.trait_name, impl_type_key, method.name.name
                )
            };
            let mut ret_ty = lower_type_with_structs(&method.return_ty, &module)?;
            if method.is_async {
                ret_ty = IrType::Task(Box::new(ret_ty));
            }
            fn_sigs.insert(mangled.clone(), ret_ty);
            if impl_def.trait_name.is_empty() {
                // Register in struct_method_map for obj.method() dispatch.
                struct_method_map
                    .entry(impl_def.type_name.clone())
                    .or_default()
                    .insert(method.name.name.clone(), mangled.clone());
            } else {
                trait_dispatch_map
                    .entry(method.name.name.clone())
                    .or_default()
                    .push((dispatch_ty.clone(), mangled.clone()));
            }
            // Build a renamed copy of the method for lowering.
            // Replace bare `self` param type with the concrete struct type.
            let mut renamed = method.clone();
            renamed.name.name = mangled;
            for param in &mut renamed.params {
                if param.name.name == "self" {
                    // A generic target gives `self` the full type, so
                    // `list_len(self)` inside the impl sees a list rather than
                    // an unresolved name.
                    if let Some(ref t) = impl_def.target_ty {
                        param.ty = t.clone();
                        continue;
                    }
                    if let crate::parser::ast::AstType::Named(ref n, _) = param.ty {
                        if n == "self" {
                            param.ty = crate::parser::ast::AstType::Named(
                                impl_def.type_name.clone(),
                                param.ty.span(),
                            );
                        }
                    }
                }
            }
            // The source checker judges a method call the same way as a
            // function call (§4.2): the declared parameter names (with
            // `self` first), the defaults a call may leave out, and the
            // types where they resolve. Types that do not resolve here are
            // lowering's to reject; such a method is not seeded.
            let mangled = renamed.name.name.clone();
            impl_method_param_names.insert(
                mangled.clone(),
                renamed.params.iter().map(|p| p.name.name.clone()).collect(),
            );
            if renamed.params.iter().any(|p| p.default.is_some()) {
                impl_method_param_defaults.insert(
                    mangled.clone(),
                    renamed.params.iter().map(|p| p.default.clone()).collect(),
                );
            }
            let lowered_params: Option<Vec<IrType>> = renamed
                .params
                .iter()
                .map(|p| lower_type_with_structs(&p.ty, &module).ok())
                .collect();
            if let Some(types) = lowered_params {
                impl_method_param_types.insert(mangled, types);
            }
            impl_fns.push(renamed);
        }
    }
    let trait_dispatch = std::rc::Rc::new(trait_dispatch_map);
    let impl_method_param_types = std::rc::Rc::new(impl_method_param_types);
    let impl_method_param_names = std::rc::Rc::new(impl_method_param_names);
    let impl_method_param_defaults = std::rc::Rc::new(impl_method_param_defaults);
    // struct_method_map is only used for mangling; the mangled names are already in fn_sigs.
    let _ = struct_method_map;

    // 3d. Collect extern function declarations so call sites can emit CallExtern.
    for ext in &ast.extern_fns {
        if module.extern_fns.iter().any(|e| e.name == ext.name.name) {
            continue;
        }
        let param_types: Vec<IrType> = ext
            .params
            .iter()
            .map(|p| lower_type_with_structs(&p.ty, &module))
            .collect::<Result<_, _>>()?;
        let ret_ty = lower_type_with_structs(&ext.ret_ty, &module)?;
        // Register in fn_sigs so lower_call resolves the correct return type.
        fn_sigs.insert(ext.name.name.clone(), ret_ty.clone());
        module.extern_fns.push(crate::ir::module::IrExternFn {
            name: ext.name.name.clone(),
            param_types,
            ret_ty,
            abi: ext.abi.clone(),
            link_lib: ext.link_lib.clone(),
        });
    }

    // 3e. Collect effect declarations and register their operations into fn_sigs and extern_fns.
    for eff in &ast.effects {
        for op in &eff.operations {
            let param_types: Vec<IrType> = op
                .params
                .iter()
                .map(|p| lower_type_with_structs(&p.ty, &module))
                .collect::<Result<_, _>>()?;
            let ret_ty = lower_type_with_structs(&op.ret_ty, &module)?;
            fn_sigs.insert(op.name.name.clone(), ret_ty.clone());
            if !module.extern_fns.iter().any(|e| e.name == op.name.name) {
                module.extern_fns.push(crate::ir::module::IrExternFn {
                    name: op.name.name.clone(),
                    param_types,
                    ret_ty,
                    abi: Some("C".to_string()),
                    link_lib: None,
                });
            }
        }
    }

    // 3f. Build the HIR: resolve every value name and register every
    // expression (src/hir). The source checker then runs on the HIR -- stage
    // 5 on stage 4, as rustc does -- and its report is absorbed into it.
    // Lowering reads types and solved `-> Infer` returns from the HIR.
    let (hir, thir) = {
        let registries = typeck::Registries {
            module: &module,
            fn_sigs: &fn_sigs,
            fn_param_types: &fn_param_types,
            fn_param_names: &fn_param_names,
            fn_defaults: &fn_defaults,
            generic_fns: &generic_fns,
            const_defs: &const_defs,
            trait_dispatch: &trait_dispatch,
            inferred_fns: &inferred_fns,
            method_param_types: &impl_method_param_types,
            method_param_names: &impl_method_param_names,
            method_param_defaults: &impl_method_param_defaults,
        };
        let mut hir = crate::hir::build::build(ast, &registries);
        if typeck::enabled() {
            let report = typeck::check(&hir, &registries);
            hir.absorb(&report);
            typeck::publish(report);
        }
        crate::hir::install(&hir);
        if crate::hir::mode().as_deref() == Some("report") {
            hir.report(module_name);
        }
        // 3g. THIR: the typed, desugared tree over the HIR. It is built on
        // every compile, audited by `IRIS_THIR=report|check`, and installed as
        // lowering's type/fact source. The AST body walk remains for control
        // flow and the forms THIR keeps source-shaped. See src/thir/mod.rs.
        let thir = std::rc::Rc::new(crate::thir::build::build(&hir, &registries));
        crate::thir::install(std::rc::Rc::clone(&thir));
        crate::lower::mono::install();
        if crate::thir::mode().as_deref() == Some("report") {
            thir.report(module_name);
        }
        (hir, thir)
    };
    // A function declared `-> Infer` returns what its body solves to.
    for func in &inferred_returns {
        let mut ret =
            typeck::solved_return(&func.name.name).ok_or_else(|| undetermined_return(func))?;
        if func.is_async {
            ret = IrType::Task(Box::new(ret));
        }
        fn_sigs.insert(func.name.name.clone(), ret);
    }

    // Shared monomorphization state across all top-level function lowerings.
    let mono_cache = std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashSet::new()));
    let mono_sigs = std::rc::Rc::new(std::cell::RefCell::new(HashMap::new()));
    // Shared lambda counter across all top-level function lowerings ensures unique names.
    let lambda_counter = std::rc::Rc::new(std::cell::Cell::new(0u32));

    // 3h. Lower blanket-impl methods and HKT generic functions once into
    // `module.generic_templates` so `MonoPass` instantiates them lazily.
    for (template_name, tp_name, template_ast) in blanket_templates {
        let mut template_subs = HashMap::new();
        template_subs.insert(tp_name.clone(), IrType::TypeParam(tp_name.clone()));
        template_subs.insert("Self".to_owned(), IrType::TypeParam(tp_name.clone()));
        if let Ok((ir_fn, lifted)) = lower_function_with_generics_and_subs(
            &template_ast,
            &module,
            &fn_sigs,
            &const_defs,
            generic_fns.clone(),
            mono_cache.clone(),
            mono_sigs.clone(),
            template_subs,
            trait_dispatch.clone(),
            fn_defaults.clone(),
            fn_param_names.clone(),
            fn_param_types.clone(),
            lambda_counter.clone(),
        ) {
            module.add_generic_template(template_name, vec![tp_name], ir_fn);
            for lf in lifted {
                let _ = module.add_function(lf);
            }
        }
    }
    for func in &ast.functions {
        let has_hkt = func
            .type_params
            .iter()
            .any(|p| matches!(p, crate::parser::ast::AstGenericParam::Hkt(..)));
        let has_const = func
            .type_params
            .iter()
            .any(|p| matches!(p, crate::parser::ast::AstGenericParam::Const { .. }));
        if !has_hkt || has_const || func.is_const {
            continue;
        }
        let mut type_params = Vec::new();
        let mut hkt_params = Vec::new();
        let mut template_subs = HashMap::new();
        for gp in &func.type_params {
            match gp {
                crate::parser::ast::AstGenericParam::Type(n, _, _) => {
                    type_params.push(n.clone());
                    template_subs.insert(n.clone(), IrType::TypeParam(n.clone()));
                }
                crate::parser::ast::AstGenericParam::Hkt(n, _, _, _) => {
                    hkt_params.push(n.clone());
                    template_subs.insert(n.clone(), IrType::TypeParam(n.clone()));
                }
                crate::parser::ast::AstGenericParam::Const { name, .. } => {
                    type_params.push(name.clone());
                    template_subs.insert(name.clone(), IrType::TypeParam(name.clone()));
                }
            }
        }
        let mut template_ast = func.clone();
        template_ast.type_params.clear();
        if let Ok((ir_fn, lifted)) = lower_function_with_generics_and_subs(
            &template_ast,
            &module,
            &fn_sigs,
            &const_defs,
            generic_fns.clone(),
            mono_cache.clone(),
            mono_sigs.clone(),
            template_subs,
            trait_dispatch.clone(),
            fn_defaults.clone(),
            fn_param_names.clone(),
            fn_param_types.clone(),
            lambda_counter.clone(),
        ) {
            module.add_generic_template_with_hkt(
                func.name.name.clone(),
                type_params,
                hkt_params,
                ir_fn,
            );
            for lf in lifted {
                let _ = module.add_function(lf);
            }
        }
    }

    // 4. Lower all non-generic function definitions (including impl methods).
    //
    // Each function is lowered against the signatures collected above, so an
    // error in one does not affect the others: every function's first error
    // is reported, not only the first function's.
    let mut all_lifted: Vec<crate::ir::function::IrFunction> = Vec::new();
    // Each function's lowering error, with the function's extent.
    let mut errors: Vec<(crate::parser::lexer::Span, LowerError)> = Vec::new();
    for func in ast
        .functions
        .iter()
        .chain(impl_fns.iter())
        .chain(state_init.iter())
    {
        if !func.type_params.is_empty() {
            continue; // generic: lowered on demand at call sites
        }
        let lowered = lower_function_with_generics(
            func,
            &module,
            &fn_sigs,
            &const_defs,
            generic_fns.clone(),
            mono_cache.clone(),
            mono_sigs.clone(),
            trait_dispatch.clone(),
            fn_defaults.clone(),
            fn_param_names.clone(),
            fn_param_types.clone(),
            lambda_counter.clone(),
        );
        let (ir_func, lifted) = match lowered {
            Ok(done) => done,
            Err(e) => {
                errors.push((func.span, e));
                continue;
            }
        };
        module
            .add_function(ir_func)
            .map_err(|_| LowerError::DuplicateFunction {
                name: func.name.name.clone(),
                span: func.name.span,
            })?;
        all_lifted.extend(lifted);
    }
    // The source type checker's errors are the program's type errors,
    // reported whether or not lowering accepted it; in a function where it
    // found one, lowering's own error is a consequence of the same mistake
    // and is not reported as well. Lowering's errors elsewhere stand.
    let mut errors = {
        let checked = typeck::take_errors();
        let at = |e: &LowerError| {
            crate::diagnostics::error_byte_offset(&crate::error::Error::Lower(e.clone_shallow()))
        };
        let checked_at: Vec<u32> = checked.iter().filter_map(at).collect();
        let mut all = checked;
        for (extent, e) in errors {
            if !checked_at
                .iter()
                .any(|b| extent.start.0 <= *b && *b < extent.end.0)
            {
                all.push(e);
            }
        }
        all.sort_by_key(|e| at(e).unwrap_or(u32::MAX));
        all
    };
    match errors.len() {
        0 => {}
        1 => return Err(errors.pop().expect("one error")),
        _ => return Err(LowerError::Multiple(errors)),
    }
    // `IRIS_HIR=check`: a valid program must have a complete HIR. Run after
    // the program's own errors have had their say, so this never masks one.
    if crate::hir::mode().as_deref() == Some("check") {
        let problems = hir.validate();
        if !problems.is_empty() {
            return Err(crate::hir::incomplete(problems));
        }
    }
    // `IRIS_THIR=check`: a valid program must have a THIR node for every
    // expression the HIR walked. Like the HIR audit, this runs only after the
    // program's own errors have been reported.
    if crate::thir::mode().as_deref() == Some("check") {
        let problems = thir.validate(&hir);
        if !problems.is_empty() {
            return Err(crate::thir::incomplete(problems));
        }
    }
    // THIR is no longer needed once lowering is done: free its thread-local
    // context and drop the owning reference so the stage does not stay alive
    // for the rest of the compile.
    crate::thir::uninstall();
    crate::lower::mono::uninstall();
    if crate::lower::mono::mode().as_deref() == Some("report") {
        crate::lower::mono::report();
    }
    if crate::lower::mono::mode().as_deref() == Some("check") {
        crate::lower::mono::check()?;
    }
    // Add all lambda-lifted functions.
    for lf in all_lifted {
        let _ = module.add_function(lf);
    }

    // Post-lowering: register any concrete monomorphized structs created or
    // referenced inside functions (e.g. Box__i64 or Array__i64_3) that are not
    // yet in `module.struct_defs`.
    fn collect_concrete_structs(
        ty: &IrType,
        module: &IrModule,
        out: &mut HashMap<String, Vec<(String, IrType)>>,
    ) {
        match ty {
            IrType::Struct { name, fields } => {
                if !fields.is_empty()
                    && module.struct_def(name).is_none()
                    && !out.contains_key(name)
                {
                    out.insert(name.clone(), fields.clone());
                }
                for (_, fty) in fields {
                    collect_concrete_structs(fty, module, out);
                }
            }
            IrType::Array { elem, .. }
            | IrType::Slice { elem, .. }
            | IrType::Option(elem)
            | IrType::Chan(elem)
            | IrType::Task(elem)
            | IrType::Atomic(elem)
            | IrType::Mutex(elem)
            | IrType::Grad(elem)
            | IrType::Sparse(elem)
            | IrType::List(elem)
            | IrType::WeakRef(elem) => collect_concrete_structs(elem, module, out),
            IrType::ResultType(a, b) | IrType::Map(a, b) => {
                collect_concrete_structs(a, module, out);
                collect_concrete_structs(b, module, out);
            }
            IrType::Tuple(elems) => {
                for e in elems {
                    collect_concrete_structs(e, module, out);
                }
            }
            IrType::Fn { params, ret } => {
                for p in params {
                    collect_concrete_structs(p, module, out);
                }
                collect_concrete_structs(ret, module, out);
            }
            _ => {}
        }
    }
    let mut extra_struct_defs: HashMap<String, Vec<(String, IrType)>> = HashMap::new();
    for func in module.functions() {
        for p in &func.params {
            collect_concrete_structs(&p.ty, &module, &mut extra_struct_defs);
        }
        collect_concrete_structs(&func.return_ty, &module, &mut extra_struct_defs);
        for ty in func.value_types.values() {
            collect_concrete_structs(ty, &module, &mut extra_struct_defs);
        }
        for block in func.blocks() {
            for instr in &block.instrs {
                if let IrInstr::MakeStruct { result_ty, .. } = instr {
                    collect_concrete_structs(result_ty, &module, &mut extra_struct_defs);
                }
            }
        }
    }
    for (name, fields) in extra_struct_defs {
        let _ = module.add_struct_def(name, fields);
    }

    // Lowering consumes the checker's types: nothing leaves it untyped.
    typeck::check_typed(&module)?;
    Ok(module)
}

type LoopFrame = (BlockId, BlockId, BlockId, Vec<String>, Option<String>, bool);

type ResolvedPlace = (
    ValueId,
    IrType,
    Option<(crate::ir::function::PlaceSlot, Vec<usize>)>,
);

struct Lowerer<'m> {
    builder: IrFunctionBuilder,
    /// Current lexical scope: name → (ValueId, IrType).
    scope: HashMap<String, (ValueId, IrType)>,
    /// Current lexical scope's IR place slots for NLL borrow checking.
    scope_slots: HashMap<String, crate::ir::function::PlaceSlot>,
    /// Functions currently being inlined for a taped call, innermost last.
    /// Guards against a recursive taped call expanding forever (#49).
    taped_inline_stack: Vec<String>,
    /// Stack of (header_block, merge_block, loop_var_names, label) for nested loops.
    loop_stack: Vec<LoopFrame>,
    /// Reference to the module for struct/enum type lookups.
    module: &'m IrModule,
    /// Pre-collected function return types for resolving call result types.
    fn_sigs: &'m HashMap<String, IrType>,
    /// Counter for unique lambda function names.
    lambda_counter: std::rc::Rc<std::cell::Cell<u32>>,
    /// Lambda functions to be added to the module after this function is lowered.
    lifted_fns: std::rc::Rc<std::cell::RefCell<Vec<crate::ir::function::IrFunction>>>,
    /// Tracks the concrete element type of channels (channel ValueId → elem IrType).
    /// Populated when `send(ch, val)` is first called; used by `recv(ch)` to avoid Infer.
    chan_elem_types: HashMap<ValueId, IrType>,
    /// Active type-parameter substitutions for monomorphized generic functions.
    /// Maps type param name (e.g. "T") → concrete IrType.
    type_param_subs: HashMap<String, IrType>,
    /// Generic function AST templates: function name → AstFunction.
    generic_fns: std::rc::Rc<HashMap<String, crate::parser::ast::AstFunction>>,
    /// Tracks already-monomorphized specializations (mangled names) to avoid duplication.
    mono_cache: std::rc::Rc<std::cell::RefCell<std::collections::HashSet<String>>>,
    /// Return types of monomorphized specializations (mangled name → IrType).
    mono_sigs: std::rc::Rc<std::cell::RefCell<HashMap<String, IrType>>>,
    /// Global constants available for inlining.
    const_defs: std::rc::Rc<HashMap<String, crate::parser::ast::AstExpr>>,
    /// Trait method dispatch table: method_name → [(dispatch_type, mangled_fn_name)].
    /// The dispatch_type is the IrType of the first argument used to select the impl.
    trait_dispatch: std::rc::Rc<HashMap<String, Vec<(IrType, String)>>>,
    /// Default parameter expressions: fn_name → [Option<AstExpr>] per param.
    fn_defaults: std::rc::Rc<HashMap<String, Vec<Option<crate::parser::ast::AstExpr>>>>,
    /// Declared parameter names per function, for resolving named arguments.
    fn_param_names: std::rc::Rc<HashMap<String, Vec<String>>>,
    fn_param_types: std::rc::Rc<HashMap<String, Vec<IrType>>>,
    /// Expected type from a `val x: T = expr` annotation — used by collection
    /// constructors (e.g. `list()`, `map()`) to infer the element/key/value type.
    binding_ty: Option<IrType>,
    /// Expected return type from the enclosing function (set during
    /// `Return` lowering so we can coerce returned struct values into
    /// `dyn Trait` trait objects).
    current_return_ty: Option<IrType>,
    /// The running task while lowering an `async def` body (its poll
    /// function); `await` then suspends instead of blocking.
    async_task: Option<ValueId>,
    /// Expected type for the current expression being lowered. Set by `val`
    /// and `return` handlers before calling `lower_expr` so that if/else
    /// branches can coerce their results to `dyn Trait` before the merge.
    expected_expr_ty: Option<IrType>,
    /// Primal SSA values that participate in source-level reverse-mode AD.
    taped_values: HashSet<ValueId>,
    /// Mapping from primal SSA values to their tape-node SSA values.
    tape_nodes: HashMap<ValueId, ValueId>,
    /// When lowering inside a handler arm, the name of the resume continuation
    /// binding (e.g. `v` in `k(p) -> resume(v) => v(x)`). When `lower_call`
    /// sees a call to this name, it emits `ResumeCont` instead of a regular
    /// call instruction.
    resume_param_name: Option<String>,
    /// Monomorphized struct defs computed on-the-fly during generic function
    /// lowering.  Checked before `module.struct_def()` in field access.
    local_struct_defs: HashMap<String, Vec<(String, IrType)>>,
    /// Default values for monomorphized structs computed on-the-fly.
    /// Checked alongside `module.struct_defaults()` during struct literal lowering.
    local_struct_defaults: HashMap<String, Vec<Option<crate::parser::ast::AstExpr>>>,
    /// Deferred expressions to emit before each return or at function exit.
    /// Entries are in declaration order; emitted in reverse (LIFO).
    defer_stack: Vec<crate::parser::ast::AstExpr>,
    /// Deferred THIR expressions to emit before each return or at function exit.
    thir_defer_stack: Vec<crate::thir::ThirExpr>,
    /// Active local variables whose types implement Drop: (var_name, value_id, drop_fn_name).
    drop_stack: Vec<(String, ValueId, String)>,
    /// Active explicit IR drop scope stack (`ScopeId`).
    drop_scope_stack: Vec<crate::ir::function::ScopeId>,
    /// Try-catch state: the catch block to branch to on `?` unwrap failure.
    try_catch_catch_bb: Option<BlockId>,
    /// Try-catch state: the continuation block after the try/catch.
    try_catch_cont_bb: Option<BlockId>,
    /// Try-catch state: the catch parameter name (e.g. `e` in `catch e { ... }`).
    try_catch_param: Option<String>,
    /// Try-catch state: the error type the body's `?`s carry to the catch.
    try_catch_err_ty: Option<IrType>,
}

impl<'m> Lowerer<'m> {
    fn resolve_unqualified_name(&self, name: &str) -> String {
        if self.scope.contains_key(name) {
            return name.to_string();
        }
        let resolved = CURRENT_BRING_PREFIXES.with(|prefixes_stack| {
            if let Some(prefixes) = prefixes_stack.borrow().last() {
                for prefix in prefixes.iter() {
                    let candidate = format!("{}__{}", prefix, name);
                    // A non-`pub` item of a brought module is not reachable by
                    // its unqualified name from another module. The defining
                    // module's own references were rewritten to the mangled name
                    // during the merge, so they do not come through here and are
                    // unaffected. See known-issues #13.
                    if CURRENT_PRIVATE_ITEMS.with(|p| p.borrow().contains(&candidate)) {
                        continue;
                    }
                    if self.fn_sigs.contains_key(&candidate)
                        || self.mono_sigs.borrow().contains_key(&candidate)
                        || self.generic_fns.contains_key(&candidate)
                        || self.const_defs.contains_key(&candidate)
                        || self.module.struct_def(&candidate).is_some()
                        || self.module.enum_def(&candidate).is_some()
                        || self.module.type_alias(&candidate).is_some()
                    {
                        return Some(candidate);
                    }
                }
            }
            None
        });
        if let Some(res) = resolved {
            return res;
        }
        if self.fn_sigs.contains_key(name)
            || self.mono_sigs.borrow().contains_key(name)
            || self.generic_fns.contains_key(name)
            || self.const_defs.contains_key(name)
            || self.module.struct_def(name).is_some()
            || self.module.enum_def(name).is_some()
            || self.module.type_alias(name).is_some()
        {
            return name.to_string();
        }
        // Fallback: scan all fn_sigs / const_defs / module registries for any
        // mangled candidate matching `*__name`. This handles transitive brings
        // where a dependency module internally brings another module (e.g.,
        // nn.iris does `bring std.ml` and calls `xavier_init` which becomes
        // `ml__xavier_init` after mangling).
        let suffix = format!("__{}", name);

        // Gather every candidate before choosing, rather than returning the
        // first key a `HashMap` happens to yield.
        //
        // Returning the first match made this resolution depend on the
        // per-process hash seed. With a nested generic, both `Box__i64` and
        // `Box__Box__i64` end with the same suffix, so the winner changed
        // between runs: `wrap(wrap(5))` compiled and ran correctly in about
        // half of attempts and failed type validation in the rest -- 6 of 12
        // when measured. See known-issues #21, and #17 for the same defect
        // class in LICM and capture ordering.
        //
        // Ties are broken by taking the *longest* candidate, then
        // lexicographically. Longest wins because a longer mangled name is the
        // more specific instantiation: resolving `Box__Box__i64` to `Box__i64`
        // loses a level of nesting, which is precisely the mismatch that was
        // being reported.
        let mut candidates: Vec<String> = Vec::new();
        for key in self.fn_sigs.keys() {
            if key.ends_with(&suffix) {
                candidates.push(key.clone());
            }
        }
        for key in self.mono_sigs.borrow().keys() {
            if key.ends_with(&suffix) {
                candidates.push(key.clone());
            }
        }
        for key in self.generic_fns.keys() {
            if key.ends_with(&suffix) {
                candidates.push(key.clone());
            }
        }
        for key in self.const_defs.keys() {
            if key.ends_with(&suffix) {
                candidates.push(key.clone());
            }
        }
        for key in self.module.struct_defs.keys() {
            if key.ends_with(&suffix) {
                candidates.push(key.clone());
            }
        }
        for key in self.module.enum_defs.keys() {
            if key.ends_with(&suffix) {
                candidates.push(key.clone());
            }
        }
        for key in self.module.type_aliases.keys() {
            if key.ends_with(&suffix) {
                candidates.push(key.clone());
            }
        }
        // This fallback exists for *transitive* brings, where the prefix of the
        // defining module is not among the current module's own bring prefixes.
        // It matched on the suffix alone, so it also resolved private items --
        // and it ran after the prefix loop, which meant refusing them there
        // achieved nothing. Filtering here is what actually enforces `pub`.
        // See known-issues #13.
        CURRENT_PRIVATE_ITEMS.with(|p| {
            let priv_set = p.borrow();
            candidates.retain(|c| !priv_set.contains(c));
        });
        if !candidates.is_empty() {
            candidates.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
            return candidates.remove(0);
        }
        name.to_string()
    }

    #[allow(dead_code)]
    fn new_with_lambda_state(
        builder: IrFunctionBuilder,
        module: &'m IrModule,
        fn_sigs: &'m HashMap<String, IrType>,
        lambda_counter: std::rc::Rc<std::cell::Cell<u32>>,
        lifted_fns: std::rc::Rc<std::cell::RefCell<Vec<crate::ir::function::IrFunction>>>,
    ) -> Self {
        Self::new_generic(
            builder,
            module,
            fn_sigs,
            lambda_counter,
            lifted_fns,
            HashMap::new(),
            std::rc::Rc::new(HashMap::new()),
            std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashSet::new())),
            std::rc::Rc::new(std::cell::RefCell::new(HashMap::new())),
            std::rc::Rc::new(HashMap::new()),
            std::rc::Rc::new(HashMap::new()),
            std::rc::Rc::new(HashMap::new()),
            std::rc::Rc::new(HashMap::new()),
            std::rc::Rc::new(HashMap::new()),
        )
    }

    fn new_sub_lowerer(&self, builder: IrFunctionBuilder) -> Self {
        let mut sub = Self::new_generic(
            builder,
            self.module,
            self.fn_sigs,
            self.lambda_counter.clone(),
            self.lifted_fns.clone(),
            self.type_param_subs.clone(),
            self.generic_fns.clone(),
            self.mono_cache.clone(),
            self.mono_sigs.clone(),
            self.const_defs.clone(),
            self.trait_dispatch.clone(),
            self.fn_defaults.clone(),
            self.fn_param_names.clone(),
            self.fn_param_types.clone(),
        );
        sub.local_struct_defs = self.local_struct_defs.clone();
        sub.local_struct_defaults = self.local_struct_defaults.clone();
        sub
    }

    #[allow(clippy::too_many_arguments)]
    fn new_generic(
        builder: IrFunctionBuilder,
        module: &'m IrModule,
        fn_sigs: &'m HashMap<String, IrType>,
        lambda_counter: std::rc::Rc<std::cell::Cell<u32>>,
        lifted_fns: std::rc::Rc<std::cell::RefCell<Vec<crate::ir::function::IrFunction>>>,
        type_param_subs: HashMap<String, IrType>,
        generic_fns: std::rc::Rc<HashMap<String, crate::parser::ast::AstFunction>>,
        mono_cache: std::rc::Rc<std::cell::RefCell<std::collections::HashSet<String>>>,
        mono_sigs: std::rc::Rc<std::cell::RefCell<HashMap<String, IrType>>>,
        const_defs: std::rc::Rc<HashMap<String, crate::parser::ast::AstExpr>>,
        trait_dispatch: std::rc::Rc<HashMap<String, Vec<(IrType, String)>>>,
        fn_defaults: std::rc::Rc<HashMap<String, Vec<Option<crate::parser::ast::AstExpr>>>>,
        fn_param_names: std::rc::Rc<HashMap<String, Vec<String>>>,
        fn_param_types: std::rc::Rc<HashMap<String, Vec<IrType>>>,
    ) -> Self {
        Self {
            builder,
            scope: HashMap::new(),
            scope_slots: HashMap::new(),
            taped_inline_stack: Vec::new(),
            loop_stack: Vec::new(),
            module,
            fn_sigs,
            lambda_counter,
            lifted_fns,
            chan_elem_types: HashMap::new(),
            type_param_subs,
            generic_fns,
            mono_cache,
            mono_sigs,
            const_defs,
            trait_dispatch,
            fn_defaults,
            fn_param_names,
            fn_param_types,
            binding_ty: None,
            current_return_ty: None,
            async_task: None,
            expected_expr_ty: None,
            taped_values: HashSet::new(),
            tape_nodes: HashMap::new(),
            resume_param_name: None,
            local_struct_defs: HashMap::new(),
            local_struct_defaults: HashMap::new(),
            defer_stack: Vec::new(),
            thir_defer_stack: Vec::new(),
            drop_stack: Vec::new(),
            drop_scope_stack: Vec::new(),
            try_catch_catch_bb: None,
            try_catch_cont_bb: None,
            try_catch_param: None,
            try_catch_err_ty: None,
        }
    }

    /// Allocates a place slot for a newly bound variable and records it in `scope_slots`.
    fn bind_place_slot(&mut self, name: &str, val: ValueId) -> crate::ir::function::PlaceSlot {
        let slot = self.builder.alloc_place_slot(name, Some(val));
        self.scope_slots.insert(name.to_string(), slot);
        slot
    }

    /// Inserts a variable into `scope` and binds a fresh IR place slot for it.
    fn bind_scope_var(&mut self, name: impl Into<String>, val: ValueId, ty: IrType) {
        let name = name.into();
        self.bind_place_slot(&name, val);
        self.scope.insert(name, (val, ty));
    }

    /// Records a read of `name` at `span` for IR-level NLL borrow checking.
    fn record_read_place(&mut self, name: &str, span: Span) {
        let resolved = self.resolve_unqualified_name(name);
        if let Some(&slot) = self.scope_slots.get(&resolved) {
            if let Some(&(val, _)) = self.scope.get(&resolved) {
                self.builder
                    .note_value_full_span(val, span.start.0, span.end.0);
            }
            self.builder.record_place_event(
                crate::ir::function::PlaceEventKind::Read(slot),
                span.start.0,
                span.end.0,
            );
        }
    }

    /// Records a write/reassignment to `name` (optionally a field) at `span`.
    fn record_assign_place(
        &mut self,
        name: &str,
        field_index: Option<usize>,
        val: ValueId,
        span: Span,
    ) {
        let resolved = self.resolve_unqualified_name(name);
        let slot = self
            .scope_slots
            .get(&resolved)
            .copied()
            .unwrap_or_else(|| self.bind_place_slot(&resolved, val));
        self.builder.map_value_slot_if_unset(val, slot);
        self.builder.record_place_event(
            crate::ir::function::PlaceEventKind::Write { slot, field_index },
            span.start.0,
            span.end.0,
        );
    }

    /// Emits `IrInstr::Borrow` and records `PlaceEventKind::Borrow` when borrowing a tracked place.
    fn emit_borrow_marker(
        &mut self,
        place_val: ValueId,
        ty: IrType,
        is_mut: bool,
        slot_proj: Option<(crate::ir::function::PlaceSlot, Vec<usize>)>,
        span: Span,
    ) -> (ValueId, IrType) {
        if let Some((slot, projections)) = slot_proj {
            self.builder.record_place_event(
                crate::ir::function::PlaceEventKind::Borrow { slot, projections },
                span.start.0,
                span.end.0,
            );
        }
        let result = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::Borrow {
                result,
                place: place_val,
                is_mut,
            },
            Some(ty.clone()),
        );
        (result, ty)
    }

    /// Emits `IrInstr::Move` and records `PlaceEventKind::Move` when moving a tracked place.
    fn emit_move_marker(
        &mut self,
        place_val: ValueId,
        ty: IrType,
        slot: Option<crate::ir::function::PlaceSlot>,
        span: Span,
    ) -> (ValueId, IrType) {
        if let Some(slot) = slot {
            self.builder.record_place_event(
                crate::ir::function::PlaceEventKind::Move(slot),
                span.start.0,
                span.end.0,
            );
        }
        let result = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::Move {
                result,
                place: place_val,
            },
            Some(ty.clone()),
        );
        (result, ty)
    }

    /// Resolves a THIR place expression without recording a `Read` on the root local.
    fn resolve_thir_place(
        &mut self,
        expr: &crate::thir::ThirExpr,
    ) -> Option<Result<ResolvedPlace, LowerError>> {
        match &expr.kind {
            crate::thir::ThirExprKind::Value { name, .. } => {
                let resolved = self.resolve_unqualified_name(name);
                if let Some(&(val, ref ty)) = self.scope.get(&resolved) {
                    let slot = self.scope_slots.get(&resolved).copied();
                    return Some(Ok((val, ty.clone(), slot.map(|s| (s, Vec::new())))));
                }
                self.try_lower_thir_expr(expr)
                    .map(|r| r.map(|(v, t)| (v, t, None)))
            }
            crate::thir::ThirExprKind::Field { base, field, .. } => {
                match self.resolve_thir_place(base) {
                    Some(Ok((base_val, base_ty, Some((slot, mut projs))))) => {
                        if let IrType::Struct { fields, .. } = &base_ty {
                            if let Some((idx, (_, f_ty))) =
                                fields.iter().enumerate().find(|(_, (n, _))| n == field)
                            {
                                let f_ty = f_ty.clone();
                                let res = self.builder.fresh_value();
                                self.builder.push_instr(
                                    IrInstr::GetField {
                                        result: res,
                                        base: base_val,
                                        field_index: idx,
                                        result_ty: f_ty.clone(),
                                    },
                                    Some(f_ty.clone()),
                                );
                                projs.push(idx);
                                return Some(Ok((res, f_ty, Some((slot, projs)))));
                            }
                        }
                        self.try_lower_thir_expr(expr)
                            .map(|r| r.map(|(v, t)| (v, t, None)))
                    }
                    Some(Ok((v, t, None))) => {
                        let _ = (v, t);
                        self.try_lower_thir_expr(expr)
                            .map(|r| r.map(|(v, t)| (v, t, None)))
                    }
                    Some(Err(e)) => Some(Err(e)),
                    None => None,
                }
            }
            crate::thir::ThirExprKind::TupleIndex { base, index } => {
                match self.resolve_thir_place(base) {
                    Some(Ok((base_val, base_ty, Some((slot, mut projs))))) => {
                        if let IrType::Tuple(elems) = &base_ty {
                            if let Some(elem_ty) = elems.get(*index) {
                                let elem_ty = elem_ty.clone();
                                let res = self.builder.fresh_value();
                                self.builder.push_instr(
                                    IrInstr::GetElement {
                                        result: res,
                                        base: base_val,
                                        index: *index,
                                        result_ty: elem_ty.clone(),
                                    },
                                    Some(elem_ty.clone()),
                                );
                                projs.push(*index);
                                return Some(Ok((res, elem_ty, Some((slot, projs)))));
                            }
                        }
                        self.try_lower_thir_expr(expr)
                            .map(|r| r.map(|(v, t)| (v, t, None)))
                    }
                    Some(Ok(_)) => self
                        .try_lower_thir_expr(expr)
                        .map(|r| r.map(|(v, t)| (v, t, None))),
                    Some(Err(e)) => Some(Err(e)),
                    None => None,
                }
            }
            _ => self
                .try_lower_thir_expr(expr)
                .map(|r| r.map(|(v, t)| (v, t, None))),
        }
    }

    /// Resolves an AST place expression without recording a `Read` on the root local.
    fn resolve_ast_place(&mut self, expr: &AstExpr) -> Result<ResolvedPlace, LowerError> {
        match expr {
            AstExpr::Ident(ident) => {
                let resolved = self.resolve_unqualified_name(&ident.name);
                if let Some(&(val, ref ty)) = self.scope.get(&resolved) {
                    let slot = self.scope_slots.get(&resolved).copied();
                    return Ok((val, ty.clone(), slot.map(|s| (s, Vec::new()))));
                }
                self.lower_expr(expr).map(|(v, t)| (v, t, None))
            }
            AstExpr::FieldAccess { base, field, .. } => {
                if let Ok((base_val, base_ty, Some((slot, mut projs)))) =
                    self.resolve_ast_place(base)
                {
                    if let IrType::Struct { fields, .. } = &base_ty {
                        if let Some((idx, (_, f_ty))) =
                            fields.iter().enumerate().find(|(_, (n, _))| n == field)
                        {
                            let f_ty = f_ty.clone();
                            let res = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::GetField {
                                    result: res,
                                    base: base_val,
                                    field_index: idx,
                                    result_ty: f_ty.clone(),
                                },
                                Some(f_ty.clone()),
                            );
                            projs.push(idx);
                            return Ok((res, f_ty, Some((slot, projs))));
                        }
                    }
                }
                self.lower_expr(expr).map(|(v, t)| (v, t, None))
            }
            AstExpr::TupleIndex { base, index, .. } => {
                if let Ok((base_val, base_ty, Some((slot, mut projs)))) =
                    self.resolve_ast_place(base)
                {
                    if let IrType::Tuple(elems) = &base_ty {
                        if let Some(elem_ty) = elems.get(*index) {
                            let elem_ty = elem_ty.clone();
                            let res = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::GetElement {
                                    result: res,
                                    base: base_val,
                                    index: *index,
                                    result_ty: elem_ty.clone(),
                                },
                                Some(elem_ty.clone()),
                            );
                            projs.push(*index);
                            return Ok((res, elem_ty, Some((slot, projs))));
                        }
                    }
                }
                self.lower_expr(expr).map(|(v, t)| (v, t, None))
            }
            _ => self.lower_expr(expr).map(|(v, t)| (v, t, None)),
        }
    }

    /// Finds the mangled destructor function name for a type if it implements Drop.
    fn find_destructor(&self, ty: &IrType) -> Option<String> {
        if let IrType::Struct { name, .. } = ty {
            if let Some(drop_fn) = self.module.find_drop_method(name) {
                return Some(drop_fn);
            }
        }
        if let Some(cands) = self.trait_dispatch.get("drop") {
            for (dispatch_ty, mangled) in cands.iter() {
                if dispatch_ty == ty {
                    return Some(mangled.clone());
                }
                if let (IrType::Struct { name: n1, .. }, IrType::Struct { name: n2, .. }) =
                    (dispatch_ty, ty)
                {
                    if n1 == n2 {
                        return Some(mangled.clone());
                    }
                }
            }
        }
        None
    }

    /// Enters a new explicit lexical drop scope and emits `IrInstr::EnterScope`.
    fn enter_drop_scope(&mut self) -> crate::ir::function::ScopeId {
        let parent = self.drop_scope_stack.last().copied();
        let scope = self.builder.alloc_drop_scope(parent);
        self.drop_scope_stack.push(scope);
        if !self.builder.is_current_block_terminated() {
            self.builder.push_instr(IrInstr::EnterScope { scope }, None);
        }
        scope
    }

    /// Registers a Drop-bearing binding in the current explicit scope and `drop_stack`.
    fn register_drop(&mut self, var_name: String, val: ValueId, drop_fn: String, ty: IrType) {
        let scope = match self.drop_scope_stack.last().copied() {
            Some(s) => s,
            None => self.enter_drop_scope(),
        };
        self.builder.register_drop_binding(
            scope,
            crate::ir::function::DropBinding {
                value: val,
                var_name: var_name.clone(),
                drop_fn: drop_fn.clone(),
                ty,
            },
        );
        self.drop_stack.push((var_name, val, drop_fn));
    }

    /// Exits the current explicit lexical drop scope and emits `IrInstr::ExitScope`.
    fn exit_drop_scope(&mut self) {
        if let Some(scope) = self.drop_scope_stack.pop() {
            if !self.builder.is_current_block_terminated() {
                self.builder.push_instr(IrInstr::ExitScope { scope }, None);
            }
        }
    }

    /// Emits explicit `IrInstr::Drop` instructions for active variables implementing Drop in LIFO order.
    fn emit_drops(&mut self, ret_values: &[ValueId]) {
        let drops: Vec<_> = self.drop_stack.clone();
        for (_var_name, val, drop_fn) in drops.iter().rev() {
            if ret_values.contains(val) {
                continue; // Moved into return value, do not destruct
            }
            let ty = self
                .scope
                .values()
                .find(|(v, _)| v == val)
                .map(|(_, t)| t.clone())
                .unwrap_or_else(|| IrType::Tuple(Vec::new()));
            self.builder.push_instr(
                IrInstr::Drop {
                    value: *val,
                    drop_fn: drop_fn.clone(),
                    ty,
                },
                None,
            );
        }
    }

    fn is_reverse_diff_scalar(ty: &IrType) -> bool {
        matches!(
            ty,
            IrType::Scalar(
                DType::F32
                    | DType::F64
                    | DType::I32
                    | DType::I64
                    | DType::U8
                    | DType::I8
                    | DType::U32
                    | DType::U64
                    | DType::USize
            ) | IrType::Tensor { .. }
        )
    }

    /// A default value of `ty`, emitted in the current block.
    ///
    /// Used for the *missing* arm of a non-exhaustive `when`. Previously a
    /// partial match forced **both** arms to unit, so `when o { some(x) => x }`
    /// on `some(42)` returned 0 -- the covered arm's value was discarded to keep
    /// the merge block well-typed. Giving only the uncovered arm a default of
    /// the same type keeps the merge consistent *and* the covered arm correct.
    /// See known-issues #54.
    fn emit_default_value(&mut self, ty: &IrType) -> ValueId {
        let v = self.builder.fresh_value();
        match ty {
            IrType::Scalar(DType::F64) | IrType::Scalar(DType::F32) => {
                self.builder.push_instr(
                    IrInstr::ConstFloat {
                        result: v,
                        value: 0.0,
                        ty: ty.clone(),
                    },
                    Some(ty.clone()),
                );
            }
            IrType::Scalar(DType::Bool) => {
                self.builder.push_instr(
                    IrInstr::ConstBool {
                        result: v,
                        value: false,
                    },
                    Some(ty.clone()),
                );
            }
            IrType::Str => {
                self.builder.push_instr(
                    IrInstr::ConstStr {
                        result: v,
                        value: String::new(),
                    },
                    Some(IrType::Str),
                );
            }
            _ => {
                // Integers and anything heap-shaped: a zero word. For a boxed
                // type this is a null pointer, which is what the uncovered arm
                // of a match that should never be taken can honestly produce.
                self.builder.push_instr(
                    IrInstr::ConstInt {
                        result: v,
                        value: 0,
                        ty: ty.clone(),
                    },
                    Some(ty.clone()),
                );
            }
        }
        v
    }

    /// A real unit value (`0`) for a construct that yields nothing.
    ///
    /// The fallback used to be a bare `fresh_value()` -- a `ValueId` that no
    /// instruction ever defines. Anything that then consumed it failed
    /// validation with "variable '%N' is used before it has been assigned a
    /// value", pointing at an internal SSA name rather than at the source. A
    /// `when` arm with a braced body (`{ a = 1; }`) has no tail expression and
    /// so hit this on every backend path -- enum, option and result alike.
    /// See known-issues #3.
    ///
    /// When the current block is already terminated no instruction can be
    /// emitted, and the value is unreachable by construction, so the phantom is
    /// harmless and retained for that case only.
    fn emit_unit_value(&mut self) -> (ValueId, IrType) {
        let ty = IrType::Scalar(DType::I64);
        let v = self.builder.fresh_value();
        if !self.builder.is_current_block_terminated() {
            self.builder.push_instr(
                IrInstr::ConstInt {
                    result: v,
                    value: 0,
                    ty: ty.clone(),
                },
                Some(ty.clone()),
            );
        }
        (v, ty)
    }

    fn tape_ref_for(&self, value: ValueId) -> ValueId {
        self.tape_nodes.get(&value).copied().unwrap_or(value)
    }

    // `_ty` is the primal's type, no longer used now that the handle is typed
    // `IrType::TapeRef` rather than as its primal. Kept in the signature because
    // callers have it to hand and a future typed-tape (`tape<f32>`) would need it.
    fn ensure_taped_leaf(&mut self, value: ValueId, _ty: &IrType) {
        if self.tape_nodes.contains_key(&value) {
            self.taped_values.insert(value);
            return;
        }

        let tape_result = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::TapeRecord {
                result: tape_result,
                value,
                op: "leaf".to_owned(),
                parents: vec![],
            },
            // The handle is a pointer into the runtime tape, not the primal.
            // Typing it as the primal made it emit as `double`, so a taped value
            // crossing a block boundary stopped being a handle (#49).
            Some(IrType::TapeRef),
        );
        self.taped_values.insert(value);
        self.tape_nodes.insert(value, tape_result);
    }

    fn maybe_record_tape_result(
        &mut self,
        primal_result: ValueId,
        primal_ty: &IrType,
        op: &str,
        parents: &[ValueId],
    ) {
        if !Self::is_reverse_diff_scalar(primal_ty)
            || !parents
                .iter()
                .any(|parent| self.taped_values.contains(parent))
        {
            return;
        }

        let tape_result = self.builder.fresh_value();
        let tape_parents = parents
            .iter()
            .copied()
            .map(|parent| self.tape_ref_for(parent))
            .collect();
        self.builder.push_instr(
            IrInstr::TapeRecord {
                result: tape_result,
                value: primal_result,
                op: op.to_owned(),
                parents: tape_parents,
            },
            Some(IrType::TapeRef),
        );
        self.taped_values.insert(primal_result);
        self.tape_nodes.insert(primal_result, tape_result);
    }

    fn resolve_ty(&self, ty: &AstType) -> Result<IrType, LowerError> {
        resolve_ast_type_with_subs(ty, &self.type_param_subs, self.module)
    }

    /// The type of `expr` without lowering it: the checker's, else what its
    /// shape says (a literal, a bound name, a tuple of those); `None` when
    /// neither knows.
    fn infer_ast_expr_type_simple(&self, expr: &AstExpr) -> Option<IrType> {
        if let Some(t) = self.typeck_type(expr.span()) {
            return Some(t);
        }
        match expr {
            AstExpr::IntLit { .. } => Some(IrType::Scalar(DType::I64)),
            AstExpr::FloatLit { .. } => Some(IrType::Scalar(DType::F64)),
            AstExpr::BoolLit { .. } => Some(IrType::Scalar(DType::Bool)),
            AstExpr::StringLit { .. } => Some(IrType::Str),
            AstExpr::Ident(ident) => {
                let resolved_name = self.resolve_unqualified_name(&ident.name);
                self.scope.get(&resolved_name).map(|(_, ty)| ty.clone())
            }
            AstExpr::Tuple { elements, .. } => Some(IrType::Tuple(
                elements
                    .iter()
                    .map(|e| self.infer_ast_expr_type_simple(e))
                    .collect::<Option<_>>()?,
            )),
            AstExpr::MapLiteral { entries, .. } => {
                let (k, v) = entries.first()?;
                Some(IrType::Map(
                    Box::new(self.infer_ast_expr_type_simple(k)?),
                    Box::new(self.infer_ast_expr_type_simple(v)?),
                ))
            }
            _ => None,
        }
    }

    fn infer_thir_expr_type_simple(&self, expr: &crate::thir::ThirExpr) -> Option<IrType> {
        if let Some(t) = self.typeck_type(expr.span) {
            return Some(t);
        }
        match &expr.kind {
            crate::thir::ThirExprKind::Literal(crate::thir::ThirLiteral::Int(_)) => {
                Some(IrType::Scalar(DType::I64))
            }
            crate::thir::ThirExprKind::Literal(crate::thir::ThirLiteral::Float(_)) => {
                Some(IrType::Scalar(DType::F64))
            }
            crate::thir::ThirExprKind::Literal(crate::thir::ThirLiteral::Bool(_)) => {
                Some(IrType::Scalar(DType::Bool))
            }
            crate::thir::ThirExprKind::Literal(crate::thir::ThirLiteral::Str(_)) => {
                Some(IrType::Str)
            }
            crate::thir::ThirExprKind::Value { name, .. } => {
                let resolved_name = self.resolve_unqualified_name(name);
                self.scope.get(&resolved_name).map(|(_, ty)| ty.clone())
            }
            crate::thir::ThirExprKind::Tuple(elements) => Some(IrType::Tuple(
                elements
                    .iter()
                    .map(|e| self.infer_thir_expr_type_simple(e))
                    .collect::<Option<_>>()?,
            )),
            crate::thir::ThirExprKind::Map { entries, .. } => {
                let (k, v) = entries.first()?;
                Some(IrType::Map(
                    Box::new(self.infer_thir_expr_type_simple(k)?),
                    Box::new(self.infer_thir_expr_type_simple(v)?),
                ))
            }
            _ => None,
        }
    }

    /// A monomorphised `base_name` whose fields have the given types (a
    /// field whose type is `None` matches any).
    /// Looks up a struct definition in the current function's local specializations first,
    /// falling back to module-wide struct definitions.
    fn lookup_struct_def(&self, name: &str) -> Option<&Vec<(String, IrType)>> {
        self.local_struct_defs
            .get(name)
            .or_else(|| self.module.struct_def(name))
    }

    fn is_generic_struct(&self, name: &str, resolved_base: &str) -> bool {
        self.module.generic_struct_names.contains(name)
            || self.module.generic_struct_names.contains(resolved_base)
            || self.module.generic_struct_asts.contains_key(resolved_base)
            || self.module.generic_struct_asts.contains_key(name)
    }

    fn copy_struct_defaults_to_local(
        &mut self,
        mangled_name: &str,
        base_name: &str,
        raw_name: &str,
    ) {
        if self.local_struct_defaults.contains_key(mangled_name) {
            return;
        }
        if let Some(template_defaults) = self
            .module
            .struct_defaults
            .get(base_name)
            .or_else(|| self.module.struct_defaults.get(raw_name))
            .cloned()
        {
            self.local_struct_defaults
                .insert(mangled_name.to_string(), template_defaults);
        }
    }

    fn resolve_struct_lit_header<F>(
        &mut self,
        name: &str,
        infer_fields: F,
        span: Span,
    ) -> Result<(String, Vec<(String, IrType)>), LowerError>
    where
        F: FnOnce(&mut Self) -> Vec<(String, Option<IrType>)>,
    {
        let base_name = resolve_brought_name(name, self.module);
        let mut resolved_name = base_name.clone();
        if let Some(IrType::Struct {
            name: expected_name,
            fields: expected_fields,
        }) = self.binding_ty.clone()
        {
            let name_prefix = format!("{name}__");
            let base_prefix = format!("{base_name}__");
            if expected_name.starts_with(&name_prefix) || expected_name.starts_with(&base_prefix) {
                if self.lookup_struct_def(&expected_name).is_none() && !expected_fields.is_empty() {
                    self.local_struct_defs
                        .insert(expected_name.clone(), expected_fields);
                    self.copy_struct_defaults_to_local(&expected_name, &base_name, name);
                }
                resolved_name = expected_name;
            }
        }
        if self.lookup_struct_def(&resolved_name).is_none() {
            let mut found = false;
            if let Some(IrType::Struct {
                name: expected_name,
                fields: expected_fields,
            }) = self.binding_ty.clone()
            {
                if expected_name == name
                    || expected_name == base_name
                    || expected_name.starts_with(&format!("{name}__"))
                    || expected_name.starts_with(&format!("{base_name}__"))
                {
                    if self.lookup_struct_def(&expected_name).is_none()
                        && !expected_fields.is_empty()
                    {
                        self.local_struct_defs
                            .insert(expected_name.clone(), expected_fields);
                        self.copy_struct_defaults_to_local(&expected_name, &base_name, name);
                    }
                    resolved_name = expected_name;
                    found = true;
                }
            }
            if !found {
                let lowered_fields = infer_fields(self);
                if let Some(matched_name) = self
                    .find_matching_monomorphized_struct(name, &lowered_fields)
                    .or_else(|| {
                        self.find_matching_monomorphized_struct(&base_name, &lowered_fields)
                    })
                {
                    resolved_name = matched_name;
                }
            }
        }

        let mut struct_fields = self
            .lookup_struct_def(&resolved_name)
            .ok_or_else(|| LowerError::UndefinedVariable {
                name: name.to_string(),
                span,
                suggestion: None,
            })?
            .clone();

        if !self.type_param_subs.is_empty() {
            for (_fname, fty) in &mut struct_fields {
                *fty = resolve_concrete_field(fty, &self.type_param_subs);
            }
        }

        Ok((resolved_name, struct_fields))
    }

    fn finish_generic_struct_lit(
        &mut self,
        name: &str,
        resolved_name: &mut String,
        struct_fields: &mut Vec<(String, IrType)>,
        field_actual_tys: &[IrType],
        has_spread: bool,
    ) {
        let base_name = resolve_brought_name(name, self.module);
        if (*resolved_name != name && *resolved_name != base_name)
            || (has_spread && self.type_param_subs.is_empty())
            || !self.is_generic_struct(name, &base_name)
        {
            return;
        }

        if let Some(tmpl) = self
            .module
            .generic_struct_asts
            .get(&base_name)
            .or_else(|| self.module.generic_struct_asts.get(name))
            .cloned()
        {
            let (type_params, hkt_params, const_params) =
                split_generic_param_names(&tmpl.type_params);
            let mut inferred_subs: HashMap<String, IrType> = HashMap::new();
            for ((field_name, _), actual_ty) in struct_fields.iter().zip(field_actual_tys.iter()) {
                if let Some(fdef) = tmpl.fields.iter().find(|f| f.name.name == *field_name) {
                    extract_from_ast_type(
                        &fdef.ty,
                        actual_ty,
                        &type_params,
                        &hkt_params,
                        &const_params,
                        self.module,
                        &mut inferred_subs,
                    );
                }
            }
            for p in type_params
                .iter()
                .chain(hkt_params.iter())
                .chain(const_params.iter())
            {
                if !inferred_subs.contains_key(p) {
                    if let Some(sub) = self.type_param_subs.get(p) {
                        inferred_subs.insert(p.clone(), sub.clone());
                    }
                }
            }
            if !inferred_subs.is_empty() {
                if let Ok(resolved_fields) = tmpl
                    .fields
                    .iter()
                    .map(|f| {
                        Ok((
                            f.name.name.clone(),
                            resolve_ast_type_with_subs(&f.ty, &inferred_subs, self.module)?,
                        ))
                    })
                    .collect::<Result<Vec<_>, LowerError>>()
                {
                    *struct_fields = resolved_fields;
                } else {
                    for (_, fty) in struct_fields.iter_mut() {
                        *fty = resolve_concrete_field(fty, &inferred_subs);
                    }
                }
                let mangle = mangle_param_list(&tmpl.type_params, &inferred_subs);
                if !mangle.is_empty() {
                    let mangled_base = if *resolved_name == base_name {
                        base_name.as_str()
                    } else {
                        name
                    };
                    *resolved_name = format!("{mangled_base}__{mangle}");
                    self.local_struct_defs
                        .insert(resolved_name.clone(), struct_fields.clone());
                    self.copy_struct_defaults_to_local(resolved_name, &base_name, name);
                }
            }
            return;
        }

        let mut inferred_subs: HashMap<String, IrType> = HashMap::new();
        for ((_, template_ty), actual_ty) in struct_fields.iter().zip(field_actual_tys.iter()) {
            if let IrType::Struct {
                name: pname,
                fields: pfields,
            } = template_ty
            {
                if pfields.is_empty()
                    && self.module.struct_def(pname).is_none()
                    && self.module.enum_def(pname).is_none()
                {
                    inferred_subs
                        .entry(pname.clone())
                        .or_insert_with(|| actual_ty.clone());
                }
            }
        }
        for (k, v) in &self.type_param_subs {
            inferred_subs.entry(k.clone()).or_insert_with(|| v.clone());
        }
        if !inferred_subs.is_empty() {
            for (_, fty) in struct_fields.iter_mut() {
                *fty = resolve_concrete_field(fty, &inferred_subs);
            }
            let mut inf_keys: Vec<&String> = inferred_subs.keys().collect();
            inf_keys.sort();
            let mangle = inf_keys
                .iter()
                .filter_map(|k| inferred_subs.get(*k))
                .map(mangle_ir_type)
                .collect::<Vec<_>>()
                .join("_");
            if !mangle.is_empty() {
                *resolved_name = format!("{resolved_name}__{mangle}");
                self.local_struct_defs
                    .insert(resolved_name.clone(), struct_fields.clone());
                self.copy_struct_defaults_to_local(resolved_name, &base_name, name);
            }
        }
    }

    fn find_matching_monomorphized_struct(
        &self,
        base_name: &str,
        fields: &[(String, Option<IrType>)],
    ) -> Option<String> {
        let prefix = format!("{}__", base_name);
        for (struct_name, struct_fields) in self
            .local_struct_defs
            .iter()
            .chain(self.module.struct_defs.iter())
        {
            if struct_name.starts_with(&prefix) && struct_fields.len() == fields.len() {
                let mut matched = true;
                for (f_name, f_ty) in fields {
                    if let Some((_, def_ty)) = struct_fields.iter().find(|(n, _)| n == f_name) {
                        if f_ty.as_ref().is_some_and(|t| t != def_ty) {
                            matched = false;
                            break;
                        }
                    } else {
                        matched = false;
                        break;
                    }
                }
                if matched {
                    return Some(struct_name.clone());
                }
            }
        }
        None
    }

    /// Looks up a variable and returns its `ValueId` and type.
    fn lookup(&mut self, ident: &Ident) -> Result<(ValueId, IrType), LowerError> {
        let resolved_name = self.resolve_unqualified_name(&ident.name);
        let found = self
            .scope
            .get(&resolved_name)
            .cloned()
            .ok_or_else(|| self.unknown_name(&ident.name, ident.span, None))?;
        self.record_read_place(&ident.name, ident.span);
        Ok(found)
    }

    /// Unknown and missing fields in `Name { ... }`, reported with fixes:
    /// renaming a misspelt field, or filling in the missing ones.
    #[allow(clippy::too_many_arguments)]
    fn check_struct_literal_fields(
        &self,
        record: &str,
        declared: &[(String, IrType)],
        fields: &[(String, AstExpr)],
        name_spans: &[Span],
        has_spread: bool,
        defaults: Option<&Vec<Option<AstExpr>>>,
        span: Span,
    ) -> Result<(), LowerError> {
        use crate::diagnostics::{Diagnostic, Fix};
        let record = record.split("__").next().unwrap_or(record);
        let has_default = |i: usize| defaults.and_then(|d| d.get(i)).is_some_and(|d| d.is_some());
        let missing: Vec<(usize, &(String, IrType))> = if has_spread {
            Vec::new()
        } else {
            declared
                .iter()
                .enumerate()
                .filter(|(i, (n, _))| !fields.iter().any(|(f, _)| f == n) && !has_default(*i))
                .collect()
        };
        let field_list = declared
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>()
            .join(", ");

        if let Some((idx, (unknown, value))) = fields
            .iter()
            .enumerate()
            .find(|(_, (n, _))| !declared.iter().any(|(d, _)| d == n))
        {
            let at = name_spans.get(idx).copied().unwrap_or_else(|| value.span());
            let mut diag = Diagnostic::error(format!("`{record}` has no field `{unknown}`"))
                .with_primary_span(at.start.0, at.end.0, Some("unknown field".into()))
                .with_note(format!("`{record}` has fields: {field_list}"));
            // One field unknown and one missing: almost certainly a misspelling.
            let rename = if missing.len() == 1 {
                Some(missing[0].1 .0.clone())
            } else {
                hints::closest(unknown, missing.iter().map(|(_, (n, _))| n.as_str()))
            };
            if let (Some(to), Some(_)) = (rename, name_spans.get(idx)) {
                diag = diag.with_fix(Fix::replace(
                    format!("rename it to `{to}`"),
                    at.start.0,
                    at.end.0,
                    to,
                ));
            }
            return Err(LowerError::rich("E0113", diag));
        }

        if !missing.is_empty() {
            let names: Vec<String> = missing.iter().map(|(_, (n, _))| format!("`{n}`")).collect();
            let message = if names.len() == 1 {
                format!("missing field {} in `{record}`", names[0])
            } else {
                format!("missing fields {} in `{record}`", names.join(", "))
            };
            let mut diag =
                Diagnostic::error(message).with_primary_span(span.start.0, span.end.0, None);
            let fill: Option<Vec<String>> = missing
                .iter()
                .map(|(_, (n, ty))| hints::placeholder(ty).map(|v| format!("{n}: {v}")))
                .collect();
            if let Some(fill) = fill {
                let text = fill.join(", ");
                let fix = match fields.last() {
                    Some((_, last)) => Fix::insert(
                        format!("add {}", names.join(", ")),
                        last.span().end.0,
                        format!(", {text}"),
                    ),
                    None => Fix::insert(
                        format!("add {}", names.join(", ")),
                        span.end.0.saturating_sub(1),
                        format!(" {text} "),
                    ),
                };
                diag = diag.with_fix(fix);
            }
            return Err(LowerError::rich("E0114", diag));
        }
        Ok(())
    }

    /// Whether `concrete` has an implementation of `trait_name` that a trait
    /// object can dispatch to. Every impl -- direct and blanket -- is
    /// registered before function bodies are lowered. Two cases are accepted
    /// without a registered method: a trait with no methods (nothing to
    /// dispatch), and a monomorphised generic record (`Box__i64`), whose impl
    /// may come from a generic-target `impl[T] Trait for Box[T]`.
    fn implements_trait(&self, concrete: &str, trait_name: &str) -> bool {
        if self
            .module
            .trait_def(trait_name)
            .map_or(true, |methods| methods.is_empty())
        {
            return true;
        }
        if self
            .module
            .trait_impl_methods()
            .get(trait_name)
            .is_some_and(|impls| impls.iter().any(|(ty, _, _)| ty == concrete))
        {
            return true;
        }
        self.module
            .generic_struct_names
            .iter()
            .any(|generic| concrete.starts_with(&format!("{generic}__")))
    }

    /// If `value` is a concrete struct and `expected` is `dyn Trait`, insert a
    /// `MakeTraitObject` and return the new trait-object value. Otherwise
    /// pass through unchanged. Used at return sites where the function
    /// signature dictates the expected trait-object type. The span is used
    /// only for error reporting.
    fn coerce_to_trait_object(
        &mut self,
        value: ValueId,
        ty: IrType,
        expected: &IrType,
        span: crate::parser::lexer::Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        if let IrType::TraitObject {
            name,
            methods: exp_methods,
        } = expected
        {
            // Already a matching trait object: no-op.
            if let IrType::TraitObject {
                name: existing_name,
                ..
            } = &ty
            {
                if existing_name == name {
                    return Ok((value, ty));
                }
            }
            if let IrType::Struct { name: concrete, .. } = &ty {
                if !self.implements_trait(concrete, name) {
                    return Err(LowerError::Rejected {
                        detail: format!(
                            "'{concrete}' does not implement trait '{name}', so it cannot be used as dyn {name}; add `impl {name} for {concrete}`"
                        ),
                        span,
                    });
                }
                let obj_val = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::MakeTraitObject {
                        result: obj_val,
                        value,
                        target_trait: name.clone(),
                        concrete_ty: concrete.clone(),
                        result_ty: expected.clone(),
                    },
                    Some(expected.clone()),
                );
                return Ok((
                    obj_val,
                    IrType::TraitObject {
                        name: name.clone(),
                        methods: exp_methods.clone(),
                    },
                ));
            } else {
                return Err(LowerError::rich(
                    "E0129",
                    crate::diagnostics::Diagnostic::error(format!(
                        "`{0}` does not implement `{1}`, so it cannot be used as `dyn {1}`",
                        hints::type_name(&ty),
                        name
                    ))
                    .with_primary_span((span).start.0, (span).end.0, None),
                ));
            }
        }
        Ok((value, ty))
    }

    fn lower_binop_expr(
        &mut self,
        op: AstBinOp,
        lhs_val: ValueId,
        lhs_ty: IrType,
        rhs_val: ValueId,
        rhs_ty: IrType,
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        if lhs_ty == IrType::Str && rhs_ty == IrType::Str && matches!(op, AstBinOp::Add) {
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::StrConcat {
                    result,
                    lhs: lhs_val,
                    rhs: rhs_val,
                },
                Some(IrType::Str),
            );
            return Ok((result, IrType::Str));
        }

        if lhs_ty == IrType::Str && matches!(op, AstBinOp::Mul) {
            let count_val = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::Cast {
                    result: count_val,
                    operand: rhs_val,
                    from_ty: rhs_ty.clone(),
                    to_ty: IrType::Scalar(DType::I64),
                },
                Some(IrType::Scalar(DType::I64)),
            );
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::StrRepeat {
                    result,
                    operand: lhs_val,
                    count: count_val,
                },
                Some(IrType::Str),
            );
            return Ok((result, IrType::Str));
        }

        if rhs_ty == IrType::Str && matches!(op, AstBinOp::Mul) {
            let count_val = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::Cast {
                    result: count_val,
                    operand: lhs_val,
                    from_ty: lhs_ty.clone(),
                    to_ty: IrType::Scalar(DType::I64),
                },
                Some(IrType::Scalar(DType::I64)),
            );
            let result = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::StrRepeat {
                    result,
                    operand: rhs_val,
                    count: count_val,
                },
                Some(IrType::Str),
            );
            return Ok((result, IrType::Str));
        }

        // Auto-promote f32 <-> f64
        let (lhs_val2, rhs_val2, lhs_ty2) = match (&lhs_ty, &rhs_ty) {
            (IrType::Scalar(DType::F32), IrType::Scalar(DType::F64)) => {
                let cast = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::Cast {
                        result: cast,
                        operand: lhs_val,
                        from_ty: lhs_ty.clone(),
                        to_ty: IrType::Scalar(DType::F64),
                    },
                    Some(IrType::Scalar(DType::F64)),
                );
                (cast, rhs_val, IrType::Scalar(DType::F64))
            }
            (IrType::Scalar(DType::F64), IrType::Scalar(DType::F32)) => {
                let cast = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::Cast {
                        result: cast,
                        operand: rhs_val,
                        from_ty: rhs_ty.clone(),
                        to_ty: IrType::Scalar(DType::F64),
                    },
                    Some(IrType::Scalar(DType::F64)),
                );
                (lhs_val, cast, lhs_ty)
            }
            (IrType::Grad(_), IrType::Scalar(_)) => (lhs_val, rhs_val, lhs_ty.clone()),
            (IrType::Scalar(_), IrType::Grad(_)) => (lhs_val, rhs_val, rhs_ty.clone()),
            (IrType::Sparse(_), IrType::Scalar(_)) if matches!(op, AstBinOp::Mul) => {
                (lhs_val, rhs_val, lhs_ty.clone())
            }
            (IrType::Scalar(_), IrType::Sparse(_)) if matches!(op, AstBinOp::Mul) => {
                (lhs_val, rhs_val, rhs_ty.clone())
            }
            _ => {
                if lhs_ty != rhs_ty {
                    return Err(binop_mismatch(
                        op,
                        &AstExpr::Ident(Ident {
                            name: String::new(),
                            span,
                        }),
                        &lhs_ty,
                        &AstExpr::Ident(Ident {
                            name: String::new(),
                            span,
                        }),
                        &rhs_ty,
                        span,
                    ));
                }
                (lhs_val, rhs_val, lhs_ty)
            }
        };
        let lhs_val = lhs_val2;
        let rhs_val = rhs_val2;
        let lhs_ty = lhs_ty2;

        // Phase 86: operator overloading for struct types.
        if let IrType::Struct {
            name: struct_name, ..
        } = &lhs_ty
        {
            let trait_method = op_trait_method(op);
            if let Some((trait_name, method_name)) = trait_method {
                let mangled = format!("{}__{}__{}", trait_name, struct_name, method_name);
                if let Some(ret_ty) = self.fn_sigs.get(&mangled).cloned() {
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::Call {
                            result: Some(result),
                            callee: mangled,
                            args: vec![lhs_val, rhs_val],
                            result_ty: Some(ret_ty.clone()),
                        },
                        Some(ret_ty.clone()),
                    );
                    return Ok((result, ret_ty));
                }
            }
        }

        let ir_op = lower_binop(op);
        let result_ty = match op {
            AstBinOp::CmpEq
            | AstBinOp::CmpNe
            | AstBinOp::CmpLt
            | AstBinOp::CmpLe
            | AstBinOp::CmpGt
            | AstBinOp::CmpGe => IrType::Scalar(DType::Bool),
            _ => lhs_ty.clone(),
        };

        let result = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BinOp {
                result,
                op: ir_op,
                lhs: lhs_val,
                rhs: rhs_val,
                ty: result_ty.clone(),
            },
            Some(result_ty.clone()),
        );
        let tape_op = match ir_op {
            BinOp::Add => Some("add"),
            BinOp::Sub => Some("sub"),
            BinOp::Mul => Some("mul"),
            BinOp::Div => Some("div"),
            _ => None,
        };
        if let Some(tape_op) = tape_op {
            self.maybe_record_tape_result(result, &result_ty, tape_op, &[lhs_val, rhs_val]);
        }
        Ok((result, result_ty))
    }

    fn lower_expr(&mut self, expr: &AstExpr) -> Result<(ValueId, IrType), LowerError> {
        // Synthetic marker: an empty block with no statements and no tail is
        // how the THIR lowerer delegates a construct it owns (a call, a loop
        // body, an arms' body, ...). The marker carries its THIR payload in
        // the registry. Real source blocks `{}` carry a source span and are
        // looked up in THIR the usual way.
        if let AstExpr::Block(b) = expr {
            if b.stmts.is_empty() && b.tail.is_none() {
                match synth_marker_at(expr.span()) {
                    Some(SynthMarker::Expr(thir)) => {
                        let result = self.try_lower_thir_expr(&thir).ok_or_else(|| {
                            LowerError::rich(
                                "E0902",
                                crate::diagnostics::Diagnostic::error(
                                    "internal compiler error: unmigratable THIR expr".to_string(),
                                )
                                .with_code("E0902"),
                            )
                        })?;
                        let lowered = result?;
                        self.builder.note_value_span(lowered.0, expr.span().start.0);
                        return Ok(lowered);
                    }
                    Some(SynthMarker::Block(tb)) => {
                        let lowered = self.lower_thir_block(&tb)?;
                        self.builder.note_value_span(lowered.0, expr.span().start.0);
                        return Ok(lowered);
                    }
                    None => {
                        // A real source block, or a real empty statement block:
                        // route through THIR the normal way.
                        let span = expr.span();
                        if !(span.start.0 == SYNTH_MARK || span.end.0 == SYNTH_MARK)
                            && crate::thir::is_source_span(span)
                        {
                            if let Some(thir) = crate::thir::expr_at(span) {
                                if let Some(result) = self.try_lower_thir_expr(&thir) {
                                    let lowered = result?;
                                    self.builder.note_value_span(lowered.0, span.start.0);
                                    return Ok(lowered);
                                }
                            } else if let Some(tb) = crate::thir::block_at(span) {
                                let lowered = self.lower_thir_block(&tb)?;
                                self.builder.note_value_span(lowered.0, span.start.0);
                                return Ok(lowered);
                            }
                        }
                    }
                }
            }
        }
        // THIR body-walk slice: when THIR has a node for this source span,
        // try to lower it structurally. Migrated forms return Some; unmigrated
        // forms return None and fall through to the AST path below. A migrated
        // form that errors does not fall back -- the error is real.
        if !IN_THIR_LOWER.with(|c| c.get()) {
            let span = expr.span();
            if !(span.start.0 == 0 && span.end.0 == 0) && crate::thir::is_source_span(span) {
                if let Some(thir) = crate::thir::expr_at(span) {
                    if thir_matches_ast(&thir, expr) {
                        if let Some(result) = self.try_lower_thir_expr(&thir) {
                            let lowered = result?;
                            self.builder.note_value_span(lowered.0, span.start.0);
                            return Ok(lowered);
                        }
                    }
                }
            }
        }
        let lowered = self.lower_expr_inner(expr)?;
        // Every value remembers the innermost expression that produced it, so
        // a type error found later, on the IR, can point at that expression.
        // Statement spans alone left a function's tail value, and every value
        // after a statement's first instruction, without a position.
        self.builder.note_value_span(lowered.0, expr.span().start.0);
        Ok(lowered)
    }

    fn lower_expr_inner(&mut self, expr: &AstExpr) -> Result<(ValueId, IrType), LowerError> {
        match expr {
            AstExpr::Ident(ident) => {
                // Special built-in identifiers
                if ident.name == "none" {
                    let elem = match &self.binding_ty {
                        Some(IrType::Option(e)) => Some((**e).clone()),
                        _ => self.typeck_hint(ident.span).and_then(|h| h.part(0)),
                    };
                    let elem = elem.ok_or_else(|| {
                        self.undetermined(ident.span, "what this `none` would hold")
                    })?;
                    let result_ty = IrType::Option(Box::new(elem));
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::MakeNone {
                            result,
                            result_ty: result_ty.clone(),
                        },
                        Some(result_ty.clone()),
                    );
                    return Ok((result, result_ty));
                }
                let resolved_name = self.resolve_unqualified_name(&ident.name);
                // Program state, unless a local shadows it.
                if !self.scope.contains_key(&resolved_name) {
                    if let Some((_, ty)) = self.module.state_slot(&ident.name) {
                        let ty = ty.clone();
                        return Ok(self.state_get(&ident.name, ty));
                    }
                }
                // If the ident is not in scope, check if it's a named function —
                // create a first-class function reference via MakeClosure.
                if !self.scope.contains_key(&resolved_name) {
                    // Source functions used as values need the same closure
                    // ABI as lambdas when they cross a parameter/record boundary.
                    // A raw function pointer cannot accept the hidden env argument.
                    if let Some(function) =
                        CURRENT_FN_ASTS.with(|asts| asts.borrow().get(&resolved_name).cloned())
                    {
                        let params: Vec<crate::parser::ast::AstParam> = function
                            .params
                            .iter()
                            .enumerate()
                            .map(|(index, parameter)| crate::parser::ast::AstParam {
                                name: Ident {
                                    name: format!("__fnref_arg_{index}"),
                                    span: ident.span,
                                },
                                ty: parameter.ty.clone(),
                                default: None,
                            })
                            .collect();
                        let body = AstExpr::Call {
                            callee: Ident {
                                name: resolved_name.clone(),
                                span: ident.span,
                            },
                            args: params
                                .iter()
                                .map(|parameter| AstExpr::Ident(parameter.name.clone()))
                                .collect(),
                            named_args: vec![],
                            span: ident.span,
                        };
                        return self.lower_lambda(&params, &body, ident.span);
                    }
                    if let Some(ret_ty) = self.fn_sigs.get(&resolved_name).cloned() {
                        let fn_ty = IrType::Fn {
                            params: vec![], // param types not tracked in fn_sigs
                            ret: Box::new(ret_ty.clone()),
                        };
                        let result = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::MakeClosure {
                                result,
                                fn_name: resolved_name,
                                captures: vec![],
                                result_ty: fn_ty.clone(),
                            },
                            Some(fn_ty.clone()),
                        );
                        return Ok((result, fn_ty));
                    }
                }
                self.lookup(ident)
            }

            AstExpr::FloatLit { value, span } => {
                let result = self.builder.fresh_value();
                // `f32` when the context declares it, as for integer widths.
                let ty = match self.typeck_type(*span) {
                    Some(IrType::Scalar(DType::F32)) => IrType::Scalar(DType::F32),
                    _ => IrType::Scalar(DType::F64),
                };
                self.builder.push_instr(
                    IrInstr::ConstFloat {
                        result,
                        value: *value,
                        ty: ty.clone(),
                    },
                    Some(ty.clone()),
                );
                Ok((result, ty))
            }

            AstExpr::IntLit { value, span } => {
                let result = self.builder.fresh_value();
                // A literal takes the integer width its context declares
                // (`val b: u8 = 'A'`, an `i32` argument): the checker solved it.
                // It used to be `i64` whatever the annotation said.
                let ty = match self.typeck_type(*span) {
                    Some(IrType::Scalar(d)) if d != DType::I64 && d.is_integer() => {
                        IrType::Scalar(d)
                    }
                    _ => IrType::Scalar(DType::I64),
                };
                self.builder.push_instr(
                    IrInstr::ConstInt {
                        result,
                        value: *value,
                        ty: ty.clone(),
                    },
                    Some(ty.clone()),
                );
                Ok((result, ty))
            }

            AstExpr::BoolLit { value, .. } => {
                let result = self.builder.fresh_value();
                let ty = IrType::Scalar(DType::Bool);
                self.builder.push_instr(
                    IrInstr::ConstBool {
                        result,
                        value: *value,
                    },
                    Some(ty.clone()),
                );
                Ok((result, ty))
            }

            // String literals are emitted as ConstStr instructions.
            AstExpr::StringLit { value, .. } => {
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::ConstStr {
                        result,
                        value: value.clone(),
                    },
                    Some(IrType::Str),
                );
                Ok((result, IrType::Str))
            }

            AstExpr::BinOp { op, lhs, rhs, span } => {
                // Short-circuit logical operators get their own control flow.
                if matches!(op, AstBinOp::And | AstBinOp::Or) {
                    return self.lower_short_circuit(*op, lhs, rhs, *span);
                }

                let (lhs_val, lhs_ty) = self.lower_expr(lhs)?;
                let (rhs_val, rhs_ty) = self.lower_expr(rhs)?;
                self.lower_binop_expr(*op, lhs_val, lhs_ty, rhs_val, rhs_ty, *span)
            }

            AstExpr::UnaryOp { op, expr, .. } => {
                let (val, ty) = self.lower_expr(expr)?;
                let result = self.builder.fresh_value();
                let ir_op = match op {
                    AstUnaryOp::Neg => ScalarUnaryOp::Neg,
                    AstUnaryOp::Not => ScalarUnaryOp::Not,
                };
                self.builder.push_instr(
                    IrInstr::UnaryOp {
                        result,
                        op: ir_op,
                        operand: val,
                        ty: ty.clone(),
                    },
                    Some(ty.clone()),
                );
                if matches!(ir_op, ScalarUnaryOp::Neg) {
                    self.maybe_record_tape_result(result, &ty, "neg", &[val]);
                }
                Ok((result, ty))
            }

            AstExpr::Call {
                callee,
                args,
                named_args,
                span,
            } => self.lower_call(callee, args, named_args, *span),

            AstExpr::If {
                cond,
                then_block,
                else_block,
                span,
            } => self.lower_if_expr(cond, then_block, else_block.as_ref(), *span),

            AstExpr::Block(block) => {
                if let Some(v) = self.lower_block(block)? {
                    Ok(v)
                } else if let Some((param_id, param_ty)) = self.builder.current_block_first_param()
                {
                    // Block has no tail but the current block has a block param
                    // (e.g. from `?` unwrapping). Use it as the block's value.
                    Ok((param_id, param_ty))
                } else {
                    // Block was terminated early (e.g. break/return/continue).
                    // The value is only used to satisfy type requirements; the
                    // current block is already sealed, so we cannot emit more
                    // instructions.
                    Ok(self.emit_unit_value())
                }
            }

            AstExpr::Mask { body, .. } => {
                // Effect mask: lower the body as if it were a normal block.
                if let Some(v) = self.lower_block(body)? {
                    Ok(v)
                } else if let Some((param_id, param_ty)) = self.builder.current_block_first_param()
                {
                    Ok((param_id, param_ty))
                } else {
                    Ok(self.emit_unit_value())
                }
            }

            AstExpr::Index {
                base,
                indices,
                span,
            } => {
                let (base_val, base_ty) = self.lower_expr(base)?;
                // Array or Slice index: arr[i] or slice[i]
                if let IrType::Array { elem, .. } | IrType::Slice { elem, .. } = &base_ty {
                    let elem_ty = (**elem).clone();
                    if indices.len() != 1 {
                        return Err(LowerError::rich(
                            "E0125",
                            crate::diagnostics::Diagnostic::error(
                                "an array or slice takes exactly 1 index".to_string(),
                            )
                            .with_primary_span(
                                span.start.0,
                                span.end.0,
                                None,
                            ),
                        ));
                    }
                    let (idx_val, _) = self.lower_expr(&indices[0])?;
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::ArrayLoad {
                            result,
                            array: base_val,
                            index: idx_val,
                            elem_ty: elem_ty.clone(),
                        },
                        Some(elem_ty.clone()),
                    );
                    return Ok((result, elem_ty));
                }
                // List index: xs[i], checked like list_get (E0403).
                if let IrType::List(elem) = &base_ty {
                    let elem_ty = (**elem).clone();
                    if indices.len() != 1 {
                        return Err(LowerError::rich(
                            "E0125",
                            crate::diagnostics::Diagnostic::error(
                                "a list takes exactly 1 index".to_string(),
                            )
                            .with_primary_span(
                                span.start.0,
                                span.end.0,
                                None,
                            ),
                        ));
                    }
                    let (index, _) = self.lower_expr(&indices[0])?;
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::ListGet {
                            result,
                            list: base_val,
                            index,
                            elem_ty: elem_ty.clone(),
                        },
                        Some(elem_ty.clone()),
                    );
                    return Ok((result, elem_ty));
                }
                // Tensor index: tensor[i, j, ...].
                let elem_ty = match &base_ty {
                    IrType::Tensor { dtype, .. } => IrType::Scalar(*dtype),
                    other => return Err(not_indexable(other, *span)),
                };
                let mut idx_vals = Vec::new();
                for idx in indices {
                    let (iv, _) = self.lower_expr(idx)?;
                    idx_vals.push(iv);
                }
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::Load {
                        result,
                        tensor: base_val,
                        indices: idx_vals,
                        result_ty: elem_ty.clone(),
                    },
                    Some(elem_ty.clone()),
                );
                Ok((result, elem_ty))
            }

            AstExpr::StructLit {
                name,
                fields,
                field_spans,
                spread,
                span,
            } => {
                let (mut resolved_name, mut struct_fields) = self.resolve_struct_lit_header(
                    name,
                    |this| {
                        fields
                            .iter()
                            .map(|(f_name, f_expr)| {
                                (f_name.clone(), this.infer_ast_expr_type_simple(f_expr))
                            })
                            .collect()
                    },
                    *span,
                )?;

                // If spread is present, evaluate it to get the source struct.
                let spread_val = if let Some(spread_expr) = spread {
                    let (sv, st) = self.lower_expr(spread_expr)?;
                    Some((sv, st))
                } else {
                    None
                };

                // Lower each field expression in declaration order.
                let mut field_vals = Vec::with_capacity(struct_fields.len());
                let mut field_actual_tys = Vec::with_capacity(struct_fields.len());
                // Look up field defaults for this struct (by resolved name).
                let field_defaults = self
                    .local_struct_defaults
                    .get(&resolved_name)
                    .or_else(|| self.module.struct_defaults.get(&resolved_name))
                    .cloned();
                self.check_struct_literal_fields(
                    name,
                    &struct_fields,
                    fields,
                    field_spans,
                    spread.is_some(),
                    field_defaults.as_ref(),
                    *span,
                )?;
                for (field_idx, (field_name, field_ty)) in struct_fields.iter().enumerate() {
                    if let Some(provided) = fields.iter().find(|(n, _)| n == field_name) {
                        // Propagate the struct field type down as binding_ty so `list()` can infer its type.
                        let prev_binding_ty = self.binding_ty.take();
                        self.binding_ty = Some(field_ty.clone());
                        let (val, actual_ty) = self.lower_expr(&provided.1)?;
                        self.binding_ty = prev_binding_ty;
                        // A concrete record stored in a `dyn Trait` field
                        // becomes a trait object, as at a `dyn` parameter.
                        let (val, actual_ty) = self.coerce_to_trait_object(
                            val,
                            actual_ty,
                            field_ty,
                            provided.1.span(),
                        )?;
                        field_vals.push(val);
                        field_actual_tys.push(actual_ty);
                    } else if let Some((spread_val_id, spread_ty)) = &spread_val {
                        // Copy field from spread source struct via GetField.
                        let spread_struct_fields = match spread_ty {
                            IrType::Struct { fields, .. } => fields.clone(),
                            _ => {
                                return Err(LowerError::rich(
                                    "E0124",
                                    crate::diagnostics::Diagnostic::error(
                                        "the value after `..` must be a record of the same type"
                                            .to_string(),
                                    )
                                    .with_primary_span(
                                        span.start.0,
                                        span.end.0,
                                        None,
                                    ),
                                ));
                            }
                        };
                        let field_index =
                            spread_struct_fields
                                .iter()
                                .position(|(n, _)| n == field_name)
                                .ok_or_else(|| {
                                    LowerError::rich(
                                        "E0113",
                                        crate::diagnostics::Diagnostic::error(format!(
                                            "the record after `..` has no field `{}`",
                                            field_name
                                        ))
                                        .with_primary_span(span.start.0, span.end.0, None),
                                    )
                                })?;
                        let result = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::GetField {
                                result,
                                base: *spread_val_id,
                                field_index,
                                result_ty: field_ty.clone(),
                            },
                            Some(field_ty.clone()),
                        );
                        field_vals.push(result);
                        field_actual_tys.push(field_ty.clone());
                    } else if let Some(defaults) = &field_defaults {
                        if let Some(Some(default_expr)) = defaults.get(field_idx) {
                            // Use the default value for this field.
                            let prev_binding_ty = self.binding_ty.take();
                            self.binding_ty = Some(field_ty.clone());
                            let (val, actual_ty) = self.lower_expr(default_expr)?;
                            self.binding_ty = prev_binding_ty;
                            let (val, actual_ty) = self.coerce_to_trait_object(
                                val,
                                actual_ty,
                                field_ty,
                                default_expr.span(),
                            )?;
                            field_vals.push(val);
                            field_actual_tys.push(actual_ty);
                        } else {
                            return Err(LowerError::rich(
                                "E0114",
                                crate::diagnostics::Diagnostic::error(format!(
                                    "missing field `{}`",
                                    field_name
                                ))
                                .with_primary_span(
                                    span.start.0,
                                    span.end.0,
                                    None,
                                ),
                            ));
                        }
                    } else {
                        return Err(LowerError::rich(
                            "E0114",
                            crate::diagnostics::Diagnostic::error(format!(
                                "missing field `{}`",
                                field_name
                            ))
                            .with_primary_span(
                                span.start.0,
                                span.end.0,
                                None,
                            ),
                        ));
                    }
                }

                // When not inside a monomorphized function, try to infer type params
                // from template fields vs actual field value types. E.g. Box { value: 42 }
                // has template field ("value", Struct{name:"T"}) and actual type Scalar(I64),
                // so we infer T=i64, resolve the fields, and mangle the name to Box__i64.
                self.finish_generic_struct_lit(
                    name,
                    &mut resolved_name,
                    &mut struct_fields,
                    &field_actual_tys,
                    spread_val.is_some(),
                );

                let result_ty = IrType::Struct {
                    name: resolved_name,
                    fields: struct_fields,
                };
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::MakeStruct {
                        result,
                        fields: field_vals,
                        result_ty: result_ty.clone(),
                    },
                    Some(result_ty.clone()),
                );
                Ok((result, result_ty))
            }

            AstExpr::FieldAccess { base, field, span } => {
                // Check if base is a bare identifier naming an enum → variant construction.
                if let AstExpr::Ident(base_ident) = base.as_ref() {
                    // Check if this is a qualified constant lookup
                    if !self.scope.contains_key(&base_ident.name) {
                        let resolved_q = resolve_qualifier(&base_ident.name);
                        let mangled_const = format!("{}__{}", resolved_q, field);
                        if let Some(val_ty) = self.scope.get(&mangled_const).cloned() {
                            return Ok(val_ty);
                        }
                    }

                    let resolved_enum = self.resolve_unqualified_name(&base_ident.name);
                    if let Some(variants) = self.module.enum_def(&resolved_enum) {
                        let variants = variants.clone();
                        let variant_idx =
                            variants.iter().position(|v| v == field).ok_or_else(|| {
                                LowerError::rich(
                                    "E0122",
                                    crate::diagnostics::Diagnostic::error(format!(
                                        "`{1}` has no variant `{0}`",
                                        field, resolved_enum
                                    ))
                                    .with_primary_span(
                                        span.start.0,
                                        span.end.0,
                                        None,
                                    ),
                                )
                            })?;
                        let result_ty = IrType::Enum {
                            name: resolved_enum,
                            variants,
                        };
                        let result = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::MakeVariant {
                                result,
                                variant_idx,
                                fields: vec![],
                                result_ty: result_ty.clone(),
                            },
                            Some(result_ty.clone()),
                        );
                        return Ok((result, result_ty));
                    }
                }

                // Check if base is a qualified enum name (e.g. `utils.Shape.Circle`)
                if let Some(enum_name) = get_qualified_enum_name(base.as_ref()) {
                    if let Some(variants) = self.module.enum_def(&enum_name) {
                        let variants = variants.clone();
                        let variant_idx =
                            variants.iter().position(|v| v == field).ok_or_else(|| {
                                LowerError::rich(
                                    "E0122",
                                    crate::diagnostics::Diagnostic::error(format!(
                                        "`{1}` has no variant `{0}`",
                                        field, enum_name
                                    ))
                                    .with_primary_span(
                                        span.start.0,
                                        span.end.0,
                                        None,
                                    ),
                                )
                            })?;
                        let result_ty = IrType::Enum {
                            name: enum_name,
                            variants,
                        };
                        let result = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::MakeVariant {
                                result,
                                variant_idx,
                                fields: vec![],
                                result_ty: result_ty.clone(),
                            },
                            Some(result_ty.clone()),
                        );
                        return Ok((result, result_ty));
                    }
                }
                // Normal struct field access — also handles grad<T>.value / grad<T>.grad
                let (base_val, base_ty) = self.lower_expr(base)?;
                // grad<T> pseudo-fields: .value → GradValue, .grad / .tangent → GradTangent
                if let IrType::Grad(inner) = &base_ty {
                    let inner_ty = *inner.clone();
                    let result = self.builder.fresh_value();
                    let (instr, ret_ty) = if field == "value" {
                        (
                            IrInstr::GradValue {
                                result,
                                operand: base_val,
                                ty: inner_ty.clone(),
                            },
                            inner_ty,
                        )
                    } else if field == "grad" || field == "tangent" {
                        (
                            IrInstr::GradTangent {
                                result,
                                operand: base_val,
                                ty: inner_ty.clone(),
                            },
                            inner_ty,
                        )
                    } else {
                        return Err(LowerError::rich(
                            "E0113",
                            crate::diagnostics::Diagnostic::error(format!(
                                "`grad<T>` has no field `{}`: use `.value` or `.grad`",
                                field
                            ))
                            .with_primary_span(
                                span.start.0,
                                span.end.0,
                                None,
                            ),
                        ));
                    };
                    self.builder.push_instr(instr, Some(ret_ty.clone()));
                    return Ok((result, ret_ty));
                }
                let struct_fields = match &base_ty {
                    IrType::Struct {
                        name: s_name,
                        fields,
                    } => {
                        if let Some(def_fields) = self.local_struct_defs.get(s_name) {
                            def_fields.clone()
                        } else if let Some(def_fields) = self.module.struct_def(s_name) {
                            def_fields.clone()
                        } else {
                            let resolved = self.resolve_unqualified_name(s_name);
                            if let Some(def_fields) = self.module.struct_def(&resolved) {
                                def_fields.clone()
                            } else {
                                fields.clone()
                            }
                        }
                    }
                    _ => {
                        return Err(LowerError::rich(
                            "E0124",
                            crate::diagnostics::Diagnostic::error(format!(
                                "`{}` has no fields: only records do",
                                hints::type_name(&base_ty)
                            ))
                            .with_primary_span(
                                span.start.0,
                                span.end.0,
                                None,
                            ),
                        ));
                    }
                };
                // THIR resolved the layout when it built the access. Use that
                // index when it still names the same field; local records and
                // generic bases can make THIR's index absent or stale, so the
                // name lookup remains the authority in those cases.
                let field_index = match crate::thir::field_index(*span) {
                    Some(index)
                        if struct_fields
                            .get(index)
                            .map(|(name, _)| name == field)
                            .unwrap_or(false) =>
                    {
                        index
                    }
                    _ => match struct_fields.iter().position(|(n, _)| n == field) {
                        Some(i) => i,
                        None => {
                            // The field name ends the access: `p.z`.
                            let at = Span::new(
                                span.end.0.saturating_sub(field.len() as u32),
                                span.end.0,
                            );
                            return Err(no_such_field(
                                &hints::type_name(&base_ty),
                                field,
                                &struct_fields,
                                at,
                            ));
                        }
                    },
                };
                let result_ty = struct_fields[field_index].1.clone();
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::GetField {
                        result,
                        base: base_val,
                        field_index,
                        result_ty: result_ty.clone(),
                    },
                    Some(result_ty.clone()),
                );
                Ok((result, result_ty))
            }

            AstExpr::When {
                scrutinee,
                arms,
                span,
            } => self.lower_when_expr(scrutinee, arms, *span),

            AstExpr::Tuple { elements, span } => {
                let mut elem_vals = Vec::with_capacity(elements.len());
                let mut elem_tys = Vec::with_capacity(elements.len());
                for e in elements {
                    let (v, t) = self.lower_expr(e)?;
                    elem_vals.push(v);
                    elem_tys.push(t);
                }
                let result_ty = IrType::Tuple(elem_tys);
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::MakeTuple {
                        result,
                        elements: elem_vals,
                        result_ty: result_ty.clone(),
                    },
                    Some(result_ty.clone()),
                );
                let _ = span;
                Ok((result, result_ty))
            }

            AstExpr::TupleIndex { base, index, span } => {
                let (base_val, base_ty) = self.lower_expr(base)?;
                let elem_types = match &base_ty {
                    IrType::Tuple(elems) => elems.clone(),
                    _ => {
                        return Err(LowerError::rich(
                            "E0124",
                            crate::diagnostics::Diagnostic::error(format!(
                                "`.N` takes an element of a tuple, but this is `{}`",
                                hints::type_name(&base_ty)
                            ))
                            .with_primary_span(
                                span.start.0,
                                span.end.0,
                                None,
                            ),
                        ));
                    }
                };
                if *index >= elem_types.len() {
                    return Err(LowerError::rich(
                        "E0125",
                        crate::diagnostics::Diagnostic::error(format!(
                            "tuple index {} is out of range: the tuple has {} elements",
                            index,
                            elem_types.len()
                        ))
                        .with_primary_span(span.start.0, span.end.0, None),
                    ));
                }
                let result_ty = elem_types[*index].clone();
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::GetElement {
                        result,
                        base: base_val,
                        index: *index,
                        result_ty: result_ty.clone(),
                    },
                    Some(result_ty.clone()),
                );
                Ok((result, result_ty))
            }

            AstExpr::Lambda { params, body, span } => self.lower_lambda(params, body, *span),

            AstExpr::ArrayLit { elems, span } => {
                if elems.is_empty() {
                    return Err(LowerError::rich("E0103", crate::diagnostics::Diagnostic::error("an empty array literal `[]` is not supported: use `list()` with a type annotation, e.g. `val xs: list<i64> = list()`".to_string()).with_primary_span(span.start.0, span.end.0, None)));
                }
                let mut elem_vals = Vec::with_capacity(elems.len());
                let mut elem_tys = Vec::with_capacity(elems.len());
                for e in elems {
                    let (v, ty) = self.lower_expr(e)?;
                    elem_vals.push(v);
                    elem_tys.push(ty);
                }
                let elem_ty = elem_tys
                    .pop()
                    .expect("the literal is not empty: checked above");
                let size = elem_vals.len();
                let result_ty = IrType::Array {
                    elem: Box::new(elem_ty.clone()),
                    len: size,
                };
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::AllocArray {
                        result,
                        elem_ty: elem_ty.clone(),
                        size,
                        init: elem_vals,
                    },
                    Some(result_ty.clone()),
                );
                let _ = span;
                Ok((result, result_ty))
            }

            AstExpr::Cast { expr, ty, span } => {
                let (operand_val, from_ty) = self.lower_expr(expr)?;
                let to_ty = lower_type(ty)?;
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::Cast {
                        result,
                        operand: operand_val,
                        from_ty: from_ty.clone(),
                        to_ty: to_ty.clone(),
                    },
                    Some(to_ty.clone()),
                );
                let _ = span;
                Ok((result, to_ty))
            }

            // await expr: expect a channel and recv its value
            AstExpr::Await { expr, span } => {
                let (val, ty) = self.lower_expr(expr)?;
                match ty {
                    // Awaiting a task: inside an `async def` this suspends the
                    // running task (lowered to a state machine by
                    // `pass::async_lower`); elsewhere it runs this thread's
                    // executor until the task is done.
                    IrType::Task(elem) => {
                        let elem_ty = (*elem).clone();
                        let name = if self.async_task.is_some() {
                            "async_await"
                        } else {
                            "async_block_on"
                        };
                        let result = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::BuiltinCall {
                                result,
                                name: name.to_owned(),
                                args: vec![val],
                                result_ty: elem_ty.clone(),
                            },
                            Some(elem_ty.clone()),
                        );
                        Ok((result, elem_ty))
                    }
                    // Awaiting a channel inside an `async def` receives without
                    // blocking the thread: other tasks run until a value arrives.
                    IrType::Chan(elem) if self.async_task.is_some() => {
                        let elem_ty = (*elem).clone();
                        let receive = self.builder.fresh_value();
                        let receive_ty = IrType::Task(Box::new(elem_ty.clone()));
                        self.builder.push_instr(
                            IrInstr::BuiltinCall {
                                result: receive,
                                name: "async_recv".to_owned(),
                                args: vec![val],
                                result_ty: receive_ty.clone(),
                            },
                            Some(receive_ty),
                        );
                        let result = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::BuiltinCall {
                                result,
                                name: "async_await".to_owned(),
                                args: vec![receive],
                                result_ty: elem_ty.clone(),
                            },
                            Some(elem_ty.clone()),
                        );
                        Ok((result, elem_ty))
                    }
                    IrType::Chan(elem) => {
                        let elem_ty = (*elem).clone();
                        let result = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::ChanRecv {
                                result,
                                chan: val,
                                elem_ty: elem_ty.clone(),
                            },
                            Some(elem_ty.clone()),
                        );
                        Ok((result, elem_ty))
                    }
                    other => Err(LowerError::TypeMismatch {
                        expected: "task<T> or chan<T>".to_owned(),
                        found: format!("{}", other),
                        span: *span,
                    }),
                }
            }

            AstExpr::Try { expr, span } => {
                let (val, res_ty) = self.lower_expr(expr)?;

                // Determine if this is a result or option type and branch accordingly.
                if let IrType::ResultType(ok, err) = &res_ty {
                    let ok_ty = (**ok).clone();
                    let err_ty = (**err).clone();

                    // Emit IsOk test.
                    let is_ok_result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::IsOk {
                            result: is_ok_result,
                            operand: val,
                        },
                        Some(IrType::Scalar(DType::Bool)),
                    );

                    let ok_bb = self.builder.create_block(Some("try_ok"));
                    let err_bb = self.builder.create_block(Some("try_err"));
                    let cont_bb = self.builder.create_block(Some("try_cont"));

                    self.builder.push_instr(
                        IrInstr::CondBr {
                            cond: is_ok_result,
                            then_block: ok_bb,
                            then_args: vec![],
                            else_block: err_bb,
                            else_args: vec![],
                        },
                        None,
                    );

                    // Ok branch: unwrap and continue.
                    self.builder.set_current_block(ok_bb);
                    let ok_unwrapped = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::ResultUnwrap {
                            result: ok_unwrapped,
                            operand: val,
                            result_ty: ok_ty.clone(),
                        },
                        Some(ok_ty.clone()),
                    );
                    self.builder.push_instr(
                        IrInstr::Br {
                            target: cont_bb,
                            args: vec![ok_unwrapped],
                        },
                        None,
                    );

                    // Err branch: early return or jump to catch block.
                    self.builder.set_current_block(err_bb);
                    let err_unwrapped = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::ResultUnwrapErr {
                            result: err_unwrapped,
                            operand: val,
                            result_ty: err_ty.clone(),
                        },
                        Some(err_ty.clone()),
                    );
                    if let (Some(catch_bb), Some(_cont_bb), Some(ref _catch_param)) = (
                        self.try_catch_catch_bb,
                        self.try_catch_cont_bb,
                        self.try_catch_param.clone(),
                    ) {
                        // Inside try/catch: jump to catch block with error
                        // value, which every `?` in the body must agree on.
                        match &self.try_catch_err_ty {
                            None => self.try_catch_err_ty = Some(err_ty.clone()),
                            Some(prev)
                                if !assignable(&err_ty, prev) && !assignable(prev, &err_ty) =>
                            {
                                return Err(LowerError::TypeMismatch {
                                    expected: format!(
                                        "{prev} (the error an earlier `?` in this `try` carries)"
                                    ),
                                    found: err_ty.to_string(),
                                    span: *span,
                                });
                            }
                            Some(_) => {}
                        }
                        self.builder.push_instr(
                            IrInstr::Br {
                                target: catch_bb,
                                args: vec![err_unwrapped],
                            },
                            None,
                        );
                    } else {
                        // Outside try/catch: early return with MakeErr, the
                        // success side the function's declared result's.
                        let err_result = self.builder.fresh_value();
                        let ok_side = match &self.current_return_ty {
                            Some(IrType::ResultType(ok, _)) => (**ok).clone(),
                            other => {
                                return Err(LowerError::TypeMismatch {
                                    expected:
                                        "a function returning `result` for `?` to return from"
                                            .into(),
                                    found: other
                                        .as_ref()
                                        .map_or("no return type".to_string(), |t| t.to_string()),
                                    span: *span,
                                })
                            }
                        };
                        let err_ret_ty =
                            IrType::ResultType(Box::new(ok_side), Box::new(err_ty.clone()));
                        self.builder.push_instr(
                            IrInstr::MakeErr {
                                result: err_result,
                                value: err_unwrapped,
                                result_ty: err_ret_ty.clone(),
                            },
                            Some(err_ret_ty.clone()),
                        );
                        self.builder.push_instr(
                            IrInstr::Return {
                                values: vec![err_result],
                            },
                            None,
                        );
                    }

                    // Continuation block: receives the Ok value.
                    self.builder.set_current_block(cont_bb);
                    let ok_result =
                        self.builder
                            .add_block_param(cont_bb, Some("try_result"), ok_ty.clone());
                    let _ = span;
                    Ok((ok_result, ok_ty))
                } else if let IrType::Option(inner) = &res_ty {
                    let ok_ty = (**inner).clone();

                    // Emit IsSome test.
                    let is_some_result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::IsSome {
                            result: is_some_result,
                            operand: val,
                        },
                        Some(IrType::Scalar(DType::Bool)),
                    );

                    let some_bb = self.builder.create_block(Some("try_some"));
                    let none_bb = self.builder.create_block(Some("try_none"));
                    let cont_bb = self.builder.create_block(Some("try_cont"));

                    self.builder.push_instr(
                        IrInstr::CondBr {
                            cond: is_some_result,
                            then_block: some_bb,
                            then_args: vec![],
                            else_block: none_bb,
                            else_args: vec![],
                        },
                        None,
                    );

                    // Some branch: unwrap and continue.
                    self.builder.set_current_block(some_bb);
                    let some_unwrapped = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::OptionUnwrap {
                            result: some_unwrapped,
                            operand: val,
                            result_ty: ok_ty.clone(),
                        },
                        Some(ok_ty.clone()),
                    );
                    self.builder.push_instr(
                        IrInstr::Br {
                            target: cont_bb,
                            args: vec![some_unwrapped],
                        },
                        None,
                    );

                    // None branch: early return with MakeNone.
                    self.builder.set_current_block(none_bb);
                    let none_result = self.builder.fresh_value();
                    // The function's declared option, not the operand's.
                    let none_ret_ty = match &self.current_return_ty {
                        Some(t @ IrType::Option(_)) => t.clone(),
                        _ => IrType::Option(Box::new(ok_ty.clone())),
                    };
                    self.builder.push_instr(
                        IrInstr::MakeNone {
                            result: none_result,
                            result_ty: none_ret_ty.clone(),
                        },
                        Some(none_ret_ty.clone()),
                    );
                    self.builder.push_instr(
                        IrInstr::Return {
                            values: vec![none_result],
                        },
                        None,
                    );

                    // Continuation block: receives the Some value.
                    self.builder.set_current_block(cont_bb);
                    let ok_result =
                        self.builder
                            .add_block_param(cont_bb, Some("try_result"), ok_ty.clone());
                    let _ = span;
                    Ok((ok_result, ok_ty))
                } else {
                    let _ = span;
                    Ok((val, res_ty))
                }
            }

            AstExpr::MethodCall {
                base,
                method,
                args,
                span,
            } => {
                // Check if base is a bare identifier naming an enum → variant construction with data.
                // e.g. `Shape.Circle(3.14)` is parsed as MethodCall(base=Ident("Shape"), method="Circle", args=[3.14])
                // Also handle module-qualified calls: `utils.normalize(x)` where `utils` is not a
                // local variable but `normalize` is a known function (imported via `bring`).
                if let AstExpr::Ident(base_ident) = base.as_ref() {
                    let resolved_q = resolve_qualifier(&base_ident.name);
                    let base_resolved_enum = self.resolve_unqualified_name(&base_ident.name);
                    // Module-qualified call: base not in scope, but method is a known function.
                    if !self.scope.contains_key(&base_ident.name)
                        && self.module.enum_def(&base_resolved_enum).is_none()
                        && self.module.struct_def(&base_resolved_enum).is_none()
                    {
                        let mangled_fn = format!("{}__{}", resolved_q, method);
                        // For generic functions, delegate to lower_call which handles monomorphization.
                        if self.generic_fns.contains_key(mangled_fn.as_str()) {
                            let synthetic_callee = Ident {
                                name: mangled_fn,
                                span: base_ident.span,
                            };
                            return self.lower_call(&synthetic_callee, args, &[], *span);
                        }
                        let ret_ty =
                            self.fn_sigs.get(mangled_fn.as_str()).cloned().or_else(|| {
                                self.mono_sigs.borrow().get(mangled_fn.as_str()).cloned()
                            });
                        if let Some(ret_ty) = ret_ty {
                            let mut arg_vals = Vec::with_capacity(args.len());
                            for arg in args {
                                let (v, _) = self.lower_expr(arg)?;
                                arg_vals.push(v);
                            }
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::Call {
                                    result: Some(result),
                                    callee: mangled_fn,
                                    args: arg_vals,
                                    result_ty: Some(ret_ty.clone()),
                                },
                                Some(ret_ty.clone()),
                            );
                            return Ok((result, ret_ty));
                        }

                        let unqualified_mangled_fn = self.resolve_unqualified_name(method);
                        // For generic functions, delegate to lower_call which handles monomorphization.
                        if self
                            .generic_fns
                            .contains_key(unqualified_mangled_fn.as_str())
                        {
                            let synthetic_callee = Ident {
                                name: unqualified_mangled_fn,
                                span: base_ident.span,
                            };
                            return self.lower_call(&synthetic_callee, args, &[], *span);
                        }
                        // Delegate to `lower_call` rather than emitting the
                        // `Call` here. Builtins live in `fn_sigs` too, so this
                        // branch used to match them and emit a *generic* call,
                        // skipping the dispatch that turns them into their real
                        // instruction. `process.exit(1)` became
                        //
                        //   %v38 = call i64 @exit(ptr 1)
                        //
                        // against `declare void @exit(i32)` -- wrong return
                        // type and wrong argument type -- which clang rejected,
                        // and the eval fallback then crashed. Bare `exit(1)`
                        // was always fine, because it went through `lower_call`.
                        // See known-issues #51.
                        //
                        // `lower_call` handles builtins, externs, trait dispatch
                        // and ordinary functions, so routing everything through
                        // it keeps one implementation instead of two that can
                        // disagree.
                        if self.fn_sigs.contains_key(unqualified_mangled_fn.as_str())
                            || self
                                .mono_sigs
                                .borrow()
                                .contains_key(unqualified_mangled_fn.as_str())
                        {
                            let synthetic_callee = Ident {
                                name: unqualified_mangled_fn,
                                span: base_ident.span,
                            };
                            return self.lower_call(&synthetic_callee, args, &[], *span);
                        }
                    }
                    if let Some(variants) = self.module.enum_def(&base_resolved_enum) {
                        let variants = variants.clone();
                        if let Some(variant_idx) = variants.iter().position(|v| v == method) {
                            // This is an enum variant constructor with data.
                            let mut field_vals = Vec::with_capacity(args.len());
                            for arg in args {
                                let (v, _) = self.lower_expr(arg)?;
                                field_vals.push(v);
                            }
                            let result_ty = IrType::Enum {
                                name: base_resolved_enum,
                                variants,
                            };
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::MakeVariant {
                                    result,
                                    variant_idx,
                                    fields: field_vals,
                                    result_ty: result_ty.clone(),
                                },
                                Some(result_ty.clone()),
                            );
                            return Ok((result, result_ty));
                        }
                    }
                }

                // Check if base is a qualified enum name (e.g. `utils.Shape.Circle(x)`)
                if let Some(enum_name) = get_qualified_enum_name(base.as_ref()) {
                    if let Some(variants) = self.module.enum_def(&enum_name) {
                        let variants = variants.clone();
                        if let Some(variant_idx) = variants.iter().position(|v| v == method) {
                            let mut field_vals = Vec::with_capacity(args.len());
                            for arg in args {
                                let (v, _) = self.lower_expr(arg)?;
                                field_vals.push(v);
                            }
                            let result_ty = IrType::Enum {
                                name: enum_name,
                                variants,
                            };
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::MakeVariant {
                                    result,
                                    variant_idx,
                                    fields: field_vals,
                                    result_ty: result_ty.clone(),
                                },
                                Some(result_ty.clone()),
                            );
                            return Ok((result, result_ty));
                        }
                    }
                }

                // Lower the receiver.
                let (base_val, base_ty) = self.lower_expr(base)?;

                // List functional method dispatch.
                if let IrType::List(inner_elem_ty) = &base_ty.clone() {
                    let elem_ty = *inner_elem_ty.clone();
                    match method.as_str() {
                        "map" => return self.lower_list_map(base_val, elem_ty, args, *span),
                        "filter" => return self.lower_list_filter(base_val, elem_ty, args, *span),
                        "fold" => return self.lower_list_fold(base_val, elem_ty, args, *span),
                        "any" => return self.lower_list_any(base_val, elem_ty, args, *span),
                        "all" => return self.lower_list_all(base_val, elem_ty, args, *span),
                        "len" => {
                            // lst.len() → ListLen
                            let result = self.builder.fresh_value();
                            let ty = IrType::Scalar(DType::I64);
                            self.builder.push_instr(
                                IrInstr::ListLen {
                                    result,
                                    list: base_val,
                                },
                                Some(ty.clone()),
                            );
                            return Ok((result, ty));
                        }
                        "push" => {
                            // lst.push(v) → list_push(lst, v) → returns unit
                            if args.len() != 1 {
                                return Err(builtin_arity_error(
                                    "push() requires exactly 1 argument",
                                    args.len(),
                                    *span,
                                ));
                            }
                            let (v, _) = self.lower_expr(&args[0])?;
                            self.builder.push_instr(
                                IrInstr::ListPush {
                                    list: base_val,
                                    value: v,
                                },
                                None,
                            );
                            // Return a dummy i64 zero as unit value
                            let zero = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::ConstInt {
                                    result: zero,
                                    value: 0,
                                    ty: IrType::Scalar(DType::I64),
                                },
                                Some(IrType::Scalar(DType::I64)),
                            );
                            return Ok((zero, IrType::Scalar(DType::I64)));
                        }
                        "pop" => {
                            // lst.pop() → list_pop(lst) → returns elem
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::ListPop {
                                    result,
                                    list: base_val,
                                    elem_ty: elem_ty.clone(),
                                },
                                Some(elem_ty.clone()),
                            );
                            return Ok((result, elem_ty));
                        }
                        "get" => {
                            // lst.get(i) → list_get(lst, i) → returns elem
                            if args.len() != 1 {
                                return Err(builtin_arity_error(
                                    "get() requires exactly 1 argument",
                                    args.len(),
                                    *span,
                                ));
                            }
                            let (idx, _) = self.lower_expr(&args[0])?;
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::ListGet {
                                    result,
                                    list: base_val,
                                    index: idx,
                                    elem_ty: elem_ty.clone(),
                                },
                                Some(elem_ty.clone()),
                            );
                            return Ok((result, elem_ty));
                        }
                        "set" => {
                            // lst.set(i, v) → list_set(lst, i, v) → returns unit
                            if args.len() != 2 {
                                return Err(builtin_arity_error(
                                    "set() requires exactly 2 arguments",
                                    args.len(),
                                    *span,
                                ));
                            }
                            let (idx, _) = self.lower_expr(&args[0])?;
                            let (v, _) = self.lower_expr(&args[1])?;
                            self.builder.push_instr(
                                IrInstr::ListSet {
                                    list: base_val,
                                    index: idx,
                                    value: v,
                                },
                                None,
                            );
                            // Return a dummy i64 zero as unit value
                            let zero = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::ConstInt {
                                    result: zero,
                                    value: 0,
                                    ty: IrType::Scalar(DType::I64),
                                },
                                Some(IrType::Scalar(DType::I64)),
                            );
                            return Ok((zero, IrType::Scalar(DType::I64)));
                        }
                        _ => {} // fall through to struct method dispatch
                    }
                }

                // Option<T> method dispatch.
                if let IrType::Option(inner_ty) = &base_ty {
                    let inner = (**inner_ty).clone();
                    match method.as_str() {
                        "is_some" => {
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::IsSome {
                                    result,
                                    operand: base_val,
                                },
                                Some(IrType::Scalar(DType::Bool)),
                            );
                            return Ok((result, IrType::Scalar(DType::Bool)));
                        }
                        "is_none" => {
                            let is_some_result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::IsSome {
                                    result: is_some_result,
                                    operand: base_val,
                                },
                                Some(IrType::Scalar(DType::Bool)),
                            );
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::UnaryOp {
                                    result,
                                    op: ScalarUnaryOp::Not,
                                    operand: is_some_result,
                                    ty: IrType::Scalar(DType::Bool),
                                },
                                Some(IrType::Scalar(DType::Bool)),
                            );
                            return Ok((result, IrType::Scalar(DType::Bool)));
                        }
                        "unwrap" => {
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::OptionUnwrap {
                                    result,
                                    operand: base_val,
                                    result_ty: inner.clone(),
                                },
                                Some(inner.clone()),
                            );
                            return Ok((result, inner));
                        }
                        "unwrap_or" => {
                            // opt.unwrap_or(default) → emulate with conditional via call to unwrap_or builtin
                            if args.len() != 1 {
                                return Err(builtin_arity_error(
                                    "unwrap_or() requires exactly 1 argument",
                                    args.len(),
                                    *span,
                                ));
                            }
                            // Build inline conditional: if is_some(opt) { opt.unwrap() } else { default }
                            let (default_val, _default_ty) = self.lower_expr(&args[0])?;
                            let is_some_result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::IsSome {
                                    result: is_some_result,
                                    operand: base_val,
                                },
                                Some(IrType::Scalar(DType::Bool)),
                            );
                            let then_bb = self.builder.create_block(Some("unwrap_or_then"));
                            let else_bb = self.builder.create_block(Some("unwrap_or_else"));
                            let merge_bb = self.builder.create_block(Some("unwrap_or_merge"));
                            self.builder.push_instr(
                                IrInstr::CondBr {
                                    cond: is_some_result,
                                    then_block: then_bb,
                                    then_args: vec![],
                                    else_block: else_bb,
                                    else_args: vec![],
                                },
                                None,
                            );
                            // Then branch: opt.unwrap()
                            self.builder.set_current_block(then_bb);
                            let unwrapped = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::OptionUnwrap {
                                    result: unwrapped,
                                    operand: base_val,
                                    result_ty: inner.clone(),
                                },
                                Some(inner.clone()),
                            );
                            let then_result = unwrapped;
                            self.builder.push_instr(
                                IrInstr::Br {
                                    target: merge_bb,
                                    args: vec![then_result],
                                },
                                None,
                            );
                            // Else branch: default
                            self.builder.set_current_block(else_bb);
                            let else_result = default_val;
                            self.builder.push_instr(
                                IrInstr::Br {
                                    target: merge_bb,
                                    args: vec![else_result],
                                },
                                None,
                            );
                            // Merge: phi
                            let merge_result = self.builder.add_block_param(
                                merge_bb,
                                Some("unwrap_or_res"),
                                inner.clone(),
                            );
                            self.builder.set_current_block(merge_bb);
                            return Ok((merge_result, inner));
                        }
                        "map" => {
                            // opt.map(f) → emulate: if is_some(opt) { some(f(opt.unwrap())) } else { none() }
                            if args.len() != 1 {
                                return Err(builtin_arity_error(
                                    "map() requires exactly 1 argument",
                                    args.len(),
                                    *span,
                                ));
                            }
                            let (f_val, f_ty) = self.lower_expr(&args[0])?;
                            // The function's return type.
                            let ret_ty = match &f_ty {
                                IrType::Fn { ret, .. } => (**ret).clone(),
                                other => {
                                    return Err(LowerError::TypeMismatch {
                                        expected: "a function".into(),
                                        found: other.to_string(),
                                        span: *span,
                                    })
                                }
                            };
                            let is_some_result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::IsSome {
                                    result: is_some_result,
                                    operand: base_val,
                                },
                                Some(IrType::Scalar(DType::Bool)),
                            );
                            let then_bb = self.builder.create_block(Some("opt_map_then"));
                            let else_bb = self.builder.create_block(Some("opt_map_else"));
                            let merge_bb = self.builder.create_block(Some("opt_map_merge"));
                            self.builder.push_instr(
                                IrInstr::CondBr {
                                    cond: is_some_result,
                                    then_block: then_bb,
                                    then_args: vec![],
                                    else_block: else_bb,
                                    else_args: vec![],
                                },
                                None,
                            );
                            // Then: call f(opt.unwrap())
                            self.builder.set_current_block(then_bb);
                            let unwrapped = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::OptionUnwrap {
                                    result: unwrapped,
                                    operand: base_val,
                                    result_ty: inner.clone(),
                                },
                                Some(inner.clone()),
                            );
                            // Call the closure
                            let call_result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::CallClosure {
                                    result: Some(call_result),
                                    closure: f_val,
                                    args: vec![unwrapped],
                                    result_ty: ret_ty.clone(),
                                    pass_env: true,
                                },
                                Some(ret_ty.clone()),
                            );
                            // Wrap in Some(...)
                            let some_result = self.builder.fresh_value();
                            let some_ty = IrType::Option(Box::new(ret_ty.clone()));
                            self.builder.push_instr(
                                IrInstr::MakeSome {
                                    result: some_result,
                                    value: call_result,
                                    result_ty: some_ty.clone(),
                                },
                                Some(some_ty.clone()),
                            );
                            self.builder.push_instr(
                                IrInstr::Br {
                                    target: merge_bb,
                                    args: vec![some_result],
                                },
                                None,
                            );
                            // Else: None
                            self.builder.set_current_block(else_bb);
                            let none_result = self.builder.fresh_value();
                            let none_ty = IrType::Option(Box::new(ret_ty.clone()));
                            self.builder.push_instr(
                                IrInstr::MakeNone {
                                    result: none_result,
                                    result_ty: none_ty.clone(),
                                },
                                Some(none_ty.clone()),
                            );
                            self.builder.push_instr(
                                IrInstr::Br {
                                    target: merge_bb,
                                    args: vec![none_result],
                                },
                                None,
                            );
                            // Merge
                            let merge_result = self.builder.add_block_param(
                                merge_bb,
                                Some("opt_map_res"),
                                none_ty.clone(),
                            );
                            self.builder.set_current_block(merge_bb);
                            return Ok((merge_result, none_ty));
                        }
                        _ => {} // fall through
                    }
                }

                // Result<T, E> method dispatch.
                if let IrType::ResultType(ok_ty, err_ty) = &base_ty {
                    let ok = (**ok_ty).clone();
                    let err = (**err_ty).clone();
                    match method.as_str() {
                        "is_ok" => {
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::IsOk {
                                    result,
                                    operand: base_val,
                                },
                                Some(IrType::Scalar(DType::Bool)),
                            );
                            return Ok((result, IrType::Scalar(DType::Bool)));
                        }
                        "is_err" => {
                            let is_ok_result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::IsOk {
                                    result: is_ok_result,
                                    operand: base_val,
                                },
                                Some(IrType::Scalar(DType::Bool)),
                            );
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::UnaryOp {
                                    result,
                                    op: ScalarUnaryOp::Not,
                                    operand: is_ok_result,
                                    ty: IrType::Scalar(DType::Bool),
                                },
                                Some(IrType::Scalar(DType::Bool)),
                            );
                            return Ok((result, IrType::Scalar(DType::Bool)));
                        }
                        "unwrap" => {
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::ResultUnwrap {
                                    result,
                                    operand: base_val,
                                    result_ty: ok.clone(),
                                },
                                Some(ok.clone()),
                            );
                            return Ok((result, ok));
                        }
                        "unwrap_err" => {
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::ResultUnwrapErr {
                                    result,
                                    operand: base_val,
                                    result_ty: err.clone(),
                                },
                                Some(err.clone()),
                            );
                            return Ok((result, err));
                        }
                        "unwrap_or" => {
                            if args.len() != 1 {
                                return Err(builtin_arity_error(
                                    "unwrap_or() requires exactly 1 argument",
                                    args.len(),
                                    *span,
                                ));
                            }
                            // Build inline conditional: if is_ok(res) { res.unwrap() } else { default }
                            let (default_val, _) = self.lower_expr(&args[0])?;
                            let is_ok_result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::IsOk {
                                    result: is_ok_result,
                                    operand: base_val,
                                },
                                Some(IrType::Scalar(DType::Bool)),
                            );
                            let then_bb = self.builder.create_block(Some("res_unwrap_or_then"));
                            let else_bb = self.builder.create_block(Some("res_unwrap_or_else"));
                            let merge_bb = self.builder.create_block(Some("res_unwrap_or_merge"));
                            self.builder.push_instr(
                                IrInstr::CondBr {
                                    cond: is_ok_result,
                                    then_block: then_bb,
                                    then_args: vec![],
                                    else_block: else_bb,
                                    else_args: vec![],
                                },
                                None,
                            );
                            // Then branch: res.unwrap()
                            self.builder.set_current_block(then_bb);
                            let unwrapped = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::ResultUnwrap {
                                    result: unwrapped,
                                    operand: base_val,
                                    result_ty: ok.clone(),
                                },
                                Some(ok.clone()),
                            );
                            self.builder.push_instr(
                                IrInstr::Br {
                                    target: merge_bb,
                                    args: vec![unwrapped],
                                },
                                None,
                            );
                            // Else branch: default
                            self.builder.set_current_block(else_bb);
                            self.builder.push_instr(
                                IrInstr::Br {
                                    target: merge_bb,
                                    args: vec![default_val],
                                },
                                None,
                            );
                            // Merge: phi
                            let merge_result = self.builder.add_block_param(
                                merge_bb,
                                Some("res_unwrap_or_res"),
                                ok.clone(),
                            );
                            self.builder.set_current_block(merge_bb);
                            return Ok((merge_result, ok));
                        }
                        _ => {} // fall through
                    }
                }

                // str method dispatch.
                if matches!(base_ty, IrType::Str) {
                    match method.as_str() {
                        "len" => {
                            let result = self.builder.fresh_value();
                            let ty = IrType::Scalar(DType::I64);
                            self.builder.push_instr(
                                IrInstr::StrLen {
                                    result,
                                    operand: base_val,
                                },
                                Some(ty.clone()),
                            );
                            return Ok((result, ty));
                        }
                        "to_upper" => {
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::StrToUpper {
                                    result,
                                    operand: base_val,
                                },
                                Some(IrType::Str),
                            );
                            return Ok((result, IrType::Str));
                        }
                        "to_lower" => {
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::StrToLower {
                                    result,
                                    operand: base_val,
                                },
                                Some(IrType::Str),
                            );
                            return Ok((result, IrType::Str));
                        }
                        "trim" => {
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::StrTrim {
                                    result,
                                    operand: base_val,
                                },
                                Some(IrType::Str),
                            );
                            return Ok((result, IrType::Str));
                        }
                        "contains" => {
                            if args.len() != 1 {
                                return Err(builtin_arity_error(
                                    "contains() requires exactly 1 argument",
                                    args.len(),
                                    *span,
                                ));
                            }
                            let (needle, _) = self.lower_expr(&args[0])?;
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::StrContains {
                                    result,
                                    haystack: base_val,
                                    needle,
                                },
                                Some(IrType::Scalar(DType::Bool)),
                            );
                            return Ok((result, IrType::Scalar(DType::Bool)));
                        }
                        "starts_with" => {
                            if args.len() != 1 {
                                return Err(builtin_arity_error(
                                    "starts_with() requires exactly 1 argument",
                                    args.len(),
                                    *span,
                                ));
                            }
                            let (prefix, _) = self.lower_expr(&args[0])?;
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::StrStartsWith {
                                    result,
                                    haystack: base_val,
                                    prefix,
                                },
                                Some(IrType::Scalar(DType::Bool)),
                            );
                            return Ok((result, IrType::Scalar(DType::Bool)));
                        }
                        "ends_with" => {
                            if args.len() != 1 {
                                return Err(builtin_arity_error(
                                    "ends_with() requires exactly 1 argument",
                                    args.len(),
                                    *span,
                                ));
                            }
                            let (suffix, _) = self.lower_expr(&args[0])?;
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::StrEndsWith {
                                    result,
                                    haystack: base_val,
                                    suffix,
                                },
                                Some(IrType::Scalar(DType::Bool)),
                            );
                            return Ok((result, IrType::Scalar(DType::Bool)));
                        }
                        "index" => {
                            // s.index(i) → StrIndex
                            if args.len() != 1 {
                                return Err(builtin_arity_error(
                                    "index() requires exactly 1 argument",
                                    args.len(),
                                    *span,
                                ));
                            }
                            let (idx, _) = self.lower_expr(&args[0])?;
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::StrIndex {
                                    result,
                                    string: base_val,
                                    index: idx,
                                },
                                Some(IrType::Scalar(DType::I64)),
                            );
                            return Ok((result, IrType::Scalar(DType::I64)));
                        }
                        "slice" => {
                            // s.slice(start, end) → StrSlice
                            if args.len() != 2 {
                                return Err(builtin_arity_error(
                                    "slice() requires exactly 2 arguments",
                                    args.len(),
                                    *span,
                                ));
                            }
                            let (start, _) = self.lower_expr(&args[0])?;
                            let (end, _) = self.lower_expr(&args[1])?;
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::StrSlice {
                                    result,
                                    string: base_val,
                                    start,
                                    end,
                                },
                                Some(IrType::Str),
                            );
                            return Ok((result, IrType::Str));
                        }
                        "find" => {
                            if args.len() != 1 {
                                return Err(builtin_arity_error(
                                    "find() requires exactly 1 argument",
                                    args.len(),
                                    *span,
                                ));
                            }
                            let (needle, _) = self.lower_expr(&args[0])?;
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::StrFind {
                                    result,
                                    haystack: base_val,
                                    needle,
                                },
                                Some(IrType::Option(Box::new(IrType::Scalar(DType::I64)))),
                            );
                            return Ok((
                                result,
                                IrType::Option(Box::new(IrType::Scalar(DType::I64))),
                            ));
                        }
                        "replace" => {
                            if args.len() != 2 {
                                return Err(builtin_arity_error(
                                    "replace() requires exactly 2 arguments",
                                    args.len(),
                                    *span,
                                ));
                            }
                            let (old_s, _) = self.lower_expr(&args[0])?;
                            let (new_s, _) = self.lower_expr(&args[1])?;
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::StrReplace {
                                    result,
                                    string: base_val,
                                    from: old_s,
                                    to: new_s,
                                },
                                Some(IrType::Str),
                            );
                            return Ok((result, IrType::Str));
                        }
                        "is_empty" => {
                            let len_result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::StrLen {
                                    result: len_result,
                                    operand: base_val,
                                },
                                Some(IrType::Scalar(DType::I64)),
                            );
                            let zero = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::ConstInt {
                                    result: zero,
                                    value: 0,
                                    ty: IrType::Scalar(DType::I64),
                                },
                                Some(IrType::Scalar(DType::I64)),
                            );
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::BinOp {
                                    result,
                                    op: BinOp::CmpEq,
                                    lhs: len_result,
                                    rhs: zero,
                                    ty: IrType::Scalar(DType::I64),
                                },
                                Some(IrType::Scalar(DType::Bool)),
                            );
                            return Ok((result, IrType::Scalar(DType::Bool)));
                        }
                        "split" => {
                            if args.len() != 1 {
                                return Err(builtin_arity_error(
                                    "split() requires exactly 1 argument",
                                    args.len(),
                                    *span,
                                ));
                            }
                            let (delim, _) = self.lower_expr(&args[0])?;
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::StrSplit {
                                    result,
                                    str_val: base_val,
                                    delim,
                                },
                                Some(IrType::List(Box::new(IrType::Str))),
                            );
                            return Ok((result, IrType::List(Box::new(IrType::Str))));
                        }
                        "repeat" => {
                            if args.len() != 1 {
                                return Err(builtin_arity_error(
                                    "repeat() requires exactly 1 argument",
                                    args.len(),
                                    *span,
                                ));
                            }
                            let (count, _) = self.lower_expr(&args[0])?;
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::StrRepeat {
                                    result,
                                    operand: base_val,
                                    count,
                                },
                                Some(IrType::Str),
                            );
                            return Ok((result, IrType::Str));
                        }
                        _ => {} // fall through
                    }
                }

                // i64/f64 method dispatch: to_str()
                if let IrType::Scalar(_dt) = &base_ty {
                    if method == "to_str" && args.is_empty() {
                        let result = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::ValueToStr {
                                result,
                                operand: base_val,
                            },
                            Some(IrType::Str),
                        );
                        return Ok((result, IrType::Str));
                    }
                    // Extension methods on scalars — e.g. 21.double()
                    // Try the extension method dispatch below.
                }

                // map<K,V> method dispatch.
                if let IrType::Map(_k_ty, v_ty) = &base_ty {
                    let v = (**v_ty).clone();
                    match method.as_str() {
                        "len" => {
                            let result = self.builder.fresh_value();
                            let ty = IrType::Scalar(DType::I64);
                            self.builder.push_instr(
                                IrInstr::MapLen {
                                    result,
                                    map: base_val,
                                },
                                Some(ty.clone()),
                            );
                            return Ok((result, ty));
                        }
                        "get" => {
                            if args.len() != 1 {
                                return Err(builtin_arity_error(
                                    "get() requires exactly 1 argument",
                                    args.len(),
                                    *span,
                                ));
                            }
                            let (k, _) = self.lower_expr(&args[0])?;
                            let result = self.builder.fresh_value();
                            let ret_ty = IrType::Option(Box::new(v.clone()));
                            self.builder.push_instr(
                                IrInstr::MapGet {
                                    result,
                                    map: base_val,
                                    key: k,
                                    val_ty: v.clone(),
                                },
                                Some(ret_ty.clone()),
                            );
                            return Ok((result, ret_ty));
                        }
                        "set" => {
                            if args.len() != 2 {
                                return Err(builtin_arity_error(
                                    "set() requires exactly 2 arguments",
                                    args.len(),
                                    *span,
                                ));
                            }
                            let (k, _) = self.lower_expr(&args[0])?;
                            let (v_val, _) = self.lower_expr(&args[1])?;
                            self.builder.push_instr(
                                IrInstr::MapSet {
                                    map: base_val,
                                    key: k,
                                    value: v_val,
                                },
                                None,
                            );
                            // Return a dummy i64 zero as unit value
                            let zero = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::ConstInt {
                                    result: zero,
                                    value: 0,
                                    ty: IrType::Scalar(DType::I64),
                                },
                                Some(IrType::Scalar(DType::I64)),
                            );
                            return Ok((zero, IrType::Scalar(DType::I64)));
                        }
                        "contains" => {
                            if args.len() != 1 {
                                return Err(builtin_arity_error(
                                    "contains() requires exactly 1 argument",
                                    args.len(),
                                    *span,
                                ));
                            }
                            let (k, _) = self.lower_expr(&args[0])?;
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::MapContains {
                                    result,
                                    map: base_val,
                                    key: k,
                                },
                                Some(IrType::Scalar(DType::Bool)),
                            );
                            return Ok((result, IrType::Scalar(DType::Bool)));
                        }
                        _ => {} // fall through
                    }
                }

                // channel<T> method dispatch.
                if let IrType::Chan(inner) = &base_ty {
                    let elem = (**inner).clone();
                    match method.as_str() {
                        "send" => {
                            if args.len() != 1 {
                                return Err(builtin_arity_error(
                                    "send() requires exactly 1 argument",
                                    args.len(),
                                    *span,
                                ));
                            }
                            let (v, v_ty) = self.lower_expr(&args[0])?;
                            // Record the concrete element type, exactly as the
                            // `send(ch, v)` builtin does. Without this the
                            // matching `recv` had nothing to look up and fell
                            // back to the channel's declared element type --
                            // `Infer` for an unannotated `channel()` -- leaving
                            // the received value boxed. See #51.
                            self.chan_elem_types
                                .entry(base_val)
                                .or_insert_with(|| v_ty.clone());
                            self.builder.push_instr(
                                IrInstr::ChanSend {
                                    chan: base_val,
                                    value: v,
                                },
                                None,
                            );
                            // Return a dummy i64 zero as unit value
                            let zero = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::ConstInt {
                                    result: zero,
                                    value: 0,
                                    ty: IrType::Scalar(DType::I64),
                                },
                                Some(IrType::Scalar(DType::I64)),
                            );
                            return Ok((zero, IrType::Scalar(DType::I64)));
                        }
                        "recv" => {
                            // Prefer the element type recorded when `send` was
                            // called, exactly as the `recv(ch)` builtin does.
                            // Using the channel's *declared* element type meant
                            // an unannotated `channel()` recv'd at `Infer`, so
                            // the result stayed boxed and
                            //
                            //   assert(ch.recv() == 99)
                            //
                            // emitted `icmp eq ptr %v, 99` -- invalid IR --
                            // while `recv(ch) == 99` was fine. Two lowerings of
                            // one operation that had drifted apart. See #51.
                            let elem_ty = self
                                .chan_elem_types
                                .get(&base_val)
                                .cloned()
                                .unwrap_or_else(|| elem.clone());
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::ChanRecv {
                                    result,
                                    chan: base_val,
                                    elem_ty: elem_ty.clone(),
                                },
                                Some(elem_ty.clone()),
                            );
                            return Ok((result, elem_ty));
                        }
                        _ => {} // fall through
                    }
                }

                // Extension method dispatch: try to find a user-defined function
                // named `method` whose first param matches the receiver type.
                // This enables `21.double()` → `double(21)` for any function `double(i64)`.
                if self.fn_sigs.contains_key(method) || self.generic_fns.contains_key(method) {
                    // Build a Call expression: method(base, args...)
                    let mut call_args = vec![*base.clone()];
                    call_args.extend(args.iter().cloned());
                    let call_expr = AstExpr::Call {
                        callee: Ident {
                            name: method.clone(),
                            span: *span,
                        },
                        args: call_args,
                        named_args: vec![],
                        span: *span,
                    };
                    return self.lower_expr(&call_expr);
                }

                // Trait-object dispatch: `obj.method(...)` where obj : dyn Trait.
                if let IrType::TraitObject {
                    name: trait_name,
                    methods: trait_methods,
                } = &base_ty
                {
                    // A trait-object type built without module context (a
                    // record field's `dyn Trait`) carries no method list; the
                    // trait's own definition is authoritative either way.
                    let method_sig = trait_methods
                        .iter()
                        .find(|m| m.name == *method)
                        .or_else(|| {
                            self.module
                                .trait_def(trait_name)
                                .and_then(|methods| methods.iter().find(|m| m.name == *method))
                        })
                        .cloned()
                        .ok_or_else(|| {
                            let uncallable = DYN_UNCALLABLE.with(|d| d.borrow().contains(&(trait_name.clone(), method.clone())));
                            let message = if uncallable {
                                format!("`{method}` of `{trait_name}` uses the trait's associated type, which only an impl knows, so it cannot be called through `dyn {trait_name}`")
                            } else {
                                format!("trait `{trait_name}` has no method `{method}`")
                            };
                            LowerError::rich("E0117", crate::diagnostics::Diagnostic::error(message).with_primary_span(span.start.0, span.end.0, None))
                        })?;
                    let mut arg_vals = Vec::with_capacity(args.len());
                    for arg in args {
                        let (v, _) = self.lower_expr(arg)?;
                        arg_vals.push(v);
                    }
                    let ret_ty = *method_sig.ret.clone();
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::DynCall {
                            result,
                            obj: base_val,
                            method_name: method.clone(),
                            args: arg_vals,
                            result_ty: ret_ty.clone(),
                        },
                        Some(ret_ty.clone()),
                    );
                    return Ok((result, ret_ty));
                }

                // A trait implemented for a non-struct receiver, e.g.
                // `impl Show for list<i64>`.
                //
                // Method dispatch required the receiver to be a struct, so an
                // impl on a container was registered in `trait_dispatch` and
                // then never consulted -- the call was rejected before reaching
                // it. That is what stopped `list` and `option` from sharing an
                // interface. See known-issues #38.
                if !matches!(base_ty, IrType::Struct { .. }) {
                    if let Some(cands) = self.trait_dispatch.get(method) {
                        // Exact match first; then, if the receiver's element
                        // type is unresolved, fall back to the constructor
                        // alone -- but only when that is unambiguous.
                        //
                        // `val n: option<i64> = none()` produces `option<_>`:
                        // the annotation does not reach the `none()` call, so
                        // an exact comparison against `option<i64>` fails even
                        // though the program named the type. Matching on the
                        // constructor recovers it; requiring uniqueness stops
                        // `option<i64>` and `option<str>` impls from silently
                        // resolving to whichever was registered first.
                        let exact = cands.iter().find(|(recv_ty, _)| recv_ty == &base_ty);
                        let chosen = match exact {
                            Some(hit) => Some(hit),
                            None => {
                                let same_ctor: Vec<&(IrType, String)> = cands
                                    .iter()
                                    .filter(|(recv_ty, _)| same_constructor(recv_ty, &base_ty))
                                    .collect();
                                if same_ctor.len() == 1 {
                                    Some(same_ctor[0])
                                } else {
                                    None
                                }
                            }
                        };
                        if let Some((_, fname)) = chosen {
                            let fname = fname.clone();
                            let mut arg_vals = Vec::with_capacity(args.len() + 1);
                            arg_vals.push(base_val);
                            for arg in args {
                                let (v, _) = self.lower_expr(arg)?;
                                arg_vals.push(v);
                            }
                            let ret_ty = self.sig_of(&fname, *span)?;
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::Call {
                                    result: Some(result),
                                    callee: fname,
                                    args: arg_vals,
                                    result_ty: Some(ret_ty.clone()),
                                },
                                Some(ret_ty.clone()),
                            );
                            return Ok((result, ret_ty));
                        }
                    }
                }

                // Determine the struct type name.
                let type_name = match &base_ty {
                    IrType::Struct { name, .. } => name.clone(),
                    IrType::TypeParam(tp_name) => {
                        let mut arg_vals = Vec::with_capacity(args.len() + 1);
                        arg_vals.push(base_val);
                        for arg in args {
                            let (v, _) = self.lower_expr(arg)?;
                            arg_vals.push(v);
                        }
                        let ret_ty = self
                            .trait_dispatch
                            .get(method)
                            .and_then(|cands| cands.first())
                            .and_then(|(_, fname)| self.fn_sigs.get(fname).cloned())
                            .unwrap_or(IrType::Str);
                        let result = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::Call {
                                result: Some(result),
                                callee: format!("__trait_method__{tp_name}____{method}"),
                                args: arg_vals,
                                result_ty: Some(ret_ty.clone()),
                            },
                            Some(ret_ty.clone()),
                        );
                        return Ok((result, ret_ty));
                    }
                    other => {
                        return Err(method_on_builtin_type(other, method, base, args, *span));
                    }
                };

                // Check if `method` is a struct field with a function type.
                // If so, extract the field and call it as a closure.
                let struct_mangled = format!("{}__{}", type_name, method);
                if !self.fn_sigs.contains_key(&struct_mangled) {
                    if let Some(fields) = self.module.struct_def(&type_name) {
                        if let Some((_, field_ty)) = fields.iter().find(|(name, _)| name == method)
                        {
                            if let IrType::Fn { params: _, ret } = field_ty {
                                let field_val = self.builder.fresh_value();
                                self.builder.push_instr(
                                    IrInstr::GetField {
                                        result: field_val,
                                        base: base_val,
                                        field_index: fields
                                            .iter()
                                            .position(|(n, _)| n == method)
                                            .unwrap(),
                                        result_ty: field_ty.clone(),
                                    },
                                    Some(field_ty.clone()),
                                );
                                let mut arg_vals = Vec::with_capacity(args.len());
                                for arg in args {
                                    let (v, _) = self.lower_expr(arg)?;
                                    arg_vals.push(v);
                                }
                                let ret_ty = *ret.clone();
                                let result = self.builder.fresh_value();
                                // Regular function via closure field: no env pointer.
                                self.builder.push_instr(
                                    IrInstr::CallClosure {
                                        result: Some(result),
                                        closure: field_val,
                                        args: arg_vals,
                                        result_ty: ret_ty.clone(),
                                        pass_env: false,
                                    },
                                    Some(ret_ty.clone()),
                                );
                                return Ok((result, ret_ty));
                            }
                        }
                    }
                }

                // Normal method dispatch: find the mangled function `TypeName__method`.
                let mangled = if self.fn_sigs.contains_key(&struct_mangled) {
                    struct_mangled
                } else if let Some(impls) = self.trait_dispatch.get(method) {
                    // Find the impl for this concrete type.
                    let dispatch_ty = IrType::Struct {
                        name: type_name.clone(),
                        fields: Vec::new(),
                    };
                    impls
                        .iter()
                        .find(|(ty, _)| {
                            if let (
                                IrType::Struct { name: n1, .. },
                                IrType::Struct { name: n2, .. },
                            ) = (ty, &dispatch_ty)
                            {
                                n1 == n2
                            } else {
                                ty == &dispatch_ty
                            }
                        })
                        .map(|(_, name)| name.clone())
                        .unwrap_or(struct_mangled)
                } else {
                    struct_mangled
                };

                // Look up return type.
                let ret_ty = self.sig_of(&mangled, *span)?;

                // Lower remaining arguments.
                let mut arg_vals = vec![base_val];
                for arg in args {
                    let (v, _) = self.lower_expr(arg)?;
                    arg_vals.push(v);
                }

                let result = self.builder.fresh_value();
                let call_instr = IrInstr::Call {
                    result: Some(result),
                    callee: mangled.clone(),
                    args: arg_vals,
                    result_ty: Some(ret_ty.clone()),
                };
                if self.module.generic_templates.contains_key(&mangled) {
                    self.builder.push_generic_call(
                        &mangled,
                        vec![base_ty.clone()],
                        call_instr,
                        Some(ret_ty.clone()),
                    );
                } else {
                    self.builder.push_instr(call_instr, Some(ret_ty.clone()));
                }
                Ok((result, ret_ty))
            }

            AstExpr::Handle {
                expr,
                arms,
                return_ty,
                ..
            } => self.lower_handle(expr, arms, return_ty),

            AstExpr::NullCoal { expr, default, .. } => {
                let (val, val_ty) = self.lower_expr(expr)?;
                let (default_val, _default_ty) = self.lower_expr(default)?;
                match &val_ty {
                    IrType::Option(inner) => {
                        let result_type = (**inner).clone();
                        let is_some_result = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::IsSome {
                                result: is_some_result,
                                operand: val,
                            },
                            Some(IrType::Scalar(DType::Bool)),
                        );
                        let then_bb = self.builder.create_block(Some("nullcoal_some"));
                        let else_bb = self.builder.create_block(Some("nullcoal_none"));
                        let merge_bb = self.builder.create_block(Some("nullcoal_merge"));
                        self.builder.push_instr(
                            IrInstr::CondBr {
                                cond: is_some_result,
                                then_block: then_bb,
                                then_args: vec![],
                                else_block: else_bb,
                                else_args: vec![],
                            },
                            None,
                        );
                        self.builder.set_current_block(then_bb);
                        let unwrapped = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::OptionUnwrap {
                                result: unwrapped,
                                operand: val,
                                result_ty: result_type.clone(),
                            },
                            Some(result_type.clone()),
                        );
                        self.builder.push_instr(
                            IrInstr::Br {
                                target: merge_bb,
                                args: vec![unwrapped],
                            },
                            None,
                        );
                        self.builder.set_current_block(else_bb);
                        self.builder.push_instr(
                            IrInstr::Br {
                                target: merge_bb,
                                args: vec![default_val],
                            },
                            None,
                        );
                        let merge_result = self.builder.add_block_param(
                            merge_bb,
                            Some("nullcoal_res"),
                            result_type.clone(),
                        );
                        self.builder.set_current_block(merge_bb);
                        Ok((merge_result, result_type))
                    }
                    IrType::ResultType(ok, _) => {
                        let result_type = (**ok).clone();
                        let is_ok_result = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::IsOk {
                                result: is_ok_result,
                                operand: val,
                            },
                            Some(IrType::Scalar(DType::Bool)),
                        );
                        let then_bb = self.builder.create_block(Some("nullcoal_ok"));
                        let else_bb = self.builder.create_block(Some("nullcoal_err"));
                        let merge_bb = self.builder.create_block(Some("nullcoal_merge"));
                        self.builder.push_instr(
                            IrInstr::CondBr {
                                cond: is_ok_result,
                                then_block: then_bb,
                                then_args: vec![],
                                else_block: else_bb,
                                else_args: vec![],
                            },
                            None,
                        );
                        self.builder.set_current_block(then_bb);
                        let unwrapped = self.builder.fresh_value();
                        self.builder.push_instr(
                            IrInstr::ResultUnwrap {
                                result: unwrapped,
                                operand: val,
                                result_ty: result_type.clone(),
                            },
                            Some(result_type.clone()),
                        );
                        self.builder.push_instr(
                            IrInstr::Br {
                                target: merge_bb,
                                args: vec![unwrapped],
                            },
                            None,
                        );
                        self.builder.set_current_block(else_bb);
                        self.builder.push_instr(
                            IrInstr::Br {
                                target: merge_bb,
                                args: vec![default_val],
                            },
                            None,
                        );
                        let merge_result = self.builder.add_block_param(
                            merge_bb,
                            Some("nullcoal_res"),
                            result_type.clone(),
                        );
                        self.builder.set_current_block(merge_bb);
                        Ok((merge_result, result_type))
                    }
                    _ => Ok((val, val_ty)),
                }
            }

            AstExpr::MapLiteral { entries, span } => {
                // Create empty map, then call map_set for each pair.
                let (key_ty, val_ty) = if let Some(IrType::Map(k, v)) = &self.binding_ty {
                    (*k.clone(), *v.clone())
                } else {
                    // THIR's key/value slots first, then the checker hint,
                    // then the first entry's type; an empty literal nothing
                    // else types was `map<str, i64>`.
                    let thir_types = crate::thir::map_types(*span);
                    let h = self.typeck_hint(*span);
                    let first = entries.first();
                    let k = thir_types
                        .as_ref()
                        .and_then(|(k, _)| k.clone())
                        .or_else(|| h.as_ref().and_then(|h| h.part(0)))
                        .or_else(|| first.and_then(|(k, _)| self.infer_ast_expr_type_simple(k)))
                        .ok_or_else(|| self.undetermined(*span, "this map's key type"))?;
                    let v = thir_types
                        .as_ref()
                        .and_then(|(_, v)| v.clone())
                        .or_else(|| h.as_ref().and_then(|h| h.part(1)))
                        .or_else(|| first.and_then(|(_, v)| self.infer_ast_expr_type_simple(v)))
                        .ok_or_else(|| self.undetermined(*span, "this map's value type"))?;
                    (k, v)
                };
                let map_ty = IrType::Map(Box::new(key_ty.clone()), Box::new(val_ty.clone()));
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::MapNew {
                        result,
                        key_ty: key_ty.clone(),
                        val_ty: val_ty.clone(),
                    },
                    Some(map_ty.clone()),
                );
                for (k, v) in entries {
                    let (key_id, _) = self.lower_expr(k)?;
                    let (val_id, _) = self.lower_expr(v)?;
                    self.builder.push_instr(
                        IrInstr::MapSet {
                            map: result,
                            key: key_id,
                            value: val_id,
                        },
                        None,
                    );
                }
                Ok((result, map_ty))
            }

            AstExpr::Ref { expr, span } => {
                let (val, ty, slot_proj) = self.resolve_ast_place(expr)?;
                Ok(self.emit_borrow_marker(val, ty, false, slot_proj, *span))
            }
            AstExpr::RefMut { expr, span } => {
                let (val, ty, slot_proj) = self.resolve_ast_place(expr)?;
                Ok(self.emit_borrow_marker(val, ty, true, slot_proj, *span))
            }
            AstExpr::Deref { expr, .. } => self.lower_expr(expr),
            AstExpr::Move { expr, span } => {
                let (val, ty, slot_proj) = self.resolve_ast_place(expr)?;
                Ok(self.emit_move_marker(val, ty, slot_proj.map(|(s, _)| s), *span))
            }
            AstExpr::Unsafe { body, .. } => self.lower_expr(body),
            AstExpr::Splat { expr, .. } => self.lower_expr(expr),
            AstExpr::MacroCall { name, .. } => Err(crate::error::LowerError::rich(
                "E0100",
                crate::diagnostics::Diagnostic::error(format!("no macro named `{}`", name.name))
                    .with_primary_span((name.span).start.0, (name.span).end.0, None),
            )),

            AstExpr::TryCatch {
                body,
                catch_param,
                catch_body,
                span: _,
            } => {
                let prev_catch = self.try_catch_catch_bb.take();
                let prev_cont = self.try_catch_cont_bb.take();
                let prev_param = self.try_catch_param.take();
                let prev_err_ty = self.try_catch_err_ty.take();

                let catch_bb = self.builder.create_block(Some("catch"));
                let cont_bb = self.builder.create_block(Some("try_cont"));

                self.try_catch_catch_bb = Some(catch_bb);
                self.try_catch_cont_bb = Some(cont_bb);
                self.try_catch_param = Some(catch_param.clone());

                let (body_val, body_ty) = self.lower_expr(body)?;
                if !self.builder.is_current_block_terminated() {
                    self.builder.push_instr(
                        IrInstr::Br {
                            target: cont_bb,
                            args: vec![body_val],
                        },
                        None,
                    );
                }

                self.builder.set_current_block(catch_bb);
                // The error the body's `?`s carry; a body without one never
                // reaches the catch, whose parameter is then never bound.
                let catch_ty = self.try_catch_err_ty.take().unwrap_or(IrType::Str);
                let catch_param_val =
                    self.builder
                        .add_block_param(catch_bb, Some(catch_param), catch_ty.clone());
                self.scope
                    .insert(catch_param.clone(), (catch_param_val, catch_ty));
                let (catch_val, _catch_ty) = self.lower_expr(catch_body)?;
                if !self.builder.is_current_block_terminated() {
                    self.builder.push_instr(
                        IrInstr::Br {
                            target: cont_bb,
                            args: vec![catch_val],
                        },
                        None,
                    );
                }

                self.builder.set_current_block(cont_bb);
                let result =
                    self.builder
                        .add_block_param(cont_bb, Some("try_result"), body_ty.clone());

                self.try_catch_catch_bb = prev_catch;
                self.try_catch_cont_bb = prev_cont;
                self.try_catch_param = prev_param;
                self.try_catch_err_ty = prev_err_ty;

                Ok((result, body_ty))
            }

            AstExpr::Raise {
                effect_name,
                args,
                span: _,
            } => {
                let mut lowered_args = Vec::new();
                let mut arg_tys = Vec::new();
                for a in args {
                    let (v, ty) = self.lower_expr(a)?;
                    lowered_args.push(v);
                    arg_tys.push(ty);
                }
                let ret_ty = self
                    .module
                    .extern_fns
                    .iter()
                    .find(|ef| ef.name == *effect_name)
                    .map(|ef| ef.ret_ty.clone())
                    .ok_or_else(|| {
                        LowerError::rich(
                            "E0100",
                            crate::diagnostics::Diagnostic::error(format!(
                                "no effect operation `{effect_name}` is declared"
                            ))
                            .with_code("E0100")
                            .with_primary_span(expr.span().start.0, expr.span().end.0, None)
                            .with_help(format!("declare it: `extern def {effect_name}(...) -> T`")),
                        )
                    })?;
                let result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::CallExtern {
                        result: Some(result),
                        name: effect_name.clone(),
                        args: lowered_args,
                        ret_ty: ret_ty.clone(),
                    },
                    Some(ret_ty.clone()),
                );
                Ok((result, ret_ty))
            }
        }
    }

    /// Lowers a lambda expression using lambda-lifting.
    ///
    /// Finds free variables (scope entries not covered by lambda params),
    /// generates a unique name `__lambda_N`, builds an `IrFunction` with
    /// `(captures..., params...)` parameter list, then emits `MakeClosure`.
    fn lower_lambda(
        &mut self,
        params: &[crate::parser::ast::AstParam],
        body: &AstExpr,
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        let counter = self.lambda_counter.get();
        self.lambda_counter.set(counter + 1);
        let fn_name = format!("__lambda_{}", counter);

        // Collect parameter names to exclude from free-variable search.
        let param_names: std::collections::HashSet<String> =
            params.iter().map(|p| p.name.name.clone()).collect();

        // Free variables: referenced scope entries that aren't lambda params.
        //
        // Sorted by name because `scope` is a `HashMap`, whose iteration order
        // changes with the per-process hash seed. The order is self-consistent
        // within one run — the lifted parameter list and the `MakeClosure`
        // argument list are both built from this vector — so programs were
        // correct, but the same source compiled to different IR on every run.
        // Reproducible builds matter on their own, and non-reproducibility also
        // hid a real miscompilation for as long as it lasted (known-issues #17).
        let mut captures: Vec<(String, ValueId, IrType)> = self
            .scope
            .iter()
            .filter(|(name, _)| {
                !param_names.contains(*name) && crate::pass::lint::expr_uses_name(body, name)
            })
            .map(|(name, (vid, ty))| (name.clone(), *vid, ty.clone()))
            .collect();
        captures.sort_by(|a, b| a.0.cmp(&b.0));
        let captures = captures;

        // Build the lifted function: params = captures + lambda_params.
        let mut lifted_params: Vec<Param> = captures
            .iter()
            .map(|(name, _, ty)| Param {
                name: name.clone(),
                ty: ty.clone(),
            })
            .collect();
        for p in params {
            lifted_params.push(Param {
                name: p.name.name.clone(),
                ty: self.resolve_ty(&p.ty)?,
            });
        }

        // The body decides the return type: lower it, then finish the
        // function with the body's type.
        let temp_builder =
            IrFunctionBuilder::new_return_decided_by_body(&fn_name, lifted_params.clone());
        let mut lambda_lowerer = self.new_sub_lowerer(temp_builder);

        let entry = lambda_lowerer.builder.create_block(Some("entry"));
        lambda_lowerer.builder.set_current_block(entry);

        // Populate the lambda scope with captured + param values.
        for (name, _, ty) in &captures {
            let val = lambda_lowerer
                .builder
                .add_block_param(entry, Some(name), ty.clone());
            lambda_lowerer.scope.insert(name.clone(), (val, ty.clone()));
        }
        for p in params {
            let ty = self.resolve_ty(&p.ty)?;
            let val = lambda_lowerer
                .builder
                .add_block_param(entry, Some(&p.name.name), ty.clone());
            lambda_lowerer.scope.insert(p.name.name.clone(), (val, ty));
        }

        let (ret_val, ret_ty) = lambda_lowerer.lower_expr(body)?;
        lambda_lowerer.builder.push_instr(
            IrInstr::Return {
                values: vec![ret_val],
            },
            None,
        );
        lambda_lowerer.builder.seal_unterminated_blocks();

        // The body's return type, and the capture count.
        let mut ir_func = lambda_lowerer.builder.build_returning(ret_ty.clone());
        ir_func.capture_count = captures.len();

        // Register the lifted function.
        self.lifted_fns.borrow_mut().push(ir_func);

        // Also register in fn_sigs-equivalent for the current lowering context
        // (no direct mutation possible; closures are called via CallClosure).

        // Emit MakeClosure in the current context.
        let capture_vals: Vec<ValueId> = captures.iter().map(|(_, v, _)| *v).collect();
        let closure_ty = IrType::Fn {
            params: lifted_params.iter().map(|p| p.ty.clone()).collect(),
            ret: Box::new(ret_ty),
        };
        let result = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::MakeClosure {
                result,
                fn_name: fn_name.clone(),
                captures: capture_vals,
                result_ty: closure_ty.clone(),
            },
            Some(closure_ty.clone()),
        );
        let _ = span;
        Ok((result, closure_ty))
    }

    /// Lowers a `handle <expr> with { <arms> }` expression.
    /// Emits `PushHandler`, body, `PopHandler`.
    fn lower_handle(
        &mut self,
        expr: &AstExpr,
        arms: &[AstHandlerArm],
        return_ty: &AstType,
    ) -> Result<(ValueId, IrType), LowerError> {
        // Lower each handler arm into an IR function + HandlerArm descriptor.
        let mut handler_arms = Vec::new();
        for arm in arms {
            let handler_arm = self.lower_handler_arm(arm)?;
            handler_arms.push(handler_arm);
        }

        // Emit PushHandler.
        self.builder
            .push_instr(IrInstr::PushHandler { arms: handler_arms }, None);

        // Lower the body expression.
        let (body_val, body_ty) = self.lower_expr(expr)?;

        // Emit PopHandler.
        self.builder.push_instr(IrInstr::PopHandler, None);

        // The result type of the handle expression: the body's, unless the
        // parser recorded one (it records `Infer`, "the body's").
        let result_ty = if is_inferred_return(return_ty) {
            body_ty
        } else {
            self.resolve_ty(return_ty)?
        };
        Ok((body_val, result_ty))
    }

    /// Lowers a single handler arm to a HandlerArm descriptor + lifted function.
    /// The handler body only receives the effect payload args (not captures),
    /// because the args are already evaluated before the extern call.
    fn lower_handler_arm(
        &mut self,
        arm: &AstHandlerArm,
    ) -> Result<crate::ir::instr::HandlerArm, LowerError> {
        let counter = self.lambda_counter.get();
        self.lambda_counter.set(counter + 1);
        let fn_name = format!("__handler_{}", counter);

        // Build the lifted function: params = handler_params only (no captures).
        // Use the extern function's signature for param types.
        // The operation's declaration types the arm's parameters and the
        // value it resumes with.
        let extern_fn = self
            .module
            .extern_fns
            .iter()
            .find(|ef| ef.name == arm.effect_name)
            .ok_or_else(|| {
                LowerError::rich(
                    "E0100",
                    crate::diagnostics::Diagnostic::error(format!(
                        "no effect operation `{}` is declared, so this handler has nothing to handle",
                        arm.effect_name
                    ))
                    .with_code("E0100")
                    .with_primary_span(arm.span.start.0, arm.span.end.0, None)
                    .with_help(format!("declare it: `extern def {}(...) -> T`", arm.effect_name)),
                )
            })?;
        if arm.params.len() > extern_fn.param_types.len() {
            return Err(LowerError::rich(
                "E0109",
                crate::diagnostics::Diagnostic::error(format!(
                    "the handler for `{}` takes {} values, but the operation passes {}",
                    arm.effect_name,
                    arm.params.len(),
                    extern_fn.param_types.len()
                ))
                .with_code("E0109")
                .with_primary_span(arm.span.start.0, arm.span.end.0, None),
            ));
        }
        let mut lifted_params: Vec<Param> = Vec::new();
        for (p, ty) in arm.params.iter().zip(&extern_fn.param_types) {
            lifted_params.push(Param {
                name: p.name.clone(),
                ty: ty.clone(),
            });
        }
        // Always add a leading block param for the continuation pointer.
        // For has_resume arms, this is named after the resume param.
        // For non-resume arms, this is an unnamed "__cont" param (ignored).
        let cont_param_name = if let Some(ref rp) = arm.resume_param {
            rp.name.clone()
        } else {
            "__cont".to_string()
        };
        // The continuation: an opaque handle, resumed with a value of the
        // operation's result type.
        let cont_ty = IrType::WeakRef(Box::new(extern_fn.ret_ty.clone()));
        lifted_params.insert(
            0,
            Param {
                name: cont_param_name.clone(),
                ty: cont_ty.clone(),
            },
        );

        let temp_builder =
            IrFunctionBuilder::new_return_decided_by_body(&fn_name, lifted_params.clone());
        let mut handler_lowerer = self.new_sub_lowerer(temp_builder);
        handler_lowerer.resume_param_name = arm.resume_param.as_ref().map(|r| r.name.clone());

        let entry = handler_lowerer.builder.create_block(Some("entry"));
        handler_lowerer.builder.set_current_block(entry);

        // Populate the handler scope with param values only.
        // The continuation is the first block param when has_resume is true.
        if let Some(ref rp) = arm.resume_param {
            let cont_ty = cont_ty.clone();
            let val =
                handler_lowerer
                    .builder
                    .add_block_param(entry, Some(&rp.name), cont_ty.clone());
            handler_lowerer
                .scope
                .insert(rp.name.clone(), (val, cont_ty));
        }
        // Use the types already recovered from the extern signature above.
        //
        // These block params were previously added as `IrType::Infer`, which
        // discarded the very types `lifted_params` had just looked up. Inference
        // then defaulted them to `i64`, so a handler that *used* its parameter
        // produced a `str` bound as an integer: interpreted, `"s:" + p`
        // evaluated to 0; natively, codegen emitted
        // `call ptr @iris_str_concat(ptr %v2, ptr %p)` with `%p` defined as
        // i64, which fails LLVM verification.
        //
        // `test_resume_handler.iris` passed on both backends throughout,
        // because its handler ignores its argument. See known-issues #31.
        // Read the types straight from the extern signature rather than from
        // `lifted_params`, which has the continuation inserted at index 0 and
        // is therefore off by one against `arm.params`. Indexing it by the arm
        // position gave every first parameter the continuation's
        // `weak_ref<_>`, which is why `"s:" + p` reported
        // `expected 'str' but found 'weak_ref<_>'`.
        for (p, ty) in arm.params.iter().zip(&extern_fn.param_types) {
            let ty = ty.clone();
            let val = handler_lowerer
                .builder
                .add_block_param(entry, Some(&p.name), ty.clone());
            handler_lowerer.scope.insert(p.name.clone(), (val, ty));
        }

        let (ret_val, ret_ty) = handler_lowerer.lower_expr(&arm.body)?;
        handler_lowerer.builder.push_instr(
            IrInstr::Return {
                values: vec![ret_val],
            },
            None,
        );
        handler_lowerer.builder.seal_unterminated_blocks();

        let mut ir_func = handler_lowerer.builder.build_returning(ret_ty.clone());
        ir_func.capture_count = 0;

        self.lifted_fns.borrow_mut().push(ir_func);

        Ok(crate::ir::instr::HandlerArm {
            effect_name: arm.effect_name.clone(),
            func_name: fn_name,
            num_args: arm.params.len(),
            has_resume: arm.resume_param.is_some(),
        })
    }

    /// Try to evaluate a `const def` function at compile time.
    /// Returns `Some(result)` if all arguments are integer/float/bool literals
    /// and the body is a simple expression (arithmetic, if/else, etc.).
    /// Returns `None` if evaluation fails (non-literal args, unsupported body, etc.).
    fn try_eval_const_fn(
        &self,
        func: &crate::parser::ast::AstFunction,
        args: &[AstExpr],
    ) -> Option<i64> {
        // All arguments must be compile-time known literals.
        let mut params: HashMap<String, i64> = HashMap::new();
        for (param, arg) in func.params.iter().zip(args.iter()) {
            let val = match arg {
                AstExpr::IntLit { value, .. } => *value,
                AstExpr::FloatLit { value, .. } => value.to_bits() as i64,
                AstExpr::BoolLit { value, .. } => {
                    if *value {
                        1
                    } else {
                        0
                    }
                }
                _ => return None,
            };
            params.insert(param.name.name.clone(), val);
        }

        self.eval_const_block(&func.body.stmts, &params)
    }

    /// Evaluate a block of statements at compile time, returning the value of
    /// the final expression statement.
    fn eval_const_block(
        &self,
        stmts: &[crate::parser::ast::AstStmt],
        params: &HashMap<String, i64>,
    ) -> Option<i64> {
        let mut locals: HashMap<String, i64> = params.clone();
        for stmt in stmts {
            match stmt {
                crate::parser::ast::AstStmt::Let { name, init, .. } => {
                    let val = self.eval_const_expr(init, &locals)?;
                    locals.insert(name.name.clone(), val);
                }
                crate::parser::ast::AstStmt::Expr(expr) => {
                    return self.eval_const_expr(expr, &locals);
                }
                _ => return None,
            }
        }
        None
    }

    /// Evaluate a simple expression at compile time.
    fn eval_const_expr(&self, expr: &AstExpr, locals: &HashMap<String, i64>) -> Option<i64> {
        match expr {
            AstExpr::IntLit { value, .. } => Some(*value),
            AstExpr::FloatLit { value, .. } => Some(value.to_bits() as i64),
            AstExpr::BoolLit { value, .. } => Some(if *value { 1 } else { 0 }),
            AstExpr::Ident(name) => locals.get(name.name.as_str()).copied(),
            AstExpr::BinOp { op, lhs, rhs, .. } => {
                let l = self.eval_const_expr(lhs, locals)?;
                let r = self.eval_const_expr(rhs, locals)?;
                match op {
                    // Checked: an overflow is not folded, so the call runs
                    // and stops with E0409, as the same code does at run time.
                    crate::parser::ast::AstBinOp::Add => l.checked_add(r),
                    crate::parser::ast::AstBinOp::Sub => l.checked_sub(r),
                    crate::parser::ast::AstBinOp::Mul => l.checked_mul(r),
                    crate::parser::ast::AstBinOp::Div => {
                        if r == 0 {
                            return None;
                        }
                        l.checked_div(r)
                    }
                    crate::parser::ast::AstBinOp::Mod => {
                        if r == 0 {
                            return None;
                        }
                        Some(l.wrapping_rem(r))
                    }
                    crate::parser::ast::AstBinOp::CmpEq => Some(if l == r { 1 } else { 0 }),
                    crate::parser::ast::AstBinOp::CmpNe => Some(if l != r { 1 } else { 0 }),
                    crate::parser::ast::AstBinOp::CmpLt => Some(if l < r { 1 } else { 0 }),
                    crate::parser::ast::AstBinOp::CmpLe => Some(if l <= r { 1 } else { 0 }),
                    crate::parser::ast::AstBinOp::CmpGt => Some(if l > r { 1 } else { 0 }),
                    crate::parser::ast::AstBinOp::CmpGe => Some(if l >= r { 1 } else { 0 }),
                    crate::parser::ast::AstBinOp::And => Some(if l != 0 && r != 0 { 1 } else { 0 }),
                    crate::parser::ast::AstBinOp::Or => Some(if l != 0 || r != 0 { 1 } else { 0 }),
                }
            }
            AstExpr::UnaryOp { op, expr, .. } => {
                let v = self.eval_const_expr(expr, locals)?;
                match op {
                    crate::parser::ast::AstUnaryOp::Neg => v.checked_neg(),
                    crate::parser::ast::AstUnaryOp::Not => Some(if v == 0 { 1 } else { 0 }),
                }
            }
            AstExpr::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                let c = self.eval_const_expr(cond, locals)?;
                if c != 0 {
                    self.eval_const_block(&then_block.stmts, locals)
                } else if let Some(eb) = else_block {
                    self.eval_const_block(&eb.stmts, locals)
                } else {
                    None
                }
            }
            AstExpr::Block(block) => {
                let mut block_locals = locals.clone();
                for stmt in &block.stmts {
                    match stmt {
                        crate::parser::ast::AstStmt::Let { name, init, .. } => {
                            let val = self.eval_const_expr(init, &block_locals)?;
                            block_locals.insert(name.name.clone(), val);
                        }
                        _ => return None,
                    }
                }
                if let Some(ref result_expr) = block.tail {
                    self.eval_const_expr(result_expr, &block_locals)
                } else {
                    Some(0)
                }
            }
            _ => None,
        }
    }

    /// Lowers `if cond { then_blk } [else { else_blk }]` to SSA control flow.
    ///
    /// **With else**: Creates three blocks (then / else / merge) with a `CondBr`.
    /// Each branch is lowered independently; the merge block receives the result
    /// via a block parameter.
    ///
    /// **Without else**: Creates two blocks (then / merge). The expression always
    /// evaluates to unit (`i64 0`) — the then branch runs for its side effects.
    ///
    /// If a branch terminates early (e.g. via `return`), no `Br` to merge is
    /// emitted for that branch.
    fn lower_if_expr(
        &mut self,
        cond: &AstExpr,
        then_blk: &AstBlock,
        else_blk: Option<&AstBlock>,
        span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        let _ = span; // span used only for error messages which no longer apply

        // 1. Evaluate condition in the current block.
        let (cond_val, _) = self.lower_expr(cond)?;
        // Consume the expected expression type (set by `val` / `return` handlers)
        // so that if/else branches can coerce their results before the merge.
        let expected_ty = self.expected_expr_ty.take();
        let outer_scope = self.scope.clone();
        let outer_slots = self.scope_slots.clone();
        let mut rebound_names = find_rebound_vars(then_blk);
        if let Some(else_blk) = else_blk {
            for name in find_rebound_vars(else_blk) {
                if !rebound_names.contains(&name) {
                    rebound_names.push(name);
                }
            }
        }
        rebound_names.retain(|name| outer_scope.contains_key(name));

        if let Some(else_blk) = else_blk {
            // Full if/else: three-block CFG (then / else / merge).
            // If both branches are statement-only, the expression evaluates to
            // unit and both branches must still jump to the merge block.
            let unit_ty = IrType::Scalar(DType::I64);
            let unit_val = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::ConstInt {
                    result: unit_val,
                    value: 0,
                    ty: unit_ty.clone(),
                },
                Some(unit_ty.clone()),
            );

            let then_bb = self.builder.create_block(Some("then"));
            let else_bb = self.builder.create_block(Some("else"));
            let merge_bb = self.builder.create_block(Some("merge"));

            self.builder.push_instr(
                IrInstr::CondBr {
                    cond: cond_val,
                    then_block: then_bb,
                    then_args: vec![],
                    else_block: else_bb,
                    else_args: vec![],
                },
                None,
            );

            // Which block each arm's `Br` ended up in, so its arguments can be
            // extended once both arms are known (autodiff handles, #50).
            let mut then_br_block: Option<BlockId> = None;
            let mut else_br_block: Option<BlockId> = None;

            // Lower THEN branch.
            self.builder.set_current_block(then_bb);
            self.scope = outer_scope.clone();
            self.scope_slots = outer_slots.clone();
            let mut then_result = self.lower_block(then_blk)?;
            // Coerce to expected dyn Trait if needed.
            if let (Some(ref expected), Some((tv, ref tt))) = (&expected_ty, &then_result) {
                let (cv, ct) = self.coerce_to_trait_object(*tv, tt.clone(), expected, span)?;
                then_result = Some((cv, ct));
            }
            let then_scope = self.scope.clone();
            if !self.builder.is_current_block_terminated() {
                let mut then_args = vec![then_result
                    .as_ref()
                    .map(|(then_val, _)| *then_val)
                    .unwrap_or(unit_val)];
                for name in &rebound_names {
                    let rebound_val = then_scope
                        .get(name)
                        .or_else(|| outer_scope.get(name))
                        .map(|(val, _)| *val)
                        .expect("rebound variable missing from then-scope");
                    then_args.push(rebound_val);
                }
                then_br_block = Some(self.builder.current_block());
                self.builder.push_instr(
                    IrInstr::Br {
                        target: merge_bb,
                        args: then_args,
                    },
                    None,
                );
            }
            self.scope = outer_scope.clone();
            self.scope_slots = outer_slots.clone();

            // Lower ELSE branch.
            self.builder.set_current_block(else_bb);
            self.scope = outer_scope.clone();
            self.scope_slots = outer_slots.clone();
            let mut else_result = self.lower_block(else_blk)?;
            // Coerce to expected dyn Trait if needed.
            if let (Some(ref expected), Some((ev, ref et))) = (&expected_ty, &else_result) {
                let (cv, ct) = self.coerce_to_trait_object(*ev, et.clone(), expected, span)?;
                else_result = Some((cv, ct));
            }
            let else_scope = self.scope.clone();
            if !self.builder.is_current_block_terminated() {
                let mut else_args = vec![else_result
                    .as_ref()
                    .map(|(else_val, _)| *else_val)
                    .unwrap_or(unit_val)];
                for name in &rebound_names {
                    let rebound_val = else_scope
                        .get(name)
                        .or_else(|| outer_scope.get(name))
                        .map(|(val, _)| *val)
                        .expect("rebound variable missing from else-scope");
                    else_args.push(rebound_val);
                }
                else_br_block = Some(self.builder.current_block());
                self.builder.push_instr(
                    IrInstr::Br {
                        target: merge_bb,
                        args: else_args,
                    },
                    None,
                );
            }
            self.scope = outer_scope.clone();
            self.scope_slots = outer_slots.clone();

            // Merge block parameter type = type of whichever branch produced a value.
            let result_ty = match (&then_result, &else_result) {
                (Some((_, ty)), _) => ty.clone(),
                (_, Some((_, ty))) => ty.clone(),
                (None, None) => unit_ty,
            };

            let result =
                self.builder
                    .add_block_param(merge_bb, Some("if_result"), result_ty.clone());
            let mut rebound_params = Vec::new();
            for name in &rebound_names {
                let Some((_, ty)) = outer_scope.get(name) else {
                    continue;
                };
                let param = self
                    .builder
                    .add_block_param(merge_bb, Some(name), ty.clone());
                rebound_params.push((name.clone(), param, ty.clone()));
            }
            // If both arms produced a taped value, the merged result must carry a
            // handle too -- otherwise it arrives at `backward` as a bare f64 and
            // is rejected. Threaded as a trailing merge parameter, appended to
            // both branch arguments after the fact because the arms were emitted
            // before their sibling was known. Only when *both* arms are taped:
            // a one-sided graph would silently drop the other path's gradient.
            let then_handle = then_result
                .as_ref()
                .and_then(|(v, _)| self.tape_nodes.get(v).copied());
            let else_handle = else_result
                .as_ref()
                .and_then(|(v, _)| self.tape_nodes.get(v).copied());
            if let (Some(th), Some(eh), Some(tb), Some(eb)) =
                (then_handle, else_handle, then_br_block, else_br_block)
            {
                let handle_param =
                    self.builder
                        .add_block_param(merge_bb, Some("if_result$tape"), IrType::TapeRef);
                let patched_then = self.builder.append_br_arg(tb, merge_bb, th);
                let patched_else = self.builder.append_br_arg(eb, merge_bb, eh);
                if patched_then && patched_else {
                    self.taped_values.insert(result);
                    self.tape_nodes.insert(result, handle_param);
                }
            }

            self.builder.set_current_block(merge_bb);
            for (name, param, ty) in rebound_params {
                if let Some(&slot) = self.scope_slots.get(&name) {
                    self.builder.map_value_slot_if_unset(param, slot);
                }
                self.scope.insert(name, (param, ty));
            }
            Ok((result, result_ty))
        } else {
            // if-without-else: two-block CFG (then / merge).
            // The whole expression evaluates to unit (i64 0).
            let unit_ty = IrType::Scalar(DType::I64);
            let unit_val = self.builder.fresh_value();
            self.builder.push_instr(
                IrInstr::ConstInt {
                    result: unit_val,
                    value: 0,
                    ty: unit_ty.clone(),
                },
                Some(unit_ty.clone()),
            );

            let then_bb = self.builder.create_block(Some("then"));
            let merge_bb = self.builder.create_block(Some("merge"));

            // False branch jumps directly to merge with the unit value.
            self.builder.push_instr(
                IrInstr::CondBr {
                    cond: cond_val,
                    then_block: then_bb,
                    then_args: vec![],
                    else_block: merge_bb,
                    else_args: std::iter::once(unit_val)
                        .chain(
                            rebound_names
                                .iter()
                                .filter_map(|name| outer_scope.get(name).map(|(val, _)| *val)),
                        )
                        .collect(),
                },
                None,
            );

            // Lower THEN branch (side effects only; result is discarded).
            self.builder.set_current_block(then_bb);
            self.scope = outer_scope.clone();
            self.scope_slots = outer_slots.clone();
            let _then_result = self.lower_block(then_blk)?;
            let then_scope = self.scope.clone();
            if !self.builder.is_current_block_terminated() {
                // Branch didn't return early: jump to merge with unit.
                let mut then_args = vec![unit_val];
                for name in &rebound_names {
                    let rebound_val = then_scope
                        .get(name)
                        .or_else(|| outer_scope.get(name))
                        .map(|(val, _)| *val)
                        .expect("rebound variable missing from then-scope");
                    then_args.push(rebound_val);
                }
                self.builder.push_instr(
                    IrInstr::Br {
                        target: merge_bb,
                        args: then_args,
                    },
                    None,
                );
            }
            self.scope = outer_scope.clone();
            self.scope_slots = outer_slots.clone();

            let merge_param =
                self.builder
                    .add_block_param(merge_bb, Some("if_result"), unit_ty.clone());
            let mut rebound_params = Vec::new();
            for name in &rebound_names {
                let Some((_, ty)) = outer_scope.get(name) else {
                    continue;
                };
                let param = self
                    .builder
                    .add_block_param(merge_bb, Some(name), ty.clone());
                rebound_params.push((name.clone(), param, ty.clone()));
            }
            self.builder.set_current_block(merge_bb);
            for (name, param, ty) in rebound_params {
                if let Some(&slot) = self.scope_slots.get(&name) {
                    self.builder.map_value_slot_if_unset(param, slot);
                }
                self.scope.insert(name, (param, ty));
            }
            Ok((merge_param, unit_ty))
        }
    }

    /// The return type registered for the function `name`, which dispatch
    /// has just chosen: one missing is lowering's own defect.
    fn sig_of(&self, name: &str, span: crate::parser::lexer::Span) -> Result<IrType, LowerError> {
        self.fn_sigs
            .get(name)
            .cloned()
            .or_else(|| self.mono_sigs.borrow().get(name).cloned())
            .ok_or_else(|| {
                LowerError::rich(
                    "E0900",
                    crate::diagnostics::Diagnostic::error(format!(
                        "internal compiler error: no signature registered for `{name}`"
                    ))
                    .with_code("E0900")
                    .with_primary_span(span.start.0, span.end.0, None),
                )
            })
    }

    /// What the checker solved of the type of the expression at `span`
    /// (see `typeck::Hint`), this instance's type parameters substituted: a
    /// generic body is checked once, with its parameters open.
    fn typeck_hint(&self, span: crate::parser::lexer::Span) -> Option<typeck::Hint> {
        // THIR owns the type-flow stage now. For a span THIR saw, its hint is
        // authoritative: `None` means the checker left the expression open,
        // not that lowering should ask the HIR again. Only lowering-
        // synthesized spans THIR never saw use the HIR fallback.
        let hint = if crate::thir::is_source_span(span) {
            crate::thir::hint(span)
        } else {
            typeck::hint(span)
        };
        hint.map(|h| h.instantiate(&self.type_param_subs))
    }

    /// The checker's whole type for the expression at `span`, when it knows
    /// every part of it.
    fn typeck_type(&self, span: crate::parser::lexer::Span) -> Option<IrType> {
        self.typeck_hint(span).and_then(|h| h.known())
    }

    /// `lhs op rhs` for a compound assignment (`x op= v`) on a value of type
    /// `ty`: string `+=` concatenates, as `+` does.
    fn compound(&mut self, op: BinOp, lhs: ValueId, ty: &IrType, rhs: ValueId) -> ValueId {
        let result = self.builder.fresh_value();
        if *ty == IrType::Str && op == BinOp::Add {
            self.builder
                .push_instr(IrInstr::StrConcat { result, lhs, rhs }, Some(IrType::Str));
        } else {
            self.builder.push_instr(
                IrInstr::BinOp {
                    result,
                    op,
                    lhs,
                    rhs,
                    ty: ty.clone(),
                },
                Some(ty.clone()),
            );
        }
        result
    }

    /// Lowers short-circuit `&&` / `||` to SSA control flow.
    ///
    /// `a && b`:
    ///   eval a → cond
    ///   CondBr cond → rhs_bb, merge_bb(false)
    ///   rhs_bb: eval b → rhs_val, Br merge_bb(rhs_val)
    ///   merge_bb(result: bool): …
    ///
    /// `a || b`:
    ///   eval a → cond
    ///   CondBr cond → merge_bb(true), rhs_bb
    ///   rhs_bb: eval b → rhs_val, Br merge_bb(rhs_val)
    ///   merge_bb(result: bool): …
    fn lower_short_circuit(
        &mut self,
        op: AstBinOp,
        lhs: &AstExpr,
        rhs: &AstExpr,
        _span: Span,
    ) -> Result<(ValueId, IrType), LowerError> {
        let bool_ty = IrType::Scalar(DType::Bool);

        // 1. Evaluate LHS.
        let (lhs_val, _) = self.lower_expr(lhs)?;

        // 2. Create blocks.
        let rhs_bb = self.builder.create_block(Some("sc_rhs"));
        let merge_bb = self.builder.create_block(Some("sc_merge"));

        // 3. Emit the short-circuit constant for the skipped case.
        let short_val = self.builder.fresh_value();
        let short_bool = matches!(op, AstBinOp::Or); // ||: true, &&: false
        self.builder.push_instr(
            IrInstr::ConstBool {
                result: short_val,
                value: short_bool,
            },
            Some(bool_ty.clone()),
        );

        // 4. Emit CondBr.
        match op {
            AstBinOp::And => {
                // If LHS is true, eval RHS; if false, short-circuit to merge with false.
                self.builder.push_instr(
                    IrInstr::CondBr {
                        cond: lhs_val,
                        then_block: rhs_bb,
                        then_args: vec![],
                        else_block: merge_bb,
                        else_args: vec![short_val],
                    },
                    None,
                );
            }
            AstBinOp::Or => {
                // If LHS is true, short-circuit to merge with true; else eval RHS.
                self.builder.push_instr(
                    IrInstr::CondBr {
                        cond: lhs_val,
                        then_block: merge_bb,
                        then_args: vec![short_val],
                        else_block: rhs_bb,
                        else_args: vec![],
                    },
                    None,
                );
            }
            _ => unreachable!(),
        }

        // 5. RHS block: evaluate rhs, branch to merge.
        self.builder.set_current_block(rhs_bb);
        let (rhs_val, _) = self.lower_expr(rhs)?;
        if !self.builder.is_current_block_terminated() {
            self.builder.push_instr(
                IrInstr::Br {
                    target: merge_bb,
                    args: vec![rhs_val],
                },
                None,
            );
        }

        // 6. Merge block with block parameter carrying the result.
        let result = self
            .builder
            .add_block_param(merge_bb, Some("sc_result"), bool_ty.clone());
        self.builder.set_current_block(merge_bb);

        Ok((result, bool_ty))
    }

    fn lower_block(&mut self, block: &AstBlock) -> Result<Option<(ValueId, IrType)>, LowerError> {
        if block.stmts.is_empty() && block.tail.is_none() {
            let marker = synth_marker_at(block.span);
            let tb = match marker {
                Some(SynthMarker::Block(tb)) => Some(tb),
                Some(SynthMarker::Expr(_)) => None,
                None => crate::thir::block_at(block.span),
            };
            if let Some(tb) = tb {
                if tb.tail.is_some() {
                    return Ok(Some(self.lower_thir_block(&tb)?));
                }
                self.lower_thir_block(&tb)?;
                return Ok(None);
            }
        }
        self.lower_block_stmts(block)?;
        if let Some(tail) = &block.tail {
            self.builder.set_line_span(tail.span().start.0);
            if self.builder.is_current_block_terminated() {
                // Block was terminated early (e.g. break in body) — skip tail.
                Ok(None)
            } else {
                Ok(Some(self.lower_expr(tail)?))
            }
        } else {
            Ok(None)
        }
    }

    /// Lowers just the statements of a block (no tail expression).
    fn lower_block_stmts(&mut self, block: &AstBlock) -> Result<(), LowerError> {
        if block.stmts.is_empty() && block.tail.is_none() {
            // Synthetic marker (see lower_expr): statements live in THIR.
            if let Some(SynthMarker::Block(tb)) = synth_marker_at(block.span) {
                self.lower_thir_block(&tb)?;
                return Ok(());
            }
            if let Some(tb) = crate::thir::block_at(block.span) {
                self.lower_thir_block(&tb)?;
                return Ok(());
            }
        }
        for stmt in &block.stmts {
            if self.builder.is_current_block_terminated() {
                break;
            }
            self.lower_stmt(stmt)?;
        }
        Ok(())
    }

    /// The name of program state variable `name`, as the builtins' operand.
    fn state_name(&mut self, name: &str) -> ValueId {
        let operand = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::ConstStr {
                result: operand,
                value: name.to_owned(),
            },
            Some(IrType::Str),
        );
        operand
    }

    /// Read program state variable `name`.
    fn state_get(&mut self, name: &str, ty: IrType) -> (ValueId, IrType) {
        let operand = self.state_name(name);
        let result = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BuiltinCall {
                result,
                name: "state_get".to_owned(),
                args: vec![operand],
                result_ty: ty.clone(),
            },
            Some(ty.clone()),
        );
        (result, ty)
    }

    /// Write program state variable `name`.
    fn state_set(&mut self, name: &str, value: ValueId) {
        let operand = self.state_name(name);
        let i64_ty = IrType::Scalar(DType::I64);
        let result = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::BuiltinCall {
                result,
                name: "state_set".to_owned(),
                args: vec![operand, value],
                result_ty: i64_ty.clone(),
            },
            Some(i64_ty),
        );
    }

    fn lower_stmt(&mut self, stmt: &AstStmt) -> Result<(), LowerError> {
        // Record source position for the debugger span table.
        let span_byte = match stmt {
            AstStmt::Let { span, .. } => Some(span.start.0),
            AstStmt::Expr(expr) => Some(expr.span().start.0),
            AstStmt::While { span, .. } => Some(span.start.0),
            AstStmt::Loop { span, .. } => Some(span.start.0),
            AstStmt::Break { span, .. } => Some(span.start.0),
            AstStmt::Continue { span, .. } => Some(span.start.0),
            AstStmt::ForRange { span, .. } => Some(span.start.0),
            AstStmt::Assign { span, .. } => Some(span.start.0),
            AstStmt::LetTuple { span, .. } => Some(span.start.0),
            AstStmt::Return { span, .. } => Some(span.start.0),
            AstStmt::Spawn { span, .. } => Some(span.start.0),
            AstStmt::ParFor { span, .. } => Some(span.start.0),
            AstStmt::ForEach { span, .. } => Some(span.start.0),
            AstStmt::MaskStmt { span, .. } => Some(span.start.0),
            AstStmt::HandleStmt { span, .. } => Some(span.start.0),
            AstStmt::Defer { span, .. } => Some(span.start.0),
            AstStmt::Select { span, .. } => Some(span.start.0),
            AstStmt::Yield { span, .. } => Some(span.start.0),
        };
        if let Some(byte) = span_byte {
            self.builder.set_span_byte(byte);
        }
        match stmt {
            AstStmt::Let {
                name,
                ty: ann_ty,
                init,
                ..
            } => {
                // Set binding_ty from the annotation so constructors like list() can
                // infer their element type (e.g. `val xs: list<f64> = list()`).
                if let Some(ast_ty) = ann_ty {
                    let ann = self.resolve_ty(ast_ty)?;
                    self.binding_ty = Some(ann.clone());
                    // Set expected_expr_ty so lower_if_expr can coerce branches
                    // before the merge (avoids branch-type mismatch).
                    self.expected_expr_ty = Some(ann);
                }
                let (mut val, mut ty) = self.lower_expr(init)?;
                self.binding_ty = None;
                self.expected_expr_ty = None;

                // Cohersion: if the binding is annotated `dyn Trait` and the
                // initializer has a concrete struct type, materialize a
                // trait-object fat pointer via MakeTraitObject.
                if let Some(ast_ty_for_box) = ann_ty {
                    let ann = self.resolve_ty(ast_ty_for_box)?;
                    let (nv, nt) =
                        self.coerce_to_trait_object(val, ty.clone(), &ann, ast_ty_for_box.span())?;
                    val = nv;
                    ty = nt;
                }

                // The annotation completes what the initialiser leaves open:

                if let Some(drop_fn) = self.find_destructor(&ty) {
                    self.register_drop(name.name.clone(), val, drop_fn, ty.clone());
                }

                if !name.name.starts_with('_') {
                    self.builder.name_value(val, &name.name);
                }
                self.bind_scope_var(name.name.clone(), val, ty);
                Ok(())
            }
            AstStmt::LetTuple {
                names, init, span, ..
            } => {
                // THIR desugared this statement into a temporary plus one
                // component per bound name. Use that shape. A missing entry is
                // an internal error, not a reason to re-derive the statement
                // from the AST: the AST walk is no longer authoritative here.
                let info = crate::thir::let_tuple(*span).ok_or_else(|| {
                    crate::thir::incomplete(vec![format!(
                        "lowering needs THIR for the tuple destructuring at byte {}",
                        span.start.0
                    )])
                })?;
                if info.components.len() != names.len() {
                    return Err(crate::thir::incomplete(vec![format!(
                        "tuple destructuring at byte {} has {} THIR component(s) for {} name(s)",
                        span.start.0,
                        info.components.len(),
                        names.len()
                    )]));
                }
                let (tuple_val, tuple_ty) = self.lower_expr(init)?;
                let elem_types = match &tuple_ty {
                    IrType::Tuple(elems) => elems.clone(),
                    IrType::Struct { fields, .. } => {
                        // Struct destructuring by position: (a, b) = struct_val
                        if names.len() != fields.len() {
                            return Err(LowerError::rich(
                                "E0125",
                                crate::diagnostics::Diagnostic::error(format!(
                                    "the record has {} fields but {} names are bound",
                                    fields.len(),
                                    names.len()
                                ))
                                .with_primary_span(
                                    span.start.0,
                                    span.end.0,
                                    None,
                                ),
                            ));
                        }
                        for (i, name) in names.iter().enumerate() {
                            // THIR's layout index, when it still names the
                            // same field. Open/generic bases fall back to the
                            // position lowering's type proves.
                            let field_index = match info.components.get(i) {
                                Some(c)
                                    if c.kind == crate::thir::ThirDestructureKind::Field
                                        && c.index < fields.len()
                                        && c.field
                                            .as_deref()
                                            .map(|f| f == fields[c.index].0.as_str())
                                            .unwrap_or(true) =>
                                {
                                    c.index
                                }
                                _ => i,
                            };
                            let elem_ty = fields[field_index].1.clone();
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::GetField {
                                    result,
                                    base: tuple_val,
                                    field_index,
                                    result_ty: elem_ty.clone(),
                                },
                                Some(elem_ty.clone()),
                            );
                            self.bind_scope_var(name.name.clone(), result, elem_ty);
                        }
                        return Ok(());
                    }
                    _ => {
                        return Err(LowerError::rich(
                            "E0124",
                            crate::diagnostics::Diagnostic::error(format!(
                                "destructuring needs a tuple, but this is `{}`",
                                hints::type_name(&tuple_ty)
                            ))
                            .with_primary_span(
                                span.start.0,
                                span.end.0,
                                None,
                            ),
                        ));
                    }
                };
                if names.len() != elem_types.len() {
                    return Err(LowerError::rich(
                        "E0125",
                        crate::diagnostics::Diagnostic::error(format!(
                            "the tuple has {} elements but {} names are bound",
                            elem_types.len(),
                            names.len()
                        ))
                        .with_primary_span(span.start.0, span.end.0, None),
                    ));
                }
                for (i, name) in names.iter().enumerate() {
                    // A known tuple carries its element index in THIR. An
                    // open destructuring keeps the source position.
                    let index = match info.components.get(i) {
                        Some(c)
                            if c.kind == crate::thir::ThirDestructureKind::Tuple
                                && c.index < elem_types.len() =>
                        {
                            c.index
                        }
                        _ => i,
                    };
                    let elem_ty = elem_types[index].clone();
                    let result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::GetElement {
                            result,
                            base: tuple_val,
                            index,
                            result_ty: elem_ty.clone(),
                        },
                        Some(elem_ty.clone()),
                    );
                    self.bind_scope_var(name.name.clone(), result, elem_ty);
                }
                Ok(())
            }
            AstStmt::Expr(expr) => {
                // If the expression is a block, lower it as a statement block (allow no tail).
                // Otherwise, lower it as a normal expression.
                if let AstExpr::Block(block) = expr.as_ref() {
                    self.lower_block(block)?;
                } else {
                    self.lower_expr(expr)?;
                }
                Ok(())
            }
            AstStmt::While {
                cond,
                body,
                span,
                label,
            } => self.lower_while(cond, body, label, *span),
            AstStmt::ForRange {
                var,
                start,
                end,
                body,
                inclusive,
                step,
                label,
                span,
            } => self.lower_for_range(
                var,
                start,
                end,
                body,
                *inclusive,
                step.as_deref(),
                label,
                *span,
            ),
            AstStmt::Loop { body, span, label } => self.lower_loop(body, label, *span),
            AstStmt::Break { label, span, .. } => self.lower_break(label, *span),
            AstStmt::Continue { label, span, .. } => self.lower_continue(label, *span),
            AstStmt::Assign {
                target,
                op,
                value,
                span,
            } => {
                match target.as_ref() {
                    // Assignment to program state (unless a local shadows it).
                    AstExpr::Ident(ident)
                        if !self.scope.contains_key(&ident.name)
                            && self.module.state_slot(&ident.name).is_some() =>
                    {
                        let state_ty = self
                            .module
                            .state_slot(&ident.name)
                            .map(|(_, ty)| ty.clone())
                            .expect("checked by the guard");
                        let (value_val, value_ty) = if let Some(bin_op) = op {
                            let (current, _) = self.state_get(&ident.name, state_ty.clone());
                            let (rhs_val, _) = self.lower_expr(value)?;
                            (self.compound(*bin_op, current, &state_ty, rhs_val), state_ty.clone())
                        } else {
                            // The state's declared type types the value, as an
                            // annotation does (`var history: list<f64> = list()`).
                            let prev_binding_ty = self.binding_ty.replace(state_ty.clone());
                            let lowered = self.lower_expr(value);
                            self.binding_ty = prev_binding_ty;
                            lowered?
                        };
                        if !assignable(&value_ty, &state_ty) {
                            return Err(LowerError::TypeMismatch {
                                expected: format!("{state_ty} (the type of program state '{}')", ident.name),
                                found: value_ty.to_string(),
                                span: *span,
                            });
                        }
                        self.state_set(&ident.name, value_val);
                        Ok(())
                    }
                    // Plain identifier assignment: rebind the name in scope (SSA-style).
                    AstExpr::Ident(ident) => {
                        let new_val = if let Some(bin_op) = op {
                            let (lhs_val, lhs_ty) = self.scope.get(&ident.name).cloned()
                                .ok_or_else(|| LowerError::rich("E0100", crate::diagnostics::Diagnostic::error(format!("cannot find `{}` in this scope", ident.name)).with_primary_span(span.start.0, span.end.0, None)))?;
                            let (rhs_val, _) = self.lower_expr(value)?;
                            (self.compound(*bin_op, lhs_val, &lhs_ty, rhs_val), lhs_ty)
                        } else {
                            self.lower_expr(value)?
                        };
                        self.builder.name_value(new_val.0, &ident.name);
                        self.record_assign_place(&ident.name, None, new_val.0, *span);
                        self.scope.insert(ident.name.clone(), (new_val.0, new_val.1));
                        Ok(())
                    }
                    // Array element store: `arr[i] = value`  or  tensor store
                    AstExpr::Index {
                        base,
                        indices,
                        span,
                    } => {
                        let (base_val, base_ty) = self.lower_expr(base)?;
                        // Arrays, slices and lists (in place), or a tensor.
                        let (single, elem_ty) = match &base_ty {
                            IrType::Array { elem, .. } | IrType::Slice { elem, .. } | IrType::List(elem) => (true, (**elem).clone()),
                            IrType::Tensor { dtype, .. } => (false, IrType::Scalar(*dtype)),
                            other => return Err(not_indexable(other, *span)),
                        };
                        if single && indices.len() != 1 {
                            return Err(LowerError::rich("E0125", crate::diagnostics::Diagnostic::error(format!("`{}` takes exactly 1 index", hints::type_name(&base_ty))).with_primary_span(span.start.0, span.end.0, None)));
                        }
                        let mut idx_vals = Vec::new();
                        for idx in indices {
                            let (iv, _) = self.lower_expr(idx)?;
                            idx_vals.push(iv);
                        }
                        let (mut value_val, _) = self.lower_expr(value)?;
                        // `x[i] op= v`: the element now, combined with `v`.
                        if let Some(bin_op) = op {
                            let current = self.builder.fresh_value();
                            let load = match &base_ty {
                                IrType::Array { .. } | IrType::Slice { .. } => IrInstr::ArrayLoad { result: current, array: base_val, index: idx_vals[0], elem_ty: elem_ty.clone() },
                                IrType::List(_) => IrInstr::ListGet { result: current, list: base_val, index: idx_vals[0], elem_ty: elem_ty.clone() },
                                _ => IrInstr::Load { result: current, tensor: base_val, indices: idx_vals.clone(), result_ty: elem_ty.clone() },
                            };
                            self.builder.push_instr(load, Some(elem_ty.clone()));
                            value_val = self.compound(*bin_op, current, &elem_ty, value_val);
                        }
                        let store = match &base_ty {
                            IrType::Array { .. } | IrType::Slice { .. } => IrInstr::ArrayStore { array: base_val, index: idx_vals[0], value: value_val },
                            IrType::List(_) => IrInstr::ListSet { list: base_val, index: idx_vals[0], value: value_val },
                            _ => IrInstr::Store { tensor: base_val, indices: idx_vals, value: value_val },
                        };
                        self.builder.push_instr(store, None);
                        Ok(())
                    }
                    AstExpr::FieldAccess { base, field, span } => {
                        let value_val = if let Some(bin_op) = op {
                            let (lhs_val, lhs_ty) = self.lower_expr(target)?;
                            let (rhs_val, _) = self.lower_expr(value)?;
                            self.compound(*bin_op, lhs_val, &lhs_ty, rhs_val)
                        } else {
                            let (v, _) = self.lower_expr(value)?;
                            v
                        };
                        self.lower_field_assignment(base, field, value_val, *span)
                    }
                    AstExpr::TupleIndex { base, index, span } => {
                        if let AstExpr::Ident(base_ident) = base.as_ref() {
                            let (base_val, base_ty) = self.lower_expr(base)?;
                            let elem_types = match &base_ty {
                                IrType::Tuple(elems) => elems.clone(),
                                _ => {
                                    return Err(LowerError::rich("E0124", crate::diagnostics::Diagnostic::error(format!("`.N` takes an element of a tuple, but this is `{}`", hints::type_name(&base_ty))).with_primary_span(span.start.0, span.end.0, None)));
                                }
                            };
                            if *index >= elem_types.len() {
                                return Err(LowerError::rich("E0125", crate::diagnostics::Diagnostic::error(format!("tuple index {} is out of range: the tuple has {} elements", index, elem_types.len())).with_primary_span(span.start.0, span.end.0, None)));
                            }
                            let (value_val, _) = self.lower_expr(value)?;

                            let mut new_elements = Vec::with_capacity(elem_types.len());
                            for (i, elem_ty) in elem_types.iter().enumerate() {
                                if i == *index {
                                    new_elements.push(value_val);
                                } else {
                                    let f_ty = elem_ty.clone();
                                    let f_val = self.builder.fresh_value();
                                    self.builder.push_instr(
                                        IrInstr::GetElement {
                                            result: f_val,
                                            base: base_val,
                                            index: i,
                                            result_ty: f_ty.clone(),
                                        },
                                        Some(f_ty.clone()),
                                    );
                                    new_elements.push(f_val);
                                }
                            }
                            let result_ty = base_ty.clone();
                            let result = self.builder.fresh_value();
                            self.builder.push_instr(
                                IrInstr::MakeTuple {
                                    result,
                                    elements: new_elements,
                                    result_ty: result_ty.clone(),
                                },
                                Some(result_ty.clone()),
                            );
                            self.record_assign_place(&base_ident.name, Some(*index), result, *span);
                            self.scope.insert(base_ident.name.clone(), (result, result_ty));
                            Ok(())
                        } else {
                            Err(LowerError::rich("E0128", crate::diagnostics::Diagnostic::error("cannot assign to this tuple element: the tuple must be a variable".to_string()).with_primary_span(span.start.0, span.end.0, None)))
                        }
                    }
                    _ => Err(LowerError::rich("E0128", crate::diagnostics::Diagnostic::error("cannot assign to this: the left of `=` must be a variable, an index, a field or a tuple element".to_string()).with_primary_span(span.start.0, span.end.0, None))),
                }
            }
            AstStmt::Return { value, span, .. } => {
                let ret_values = if let Some(expr) = value {
                    // Seed expected_expr_ty so that if/else tail expressions in
                    // the return value get coerced before branch merge.
                    if self.expected_expr_ty.is_none() {
                        self.expected_expr_ty = self.current_return_ty.clone();
                    }
                    let (val, ty) = self.lower_expr(expr)?;
                    self.expected_expr_ty = None;
                    // If the function returns `dyn Trait` and we're returning a
                    // concrete struct, coerce it via MakeTraitObject.
                    let (val, _ty) = if let Some(expected) = self.current_return_ty.clone() {
                        self.coerce_to_trait_object(val, ty, &expected, *span)?
                    } else {
                        (val, ty)
                    };
                    vec![val]
                } else {
                    vec![]
                };
                // Emit active destructors (Drop trait) in reverse order (LIFO) before return.
                self.emit_drops(&ret_values);
                // Emit deferred expressions in reverse order (LIFO) before return.
                self.emit_defers();
                self.builder
                    .push_instr(IrInstr::Return { values: ret_values }, None);
                Ok(())
            }

            AstStmt::Spawn { body, span, group } => {
                // Lambda-lift the spawn body into a function __spawn_N().
                let counter = self.lambda_counter.get();
                self.lambda_counter.set(counter + 1);
                let fn_name = format!("__spawn_{}", counter);

                // Seed the lifted scope with all visible bindings, then remove
                // unused parameters after lowering. Inspecting IR operands also
                // handles indexed/named arguments, shadowing, and nested tasks
                // without a second, potentially incomplete AST name walker.
                let mut captures: Vec<(String, ValueId, IrType)> = self
                    .scope
                    .iter()
                    .map(|(name, (vid, ty))| (name.clone(), *vid, ty.clone()))
                    .collect();
                captures.sort_by(|a, b| a.0.cmp(&b.0));

                let lifted_params: Vec<crate::ir::function::Param> = captures
                    .iter()
                    .map(|(name, _, ty)| crate::ir::function::Param {
                        name: name.clone(),
                        ty: ty.clone(),
                    })
                    .collect();

                // Build the lifted function with a synthetic AstBlock.
                let ast_block = AstBlock {
                    stmts: body.clone(),
                    tail: None,
                    span: *span,
                };
                let temp_builder = IrFunctionBuilder::new(
                    &fn_name,
                    lifted_params.clone(),
                    IrType::Scalar(DType::I64),
                );
                let mut spawn_lowerer = self.new_sub_lowerer(temp_builder);
                let entry = spawn_lowerer.builder.create_block(Some("entry"));
                spawn_lowerer.builder.set_current_block(entry);
                // Track outer_val → inner_val mapping to propagate chan_elem_types back.
                let mut capture_val_map: Vec<(ValueId, ValueId)> = Vec::new();
                for (name, outer_val, ty) in &captures {
                    let inner_val =
                        spawn_lowerer
                            .builder
                            .add_block_param(entry, Some(name), ty.clone());
                    spawn_lowerer
                        .scope
                        .insert(name.clone(), (inner_val, ty.clone()));
                    capture_val_map.push((*outer_val, inner_val));
                }
                // Pre-populate spawn_lowerer's chan_elem_types from parent (inner val → elem ty).
                for (outer_val, inner_val) in &capture_val_map {
                    if let Some(elem_ty) = self.chan_elem_types.get(outer_val) {
                        spawn_lowerer
                            .chan_elem_types
                            .insert(*inner_val, elem_ty.clone());
                    }
                }
                spawn_lowerer.lower_block(&ast_block)?;
                // Propagate any new chan_elem_types discovered in spawn back to parent.
                for (outer_val, inner_val) in &capture_val_map {
                    if let Some(elem_ty) = spawn_lowerer.chan_elem_types.get(inner_val) {
                        self.chan_elem_types
                            .entry(*outer_val)
                            .or_insert_with(|| elem_ty.clone());
                    }
                }
                // Emit a return of 0 if not already terminated.
                let dummy_ret = spawn_lowerer.builder.fresh_value();
                spawn_lowerer.builder.push_instr(
                    IrInstr::ConstInt {
                        result: dummy_ret,
                        value: 0,
                        ty: IrType::Scalar(DType::I64),
                    },
                    Some(IrType::Scalar(DType::I64)),
                );
                spawn_lowerer.builder.push_instr(
                    IrInstr::Return {
                        values: vec![dummy_ret],
                    },
                    None,
                );
                spawn_lowerer.builder.seal_unterminated_blocks();
                let mut ir_func = spawn_lowerer.builder.build();
                let used: HashSet<ValueId> = ir_func
                    .blocks
                    .iter()
                    .flat_map(|block| block.all_operands())
                    .collect();
                let keep: Vec<bool> = ir_func.blocks[0]
                    .params
                    .iter()
                    .map(|param| used.contains(&param.id))
                    .collect();
                let removed: HashSet<ValueId> = ir_func.blocks[0]
                    .params
                    .iter()
                    .filter(|param| !used.contains(&param.id))
                    .map(|param| param.id)
                    .collect();
                ir_func.params = ir_func
                    .params
                    .into_iter()
                    .zip(&keep)
                    .filter_map(|(param, keep)| keep.then_some(param))
                    .collect();
                captures = captures
                    .into_iter()
                    .zip(&keep)
                    .filter_map(|(capture, keep)| keep.then_some(capture))
                    .collect();
                ir_func.blocks[0]
                    .params
                    .retain(|param| used.contains(&param.id));
                ir_func.value_defs.retain(|id, _| !removed.contains(id));
                ir_func.value_types.retain(|id, _| !removed.contains(id));
                self.lifted_fns.borrow_mut().push(ir_func);

                let capture_vals: Vec<ValueId> = captures.iter().map(|(_, v, _)| *v).collect();
                if let Some(group_expr) = group {
                    let (group_val, _) = self.lower_expr(group_expr)?;
                    self.builder.push_instr(
                        IrInstr::TaskGroupSpawn {
                            group: group_val,
                            body_fn: fn_name,
                            args: capture_vals,
                        },
                        None,
                    );
                } else {
                    self.builder.push_instr(
                        IrInstr::Spawn {
                            body_fn: fn_name,
                            args: capture_vals,
                        },
                        None,
                    );
                }
                let _ = span;
                Ok(())
            }

            AstStmt::ForEach {
                var,
                iter,
                body,
                span,
                ..
            } => self.lower_foreach(var, iter, body, *span),

            AstStmt::ParFor {
                var,
                start,
                end,
                body,
                inclusive,
                span,
                ..
            } => {
                // Lambda-lift body into __par_body_N(var: i64, captures...) { body }.
                let counter = self.lambda_counter.get();
                self.lambda_counter.set(counter + 1);
                let fn_name = format!("__par_body_{}", counter);

                // Collect outer-scope captures (all in-scope variables except
                // the loop var), sorted by name so the generated
                // `__par_body_N` signature does not depend on `HashMap`
                // iteration order. See the note in `lower_lambda`.
                let mut captures: Vec<(String, ValueId, IrType)> = self
                    .scope
                    .iter()
                    .filter(|(name, _)| *name != &var.name)
                    .map(|(name, (vid, ty))| (name.clone(), *vid, ty.clone()))
                    .collect();
                captures.sort_by(|a, b| a.0.cmp(&b.0));
                let captures = captures;

                // Reject mutation of a captured collection from the loop body.
                //
                // `par for` runs its body on several OS threads. A captured
                // `list` or `map` mutated from inside is a data race: the
                // runtime now locks the collection primitives, so it will not
                // corrupt memory, but the *result* is still order-dependent and
                // the program is not deterministic. Locking makes it survivable;
                // it does not make it correct.
                //
                // A language whose selling point is a statically verifiable
                // autonomy layer should not admit a race through the front door,
                // and the captures are already enumerated here, so the check
                // costs nothing.
                //
                // Limit, stated plainly: this catches a mutating builtin applied
                // directly to a captured name. Mutation reached through a
                // user-defined function is not detected — that needs
                // interprocedural analysis. False negatives, never false
                // positives.
                {
                    let captured_names: std::collections::HashSet<&str> =
                        captures.iter().map(|(n, _, _)| n.as_str()).collect();
                    let mut offender: Option<(String, String)> = None;
                    find_captured_mutation(body, &captured_names, &mut offender);
                    if let Some((coll, op)) = offender {
                        return Err(LowerError::Rejected {
                            detail: format!("`par for` body mutates the captured collection `{}` via `{}`.\nIterations run concurrently, so the result depends on thread scheduling.\nUse `atomic` for a shared counter, or build a per-iteration value and combine after the loop.", coll, op),
                            span: *span,
                        });
                    }
                }

                // Build params: loop var first, then captures.
                let mut params = vec![crate::ir::function::Param {
                    name: var.name.clone(),
                    ty: IrType::Scalar(DType::I64),
                }];
                for (name, _, ty) in &captures {
                    params.push(crate::ir::function::Param {
                        name: name.clone(),
                        ty: ty.clone(),
                    });
                }

                let temp_builder =
                    IrFunctionBuilder::new(&fn_name, params, IrType::Scalar(DType::I64));
                let mut body_lowerer = self.new_sub_lowerer(temp_builder);
                let entry = body_lowerer.builder.create_block(Some("entry"));
                body_lowerer.builder.set_current_block(entry);
                // Add loop var as first block param.
                let var_val = body_lowerer.builder.add_block_param(
                    entry,
                    Some(&var.name),
                    IrType::Scalar(DType::I64),
                );
                body_lowerer
                    .scope
                    .insert(var.name.clone(), (var_val, IrType::Scalar(DType::I64)));
                // Add capture params.
                for (name, _, ty) in &captures {
                    let inner_val =
                        body_lowerer
                            .builder
                            .add_block_param(entry, Some(name), ty.clone());
                    body_lowerer
                        .scope
                        .insert(name.clone(), (inner_val, ty.clone()));
                }
                body_lowerer.lower_block(body)?;
                let dummy_ret = body_lowerer.builder.fresh_value();
                body_lowerer.builder.push_instr(
                    IrInstr::ConstInt {
                        result: dummy_ret,
                        value: 0,
                        ty: IrType::Scalar(DType::I64),
                    },
                    Some(IrType::Scalar(DType::I64)),
                );
                body_lowerer.builder.push_instr(
                    IrInstr::Return {
                        values: vec![dummy_ret],
                    },
                    None,
                );
                body_lowerer.builder.seal_unterminated_blocks();
                let ir_func = body_lowerer.builder.build();
                self.lifted_fns.borrow_mut().push(ir_func);

                let (start_val, _) = self.lower_expr(start)?;
                let (end_val, _) = self.lower_expr(end)?;
                let var_id = self.builder.fresh_value();
                let capture_vals: Vec<ValueId> = captures.iter().map(|(_, v, _)| *v).collect();
                // The loop's own position, for an error in starting it (a GPU
                // launch): the statement's went to its first instruction.
                self.builder.set_span_byte(span.start.0);
                self.builder.push_instr(
                    IrInstr::ParFor {
                        var: var_id,
                        start: start_val,
                        end: end_val,
                        inclusive: *inclusive,
                        body_fn: fn_name,
                        args: capture_vals,
                    },
                    None,
                );
                Ok(())
            }
            AstStmt::MaskStmt { body, .. } => {
                self.lower_block(body)?;
                Ok(())
            }
            AstStmt::HandleStmt {
                expr,
                arms,
                return_ty,
                ..
            } => {
                // Lower each handler arm into an IR function + HandlerArm descriptor.
                let mut handler_arms = Vec::new();
                for arm in arms {
                    let ha = self.lower_handler_arm(arm)?;
                    handler_arms.push(ha);
                }

                // Emit PushHandler.
                self.builder
                    .push_instr(IrInstr::PushHandler { arms: handler_arms }, None);

                // Lower body.
                self.lower_expr(expr)?;

                // Emit PopHandler.
                self.builder.push_instr(IrInstr::PopHandler, None);

                let _ = return_ty;
                Ok(())
            }
            AstStmt::Defer { expr, .. } => {
                self.defer_stack.push(expr.as_ref().clone());
                Ok(())
            }
            AstStmt::Yield { expr, .. } => {
                self.lower_expr(expr)?;
                Ok(())
            }
            AstStmt::Select { arms, default, .. } => {
                let outer_scope = self.scope.clone();

                // Collect rebound variable names from all arm bodies and default body.
                let mut rebound_names: Vec<String> = Vec::new();
                for arm in arms {
                    collect_rebound_vars_in_block(&arm.body, &mut rebound_names, false);
                }
                if let Some(d) = default {
                    collect_rebound_vars_in_block(d, &mut rebound_names, false);
                }
                rebound_names.retain(|name| outer_scope.contains_key(name));

                let loop_bb = self.builder.create_block(Some("select_loop"));
                let merge_bb = self.builder.create_block(Some("select_merge"));

                // Add block params to merge_bb for rebound variables (phi merge).
                let mut rebound_params: Vec<(String, ValueId, IrType)> = Vec::new();
                for name in &rebound_names {
                    let Some((_, ty)) = outer_scope.get(name) else {
                        continue;
                    };
                    let param = self
                        .builder
                        .add_block_param(merge_bb, Some(name), ty.clone());
                    rebound_params.push((name.clone(), param, ty.clone()));
                }

                // Branch to the loop
                self.builder.push_instr(
                    IrInstr::Br {
                        target: loop_bb,
                        args: vec![],
                    },
                    None,
                );

                // -- loop_bb: call select(ch0, ch1, ...)
                self.builder.set_current_block(loop_bb);
                let channels: Vec<(ValueId, IrType)> = arms
                    .iter()
                    .map(|arm| {
                        let (value, ty) = self.lower_expr(&arm.channel)?;
                        Ok((value, ty))
                    })
                    .collect::<Result<Vec<_>, LowerError>>()?;
                let channel_values: Vec<ValueId> =
                    channels.iter().map(|(value, _)| *value).collect();

                // Emit select(ch0, ch1, ...) builtin call
                let select_result = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::BuiltinCall {
                        result: select_result,
                        name: "select".to_string(),
                        args: channel_values,
                        result_ty: IrType::Scalar(DType::I64),
                    },
                    Some(IrType::Scalar(DType::I64)),
                );

                // For each arm: check if idx == i, if so recv and execute
                let next_check_bb = self.builder.create_block(Some("select_no_match"));
                for (i, arm) in arms.iter().enumerate() {
                    let idx_val = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::ConstInt {
                            result: idx_val,
                            value: i as i64,
                            ty: IrType::Scalar(DType::I64),
                        },
                        Some(IrType::Scalar(DType::I64)),
                    );
                    let cmp_val = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::BinOp {
                            result: cmp_val,
                            op: BinOp::CmpEq,
                            lhs: select_result,
                            rhs: idx_val,
                            ty: IrType::Scalar(DType::Bool),
                        },
                        Some(IrType::Scalar(DType::Bool)),
                    );
                    let arm_body_bb = self
                        .builder
                        .create_block(Some(&format!("select_arm_{}", i)));
                    let fall_bb = if i < arms.len() - 1 {
                        self.builder
                            .create_block(Some(&format!("select_check_{}", i + 1)))
                    } else {
                        next_check_bb
                    };
                    self.builder.push_instr(
                        IrInstr::CondBr {
                            cond: cmp_val,
                            then_block: arm_body_bb,
                            else_block: fall_bb,
                            then_args: vec![],
                            else_args: vec![],
                        },
                        None,
                    );

                    // arm_body_bb: recv from channel and execute body
                    self.builder.set_current_block(arm_body_bb);
                    self.scope = outer_scope.clone();
                    let (channel, channel_ty) = &channels[i];
                    let elem_ty = match self.chan_elem_types.get(channel) {
                        Some(ty) => ty.clone(),
                        None => match channel_ty {
                            IrType::Chan(elem) => (**elem).clone(),
                            _ => {
                                return Err(self
                                    .undetermined(arm.channel.span(), "what this channel carries"))
                            }
                        },
                    };
                    let recv_result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::ChanRecv {
                            result: recv_result,
                            chan: *channel,
                            elem_ty: elem_ty.clone(),
                        },
                        Some(elem_ty.clone()),
                    );
                    // Bind the received value
                    self.scope
                        .insert(arm.binding.clone(), (recv_result, elem_ty));
                    self.lower_block_stmts(&arm.body)?;
                    if !self.builder.is_current_block_terminated() {
                        let mut merge_args: Vec<ValueId> = Vec::new();
                        for name in &rebound_names {
                            let val = self
                                .scope
                                .get(name)
                                .or_else(|| outer_scope.get(name))
                                .map(|(v, _)| *v)
                                .expect("rebound variable missing from select arm scope");
                            merge_args.push(val);
                        }
                        self.builder.push_instr(
                            IrInstr::Br {
                                target: merge_bb,
                                args: merge_args,
                            },
                            None,
                        );
                    }

                    // Set next check block
                    if i < arms.len() - 1 {
                        self.builder.set_current_block(fall_bb);
                    }
                }

                // next_check_bb: no arm matched — check default
                self.builder.set_current_block(next_check_bb);
                self.scope = outer_scope.clone();
                if let Some(default_block) = default {
                    self.lower_block_stmts(default_block)?;
                    if !self.builder.is_current_block_terminated() {
                        let mut merge_args: Vec<ValueId> = Vec::new();
                        for name in &rebound_names {
                            let val = self
                                .scope
                                .get(name)
                                .or_else(|| outer_scope.get(name))
                                .map(|(v, _)| *v)
                                .expect("rebound variable missing from select default scope");
                            merge_args.push(val);
                        }
                        self.builder.push_instr(
                            IrInstr::Br {
                                target: merge_bb,
                                args: merge_args,
                            },
                            None,
                        );
                    }
                } else {
                    // No default: sleep(1) and loop back
                    let ms_val = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::ConstInt {
                            result: ms_val,
                            value: 1,
                            ty: IrType::Scalar(DType::I64),
                        },
                        Some(IrType::Scalar(DType::I64)),
                    );
                    let sleep_result = self.builder.fresh_value();
                    self.builder.push_instr(
                        IrInstr::SleepMs {
                            result: sleep_result,
                            ms: ms_val,
                        },
                        Some(IrType::Scalar(DType::I64)),
                    );
                    self.builder.push_instr(
                        IrInstr::Br {
                            target: loop_bb,
                            args: vec![],
                        },
                        None,
                    );
                }

                // merge_bb: insert block params into scope for rebound variables
                self.scope = outer_scope;
                for (name, param, ty) in rebound_params {
                    self.scope.insert(name, (param, ty));
                }
                self.builder.set_current_block(merge_bb);
                Ok(())
            }
        }
    }

    fn lower_field_assignment(
        &mut self,
        base: &AstExpr,
        field: &str,
        value_val: ValueId,
        span: Span,
    ) -> Result<(), LowerError> {
        let (base_val, base_ty) = self.lower_expr(base)?;
        let struct_fields = match &base_ty {
            IrType::Struct { fields, .. } => fields.clone(),
            _ => {
                return Err(LowerError::rich(
                    "E0124",
                    crate::diagnostics::Diagnostic::error(format!(
                        "cannot assign a field of `{}`: only records have fields",
                        hints::type_name(&base_ty)
                    ))
                    .with_primary_span((span).start.0, (span).end.0, None),
                ));
            }
        };
        let field_index = struct_fields
            .iter()
            .position(|(n, _)| n == field)
            .ok_or_else(|| {
                LowerError::rich(
                    "E0113",
                    crate::diagnostics::Diagnostic::error(format!(
                        "this record has no field `{}`",
                        field
                    ))
                    .with_primary_span((span).start.0, (span).end.0, None),
                )
            })?;

        let mut new_fields = Vec::with_capacity(struct_fields.len());
        for (i, field_info) in struct_fields.iter().enumerate() {
            if i == field_index {
                new_fields.push(value_val);
            } else {
                let f_ty = field_info.1.clone();
                let f_val = self.builder.fresh_value();
                self.builder.push_instr(
                    IrInstr::GetField {
                        result: f_val,
                        base: base_val,
                        field_index: i,
                        result_ty: f_ty.clone(),
                    },
                    Some(f_ty.clone()),
                );
                new_fields.push(f_val);
            }
        }
        let result_ty = base_ty.clone();
        let result = self.builder.fresh_value();
        self.builder.push_instr(
            IrInstr::MakeStruct {
                result,
                fields: new_fields,
                result_ty: result_ty.clone(),
            },
            Some(result_ty.clone()),
        );

        match base {
            // `state.field = value` on program state stores the updated record.
            AstExpr::Ident(base_ident)
                if !self.scope.contains_key(&base_ident.name)
                    && self.module.state_slot(&base_ident.name).is_some() =>
            {
                self.state_set(&base_ident.name, result);
                Ok(())
            }
            AstExpr::Ident(base_ident) => {
                self.record_assign_place(&base_ident.name, Some(field_index), result, span);
                self.scope
                    .insert(base_ident.name.clone(), (result, result_ty));
                Ok(())
            }
            AstExpr::FieldAccess {
                base: parent_base,
                field: parent_field,
                span: parent_span,
            } => self.lower_field_assignment(parent_base, parent_field, result, *parent_span),
            _ => Err(LowerError::rich(
                "E0128",
                crate::diagnostics::Diagnostic::error(
                    "cannot assign to this field: it must belong to a variable".to_string(),
                )
                .with_primary_span((span).start.0, (span).end.0, None),
            )),
        }
    }
}

/// Register the program's state variables and generate
/// `def __iris_state_init() -> i64 { name = initial; ...; 0 }`, which gives
/// each its initial value in declaration order.
fn register_program_state(
    ast: &AstModule,
    module: &mut IrModule,
) -> Result<Option<crate::parser::ast::AstFunction>, LowerError> {
    if ast.state_vars.is_empty() {
        return Ok(None);
    }
    for var in &ast.state_vars {
        if module.state_slot(&var.name.name).is_some() {
            return Err(LowerError::DuplicateFunction {
                name: var.name.name.clone(),
                span: var.name.span,
            });
        }
        let ty = lower_type_with_structs(&var.ty, module)?;
        module.state_vars.push((var.name.name.clone(), ty));
    }
    let template = format!("def {}() -> i64 {{ 0 }}", crate::ir::module::STATE_INIT_FN);
    let (mut parsed, errors) = crate::compile_with_recovery(&template);
    if !errors.is_empty() || parsed.functions.is_empty() {
        return Err(LowerError::rich(
            "E0900",
            crate::diagnostics::Diagnostic::error(
                "internal compiler error: the program state initializer did not parse".to_string(),
            )
            .with_primary_span(
                (ast.state_vars[0].span).start.0,
                (ast.state_vars[0].span).end.0,
                None,
            ),
        ));
    }
    let mut function = parsed.functions.remove(0);
    function.body.stmts = ast
        .state_vars
        .iter()
        .map(|var| AstStmt::Assign {
            target: Box::new(AstExpr::Ident(var.name.clone())),
            op: None,
            value: Box::new(var.init.clone()),
            span: var.span,
        })
        .collect();
    Ok(Some(function))
}

/// Name prefix of the poll function generated for each `async def`.
pub(crate) const ASYNC_POLL_PREFIX: &str = "__poll_";
/// Attribute marking an `async def` body being lowered as a poll function.
pub(crate) const ASYNC_POLL_ATTR: &str = "__async_poll";
/// The poll function's single parameter: the running task.
pub(super) const ASYNC_TASK_PARAM: &str = "__task";
