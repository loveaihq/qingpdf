//! Where the pages are: a column of pages with a gap between, in pixels at the current scale and rotation. Plain
//! arithmetic; nothing here knows about windows.

use std::ops::Range;

/// The most a side of a page counts for, in points (the engine tells no more than this: see `MAX_PAGE_UNITS`). A bigger or
/// not-a-number size is cut to this here too, so that nothing below can overflow.
pub const MAX_PAGE_POINTS: f64 = qingpdf_core::view::MAX_PAGE_UNITS;

/// The most pixels a side of a page has in the layout.
const MAX_SIDE_PIXELS: f64 = 1.0e9;

/// The pages laid out one under the other. Coordinates are those of the whole document: x from the left edge of
/// the widest page, y from the top of the first page.
pub struct Layout {
    /// Size of each page in points as the engine shows it (its own `/Rotate` already applied).
    sizes: Vec<(f64, f64)>,
    rotation: u16,
    scale: f64,
    gap: i64,
    margin: i64,
    widths: Vec<i64>,
    heights: Vec<i64>,
    tops: Vec<i64>,
    content_w: i64,
    content_h: i64,
}

impl Layout {
    pub fn new(sizes: Vec<(f64, f64)>) -> Layout {
        let sizes = sizes.into_iter().map(|(w, h)| (points(w), points(h))).collect();
        let mut l = Layout { sizes, rotation: 0, scale: 1.0, gap: 0, margin: 0, widths: Vec::new(), heights: Vec::new(), tops: Vec::new(), content_w: 0, content_h: 0 };
        l.set(1.0, 0, 0, 0);
        l
    }

    /// Lay the pages out at `scale` pixels a point, turned `rotation` degrees (a multiple of 90) further, with `gap`
    /// pixels between pages and `margin` around the whole.
    pub fn set(&mut self, scale: f64, rotation: u16, gap: i64, margin: i64) {
        self.scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
        self.rotation = rotation % 360;
        self.gap = gap.clamp(0, 1_000_000);
        self.margin = margin.clamp(0, 1_000_000);
        let turned = self.rotation % 180 == 90;
        self.widths.clear();
        self.heights.clear();
        self.tops.clear();
        let mut y = self.margin;
        let mut widest = 0;
        for &(w, h) in &self.sizes {
            let (w, h) = if turned { (h, w) } else { (w, h) };
            let (pw, ph) = (pixels(w * self.scale), pixels(h * self.scale));
            self.tops.push(y);
            self.widths.push(pw);
            self.heights.push(ph);
            y = y.saturating_add(ph).saturating_add(self.gap);
            widest = widest.max(pw);
        }
        self.content_h = if self.sizes.is_empty() { 0 } else { y.saturating_sub(self.gap).saturating_add(self.margin) };
        self.content_w = widest.saturating_add(self.margin.saturating_mul(2));
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

    /// The width in points of the widest page as it is laid out (turned).
    pub fn widest_pt(&self) -> f64 {
        let turned = self.rotation % 180 == 90;
        self.sizes.iter().map(|&(w, h)| if turned { h } else { w }).fold(0.0, f64::max)
    }

    /// A page's size in pixels.
    pub fn size_px(&self, page: usize) -> (i64, i64) {
        (self.widths.get(page).copied().unwrap_or(0), self.heights.get(page).copied().unwrap_or(0))
    }

    pub fn top(&self, page: usize) -> i64 {
        self.tops.get(page).copied().unwrap_or(0)
    }

    pub fn content_width(&self) -> i64 {
        self.content_w
    }

    pub fn content_height(&self) -> i64 {
        self.content_h
    }

    /// The page that holds document row `y`, or the one nearest to it (the gap belongs to the page above).
    pub fn page_at(&self, y: i64) -> usize {
        if self.tops.is_empty() {
            return 0;
        }
        self.tops.partition_point(|&t| t <= y).saturating_sub(1)
    }

    /// The pages that have a row in `y0..y1`.
    pub fn visible(&self, y0: i64, y1: i64) -> Range<usize> {
        if self.tops.is_empty() || y1 <= y0 {
            return 0..0;
        }
        let first = self.page_at(y0);
        // A page after the first one that starts before y1.
        let end = self.tops.partition_point(|&t| t < y1);
        // The first page may end above y0 (y0 is in the gap below it).
        let first = if self.top(first) + self.size_px(first).1 <= y0 && first + 1 < self.len() { first + 1 } else { first };
        first..end.max(first)
    }

    /// The left edge of a page in a viewport `viewport_w` wide that is scrolled `scroll_x` pixels: the page is
    /// centred when the document is narrower than the viewport.
    pub fn left(&self, page: usize, viewport_w: i64, scroll_x: i64) -> i64 {
        let w = self.widths.get(page).copied().unwrap_or(0);
        if self.content_w <= viewport_w {
            (viewport_w - w) / 2
        } else {
            (self.margin + (self.content_w - 2 * self.margin - w) / 2).saturating_sub(scroll_x)
        }
    }

    /// The most the document can be scrolled: (right, down), 0 when it fits.
    pub fn max_scroll(&self, viewport_w: i64, viewport_h: i64) -> (i64, i64) {
        ((self.content_w - viewport_w).max(0), (self.content_h - viewport_h).max(0))
    }

    /// The place `scroll_y` has to be at for page `page` to start `offset_px` pixels below the top of the viewport.
    pub fn scroll_to_page(&self, page: usize, offset_px: i64) -> i64 {
        self.top(page).saturating_sub(offset_px)
    }

    /// Where `y` is in its page: (page, fraction down the page from 0 to 1, the gap below counted as the page's end).
    pub fn locate(&self, y: i64) -> (usize, f64) {
        let page = self.page_at(y);
        let h = self.size_px(page).1.max(1);
        (page, y.saturating_sub(self.top(page)) as f64 / h as f64)
    }

    /// The inverse of [`Layout::locate`].
    pub fn y_of(&self, page: usize, fraction: f64) -> i64 {
        self.top(page).saturating_add((fraction * self.size_px(page).1 as f64).round() as i64)
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
        assert_eq!(l.widest_pt(), 150.0);
        l.set(1.0, 90, 0, 0);
        assert_eq!(l.widest_pt(), 200.0);
        assert_eq!(Layout::new(Vec::new()).widest_pt(), 0.0);
    }

    #[test]
    fn pages_of_absurd_sizes_are_cut_to_the_limit_and_nothing_overflows() {
        let mut l = Layout::new(vec![(1e300, 1e300), (f64::INFINITY, f64::NEG_INFINITY), (f64::NAN, 5.0), (-3.0, 0.0), (1e300, 10.0)]);
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
            assert_eq!(page, last);
            assert!(fraction.is_finite());
            let _ = (l.y_of(page, fraction), l.left(0, 800, i64::MAX / 4), l.max_scroll(800, 600), l.scroll_to_page(last, i64::MIN), l.visible(0, i64::MAX));
        }
        // At the largest zoom (6400 percent at 96 dpi) the largest page is a big but plain number of pixels.
        let mut l = Layout::new(vec![(MAX_PAGE_POINTS, MAX_PAGE_POINTS); 1000]);
        l.set(64.0 * 96.0 / 72.0, 0, 10, 12);
        assert_eq!(l.size_px(0), (1_228_800, 1_228_800));
        assert_eq!(l.content_height(), 1000 * 1_228_800 + 999 * 10 + 24);
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
}
