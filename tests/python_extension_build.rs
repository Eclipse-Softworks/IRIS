//! A Python extension built from IRIS (`--emit python-ext -o NAME.pyd`),
//! imported and called by Python: every kind of value converts both ways, an
//! `err` result and a runtime error raise `IrisError` (Python carries on),
//! and bad arguments raise a TypeError naming the argument.
//!
//! Needs a Python with its development headers; without one the test says
//! so and does nothing.

use std::path::PathBuf;
use std::process::Command;

const PROGRAM: &str = r#"
bring std.tensor
record Point { x: f64, y: f64 }
choice Shape { Circle(f64), Square(f64), Empty }

def add(a: i64, b: i64) -> i64 { a + b }
def small(a: i32) -> i32 { a * 2 }
def scale(p: Point, k: f64) -> Point { Point { x: p.x * k, y: p.y * k } }
def norms(ps: list<Point>) -> list<f64> {
    val out: list<f64> = list()
    for p in ps {
        list_push(out, sqrt(p.x * p.x + p.y * p.y))
    }
    out
}
def area(s: Shape) -> f64 {
    when s {
        Shape.Circle(r) => 3.0 * r * r,
        Shape.Square(a) => a * a,
        Shape.Empty => 0.0,
    }
}
def grow(s: Shape) -> Shape {
    when s {
        Shape.Circle(r) => Shape.Square(r * 2.0),
        _ => Shape.Empty,
    }
}
def greet(name: str) -> str { "hello, " + name }
def find(xs: list<i64>, x: i64) -> option<i64> {
    var i = 0
    var found: option<i64> = none()
    while i < list_len(xs) {
        if list_get(xs, i) == x { found = some(i) }
        i = i + 1
    }
    found
}
def safe_div(a: i64, b: i64) -> result<i64, str> {
    if b == 0 { err("division by zero") } else { ok(a / b) }
}
def pair(a: i64) -> (i64, str) { (a * 2, to_str(a)) }
def nth(xs: list<i64>, i: i64) -> i64 { list_get(xs, i) }
def counts(words: list<str>) -> map<str, i64> {
    val m: map<str, i64> = map()
    for w in words {
        val c = if map_contains(m, w) { unwrap(map_get(m, w)) } else { 0 }
        map_set(m, w, c + 1)
    }
    m
}
def is_pos(x: i64) -> bool { x > 0 }
def doubled(t: DiffTensor) -> DiffTensor { tensor_ad_add(t, t) }
def nested(xs: list<list<i64>>) -> i64 {
    var total = 0
    for row in xs {
        for x in row { total = total + x }
    }
    total
}
def squares(n: i64) -> i64 {
    val out = tensor_ad_from_data(list(0.0, 0.0, 0.0, 0.0), list(4))
    par for i in 0..n {
        out[i] = (i * i) to f32
    }
    to_f64(out[3]) to i64
}
def quiet() { println("side effect"); }
"#;

const CHECK: &str = r#"
import sys
sys.path.insert(0, sys.argv[1])
import ext

assert ext.add(2, 40) == 42
assert ext.small(21) == 42
assert ext.scale({"x": 1.5, "y": -2.0}, 2.0) == {"x": 3.0, "y": -4.0}
assert ext.norms([{"x": 3.0, "y": 4.0}, {"x": 0.0, "y": 1.0}]) == [5.0, 1.0]
assert ext.area(("Circle", 2.0)) == 12.0
assert ext.area("Empty") == 0.0
assert ext.grow(("Circle", 1.5)) == ("Square", 3.0)
assert ext.grow("Empty") == "Empty"
assert ext.greet("iris") == "hello, iris"
assert ext.find([5, 6, 7], 7) == 2
assert ext.find([5, 6, 7], 9) is None
assert ext.safe_div(10, 3) == 3
try:
    ext.safe_div(1, 0)
    raise AssertionError("expected IrisError")
except ext.IrisError as e:
    assert e.args == ("division by zero",), e.args
assert ext.pair(21) == (42, "21")
assert ext.counts(["a", "b", "a"]) == {"a": 2, "b": 1}
assert ext.is_pos(3) is True and ext.is_pos(-3) is False
assert ext.doubled([[1.0, 2.0], [3.0, 4.0]]) == [[2.0, 4.0], [6.0, 8.0]]
assert ext.nested([[1, 2], [3], []]) == 6
assert ext.squares(4) == 9
assert ext.quiet() is None

# A runtime error inside IRIS raises; Python carries on.
for call, code in [(lambda: ext.nth([1, 2, 3], 7), "E0403"), (lambda: ext.squares(5), "E0403")]:
    try:
        call()
        raise AssertionError("expected IrisError")
    except ext.IrisError as e:
        assert f"error[{code}]" in str(e), str(e)
        assert "ext.iris:" in str(e), str(e)
assert ext.nth([1, 2, 3], 2) == 3

for call, needle in [
    (lambda: ext.add(1, "x"), "argument `b`"),
    (lambda: ext.add(1), "takes 2 arguments"),
    (lambda: ext.add(2**70, 1), "out of range"),
    (lambda: ext.small(2**40), "out of range"),
    (lambda: ext.scale({"x": 1.0}, 1.0), "fields"),
    (lambda: ext.area(("Triangle", 1.0)), "no variant `Triangle`"),
    (lambda: ext.greet("a\0b"), "NUL"),
]:
    try:
        call()
        raise AssertionError("expected an error: " + needle)
    except (TypeError, ValueError, OverflowError) as e:
        assert needle in str(e), (needle, str(e))
print("ok")
"#;

fn python() -> Option<String> {
    let candidates: &[&str] = if cfg!(windows) {
        &["python", "python3"]
    } else {
        &["python3", "python"]
    };
    candidates.iter().find_map(|py| {
        let out = Command::new(py)
            .args(["-c", "import sysconfig, os; print(os.path.exists(os.path.join(sysconfig.get_paths()['include'], 'Python.h')))"])
            .output()
            .ok()?;
        (String::from_utf8_lossy(&out.stdout).trim() == "True").then(|| py.to_string())
    })
}

#[test]
fn an_iris_extension_is_imported_and_called_from_python() {
    let Some(py) = python() else {
        eprintln!("skipped: no Python with development headers");
        return;
    };
    let dir = std::env::temp_dir().join(format!("iris_pyext_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src: PathBuf = dir.join("ext.iris");
    std::fs::write(&src, PROGRAM).unwrap();
    let ext = dir.join(if cfg!(windows) { "ext.pyd" } else { "ext.so" });
    let out = Command::new(env!("CARGO_BIN_EXE_iris"))
        .args(["--emit", "python-ext", "-o"])
        .arg(&ext)
        .arg(&src)
        .env("IRIS_PYTHON", &py)
        .output()
        .expect("run iris");
    assert!(
        out.status.success(),
        "build failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let check = dir.join("check.py");
    std::fs::write(&check, CHECK).unwrap();
    let run = Command::new(&py)
        .arg(&check)
        .arg(&dir)
        .current_dir(&dir)
        .output()
        .expect("run python");
    assert!(
        run.status.success(),
        "python failed (exit {:?}):\n{}\n{}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(String::from_utf8_lossy(&run.stdout).contains("ok"));
}
