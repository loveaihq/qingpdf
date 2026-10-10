//! A page at a zoom is drawn whole when it is small enough, and in square pieces (tiles) when it is not, so that no
//! single piece takes more memory than the plan allows and only the pieces in view need to be drawn.

/// The side of a tile of a page that is cut up.
const EDGE: i64 = 2048;

/// A piece of a page: `tx`, `ty` name it; `x`, `y`, `w`, `h` are pixels of the whole page at the zoom it is drawn at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Tile {
    pub tx: u32,
    pub ty: u32,
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
}

/// How a page of `page_w` by `page_h` pixels is cut: the side of a tile, or `None` for no cutting.
fn edge(page_w: i64, page_h: i64, max_pixels: u32) -> Option<i64> {
    if page_w.saturating_mul(page_h) <= i64::from(max_pixels) {
        return None;
    }
    // Squares as big as the limit allows, but no bigger than EDGE.
    let side = (f64::from(max_pixels).sqrt() as i64).clamp(64, EDGE);
    Some(side)
}

/// The tiles of the page that overlap the pixels `x0..x1`, `y0..y1` of it (given in page pixels; cut to the page),
/// from the top left, row by row.
pub fn overlapping(page_w: i64, page_h: i64, max_pixels: u32, x0: i64, y0: i64, x1: i64, y1: i64) -> Vec<Tile> {
    let (x0, y0, x1, y1) = (x0.max(0), y0.max(0), x1.min(page_w), y1.min(page_h));
    if page_w <= 0 || page_h <= 0 || x0 >= x1 || y0 >= y1 {
        return Vec::new();
    }
    let Some(side) = edge(page_w, page_h, max_pixels) else {
        return vec![Tile { tx: 0, ty: 0, x: 0, y: 0, w: page_w, h: page_h }];
    };
    let (c0, c1) = (x0 / side, (x1 - 1) / side);
    let (r0, r1) = (y0 / side, (y1 - 1) / side);
    let mut out = Vec::new();
    for r in r0..=r1 {
        for c in c0..=c1 {
            let (x, y) = (c * side, r * side);
            out.push(Tile { tx: c as u32, ty: r as u32, x, y, w: side.min(page_w - x), h: side.min(page_h - y) });
        }
    }
    out
}

/// How many tiles the whole page has.
pub fn count(page_w: i64, page_h: i64, max_pixels: u32) -> u64 {
    match edge(page_w, page_h, max_pixels) {
        None => 1,
        Some(side) => ((page_w + side - 1) / side) as u64 * ((page_h + side - 1) / side) as u64,
    }
}

/// The tile `tx`, `ty` of a page, if it has one.
#[cfg(test)]
pub fn tile(page_w: i64, page_h: i64, max_pixels: u32, tx: u32, ty: u32) -> Option<Tile> {
    let Some(side) = edge(page_w, page_h, max_pixels) else {
        return (tx == 0 && ty == 0 && page_w > 0 && page_h > 0).then_some(Tile { tx, ty, x: 0, y: 0, w: page_w, h: page_h });
    };
    let (x, y) = (i64::from(tx) * side, i64::from(ty) * side);
    (x < page_w && y < page_h).then_some(Tile { tx, ty, x, y, w: side.min(page_w - x), h: side.min(page_h - y) })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX: u32 = 4 * 1024 * 1024;

    #[test]
    fn a_small_page_is_one_tile() {
        let t = overlapping(900, 1270, MAX, 0, 0, 900, 100);
        assert_eq!(t, vec![Tile { tx: 0, ty: 0, x: 0, y: 0, w: 900, h: 1270 }]);
        assert_eq!(count(900, 1270, MAX), 1);
        // A page of exactly the limit is still one.
        assert_eq!(count(2048, 2048, MAX), 1);
        assert_eq!(count(2049, 2048, MAX), 2);
    }

    #[test]
    fn a_big_page_is_cut_and_only_the_tiles_in_view_are_listed() {
        // 800 percent of A4: 4760 by 6736 pixels.
        let (w, h) = (4760, 6736);
        assert_eq!(count(w, h, MAX), 3 * 4);
        let t = overlapping(w, h, MAX, 2000, 2000, 2100, 2100);
        // Straddles the corner of four tiles (the line is at 2048).
        assert_eq!(t.len(), 4);
        assert_eq!((t[0].tx, t[0].ty, t[3].tx, t[3].ty), (0, 0, 1, 1));
        let last = overlapping(w, h, MAX, w - 5, h - 5, w + 100, h + 100);
        assert_eq!(last.len(), 1);
        assert_eq!((last[0].x, last[0].y, last[0].w, last[0].h), (4096, 6144, 664, 592));
        // Nothing outside the page.
        assert!(overlapping(w, h, MAX, -50, -50, -1, 10).is_empty());
        assert!(overlapping(w, h, MAX, 10, 10, 10, 20).is_empty());
    }

    #[test]
    fn the_tiles_cover_the_page_once_and_stay_under_the_limit() {
        for &(w, h, max) in &[(4760i64, 6736i64, MAX), (3000, 3000, 1 << 20), (70000, 90, 1 << 20), (100, 100, 5000)] {
            let tiles = overlapping(w, h, max, 0, 0, w, h);
            let area: i64 = tiles.iter().map(|t| t.w * t.h).sum();
            assert_eq!(area, w * h);
            assert_eq!(tiles.len() as u64, count(w, h, max));
            assert!(tiles.iter().all(|t| t.w * t.h <= i64::from(max.max(64 * 64))), "{w}x{h}");
            for t in &tiles {
                assert_eq!(tile(w, h, max, t.tx, t.ty), Some(*t));
            }
        }
        assert_eq!(tile(900, 1270, MAX, 1, 0), None);
    }
}
