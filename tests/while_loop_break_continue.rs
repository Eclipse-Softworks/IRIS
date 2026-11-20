//! Phase 9 integration tests: while/loop/break/continue.

use iris::{compile, EmitKind};

#[test]
fn test_while_basic() {
    // Verify the while loop compiles and evaluates correctly.
    // LoopUnrollPass may unroll constant-bound loops (removing while_header/cmplt
    // from live IR), so we check semantics rather than IR structure names.
    let src = "def count() -> i64 { val x = 0; while x < 5 { val x = x + 1 } x }";
    let result = compile(src, "test", EmitKind::Eval).expect("should eval");
    assert_eq!(result.trim(), "5", "counter should reach 5: {}", result);
}

#[test]
fn test_while_zero_iterations() {
    let src = "def zero() -> i64 { val x = 0; while false { val x = x + 1 } x }";
    let result = compile(src, "test", EmitKind::Ir);
    assert!(result.is_ok(), "should compile: {:?}", result.err());
}

#[test]
fn test_loop_with_break() {
    let src = "def find() -> bool { loop { break } false }";
    let ir = compile(src, "test", EmitKind::Ir).expect("should compile");
    assert!(
        ir.contains("loop_merge"),
        "IR should contain loop_merge: {}",
        ir
    );
}

#[test]
fn test_while_ir_has_back_edge() {
    let src = "def count() -> i64 { val x = 0; while x < 10 { val x = x + 1 } x }";
    let ir = compile(src, "test", EmitKind::Ir).expect("should compile");
    let br_count = ir.matches("br ").count();
    assert!(
        br_count >= 2,
        "should have at least 2 br instructions, got {}: {}",
        br_count,
        ir
    );
}

#[test]
fn test_break_exits_loop() {
    let src = r#"
        def search(x: i64) -> i64 {
            val result = 0
            while x > 0 {
                val result = x
                break
            }
            result
        }
    "#;
    let result = compile(src, "test", EmitKind::Ir);
    assert!(result.is_ok(), "should compile: {:?}", result.err());
}

#[test]
fn test_continue_skips_body() {
    let src = r#"
        def skip(x: i64) -> i64 {
            val y = 0
            while x > 0 {
                val x = x - 1
                continue
            }
            y
        }
    "#;
    let result = compile(src, "test", EmitKind::Ir);
    assert!(result.is_ok(), "should compile: {:?}", result.err());
}

#[test]
fn test_nested_while() {
    let src = r#"
        def nested() -> i64 {
            val i = 0
            while i < 3 {
                val j = 0
                while j < 3 {
                    val j = j + 1
                }
                val i = i + 1
            }
            i
        }
    "#;
    let result = compile(src, "test", EmitKind::Ir);
    assert!(
        result.is_ok(),
        "nested while should compile: {:?}",
        result.err()
    );
}

#[test]
fn test_while_llvm() {
    let src = "def count() -> i64 { val x = 0; while x < 5 { val x = x + 1 } x }";
    let llvm = compile(src, "test", EmitKind::Llvm).expect("should compile to LLVM");
    assert!(
        llvm.contains("br label"),
        "LLVM IR should contain 'br label': {}",
        llvm
    );
}

#[test]
fn test_for_wildcard_with_discard_let_terminates() {
    let src = r#"
        def count_pushes(n: i64) -> i64 effect alloc {
            val xs: list<f64> = list()
            for _ in 0..n {
                val _ = list_push(xs, 1.5)
            }
            list_len(xs)
        }
        def main() -> i64 effect alloc { count_pushes(5) }
    "#;
    let result = compile(src, "test", EmitKind::Eval).expect("should eval");
    assert_eq!(result.trim(), "5", "loop should push 5 times: {}", result);
}

#[test]
fn test_for_shadowing_and_break_continue() {
    let src = r#"
        def run() -> i64 effect alloc {
            val i = 100
            val _ = "outer_discard"
            val xs: list<i64> = list()
            for i in 0..6 {
                val i = 999
                val _ = list_push(xs, i)
                if list_len(xs) == 2 {
                    continue
                }
                if list_len(xs) == 4 {
                    break
                }
            }
            while list_len(xs) < 5 {
                val _ = list_push(xs, i)
            }
            list_len(xs) * 1000 + i
        }
        def main() -> i64 effect alloc { run() }
    "#;
    let result = compile(src, "test", EmitKind::Eval).expect("should eval");
    assert_eq!(
        result.trim(),
        "5100",
        "expected 5 items and preserved outer i=100: {}",
        result
    );
}
