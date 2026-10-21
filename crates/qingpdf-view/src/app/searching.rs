//! The find box and the search (3d-2): what is typed, when to look, the places found and moving between them.

use qingpdf_core::view::{SearchHit, SearchStatus};

use super::*;

impl App {
    /// The find box (the edit control) in the window: at the top right of the pages. With the toolbar the box is the toolbar's, and
    /// only the count is shown over the pages: the rectangle has no width then.
    pub(super) fn find_rect(&self) -> Rect {
        let (w, h, m) = (if self.toolbar_visible() { 0 } else { self.px(FIND_EDIT_W) }, self.px(FIND_EDIT_H), self.px(FIND_MARGIN));
        Rect { x: clamp32((self.view_x + self.view_w - w - m).max(self.view_x)), y: clamp32(m), w: clamp32(w), h: clamp32(h) }
    }

    pub(super) fn open_find(&mut self) -> Vec<Action> {
        if self.doc.is_none() {
            return Vec::new();
        }
        self.find_open = true;
        let mut actions = vec![Action::Find(Some(self.find_rect())), Action::Invalidate];
        // Words left in the box from before are looked for again.
        if self.search.is_none() && !self.find_text.trim().is_empty() {
            actions.extend(self.start_search(&self.find_text.clone()));
        }
        actions
    }

    pub(super) fn close_find(&mut self) -> Vec<Action> {
        self.find_open = false;
        if let Some(s) = self.search.take() {
            self.engine.cancel(s.id);
        }
        vec![Action::KillTimer(TIMER_FIND), Action::Find(None), Action::Invalidate]
    }

    /// Esc: stops a print, else a Word export, else text recognition, else closes the find box, else lets go of the selection.
    pub(super) fn escape(&mut self) -> Vec<Action> {
        if let Some(actions) = self.cancel_thumb_drag() {
            return actions;
        }
        if self.print.is_some() {
            let mut actions = self.cancel_print_for_new_document();
            actions.extend(self.say(t(self.lang, Msg::NotePrintCancelled).to_string()));
            return actions;
        }
        if let Some(actions) = self.cancel_export() {
            return actions;
        }
        if let Some(actions) = self.cancel_compress() {
            return actions;
        }
        if let Some(actions) = self.cancel_pictures().or_else(|| self.cancel_crop_look()) {
            return actions;
        }
        if let Some(actions) = self.stop_ocr() {
            return actions;
        }
        if self.find_open {
            return self.close_find();
        }
        if let Some(actions) = self.escape_tool() {
            return actions;
        }
        if self.selection.take().is_some() {
            return vec![Action::Invalidate];
        }
        // Nothing else wanted Esc: full screen is over.
        self.escape_full()
    }

    /// The text of the find box changed: look for it when it has been left alone for a moment.
    pub(super) fn find_text_changed(&mut self, text: String) -> Vec<Action> {
        self.find_text = text;
        if self.find_text.trim().is_empty() {
            if let Some(s) = self.search.take() {
                self.engine.cancel(s.id);
            }
            return vec![Action::KillTimer(TIMER_FIND), Action::Invalidate];
        }
        vec![Action::SetTimer { id: TIMER_FIND, ms: FIND_MS }]
    }

    pub(super) fn find_timer(&mut self) -> Vec<Action> {
        let mut actions = vec![Action::KillTimer(TIMER_FIND)];
        if self.doc.is_some() && !self.find_text.trim().is_empty() && self.search.as_ref().is_none_or(|s| s.query != self.find_text) {
            actions.extend(self.start_search(&self.find_text.clone()));
        }
        actions
    }

    /// Enter: look for the words now if they are new, else go on to the next place (Shift: the one before).
    pub(super) fn find_enter(&mut self, shift: bool) -> Vec<Action> {
        if !self.find_open {
            return Vec::new();
        }
        if self.search.as_ref().is_none_or(|s| s.query != self.find_text) && !self.find_text.trim().is_empty() {
            let mut actions = vec![Action::KillTimer(TIMER_FIND)];
            actions.extend(self.start_search(&self.find_text.clone()));
            return actions;
        }
        self.find_step(!shift)
    }

