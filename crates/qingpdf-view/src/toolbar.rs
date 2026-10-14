//! The toolbar under the menu: what its buttons are, when each of them can be used, what the page box says and how what is typed in it
//! is read, the words of the tooltips. Plain logic; `win32.rs` makes the controls and draws the pictures.
//!
//! The pictures are not bitmaps in the program: they are letters of the icon font of Windows (Segoe Fluent Icons on Windows 11, Segoe
//! MDL2 Assets on Windows 10; the code points are the same in both), drawn at run time into an image list in the size the screen
//! asks for. Without either font the buttons say what they are in words.

use crate::app::{CMD_FIT_PAGE, CMD_FIT_WIDTH, CMD_NEXT_PAGE, CMD_OPEN, CMD_PREV_PAGE, CMD_PRINT, CMD_TOOL_HIGHLIGHT, CMD_TOOL_INK, CMD_TOOL_NOTE, CMD_ZOOM_IN, CMD_ZOOM_OUT};
use crate::i18n::{Lang, Msg, t};
use crate::ui::{ToolItem, ToolbarSpec, ToolbarState};

/// The font the pictures come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconFont {
    Fluent,
    Mdl2,
    /// Neither is on this computer: the buttons have words.
    Words,
}

impl IconFont {
    /// The name of the font (none for [`IconFont::Words`]).
    pub fn face(self) -> Option<&'static str> {
        match self {
            IconFont::Fluent => Some(FLUENT),
            IconFont::Mdl2 => Some(MDL2),
            IconFont::Words => None,
        }
    }
}

pub const FLUENT: &str = "Segoe Fluent Icons";
pub const MDL2: &str = "Segoe MDL2 Assets";

/// The font to draw the pictures with, given which of the two this computer has: the newer if it can, else the older, else words.
pub fn pick_icon_font(has_fluent: bool, has_mdl2: bool) -> IconFont {
    if has_fluent {
        IconFont::Fluent
    } else if has_mdl2 {
        IconFont::Mdl2
    } else {
        IconFont::Words
    }
}

/// When a button can be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rule {
    Always,
    /// A file is open.
    Doc,
    /// A file is open and it is not at its first page, or not at its last.
    Prev,
    Next,
    /// A file is open and may be annotated.
    Edit,
}

struct Def {
    cmd: u32,
    glyph: char,
    tip: Msg,
    rule: Rule,
    toggle: bool,
}

enum Slot {
    Button(Def),
    Separator,
    Page,
    Search,
}

const fn button(cmd: u32, glyph: char, tip: Msg, rule: Rule) -> Slot {
    Slot::Button(Def { cmd, glyph, tip, rule, toggle: false })
}

const fn tool(cmd: u32, glyph: char, tip: Msg) -> Slot {
    Slot::Button(Def { cmd, glyph, tip, rule: Rule::Edit, toggle: true })
}

/// Everything in the toolbar, left to right.
const SLOTS: [Slot; 17] = [
    button(CMD_OPEN, '\u{ED25}', Msg::Open, Rule::Always),
    button(CMD_PRINT, '\u{E749}', Msg::Print, Rule::Doc),
    Slot::Separator,
    button(CMD_PREV_PAGE, '\u{E70E}', Msg::PrevPage, Rule::Prev),
    button(CMD_NEXT_PAGE, '\u{E70D}', Msg::NextPage, Rule::Next),
    Slot::Page,
    Slot::Separator,
    button(CMD_ZOOM_OUT, '\u{E71F}', Msg::ZoomOut, Rule::Doc),
    button(CMD_ZOOM_IN, '\u{E8A3}', Msg::ZoomIn, Rule::Doc),
    button(CMD_FIT_WIDTH, '\u{E8AB}', Msg::FitWidth, Rule::Doc),
    button(CMD_FIT_PAGE, '\u{E9A6}', Msg::FitPage, Rule::Doc),
    Slot::Separator,
    tool(CMD_TOOL_HIGHLIGHT, '\u{E7E6}', Msg::ToolHighlight),
    tool(CMD_TOOL_NOTE, '\u{E70B}', Msg::ToolNote),
    tool(CMD_TOOL_INK, '\u{EE56}', Msg::ToolInk),
    Slot::Separator,
    Slot::Search,
];

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

