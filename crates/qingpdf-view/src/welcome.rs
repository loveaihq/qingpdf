//! The welcome page (2026-10-21): what the window shows when no file is open. The mark and the name with a line under it, the button that opens a file, the
//! hint that a file can be dropped, and the list of the files opened last, each row with the name, the folder (dimmed) and the day (today, yesterday or the
//! date). Plain logic: the days, where everything is, what a place is on, and where the keys move the row that has the keyboard. `app/welcome.rs` draws it.

use crate::i18n::Lang;
use crate::ui::Rect;

// --- the days ----------------------------------------------------------------------------------------------------

/// A day as the list says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rel {
    Today,
    Yesterday,
    /// Any other day. `year_shown` when the year is not this one's (it is told then).
    Date { year: i32, month: u32, day: u32, year_shown: bool },
    /// No day is known (an entry made before the list kept them, or a date that cannot be read).
    Unknown,
}

/// `YYYY-MM-DD` as year, month, day; `None` for anything else (also for a month or a day that cannot be).
pub fn parse_date(s: &str) -> Option<(i32, u32, u32)> {
    let mut parts = s.trim().splitn(3, '-');
    let (y, m, d) = (parts.next()?, parts.next()?, parts.next()?);
    if y.len() != 4 || m.len() != 2 || d.len() != 2 || !(y.bytes().chain(m.bytes()).chain(d.bytes())).all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (y, m, d) = (y.parse::<i32>().ok()?, m.parse::<u32>().ok()?, d.parse::<u32>().ok()?);
    ((1..=12).contains(&m) && d >= 1 && d <= days_in_month(y, m)).then_some((y, m, d))
}

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// The days from 1970-01-01 to this day of the calendar (negative before it).
fn day_number(y: i32, m: u32, d: u32) -> i64 {
    let y = i64::from(y) - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (i64::from(m) + if m > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// How `then` (a day, `YYYY-MM-DD`) is told on `today`: today, yesterday, or its date (with the year when it is another year's).
pub fn relative(today: &str, then: &str) -> Rel {
    let (Some((ty, tm, td)), Some((y, m, d))) = (parse_date(today), parse_date(then)) else { return Rel::Unknown };
    match day_number(ty, tm, td) - day_number(y, m, d) {
        0 => Rel::Today,
        1 => Rel::Yesterday,
        _ => Rel::Date { year: y, month: m, day: d, year_shown: y != ty },
    }
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// The words for a day: "今天", "昨天", "10月15日", "2025年10月15日"; "Today", "Yesterday", "Oct 15", "Oct 15, 2025"; nothing for an unknown day.
pub fn day_text(lang: Lang, rel: Rel) -> String {
    match (lang, rel) {
        (_, Rel::Unknown) => String::new(),
        (Lang::Zh, Rel::Today) => "今天".to_string(),
        (Lang::Zh, Rel::Yesterday) => "昨天".to_string(),
        (Lang::En, Rel::Today) => "Today".to_string(),
        (Lang::En, Rel::Yesterday) => "Yesterday".to_string(),
        (Lang::Zh, Rel::Date { year, month, day, year_shown }) => if year_shown { format!("{year}年{month}月{day}日") } else { format!("{month}月{day}日") },
        (Lang::En, Rel::Date { year, month, day, year_shown }) => {
            let name = MONTHS.get((month as usize).saturating_sub(1)).copied().unwrap_or("");
            if year_shown { format!("{name} {day}, {year}") } else { format!("{name} {day}") }
        }
    }
}

/// The name of a file and the folder it is in, from its path (the folder is empty for a name with no folder).
pub fn split_path(path: &str) -> (&str, &str) {
    match path.rfind(['\\', '/']) {
        Some(i) => (path.get(i + 1..).unwrap_or(path), path.get(..i).unwrap_or("")),
        None => (path, ""),
    }
}

// --- the page ----------------------------------------------------------------------------------------------------

/// Sizes at 96 dpi: the mark, the width of the column, a row of the list, the button that opens a file (its width and height), the room kept at the top and
/// the bottom, and the corner radius of the card the list stands on.
pub const LOGO: i32 = 72;
pub const COLUMN: i32 = 480;
pub const ROW: i32 = 52;
const BUTTON_W: i32 = 168;
const BUTTON_H: i32 = 40;
const MARGIN: i32 = 24;
const CARD_PAD: i32 = 6;
pub const CARD_RADIUS: i32 = 12;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub logo: Rect,
    /// The top of the name of the program and of the line under it, and of the hint about dropping a file.
    pub title_y: i32,
    pub tagline_y: i32,
    pub hint_y: i32,
    pub button: Rect,
    /// The heading of the list (its top), the card the rows stand on, and the rows that fit.
    pub heading_y: i32,
    pub card: Rect,
    pub rows: Vec<Rect>,
}

/// The page for a page area `area` at `dpi`, with `entries` files to list; `title_h` and `line_h` are the heights of the name of the program and of a line of
/// text. The column is in the middle; the list has as many rows as fit (at least one: an empty list says so in it), and the whole is as high in the middle as it
/// can be and no higher than the room.
pub fn layout(area: Rect, dpi: u32, entries: usize, title_h: i32, line_h: i32) -> Layout {
    let s = |v: i32| (i64::from(v) * i64::from(dpi.max(1)) / 96) as i32;
    let (logo, margin, row) = (s(LOGO), s(MARGIN), s(ROW));
    let (button_w, button_h) = (s(BUTTON_W), s(BUTTON_H));
    let head = logo + s(16) + title_h + s(4) + line_h + s(24) + button_h + s(12) + line_h;
    let list_head = s(32) + line_h + s(8);
    let pad = s(CARD_PAD);
    let room = (area.h - 2 * margin - head - list_head - 2 * pad).max(0);
    let rows = if row > 0 { crate::ui::clamp_to(room / row, 0, i32::try_from(entries.max(1)).unwrap_or(1)) } else { 0 };
    // Without the room for even one row the list is left out.
    let (list, rows) = if rows >= 1 { (list_head + rows * row + 2 * pad, rows) } else { (0, 0) };
    let total = head + list;
    let top = area.y + ((area.h - total) / 2).max(margin.min((area.h - total).max(0)));
    let column = s(COLUMN).min(area.w - 2 * margin).max(0);
    let cx = area.x + area.w / 2;
    let left = cx - column / 2;
    let logo_r = Rect { x: cx - logo / 2, y: top, w: logo, h: logo };
    let title_y = top + logo + s(16);
    let tagline_y = title_y + title_h + s(4);
    let button = Rect { x: cx - button_w / 2, y: tagline_y + line_h + s(24), w: button_w, h: button_h };
    let hint_y = button.y + button_h + s(12);
    let heading_y = hint_y + line_h + s(32);
    let card = Rect { x: left, y: heading_y + line_h + s(8), w: column, h: if rows > 0 { rows * row + 2 * pad } else { 0 } };
    let rects = (0..rows).map(|i| Rect { x: card.x + pad, y: card.y + pad + i * row, w: column - 2 * pad, h: row }).collect();
    Layout { logo: logo_r, title_y, tagline_y, hint_y, button, heading_y, card, rows: rects }
}

/// What a place of the page is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Open,
    Row(usize),
}

fn inside(r: Rect, x: i32, y: i32) -> bool {
    x >= r.x && y >= r.y && x < r.x.saturating_add(r.w) && y < r.y.saturating_add(r.h)
}

/// What is at (`x`, `y`) of the page; a row counts only when it holds a file (`entries` of them).
pub fn hit(l: &Layout, entries: usize, x: i32, y: i32) -> Option<Hit> {
    if inside(l.button, x, y) {
        return Some(Hit::Open);
    }
    l.rows.iter().take(entries).position(|r| inside(*r, x, y)).map(Hit::Row)
}

/// A key that moves the row that has the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Home,
    End,
}

