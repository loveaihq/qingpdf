//! The sidebar beside the pages (2026-10-14): three tabs, the bookmarks (the window's tree control, put under the tabs), the
//! thumbnails (a list this file draws and handles, from the small pictures the engine makes) and the annotations (`annotlist.rs`). The rows, the cache, what to ask for and
//! the keys are plain logic in `thumbs.rs`; this is the part that knows about the engine, the window and the painter.
//!
//! The pictures are asked for at a priority after everything the pages need (`thumbs::P_VISIBLE`), only for the rows in view and a few next
//! to them, at most two at a time and only while the pages have nothing outstanding; when the pages ask for something, the ones being
//! drawn are taken back. The list is not built until the thumbnails are shown.

use super::*;
use crate::thumbs::{Built, ListKey, Metrics, Strip, Thumb, tab_at};

// Sizes at 96 dpi.
pub(super) const TAB_H: i64 = 28;
/// The width of a thumbnail, however wide the sidebar is; it is shrunk only when the sidebar is narrower than that and `THUMB_MARGIN` on each side.
const THUMB_W: i64 = 130;
const THUMB_MARGIN: i64 = 18;
const THUMB_GUTTER: i64 = 12;
const THUMB_ABOVE: i64 = 8;
const THUMB_GAP: i64 = 4;
const THUMB_LABEL_H: i64 = 18;
const THUMB_AFTER: i64 = 6;
const BAR_W: i64 = 6;
const BAR_EDGE: i64 = 3;
/// The width of the sidebar when none was kept: a thumbnail with room on each side for the bar at the edge and the margin.
pub(super) const DEFAULT_WIDTH: i64 = THUMB_W + 2 * (THUMB_MARGIN + BAR_W + BAR_EDGE);

/// The key of the list a key of the keyboard is, if it is one.
pub(super) fn list_key(key: u32) -> Option<ListKey> {
    match key {
        vk::UP => Some(ListKey::Up),
        vk::DOWN => Some(ListKey::Down),
        vk::PRIOR => Some(ListKey::PageUp),
        vk::NEXT => Some(ListKey::PageDown),
        vk::HOME => Some(ListKey::Home),
        vk::END => Some(ListKey::End),
        _ => None,
    }
}

fn rect(x: i64, y: i64, w: i64, h: i64) -> Rect {
    Rect { x: clamp32(x), y: clamp32(y), w: clamp32(w), h: clamp32(h) }
}

impl App {
    /// The width of the sidebar beside the pages, with the strip at its edge that is dragged (0 when there is none: no file, it is
    /// hidden, or there are no bookmarks and it is not open on the thumbnails).
    pub(super) fn sidebar_width(&self) -> i64 {
        let wanted = self.show_sidebar && (!self.outline.is_empty() || self.thumbs_alone);
        if self.doc.is_some() && wanted && !self.full && (self.bench.is_none() || self.bench_sidebar) {
            (self.px(self.sidebar_pref) + self.px(SPLIT_GAP)).min(self.client_w / 2)
        } else {
            0
        }
    }

    /// The width of what is in the sidebar (without the strip at its edge).
    pub(super) fn list_w(&self) -> i64 {
        (self.view_x - self.px(SPLIT_GAP)).max(0)
    }

    /// The height of the list under the tabs.
    pub(super) fn list_h(&self) -> i64 {
        (self.view_h - self.px(TAB_H)).max(0)
    }

    /// The tab that shows: the person's, but the thumbnails when they chose the bookmarks and there are none.
    pub(super) fn active_tab(&self) -> Tab {
        if self.outline.is_empty() && self.tab == Tab::Bookmarks { Tab::Pages } else { self.tab }
    }

    /// Are the thumbnails on the screen?
    pub(super) fn thumbs_shown(&self) -> bool {
        self.view_x > 0 && self.doc.is_some() && self.active_tab() == Tab::Pages
    }

    /// Where the tree of bookmarks (under the tabs, when they are the tab shown) and the find box go in the window now.
    pub(super) fn chrome(&self) -> Vec<Action> {
        // The tree stops short of the pages by the strip that is dragged to change the sidebar's width.
        let tabs = self.px(TAB_H);
        let (w, h) = (clamp32(self.list_w()), clamp32((self.view_h - tabs).max(0)));
        let side = if self.view_x > 0 && self.active_tab() == Tab::Bookmarks { Rect { x: 0, y: clamp32(tabs), w, h } } else { Rect { x: 0, y: 0, w: 0, h: 0 } };
        let find = self.find_open.then(|| self.find_rect());
        vec![Action::Sidebar(side), Action::Find(find)]
    }

    /// The pages have to be laid out again because the sidebar came, went, or was shown or hidden (or the bookmarks came).
    pub(super) fn apply_sidebar(&mut self) -> Vec<Action> {
        let width = self.sidebar_width();
        let mut actions = Vec::new();
        if width != self.view_x {
            self.view_x = width;
            self.view_w = (self.client_w - width).max(0);
            let (ax, ay) = (self.view_w / 2, self.view_h / 2);
            self.relayout(ax, ay, None);
            actions.extend(self.after_view_change(false));
        } else {
            // The tab that shows may have changed (the bookmarks came).
            self.sync_thumbs();
            self.schedule_thumbs();
            self.ask_for_annot_list();
            actions.push(Action::Invalidate);
        }
        actions.extend(self.chrome());
        self.menus_dirty = true;
        self.ask_for_annot_list();
        actions
    }

    /// View > Show sidebar (F4). Shown, it is hidden; hidden, it is shown, and for a file without bookmarks it is shown on the thumbnails.
    pub(super) fn toggle_sidebar(&mut self) -> Vec<Action> {
        if self.doc.is_none() {
            return Vec::new();
        }
        if self.sidebar_width() > 0 {
            self.show_sidebar = false;
        } else {
            self.show_sidebar = true;
            // Without bookmarks the thumbnails show (`active_tab`); the tab the person chose is left as it was.
            if self.outline.is_empty() {
                self.thumbs_alone = true;
            }
        }
        self.thumb_focus = false;
        self.apply_sidebar()
    }

    /// A tab was clicked: it is the one shown, and the one kept for the next time.
    fn choose_tab(&mut self, tab: Tab) -> Vec<Action> {
        if tab == Tab::Bookmarks && self.outline.is_empty() {
            return Vec::new();
        }
        self.tab = tab;
        // (Without bookmarks the sidebar is there only for the tab that is not them.)
        self.thumbs_alone = tab != Tab::Bookmarks;
        self.thumb_focus = tab == Tab::Pages;
        self.annots_focus = tab == Tab::Annots;
        self.keep(|s| s.tab = Some(tab));
        self.sync_thumbs();
        self.schedule_thumbs();
        self.ask_for_annot_list();
        let mut actions = self.chrome();
        actions.push(Action::Invalidate);
        actions
    }

