//! Native live camera capture (Windows Media Foundation via dynamic Win32 loading)
//! with deterministic synthetic tactical sensor fallback and fast grid downsampling.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub const CAM_MODE_OFFLINE: i32 = 0;
pub const CAM_MODE_LIVE: i32 = 1;
pub const CAM_MODE_SYNTHETIC: i32 = 2;

pub const CAM_TINT_RGB: i32 = 0;
pub const CAM_TINT_NVG: i32 = 1;
pub const CAM_TINT_FLIR: i32 = 2;
pub const CAM_TINT_SLATE: i32 = 3;

pub const MAX_SIM_TARGETS: usize = 6;

#[derive(Debug, Clone, Copy, Default)]
pub struct SimTargetSpec {
    pub nx: f32,
    pub ny: f32,
    pub radius: f32,
    pub alive: bool,
    pub hit_flash: u8,
}

#[derive(Debug, Clone)]
pub struct CameraFrameSnapshot {
    pub width: u32,
    pub height: u32,
    pub seq: u64,
    pub rgba: Vec<u8>,
    pub mode: i32,
    pub device_name: String,
}

#[derive(Debug, Clone)]
pub struct CameraInfo {
    pub width: u32,
    pub height: u32,
    pub mode: i32,
    pub seq: u64,
    pub device_name: String,
}

struct CameraSharedState {
    width: u32,
    height: u32,
    seq: u64,
    rgba: Vec<u8>,
    mode: i32,
    device_name: String,
    sim_targets: [SimTargetSpec; MAX_SIM_TARGETS],
    sim_dirty: bool,
    sim_external: bool,
    sim_backdrop: Vec<u8>,
    sim_backdrop_dims: (u32, u32),
}

impl CameraSharedState {
    fn new(width: u32, height: u32, mode: i32, device_name: String) -> Self {
        let mut sim_targets = [SimTargetSpec::default(); MAX_SIM_TARGETS];
        sim_targets[0] = SimTargetSpec {
            nx: 0.28,
            ny: 0.38,
            radius: 0.055,
            alive: true,
            hit_flash: 0,
        };
        sim_targets[1] = SimTargetSpec {
            nx: 0.68,
            ny: 0.56,
            radius: 0.048,
            alive: true,
            hit_flash: 0,
        };
        Self {
            width,
            height,
            seq: 0,
            rgba: vec![0; (width as usize) * (height as usize) * 4],
            mode,
            device_name,
            sim_targets,
            sim_dirty: true,
            sim_external: false,
            sim_backdrop: Vec::new(),
            sim_backdrop_dims: (0, 0),
        }
    }
}

