//! `iris upgrade`: replace this compiler with the latest release.
//!
//! Asks GitHub for the latest release of the IRIS repository, takes the
//! archive built for this machine's target triple -- the lightweight one,
//! which holds the compiler alone, when there is one -- checks it against
//! the SHA-256 digest GitHub publishes for it, takes the `iris` executable
//! out of it (`.zip` or `.tar.gz`), and swaps it in for the running one,
//! keeping the old one until the swap has succeeded.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

/// The repository whose releases are IRIS's.
pub const RELEASE_REPO: &str = "Eclipse-Softworks/IRIS";

#[derive(Deserialize, Debug, Clone)]
pub struct GitHubRelease {
    pub tag_name: String,
    pub assets: Vec<GitHubAsset>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct GitHubAsset {
    pub name: String,
    pub browser_download_url: String,
    /// `sha256:<hex>`, which GitHub computes for every asset.
    #[serde(default)]
    pub digest: Option<String>,
}

/// A release version: semver, pre-releases ordered before their release
/// (`1.0.0-rc1 < 1.0.0-rc2 < 1.0.0`). A leading `v` is accepted, and
/// `MAJOR.MINOR` means `MAJOR.MINOR.0`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemVer {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    version: semver::Version,
}

impl SemVer {
    pub fn parse(s: &str) -> Option<Self> {
        let clean = s.trim().trim_start_matches('v');
        let core_end = clean.find(['-', '+']).unwrap_or(clean.len());
        let full = match clean[..core_end].split('.').count() {
            2 => format!("{}.0{}", &clean[..core_end], &clean[core_end..]),
            _ => clean.to_owned(),
        };
        let version = semver::Version::parse(&full).ok()?;
        Some(Self {
            major: version.major,
            minor: version.minor,
            patch: version.patch,
            version,
        })
    }
}

impl PartialOrd for SemVer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SemVer {
    fn cmp(&self, other: &Self) -> Ordering {
        self.version.cmp(&other.version)
    }
}

impl std::fmt::Display for SemVer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.version)
    }
}

/// The target triple this machine's release archives are built for.
pub fn release_triple() -> Option<&'static str> {
    Some(match (env::consts::ARCH, env::consts::OS) {
        ("x86_64", "windows") => "x86_64-pc-windows-msvc",
        ("aarch64", "windows") => "aarch64-pc-windows-msvc",
        ("x86_64", "linux") => "x86_64-unknown-linux-gnu",
        ("aarch64", "linux") => "aarch64-unknown-linux-gnu",
        ("x86_64", "macos") => "x86_64-apple-darwin",
        ("aarch64", "macos") => "aarch64-apple-darwin",
        _ => return None,
    })
}

/// The archive for `triple`: `iris-vX-TRIPLE-lightweight.{zip,tar.gz}` (the
/// compiler alone) first, else the full `iris-vX-TRIPLE.{zip,tar.gz}`.
pub fn select_asset<'a>(assets: &'a [GitHubAsset], triple: &str) -> Option<&'a GitHubAsset> {
    let archive = |n: &str| n.ends_with(".zip") || n.ends_with(".tar.gz");
    let light = format!("-{triple}-lightweight.");
    let full = format!("-{triple}.");
    assets
        .iter()
        .find(|a| a.name.contains(&light) && archive(&a.name))
        .or_else(|| {
            assets
                .iter()
                .find(|a| a.name.contains(&full) && archive(&a.name))
        })
}

/// Check `bytes` against `digest` (`sha256:<hex>`).
pub fn verify_digest(bytes: &[u8], digest: &str) -> Result<(), String> {
    let want = digest
        .strip_prefix("sha256:")
        .ok_or_else(|| format!("unsupported digest `{digest}`"))?;
    let got: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if got.eq_ignore_ascii_case(want) {
        Ok(())
    } else {
        Err(format!("the download does not match its published digest (expected sha256:{want}, got sha256:{got})"))
    }
}

