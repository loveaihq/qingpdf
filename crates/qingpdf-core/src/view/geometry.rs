//! Boxes of characters, for the window's selection and highlights: which character is under the pointer, and the
//! rectangles that cover a run of characters (one for each line). Plain functions on the boxes the engine tells
//! (`[left, top, right, bottom]`, see [`crate::text::PageChars`]), so that every window gets the same answers.

/// A box with no area: the space between two pieces of text, the end of a line.
pub fn is_blank(b: &[f32; 4]) -> bool {
    !(b[2] > b[0] && b[3] > b[1])
}

/// Is `b` the next character after `prev` in the same line (a horizontal line: they overlap across, and are close
/// along it) or the same column (a vertical one)?
fn continues(prev: &[f32; 4], b: &[f32; 4]) -> bool {
    let (w1, h1, w2, h2) = (prev[2] - prev[0], prev[3] - prev[1], b[2] - b[0], b[3] - b[1]);
    let overlap_y = prev[3].min(b[3]) - prev[1].max(b[1]);
    let overlap_x = prev[2].min(b[2]) - prev[0].max(b[0]);
    if overlap_y > 0.5 * h1.min(h2) {
        let gap = prev[0].max(b[0]) - prev[2].min(b[2]);
        gap < 3.0 * h1.max(h2)
    } else if overlap_x > 0.5 * w1.min(w2) {
        let gap = prev[1].max(b[1]) - prev[3].min(b[3]);
        gap < 3.0 * w1.max(w2)
    } else {
        false
    }
}

/// The rectangles that cover a run of characters, given their boxes in reading order: the boxes of neighbours in one line
/// (or one column) are joined, so a selected paragraph is one rectangle for each line. Blank boxes are skipped over.
pub fn line_rects(boxes: &[[f32; 4]]) -> Vec<[f32; 4]> {
    let mut out: Vec<[f32; 4]> = Vec::new();
    let mut current: Option<[f32; 4]> = None;
    let mut last = [0.0f32; 4];
    for b in boxes {
        if is_blank(b) || b.iter().any(|v| !v.is_finite()) {
            continue;
        }
        current = match current {
            Some(c) if continues(&last, b) => Some([c[0].min(b[0]), c[1].min(b[1]), c[2].max(b[2]), c[3].max(b[3])]),
            Some(c) => {
                out.push(c);
                Some(*b)
            }
            None => Some(*b),
        };
        last = *b;
    }
    out.extend(current);
    out
}

/// The character nearest to the point (`x`, `y`): the one whose box holds it, else the one with the closest box (the
/// first of equals). `None` when no character has a box.
pub fn nearest_char(boxes: &[[f32; 4]], x: f32, y: f32) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (i, b) in boxes.iter().enumerate() {
        if is_blank(b) || b.iter().any(|v| !v.is_finite()) {
            continue;
        }
        let dx = (b[0] - x).max(0.0).max(x - b[2]);
        let dy = (b[1] - y).max(0.0).max(y - b[3]);
        let d = dx * dx + dy * dy;
        if best.is_none_or(|(_, bd)| d < bd) {
            best = Some((i, d));
            if d == 0.0 {
                break;
            }
        }
    }
    best.map(|(i, _)| i)
}

/// A run of neighbouring characters (a line, a column): the characters `start..end` of the page and the box that covers them.
#[derive(Clone, Copy, Debug)]
struct Run {
    start: u32,
    end: u32,
    bounds: [f32; 4],
}

/// The boxes of a page cut into runs of neighbours, so that the character nearest to a point is found by looking at the
/// runs and then only at the characters of the runs that could hold it (a few lines of a page) instead of at every
/// character of the page. It gives the same answer as [`nearest_char`] on the same boxes.
#[derive(Clone, Debug, Default)]
pub struct CharIndex {
    runs: Vec<Run>,
}

/// The distance squared from a point to a box (0 inside).
fn distance2(b: &[f32; 4], x: f32, y: f32) -> f32 {
    let dx = (b[0] - x).max(0.0).max(x - b[2]);
    let dy = (b[1] - y).max(0.0).max(y - b[3]);
    dx * dx + dy * dy
}

impl CharIndex {
    /// The runs of `boxes` (the boxes as the engine tells them).
    pub fn new(boxes: &[[f32; 4]]) -> CharIndex {
        let mut runs: Vec<Run> = Vec::new();
        let mut last: Option<[f32; 4]> = None;
        for (i, b) in boxes.iter().enumerate() {
            if is_blank(b) || b.iter().any(|v| !v.is_finite()) {
                continue;
            }
            let i = u32::try_from(i).unwrap_or(u32::MAX);
            match (runs.last_mut(), last) {
                (Some(run), Some(prev)) if continues(&prev, b) => {
                    run.end = i.saturating_add(1);
                    run.bounds = [run.bounds[0].min(b[0]), run.bounds[1].min(b[1]), run.bounds[2].max(b[2]), run.bounds[3].max(b[3])];
                }
                _ => runs.push(Run { start: i, end: i.saturating_add(1), bounds: *b }),
            }
            last = Some(*b);
        }
        CharIndex { runs }
    }

    /// What the index takes in memory, in bytes.
    pub fn bytes(&self) -> usize {
        self.runs.len() * std::mem::size_of::<Run>()
    }

