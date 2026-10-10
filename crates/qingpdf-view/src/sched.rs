//! What to ask the engine to draw, and when to take a request back. Given what the window shows (which pages, at
//! what zoom, whether the view is still moving) this lists the pieces wanted, most urgent first; comparing that
//! with what is cached and what is already asked for gives the requests to send and the ones to cancel.
//!
//! What is in view is always asked for, whatever the cache holds: if it does not fit the cache at the zoom the window
//! is at, it is asked for at a smaller scale and stretched ([`draw_scale`]), never left blank. Only what is next to
//! the view (the pages about to be turned to, the rows about to be scrolled to) is held to the cache's bytes.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use crate::cache::{BitmapCache, Key};
use crate::layout::Layout;
use crate::tiles;

/// The most pixels of a preview (a small picture of a whole page).
pub const PREVIEW_PIXELS: f64 = 64_000.0;
/// How many pages on each side of the view have a preview. (The top of the next two pages and of the one before is also
/// drawn in full: see [`wanted`].)
const PREVIEW_RANGE: usize = 8;

/// Priorities (the engine draws the smallest first).
const P_PREVIEW_IN_VIEW: u32 = 0;
const P_IN_VIEW: u32 = 10;
const P_RING: u32 = 1000;
const P_PREVIEW_NEAR: u32 = 100_000;
// What is in view is drawn before the engine's quick requests (links, bookmarks, boxes of characters), the ring round it after them.
const _: () = assert!(P_RING == qingpdf_core::view::BACKGROUND_PRIORITY);

/// The scale is cut by this much at a time when the pieces in view do not fit the cache, down to a tenth.
const REDUCE_STEP: f64 = 0.8;
const LOWEST_FRACTION: u64 = 10;
/// The lowest scale key the engine can be asked for: 1 dpi.
const LOWEST_SCALE: u64 = 1000;

/// What the window shows.
pub struct Scene<'a> {
    pub layout: &'a Layout,
    pub scroll_x: i64,
    pub scroll_y: i64,
    pub view_w: i64,
    pub view_h: i64,
    /// The zoom in thousandths and the screen's dpi: together the scale of the pieces ([`scale_key`]).
    pub zoom: u32,
    pub dpi: u32,
    /// The view is still being scrolled or zoomed: nothing sharp is asked for yet.
    pub moving: bool,
    pub max_tile_pixels: u32,
    /// What the cache may hold.
    pub budget: u64,
}

/// What the small pictures round about take in the cache at most.
fn previews_bytes() -> u64 {
    ((2 * PREVIEW_RANGE + 1) as f64 * PREVIEW_PIXELS * 4.0) as u64
}

/// A scale as a whole number (the cache key): pixels a point times 72000, which is `zoom * dpi`.
pub fn scale_key(zoom: u32, dpi: u32) -> u64 {
    u64::from(zoom) * u64::from(dpi)
}

/// The dpi the engine is asked to draw a scale at.
pub fn dpi_of(scale: u64) -> f64 {
    scale as f64 / 1000.0
}

/// The size in pixels of a page of `w_pt` by `h_pt` points at a scale (pixels a point times 72000): the same
/// arithmetic as the engine's.
pub fn page_px(w_pt: f64, h_pt: f64, scale: u64) -> (i64, i64) {
    let s = dpi_of(scale) / 72.0;
    (((w_pt * s).round() as i64).max(1), ((h_pt * s).round() as i64).max(1))
}

/// A piece to ask for.
#[derive(Clone, Debug, PartialEq)]
pub struct Want {
    pub key: Key,
    pub preview: bool,
    pub priority: u32,
    /// Where the piece is, in pixels of the page at the key's scale, and how big the whole page is there.
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
    pub page_w: i64,
    pub page_h: i64,
}

impl Want {
    pub fn bytes(&self) -> u64 {
        (self.w.max(0) as u64).saturating_mul(self.h.max(0) as u64).saturating_mul(4)
    }
}

/// The scale a preview of a page is drawn at, or `None` when the page is as small as a preview already.
fn preview_scale(w_pt: f64, h_pt: f64, full: u64) -> Option<u64> {
    let full_px = dpi_of(full) / 72.0;
    if w_pt * h_pt * full_px * full_px <= PREVIEW_PIXELS {
        return None;
    }
    let s = (PREVIEW_PIXELS / (w_pt * h_pt).max(1.0)).sqrt();
    let key = (s * 72_000.0).floor() as u64;
    (key >= 72).then_some(key)
}

