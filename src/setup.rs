//! Cross-platform component and dependency installer (`iris setup`).
//!
//! Fetches pinned relocatable IRIS toolchains, `iris_ui_host`, VS Code
//! extension builds, WASI sysroots, and optional ML SDKs into `~/.iris/` (or
//! `$IRIS_HOME` / `--dir`) without requiring administrator privileges.

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::upgrade::{
    download_url, extract_archive_subdir, extract_file, fetch_latest_release, release_triple,
    select_asset, select_full_asset, select_vsix_asset, verify_digest, GitHubRelease,
};

/// Pinned LLVM release expected by this compiler build.
pub const LLVM_VERSION: &str = include_str!("../LLVM_VERSION");

const WASI_SDK_TAG: &str = "wasi-sdk-24";
const WASI_SYSROOT_ARCHIVE: &str = "wasi-sysroot-24.0.tar.gz";
const ONNX_RUNTIME_VERSION: &str = "1.20.1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupComponent {
    Base,
    Toolchain,
    Ui,
    Vscode,
    Wasm,
    Ml,
    All,
    List,
}

impl SetupComponent {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "base" | "default" => Ok(Self::Base),
            "toolchain" | "llvm" | "clang" => Ok(Self::Toolchain),
            "ui" | "ui-host" | "gui" => Ok(Self::Ui),
            "vscode" | "vsix" | "extension" => Ok(Self::Vscode),
            "wasm" | "wasi" => Ok(Self::Wasm),
            "ml" | "onnx" | "onnxruntime" => Ok(Self::Ml),
            "all" | "full" => Ok(Self::All),
            "list" | "status" => Ok(Self::List),
            other => Err(format!(
                "unknown setup component `{other}` (expected: base, toolchain, ui, vscode, wasm, ml, all, list)"
            )),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Toolchain => "toolchain",
            Self::Ui => "ui",
            Self::Vscode => "vscode",
            Self::Wasm => "wasm",
            Self::Ml => "ml",
            Self::All => "all",
            Self::List => "list",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SetupOptions {
    pub components: Vec<SetupComponent>,
    pub dir: Option<PathBuf>,
    pub yes: bool,
    pub force: bool,
}

impl Default for SetupOptions {
    fn default() -> Self {
        Self {
            components: vec![SetupComponent::Base],
            dir: None,
            yes: false,
            force: false,
        }
    }
}

/// Resolve the default user-local IRIS root directory (`~/.iris` or `$IRIS_HOME`).
pub fn default_install_root() -> PathBuf {
    if let Some(home) = env::var_os("IRIS_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(home);
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            if let Some(prefix) = bin_dir.parent() {
                let prefix_str = prefix.to_string_lossy();
                if prefix_str.ends_with(".iris") || prefix_str.ends_with("Programs\\IRIS") {
                    return prefix.to_path_buf();
                }
            }
        }
    }
    if let Some(home) = env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .filter(|v| !v.is_empty())
    {
        return PathBuf::from(home).join(".iris");
    }
    PathBuf::from(".iris")
}

/// Expand `Base` and `All` aliases into concrete setup steps (deduplicated, preserving order).
pub fn expand_components(raw: &[SetupComponent]) -> Vec<SetupComponent> {
    let input = if raw.is_empty() {
        &[SetupComponent::Base][..]
    } else {
        raw
    };
    let mut out = Vec::new();
    let mut push = |c: SetupComponent| {
        if !out.contains(&c) {
            out.push(c);
        }
    };
    for &c in input {
        match c {
            SetupComponent::List => return vec![SetupComponent::List],
            SetupComponent::Base => {
                push(SetupComponent::Toolchain);
                push(SetupComponent::Ui);
                push(SetupComponent::Vscode);
            }
            SetupComponent::All => {
                push(SetupComponent::Toolchain);
                push(SetupComponent::Ui);
                push(SetupComponent::Vscode);
                push(SetupComponent::Wasm);
                push(SetupComponent::Ml);
            }
            other => push(other),
        }
    }
    out
}

/// Entry point compatible with zero-arg callers.
pub fn run_setup_command() -> Result<(), String> {
    run_setup_with_options(&SetupOptions::default())
}

