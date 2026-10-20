//! Where the pages are: rows of pages with a gap between, in pixels at the current scale and rotation. A row is one page (the usual
//! column) or two (a spread, 2026-10-20). Plain arithmetic; nothing here knows about windows.

use std::ops::Range;

/// The most a side of a page counts for, in points (the engine tells no more than this: see `MAX_PAGE_UNITS`). A bigger or
/// not-a-number size is cut to this here too, so that nothing below can overflow.
pub const MAX_PAGE_POINTS: f64 = qingpdf_core::view::MAX_PAGE_UNITS;

/// The most pixels a side of a page has in the layout.
const MAX_SIDE_PIXELS: f64 = 1.0e9;

/// How many pages go in a row, and which page the first row has to itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Spread {
    /// One page a row (the usual column).
    #[default]
    Single,
    /// Two pages a row: 1 and 2, 3 and 4, ...
    Pair,
    /// Two pages a row with the first page (the cover) alone: 1, then 2 and 3, 4 and 5, ...
    Cover,
}

impl Spread {
    const ALL: [Spread; 3] = [Spread::Single, Spread::Pair, Spread::Cover];

    /// The number kept in the file of places.
    pub fn code(self) -> u8 {
        self as u8
    }

    pub fn from_code(code: u8) -> Option<Spread> {
        Spread::ALL.get(usize::from(code)).copied()
    }

    /// The word kept in the settings.
    pub fn word(self) -> &'static str {
        ["single", "spread", "cover"].get(self as usize).copied().unwrap_or("single")
    }

    pub fn parse(s: &str) -> Option<Spread> {
        Spread::ALL.into_iter().find(|v| v.word() == s.trim())
    }

    /// What a file asks for with `/PageLayout` (`qingpdf_core::view::SPREAD_*`).
    pub fn from_hint(hint: u8) -> Option<Spread> {
        match hint {
            qingpdf_core::view::SPREAD_FIRST_LEFT => Some(Spread::Pair),
            qingpdf_core::view::SPREAD_FIRST_RIGHT => Some(Spread::Cover),
            _ => None,
        }
    }
}

/// The pages of a spread, and whether they read from the right (the first page of a pair on the right, a cover on the left), as books
/// of vertical Chinese or Japanese do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Arrangement {
    pub spread: Spread,
    pub r2l: bool,
}

/// A row of pages: the pages `first..end` (one or two), its top and its height in pixels.
#[derive(Clone, Copy)]
struct Row {
    first: usize,
    end: usize,
    top: i64,
    height: i64,
}

/// The pages laid out. Coordinates are those of the whole document: x from the left edge of the widest row, y from the top of the first row.
pub struct Layout {
    /// Size of each page in points as the engine shows it (its own `/Rotate` already applied).
    sizes: Vec<(f64, f64)>,
    rotation: u16,
    scale: f64,
    gap: i64,
    margin: i64,
    arr: Arrangement,
    /// Per page: its size in pixels, its top (a page shorter than the other of its row is in the middle of the row's height) and its left
    /// edge from the left margin.
    widths: Vec<i64>,
    heights: Vec<i64>,
    tops: Vec<i64>,
    xs: Vec<i64>,
    rows: Vec<Row>,
    /// Per page: its row.
    row_of: Vec<u32>,
    /// The widths in points of the left and the right half of a spread (the widest page on each side of the gutter), and the gap between.
    halves_pt: (f64, f64),
    spread_gap: i64,
    content_w: i64,
    content_h: i64,
}

impl Layout {
    pub fn new(sizes: Vec<(f64, f64)>) -> Layout {
        let sizes = sizes.into_iter().map(|(w, h)| (points(w), points(h))).collect();
        let mut l = Layout {
            sizes,
            rotation: 0,
            scale: 1.0,
            gap: 0,
            margin: 0,
            arr: Arrangement::default(),
            widths: Vec::new(),
            heights: Vec::new(),
            tops: Vec::new(),
            xs: Vec::new(),
            rows: Vec::new(),
            row_of: Vec::new(),
            halves_pt: (0.0, 0.0),
            spread_gap: 0,
            content_w: 0,
            content_h: 0,
        };
        l.set(1.0, 0, 0, 0);
        l
    }

    /// How the pages go in rows; takes effect with the next [`Layout::set`].
    pub fn arrange(&mut self, arr: Arrangement) {
        self.arr = arr;
    }

    pub fn arrangement(&self) -> Arrangement {
        self.arr
    }

