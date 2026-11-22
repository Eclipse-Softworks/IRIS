//! Minimal SVG path-data parser used by the native renderer's `SvgPath` opcode.
//!
//! It recognizes the standard `M/m L/l H/h V/v C/c S/s Q/q T/t A/a Z/z`
//! commands (including implicit repeats) and converts them into the renderer's
//! `PathVerb` stream. Elliptical arcs are sampled into line segments so the
//! whole parser stays dependency-free and works on every supported target.

use crate::protocol::PathVerb;

struct Parser<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Self { src, pos: 0 }
    }

    fn skip_separators(&mut self) {
        let bytes = self.src.as_bytes();
        while self.pos < bytes.len()
            && (bytes[self.pos].is_ascii_whitespace() || bytes[self.pos] == b',')
        {
            self.pos += 1;
        }
    }

    fn peek_command(&mut self) -> Option<(char, bool)> {
        self.skip_separators();
        let bytes = self.src.as_bytes();
        let b = *bytes.get(self.pos)?;
        if b.is_ascii_alphabetic() {
            self.pos += 1;
            let c = b as char;
            Some((c.to_ascii_uppercase(), c.is_ascii_lowercase()))
        } else {
            None
        }
    }

    fn has_number(&mut self) -> bool {
        self.skip_separators();
        let bytes = self.src.as_bytes();
        match bytes.get(self.pos) {
            Some(b) => b.is_ascii_digit() || *b == b'.' || *b == b'+' || *b == b'-',
            None => false,
        }
    }

    fn flag(&mut self) -> Option<bool> {
        self.skip_separators();
        let bytes = self.src.as_bytes();
        match bytes.get(self.pos) {
            Some(b'0') => {
                self.pos += 1;
                Some(false)
            }
            Some(b'1') => {
                self.pos += 1;
                Some(true)
            }
            _ => None,
        }
    }

    fn number(&mut self) -> Option<f32> {
        self.skip_separators();
        let bytes = self.src.as_bytes();
        let start = self.pos;
        let mut i = start;
        if matches!(bytes.get(i), Some(b'+') | Some(b'-')) {
            i += 1;
        }
        let mut digits = 0;
        while let Some(b) = bytes.get(i) {
            if b.is_ascii_digit() {
                digits += 1;
                i += 1;
            } else {
                break;
            }
        }
        if bytes.get(i) == Some(&b'.') {
            i += 1;
            while let Some(b) = bytes.get(i) {
                if b.is_ascii_digit() {
                    digits += 1;
                    i += 1;
                } else {
                    break;
                }
            }
        }
        if digits == 0 {
            return None;
        }
        if matches!(bytes.get(i), Some(b'e') | Some(b'E')) {
            let mut j = i + 1;
            if matches!(bytes.get(j), Some(b'+') | Some(b'-')) {
                j += 1;
            }
            let exp_start = j;
            while let Some(b) = bytes.get(j) {
                if b.is_ascii_digit() {
                    j += 1;
                } else {
                    break;
                }
            }
            if j > exp_start {
                i = j;
            }
        }
        let text = &self.src[start..i];
        self.pos = i;
        text.parse::<f32>().ok()
    }
}

