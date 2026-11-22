//! Standalone application compilation, native bundling, icon/archive encoders, and OS installation.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use flate2::write::{DeflateEncoder, ZlibEncoder};
use flate2::Compression;

use crate::codegen::build::{build_binary_with_security_and_options, NativeBuildOptions};
use crate::pkg::{AppManifest, Manifest};

/// Target distribution format produced by `iris bundle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleFormat {
    Auto,
    Dir,
    Zip,
    Inno,
    App,
    AppImage,
}

impl BundleFormat {
    /// Parse a user-supplied `--format` CLI string.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "dir" | "directory" | "portable" => Some(Self::Dir),
            "zip" | "archive" => Some(Self::Zip),
            "inno" | "iss" | "installer" | "exe-installer" => Some(Self::Inno),
            "app" | "macos" | "bundle" => Some(Self::App),
            "appimage" | "appdir" | "linux" => Some(Self::AppImage),
            _ => None,
        }
    }

    /// Resolve `Auto` into the host OS's primary standalone format.
    pub fn resolve_auto(self) -> Self {
        match self {
            Self::Auto => {
                if cfg!(target_os = "macos") {
                    Self::App
                } else if cfg!(target_os = "linux") {
                    Self::AppImage
                } else {
                    Self::Dir
                }
            }
            other => other,
        }
    }
}

/// Configuration for `run_bundle`.
#[derive(Debug, Clone)]
pub struct BundleConfig {
    pub input: PathBuf,
    pub output: Option<PathBuf>,
    pub format: BundleFormat,
    pub icon: Option<PathBuf>,
    pub title: Option<String>,
    pub identifier: Option<String>,
    pub gui: Option<bool>,
    pub install: bool,
    pub uninstall: bool,
    pub install_dir: Option<PathBuf>,
    pub no_register: bool,
    pub release: bool,
}

/// Result paths and metadata returned by `run_bundle`.
#[derive(Debug, Clone)]
pub struct BundleOutput {
    pub bundle_dir: PathBuf,
    pub artifact_path: PathBuf,
    pub executable_path: PathBuf,
    pub ui_host_bundled: Option<PathBuf>,
    pub installed_dir: Option<PathBuf>,
    pub format_used: BundleFormat,
    pub note: Option<String>,
}

struct ResolvedMeta {
    root_dir: PathBuf,
    entry_file: PathBuf,
    title: String,
    slug: String,
    version: String,
    identifier: String,
    publisher: String,
    categories: String,
    use_gui: bool,
    bundle_ui_host: bool,
    icon_path: Option<PathBuf>,
    assets: Vec<String>,
    window_width: u32,
    window_height: u32,
    min_width: u32,
    min_height: u32,
    resizable: bool,
}

/// Compute IEEE 802.3 CRC-32 checksum over `data`.
fn crc32_ieee(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn push_png_chunk(out: &mut Vec<u8>, tag: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(tag);
    out.extend_from_slice(payload);
    let mut crc_buf = Vec::with_capacity(4 + payload.len());
    crc_buf.extend_from_slice(tag);
    crc_buf.extend_from_slice(payload);
    out.extend_from_slice(&crc32_ieee(&crc_buf).to_be_bytes());
}

/// Encode straight RGBA8 pixel bytes (`width * height * 4`) into a valid PNG image.
pub fn encode_rgba_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    if width == 0 || height == 0 {
        return Err("PNG icon width and height must be positive".to_owned());
    }
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|px| px.checked_mul(4))
        .ok_or_else(|| "PNG dimensions overflow".to_owned())?;
    if rgba.len() < expected {
        return Err(format!(
            "RGBA pixel buffer too small: need {expected}, got {}",
            rgba.len()
        ));
    }

    let stride = (width as usize) * 4;
    let mut raw_scanlines = Vec::with_capacity((stride + 1) * height as usize);
    for y in 0..height as usize {
        raw_scanlines.push(0u8); // Filter type 0 (None)
        let row_start = y * stride;
        raw_scanlines.extend_from_slice(&rgba[row_start..row_start + stride]);
    }

    let mut zlib = ZlibEncoder::new(Vec::new(), Compression::default());
    zlib.write_all(&raw_scanlines)
        .map_err(|e| format!("zlib compression failed: {e}"))?;
    let idat = zlib
        .finish()
        .map_err(|e| format!("zlib finish failed: {e}"))?;

    let mut out = Vec::with_capacity(64 + idat.len());
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(6); // RGBA color type
    ihdr.push(0); // compression
    ihdr.push(0); // filter
    ihdr.push(0); // interlace
    push_png_chunk(&mut out, b"IHDR", &ihdr);
    push_png_chunk(&mut out, b"IDAT", &idat);
    push_png_chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

/// Generate a crisp procedural `size x size` IRIS obsidian-and-cyan application emblem (RGBA8).
pub fn generate_default_iris_icon_rgba(size: u32) -> Vec<u8> {
    let s = size.max(16);
    let mut rgba = vec![0u8; (s as usize) * (s as usize) * 4];
    let sf = s as f32;
    let half = sf * 0.5;
    let corner = sf * 0.22;
    let margin = sf * 0.04;
    let inner_half = half - margin - corner;

    for py in 0..s {
        for px in 0..s {
            let fx = (px as f32 + 0.5) - half;
            let fy = (py as f32 + 0.5) - half;
            // Rounded squircle SDF for app tile silhouette
            let qx = fx.abs() - inner_half;
            let qy = fy.abs() - inner_half;
            let outside = (qx.max(0.0).hypot(qy.max(0.0))) + qx.max(qy).min(0.0) - corner;
            let tile_alpha = (0.5 - outside).clamp(0.0, 1.0);
            if tile_alpha <= 0.0 {
                continue;
            }

            let grad_t = ((py as f32) / sf).clamp(0.0, 1.0);
            let mut r = 20.0 + 16.0 * grad_t;
            let mut g = 24.0 + 12.0 * grad_t;
            let mut b = 40.0 + 24.0 * (1.0 - grad_t);

            // Subtle border highlight
            if outside > -sf * 0.03 {
                let rim = ((outside + sf * 0.03) / (sf * 0.03)).clamp(0.0, 1.0);
                r = r * (1.0 - 0.3 * rim) + 92.0 * (0.3 * rim);
                g = g * (1.0 - 0.3 * rim) + 172.0 * (0.3 * rim);
                b = b * (1.0 - 0.3 * rim) + 230.0 * (0.3 * rim);
            }

            // Outer iris orbital ring + glowing core
            let dist = (fx * fx + fy * fy).sqrt() / sf;
            let ring = (1.0 - ((dist - 0.24).abs() / 0.055)).clamp(0.0, 1.0);
            if ring > 0.0 {
                let blend = if fx > fy { 0.0 } else { 1.0 };
                let rr = 56.0 * (1.0 - blend) + 197.0 * blend;
                let rg = 189.0 * (1.0 - blend) + 134.0 * blend;
                let rb = 248.0 * (1.0 - blend) + 192.0 * blend;
                r = r * (1.0 - ring) + rr * ring;
                g = g * (1.0 - ring) + rg * ring;
                b = b * (1.0 - ring) + rb * ring;
            }
            let core = (1.0 - ((dist - 0.0) / 0.115)).clamp(0.0, 1.0);
            if core > 0.0 {
                r = r * (1.0 - core) + 90.0 * core;
                g = g * (1.0 - core) + 230.0 * core;
                b = b * (1.0 - core) + 255.0 * core;
            }

            let idx = ((py as usize) * (s as usize) + (px as usize)) * 4;
            rgba[idx] = r.clamp(0.0, 255.0) as u8;
            rgba[idx + 1] = g.clamp(0.0, 255.0) as u8;
            rgba[idx + 2] = b.clamp(0.0, 255.0) as u8;
            rgba[idx + 3] = (tile_alpha * 255.0).round() as u8;
        }
    }
    rgba
}

