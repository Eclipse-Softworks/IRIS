//! Tests for dereference write-back (`*y = v`, `*y op= v`, `**pp = v`,
//! `(*p).field = v`, `(*p).0 = v`, conditional `&mut` branches, loops,
//! rebinding `&mut` locals, `&mut T` function/method/closure/generic params)
//! across both the native LLVM backend and the interpreter (`IRIS_FORCE_INTERP=1`),
//! plus borrow-checker and mutability diagnostics.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use iris::compile_to_module;

static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

fn run_parity(name: &str, src: &str, expected_stdout: &str) {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir: PathBuf =
        std::env::temp_dir().join(format!("iris_deref_wb_{}_{}", std::process::id(), id));
    std::fs::create_dir_all(&dir).expect("create temp test dir");
    let file = dir.join(format!("{name}.iris"));
    std::fs::write(&file, src).expect("write temp iris source");

    for force_interp in [false, true] {
        let backend = if force_interp { "interp" } else { "llvm" };
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_iris"));
        cmd.args(["run"]).arg(&file);
        if force_interp {
            cmd.env("IRIS_FORCE_INTERP", "1");
        }
        let out = cmd.output().expect("launch iris binary");
        let stdout = String::from_utf8_lossy(&out.stdout)
            .replace("\r\n", "\n")
            .trim()
            .to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(
            out.status.success(),
            "[{name} @ {backend}] exited {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            out.status.code()
        );
        assert_eq!(
            stdout, expected_stdout,
            "[{name} @ {backend}] stdout mismatch\nstderr:\n{stderr}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn deref_writeback_locals_branches_loops_and_projections_parity() {
    let src = r#"
record Pair {
    a: i64,
    b: i64,
}

def pick_branch(flag: bool) -> i64 {
    val x: i64 = 10
    val y: i64 = 20
    val p = if flag { &mut x } else { &mut y }
    *p = 77
    return x * 100 + y
}

def pick_when(code: i64) -> i64 {
    val a: i64 = 1
    val b: i64 = 2
    val p = when code {
        0 => { &mut a }
        _ => { &mut b }
    }
    *p += 40
    return a * 100 + b
}

def main() -> i64 {
    // 1. Direct write-back & compound assignment
    val x: i64 = 41
    val c = 1 + 1
    val y = if c == 2 { &mut x } else { &mut x }
    *y = 7
    assert(*y == 7)
    assert(x == 7)
    *y += 3
    *y -= 2
    *y *= 4
    assert(x == 32)

    // 2. Distinct &mut arms in `if` and `when`
    assert(pick_branch(true) == 7720)
    assert(pick_branch(false) == 1077)
    assert(pick_when(0) == 4102)
    assert(pick_when(1) == 142)

    // 3. Rebinding `var y = &mut a; y = &mut b` and loops mutating `*y`
    val a: i64 = 5
    val b: i64 = 6
    var r: &mut i64 = &mut a
    *r = 10
    r = &mut b
    *r = 20
    assert(a == 10 && b == 20)

    var loop_acc: i64 = 0
    val lp: &mut i64 = &mut loop_acc
    var i: i64 = 0
    while i < 4 {
        *lp += i + 1
        i += 1
    }
    assert(*lp == 10)

    for j in 0..3 {
        *lp += j
    }
    assert(loop_acc == 13)

    // 4. Double dereference `**pp = 7`
    val deep: i64 = 1
    val p1: &mut i64 = &mut deep
    val pp: &mut &mut i64 = &mut p1
    **pp = 99
    assert(deep == 99)

    // 5. Struct field projection `(*p).field = 5`
    val pair = Pair { a: 1, b: 2 }
    val pr: &mut Pair = &mut pair
    (*pr).a = 5
    (*pr).b += 8
    assert(pair.a == 5 && pair.b == 10)

    // 6. Tuple element projection `(*p).0 = 5`
    val tup = (1, 2)
    val pt = &mut tup
    (*pt).0 = 5
    (*pt).1 += 10
    assert(tup.0 == 5 && tup.1 == 12)

    println(f"{x}:{loop_acc}:{deep}:{pair.a}:{tup.0}")
    return 0
}
"#;
    run_parity("locals_branches_loops_projections", src, "32:13:99:5:5");
}

#[test]
fn deref_writeback_functions_methods_closures_and_generics_parity() {
    let src = r#"
record Counter {
    delta: i64,
}

impl Counter {
    def bump(self: Counter, target: &mut i64) -> i64 {
        *target += self.delta
        return *target
    }
}

def set_direct(p: &mut i64, v: i64) -> i64 {
    *p = v
    return *p
}

def overwrite_generic[T](p: &mut T, v: T) -> i64 {
    *p = v
    return 0
}

def main() -> i64 {
    // 1. Direct &mut function param write-back
    val x: i64 = 4
    val r1 = set_direct(&mut x, 15)
    assert(x == 15 && r1 == 15)

    // 2. Method c.bump(&mut x) write-back
    val c = Counter { delta: 7 }
    val r2 = c.bump(&mut x)
    assert(x == 22 && r2 == 22)

    // 3. Generic function &mut T write-back
    val g: i64 = 0
    overwrite_generic(&mut g, 55)
    assert(g == 55)

    val s: str = "before"
    overwrite_generic(&mut s, "after")
    assert(s == "after")

    // 4. Closure &mut i64 parameter write-back
    val z: i64 = 3
    val add_ten = |p: &mut i64| {
        *p += 10;
        *p
    }
    val r3 = add_ten(&mut z)
    assert(z == 13 && r3 == 13)

    // 5. Mixed disjoint `&a` vs `&mut b` read-only merge allowed
    val da: i64 = 8
    val db: i64 = 9
    val cond = 1 + 1
    val picked = if cond == 2 { &da } else { &mut db }
    assert(*picked == 8)

    println(f"{x}:{g}:{s}:{z}:{*picked}")
    return 0
}
"#;
    run_parity(
        "functions_methods_closures_generics",
        src,
        "22:55:after:13:8",
    );
}

#[test]
fn deref_write_through_shared_ref_is_rejected() {
    let src = r#"
def main() -> i64 {
    val x: i64 = 10
    val r: &i64 = &x
    *r = 7
    return 0
}
"#;
    let err =
        compile_to_module(src, "shared_ref_write").expect_err("writing through `&T` must fail");
    assert!(!err.to_string().is_empty());
}

#[test]
fn deref_write_on_non_reference_is_rejected() {
    let src = r#"
def main() -> i64 {
    val n: i64 = 10
    *n = 7
    return 0
}
"#;
    let err = compile_to_module(src, "non_ref_write")
        .expect_err("deref write on non-reference `i64` must fail");
    assert!(!err.to_string().is_empty());
}

#[test]
fn write_to_var_while_mut_borrow_is_live_is_rejected() {
    let src = r#"
def main() -> i64 {
    var x: i64 = 10
    val y = &mut x
    x = 20
    println(*y)
    return 0
}
"#;
    let err = compile_to_module(src, "write_while_mut_borrowed")
        .expect_err("assigning `x` while `&mut x` is live must fail");
    assert_eq!(err.diagnostic_code(), "E0382", "{err}");
}

#[test]
fn read_var_while_mut_borrow_is_live_is_rejected() {
    let src = r#"
def main() -> i64 {
    val x: i64 = 10
    val y = &mut x
    println(x)
    *y = 7
    return *y
}
"#;
    let err = compile_to_module(src, "read_while_mut_borrowed")
        .expect_err("reading `x` while `&mut x` is live and later written must fail");
    assert_eq!(err.diagnostic_code(), "E0382", "{err}");
}

#[test]
fn mixed_shared_and_mut_borrow_same_var_in_branches_is_rejected() {
    let src = r#"
def main() -> i64 {
    val x: i64 = 10
    val c = 1 + 1
    val y = if c == 2 { &x } else { &mut x }
    println(*y)
    return 0
}
"#;
    let err = compile_to_module(src, "mixed_branch_same_var")
        .expect_err("mixing `&x` and `&mut x` across branches must fail");
    assert_eq!(err.diagnostic_code(), "E0382", "{err}");
}

#[test]
fn mixed_shared_and_mut_borrow_disjoint_vars_compiles() {
    let src = r#"
def main() -> i64 {
    val x: i64 = 10
    val y: i64 = 20
    val c = 1 + 1
    val z = if c == 2 { &x } else { &mut y }
    println(*z)
    return 0
}
"#;
    compile_to_module(src, "mixed_branch_disjoint")
        .expect("mixing `&x` and `&mut y` on disjoint variables must compile");
}
