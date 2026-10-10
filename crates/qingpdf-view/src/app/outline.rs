//! The reader's bookmarks, links and moving to a place (3d-2), and the engine's answers about them and about text.

use qingpdf_core::view::{Event as EngineEvent, Link, LinkKind, OutlineItem, safe_uri};

use super::*;
use crate::ui::OutlineNode;

/// Pages round the window whose links are kept.
const LINK_PAGES_KEPT: u32 = 12;
/// Most pages in view that links are asked for at once (a tiny zoom shows hundreds).
const LINK_PAGES_ASKED: u32 = 8;

impl App {
    /// The width of the bookmarks beside the pages (0 when there are none, or they are hidden).
    pub(super) fn sidebar_width(&self) -> i64 {
        if self.show_outline && !self.outline.is_empty() && self.bench.is_none() {
            self.px(SIDEBAR_WIDTH).min(self.client_w / 2)
        } else {
            0
        }
    }

    /// Where the tree of bookmarks and the find box go in the window now.
    pub(super) fn chrome(&self) -> Vec<Action> {
        let side = Rect { x: 0, y: 0, w: clamp32(self.view_x), h: clamp32(self.view_h) };
        let find = self.find_open.then(|| self.find_rect());
        vec![Action::Sidebar(side), Action::Find(find)]
    }

    /// The pages have to be laid out again because the bookmarks came, went or were shown or hidden.
    fn apply_sidebar(&mut self) -> Vec<Action> {
        let width = self.sidebar_width();
        let mut actions = Vec::new();
        if width != self.view_x {
            self.view_x = width;
            self.view_w = (self.client_w - width).max(0);
            let (ax, ay) = (self.view_w / 2, self.view_h / 2);
            self.relayout(ax, ay, None);
            actions.extend(self.after_view_change(false));
        }
        actions.extend(self.chrome());
        self.menus_dirty = true;
        actions
    }

    pub(super) fn toggle_outline(&mut self) -> Vec<Action> {
        self.show_outline = !self.show_outline;
        self.apply_sidebar()
    }

    pub(super) fn ask_for_outline(&mut self) -> Vec<Action> {
        let id = self.new_id();
        self.engine.outline(self.doc_id, id);
        Vec::new()
    }

    fn on_outline(&mut self, items: Vec<OutlineItem>) -> Vec<Action> {
        let untitled = t(self.lang, Msg::Untitled);
        let nodes: Vec<OutlineNode> = items
            .iter()
            .map(|i| OutlineNode { title: if i.title.trim().is_empty() { untitled.to_string() } else { i.title.clone() }, depth: i.depth, open: i.open })
            .collect();
        self.outline = items;
        let mut actions = vec![Action::Outline(nodes)];
        actions.extend(self.apply_sidebar());
        actions
    }

    pub(super) fn outline_click(&mut self, index: usize) -> Vec<Action> {
        let Some(item) = self.outline.get(index) else { return Vec::new() };
        match item.page {
            Some(page) => self.go_to_target(page, item.y),
            None => Vec::new(),
        }
    }

    /// Move so that a place of a page (`y` points down it, as the page is shown) is near the top of the window.
    pub(super) fn go_to_target(&mut self, page: u32, y: Option<f32>) -> Vec<Action> {
        let Some(doc) = &self.doc else { return Vec::new() };
        let page = (page as usize).min(doc.layout.len().saturating_sub(1));
        let offset = match (y, doc.layout.page_map(page)) {
            (Some(y), Some(map)) => {
                let (_, py) = map.to_px(0.0, f64::from(y));
                (py.round() as i64).clamp(0, doc.layout.size_px(page).1)
            }
            _ => 0,
        };
        let target = doc.layout.top(page).saturating_add(offset).saturating_sub(self.px(MARGIN) / 2);
        self.scroll_to(self.scroll_x, target, false)
    }

    /// Scroll so that a rectangle of a page (points, as the page is shown) is in view, if it is not.
    pub(super) fn reveal(&mut self, page: u32, rect: [f32; 4]) -> Vec<Action> {
        let Some(doc) = &self.doc else { return Vec::new() };
        let Some(map) = doc.layout.page_map(page as usize) else { return Vec::new() };
        let (x0, y0, x1, y1) = map.rect_to_px(rect);
        let top = doc.layout.top(page as usize).saturating_add(y0 as i64);
        let bottom = doc.layout.top(page as usize).saturating_add(y1.ceil() as i64);
        let left = doc.layout.left(page as usize, self.view_w, 0).saturating_add(x0 as i64);
        let right = doc.layout.left(page as usize, self.view_w, 0).saturating_add(x1.ceil() as i64);
        let margin = self.px(MARGIN);
        let (mut sx, mut sy) = (self.scroll_x, self.scroll_y);
        if top < sy + margin || bottom > sy + self.view_h - margin {
            sy = top - self.view_h / 3;
        }
        if left < sx + margin || right > sx + self.view_w - margin {
            sx = left - self.view_w / 3;
        }
        if (sx, sy) == (self.scroll_x, self.scroll_y) {
            return vec![Action::Invalidate];
        }
        self.scroll_to(sx, sy, false)
    }

    // --- links -------------------------------------------------------------------------------------------------