/// Encode a PNG image payload into a Windows `.ico` container (modern PNG-in-ICO format).
pub fn encode_png_as_ico(png_bytes: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(22 + png_bytes.len());
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // image type: 1 = icon
    out.extend_from_slice(&1u16.to_le_bytes()); // count = 1

    let wb = if width >= 256 || width == 0 {
        0u8
    } else {
        width as u8
    };
    let hb = if height >= 256 || height == 0 {
        0u8
    } else {
        height as u8
    };
    out.push(wb);
    out.push(hb);
    out.push(0); // color palette count
    out.push(0); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // color planes
    out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
    out.extend_from_slice(&(png_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&22u32.to_le_bytes()); // data offset (6 + 16)
    out.extend_from_slice(png_bytes);
    out
}

/// Encode raw bottom-up BGRA32 DIB ICO for older `windres` tools that reject PNG-in-ICO.
fn encode_rgba_as_dib_ico(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let xor_size = w * h * 4;
    let mask_stride = (w.div_ceil(32)) * 4;
    let and_size = mask_stride * h;
    let dib_size = 40 + xor_size + and_size;
    let mut out = Vec::with_capacity(22 + dib_size);

    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.push(if width >= 256 { 0 } else { width as u8 });
    out.push(if height >= 256 { 0 } else { height as u8 });
    out.push(0);
    out.push(0);
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&(dib_size as u32).to_le_bytes());
    out.extend_from_slice(&22u32.to_le_bytes());

    // BITMAPINFOHEADER (height doubled because XOR + AND masks follow)
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&((height * 2) as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // planes
    out.extend_from_slice(&32u16.to_le_bytes()); // bpp
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&((xor_size + and_size) as u32).to_le_bytes());
    out.extend_from_slice(&[0u8; 16]);

    // Bottom-up BGRA scanlines
    for row in (0..h).rev() {
        for col in 0..w {
            let p = (row * w + col) * 4;
            let r = rgba.get(p).copied().unwrap_or(0);
            let g = rgba.get(p + 1).copied().unwrap_or(0);
            let b = rgba.get(p + 2).copied().unwrap_or(0);
            let a = rgba.get(p + 3).copied().unwrap_or(0);
            out.extend_from_slice(&[b, g, r, a]);
        }
    }
    // Zero AND mask (alpha channel controls transparency)
    out.resize(out.len() + and_size, 0);
    out
}

/// Encode a 128×128 (or 256×256) PNG payload into a valid macOS `.icns` container (`ic07` chunk).
pub fn encode_png_as_icns(png_bytes: &[u8]) -> Vec<u8> {
    let chunk_len = (8 + png_bytes.len()) as u32;
    let total_len = 8 + chunk_len;
    let mut out = Vec::with_capacity(total_len as usize);
    out.extend_from_slice(b"icns");
    out.extend_from_slice(&total_len.to_be_bytes());
    out.extend_from_slice(b"ic07");
    out.extend_from_slice(&chunk_len.to_be_bytes());
    out.extend_from_slice(png_bytes);
    out
}

fn collect_files_recursive(cur: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(cur)
        .map_err(|e| format!("failed to read directory '{}': {e}", cur.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("directory entry error: {e}"))?;
        let path = entry.path();
        if path.is_dir() {
            collect_files_recursive(&path, out)?;
        } else if path.is_file() {
            out.push(path);
        }
    }
    out.sort();
    Ok(())
}