/// The toolbar's items, with the words in `lang`.
pub fn spec(lang: Lang, visible: bool) -> ToolbarSpec {
    let items = SLOTS
        .iter()
        .map(|slot| match slot {
            Slot::Button(d) => ToolItem::Button { cmd: d.cmd, glyph: d.glyph, label: plain_label(t(lang, d.tip)).0, tip: tooltip(lang, d.tip), toggle: d.toggle },
            Slot::Separator => ToolItem::Separator,
            Slot::Page => ToolItem::PageBox,
            Slot::Search => ToolItem::SearchBox { hint: tooltip(lang, Msg::Find) },
        })
        .collect();
    ToolbarSpec { visible, items }
}

/// What the buttons depend on.
#[derive(Clone, Copy, Debug)]
pub struct Inputs {
    pub has_doc: bool,
    /// The page in view (from 0) and the number of pages.
    pub page: usize,
    pub pages: usize,
    /// Annotations may be added.
    pub can_edit: bool,
    /// The command of the tool in use (`CMD_TOOL_*`).
    pub tool: u32,
}

/// "3 / 120" (nothing when no file is open).
pub fn page_text(page: usize, pages: usize) -> String {
    if pages == 0 { String::new() } else { format!("{} / {pages}", page + 1) }
}

/// The state of every button, and what the page box says.
pub fn state(i: &Inputs) -> ToolbarState {
    let buttons = SLOTS
        .iter()
        .filter_map(|slot| match slot {
            Slot::Button(d) => {
                let enabled = match d.rule {
                    Rule::Always => true,
                    Rule::Doc => i.has_doc,
                    Rule::Prev => i.has_doc && i.page > 0,
                    Rule::Next => i.has_doc && i.page + 1 < i.pages,
                    Rule::Edit => i.has_doc && i.can_edit,
                };
                Some((d.cmd, enabled, d.toggle && i.tool == d.cmd))
            }
            _ => None,
        })
        .collect();
    ToolbarState { buttons, boxes_enabled: i.has_doc, page_text: if i.has_doc { page_text(i.page, i.pages) } else { String::new() } }
}

/// The page asked for by what was typed in the page box, from 0 (held to the pages there are): the first number in it, so that
/// `5`, `5 / 120` and `page 5` all mean the fifth. `None` when there is no number, it is 0, or there are no pages.
pub fn parse_page_box(text: &str, pages: usize) -> Option<usize> {
    let digits: String = text.chars().skip_while(|c| !c.is_ascii_digit()).take_while(char::is_ascii_digit).take(9).collect();
    let n = digits.parse::<usize>().ok().filter(|&n| n >= 1)?;
    (pages > 0).then(|| n.min(pages) - 1)
}

