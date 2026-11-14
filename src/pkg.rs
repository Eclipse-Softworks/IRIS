//! IRIS Package Manager (`iris pkg`).
//!
//! Supports local path, git, and registry-based dependencies.
//!
//! ## Manifest format (`iris.toml`)
//!
//! ```toml
//! [package]
//! name    = "my-project"
//! version = "0.1.0"
//! entry   = "main.iris"
//! # Optional: the registry for registry deps (default: the IRIS registry)
//! registry = "https://raw.githubusercontent.com/Eclipse-Softworks/IRIS/master/registry"
//!
//! [dependencies]
//! utils = { path = "../shared/utils" }
//! web   = { git = "https://github.com/user/iris-web.git" }
//! auth  = { git = "https://github.com/user/iris-auth.git", tag  = "v1.2.0" }
//! core  = { git = "https://github.com/user/iris-core.git", rev  = "a1b2c3d" }
//! dev   = { git = "https://github.com/user/iris-dev.git",  branch = "main" }
//! json  = { registry = "json", version = "^1.0.0" }
//! ```
//!
//! ## Lock file (`iris.lock`)
//!
//! Auto-generated next to `iris.toml`. Commit it to source control for
//! reproducible builds. Records git commit SHAs and content checksums.
//!
//! ## Commands
//!
//! - `iris pkg init`                  — create a new `iris.toml`
//! - `iris pkg add <n> --path <p>`    — add a local path dependency
//! - `iris pkg add <n> --git <url>`   — add a git dependency
//! - `iris pkg add <n> --registry <pkg> --version <req>` — registry dep
//! - `iris pkg remove <name>`         — remove a dependency
//! - `iris pkg install [--offline]`   — fetch/sync all deps into `.iris/deps/`
//! - `iris pkg update [name]`         — update deps to latest matching ref
//! - `iris pkg list`                  — list current dependencies
//! - `iris pkg check`                 — verify all deps are installed
//! - `iris pkg build [--offline]`     — install deps + build entry binary
//! - `iris pkg run [--offline]`       — build + run
//! - `iris pkg vendor`                — download all deps for offline builds
//! - `iris pkg publish [--registry R] [--dry-run]` — publish this package
//! - `iris pkg search <text> [--registry R]` — find packages
//! - `iris pkg serve <dir> [--port N]` — serve a registry directory over HTTP
//!
//! ## Registries
//!
//! A registry is a directory -- or the same directory served over HTTP(S),
//! or a `file://` URL to it -- holding a JSON index and the packages'
//! archives:
//! ```text
//! index.json
//! pkgs/json-1.0.0.tar.gz
//! ```
//! ```json
//! {
//!   "packages": {
//!     "json": {
//!       "description": "...",
//!       "versions": {
//!         "1.0.0": { "url": "pkgs/json-1.0.0.tar.gz", "checksum": "sha256:..." }
//!       }
//!     }
//!   }
//! }
//! ```
//! A relative `url` is resolved against the registry, so a registry moves
//! as a whole. `iris pkg publish` adds a version to a registry directory --
//! a version, once published, is never replaced -- and a directory pushed
//! to a web host or a git repository's raw files is an HTTP registry. The
//! default registry is `registry/` of the IRIS repository; `IRIS_REGISTRY`,
//! or `registry` in `[package]`, chooses another.

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

const VENDOR_DIR: &str = ".iris/vendor";

// ── Manifest types ────────────────────────────────────────────────────────────

/// Parsed `iris.toml` manifest.
#[derive(Debug, Clone)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub entry: String,
    pub description: String,
    pub license: String,
    pub repository: String,
    /// Default registry URL for registry deps (optional).
    pub registry: Option<String>,
    pub deps: BTreeMap<String, Dep>,
}

/// A dependency source.
#[derive(Debug, Clone)]
pub enum Dep {
    /// `name = { path = "..." }`
    Path(String),
    /// `name = { git = "...", [branch = "..."], [tag = "..."], [rev = "..."] }`
    Git {
        url: String,
        branch: Option<String>,
        tag: Option<String>,
        rev: Option<String>,
    },
    /// `name = { registry = "...", version = "..." }`
    Registry {
        /// Package name in the registry (may differ from the dep key).
        package: String,
        /// Semver version requirement, e.g. "^1.0.0", "~2.3", ">=0.5".
        version_req: String,
    },
}

impl fmt::Display for Dep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Dep::Path(p) => write!(f, "{{ path = \"{}\" }}", p),
            Dep::Git {
                url,
                branch,
                tag,
                rev,
            } => {
                write!(f, "{{ git = \"{}\"", url)?;
                if let Some(b) = branch {
                    write!(f, ", branch = \"{}\"", b)?;
                }
                if let Some(t) = tag {
                    write!(f, ", tag = \"{}\"", t)?;
                }
                if let Some(r) = rev {
                    write!(f, ", rev = \"{}\"", r)?;
                }
                write!(f, " }}")
            }
            Dep::Registry {
                package,
                version_req,
            } => {
                write!(
                    f,
                    "{{ registry = \"{}\", version = \"{}\" }}",
                    package, version_req
                )
            }
        }
    }
}

// ── Serde mapping structs for TOML ────────────────────────────────────────────