    // --- the rows ----------------------------------------------------------------------------------------------

    /// The room for a picture: a fixed width (centred in the list when the rows are drawn), so the rows do not change when the sidebar is
    /// made wider or narrower, until it is too narrow for them.
    fn thumb_metrics(&self) -> Metrics {
        let avail_w = thumbs::picture_width(self.px(THUMB_W), self.list_w(), self.px(THUMB_MARGIN), self.px(32));
        Metrics::new(avail_w, self.px(THUMB_ABOVE), self.px(THUMB_GAP + THUMB_LABEL_H + THUMB_AFTER))
    }

    /// The page the list shows as the current one: the one chosen in it, while that is in view, or the one in the middle of the window.
    pub(super) fn thumb_current(&self) -> usize {
        match self.thumb_target {
            Some(t) if self.page_in_view(t as u32) => t,
            _ => self.current_page(),
        }
    }

    /// Make the rows again if the width, the turn or the file changed, and move the list to show the current page if that changed.
    pub(super) fn sync_thumbs(&mut self) {
        if !self.thumbs_shown() {
            self.cancel_thumbs();
            return;
        }
        let metrics = self.thumb_metrics();
        let Some(doc) = &self.doc else { return };
        let pages = doc.layout.len();
        let built = Built { metrics, rotation: self.rotation, pages };
        if self.thumbs.ensure(built, || Strip::new((0..pages).filter_map(|p| doc.layout.size_pt(p)), &metrics)) {
            // The requests out are for rows that are not there any more (or have another size).
            self.cancel_thumbs();
        }
        let (current, list_h) = (self.thumb_current(), self.list_h());
        if self.thumbs.synced != Some(current) {
            self.thumbs.synced = Some(current);
            self.thumbs.scroll = self.thumbs.strip.reveal(current, self.thumbs.scroll, list_h);
        }
        self.thumbs.clamp(list_h);
    }

    /// A page was changed (a note added, text edited): its picture is out of date, and asked for again.
    pub(super) fn forget_thumb(&mut self, page: u32) {
        self.thumbs.cache.remove(page);
        self.thumbs.forget_failure(page);
        let stale: Vec<u64> = self.thumbs.asked.iter().filter(|(_, p)| **p == page).map(|(id, _)| *id).collect();
        for id in stale {
            self.engine.cancel(id);
            self.thumbs.asked.remove(&id);
        }
    }

    pub(super) fn cancel_thumbs(&mut self) {
        for (id, _) in self.thumbs.asked.drain() {
            self.engine.cancel(id);
        }
    }

    // --- asking for the pictures -------------------------------------------------------------------------------

    /// Ask the engine for the pictures of the rows in view and next to it, and take back the requests for rows scrolled away.
    pub(super) fn schedule_thumbs(&mut self) {
        if !self.thumbs_shown() {
            return;
        }
        let want = thumbs::wanted(&self.thumbs.strip, self.thumbs.scroll, self.list_h(), self.thumbs.cache.cap());
        // Only while the pages have nothing outstanding, and not in the middle of a drag of the bar or of the edge or while the pages move;
        // the requests for rows scrolled away are taken back all the same.
        let busy = !self.in_flight.is_empty() || self.thumb_drag || self.split_drag.is_some() || self.moving;
        let room = if busy { 0 } else { thumbs::MAX_IN_FLIGHT };
        let rotation = self.rotation;
        let t = &self.thumbs;
        let plan = thumbs::plan(&want, &t.asked, |p| t.is_done(p, rotation), room);
        for id in plan.cancel {
            self.engine.cancel(id);
            self.thumbs.asked.remove(&id);
        }
        for w in plan.issue {
            let Some(row) = self.thumbs.strip.row(w.page as usize).copied() else { continue };
            let id = self.new_id();
            self.engine.render(RenderRequest { doc: self.doc_id, id, page: w.page, dpi: sched::dpi_of(row.scale), rotation, x: 0, y: 0, width: 0, height: 0, priority: w.priority });
            self.thumbs.asked.insert(id, w.page);
        }
    }

    /// A picture came back. `true`: its row is in view, so the window needs a new picture.
    pub(super) fn on_thumb_rendered(&mut self, page: u32, mut r: Rendered) -> bool {
        let whole = r.width > 0 && r.height > 0 && r.bgra.len() as u64 == u64::from(r.width) * u64::from(r.height) * 4;
        let view = self.thumbs.strip.rows_in(self.thumbs.scroll, self.thumbs.scroll + self.list_h());
        if r.status == RenderStatus::Failed || !whole {
            // Asked for once more (by `schedule_thumbs`); a second failure makes the row a grey box.
            self.thumbs.note_failure(page);
        } else if let Some(row) = self.thumbs.strip.row(page as usize) {
            let (first, last) = (view.start as u32, view.end.saturating_sub(1) as u32);
            recolor(&mut r.bgra, self.tint);
            let thumb = Thumb { page, rotation: self.rotation, scale: row.scale, width: r.width, height: r.height, bgra: r.bgra };
            self.thumbs.cache.insert(thumb, first, last);
        }
        view.contains(&(page as usize))
    }

    // --- the mouse and the keys --------------------------------------------------------------------------------

    /// Is this place of the page area over the list of thumbnails?
    pub(super) fn over_thumbs(&self, x: i64, y: i64) -> bool {
        self.thumbs_shown() && x >= 0 && x < self.list_w() && y >= 0 && y < self.view_h
    }

    pub(super) fn thumb_wheel(&mut self, delta: i32) -> Vec<Action> {
        self.thumbs.scroll -= i64::from(delta) * self.px(WHEEL_STEP) / 120;
        self.thumbs.clamp(self.list_h());
        self.schedule_thumbs();
        vec![Action::Invalidate]
    }

    pub(super) fn thumb_key(&mut self, key: ListKey) -> Vec<Action> {
        let count = self.page_count();
        let current = self.thumb_current();
        let screen = self.thumbs.strip.rows_in(self.thumbs.scroll, self.thumbs.scroll + self.list_h()).len();
        let to = thumbs::key_target(key, current, count, screen);
        if count == 0 || to == current {
            return Vec::new();
        }
        self.pg.chosen.only(to);
        self.go_to_thumb(to)
    }

