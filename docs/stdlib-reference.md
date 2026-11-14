# IRIS Standard Library Reference

All stdlib modules are imported with `bring std.<module>`:

```iris
bring std.math
bring std.string
bring std.fs
```

---

## math

Numeric utilities, constants, trigonometry, interpolation, number theory, and descriptive statistics implemented in pure IRIS.

```iris
bring std.math

val g = gcd(12, 8)           // 4
val l = lcm(4, 6)            // 12
val a = abs_i64(-5)          // 5
val c = clamp_i64(15, 0, 10) // 10
val e = is_even(4)           // true
val o = is_odd(3)            // true
```

**Functions:** `gcd`, `lcm`, `abs_i64`, `clamp_i64`, `min_i64`, `max_i64`, `sign_i64`, `is_even`, `is_odd`, `pi`, `tau`, `e_const`, `sqrt2`, `ln2`, `ln10`, `abs_f64`, `clamp_f64`, `min_f64`, `max_f64`, `sign_f64`, `deg_to_rad`, `rad_to_deg`, `atan2`, `hypot`, `sinh`, `cosh`, `tanh`, `lerp`, `inv_lerp`, `remap`, `smoothstep`, `is_close`, `round_to_decimals`, `is_prime`, `next_prime`, `factorial`, `permutations`, `combinations`, `is_power_of_two`, `next_power_of_two`, `integer_sqrt`, `pow_i64`, `sum_f64`, `mean`, `variance`, `std_dev`, `min_in_list`, `max_in_list`

---

## string

String manipulation utilities.

```iris
bring std.string

val ws  = words("hello world iris")   // list["hello","world","iris"]
val ls  = lines("a\nb\nc")           // list["a","b","c"]
val j   = str_join(ls, ", ")         // "a, b, c"
val e   = is_empty("")               // true
val pl  = pad_left("42", 5, "0")     // "00042"
val pr  = pad_right("hi", 5, ".")    // "hi..."
val rep = str_repeat("ab", 3)        // "ababab"
val tl  = trim_start("  hello  ")    // "hello  "
val te  = trim_end("  hello  ")      // "  hello"
```

**Functions:** `words`, `lines`, `str_join`, `is_empty`, `pad_left`, `pad_right`, `str_repeat`, `trim_start`, `trim_end`, `count`, `strip_prefix`, `strip_suffix`, `split_once`, `split_n`, `replace_n`, `normalize_whitespace`, `is_blank`, `capitalize`, `truncate`, `reverse_str`, `is_ascii_digit`, `is_ascii_alphabetic`, `is_ascii_alphanumeric`, `is_ascii_whitespace`, `levenshtein_distance`, `hamming_distance`

`trim_start`, `trim_end`, and `words` recognize ASCII whitespace (space and
bytes 9-13). They preserve UTF-8 content; `words` collapses whitespace runs and
returns an empty list for empty or whitespace-only input. `lines` recognizes LF
and CRLF, preserves interior empty lines and lone CR characters, and omits the
empty segment after a final newline. Empty input produces an empty list.

Padding widths count UTF-8 bytes, matching `len`. Padding repeats whole copies
of the supplied string until the minimum width is reached, so a multi-byte or
multi-character pad can exceed the requested width. Empty padding and widths
no larger than the input leave it unchanged.

### Parsing and cleanup

| Function | Return type | Behavior |
| --- | --- | --- |
| `strip_prefix(s, prefix)` | `option<str>` | Remove one matching prefix; `none` if absent. |
| `strip_suffix(s, suffix)` | `option<str>` | Remove one matching suffix; `none` if absent. |
| `split_once(s, delim)` | `option<(str, str)>` | Split at the first match, excluding the delimiter; `none` if absent. |
| `split_n(s, delim, max_parts)` | `list<str>` | At most `max_parts` fields; the last holds the unsplit remainder. |
| `replace_n(s, pattern, replacement, limit)` | `str` | Replace up to `limit` non-overlapping matches from left to right. |
| `normalize_whitespace(s)` | `str` | Collapse ASCII whitespace runs to one space and trim both ends. |
| `is_blank(s)` | `bool` | True for empty or entirely ASCII-whitespace input. |

Affixes and delimiters are literal strings, not regular expressions. Empty
prefixes/suffixes match without changing the input. `split_once(s, "")` returns
`some(("", s))`. `split_n` returns an empty list for nonpositive limits;
otherwise an empty delimiter leaves the input as one field. Adjacent and
trailing delimiters produce empty fields. Empty input is one empty field when
the limit is positive. `replace_n` leaves the input unchanged for an empty
pattern or nonpositive limit and never searches the replacement text. These
helpers preserve UTF-8 content and do not perform Unicode normalization.

```iris
val pair = split_once("name=iris=compiler", "=") // some(("name", "iris=compiler"))
val fields = split_n("a,b,c", ",", 2)           // list containing "a", "b,c"
val text = replace_n("a-a-a", "a", "b", 2)      // "b-b-a"
val clean = normalize_whitespace("  a\t b  ")  // "a b"
```

---

## fmt

Text formatting helpers.

```iris
bring std.fmt

val s = pad_int(7, 4)          // "   7"
val z = zero_pad_int(7, 4)     // "0007"
val l = left_align("hi", 6)    // "hi    "
val r = right_align("hi", 6)   // "    hi"
```

**Functions:** `pad_int`, `zero_pad_int`, `left_align`, `right_align`, `sprintf`, `printf`, `format_table`

Zero padding follows the sign: `zero_pad_int(-42, 6)` returns `"-00042"`.
`sprintf("%06d", split("-42", ","))` follows the same rule.

`format_table(headers, rows, col_widths)` accepts `list<str>`,
`list<list<str>>`, and `option<list<i64>>`. It calculates column widths from
all cells; supplied widths are minimum UTF-8 byte counts and never truncate
content. Missing cells are empty; extra cells create columns. Empty headers
are omitted. Columns are separated by `" | "`, rows by LF, with no final
newline and no padding after the final column. Empty input returns `""`.

```iris
val headers = split("Name,N", ",")
val rows: list<list<str>> = list()
list_push(rows, split("Ada,12", ","));
val table = format_table(headers, rows, none)
// "Name | N\nAda  | 12"
```

---

## fs

File system I/O.

```iris
bring std.fs

val text = read_text("data.txt")          // result<str, str>
val ok   = write_text("out.txt", "hello") // result<bool, str>
val ok2  = append_text("log.txt", "line\n")
val ex   = path_exists("file.iris")       // bool
val lns  = read_lines("data.txt")         // result<list<str>, str>
val ok3  = copy_file("src.txt", "dst.txt")
```

