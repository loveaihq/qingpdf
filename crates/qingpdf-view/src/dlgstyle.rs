//! The look of the boxes the reader opens itself (2026-10-22: the form box with its many kinds, the box that asks for a line of text, the passwords): one
//! style for all of them. The apps lay a box out in the old way (small controls, groups in frames); [`lay_out`] makes of that the new one: 24 pixels of room
//! round the inside, the groups' frames become headings, every control 32 pixels high, 12 between rows, and the buttons in a row at the foot. The buttons, the
//! tick boxes and the frame of an edit box are drawn by the reader ([`paint_button`], [`paint_check`], [`paint_edit_frame`]) on a [`Painter`]. Plain logic;
//! `win32.rs` makes the windows. All sizes are those of 96 dpi.

use crate::accent;
use crate::ui::{Font, FormKind, Painter, Rect, Skin, fit_text};

pub const PAD: i32 = 24;
pub const CONTROL_H: i32 = 32;
pub const LABEL_H: i32 = 20;
pub const CHECK_H: i32 = 24;
pub const ROW_GAP: i32 = 12;
pub const HEAD_H: i32 = 22;
const HEAD_GAP: i32 = 6;
const SECTION_GAP: i32 = 10;
pub const BTN_H: i32 = 32;
const BTN_MIN: i32 = 88;
const BTN_GAP: i32 = 8;
/// The height an old layout gave to the line of a list that is closed.
const OLD_CLOSED: i32 = 24;
/// The corner radius of a control, and the room round the text in an edit box.
pub const RADIUS: i32 = 6;
pub const EDIT_PAD: i32 = 8;

pub type Box4 = (i32, i32, i32, i32);

/// The new place of everything in a box.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Laid {
    /// The new place of each item, in the order they were given. A group has the place of its heading.
    pub rects: Vec<Box4>,
    /// The sketch of a page, if the box has one.
    pub preview: Option<Box4>,
    /// The size of the inside of the box.
    pub size: (i32, i32),
    pub ok: Box4,
    pub cancel: Box4,
}

/// What the person will read on a button: about how wide that is.
fn text_w(s: &str) -> i32 {
    s.chars().map(|c| if c.is_ascii() { 7 } else { 13 }).sum()
}

fn new_height(kind: FormKind, old_h: i32) -> i32 {
    match kind {
        FormKind::Edit | FormKind::Secret | FormKind::Combo | FormKind::Pick | FormKind::Button => CONTROL_H,
        FormKind::Check => CHECK_H,
        // A line of words is as before; a block of them is a little taller.
        FormKind::Label | FormKind::Group => if old_h > OLD_CLOSED { old_h + 2 } else { LABEL_H },
    }
}

