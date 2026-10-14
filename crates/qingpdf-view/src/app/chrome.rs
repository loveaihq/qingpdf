//! The window round the pages, as far as the reader decides it (2026-10-14): what the toolbar and the status bar say and when they are
//! told, the language, the settings kept between runs, the menu on the page, double and triple clicks, the I-beam over text, and the
//! edge of the bookmarks that is dragged.

use qingpdf_core::view::is_blank;

use super::editing::Tool;
use super::*;
use crate::select::{CharPos, Selection};
use crate::settings::{self, SIDEBAR_RANGE, Settings};
use crate::toolbar::{self, Inputs};
use crate::ui::MenuEntry;
use crate::wordsel;

/// How far (in points) from a character's box a click still counts as being on it.
const CLICK_SLOP: f32 = 3.0;
/// The same for the I-beam over text.
const HOVER_SLOP: f32 = 1.0;

/// What waits for the characters of a page to come from the engine.
#[derive(Clone, Copy, Debug)]
pub(super) enum PendingSelect {
    /// A double or a triple click (`clicks`) at this place of the page, in points.
    Click { page: u32, x: f64, y: f64, clicks: u8 },
    /// "Select all on this page".
    Page(u32),
}

impl PendingSelect {
    pub(super) fn page(self) -> u32 {
        match self {
            PendingSelect::Click { page, .. } | PendingSelect::Page(page) => page,
        }
    }
}

impl App {
    /// The toolbar is on the window (the speed test has none: its page area is the size it was asked for).
    pub(super) fn toolbar_visible(&self) -> bool {
        self.toolbar_on && self.bench.is_none()
    }

    /// Take what was kept between runs: the language, whether the toolbar is shown, the width of the bookmarks.
    pub fn apply_settings(&mut self, s: &Settings) {
        self.lang_choice = s.lang;
        self.lang = s.lang.resolve(self.system_lang);
        self.toolbar_on = s.toolbar;
        if let Some(w) = s.sidebar {
            self.sidebar_pref = i64::from(w);
        }
        if let Some(tab) = s.tab {
            self.tab = tab;
            self.thumbs_alone = tab == Tab::Pages;
        }
        self.toolbar_dirty = true;
        self.menus_dirty = true;
    }

    /// Change the settings in the file (not in the tests, which must not touch the person's).
    pub(super) fn keep(&self, change: impl FnOnce(&mut Settings)) {
        if self.persist {
            settings::update(change);
        }
    }

    /// What the window is to be told about its toolbar and status bar after an event: only what has changed since it was last told.
    pub(super) fn chrome_updates(&mut self) -> Vec<Action> {
        let mut actions = Vec::new();
        if std::mem::take(&mut self.toolbar_dirty) {
            actions.push(Action::Toolbar(toolbar::spec(self.lang, self.toolbar_visible())));
            // The buttons are made again, from nothing: they are told how they are.
            self.last_toolbar = None;
        }
        let inputs = Inputs { has_doc: self.doc.is_some(), page: self.current_page(), pages: self.page_count(), can_edit: self.doc.is_some() && self.rights.annotate, tool: self.tool_command() };
        let state = toolbar::state(&inputs);
        if self.last_toolbar.as_ref() != Some(&state) {
            self.last_toolbar = Some(state.clone());
            actions.push(Action::ToolbarState(state));
        }
        let status = self.status_text();
        if self.last_status.as_ref() != Some(&status) {
            self.last_status = Some(status.clone());
            actions.push(Action::Status(status));
        }
        actions
    }

    /// The status bar: the page, a note, the zoom.
    fn status_text(&self) -> StatusText {
        if self.doc.is_none() {
            return StatusText::default();
        }
        StatusText {
            page: i18n::page_of(self.lang, self.current_page() + 1, self.page_count()),
            note: self.note.clone().unwrap_or_default(),
            zoom: format!("{}%", (f64::from(self.zoom) / 10.0).round() as u32),
        }
    }

    // --- the language, the toolbar, the pages ------------------------------------------------------------------

