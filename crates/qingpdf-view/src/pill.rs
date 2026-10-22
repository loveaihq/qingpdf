//! The page pill (2026-10-21): the rounded card that floats at the bottom centre of the pages, with ‹ page / pages ›, − zoom +, and a button that fits the
//! pages to the window. Plain arithmetic: where each part is for a page area of a given place and size and a given number of dots per inch, what a
//! place of it is on, how the numbers are written and when it fades. `app/overlay.rs` draws it.

use crate::ui::Rect;

/// Sizes at 96 dpi: the height of the card, a button's side, the room at its ends, the width of a separator, and the distance from the bottom of the
/// pages.
pub const H: i32 = 40;
const BTN: i32 = 32;
const PAD: i32 = 4;
const SEP: i32 = 9;
pub const FROM_BOTTOM: i32 = 20;
/// The room round the words of the page and of the zoom.
const TEXT_PAD: i32 = 6;

/// While the pages are scrolled the pill is faint (so that it does not hide a link under it for long), and it comes back a little after the last scroll.
pub const FADE_HOLD: u64 = 900;
pub const FADE_BACK: u64 = 250;
const FAINT: u8 = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Prev,
    Page,
    Next,
    ZoomOut,
    Zoom,
    ZoomIn,
    Fit,
}

/// Where the card, its parts and the two thin lines between its groups are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub card: Rect,
    pub parts: Vec<(Part, Rect)>,
    pub seps: [Rect; 2],
}

/// The pill for a page area `area` (in the window's pixels) at `dpi`: `page_w` and `zoom_w` are the widths of the longest page text ("120 / 120") and
/// zoom text ("1000%") there can be, so that the pill does not change its width while the pages are turned.
pub fn layout(area: Rect, dpi: u32, page_w: i32, zoom_w: i32) -> Layout {
    let s = |v: i32| (i64::from(v) * i64::from(dpi.max(1)) / 96) as i32;
    let (h, btn, pad, sep, tp) = (s(H), s(BTN), s(PAD), s(SEP), s(TEXT_PAD));
    let (page_w, zoom_w) = (page_w.max(0) + 2 * tp, zoom_w.max(0) + 2 * tp);
    let width = pad * 2 + btn * 4 + page_w + zoom_w + btn + 2 * sep;
    let card = Rect { x: area.x + (area.w - width) / 2, y: area.y + area.h - s(FROM_BOTTOM) - h, w: width, h };
    let y = card.y + (h - btn) / 2;
    let prev = card.x + pad;
    let page = prev + btn;
    let next = page + page_w;
    let first_sep = next + btn;
    let zoom_out = first_sep + sep;
    let zoom = zoom_out + btn;
    let zoom_in = zoom + zoom_w;
    let second_sep = zoom_in + btn;
    let fit = second_sep + sep;
    let at = |x: i32, w: i32| Rect { x, y, w, h: btn };
    let parts = vec![
        (Part::Prev, at(prev, btn)),
        (Part::Page, at(page, page_w)),
        (Part::Next, at(next, btn)),
        (Part::ZoomOut, at(zoom_out, btn)),
        (Part::Zoom, at(zoom, zoom_w)),
        (Part::ZoomIn, at(zoom_in, btn)),
        (Part::Fit, at(fit, btn)),
    ];
    let sep1 = Rect { x: first_sep + sep / 2, y: card.y + s(11), w: s(1).max(1), h: h - 2 * s(11) };
    let sep2 = Rect { x: second_sep + sep / 2, ..sep1 };
    Layout { card, parts, seps: [sep1, sep2] }
}

fn inside(r: Rect, x: i32, y: i32) -> bool {
    x >= r.x && y >= r.y && x < r.x.saturating_add(r.w) && y < r.y.saturating_add(r.h)
}

/// Is this place on the card (a click there is the card's, whatever part it is on)?
pub fn on_card(l: &Layout, x: i32, y: i32) -> bool {
    inside(l.card, x, y)
}

/// The part at this place, if it is on one.
pub fn hit(l: &Layout, x: i32, y: i32) -> Option<Part> {
    l.parts.iter().find(|(_, r)| inside(*r, x, y)).map(|(p, _)| *p)
}

/// The page or pages in view as the pill writes them: "3 / 120", "2-3 / 120" for the two pages of a spread. (`page` and `last` count from 1.)
pub fn page_label(page: usize, last: usize, total: usize) -> String {
    if total == 0 {
        String::new()
    } else if last > page {
        format!("{page}-{last} / {total}")
    } else {
        format!("{page} / {total}")
    }
}

