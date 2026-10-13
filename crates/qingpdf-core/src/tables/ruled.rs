//! Ruled tables (5b): the thin lines of a page (`PageLayout::rules`) joined into grids. Lines that touch make one table; the
//! places where they cross are the cell corners; a cell edge with no line along it is not an edge, so the cells either side of
//! it are one cell (a merged cell). A table needs at least two rows and two columns of cells: boxes in a drawing and text with no lines around it are no table here.

use super::{Grid, MAX_COLS, MAX_ROWS, MAX_TABLES, sort};

/// How near (points) two lines must be to be one line, and to touch.
const TOL: f32 = 2.0;
/// Most lines of a page we try to find tables in (a chart or a hatched area has thousands).
const MAX_LINES: usize = 4_000;
/// Most pairs of a horizontal and a vertical line of a page that are looked at for a crossing (a page with more has no tables found
/// in it), and what each costs in the work meter.
const MAX_PAIRS: usize = 1 << 20;
pub(crate) const PAIR_WORK: f64 = 8.0;

#[derive(Clone, Copy)]
struct Seg {
    /// The fixed coordinate and the extent along the line.
    at: f32,
    from: f32,
    to: f32,
}

/// Lines of the same coordinate (within `TOL`) that overlap or nearly touch become one.
fn merge(mut segs: Vec<Seg>) -> Vec<Seg> {
    sort(&mut segs, &mut |a, b| (a.at, a.from) < (b.at, b.from));
    let mut out: Vec<Seg> = Vec::with_capacity(segs.len());
    for s in segs {
        match out.last_mut() {
            Some(last) if (s.at - last.at).abs() <= TOL && s.from <= last.to + TOL => last.to = last.to.max(s.to),
            _ => out.push(s),
        }
    }
    out
}

/// Sets of lines that touch: union by rank with path compression, so that joining two lines costs next to nothing however many
/// there are (a page of a thousand rules each crossing a thousand others must not take longer than reading them).
struct Sets {
    parent: Vec<usize>,
    rank: Vec<u8>,
}

impl Sets {
    fn new(n: usize) -> Sets {
        Sets { parent: (0..n).collect(), rank: vec![0; n] }
    }

    fn root(&mut self, x: usize) -> usize {
        let mut r = x;
        while let Some(&p) = self.parent.get(r) {
            if p == r {
                break;
            }
            r = p;
        }
        // (Everything on the way now points straight at the root.)
        let mut at = x;
        while at != r {
            let next = self.parent.get(at).copied().unwrap_or(r);
            if let Some(slot) = self.parent.get_mut(at) {
                *slot = r;
            }
            at = next;
        }
        r
    }

    fn union(&mut self, a: usize, b: usize) {
        let (x, y) = (self.root(a), self.root(b));
        if x == y {
            return;
        }
        let (rx, ry) = (self.rank.get(x).copied().unwrap_or(0), self.rank.get(y).copied().unwrap_or(0));
        let (low, high) = if rx < ry { (x, y) } else { (y, x) };
        if let Some(slot) = self.parent.get_mut(low) {
            *slot = high;
        }
        if rx == ry
            && let Some(r) = self.rank.get_mut(high)
        {
            *r = r.saturating_add(1);
        }
    }
}

/// The edges of a set of coordinates: sorted, the ones within `TOL` of each other made one.
fn edges(mut v: Vec<f32>) -> Vec<f32> {
    sort(&mut v, &mut |a, b| a < b);
    let mut out: Vec<f32> = Vec::new();
    for x in v {
        match out.last() {
            Some(&l) if x - l <= TOL => {}
            _ => out.push(x),
        }
    }
    out
}

