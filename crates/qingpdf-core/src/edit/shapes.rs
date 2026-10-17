//! Shapes (2026-10-17): `Square` and `Circle` (12.5.6.8) and `Line` (12.5.6.7) annotations, the line with an arrow head if it is an
//! arrow, and the same appearances made again when a shape is recoloured, resized or its line made thicker (`modify.rs`).
//!
//! Like every annotation the reader makes, a shape has an appearance of its own: a form whose `/BBox` is the annotation's `/Rect` and
//! whose matrix is the identity, in default user space, so that every reader draws what we drew. A square or circle has its path
//! inset from `/Rect` by half the line's width (`/RD` says so: 12.5.6.8), a line has a `/Rect` that holds the line, its heads and half
//! the width of its pen.

use super::appearance::{Appearance, color, num, point};
use super::modify::{bad, put_reals, too_much};
use super::{Ctx, MAX_INK_WIDTH, MIN_INK_WIDTH, annotation, finite_place, reals, text_string, unit, with_appearance};
use crate::error::Result;
use crate::object::{Dict, ObjRef, Object};
use crate::text::Shown;

/// The four shapes the reader draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ShapeKind {
    /// A `Square` annotation (any rectangle).
    Rectangle = 0,
    /// A `Circle` annotation (any ellipse).
    Ellipse = 1,
    /// A `Line`.
    Line = 2,
    /// A `Line` whose end has an open arrow head (`/LE [/None /OpenArrow]`, `/IT /LineArrow`).
    Arrow = 3,
}

/// How a line ends (12.5.6.7, `/LE`): nothing, an open arrow head (two barbs) or a closed one (a triangle, filled).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum End {
    Plain,
    Open,
    Closed,
}

/// The shortest side of a shape or line that is drawn, in points.
const MIN_SHAPE: f64 = 2.0;
/// What drawing a shape costs the work meter.
const SHAPE_COST: f64 = 20_000.0;
/// The half angle of an arrow head (radians), and the longest and the shortest its barbs are, in points.
const HEAD_ANGLE: f64 = 0.5;
const HEAD_LONGEST: f64 = 40.0;
const HEAD_SHORTEST: f64 = 8.0;
/// How far a fill is lightened towards white (the part of the way): a filled shape does not hide what is under it.
const LIGHTEN: f64 = 0.7;

/// The colour a shape is filled with: its own, lightened.
pub(super) fn lighten(rgb: [u8; 3]) -> [u8; 3] {
    rgb.map(|c| (f64::from(c) + (255.0 - f64::from(c)) * LIGHTEN).round().clamp(0.0, 255.0) as u8)
}

/// How a shape is drawn: the colour and the width of its line, and the colour of its fill if it has one.
pub(super) struct Look {
    pub rgb: [u8; 3],
    pub width: f64,
    pub fill: Option<[u8; 3]>,
}

/// One operator of a content stream with its numbers.
fn cmd(c: &mut String, args: &[f64], op: &str) {
    for a in args {
        c.push_str(&num(*a));
        c.push(' ');
    }
    c.push_str(op);
    c.push('\n');
}

/// An ellipse as four curves, filling the rectangle `[x0, y0, x1, y1]` (the path only).
fn ellipse(c: &mut String, [x0, y0, x1, y1]: [f64; 4]) {
    const K: f64 = 0.552_284_75;
    let (cx, cy, rx, ry) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0, (x1 - x0) / 2.0, (y1 - y0) / 2.0);
    let (kx, ky) = (rx * K, ry * K);
    cmd(c, &[x1, cy], "m");
    cmd(c, &[x1, cy + ky, cx + kx, y1, cx, y1], "c");
    cmd(c, &[cx - kx, y1, x0, cy + ky, x0, cy], "c");
    cmd(c, &[x0, cy - ky, cx - kx, y0, cx, y0], "c");
    cmd(c, &[cx + kx, y0, x1, cy - ky, x1, cy], "c");
    c.push_str("h\n");
}

