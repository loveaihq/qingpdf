//! Tables with no lines round them (5d): bank statements, financial reports, price lists. The text is found in rows and columns
//! that line up.
//!
//! * The characters of a line are cut into *fragments* where the gap between two of them is wide (0.8 of a font size): a cell, or a
//!   piece of one. Fragments are put in *rows* by their baselines, whichever line of the page they were drawn in (a file may draw a
//!   table row by row or column by column).
//! * A run of rows of which at least three have two fragments or more is a table, if the places where the fragments are line up: the *columns* are what is left between the strips of the page that no fragment of the rows covers
//!   (a strip crossed by one row in four is still a gap: a heading across two columns). Text that fills its columns, as the lines of
//!   a book printed in two columns do, is no table. Nor is a list with dot leaders (a table of contents: `Chapter 1 ......... 5`).
//! * Rows follow each other by the spacing of the lines. A row with only a piece of text under a cell of the row above, at the
//!   spacing of the lines of one paragraph, is the second line of that cell (a cell folded in two): it joins the row above. A
//!   fragment across two columns or more is a merged cell.
//!
//! Everything is done in a few passes over the fragments (the sorts are the only part that is not linear), so a page of a hundred
//! thousand lines is no slower than reading it. The result is a [`Grid`] like the ruled tables', with no lines on any edge.

use qingpdf_base::text::TextLine;

use super::{Grid, MAX_COLS, MAX_ROWS, MAX_TABLES};

/// Work units for a fragment of text looked at (put in a row, counted in the columns, put in a cell).
pub(crate) const FRAGMENT_WORK: f64 = 2_000.0;
/// A page with more fragments than this has no tables looked for in it (a page has no more characters than this, so none).
const MAX_FRAGMENTS: usize = 400_000;
/// A gap wider than this many font sizes between two characters of a line starts a new fragment.
const FRAGMENT_GAP: f32 = 0.8;
/// Fragments whose baselines are within this many font sizes of the first of a row are in the row.
const ROW_TOLERANCE: f32 = 0.45;
/// Blank space between two rows of more than this many font sizes ends a table.
const ROW_BREAK: f32 = 1.6;
/// Most rows of one fragment, one after the other, inside a table.
const MAX_SPARSE: usize = 4;
/// A fragment as long as this many font sizes, or of this many spaces, is a piece of running text; a table of which half the fragments
/// are is text in columns.
const LONG: f32 = 12.0;
const WORDS: u16 = 3;
/// The second line of a cell is at most this many font sizes under the first (the spacing of the lines of a paragraph).
const FOLD_PITCH: f32 = 1.45;

#[derive(Clone, Copy)]
struct Frag {
    a0: f32,
    a1: f32,
    base: f32,
    top: f32,
    bot: f32,
    size: f32,
    /// Spaces in the text (words, less one), digits, and characters that are not spaces.
    spaces: u16,
    digits: u16,
    ink: u16,
    /// The line has a dot leader (`……` or `.....`).
    leader: bool,
}

impl Frag {
    /// Mostly digits: a number, a date, an amount.
    fn numeric(&self) -> bool {
        self.ink > 0 && self.digits * 2 >= self.ink
    }
}

/// Fragments in a row, `order[from..to]`.
#[derive(Clone, Copy)]
struct Row {
    from: usize,
    to: usize,
    base: f32,
    size: f32,
    top: f32,
    bot: f32,
}

impl Row {
    fn count(&self) -> usize {
        self.to - self.from
    }

    /// The middle of the glyph boxes of the row: the box runs from 0.8 of the size over the baseline to 0.2 under it.
    fn middle(&self) -> f32 {
        self.base - 0.3 * self.size
    }
}

/// Sort pairs by the number, and then by the index.
fn sort_pairs(v: &mut [(f32, u32)]) {
    v.sort_unstable_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
}

