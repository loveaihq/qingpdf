//! The look of the menus (2026-10-22): the pop-up menus (the "..." menu with its ten menus inside, the menus on the page, the lists a button opens) are drawn
//! by the reader (`MF_OWNERDRAW`: Windows asks how big an item is and then asks for it to be drawn), in the reader's fonts and colours, light or dark. This
//! file is the plain part: how a label is taken apart (its shortcut letter, its key), how big an item is, how it is drawn on a [`Painter`], and which item a
//! letter chooses. `win32.rs` does the asking and puts the picture on the menu. Sizes are those of 96 dpi.

use crate::ui::{Font, Painter, Rect, Skin};

pub const ROW_H: i32 = 32;
pub const SEP_H: i32 = 9;
/// The space at the side of a row (the chosen row stands in from the edge of the menu by this much), the radius of the chosen row, the room inside it at the
/// sides, the column of the tick or dot, the gap between the words and the key, and the column of the arrow of a submenu.
pub const SIDE: i32 = 4;
pub const RADIUS: i32 = 8;
const PAD: i32 = 10;
const MARK_W: i32 = 24;
const GAP: i32 = 28;
const ARROW_W: i32 = 22;

/// A label taken apart: the words, the place (a character number) of the letter that chooses the item with Alt (if it has one), and the key beside it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Label {
    pub text: String,
    pub accel: Option<usize>,
    pub key: String,
}

/// What a menu item is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Item,
    /// An item that opens a menu of its own.
    Sub,
    Separator,
}

/// One item of a menu, as the window keeps it while the menu is up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub label: Label,
    pub kind: Kind,
    pub checked: bool,
    pub radio: bool,
    pub enabled: bool,
}

/// Take a label apart: `&` marks the letter after it (`&&` is an `&`), and what follows a tab is the key.
pub fn parse(label: &str) -> Label {
    let (words, key) = label.split_once('\t').unwrap_or((label, ""));
    let mut out = Label { key: key.to_string(), ..Label::default() };
    let mut chars = words.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            match chars.peek() {
                Some('&') => {
                    chars.next();
                    out.text.push('&');
                }
                Some(_) if out.accel.is_none() => out.accel = Some(out.text.chars().count()),
                _ => {}
            }
        } else {
            out.text.push(c);
        }
    }
    out
}

/// The size of an item (width, height), given how wide the words and the key are.
pub fn measure(e: &Entry, dpi: u32, text_w: i32, key_w: i32) -> (i32, i32) {
    let px = |v: i32| v * dpi as i32 / 96;
    if e.kind == Kind::Separator {
        return (px(2 * SIDE + 40), px(SEP_H));
    }
    let key = if e.label.key.is_empty() { 0 } else { px(GAP) + key_w };
    let arrow = if e.kind == Kind::Sub { px(ARROW_W) } else { px(PAD) };
    (px(2 * SIDE + PAD + MARK_W) + text_w + key + arrow, px(ROW_H))
}

/// The item that the letter `c` chooses among `letters` (the Alt letter of each item, if it has one), with the item `current` chosen now: the next one after it
/// that has the letter, going round. The second is whether it is the only one.
pub fn choose_by_letter(letters: &[Option<char>], c: char, current: Option<usize>) -> Option<(usize, bool)> {
    let same = |l: &Option<char>| l.is_some_and(|l| l.to_lowercase().eq(c.to_lowercase()));
    let hits: Vec<usize> = letters.iter().enumerate().filter(|(_, l)| same(l)).map(|(i, _)| i).collect();
    let first = *hits.first()?;
    let next = hits.iter().copied().find(|&i| current.is_some_and(|n| i > n)).unwrap_or(first);
    Some((next, hits.len() == 1))
}

