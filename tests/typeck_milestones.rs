//! Type-system milestones 4, 5 and 6 (docs/architecture-vs-rustc.md §4.2).
//!
//! 4. The source type checker's errors are the program's type errors: they
//!    are reported whether or not lowering accepts the program, in place of
//!    lowering's own error in the same function, with the fixes lowering
//!    offered; the IR passes no longer find type errors it misses (the
//!    diagnostics corpus is checked for that in `diagnostic_quality.rs`).
//! 5. Lowering takes the checker's types and leaves no value untyped: `IrType`
//!    has no "unknown" variant, so a value whose type nothing determines stops
//!    lowering with E0207, and every IR value carries its type.
//! 6. Lowering's name and shape errors are the checker's: unknown names,
//!    wrong argument counts, missing record fields and a `when` that misses an
//!    enum variant are reported from the source tree before any IR exists.

use iris::compile_to_module;

fn rejected(source: &str) -> (String, &'static str) {
    let error = compile_to_module(source, "m").expect_err("program must be rejected");
    (error.to_string(), error.diagnostic_code())
}

fn accepted(source: &str) -> iris::ir::module::IrModule {
    compile_to_module(source, "m").unwrap_or_else(|e| panic!("program must compile: {e}"))
}

#[test]
fn question_mark_in_a_function_that_returns_no_result_is_an_error() {
    let (message, code) = rejected(
        "def half(n: i64) -> result<i64, str> {
    if n % 2 == 1 { err(\"odd\") } else { ok(n / 2) }
}
def main() -> i64 {
    val h = half(4)?
    h
}",
    );
    assert_eq!(code, "E0202", "{message}");
    assert!(
        message.contains("`?` returns early from `main`, which returns `i64`"),
        "{message}"
    );
}

#[test]
fn every_question_mark_in_a_try_carries_one_error_type() {
    let (message, code) = rejected(
        "def a() -> result<i64, str> { ok(1) }
def b() -> result<i64, i64> { ok(2) }
def main() -> i64 {
    val r: i64 = try {
        val x = a()?
        val y = b()?
        x + y
    } catch e {
        0
    }
    r
}",
    );
    assert_eq!(code, "E0202", "{message}");
    assert!(
        message.contains("passes on a `i64`, but an earlier `?` in this `try` passes on a `str`"),
        "{message}"
    );
}

#[test]
fn a_catch_parameter_has_the_error_type_its_try_carries() {
    let module = accepted(
        "def parse(s: str) -> result<i64, str> {
    if s == \"\" { err(\"empty\") } else { ok(1) }
}
def main() -> i64 {
    val n: i64 = try { parse(\"\")? } catch e { len(e) }
    n
}",
    );
    let main = module.function_by_name("main").unwrap();
    let catch_param = main
        .blocks()
        .iter()
        .find(|b| b.name.as_deref() == Some("catch"))
        .and_then(|b| b.params.first())
        .expect("a catch block with its parameter");
    assert_eq!(catch_param.ty, iris::ir::types::IrType::Str);
}

#[test]
fn a_list_holds_one_type() {
    let (message, code) = rejected(
        "def main() -> i64 {
    val items = list()
    push(items, ok(1))
    push(items, none())
    len(items)
}",
    );
    assert_eq!(code, "E0202", "{message}");
    assert!(message.contains("this element is"), "{message}");
}

#[test]
fn concat_takes_strings() {
    let (message, code) =
        rejected("def main() -> i64 {\n    println(concat(\"n = \", 3))\n    0\n}");
    assert_eq!(code, "E0110", "{message}");
}

#[test]
fn the_checker_reports_even_where_lowering_rejects_another_function() {
    // `f` has a type error the checker finds; `g` an unknown name only
    // lowering finds. Both are reported, in source order.
    let error = compile_to_module(
        "def f() -> i64 {
    val x: str = 1 + 2
    0
}
def g() -> i64 {
    undefined_thing + 1
}
def main() -> i64 { f() + g() }",
        "m",
    )
    .expect_err("two errors");
    let iris::error::Error::Lower(iris::error::LowerError::Multiple(errors)) = &error else {
        panic!("expected both errors, got: {error}");
    };
    let texts: Vec<String> = errors.iter().map(|e| e.to_string()).collect();
    assert_eq!(texts.len(), 2, "{texts:?}");
    assert!(
        texts[0].contains("expected `str`"),
        "the checker's, first in the source: {texts:?}"
    );
    assert!(
        texts[1].contains("undefined_thing"),
        "lowering's: {texts:?}"
    );
}

#[test]
fn operator_mismatches_keep_lowering_s_fixes() {
    let (message, code) =
        rejected("def main() -> i64 {\n    val s = \"n = \" + 3\n    println(s)\n    0\n}");
    assert_eq!(code, "E0101", "{message}");
    assert!(message.contains("cannot add `str` and `i64`"), "{message}");
}

