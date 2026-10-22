//! The mark of qingpdf (2026-10-21): the character 轻 in white on a rounded teal square, drawn by us as a few bold strokes with round
//! ends (a simple, geometric build of 车 on the left and 圣, a 又 over a 土, on the right), so that no outline is taken from any font. The
//! same strokes are drawn in the title strip, on the welcome page and, by `tests/tools/make_icon.py` (which reads this file), into the
//! frames of the program's icon. All numbers are in a square of 100 by 100 units, y down.
//!
//! Below 25 pixels the build is simplified (`SMALL`: thicker strokes, three flat bars for 车, fewer in 圣); at 16 pixels and less (`TINY`) the strokes also stand on
//! the middles of the pixels of a 16 by 16 square, so that they come out sharp, and so that it reads at 16.

/// One stroke: a line through these points with round caps and joins, `width` thick.
pub struct Stroke {
    pub pts: &'static [(f32, f32)],
    pub width: f32,
}

/// The corner radius of the rounded square.
pub const RADIUS: f32 = 22.0;

/// The full build, for 25 pixels and more.
pub const FULL: &[Stroke] = &[
    Stroke { pts: &[(19.0, 27.0), (41.0, 27.0)], width: 7.5 },
    Stroke { pts: &[(37.0, 38.0), (20.0, 51.0), (45.0, 51.0)], width: 7.5 },
    Stroke { pts: &[(13.0, 67.0), (47.0, 67.0)], width: 7.5 },
    Stroke { pts: &[(31.0, 14.0), (31.0, 88.0)], width: 7.5 },
    Stroke { pts: &[(55.0, 25.0), (85.0, 25.0), (63.0, 46.0)], width: 7.5 },
    Stroke { pts: &[(66.0, 33.0), (87.0, 47.0)], width: 7.5 },
    Stroke { pts: &[(62.0, 60.0), (82.0, 60.0)], width: 7.5 },
    Stroke { pts: &[(72.0, 60.0), (72.0, 86.0)], width: 7.5 },
    Stroke { pts: &[(56.0, 86.0), (88.0, 86.0)], width: 7.5 },
];

/// The simplified build, for up to 24 pixels.
pub const SMALL: &[Stroke] = &[
    Stroke { pts: &[(18.0, 30.0), (44.0, 30.0)], width: 9.5 },
    Stroke { pts: &[(13.0, 54.0), (48.0, 54.0)], width: 9.5 },
    Stroke { pts: &[(13.0, 78.0), (48.0, 78.0)], width: 9.5 },
    Stroke { pts: &[(31.0, 14.0), (31.0, 90.0)], width: 9.5 },
    Stroke { pts: &[(56.0, 28.0), (86.0, 28.0), (60.0, 52.0)], width: 9.5 },
    Stroke { pts: &[(68.0, 36.0), (88.0, 52.0)], width: 9.5 },
    Stroke { pts: &[(72.0, 62.0), (72.0, 90.0)], width: 9.5 },
    Stroke { pts: &[(58.0, 90.0), (90.0, 90.0)], width: 9.5 },
];

/// The build for 16 pixels and less: the strokes are 1.3 pixels thick (1.6 on the right) and their middles are on the middles of pixels of a 16 by 16 square.
pub const TINY: &[Stroke] = &[
    Stroke { pts: &[(34.375, 15.625), (34.375, 84.375)], width: 8.125 },
    Stroke { pts: &[(21.875, 28.125), (46.875, 28.125)], width: 8.125 },
    Stroke { pts: &[(15.625, 46.875), (53.125, 46.875)], width: 8.125 },
    Stroke { pts: &[(15.625, 65.625), (53.125, 65.625)], width: 8.125 },
    Stroke { pts: &[(59.375, 28.125), (84.375, 28.125), (63.75, 45.625)], width: 10.0 },
    Stroke { pts: &[(71.875, 53.125), (71.875, 84.375)], width: 10.0 },
    Stroke { pts: &[(59.375, 84.375), (84.375, 84.375)], width: 10.0 },
    Stroke { pts: &[(62.5, 65.625), (81.25, 65.625)], width: 10.0 },
];

/// The strokes to draw the mark with when it is `px` pixels wide.
pub fn strokes(px: i32) -> &'static [Stroke] {
    if px <= 16 {
        TINY
    } else if px <= 24 {
        SMALL
    } else {
        FULL
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_stroke_is_inside_the_square_with_its_width_and_has_two_points_or_more() {
        for set in [FULL, SMALL, TINY] {
            for s in set {
                assert!(s.pts.len() >= 2 && s.width > 0.0);
                for &(x, y) in s.pts {
                    let half = s.width / 2.0;
                    assert!(x - half >= 0.0 && x + half <= 100.0 && y - half >= 0.0 && y + half <= 100.0, "({x}, {y}) is too near the edge");
                }
            }
        }
    }

    #[test]
    fn the_glyph_is_in_the_middle_of_the_square() {
        for set in [FULL, SMALL, TINY] {
            let (mut x0, mut x1, mut y0, mut y1) = (100.0f32, 0.0f32, 100.0f32, 0.0f32);
            for s in set {
                for &(x, y) in s.pts {
                    (x0, x1, y0, y1) = (x0.min(x - s.width / 2.0), x1.max(x + s.width / 2.0), y0.min(y - s.width / 2.0), y1.max(y + s.width / 2.0));
                }
            }
            assert!(((x0 + x1) / 2.0 - 50.0).abs() <= 3.0 && ((y0 + y1) / 2.0 - 50.0).abs() <= 3.0, "{x0} {x1} {y0} {y1}");
        }
    }

    #[test]
    fn small_sizes_get_the_simple_builds_and_the_thicker_strokes() {
        let build = |px: i32| (strokes(px).len(), strokes(px).first().map(|s| s.width));
        assert!(build(16) == (TINY.len(), Some(TINY[0].width)) && build(10) == build(16));
        assert!(build(17) == (SMALL.len(), Some(SMALL[0].width)) && build(24) == build(17));
        assert!(build(25) == (FULL.len(), Some(FULL[0].width)) && build(256) == build(25));
        assert!(SMALL.len() <= FULL.len() && TINY.len() <= FULL.len());
        let thinnest = |set: &[Stroke]| set.iter().map(|s| s.width).fold(f32::MAX, f32::min);
        assert!(thinnest(SMALL) > thinnest(FULL) && thinnest(TINY) > thinnest(FULL));
    }

    #[test]
    fn the_tiny_build_stands_on_the_middles_of_the_pixels_of_a_16_pixel_square() {
        // A unit is a 6.25th of a pixel at 16 pixels: the places of the strokes' points are halves of pixels.
        for s in TINY {
            for &(x, y) in s.pts {
                for v in [x, y] {
                    let px = v / 6.25;
                    let near_half = (px - (px - 0.5).round() - 0.5).abs() < 0.25;
                    assert!(near_half || (px - px.round()).abs() < 0.5, "{px}");
                }
            }
        }
        // The vertical of 车 and of 土 are on the middle of a pixel exactly.
        assert_eq!((TINY[0].pts[0].0 / 6.25).fract(), 0.5);
        assert_eq!((TINY[5].pts[0].0 / 6.25).fract(), 0.5);
    }
}
