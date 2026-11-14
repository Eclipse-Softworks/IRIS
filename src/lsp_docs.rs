//! Hover documentation for IRIS keywords, types, literals and contextual
//! words, used by the language server.
//!
//! Every entry with an `example` is a complete program that asserts its own
//! result; the tests at the bottom compile and run each one in the
//! interpreter, so documentation that drifts from the language fails CI
//! instead of misleading users.

/// One documented word.
pub(crate) struct Doc {
    pub name: &'static str,
    /// "keyword", "type", "literal", or "contextual keyword".
    pub kind: &'static str,
    /// One-line summary.
    pub summary: &'static str,
    /// Explanation in Markdown: semantics, rules, pitfalls.
    pub detail: &'static str,
    /// Idiomatic usage advice, if any.
    pub idiom: Option<&'static str>,
    /// A complete program that asserts what it demonstrates.
    pub example: Option<&'static str>,
}

/// Markdown hover text for `name`, if it is documented.
pub(crate) fn hover(name: &str) -> Option<String> {
    let doc = DOCS.iter().find(|doc| doc.name == name)?;
    let mut text = format!("**`{}`** · *{}* — {}", doc.name, doc.kind, doc.summary);
    if !doc.detail.is_empty() {
        text.push_str("\n\n");
        text.push_str(doc.detail);
    }
    if let Some(idiom) = doc.idiom {
        text.push_str("\n\n**Idiomatic:** ");
        text.push_str(idiom);
    }
    if let Some(example) = doc.example {
        text.push_str("\n\n```iris\n");
        text.push_str(example.trim());
        text.push_str("\n```");
    }
    Some(text)
}

