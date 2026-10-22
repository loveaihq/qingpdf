//! The thumbnails beside the pages (2026-10-14), as far as they are plain logic: the two tabs of the sidebar, how the rows of the list
//! lie (each a small picture of a page with its number under it), which rows are in view, which pictures to ask the engine for and
//! which to take back, the cache that holds the pictures, and where the keys move to. Nothing here calls Windows or the engine; `app/sidebar.rs`
//! does the asking and the drawing.
//!
//! The list is virtual: a row costs a few numbers, a picture only exists for the rows in view and a few next to them, held to a
//! number of bytes (`view::THUMB_CACHE_BYTES`, part of the memory plan). A thousand-page file asks for a dozen pictures, not a thousand.

use std::ops::Range;

use crate::sched;
use qingpdf_core::idmap::{IdMap, IdSet};

/// The three tabs of the sidebar: the bookmarks, the thumbnails of the pages, the list of the annotations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Bookmarks,
    Pages,
    Annots,
}

impl Tab {
    /// The word kept in the settings file.
    pub fn as_str(self) -> &'static str {
        match self {
            Tab::Bookmarks => "bookmarks",
            Tab::Pages => "pages",
            Tab::Annots => "annots",
        }
    }

    pub fn parse(s: &str) -> Option<Tab> {
        match s.trim().to_ascii_lowercase().as_str() {
            "bookmarks" => Some(Tab::Bookmarks),
            "pages" => Some(Tab::Pages),
            "annots" => Some(Tab::Annots),
            _ => None,
        }
    }
}

/// The tab at this place across a header of `width` pixels: the left third is the bookmarks, the middle third the pages, the right third the annotations.
pub fn tab_at(x: i64, width: i64) -> Tab {
    if x * 3 < width {
        Tab::Bookmarks
    } else if x * 3 < width * 2 {
        Tab::Pages
    } else {
        Tab::Annots
    }
}

/// The most pixels of a picture the engine is asked for (a bigger box is drawn from a smaller picture, stretched).
pub const MAX_RENDER_PIXELS: u64 = 400_000;
/// The rows beyond the view, on each side, whose pictures are asked for as well.
pub const PREFETCH_ROWS: usize = 3;
/// The most pictures asked for at once: the engine draws one at a time, and what the person is looking at comes before them.
pub const MAX_IN_FLIGHT: usize = 2;
/// The priorities of the requests (the engine draws the smallest first): after everything the pages need, the pieces in view, the
/// ring round them and the small pictures of the pages round about (`sched`), all of which are asked for with a smaller number.
pub const P_VISIBLE: u32 = 200_000;
pub const P_NEAR: u32 = 200_100;
const _: () = assert!(P_VISIBLE > sched::P_PREVIEW_NEAR + 1000);
const _: () = assert!(sched::P_PREVIEW_NEAR > qingpdf_core::view::BACKGROUND_PRIORITY);

/// The tallest a picture is shown, in percent of its width: a very tall page stays small, a landscape page is shorter than it is wide.
pub const MAX_HEIGHT_PERCENT: i64 = 150;

/// The width a picture is shown at: `fixed` however wide the list is, shrunk only when the list (`list_w`) is narrower than the picture with
/// `margin` on each side; never less than `least`, and never below 1.
pub fn picture_width(fixed: i64, list_w: i64, margin: i64, least: i64) -> i64 {
    fixed.min(list_w.saturating_sub(margin.saturating_mul(2))).max(least).max(1)
}

/// The room one row takes besides its picture, and the room for the picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metrics {
    /// The widest and the tallest a picture is shown.
    pub avail_w: i64,
    pub max_h: i64,
    /// The room above the picture, and below it (the number and the space after it).
    pub above: i64,
    pub below: i64,
}

impl Metrics {
    /// Pictures at most `avail_w` wide and `MAX_HEIGHT_PERCENT` of that tall.
    pub const fn new(avail_w: i64, above: i64, below: i64) -> Metrics {
        Metrics { avail_w, max_h: avail_w.saturating_mul(MAX_HEIGHT_PERCENT) / 100, above, below }
    }
}

/// One row of the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    pub top: i64,
    pub height: i64,
    /// The picture as it is shown.
    pub img_w: i64,
    pub img_h: i64,
    /// What the engine is asked for: the scale (as `sched::scale_key`: dots per inch times 1000) and the size that comes back.
    pub scale: u64,
    pub render_w: u32,
    pub render_h: u32,
}

impl Row {
    /// The bytes of the picture the engine makes for it.
    pub fn render_bytes(&self) -> u64 {
        u64::from(self.render_w) * u64::from(self.render_h) * 4
    }

    /// The row of a page of `w` by `h` points (as it is shown): the picture is as wide as the room, or as tall, keeping the shape.
    fn fit(w: f64, h: f64, m: &Metrics) -> Row {
        let (w, h) = (if w.is_finite() && w >= 1.0 { w } else { 612.0 }, if h.is_finite() && h >= 1.0 { h } else { 792.0 });
        let per_pt = (m.avail_w.max(1) as f64 / w).min(m.max_h.max(1) as f64 / h);
        let key = ((per_pt * 72_000.0).floor() as u64).max(1);
        let (img_w, img_h) = sched::page_px(w, h, key);
        // A big box is drawn from a picture of at most `MAX_RENDER_PIXELS`, and the engine draws from 1 dot per inch up.
        let pixels = (img_w as u64).saturating_mul(img_h as u64);
        let shrink = if pixels > MAX_RENDER_PIXELS { (MAX_RENDER_PIXELS as f64 / pixels as f64).sqrt() } else { 1.0 };
        let scale = ((key as f64 * shrink).floor() as u64).clamp(1_000, 2_400_000);
        let (rw, rh) = sched::page_px(w, h, scale);
        Row { top: 0, height: 0, img_w, img_h, scale, render_w: u32::try_from(rw).unwrap_or(u32::MAX), render_h: u32::try_from(rh).unwrap_or(u32::MAX) }
    }
}

/// The rows of the list, one for each page, from the top. Positions are pixels from the top of the list.
#[derive(Clone, Debug, Default)]
pub struct Strip {
    rows: Vec<Row>,
    total: i64,
}