    /// [`nearest_char`] of `boxes`, which must be the boxes the index was made from. A run that is farther away than the best
    /// found so far cannot hold a nearer character (its box covers all of them), so it is not looked into; the runs come in
    /// the order of the characters, so of equal distances the first character still wins.
    pub fn nearest(&self, boxes: &[[f32; 4]], x: f32, y: f32) -> Option<usize> {
        let mut best: Option<(usize, f32)> = None;
        for run in &self.runs {
            if best.is_some_and(|(_, bd)| distance2(&run.bounds, x, y) >= bd) {
                continue;
            }
            for (i, b) in boxes.get(run.start as usize..run.end as usize).unwrap_or(&[]).iter().enumerate() {
                if is_blank(b) || b.iter().any(|v| !v.is_finite()) {
                    continue;
                }
                let d = distance2(b, x, y);
                if best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((run.start as usize + i, d));
                    if d == 0.0 {
                        return best.map(|(i, _)| i);
                    }
                }
            }
        }
        best.map(|(i, _)| i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(n: usize, top: f32) -> Vec<[f32; 4]> {
        (0..n).map(|i| [10.0 * i as f32, top, 10.0 * i as f32 + 10.0, top + 12.0]).collect()
    }

    #[test]
    fn neighbours_in_a_line_are_one_rectangle_and_lines_are_apart() {
        let mut boxes = row(5, 0.0);
        boxes.push([0.0; 4]); // the line end
        boxes.extend(row(3, 20.0));
        assert_eq!(line_rects(&boxes), vec![[0.0, 0.0, 50.0, 12.0], [0.0, 20.0, 30.0, 32.0]]);
        assert!(line_rects(&[]).is_empty());
        assert!(line_rects(&[[0.0; 4], [5.0, 5.0, 5.0, 9.0]]).is_empty());
    }

    #[test]
    fn a_line_with_a_blank_between_stays_whole_but_a_far_piece_does_not() {
        let mut boxes = row(2, 0.0);
        boxes.push([0.0; 4]);
        boxes.push([30.0, 0.0, 40.0, 12.0]);
        assert_eq!(line_rects(&boxes), vec![[0.0, 0.0, 40.0, 12.0]]);
        let far = vec![[0.0, 0.0, 10.0, 12.0], [200.0, 0.0, 210.0, 12.0]];
        assert_eq!(line_rects(&far).len(), 2);
    }

    #[test]
    fn a_vertical_column_is_one_rectangle() {
        let col: Vec<[f32; 4]> = (0..4).map(|i| [100.0, 14.0 * i as f32, 112.0, 14.0 * i as f32 + 12.0]).collect();
        assert_eq!(line_rects(&col), vec![[100.0, 0.0, 112.0, 54.0]]);
    }

    #[test]
    fn the_nearest_character_is_the_one_under_the_pointer_or_the_closest() {
        let mut boxes = row(3, 0.0);
        boxes.push([0.0; 4]);
        boxes.extend(row(3, 20.0));
        assert_eq!(nearest_char(&boxes, 15.0, 5.0), Some(1));
        assert_eq!(nearest_char(&boxes, 25.0, 25.0), Some(6));
        // Beyond the end of a line: the last character of it; in the margin below: the closest line.
        assert_eq!(nearest_char(&boxes, 500.0, 6.0), Some(2));
        assert_eq!(nearest_char(&boxes, 5.0, 300.0), Some(4));
        assert_eq!(nearest_char(&[[0.0; 4]], 1.0, 1.0), None);
        assert_eq!(nearest_char(&[], 1.0, 1.0), None);
    }

    #[test]
    fn the_index_gives_the_same_answers_as_looking_at_every_character() {
        // A page of lines and a column, with blanks, a box that is not finite and a stray box, and points all over and beyond it.
        let mut boxes: Vec<[f32; 4]> = Vec::new();
        for line in 0..30 {
            boxes.extend(row(40, 20.0 * line as f32));
            boxes.push([0.0; 4]);
        }
        boxes.push([f32::NAN, 0.0, 1.0, 1.0]);
        boxes.extend((0..10).map(|i| [500.0, 14.0 * i as f32, 512.0, 14.0 * i as f32 + 12.0]));
        boxes.push([100.0, 100.0, 103.0, 104.0]);
        let index = CharIndex::new(&boxes);
        assert!(index.runs.len() < 50, "{}", index.runs.len());
        let mut seed = 12345u32;
        let mut next = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32
        };
        for _ in 0..2_000 {
            let (x, y) = (next() * 700.0 - 100.0, next() * 700.0 - 50.0);
            assert_eq!(index.nearest(&boxes, x, y), nearest_char(&boxes, x, y), "({x}, {y})");
        }
        // Points on the edge between two characters: the first of the equals.
        assert_eq!(index.nearest(&boxes, 10.0, 5.0), nearest_char(&boxes, 10.0, 5.0));
        assert_eq!(CharIndex::new(&[]).nearest(&[], 1.0, 1.0), None);
        assert_eq!(CharIndex::new(&[[0.0; 4]]).nearest(&[[0.0; 4]], 1.0, 1.0), None);
    }
}