struct CameraSession {
    shared: Arc<Mutex<CameraSharedState>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Drop for CameraSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

pub struct CameraManager {
    next_id: i64,
    sessions: HashMap<i64, CameraSession>,
}

impl Default for CameraManager {
    fn default() -> Self {
        Self::new()
    }
}

impl CameraManager {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            sessions: HashMap::new(),
        }
    }

    pub fn open(
        &mut self,
        device_index: i64,
        req_w: u32,
        req_h: u32,
        synthetic_only: bool,
    ) -> Result<i64, String> {
        let w = req_w.clamp(160, 1920);
        let h = req_h.clamp(120, 1080);
        let id = self.next_id;
        self.next_id += 1;

        let shared = Arc::new(Mutex::new(CameraSharedState::new(
            640,
            360,
            CAM_MODE_SYNTHETIC,
            "IRIS Tactical Sim Feed".to_owned(),
        )));

        // Synchronously render one initial frame so immediate headless queries always see valid data.
        render_synthetic_frame(&shared, 0.0);

        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::sync_channel::<()>(1);
        let shared_w = Arc::clone(&shared);
        let stop_w = Arc::clone(&stop);

        let handle = thread::Builder::new()
            .name(format!("iris-cam-{id}"))
            .spawn(move || {
                run_camera_worker(device_index, w, h, synthetic_only, shared_w, stop_w, tx);
            })
            .map_err(|e| format!("failed to spawn camera worker thread: {e}"))?;

        let _ = rx.recv_timeout(Duration::from_millis(2200));

        self.sessions.insert(
            id,
            CameraSession {
                shared,
                stop,
                worker: Some(handle),
            },
        );
        Ok(id)
    }

    pub fn close(&mut self, cam_id: i64) -> bool {
        self.sessions.remove(&cam_id).is_some()
    }

    pub fn info(&self, cam_id: i64) -> Option<CameraInfo> {
        let sess = self.sessions.get(&cam_id)?;
        let guard = sess.shared.lock().ok()?;
        Some(CameraInfo {
            width: guard.width,
            height: guard.height,
            mode: guard.mode,
            seq: guard.seq,
            device_name: guard.device_name.clone(),
        })
    }

    pub fn set_sim_target(&self, cam_id: i64, slot: usize, spec: SimTargetSpec) -> bool {
        let Some(sess) = self.sessions.get(&cam_id) else {
            return false;
        };
        let Ok(mut guard) = sess.shared.lock() else {
            return false;
        };
        if slot >= MAX_SIM_TARGETS {
            return false;
        }
        guard.sim_targets[slot] = spec;
        guard.sim_dirty = true;
        guard.sim_external = true;
        guard.seq = guard.seq.wrapping_add(1);
        true
    }

    /// Invoke `f` while borrowing the latest `RGBA8` frame directly with zero intermediate copy.
    pub fn with_latest_frame<R>(
        &self,
        cam_id: i64,
        f: impl FnOnce(u32, u32, &[u8]) -> R,
    ) -> Option<R> {
        let sess = self.sessions.get(&cam_id)?;
        let mut guard = sess.shared.lock().ok()?;
        ensure_synthetic_fresh(&mut guard);
        let w = guard.width;
        let h = guard.height;
        let total = (w as usize) * (h as usize) * 4;
        if w == 0 || h == 0 || guard.rgba.len() < total {
            return None;
        }
        Some(f(w, h, &guard.rgba[..total]))
    }

    /// Downsample latest frame into `out_buf` (`grid_w * grid_h * 4` straight RGBA8) and return `seq`.
    pub fn sample_grid(
        &self,
        cam_id: i64,
        grid_w: u32,
        grid_h: u32,
        mirror: bool,
        out_buf: &mut [u8],
    ) -> Option<u64> {
        let gw = grid_w.clamp(4, 512) as usize;
        let gh = grid_h.clamp(4, 512) as usize;
        let need = gw * gh * 4;
        if out_buf.len() < need {
            return None;
        }
        let sess = self.sessions.get(&cam_id)?;
        let mut guard = sess.shared.lock().ok()?;
        ensure_synthetic_fresh(&mut guard);
        let sw = guard.width as usize;
        let sh = guard.height as usize;
        let stride = sw * 4;
        if sw == 0 || sh == 0 || guard.rgba.len() < sh * stride {
            return None;
        }
        let rgba = &guard.rgba;

        for gy in 0..gh {
            let y0 = (gy * sh) / gh;
            let y1 = (((gy + 1) * sh) / gh).max(y0 + 1).min(sh);
            let span_y = y1 - y0;
            let sy_a = y0 + span_y / 4;
            let sy_b = (y0 + (span_y * 3) / 4).min(sh - 1);
            let row_a = sy_a * stride;
            let row_b = sy_b * stride;
            for gx in 0..gw {
                let src_gx = if mirror { gw - 1 - gx } else { gx };
                let x0 = (src_gx * sw) / gw;
                let x1 = (((src_gx + 1) * sw) / gw).max(x0 + 1).min(sw);
                let span_x = x1 - x0;
                let sx_a = x0 + span_x / 4;
                let sx_b = (x0 + (span_x * 3) / 4).min(sw - 1);
                let p00 = row_a + sx_a * 4;
                let p01 = row_a + sx_b * 4;
                let p10 = row_b + sx_a * 4;
                let p11 = row_b + sx_b * 4;
                let sr = rgba[p00] as u32 + rgba[p01] as u32 + rgba[p10] as u32 + rgba[p11] as u32;
                let sg = rgba[p00 + 1] as u32
                    + rgba[p01 + 1] as u32
                    + rgba[p10 + 1] as u32
                    + rgba[p11 + 1] as u32;
                let sb = rgba[p00 + 2] as u32
                    + rgba[p01 + 2] as u32
                    + rgba[p10 + 2] as u32
                    + rgba[p11 + 2] as u32;
                let dst = (gy * gw + gx) * 4;
                out_buf[dst] = (sr >> 2) as u8;
                out_buf[dst + 1] = (sg >> 2) as u8;
                out_buf[dst + 2] = (sb >> 2) as u8;
                out_buf[dst + 3] = 255;
            }
        }
        Some(guard.seq)
    }

    /// Copy the latest frame directly into a renderer scratch buffer with optional mirror + optical tint.
    pub fn write_tinted_premul(
        &self,
        cam_id: i64,
        mirror: bool,
        tint_mode: i32,
        out_premul: &mut Vec<u8>,
    ) -> Option<(u32, u32)> {
        let sess = self.sessions.get(&cam_id)?;
        let mut guard = sess.shared.lock().ok()?;
        ensure_synthetic_fresh(&mut guard);
        let w = guard.width;
        let h = guard.height;
        let sw = w as usize;
        let sh = h as usize;
        let total = sw * sh * 4;
        if w == 0 || h == 0 || guard.rgba.len() < total {
            return None;
        }
        out_premul.resize(total, 255);
        if !mirror && tint_mode == CAM_TINT_RGB {
            out_premul.copy_from_slice(&guard.rgba[..total]);
            return Some((w, h));
        }
        for y in 0..sh {
            let row_off = y * sw * 4;
            for x in 0..sw {
                let sx = if mirror { sw - 1 - x } else { x };
                let soff = row_off + sx * 4;
                let doff = row_off + x * 4;
                let [tr, tg, tb] = apply_sensor_tint(
                    guard.rgba[soff],
                    guard.rgba[soff + 1],
                    guard.rgba[soff + 2],
                    tint_mode,
                );
                out_premul[doff] = tr;
                out_premul[doff + 1] = tg;
                out_premul[doff + 2] = tb;
                out_premul[doff + 3] = 255;
            }
        }
        Some((w, h))
    }
}

const fn build_nvg_lut() -> [[u8; 3]; 256] {
    let mut table = [[0u8; 3]; 256];
    let mut i = 0usize;
    while i < 256 {
        let luma = i as u32;
        let raw_boost = luma * 115 / 100;
        let boost = if raw_boost > 255 { 255 } else { raw_boost } as u8;
        table[i] = [(luma * 16 / 100) as u8, boost, (luma * 42 / 100) as u8];
        i += 1;
    }
    table
}

const fn build_flir_lut() -> [[u8; 3]; 256] {
    let mut table = [[0u8; 3]; 256];
    let mut i = 0usize;
    while i < 256 {
        let luma = i as u32;
        table[i] = if luma < 72 {
            let t = luma * 255 / 72;
            [
                (12 + t * 28 / 255) as u8,
                (18 + t * 52 / 255) as u8,
                (48 + t * 120 / 255) as u8,
            ]
        } else if luma < 156 {
            let t = (luma - 72) * 255 / 84;
            [
                (40 + t * 185 / 255) as u8,
                (70 + t * 48 / 255) as u8,
                (168 - t * 120 / 255) as u8,
            ]
        } else {
            let t = (luma - 156) * 255 / 99;
            let r = 225 + t * 30 / 255;
            let g = 118 + t * 134 / 255;
            let b = 48 + t * 195 / 255;
            [
                if r > 255 { 255 } else { r } as u8,
                if g > 255 { 255 } else { g } as u8,
                if b > 255 { 255 } else { b } as u8,
            ]
        };
        i += 1;
    }
    table
}

const fn build_slate_lut() -> [[u8; 3]; 256] {
    let mut table = [[0u8; 3]; 256];
    let mut i = 0usize;
    while i < 256 {
        let luma = i as u32;
        table[i] = [(luma * 68 / 100) as u8, (luma * 86 / 100) as u8, i as u8];
        i += 1;
    }
    table
}

const NVG_LUT: [[u8; 3]; 256] = build_nvg_lut();
const FLIR_LUT: [[u8; 3]; 256] = build_flir_lut();
const SLATE_LUT: [[u8; 3]; 256] = build_slate_lut();

#[inline(always)]
pub(crate) fn apply_sensor_tint(r: u8, g: u8, b: u8, tint_mode: i32) -> [u8; 3] {
    match tint_mode {
        CAM_TINT_NVG => {
            let luma = ((r as usize * 77 + g as usize * 150 + b as usize * 29) >> 8) & 0xFF;
            NVG_LUT[luma]
        }
        CAM_TINT_FLIR => {
            let luma = ((r as usize * 77 + g as usize * 150 + b as usize * 29) >> 8) & 0xFF;
            FLIR_LUT[luma]
        }
        CAM_TINT_SLATE => {
            let luma = ((r as usize * 77 + g as usize * 150 + b as usize * 29) >> 8) & 0xFF;
            SLATE_LUT[luma]
        }
        _ => [r, g, b],
    }
}