/// The fragments of the text of a page (not of the lines whose middle is inside one of `taken`). `None`: more than `MAX_FRAGMENTS`.
fn fragments(lines: &[TextLine], taken: &[[f32; 4]]) -> Option<Vec<Frag>> {
    let mut out: Vec<Frag> = Vec::new();
    for line in lines {
        if line.vertical {
            continue;
        }
        let first = out.len();
        let leader = line.text.contains("……") || line.text.contains(".....");
        let mut cur: Option<Frag> = None;
        let mut prev_end = 0usize;
        for p in &line.pieces {
            let [l, t, r, b] = p.bbox;
            if !(l.is_finite() && t.is_finite() && r.is_finite() && b.is_finite() && p.size.is_finite() && p.size > 0.0) {
                continue;
            }
            // (A space or a tab is no ink: the gap is measured between the letters either side.)
            if line.text.get(p.start as usize..p.end as usize).is_none_or(|s| s.trim().is_empty()) {
                continue;
            }
            let blanks = |from: usize| line.text.get(from..p.end as usize).map_or(0, |t| t.matches(' ').count().min(1000)) as u16;
            let end = p.end as usize;
            let (digits, ink) = line.text.get(p.start as usize..end).map_or((0, 0), |t| {
                (t.chars().filter(char::is_ascii_digit).count().min(1000) as u16, t.chars().filter(|c| !c.is_whitespace()).count().min(1000) as u16)
            });
            match cur.as_mut() {
                Some(f) if l - f.a1 <= FRAGMENT_GAP * f.size.max(p.size) => {
                    f.spaces = f.spaces.saturating_add(blanks(prev_end));
                    f.digits = f.digits.saturating_add(digits);
                    f.ink = f.ink.saturating_add(ink);
                    f.a0 = f.a0.min(l);
                    f.a1 = f.a1.max(r);
                    f.top = f.top.min(t);
                    f.bot = f.bot.max(b);
                    f.size = f.size.max(p.size);
                }
                _ => {
                    if let Some(done) = cur.take() {
                        out.push(done);
                    }
                    cur = Some(Frag { a0: l, a1: r, base: b - 0.2 * p.size, top: t, bot: b, size: p.size, spaces: blanks(p.start as usize), digits, ink, leader });
                }
            }
            prev_end = end;
        }
        out.extend(cur);
        // A line in a ruled table belongs to that table.
        if let Some(mine) = out.get(first..).filter(|m| !m.is_empty()) {
            let x = 0.5 * (mine.iter().map(|f| f.a0).fold(f32::INFINITY, f32::min) + mine.iter().map(|f| f.a1).fold(f32::NEG_INFINITY, f32::max));
            let y = 0.5 * (mine.iter().map(|f| f.top).fold(f32::INFINITY, f32::min) + mine.iter().map(|f| f.bot).fold(f32::NEG_INFINITY, f32::max));
            if taken.iter().any(|b| x >= b[0] - 1.5 && x <= b[2] + 1.5 && y >= b[1] - 1.5 && y <= b[3] + 1.5) {
                out.truncate(first);
            }
        }
        if out.len() > MAX_FRAGMENTS {
            return None;
        }
    }
    Some(out)
}

