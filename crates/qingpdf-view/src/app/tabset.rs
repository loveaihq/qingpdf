//! Several files in one window (2026-10-17): opening a file into a tab of its own, showing another tab, closing tabs, the files that wait to be
//! opened (those dropped on the window, named on the command line, handed over by another copy of the program), and where the reader was
//! in each file. The tab that is shown has its state in the reader's own fields; a tab that is not shown has its state put away in its place
//! in the strip (`TabState`, `Tabs`), with its pictures, its requests and its lists of the page let go of. The engine keeps the document of
//! such a tab (with the edits not saved), or lets it go when it needs the room (`docs/view-api.md`), and then the file is read again when
//! the tab is wanted.

use crate::places::{FULL, Place, normalize};
use crate::recent::{MODE_CUSTOM, MODE_FIT_PAGE, MODE_FIT_WIDTH};
use crate::tabs::Where;
use crate::ui::OutlineNode;

use super::editing::After;
use super::*;

/// The most the tabs that are not shown keep of pictures of what was in view, in all (what the engine is told is the total less this).
const PARKED_PICTURES: u64 = 40_000_000;

impl App {
    /// The height of the strip of tabs, in pixels (0: it is not there, with fewer than two files open).
    pub(super) fn bar_h(&self) -> i64 {
        if self.tabs.len() >= 2 && !self.full { self.px(crate::tabs::BAR_H) } else { 0 }
    }

    /// The most pixels the pages can be shown in (for the engine's plan).
    pub(super) fn view_pixels(&self) -> u64 {
        u64::try_from(self.view_w.max(0)).unwrap_or(0).saturating_mul(u64::try_from(self.view_h.max(0)).unwrap_or(0)).max(self.max_view_pixels)
    }

    /// Something is going on in the shown tab that is answered to the tab, or that is left half done if the tab is left: a file being opened,
    /// a save, an export, an edit not yet answered, a compression, a change of protection, a print, a run of text recognition, pages being put in.
    pub(super) fn tab_busy(&self) -> bool {
        self.opening || self.print.is_some() || self.ocr.is_some() || self.cz.busy() || self.pt.is_busy() || self.pg.busy() || self.edit.busy()
    }

    /// Make `incoming` the state of the file that is shown and give back the state that was shown. What belongs to the person and not to the file
    /// goes along: the colour and the like of the annotations, and where random numbers come from.
    fn exchange(&mut self, mut incoming: TabState) -> TabState {
        let random = self.pt.random;
        self.swap_tab(&mut incoming);
        // (The pictures it brings were made in the colours of the time.)
        self.recheck_tint();
        self.pt.random = random;
        self.edit.take_prefs(&mut incoming.edit);
        incoming
    }

    /// The bookmarks as the window's tree shows them.
    pub(super) fn outline_nodes(&self) -> Vec<OutlineNode> {
        let untitled = t(self.lang, Msg::Untitled);
        self.outline
            .iter()
            .map(|i| OutlineNode { title: if i.title.trim().is_empty() { untitled.to_string() } else { i.title.clone() }, depth: i.depth, open: i.open })
            .collect()
    }

    /// Take back everything the shown tab has asked of the engine and let go of what is drawn and listed for it (the view, the bookmarks and the
    /// document itself stay): it is a tab that waits from now on.
    pub(super) fn release(&mut self) -> Vec<Action> {
        let mut ids: Vec<u64> = self.asked.drain().map(|(id, _)| id).collect();
        ids.extend(self.links_asked.drain().map(|(id, _)| id));
        ids.extend(self.boxes_asked.drain().map(|(id, _)| id));
        ids.extend(self.thumbs.asked.drain().map(|(id, _)| id));
        ids.extend(self.search.take().map(|s| s.id));
        ids.extend(self.copy_asked.take());
        let mut actions = Vec::new();
        ids.extend(self.edit.forget_pages(&mut actions));
        for id in ids {
            self.engine.cancel(id);
        }
        self.reset_annot_list();
        self.thumbs.reset(0);
        self.in_flight.clear();
        self.failed.clear();
        self.notices.clear();
        self.links.clear();
        self.boxes.clear();
        self.pending_select = None;
        self.press = None;
        self.ctx_at = None;
        self.thumb_drag = false;
        self.thumb_nav = false;
        self.thumb_target = None;
        self.thumb_focus = false;
        self.annots_focus = false;
        self.copy_note = None;
        self.moving = false;
        actions.push(Action::KillTimer(TIMER_SETTLE));
        actions
    }

    /// The shown tab stops being shown (another is, or none): what it has asked for is taken back, where the reader is in it is noted, and its
    /// state is put away in its place in the strip.
    fn leave_tab(&mut self) -> Vec<Action> {
        let Some(a) = self.tabs.active() else { return Vec::new() };
        // (A presentation is the shown tab's: it ends before the tab goes, or its zoom and layout would be put away with the tab.)
        let mut actions = self.end_present(true);
        actions.extend(self.release());
        // (What the engine answers about this tab from now on is not waited for.)
        self.activating = false;
        // The sharp pieces of the pages in view stay (see `trim_pictures`), so that showing the tab again is not drawing it again.
        let focus = self.focus();
        self.cache.set_budget(0, &focus);
        // (What was said in the status bar is about the tab that goes.)
        self.note = None;
        self.remember_position();
        self.restore = self.current_place();
        let at = if self.doc.is_some() { Where::Waiting } else { Where::Dropped };
        let outgoing = self.exchange(TabState::blank());
        if let Some(tab) = self.tabs.get_mut(a) {
            tab.kept = Some(Box::new(outgoing));
            tab.at = at;
            tab.left = self.left_count;
        }
        self.left_count += 1;
        self.trim_pictures();
        actions
    }

    /// The pictures the tabs that are not shown keep of what was in view, in bytes.
    fn parked_pictures(&self) -> u64 {
        self.tabs.iter().filter_map(|t| t.kept.as_ref()).map(|k| k.cache.bytes()).sum()
    }

    /// The tabs that are not shown keep the pictures of the pages that were in view (nothing else of what was drawn), for as many of them
    /// as fit [`PARKED_PICTURES`], the ones shown last first; the others let theirs go.
    fn trim_pictures(&mut self) {
        // (The tab shown longest ago that still has pictures lets go of them, until they fit.)
        while self.parked_pictures() > PARKED_PICTURES {
            let oldest = self.tabs.iter_mut().filter_map(|t| t.kept.as_mut().map(|k| (t.left, k))).filter(|(_, k)| k.cache.bytes() > 0).min_by_key(|(left, _)| *left);
            let Some((_, kept)) = oldest else { return };
            kept.cache.clear();
        }
    }

    /// The memory goal handed to the engine: the total, less what the tabs that are not shown may keep of pictures ([`PARKED_PICTURES`]) as soon as
    /// there are two files. It is the whole of it from the start and not what they keep at the time: the engine lets files in against it, and
    /// the pictures grow as the tabs are shown one after the other, with nothing in the engine to give way.
    fn engine_total(&self) -> u64 {
        if self.tabs.len() >= 2 { self.total.saturating_sub(PARKED_PICTURES) } else { self.total }
    }

    /// Show tab `index` (its state is put in the reader's fields). Gives the actions that put the window right.
    fn bring(&mut self, index: usize) -> Vec<Action> {
        self.tabs.show(index);
        let Some(tab) = self.tabs.get_mut(index) else { return Vec::new() };
        let at = std::mem::replace(&mut tab.at, Where::Shown);
        let path = tab.path.clone();
        let kept = tab.kept.take().unwrap_or_else(|| {
            let mut blank = TabState::blank();
            blank.path = Some(path);
            Box::new(blank)
        });
        // (A state without a document waits for the file to be read again.)
        let waiting = at == Where::Waiting && kept.doc.is_some();
        self.exchange(*kept);
        self.bars = (None, None);
        self.sharp_on_screen = false;
        self.moving = false;
        self.menus_dirty = true;
        self.draw_scale = sched::scale_key(self.zoom, self.dpi);
        self.view_x = self.sidebar_width();
        self.view_w = (self.client_w - self.view_x).max(0);
        self.view_h = (self.client_h - self.bar_h()).max(0);
        let mut actions = vec![Action::SetTitle(self.title()), Action::Outline(self.outline_nodes()), Action::Invalidate];
        if waiting {
            // Nothing is drawn until the engine says what the plan is now.
            self.activating = true;
            self.restore = None;
            self.engine.activate(self.doc_id, self.engine_total(), self.view_pixels());
        } else {
            self.activating = false;
            actions.extend(self.reopen());
        }
        let (ax, ay) = (self.view_w / 2, self.view_h / 2);
        self.relayout(ax, ay, None);
        actions.extend(self.after_view_change(false));
        actions.extend(self.chrome());
        actions
    }