    /// Lay the pages out at `scale` pixels a point, turned `rotation` degrees (a multiple of 90) further, with `gap`
    /// pixels between rows (half of that between the two pages of a spread) and `margin` around the whole.
    pub fn set(&mut self, scale: f64, rotation: u16, gap: i64, margin: i64) {
        self.scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
        self.rotation = rotation % 360;
        self.gap = gap.clamp(0, 1_000_000);
        self.margin = margin.clamp(0, 1_000_000);
        let turned = self.rotation % 180 == 90;
        let n = self.sizes.len();
        let mut widest = 0;
        self.widths.clear();
        self.heights.clear();
        for &(w, h) in &self.sizes {
            let (w, h) = if turned { (h, w) } else { (w, h) };
            let (pw, ph) = (pixels(w * self.scale), pixels(h * self.scale));
            self.widths.push(pw);
            self.heights.push(ph);
            widest = widest.max(pw);
        }
        // The rows: consecutive pages, one or two.
        self.rows.clear();
        self.row_of.clear();
        let mut first = 0;
        while first < n {
            let count = match self.arr.spread {
                Spread::Single => 1,
                Spread::Cover if first == 0 => 1,
                _ => (n - first).min(2),
            };
            let height = (first..first + count).map(|p| self.size_px(p).1).max().unwrap_or(0);
            self.row_of.extend(std::iter::repeat_n(self.rows.len() as u32, count));
            self.rows.push(Row { first, end: first + count, top: 0, height });
            first += count;
        }
        // The halves of a spread are as wide as the widest page on their side, so that the gutter is in one place all down the column.
        let (mut half_l, mut half_r) = ((0, 0.0_f64), (0, 0.0_f64));
        for row in 0..self.rows.len() {
            for page in self.pages_of_row(row) {
                let (w, wp) = (self.size_px(page).0, self.size_pt(page).map_or(0.0, |s| s.0));
                match self.slot(row, page) {
                    Some(true) => half_l = (half_l.0.max(w), half_l.1.max(wp)),
                    Some(false) => half_r = (half_r.0.max(w), half_r.1.max(wp)),
                    None => {}
                }
            }
        }
        self.halves_pt = (half_l.1, half_r.1);
        self.spread_gap = if half_l.0 > 0 && half_r.0 > 0 { self.gap / 2 } else { 0 };
        let inner = if self.arr.spread == Spread::Single { widest } else { half_l.0.saturating_add(self.spread_gap).saturating_add(half_r.0) };
        self.tops.clear();
        self.xs.clear();
        let mut y = self.margin;
        for row in 0..self.rows.len() {
            let height = self.rows.get(row).map_or(0, |r| r.height);
            if let Some(r) = self.rows.get_mut(row) {
                r.top = y;
            }
            for page in self.pages_of_row(row) {
                let (w, h) = self.size_px(page);
                self.tops.push(y.saturating_add((height - h) / 2));
                self.xs.push(match self.slot(row, page) {
                    None => (inner - w) / 2,
                    Some(true) => half_l.0 - w,
                    Some(false) => half_l.0.saturating_add(self.spread_gap),
                });
            }
            y = y.saturating_add(height).saturating_add(self.gap);
        }
        self.content_h = if n == 0 { 0 } else { y.saturating_sub(self.gap).saturating_add(self.margin) };
        self.content_w = inner.saturating_add(self.margin.saturating_mul(2));
    }

    fn pages_of_row(&self, row: usize) -> Range<usize> {
        self.rows.get(row).map_or(0..0, |r| r.first..r.end)
    }

    /// The side of the gutter that page `page`, in row `row`, is on: `Some(true)` the left, `Some(false)` the right, `None` in the middle of
    /// the column. The first page of a pair is on the left (on the right when the pages read from the right); a page alone is where the
    /// second page of a pair would be if it is the cover, and where the first would be if it is the last page.
    fn slot(&self, row: usize, page: usize) -> Option<bool> {
        if self.arr.spread == Spread::Single {
            return None;
        }
        let pages = self.pages_of_row(row);
        let cover = pages.len() == 1 && self.arr.spread == Spread::Cover && row == 0;
        let first = if pages.len() == 1 { !cover } else { page == pages.start };
        Some(first != self.arr.r2l)
    }

    pub fn len(&self) -> usize {
        self.sizes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sizes.is_empty()
    }

    pub fn scale(&self) -> f64 {
        self.scale
    }

    pub fn rotation(&self) -> u16 {
        self.rotation
    }

    /// A page's size in points as it is laid out (turned).
    pub fn size_pt(&self, page: usize) -> Option<(f64, f64)> {
        let &(w, h) = self.sizes.get(page)?;
        Some(if self.rotation % 180 == 90 { (h, w) } else { (w, h) })
    }