/// Install or inspect IRIS toolchain and SDK components.
pub fn run_setup_with_options(opts: &SetupOptions) -> Result<(), String> {
    let root = opts.dir.clone().unwrap_or_else(default_install_root);
    let expanded = expand_components(&opts.components);

    if expanded == [SetupComponent::List] {
        print_component_status(&root);
        return Ok(());
    }

    let explicit_vscode = opts.components.contains(&SetupComponent::Vscode)
        || opts.components.contains(&SetupComponent::All);

    if !opts.yes {
        let names: Vec<&str> = expanded.iter().map(|c| c.name()).collect();
        print!(
            "Install IRIS components [{}] into {}? [Y/n]: ",
            names.join(", "),
            root.display()
        );
        io::stdout().flush().map_err(|e| e.to_string())?;
        let mut line = String::new();
        io::stdin().read_line(&mut line).map_err(|e| e.to_string())?;
        let ans = line.trim().to_ascii_lowercase();
        if !ans.is_empty() && !ans.starts_with('y') {
            println!("Setup cancelled.");
            return Ok(());
        }
    }

    fs::create_dir_all(&root)
        .map_err(|e| format!("cannot create install root {}: {e}", root.display()))?;

    let mut release_cache: Option<GitHubRelease> = None;
    let mut archive_cache: Option<(String, Vec<u8>)> = None;

    for component in expanded {
        match component {
            SetupComponent::Toolchain => {
                install_toolchain(&root, opts.force, &mut release_cache, &mut archive_cache)?;
            }
            SetupComponent::Ui => {
                install_ui_host(&root, opts.force, &mut release_cache, &mut archive_cache)?;
            }
            SetupComponent::Vscode => {
                install_vscode_extension(&root, opts.force, explicit_vscode, &mut release_cache)?;
            }
            SetupComponent::Wasm => install_wasm_sdk(&root, opts.force)?,
            SetupComponent::Ml => install_ml_sdk(&root, opts.force)?,
            SetupComponent::Base | SetupComponent::All | SetupComponent::List => {}
        }
    }

    println!("\nSetup complete. Run `iris doctor` to verify the toolchain.");
    Ok(())
}

fn ensure_release<'a>(cache: &'a mut Option<GitHubRelease>) -> Result<&'a GitHubRelease, String> {
    if cache.is_none() {
        println!("Fetching latest IRIS release metadata...");
        *cache = Some(fetch_latest_release()?);
    }
    Ok(cache.as_ref().expect("populated release cache"))
}

fn ensure_release_archive<'a>(
    prefer_full: bool,
    release_cache: &mut Option<GitHubRelease>,
    archive_cache: &'a mut Option<(String, Vec<u8>)>,
) -> Result<(&'a str, &'a [u8]), String> {
    let want_full = prefer_full;
    let needs_download = match archive_cache {
        None => true,
        Some((name, _)) => want_full && name.contains("-lightweight"),
    };
    if needs_download {
        let release = ensure_release(release_cache)?;
        let triple = release_triple().ok_or_else(|| {
            format!(
                "no prebuilt IRIS archive for {}-{}",
                env::consts::ARCH,
                env::consts::OS
            )
        })?;
        let asset = if want_full {
            select_full_asset(&release.assets, triple)
        } else {
            select_asset(&release.assets, triple)
                .or_else(|| select_full_asset(&release.assets, triple))
        }
        .ok_or_else(|| {
            format!(
                "release {} does not contain an archive for {triple}",
                release.tag_name
            )
        })?;

        println!("Downloading {} ...", asset.browser_download_url);
        let bytes = download_url(&asset.browser_download_url, false)?;
        let digest = asset.digest.as_deref().ok_or_else(|| {
            format!(
                "release asset `{}` has no SHA-256 digest to verify",
                asset.name
            )
        })?;
        verify_digest(&bytes, digest)?;
        println!("Verified {digest}");
        *archive_cache = Some((asset.name.clone(), bytes));
    }
    let (name, bytes) = archive_cache.as_ref().expect("populated archive cache");
    Ok((name.as_str(), bytes.as_slice()))
}

fn clang_works(clang: &str) -> bool {
    Command::new(clang)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn toolchain_ready() -> bool {
    let tc = crate::codegen::build::toolchain_report();
    if !clang_works(&tc.clang) {
        return false;
    }
    #[cfg(target_os = "windows")]
    {
        if tc.sysroot_include.is_none() || tc.sysroot_lib.is_none() || tc.sysroot_gcc.is_none() {
            return false;
        }
    }
    true
}

fn install_toolchain(
    root: &Path,
    force: bool,
    release_cache: &mut Option<GitHubRelease>,
    archive_cache: &mut Option<(String, Vec<u8>)>,
) -> Result<(), String> {
    if !force && toolchain_ready() {
        let tc = crate::codegen::build::toolchain_report();
        println!("[ok] toolchain already ready ({})", tc.clang);
        return Ok(());
    }

    let (asset_name, archive) = ensure_release_archive(true, release_cache, archive_cache)?;
    let tc_dest = root.join("toolchain");
    let count = extract_archive_subdir(archive, asset_name, Some("toolchain"), &tc_dest)?;
    if count == 0 {
        return Err(format!(
            "archive `{asset_name}` did not contain a bundled toolchain/ directory"
        ));
    }
    let share_dir = root.join("share").join("iris");
    let _ = fs::create_dir_all(&share_dir);
    let llvm_ver = extract_file(archive, asset_name, "LLVM_VERSION")
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| LLVM_VERSION.trim().to_owned());
    let _ = fs::write(share_dir.join("LLVM_VERSION"), format!("{llvm_ver}\n"));
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("xattr").args(["-cr", &tc_dest.to_string_lossy()]).output();
    }
    println!(
        "[ok] installed bundled LLVM {llvm_ver} toolchain ({count} files) -> {}",
        tc_dest.display()
    );
    Ok(())
}

