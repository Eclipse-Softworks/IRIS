//! `iris build -g`: DWARF line tables that map native code back to `.iris`
//! source, across brought modules, verified with a real debugger when one is
//! installed (GDB; the build itself is checked unconditionally).

use std::path::{Path, PathBuf};
use std::process::Command;

use iris::codegen::build::build_binary_debug;
use iris::compile_file_to_module_debug;

const HELPER: &str = "pub def scale(x: f64, k: f64) -> f64 {
    val y = x * k;
    y + 1.0
}
";

const MAIN: &str = "bring \"helper.iris\"

def control(error: f64, rate: f64) -> f64 {
    val p = 2.0 * error;
    val d = 0.5 * rate;
    p - d
}

def main() -> i64 {
    var total = 0.0;
    for i in 0..3 {
        total = total + control(1.0, 0.5);
    };
    val scaled = scale(total, 2.0);
    assert(scaled == 11.5);
    print(scaled);
    0
}
";

struct Project {
    dir: PathBuf,
}

impl Project {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("iris_native_debug_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("helper.iris"), HELPER).unwrap();
        std::fs::write(dir.join("main.iris"), MAIN).unwrap();
        Self { dir }
    }

    fn build(&self) -> PathBuf {
        let (module, map) = compile_file_to_module_debug(&self.dir.join("main.iris"))
            .expect("debug compile with a brought module");
        let files = map
            .files()
            .iter()
            .map(|file| {
                file.path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(files, ["main.iris", "helper.iris"]);
        let exe = self
            .dir
            .join(format!("main{}", std::env::consts::EXE_SUFFIX));
        build_binary_debug(&module, &map, &exe).expect("debug build")
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn gdb() -> Option<PathBuf> {
    let mut candidates = vec![PathBuf::from("gdb")];
    if cfg!(windows) {
        candidates.push(PathBuf::from(r"C:\msys64\ucrt64\bin\gdb.exe"));
    }
    let found = candidates
        .into_iter()
        .find(|gdb| Command::new(gdb).arg("--version").output().is_ok());
    // CI sets this where gdb is installed, so a missing debugger fails the
    // run instead of silently skipping the debugger-driven checks.
    assert!(
        found.is_some() || std::env::var_os("IRIS_REQUIRE_NATIVE_DEBUGGER").is_none(),
        "IRIS_REQUIRE_NATIVE_DEBUGGER is set but gdb was not found"
    );
    found
}

fn run_gdb(gdb: &Path, exe: &Path, cwd: &Path, commands: &[&str]) -> String {
    let mut command = Command::new(gdb);
    command.current_dir(cwd).args(["-q", "-batch", "-nx"]);
    for line in commands {
        command.args(["-ex", line]);
    }
    let output = command.arg(exe).output().expect("run gdb");
    String::from_utf8_lossy(&output.stdout).replace('\r', "")
}

#[test]
fn debug_build_runs_and_matches_the_optimized_result() {
    let project = Project::new("runs");
    let exe = project.build();
    let output = Command::new(&exe).output().expect("run debug binary");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "11.5");
}

#[test]
fn gdb_breaks_steps_and_unwinds_on_iris_lines_across_files() {
    let Some(gdb) = gdb() else {
        eprintln!("gdb not installed; debugger-driven check skipped");
        return;
    };
    let project = Project::new("gdb");
    let exe = project.build();
    let text = run_gdb(
        &gdb,
        &exe,
        &project.dir,
        &[
            "break main.iris:5",
            "break helper.iris:2",
            "run",
            "bt",
            "info args",
            "next",
            "next",
            "delete 1",
            "continue",
            "bt 1",
            "info args",
            "next",
            "kill",
        ],
    );
    // A breakpoint on an IRIS line in the main file, with arguments.
    assert!(
        text.contains("control (error=1, rate=0.5) at main.iris:5"),
        "{text}"
    );
    // The backtrace names the IRIS caller and its line.
    assert!(text.contains("at main.iris:12"), "{text}");
    // Stepping follows the source, including the tail expression.
    assert!(text.contains("\n6\t    p - d"), "{text}");
    // A breakpoint in a brought module resolves to that module's file.
    assert!(text.contains("at helper.iris:2"), "{text}");
    assert!(text.contains("x = 5.25"), "{text}");
    assert!(text.contains("\n3\t    y + 1.0"), "{text}");
}

const LOCALS: &str = "#define TUNED 1
def control(error: f64, rate: f64) -> f64 {
    val p = 2.0 * error;
    val d = 0.5 * rate;
    p - d
}

def main() -> i64 {
    val label = \"pd-loop\";
    var total = 0.0;
    var steps = 0;
    for i in 0..3 {
        total = total + control(1.0, 0.5);
        steps = steps + 1;
    };
    println(label);
    print(total);
    0
}
";

#[test]
fn gdb_shows_parameters_locals_and_strings_after_a_directive() {
    let Some(gdb) = gdb() else {
        eprintln!("gdb not installed; debugger-driven check skipped");
        return;
    };
    let dir = std::env::temp_dir().join(format!("iris_native_debug_locals_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("locals.iris");
    std::fs::write(&source, LOCALS).unwrap();
    let (module, map) = compile_file_to_module_debug(&source).expect("debug compile");
    let exe = build_binary_debug(
        &module,
        &map,
        &dir.join(format!("locals{}", std::env::consts::EXE_SUFFIX)),
    )
    .expect("debug build");
    let text = run_gdb(
        &gdb,
        &exe,
        &dir,
        &[
            "break locals.iris:5",
            "break locals.iris:16",
            "run",
            "info args",
            "info locals",
            "delete 1",
            "continue",
            "info locals",
            "kill",
        ],
    );
    // The `#define` on line 1 must not shift the line table.
    assert!(text.contains("at locals.iris:5"), "{text}");
    assert!(text.contains("\n5\t    p - d"), "{text}");
    // Parameters survive their last use (stack-slot described).
    assert!(text.contains("error = 1\n"), "{text}");
    assert!(text.contains("rate = 0.5\n"), "{text}");
    // `val` bindings.
    assert!(text.contains("p = 2\n"), "{text}");
    assert!(text.contains("d = 0.25\n"), "{text}");
    // `var`s carried through a loop, the loop variable, and a `str`.
    assert!(text.contains("total = 5.25\n"), "{text}");
    assert!(text.contains("steps = 3\n"), "{text}");
    assert!(text.contains("\"pd-loop\""), "{text}");
    let _ = std::fs::remove_dir_all(dir);
}

const COMPOSITES: &str = "record Point { x: f64, y: f64 }
choice Color { Red, Green, Blue }
choice Shape { Dot, Circle(f64) }

def inspect(pt: Point, xs: list<i64>, mp: map<str, i64>, sh: Shape) -> i64 {
    val col = Color.Green;
    val opt = some(42);
    val arr: [i64; 3] = [3, 4, 5];
    val sum = arr[0] + arr[2];
    sum + xs[1]
}

def main() -> i64 {
    val pt = Point { x: 1.5, y: 2.5 };
    val xs = list_new();
    list_push(xs, 10);
    list_push(xs, 20);
    list_push(xs, 30);
    val mp = map_new();
    map_set(mp, \"k\", 7);
    val sh = Shape.Circle(4.5);
    val r = inspect(pt, xs, mp, sh);
    print(r);
    0
}
";

#[test]
fn gdb_inspects_record_list_map_enum_option_and_scalar_array_locals() {
    let Some(gdb) = gdb() else {
        eprintln!("gdb not installed; debugger-driven check skipped");
        return;
    };
    let dir = std::env::temp_dir().join(format!(
        "iris_native_debug_composites_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("composites.iris");
    std::fs::write(&source, COMPOSITES).unwrap();
    let (module, map) = compile_file_to_module_debug(&source).expect("debug compile");
    let exe = build_binary_debug(
        &module,
        &map,
        &dir.join(format!("composites{}", std::env::consts::EXE_SUFFIX)),
    )
    .expect("debug build");
    let text = run_gdb(
        &gdb,
        &exe,
        &dir,
        &[
            "break composites.iris:10",
            "run",
            "p pt->x",
            "p pt->y",
            "p xs->len",
            "p xs->data[1]->i64",
            "p mp->len",
            "p col",
            "p sh->tag",
            "p sh->ptr->tag",
            "p sh->ptr->fields[0]->f64",
            "p opt->has_value",
            "p opt->value->i64",
            "p *arr",
            "kill",
        ],
    );
    assert!(text.contains("at composites.iris:10"), "{text}");
    assert!(text.contains("= 1.5\n"), "pt->x missing in:\n{text}");
    assert!(text.contains("= 2.5\n"), "pt->y missing in:\n{text}");
    assert!(text.contains("= 3\n"), "xs->len missing in:\n{text}");
    assert!(
        text.contains("= 20\n"),
        "xs->data[1]->i64 missing in:\n{text}"
    );
    assert!(text.contains("= 1\n"), "mp->len missing in:\n{text}");
    assert!(text.contains("= Green\n"), "col enum missing in:\n{text}");
    assert!(
        text.contains("= IRIS_TAG_ENUM\n"),
        "sh->tag missing in:\n{text}"
    );
    assert!(
        text.contains("= Circle\n"),
        "sh->ptr->tag missing in:\n{text}"
    );
    assert!(
        text.contains("= 4.5\n"),
        "sh->ptr->fields[0]->f64 missing in:\n{text}"
    );
    assert!(
        text.contains("= 1 '\\001'") || text.contains("= 1\n"),
        "opt->has_value missing in:\n{text}"
    );
    assert!(
        text.contains("= 42\n"),
        "opt->value->i64 missing in:\n{text}"
    );
    assert!(
        text.contains("= {3, 4, 5}"),
        "scalar array *arr missing in:\n{text}"
    );
    let _ = std::fs::remove_dir_all(dir);
}