/// Package `src_dir` cleanly into a standard Deflate `.zip` archive at `zip_path`.
pub fn write_zip_directory(src_dir: &Path, zip_path: &Path) -> Result<(), String> {
    if let Some(parent) = zip_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| {
                format!("failed to create zip directory '{}': {e}", parent.display())
            })?;
        }
    }
    let root_prefix = src_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("app");

    let mut files = Vec::new();
    collect_files_recursive(src_dir, &mut files)?;

    struct CentralEntry {
        name: Vec<u8>,
        method: u16,
        crc: u32,
        comp_size: u32,
        uncomp_size: u32,
        local_offset: u32,
        ext_attr: u32,
    }

    let mut out = File::create(zip_path)
        .map_err(|e| format!("failed to create zip '{}': {e}", zip_path.display()))?;
    let mut offset: u32 = 0;
    let mut central = Vec::with_capacity(files.len());

    for file_path in files {
        let rel = file_path
            .strip_prefix(src_dir)
            .unwrap_or(file_path.as_path());
        let entry_name = format!(
            "{}/{}",
            root_prefix,
            rel.to_string_lossy().replace('\\', "/")
        );
        let name_bytes = entry_name.into_bytes();
        let raw = fs::read(&file_path)
            .map_err(|e| format!("failed to read '{}': {e}", file_path.display()))?;
        let crc = crc32_ieee(&raw);
        let uncomp_size = raw.len() as u32;

        let mut def = DeflateEncoder::new(Vec::new(), Compression::default());
        def.write_all(&raw)
            .map_err(|e| format!("deflate write failed: {e}"))?;
        let compressed = def
            .finish()
            .map_err(|e| format!("deflate finish failed: {e}"))?;

        let (method, payload): (u16, &[u8]) = if compressed.len() < raw.len() {
            (8, &compressed)
        } else {
            (0, &raw)
        };
        let comp_size = payload.len() as u32;
        let is_exec = file_path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| {
                e.eq_ignore_ascii_case("exe")
                    || e.eq_ignore_ascii_case("dll")
                    || e.eq_ignore_ascii_case("so")
                    || e.eq_ignore_ascii_case("dylib")
            });
        let ext_attr = if is_exec {
            0o100755u32 << 16
        } else {
            0o100644u32 << 16
        };

        let local_offset = offset;
        let mut hdr = Vec::with_capacity(30 + name_bytes.len());
        hdr.extend_from_slice(&0x0403_4B50u32.to_le_bytes());
        hdr.extend_from_slice(&20u16.to_le_bytes()); // version needed
        hdr.extend_from_slice(&0u16.to_le_bytes()); // flags
        hdr.extend_from_slice(&method.to_le_bytes());
        hdr.extend_from_slice(&0u16.to_le_bytes()); // mod time
        hdr.extend_from_slice(&0x5421u16.to_le_bytes()); // mod date
        hdr.extend_from_slice(&crc.to_le_bytes());
        hdr.extend_from_slice(&comp_size.to_le_bytes());
        hdr.extend_from_slice(&uncomp_size.to_le_bytes());
        hdr.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        hdr.extend_from_slice(&0u16.to_le_bytes()); // extra len
        hdr.extend_from_slice(&name_bytes);

        out.write_all(&hdr)
            .and_then(|_| out.write_all(payload))
            .map_err(|e| format!("failed to write zip entry: {e}"))?;
        offset = offset
            .checked_add(hdr.len() as u32 + comp_size)
            .ok_or_else(|| "zip archive exceeds 4 GiB".to_owned())?;

        central.push(CentralEntry {
            name: name_bytes,
            method,
            crc,
            comp_size,
            uncomp_size,
            local_offset,
            ext_attr,
        });
    }

    let cd_offset = offset;
    let mut cd_size: u32 = 0;
    for c in &central {
        let mut cd = Vec::with_capacity(46 + c.name.len());
        cd.extend_from_slice(&0x0201_4B50u32.to_le_bytes());
        cd.extend_from_slice(&((3u16 << 8) | 20u16).to_le_bytes()); // version made by
        cd.extend_from_slice(&20u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&c.method.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0x5421u16.to_le_bytes());
        cd.extend_from_slice(&c.crc.to_le_bytes());
        cd.extend_from_slice(&c.comp_size.to_le_bytes());
        cd.extend_from_slice(&c.uncomp_size.to_le_bytes());
        cd.extend_from_slice(&(c.name.len() as u16).to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes()); // extra len
        cd.extend_from_slice(&0u16.to_le_bytes()); // comment len
        cd.extend_from_slice(&0u16.to_le_bytes()); // disk number
        cd.extend_from_slice(&0u16.to_le_bytes()); // int attr
        cd.extend_from_slice(&c.ext_attr.to_le_bytes());
        cd.extend_from_slice(&c.local_offset.to_le_bytes());
        cd.extend_from_slice(&c.name);
        out.write_all(&cd)
            .map_err(|e| format!("failed to write zip central directory: {e}"))?;
        cd_size += cd.len() as u32;
    }

    let count = central.len() as u16;
    let mut eocd = Vec::with_capacity(22);
    eocd.extend_from_slice(&0x0605_4B50u32.to_le_bytes());
    eocd.extend_from_slice(&0u16.to_le_bytes());
    eocd.extend_from_slice(&0u16.to_le_bytes());
    eocd.extend_from_slice(&count.to_le_bytes());
    eocd.extend_from_slice(&count.to_le_bytes());
    eocd.extend_from_slice(&cd_size.to_le_bytes());
    eocd.extend_from_slice(&cd_offset.to_le_bytes());
    eocd.extend_from_slice(&0u16.to_le_bytes());
    out.write_all(&eocd)
        .map_err(|e| format!("failed to write zip end record: {e}"))?;
    Ok(())
}

fn sanitize_slug(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if (ch == '-' || ch == '_' || ch == ' ') && !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    if out.is_empty() {
        "iris_app".to_owned()
    } else {
        out
    }
}

fn find_manifest_near(start: &Path) -> Option<(PathBuf, Manifest)> {
    let dir = if start.is_dir() {
        start.to_path_buf()
    } else {
        start.parent()?.to_path_buf()
    };
    let toml_path = dir.join("iris.toml");
    if toml_path.is_file() {
        if let Ok(text) = fs::read_to_string(&toml_path) {
            if let Ok(m) = Manifest::parse(&text) {
                return Some((dir, m));
            }
        }
    }
    None
}