/// The appearance of a square or circle whose annotation rectangle is `rect` (user space): the path runs half a line width inside it,
/// so the line fills the rectangle exactly. A fill is painted with the blend mode Multiply, so that what is under shows through.
pub(super) fn boxed(circle: bool, rect: [f64; 4], look: &Look) -> Appearance {
    let h = look.width / 2.0;
    let s = [rect[0] + h, rect[1] + h, rect[2] - h, rect[3] - h];
    let mut c = String::new();
    let path = |c: &mut String| {
        if circle {
            ellipse(c, s);
        } else {
            cmd(c, &[s[0], s[1], s[2] - s[0], s[3] - s[1]], "re");
        }
    };
    if let Some(fill) = look.fill {
        c.push_str("q\n/GS0 gs\n");
        color(&mut c, unit(fill), "rg");
        path(&mut c);
        c.push_str("f\nQ\n");
    }
    color(&mut c, unit(look.rgb), "RG");
    cmd(&mut c, &[look.width], "w");
    path(&mut c);
    c.push_str("S\n");
    Appearance { bbox: rect, content: c, multiply: look.fill.is_some() }
}

/// The two barbs of the head at `tip` of a line that comes from `from`, and the length they have.
fn head_points(tip: (f64, f64), from: (f64, f64), width: f64) -> Option<[(f64, f64); 2]> {
    let (dx, dy) = (tip.0 - from.0, tip.1 - from.1);
    let len = (dx * dx + dy * dy).sqrt();
    if len <= 0.0 {
        return None;
    }
    let (ux, uy) = (dx / len, dy / len);
    let barb = (4.0 * width + 6.0).clamp(HEAD_SHORTEST, HEAD_LONGEST).min(0.6 * len).max(1.0);
    let (back, side) = (barb * HEAD_ANGLE.cos(), barb * HEAD_ANGLE.sin());
    Some([-1.0, 1.0].map(|s| (tip.0 - ux * back - uy * side * s, tip.1 - uy * back + ux * side * s)))
}

/// The appearance of a line from `p` to `q` (user space) with the heads `ends` (at `p` and at `q`). Its box holds the line, the heads and
/// half the width of the pen (round caps and joins).
pub(super) fn line(p: (f64, f64), q: (f64, f64), ends: [End; 2], look: &Look) -> Appearance {
    let mut c = String::new();
    color(&mut c, unit(look.rgb), "RG");
    // (A closed head is filled with the interior colour, `/IC`, if the line has one.)
    color(&mut c, unit(look.fill.unwrap_or(look.rgb)), "rg");
    cmd(&mut c, &[look.width], "w");
    c.push_str("1 J 1 j\n");
    point(&mut c, p, "m");
    point(&mut c, q, "l");
    c.push_str("S\n");
    let mut points = vec![p, q];
    for (tip, from, end) in [(p, q, ends[0]), (q, p, ends[1])] {
        let Some([a, b]) = head_points(tip, from, look.width).filter(|_| end != End::Plain) else { continue };
        point(&mut c, a, "m");
        point(&mut c, tip, "l");
        point(&mut c, b, "l");
        c.push_str(if end == End::Closed { "b\n" } else { "S\n" });
        points.extend([a, b]);
    }
    let pad = look.width / 2.0 + 1.0;
    let b = points.iter().fold([f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY], |m, &(x, y)| [m[0].min(x), m[1].min(y), m[2].max(x), m[3].max(y)]);
    Appearance { bbox: [b[0] - pad, b[1] - pad, b[2] + pad, b[3] + pad], content: c, multiply: false }
}

/// The names `/LE` takes for the heads the reader draws.
pub(super) fn end_name(end: End) -> &'static str {
    match end {
        End::Plain => "None",
        End::Open => "OpenArrow",
        End::Closed => "ClosedArrow",
    }
}