    /// A page's size in points as the engine tells it (not turned by the view).
    pub fn page_size_pt(&self, page: usize) -> Option<(f64, f64)> {
        self.sizes.get(page).copied()
    }

    /// The width in points of the widest row as it is laid out (turned), and the pixels between the two pages of a spread that go with it:
    /// what the zoom that fits the width fits to the window.
    pub fn widest_pt(&self) -> (f64, i64) {
        if self.arr.spread != Spread::Single {
            return (self.halves_pt.0 + self.halves_pt.1, self.spread_gap);
        }
        let turned = self.rotation % 180 == 90;
        (self.sizes.iter().map(|&(w, h)| if turned { h } else { w }).fold(0.0, f64::max), 0)
    }

    /// The height in points of the row `page` is in (the page, or the taller of the two pages of a spread).
    pub fn row_height_pt(&self, page: usize) -> f64 {
        self.pages_in_row(page).filter_map(|p| self.size_pt(p)).map(|s| s.1).fold(0.0, f64::max)
    }

    /// A page's size in pixels.
    pub fn size_px(&self, page: usize) -> (i64, i64) {
        (self.widths.get(page).copied().unwrap_or(0), self.heights.get(page).copied().unwrap_or(0))
    }

    /// The top of a page (a page shorter than the other page of its row is a little lower than the row's top).
    pub fn top(&self, page: usize) -> i64 {
        self.tops.get(page).copied().unwrap_or(0)
    }

    pub fn content_width(&self) -> i64 {
        self.content_w
    }

    pub fn content_height(&self) -> i64 {
        self.content_h
    }

    /// The pages of the row page `page` is in (just it, or it and the other page of the spread); none if there is no such page.
    pub fn pages_in_row(&self, page: usize) -> Range<usize> {
        self.row_of.get(page).map_or(0..0, |&r| self.pages_of_row(r as usize))
    }

    /// The row that holds document row `y`, or the one nearest to it (the gap belongs to the row above).
    fn row_at(&self, y: i64) -> usize {
        self.rows.partition_point(|r| r.top <= y).saturating_sub(1)
    }

    /// The first page of the row that holds document row `y`, or of the one nearest to it.
    pub fn page_at(&self, y: i64) -> usize {
        self.rows.get(self.row_at(y)).map_or(0, |r| r.first)
    }

    /// The pages that have a row in `y0..y1`.
    pub fn visible(&self, y0: i64, y1: i64) -> Range<usize> {
        if self.rows.is_empty() || y1 <= y0 {
            return 0..0;
        }
        let first = self.row_at(y0);
        // A row after the first one that starts before y1.
        let end = self.rows.partition_point(|r| r.top < y1);
        // The first row may end above y0 (y0 is in the gap below it).
        let first = match self.rows.get(first) {
            Some(r) if r.top.saturating_add(r.height) <= y0 && first + 1 < self.rows.len() => first + 1,
            _ => first,
        };
        let page = |row: usize| self.rows.get(row).map_or(self.sizes.len(), |r| r.first);
        page(first)..page(end.max(first))
    }

    /// The left edge of a page in a viewport `viewport_w` wide that is scrolled `scroll_x` pixels: the rows are centred when the document is
    /// narrower than the viewport.
    pub fn left(&self, page: usize, viewport_w: i64, scroll_x: i64) -> i64 {
        let x = self.margin.saturating_add(self.xs.get(page).copied().unwrap_or(0));
        if self.content_w <= viewport_w { (viewport_w - self.content_w) / 2 + x } else { x.saturating_sub(scroll_x) }
    }

    /// The page at a place of the viewport (`x` across it, `y` down the document): the page whose rectangle holds it, else the nearest one of
    /// the row that holds `y`.
    pub fn page_under(&self, x: i64, y: i64, viewport_w: i64, scroll_x: i64) -> usize {
        let first = self.page_at(y);
        let off = |p: usize| {
            let (l, w) = (self.left(p, viewport_w, scroll_x), self.size_px(p).0);
            let r = l.saturating_add(w);
            if x < l { l.saturating_sub(x) } else if x >= r { x.saturating_sub(r).saturating_add(1) } else { 0 }
        };
        self.pages_in_row(first).min_by_key(|&p| off(p)).unwrap_or(first)
    }

    /// The most the document can be scrolled: (right, down), 0 when it fits.
    pub fn max_scroll(&self, viewport_w: i64, viewport_h: i64) -> (i64, i64) {
        ((self.content_w - viewport_w).max(0), (self.content_h - viewport_h).max(0))
    }

