//! `--gpu cuda`, end to end: the native build embeds the kernels' PTX and
//! launches device-code `par for` loops through the runtime, keeps the others
//! on CPU threads (saying why), and the program either runs on the GPU or --
//! with no NVIDIA driver, as on most CI machines -- stops with E0410 at the
//! loop that needed it.

use std::process::Command;

const PROGRAM: &str = r#"bring std.tensor
def main() -> i64 {
    val n = 64
    val xs: list<f64> = list()
    var i = 0
    while i < n {
        list_push(xs, to_f64(i))
        i = i + 1
    }
    val a = tensor_ad_from_data(xs, list(n))
    val out = tensor_ad_from_data(xs, list(n))
    par for j in 0..n {
        out[j] = a[j] * a[j]
    }
    par for j in 0..2 {
        println(to_str(j))
    }
    assert(to_f64(out[5]) == 25.0)
    println("gpu ok")
    return 0
}
"#;

#[test]
fn a_gpu_build_runs_its_loops_on_the_gpu_or_says_why_not() {
    let dir = std::env::temp_dir().join(format!("iris_gpu_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("gpu.iris");
    std::fs::write(&src, PROGRAM).unwrap();
    let exe = dir.join(format!("gpu{}", std::env::consts::EXE_SUFFIX));
    let build = Command::new(env!("CARGO_BIN_EXE_iris"))
        .args(["--gpu", "cuda", "--emit", "binary", "-o"])
        .arg(&exe)
        .arg(&src)
        .output()
        .expect("run iris");
    let notes = String::from_utf8_lossy(&build.stderr);
    if !build.status.success() && notes.contains("NVPTX") {
        eprintln!("skipped: no clang with the NVPTX target: {notes}");
        return;
    }
    assert!(build.status.success(), "{notes}");
    assert!(
        notes.contains("the `par for` in `main` runs on the CPU: its body uses `ValueToStr`"),
        "{notes}"
    );

    let run = Command::new(&exe).output().expect("runs");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    if run.status.success() {
        // A machine with an NVIDIA GPU.
        assert!(stdout.contains("gpu ok"), "{stdout}");
    } else {
        assert_eq!(run.status.code(), Some(101), "{stderr}");
        assert!(stderr.contains("error[E0410]"), "{stderr}");
        assert!(
            stderr.contains("gpu.iris:12:"),
            "the position of the `par for`: {stderr}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
