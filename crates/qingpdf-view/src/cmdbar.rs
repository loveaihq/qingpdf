//! The command bar under the title strip (2026-10-21; before that, the toolbar): what its buttons are, when each can be used and pressed, where each
//! is for a window of a given width and number of dots per inch, what a place of it is on, and the words of the tooltips. Plain logic; `app/cmdbar.rs`
//! draws it and `win32.rs` supplies the letters.
//!
//! The pictures are letters of the icon font of Windows (Segoe Fluent Icons on Windows 11, Segoe MDL2 Assets on Windows 10; the code points are the
//! same in both). From the left: the sidebar, open, save, print; then the tools (select, highlight, note, draw, shapes, text box, redact); then, after
//! a gap that takes up what the window has to spare, the search box (a pill) and the button of the menus ("⋯").

use crate::app::{CMD_BOOKMARKS, CMD_OPEN, CMD_PRINT, CMD_SAVE, CMD_TOOL_ARROW, CMD_TOOL_ELLIPSE, CMD_TOOL_HIGHLIGHT, CMD_TOOL_INK, CMD_TOOL_LINE, CMD_TOOL_NOTE, CMD_TOOL_RECT, CMD_TOOL_REDACT, CMD_TOOL_SELECT, CMD_TOOL_TEXTBOX};
use crate::i18n::{Lang, Msg, t};
use crate::ui::Rect;

/// The fonts the pictures come from, by name.
pub const FLUENT: &str = "Segoe Fluent Icons";
pub const MDL2: &str = "Segoe MDL2 Assets";

/// The font to draw the pictures with, given whether this computer has the newer one: it, else the older.
pub fn pick_icon_font(has_fluent: bool) -> &'static str {
    if has_fluent { FLUENT } else { MDL2 }
}

/// Sizes at 96 dpi: the height of the bar, a button's side, the gap between buttons and between groups, the room at the ends, the search box.
pub const BAR_H: i32 = 44;
pub const BTN: i32 = 28;
const GAP: i32 = 2;
const GROUP_GAP: i32 = 14;
const PAD: i32 = 12;
pub const SEARCH_W: i32 = 240;
const SEARCH_MIN: i32 = 120;
/// The room between the tools and the search box that is kept before the search box is dropped.
const SPARE: i32 = 12;
const SEARCH_GAP: i32 = 8;
pub const ICON_PX: i32 = 16;
/// Inside the search box: the room at its ends and between the picture and the box.
pub const SEARCH_PAD: i32 = 10;
pub const SEARCH_ICON_GAP: i32 = 6;

/// When a button can be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    Always,
    /// A file is open.
    Doc,
    /// A file is open and may be annotated.
    Edit,
}

/// How a button stays pressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Plain,
    /// Pressed while its tool is in use.
    Tool,
    /// Pressed while one of the shapes is the tool; opens a list of them.
    Shapes,
    /// Pressed while the sidebar is shown.
    Sidebar,
}

pub struct Def {
    /// The menu command it does (0 for the button of the shapes, which opens a list).
    pub cmd: u32,
    pub glyph: char,
    pub tip: Msg,
    pub rule: Rule,
    pub kind: Kind,
}

const fn def(cmd: u32, glyph: char, tip: Msg, rule: Rule, kind: Kind) -> Def {
    Def { cmd, glyph, tip, rule, kind }
}

/// The buttons, in the order they stand.
pub const BUTTONS: [Def; 11] = [
    def(CMD_BOOKMARKS, '\u{E8A0}', Msg::ShowSidebar, Rule::Doc, Kind::Sidebar),
    def(CMD_OPEN, '\u{E838}', Msg::Open, Rule::Always, Kind::Plain),
    def(CMD_SAVE, '\u{E74E}', Msg::Save, Rule::Doc, Kind::Plain),
    def(CMD_PRINT, '\u{E749}', Msg::Print, Rule::Doc, Kind::Plain),
    def(CMD_TOOL_SELECT, '\u{E8B0}', Msg::ToolSelect, Rule::Doc, Kind::Tool),
    def(CMD_TOOL_HIGHLIGHT, '\u{E7E6}', Msg::ToolHighlight, Rule::Edit, Kind::Tool),
    def(CMD_TOOL_NOTE, '\u{E70B}', Msg::ToolNote, Rule::Edit, Kind::Tool),
    def(CMD_TOOL_INK, '\u{EE56}', Msg::ToolInk, Rule::Edit, Kind::Tool),
    def(0, '\u{E739}', Msg::ToolShapes, Rule::Edit, Kind::Shapes),
    def(CMD_TOOL_TEXTBOX, '\u{E8AC}', Msg::ToolTextBox, Rule::Edit, Kind::Tool),
    def(CMD_TOOL_REDACT, '\u{E8A8}', Msg::RdToolArea, Rule::Edit, Kind::Tool),
];