fn preview_want(scene: &Scene<'_>, page: usize, priority: u32) -> Option<Want> {
    let (w_pt, h_pt) = scene.layout.size_pt(page)?;
    let full = scale_key(scene.zoom, scene.dpi);
    let scale = preview_scale(w_pt, h_pt, full)?;
    let (w, h) = page_px(w_pt, h_pt, scale);
    Some(Want { key: Key { page: page as u32, rotation: scene.layout.rotation(), scale, tx: 0, ty: 0 }, preview: true, priority, x: 0, y: 0, w, h, page_w: w, page_h: h })
}

/// The pieces of `page` at `scale` that overlap `region` (left, top, right, bottom), given in pixels of the page at
/// the zoom the window is at. `scale` is that zoom or a smaller one. Priorities are not set.
fn pieces(scene: &Scene<'_>, page: usize, scale: u64, region: [i64; 4]) -> Vec<Want> {
    let layout = scene.layout;
    let (pw, ph) = layout.size_px(page);
    let (page_w, page_h, region) = if scale == scale_key(scene.zoom, scene.dpi) || pw <= 0 || ph <= 0 {
        (pw, ph, region)
    } else {
        let Some((w_pt, h_pt)) = layout.size_pt(page) else { return Vec::new() };
        let (qw, qh) = page_px(w_pt, h_pt, scale);
        let (fx, fy) = (qw as f64 / pw as f64, qh as f64 / ph as f64);
        (qw, qh, [(region[0] as f64 * fx).floor() as i64, (region[1] as f64 * fy).floor() as i64, (region[2] as f64 * fx).ceil() as i64, (region[3] as f64 * fy).ceil() as i64])
    };
    tiles::overlapping(page_w, page_h, scene.max_tile_pixels, region[0], region[1], region[2], region[3])
        .into_iter()
        .map(|t| Want {
            key: Key { page: page as u32, rotation: layout.rotation(), scale, tx: t.tx, ty: t.ty },
            preview: false,
            priority: 0,
            x: t.x,
            y: t.y,
            w: t.w,
            h: t.h,
            page_w,
            page_h,
        })
        .collect()
}

/// The pieces of a page in view: what the window shows of it.
fn in_view_pieces(scene: &Scene<'_>, page: usize, scale: u64) -> Vec<Want> {
    let left = scene.layout.left(page, scene.view_w, scene.scroll_x);
    let top = scene.layout.top(page) - scene.scroll_y;
    pieces(scene, page, scale, [-left, -top, scene.view_w - left, scene.view_h - top])
}

fn visible_pages(scene: &Scene<'_>) -> Range<usize> {
    scene.layout.visible(scene.scroll_y, scene.scroll_y + scene.view_h)
}

/// The pieces in view that [`wanted`] asks for when the view is at rest, and the scale they are asked at.
pub fn in_view(scene: &Scene<'_>) -> (Vec<Want>, u64) {
    let scale = draw_scale(scene);
    let wants = visible_pages(scene).flat_map(|page| in_view_pieces(scene, page, scale)).collect();
    (wants, scale)
}

/// The scale the pieces in view are drawn at: the zoom the window is at (`scale_key`), unless they would not fit
/// the cache at that, in which case they are drawn smaller (by a fifth at a time, down to a tenth) and stretched
/// to the size they are seen at.
pub fn draw_scale(scene: &Scene<'_>) -> u64 {
    let full = scale_key(scene.zoom, scene.dpi);
    if scene.layout.is_empty() || scene.view_w <= 0 || scene.view_h <= 0 {
        return full;
    }
    let visible = visible_pages(scene);
    let lowest = (full / LOWEST_FRACTION).max(LOWEST_SCALE).min(full);
    let mut scale = full;
    loop {
        let bytes: u64 = visible.clone().flat_map(|page| in_view_pieces(scene, page, scale)).map(|w| w.bytes()).sum();
        if bytes <= scene.budget || scale <= lowest {
            return scale;
        }
        scale = ((scale as f64 * REDUCE_STEP) as u64).max(lowest);
    }
}