**Functions:** `read_text`, `write_text`, `append_text`, `path_exists`, `read_lines`, `copy_file`

`read_text` returns `result<str, str>`; `read_lines` returns
`result<list<str>, str>`. Write, append, and copy return `result<bool, str>`.
Handle `ok(value)` and `err(message)` explicitly. `path_exists` returns `bool`.
These operations require `effect fs, io`.

---

## path

Pure-IRIS path manipulation (no OS calls).

```iris
bring std.path

val b = basename("/home/user/file.txt")  // "file.txt"
val d = dirname("/home/user/file.txt")   // "/home/user"
val e = extension("report.pdf")          // "pdf"
val s = stem("report.pdf")              // "report"
val j = join_path("/home/user", "docs") // "/home/user/docs"
```

**Functions:** `basename`, `dirname`, `extension`, `stem`, `join_path`, `path_is_absolute`, `path_components`, `normalize_path`, `with_extension`

### Lexical path operations

These helpers use POSIX-style paths on every host: `/` is the only separator,
and a leading `/` makes a path absolute. Backslashes and drive letters are
ordinary characters. They perform no filesystem access or symlink resolution.

| Function | Return type | Behavior |
| --- | --- | --- |
| `path_is_absolute(p)` | `bool` | Whether the path starts with `/`. |
| `path_components(p)` | `list<str>` | Nonempty components, retaining `.` and `..`. |
| `normalize_path(p)` | `str` | Collapse repeated separators, `.` and cancellable `..`. |
| `with_extension(p, ext)` | `str` | Replace the final filename extension; empty `ext` removes it. |

Normalization preserves leading `..` in relative paths and clamps absolute
paths at `/`. Empty relative results become `"."`; trailing separators are
removed except for the root. Normalization is lexical and does not establish
whether a path stays inside a directory when symlinks are involved.

`with_extension` expects the extension without a leading dot. A filename's
leading dot is not an extension separator: `.env` becomes `.env.local` when
given `"local"`. Empty paths, paths ending in `/`, and final `.` or `..`
components remain unchanged. Parent-directory spelling is preserved.

```iris
val p = normalize_path("./build/../out//report.txt") // "out/report.txt"
val q = with_extension(p, "json")                   // "out/report.json"
val r = normalize_path("../../a/../b")              // "../../b"
```

---

## time

Timing and stopwatch utilities.

```iris
bring std.time

val ms  = now_ms()              // milliseconds since epoch
val s   = now_s()               // seconds since epoch
val t0  = stopwatch_start()
// ... work ...
val dur = stopwatch_stop(t0)    // elapsed ms
val fmt = format_duration(dur)  // e.g. "1.23s" or "450ms"
sleep(100)                      // sleep 100ms
```

**Functions:** `now_ms`, `now_s`, `sleep`, `elapsed_ms`, `stopwatch_start`, `stopwatch_stop`, `format_duration`

---

## stochastic

Stochastic calculus helpers for Brownian motion and related processes.

```iris
bring std.stochastic

val z = normal()
val incs = brownian_increments(3, 0.1)
val path = brownian_path(3, 0.1)
val gbm = gbm_path(3, 0.1, 1.0, 0.05, 0.2)
```

**Functions:** `normal_pair`, `normal`, `normal_mu_sigma`, `brownian_step`, `brownian_increments`, `brownian_path`, `brownian_motion`, `gbm_path`

---

## tensorx

Dense tensor helpers over flat `list<f64>` storage plus shape metadata.

```iris
bring std.tensorx

val shape = list();
val _ = list_push(shape, 2);
val _ = list_push(shape, 2);
val data = list();
val _ = list_push(data, 1.0);
val _ = list_push(data, 2.0);
val _ = list_push(data, 3.0);
val _ = list_push(data, 4.0);
val t = tensor_from_data(data, shape)
val r = tensor_relu(t)
```

**Functions:** `tensor_numel`, `tensor_full`, `tensor_zeros`, `tensor_from_data`, `tensor_get`, `tensor_set`, `tensor_data`, `tensor_shape`, `tensor_add`, `tensor_relu`, `tensor_sigmoid`, `tensor_matmul2`, `tensor_batch_matmul`

---

## reverse-mode builtins

Interpreter-backed reverse-mode autodiff is available through builtins:

```iris
def main() -> f64 {
    val x = tape(3.0)
    val y = x * x + 2.0 * x
    val _ = backward(y)
    return grad(x)
}
```

**Builtins:** `tape`, `backward`, `grad`

---

## testing

Test assertion helpers.

```iris
bring std.testing

def test_basic() -> bool {
    val ok = assert_eq(1 + 1, 2, "addition");
    val ok2 = assert_str_eq("hi", "hi", "strings");
    return ok && ok2
}
```

**Functions:** `assert_eq`, `assert_approx_eq`, `assert_str_eq`, `assert_true`, `assert_false`, `assert_some`, `assert_none`, `assert_ok`, `assert_err`, `assert_list_eq`, `fail`

---

## log

Structured logging with severity levels.

```iris
bring std.log

log("server started")
info("Connected to port 8080")
warn("Disk usage above 80%")
debug("req_id=42 path=/api/data")
```

**Constants:** `LOG_DEBUG` (0), `LOG_INFO` (1), `LOG_WARN` (2), `LOG_ERROR` (3)

**Functions:** `debug`, `info`, `warn`, `error`, `log`, `log_at`, `log_kv`

---

## json

Flat-key JSON builder and parser.

```iris
bring std.json

// Build JSON
val s = json_str("hello")           // "\"hello\""
val arr = json_arr(["1","2","3"])   // "[1,2,3]"
val keys = list(); push(keys, "name");
val vals = list(); push(vals, json_str("Alice"));
val obj = json_obj(keys, vals)      // {"name":"Alice"}

// Simple flat object
val doc = json_new()
json_set(doc, "x", "1")
json_set(doc, "y", "2")
val v = json_get(doc, "x")         // some("1")
```

**Functions:** `json_str`, `json_arr`, `json_obj`, `json_new`, `json_set`, `json_get`

---

## iter

Functional list utilities (all operate on `list<i64>`).

```iris
bring std.iter

val nums = list(); push(nums, 3); push(nums, 1); push(nums, 2);
val s  = sum(nums)               // 6
val p  = product(nums)           // 6
val mn = min(nums)               // 1
val mx = max(nums)               // 3
val rv = reverse(nums)           // [2,1,3]
val tk = take(nums, 2)           // [3,1]
val dr = drop(nums, 1)           // [1,2]
val ct = count(nums, 1)          // 1
val idx = index_of(nums, 2)      // 2
```