/// What lowering produces is fully typed -- every value, block parameter and
/// signature. `IrType` has no "unknown" variant, so the walk asserts each
/// value carries a type; a value whose type nothing determines stops lowering
/// with E0207, so compiling at all is itself the proof (the corpus compiles).
#[test]
fn lowering_leaves_nothing_untyped() {
    let program = "bring std.string
bring std.speculation
bring std.async
var history: list<f64> = list()
def evens(n: i64) -> list<i64> {
    var i = 0
    while i < n {
        yield i
        i = i + 2
    }
}
def check4(n: i64) -> result<i64, str> {
    if n % 4 == 0 { ok(n) } else { err(\"not a multiple\") }
}
def quarter(n: i64) -> result<i64, str> {
    val q = check4(n)?
    ok(q / 4)
}
def main() -> i64 {
    val ch = channel()
    send(ch, 3)
    val m = map()
    m.set(\"a\", 1)
    val r = ok(7)
    val v = when r { ok(x) => x, err(_) => 0 }
    val parts = split_once(\"a=b\", \"=\")
    push(history, 1.5)
    val q = when quarter(8) { ok(x) => x, err(_) => 0 }
    recv(ch) + v + len(evens(6)) + q + m.len() + (if is_some(parts) { 1 } else { 0 })
}
";
    let dir = std::env::temp_dir().join(format!("iris_typed_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("typed.iris");
    std::fs::write(&path, program).unwrap();
    let module = iris::compile_file_to_module_with_opts(&path, None)
        .unwrap_or_else(|e| panic!("program must compile: {e}"));
    let _ = std::fs::remove_dir_all(&dir);
    for f in module.functions() {
        for block in f.blocks() {
            for instr in &block.instrs {
                if let Some(v) = instr.result() {
                    f.value_type(v)
                        .unwrap_or_else(|| panic!("{}: a value without a type: {instr:?}", f.name));
                }
            }
        }
    }
}

/// A function declared `-> Infer` (as the REPL declares the wrapper it
/// evaluates an expression in) returns what its body solves to, decided
/// before lowering: for the function and for its callers.
#[test]
fn an_inferred_return_type_is_solved_from_the_body() {
    let module = accepted(
        "def label(n: i64) -> Infer {
    if n > 0 { \"positive\" } else { \"not positive\" }
}
def main() -> i64 {
    val s = label(3)
    len(s)
}",
    );
    let label = module.function_by_name("label").unwrap();
    assert_eq!(label.return_ty, iris::ir::types::IrType::Str);
}

// ── Milestone 6: name and shape errors are the checker's ──────────────────
//
// Each of these programs is rejected by the source checker before any IR is
// built: lowering's old E0100/E0109/E0113/E0114/E0118/E0122/E0210 are now
// judgments on the AST. `diagnostic_quality.rs` holds the rendered messages
// and spans byte-exact; these pin the error classification.

#[test]
fn an_unknown_name_is_reported_from_the_source() {
    let (message, code) = rejected(
        "def main() -> i64 {
    val total = 3
    println(to_str(count))
    total
}",
    );
    assert_eq!(code, "E0100", "{message}");
    assert!(
        message.contains("cannot find `count` in this scope"),
        "{message}"
    );
}

#[test]
fn a_wrong_argument_count_is_reported_from_the_source() {
    let (message, code) = rejected(
        "def add(a: i64, b: i64) -> i64 { a + b }
def main() -> i64 { add(1) }",
    );
    assert_eq!(code, "E0109", "{message}");
    assert!(
        message.contains("`add` takes 2 arguments but 1 was given"),
        "{message}"
    );
}

#[test]
fn a_record_literal_rejects_a_field_it_does_not_have() {
    let (message, code) = rejected(
        "record Point { x: i64, y: i64 }
def main() -> i64 {
    val p = Point { x: 1, z: 2 }
    p.x
}",
    );
    assert_eq!(code, "E0113", "{message}");
    assert!(message.contains("`Point` has no field `z`"), "{message}");
}

#[test]
fn a_record_literal_requires_every_field_without_a_default() {
    let (message, code) = rejected(
        "record Point { x: i64, y: i64 }
def main() -> i64 {
    val p = Point { x: 1 }
    p.x
}",
    );
    assert_eq!(code, "E0114", "{message}");
    assert!(
        message.contains("missing field `y` in `Point`"),
        "{message}"
    );
}

#[test]
fn a_value_that_is_not_a_function_cannot_be_called() {
    let (message, code) = rejected(
        "def main() -> i64 {
    val n = 3
    n(1)
}",
    );
    assert_eq!(code, "E0118", "{message}");
    assert!(
        message.contains("`n` is not a function: it has type `i64`"),
        "{message}"
    );
}

#[test]
fn a_choice_reference_rejects_a_variant_it_does_not_have() {
    let (message, code) = rejected(
        "choice Color { Red, Green }
def main() -> i64 {
    val c = Color.Blue
    0
}",
    );
    assert_eq!(code, "E0122", "{message}");
    assert!(
        message.contains("`Color` has no variant `Blue`"),
        "{message}"
    );
}

#[test]
fn a_when_must_handle_every_variant() {
    let (message, code) = rejected(
        "choice Color { Red, Green, Blue }
def main() -> i64 {
    when Color.Red {
        Color.Red => 1,
        Color.Green => 2
    }
}",
    );
    assert_eq!(code, "E0210", "{message}");
    assert!(
        message.contains("does not handle `Color.Blue`"),
        "{message}"
    );
}

/// The builtin wins over a local of the same name in call position, as it
/// does in lowering (known-issue #15): this program compiles today, so the
/// checker must not turn its shadowed local into E0118.
#[test]
fn a_builtin_call_name_shadowed_by_a_local_is_not_an_error() {
    let module = accepted(
        "def main() -> i64 {
    val len = 2
    len(\"abc\") + len
}",
    );
    assert!(module.function_by_name("main").is_some());
}