fn resolve_metadata(cfg: &BundleConfig) -> Result<ResolvedMeta, String> {
    let input_abs = if cfg.input.is_relative() {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(&cfg.input)
    } else {
        cfg.input.clone()
    };

    let (root_dir, manifest_opt) = if input_abs.is_dir() {
        if let Some((dir, m)) = find_manifest_near(&input_abs) {
            (dir, Some(m))
        } else {
            let main_f = input_abs.join("main.iris");
            if !main_f.is_file() {
                return Err(format!(
                    "no `iris.toml` or `main.iris` found in '{}'",
                    input_abs.display()
                ));
            }
            (input_abs.clone(), None)
        }
    } else {
        let dir = input_abs
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let found = find_manifest_near(&dir);
        (dir, found.map(|(_, m)| m))
    };

    let entry_file = if input_abs.is_file() {
        input_abs.clone()
    } else if let Some(ref m) = manifest_opt {
        root_dir.join(&m.entry)
    } else {
        root_dir.join("main.iris")
    };

    if !entry_file.is_file() {
        return Err(format!(
            "entry source file '{}' does not exist",
            entry_file.display()
        ));
    }

    let source_text = fs::read_to_string(&entry_file).unwrap_or_default();
    let app_cfg = manifest_opt
        .as_ref()
        .and_then(|m| m.app.clone())
        .unwrap_or_else(AppManifest::default);

    let pkg_name = manifest_opt
        .as_ref()
        .map(|m| m.name.clone())
        .unwrap_or_else(|| {
            entry_file
                .parent()
                .and_then(|p| {
                    if p == Path::new("") {
                        None
                    } else {
                        entry_file.file_stem()
                    }
                })
                .and_then(|s| s.to_str())
                .unwrap_or("iris_app")
                .to_owned()
        });

    let title = cfg
        .title
        .clone()
        .or(app_cfg.title)
        .unwrap_or_else(|| pkg_name.clone());
    let slug = sanitize_slug(&pkg_name);
    let version = manifest_opt
        .as_ref()
        .map(|m| m.version.clone())
        .unwrap_or_else(|| "0.1.0".to_owned());
    let identifier = cfg
        .identifier
        .clone()
        .or(app_cfg.identifier)
        .unwrap_or_else(|| format!("dev.iris.{slug}"));
    let publisher = app_cfg.publisher.unwrap_or_else(|| "IRIS".to_owned());
    let categories = app_cfg
        .categories
        .unwrap_or_else(|| "Utility;Development;".to_owned());
    let imports_ui = source_text.contains("bring std.ui");
    let use_gui = cfg.gui.or(app_cfg.gui).unwrap_or(imports_ui);
    let bundle_ui_host = app_cfg.bundle_ui_host.unwrap_or(imports_ui || use_gui);
    let icon_path = cfg
        .icon
        .clone()
        .or_else(|| app_cfg.icon.as_ref().map(|p| root_dir.join(p)));

    Ok(ResolvedMeta {
        root_dir,
        entry_file,
        title,
        slug,
        version,
        identifier,
        publisher,
        categories,
        use_gui,
        bundle_ui_host,
        icon_path,
        assets: app_cfg.assets,
        window_width: app_cfg.window_width.unwrap_or(1080),
        window_height: app_cfg.window_height.unwrap_or(720),
        min_width: app_cfg.min_width.unwrap_or(360),
        min_height: app_cfg.min_height.unwrap_or(240),
        resizable: app_cfg.resizable.unwrap_or(true),
    })
}

fn ui_host_lib_filename() -> &'static str {
    if cfg!(target_os = "windows") {
        "iris_ui_host.dll"
    } else if cfg!(target_os = "macos") {
        "libiris_ui_host.dylib"
    } else {
        "libiris_ui_host.so"
    }
}

fn find_repo_root(start: &Path) -> Option<PathBuf> {
    let mut cur = Some(start);
    while let Some(dir) = cur {
        if dir
            .join("crates")
            .join("iris_ui_host")
            .join("Cargo.toml")
            .is_file()
        {
            return Some(dir.to_path_buf());
        }
        cur = dir.parent();
    }
    None
}

/// Locate a preinstalled `iris_ui_host` dynamic library via `IRIS_UI_HOST_LIB`,
/// next to the running `iris` binary, inside any discovered install prefix, or
/// in standard Unix system library directories.
pub fn find_installed_ui_host() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("IRIS_UI_HOST_LIB") {
        let p = PathBuf::from(explicit);
        if p.is_file() {
            return Some(p);
        }
    }
    let lib_name = ui_host_lib_filename();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let cand = dir.join(lib_name);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    for prefix in crate::codegen::build::install_prefix_dirs() {
        for sub in ["bin", "", "lib", "share/iris"] {
            let cand = if sub.is_empty() {
                prefix.join(lib_name)
            } else {
                prefix.join(sub).join(lib_name)
            };
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        for dir in [
            "/usr/local/lib",
            "/usr/lib",
            "/usr/share/iris",
            "/usr/local/share/iris",
        ] {
            let cand = Path::new(dir).join(lib_name);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

fn find_or_build_ui_host(start: &Path, prefer_release: bool) -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("IRIS_UI_HOST_LIB") {
        let p = PathBuf::from(explicit);
        if p.is_file() {
            return Some(p);
        }
    }
    let lib_name = ui_host_lib_filename();
    // Prefer workspace target/release before target/debug (and before current_exe().parent())
    // so running `cargo` / `./target/debug/iris.exe` never accidentally ships the slow debug DLL.
    let profile_order: &[&str] = if prefer_release {
        &["release"]
    } else {
        &["release", "debug"]
    };

    for anchor in [
        start.to_path_buf(),
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from(".")),
    ] {
        let mut cur = Some(anchor.as_path());
        while let Some(dir) = cur {
            for prof in profile_order {
                let cand = dir.join("target").join(prof).join(lib_name);
                if cand.is_file() {
                    return Some(cand);
                }
            }
            cur = dir.parent();
        }
    }

    if let Some(installed) = find_installed_ui_host() {
        return Some(installed);
    }

    let repo = find_repo_root(start)
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .and_then(|cwd| find_repo_root(&cwd))
        })
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|e| e.parent().and_then(find_repo_root))
        });
    if let Some(repo) = repo {
        let profile_dir = if prefer_release { "release" } else { "debug" };
        let mut cmd = Command::new("cargo");
        cmd.current_dir(&repo).args(["build", "-p", "iris_ui_host"]);
        if prefer_release {
            cmd.arg("--release");
        }
        if let Ok(status) = cmd.status() {
            if status.success() {
                let built = repo.join("target").join(profile_dir).join(lib_name);
                if built.is_file() {
                    return Some(built);
                }
                if let Ok(target) = std::env::var("CARGO_BUILD_TARGET") {
                    let trimmed = target.trim();
                    if !trimmed.is_empty() {
                        let targeted = repo
                            .join("target")
                            .join(trimmed)
                            .join(profile_dir)
                            .join(lib_name);
                        if targeted.is_file() {
                            return Some(targeted);
                        }
                    }
                }
            }
        }
    }

    None
}

fn find_windres() -> Option<String> {
    for candidate in [
        "windres",
        r"C:\msys64\ucrt64\bin\windres.exe",
        r"C:\msys64\mingw64\bin\windres.exe",
    ] {
        if Command::new(candidate)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
        {
            return Some(candidate.to_owned());
        }
    }
    None
}

fn parse_version_quad(version: &str) -> (u16, u16, u16, u16) {
    let mut parts = [0u16; 4];
    for (i, seg) in version.split(['.', '-']).take(4).enumerate() {
        parts[i] = seg.parse::<u16>().unwrap_or(0);
    }
    (parts[0], parts[1], parts[2], parts[3])
}

