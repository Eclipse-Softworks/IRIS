//! Keeps release packaging, dependency installation, and runtime discovery on
//! the repository's declared LLVM release.

#[test]
fn llvm_23_release_policy_is_consistent() {
    let version = include_str!("../LLVM_VERSION").trim();
    assert_eq!(version, "23.1.1");

    let release = include_str!("../.github/workflows/release.yml");
    assert!(release.contains("LLVM_VER=\"$(tr -d '\\r\\n' < LLVM_VERSION)\""));
    for asset in [
        "clang+llvm-${LLVM_VER}-x86_64-pc-windows-msvc.tar.xz",
        "clang+llvm-${LLVM_VER}-aarch64-pc-windows-msvc.tar.xz",
        "LLVM-${LLVM_VER}-Linux-X64.tar.xz",
        "LLVM-${LLVM_VER}-Linux-ARM64.tar.xz",
        "LLVM-${LLVM_VER}-macOS-ARM64.tar.xz",
    ] {
        assert!(release.contains(asset), "missing release asset: {asset}");
    }
    assert!(release.contains("Expected LLVM ${LLVM_VER}"));

    let installer = include_str!("../installer/windows/setup_dependencies.ps1");
    assert!(installer.contains("LLVM_VERSION"));
    assert!(installer.contains("LLVM-$requiredLlvmVersion-$llvmArch.msi"));
    assert!(installer.contains("$installedLlvmVersion -ge $requiredLlvmVersion"));

    let loader = include_str!("../src/codegen/llvm_c_api.rs");
    assert!(loader.contains("RECOMMENDED_LLVM_VERSION: &str = \"23.1.1\""));
    assert!(loader.contains("IRIS_LLVM_C_API"));
    assert!(loader.contains(r#"C:\llvm-23.1.1\bin"#));

    let orc = include_str!("../src/codegen/llvm_orc.rs");
    assert!(orc.contains("llvm_library_candidates(default_name)"));
    assert!(orc.contains("RECOMMENDED_LLVM_VERSION"));

    let setup = include_str!("../src/setup.rs");
    assert!(setup.contains("LLVM_VERSION: &str = include_str!(\"../LLVM_VERSION\");"));
    assert!(setup.contains("temp_dir.join(\"LLVM_VERSION\")"));

    let portable = include_str!("../installer/windows/build_portable.ps1");
    assert!(portable.contains("LLVM_VERSION"));

    let deb = include_str!("../installer/linux/build-deb.sh");
    assert!(deb.contains("LLVM_VERSION"));

    let dmg = include_str!("../installer/macos/build-dmg.sh");
    assert!(dmg.contains("LLVM_VERSION"));

    let ci_llvm_as = include_str!("../scripts/ci_llvm_as.sh");
    assert!(ci_llvm_as.contains("llvm-as-23"));

    let ci_capture = include_str!("../scripts/ci_capture.sh");
    assert!(ci_capture.contains("clang-23"));

    let dockerfile = include_str!("../docker/Dockerfile");
    assert!(dockerfile.contains("llvm-toolchain-bullseye-23"));
    assert!(dockerfile.contains("clang-23"));
}
