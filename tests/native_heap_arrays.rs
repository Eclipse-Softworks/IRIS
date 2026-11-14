//! Fixed arrays of heap values (strings, records) keep their initial
//! elements in a native executable. They used to be emitted as empty runtime
//! lists, so `["a", "b"][1]` failed its bounds check natively while the
//! interpreter was right.

const SOURCE: &str = r#"
record Point { x: i64, y: i64 }

def label(k: i64) -> str {
    val names = ["zero", "one", "two"];
    names[k]
}

def main() -> i64 {
    assert(label(0) == "zero");
    assert(label(2) == "two");
    val points = [Point { x: 1, y: 2 }, Point { x: 3, y: 4 }];
    assert(points[1].x == 3);
    var words = ["a", "b"];
    words[1] = "changed";
    assert(words[1] == "changed");
    assert(words[0] == "a");
    println("heap arrays: ok");
    0
}
"#;

#[test]
fn arrays_of_strings_and_records_keep_their_elements_natively() {
    let module = iris::compile_to_module(SOURCE, "heap_arrays").expect("compile");
    let exe = std::env::temp_dir().join(format!(
        "iris_heap_arrays_{}{}",
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
    assert!(stdout.contains("heap arrays: ok"), "{stdout}");
}
