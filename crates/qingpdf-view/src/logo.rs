//! The mark of qingpdf (2026-10-21, drawn again 2026-10-22): the character 轻 in white on a rounded teal square, drawn by us as a few bold strokes so that no
//! outline is taken from any font. On the left is 车, in the four strokes it is written in: 横 (the short bar at the top), 撇折 (down to the left and then across),
//! 竖 (the stem through all of it) and 提 (the last bar, rising to the right). On the right is 又 (a 横撇 and a 捺) over 工 (a short bar, the stem between the two bars, which does not stick out above the
//! short one, and the long bar under it, which is as wide as the 又 above). All the strokes are as thick as each other and end in the same round cap. The same strokes are drawn in the title
//! strip, on the welcome page and, by `tests/tools/make_icon.py` (which reads this file), into the frames of the program's icon (48 and 256 pixels).
//!
//! At 16, 24 and 32 pixels the mark is not drawn from the strokes but from a bitmap made by hand (`P16`, `P24`, `P32`: `#` is a pixel of the character, whole
//! pixels, one pixel thick at 16 and two at 24 and 32, simplified where the room is short), so that the bars and the stems come out sharp where the strokes would
//! be smeared over two pixels. All numbers of the strokes are in a square of 100 by 100 units, y down.

/// One stroke: a line through these points with round caps and joins, `width` thick.
pub struct Stroke {
    pub pts: &'static [(f32, f32)],
    pub width: f32,
}

/// The corner radius of the rounded square.
pub const RADIUS: f32 = 22.0;

/// The strokes, for the sizes that have no bitmap (48 pixels and more, and the sizes between the bitmaps').
pub const FULL: &[Stroke] = &[
    Stroke { pts: &[(15.0, 27.0), (40.0, 27.0)], width: 7.2 },
    Stroke { pts: &[(39.0, 37.5), (19.5, 50.5), (45.0, 50.5)], width: 7.2 },
    Stroke { pts: &[(31.0, 13.0), (31.0, 88.0)], width: 7.2 },
    Stroke { pts: &[(13.5, 72.0), (46.5, 62.5)], width: 7.2 },
    Stroke { pts: &[(55.0, 24.0), (86.0, 24.0), (63.0, 43.0)], width: 7.2 },
    Stroke { pts: &[(67.0, 31.0), (88.0, 45.0)], width: 7.2 },
    Stroke { pts: &[(64.0, 61.0), (82.0, 61.0)], width: 7.2 },
    Stroke { pts: &[(73.0, 61.0), (73.0, 86.0)], width: 7.2 },
    Stroke { pts: &[(56.0, 86.0), (90.0, 86.0)], width: 7.2 },
];

/// The strokes to draw the mark with when it is `px` pixels wide and has no bitmap.
pub fn strokes(_px: i32) -> &'static [Stroke] {
    FULL
}

/// The bitmaps made by hand, for 16, 24 and 32 pixels (`#` is a pixel of the character).
pub const P16: &[&str] = &[
    "................",
    "................",
    ".....#..........",
    "...####..#####..",
    ".....#.#.....#..",
    ".....##...#.#...",
    "....##.....#....",
    "...#####..#.#...",
    ".....#...#...#..",
    ".....###........",
    "....##....###...",
    "..##.#.....#....",
    ".....#.....#....",
    ".....#...#####..",
    "................",
    "................",
];

pub const P24: &[&str] = &[
    "........................",
    "........................",
    "........................",
    ".......##...............",
    ".......##...............",
    ".......##...............",
    "....#######..########...",
    "....#######..########...",
    ".......##......##..##...",
    ".......##..##...####....",
    ".......##.##....###.....",
    ".......####....##.##....",
    ".......##.....##...##...",
    "......###...............",
    "....########....####....",
    "....########....####....",
    ".......##.##.....##.....",
    ".......#####.....##.....",
    ".....#####.......##.....",
    "...######........##.....",
    "...##..##.....########..",
    "..............########..",
    "........................",
    "........................",
];

pub const P32: &[&str] = &[
    "................................",
    "................................",
    "................................",
    "................................",
    ".........##.....................",
    ".........##.....................",
    ".........##.....................",
    ".........##.....................",
    ".....##########...##########....",
    ".....##########...##########....",
    ".........##..............###....",
    ".........##...###....######.....",
    ".........##..###......####......",
    ".........##.###.......####......",
    ".........#####.......######.....",
    ".........###........###..###....",
    "........###.....................",
    "......##########................",
    "......##########................",
    ".........##...........#####.....",
    ".........##...........#####.....",
    ".........##.............##......",
    ".........##....##.......##......",
    ".........##.#####.......##......",
    ".........######.........##......",
    "......######............##......",
    "....#######.............##......",
    "....##...##........##########...",
    ".........##........##########...",
    "................................",
    "................................",
    "................................",
];