#[derive(Serialize, Deserialize, Debug, Clone)]
struct TomlManifest {
    package: TomlPackage,
    #[serde(default)]
    dependencies: BTreeMap<String, TomlDep>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct TomlPackage {
    name: String,
    version: String,
    entry: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    repository: Option<String>,
    #[serde(default)]
    registry: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
enum TomlDep {
    Simple(String),
    Detailed(DetailedDep),
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
struct DetailedDep {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    git: Option<String>,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    rev: Option<String>,
    /// Registry package name.
    #[serde(default)]
    registry: Option<String>,
    /// Semver requirement (used with `registry`).
    #[serde(default)]
    version: Option<String>,
}

impl Manifest {
    pub fn parse(src: &str) -> Result<Self, String> {
        let toml_manifest: TomlManifest =
            toml::from_str(src).map_err(|e| format!("failed to parse iris.toml: {}", e))?;

        let mut deps = BTreeMap::new();
        for (name, toml_dep) in toml_manifest.dependencies {
            let dep = match toml_dep {
                TomlDep::Simple(s) => {
                    if s.starts_with("http://")
                        || s.starts_with("https://")
                        || s.starts_with("git@")
                    {
                        Dep::Git {
                            url: s,
                            branch: None,
                            tag: None,
                            rev: None,
                        }
                    } else {
                        Dep::Path(s)
                    }
                }
                TomlDep::Detailed(d) => {
                    if let Some(p) = d.path {
                        Dep::Path(p)
                    } else if let Some(git) = d.git {
                        Dep::Git {
                            url: git,
                            branch: d.branch,
                            tag: d.tag,
                            rev: d.rev,
                        }
                    } else if let Some(pkg) = d.registry {
                        Dep::Registry {
                            package: pkg,
                            version_req: d.version.unwrap_or_else(|| "*".into()),
                        }
                    } else {
                        return Err(format!(
                            "dependency '{}' must specify 'path', 'git', or 'registry'",
                            name
                        ));
                    }
                }
            };
            deps.insert(name, dep);
        }

        let pkg = toml_manifest.package;
        Ok(Manifest {
            name: pkg.name,
            version: pkg.version,
            entry: pkg.entry,
            description: pkg.description.unwrap_or_default(),
            license: pkg.license.unwrap_or_default(),
            repository: pkg.repository.unwrap_or_default(),
            registry: pkg.registry,
            deps,
        })
    }

    pub fn to_toml(&self) -> String {
        let mut toml_deps = BTreeMap::new();
        for (name, dep) in &self.deps {
            let toml_dep = match dep {
                Dep::Path(p) => TomlDep::Detailed(DetailedDep {
                    path: Some(p.clone()),
                    ..Default::default()
                }),
                Dep::Git {
                    url,
                    branch,
                    tag,
                    rev,
                } => TomlDep::Detailed(DetailedDep {
                    path: None,
                    git: Some(url.clone()),
                    branch: branch.clone(),
                    tag: tag.clone(),
                    rev: rev.clone(),
                    ..Default::default()
                }),
                Dep::Registry {
                    package,
                    version_req,
                } => TomlDep::Detailed(DetailedDep {
                    registry: Some(package.clone()),
                    version: Some(version_req.clone()),
                    ..Default::default()
                }),
            };
            toml_deps.insert(name.clone(), toml_dep);
        }

        let toml_manifest = TomlManifest {
            package: TomlPackage {
                name: self.name.clone(),
                version: self.version.clone(),
                entry: self.entry.clone(),
                description: if self.description.is_empty() {
                    None
                } else {
                    Some(self.description.clone())
                },
                license: if self.license.is_empty() {
                    None
                } else {
                    Some(self.license.clone())
                },
                repository: if self.repository.is_empty() {
                    None
                } else {
                    Some(self.repository.clone())
                },
                registry: self.registry.clone(),
            },
            dependencies: toml_deps,
        };

        toml::to_string_pretty(&toml_manifest).unwrap_or_else(|_| {
            let mut out = String::new();
            out.push_str("[package]\n");
            out.push_str(&format!("name    = \"{}\"\n", self.name));
            out.push_str(&format!("version = \"{}\"\n", self.version));
            out.push_str(&format!("entry   = \"{}\"\n", self.entry));
            out
        })
    }
}

// ── Lock file ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct LockEntry {
    pub kind: String,
    pub source: String,
    pub commit: Option<String>,
    /// SHA-256 hex checksum of the installed package directory (computed as
    /// the hash of a canonical listing of all files).
    pub checksum: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct LockFile {
    pub entries: BTreeMap<String, LockEntry>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
struct TomlLockFile {
    #[serde(default)]
    dep: BTreeMap<String, TomlLockEntry>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
struct TomlLockEntry {
    kind: String,
    source: String,
    #[serde(default)]
    commit: Option<String>,
    #[serde(default)]
    checksum: Option<String>,
}

impl LockFile {
    pub fn parse(src: &str) -> Self {
        let lock: TomlLockFile = toml::from_str(src).unwrap_or_default();
        let mut entries = BTreeMap::new();
        for (name, entry) in lock.dep {
            entries.insert(
                name,
                LockEntry {
                    kind: entry.kind,
                    source: entry.source,
                    commit: entry.commit,
                    checksum: entry.checksum,
                },
            );
        }
        LockFile { entries }
    }

    pub fn to_text(&self) -> String {
        let mut toml_dep = BTreeMap::new();
        for (name, entry) in &self.entries {
            toml_dep.insert(
                name.clone(),
                TomlLockEntry {
                    kind: entry.kind.clone(),
                    source: entry.source.clone(),
                    commit: entry.commit.clone(),
                    checksum: entry.checksum.clone(),
                },
            );
        }
        let lock = TomlLockFile { dep: toml_dep };
        let mut out = String::from(
            "# iris.lock — generated by `iris pkg install`. Commit to version control.\n\
             # Do not edit manually.\n\n",
        );
        if let Ok(toml_str) = toml::to_string_pretty(&lock) {
            out.push_str(&toml_str);
        }
        out
    }
}

// ── Manifest / lock file I/O ──────────────────────────────────────────────────

fn find_manifest(start_dir: &Path) -> Option<PathBuf> {
    let mut dir = start_dir.to_path_buf();
    loop {
        let candidate = dir.join("iris.toml");
        if candidate.exists() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn load_manifest() -> Result<(PathBuf, Manifest), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cannot read cwd: {}", e))?;
    let path = find_manifest(&cwd)
        .ok_or_else(|| "no iris.toml found (run `iris pkg init` to create one)".to_string())?;
    let text =
        fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
    let manifest = Manifest::parse(&text)?;
    Ok((path, manifest))
}

fn save_manifest(path: &Path, manifest: &Manifest) -> Result<(), String> {
    fs::write(path, manifest.to_toml())
        .map_err(|e| format!("cannot write {}: {}", path.display(), e))
}

fn lock_path(manifest_path: &Path) -> PathBuf {
    manifest_path.with_file_name("iris.lock")
}

fn load_lock(manifest_path: &Path) -> LockFile {
    let lp = lock_path(manifest_path);
    fs::read_to_string(&lp)
        .map(|s| LockFile::parse(&s))
        .unwrap_or_default()
}

fn save_lock(manifest_path: &Path, lock: &LockFile) -> Result<(), String> {
    let lp = lock_path(manifest_path);
    fs::write(&lp, lock.to_text()).map_err(|e| format!("cannot write iris.lock: {}", e))
}

// ── Content hashing ───────────────────────────────────────────────────────────

/// Compute SHA-256 of all files under `dir`, sorted by relative path.
/// Returns a hex string, or `None` if the directory doesn't exist.
fn dir_checksum(dir: &Path) -> Option<String> {
    let mut entries: Vec<PathBuf> = Vec::new();
    collect_files(dir, dir, &mut entries).ok()?;
    if entries.is_empty() {
        return None;
    }
    entries.sort();
    let mut hasher = Sha256::new();
    for rel in &entries {
        let abs = dir.join(rel);
        if let Ok(data) = fs::read(&abs) {
            hasher.update(rel.to_string_lossy().as_bytes());
            hasher.update([0u8; 1]);
            hasher.update(&data);
            hasher.update([0u8; 1]);
        }
    }
    Some(hex_encode(&hasher.finalize()))
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

fn collect_files(base: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let rel = path.strip_prefix(base).unwrap_or(&path).to_path_buf();
        if entry.file_type()?.is_dir() {
            collect_files(base, &path, out)?;
        } else {
            out.push(rel);
        }
    }
    Ok(())
}

// ── Registry helpers ──────────────────────────────────────────────────────────

fn parse_version_req(version_req: &str) -> Result<VersionReq, String> {
    if version_req == "*" {
        Ok(VersionReq::STAR)
    } else {
        VersionReq::parse(version_req)
            .map_err(|e| format!("invalid version requirement '{}': {}", version_req, e))
    }
}

/// Extract pinned version from a registry lock entry source (`"<pkg>@<version>"`).
fn locked_registry_version(entry: &LockEntry, package: &str) -> Option<Version> {
    if entry.kind != "registry" {
        return None;
    }
    let ver_str = entry.source.strip_prefix(&format!("{package}@"))?;
    Version::parse(ver_str).ok()
}

/// Fetch the registry index and resolve a version constraint to a concrete
/// version + download URL + checksum.
fn resolve_registry_dep(
    registry_url: &str,
    package: &str,
    version_req: &str,
    locked_pin: Option<&Version>,
) -> Result<(Version, String, String), String> {
    let req = parse_version_req(version_req)?;

    // Fetch the registry index.
    let index_url = format!("{}/index.json", registry_url.trim_end_matches('/'));
    let resp =
        ureq_get(&index_url).map_err(|e| format!("failed to fetch registry index: {}", e))?;
    let text =
        String::from_utf8(resp).map_err(|e| format!("invalid UTF-8 in registry index: {}", e))?;
    let index: RegistryIndex =
        serde_json::from_str(&text).map_err(|e| format!("invalid registry index: {}", e))?;

    let versions = index
        .packages
        .get(package)
        .and_then(|p| p.versions.as_ref())
        .ok_or_else(|| format!("package '{}' not found in registry", package))?;

    if let Some(pin) = locked_pin {
        if !req.matches(pin) {
            return Err(format!(
                "--locked: locked version '{}@{}' no longer satisfies manifest requirement '{}'",
                package, pin, version_req
            ));
        }
        let pin_str = pin.to_string();
        let entry = versions.get(&pin_str).ok_or_else(|| {
            format!(
                "--locked: pinned version '{}@{}' not found in registry",
                package, pin
            )
        })?;
        return Ok((
            pin.clone(),
            registry_join(registry_url, &entry.url),
            entry.checksum.clone(),
        ));
    }

    // Find the latest matching version.
    let mut best: Option<(Version, &RegistryVersionEntry)> = None;
    for (ver_str, entry) in versions {
        if let Ok(ver) = Version::parse(ver_str) {
            if req.matches(&ver) {
                let is_better = match &best {
                    None => true,
                    Some((b, _)) => ver > *b,
                };
                if is_better {
                    best = Some((ver, entry));
                }
            }
        }
    }

    let (version, entry) = best.ok_or_else(|| {
        format!(
            "no version matching '{}' found for package '{}'",
            version_req, package
        )
    })?;

    Ok((
        version,
        registry_join(registry_url, &entry.url),
        entry.checksum.clone(),
    ))
}

#[derive(Deserialize, Debug)]
struct RegistryIndex {
    packages: BTreeMap<String, RegistryPackage>,
}

#[derive(Deserialize, Debug)]
struct RegistryPackage {
    #[serde(default)]
    versions: Option<BTreeMap<String, RegistryVersionEntry>>,
}

#[derive(Deserialize, Debug, Clone)]
struct RegistryVersionEntry {
    url: String,
    /// "sha256:<hex>" format.
    #[serde(default)]
    checksum: String,
}

/// Download a tarball from `url`, verify its checksum, and extract into `target`.
fn download_and_extract(
    url: &str,
    expected_checksum: &str,
    target: &Path,
    name: &str,
) -> Result<(), String> {
    eprintln!("  {} — downloading {} ...", name, url);
    let data = ureq_get(url).map_err(|e| format!("failed to download {}: {}", url, e))?;

    // Verify checksum if provided.
    if let Some(expected) = expected_checksum.strip_prefix("sha256:") {
        let actual = hex_encode(&Sha256::digest(&data));
        if actual != expected {
            return Err(format!(
                "checksum mismatch for {}: expected sha256:{} but got sha256:{}",
                name, expected, actual
            ));
        }
        eprintln!("  {} — checksum OK", name);
    }

    // Extract tarball (gzipped).
    let decoder = flate2::read::GzDecoder::new(&data[..]);
    let mut archive = tar::Archive::new(decoder);
    if target.exists() {
        fs::remove_dir_all(target)
            .map_err(|e| format!("cannot remove {}: {}", target.display(), e))?;
    }
    fs::create_dir_all(target).map_err(|e| format!("cannot create {}: {}", target.display(), e))?;
    archive
        .unpack(target)
        .map_err(|e| format!("failed to extract {}: {}", name, e))?;

    eprintln!("  {} — extracted to {}", name, target.display());
    Ok(())
}

/// Read `url`: `http(s)://` through curl (an HTTP error is an error, not a
/// page to parse), `file://` and plain paths from disk.
fn ureq_get(url: &str) -> Result<Vec<u8>, String> {
    if url.starts_with("http://") || url.starts_with("https://") {
        let output = Command::new(if cfg!(windows) { "curl.exe" } else { "curl" })
            .args(["-fsSL", url])
            .output()
            .map_err(|e| format!("curl could not start: {}", e))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("{}: {}", url, stderr.trim()));
        }
        return Ok(output.stdout);
    }
    let path = file_url_path(url);
    fs::read(&path).map_err(|e| format!("{}: {}", path.display(), e))
}

/// The path a `file://` URL, or a plain path, names.
fn file_url_path(url: &str) -> PathBuf {
    match url.strip_prefix("file://") {
        // file:///C:/x on Windows, file:///x elsewhere.
        Some(rest) if cfg!(windows) && rest.starts_with('/') && rest.get(2..3) == Some(":") => {
            PathBuf::from(&rest[1..])
        }
        Some(rest) => PathBuf::from(rest),
        None => PathBuf::from(url),
    }
}

/// `rel` against the registry `base`; absolute URLs and paths stay as they are.
fn registry_join(base: &str, rel: &str) -> String {
    let absolute = rel.contains("://") || Path::new(rel).is_absolute();
    if absolute {
        rel.to_owned()
    } else {
        format!(
            "{}/{}",
            base.trim_end_matches('/'),
            rel.trim_start_matches("./")
        )
    }
}

fn is_remote(registry: &str) -> bool {
    registry.starts_with("http://") || registry.starts_with("https://")
}

// ── Commands ──────────────────────────────────────────────────────────────────

pub fn cmd_init() -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cannot read cwd: {}", e))?;
    let manifest_path = cwd.join("iris.toml");

    if manifest_path.exists() {
        return Err("iris.toml already exists in this directory".into());
    }

    let dir_name = cwd
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("my-project")
        .to_string();

    let manifest = Manifest {
        name: dir_name.clone(),
        version: "0.1.0".into(),
        entry: "main.iris".into(),
        description: String::new(),
        license: String::new(),
        repository: String::new(),
        registry: None,
        deps: BTreeMap::new(),
    };

    save_manifest(&manifest_path, &manifest)?;

    let main_path = cwd.join("main.iris");
    if !main_path.exists() {
        fs::write(
            &main_path,
            format!(
                "// {} — entry point\n\ndef main() -> i64 {{\n    print(\"Hello from {}!\");\n    0\n}}\n",
                dir_name, dir_name
            ),
        )
        .map_err(|e| format!("cannot write main.iris: {}", e))?;
    }

    fs::create_dir_all(cwd.join(".iris")).map_err(|e| format!("cannot create .iris/: {}", e))?;

    eprintln!(
        "initialized IRIS project '{}' in {}",
        dir_name,
        cwd.display()
    );
    Ok(())
}

pub fn cmd_add(name: &str, dep: Dep) -> Result<(), String> {
    let (path, mut manifest) = load_manifest()?;
    manifest.deps.insert(name.to_string(), dep.clone());
    save_manifest(&path, &manifest)?;
    eprintln!("added dependency '{}' = {}", name, dep);
    Ok(())
}

pub fn cmd_remove(name: &str) -> Result<(), String> {
    let (path, mut manifest) = load_manifest()?;
    if manifest.deps.remove(name).is_none() {
        return Err(format!("dependency '{}' not found in iris.toml", name));
    }
    let mut lock = load_lock(&path);
    lock.entries.remove(name);
    save_manifest(&path, &manifest)?;
    save_lock(&path, &lock)?;
    eprintln!("removed dependency '{}'", name);
    Ok(())
}

pub fn cmd_list() -> Result<(), String> {
    let (manifest_path, manifest) = load_manifest()?;
    let lock = load_lock(&manifest_path);
    eprintln!("{} v{}", manifest.name, manifest.version);
    if manifest.deps.is_empty() {
        eprintln!("  (no dependencies)");
    } else {
        for (name, dep) in &manifest.deps {
            let locked = lock.entries.get(name);
            let pin = match locked {
                Some(e) if e.commit.is_some() => format!(
                    " [{}]",
                    &e.commit.as_deref().unwrap_or("")
                        [..8.min(e.commit.as_deref().unwrap_or("").len())]
                ),
                Some(e) if e.checksum.is_some() => format!(
                    " [{}]",
                    &e.checksum.as_deref().unwrap_or("")
                        [..12.min(e.checksum.as_deref().unwrap_or("").len())]
                ),
                _ => String::new(),
            };
            eprintln!("  {} = {}{}", name, dep, pin);
        }
    }
    Ok(())
}

fn collect_missing_deps(
    manifest: &Manifest,
    deps_dir: &Path,
) -> Result<(Vec<String>, usize), String> {
    let mut visited = std::collections::HashSet::new();
    let mut queue: std::collections::VecDeque<(String, Dep)> = manifest
        .deps
        .iter()
        .map(|(n, d)| (n.clone(), d.clone()))
        .collect();
    let mut missing = Vec::new();
    let mut checked = 0usize;
    while let Some((name, _dep)) = queue.pop_front() {
        if !visited.insert(name.clone()) {
            continue;
        }
        checked += 1;
        let target = deps_dir.join(&name);
        if !target.exists() {
            missing.push(name);
            continue;
        }
        let sub_path = target.join("iris.toml");
        if sub_path.exists() {
            if let Ok(text) = fs::read_to_string(&sub_path) {
                if let Ok(sub) = Manifest::parse(&text) {
                    for (sub_name, sub_dep) in sub.deps {
                        if !visited.contains(&sub_name) {
                            queue.push_back((sub_name, sub_dep));
                        }
                    }
                }
            }
        }
    }
    Ok((missing, checked))
}

/// `iris pkg check` — verify direct and transitive deps are installed in `.iris/deps/`.
pub fn cmd_check() -> Result<(), String> {
    let (manifest_path, manifest) = load_manifest()?;
    let project_dir = manifest_path
        .parent()
        .ok_or("cannot determine project directory")?;
    let deps_dir = project_dir.join(".iris").join("deps");
    let (missing, checked) = collect_missing_deps(&manifest, &deps_dir)?;
    if missing.is_empty() {
        eprintln!("all {} dependencies installed", checked);
        Ok(())
    } else {
        for m in &missing {
            eprintln!("  missing: {}", m);
        }
        Err(format!(
            "{} dependency/ies missing — run `iris pkg install`",
            missing.len()
        ))
    }
}

/// `iris pkg verify` — verify `.iris/deps/` against `iris.lock` SHA-256 checksums and semver constraints.
pub fn cmd_verify() -> Result<(), String> {
    let (manifest_path, manifest) = load_manifest()?;
    let project_dir = manifest_path
        .parent()
        .ok_or("cannot determine project directory")?;
    let deps_dir = project_dir.join(".iris").join("deps");
    if manifest.deps.is_empty() {
        eprintln!("verified 0 locked dependencies (no dependencies)");
        return Ok(());
    }
    if !lock_path(&manifest_path).exists() {
        return Err("iris.lock not found (run `iris pkg install` first)".into());
    }
    let lock = load_lock(&manifest_path);

    let (missing, _checked) = collect_missing_deps(&manifest, &deps_dir)?;
    if !missing.is_empty() {
        return Err(format!(
            "verification failed: missing installed dependencies: {}",
            missing.join(", ")
        ));
    }

    // Verify direct and transitive manifest edges exist in lockfile and match semver if registry.
    let mut visited = std::collections::HashSet::new();
    let mut queue: std::collections::VecDeque<(String, Dep)> = manifest
        .deps
        .iter()
        .map(|(n, d)| (n.clone(), d.clone()))
        .collect();
    while let Some((name, dep)) = queue.pop_front() {
        let entry = lock.entries.get(&name).ok_or_else(|| {
            format!(
                "verification failed: dependency '{}' is missing from iris.lock",
                name
            )
        })?;
        let target = deps_dir.join(&name);
        if let Dep::Registry {
            package,
            version_req,
        } = &dep
        {
            let req = parse_version_req(version_req)?;
            let locked_ver = locked_registry_version(entry, package).ok_or_else(|| {
                format!(
                    "verification failed: lock entry for '{}' has invalid source '{}'",
                    name, entry.source
                )
            })?;
            if !req.matches(&locked_ver) {
                return Err(format!(
                    "verification failed: '{}@{}' does not satisfy requirement '{}'",
                    package, locked_ver, version_req
                ));
            }
        }
        if !visited.insert(name.clone()) {
            continue;
        }
        let sub_path = target.join("iris.toml");
        if sub_path.exists() {
            if let Ok(text) = fs::read_to_string(&sub_path) {
                if let Ok(sub) = Manifest::parse(&text) {
                    for (sub_name, sub_dep) in sub.deps {
                        queue.push_back((sub_name, sub_dep));
                    }
                }
            }
        }
    }

    // Verify every lockfile entry matches on-disk directory SHA-256 checksum.
    for (name, entry) in &lock.entries {
        let target = deps_dir.join(name);
        if !target.exists() {
            return Err(format!(
                "verification failed: locked dependency '{}' is not installed in {}",
                name,
                target.display()
            ));
        }
        let expected = entry.checksum.as_deref().ok_or_else(|| {
            format!(
                "verification failed: lock entry for '{}' has no checksum",
                name
            )
        })?;
        let actual = dir_checksum(&target).ok_or_else(|| {
            format!(
                "verification failed: cannot hash installed directory for '{}'",
                name
            )
        })?;
        if actual != expected {
            return Err(format!(
                "verification failed for '{}': directory checksum mismatch (expected {}, got {})",
                name, expected, actual
            ));
        }
    }

    eprintln!(
        "verified {} locked dependencies (checksums OK)",
        lock.entries.len()
    );
    Ok(())
}

/// `iris pkg tree` — print direct and transitive dependency tree.
pub fn cmd_tree() -> Result<(), String> {
    let (manifest_path, manifest) = load_manifest()?;
    let project_dir = manifest_path
        .parent()
        .ok_or("cannot determine project directory")?;
    let deps_dir = project_dir.join(".iris").join("deps");
    let lock = load_lock(&manifest_path);

    println!("{} v{}", manifest.name, manifest.version);
    let mut visited = std::collections::HashSet::new();
    let total = manifest.deps.len();
    for (idx, (name, dep)) in manifest.deps.iter().enumerate() {
        let is_last = idx + 1 == total;
        print_tree_node(name, dep, "", is_last, &deps_dir, &lock, &mut visited);
    }
    Ok(())
}

fn format_dep_summary(name: &str, dep: &Dep, deps_dir: &Path, lock: &LockFile) -> String {
    let installed_ver = fs::read_to_string(deps_dir.join(name).join("iris.toml"))
        .ok()
        .and_then(|s| Manifest::parse(&s).ok())
        .map(|m| m.version);
    let spec = match dep {
        Dep::Path(p) => format!("path:{}", p),
        Dep::Git { url, .. } => format!("git:{}", url),
        Dep::Registry {
            package,
            version_req,
        } => format!("registry:{} ({})", package, version_req),
    };
    if let Some(ver) = installed_ver {
        format!("{} v{} [{}]", name, ver, spec)
    } else if let Some(entry) = lock.entries.get(name) {
        format!("{} ({}) [{}]", name, entry.source, spec)
    } else {
        format!("{} (not installed) [{}]", name, spec)
    }
}

fn print_tree_node(
    name: &str,
    dep: &Dep,
    prefix: &str,
    is_last: bool,
    deps_dir: &Path,
    lock: &LockFile,
    visited: &mut std::collections::HashSet<String>,
) {
    let branch = if is_last { "└── " } else { "├── " };
    let label = format_dep_summary(name, dep, deps_dir, lock);
    if !visited.insert(name.to_string()) {
        println!("{}{}{label} (*)", prefix, branch);
        return;
    }
    println!("{}{}{label}", prefix, branch);

    let sub_path = deps_dir.join(name).join("iris.toml");
    if sub_path.exists() {
        if let Ok(text) = fs::read_to_string(&sub_path) {
            if let Ok(sub) = Manifest::parse(&text) {
                let child_prefix = format!("{}{}", prefix, if is_last { "    " } else { "│   " });
                let count = sub.deps.len();
                for (i, (sub_name, sub_dep)) in sub.deps.iter().enumerate() {
                    print_tree_node(
                        sub_name,
                        sub_dep,
                        &child_prefix,
                        i + 1 == count,
                        deps_dir,
                        lock,
                        visited,
                    );
                }
            }
        }
    }
}

/// `iris pkg clean [--all]` — clean `.iris/deps/` (and `.iris/vendor/` + built binary with `--all`).
pub fn cmd_clean(all: bool) -> Result<(), String> {
    let (manifest_path, manifest) = load_manifest()?;
    let project_dir = manifest_path
        .parent()
        .ok_or("cannot determine project directory")?;
    let deps_dir = project_dir.join(".iris").join("deps");
    if deps_dir.exists() {
        remove_dir_all_safe(&deps_dir)?;
        eprintln!("removed {}", deps_dir.display());
    }
    if all {
        let vendor_dir = project_dir.join(VENDOR_DIR);
        if vendor_dir.exists() {
            remove_dir_all_safe(&vendor_dir)?;
            eprintln!("removed {}", vendor_dir.display());
        }
        let bin_path =
            project_dir.join(format!("{}{}", manifest.name, std::env::consts::EXE_SUFFIX));
        if bin_path.exists() {
            fs::remove_file(&bin_path)
                .map_err(|e| format!("cannot remove {}: {}", bin_path.display(), e))?;
            eprintln!("removed {}", bin_path.display());
        }
    }
    Ok(())
}

fn check_dep_conflict(
    name: &str,
    dep: &Dep,
    resolved_versions: &BTreeMap<String, (String, Version)>,
    resolved_git_urls: &BTreeMap<String, String>,
) -> Result<(), String> {
    match dep {
        Dep::Registry {
            package,
            version_req,
        } => {
            let req = parse_version_req(version_req)?;
            if let Some((prev_pkg, prev_ver)) = resolved_versions.get(name) {
                if prev_pkg != package {
                    return Err(format!(
                        "dependency conflict for '{}': requires both registry package '{}' and '{}'",
                        name, prev_pkg, package
                    ));
                }
                if !req.matches(prev_ver) {
                    return Err(format!(
                        "semver conflict for '{}' ({}): installed v{}, but dependency also requires '{}'",
                        name, package, prev_ver, version_req
                    ));
                }
            }
        }
        Dep::Git { url, .. } => {
            if let Some(prev_url) = resolved_git_urls.get(name) {
                if prev_url != url {
                    return Err(format!(
                        "dependency conflict for '{}': requires both git '{}' and '{}'",
                        name, prev_url, url
                    ));
                }
            }
        }
        Dep::Path(_) => {}
    }
    Ok(())
}

fn install_from_vendor(
    vendor_dir: &Path,
    target: &Path,
    name: &str,
    dep: &Dep,
    prev_lock: Option<&LockEntry>,
    locked: bool,
) -> Result<(LockEntry, Option<(String, Version)>), String> {
    let vendored = vendor_dir.join(name);
    if !vendored.exists() {
        return Err(format!(
            "offline mode: dependency '{}' not found in {} (run `iris pkg vendor` while online)",
            name,
            vendor_dir.display()
        ));
    }
    if target.exists() {
        remove_dir_all_safe(target)?;
    }
    copy_dir_recursive(&vendored, target)
        .map_err(|e| format!("cannot restore vendored '{}': {}", name, e))?;
    let cs = dir_checksum(target);

    let sub_manifest = fs::read_to_string(target.join("iris.toml"))
        .ok()
        .and_then(|t| Manifest::parse(&t).ok());
    let vendored_ver = sub_manifest
        .as_ref()
        .and_then(|m| Version::parse(&m.version).ok());

    let mut resolved_ver: Option<(String, Version)> = None;
    if let Dep::Registry {
        package,
        version_req,
    } = dep
    {
        let req = parse_version_req(version_req)?;
        if let Some(ref ver) = vendored_ver {
            if !req.matches(ver) {
                return Err(format!(
                    "vendored dependency '{}' v{} does not satisfy requirement '{}'",
                    name, ver, version_req
                ));
            }
            if locked {
                if let Some(lock_entry) = prev_lock {
                    if let Some(pin) = locked_registry_version(lock_entry, package) {
                        if *ver != pin {
                            return Err(format!(
                                "--locked: vendored '{}' is v{}, expected locked v{}",
                                name, ver, pin
                            ));
                        }
                    }
                }
            }
            resolved_ver = Some((package.clone(), ver.clone()));
        }
    }

    let entry = if let Some(prev) = prev_lock {
        LockEntry {
            kind: prev.kind.clone(),
            source: prev.source.clone(),
            commit: prev.commit.clone(),
            checksum: cs,
        }
    } else {
        match dep {
            Dep::Path(rel) => LockEntry {
                kind: "path".into(),
                source: rel.clone(),
                commit: None,
                checksum: cs,
            },
            Dep::Git { url, .. } => LockEntry {
                kind: "git".into(),
                source: url.clone(),
                commit: None,
                checksum: cs,
            },
            Dep::Registry { package, .. } => {
                let ver_suffix = vendored_ver
                    .as_ref()
                    .map(|v| format!("@{v}"))
                    .unwrap_or_default();
                LockEntry {
                    kind: "registry".into(),
                    source: format!("{package}{ver_suffix}"),
                    commit: None,
                    checksum: cs,
                }
            }
        }
    };
    eprintln!("  {} — restored from vendor ({})", name, vendored.display());
    Ok((entry, resolved_ver))
}

fn verify_locked_entry(
    name: &str,
    new_entry: &LockEntry,
    existing: &LockFile,
) -> Result<(), String> {
    let locked = existing.entries.get(name).ok_or_else(|| {
        format!(
            "--locked: dependency '{}' is not listed in iris.lock (run `iris pkg install`)",
            name
        )
    })?;
    if locked.kind != new_entry.kind || locked.source != new_entry.source {
        return Err(format!(
            "--locked: dependency '{}' changed (lock has {} '{}', resolved {} '{}')",
            name, locked.kind, locked.source, new_entry.kind, new_entry.source
        ));
    }
    if locked.commit != new_entry.commit {
        return Err(format!(
            "--locked: git commit for '{}' changed (lock {:?}, resolved {:?})",
            name, locked.commit, new_entry.commit
        ));
    }
    if let Some(expected_cs) = &locked.checksum {
        if new_entry.checksum.as_deref() != Some(expected_cs.as_str()) {
            return Err(format!(
                "--locked: checksum mismatch for '{}' (expected {}, got {:?})",
                name, expected_cs, new_entry.checksum
            ));
        }
    }
    Ok(())
}

/// `iris pkg install [--offline]`
pub fn cmd_install(offline: bool) -> Result<(), String> {
    cmd_install_with_opts(offline, false)
}

/// `iris pkg install [--offline] [--locked]`
pub fn cmd_install_with_opts(offline: bool, locked: bool) -> Result<(), String> {
    let (manifest_path, manifest) = load_manifest()?;
    let project_dir = manifest_path
        .parent()
        .ok_or("cannot determine project directory")?;
    let deps_dir = project_dir.join(".iris").join("deps");
    let vendor_dir = project_dir.join(VENDOR_DIR);

    fs::create_dir_all(&deps_dir).map_err(|e| format!("cannot create .iris/deps/: {}", e))?;

    if manifest.deps.is_empty() {
        eprintln!("no dependencies to install");
        return Ok(());
    }

    if locked && !lock_path(&manifest_path).exists() {
        return Err(
            "iris.lock not found (--locked requires an existing lockfile; run `iris pkg install` first)"
                .into(),
        );
    }

    let existing_lock = load_lock(&manifest_path);
    let mut lock = existing_lock.clone();

    let mut queue: std::collections::VecDeque<(String, Dep, PathBuf, Option<String>)> = manifest
        .deps
        .iter()
        .map(|(n, d)| {
            (
                n.clone(),
                d.clone(),
                project_dir.to_path_buf(),
                manifest.registry.clone(),
            )
        })
        .collect();

    let mut installed: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut resolved_versions: BTreeMap<String, (String, Version)> = BTreeMap::new();
    let mut resolved_git_urls: BTreeMap<String, String> = BTreeMap::new();

    while let Some((name, dep, from_dir, inherited_reg)) = queue.pop_front() {
        check_dep_conflict(&name, &dep, &resolved_versions, &resolved_git_urls)?;
        if installed.contains(&name) {
            continue;
        }
        installed.insert(name.clone());

        let target = deps_dir.join(&name);
        let prev_entry = existing_lock.entries.get(&name);
        let use_vendor =
            offline && (vendor_dir.join(&name).exists() || !matches!(dep, Dep::Path(_)));

        let (lock_entry, transitive_from_dir, resolved_ver) = if use_vendor {
            let (entry, ver) =
                install_from_vendor(&vendor_dir, &target, &name, &dep, prev_entry, locked)?;
            (entry, target.clone(), ver)
        } else {
            match &dep {
                Dep::Path(rel) => {
                    let source = from_dir.join(rel);
                    let source = source.canonicalize().unwrap_or_else(|_| source.clone());
                    if !source.exists() {
                        return Err(format!(
                            "dependency '{}': path '{}' does not exist",
                            name,
                            source.display()
                        ));
                    }
                    install_path_dep(&source, &target, &name)?;
                    let cs = dir_checksum(&target);
                    let entry = LockEntry {
                        kind: "path".into(),
                        source: source.to_string_lossy().into_owned(),
                        commit: None,
                        checksum: cs,
                    };
                    (entry, source.clone(), None)
                }
                Dep::Git {
                    url,
                    branch,
                    tag,
                    rev,
                } => {
                    let effective_rev = if locked {
                        prev_entry
                            .and_then(|e| e.commit.as_deref())
                            .or(rev.as_deref())
                    } else {
                        rev.as_deref()
                    };
                    let commit = install_git_dep(
                        url,
                        branch.as_deref(),
                        tag.as_deref(),
                        effective_rev,
                        &target,
                        &name,
                    )?;
                    resolved_git_urls.insert(name.clone(), url.clone());
                    let cs = dir_checksum(&target);
                    let entry = LockEntry {
                        kind: "git".into(),
                        source: url.clone(),
                        commit: Some(commit),
                        checksum: cs,
                    };
                    (entry, target.clone(), None)
                }
                Dep::Registry {
                    package,
                    version_req,
                } => {
                    let registry_url = effective_registry(inherited_reg.as_deref());
                    let locked_pin = if locked {
                        prev_entry.and_then(|e| locked_registry_version(e, package))
                    } else {
                        None
                    };
                    let (version, url, checksum) = resolve_registry_dep(
                        &registry_url,
                        package,
                        version_req,
                        locked_pin.as_ref(),
                    )?;
                    download_and_extract(&url, &checksum, &target, &name)?;
                    let cs = dir_checksum(&target);
                    let entry = LockEntry {
                        kind: "registry".into(),
                        source: format!("{}@{}", package, version),
                        commit: None,
                        checksum: cs,
                    };
                    (entry, target.clone(), Some((package.clone(), version)))
                }
            }
        };

        if let Some((pkg, ver)) = resolved_ver {
            resolved_versions.insert(name.clone(), (pkg, ver));
        }

        if locked {
            verify_locked_entry(&name, &lock_entry, &existing_lock)?;
        }
        lock.entries.insert(name.clone(), lock_entry);

        // Transitive dependencies.
        let sub_manifest_path = transitive_from_dir.join("iris.toml");
        if sub_manifest_path.exists() {
            let text = fs::read_to_string(&sub_manifest_path).map_err(|e| {
                format!(
                    "cannot read transitive manifest {}: {}",
                    sub_manifest_path.display(),
                    e
                )
            })?;
            let sub_manifest = Manifest::parse(&text)?;
            let next_reg = sub_manifest.registry.or(inherited_reg);
            for (sub_name, sub_dep) in sub_manifest.deps {
                queue.push_back((
                    sub_name,
                    sub_dep,
                    transitive_from_dir.clone(),
                    next_reg.clone(),
                ));
            }
        }
    }

    if locked {
        for name in existing_lock.entries.keys() {
            if !installed.contains(name) {
                return Err(format!(
                    "--locked: iris.lock contains stale dependency '{}' no longer in dependency graph",
                    name
                ));
            }
        }
    } else {
        save_lock(&manifest_path, &lock)?;
    }
    eprintln!("installed {} dependencies", installed.len());
    Ok(())
}

/// `iris pkg update [name]`.
pub fn cmd_update(only: Option<&str>) -> Result<(), String> {
    let (manifest_path, manifest) = load_manifest()?;
    let project_dir = manifest_path
        .parent()
        .ok_or("cannot determine project directory")?;
    let deps_dir = project_dir.join(".iris").join("deps");
    let mut lock = load_lock(&manifest_path);
    let mut updated = 0usize;

    for (name, dep) in &manifest.deps {
        if let Some(filter) = only {
            if name != filter {
                continue;
            }
        }
        match dep {
            Dep::Git {
                url,
                branch,
                tag,
                rev,
            } => {
                let target = deps_dir.join(name);
                if !target.exists() {
                    eprintln!(
                        "  {} — not installed, skipping (run `iris pkg install`)",
                        name
                    );
                    continue;
                }
                eprintln!("  {} — updating {}", name, url);
                let commit = git_pull_or_fetch(
                    &target,
                    url,
                    branch.as_deref(),
                    tag.as_deref(),
                    rev.as_deref(),
                    name,
                )?;
                let cs = dir_checksum(&target);
                lock.entries.insert(
                    name.clone(),
                    LockEntry {
                        kind: "git".into(),
                        source: url.clone(),
                        commit: Some(commit),
                        checksum: cs,
                    },
                );
                updated += 1;
            }
            Dep::Path(_) => {
                eprintln!("  {} — path dep, nothing to update", name);
            }
            Dep::Registry {
                package,
                version_req,
            } => {
                let registry_url = effective_registry(manifest.registry.as_deref());
                let target = deps_dir.join(name);
                if target.exists() {
                    fs::remove_dir_all(&target)
                        .map_err(|e| format!("cannot remove {}: {}", target.display(), e))?;
                }
                let (version, url, checksum) =
                    resolve_registry_dep(&registry_url, package, version_req, None)?;
                download_and_extract(&url, &checksum, &target, name)?;
                let cs = dir_checksum(&target);
                lock.entries.insert(
                    name.clone(),
                    LockEntry {
                        kind: "registry".into(),
                        source: format!("{}@{}", package, version),
                        commit: None,
                        checksum: cs,
                    },
                );
                updated += 1;
            }
        }
    }

    save_lock(&manifest_path, &lock)?;
    eprintln!("updated {} dependencies", updated);
    Ok(())
}

/// `iris pkg vendor` — download all deps into `.iris/vendor/` for offline builds.
pub fn cmd_vendor() -> Result<(), String> {
    let (manifest_path, manifest) = load_manifest()?;
    let project_dir = manifest_path
        .parent()
        .ok_or("cannot determine project directory")?;
    let vendor_dir = project_dir.join(VENDOR_DIR);
    let deps_dir = project_dir.join(".iris").join("deps");

    fs::create_dir_all(&vendor_dir)
        .map_err(|e| format!("cannot create {}: {}", vendor_dir.display(), e))?;

    if manifest.deps.is_empty() {
        eprintln!("no dependencies to vendor");
        return Ok(());
    }

    // Ensure direct and transitive deps are installed first.
    if !deps_dir.exists()
        || deps_dir
            .read_dir()
            .map(|mut i| i.next().is_none())
            .unwrap_or(true)
    {
        eprintln!("no installed deps found — running install first ...");
        cmd_install(false)?;
    } else {
        let (missing, _) = collect_missing_deps(&manifest, &deps_dir)?;
        if !missing.is_empty() {
            eprintln!(
                "missing dependencies ({}) — running install first ...",
                missing.join(", ")
            );
            cmd_install(false)?;
        }
    }

    // Remove stale vendored dirs not present in `.iris/deps/` so offline restores stay exact.
    if let Ok(entries) = fs::read_dir(&vendor_dir) {
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                let name = entry.file_name();
                if !deps_dir.join(&name).exists() {
                    remove_dir_all_safe(&entry.path())?;
                }
            }
        }
    }

    let mut dep_names: Vec<String> = fs::read_dir(&deps_dir)
        .map_err(|e| format!("cannot read {}: {}", deps_dir.display(), e))?
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    dep_names.sort();

    let mut vendored = 0usize;
    for name in dep_names {
        let src = deps_dir.join(&name);
        let dst = vendor_dir.join(&name);
        if dst.exists() {
            remove_dir_all_safe(&dst)?;
        }
        copy_dir_recursive(&src, &dst).map_err(|e| format!("cannot copy {}: {}", name, e))?;
        eprintln!("  {} → {}", name, dst.display());
        vendored += 1;
    }

    eprintln!(
        "vendored {} dependencies into {}",
        vendored,
        vendor_dir.display()
    );
    Ok(())
}

// ── Install helpers ───────────────────────────────────────────────────────────

fn install_path_dep(source: &Path, target: &Path, name: &str) -> Result<(), String> {
    if target.exists() {
        remove_dir_all_safe(target)?;
    }

    #[cfg(unix)]
    {
        if std::os::unix::fs::symlink(source, target).is_ok() {
            eprintln!("  {} → {} (symlink)", name, source.display());
            return Ok(());
        }
    }

    #[cfg(windows)]
    {
        if std::os::windows::fs::symlink_dir(source, target).is_ok() {
            eprintln!("  {} → {} (junction)", name, source.display());
            return Ok(());
        }
    }

    copy_dir_recursive(source, target)
        .map_err(|e| format!("dependency '{}': copy failed: {}", name, e))?;
    eprintln!("  {} → {} (copied)", name, source.display());
    Ok(())
}

fn install_git_dep(
    url: &str,
    branch: Option<&str>,
    tag: Option<&str>,
    rev: Option<&str>,
    target: &Path,
    name: &str,
) -> Result<String, String> {
    if target.join(".git").exists() {
        git_pull_or_fetch(target, url, branch, tag, rev, name)
    } else {
        git_clone(url, branch, tag, rev, target, name)
    }
}

fn git_clone(
    url: &str,
    branch: Option<&str>,
    tag: Option<&str>,
    rev: Option<&str>,
    target: &Path,
    name: &str,
) -> Result<String, String> {
    eprintln!("  {} — cloning {} ...", name, url);
    if target.exists() {
        remove_dir_all_safe(target)?;
    }

    let ref_arg: Option<&str> = tag.or(branch);
    let mut cmd = Command::new("git");
    cmd.arg("clone").arg("--depth").arg("1");
    if let Some(r) = ref_arg {
        cmd.arg("--branch").arg(r);
    }
    cmd.arg(url).arg(target.as_os_str());

    let status = cmd
        .status()
        .map_err(|e| format!("dependency '{}': git clone failed: {}", name, e))?;
    if !status.success() {
        return Err(format!("dependency '{}': git clone failed", name));
    }

    if let Some(r) = rev {
        git_checkout(target, r, name)?;
    }

    git_head_commit(target, name)
}

fn git_pull_or_fetch(
    target: &Path,
    url: &str,
    branch: Option<&str>,
    tag: Option<&str>,
    rev: Option<&str>,
    name: &str,
) -> Result<String, String> {
    let _ = Command::new("git")
        .args(["remote", "set-url", "origin", url])
        .current_dir(target)
        .status();

    if let Some(r) = rev {
        let _ = Command::new("git")
            .args(["fetch", "--depth", "1", "origin", r])
            .current_dir(target)
            .status();
        git_checkout(target, r, name)?;
    } else if let Some(t) = tag {
        let _ = Command::new("git")
            .args([
                "fetch",
                "--depth",
                "1",
                "origin",
                &format!("refs/tags/{}", t),
            ])
            .current_dir(target)
            .status();
        git_checkout(target, t, name)?;
    } else {
        let mut pull = Command::new("git");
        pull.arg("pull").arg("--ff-only");
        if let Some(b) = branch {
            pull.arg("origin").arg(b);
        }
        let status = pull
            .current_dir(target)
            .status()
            .map_err(|e| format!("dependency '{}': git pull failed: {}", name, e))?;
        if !status.success() {
            return Err(format!("dependency '{}': git pull failed", name));
        }
    }

    git_head_commit(target, name)
}

fn git_checkout(target: &Path, git_ref: &str, name: &str) -> Result<(), String> {
    let status = Command::new("git")
        .args(["checkout", git_ref])
        .current_dir(target)
        .status()
        .map_err(|e| format!("dependency '{}': git checkout failed: {}", name, e))?;
    if !status.success() {
        return Err(format!(
            "dependency '{}': git checkout '{}' failed",
            name, git_ref
        ));
    }
    Ok(())
}

fn git_head_commit(target: &Path, name: &str) -> Result<String, String> {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(target)
        .output()
        .map_err(|e| format!("dependency '{}': git rev-parse failed: {}", name, e))?;
    if !out.status.success() {
        return Err(format!("dependency '{}': git rev-parse HEAD failed", name));
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    eprintln!("  {} → {}", name, &sha[..sha.len().min(12)]);
    Ok(sha)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dest = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &dest)?;
        } else {
            fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

fn remove_dir_all_safe(path: &Path) -> Result<(), String> {
    fs::remove_dir_all(path).map_err(|e| format!("cannot remove {}: {}", path.display(), e))
}

// ── Build / run ───────────────────────────────────────────────────────────────

pub fn cmd_build(run_after: bool, offline: bool) -> Result<(), String> {
    cmd_build_with_opts(run_after, offline, false)
}

pub fn cmd_build_with_opts(run_after: bool, offline: bool, locked: bool) -> Result<(), String> {
    let (manifest_path, manifest) = load_manifest()?;
    let project_dir = manifest_path
        .parent()
        .ok_or("cannot determine project directory")?;

    cmd_install_with_opts(offline, locked)?;

    let entry_path = project_dir.join(&manifest.entry);
    if !entry_path.exists() {
        return Err(format!(
            "entry file '{}' not found (set `entry` in [package])",
            entry_path.display()
        ));
    }

    let deps_dir = project_dir.join(".iris").join("deps");
    let mut extra_paths: Vec<PathBuf> = Vec::new();
    if deps_dir.exists() {
        for entry in fs::read_dir(&deps_dir)
            .map_err(|e| e.to_string())?
            .flatten()
        {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                extra_paths.push(entry.path());
            }
        }
    }

    let extra_refs: Vec<&Path> = extra_paths.iter().map(|p| p.as_path()).collect();
    let compiler = crate::FileCompiler::new();
    let mut main_ast = compiler
        .compile_file_to_ast(&entry_path, &extra_refs)
        .map_err(|e| format!("{}", e))?;

    let module_name = entry_path
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or("main");
    let ir = crate::compile_ast_to_module(&mut main_ast, module_name, None)
        .map_err(|e| format!("{}", e))?;

    let output_name = format!("{}{}", manifest.name, std::env::consts::EXE_SUFFIX);
    let output_path = project_dir.join(&output_name);
    crate::codegen::build_binary(&ir, &output_path).map_err(|e| format!("{}", e))?;
    eprintln!("wrote binary: {}", output_path.display());

    if run_after {
        let run_path = fs::canonicalize(&output_path).unwrap_or_else(|_| output_path.clone());
        let status = Command::new(&run_path)
            .current_dir(project_dir)
            .status()
            .map_err(|e| format!("cannot run binary: {}", e))?;
        std::process::exit(status.code().unwrap_or(1));
    }

    Ok(())
}

// ── Default registry ──────────────────────────────────────────────────────────

/// The IRIS registry: `registry/` of the IRIS repository, served by GitHub.
/// `[package].registry` or `IRIS_REGISTRY` chooses another.
const DEFAULT_REGISTRY: &str =
    "https://raw.githubusercontent.com/Eclipse-Softworks/IRIS/master/registry";

fn effective_registry(manifest_registry: Option<&str>) -> String {
    if let Ok(env_reg) = std::env::var("IRIS_REGISTRY") {
        return env_reg;
    }
    manifest_registry.unwrap_or(DEFAULT_REGISTRY).to_string()
}

// ── Publishing, searching, serving ────────────────────────────────────────────

/// Directories a package never carries.
const UNPACKED_DIRS: &[&str] = &[".git", ".iris", "target", "node_modules", "__pycache__"];
/// Build products a package never carries.
const UNPACKED_EXTS: &[&str] = &[
    "exe", "o", "obj", "dll", "so", "dylib", "pdb", "a", "lib", "pyd", "engine", "onnx",
];

/// The package's files, relative to `dir`, in a stable order.
fn package_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type()?.is_dir() {
                if !name.starts_with('.') && !UNPACKED_DIRS.contains(&name.as_str()) {
                    walk(base, &path, out)?;
                }
            } else {
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if !UNPACKED_EXTS.contains(&ext.as_str()) && !name.starts_with('.') {
                    out.push(path.strip_prefix(base).unwrap_or(&path).to_path_buf());
                }
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(dir, dir, &mut files).map_err(|e| format!("cannot read {}: {}", dir.display(), e))?;
    files.sort();
    Ok(files)
}

/// The package as a `.tar.gz`: the same files give the same bytes.
fn package_archive(dir: &Path, files: &[PathBuf]) -> Result<Vec<u8>, String> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    for rel in files {
        let data =
            fs::read(dir.join(rel)).map_err(|e| format!("cannot read {}: {}", rel.display(), e))?;
        if data.len() > 10 << 20 {
            return Err(format!(
                "{} is over 10 MB; a package carries source",
                rel.display()
            ));
        }
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        let name = rel.to_string_lossy().replace('\\', "/");
        tar.append_data(&mut header, &name, &data[..])
            .map_err(|e| format!("cannot pack {}: {}", name, e))?;
    }
    let gz = tar.into_inner().map_err(|e| e.to_string())?;
    gz.finish().map_err(|e| e.to_string())
}