fn ensure_synthetic_fresh(guard: &mut CameraSharedState) {
    if guard.mode == CAM_MODE_SYNTHETIC && guard.sim_dirty {
        let w = guard.width;
        let h = guard.height;
        let specs = guard.sim_targets;
        paint_synthetic_rgba_fast(
            w,
            h,
            &specs,
            &mut guard.sim_backdrop,
            &mut guard.sim_backdrop_dims,
            &mut guard.rgba,
        );
        guard.sim_dirty = false;
    }
}

fn render_synthetic_frame(shared: &Arc<Mutex<CameraSharedState>>, elapsed: f64) {
    let Ok(mut guard) = shared.lock() else {
        return;
    };
    let seq = guard.seq.wrapping_add(1);
    let w = guard.width;
    let h = guard.height;
    let specs = guard.sim_targets;
    let mut anim_specs = specs;
    for (idx, t) in anim_specs.iter_mut().enumerate() {
        if t.alive && t.radius <= 0.001 {
            let phase = elapsed * 0.8 + (idx as f64) * 1.4;
            t.nx = 0.5 + 0.25 * (phase.cos() as f32);
            t.ny = 0.5 + 0.18 * ((phase * 1.3).sin() as f32);
            t.radius = 0.05;
        }
    }
    let g = &mut *guard;
    paint_synthetic_rgba_fast(
        w,
        h,
        &anim_specs,
        &mut g.sim_backdrop,
        &mut g.sim_backdrop_dims,
        &mut g.rgba,
    );
    g.sim_dirty = false;
    g.seq = seq;
}

fn paint_synthetic_rgba_fast(
    w: u32,
    h: u32,
    targets: &[SimTargetSpec; MAX_SIM_TARGETS],
    backdrop: &mut Vec<u8>,
    backdrop_dims: &mut (u32, u32),
    out: &mut Vec<u8>,
) {
    let sw = w as usize;
    let sh = h as usize;
    let need = sw * sh * 4;
    if *backdrop_dims != (w, h) || backdrop.len() != need {
        backdrop.resize(need, 255);
        let hf = h as f32;
        for y in 0..sh {
            let yf = (y as f32) / hf;
            let scan = if y % 4 == 0 { 4u8 } else { 0u8 };
            let grid_y = y % 45 == 0;
            let row_off = y * sw * 4;
            for x in 0..sw {
                let hash =
                    ((x as u64).wrapping_mul(73_856_093) ^ (y as u64).wrapping_mul(19_349_663)) & 3;
                let grid_hit = grid_y || x % 64 == 0;
                let off = row_off + x * 4;
                backdrop[off] = 14 + scan + (hash as u8) + if grid_hit { 8 } else { 0 };
                backdrop[off + 1] =
                    22 + ((yf * 16.0) as u8) + scan + (hash as u8) + if grid_hit { 12 } else { 0 };
                backdrop[off + 2] =
                    34 + ((yf * 24.0) as u8) + scan + (hash as u8) + if grid_hit { 18 } else { 0 };
                backdrop[off + 3] = 255;
            }
        }
        *backdrop_dims = (w, h);
    }

    if out.len() != need {
        out.resize(need, 255);
    }
    out.copy_from_slice(backdrop);
    let wf = w as f32;
    let hf = h as f32;

    // Draw high-contrast target silhouettes & hit flash explosions for optical/motion tracking.
    for spec in targets {
        if !spec.alive && spec.hit_flash == 0 {
            continue;
        }
        let cx = spec.nx.clamp(0.05, 0.95) * wf;
        let cy = spec.ny.clamp(0.08, 0.92) * hf;
        let rad_px = (spec.radius.clamp(0.02, 0.25) * hf).max(8.0);
        let max_r = if spec.hit_flash > 0 {
            rad_px * 1.9
        } else {
            rad_px * 1.35
        };
        let min_x = (cx - max_r).floor().max(0.0) as usize;
        let max_x = ((cx + max_r).ceil() as usize + 1).min(sw);
        let min_y = (cy - max_r).floor().max(0.0) as usize;
        let max_y = ((cy + max_r).ceil() as usize + 1).min(sh);

        for y in min_y..max_y {
            let dy = (y as f32) - cy;
            let row_off = y * sw * 4;
            for x in min_x..max_x {
                let dx = (x as f32) - cx;
                let dist = (dx * dx + dy * dy).sqrt();
                let off = row_off + x * 4;
                if spec.hit_flash > 0 && dist <= rad_px * 1.8 {
                    let fade = (1.0 - dist / (rad_px * 1.8)).clamp(0.0, 1.0);
                    let intensity = ((spec.hit_flash as f32) / 255.0) * fade;
                    out[off] = (out[off] as f32 + 255.0 * intensity).min(255.0) as u8;
                    out[off + 1] = (out[off + 1] as f32 + 195.0 * intensity).min(255.0) as u8;
                    out[off + 2] = (out[off + 2] as f32 + 95.0 * intensity).min(255.0) as u8;
                }
                if spec.alive {
                    if dist <= rad_px * 0.42 {
                        // Hot engine/core signature
                        out[off] = 250;
                        out[off + 1] = 110;
                        out[off + 2] = 68;
                    } else if dist <= rad_px {
                        // Airframe body halo + lateral wings
                        let body = (1.0 - (dist / rad_px)).clamp(0.0, 1.0);
                        out[off] = (120.0 + 125.0 * body).min(255.0) as u8;
                        out[off + 1] = (200.0 + 50.0 * body).min(255.0) as u8;
                        out[off + 2] = (240.0 + 15.0 * body).min(255.0) as u8;
                    } else if dy.abs() <= rad_px * 0.22 && dx.abs() <= rad_px * 1.32 {
                        // Horizontal drone rotors/wings
                        out[off] = 180;
                        out[off + 1] = 235;
                        out[off + 2] = 255;
                    }
                }
            }
        }
    }
}