fn compile_windows_resource_object(
    meta: &ResolvedMeta,
    ico_bytes: &[u8],
    rgba64: &[u8],
    tmp_dir: &Path,
) -> Option<PathBuf> {
    if !cfg!(target_os = "windows") {
        return None;
    }
    let windres = find_windres()?;
    let ico_path = tmp_dir.join("app.ico");
    if fs::write(&ico_path, ico_bytes).is_err() {
        return None;
    }
    let (v0, v1, v2, v3) = parse_version_quad(&meta.version);
    let esc_title = meta.title.replace('"', "'");
    let esc_pub = meta.publisher.replace('"', "'");
    let esc_slug = meta.slug.replace('"', "'");
    let rc_content = format!(
        "1 ICON \"app.ico\"\n\
         1 VERSIONINFO\n\
         FILEVERSION {v0},{v1},{v2},{v3}\n\
         PRODUCTVERSION {v0},{v1},{v2},{v3}\n\
         FILEOS 0x40004\n\
         FILETYPE 0x1\n\
         BEGIN\n\
           BLOCK \"StringFileInfo\"\n\
           BEGIN\n\
             BLOCK \"040904B0\"\n\
             BEGIN\n\
               VALUE \"CompanyName\", \"{esc_pub}\"\n\
               VALUE \"FileDescription\", \"{esc_title}\"\n\
               VALUE \"FileVersion\", \"{v0}.{v1}.{v2}.{v3}\"\n\
               VALUE \"InternalName\", \"{esc_slug}\"\n\
               VALUE \"OriginalFilename\", \"{esc_slug}.exe\"\n\
               VALUE \"ProductName\", \"{esc_title}\"\n\
               VALUE \"ProductVersion\", \"{v0}.{v1}.{v2}.{v3}\"\n\
             END\n\
           END\n\
           BLOCK \"VarFileInfo\"\n\
           BEGIN\n\
             VALUE \"Translation\", 0x0409, 1200\n\
           END\n\
         END\n"
    );
    let rc_path = tmp_dir.join("app.rc");
    let res_obj = tmp_dir.join("app_res.o");
    if fs::write(&rc_path, rc_content).is_err() {
        return None;
    }
    let try_run = |w: &str| {
        Command::new(w)
            .current_dir(tmp_dir)
            .args(["-i", "app.rc", "-O", "coff", "-o", "app_res.o"])
            .output()
    };
    if let Ok(out) = try_run(&windres) {
        if out.status.success() && res_obj.is_file() {
            return Some(res_obj);
        }
    }
    // Fallback to uncompressed DIB ICO for older GNU windres builds
    let dib_ico = encode_rgba_as_dib_ico(64, 64, rgba64);
    if fs::write(&ico_path, dib_ico).is_ok() {
        if let Ok(out) = try_run(&windres) {
            if out.status.success() && res_obj.is_file() {
                return Some(res_obj);
            }
        }
    }
    None
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst)
        .map_err(|e| format!("failed to create directory '{}': {e}", dst.display()))?;
    for entry in
        fs::read_dir(src).map_err(|e| format!("failed to read '{}': {e}", src.display()))?
    {
        let entry = entry.map_err(|e| format!("directory entry error: {e}"))?;
        let p = entry.path();
        let target = dst.join(entry.file_name());
        if p.is_dir() {
            copy_dir_recursive(&p, &target)?;
        } else if p.is_file() {
            fs::copy(&p, &target).map_err(|e| {
                format!(
                    "failed to copy '{}' to '{}': {e}",
                    p.display(),
                    target.display()
                )
            })?;
        }
    }
    Ok(())
}

fn stage_assets(meta: &ResolvedMeta, dest_root: &Path) -> Result<(), String> {
    for rel in &meta.assets {
        let src = meta.root_dir.join(rel);
        if !src.exists() {
            return Err(format!("declared asset '{}' does not exist", src.display()));
        }
        let file_name = src
            .file_name()
            .ok_or_else(|| format!("invalid asset path '{rel}'"))?;
        let target = dest_root.join(file_name);
        if src.is_dir() {
            copy_dir_recursive(&src, &target)?;
        } else {
            if let Some(p) = target.parent() {
                fs::create_dir_all(p).map_err(|e| format!("failed to create asset dir: {e}"))?;
            }
            fs::copy(&src, &target)
                .map_err(|e| format!("failed to copy asset '{}': {e}", src.display()))?;
        }
    }
    Ok(())
}

struct PreparedIcons {
    png: Vec<u8>,
    ico: Vec<u8>,
    icns: Vec<u8>,
    rgba: Vec<u8>,
}

fn build_icon_artifacts(meta: &ResolvedMeta) -> Result<PreparedIcons, String> {
    let rgba64 = generate_default_iris_icon_rgba(64);
    let default_png64 = encode_rgba_png(64, 64, &rgba64)?;
    let default_png128 = encode_rgba_png(128, 128, &generate_default_iris_icon_rgba(128))?;
    let mut png_bytes = default_png64.clone();
    let mut ico_bytes = encode_png_as_ico(&default_png64, 64, 64);
    let mut icns_bytes = encode_png_as_icns(&default_png128);

    if let Some(ref custom) = meta.icon_path {
        if !custom.is_file() {
            return Err(format!("icon file '{}' does not exist", custom.display()));
        }
        let bytes = fs::read(custom)
            .map_err(|e| format!("failed to read icon '{}': {e}", custom.display()))?;
        let ext = custom
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        match ext.as_str() {
            "ico" => ico_bytes = bytes,
            "icns" => icns_bytes = bytes,
            "png" => {
                ico_bytes = encode_png_as_ico(&bytes, 128, 128);
                icns_bytes = encode_png_as_icns(&bytes);
                png_bytes = bytes;
            }
            other => {
                return Err(format!(
                    "unsupported icon format '.{other}' (expected .png, .ico, or .icns)"
                ));
            }
        }
    }
    Ok(PreparedIcons {
        png: png_bytes,
        ico: ico_bytes,
        icns: icns_bytes,
        rgba: rgba64,
    })
}