impl Strip {
    /// The sizes are the pages' in points, as they are shown (turned by the view).
    pub fn new(sizes: impl IntoIterator<Item = (f64, f64)>, m: &Metrics) -> Strip {
        let sizes = sizes.into_iter();
        // Room for all the rows at once: growing by doubling would hold half as much again for a moment.
        let mut rows = Vec::with_capacity(sizes.size_hint().1.unwrap_or(0));
        let mut top = 0i64;
        for (w, h) in sizes {
            let mut row = Row::fit(w, h, m);
            row.top = top;
            row.height = row.img_h.saturating_add(m.above).saturating_add(m.below);
            top = top.saturating_add(row.height);
            rows.push(row);
        }
        Strip { rows, total: top }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn total(&self) -> i64 {
        self.total
    }

    pub fn row(&self, i: usize) -> Option<&Row> {
        self.rows.get(i)
    }

    /// The rows that show anything between `y0` and `y1`.
    pub fn rows_in(&self, y0: i64, y1: i64) -> Range<usize> {
        let start = self.rows.partition_point(|r| r.top.saturating_add(r.height) <= y0);
        let end = self.rows.partition_point(|r| r.top < y1).max(start);
        start..end
    }

    /// The row that holds the place `y`, if there is one.
    pub fn row_at(&self, y: i64) -> Option<usize> {
        let r = self.rows_in(y, y.saturating_add(1));
        (!r.is_empty() && y >= 0).then_some(r.start)
    }

    pub fn max_scroll(&self, view_h: i64) -> i64 {
        (self.total - view_h).max(0)
    }

    /// The row at `y` and how far down it `y` is (0 to 1), so that the place can be found again when the rows change size.
    pub fn locate(&self, y: i64) -> (usize, f64) {
        let i = self.rows.partition_point(|r| r.top.saturating_add(r.height) <= y).min(self.rows.len().saturating_sub(1));
        match self.rows.get(i) {
            Some(r) => (i, ((y - r.top) as f64 / r.height.max(1) as f64).clamp(0.0, 1.0)),
            None => (0, 0.0),
        }
    }

    pub fn y_of(&self, row: usize, fraction: f64) -> i64 {
        self.rows.get(row.min(self.rows.len().saturating_sub(1))).map_or(0, |r| r.top + (fraction * r.height as f64).round() as i64)
    }

    /// Where to scroll to, from `scroll`, to have a row in view: left alone if it is all in view, moved the least that shows it if it
    /// is partly in view (its top, if it is taller than the view), and put in the middle if it is out of view altogether.
    pub fn reveal(&self, row: usize, scroll: i64, view_h: i64) -> i64 {
        let Some(r) = self.rows.get(row) else { return scroll };
        let bottom = r.top.saturating_add(r.height);
        let to = if r.top >= scroll && bottom <= scroll + view_h {
            scroll
        } else if bottom <= scroll || r.top >= scroll + view_h {
            r.top - (view_h - r.height) / 2
        } else if r.height > view_h || r.top < scroll {
            r.top
        } else {
            bottom - view_h
        };
        to.clamp(0, self.max_scroll(view_h))
    }
}

/// A picture the engine made for a row.
pub struct Thumb {
    pub page: u32,
    pub rotation: u16,
    pub scale: u64,
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

impl Thumb {
    fn bytes(&self) -> u64 {
        self.bgra.len() as u64
    }
}

/// The pictures held, to a number of bytes: when a new one does not fit, the ones on the rows farthest from the rows in view go first.
pub struct ThumbCache {
    map: IdMap<u32, Thumb>,
    bytes: u64,
    cap: u64,
}

impl ThumbCache {
    pub fn new(cap: u64) -> ThumbCache {
        ThumbCache { map: IdMap::new(), bytes: 0, cap }
    }

    pub fn cap(&self) -> u64 {
        self.cap
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn get(&self, page: u32) -> Option<&Thumb> {
        self.map.get(&page)
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.bytes = 0;
    }

    /// Let go of the picture of one page (it was changed).
    pub fn remove(&mut self, page: u32) {
        if let Some(old) = self.map.remove(&page) {
            self.bytes = self.bytes.saturating_sub(old.bytes());
        }
    }

    /// Let go of the pictures drawn at another turn of the view (they are the wrong shape for the rows now).
    pub fn keep_rotation(&mut self, rotation: u16) {
        self.map.retain(|_, t| t.rotation == rotation);
        self.bytes = self.map.values().map(Thumb::bytes).sum();
    }

    /// Put a picture in, making room first: the rows `first` to `last` are in view. The bytes held never go over the cap. A picture bigger
    /// than the cap is not kept (and nothing makes way for it); nor is one farther from the view than everything that would have to go.
    pub fn insert(&mut self, thumb: Thumb, first: u32, last: u32) {
        self.remove(thumb.page);
        let size = thumb.bytes();
        if size > self.cap {
            return;
        }
        let distance = |p: u32| if p < first { first - p } else { p.saturating_sub(last) };
        // The farthest goes first; at the same distance the one behind the view (people read forward).
        let rank = |p: u32| (distance(p), p < first, p);
        while self.bytes.saturating_add(size) > self.cap {
            let Some(page) = self.map.keys().copied().max_by_key(|&p| rank(p)) else { break };
            if rank(page) < rank(thumb.page) {
                return;
            }
            self.remove(page);
        }
        self.bytes = self.bytes.saturating_add(size);
        self.map.insert(thumb.page, thumb);
    }
}

/// A picture to have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Want {
    pub page: u32,
    pub priority: u32,
}

/// The pictures wanted, most urgent first: the rows in view from the top, then the rows next to them, nearest first (below before
/// above). It stops at the row whose picture would take the cache past `cap`, so that what is wanted always fits.
pub fn wanted(strip: &Strip, scroll: i64, view_h: i64, cap: u64) -> Vec<Want> {
    let view = strip.rows_in(scroll, scroll.saturating_add(view_h.max(0)));
    let mut out = Vec::new();
    if view.is_empty() {
        return out;
    }
    let mut bytes = 0u64;
    let mut take = |page: usize, priority: u32| -> bool {
        let Some(row) = strip.row(page) else { return true };
        bytes = bytes.saturating_add(row.render_bytes());
        if bytes > cap {
            return false;
        }
        out.push(Want { page: page as u32, priority });
        true
    };
    for (k, page) in view.clone().enumerate() {
        if !take(page, P_VISIBLE + k.min(99) as u32) {
            return out;
        }
    }
    for d in 1..=PREFETCH_ROWS {
        if !take(view.end - 1 + d, P_NEAR + 2 * d as u32) {
            return out;
        }
        if let Some(above) = view.start.checked_sub(d)
            && !take(above, P_NEAR + 2 * d as u32 + 1)
        {
            return out;
        }
    }
    out
}

/// What to do about the requests: the ones to take back and the ones to make.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub cancel: Vec<u64>,
    pub issue: Vec<Want>,
}

/// Compare what is wanted with what is asked for (request, page) and what is there already (`done`). A request for a page that is not
/// wanted any more is taken back; new ones are made while fewer than `room` are out.
pub fn plan(wanted: &[Want], asked: &IdMap<u64, u32>, done: impl Fn(u32) -> bool, room: usize) -> Plan {
    let mut cancel: Vec<u64> = asked.iter().filter(|(_, page)| !wanted.iter().any(|w| w.page == **page)).map(|(id, _)| *id).collect();
    cancel.sort_unstable();
    let out = asked.len() - cancel.len();
    let issue = wanted.iter().filter(|w| !done(w.page) && !asked.values().any(|&p| p == w.page)).take(room.saturating_sub(out)).copied().collect();
    Plan { cancel, issue }
}

/// Everything the list keeps: the rows, where it is scrolled to, the pictures and the requests for them.
pub struct Thumbs {
    pub strip: Strip,
    pub scroll: i64,
    pub cache: ThumbCache,
    /// The requests out: request, page.
    pub asked: IdMap<u64, u32>,
    /// The pages the engine could not draw (twice), and the ones it could not draw once (they are asked for again).
    pub failed: IdSet<u32>,
    retried: IdSet<u32>,
    /// What the rows were made for.
    built: Option<Built>,
    /// The page the list was last moved to show.
    pub synced: Option<usize>,
    /// The pages changed (turned, taken out, moved, put in): the rows are made again but the list stays about where it is.
    keep_place: bool,
}

/// What the rows depend on: the room, the turn of the view, the number of pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Built {
    pub metrics: Metrics,
    pub rotation: u16,
    pub pages: usize,
}