/// The tables made of text only, top to bottom, and how many fragments were looked at (for the work meter). `lines` are the page's
/// lines; `taken` the boxes of the ruled tables, whose text is left to them. `numbers`: a table needs a column of numbers (a
/// table of words only is a diagram, a list or a layout, to a reader of a text document).
pub(crate) fn find(lines: &[TextLine], taken: &[[f32; 4]], numbers: bool) -> (Vec<Grid>, u64) {
    let Some(frags) = fragments(lines, taken) else { return (Vec::new(), MAX_FRAGMENTS as u64) };
    let looked = frags.len() as u64;
    if frags.len() < 6 {
        return (Vec::new(), looked);
    }
    // Rows: the fragments by baseline, cut where the baseline moves on by more than the tolerance, each row by position.
    let mut order: Vec<(f32, u32)> = frags.iter().enumerate().map(|(i, f)| (f.base, i as u32)).collect();
    sort_pairs(&mut order);
    let mut rows: Vec<Row> = Vec::new();
    for (k, &(_, i)) in order.iter().enumerate() {
        let Some(f) = frags.get(i as usize) else { continue };
        match rows.last_mut() {
            Some(r) if f.base - r.base <= ROW_TOLERANCE * r.size.max(f.size) => {
                r.to = k + 1;
                r.size = r.size.max(f.size);
                r.top = r.top.min(f.top);
                r.bot = r.bot.max(f.bot);
            }
            _ => rows.push(Row { from: k, to: k + 1, base: f.base, size: f.size, top: f.top, bot: f.bot }),
        }
    }
    for r in &rows {
        if r.count() > 1
            && let Some(slice) = order.get_mut(r.from..r.to)
        {
            slice.iter_mut().for_each(|e| e.0 = frags.get(e.1 as usize).map_or(0.0, |f| f.a0));
            sort_pairs(slice);
        }
    }
    let mut grids = Vec::new();
    let mut at = 0usize;
    while at < rows.len() && grids.len() < MAX_TABLES {
        if rows.get(at).is_none_or(|r| r.count() < 2) {
            at += 1;
            continue;
        }
        // The run of rows: ends at the last row of two fragments or more that is followed by too much blank space or by too many
        // rows of one.
        let (mut last, mut sparse, mut k) = (at, 0usize, at + 1);
        while let (Some(prev), Some(row)) = (rows.get(k - 1), rows.get(k)) {
            if row.top - prev.bot > ROW_BREAK * row.size.max(prev.size) || k - at >= MAX_ROWS {
                break;
            }
            if row.count() >= 2 {
                last = k;
                sparse = 0;
            } else {
                sparse += 1;
                if sparse > MAX_SPARSE {
                    break;
                }
            }
            k += 1;
        }
        match table(&frags, &order, &rows, at, last, numbers) {
            Some((grid, end)) => {
                grids.push(grid);
                at = end + 1;
            }
            None => at = last + 1,
        }
    }
    (grids, looked)
}