**Functions:** `sum`, `product`, `min`, `max`, `reverse`, `take`, `drop`, `contains`, `count`, `index_of`, `flatten_i64`, `range`, `map_i64`, `filter_i64`, `fold_i64`, `any_i64`, `all_i64`, `find_i64`, `partition_i64`, `unique_i64`, `chunks_i64`, `windows_i64`

### List pipelines

The following helpers operate on `list<i64>`. Callbacks may be named functions
or closures, including closures that capture values. Collection-producing
operations allocate fresh lists and preserve input order. Callbacks receive
the original values from left to right; they should not structurally modify
the input list while it is being traversed.

| Function | Return type | Behavior |
| --- | --- | --- |
| `map_i64(xs, transform)` | `list<i64>` | Apply `transform: \|i64\| -> i64` once to each element. |
| `filter_i64(xs, predicate)` | `list<i64>` | Retain elements satisfying `predicate: \|i64\| -> bool`. |
| `fold_i64(xs, initial, combine)` | `i64` | Left fold with `combine: \|i64, i64\| -> i64`. |
| `any_i64(xs, predicate)` | `bool` | Stop at the first true predicate result; false for empty input. |
| `all_i64(xs, predicate)` | `bool` | Stop at the first false predicate result; true for empty input. |
| `find_i64(xs, predicate)` | `option<i64>` | First matching value, or `none`; stops at the first match. |
| `partition_i64(xs, predicate)` | `(list<i64>, list<i64>)` | Matching and nonmatching values, testing each exactly once. |
| `unique_i64(xs)` | `list<i64>` | First occurrence of each distinct value in encounter order. |
| `chunks_i64(xs, size)` | `list<list<i64>>` | Non-overlapping chunks; include the final short chunk. |
| `windows_i64(xs, size)` | `list<list<i64>>` | Every full overlapping window with a stride of one. |

An empty fold returns `initial` without invoking its callback. Other
collection-producing helpers return empty lists for empty input; partition
returns two empty lists. Nonpositive chunk/window sizes return empty lists.
Oversized chunks contain the entire input; oversized windows produce no
windows. Each chunk/window owns a separate list, so modifying one does not
modify its neighbors or the source.

`unique_i64` uses a linear membership scan per element (O(n²) worst case).
The other flat pipelines are O(n), excluding callback costs. Window copying
costs O((n - size + 1) × size) for valid sizes.

```iris
val xs = range(1, 6)
val doubled = map_i64(xs, |x: i64| x * 2)
val selected = filter_i64(doubled, |x: i64| x > 5)
val total = fold_i64(selected, 0, |acc: i64, x: i64| acc + x) // 24
val batches = chunks_i64(xs, 2) // lists containing (1,2), (3,4), (5)
```

For a complete example combining string parsing, paths, and list pipelines,
see [`examples/06_data/stdlib_pipeline.iris`](../examples/06_data/stdlib_pipeline.iris).

---

## set

Sorted-list-backed set for `i64` values.

```iris
bring std.set

val s = set_new()
set_add(s, 10);
set_add(s, 20);
val has = set_contains(s, 10)    // true
set_remove(s, 10)
val n = set_len(s)               // 1
val u = set_union(s1, s2)
val i = set_intersection(s1, s2)
val d = set_difference(s1, s2)
val l = set_to_list(s)
```

**Functions:** `set_new`, `set_add`, `set_remove`, `set_contains`, `set_len`, `set_union`, `set_intersection`, `set_difference`, `set_to_list`

---

## queue

FIFO queue backed by a list.

```iris
bring std.queue

var q = queue_new()
q = enqueue(q, 10)
q = enqueue(q, 20)
val v = dequeue_val(q)     // 10
q = dequeue_queue(q)
val empty = queue_is_empty(q)
val n = queue_len(q)
```

**Functions:** `queue_new`, `enqueue`, `dequeue_val`, `dequeue_queue`, `queue_peek`, `queue_len`, `queue_is_empty`

---

## heap

Min-heap (sorted-list-backed) for `i64` values.

```iris
bring std.heap

var h = heap_new()
h = heap_push(h, 5)
h = heap_push(h, 3)
h = heap_push(h, 8)
val min = heap_peek(h)    // 3
val v   = heap_pop_val(h) // 3
h = heap_pop_heap(h)
val n = heap_len(h)
```

**Functions:** `heap_new`, `heap_push`, `heap_peek`, `heap_pop_val`, `heap_pop_heap`, `heap_len`

---

## deque

Double-ended queue.

```iris
bring std.deque

val d = deque_new()
deque_push_front(d, 1);
deque_push_back(d, 2);
val f = deque_front(d)      // 1
val b = deque_back(d)       // 2
val pf = deque_pop_front(d) // 1
val pb = deque_pop_back(d)  // 2
val n = deque_len(d)
```

**Functions:** `deque_new`, `deque_push_front`, `deque_push_back`, `deque_pop_front`, `deque_pop_back`, `deque_front`, `deque_back`, `deque_len`

---

## bitset

Fixed-size bitset backed by an `i64`.

```iris
bring std.bitset

val b = bitset_new(64)
bitset_set(b, 3)
val v = bitset_get(b, 3)    // true
bitset_clear(b, 3)
val n = bitset_count(b)     // 0
```

**Functions:** `bitset_new`, `bitset_set`, `bitset_get`, `bitset_clear`, `bitset_count`

---

## os

Operating system utilities.

```iris
bring std.os

val dir = getcwd()             // current directory
val env = getenv("HOME")       // environment variable
setenv("MY_VAR", "value")
val files = readdir(".")       // list<str>
val ok = make_dir("newdir")
val ok2 = exists("file.txt")
val pid = get_pid()
val out = shell("echo hello")  // "hello\n"
exit(0)
```

**Functions:** `getcwd`, `getenv`, `setenv`, `get_pid`, `shell`, `exit`, `readdir`, `make_dir`, `exists`, `cpu_count`

---

## http

Typed HTTP client. HTTPS is provided by WinHTTP on the validated Windows target;
check `http_tls_available()` on portable code.

```iris
bring std.http

val response = http_send(HttpRequest {
    method: "POST",
    url: "https://api.example.com/v1/items",
    headers: "Authorization: Bearer token\r\n",
    body: "{\"x\":1}",
    timeout_ms: 10000,
})
if is_ok(response) {
    println(to_str(unwrap(response).status));
}
```

**Records:** `HttpRequest`, `HttpResponse`

**Functions:** `http_send`, `http_tls_available`, `http_get`, `http_post`,
`http_get_request`, `http_post_request`, `http_response`, `http_status_code`,
`http_header`, `http_body`