fn write_app_metadata_json(meta: &ResolvedMeta, dst: &Path) -> Result<(), String> {
    let doc = serde_json::json!({
        "name": meta.slug,
        "title": meta.title,
        "version": meta.version,
        "identifier": meta.identifier,
        "publisher": meta.publisher,
        "gui": meta.use_gui,
        "window": {
            "width": meta.window_width,
            "height": meta.window_height,
            "min_width": meta.min_width,
            "min_height": meta.min_height,
            "resizable": meta.resizable,
        }
    });
    let text = serde_json::to_string_pretty(&doc)
        .map_err(|e| format!("failed to serialize app.json: {e}"))?;
    fs::write(dst, text).map_err(|e| format!("failed to write '{}': {e}", dst.display()))
}

fn default_install_dir(meta: &ResolvedMeta) -> Result<PathBuf, String> {
    if cfg!(target_os = "windows") {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .ok_or_else(|| "LOCALAPPDATA environment variable is not set".to_owned())?;
        Ok(base.join("Programs").join(&meta.title))
    } else if cfg!(target_os = "macos") {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| "HOME environment variable is not set".to_owned())?;
        Ok(home
            .join("Applications")
            .join(format!("{}.app", meta.title)))
    } else {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| "HOME environment variable is not set".to_owned())?;
        Ok(home.join(".local").join("share").join(&meta.slug))
    }
}

fn register_os_installation(
    meta: &ResolvedMeta,
    install_dir: &Path,
    exe_path: &Path,
) -> Result<(), String> {
    if cfg!(target_os = "windows") {
        let esc_title = meta.title.replace('\'', "''");
        let esc_ver = meta.version.replace('\'', "''");
        let esc_pub = meta.publisher.replace('\'', "''");
        let esc_slug = meta.slug.replace('\'', "''");
        let esc_dir = install_dir.display().to_string().replace('\'', "''");
        let esc_exe = exe_path.display().to_string().replace('\'', "''");
        let ps = format!(
            "$ws = New-Object -ComObject WScript.Shell;\n\
             $lnkDir = Join-Path $env:APPDATA 'Microsoft\\Windows\\Start Menu\\Programs';\n\
             New-Item -ItemType Directory -Force -Path $lnkDir | Out-Null;\n\
             $sc = $ws.CreateShortcut((Join-Path $lnkDir ('{esc_title}.lnk')));\n\
             $sc.TargetPath = '{esc_exe}';\n\
             $sc.WorkingDirectory = '{esc_dir}';\n\
             $sc.IconLocation = '{esc_exe},0';\n\
             $sc.Save();\n\
             $reg = 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\IRIS_{esc_slug}';\n\
             New-Item -Path $reg -Force | Out-Null;\n\
             New-ItemProperty -Path $reg -Name 'DisplayName' -Value '{esc_title}' -Force | Out-Null;\n\
             New-ItemProperty -Path $reg -Name 'DisplayVersion' -Value '{esc_ver}' -Force | Out-Null;\n\
             New-ItemProperty -Path $reg -Name 'Publisher' -Value '{esc_pub}' -Force | Out-Null;\n\
             New-ItemProperty -Path $reg -Name 'InstallLocation' -Value '{esc_dir}' -Force | Out-Null;\n\
             New-ItemProperty -Path $reg -Name 'DisplayIcon' -Value '{esc_exe}' -Force | Out-Null;"
        );
        let out = Command::new("powershell")
            .args(["-NoProfile", "-Command", &ps])
            .output()
            .map_err(|e| format!("failed to launch powershell for shortcut registration: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "Windows installation registration failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
    } else if cfg!(target_os = "linux") {
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            let apps = home.join(".local").join("share").join("applications");
            fs::create_dir_all(&apps)
                .map_err(|e| format!("failed to create applications dir: {e}"))?;
            let desktop = format!(
                "[Desktop Entry]\nType=Application\nName={}\nExec=\"{}\"\nIcon={}\nCategories={}\nTerminal={}\n",
                meta.title,
                exe_path.display(),
                install_dir.join(format!("{}.png", meta.slug)).display(),
                meta.categories,
                if meta.use_gui { "false" } else { "true" }
            );
            fs::write(apps.join(format!("{}.desktop", meta.slug)), desktop)
                .map_err(|e| format!("failed to write .desktop entry: {e}"))?;
        }
    }
    Ok(())
}

fn unregister_os_installation(meta: &ResolvedMeta) {
    if cfg!(target_os = "windows") {
        let esc_title = meta.title.replace('\'', "''");
        let esc_slug = meta.slug.replace('\'', "''");
        let ps = format!(
            "Remove-Item -ErrorAction SilentlyContinue (Join-Path $env:APPDATA 'Microsoft\\Windows\\Start Menu\\Programs\\{esc_title}.lnk');\n\
             Remove-Item -Recurse -Force -ErrorAction SilentlyContinue 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\IRIS_{esc_slug}';"
        );
        let _ = Command::new("powershell")
            .args(["-NoProfile", "-Command", &ps])
            .output();
    } else if cfg!(target_os = "linux") {
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            let _ = fs::remove_file(
                home.join(".local")
                    .join("share")
                    .join("applications")
                    .join(format!("{}.desktop", meta.slug)),
            );
        }
    }
}

fn generate_inno_script(
    meta: &ResolvedMeta,
    bundle_dir: &Path,
    iss_path: &Path,
    out_dir: &Path,
    has_ico: bool,
) -> Result<(), String> {
    let esc_src = bundle_dir.display().to_string();
    let esc_out = out_dir.display().to_string();
    let ico_line = if has_ico {
        format!("SetupIconFile={}\n", bundle_dir.join("app.ico").display())
    } else {
        String::new()
    };
    let iss = format!(
        "[Setup]\n\
         AppId={{{{{id}}}}}\n\
         AppName={title}\n\
         AppVersion={version}\n\
         AppPublisher={pub_name}\n\
         DefaultDirName={{autopf}}\\{title}\n\
         DefaultGroupName={title}\n\
         OutputDir={out_dir}\n\
         OutputBaseFilename={slug}-setup-{version}\n\
         Compression=lzma\n\
         SolidCompression=yes\n\
         ArchitecturesAllowed=x64compatible\n\
         ArchitecturesInstallIn64BitMode=x64compatible\n\
         {ico_line}\n\
         [Files]\n\
         Source: \"{src}\\*\"; DestDir: \"{{app}}\"; Flags: ignoreversion recursesubdirs createallsubdirs\n\n\
         [Icons]\n\
         Name: \"{{group}}\\{title}\"; Filename: \"{{app}}\\{slug}.exe\"\n\
         Name: \"{{autodesktop}}\\{title}\"; Filename: \"{{app}}\\{slug}.exe\"\n\n\
         [Run]\n\
         Filename: \"{{app}}\\{slug}.exe\"; Description: \"Launch {title}\"; Flags: nowait postinstall skipifsilent\n",
        id = meta.identifier,
        title = meta.title,
        version = meta.version,
        pub_name = meta.publisher,
        out_dir = esc_out,
        slug = meta.slug,
        src = esc_src,
    );
    fs::write(iss_path, iss).map_err(|e| format!("failed to write '{}': {e}", iss_path.display()))
}

