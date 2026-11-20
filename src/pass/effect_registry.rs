// Effect registry — maps builtin function names to their effect rows.
//
// Used by the effect checker to look up the effects of primitive / stdlib
// calls when inferring effect rows for user functions.

use std::collections::HashMap;

/// A row of effect tags (e.g. ["io", "alloc"]). Sorted and deduplicated.
/// Supports effect row variables for row polymorphism (e.g. `effect E`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct EffectRow {
    pub effects: Vec<String>,
    /// Effect row variables (e.g. `effect E` in a function signature).
    /// At call sites, these are instantiated with the caller's effect row.
    pub vars: Vec<String>,
}

impl EffectRow {
    pub fn pure() -> Self {
        Self {
            effects: vec![],
            vars: vec![],
        }
    }

    pub fn new(effects: Vec<String>) -> Self {
        let mut s = effects;
        // `pure` names the empty row; it is never an effect a body performs.
        // Keeping it as a tag made `effect pure` leak into callers' rows.
        s.retain(|e| e != "pure");
        s.sort();
        s.dedup();
        Self {
            effects: s,
            vars: vec![],
        }
    }

    pub fn from_strs(effects: &[&str]) -> Self {
        Self::new(effects.iter().map(|s| s.to_string()).collect())
    }

    /// Build from effect list + effect variable names.
    pub fn from_parts(effects: Vec<String>, vars: Vec<String>) -> Self {
        let mut s = effects;
        s.retain(|e| e != "pure");
        s.sort();
        s.dedup();
        Self { effects: s, vars }
    }

    pub fn is_pure(&self) -> bool {
        self.effects.is_empty() && self.vars.is_empty()
    }

    pub fn contains(&self, e: &str) -> bool {
        self.effects.iter().any(|x| x == e)
    }

    /// Returns true if every effect in `self` is also in `other` (subset relation).
    /// Effect variables are treated as always-satisfied (they'll be instantiated).
    pub fn subset(&self, other: &Self) -> bool {
        self.effects.iter().all(|e| other.effects.contains(e))
    }

    /// Set union of two effect rows.
    pub fn union(&self, other: &Self) -> Self {
        let mut combined = self.effects.clone();
        combined.extend(other.effects.clone());
        let mut vcombined = self.vars.clone();
        vcombined.extend(other.vars.clone());
        vcombined.sort();
        vcombined.dedup();
        Self {
            effects: Self::new(combined).effects,
            vars: vcombined,
        }
    }

    /// Set intersection of two effect rows.
    pub fn intersect(&self, other: &Self) -> Self {
        let combined: Vec<String> = self
            .effects
            .iter()
            .filter(|e| other.effects.contains(e))
            .cloned()
            .collect();
        Self {
            effects: Self::new(combined).effects,
            vars: vec![],
        }
    }

    /// Instantiate effect variables by substituting them with a concrete row.
    /// Used at call sites: `fn_row.instantiate(&caller_row)` yields the concrete row.
    pub fn instantiate(&self, with: &Self) -> Self {
        let row = Self::from_parts(self.effects.clone(), vec![]);
        // Effect variables absorb the caller's effects.
        if self.vars.is_empty() {
            row
        } else {
            row.union(with)
        }
    }

    pub fn display(&self) -> String {
        let parts = self.effects.to_vec();
        let mut s = parts.join(", ");
        if !self.vars.is_empty() {
            if !s.is_empty() {
                s.push_str(", ");
            }
            s.push_str(&self.vars.join(", "));
        }
        if s.is_empty() {
            "pure".to_string()
        } else {
            s
        }
    }
}

pub struct EffectRegistry {
    /// Maps builtin / stdlib function name -> effect row.
    pub builtins: HashMap<String, EffectRow>,
}

impl EffectRegistry {
    pub fn new() -> Self {
        let mut r = Self {
            builtins: HashMap::new(),
        };
        r.register_stdlib();
        r
    }

    /// Look up the effect row for a function name. Returns None if not a known builtin.
    pub fn lookup(&self, name: &str) -> Option<&EffectRow> {
        self.builtins.get(name)
    }

    fn reg(&mut self, names: &[&str], row: EffectRow) {
        for n in names {
            self.builtins.insert(n.to_string(), row.clone());
        }
    }

