//! The menus (3d-2): File (open, recent files, print, exit), Edit (copy, find), View (zoom, turn, the sidebar), Go (page) and Help
//! (about).

use super::editing::{COLORS, Tool, WIDTHS};
use super::forms::{TEXT_SIZES, WM_ANGLE, WM_OPACITY, Watermarking};
use super::*;
use crate::ui::MenuEntry;

/// What decides how the menus look: whether a file is open (the items that need one are greyed otherwise), whether the
/// sidebar is shown, and the files in the list of recent ones.
pub(super) struct MenuState {
    pub has_doc: bool,
    pub show_sidebar: bool,
    pub recent: Vec<String>,
    /// Annotations may be added (the file allows it) and what can be undone, redone and taken out.
    pub can_edit: bool,
    pub can_undo: bool,
    pub can_redo: bool,
    pub has_picked: bool,
    /// With an annotation picked: its colour, the width of its line and its fill can be changed (the reader draws that kind and it is not locked).
    /// With none, they are for the next ones, so they can.
    pub color_ok: bool,
    pub width_ok: bool,
    pub fill_ok: bool,
    /// The shapes are filled.
    pub fill: bool,
    /// The command of the tool in use (`CMD_TOOL_*`), the colour chosen and the line width.
    pub tool: u32,
    pub color: usize,
    pub width: usize,
    pub text_size: usize,
    /// The page's content may be changed (the Insert menu), and what the watermark is like.
    pub can_modify: bool,
    /// The bookmarks may be changed (Insert > Bookmark, Tools > Bookmarks from headings).
    pub can_bookmarks: bool,
    /// Pages may be cropped (the Page menu).
    pub can_crop: bool,
    pub wm: Watermarking,
    /// Text of scans can be recognized (a file is open and no recognition is going on).
    pub can_ocr: bool,
    /// The toolbar is shown, and the choice made in View > Language.
    pub toolbar: bool,
    pub lang_choice: LangChoice,
    /// How the pages go in rows, whether the window is full screen, what the person chose for the look of the window and the colours of the pages.
    pub spread: Spread,
    pub full: bool,
    pub theme: ThemeChoice,
    pub tint: Tint,
}