/// Parse an SVG `d` attribute into renderer path verbs.
pub fn parse_path_data(data: &str) -> Vec<PathVerb> {
    let mut p = Parser::new(data);
    let mut verbs = Vec::new();
    let mut active: Option<(char, bool)> = None;
    let (mut cx, mut cy) = (0.0f32, 0.0f32);
    let (mut start_x, mut start_y) = (0.0f32, 0.0f32);
    let (mut ctrl_x, mut ctrl_y) = (0.0f32, 0.0f32);
    let mut ctrl_kind = ' ';

    loop {
        if let Some(cmd) = p.peek_command() {
            active = Some(cmd);
            if cmd.0 == 'Z' {
                verbs.push(PathVerb::Close);
                cx = start_x;
                cy = start_y;
                active = None;
                ctrl_kind = ' ';
                continue;
            }
        }
        let Some((letter, relative)) = active else {
            break;
        };
        if !p.has_number() {
            break;
        }
        let ox = if relative { cx } else { 0.0 };
        let oy = if relative { cy } else { 0.0 };
        match letter {
            'M' => {
                let (Some(x), Some(y)) = (p.number(), p.number()) else {
                    break;
                };
                cx = x + ox;
                cy = y + oy;
                start_x = cx;
                start_y = cy;
                verbs.push(PathVerb::MoveTo(cx, cy));
                // Additional coordinate pairs after M are implicit line-tos.
                active = Some(('L', relative));
                ctrl_kind = ' ';
            }
            'L' => {
                let (Some(x), Some(y)) = (p.number(), p.number()) else {
                    break;
                };
                cx = x + ox;
                cy = y + oy;
                verbs.push(PathVerb::LineTo(cx, cy));
                ctrl_kind = ' ';
            }
            'H' => {
                let Some(x) = p.number() else { break };
                cx = x + ox;
                verbs.push(PathVerb::LineTo(cx, cy));
                ctrl_kind = ' ';
            }
            'V' => {
                let Some(y) = p.number() else { break };
                cy = y + oy;
                verbs.push(PathVerb::LineTo(cx, cy));
                ctrl_kind = ' ';
            }
            'C' => {
                let (Some(x1), Some(y1), Some(x2), Some(y2), Some(x), Some(y)) = (
                    p.number(),
                    p.number(),
                    p.number(),
                    p.number(),
                    p.number(),
                    p.number(),
                ) else {
                    break;
                };
                let (ax, ay, bx, by) = (x1 + ox, y1 + oy, x2 + ox, y2 + oy);
                cx = x + ox;
                cy = y + oy;
                verbs.push(PathVerb::CubicTo(ax, ay, bx, by, cx, cy));
                ctrl_x = bx;
                ctrl_y = by;
                ctrl_kind = 'C';
            }
            'S' => {
                let (Some(x2), Some(y2), Some(x), Some(y)) =
                    (p.number(), p.number(), p.number(), p.number())
                else {
                    break;
                };
                let reflect = if ctrl_kind == 'C' || ctrl_kind == 'S' {
                    (2.0 * cx - ctrl_x, 2.0 * cy - ctrl_y)
                } else {
                    (cx, cy)
                };
                let (bx, by) = (x2 + ox, y2 + oy);
                cx = x + ox;
                cy = y + oy;
                verbs.push(PathVerb::CubicTo(reflect.0, reflect.1, bx, by, cx, cy));
                ctrl_x = bx;
                ctrl_y = by;
                ctrl_kind = 'S';
            }
            'Q' => {
                let (Some(x1), Some(y1), Some(x), Some(y)) =
                    (p.number(), p.number(), p.number(), p.number())
                else {
                    break;
                };
                let (ax, ay) = (x1 + ox, y1 + oy);
                cx = x + ox;
                cy = y + oy;
                verbs.push(PathVerb::QuadTo(ax, ay, cx, cy));
                ctrl_x = ax;
                ctrl_y = ay;
                ctrl_kind = 'Q';
            }
            'T' => {
                let (Some(x), Some(y)) = (p.number(), p.number()) else {
                    break;
                };
                let reflect = if ctrl_kind == 'Q' || ctrl_kind == 'T' {
                    (2.0 * cx - ctrl_x, 2.0 * cy - ctrl_y)
                } else {
                    (cx, cy)
                };
                cx = x + ox;
                cy = y + oy;
                verbs.push(PathVerb::QuadTo(reflect.0, reflect.1, cx, cy));
                ctrl_x = reflect.0;
                ctrl_y = reflect.1;
                ctrl_kind = 'T';
            }
            'A' => {
                let (Some(rx), Some(ry), Some(rot), Some(large), Some(sweep), Some(x), Some(y)) = (
                    p.number(),
                    p.number(),
                    p.number(),
                    p.flag(),
                    p.flag(),
                    p.number(),
                    p.number(),
                ) else {
                    break;
                };
                let (ex, ey) = (x + ox, y + oy);
                let end = arc_samples(cx, cy, rx, ry, rot, large, sweep, ex, ey);
                for (px, py) in end {
                    verbs.push(PathVerb::LineTo(px, py));
                }
                cx = ex;
                cy = ey;
                ctrl_kind = ' ';
            }
            _ => break,
        }
    }
    verbs
}

fn angle_between(ux: f32, uy: f32, vx: f32, vy: f32) -> f32 {
    let dot = ux * vx + uy * vy;
    let cross = ux * vy - uy * vx;
    cross.atan2(dot)
}