---

## kv

File-backed key-value store (text format: `key=value\n`).

```iris
bring std.kv

kv_set("store.txt", "name", "Alice")
val v = kv_get("store.txt", "name")   // "Alice"
kv_delete("store.txt", "name")
val keys = kv_keys("store.txt")        // list<str>
```

**Functions:** `kv_get`, `kv_set`, `kv_delete`, `kv_keys`, `kv_read_file`, `kv_write_file`, `kv_pair`

---

## ml

Machine learning algorithms.

```iris
bring std.ml

// Linear regression
val model = linreg_train(X, y, 0.01, 1000)
val pred  = linreg_predict(model, x_new)

// Logistic regression
val clf = logreg_train(X, y, 0.01, 500)
val p   = logreg_predict(clf, x_new)

// k-NN
val label = knn_predict(X_train, y_train, x_query, k)

// k-Means
val centroids = kmeans_train(data, k, 100)

// Naive Bayes
val gnb = gnb_train(X, y, n_classes)
val pred = gnb_predict(gnb, x_new)

// Metrics
val acc = accuracy(y_true, y_pred)
val pr  = precision(y_true, y_pred)
val rc  = recall(y_true, y_pred)
val m   = mse(y_true, y_pred)
val mae_v = mae(y_true, y_pred)
```

---

## nn

Neural network building blocks.

```iris
bring std.nn

val mlp = mlp_create(layer_sizes)
mlp_train(mlp, X, y, lr, epochs)
val pred = mlp_predict(mlp, x)
val cls  = mlp_predict_class(mlp, x)
```

---

## crypto

Hashing and encoding.

```iris
bring std.crypto

val id  = generate_uuid()               // UUID v4 string
val h   = hash_code("hello")            // i64 hash
val b64 = to_base64("hello")            // base64 string
val raw = from_base64(b64)              // original string
val hex = to_hex("hello")               // hex-encoded
val dec = from_hex(hex)                 // decoded
val sha = hash_sha256("hello")          // hex digest
val md5 = md5_str("hello")              // hex digest (for identifiers such as JA3, not security)
```

**Functions:** `generate_uuid`, `hash_code`, `to_base64`, `from_base64`, `to_hex`, `from_hex`,
`hash_sha256`, `sha1_str`, `sha1_bytes`, `md5_str`, `md5_bytes`, `md5_hex`, `hmac_simple`,
`hmac_verify`, `timing_safe_equal`, and AES-128 (`aes128_encrypt_cbc`, `aes128_decrypt_cbc`)

---

## ffi

Foreign function interface for calling native C/Rust libraries.

```iris
bring std.ffi

val lib = lib_open("./mylib.so")
val res = lib_call(lib, "my_function")
lib_close(lib)
```

**Functions:** `lib_open`, `lib_call`, `lib_close`, `py_eval`, `py_exec`, `py_call`, `py_version`, `rust_open`

---

## csv

CSV file parser and emitter.

```iris
bring std.csv

val text = read_text("data.csv")
val rows = csv_row_count(text)
val cols = csv_col_count(text)
val row  = csv_get_row(text, 0)       // list<str>
val r    = csv_parse_row("a,b,c")     // list<str>
val line = csv_emit_row(cells)        // "a,b,c"
```

**Functions:** `csv_row_count`, `csv_col_count`, `csv_get_row`, `csv_parse_row`, `csv_emit_row`

These split on every comma and every line: a quoted field that holds a
comma, a quote or a line break is split wrongly. For RFC 4180 input use
`csv_records` in [projects/cli_tools/csvkit.iris](../projects/cli_tools/csvkit.iris).

---

## rl

Reinforcement learning primitives and experience replay buffers.

```iris
bring std.rl

// Experience tuple
val exp = Experience {
    state: [1.0, 0.0],
    action: 1,
    reward: 1.0,
    next_state: [1.0, 1.0],
    done: false
}

// Experience replay buffer
var buf = replay_buffer_new(100)
buf = replay_buffer_push(buf, exp)
val indices = replay_buffer_sample_indices(buf, 10)

// Q-Learning updates
val q = q_table_new(10, 2)
val val_q = q_get(q, 3, 1, 2)
q_set(q, 3, 1, 2, 0.5)
val updated_q = q_learning_update(q, 3, 1, 1.0, 4, 2, 0.1, 0.99)
val best_action = q_best_action(q, 3, 2)

// Environment step simulations
val step_res = grid_step(3, 1, 10) // (new_state, reward, done)
val reward = bandit_pull(2, 4)
```

**Records:** `Experience`, `ReplayBuffer`

**Functions:** `replay_buffer_new`, `replay_buffer_push`, `replay_buffer_sample_indices`, `q_table_new`, `q_get`, `q_set`, `q_learning_update`, `sarsa_update`, `q_best_action`, `policy_gradient_loss`, `log_prob_softmax`, `grid_step`, `bandit_pull`

---

## ais

Autonomous intelligent systems: an agent that infers its state, chooses by
expected free energy and learns its model; an online world model that keeps a
plant viable as its dynamics change; a safety policy over the state and the
model's predictions; and the older building blocks (homeostat, MAPE-K loop,
evolution, EWC, bandit strategies). Each part is shown working in a
`tests/test_ais_*.iris` file, on both backends.

```iris
bring std.ais

// Active inference: a two-state world where action a leads to state a.
val m = pomdp_new(2, 2, 2)
pomdp_set_likelihood(m, 0, list(1.0, 0.0), 100.0)   // state 0 shows obs 0
pomdp_set_likelihood(m, 1, list(0.0, 1.0), 100.0)
for a in 0..2 {
    for s in 0..2 {
        val row: list<f64> = list(0.0, 0.0)
        list_set(row, a, 1.0)
        pomdp_set_transition(m, a, s, row, 100.0)
    }
}
pomdp_set_preferences(m, list(0.0, 3.0))            // prefer observation 1
var agent = aif_agent_new(m, 1, 4.0)                 // horizon 1, precision 4
agent = aif_observe(agent, 0)                        // exact Bayes
val step = aif_decide(agent)                         // (agent, decision)
assert(step.1.action == 1)

// An online world model: next = W [state, action, 1], fitted by RLS.
var wm = wm_new(1, 1, 0.98)
wm = wm_learn(wm, list(20.0), list(1.0), list(21.5))
val next = wm_predict(wm, list(20.0), list(1.0))

// A safety policy: never heat above 58, cool above 70.
val safety = safety_policy_new()
safety_forbid_when(safety, "no heat above 58", 0, ">", 58.0, 1, 5)
safety_require_when(safety, "cool above 70", 0, ">", 70.0, 2, 10)
val allowed = safety_permitted_now(safety, list(60.0), 3)   // heat forbidden
assert(!list_get(allowed, 1))
```