impl MenuState {
    /// Nothing open, nothing changed.
    #[cfg(test)]
    pub(super) fn idle() -> MenuState {
        MenuState {
            has_doc: false,
            show_sidebar: false,
            recent: Vec::new(),
            can_edit: false,
            can_undo: false,
            can_redo: false,
            has_picked: false,
            color_ok: true,
            width_ok: true,
            fill_ok: true,
            fill: false,
            tool: CMD_TOOL_SELECT,
            color: 0,
            width: 1,
            text_size: 1,
            can_modify: false,
            can_bookmarks: false,
            can_crop: false,
            wm: Watermarking { opacity: 1, angle: 0, tile: false, under: false, page_only: false },
            can_ocr: false,
            toolbar: true,
            lang_choice: LangChoice::System,
            spread: Spread::Single,
            full: false,
            theme: ThemeChoice::System,
            tint: Tint::Original,
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

/// A submenu of choices of one kind, the one made ticked.
fn choices<T: PartialEq + Copy>(lang: Lang, title: Msg, enabled: bool, chosen: T, list: &[(u32, Msg, T)]) -> MenuEntry {
    let entries = list.iter().map(|&(id, m, v)| MenuEntry::Item { id, label: t(lang, m).to_string(), checked: chosen == v, enabled }).collect();
    MenuEntry::Submenu { title: t(lang, title).to_string(), entries }
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
                item(CMD_EXPORT_DOCX, Msg::ExportWord, st.has_doc),
                item(CMD_EXPORT_XLSX, Msg::ExportExcel, st.has_doc),
                item(CMD_EXPORT_IMAGES, Msg::ExportImgMenu, st.has_doc),
                item(CMD_EXTRACT_IMAGES, Msg::ExtractImgMenu, st.has_doc),
                item(CMD_PROTECT, Msg::ProtectMenu, st.has_doc),
                item(CMD_COMPRESS, Msg::CzMenu, st.has_doc),
                item(CMD_PROPERTIES, Msg::PropsMenu, st.has_doc),
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
                choices(lang, Msg::LayoutMenu, st.has_doc, st.spread, &[(CMD_LAYOUT_SINGLE, Msg::LayoutSingle, Spread::Single), (CMD_LAYOUT_SPREAD, Msg::LayoutSpread, Spread::Pair), (CMD_LAYOUT_COVER, Msg::LayoutCover, Spread::Cover)]),
                MenuEntry::Item { id: CMD_FULLSCREEN, label: t(lang, Msg::FullScreen).to_string(), checked: st.full, enabled: true },
                item(CMD_PRESENT, Msg::Present, st.has_doc),
                MenuEntry::Separator,
                choices(lang, Msg::ThemeMenu, true, st.theme, &[(CMD_THEME_SYSTEM, Msg::LangSystem, ThemeChoice::System), (CMD_THEME_LIGHT, Msg::ThemeLight, ThemeChoice::Light), (CMD_THEME_DARK, Msg::ThemeDark, ThemeChoice::Dark)]),
                choices(lang, Msg::PageColorMenu, st.has_doc, st.tint, &[(CMD_TINT_ORIGINAL, Msg::PcOriginal, Tint::Original), (CMD_TINT_NIGHT, Msg::PcNight, Tint::Night), (CMD_TINT_SEPIA, Msg::PcSepia, Tint::Sepia)]),
                MenuEntry::Separator,
                MenuEntry::Item { id: CMD_BOOKMARKS, label: t(lang, Msg::ShowSidebar).to_string(), checked: st.show_sidebar, enabled: st.has_doc },
                MenuEntry::Item { id: CMD_TOOLBAR, label: t(lang, Msg::ShowToolbar).to_string(), checked: st.toolbar, enabled: true },
                MenuEntry::Separator,
                MenuEntry::Submenu {
                    title: t(lang, Msg::LanguageMenu).to_string(),
                    entries: [(CMD_LANG_SYSTEM, Msg::LangSystem, LangChoice::System), (CMD_LANG_ZH, Msg::LangChinese, LangChoice::Zh), (CMD_LANG_EN, Msg::LangEnglish, LangChoice::En)]
                        .into_iter()
                        .map(|(id, m, c)| MenuEntry::Item { id, label: t(lang, m).to_string(), checked: st.lang_choice == c, enabled: true })
                        .collect(),
                },
            ],
        },
        Menu {
            title: t(lang, Msg::PageMenu).to_string(),
            entries: vec![
                item(CMD_CROP, Msg::CropMenu, st.can_crop),
                MenuEntry::Item { id: CMD_TOOL_CROP, label: t(lang, Msg::CropToolMenu).to_string(), checked: st.tool == CMD_TOOL_CROP, enabled: st.can_crop },
                item(CMD_UNCROP, Msg::UncropMenu, st.can_crop),
            ],
        },
        Menu {
            title: t(lang, Msg::CommentMenu).to_string(),
            entries: {
                let tool = |id: u32, m: Msg| MenuEntry::Item { id, label: t(lang, m).to_string(), checked: st.tool == id, enabled: id == CMD_TOOL_SELECT && st.has_doc || st.can_edit };
                let colors: Vec<MenuEntry> = COLORS.iter().enumerate().map(|(i, (m, _))| MenuEntry::Item { id: CMD_COLOR_BASE + i as u32, label: t(lang, *m).to_string(), checked: st.color == i, enabled: st.can_edit && st.color_ok }).collect();
                let sizes: Vec<MenuEntry> = TEXT_SIZES.iter().enumerate().map(|(i, (m, _))| MenuEntry::Item { id: CMD_SIZE_BASE + i as u32, label: t(lang, *m).to_string(), checked: st.text_size == i, enabled: st.can_edit }).collect();
                let widths: Vec<MenuEntry> = WIDTHS.iter().enumerate().map(|(i, (m, _))| MenuEntry::Item { id: CMD_WIDTH_BASE + i as u32, label: t(lang, *m).to_string(), checked: st.width == i, enabled: st.can_edit && st.width_ok }).collect();
                vec![
                    tool(CMD_TOOL_SELECT, Msg::ToolSelect),
                    MenuEntry::Separator,
                    tool(CMD_TOOL_HIGHLIGHT, Msg::ToolHighlight),
                    tool(CMD_TOOL_UNDERLINE, Msg::ToolUnderline),
                    tool(CMD_TOOL_STRIKE, Msg::ToolStrike),
                    tool(CMD_TOOL_SQUIGGLY, Msg::ToolSquiggly),
                    tool(CMD_TOOL_INK, Msg::ToolInk),
                    tool(CMD_TOOL_RECT, Msg::ToolRect),
                    tool(CMD_TOOL_ELLIPSE, Msg::ToolEllipse),
                    tool(CMD_TOOL_LINE, Msg::ToolLine),
                    tool(CMD_TOOL_ARROW, Msg::ToolArrow),
                    MenuEntry::Item { id: CMD_FILL, label: t(lang, Msg::FillShapes).to_string(), checked: st.fill, enabled: st.can_edit && st.fill_ok },
                    tool(CMD_TOOL_NOTE, Msg::ToolNote),
                    tool(CMD_TOOL_TEXTBOX, Msg::ToolTextBox),
                    tool(CMD_STAMP, Msg::ToolStamp),
                    MenuEntry::Separator,
                    MenuEntry::Submenu { title: t(lang, Msg::ColorMenu).to_string(), entries: colors },
                    MenuEntry::Submenu { title: t(lang, Msg::WidthMenu).to_string(), entries: widths },
                    MenuEntry::Submenu { title: t(lang, Msg::TextSizeMenu).to_string(), entries: sizes },
                    item(CMD_AUTHOR, Msg::AuthorItem, st.has_doc),
                    MenuEntry::Separator,
                    item(CMD_REMOVE, Msg::RemoveAnnot, st.has_picked && st.can_edit),
                ]
            },
        },
        Menu {
            title: t(lang, Msg::InsertMenu).to_string(),
            entries: {
                let item = |id: u32, m: Msg| MenuEntry::Item { id, label: t(lang, m).to_string(), checked: false, enabled: st.can_modify };
                let switch = |id: u32, m: Msg, on: bool| MenuEntry::Item { id, label: t(lang, m).to_string(), checked: on, enabled: st.can_modify };
                // The choices of a list are named by their numbers.
                let choices = |base: u32, names: Vec<String>, chosen: usize| {
                    names.into_iter().enumerate().map(|(i, label)| MenuEntry::Item { id: base + i as u32, label, checked: chosen == i, enabled: st.can_modify }).collect::<Vec<_>>()
                };
                let percents = WM_OPACITY.iter().map(|p| format!("{p}%")).collect();
                let degrees = WM_ANGLE.iter().map(|a| format!("{a}°")).collect();
                vec![
                    MenuEntry::Item { id: CMD_TOOL_EDITTEXT, label: t(lang, Msg::ToolEditText).to_string(), checked: st.tool == CMD_TOOL_EDITTEXT, enabled: st.can_modify },
                    MenuEntry::Separator,
                    item(CMD_INS_TEXT, Msg::InsertText),
                    item(CMD_INS_IMAGE, Msg::InsertImage),
                    MenuEntry::Separator,
                    item(CMD_INS_WM, Msg::InsertWatermark),
                    MenuEntry::Submenu { title: t(lang, Msg::WmOpacity).to_string(), entries: choices(CMD_WM_OPACITY_BASE, percents, st.wm.opacity) },
                    MenuEntry::Submenu { title: t(lang, Msg::WmAngle).to_string(), entries: choices(CMD_WM_ANGLE_BASE, degrees, st.wm.angle) },
                    switch(CMD_WM_TILE, Msg::WmTile, st.wm.tile),
                    switch(CMD_WM_UNDER, Msg::WmUnder, st.wm.under),
                    switch(CMD_WM_PAGE, Msg::WmPage, st.wm.page_only),
                    MenuEntry::Separator,
                    item(CMD_INS_PAGENUM, Msg::InsertPagenum),
                    MenuEntry::Separator,
                    MenuEntry::Item { id: CMD_BM_ADD, label: t(lang, Msg::BmAdd).to_string(), checked: false, enabled: st.can_bookmarks },
                ]
            },
        },
        Menu {
            title: t(lang, Msg::RdMenu).to_string(),
            entries: vec![
                MenuEntry::Item { id: CMD_TOOL_REDACT, label: t(lang, Msg::RdToolArea).to_string(), checked: st.tool == CMD_TOOL_REDACT, enabled: st.can_edit },
                item(CMD_REDACT_SELECTION, Msg::RdSelection, st.can_edit),
                item(CMD_REDACT_FIND, Msg::RdFind, st.can_edit),
                MenuEntry::Separator,
                item(CMD_REDACT_APPLY, Msg::RdApply, st.can_modify),
            ],
        },
        Menu {
            title: t(lang, Msg::ToolsMenu).to_string(),
            entries: vec![
                item(CMD_OCR_ALL, Msg::OcrAll, st.can_ocr),
                item(CMD_OCR_PAGE, Msg::OcrPageOnly, st.can_ocr),
                MenuEntry::Separator,
                MenuEntry::Item { id: CMD_BM_AUTO, label: t(lang, Msg::BmAuto).to_string(), checked: false, enabled: st.can_bookmarks },
            ],
        },
        Menu {
            title: t(lang, Msg::GoMenu).to_string(),
            entries: vec![item(CMD_PREV_PAGE, Msg::PrevPage, st.has_doc), item(CMD_NEXT_PAGE, Msg::NextPage, st.has_doc), MenuEntry::Separator, item(CMD_GOTO, Msg::PageNumber, st.has_doc)],
        },
        Menu { title: t(lang, Msg::Help).to_string(), entries: vec![item(CMD_ABOUT, Msg::About, true)] },
    ]
}