    fn register_stdlib(&mut self) {
        // === io (terminal I/O) ===
        self.reg(
            &[
                "println",
                "print",
                "eprint",
                "eprintln",
                "std.fmt.println",
                "std.fmt.print",
                "std.io.read_line",
                "std.io.write",
                "std.io.flush",
                "std.io.getline",
                "std.term.read_key",
                "std.term.term_rows",
                "std.term.term_cols",
                "read_key",
                "term_clear",
                "term_cursor",
                "term_show_cursor",
                "term_set_color",
                "term_reset",
                "term_rows",
                "term_cols",
            ],
            EffectRow::from_strs(&["io"]),
        );

        // === io + alloc (terminal I/O that allocates strings) ===
        self.reg(
            &[
                "std.fmt.printf",
                "std.io.putline",
                "read_password",
                "read_line",
                "getline",
            ],
            EffectRow::from_strs(&["io", "alloc"]),
        );

        // === alloc (heap allocation) ===
        //
        // Both spellings matter. The IR-level names (`list_new`, `list_push`)
        // are what the lowerer emits, but the effect checker runs on the AST
        // and therefore sees the *surface* names a programmer writes — `list`,
        // `push`, `pop`. Registering only the IR names left the single most
        // common allocation in any IRIS program invisible to the checker.
        self.reg(
            &[
                // Pure in-memory string formatting / conversion / slicing / joining.
                "std.fmt.sprintf",
                "std.fmt.format",
                "std.string.concat",
                "sprintf",
                "format",
                "concat",
                "to_str",
                "string_concat",
                "join",
                "slice",
                "split",
                "repeat",
                "trim",
                "to_upper",
                "to_lower",
                "replace",
                "char_at",
                "str_reverse",
                "str_chars",
                "str_bytes",
                "str_pad_left",
                "str_pad_right",
                "json_stringify",
                "base64_encode",
                "base64_decode",
                "hex_encode",
                "hex_decode",
                "sha256",
                "regex_find_all",
                "regex_replace",
                "regex_replace_all",
                "type_of",
                "path_join",
                // Surface spellings (what the effect checker actually matches).
                "list",
                "push",
                "pop",
                "map",
                "list_new",
                "list_with_capacity",
                "list_push",
                "list_get",
                "list_set",
                "list_len",
                "list_pop",
                "list_insert",
                "list_remove",
                "list_contains",
                "list_index_of",
                "list_count",
                "list_map",
                "list_filter",
                "list_reduce",
                "list_any",
                "list_all",
                "list_zip",
                "list_enumerate",
                "list_flatten",
                "list_unique",
                "list_reverse",
                "list_sorted",
                "list_sum",
                "list_min",
                "list_max",
                "list_take",
                "list_drop",
                "map_new",
                "map_with_capacity",
                "map_insert",
                "map_set",
                "map_get",
                "map_remove",
                "map_contains",
                "map_len",
                "map_keys",
                "map_values",
                "map_entries",
                "sorted_keys",
                "deque_new",
                "deque_push_front",
                "deque_push_back",
                "deque_pop_front",
                "deque_pop_back",
                "deque_len",
                "deque_front",
                "deque_back",
                "bitset_new",
                "bitset_set",
                "bitset_get",
                "bitset_count",
                "bitset_clear",
                "channel",
                "chan_new",
                "task_group",
                "task_group_new",
                "gc_collect",
                "gc_collect_call",
                "gc_stats",
                "gc_stats_map",
                "vec_new",
                "vec_with_capacity",
                "option_new",
                "result_new",
                "some",
                "none",
                "ok",
                "err",
                "dtensor",
                "dtensor_tape",
                "dtensor_add",
                "dtensor_sub",
                "dtensor_mul",
                "dtensor_div",
                "dtensor_matmul",
                "dtensor_relu",
                "dtensor_sigmoid",
                "dtensor_tanh",
                "dtensor_sum",
                "dtensor_backward",
                "dtensor_grad",
                "dtensor_to_list",
            ],
            EffectRow::from_strs(&["alloc"]),
        );

        // === fs (file system) ===
        self.reg(
            &[
                "file_read",
                "file_read_all",
                "file_read_lines",
                "file_lines",
                "file_read_bytes",
                "file_write",
                "file_write_all",
                "file_write_bytes",
                "file_append",
                "file_open",
                "file_close",
                "file_exists",
                "path_exists",
                "file_size",
                "file_delete",
                "remove_file",
                "file_rename",
                "dir_create",
                "mkdir",
                "dir_list",
                "list_dir",
                "listdir",
                "cwd",
                "dir_delete",
                "std.fs.read_file",
                "std.fs.write_file",
                "std.fs.exists",
                "std.fs.delete",
                "std.fs.list_dir",
                "std.fs.append",
                "std.fs.file_size",
                "std.fs.read_lines",
                "std.fs.read_bytes",
                "std.fs.write_bytes",
            ],
            EffectRow::from_strs(&["fs", "alloc"]),
        );

        // === net (networking) ===
        self.reg(
            &[
                "tcp_listen",
                "tcp_connect",
                "tcp_connect_async",
                "tcp_send",
                "tcp_recv",
                "tcp_read",
                "tcp_write",
                "tcp_write_async",
                "tcp_close",
                "tcp_accept",
                // Tasks that complete when a socket is ready.
                "async_readable",
                "async_writable",
                "tcp_set_timeout",
                "udp_open",
                "udp_bind",
                "udp_send",
                "udp_recv",
                "udp_close",
                "http_get",
                "http_get_async",
                "http_post",
                "http_post_json",
                "http_request",
                "std.net.tcp_listen",
                "std.net.tcp_connect",
                "std.net.tcp_send",
                "std.net.tcp_recv",
                "std.net.tcp_accept",
                "std.net.tcp_close",
                "std.net.udp_bind",
                "std.net.udp_send",
                "std.net.udp_recv",
                "std.net.udp_close",
                "std.net.resolve_host",
                "std.http.get",
                "std.http.post",
                "std.http.request",
            ],
            EffectRow::from_strs(&["net", "alloc"]),
        );

        // === thread (concurrency) ===
        // `spawn` is a language keyword and therefore cannot be written in an
        // effect clause.  The public effect name for concurrency is `thread`.
        self.reg(
            &[
                "spawn",
                "spawn_async",
                "chan_send",
                "chan_recv",
                "recv_timeout",
                // A task that completes with the channel's next value or races tasks.
                "async_recv",
                "async_any",
                "async_yield",
                "task_group_cancelled",
                "chan_close",
                "chan_try_recv",
                "chan_len",
                "chan_select",
                "task_group_spawn",
                "task_group_join",
                "task_group_cancel",
                "std.async.spawn",
                "std.async.chan_new",
                "std.async.chan_send",
                "std.async.chan_recv",
                "std.async.task_group_new",
                "std.async.task_group_spawn",
                "std.async.task_group_join",
                "std.async.task_group_cancel",
                "cancel",
                "atomic",
            ],
            EffectRow::from_strs(&["thread", "alloc"]),
        );

        // Non-allocating task/atomic synchronization checks.
        self.reg(
            &[
                "task_done",
                "async_is_done",
                "task_cancelled",
                "async_is_cancelled",
                "task_cancel",
                "async_cancel",
                "atomic_add",
                "atomic_load",
                "cancellation_requested",
            ],
            EffectRow::from_strs(&["thread"]),
        );

        // === throw (exceptions/panics) ===
        self.reg(
            &[
                "panic",
                "unreachable",
                "todo",
                "assert",
                "std.assert.assert_eq",
                "std.assert.assert_ne",
                "std.assert.assert_true",
                "std.assert.assert_false",
                "std.assert.assert_some",
                "std.assert.assert_none",
                "std.assert.assert_ok",
                "std.assert.assert_err",
                "iris_panic",
                "unwrap",
                "unwrap_or",
            ],
            EffectRow::from_strs(&["throw"]),
        );

        // === ffi (foreign function interface & DB bindings) ===
        self.reg(
            &[
                "py_eval",
                "py_call",
                "py_import",
                "py_get",
                "py_set",
                "std.ffi.py_eval",
                "std.ffi.py_call",
                "dlopen",
                "dlsym",
                "dlclose",
                "std.ffi.dlopen",
                "std.ffi.dlsym",
                "std.ffi.dlclose",
                "iris_ffi_call",
                "ffi_call_str",
                "ffi_out_new",
                "ffi_out_get_str",
                "ffi_out_get_str_range",
                "python_eval",
                "python_exec",
                "python_call",
                "python_version",
                "db_query",
                "db_query_params",
                "db_exec_params",
            ],
            EffectRow::from_strs(&["ffi", "alloc"]),
        );

        // Scalar FFI / handle calls that do not allocate IRIS heap objects.
        self.reg(
            &[
                "ffi_open",
                "ffi_close",
                "ffi_call",
                "ffi_call_args",
                "ffi_call_i64",
                "ffi_call_f64",
                "ffi_call_void",
                "ffi_out_free",
                "ffi_out_len",
                "ffi_out_get_f64",
                "ffi_out_get_i64",
                "ffi_out_get_i32",
                "ffi_out_get_i32_i64",
                "ffi_out_get_f32",
                "ffi_out_get_u8",
                "ffi_out_set_f64",
                "ffi_out_set_i64",
                "ffi_out_set_i32",
                "ffi_out_set_f32",
                "ffi_out_set_u8",
                "ffi_out_write_bytes",
                "rust_lib_open",
                "rust_call_i64",
                "rust_call_f64",
                "rust_call_void",
                "db_open",
                "db_exec",
                "db_close",
            ],
            EffectRow::from_strs(&["ffi"]),
        );

        // === time (clock) ===
        self.reg(
            &[
                "now",
                "now_ms",
                "now_us",
                "time_now_ms",
                "datetime_timestamp",
                "monotonic",
                "sleep",
                "sleep_ms",
                "sleep_us",
                "timeout",
                // A task that completes after the given milliseconds.
                "async_sleep",
                "std.time.now",
                "std.time.now_ms",
                "std.time.now_us",
                "std.time.sleep",
                "std.time.sleep_ms",
                "std.time.monotonic",
            ],
            EffectRow::from_strs(&["time"]),
        );

        self.reg(
            &["datetime_now", "datetime_format"],
            EffectRow::from_strs(&["time", "alloc"]),
        );

        // === random (random numbers) ===
        self.reg(
            &[
                "random",
                "random_int",
                "random_range",
                "random_float",
                "random_seed",
                "std.random.random",
                "std.random.random_int",
                "std.random.random_float",
                "std.random.seed",
            ],
            EffectRow::from_strs(&["random", "time"]),
        );

        self.reg(&["seed"], EffectRow::from_strs(&["random"]));
        self.reg(
            &["uuid"],
            EffectRow::from_strs(&["random", "time", "alloc"]),
        );

        // === env (environment) ===
        self.reg(
            &[
                "getenv",
                "env_get",
                "setenv",
                "args",
                "argc",
                "argv",
                "std.env.getenv",
                "std.env.setenv",
                "std.env.args",
                "std.env.argc",
                "std.env.argv",
                "std.env.cwd",
                "std.env.set_cwd",
            ],
            EffectRow::from_strs(&["env", "alloc"]),
        );

        self.reg(
            &["env_set", "pid", "exit_code", "thread_count"],
            EffectRow::from_strs(&["env"]),
        );

        // === sys (system calls) ===
        self.reg(
            &[
                "system",
                "exec_cmd",
                "popen",
                "pclose",
                "exit",
                "abort",
                "std.sys.system",
                "std.sys.popen",
                "std.sys.pclose",
                "std.sys.exit",
                "std.sys.abort",
                "std.proc.exit",
                "std.proc.spawn",
                "kill",
                "raise",
                "signal",
            ],
            EffectRow::from_strs(&["sys", "alloc"]),
        );

        // === reflect (dynamic parsing, verification and evaluation) ===
        // Dynamic source evaluation is never pure merely because the source is
        // supplied as a string. Track it explicitly so strict mode cannot hide
        // reflective execution behind an otherwise pure wrapper.
        self.reg(
            &[
                "iris_validate",
                "reflect_validate",
                "validate",
                "iris_eval",
                "reflect_eval",
                "eval",
                "iris_eval_i64",
                "reflect_eval_i64",
                "eval_i64",
            ],
            EffectRow::from_strs(&["reflect", "alloc"]),
        );

        // === transaction (speculative state and staged filesystem writes) ===
        self.reg(
            &[
                "transaction_begin",
                "transaction_commit",
                "transaction_rollback",
                "transaction_depth",
            ],
            EffectRow::from_strs(&["transaction"]),
        );

        // === math (math library, pure but tracked for FP exceptions) ===
        // Pure math functions: sqrt, sin, cos, log, exp, etc.
        // Not annotated — default to pure.
    }
}