**Active inference** (`Pomdp`, `AifAgent`, `AifDecision`, `PolicyValue`):
`pomdp_new`, `pomdp_set_likelihood`, `pomdp_set_transition`,
`pomdp_set_preferences`, `pomdp_set_prior`, `pomdp_predict`,
`pomdp_observation_probs`, `pomdp_posterior`, `pomdp_policy_value` (risk,
ambiguity, information gain, utility, novelty and G), `policies_all`,
`pomdp_learn_likelihood`, `pomdp_learn_transition`, `aif_agent_new`,
`aif_with_drives` (weights on information and on novelty: 0, 0 is a purely
reward-seeking agent), `aif_with_learning`, `aif_with_sampling`, `aif_reset`,
`aif_observe`, `aif_decide`, `aif_decide_permitted`.

**World model and viability** (`LinearModel`, `ViabilityChoice`):
`linear_model_new`, `linear_model_predict`, `linear_model_update`,
`linear_model_variance`, `wm_new`, `wm_features`, `wm_learn`, `wm_predict`,
`wm_rollout`, `viability_choose`, and from v2 `viability_bound_new`,
`viability_assess`.

**Safety** (`SafetyPolicy`, `SafetyRule`, `SafetyVerdict`):
`safety_policy_new`, `safety_forbid_when`, `safety_require_when`,
`safety_keep_within` (forbids an action the world model predicts would leave
a range), `safety_condition`, `safety_permitted_now`, `safety_permitted`,
`safety_required`, `safety_resolve` (with an audit trail).

**Autopilot** (`Autopilot`, `AutopilotStep`): `autopilot_new`,
`autopilot_step`, `autopilot_with_learning` -- learns the model from each
step, applies the safety policy, and chooses the viable action.

**Governor, continual learning, persistence:** `software_governor_step`
(`GovernorStep`), `ewc_consolidate`, `ewc_penalty_grad`, `pomdp_save`,
`pomdp_load`, `linear_model_save`, `linear_model_load`.

**Building blocks:** `probs_normalize`, `probs_softmax`, `probs_log_softmax`,
`probs_kl`, `probs_argmax`, `probs_sample`, `agent_loop`,
`agent_loop_rewards`, `mapek_agent_new`, `mapek_step`, `mapek_run`,
`homeostat_new`, `homeostat_update`, `homeostat_is_safe`, `argmax`,
`epsilon_greedy`, `softmax_sample`, `boltzmann_sample`, `ucb_select`,
`discount_rewards`, `gae`, `normalize_list`, `normalize_input`, `clip_list`,
`population_new`, `population_evolve`, `ewc_new`, `ewc_penalty`,
`model_save`, `model_load`.

---

## net

Typed TCP and UDP transport. TCP helpers return `result` values and provide
timeouts, exact writes, bounded reads, shutdown, and close.

```iris
bring std.net

val connected = tcp_connect_checked("127.0.0.1", 8080, 2000, 5000)
if is_ok(connected) {
    val stream = unwrap(connected)
    val sent = tcp_write_all_checked(stream, "ping")
    val reply = tcp_read_with_timeout(stream, 2000, 4096)
    tcp_shutdown_checked(stream);
    tcp_close_checked(stream);
}
```

`tcp_listen_checked` and `tcp_listen_on` listen on every interface;
`tcp_listen_local_checked` and `tcp_listen_local` on 127.0.0.1 only, which
is what a service meant for this machine should use. The `_checked` reads
and accepts block their thread (bounded by the timeouts). In an `async def`,
`await tcp_accept_async(listener, read_ms, write_ms)` and
`await tcp_read_async(stream, max_bytes)` wait for a connection or for data
without blocking: other tasks and timers run meanwhile, so one thread can
serve many connections. They wait on the socket's readiness (`poll`, or
`WSAPoll` on Windows) through the builtin `async_readable(handle)`; writes
still block until the data is handed to the operating system.

**Records:** `TcpStream`, `TcpListener`

**Functions:** `tcp_connect_checked`, `tcp_listen_checked`,
`tcp_listen_local_checked`, `tcp_accept_checked`, `tcp_accept_async`,
`tcp_write_all_checked`, `tcp_read_bounded`, `tcp_read_async`, `tcp_read_with_timeout`, `tcp_shutdown_checked`,
`tcp_close_checked`, `tcp_close_listener`, and the raw-handle forms
`tcp_listen_on`, `tcp_listen_local`, `tcp_accept_conn`, `tcp_read_conn`,
`tcp_write_conn`, `tcp_close_conn`

---

## pcap

libpcap captures and Ethernet, IPv4, TCP and UDP dissection. Reads classic
libpcap in either byte order, with microsecond or nanosecond timestamps (not
pcapng); writes big-endian microsecond files.

```iris
bring std.buffer
bring std.pcap

val bytes = file_read_bytes("capture.pcap")      // list<i64>, NULs kept
val buf = ByteBuffer { data: bytes, len: list_len(bytes) }
val header = unwrap(pcap_parse_global_header(buf))
var offset = 24
val first = pcap_read_packet(buf, offset, header)  // next: offset + 16 + incl_len
```

**Records:** `PcapGlobalHeader`, `PcapPacket`, `EthernetFrame`, `Ipv4Packet`,
`TcpSegment`, `UdpDatagram`

**Functions:** `pcap_parse_global_header`, `pcap_read_packet`,
`dissect_ethernet`, `dissect_ipv4`, `dissect_tcp`, `dissect_udp`,
`pcap_create_global_header`, `pcap_create_packet_record`, `pcap_format_ip`,
`pcap_format_mac`

---

## tls

TLS ClientHello dissection and fingerprints. JA3 follows Salesforce's
definition (version, ciphers, extensions, groups, point formats, GREASE
removed; MD5) and JA4 FoxIO's.

```iris
bring std.tls

val tls_record = tls_parse_record(payload)             // a TCP payload
val hello = unwrap(tls_parse_client_hello(unwrap(tls_record).payload))
val ja3 = tls_calculate_ja3(hello)                     // .ja3_str, .ja3_hash
val ja4 = tls_calculate_ja4(hello, false)              // .ja4
```

**Records:** `TlsRecord`, `TlsClientHello`, `Ja3Result`, `Ja4Result`

**Functions:** `tls_parse_record`, `tls_parse_client_hello`,
`tls_calculate_ja3`, `tls_calculate_ja4`, `tls_is_grease`