/// What a line's `/LE` name stands for; `None` for a style the reader does not draw.
pub(super) fn end_of(name: &[u8]) -> Option<End> {
    match name {
        b"None" => Some(End::Plain),
        b"OpenArrow" => Some(End::Open),
        b"ClosedArrow" => Some(End::Closed),
        _ => None,
    }
}

/// The pen of an annotation: `/BS /W`, else the border width of `/Border`, within the limits of a drawing; one point if it says nothing.
pub(super) fn clamp_width(w: f64) -> f64 {
    if w.is_finite() { w.clamp(MIN_INK_WIDTH, MAX_INK_WIDTH) } else { 1.0 }
}

/// The entries every shape has in common: the pen (`/BS`), the fill (`/IC`), the colour (`/C`).
pub(super) fn set_look(d: &mut Dict, look: &Look) {
    put_reals(d, "C", &unit(look.rgb));
    let mut border = Dict::new();
    border.set("W", Object::Real(look.width));
    border.set("S", Object::from("S"));
    d.set("BS", Object::Dict(border));
    match look.fill {
        Some(fill) => d.set("IC", reals(&unit(fill))),
        None => {
            d.remove("IC");
        }
    }
}

/// A shape on the page: `from` and `to` are two opposite corners of a rectangle or ellipse, or the two ends of a line (points of the page as
/// shown). Returns the objects it made, the last of which is the annotation.
#[allow(clippy::too_many_arguments)]
pub(super) fn shape(ctx: &mut Ctx<'_>, shown: &Shown, kind: ShapeKind, from: [f32; 2], to: [f32; 2], rgb: [u8; 3], width: f32, fill: bool, author: &str, now: &str) -> Result<Vec<ObjRef>> {
    let width = clamp_width(f64::from(width));
    if !ctx.work.charge(SHAPE_COST) {
        return Err(too_much("the shape took too much work"));
    }
    let (page_w, page_h) = shown.size();
    let place = |p: [f32; 2]| -> Result<(f64, f64)> { Ok((finite_place(p[0])?.clamp(0.0, page_w), finite_place(p[1])?.clamp(0.0, page_h))) };
    let (a, b) = (place(from)?, place(to)?);
    let (side_x, side_y) = ((a.0 - b.0).abs(), (a.1 - b.1).abs());
    let boxed_shape = matches!(kind, ShapeKind::Rectangle | ShapeKind::Ellipse);
    let too_small = if boxed_shape { side_x < MIN_SHAPE || side_y < MIN_SHAPE } else { side_x.hypot(side_y) < MIN_SHAPE };
    if too_small {
        return Err(bad("the shape is too small"));
    }
    let (ua, ub) = (shown.unmap(a.0, a.1), shown.unmap(b.0, b.1));
    let look = Look { rgb, width, fill: (fill && boxed_shape).then(|| lighten(rgb)) };
    let (ap, mut d) = if boxed_shape {
        let h = width / 2.0;
        let rect = [ua.0.min(ub.0) - h, ua.1.min(ub.1) - h, ua.0.max(ub.0) + h, ua.1.max(ub.1) + h];
        let ap = boxed(kind == ShapeKind::Ellipse, rect, &look);
        let mut d = annotation(if kind == ShapeKind::Ellipse { "Circle" } else { "Square" }, rect, rgb, author, now);
        put_reals(&mut d, "RD", &[h, h, h, h]);
        (ap, d)
    } else {
        let arrow = kind == ShapeKind::Arrow;
        let ap = line(ua, ub, [End::Plain, if arrow { End::Open } else { End::Plain }], &look);
        let mut d = annotation("Line", ap.bbox, rgb, author, now);
        put_reals(&mut d, "L", &[ua.0, ua.1, ub.0, ub.1]);
        d.set("LE", Object::Array(vec![Object::from(end_name(End::Plain)), Object::from(end_name(if arrow { End::Open } else { End::Plain }))]));
        if arrow {
            d.set("IT", Object::from("LineArrow"));
        }
        (ap, d)
    };
    set_look(&mut d, &look);
    d.set("Contents", text_string(""));
    let subject = match kind {
        ShapeKind::Rectangle => "Rectangle",
        ShapeKind::Ellipse => "Oval",
        ShapeKind::Line => "Line",
        ShapeKind::Arrow => "Arrow",
    };
    d.set("Subj", text_string(subject));
    let form = ctx.form(&ap)?;
    let annot_ref = ctx.alloc()?;
    with_appearance(&mut d, form);
    ctx.objects.push((annot_ref.num, Object::Dict(d)));
    Ok(vec![annot_ref])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn look(fill: bool) -> Look {
        Look { rgb: [230, 30, 30], width: 4.0, fill: fill.then(|| lighten([230, 30, 30])) }
    }

    #[test]
    fn the_fill_is_the_colour_lightened_and_black_becomes_grey() {
        assert_eq!(lighten([0, 0, 0]), [179, 179, 179]);
        assert_eq!(lighten([255, 255, 255]), [255, 255, 255]);
        let l = lighten([230, 30, 30]);
        assert!(l[0] > 230 && l[1] > 30 && l[2] > 30 && l[1] == l[2], "{l:?}");
    }

    #[test]
    fn a_square_is_drawn_half_a_line_inside_its_rect_and_a_fill_goes_under_it() {
        let plain = boxed(false, [10.0, 20.0, 110.0, 70.0], &look(false));
        assert_eq!(plain.bbox, [10.0, 20.0, 110.0, 70.0]);
        assert!(plain.content.contains("12 22 96 46 re\nS"), "{}", plain.content);
        assert!(!plain.multiply && !plain.content.contains("/GS0"));
        let filled = boxed(false, [10.0, 20.0, 110.0, 70.0], &look(true));
        assert!(filled.multiply && filled.content.starts_with("q\n/GS0 gs\n"), "{}", filled.content);
        assert!(filled.content.find(" rg\n").unwrap() < filled.content.find(" RG\n").unwrap(), "the fill is painted first");
    }

    #[test]
    fn an_ellipse_touches_the_middle_of_each_side_of_its_box() {
        let a = boxed(true, [0.0, 0.0, 100.0, 60.0], &Look { rgb: [0, 0, 0], width: 2.0, fill: None });
        // Right middle, the top (y = 59), the left (x = 1) and the bottom (y = 1).
        for point in ["99 30 m", "50 59 c", "1 30 c", "50 1 c"] {
            assert!(a.content.contains(point) || a.content.lines().any(|l| l.ends_with(point)), "{point} is not in\n{}", a.content);
        }
    }

    #[test]
    fn an_arrow_has_two_barbs_at_its_end_and_a_box_that_holds_them() {
        let a = line((10.0, 10.0), (110.0, 10.0), [End::Plain, End::Open], &look(false));
        // The shaft, then the head: barbs behind the tip, one on each side.
        assert!(a.content.contains("10 10 m\n110 10 l\nS"), "{}", a.content);
        let head: Vec<&str> = a.content.lines().skip_while(|l| *l != "S").skip(1).collect();
        assert!(head.len() >= 4, "{head:?}");
        assert!(a.bbox[0] < 10.0 && a.bbox[2] > 110.0 && a.bbox[1] < 10.0 && a.bbox[3] > 10.0);
        // A line without heads is just the line.
        let plain = line((0.0, 0.0), (50.0, 50.0), [End::Plain, End::Plain], &look(false));
        assert_eq!(plain.content.matches(" l\n").count(), 1);
    }

    #[test]
    fn a_head_is_never_longer_than_most_of_a_short_line_and_a_point_has_none() {
        let [a, _] = head_points((10.0, 0.0), (0.0, 0.0), 8.0).unwrap();
        assert!((10.0 - a.0).abs() <= 6.0 + 1e-9, "{a:?}");
        assert!(head_points((5.0, 5.0), (5.0, 5.0), 2.0).is_none());
    }
}