fn read_index(registry: &str) -> Result<serde_json::Value, String> {
    let index_url = registry_join(registry, "index.json");
    let text = String::from_utf8(ureq_get(&index_url)?)
        .map_err(|e| format!("the registry index is not UTF-8: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("the registry index is not valid JSON: {e}"))
}

/// `iris pkg publish`: add this package's version to a registry directory.
pub fn cmd_publish(registry: Option<&str>, dry_run: bool) -> Result<(), String> {
    let (manifest_path, manifest) = load_manifest()?;
    let project_dir = manifest_path
        .parent()
        .ok_or("cannot determine the project directory")?;
    let version = Version::parse(&manifest.version).map_err(|e| {
        format!(
            "version `{}` is not semver ({e}); a published version must be",
            manifest.version
        )
    })?;
    let name = &manifest.name;
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "package name `{name}`: letters, digits, `-` and `_` only"
        ));
    }
    if !project_dir.join(&manifest.entry).exists() {
        return Err(format!(
            "the entry file `{}` does not exist",
            manifest.entry
        ));
    }
    let registry = registry
        .map(str::to_owned)
        .unwrap_or_else(|| effective_registry(manifest.registry.as_deref()));
    if is_remote(&registry) {
        return Err(format!(
            "{registry} is served over HTTP, which is read-only: publish into the directory it is served from \
             (for the IRIS registry, a checkout of the IRIS repository's `registry/`), then push it"
        ));
    }
    let dir = file_url_path(&registry);

    let files = package_files(project_dir)?;
    let archive = package_archive(project_dir, &files)?;
    let checksum = format!("sha256:{}", hex_encode(&Sha256::digest(&archive)));
    let rel_url = format!("pkgs/{name}-{version}.tar.gz");

    let mut index = if dir.join("index.json").exists() {
        read_index(&dir.to_string_lossy())?
    } else {
        serde_json::json!({ "packages": {} })
    };
    let packages = index
        .get_mut("packages")
        .and_then(|p| p.as_object_mut())
        .ok_or("the registry index has no `packages` object")?;
    let entry = packages
        .entry(name.clone())
        .or_insert_with(|| serde_json::json!({ "versions": {} }));
    if !manifest.description.is_empty() {
        entry["description"] = serde_json::Value::String(manifest.description.clone());
    }
    let versions = entry
        .as_object_mut()
        .and_then(|e| {
            e.entry("versions").or_insert_with(|| serde_json::json!({}));
            e.get_mut("versions")
        })
        .and_then(|v| v.as_object_mut())
        .ok_or("the registry index entry has no `versions` object")?;
    if versions.contains_key(&version.to_string()) {
        return Err(format!(
            "{name} {version} is already in the registry; a published version is never replaced"
        ));
    }
    versions.insert(
        version.to_string(),
        serde_json::json!({ "url": rel_url, "checksum": checksum }),
    );

    eprintln!(
        "  {name} {version}: {} files, {} bytes, {checksum}",
        files.len(),
        archive.len()
    );
    if dry_run {
        for f in &files {
            eprintln!("    {}", f.display());
        }
        eprintln!("  (dry run: nothing written)");
        return Ok(());
    }
    fs::create_dir_all(dir.join("pkgs"))
        .map_err(|e| format!("cannot create {}: {e}", dir.join("pkgs").display()))?;
    let archive_path = dir.join(&rel_url);
    fs::write(&archive_path, &archive)
        .map_err(|e| format!("cannot write {}: {e}", archive_path.display()))?;
    let text = serde_json::to_string_pretty(&index).map_err(|e| e.to_string())? + "\n";
    let tmp = dir.join("index.json.tmp");
    fs::write(&tmp, text).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, dir.join("index.json"))
        .map_err(|e| format!("cannot update the index: {e}"))?;
    eprintln!("  published {name} {version} to {}", dir.display());
    Ok(())
}

