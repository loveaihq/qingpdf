//! The pages of the document, in the list of thumbnails (2026-10-15): choosing thumbnails (click, Ctrl+click, Shift+click, Ctrl+A),
//! turning, deleting and dragging them to other places, pages put in from a file, pages extracted to a file of their own. The engine does
//! all the work and keeps the history (`docs/view-api.md`, "Pages"); what is here is what the window asks and what it does when the
//! engine says the pages are not the pages they were: everything it keeps by page is let go of, and the pages are laid out again.
//! The plain parts (which pages are chosen, where a drop goes, how fast the list scrolls) are in `thumbs.rs`.

use std::path::PathBuf;

use qingpdf_core::view::{Engine, PageInfo, SaveStatus};

use super::editing::Asked;
use super::sidebar::TAB_H;
use super::*;
use crate::layout::Layout;
use crate::thumbs::{self, Chosen};
use crate::ui::MenuEntry;

/// How far (pixels at 96 dpi) the pointer goes with the button down on a thumbnail before it is a drag, how near to the edge of the list a
/// drag starts to scroll it, and how fast it scrolls there at most (pixels of a tick, at 96 dpi).
const DRAG_START: i64 = 5;
const EDGE: i64 = 28;
const EDGE_SPEED: i64 = 18;
/// The line that shows where a drop goes.
const DROP_COLOR: u32 = 0xFF7A00;
const DROP_H: i64 = 4;

/// A press on a thumbnail that is, or may become, a drag of the chosen pages.
pub(super) struct Drag {
    /// The row it went down on, and where (window pixels).
    row: usize,
    from: (i64, i64),
    /// It went down on one of several chosen pages without a key: if it comes up without a drag it is a click on that page.
    among: bool,
    /// The pointer has gone far enough to drag, and the place between rows it is over (none: not over the list).
    pub(super) moved: bool,
    pub(super) gap: Option<usize>,
    /// The pages may be moved (the file allows it).
    movable: bool,
}

/// What is chosen once the engine says a change is done.
enum Then {
    /// These pages (they were turned).
    Same(Vec<u32>),
    /// These pages, where they were moved to: the window goes to the first.
    Moved(Vec<u32>),
    /// The pages that came in: the place and how many pages there were.
    Inserted { at: usize, before: usize },
    Nothing,
}

pub(super) struct PageWork {
    pub(super) chosen: Chosen,
    pub(super) drag: Option<Drag>,
    /// What the engine was asked and has not yet answered (request, and what to choose when it has).
    then: Vec<(u64, Then)>,
    /// The place the pages of a file go while the box that asks for the file is open; while the file is being put in (and a password is
    /// asked for): the place and the file.
    insert_at: Option<usize>,
    inserting: Option<(usize, PathBuf)>,
    /// The pages to extract while the box that asks where to write them is open, and the request that is writing them.
    extract: Option<Vec<u32>>,
    extracting: Option<u64>,
}

impl PageWork {
    pub(super) fn new() -> PageWork {
        PageWork { chosen: Chosen::default(), drag: None, then: Vec::new(), insert_at: None, inserting: None, extract: None, extracting: None }
    }

    /// The document is gone.
    pub(super) fn reset(&mut self) {
        *self = PageWork::new();
    }

    /// Pages are being changed, put in or written to a file, and the engine has not answered yet.
    pub(super) fn busy(&self) -> bool {
        !self.then.is_empty() || self.inserting.is_some() || self.extracting.is_some()
    }

    /// The box that asks for a file of pages is open.
    pub(super) fn insert_pending(&self) -> bool {
        self.insert_at.is_some()
    }
}

/// Is this menu command one of the page commands (the list of thumbnails keeps the keyboard for them)?
pub(super) fn is_page_command(cmd: u32) -> bool {
    (CMD_PG_LEFT..=CMD_PG_EXTRACT).contains(&cmd)
}

impl App {
    /// The pages a change is for: the ones chosen in the list, else the page in view.
    pub(super) fn target_pages(&self) -> Vec<u32> {
        if !self.pg.chosen.is_empty() {
            return self.pg.chosen.pages();
        }
        if self.doc.is_some() { vec![self.thumb_current() as u32] } else { Vec::new() }
    }

    /// Why pages cannot be changed now, as the actions that say so; `None` when they can.
    fn pages_not_now(&mut self) -> Option<Vec<Action>> {
        if self.doc.is_none() {
            return Some(Vec::new());
        }
        if !self.rights.assemble {
            return Some(self.say(t(self.lang, Msg::NotePagesDenied).to_string()));
        }
        if self.ocr.is_some() || self.print.is_some() {
            return Some(self.say(t(self.lang, Msg::NoteBusy).to_string()));
        }
        // It waits for the answer (a few milliseconds) by being ignored.
        self.pages_busy().then(Vec::new)
    }

    /// Is a change to the pages (or an undo or redo, which may be one) not answered yet? The pages the person sees are not the pages it will
    /// leave: a request that names pages by their numbers, a second change or an annotation, would name the wrong ones.
    pub(super) fn pages_busy(&self) -> bool {
        self.edit.asked.values().any(|a| matches!(a, Asked::Undo | Asked::Redo) || a.pages_note().is_some())
    }

    /// What is said instead of sending an edit that names pages by their numbers while [`App::pages_busy`]; `None` when it can be sent.
    /// (Nothing is lost but the doing of it: the person does it again.)
    pub(super) fn edit_wait(&mut self) -> Option<Vec<Action>> {
        self.pages_busy().then(|| self.say(t(self.lang, Msg::NotePagesBusy).to_string()))
    }

    /// Ask the engine for a change to the pages; `then` says what is chosen when it is done.
    fn ask_pages(&mut self, what: Asked, then: Then, call: impl FnOnce(&Engine, u64, u64)) {
        let id = self.new_id();
        call(&self.engine, self.doc_id, id);
        self.edit.asked.insert(id, what);
        self.pg.then.push((id, then));
    }

    pub(super) fn rotate_pages(&mut self, degrees: i32) -> Vec<Action> {
        if let Some(actions) = self.pages_not_now() {
            return actions;
        }
        let pages = self.target_pages();
        self.ask_pages(Asked::Rotate, Then::Same(pages.clone()), |e, doc, id| e.rotate_pages(doc, id, pages, degrees));
        Vec::new()
    }

    pub(super) fn delete_pages(&mut self) -> Vec<Action> {
        if let Some(actions) = self.pages_not_now() {
            return actions;
        }
        let pages = self.target_pages();
        if pages.is_empty() || pages.len() >= self.page_count() {
            return self.say(t(self.lang, Msg::NoteLastPage).to_string());
        }
        self.ask_pages(Asked::Delete, Then::Nothing, |e, doc, id| e.delete_pages(doc, id, pages));
        Vec::new()
    }