fn run_camera_worker(
    device_index: i64,
    req_w: u32,
    req_h: u32,
    synthetic_only: bool,
    shared: Arc<Mutex<CameraSharedState>>,
    stop: Arc<AtomicBool>,
    ready_tx: std::sync::mpsc::SyncSender<()>,
) {
    #[cfg(target_os = "windows")]
    let mut signaled = false;

    #[cfg(target_os = "windows")]
    if !synthetic_only
        && win_mf::capture_loop(device_index, req_w, req_h, &shared, &stop, || {
            if !signaled {
                signaled = true;
                let _ = ready_tx.try_send(());
            }
        })
        .is_ok()
    {
        return;
    }

    #[cfg(not(target_os = "windows"))]
    let _ = (device_index, req_w, req_h, synthetic_only);

    let _ = ready_tx.try_send(());
    let start = Instant::now();
    while !stop.load(Ordering::Acquire) {
        let external = shared.lock().map(|g| g.sim_external).unwrap_or(false);
        if !external {
            render_synthetic_frame(&shared, start.elapsed().as_secs_f64());
        }
        thread::sleep(Duration::from_millis(33));
    }
}

#[cfg(target_os = "windows")]
mod win_mf {
    use std::ffi::c_void;
    use std::ptr;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use super::{CameraSharedState, CAM_MODE_LIVE};

    type Hresult = i32;

    #[repr(C)]
    #[derive(Clone, Copy, PartialEq, Eq)]
    struct Guid {
        d1: u32,
        d2: u16,
        d3: u16,
        d4: [u8; 8],
    }

    const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> Guid {
        Guid { d1, d2, d3, d4 }
    }

    const COINIT_MULTITHREADED: u32 = 0x0;
    const S_OK: Hresult = 0;
    const S_FALSE: Hresult = 1;
    const RPC_E_CHANGED_MODE: Hresult = -2147417850; // 0x80010106

    const MF_SDK_VERSION: u32 = 0x2;
    const MF_API_VERSION: u32 = 0x0070;
    const MF_VERSION: u32 = (MF_SDK_VERSION << 16) | MF_API_VERSION;
    const MFSTARTUP_NOSOCKET: u32 = 1;
    const MF_SOURCE_READER_FIRST_VIDEO_STREAM: u32 = 0xFFFF_FFFC;
    const MF_SOURCE_READERF_ERROR: u32 = 0x0000_0001;
    const MF_SOURCE_READERF_ENDOFSTREAM: u32 = 0x0000_0002;
    const MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED: u32 = 0x0000_0020;

