use std::f32::consts::FRAC_PI_2;
use std::path::Path;

use tiny_skia::{
    Color, FillRule, FilterQuality, GradientStop, LineCap, LineJoin, LinearGradient, Mask, Paint,
    Path as SkiaPath, PathBuilder, Pixmap, PixmapPaint, Point, RadialGradient, Rect, SpreadMode,
    Stroke, StrokeDash, Transform,
};

use crate::accessibility::A11yTreeState;
use crate::camera::{apply_sensor_tint, CameraManager, CAM_TINT_RGB};
use crate::protocol::{
    AffineTransform, DrawCmd, DrawPacket, PathVerb, RgbaColor, MAX_OPACITY_DEPTH,
    MAX_TRANSFORM_DEPTH,
};
use crate::svg;
use crate::text::{PixelClip, TextEngine};

const EMPTY_PIXEL_CLIP: PixelClip = PixelClip {
    x0: 0,
    y0: 0,
    x1: 0,
    y1: 0,
};

#[derive(Clone, Copy, PartialEq, Eq)]
struct ClipKey {
    is_plain_rect: bool,
    pixel_clip: PixelClip,
    rect_bits: [u32; 4],
    radii_bits: [u32; 4],
    xform_bits: [u32; 6],
}

impl ClipKey {
    fn plain(pixel_clip: PixelClip) -> Self {
        Self {
            is_plain_rect: true,
            pixel_clip,
            rect_bits: [0; 4],
            radii_bits: [0; 4],
            xform_bits: [0; 6],
        }
    }

    fn transform_bits(xform: Transform) -> [u32; 6] {
        [
            xform.sx.to_bits(),
            xform.ky.to_bits(),
            xform.kx.to_bits(),
            xform.sy.to_bits(),
            xform.tx.to_bits(),
            xform.ty.to_bits(),
        ]
    }

    fn rect(pixel_clip: PixelClip, x: f32, y: f32, w: f32, h: f32, xform: Transform) -> Self {
        if xform.kx == 0.0 && xform.ky == 0.0 {
            return Self::plain(pixel_clip);
        }
        Self {
            is_plain_rect: false,
            pixel_clip,
            rect_bits: [x.to_bits(), y.to_bits(), w.to_bits(), h.to_bits()],
            radii_bits: [0; 4],
            xform_bits: Self::transform_bits(xform),
        }
    }

    fn round(
        pixel_clip: PixelClip,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radii: [f32; 4],
        xform: Transform,
    ) -> Self {
        Self {
            is_plain_rect: false,
            pixel_clip,
            rect_bits: [x.to_bits(), y.to_bits(), w.to_bits(), h.to_bits()],
            radii_bits: [
                radii[0].to_bits(),
                radii[1].to_bits(),
                radii[2].to_bits(),
                radii[3].to_bits(),
            ],
            xform_bits: Self::transform_bits(xform),
        }
    }
}

pub trait RendererBackend {
    fn resize(&mut self, width_px: u32, height_px: u32) -> Result<(), String>;
    fn render_packet(
        &mut self,
        packet: &DrawPacket,
        scale: f32,
        text: &mut TextEngine,
        a11y: &mut A11yTreeState,
        cameras: Option<&CameraManager>,
    ) -> Result<(), String>;
    fn dimensions_px(&self) -> (u32, u32);
    fn rgba_bytes(&self) -> &[u8];
    fn save_png(&self, path: &Path) -> Result<(), String>;
    fn sample_pixel(&self, x_px: u32, y_px: u32) -> Option<RgbaColor>;
}

#[derive(Clone)]
struct ClipEntry {
    pixel_clip: PixelClip,
    path: Option<SkiaPath>,
    xform: Transform,
    is_plain_rect: bool,
    key: ClipKey,
}

fn clamp_clip_to_dims(c: PixelClip, w: u32, h: u32) -> PixelClip {
    let wi = w as i32;
    let hi = h as i32;
    let x0 = c.x0.clamp(0, wi);
    let y0 = c.y0.clamp(0, hi);
    let x1 = c.x1.clamp(x0, wi);
    let y1 = c.y1.clamp(y0, hi);
    PixelClip { x0, y0, x1, y1 }
}

fn expand_clip(c: PixelClip, pad: i32, w: u32, h: u32) -> PixelClip {
    clamp_clip_to_dims(
        PixelClip {
            x0: c.x0.saturating_sub(pad),
            y0: c.y0.saturating_sub(pad),
            x1: c.x1.saturating_add(pad),
            y1: c.y1.saturating_add(pad),
        },
        w,
        h,
    )
}

fn clear_mask_rect(mask: &mut Mask, bounds: PixelClip) {
    let mw = mask.width();
    let mh = mask.height();
    let b = clamp_clip_to_dims(bounds, mw, mh);
    if b.x1 <= b.x0 || b.y1 <= b.y0 {
        return;
    }
    let stride = mw as usize;
    let data = mask.data_mut();
    let x0 = b.x0 as usize;
    let x1 = b.x1 as usize;
    for y in (b.y0 as usize)..(b.y1 as usize) {
        let row = y * stride;
        data[row + x0..row + x1].fill(0);
    }
}

fn fill_mask_rect(mask: &mut Mask, bounds: PixelClip, val: u8) {
    let mw = mask.width();
    let mh = mask.height();
    let b = clamp_clip_to_dims(bounds, mw, mh);
    if b.x1 <= b.x0 || b.y1 <= b.y0 {
        return;
    }
    let stride = mw as usize;
    let data = mask.data_mut();
    let x0 = b.x0 as usize;
    let x1 = b.x1 as usize;
    for y in (b.y0 as usize)..(b.y1 as usize) {
        let row = y * stride;
        data[row + x0..row + x1].fill(val);
    }
}

fn rect_contained_in_plain_clip(
    clip_stack: &[ClipEntry],
    active_clip: PixelClip,
    xform: Transform,
    rect: [f32; 4],
    pad_px: i32,
) -> bool {
    if clip_stack.len() <= 1 {
        return true;
    }
    let all_plain_axis_aligned = clip_stack[1..]
        .iter()
        .all(|c| c.is_plain_rect && c.xform.kx == 0.0 && c.xform.ky == 0.0);
    if !all_plain_axis_aligned
        || active_clip.x1 <= active_clip.x0
        || active_clip.y1 <= active_clip.y0
    {
        return false;
    }
    let pc = transformed_pixel_clip(xform, rect[0], rect[1], rect[2], rect[3]);
    pc.x0.saturating_sub(pad_px) >= active_clip.x0
        && pc.y0.saturating_sub(pad_px) >= active_clip.y0
        && pc.x1.saturating_add(pad_px) <= active_clip.x1
        && pc.y1.saturating_add(pad_px) <= active_clip.y1
}

fn polyline_bounds(points: &[(f32, f32)]) -> [f32; 4] {
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for &(px, py) in points {
        min_x = min_x.min(px);
        min_y = min_y.min(py);
        max_x = max_x.max(px);
        max_y = max_y.max(py);
    }
    [
        min_x,
        min_y,
        (max_x - min_x).max(0.0),
        (max_y - min_y).max(0.0),
    ]
}

pub struct TinySkiaBackend {
    pixmap: Pixmap,
    /// Reusable full-frame clip mask scratch.
    clip_mask: Option<Mask>,
    clip_mask_bounds: PixelClip,
    /// Reusable secondary mask for intersecting nested/rounded clips without reallocating.
    clip_submask: Option<Mask>,
    clip_submask_bounds: PixelClip,
    /// Cache key describing the mask contents currently materialized in `clip_mask`.
    materialized_clip: Vec<ClipKey>,
    /// Reusable horizontal source-offset LUT for fast camera scanline blitting.
    cam_src_x_map: Vec<usize>,
    /// Reusable premultiplication scratch + pixmap for RGBA images.
    img_premul: Vec<u8>,
    img_pixmap: Option<Pixmap>,
    /// Reusable scratch pixmap and blur scratch for box shadows.
    shadow_pixmap: Option<Pixmap>,
    shadow_blur_tmp: Vec<u8>,
}