    /// View > Language: the menus, the toolbar's tooltips, the status bar and the title are put again in the language, now.
    pub(super) fn set_language(&mut self, choice: LangChoice) -> Vec<Action> {
        self.lang_choice = choice;
        self.lang = choice.resolve(self.system_lang);
        self.keep(|s| s.lang = choice);
        self.menus_dirty = true;
        self.toolbar_dirty = true;
        vec![Action::SetTitle(self.title()), Action::Invalidate]
    }

    /// View > Toolbar.
    pub(super) fn toggle_toolbar(&mut self) -> Vec<Action> {
        self.toolbar_on = !self.toolbar_on;
        let on = self.toolbar_on;
        self.keep(|s| s.toolbar = on);
        self.toolbar_dirty = true;
        self.menus_dirty = true;
        // The find box of the toolbar is gone, or comes: what is open starts again with the other.
        if self.find_open { self.close_find() } else { Vec::new() }
    }

    /// The button of a tool: a tool in use is let go of.
    pub(super) fn toggle_tool(&mut self, tool: Tool) -> Vec<Action> {
        if self.edit.tool == tool { self.set_tool(Tool::Select) } else { self.set_tool(tool) }
    }

    /// The page after (`forward`) or before the one in view.
    pub(super) fn step_page(&mut self, forward: bool) -> Vec<Action> {
        if self.doc.is_none() {
            return Vec::new();
        }
        let here = self.current_page();
        let to = if forward { here + 1 } else { here.saturating_sub(1) };
        self.go_to_page(to.min(self.page_count().saturating_sub(1)))
    }

    /// The keyboard went into the search box of the toolbar: it is the find box, open.
    pub(super) fn find_focused(&mut self) -> Vec<Action> {
        if self.doc.is_none() || self.find_open {
            return Vec::new();
        }
        self.find_open = true;
        let mut actions = vec![Action::Invalidate];
        if self.search.is_none() && !self.find_text.trim().is_empty() {
            actions.extend(self.start_search(&self.find_text.clone()));
        }
        actions
    }

    // --- the menu on the page -----------------------------------------------------------------------------------

    /// The right button: copy and highlight for the text that is selected, a note and "select all" for the page under the pointer.
    pub(super) fn context_menu(&mut self, x: i64, y: i64) -> Vec<Action> {
        if self.doc.is_none() {
            return Vec::new();
        }
        let over = self.page_at_point(x, y).and_then(|(page, px, py)| {
            let (xp, yp) = self.doc.as_ref()?.layout.page_map(page)?.to_pt(px, py);
            Some((page as u32, xp as f32, yp as f32))
        });
        if over.is_none() && self.selection.is_none() {
            return Vec::new();
        }
        self.ctx_at = over;
        let has_selection = self.selection.is_some();
        let item = |id: u32, m: Msg, enabled: bool| MenuEntry::Item { id, label: t(self.lang, m).to_string(), checked: false, enabled };
        let entries = vec![
            item(CMD_COPY, Msg::Copy, has_selection && self.rights.copy),
            item(CMD_CTX_HIGHLIGHT, Msg::CtxHighlight, has_selection && self.rights.annotate),
            MenuEntry::Separator,
            item(CMD_CTX_NOTE, Msg::CtxNote, over.is_some() && self.rights.annotate),
            item(CMD_CTX_SELECT_ALL, Msg::CtxSelectAll, over.is_some() && (self.rights.copy || self.rights.annotate)),
        ];
        vec![Action::ContextMenu { entries, at: (clamp32(x), clamp32(y)) }]
    }

    /// "Select all on this page", for the page the menu was opened on.
    pub(super) fn select_all_here(&mut self) -> Vec<Action> {
        let Some((page, ..)) = self.ctx_at.take() else { return Vec::new() };
        if self.boxes.contains_key(&page) {
            return self.select_page(page);
        }
        self.pending_select = Some(PendingSelect::Page(page));
        self.ask_for_boxes(page)
    }

    fn select_page(&mut self, page: u32) -> Vec<Action> {
        let Some(b) = self.boxes.get(&page) else { return Vec::new() };
        let (Some(first), Some(last)) = (b.boxes.iter().position(|r| !is_blank(r)), b.boxes.iter().rposition(|r| !is_blank(r))) else { return Vec::new() };
        self.selection = Some(Selection { anchor: CharPos { page, index: first as u32 }, head: CharPos { page, index: last as u32 } });
        vec![Action::Invalidate]
    }

