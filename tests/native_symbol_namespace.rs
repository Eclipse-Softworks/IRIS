//! User functions named like symbols of the C runtime's libraries (`send`,
//! `connect` from Winsock or libc, `free` from the C library) link and run in
//! a native executable, called directly and through a function value.

const SOURCE: &str = r#"
def send(x: i64) -> i64 { x + 1 }
def connect(a: i64, b: i64) -> i64 { a * b }
def free(x: i64) -> i64 { x - 1 }
def apply(f: (i64) -> i64, x: i64) -> i64 { f(x) }

def main() -> i64 {
    assert(send(1) == 2);
    assert(connect(6, 7) == 42);
    assert(free(10) == 9);
    assert(apply(send, 41) == 42);
    val note = "mail @send and @free stay text";
    assert(len(note) == 30);
    println(note);
    println("native symbols: ok");
    0
}
"#;

#[test]
fn user_functions_named_like_c_library_symbols_link_and_run() {
    let module = iris::compile_to_module(SOURCE, "native_symbols").expect("compile");
    let exe = std::env::temp_dir().join(format!(
        "iris_native_symbols_{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ));
    let exe = iris::codegen::build::build_binary(&module, &exe).expect("build native program");
    let run = std::process::Command::new(&exe)
        .output()
        .expect("run native program");
    let _ = std::fs::remove_file(&exe);
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        run.status.success(),
        "native run failed ({}):\n{stdout}\n{stderr}",
        run.status
    );
    assert!(
        stdout.contains("mail @send and @free stay text"),
        "{stdout}"
    );
    assert!(stdout.contains("native symbols: ok"), "{stdout}");
}