    /// Go to a page that was chosen in the list: it is the current one while it is in view.
    pub(super) fn go_to_thumb(&mut self, page: usize) -> Vec<Action> {
        self.thumb_target = Some(page);
        self.thumb_nav = true;
        let actions = self.go_to_page(page);
        self.thumb_nav = false;
        actions
    }

    /// A press in the sidebar: on a tab, on the bar at the side of the list, or on a row. `None`: it is not in the sidebar.
    pub(super) fn sidebar_mouse_down(&mut self, x: i64, y: i64, ctrl: bool, shift: bool) -> Option<Vec<Action>> {
        let width = self.list_w();
        if self.view_x <= 0 || x < 0 || x >= width || y < 0 || y >= self.view_h {
            return None;
        }
        let tabs = self.px(TAB_H);
        if y < tabs {
            return Some(self.choose_tab(tab_at(x, width)));
        }
        if self.annots_shown() {
            return Some(self.annots_press(x, y));
        }
        if !self.thumbs_shown() {
            return Some(Vec::new());
        }
        self.annots_focus = false;
        self.thumb_focus = true;
        if x >= width - self.px(THUMB_GUTTER) && self.thumbs.strip.total() > self.list_h() {
            self.thumb_drag = true;
            return Some(self.thumb_drag_to(y));
        }
        let at = self.thumbs.scroll + y - tabs;
        Some(match self.thumbs.strip.row_at(at) {
            Some(page) => self.thumb_press(page, ctrl, shift, x, y),
            None => Vec::new(),
        })
    }

    /// The bar at the side of the list is dragged, the pointer now at `y`.
    pub(super) fn thumb_drag_to(&mut self, y: i64) -> Vec<Action> {
        let list_h = self.list_h();
        self.thumbs.scroll = thumbs::scroll_at_bar(y - self.px(TAB_H), list_h, self.thumbs.strip.total());
        self.thumbs.clamp(list_h);
        // Nothing new is asked for while the bar is held, but the requests for the rows it left are taken back.
        self.schedule_thumbs();
        vec![Action::Invalidate]
    }

    /// The drag is over: the pictures of the rows it stopped at are asked for.
    pub(super) fn thumb_drag_end(&mut self) -> Vec<Action> {
        if !std::mem::take(&mut self.thumb_drag) {
            return Vec::new();
        }
        self.schedule_thumbs();
        vec![Action::Invalidate]
    }

    /// The speed test shows the sidebar on the thumbnails.
    pub fn bench_thumbs(&mut self) {
        self.bench_sidebar = true;
        self.thumbs_alone = true;
        self.tab = Tab::Pages;
    }

    // --- drawing -----------------------------------------------------------------------------------------------

    pub(super) fn draw_sidebar(&self, p: &mut dyn Painter) {
        if self.view_x <= 0 {
            return;
        }
        let (width, tabs) = (self.list_w(), self.px(TAB_H));
        if self.thumbs_shown() {
            self.draw_thumbs(p, width, tabs);
        } else if self.annots_shown() {
            self.draw_annot_list(p, width, tabs);
        }
        // The tabs come last: a row scrolled up under them is hidden by them.
        self.draw_tabs(p, width, tabs);
    }

    fn draw_tabs(&self, p: &mut dyn Painter, width: i64, tabs: i64) {
        p.fill_rect(rect(0, 0, width, tabs), self.pal.header_bg);
        let active = self.active_tab();
        let (third, two_thirds) = (width / 3, width * 2 / 3);
        for (tab, x0, x1, msg) in [(Tab::Bookmarks, 0, third, Msg::TabBookmarks), (Tab::Pages, third, two_thirds, Msg::TabPages), (Tab::Annots, two_thirds, width, Msg::TabAnnots)] {
            let on = tab == active;
            let enabled = tab != Tab::Bookmarks || !self.outline.is_empty();
            if on {
                p.fill_rect(rect(x0, 0, x1 - x0, tabs), self.pal.tab_on_bg);
                p.fill_rect(rect(x0, tabs - self.px(3), x1 - x0, self.px(3)), self.pal.accent);
            }
            let s = t(self.lang, msg);
            let w = i64::from(p.text_width(s));
            let color = if !enabled { self.pal.disabled_fg } else if on { self.pal.selected_fg } else { self.pal.number_fg };
            p.text(clamp32(x0 + (x1 - x0 - w) / 2), clamp32((tabs - i64::from(p.line_height())) / 2 - self.px(1)), s, color);
        }
        p.fill_rect(rect(0, tabs - self.px(1).max(1), width, self.px(1).max(1)), self.pal.header_edge);
        p.fill_rect(rect(third, 0, self.px(1).max(1), tabs), self.pal.header_edge);
        p.fill_rect(rect(two_thirds, 0, self.px(1).max(1), tabs), self.pal.header_edge);
    }