fn ui_host_filename() -> &'static str {
    if cfg!(target_os = "windows") {
        "iris_ui_host.dll"
    } else if cfg!(target_os = "macos") {
        "libiris_ui_host.dylib"
    } else {
        "libiris_ui_host.so"
    }
}

fn install_ui_host(
    root: &Path,
    force: bool,
    release_cache: &mut Option<GitHubRelease>,
    archive_cache: &mut Option<(String, Vec<u8>)>,
) -> Result<(), String> {
    if !force {
        if let Some(existing) = crate::codegen::app_bundle::find_installed_ui_host() {
            println!("[ok] ui host already available ({})", existing.display());
            return Ok(());
        }
    }

    let lib_name = ui_host_filename();
    let (asset_name, archive) = ensure_release_archive(false, release_cache, archive_cache)?;
    let bytes = extract_file(archive, asset_name, lib_name).map_err(|e| {
        format!(
            "{e} in `{asset_name}`; build locally with `cargo build --release -p iris_ui_host`"
        )
    })?;

    let bin_dir = root.join("bin");
    fs::create_dir_all(&bin_dir)
        .map_err(|e| format!("cannot create {}: {e}", bin_dir.display()))?;
    let dest = bin_dir.join(lib_name);
    fs::write(&dest, &bytes).map_err(|e| format!("failed to write {}: {e}", dest.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dest, fs::Permissions::from_mode(0o755));
    }
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("xattr").args(["-cr", &dest.to_string_lossy()]).output();
    }
    println!("[ok] installed {lib_name} -> {}", dest.display());
    Ok(())
}

fn find_editor_clis() -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut push_if = |p: PathBuf| {
        if !found.contains(&p) {
            found.push(p);
        }
    };

    for name in ["code", "code-insiders", "cursor", "codium"] {
        let cmd = if cfg!(windows) {
            format!("{name}.cmd")
        } else {
            name.to_owned()
        };
        if Command::new(&cmd).arg("--version").output().is_ok_and(|o| o.status.success()) {
            push_if(PathBuf::from(cmd));
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(lad) = env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()) {
            let lad_p = PathBuf::from(lad);
            for rel in [
                r"Programs\Microsoft VS Code\bin\code.cmd",
                r"Programs\Microsoft VS Code Insiders\bin\code-insiders.cmd",
                r"Programs\cursor\resources\app\bin\cursor.cmd",
            ] {
                let p = lad_p.join(rel);
                if p.is_file() {
                    push_if(p);
                }
            }
        }
        let pf = PathBuf::from(r"C:\Program Files\Microsoft VS Code\bin\code.cmd");
        if pf.is_file() {
            push_if(pf);
        }
    }

    found
}