/// Lay a box out anew: the kind and the old place of each item (a line of words that has none is given a height of 0) (a list that drops down is as tall as its list was), the old place of the sketch, and the
/// words on the two buttons.
pub fn lay_out(items: &[(FormKind, Box4)], preview: Option<Box4>, ok: &str, cancel: &str) -> Laid {
    let groups: Vec<usize> = items.iter().enumerate().filter(|(_, i)| i.0 == FormKind::Group).map(|(k, _)| k).collect();
    // The line an item takes up: a list that drops down takes the line of a closed one.
    let span = |k: usize, (kind, r): (FormKind, Box4)| (r.1, r.1 + if matches!(kind, FormKind::Combo | FormKind::Pick) { OLD_CLOSED } else { r.3 }, k);
    let group_of = |(kind, r): (FormKind, Box4)| {
        let (top, bottom, _) = span(0, (kind, r));
        let mid = (top + bottom) / 2;
        groups.iter().copied().find(|&g| items.get(g).is_some_and(|(_, gr)| r.0 >= gr.0 && r.0 < gr.0 + gr.2 && mid >= gr.1 && mid < gr.1 + gr.3))
    };
    // (A line of words with no words, which the apps give a height of nothing, takes no room.)
    let mut order: Vec<(i32, i32, usize)> = items.iter().enumerate().filter(|(_, i)| i.0 != FormKind::Group && !(i.0 == FormKind::Label && (i.1).3 == 0)).map(|(k, i)| span(k, *i)).collect();
    order.sort_by_key(|&(top, _, k)| (top, items.get(k).map_or(0, |i| (i.1).0)));
    // Items that stand on one line are a row.
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut range = (0, 0);
    for (top, bottom, k) in order {
        let overlap = bottom.min(range.1) - top.max(range.0);
        if !rows.is_empty() && overlap * 2 >= (bottom - top).min(range.1 - range.0) {
            range = (range.0.min(top), range.1.max(bottom));
            if let Some(row) = rows.last_mut() {
                row.push(k);
            }
        } else {
            rows.push(vec![k]);
            range = (top, bottom);
        }
    }
    let x_base = items.iter().map(|i| (i.1).0).min().unwrap_or(0);
    let dx = PAD - x_base;
    let mut rects: Vec<Box4> = items.iter().map(|&(_, r)| (r.0 + dx, r.1, r.2, r.3)).collect();
    let (mut y, mut last_group) = (PAD, None);
    for row in &rows {
        let g = row.first().and_then(|&k| items.get(k)).and_then(|&i| group_of(i));
        if g.is_some() && g != last_group {
            if y > PAD {
                y += SECTION_GAP;
            }
            if let Some(gr) = g.and_then(|g| rects.get_mut(g)) {
                *gr = (gr.0, y, gr.2, HEAD_H);
            }
            y += HEAD_H + HEAD_GAP;
        }
        last_group = g;
        let h = row.iter().filter_map(|&k| items.get(k)).map(|&(kind, r)| new_height(kind, r.3)).max().unwrap_or(0);
        for &k in row {
            let (Some(&(kind, old)), Some(slot)) = (items.get(k), rects.get_mut(k)) else { continue };
            let nh = new_height(kind, old.3);
            *slot = (slot.0, y + (h - nh) / 2, old.2, nh);
        }
        y += h + ROW_GAP;
    }
    let bottom = (y - ROW_GAP).max(PAD);
    let mut right = rects.iter().zip(items).filter(|(_, i)| i.0 != FormKind::Group).map(|(r, _)| r.0 + r.2).max().unwrap_or(0);
    let preview = preview.map(|p| {
        let r = (p.0 + dx, PAD, p.2, (bottom - PAD).max(p.3 / 2));
        right = right.max(r.0 + r.2);
        r
    });
    let (ok_w, cancel_w) = ((text_w(ok) + 32).max(BTN_MIN), (text_w(cancel) + 32).max(BTN_MIN));
    let width = (right + PAD).max(2 * PAD + ok_w + cancel_w + BTN_GAP);
    let foot = bottom + PAD;
    let cancel_box = (width - PAD - cancel_w, foot, cancel_w, BTN_H);
    let ok_box = (cancel_box.0 - BTN_GAP - ok_w, foot, ok_w, BTN_H);
    Laid { rects, preview, size: (width, foot + BTN_H + PAD), ok: ok_box, cancel: cancel_box }
}

/// The edit box inside the frame an edit box is drawn in: the frame is `r` and the line of text `line` high, both in pixels of a screen of `dpi` dots per inch.
pub fn edit_inner(r: Box4, line: i32, dpi: u32) -> Box4 {
    let pad = px(1 + EDIT_PAD, dpi);
    (r.0 + pad, r.1 + (r.3 - line) / 2, (r.2 - 2 * pad).max(1), line)
}

/// How a control is at the moment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub hover: bool,
    pub pressed: bool,
    pub focus: bool,
    pub checked: bool,
    pub enabled: bool,
}

fn px(v: i32, dpi: u32) -> i32 {
    v * dpi as i32 / 96
}