/// `iris pkg search`: the registry's packages whose name or description
/// contains `query`, with their versions.
pub fn cmd_search(query: &str, registry: Option<&str>) -> Result<(), String> {
    let registry = match registry {
        Some(r) => r.to_owned(),
        None => {
            let manifest_registry = load_manifest().ok().and_then(|(_, m)| m.registry);
            effective_registry(manifest_registry.as_deref())
        }
    };
    let index = read_index(&registry)?;
    let q = query.to_ascii_lowercase();
    let mut found = 0;
    if let Some(packages) = index.get("packages").and_then(|p| p.as_object()) {
        for (name, entry) in packages {
            let description = entry
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("");
            if !name.to_ascii_lowercase().contains(&q)
                && !description.to_ascii_lowercase().contains(&q)
            {
                continue;
            }
            let mut versions: Vec<Version> = entry
                .get("versions")
                .and_then(|v| v.as_object())
                .map(|v| v.keys().filter_map(|k| Version::parse(k).ok()).collect())
                .unwrap_or_default();
            versions.sort();
            let latest = versions
                .last()
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".into());
            println!("{name} {latest}  {description}");
            found += 1;
        }
    }
    if found == 0 {
        eprintln!("no package in {registry} matches `{query}`");
    }
    Ok(())
}