/// The widest `page_label` for `total` pages (what the pill is made wide enough for).
pub fn widest_page_label(total: usize) -> String {
    if total < 2 { page_label(1, 1, total) } else { page_label(total - 1, total, total) }
}

/// The zoom (in thousandths: 1000 is 100 percent) as the pill writes it.
pub fn zoom_label(zoom: u32) -> String {
    format!("{}%", (f64::from(zoom) / 10.0).round() as u32)
}

/// The zoom a person typed: a number of percent, with the percent sign if they like (`150`, ` 150 % `, `1.5e2` is not a number), in thousandths and
/// held to what the reader allows. `None` when there is no number in it.
pub fn parse_zoom(text: &str, least: u32, most: u32) -> Option<u32> {
    let digits: String = text.trim().trim_end_matches('%').trim().chars().filter(|c| !c.is_whitespace()).collect();
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit() || c == '.') || digits.matches('.').count() > 1 {
        return None;
    }
    let percent: f64 = digits.parse().ok()?;
    (percent > 0.0).then(|| crate::ui::clamp_to((percent * 10.0).round().min(f64::from(u32::MAX)) as u32, least, most))
}

/// How opaque the pill is, 0 to 255: faint from a scroll until `FADE_HOLD` later, then back to full over `FADE_BACK`; full when the pointer is on it. (`scrolled`
/// is when the pages were last scrolled, in milliseconds on the reader's clock.)
pub fn alpha(now: u64, scrolled: Option<u64>, hover: bool) -> u8 {
    if hover {
        return 255;
    }
    let Some(since) = scrolled.map(|t| now.saturating_sub(t)) else { return 255 };
    if since < FADE_HOLD {
        FAINT
    } else if since < FADE_HOLD + FADE_BACK {
        let k = (since - FADE_HOLD) * 255 / FADE_BACK;
        (u64::from(FAINT) + (255 - u64::from(FAINT)) * k / 255) as u8
    } else {
        255
    }
}

/// Is the pill to be used (the click is its, not the page's)? Not while it is faint: it lets the click through to what is under it.
pub fn usable(alpha: u8) -> bool {
    alpha >= 200
}

/// Does the pill change by itself at `now` (a timer is needed)?
pub fn moving(now: u64, scrolled: Option<u64>) -> bool {
    scrolled.is_some_and(|t| now.saturating_sub(t) < FADE_HOLD + FADE_BACK)
}

/// Does the picture of the pill change as time passes at `now` (it comes back from faint), so that the clock must run at its fast pace?
pub fn changing(now: u64, scrolled: Option<u64>) -> bool {
    scrolled.is_some_and(|t| (FADE_HOLD..FADE_HOLD + FADE_BACK).contains(&now.saturating_sub(t)))
}