impl Thumbs {
    pub fn new() -> Thumbs {
        Thumbs { strip: Strip::default(), scroll: 0, cache: ThumbCache::new(0), asked: IdMap::new(), failed: IdSet::new(), retried: IdSet::new(), built: None, synced: None, keep_place: false }
    }

    /// Forget everything (another file, or none), with room for `cap` bytes of pictures.
    pub fn reset(&mut self, cap: u64) {
        *self = Thumbs::new();
        self.cache = ThumbCache::new(cap);
    }

    /// Let go of the pictures (they were made in other colours): they are asked for again.
    pub fn forget_pictures(&mut self) {
        self.cache.clear();
        self.failed.clear();
        self.retried.clear();
    }

    /// Make the rows again if what they depend on has changed (`build` makes them). The place in the list stays on the same row.
    /// `true`: they were made again, so the requests out are for rows that are gone.
    pub fn ensure(&mut self, now: Built, build: impl FnOnce() -> Strip) -> bool {
        if self.built == Some(now) {
            return false;
        }
        let anchor = self.strip.locate(self.scroll);
        let same_file = self.built.is_some_and(|b| b.pages == now.pages) || std::mem::take(&mut self.keep_place);
        if self.built.is_some_and(|b| b.rotation != now.rotation) {
            self.cache.keep_rotation(now.rotation);
        }
        // The old rows go first, so that two lists of a hundred thousand rows are not held at once.
        self.strip = Strip::default();
        self.strip = build();
        self.scroll = if same_file { self.strip.y_of(anchor.0, anchor.1) } else { 0 };
        self.failed.clear();
        self.retried.clear();
        self.built = Some(now);
        true
    }

    /// The pages are not the pages they were: no picture is any good, and the rows are made again; the list stays where it is.
    pub fn pages_changed(&mut self) {
        self.cache.clear();
        self.failed.clear();
        self.retried.clear();
        self.built = None;
        self.keep_place = true;
    }

    /// The engine could not draw a page: it is asked for once more (it may have been a passing trouble), and after a second failure the
    /// page stays a grey box.
    pub fn note_failure(&mut self, page: u32) {
        if !self.retried.insert(page) {
            self.failed.insert(page);
        }
    }

    /// Forget what is known of the failures of a page (it was changed): its picture is made again from the start.
    pub fn forget_failure(&mut self, page: u32) {
        self.failed.remove(&page);
        self.retried.remove(&page);
    }

    pub fn clamp(&mut self, view_h: i64) {
        self.scroll = self.scroll.clamp(0, self.strip.max_scroll(view_h));
    }

    /// Whether the picture of a page is there (with the shape the row has now) or the engine has said it cannot draw it.
    pub fn is_done(&self, page: u32, rotation: u16) -> bool {
        self.failed.contains(&page) || self.fresh(page, rotation)
    }

    /// Is the picture there, for this row's scale and this turn?
    pub fn fresh(&self, page: u32, rotation: u16) -> bool {
        match (self.cache.get(page), self.strip.row(page as usize)) {
            (Some(t), Some(r)) => t.scale == r.scale && t.rotation == rotation,
            _ => false,
        }
    }
}

impl Default for Thumbs {
    fn default() -> Self {
        Thumbs::new()
    }
}

/// A key of the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListKey {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
}

/// The page a key goes to from page `current`, among `count` pages; `screen` is the rows in view (Page Up and Page Down go one fewer, but at
/// least one).
pub fn key_target(key: ListKey, current: usize, count: usize, screen: usize) -> usize {
    let last = count.saturating_sub(1);
    let page = screen.saturating_sub(1).max(1);
    match key {
        ListKey::Up => current.saturating_sub(1),
        ListKey::Down => current.saturating_add(1),
        ListKey::PageUp => current.saturating_sub(page),
        ListKey::PageDown => current.saturating_add(page),
        ListKey::Home => 0,
        ListKey::End => last,
    }
    .min(last)
}

/// The thumb of the thin bar at the side of a list of `total` pixels seen through `view_h`, as (top, length); none if all of it is in view.
pub fn bar(scroll: i64, view_h: i64, total: i64) -> Option<(i64, i64)> {
    if total <= view_h || view_h <= 0 {
        return None;
    }
    let len = crate::ui::clamp_to(view_h * view_h / total, 24.min(view_h), view_h);
    let top = scroll * (view_h - len) / (total - view_h);
    Some((crate::ui::clamp_to(top, 0, view_h - len), len))
}

