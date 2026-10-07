//! From characters with positions to lines of text.
//!
//! Lines are built in the order the characters were drawn: a character joins the
//! current line when its baseline is (nearly) the line's and it runs the same way;
//! otherwise it starts a new line. Within a line the pieces drawn one after the
//! other are put in position order (pieces drawn right to left, or the second half
//! of a line drawn first, come out right). Vertical text (writing mode 1) runs
//! down the page: a column is a line, and columns come in drawing order, which for
//! the books we have is right to left. Characters turned by the text matrix get
//! lines of their own along their own direction, and a few horizontal characters
//! inside a vertical column (digits, Latin words) stay in the column.
//!
//! Spaces: a character that starts more than 0.15 of the font size after the end
//! of the previous one gets a space before it (0.8 when both are CJK, where
//! justified lines open up without meaning gaps).

use super::interp::Glyph;

/// Gap (in font sizes) from which a space is inserted, and the same when both neighbours are wide.
const SPACE_GAP: f64 = 0.15;
const SPACE_GAP_WIDE: f64 = 0.8;
/// How far (in font sizes) a character's baseline may be off the line's and still belong to it.
const BASELINE_TOLERANCE: f64 = 0.5;
/// How many glyphs after one (in position order) are looked at for a repeat of it.
const REPEAT_WINDOW: usize = 16;
/// Directions closer than this (cosine) are the same direction.
const PARALLEL: f64 = 0.98;

struct Line {
    end: usize,
    /// First glyph of every run (a piece drawn in one go from left to right).
    runs: Vec<usize>,
    dir: (f64, f64),
    /// Position of the first glyph across the writing direction.
    base: f64,
    size: f64,
}

fn dot(a: (f64, f64), b: (f64, f64)) -> f64 {
    a.0 * b.0 + a.1 * b.1
}

fn is_wide(c: char) -> bool {
    matches!(u32::from(c), 0x2E80..=0xA4CF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFFEF | 0x20000..=0x2FFFF)
}

fn dir_of(g: &Glyph) -> (f64, f64) {
    (g.dx, g.dy)
}

/// How far `g` starts after the end of `prev`, along `prev`'s direction.
fn gap_after(prev: &Glyph, g: &Glyph) -> f64 {
    dot((g.x - prev.x, g.y - prev.y), dir_of(prev)) - prev.adv
}

fn new_line(i: usize, g: &Glyph) -> Line {
    let dir = dir_of(g);
    Line { end: i + 1, runs: vec![i], dir, base: dot((g.x, g.y), (-dir.1, dir.0)), size: g.size }
}

/// Does `g` (which follows `prev`, the last glyph of `line`) belong to `line`? And does it start a new run?
fn join(line: &Line, prev: &Glyph, g: &Glyph) -> Option<bool> {
    let normal = (-line.dir.1, line.dir.0);
    let cos = dot(dir_of(g), line.dir);
    let perp = dot((g.x, g.y), normal) - line.base;
    let belongs = if cos > PARALLEL {
        let tol = (BASELINE_TOLERANCE * line.size.min(1.6 * g.size)).max(0.05);
        perp.abs() <= tol
    } else if cos.abs() < 0.3 && line.dir.1.abs() > 0.9 {
        // Horizontal characters inside a vertical column: close to the column and next in line.
        let along = dot((g.x, g.y), line.dir);
        let end = dot((prev.x, prev.y), line.dir) + prev.adv * dot(dir_of(prev), line.dir);
        perp.abs() <= line.size.max(g.size) && along >= end - 0.6 * line.size && along <= end + 2.5 * line.size
    } else {
        false
    };
    if !belongs {
        return None;
    }
    let continues = dot(dir_of(g), dir_of(prev)) > PARALLEL && gap_after(prev, g) >= -0.5 * g.size.max(prev.size).max(0.01);
    Some(!continues)
}

fn build_lines(glyphs: &[Glyph]) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    for (i, g) in glyphs.iter().enumerate() {
        let prev = i.checked_sub(1).and_then(|p| glyphs.get(p));
        if let (Some(line), Some(prev)) = (lines.last_mut(), prev)
            && let Some(new_run) = join(line, prev, g)
        {
            line.end = i + 1;
            if new_run {
                line.runs.push(i);
            }
            continue;
        }
        lines.push(new_line(i, g));
    }
    lines
}