    fn draw_thumbs(&self, p: &mut dyn Painter, width: i64, tabs: i64) {
        let (strip, scroll, list_h) = (&self.thumbs.strip, self.thumbs.scroll, self.list_h());
        p.fill_rect(rect(0, tabs, width, list_h), self.pal.list_bg);
        let current = self.thumb_current();
        // (In a spread both pages of the row in view are the page in view.)
        let shown = self.doc.as_ref().map_or(current..current + 1, |d| d.layout.pages_in_row(current));
        let chosen = &self.pg.chosen;
        let (above, inset, edge) = (self.px(THUMB_ABOVE), self.px(4), self.px(2));
        let label_top = self.px(THUMB_GAP);
        let label_h = self.px(THUMB_LABEL_H);
        for i in strip.rows_in(scroll, scroll + list_h) {
            let Some(row) = strip.row(i) else { continue };
            let top = tabs + row.top - scroll;
            // The chosen rows are marked; with none chosen the page in view is (as it always was), and when others are chosen it has a frame.
            let picked = chosen.contains(i) || (chosen.is_empty() && shown.contains(&i));
            if picked {
                p.fill_rect(rect(inset, top + edge, width - 2 * inset, row.height - 2 * edge), self.pal.selected_border);
                p.fill_rect(rect(inset + edge, top + 2 * edge, width - 2 * (inset + edge), row.height - 4 * edge), self.pal.selected_bg);
            } else if shown.contains(&i) {
                p.fill_rect(rect(inset, top + edge, width - 2 * inset, row.height - 2 * edge), self.pal.current_frame);
                p.fill_rect(rect(inset + edge, top + 2 * edge, width - 2 * (inset + edge), row.height - 4 * edge), self.pal.list_bg);
            }
            let (x, y) = ((width - row.img_w) / 2, top + above);
            p.fill_rect(rect(x - 1, y - 1, row.img_w + 2, row.img_h + 2), self.pal.thumb_border);
            // A picture drawn for another turn of the view is not the shape of the row: it waits for the new one.
            let picture = self.thumbs.cache.get(i as u32).filter(|t| t.rotation == self.rotation);
            match picture {
                Some(t) => p.draw_bgra(rect(x, y, row.img_w, row.img_h), clamp32(i64::from(t.width)), clamp32(i64::from(t.height)), &t.bgra),
                None => p.fill_rect(rect(x, y, row.img_w, row.img_h), if self.thumbs.failed.contains(&(i as u32)) { self.pal.damaged } else { self.pal.placeholder }),
            }
            let number = (i + 1).to_string();
            let w = i64::from(p.text_width(&number));
            let ty = y + row.img_h + label_top + (label_h - i64::from(p.line_height())) / 2;
            p.text(clamp32((width - w) / 2), clamp32(ty), &number, if picked { self.pal.selected_fg } else { self.pal.number_fg });
        }
        self.draw_drop_mark(p, width, tabs);
        if let Some((bar_top, bar_len)) = thumbs::bar(scroll, list_h, strip.total()) {
            p.fill_rect(rect(width - self.px(BAR_W + BAR_EDGE), tabs + bar_top, self.px(BAR_W), bar_len), self.pal.bar_color);
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::settings::Settings;

    /// A painter that writes down what it was asked to draw.
    #[derive(Default)]
    pub(in crate::app) struct Rec {
        pub fills: Vec<(Rect, u32)>,
        pub pictures: Vec<(Rect, i32, i32)>,
        pub texts: Vec<(i32, i32, String)>,
    }

    impl Painter for Rec {
        fn fill_rect(&mut self, r: Rect, color: u32) {
            self.fills.push((r, color));
        }
        fn fill_rect_alpha(&mut self, r: Rect, color: u32, _alpha: u8) {
            self.fills.push((r, color));
        }
        fn draw_bgra(&mut self, dest: Rect, src_w: i32, src_h: i32, bgra: &[u8]) {
            assert!(bgra.len() >= (src_w as usize) * (src_h as usize) * 4, "a picture with fewer bytes than it says");
            self.pictures.push((dest, src_w, src_h));
        }
        fn text(&mut self, x: i32, y: i32, s: &str, _color: u32) {
            self.texts.push((x, y, s.to_string()));
        }
        fn text_width(&mut self, s: &str) -> i32 {
            7 * s.chars().count() as i32
        }
        fn line_height(&self) -> i32 {
            16
        }
    }

    /// A PDF of pages of the given sizes and own turns (a rectangle drawn on each); the pages in `broken` have a content stream that is garbage.
    pub(in crate::app) fn pages_pdf(sizes: &[(u32, u32, u32)], broken: &[usize]) -> Vec<u8> {
        pages_pdf_with(sizes, broken, "")
    }

    /// [`pages_pdf`] with more in the catalog (`/PageLayout /TwoColumnLeft`, say).
    pub(in crate::app) fn pages_pdf_with(sizes: &[(u32, u32, u32)], broken: &[usize], catalog: &str) -> Vec<u8> {
        let n = sizes.len() as u32;
        let mut objects: Vec<(u32, String)> = vec![(1, format!("<< /Type /Catalog /Pages 2 0 R {catalog} >>"))];
        let kids: String = (0..n).map(|i| format!("{} 0 R ", 3 + 2 * i)).collect();
        objects.push((2, format!("<< /Type /Pages /Kids [{kids}] /Count {n} >>")));
        for (i, &(w, h, rotate)) in sizes.iter().enumerate() {
            let (page, content) = (3 + 2 * i as u32, 4 + 2 * i as u32);
            objects.push((page, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Rotate {rotate} /Contents {content} 0 R >>")));
            let body = if broken.contains(&i) { "q 1 0 0 rg 10 10 m 20 BOGUS ( ( ( [ [".to_string() } else { format!("0.2 0.4 0.8 rg 10 10 {} {} re f", w.saturating_sub(20), h / 2) };
            objects.push((content, format!("<< /Length {} >>\nstream\n{body}\nendstream", body.len())));
        }
        let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
        let mut offsets = std::collections::BTreeMap::new();
        for (n, body) in &objects {
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

    pub(in crate::app) fn app() -> App {
        let mut app = App::new(Lang::En, Arc::new(|| {}), Instant::now(), 1920 * 1080, "Tester".to_string());
        // The tests keep nothing: not the settings, not the list of recent files.
        app.persist = false;
        app.recent_path = None;
        app
    }

    pub(in crate::app) fn pump(app: &mut App, what: &str, until: impl Fn(&App) -> bool) {
        for _ in 0..1500 {
            app.event(Event::Wake);
            if until(app) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for {what}");
    }

    pub(in crate::app) fn open_pages(name: &str, sizes: &[(u32, u32, u32)], thumbs_tab: bool) -> App {
        open_broken(name, sizes, &[], thumbs_tab)
    }

    pub(in crate::app) fn open_with_catalog(name: &str, sizes: &[(u32, u32, u32)], catalog: &str) -> App {
        open_file(name, &pages_pdf_with(sizes, &[], catalog), false)
    }

    fn open_broken(name: &str, sizes: &[(u32, u32, u32)], broken: &[usize], thumbs_tab: bool) -> App {
        open_file(name, &pages_pdf(sizes, broken), thumbs_tab)
    }

    fn open_file(name: &str, bytes: &[u8], thumbs_tab: bool) -> App {
        let file = std::env::temp_dir().join(format!("qingpdf-thumbs-{}-{name}.pdf", std::process::id()));
        std::fs::write(&file, bytes).expect("writes");
        let mut app = app();
        if thumbs_tab {
            app.apply_settings(&Settings { tab: Some(Tab::Pages), ..Settings::default() });
        }
        app.event(Event::Size { w: 1000, h: 1100 });
        app.open_path(&file, "");
        pump(&mut app, "the file to open", |a| a.has_document());
        let _ = std::fs::remove_file(&file);
        app
    }

    pub(in crate::app) fn letter(n: usize) -> Vec<(u32, u32, u32)> {
        vec![(612, 792, 0); n]
    }

    fn tab_click(app: &mut App, x: i32) {
        app.event(Event::MouseDown { x, y: 10, ctrl: false, shift: false, clicks: 1 });
        app.event(Event::MouseUp { x, y: 10 });
    }

    #[test]
    fn a_file_without_bookmarks_has_a_sidebar_only_when_it_is_open_on_the_thumbnails() {
        let mut app = open_pages("none", &letter(3), false);
        assert_eq!(app.view_x, 0);
        assert!(!app.thumbs_shown());
        // F4 opens it on the thumbnails (the bookmarks have nothing to show), F4 again closes it.
        let open = app.event(Event::Key { vk: vk::F4, ctrl: false, shift: false });
        assert!(app.view_x > 0 && app.thumbs_shown(), "{}", app.view_x);
        // The tree of bookmarks is told to stay away, and the pages are told to the right of the sidebar.
        assert!(open.iter().any(|a| matches!(a, Action::Sidebar(r) if r.w == 0)));
        assert_eq!(app.view_w, app.client_w - app.view_x);
        assert_eq!(app.active_tab(), Tab::Pages);
        app.event(Event::Key { vk: vk::F4, ctrl: false, shift: false });
        assert_eq!(app.view_x, 0);
        assert!(!app.thumbs_shown());
        // The menu item is checked while it shows.
        app.event(Event::Key { vk: vk::F4, ctrl: false, shift: false });
        let menus = app.menus();
        let view = menus.iter().find(|m| m.title.contains("View")).expect("a View menu");
        assert!(view.entries.iter().any(|e| matches!(e, crate::ui::MenuEntry::Item { id, checked: true, enabled: true, .. } if *id == CMD_BOOKMARKS)));
    }

    #[test]
    fn a_thousand_pages_ask_for_a_handful_of_pictures_and_keep_them_inside_the_cap() {
        let mut app = open_pages("thousand", &letter(1000), true);
        assert!(app.thumbs_shown());
        pump(&mut app, "the first pictures", |a| a.thumbs.cache.len() >= 5);
        pump(&mut app, "the engine to be idle", |a| a.thumbs.asked.is_empty() && a.is_idle());
        let list_h = app.list_h();
        let in_view = app.thumbs.strip.rows_in(0, list_h).len();
        let wanted = in_view + 2 * thumbs::PREFETCH_ROWS;
        assert!(app.thumbs.cache.len() <= wanted, "{} pictures for {in_view} rows in view", app.thumbs.cache.len());
        assert!(app.thumbs.cache.len() >= in_view, "the rows in view all have theirs");
        // Never more than two requests at once.
        assert!(app.thumbs.asked.len() <= thumbs::MAX_IN_FLIGHT);
        // Scrolling the whole list by the wheel, with the engine answering as it goes, never takes the cache over its cap.
        let cap = app.thumbs.cache.cap();
        assert_eq!(cap, qingpdf_core::view::THUMB_CACHE_BYTES);
        for step in 0..400 {
            app.event(Event::Wheel { delta: -360, ctrl: false, shift: false, x: 50, y: 300 });
            app.event(Event::Wake);
            assert!(app.thumbs.asked.len() <= thumbs::MAX_IN_FLIGHT, "{step}");
            assert!(app.thumbs.cache.bytes() <= cap, "{} bytes after {step} steps", app.thumbs.cache.bytes());
            if step % 7 == 0 {
                std::thread::sleep(Duration::from_millis(3));
            }
        }
        assert!(app.thumbs.scroll > 0 && app.thumbs.scroll <= app.thumbs.strip.max_scroll(list_h));
        // Every request out is for a row near the view: the ones for rows scrolled away were taken back.
        let view = app.thumbs.strip.rows_in(app.thumbs.scroll, app.thumbs.scroll + list_h);
        assert!(app.thumbs.asked.values().all(|&p| (p as usize + thumbs::PREFETCH_ROWS + 1) >= view.start && (p as usize) <= view.end + thumbs::PREFETCH_ROWS), "{:?} {view:?}", app.thumbs.asked);
        pump(&mut app, "the pictures of the new rows", |a| a.thumbs.cache.get(view.start as u32).is_some());
        assert!(app.thumbs.cache.bytes() <= cap);
    }

    #[test]
    fn a_click_goes_to_the_page_and_the_list_follows_the_page_when_the_pages_move() {
        let mut app = open_pages("follow", &letter(200), true);
        let tabs = app.px(TAB_H);
        // Click the third row.
        let third = app.thumbs.strip.row(2).copied().expect("a row");
        let y = (tabs + third.top + third.height / 2) as i32;
        app.event(Event::MouseDown { x: 80, y, ctrl: false, shift: false, clicks: 1 });
        app.event(Event::MouseUp { x: 80, y });
        assert_eq!(app.thumb_current(), 2);
        assert_eq!(app.current_page(), 2);
        assert!(app.thumb_focus);
        // The pages move on by themselves (the toolbar's button, the page box): the list shows the page.
        app.event(Event::PageBox("150".to_string()));
        assert_eq!(app.current_page(), 149);
        assert_eq!(app.thumb_current(), 149);
        let row = app.thumbs.strip.row(149).copied().expect("a row");
        let list_h = app.list_h();
        assert!(row.top >= app.thumbs.scroll && row.top + row.height <= app.thumbs.scroll + list_h, "row 149 is in view of the list");
        // Scrolling the list by hand does not move the pages, and the list stays where the person put it.
        let before = app.scroll_y;
        app.event(Event::Wheel { delta: 120, ctrl: false, shift: false, x: 50, y: 300 });
        assert_eq!(app.scroll_y, before);
        let moved = app.thumbs.scroll;
        app.event(Event::Wake);
        assert_eq!(app.thumbs.scroll, moved);
        // The wheel over the pages still scrolls the pages.
        app.event(Event::Wheel { delta: -120, ctrl: false, shift: false, x: 600, y: 300 });
        assert!(app.scroll_y > before);
        assert_eq!(app.thumbs.scroll, moved);
    }

    #[test]
    fn the_arrow_keys_move_through_the_pages_when_the_list_has_the_keyboard() {
        let mut app = open_pages("keys", &letter(60), true);
        let key = |app: &mut App, vk: u32| app.event(Event::Key { vk, ctrl: false, shift: false });
        // Without the keyboard, Down scrolls the pages as it always did.
        key(&mut app, vk::DOWN);
        assert_eq!(app.current_page(), 0);
        assert!(app.scroll_y > 0);
        // Click the list: the keys are its.
        let row = app.thumbs.strip.row(0).copied().expect("a row");
        let y = (app.px(TAB_H) + row.top + row.height / 2) as i32;
        app.event(Event::MouseDown { x: 80, y, ctrl: false, shift: false, clicks: 1 });
        app.event(Event::MouseUp { x: 80, y });
        assert_eq!(app.thumb_current(), 0);
        key(&mut app, vk::DOWN);
        assert_eq!(app.thumb_current(), 1);
        key(&mut app, vk::DOWN);
        key(&mut app, vk::UP);
        assert_eq!(app.thumb_current(), 1);
        key(&mut app, vk::NEXT);
        let after_page_down = app.thumb_current();
        assert!(after_page_down > 2 && after_page_down < 10, "a screenful of rows: {after_page_down}");
        key(&mut app, vk::PRIOR);
        assert!(app.thumb_current() < after_page_down);
        key(&mut app, vk::END);
        assert_eq!(app.thumb_current(), 59);
        key(&mut app, vk::DOWN);
        assert_eq!(app.thumb_current(), 59, "stops at the last page");
        key(&mut app, vk::HOME);
        assert_eq!(app.thumb_current(), 0);
        assert_eq!(app.thumbs.scroll, 0);
        // A click on the pages gives the keyboard back to them.
        app.event(Event::MouseDown { x: 600, y: 300, ctrl: false, shift: false, clicks: 1 });
        app.event(Event::MouseUp { x: 600, y: 300 });
        assert!(!app.thumb_focus);
        let before = app.scroll_y;
        key(&mut app, vk::DOWN);
        assert!(app.scroll_y > before);
    }

    #[test]
    fn a_page_chosen_in_the_list_stays_current_while_in_view_and_scrolling_the_pages_takes_the_choice_back() {
        // Small pages, so that several are in view at once: the page in the middle of the window is not the one at the top.
        let mut app = open_pages("small", &vec![(100, 100, 0); 40], true);
        app.event(Event::Command(CMD_ACTUAL));
        let (first, last) = app.visible().expect("pages in view");
        assert!(last > first + 2, "{first}..{last}");
        let row = app.thumbs.strip.row(3).copied().expect("a row");
        let y = (app.px(TAB_H) + row.top + 4) as i32;
        app.event(Event::MouseDown { x: 80, y, ctrl: false, shift: false, clicks: 1 });
        app.event(Event::MouseUp { x: 80, y });
        assert_eq!(app.thumb_current(), 3);
        assert_ne!(app.current_page(), 3, "the middle of the window is another page");
        // The pages are scrolled some other way: the middle page is current again.
        app.event(Event::Wheel { delta: -120, ctrl: false, shift: false, x: 600, y: 300 });
        assert_eq!(app.thumb_current(), app.current_page());
    }

    #[test]
    fn the_tabs_switch_the_list_and_the_tree_and_the_sidebar_comes_with_the_bookmarks() {
        let mut app = app();
        app.event(Event::Size { w: 1000, h: 700 });
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/corpus/public/outline-form-attach/bookmarks.pdf");
        app.open_path(std::path::Path::new(file), "");
        pump(&mut app, "the bookmarks", |a| !a.outline.is_empty());
        assert!(app.view_x > 0);
        assert_eq!(app.active_tab(), Tab::Bookmarks);
        assert!(!app.thumbs_shown());
        assert!(app.thumbs.asked.is_empty() && app.thumbs.cache.is_empty(), "nothing is made for thumbnails nobody sees");
        // The tree sits under the tabs.
        let actions = app.chrome();
        let tabs = app.px(TAB_H);
        assert!(actions.iter().any(|a| matches!(a, Action::Sidebar(r) if i64::from(r.y) == tabs && r.w > 0 && i64::from(r.y + r.h) == app.view_h)));
        // The middle third of the header is the pages.
        let middle = (app.list_w() / 2) as i32;
        tab_click(&mut app, middle);
        assert_eq!(app.active_tab(), Tab::Pages);
        assert!(app.thumbs_shown() && app.thumb_focus);
        assert!(app.chrome().iter().any(|a| matches!(a, Action::Sidebar(r) if r.w == 0)), "the tree is hidden");
        pump(&mut app, "a picture", |a| !a.thumbs.cache.is_empty());
        // ... and the left half the bookmarks again.
        tab_click(&mut app, 10);
        assert_eq!(app.active_tab(), Tab::Bookmarks);
        assert!(!app.thumb_focus && app.thumbs.asked.is_empty());
    }

    #[test]
    fn the_last_tab_comes_from_the_settings_and_goes_back_to_them() {
        let mut app = app();
        app.apply_settings(&Settings { tab: Some(Tab::Pages), ..Settings::default() });
        assert_eq!((app.tab, app.thumbs_alone), (Tab::Pages, true));
        app.apply_settings(&Settings { tab: Some(Tab::Bookmarks), ..Settings::default() });
        assert_eq!((app.tab, app.thumbs_alone), (Tab::Bookmarks, false));
        app.apply_settings(&Settings::default());
        assert_eq!(app.tab, Tab::Bookmarks);
    }

    #[test]
    fn rotated_pages_and_wide_pages_keep_their_shape_and_a_broken_page_never_stops_the_list() {
        // A portrait page, one turned a quarter by its own /Rotate, a landscape page, a page whose content is garbage.
        let sizes = [(612, 792, 0), (612, 792, 90), (792, 612, 0), (612, 792, 0), (200, 300, 0)];
        let mut app = open_broken("shapes", &sizes, &[3], true);
        pump(&mut app, "all five pictures", |a| (0..5).all(|p| a.thumbs.cache.get(p).is_some() || a.thumbs.failed.contains(&p)));
        let shape = |app: &App, p: usize| app.thumbs.strip.row(p).map(|r| (r.img_w, r.img_h)).expect("a row");
        let (w0, h0) = shape(&app, 0);
        let (w1, h1) = shape(&app, 1);
        assert!(h0 > w0, "portrait is taller than wide");
        assert!(w1 > h1 && (w1, h1) == shape(&app, 2), "turned by its own /Rotate: as wide as the landscape page");
        // The view's own turn (Ctrl+R) turns every row.
        let before = app.thumbs.strip.total();
        app.event(Event::Key { vk: vk::R, ctrl: true, shift: false });
        let (w0r, h0r) = shape(&app, 0);
        assert!(w0r > h0r, "turned: wide now");
        assert_ne!(app.thumbs.strip.total(), before);
        pump(&mut app, "pictures of the turned pages", |a| a.thumbs.fresh(0, 90) && a.thumbs.fresh(4, 90) && (a.thumbs.fresh(3, 90) || a.thumbs.failed.contains(&3)));
        // The pictures that came back are the size of the row, so nothing is stretched.
        let t = app.thumbs.cache.get(0).expect("a picture");
        assert_eq!((i64::from(t.width), i64::from(t.height)), (w0r, h0r));
        // Drawing: every row has a picture or a box, and the number under it.
        let mut rec = Rec::default();
        app.paint(&mut rec);
        assert!(rec.pictures.iter().any(|(r, ..)| i64::from(r.w) == w0r));
        for n in 1..=4 {
            assert!(rec.texts.iter().any(|(_, _, s)| *s == n.to_string()), "row {n} has its number");
        }
        // The garbage page is either a partial picture or a grey box; either way it is drawn and nothing else is the worse for it.
        assert!(app.thumbs.cache.get(3).is_some() || app.thumbs.failed.contains(&3));
        if app.thumbs.failed.contains(&3) {
            assert!(rec.fills.iter().any(|(_, c)| *c == app.pal.damaged));
        }
    }

    #[test]
    fn the_bar_at_the_side_scrolls_the_list_and_the_pictures_wait_for_the_drag_to_end() {
        let mut app = open_pages("bar", &letter(500), true);
        pump(&mut app, "the first pictures", |a| a.thumbs.cache.len() >= 3 && a.thumbs.asked.is_empty() && a.is_idle());
        let x = (app.list_w() - 4) as i32;
        let list_h = app.list_h();
        let tabs = app.px(TAB_H);
        let y_at = move |frac: f64| (tabs as f64 + frac * list_h as f64) as i32;
        app.event(Event::MouseDown { x, y: y_at(0.5), ctrl: false, shift: false, clicks: 1 });
        assert!(app.thumb_drag);
        let mid = app.thumbs.scroll;
        assert!(mid > app.thumbs.strip.total() / 4 && mid < app.thumbs.strip.total() * 3 / 4, "about the middle: {mid}");
        app.event(Event::MouseMove { x, y: y_at(1.0), shift: false });
        assert_eq!(app.thumbs.scroll, app.thumbs.strip.max_scroll(list_h));
        assert!(app.thumbs.asked.is_empty(), "nothing is asked for while the bar is dragged");
        app.event(Event::MouseUp { x, y: y_at(1.0) });
        assert!(!app.thumb_drag);
        pump(&mut app, "pictures of the end of the list", |a| a.thumbs.cache.get(499).is_some());
        // The page was not changed by the drag.
        assert_eq!(app.current_page(), 0);
    }

    #[test]
    fn the_thumbnails_are_a_fixed_size_centred_and_the_default_sidebar_just_fits_them() {
        let mut app = open_pages("fixed", &letter(30), true);
        pump(&mut app, "every picture wanted", |a| a.thumbs.asked.is_empty() && a.is_idle() && thumbs::wanted(&a.thumbs.strip, a.thumbs.scroll, a.list_h(), a.thumbs.cache.cap()).iter().all(|w| a.thumbs.fresh(w.page, 0)));
        // Nothing was kept: the sidebar is as wide as a thumbnail with its margins and the bar needs (180 to 190 at 96 dpi).
        assert_eq!(app.sidebar_pref, DEFAULT_WIDTH);
        assert!((180..=190).contains(&DEFAULT_WIDTH), "{DEFAULT_WIDTH}");
        assert_eq!(app.list_w(), app.px(DEFAULT_WIDTH));
        let row = app.thumbs.strip.row(0).copied().expect("a row");
        assert_eq!((row.img_w, row.img_h), (app.px(THUMB_W), app.px(THUMB_W) * 792 / 612));
        assert!(row.img_h <= app.px(THUMB_W) * 3 / 2);
        let side = (app.list_w() - row.img_w) / 2;
        assert!(side >= app.px(THUMB_MARGIN) && side >= app.px(BAR_W + BAR_EDGE), "{side} of room on each side");
        let centred = |app: &mut App, img_w: i64| {
            let mut rec = Rec::default();
            app.paint(&mut rec);
            let width = app.list_w();
            let shown: Vec<_> = rec.pictures.iter().filter(|(r, ..)| i64::from(r.x) < width).collect();
            assert!(!shown.is_empty() && shown.iter().all(|(r, ..)| i64::from(r.w) == img_w && (i64::from(r.x) * 2 + i64::from(r.w) - width).abs() <= 1), "{shown:?}");
        };
        centred(&mut app, row.img_w);
        // Dragging the edge wider does not make them bigger: the rows are the same, the pictures stay, nothing is asked for again; they are centred.
        let (rows, bytes) = (app.thumbs.strip.clone(), app.thumbs.cache.bytes());
        let edge = (app.view_x - 2) as i32;
        app.event(Event::MouseDown { x: edge, y: 300, ctrl: false, shift: false, clicks: 1 });
        app.event(Event::MouseMove { x: edge + 80, y: 300, shift: false });
        assert!(app.thumbs.asked.is_empty(), "nothing is asked for in the middle of the drag");
        app.event(Event::MouseUp { x: edge + 80, y: 300 });
        assert_eq!(app.list_w(), app.px(DEFAULT_WIDTH + 80));
        assert_eq!(app.thumbs.strip.row(0).copied(), Some(row));
        assert_eq!(app.thumbs.strip.total(), rows.total());
        assert!(app.thumbs.fresh(0, 0) && app.thumbs.cache.bytes() >= bytes && app.thumbs.asked.is_empty());
        centred(&mut app, row.img_w);
        // Made as narrow as it goes (120), it is too narrow for the margins: the thumbnails shrink to fit, and are asked for at the new size.
        let edge = (app.view_x - 2) as i32;
        app.event(Event::MouseDown { x: edge, y: 300, ctrl: false, shift: false, clicks: 1 });
        app.event(Event::MouseMove { x: edge - 400, y: 300, shift: false });
        app.event(Event::MouseUp { x: edge - 400, y: 300 });
        assert_eq!(app.sidebar_pref, 120);
        let narrow = app.thumbs.strip.row(0).copied().expect("a row");
        assert_eq!(narrow.img_w, app.list_w() - 2 * app.px(THUMB_MARGIN));
        assert!(narrow.img_w > 0 && narrow.img_w < row.img_w && narrow.scale < row.scale);
        pump(&mut app, "pictures at the new size", |a| a.thumbs.fresh(0, 0));
        assert_eq!(app.thumbs.cache.get(0).map(|t| (i64::from(t.width), i64::from(t.height))), Some((narrow.img_w, narrow.img_h)), "drawn at the size shown");
        assert!(app.thumbs.cache.bytes() <= app.thumbs.cache.cap());
        centred(&mut app, narrow.img_w);
    }

    #[test]
    fn a_saved_width_wins_and_the_thumbnails_stay_the_same_size_in_it() {
        let mut app = app();
        app.apply_settings(&Settings { tab: Some(Tab::Pages), sidebar: Some(300), ..Settings::default() });
        let file = std::env::temp_dir().join(format!("qingpdf-thumbs-{}-saved.pdf", std::process::id()));
        std::fs::write(&file, pages_pdf(&letter(4), &[])).expect("writes");
        app.event(Event::Size { w: 1000, h: 1100 });
        app.open_path(&file, "");
        pump(&mut app, "the file to open", |a| a.has_document());
        let _ = std::fs::remove_file(&file);
        assert_eq!(app.list_w(), app.px(300));
        assert_eq!(app.thumbs.strip.row(0).map(|r| r.img_w), Some(app.px(THUMB_W)));
    }

    #[test]
    fn f4_on_a_file_without_bookmarks_leaves_the_tab_the_person_chose_alone() {
        let mut app = open_pages("remembered", &letter(3), false);
        assert_eq!(app.tab, Tab::Bookmarks);
        app.event(Event::Key { vk: vk::F4, ctrl: false, shift: false });
        // The thumbnails show because there is nothing else to show, but the choice (what is kept for the next run) is not overwritten.
        assert!(app.thumbs_shown() && app.active_tab() == Tab::Pages);
        assert_eq!(app.tab, Tab::Bookmarks);
        // A file with bookmarks, opened next: the bookmarks, as chosen.
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/corpus/public/outline-form-attach/bookmarks.pdf");
        app.open_path(std::path::Path::new(file), "");
        pump(&mut app, "the bookmarks", |a| !a.outline.is_empty());
        assert_eq!(app.active_tab(), Tab::Bookmarks);
    }

    #[test]
    fn requests_for_rows_scrolled_away_are_taken_back_even_while_the_bar_or_the_edge_is_held() {
        let mut app = open_pages("cancel", &letter(500), true);
        pump(&mut app, "the first pictures", |a| a.thumbs.fresh(0, 0) && a.thumbs.asked.is_empty() && a.is_idle());
        app.thumbs.cache.clear();
        for hold in 0..2 {
            // A request for a row far from the view, and the view at the top.
            app.thumbs.asked.insert(90_000 + hold, 400);
            if hold == 0 { app.thumb_drag = true } else { app.split_drag = Some((0, 0)) }
            app.schedule_thumbs();
            assert!(app.thumbs.asked.is_empty(), "taken back, and nothing new while held ({hold})");
            app.thumb_drag = false;
            app.split_drag = None;
        }
        // Let go: the rows in view are asked for.
        app.schedule_thumbs();
        assert!(app.thumbs.asked.values().any(|&p| p == 0));
    }

    fn failing(app: &App, id: u64, page: u32) -> Rendered {
        Rendered { doc: app.doc_id, id, page, status: RenderStatus::Failed, message: "no".to_string(), width: 0, height: 0, bgra: Vec::new() }
    }

    #[test]
    fn a_page_that_failed_is_asked_for_once_more_and_then_drawn_grey() {
        let mut app = open_pages("retry", &letter(3), true);
        pump(&mut app, "the pictures", |a| (0..3).all(|p| a.thumbs.fresh(p, 0)) && a.thumbs.asked.is_empty() && a.is_idle());
        app.thumbs.cache.remove(1);
        app.thumbs.asked.insert(70_000, 1);
        app.on_rendered(failing(&app, 70_000, 1));
        assert!(!app.thumbs.failed.contains(&1), "the first failure is not the last");
        app.schedule_thumbs();
        assert!(app.thumbs.asked.values().any(|&p| p == 1), "asked for again");
        // That one is taken back and the answer is a failure as well: now it is grey, and not asked for again.
        app.cancel_thumbs();
        app.thumbs.asked.insert(70_001, 1);
        app.on_rendered(failing(&app, 70_001, 1));
        assert!(app.thumbs.failed.contains(&1));
        app.schedule_thumbs();
        assert!(app.thumbs.asked.is_empty());
        let mut rec = Rec::default();
        app.paint(&mut rec);
        assert!(rec.fills.iter().any(|(_, c)| *c == app.pal.damaged));
        // A page that was changed gets another chance.
        app.forget_thumb(1);
        assert!(!app.thumbs.failed.contains(&1));
        pump(&mut app, "the picture again", |a| a.thumbs.fresh(1, 0));
    }

    #[test]
    fn a_page_that_is_changed_gets_a_new_picture() {
        let mut app = open_pages("changed", &letter(3), true);
        pump(&mut app, "the pictures", |a| a.thumbs.fresh(0, 0) && a.thumbs.asked.is_empty());
        let bytes = app.thumbs.cache.bytes();
        app.forget_thumb(0);
        assert!(app.thumbs.cache.get(0).is_none() && app.thumbs.cache.bytes() < bytes);
        pump(&mut app, "the picture again", |a| a.thumbs.fresh(0, 0));
    }

    #[test]
    fn the_sidebar_is_drawn_with_its_tabs_in_both_languages_and_the_selected_row_stands_out() {
        let mut app = open_pages("draw", &letter(4), true);
        pump(&mut app, "a picture", |a| a.thumbs.cache.get(0).is_some());
        for (lang, bookmarks, pages) in [(Lang::En, "Bookmarks", "Pages"), (Lang::Zh, "书签", "缩略图")] {
            app.lang = lang;
            let mut rec = Rec::default();
            app.paint(&mut rec);
            assert!(rec.texts.iter().any(|(_, _, s)| s == bookmarks) && rec.texts.iter().any(|(_, _, s)| s == pages), "{lang:?}");
        }
        let mut rec = Rec::default();
        app.paint(&mut rec);
        assert!(rec.fills.iter().any(|(_, c)| *c == app.pal.selected_bg), "the current row is highlighted");
        assert_eq!(rec.fills.iter().filter(|(_, c)| *c == app.pal.selected_bg).count(), 1);
        // The pictures are inside the list, centred, and the placeholders are lighter than the list.
        let width = app.list_w() as i32;
        assert!(rec.pictures.iter().any(|(r, ..)| r.x > 0 && r.x + r.w < width));
    }
}