impl Default for EffectRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::EffectRegistry;

    #[test]
    fn collection_join_is_not_a_thread_effect() {
        let registry = EffectRegistry::new();
        let join = registry.lookup("join").expect("join must be registered");
        assert!(join.contains("alloc"));
        assert!(!join.contains("thread"));
        assert!(!join.contains("spawn"));
    }

    #[test]
    fn task_join_uses_the_spellable_thread_effect() {
        let registry = EffectRegistry::new();
        let join = registry
            .lookup("task_group_join")
            .expect("task_group_join must be registered");
        assert!(join.contains("thread"));
        assert!(!join.contains("spawn"));
    }

    #[test]
    fn in_memory_formatting_allocates_without_terminal_io() {
        let registry = EffectRegistry::new();
        for name in ["concat", "to_str", "format", "sprintf", "slice", "split"] {
            let row = registry
                .lookup(name)
                .unwrap_or_else(|| panic!("{name} must be registered"));
            assert!(row.contains("alloc"), "{name} should require alloc");
            assert!(!row.contains("io"), "{name} must not require terminal io");
        }
    }

    #[test]
    fn host_os_fs_ffi_and_time_builtins_have_contracts() {
        let registry = EffectRegistry::new();
        assert!(registry.lookup("env_get").unwrap().contains("env"));
        assert!(registry.lookup("exec_cmd").unwrap().contains("sys"));
        assert!(registry.lookup("file_lines").unwrap().contains("fs"));
        assert!(registry.lookup("ffi_call_i64").unwrap().contains("ffi"));
        assert!(registry.lookup("time_now_ms").unwrap().contains("time"));
        assert!(registry
            .lookup("datetime_timestamp")
            .unwrap()
            .contains("time"));
    }
}
