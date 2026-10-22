//! The rows of the tree of bookmarks (2026-10-22): the tree is Windows' own control (it keeps the label that is typed over, the drag, the keys), but each row is
//! drawn by the reader (`NM_CUSTOMDRAW`), so that the chosen row can be a rounded tint of the accent and the colours are the window's, light or dark. This is the
//! plain part: how one row is drawn on a [`Painter`]. Sizes are those of 96 dpi.

use crate::ui::{Font, Painter, Rect, Skin, fit_text};

/// The height of a row, the room at the sides of the chosen row, the corner radius.
pub const ROW_H: i32 = 28;
pub const INSET: i32 = 2;
pub const RADIUS: i32 = 8;
/// How far each level of the tree stands in (the width of the arrow's place).
pub const INDENT: i32 = 20;

/// What a row is: its words, how deep it is, whether it has rows under it and shows them, and whether it is the one chosen, the one the pointer is on, or the
/// one a drag would drop into.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    pub text: String,
    pub has_children: bool,
    pub open: bool,
    pub chosen: bool,
    pub hot: bool,
    pub drop: bool,
}

/// Draw the row in `r` (the width of the tree and the height of a row), with its words starting at `label_x` (where the tree puts the label of the row; its arrow
/// is in the width of one level before that).
pub fn paint(p: &mut dyn Painter, r: Rect, label_x: i32, row: &Row, sk: &Skin, dpi: u32) {
    let px = |v: i32| v * dpi as i32 / 96;
    p.fill_rect(r, sk.surface);
    let frame = Rect { x: r.x + px(INSET * 2), y: r.y + px(INSET), w: (r.w - px(INSET * 4)).max(0), h: r.h - px(INSET * 2) };
    if row.chosen || row.drop {
        p.fill_shape(frame, [px(RADIUS); 4], sk.tint, 255);
    } else if row.hot {
        p.fill_shape(frame, [px(RADIUS); 4], sk.hover, 24);
    }
    if row.has_children {
        let side = px(INDENT);
        p.glyph(Rect { x: label_x - side - px(2), y: r.y, w: side, h: r.h }, if row.open { '\u{E70D}' } else { '\u{E76C}' }, sk.dim);
    }
    p.set_font(Font::Body);
    let ty = r.y + (r.h - p.line_height()) / 2;
    let room = (frame.x + frame.w - label_x - px(6)).max(0);
    // A title too long for the row is cut short with an ellipsis.
    let text = fit_text(p, &row.text, room);
    p.text(label_x, ty, &text, if row.chosen || row.drop { sk.on_tint } else { sk.text });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Rec {
        shapes: Vec<(Rect, u32, u8)>,
        texts: Vec<(i32, String, u32)>,
        glyphs: Vec<char>,
    }

    impl Painter for Rec {
        fn fill_rect(&mut self, _r: Rect, _c: u32) {}
        fn fill_rect_alpha(&mut self, _r: Rect, _c: u32, _a: u8) {}
        fn draw_bgra(&mut self, _d: Rect, _w: i32, _h: i32, _b: &[u8]) {}
        fn text(&mut self, x: i32, _y: i32, s: &str, c: u32) {
            self.texts.push((x, s.to_string(), c));
        }
        fn text_width(&mut self, s: &str) -> i32 {
            7 * s.chars().count() as i32
        }
        fn line_height(&self) -> i32 {
            16
        }
        fn fill_shape(&mut self, r: Rect, _radii: [i32; 4], c: u32, a: u8) {
            self.shapes.push((r, c, a));
        }
        fn glyph(&mut self, _r: Rect, ch: char, _c: u32) {
            self.glyphs.push(ch);
        }
    }

    fn skin() -> Skin {
        Skin { surface: 1, text: 2, dim: 3, off: 4, line: 5, field: 6, accent: 7, accent_hover: 8, tint: 9, on_tint: 10, on_accent: 11, hover: 12 }
    }

    fn row(text: &str) -> Row {
        Row { text: text.to_string(), ..Row::default() }
    }

    #[test]
    fn the_chosen_row_is_a_rounded_tint_the_hot_one_a_faint_fill_and_the_others_have_neither() {
        let r = Rect { x: 0, y: 0, w: 240, h: ROW_H };
        let mut p = Rec::default();
        paint(&mut p, r, 40, &Row { chosen: true, ..row("Chapter 1") }, &skin(), 96);
        assert_eq!(p.shapes.len(), 1);
        assert_eq!((p.shapes[0].1, p.shapes[0].2), (9, 255));
        assert_eq!(p.texts[0].2, 10);
        let mut p = Rec::default();
        paint(&mut p, r, 40, &Row { hot: true, ..row("Chapter 1") }, &skin(), 96);
        assert_eq!((p.shapes[0].1, p.shapes[0].2 < 100), (12, true));
        assert_eq!(p.texts[0].2, 2);
        let mut p = Rec::default();
        paint(&mut p, r, 40, &row("Chapter 1"), &skin(), 96);
        assert!(p.shapes.is_empty());
        // A row a drag would drop into is lit like the chosen one.
        let mut p = Rec::default();
        paint(&mut p, r, 40, &Row { drop: true, ..row("Chapter 1") }, &skin(), 96);
        assert_eq!(p.shapes[0].1, 9);
    }

    #[test]
    fn an_arrow_shows_for_a_row_with_rows_under_it_down_when_they_are_shown() {
        let r = Rect { x: 0, y: 0, w: 240, h: ROW_H };
        let mut p = Rec::default();
        paint(&mut p, r, 40, &Row { has_children: true, ..row("A") }, &skin(), 96);
        assert_eq!(p.glyphs, vec!['\u{E76C}']);
        let mut p = Rec::default();
        paint(&mut p, r, 40, &Row { has_children: true, open: true, ..row("A") }, &skin(), 96);
        assert_eq!(p.glyphs, vec!['\u{E70D}']);
        let mut p = Rec::default();
        paint(&mut p, r, 40, &row("A"), &skin(), 96);
        assert!(p.glyphs.is_empty());
        // The words begin where the tree puts the label.
        assert_eq!(p.texts[0].0, 40);
    }

    #[test]
    fn a_title_too_long_for_the_row_is_cut_with_an_ellipsis_and_one_that_fits_is_whole() {
        let r = Rect { x: 0, y: 0, w: 100, h: ROW_H };
        let mut p = Rec::default();
        paint(&mut p, r, 40, &row("A very long title of a chapter"), &skin(), 96);
        assert!(p.texts[0].1.ends_with('\u{2026}') && p.texts[0].1.chars().count() < 30);
        assert!(7 * p.texts[0].1.chars().count() as i32 <= 100 - 40);
        let mut p = Rec::default();
        paint(&mut p, r, 40, &row("Short"), &skin(), 96);
        assert_eq!(p.texts[0].1, "Short");
    }
}