impl TinySkiaBackend {
    pub fn new(width_px: u32, height_px: u32) -> Result<Self, String> {
        let w = width_px.max(1);
        let h = height_px.max(1);
        let pixmap =
            Pixmap::new(w, h).ok_or_else(|| format!("failed to allocate {w}x{h} pixmap"))?;
        Ok(Self {
            pixmap,
            clip_mask: None,
            clip_mask_bounds: EMPTY_PIXEL_CLIP,
            clip_submask: None,
            clip_submask_bounds: EMPTY_PIXEL_CLIP,
            materialized_clip: Vec::new(),
            cam_src_x_map: Vec::new(),
            img_premul: Vec::new(),
            img_pixmap: None,
            shadow_pixmap: None,
            shadow_blur_tmp: Vec::new(),
        })
    }

    fn ensure_clip_mask(&mut self, width_px: u32, height_px: u32, clips: &[ClipEntry]) {
        if clips.len() <= 1 {
            return;
        }
        let w = width_px.max(1);
        let h = height_px.max(1);
        let all_plain_axis_aligned = clips[1..]
            .iter()
            .all(|c| c.is_plain_rect && c.xform.kx == 0.0 && c.xform.ky == 0.0);
        let active_clip = clips
            .last()
            .map(|c| c.pixel_clip)
            .unwrap_or_else(|| PixelClip::full(w as i32, h as i32));

        let mask_ok = self
            .clip_mask
            .as_ref()
            .is_some_and(|m| m.width() == w && m.height() == h);
        if mask_ok {
            let cache_hit = if all_plain_axis_aligned {
                self.materialized_clip.len() == 1
                    && self.materialized_clip[0] == ClipKey::plain(active_clip)
            } else {
                self.materialized_clip.len() == clips.len() - 1
                    && self
                        .materialized_clip
                        .iter()
                        .zip(&clips[1..])
                        .all(|(k, c)| *k == c.key)
            };
            if cache_hit {
                return;
            }
        } else {
            self.clip_mask = Mask::new(w, h);
            self.clip_mask_bounds = EMPTY_PIXEL_CLIP;
            self.materialized_clip.clear();
        }

        let Some(mask) = self.clip_mask.as_mut() else {
            return;
        };
        clear_mask_rect(mask, self.clip_mask_bounds);
        self.clip_mask_bounds = EMPTY_PIXEL_CLIP;
        self.materialized_clip.clear();

        if all_plain_axis_aligned {
            let bounded = clamp_clip_to_dims(active_clip, w, h);
            fill_mask_rect(mask, bounded, 255);
            self.clip_mask_bounds = bounded;
            self.materialized_clip.push(ClipKey::plain(active_clip));
            return;
        }

        let entries = &clips[1..];
        let Some(first_path) = entries[0].path.as_ref() else {
            return;
        };
        mask.fill_path(
            first_path,
            FillRule::Winding,
            !entries[0].is_plain_rect,
            entries[0].xform,
        );
        let first_bounds = expand_clip(entries[0].pixel_clip, 2, w, h);
        self.clip_mask_bounds = first_bounds;

        if entries.len() > 1 {
            if self
                .clip_submask
                .as_ref()
                .is_none_or(|m| m.width() != w || m.height() != h)
            {
                self.clip_submask = Mask::new(w, h);
                self.clip_submask_bounds = EMPTY_PIXEL_CLIP;
            }
            if let Some(sub) = self.clip_submask.as_mut() {
                let stride = w as usize;
                for entry in &entries[1..] {
                    clear_mask_rect(sub, self.clip_submask_bounds);
                    self.clip_submask_bounds = EMPTY_PIXEL_CLIP;
                    let Some(path) = entry.path.as_ref() else {
                        clear_mask_rect(mask, first_bounds);
                        self.clip_mask_bounds = EMPTY_PIXEL_CLIP;
                        return;
                    };
                    sub.fill_path(path, FillRule::Winding, !entry.is_plain_rect, entry.xform);
                    self.clip_submask_bounds = expand_clip(entry.pixel_clip, 2, w, h);
                    let x0 = first_bounds.x0 as usize;
                    let x1 = first_bounds.x1 as usize;
                    let m_data = mask.data_mut();
                    let s_data = sub.data();
                    for y in (first_bounds.y0 as usize)..(first_bounds.y1 as usize) {
                        let row = y * stride;
                        for (a, b) in m_data[row + x0..row + x1]
                            .iter_mut()
                            .zip(&s_data[row + x0..row + x1])
                        {
                            *a = ((*a as u16 * *b as u16 + 127) / 255) as u8;
                        }
                    }
                }
            }
        }
        self.materialized_clip.extend(entries.iter().map(|c| c.key));
    }
}

impl RendererBackend for TinySkiaBackend {
    fn resize(&mut self, width_px: u32, height_px: u32) -> Result<(), String> {
        let w = width_px.max(1);
        let h = height_px.max(1);
        if self.pixmap.width() == w && self.pixmap.height() == h {
            return Ok(());
        }
        self.pixmap =
            Pixmap::new(w, h).ok_or_else(|| format!("failed to resize pixmap to {w}x{h}"))?;
        self.clip_mask = None;
        self.clip_mask_bounds = EMPTY_PIXEL_CLIP;
        self.clip_submask = None;
        self.clip_submask_bounds = EMPTY_PIXEL_CLIP;
        self.materialized_clip.clear();
        Ok(())
    }

    fn dimensions_px(&self) -> (u32, u32) {
        (self.pixmap.width(), self.pixmap.height())
    }

    fn rgba_bytes(&self) -> &[u8] {
        self.pixmap.data()
    }

    fn save_png(&self, path: &Path) -> Result<(), String> {
        self.pixmap
            .save_png(path)
            .map_err(|e| format!("failed to save PNG '{}': {e}", path.display()))
    }

    fn sample_pixel(&self, x_px: u32, y_px: u32) -> Option<RgbaColor> {
        if x_px >= self.pixmap.width() || y_px >= self.pixmap.height() {
            return None;
        }
        let idx = ((y_px as usize) * (self.pixmap.width() as usize) + (x_px as usize)) * 4;
        let data = self.pixmap.data();
        Some(RgbaColor {
            r: data[idx],
            g: data[idx + 1],
            b: data[idx + 2],
            a: data[idx + 3],
        })
    }