fn curl(url: &str, api: bool) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new(if cfg!(windows) { "curl.exe" } else { "curl" });
    cmd.args(["-fsSL", "-H", "User-Agent: iris-upgrade"]);
    if api {
        cmd.args(["-H", "Accept: application/vnd.github+json"]);
    }
    let out = cmd
        .arg(url)
        .output()
        .map_err(|e| format!("curl could not start: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{url}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

pub fn fetch_latest_release() -> Result<GitHubRelease, String> {
    let json = curl(
        &format!("https://api.github.com/repos/{RELEASE_REPO}/releases/latest"),
        true,
    )?;
    serde_json::from_slice(&json).map_err(|e| format!("unexpected GitHub release JSON: {e}"))
}

/// The full archive for `triple`: `iris-vX-TRIPLE.{zip,tar.gz}` (includes toolchain).
pub fn select_full_asset<'a>(assets: &'a [GitHubAsset], triple: &str) -> Option<&'a GitHubAsset> {
    let archive = |n: &str| n.ends_with(".zip") || n.ends_with(".tar.gz");
    let full = format!("-{triple}.");
    assets
        .iter()
        .find(|a| a.name.contains(&full) && !a.name.contains("-lightweight") && archive(&a.name))
}

/// The VS Code extension package (`iris-lang-*.vsix`) attached to a release.
pub fn select_vsix_asset<'a>(assets: &'a [GitHubAsset]) -> Option<&'a GitHubAsset> {
    assets
        .iter()
        .find(|a| a.name.starts_with("iris-lang-") && a.name.ends_with(".vsix"))
}

/// Download raw bytes from `url` via system curl.
pub fn download_url(url: &str, api: bool) -> Result<Vec<u8>, String> {
    curl(url, api)
}

/// Sanitize a relative archive path to reject absolute roots, drive letters, and `..`.
fn sanitize_rel_path(raw: &str) -> Option<PathBuf> {
    let normalized = raw.replace('\\', "/");
    let trimmed = normalized.trim_start_matches("./").trim_end_matches('/');
    if trimmed.is_empty() || trimmed.starts_with('/') {
        return None;
    }
    let mut out = PathBuf::new();
    for comp in Path::new(trimmed).components() {
        match comp {
            Component::Normal(part) => {
                let s = part.to_string_lossy();
                if s.contains(':') || s.contains('\0') {
                    return None;
                }
                out.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Resolve the relative output path for an archive entry matching an optional directory prefix.
/// Accepts `<prefix>/...` as well as `<top-wrapper>/<prefix>/...` so archives with or without a
/// top-level container folder extract cleanly into `dest_dir`.
fn match_subdir_entry(raw_entry: &str, subdir_prefix: Option<&str>) -> Option<PathBuf> {
    let normalized = raw_entry.replace('\\', "/");
    if normalized.ends_with('/') {
        return None;
    }
    let clean = sanitize_rel_path(&normalized)?;
    let Some(prefix) = subdir_prefix
        .map(|p| p.trim_matches('/'))
        .filter(|p| !p.is_empty())
    else {
        return Some(clean);
    };
    let clean_str = clean.to_string_lossy().replace('\\', "/");
    let prefix_slash = format!("{prefix}/");
    if let Some(rest) = clean_str.strip_prefix(&prefix_slash) {
        return sanitize_rel_path(rest);
    }
    if let Some((_top, tail)) = clean_str.split_once('/') {
        if let Some(rest) = tail.strip_prefix(&prefix_slash) {
            return sanitize_rel_path(rest);
        }
    }
    None
}

fn iter_zip_entries<F>(archive: &[u8], mut visitor: F) -> Result<(), String>
where
    F: FnMut(&str, &[u8]) -> Result<bool, String>,
{
    let u16_at = |at: usize| {
        archive
            .get(at..at + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]) as usize)
    };
    let u32_at = |at: usize| {
        archive
            .get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
    };
    let bad = || "not a readable zip archive".to_string();
    // End of central directory: signature 0x06054b50, within the last 64 KiB.
    let eocd = (0..archive.len().saturating_sub(21))
        .rev()
        .take(65_557)
        .find(|&i| archive[i..i + 4] == [0x50, 0x4b, 0x05, 0x06])
        .ok_or_else(bad)?;
    let entries = u16_at(eocd + 10).ok_or_else(bad)?;
    let mut at = u32_at(eocd + 16).ok_or_else(bad)?;
    for _ in 0..entries {
        if u32_at(at) != Some(0x0201_4b50) {
            return Err(bad());
        }
        let method = u16_at(at + 10).ok_or_else(bad)?;
        let compressed = u32_at(at + 20).ok_or_else(bad)?;
        let size = u32_at(at + 24).ok_or_else(bad)?;
        let name_len = u16_at(at + 28).ok_or_else(bad)?;
        let extra_len = u16_at(at + 30).ok_or_else(bad)?;
        let comment_len = u16_at(at + 32).ok_or_else(bad)?;
        let local = u32_at(at + 42).ok_or_else(bad)?;
        let entry_name =
            String::from_utf8_lossy(archive.get(at + 46..at + 46 + name_len).ok_or_else(bad)?)
                .into_owned();
        at += 46 + name_len + extra_len + comment_len;
        if compressed == 0xFFFF_FFFF || size == 0xFFFF_FFFF {
            return Err(format!("`{entry_name}` is a zip64 entry"));
        }
        if entry_name.ends_with('/') || entry_name.ends_with('\\') {
            continue;
        }
        if u32_at(local) != Some(0x0403_4b50) {
            return Err(bad());
        }
        let data_at = local
            + 30
            + u16_at(local + 26).ok_or_else(bad)?
            + u16_at(local + 28).ok_or_else(bad)?;
        let raw = archive.get(data_at..data_at + compressed).ok_or_else(bad)?;
        let data = match method {
            0 => raw.to_vec(),
            8 => {
                let mut out = Vec::with_capacity(size);
                flate2::read::DeflateDecoder::new(raw)
                    .read_to_end(&mut out)
                    .map_err(|e| e.to_string())?;
                out
            }
            m => return Err(format!("`{entry_name}` uses zip compression method {m}")),
        };
        if data.len() != size {
            return Err(format!(
                "`{entry_name}` unpacked to {} bytes, not {size}",
                data.len()
            ));
        }
        if !visitor(&entry_name, &data)? {
            break;
        }
    }
    Ok(())
}

/// The file `name` (by its last path component) inside a `.tar.gz`.
fn from_tar_gz(archive: &[u8], name: &str) -> Result<Vec<u8>, String> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    for entry in tar.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?.into_owned();
        if path.file_name().and_then(|f| f.to_str()) == Some(name)
            && entry.header().entry_type().is_file()
        {
            let mut data = Vec::new();
            entry.read_to_end(&mut data).map_err(|e| e.to_string())?;
            return Ok(data);
        }
    }
    Err(format!("no `{name}` in the archive"))
}

