# IRIS Standard Library Reference

All stdlib modules are imported with `bring std.<module>`:

```iris
bring std.math
bring std.string
bring std.fs
```

---

## math

Numeric utilities implemented in pure IRIS.

```iris
bring std.math

val g = gcd(12, 8)           // 4
val l = lcm(4, 6)            // 12
val a = abs_i64(-5)          // 5
val c = clamp_i64(15, 0, 10) // 10
val e = is_even(4)           // true
val o = is_odd(3)            // true
```

**Functions:** `gcd`, `lcm`, `abs_i64`, `clamp_i64`, `min_i64`, `max_i64`, `sign_i64`, `is_even`, `is_odd`

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

**Functions:** `words`, `lines`, `str_join`, `is_empty`, `pad_left`, `pad_right`, `str_repeat`, `trim_start`, `trim_end`, `count`, `strip_prefix`, `strip_suffix`, `split_once`, `split_n`, `replace_n`, `normalize_whitespace`, `is_blank`

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
    grad(x)
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
    ok && ok2
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
val b64 = encode_b64("hello")           // base64 string
val raw = decode_b64(b64)               // original string
val hex = to_hex("hello")               // hex-encoded
val dec = from_hex(hex)                 // decoded
```

**Functions:** `generate_uuid`, `hash_code`, `encode_b64`, `decode_b64`, `to_hex`, `from_hex`

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

Autonomous Intelligent Systems framework for perceptions, decisions, actions, and persistence.

```iris
bring std.ais

// Run autonomous agent loop
val total_steps = agent_loop(
    || sensor_read(), 
    |obs| predict(obs), 
    |act| actuate(act), 
    1000
)

// Decision strategy sampling
val best = argmax([1.0, 2.5, 0.5])         // 1
val a1   = epsilon_greedy([1.0, 2.5], 0.1) // random or best
val a2   = softmax_sample([1.0, 2.0, 0.5])
val a3   = boltzmann_sample([1.0, 2.0], 1.5)

// Reward engineering pipelines
val returns = discount_rewards([0.0, 1.0], 0.99)
val norm_returns = normalize_input(returns)
val clipped = clip_list(norm_returns, 0.0, 1.0)

// Model persistence
val ok = model_save(weights, "weights.txt")
val loaded_weights = model_load("weights.txt")
```

**Functions:** `agent_loop`, `agent_loop_rewards`, `argmax`, `epsilon_greedy`, `softmax_sample`, `boltzmann_sample`, `discount_rewards`, `gae`, `normalize_list`, `normalize_input`, `clip_list`, `model_save`, `model_load`

---

## net

Typed TCP and UDP transport. TCP helpers return `result` values and provide
timeouts, exact writes, bounded reads, shutdown, and close.

```iris
bring std.net

val connected = tcp_connect_checked("127.0.0.1", 8080, 2000, 5000)
if is_ok(connected) {
    val stream = unwrap(connected)
    val sent = tcp_write_exact(stream, "ping")
    val reply = tcp_read_bounded(stream, 4096)
    tcp_shutdown(stream, 2);
    tcp_close_stream(stream);
}
```

**Records:** `TcpStream`, `TcpListener`

**Functions:** `tcp_connect_checked`, `tcp_listen_checked`,
`tcp_accept_checked`, `tcp_set_timeouts`, `tcp_write_exact`,
`tcp_read_bounded`, `tcp_shutdown`, `tcp_close_stream`, `tcp_close_listener`

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

**Records:** `LlmClient`, `LlmMessage`, `LlmTool`, `LlmResponse`,
`EmbeddingResponse`, `LocalModel`

**Functions:** `llm_chat`, `llm_chat_with_tools`, `llm_embed`,
`llm_request_json`, `llm_response_from_json`, `local_onnx_model`,
`local_model_run`, `local_pytorch_train_step`

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

Structured concurrency helpers and scheduler telemetry.

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

**Functions:** `new_task_group`, `cancellation_requested`,
`task_group_cancelled`, `async_worker_count`, `async_queued_tasks`,
`try_recv`, `channel_bounded`, `channel_unbounded`, `channel_len`, `delay`,
`sleep`, `num_threads`, `select_first`, `recv_timeout_ms`,
`gc_collect_sweep`, `gc_get_stats`

Native `spawn`, `spawn(group)`, and lowered `async def` work run on the bounded
executor. Cancellation is cooperative: queued grouped tasks skip their body at
entry, and long-running tasks should call `cancellation_requested()` at bounded
intervals.

---

## Production lifecycle additions

`std.ml` adds `ModelSession`, `ModelHealth`, and `ModelRegistry` with versioned
backend discovery, open/run/batch/train/close, request/failure counts, mean
latency and health degradation. `std.ais` adds `AgentRuntime`, `AgentStep`,
`agent_record_outcome`, and `agent_production_step`; repeated failures or
homeostatic risk force the caller-provided emergency action.