    fn render_packet(
        &mut self,
        packet: &DrawPacket,
        scale: f32,
        text: &mut TextEngine,
        a11y: &mut A11yTreeState,
        cameras: Option<&CameraManager>,
    ) -> Result<(), String> {
        let bg = to_skia_color(packet.clear_color);
        self.pixmap.fill(bg);
        a11y.begin_frame();

        let pw = self.pixmap.width() as i32;
        let ph = self.pixmap.height() as i32;
        let iw = self.pixmap.width();
        let ih = self.pixmap.height();
        let mut clip_stack = vec![ClipEntry {
            pixel_clip: PixelClip::full(pw, ph),
            path: None,
            xform: Transform::identity(),
            is_plain_rect: true,
            key: ClipKey::plain(PixelClip::full(pw, ph)),
        }];
        let mut transform_stack: Vec<Transform> = Vec::new();
        let mut opacity_stack: Vec<f32> = Vec::new();

        for cmd in &packet.commands {
            let active_clip = clip_stack
                .last()
                .map(|c| c.pixel_clip)
                .unwrap_or(PixelClip::full(pw, ph));
            let clip_active = clip_stack.len() > 1;
            let user_xform = transform_stack
                .last()
                .copied()
                .unwrap_or(Transform::identity());
            let xform = user_xform.post_concat(Transform::from_scale(scale, scale));
            let alpha_mul = opacity_stack.last().copied().unwrap_or(1.0);

            match cmd {
                DrawCmd::PushClip { x, y, w, h } => {
                    let next_pc = transformed_pixel_clip(xform, *x, *y, *w, *h);
                    let intersected = active_clip.intersect(next_pc);
                    let rect_path = Rect::from_xywh(*x, *y, (*w).max(0.0), (*h).max(0.0))
                        .map(PathBuilder::from_rect);
                    let key = ClipKey::rect(intersected, *x, *y, *w, *h, xform);
                    clip_stack.push(ClipEntry {
                        pixel_clip: intersected,
                        path: rect_path,
                        xform,
                        is_plain_rect: true,
                        key,
                    });
                }
                DrawCmd::PushRoundClip { x, y, w, h, radii } => {
                    let next_pc = transformed_pixel_clip(xform, *x, *y, *w, *h);
                    let intersected = active_clip.intersect(next_pc);
                    let round_path = build_rounded_rect_path4(*x, *y, *w, *h, *radii);
                    let key = ClipKey::round(intersected, *x, *y, *w, *h, *radii, xform);
                    clip_stack.push(ClipEntry {
                        pixel_clip: intersected,
                        path: round_path,
                        xform,
                        is_plain_rect: false,
                        key,
                    });
                }
                DrawCmd::PopClip => {
                    if clip_stack.len() > 1 {
                        clip_stack.pop();
                    }
                }
                DrawCmd::PushOpacity(op) => {
                    if opacity_stack.len() < MAX_OPACITY_DEPTH {
                        opacity_stack.push((alpha_mul * op.clamp(0.0, 1.0)).clamp(0.0, 1.0));
                    }
                }
                DrawCmd::PopOpacity => {
                    opacity_stack.pop();
                }
                DrawCmd::FillRoundRect {
                    x,
                    y,
                    w,
                    h,
                    radius,
                    fill,
                    stroke,
                    stroke_width,
                } => {
                    let pad = (*stroke_width * scale).ceil() as i32 + 3;
                    let mask = if !rect_contained_in_plain_clip(
                        &clip_stack,
                        active_clip,
                        xform,
                        [*x, *y, *w, *h],
                        pad,
                    ) {
                        self.ensure_clip_mask(iw, ih, &clip_stack);
                        self.clip_mask.as_ref()
                    } else {
                        None
                    };
                    draw_round_rect(
                        &mut self.pixmap,
                        mask,
                        xform,
                        scale,
                        *x,
                        *y,
                        *w,
                        *h,
                        *radius,
                        apply_opacity(*fill, alpha_mul),
                        apply_opacity(*stroke, alpha_mul),
                        *stroke_width,
                    );
                }
                DrawCmd::Line {
                    x1,
                    y1,
                    x2,
                    y2,
                    stroke,
                    stroke_width,
                    dashed,
                } => {
                    let pad = (*stroke_width * scale).ceil() as i32 + 3;
                    let bx = x1.min(*x2);
                    let by = y1.min(*y2);
                    let bw = (x2 - x1).abs();
                    let bh = (y2 - y1).abs();
                    let mask = if !rect_contained_in_plain_clip(
                        &clip_stack,
                        active_clip,
                        xform,
                        [bx, by, bw, bh],
                        pad,
                    ) {
                        self.ensure_clip_mask(iw, ih, &clip_stack);
                        self.clip_mask.as_ref()
                    } else {
                        None
                    };
                    draw_line(
                        &mut self.pixmap,
                        mask,
                        xform,
                        scale,
                        *x1,
                        *y1,
                        *x2,
                        *y2,
                        apply_opacity(*stroke, alpha_mul),
                        *stroke_width,
                        *dashed,
                    );
                }
                DrawCmd::Circle {
                    cx,
                    cy,
                    radius,
                    fill,
                    stroke,
                    stroke_width,
                } => {
                    let pad = (*stroke_width * scale).ceil() as i32 + 3;
                    let d = radius * 2.0;
                    let mask = if !rect_contained_in_plain_clip(
                        &clip_stack,
                        active_clip,
                        xform,
                        [cx - radius, cy - radius, d, d],
                        pad,
                    ) {
                        self.ensure_clip_mask(iw, ih, &clip_stack);
                        self.clip_mask.as_ref()
                    } else {
                        None
                    };
                    draw_circle(
                        &mut self.pixmap,
                        mask,
                        xform,
                        scale,
                        *cx,
                        *cy,
                        *radius,
                        apply_opacity(*fill, alpha_mul),
                        apply_opacity(*stroke, alpha_mul),
                        *stroke_width,
                    );
                }
                DrawCmd::Arc {
                    cx,
                    cy,
                    radius,
                    start_angle,
                    sweep_angle,
                    stroke,
                    stroke_width,
                    line_cap,
                } => {
                    let pad = (*stroke_width * scale).ceil() as i32 + 3;
                    let d = radius * 2.0;
                    let mask = if !rect_contained_in_plain_clip(
                        &clip_stack,
                        active_clip,
                        xform,
                        [cx - radius, cy - radius, d, d],
                        pad,
                    ) {
                        self.ensure_clip_mask(iw, ih, &clip_stack);
                        self.clip_mask.as_ref()
                    } else {
                        None
                    };
                    draw_arc(
                        &mut self.pixmap,
                        mask,
                        xform,
                        scale,
                        *cx,
                        *cy,
                        *radius,
                        *start_angle,
                        *sweep_angle,
                        apply_opacity(*stroke, alpha_mul),
                        *stroke_width,
                        *line_cap,
                    );
                }
                DrawCmd::Polyline {
                    stroke,
                    stroke_width,
                    fill_under,
                    points,
                } => {
                    let pad = (*stroke_width * scale).ceil() as i32 + 3;
                    let contained = points.len() < 2
                        || rect_contained_in_plain_clip(
                            &clip_stack,
                            active_clip,
                            xform,
                            polyline_bounds(points),
                            pad,
                        );
                    let mask = if !contained {
                        self.ensure_clip_mask(iw, ih, &clip_stack);
                        self.clip_mask.as_ref()
                    } else {
                        None
                    };
                    draw_polyline(
                        &mut self.pixmap,
                        mask,
                        xform,
                        scale,
                        apply_opacity(*stroke, alpha_mul),
                        *stroke_width,
                        apply_opacity(*fill_under, alpha_mul),
                        points,
                    );
                }
                DrawCmd::Text {
                    x,
                    y,
                    max_w,
                    font_size,
                    line_height,
                    color,
                    flags,
                    text: body,
                } => {
                    let bw = self.pixmap.width() as usize;
                    let bh = self.pixmap.height() as usize;
                    let mut pt = Point::from_xy(*x, *y);
                    xform.map_point(&mut pt);
                    let eff_scale = effective_scale(xform).max(0.0);
                    text.draw_text(
                        self.pixmap.data_mut(),
                        bw,
                        bh,
                        active_clip,
                        pt.x,
                        pt.y,
                        *max_w,
                        *font_size,
                        *line_height,
                        eff_scale,
                        apply_opacity(*color, alpha_mul),
                        *flags,
                        body,
                    );
                }
                DrawCmd::ImageRgba {
                    dx,
                    dy,
                    dw,
                    dh,
                    width,
                    height,
                    pixels,
                } => {
                    let mask = if !rect_contained_in_plain_clip(
                        &clip_stack,
                        active_clip,
                        xform,
                        [*dx, *dy, *dw, *dh],
                        2,
                    ) {
                        self.ensure_clip_mask(iw, ih, &clip_stack);
                        self.clip_mask.as_ref()
                    } else {
                        None
                    };
                    draw_image_rgba(
                        &mut self.pixmap,
                        mask,
                        xform,
                        alpha_mul,
                        *dx,
                        *dy,
                        *dw,
                        *dh,
                        *width,
                        *height,
                        pixels,
                        &mut self.img_premul,
                        &mut self.img_pixmap,
                    );
                }
                DrawCmd::A11yNode(node) => {
                    a11y.push_node(node);
                }
                DrawCmd::FillRoundRectCorners {
                    x,
                    y,
                    w,
                    h,
                    radii,
                    fill,
                    stroke,
                    stroke_width,
                } => {
                    let pad = (*stroke_width * scale).ceil() as i32 + 3;
                    let mask = if !rect_contained_in_plain_clip(
                        &clip_stack,
                        active_clip,
                        xform,
                        [*x, *y, *w, *h],
                        pad,
                    ) {
                        self.ensure_clip_mask(iw, ih, &clip_stack);
                        self.clip_mask.as_ref()
                    } else {
                        None
                    };
                    draw_round_rect_corners(
                        &mut self.pixmap,
                        mask,
                        xform,
                        scale,
                        *x,
                        *y,
                        *w,
                        *h,
                        *radii,
                        apply_opacity(*fill, alpha_mul),
                        apply_opacity(*stroke, alpha_mul),
                        *stroke_width,
                    );
                }
                DrawCmd::LinearGradientRect {
                    x,
                    y,
                    w,
                    h,
                    radii,
                    x0,
                    y0,
                    x1,
                    y1,
                    spread,
                    stops,
                } => {
                    let mask = if !rect_contained_in_plain_clip(
                        &clip_stack,
                        active_clip,
                        xform,
                        [*x, *y, *w, *h],
                        2,
                    ) {
                        self.ensure_clip_mask(iw, ih, &clip_stack);
                        self.clip_mask.as_ref()
                    } else {
                        None
                    };
                    draw_linear_gradient_rect(
                        &mut self.pixmap,
                        mask,
                        xform,
                        alpha_mul,
                        *x,
                        *y,
                        *w,
                        *h,
                        *radii,
                        *x0,
                        *y0,
                        *x1,
                        *y1,
                        *spread,
                        stops,
                    );
                }
                DrawCmd::RadialGradientRect {
                    x,
                    y,
                    w,
                    h,
                    radii,
                    cx,
                    cy,
                    radius,
                    spread,
                    stops,
                } => {
                    let mask = if !rect_contained_in_plain_clip(
                        &clip_stack,
                        active_clip,
                        xform,
                        [*x, *y, *w, *h],
                        2,
                    ) {
                        self.ensure_clip_mask(iw, ih, &clip_stack);
                        self.clip_mask.as_ref()
                    } else {
                        None
                    };
                    draw_radial_gradient_rect(
                        &mut self.pixmap,
                        mask,
                        xform,
                        alpha_mul,
                        *x,
                        *y,
                        *w,
                        *h,
                        *radii,
                        *cx,
                        *cy,
                        *radius,
                        *spread,
                        stops,
                    );
                }
                DrawCmd::BoxShadow {
                    x,
                    y,
                    w,
                    h,
                    radii,
                    offset_x,
                    offset_y,
                    blur,
                    spread,
                    color,
                } => {
                    let mask = if clip_active {
                        self.ensure_clip_mask(iw, ih, &clip_stack);
                        self.clip_mask.as_ref()
                    } else {
                        None
                    };
                    draw_box_shadow(
                        &mut self.pixmap,
                        mask,
                        self.shadow_pixmap.get_or_insert_with(|| {
                            Pixmap::new(1, 1).expect("1x1 shadow scratch allocation")
                        }),
                        &mut self.shadow_blur_tmp,
                        xform,
                        scale,
                        *x,
                        *y,
                        *w,
                        *h,
                        *radii,
                        *offset_x,
                        *offset_y,
                        *blur,
                        *spread,
                        apply_opacity(*color, alpha_mul),
                    );
                }
                DrawCmd::FillPath {
                    transform,
                    verbs,
                    fill,
                    stroke,
                    stroke_width,
                    fill_rule,
                } => {
                    let mask = if clip_active {
                        self.ensure_clip_mask(iw, ih, &clip_stack);
                        self.clip_mask.as_ref()
                    } else {
                        None
                    };
                    draw_path(
                        &mut self.pixmap,
                        mask,
                        xform,
                        *transform,
                        verbs,
                        apply_opacity(*fill, alpha_mul),
                        apply_opacity(*stroke, alpha_mul),
                        *stroke_width,
                        *fill_rule,
                    );
                }
                DrawCmd::PushTransform(t) => {
                    if transform_stack.len() < MAX_TRANSFORM_DEPTH {
                        let local = affine_to_transform(*t);
                        let composed = local.post_concat(user_xform);
                        transform_stack.push(composed);
                    }
                }
                DrawCmd::PopTransform => {
                    transform_stack.pop();
                }
                DrawCmd::SvgPath {
                    x,
                    y,
                    w,
                    h,
                    view_w,
                    view_h,
                    transform,
                    path_data,
                    fill,
                    stroke,
                    stroke_width,
                } => {
                    let pad = (*stroke_width * scale).ceil() as i32 + 3;
                    let vw_safe = if *view_w > 0.0 { *view_w } else { 1.0 };
                    let vh_safe = if *view_h > 0.0 { *view_h } else { 1.0 };
                    let fit =
                        Transform::from_scale(w / vw_safe, h / vh_safe).post_translate(*x, *y);
                    let svg_xf = affine_to_transform(*transform)
                        .post_concat(fit)
                        .post_concat(xform);
                    let mask = if !rect_contained_in_plain_clip(
                        &clip_stack,
                        active_clip,
                        svg_xf,
                        [0.0, 0.0, vw_safe, vh_safe],
                        pad,
                    ) {
                        self.ensure_clip_mask(iw, ih, &clip_stack);
                        self.clip_mask.as_ref()
                    } else {
                        None
                    };
                    draw_svg_path(
                        &mut self.pixmap,
                        mask,
                        xform,
                        *transform,
                        *x,
                        *y,
                        *w,
                        *h,
                        *view_w,
                        *view_h,
                        path_data,
                        apply_opacity(*fill, alpha_mul),
                        apply_opacity(*stroke, alpha_mul),
                        *stroke_width,
                    );
                }
                DrawCmd::CameraFrame {
                    x,
                    y,
                    w,
                    h,
                    cam_id,
                    flags,
                } => {
                    if let Some(mgr) = cameras {
                        let mask = if !rect_contained_in_plain_clip(
                            &clip_stack,
                            active_clip,
                            xform,
                            [*x, *y, *w, *h],
                            2,
                        ) {
                            self.ensure_clip_mask(iw, ih, &clip_stack);
                            self.clip_mask.as_ref()
                        } else {
                            None
                        };
                        draw_camera_frame(
                            &mut self.pixmap,
                            mask,
                            active_clip,
                            xform,
                            alpha_mul,
                            *x,
                            *y,
                            *w,
                            *h,
                            mgr,
                            *cam_id as i64,
                            *flags,
                            &mut self.cam_src_x_map,
                            &mut self.img_premul,
                            &mut self.img_pixmap,
                        );
                    }
                }
            }
        }
        Ok(())
    }
}