/// Where the list is scrolled to when the thumb of the bar is dragged to have its middle at `y` of the view.
pub fn scroll_at_bar(y: i64, view_h: i64, total: i64) -> i64 {
    let Some((_, len)) = bar(0, view_h, total) else { return 0 };
    let room = (view_h - len).max(1);
    crate::ui::clamp_to(crate::ui::clamp_to(y - len / 2, 0, room) * (total - view_h) / room, 0, (total - view_h).max(0))
}

// --- choosing thumbnails and moving them (2026-10-15) ------------------------------------------------------------------------------

/// The thumbnails that are chosen (ascending, each once), and the one the next Shift+click counts from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Chosen {
    set: Vec<u32>,
    anchor: u32,
}

impl Chosen {
    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    pub fn len(&self) -> usize {
        self.set.len()
    }

    pub fn contains(&self, page: usize) -> bool {
        u32::try_from(page).is_ok_and(|p| self.set.binary_search(&p).is_ok())
    }

    pub fn clear(&mut self) {
        self.set.clear();
    }

    /// A click: this page and no other.
    pub fn only(&mut self, page: usize) {
        self.anchor = u32::try_from(page).unwrap_or(u32::MAX);
        self.set.clear();
        self.set.push(self.anchor);
    }

    /// Ctrl+click: this page is chosen if it was not, and not if it was.
    pub fn toggle(&mut self, page: usize) {
        let p = u32::try_from(page).unwrap_or(u32::MAX);
        match self.set.binary_search(&p) {
            Ok(i) => drop(self.set.remove(i)),
            Err(i) => self.set.insert(i, p),
        }
        self.anchor = p;
    }

    /// Shift+click: the pages from the one last clicked to this one, and no others.
    pub fn up_to(&mut self, page: usize) {
        if self.set.is_empty() {
            return self.only(page);
        }
        let p = u32::try_from(page).unwrap_or(u32::MAX);
        self.set = (self.anchor.min(p)..=self.anchor.max(p)).collect();
    }

    /// Ctrl+A.
    pub fn all(&mut self, count: usize) {
        self.set = (0..u32::try_from(count).unwrap_or(u32::MAX)).collect();
        self.anchor = 0;
    }

    /// These pages and no other.
    pub fn set(&mut self, pages: impl IntoIterator<Item = u32>) {
        let mut v: Vec<u64> = pages.into_iter().map(u64::from).collect();
        qingpdf_core::order::sort_u64(&mut v);
        v.dedup();
        self.set = v.into_iter().map(|p| p as u32).collect();
        self.anchor = self.set.first().copied().unwrap_or(0);
    }

    /// The chosen pages, in order.
    pub fn pages(&self) -> Vec<u32> {
        self.set.clone()
    }
}

/// The place between two rows (0: above the first, the number of rows: below the last) that a place `y` of the list is nearest to.
pub fn gap_at(strip: &Strip, y: i64) -> usize {
    match strip.row_at(y) {
        Some(i) => strip.row(i).map_or(i, |r| if y < r.top.saturating_add(r.height / 2) { i } else { i + 1 }),
        None if y < 0 => 0,
        None => strip.len(),
    }
}

/// Where the line that marks a place between rows is drawn, in the list's own pixels.
pub fn gap_top(strip: &Strip, gap: usize) -> i64 {
    match strip.row(gap) {
        Some(r) => r.top,
        None => strip.total(),
    }
}

/// Dropping the chosen pages (ascending) in front of row `gap` leaves them where they are: they are next to each other and the place is
/// in front of the first, in front of the last, or between.
pub fn leaves_in_place(chosen: &[u32], gap: usize) -> bool {
    let (Some(&first), Some(&last)) = (chosen.first(), chosen.last()) else { return true };
    let together = (last - first) as usize + 1 == chosen.len();
    together && gap >= first as usize && gap <= last as usize + 1
}

/// Where the chosen pages are after they are put in front of row `gap`: their places in the order `qingpdf_core::view::order_after_move` gives.
pub fn places_after_move(count: usize, chosen: &[u32], gap: usize) -> Vec<u32> {
    let order = qingpdf_core::view::order_after_move(count, chosen, gap);
    order.iter().enumerate().filter(|(_, p)| chosen.binary_search(p).is_ok()).map(|(j, _)| j as u32).collect()
}