/// The tables of a page, and how many pairs of lines were looked at for a crossing (for the work meter). `rules` are
/// `[left, top, right, bottom]` lines with one of the two sizes 0; `page` is the page's size.
pub(crate) fn find(rules: &[[f32; 4]], page: (f32, f32)) -> (Vec<Grid>, u64) {
    let (mut hs, mut vs) = (Vec::new(), Vec::new());
    for r in rules {
        if r[3] - r[1] < 0.5 && r[2] - r[0] >= 3.0 {
            hs.push(Seg { at: 0.5 * (r[1] + r[3]), from: r[0], to: r[2] });
        } else if r[2] - r[0] < 0.5 && r[3] - r[1] >= 3.0 {
            vs.push(Seg { at: 0.5 * (r[0] + r[2]), from: r[1], to: r[3] });
        }
    }
    let (hs, vs) = (merge(hs), merge(vs));
    let pairs = hs.len().saturating_mul(vs.len());
    if hs.len() < 2 || vs.len() < 2 || hs.len() + vs.len() > MAX_LINES || pairs > MAX_PAIRS {
        return (Vec::new(), 0);
    }
    // Lines that touch are one table.
    let n = hs.len() + vs.len();
    let mut sets = Sets::new(n);
    for (i, h) in hs.iter().enumerate() {
        for (j, v) in vs.iter().enumerate() {
            if v.at >= h.from - TOL && v.at <= h.to + TOL && h.at >= v.from - TOL && h.at <= v.to + TOL {
                sets.union(i, hs.len() + j);
            }
        }
    }
    // The lines of each set, by the root of the set.
    let mut members: Vec<Vec<usize>> = vec![Vec::new(); n];
    for i in 0..n {
        let r = sets.root(i);
        if let Some(list) = members.get_mut(r) {
            list.push(i);
        }
    }
    let mut tables = Vec::new();
    for start in 0..n {
        let top = sets.root(start);
        let Some(list) = members.get_mut(top).map(std::mem::take).filter(|l| !l.is_empty()) else { continue };
        let mine_h: Vec<&Seg> = list.iter().filter(|&&i| i < hs.len()).filter_map(|&i| hs.get(i)).collect();
        let mine_v: Vec<&Seg> = list.iter().filter(|&&i| i >= hs.len()).filter_map(|&i| vs.get(i - hs.len())).collect();
        if mine_h.len() < 2 || mine_v.len() < 2 {
            continue;
        }
        let ys = edges(mine_h.iter().map(|s| s.at).collect());
        let xs = edges(mine_v.iter().map(|s| s.at).collect());
        let (rows, cols) = (ys.len().saturating_sub(1), xs.len().saturating_sub(1));
        if rows < 2 || cols < 2 || rows > MAX_ROWS || cols > MAX_COLS {
            continue;
        }
        let at = |v: &[f32], i: usize| v.get(i).copied().unwrap_or(0.0);
        let bbox = [at(&xs, 0), at(&ys, 0), at(&xs, cols), at(&ys, rows)];
        // A frame around the whole page is no table.
        if (bbox[2] - bbox[0]) * (bbox[3] - bbox[1]) > 0.85 * page.0 * page.1 {
            continue;
        }
        let near = |edges: &[f32], v: f32| edges.iter().position(|&e| (e - v).abs() <= TOL);
        // Which edges have a line along them: a line at the edge's coordinate that spans the cell.
        let mut vline = vec![false; rows * (cols + 1)];
        let mut hline = vec![false; (rows + 1) * cols];
        for v in &mine_v {
            let Some(c) = near(&xs, v.at) else { continue };
            for r in 0..rows {
                if v.from <= at(&ys, r) + TOL && v.to >= at(&ys, r + 1) - TOL
                    && let Some(slot) = vline.get_mut(r * (cols + 1) + c)
                {
                    *slot = true;
                }
            }
        }
        for h in &mine_h {
            let Some(r) = near(&ys, h.at) else { continue };
            for c in 0..cols {
                if h.from <= at(&xs, c) + TOL && h.to >= at(&xs, c + 1) - TOL
                    && let Some(slot) = hline.get_mut(r * cols + c)
                {
                    *slot = true;
                }
            }
        }
        // A cell with no line on its left edge is part of the one on its left, else with none on its top edge, of the one above.
        let mut owner: Vec<(u16, u16)> = Vec::with_capacity(rows * cols);
        for r in 0..rows {
            for c in 0..cols {
                let flag = |v: &[bool], i: usize| v.get(i).copied().unwrap_or(false);
                let own = |owner: &[(u16, u16)], i: usize| owner.get(i).copied().unwrap_or((0, 0));
                let o = if c > 0 && !flag(&vline, r * (cols + 1) + c) {
                    own(&owner, r * cols + c - 1)
                } else if r > 0 && !flag(&hline, r * cols + c) {
                    own(&owner, (r - 1) * cols + c)
                } else {
                    (r as u16, c as u16)
                };
                owner.push(o);
            }
        }
        tables.push(Grid { bbox, xs, ys, vline, hline, owner });
        if tables.len() >= MAX_TABLES {
            break;
        }
    }
    sort(&mut tables, &mut |a, b| (a.bbox[1], a.bbox[0]) < (b.bbox[1], b.bbox[0]));
    (tables, pairs as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid_lines(x: [f32; 4], y: [f32; 3]) -> Vec<[f32; 4]> {
        let mut r = Vec::new();
        for &yy in &y {
            r.push([x[0], yy, x[3], yy]);
        }
        for &xx in &x {
            r.push([xx, y[0], xx, y[2]]);
        }
        r
    }

    #[test]
    fn a_grid_of_lines_is_a_table() {
        let (g, _) = find(&grid_lines([100.0, 200.0, 300.0, 400.0], [100.0, 130.0, 160.0]), (612.0, 792.0));
        assert_eq!(g.len(), 1);
        assert_eq!((g[0].rows(), g[0].cols()), (2, 3));
        assert_eq!(g[0].cell_at(150.0, 110.0), (0, 0));
        assert_eq!(g[0].cell_at(350.0, 150.0), (1, 2));
    }

    #[test]
    fn a_missing_inner_line_merges_cells() {
        let mut lines = grid_lines([100.0, 200.0, 300.0, 400.0], [100.0, 130.0, 160.0]);
        // Take the inner vertical line at x = 200 away from the first row only: cut it to the second row.
        lines.retain(|l| !(l[0] == 200.0 && l[2] == 200.0));
        lines.push([200.0, 130.0, 200.0, 160.0]);
        let (g, _) = find(&lines, (612.0, 792.0));
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].owner(0, 1), (0, 0));
        assert_eq!(g[0].owner(1, 1), (1, 1));
    }

    #[test]
    fn lines_alone_and_page_frames_are_no_tables() {
        assert!(find(&[[10.0, 10.0, 200.0, 10.0], [10.0, 40.0, 200.0, 40.0]], (612.0, 792.0)).0.is_empty());
        assert!(find(&grid_lines([5.0, 300.0, 400.0, 607.0], [5.0, 400.0, 787.0]), (612.0, 792.0)).0.is_empty());
    }

    #[test]
    fn a_page_of_a_thousand_crossing_rules_is_looked_at_in_no_time() {
        let cross = |n: usize| -> Vec<[f32; 4]> {
            (0..n).flat_map(|i| [[10.0, 10.0 + 3.0 * i as f32, 5000.0, 10.0 + 3.0 * i as f32], [10.0 + 3.0 * i as f32, 10.0, 10.0 + 3.0 * i as f32, 5000.0]]).collect()
        };
        let t = std::time::Instant::now();
        // 1500 rules across and 1500 down, each crossing all of the others: 2.25 million pairs, more than a page is allowed.
        let (grids, tested) = find(&cross(1500), (20000.0, 20000.0));
        assert!(grids.is_empty() && tested == 0);
        // Within the allowed number of pairs, the lines are joined at once (and the table has far too many rows).
        let (grids, tested) = find(&cross(1000), (20000.0, 20000.0));
        assert!(grids.is_empty() && tested == 1_000_000);
        assert!(t.elapsed() < std::time::Duration::from_secs(2), "{:?}", t.elapsed());
    }
}
