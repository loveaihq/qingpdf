//! The menus (3d-2): File (open, recent files, print, exit), Edit (copy, find), View (zoom, turn, the bookmarks), Go (page) and Help
//! (about).

use super::editing::{COLORS, Tool, WIDTHS};
use super::*;
use crate::ui::MenuEntry;

/// What decides how the menus look: whether a file is open (the items that need one are greyed otherwise), whether it has
/// bookmarks and whether they are shown, and the files in the list of recent ones.
pub(super) struct MenuState {
    pub has_doc: bool,
    pub has_outline: bool,
    pub show_outline: bool,
    pub recent: Vec<String>,
    /// Annotations may be added (the file allows it) and what can be undone, redone and taken out.
    pub can_edit: bool,
    pub can_undo: bool,
    pub can_redo: bool,
    pub has_picked: bool,
    /// The command of the tool in use (`CMD_TOOL_*`), the colour chosen and the line width.
    pub tool: u32,
    pub color: usize,
    pub width: usize,
}

impl MenuState {
    /// Nothing open, nothing changed.
    pub(super) fn idle() -> MenuState {
        MenuState {
            has_doc: false,
            has_outline: false,
            show_outline: true,
            recent: Vec::new(),
            can_edit: false,
            can_undo: false,
            can_redo: false,
            has_picked: false,
            tool: CMD_TOOL_SELECT,
            color: 0,
            width: 1,
        }
    }
}

/// A path as a menu item's text: cut in the middle if it is long, and with `&` doubled (a single one marks a shortcut letter).
fn shorten(path: &str, most: usize) -> String {
    let chars: Vec<char> = path.chars().collect();
    let cut: String = if chars.len() <= most {
        path.to_string()
    } else {
        let keep = most.saturating_sub(1);
        let (head, tail) = (keep / 3, keep - keep / 3);
        chars.iter().take(head).chain(std::iter::once(&'\u{2026}')).chain(chars.iter().skip(chars.len() - tail)).collect()
    };
    cut.replace('&', "&&")
}