/// The row that has the keyboard after `key`, of `shown` rows (`current` is none when none has it: Down goes to the first row, Up to the last).
pub fn step_focus(current: Option<usize>, shown: usize, key: Key) -> Option<usize> {
    if shown == 0 {
        return None;
    }
    let last = shown - 1;
    Some(match (current, key) {
        (_, Key::Home) => 0,
        (_, Key::End) => last,
        (None, Key::Down) => 0,
        (None, Key::Up) => last,
        (Some(i), Key::Down) => (i + 1).min(last),
        (Some(i), Key::Up) => i.saturating_sub(1),
    })
}

/// The row that has the keyboard after the entry at `removed` was taken out of a list that now has `left` entries (the one that took its place, else the last).
pub fn focus_after_removal(removed: usize, left: usize) -> Option<usize> {
    (left > 0).then(|| removed.min(left - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_date_is_four_two_and_two_digits_of_a_day_that_there_is() {
        assert_eq!(parse_date("2026-10-21"), Some((2026, 10, 21)));
        assert_eq!(parse_date(" 2024-02-29 "), Some((2024, 2, 29)));
        for bad in ["", "2026-10", "2026-13-01", "2026-00-10", "2026-10-32", "2026-10-00", "2025-02-29", "2100-02-29", "26-10-21", "2026/10/21", "2026-1-21", "abcd-ef-gh", "2026-10-21-1"] {
            assert_eq!(parse_date(bad), None, "{bad:?}");
        }
        assert_eq!(parse_date("2000-02-29"), Some((2000, 2, 29)));
    }

    #[test]
    fn a_day_is_today_yesterday_or_its_date() {
        let today = "2026-10-21";
        assert_eq!(relative(today, "2026-10-21"), Rel::Today);
        assert_eq!(relative(today, "2026-10-20"), Rel::Yesterday);
        assert_eq!(relative(today, "2026-10-19"), Rel::Date { year: 2026, month: 10, day: 19, year_shown: false });
        assert_eq!(relative(today, "2025-12-31"), Rel::Date { year: 2025, month: 12, day: 31, year_shown: true });
        // Across a month, a year and a leap day.
        assert_eq!(relative("2026-11-01", "2026-10-31"), Rel::Yesterday);
        assert_eq!(relative("2026-01-01", "2025-12-31"), Rel::Yesterday);
        assert_eq!(relative("2024-03-01", "2024-02-29"), Rel::Yesterday);
        assert_eq!(relative("2025-03-01", "2025-02-28"), Rel::Yesterday);
        // A day to come (the clock was changed) is told by its date; what cannot be read is unknown.
        assert!(matches!(relative(today, "2026-10-22"), Rel::Date { day: 22, .. }));
        assert_eq!(relative(today, ""), Rel::Unknown);
        assert_eq!(relative("", "2026-10-21"), Rel::Unknown);
        assert_eq!(relative(today, "yesterday"), Rel::Unknown);
    }

    #[test]
    fn the_days_are_told_in_both_languages() {
        let today = "2026-10-21";
        let say = |lang, then| day_text(lang, relative(today, then));
        assert_eq!((say(Lang::Zh, "2026-10-21"), say(Lang::Zh, "2026-10-20")), ("今天".to_string(), "昨天".to_string()));
        assert_eq!((say(Lang::En, "2026-10-21"), say(Lang::En, "2026-10-20")), ("Today".to_string(), "Yesterday".to_string()));
        assert_eq!(say(Lang::Zh, "2026-10-05"), "10月5日");
        assert_eq!(say(Lang::En, "2026-10-05"), "Oct 5");
        assert_eq!(say(Lang::Zh, "2025-03-09"), "2025年3月9日");
        assert_eq!(say(Lang::En, "2025-03-09"), "Mar 9, 2025");
        assert_eq!(say(Lang::En, "2025-12-31"), "Dec 31, 2025");
        assert_eq!((say(Lang::Zh, ""), say(Lang::En, "x")), (String::new(), String::new()));
    }

    #[test]
    fn a_path_is_a_name_and_a_folder() {
        assert_eq!(split_path("C:\\docs\\a b.pdf"), ("a b.pdf", "C:\\docs"));
        assert_eq!(split_path("C:\\a.pdf"), ("a.pdf", "C:"));
        assert_eq!(split_path("\\\\server\\share\\a.pdf"), ("a.pdf", "\\\\server\\share"));
        assert_eq!(split_path("/home/me/a.pdf"), ("a.pdf", "/home/me"));
        assert_eq!(split_path("a.pdf"), ("a.pdf", ""));
        assert_eq!(split_path("C:\\docs\\"), ("", "C:\\docs"));
        assert_eq!(split_path(""), ("", ""));
    }

    const AREA: Rect = Rect { x: 0, y: 84, w: 1200, h: 700 };

    #[test]
    fn the_page_is_in_the_middle_and_the_list_has_as_many_rows_as_fit() {
        let l = layout(AREA, 96, 10, 36, 20);
        // The logo and the button are centred across the page; everything is in order from the top.
        assert_eq!(l.logo.x + l.logo.w / 2, 600);
        assert_eq!(l.button.x + l.button.w / 2, 600);
        assert!(l.logo.y >= AREA.y && l.title_y > l.logo.y + 72 && l.tagline_y > l.title_y && l.button.y > l.tagline_y && l.hint_y > l.button.y && l.heading_y > l.hint_y && l.card.y > l.heading_y);
        assert_eq!((l.logo.w, l.button.h, l.card.w), (72, 40, 480));
        // 700 high: not all ten rows fit; those that do are in the card, one under the other, and the card ends before the page does.
        assert!(!l.rows.is_empty() && l.rows.len() < 10, "{}", l.rows.len());
        for (i, r) in l.rows.iter().enumerate() {
            assert_eq!((r.h, r.y), (52, l.card.y + 6 + 52 * i as i32));
            assert!(r.x >= l.card.x && r.x + r.w <= l.card.x + l.card.w);
        }
        assert!(l.card.y + l.card.h <= AREA.y + AREA.h);
        // A tall window shows them all; a list of three shows three.
        let tall = layout(Rect { h: 1400, ..AREA }, 96, 10, 36, 20);
        assert_eq!(tall.rows.len(), 10);
        assert_eq!(layout(AREA, 96, 3, 36, 20).rows.len(), 3);
    }

    #[test]
    fn an_empty_list_has_a_row_to_say_so_and_a_page_too_small_for_the_list_has_none() {
        let l = layout(AREA, 96, 0, 36, 20);
        assert_eq!(l.rows.len(), 1);
        assert_eq!(hit(&l, 0, l.rows[0].x + 5, l.rows[0].y + 5), None, "no file to open in it");
        let small = layout(Rect { x: 0, y: 84, w: 800, h: 330 }, 96, 5, 36, 20);
        assert!(small.rows.is_empty() && small.card.h == 0);
        // Even then the button is on the page.
        assert!(small.button.y >= 84 && small.button.y + small.button.h <= 84 + 330);
        // A page narrower than the column: the column is the page less its margins.
        assert_eq!(layout(Rect { x: 0, y: 0, w: 300, h: 900 }, 96, 3, 36, 20).card.w, 300 - 48);
        let none = layout(Rect { x: 0, y: 0, w: 0, h: 0 }, 96, 3, 36, 20);
        assert!(none.rows.is_empty());
    }

    #[test]
    fn everything_is_in_proportion_at_a_greater_number_of_dots_per_inch() {
        let a = layout(Rect { x: 0, y: 0, w: 1200, h: 1400 }, 96, 4, 36, 20);
        let b = layout(Rect { x: 0, y: 0, w: 2400, h: 2800 }, 192, 4, 72, 40);
        assert_eq!((b.logo.w, b.button.w, b.button.h, b.card.w), (a.logo.w * 2, a.button.w * 2, a.button.h * 2, a.card.w * 2));
        assert_eq!(b.rows.len(), a.rows.len());
        assert_eq!(b.rows[0].h, a.rows[0].h * 2);
    }

    #[test]
    fn a_place_is_on_the_button_on_a_row_that_holds_a_file_or_on_nothing() {
        let l = layout(AREA, 96, 10, 36, 20);
        assert_eq!(hit(&l, 10, l.button.x + 10, l.button.y + 10), Some(Hit::Open));
        for (i, r) in l.rows.iter().enumerate() {
            assert_eq!(hit(&l, 10, r.x + r.w / 2, r.y + r.h / 2), Some(Hit::Row(i)));
        }
        // With fewer files than rows the rows after them are not there.
        let two = layout(AREA, 96, 2, 36, 20);
        assert_eq!(two.rows.len(), 2);
        assert_eq!(hit(&two, 1, two.rows[1].x + 4, two.rows[1].y + 4), None);
        assert_eq!(hit(&l, 10, 5, 5), None);
        assert_eq!(hit(&l, 10, l.logo.x + 5, l.logo.y + 5), None);
        assert_eq!(hit(&l, 10, l.button.x - 1, l.button.y), None);
    }

    #[test]
    fn the_arrow_keys_move_the_focus_down_the_rows_and_stop_at_the_ends() {
        assert_eq!(step_focus(None, 4, Key::Down), Some(0));
        assert_eq!(step_focus(None, 4, Key::Up), Some(3));
        assert_eq!(step_focus(Some(0), 4, Key::Down), Some(1));
        assert_eq!(step_focus(Some(3), 4, Key::Down), Some(3));
        assert_eq!(step_focus(Some(0), 4, Key::Up), Some(0));
        assert_eq!(step_focus(Some(2), 4, Key::Up), Some(1));
        assert_eq!(step_focus(Some(2), 4, Key::Home), Some(0));
        assert_eq!(step_focus(None, 4, Key::End), Some(3));
        assert_eq!(step_focus(Some(1), 0, Key::Down), None);
        // After an entry is taken out, the one that took its place has the focus, or the last, or none when the list is empty.
        assert_eq!(focus_after_removal(1, 3), Some(1));
        assert_eq!(focus_after_removal(2, 2), Some(1));
        assert_eq!(focus_after_removal(0, 0), None);
    }
}
