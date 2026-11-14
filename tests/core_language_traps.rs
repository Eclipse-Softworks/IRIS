//! Diagnostics for the core-language traps closed on 2026-10-18: a used value
//! whose type nothing determines (formerly a silent `i64`), and a record used
//! as `dyn Trait` without implementing the trait. The corpus files cover the
//! behaviour; these pin the exact error so a negative test cannot pass for an
//! unrelated reason.

use iris::compile_to_module;

fn diagnostic(source: &str) -> (String, &'static str) {
    let error = compile_to_module(source, "trap").expect_err("program must be rejected");
    (error.to_string(), error.diagnostic_code())
}

#[test]
fn a_used_value_of_undetermined_type_needs_an_annotation() {
    let (message, code) = diagnostic(
        "def main() -> i64 {
    val names = list();
    val first = list_get(names, 0);
    println(to_str(first));
    0
}",
    );
    assert_eq!(code, "E0207", "{message}");
    assert!(message.contains("type annotations needed"), "{message}");
}

#[test]
fn the_annotation_resolves_it() {
    compile_to_module(
        "def main() -> i64 {
    val names: list<str> = list();
    push(names, \"ada\");
    val first = list_get(names, 0);
    println(first);
    0
}",
        "annotated",
    )
    .expect("an annotated list is fully typed");
}

#[test]
fn an_unknown_part_of_a_compound_is_not_an_error() {
    // The error type of `r` and the element type of `xs` are never
    // determined, but neither is ever read, so nothing is assumed.
    compile_to_module(
        "def main() -> i64 {
    val r = ok(5);
    val n = when r { ok(v) => v, err(e) => 0, };
    val xs = list();
    assert(n + list_len(xs) == 5);
    0
}",
        "compound",
    )
    .expect("unobserved unknown parts stay legal");
}

#[test]
fn a_record_must_implement_the_trait_to_become_dyn() {
    let (message, _) = diagnostic(
        "trait Shape { def area(self) -> f64 }
record Label { text: str }
def make() -> dyn Shape { Label { text: \"x\" } }
def main() -> i64 { 0 }",
    );
    assert!(
        message.contains("'Label' does not implement trait 'Shape'"),
        "{message}"
    );
}

#[test]
fn returned_and_stored_values_coerce_to_dyn() {
    compile_to_module(
        "trait Shape { def area(self) -> f64 }
record Sq { s: f64 }
record Rect { w: f64, h: f64 }
impl Shape for Sq { def area(self) -> f64 { self.s * self.s } }
impl Shape for Rect { def area(self) -> f64 { self.w * self.h } }
record Holder { shape: dyn Shape }
def pick(big: bool) -> dyn Shape {
    if big { Rect { w: 2.0, h: 5.0 } } else { Sq { s: 3.0 } }
}
def main() -> i64 {
    val h: Holder = Holder { shape: pick(true) };
    assert(h.shape.area() == 10.0);
    0
}",
        "coercions",
    )
    .expect("tail, branch and field coercions");
}