pub(super) fn build_menus(lang: Lang, st: &MenuState) -> Vec<Menu> {
    let item = |id: u32, m: Msg, enabled: bool| MenuEntry::Item { id, label: t(lang, m).to_string(), checked: false, enabled };
    let recent: Vec<MenuEntry> = if st.recent.is_empty() {
        vec![MenuEntry::Item { id: 0, label: t(lang, Msg::NoRecent).to_string(), checked: false, enabled: false }]
    } else {
        let mut entries: Vec<MenuEntry> = st
            .recent
            .iter()
            .enumerate()
            .map(|(i, path)| {
                // The first nine have a number to press.
                let label = if i < 9 { format!("&{} {}", i + 1, shorten(path, 60)) } else { shorten(path, 60) };
                MenuEntry::Item { id: CMD_RECENT_BASE + i as u32, label, checked: false, enabled: true }
            })
            .collect();
        entries.push(MenuEntry::Separator);
        entries.push(item(CMD_CLEAR_RECENT, Msg::ClearRecent, true));
        entries
    };
    vec![
        Menu {
            title: t(lang, Msg::File).to_string(),
            entries: vec![
                item(CMD_OPEN, Msg::Open, true),
                MenuEntry::Submenu { title: t(lang, Msg::Recent).to_string(), entries: recent },
                MenuEntry::Separator,
                item(CMD_SAVE, Msg::Save, st.has_doc),
                item(CMD_SAVE_AS, Msg::SaveAs, st.has_doc),
                MenuEntry::Separator,
                item(CMD_PRINT, Msg::Print, st.has_doc),
                MenuEntry::Separator,
                item(CMD_EXIT, Msg::Exit, true),
            ],
        },
        Menu {
            title: t(lang, Msg::Edit).to_string(),
            entries: vec![
                item(CMD_UNDO, Msg::Undo, st.can_undo),
                item(CMD_REDO, Msg::Redo, st.can_redo),
                MenuEntry::Separator,
                item(CMD_COPY, Msg::Copy, st.has_doc),
                MenuEntry::Separator,
                item(CMD_FIND, Msg::Find, st.has_doc),
                item(CMD_FIND_NEXT, Msg::FindNext, st.has_doc),
                item(CMD_FIND_PREV, Msg::FindPrev, st.has_doc),
            ],
        },
        Menu {
            title: t(lang, Msg::View).to_string(),
            entries: vec![
                item(CMD_ZOOM_IN, Msg::ZoomIn, st.has_doc),
                item(CMD_ZOOM_OUT, Msg::ZoomOut, st.has_doc),
                MenuEntry::Separator,
                item(CMD_FIT_WIDTH, Msg::FitWidth, st.has_doc),
                item(CMD_FIT_PAGE, Msg::FitPage, st.has_doc),
                item(CMD_ACTUAL, Msg::ActualSize, st.has_doc),
                MenuEntry::Separator,
                item(CMD_ROTATE, Msg::Rotate, st.has_doc),
                MenuEntry::Separator,
                MenuEntry::Item { id: CMD_BOOKMARKS, label: t(lang, Msg::ShowBookmarks).to_string(), checked: st.show_outline, enabled: st.has_outline },
            ],
        },
        Menu {
            title: t(lang, Msg::CommentMenu).to_string(),
            entries: {
                let tool = |id: u32, m: Msg| MenuEntry::Item { id, label: t(lang, m).to_string(), checked: st.tool == id, enabled: id == CMD_TOOL_SELECT && st.has_doc || st.can_edit };
                let colors: Vec<MenuEntry> = COLORS.iter().enumerate().map(|(i, (m, _))| MenuEntry::Item { id: CMD_COLOR_BASE + i as u32, label: t(lang, *m).to_string(), checked: st.color == i, enabled: st.can_edit }).collect();
                let widths: Vec<MenuEntry> = WIDTHS.iter().enumerate().map(|(i, (m, _))| MenuEntry::Item { id: CMD_WIDTH_BASE + i as u32, label: t(lang, *m).to_string(), checked: st.width == i, enabled: st.can_edit }).collect();
                vec![
                    tool(CMD_TOOL_SELECT, Msg::ToolSelect),
                    MenuEntry::Separator,
                    tool(CMD_TOOL_HIGHLIGHT, Msg::ToolHighlight),
                    tool(CMD_TOOL_UNDERLINE, Msg::ToolUnderline),
                    tool(CMD_TOOL_STRIKE, Msg::ToolStrike),
                    tool(CMD_TOOL_SQUIGGLY, Msg::ToolSquiggly),
                    tool(CMD_TOOL_INK, Msg::ToolInk),
                    tool(CMD_TOOL_NOTE, Msg::ToolNote),
                    MenuEntry::Separator,
                    MenuEntry::Submenu { title: t(lang, Msg::ColorMenu).to_string(), entries: colors },
                    MenuEntry::Submenu { title: t(lang, Msg::WidthMenu).to_string(), entries: widths },
                    item(CMD_AUTHOR, Msg::AuthorItem, st.has_doc),
                    MenuEntry::Separator,
                    item(CMD_REMOVE, Msg::RemoveAnnot, st.has_picked && st.can_edit),
                ]
            },
        },
        Menu {
            title: t(lang, Msg::GoMenu).to_string(),
            entries: vec![item(CMD_GOTO, Msg::PageNumber, st.has_doc)],
        },
        Menu { title: t(lang, Msg::Help).to_string(), entries: vec![item(CMD_ABOUT, Msg::About, true)] },
    ]
}

impl App {
    /// The menus before anything is open (the window is made with these; the reader puts the others).
    pub fn initial_menus(lang: Lang) -> Vec<Menu> {
        build_menus(lang, &MenuState::idle())
    }