/// The shapes the list of the shapes button holds.
pub const SHAPES: [(u32, Msg); 4] = [(CMD_TOOL_RECT, Msg::ToolRect), (CMD_TOOL_ELLIPSE, Msg::ToolEllipse), (CMD_TOOL_LINE, Msg::ToolLine), (CMD_TOOL_ARROW, Msg::ToolArrow)];

/// Is `cmd` the tool of one of the shapes?
pub fn is_shape(cmd: u32) -> bool {
    SHAPES.iter().any(|&(c, _)| c == cmd)
}

#[derive(Clone, Copy)]
enum Slot {
    Button(usize),
    Separator,
}

/// Everything in the bar, left to right (the gap, the search box and the menu button follow).
const SLOTS: [Slot; 12] = [
    Slot::Button(0),
    Slot::Button(1),
    Slot::Button(2),
    Slot::Button(3),
    Slot::Separator,
    Slot::Button(4),
    Slot::Button(5),
    Slot::Button(6),
    Slot::Button(7),
    Slot::Button(8),
    Slot::Button(9),
    Slot::Button(10),
];

/// Where everything is, in pixels from the top left corner of the bar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Each button (its number in [`BUTTONS`]) with its place.
    pub buttons: Vec<(usize, Rect)>,
    /// The thin lines between the groups.
    pub seps: Vec<Rect>,
    /// The search box, when the window is wide enough for one.
    pub search: Option<Rect>,
    pub more: Rect,
    pub height: i32,
}

/// What a place of the bar is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Button(usize),
    Search,
    More,
}

fn inside(r: Rect, x: i32, y: i32) -> bool {
    x >= r.x && y >= r.y && x < r.x.saturating_add(r.w) && y < r.y.saturating_add(r.h)
}

/// The layout for a window `width` pixels wide at `dpi` dots per inch.
pub fn layout(width: i32, dpi: u32) -> Layout {
    let s = |v: i32| (i64::from(v) * i64::from(dpi.max(1)) / 96) as i32;
    let (btn, gap, group, pad) = (s(BTN), s(GAP), s(GROUP_GAP), s(PAD));
    let height = s(BAR_H);
    let y = (height - btn) / 2;
    let mut x = pad;
    let (mut buttons, mut seps) = (Vec::new(), Vec::new());
    for slot in SLOTS {
        match slot {
            Slot::Button(i) => {
                buttons.push((i, Rect { x, y, w: btn, h: btn }));
                x += btn + gap;
            }
            Slot::Separator => {
                // The group gap is between the edges of the buttons on either side: the gap that followed the last button is taken back.
                let edge = x - gap;
                seps.push(Rect { x: edge + group / 2, y: y + s(6), w: s(1).max(1), h: btn - 2 * s(6) });
                x = edge + group;
            }
        }
    }
    let left_end = x - gap;
    let more = Rect { x: (width - pad - btn).max(left_end), y, w: btn, h: btn };
    let room = more.x - s(SEARCH_GAP) - left_end - s(SPARE);
    let search = (room >= s(SEARCH_MIN)).then(|| {
        let w = room.min(s(SEARCH_W));
        Rect { x: more.x - s(SEARCH_GAP) - w, y, w, h: btn }
    });
    Layout { buttons, seps, search, more, height }
}