/// `iris pkg serve`: serve a registry directory over HTTP, read-only.
pub fn cmd_serve(dir: &str, port: u16) -> Result<(), String> {
    use std::io::{Read, Write};
    let root = fs::canonicalize(dir).map_err(|e| format!("{dir}: {e}"))?;
    if !root.join("index.json").exists() {
        return Err(format!(
            "{} has no index.json (publish into it first)",
            root.display()
        ));
    }
    let listener = std::net::TcpListener::bind(("0.0.0.0", port))
        .map_err(|e| format!("cannot listen on port {port}: {e}"))?;
    let bound = listener.local_addr().map_err(|e| e.to_string())?.port();
    eprintln!(
        "serving the registry {} at http://127.0.0.1:{bound}/",
        root.display()
    );
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let root = root.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let mut parts = req.split_whitespace();
            let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"));
            let rel = target
                .split('?')
                .next()
                .unwrap_or("")
                .trim_start_matches('/');
            let safe = !rel.is_empty()
                && rel
                    .split('/')
                    .all(|c| !c.is_empty() && c != ".." && c != ".");
            let path = root.join(rel);
            let (status, ctype, body) = match (method, safe, fs::read(&path)) {
                ("GET", true, Ok(body)) if path.starts_with(&root) => {
                    let ctype = if rel.ends_with(".json") {
                        "application/json"
                    } else {
                        "application/gzip"
                    };
                    ("200 OK", ctype, body)
                }
                ("GET", _, _) => ("404 Not Found", "text/plain", b"not found\n".to_vec()),
                _ => (
                    "405 Method Not Allowed",
                    "text/plain",
                    b"read-only\n".to_vec(),
                ),
            };
            let head = format!("HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        });
    }
    Ok(())
}

fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str())
}

// ── CLI dispatcher ────────────────────────────────────────────────────────────

pub fn run_pkg_command(args: &[String]) -> Result<(), String> {
    // `args` are the trailing arguments after `iris pkg` (e.g. ["init"] or ["add", "foo", ...]).
    let sub = args.first().map(|s| s.as_str()).unwrap_or("help");

    match sub {
        "init" => cmd_init(),

        "add" => {
            let name = args.get(1)
                .ok_or("usage: iris pkg add <name> --path <p> | --git <url> [--tag t | --rev r | --branch b] | --registry <pkg> --version <req>")?;

            let mut path_val: Option<String> = None;
            let mut git_val: Option<String> = None;
            let mut tag_val: Option<String> = None;
            let mut rev_val: Option<String> = None;
            let mut branch_val: Option<String> = None;
            let mut registry_val: Option<String> = None;
            let mut version_val: Option<String> = None;

            let mut i = 2usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--path" => {
                        i += 1;
                        path_val = args.get(i).cloned();
                    }
                    "--git" => {
                        i += 1;
                        git_val = args.get(i).cloned();
                    }
                    "--tag" => {
                        i += 1;
                        tag_val = args.get(i).cloned();
                    }
                    "--rev" => {
                        i += 1;
                        rev_val = args.get(i).cloned();
                    }
                    "--branch" => {
                        i += 1;
                        branch_val = args.get(i).cloned();
                    }
                    "--registry" => {
                        i += 1;
                        registry_val = args.get(i).cloned();
                    }
                    "--version" => {
                        i += 1;
                        version_val = args.get(i).cloned();
                    }
                    other => return Err(format!("unknown flag: {}", other)),
                }
                i += 1;
            }

            let dep = if let Some(p) = path_val {
                Dep::Path(p)
            } else if let Some(url) = git_val {
                Dep::Git {
                    url,
                    branch: branch_val,
                    tag: tag_val,
                    rev: rev_val,
                }
            } else if let Some(pkg) = registry_val {
                Dep::Registry {
                    package: pkg,
                    version_req: version_val.unwrap_or_else(|| "*".into()),
                }
            } else {
                return Err(
                    "usage: iris pkg add <name> --path <p> | --git <url> [--tag t | --rev r | --branch b] | --registry <pkg> --version <req>"
                        .into(),
                );
            };

            cmd_add(name, dep)
        }

        "remove" | "rm" => {
            let name = args.get(1).ok_or("usage: iris pkg remove <name>")?;
            cmd_remove(name)
        }

        "install" | "i" => {
            let offline = args.iter().any(|a| a == "--offline");
            let locked = args.iter().any(|a| a == "--locked");
            cmd_install_with_opts(offline, locked)
        }

        "update" | "u" => {
            let only = args.get(1).map(|s| s.as_str());
            cmd_update(only)
        }

        "list" | "ls" => cmd_list(),

        "check" => cmd_check(),

        "verify" => cmd_verify(),

        "tree" => cmd_tree(),

        "clean" => {
            let all = args.iter().any(|a| a == "--all");
            cmd_clean(all)
        }

        "vendor" => cmd_vendor(),

        "build" | "b" => {
            let offline = args.iter().any(|a| a == "--offline");
            let locked = args.iter().any(|a| a == "--locked");
            cmd_build_with_opts(false, offline, locked)
        }

        "run" | "r" => {
            let offline = args.iter().any(|a| a == "--offline");
            let locked = args.iter().any(|a| a == "--locked");
            cmd_build_with_opts(true, offline, locked)
        }

        "publish" => cmd_publish(
            flag_value(args, "--registry"),
            args.iter().any(|a| a == "--dry-run"),
        ),

        "search" => {
            let query = args
                .get(1)
                .filter(|a| !a.starts_with("--"))
                .map(|s| s.as_str())
                .unwrap_or("");
            cmd_search(query, flag_value(args, "--registry"))
        }

        "serve" => {
            let dir = args
                .get(1)
                .ok_or("usage: iris pkg serve <registry-dir> [--port N]")?;
            let port = match flag_value(args, "--port") {
                Some(p) => p.parse().map_err(|_| format!("bad port `{p}`"))?,
                None => 8765,
            };
            cmd_serve(dir, port)
        }

        "help" | "--help" | "-h" => {
            eprintln!("{}", pkg_help_text());
            Ok(())
        }

        other => Err(format!(
            "unknown pkg subcommand: '{}'\n\n{}",
            other,
            pkg_help_text()
        )),
    }
}