    // --- double and triple clicks, the I-beam ------------------------------------------------------------------------

    /// A double click (a word) or a triple click (a line) at a place of a page (points).
    pub(super) fn select_by_clicks(&mut self, page: u32, x: f64, y: f64, clicks: u8) -> Vec<Action> {
        // Without the right to copy the engine gives no characters, only where they are: nothing to find words in.
        if !self.rights.copy {
            return Vec::new();
        }
        if !self.boxes.contains_key(&page) {
            self.pending_select = Some(PendingSelect::Click { page, x, y, clicks });
            return self.ask_for_boxes(page);
        }
        self.pick_by_clicks(page, x, y, clicks)
    }

    fn pick_by_clicks(&mut self, page: u32, x: f64, y: f64, clicks: u8) -> Vec<Action> {
        let Some(b) = self.boxes.get(&page) else { return Vec::new() };
        if b.text.len() != b.boxes.len() {
            return Vec::new();
        }
        let Some(i) = b.nearest(x as f32, y as f32) else { return Vec::new() };
        let Some(r) = b.boxes.get(i) else { return Vec::new() };
        // A click out in the margin, or far from the text, takes nothing.
        let (xf, yf) = (x as f32, y as f32);
        if xf < r[0] - CLICK_SLOP || xf > r[2] + CLICK_SLOP || yf < r[1] - CLICK_SLOP || yf > r[3] + CLICK_SLOP {
            return Vec::new();
        }
        let range = if clicks >= 3 { wordsel::line_range(&b.text, i) } else { wordsel::word_range(&b.text, i) };
        let Some((a, z)) = range else { return Vec::new() };
        self.selection = Some(Selection { anchor: CharPos { page, index: a as u32 }, head: CharPos { page, index: (z - 1) as u32 } });
        let mut actions = vec![Action::Invalidate];
        // With a marking tool in hand the word or line is marked, as a drag over it would.
        actions.extend(self.drag_finished());
        actions
    }

    /// The characters of a page have come: do what waited for them.
    pub(super) fn finish_pending_select(&mut self, page: u32) -> Vec<Action> {
        match self.pending_select {
            Some(p) if p.page() == page => {
                self.pending_select = None;
                match p {
                    PendingSelect::Click { x, y, clicks, .. } => self.pick_by_clicks(page, x, y, clicks),
                    PendingSelect::Page(_) => self.select_page(page),
                }
            }
            _ => Vec::new(),
        }
    }

    /// The shape of the pointer over text that can be selected: an I-beam. (The characters of a page are asked for when the pointer is
    /// first over it; until they have come the pointer is an arrow.)
    pub(super) fn text_cursor(&mut self, x: i64, y: i64) -> Cursor {
        if self.edit.tool != Tool::Select || (!self.rights.copy && !self.rights.annotate) {
            return Cursor::Arrow;
        }
        let Some((page, px, py)) = self.page_at_point(x, y) else { return Cursor::Arrow };
        let Some(map) = self.doc.as_ref().and_then(|d| d.layout.page_map(page)) else { return Cursor::Arrow };
        let (xp, yp) = map.to_pt(px, py);
        let Some(b) = self.boxes.get(&(page as u32)) else {
            self.ask_for_boxes(page as u32);
            return Cursor::Arrow;
        };
        let (xf, yf) = (xp as f32, yp as f32);
        match b.nearest(xf, yf).and_then(|i| b.boxes.get(i)) {
            Some(r) if xf >= r[0] - HOVER_SLOP && xf <= r[2] + HOVER_SLOP && yf >= r[1] - HOVER_SLOP && yf <= r[3] + HOVER_SLOP => Cursor::Text,
            _ => Cursor::Arrow,
        }
    }

    // --- the edge of the bookmarks -----------------------------------------------------------------------------------

    /// Is this place on the strip between the bookmarks and the pages, which is dragged to change the width of the bookmarks?
    pub(super) fn on_split(&self, x: i64, y: i64) -> bool {
        self.view_x > 0 && x >= self.view_x - self.px(SPLIT_GAP) && x < self.view_x && y >= 0 && y < self.view_h
    }