fn install_vscode_extension(
    root: &Path,
    force: bool,
    explicit: bool,
    release_cache: &mut Option<GitHubRelease>,
) -> Result<(), String> {
    let editors = find_editor_clis();
    if editors.is_empty() {
        if explicit {
            return Err(
                "no supported editor CLI (`code`, `code-insiders`, `cursor`, `codium`) found on PATH".into(),
            );
        }
        println!("[skip] VS Code / Cursor CLI not detected (skipped extension setup)");
        return Ok(());
    }

    if !force {
        let already = editors.iter().any(|cli| {
            Command::new(cli)
                .arg("--list-extensions")
                .output()
                .ok()
                .is_some_and(|o| {
                    String::from_utf8_lossy(&o.stdout)
                        .lines()
                        .any(|l| l.to_ascii_lowercase().contains("iris-lang"))
                })
        });
        if already {
            println!("[ok] VS Code IRIS extension already installed");
            return Ok(());
        }
    }

    let share_dir = root.join("share").join("iris");
    fs::create_dir_all(&share_dir)
        .map_err(|e| format!("cannot create {}: {e}", share_dir.display()))?;

    // Use a cached .vsix in share_dir unless --force is set, else download latest release VSIX.
    let cached_vsix = fs::read_dir(&share_dir).ok().and_then(|rd| {
        rd.filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.starts_with("iris-lang-") && s.ends_with(".vsix"))
            })
            .max()
    });

    let vsix_path = match cached_vsix {
        Some(p) if !force => p,
        _ => {
            let release = ensure_release(release_cache)?;
            let vsix = select_vsix_asset(&release.assets).ok_or_else(|| {
                format!(
                    "release {} does not include a .vsix package",
                    release.tag_name
                )
            })?;
            println!("Downloading {} ...", vsix.browser_download_url);
            let bytes = download_url(&vsix.browser_download_url, false)?;
            if let Some(digest) = &vsix.digest {
                verify_digest(&bytes, digest)?;
                println!("Verified {digest}");
            }
            let out = share_dir.join(&vsix.name);
            fs::write(&out, &bytes)
                .map_err(|e| format!("failed to write {}: {e}", out.display()))?;
            out
        }
    };

    for cli in editors {
        let mut cmd = Command::new(&cli);
        cmd.arg("--install-extension").arg(&vsix_path);
        if force {
            cmd.arg("--force");
        }
        match cmd.output() {
            Ok(out) if out.status.success() => {
                println!(
                    "[ok] installed {} using {}",
                    vsix_path.display(),
                    cli.display()
                );
                return Ok(());
            }
            Ok(out) => {
                eprintln!(
                    "warning: {} extension install failed: {}",
                    cli.display(),
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            Err(e) => {
                eprintln!("warning: could not launch {}: {e}", cli.display());
            }
        }
    }

    if explicit {
        Err(format!(
            "downloaded {} but automatic installation failed; install manually via `code --install-extension {}`",
            vsix_path.display(),
            vsix_path.display()
        ))
    } else {
        Ok(())
    }
}

fn find_wasi_sysroot_installed(root: &Path) -> Option<PathBuf> {
    let base = root.join("toolchain").join("wasi-sysroot");
    if base.join("lib/wasm32-wasip1/libc.a").is_file() {
        return Some(base);
    }
    if let Ok(rd) = fs::read_dir(&base) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.join("lib/wasm32-wasip1/libc.a").is_file() {
                return Some(p);
            }
        }
    }
    None
}

fn install_wasm_sdk(root: &Path, force: bool) -> Result<(), String> {
    let dest = root.join("toolchain");
    if !force {
        if let Some(existing) = find_wasi_sysroot_installed(root) {
            println!("[ok] WASI sysroot already installed ({})", existing.display());
            return Ok(());
        }
    }
    let url = format!(
        "https://github.com/WebAssembly/wasi-sdk/releases/download/{WASI_SDK_TAG}/{WASI_SYSROOT_ARCHIVE}"
    );
    println!("Downloading WASI sysroot from {url} ...");
    let bytes = download_url(&url, false)?;
    let sysroot_base = dest.join("wasi-sysroot");
    let count = extract_archive_subdir(&bytes, WASI_SYSROOT_ARCHIVE, None, &sysroot_base)?;
    if count == 0 {
        return Err("WASI sysroot archive contained no files".into());
    }
    println!(
        "[ok] installed WASI sysroot ({count} files) -> {}",
        sysroot_base.display()
    );
    println!("Note: WASM component builds also use `wasm-tools` (`cargo install wasm-tools`).");
    Ok(())
}

fn onnxruntime_asset_name() -> Result<&'static str, String> {
    match (env::consts::OS, env::consts::ARCH) {
        ("windows", "x86_64") => Ok("onnxruntime-win-x64-1.20.1.zip"),
        ("windows", "aarch64") => Ok("onnxruntime-win-arm64-1.20.1.zip"),
        ("linux", "x86_64") => Ok("onnxruntime-linux-x64-1.20.1.tgz"),
        ("linux", "aarch64") => Ok("onnxruntime-linux-aarch64-1.20.1.tgz"),
        ("macos", "aarch64") => Ok("onnxruntime-osx-arm64-1.20.1.tgz"),
        ("macos", "x86_64") => Ok("onnxruntime-osx-x86_64-1.20.1.tgz"),
        (os, arch) => Err(format!("no prebuilt ONNX Runtime package configured for {os}/{arch}")),
    }
}

fn ort_lib_filename() -> &'static str {
    if cfg!(target_os = "windows") {
        "onnxruntime.dll"
    } else if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    }
}