/// The table in rows `start..=last` (the first and last of two fragments or more), and the last row it takes.
fn table(frags: &[Frag], order: &[(f32, u32)], rows: &[Row], start: usize, last: usize, numbers: bool) -> Option<(Grid, usize)> {
    let run = rows.get(start..=last)?;
    let tabular = run.iter().filter(|r| r.count() >= 2).count();
    if tabular < 3 {
        return None;
    }
    let frag_of = |k: usize| order.get(k).and_then(|e| frags.get(e.1 as usize));
    // The columns: the strips of the page that at most one row in four covers, between strips that more do.
    let (mut events, mut size_sum, mut n) = (Vec::new(), 0.0f32, 0u32);
    for r in run.iter().filter(|r| r.count() >= 2) {
        for f in (r.from..r.to).filter_map(frag_of) {
            events.push((f.a0, 1u32));
            events.push((f.a1, 0u32));
            size_sum += f.size;
            n += 1;
        }
    }
    sort_pairs(&mut events);
    let min_gutter = (0.5 * size_sum / n.max(1) as f32).max(2.0);
    let allow = (tabular / 4) as i64;
    // Between two strips that more rows cover, a gap is where a column ends: at the middle of the widest part of it that the
    // fewest rows cover.
    let (mut covered, mut dense) = (0i64, false);
    let mut gap: Option<(f32, i64, f32, f32)> = None;
    let mut inner: Vec<f32> = Vec::new();
    for (i, &(pos, kind)) in events.iter().enumerate() {
        covered += if kind == 1 { 1 } else { -1 };
        if covered > allow {
            if let Some((from, _, _, middle)) = gap.take()
                && pos - from >= min_gutter
            {
                inner.push(middle);
            }
            dense = true;
        } else if dense {
            let next = events.get(i + 1).map_or(pos, |e| e.0);
            let (width, middle) = (next - pos, 0.5 * (pos + next));
            match gap.as_mut() {
                Some(best) if covered > best.1 || (covered == best.1 && width <= best.2) => {}
                Some(best) => (best.1, best.2, best.3) = (covered, width, middle),
                None => gap = Some((pos, covered, width, middle)),
            }
        }
    }
    let cols = inner.len() + 1;
    if !(2..=MAX_COLS).contains(&cols) {
        return None;
    }
    let col_of = |x: f32| inner.partition_point(|&e| e <= x);
    // The columns a fragment covers, first to last.
    let span_of = |f: &Frag| (col_of(f.a0), col_of(f.a1).max(col_of(f.a0)));
    // The columns the fragments of a row begin in (a bit for each).
    let mask_of = |r: &Row| (r.from..r.to).filter_map(frag_of).fold(0u64, |m, f| m | 1u64 << span_of(f).0);
    let spans = |r: &Row| (r.from..r.to).filter_map(frag_of).any(|f| span_of(f).0 != span_of(f).1);

    // A row with a piece of text under some of the cells of the row that opened the group (the row above it, or the first of a
    // folded cell), in no first column, at the spacing of the lines of one paragraph, goes on from that row.
    let goes_on = |parent: &Row, prev: &Row, row: &Row| {
        let (m, p) = (mask_of(row), mask_of(parent));
        m != 0 && m & 1 == 0 && m & !p == 0 && row.count() < parent.count() && !spans(row) && row.base - prev.base <= FOLD_PITCH * row.size
    };
    // The rows of the table, in groups of rows that are one; then the rows under the last that go on from it.
    let mut groups: Vec<(usize, usize)> = Vec::new();
    for k in start..=last {
        let Some(row) = rows.get(k) else { continue };
        match groups.last_mut() {
            Some(g) if rows.get(g.0).zip(rows.get(k - 1)).is_some_and(|(parent, prev)| goes_on(parent, prev, row)) => g.1 = k,
            _ => groups.push((k, k)),
        }
    }
    let mut end = last;
    while let (Some(prev), Some(next), Some(opener)) = (rows.get(end), rows.get(end + 1), groups.last().and_then(|g| rows.get(g.0))) {
        if next.top - prev.bot > ROW_BREAK * next.size.max(prev.size) || end + 1 - start >= MAX_ROWS || !goes_on(opener, prev, next) {
            break;
        }
        end += 1;
        if let Some(g) = groups.last_mut() {
            g.1 = end;
        }
    }

    // How the table looks: where it begins and ends, whether it is made of running text, how much of it is filled, and whether
    // the second of two columns is made of numbers.
    let (mut lo, mut hi, mut top, mut bot) = (f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::NEG_INFINITY);
    let (mut all, mut wordy, mut spread, mut filled, mut second, mut second_numbers) = (0usize, 0usize, 0usize, 0u32, 0usize, 0usize);
    // The fragments with a dot leader, and for each column the fragments in it alone and how many of them are numbers.
    let (mut leaders, mut by_column) = (0usize, vec![(0usize, 0usize); cols]);
    for &(first, finish) in &groups {
        let mut slots = 0u64;
        for r in rows.get(first..=finish)? {
            top = top.min(r.top);
            bot = bot.max(r.bot);
            for f in (r.from..r.to).filter_map(frag_of) {
                lo = lo.min(f.a0);
                hi = hi.max(f.a1);
                all += 1;
                wordy += usize::from(f.spaces >= WORDS || f.a1 - f.a0 >= LONG * f.size);
                leaders += usize::from(f.leader);
                let (c0, c1) = span_of(f);
                if c0 == c1
                    && let Some(n) = by_column.get_mut(c0)
                {
                    *n = (n.0 + 1, n.1 + usize::from(f.numeric()));
                }
                slots |= (u64::MAX >> (63 - c1)) & (u64::MAX << c0);
                if c0 == 1 {
                    second += 1;
                    second_numbers += usize::from(f.numeric());
                }
            }
            spread += usize::from(r.count() >= 2 && mask_of(r).count_ones() >= 2);
        }
        filled += slots.count_ones();
    }
    let cells = groups.len() * cols;
    let enough = cols >= 3 || second_numbers * 10 >= second * 7;
    if !(lo.is_finite() && hi.is_finite() && top.is_finite() && bot.is_finite())
        || wordy * 5 >= all * 2
        || leaders * 5 >= all
        || (numbers && !by_column.iter().any(|&(n, num)| n >= 2 && num * 2 >= n))
        || spread * 5 < tabular * 3
        || filled as usize * 2 < cells
        || !enough
        || !steady(groups.iter().filter_map(|g| rows.get(g.0)).map(|r| r.base))
        || !lined_up(&groups, rows, order, frags, &span_of)
    {
        return None;
    }
    if groups.len() < 2 || groups.len() > MAX_ROWS {
        return None;
    }
    let mut ys = Vec::with_capacity(groups.len() + 1);
    ys.push(top - 1.0);
    for w in groups.windows(2) {
        if let [a, b] = w
            && let (Some(a), Some(b)) = (rows.get(a.1), rows.get(b.0))
        {
            ys.push(0.5 * (a.middle() + b.middle()));
        }
    }
    ys.push(bot + 1.0);
    let mut xs = Vec::with_capacity(cols + 1);
    xs.push(lo - 1.0);
    xs.extend(inner.iter().copied());
    xs.push(hi + 1.0);

    // Merged cells: a fragment across more than one column.
    let (nr, nc) = (groups.len(), cols);
    let mut owner: Vec<(u16, u16)> = (0..nr * nc).map(|i| ((i / nc) as u16, (i % nc) as u16)).collect();
    for (g, &(first, finish)) in groups.iter().enumerate() {
        for r in rows.get(first..=finish).unwrap_or_default() {
            for f in (r.from..r.to).filter_map(frag_of) {
                let (c0, c1) = span_of(f);
                let head = owner.get(g * nc + c0).copied().unwrap_or((g as u16, c0 as u16));
                for c in c0 + 1..=c1 {
                    if let Some(slot) = owner.get_mut(g * nc + c) {
                        *slot = head;
                    }
                }
            }
        }
    }
    let bbox = [lo - 1.0, top - 1.0, hi + 1.0, bot + 1.0];
    Some((Grid { bbox, xs, ys, vline: vec![false; nr * (nc + 1)], hline: vec![false; (nr + 1) * nc], owner }, end))
}

