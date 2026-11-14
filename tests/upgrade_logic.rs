//! Integration tests for the compiler self-upgrade functionality.

use std::io::Write;

use iris::upgrade::{
    extract_executable, from_zip, select_asset, verify_digest, GitHubAsset, SemVer,
};

#[test]
fn test_integration_semver_parsing() {
    let current = SemVer::parse("v0.6.0").expect("parse current");
    let remote = SemVer::parse("0.6.1").expect("parse remote");
    let remote_patch = SemVer::parse("v0.6.2-alpha").expect("parse alpha");

    assert!(remote > current);
    assert!(remote_patch > remote);
    assert_eq!(current.major, 0);
    assert_eq!(current.minor, 6);
    assert_eq!(current.patch, 0);
}

#[test]
fn test_integration_semver_invalid() {
    assert!(SemVer::parse("v").is_none());
    assert!(SemVer::parse("0").is_none());
    assert!(SemVer::parse("invalid-version").is_none());
}

#[test]
fn pre_releases_order_before_their_release() {
    let rc1 = SemVer::parse("v1.0.0-rc1").unwrap();
    let rc2 = SemVer::parse("1.0.0-rc2").unwrap();
    let release = SemVer::parse("v1.0.0").unwrap();
    assert!(rc1 < rc2 && rc2 < release);
    assert_eq!(
        SemVer::parse("1.2").unwrap(),
        SemVer::parse("1.2.0").unwrap()
    );
}

fn asset(name: &str) -> GitHubAsset {
    GitHubAsset {
        name: name.into(),
        browser_download_url: format!("https://example/{name}"),
        digest: None,
    }
}

#[test]
fn the_asset_is_this_triples_lightweight_archive() {
    // The names of v1.0.0-rc2's assets.
    let assets: Vec<GitHubAsset> = [
        "IRIS-1.0.0-rc2-windows-x64-portable.zip",
        "IRIS-1.0.0-rc2-windows-x64-setup.exe",
        "iris-v1.0.0-rc2-aarch64-pc-windows-msvc-lightweight.zip",
        "iris-v1.0.0-rc2-aarch64-pc-windows-msvc.zip",
        "iris-v1.0.0-rc2-x86_64-pc-windows-msvc-lightweight.zip",
        "iris-v1.0.0-rc2-x86_64-pc-windows-msvc.zip",
        "iris-v1.0.0-rc2-x86_64-unknown-linux-gnu.tar.gz",
        "iris-v1.0.0-rc2-x86_64-unknown-linux-gnu-lightweight.tar.gz",
        "iris_1.0.0-rc2_amd64.deb",
    ]
    .iter()
    .map(|n| asset(n))
    .collect();
    assert_eq!(
        select_asset(&assets, "x86_64-pc-windows-msvc")
            .unwrap()
            .name,
        "iris-v1.0.0-rc2-x86_64-pc-windows-msvc-lightweight.zip"
    );
    assert_eq!(
        select_asset(&assets, "x86_64-unknown-linux-gnu")
            .unwrap()
            .name,
        "iris-v1.0.0-rc2-x86_64-unknown-linux-gnu-lightweight.tar.gz"
    );
    // No lightweight archive: the full one.
    let full: Vec<GitHubAsset> = vec![asset("iris-v2.0.0-aarch64-apple-darwin.tar.gz")];
    assert_eq!(
        select_asset(&full, "aarch64-apple-darwin").unwrap().name,
        full[0].name
    );
    assert!(select_asset(&assets, "riscv64gc-unknown-linux-gnu").is_none());
}

#[test]
fn a_download_must_match_its_digest() {
    let digest = "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"; // "hello"
    assert!(verify_digest(b"hello", digest).is_ok());
    assert!(verify_digest(b"hellO", digest)
        .unwrap_err()
        .contains("does not match"));
}

/// A zip of `files`, deflated or stored, as release archives are.
fn zip(files: &[(&str, &[u8], bool)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data, deflate) in files {
        let body = if *deflate {
            let mut e =
                flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(data).unwrap();
            e.finish().unwrap()
        } else {
            data.to_vec()
        };
        let method: u16 = if *deflate { 8 } else { 0 };
        let local = out.len() as u32;
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&[20, 0, 0, 0]);
        out.extend_from_slice(&method.to_le_bytes());
        out.extend_from_slice(&[0; 8]); // time, date, crc
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&body);
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&[20, 0, 20, 0, 0, 0]);
        central.extend_from_slice(&method.to_le_bytes());
        central.extend_from_slice(&[0; 8]);
        central.extend_from_slice(&(body.len() as u32).to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0; 12]); // extra, comment, disk, attrs
        central.extend_from_slice(&local.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let cd_at = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_at.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out
}

#[test]
fn the_executable_is_taken_out_of_a_zip() {
    let exe = b"MZ fake executable ".repeat(100);
    let archive = zip(&[
        ("iris/README.md", b"read me", false),
        ("iris/bin/iris.exe", &exe, true),
        ("iris/bin/iris", &exe, false),
    ]);
    assert_eq!(from_zip(&archive, "iris.exe").unwrap(), exe);
    assert_eq!(from_zip(&archive, "iris").unwrap(), exe);
    assert!(from_zip(&archive, "other")
        .unwrap_err()
        .contains("no `other`"));
}

#[test]
fn the_executable_is_taken_out_of_a_tar_gz() {
    let exe = b"\x7fELF fake".repeat(50);
    let name = if cfg!(windows) { "iris.exe" } else { "iris" };
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    let mut h = tar::Header::new_gnu();
    h.set_size(exe.len() as u64);
    h.set_mode(0o755);
    h.set_cksum();
    tar.append_data(&mut h, format!("iris-v9/bin/{name}"), &exe[..])
        .unwrap();
    let archive = tar.into_inner().unwrap().finish().unwrap();
    assert_eq!(
        extract_executable(&archive, "iris-v9-x.tar.gz").unwrap(),
        exe
    );
}

/// The real thing: this platform's latest release archive, downloaded,
/// verified against GitHub's digest, and its executable run. Network.
#[test]
#[ignore]
fn the_latest_release_downloads_verifies_and_runs() {
    let release = iris::upgrade::fetch_latest_release().expect("GitHub");
    let triple = iris::upgrade::release_triple().expect("a released platform");
    let asset = select_asset(&release.assets, triple).expect("an archive for this platform");
    let out = std::process::Command::new(if cfg!(windows) { "curl.exe" } else { "curl" })
        .args(["-fsSL", &asset.browser_download_url])
        .output()
        .unwrap();
    assert!(out.status.success());
    verify_digest(
        &out.stdout,
        asset.digest.as_deref().expect("GitHub publishes digests"),
    )
    .unwrap();
    let exe = extract_executable(&out.stdout, &asset.name).unwrap();
    let path = std::env::temp_dir().join(format!(
        "iris_upgrade_probe{}",
        std::env::consts::EXE_SUFFIX
    ));
    std::fs::write(&path, &exe).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let v = std::process::Command::new(&path)
        .arg("--version")
        .output()
        .expect("runs");
    let _ = std::fs::remove_file(&path);
    assert!(
        String::from_utf8_lossy(&v.stdout).contains("iris"),
        "{:?}",
        v
    );
}