fn install_ml_sdk(root: &Path, force: bool) -> Result<(), String> {
    let sdk_lib_dir = root.join("sdks").join("onnxruntime").join("lib");
    let lib_file = ort_lib_filename();
    if !force && sdk_lib_dir.join(lib_file).is_file() {
        println!(
            "[ok] ONNX Runtime already installed ({})",
            sdk_lib_dir.join(lib_file).display()
        );
        return Ok(());
    }
    let asset = onnxruntime_asset_name()?;
    let url = format!(
        "https://github.com/microsoft/onnxruntime/releases/download/v{ONNX_RUNTIME_VERSION}/{asset}"
    );
    println!("Downloading ONNX Runtime {ONNX_RUNTIME_VERSION} from {url} ...");
    let bytes = download_url(&url, false)?;
    let archive_kind = if asset.ends_with(".zip") {
        asset.to_owned()
    } else {
        asset.replace(".tgz", ".tar.gz")
    };
    let count = extract_archive_subdir(&bytes, &archive_kind, Some("lib"), &sdk_lib_dir)?;
    if count == 0 {
        return Err(format!("ONNX Runtime archive `{asset}` did not contain lib/"));
    }
    println!(
        "[ok] installed ONNX Runtime ({count} files) -> {}",
        sdk_lib_dir.display()
    );
    Ok(())
}

fn print_component_status(root: &Path) {
    let tc = crate::codegen::build::toolchain_report();
    let clang_ok = clang_works(&tc.clang);
    println!("IRIS Setup Components (prefix: {})\n", root.display());

    let tc_status = if clang_ok {
        format!("installed ({})", tc.clang)
    } else {
        "not found (run `iris setup toolchain`)".to_owned()
    };
    println!("  toolchain : {tc_status}");
    #[cfg(target_os = "windows")]
    println!(
        "  sysroot   : {}",
        tc.sysroot_include
            .as_deref()
            .unwrap_or("not found (run `iris setup toolchain`)")
    );

    let ui_status = match tc.ui_host {
        Some(p) => format!("installed ({})", p.display()),
        None => "not found (run `iris setup ui`)".to_owned(),
    };
    println!("  ui        : {ui_status}");

    let editors = find_editor_clis();
    let vs_status = if editors.is_empty() {
        "editor CLI not found".to_owned()
    } else {
        format!(
            "editor detected ({}); run `iris setup vscode` to install/update",
            editors[0].display()
        )
    };
    println!("  vscode    : {vs_status}");

    let wasm_status = find_wasi_sysroot_installed(root)
        .map(|p| format!("installed ({})", p.display()))
        .unwrap_or_else(|| "not installed (optional: `iris setup wasm`)".to_owned());
    println!("  wasm      : {wasm_status}");

    let ort_path = root.join("sdks").join("onnxruntime").join("lib").join(ort_lib_filename());
    let ml_status = if ort_path.is_file() {
        format!("installed ({})", ort_path.display())
    } else {
        "not installed (optional: `iris setup ml`)".to_owned()
    };
    println!("  ml        : {ml_status}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_component_parses_aliases() {
        assert_eq!(SetupComponent::parse("base").unwrap(), SetupComponent::Base);
        assert_eq!(
            SetupComponent::parse("LLVM").unwrap(),
            SetupComponent::Toolchain
        );
        assert_eq!(SetupComponent::parse("ui-host").unwrap(), SetupComponent::Ui);
        assert_eq!(SetupComponent::parse("vsix").unwrap(), SetupComponent::Vscode);
        assert_eq!(SetupComponent::parse("wasi").unwrap(), SetupComponent::Wasm);
        assert_eq!(SetupComponent::parse("onnx").unwrap(), SetupComponent::Ml);
        assert_eq!(SetupComponent::parse("status").unwrap(), SetupComponent::List);
        assert!(SetupComponent::parse("unknown-xyz").is_err());
    }

    #[test]
    fn expand_components_resolves_base_and_all() {
        assert_eq!(
            expand_components(&[]),
            vec![
                SetupComponent::Toolchain,
                SetupComponent::Ui,
                SetupComponent::Vscode
            ]
        );
        assert_eq!(
            expand_components(&[SetupComponent::All]),
            vec![
                SetupComponent::Toolchain,
                SetupComponent::Ui,
                SetupComponent::Vscode,
                SetupComponent::Wasm,
                SetupComponent::Ml
            ]
        );
        assert_eq!(
            expand_components(&[SetupComponent::Ui, SetupComponent::List]),
            vec![SetupComponent::List]
        );
    }

    #[test]
    fn default_install_root_is_non_empty() {
        assert!(!default_install_root().as_os_str().is_empty());
    }
}
