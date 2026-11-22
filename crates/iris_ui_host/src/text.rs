use std::collections::HashMap;

use cosmic_text::{
    Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Style, SwashCache, SwashContent, Weight,
    Wrap,
};

use crate::protocol::{MeasureTextRequest, RgbaColor, TextAlign, TextFlags};

const TEXT_CACHE_MAX_ENTRIES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextMetrics {
    pub width: f32,
    pub height: f32,
    pub lines: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelClip {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl PixelClip {
    pub fn full(w: i32, h: i32) -> Self {
        Self {
            x0: 0,
            y0: 0,
            x1: w.max(0),
            y1: h.max(0),
        }
    }

    pub fn intersect(self, other: Self) -> Self {
        let x0 = self.x0.max(other.x0);
        let y0 = self.y0.max(other.y0);
        let x1 = self.x1.min(other.x1).max(x0);
        let y1 = self.y1.min(other.y1).max(y0);
        Self { x0, y0, x1, y1 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TextCacheKey {
    text: String,
    px_size_bits: u32,
    px_lh_bits: u32,
    max_w_bits: u32,
    flags_bits: i32,
}

struct CachedBuffer {
    buffer: Buffer,
    width: f32,
    height: f32,
    lines: i32,
    last_used: u64,
}

pub struct TextEngine {
    font_system: FontSystem,
    swash_cache: SwashCache,
    cache: HashMap<TextCacheKey, CachedBuffer>,
    clock: u64,
}

impl Default for TextEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl TextEngine {
    pub fn new() -> Self {
        Self {
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
            cache: HashMap::with_capacity(64),
            clock: 0,
        }
    }

    pub fn measure(&mut self, req: &MeasureTextRequest) -> TextMetrics {
        let font_size = req.font_size.clamp(6.0, 256.0);
        let line_height = if req.line_height > 0.0 {
            req.line_height.max(font_size)
        } else {
            (font_size * 1.35).round()
        };
        if req.text.is_empty() {
            return TextMetrics {
                width: 0.0,
                height: line_height,
                lines: 1,
            };
        }

        let max_w = if req.max_width > 0.0 {
            Some(req.max_width)
        } else {
            None
        };
        let empty_fonts = self.font_system.db().is_empty();
        let entry = self.get_or_shape(&req.text, font_size, line_height, max_w, req.flags);
        let mut measured_w = entry.width;
        let lines = entry.lines;
        // Fallback estimate when running in an environment with no system fonts installed.
        if empty_fonts && measured_w == 0.0 {
            measured_w = (req.text.chars().count() as f32) * font_size * 0.56;
        }
        TextMetrics {
            width: measured_w.ceil(),
            height: entry.height.ceil(),
            lines,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_text(
        &mut self,
        rgba: &mut [u8],
        buf_w: usize,
        buf_h: usize,
        clip: PixelClip,
        origin_px_x: f32,
        origin_px_y: f32,
        max_w: f32,
        font_size: f32,
        line_height: f32,
        scale: f32,
        color: RgbaColor,
        flags: TextFlags,
        text: &str,
    ) {
        if text.is_empty() || color.a == 0 || buf_w == 0 || buf_h == 0 || scale <= 0.05 {
            return;
        }
        let px_size = (font_size * scale).clamp(6.0, 512.0);
        let raw_lh = if line_height > 0.0 {
            line_height
        } else {
            font_size * 1.35
        };
        let px_lh = (raw_lh * scale).max(px_size);
        let scaled_max_w = if max_w > 0.0 {
            Some(max_w * scale)
        } else {
            None
        };

        // Populate/lookup in cache before borrowing `self.font_system`/`self.swash_cache`.
        let key = self.ensure_cached(text, px_size, px_lh, scaled_max_w, flags);
        let Some(entry) = self.cache.get(&key) else {
            return;
        };

        for run in entry.buffer.layout_runs() {
            let align_dx = match (scaled_max_w, flags.align) {
                (Some(mw), TextAlign::Center) if mw > run.line_w => (mw - run.line_w) * 0.5,
                (Some(mw), TextAlign::Right) if mw > run.line_w => mw - run.line_w,
                _ => 0.0,
            };
            for glyph in run.glyphs.iter() {
                let physical = glyph.physical((origin_px_x + align_dx, origin_px_y), 1.0);
                let Some(image) = self
                    .swash_cache
                    .get_image(&mut self.font_system, physical.cache_key)
                    .as_ref()
                else {
                    continue;
                };
                let gx = physical.x + image.placement.left;
                let gy = (physical.y as f32 + run.line_y).round() as i32 - image.placement.top;
                let gw = image.placement.width as usize;
                let gh = image.placement.height as usize;
                match image.content {
                    SwashContent::Mask => {
                        blit_mask(rgba, buf_w, buf_h, clip, gx, gy, gw, gh, &image.data, color);
                    }
                    SwashContent::SubpixelMask => {
                        blit_subpixel_mask(
                            rgba,
                            buf_w,
                            buf_h,
                            clip,
                            gx,
                            gy,
                            gw,
                            gh,
                            &image.data,
                            color,
                        );
                    }
                    SwashContent::Color => {
                        blit_color_bitmap(rgba, buf_w, buf_h, clip, gx, gy, gw, gh, &image.data);
                    }
                }
            }
        }
    }

    fn get_or_shape(
        &mut self,
        text: &str,
        px_size: f32,
        px_lh: f32,
        max_w: Option<f32>,
        flags: TextFlags,
    ) -> &CachedBuffer {
        let key = self.ensure_cached(text, px_size, px_lh, max_w, flags);
        &self.cache[&key]
    }

    fn ensure_cached(
        &mut self,
        text: &str,
        px_size: f32,
        px_lh: f32,
        max_w: Option<f32>,
        flags: TextFlags,
    ) -> TextCacheKey {
        self.clock = self.clock.wrapping_add(1);
        let key = TextCacheKey {
            text: text.to_owned(),
            px_size_bits: px_size.to_bits(),
            px_lh_bits: px_lh.to_bits(),
            max_w_bits: max_w.unwrap_or(-1.0).to_bits(),
            flags_bits: flags.to_bits(),
        };
        if let Some(existing) = self.cache.get_mut(&key) {
            existing.last_used = self.clock;
            return key;
        }
        if self.cache.len() >= TEXT_CACHE_MAX_ENTRIES {
            evict_oldest_half(&mut self.cache);
        }
        let built = shape_cached(
            &mut self.font_system,
            text,
            px_size,
            px_lh,
            max_w,
            flags,
            self.clock,
        );
        self.cache.insert(key.clone(), built);
        key
    }
}

fn evict_oldest_half(cache: &mut HashMap<TextCacheKey, CachedBuffer>) {
    let mut stamps: Vec<u64> = cache.values().map(|v| v.last_used).collect();
    if stamps.is_empty() {
        return;
    }
    let mid = stamps.len() / 2;
    let (_, median, _) = stamps.select_nth_unstable(mid);
    let cutoff = *median;
    cache.retain(|_, v| v.last_used > cutoff);
}

fn shape_cached(
    font_system: &mut FontSystem,
    text: &str,
    px_size: f32,
    px_lh: f32,
    max_w: Option<f32>,
    flags: TextFlags,
    last_used: u64,
) -> CachedBuffer {
    let attrs = make_attrs(flags);
    let metrics = Metrics::new(px_size, px_lh);
    let buffer = if flags.ellipsis {
        if let Some(mw) = max_w {
            shape_with_ellipsis(font_system, text, metrics, &attrs, mw)
        } else {
            shape_plain(font_system, text, metrics, &attrs, None)
        }
    } else {
        shape_plain(font_system, text, metrics, &attrs, max_w)
    };
    let mut measured_w: f32 = 0.0;
    let mut lines = 0i32;
    for run in buffer.layout_runs() {
        measured_w = measured_w.max(run.line_w);
        lines += 1;
    }
    if lines == 0 {
        lines = 1;
    }
    CachedBuffer {
        buffer,
        width: measured_w,
        height: (lines as f32) * px_lh,
        lines,
        last_used,
    }
}

fn shape_plain(
    font_system: &mut FontSystem,
    text: &str,
    metrics: Metrics,
    attrs: &Attrs<'_>,
    max_w: Option<f32>,
) -> Buffer {
    let mut buffer = Buffer::new(font_system, metrics);
    buffer.set_wrap(
        font_system,
        if max_w.is_some() {
            Wrap::WordOrGlyph
        } else {
            Wrap::None
        },
    );
    buffer.set_size(font_system, max_w, None);
    buffer.set_text(font_system, text, attrs, Shaping::Advanced);
    buffer.shape_until_scroll(font_system, false);
    buffer
}

fn measure_single_line_width(
    font_system: &mut FontSystem,
    text: &str,
    metrics: Metrics,
    attrs: &Attrs<'_>,
) -> (Buffer, f32) {
    let buffer = shape_plain(font_system, text, metrics, attrs, None);
    let mut w = 0.0f32;
    for run in buffer.layout_runs() {
        w = w.max(run.line_w);
    }
    (buffer, w)
}

fn shape_with_ellipsis(
    font_system: &mut FontSystem,
    text: &str,
    metrics: Metrics,
    attrs: &Attrs<'_>,
    max_w: f32,
) -> Buffer {
    let single = text.split('\n').next().unwrap_or(text);
    let (full_buf, full_w) = measure_single_line_width(font_system, single, metrics, attrs);
    if full_w <= max_w || max_w <= 0.0 {
        return full_buf;
    }
    let boundaries: Vec<usize> = single
        .char_indices()
        .map(|(idx, _)| idx)
        .chain(std::iter::once(single.len()))
        .collect();
    if boundaries.len() <= 1 {
        return full_buf;
    }
    let mut lo = 0usize;
    let mut hi = boundaries.len() - 1;
    let mut best_buf = None;
    let mut candidate = String::with_capacity(single.len() + 3);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        candidate.clear();
        candidate.push_str(&single[..boundaries[mid]]);
        candidate.push_str("...");
        let (buf, w) = measure_single_line_width(font_system, &candidate, metrics, attrs);
        if w <= max_w {
            lo = mid;
            best_buf = Some(buf);
        } else {
            hi = mid - 1;
        }
    }
    if let Some(buf) = best_buf {
        return buf;
    }
    shape_plain(font_system, "...", metrics, attrs, None)
}

fn make_attrs(flags: TextFlags) -> Attrs<'static> {
    let mut attrs = Attrs::new();
    attrs = attrs.family(if flags.monospace {
        Family::Monospace
    } else {
        Family::SansSerif
    });
    if flags.bold {
        attrs = attrs.weight(Weight::SEMIBOLD);
    }
    if flags.italic {
        attrs = attrs.style(Style::Italic);
    }
    attrs
}

#[allow(clippy::too_many_arguments)]
fn blit_mask(
    dst: &mut [u8],
    buf_w: usize,
    buf_h: usize,
    clip: PixelClip,
    gx: i32,
    gy: i32,
    gw: usize,
    gh: usize,
    data: &[u8],
    color: RgbaColor,
) {
    let min_x = clip.x0.max(0);
    let min_y = clip.y0.max(0);
    let max_x = clip.x1.min(buf_w as i32);
    let max_y = clip.y1.min(buf_h as i32);

    for row in 0..gh {
        let py = gy + row as i32;
        if py < min_y || py >= max_y {
            continue;
        }
        let row_off = row * gw;
        for col in 0..gw {
            let px = gx + col as i32;
            if px < min_x || px >= max_x {
                continue;
            }
            let cov = data[row_off + col];
            if cov == 0 {
                continue;
            }
            let idx = (py as usize * buf_w + px as usize) * 4;
            blend_premul_pixel(&mut dst[idx..idx + 4], color, cov);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn blit_subpixel_mask(
    dst: &mut [u8],
    buf_w: usize,
    buf_h: usize,
    clip: PixelClip,
    gx: i32,
    gy: i32,
    gw: usize,
    gh: usize,
    data: &[u8],
    color: RgbaColor,
) {
    let min_x = clip.x0.max(0);
    let min_y = clip.y0.max(0);
    let max_x = clip.x1.min(buf_w as i32);
    let max_y = clip.y1.min(buf_h as i32);

    for row in 0..gh {
        let py = gy + row as i32;
        if py < min_y || py >= max_y {
            continue;
        }
        let row_off = row * gw * 3;
        for col in 0..gw {
            let px = gx + col as i32;
            if px < min_x || px >= max_x {
                continue;
            }
            let base = row_off + col * 3;
            let cov =
                ((data[base] as u16 + data[base + 1] as u16 + data[base + 2] as u16) / 3) as u8;
            if cov == 0 {
                continue;
            }
            let idx = (py as usize * buf_w + px as usize) * 4;
            blend_premul_pixel(&mut dst[idx..idx + 4], color, cov);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn blit_color_bitmap(
    dst: &mut [u8],
    buf_w: usize,
    buf_h: usize,
    clip: PixelClip,
    gx: i32,
    gy: i32,
    gw: usize,
    gh: usize,
    data: &[u8],
) {
    let min_x = clip.x0.max(0);
    let min_y = clip.y0.max(0);
    let max_x = clip.x1.min(buf_w as i32);
    let max_y = clip.y1.min(buf_h as i32);

    for row in 0..gh {
        let py = gy + row as i32;
        if py < min_y || py >= max_y {
            continue;
        }
        let row_off = row * gw * 4;
        for col in 0..gw {
            let px = gx + col as i32;
            if px < min_x || px >= max_x {
                continue;
            }
            let base = row_off + col * 4;
            let color = RgbaColor {
                r: data[base],
                g: data[base + 1],
                b: data[base + 2],
                a: data[base + 3],
            };
            if color.a == 0 {
                continue;
            }
            let idx = (py as usize * buf_w + px as usize) * 4;
            blend_premul_pixel(&mut dst[idx..idx + 4], color, 255);
        }
    }
}

fn blend_premul_pixel(pixel: &mut [u8], color: RgbaColor, coverage: u8) {
    let eff_a = (color.a as u32 * coverage as u32 + 127) / 255;
    if eff_a == 0 {
        return;
    }
    let src_r = (color.r as u32 * eff_a + 127) / 255;
    let src_g = (color.g as u32 * eff_a + 127) / 255;
    let src_b = (color.b as u32 * eff_a + 127) / 255;
    let inv_a = 255 - eff_a;
    pixel[0] = (src_r + (pixel[0] as u32 * inv_a + 127) / 255).min(255) as u8;
    pixel[1] = (src_g + (pixel[1] as u32 * inv_a + 127) / 255).min(255) as u8;
    pixel[2] = (src_b + (pixel[2] as u32 * inv_a + 127) / 255).min(255) as u8;
    pixel[3] = (eff_a + (pixel[3] as u32 * inv_a + 127) / 255).min(255) as u8;
}