`matched_threat` is a coarse heuristic over the cipher and extension lists,
not a lookup in a fingerprint database; compare fingerprints with a list you
trust, as `projects/cli_tools/tlsfp.iris --known` does.

---

## llm

Provider-neutral remote chat/tool/embedding requests and local model wrappers.
Credentials are read from the configured environment variable at call time.

```iris
bring std.llm

val client = llm_client(chat_url, embeddings_url, "model-name", "API_KEY", 30000)
val messages: list<LlmMessage> = list()
list_push(messages, llm_message("user", "Summarize this event"));
val response = llm_chat(client, messages, 0.2, 256)
```

A tool loop: send the tool table, run what the model asks for, and send each
result back tied to its call:

```iris
val reply = unwrap(llm_chat_with_tools(client, messages, 0.0, 512, tools_json))
val calls = llm_tool_calls(reply)                  // list<LlmToolCall>: id, name, arguments_json
if list_len(calls) > 0 {
    list_push(messages, llm_assistant_turn(reply));
    val call = list_get(calls, 0)
    list_push(messages, llm_tool_result(call.id, run_my_tool(call.name, call.arguments_json)));
}
```

Treat `arguments_json` as untrusted input: decode and check it before acting
([projects/llm_agent](../projects/llm_agent/README.md) does this per tool).

**Records:** `LlmClient`, `LlmMessage`, `LlmTool`, `LlmToolCall`, `LlmResponse`,
`EmbeddingResponse`, `LocalModel`

**Functions:** `llm_chat`, `llm_chat_with_tools`, `llm_tool_calls`,
`llm_assistant_turn`, `llm_tool_result`, `llm_embed`, `llm_request_json`,
`llm_response_from_json`, `local_onnx_model`, `local_model_run`,
`local_pytorch_train_step`

---

## meta

Compiler-hosted typed metaprogramming without a self-hosted compiler.

```iris
bring std.meta

val program = meta_program("def answer() -> i64 { return 41 }")
if meta_available() {
    val analysis = meta_analyze(program)
    val edited = meta_apply(program, MetaEdit {
        start_byte: 29,
        end_byte: 31,
        replacement: "42",
    })
}
```

**Records:** `MetaProgram`, `MetaEdit`

**Functions:** `meta_available`, `meta_program`, `meta_analyze`, `meta_emit_ir`,
`meta_apply`

`meta_apply` returns edited source only after the complete compiler accepts it.
Standalone native programs currently report the compiler service unavailable.

---

## async

Task helpers, structured concurrency, and scheduler telemetry.

Calling an `async def` returns a `task<T>` and schedules it on the calling
thread's executor; `await` it for the result. The compiler turns each
`async def` body into a state machine, so an `await` inside one suspends only
that task. The language builtins for tasks are `async_sleep(ms)`,
`async_yield()`, `async_recv(ch)`, `async_readable(socket)` (the socket
can be read; see `net`), `async_any(t1, t2, ...)` /
`async_any(tasks)` (the index of the first to finish; losing timers and
receives are cancelled), `task_done(t)`, `task_cancel(t)` and
`task_cancelled(t)`.

```iris
bring std.async

async def fetch(id: i64, delay: i64) -> i64 {
    await async_sleep(delay);
    return id * 10
}

def main() -> i64 effect alloc, thread, time {
    // Both tasks run concurrently.
    val tasks: list<task<i64>> = list()
    list_push(tasks, fetch(1, 30));
    list_push(tasks, fetch(2, 10));
    val results = await join_all(tasks)
    // A deadline: none if the task has not finished in 20 ms.
    val late = await timeout_after(fetch(3, 500), 20)
    return 0
}
```

**Task functions:** `timeout_after[T](t, ms) -> option<T>`,
`join_all[T](tasks) -> list<T>`, `cancel_all[T](tasks) -> i64`

```iris
bring std.async

val group = new_task_group()
spawn(group) {
    if !cancellation_requested() {
        atomic_add(counter, 1)
    }
}
task_group_cancel(group);
task_group_join(group);
val cancelled = task_group_cancelled(group)
```

**Structured concurrency and channels:** `new_task_group`, `cancellation_requested`,
`task_group_cancelled`, `async_worker_count`, `async_queued_tasks`,
`try_recv`, `channel_bounded`, `channel_unbounded`, `channel_len`, `delay`,
`sleep`, `num_threads`, `select_first`, `recv_timeout_ms`,
`gc_collect_sweep`, `gc_get_stats`

Native `spawn` and `spawn(group)` jobs run on the bounded worker pool
(`async_worker_count`, `async_queued_tasks`); `async def` tasks do not use it.
Task-group cancellation is cooperative: queued grouped jobs skip their body at
entry, and long-running jobs should call `cancellation_requested()` at bounded
intervals.

---

## ros2

ROS 2 on rclcpp, rclcpp_action, rclcpp_lifecycle and tf2_ros, through the
IRIS ROS 2 bridge (build it with `python build_ros2_bridge.py`; run in a
sourced ROS 2 environment). Verified on Windows Humble with Cyclone DDS;
Linux is not built yet and each context has one single-threaded executor.
Build, model and limits: [ros2-build.md](ros2-build.md).

Callbacks become events: `spin_once(ctx, timeout_ms)` runs the context's
executor and returns a `ROS2Event` (`kind`, `source`, `message`, `id`, `aux`).
Messages are handles to real ROS 2 messages of any installed type, read and
written by field path.

```iris
bring std.ros2

def main() -> i64 {
    val ctx = context_create()
    val node = node_create(ctx, "follower", "")
    val scans = subscription_create_qos(node, "/scan", "sensor_msgs/msg/LaserScan", qos_sensor_data())
    val cmd = publisher_create(node, "/cmd_vel", "geometry_msgs/msg/Twist")
    val gain = param_declare_f64_range(node, "gain", 0.5, 0.0, 2.0, "speed per metre")
    val ev = spin_once(ctx, 100)
    if is_message(ev, scans) {
        val nearest = msg_f64(ev.message, "range_min")
        publish_twist(cmd, make_twist(param_f64(node, "gain") * nearest, 0.0, 0.0, 0.0, 0.0, 0.0));
    };
    assert(context_destroy(ctx));
    return 0
}
```

**Nodes and the executor:** `context_create`, `context_create_with(args,
domain_id)`, `node_create`, `node_create_with(ctx, name, ns, args, flags)`,
`lifecycle_node_create`, `spin_once`, `spin_some`, `event_text`, `is_message`,
`is_timer`, `is_request`, `is_response`, `is_goal`, `is_cancel`,
`is_parameter_change`, `is_transition`, `node_now_ns`, `log_info`/`warn`/
`error`/`debug`, `graph_topics`/`services`/`nodes`