/// The bitmap for a mark that is exactly `px` pixels wide, if there is one.
pub fn bitmap(px: i32) -> Option<&'static [&'static str]> {
    match px {
        16 => Some(P16),
        24 => Some(P24),
        32 => Some(P32),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_stroke_is_inside_the_square_with_its_width_and_has_two_points_or_more() {
        for s in FULL {
            assert!(s.pts.len() >= 2 && s.width > 0.0);
            for &(x, y) in s.pts {
                let half = s.width / 2.0;
                assert!(x - half >= 0.0 && x + half <= 100.0 && y - half >= 0.0 && y + half <= 100.0, "({x}, {y}) is too near the edge");
            }
        }
    }

    #[test]
    fn the_strokes_are_as_thick_as_each_other_and_the_glyph_is_in_the_middle_of_the_square() {
        assert!(FULL.windows(2).all(|w| w[0].width == w[1].width), "one thickness for all the strokes");
        let (mut x0, mut x1, mut y0, mut y1) = (100.0f32, 0.0f32, 100.0f32, 0.0f32);
        for s in FULL {
            for &(x, y) in s.pts {
                (x0, x1, y0, y1) = (x0.min(x - s.width / 2.0), x1.max(x + s.width / 2.0), y0.min(y - s.width / 2.0), y1.max(y + s.width / 2.0));
            }
        }
        assert!(((x0 + x1) / 2.0 - 50.0).abs() <= 2.0 && ((y0 + y1) / 2.0 - 50.0).abs() <= 2.0, "{x0} {x1} {y0} {y1}");
        // 车 is on the left of 圣, and the long bar under 圣 is as wide as the 又 above it.
        let reach = |from: usize, to: usize| {
            let (mut lo, mut hi) = (100.0f32, 0.0f32);
            for s in &FULL[from..to] {
                for &(x, _) in s.pts {
                    (lo, hi) = (lo.min(x), hi.max(x));
                }
            }
            (lo, hi)
        };
        let (left, right, top, bottom) = (reach(0, 4), reach(4, 9), reach(4, 6), reach(8, 9));
        assert!(left.1 < right.0 + 8.0 && left.0 < right.0);
        assert!(((top.1 - top.0) - (bottom.1 - bottom.0)).abs() <= 4.0, "{top:?} {bottom:?}");
        // 工: the short bar is clearly shorter than the long one, and the stem is between the two, in the middle of both, and does not stick out above the short bar.
        let (upper, stem, lower) = (&FULL[6], &FULL[7], &FULL[8]);
        let length = |s: &Stroke| (s.pts[1].0 - s.pts[0].0).abs();
        assert!(length(upper) <= 0.6 * length(lower), "{} {}", length(upper), length(lower));
        assert!(stem.pts[0].1 >= upper.pts[0].1 && stem.pts[1].1 <= lower.pts[0].1 + 0.1, "the stem is between the bars");
        let middle = |s: &Stroke| (s.pts[0].0 + s.pts[1].0) / 2.0;
        assert!((stem.pts[0].0 - middle(upper)).abs() <= 1.0 && (stem.pts[0].0 - middle(lower)).abs() <= 1.0);
    }

    #[test]
    fn a_mark_of_16_24_or_32_pixels_has_its_bitmap_and_other_sizes_are_drawn_from_the_strokes() {
        for (px, bm) in [(16, P16), (24, P24), (32, P32)] {
            assert_eq!(bitmap(px).map(|b| b.len()), Some(px as usize));
            assert!(bm.iter().all(|row| row.len() == px as usize && row.bytes().all(|c| c == b'#' || c == b'.')), "{px}");
        }
        assert!(bitmap(20).is_none() && bitmap(48).is_none() && bitmap(256).is_none());
    }

    #[test]
    fn each_bitmap_has_the_character_in_the_middle_of_its_square_and_stems_that_are_whole_pixels() {
        for (px, bm) in [(16usize, P16), (24, P24), (32, P32)] {
            let on: Vec<(usize, usize)> = bm.iter().enumerate().flat_map(|(y, row)| row.bytes().enumerate().filter(|(_, c)| *c == b'#').map(move |(x, _)| (x, y))).collect();
            let (x0, x1) = (on.iter().map(|p| p.0).min().unwrap_or(0), on.iter().map(|p| p.0).max().unwrap_or(0));
            let (y0, y1) = (on.iter().map(|p| p.1).min().unwrap_or(0), on.iter().map(|p| p.1).max().unwrap_or(0));
            // The room on a side differs from the room on the other by at most a pixel.
            assert!((x0 as i32 - (px as i32 - 1 - x1 as i32)).abs() <= 1 && (y0 as i32 - (px as i32 - 1 - y1 as i32)).abs() <= 1, "{px}: {x0}..{x1}, {y0}..{y1}");
            // The glyph does not touch the edge of the square (the corners are round).
            assert!(x0 >= px / 10 && y0 >= px / 10 && x1 + px / 10 < px && y1 + px / 10 < px, "{px}");
            // The stem of 车 is the columns that are as tall as the character (nine tenths): one pixel wide at 16, two at 24 and 32.
            let (thick, tall) = (if px == 16 { 1 } else { 2 }, y1 - y0 + 1);
            let stem = (0..px).filter(|&x| bm.iter().filter(|row| row.as_bytes()[x] == b'#').count() * 10 >= tall * 9).count();
            assert_eq!(stem, thick, "{px}: the stem is {stem} wide");
        }
    }
}