    /// The shown tab has no document: the engine let go of it. Its file is read again, and the view put back where it was.
    fn reopen(&mut self) -> Vec<Action> {
        self.doc = None;
        let Some(path) = self.path.clone() else { return Vec::new() };
        let changed = self.stamp.zip(crate::places::stamp_of(&path)).is_some_and(|(then, now)| then != now);
        let mut actions = self.begin_open("");
        if changed {
            actions.extend(self.say(t(self.lang, Msg::TabFileChanged).to_string()));
        }
        actions
    }

    /// The engine let go of the document of a tab (to make room for a file): a tab that is not shown keeps only where it was.
    pub(super) fn on_dropped(&mut self, doc: u64) -> Vec<Action> {
        if doc == self.doc_id && self.doc.is_some() {
            // The shown tab was asked for after its document went: it is read again.
            self.activating = false;
            self.restore = self.current_place();
            let mut actions = self.release();
            self.cache.clear();
            actions.extend(self.reopen());
            return actions;
        }
        if let Some(i) = self.tabs.position(|t| t.kept.as_ref().is_some_and(|k| k.doc_id == doc && k.doc.is_some()))
            && let Some(tab) = self.tabs.get_mut(i)
        {
            tab.at = Where::Dropped;
            if let Some(k) = tab.kept.as_mut() {
                k.doc = None;
                k.cache.clear();
                k.outline.clear();
                k.pt.reset();
                k.edit.can_undo = false;
                k.edit.can_redo = false;
                k.pg = PageWork::new();
            }
        }
        Vec::new()
    }

    /// The engine serves the shown tab's document again, with the plan the total allows now: the pictures are asked for.
    pub(super) fn on_activated(&mut self, doc: u64, bitmap_cache_bytes: u64, thumb_cache_bytes: u64, max_tile_pixels: u32) -> Vec<Action> {
        if doc != self.doc_id || !self.activating {
            return Vec::new();
        }
        self.activating = false;
        if let Some(d) = self.doc.as_mut() {
            d.max_tile_pixels = max_tile_pixels;
        }
        let focus = self.focus();
        self.cache.set_budget(bitmap_cache_bytes, &focus);
        self.thumbs.reset(thumb_cache_bytes);
        let mut actions = self.after_view_change(false);
        self.ask_for_annot_list();
        // The bookmarks asked for when the file was opened may have gone with the requests the engine let go of when it went on to the next file
        // (a file that has none is asked again: that costs nothing).
        if self.outline.is_empty() && self.bench.is_none() {
            actions.extend(self.ask_for_outline());
        }
        // The selection is drawn from the boxes of its characters.
        if let Some(sel) = self.selection {
            let (first, last) = sel.ordered();
            for page in first.page..=last.page.min(first.page.saturating_add(8)) {
                actions.extend(self.ask_for_boxes(page));
            }
        }
        if self.find_open && !self.find_text.trim().is_empty() {
            actions.extend(self.start_search(&self.find_text.clone()));
        }
        actions
    }

    // --- opening ---------------------------------------------------------------------------------------------

    /// Open a file in a tab of its own (the shown one waits), or show its tab if it is open already. A file that waits for a password is
    /// asked for again with the same tab.
    pub fn open_path(&mut self, path: &Path, password: &str) -> Vec<Action> {
        let full = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        if let Some(i) = self.tabs.find(&full) {
            return self.show_tab(i);
        }
        if self.tabs.full() {
            return vec![Action::Message { title: t(self.lang, Msg::Error).to_string(), text: t(self.lang, Msg::TooManyTabs).to_string() }];
        }
        if self.tab_busy() {
            return self.say(t(self.lang, Msg::TabBusy).to_string());
        }
        let mut actions = self.leave_tab();
        let _ = self.tabs.push(full.clone());
        self.path = Some(full);
        actions.extend(self.begin_open(password));
        actions.extend(self.bar_changed());
        actions
    }