/// Do the numbers (baselines of the rows of a table) follow each other at the same distance, for the most part?
fn steady(bases: impl Iterator<Item = f32>) -> bool {
    let bases: Vec<f32> = bases.collect();
    let mut steps: Vec<(f32, u32)> = bases.windows(2).filter_map(|w| if let [a, b] = w { Some((b - a, 0)) } else { None }).collect();
    sort_pairs(&mut steps);
    let Some(&(middle, _)) = steps.get(steps.len() / 2) else { return false };
    let regular = steps.iter().filter(|s| (s.0 - middle).abs() <= (0.12 * middle).max(1.5)).count();
    regular * 5 >= steps.len() * 3
}

/// Do the pieces of text in most of the columns begin at one place, or end at one, or have one middle (within a third of a font size)?
fn lined_up(groups: &[(usize, usize)], rows: &[Row], order: &[(f32, u32)], frags: &[Frag], span_of: &dyn Fn(&Frag) -> (usize, usize)) -> bool {
    // The pieces that are in one column, by column: [left, right, middle, size] of each.
    let mut by_column: Vec<(f32, u32)> = Vec::new();
    let mut edges: Vec<[f32; 4]> = Vec::new();
    for g in groups {
        for r in rows.get(g.0..=g.1).unwrap_or_default() {
            for f in (r.from..r.to).filter_map(|k| order.get(k).and_then(|e| frags.get(e.1 as usize))) {
                let (c0, c1) = span_of(f);
                if c0 == c1 {
                    by_column.push((c0 as f32, edges.len() as u32));
                    edges.push([f.a0, f.a1, 0.5 * (f.a0 + f.a1), f.size]);
                }
            }
        }
    }
    sort_pairs(&mut by_column);
    let (mut checked, mut good, mut from) = (0usize, 0usize, 0usize);
    while from < by_column.len() {
        let col = by_column.get(from).map_or(0.0, |c| c.0);
        let to = from + by_column.get(from..).unwrap_or_default().iter().take_while(|c| c.0 == col).count();
        let mine: Vec<&[f32; 4]> = by_column.get(from..to).unwrap_or_default().iter().filter_map(|c| edges.get(c.1 as usize)).collect();
        if mine.len() >= 3 {
            checked += 1;
            let mut best = 0usize;
            for k in 0..3 {
                let at = |e: &[f32; 4]| e.get(k).copied().unwrap_or(0.0);
                let mut v: Vec<(f32, u32)> = mine.iter().map(|e| (at(e), 0)).collect();
                sort_pairs(&mut v);
                let middle = v.get(v.len() / 2).map_or(0.0, |m| m.0);
                best = best.max(mine.iter().filter(|e| (at(e) - middle).abs() <= e.get(3).copied().unwrap_or(0.0) / 3.0).count());
            }
            good += usize::from(best * 10 >= mine.len() * 7);
        }
        from = to.max(from + 1);
    }
    checked > 0 && good * 5 >= checked * 3
}