    /// Put the chosen pages in front of row `gap`.
    fn move_chosen(&mut self, gap: usize) -> Vec<Action> {
        if let Some(actions) = self.pages_not_now() {
            return actions;
        }
        let pages = self.pg.chosen.pages();
        let places = thumbs::places_after_move(self.page_count(), &pages, gap);
        self.ask_pages(Asked::Move, Then::Moved(places), |e, doc, id| e.move_pages(doc, id, pages, gap as u32));
        Vec::new()
    }

    // --- pages from a file ---------------------------------------------------------------------------------------

    /// The menu's "insert pages from a file before / after": ask which file.
    pub(super) fn ask_insert_file(&mut self, after: bool) -> Vec<Action> {
        if let Some(actions) = self.pages_not_now() {
            return actions;
        }
        let pages = self.target_pages();
        let at = if after { pages.last().map_or(0, |&p| p as usize + 1) } else { pages.first().map_or(0, |&p| p as usize) };
        self.pg.insert_at = Some(at.min(self.page_count()));
        let filters = vec![(t(self.lang, Msg::FilterPdf).to_string(), "*.pdf".to_string())];
        vec![Action::PickFile { title: t(self.lang, Msg::PgInsertTitle).to_string(), filters }]
    }

    /// The box that asks for the file of pages was answered. `None`: it was not asked for pages.
    pub(super) fn insert_picked(&mut self, path: Option<PathBuf>) -> Option<Vec<Action>> {
        let at = self.pg.insert_at.take()?;
        let path = std::path::absolute(path.as_deref()?).ok()?;
        self.pg.inserting = Some((at, path));
        Some(self.send_insert(""))
    }

    fn send_insert(&mut self, password: &str) -> Vec<Action> {
        if self.doc.is_none() {
            return Vec::new();
        }
        let Some((at, path)) = self.pg.inserting.clone() else { return Vec::new() };
        let before = self.page_count();
        let text = path.to_string_lossy().into_owned();
        self.ask_pages(Asked::Insert, Then::Inserted { at, before }, |e, doc, id| e.insert_pages(doc, id, at as u32, &text, password));
        Vec::new()
    }

    /// The file the pages come from needs a password (or the one given is wrong): ask for it.
    pub(super) fn ask_source_password(&mut self, wrong: bool) -> Vec<Action> {
        let lang = self.lang;
        vec![Action::AskText {
            tag: TAG_SRC_PASSWORD,
            title: t(lang, Msg::PasswordTitle).to_string(),
            prompt: t(lang, if wrong { Msg::PasswordWrongPrompt } else { Msg::PasswordPrompt }).to_string(),
            secret: true,
            initial: String::new(),
            ok: t(lang, Msg::Ok).to_string(),
            cancel: t(lang, Msg::Cancel).to_string(),
        }]
    }

    pub(super) fn source_password_answered(&mut self, text: Option<String>) -> Vec<Action> {
        let Some(mut password) = text else {
            self.pg.inserting = None;
            return Vec::new();
        };
        let actions = self.send_insert(&password);
        // The password has gone to the engine; overwrite our copy.
        let mut bytes = std::mem::take(&mut password).into_bytes();
        bytes.fill(0);
        std::hint::black_box(&bytes);
        actions
    }

    // --- pages to a file of their own ------------------------------------------------------------------------------

    pub(super) fn ask_extract(&mut self) -> Vec<Action> {
        let Some(path) = self.path.clone().filter(|_| self.doc.is_some()) else { return Vec::new() };
        if !self.rights.copy {
            return self.say(t(self.lang, Msg::NoteCopyDenied).to_string());
        }
        self.pg.extract = Some(self.target_pages());
        let stem = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let filters = vec![(t(self.lang, Msg::FilterPdf).to_string(), "*.pdf".to_string())];
        vec![Action::PickSavePath { title: t(self.lang, Msg::PgExtractTitle).to_string(), filters, name: format!("{stem}-extract.pdf") }]
    }

    /// The box that asks where to write the pages was answered. `None`: it was not asked for pages.
    pub(super) fn extract_picked(&mut self, path: Option<PathBuf>) -> Option<Vec<Action>> {
        let pages = self.pg.extract.take()?;
        let Some(path) = path.map(|p| std::path::absolute(&p).unwrap_or(p)) else { return Some(Vec::new()) };
        // The file that is open is not written over by this: pages go to a file of their own.
        let same = self.path.as_ref().is_some_and(|open| open.as_os_str().eq_ignore_ascii_case(path.as_os_str()));
        if same || self.doc.is_none() {
            return Some(self.say(t(self.lang, Msg::NoteExtractSame).to_string()));
        }
        let id = self.new_id();
        self.engine.extract_pages(self.doc_id, id, pages, &path.to_string_lossy());
        self.pg.extracting = Some(id);
        Some(Vec::new())
    }

    /// The engine has written the pages (or not). `None`: that request was not the extraction.
    pub(super) fn on_extracted(&mut self, id: u64, status: SaveStatus, message: &str, pages: u32) -> Option<Vec<Action>> {
        if self.pg.extracting != Some(id) {
            return None;
        }
        self.pg.extracting = None;
        let lang = self.lang;
        Some(match status {
            SaveStatus::Done => self.say(t(lang, Msg::NoteExtracted).replace("{n}", &pages.to_string())),
            SaveStatus::Failed => vec![Action::Message { title: t(lang, Msg::Error).to_string(), text: format!("{}\n{message}", t(lang, Msg::NoteExtractFailed)) }],
        })
    }

    // --- the mouse -------------------------------------------------------------------------------------------------------------

    /// The button went down on the thumbnail of `page` (the pointer at window place `x`, `y`).
    pub(super) fn thumb_press(&mut self, page: usize, ctrl: bool, shift: bool, x: i64, y: i64) -> Vec<Action> {
        if ctrl {
            self.pg.chosen.toggle(page);
            return vec![Action::Invalidate];
        }
        if shift {
            self.pg.chosen.up_to(page);
            return vec![Action::Invalidate];
        }
        // A press on one of several chosen pages keeps them all (they may be dragged); a click that does not drag is a click on that page.
        let among = self.pg.chosen.contains(page) && self.pg.chosen.len() > 1;
        self.pg.drag = Some(Drag { row: page, from: (x, y), among, moved: false, gap: None, movable: self.rights.assemble });
        if among {
            return vec![Action::Invalidate];
        }
        self.pg.chosen.only(page);
        self.go_to_thumb(page)
    }

    /// Where a drop would go with the pointer at (`x`, `y`): the place between rows, if the pointer is over the list.
    fn drop_gap(&self, x: i64, y: i64) -> Option<usize> {
        if x < 0 || x >= self.list_w() {
            return None;
        }
        let tabs = self.px(TAB_H);
        Some(thumbs::gap_at(&self.thumbs.strip, self.thumbs.scroll + (y - tabs).max(0).min((self.view_h - tabs).max(0))))
    }

