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
use std::path::Path;
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

/// The file `name` (by its last path component) inside a `.zip`: the
/// central directory names it, its local header precedes its data, stored
/// or deflated.
pub fn from_zip(archive: &[u8], name: &str) -> Result<Vec<u8>, String> {
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
        if entry_name.rsplit(['/', '\\']).next() != Some(name) {
            continue;
        }
        if compressed == 0xFFFF_FFFF || size == 0xFFFF_FFFF {
            return Err(format!("`{entry_name}` is a zip64 entry"));
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
        return Ok(data);
    }
    Err(format!("no `{name}` in the archive"))
}

/// The `iris` executable inside a release archive called `asset_name`.
pub fn extract_executable(archive: &[u8], asset_name: &str) -> Result<Vec<u8>, String> {
    let exe = if cfg!(windows) { "iris.exe" } else { "iris" };
    if asset_name.ends_with(".zip") {
        from_zip(archive, exe)
    } else {
        from_tar_gz(archive, exe)
    }
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
    let asset = select_asset(&release.assets, triple)
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
    println!("Upgraded IRIS to v{latest}.");
    Ok(())
}