    // Exact Media Foundation GUIDs verified against UCRT64 mfapi.h / mfidl.h / mfreadwrite.h.
    const MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE: Guid = guid(
        0xc60a_c5fe,
        0x252a,
        0x478f,
        [0xa0, 0xef, 0xbc, 0x8f, 0xa5, 0xf7, 0xca, 0xd3],
    );
    const MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID: Guid = guid(
        0x8ac3_587a,
        0x4ae7,
        0x42d8,
        [0x99, 0xe0, 0x0a, 0x60, 0x13, 0xee, 0xf9, 0x0f],
    );
    const MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME: Guid = guid(
        0x60d0_e559,
        0x52f8,
        0x4fa2,
        [0xbb, 0xce, 0xac, 0xdb, 0x34, 0xa8, 0xec, 0x01],
    );
    const IID_IMF_MEDIA_SOURCE: Guid = guid(
        0x279a_808d,
        0xaec7,
        0x40c8,
        [0x9c, 0x6b, 0xa6, 0xb4, 0x92, 0xc7, 0x8a, 0x66],
    );
    const MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING: Guid = guid(
        0xfb39_4f3d,
        0xccf1,
        0x42ee,
        [0xbb, 0xb3, 0xf9, 0xb8, 0x45, 0xd5, 0x68, 0x1d],
    );
    const MF_MT_MAJOR_TYPE: Guid = guid(
        0x48eb_a18e,
        0xf8c9,
        0x4687,
        [0xbf, 0x11, 0x0a, 0x74, 0xc9, 0xf9, 0x6a, 0x8f],
    );
    const MF_MT_SUBTYPE: Guid = guid(
        0xf7e3_4c9a,
        0x42e8,
        0x4714,
        [0xb7, 0x4b, 0xcb, 0x29, 0xd7, 0x2c, 0x35, 0xe5],
    );
    const MF_MT_FRAME_SIZE: Guid = guid(
        0x1652_c33d,
        0xd6b2,
        0x4012,
        [0xb8, 0x34, 0x72, 0x03, 0x08, 0x49, 0xa3, 0x7d],
    );
    const MF_MT_DEFAULT_STRIDE: Guid = guid(
        0x644b_4e48,
        0x1e02,
        0x4516,
        [0xb0, 0xeb, 0xc0, 0x1c, 0xa9, 0xd4, 0x9a, 0xc6],
    );
    const MF_MEDIA_TYPE_VIDEO: Guid = guid(
        0x7364_6976,
        0x0000,
        0x0010,
        [0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71],
    );
    const MF_VIDEO_FORMAT_RGB32: Guid = guid(
        22,
        0x0000,
        0x0010,
        [0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71],
    );
    const MF_VIDEO_FORMAT_YUY2: Guid = guid(
        u32::from_le_bytes(*b"YUY2"),
        0x0000,
        0x0010,
        [0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71],
    );
    const MF_VIDEO_FORMAT_NV12: Guid = guid(
        u32::from_le_bytes(*b"NV12"),
        0x0000,
        0x0010,
        [0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71],
    );

    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryA(lp_lib_file_name: *const u8) -> *mut c_void;
        fn GetProcAddress(h_module: *mut c_void, lp_proc_name: *const u8) -> *const c_void;
        fn FreeLibrary(h_lib_module: *mut c_void) -> i32;
    }

    struct LibGuard(*mut c_void);

    impl LibGuard {
        fn open(name: &[u8]) -> Option<Self> {
            // SAFETY: Caller passes NUL-terminated ASCII DLL name.
            let h = unsafe { LoadLibraryA(name.as_ptr()) };
            if h.is_null() {
                None
            } else {
                Some(Self(h))
            }
        }

        fn sym(&self, proc_name: &[u8]) -> Option<*const c_void> {
            // SAFETY: `self.0` is a valid module handle and `proc_name` is NUL-terminated ASCII.
            let p = unsafe { GetProcAddress(self.0, proc_name.as_ptr()) };
            if p.is_null() {
                None
            } else {
                Some(p)
            }
        }
    }

    impl Drop for LibGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: Paired with a successful `LoadLibraryA`.
                unsafe {
                    let _ = FreeLibrary(self.0);
                }
            }
        }
    }

    struct ComGuard(bool);

    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.0 {
                // SAFETY: Resolved via live ole32 or skipped on RPC_E_CHANGED_MODE.
                if let Some(ole) = LibGuard::open(b"ole32.dll\0") {
                    if let Some(p) = ole.sym(b"CoUninitialize\0") {
                        let f: unsafe extern "system" fn() = unsafe { std::mem::transmute(p) };
                        unsafe { f() };
                    }
                }
            }
        }
    }

    struct MfGuard {
        shutdown: unsafe extern "system" fn() -> Hresult,
        active: bool,
    }

    impl Drop for MfGuard {
        fn drop(&mut self) {
            if self.active {
                // SAFETY: Paired with a successful `MFStartup` while `mfplat.dll` stays loaded.
                unsafe {
                    let _ = (self.shutdown)();
                }
            }
        }
    }

    struct ComPtr(*mut c_void);

    impl ComPtr {
        fn as_ptr(&self) -> *mut c_void {
            self.0
        }

        unsafe fn vtable(&self) -> *const *const c_void {
            // SAFETY: Valid COM object begins with a pointer to its vtable.
            unsafe { *(self.0 as *const *const *const c_void) }
        }
    }

    impl Drop for ComPtr {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: Every `IUnknown` COM pointer puts `Release` at vtable slot 2.
                unsafe {
                    let release: unsafe extern "system" fn(*mut c_void) -> u32 =
                        std::mem::transmute(*self.vtable().add(2));
                    let _ = release(self.0);
                }
                self.0 = ptr::null_mut();
            }
        }
    }

    struct CoTaskMemGuard(*mut c_void, unsafe extern "system" fn(*mut c_void));

    impl Drop for CoTaskMemGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: Pointer returned by COM allocator (`CoTaskMemAlloc`).
                unsafe { (self.1)(self.0) };
                self.0 = ptr::null_mut();
            }
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum PixelFmt {
        Rgb32,
        Yuy2,
        Nv12,
    }

    struct FormatSpec {
        w: u32,
        h: u32,
        stride: i32,
        fmt: PixelFmt,
    }

    pub fn capture_loop<F: FnMut()>(
        device_index: i64,
        req_w: u32,
        req_h: u32,
        shared: &Arc<Mutex<CameraSharedState>>,
        stop: &Arc<AtomicBool>,
        mut on_live: F,
    ) -> Result<(), String> {
        let ole32 = LibGuard::open(b"ole32.dll\0").ok_or("ole32.dll missing")?;
        let mfplat = LibGuard::open(b"mfplat.dll\0").ok_or("mfplat.dll missing")?;
        let mf = LibGuard::open(b"mf.dll\0").ok_or("mf.dll missing")?;
        let mfreadwrite = LibGuard::open(b"mfreadwrite.dll\0").ok_or("mfreadwrite.dll missing")?;

        let co_init: unsafe extern "system" fn(*mut c_void, u32) -> Hresult =
            unsafe { std::mem::transmute(ole32.sym(b"CoInitializeEx\0").ok_or("CoInitializeEx")?) };
        let co_task_free: unsafe extern "system" fn(*mut c_void) =
            unsafe { std::mem::transmute(ole32.sym(b"CoTaskMemFree\0").ok_or("CoTaskMemFree")?) };

        let mf_startup: unsafe extern "system" fn(u32, u32) -> Hresult =
            unsafe { std::mem::transmute(mfplat.sym(b"MFStartup\0").ok_or("MFStartup")?) };
        let mf_shutdown: unsafe extern "system" fn() -> Hresult =
            unsafe { std::mem::transmute(mfplat.sym(b"MFShutdown\0").ok_or("MFShutdown")?) };
        let mf_create_attrs: unsafe extern "system" fn(*mut *mut c_void, u32) -> Hresult = unsafe {
            std::mem::transmute(
                mfplat
                    .sym(b"MFCreateAttributes\0")
                    .ok_or("MFCreateAttributes")?,
            )
        };
        let mf_create_mt: unsafe extern "system" fn(*mut *mut c_void) -> Hresult = unsafe {
            std::mem::transmute(
                mfplat
                    .sym(b"MFCreateMediaType\0")
                    .ok_or("MFCreateMediaType")?,
            )
        };
        let mf_enum_sources: unsafe extern "system" fn(
            *mut c_void,
            *mut *mut *mut c_void,
            *mut u32,
        ) -> Hresult = unsafe {
            std::mem::transmute(
                mf.sym(b"MFEnumDeviceSources\0")
                    .ok_or("MFEnumDeviceSources")?,
            )
        };
        let mf_create_reader: unsafe extern "system" fn(
            *mut c_void,
            *mut c_void,
            *mut *mut c_void,
        ) -> Hresult = unsafe {
            std::mem::transmute(
                mfreadwrite
                    .sym(b"MFCreateSourceReaderFromMediaSource\0")
                    .ok_or("MFCreateSourceReaderFromMediaSource")?,
            )
        };

        // SAFETY: Initializing COM on this capture worker thread.
        let co_hr = unsafe { co_init(ptr::null_mut(), COINIT_MULTITHREADED) };
        let _com_guard = if co_hr == S_OK || co_hr == S_FALSE {
            ComGuard(true)
        } else if co_hr == RPC_E_CHANGED_MODE {
            ComGuard(false)
        } else {
            return Err(format!("CoInitializeEx failed: 0x{:08x}", co_hr as u32));
        };

        // SAFETY: Start Windows Media Foundation after COM init.
        let mf_hr = unsafe { mf_startup(MF_VERSION, MFSTARTUP_NOSOCKET) };
        if mf_hr < 0 {
            return Err(format!("MFStartup failed: 0x{:08x}", mf_hr as u32));
        }
        let _mf_guard = MfGuard {
            shutdown: mf_shutdown,
            active: true,
        };

        // Enumerate video capture devices.
        let mut enum_raw = ptr::null_mut();
        let hr = unsafe { mf_create_attrs(&mut enum_raw, 1) };
        if hr < 0 || enum_raw.is_null() {
            return Err("MFCreateAttributes failed".to_owned());
        }
        let enum_attrs = ComPtr(enum_raw);
        unsafe {
            let set_guid: unsafe extern "system" fn(
                *mut c_void,
                *const Guid,
                *const Guid,
            ) -> Hresult = std::mem::transmute(*enum_attrs.vtable().add(24));
            let hr = set_guid(
                enum_attrs.as_ptr(),
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
            );
            if hr < 0 {
                return Err(format!("SetGUID vidcap failed: 0x{:08x}", hr as u32));
            }
        }

        let mut raw_list: *mut *mut c_void = ptr::null_mut();
        let mut count: u32 = 0;
        let hr = unsafe { mf_enum_sources(enum_attrs.as_ptr(), &mut raw_list, &mut count) };
        if hr < 0 || count == 0 || raw_list.is_null() {
            if !raw_list.is_null() {
                unsafe { co_task_free(raw_list as *mut c_void) };
            }
            return Err("no video capture devices found".to_owned());
        }

        // Own every IMFActivate pointer in `activates` so none leak on early exit.
        let mut activates = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            activates.push(ComPtr(unsafe { *raw_list.add(i) }));
        }
        unsafe { co_task_free(raw_list as *mut c_void) };

        let chosen_idx = device_index.clamp(0, (activates.len() - 1) as i64) as usize;
        let activate = &activates[chosen_idx];
        let device_name = read_friendly_name(activate, co_task_free);

        let mut source_raw = ptr::null_mut();
        let hr = unsafe {
            let act_obj: unsafe extern "system" fn(
                *mut c_void,
                *const Guid,
                *mut *mut c_void,
            ) -> Hresult = std::mem::transmute(*activate.vtable().add(33));
            act_obj(activate.as_ptr(), &IID_IMF_MEDIA_SOURCE, &mut source_raw)
        };
        if hr < 0 || source_raw.is_null() {
            return Err(format!("ActivateObject failed: 0x{:08x}", hr as u32));
        }
        let media_source = ComPtr(source_raw);

        // Create source reader with video processing enabled so MF handles YUV->RGB32 conversion cleanly.
        let mut rd_attr_raw = ptr::null_mut();
        let hr = unsafe { mf_create_attrs(&mut rd_attr_raw, 1) };
        if hr < 0 || rd_attr_raw.is_null() {
            return Err("MFCreateAttributes reader failed".to_owned());
        }
        let reader_attrs = ComPtr(rd_attr_raw);
        unsafe {
            let set_u32: unsafe extern "system" fn(*mut c_void, *const Guid, u32) -> Hresult =
                std::mem::transmute(*reader_attrs.vtable().add(21));
            let _ = set_u32(
                reader_attrs.as_ptr(),
                &MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING,
                1,
            );
        }

        let mut reader_raw = ptr::null_mut();
        let hr = unsafe {
            mf_create_reader(
                media_source.as_ptr(),
                reader_attrs.as_ptr(),
                &mut reader_raw,
            )
        };
        if hr < 0 || reader_raw.is_null() {
            return Err(format!(
                "MFCreateSourceReaderFromMediaSource failed: 0x{:08x}",
                hr as u32
            ));
        }
        let reader = ComPtr(reader_raw);

        let mut spec = negotiate_media_type(&reader, mf_create_mt, req_w, req_h)?;
        let (init_w, init_h) = if spec.w >= 960 && spec.h >= 540 {
            (spec.w / 2, spec.h / 2)
        } else {
            (spec.w, spec.h)
        };
        {
            if let Ok(mut guard) = shared.lock() {
                guard.width = init_w;
                guard.height = init_h;
                guard.mode = CAM_MODE_LIVE;
                guard.device_name = device_name;
                guard
                    .rgba
                    .resize((init_w as usize) * (init_h as usize) * 4, 255);
            }
        }

        let read_sample: unsafe extern "system" fn(
            *mut c_void,
            u32,
            u32,
            *mut u32,
            *mut u32,
            *mut i64,
            *mut *mut c_void,
        ) -> Hresult = unsafe { std::mem::transmute(*reader.vtable().add(9)) };

        let mut staging: Vec<u8> = Vec::with_capacity((init_w as usize) * (init_h as usize) * 4);
        let mut got_first = false;
        let mut null_spins = 0u32;
        while !stop.load(Ordering::Acquire) {
            let mut actual_idx = 0u32;
            let mut stream_flags = 0u32;
            let mut timestamp = 0i64;
            let mut sample_raw = ptr::null_mut();
            let hr = unsafe {
                read_sample(
                    reader.as_ptr(),
                    MF_SOURCE_READER_FIRST_VIDEO_STREAM,
                    0,
                    &mut actual_idx,
                    &mut stream_flags,
                    &mut timestamp,
                    &mut sample_raw,
                )
            };
            if hr < 0
                || (stream_flags & (MF_SOURCE_READERF_ERROR | MF_SOURCE_READERF_ENDOFSTREAM)) != 0
            {
                if !got_first {
                    return Err(format!("ReadSample stream failure: 0x{:08x}", hr as u32));
                }
                break;
            }
            if (stream_flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED) != 0 {
                if let Ok(updated) = read_current_format(&reader) {
                    spec = updated;
                }
            }
            if sample_raw.is_null() {
                null_spins += 1;
                if !got_first && null_spins > 60 {
                    return Err("timed out waiting for first webcam frame".to_owned());
                }
                thread::sleep(Duration::from_millis(8));
                continue;
            }
            let sample = ComPtr(sample_raw);
            if let Ok((out_w, out_h)) = decode_sample_to_buf(&sample, &spec, &mut staging) {
                if let Ok(mut guard) = shared.lock() {
                    guard.width = out_w;
                    guard.height = out_h;
                    std::mem::swap(&mut guard.rgba, &mut staging);
                    guard.seq = guard.seq.wrapping_add(1);
                }
                if !got_first {
                    got_first = true;
                    on_live();
                }
            }
        }
        on_live();
        Ok(())
    }

    fn read_friendly_name(
        activate: &ComPtr,
        co_task_free: unsafe extern "system" fn(*mut c_void),
    ) -> String {
        let mut wide_ptr: *mut u16 = ptr::null_mut();
        let mut wide_len: u32 = 0;
        let hr = unsafe {
            let get_alloc_str: unsafe extern "system" fn(
                *mut c_void,
                *const Guid,
                *mut *mut u16,
                *mut u32,
            ) -> Hresult = std::mem::transmute(*activate.vtable().add(13));
            get_alloc_str(
                activate.as_ptr(),
                &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME,
                &mut wide_ptr,
                &mut wide_len,
            )
        };
        if hr >= 0 && !wide_ptr.is_null() && wide_len > 0 {
            let _guard = CoTaskMemGuard(wide_ptr as *mut c_void, co_task_free);
            // SAFETY: COM returned valid UTF-16 slice of `wide_len` u16 code units.
            let slice = unsafe { std::slice::from_raw_parts(wide_ptr, wide_len as usize) };
            let name = String::from_utf16_lossy(slice)
                .trim_end_matches('\0')
                .to_owned();
            if !name.is_empty() {
                return name;
            }
        }
        "Laptop Camera".to_owned()
    }

    fn negotiate_media_type(
        reader: &ComPtr,
        mf_create_mt: unsafe extern "system" fn(*mut *mut c_void) -> Hresult,
        req_w: u32,
        req_h: u32,
    ) -> Result<FormatSpec, String> {
        let set_current: unsafe extern "system" fn(
            *mut c_void,
            u32,
            *mut u32,
            *mut c_void,
        ) -> Hresult = unsafe { std::mem::transmute(*reader.vtable().add(7)) };

        let candidates = [
            MF_VIDEO_FORMAT_RGB32,
            MF_VIDEO_FORMAT_NV12,
            MF_VIDEO_FORMAT_YUY2,
        ];
        let mut size_attempts: Vec<Option<(u32, u32)>> = Vec::with_capacity(4);
        size_attempts.push(Some((req_w, req_h)));
        if (req_w, req_h) != (640, 480) {
            size_attempts.push(Some((640, 480)));
        }
        if (req_w, req_h) != (640, 360) {
            size_attempts.push(Some((640, 360)));
        }
        size_attempts.push(None);

        for sz in size_attempts {
            for sub in candidates {
                let mut mt_raw = ptr::null_mut();
                let hr = unsafe { mf_create_mt(&mut mt_raw) };
                if hr < 0 || mt_raw.is_null() {
                    continue;
                }
                let mt = ComPtr(mt_raw);
                let ok_set = unsafe {
                    let set_guid: unsafe extern "system" fn(
                        *mut c_void,
                        *const Guid,
                        *const Guid,
                    ) -> Hresult = std::mem::transmute(*mt.vtable().add(24));
                    let set_u64: unsafe extern "system" fn(
                        *mut c_void,
                        *const Guid,
                        u64,
                    ) -> Hresult = std::mem::transmute(*mt.vtable().add(22));
                    let mut ok = set_guid(mt.as_ptr(), &MF_MT_MAJOR_TYPE, &MF_MEDIA_TYPE_VIDEO)
                        >= 0
                        && set_guid(mt.as_ptr(), &MF_MT_SUBTYPE, &sub) >= 0;
                    if ok {
                        if let Some((w, h)) = sz {
                            let packed = ((w as u64) << 32) | (h as u64);
                            ok = set_u64(mt.as_ptr(), &MF_MT_FRAME_SIZE, packed) >= 0;
                        }
                    }
                    ok
                };
                if ok_set
                    && unsafe {
                        set_current(
                            reader.as_ptr(),
                            MF_SOURCE_READER_FIRST_VIDEO_STREAM,
                            ptr::null_mut(),
                            mt.as_ptr(),
                        )
                    } >= 0
                {
                    if let Ok(spec) = read_current_format(reader) {
                        return Ok(spec);
                    }
                }
            }
        }
        Err("failed to negotiate RGB32/NV12/YUY2 stream type".to_owned())
    }

    fn read_current_format(reader: &ComPtr) -> Result<FormatSpec, String> {
        let mut cur_raw = ptr::null_mut();
        let hr = unsafe {
            let get_current: unsafe extern "system" fn(
                *mut c_void,
                u32,
                *mut *mut c_void,
            ) -> Hresult = std::mem::transmute(*reader.vtable().add(6));
            get_current(
                reader.as_ptr(),
                MF_SOURCE_READER_FIRST_VIDEO_STREAM,
                &mut cur_raw,
            )
        };
        if hr < 0 || cur_raw.is_null() {
            return Err("GetCurrentMediaType failed".to_owned());
        }
        let mt = ComPtr(cur_raw);
        let mut frame_size: u64 = 0;
        let hr = unsafe {
            let get_u64: unsafe extern "system" fn(*mut c_void, *const Guid, *mut u64) -> Hresult =
                std::mem::transmute(*mt.vtable().add(8));
            get_u64(mt.as_ptr(), &MF_MT_FRAME_SIZE, &mut frame_size)
        };
        if hr < 0 {
            return Err("MF_MT_FRAME_SIZE unavailable".to_owned());
        }
        let w = (frame_size >> 32) as u32;
        let h = (frame_size & 0xFFFF_FFFF) as u32;
        if w == 0 || h == 0 {
            return Err("invalid zero camera dimensions".to_owned());
        }

        let mut subtype = Guid {
            d1: 0,
            d2: 0,
            d3: 0,
            d4: [0; 8],
        };
        unsafe {
            let get_guid: unsafe extern "system" fn(
                *mut c_void,
                *const Guid,
                *mut Guid,
            ) -> Hresult = std::mem::transmute(*mt.vtable().add(10));
            let _ = get_guid(mt.as_ptr(), &MF_MT_SUBTYPE, &mut subtype);
        }

        let fmt = if subtype == MF_VIDEO_FORMAT_YUY2 {
            PixelFmt::Yuy2
        } else if subtype == MF_VIDEO_FORMAT_NV12 {
            PixelFmt::Nv12
        } else {
            PixelFmt::Rgb32
        };

        let mut stride_u32 = 0u32;
        let stride_hr = unsafe {
            let get_u32: unsafe extern "system" fn(*mut c_void, *const Guid, *mut u32) -> Hresult =
                std::mem::transmute(*mt.vtable().add(7));
            get_u32(mt.as_ptr(), &MF_MT_DEFAULT_STRIDE, &mut stride_u32)
        };
        let stride = if stride_hr >= 0 && stride_u32 != 0 {
            stride_u32 as i32
        } else {
            match fmt {
                PixelFmt::Rgb32 => (w * 4) as i32,
                PixelFmt::Yuy2 => (w * 2) as i32,
                PixelFmt::Nv12 => w as i32,
            }
        };

        Ok(FormatSpec { w, h, stride, fmt })
    }

    fn decode_sample_to_buf(
        sample: &ComPtr,
        spec: &FormatSpec,
        out_rgba: &mut Vec<u8>,
    ) -> Result<(u32, u32), String> {
        let mut buf_raw = ptr::null_mut();
        let hr = unsafe {
            let convert: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> Hresult =
                std::mem::transmute(*sample.vtable().add(41));
            convert(sample.as_ptr(), &mut buf_raw)
        };
        if hr < 0 || buf_raw.is_null() {
            return Err("ConvertToContiguousBuffer failed".to_owned());
        }
        let media_buf = ComPtr(buf_raw);

        struct UnlockGuard<'a>(&'a ComPtr, bool);
        impl<'a> Drop for UnlockGuard<'a> {
            fn drop(&mut self) {
                if self.1 {
                    unsafe {
                        let unlock: unsafe extern "system" fn(*mut c_void) -> Hresult =
                            std::mem::transmute(*self.0.vtable().add(4));
                        let _ = unlock(self.0.as_ptr());
                    }
                }
            }
        }

        let mut data_ptr: *mut u8 = ptr::null_mut();
        let mut max_len: u32 = 0;
        let mut cur_len: u32 = 0;
        let hr = unsafe {
            let lock: unsafe extern "system" fn(
                *mut c_void,
                *mut *mut u8,
                *mut u32,
                *mut u32,
            ) -> Hresult = std::mem::transmute(*media_buf.vtable().add(3));
            lock(
                media_buf.as_ptr(),
                &mut data_ptr,
                &mut max_len,
                &mut cur_len,
            )
        };
        if hr < 0 || data_ptr.is_null() || cur_len == 0 {
            return Err("IMFMediaBuffer::Lock failed".to_owned());
        }
        let _unlock = UnlockGuard(&media_buf, true);

        // SAFETY: Buffer is locked through `_unlock` across the entire read below.
        let src = unsafe { std::slice::from_raw_parts(data_ptr, cur_len as usize) };
        let sw = spec.w as usize;
        let sh = spec.h as usize;
        let step = if sw >= 960 && sh >= 540 { 2 } else { 1 };
        let ow = sw / step;
        let oh = sh / step;
        let need = ow * oh * 4;
        if out_rgba.len() != need {
            out_rgba.resize(need, 255);
        }

        let abs_stride = spec.stride.unsigned_abs() as usize;
        let bottom_up = spec.stride < 0;

        match spec.fmt {
            PixelFmt::Rgb32 => {
                let row_bytes = abs_stride.max(sw * 4);
                for dy in 0..oh {
                    let sy = dy * step;
                    let src_y = if bottom_up { sh - 1 - sy } else { sy };
                    let row_start = src_y * row_bytes;
                    if row_start + sw * 4 > src.len() {
                        break;
                    }
                    let dst_start = dy * ow * 4;
                    for dx in 0..ow {
                        let sp = row_start + (dx * step) * 4;
                        let dp = dst_start + dx * 4;
                        // MFVideoFormat_RGB32 memory layout is BGRX in little-endian.
                        out_rgba[dp] = src[sp + 2];
                        out_rgba[dp + 1] = src[sp + 1];
                        out_rgba[dp + 2] = src[sp];
                        out_rgba[dp + 3] = 255;
                    }
                }
            }
            PixelFmt::Yuy2 => {
                let row_bytes = abs_stride.max(sw * 2);
                for dy in 0..oh {
                    let row_start = (dy * step) * row_bytes;
                    if row_start + sw * 2 > src.len() {
                        break;
                    }
                    let dst_start = dy * ow * 4;
                    if step == 1 {
                        let mut x = 0;
                        while x + 1 < ow {
                            let sp = row_start + x * 2;
                            let y0 = src[sp] as i32;
                            let u = src[sp + 1] as i32;
                            let y1 = src[sp + 2] as i32;
                            let v = src[sp + 3] as i32;
                            let [r0, g0, b0] = yuv_to_rgb(y0, u, v);
                            let [r1, g1, b1] = yuv_to_rgb(y1, u, v);
                            let dp0 = dst_start + x * 4;
                            out_rgba[dp0] = r0;
                            out_rgba[dp0 + 1] = g0;
                            out_rgba[dp0 + 2] = b0;
                            out_rgba[dp0 + 3] = 255;
                            let dp1 = dp0 + 4;
                            out_rgba[dp1] = r1;
                            out_rgba[dp1 + 1] = g1;
                            out_rgba[dp1 + 2] = b1;
                            out_rgba[dp1 + 3] = 255;
                            x += 2;
                        }
                    } else {
                        for dx in 0..ow {
                            let sp = row_start + dx * 4;
                            let [r, g, b] =
                                yuv_to_rgb(src[sp] as i32, src[sp + 1] as i32, src[sp + 3] as i32);
                            let dp = dst_start + dx * 4;
                            out_rgba[dp] = r;
                            out_rgba[dp + 1] = g;
                            out_rgba[dp + 2] = b;
                            out_rgba[dp + 3] = 255;
                        }
                    }
                }
            }
            PixelFmt::Nv12 => {
                let row_bytes = abs_stride.max(sw);
                let uv_base = row_bytes * sh;
                for dy in 0..oh {
                    let sy = dy * step;
                    let y_row = sy * row_bytes;
                    let uv_row = uv_base + (sy / 2) * row_bytes;
                    if y_row + sw > src.len() || uv_row + sw > src.len() {
                        break;
                    }
                    let dst_start = dy * ow * 4;
                    for dx in 0..ow {
                        let sx = dx * step;
                        let yv = src[y_row + sx] as i32;
                        let uv_off = uv_row + (sx & !1);
                        let u = src[uv_off] as i32;
                        let v = src[uv_off + 1] as i32;
                        let [r, g, b] = yuv_to_rgb(yv, u, v);
                        let dp = dst_start + dx * 4;
                        out_rgba[dp] = r;
                        out_rgba[dp + 1] = g;
                        out_rgba[dp + 2] = b;
                        out_rgba[dp + 3] = 255;
                    }
                }
            }
        }
        Ok((ow as u32, oh as u32))
    }

    #[inline]
    fn yuv_to_rgb(y: i32, u: i32, v: i32) -> [u8; 3] {
        let c = (y - 16).max(0);
        let d = u - 128;
        let e = v - 128;
        let r = ((298 * c + 409 * e + 128) >> 8).clamp(0, 255) as u8;
        let g = ((298 * c - 100 * d - 208 * e + 128) >> 8).clamp(0, 255) as u8;
        let b = ((298 * c + 516 * d + 128) >> 8).clamp(0, 255) as u8;
        [r, g, b]
    }
}
