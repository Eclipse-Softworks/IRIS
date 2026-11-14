//! A function that declares a value must not reach the end of its body
//! without one (known-issues #89). Falling off used to compile: an uncalled
//! function reached the binary, and a called one failed with an internal
//! "operand not defined" error after inlining.

use iris::{compile, EmitKind};

fn rejects(src: &str, function: &str) {
    let err = compile(src, "test", EmitKind::Ir).expect_err("should be rejected");
    let text = err.to_string();
    assert!(
        text.contains(&format!("`{function}` may end without returning a value")),
        "unexpected error: {text}"
    );
    assert_eq!(err.diagnostic_code(), "E0116", "{text}");
}

fn accepts(src: &str) {
    if let Err(err) = compile(src, "test", EmitKind::Ir) {
        panic!("should compile: {err}");
    }
}

#[test]
fn empty_body_is_rejected() {
    rejects("def f() -> i64 {\n}\ndef main() -> i64 {\n    0\n}\n", "f");
}

#[test]
fn one_branch_returning_is_rejected() {
    rejects(
        "def f(x: i64) -> i64 {\n    if x > 0 { return 1 };\n}\ndef main() -> i64 {\n    f(1)\n}\n",
        "f",
    );
}

#[test]
fn statement_only_body_is_rejected_for_str() {
    rejects(
        "def g() -> str {\n    println(\"x\");\n}\ndef main() -> i64 {\n    0\n}\n",
        "g",
    );
}

#[test]
fn bodies_that_cannot_fall_off_are_accepted() {
    accepts(
        r#"
def a() -> i64 {
    var i = 0;
    while true {
        i = i + 1;
        if i > 3 { return i };
    };
}
def b() -> i64 {
    loop { return 5 };
}
def c(x: i64) -> i64 {
    if x > 0 { return 1 } else { return 2 };
}
def d(x: i64) -> i64 {
    if x > 100 { return 7 };
    panic("too small");
}
def u() -> unit {
    println("no value needed");
}
def main() -> i64 {
    a() + b() + c(1) + d(200)
}
"#,
    );
}