const DOCS: &[Doc] = &[
    // ── Declarations ────────────────────────────────────────────────────────
    Doc {
        name: "def",
        kind: "keyword",
        summary: "define a function",
        detail: "`def name(param: Type, ...) -> Ret { body }`. Use `return expr` to return a value \
(tail expressions without `;` also work). Optional parts: generic parameters `def f[T](x: T) -> T`, \
an effect clause `-> i64 effect io`, `pub` to export, `async`, and `const def` for \
compile-time evaluation.",
        idiom: Some("use explicit `return` for function return values (`return value`); expression tails work inside blocks and closures."),
        example: Some(
            r#"
def add(a: i64, b: i64) -> i64 { return a + b }

def main() -> i64 {
    assert(add(2, 3) == 5);
    return 0
}
"#,
        ),
    },
    Doc {
        name: "pub",
        kind: "keyword",
        summary: "export an item to other files and modules",
        detail: "Applies to `def`, `record`, `choice`, `const`, `trait` and items inside `mod`. \
Items without `pub` are private to their file once it is brought into another.",
        idiom: None,
        example: Some(
            r#"
mod geometry {
    pub def area(w: i64, h: i64) -> i64 { return w * h }
}

bring geometry

def main() -> i64 {
    assert(area(3, 4) == 12);
    return 0
}
"#,
        ),
    },
    Doc {
        name: "record",
        kind: "keyword",
        summary: "define a struct type with named fields",
        detail: "`record Name { field: Type, ... }`. Construct with `Name { field: value }` and read \
with `value.field`. Fields may have defaults (`port: i64 = 8080`), which a literal may omit. \
Records are generic with `record Box[T] { value: T }`. Records are heap values, so they are not \
allowed in allocation-free (embedded / `--require-allocation-free`) code.",
        idiom: Some("give configuration records defaults and override only what differs: `Config { port: 443 }`."),
        example: Some(
            r#"
record Config {
    host: str = "localhost",
    port: i64 = 8080,
}

def main() -> i64 {
    val c: Config = Config { port: 443 };
    assert(c.port == 443);
    assert(c.host == "localhost");
    return 0
}
"#,
        ),
    },
    Doc {
        name: "choice",
        kind: "keyword",
        summary: "define an enum (sum type) with named variants",
        detail: "`choice Name { A, B, C }`; construct a variant with `Name.A` and branch on it with \
`when`. The compiler checks that `when` covers every variant.",
        idiom: Some("model states and modes as a `choice` instead of integer codes, so missing cases are compile errors."),
        example: Some(
            r#"
choice Mode { Idle, Heating, Cooling }

def power(m: Mode) -> i64 {
    return when m {
        Mode.Idle => 0,
        Mode.Heating => 100,
        Mode.Cooling => 60,
    }
}

def main() -> i64 {
    assert(power(Mode.Heating) == 100);
    assert(power(Mode.Idle) == 0);
    return 0
}
"#,
        ),
    },
    Doc {
        name: "const",
        kind: "keyword",
        summary: "compile-time constant, or a compile-time function with `const def`",
        detail: "`const NAME: Type = expr` at top level (no trailing `;`). `const def f(...)` marks a \
function that may be evaluated at compile time.",
        idiom: Some("name constants in `UPPER_SNAKE_CASE` and give them an explicit type."),
        example: Some(
            r#"
const MAX_SPEED: i64 = 120

def main() -> i64 {
    assert(MAX_SPEED * 2 == 240);
    return 0
}
"#,
        ),
    },
    Doc {
        name: "type",
        kind: "keyword",
        summary: "type alias",
        detail: "`type Name = ExistingType`. An alias is interchangeable with the type it names; it \
documents intent without creating a new type. Inside a `trait`, `type` declares an associated type.",
        idiom: None,
        example: Some(
            r#"
type Meters = f64

def double(d: Meters) -> Meters { return d * 2.0 }

def main() -> i64 {
    assert(double(1.5) == 3.0);
    return 0
}
"#,
        ),
    },
    Doc {
        name: "trait",
        kind: "keyword",
        summary: "define an interface of methods",
        detail: "`trait Name { def method(self) -> T }`. Types implement it with `impl Name for Type`. \
Trait methods are called with method syntax `value.method()`. Generic code can require a trait \
with a bound: `impl[T where T: Show] Describable for T`.",
        idiom: None,
        example: Some(
            r#"
trait Shape {
    def area(self) -> i64
}

record Square { side: i64 }

impl Shape for Square {
    def area(self) -> i64 { return self.side * self.side }
}

def main() -> i64 {
    val s: Square = Square { side: 4 };
    assert(s.area() == 16);
    return 0
}
"#,
        ),
    },
    Doc {
        name: "impl",
        kind: "keyword",
        summary: "implement a trait for a type",
        detail: "`impl Trait for Type { def method(self) -> T { ... } }`. Blanket implementations \
for every type meeting a bound use `impl[T where T: Bound] Trait for T`.",
        idiom: Some("for a single helper on one type, an extension method (a plain `def` whose first parameter is that type) is lighter than a trait."),
        example: None,
    },
    Doc {
        name: "extern",
        kind: "keyword",
        summary: "declare a function implemented outside IRIS",
        detail: "`extern def name(args) -> Ret`, optionally with an effect contract (`effect ffi`). \
Under `--strict-effects` an extern without an effect clause is an error, since a host call is \
never assumed pure. An extern call can also be intercepted by an effect handler (`handle ... with`).",
        idiom: None,
        example: Some(
            r#"
extern def lookup(key: str) -> str

def main() -> i64 {
    val v = handle { lookup("mode") } with {
        lookup(k) => "simulated"
    };
    assert(v == "simulated");
    return 0
}
"#,
        ),
    },
    Doc {
        name: "mod",
        kind: "keyword",
        summary: "define an inline module",
        detail: "`mod name { pub def ... }` groups items under a namespace in the same file. Bring it \
into scope with `bring name`, then call its public items directly.",
        idiom: None,
        example: Some(
            r#"
mod arithmetic {
    pub def triple(v: i64) -> i64 { return v * 3 }
}

bring arithmetic

def main() -> i64 {
    assert(triple(14) == 42);
    return 0
}
"#,
        ),
    },
    Doc {
        name: "bring",
        kind: "keyword",
        summary: "import a module",
        detail: "`bring std.math` (standard library), `bring \"sensor.iris\"` (a file, relative to \
this one) or `bring name` (an inline `mod`). Only `pub` items are visible; module dependency \
cycles are rejected. A brought module's functions can also be called qualified: `utils.fn()`.",
        idiom: None,
        example: None,
    },
    Doc {
        name: "defmacro",
        kind: "keyword",
        summary: "define a syntactic macro",
        detail: "`defmacro name(params) => expansion`, invoked as `name!(args)`. Expansion happens \
before type checking, so arguments are substituted as syntax.",
        idiom: Some("prefer an ordinary (or `const`) function unless you need to manipulate syntax."),
        example: Some(
            r#"
defmacro double(x) => x * 2

def main() -> i64 {
    val v: i64 = double!(21);
    assert(v == 42);
    return 0
}
"#,
        ),
    },
    Doc {
        name: "model",
        kind: "keyword",
        summary: "declare a neural-network graph",
        detail: "Inside `model Name { ... }`, `input`, `layer` and `output` are keywords:\n\n\
```iris\nmodel Classifier {\n    input features : tensor<f32, [1, 4]>\n    layer hidden Dense(units = 8)\n    \
layer activated ReLU(hidden)\n    output activated\n}\n```\n\n\
Inspect with `iris --emit graph`; export with `iris --emit onnx-binary`.",
        idiom: None,
        example: None,
    },
    // ── Bindings ────────────────────────────────────────────────────────────
    Doc {
        name: "val",
        kind: "keyword",
        summary: "immutable binding",
        detail: "`val name = expr` or `val name: Type = expr`. The type is inferred unless \
annotated; a binding cannot be reassigned. An annotation also guides constructors such as \
`val xs: list<f64> = list()`.",
        idiom: Some("default to `val`; reach for `var` only when the value really changes."),
        example: Some(
            r#"
def main() -> i64 {
    val answer = 42;
    val ratio: f64 = 0.5;
    assert(answer == 42);
    assert(ratio == 0.5);
    return 0
}
"#,
        ),
    },
    Doc {
        name: "var",
        kind: "keyword",
        summary: "mutable binding, or program state at the top level",
        detail: "`var name = expr`, reassigned with `name = expr`. Assigning an enclosing `var` \
from inside `when` arms is a known limitation; make the `when` an expression instead. At the \
top level, `var name: Type = expr` declares program state: one variable per running program, \
initialised before `main`, readable and writable from every function, and carried over to the \
next generation when the program is evolved. The type is required there.",
        idiom: Some("`val x = when ... { a => 1, _ => 2 }` rather than assigning a `var` in each arm."),
        example: Some(
            r#"
def main() -> i64 {
    var total = 0;
    for i in 1..4 {
        total = total + i;
    };
    assert(total == 6);
    return 0
}
"#,
        ),
    },
    Doc {
        name: "let",
        kind: "keyword",
        summary: "pattern binding in `if let` / `while let`",
        detail: "`if let some(v) = maybe { ... } else { ... }` runs the first block only when the \
pattern matches, binding `v`. `while let` loops while it matches.",
        idiom: Some("use `if let` for a single optional case; use `when` when several cases matter."),
        example: Some(
            r#"
def describe(maybe: option<i64>) -> i64 {
    return if let some(v) = maybe { v } else { -1 }
}

def main() -> i64 {
    assert(describe(some(7)) == 7);
    assert(describe(none()) == -1);
    return 0
}
"#,
        ),
    },
    Doc {
        name: "move",
        kind: "keyword",
        summary: "transfer ownership of a value",
        detail: "`val b = move a` hands `a`'s value to `b`; the borrow checker rejects later uses of `a`.",
        idiom: None,
        example: Some(
            r#"
def main() -> i64 {
    val a: list<i64> = list(1, 2, 3);
    val b = move a;
    assert(list_len(b) == 3);
    return 0
}
"#,
        ),
    },
    // ── Control flow ───────────────────────────────────────────────────────
    Doc {
        name: "if",
        kind: "keyword",
        summary: "conditional expression",
        detail: "`if cond { a } else { b }` is an expression: both branches produce a value of the \
same type. Used as a statement followed by more code, end it with `;`.",
        idiom: Some("bind the result directly: `val sign = if x < 0 { -1 } else { 1 };`."),
        example: Some(
            r#"
def sign(x: i64) -> i64 {
    return if x < 0 { -1 } else { if x == 0 { 0 } else { 1 } }
}

def main() -> i64 {
    assert(sign(-5) == -1);
    assert(sign(0) == 0);
    assert(sign(9) == 1);
    return 0
}
"#,
        ),
    },
    Doc {
        name: "else",
        kind: "keyword",
        summary: "alternative branch of `if` / `if let`",
        detail: "Required when the `if` is used as a value.",
        idiom: None,
        example: None,
    },
    Doc {
        name: "when",
        kind: "keyword",
        summary: "pattern match expression",
        detail: "`when value { pattern => result, ... }`. Patterns: literals, ranges `1..=9`, \
`_`, enum variants `Mode.Idle`, alternatives `A | B`, record patterns `Point { x: 0, y }`, \
`some(v)` / `none`, `ok(v)` / `err(e)`, list patterns `[a, ..rest]`, and guards `p if cond`. \
Matches on a `choice` must be exhaustive. `match` is a synonym.",
        idiom: Some("use `when` as an expression and let each arm yield the value."),
        example: Some(
            r#"
def bucket(n: i64) -> str {
    return when n {
        0 => "zero",
        1..=9 => "small",
        _ => "large",
    }
}

def main() -> i64 {
    assert(bucket(0) == "zero");
    assert(bucket(5) == "small");
    assert(bucket(50) == "large");
    return 0
}
"#,
        ),
    },
    Doc {
        name: "match",
        kind: "keyword",
        summary: "synonym for `when`",
        detail: "Identical to `when`; IRIS code conventionally uses `when`.",
        idiom: Some("write `when`."),
        example: None,
    },
    Doc {
        name: "for",
        kind: "keyword",
        summary: "range or collection loop",
        detail: "`for i in start..end { }` iterates the half-open range `[start, end)`; add `by step` \
for a stride. `for x in collection { }` iterates a list. Loops may be labelled: \
`for outer i in 0..n { ... break outer; }`. `par for` runs iterations in parallel.",
        idiom: None,
        example: Some(
            r#"
def main() -> i64 {
    var evens = 0;
    for i in 0..10 by 2 {
        evens = evens + 1;
    };
    assert(evens == 5);
    return 0
}
"#,
        ),
    },
    Doc {
        name: "in",
        kind: "keyword",
        summary: "the source of a `for` loop",
        detail: "`for i in 0..n` or `for x in xs`.",
        idiom: None,
        example: None,
    },
    Doc {
        name: "by",
        kind: "keyword",
        summary: "step of a range loop",
        detail: "`for i in 0..10 by 3` visits 0, 3, 6, 9.",
        idiom: None,
        example: None,
    },
    Doc {
        name: "while",
        kind: "keyword",
        summary: "loop while a condition holds",
        detail: "`while cond { }`; `while let pattern = value { }` loops while the pattern matches.",
        idiom: None,
        example: Some(
            r#"
def main() -> i64 {
    var n = 1;
    while n < 100 {
        n = n * 2;
    };
    assert(n == 128);
    return 0
}
"#,
        ),
    },
    Doc {
        name: "loop",
        kind: "keyword",
        summary: "infinite loop, exited with `break`",
        detail: "`loop { ... }`, optionally labelled: `loop scan { ... break scan; }`.",
        idiom: None,
        example: Some(
            r#"
def main() -> i64 {
    var i = 0;
    loop {
        i = i + 1;
        if i == 3 { break };
    };
    assert(i == 3);
    return 0
}
"#,
        ),
    },
    Doc {
        name: "break",
        kind: "keyword",
        summary: "exit the innermost (or labelled) loop",
        detail: "`break` or `break label`.",
        idiom: None,
        example: None,
    },
    Doc {
        name: "continue",
        kind: "keyword",
        summary: "skip to the next loop iteration",
        detail: "`continue` or `continue label`. Labelled `continue` is reliable in native builds \
only; the interpreter rejects it (known issue #17).",
        idiom: None,
        example: None,
    },
    Doc {
        name: "return",
        kind: "keyword",
        summary: "return a value from a function",
        detail: "`return expr` exits the enclosing function with `expr`. Expression tails without `;` are also supported for final block expressions.",
        idiom: Some("use explicit `return` for function return values and guard clauses (`if bad { return -1 };`)."),
        example: Some(
            r#"
def safe_div(a: i64, b: i64) -> i64 {
    if b == 0 { return 0 };
    return a / b
}

def main() -> i64 {
    assert(safe_div(10, 2) == 5);
    assert(safe_div(1, 0) == 0);
    return 0
}
"#,
        ),
    },
    Doc {
        name: "to",
        kind: "keyword",
        summary: "numeric conversion",
        detail: "`expr to Type`, e.g. `count to f64` or `x to i64` (truncates toward zero).",
        idiom: None,
        example: Some(
            r#"
def main() -> i64 {
    val n = 3;
    val half = (n to f64) / 2.0;
    assert(half == 1.5);
    assert((2.9 to i64) == 2);
    return 0
}
"#,
        ),
    },
    Doc {
        name: "defer",
        kind: "keyword",
        summary: "run an expression when the function exits",
        detail: "Deferred expressions run in reverse order on every exit path, including early \
`return`.",
        idiom: Some("pair each acquisition with a `defer` release on the next line."),
        example: Some(
            r#"
def note(log: list<str>, msg: str) -> i64 {
    list_push(log, msg);
    return 0
}

def work(log: list<str>) -> i64 {
    defer note(log, "closed");
    return 42
}

def main() -> i64 {
    val log: list<str> = list();
    assert(work(log) == 42);
    assert(list_get(log, 0) == "closed");
    return 0
}
"#,
        ),
    },
    Doc {
        name: "yield",
        kind: "keyword",
        summary: "add an element to a generator's result list",
        detail: "A function returning `list<T>` may `yield` values; the function returns every \
yielded value in order.",
        idiom: None,
        example: Some(
            r#"
def evens(max: i64) -> list<i64> {
    var i = 2;
    while i <= max {
        yield i;
        i = i + 2;
    }
}

def main() -> i64 {
    val xs = evens(6);
    assert(list_len(xs) == 3);
    assert(list_get(xs, 2) == 6);
    return 0
}
"#,
        ),
    },
    // ── Errors ──────────────────────────────────────────────────────────────
    Doc {
        name: "try",
        kind: "keyword",
        summary: "catch an `err` propagated by `?`",
        detail: "`try { ... } catch e { ... }` is an expression. Inside the block, `expr?` unwraps \
`ok(v)` or jumps to the `catch` arm with the error bound to `e`.",
        idiom: Some("return `result<T, E>` from fallible functions and use `?` to propagate; reserve `panic` for bugs."),
        example: Some(
            r#"
def safe_divide(a: i64, b: i64) -> result<i64, str> {
    return if b == 0 { err("division by zero") } else { ok(a / b) }
}

def main() -> i64 {
    val r: i64 = try {
        val v: i64 = safe_divide(10, 0)?;
        v
    } catch e {
        -1
    };
    assert(r == -1);
    return 0
}
"#,
        ),
    },
    Doc {
        name: "catch",
        kind: "keyword",
        summary: "error arm of `try`",
        detail: "`catch e { ... }` binds the propagated error.",
        idiom: None,
        example: None,
    },
    Doc {
        name: "raise",
        kind: "keyword",
        summary: "perform an effect operation",
        detail: "`raise Op(args)` invokes an operation for the nearest enclosing `handle ... with` \
to interpret. Experimental: no test or example in the repository exercises it yet.",
        idiom: None,
        example: None,
    },
    // ── Effects ─────────────────────────────────────────────────────────────
    Doc {
        name: "effect",
        kind: "keyword",
        summary: "declare what a function may do",
        detail: "`def f() -> T effect io, alloc { }`. Effects: `pure` (none), `io`, `alloc`, \
`fs`, `net`, `thread`, `ffi`, `throw`, `transaction`, … A row variable (`effect E`) makes a \
higher-order function polymorphic in its callback's effects; function types carry rows too \
(`|i64| -> i64 effect io`). With `--strict-effects` every effectful function needs a clause \
that covers its body, so `effect pure` is a checked proof of no I/O and no allocation.",
        idiom: Some("mark control-loop code `effect pure` and build with `--strict-effects`."),
        example: Some(
            r#"
def gain(error: f64) -> f64 effect pure { return 2.0 * error }

def report(x: f64) -> i64 effect io {
    println(to_str(x));
    return 0
}

def main() -> i64 effect io, throw {
    assert(gain(1.5) == 3.0);
    assert(report(gain(1.0)) == 0);
    return 0
}
"#,
        ),
    },
    Doc {
        name: "with",
        kind: "keyword",
        summary: "effect handler arms, or an effect mask",
        detail: "In `handle { body } with { op(args) => result }` it introduces the handler arms. \
As a statement, `with pure { ... }` / `with io, alloc { ... }` restricts the effects allowed \
inside the block (checked under `--strict-effects`).",
        idiom: None,
        example: None,
    },
    Doc {
        name: "handle",
        kind: "contextual keyword",
        summary: "run a block with effect handlers",
        detail: "`handle { body } with { op(args) => value }` intercepts calls to `op` (typically an \
`extern`) made in `body`. An arm written `op(args) -> resume(k) => k(value)` resumes the \
interrupted computation with `value`.",
        idiom: Some("use handlers to substitute simulated I/O in tests."),
        example: Some(
            r#"
extern def echo(s: str) -> str

def main() -> i64 {
    val result = handle { echo("hello") } with {
        echo(s) -> resume(v) => v("intercepted")
    };
    assert(result == "intercepted");
    return 0
}
"#,
        ),
    },
    Doc {
        name: "resume",
        kind: "keyword",
        summary: "continue the computation an effect handler interrupted",
        detail: "In a handler arm `op(args) -> resume(k) => k(value)`, calling `k(value)` resumes the \
handled block with `value` as the operation's result. Supported inline; a `handle` block used as a \
function's return value is a known issue (#16).",
        idiom: None,
        example: None,
    },
    Doc {
        name: "unsafe",
        kind: "keyword",
        summary: "block exempt from safety lints",
        detail: "`unsafe { ... }` is an expression yielding its tail value. The language server marks \
functions containing one in hovers.",
        idiom: Some("keep unsafe blocks small and explain why in a comment."),
        example: Some(
            r#"
def main() -> i64 {
    val v = unsafe {
        val x = 10;
        x + 20
    };
    assert(v == 30);
    return 0
}
"#,
        ),
    },
    // ── Concurrency ─────────────────────────────────────────────────────────
    Doc {
        name: "spawn",
        kind: "keyword",
        summary: "run a block on another thread",
        detail: "`spawn { ... }` starts the block concurrently. Communicate through channels \
(`channel()`, `send`, `recv`) or atomics rather than shared mutable state.",
        idiom: Some("send results back over a channel; `recv` blocks until a value arrives."),
        example: Some(
            r#"
def main() -> i64 {
    val ch = channel();
    spawn { send(ch, 41) };
    assert(recv(ch) + 1 == 42);
    return 0
}
"#,
        ),
    },
    Doc {
        name: "par",
        kind: "keyword",
        summary: "parallel loop: `par for`",
        detail: "`par for i in start..end { }` runs iterations in parallel. Combine results with an \
atomic (`atomic_add`) or per-index writes; do not assign captured `var`s.",
        idiom: None,
        example: Some(
            r#"
def main() -> i64 {
    val count = atomic(0);
    par for i in 0..100 {
        atomic_add(count, 1);
    };
    assert(atomic_load(count) == 100);
    return 0
}
"#,
        ),
    },
    Doc {
        name: "async",
        kind: "keyword",
        summary: "asynchronous function: calling it returns a `task<T>`",
        detail: "`async def f(...) -> T`. A call creates a task and schedules it on this thread's \
executor; `await` it for the `T`. The compiler turns the body into a state machine, so each \
`await` inside it suspends the task, without blocking the thread, while other tasks run. \
Tasks run whenever their thread waits: on `await`, or on a blocking `recv`. A program behaves \
the same under the interpreter and natively.",
        idiom: Some("Start several tasks first, then await them: they run concurrently."),
        example: Some(
            r#"
async def fetch(id: i64, delay: i64) -> i64 {
    await async_sleep(delay);
    return id * 10
}

def main() -> i64 {
    val a = fetch(1, 30);
    val b = fetch(2, 10);
    // Both wait at the same time: about 30 ms in total, not 40.
    assert(await a + await b == 30);
    return 0
}
"#,
        ),
    },
    Doc {
        name: "await",
        kind: "keyword",
        summary: "wait for a task's result",
        detail: "`await t` on a `task<T>` yields its `T`. Inside an `async def` it suspends the \
current task until `t` is done; elsewhere it runs this thread's tasks until then. Inside an \
`async def`, `await ch` on a `chan<T>` receives without blocking the thread.",
        idiom: None,
        example: Some(
            r#"
async def add1(x: i64) -> i64 { return x + 1 }

async def twice(x: i64) -> i64 {
    val a = await add1(x);
    return await add1(a)
}

def main() -> i64 {
    val r = await twice(40);
    assert(r == 42);
    return 0
}
"#,
        ),
    },
    Doc {
        name: "select",
        kind: "keyword",
        summary: "receive from whichever channel is ready: `select!`",
        detail: "`select! { msg = ch => { ... }, default => { ... } }` takes a ready channel's value, \
or the `default` arm if none is ready.",
        idiom: None,
        example: None,
    },
    Doc {
        name: "dyn",
        kind: "keyword",
        summary: "trait object type: `dyn Trait`",
        detail: "A value of any type implementing the trait, dispatched at run time. **Interpreter \
only:** the native backend has no implementation yet (known issues #18/#18b); prefer generics or \
a `choice` for native code.",
        idiom: None,
        example: None,
    },
    // ── Contextual words ─────────────────────────────────────────────────────
    Doc {
        name: "where",
        kind: "contextual keyword",
        summary: "trait bound on a type parameter",
        detail: "`impl[T where T: Show] Describable for T` applies only to types implementing `Show`.",
        idiom: None,
        example: None,
    },
    Doc {
        name: "self",
        kind: "contextual keyword",
        summary: "the receiver of a method",
        detail: "The first parameter of a trait or impl method; its type may be omitted.",
        idiom: None,
        example: None,
    },
    // ── Literals and constructors ───────────────────────────────────────────
    Doc {
        name: "true",
        kind: "literal",
        summary: "`bool` true",
        detail: "",
        idiom: None,
        example: None,
    },
    Doc {
        name: "false",
        kind: "literal",
        summary: "`bool` false",
        detail: "",
        idiom: None,
        example: None,
    },
    Doc {
        name: "none",
        kind: "literal",
        summary: "the empty `option<T>`",
        detail: "Write `none()`; test with `is_some` or match with `when` / `if let`.",
        idiom: None,
        example: None,
    },
    // ── Types ───────────────────────────────────────────────────────────────
    Doc {
        name: "i64",
        kind: "type",
        summary: "64-bit signed integer (the default integer type)",
        detail: "Integer literals are `i64` unless context says otherwise. Hex `0xFF` and binary \
`0b1010` literals accept `_` separators.",
        idiom: None,
        example: None,
    },
    Doc {
        name: "i32",
        kind: "type",
        summary: "32-bit signed integer",
        detail: "Convert with `to i32`.",
        idiom: None,
        example: None,
    },
    Doc {
        name: "i8",
        kind: "type",
        summary: "8-bit signed integer",
        detail: "",
        idiom: None,
        example: None,
    },
    Doc {
        name: "u8",
        kind: "type",
        summary: "8-bit unsigned integer (bytes)",
        detail: "",
        idiom: None,
        example: None,
    },
    Doc {
        name: "u32",
        kind: "type",
        summary: "32-bit unsigned integer",
        detail: "",
        idiom: None,
        example: None,
    },
    Doc {
        name: "u64",
        kind: "type",
        summary: "64-bit unsigned integer",
        detail: "",
        idiom: None,
        example: None,
    },
    Doc {
        name: "usize",
        kind: "type",
        summary: "pointer-sized unsigned integer",
        detail: "",
        idiom: None,
        example: None,
    },
    Doc {
        name: "f64",
        kind: "type",
        summary: "64-bit float (the default float type)",
        detail: "Float literals such as `2.5` are `f64`. Math builtins (`sin`, `sqrt`, `pow`, …) \
accept `f64` or `f32`.",
        idiom: None,
        example: None,
    },
    Doc {
        name: "f32",
        kind: "type",
        summary: "32-bit float",
        detail: "The element type of most tensors: `tensor<f32, [M, K]>`.",
        idiom: None,
        example: None,
    },
    Doc {
        name: "bool",
        kind: "type",
        summary: "`true` or `false`",
        detail: "Combine with `&&` and `||` (short-circuit) and `!`.",
        idiom: None,
        example: None,
    },
    Doc {
        name: "str",
        kind: "type",
        summary: "immutable UTF-8 string",
        detail: "Concatenate with `+`; interpolate with f-strings `f\"x = {x}\"`; convert values with \
`to_str`. `len` counts bytes.",
        idiom: Some("`f\"sum = {total}\"` instead of chains of `+ to_str(...)`."),
        example: Some(
            r#"
def main() -> i64 {
    val total = 7;
    val s = f"sum = {total}";
    assert(s == "sum = 7");
    return 0
}
"#,
        ),
    },
    Doc {
        name: "tensor",
        kind: "type",
        summary: "n-dimensional array with a static shape",
        detail: "`tensor<f32, [M, K]>`; dimensions are literals or symbolic names, and shapes are \
checked at compile time (including `einsum(\"mk,kn->mn\", a, b)` notation).",
        idiom: None,
        example: None,
    },
    Doc {
        name: "option",
        kind: "type",
        summary: "`option<T>`: a value or nothing",
        detail: "Construct with `some(v)` / `none()`; inspect with `is_some`, `unwrap`, `if let some(v) = x`, \
or `when`. `x ?? default` supplies a fallback.",
        idiom: Some("return `option<T>` for lookups that can miss instead of a sentinel like -1."),
        example: None,
    },
    Doc {
        name: "result",
        kind: "type",
        summary: "`result<T, E>`: success or error",
        detail: "Construct with `ok(v)` / `err(e)`; propagate with `?`; inspect with `is_ok`, `when`, \
or `try ... catch`.",
        idiom: None,
        example: None,
    },
    Doc {
        name: "chan",
        kind: "type",
        summary: "`chan<T>`: a thread-safe channel",
        detail: "Created by `channel()`; `send(ch, v)` and blocking `recv(ch)`.",
        idiom: None,
        example: None,
    },
    Doc {
        name: "task",
        kind: "type",
        summary: "`task<T>`: a running `async def` call",
        detail: "What calling an `async def` returns; `await` it for the `T`, and `task_done(t)` \
tells whether it has finished. A task runs on the thread that created it, cooperatively: it \
gives way to other tasks at each `await`. `async_sleep(ms)`, `async_yield()` and \
`async_recv(ch)` make tasks to await.",
        idiom: None,
        example: Some(
            r#"
async def leaf(i: i64) -> i64 {
    await async_yield();
    return i * 2
}

def main() -> i64 {
    val tasks: list<task<i64>> = list();
    var i = 0;
    while i < 100 {
        push(tasks, leaf(i));
        i = i + 1;
    }
    var total = 0;
    var j = 0;
    while j < 100 {
        total = total + await list_get(tasks, j);
        j = j + 1;
    }
    assert(total == 9900);
    return 0
}
"#,
        ),
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::{InterpOptions, IrValue};

    /// Every example must compile and run to completion with `main` returning
    /// 0; a failing `assert` makes the run an error.
    #[test]
    fn every_documented_example_compiles_and_its_assertions_pass() {
        let mut failures = Vec::new();
        for doc in DOCS {
            let Some(example) = doc.example else { continue };
            let result = crate::compile_to_module(example, &format!("doc_{}", doc.name))
                .map_err(|error| format!("compile: {error}"))
                .and_then(|module| {
                    let main = module
                        .function_by_name("main")
                        .ok_or_else(|| "no main".to_owned())?;
                    crate::interp::eval_function_in_module_opts(
                        &module,
                        main,
                        &[],
                        InterpOptions {
                            max_steps: 1_000_000,
                            max_depth: 250,
                        },
                    )
                    .map_err(|error| format!("run: {error}"))
                });
            match result {
                Ok(values) if matches!(values.as_slice(), [IrValue::I64(0)]) => {}
                Ok(values) => failures.push(format!("{}: main returned {values:?}", doc.name)),
                Err(error) => failures.push(format!("{}: {error}", doc.name)),
            }
        }
        assert!(
            failures.is_empty(),
            "failing doc examples:\n{}",
            failures.join("\n")
        );
    }

    #[test]
    fn every_lexer_keyword_is_documented() {
        let keywords = [
            "def", "val", "var", "let", "return", "if", "else", "while", "loop", "break",
            "continue", "record", "bring", "when", "match", "choice", "for", "in", "to", "spawn",
            "par", "async", "await", "const", "type", "trait", "impl", "pub", "extern", "model",
            "effect", "with", "dyn", "resume", "defer", "try", "catch", "raise", "move", "yield",
            "select", "mod", "unsafe", "defmacro", "by", "f32", "f64", "i32", "i64", "i8", "u8",
            "u32", "u64", "usize", "bool", "tensor", "str", "true", "false",
        ];
        let missing: Vec<_> = keywords
            .iter()
            .filter(|keyword| hover(keyword).is_none())
            .collect();
        assert!(missing.is_empty(), "undocumented keywords: {missing:?}");
    }

    #[test]
    fn names_are_unique() {
        let mut names: Vec<_> = DOCS.iter().map(|doc| doc.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "duplicate doc entries");
    }
}