fn try_run_iscc(iss_path: &Path) -> Option<PathBuf> {
    for candidate in [
        "iscc",
        r"C:\Program Files (x86)\Inno Setup 6\ISCC.exe",
        r"C:\Program Files\Inno Setup 6\ISCC.exe",
    ] {
        if let Ok(out) = Command::new(candidate).arg(iss_path).output() {
            if out.status.success() {
                return Some(PathBuf::from(candidate));
            }
        }
    }
    None
}

/// Compile, bundle icons/runtime libraries, package, and optionally install an IRIS app.
pub fn run_bundle(cfg: &BundleConfig) -> Result<BundleOutput, String> {
    let meta = resolve_metadata(cfg)?;
    let fmt_used = cfg.format.resolve_auto();

    if cfg.uninstall {
        let target_dir = match &cfg.install_dir {
            Some(d) => d.clone(),
            None => default_install_dir(&meta)?,
        };
        if target_dir.exists() {
            fs::remove_dir_all(&target_dir)
                .map_err(|e| format!("failed to remove '{}': {e}", target_dir.display()))?;
        }
        if !cfg.no_register {
            unregister_os_installation(&meta);
        }
        return Ok(BundleOutput {
            bundle_dir: target_dir.clone(),
            artifact_path: target_dir.clone(),
            executable_path: target_dir,
            ui_host_bundled: None,
            installed_dir: None,
            format_used: fmt_used,
            note: Some(format!("uninstalled '{}'", meta.title)),
        });
    }

    let base_out = cfg.output.clone().unwrap_or_else(|| {
        meta.root_dir
            .join("target")
            .join("bundle")
            .join(if fmt_used == BundleFormat::App {
                format!("{}.app", meta.title)
            } else {
                meta.slug.clone()
            })
    });

    let staging_dir = if fmt_used == BundleFormat::Zip || fmt_used == BundleFormat::Inno {
        base_out.with_extension("bundle_dir")
    } else if fmt_used == BundleFormat::AppImage
        && base_out.extension().and_then(|s| s.to_str()) != Some("AppDir")
    {
        base_out.with_extension("AppDir")
    } else {
        base_out.clone()
    };

    if staging_dir.exists() {
        let _ = fs::remove_dir_all(&staging_dir);
    }
    fs::create_dir_all(&staging_dir).map_err(|e| {
        format!(
            "failed to create bundle directory '{}': {e}",
            staging_dir.display()
        )
    })?;

    let PreparedIcons {
        png: png_bytes,
        ico: ico_bytes,
        icns: icns_bytes,
        rgba: rgba64,
    } = build_icon_artifacts(&meta)?;

    let tmp_res = std::env::temp_dir().join(format!(
        "iris_bundle_res_{}_{}",
        meta.slug,
        std::process::id()
    ));
    let _ = fs::create_dir_all(&tmp_res);
    let extra_obj = compile_windows_resource_object(&meta, &ico_bytes, &rgba64, &tmp_res);

    let (exe_dest, ui_host_dest_dir, assets_dest, meta_dest, rpaths) = match fmt_used {
        BundleFormat::App => {
            let contents = staging_dir.join("Contents");
            let macos_dir = contents.join("MacOS");
            let fw_dir = contents.join("Frameworks");
            let res_dir = contents.join("Resources");
            fs::create_dir_all(&macos_dir).map_err(|e| e.to_string())?;
            fs::create_dir_all(&fw_dir).map_err(|e| e.to_string())?;
            fs::create_dir_all(&res_dir).map_err(|e| e.to_string())?;
            let plist = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                 <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
                 <plist version=\"1.0\"><dict>\n\
                   <key>CFBundleName</key><string>{}</string>\n\
                   <key>CFBundleDisplayName</key><string>{}</string>\n\
                   <key>CFBundleIdentifier</key><string>{}</string>\n\
                   <key>CFBundleVersion</key><string>{}</string>\n\
                   <key>CFBundleShortVersionString</key><string>{}</string>\n\
                   <key>CFBundleExecutable</key><string>{}</string>\n\
                   <key>CFBundleIconFile</key><string>app.icns</string>\n\
                   <key>CFBundlePackageType</key><string>APPL</string>\n\
                   <key>NSHighResolutionCapable</key><true/>\n\
                 </dict></plist>\n",
                meta.title, meta.title, meta.identifier, meta.version, meta.version, meta.slug
            );
            fs::write(contents.join("Info.plist"), plist).map_err(|e| e.to_string())?;
            fs::write(res_dir.join("app.icns"), &icns_bytes).map_err(|e| e.to_string())?;
            (
                macos_dir.join(&meta.slug),
                fw_dir,
                res_dir.clone(),
                res_dir.join("app.json"),
                vec![
                    "@executable_path/../Frameworks".to_owned(),
                    "@executable_path".to_owned(),
                ],
            )
        }
        BundleFormat::AppImage => {
            let usr_bin = staging_dir.join("usr").join("bin");
            let lib_dir = staging_dir.join("lib");
            fs::create_dir_all(&usr_bin).map_err(|e| e.to_string())?;
            fs::create_dir_all(&lib_dir).map_err(|e| e.to_string())?;
            fs::write(staging_dir.join(format!("{}.png", meta.slug)), &png_bytes)
                .map_err(|e| e.to_string())?;
            let desktop = format!(
                "[Desktop Entry]\nType=Application\nName={}\nExec={}\nIcon={}\nCategories={}\nTerminal={}\n",
                meta.title,
                meta.slug,
                meta.slug,
                meta.categories,
                if meta.use_gui { "false" } else { "true" }
            );
            fs::write(staging_dir.join(format!("{}.desktop", meta.slug)), desktop)
                .map_err(|e| e.to_string())?;
            let apprun = format!(
                "#!/bin/sh\nHERE=\"$(dirname \"$(readlink -f \"$0\")\")\"\n\
                 export LD_LIBRARY_PATH=\"$HERE/lib:$HERE/usr/lib:$LD_LIBRARY_PATH\"\n\
                 exec \"$HERE/usr/bin/{}\" \"$@\"\n",
                meta.slug
            );
            let apprun_path = staging_dir.join("AppRun");
            fs::write(&apprun_path, apprun).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(m) = fs::metadata(&apprun_path) {
                    let mut p = m.permissions();
                    p.set_mode(0o755);
                    let _ = fs::set_permissions(&apprun_path, p);
                }
            }
            (
                usr_bin.join(&meta.slug),
                lib_dir,
                staging_dir.clone(),
                staging_dir.join("app.json"),
                vec!["$ORIGIN/../../lib".to_owned(), "$ORIGIN/../lib".to_owned()],
            )
        }
        _ => {
            let exe_name = format!("{}{}", meta.slug, std::env::consts::EXE_SUFFIX);
            let _ = fs::write(staging_dir.join("app.png"), &png_bytes);
            if cfg!(target_os = "windows") {
                let _ = fs::write(staging_dir.join("app.ico"), &ico_bytes);
            }
            let rpaths = if cfg!(target_os = "macos") {
                vec![
                    "@executable_path/lib".to_owned(),
                    "@executable_path".to_owned(),
                ]
            } else {
                vec!["$ORIGIN/lib".to_owned(), "$ORIGIN".to_owned()]
            };
            (
                staging_dir.join(exe_name),
                staging_dir.clone(),
                staging_dir.clone(),
                staging_dir.join("app.json"),
                rpaths,
            )
        }
    };

    let module = crate::compile_file_to_module(&meta.entry_file).map_err(|e| {
        let src = fs::read_to_string(&meta.entry_file).unwrap_or_default();
        crate::diagnostics::render_error(&src, &e)
    })?;
    crate::security::validate_module_ffi(&module.extern_fns).map_err(|e| e.to_string())?;
    let policy = crate::security::get_security_policy();

    let mut extra_objects = Vec::new();
    if let Some(obj) = extra_obj {
        extra_objects.push(obj);
    }
    let build_opts = NativeBuildOptions {
        gui_subsystem: meta.use_gui,
        extra_objects,
        rpaths,
    };
    let built_exe =
        build_binary_with_security_and_options(&module, &exe_dest, None, &policy, &build_opts)
            .map_err(|e| format!("failed to build standalone binary: {e}"))?;
    let _ = fs::remove_dir_all(&tmp_res);

    let mut ui_host_bundled = None;
    if meta.bundle_ui_host {
        if let Some(lib_src) = find_or_build_ui_host(&meta.root_dir, cfg.release) {
            fs::create_dir_all(&ui_host_dest_dir).map_err(|e| e.to_string())?;
            let dst = ui_host_dest_dir.join(ui_host_lib_filename());
            fs::copy(&lib_src, &dst).map_err(|e| {
                format!(
                    "failed to copy UI host library '{}' to '{}': {e}",
                    lib_src.display(),
                    dst.display()
                )
            })?;
            ui_host_bundled = Some(dst);
        } else {
            return Err(format!(
                "could not find `{}` to bundle; build it with `cargo build --release -p iris_ui_host` or set IRIS_UI_HOST_LIB",
                ui_host_lib_filename()
            ));
        }
    }

    stage_assets(&meta, &assets_dest)?;
    write_app_metadata_json(&meta, &meta_dest)?;

    let mut note: Option<String> = None;
    let artifact_path = match fmt_used {
        BundleFormat::Zip => {
            let zip_out = if base_out.extension().and_then(|s| s.to_str()) == Some("zip") {
                base_out.clone()
            } else {
                base_out.with_extension("zip")
            };
            write_zip_directory(&staging_dir, &zip_out)?;
            zip_out
        }
        BundleFormat::Inno => {
            let parent = base_out
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            fs::create_dir_all(&parent).map_err(|e| e.to_string())?;
            let iss_path = parent.join(format!("{}.iss", meta.slug));
            generate_inno_script(
                &meta,
                &staging_dir,
                &iss_path,
                &parent,
                cfg!(target_os = "windows"),
            )?;
            if let Some(_compiler) = try_run_iscc(&iss_path) {
                let exe_out = parent.join(format!("{}-setup-{}.exe", meta.slug, meta.version));
                if exe_out.is_file() {
                    exe_out
                } else {
                    iss_path
                }
            } else {
                note = Some(format!(
                    "ISCC.exe not found on PATH; generated Inno Setup script '{}'",
                    iss_path.display()
                ));
                iss_path
            }
        }
        BundleFormat::App => {
            if cfg!(target_os = "macos") {
                let _ = Command::new("codesign")
                    .args(["--force", "--deep", "--sign", "-"])
                    .arg(&staging_dir)
                    .output();
            }
            staging_dir.clone()
        }
        BundleFormat::AppImage => {
            let appimg_out = base_out.with_extension("AppImage");
            if let Ok(out) = Command::new("appimagetool")
                .arg(&staging_dir)
                .arg(&appimg_out)
                .output()
            {
                if out.status.success() && appimg_out.is_file() {
                    appimg_out
                } else {
                    staging_dir.clone()
                }
            } else {
                note = Some(format!(
                    "appimagetool not found; prepared AppDir '{}'",
                    staging_dir.display()
                ));
                staging_dir.clone()
            }
        }
        BundleFormat::Dir | BundleFormat::Auto => staging_dir.clone(),
    };

    let installed_dir = if cfg.install {
        let dest = match &cfg.install_dir {
            Some(d) => d.clone(),
            None => default_install_dir(&meta)?,
        };
        if dest.exists() {
            let _ = fs::remove_dir_all(&dest);
        }
        copy_dir_recursive(&staging_dir, &dest)?;
        let installed_exe = match fmt_used {
            BundleFormat::App => dest.join("Contents").join("MacOS").join(&meta.slug),
            BundleFormat::AppImage => dest.join("usr").join("bin").join(&meta.slug),
            _ => dest.join(built_exe.file_name().unwrap_or_default()),
        };
        if !cfg.no_register {
            register_os_installation(&meta, &dest, &installed_exe)?;
        }
        Some(dest)
    } else {
        None
    };

    Ok(BundleOutput {
        bundle_dir: staging_dir,
        artifact_path,
        executable_path: built_exe,
        ui_host_bundled,
        installed_dir,
        format_used: fmt_used,
        note,
    })
}