**Messages:** `msg_new`, `msg_from_json`, `msg_free`, `msg_clone`,
`msg_type`, `msg_json`, `msg_get_json`/`msg_set_json`, `msg_f64`/`i64`/
`bool`/`str` and their `msg_set_*`, `msg_f64s`/`msg_set_f64s`, `msg_len`,
`msg_resize`, `msg_has`, `msg_stamp`/`msg_set_stamp`, `msg_schema`

**Topics:** `publisher_create(_qos)`, `publisher_msg`, `publish_msg`,
`publish_json`, `subscription_create(_qos)`, `take_msg`, `wait_for_message`,
typed `publish_*`/`take_*` for Float64, Int64, String, Vector3, Twist, Pose;
QoS presets `qos_default`, `qos_sensor_data`, `qos_control`, `qos_latched`,
`qos_keep_last`, `qos_best_effort`

**Services, timers, parameters, lifecycle:** `service_create`, `request_of`,
`response_of`, `service_reply`, `client_create`, `client_request`,
`client_call`, `client_send`, `client_wait_response`, `timer_create`,
`wall_timer_create`, `param_declare_f64`/`i64`/`bool`/`str`/`json`,
`param_declare_*_range`, `param_f64`/…, `param_set_*` (false when rclcpp
rejects the value), `lifecycle_configure`/`activate`/`deactivate`/`cleanup`/
`shutdown`, `lifecycle_state`, `lifecycle_set_result`

**Actions:** `action_server_create(_with)`, `goal_of`, `goal_request`,
`goal_feedback_msg`, `goal_publish_feedback`, `goal_result_msg`,
`goal_succeed`/`abort`/`canceled`, `action_client_create`, `action_goal_msg`,
`action_send_goal`, `action_cancel`, `action_status`, `action_wait_result`

**tf2:** `tf_buffer_create(node)` (listener on /tf and /tf_static),
`tf_buffer_local()`, `tf_set`, `tf_lookup`, `tf_lookup_at`,
`tf_can_transform`, `tf_broadcaster_create`, `tf_static_broadcaster_create`,
`tf_send`, plus pure transform arithmetic (`transform_compose`,
`transform_inverse`, `transform_point`, quaternion helpers) that needs no
ROS 2 install.

---

## adaptive

Adaptive AI runtime primitives for online learning, risk awareness, self-training, and adaptive behavior.

```iris
bring std.adaptive

val handle = adaptive_new("controller", 4, 0.05, 0.1)
adaptive_record_error(handle, 0.02);
val safe = !adaptive_is_unsafe(handle)
val conf = adaptive_confidence(handle)
adaptive_free(handle);
```

**Types:** `RiskMetrics = (f64, f64, i64, i64, f64, f64)`

**Functions:** `adaptive_new`, `adaptive_free`, `adaptive_name`, `adaptive_get_param`, `adaptive_set_param`, `adaptive_n_params`, `adaptive_learning_rate`, `adaptive_set_learning_rate`, `adaptive_record_error`, `adaptive_is_unsafe`, `adaptive_set_risk_threshold`, `adaptive_should_retrain`, `adaptive_set_retrain_threshold`, `adaptive_adapt_threshold`, `adaptive_current_threshold`, `adaptive_observation_count`, `adaptive_mean_error`, `adaptive_max_error`, `adaptive_error_count`, `adaptive_last_risk`, `adaptive_confidence`, `adaptive_reset_stats`, `adaptive_predict`, `adaptive_train_step`, `adaptive_train_batch`, `risk_mean_error`, `risk_max_error`, `risk_observations`, `risk_errors`

---

## buffer

Safe, length-tracked binary byte buffers handling arbitrary binary protocols and embedded null bytes (`0x00`) without truncation.

```iris
bring std.buffer

val buf = byte_buffer_new()
val b1  = byte_buffer_push(buf, 0x7F)
val b2  = byte_buffer_push(b1, 0x00)
val len = byte_buffer_len(b2)            // 2
val hex = byte_buffer_to_hex(b2)         // "7f00"
```

**Records:** `ByteBuffer { data: list<i64>, len: i64 }`

**Functions:** `byte_buffer_new`, `byte_buffer_zeros`, `byte_buffer_push`, `byte_buffer_get`, `byte_buffer_set`, `byte_buffer_len`, `byte_buffer_to_hex`, `byte_buffer_from_hex`, `byte_buffer_slice`, `byte_buffer_append`, `byte_buffer_read_u16_be`, `byte_buffer_write_u16_be`, `byte_buffer_read_u32_be`, `byte_buffer_write_u32_be`

---

## container

Unified generic container interfaces and higher-kinded type abstractions across `list` and `option`.

```iris
bring std.container

val xs = list(1, 2, 3)
val sz = xs.size()                      // 3
val has = xs.has_any()                  // true
val f = xs.first()                      // some(1)
```

**Traits:** `Sized` (`size`, `is_empty`, `has_any`), `Linear[T]` (`first`, `contains`), `Appendable[T]` (`push`)

**Implementations:** Generic `Sized` and `Linear` implementations for `list<T>` and `option<T>`.

---

## dataframe

Column-based structured tabular storage (mini pandas) with contiguous flat data storage in `list<f64>`.

```iris
bring std.dataframe

val df0 = df_new(list("x", "y"))
val df1 = df_add_row(df0, list(1.0, 2.0))
val df2 = df_add_row(df1, list(3.0, 4.0))
val n   = df_len(df2)                   // 2
val xs  = df_col(df2, "x")              // list(1.0, 3.0)
val avg = df_mean(df2, "y")             // 3.0
```

**Records:** `DataFrame { columns: list<str>, data: list<f64>, n_rows: i64, n_cols: i64 }`

**Functions:** `df_new`, `df_add_row`, `df_len`, `df_col_idx`, `df_col`, `df_row`, `df_get`, `df_mean`, `df_sum`, `df_filter`

---

## dataset

Machine learning dataset preprocessing, statistical summaries, and feature normalization for numerical data vectors.

```iris
bring std.dataset

val raw = list(10.0, 20.0, 30.0, 40.0, 50.0)
val m   = dataset_mean(raw)             // 30.0
val s   = dataset_std(raw)
val nrm = dataset_normalize(raw)        // zero-mean, unit-variance
val pair = dataset_split(raw, 0.8)      // (train_set, test_set)
```