/// A push button: the one that is the answer (`primary`) is filled with the accent and its words are white; the others have an outline. The one the keyboard
/// is on has a ring.
pub fn paint_button(p: &mut dyn Painter, r: Rect, text: &str, primary: bool, st: State, sk: &Skin, dpi: u32) {
    let radius = px(RADIUS, dpi);
    p.fill_rect(r, sk.surface);
    p.set_font(Font::Body);
    let ink;
    if primary {
        let fill = if !st.enabled {
            accent::mix(sk.surface, sk.accent, 90)
        } else if st.pressed {
            accent::mix(sk.accent_hover, 0x000000, 40)
        } else if st.hover {
            sk.accent_hover
        } else {
            sk.accent
        };
        p.fill_shape(r, [radius; 4], fill, 255);
        ink = sk.on_accent;
        if st.focus {
            p.stroke_round(Rect { x: r.x + px(2, dpi), y: r.y + px(2, dpi), w: r.w - px(4, dpi), h: r.h - px(4, dpi) }, radius - px(2, dpi), px(1, dpi).max(1), sk.on_accent, 255);
        }
    } else {
        p.fill_shape(r, [radius; 4], sk.surface, 255);
        if st.hover || st.pressed {
            p.fill_shape(r, [radius; 4], sk.hover, if st.pressed { 40 } else { 22 });
        }
        p.stroke_round(r, radius, px(1, dpi).max(1), if st.focus { sk.accent } else { sk.line }, 255);
        if st.focus {
            p.stroke_round(Rect { x: r.x + px(1, dpi), y: r.y + px(1, dpi), w: r.w - px(2, dpi), h: r.h - px(2, dpi) }, radius, px(1, dpi).max(1), sk.accent, 255);
        }
        ink = if st.enabled { sk.text } else { sk.off };
    }
    let (w, h) = (p.text_width(text), p.line_height());
    p.text(r.x + (r.w - w) / 2, r.y + (r.h - h) / 2, text, ink);
}

/// A tick box (or a round one, a radio button) with its words: the box is 20 pixels, in the accent and with a white tick when ticked.
pub fn paint_check(p: &mut dyn Painter, r: Rect, text: &str, round: bool, st: State, sk: &Skin, dpi: u32) {
    p.fill_rect(r, sk.surface);
    p.set_font(Font::Body);
    let side = px(20, dpi);
    let b = Rect { x: r.x + px(1, dpi), y: r.y + (r.h - side) / 2, w: side, h: side };
    let radius = if round { side / 2 } else { px(5, dpi) };
    let edge = if st.hover || st.pressed { sk.accent } else { sk.dim };
    if st.checked {
        p.fill_shape(b, [radius; 4], if st.pressed { sk.accent_hover } else { sk.accent }, 255);
        let (cx, cy, k) = ((b.x + b.w / 2) as f32, (b.y + b.h / 2) as f32, dpi as f32 / 96.0);
        if round {
            let d = px(8, dpi);
            p.fill_shape(Rect { x: b.x + (b.w - d) / 2, y: b.y + (b.h - d) / 2, w: d, h: d }, [d / 2; 4], sk.on_accent, 255);
        } else {
            p.polyline(&[(cx - 4.5 * k, cy + 0.3 * k), (cx - 1.5 * k, cy + 3.3 * k), (cx + 4.8 * k, cy - 3.6 * k)], (1.8 * k).max(1.5), sk.on_accent, 255);
        }
    } else {
        p.fill_shape(b, [radius; 4], sk.field, 255);
        p.stroke_round(b, radius, px(1, dpi).max(1), edge, 255);
    }
    if st.focus {
        p.stroke_round(Rect { x: r.x, y: r.y + (r.h - side) / 2 - px(2, dpi), w: r.w, h: side + px(4, dpi) }, px(7, dpi), px(1, dpi).max(1), sk.accent, 255);
    }
    let h = p.line_height();
    p.text(r.x + side + px(10, dpi), r.y + (r.h - h) / 2, text, if st.enabled { sk.text } else { sk.off });
}

/// What a key does to a list that drops down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListKey {
    Open,
    Prev,
    Next,
}