#[allow(clippy::too_many_arguments)]
fn arc_samples(
    x0: f32,
    y0: f32,
    rx: f32,
    ry: f32,
    rotation_deg: f32,
    large_arc: bool,
    sweep: bool,
    x1: f32,
    y1: f32,
) -> Vec<(f32, f32)> {
    if rx.abs() < f32::EPSILON
        || ry.abs() < f32::EPSILON
        || (x0 - x1).abs() + (y0 - y1).abs() < f32::EPSILON
    {
        return vec![(x1, y1)];
    }
    let phi = rotation_deg.to_radians();
    let (cos_phi, sin_phi) = (phi.cos(), phi.sin());
    let dx2 = (x0 - x1) * 0.5;
    let dy2 = (y0 - y1) * 0.5;
    let x1p = cos_phi * dx2 + sin_phi * dy2;
    let y1p = -sin_phi * dx2 + cos_phi * dy2;
    let mut rx = rx.abs();
    let mut ry = ry.abs();
    let lambda = x1p * x1p / (rx * rx) + y1p * y1p / (ry * ry);
    if lambda > 1.0 {
        let s = lambda.sqrt();
        rx *= s;
        ry *= s;
    }
    let numerator = (rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p).max(0.0);
    let denominator = rx * rx * y1p * y1p + ry * ry * x1p * x1p;
    let coef = if denominator > 0.0 {
        (numerator / denominator).sqrt()
    } else {
        0.0
    } * if large_arc == sweep { -1.0 } else { 1.0 };
    let cxp = coef * rx * y1p / ry;
    let cyp = coef * -ry * x1p / rx;
    let cx = cos_phi * cxp - sin_phi * cyp + (x0 + x1) * 0.5;
    let cy = sin_phi * cxp + cos_phi * cyp + (y0 + y1) * 0.5;
    let ux = (x1p - cxp) / rx;
    let uy = (y1p - cyp) / ry;
    let vx = (-x1p - cxp) / rx;
    let vy = (-y1p - cyp) / ry;
    let mut delta = angle_between(ux, uy, vx, vy);
    if !sweep && delta > 0.0 {
        delta -= std::f32::consts::TAU;
    } else if sweep && delta < 0.0 {
        delta += std::f32::consts::TAU;
    }
    let step_angle = std::f32::consts::PI / 6.0;
    let steps = ((delta.abs() / step_angle).ceil() as usize).clamp(4, 128);
    let mut out = Vec::with_capacity(steps);
    for i in 1..=steps {
        if i == steps {
            out.push((x1, y1));
            continue;
        }
        let t = delta * (i as f32) / (steps as f32);
        let (st, ct) = t.sin_cos();
        let px = rx * (ux * ct - uy * st) + cxp;
        let py = ry * (ux * st + uy * ct) + cyp;
        out.push((
            cos_phi * px - sin_phi * py + cx,
            sin_phi * px + cos_phi * py + cy,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_absolute_and_relative_commands() {
        let verbs = parse_path_data("M0 0 L10 0 l0 10 h-10 Z");
        assert_eq!(verbs.len(), 5);
        assert_eq!(verbs[0], PathVerb::MoveTo(0.0, 0.0));
        assert_eq!(verbs[1], PathVerb::LineTo(10.0, 0.0));
        assert_eq!(verbs[2], PathVerb::LineTo(10.0, 10.0));
        assert_eq!(verbs[3], PathVerb::LineTo(0.0, 10.0));
        assert_eq!(verbs[4], PathVerb::Close);
    }

    #[test]
    fn implicit_line_repeats_and_exponents() {
        let verbs = parse_path_data("M 0,0 L 1e1 0 20 0");
        assert_eq!(verbs.len(), 3);
        assert_eq!(verbs[1], PathVerb::LineTo(10.0, 0.0));
        assert_eq!(verbs[2], PathVerb::LineTo(20.0, 0.0));
    }

    #[test]
    fn curves_and_arcs_produce_geometry() {
        let verbs = parse_path_data("M0 0 C1 1 2 2 3 3 S5 5 6 6 Q7 7 8 8 T9 9 A4 4 0 0 1 12 12");
        assert!(verbs.iter().any(|v| matches!(v, PathVerb::CubicTo(..))));
        assert!(verbs.iter().any(|v| matches!(v, PathVerb::QuadTo(..))));
        let lines = verbs
            .iter()
            .filter(|v| matches!(v, PathVerb::LineTo(..)))
            .count();
        assert!(
            lines >= 4,
            "arc should be sampled into several line segments"
        );
    }

    #[test]
    fn compact_arc_flags_and_semicircle_geometry() {
        let verbs = parse_path_data("M2 12a10 10 0 0120 0");
        assert!(verbs.len() >= 5, "compact flags should parse arc cleanly");
        let last = verbs.last().copied();
        assert_eq!(last, Some(PathVerb::LineTo(22.0, 12.0)));
        // Sweep=1 upper semicircle from (2,12) to (22,12) passes through (12, 2).
        let min_y = verbs
            .iter()
            .filter_map(|v| match *v {
                PathVerb::MoveTo(_, y) | PathVerb::LineTo(_, y) => Some(y),
                _ => None,
            })
            .fold(f32::INFINITY, f32::min);
        assert!(
            (min_y - 2.0).abs() < 0.5,
            "expected apex near y=2, got {min_y}"
        );
    }
}