    /// F3: the next place found (`forward`), or the one before.
    pub(super) fn find_step(&mut self, forward: bool) -> Vec<Action> {
        if self.doc.is_none() {
            return Vec::new();
        }
        let Some(s) = self.search.as_mut() else {
            // Nothing searched for yet: the box opens (and looks for what it holds).
            return self.open_find();
        };
        if forward { s.next() } else { s.prev() };
        let mut actions = self.reveal_current_hit();
        actions.push(Action::Invalidate);
        actions
    }

    fn reveal_current_hit(&mut self) -> Vec<Action> {
        let Some(s) = &self.search else { return Vec::new() };
        let (Some(c), Some(rect)) = (s.current, s.current_hit().and_then(|h| h.rects.first().copied())) else { return Vec::new() };
        self.reveal(c.page, rect)
    }

    pub(super) fn start_search(&mut self, query: &str) -> Vec<Action> {
        if let Some(old) = self.search.take() {
            self.engine.cancel(old.id);
        }
        if self.doc.is_none() || query.trim().is_empty() {
            return vec![Action::Invalidate];
        }
        let id = self.new_id();
        // From the first page in view, so that the first place found is the next one after where the person is.
        let start = self.visible().map_or(0, |v| v.0);
        self.engine.search(self.doc_id, id, query, start);
        self.search = Some(SearchState::new(id, query));
        vec![Action::Invalidate]
    }

    pub(super) fn on_search_hits(&mut self, id: u64, page: u32, hits: Vec<SearchHit>) -> Vec<Action> {
        let Some(s) = self.search.as_mut().filter(|s| s.id == id) else { return Vec::new() };
        let first = s.add(page, hits);
        let mut actions = vec![Action::Invalidate];
        if first {
            actions.extend(self.reveal_current_hit());
        }
        actions
    }

    pub(super) fn on_search_progress(&mut self, id: u64, done: u32, total: u32) -> Vec<Action> {
        let Some(s) = self.search.as_mut().filter(|s| s.id == id) else { return Vec::new() };
        (s.pages_done, s.pages_total) = (done, total);
        vec![Action::Invalidate]
    }

    pub(super) fn on_search_done(&mut self, id: u64, status: SearchStatus, skipped: u32) -> Vec<Action> {
        let Some(s) = self.search.as_mut().filter(|s| s.id == id) else { return Vec::new() };
        s.running = false;
        s.limit = status == SearchStatus::HitLimit;
        s.skipped = skipped;
        vec![Action::Invalidate]
    }

    /// The count beside the find box, drawn over the pages.
    pub(super) fn draw_find_panel(&self, p: &mut dyn Painter) {
        if !self.find_open {
            return;
        }
        let label = match &self.search {
            None => String::new(),
            Some(s) if s.total() == 0 => t(self.lang, if s.running { Msg::FindSearching } else { Msg::FindNone }).to_string(),
            Some(s) => {
                let progress = s.running.then_some((s.pages_done, s.pages_total));
                i18n::found(self.lang, s.current.map_or(0, |c| s.number_of(c)), s.total(), s.limit, progress)
            }
        };
        // With the toolbar the count is all there is to show, and there is nothing to show before a search.
        if label.is_empty() && self.toolbar_visible() {
            return;
        }
        let e = self.find_rect();
        // With the toolbar the panel is as wide as the count, and sits against the right side of the pages.
        let label_w = if self.toolbar_visible() { p.text_width(&label) } else { clamp32(self.px(FIND_LABEL_W)) };
        let pad = clamp32(self.px(8));
        let panel = Rect { x: (e.x - label_w - pad).max(clamp32(self.view_x)), y: e.y - pad / 2, w: e.w + label_w + 2 * pad, h: e.h + pad };
        p.fill_rect(Rect { x: panel.x - 1, y: panel.y - 1, w: panel.w + 2, h: panel.h + 2 }, self.pal.border);
        p.fill_rect(panel, self.pal.status_bg);
        p.text(panel.x + pad, e.y + (e.h - p.line_height()) / 2, &label, self.pal.status_fg);
    }
}