/// What a key (a virtual-key code, `alt` if Alt is held) does to a list that drops down. F4 and Alt with an arrow open it, in any list. A list that is
/// only a list (`own`: it has no box to type in) also opens with the space bar, and its arrows go to the entry before and the entry after; in the box a
/// list goes with the arrows are the box's own.
pub fn list_key(vk: u32, alt: bool, own: bool) -> Option<ListKey> {
    const F4: u32 = 0x73;
    const SPACE: u32 = 0x20;
    const UP: u32 = 0x26;
    const DOWN: u32 = 0x28;
    match (vk, alt) {
        (F4, _) | (UP | DOWN, true) => Some(ListKey::Open),
        (SPACE, false) if own => Some(ListKey::Open),
        (UP, false) if own => Some(ListKey::Prev),
        (DOWN, false) if own => Some(ListKey::Next),
        _ => None,
    }
}

/// The entry a list is at after `key` moved it (`Prev` or `Next`) among `len` entries, from `selected` (none: no entry chosen, then the first); the same when
/// it cannot move, and none for a list with no entries.
pub fn step(selected: Option<usize>, len: usize, key: ListKey) -> Option<usize> {
    let last = len.checked_sub(1)?;
    Some(match (key, selected) {
        (ListKey::Open, s) => return s.filter(|&s| s <= last),
        (_, None) => 0,
        (ListKey::Prev, Some(s)) => s.min(last).saturating_sub(1),
        (ListKey::Next, Some(s)) => (s + 1).min(last),
    })
}

/// The width of the arrow of a list that goes with an edit box, at 96 dpi.
pub const ARROW_W: i32 = 28;

/// A list that drops down: the frame of an edit box with the entry chosen in it and a chevron at its right end; the frame is the accent while the keyboard is in
/// it, the pointer is on it or the list is open.
pub fn paint_dropdown(p: &mut dyn Painter, r: Rect, text: &str, st: State, sk: &Skin, dpi: u32) {
    paint_edit_frame(p, r, st.focus || st.hover || st.pressed, sk, dpi);
    p.set_font(Font::Body);
    let h = p.line_height();
    let ink = if st.enabled { sk.text } else { sk.off };
    // (A long entry is cut short before the chevron.)
    let text = fit_text(p, text, r.w - px(2 * (1 + EDIT_PAD) + ARROW_W, dpi));
    p.text(r.x + px(1 + EDIT_PAD, dpi), r.y + (r.h - h) / 2, &text, ink);
    p.glyph(Rect { x: r.x + r.w - px(ARROW_W + 2, dpi), y: r.y, w: px(ARROW_W, dpi), h: r.h }, '\u{E70D}', sk.dim);
}

/// The arrow that opens the list of an edit box (it stands in the frame of the box, at its right end): a chevron, with a pale fill under it while the pointer is on it.
pub fn paint_arrow(p: &mut dyn Painter, r: Rect, st: State, sk: &Skin, dpi: u32) {
    p.fill_rect(r, sk.field);
    if st.hover || st.pressed {
        p.fill_shape(r, [px(RADIUS - 2, dpi); 4], sk.hover, if st.pressed { 40 } else { 22 });
    }
    p.glyph(r, '\u{E70D}', sk.dim);
}

/// The frame an edit box stands in: 1 pixel, and the accent while the keyboard is in it.
pub fn paint_edit_frame(p: &mut dyn Painter, r: Rect, focus: bool, sk: &Skin, dpi: u32) {
    let radius = px(RADIUS, dpi);
    p.fill_rect(r, sk.surface);
    p.fill_shape(r, [radius; 4], sk.field, 255);
    p.stroke_round(r, radius, px(1, dpi).max(1), if focus { sk.accent } else { sk.line }, 255);
}

#[cfg(test)]
mod tests {
    use super::*;
    use FormKind::*;

    #[derive(Default)]
    struct Rec {
        shapes: Vec<(Rect, u32)>,
        strokes: Vec<(Rect, u32, i32)>,
        texts: Vec<(i32, String, u32)>,
        lines: usize,
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
        fn fill_shape(&mut self, r: Rect, _radii: [i32; 4], c: u32, _a: u8) {
            self.shapes.push((r, c));
        }
        fn stroke_round(&mut self, r: Rect, _radius: i32, w: i32, c: u32, _a: u8) {
            self.strokes.push((r, c, w));
        }
        fn polyline(&mut self, _p: &[(f32, f32)], _w: f32, _c: u32, _a: u8) {
            self.lines += 1;
        }
        fn glyph(&mut self, _r: Rect, ch: char, _c: u32) {
            self.glyphs.push(ch);
        }
    }

