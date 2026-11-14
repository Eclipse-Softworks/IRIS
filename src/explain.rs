//! `iris explain <code>`: what an error code means, an example, and the fix.
//!
//! Every code the compiler can emit has an entry; `tests/diagnostic_quality.rs`
//! scans the source for codes and fails on any without one.

struct Entry {
    code: &'static str,
    title: &'static str,
    what: &'static str,
    wrong: &'static str,
    right: &'static str,
    fix: &'static str,
}

const ENTRIES: &[Entry] = &[
    // ── syntax ──────────────────────────────────────────────────────────
    Entry {
        code: "E0001",
        title: "Unexpected Character",
        what: "The source contains a character IRIS does not use.",
        wrong: "# a comment\ndef main() -> i64 { return 0 }",
        right: "// a comment\ndef main() -> i64 { return 0 }",
        fix: "Comments start with `//`. `@` is only valid for attributes such as `@differentiable`.",
    },
    Entry {
        code: "E0002",
        title: "Unterminated String",
        what: "A string literal is opened but never closed, or a string uses single quotes.",
        wrong: "val s = 'hi'",
        right: "val s = \"hi\"",
        fix: "Strings use double quotes, and every `\"` needs a matching closing `\"` on the same line.",
    },
    Entry {
        code: "E0003",
        title: "Invalid Escape Sequence",
        what: "A backslash in a string is followed by a character that is not an escape.",
        wrong: "val p = \"C:\\data\"",
        right: "val p = \"C:\\\\data\"",
        fix: "The escapes are `\\n`, `\\t`, `\\r`, `\\\\` and `\\\"`. Write a literal backslash as `\\\\`.",
    },
    Entry {
        code: "E0004",
        title: "Invalid Literal",
        what: "A number, string or boolean literal is malformed.",
        wrong: "val x = 1.2.3",
        right: "val x = 1.23",
        fix: "Check the literal: one decimal point, digits only, `true`/`false` in lower case.",
    },
    Entry {
        code: "E0005",
        title: "Unexpected Token",
        what: "The parser found something other than what the grammar allows here. The message names what was expected; common causes are a missing `)`, `=` where `==` was meant, a habit from another language (`fn`, `elif`, `and`, `i++`), or a statement outside any function.",
        wrong: "if x = 2 { println(\"two\"); }",
        right: "if x == 2 { println(\"two\"); }",
        fix: "Read the expected token in the message; the suggested fix, when there is one, is usually it.",
    },
    Entry {
        code: "E0006",
        title: "Unclosed Delimiter",
        what: "The file ended while a `{`, `(` or `[` was still open.",
        wrong: "def main() -> i64 {\n    if true {\n        println(\"hi\");\n    return 0\n}",
        right: "def main() -> i64 {\n    if true {\n        println(\"hi\");\n    }\n    return 0\n}",
        fix: "Close the delimiter the message points at. Indentation usually shows where the `}` belongs.",
    },
    Entry {
        code: "E0007",
        title: "Nesting Too Deep",
        what: "Expressions or blocks are nested deeper than the parser allows.",
        wrong: "val x = ((((((((((((1))))))))))))  // hundreds deep",
        right: "val inner = compute()\nval x = wrap(inner)",
        fix: "Split the expression into named intermediate values.",
    },
    // ── names, calls and types (lowering) ───────────────────────────────
    Entry {
        code: "E0100",
        title: "Undefined Variable or Function",
        what: "A name is used that is not declared in scope. It may be misspelled, declared later in a block, private to another module, or a name from another language (`range`, `null`, `True`).",
        wrong: "val total = 3\nreturn totl",
        right: "val total = 3\nreturn total",
        fix: "Use the suggested name, declare it before use, or `bring` the module that defines it (and mark it `pub` there).",
    },
    Entry {
        code: "E0101",
        title: "Type Mismatch",
        what: "Two values that must have the same type do not: the sides of an operator, or a value and the type it is assigned to. IRIS does not convert implicitly.",
        wrong: "val s = \"n = \" + 3\nval x: f64 = 1 / 2.0",
        right: "val s = \"n = \" + to_str(3)\nval x: f64 = 1.0 / 2.0",
        fix: "Convert explicitly: `to_str(x)`, `x to f64`, or write the literal in the type you need (`1.0`).",
    },
    Entry {
        code: "E0102",
        title: "Duplicate Definition",
        what: "Two functions in one module have the same name.",
        wrong: "def area(r: f64) -> f64 { return r * r }\ndef area(w: f64, h: f64) -> f64 { return w * h }",
        right: "def circle_area(r: f64) -> f64 { return r * r }\ndef rect_area(w: f64, h: f64) -> f64 { return w * h }",
        fix: "Rename one of them; IRIS has no overloading by parameter list.",
    },
    Entry {
        code: "E0103",
        title: "Unsupported Construct",
        what: "The program is valid IRIS, but this compiler does not implement the construct in this position yet: an empty `[]` literal, some patterns inside `|` alternatives, and backend-specific operations listed in docs/known-issues.md.",
        wrong: "(depends on the construct named in the message)",
        right: "(rewrite with the alternative named in the message)",
        fix: "Use the alternative the message suggests; docs/known-issues.md lists current limits.",
    },
    Entry {
        code: "E0104",
        title: "Undefined Layer",
        what: "A `model` refers to a layer or input that is not defined before it.",
        wrong: "model M { input x: tensor<f32, [4]>\n  y = dense(z, 8) }",
        right: "model M { input x: tensor<f32, [4]>\n  y = dense(x, 8) }",
        fix: "Define the layer first, or fix the name.",
    },
    Entry {
        code: "E0105",
        title: "Duplicate Node",
        what: "Two nodes in a `model` have the same name.",
        wrong: "h = dense(x, 8)\nh = relu(h)",
        right: "h = dense(x, 8)\na = relu(h)",
        fix: "Give each node its own name.",
    },
    Entry {
        code: "E0106",
        title: "Invalid Layer Parameter",
        what: "A `model` layer was given a hyperparameter it does not accept, or an invalid value.",
        wrong: "y = dense(x, -8)",
        right: "y = dense(x, 8)",
        fix: "Check the layer's parameters in docs/SPEC.md.",
    },
    Entry {
        code: "E0107",
        title: "Unknown Model Operation",
        what: "A `model` uses an operation with no shape rule.",
        wrong: "y = frobnicate(x)",
        right: "y = relu(x)",
        fix: "Use one of the operations listed in docs/SPEC.md.",
    },
    Entry {
        code: "E0108",
        title: "Rejected Construct",
        what: "The program is well-formed but breaks a rule the language enforces on purpose, such as sharing mutable state with a spawned task.",
        wrong: "var n = 0\nspawn { n = n + 1 }",
        right: "val c: chan<i64> = chan()\nspawn { send(c, 1) }",
        fix: "Follow the rule in the message; for concurrency, communicate through channels.",
    },
    Entry {
        code: "E0109",
        title: "Wrong Number of Arguments",
        what: "A function is called with more or fewer arguments than it declares (parameters with defaults may be left out).",
        wrong: "def add(a: i64, b: i64) -> i64 { return a + b }\nval x = add(1)",
        right: "val x = add(1, 2)",
        fix: "Pass one argument per parameter; the note points at the declaration.",
    },
    Entry {
        code: "E0110",
        title: "Argument Type Mismatch",
        what: "An argument's type is not the type of the parameter it is passed to.",
        wrong: "def add(a: i64, b: i64) -> i64 { return a + b }\nadd(\"one\", 2)",
        right: "add(1, 2)",
        fix: "Pass a value of the declared type, converting explicitly if needed (`parse_i64`, `to_str`, `x to f64`).",
    },
    Entry {
        code: "E0111",
        title: "Unknown Type",
        what: "A type name is not a built-in type, record, choice or type alias in scope.",
        wrong: "val x: integer = 1\ndef greet(name: String) -> i64 { return 0 }",
        right: "val x: i64 = 1\ndef greet(name: str) -> i64 { return 0 }",
        fix: "The built-in types are i8..i64, u8..u64, f32, f64, bool, str, list<T>, map<K, V>, option<T>, result<T, E>, chan<T>, task<T> and unit.",
    },
    Entry {
        code: "E0112",
        title: "Assignment to an Immutable Binding",
        what: "A name declared with `val` is assigned again. `val` bindings cannot change.",
        wrong: "val x = 1\nx = 2;",
        right: "var x = 1\nx = 2;",
        fix: "Declare it with `var` if it should change, or bind the new value to a new name.",
    },
    Entry {
        code: "E0113",
        title: "No Such Field",
        what: "A record is read or built with a field it does not have.",
        wrong: "record Point { x: i64, y: i64 }\np.z",
        right: "p.x",
        fix: "Use one of the fields the note lists.",
    },
    Entry {
        code: "E0114",
        title: "Missing Field",
        what: "A record literal leaves out a field. Every field must be given a value.",
        wrong: "Point { x: 1 }",
        right: "Point { x: 1, y: 0 }",
        fix: "Add the missing fields, or copy the rest from another value with `..other`.",
    },
    Entry {
        code: "E0115",
        title: "Unknown Module",
        what: "`bring` names a standard-library module that does not exist, or a file that cannot be found.",
        wrong: "bring std.mathz",
        right: "bring std.math",
        fix: "Use the suggested module; docs/stdlib-reference.md lists them. A file path is relative to the bringing file.",
    },
    Entry {
        code: "E0116",
        title: "Missing Return",
        what: "A function with a return type can reach its end without returning a value, typically an `if` without `else`.",
        wrong: "def sign(x: i64) -> i64 {\n    if x > 0 { return 1 }\n}",
        right: "def sign(x: i64) -> i64 {\n    if x > 0 { return 1 }\n    return 0\n}",
        fix: "Return a value on every path: add a final `return`, or an `else` branch.",
    },
    Entry {
        code: "E0117",
        title: "No Such Method",
        what: "A method is called on a type that has no method of that name. Built-in types are used through functions (`len(s)`), not methods.",
        wrong: "s.size()\nxs.length()",
        right: "len(s)\nlist_len(xs)",
        fix: "Call the function the message suggests, or define the method in an `impl` block for your record.",
    },
    Entry {
        code: "E0118",
        title: "Not a Function",
        what: "A value that is not a function or closure is called.",
        wrong: "val x = 3\nx(1)",
        right: "val f = |n: i64| n + 3\nf(1)",
        fix: "Call a function or closure; check the name is not shadowed by a variable.",
    },
    Entry {
        code: "E0119",
        title: "`if` Without `else` Used as a Value",
        what: "An `if` without `else` has no value when its condition is false, so it cannot be bound or returned.",
        wrong: "val x = if ready { 1 }",
        right: "val x = if ready { 1 } else { 0 }",
        fix: "Add an `else` branch with the value for the other case.",
    },
    Entry {
        code: "E0122",
        title: "No Such Variant",
        what: "A choice is used with a variant name it does not declare.",
        wrong: "choice Color { Red, Green }\nColor.Blue",
        right: "Color.Green",
        fix: "Use one of the variants the choice declares.",
    },
    Entry {
        code: "E0123",
        title: "`break` or `continue` Out of Place",
        what: "`break` or `continue` is used outside any loop, or names a label no enclosing loop has.",
        wrong: "def f() -> i64 { break; return 0 }",
        right: "for i in 0..10 { if i == 3 { break } }",
        fix: "Use it inside a loop; a label must name an enclosing loop (`for outer i in ... { break outer }`).",
    },
    Entry {
        code: "E0124",
        title: "Operation Not Defined for This Type",
        what: "An operation is applied to a kind of value that does not have it: a field of something that is not a record, `.0` of something that is not a tuple, a variant pattern on something that is not a choice, `for` over a number.",
        wrong: "val n = 3\nn.x",
        right: "val p = Point { x: 3, y: 4 }\np.x",
        fix: "Check the value's type: the message names it.",
    },
    Entry {
        code: "E0125",
        title: "Wrong Number of Elements",
        what: "A tuple index or a destructuring does not match the number of elements the value has.",
        wrong: "val (a, b, c) = (1, 2)",
        right: "val (a, b) = (1, 2)",
        fix: "Bind one name per element, and index within the tuple's length.",
    },
    Entry {
        code: "E0126",
        title: "Format String Mismatch",
        what: "A format string must be a literal, and its `{}` holes must match the values given.",
        wrong: "format(\"{} and {}\", a)",
        right: "format(\"{} and {}\", a, b)",
        fix: "Give one value per `{}`, and write the format string literally.",
    },
    Entry {
        code: "E0127",
        title: "Malformed `when`",
        what: "A `when` has no arms, or an option or result `when` lacks the arms its type needs.",
        wrong: "when maybe { some(v) => v }",
        right: "when maybe { some(v) => v, none => 0 }",
        fix: "Handle every case: `some`/`none` for an option, `ok`/`err` for a result.",
    },
    Entry {
        code: "E0128",
        title: "Cannot Assign to This",
        what: "The left of `=` is not something that can be assigned: a call, a literal, or a field of a temporary value.",
        wrong: "f().x = 1",
        right: "var p = f()\np.x = 1",
        fix: "Assign to a variable, an index, a field of a variable, or an element of a tuple variable.",
    },
    Entry {
        code: "E0129",
        title: "Does Not Implement the Trait",
        what: "A value is used as `dyn Trait`, but its type has no `impl Trait for Type`.",
        wrong: "trait Shape { def area(self) -> f64 }\nval s: dyn Shape = Point { x: 0, y: 0 }",
        right: "impl Shape for Point { def area(self) -> f64 { return 0.0 } }",
        fix: "Implement the trait for the type, or pass a value of a type that implements it.",
    },
    // ── type checking (passes) ─────────────────────────────────────────
    Entry {
        code: "E0200",
        title: "Use Before Definition",
        what: "A value is used before it has been assigned. When this names no variable of yours it is a compiler defect.",
        wrong: "(rare; usually reported as E0900)",
        right: "",
        fix: "Assign the variable before using it; if the message names no variable you wrote, run `iris bug-report`.",
    },
    Entry {
        code: "E0201",
        title: "Multiple Definition",
        what: "A value was defined twice in the compiler's intermediate form. This is a compiler defect.",
        wrong: "",
        right: "",
        fix: "Run `iris bug-report <file.iris>` and report it.",
    },
    Entry {
        code: "E0202",
        title: "Type Error",
        what: "Type checking found values whose types cannot agree.",
        wrong: "val xs: list<i64> = list()\npush(xs, \"a\");",
        right: "val xs: list<str> = list()\npush(xs, \"a\");",
        fix: "Make the types agree, converting explicitly where needed.",
    },
    Entry {
        code: "E0203",
        title: "Missing Terminator",
        what: "A block in the compiler's intermediate form has no ending. This is a compiler defect.",
        wrong: "",
        right: "",
        fix: "Run `iris bug-report <file.iris>` and report it.",
    },
    Entry {
        code: "E0204",
        title: "Shape Mismatch",
        what: "Tensor dimensions in an operation are incompatible.",
        wrong: "matmul(a /* [2,3] */, b /* [2,3] */)",
        right: "matmul(a /* [2,3] */, transpose(b) /* [3,2] */)",
        fix: "Check the shapes; inner dimensions of a matmul must match.",
    },
    Entry {
        code: "E0205",
        title: "Unresolved Type",
        what: "Nothing in the program determines a value's type.",
        wrong: "val xs = list()\nreturn list_get(xs, 0)",
        right: "val xs: list<i64> = list()",
        fix: "Annotate the binding.",
    },
    Entry {
        code: "E0206",
        title: "Compile-time Evaluation Error",
        what: "A `const def` call evaluated at compile time failed.",
        wrong: "const N: i64 = div(1, 0)",
        right: "const N: i64 = div(1, 1)",
        fix: "Fix the computation, or call the function at run time instead.",
    },
    Entry {
        code: "E0207",
        title: "Type Annotation Needed",
        what: "A value is used, but nothing in the program says what type it has.",
        wrong: "val xs = list()\nprintln(list_get(xs, 0));",
        right: "val xs: list<str> = list()",
        fix: "Annotate the binding or the collection.",
    },
    Entry {
        code: "E0208",
        title: "Branches Have Different Types",
        what: "The branches of an `if` or `when` that produce a value produce different types, or a variable is given values of different types along different paths.",
        wrong: "val x = if ok { 1 } else { \"no\" }",
        right: "val x = if ok { 1 } else { 0 }",
        fix: "Make every branch produce the same type.",
    },
    Entry {
        code: "E0209",
        title: "Wrong Return Type",
        what: "A function returns a value of a type other than its declared return type, or `return` gives no value in a function that has one.",
        wrong: "def name() -> i64 { return \"iris\" }",
        right: "def name() -> str { return \"iris\" }",
        fix: "Return a value of the declared type, or change the declared type.",
    },
    Entry {
        code: "E0210",
        title: "Non-exhaustive `when`",
        what: "A `when` on a choice does not handle every variant and has no `_` arm.",
        wrong: "when c { Color.Red => 1, Color.Green => 2 }",
        right: "when c { Color.Red => 1, Color.Green => 2, Color.Blue => 3 }",
        fix: "Add an arm for each variant the message names, or a `_ =>` arm.",
    },
    // ── native code ────────────────────────────────────────────────────
    Entry {
        code: "E0300",
        title: "Native Code Generation Failed",
        what: "IRIS could not turn the program into native code. This is a compiler or toolchain defect, not an error in the program.",
        wrong: "",
        right: "",
        fix: "Run `iris doctor` to check the toolchain, then `iris bug-report <file.iris>`.",
    },
    Entry {
        code: "E0301",
        title: "No Native Toolchain",
        what: "No C compiler, linker or LLVM library was found to build native code with.",
        wrong: "",
        right: "",
        fix: "`iris doctor` shows what is installed and how to install what is missing; `iris setup` downloads a toolchain.",
    },
    Entry {
        code: "E0302",
        title: "Program Failed",
        what: "The program was built and ran, then stopped with an error: a failed assert, a panic, an out-of-bounds index. The message is the program's own.",
        wrong: "assert(list_len(xs) == 1);  // xs is empty",
        right: "",
        fix: "Fix the condition the message reports; it gives the file, line and column.",
    },
    Entry {
        code: "E0303",
        title: "Not Available on This Target",
        what: "The program uses something the target platform does not have: on `wasm32-wasip1`, the network (WASI preview 1 has no sockets). The error names each function that reaches it.",
        wrong: "iris --target wasm32-wasi --emit binary -o app.wasm client.iris  // client.iris calls tcp_connect",
        right: "iris --target wasm32-wasip2 --emit binary -o app.wasm client.iris",
        fix: "Build for a target that has it (`wasm32-wasip2` has sockets, as does a native build), or keep the program from reaching it on this one, for example behind `#ifndef WASM`.",
    },
    Entry {
        code: "E0304",
        title: "Effect Bound Exceeded",
        what: "A callback argument or trait `impl` method performs side effects not permitted by the declared effect contract.",
        wrong: "trait PureEval { def eval(self: Self) -> i64 effect pure }\nrecord Printer { id: i64 }\nimpl PureEval for Printer { def eval(self: Printer) -> i64 { println(\"hi\"); return 1 } }",
        right: "impl PureEval for Printer { def eval(self: Printer) -> i64 { return 1 } }",
        fix: "Remove the disallowed effect from the implementation or callback, or declare the effect in the trait or parameter contract.",
    },
    Entry {
        code: "E0382",
        title: "Use After Move",
        what: "A value is used after ownership of it moved elsewhere, or while it is borrowed.",
        wrong: "val a = make()\nval b = move a\nuse(a)",
        right: "val a = make()\nval b = copy(a)\nuse(a)",
        fix: "Use the value before moving it, or move a copy.",
    },
    // ── run time ───────────────────────────────────────────────────────
    Entry {
        code: "E0400",
        title: "Runtime Error",
        what: "The program stopped with an error while running. Natively this also covers an invalid memory access (a segmentation fault), which is a defect in IRIS or in a C library the program calls.",
        wrong: "",
        right: "",
        fix: "The message and location say what failed; for a segmentation fault, run `iris bug-report <file.iris>`.",
    },
    Entry {
        code: "E0401",
        title: "Undefined Value",
        what: "The interpreter met a value that was never defined. This is a compiler defect.",
        wrong: "",
        right: "",
        fix: "Run `iris bug-report <file.iris>` and report it.",
    },
    Entry {
        code: "E0402",
        title: "Division By Zero",
        what: "An integer division or remainder had a zero divisor.",
        wrong: "val avg = total / count  // count == 0",
        right: "val avg = if count == 0 { 0 } else { total / count }",
        fix: "Check the divisor before dividing.",
    },
    Entry {
        code: "E0403",
        title: "Index Out of Bounds",
        what: "A list, array or string was indexed outside its length, or with a negative index. Both backends check every index; the message gives the index and the length.",
        wrong: "list_get(xs, list_len(xs))",
        right: "list_get(xs, list_len(xs) - 1)",
        fix: "Valid indices are 0 to len - 1; check with `list_len` first, or loop with `for x in xs`.",
    },
    Entry {
        code: "E0404",
        title: "Runtime Type Error",
        what: "An operation received a value of the wrong kind at run time.",
        wrong: "",
        right: "",
        fix: "Convert explicitly with `to_str`, `to_i64` or `x to f64`; if the program type-checks, report it with `iris bug-report`.",
    },
    Entry {
        code: "E0405",
        title: "Unsupported at Run Time",
        what: "The interpreter does not implement an operation the program used.",
        wrong: "",
        right: "",
        fix: "Build natively (`iris run`), or see docs/known-issues.md.",
    },
    Entry {
        code: "E0406",
        title: "Panic",
        what: "The program called `panic`, an `assert` failed, or `unwrap` met `none` or an error.",
        wrong: "unwrap(find(\"abc\", \"z\"))",
        right: "val i = find(\"abc\", \"z\")\nif is_some(i) { use(unwrap(i)) }",
        fix: "Check options and results before unwrapping; the message names the failing condition.",
    },
    Entry {
        code: "E0407",
        title: "Security Violation",
        what: "Under `--sandbox` the program tried to use the file system, network, FFI or processes.",
        wrong: "iris run --sandbox net_tool.iris",
        right: "iris run net_tool.iris",
        fix: "Run without `--sandbox`, or remove the operation.",
    },
    Entry {
        code: "E0408",
        title: "Stack Overflow",
        what: "Recursion went too deep: it never reaches its base case, or the data is deeper than the stack. The interpreter stops at a call depth of 250 by default (`--max-depth` raises it); a native program stops when its stack runs out. The location is the call that recursed.",
        wrong: "def count(n: i64) -> i64 { return count(n + 1) }",
        right: "def count(n: i64) -> i64 { if n >= 10 { return n } else { return count(n + 1) } }",
        fix: "Give the recursion a base case it reaches; a loop handles iteration of any depth.",
    },
    Entry {
        code: "E0409",
        title: "Integer Overflow",
        what: "Integer `+`, `-`, `*` or `/` gave a result outside the type's range (i64: -9223372036854775808 to 9223372036854775807). IRIS checks integer arithmetic on both backends and stops rather than wrap around silently; the message gives the operands.",
        wrong: "val big = 9223372036854775807
big + 1",
        right: "wrapping_add(big, 1)   // when wrap-around is meant, as in a hash",
        fix: "Use `wrapping_add`, `wrapping_sub` or `wrapping_mul` where wrap-around is intended, or `f64` for magnitudes beyond i64.",
    },
    Entry {
        code: "E0410",
        title: "GPU Error",
        what: "A program built with `--gpu cuda` could not run a `par for` loop on the GPU: there is no CUDA driver or NVIDIA GPU, the GPU could not load or run the kernel, or a tensor did not fit in its memory. Errors in the loop's own code -- an index out of range, an overflow -- are reported with their usual codes and positions.",
        wrong: "iris --gpu cuda --emit binary -o app app.iris   # run where there is no NVIDIA GPU",
        right: "iris --emit binary -o app app.iris              # the same loops on CPU threads",
        fix: "Run the program on a machine with an NVIDIA GPU and its driver, or build it without `--gpu cuda`.",
    },
    // ── host ───────────────────────────────────────────────────────────
    Entry {
        code: "E0500",
        title: "I/O Error",
        what: "A file could not be read or written.",
        wrong: "iris run missing.iris",
        right: "iris run main.iris",
        fix: "Check the path and permissions.",
    },
    Entry {
        code: "E0501",
        title: "Preprocessor Error",
        what: "A `#if`/`#define` directive is malformed.",
        wrong: "#if",
        right: "#if DEBUG",
        fix: "Complete the directive.",
    },
    Entry {
        code: "E0900",
        title: "Internal Compiler Error",
        what: "IRIS generated invalid code for a valid-looking program. This is a bug in IRIS, not in your program.",
        wrong: "",
        right: "",
        fix: "Run `iris bug-report <file.iris>`: it collects the source, versions and compiler detail into one folder to attach to an issue. Rewriting the expression on the reported line often avoids the bug meanwhile.",
    },
    Entry {
        code: "E0901",
        title: "Incomplete HIR (Internal)",
        what: "The compiler's name-resolved, typed HIR was incomplete: an expression was not registered, a value name resolved to nothing, or a call had no target. This is a bug in IRIS, not in your program.",
        wrong: "",
        right: "",
        fix: "Run `IRIS_HIR=report iris check <file.iris>` to print the HIR counts, and `iris bug-report <file.iris>` to attach the source and compiler detail to an issue. This mode is a compiler audit (`IRIS_HIR=check`) and is not enabled for normal builds.",
    },
    Entry {
        code: "E0902",
        title: "Incomplete THIR (Internal)",
        what: "The compiler's typed, desugared THIR was incomplete: an expression the HIR walked was not rebuilt, a value name resolved to nothing, or a call had no target. This is a bug in IRIS, not in your program.",
        wrong: "",
        right: "",
        fix: "Run `IRIS_THIR=report iris check <file.iris>` to print the THIR counts, and `iris bug-report <file.iris>` to attach the source and compiler detail to an issue. This mode is a compiler audit (`IRIS_THIR=check`) and is not enabled for normal builds.",
    },
];