**Functions:** `dataset_mean`, `dataset_std`, `dataset_normalize`, `dataset_split`, `dataset_batch`, `dataset_min`, `dataset_max`, `dataset_minmax_scale`, `dataset_shuffle`

---

## http_server

HTTP/1.1 server primitives supporting single-threaded blocking and multi-threaded connection handling.

```iris
bring std.http_server
bring std.http

def handle(req: str) -> str {
    val path = http_request_path(req)
    if path == "/health" {
        return http_response(200, "OK", "{\"status\":\"ok\"}")
    } else {
        return http_response(404, "Not Found", "not found")
    }
}

// Single-threaded server loop:
// http_serve(8080, |req: str| handle(req))

// Multi-threaded worker pool server loop:
// http_serve_mt(8080, |req: str| handle(req))
```

**Functions:**
- `http_serve(port: i64, handler: |str| -> str) -> i64 effect net`
- `http_serve_mt(port: i64, handler: |str| -> str) -> i64 effect net, thread`

---

## meta_learning

Adaptive meta-learning and Model-Agnostic Meta-Learning (MAML) style few-shot adaptation routines.

```iris
bring std.meta_learning
bring std.adaptive

val task = few_shot_task(support_x, support_y, query_x, query_y)
val h = adaptive_new("meta_agent", 4, 0.01, 0.1)
maml_inner_update(h, support_x, support_y, 5, 4, 0.01);
val q_loss = maml_eval(h, query_x, query_y, 5, 4)
```

**Types:** `FewShotTask = (list<f64>, list<f64>, list<f64>, list<f64>)`

**Functions:** `few_shot_task`, `maml_inner_update`, `maml_eval`, `meta_loss`

---

## reflect

In-process reflection, program introspection, and dynamic code evaluation.

```iris
bring std.reflect

if reflect_available() {
    val res = reflect_eval_i64("def main() -> i64 { 21 * 2 }")
    when res {
        ok(v)  => print("Evaluated: " + to_str(v)),
        err(e) => print("Failed: " + e)
    }
}
```

**Functions:** `reflect_available`, `reflect_eval`, `reflect_eval_i64`, `reflect_validate`

---

## serial

Line-oriented serial I/O communication for microcontrollers and peripheral sensor buses.

```iris
bring std.serial

val port = serial_open("COM3", 115200)
serial_write_line(port, "PING\n");
val line = serial_read_line(port)
serial_close(port);
```

**Records:** `Serial { file: i64, port: str, baud: i64 }`

**Functions:** `serial_open`, `serial_close`, `serial_read_line`, `serial_write_line`, `serial_configure`, `serial_encode_packet`, `serial_decode_packet`

---

## speculation

Speculative execution, invariant sandboxing, and reversible transactional rollbacks for safety-critical control pipelines.

```iris
bring std.speculation

val candidate = || { 40 + 2 }
val validator = |x: i64| { x > 0 && x <= 100 }
val result = speculate(candidate, validator) // some(42)

val tx_res = speculate_transactional(validator, || {
    // Reversible allocations and staged writes
    42
})
```

**Records:** `SpeculationOutcome[T] { success: bool, value: option<T>, reason: str }`

**Functions:** `speculate`, `speculate_transactional`, `speculate_with_fallback`

---

## sql

Embedded SQLite connector helpers and tabular query extraction for supervised learning.

```iris
bring std.sql

val db = sql_open("telemetry.db")
val _  = sql_exec(db, "CREATE TABLE IF NOT EXISTS log (id INT, val REAL);")
val rows = sql_query(db, "SELECT * FROM log;")
sql_close(db);
```

**Functions:** `sql_open`, `sql_exec`, `sql_query`, `sql_query_params`, `sql_exec_params`, `sql_close`, `sql_query_xy_params`

---

## svg

Lightweight vector graphics generation in XML SVG format.

```iris
bring std.svg

var ctx = svg_new(200, 200)
ctx = svg_rect(ctx, 10.0, 10.0, 50.0, 50.0, "#ff0000");
ctx = svg_circle(ctx, 100.0, 100.0, 30.0, "#00ff00");
val xml = svg_finish(ctx)
```

**Records:** `SVGContext { width: i64, height: i64, content: str }`

**Functions:** `svg_new`, `svg_rect`, `svg_circle`, `svg_line`, `svg_text`, `svg_finish`, `svg_save`

---

## table

In-memory relational table manipulation with column schemas and tab-encoded rows.

```iris
bring std.table

var t = table_new(list("id", "status"))
t = table_insert(t, list("1", "active"));
t = table_insert(t, list("2", "pending"));
val len = table_len(t)                  // 2
val row = table_get_row(t, 0)           // list("1", "active")
```

**Records:** `Table { columns: list<str>, rows: list<str> }`

**Functions:** `table_new`, `table_insert`, `table_len`, `table_col_idx`, `table_get_row`, `table_get`, `table_select`, `table_filter`, `table_encode_row`, `table_decode_row`

---

## termplot

ASCII text-mode plotting for terminal diagnostics and console visualization.

```iris
bring std.termplot

val vals = list(1.0, 3.5, 7.2, 5.0, 2.1)
term_plot_bar(vals, 20, "Telemetry Distribution");
```

**Functions:** `term_plot_bar`

---

## uncertainty

Confidence estimation, Bayesian updating, and uncertainty quantification for adaptive control systems.

```iris
bring std.uncertainty

val prior = nig_default()
val post1 = nig_update(prior, 12.5)
val mean  = nig_mean(post1)
val var_e = nig_predictive_variance(post1)
```

**Types:** `NormalInvGamma = (f64, f64, f64, f64)`

**Functions:** `nig_prior`, `nig_default`, `nig_update`, `nig_update_batch`, `nig_mean`, `nig_variance`, `nig_predictive_variance`, `nig_confidence`, `entropy_shannon`, `epistemic_uncertainty`

---

## unicode

Unicode UTF-8 codepoint decoding, character properties, and multibyte string utilities.

```iris
bring std.unicode

val text = "hello 🌍"
val count = codepoint_count(text)       // 7
val first_cp = decode_at(text, 0)       // (104, 1)
```

**Functions:** `is_continuation`, `codepoint_count`, `decode_at`, `codepoint_to_utf8`, `is_combining`, `is_cjk`, `codepoints`

---

## Production lifecycle additions

`std.ml` adds `ModelSession`, `ModelHealth`, and `ModelRegistry` with versioned
backend discovery, open/run/batch/train/close, request/failure counts, mean
latency and health degradation. `std.ais` adds `AgentRuntime`, `AgentStep`,
`agent_record_outcome`, and `agent_production_step`; repeated failures or
homeostatic risk force the caller-provided emergency action.
