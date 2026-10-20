//! Editing the bookmarks in the sidebar (2026-10-20): the item selected in the tree, adding, typing over a title, taking out, moving,
//! one level deeper or shallower, the place an item leads to, the bookmarks made from the headings, and what the engine answers. The list
//! and its rules are `qingpdf_core::bookmarks`; the tree itself, with its drag and its box for typing, is the window's.
//!
//! A change is the whole list sent to the engine (each item with the object it was read from, a new one with 0) and nothing is shown until the
//! engine has answered and the list has been read again: the tree is then what the file has, and the numbers in it are the right ones. Until
//! that is so, another change waits by being ignored (it takes a few milliseconds), as the changes to the pages do.

use std::time::Duration;

use qingpdf_core::bookmarks as marks;
use qingpdf_core::view::{MAX_TITLE_CHARS, SaveStatus, TextStatus};

use super::editing::Asked;
use super::*;
use crate::ui::MenuEntry;

/// A second click on the item that is selected starts typing over its title if it comes after the time of a double click and before this.
const SLOW_MIN: Duration = Duration::from_millis(600);
const SLOW_MAX: Duration = Duration::from_millis(2500);
/// How many characters of the page's text a title is taken from when what is selected is long.
const TITLE_SPAN: u32 = 600;

/// A bookmark that waits for its title: the request that finds it (the selected text, or the headings of the page), the place it leads to
/// and the item it goes after.
struct AddWait {
    id: u64,
    page: u32,
    x: f32,
    y: f32,
    after: Option<usize>,
    /// The request is for the headings of the page (the text that is selected gave nothing).
    headings: bool,
}

#[derive(Default)]
pub(super) struct BmWork {
    /// The item selected in the tree (its place in `App::outline`), and the last click on an item.
    pub sel: Option<usize>,
    clicked: Option<(usize, Instant)>,
    /// The keys belong to the tree (it was clicked last).
    pub focus: bool,
    /// The list was sent and has not come back: the numbers in `App::outline` are not the file's until it does.
    pub reload: bool,
    /// The item to select when the list comes back.
    want_sel: Option<usize>,
    add: Option<AddWait>,
    /// The request for the headings of the whole file, and what it found while the person is asked.
    auto: Option<u64>,
    found: Vec<OutlineItem>,
}

/// What is typed or selected, as a title: one line, not longer than a title may be.
fn title_of(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(MAX_TITLE_CHARS).collect()
}

impl App {
    /// May the bookmarks of the open file be changed (permission bit 11 or 4, or the owner password)?
    pub(super) fn bm_allowed(&self) -> bool {
        self.doc.is_some() && (self.rights.assemble || self.rights.modify)
    }

    /// Are the bookmarks on the screen, in the tree?
    pub(super) fn bookmarks_shown(&self) -> bool {
        self.view_x > 0 && self.doc.is_some() && self.active_tab() == Tab::Bookmarks
    }

    /// Why the bookmarks cannot be changed now, as the actions that say so; `None` when they can.
    fn bm_not_now(&mut self) -> Option<Vec<Action>> {
        if self.doc.is_none() {
            return Some(Vec::new());
        }
        if !self.bm_allowed() {
            return Some(self.say(t(self.lang, Msg::NoteBmDenied).to_string()));
        }
        if self.ocr.is_some() || self.print.is_some() {
            return Some(self.say(t(self.lang, Msg::NoteBusy).to_string()));
        }
        // It waits for the answer (a few milliseconds) by being ignored.
        (self.pages_busy() || self.bm.reload || self.bm.add.is_some()).then(Vec::new)
    }

    /// The tree as it is to be: the list, and the item to select.
    pub(super) fn outline_action(&self, select: Option<usize>) -> Action {
        Action::Outline { nodes: self.outline_nodes(), select }
    }

    /// Send the list to the engine as the bookmarks of the file; `select` is the item to select when it has come back.
    fn bm_send(&mut self, items: Vec<OutlineItem>, select: Option<usize>) -> Vec<Action> {
        let id = self.new_id();
        self.engine.set_bookmarks(self.doc_id, id, items);
        self.edit.asked.insert(id, Asked::Bookmarks);
        self.bm.reload = true;
        self.bm.want_sel = select;
        Vec::new()
    }

    /// The engine has the bookmarks of the file (read again after a change, an undo or a redo, or the first time).
    pub(super) fn bm_listed(&mut self, first: bool) -> Vec<Action> {
        self.bm.reload = false;
        let select = self.bm.want_sel.take().or(self.bm.sel).filter(|&i| i < self.outline.len());
        self.bm.sel = select;
        self.bm.clicked = None;
        let mut actions = vec![self.outline_action(select)];
        // The first bookmark of a file that had none is shown.
        if first && select.is_some() && !self.outline.is_empty() {
            actions.extend(self.choose_tab(Tab::Bookmarks));
        }
        actions
    }

    /// The engine could not make the change (the file does not allow it, it is signed, the list was not the file's: a file with damaged
    /// structure data is written afresh by the first change, and the numbers in the list are not its objects' any more): the bookmarks are
    /// read again, and the tree is put as they are.
    pub(super) fn bm_failed(&mut self) -> Vec<Action> {
        self.bm.reload = true;
        self.bm.want_sel = None;
        self.ask_for_outline()
    }