/// What is at (`x`, `y`) of the bar.
pub fn hit(l: &Layout, x: i32, y: i32) -> Option<Hit> {
    if let Some((i, _)) = l.buttons.iter().find(|(_, r)| inside(*r, x, y)) {
        return Some(Hit::Button(*i));
    }
    if inside(l.more, x, y) {
        return Some(Hit::More);
    }
    l.search.filter(|r| inside(*r, x, y)).map(|_| Hit::Search)
}

/// The parts of the search box: the picture of a magnifying glass, the box the words are typed in and, when there is a count to show ("3 / 12"),
/// the room for it at the right end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchParts {
    pub icon: Rect,
    pub edit: Rect,
    pub count: Option<Rect>,
}

pub fn search_parts(pill: Rect, dpi: u32, count_w: i32, edit_h: i32) -> SearchParts {
    let s = |v: i32| (i64::from(v) * i64::from(dpi.max(1)) / 96) as i32;
    let (pad, icon, gap) = (s(SEARCH_PAD), s(ICON_PX), s(SEARCH_ICON_GAP));
    let icon_r = Rect { x: pill.x + pad, y: pill.y + (pill.h - icon) / 2, w: icon, h: icon };
    let count = (count_w > 0).then(|| Rect { x: pill.x + pill.w - pad - count_w, y: pill.y, w: count_w, h: pill.h });
    let left = icon_r.x + icon + gap;
    let right = count.map_or(pill.x + pill.w - pad, |c| c.x - gap);
    let h = edit_h.min(pill.h);
    SearchParts { icon: icon_r, edit: Rect { x: left, y: pill.y + (pill.h - h) / 2, w: (right - left).max(0), h }, count }
}

/// A menu item's text as plain words: without the `&` that marks a shortcut letter (and the `(&X)` of the Chinese texts), without the
/// dots that say a box follows, and the shortcut (what follows the tab) apart. `("Open", Some("Ctrl+O"))`.
pub fn plain_label(menu_text: &str) -> (String, Option<String>) {
    let (label, shortcut) = match menu_text.split_once('\t') {
        Some((l, s)) => (l, Some(s.trim().to_string()).filter(|s| !s.is_empty())),
        None => (menu_text, None),
    };
    let mut out = String::new();
    let mut chars = label.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '(' {
            // "(&X)": a shortcut letter in brackets.
            let mut ahead = chars.clone();
            if ahead.next() == Some('&') && ahead.next().is_some() && ahead.next() == Some(')') {
                chars = ahead;
                continue;
            }
        }
        if c == '&' {
            continue;
        }
        out.push(c);
    }
    let out = out.trim().trim_end_matches(['.', '\u{2026}']).trim().to_string();
    (out, shortcut)
}

/// The tooltip of a button: its name, and its shortcut in brackets if it has one.
pub fn tooltip(lang: Lang, m: Msg) -> String {
    match plain_label(t(lang, m)) {
        (label, Some(shortcut)) => format!("{label} ({shortcut})"),
        (label, None) => label,
    }
}

/// What the buttons depend on.
#[derive(Clone, Copy, Debug)]
pub struct Inputs {
    pub has_doc: bool,
    /// Annotations may be added.
    pub can_edit: bool,
    /// The command of the tool in use (`CMD_TOOL_*`).
    pub tool: u32,
    /// The sidebar is shown.
    pub sidebar: bool,
}

/// Whether a button can be used, and whether it is pressed.
pub fn button_state(d: &Def, i: &Inputs) -> (bool, bool) {
    let enabled = match d.rule {
        Rule::Always => true,
        Rule::Doc => i.has_doc,
        Rule::Edit => i.has_doc && i.can_edit,
    };
    let active = enabled
        && match d.kind {
            Kind::Plain => false,
            Kind::Tool => i.tool == d.cmd,
            Kind::Shapes => is_shape(i.tool),
            Kind::Sidebar => i.sidebar,
        };
    (enabled, active)
}

/// "3 / 120" (nothing when no file is open).
pub fn page_text(page: usize, pages: usize) -> String {
    if pages == 0 { String::new() } else { format!("{} / {pages}", page + 1) }
}

