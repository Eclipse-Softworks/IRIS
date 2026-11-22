use std::convert::TryFrom;

use thiserror::Error;

/// Magic header (`"IRUI"` little-endian) for binary draw-command buffers.
pub const DRAW_PACKET_MAGIC: u32 = 0x4952_5549;
pub const DRAW_PACKET_VERSION: u32 = 1;
pub const EVENT_RECORD_BYTES: usize = 64;

/// Maximum number of gradient stops accepted in one paint command.
pub const MAX_GRADIENT_STOPS: usize = 64;
/// Maximum number of affine transforms kept on the transform stack.
pub const MAX_TRANSFORM_DEPTH: usize = 64;
/// Maximum number of nested opacity layers pushed onto the opacity stack.
pub const MAX_OPACITY_DEPTH: usize = 64;
/// Maximum number of SVG path verbs accepted in one command.
pub const MAX_PATH_VERBS: usize = 250_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum CursorKind {
    #[default]
    Default = 0,
    Pointer = 1,
    Text = 2,
    Move = 3,
    Grab = 4,
    Grabbing = 5,
    ResizeEW = 6,
    ResizeNS = 7,
    NotAllowed = 8,
    Wait = 9,
}

impl TryFrom<i64> for CursorKind {
    type Error = ProtocolError;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Default),
            1 => Ok(Self::Pointer),
            2 => Ok(Self::Text),
            3 => Ok(Self::Move),
            4 => Ok(Self::Grab),
            5 => Ok(Self::Grabbing),
            6 => Ok(Self::ResizeEW),
            7 => Ok(Self::ResizeNS),
            8 => Ok(Self::NotAllowed),
            9 => Ok(Self::Wait),
            other => Err(ProtocolError::InvalidCursorKind(other)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum HostMode {
    NativeWindow = 0,
    Headless = 1,
}

impl TryFrom<i64> for HostMode {
    type Error = ProtocolError;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::NativeWindow),
            1 => Ok(Self::Headless),
            other => Err(ProtocolError::InvalidHostMode(other)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum EventKind {
    Resized = 1,
    CloseRequested = 2,
    PointerMove = 3,
    PointerDown = 4,
    PointerUp = 5,
    Wheel = 6,
    KeyDown = 7,
    KeyUp = 8,
    TextInput = 9,
    FocusChanged = 10,
    RedrawRequested = 11,
    ScaleChanged = 12,
    FileDropped = 13,
    PointerLeave = 14,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RgbaColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl RgbaColor {
    pub const fn from_packed_u32(packed: u32) -> Self {
        let [r, g, b, a] = packed.to_le_bytes();
        Self { r, g, b, a }
    }

    pub const fn to_packed_u32(self) -> u32 {
        u32::from_le_bytes([self.r, self.g, self.b, self.a])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum TextAlign {
    #[default]
    Left = 0,
    Center = 1,
    Right = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextFlags {
    pub bold: bool,
    pub monospace: bool,
    pub italic: bool,
    pub align: TextAlign,
    pub ellipsis: bool,
}

impl TextFlags {
    pub const fn from_bits(bits: i32) -> Self {
        let align = match (bits >> 3) & 3 {
            1 => TextAlign::Center,
            2 => TextAlign::Right,
            _ => TextAlign::Left,
        };
        Self {
            bold: (bits & 1) != 0,
            monospace: (bits & 2) != 0,
            italic: (bits & 4) != 0,
            align,
            ellipsis: (bits & 32) != 0,
        }
    }

    pub const fn to_bits(self) -> i32 {
        (if self.bold { 1 } else { 0 })
            | (if self.monospace { 2 } else { 0 })
            | (if self.italic { 4 } else { 0 })
            | ((self.align as i32) << 3)
            | (if self.ellipsis { 32 } else { 0 })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct A11yCommand {
    pub node_id: u64,
    pub parent_id: u64,
    pub role: i32,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub state_flags: i32,
    pub label: String,
}

/// A single verb in an arbitrary vector path. Coordinates are logical pixels;
/// the enclosing `FillPath` transform maps them into surface space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PathVerb {
    MoveTo(f32, f32),
    LineTo(f32, f32),
    QuadTo(f32, f32, f32, f32),
    CubicTo(f32, f32, f32, f32, f32, f32),
    Close,
}

/// Affine transform in row-major order: `[a, b, c, d, e, f]` maps
/// `(x, y)` to `(a*x + c*y + e, b*x + d*y + f)`.
pub type AffineTransform = [f32; 6];

#[derive(Debug, Clone, PartialEq)]
pub enum DrawCmd {
    FillRoundRect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        fill: RgbaColor,
        stroke: RgbaColor,
        stroke_width: f32,
    },
    Line {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        stroke: RgbaColor,
        stroke_width: f32,
        dashed: bool,
    },
    Circle {
        cx: f32,
        cy: f32,
        radius: f32,
        fill: RgbaColor,
        stroke: RgbaColor,
        stroke_width: f32,
    },
    Polyline {
        stroke: RgbaColor,
        stroke_width: f32,
        fill_under: RgbaColor,
        points: Vec<(f32, f32)>,
    },
    PushClip {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    },
    PopClip,
    Text {
        x: f32,
        y: f32,
        max_w: f32,
        font_size: f32,
        line_height: f32,
        color: RgbaColor,
        flags: TextFlags,
        text: String,
    },
    ImageRgba {
        dx: f32,
        dy: f32,
        dw: f32,
        dh: f32,
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    },
    A11yNode(A11yCommand),

    /// Rounded rectangle with four independent corner radii (tl, tr, br, bl).
    FillRoundRectCorners {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radii: [f32; 4],
        fill: RgbaColor,
        stroke: RgbaColor,
        stroke_width: f32,
    },
    /// Axis-aligned linear gradient painted over a (possibly rounded) rect.
    LinearGradientRect {
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
        stops: Vec<(f32, RgbaColor)>,
    },
    /// Radial gradient painted over a (possibly rounded) rect.
    RadialGradientRect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radii: [f32; 4],
        cx: f32,
        cy: f32,
        radius: f32,
        spread: i32,
        stops: Vec<(f32, RgbaColor)>,
    },
    /// Blurred, offset rounded-rectangle silhouette painted behind later commands.
    BoxShadow {
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
    },
    /// Arbitrary vector path (line/quad/cubic verbs) with optional fill/stroke.
    FillPath {
        transform: AffineTransform,
        verbs: Vec<PathVerb>,
        fill: RgbaColor,
        stroke: RgbaColor,
        stroke_width: f32,
        fill_rule: i32,
    },
    /// Push an affine transform onto the renderer transform stack.
    PushTransform(AffineTransform),
    /// Pop the most recently pushed transform.
    PopTransform,
    /// SVG path-data string rasterised with the supplied paint into a viewport.
    SvgPath {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        view_w: f32,
        view_h: f32,
        transform: AffineTransform,
        path_data: String,
        fill: RgbaColor,
        stroke: RgbaColor,
        stroke_width: f32,
    },
    /// Push a rounded-corner clip region onto the renderer clip stack.
    PushRoundClip {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radii: [f32; 4],
    },
    /// Push a multiplicative opacity layer (`0.0..=1.0`) onto the opacity stack.
    PushOpacity(f32),
    /// Pop the most recently pushed opacity layer.
    PopOpacity,
    /// Circular vector arc stroked with smooth cubic Bezier segments.
    Arc {
        cx: f32,
        cy: f32,
        radius: f32,
        start_angle: f32,
        sweep_angle: f32,
        stroke: RgbaColor,
        stroke_width: f32,
        line_cap: i32,
    },
    /// Live or simulated camera feed frame blitted into `(x, y, w, h)`.
    /// `flags & 1`: horizontal mirror; `(flags >> 1) & 3`: tint mode (0=RGB, 1=NVG, 2=FLIR, 3=Slate).
    CameraFrame {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        cam_id: i32,
        flags: i32,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct DrawPacket {
    pub clear_color: RgbaColor,
    pub commands: Vec<DrawCmd>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MeasureTextRequest {
    pub font_size: f32,
    pub line_height: f32,
    pub max_width: f32,
    pub flags: TextFlags,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UiHostEvent {
    /// 1-based window id. A zero in an incoming record means "primary window".
    pub window_id: i64,
    pub kind: EventKind,
    pub target_or_button: i32,
    pub modifiers: i32,
    pub key_code: i32,
    pub x: f32,
    pub y: f32,
    pub text: String,
}

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("invalid host mode: {0}")]
    InvalidHostMode(i64),
    #[error("packet too short: need at least {needed} bytes, got {actual}")]
    Truncated { needed: usize, actual: usize },
    #[error("invalid draw packet magic: 0x{0:08x}")]
    InvalidMagic(u32),
    #[error("unsupported draw packet version: {0}")]
    UnsupportedVersion(u32),
    #[error("unknown draw opcode {0} at command index {1}")]
    UnknownOpcode(i32, usize),
    #[error("invalid string byte length {0}")]
    InvalidByteLength(i32),
    #[error("invalid UTF-8 string payload in command buffer")]
    InvalidUtf8,
    #[error("invalid image dimensions {width}x{height}")]
    InvalidImageDimensions { width: i32, height: i32 },
    #[error("unknown event kind: {0}")]
    UnknownEventKind(i32),
    #[error("invalid gradient stop count {0}")]
    InvalidGradientStops(i32),
    #[error("invalid path verb {0}")]
    InvalidPathVerb(i32),
    #[error("transform stack underflow")]
    TransformStackUnderflow,
    #[error("invalid cursor kind: {0}")]
    InvalidCursorKind(i64),
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn read_bytes(&mut self, n: usize) -> Result<&'a [u8], ProtocolError> {
        let end = self.offset.checked_add(n).ok_or(ProtocolError::Truncated {
            needed: usize::MAX,
            actual: self.bytes.len(),
        })?;
        if end > self.bytes.len() {
            return Err(ProtocolError::Truncated {
                needed: end,
                actual: self.bytes.len(),
            });
        }
        let slice = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(slice)
    }

    fn read_u32(&mut self) -> Result<u32, ProtocolError> {
        let b = self.read_bytes(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn read_i32(&mut self) -> Result<i32, ProtocolError> {
        Ok(self.read_u32()? as i32)
    }

    fn read_f32(&mut self) -> Result<f32, ProtocolError> {
        Ok(f32::from_bits(self.read_u32()?))
    }

    fn read_raw_str(&mut self, byte_len: i32) -> Result<String, ProtocolError> {
        if byte_len < 0 || byte_len as usize > 4 * 1024 * 1024 {
            return Err(ProtocolError::InvalidByteLength(byte_len));
        }
        let raw = self.read_bytes(byte_len as usize)?;
        let s = std::str::from_utf8(raw).map_err(|_| ProtocolError::InvalidUtf8)?;
        Ok(s.to_owned())
    }

    fn read_padded_str(&mut self, byte_len: i32) -> Result<String, ProtocolError> {
        if byte_len < 0 || byte_len as usize > 4 * 1024 * 1024 {
            return Err(ProtocolError::InvalidByteLength(byte_len));
        }
        let len = byte_len as usize;
        let padded = (len + 3) & !3;
        let raw = self.read_bytes(padded)?;
        let s = std::str::from_utf8(&raw[..len]).map_err(|_| ProtocolError::InvalidUtf8)?;
        Ok(s.to_owned())
    }
}

pub fn decode_draw_packet(bytes: &[u8]) -> Result<DrawPacket, ProtocolError> {
    let mut cur = Cursor::new(bytes);
    let magic = cur.read_u32()?;
    if magic != DRAW_PACKET_MAGIC {
        return Err(ProtocolError::InvalidMagic(magic));
    }
    let version = cur.read_u32()?;
    if version != DRAW_PACKET_VERSION {
        return Err(ProtocolError::UnsupportedVersion(version));
    }
    let clear_color = RgbaColor::from_packed_u32(cur.read_u32()?);
    let cmd_count = cur.read_i32()?;
    if cmd_count < 0 || cmd_count as usize > 1_000_000 {
        return Err(ProtocolError::InvalidByteLength(cmd_count));
    }
    let count = cmd_count as usize;
    let mut commands = Vec::with_capacity(count.min(4096));
    for idx in 0..count {
        commands.push(decode_draw_cmd(&mut cur, idx)?);
    }
    Ok(DrawPacket {
        clear_color,
        commands,
    })
}

fn decode_draw_cmd(cur: &mut Cursor<'_>, idx: usize) -> Result<DrawCmd, ProtocolError> {
    let opcode = cur.read_i32()?;
    match opcode {
        1 => Ok(DrawCmd::FillRoundRect {
            x: cur.read_f32()?,
            y: cur.read_f32()?,
            w: cur.read_f32()?,
            h: cur.read_f32()?,
            radius: cur.read_f32()?,
            fill: RgbaColor::from_packed_u32(cur.read_u32()?),
            stroke: RgbaColor::from_packed_u32(cur.read_u32()?),
            stroke_width: cur.read_f32()?,
        }),
        2 => Ok(DrawCmd::Line {
            x1: cur.read_f32()?,
            y1: cur.read_f32()?,
            x2: cur.read_f32()?,
            y2: cur.read_f32()?,
            stroke: RgbaColor::from_packed_u32(cur.read_u32()?),
            stroke_width: cur.read_f32()?,
            dashed: cur.read_i32()? != 0,
        }),
        3 => Ok(DrawCmd::Circle {
            cx: cur.read_f32()?,
            cy: cur.read_f32()?,
            radius: cur.read_f32()?,
            fill: RgbaColor::from_packed_u32(cur.read_u32()?),
            stroke: RgbaColor::from_packed_u32(cur.read_u32()?),
            stroke_width: cur.read_f32()?,
        }),
        4 => decode_polyline(cur),
        5 => Ok(DrawCmd::PushClip {
            x: cur.read_f32()?,
            y: cur.read_f32()?,
            w: cur.read_f32()?,
            h: cur.read_f32()?,
        }),
        6 => Ok(DrawCmd::PopClip),
        7 => decode_text_cmd(cur),
        8 => decode_image_cmd(cur),
        9 => decode_a11y_cmd(cur),
        10 => decode_round_rect_corners_cmd(cur),
        11 => decode_linear_gradient_cmd(cur),
        12 => decode_radial_gradient_cmd(cur),
        13 => decode_box_shadow_cmd(cur),
        14 => decode_fill_path_cmd(cur),
        15 => Ok(DrawCmd::PushTransform(read_transform(cur)?)),
        16 => Ok(DrawCmd::PopTransform),
        17 => decode_svg_path_cmd(cur),
        18 => Ok(DrawCmd::PushRoundClip {
            x: cur.read_f32()?,
            y: cur.read_f32()?,
            w: cur.read_f32()?,
            h: cur.read_f32()?,
            radii: read_radii(cur)?,
        }),
        19 => Ok(DrawCmd::PushOpacity(cur.read_f32()?.clamp(0.0, 1.0))),
        20 => Ok(DrawCmd::PopOpacity),
        21 => Ok(DrawCmd::Arc {
            cx: cur.read_f32()?,
            cy: cur.read_f32()?,
            radius: cur.read_f32()?,
            start_angle: cur.read_f32()?,
            sweep_angle: cur.read_f32()?,
            stroke: RgbaColor::from_packed_u32(cur.read_u32()?),
            stroke_width: cur.read_f32()?,
            line_cap: cur.read_i32()?,
        }),
        22 => Ok(DrawCmd::CameraFrame {
            x: cur.read_f32()?,
            y: cur.read_f32()?,
            w: cur.read_f32()?,
            h: cur.read_f32()?,
            cam_id: cur.read_i32()?,
            flags: cur.read_i32()?,
        }),
        other => Err(ProtocolError::UnknownOpcode(other, idx)),
    }
}

fn read_radii(cur: &mut Cursor<'_>) -> Result<[f32; 4], ProtocolError> {
    Ok([
        cur.read_f32()?,
        cur.read_f32()?,
        cur.read_f32()?,
        cur.read_f32()?,
    ])
}

fn read_transform(cur: &mut Cursor<'_>) -> Result<AffineTransform, ProtocolError> {
    Ok([
        cur.read_f32()?,
        cur.read_f32()?,
        cur.read_f32()?,
        cur.read_f32()?,
        cur.read_f32()?,
        cur.read_f32()?,
    ])
}

fn decode_stops(cur: &mut Cursor<'_>) -> Result<Vec<(f32, RgbaColor)>, ProtocolError> {
    let count = cur.read_i32()?;
    if count <= 0 || count as usize > MAX_GRADIENT_STOPS {
        return Err(ProtocolError::InvalidGradientStops(count));
    }
    let mut stops = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let offset = cur.read_f32()?;
        let color = RgbaColor::from_packed_u32(cur.read_u32()?);
        stops.push((offset, color));
    }
    Ok(stops)
}

fn decode_round_rect_corners_cmd(cur: &mut Cursor<'_>) -> Result<DrawCmd, ProtocolError> {
    let x = cur.read_f32()?;
    let y = cur.read_f32()?;
    let w = cur.read_f32()?;
    let h = cur.read_f32()?;
    let radii = read_radii(cur)?;
    let fill = RgbaColor::from_packed_u32(cur.read_u32()?);
    let stroke = RgbaColor::from_packed_u32(cur.read_u32()?);
    let stroke_width = cur.read_f32()?;
    Ok(DrawCmd::FillRoundRectCorners {
        x,
        y,
        w,
        h,
        radii,
        fill,
        stroke,
        stroke_width,
    })
}

fn decode_linear_gradient_cmd(cur: &mut Cursor<'_>) -> Result<DrawCmd, ProtocolError> {
    let x = cur.read_f32()?;
    let y = cur.read_f32()?;
    let w = cur.read_f32()?;
    let h = cur.read_f32()?;
    let radii = read_radii(cur)?;
    let x0 = cur.read_f32()?;
    let y0 = cur.read_f32()?;
    let x1 = cur.read_f32()?;
    let y1 = cur.read_f32()?;
    let spread = cur.read_i32()?;
    let stops = decode_stops(cur)?;
    Ok(DrawCmd::LinearGradientRect {
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
    })
}

fn decode_radial_gradient_cmd(cur: &mut Cursor<'_>) -> Result<DrawCmd, ProtocolError> {
    let x = cur.read_f32()?;
    let y = cur.read_f32()?;
    let w = cur.read_f32()?;
    let h = cur.read_f32()?;
    let radii = read_radii(cur)?;
    let cx = cur.read_f32()?;
    let cy = cur.read_f32()?;
    let radius = cur.read_f32()?;
    let spread = cur.read_i32()?;
    let stops = decode_stops(cur)?;
    Ok(DrawCmd::RadialGradientRect {
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
    })
}

fn decode_box_shadow_cmd(cur: &mut Cursor<'_>) -> Result<DrawCmd, ProtocolError> {
    let x = cur.read_f32()?;
    let y = cur.read_f32()?;
    let w = cur.read_f32()?;
    let h = cur.read_f32()?;
    let radii = read_radii(cur)?;
    let offset_x = cur.read_f32()?;
    let offset_y = cur.read_f32()?;
    let blur = cur.read_f32()?;
    let spread = cur.read_f32()?;
    let color = RgbaColor::from_packed_u32(cur.read_u32()?);
    Ok(DrawCmd::BoxShadow {
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
    })
}

fn decode_verbs(cur: &mut Cursor<'_>) -> Result<Vec<PathVerb>, ProtocolError> {
    let count = cur.read_i32()?;
    if count < 0 || count as usize > MAX_PATH_VERBS {
        return Err(ProtocolError::InvalidByteLength(count));
    }
    let mut verbs = Vec::with_capacity((count as usize).min(4096));
    for _ in 0..count {
        let kind = cur.read_i32()?;
        let verb = match kind {
            1 => PathVerb::MoveTo(cur.read_f32()?, cur.read_f32()?),
            2 => PathVerb::LineTo(cur.read_f32()?, cur.read_f32()?),
            3 => PathVerb::QuadTo(
                cur.read_f32()?,
                cur.read_f32()?,
                cur.read_f32()?,
                cur.read_f32()?,
            ),
            4 => PathVerb::CubicTo(
                cur.read_f32()?,
                cur.read_f32()?,
                cur.read_f32()?,
                cur.read_f32()?,
                cur.read_f32()?,
                cur.read_f32()?,
            ),
            5 => PathVerb::Close,
            other => return Err(ProtocolError::InvalidPathVerb(other)),
        };
        verbs.push(verb);
    }
    Ok(verbs)
}

fn decode_fill_path_cmd(cur: &mut Cursor<'_>) -> Result<DrawCmd, ProtocolError> {
    let transform = read_transform(cur)?;
    let verbs = decode_verbs(cur)?;
    let fill = RgbaColor::from_packed_u32(cur.read_u32()?);
    let stroke = RgbaColor::from_packed_u32(cur.read_u32()?);
    let stroke_width = cur.read_f32()?;
    let fill_rule = cur.read_i32()?;
    Ok(DrawCmd::FillPath {
        transform,
        verbs,
        fill,
        stroke,
        stroke_width,
        fill_rule,
    })
}

fn decode_svg_path_cmd(cur: &mut Cursor<'_>) -> Result<DrawCmd, ProtocolError> {
    let x = cur.read_f32()?;
    let y = cur.read_f32()?;
    let w = cur.read_f32()?;
    let h = cur.read_f32()?;
    let view_w = cur.read_f32()?;
    let view_h = cur.read_f32()?;
    let transform = read_transform(cur)?;
    let byte_len = cur.read_i32()?;
    let path_data = cur.read_padded_str(byte_len)?;
    let fill = RgbaColor::from_packed_u32(cur.read_u32()?);
    let stroke = RgbaColor::from_packed_u32(cur.read_u32()?);
    let stroke_width = cur.read_f32()?;
    Ok(DrawCmd::SvgPath {
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
    })
}

fn decode_polyline(cur: &mut Cursor<'_>) -> Result<DrawCmd, ProtocolError> {
    let count = cur.read_i32()?;
    if count < 0 || count as usize > 250_000 {
        return Err(ProtocolError::InvalidByteLength(count));
    }
    let stroke = RgbaColor::from_packed_u32(cur.read_u32()?);
    let stroke_width = cur.read_f32()?;
    let fill_under = RgbaColor::from_packed_u32(cur.read_u32()?);
    let mut points = Vec::with_capacity((count as usize).min(4096));
    for _ in 0..count {
        let px = cur.read_f32()?;
        let py = cur.read_f32()?;
        points.push((px, py));
    }
    Ok(DrawCmd::Polyline {
        stroke,
        stroke_width,
        fill_under,
        points,
    })
}

fn decode_text_cmd(cur: &mut Cursor<'_>) -> Result<DrawCmd, ProtocolError> {
    let x = cur.read_f32()?;
    let y = cur.read_f32()?;
    let max_w = cur.read_f32()?;
    let font_size = cur.read_f32()?;
    let line_height = cur.read_f32()?;
    let color = RgbaColor::from_packed_u32(cur.read_u32()?);
    let flags = TextFlags::from_bits(cur.read_i32()?);
    let byte_len = cur.read_i32()?;
    let text = cur.read_padded_str(byte_len)?;
    Ok(DrawCmd::Text {
        x,
        y,
        max_w,
        font_size,
        line_height,
        color,
        flags,
        text,
    })
}

fn decode_image_cmd(cur: &mut Cursor<'_>) -> Result<DrawCmd, ProtocolError> {
    let dx = cur.read_f32()?;
    let dy = cur.read_f32()?;
    let dw = cur.read_f32()?;
    let dh = cur.read_f32()?;
    let width = cur.read_i32()?;
    let height = cur.read_i32()?;
    if width <= 0 || height <= 0 || width > 4096 || height > 4096 {
        return Err(ProtocolError::InvalidImageDimensions { width, height });
    }
    let pixel_count = (width as usize) * (height as usize);
    let byte_count = pixel_count * 4;
    let raw = cur.read_bytes(byte_count)?;
    Ok(DrawCmd::ImageRgba {
        dx,
        dy,
        dw,
        dh,
        width: width as u32,
        height: height as u32,
        pixels: raw.to_vec(),
    })
}

fn decode_a11y_cmd(cur: &mut Cursor<'_>) -> Result<DrawCmd, ProtocolError> {
    let node_id = cur.read_u32()? as u64;
    let parent_id = cur.read_u32()? as u64;
    let role = cur.read_i32()?;
    let x = cur.read_f32()?;
    let y = cur.read_f32()?;
    let w = cur.read_f32()?;
    let h = cur.read_f32()?;
    let state_flags = cur.read_i32()?;
    let label_len = cur.read_i32()?;
    let label = cur.read_padded_str(label_len)?;
    Ok(DrawCmd::A11yNode(A11yCommand {
        node_id,
        parent_id,
        role,
        x,
        y,
        w,
        h,
        state_flags,
        label,
    }))
}

pub fn decode_measure_request(bytes: &[u8]) -> Result<MeasureTextRequest, ProtocolError> {
    let mut cur = Cursor::new(bytes);
    let font_size = cur.read_f32()?;
    let line_height = cur.read_f32()?;
    let max_width = cur.read_f32()?;
    let flags = TextFlags::from_bits(cur.read_i32()?);
    let byte_len = cur.read_i32()?;
    let text = cur.read_raw_str(byte_len)?;
    Ok(MeasureTextRequest {
        font_size,
        line_height,
        max_width,
        flags,
        text,
    })
}

pub fn encode_event_record(ev: &UiHostEvent, out: &mut [u8; EVENT_RECORD_BYTES]) {
    out.fill(0);
    write_i32(out, 0, ev.kind as i32);
    write_i32(out, 4, ev.target_or_button);
    if matches!(ev.kind, EventKind::TextInput | EventKind::FileDropped) {
        let bytes = ev.text.as_bytes();
        let copy_len = clamp_utf8_prefix(bytes, 32);
        write_i32(out, 8, copy_len as i32);
        out[24..24 + copy_len].copy_from_slice(&bytes[..copy_len]);
    } else {
        write_i32(out, 8, ev.modifiers);
    }
    write_i32(out, 12, ev.key_code);
    write_f32(out, 16, ev.x);
    write_f32(out, 20, ev.y);
    // Bytes 56..64 are free (text payload ends at 56). Older records leave
    // this zero, which decode treats as the primary window.
    write_i32(out, 56, ev.window_id as i32);
}

pub fn decode_event_record(bytes: &[u8]) -> Result<UiHostEvent, ProtocolError> {
    if bytes.len() < EVENT_RECORD_BYTES {
        return Err(ProtocolError::Truncated {
            needed: EVENT_RECORD_BYTES,
            actual: bytes.len(),
        });
    }
    let kind_raw = read_i32_at(bytes, 0);
    let kind = match kind_raw {
        1 => EventKind::Resized,
        2 => EventKind::CloseRequested,
        3 => EventKind::PointerMove,
        4 => EventKind::PointerDown,
        5 => EventKind::PointerUp,
        6 => EventKind::Wheel,
        7 => EventKind::KeyDown,
        8 => EventKind::KeyUp,
        9 => EventKind::TextInput,
        10 => EventKind::FocusChanged,
        11 => EventKind::RedrawRequested,
        12 => EventKind::ScaleChanged,
        13 => EventKind::FileDropped,
        14 => EventKind::PointerLeave,
        other => return Err(ProtocolError::UnknownEventKind(other)),
    };
    let target_or_button = read_i32_at(bytes, 4);
    let modifiers_or_len = read_i32_at(bytes, 8);
    let key_code = read_i32_at(bytes, 12);
    let x = f32::from_bits(read_i32_at(bytes, 16) as u32);
    let y = f32::from_bits(read_i32_at(bytes, 20) as u32);
    let (modifiers, text) = if matches!(kind, EventKind::TextInput | EventKind::FileDropped) {
        let len = (modifiers_or_len.max(0) as usize).min(32);
        let s =
            std::str::from_utf8(&bytes[24..24 + len]).map_err(|_| ProtocolError::InvalidUtf8)?;
        (0, s.to_owned())
    } else {
        (modifiers_or_len, String::new())
    };
    let window_id = read_i32_at(bytes, 56);
    Ok(UiHostEvent {
        window_id: if window_id > 0 { window_id as i64 } else { 1 },
        kind,
        target_or_button,
        modifiers,
        key_code,
        x,
        y,
        text,
    })
}

fn clamp_utf8_prefix(bytes: &[u8], max_bytes: usize) -> usize {
    if bytes.len() <= max_bytes {
        return bytes.len();
    }
    let mut end = max_bytes;
    while end > 0 && (bytes[end] & 0xC0) == 0x80 {
        end -= 1;
    }
    end
}

fn write_i32(out: &mut [u8], off: usize, v: i32) {
    out[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn write_f32(out: &mut [u8], off: usize, v: f32) {
    out[off..off + 4].copy_from_slice(&v.to_bits().to_le_bytes());
}

fn read_i32_at(bytes: &[u8], off: usize) -> i32 {
    i32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]])
}