#[cfg(test)]
mod tests {
    use super::*;
    use qingpdf_base::text::Piece;

    /// A line of text at baseline `y` made of the words `(left, right, text)`, size 10 (a box runs from y - 8 to y + 2).
    fn line(y: f32, words: &[(f32, f32, &str)]) -> TextLine {
        let mut text = String::new();
        let mut pieces = Vec::new();
        for (i, &(l, r, w)) in words.iter().enumerate() {
            if i > 0 {
                text.push(' ');
            }
            let start = text.len() as u32;
            text.push_str(w);
            pieces.push(Piece { start, end: text.len() as u32, bbox: [l, y - 8.0, r, y + 2.0], size: 10.0, style: 0 });
        }
        TextLine { text, pieces, vertical: false }
    }

    fn statement() -> Vec<TextLine> {
        let mut lines = vec![line(100.0, &[(50.0, 90.0, "Date"), (150.0, 190.0, "Memo"), (300.0, 350.0, "Amount"), (400.0, 450.0, "Balance")])];
        for i in 0..6 {
            let y = 114.0 + 14.0 * i as f32;
            lines.push(line(y, &[(50.0, 100.0, "2024-05-01"), (150.0, 200.0 + 5.0 * i as f32, "Salary"), (310.0, 350.0, "1,200.00"), (410.0, 450.0, "8,800.00")]));
        }
        lines
    }

    #[test]
    fn text_lined_up_in_columns_is_a_table() {
        let (grids, looked) = find(&statement(), &[], false);
        assert_eq!(grids.len(), 1);
        assert_eq!(looked, 28);
        let g = &grids[0];
        assert_eq!((g.rows(), g.cols()), (7, 4));
        assert_eq!(g.cell_at(60.0, 98.0), (0, 0));
        assert_eq!(g.cell_at(330.0, 128.0), (2, 2));
        assert_eq!(g.cell_at(430.0, 180.0), (6, 3));
        assert!(!g.v(1, 1) && !g.h(1, 1), "no lines");
    }

