//! Regression tests for `ui_frame_pool` destruction paired with runtime subsystems
//! (`DiffTensor` autodiff, `json_query`, and `ui_frame_pool_pause` detached clones)
//! to catch double-free heap corruption (`0xC0000374`) between `xmalloc` and `free`.

const POOL_STABILITY_PROGRAM: &str = r#"
bring std.async
bring std.json
bring std.tensor
bring std.ui_protocol

def clone_str_safe(s: str) -> str effect alloc {
    if len(s) == 0 {
        return ""
    }
    val padded = concat("_", s)
    return slice(padded, 1, 1 + len(s))
}

def run_autodiff_pool_iteration(scale: f64) -> f64 effect ffi, alloc {
    val pool = ui_frame_pool_begin()
    val shape: list<i64> = list(2)
    val w_data: list<f64> = list(2.0 * scale, 4.0 * scale)
    val w = tensor_ad_parameter(tensor_ad_from_data(w_data, shape))
    val sq = tensor_ad_mul(w, w)
    val loss = tensor_ad_sum(sq)
    tensor_ad_backward(loss)
    val grad = tensor_ad_data(tensor_ad_grad(w))
    val grad_sum = list_get(grad, 0) + list_get(grad, 1)
    val _freed = ui_frame_pool_destroy(pool)
    return grad_sum
}

def run_json_pool_iteration(payload: str) -> str effect ffi, alloc {
    val pool = ui_frame_pool_begin()
    val extracted = json_query(payload, "metrics.samples.1")
    val tag = json_query(payload, "station")
    val combined = concat(tag, concat(":", extracted))
    val _paused = ui_frame_pool_pause()
    val detached = clone_str_safe(combined)
    val _resumed = ui_frame_pool_resume(pool)
    val _freed = ui_frame_pool_destroy(pool)
    return detached
}

def main() -> i64 effect ffi, io, alloc, throw {
    var grad_total = 0.0
    for i in 0..30 {
        val factor = if i == 0 { 1.0 } else { 1.0 }
        grad_total = grad_total + run_autodiff_pool_iteration(factor)
    }
    assert(grad_total > 359.9 && grad_total < 360.1)

    val sample_json = "{\"station\":\"NRT-01\",\"metrics\":{\"samples\":[10,42,99]}}"
    var last_detached = ""
    for _ in 0..40 {
        last_detached = run_json_pool_iteration(sample_json)
    }
    assert(last_detached == "NRT-01:42")

    val stats = gc_get_stats()
    val alloc_count = map_get(stats, "allocated") ?? 0
    val freed_count = map_get(stats, "freed") ?? 0
    assert(alloc_count >= freed_count)
    println("ui_pool_runtime_stability: ok")
    return 0
}
"#;

#[test]
fn ui_frame_pool_with_autodiff_and_json_stays_heap_clean() {
    let pid = std::process::id();
    let src_path = std::env::temp_dir().join(format!("iris_ui_pool_stability_{pid}.iris"));
    std::fs::write(&src_path, POOL_STABILITY_PROGRAM).expect("write test .iris fixture");
    let compiled = iris::compile_file_to_module(&src_path);
    let _ = std::fs::remove_file(&src_path);
    let module = compiled.expect("compile ui_pool_runtime_stability");

    let exe = std::env::temp_dir().join(format!(
        "iris_ui_pool_stability_{pid}{}",
        std::env::consts::EXE_SUFFIX
    ));
    let exe = iris::codegen::build::build_binary(&module, &exe).expect("build native binary");
    let run = std::process::Command::new(&exe)
        .output()
        .expect("run native binary");
    let _ = std::fs::remove_file(&exe);
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        run.status.success(),
        "native run failed ({:?}):\n{stdout}\n{stderr}",
        run.status
    );
    assert!(stdout.contains("ui_pool_runtime_stability: ok"), "{stdout}");
}