impl App {
    /// The command of the tool in use.
    pub(super) fn tool_command(&self) -> u32 {
        match self.edit.tool {
            Tool::Select => CMD_TOOL_SELECT,
            Tool::Markup(MarkupKind::Highlight) => CMD_TOOL_HIGHLIGHT,
            Tool::Markup(MarkupKind::Underline) => CMD_TOOL_UNDERLINE,
            Tool::Markup(MarkupKind::StrikeOut) => CMD_TOOL_STRIKE,
            Tool::Markup(MarkupKind::Squiggly | MarkupKind::Redact) => CMD_TOOL_SQUIGGLY,
            Tool::Ink => CMD_TOOL_INK,
            Tool::Note => CMD_TOOL_NOTE,
            Tool::TextBox => CMD_TOOL_TEXTBOX,
            Tool::Stamp => CMD_STAMP,
            Tool::EditText => CMD_TOOL_EDITTEXT,
            Tool::Shape(ShapeKind::Rectangle) => CMD_TOOL_RECT,
            Tool::Shape(ShapeKind::Ellipse) => CMD_TOOL_ELLIPSE,
            Tool::Shape(ShapeKind::Line) => CMD_TOOL_LINE,
            Tool::Shape(ShapeKind::Arrow) => CMD_TOOL_ARROW,
            Tool::Shape(ShapeKind::Redact) => CMD_TOOL_REDACT,
            Tool::Crop => CMD_TOOL_CROP,
        }
    }