/// Fake bold: the same character drawn again at (almost) the same place. Returns which glyphs
/// of the line are such repeats (the later drawn one of two is the repeat), or `None` when
/// the line has no place where one glyph starts before the previous one has ended, which
/// repeats need.
fn repeats(glyphs: &[Glyph], line: &Line, order: &[usize]) -> Option<Vec<bool>> {
    let overlaps = order.windows(2).any(|w| match w {
        [a, b] => match (glyphs.get(*a), glyphs.get(*b)) {
            (Some(p), Some(g)) => gap_after(p, g) < -0.12 * p.size.max(g.size),
            _ => false,
        },
        _ => false,
    });
    if !overlaps {
        return None;
    }
    let normal = (-line.dir.1, line.dir.0);
    // (along, across, glyph index) of the glyphs that have a character, sorted by along
    let mut pts: Vec<(f64, f64, usize)> = order
        .iter()
        .filter_map(|&i| {
            let g = glyphs.get(i)?;
            let c = g.uni.first()?;
            (!c.is_whitespace()).then(|| (dot((g.x, g.y), line.dir), dot((g.x, g.y), normal), i))
        })
        .collect();
    pts.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut gone = vec![false; glyphs.len().min(line.end)];
    let mut any = false;
    for (k, &(along, across, i)) in pts.iter().enumerate() {
        let Some(g) = glyphs.get(i) else { continue };
        // A repeat sits right after the glyph it repeats in this order; looking further than a few
        // places would make a line of a million glyphs on one spot a quadratic job.
        for &(along2, across2, j) in pts.iter().skip(k + 1).take(REPEAT_WINDOW) {
            let Some(h) = glyphs.get(j) else { continue };
            let tol = 0.12 * g.size.max(h.size).max(0.1);
            if along2 - along > tol {
                break;
            }
            if (across2 - across).abs() <= tol && g.uni == h.uni {
                let later = i.max(j);
                if let Some(slot) = gone.get_mut(later) {
                    any = true;
                    *slot = true;
                }
            }
        }
    }
    any.then_some(gone)
}

/// The text of one line: runs in position order, spaces where the gaps are wide.
fn line_text(glyphs: &[Glyph], line: &Line, out: &mut String) {
    let mut runs: Vec<(f64, usize, usize)> = Vec::with_capacity(line.runs.len());
    for (k, &start) in line.runs.iter().enumerate() {
        let end = line.runs.get(k + 1).copied().unwrap_or(line.end);
        let along = glyphs.get(start).map_or(0.0, |g| dot((g.x, g.y), line.dir));
        runs.push((along, start, end));
    }
    let slack = 0.5 * line.size;
    if runs.windows(2).any(|w| matches!(w, [a, b] if b.0 < a.0 - slack)) {
        runs.sort_by(|a, b| a.0.total_cmp(&b.0));
    }
    let begin = out.len();
    let mut prev: Option<&Glyph> = None;
    let order: Vec<usize> = runs.iter().flat_map(|&(_, start, end)| start..end).collect();
    let gone = repeats(glyphs, line, &order);
    for &i in &order {
        let Some(g) = glyphs.get(i) else { continue };
        if gone.as_ref().is_some_and(|gone| gone.get(i).copied().unwrap_or(false)) {
            continue;
        }
        if let Some(p) = prev
            && let (Some(a), Some(b)) = (p.uni.first(), g.uni.first())
            && !a.is_whitespace()
            && !b.is_whitespace()
            && dot(dir_of(p), dir_of(g)) > PARALLEL
            && !out.ends_with(' ')
        {
            let threshold = if is_wide(a) && is_wide(b) { SPACE_GAP_WIDE } else { SPACE_GAP };
            if gap_after(p, g) > threshold * 0.5 * (p.size + g.size) {
                out.push(' ');
            }
        }
        g.uni.push_to(out);
        prev = Some(g);
    }
    let written = out.get(begin..).unwrap_or("");
    let leading = written.len() - written.trim_start_matches(' ').len();
    let kept = written.trim_matches(' ').len();
    if kept == 0 {
        out.truncate(begin);
        return;
    }
    out.truncate(begin + leading + kept);
    out.drain(begin..begin + leading);
    out.push('\n');
}

