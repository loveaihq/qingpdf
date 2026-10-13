//! Tables on a page (5b, 5d), shared by the Word and the Excel export. A table is a [`Grid`]: column edges, row edges, and which cells
//! are merged into which. Two ways to find one:
//!
//! * [`ruled`]: the thin lines of the page joined into a grid (the edges of the cells are where the lines are);
//! * [`loose`]: text with no lines round it, in rows and columns that line up (bank statements, financial reports).
//!
//! [`find`] gives both, the ruled ones first and the loose ones in the text that is not in a ruled table.

use qingpdf_base::text::PageLayout;

pub(crate) mod loose;
pub(crate) mod ruled;

/// Most tables one page gives, and most rows and columns of a table.
pub(crate) const MAX_TABLES: usize = 64;
pub(crate) const MAX_ROWS: usize = 300;
pub(crate) const MAX_COLS: usize = 40;

pub(crate) struct Grid {
    pub(crate) bbox: [f32; 4],
    /// The column edges (left to right) and the row edges (top to bottom).
    pub(crate) xs: Vec<f32>,
    pub(crate) ys: Vec<f32>,
    /// Is there a line on the left edge of cell (row, column), `(rows) × (cols + 1)`, and on the top edge, `(rows + 1) × (cols)`.
    vline: Vec<bool>,
    hline: Vec<bool>,
    /// The cell (row, column) that each cell belongs to: itself, or the one it was merged into.
    owner: Vec<(u16, u16)>,
}

impl Grid {
    pub(crate) fn rows(&self) -> usize {
        self.ys.len().saturating_sub(1)
    }

    pub(crate) fn cols(&self) -> usize {
        self.xs.len().saturating_sub(1)
    }

    pub(crate) fn owner(&self, r: usize, c: usize) -> (usize, usize) {
        self.owner.get(r * self.cols() + c).map_or((r, c), |&(a, b)| (usize::from(a), usize::from(b)))
    }

    /// Is there a line on the left edge of column `c` (`c == cols` is the right edge of the last one) in row `r`?
    pub(crate) fn v(&self, r: usize, c: usize) -> bool {
        self.vline.get(r * (self.cols() + 1) + c).copied().unwrap_or(false)
    }

    /// Is there a line on the top edge of row `r` (`r == rows` is the bottom of the last) in column `c`?
    pub(crate) fn h(&self, r: usize, c: usize) -> bool {
        self.hline.get(r * self.cols() + c).copied().unwrap_or(false)
    }

    /// The cell (row, column) a point is in, clamped to the table.
    pub(crate) fn cell_at(&self, x: f32, y: f32) -> (usize, usize) {
        let find = |edges: &[f32], v: f32| edges.partition_point(|&e| e <= v).saturating_sub(1).min(edges.len().saturating_sub(2));
        self.owner(find(&self.ys, y), find(&self.xs, x))
    }
}

/// Sort `v` so that `less(a, b)` holds for each `a` before a `b` (a shell sort: the sorts of the library are some kilobytes for each
/// kind of thing sorted, and the lists here are short; this is one small loop for all of them).
pub(crate) fn sort<T>(v: &mut [T], less: &mut dyn FnMut(&T, &T) -> bool) {
    let n = v.len();
    let mut h = 1;
    while h < n / 3 {
        h = 3 * h + 1;
    }
    while h >= 1 {
        for i in h..n {
            let mut j = i;
            while j >= h && v.get(j).zip(v.get(j - h)).is_some_and(|(a, b)| less(a, b)) {
                v.swap(j, j - h);
                j -= h;
            }
        }
        h /= 3;
    }
}

/// Is the character a wide one (Chinese, Japanese, Korean, full-width), which is written with no space round it?
pub(crate) fn is_wide(c: char) -> bool {
    matches!(u32::from(c), 0x2E80..=0xA4CF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFFEF | 0x20000..=0x2FFFF)
}

/// Is the page mostly vertical text (no tables are looked for in it)?
pub(crate) fn vertical(layout: &PageLayout) -> bool {
    let total: usize = layout.lines.iter().map(|l| l.text.len()).sum();
    let vertical: usize = layout.lines.iter().filter(|l| l.vertical).map(|l| l.text.len()).sum();
    total > 0 && vertical * 2 > total
}

/// The cost of the analysis of the text and lines of a page, for the work meter: units per character, line and rule.
pub(crate) fn cost(layout: &PageLayout) -> f64 {
    let chars: usize = layout.lines.iter().map(|l| l.text.len()).sum();
    chars as f64 * 150.0 + layout.lines.len() as f64 * 5_000.0 + layout.rules.len() as f64 * 4_000.0
}

/// The tables of a page, top to bottom, and the work they took to find (for the work meter). `vertical`: the page is vertical text
/// (no tables are looked for). `numbers`: a table made of text only needs a column of numbers (see [`loose::find`]).
pub(crate) fn find(layout: &PageLayout, vertical: bool, numbers: bool) -> (Vec<Grid>, f64) {
    if vertical {
        return (Vec::new(), 0.0);
    }
    let (mut grids, pairs) = ruled::find(&layout.rules, layout.size);
    let taken: Vec<[f32; 4]> = grids.iter().map(|g| g.bbox).collect();
    let (more, looked) = loose::find(&layout.lines, &taken, numbers);
    grids.extend(more);
    sort(&mut grids, &mut |a, b| (a.bbox[1], a.bbox[0]) < (b.bbox[1], b.bbox[0]));
    grids.truncate(MAX_TABLES);
    (grids, pairs as f64 * ruled::PAIR_WORK + looked as f64 * loose::FRAGMENT_WORK)
}
