use std::path::Path;

use tiny_skia::{
    Color, FillRule, FilterQuality, GradientStop, LineCap, LineJoin, LinearGradient, Mask, Paint,
    Path as SkiaPath, PathBuilder, Pixmap, PixmapPaint, Point, RadialGradient, Rect, SpreadMode,
    Stroke, StrokeDash, Transform,
};

use crate::accessibility::A11yTreeState;
use crate::protocol::{AffineTransform, DrawCmd, DrawPacket, PathVerb, RgbaColor};
use crate::svg;
use crate::text::{PixelClip, TextEngine};

pub trait RendererBackend {
    fn resize(&mut self, width_px: u32, height_px: u32) -> Result<(), String>;
    fn render_packet(
        &mut self,
        packet: &DrawPacket,
        scale: f32,
        text: &mut TextEngine,
        a11y: &mut A11yTreeState,
    ) -> Result<(), String>;
    fn dimensions_px(&self) -> (u32, u32);
    fn rgba_bytes(&self) -> &[u8];
    fn save_png(&self, path: &Path) -> Result<(), String>;
    fn sample_pixel(&self, x_px: u32, y_px: u32) -> Option<RgbaColor>;
}

pub struct TinySkiaBackend {
    pixmap: Pixmap,
    /// Reusable full-frame clip mask scratch. Clips are pushed and popped on
    /// almost every frame; without this cache each push allocated and zeroed a
    /// fresh w×h mask.
    clip_mask: Option<Mask>,
    /// Reusable premultiplication scratch + pixmap for RGBA images drawn every
    /// frame (e.g. embedded map tiles).
    img_premul: Vec<u8>,
    img_pixmap: Option<Pixmap>,
    /// Reusable scratch pixmap for blurred box shadows.
    shadow_pixmap: Option<Pixmap>,
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
            img_premul: Vec::new(),
            img_pixmap: None,
            shadow_pixmap: None,
        })
    }

    /// (Re)fill the reusable clip mask scratch to match `clip`. Creating a
    /// `w×h` mask per clip command — previously twice per frame or more —
    /// churned a full-frame allocation every paint.
    fn fill_clip_mask(&mut self, width_px: u32, height_px: u32, clip: PixelClip) {
        let w = width_px.max(1);
        let h = height_px.max(1);
        let needs_new = self
            .clip_mask
            .as_ref()
            .map(|m| m.width() != w || m.height() != h)
            .unwrap_or(true);
        if needs_new {
            self.clip_mask = Mask::new(w, h);
        }
        let Some(mask) = self.clip_mask.as_mut() else {
            return;
        };
        mask.data_mut().fill(0);
        if clip.x1 > clip.x0 && clip.y1 > clip.y0 {
            if let Some(rect) = Rect::from_ltrb(
                clip.x0 as f32,
                clip.y0 as f32,
                clip.x1 as f32,
                clip.y1 as f32,
            ) {
                let path = PathBuilder::from_rect(rect);
                mask.fill_path(&path, FillRule::Winding, false, Transform::identity());
            }
        }
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
        // Clip masks are sized to the frame; drop the scratch on resize.
        self.clip_mask = None;
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
    ) -> Result<(), String> {
        let bg = to_skia_color(packet.clear_color);
        self.pixmap.fill(bg);
        a11y.begin_frame();

        let pw = self.pixmap.width() as i32;
        let ph = self.pixmap.height() as i32;
        let iw = self.pixmap.width();
        let ih = self.pixmap.height();
        let mut clip_stack = vec![PixelClip::full(pw, ph)];
        // Vector commands are authored in logical coordinates; the stack holds
        // user transforms which are composed with the display scale at paint time.
        let mut transform_stack: Vec<Transform> = Vec::new();

        for cmd in &packet.commands {
            let active_clip = *clip_stack.last().unwrap_or(&PixelClip::full(pw, ph));
            let user_xform = transform_stack
                .last()
                .copied()
                .unwrap_or(Transform::identity());
            // physical = scale ∘ user_transform (Transform::post_concat post-multiplies).
            let xform = user_xform.post_concat(Transform::from_scale(scale, scale));
            match cmd {
                DrawCmd::PushClip { x, y, w, h } => {
                    let next = PixelClip {
                        x0: (x * scale).floor() as i32,
                        y0: (y * scale).floor() as i32,
                        x1: ((x + w) * scale).ceil() as i32,
                        y1: ((y + h) * scale).ceil() as i32,
                    };
                    let intersected = active_clip.intersect(next);
                    clip_stack.push(intersected);
                    self.fill_clip_mask(iw, ih, intersected);
                }
                DrawCmd::PopClip => {
                    if clip_stack.len() > 1 {
                        clip_stack.pop();
                    }
                    let top = *clip_stack.last().unwrap_or(&PixelClip::full(pw, ph));
                    if clip_stack.len() > 1 {
                        self.fill_clip_mask(iw, ih, top);
                    } else {
                        self.clip_mask = None;
                    }
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
                    draw_round_rect(
                        &mut self.pixmap,
                        self.clip_mask.as_ref(),
                        scale,
                        *x,
                        *y,
                        *w,
                        *h,
                        *radius,
                        *fill,
                        *stroke,
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
                    draw_line(
                        &mut self.pixmap,
                        self.clip_mask.as_ref(),
                        scale,
                        *x1,
                        *y1,
                        *x2,
                        *y2,
                        *stroke,
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
                    draw_circle(
                        &mut self.pixmap,
                        self.clip_mask.as_ref(),
                        scale,
                        *cx,
                        *cy,
                        *radius,
                        *fill,
                        *stroke,
                        *stroke_width,
                    );
                }
                DrawCmd::Polyline {
                    stroke,
                    stroke_width,
                    fill_under,
                    points,
                } => {
                    draw_polyline(
                        &mut self.pixmap,
                        self.clip_mask.as_ref(),
                        scale,
                        *stroke,
                        *stroke_width,
                        *fill_under,
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
                    text.draw_text(
                        self.pixmap.data_mut(),
                        bw,
                        bh,
                        active_clip,
                        *x,
                        *y,
                        *max_w,
                        *font_size,
                        *line_height,
                        scale,
                        *color,
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
                    draw_image_rgba(
                        &mut self.pixmap,
                        self.clip_mask.as_ref(),
                        scale,
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
                    draw_round_rect_corners(
                        &mut self.pixmap,
                        self.clip_mask.as_ref(),
                        xform,
                        *x,
                        *y,
                        *w,
                        *h,
                        *radii,
                        *fill,
                        *stroke,
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
                    draw_linear_gradient_rect(
                        &mut self.pixmap,
                        self.clip_mask.as_ref(),
                        xform,
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
                    draw_radial_gradient_rect(
                        &mut self.pixmap,
                        self.clip_mask.as_ref(),
                        xform,
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
                    draw_box_shadow(
                        &mut self.pixmap,
                        self.clip_mask.as_ref(),
                        self.shadow_pixmap.get_or_insert_with(|| {
                            Pixmap::new(1, 1).expect("1x1 shadow scratch allocation")
                        }),
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
                        *color,
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
                    draw_path(
                        &mut self.pixmap,
                        self.clip_mask.as_ref(),
                        xform,
                        *transform,
                        verbs,
                        *fill,
                        *stroke,
                        *stroke_width,
                        *fill_rule,
                    );
                }
                DrawCmd::PushTransform(t) => {
                    if transform_stack.len() < crate::protocol::MAX_TRANSFORM_DEPTH {
                        let local = affine_to_transform(*t);
                        // Compose with the transform already on the stack:
                        // new = old ∘ local (local applied to geometry first).
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
                    draw_svg_path(
                        &mut self.pixmap,
                        self.clip_mask.as_ref(),
                        xform,
                        *transform,
                        *x,
                        *y,
                        *w,
                        *h,
                        *view_w,
                        *view_h,
                        path_data,
                        *fill,
                        *stroke,
                        *stroke_width,
                    );
                }
            }
        }
        Ok(())
    }
}

fn to_skia_color(c: RgbaColor) -> Color {
    Color::from_rgba8(c.r, c.g, c.b, c.a)
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
    let Some(path) =
        build_rounded_rect_path(x * scale, y * scale, w * scale, h * scale, radius * scale)
    else {
        return;
    };
    if fill.a > 0 {
        let mut paint = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        paint.set_color(to_skia_color(fill));
        pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            mask,
        );
    }
    if stroke.a > 0 && stroke_width > 0.0 {
        let mut paint = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        paint.set_color(to_skia_color(stroke));
        let st = Stroke {
            width: (stroke_width * scale).max(0.75),
            ..Stroke::default()
        };
        pixmap.stroke_path(&path, &paint, &st, Transform::identity(), mask);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_line(
    pixmap: &mut Pixmap,
    mask: Option<&Mask>,
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
    pb.move_to(x1 * scale, y1 * scale);
    pb.line_to(x2 * scale, y2 * scale);
    let Some(path) = pb.finish() else {
        return;
    };
    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };
    paint.set_color(to_skia_color(stroke));
    let mut st = Stroke {
        width: (stroke_width * scale).max(0.75),
        line_cap: LineCap::Round,
        ..Stroke::default()
    };
    if dashed {
        st.dash = StrokeDash::new(vec![5.0 * scale, 4.0 * scale], 0.0);
    }
    pixmap.stroke_path(&path, &paint, &st, Transform::identity(), mask);
}

#[allow(clippy::too_many_arguments)]
fn draw_circle(
    pixmap: &mut Pixmap,
    mask: Option<&Mask>,
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
    let Some(path) = PathBuilder::from_circle(cx * scale, cy * scale, radius * scale) else {
        return;
    };
    if fill.a > 0 {
        let mut paint = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        paint.set_color(to_skia_color(fill));
        pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            mask,
        );
    }
    if stroke.a > 0 && stroke_width > 0.0 {
        let mut paint = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        paint.set_color(to_skia_color(stroke));
        let st = Stroke {
            width: (stroke_width * scale).max(0.75),
            ..Stroke::default()
        };
        pixmap.stroke_path(&path, &paint, &st, Transform::identity(), mask);
    }
}

fn draw_polyline(
    pixmap: &mut Pixmap,
    mask: Option<&Mask>,
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
        area.move_to(points[0].0 * scale, max_y * scale);
        for &(px, py) in points {
            area.line_to(px * scale, py * scale);
        }
        if let Some(&(last_x, _)) = points.last() {
            area.line_to(last_x * scale, max_y * scale);
        }
        area.close();
        if let Some(area_path) = area.finish() {
            let mut paint = Paint {
                anti_alias: true,
                ..Paint::default()
            };
            paint.set_color(to_skia_color(fill_under));
            pixmap.fill_path(
                &area_path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                mask,
            );
        }
    }
    if stroke.a > 0 && stroke_width > 0.0 {
        let mut pb = PathBuilder::new();
        pb.move_to(points[0].0 * scale, points[0].1 * scale);
        for &(px, py) in &points[1..] {
            pb.line_to(px * scale, py * scale);
        }
        if let Some(path) = pb.finish() {
            let mut paint = Paint {
                anti_alias: true,
                ..Paint::default()
            };
            paint.set_color(to_skia_color(stroke));
            let st = Stroke {
                width: (stroke_width * scale).max(0.8),
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Stroke::default()
            };
            pixmap.stroke_path(&path, &paint, &st, Transform::identity(), mask);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_image_rgba(
    pixmap: &mut Pixmap,
    mask: Option<&Mask>,
    scale: f32,
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
    if dw <= 0.0 || dh <= 0.0 || pixels.len() < (img_w as usize) * (img_h as usize) * 4 {
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
    let needs_new = pixmap_scratch
        .as_ref()
        .map(|p| p.width() != img_w || p.height() != img_h)
        .unwrap_or(true);
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
    let sx = (dw * scale) / (img_w as f32);
    let sy = (dh * scale) / (img_h as f32);
    let xform = Transform::from_scale(sx, sy).post_translate(dx * scale, dy * scale);
    let paint = PixmapPaint {
        opacity: 1.0,
        quality: FilterQuality::Bilinear,
        ..PixmapPaint::default()
    };
    pixmap.draw_pixmap(0, 0, src.as_ref(), &paint, xform, mask);
}

// ---------------------------------------------------------------------------
// Extended draw opcodes: gradients, per-corner radii, shadows, paths and SVG.
// ---------------------------------------------------------------------------

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
            width: stroke_width.max(0.75),
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
        .map(|(pos, c)| GradientStop::new(*pos, to_skia_color(*c)))
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
        .map(|(pos, c)| GradientStop::new(*pos, to_skia_color(*c)))
        .collect();
    let shader = RadialGradient::new(
        Point::from_xy(cx, cy),
        radius,
        Point::from_xy(cx, cy),
        radius,
        skia_stops,
        spread_mode(spread),
        Transform::identity(),
    );
    paint_gradient(pixmap, mask, xform, x, y, w, h, radii, shader);
}

/// Separable box blur over premultiplied RGBA bytes (three passes ≈ Gaussian).
fn box_blur_rgba(data: &mut [u8], w: usize, h: usize, radius: u32) {
    if radius == 0 || w == 0 || h == 0 {
        return;
    }
    let r = radius as usize;
    let mut tmp = vec![0u8; data.len()];
    for y in 0..h {
        let row = y * w * 4;
        for c in 0..4 {
            let mut sum = 0u32;
            for i in 0..=r.min(w - 1) {
                sum += data[row + i * 4 + c] as u32;
            }
            let count = (r.min(w - 1) + 1) as u32;
            for x in 0..w {
                tmp[row + x * 4 + c] = (sum / count) as u8;
                let add = ((x + r + 1).min(w - 1)) * 4 + c;
                let sub = x.saturating_sub(r) * 4 + c;
                sum = sum + data[row + add] as u32 - data[row + sub] as u32;
            }
        }
    }
    for x in 0..w {
        for c in 0..4 {
            let mut sum = 0u32;
            for i in 0..=r.min(h - 1) {
                sum += tmp[(i * w + x) * 4 + c] as u32;
            }
            let count = (r.min(h - 1) + 1) as u32;
            for y in 0..h {
                data[(y * w + x) * 4 + c] = (sum / count) as u8;
                let add = (y + r + 1).min(h - 1) * w + x;
                let sub = y.saturating_sub(r) * w + x;
                sum = sum + tmp[add * 4 + c] as u32 - tmp[sub * 4 + c] as u32;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_box_shadow(
    pixmap: &mut Pixmap,
    mask: Option<&Mask>,
    scratch: &mut Pixmap,
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
    // Work in physical pixels so the shadow scratch matches the target pixmap.
    let x = x * scale;
    let y = y * scale;
    let w = w * scale;
    let h = h * scale;
    let offset_x = offset_x * scale;
    let offset_y = offset_y * scale;
    let blur = blur * scale;
    let spread = spread * scale;
    let pad = (blur * 3.0).max(1.0);
    let sx = x + offset_x - spread - pad;
    let sy = y + offset_y - spread - pad;
    let sw = w + 2.0 * spread + 2.0 * pad;
    let sh = h + 2.0 * spread + 2.0 * pad;
    let pw = sw.max(2.0).ceil() as u32;
    let ph = sh.max(2.0).ceil() as u32;
    if scratch.width() != pw || scratch.height() != ph {
        *scratch = match Pixmap::new(pw, ph) {
            Some(pm) => pm,
            None => return,
        };
    }
    scratch.fill(Color::TRANSPARENT);
    let expanded = [
        radii[0] + spread,
        radii[1] + spread,
        radii[2] + spread,
        radii[3] + spread,
    ];
    if let Some(path) = build_rounded_rect_path4(spread + pad, spread + pad, w, h, expanded) {
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
    box_blur_rgba(
        scratch.data_mut(),
        pw as usize,
        ph as usize,
        (blur * 1.5).round() as u32,
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