    // --- the tree ----------------------------------------------------------------------------------------------

    /// An item was clicked or reached with the keys. `true`: it is the slow second click on the item that was selected, which starts typing
    /// over its title.
    pub(super) fn bm_clicked(&mut self, index: usize) -> bool {
        let slow = self.bm.clicked.is_some_and(|(i, at)| i == index && (SLOW_MIN..SLOW_MAX).contains(&at.elapsed())) && self.bm.sel == Some(index);
        self.bm.sel = Some(index);
        self.bm.focus = true;
        self.bm.clicked = Some((index, Instant::now()));
        slow && self.bm_allowed()
    }

    /// An item was opened or closed: the list says so, and the next change to the bookmarks writes it.
    pub(super) fn bm_expanded(&mut self, index: usize, open: bool) -> Vec<Action> {
        if let Some(item) = self.outline.get_mut(index) {
            item.open = open;
        }
        Vec::new()
    }

    /// What the tree shows for an item: its title, or the word for none.
    pub(super) fn shown_title(&self, item: &OutlineItem) -> String {
        if item.title.trim().is_empty() { t(self.lang, Msg::Untitled).to_string() } else { item.title.clone() }
    }

    /// The title of an item was typed over in the tree. Leaving the box as it was (Enter without typing) is no change: what is in it then is
    /// what the tree showed, so that is what it is compared with (an item with no title shows the word for none).
    pub(super) fn bm_renamed(&mut self, index: usize, text: &str) -> Vec<Action> {
        let title = title_of(&text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>());
        if title.is_empty() || self.outline.get(index).is_none_or(|i| title_of(&self.shown_title(i)) == title) {
            return vec![self.outline_action(self.bm.sel)];
        }
        if let Some(mut actions) = self.bm_not_now() {
            actions.push(self.outline_action(self.bm.sel));
            return actions;
        }
        let mut items = self.outline.clone();
        if let Some(item) = items.get_mut(index) {
            item.title = title;
        }
        self.bm_send(items, Some(index))
    }

    /// An item was dragged to a place of another.
    pub(super) fn bm_moved(&mut self, from: usize, to: usize, place: marks::Place) -> Vec<Action> {
        if let Some(actions) = self.bm_not_now() {
            return actions;
        }
        let mut items = self.outline.clone();
        let into_itself = (from..marks::branch_end(&items, from)).contains(&to);
        match marks::move_branch(&mut items, from, to, place) {
            Some(at) => self.bm_send(items, Some(at)),
            None if into_itself => self.say(t(self.lang, Msg::NoteBmNo).to_string()),
            None => Vec::new(),
        }
    }

    // --- the commands ------------------------------------------------------------------------------------------

    /// The item the commands are for.
    fn bm_item(&self) -> Option<usize> {
        self.bm.sel.filter(|&i| i < self.outline.len())
    }

    /// The page at the top of the window, and the place the top left corner of the window is at on it (points of the page as shown).
    fn view_place(&self) -> Option<(u32, f32, f32)> {
        let doc = self.doc.as_ref()?;
        let probe = self.scroll_y + self.px(MARGIN) / 2;
        let page = doc.layout.page_at(probe);
        let map = doc.layout.page_map(page)?;
        let down = (probe - doc.layout.top(page)).max(0);
        let across = (-doc.layout.left(page, self.view_w, self.scroll_x)).max(0);
        let (x, y) = map.to_pt(across as f64, down as f64);
        Some((u32::try_from(page).ok()?, x.max(0.0) as f32, y.max(0.0) as f32))
    }

    /// Add a bookmark that leads to the top of the window, after the selected one (and its branch) or at the end. Its title is the text that is
    /// selected, else the heading nearest to the place on its page, else the number of the page.
    pub(super) fn bm_add(&mut self) -> Vec<Action> {
        if let Some(actions) = self.bm_not_now() {
            return actions;
        }
        let Some((page, x, y)) = self.view_place() else { return Vec::new() };
        let after = self.bm_item();
        let id = self.new_id();
        let span = self.selection.filter(|_| self.rights.copy).map(|s| s.copy_span());
        match span {
            Some((from, to)) => {
                let to = if to.0 == from.0 { (to.0, to.1.min(from.1.saturating_add(TITLE_SPAN))) } else { (from.0, from.1.saturating_add(TITLE_SPAN)) };
                self.engine.copy_text(self.doc_id, id, from, to);
            }
            None => self.engine.headings(self.doc_id, id, Some(page)),
        }
        self.bm.add = Some(AddWait { id, page, x, y, after, headings: span.is_none() });
        Vec::new()
    }

    /// The new bookmark has its title.
    fn bm_added(&mut self, title: &str) -> Vec<Action> {
        let Some(wait) = self.bm.add.take() else { return Vec::new() };
        if let Some(actions) = self.bm_not_now() {
            return actions;
        }
        let title = if title.is_empty() { t(self.lang, Msg::BmPageTitle).replace("{n}", &(wait.page + 1).to_string()) } else { title.to_string() };
        let mut items = self.outline.clone();
        let item = OutlineItem::made(&title, 0, Some(wait.page), Some(wait.x), Some(wait.y));
        let after = wait.after.filter(|&i| i < items.len());
        match marks::add(&mut items, after, item) {
            Some(at) => self.bm_send(items, Some(at)),
            None => self.say(t(self.lang, Msg::NoteBmNo).to_string()),
        }
    }