/// All codes with an explanation.
pub fn codes() -> impl Iterator<Item = &'static str> {
    ENTRIES.iter().map(|e| e.code)
}

pub fn explain(code: &str) -> Option<String> {
    let code = code.trim().to_ascii_uppercase();
    let e = ENTRIES.iter().find(|e| e.code == code)?;
    let mut out = format!("# {}: {}\n\n{}\n", e.code, e.title, e.what);
    if !e.wrong.is_empty() {
        out.push_str(&format!("\n## Example\n```iris\n{}\n```\n", e.wrong));
    }
    if !e.right.is_empty() {
        out.push_str(&format!("\n## Corrected\n```iris\n{}\n```\n", e.right));
    }
    out.push_str(&format!("\n## How to fix\n{}\n", e.fix));
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_explain_valid() {
        let e100 = explain("E0100");
        assert!(e100.is_some());
        assert!(e100.unwrap().contains("Undefined Variable"));

        let e402 = explain("e0402");
        assert!(e402.is_some());
        assert!(e402.unwrap().contains("Division By Zero"));
    }

    #[test]
    fn test_explain_invalid() {
        assert!(explain("E9999").is_none());
        assert!(explain("random").is_none());
    }

    #[test]
    fn codes_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for c in codes() {
            assert!(seen.insert(c), "duplicate entry {c}");
        }
    }
}