    /// Ask the engine for the links of the pages in view (and drop the ones far away).
    pub(super) fn ask_for_links(&mut self) {
        if self.bench.is_some() {
            return;
        }
        let Some((first, last)) = self.visible() else { return };
        let last = last.min(first.saturating_add(LINK_PAGES_ASKED));
        // Requests for pages that are no longer in view are withdrawn.
        let stale: Vec<u64> = self.links_asked.iter().filter(|(_, p)| !(first..=last).contains(p)).map(|(id, _)| *id).collect();
        for id in stale {
            self.engine.cancel(id);
            self.links_asked.remove(&id);
        }
        for page in first..=last {
            if self.links.contains_key(&page) || self.links_asked.values().any(|&p| p == page) {
                continue;
            }
            let id = self.new_id();
            self.engine.links(self.doc_id, id, page);
            self.links_asked.insert(id, page);
        }
        self.links.retain(|&p, _| p.abs_diff(first) <= LINK_PAGES_KEPT && p.abs_diff(last) <= LINK_PAGES_KEPT);
    }

    /// The page under a place of the client area, and the place in pixels from the top left of the page; none if it is not on a page.
    pub(super) fn page_at_point(&self, x: i64, y: i64) -> Option<(usize, f64, f64)> {
        let doc = self.doc.as_ref()?;
        let vx = x - self.view_x;
        if vx < 0 || vx >= self.view_w || y < 0 || y >= self.view_h {
            return None;
        }
        let l = &doc.layout;
        let dy = y + self.scroll_y;
        let page = l.page_at(dy);
        let (pw, ph) = l.size_px(page);
        let (px, py) = (vx - l.left(page, self.view_w, self.scroll_x), dy - l.top(page));
        (px >= 0 && py >= 0 && px < pw && py < ph).then_some((page, px as f64, py as f64))
    }

    /// The link of `page` that holds the point (x, y) of the page in points.
    pub(super) fn link_at(&self, page: usize, x: f64, y: f64) -> Option<usize> {
        let (x, y) = (x as f32, y as f32);
        self.links.get(&(page as u32))?.iter().position(|l| l.rect[0] <= x && x <= l.rect[2] && l.rect[1] <= y && y <= l.rect[3])
    }

    pub(super) fn cursor_over(&self, x: i64, y: i64) -> Cursor {
        if let Some(shape) = self.annotation_cursor()
            && self.page_at_point(x, y).is_some()
        {
            return shape;
        }
        if let Some((page, px, py)) = self.page_at_point(x, y)
            && let Some(map) = self.doc.as_ref().and_then(|d| d.layout.page_map(page))
        {
            let (xp, yp) = map.to_pt(px, py);
            if self.link_at(page, xp, yp).is_some() {
                return Cursor::Hand;
            }
        }
        Cursor::Arrow
    }

    /// The link `index` of `page` was clicked.
    pub(super) fn activate_link(&mut self, page: u32, index: usize) -> Vec<Action> {
        let Some(link) = self.links.get(&page).and_then(|l| l.get(index)).cloned() else { return Vec::new() };
        match link.kind {
            LinkKind::Page => self.go_to_target(link.page, link.y),
            LinkKind::Uri => {
                // Checked again here, whatever the engine said: only these addresses are ever offered to the person.
                let Some(uri) = safe_uri(link.uri.as_bytes()) else { return Vec::new() };
                self.pending_uri = Some(uri.clone());
                let text = format!("{}\n\n{uri}\n\n{}", t(self.lang, Msg::LinkPrompt), t(self.lang, Msg::LinkAsk));
                vec![Action::Confirm { tag: TAG_LINK, title: t(self.lang, Msg::LinkTitle).to_string(), text }]
            }
        }
    }

    pub(super) fn link_confirmed(&mut self, yes: bool) -> Vec<Action> {
        match self.pending_uri.take() {
            Some(uri) if yes && safe_uri(uri.as_bytes()).is_some() => vec![Action::OpenUri(uri)],
            _ => Vec::new(),
        }
    }

    // --- the engine's answers about text -------------------------------------------------------------------------

    pub(super) fn on_other_event(&mut self, event: EngineEvent) -> Vec<Action> {
        match event {
            EngineEvent::Outline { doc, items, .. } if doc == self.doc_id => self.on_outline(items),
            EngineEvent::Links { doc, id, page, links, .. } if doc == self.doc_id => {
                self.on_links(id, page, links);
                Vec::new()
            }
            EngineEvent::CharBoxes(b) if b.doc == self.doc_id => self.on_boxes(b),
            EngineEvent::Annotations { doc, id, page, items, .. } if doc == self.doc_id => self.on_annotations(id, page, items),
            EngineEvent::Edited(r) if r.doc == self.doc_id => self.on_edited(r),
            EngineEvent::Saved { doc, id, status, message, .. } if doc == self.doc_id => self.on_saved(id, status, &message),
            EngineEvent::SearchHits { doc, id, page, hits } if doc == self.doc_id => self.on_search_hits(id, page, hits),
            EngineEvent::SearchProgress { doc, id, pages_done, pages_total, .. } if doc == self.doc_id => self.on_search_progress(id, pages_done, pages_total),
            EngineEvent::SearchDone { doc, id, status, pages_skipped, .. } if doc == self.doc_id => self.on_search_done(id, status, pages_skipped),
            EngineEvent::Copied { doc, id, status, text, truncated, .. } if doc == self.doc_id => self.on_copied(id, status, text, truncated),
            EngineEvent::PrintBand(b) if b.doc == self.doc_id => self.on_print_band(b),
            EngineEvent::PrintDone { doc, id, status, message, .. } if doc == self.doc_id => self.on_print_done(id, status, &message),
            _ => Vec::new(),
        }
    }

    fn on_links(&mut self, id: u64, page: u32, links: Vec<Link>) {
        if self.links_asked.remove(&id).is_some() {
            self.links.insert(page, links);
        }
    }
}