fn apply_opacity(c: RgbaColor, alpha_mul: f32) -> RgbaColor {
    if alpha_mul >= 0.9999 {
        return c;
    }
    let a = ((c.a as f32) * alpha_mul.clamp(0.0, 1.0)).round() as u8;
    RgbaColor {
        r: c.r,
        g: c.g,
        b: c.b,
        a,
    }
}

fn to_skia_color(c: RgbaColor) -> Color {
    Color::from_rgba8(c.r, c.g, c.b, c.a)
}

fn effective_scale(xform: Transform) -> f32 {
    let sx = (xform.sx * xform.sx + xform.ky * xform.ky).sqrt();
    let sy = (xform.kx * xform.kx + xform.sy * xform.sy).sqrt();
    (sx + sy) * 0.5
}

fn transformed_pixel_clip(xform: Transform, x: f32, y: f32, w: f32, h: f32) -> PixelClip {
    let mut pts = [
        Point::from_xy(x, y),
        Point::from_xy(x + w, y),
        Point::from_xy(x, y + h),
        Point::from_xy(x + w, y + h),
    ];
    xform.map_points(&mut pts);
    let min_x = pts.iter().map(|p| p.x).fold(f32::INFINITY, f32::min);
    let min_y = pts.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
    let max_x = pts.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max);
    let max_y = pts.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max);
    PixelClip {
        x0: min_x.floor() as i32,
        y0: min_y.floor() as i32,
        x1: max_x.ceil() as i32,
        y1: max_y.ceil() as i32,
    }
}