/// The page asked for by what was typed in the page box, from 0 (held to the pages there are): the first number in it, so that
/// `5`, `5 / 120` and `page 5` all mean the fifth. `None` when there is no number, it is 0, or there are no pages.
pub fn parse_page_box(text: &str, pages: usize) -> Option<usize> {
    let digits: String = text.chars().skip_while(|c| !c.is_ascii_digit()).take_while(char::is_ascii_digit).take(9).collect();
    let n = digits.parse::<usize>().ok().filter(|&n| n >= 1)?;
    (pages > 0).then(|| n.min(pages) - 1)
}

/// The commands the bar's buttons send (for the tests).
pub fn commands() -> Vec<u32> {
    BUTTONS.iter().map(|d| d.cmd).filter(|&c| c != 0).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_icon_font_is_the_newest_there_is() {
        assert_eq!(pick_icon_font(true), "Segoe Fluent Icons");
        assert_eq!(pick_icon_font(false), "Segoe MDL2 Assets");
    }

    #[test]
    fn a_tooltip_is_the_name_and_the_shortcut_without_the_marks_of_the_menu() {
        assert_eq!(plain_label("&Open...\tCtrl+O"), ("Open".to_string(), Some("Ctrl+O".to_string())));
        assert_eq!(plain_label("打开(&O)...\tCtrl+O"), ("打开".to_string(), Some("Ctrl+O".to_string())));
        assert_eq!(plain_label("&Highlight"), ("Highlight".to_string(), None));
        assert_eq!(plain_label("高亮(&H)"), ("高亮".to_string(), None));
        assert_eq!(plain_label("Zoom &in\tCtrl++"), ("Zoom in".to_string(), Some("Ctrl++".to_string())));
        assert_eq!(tooltip(Lang::En, Msg::Print), "Print (Ctrl+P)");
        assert_eq!(tooltip(Lang::Zh, Msg::FitWidth), "适合宽度 (Ctrl+1)");
        assert_eq!(tooltip(Lang::En, Msg::ToolNote), "Note");
        assert_eq!(tooltip(Lang::En, Msg::Find), "Find (Ctrl+F)");
        assert_eq!(tooltip(Lang::En, Msg::ShowSidebar), "Show sidebar (F4)");
    }

    #[test]
    fn every_button_has_a_tooltip_in_both_languages_a_glyph_of_the_icon_font_and_a_command_that_is_in_no_other_button() {
        let mut cmds = Vec::new();
        for lang in [Lang::En, Lang::Zh] {
            for d in &BUTTONS {
                let tip = tooltip(lang, d.tip);
                assert!(!tip.is_empty() && !tip.contains('&') && !tip.contains('\t'), "{tip}");
                assert!(('\u{E000}'..='\u{F8FF}').contains(&d.glyph), "{:?} is not in the icon font's private area", d.glyph);
            }
            for (cmd, m) in SHAPES {
                assert!(!tooltip(lang, m).is_empty() && cmd != 0);
            }
        }
        for d in &BUTTONS {
            cmds.push(d.cmd);
        }
        cmds.retain(|&c| c != 0);
        let n = cmds.len();
        cmds.sort_unstable();
        cmds.dedup();
        assert_eq!(cmds.len(), n);
        assert_eq!(commands().len(), n);
        // The tools: a tool the button of the shapes covers is not a button of its own.
        assert!(BUTTONS.iter().filter(|d| d.kind == Kind::Shapes).count() == 1 && !commands().iter().any(|&c| is_shape(c)));
    }

    fn inputs() -> Inputs {
        Inputs { has_doc: true, can_edit: true, tool: CMD_TOOL_SELECT, sidebar: false }
    }

    fn of(i: &Inputs, cmd: u32) -> (bool, bool) {
        BUTTONS.iter().find(|d| d.cmd == cmd).map(|d| button_state(d, i)).unwrap_or((false, false))
    }

    #[test]
    fn without_a_file_only_open_works() {
        let none = Inputs { has_doc: false, can_edit: false, tool: CMD_TOOL_SELECT, sidebar: false };
        assert_eq!(of(&none, CMD_OPEN), (true, false));
        for d in BUTTONS.iter().filter(|d| d.cmd != CMD_OPEN) {
            assert_eq!(button_state(d, &none), (false, false), "{}", d.cmd);
        }
    }

    #[test]
    fn the_tool_in_use_the_sidebar_and_the_shapes_are_pressed_while_they_are_on() {
        assert_eq!(of(&inputs(), CMD_TOOL_SELECT), (true, true));
        let ink = Inputs { tool: CMD_TOOL_INK, ..inputs() };
        assert_eq!(of(&ink, CMD_TOOL_INK), (true, true));
        assert_eq!(of(&ink, CMD_TOOL_NOTE), (true, false));
        assert_eq!(of(&ink, CMD_TOOL_SELECT), (true, false));
        let shapes = BUTTONS.iter().find(|d| d.kind == Kind::Shapes).unwrap();
        assert_eq!(button_state(shapes, &ink), (true, false));
        assert_eq!(button_state(shapes, &Inputs { tool: CMD_TOOL_ARROW, ..inputs() }), (true, true));
        assert_eq!(of(&Inputs { sidebar: true, ..inputs() }, CMD_BOOKMARKS), (true, true));
        assert_eq!(of(&inputs(), CMD_BOOKMARKS), (true, false));
    }

    #[test]
    fn the_tools_need_a_file_that_may_be_annotated_and_the_rest_do_not() {
        let locked = Inputs { can_edit: false, ..inputs() };
        for cmd in [CMD_TOOL_INK, CMD_TOOL_NOTE, CMD_TOOL_HIGHLIGHT, CMD_TOOL_TEXTBOX, CMD_TOOL_REDACT] {
            assert_eq!(of(&locked, cmd), (false, false), "{cmd}");
        }
        let shapes = BUTTONS.iter().find(|d| d.kind == Kind::Shapes).unwrap();
        assert_eq!(button_state(shapes, &locked), (false, false));
        // Select, save, print and the sidebar work on a file that may not be annotated.
        for cmd in [CMD_TOOL_SELECT, CMD_SAVE, CMD_PRINT, CMD_BOOKMARKS, CMD_OPEN] {
            assert!(of(&locked, cmd).0, "{cmd}");
        }
    }

    #[test]
    fn the_buttons_stand_in_a_row_with_the_search_box_and_the_menu_button_at_the_right_end() {
        let l = layout(1200, 96);
        assert_eq!(l.height, 44);
        assert_eq!(l.buttons.len(), BUTTONS.len());
        // The buttons are 28 square, 8 from the top, in the order of the list, left to right with no overlap.
        let mut right = 0;
        for (n, (i, r)) in l.buttons.iter().enumerate() {
            assert_eq!(*i, n);
            assert_eq!((r.w, r.h, r.y), (28, 28, 8));
            assert!(r.x >= right, "{n} overlaps");
            right = r.x + r.w;
        }
        assert_eq!(l.buttons[0].1.x, 12);
        // One line between the groups, after print.
        assert_eq!(l.seps.len(), 1);
        assert!(l.seps[0].x > l.buttons[3].1.x + 28 && l.seps[0].x < l.buttons[4].1.x);
        // The menu button is at the right end, 12 from the edge; the search box is next to it, 8 away, as wide as it may be.
        assert_eq!((l.more.x + l.more.w, l.more.w), (1200 - 12, 28));
        let s = l.search.unwrap();
        assert_eq!((s.w, s.h, s.x + s.w + 8), (SEARCH_W, 28, l.more.x));
        assert!(s.x > right + 12);
    }

    #[test]
    fn a_narrow_window_makes_the_search_box_smaller_and_then_leaves_it_out() {
        let wide = layout(1200, 96);
        let tools_end = wide.buttons.last().unwrap().1.x + 28;
        let medium = layout(tools_end + 12 + 8 + 28 + 12 + 150, 96);
        let m = medium.search.unwrap();
        assert!(m.w >= 120 && m.w < SEARCH_W, "{}", m.w);
        let narrow = layout(tools_end + 100, 96);
        assert!(narrow.search.is_none());
        // Nothing is drawn beyond the window's right edge for any width that has room for the tools; the menu button never moves left of the tools.
        for w in [400, 600, 640, 800, 1000, 1920] {
            let l = layout(w, 96);
            assert!(l.more.x >= tools_end, "{w}");
            if let Some(s) = l.search {
                assert!(s.x >= tools_end && s.x + s.w <= l.more.x, "{w}");
            }
        }
        let tiny = layout(0, 96);
        assert!(tiny.search.is_none() && tiny.buttons.len() == BUTTONS.len());
    }

    #[test]
    fn the_layout_follows_the_dots_per_inch() {
        let a = layout(1500, 144);
        assert_eq!((a.height, a.buttons[0].1.w, a.buttons[0].1.y, a.buttons[0].1.x), (66, 42, 12, 18));
        assert_eq!(a.more.x + a.more.w, 1500 - 18);
        // 100 and 200 percent are exactly double.
        let (one, two) = (layout(1000, 96), layout(2000, 192));
        assert_eq!(two.buttons[3].1.x, one.buttons[3].1.x * 2);
        assert_eq!(two.search.unwrap().w, one.search.unwrap().w * 2);
    }

    #[test]
    fn a_place_of_the_bar_is_on_a_button_the_search_box_the_menu_button_or_nothing() {
        let l = layout(1200, 96);
        for (i, r) in &l.buttons {
            assert_eq!(hit(&l, r.x + r.w / 2, r.y + r.h / 2), Some(Hit::Button(*i)));
            assert_eq!(hit(&l, r.x, r.y), Some(Hit::Button(*i)));
        }
        let s = l.search.unwrap();
        assert_eq!(hit(&l, s.x + 40, s.y + 10), Some(Hit::Search));
        assert_eq!(hit(&l, l.more.x + 5, l.more.y + 5), Some(Hit::More));
        // The gap between the tools and the search box, the bar above and below the buttons, and left of the first button.
        assert_eq!(hit(&l, s.x - 20, 22), None);
        assert_eq!(hit(&l, 100, 2), None);
        assert_eq!(hit(&l, 100, 43), None);
        assert_eq!(hit(&l, 5, 22), None);
        assert_eq!(hit(&l, -50, 22), None);
        assert_eq!(hit(&layout(0, 96), 5, 5), None);
    }

    #[test]
    fn the_search_box_is_a_glass_the_box_to_type_in_and_room_for_the_count() {
        let pill = Rect { x: 800, y: 8, w: 240, h: 28 };
        let plain = search_parts(pill, 96, 0, 18);
        assert_eq!(plain.icon, Rect { x: 810, y: 14, w: 16, h: 16 });
        assert_eq!(plain.edit, Rect { x: 832, y: 13, w: 1030 - 832, h: 18 });
        assert!(plain.count.is_none());
        let counted = search_parts(pill, 96, 50, 18);
        let c = counted.count.unwrap();
        assert_eq!((c.x + c.w, c.w), (1030, 50));
        assert_eq!(counted.edit.x + counted.edit.w + 6, c.x);
        // A box too small for any of it has an edit of no width, not a negative one.
        assert_eq!(search_parts(Rect { x: 0, y: 0, w: 20, h: 28 }, 96, 80, 18).edit.w, 0);
    }

    #[test]
    fn what_is_typed_in_the_page_box_is_read_as_a_page() {
        assert_eq!(parse_page_box("5", 120), Some(4));
        assert_eq!(parse_page_box(" 5 / 120 ", 120), Some(4));
        assert_eq!(parse_page_box("第 7 页", 120), Some(6));
        assert_eq!(parse_page_box("999", 120), Some(119));
        assert_eq!(parse_page_box("1", 120), Some(0));
        assert_eq!(parse_page_box("0", 120), None);
        assert_eq!(parse_page_box("", 120), None);
        assert_eq!(parse_page_box("abc", 120), None);
        assert_eq!(parse_page_box("5", 0), None);
        assert_eq!(parse_page_box("99999999999999999999", 120), Some(119));
        assert_eq!(page_text(0, 1), "1 / 1");
        assert_eq!(page_text(2, 120), "3 / 120");
        assert_eq!(page_text(0, 0), "");
    }
}
