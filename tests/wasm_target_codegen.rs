use iris::codegen::llvm_ir::{
    emit_llvm_ir_with_target, target_data_layout, target_preset_to_triple,
};
use iris::compile_to_module;

#[test]
fn test_wasm32_target_resolution_and_data_layout() {
    let triple_wasi = target_preset_to_triple("wasm32-wasi").expect("resolve wasm32-wasi");
    assert_eq!(triple_wasi, "wasm32-wasip1");

    let triple_wasm = target_preset_to_triple("wasm32").expect("resolve wasm32");
    assert_eq!(triple_wasm, "wasm32-wasip1");

    let triple_unknown = target_preset_to_triple("wasm32-unknown").expect("resolve wasm32-unknown");
    assert_eq!(triple_unknown, "wasm32-unknown-unknown");

    let layout = target_data_layout(triple_wasi);
    assert!(
        layout.contains("p:32:32"),
        "wasm data layout must specify 32-bit pointers"
    );
    assert!(
        layout.contains("i64:64"),
        "wasm data layout must align i64 to 64 bits"
    );
}

#[test]
fn test_wasm32_llvm_ir_emission() {
    let src = r#"
def compute_energy(mass: i64, c: i64) -> i64 {
    return mass * c * c
}
"#;

    let module = compile_to_module(src, "wasm_module").expect("compile to IR");
    let llvm_ir =
        emit_llvm_ir_with_target(&module, Some("wasm32-wasi")).expect("emit wasm32 llvm ir");

    assert!(
        llvm_ir.contains("target triple = \"wasm32-wasip1\""),
        "LLVM IR must specify wasm32-wasip1 target triple"
    );
    assert!(
        llvm_ir.contains("target datalayout ="),
        "LLVM IR must include wasm datalayout"
    );
    assert!(
        llvm_ir.contains("@compute_energy"),
        "LLVM IR must declare or define compute_energy"
    );
}

/// WASI preview 1 has no sockets: a program that reaches the network is a
/// build error naming the calls, not a module whose every connect fails.
/// Networking code nothing calls does not count.
#[test]
fn wasip1_builds_refuse_network_use() {
    let dir = std::env::temp_dir().join(format!("iris_wasi_net_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let build = |name: &str, src: &str| {
        let path = dir.join(format!("{name}.iris"));
        std::fs::write(&path, src).unwrap();
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_iris"))
            .args(["--target", "wasm32-wasi", "--emit", "binary", "-o"])
            .arg(dir.join(format!("{name}.wasm")))
            .arg(&path)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    let uses = build(
        "uses",
        "def fetch() -> i64 {\n    tcp_connect(\"127.0.0.1\", 80)\n}\ndef main() -> i64 {\n    fetch()\n}\n",
    );
    assert!(uses.contains("WASI preview 1 has no sockets"), "{uses}");
    assert!(uses.contains("calls `iris_tcp_connect`"), "{uses}");
    let unused = build(
        "unused",
        "def fetch() -> i64 {\n    tcp_connect(\"127.0.0.1\", 80)\n}\ndef main() -> i64 {\n    0\n}\n",
    );
    assert!(
        !unused.contains("no sockets"),
        "unreached networking is not an error: {unused}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A WASI build runs: built with the WASI sysroot and run under Node's WASI
/// preview 1 (skipped without either). The runtime once failed to compile
/// for WebAssembly, and a module that did link trapped at start for want of
/// `main`, without any test noticing.
#[test]
fn a_wasi_build_runs() {
    let node_ok = std::process::Command::new("node")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !node_ok {
        eprintln!("skipped: no node to run WASI modules");
        return;
    }
    let dir = std::env::temp_dir().join(format!("iris_wasi_run_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("app.iris");
    std::fs::write(
        &src,
        "def fib(n: i64) -> i64 {\n    if n < 2 { n } else { fib(n - 1) + fib(n - 2) }\n}\n\
         def main() -> i64 {\n    val xs: list<str> = list()\n    push(xs, \"wasi\")\n    \
         println(xs[0] + \" \" + to_str(fib(20)))\n    assert(fib(20) == 6765)\n    0\n}\n",
    )
    .unwrap();
    let wasm = dir.join("app.wasm");
    let build = std::process::Command::new(env!("CARGO_BIN_EXE_iris"))
        .args(["--target", "wasm32-wasi", "--emit", "binary", "-o"])
        .arg(&wasm)
        .arg(&src)
        .output()
        .unwrap();
    let notes = String::from_utf8_lossy(&build.stderr);
    if !build.status.success()
        && (notes.contains("no native toolchain") || notes.contains("sysroot"))
    {
        eprintln!("skipped: no WASI toolchain: {notes}");
        return;
    }
    assert!(build.status.success(), "{notes}");
    let runner = dir.join("run.mjs");
    std::fs::write(
        &runner,
        "import { readFile } from 'node:fs/promises';\nimport { WASI } from 'node:wasi';\n\
         const wasi = new WASI({ version: 'preview1', args: ['app'], env: {}, returnOnExit: true });\n\
         const m = await WebAssembly.compile(await readFile(process.argv[2]));\n\
         process.exit(wasi.start(await WebAssembly.instantiate(m, wasi.getImportObject())));\n",
    )
    .unwrap();
    let run = std::process::Command::new("node")
        .arg(&runner)
        .arg(&wasm)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(stdout.trim(), "wasi 6765");
    let _ = std::fs::remove_dir_all(&dir);
}