    /// The pointer moved with the button down on a thumbnail. `None`: no such press.
    pub(super) fn thumb_drag_move(&mut self, x: i64, y: i64) -> Option<Vec<Action>> {
        let threshold = self.px(DRAG_START);
        let gap = self.drop_gap(x, y);
        let drag = self.pg.drag.as_mut()?;
        let mut actions = Vec::new();
        if !drag.moved {
            if !drag.movable || (x - drag.from.0).abs().max((y - drag.from.1).abs()) < threshold {
                return Some(actions);
            }
            drag.moved = true;
            actions.push(Action::SetTimer { id: TIMER_THUMB_DRAG, ms: DRAG_MS });
        }
        drag.gap = gap;
        actions.push(Action::Invalidate);
        Some(actions)
    }

    /// A tick of the timer while a drag is on: the list scrolls when the pointer is held at its edge.
    pub(super) fn thumb_drag_tick(&mut self) -> Vec<Action> {
        if !self.pg.drag.as_ref().is_some_and(|d| d.moved) {
            return vec![Action::KillTimer(TIMER_THUMB_DRAG)];
        }
        let (tabs, list_h) = (self.px(TAB_H), self.view_h - self.px(TAB_H));
        let delta = thumbs::edge_scroll(self.mouse.1, tabs, tabs + list_h, self.px(EDGE), self.px(EDGE_SPEED));
        if delta == 0 {
            return Vec::new();
        }
        self.thumbs.scroll += delta;
        self.thumbs.clamp(list_h.max(0));
        let gap = self.drop_gap(self.mouse.0, self.mouse.1);
        if let Some(d) = self.pg.drag.as_mut() {
            d.gap = gap;
        }
        self.schedule_thumbs();
        vec![Action::Invalidate]
    }

    /// The button came up. `None`: no press on a thumbnail was going.
    pub(super) fn thumb_drop(&mut self) -> Option<Vec<Action>> {
        let drag = self.pg.drag.take()?;
        let mut actions = vec![Action::KillTimer(TIMER_THUMB_DRAG), Action::Invalidate];
        if drag.moved {
            if let Some(gap) = drag.gap.filter(|_| drag.movable)
                && !thumbs::leaves_in_place(&self.pg.chosen.pages(), gap)
            {
                actions.extend(self.move_chosen(gap));
            }
        } else if drag.among {
            self.pg.chosen.only(drag.row);
            actions.extend(self.go_to_thumb(drag.row));
        }
        Some(actions)
    }

    /// Esc, or the window lost the pointer: a drag in progress is given up. `None`: there was none.
    pub(super) fn cancel_thumb_drag(&mut self) -> Option<Vec<Action>> {
        self.pg.drag.take().map(|_| vec![Action::KillTimer(TIMER_THUMB_DRAG), Action::Invalidate])
    }

    /// The right button on the list of thumbnails: the page under it joins the chosen ones if it was not among them, and the menu opens.
    pub(super) fn thumb_menu(&mut self, x: i64, y: i64) -> Vec<Action> {
        let at = self.thumbs.scroll + y - self.px(TAB_H);
        let Some(row) = self.thumbs.strip.row_at(at) else { return Vec::new() };
        if !self.pg.chosen.contains(row) {
            self.pg.chosen.only(row);
        }
        self.thumb_focus = true;
        let (pages, count) = (self.pg.chosen.len(), self.page_count());
        let assemble = self.rights.assemble;
        let item = |id: u32, m: Msg, enabled: bool| MenuEntry::Item { id, label: t(self.lang, m).to_string(), checked: false, enabled };
        let entries = vec![
            item(CMD_PG_LEFT, Msg::PgLeft, assemble),
            item(CMD_PG_RIGHT, Msg::PgRight, assemble),
            item(CMD_PG_DELETE, Msg::PgDelete, assemble && pages < count),
            MenuEntry::Separator,
            item(CMD_PG_INS_BEFORE, Msg::PgInsertBefore, assemble),
            item(CMD_PG_INS_AFTER, Msg::PgInsertAfter, assemble),
            MenuEntry::Separator,
            item(CMD_PG_EXTRACT, Msg::PgExtract, self.rights.copy),
        ];
        vec![Action::ContextMenu { entries, at: (clamp32(x), clamp32(y)) }, Action::Invalidate]
    }

    // --- when the pages have changed -------------------------------------------------------------------------------------

    /// The engine says the list of pages is now `infos` (after a change to the pages, or its undo or redo): what the window keeps by
    /// page is let go of, the pages are laid out again (the place in the document stays on the same page number), the thumbnails are
    /// made again, and the pages the change was about are the ones chosen.
    pub(super) fn pages_changed(&mut self, infos: Vec<PageInfo>, id: u64) -> Vec<Action> {
        let mut actions = Vec::new();
        let stale: Vec<u64> = self
            .asked
            .drain()
            .map(|(i, _)| i)
            .chain(self.links_asked.drain().map(|(i, _)| i))
            .chain(self.boxes_asked.drain().map(|(i, _)| i))
            .chain(self.thumbs.asked.drain().map(|(i, _)| i))
            .chain(self.edit.forget_pages(&mut actions))
            .collect();
        for i in stale {
            self.engine.cancel(i);
        }
        self.in_flight.clear();
        self.cache.clear();
        self.failed.clear();
        self.notices.clear();
        self.links.clear();
        self.boxes.clear();
        self.pending_select = None;
        self.selection = None;
        self.press = None;
        self.ctx_at = None;
        self.thumbs.pages_changed();
        self.thumb_target = None;
        // The annotations are listed by page number: the list is made again.
        self.forget_listed_selection();
        // The bookmarks lead to pages by their number: the engine says where they lead now.
        if !self.outline.is_empty() && self.bench.is_none() {
            actions.extend(self.ask_for_outline());
        }
        // What was found by page: found again, if the find box is open.
        if let Some(s) = self.search.take() {
            self.engine.cancel(s.id);
        }
        let query = self.find_text.clone();
        if self.find_open && !query.trim().is_empty() {
            actions.extend(self.start_search(&query));
        }
        // The pages again, the same place in the document by page number.
        let count = infos.len();
        let (ax, ay) = (self.view_w / 2, self.view_h / 2);
        let (gap, margin, rotation) = (self.px(GAP), self.px(MARGIN), self.rotation);
        if let Some(doc) = self.doc.as_mut() {
            let (page, fraction) = doc.layout.locate(self.scroll_y + ay);
            let scale = doc.layout.scale();
            let mut layout = Layout::new(infos.iter().map(|p| (p.width, p.height)).collect());
            layout.set(scale, rotation, gap, margin);
            self.scroll_y = layout.y_of(page.min(count.saturating_sub(1)), fraction) - ay;
            doc.layout = layout;
        }
        self.relayout(ax, ay, None);
        // What is chosen now (after the view is told: a view that moves of itself lets go of one page chosen).
        self.menus_dirty = true;
        actions.extend(self.after_view_change(false));
        let asked = self.pg.then.iter().position(|(i, _)| *i == id).map(|at| self.pg.then.remove(at).1);
        let (chosen, go_to) = match asked {
            Some(Then::Same(pages)) => (pages, None),
            Some(Then::Moved(pages)) => {
                let first = pages.iter().copied().filter(|&p| (p as usize) < count).min();
                (pages, first.map(|p| p as usize))
            }
            Some(Then::Inserted { at, before }) => {
                let added = count.saturating_sub(before);
                (((at as u32)..((at + added) as u32)).collect(), Some(at))
            }
            Some(Then::Nothing) | None => (Vec::new(), None),
        };
        self.pg.chosen.set(chosen.into_iter().filter(|&p| (p as usize) < count));
        if let Some(page) = go_to {
            actions.extend(self.go_to_thumb(page));
        }
        actions.push(Action::Invalidate);
        actions
    }