/// Draw one item in `r`: the ground of the menu, the chosen row, the tick or the dot, the words, the key and the arrow. `accel`: the letters that choose
/// items are underlined (they are after Alt has been pressed).
pub fn paint(p: &mut dyn Painter, r: Rect, e: &Entry, selected: bool, accel: bool, sk: &Skin, dpi: u32) {
    let px = |v: i32| v * dpi as i32 / 96;
    p.fill_rect(r, sk.surface);
    if e.kind == Kind::Separator {
        p.fill_rect(Rect { x: r.x + px(SIDE + 6), y: r.y + r.h / 2, w: r.w - px(2 * (SIDE + 6)), h: px(1).max(1) }, sk.line);
        return;
    }
    let row = Rect { x: r.x + px(SIDE), y: r.y + px(2), w: r.w - px(2 * SIDE), h: r.h - px(4) };
    let live = e.enabled;
    if selected {
        p.fill_shape(row, [px(RADIUS); 4], sk.tint, if live { 255 } else { 90 });
    }
    let ink = if !live { sk.off } else if selected { sk.on_tint } else { sk.text };
    let mark = if live { sk.accent } else { sk.off };
    // The tick or the dot, in the column before the words.
    let col = Rect { x: row.x + px(PAD - 2), y: row.y, w: px(MARK_W - 4), h: row.h };
    if e.checked {
        let (cx, cy) = ((col.x + col.w / 2) as f32, (row.y + row.h / 2) as f32);
        let k = dpi as f32 / 96.0;
        if e.radio {
            let d = (8.0 * k).round() as i32;
            p.fill_shape(Rect { x: cx as i32 - d / 2, y: cy as i32 - d / 2, w: d, h: d }, [d / 2; 4], mark, 255);
        } else {
            p.polyline(&[(cx - 5.0 * k, cy + 0.5 * k), (cx - 1.5 * k, cy + 4.0 * k), (cx + 5.5 * k, cy - 4.0 * k)], (1.8 * k).max(1.5), mark, 255);
        }
    }
    p.set_font(Font::Body);
    let lh = p.line_height();
    let ty = row.y + (row.h - lh) / 2;
    let tx = row.x + px(PAD + MARK_W - 2);
    p.text(tx, ty, &e.label.text, ink);
    if accel
        && live
        && let Some(i) = e.label.accel
    {
        let before: String = e.label.text.chars().take(i).collect();
        let letter: String = e.label.text.chars().skip(i).take(1).collect();
        let (x, w) = (tx + p.text_width(&before), p.text_width(&letter));
        p.fill_rect(Rect { x, y: ty + lh - px(2), w, h: px(1).max(1) }, ink);
    }
    let right = row.x + row.w;
    if e.kind == Kind::Sub {
        p.glyph(Rect { x: right - px(ARROW_W), y: row.y, w: px(ARROW_W - 4), h: row.h }, '\u{E76C}', if live { sk.dim } else { sk.off });
    } else if !e.label.key.is_empty() {
        let w = p.text_width(&e.label.key);
        p.text(right - px(PAD) - w, ty, &e.label.key, if live { sk.dim } else { sk.off });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Rec {
        fills: Vec<(Rect, u32)>,
        shapes: Vec<(Rect, u32, u8)>,
        lines: usize,
        texts: Vec<(i32, String, u32)>,
        glyphs: Vec<char>,
    }

    impl Painter for Rec {
        fn fill_rect(&mut self, r: Rect, c: u32) {
            self.fills.push((r, c));
        }
        fn fill_rect_alpha(&mut self, r: Rect, c: u32, _a: u8) {
            self.fills.push((r, c));
        }
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
        fn polyline(&mut self, _p: &[(f32, f32)], _w: f32, _c: u32, _a: u8) {
            self.lines += 1;
        }
        fn glyph(&mut self, _r: Rect, ch: char, _c: u32) {
            self.glyphs.push(ch);
        }
    }

    fn skin() -> Skin {
        Skin { surface: 1, text: 2, dim: 3, off: 4, line: 5, field: 6, accent: 7, accent_hover: 8, tint: 9, on_tint: 10, on_accent: 11, hover: 12 }
    }

    fn entry(label: &str, kind: Kind) -> Entry {
        Entry { label: parse(label), kind, checked: false, radio: false, enabled: true }
    }

    #[test]
    fn a_label_is_taken_apart_into_words_a_letter_and_a_key() {
        let l = parse("&Open...\tCtrl+O");
        assert_eq!((l.text.as_str(), l.accel, l.key.as_str()), ("Open...", Some(0), "Ctrl+O"));
        // The Chinese labels put the letter in brackets after the words.
        let l = parse("打开(&O)...\tCtrl+O");
        assert_eq!((l.text.as_str(), l.accel), ("打开(O)...", Some(3)));
        // A doubled & is an &; a path with one has no letter.
        let l = parse("C:\\a&&b.pdf");
        assert_eq!((l.text.as_str(), l.accel), ("C:\\a&b.pdf", None));
        let l = parse("&1 C:\\x.pdf");
        assert_eq!((l.text.as_str(), l.accel), ("1 C:\\x.pdf", Some(0)));
        // A lone & at the end, and a label with no key.
        assert_eq!(parse("odd&").text, "odd");
        assert!(parse("Plain").key.is_empty());
    }

    #[test]
    fn an_item_is_a_row_of_32_pixels_wide_enough_for_its_words_and_its_key_and_scales_with_the_screen() {
        let plain = entry("&Open\tCtrl+O", Kind::Item);
        let (w, h) = measure(&plain, 96, 40, 50);
        assert_eq!(h, 32);
        let no_key = measure(&entry("&Open", Kind::Item), 96, 40, 0).0;
        assert!(w >= no_key + 50 + GAP, "{w} {no_key}");
        // A submenu has room for its arrow; a separator is thin.
        assert!(measure(&entry("&File", Kind::Sub), 96, 40, 0).0 > no_key);
        assert_eq!(measure(&entry("", Kind::Separator), 96, 0, 0).1, SEP_H);
        assert_eq!(measure(&plain, 192, 80, 100).1, 64);
        assert!(measure(&plain, 192, 80, 100).0 > w);
    }

    #[test]
    fn a_letter_chooses_the_next_item_that_has_it_and_says_when_it_is_the_only_one() {
        let letters = [Some('f'), Some('E'), None, Some('F'), Some('v')];
        assert_eq!(choose_by_letter(&letters, 'v', None), Some((4, true)));
        assert_eq!(choose_by_letter(&letters, 'e', Some(4)), Some((1, true)));
        // Two have F: the first, then the next after the one chosen, then round again.
        assert_eq!(choose_by_letter(&letters, 'F', None), Some((0, false)));
        assert_eq!(choose_by_letter(&letters, 'f', Some(0)), Some((3, false)));
        assert_eq!(choose_by_letter(&letters, 'f', Some(3)), Some((0, false)));
        assert_eq!(choose_by_letter(&letters, 'x', None), None);
        assert_eq!(choose_by_letter(&[], 'a', None), None);
    }

    #[test]
    fn the_chosen_row_is_tinted_and_rounded_the_tick_and_the_dot_are_in_the_accent_and_a_greyed_item_is_pale() {
        let r = Rect { x: 0, y: 0, w: 240, h: 32 };
        let sk = skin();
        let mut p = Rec::default();
        let mut e = entry("&Open\tCtrl+O", Kind::Item);
        paint(&mut p, r, &e, true, false, &sk, 96);
        // The ground, then the chosen row in the tint, inset from the edge, with the words in the ink that goes on the tint and the key right-aligned.
        assert_eq!(p.fills[0], (r, 1));
        assert_eq!((p.shapes[0].1, p.shapes[0].2), (9, 255));
        assert!(p.shapes[0].0.x == SIDE && p.shapes[0].0.w == 240 - 2 * SIDE);
        assert_eq!(p.texts[0].1, "Open");
        assert_eq!(p.texts[0].2, 10);
        assert_eq!(p.texts[1].1, "Ctrl+O");
        assert_eq!(p.texts[1].2, 3);
        assert!(p.texts[1].0 + 7 * 6 <= 240 - SIDE - PAD + 1, "the key stands at the right edge");
        assert_eq!(p.lines, 0);
        // A tick is a line; a dot is a shape in the accent.
        e.checked = true;
        let mut p = Rec::default();
        paint(&mut p, r, &e, false, false, &sk, 96);
        assert_eq!((p.lines, p.shapes.len()), (1, 0));
        e.radio = true;
        let mut p = Rec::default();
        paint(&mut p, r, &e, false, false, &sk, 96);
        assert_eq!((p.lines, p.shapes.len()), (0, 1));
        assert_eq!(p.shapes[0].1, 7);
        // Greyed: the words and the key are the pale ink, and a row chosen is faint.
        e.enabled = false;
        let mut p = Rec::default();
        paint(&mut p, r, &e, true, false, &sk, 96);
        assert!(p.texts.iter().all(|t| t.2 == 4));
        assert!(p.shapes.iter().any(|s| s.2 < 255));
        // An item that opens a menu has an arrow and no key.
        let mut p = Rec::default();
        paint(&mut p, r, &entry("&File", Kind::Sub), false, false, &sk, 96);
        assert_eq!(p.glyphs, vec!['\u{E76C}']);
        // A separator is a line of the hairline colour across the menu.
        let mut p = Rec::default();
        paint(&mut p, Rect { x: 0, y: 0, w: 240, h: SEP_H }, &entry("", Kind::Separator), false, false, &sk, 96);
        assert_eq!(p.fills.len(), 2);
        assert_eq!(p.fills[1].1, 5);
        assert_eq!(p.fills[1].0.h, 1);
    }

    #[test]
    fn the_letter_that_chooses_is_underlined_only_when_asked() {
        let r = Rect { x: 0, y: 0, w: 240, h: 32 };
        let sk = skin();
        let e = entry("打开(&O)", Kind::Item);
        let mut shown = Rec::default();
        paint(&mut shown, r, &e, false, true, &sk, 96);
        let mut hidden = Rec::default();
        paint(&mut hidden, r, &e, false, false, &sk, 96);
        assert_eq!(shown.fills.len(), hidden.fills.len() + 1);
        // The underline is under the O, which comes after three characters.
        let tx = shown.texts[0].0;
        assert_eq!(shown.fills.last().map(|f| f.0.x), Some(tx + 7 * 3));
    }
}