    /// The text of the selection came (for a title).
    pub(super) fn bm_copied(&mut self, id: u64, status: TextStatus, text: &str) -> Option<Vec<Action>> {
        if self.bm.add.as_ref().is_none_or(|a| a.id != id || a.headings) {
            return None;
        }
        let title = title_of(text);
        if status == TextStatus::Ok && !title.is_empty() {
            return Some(self.bm_added(&title));
        }
        // Nothing to take the title from: the headings of the page are looked at.
        let next = self.new_id();
        let page = self.bm.add.as_ref().map_or(0, |a| a.page);
        self.engine.headings(self.doc_id, next, Some(page));
        if let Some(a) = self.bm.add.as_mut() {
            (a.id, a.headings) = (next, true);
        }
        Some(Vec::new())
    }

    /// The bookmarks the headings make came: for the title of a new bookmark (the one nearest to its place), or for the whole file.
    pub(super) fn on_headings(&mut self, id: u64, status: SaveStatus, message: &str, items: Vec<OutlineItem>) -> Vec<Action> {
        if let Some(wait) = self.bm.add.as_ref().filter(|a| a.id == id && a.headings) {
            let y = wait.y;
            let nearest = items.iter().filter(|i| status == SaveStatus::Done && i.page == Some(wait.page)).min_by(|a, b| {
                let dist = |i: &OutlineItem| (i.y.unwrap_or(0.0) - y).abs();
                dist(a).total_cmp(&dist(b))
            });
            let title = nearest.map_or_else(String::new, |i| i.title.clone());
            return self.bm_added(&title);
        }
        if self.bm.auto != Some(id) {
            return Vec::new();
        }
        self.bm.auto = None;
        if status != SaveStatus::Done {
            return self.say(format!("{} {message}", t(self.lang, Msg::NoteEditFailed)));
        }
        if items.is_empty() {
            return self.say(t(self.lang, Msg::NoteBmNoHeadings).to_string());
        }
        let mut text = t(self.lang, Msg::BmAutoAsk).replace("{n}", &items.len().to_string());
        if !self.outline.is_empty() {
            text.push_str("\n\n");
            text.push_str(&t(self.lang, Msg::BmAutoReplace).replace("{m}", &self.outline.len().to_string()));
        }
        self.bm.found = items;
        vec![Action::Confirm { tag: TAG_BM_AUTO, title: t(self.lang, Msg::TabBookmarks).to_string(), text }]
    }

    /// The answer to "make them the bookmarks?".
    pub(super) fn bm_auto_answered(&mut self, yes: bool) -> Vec<Action> {
        let found = std::mem::take(&mut self.bm.found);
        if !yes || found.is_empty() {
            return Vec::new();
        }
        if let Some(actions) = self.bm_not_now() {
            return actions;
        }
        self.bm_send(found, Some(0))
    }

    /// Look for the headings of the file, to make its bookmarks of them (the person is asked first).
    pub(super) fn bm_auto(&mut self) -> Vec<Action> {
        if let Some(actions) = self.bm_not_now() {
            return actions;
        }
        if let Some(old) = self.bm.auto.take() {
            self.engine.cancel(old);
        }
        let id = self.new_id();
        self.engine.headings(self.doc_id, id, None);
        self.bm.auto = Some(id);
        self.say(t(self.lang, Msg::NoteBmSearching).to_string())
    }

    pub(super) fn bm_rename(&mut self) -> Vec<Action> {
        match self.bm_item() {
            Some(i) if self.bm_not_now().is_none() => vec![Action::OutlineRename(i)],
            _ => Vec::new(),
        }
    }

    pub(super) fn bm_delete(&mut self) -> Vec<Action> {
        let Some(sel) = self.bm_item() else { return Vec::new() };
        if let Some(actions) = self.bm_not_now() {
            return actions;
        }
        let mut items = self.outline.clone();
        marks::remove(&mut items, sel);
        let next = items.len().checked_sub(1).map(|last| sel.min(last));
        self.bm_send(items, next)
    }

    /// One level deeper (a child of the brother before it) or shallower.
    pub(super) fn bm_shift(&mut self, deeper: bool) -> Vec<Action> {
        let Some(sel) = self.bm_item() else { return Vec::new() };
        if let Some(actions) = self.bm_not_now() {
            return actions;
        }
        let mut items = self.outline.clone();
        let done = if deeper { marks::indent(&mut items, sel) } else { marks::outdent(&mut items, sel) };
        if done { self.bm_send(items, Some(sel)) } else { Vec::new() }
    }

    /// The item leads to the top of the window from now on.
    pub(super) fn bm_here(&mut self) -> Vec<Action> {
        let Some(sel) = self.bm_item() else { return Vec::new() };
        if let Some(actions) = self.bm_not_now() {
            return actions;
        }
        if self.outline.get(sel).is_some_and(|i| i.action) {
            return self.say(t(self.lang, Msg::NoteBmNo).to_string());
        }
        let Some((page, x, y)) = self.view_place() else { return Vec::new() };
        let mut items = self.outline.clone();
        if let Some(item) = items.get_mut(sel) {
            (item.page, item.x, item.y, item.placed) = (Some(page), Some(x), Some(y), true);
        }
        self.bm_send(items, Some(sel))
    }