    fn skin() -> Skin {
        Skin { surface: 0xFAFBFB, text: 0x1E2422, dim: 0x5E6865, off: 0xA3ABA8, line: 0xD6DBD9, field: 0xEFF2F1, accent: 0x1D9E75, accent_hover: 0x0F6E56, tint: 0xE1F5EE, on_tint: 0x0F6E56, on_accent: 0xFFFFFF, hover: 0x1E2422 }
    }

    fn overlap(a: Box4, b: Box4) -> bool {
        a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3
    }

    /// The box of the page numbers as the app lays it out (the groups in frames, the lists as tall as their lists).
    fn pagenum() -> (Vec<(FormKind, Box4)>, Box4) {
        let items = vec![
            (Group, (12, 6, 616, 66)),
            (Label, (24, 36, 36, 18)),
            (Combo, (60, 32, 150, 150)),
            (Label, (222, 36, 36, 18)),
            (Combo, (258, 32, 150, 150)),
            (Group, (12, 80, 616, 66)),
            (Label, (24, 110, 36, 18)),
            (Combo, (60, 106, 150, 150)),
            (Label, (12, 152, 616, 18)),
            (Label, (12, 184, 40, 18)),
            (Edit, (54, 180, 50, 24)),
            (Label, (116, 184, 40, 18)),
            (Pick, (158, 180, 84, 150)),
            (Check, (12, 246, 380, 22)),
            (Check, (400, 246, 220, 22)),
            (Button, (12, 282, 280, 28)),
        ];
        (items, (644, 10, 164, 232))
    }

    #[test]
    fn a_box_gets_24_pixels_of_room_32_pixel_controls_and_a_row_of_buttons_at_the_foot() {
        let (items, preview) = pagenum();
        let laid = lay_out(&items, Some(preview), "OK", "Cancel");
        assert_eq!(laid.rects.len(), items.len());
        // The groups' headings and the lines that are not in a group start 24 from the left; what is in a group is in a little from there.
        assert_eq!((laid.rects[0].0, laid.rects[8].0, laid.rects[1].0), (PAD, PAD, PAD + 12));
        // The first thing is 24 down from the top, and nothing else is above it.
        assert_eq!(laid.rects[0].1, PAD);
        // The controls are 32 high (a list too: its list is a menu), checks 24, and the labels are in the middle of their row.
        for (k, (kind, _)) in items.iter().enumerate() {
            let (h, want) = (laid.rects[k].3, match kind { Edit | Secret | Button => CONTROL_H, Check => CHECK_H, Label => LABEL_H, Group => HEAD_H, Combo | Pick => CONTROL_H });
            assert_eq!(h, want, "{kind:?} #{k}");
        }
        // A label and the control beside it are centred on each other.
        let centre = |k: usize| laid.rects[k].1 * 2 + laid.rects[k].3;
        assert_eq!(centre(1), laid.rects[2].1 * 2 + CONTROL_H);
        assert_eq!(centre(9), centre(10));
        // The buttons: Cancel at the right edge with 24 round it, OK beside it, both 32 high, and the box ends 24 under them.
        let (w, h) = laid.size;
        assert_eq!(laid.cancel.0 + laid.cancel.2, w - PAD);
        assert_eq!(laid.ok.0 + laid.ok.2 + 8, laid.cancel.0);
        assert_eq!((laid.ok.3, laid.cancel.3, laid.ok.1), (BTN_H, BTN_H, laid.cancel.1));
        assert_eq!(h, laid.cancel.1 + BTN_H + PAD);
        // The sketch is as high as the rows and inside the box on the right.
        let pv = laid.preview.expect("a sketch");
        assert_eq!((pv.1, pv.0 + pv.2 + PAD), (PAD, w));
        // Nothing overlaps anything (a group is its heading), and nothing leaves the box.
        for (a, ra) in laid.rects.iter().enumerate() {
            assert!(ra.0 >= PAD && ra.0 + ra.2 <= w - PAD && ra.1 + ra.3 <= laid.ok.1, "#{a} {ra:?}");
            for (b, rb) in laid.rects.iter().enumerate().skip(a + 1) {
                assert!(!overlap(*ra, *rb), "#{a} {ra:?} and #{b} {rb:?}");
            }
        }
        // The two groups are headings with the rows of the group under each, the second after the first.
        assert!(laid.rects[0].1 + HEAD_H <= laid.rects[1].1.min(laid.rects[2].1));
        assert!(laid.rects[2].1 + CONTROL_H <= laid.rects[5].1);
        assert!(laid.rects[5].1 + HEAD_H <= laid.rects[7].1);
    }

