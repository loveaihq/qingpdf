//! The appearance streams (12.5.5) of the annotations the reader makes: a form XObject with `/BBox` equal to the
//! annotation's `/Rect` and the identity `/Matrix`, so that Algorithm 8.1 (the form's box, through its matrix, put onto the
//! `/Rect`) moves and scales nothing and every reader draws the same thing. All coordinates are default user space.

use std::fmt::Write;

use super::MarkupKind;

/// One piece of text-markup: the four corners of a quadrilateral, in the order `/QuadPoints` keeps them in practice
/// (Table 179 says counterclockwise, Acrobat and everything that reads its files uses this one): upper left, upper right,
/// lower left, lower right. "Upper" is what the reader sees as up on the page as it is shown.
pub(crate) type Quad = [(f64, f64); 4];

/// What an appearance stream is made of.
pub(crate) struct Appearance {
    /// `/BBox` of the form, and the `/Rect` of the annotation.
    pub bbox: [f64; 4],
    pub content: String,
    /// The content paints with the blend mode Multiply (a highlight lets the text through).
    pub multiply: bool,
}

/// A number for a content stream: four decimals at most, no exponent, no trailing zeros.
pub(crate) fn num(v: f64) -> String {
    let v = if v.is_finite() { v } else { 0.0 };
    let mut s = format!("{v:.4}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" { "0".to_string() } else { s }
}

fn point(out: &mut String, p: (f64, f64), op: &str) {
    let _ = writeln!(out, "{} {} {op}", num(p.0), num(p.1));
}

fn color(out: &mut String, c: [f64; 3], op: &str) {
    let _ = writeln!(out, "{} {} {} {op}", num(c[0]), num(c[1]), num(c[2]));
}

/// The box that holds every corner of `quads`, grown by `pad` on every side.
pub(crate) fn quad_bounds(quads: &[Quad], pad: f64) -> [f64; 4] {
    let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for q in quads {
        for &(x, y) in q {
            b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
        }
    }
    [b[0] - pad, b[1] - pad, b[2] + pad, b[3] + pad]
}

/// A line along the lower edge of a quad, `k` of the way up it (as a vector: from lower left to upper left).
fn along_lower_edge(q: &Quad, k: f64) -> ((f64, f64), (f64, f64)) {
    let [ul, ur, ll, lr] = *q;
    let up = (ul.0 - ll.0, ul.1 - ll.1);
    let up_r = (ur.0 - lr.0, ur.1 - lr.1);
    ((ll.0 + k * up.0, ll.1 + k * up.1), (lr.0 + k * up_r.0, lr.1 + k * up_r.1))
}

fn height_of(q: &Quad) -> f64 {
    let [ul, _, ll, _] = *q;
    ((ul.0 - ll.0).powi(2) + (ul.1 - ll.1).powi(2)).sqrt()
}

/// The most zigzag steps one quad's wavy line is drawn with, and the most for all the quads of one annotation together (the
/// stream is a few hundred kilobytes at most, however much is selected).
const MAX_WAVES: usize = 4_000;
const MAX_WAVES_IN_ALL: usize = 20_000;

pub(crate) fn markup(kind: MarkupKind, quads: &[Quad], rgb: [f64; 3]) -> Appearance {
    let mut c = String::new();
    match kind {
        MarkupKind::Highlight => {
            c.push_str("/GS0 gs\n");
            color(&mut c, rgb, "rg");
            for q in quads {
                let [ul, ur, ll, lr] = *q;
                point(&mut c, ul, "m");
                point(&mut c, ur, "l");
                point(&mut c, lr, "l");
                point(&mut c, ll, "l");
                c.push_str("h f\n");
            }
            Appearance { bbox: quad_bounds(quads, 0.0), content: c, multiply: true }
        }
        MarkupKind::Underline | MarkupKind::StrikeOut => {
            let k = if kind == MarkupKind::Underline { 0.1 } else { 0.45 };
            color(&mut c, rgb, "RG");
            for q in quads {
                let h = height_of(q);
                let _ = writeln!(c, "{} w", num((0.06 * h).clamp(0.5, 3.0)));
                let (a, b) = along_lower_edge(q, k);
                point(&mut c, a, "m");
                point(&mut c, b, "l");
                c.push_str("S\n");
            }
            Appearance { bbox: quad_bounds(quads, 1.5), content: c, multiply: false }
        }
        MarkupKind::Squiggly => {
            color(&mut c, rgb, "RG");
            c.push_str("1 j 1 J\n");
            // How many steps each quad wants; when all together want more than the stream may have, each takes a share (the zigzag
            // gets coarser, it is still a wavy line).
            let wanted: Vec<usize> = quads
                .iter()
                .map(|q| {
                    let (a, b) = along_lower_edge(q, 0.02);
                    let length = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
                    let step = (0.14 * height_of(q)).max(1.6);
                    if length > 0.0 { ((length / step).ceil() as usize).clamp(1, MAX_WAVES) } else { 1 }
                })
                .collect();
            let all: usize = wanted.iter().sum();
            for (q, want) in quads.iter().zip(wanted) {
                let h = height_of(q);
                let _ = writeln!(c, "{} w", num((0.05 * h).clamp(0.5, 2.0)));
                let (a, b) = along_lower_edge(q, 0.02);
                let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                let step = (0.14 * h).max(1.6);
                let waves = if all > MAX_WAVES_IN_ALL { (want * MAX_WAVES_IN_ALL / all).max(1) } else { want };
                // The zigzag leans out of the line by `step / 2`, towards the top of the quad.
                let [ul, _, ll, _] = *q;
                let (ux, uy) = (ul.0 - ll.0, ul.1 - ll.1);
                let un = (ux * ux + uy * uy).sqrt().max(1e-9);
                let lean = (0.5 * step, 0.5 * step);
                point(&mut c, a, "m");
                for i in 1..=waves {
                    let t = i as f64 / waves as f64;
                    let up = if i % 2 == 1 { 1.0 } else { 0.0 };
                    point(&mut c, (a.0 + dx * t + ux / un * lean.0 * up, a.1 + dy * t + uy / un * lean.1 * up), "l");
                }
                c.push_str("S\n");
            }
            Appearance { bbox: quad_bounds(quads, 1.5), content: c, multiply: false }
        }
    }
}

/// A freehand drawing (12.5.6.13): the strokes in `strokes` (points in user space), `width` wide, round caps and joins.
pub(crate) fn ink(strokes: &[Vec<(f64, f64)>], rgb: [f64; 3], width: f64) -> Appearance {
    let mut c = String::new();
    color(&mut c, rgb, "RG");
    let _ = writeln!(c, "{} w\n1 J 1 j", num(width));
    let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for stroke in strokes {
        for (i, &(x, y)) in stroke.iter().enumerate() {
            b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
            point(&mut c, (x, y), if i == 0 { "m" } else { "l" });
        }
        // A stroke of one point is a dot: a line of no length with round caps.
        if stroke.len() == 1
            && let Some(&(x, y)) = stroke.first()
        {
            point(&mut c, (x + 0.01, y), "l");
        }
        c.push_str("S\n");
    }
    let pad = width / 2.0 + 1.0;
    Appearance { bbox: [b[0] - pad, b[1] - pad, b[2] + pad, b[3] + pad], content: c, multiply: false }
}

/// The side of the icon of a note, in points.
pub(crate) const NOTE_SIZE: f64 = 20.0;

/// The icon of a note (12.5.6.4, a speech bubble with three lines of text), drawn in a box `NOTE_SIZE` square that starts at
/// the origin; the annotation's `/Rect` puts it on the page.
pub(crate) fn note(rgb: [f64; 3]) -> Appearance {
    let mut c = String::new();
    color(&mut c, rgb, "rg");
    color(&mut c, [0.15 * rgb[0], 0.15 * rgb[1], 0.15 * rgb[2]], "RG");
    c.push_str("0.8 w 1 j\n");
    c.push_str("1 19 m 19 19 l 19 6 l 10 6 l 5 1 l 5 6 l 1 6 l h B\n");
    c.push_str("0.25 0.25 0.25 RG\n0.9 w\n4 15 m 16 15 l S\n4 12 m 16 12 l S\n4 9 m 12 9 l S\n");
    Appearance { bbox: [0.0, 0.0, NOTE_SIZE, NOTE_SIZE], content: c, multiply: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_short_and_never_use_an_exponent() {
        assert_eq!(num(0.0), "0");
        assert_eq!(num(-0.00001), "0");
        assert_eq!(num(12.5), "12.5");
        assert_eq!(num(100.0), "100");
        assert_eq!(num(1.0 / 3.0), "0.3333");
        assert_eq!(num(1.0e9), "1000000000");
        assert_eq!(num(f64::NAN), "0");
    }

    #[test]
    fn a_highlight_fills_the_quad_and_the_box_is_the_quad() {
        let q: Quad = [(10.0, 20.0), (50.0, 20.0), (10.0, 8.0), (50.0, 8.0)];
        let a = markup(MarkupKind::Highlight, &[q], [1.0, 0.9, 0.0]);
        assert_eq!(a.bbox, [10.0, 8.0, 50.0, 20.0]);
        assert!(a.multiply && a.content.contains("10 20 m\n50 20 l\n50 8 l\n10 8 l\nh f"), "{}", a.content);
    }

    #[test]
    fn a_squiggly_over_many_wide_lines_has_a_stream_of_bounded_size() {
        let q: Quad = [(0.0, 20.0), (5000.0, 20.0), (0.0, 8.0), (5000.0, 8.0)];
        let a = markup(MarkupKind::Squiggly, &vec![q; 4000], [1.0, 0.0, 0.0]);
        assert!(a.content.len() < 1_000_000, "{} bytes", a.content.len());
        // One line keeps its detail.
        let one = markup(MarkupKind::Squiggly, &[q], [1.0, 0.0, 0.0]);
        assert!(one.content.lines().count() > 1_000, "{} lines", one.content.lines().count());
    }

    #[test]
    fn a_dot_is_a_line_of_almost_no_length() {
        let a = ink(&[vec![(5.0, 5.0)]], [0.0, 0.0, 1.0], 2.0);
        assert!(a.content.contains("5 5 m\n5.01 5 l\nS"), "{}", a.content);
        assert_eq!(a.bbox, [3.0, 3.0, 7.0, 7.0]);
    }
}