    /// The keys of the tree when it has the keyboard: F2, Delete, Tab and Shift+Tab. `None`: not one of them.
    pub(super) fn bm_key(&mut self, key: u32, shift: bool) -> Option<Vec<Action>> {
        if !(self.bm.focus && self.bookmarks_shown()) {
            return None;
        }
        match key {
            vk::F2 => Some(self.bm_rename()),
            vk::DELETE => Some(self.bm_delete()),
            vk::TAB => Some(self.bm_shift(!shift)),
            _ => None,
        }
    }

    /// The right button on the tree: the menu of what can be done to the bookmarks.
    pub(super) fn bm_menu(&mut self, index: Option<usize>, x: i32, y: i32) -> Vec<Action> {
        if index.is_some() {
            self.bm.sel = index;
            self.bm.focus = true;
        }
        let allowed = self.bm_allowed();
        let item = self.bm_item();
        let (deeper, shallower) = (item.is_some_and(|i| marks::can_indent(&self.outline, i)), item.is_some_and(|i| marks::can_outdent(&self.outline, i)));
        let plain = item.is_some_and(|i| self.outline.get(i).is_some_and(|x| !x.action));
        let entry = |id: u32, m: Msg, enabled: bool| MenuEntry::Item { id, label: t(self.lang, m).to_string(), checked: false, enabled: allowed && enabled };
        let mut entries = Vec::new();
        if !allowed {
            entries.push(MenuEntry::Item { id: 0, label: t(self.lang, Msg::NoteBmDenied).to_string(), checked: false, enabled: false });
            entries.push(MenuEntry::Separator);
        }
        entries.extend([
            entry(CMD_BM_ADD, Msg::BmAdd, true),
            entry(CMD_BM_RENAME, Msg::BmRename, item.is_some()),
            entry(CMD_BM_DELETE, Msg::BmDelete, item.is_some()),
            MenuEntry::Separator,
            entry(CMD_BM_INDENT, Msg::BmIndent, deeper),
            entry(CMD_BM_OUTDENT, Msg::BmOutdent, shallower),
            MenuEntry::Separator,
            entry(CMD_BM_HERE, Msg::BmHere, plain),
            MenuEntry::Separator,
            entry(CMD_BM_AUTO, Msg::BmAuto, true),
        ]);
        vec![Action::ContextMenu { entries, at: (x, y) }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_is_one_line_of_at_most_the_length_a_title_may_have() {
        assert_eq!(title_of("  Chapter \n one\t two  "), "Chapter one two");
        assert_eq!(title_of("").len(), 0);
        assert_eq!(title_of(&"x".repeat(MAX_TITLE_CHARS + 50)).chars().count(), MAX_TITLE_CHARS);
    }
}

#[cfg(test)]
mod app_tests {
    use super::super::sidebar::tests::{app, pump};
    use super::*;
    use crate::select::{CharPos, Selection};
    use crate::ui::vk;

    /// A PDF of four pages with a heading (24 points) and ten lines of text (10 points) on each; with bookmarks `A` (page 1) with `A1` below it
    /// (page 2), `B` (page 3) and `C` (page 4) when `marked`.
    fn four_pages(marked: bool) -> Vec<u8> {
        let mut objects: Vec<(u32, String)> = vec![
            (1, format!("<< /Type /Catalog /Pages 2 0 R {} >>", if marked { "/Outlines 5 0 R" } else { "" })),
            (2, "<< /Type /Pages /Kids [10 0 R 12 0 R 14 0 R 16 0 R] /Count 4 >>".to_string()),
            (3, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string()),
        ];
        for i in 0..4u32 {
            let mut body = format!("BT /F1 24 Tf 72 700 Td (Heading Page {}) Tj ET\n", i + 1);
            for k in 0..10 {
                body.push_str(&format!("BT /F1 10 Tf 72 {} Td (This is line {k} of the text of the page, which is body text.) Tj ET\n", 650 - 14 * k));
            }
            objects.push((10 + 2 * i, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>", 11 + 2 * i)));
            objects.push((11 + 2 * i, format!("<< /Length {} >>\nstream\n{body}endstream", body.len())));
        }
        if marked {
            objects.push((5, "<< /Type /Outlines /First 20 0 R /Last 23 0 R /Count 4 >>".to_string()));
            objects.push((20, "<< /Title (A) /Parent 5 0 R /Next 22 0 R /First 21 0 R /Last 21 0 R /Count 1 /Dest [10 0 R /XYZ 0 700 0] >>".to_string()));
            objects.push((21, "<< /Title (A1) /Parent 20 0 R /Dest [12 0 R /XYZ 0 700 0] >>".to_string()));
            objects.push((22, "<< /Title (B) /Parent 5 0 R /Prev 20 0 R /Next 23 0 R /Dest [14 0 R /XYZ 0 700 0] >>".to_string()));
            objects.push((23, "<< /Title (C) /Parent 5 0 R /Prev 22 0 R /Dest [16 0 R /XYZ 0 700 0] >>".to_string()));
        }
        classic(&objects)
    }

    fn classic(objects: &[(u32, String)]) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = std::collections::BTreeMap::new();
        for (n, body) in objects {
            offsets.insert(*n, out.len());
            out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
        }
        let size = objects.iter().map(|o| o.0).max().unwrap_or(0) + 1;
        let at = out.len();
        out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        for n in 0..size {
            match offsets.get(&n) {
                Some(o) => out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes()),
                None => out.extend_from_slice(b"0000000000 65535 f \n"),
            }
        }
        out.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{at}\n%%EOF\n").as_bytes());
        out
    }

    fn pages_without_text() -> Vec<u8> {
        classic(&[(1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()), (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()), (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>".to_string())])
    }

    fn opened(name: &str, bytes: &[u8]) -> App {
        let file = std::env::temp_dir().join(format!("qingpdf-bookmarks-ui-{}-{name}.pdf", std::process::id()));
        std::fs::write(&file, bytes).expect("writes");
        let mut app = app();
        app.event(Event::Size { w: 1000, h: 800 });
        app.open_path(&file, "");
        pump(&mut app, "the file to open", |a| a.has_document());
        let _ = std::fs::remove_file(&file);
        app
    }

    /// An app with the four pages, and its bookmarks read.
    fn marked(name: &str) -> App {
        let mut app = opened(name, &four_pages(true));
        pump(&mut app, "the bookmarks", |a| a.outline.len() == 4);
        app
    }

    fn titles(app: &App) -> Vec<(String, u32)> {
        app.outline.iter().map(|i| (i.title.clone(), i.depth)).collect()
    }

    /// Pump until the bookmarks have been changed and read again (nothing is being written and `change` shows in the list).
    fn settled(app: &mut App, what: &str, change: impl Fn(&App) -> bool) {
        pump(app, what, |a| !a.bm.reload && a.edit.asked.is_empty() && change(a));
    }

    fn key(app: &mut App, vk: u32, ctrl: bool, shift: bool) -> Vec<Action> {
        app.event(Event::Key { vk, ctrl, shift })
    }

    /// Select item `i` of the tree the way a click on it does.
    fn click(app: &mut App, i: usize) -> Vec<Action> {
        app.event(Event::OutlineClick(i))
    }

    /// The actions the reader answers with while the engine works, until one is a question (a confirmation).
    fn question(app: &mut App) -> (u32, String) {
        for _ in 0..1500 {
            for a in app.event(Event::Wake) {
                if let Action::Confirm { tag, text, .. } = a {
                    return (tag, text);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("no question was asked");
    }

    fn menu_of(told: Vec<Action>) -> Vec<MenuEntry> {
        told.into_iter().find_map(|a| if let Action::ContextMenu { entries, .. } = a { Some(entries) } else { None }).expect("a menu")
    }

    #[test]
    fn ctrl_b_adds_a_bookmark_after_the_selected_one_with_the_heading_of_the_page_as_its_title() {
        let mut app = marked("add");
        // (The click on B takes the window to its page, 3: that is where the new one leads, and what its heading is.)
        click(&mut app, 2);
        pump(&mut app, "the move", |a| a.current_page() == 2);
        key(&mut app, vk::B, true, false);
        settled(&mut app, "the new bookmark", |a| a.outline.len() == 5);
        assert_eq!(titles(&app), [("A".to_string(), 0), ("A1".to_string(), 1), ("B".to_string(), 0), ("Heading Page 3".to_string(), 0), ("C".to_string(), 0)]);
        // It leads to the top of the window, is selected, and the file is changed.
        assert_eq!((app.outline[3].page, app.bm.sel), (Some(2), Some(3)));
        assert!(app.edit.dirty && app.edit.can_undo);
        // Undo takes it away and the tree is read again.
        app.event(Event::Command(CMD_UNDO));
        settled(&mut app, "the undo", |a| a.outline.len() == 4);
        assert_eq!(titles(&app).iter().map(|t| t.0.as_str()).collect::<Vec<_>>(), ["A", "A1", "B", "C"]);
        assert!(!app.edit.dirty);
    }

    #[test]
    fn the_text_that_is_selected_is_the_title_and_page_n_is_the_title_when_nothing_is_there() {
        let mut app = marked("title");
        app.selection = Some(Selection { anchor: CharPos { page: 0, index: 0 }, head: CharPos { page: 0, index: 6 } });
        key(&mut app, vk::B, true, false);
        settled(&mut app, "the new bookmark", |a| a.outline.len() == 5);
        // (No bookmark was selected: it is at the end.)
        assert_eq!(app.outline[4].title, "Heading");
        // A page with no text: "Page N".
        let mut bare = opened("bare", &pages_without_text());
        key(&mut bare, vk::B, true, false);
        settled(&mut bare, "the new bookmark", |a| a.outline.len() == 1);
        assert_eq!(bare.outline[0].title, "Page 1");
        // The first bookmark of a file that had none opens the bookmarks in the sidebar.
        assert!(bare.view_x > 0 && bare.active_tab() == Tab::Bookmarks);
    }

    #[test]
    fn a_title_typed_over_is_the_title_and_an_empty_one_or_the_same_changes_nothing() {
        let mut app = marked("rename");
        app.event(Event::OutlineRename { index: 2, text: "  新标题 \t renamed  ".to_string() });
        settled(&mut app, "the new title", |a| a.outline.get(2).is_some_and(|i| i.title == "新标题 renamed"));
        assert!(app.edit.dirty);
        let before = app.edit.asked.len();
        let told = app.event(Event::OutlineRename { index: 2, text: "   ".to_string() });
        assert!(app.edit.asked.len() == before && told.iter().any(|a| matches!(a, Action::Outline { .. })), "the tree is put as the list has it");
        app.event(Event::OutlineRename { index: 2, text: "新标题 renamed".to_string() });
        assert!(app.edit.asked.is_empty());
    }

    #[test]
    fn delete_takes_a_bookmark_and_what_is_below_it_and_undo_brings_them_back() {
        let mut app = marked("delete");
        click(&mut app, 0);
        key(&mut app, vk::DELETE, false, false);
        settled(&mut app, "the delete", |a| a.outline.len() == 2);
        assert_eq!(titles(&app), [("B".to_string(), 0), ("C".to_string(), 0)]);
        assert_eq!(app.bm.sel, Some(0));
        app.event(Event::Command(CMD_UNDO));
        settled(&mut app, "the undo", |a| a.outline.len() == 4);
        app.event(Event::Command(CMD_REDO));
        settled(&mut app, "the redo", |a| a.outline.len() == 2);
        // Without a bookmark selected nothing is taken out.
        let mut other = marked("delete-none");
        key(&mut other, vk::DELETE, false, false);
        assert!(other.edit.asked.is_empty());
    }

    #[test]
    fn tab_and_shift_tab_make_a_bookmark_a_level_deeper_or_shallower() {
        let mut app = marked("indent");
        click(&mut app, 3);
        key(&mut app, vk::TAB, false, false);
        settled(&mut app, "the indent", |a| a.outline.get(3).is_some_and(|i| i.depth == 1));
        assert!(app.outline[2].open, "the brother it went under shows it");
        key(&mut app, vk::TAB, false, true);
        settled(&mut app, "the outdent", |a| a.outline.get(3).is_some_and(|i| i.depth == 0));
        // The first has no brother before it.
        click(&mut app, 0);
        let before = app.edit.asked.len();
        key(&mut app, vk::TAB, false, false);
        assert_eq!(app.edit.asked.len(), before);
    }

    #[test]
    fn a_drop_moves_a_branch_before_after_or_inside_another_and_never_into_itself() {
        let mut app = marked("drop");
        app.event(Event::OutlineDrop { from: 3, to: 0, place: marks::Place::Before });
        settled(&mut app, "the move", |a| a.outline.first().is_some_and(|i| i.title == "C"));
        assert_eq!(titles(&app).iter().map(|t| t.0.as_str()).collect::<Vec<_>>(), ["C", "A", "A1", "B"]);
        // The branch of A goes inside B.
        app.event(Event::OutlineDrop { from: 1, to: 3, place: marks::Place::Inside });
        settled(&mut app, "the move inside", |a| a.outline.len() == 4 && a.outline.get(2).is_some_and(|i| i.title == "A" && i.depth == 1));
        assert_eq!(titles(&app), [("C".to_string(), 0), ("B".to_string(), 0), ("A".to_string(), 1), ("A1".to_string(), 2)]);
        assert!(app.outline[1].open);
        // Into its own branch: refused, said, nothing asked.
        app.event(Event::OutlineDrop { from: 2, to: 3, place: marks::Place::Inside });
        assert!(app.edit.asked.is_empty());
        assert_eq!(app.note.as_deref(), Some(t(app.lang, Msg::NoteBmNo)));
    }

    #[test]
    fn a_bookmark_is_set_to_the_place_the_window_is_at() {
        let mut app = marked("here");
        click(&mut app, 0);
        app.go_to_page(2);
        pump(&mut app, "the scroll", |a| a.current_page() == 2);
        app.event(Event::Command(CMD_BM_HERE));
        settled(&mut app, "the new place", |a| a.outline.first().is_some_and(|i| i.page == Some(2)));
        assert_eq!(app.outline[0].title, "A");
        assert!(app.outline[0].y.is_some());
    }

    #[test]
    fn opening_and_closing_an_item_is_kept_with_the_next_change() {
        let mut app = marked("open");
        assert!(app.outline[0].open);
        app.event(Event::OutlineExpand { index: 0, open: false });
        assert!(app.edit.asked.is_empty() && !app.outline[0].open, "the list says so; nothing is written for it alone");
        app.event(Event::OutlineRename { index: 2, text: "B2".to_string() });
        settled(&mut app, "the change", |a| a.outline.get(2).is_some_and(|i| i.title == "B2"));
        assert!(!app.outline[0].open, "it is closed in the file now");
    }

    #[test]
    fn the_right_button_opens_a_menu_with_what_can_be_done_to_the_bookmark_and_its_reason_when_nothing_can() {
        let mut app = marked("menu");
        let entries = menu_of(app.event(Event::OutlineMenu { index: Some(1), x: 20, y: 60 }));
        let items: Vec<(u32, bool)> = entries.iter().filter_map(|e| if let MenuEntry::Item { id, enabled, .. } = e { Some((*id, *enabled)) } else { None }).collect();
        // A1 is the only child of A: it can be made shallower, and cannot be made deeper (no brother before it).
        assert_eq!(items, [(CMD_BM_ADD, true), (CMD_BM_RENAME, true), (CMD_BM_DELETE, true), (CMD_BM_INDENT, false), (CMD_BM_OUTDENT, true), (CMD_BM_HERE, true), (CMD_BM_AUTO, true)]);
        assert_eq!(app.bm.sel, Some(1));
        // A file that does not allow it: every item greyed, and the reason first.
        let mut denied = marked("menu-denied");
        denied.rights.assemble = false;
        denied.rights.modify = false;
        let entries = menu_of(denied.event(Event::OutlineMenu { index: None, x: 20, y: 60 }));
        assert!(matches!(entries.first(), Some(MenuEntry::Item { enabled: false, label, .. }) if label == t(Lang::En, Msg::NoteBmDenied)));
        assert!(entries.iter().all(|e| !matches!(e, MenuEntry::Item { enabled: true, .. })));
        // ... and the keys say why.
        key(&mut denied, vk::B, true, false);
        assert!(denied.edit.asked.is_empty());
        assert_eq!(denied.note.as_deref(), Some(t(Lang::En, Msg::NoteBmDenied)));
        let listed: Vec<(u32, bool)> = denied.menus().iter().flat_map(|m| m.entries.iter()).filter_map(|e| if let MenuEntry::Item { id, enabled, .. } = e { Some((*id, *enabled)) } else { None }).collect();
        assert!(listed.contains(&(CMD_BM_ADD, false)) && listed.contains(&(CMD_BM_AUTO, false)));
    }

    #[test]
    fn a_second_click_on_the_selected_bookmark_a_moment_after_the_first_types_over_its_title() {
        let mut app = marked("slow");
        click(&mut app, 1);
        // Two clicks close together are a double click (the tree opens or closes the item), not this.
        let quick = click(&mut app, 1);
        assert!(!quick.iter().any(|a| matches!(a, Action::OutlineRename(_))));
        std::thread::sleep(std::time::Duration::from_millis(750));
        let slow = click(&mut app, 1);
        assert!(slow.iter().any(|a| matches!(a, Action::OutlineRename(1))), "{slow:?}");
        // F2 does it at once.
        let f2 = key(&mut app, vk::F2, false, false);
        assert!(f2.iter().any(|a| matches!(a, Action::OutlineRename(1))));
        // Not in a file that does not allow it.
        app.rights.assemble = false;
        app.rights.modify = false;
        assert!(!key(&mut app, vk::F2, false, false).iter().any(|a| matches!(a, Action::OutlineRename(_))));
    }

    /// The file with its cross-reference table cut out (it is read by scanning, and written afresh by the first change).
    fn damaged(bytes: &[u8]) -> Vec<u8> {
        let find = |needle: &[u8]| bytes.windows(needle.len()).rposition(|w| w == needle).expect("is in the file");
        let (table, trailer) = (find(b"\nxref\n") + 1, find(b"trailer"));
        let mut out = bytes[..table].to_vec();
        out.extend_from_slice(&bytes[trailer..]);
        out
    }

    fn numbers(app: &App) -> Vec<u32> {
        app.outline.iter().map(|i| i.id).collect()
    }

    #[test]
    fn a_bookmark_with_no_title_is_shown_as_the_word_and_enter_in_its_box_leaves_it_as_it_was() {
        // (The title of B is a blank, of the same length, so that the table of the file is right.)
        let text = String::from_utf8(four_pages(true)).expect("is text").replace("/Title (B)", "/Title ( )");
        let mut app = opened("untitled", text.as_bytes());
        pump(&mut app, "the bookmarks", |a| a.outline.len() == 4);
        assert_eq!(app.outline[2].title.trim(), "");
        let shown = t(app.lang, Msg::Untitled);
        assert_eq!(app.outline_nodes()[2].title, shown, "the tree shows the word");
        // Enter in the box without typing: the box held the word, which is what it was, so nothing is asked and no title is made of it.
        let before = app.edit.asked.len();
        let told = app.event(Event::OutlineRename { index: 2, text: shown.to_string() });
        assert!(app.edit.asked.len() == before && !app.edit.dirty, "nothing is changed");
        assert!(told.iter().any(|a| matches!(a, Action::Outline { .. })), "the tree is put as the list has it");
        assert_eq!(app.outline[2].title.trim(), "");
        // A title that is typed is the title.
        app.event(Event::OutlineRename { index: 2, text: "B2".to_string() });
        settled(&mut app, "the new title", |a| a.outline.get(2).is_some_and(|i| i.title == "B2"));
        // And a bookmark that has a title keeps what is typed even when it is the word.
        app.event(Event::OutlineRename { index: 3, text: shown.to_string() });
        settled(&mut app, "the word as a title", |a| a.outline.get(3).is_some_and(|i| i.title == shown));
    }

    #[test]
    fn the_first_bookmark_change_in_a_damaged_file_is_made_though_the_file_is_numbered_anew() {
        let mut app = opened("damaged-rename", &damaged(&four_pages(true)));
        pump(&mut app, "the bookmarks", |a| a.outline.len() == 4);
        let was = numbers(&app);
        app.event(Event::OutlineRename { index: 2, text: "B2".to_string() });
        settled(&mut app, "the new title", |a| a.outline.get(2).is_some_and(|i| i.title == "B2"));
        assert!(app.edit.dirty);
        assert_ne!(numbers(&app), was, "the file was not numbered anew, so the test shows nothing");
        assert_eq!(titles(&app).iter().map(|t| t.0.as_str()).collect::<Vec<_>>(), ["A", "A1", "B2", "C"]);
        // The next one too.
        app.event(Event::OutlineRename { index: 0, text: "A2".to_string() });
        settled(&mut app, "the second title", |a| a.outline.first().is_some_and(|i| i.title == "A2"));
    }

    #[test]
    fn a_damaged_file_written_afresh_by_a_change_to_the_pages_has_its_bookmarks_read_again() {
        let mut app = opened("damaged-pages", &damaged(&four_pages(true)));
        pump(&mut app, "the bookmarks", |a| a.outline.len() == 4);
        let was = numbers(&app);
        app.rotate_pages(90);
        pump(&mut app, "the change and the bookmarks read again", |a| a.edit.dirty && !a.bm.reload && a.edit.asked.is_empty() && numbers(a) != was);
        assert_eq!(titles(&app).iter().map(|t| t.0.as_str()).collect::<Vec<_>>(), ["A", "A1", "B", "C"]);
        // The list the window has is the file's: a change to a bookmark is made.
        app.event(Event::OutlineRename { index: 1, text: "A1b".to_string() });
        settled(&mut app, "the new title", |a| a.outline.get(1).is_some_and(|i| i.title == "A1b"));
    }

    #[test]
    fn a_damaged_file_written_afresh_by_a_mark_on_the_text_has_its_bookmarks_read_again() {
        // (A change that has nothing to do with the bookmarks, which the pages' own reloading does not cover.)
        let mut app = opened("damaged-mark", &damaged(&four_pages(true)));
        pump(&mut app, "the bookmarks", |a| a.outline.len() == 4);
        app.ask_for_boxes(0);
        pump(&mut app, "the characters", |a| a.boxes.contains_key(&0));
        let was = numbers(&app);
        app.selection = Some(Selection { anchor: CharPos { page: 0, index: 0 }, head: CharPos { page: 0, index: 6 } });
        app.highlight_selection();
        pump(&mut app, "the mark and the bookmarks read again", |a| a.edit.dirty && !a.bm.reload && a.edit.asked.is_empty() && numbers(a) != was);
        app.event(Event::OutlineRename { index: 3, text: "C2".to_string() });
        settled(&mut app, "the new title", |a| a.outline.get(3).is_some_and(|i| i.title == "C2"));
    }

    #[test]
    fn after_save_as_the_bookmarks_are_read_again_and_the_next_change_is_made() {
        let mut app = marked("save-as");
        let was = numbers(&app);
        let copy = std::env::temp_dir().join(format!("qingpdf-bookmarks-ui-{}-save-as-copy.pdf", std::process::id()));
        let _ = std::fs::remove_file(&copy);
        app.event(Event::SavePicked(Some(copy.clone())));
        pump(&mut app, "the save and the bookmarks read again", |a| a.edit.saving.is_none() && !a.bm.reload && numbers(a) != was);
        assert_eq!(titles(&app).iter().map(|t| t.0.as_str()).collect::<Vec<_>>(), ["A", "A1", "B", "C"]);
        app.event(Event::OutlineRename { index: 2, text: "B2".to_string() });
        settled(&mut app, "the new title", |a| a.outline.get(2).is_some_and(|i| i.title == "B2"));
        assert!(app.edit.dirty);
        let _ = std::fs::remove_file(&copy);
    }

    #[test]
    fn the_headings_make_the_bookmarks_after_the_person_says_yes() {
        let mut app = opened("auto", &four_pages(false));
        assert!(app.outline.is_empty());
        app.event(Event::Command(CMD_BM_AUTO));
        let (tag, text) = question(&mut app);
        assert_eq!(tag, TAG_BM_AUTO);
        assert!(text.contains("4 headings"), "{text}");
        // No: nothing is changed.
        app.event(Event::Confirmed { tag, yes: false });
        assert!(app.edit.asked.is_empty() && app.outline.is_empty());
        // Yes.
        app.event(Event::Command(CMD_BM_AUTO));
        let (tag, _) = question(&mut app);
        app.event(Event::Confirmed { tag, yes: true });
        settled(&mut app, "the bookmarks", |a| a.outline.len() == 4);
        assert_eq!(titles(&app).iter().map(|t| t.0.as_str()).collect::<Vec<_>>(), ["Heading Page 1", "Heading Page 2", "Heading Page 3", "Heading Page 4"]);
        assert_eq!(app.outline.iter().map(|i| i.page).collect::<Vec<_>>(), [Some(0), Some(1), Some(2), Some(3)]);
        assert!(app.view_x > 0 && app.active_tab() == Tab::Bookmarks);
        // A file that has bookmarks is told they will be replaced.
        let mut app = marked("auto-again");
        app.event(Event::Command(CMD_BM_AUTO));
        let (_, text) = question(&mut app);
        assert!(text.contains("4 bookmarks the file has are replaced"), "{text}");
    }
}