    #[test]
    fn rows_are_found_by_where_the_items_stand_and_a_block_of_words_is_a_row_of_its_own() {
        // The old box of the crop: a paragraph, then four margins in two columns, a tick box, a list and a range.
        let items = vec![
            (Label, (12, 8, 456, 34)),
            (Label, (12, 53, 56, 18)),
            (Edit, (70, 50, 90, 22)),
            (Label, (244, 53, 56, 18)),
            (Edit, (302, 50, 90, 22)),
            (Label, (12, 81, 56, 18)),
            (Edit, (70, 78, 90, 22)),
            (Check, (12, 110, 456, 20)),
            (Label, (12, 140, 90, 18)),
            (Pick, (104, 136, 364, 200)),
        ];
        let laid = lay_out(&items, None, "Crop", "Cancel");
        // Two margins on a row (the same top), the next row lower by a control and a gap.
        assert_eq!(laid.rects[2].1, laid.rects[4].1);
        assert_eq!(laid.rects[6].1, laid.rects[2].1 + CONTROL_H + ROW_GAP);
        // The paragraph is a block: taller than a line, and the rows start after it.
        assert!(laid.rects[0].3 > LABEL_H);
        assert!(laid.rects[2].1 >= laid.rects[0].1 + laid.rects[0].3 + ROW_GAP);
        assert!(laid.preview.is_none());
        // No sketch, no groups: the width is that of the widest row and the room.
        assert_eq!(laid.size.0, laid.rects[7].0 + laid.rects[7].2 + PAD);
    }

    #[test]
    fn a_line_of_words_with_no_words_takes_no_room() {
        let items = vec![(Label, (12, 8, 400, 18)), (Label, (12, 28, 400, 0)), (Label, (12, 48, 400, 0)), (Edit, (12, 68, 400, 24))];
        let laid = lay_out(&items, None, "OK", "Cancel");
        // The edit box comes straight after the first line.
        assert_eq!(laid.rects[3].1, laid.rects[0].1 + LABEL_H + ROW_GAP);
        assert_eq!((laid.rects[1].3, laid.rects[2].3), (0, 0));
    }

    #[test]
    fn a_box_with_nothing_in_it_still_holds_its_two_buttons_and_long_words_make_a_button_wider() {
        let laid = lay_out(&[], None, "OK", "Cancel");
        assert!(laid.size.0 >= 2 * PAD + 2 * BTN_MIN + 8);
        assert_eq!(laid.size.1, PAD + PAD + BTN_H + PAD);
        let wide = lay_out(&[], None, "Save an encrypted copy", "Cancel");
        assert!(wide.ok.2 > BTN_MIN && wide.cancel.2 == BTN_MIN);
        let zh = lay_out(&[], None, "保存加密的副本", "取消");
        assert!(zh.ok.2 > BTN_MIN, "{}", zh.ok.2);
    }

    #[test]
    fn the_line_of_an_edit_box_stands_in_the_middle_of_its_frame() {
        let frame = (24, 100, 300, CONTROL_H);
        let inner = edit_inner(frame, 20, 96);
        assert_eq!((inner.1, inner.3), (106, 20));
        assert_eq!(inner.0, 24 + 1 + EDIT_PAD);
        assert_eq!(inner.0 + inner.2 + EDIT_PAD + 1, frame.0 + frame.2);
        // At twice the dots per inch the room at the sides is twice as wide.
        let big = edit_inner((48, 200, 600, 64), 40, 192);
        assert_eq!((big.0 - 48, big.1 - 200, 48 + 600 - big.0 - big.2), (2 * (1 + EDIT_PAD), 12, 2 * (1 + EDIT_PAD)));
    }