/// The pieces wanted, most urgent first. `cache` says which pages have anything at all.
pub fn wanted(scene: &Scene<'_>, cache: &BitmapCache) -> Vec<Want> {
    let layout = scene.layout;
    let mut out: Vec<Want> = Vec::new();
    if layout.is_empty() || scene.view_w <= 0 || scene.view_h <= 0 {
        return out;
    }
    let visible = visible_pages(scene);
    let centre = (scene.view_w / 2, scene.view_h / 2);

    if scene.moving {
        // Moving: a small picture of each page that has nothing to show yet, the one nearest the middle first.
        let mut near: Vec<(i64, Want)> = Vec::new();
        for page in visible {
            if cache.page(page as u32).next().is_none() {
                let (_, ph) = layout.size_px(page);
                let top = layout.top(page) - scene.scroll_y;
                if let Some(w) = preview_want(scene, page, P_PREVIEW_IN_VIEW) {
                    near.push(((top + ph / 2 - centre.1).abs(), w));
                }
            }
        }
        near.sort_by_key(|(d, _)| *d);
        for (rank, (_, mut w)) in near.into_iter().enumerate() {
            w.priority = P_PREVIEW_IN_VIEW + rank.min(900) as u32;
            out.push(w);
        }
        return out;
    }

    // Pieces in view, the ones nearest the middle of the window first. Always all of them.
    let scale = draw_scale(scene);
    let mut seen: HashSet<Key> = HashSet::new();
    let mut total = 0u64;
    let mut in_view: Vec<(i64, Want)> = Vec::new();
    for page in visible.clone() {
        let (pw, ph) = layout.size_px(page);
        let (left, top) = (layout.left(page, scene.view_w, scene.scroll_x), layout.top(page) - scene.scroll_y);
        for w in in_view_pieces(scene, page, scale) {
            // The piece's middle, in pixels of the window (the piece is at `scale`, the page is drawn at its own size).
            let (fx, fy) = (pw as f64 / w.page_w.max(1) as f64, ph as f64 / w.page_h.max(1) as f64);
            let cx = left + ((w.x as f64 + w.w as f64 / 2.0) * fx) as i64;
            let cy = top + ((w.y as f64 + w.h as f64 / 2.0) * fy) as i64;
            in_view.push(((cx - centre.0).abs() + (cy - centre.1).abs(), w));
        }
    }
    in_view.sort_by_key(|(d, _)| *d);
    for (rank, (_, mut w)) in in_view.into_iter().enumerate() {
        w.priority = P_IN_VIEW + rank.min(900) as u32;
        total = total.saturating_add(w.bytes());
        seen.insert(w.key);
        out.push(w);
    }

    // Next to the view: the top of the pages that a page turn goes to, and the rows a scroll goes to. All their pieces
    // whatever the count, as far as the cache has room for them after what is in view and the small pictures (which the
    // cache drops before anything else but would push these out if they were not counted).
    let room = scene.budget.saturating_sub(previews_bytes());
    let mut ring: Vec<Want> = Vec::new();
    let mut add = |rank: u32, wants: Vec<Want>| {
        for (i, mut w) in wants.into_iter().enumerate() {
            w.priority = P_RING + rank * 100 + (i as u32).min(99);
            ring.push(w);
        }
    };
    let page_top = |page: usize| {
        let left = layout.left(page, scene.view_w, scene.scroll_x);
        pieces(scene, page, scale, [-left, 0, scene.view_w - left, scene.view_h])
    };
    let band = |y0: i64, y1: i64| -> Vec<Want> {
        layout
            .visible(y0, y1)
            .flat_map(|page| {
                let left = layout.left(page, scene.view_w, scene.scroll_x);
                let top = layout.top(page);
                pieces(scene, page, scale, [-left, y0 - top, scene.view_w - left, y1 - top])
            })
            .collect()
    };
    let last = visible.end.saturating_sub(1);
    let (below, above) = (scene.scroll_y.saturating_add(scene.view_h), scene.scroll_y);
    if last + 1 < layout.len() {
        add(1, page_top(last + 1));
    }
    add(2, band(below, below.saturating_add(scene.view_h)));
    if visible.start > 0 {
        add(3, page_top(visible.start - 1));
    }
    if last + 2 < layout.len() {
        add(4, page_top(last + 2));
    }
    add(5, band(above.saturating_sub(scene.view_h), above));
    ring.sort_by_key(|w| w.priority);
    for w in ring {
        if seen.contains(&w.key) {
            continue;
        }
        // Held to the cache: what is in view, and then what is next, until the bytes run out.
        if total.saturating_add(w.bytes()) <= room {
            total += w.bytes();
            seen.insert(w.key);
            out.push(w);
        }
    }

    // Small pictures of the pages round about (they cost little and are kept longest).
    let lo = visible.start.saturating_sub(PREVIEW_RANGE);
    let hi = (visible.end + PREVIEW_RANGE).min(layout.len());
    for page in lo..hi {
        let dist = if page < visible.start { visible.start - page } else { page.saturating_sub(visible.end.saturating_sub(1)) };
        if let Some(w) = preview_want(scene, page, P_PREVIEW_NEAR + dist as u32 * 10) {
            out.push(w);
        }
    }
    out.sort_by_key(|w| w.priority);
    out
}