/// The file `name` (by its last path component) inside a `.zip`.
pub fn from_zip(archive: &[u8], name: &str) -> Result<Vec<u8>, String> {
    let mut found: Option<Vec<u8>> = None;
    iter_zip_entries(archive, |entry_name, data| {
        if entry_name.rsplit(['/', '\\']).next() == Some(name) {
            found = Some(data.to_vec());
            return Ok(false);
        }
        Ok(true)
    })?;
    found.ok_or_else(|| format!("no `{name}` in the archive"))
}

/// Extract a single file matching `target_filename` (by basename) from a `.zip` or `.tar.gz`.
pub fn extract_file(
    archive: &[u8],
    asset_name: &str,
    target_filename: &str,
) -> Result<Vec<u8>, String> {
    if asset_name.ends_with(".zip") {
        from_zip(archive, target_filename)
    } else {
        from_tar_gz(archive, target_filename)
    }
}

/// Safely extract all files inside `subdir_prefix` (or all files if `None`) from `.zip` or `.tar.gz`
/// into `dest_dir`, rejecting path traversal entries. Returns the number of extracted files.
pub fn extract_archive_subdir(
    archive: &[u8],
    asset_name: &str,
    subdir_prefix: Option<&str>,
    dest_dir: &Path,
) -> Result<usize, String> {
    let mut extracted = 0usize;
    let write_entry = |rel: PathBuf, data: &[u8], executable_hint: bool| -> Result<(), String> {
        let out_path = dest_dir.join(&rel);
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create directory {}: {e}", parent.display()))?;
        }
        fs::write(&out_path, data)
            .map_err(|e| format!("failed to write {}: {e}", out_path.display()))?;
        #[cfg(unix)]
        if executable_hint
            || rel
                .components()
                .any(|c| matches!(c, Component::Normal(s) if s == "bin"))
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&out_path, fs::Permissions::from_mode(0o755));
        }
        #[cfg(not(unix))]
        let _ = executable_hint;
        Ok(())
    };

    if asset_name.ends_with(".zip") {
        iter_zip_entries(archive, |entry_name, data| {
            // Check raw safety first so traversal entries outside subdir_prefix are still spotted cleanly.
            if Path::new(&entry_name.replace('\\', "/"))
                .components()
                .any(|c| matches!(c, Component::ParentDir))
            {
                return Err(format!("unsafe archive entry path rejected: {entry_name}"));
            }
            if let Some(rel) = match_subdir_entry(entry_name, subdir_prefix) {
                let exec = entry_name.contains("/bin/")
                    || entry_name.ends_with(".so")
                    || entry_name.ends_with(".dylib");
                write_entry(rel, data, exec)?;
                extracted += 1;
            }
            Ok(true)
        })?;
    } else {
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
        for entry in tar.entries().map_err(|e| e.to_string())? {
            let mut entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path().map_err(|e| e.to_string())?.into_owned();
            let raw = path.to_string_lossy().into_owned();
            if path.components().any(|c| matches!(c, Component::ParentDir)) {
                return Err(format!("unsafe archive entry path rejected: {raw}"));
            }
            if !entry.header().entry_type().is_file() {
                continue;
            }
            let Some(rel) = match_subdir_entry(&raw, subdir_prefix) else {
                continue;
            };
            let mode = entry.header().mode().unwrap_or(0o644);
            let mut data = Vec::new();
            entry.read_to_end(&mut data).map_err(|e| e.to_string())?;
            write_entry(rel, &data, (mode & 0o111) != 0)?;
            extracted += 1;
        }
    }

    Ok(extracted)
}