    #[test]
    fn the_answer_is_filled_with_the_accent_the_others_are_outlined_and_the_state_shows() {
        let sk = skin();
        let r = Rect { x: 0, y: 0, w: 100, h: 32 };
        let mut p = Rec::default();
        paint_button(&mut p, r, "OK", true, State { enabled: true, ..State::default() }, &sk, 96);
        assert_eq!(p.shapes.last().map(|s| s.1), Some(0x1D9E75));
        assert_eq!(p.texts[0].2, 0xFFFFFF);
        let mut p = Rec::default();
        paint_button(&mut p, r, "OK", true, State { enabled: true, hover: true, ..State::default() }, &sk, 96);
        assert_eq!(p.shapes.last().map(|s| s.1), Some(0x0F6E56));
        let mut p = Rec::default();
        paint_button(&mut p, r, "OK", true, State { enabled: true, pressed: true, hover: true, ..State::default() }, &sk, 96);
        assert!(p.shapes.last().is_some_and(|s| s.1 != 0x1D9E75 && s.1 != 0x0F6E56));
        // Outlined: a line of the hairline colour, the accent while the keyboard is on it, and the words in the ink.
        let mut p = Rec::default();
        paint_button(&mut p, r, "Cancel", false, State { enabled: true, ..State::default() }, &sk, 96);
        assert_eq!((p.strokes.len(), p.strokes[0].1, p.texts[0].2), (1, 0xD6DBD9, 0x1E2422));
        let mut p = Rec::default();
        paint_button(&mut p, r, "Cancel", false, State { enabled: true, focus: true, ..State::default() }, &sk, 96);
        assert!(p.strokes.iter().all(|s| s.1 == 0x1D9E75));
        // Centred words.
        assert_eq!(p.texts[0].0, (100 - 7 * 6) / 2);
    }

    #[test]
    fn a_tick_box_is_filled_with_the_accent_and_ticked_or_an_outline_and_a_radio_button_is_round() {
        let sk = skin();
        let r = Rect { x: 0, y: 0, w: 200, h: 24 };
        let mut p = Rec::default();
        paint_check(&mut p, r, "Mirror", false, State { enabled: true, checked: true, ..State::default() }, &sk, 96);
        assert_eq!((p.shapes.last().map(|s| s.1), p.lines), (Some(0x1D9E75), 1));
        assert_eq!(p.texts[0].1, "Mirror");
        let mut p = Rec::default();
        paint_check(&mut p, r, "Mirror", false, State { enabled: true, ..State::default() }, &sk, 96);
        assert_eq!((p.lines, p.strokes.len()), (0, 1));
        // The box is 20 pixels, 40 at twice the dots per inch, and the words come after it.
        let mut big = Rec::default();
        paint_check(&mut big, Rect { x: 0, y: 0, w: 400, h: 48 }, "Mirror", false, State { enabled: true, ..State::default() }, &sk, 192);
        assert_eq!((p.shapes.last().map(|s| s.0.w), big.shapes.last().map(|s| s.0.w)), (Some(20), Some(40)));
        assert!(big.texts[0].0 > p.texts[0].0);
        // A radio button ticked has a dot, not a tick.
        let mut p = Rec::default();
        paint_check(&mut p, r, "A", true, State { enabled: true, checked: true, ..State::default() }, &sk, 96);
        assert_eq!(p.lines, 0);
        assert_eq!(p.shapes.len(), 2);
    }