    /// The place `scroll_y` has to be at for the row of page `page` to start `offset_px` pixels below the top of the viewport.
    pub fn scroll_to_page(&self, page: usize, offset_px: i64) -> i64 {
        self.row_top_of(page).saturating_sub(offset_px)
    }

    fn row_top_of(&self, page: usize) -> i64 {
        self.row_of.get(page).and_then(|&r| self.rows.get(r as usize)).map_or(0, |r| r.top)
    }

    /// Where `y` is in its row: (the row's first page, fraction down the row from 0 to 1, the gap below counted as the row's end).
    pub fn locate(&self, y: i64) -> (usize, f64) {
        let row = self.rows.get(self.row_at(y));
        let (top, h) = row.map_or((0, 1), |r| (r.top, r.height.max(1)));
        (self.page_at(y), y.saturating_sub(top) as f64 / h as f64)
    }

    /// The inverse of [`Layout::locate`] (for any page of the row).
    pub fn y_of(&self, page: usize, fraction: f64) -> i64 {
        let Some(row) = self.row_of.get(page).and_then(|&r| self.rows.get(r as usize)) else { return 0 };
        row.top.saturating_add((fraction * row.height as f64).round() as i64)
    }

    /// How points of page `page` (as the engine tells them) are put in its rectangle of pixels.
    pub fn page_map(&self, page: usize) -> Option<PageMap> {
        let &(w, h) = self.sizes.get(page)?;
        let (pw, ph) = self.size_px(page);
        let turned = self.rotation % 180 == 90;
        let (shown_w, shown_h) = if turned { (h, w) } else { (w, h) };
        Some(PageMap { w, h, rotation: self.rotation, sx: pw as f64 / shown_w, sy: ph as f64 / shown_h })
    }
}

/// The places on a page, in points as the engine tells them (the page turned by its own `/Rotate`, the origin at the top left,
/// y down), and the same places in the pixels of the page's rectangle on the screen, where the view is turned a further
/// `rotation` degrees clockwise and the page is drawn at some size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageMap {
    /// The size of the page in points before the view's own turn.
    w: f64,
    h: f64,
    rotation: u16,
    /// Pixels to a point, across and down the page as it is shown.
    sx: f64,
    sy: f64,
}

impl PageMap {
    /// A point of the page in pixels from the top left of the page's rectangle.
    pub fn to_px(self, x: f64, y: f64) -> (f64, f64) {
        let (u, v) = match self.rotation {
            90 => (self.h - y, x),
            180 => (self.w - x, self.h - y),
            270 => (y, self.w - x),
            _ => (x, y),
        };
        (u * self.sx, v * self.sy)
    }

    /// The point of the page at a place `(px, py)` pixels from the top left of the page's rectangle.
    pub fn to_pt(self, px: f64, py: f64) -> (f64, f64) {
        let (u, v) = (px / self.sx, py / self.sy);
        match self.rotation {
            90 => (v, self.h - u),
            180 => (self.w - u, self.h - v),
            270 => (self.w - v, u),
            _ => (u, v),
        }
    }

    /// A rectangle `[left, top, right, bottom]` of the page in pixels: `(left, top, right, bottom)`.
    pub fn rect_to_px(self, r: [f32; 4]) -> (f64, f64, f64, f64) {
        let (a, b) = self.to_px(f64::from(r[0]), f64::from(r[1]));
        let (c, d) = self.to_px(f64::from(r[2]), f64::from(r[3]));
        (a.min(c), b.min(d), a.max(c), b.max(d))
    }
}

/// A length in pixels: at least one, at most [`MAX_SIDE_PIXELS`].
fn pixels(v: f64) -> i64 {
    if v.is_finite() { v.round().clamp(1.0, MAX_SIDE_PIXELS) as i64 } else { 1 }
}