/// The `iris` executable inside a release archive called `asset_name`.
pub fn extract_executable(archive: &[u8], asset_name: &str) -> Result<Vec<u8>, String> {
    let exe = if cfg!(windows) { "iris.exe" } else { "iris" };
    extract_file(archive, asset_name, exe)
}

/// Put `new_bin_path` where `current_exe_path` is, keeping the old one
/// until the new one is in place.
fn perform_swap(new_bin_path: &Path, current_exe_path: &Path) -> Result<(), String> {
    let old_exe_path = current_exe_path.with_extension("old");
    if old_exe_path.exists() {
        let _ = fs::remove_file(&old_exe_path);
    }

    fs::rename(current_exe_path, &old_exe_path)
        .map_err(|e| format!("cannot move the running executable aside: {e}"))?;

    if let Err(e) = fs::rename(new_bin_path, current_exe_path) {
        let _ = fs::rename(&old_exe_path, current_exe_path);
        return Err(format!("cannot put the new executable in place: {e}"));
    }

    // The old one may still be running (it is, on Windows): delete it after exit.
    if cfg!(target_os = "windows") {
        let cmd = format!(
            "Start-Sleep -Seconds 1; Remove-Item -LiteralPath '{}' -Force",
            old_exe_path.to_string_lossy()
        );
        let _ = Command::new("powershell")
            .args(["-NoProfile", "-Command", &cmd])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    } else {
        let _ = fs::remove_file(&old_exe_path);
    }
    Ok(())
}

pub fn run_upgrade_command(check: bool, yes: bool, force: bool) -> Result<(), String> {
    run_upgrade_command_with_options(check, yes, force, false)
}