    #[test]
    fn a_list_that_drops_down_is_a_frame_with_the_entry_and_a_chevron_and_its_arrow_goes_with_an_edit_box() {
        let sk = skin();
        let r = Rect { x: 0, y: 0, w: 200, h: 32 };
        let mut p = Rec::default();
        paint_dropdown(&mut p, r, "Black", State { enabled: true, ..State::default() }, &sk, 96);
        assert_eq!((p.strokes[0].1, p.texts[0].1.as_str(), p.texts[0].2), (0xD6DBD9, "Black", 0x1E2422));
        assert_eq!(p.texts[0].0, 1 + EDIT_PAD);
        assert_eq!(p.glyphs, vec!['\u{E70D}']);
        // The pointer on it, the keyboard in it or the list open: the frame is the accent.
        for st in [State { enabled: true, hover: true, ..State::default() }, State { enabled: true, focus: true, ..State::default() }, State { enabled: true, pressed: true, ..State::default() }] {
            let mut p = Rec::default();
            paint_dropdown(&mut p, r, "Black", st, &sk, 96);
            assert_eq!(p.strokes[0].1, 0x1D9E75);
        }
        // The arrow of an edit box: the ground of the box, a pale fill only while the pointer is on it.
        let mut p = Rec::default();
        paint_arrow(&mut p, Rect { x: 0, y: 0, w: 28, h: 28 }, State { enabled: true, ..State::default() }, &sk, 96);
        assert_eq!((p.shapes.len(), p.glyphs.len()), (0, 1));
        let mut p = Rec::default();
        paint_arrow(&mut p, Rect { x: 0, y: 0, w: 28, h: 28 }, State { enabled: true, hover: true, ..State::default() }, &sk, 96);
        assert_eq!(p.shapes.len(), 1);
    }

    #[test]
    fn f4_and_alt_with_an_arrow_open_a_list_the_space_bar_and_the_arrows_are_for_a_list_without_a_box_to_type_in() {
        let (f4, space, up, down, enter) = (0x73, 0x20, 0x26, 0x28, 0x0D);
        for own in [true, false] {
            assert_eq!(list_key(f4, false, own), Some(ListKey::Open), "F4 opens any list");
            assert_eq!(list_key(down, true, own), Some(ListKey::Open), "Alt+Down opens any list");
            assert_eq!(list_key(up, true, own), Some(ListKey::Open));
            assert_eq!(list_key(enter, false, own), None);
        }
        assert_eq!((list_key(space, false, true), list_key(up, false, true), list_key(down, false, true)), (Some(ListKey::Open), Some(ListKey::Prev), Some(ListKey::Next)));
        // In the box a list goes with, the space bar types a space and the arrows are the box's.
        assert_eq!((list_key(space, false, false), list_key(up, false, false), list_key(down, false, false)), (None, None, None));
    }

    #[test]
    fn the_arrows_step_through_the_entries_and_stop_at_the_ends() {
        assert_eq!(step(Some(2), 6, ListKey::Next), Some(3));
        assert_eq!(step(Some(5), 6, ListKey::Next), Some(5));
        assert_eq!(step(Some(0), 6, ListKey::Prev), Some(0));
        assert_eq!(step(Some(3), 6, ListKey::Prev), Some(2));
        // With no entry chosen the first is; with nothing to choose from there is no answer; an entry out of the list is held to it.
        assert_eq!((step(None, 6, ListKey::Next), step(None, 6, ListKey::Prev)), (Some(0), Some(0)));
        assert_eq!((step(Some(1), 0, ListKey::Next), step(None, 0, ListKey::Prev)), (None, None));
        assert_eq!(step(Some(9), 3, ListKey::Prev), Some(1));
        assert_eq!(step(Some(9), 3, ListKey::Open), None);
    }

    #[test]
    fn the_frame_of_an_edit_box_is_one_pixel_and_turns_to_the_accent_with_the_keyboard() {
        let sk = skin();
        let r = Rect { x: 0, y: 0, w: 200, h: 32 };
        let mut p = Rec::default();
        paint_edit_frame(&mut p, r, false, &sk, 96);
        assert_eq!((p.strokes[0].1, p.strokes[0].2, p.shapes[0].1), (0xD6DBD9, 1, 0xEFF2F1));
        let mut p = Rec::default();
        paint_edit_frame(&mut p, r, true, &sk, 96);
        assert_eq!((p.strokes[0].1, p.strokes[0].2), (0x1D9E75, 1));
    }
}