/// The commands the toolbar can send (for the tests, and for the window to know what a click on a toolbar button is).
pub fn commands() -> Vec<u32> {
    SLOTS.iter().filter_map(|s| if let Slot::Button(d) = s { Some(d.cmd) } else { None }).collect()
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::CMD_TOOL_SELECT;

    #[test]
    fn the_icon_font_is_the_newest_there_is_else_words() {
        assert_eq!(pick_icon_font(true, true), IconFont::Fluent);
        assert_eq!(pick_icon_font(true, false), IconFont::Fluent);
        assert_eq!(pick_icon_font(false, true), IconFont::Mdl2);
        assert_eq!(pick_icon_font(false, false), IconFont::Words);
        assert_eq!(IconFont::Fluent.face(), Some("Segoe Fluent Icons"));
        assert_eq!(IconFont::Mdl2.face(), Some("Segoe MDL2 Assets"));
        assert_eq!(IconFont::Words.face(), None);
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
    }

    #[test]
    fn every_button_has_a_tooltip_in_both_languages_and_a_command_that_is_in_no_other_button() {
        for lang in [Lang::En, Lang::Zh] {
            let s = spec(lang, true);
            let mut cmds = Vec::new();
            for item in &s.items {
                if let ToolItem::Button { cmd, tip, label, glyph, .. } = item {
                    assert!(!tip.is_empty() && !label.is_empty() && !tip.contains('&') && !tip.contains('\t'), "{tip}");
                    assert!(('\u{E000}'..='\u{F8FF}').contains(glyph), "{glyph:?} is not in the icon font's private area");
                    cmds.push(*cmd);
                }
            }
            let n = cmds.len();
            cmds.sort_unstable();
            cmds.dedup();
            assert_eq!(cmds.len(), n);
            assert_eq!(n, 11);
            assert_eq!(s.items.iter().filter(|i| matches!(i, ToolItem::PageBox)).count(), 1);
            assert_eq!(s.items.iter().filter(|i| matches!(i, ToolItem::SearchBox { .. })).count(), 1);
            // The tooltips are in the language asked for.
            if lang == Lang::Zh {
                assert!(s.items.iter().any(|i| matches!(i, ToolItem::Button { tip, .. } if !tip.is_ascii())));
            }
        }
        assert_eq!(commands().len(), 11);
    }

    fn inputs() -> Inputs {
        Inputs { has_doc: true, page: 2, pages: 120, can_edit: true, tool: CMD_TOOL_SELECT }
    }

    fn of(st: &ToolbarState, cmd: u32) -> (bool, bool) {
        st.buttons.iter().find(|b| b.0 == cmd).map(|b| (b.1, b.2)).unwrap_or((false, false))
    }

    #[test]
    fn without_a_file_only_open_works() {
        let st = state(&Inputs { has_doc: false, page: 0, pages: 0, can_edit: false, tool: CMD_TOOL_SELECT });
        assert_eq!(of(&st, CMD_OPEN), (true, false));
        for cmd in commands().into_iter().filter(|&c| c != CMD_OPEN) {
            assert_eq!(of(&st, cmd), (false, false), "{cmd}");
        }
        assert!(!st.boxes_enabled);
        assert_eq!(st.page_text, "");
    }

    #[test]
    fn the_page_buttons_stop_at_the_ends_and_the_page_box_says_where_it_is() {
        let st = state(&inputs());
        assert_eq!(st.page_text, "3 / 120");
        assert!(st.boxes_enabled);
        assert!(of(&st, CMD_PREV_PAGE).0 && of(&st, CMD_NEXT_PAGE).0);
        let first = state(&Inputs { page: 0, ..inputs() });
        assert!(!of(&first, CMD_PREV_PAGE).0 && of(&first, CMD_NEXT_PAGE).0);
        let last = state(&Inputs { page: 119, ..inputs() });
        assert!(of(&last, CMD_PREV_PAGE).0 && !of(&last, CMD_NEXT_PAGE).0);
        let one = state(&Inputs { page: 0, pages: 1, ..inputs() });
        assert!(!of(&one, CMD_PREV_PAGE).0 && !of(&one, CMD_NEXT_PAGE).0);
        assert_eq!(page_text(0, 1), "1 / 1");
    }

    #[test]
    fn the_tools_are_pressed_while_in_use_and_need_a_file_that_may_be_annotated() {
        let st = state(&Inputs { tool: CMD_TOOL_INK, ..inputs() });
        assert_eq!(of(&st, CMD_TOOL_INK), (true, true));
        assert_eq!(of(&st, CMD_TOOL_NOTE), (true, false));
        assert_eq!(of(&st, CMD_TOOL_HIGHLIGHT), (true, false));
        let locked = state(&Inputs { can_edit: false, ..inputs() });
        for cmd in [CMD_TOOL_INK, CMD_TOOL_NOTE, CMD_TOOL_HIGHLIGHT] {
            assert_eq!(of(&locked, cmd), (false, false));
        }
        // Zoom still works on a file that may not be annotated.
        assert!(of(&locked, CMD_ZOOM_IN).0 && of(&locked, CMD_FIT_PAGE).0 && of(&locked, CMD_PRINT).0);
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
    }
}