    #[test]
    fn a_heading_across_columns_is_a_merged_cell_and_a_folded_cell_joins_the_row_above() {
        let mut lines = statement();
        // Row 3 has a memo of two lines (the second 11 points under the first, at the spacing of the lines of a paragraph).
        lines.insert(4, line(137.0, &[(150.0, 200.0, "continued")]));
        let (grids, _) = find(&lines, &[], false);
        assert_eq!(grids.len(), 1);
        let g = &grids[0];
        assert_eq!(g.rows(), 7, "the second line of the memo is no row of its own");
        // The piece of the memo's second line is in the cell of the first.
        assert_eq!(g.cell_at(160.0, 134.0), g.cell_at(160.0, 126.0));
        // A row across the second and third columns:
        let mut lines = statement();
        lines[3] = line(142.0, &[(50.0, 100.0, "2024-05-01"), (150.0, 350.0, "Transfer to someone, with a long memo"), (410.0, 450.0, "8,800.00")]);
        let (grids, _) = find(&lines, &[], false);
        let g = &grids[0];
        assert_eq!((g.rows(), g.cols()), (7, 4));
        assert_eq!(g.owner(3, 2), (3, 1));
        assert_eq!(g.owner(3, 1), (3, 1));
        assert_eq!(g.owner(2, 2), (2, 2));
    }

    #[test]
    fn text_in_two_columns_and_lines_of_one_piece_are_no_table() {
        let mut lines = Vec::new();
        for i in 0..20 {
            let y = 100.0 + 12.0 * i as f32;
            lines.push(line(y, &[(50.0, 250.0, "the first column is full of text"), (280.0, 480.0, "so is the second, to its edges")]));
        }
        assert!(find(&lines, &[], false).0.is_empty());
        let lines: Vec<TextLine> = (0..20).map(|i| line(100.0 + 12.0 * i as f32, &[(50.0, 250.0, "a line of a paragraph")])).collect();
        assert!(find(&lines, &[], false).0.is_empty());
        // Words with ordinary spaces between them are one fragment however many lines.
        let lines: Vec<TextLine> = (0..8).map(|i| line(100.0 + 12.0 * i as f32, &[(50.0, 80.0, "one"), (83.0, 110.0, "two"), (113.0, 150.0, "three")])).collect();
        assert!(find(&lines, &[], false).0.is_empty());
    }

    #[test]
    fn rows_that_are_not_evenly_spaced_pieces_not_lined_up_and_scattered_text_are_no_table() {
        let fine = |lines: &[TextLine]| !find(lines, &[], false).0.is_empty();
        let rows = |ys: &[f32], x: &dyn Fn(usize) -> [f32; 3]| -> Vec<TextLine> {
            ys.iter().enumerate().map(|(i, &y)| line(y, &[(x(i)[0], x(i)[0] + 30.0, "1.5"), (x(i)[1], x(i)[1] + 30.0, "2.5"), (x(i)[2], x(i)[2] + 30.0, "3.5")])).collect()
        };
        let straight = |_: usize| [50.0, 150.0, 250.0];
        // Rows 14 points apart in a straight grid are a table...
        assert!(fine(&rows(&[100.0, 114.0, 128.0, 142.0], &straight)));
        // ...rows that are not (the lines of displayed equations, with their numbers)...
        assert!(!fine(&rows(&[100.0, 114.0, 160.0, 175.0], &straight)));
        // ...pieces of text that do not begin, end or centre at one place in a column (the labels of a drawing)...
        let ragged = |i: usize| [50.0 + 13.0 * i as f32, 150.0 + 7.0 * (i % 2) as f32 * 3.0, 250.0 + 9.0 * i as f32];
        assert!(!fine(&rows(&[100.0, 114.0, 128.0, 142.0, 156.0], &ragged)));
        // ...and a few labels in a big empty grid.
        let mut sparse = rows(&[100.0, 114.0, 128.0, 142.0, 156.0, 170.0], &straight);
        for (i, l) in sparse.iter_mut().enumerate() {
            if i % 3 != 0 {
                l.pieces.truncate(2);
            }
        }
        assert!(fine(&sparse), "two thirds filled is a table");
        let mut few: Vec<TextLine> = vec![line(100.0, &[(50.0, 80.0, "1.5"), (150.0, 180.0, "2.5"), (250.0, 280.0, "3.5")])];
        few.extend((1..6).map(|i| line(100.0 + 14.0 * i as f32, &[(if i % 2 == 0 { 50.0 } else { 250.0 }, 80.0 + if i % 2 == 0 { 0.0 } else { 200.0 }, "x1")])));
        assert!(!fine(&few));
    }