/// How long (milliseconds) until a pill that is faint begins to come back (none: it is not waiting for that).
pub fn hold_left(now: u64, scrolled: Option<u64>) -> Option<u64> {
    scrolled.map(|t| now.saturating_sub(t)).filter(|&since| since < FADE_HOLD).map(|since| FADE_HOLD - since)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect { x: 200, y: 84, w: 1000, h: 700 };

    #[test]
    fn the_card_floats_at_the_bottom_centre_of_the_pages_a_little_up_from_the_edge() {
        let l = layout(AREA, 96, 60, 40);
        assert_eq!(l.card.h, 40);
        assert_eq!(l.card.y + l.card.h + 20, AREA.y + AREA.h);
        let (left, right) = (l.card.x - AREA.x, AREA.x + AREA.w - (l.card.x + l.card.w));
        assert!((left - right).abs() <= 1, "{left} {right}");
        // The parts are in order, between the card's ends, and inside its height.
        let order: Vec<Part> = l.parts.iter().map(|p| p.0).collect();
        assert_eq!(order, vec![Part::Prev, Part::Page, Part::Next, Part::ZoomOut, Part::Zoom, Part::ZoomIn, Part::Fit]);
        let mut edge = l.card.x;
        for (_, r) in &l.parts {
            assert!(r.x >= edge && r.y >= l.card.y && r.y + r.h <= l.card.y + l.card.h, "{r:?}");
            edge = r.x + r.w;
        }
        assert!(edge <= l.card.x + l.card.w);
        // The lines between the groups fall in the gaps between parts.
        assert!(l.seps[0].x >= l.parts[2].1.x + 32 && l.seps[0].x < l.parts[3].1.x);
        assert!(l.seps[1].x >= l.parts[5].1.x + 32 && l.seps[1].x < l.parts[6].1.x);
    }

    #[test]
    fn the_card_follows_the_dots_per_inch_and_the_width_of_its_words() {
        let one = layout(AREA, 96, 60, 40);
        let wider = layout(AREA, 96, 80, 40);
        assert_eq!(wider.card.w, one.card.w + 20);
        let two = layout(Rect { x: 0, y: 0, w: 2000, h: 1400 }, 192, 120, 80);
        assert_eq!((two.card.h, two.parts[0].1.w), (80, 64));
        // A page area that is not wide enough still gets a card (it runs over the edges, and is not negative).
        let tiny = layout(Rect { x: 0, y: 0, w: 100, h: 100 }, 96, 60, 40);
        assert!(tiny.card.w > 100 && tiny.card.y == 100 - 20 - 40);
    }

    #[test]
    fn a_place_is_on_a_part_on_the_card_or_off_it() {
        let l = layout(AREA, 96, 60, 40);
        for (p, r) in &l.parts {
            assert_eq!(hit(&l, r.x + r.w / 2, r.y + r.h / 2), Some(*p));
            assert!(on_card(&l, r.x + 1, r.y + 1));
        }
        // On the card between the parts: the card's, not a part's.
        let s = l.seps[0];
        assert_eq!(hit(&l, s.x, l.card.y + 2), None);
        assert!(on_card(&l, s.x, l.card.y + 2));
        // Off the card: nothing.
        assert!(!on_card(&l, l.card.x - 1, l.card.y + 5) && !on_card(&l, l.card.x + 5, l.card.y - 1) && !on_card(&l, l.card.x + l.card.w, l.card.y + 5));
        assert_eq!(hit(&l, 0, 0), None);
    }

    #[test]
    fn the_page_is_written_as_a_number_over_the_pages_and_two_numbers_for_a_spread() {
        assert_eq!(page_label(3, 3, 120), "3 / 120");
        assert_eq!(page_label(2, 3, 120), "2-3 / 120");
        assert_eq!(page_label(1, 1, 0), "");
        assert_eq!(widest_page_label(120), "119-120 / 120");
        assert_eq!(widest_page_label(1), "1 / 1");
        assert_eq!(widest_page_label(0), "");
        assert!(widest_page_label(9).len() >= page_label(8, 9, 9).len());
        assert_eq!(zoom_label(1000), "100%");
        assert_eq!(zoom_label(1255), "126%");
        assert_eq!(zoom_label(250), "25%");
    }

    #[test]
    fn a_typed_zoom_is_a_number_of_percent_held_to_what_is_allowed() {
        assert_eq!(parse_zoom("150", 100, 8000), Some(1500));
        assert_eq!(parse_zoom(" 150 % ", 100, 8000), Some(1500));
        assert_eq!(parse_zoom("150%", 100, 8000), Some(1500));
        assert_eq!(parse_zoom("33.3", 100, 8000), Some(333));
        assert_eq!(parse_zoom("1", 100, 8000), Some(100));
        assert_eq!(parse_zoom("99999", 100, 8000), Some(8000));
        assert_eq!(parse_zoom("0", 100, 8000), None);
        assert_eq!(parse_zoom("", 100, 8000), None);
        assert_eq!(parse_zoom("%", 100, 8000), None);
        assert_eq!(parse_zoom("abc", 100, 8000), None);
        assert_eq!(parse_zoom("1.2.3", 100, 8000), None);
        assert_eq!(parse_zoom("-5", 100, 8000), None);
        assert_eq!(parse_zoom("1e3", 100, 8000), None);
        assert_eq!(parse_zoom(&"9".repeat(400), 100, 8000), Some(8000));
    }

    #[test]
    fn the_pill_is_faint_while_the_pages_scroll_comes_back_after_a_moment_and_is_full_under_the_pointer() {
        assert_eq!(alpha(5000, None, false), 255);
        assert_eq!(alpha(5000, Some(5000), false), FAINT);
        assert_eq!(alpha(5000 + FADE_HOLD - 1, Some(5000), false), FAINT);
        let mid = alpha(5000 + FADE_HOLD + FADE_BACK / 2, Some(5000), false);
        assert!(mid > FAINT && mid < 255, "{mid}");
        assert_eq!(alpha(5000 + FADE_HOLD + FADE_BACK, Some(5000), false), 255);
        assert_eq!(alpha(5000, Some(5000), true), 255);
        // It is not used while faint, and it is used once it is back; a clock that went back does not break it.
        assert!(!usable(FAINT) && usable(255) && !usable(mid));
        assert_eq!(alpha(4000, Some(5000), false), FAINT);
        assert!(moving(5100, Some(5000)) && !moving(9000, Some(5000)) && !moving(100, None));
    }
}