/// All the text of a page, one line per `\n`.
pub(crate) fn render(glyphs: &[Glyph]) -> String {
    let mut out = String::new();
    for line in build_lines(glyphs) {
        line_text(glyphs, &line, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::cmap::Uni;

    fn g(c: char, x: f64, y: f64) -> Glyph {
        Glyph { uni: Uni::One(c), x, y, dx: 1.0, dy: 0.0, adv: 10.0, size: 10.0 }
    }

    fn v(c: char, x: f64, y: f64) -> Glyph {
        Glyph { uni: Uni::One(c), x, y, dx: 0.0, dy: -1.0, adv: 10.0, size: 10.0 }
    }

    fn word(s: &str, x: f64, y: f64) -> Vec<Glyph> {
        s.chars().enumerate().map(|(i, c)| g(c, x + 10.0 * i as f64, y)).collect()
    }

    #[test]
    fn lines_and_spaces() {
        let mut gl = word("ab", 0.0, 100.0);
        gl.extend(word("cd", 25.0, 100.0)); // 5 units after "ab" ends: a word space
        gl.extend(word("ef", 0.0, 86.0)); // next line
        assert_eq!(render(&gl), "ab cd\nef\n");
    }

    #[test]
    fn tight_kerning_makes_no_space() {
        let mut gl = word("ab", 0.0, 0.0);
        gl.extend(word("cd", 19.0, 0.0)); // 1 unit back
        gl.extend(word("e", 31.0, 0.0)); // 1 unit gap: under 0.15 of the size
        assert_eq!(render(&gl), "abcde\n");
    }

    #[test]
    fn a_piece_drawn_first_but_placed_second_is_put_in_position_order() {
        let mut gl = word("world", 60.0, 0.0);
        gl.extend(word("hello", 0.0, 0.0));
        assert_eq!(render(&gl), "hello world\n");
    }

    #[test]
    fn superscripts_stay_in_the_line() {
        let mut gl = word("E=mc", 0.0, 0.0);
        let mut sup = g('2', 40.0, 4.0);
        sup.size = 7.0;
        gl.push(sup);
        assert_eq!(render(&gl), "E=mc2\n");
    }

    #[test]
    fn vertical_columns() {
        let col1: Vec<Glyph> = "床前明".chars().enumerate().map(|(i, c)| v(c, 100.0, 200.0 - 10.0 * i as f64)).collect();
        let col2: Vec<Glyph> = "举头望".chars().enumerate().map(|(i, c)| v(c, 80.0, 200.0 - 10.0 * i as f64)).collect();
        let mut gl = col1;
        gl.extend(col2);
        assert_eq!(render(&gl), "床前明\n举头望\n");
    }

    #[test]
    fn wide_characters_keep_together_unless_far_apart() {
        let mut gl = vec![g('中', 0.0, 0.0), g('文', 14.0, 0.0)]; // 4 units gap: justified
        gl.push(g('图', 50.0, 0.0)); // 26 units gap: a real gap
        assert_eq!(render(&gl), "中文 图\n");
    }

    #[test]
    fn horizontal_digits_inside_a_vertical_column() {
        let mut gl = vec![v('一', 100.0, 200.0), v('二', 100.0, 190.0)];
        gl.push(g('1', 96.0, 178.0));
        gl.push(g('2', 104.0, 178.0));
        gl.push(v('三', 100.0, 166.0));
        assert_eq!(render(&gl), "一二12三\n");
    }

    #[test]
    fn turned_text_is_a_line_of_its_own() {
        let mut gl = word("ab", 0.0, 0.0);
        let mut side: Vec<Glyph> = "xy".chars().enumerate().map(|(i, c)| g(c, 300.0, 10.0 * i as f64)).collect();
        for s in &mut side {
            s.dx = 0.0;
            s.dy = 1.0;
        }
        gl.extend(side);
        gl.extend(word("cd", 0.0, -14.0));
        assert_eq!(render(&gl), "ab\nxy\ncd\n");
    }
}