    #[test]
    fn two_columns_make_a_table_only_when_the_second_is_made_of_numbers() {
        let pairs = |second: &str| -> Vec<TextLine> { (0..6).map(|i| line(100.0 + 14.0 * i as f32, &[(50.0, 100.0, "Cash"), (300.0, 350.0, second)])).collect() };
        assert!(find(&pairs("Yes"), &[], false).0.is_empty());
        assert_eq!(find(&pairs("1,200.00"), &[], false).0.len(), 1);
    }

    #[test]
    fn a_list_with_dot_leaders_is_no_table_and_for_a_text_document_a_table_needs_a_column_of_numbers() {
        // A table of contents: a title, dots, a page number.
        let contents = |dots: &str| -> Vec<TextLine> {
            (0..8).map(|i| line(100.0 + 14.0 * i as f32, &[(50.0, 150.0, "Chapter"), (170.0, 300.0, dots), (400.0, 420.0, &format!("{}", 5 + i))])).collect()
        };
        assert_eq!(find(&contents("5"), &[], false).0.len(), 1, "the same rows with no leader are a table");
        assert!(find(&contents("........."), &[], false).0.is_empty());
        assert!(find(&contents("………………"), &[], false).0.is_empty());
        // A stray leader in one row of a long table does not matter.
        let mut lines = statement();
        lines[3] = line(142.0, &[(50.0, 100.0, "2024-05-01"), (150.0, 200.0, "Memo......."), (310.0, 350.0, "1,200.00"), (410.0, 450.0, "8,800.00")]);
        assert_eq!(find(&lines, &[], false).0.len(), 1);
        // Columns of words only (a diagram's boxes) are a table for a spreadsheet, not for a Word document.
        let words: Vec<TextLine> = (0..6).map(|i| line(100.0 + 14.0 * i as f32, &[(50.0, 100.0, "Alpha"), (150.0, 200.0, "Beta"), (300.0, 350.0, "Gamma")])).collect();
        assert_eq!(find(&words, &[], false).0.len(), 1);
        assert!(find(&words, &[], true).0.is_empty());
        assert_eq!(find(&statement(), &[], true).0.len(), 1);
    }

    #[test]
    fn a_table_with_lines_round_it_is_left_to_the_ruled_finder() {
        let (grids, _) = find(&statement(), &[[40.0, 90.0, 470.0, 200.0]], false);
        assert!(grids.is_empty());
    }

    #[test]
    fn a_page_of_a_hundred_thousand_lines_is_looked_at_in_no_time() {
        // Two short fragments to a line, 50 rows across 2000 columns of text (a hundred thousand lines).
        let mut lines = Vec::with_capacity(100_000);
        for i in 0..100_000 {
            let y = 20.0 + 12.0 * (i % 1000) as f32;
            let x = 10.0 + 60.0 * (i / 1000) as f32;
            lines.push(line(y, &[(x, x + 20.0, "ab"), (x + 30.0, x + 50.0, "cd")]));
        }
        let t = std::time::Instant::now();
        let (grids, looked) = find(&lines, &[], false);
        assert_eq!(looked, 200_000);
        assert!(grids.len() <= MAX_TABLES);
        assert!(t.elapsed() < std::time::Duration::from_secs(3), "{:?}", t.elapsed());
    }
}