fn install_prefix_for_exe(exe_path: &Path) -> Option<PathBuf> {
    let dir = exe_path.parent()?;
    if dir.file_name().and_then(|s| s.to_str()) == Some("bin") {
        dir.parent().map(Path::to_path_buf)
    } else {
        Some(dir.to_path_buf())
    }
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

pub fn run_upgrade_command_with_options(
    check: bool,
    yes: bool,
    force: bool,
    full: bool,
) -> Result<(), String> {
    let current_ver_str = env!("CARGO_PKG_VERSION");
    let current = SemVer::parse(current_ver_str)
        .ok_or_else(|| format!("invalid current version: {current_ver_str}"))?;

    println!("Current IRIS version: v{current}");
    println!("Checking for the latest release of {RELEASE_REPO}...");
    let release = fetch_latest_release()?;
    let latest = SemVer::parse(&release.tag_name)
        .ok_or_else(|| format!("invalid release tag: {}", release.tag_name))?;

    if !force && latest <= current {
        println!("IRIS is up to date: v{current}");
        return Ok(());
    }
    println!("New version: v{latest} (current: v{current})");
    if check {
        return Ok(());
    }

    let triple = release_triple().ok_or_else(|| {
        format!(
            "no IRIS release is built for {}-{}",
            env::consts::ARCH,
            env::consts::OS
        )
    })?;
    let asset = if full {
        select_full_asset(&release.assets, triple).or_else(|| select_asset(&release.assets, triple))
    } else {
        select_asset(&release.assets, triple)
    }
    .ok_or_else(|| format!("release {} has no archive for {triple}", release.tag_name))?;

    if !yes {
        print!("Upgrade to v{latest} from {}? [y/N]: ", asset.name);
        io::stdout().flush().map_err(|e| e.to_string())?;
        let mut response = String::new();
        io::stdin()
            .read_line(&mut response)
            .map_err(|e| e.to_string())?;
        if !response.trim().to_lowercase().starts_with('y') {
            println!("Upgrade cancelled.");
            return Ok(());
        }
    }

    println!("Downloading {} ...", asset.browser_download_url);
    let archive = curl(&asset.browser_download_url, false)?;
    match &asset.digest {
        Some(d) => {
            verify_digest(&archive, d)?;
            println!("Verified {d}");
        }
        None => {
            return Err(format!(
                "{} has no published digest to verify it against",
                asset.name
            ))
        }
    }
    let binary = extract_executable(&archive, &asset.name)?;

    let current_exe =
        env::current_exe().map_err(|e| format!("cannot find the running executable: {e}"))?;
    let staged = current_exe.with_extension("new");
    fs::write(&staged, &binary).map_err(|e| format!("cannot write {}: {e}", staged.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&staged, fs::Permissions::from_mode(0o755));
    }
    println!("Applying update...");
    if let Err(e) = perform_swap(&staged, &current_exe) {
        let _ = fs::remove_file(&staged);
        return Err(e);
    }

    // Update sibling iris_ui_host shared library if bundled in the release archive.
    let ui_name = ui_host_filename();
    if let Ok(ui_bytes) = extract_file(&archive, &asset.name, ui_name) {
        if let Some(exe_dir) = current_exe.parent() {
            let ui_dest = exe_dir.join(ui_name);
            let ui_staged = exe_dir.join(format!("{ui_name}.new"));
            if fs::write(&ui_staged, &ui_bytes).is_ok() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = fs::set_permissions(&ui_staged, fs::Permissions::from_mode(0o755));
                }
                if fs::rename(&ui_staged, &ui_dest).is_err() {
                    let _ = fs::remove_file(&ui_staged);
                } else {
                    println!("Updated {ui_name}.");
                }
            }
        }
    }

    // Refresh or notify on bundled LLVM toolchain changes.
    if let Some(prefix) = install_prefix_for_exe(&current_exe) {
        let remote_llvm = extract_file(&archive, &asset.name, "LLVM_VERSION")
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        let local_llvm = fs::read_to_string(prefix.join("share/iris/LLVM_VERSION"))
            .or_else(|_| fs::read_to_string(prefix.join("LLVM_VERSION")))
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| crate::setup::LLVM_VERSION.trim().to_owned());

        if full {
            let tc_dest = prefix.join("toolchain");
            match extract_archive_subdir(&archive, &asset.name, Some("toolchain"), &tc_dest) {
                Ok(n) if n > 0 => {
                    if let Some(ver) = &remote_llvm {
                        let share = prefix.join("share").join("iris");
                        let _ = fs::create_dir_all(&share);
                        let _ = fs::write(share.join("LLVM_VERSION"), format!("{ver}\n"));
                    }
                    println!("Refreshed toolchain/ ({n} files).");
                }
                Ok(_) => println!("No toolchain directory present in {}.", asset.name),
                Err(e) => eprintln!("warning: could not update toolchain: {e}"),
            }
        } else if let Some(new_ver) = remote_llvm {
            if new_ver != local_llvm {
                println!(
                    "Note: this release uses LLVM {new_ver} (installed: {local_llvm}). \
                     Run `iris setup toolchain` or `iris upgrade --full` to refresh your toolchain."
                );
            }
        }
    }

    println!("Upgraded IRIS to v{latest}.");
    Ok(())
}