fn min_stroke_width(stroke_width: f32, scale: f32) -> f32 {
    let min_logical = 0.75 / scale.max(0.5);
    stroke_width.max(min_logical)
}

fn build_rounded_rect_path(x: f32, y: f32, w: f32, h: f32, radius: f32) -> Option<SkiaPath> {
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let r = radius.max(0.0).min(w * 0.5).min(h * 0.5);
    if r <= 0.05 {
        let rect = Rect::from_xywh(x, y, w, h)?;
        return Some(PathBuilder::from_rect(rect));
    }
    let k = r * 0.552_284_8;
    let x1 = x + w;
    let y1 = y + h;
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x1 - r, y);
    pb.cubic_to(x1 - r + k, y, x1, y + r - k, x1, y + r);
    pb.line_to(x1, y1 - r);
    pb.cubic_to(x1, y1 - r + k, x1 - r + k, y1, x1 - r, y1);
    pb.line_to(x + r, y1);
    pb.cubic_to(x + r - k, y1, x, y1 - r + k, x, y1 - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

#[allow(clippy::too_many_arguments)]
fn draw_round_rect(
    pixmap: &mut Pixmap,
    mask: Option<&Mask>,
    xform: Transform,
    scale: f32,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: f32,
    fill: RgbaColor,
    stroke: RgbaColor,
    stroke_width: f32,
) {
    let Some(path) = build_rounded_rect_path(x, y, w, h, radius) else {
        return;
    };
    if fill.a > 0 {
        let mut paint = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        paint.set_color(to_skia_color(fill));
        pixmap.fill_path(&path, &paint, FillRule::Winding, xform, mask);
    }
    if stroke.a > 0 && stroke_width > 0.0 {
        let mut paint = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        paint.set_color(to_skia_color(stroke));
        let st = Stroke {
            width: min_stroke_width(stroke_width, scale),
            ..Stroke::default()
        };
        pixmap.stroke_path(&path, &paint, &st, xform, mask);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_line(
    pixmap: &mut Pixmap,
    mask: Option<&Mask>,
    xform: Transform,
    scale: f32,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    stroke: RgbaColor,
    stroke_width: f32,
    dashed: bool,
) {
    if stroke.a == 0 || stroke_width <= 0.0 {
        return;
    }
    let mut pb = PathBuilder::new();
    pb.move_to(x1, y1);
    pb.line_to(x2, y2);
    let Some(path) = pb.finish() else {
        return;
    };
    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };
    paint.set_color(to_skia_color(stroke));
    let mut st = Stroke {
        width: min_stroke_width(stroke_width, scale),
        line_cap: LineCap::Round,
        ..Stroke::default()
    };
    if dashed {
        st.dash = StrokeDash::new(vec![5.0, 4.0], 0.0);
    }
    pixmap.stroke_path(&path, &paint, &st, xform, mask);
}

#[allow(clippy::too_many_arguments)]
fn draw_circle(
    pixmap: &mut Pixmap,
    mask: Option<&Mask>,
    xform: Transform,
    scale: f32,
    cx: f32,
    cy: f32,
    radius: f32,
    fill: RgbaColor,
    stroke: RgbaColor,
    stroke_width: f32,
) {
    if radius <= 0.0 {
        return;
    }
    let Some(path) = PathBuilder::from_circle(cx, cy, radius) else {
        return;
    };
    if fill.a > 0 {
        let mut paint = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        paint.set_color(to_skia_color(fill));
        pixmap.fill_path(&path, &paint, FillRule::Winding, xform, mask);
    }
    if stroke.a > 0 && stroke_width > 0.0 {
        let mut paint = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        paint.set_color(to_skia_color(stroke));
        let st = Stroke {
            width: min_stroke_width(stroke_width, scale),
            ..Stroke::default()
        };
        pixmap.stroke_path(&path, &paint, &st, xform, mask);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_arc(
    pixmap: &mut Pixmap,
    mask: Option<&Mask>,
    xform: Transform,
    scale: f32,
    cx: f32,
    cy: f32,
    radius: f32,
    start_angle: f32,
    sweep_angle: f32,
    stroke: RgbaColor,
    stroke_width: f32,
    line_cap: i32,
) {
    if radius <= 0.0 || stroke.a == 0 || stroke_width <= 0.0 || sweep_angle.abs() < 1e-4 {
        return;
    }
    let sweep = sweep_angle.clamp(-std::f32::consts::TAU, std::f32::consts::TAU);
    let segments = ((sweep.abs() / FRAC_PI_2).ceil() as usize).clamp(1, 4);
    let seg_angle = sweep / (segments as f32);
    let k = (4.0 / 3.0) * (seg_angle * 0.25).tan();

    let mut pb = PathBuilder::new();
    let mut a0 = start_angle;
    let (s0, c0) = a0.sin_cos();
    pb.move_to(cx + radius * c0, cy + radius * s0);
    for _ in 0..segments {
        let a1 = a0 + seg_angle;
        let (sa0, ca0) = a0.sin_cos();
        let (sa1, ca1) = a1.sin_cos();
        let x0 = cx + radius * ca0;
        let y0 = cy + radius * sa0;
        let x3 = cx + radius * ca1;
        let y3 = cy + radius * sa1;
        let c1x = x0 - k * radius * sa0;
        let c1y = y0 + k * radius * ca0;
        let c2x = x3 + k * radius * sa1;
        let c2y = y3 - k * radius * ca1;
        pb.cubic_to(c1x, c1y, c2x, c2y, x3, y3);
        a0 = a1;
    }
    let Some(path) = pb.finish() else {
        return;
    };
    let cap = match line_cap {
        1 => LineCap::Butt,
        2 => LineCap::Square,
        _ => LineCap::Round,
    };
    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };
    paint.set_color(to_skia_color(stroke));
    let st = Stroke {
        width: min_stroke_width(stroke_width, scale),
        line_cap: cap,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    pixmap.stroke_path(&path, &paint, &st, xform, mask);
}

#[allow(clippy::too_many_arguments)]
fn draw_polyline(
    pixmap: &mut Pixmap,
    mask: Option<&Mask>,
    xform: Transform,
    scale: f32,
    stroke: RgbaColor,
    stroke_width: f32,
    fill_under: RgbaColor,
    points: &[(f32, f32)],
) {
    if points.len() < 2 {
        return;
    }
    if fill_under.a > 0 {
        let max_y = points.iter().map(|p| p.1).fold(points[0].1, f32::max);
        let mut area = PathBuilder::new();
        area.move_to(points[0].0, max_y);
        for &(px, py) in points {
            area.line_to(px, py);
        }
        if let Some(&(last_x, _)) = points.last() {
            area.line_to(last_x, max_y);
        }
        area.close();
        if let Some(area_path) = area.finish() {
            let mut paint = Paint {
                anti_alias: true,
                ..Paint::default()
            };
            paint.set_color(to_skia_color(fill_under));
            pixmap.fill_path(&area_path, &paint, FillRule::Winding, xform, mask);
        }
    }
    if stroke.a > 0 && stroke_width > 0.0 {
        let mut pb = PathBuilder::new();
        pb.move_to(points[0].0, points[0].1);
        for &(px, py) in &points[1..] {
            pb.line_to(px, py);
        }
        if let Some(path) = pb.finish() {
            let mut paint = Paint {
                anti_alias: true,
                ..Paint::default()
            };
            paint.set_color(to_skia_color(stroke));
            let st = Stroke {
                width: stroke_width.max(0.8 / scale.max(0.5)),
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Stroke::default()
            };
            pixmap.stroke_path(&path, &paint, &st, xform, mask);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn blit_premul_pixmap(
    pixmap: &mut Pixmap,
    mask: Option<&Mask>,
    xform: Transform,
    alpha_mul: f32,
    dx: f32,
    dy: f32,
    dw: f32,
    dh: f32,
    img_w: u32,
    img_h: u32,
    premul: &[u8],
    pixmap_scratch: &mut Option<Pixmap>,
) {
    let needs_new = pixmap_scratch
        .as_ref()
        .is_none_or(|p| p.width() != img_w || p.height() != img_h);
    if needs_new {
        *pixmap_scratch = Pixmap::new(img_w.max(1), img_h.max(1));
    }
    let Some(src) = pixmap_scratch.as_mut() else {
        return;
    };
    let src_data = src.data_mut();
    if src_data.len() == premul.len() {
        src_data.copy_from_slice(premul);
    } else {
        return;
    }
    let sx = dw / (img_w as f32);
    let sy = dh / (img_h as f32);
    let img_xform = Transform::from_scale(sx, sy)
        .post_translate(dx, dy)
        .post_concat(xform);
    let paint = PixmapPaint {
        opacity: alpha_mul.clamp(0.0, 1.0),
        quality: FilterQuality::Bilinear,
        ..PixmapPaint::default()
    };
    pixmap.draw_pixmap(0, 0, src.as_ref(), &paint, img_xform, mask);
}

#[allow(clippy::too_many_arguments)]
fn draw_image_rgba(
    pixmap: &mut Pixmap,
    mask: Option<&Mask>,
    xform: Transform,
    alpha_mul: f32,
    dx: f32,
    dy: f32,
    dw: f32,
    dh: f32,
    img_w: u32,
    img_h: u32,
    pixels: &[u8],
    premul: &mut Vec<u8>,
    pixmap_scratch: &mut Option<Pixmap>,
) {
    if dw <= 0.0
        || dh <= 0.0
        || alpha_mul <= 0.001
        || pixels.len() < (img_w as usize) * (img_h as usize) * 4
    {
        return;
    }
    premul.clear();
    premul.reserve(pixels.len());
    for base in (0..pixels.len()).step_by(4) {
        if base + 3 >= pixels.len() {
            break;
        }
        let a = pixels[base + 3] as u32;
        premul.push(((pixels[base] as u32 * a + 127) / 255) as u8);
        premul.push(((pixels[base + 1] as u32 * a + 127) / 255) as u8);
        premul.push(((pixels[base + 2] as u32 * a + 127) / 255) as u8);
        premul.push(pixels[base + 3]);
    }
    blit_premul_pixmap(
        pixmap,
        mask,
        xform,
        alpha_mul,
        dx,
        dy,
        dw,
        dh,
        img_w,
        img_h,
        premul,
        pixmap_scratch,
    );
}

#[inline]
fn blend_scanline_pixel(dst: &mut [u8], d_off: usize, rgb: [u8; 3], cov: u8) {
    if cov == 255 {
        dst[d_off] = rgb[0];
        dst[d_off + 1] = rgb[1];
        dst[d_off + 2] = rgb[2];
        dst[d_off + 3] = 255;
    } else if cov > 0 {
        let cov32 = cov as u32;
        let inv = 255 - cov32;
        dst[d_off] = ((rgb[0] as u32 * cov32 + dst[d_off] as u32 * inv + 127) / 255) as u8;
        dst[d_off + 1] = ((rgb[1] as u32 * cov32 + dst[d_off + 1] as u32 * inv + 127) / 255) as u8;
        dst[d_off + 2] = ((rgb[2] as u32 * cov32 + dst[d_off + 2] as u32 * inv + 127) / 255) as u8;
        dst[d_off + 3] = ((255 * cov32 + dst[d_off + 3] as u32 * inv + 127) / 255) as u8;
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_camera_frame(
    pixmap: &mut Pixmap,
    mask: Option<&Mask>,
    active_clip: PixelClip,
    xform: Transform,
    alpha_mul: f32,
    dx: f32,
    dy: f32,
    dw: f32,
    dh: f32,
    cameras: &CameraManager,
    cam_id: i64,
    flags: i32,
    cam_src_x_map: &mut Vec<usize>,
    premul: &mut Vec<u8>,
    pixmap_scratch: &mut Option<Pixmap>,
) {
    if dw <= 0.0 || dh <= 0.0 || alpha_mul <= 0.001 {
        return;
    }
    let mirror = (flags & 1) != 0;
    let tint_mode = (flags >> 1) & 3;

    if xform.kx == 0.0 && xform.ky == 0.0 && xform.sx > 0.0 && xform.sy > 0.0 && alpha_mul >= 0.999
    {
        let mut p0 = Point::from_xy(dx, dy);
        let mut p1 = Point::from_xy(dx + dw, dy + dh);
        xform.map_point(&mut p0);
        xform.map_point(&mut p1);
        let pw = pixmap.width() as i32;
        let ph = pixmap.height() as i32;
        let fx0 = p0.x;
        let fy0 = p0.y;
        let span_w = (p1.x - p0.x).max(1.0);
        let span_h = (p1.y - p0.y).max(1.0);
        let vis_x0 = (fx0.floor() as i32).max(active_clip.x0).max(0);
        let vis_y0 = (fy0.floor() as i32).max(active_clip.y0).max(0);
        let vis_x1 = (p1.x.ceil() as i32).min(active_clip.x1).min(pw);
        let vis_y1 = (p1.y.ceil() as i32).min(active_clip.y1).min(ph);
        if vis_x1 <= vis_x0 || vis_y1 <= vis_y0 {
            return;
        }
        let dst_pw = pixmap.width() as usize;
        let dst = pixmap.data_mut();
        cameras.with_latest_frame(cam_id, |cam_w, cam_h, rgba| {
            let cw = cam_w as usize;
            let ch = cam_h as usize;
            let src_stride = cw * 4;
            cam_src_x_map.clear();
            cam_src_x_map.reserve((vis_x1 - vis_x0) as usize);
            let cwf = cam_w as f32;
            let chf = cam_h as f32;
            for px in vis_x0..vis_x1 {
                let u = ((px as f32 + 0.5) - fx0) / span_w;
                let sx_raw = (u.clamp(0.0, 0.999_99) * cwf) as usize;
                let sx = if mirror {
                    (cw - 1).saturating_sub(sx_raw)
                } else {
                    sx_raw.min(cw - 1)
                };
                cam_src_x_map.push(sx * 4);
            }
            let vx0_u = vis_x0 as usize;
            let vx1_u = vis_x1 as usize;
            for py in vis_y0..vis_y1 {
                let v = ((py as f32 + 0.5) - fy0) / span_h;
                let sy = ((v.clamp(0.0, 0.999_99) * chf) as usize).min(ch - 1);
                let src_row = &rgba[sy * src_stride..(sy + 1) * src_stride];
                let dst_row_px = (py as usize) * dst_pw;
                if let Some(m) = mask {
                    let mask_row = &m.data()[dst_row_px + vx0_u..dst_row_px + vx1_u];
                    for (i, &soff) in cam_src_x_map.iter().enumerate() {
                        let cov = mask_row[i];
                        if cov == 0 {
                            continue;
                        }
                        let rgb = if tint_mode == CAM_TINT_RGB {
                            [src_row[soff], src_row[soff + 1], src_row[soff + 2]]
                        } else {
                            apply_sensor_tint(
                                src_row[soff],
                                src_row[soff + 1],
                                src_row[soff + 2],
                                tint_mode,
                            )
                        };
                        let d_off = (dst_row_px + vx0_u + i) * 4;
                        blend_scanline_pixel(dst, d_off, rgb, cov);
                    }
                } else if tint_mode == CAM_TINT_RGB {
                    for (i, &soff) in cam_src_x_map.iter().enumerate() {
                        let d_off = (dst_row_px + vx0_u + i) * 4;
                        dst[d_off] = src_row[soff];
                        dst[d_off + 1] = src_row[soff + 1];
                        dst[d_off + 2] = src_row[soff + 2];
                        dst[d_off + 3] = 255;
                    }
                } else {
                    for (i, &soff) in cam_src_x_map.iter().enumerate() {
                        let rgb = apply_sensor_tint(
                            src_row[soff],
                            src_row[soff + 1],
                            src_row[soff + 2],
                            tint_mode,
                        );
                        let d_off = (dst_row_px + vx0_u + i) * 4;
                        dst[d_off] = rgb[0];
                        dst[d_off + 1] = rgb[1];
                        dst[d_off + 2] = rgb[2];
                        dst[d_off + 3] = 255;
                    }
                }
            }
        });
        return;
    }

    let Some((img_w, img_h)) = cameras.write_tinted_premul(cam_id, mirror, tint_mode, premul)
    else {
        return;
    };
    blit_premul_pixmap(
        pixmap,
        mask,
        xform,
        alpha_mul,
        dx,
        dy,
        dw,
        dh,
        img_w,
        img_h,
        premul,
        pixmap_scratch,
    );
}

fn affine_to_transform(t: AffineTransform) -> Transform {
    Transform::from_row(t[0], t[1], t[2], t[3], t[4], t[5])
}

fn spread_mode(code: i32) -> SpreadMode {
    match code {
        1 => SpreadMode::Reflect,
        2 => SpreadMode::Repeat,
        _ => SpreadMode::Pad,
    }
}

fn resolve_fill_rule(code: i32) -> FillRule {
    if code == 1 {
        FillRule::EvenOdd
    } else {
        FillRule::Winding
    }
}

/// Clamp four independent corner radii so adjacent corners never overlap.
fn fit_radii(w: f32, h: f32, radii: [f32; 4]) -> [f32; 4] {
    let [tl, tr, br, bl] = radii;
    let mut r = [
        tl.max(0.0).min(w).min(h),
        tr.max(0.0).min(w).min(h),
        br.max(0.0).min(w).min(h),
        bl.max(0.0).min(w).min(h),
    ];
    let sx = if r[0] + r[1] > w && r[0] + r[1] > 0.0 {
        w / (r[0] + r[1])
    } else {
        1.0
    };
    let sxb = if r[2] + r[3] > w && r[2] + r[3] > 0.0 {
        w / (r[2] + r[3])
    } else {
        1.0
    };
    let sy = if r[0] + r[3] > h && r[0] + r[3] > 0.0 {
        h / (r[0] + r[3])
    } else {
        1.0
    };
    let syb = if r[1] + r[2] > h && r[1] + r[2] > 0.0 {
        h / (r[1] + r[2])
    } else {
        1.0
    };
    let s = sx.min(sxb).min(sy).min(syb);
    if s < 1.0 {
        for value in &mut r {
            *value *= s;
        }
    }
    r
}

fn build_rounded_rect_path4(x: f32, y: f32, w: f32, h: f32, radii: [f32; 4]) -> Option<SkiaPath> {
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let [tl, tr, br, bl] = fit_radii(w, h, radii);
    if tl <= 0.05 && tr <= 0.05 && br <= 0.05 && bl <= 0.05 {
        return Some(PathBuilder::from_rect(Rect::from_xywh(x, y, w, h)?));
    }
    let k = 0.552_284_8;
    let x1 = x + w;
    let y1 = y + h;
    let mut pb = PathBuilder::new();
    pb.move_to(x + tl, y);
    pb.line_to(x1 - tr, y);
    if tr > 0.05 {
        pb.cubic_to(x1 - tr + tr * k, y, x1, y + tr - tr * k, x1, y + tr);
    }
    pb.line_to(x1, y1 - br);
    if br > 0.05 {
        pb.cubic_to(x1, y1 - br + br * k, x1 - br + br * k, y1, x1 - br, y1);
    }
    pb.line_to(x + bl, y1);
    if bl > 0.05 {
        pb.cubic_to(x + bl - bl * k, y1, x, y1 - bl + bl * k, x, y1 - bl);
    }
    pb.line_to(x, y + tl);
    if tl > 0.05 {
        pb.cubic_to(x, y + tl - tl * k, x + tl - tl * k, y, x + tl, y);
    }
    pb.close();
    pb.finish()
}

#[allow(clippy::too_many_arguments)]
fn draw_round_rect_corners(
    pixmap: &mut Pixmap,
    mask: Option<&Mask>,
    xform: Transform,
    scale: f32,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radii: [f32; 4],
    fill: RgbaColor,
    stroke: RgbaColor,
    stroke_width: f32,
) {
    let Some(path) = build_rounded_rect_path4(x, y, w, h, radii) else {
        return;
    };
    if fill.a > 0 {
        let mut paint = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        paint.set_color(to_skia_color(fill));
        pixmap.fill_path(&path, &paint, FillRule::Winding, xform, mask);
    }
    if stroke.a > 0 && stroke_width > 0.0 {
        let mut paint = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        paint.set_color(to_skia_color(stroke));
        let st = Stroke {
            width: min_stroke_width(stroke_width, scale),
            ..Stroke::default()
        };
        pixmap.stroke_path(&path, &paint, &st, xform, mask);
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_gradient(
    pixmap: &mut Pixmap,
    mask: Option<&Mask>,
    xform: Transform,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radii: [f32; 4],
    shader: Option<tiny_skia::Shader<'static>>,
) {
    let Some(path) = build_rounded_rect_path4(x, y, w, h, radii) else {
        return;
    };
    let Some(shader) = shader else {
        return;
    };
    let paint = Paint {
        shader,
        anti_alias: true,
        ..Paint::default()
    };
    pixmap.fill_path(&path, &paint, FillRule::Winding, xform, mask);
}

#[allow(clippy::too_many_arguments)]
fn draw_linear_gradient_rect(
    pixmap: &mut Pixmap,
    mask: Option<&Mask>,
    xform: Transform,
    alpha_mul: f32,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radii: [f32; 4],
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    spread: i32,
    stops: &[(f32, RgbaColor)],
) {
    let skia_stops: Vec<GradientStop> = stops
        .iter()
        .map(|(pos, c)| GradientStop::new(*pos, to_skia_color(apply_opacity(*c, alpha_mul))))
        .collect();
    let shader = LinearGradient::new(
        Point::from_xy(x0, y0),
        Point::from_xy(x1, y1),
        skia_stops,
        spread_mode(spread),
        Transform::identity(),
    );
    paint_gradient(pixmap, mask, xform, x, y, w, h, radii, shader);
}

#[allow(clippy::too_many_arguments)]
fn draw_radial_gradient_rect(
    pixmap: &mut Pixmap,
    mask: Option<&Mask>,
    xform: Transform,
    alpha_mul: f32,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radii: [f32; 4],
    cx: f32,
    cy: f32,
    radius: f32,
    spread: i32,
    stops: &[(f32, RgbaColor)],
) {
    let skia_stops: Vec<GradientStop> = stops
        .iter()
        .map(|(pos, c)| GradientStop::new(*pos, to_skia_color(apply_opacity(*c, alpha_mul))))
        .collect();
    let shader = RadialGradient::new(
        Point::from_xy(cx, cy),
        0.0,
        Point::from_xy(cx, cy),
        radius.max(0.001),
        skia_stops,
        spread_mode(spread),
        Transform::identity(),
    );
    paint_gradient(pixmap, mask, xform, x, y, w, h, radii, shader);
}

/// Symmetric separable 3-pass box blur over premultiplied RGBA bytes (approximates Gaussian).
fn box_blur_rgba(data: &mut [u8], tmp: &mut Vec<u8>, w: usize, h: usize, radius: u32) {
    if radius == 0 || w == 0 || h == 0 {
        return;
    }
    let r = (radius as usize).min(48);
    let win = (2 * r + 1) as u32;
    tmp.resize(data.len(), 0);

    for _pass in 0..3 {
        // Horizontal pass: data -> tmp
        for y in 0..h {
            let row = y * w * 4;
            for c in 0..4 {
                let first = data[row + c] as u32;
                let mut sum = first * (r as u32 + 1);
                for i in 1..=r {
                    let sx = i.min(w - 1);
                    sum += data[row + sx * 4 + c] as u32;
                }
                for x in 0..w {
                    tmp[row + x * 4 + c] = (sum / win) as u8;
                    let add_x = (x + r + 1).min(w - 1);
                    let sub_x = x.saturating_sub(r);
                    sum += data[row + add_x * 4 + c] as u32;
                    sum -= data[row + sub_x * 4 + c] as u32;
                }
            }
        }
        // Vertical pass: tmp -> data
        for x in 0..w {
            for c in 0..4 {
                let first = tmp[x * 4 + c] as u32;
                let mut sum = first * (r as u32 + 1);
                for i in 1..=r {
                    let sy = i.min(h - 1);
                    sum += tmp[(sy * w + x) * 4 + c] as u32;
                }
                for y in 0..h {
                    data[(y * w + x) * 4 + c] = (sum / win) as u8;
                    let add_y = (y + r + 1).min(h - 1);
                    let sub_y = y.saturating_sub(r);
                    sum += tmp[(add_y * w + x) * 4 + c] as u32;
                    sum -= tmp[(sub_y * w + x) * 4 + c] as u32;
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_box_shadow(
    pixmap: &mut Pixmap,
    mask: Option<&Mask>,
    scratch: &mut Pixmap,
    blur_tmp: &mut Vec<u8>,
    xform: Transform,
    scale: f32,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radii: [f32; 4],
    offset_x: f32,
    offset_y: f32,
    blur: f32,
    spread: f32,
    color: RgbaColor,
) {
    if w <= 0.0 || h <= 0.0 || color.a == 0 {
        return;
    }
    let eff_scale = effective_scale(xform).max(scale).max(0.25);
    let mut origin = Point::from_xy(x + offset_x, y + offset_y);
    xform.map_point(&mut origin);
    let sw_px = w * eff_scale;
    let sh_px = h * eff_scale;
    let blur_px = (blur * eff_scale).max(0.0);
    let spread_px = spread * eff_scale;
    let pad = (blur_px * 1.8).max(1.0);
    let sx = origin.x - spread_px - pad;
    let sy = origin.y - spread_px - pad;
    let tw = sw_px + 2.0 * spread_px + 2.0 * pad;
    let th = sh_px + 2.0 * spread_px + 2.0 * pad;
    let pw = tw.max(2.0).ceil() as u32;
    let ph = th.max(2.0).ceil() as u32;
    if scratch.width() != pw || scratch.height() != ph {
        *scratch = match Pixmap::new(pw, ph) {
            Some(pm) => pm,
            None => return,
        };
    }
    scratch.fill(Color::TRANSPARENT);
    let expanded = [
        (radii[0] * eff_scale + spread_px).max(0.0),
        (radii[1] * eff_scale + spread_px).max(0.0),
        (radii[2] * eff_scale + spread_px).max(0.0),
        (radii[3] * eff_scale + spread_px).max(0.0),
    ];
    if let Some(path) =
        build_rounded_rect_path4(spread_px + pad, spread_px + pad, sw_px, sh_px, expanded)
    {
        let paint = Paint {
            shader: tiny_skia::Shader::SolidColor(to_skia_color(color)),
            anti_alias: true,
            ..Paint::default()
        };
        scratch.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
    let box_r = if blur_px >= 0.5 {
        ((blur_px * 0.30).round() as u32).clamp(1, 32)
    } else {
        0
    };
    box_blur_rgba(
        scratch.data_mut(),
        blur_tmp,
        pw as usize,
        ph as usize,
        box_r,
    );
    let paint = PixmapPaint {
        opacity: 1.0,
        quality: FilterQuality::Bilinear,
        ..PixmapPaint::default()
    };
    pixmap.draw_pixmap(
        0,
        0,
        scratch.as_ref(),
        &paint,
        Transform::from_translate(sx, sy),
        mask,
    );
}

#[allow(clippy::too_many_arguments)]
fn build_path(verbs: &[PathVerb]) -> Option<SkiaPath> {
    let mut pb = PathBuilder::new();
    let mut started = false;
    for verb in verbs {
        match *verb {
            PathVerb::MoveTo(x, y) => {
                pb.move_to(x, y);
                started = true;
            }
            PathVerb::LineTo(x, y) => {
                if started {
                    pb.line_to(x, y);
                }
            }
            PathVerb::QuadTo(cx, cy, x, y) => {
                if started {
                    pb.quad_to(cx, cy, x, y);
                }
            }
            PathVerb::CubicTo(c1x, c1y, c2x, c2y, x, y) => {
                if started {
                    pb.cubic_to(c1x, c1y, c2x, c2y, x, y);
                }
            }
            PathVerb::Close => {
                if started {
                    pb.close();
                }
            }
        }
    }
    pb.finish()
}

#[allow(clippy::too_many_arguments)]
fn draw_path(
    pixmap: &mut Pixmap,
    mask: Option<&Mask>,
    xform: Transform,
    local: AffineTransform,
    verbs: &[PathVerb],
    fill: RgbaColor,
    stroke: RgbaColor,
    stroke_width: f32,
    fill_rule: i32,
) {
    let Some(path) = build_path(verbs) else {
        return;
    };
    let xform = affine_to_transform(local).post_concat(xform);
    if fill.a > 0 {
        let mut paint = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        paint.set_color(to_skia_color(fill));
        pixmap.fill_path(&path, &paint, resolve_fill_rule(fill_rule), xform, mask);
    }
    if stroke.a > 0 && stroke_width > 0.0 {
        let mut paint = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        paint.set_color(to_skia_color(stroke));
        let st = Stroke {
            width: stroke_width,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Stroke::default()
        };
        pixmap.stroke_path(&path, &paint, &st, xform, mask);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_svg_path(
    pixmap: &mut Pixmap,
    mask: Option<&Mask>,
    xform: Transform,
    local: AffineTransform,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    view_w: f32,
    view_h: f32,
    path_data: &str,
    fill: RgbaColor,
    stroke: RgbaColor,
    stroke_width: f32,
) {
    let verbs = svg::parse_path_data(path_data);
    if verbs.is_empty() {
        return;
    }
    let Some(path) = build_path(&verbs) else {
        return;
    };
    let sx = if view_w > 0.0 { w / view_w } else { 1.0 };
    let sy = if view_h > 0.0 { h / view_h } else { 1.0 };
    let fit = Transform::from_scale(sx, sy).post_translate(x, y);
    let xform = affine_to_transform(local)
        .post_concat(fit)
        .post_concat(xform);
    if fill.a > 0 {
        let mut paint = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        paint.set_color(to_skia_color(fill));
        pixmap.fill_path(&path, &paint, FillRule::Winding, xform, mask);
    }
    if stroke.a > 0 && stroke_width > 0.0 {
        let mut paint = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        paint.set_color(to_skia_color(stroke));
        let st = Stroke {
            width: stroke_width,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Stroke::default()
        };
        pixmap.stroke_path(&path, &paint, &st, xform, mask);
    }
}