fn pkg_help_text() -> &'static str {
    "IRIS Package Manager\n\
     \n\
     Usage: iris pkg <command> [args...]\n\
     \n\
     Commands:\n\
       init                                        Create a new iris.toml\n\
       add <name> --path <path>                    Add a local path dependency\n\
       add <name> --git <url> [--branch|--tag|--rev] Add a git dependency\n\
       add <name> --registry <pkg> --version <req> Add a registry dependency\n\
       remove <name>                               Remove a dependency\n\
       install [--offline] [--locked]              Fetch/sync all deps into .iris/deps/\n\
       update [name]                               Update deps to latest matching ref\n\
       list                                        List dependencies (with lock info)\n\
       check                                       Verify all deps are installed\n\
       verify                                      Verify .iris/deps/ against iris.lock checksums\n\
       tree                                        Print direct and transitive dependency tree\n\
       clean [--all]                               Remove .iris/deps/ (and vendor/binary with --all)\n\
       vendor                                      Copy all direct & transitive deps for offline use\n\
       build [--offline] [--locked]                Install deps and build the project binary\n\
       run [--offline] [--locked]                  Install deps, build, and run\n\
       publish [--registry R] [--dry-run]          Publish this package to a registry directory\n\
       search <text> [--registry R]                Find packages in the registry\n\
       serve <dir> [--port N]                      Serve a registry directory over HTTP\n\
       help                                        Show this help message\n\
     \n\
     Aliases: rm=remove, i=install, u=update, ls=list, b=build, r=run\n\
     \n\
     The --offline flag uses vendored copies from .iris/vendor/ when set.\n\
     \n\
     Lock file:\n\
       iris.lock is auto-generated next to iris.toml.\n\
       Commit it to source control for reproducible builds.\n\
     \n\
     Registry:\n\
       The IRIS registry by default; [package].registry in iris.toml or the\n\
       IRIS_REGISTRY env var chooses another: an http(s):// URL, a file:// URL\n\
       or a directory.\n\
     \n\
     Vendor:\n\
       Run `iris pkg vendor` to copy all deps into .iris/vendor/.\n\
       Then use `iris pkg install/build/run --offline` to use them\n\
       without network access.\n\
     \n\
     Transitive dependencies:\n\
       If an installed dep has its own iris.toml, its dependencies\n\
       are automatically installed into the same .iris/deps/ directory.\n"
}