    pub(super) fn menus(&self) -> Vec<Menu> {
        let tool = match self.edit.tool {
            Tool::Select => CMD_TOOL_SELECT,
            Tool::Markup(MarkupKind::Highlight) => CMD_TOOL_HIGHLIGHT,
            Tool::Markup(MarkupKind::Underline) => CMD_TOOL_UNDERLINE,
            Tool::Markup(MarkupKind::StrikeOut) => CMD_TOOL_STRIKE,
            Tool::Markup(MarkupKind::Squiggly) => CMD_TOOL_SQUIGGLY,
            Tool::Ink => CMD_TOOL_INK,
            Tool::Note => CMD_TOOL_NOTE,
        };
        let st = MenuState {
            has_doc: self.doc.is_some(),
            has_outline: !self.outline.is_empty(),
            show_outline: self.show_outline,
            recent: self.recent.iter().map(|e| e.path.clone()).collect(),
            can_edit: self.doc.is_some() && self.rights.annotate,
            can_undo: self.edit.can_undo,
            can_redo: self.edit.can_redo,
            has_picked: self.has_picked(),
            tool,
            color: self.edit.current_color(),
            width: self.edit.width,
        };
        build_menus(self.lang, &st)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(entries: &[MenuEntry], out: &mut Vec<(u32, String, bool)>) {
        for e in entries {
            match e {
                MenuEntry::Item { id, label, enabled, .. } => out.push((*id, label.clone(), *enabled)),
                MenuEntry::Submenu { entries, .. } => items(entries, out),
                MenuEntry::Separator => {}
            }
        }
    }

    fn all(menus: &[Menu]) -> Vec<(u32, String, bool)> {
        let mut out = Vec::new();
        for m in menus {
            items(&m.entries, &mut out);
        }
        out
    }

    #[test]
    fn there_are_six_menus_and_every_command_is_in_one() {
        let st = MenuState {
            has_doc: true,
            has_outline: true,
            recent: vec!["C:\\a.pdf".to_string(), "C:\\b&c.pdf".to_string()],
            can_edit: true,
            can_undo: true,
            can_redo: true,
            has_picked: true,
            ..MenuState::idle()
        };
        for lang in [Lang::En, Lang::Zh] {
            let menus = build_menus(lang, &st);
            assert_eq!(menus.len(), 6);
            let listed = all(&menus);
            let mut ids: Vec<u32> = listed.iter().map(|(id, ..)| *id).collect();
            ids.sort_unstable();
            let n = ids.len();
            ids.dedup();
            assert_eq!(ids.len(), n, "a command is listed twice");
            for cmd in [CMD_OPEN, CMD_PRINT, CMD_EXIT, CMD_ZOOM_IN, CMD_ZOOM_OUT, CMD_FIT_WIDTH, CMD_FIT_PAGE, CMD_ACTUAL, CMD_ROTATE, CMD_BOOKMARKS, CMD_GOTO, CMD_COPY, CMD_FIND, CMD_FIND_NEXT, CMD_FIND_PREV, CMD_ABOUT, CMD_CLEAR_RECENT, CMD_RECENT_BASE, CMD_RECENT_BASE + 1, CMD_SAVE, CMD_SAVE_AS, CMD_UNDO, CMD_REDO, CMD_REMOVE, CMD_AUTHOR, CMD_TOOL_SELECT, CMD_TOOL_HIGHLIGHT, CMD_TOOL_UNDERLINE, CMD_TOOL_STRIKE, CMD_TOOL_SQUIGGLY, CMD_TOOL_INK, CMD_TOOL_NOTE, CMD_COLOR_BASE, CMD_COLOR_BASE + 5, CMD_WIDTH_BASE, CMD_WIDTH_BASE + 3] {
                assert!(ids.contains(&cmd), "{cmd} is in no menu");
            }
            // An & in a path is doubled, so that it does not make a shortcut letter.
            assert!(listed.iter().any(|(_, label, _)| label.contains("b&&c.pdf")));
        }
    }

    #[test]
    fn what_needs_a_file_is_greyed_without_one_and_the_empty_list_says_so() {
        let none = MenuState::idle();
        let listed = all(&build_menus(Lang::En, &none));
        let enabled = |cmd: u32| listed.iter().find(|(id, ..)| *id == cmd).map(|(.., e)| *e);
        assert_eq!(enabled(CMD_OPEN), Some(true));
        assert_eq!(enabled(CMD_EXIT), Some(true));
        assert_eq!(enabled(CMD_ABOUT), Some(true));
        for cmd in [CMD_PRINT, CMD_ZOOM_IN, CMD_ROTATE, CMD_BOOKMARKS, CMD_GOTO, CMD_SAVE, CMD_SAVE_AS, CMD_UNDO, CMD_REDO, CMD_TOOL_HIGHLIGHT, CMD_TOOL_INK, CMD_TOOL_NOTE, CMD_AUTHOR, CMD_REMOVE, CMD_COPY, CMD_FIND, CMD_FIND_NEXT, CMD_FIND_PREV] {
            assert_eq!(enabled(cmd), Some(false), "{cmd}");
        }
        assert!(listed.iter().any(|(id, label, e)| *id == 0 && label == "(none)" && !e));
    }

    #[test]
    fn a_long_path_is_cut_in_the_middle() {
        let long = format!("C:\\{}\\file.pdf", "folder\\".repeat(20));
        let s = shorten(&long, 60);
        assert_eq!(s.chars().count(), 60);
        assert!(s.starts_with("C:\\") && s.ends_with("file.pdf") && s.contains('\u{2026}'));
        assert_eq!(shorten("short.pdf", 60), "short.pdf");
    }
}