    /// Draw the line that shows where a drop goes, over a list whose rows start at `tabs` pixels from the top of the window.
    pub(super) fn draw_drop_mark(&self, p: &mut dyn Painter, width: i64, tabs: i64) {
        let Some(Drag { moved: true, gap: Some(gap), .. }) = &self.pg.drag else { return };
        // Where the drop would change nothing there is no line.
        if thumbs::leaves_in_place(&self.pg.chosen.pages(), *gap) {
            return;
        }
        let y = tabs + thumbs::gap_top(&self.thumbs.strip, *gap) - self.thumbs.scroll;
        let h = self.px(DROP_H);
        let rect = Rect { x: clamp32(self.px(4)), y: clamp32(y - h / 2), w: clamp32((width - self.px(8)).max(1)), h: clamp32(h) };
        p.fill_rect(rect, DROP_COLOR);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::sidebar::tests::{Rec, app, letter, open_pages, pages_pdf, pump};
    use super::*;
    use crate::ui::vk;

    /// Pages that are told apart by their width: page `i` is `100 + 10 i` points wide and 200 tall.
    fn distinct(n: usize) -> Vec<(u32, u32, u32)> {
        (0..n as u32).map(|i| (100 + 10 * i, 200, 0)).collect()
    }

    fn widths(app: &App) -> Vec<i64> {
        let l = &app.doc.as_ref().expect("a document").layout;
        (0..l.len()).map(|i| l.size_pt(i).map_or(-1, |s| s.0.round() as i64)).collect()
    }

    fn sizes(app: &App) -> Vec<(i64, i64)> {
        let l = &app.doc.as_ref().expect("a document").layout;
        (0..l.len()).map(|i| l.size_pt(i).map_or((-1, -1), |s| (s.0.round() as i64, s.1.round() as i64))).collect()
    }

    /// The window place (`y`) of the middle of row `i` of the list.
    fn row_y(app: &App, i: usize) -> i32 {
        let r = app.thumbs.strip.row(i).copied().expect("a row");
        (app.px(TAB_H) + r.top - app.thumbs.scroll + r.height / 2) as i32
    }

    /// Scroll the list so that row `i` is in view.
    fn show(app: &mut App, i: usize) {
        let list_h = app.list_h();
        app.thumbs.scroll = app.thumbs.strip.reveal(i, app.thumbs.scroll, list_h);
    }

    fn click_row(app: &mut App, row: usize, ctrl: bool, shift: bool) {
        show(app, row);
        let y = row_y(app, row);
        app.event(Event::MouseDown { x: 60, y, ctrl, shift, clicks: 1 });
        app.event(Event::MouseUp { x: 60, y });
    }

    fn key(app: &mut App, vk: u32, ctrl: bool) -> Vec<Action> {
        app.event(Event::Key { vk, ctrl, shift: false })
    }

    /// Pump the engine until `until`, giving all the actions the reader answered with.
    fn pump_actions(app: &mut App, what: &str, until: impl Fn(&App) -> bool) -> Vec<Action> {
        let mut all = Vec::new();
        for _ in 0..1500 {
            all.extend(app.event(Event::Wake));
            if until(app) {
                return all;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for {what}");
    }

    fn temp_file(name: &str, bytes: &[u8]) -> PathBuf {
        let path = std::env::temp_dir().join(format!("qingpdf-pages-ui-{}-{name}", std::process::id()));
        std::fs::write(&path, bytes).expect("writes");
        path
    }

    #[test]
    fn clicks_choose_thumbnails_and_the_chosen_ones_are_marked() {
        let mut app = open_pages("choose", &distinct(8), true);
        // A click chooses that page alone and goes to it.
        click_row(&mut app, 2, false, false);
        assert_eq!(app.pg.chosen.pages(), [2]);
        assert_eq!(app.current_page(), 2);
        // Ctrl+click adds and takes away; Shift+click is a range from the page last clicked.
        click_row(&mut app, 4, true, false);
        assert_eq!(app.pg.chosen.pages(), [2, 4]);
        click_row(&mut app, 6, false, true);
        assert_eq!(app.pg.chosen.pages(), [4, 5, 6]);
        click_row(&mut app, 5, true, false);
        assert_eq!(app.pg.chosen.pages(), [4, 6]);
        // Ctrl+A in the list chooses every page.
        key(&mut app, vk::A, true);
        assert_eq!(app.pg.chosen.len(), 8);
        // The rows that are in view and chosen have the mark.
        let list_h = app.list_h();
        let in_view = app.thumbs.strip.rows_in(app.thumbs.scroll, app.thumbs.scroll + list_h).len();
        let mut rec = Rec::default();
        app.paint(&mut rec);
        assert_eq!(rec.fills.iter().filter(|(_, c)| *c == super::super::sidebar::SELECTED_BG).count(), in_view);
        // Nothing chosen: the page in view has the mark, as it always had.
        app.pg.chosen.clear();
        let mut rec = Rec::default();
        app.paint(&mut rec);
        assert_eq!(rec.fills.iter().filter(|(_, c)| *c == super::super::sidebar::SELECTED_BG).count(), 1);
        // One page chosen follows the page in view when the pages are moved some other way.
        click_row(&mut app, 1, false, false);
        app.event(Event::Wheel { delta: -120, ctrl: false, shift: false, x: 600, y: 300 });
        assert!(app.pg.chosen.is_empty(), "one page chosen follows the page in view");
        // Several stay.
        click_row(&mut app, 0, false, false);
        click_row(&mut app, 2, true, false);
        app.event(Event::Wheel { delta: -120, ctrl: false, shift: false, x: 600, y: 300 });
        assert_eq!(app.pg.chosen.pages(), [0, 2]);
    }

    #[test]
    fn turning_the_chosen_pages_lays_them_out_again_asks_for_new_pictures_and_undo_brings_them_back() {
        let mut app = open_pages("turn", &distinct(6), true);
        pump(&mut app, "the pictures", |a| a.thumbs.fresh(0, 0) && a.visible_ready());
        click_row(&mut app, 1, false, false);
        click_row(&mut app, 3, true, false);
        key(&mut app, vk::OEM_6, true);
        pump(&mut app, "the pages turned", |a| a.edit.dirty && widths(a)[1] == 200);
        assert_eq!(sizes(&app)[1], (200, 110));
        assert_eq!(sizes(&app)[3], (200, 130));
        assert_eq!(sizes(&app)[2], (120, 200));
        // The same pages are still chosen, the title says the file is changed, and the list's rows have the new shapes.
        assert_eq!(app.pg.chosen.pages(), [1, 3]);
        assert!(app.title().starts_with("* "));
        let (r1, r2) = (app.thumbs.strip.row(1).copied().expect("a row"), app.thumbs.strip.row(2).copied().expect("a row"));
        assert!(r1.img_w > r1.img_h && r2.img_h > r2.img_w, "the turned page is wide in the list now: {r1:?}");
        // No picture of the page as it was is kept: what is drawn of page 1 is the size of the turned page, in the list and in the window.
        pump(&mut app, "the turned page drawn", |a| a.thumbs.fresh(1, 0));
        let thumb = app.thumbs.cache.get(1).expect("a picture");
        assert_eq!((i64::from(thumb.width), i64::from(thumb.height)), (r1.img_w, r1.img_h), "the picture of the list is of the turned page");
        pump(&mut app, "the pages drawn", |a| a.visible_ready());
        let px = app.doc.as_ref().expect("a document").layout.size_px(1);
        if app.visible().is_some_and(|(a, b)| (a..=b).contains(&1)) {
            let drawn: Vec<_> = app.cache.page(1).filter(|e| !e.preview).map(|e| (e.page_w, e.page_h)).collect();
            assert!(!drawn.is_empty() && drawn.iter().all(|d| *d == px), "{drawn:?} vs {px:?}");
        }
        // Left turns the other way: back to upright, no turn left.
        key(&mut app, vk::OEM_4, true);
        pump(&mut app, "the pages turned back", |a| widths(a)[1] == 110);
        assert_eq!(sizes(&app)[3], (130, 200));
        // Undo twice: the pages as they were.
        app.event(Event::Command(CMD_UNDO));
        pump(&mut app, "the first undo", |a| widths(a)[1] == 200);
        app.event(Event::Command(CMD_UNDO));
        pump(&mut app, "the second undo", |a| widths(a)[1] == 110 && !a.edit.dirty);
        assert!(!app.title().starts_with("* "));
        assert!(app.pg.chosen.is_empty(), "after an undo nothing is chosen");
    }

    #[test]
    fn delete_takes_the_chosen_pages_out_but_never_all_of_them() {
        let mut app = open_pages("delete", &distinct(6), true);
        click_row(&mut app, 2, false, false);
        click_row(&mut app, 3, true, false);
        key(&mut app, vk::DELETE, false);
        pump(&mut app, "the pages to go", |a| a.page_count() == 4);
        assert_eq!(widths(&app), [100, 110, 140, 150]);
        assert!(app.pg.chosen.is_empty() && app.edit.dirty);
        assert!(app.note.as_deref().is_some_and(|n| n.contains("Pages deleted")), "{:?}", app.note);
        // All of them: refused, nothing sent.
        key(&mut app, vk::A, true);
        assert_eq!(app.pg.chosen.len(), 4);
        key(&mut app, vk::DELETE, false);
        assert_eq!(app.note.as_deref(), Some("A document needs at least one page."));
        assert!(app.edit.asked.is_empty());
        // Undo brings them back, in their places.
        app.event(Event::Command(CMD_UNDO));
        pump(&mut app, "the undo", |a| a.page_count() == 6);
        assert_eq!(widths(&app), [100, 110, 120, 130, 140, 150]);
        // With the pages in hand, Delete is the annotation's (nothing is picked: nothing happens).
        app.event(Event::MouseDown { x: 600, y: 300, ctrl: false, shift: false, clicks: 1 });
        app.event(Event::MouseUp { x: 600, y: 300 });
        key(&mut app, vk::DELETE, false);
        assert_eq!(app.page_count(), 6);
    }

    #[test]
    fn dragging_the_chosen_pages_shows_where_they_go_and_drops_them_there() {
        let mut app = open_pages("drag", &distinct(8), true);
        click_row(&mut app, 0, false, false);
        // A press and a tiny move is not a drag.
        app.event(Event::MouseDown { x: 60, y: row_y(&app, 0), ctrl: false, shift: false, clicks: 1 });
        app.event(Event::MouseMove { x: 61, y: row_y(&app, 0) + 1, shift: false });
        assert!(app.pg.drag.as_ref().is_some_and(|d| !d.moved));
        // Moved to the lower half of row 4: the drop goes in front of row 5, and a line is drawn there.
        let y = row_y(&app, 4) + app.thumbs.strip.row(4).map_or(0, |r| r.height / 2) as i32 - 3;
        let actions = app.event(Event::MouseMove { x: 60, y, shift: false });
        assert!(actions.iter().any(|a| matches!(a, Action::SetTimer { id: TIMER_THUMB_DRAG, .. })));
        assert_eq!(app.pg.drag.as_ref().map(|d| (d.moved, d.gap)), Some((true, Some(5))));
        let mut rec = Rec::default();
        app.paint(&mut rec);
        let line = rec.fills.iter().find(|(_, c)| *c == DROP_COLOR).expect("the line that shows the place").0;
        let want = app.px(TAB_H) + thumbs::gap_top(&app.thumbs.strip, 5) - app.thumbs.scroll;
        assert!((i64::from(line.y) + i64::from(line.h) / 2 - want).abs() <= 1, "{line:?} for the gap at {want}");
        // Let go: page 0 goes in front of page 5, i.e. after page 4.
        app.event(Event::MouseUp { x: 60, y });
        pump(&mut app, "the move", |a| widths(a)[0] == 110);
        assert_eq!(widths(&app), [110, 120, 130, 140, 100, 150, 160, 170]);
        assert_eq!(app.pg.chosen.pages(), [4], "the moved page is the one chosen");
        assert_eq!(app.current_page(), 4, "the window shows it");
        assert!(app.pg.drag.is_none() && app.note.as_deref().is_some_and(|n| n.contains("moved")));
        // Undo.
        app.event(Event::Command(CMD_UNDO));
        pump(&mut app, "the undo", |a| widths(a)[0] == 100);
        assert_eq!(widths(&app), [100, 110, 120, 130, 140, 150, 160, 170]);
    }

    #[test]
    fn a_drop_that_changes_nothing_or_is_given_up_asks_the_engine_for_nothing() {
        let mut app = open_pages("nodrop", &distinct(8), true);
        click_row(&mut app, 2, false, false);
        click_row(&mut app, 3, true, false);
        let drag_to = |app: &mut App, from: i32, to: i32, x: i32| {
            app.event(Event::MouseDown { x: 60, y: from, ctrl: false, shift: false, clicks: 1 });
            app.event(Event::MouseMove { x, y: to, shift: false });
        };
        // In front of itself.
        let (a, b, far) = (row_y(&app, 2), row_y(&app, 3), row_y(&app, 6));
        drag_to(&mut app, a, b + 10, 60);
        app.event(Event::MouseUp { x: 60, y: b + 10 });
        assert!(app.edit.asked.is_empty() && app.pg.chosen.pages() == [2, 3]);
        // Esc gives it up: nothing is moved, nothing is left of the drag.
        drag_to(&mut app, a, far, 60);
        assert!(app.pg.drag.as_ref().is_some_and(|d| d.moved));
        let esc = key(&mut app, vk::ESCAPE, false);
        assert!(esc.iter().any(|a| matches!(a, Action::KillTimer(TIMER_THUMB_DRAG))) && app.pg.drag.is_none());
        app.event(Event::MouseUp { x: 60, y: far });
        assert!(app.edit.asked.is_empty());
        // Dropped outside the list (over the pages): nothing.
        drag_to(&mut app, a, far, 600);
        assert_eq!(app.pg.drag.as_ref().and_then(|d| d.gap), None);
        app.event(Event::MouseUp { x: 600, y: far });
        assert!(app.edit.asked.is_empty() && widths(&app) == [100, 110, 120, 130, 140, 150, 160, 170]);
        // The pointer lost (another window took it): given up.
        drag_to(&mut app, a, far, 60);
        app.event(Event::MouseLost);
        assert!(app.pg.drag.is_none());
        // A plain press on one of the two chosen, and no drag: that page alone.
        app.event(Event::MouseDown { x: 60, y: a, ctrl: false, shift: false, clicks: 1 });
        assert_eq!(app.pg.chosen.pages(), [2, 3], "they stay while they may be dragged");
        app.event(Event::MouseUp { x: 60, y: a });
        assert_eq!(app.pg.chosen.pages(), [2]);
    }

    #[test]
    fn a_drag_held_at_the_edge_scrolls_the_list() {
        let mut app = open_pages("scroll", &letter(60), true);
        click_row(&mut app, 0, false, false);
        let bottom = app.view_h as i32 - 3;
        app.event(Event::MouseDown { x: 60, y: row_y(&app, 0), ctrl: false, shift: false, clicks: 1 });
        app.event(Event::MouseMove { x: 60, y: bottom, shift: false });
        let (before, gap) = (app.thumbs.scroll, app.pg.drag.as_ref().and_then(|d| d.gap).expect("over the list"));
        for _ in 0..5 {
            app.event(Event::Timer(TIMER_THUMB_DRAG));
        }
        assert!(app.thumbs.scroll > before, "{} {}", app.thumbs.scroll, before);
        assert!(app.pg.drag.as_ref().and_then(|d| d.gap).is_some_and(|g| g > gap), "the place the drop goes to follows the list");
        // The middle of the list does not scroll it.
        let middle = app.view_h as i32 / 2;
        app.event(Event::MouseMove { x: 60, y: middle, shift: false });
        let held = app.thumbs.scroll;
        app.event(Event::Timer(TIMER_THUMB_DRAG));
        assert_eq!(app.thumbs.scroll, held);
        // At the top it scrolls up.
        app.event(Event::MouseMove { x: 60, y: app.px(TAB_H) as i32 + 2, shift: false });
        app.event(Event::Timer(TIMER_THUMB_DRAG));
        assert!(app.thumbs.scroll < held);
        // The timer stops when the drag is over.
        app.event(Event::MouseUp { x: 60, y: app.px(TAB_H) as i32 + 2 });
        let after = app.event(Event::Timer(TIMER_THUMB_DRAG));
        assert!(after.iter().any(|a| matches!(a, Action::KillTimer(TIMER_THUMB_DRAG))));
    }

    #[test]
    fn the_menu_on_the_list_has_the_page_commands_and_chooses_the_page_it_was_opened_on() {
        let mut app = open_pages("menu", &distinct(5), true);
        let entries_of = |actions: Vec<Action>| {
            actions.into_iter().find_map(|a| if let Action::ContextMenu { entries, .. } = a { Some(entries) } else { None }).expect("a menu")
        };
        let enabled = |entries: &[MenuEntry], cmd: u32| entries.iter().find_map(|e| if let MenuEntry::Item { id, enabled, .. } = e { (*id == cmd).then_some(*enabled) } else { None });
        let menu = entries_of(app.event(Event::ContextMenu { x: 60, y: row_y(&app, 3) }));
        assert_eq!(app.pg.chosen.pages(), [3], "the page under the pointer is chosen");
        for cmd in [CMD_PG_LEFT, CMD_PG_RIGHT, CMD_PG_DELETE, CMD_PG_INS_BEFORE, CMD_PG_INS_AFTER, CMD_PG_EXTRACT] {
            assert_eq!(enabled(&menu, cmd), Some(true), "{cmd}");
        }
        // The words are the language's own.
        let label = |e: &MenuEntry| if let MenuEntry::Item { label, .. } = e { label.clone() } else { String::new() };
        assert!(menu.iter().any(|e| label(e).starts_with("Rotate &left")) && menu.iter().any(|e| label(e).contains("Insert pages from a file")));
        app.lang = Lang::Zh;
        let zh = entries_of(app.event(Event::ContextMenu { x: 60, y: row_y(&app, 3) }));
        assert!(zh.iter().any(|e| label(e).starts_with("向左旋转")) && zh.iter().any(|e| label(e).starts_with("提取为新 PDF")));
        app.lang = Lang::En;
        // Opened on one of several chosen pages it keeps them; on another page it chooses that one.
        click_row(&mut app, 1, false, false);
        click_row(&mut app, 2, false, true);
        entries_of(app.event(Event::ContextMenu { x: 60, y: row_y(&app, 2) }));
        assert_eq!(app.pg.chosen.pages(), [1, 2]);
        entries_of(app.event(Event::ContextMenu { x: 60, y: row_y(&app, 4) }));
        assert_eq!(app.pg.chosen.pages(), [4]);
        // Every page chosen: nothing is left to keep, so "delete" is greyed.
        key(&mut app, vk::A, true);
        let menu = entries_of(app.event(Event::ContextMenu { x: 60, y: row_y(&app, 0) }));
        assert_eq!((enabled(&menu, CMD_PG_DELETE), enabled(&menu, CMD_PG_LEFT)), (Some(false), Some(true)));
        // A file that does not let pages be changed: the commands are greyed, and a key says why.
        app.rights.assemble = false;
        app.rights.copy = false;
        let menu = entries_of(app.event(Event::ContextMenu { x: 60, y: row_y(&app, 0) }));
        assert!([CMD_PG_LEFT, CMD_PG_RIGHT, CMD_PG_DELETE, CMD_PG_INS_BEFORE, CMD_PG_INS_AFTER, CMD_PG_EXTRACT].iter().all(|&c| enabled(&menu, c) == Some(false)));
        key(&mut app, vk::OEM_6, true);
        assert!(app.note.as_deref().is_some_and(|n| n.contains("does not allow inserting")) && app.edit.asked.is_empty());
        // Over a page the menu is the old one.
        let over = app.event(Event::ContextMenu { x: 600, y: 300 });
        assert!(over.iter().all(|a| !matches!(a, Action::ContextMenu { entries, .. } if entries.iter().any(|e| matches!(e, MenuEntry::Item { id, .. } if *id == CMD_PG_LEFT)))));
    }

    #[test]
    fn pages_from_a_file_come_in_before_or_after_the_chosen_page_and_a_file_with_a_password_asks_for_it() {
        let mut app = open_pages("insert", &distinct(3), true);
        let source = temp_file("two.pdf", &pages_pdf(&[(500, 500, 0), (510, 510, 0)], &[]));
        click_row(&mut app, 1, false, false);
        // After page 1: the box that asks for the file, and the answer puts the pages in.
        let asked = app.event(Event::Command(CMD_PG_INS_AFTER));
        assert!(asked.iter().any(|a| matches!(a, Action::PickFile { .. })));
        assert!(app.pg.insert_pending() && app.thumb_focus);
        app.event(Event::FilePicked(Some(source.clone())));
        pump(&mut app, "the pages to come in", |a| a.page_count() == 5);
        assert_eq!(widths(&app), [100, 110, 500, 510, 120]);
        assert_eq!(app.pg.chosen.pages(), [2, 3], "the pages that came in are chosen");
        assert!(app.note.as_deref().is_some_and(|n| n.contains("inserted")), "{:?}", app.note);
        // Before page 0.
        click_row(&mut app, 0, false, false);
        app.event(Event::Command(CMD_PG_INS_BEFORE));
        app.event(Event::FilePicked(Some(source.clone())));
        pump(&mut app, "the pages before", |a| a.page_count() == 7);
        assert_eq!(widths(&app), [500, 510, 100, 110, 500, 510, 120]);
        // Cancelled: nothing.
        click_row(&mut app, 0, false, false);
        app.event(Event::Command(CMD_PG_INS_AFTER));
        app.event(Event::FilePicked(None));
        assert!(!app.pg.insert_pending() && app.page_count() == 7);
        // Undo twice.
        app.event(Event::Command(CMD_UNDO));
        pump(&mut app, "the first undo", |a| a.page_count() == 5);
        app.event(Event::Command(CMD_UNDO));
        pump(&mut app, "the second undo", |a| a.page_count() == 3);
        // A file with a user password: asked for, a wrong one asked for again, the right one lets the pages in.
        let locked = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/corpus/public/encrypted/qpdf-generated/rects.r6-aes256-user-long.pdf");
        let password = "correct horse battery staple, a very long password of more than thirty-two characters";
        click_row(&mut app, 2, false, false);
        app.event(Event::Command(CMD_PG_INS_AFTER));
        app.event(Event::FilePicked(Some(PathBuf::from(locked))));
        let asks = |actions: &[Action]| actions.iter().filter(|a| matches!(a, Action::AskText { tag: TAG_SRC_PASSWORD, secret: true, .. })).count();
        let actions = pump_actions(&mut app, "the question", |a| a.edit.asked.is_empty());
        assert_eq!(asks(&actions), 1);
        app.event(Event::Text { tag: TAG_SRC_PASSWORD, text: Some("wrong".to_string()) });
        let actions = pump_actions(&mut app, "the second question", |a| a.edit.asked.is_empty());
        assert_eq!(asks(&actions), 1);
        app.event(Event::Text { tag: TAG_SRC_PASSWORD, text: Some(password.to_string()) });
        pump(&mut app, "the pages of the locked file", |a| a.page_count() == 8);
        // A password question that is cancelled drops the pages.
        app.event(Event::Command(CMD_UNDO));
        pump(&mut app, "the undo", |a| a.page_count() == 3);
        click_row(&mut app, 0, false, false);
        app.event(Event::Command(CMD_PG_INS_AFTER));
        app.event(Event::FilePicked(Some(PathBuf::from(locked))));
        pump_actions(&mut app, "the question", |a| a.edit.asked.is_empty());
        app.event(Event::Text { tag: TAG_SRC_PASSWORD, text: None });
        assert!(app.pg.inserting.is_none() && app.page_count() == 3);
        let _ = std::fs::remove_file(source);
    }

    #[test]
    fn the_chosen_pages_are_extracted_to_a_file_of_their_own_and_never_over_the_open_one() {
        let mut app = open_pages("extract", &distinct(5), true);
        click_row(&mut app, 1, false, false);
        click_row(&mut app, 3, true, false);
        let asked = app.event(Event::Command(CMD_PG_EXTRACT));
        let Some(Action::PickSavePath { name, .. }) = asked.iter().find(|a| matches!(a, Action::PickSavePath { .. })) else { panic!("no question: {asked:?}") };
        assert!(name.ends_with("-extract.pdf"), "{name}");
        // The open file itself: refused.
        let open = app.path.clone().expect("a path");
        app.event(Event::SavePicked(Some(open)));
        assert!(app.note.as_deref().is_some_and(|n| n.contains("new file name")), "{:?}", app.note);
        // A new file: written.
        app.event(Event::Command(CMD_PG_EXTRACT));
        let target = std::env::temp_dir().join(format!("qingpdf-pages-ui-{}-extracted.pdf", std::process::id()));
        let _ = std::fs::remove_file(&target);
        app.event(Event::SavePicked(Some(target.clone())));
        pump(&mut app, "the extraction", |a| a.pg.extracting.is_none() && a.note.as_deref().is_some_and(|n| n.contains("Extracted 2 page")));
        let doc = qingpdf_core::Document::open(&target).expect("the extracted file opens");
        let pages = doc.pages().expect("pages");
        assert_eq!(pages.len(), 2);
        assert_eq!(pages.iter().map(|p| p.media_box().map_or(0.0, |b| b[2]) as i64).collect::<Vec<_>>(), [110, 130]);
        // The open file is not changed by it: no changes to save.
        assert!(!app.edit.dirty);
        let _ = std::fs::remove_file(target);
        // Cancelled: nothing.
        app.event(Event::Command(CMD_PG_EXTRACT));
        app.event(Event::SavePicked(None));
        assert!(app.pg.extract.is_none() && app.pg.extracting.is_none());
    }

    /// Turn, delete and move on a file of a thousand pages, with the list of thumbnails open: from the key (or the drop) to the pages
    /// drawn again sharp in the window, best of three. (In a build with the checks on, it only runs: the numbers are of the real build.)
    #[test]
    fn a_thousand_pages_are_turned_deleted_and_moved_within_a_tenth_of_a_second() {
        let mut app = open_pages("thousand", &letter(1000), true);
        pump(&mut app, "the first picture", |a| a.visible_ready() && a.edit.asked.is_empty());
        let best = |app: &mut App, what: &str, run: &dyn Fn(&mut App), done: &dyn Fn(&App) -> bool| -> f64 {
            let mut fastest = f64::MAX;
            for _ in 0..3 {
                let t = std::time::Instant::now();
                run(app);
                while !(done(app) && app.edit.asked.is_empty() && app.visible_ready()) {
                    app.event(Event::Wake);
                    assert!(t.elapsed() < Duration::from_secs(20), "{what}: no end");
                    std::thread::yield_now();
                }
                fastest = fastest.min(t.elapsed().as_secs_f64() * 1000.0);
                // Settled: the pictures of the list are in again, so that the next round starts from the same.
                pump(app, "the list", |a| a.thumbs.asked.is_empty() && a.is_idle());
            }
            fastest
        };
        let before = app.page_count();
        let turn = best(&mut app, "turn", &|a| {
            a.pg.chosen.set([5, 6, 7]);
            key(a, vk::OEM_6, true);
        }, &|a| a.edit.dirty && a.page_count() == before);
        let delete = best(&mut app, "delete", &|a| {
            a.pg.chosen.set([10, 11]);
            a.thumb_focus = true;
            key(a, vk::DELETE, false);
        }, &|a| a.page_count() < before);
        let count = app.page_count();
        let reorder = best(&mut app, "move", &|a| {
            a.pg.chosen.set([20, 21]);
            let _ = a.move_chosen(0);
        }, &|a| a.page_count() == count && a.pg.chosen.pages() == [0, 1]);
        println!("1000 pages, to the pages drawn again: turn {turn:.1} ms, delete {delete:.1} ms, move {reorder:.1} ms");
        if !cfg!(debug_assertions) {
            assert!(turn <= 100.0 && delete <= 100.0 && reorder <= 100.0, "turn {turn:.1} delete {delete:.1} move {reorder:.1}");
        }
    }

    #[test]
    fn a_second_change_waits_for_the_answer_to_the_first_instead_of_naming_pages_that_are_not_there_any_more() {
        let mut app = open_pages("twice", &distinct(6), true);
        click_row(&mut app, 1, false, false);
        click_row(&mut app, 2, true, false);
        key(&mut app, vk::DELETE, false);
        // Before the answer came: the same keys again, and an undo that would be queued behind it, change nothing more.
        key(&mut app, vk::DELETE, false);
        key(&mut app, vk::OEM_6, true);
        assert_eq!(app.edit.asked.len(), 1);
        pump(&mut app, "the answer", |a| a.page_count() == 4);
        assert_eq!(widths(&app), [100, 130, 140, 150]);
        // Answered: the next change is made.
        click_row(&mut app, 0, false, false);
        key(&mut app, vk::OEM_6, true);
        pump(&mut app, "the turn", |a| widths(a)[0] == 200);
    }

    #[test]
    fn an_edit_that_names_pages_waits_while_a_change_to_the_pages_is_not_answered() {
        let mut app = open_pages("edit-waits", &distinct(6), true);
        click_row(&mut app, 1, false, false);
        key(&mut app, vk::DELETE, false);
        assert!(app.pages_busy() && app.edit.asked.len() == 1);
        // A watermark (it names pages), a recognition run: each would name pages as they were, so none is sent; the person is told.
        app.note = None;
        app.watermark_answered(Some("DRAFT".to_string()));
        assert!(app.note.as_deref().is_some_and(|n| n.contains("try again")), "{:?}", app.note);
        app.start_ocr(true);
        assert!(app.ocr.is_none());
        assert_eq!(app.edit.asked.len(), 1, "only the deletion was sent");
        // Answered: the same request goes through.
        pump(&mut app, "the answer", |a| a.page_count() == 5);
        assert!(!app.pages_busy());
        app.watermark_answered(Some("DRAFT".to_string()));
        assert_eq!(app.edit.asked.len(), 1);
        pump(&mut app, "the watermark", |a| a.edit.asked.is_empty());
    }

    #[test]
    fn a_file_that_does_not_allow_assembling_is_not_changed() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/corpus/public/encrypted/qpdf-generated/bookmarks.r4-aes128-user-assemble-n.pdf");
        let mut app = app();
        app.event(Event::Size { w: 1000, h: 1100 });
        app.apply_settings(&crate::settings::Settings { tab: Some(crate::thumbs::Tab::Pages), ..crate::settings::Settings::default() });
        app.open_path(std::path::Path::new(file), "user");
        pump(&mut app, "the file to open", |a| a.has_document());
        assert!(!app.rights.assemble);
        click_row(&mut app, 0, false, false);
        key(&mut app, vk::OEM_6, true);
        assert!(app.note.as_deref().is_some_and(|n| n.contains("does not allow inserting")) && app.edit.asked.is_empty() && !app.edit.dirty);
        // Dragging does not even start.
        app.event(Event::MouseDown { x: 60, y: row_y(&app, 0), ctrl: false, shift: false, clicks: 1 });
        app.event(Event::MouseMove { x: 60, y: row_y(&app, 1), shift: false });
        assert!(app.pg.drag.as_ref().is_some_and(|d| !d.moved));
        app.event(Event::MouseUp { x: 60, y: row_y(&app, 1) });
        assert!(app.edit.asked.is_empty());
    }
}

#[cfg(test)]
mod outline_tests {
    use super::super::sidebar::tests::{app, pump};
    use super::*;

    #[test]
    fn the_bookmarks_lead_to_the_pages_they_led_to_after_the_pages_move() {
        let mut app = app();
        app.event(Event::Size { w: 1000, h: 800 });
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/corpus/public/outline-form-attach/bookmarks.pdf");
        app.open_path(std::path::Path::new(file), "");
        pump(&mut app, "the bookmarks", |a| !a.outline.is_empty());
        let before: Vec<Option<u32>> = app.outline.iter().map(|i| i.page).collect();
        assert!(before.iter().any(|p| p.is_some()), "{before:?}");
        // Page 0 is moved behind page 1 (the engine is asked for it as the drag of the list asks).
        app.pg.chosen.set([0]);
        app.pg.drag = None;
        let _ = app.move_chosen(2);
        pump(&mut app, "the move", |a| a.edit.dirty);
        pump(&mut app, "the bookmarks again", |a| a.outline.iter().map(|i| i.page).collect::<Vec<_>>() != before);
        let after: Vec<Option<u32>> = app.outline.iter().map(|i| i.page).collect();
        let expect: Vec<Option<u32>> = before.iter().map(|p| p.map(|p| 1 - p.min(1))).collect();
        assert_eq!(after, expect, "each bookmark leads to the page it led to, which is at another place now");
    }
}