/// How far to scroll a list that spans `top` to `bottom` while a drag holds the pointer at `y`: nothing in the middle, and more the
/// nearer to (or past) an edge, within `edge` pixels of it. Negative scrolls up.
pub fn edge_scroll(y: i64, top: i64, bottom: i64, edge: i64, speed: i64) -> i64 {
    let (above, below) = (top + edge - y, y - (bottom - edge));
    if above > 0 {
        -(speed * above.min(edge * 2) / edge.max(1)).max(1)
    } else if below > 0 {
        (speed * below.min(edge * 2) / edge.max(1)).max(1)
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: Metrics = Metrics::new(130, 8, 28);

    fn letter(n: usize) -> Strip {
        Strip::new(std::iter::repeat_n((612.0, 792.0), n), &M)
    }

    fn thumb(page: u32, bytes: usize, scale: u64) -> Thumb {
        Thumb { page, rotation: 0, scale, width: 10, height: (bytes / 40) as u32, bgra: vec![0; bytes] }
    }

    #[test]
    fn a_picture_is_130_wide_and_keeps_the_shape_of_the_page() {
        let s = letter(3);
        let r = s.row(0).expect("a row");
        assert_eq!((r.img_w, r.img_h), (130, 168), "130 wide, 792/612 as tall");
        assert_eq!(r.height, 168 + 8 + 28);
        // The engine is asked for the size it is shown at, so that nothing is stretched.
        assert_eq!((i64::from(r.render_w), i64::from(r.render_h)), (r.img_w, r.img_h));
        assert_eq!(s.row(1).map(|r| r.top), Some(r.height));
        assert_eq!(s.total(), 3 * r.height);
    }

    #[test]
    fn a_page_turned_a_quarter_is_wide_and_a_tall_page_is_as_tall_as_the_room_allows() {
        // 792 by 612 (a landscape page, or a portrait page turned): wider than tall.
        let wide = Strip::new([(792.0, 612.0)], &M);
        let r = wide.row(0).expect("a row");
        assert_eq!((r.img_w, r.img_h), (130, 100), "wide: 130 wide and shorter");
        // A page 100 points wide and 2000 tall would be 2600 pixels tall at the width of the room: it is cut to one and a half times the width.
        let tall = Strip::new([(100.0, 2000.0)], &M);
        let r = tall.row(0).expect("a row");
        assert_eq!(r.img_h, 195);
        assert_eq!(r.img_w, 10);
    }

    #[test]
    fn a_page_without_a_size_gets_a_letter_page_and_a_huge_box_is_drawn_small_and_stretched() {
        let s = Strip::new([(0.0, 0.0), (f64::NAN, f64::NAN), (-3.0, -4.0)], &M);
        for i in 0..3 {
            assert_eq!(s.row(i).map(|r| (r.img_w, r.img_h)), Some((130, 168)), "{i}");
        }
        let big = Metrics::new(2000, 8, 28);
        let r = *Strip::new([(612.0, 792.0)], &big).row(0).expect("a row");
        assert_eq!(r.img_w, 2000);
        assert!(r.render_bytes() <= MAX_RENDER_PIXELS * 4 + 8000, "{r:?}");
        assert!(r.render_w < 700 && r.scale >= 1_000);
        // Never below one dot per inch, which is where the engine starts.
        let tiny = Metrics::new(20, 0, 0);
        let r = *Strip::new([(14_400.0, 14_400.0)], &tiny).row(0).expect("a row");
        assert_eq!(r.scale, 1_000);
    }

    #[test]
    fn the_rows_in_view_are_found_by_position_and_a_place_finds_its_row() {
        let s = letter(1000);
        let h = s.row(0).map_or(1, |r| r.height);
        assert_eq!(s.rows_in(0, 1), 0..1);
        assert_eq!(s.rows_in(0, h), 0..1);
        assert_eq!(s.rows_in(0, h + 1), 0..2);
        assert_eq!(s.rows_in(h, h + 1), 1..2);
        assert_eq!(s.rows_in(10 * h - 1, 12 * h), 9..12);
        assert!(s.rows_in(s.total(), s.total() + 10).is_empty());
        assert!(s.rows_in(-50, -1).is_empty());
        assert_eq!((s.row_at(0), s.row_at(h - 1), s.row_at(h), s.row_at(-1), s.row_at(s.total())), (Some(0), Some(0), Some(1), None, None));
        assert!(Strip::default().rows_in(0, 100).is_empty());
        assert_eq!(Strip::default().row_at(5), None);
    }

    #[test]
    fn a_place_is_found_again_when_the_rows_change_size() {
        let s = letter(50);
        let y = s.y_of(7, 0.5);
        let (row, f) = s.locate(y);
        assert_eq!(row, 7);
        assert!((f - 0.5).abs() < 0.01);
        let narrow = Strip::new(std::iter::repeat_n((612.0, 792.0), 50), &Metrics::new(100, 8, 28));
        let (row, _) = narrow.locate(narrow.y_of(row, f));
        assert_eq!(row, 7);
        // Past the end, the last row.
        assert_eq!(s.locate(i64::MAX / 2).0, 49);
        assert_eq!(Strip::default().locate(10), (0, 0.0));
    }

    #[test]
    fn a_row_is_brought_into_view_by_the_least_movement() {
        let s = letter(100);
        let h = s.row(0).map_or(1, |r| r.height);
        let view = 3 * h + 20;
        // All in view: nothing moves.
        assert_eq!(s.reveal(1, 0, view), 0);
        // The row just below the view comes up by what shows it (its bottom at the bottom of the view).
        assert_eq!(s.reveal(3, 0, view), 4 * h - view);
        // Partly above the view: its top at the top.
        assert_eq!(s.reveal(2, 2 * h + 30, view), 2 * h);
        // Far away: in the middle, but never past either end.
        let mid = s.reveal(50, 0, view);
        assert!(mid > 0 && mid <= s.row(50).map_or(0, |r| r.top) && mid + view >= s.row(50).map_or(0, |r| r.top + r.height));
        assert_eq!(s.reveal(99, 0, view), s.max_scroll(view));
        assert_eq!(s.reveal(0, 5 * h, view), 0);
        // A row taller than the view: its top.
        assert_eq!(s.reveal(4, 4 * h - 20, h / 2), 4 * h);
        assert_eq!(s.reveal(500, 7, view), 7, "no such row");
    }

    #[test]
    fn only_the_rows_near_the_view_are_wanted_even_in_a_file_of_a_thousand_pages() {
        let s = letter(1000);
        let h = s.row(0).map_or(1, |r| r.height);
        let w = wanted(&s, 0, 1000, 8_000_000);
        // The rows in view come first, in order, then the ring: below first, then above.
        let pages: Vec<u32> = w.iter().map(|w| w.page).collect();
        assert_eq!(pages.first(), Some(&0));
        let in_view = s.rows_in(0, 1000).len();
        assert!(w.len() <= in_view + 2 * PREFETCH_ROWS && w.len() >= in_view, "{} wanted", w.len());
        assert!(w.windows(2).all(|p| p[0].priority < p[1].priority || p[0].priority >= P_NEAR), "ordered");
        assert!(w.iter().take(in_view).all(|w| w.priority < P_NEAR) && w.iter().skip(in_view).all(|w| w.priority >= P_NEAR));
        // In the middle of the file: the same few, round the view.
        let mid = wanted(&s, 500 * h, 1000, 8_000_000);
        assert!(mid.len() <= in_view + 2 * PREFETCH_ROWS);
        let mut sorted: Vec<u32> = mid.iter().map(|w| w.page).collect();
        sorted.sort_unstable();
        assert!(sorted.first().is_some_and(|&p| p >= 495) && sorted.last().is_some_and(|&p| p <= 510), "{sorted:?}");
        // At the end there is no row to ask for beyond it.
        let end = wanted(&s, s.max_scroll(1000), 1000, 8_000_000);
        assert!(end.iter().all(|w| w.page < 1000) && end.iter().any(|w| w.page == 999));
        // Nothing is in view: nothing is wanted.
        assert!(wanted(&s, 0, 0, 8_000_000).is_empty() && wanted(&Strip::default(), 0, 500, 8_000_000).is_empty());
    }

    #[test]
    fn what_is_wanted_always_fits_the_cache() {
        let s = letter(100);
        let one = s.row(0).map_or(0, Row::render_bytes);
        for rows in 1..12u64 {
            let w = wanted(&s, 0, 2000, rows * one);
            assert!(w.len() as u64 <= rows, "{} for {rows}", w.len());
            assert!(!w.is_empty() || rows == 0);
        }
        assert!(wanted(&s, 0, 2000, one - 1).is_empty());
    }

    #[test]
    fn the_cache_holds_to_its_bytes_and_lets_the_farthest_rows_go_first() {
        let mut c = ThumbCache::new(1000);
        for p in 0..10 {
            c.insert(thumb(p, 100, 1), 4, 5);
        }
        assert_eq!((c.bytes(), c.len()), (1000, 10));
        // One more, with the view now at rows 8 and 9: the farthest from them goes.
        c.insert(thumb(10, 100, 1), 8, 9);
        assert_eq!((c.bytes(), c.len()), (1000, 10));
        assert!(c.get(0).is_none() && c.get(9).is_some() && c.get(10).is_some());
        // A picture far from the view that does not fit is the one dropped; nothing else is touched.
        c.insert(thumb(50, 100, 1), 8, 9);
        assert!(c.get(50).is_none() && c.get(4).is_some() && c.get(5).is_some());
        assert_eq!((c.bytes(), c.len()), (1000, 10));
        // Replacing a picture counts its bytes once.
        c.insert(thumb(4, 300, 2), 8, 9);
        assert!(c.bytes() <= 1000 && c.get(4).map(|t| t.scale) == Some(2));
        // A picture bigger than the cache is turned away at the door: nobody makes way for it, and the cache is never over its bytes.
        let (bytes, len) = (c.bytes(), c.len());
        let had_five = u64::from(c.get(5).is_some());
        c.insert(thumb(30, 1001, 1), 8, 9);
        assert!(c.get(30).is_none() && (c.bytes(), c.len()) == (bytes, len), "{} bytes in {} pictures", c.bytes(), c.len());
        c.insert(thumb(5, 5000, 1), 8, 9);
        assert!(c.get(5).is_none() && c.bytes() == bytes - 100 * had_five && c.len() == len - had_five as usize);
        // Whatever goes in, the bytes held never pass the cap, not even for a moment after an insert.
        for p in 0..200u32 {
            c.insert(thumb(p * 7 % 40, 70 + (p as usize * 37) % 400, 1), p % 30, p % 30 + 3);
            assert!(c.bytes() <= c.cap(), "{} bytes after {p}", c.bytes());
            assert_eq!(c.bytes(), c.map.values().map(Thumb::bytes).sum::<u64>(), "the count is the sum");
        }
        c.clear();
        assert!(c.is_empty() && c.bytes() == 0);
    }

    #[test]
    fn a_picture_is_taken_out_by_its_page_and_a_page_without_one_is_no_matter() {
        let mut c = ThumbCache::new(10_000);
        c.insert(thumb(3, 100, 1), 0, 0);
        c.insert(thumb(4, 200, 1), 0, 0);
        c.remove(3);
        c.remove(99);
        assert_eq!((c.len(), c.bytes()), (1, 200));
        assert!(c.get(3).is_none() && c.get(4).is_some());
    }

    #[test]
    fn pictures_drawn_at_another_turn_are_let_go_of() {
        let mut c = ThumbCache::new(10_000);
        c.insert(thumb(0, 100, 1), 0, 0);
        c.insert(Thumb { rotation: 90, ..thumb(1, 100, 1) }, 0, 0);
        c.keep_rotation(90);
        assert_eq!((c.len(), c.bytes()), (1, 100));
        assert!(c.get(1).is_some());
    }

    #[test]
    fn requests_for_rows_that_are_no_longer_wanted_are_taken_back_and_few_are_made_at_a_time() {
        let want: Vec<Want> = (3..9).map(|p| Want { page: p, priority: P_VISIBLE + p }).collect();
        let none = IdMap::new();
        // Nothing asked: the first two are asked for (the room), in order.
        let p = plan(&want, &none, |_| false, MAX_IN_FLIGHT);
        assert_eq!((p.cancel.len(), p.issue.iter().map(|w| w.page).collect::<Vec<_>>()), (0, vec![3, 4]));
        // Page 3 is there already: the next two.
        let p = plan(&want, &none, |p| p == 3, MAX_IN_FLIGHT);
        assert_eq!(p.issue.iter().map(|w| w.page).collect::<Vec<_>>(), vec![4, 5]);
        // 4 is asked for, and 1 (scrolled away) too: 1 is taken back, 4 is not asked again, and one more is asked for to fill the room.
        let asked: IdMap<u64, u32> = [(7, 4), (8, 1)].into_iter().collect();
        let p = plan(&want, &asked, |p| p == 3, MAX_IN_FLIGHT);
        assert_eq!(p.cancel, vec![8]);
        assert_eq!(p.issue.iter().map(|w| w.page).collect::<Vec<_>>(), vec![5]);
        // No room (the pages are being drawn): nothing new, but the far ones still go.
        let p = plan(&want, &asked, |_| false, 0);
        assert_eq!((p.cancel, p.issue.len()), (vec![8], 0));
        // Everything there: nothing to do.
        assert_eq!(plan(&want, &none, |_| true, MAX_IN_FLIGHT), Plan::default());
    }

    #[test]
    fn the_keys_move_by_one_page_or_a_screen_and_stop_at_the_ends() {
        use ListKey::*;
        let k = |key, cur| key_target(key, cur, 10, 4);
        assert_eq!((k(Down, 3), k(Up, 3), k(Up, 0), k(Down, 9)), (4, 2, 0, 9));
        assert_eq!((k(PageDown, 2), k(PageUp, 8), k(PageUp, 1), k(PageDown, 8)), (5, 5, 0, 9));
        assert_eq!((k(Home, 6), k(End, 2)), (0, 9));
        // A screen of one row still moves a page.
        assert_eq!(key_target(PageDown, 2, 10, 1), 3);
        assert_eq!(key_target(PageDown, 0, 10, 0), 1);
        // No pages: stays at 0.
        assert_eq!((key_target(Down, 0, 0, 3), key_target(End, 0, 0, 3)), (0, 0));
    }

    #[test]
    fn the_bar_shows_where_the_view_is_and_dragging_it_scrolls() {
        assert_eq!(bar(0, 500, 400), None);
        assert_eq!(bar(0, 0, 400), None);
        let (top, len) = bar(0, 500, 5000).expect("a bar");
        assert_eq!((top, len), (0, 50));
        let (top, len) = bar(4500, 500, 5000).expect("a bar");
        assert_eq!(top + len, 500);
        let (top, _) = bar(2250, 500, 5000).expect("a bar");
        assert!((top - 225).abs() <= 1);
        // The middle of the thumb at the top or the bottom of the track: the ends; in the middle of the track: the middle.
        assert_eq!(scroll_at_bar(0, 500, 5000), 0);
        assert_eq!(scroll_at_bar(500, 500, 5000), 4500);
        assert!((scroll_at_bar(250, 500, 5000) - 2250).abs() < 20);
        assert_eq!(scroll_at_bar(100, 500, 300), 0);
        // A very long list keeps a thumb that can be taken hold of.
        assert_eq!(bar(0, 500, 10_000_000).map(|b| b.1), Some(24));
    }

    #[test]
    fn the_rows_are_made_again_only_when_something_they_depend_on_changes_and_the_place_stays() {
        let mut t = Thumbs::new();
        t.reset(1000);
        let built = Built { metrics: M, rotation: 0, pages: 100 };
        assert!(t.ensure(built, || letter(100)));
        assert!(!t.ensure(built, || panic!("not made again")));
        t.scroll = t.strip.y_of(20, 0.0);
        let narrow = Metrics::new(100, 8, 28);
        assert!(t.ensure(Built { metrics: narrow, ..built }, || Strip::new(std::iter::repeat_n((612.0, 792.0), 100), &narrow)));
        assert_eq!(t.strip.locate(t.scroll).0, 20, "still on row 20");
        // Another file: back to the top, nothing kept.
        t.failed.insert(3);
        assert!(t.ensure(Built { pages: 7, ..built }, || letter(7)));
        assert_eq!(t.scroll, 0);
        assert!(t.failed.is_empty());
        t.scroll = 99_999;
        t.clamp(500);
        assert_eq!(t.scroll, t.strip.max_scroll(500));
    }

    #[test]
    fn a_picture_counts_as_there_only_for_the_scale_and_the_turn_of_its_row() {
        let mut t = Thumbs::new();
        t.reset(100_000);
        t.ensure(Built { metrics: M, rotation: 0, pages: 3 }, || letter(3));
        let scale = t.strip.row(1).map_or(0, |r| r.scale);
        t.cache.insert(thumb(1, 400, scale), 0, 2);
        assert!(t.fresh(1, 0) && t.is_done(1, 0));
        assert!(!t.fresh(1, 90) && !t.fresh(0, 0) && !t.fresh(7, 0));
        t.cache.insert(thumb(2, 400, scale + 5), 0, 2);
        assert!(!t.fresh(2, 0) && !t.is_done(2, 0), "drawn for another width: asked for again");
        t.failed.insert(2);
        assert!(t.is_done(2, 0), "the engine cannot draw it: not asked for again");
    }

    #[test]
    fn the_picture_keeps_its_width_however_wide_the_list_is_and_shrinks_only_in_a_narrow_one() {
        // 130 with 16 on each side: the list needs 162.
        assert_eq!(picture_width(130, 400, 16, 32), 130);
        assert_eq!(picture_width(130, 162, 16, 32), 130);
        assert_eq!(picture_width(130, 161, 16, 32), 129);
        assert_eq!(picture_width(130, 120, 16, 32), 88);
        // Never less than `least`, never zero or negative.
        assert_eq!(picture_width(130, 40, 16, 32), 32);
        assert_eq!(picture_width(130, -5, 16, 0), 1);
        assert_eq!(picture_width(130, i64::MIN, i64::MAX, 0), 1);
        // The height is held to one and a half times the width.
        let m = Metrics::new(130, 8, 28);
        assert_eq!((m.avail_w, m.max_h, m.above, m.below), (130, 195, 8, 28));
        assert_eq!(Metrics::new(1, 0, 0).max_h, 1);
        // Whatever the width of the list, the rows depend on the same numbers while it is wide enough: they are not made again.
        let mut t = Thumbs::new();
        t.reset(1000);
        let at = |list_w| Built { metrics: Metrics::new(picture_width(130, list_w, 16, 32), 8, 28), rotation: 0, pages: 100 };
        assert!(t.ensure(at(184), || letter(100)));
        for list_w in [162, 200, 400, 800] {
            assert!(!t.ensure(at(list_w), || panic!("rows made again at {list_w}")));
        }
        assert!(t.ensure(at(140), || Strip::new(std::iter::repeat_n((612.0, 792.0), 100), &Metrics::new(108, 8, 28))));
    }

    #[test]
    fn the_rows_are_made_with_room_for_all_of_them_at_once() {
        let s = Strip::new((0..100u32).filter(|&i| i < 1000).map(|_| (612.0, 792.0)), &M);
        assert_eq!(s.len(), 100);
        assert!(s.rows.capacity() < 125, "{} rows in room for {}", s.len(), s.rows.capacity());
    }

    #[test]
    fn a_page_the_engine_cannot_draw_is_asked_for_once_more_and_then_stays_grey() {
        let mut t = Thumbs::new();
        t.reset(1000);
        let built = Built { metrics: M, rotation: 0, pages: 3 };
        t.ensure(built, || letter(3));
        t.note_failure(1);
        assert!(!t.is_done(1, 0) && t.failed.is_empty(), "one failure: asked for again");
        t.note_failure(1);
        assert!(t.is_done(1, 0) && t.failed.contains(&1), "two: a grey box");
        assert!(!t.is_done(2, 0));
        // A page that was changed starts again, and so do all of them when the rows are made again.
        t.forget_failure(1);
        assert!(!t.is_done(1, 0));
        t.note_failure(1);
        t.note_failure(1);
        assert!(t.ensure(Built { rotation: 90, ..built }, || letter(3)));
        assert!(t.failed.is_empty() && !t.is_done(1, 90));
        t.note_failure(1);
        assert!(!t.is_done(1, 90), "the one more try is back");
    }

    #[test]
    fn the_tabs_are_the_thirds_of_the_header_and_their_words_are_kept_in_the_settings() {
        assert_eq!((tab_at(0, 270), tab_at(89, 270), tab_at(90, 270), tab_at(179, 270), tab_at(180, 270), tab_at(269, 270)), (Tab::Bookmarks, Tab::Bookmarks, Tab::Pages, Tab::Pages, Tab::Annots, Tab::Annots));
        for t in [Tab::Bookmarks, Tab::Pages, Tab::Annots] {
            assert_eq!(Tab::parse(t.as_str()), Some(t));
        }
        assert_eq!(Tab::parse(" Pages "), Some(Tab::Pages));
        assert_eq!(Tab::parse("thumbnails"), None);
        assert_eq!(Tab::default(), Tab::Bookmarks);
    }

    // --- choosing and moving ---

    #[test]
    fn clicks_choose_pages_the_way_a_list_does() {
        let mut c = Chosen::default();
        assert!(c.is_empty());
        c.only(3);
        assert_eq!(c.pages(), [3]);
        c.toggle(5);
        c.toggle(1);
        assert_eq!(c.pages(), [1, 3, 5]);
        c.toggle(3);
        assert_eq!(c.pages(), [1, 5]);
        assert!(c.contains(5) && !c.contains(3) && !c.contains(usize::MAX));
        // Shift+click counts from the page last clicked, either way, and drops the others.
        c.up_to(8);
        assert_eq!(c.pages(), [3, 4, 5, 6, 7, 8], "from page 3, which was clicked last");
        c.up_to(0);
        assert_eq!(c.pages(), [0, 1, 2, 3]);
        c.only(9);
        c.up_to(7);
        assert_eq!(c.pages(), [7, 8, 9]);
        // With nothing chosen a Shift+click is a click.
        c.clear();
        c.up_to(4);
        assert_eq!(c.pages(), [4]);
        c.all(5);
        assert_eq!((c.len(), c.pages()), (5, vec![0, 1, 2, 3, 4]));
        c.set([6, 2, 6]);
        assert_eq!(c.pages(), [2, 6]);
        c.all(0);
        assert!(c.is_empty());
    }

    #[test]
    fn the_place_a_drop_goes_to_is_the_gap_nearest_the_pointer() {
        let s = letter(4);
        let h = s.row(0).map_or(1, |r| r.height);
        assert_eq!(gap_at(&s, -50), 0);
        assert_eq!(gap_at(&s, 0), 0);
        assert_eq!(gap_at(&s, h / 2 - 1), 0);
        assert_eq!(gap_at(&s, h / 2), 1);
        assert_eq!(gap_at(&s, h + 5), 1);
        assert_eq!(gap_at(&s, h + h / 2 + 1), 2);
        assert_eq!(gap_at(&s, 4 * h - 1), 4);
        assert_eq!(gap_at(&s, 4 * h + 100), 4);
        assert_eq!((gap_top(&s, 0), gap_top(&s, 2), gap_top(&s, 4), gap_top(&s, 9)), (0, 2 * h, 4 * h, 4 * h));
        assert_eq!(gap_at(&Strip::default(), 10), 0);
    }

    #[test]
    fn a_drop_that_changes_nothing_is_known_and_the_places_after_a_move_are_worked_out() {
        // The block 3..=4 dropped in front of 3, 4 or 5 stays; in front of 2 or 6 it moves.
        for (gap, stays) in [(2, false), (3, true), (4, true), (5, true), (6, false), (0, false)] {
            assert_eq!(leaves_in_place(&[3, 4], gap), stays, "gap {gap}");
        }
        // Pages that are apart are gathered by any drop; nothing chosen changes nothing.
        assert!(!leaves_in_place(&[1, 4], 2) && !leaves_in_place(&[1, 4], 5));
        assert!(leaves_in_place(&[], 3));
        assert!(leaves_in_place(&[2], 2) && leaves_in_place(&[2], 3) && !leaves_in_place(&[2], 4));
        // Where the pages that were moved are afterwards.
        assert_eq!(places_after_move(6, &[1, 4], 0), [0, 1]);
        assert_eq!(places_after_move(6, &[1, 2], 6), [4, 5]);
        assert_eq!(places_after_move(6, &[0], 3), [2]);
        assert_eq!(places_after_move(6, &[5], 2), [2]);
    }

    #[test]
    fn the_list_scrolls_faster_the_nearer_the_pointer_is_to_an_edge_and_not_at_all_in_the_middle() {
        let scroll = |y| edge_scroll(y, 100, 500, 40, 20);
        assert_eq!(scroll(300), 0);
        assert_eq!(scroll(140), 0);
        assert!(scroll(130) < 0 && scroll(110) < scroll(130), "{} {}", scroll(130), scroll(110));
        assert!(scroll(470) > 0 && scroll(495) > scroll(470));
        // Past the edge it speeds up to a limit (twice the width of the edge zone), and never stops short of one step.
        assert_eq!(scroll(-1000), -40);
        assert_eq!(scroll(9000), 40);
        assert_eq!(scroll(139), -1);
        assert_eq!(edge_scroll(-5, 0, 10, 0, 5), -1, "an edge of nothing does not divide by zero");
        assert_eq!(edge_scroll(5, 0, 10, 0, 5), 0);
    }

    #[test]
    fn changed_pages_make_the_rows_again_and_the_list_stays_where_it_was() {
        let mut t = Thumbs::new();
        t.reset(1 << 20);
        let built = |pages| Built { metrics: M, rotation: 0, pages };
        assert!(t.ensure(built(100), || letter(100)));
        t.scroll = t.strip.y_of(40, 0.0);
        t.cache.insert(thumb(40, 400, 1), 40, 44);
        t.failed.insert(2);
        // The same file: the rows stay where they are (the number of pages is the same); after a change to the pages they are made again.
        assert!(!t.ensure(built(100), || letter(100)));
        t.pages_changed();
        assert!(t.cache.is_empty() && t.failed.is_empty());
        assert!(t.ensure(built(100), || letter(100)), "the rows are made again though the number of pages is the same");
        assert_eq!(t.strip.locate(t.scroll).0, 40, "the list is where it was");
        // Fewer pages: the list is where it was, as far as it can be.
        t.pages_changed();
        assert!(t.ensure(built(30), || letter(30)));
        assert_eq!(t.strip.locate(t.scroll).0, 29);
        // And a new file starts at the top again.
        t.reset(1 << 20);
        assert!(t.ensure(built(30), || letter(30)));
        assert_eq!(t.scroll, 0);
    }
}