/// A side of a page in points: at least a hundredth of a point, at most [`MAX_PAGE_POINTS`] (a size that is not a number is
/// one point).
fn points(v: f64) -> f64 {
    if v.is_nan() { 1.0 } else { v.clamp(0.01, MAX_PAGE_POINTS) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn three() -> Layout {
        // A4, a landscape page, a small one.
        let mut l = Layout::new(vec![(595.0, 842.0), (842.0, 595.0), (300.0, 400.0)]);
        l.set(1.0, 0, 10, 20);
        l
    }

    #[test]
    fn pages_follow_each_other_with_gaps_and_a_margin() {
        let l = three();
        assert_eq!(l.size_px(0), (595, 842));
        assert_eq!((l.top(0), l.top(1), l.top(2)), (20, 20 + 842 + 10, 20 + 842 + 10 + 595 + 10));
        assert_eq!(l.content_height(), 20 + 842 + 10 + 595 + 10 + 400 + 20);
        assert_eq!(l.content_width(), 842 + 40);
    }

    #[test]
    fn the_scale_and_the_rotation_change_the_sizes() {
        let mut l = three();
        l.set(2.0, 90, 10, 20);
        assert_eq!(l.size_px(0), (1684, 1190));
        assert_eq!(l.size_pt(0), Some((842.0, 595.0)));
        l.set(0.5, 180, 0, 0);
        assert_eq!(l.size_px(2), (150, 200));
        assert_eq!(l.rotation(), 180);
        // Never smaller than a pixel.
        l.set(0.0001, 0, 0, 0);
        assert_eq!(l.size_px(0), (1, 1));
    }

    #[test]
    fn the_page_at_a_row_and_the_pages_in_view() {
        let l = three();
        assert_eq!(l.page_at(-100), 0);
        assert_eq!(l.page_at(20), 0);
        assert_eq!(l.page_at(861), 0);
        // The gap belongs to the page above, the next page starts at its top.
        assert_eq!(l.page_at(870), 0);
        assert_eq!(l.page_at(872), 1);
        assert_eq!(l.page_at(1_000_000), 2);
        assert_eq!(l.visible(0, 100), 0..1);
        assert_eq!(l.visible(800, 900), 0..2);
        // Row 865 is in the gap below page 0: only page 1 is seen.
        assert_eq!(l.visible(865, 900), 1..2);
        assert_eq!(l.visible(0, 100_000), 0..3);
        assert_eq!(l.visible(10, 10), 0..0);
        assert_eq!(Layout::new(Vec::new()).visible(0, 100), 0..0);
    }

    #[test]
    fn pages_are_centred_when_narrow_and_scroll_when_wide() {
        let l = three();
        // The viewport is wider than the widest page: all are centred.
        assert_eq!(l.left(0, 1000, 0), (1000 - 595) / 2);
        assert_eq!(l.left(1, 1000, 0), (1000 - 842) / 2);
        // Narrower: page 1 (the widest) starts at the margin, scrolled by scroll_x.
        assert_eq!(l.left(1, 500, 0), 20);
        assert_eq!(l.left(1, 500, 30), 20 - 30);
        assert_eq!(l.left(2, 500, 0), 20 + (842 - 300) / 2);
        assert_eq!(l.max_scroll(500, 400), (882 - 500, l.content_height() - 400));
        assert_eq!(l.max_scroll(2000, 100000), (0, 0));
    }

    #[test]
    fn the_widest_page_is_found_whichever_way_the_pages_are_turned() {
        let mut l = Layout::new(vec![(100.0, 200.0), (150.0, 50.0), (120.0, 120.0)]);
        assert_eq!(l.widest_pt(), (150.0, 0));
        l.set(1.0, 90, 0, 0);
        assert_eq!(l.widest_pt(), (200.0, 0));
        assert_eq!(Layout::new(Vec::new()).widest_pt(), (0.0, 0));
    }

    #[test]
    fn pages_of_absurd_sizes_are_cut_to_the_limit_and_nothing_overflows() {
        let mut l = Layout::new(vec![(1e300, 1e300), (f64::INFINITY, f64::NEG_INFINITY), (f64::NAN, 5.0), (-3.0, 0.0), (1e300, 10.0)]);
        for spread in Spread::ALL {
            for r2l in [false, true] {
                l.arrange(Arrangement { spread, r2l });
                for (scale, rotation) in [(1.0, 0), (64.0 * 4.0 / 3.0, 90), (1e300, 0), (f64::NAN, 270), (1e-300, 180)] {
                    l.set(scale, rotation, i64::MAX, i64::MAX);
                    l.set(scale, rotation, 10, 20);
                    for p in 0..l.len() {
                        let (w, h) = l.size_pt(p).expect("a page");
                        assert!((0.01..=MAX_PAGE_POINTS).contains(&w) && (0.01..=MAX_PAGE_POINTS).contains(&h), "{w} {h}");
                        let (pw, ph) = l.size_px(p);
                        assert!((1..=1_000_000_000).contains(&pw) && (1..=1_000_000_000).contains(&ph));
                    }
                    assert!(l.content_height() > 0 && l.content_width() > 0);
                    // Everything that works with places in the document stays sane at the far end of it.
                    let last = l.len() - 1;
                    let (page, fraction) = l.locate(i64::MAX);
                    assert!(l.pages_in_row(last).contains(&page));
                    assert!(fraction.is_finite());
                    let _ = (l.y_of(page, fraction), l.left(0, 800, i64::MAX / 4), l.max_scroll(800, 600), l.scroll_to_page(last, i64::MIN), l.visible(0, i64::MAX), l.page_under(i64::MAX, i64::MAX, 800, i64::MIN / 2));
                }
            }
        }
        // At the largest zoom (6400 percent at 96 dpi) the largest page is a big but plain number of pixels.
        let mut l = Layout::new(vec![(MAX_PAGE_POINTS, MAX_PAGE_POINTS); 1000]);
        l.set(64.0 * 96.0 / 72.0, 0, 10, 12);
        assert_eq!(l.size_px(0), (1_228_800, 1_228_800));
        assert_eq!(l.content_height(), 1000 * 1_228_800 + 999 * 10 + 24);
    }

    #[test]
    fn points_of_a_page_and_pixels_of_its_rectangle_go_both_ways_whatever_the_turn() {
        let mut l = Layout::new(vec![(200.0, 100.0)]);
        for rotation in [0u16, 90, 180, 270] {
            l.set(2.0, rotation, 0, 0);
            let m = l.page_map(0).expect("a map");
            // The corners of the page: the top left of the page as the engine tells it goes where the view's turn puts it.
            let (w, h) = l.size_px(0);
            let corner = m.to_px(0.0, 0.0);
            let expect = match rotation {
                0 => (0.0, 0.0),
                90 => (w as f64, 0.0),
                180 => (w as f64, h as f64),
                _ => (0.0, h as f64),
            };
            assert_eq!(corner, expect, "{rotation}");
            // And back, for places all over the page.
            for (x, y) in [(0.0, 0.0), (200.0, 100.0), (37.5, 81.25), (150.0, 3.0)] {
                let (px, py) = m.to_px(x, y);
                let (bx, by) = m.to_pt(px, py);
                assert!((bx - x).abs() < 1e-9 && (by - y).abs() < 1e-9, "{rotation}: ({x}, {y}) -> ({px}, {py}) -> ({bx}, {by})");
                assert!(px >= -1e-9 && px <= w as f64 + 1e-9 && py >= -1e-9 && py <= h as f64 + 1e-9);
            }
        }
        // A rectangle comes out with its corners in order.
        l.set(1.0, 90, 0, 0);
        let m = l.page_map(0).expect("a map");
        assert_eq!(m.rect_to_px([10.0, 20.0, 30.0, 40.0]), (60.0, 10.0, 80.0, 30.0));
        assert!(l.page_map(5).is_none());
    }

    #[test]
    fn a_place_in_a_page_survives_a_change_of_scale() {
        let mut l = three();
        let y = l.top(1) + 200;
        let (page, fraction) = l.locate(y);
        assert_eq!(page, 1);
        l.set(2.0, 0, 10, 20);
        let again = l.y_of(page, fraction);
        // The same place: 200 pixels down at the old scale is about 400 down at the new one.
        assert!((again - (l.top(1) + 400)).abs() <= 1, "{again}");
    }

    /// Five pages for the spreads: A4, A4, a small landscape page, A4, a taller page than A4.
    fn five(spread: Spread, r2l: bool) -> Layout {
        let mut l = Layout::new(vec![(595.0, 842.0), (595.0, 842.0), (400.0, 300.0), (595.0, 842.0), (500.0, 1000.0)]);
        l.arrange(Arrangement { spread, r2l });
        l.set(1.0, 0, 10, 20);
        l
    }

    #[test]
    fn a_spread_has_two_pages_in_a_row_with_the_gutter_in_one_place() {
        let l = five(Spread::Pair, false);
        // Rows: (0, 1), (2, 3), (4).
        assert_eq!((l.pages_in_row(0), l.pages_in_row(1), l.pages_in_row(2), l.pages_in_row(3), l.pages_in_row(4)), (0..2, 0..2, 2..4, 2..4, 4..5));
        assert_eq!(l.pages_in_row(5), 0..0);
        // The left pages are 595 wide at the most, the right ones 595: the gutter is 595 + 20 from the left edge, in every row; a page
        // hugs it on its side, and the pages of a row are in the middle of the row's height.
        assert_eq!(l.content_width(), 595 + 5 + 595 + 40);
        let wide = 1400;
        let gutter = |p: usize| (l.left(p, wide, 0) + l.size_px(p).0, l.left(p, wide, 0));
        assert_eq!(gutter(0).0 + 5, gutter(1).1, "the pages of a row are 5 pixels apart");
        assert_eq!(gutter(2).0 + 5, gutter(3).1);
        assert_eq!(gutter(0).0, gutter(2).0, "the left pages end at the same place");
        assert_eq!(gutter(1).1, gutter(3).1, "and the right ones begin at the same place");
        // The small landscape page (300 high) and the A4 beside it (842) share the middle of the row.
        assert_eq!(l.top(3), l.top(2) - (842 - 300) / 2);
        assert_eq!(l.top(3), 20 + 842 + 10);
        assert_eq!(l.top(0), l.top(1));
        // The last page is alone, on the left.
        assert_eq!(l.left(4, wide, 0) + l.size_px(4).0, gutter(0).0);
        assert_eq!(l.content_height(), 20 + 842 + 10 + 842 + 10 + 1000 + 20);
        // Centred in a window wider than the pages, and moved by the scroll when it is narrower.
        assert_eq!(l.left(0, wide, 0), (wide - l.content_width()) / 2 + 20);
        assert_eq!(l.left(0, 500, 0), 20);
        assert_eq!(l.left(1, 500, 7), 20 + 595 + 5 - 7);
    }

    #[test]
    fn the_cover_is_alone_and_the_pairs_follow() {
        let l = five(Spread::Cover, false);
        // Rows: (0), (1, 2), (3, 4).
        assert_eq!((l.pages_in_row(0), l.pages_in_row(1), l.pages_in_row(2), l.pages_in_row(3), l.pages_in_row(4)), (0..1, 1..3, 1..3, 3..5, 3..5));
        let wide = 1600;
        // The cover is on the right of the gutter, where the second page of a pair would be.
        assert_eq!(l.left(0, wide, 0), l.left(2, wide, 0));
        assert_eq!(l.left(0, wide, 0), l.left(4, wide, 0));
        assert!(l.left(0, wide, 0) > l.left(1, wide, 0) + 595);
        // The pairs are (1 left, 2 right), (3 left, 4 right).
        assert!(l.left(1, wide, 0) < l.left(2, wide, 0) && l.left(3, wide, 0) < l.left(4, wide, 0));
        assert_eq!(l.page_at(l.top(0)), 0);
        // One page only: a cover with nothing beside it is in the middle of the window (no gutter, no empty half).
        let mut one = Layout::new(vec![(100.0, 100.0)]);
        one.arrange(Arrangement { spread: Spread::Cover, r2l: false });
        one.set(1.0, 0, 10, 20);
        assert_eq!((one.content_width(), one.left(0, 500, 0)), (140, 200));
        // Even pages: the last row is full.
        let mut four = Layout::new(vec![(100.0, 100.0); 4]);
        four.arrange(Arrangement { spread: Spread::Pair, r2l: false });
        four.set(1.0, 0, 10, 20);
        assert_eq!((four.pages_in_row(3), four.content_width()), (2..4, 100 + 5 + 100 + 40));
    }

    #[test]
    fn pages_that_read_from_the_right_are_the_mirror_of_those_that_read_from_the_left() {
        let (a, b) = (five(Spread::Pair, false), five(Spread::Pair, true));
        let wide = 1401;
        // The first page of a pair is on the right now.
        assert!(b.left(0, wide, 0) > b.left(1, wide, 0));
        assert!(b.left(2, wide, 0) > b.left(3, wide, 0));
        // The rows and their heights are the same ...
        assert_eq!((a.content_width(), a.content_height()), (b.content_width(), b.content_height()));
        assert_eq!((a.top(0), a.top(2), a.top(4)), (b.top(0), b.top(2), b.top(4)));
        // ... and every page is where the mirror image of the other layout puts it (the window is as wide as an odd number of pixels more than
        // the document, so that the two margins are the same).
        for p in 0..5 {
            assert_eq!(b.left(p, wide, 0), wide - a.left(p, wide, 0) - a.size_px(p).0, "page {p}");
        }
        // The last page is alone, on the right; the cover alone, on the left.
        assert_eq!(b.left(4, wide, 0), b.left(0, wide, 0));
        let (c, d) = (five(Spread::Cover, true), five(Spread::Cover, false));
        assert!(c.left(0, wide, 0) < c.left(1, wide, 0));
        assert!(c.left(1, wide, 0) > c.left(2, wide, 0));
        assert_eq!(c.left(0, wide, 0) + 595, c.left(2, wide, 0) + 400);
        for p in 0..5 {
            assert_eq!(c.left(p, wide, 0), wide - d.left(p, wide, 0) - d.size_px(p).0, "page {p}");
        }
    }

    #[test]
    fn the_page_under_a_place_in_a_spread_and_the_nearest_when_it_is_in_the_gap() {
        let l = five(Spread::Pair, false);
        let wide = 1400;
        let (l0, l1) = (l.left(0, wide, 0), l.left(1, wide, 0));
        let y = l.top(0) + 100;
        assert_eq!(l.page_under(l0 + 10, y, wide, 0), 0);
        assert_eq!(l.page_under(l1 + 10, y, wide, 0), 1);
        // The gutter belongs to the page it is nearer to (a tie goes to the left page); the margins to the page beside them.
        assert_eq!(l.page_under(l0 + 595 + 1, y, wide, 0), 0);
        assert_eq!(l.page_under(l1 - 1, y, wide, 0), 1);
        assert_eq!(l.page_under(-50, y, wide, 0), 0);
        assert_eq!(l.page_under(5000, y, wide, 0), 1);
        // Below the short page of the second row, where only the tall page beside it is, the nearest page of the row is found.
        let y2 = l.top(2) + 300 + 20;
        assert_eq!(l.page_under(l.left(2, wide, 0) + 10, y2, wide, 0), 2);
        assert_eq!(l.page_under(l.left(3, wide, 0) + 10, y2, wide, 0), 3);
        // Down in the gap between rows and past the end.
        assert_eq!(l.page_under(l1 + 5, l.content_height() + 100, wide, 0), 4);
        // Scrolling sideways moves the pages under the pointer.
        assert_eq!(l.page_under(10, y, 500, 700), 1);
        assert_eq!(l.page_under(10, y, 500, 0), 0);
        // In the column it is the page of the row.
        let col = three();
        assert_eq!(col.page_under(0, col.top(1) + 1, 1000, 0), 1);
    }

    #[test]
    fn rows_are_what_is_in_view_what_the_scroll_goes_to_and_what_a_place_is_kept_by() {
        let l = five(Spread::Pair, false);
        // Rows start at 20, 20 + 842 + 10 = 872, 872 + 842 + 10 = 1724.
        assert_eq!(l.visible(0, 100), 0..2);
        assert_eq!(l.visible(0, 900), 0..4);
        assert_eq!(l.visible(1000, 1100), 2..4);
        assert_eq!(l.visible(1724, 1725), 4..5);
        // y in the gap under the first row: the second row only.
        assert_eq!(l.visible(865, 900), 2..4);
        assert_eq!(l.visible(0, 10_000), 0..5);
        assert_eq!((l.page_at(0), l.page_at(900), l.page_at(1800), l.page_at(-5)), (0, 2, 4, 0));
        // Turning to a page puts its row at the top; either page of the row is the same place.
        assert_eq!((l.scroll_to_page(0, 6), l.scroll_to_page(1, 6), l.scroll_to_page(3, 6), l.scroll_to_page(4, 0)), (14, 14, 866, 1724));
        // A place in a row is kept as a fraction of the row, for any page of the row, and survives a change of scale.
        let (page, f) = l.locate(872 + 421);
        assert_eq!(page, 2);
        assert!((f - 0.5).abs() < 0.01);
        let mut m = five(Spread::Pair, false);
        m.set(2.0, 0, 10, 20);
        let y = m.y_of(3, f);
        assert!((y - (m.scroll_to_page(2, 0) + m.rows[1].height / 2)).abs() <= 2, "{y}");
        assert_eq!(m.y_of(99, 0.5), 0);
        // The zoom that fits is about the widest row, and the row's height about the page shown.
        let (pt, gap) = l.widest_pt();
        assert_eq!((pt, gap), (595.0 + 595.0, 5));
        assert_eq!((l.row_height_pt(2), l.row_height_pt(4), l.row_height_pt(9)), (842.0, 1000.0, 0.0));
        // A column again: the same layout, a page to a row.
        let mut c = five(Spread::Single, false);
        assert_eq!((c.pages_in_row(1), c.widest_pt()), (1..2, (595.0, 0)));
        c.arrange(Arrangement { spread: Spread::Cover, r2l: true });
        c.set(1.0, 0, 10, 20);
        assert_eq!(c.pages_in_row(0), 0..1);
    }

    #[test]
    fn the_modes_have_numbers_and_words_that_come_back() {
        for s in Spread::ALL {
            assert_eq!(Spread::from_code(s.code()), Some(s));
            assert_eq!(Spread::parse(s.word()), Some(s));
        }
        assert_eq!((Spread::from_code(3), Spread::parse("two"), Spread::parse(" cover ")), (None, None, Some(Spread::Cover)));
        use qingpdf_core::view::{SPREAD_FIRST_LEFT, SPREAD_FIRST_RIGHT, SPREAD_NONE};
        assert_eq!((Spread::from_hint(SPREAD_NONE), Spread::from_hint(SPREAD_FIRST_LEFT), Spread::from_hint(SPREAD_FIRST_RIGHT), Spread::from_hint(9)), (None, Some(Spread::Pair), Some(Spread::Cover), None));
    }
}