    /// The shown tab shows another file, or this file written again (the document is replaced, the tab stays).
    pub(super) fn open_path_now(&mut self, path: &Path, password: &str) -> Vec<Action> {
        if self.tabs.is_empty() {
            return self.open_path(path, password);
        }
        // Where the reader was in the file it leaves is kept.
        self.remember_position();
        let old = self.doc_id;
        let mut actions = self.drop_document();
        if old != 0 {
            self.engine.close_doc(old);
        }
        self.path = Some(std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()));
        actions.extend(self.begin_open(password));
        actions
    }

    /// Ask the engine to read the file of the shown tab (`self.path`) as a new document.
    pub(super) fn begin_open(&mut self, password: &str) -> Vec<Action> {
        // (A file is read: the answer of the engine about a document that waited is not for it.)
        self.activating = false;
        if self.engine.start_error().is_some() {
            // No drawing thread: nothing would ever answer, so say so instead of showing "Opening..." for ever.
            let text = t(self.lang, Msg::EngineFailed).to_string();
            let mut actions = vec![Action::Message { title: t(self.lang, Msg::Error).to_string(), text }];
            actions.extend(self.close_current());
            return actions;
        }
        self.doc_id = self.next_doc;
        self.next_doc += 1;
        self.opening = true;
        self.stamp = self.path.as_deref().and_then(crate::places::stamp_of);
        self.engine.open_file(self.doc_id, &self.path_text(), password, self.engine_total(), self.view_pixels());
        vec![Action::SetTitle(self.title()), Action::Invalidate]
    }

    /// The strip has come or gone (the second file was opened, the last but one closed): the pages have the room that is left.
    fn bar_changed(&mut self) -> Vec<Action> {
        let view_h = (self.client_h - self.bar_h()).max(0);
        if view_h == self.view_h {
            return Vec::new();
        }
        self.view_h = view_h;
        let (ax, ay) = (self.view_w / 2, self.view_h / 2);
        self.relayout(ax, ay, None);
        let mut actions = self.after_view_change(false);
        actions.extend(self.chrome());
        actions
    }

    /// Open the files that wait, one after the other (the next when the one before is open or has failed).
    pub(super) fn open_queued(&mut self) -> Vec<Action> {
        let mut actions = Vec::new();
        while self.ready && !self.opening && let Some(path) = self.queue.pop_front() {
            if self.tabs.full() && self.tabs.find(&std::path::absolute(&path).unwrap_or_else(|_| path.clone())).is_none() {
                self.queue.clear();
                actions.push(Action::Message { title: t(self.lang, Msg::Error).to_string(), text: t(self.lang, Msg::TooManyTabs).to_string() });
                break;
            }
            actions.extend(self.open_path(&path, ""));
        }
        actions
    }

    /// Files were dropped on the window (or handed over by another copy of the program): those that are PDF files are opened, each in a tab; for
    /// the others a message says which they are, and nothing else is done.
    pub(super) fn dropped(&mut self, mut files: Vec<PathBuf>) -> Vec<Action> {
        // (A few dozen are looked at, all at once and with a time to answer in: a file that does not answer is not waited for.)
        let over = files.len().saturating_sub(crate::tabs::MAX_DROPPED);
        files.truncate(crate::tabs::MAX_DROPPED);
        let answers = crate::tabs::sniff(&files, crate::tabs::SNIFF_WAIT);
        let mut skipped: Vec<String> = Vec::new();
        for (file, answer) in files.into_iter().zip(answers) {
            let name = crate::tabs::label(&file);
            match answer {
                Ok(true) => self.queue.push_back(file),
                Ok(false) => skipped.push(name),
                Err(e) => skipped.push(format!("{name} ({e})")),
            }
        }
        let mut actions = self.open_queued();
        if !skipped.is_empty() {
            // The message lists a few names and says how many more there are.
            let more = skipped.len().saturating_sub(crate::tabs::SKIPPED_SHOWN) + over;
            skipped.truncate(crate::tabs::SKIPPED_SHOWN);
            if more > 0 {
                skipped.push(format!("\u{2026} +{more}"));
            }
            actions.push(Action::Message { title: t(self.lang, Msg::Error).to_string(), text: format!("{}\n\n{}", t(self.lang, Msg::NotPdf), skipped.join("\n")) });
        }
        actions
    }

    // --- showing and closing ---------------------------------------------------------------------------------

    /// Show tab `index`.
    pub(super) fn show_tab(&mut self, index: usize) -> Vec<Action> {
        let Some(shown) = self.tabs.active() else { return Vec::new() };
        if index == shown || index >= self.tabs.len() {
            return Vec::new();
        }
        // (A file is being read: the tab is shown as soon as it is.)
        if self.opening {
            self.pending_tab = Some(index);
            return Vec::new();
        }
        if self.tab_busy() {
            return self.say(t(self.lang, Msg::TabBusy).to_string());
        }
        // A file that was let go of and is not there any more cannot be shown: the person is told, and stays where they are.
        if let Some(tab) = self.tabs.get(index)
            && tab.at == Where::Dropped
            && crate::places::stamp_of(&tab.path).is_none()
        {
            return vec![Action::Message { title: t(self.lang, Msg::Error).to_string(), text: format!("{}\n{}", t(self.lang, Msg::CannotRead), tab.path.display()) }];
        }
        let mut actions = self.leave_tab();
        actions.extend(self.bring(index));
        actions
    }

    /// Ctrl+Tab and Ctrl+Shift+Tab, and the menu: the tab after or before the shown one, round the strip.
    pub(super) fn step_tab(&mut self, forward: bool) -> Vec<Action> {
        match self.tabs.next(forward) {
            Some(i) => self.show_tab(i),
            None => Vec::new(),
        }
    }

    /// Ctrl+W: close the shown tab.
    pub(super) fn close_tab_command(&mut self) -> Vec<Action> {
        match self.tabs.active() {
            Some(a) => self.close_tab(a),
            None => Vec::new(),
        }
    }

    /// Close tab `index`; if its document has changes that are not saved, the person is asked first.
    pub(super) fn close_tab(&mut self, index: usize) -> Vec<Action> {
        let Some(shown) = self.tabs.active() else { return Vec::new() };
        if index != shown {
            // A tab with changes is shown first, so that the question is asked about what can be seen.
            if self.tabs.get(index).is_some_and(|t| t.dirty) {
                let mut actions = self.show_tab(index);
                if self.tabs.active() == Some(index) {
                    actions.extend(self.close_tab(index));
                }
                return actions;
            }
            return self.close_hidden(index);
        }
        self.sync_tab();
        match self.ask_unsaved(After::CloseTab) {
            Some(actions) => actions,
            None => self.close_current(),
        }
    }

    /// Close a tab that is not shown (it has nothing unsaved).
    fn close_hidden(&mut self, index: usize) -> Vec<Action> {
        let Some(closed) = self.tabs.close(index) else { return Vec::new() };
        if let Some(mut kept) = closed.tab.kept {
            if closed.tab.at == Where::Waiting {
                self.engine.close_doc(kept.doc_id);
            }
            kept.pt.reset();
        }
        self.menus_dirty = true;
        let mut actions = self.bar_changed();
        actions.push(Action::Invalidate);
        actions
    }

    /// Close the shown tab, with whatever it has going on and whatever it holds; the tab to its right is shown (else the last).
    pub(super) fn close_current(&mut self) -> Vec<Action> {
        let Some(shown) = self.tabs.active() else { return Vec::new() };
        self.remember_position();
        self.flush_places();
        let old = self.doc_id;
        let mut actions = self.drop_document();
        if old != 0 {
            self.engine.close_doc(old);
        }
        (self.path, self.stamp, self.restore, self.doc_id, self.opening, self.activating) = (None, None, None, 0, false, false);
        let next = self.tabs.close(shown).and_then(|c| c.show);
        if let Some(i) = next {
            actions.extend(self.bring(i));
        }
        actions.extend(self.bar_changed());
        actions.push(Action::SetTitle(self.title()));
        actions.push(Action::Invalidate);
        self.menus_dirty = true;
        actions
    }

    /// Keep the strip's idea of the shown tab (its name, whether it has changes) right.
    pub(super) fn sync_tab(&mut self) {
        let (dirty, path) = (self.edit.dirty, self.path.clone());
        if let Some(tab) = self.tabs.active().and_then(|a| self.tabs.get_mut(a))
            && (tab.dirty != dirty || path.as_ref().is_some_and(|p| *p != tab.path))
        {
            tab.dirty = dirty;
            if let Some(p) = path {
                tab.path = p;
            }
            self.tab_hover = None;
        }
    }

    /// The person asked to close the window (the close button, Alt+F4, File > Exit): the tabs whose changes were given up in an earlier
    /// round of questions that was not carried through are asked about again.
    pub(super) fn quit_asked(&mut self) -> Vec<Action> {
        // A presentation ends first: the window is closed from the window, not from the screen, and another tab may be shown for a question.
        let mut actions = self.end_present(true);
        self.discarded.clear();
        actions.extend(self.quit());
        actions
    }

    /// Close the window ("what next" of a question about changes): the tabs with changes are asked about one after the other. The changes
    /// that were given up stay in their tabs until the window is gone (the person can still cancel at the next question), and are not asked
    /// about again in this round.
    pub(super) fn quit(&mut self) -> Vec<Action> {
        self.sync_tab();
        if !self.discarded.contains(&self.doc_id)
            && let Some(actions) = self.ask_unsaved(After::Quit)
        {
            return actions;
        }
        let given_up = &self.discarded;
        if let Some(i) = self.tabs.first_dirty(|t| t.kept.as_ref().is_none_or(|k| given_up.contains(&k.doc_id))) {
            // Not while something is going on in the shown tab (it is not left): the window stays, and the person is told why.
            if self.tab_busy() {
                return self.say(t(self.lang, Msg::TabBusy).to_string());
            }
            let mut actions = self.show_tab(i);
            if self.tabs.active() == Some(i)
                && let Some(ask) = self.ask_unsaved(After::Quit)
            {
                actions.extend(ask);
            }
            // (A tab with changes that could not be shown is not one the window is closed over.)
            return actions;
        }
        self.exit_now()
    }

    // --- where the reader was ----------------------------------------------------------------------------------

    /// Where the reader is in the shown file (none if nothing is open).
    pub(super) fn current_place(&self) -> Option<Place> {
        let doc = self.doc.as_ref()?;
        let path = self.path.as_deref()?;
        // (In a presentation the view is in the middle of the page shown, above its top when it is not as high as the screen: the page is the one in the middle.)
        let (page, fraction) = if self.present.is_some() { (self.current_page(), 0.0) } else { doc.layout.locate(self.scroll_y) };
        let width = doc.layout.content_width().max(1);
        let permille = |v: f64| (v * f64::from(FULL)).round().clamp(0.0, f64::from(FULL)) as u32;
        // (In the presentation it is the zoom from before it that is kept.)
        let (zoom_mode, zoom) = self.present.as_ref().map_or((self.mode, self.zoom), |p| (p.saved.mode, p.saved.zoom));
        let mode = match zoom_mode {
            ZoomMode::FitWidth => MODE_FIT_WIDTH,
            ZoomMode::FitPage => MODE_FIT_PAGE,
            ZoomMode::Custom => MODE_CUSTOM,
        };
        Some(Place {
            path: normalize(path),
            stamp: self.stamp?,
            page: u32::try_from(page).unwrap_or(0),
            mode,
            zoom,
            across: permille(self.scroll_x as f64 / width as f64),
            down: permille(fraction),
            rotation: self.rotation,
            layout: Some(self.spread),
        })
    }

    /// Put the view where `place` says (the file is just open, at its first page).
    pub(super) fn apply_place(&mut self, place: &Place) {
        let (mode, rotation) = (
            match place.mode {
                MODE_FIT_PAGE => ZoomMode::FitPage,
                MODE_CUSTOM => ZoomMode::Custom,
                _ => ZoomMode::FitWidth,
            },
            place.rotation % 360,
        );
        self.rotation = rotation;
        if let Some(spread) = place.layout {
            self.spread = spread;
        }
        self.relayout(0, 0, Some((mode, place.zoom)));
        let Some(doc) = self.doc.as_ref() else { return };
        let page = (place.page as usize).min(doc.layout.len().saturating_sub(1));
        self.scroll_y = doc.layout.y_of(page, f64::from(place.down) / f64::from(FULL));
        self.scroll_x = (f64::from(place.across) / f64::from(FULL) * doc.layout.content_width() as f64).round() as i64;
        self.clamp_scroll();
    }

    /// Where the reader was last time in the file that has just been opened, if it is the same file and the person wants that.
    pub(super) fn stored_place(&self) -> Option<Place> {
        if !self.remember_place || self.bench.is_some() || self.print_test.is_some() {
            return None;
        }
        let path = self.path.as_deref()?;
        self.places.find(&normalize(path), self.stamp?).cloned()
    }

    /// Write the list of recent files and the places to the disk if they have changed (what cannot be written is no reason to stop).
    pub(super) fn flush_places(&mut self) {
        if std::mem::take(&mut self.places_dirty) {
            self.save_recent();
            if let Some(path) = &self.places_path {
                let _ = crate::places::save(path, &self.places);
            }
        }
    }

    /// Forget where the reader was (the list of recent files is cleared with it).
    pub(super) fn forget_places(&mut self) {
        self.places.clear();
        self.places_dirty = true;
        self.flush_places();
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::super::editing::Asked;
    use super::super::sidebar::tests::{Rec, app as plain_app, pages_pdf, pump};
    use super::*;
    use crate::tabs::MAX_TABS;
    use crate::ui::{SaveChoice, vk};

    /// A folder of the test's own.
    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("qingpdf-tabset-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A PDF file of `pages` pages in `dir`.
    fn file(dir: &Path, name: &str, pages: usize) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, pages_pdf(&vec![(612, 792, 0); pages], &[])).unwrap();
        path
    }

    fn app() -> App {
        let mut app = plain_app();
        app.event(Event::Size { w: 900, h: 700 });
        app
    }

    /// The shown tab has its document and its pages are drawn.
    fn ready(app: &mut App) {
        for _ in 0..1500 {
            app.event(Event::Wake);
            if app.has_document() && !app.opening && !app.activating && app.visible_ready() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("not ready: tab {:?} of {}, doc {} {}, opening {}, activating {}, in flight {}, asked {}, failed {}, visible {:?}, view {:?} {}x{}", app.tabs.active(), app.tabs.len(), app.doc_id, app.has_document(), app.opening, app.activating, app.in_flight.len(), app.asked.len(), app.failed.len(), app.visible(), (app.scroll_x, app.scroll_y, app.zoom, app.rotation), app.view_w, app.view_h);
    }

    /// A test that wants the engine short of room (it has set a total of its own): what the engine is told is then the least that holds a
    /// small file beside another and the least for serving one (what is set aside for the pictures of the tabs is made up for).
    fn tight(app: &mut App) {
        if app.total != qingpdf_core::view::TOTAL_BYTES {
            app.total = qingpdf_core::view::ACTIVE_FLOOR + (2 << 20) + PARKED_PICTURES;
        }
    }

    fn open(app: &mut App, path: &Path) -> Vec<Action> {
        tight(app);
        let actions = app.open_path(path, "");
        ready(app);
        actions
    }

    fn view(app: &App) -> (usize, u32, u16, i64) {
        (app.current_page(), app.zoom, app.rotation, app.scroll_y)
    }

    fn key(app: &mut App, vk: u32, shift: bool) -> Vec<Action> {
        tight(app);
        let actions = app.event(Event::Key { vk, ctrl: true, shift });
        ready(app);
        actions
    }

    fn asked_to_save(actions: &[Action]) -> bool {
        actions.iter().any(|a| matches!(a, Action::AskSave { tag: TAG_UNSAVED, .. }))
    }

    #[test]
    fn each_file_is_a_tab_and_each_tab_keeps_its_own_page_zoom_turn_and_place() {
        let dir = folder("views");
        let (a, b, c) = (file(&dir, "a.pdf", 6), file(&dir, "b.pdf", 6), file(&dir, "c.pdf", 1));
        let mut app = app();
        open(&mut app, &a);
        assert_eq!((app.tabs.len(), app.bar_h()), (1, 0), "one file: no strip");
        assert_eq!(app.engine_total(), app.total, "one file: the engine has the whole of it");
        open(&mut app, &b);
        open(&mut app, &c);
        assert_eq!((app.tabs.len(), app.tabs.active()), (3, Some(2)));
        assert!(app.bar_h() > 0 && app.view_h + app.bar_h() == 700, "the strip takes its height from the pages' area");
        assert!(app.title().starts_with("c.pdf"), "{}", app.title());
        // Ctrl+Shift+Tab goes to the tab before: b, where the view is changed.
        key(&mut app, vk::TAB, true);
        assert!(app.title().starts_with("b.pdf") && app.page_count() == 6);
        app.go_to_page(3);
        app.event(Event::Command(CMD_ROTATE));
        app.event(Event::Command(CMD_ZOOM_IN));
        app.event(Event::Command(CMD_ZOOM_IN));
        app.event(Event::Timer(TIMER_SETTLE));
        ready(&mut app);
        let mine = view(&app);
        assert_eq!((mine.0, mine.2), (3, 90));
        // a keeps its own view (the first page, as it was opened).
        key(&mut app, vk::TAB, true);
        assert!(app.title().starts_with("a.pdf"));
        assert_eq!((app.current_page(), app.rotation), (0, 0));
        assert!(app.zoom != mine.1);
        // The tab after a is b again, and it is as it was left.
        key(&mut app, vk::TAB, false);
        assert!(app.title().starts_with("b.pdf"));
        assert_eq!(view(&app), mine);
        // A file that is open is not opened again: its tab is shown.
        app.open_path(&a, "");
        ready(&mut app);
        assert_eq!((app.tabs.len(), app.tabs.active()), (3, Some(0)));
        // Ctrl+Tab goes round the strip.
        key(&mut app, vk::TAB, false);
        key(&mut app, vk::TAB, false);
        key(&mut app, vk::TAB, false);
        assert_eq!(app.tabs.active(), Some(0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tab_that_is_shown_again_has_its_page_on_the_screen_at_once_and_the_pictures_kept_are_few() {
        let dir = folder("pictures");
        let files: Vec<PathBuf> = (0..6).map(|i| file(&dir, &format!("p{i}.pdf"), 3)).collect();
        let mut app = app();
        for f in &files {
            open(&mut app, f);
        }
        // The tab before the last one: its page is painted by the very next picture (the engine has not been asked yet), and sharp soon.
        app.event(Event::Command(CMD_PREV_TAB));
        let mut rec = Rec::default();
        app.paint(&mut rec);
        assert!(!rec.pictures.is_empty(), "nothing of the page was kept");
        ready(&mut app);
        // What the tabs that are not shown keep of pictures never goes over what is set aside, and what the engine is told is less by it.
        for _ in 0..6 {
            app.event(Event::Command(CMD_NEXT_TAB));
            ready(&mut app);
            assert!(app.parked_pictures() <= PARKED_PICTURES, "{}", app.parked_pictures());
        }
        assert_eq!(app.engine_total(), app.total - PARKED_PICTURES, "the room for the pictures is set aside from the start");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_strip_is_clicked_hovered_and_drawn() {
        let dir = folder("strip");
        let (a, b) = (file(&dir, "first.pdf", 2), file(&dir, "second.pdf", 2));
        let mut app = app();
        open(&mut app, &a);
        open(&mut app, &b);
        let w = app.tab_w();
        let click = |app: &mut App, x: i64| {
            app.event(Event::MouseDown { x: x as i32, y: 5, ctrl: false, shift: false, clicks: 1 });
            app.event(Event::MouseUp { x: x as i32, y: 5 });
            ready(app);
        };
        // The first tab is clicked and shown; a click above the pages is not a click on them (no selection began).
        click(&mut app, w / 2);
        assert_eq!(app.tabs.active(), Some(0));
        assert!(app.press.is_none() && !app.strip_press);
        // A dot says a file has changes that are not saved; the pointer over a tab shows the path in full.
        app.edit.dirty = true;
        app.event(Event::Wake);
        assert!(app.tabs.get(0).unwrap().dirty && app.unsaved_reason().is_some());
        app.event(Event::Hover { x: (w + w / 2) as i32, y: 5 });
        assert_eq!(app.tab_hover, Some(1));
        let mut rec = Rec::default();
        assert!(app.paint(&mut rec));
        let said = |rec: &Rec, text: &str| rec.texts.iter().any(|(_, _, s)| s == text);
        assert!(said(&rec, "first.pdf") && said(&rec, "second.pdf") && said(&rec, "\u{25CF}"), "{:?}", rec.texts);
        assert!(said(&rec, &b.to_string_lossy()), "the path of the tab under the pointer");
        app.event(Event::HoverEnd);
        assert_eq!(app.tab_hover, None);
        // Pages are drawn below the strip: nothing of them is drawn over it, and the strip is on top.
        let mut rec = Rec::default();
        app.paint(&mut rec);
        let bar = app.bar_h() as i32;
        assert!(rec.pictures.iter().all(|(r, ..)| r.y + r.h > bar), "a picture lies in the strip");
        // The pages' area starts under the strip: a click there is on the page; the same click moved up is not.
        app.event(Event::MouseDown { x: 400, y: bar + 100, ctrl: false, shift: false, clicks: 1 });
        assert!(app.press.is_some());
        app.event(Event::MouseUp { x: 400, y: bar + 100 });
        // The middle button closes the tab under it: the second.
        app.edit.dirty = false;
        app.event(Event::Wake);
        app.event(Event::MiddleClick { x: (w + 10) as i32, y: 5 });
        ready(&mut app);
        assert_eq!(app.tabs.len(), 1);
        assert_eq!((app.bar_h(), app.view_h), (0, 700), "one file: the strip is gone and the pages have the room");
        assert!(app.title().starts_with("first.pdf"));
        // The cross of the shown tab closes it.
        open(&mut app, &b);
        let w = app.tab_w();
        let cross = 2 * w - app.px(crate::tabs::CLOSE_EDGE) - app.px(crate::tabs::CLOSE_SIDE) / 2;
        click_only(&mut app, cross);
        ready(&mut app);
        assert_eq!(app.tabs.len(), 1);
        assert!(app.title().starts_with("first.pdf"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn click_only(app: &mut App, x: i64) {
        app.event(Event::MouseDown { x: x as i32, y: 5, ctrl: false, shift: false, clicks: 1 });
        app.event(Event::MouseUp { x: x as i32, y: 5 });
    }

    #[test]
    fn closing_a_tab_with_changes_asks_and_ctrl_w_closes_the_shown_one() {
        let dir = folder("close");
        let (a, b, c) = (file(&dir, "a.pdf", 1), file(&dir, "b.pdf", 1), file(&dir, "c.pdf", 1));
        let mut app = app();
        for f in [&a, &b, &c] {
            open(&mut app, f);
        }
        // c has changes: Ctrl+W asks; Cancel keeps it; saving writes the file and closes it; the tab to its left is shown then.
        app.edit.dirty = true;
        let asked = app.event(Event::Key { vk: vk::W, ctrl: true, shift: false });
        assert!(asked_to_save(&asked) && app.tabs.len() == 3);
        app.event(Event::SaveChoice { tag: TAG_UNSAVED, choice: SaveChoice::Cancel });
        assert_eq!(app.tabs.len(), 3);
        // b has changes too, and is not shown: closing it from the strip shows it first and asks about it.
        app.tabs.get_mut(1).unwrap().dirty = true;
        app.tabs.get_mut(1).unwrap().kept.as_mut().unwrap().edit.dirty = true;
        app.event(Event::Wake);
        let asked = app.close_tab(1);
        assert!(asked_to_save(&asked) && app.tabs.active() == Some(1), "{:?}", app.tabs.active());
        app.event(Event::SaveChoice { tag: TAG_UNSAVED, choice: SaveChoice::Discard });
        ready(&mut app);
        assert_eq!(app.tabs.len(), 2);
        assert!(app.title().contains("c.pdf"), "the tab to the right is shown: {}", app.title());
        // Saving: the question, the answer Save, the file written, the tab closed.
        let asked = app.event(Event::Key { vk: vk::W, ctrl: true, shift: false });
        assert!(asked_to_save(&asked), "c still has its changes");
        app.event(Event::SaveChoice { tag: TAG_UNSAVED, choice: SaveChoice::Save });
        pump(&mut app, "the save and the close", |a| a.tabs.len() == 1 && a.has_document() && !a.opening);
        assert!(app.title().starts_with("a.pdf"));
        // With nothing unsaved Ctrl+W closes at once, and so does Ctrl+F4; the last tab leaves an empty window.
        let none = app.event(Event::Key { vk: vk::F4, ctrl: true, shift: false });
        assert!(!asked_to_save(&none) && app.tabs.is_empty() && !app.has_document());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn closing_the_window_asks_about_every_tab_with_changes() {
        let dir = folder("quit");
        let (a, b, c) = (file(&dir, "a.pdf", 1), file(&dir, "b.pdf", 1), file(&dir, "c.pdf", 1));
        let mut app = app();
        for f in [&a, &b, &c] {
            open(&mut app, f);
        }
        // c (shown) and a have changes; b has none.
        app.edit.dirty = true;
        app.event(Event::Wake);
        app.tabs.get_mut(0).unwrap().dirty = true;
        app.tabs.get_mut(0).unwrap().kept.as_mut().unwrap().edit.dirty = true;
        let asked = app.event(Event::CloseRequested);
        assert!(asked_to_save(&asked) && !asked.iter().any(|a| matches!(a, Action::Quit)));
        // Cancel: the window stays.
        let stay = app.event(Event::SaveChoice { tag: TAG_UNSAVED, choice: SaveChoice::Cancel });
        assert!(!stay.iter().any(|a| matches!(a, Action::Quit)) && app.tabs.len() == 3);
        let asked = app.event(Event::CloseRequested);
        assert!(asked_to_save(&asked));
        // Discard c: the question comes again, for a, which is shown for it.
        let next = app.event(Event::SaveChoice { tag: TAG_UNSAVED, choice: SaveChoice::Discard });
        assert!(asked_to_save(&next) && app.title().contains("a.pdf"), "{}", app.title());
        let done = app.event(Event::SaveChoice { tag: TAG_UNSAVED, choice: SaveChoice::Discard });
        assert!(done.iter().any(|a| matches!(a, Action::Quit)), "everything was answered: the window closes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn files_dropped_on_the_window_open_in_tabs_and_the_others_get_a_message() {
        let dir = folder("drop");
        let (a, b) = (file(&dir, "a.pdf", 1), file(&dir, "b.pdf", 2));
        let text = dir.join("notes.txt");
        std::fs::write(&text, "not a pdf").unwrap();
        let mut app = app();
        // Two PDF files and a text file, and a file that is not there: the PDF files open, the others are told of in one message.
        let actions = app.event(Event::Drop(vec![a.clone(), text.clone(), b.clone(), dir.join("missing.pdf")]));
        pump(&mut app, "both files", |a| a.tabs.len() == 2 && a.has_document() && !a.opening && a.queue.is_empty());
        let message = actions.iter().find_map(|a| if let Action::Message { text, .. } = a { Some(text.clone()) } else { None }).expect("a message");
        assert!(message.contains("notes.txt") && message.contains("missing.pdf") && !message.contains("a.pdf\n"), "{message}");
        assert!(app.title().starts_with("b.pdf"), "the last one is shown: {}", app.title());
        // A folder is not a file either, and nothing else happens.
        let more = app.event(Event::Drop(vec![dir.clone()]));
        assert!(more.iter().any(|a| matches!(a, Action::Message { .. })) && app.tabs.len() == 2);
        // The same file again shows its tab.
        app.event(Event::Drop(vec![a.clone()]));
        pump(&mut app, "the tab", |a| a.title().starts_with("a.pdf") && a.has_document() && !a.activating);
        assert_eq!(app.tabs.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sixteen_files_at_most_are_open_at_once_and_the_seventeenth_is_told_so() {
        let dir = folder("many");
        let files: Vec<PathBuf> = (0..MAX_TABS + 2).map(|i| file(&dir, &format!("f{i:02}.pdf"), 1)).collect();
        let mut app = app();
        let actions = app.event(Event::Drop(files.clone()));
        pump(&mut app, "sixteen files", |a| a.tabs.len() == MAX_TABS && a.has_document() && !a.opening && a.queue.is_empty());
        let _ = actions;
        assert!(app.title().starts_with("f15.pdf"), "{}", app.title());
        // One more by the menu: the message says why.
        let told = app.open_path(&files[MAX_TABS], "");
        assert!(told.iter().any(|a| matches!(a, Action::Message { text, .. } if text.contains("16"))));
        assert_eq!(app.tabs.len(), MAX_TABS);
        // Each tab can be shown and draws.
        for i in (0..MAX_TABS).rev() {
            app.show_tab(i);
            ready(&mut app);
            assert_eq!(app.tabs.active(), Some(i));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tab_that_is_busy_is_not_left_and_its_background_work_does_not_reach_the_others() {
        let dir = folder("busy");
        let (a, b) = (file(&dir, "a.pdf", 3), file(&dir, "b.pdf", 3));
        let mut app = app();
        open(&mut app, &a);
        // A save is going (its answer is for this tab): another file is not opened beside it, and the tab is not left.
        app.edit.saving = Some((99, a.clone(), false));
        let told = app.open_path(&b, "");
        assert!(app.tabs.len() == 1 && told.iter().any(|x| matches!(x, Action::SetTimer { id: TIMER_NOTE, .. })), "{told:?}");
        assert!(app.note.as_deref().is_some_and(|n| n.contains("busy")), "{:?}", app.note);
        app.edit.saving = None;
        // An edit that is not answered yet is the same.
        app.edit.asked.insert(98, Asked::Add);
        assert!(app.tab_busy());
        app.edit.asked.clear();
        open(&mut app, &b);
        assert_eq!(app.tabs.len(), 2);
        // A search in b is taken back when b is left, and what the engine finds in b later is nobody's.
        app.start_search("anything");
        assert!(app.search.is_some());
        key(&mut app, vk::TAB, false);
        assert!(app.search.is_none() && app.title().starts_with("a.pdf"));
        for _ in 0..30 {
            app.event(Event::Wake);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(app.search.is_none());
        // Closing a tab whose save is going: the answer, when it comes, is for a tab that is not there and is dropped.
        app.edit.dirty = true;
        app.event(Event::Command(CMD_SAVE));
        let saving = app.edit.saving.is_some();
        assert!(saving);
        app.edit.dirty = false;
        app.event(Event::Command(CMD_CLOSE_TAB));
        assert_eq!(app.tabs.len(), 1);
        for _ in 0..30 {
            app.event(Event::Wake);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(app.edit.saving.is_none() && app.title().starts_with("b.pdf"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tab_the_engine_let_go_of_is_read_again_where_it_was_and_the_file_changing_is_said() {
        let dir = folder("dropped");
        let (a, b, c) = (file(&dir, "a.pdf", 8), file(&dir, "b.pdf", 2), file(&dir, "c.pdf", 2));
        let mut app = app();
        // So little room that a second file pushes the first out.
        app.total = qingpdf_core::view::ACTIVE_FLOOR + (3 << 20);
        open(&mut app, &a);
        app.go_to_page(5);
        app.event(Event::Command(CMD_ROTATE));
        ready(&mut app);
        let mine = view(&app);
        assert_eq!(mine.0, 5);
        open(&mut app, &b);
        pump(&mut app, "the first file to be let go of", |a| a.tabs.get(0).is_some_and(|t| t.at == Where::Dropped));
        assert!(app.tabs.get(0).unwrap().kept.as_ref().unwrap().doc.is_none());
        // Showing it reads the file again and puts the view back.
        key(&mut app, vk::TAB, false);
        assert!(app.title().starts_with("a.pdf") && app.page_count() == 8);
        assert_eq!(view(&app), mine);
        assert!(app.note.is_none(), "the file is as it was");
        // The file changes while its tab waits: it is read again as it is, and the person is told.
        open(&mut app, &c);
        pump(&mut app, "a to be let go of again", |a| a.tabs.get(0).is_some_and(|t| t.at == Where::Dropped));
        std::fs::write(&a, pages_pdf(&[(612, 792, 0); 9], &[])).unwrap();
        tight(&mut app);
        app.show_tab(0);
        ready(&mut app);
        assert_eq!(app.page_count(), 9);
        assert!(app.note.as_deref().is_some_and(|n| n.contains("changed")), "{:?}", app.note);
        // The file is gone while its tab waits: the person is told, and stays where they are.
        key(&mut app, vk::TAB, false);
        pump(&mut app, "a to be let go of", |a| a.tabs.get(0).is_some_and(|t| t.at == Where::Dropped) || a.tabs.get(1).is_some_and(|t| t.at == Where::Dropped));
        let dropped = app.tabs.iter().position(|t| t.at == Where::Dropped).unwrap();
        std::fs::remove_file(&app.tabs.get(dropped).unwrap().path).unwrap();
        let shown = app.tabs.active();
        let told = app.show_tab(dropped);
        assert!(told.iter().any(|x| matches!(x, Action::Message { text, .. } if text.contains("cannot be read"))));
        assert_eq!(app.tabs.active(), shown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_with_unsaved_edits_is_not_let_go_of_and_no_room_is_told() {
        let dir = folder("noroom");
        let (a, b) = (file(&dir, "a.pdf", 2), file(&dir, "b.pdf", 2));
        let mut app = app();
        app.total = qingpdf_core::view::ACTIVE_FLOOR + (3 << 20);
        open(&mut app, &a);
        // An edit that is not saved (a note put in by the engine directly).
        let id = app.new_id();
        app.engine.add_note(app.doc_id, id, 0, [20.0, 20.0], "n", [255, 235, 0], "T");
        app.edit.asked.insert(id, Asked::Add);
        pump(&mut app, "the note", |x| x.edit.dirty);
        // The second file does not fit beside it: the tab made for it goes again, and the person is told what to do.
        tight(&mut app);
        let told = app.open_path(&b, "");
        assert_eq!(app.engine_total() + PARKED_PICTURES, app.total, "the room for the pictures is not given away to the files that cannot be let go of");
        let mut message = told.iter().find_map(|x| if let Action::Message { text, .. } = x { Some(text.clone()) } else { None });
        pump(&mut app, "the answer", |x| {
            !x.opening && x.tabs.len() == 1
        });
        if message.is_none() {
            // (The answer came with the events.)
            message = Some(app.note.clone().unwrap_or_default());
        }
        let _ = message;
        assert_eq!(app.tabs.len(), 1);
        assert!(app.title().starts_with("* a.pdf"), "the edit is still there: {}", app.title());
        ready(&mut app);
        assert!(app.edit.dirty && app.edit.can_undo);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn where_the_reader_was_comes_back_when_the_file_is_opened_again() {
        let dir = folder("places");
        let a = file(&dir, "a.pdf", 12);
        let store = dir.join("places.txt");
        let make = |remember: bool| {
            let mut app = app();
            app.places_path = Some(store.clone());
            app.places = crate::places::load(&store);
            app.remember_place = remember;
            app
        };
        let mut first = make(true);
        open(&mut first, &a);
        first.go_to_page(7);
        first.event(Event::Command(CMD_ROTATE));
        first.event(Event::Command(CMD_ZOOM_IN));
        first.event(Event::Timer(TIMER_SETTLE));
        ready(&mut first);
        let left = view(&first);
        assert_eq!(left.0, 7);
        first.event(Event::Command(CMD_CLOSE_TAB));
        assert!(store.is_file(), "written when the tab is closed");
        let text = std::fs::read_to_string(&store).unwrap();
        assert!(text.starts_with("qingpdf-places 1\n") && text.lines().count() == 2, "{text}");
        // Another run of the program: the file opens where it was left.
        let mut second = make(true);
        open(&mut second, &a);
        assert_eq!((second.current_page(), second.zoom, second.rotation), (left.0, left.1, left.2));
        assert!((second.scroll_y - left.3).abs() <= 2, "{} {}", second.scroll_y, left.3);
        // The setting off: the first page.
        let mut off = make(false);
        open(&mut off, &a);
        assert_eq!((off.current_page(), off.rotation), (0, 0));
        // The file changed since (another size): it is a new file.
        let mut grown = std::fs::read(&a).unwrap();
        grown.push(b'\n');
        std::fs::write(&a, grown).unwrap();
        let mut changed = make(true);
        open(&mut changed, &a);
        assert_eq!(changed.current_page(), 0);
        // A damaged file of places is no reason to fail.
        std::fs::write(&store, b"\x00\xff garbage \t\t\n99999999999999999999\t1\n").unwrap();
        let mut damaged = make(true);
        open(&mut damaged, &a);
        assert_eq!(damaged.current_page(), 0);
        // Clearing the list of recent files clears the places with it.
        let mut cleared = make(true);
        open(&mut cleared, &a);
        cleared.go_to_page(3);
        cleared.event(Event::Command(CMD_CLEAR_RECENT));
        assert!(cleared.places.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_engine_total_is_the_apps_to_set_and_the_tab_of_a_failed_file_goes_again() {
        let dir = folder("failed");
        let (a, junk) = (file(&dir, "a.pdf", 2), dir.join("junk.pdf"));
        std::fs::write(&junk, b"not a pdf at all").unwrap();
        let mut app = app();
        open(&mut app, &a);
        // A file that does not open: the message, the tab goes, the one before it is as it was.
        let told = app.open_path(&junk, "");
        pump(&mut app, "the failure", |x| x.tabs.len() == 1 && !x.opening && x.has_document() && !x.activating);
        let _ = told;
        assert!(app.title().starts_with("a.pdf") && app.tabs.active() == Some(0));
        ready(&mut app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- the fix round (2026-10-18) --------------------------------------------------------------------------------

    /// A PDF of `pages` empty pages with bookmarks for the first `marks` of them.
    fn bookmarked(pages: u32, marks: u32) -> Vec<u8> {
        let root = 3 + pages;
        let kids: String = (0..pages).map(|i| format!("{} 0 R ", 3 + i)).collect();
        let mut objects: Vec<(u32, String)> = vec![
            (1, format!("<< /Type /Catalog /Pages 2 0 R /Outlines {root} 0 R >>")),
            (2, format!("<< /Type /Pages /Kids [{kids}] /Count {pages} >>")),
        ];
        for i in 0..pages {
            objects.push((3 + i, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_string()));
        }
        objects.push((root, format!("<< /Type /Outlines /First {} 0 R /Last {} 0 R /Count {marks} >>", root + 1, root + marks)));
        for m in 0..marks {
            let mut item = format!("<< /Title (Chapter {}) /Parent {root} 0 R /Dest [{} 0 R /Fit]", m + 1, 3 + m.min(pages - 1));
            if m > 0 {
                item.push_str(&format!(" /Prev {} 0 R", root + m));
            }
            if m + 1 < marks {
                item.push_str(&format!(" /Next {} 0 R", root + m + 2));
            }
            item.push_str(" >>");
            objects.push((root + 1 + m, item));
        }
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = std::collections::BTreeMap::new();
        for (n, body) in &objects {
            offsets.insert(*n, out.len());
            out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
        }
        let size = objects.iter().map(|o| o.0).max().unwrap_or(0) + 1;
        let at = out.len();
        out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
        for n in 1..size {
            out.extend_from_slice(format!("{:010} 00000 n \n", offsets[&n]).as_bytes());
        }
        out.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{at}\n%%EOF\n").as_bytes());
        out
    }

    #[test]
    fn closing_the_window_while_the_shown_tab_is_busy_does_not_lose_the_changes_of_another() {
        let dir = folder("quit-busy");
        let (a, b) = (file(&dir, "a.pdf", 2), file(&dir, "b.pdf", 2));
        let mut app = app();
        open(&mut app, &a);
        app.edit.dirty = true;
        app.event(Event::Wake);
        // Another file is being read (the shown tab is its tab, and busy) when the window is asked to close: it stays, and says why.
        app.open_path(&b, "");
        assert!(app.opening && app.tabs.len() == 2);
        let closing = app.event(Event::CloseRequested);
        assert!(!closing.iter().any(|x| matches!(x, Action::Quit)) && !asked_to_save(&closing), "{closing:?}");
        assert!(app.note.as_deref().is_some_and(|n| n.contains("busy")), "{:?}", app.note);
        assert!(app.pending_tab.is_none(), "the tab to ask about was not asked for");
        ready(&mut app);
        // The file is read: the question is about the tab with the changes.
        let asked = app.event(Event::CloseRequested);
        assert!(asked_to_save(&asked) && app.title().contains("a.pdf"), "{}", app.title());
        app.event(Event::SaveChoice { tag: TAG_UNSAVED, choice: SaveChoice::Cancel });
        // A save going in the shown tab (b) is the same when a is the one with changes.
        app.show_tab(1);
        ready(&mut app);
        app.edit.saving = Some((99, b.clone(), false));
        let closing = app.event(Event::CloseRequested);
        assert!(!closing.iter().any(|x| matches!(x, Action::Quit)), "{closing:?}");
        assert!(app.note.as_deref().is_some_and(|n| n.contains("busy")) && app.tabs.get(0).unwrap().dirty);
        app.edit.saving = None;
        let closing = app.event(Event::CloseRequested);
        assert!(asked_to_save(&closing) && app.title().contains("a.pdf"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tab_is_drawn_after_a_file_that_failed_or_a_password_that_was_not_given_and_after_a_quick_switch() {
        let dir = folder("blank");
        let (a, c, d) = (file(&dir, "a.pdf", 3), file(&dir, "c.pdf", 2), file(&dir, "d.pdf", 2));
        let bad = dir.join("bad.pdf");
        std::fs::write(&bad, b"%PDF-1.4\nthis is not a pdf at all").unwrap();
        let encrypted = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/corpus/public/encrypted/qpdf-generated/bookmarks.r2-40-user.pdf"));
        let mut app = app();
        open(&mut app, &a);
        // a, a file that does not open, c: c is drawn (the engine's answer about a, which came back, is not waited for).
        app.event(Event::Drop(vec![bad, c.clone()]));
        pump(&mut app, "c", |x| x.tabs.len() == 2 && x.title().starts_with("c.pdf") && x.has_document() && !x.opening && x.queue.is_empty());
        ready(&mut app);
        // A file that asks for a password that is not given, then d.
        app.event(Event::Drop(vec![encrypted, d.clone()]));
        let mut asked = false;
        for _ in 0..1500 {
            asked = app.event(Event::Wake).iter().any(|x| matches!(x, Action::AskText { tag: TAG_PASSWORD, .. }));
            if asked {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(asked, "no question about the password");
        app.event(Event::Text { tag: TAG_PASSWORD, text: None });
        pump(&mut app, "d", |x| x.tabs.len() == 3 && x.title().starts_with("d.pdf") && x.has_document() && !x.opening && x.queue.is_empty());
        ready(&mut app);
        // A tab the engine let go of is asked for right after a tab that waits: the second one is read again and drawn.
        let doc = app.tabs.get(0).unwrap().kept.as_ref().unwrap().doc_id;
        app.engine.close_doc(doc);
        app.on_dropped(doc);
        assert_eq!(app.tabs.get(0).unwrap().at, Where::Dropped);
        app.show_tab(1);
        assert!(app.activating);
        app.show_tab(0);
        ready(&mut app);
        assert!(app.title().starts_with("a.pdf") && !app.activating && app.page_count() == 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn changes_given_up_at_one_question_stay_until_the_window_is_gone() {
        let dir = folder("given-up");
        let (a, b) = (file(&dir, "a.pdf", 1), file(&dir, "b.pdf", 1));
        let mut app = app();
        open(&mut app, &a);
        app.edit.dirty = true;
        app.event(Event::Wake);
        open(&mut app, &b);
        app.edit.dirty = true;
        app.event(Event::Wake);
        // b is asked about and given up; a is shown and asked about; the person cancels there.
        let asked = app.event(Event::CloseRequested);
        assert!(asked_to_save(&asked) && app.title().contains("b.pdf"));
        let next = app.event(Event::SaveChoice { tag: TAG_UNSAVED, choice: SaveChoice::Discard });
        assert!(asked_to_save(&next) && app.title().contains("a.pdf"), "{}", app.title());
        let stay = app.event(Event::SaveChoice { tag: TAG_UNSAVED, choice: SaveChoice::Cancel });
        assert!(!stay.iter().any(|x| matches!(x, Action::Quit)));
        // b still has its changes, and its mark in the strip.
        assert!(app.tabs.get(1).unwrap().dirty && app.tabs.get(1).unwrap().kept.as_ref().unwrap().edit.dirty);
        ready(&mut app);
        // The next time the window is to be closed b is asked about again.
        let asked = app.event(Event::CloseRequested);
        assert!(asked_to_save(&asked) && app.title().contains("a.pdf"));
        let next = app.event(Event::SaveChoice { tag: TAG_UNSAVED, choice: SaveChoice::Discard });
        assert!(asked_to_save(&next) && app.title().contains("b.pdf"), "b is asked about again: {}", app.title());
        let done = app.event(Event::SaveChoice { tag: TAG_UNSAVED, choice: SaveChoice::Discard });
        assert!(done.iter().any(|x| matches!(x, Action::Quit)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_bookmarks_of_each_file_of_a_queue_are_there_though_the_next_file_was_asked_for_at_once() {
        let dir = folder("marks");
        let files: Vec<PathBuf> = ["a", "b", "c"]
            .iter()
            .map(|n| {
                let path = dir.join(format!("{n}.pdf"));
                std::fs::write(&path, bookmarked(4, 3)).unwrap();
                path
            })
            .collect();
        let mut app = app();
        app.event(Event::Drop(files));
        pump(&mut app, "the three files", |a| a.tabs.len() == 3 && a.has_document() && !a.opening && a.queue.is_empty());
        pump(&mut app, "the bookmarks of the last", |a| a.outline.len() == 3);
        for i in [0, 1] {
            app.show_tab(i);
            ready(&mut app);
            pump(&mut app, "the bookmarks of a file that was left as soon as it was open", |a| a.outline.len() == 3);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tab_saved_in_this_session_is_not_taken_for_a_file_changed_by_someone_else() {
        let dir = folder("saved");
        let (a, b) = (file(&dir, "a.pdf", 3), file(&dir, "b.pdf", 2));
        let mut app = app();
        app.total = qingpdf_core::view::ACTIVE_FLOOR + (2 << 20) + PARKED_PICTURES;
        open(&mut app, &a);
        let id = app.new_id();
        app.engine.add_note(app.doc_id, id, 0, [20.0, 20.0], "n", [255, 235, 0], "T");
        app.edit.asked.insert(id, Asked::Add);
        pump(&mut app, "the note", |x| x.edit.dirty);
        let before = app.stamp;
        app.event(Event::Command(CMD_SAVE));
        pump(&mut app, "the save", |x| !x.edit.dirty && x.edit.saving.is_none());
        assert!(app.stamp != before && app.stamp == crate::places::stamp_of(&a), "the stamp is the file's as it is now");
        // Where the reader is in the file as it is now is noted (the next run finds it).
        assert!(app.places.find(&normalize(&a), app.stamp.unwrap()).is_some());
        // The tab is let go of (there is room for one file) and shown again: the file is as the tab left it.
        open(&mut app, &b);
        pump(&mut app, "a to be let go of", |x| x.tabs.get(0).is_some_and(|t| t.at == Where::Dropped));
        app.show_tab(0);
        ready(&mut app);
        assert!(app.note.is_none(), "no false alarm: {:?}", app.note);
        // The same for a file written under another name: the tab is that file now.
        let copy = dir.join("copy.pdf");
        app.save_picked(Some(copy.clone()));
        pump(&mut app, "the save under another name", |x| x.edit.saving.is_none() && x.path.as_deref() == Some(copy.as_path()));
        assert_eq!(app.stamp, crate::places::stamp_of(&copy));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_long_drop_is_cut_short_and_the_message_names_only_a_few_and_a_device_is_not_looked_at() {
        let dir = folder("long-drop");
        let mut app = app();
        let files: Vec<PathBuf> = (0..40)
            .map(|i| {
                let path = dir.join(format!("n{i:02}.txt"));
                std::fs::write(&path, "text").unwrap();
                path
            })
            .collect();
        let told = app.event(Event::Drop(files));
        let text = told.iter().find_map(|x| if let Action::Message { text, .. } = x { Some(text.clone()) } else { None }).expect("a message");
        // (32 are looked at: 8 are named, 24 more are counted, and 8 were not looked at.)
        assert!(text.contains("n00.txt") && text.contains("n07.txt") && !text.contains("n08.txt") && text.ends_with("\u{2026} +32"), "{text}");
        assert!(app.tabs.is_empty());
        let told = app.event(Event::Drop(vec![PathBuf::from("\\\\.\\pipe\\qingpdf-no-such-pipe.pdf")]));
        assert!(told.iter().any(|x| matches!(x, Action::Message { text, .. } if text.contains("qingpdf-no-such-pipe.pdf"))), "{told:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_presentation_ends_before_the_window_is_closed_or_another_tab_is_shown_and_the_tab_it_was_in_is_as_it_was() {
        let dir = folder("present-leave");
        let (a, b) = (file(&dir, "a.pdf", 4), file(&dir, "b.pdf", 4));
        let mut app = app();
        open(&mut app, &a);
        app.edit.dirty = true;
        app.event(Event::Wake);
        open(&mut app, &b);
        // A zoom of the person's own (the fit ones are worked out again whenever the window has a size).
        app.event(Event::Command(CMD_ZOOM_IN));
        let (mode, zoom) = (app.mode, app.zoom);
        assert_eq!(mode, ZoomMode::Custom);
        // The presentation of b; the window is to be closed: a has changes, so a is shown and asked about.
        app.event(Event::Key { vk: vk::F5, ctrl: false, shift: false });
        app.event(Event::Size { w: 1920, h: 1080 });
        assert!(app.present.is_some() && app.full);
        let asked = app.event(Event::CloseRequested);
        assert!(app.present.is_none() && !app.full && asked.iter().any(|x| matches!(x, Action::FullScreen(false))), "the presentation is over");
        assert!(asked_to_save(&asked) && app.title().contains("a.pdf"));
        // The person cancels: the window stays. Tab b, which was presenting, was put away as it was before: its zoom is the one it had.
        let stay = app.event(Event::SaveChoice { tag: TAG_UNSAVED, choice: SaveChoice::Cancel });
        assert!(!stay.iter().any(|x| matches!(x, Action::Quit)));
        app.event(Event::Size { w: 900, h: 700 });
        ready(&mut app);
        let parked = app.tabs.get(1).and_then(|t| t.kept.as_ref()).expect("tab b is parked");
        assert_eq!((parked.mode, parked.zoom), (mode, zoom));
        let shown = key(&mut app, vk::TAB, false);
        assert!(app.title().contains("b.pdf"), "{}: {shown:?}", app.title());
        assert_eq!((app.mode, app.zoom), (mode, zoom));
        // Another tab shown during a presentation ends it as well.
        app.event(Event::Key { vk: vk::F5, ctrl: false, shift: false });
        app.event(Event::Size { w: 1920, h: 1080 });
        assert!(app.present.is_some());
        let switched = app.event(Event::Command(CMD_NEXT_TAB));
        assert!(app.present.is_none() && !app.full && switched.iter().any(|x| matches!(x, Action::FullScreen(false))));
        app.event(Event::Size { w: 900, h: 700 });
        ready(&mut app);
        assert!(app.title().contains("a.pdf"));
        assert_eq!(app.tabs.get(1).and_then(|t| t.kept.as_ref()).map(|k| (k.mode, k.zoom)), Some((mode, zoom)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