    /// The pointer is dragging the edge, now at `x`.
    pub(super) fn split_drag_to(&mut self, x: i64) -> Vec<Action> {
        let Some((from, start)) = self.split_drag else { return Vec::new() };
        let dpi = i64::from(self.dpi).max(1);
        let (low, high) = (i64::from(*SIDEBAR_RANGE.start()), i64::from(*SIDEBAR_RANGE.end()));
        // No wider than half the window, and no narrower than the least that is kept.
        let most = ((self.client_w / 2 - self.px(SPLIT_GAP)) * 96 / dpi).clamp(low, high);
        self.sidebar_pref = (start + (x - from) * 96 / dpi).clamp(low, most);
        let width = self.sidebar_width();
        if width == self.view_x {
            return Vec::new();
        }
        self.view_x = width;
        self.view_w = (self.client_w - width).max(0);
        let (ax, ay) = (self.view_w / 2, self.view_h / 2);
        self.relayout(ax, ay, None);
        let mut actions = self.after_view_change(true);
        actions.extend(self.chrome());
        actions
    }

    /// The drag is over: the width is kept for the next run, and the pages are drawn sharp.
    pub(super) fn split_drag_end(&mut self) -> Vec<Action> {
        if self.split_drag.take().is_none() {
            return Vec::new();
        }
        let width = self.sidebar_pref as i32;
        self.keep(|s| s.sidebar = Some(width));
        self.after_view_change(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Instant;

    fn app() -> App {
        let mut app = App::new(Lang::En, Arc::new(|| {}), Instant::now(), 1920 * 1080, "Tester".to_string());
        // The tests keep nothing: not the settings, not the list of recent files.
        app.persist = false;
        app.recent_path = None;
        app
    }

    #[test]
    fn the_language_is_put_at_once_and_follows_the_system_when_asked() {
        let mut app = app();
        app.apply_settings(&Settings { lang: LangChoice::Zh, ..Settings::default() });
        assert_eq!(app.lang, Lang::Zh);
        let actions = app.event(Event::Command(CMD_LANG_EN));
        assert_eq!(app.lang, Lang::En);
        // The menus and the toolbar's words come again in the new language, and the window is told what the title is.
        assert!(actions.iter().any(|a| matches!(a, Action::SetMenus(m) if m.first().is_some_and(|m| m.title.contains("File")))));
        assert!(actions.iter().any(|a| matches!(a, Action::Toolbar(s) if s.items.iter().any(|i| matches!(i, crate::ui::ToolItem::Button { tip, .. } if tip.starts_with("Open"))))));
        assert!(actions.iter().any(|a| matches!(a, Action::SetTitle(_))));
        let actions = app.event(Event::Command(CMD_LANG_ZH));
        assert!(actions.iter().any(|a| matches!(a, Action::SetMenus(m) if m.first().is_some_and(|m| m.title.starts_with("文件")))));
        assert!(actions.iter().any(|a| matches!(a, Action::Toolbar(s) if s.items.iter().any(|i| matches!(i, crate::ui::ToolItem::Button { tip, .. } if tip.starts_with("打开"))))));
        // "Follow the system" is the language the program started in (English here).
        app.event(Event::Command(CMD_LANG_SYSTEM));
        assert_eq!(app.lang, Lang::En);
    }

    #[test]
    fn the_toolbar_is_told_once_and_again_only_when_something_changes() {
        let mut app = app();
        let first = app.event(Event::Size { w: 1000, h: 700 });
        assert!(first.iter().any(|a| matches!(a, Action::Toolbar(s) if s.visible && s.items.len() == 17)));
        assert!(first.iter().any(|a| matches!(a, Action::ToolbarState(s) if s.buttons.iter().filter(|b| b.1).count() == 1)));
        // Nothing changed: nothing is said.
        let second = app.event(Event::Size { w: 1000, h: 700 });
        assert!(!second.iter().any(|a| matches!(a, Action::Toolbar(_) | Action::ToolbarState(_) | Action::Status(_))));
        // Hiding the toolbar tells the window, and the menu's tick goes.
        let hidden = app.event(Event::Command(CMD_TOOLBAR));
        assert!(hidden.iter().any(|a| matches!(a, Action::Toolbar(s) if !s.visible)));
        let shown = app.event(Event::Command(CMD_TOOLBAR));
        assert!(shown.iter().any(|a| matches!(a, Action::Toolbar(s) if s.visible)));
    }

    fn pump(app: &mut App, what: &str, until: impl Fn(&App) -> bool) {
        for _ in 0..1000 {
            app.event(Event::Wake);
            if until(app) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("timed out waiting for {what}");
    }

    /// The place in the window of the middle of character `i` of the first page.
    fn middle_of(app: &App, i: usize) -> (i32, i32) {
        let l = &app.doc.as_ref().unwrap().layout;
        let b = app.boxes.get(&0).unwrap().boxes[i];
        let (px, py) = l.page_map(0).unwrap().to_px(f64::from(b[0] + b[2]) / 2.0, f64::from(b[1] + b[3]) / 2.0);
        ((app.view_x + l.left(0, app.view_w, app.scroll_x) + px as i64) as i32, (l.top(0) - app.scroll_y + py as i64) as i32)
    }

    fn selected(app: &App) -> Option<((u32, u32), (u32, u32))> {
        app.selection.map(|s| {
            let (a, b) = s.ordered();
            ((a.page, a.index), (b.page, b.index))
        })
    }

    #[test]
    fn the_menu_on_the_page_needs_an_open_file_and_a_double_or_triple_click_takes_a_word_or_a_line() {
        let mut app = app();
        app.event(Event::Size { w: 900, h: 700 });
        assert!(!app.event(Event::ContextMenu { x: 10, y: 10 }).iter().any(|a| matches!(a, Action::ContextMenu { .. })));
        let corpus = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/corpus/public/xref-classic/hello_world.pdf");
        app.open_path(std::path::Path::new(corpus), "");
        pump(&mut app, "the file to open", |a| a.has_document());
        // The pointer over a page asks for its characters (it is an arrow until they come) and is an I-beam over a letter.
        assert_eq!(app.cursor_at(450, 300), Cursor::Arrow);
        pump(&mut app, "the characters", |a| a.boxes.contains_key(&0));
        // "Hello, world!": the w of "world" is the 8th character (index 7); the page's line ends are line feeds.
        let (x, y) = middle_of(&app, 8);
        assert_eq!(app.cursor_at(x, y), Cursor::Text);
        assert_eq!(app.cursor_at(880, 690), Cursor::Arrow);
        let click = |app: &mut App, clicks: u8| {
            app.event(Event::MouseDown { x, y, ctrl: false, shift: false, clicks });
            app.event(Event::MouseUp { x, y });
        };
        click(&mut app, 2);
        assert_eq!(selected(&app), Some(((0, 7), (0, 11))), "world");
        click(&mut app, 3);
        assert_eq!(selected(&app), Some(((0, 0), (0, 12))), "the line");
        // A click far from any letter takes nothing, and a single click lets go of the selection.
        app.event(Event::MouseDown { x: 880, y: 690, ctrl: false, shift: false, clicks: 2 });
        assert_eq!(selected(&app), None);
        click(&mut app, 2);
        assert!(app.selection.is_some());
        click(&mut app, 1);
        assert_eq!(selected(&app), None);

        // The menu: with a selection copy and highlight can be chosen, and the note and "select all" belong to the page it was opened on.
        click(&mut app, 2);
        let menu = app.event(Event::ContextMenu { x, y });
        let entries = menu.iter().find_map(|a| if let Action::ContextMenu { entries, .. } = a { Some(entries) } else { None }).expect("a menu");
        let enabled = |cmd: u32| entries.iter().find_map(|e| if let MenuEntry::Item { id, enabled, .. } = e { (*id == cmd).then_some(*enabled) } else { None });
        for cmd in [CMD_COPY, CMD_CTX_HIGHLIGHT, CMD_CTX_NOTE, CMD_CTX_SELECT_ALL] {
            assert_eq!(enabled(cmd), Some(true), "{cmd}");
        }
        let note = app.event(Event::Command(CMD_CTX_NOTE));
        assert!(note.iter().any(|a| matches!(a, Action::AskText { tag: TAG_NOTE, .. })));
        app.event(Event::Text { tag: TAG_NOTE, text: None });
        app.event(Event::ContextMenu { x, y });
        app.event(Event::Command(CMD_CTX_SELECT_ALL));
        let (first, last) = selected(&app).expect("everything is selected");
        assert_eq!((first, last.0), ((0, 0), 0));
        assert!(last.1 as usize >= 26, "the last character of the page");
        // With nothing selected, copy and highlight are greyed.
        app.event(Event::MouseDown { x, y, ctrl: false, shift: false, clicks: 1 });
        app.event(Event::MouseUp { x, y });
        let menu = app.event(Event::ContextMenu { x, y });
        let entries = menu.iter().find_map(|a| if let Action::ContextMenu { entries, .. } = a { Some(entries) } else { None }).expect("a menu");
        assert!(entries.iter().any(|e| matches!(e, MenuEntry::Item { id, enabled: false, .. } if *id == CMD_COPY)));
        assert!(entries.iter().any(|e| matches!(e, MenuEntry::Item { id, enabled: false, .. } if *id == CMD_CTX_HIGHLIGHT)));
    }

    #[test]
    fn the_page_buttons_the_page_box_and_the_tool_buttons_do_what_they_say() {
        let mut app = app();
        app.event(Event::Size { w: 900, h: 700 });
        let corpus = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/corpus/public/xref-classic/hello_world_2_pages.pdf");
        app.open_path(std::path::Path::new(corpus), "");
        pump(&mut app, "the file to open", |a| a.has_document());
        let state = |actions: &[Action]| actions.iter().rev().find_map(|a| if let Action::ToolbarState(s) = a { Some(s.clone()) } else { None });
        let of = |s: &ToolbarState, cmd: u32| s.buttons.iter().find(|b| b.0 == cmd).map(|b| (b.1, b.2));
        assert_eq!(app.current_page(), 0);
        let next = app.event(Event::Command(CMD_NEXT_PAGE));
        assert_eq!(app.current_page(), 1);
        let st = state(&next).expect("the toolbar is told the page changed");
        assert_eq!(st.page_text, "2 / 2");
        assert_eq!((of(&st, CMD_PREV_PAGE), of(&st, CMD_NEXT_PAGE)), (Some((true, false)), Some((false, false))));
        // The keys: Ctrl+PgUp goes back, Ctrl+PgDn forward (and stops at the last page).
        app.event(Event::Key { vk: vk::PRIOR, ctrl: true, shift: false });
        assert_eq!(app.current_page(), 0);
        app.event(Event::Key { vk: vk::NEXT, ctrl: true, shift: false });
        app.event(Event::Key { vk: vk::NEXT, ctrl: true, shift: false });
        assert_eq!(app.current_page(), 1);
        // What is typed in the page box.
        app.event(Event::PageBox("1 / 2".to_string()));
        assert_eq!(app.current_page(), 0);
        app.event(Event::PageBox("nonsense".to_string()));
        assert_eq!(app.current_page(), 0);
        // A tool button presses its tool, and the same button lets it go.
        let on = app.event(Event::Command(CMD_TOOL_INK));
        assert_eq!(of(&state(&on).expect("told"), CMD_TOOL_INK), Some((true, true)));
        let off = app.event(Event::Command(CMD_TOOL_INK));
        assert_eq!(of(&state(&off).expect("told"), CMD_TOOL_INK), Some((true, false)));
    }

    #[test]
    fn the_edge_of_the_bookmarks_is_where_the_strip_between_them_and_the_pages_is() {
        let mut app = app();
        app.client_w = 1000;
        app.view_h = 700;
        assert!(!app.on_split(260, 100));
        app.view_x = app.px(SPLIT_GAP) + app.px(260);
        assert!(app.on_split(app.view_x - 1, 100));
        assert!(app.on_split(app.view_x - app.px(SPLIT_GAP), 100));
        assert!(!app.on_split(app.view_x, 100));
        assert!(!app.on_split(app.view_x - app.px(SPLIT_GAP) - 1, 100));
        assert!(!app.on_split(app.view_x - 1, 701));
    }
}
