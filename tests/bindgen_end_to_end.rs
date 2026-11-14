//! End-to-end integration tests for `iris bindgen`:
//! exercises Clang preprocessor/JSON-AST + `--no-clang` fallback across `#if`/`#ifdef`,
//! multi-line expression macros, `enum class`, `namespace`, C++ classes, concrete
//! `template<...>` specializations (`using`), overloaded functions, function-pointer
//! typedefs, and compiles + links + executes the generated `.iris` + `.cpp` shim.

use std::path::{Path, PathBuf};
use std::process::Command;

fn cpp_compiler() -> Option<String> {
    if cfg!(target_os = "windows") {
        let mingw = r"C:\msys64\ucrt64\bin\g++.exe";
        if Path::new(mingw).is_file() {
            return Some(mingw.to_owned());
        }
    }
    for cand in ["clang++", "g++"] {
        if Command::new(cand)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
        {
            return Some(cand.to_owned());
        }
    }
    None
}

#[test]
fn bindgen_c_and_cpp_clang_ast_with_shim_compiles_and_executes_natively() {
    let Some(cxx) = cpp_compiler() else {
        eprintln!("no C++ compiler available; skipping native link test");
        return;
    };

    let root: PathBuf =
        std::env::temp_dir().join(format!("iris_bindgen_e2e_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    // Shared included header to verify `#include` + macro expansion
    std::fs::write(
        root.join("common_defs.h"),
        "#pragma once\n#define COMMON_BIAS 10\n",
    )
    .unwrap();

    // Rich C++ header exercising all constructs beyond basic C bindgen
    let header_path = root.join("math_engine.hpp");
    std::fs::write(
        &header_path,
        r#"#pragma once
#include <cstdint>
#include "common_defs.h"

#define ENGINE_VER 3
#define ENGINE_SCALE (ENGINE_VER * 10 + COMMON_BIAS)
#define APPLY_BIAS(x) \
    ((x) + COMMON_BIAS)

#ifdef ENABLE_TURBO
#define TURBO_MULT 2
#else
#define TURBO_MULT 1
#endif

#if ENGINE_SCALE >= 40
#define SCALE_READY 1
#endif

typedef int (*CombineCb)(int, int);

typedef struct {
    int64_t id;
    const char* name;
    double factor;
} EngineTag;

namespace math {

enum class Mode : int {
    Linear = 1,
    Quadratic = 2
};

template <typename T>
class Accumulator {
private:
    T value;
public:
    Accumulator(T init);
    ~Accumulator();
    T add(T delta);
    T current() const;
    static int api_level();
};

using IntAcc = Accumulator<int>;

void set_bonus(int b);
int get_bonus();
int compute(int x);
double compute(double x);
int invoke_cb(CombineCb cb, int a, int b);

} // namespace math
"#,
    )
    .unwrap();

    // Real C++ implementation
    let impl_cpp = root.join("math_engine.cpp");
    std::fs::write(
        &impl_cpp,
        r#"#include "math_engine.hpp"

namespace math {

static int g_bonus = 0;

template <typename T>
Accumulator<T>::Accumulator(T init) : value(init) {}

template <typename T>
Accumulator<T>::~Accumulator() {}

template <typename T>
T Accumulator<T>::add(T delta) {
    value += delta;
    return value;
}

template <typename T>
T Accumulator<T>::current() const {
    return value;
}

template <typename T>
int Accumulator<T>::api_level() {
    return 42;
}

template class Accumulator<int>;

void set_bonus(int b) { g_bonus = b; }
int get_bonus() { return g_bonus; }
int compute(int x) { return x * 3 + g_bonus; }
double compute(double x) { return x * 0.5; }
int invoke_cb(CombineCb cb, int a, int b) {
    return cb ? cb(a, b) : -1;
}

} // namespace math
"#,
    )
    .unwrap();

    let iris_out = root.join("bindings.iris");
    let shim_out = root.join("shim.cpp");
    let lib_input = if cfg!(target_os = "windows") {
        root.join("libiris_cppdemo.a")
    } else {
        root.join("libiris_cppdemo.so")
    };
    let lib_input_str = lib_input.to_string_lossy().replace('\\', "/");

    // 1. Run `iris bindgen` with clang AST + preprocessor enabled
    let bindgen_cmd = Command::new(env!("CARGO_BIN_EXE_iris"))
        .args([
            "bindgen",
            header_path.to_str().unwrap(),
            "--cpp",
            "-I",
            root.to_str().unwrap(),
            "-D",
            "ENABLE_TURBO=1",
            "--lib",
            &lib_input_str,
            "--shim-out",
            shim_out.to_str().unwrap(),
            "-o",
            iris_out.to_str().unwrap(),
        ])
        .output()
        .expect("run iris bindgen");
    assert!(
        bindgen_cmd.status.success(),
        "iris bindgen failed:\nstderr: {}",
        String::from_utf8_lossy(&bindgen_cmd.stderr)
    );

    let generated_iris = std::fs::read_to_string(&iris_out).unwrap();
    let generated_shim = std::fs::read_to_string(&shim_out).unwrap();

    assert!(
        generated_iris.contains("const ENGINE_VER: i64 = 3"),
        "{generated_iris}"
    );
    assert!(
        generated_iris.contains("const ENGINE_SCALE: i64 = 40"),
        "{generated_iris}"
    );
    assert!(
        generated_iris.contains("const TURBO_MULT: i64 = 2"),
        "{generated_iris}"
    );
    assert!(
        generated_iris.contains("const SCALE_READY: i64 = 1"),
        "{generated_iris}"
    );
    assert!(
        generated_iris.contains("pub def APPLY_BIAS(x: i64) -> i64"),
        "{generated_iris}"
    );
    assert!(
        generated_iris.contains("const math_Mode_Quadratic: i64 = 2"),
        "{generated_iris}"
    );
    assert!(
        generated_iris.contains("record EngineTag {"),
        "{generated_iris}"
    );
    assert!(
        generated_iris.contains("extern \"C\" def math_IntAcc_new(init: i32) -> i64"),
        "{generated_iris}"
    );
    assert!(
        generated_iris.contains("extern \"C\" def math_IntAcc_add(handle: i64, delta: i32) -> i32"),
        "{generated_iris}"
    );
    assert!(
        generated_iris.contains("extern \"C\" def math_IntAcc_free(handle: i64)"),
        "{generated_iris}"
    );

    // 2. Also verify `--no-clang` produces equivalent core declarations
    let noclang_iris = root.join("bindings_noclang.iris");
    let noclang_cmd = Command::new(env!("CARGO_BIN_EXE_iris"))
        .args([
            "bindgen",
            header_path.to_str().unwrap(),
            "--cpp",
            "--no-clang",
            "-I",
            root.to_str().unwrap(),
            "-D",
            "ENABLE_TURBO=1",
            "--lib",
            &lib_input_str,
            "-o",
            noclang_iris.to_str().unwrap(),
        ])
        .output()
        .expect("run iris bindgen --no-clang");
    assert!(noclang_cmd.status.success());
    let noclang_src = std::fs::read_to_string(&noclang_iris).unwrap();
    assert!(
        noclang_src.contains("const ENGINE_SCALE: i64 = 40"),
        "{noclang_src}"
    );
    assert!(
        noclang_src.contains("extern \"C\" def math_IntAcc_new(init: i32) -> i64"),
        "{noclang_src}"
    );

    // 3. Compile C++ impl + generated shim into shared library
    let dll_or_so = if cfg!(target_os = "windows") {
        root.join("iris_cppdemo.dll")
    } else if cfg!(target_os = "macos") {
        root.join("libiris_cppdemo.dylib")
    } else {
        root.join("libiris_cppdemo.so")
    };
    let mut cxx_cmd = Command::new(&cxx);
    cxx_cmd
        .arg("-std=c++17")
        .arg("-shared")
        .arg("-I")
        .arg(&root);
    if cfg!(unix) {
        cxx_cmd.arg("-fPIC");
    }
    cxx_cmd
        .arg(&impl_cpp)
        .arg(&shim_out)
        .arg("-o")
        .arg(&dll_or_so);
    if cfg!(target_os = "windows") {
        cxx_cmd
            .arg("-static-libstdc++")
            .arg("-static-libgcc")
            .arg(format!("-Wl,--out-implib,{}", lib_input.display()));
    }
    let cxx_out = cxx_cmd.output().expect("compile C++ library + shim");
    assert!(
        cxx_out.status.success(),
        "C++ compile failed:\n{}\n{}",
        String::from_utf8_lossy(&cxx_out.stderr),
        generated_shim
    );

    // 4. Write an IRIS program importing generated bindings, build & execute it natively
    let main_iris = root.join("main.iris");
    std::fs::write(
        &main_iris,
        r#"bring "bindings"

def main() -> i64 {
    assert(ENGINE_VER == 3);
    assert(ENGINE_SCALE == 40);
    assert(TURBO_MULT == 2);
    assert(SCALE_READY == 1);
    assert(APPLY_BIAS(5) == 15);
    assert(math_Mode_Linear == 1);
    assert(math_Mode_Quadratic == 2);

    math_set_bonus(5);
    assert((math_get_bonus() to i64) == 5);
    assert((math_compute(4 to i32) to i64) == 17);
    assert(math_compute_f64(7.0) == 3.5);
    assert((math_invoke_cb(0, 1, 2) to i64) == -1);

    val acc = math_IntAcc_new(10 to i32);
    assert(acc != 0);
    assert((math_IntAcc_add(acc, 7 to i32) to i64) == 17);
    assert((math_IntAcc_current(acc) to i64) == 17);
    assert((math_IntAcc_api_level() to i64) == 42);
    math_IntAcc_free(acc);

    val tag = EngineTag { id: 9, name: "ok", factor: 1.5 };
    assert(tag.id == 9);
    assert(tag.name == "ok");

    println("bindgen e2e ok");
    return 0
}
"#,
    )
    .unwrap();

    let exe_out = root.join(format!("app{}", std::env::consts::EXE_SUFFIX));
    let build_out = Command::new(env!("CARGO_BIN_EXE_iris"))
        .args([
            "build",
            main_iris.to_str().unwrap(),
            "-o",
            exe_out.to_str().unwrap(),
        ])
        .current_dir(&root)
        .output()
        .expect("iris build with bindgen");
    assert!(
        build_out.status.success(),
        "iris build failed:\nstdout: {}\nstderr: {}\nbindings:\n{}",
        String::from_utf8_lossy(&build_out.stdout),
        String::from_utf8_lossy(&build_out.stderr),
        generated_iris
    );

    let mut run_cmd = Command::new(&exe_out);
    run_cmd.current_dir(&root);
    if cfg!(unix) {
        run_cmd.env("LD_LIBRARY_PATH", &root);
        run_cmd.env("DYLD_LIBRARY_PATH", &root);
    }
    let run_out = run_cmd.output().expect("execute built binary");
    assert!(
        run_out.status.success(),
        "built binary failed (exit {:?}):\nstdout: {}\nstderr: {}",
        run_out.status.code(),
        String::from_utf8_lossy(&run_out.stdout),
        String::from_utf8_lossy(&run_out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run_out.stdout).contains("bindgen e2e ok"),
        "{}",
        String::from_utf8_lossy(&run_out.stdout)
    );

    let _ = std::fs::remove_dir_all(&root);
}
