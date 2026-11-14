use iris::compile_to_module;
use std::process::Command;

const UNUSED_RECORD: &str = r#"
record Plan { order: list<i64>, score: f64 }
def wave(plan: Plan) -> i64 {
    val results: chan<i64> = channel(2);
    val group = task_group();
    for index in 0 .. 2 {
        val value = list_get(plan.order, index);
        spawn(group) { send(results, value); };
    };
    task_group_join(group);
    assert(plan.score == 1.0);
    return recv(results) + recv(results)
}
def main() -> i64 {
    val plan = Plan { order: list(17, 25), score: 1.0 };
    for iteration in 0 .. 32 { assert(wave(plan) == 42); };
    assert(list_get(plan.order, 1) == 25);
    println("spawn capture regression: ok");
    return 0
}
"#;

#[test]
fn spawn_omits_unreferenced_record_and_parent_scope() {
    let module = compile_to_module(UNUSED_RECORD, "unused_spawn_capture").unwrap();
    let spawned = module
        .functions()
        .iter()
        .find(|f| f.name.starts_with("__spawn_"))
        .unwrap();
    let names: Vec<_> = spawned.params.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["results", "value"]);
}

#[test]
fn nested_spawn_preserves_index_and_group_captures() {
    let source = r#"
def main() -> i64 {
    val unused = 999;
    val values: list<i64> = list(42);
    val index = 0;
    val results: chan<i64> = channel();
    val outer = task_group();
    val inner = task_group();
    spawn(outer) {
        spawn(inner) { send(results, list_get(values, index)); };
        task_group_join(inner);
    };
    task_group_join(outer);
    assert(recv(results) == 42);
    return 0
}
"#;
    let module = compile_to_module(source, "nested_spawn_capture").unwrap();
    let spawned: Vec<_> = module
        .functions()
        .iter()
        .filter(|f| f.name.starts_with("__spawn_"))
        .collect();
    assert_eq!(spawned.len(), 2);
    for function in &spawned {
        assert!(!function
            .params
            .iter()
            .any(|p| p.name == "unused" || p.name == "outer"));
        for required in ["index", "results", "values"] {
            assert!(function.params.iter().any(|p| p.name == required));
        }
    }
    assert!(spawned
        .iter()
        .any(|f| f.params.iter().any(|p| p.name == "inner")));
}

#[test]
fn detached_spawn_does_not_capture_a_shadowed_binding() {
    let source = r#"
def main() -> i64 {
    val value = 999;
    val results: chan<i64> = channel();
    spawn { val value = 42; send(results, value); };
    assert(recv(results) == 42);
    return 0
}
"#;
    let module = compile_to_module(source, "shadowed_spawn_capture").unwrap();
    let spawned = module
        .functions()
        .iter()
        .find(|f| f.name.starts_with("__spawn_"))
        .unwrap();
    let names: Vec<_> = spawned.params.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["results"]);
}

#[test]
fn repeated_native_spawns_leave_uncaptured_record_alive() {
    let work = std::env::temp_dir().join(format!("iris_spawn_capture_{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let source = work.join("capture.iris");
    std::fs::write(&source, UNUSED_RECORD).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_iris"))
        .current_dir(&work)
        .env_remove("IRIS_FORCE_INTERP")
        .arg("run")
        .arg(&source)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&work);
    assert!(
        output.status.success(),
        "{:?}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("spawn capture regression: ok"));
}