    pub(super) fn menus(&self) -> Vec<Menu> {
        let tool = self.tool_command();
        let picked = self.edit.picked_info();
        let st = MenuState {
            has_doc: self.doc.is_some(),
            show_sidebar: self.sidebar_width() > 0,
            recent: self.recent.iter().map(|e| e.path.clone()).collect(),
            can_edit: self.doc.is_some() && self.rights.annotate,
            can_undo: self.edit.can_undo,
            can_redo: self.edit.can_redo,
            has_picked: picked.is_some_and(|a| !a.locked()),
            color_ok: picked.is_none_or(|a| a.restylable() && !a.locked()),
            width_ok: picked.is_none_or(|a| a.has_width() && !a.locked()),
            fill_ok: picked.is_none_or(|a| a.fillable() && !a.locked()),
            fill: self.edit.ae.fill,
            tool,
            color: self.edit.current_color(),
            width: self.edit.width,
            text_size: self.edit.forms.text_size,
            can_modify: self.doc.is_some() && self.rights.modify,
            can_bookmarks: self.bm_allowed(),
            can_crop: self.can_crop(),
            wm: self.edit.forms.wm,
            can_ocr: self.doc.is_some() && self.ocr.is_none(),
            toolbar: self.toolbar_on,
            lang_choice: self.lang_choice,
            spread: self.spread,
            full: self.full,
            theme: self.theme_choice,
            tint: self.tint,
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
    fn there_are_ten_menus_and_every_command_is_in_one() {
        let st = MenuState {
            has_doc: true,
            recent: vec!["C:\\a.pdf".to_string(), "C:\\b&c.pdf".to_string()],
            can_edit: true,
            can_undo: true,
            can_redo: true,
            has_picked: true,
            can_modify: true,
            can_bookmarks: true,
            can_crop: true,
            can_ocr: true,
            ..MenuState::idle()
        };
        for lang in [Lang::En, Lang::Zh] {
            let menus = build_menus(lang, &st);
            assert_eq!(menus.len(), 10);
            let listed = all(&menus);
            let mut ids: Vec<u32> = listed.iter().map(|(id, ..)| *id).collect();
            ids.sort_unstable();
            let n = ids.len();
            ids.dedup();
            assert_eq!(ids.len(), n, "a command is listed twice");
            for cmd in [CMD_OPEN, CMD_PRINT, CMD_EXIT, CMD_ZOOM_IN, CMD_ZOOM_OUT, CMD_FIT_WIDTH, CMD_FIT_PAGE, CMD_ACTUAL, CMD_ROTATE, CMD_BOOKMARKS, CMD_GOTO, CMD_COPY, CMD_FIND, CMD_FIND_NEXT, CMD_FIND_PREV, CMD_ABOUT, CMD_CLEAR_RECENT, CMD_RECENT_BASE, CMD_RECENT_BASE + 1, CMD_SAVE, CMD_SAVE_AS, CMD_EXPORT_DOCX, CMD_EXPORT_XLSX, CMD_PROTECT, CMD_COMPRESS, CMD_UNDO, CMD_REDO, CMD_REMOVE, CMD_AUTHOR, CMD_TOOL_SELECT, CMD_TOOL_HIGHLIGHT, CMD_TOOL_UNDERLINE, CMD_TOOL_STRIKE, CMD_TOOL_SQUIGGLY, CMD_TOOL_INK, CMD_TOOL_NOTE, CMD_TOOL_TEXTBOX, CMD_TOOL_EDITTEXT, CMD_STAMP, CMD_SIZE_BASE, CMD_SIZE_BASE + 3, CMD_COLOR_BASE, CMD_COLOR_BASE + 5, CMD_WIDTH_BASE, CMD_WIDTH_BASE + 3, CMD_INS_TEXT, CMD_INS_IMAGE, CMD_INS_WM, CMD_WM_OPACITY_BASE, CMD_WM_OPACITY_BASE + 2, CMD_WM_ANGLE_BASE, CMD_WM_ANGLE_BASE + 2, CMD_WM_TILE, CMD_WM_UNDER, CMD_WM_PAGE, CMD_OCR_ALL, CMD_OCR_PAGE, CMD_BM_ADD, CMD_BM_AUTO, CMD_TOOL_REDACT, CMD_REDACT_SELECTION, CMD_REDACT_FIND, CMD_REDACT_APPLY, CMD_PREV_PAGE, CMD_NEXT_PAGE, CMD_TOOLBAR, CMD_LANG_SYSTEM, CMD_LANG_ZH, CMD_LANG_EN, CMD_LAYOUT_SINGLE, CMD_LAYOUT_SPREAD, CMD_LAYOUT_COVER, CMD_FULLSCREEN, CMD_PRESENT, CMD_THEME_SYSTEM, CMD_THEME_LIGHT, CMD_THEME_DARK, CMD_TINT_ORIGINAL, CMD_TINT_NIGHT, CMD_TINT_SEPIA, CMD_CROP, CMD_TOOL_CROP, CMD_UNCROP, CMD_EXPORT_IMAGES, CMD_EXTRACT_IMAGES, CMD_PROPERTIES] {
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
        // The look of the window and full screen need no file; the layout, the presentation and the colours of the pages do.
        for cmd in [CMD_THEME_SYSTEM, CMD_THEME_LIGHT, CMD_THEME_DARK, CMD_FULLSCREEN] {
            assert_eq!(enabled(cmd), Some(true), "{cmd}");
        }
        for cmd in [CMD_LAYOUT_SINGLE, CMD_LAYOUT_SPREAD, CMD_LAYOUT_COVER, CMD_PRESENT, CMD_TINT_ORIGINAL, CMD_TINT_NIGHT, CMD_TINT_SEPIA] {
            assert_eq!(enabled(cmd), Some(false), "{cmd}");
        }
        for cmd in [CMD_PRINT, CMD_ZOOM_IN, CMD_ROTATE, CMD_BOOKMARKS, CMD_GOTO, CMD_SAVE, CMD_SAVE_AS, CMD_EXPORT_DOCX, CMD_EXPORT_XLSX, CMD_PROTECT, CMD_COMPRESS, CMD_UNDO, CMD_REDO, CMD_TOOL_HIGHLIGHT, CMD_TOOL_INK, CMD_TOOL_NOTE, CMD_INS_TEXT, CMD_INS_IMAGE, CMD_INS_WM, CMD_WM_TILE, CMD_AUTHOR, CMD_REMOVE, CMD_COPY, CMD_FIND, CMD_FIND_NEXT, CMD_FIND_PREV, CMD_OCR_ALL, CMD_OCR_PAGE, CMD_BM_ADD, CMD_BM_AUTO, CMD_CROP, CMD_TOOL_CROP, CMD_UNCROP, CMD_EXPORT_IMAGES, CMD_EXTRACT_IMAGES, CMD_PROPERTIES] {
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