/// What to do about the engine's queue.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    /// Requests (by their ids) that are not wanted any more.
    pub cancel: Vec<u64>,
    /// Pieces to ask for, most urgent first.
    pub issue: Vec<Want>,
}

/// Compare what is wanted with what is cached and what is asked for already (`in_flight`: key to request id).
pub fn plan(wanted: &[Want], cache: &BitmapCache, in_flight: &HashMap<Key, u64>) -> Plan {
    let wanted_keys: HashSet<Key> = wanted.iter().map(|w| w.key).collect();
    let mut cancel: Vec<u64> = in_flight.iter().filter(|(k, _)| !wanted_keys.contains(k)).map(|(_, id)| *id).collect();
    cancel.sort_unstable();
    let issue = wanted.iter().filter(|w| !cache.contains(&w.key) && !in_flight.contains_key(&w.key)).cloned().collect();
    Plan { cancel, issue }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{Entry, Focus};

    const MAX: u32 = 4 * 1024 * 1024;

    fn layout(pages: usize) -> Layout {
        let mut l = Layout::new(vec![(595.0, 842.0); pages]);
        // 150 percent at 96 dpi: 2 pixels a point.
        l.set(2.0, 0, 8, 8);
        l
    }

    fn scene<'a>(l: &'a Layout, scroll_y: i64, moving: bool) -> Scene<'a> {
        Scene { layout: l, scroll_x: 0, scroll_y, view_w: 1400, view_h: 900, zoom: 1500, dpi: 96, moving, max_tile_pixels: MAX, budget: 100_000_000 }
    }

    fn pages_of(w: &[Want], preview: bool) -> Vec<u32> {
        w.iter().filter(|w| w.preview == preview).map(|w| w.key.page).collect()
    }

    #[test]
    fn the_scale_is_the_same_whichever_way_it_is_asked() {
        assert_eq!(scale_key(1000, 96), 96_000);
        assert!((dpi_of(scale_key(1500, 96)) - 144.0).abs() < 1e-9);
        // 595 points at 144 dpi: 1190 pixels.
        assert_eq!(page_px(595.0, 842.0, scale_key(1500, 96)), (1190, 1684));
        assert_eq!(page_px(0.1, 0.1, 100), (1, 1));
    }

    #[test]
    fn at_rest_the_pages_in_view_come_first_then_the_next_ones_then_pictures() {
        let l = layout(30);
        let cache = BitmapCache::new(100_000_000);
        // Scrolled so that page 10 is the one in view (1684 + 8 pixels a page).
        let y = l.top(10);
        let w = wanted(&scene(&l, y, false), &cache);
        let sharp = pages_of(&w, false);
        assert_eq!(sharp[0], 10, "{sharp:?}");
        // The order: in view, then the next page, the one behind, the one after the next.
        assert_eq!(sharp, vec![10, 11, 9, 12]);
        // Pictures of the pages round about, after all the full ones.
        let pictures = pages_of(&w, true);
        assert!(pictures.len() >= 10 && pictures.iter().all(|p| (2..=19).contains(p)), "{pictures:?}");
        let last_sharp = w.iter().rposition(|w| !w.preview).expect("sharp");
        let first_picture = w.iter().position(|w| w.preview).expect("picture");
        assert!(last_sharp < first_picture);
    }

    #[test]
    fn while_moving_only_pictures_of_pages_with_nothing_are_asked_for() {
        let l = layout(30);
        let mut cache = BitmapCache::new(100_000_000);
        let y = l.top(10) + 1000;
        let w = wanted(&scene(&l, y, true), &cache);
        assert!(w.iter().all(|w| w.preview && w.priority < 100), "{w:?}");
        assert_eq!(pages_of(&w, true), vec![10, 11].into_iter().filter(|p| w.iter().any(|x| x.key.page == *p)).collect::<Vec<_>>());
        assert!(!w.is_empty());
        // Page 10 has a piece already (of any scale): no picture for it.
        let focus = Focus { first: 10, last: 11, rotation: 0, scale: scale_key(1500, 96) };
        cache.insert(Entry { key: Key { page: 10, rotation: 0, scale: 5, tx: 0, ty: 0 }, width: 1, height: 1, bgra: vec![0; 4], preview: true, x: 0, y: 0, page_w: 1, page_h: 1 }, &focus);
        let w = wanted(&scene(&l, y, true), &cache);
        assert!(w.iter().all(|w| w.key.page != 10));
    }

    #[test]
    fn a_big_zoom_asks_for_the_tiles_in_view_nearest_the_middle_first() {
        let mut l = Layout::new(vec![(595.0, 842.0); 3]);
        // 800 percent at 96 dpi: 10.67 pixels a point; the page is 6347 by 8981 pixels.
        l.set(scale_from(8000, 96), 0, 8, 8);
        let s = Scene { layout: &l, scroll_x: 2000, scroll_y: 3000, view_w: 1400, view_h: 900, zoom: 8000, dpi: 96, moving: false, max_tile_pixels: MAX, budget: 100_000_000 };
        let w = wanted(&s, &BitmapCache::new(100_000_000));
        let sharp: Vec<&Want> = w.iter().filter(|w| !w.preview && w.key.page == 0).collect();
        // The window shows 1400 by 900 pixels at an offset: at most 2 by 2 tiles of 2048 are in view.
        let in_view: Vec<&Want> = sharp.iter().copied().filter(|w| w.priority < P_RING).collect();
        assert!(!in_view.is_empty() && in_view.len() <= 4, "{}", in_view.len());
        assert!(sharp.iter().all(|w| w.w <= 2048 && w.h <= 2048));
        // The first is the one nearest the middle of the window.
        let centre = |w: &Want| (l.left(0, 1400, 2000) + w.x + w.w / 2 - 700).abs() + (l.top(0) - 3000 + w.y + w.h / 2 - 450).abs();
        let first = centre(in_view[0]);
        assert!(in_view.iter().all(|w| centre(w) >= first));
        // What is next is asked for after what is in view, tile by tile: the rows just under the window, and the top of the next page.
        assert!(sharp.len() > in_view.len(), "the rows under the window");
        assert!(w.iter().any(|w| !w.preview && w.key.page == 1 && w.priority >= P_RING), "the top of the next page, in tiles");
        assert!(w.iter().filter(|w| !w.preview).all(|w| w.key.page <= 2));
    }

    fn scale_from(zoom: u32, dpi: u32) -> f64 {
        f64::from(zoom) / 1000.0 * f64::from(dpi) / 72.0
    }

    #[test]
    fn what_does_not_fit_in_the_cache_is_not_asked_for_beyond_what_is_in_view() {
        let l = layout(30);
        let cache = BitmapCache::new(20_000_000);
        // A page is 1190 * 1684 * 4 = 8 MB: two fit in 24 MB less the pictures (4.4 MB).
        let mut s = scene(&l, l.top(10), false);
        s.budget = 24_000_000;
        let w = wanted(&s, &cache);
        assert_eq!(pages_of(&w, false), vec![10, 11]);
    }

    #[test]
    fn what_is_in_view_is_always_asked_for_at_a_smaller_scale_if_it_does_not_fit() {
        let l = layout(30);
        let cache = BitmapCache::new(1_000_000);
        // Two pages are in view (scrolled half way down page 10): 16 MB at full scale, a cache of 1 MB.
        let mut s = scene(&l, l.top(10) + 900, false);
        s.budget = 1_000_000;
        let full = scale_key(1500, 96);
        let scale = draw_scale(&s);
        assert!(scale < full && scale >= full / 10, "{scale}");
        let w = wanted(&s, &cache);
        let sharp: Vec<&Want> = w.iter().filter(|w| !w.preview).collect();
        assert!(sharp.iter().map(|w| w.key.page).collect::<HashSet<_>>().is_superset(&[10, 11].into_iter().collect()));
        assert!(sharp.iter().all(|w| w.key.scale == scale), "{sharp:?}");
        // Each piece is the whole page at the smaller scale: it is stretched to the page's size in the window.
        assert!(sharp.iter().all(|w| w.page_w < 1190 && w.w <= w.page_w));
        // Nothing next to the view is asked for: the cache is over already.
        assert!(sharp.iter().all(|w| w.priority < P_RING));
        // With room for it, the scale is the zoom.
        s.budget = 100_000_000;
        assert_eq!(draw_scale(&s), full);
        // Down to a tenth at the lowest, however small the cache.
        s.budget = 0;
        assert_eq!(draw_scale(&s), (full / LOWEST_FRACTION).max(LOWEST_SCALE));
    }

    #[test]
    fn the_plan_cancels_what_is_not_wanted_and_asks_for_what_is_missing() {
        let l = layout(30);
        let mut cache = BitmapCache::new(100_000_000);
        let w = wanted(&scene(&l, l.top(10), false), &cache);
        let mut in_flight = HashMap::new();
        // Page 10 is cached, page 11 is asked for, page 3 is asked for and no longer wanted.
        let focus = Focus { first: 10, last: 10, rotation: 0, scale: scale_key(1500, 96) };
        let k10 = w.iter().find(|x| x.key.page == 10 && !x.preview).expect("page 10").key;
        cache.insert(Entry { key: k10, width: 1, height: 1, bgra: vec![0; 4], preview: false, x: 0, y: 0, page_w: 1, page_h: 1 }, &focus);
        let k11 = w.iter().find(|x| x.key.page == 11 && !x.preview).expect("page 11").key;
        in_flight.insert(k11, 41);
        in_flight.insert(Key { page: 3, rotation: 0, scale: scale_key(1500, 96), tx: 0, ty: 0 }, 40);
        let p = plan(&w, &cache, &in_flight);
        assert_eq!(p.cancel, vec![40]);
        assert!(p.issue.iter().all(|x| x.key != k10 && x.key != k11));
        assert!(p.issue.iter().any(|x| x.key.page == 12 && !x.preview));
        // Everything wanted is asked for exactly once.
        let again = plan(&w, &cache, &HashMap::new());
        assert_eq!(again.issue.len(), w.len() - 1);
        assert!(again.cancel.is_empty());
    }

    #[test]
    fn a_page_as_small_as_a_picture_has_no_picture_and_nothing_wanted_with_no_window() {
        let l = layout(3);
        assert!(preview_scale(595.0, 842.0, scale_key(100, 96)).is_none());
        assert!(preview_scale(595.0, 842.0, scale_key(1500, 96)).is_some());
        let mut s = scene(&l, 0, false);
        s.view_w = 0;
        assert!(wanted(&s, &BitmapCache::new(1)).is_empty());
        assert!(wanted(&scene(&Layout::new(Vec::new()), 0, false), &BitmapCache::new(1)).is_empty());
    }

    #[test]
    fn a_page_turn_on_a_full_hd_window_needs_few_requests_and_the_neighbours_are_ready() {
        // An A4 page at fit-width on a 1920 by 1054 window: 1896 by 2681 pixels, two tiles of 2048.
        let mut l = Layout::new(vec![(595.0, 842.0); 10]);
        let scale = 1896.0 / 595.0;
        l.set(scale, 0, 10, 12);
        let zoom = (scale * 72.0 / 96.0 * 1000.0) as u32;
        let s = Scene { layout: &l, scroll_x: 0, scroll_y: l.top(4) - 6, view_w: 1920, view_h: 1054, zoom, dpi: 96, moving: false, max_tile_pixels: MAX, budget: 60_000_000 };
        let w = wanted(&s, &BitmapCache::new(60_000_000));
        let sharp: Vec<&Want> = w.iter().filter(|w| !w.preview).collect();
        let in_view = sharp.iter().filter(|w| w.priority < P_RING).count();
        assert_eq!(in_view, 1, "one request to show the page");
        // The next page's top is asked for in the same go, one request.
        assert!(sharp.iter().any(|w| w.key.page == 5 && w.key.ty == 0 && w.priority >= P_RING));
        assert!(sharp.iter().all(|w| w.w <= 2048 && w.h <= 2048));
    }
}

