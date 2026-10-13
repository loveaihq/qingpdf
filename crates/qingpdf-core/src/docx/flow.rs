//! Layout analysis (5b): the lines, images and rules of a page (`PageLayout`) into what a word processor has: paragraphs, tables
//! and pictures in reading order.
//!
//! Everything is worked out in the page's *writing frame*: `a` runs along a line in the direction it is read, `c` across the
//! lines in the direction they follow each other. For a horizontal page that is `a = x`, `c = y`; for a page of vertical text it
//! is `a = y` and `c = -x` (columns follow each other from right to left). So one set of rules serves both.
//!
//! * Reading order (`order`): cut the page where there is a free strip. A strip across the writing direction wide enough for a
//!   column gutter and free of every line and image is a gutter between columns; if there is none, cut along the widest free
//!   strip in the other direction. The pieces are read one after the other.
//! * Paragraphs (`paragraphs`): consecutive lines of one piece are one paragraph unless the line spacing, size, weight,
//!   indentation or the shortness of the line before says a new one starts. Hyphenated words are joined.
//! * Lists (`mark_of`): a bullet or a number at the start of a paragraph.
//! * Tables: rules (`table.rs`), and the text in each cell by the position of each character.
//! * Headings, page headers, footers and page numbers are decided over the whole document (`mod.rs`).

use qingpdf_base::text::font::STYLE_HEAVY;
use qingpdf_base::text::{PageLayout, Piece, TextLine};

use super::table::{self, Grid};

/// The bits of a font's style that make a run differ from the one beside it (bold, italic, family, kind of Chinese face).
const STYLE_MASK: u8 = 0x3F;
/// The narrowest strip between two columns, points.
const MIN_GUTTER: f32 = 7.0;
/// The deepest the cutting of a page goes.
const MAX_DEPTH: u32 = 24;
/// How far (points) into a table's box the centre of a line may be and still be in the table.
const TOL_IN: f32 = 1.5;

#[derive(Clone, Debug)]
pub(super) struct Run {
    pub text: String,
    pub size: f32,
    pub style: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Align {
    Left,
    Center,
    Right,
    Both,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NumFmt {
    /// `1.`
    Dot,
    /// `1)`
    Paren,
    /// `(1)`
    Round,
    /// The same with the full-width punctuation of a Chinese document: `1．` `1）` `（1）` (kept as they are, so that no character
    /// of the text is changed into another).
    DotWide,
    ParenWide,
    RoundWide,
}

/// The bullet or the number a list item began with (it is taken out of the text, Word writes it again).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mark {
    Bullet(char),
    Number(u32, NumFmt),
}

/// A rectangle in the writing frame: `[a0, c0, a1, c1]`.
pub(super) type Rect = [f32; 4];

#[derive(Clone, Debug)]
pub(super) struct Para {
    pub runs: Vec<Run>,
    /// The size most of the text has, and whether most of it is heavy (bold, or a Chinese Hei face).
    pub size: f32,
    pub heavy: bool,
    pub lines: u32,
    pub chars: u32,
    pub align: Align,
    /// Points in from the left edge of the text column, of the paragraph and (added) of its first line (negative: hanging).
    pub left: f32,
    pub first: f32,
    /// Points of space before the paragraph (from the gap to the one before).
    pub before: f32,
    /// The distance from line to line inside the paragraph (points).
    pub pitch: f32,
    pub mark: Option<Mark>,
    /// 0 body, 1 where a page header goes, 2 where a footer goes.
    pub band: u8,
    /// Where it is in the writing frame.
    pub a0: f32,
    pub a1: f32,
    pub c0: f32,
    pub c1: f32,
    /// Heading level (1 to 4), 0 for none; set over the whole document.
    pub level: u8,
    /// The last line runs to the right edge of the column (so the paragraph may go on, on the next page).
    pub reaches: bool,
}

/// Does paragraph `b`, which comes where a column or a page ends, go on with `a`? Yes when `a` filled its last line, did not end a
/// sentence, and `b` is in the same size and weight without an indent or a bullet.
pub(super) fn continues(a: &Para, b: &Para) -> bool {
    let end = a.runs.iter().rev().find_map(|r| r.text.trim_end().chars().next_back()).unwrap_or(' ');
    a.reaches
        && a.band == 0
        && b.band == 0
        && a.mark.is_none()
        && b.mark.is_none()
        && (a.size - b.size).abs() < 0.5
        && a.heavy == b.heavy
        && b.first.abs() < 0.5 * b.size
        && b.left == 0.0
        && !"。！？.!?".contains(end)
}

impl Para {
    fn scale(&mut self, k: f32) {
        for r in &mut self.runs {
            r.size *= k;
        }
        for v in [&mut self.size, &mut self.left, &mut self.first, &mut self.before, &mut self.pitch, &mut self.a0, &mut self.a1, &mut self.c0, &mut self.c1] {
            *v *= k;
        }
    }

    /// The paragraph goes on with `next` (the first paragraph of the next page): its text is added.
    pub(super) fn append(&mut self, next: Para) {
        join(&mut self.runs, next.runs);
        self.lines += next.lines;
        self.chars += next.chars;
        self.a0 = self.a0.min(next.a0);
        self.a1 = self.a1.max(next.a1);
        self.reaches = next.reaches;
    }
}

#[derive(Clone, Debug)]
pub(super) struct Cell {
    /// Columns the cell spans; `vmerge`: 0 a cell of its own, 1 the first of cells merged downwards, 2 one of the others.
    pub span: u16,
    pub vmerge: u8,
    /// Which of its edges have a line: top, left, bottom, right.
    pub edges: [bool; 4],
    pub paras: Vec<Para>,
}

#[derive(Clone, Debug)]
pub(super) struct Table {
    /// Column widths in points.
    pub cols: Vec<f32>,
    pub rows: Vec<Vec<Cell>>,
    pub rect: Rect,
}

#[derive(Clone, Debug)]
pub(super) struct Pic {
    /// The number of the image file (`word/media/image{n}`) and its extension.
    pub index: usize,
    pub ext: &'static str,
    /// Size on the page, points.
    pub w: f32,
    pub h: f32,
    pub flip_h: bool,
    pub flip_v: bool,
    pub align: Align,
    pub rect: Rect,
}

#[derive(Clone, Debug)]
pub(super) enum Block {
    Para(Para),
    Table(Table),
    Pic(Pic),
}

impl Block {
    /// Where it is on the page, in the writing frame.
    pub(super) fn rect(&self) -> Rect {
        match self {
            Block::Para(p) => [p.a0, p.c0, p.a1, p.c1],
            Block::Table(t) => t.rect,
            Block::Pic(p) => p.rect,
        }
    }
}

/// One page, worked out.
#[derive(Debug)]
pub(super) struct PageOut {
    pub w: f32,
    pub h: f32,
    pub vertical: bool,
    pub blocks: Vec<Block>,
    /// How far down the page the text reaches (0 to 1); 0 for a page that is a picture, so that the next page starts afresh.
    pub filled: f32,
    /// Pairs of ruled lines looked at for a crossing (the work meter is charged for them).
    pub crossings: u64,
}

impl PageOut {
    /// The whole page made `k` times as big: its size, every position, indent, space and width, the sizes of the text and the
    /// pictures (the page goes into a Word page no bigger than Word's, and looks the same on it).
    pub(super) fn scale(&mut self, k: f32) {
        self.w *= k;
        self.h *= k;
        let rect = |r: &mut Rect| r.iter_mut().for_each(|v| *v *= k);
        for b in &mut self.blocks {
            match b {
                Block::Para(p) => p.scale(k),
                Block::Table(t) => {
                    t.cols.iter_mut().for_each(|c| *c *= k);
                    rect(&mut t.rect);
                    t.rows.iter_mut().flatten().flat_map(|c| &mut c.paras).for_each(|p| p.scale(k));
                }
                Block::Pic(p) => {
                    p.w *= k;
                    p.h *= k;
                    rect(&mut p.rect);
                }
            }
        }
    }

    /// How many runs of text the page has (what makes the file big, besides the letters).
    pub(super) fn run_count(&self) -> usize {
        let of = |p: &Para| p.runs.len();
        self.blocks
            .iter()
            .map(|b| match b {
                Block::Para(p) => of(p),
                Block::Table(t) => t.rows.iter().flatten().flat_map(|c| &c.paras).map(of).sum(),
                Block::Pic(_) => 0,
            })
            .sum()
    }
}

/// A line of text in the writing frame.
#[derive(Clone, Debug)]
struct Ln {
    vertical: bool,
    text: String,
    pieces: Vec<Piece>,
    a0: f32,
    a1: f32,
    c0: f32,
    c1: f32,
    size: f32,
    heavy: f32,
    chars: u32,
}

enum Item {
    Line(Ln),
    Pic(Pic),
    Table(Table),
}

pub(super) fn is_wide(c: char) -> bool {
    matches!(u32::from(c), 0x2E80..=0xA4CF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFFEF | 0x20000..=0x2FFFF)
}

fn heavy_style(style: u8) -> bool {
    style & STYLE_HEAVY != 0
}

pub(super) fn round_size(s: f32) -> f32 {
    (s * 2.0).round() / 2.0
}

/// The writing frame of a box `[left, top, right, bottom]`.
pub(super) fn frame(b: &[f32; 4], vertical: bool) -> Rect {
    if vertical { [b[1], -b[2], b[3], -b[0]] } else { *b }
}

/// A line made of pieces `pieces` of `text` (offsets relative to `text`).
fn line_of(text: String, pieces: Vec<Piece>, vertical: bool) -> Option<Ln> {
    let mut r = [f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY];
    let (mut weight, mut size, mut heavy) = (0u32, 0.0f32, 0u32);
    for p in &pieces {
        let f = frame(&p.bbox, vertical);
        r = [r[0].min(f[0]), r[1].min(f[1]), r[2].max(f[2]), r[3].max(f[3])];
        let n = text.get(p.start as usize..p.end as usize).map_or(1, |t| t.chars().count().max(1)) as u32;
        weight += n;
        size += p.size * n as f32;
        if heavy_style(p.style) {
            heavy += n;
        }
    }
    if weight == 0 || !r.iter().all(|v| v.is_finite()) {
        return None;
    }
    Some(Ln { vertical, text, pieces, a0: r[0], a1: r[2], c0: r[1], c1: r[3], size: size / weight as f32, heavy: heavy as f32 / weight as f32, chars: weight })
}

/// The part of `line` made of its pieces `from..to`.
fn sub_line(line: &TextLine, from: usize, to: usize, vertical: bool) -> Option<Ln> {
    let first = line.pieces.get(from)?;
    let last = line.pieces.get(to.checked_sub(1)?)?;
    let base = first.start;
    let text = line.text.get(base as usize..last.end as usize)?.to_string();
    let pieces = line.pieces.get(from..to)?.iter().map(|p| Piece { start: p.start - base, end: p.end - base, ..*p }).collect();
    line_of(text, pieces, vertical)
}

// --- pieces of one line ----------------------------------------------------------------------------------------------------------

/// Do `l` and `p` (the line before it in the order they were drawn) make one line on the page? A superscript or a reference mark
/// is drawn as a line of its own when it sits higher than the text beside it: it is, in a box that overlaps the box of the
/// line, and starts where the other one ends.
fn on_one_line(p: &Ln, l: &Ln) -> bool {
    if p.vertical || l.vertical {
        return false;
    }
    let big = p.size.max(l.size);
    let overlap = p.c1.min(l.c1) - p.c0.max(l.c0);
    overlap >= 0.5 * (p.c1 - p.c0).min(l.c1 - l.c0) && l.a0 >= p.a1 - 0.3 * big && l.a0 <= p.a1 + 0.8 * big
}

/// Lines drawn one after the other that are one line on the page, joined.
fn merge_fragments(lines: Vec<Ln>) -> Vec<Ln> {
    let mut out: Vec<Ln> = Vec::with_capacity(lines.len());
    for l in lines {
        if let Some(p) = out.last_mut()
            && on_one_line(p, &l)
        {
            // A space where the gap is wide, as the layout of the text puts one.
            let (a, b) = (p.text.chars().next_back(), l.text.chars().next());
            let spaced = matches!((a, b), (Some(a), Some(b)) if !a.is_whitespace() && !b.is_whitespace() && !is_wide(a) && !is_wide(b))
                && l.a0 - p.a1 > 0.075 * (p.size + l.size);
            let mut text = p.text.clone();
            if spaced {
                text.push(' ');
            }
            let shift = text.len() as u32;
            text.push_str(&l.text);
            let mut pieces = p.pieces.clone();
            pieces.extend(l.pieces.iter().map(|q| Piece { start: q.start + shift, end: q.end + shift, ..*q }));
            if let Some(joined) = line_of(text, pieces, false) {
                *p = joined;
                continue;
            }
        }
        out.push(l);
    }
    out
}

// --- runs --------------------------------------------------------------------------------------------------------------------

/// The runs of a line: pieces of one size and style are one run; the spaces the layout put between pieces go with the piece before.
fn runs_of(line: &Ln) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    let mut at = 0usize;
    for p in &line.pieces {
        let (start, end) = (p.start as usize, p.end as usize);
        let (size, style) = (round_size(p.size), p.style & STYLE_MASK);
        let gap = line.text.get(at..start).unwrap_or("");
        let text = line.text.get(start..end).unwrap_or("");
        match runs.last_mut() {
            Some(r) if r.size == size && r.style == style => {
                r.text.push_str(gap);
                r.text.push_str(text);
            }
            Some(r) => {
                r.text.push_str(gap);
                runs.push(Run { text: text.to_string(), size, style });
            }
            None => runs.push(Run { text: text.to_string(), size, style }),
        }
        at = at.max(end);
    }
    runs
}

fn last_char(runs: &[Run]) -> Option<char> {
    runs.iter().rev().find_map(|r| r.text.chars().next_back())
}

fn first_char(runs: &[Run]) -> Option<char> {
    runs.iter().find_map(|r| r.text.chars().next())
}

/// Add `next` after `runs`: a hyphenated word is joined, two Chinese characters or a space already there need nothing, anything
/// else gets a space.
fn join(runs: &mut Vec<Run>, next: Vec<Run>) {
    if let (Some(a), Some(b)) = (last_char(runs), first_char(&next)) {
        let before_hyphen = runs.iter().rev().flat_map(|r| r.text.chars().rev()).nth(1);
        if a == '-' && before_hyphen.is_some_and(|c| c.is_ascii_lowercase()) && b.is_ascii_lowercase() {
            if let Some(r) = runs.iter_mut().rev().find(|r| !r.text.is_empty()) {
                r.text.pop();
            }
        } else if !(is_wide(a) || is_wide(b) || a.is_whitespace() || b.is_whitespace())
            && let Some(r) = runs.last_mut()
        {
            r.text.push(' ');
        }
    }
    for r in next {
        match runs.last_mut() {
            Some(l) if l.size == r.size && l.style == r.style => l.text.push_str(&r.text),
            _ => runs.push(r),
        }
    }
}

// --- lists -------------------------------------------------------------------------------------------------------------------

/// The bullet or number a text starts with, and how many bytes it takes (with the space after it).
pub(super) fn mark_of(text: &str) -> Option<(Mark, usize)> {
    const BULLETS: &str = "•·●○■□▪◦▶►‣⁃◆◇➢➤✓✔∙";
    const DASHES: &str = "-–—*";
    let c = text.chars().next()?;
    // The end of the marker with the spaces after it; something must follow.
    let after = |end: usize| -> Option<usize> {
        let tail = text.get(end..)?;
        let spaces = tail.len() - tail.trim_start_matches(' ').len();
        (tail.len() > spaces).then_some(end + spaces)
    };
    if BULLETS.contains(c) || DASHES.contains(c) {
        let end = c.len_utf8();
        // A dash or an asterisk is a bullet only with a space after it.
        if DASHES.contains(c) && !text.get(end..).is_some_and(|t| t.starts_with(' ')) {
            return None;
        }
        return after(end).map(|e| (Mark::Bullet(if DASHES.contains(c) { '•' } else { c }), e));
    }
    // 1.  1)  (1)  （1）
    let round = c == '(' || c == '（';
    let from = if round { c.len_utf8() } else { 0 };
    let digits: String = text.get(from..)?.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.len() > 3 {
        return None;
    }
    let n: u32 = digits.parse().ok()?;
    let at = from + digits.len();
    let d = text.get(at..)?.chars().next()?;
    let fmt = match (round, c, d) {
        (true, '(', ')') => NumFmt::Round,
        (true, '（', '）') => NumFmt::RoundWide,
        (false, _, '.') => NumFmt::Dot,
        (false, _, '．') => NumFmt::DotWide,
        (false, _, ')') => NumFmt::Paren,
        (false, _, '）') => NumFmt::ParenWide,
        _ => return None,
    };
    let end = at + d.len_utf8();
    // "3.5" is a number, "3. five" a list item: a space follows the marker.
    if !text.get(end..).is_some_and(|t| t.starts_with(' ')) {
        return None;
    }
    after(end).map(|e| (Mark::Number(n, fmt), e))
}

/// Take `bytes` off the front of the runs.
fn strip_front(runs: &mut Vec<Run>, mut bytes: usize) {
    while bytes > 0 {
        let Some(r) = runs.first_mut() else { break };
        if r.text.len() <= bytes {
            bytes -= r.text.len();
            runs.remove(0);
        } else {
            r.text.drain(..bytes);
            break;
        }
    }
}

// --- paragraphs --------------------------------------------------------------------------------------------------------------

struct Column {
    left: f32,
    right: f32,
    pitch: f32,
}

/// Sort `v` so that `less(a, b)` holds for each `a` before a `b` (a shell sort: the sorts of the library are some kilobytes for each
/// kind of thing sorted, and the lists here are short; this is one small loop for all of them).
pub(super) fn sort<T>(v: &mut [T], less: &mut dyn FnMut(&T, &T) -> bool) {
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

fn median(v: &mut [f32]) -> Option<f32> {
    sort(v, &mut |a, b| a < b);
    v.get(v.len() / 2).copied()
}

/// The text column the lines are in: its left and right edges (so that the shortness of a line can be told), and the usual
/// distance between the lines.
fn column_of(lines: &[Ln]) -> Column {
    let mut a0: Vec<f32> = lines.iter().map(|l| l.a0).collect();
    sort(&mut a0, &mut |a, b| a < b);
    let left = a0.get(a0.len() / 10).copied().unwrap_or(0.0);
    let mut a1: Vec<f32> = lines.iter().filter(|l| l.chars >= 20).map(|l| l.a1).collect();
    if a1.is_empty() {
        a1 = lines.iter().map(|l| l.a1).collect();
    }
    sort(&mut a1, &mut |a, b| a > b);
    let right = a1.get(a1.len() / 5).copied().unwrap_or(left);
    let mut pitches: Vec<f32> = lines
        .windows(2)
        .filter_map(|w| match w {
            [a, b] => {
                let d = 0.5 * (b.c0 + b.c1) - 0.5 * (a.c0 + a.c1);
                ((b.size - a.size).abs() < 0.1 * a.size && d >= 0.7 * a.size && d <= 2.6 * a.size).then_some(d)
            }
            _ => None,
        })
        .collect();
    let size = median(&mut lines.iter().map(|l| l.size).collect::<Vec<_>>()).unwrap_or(10.0);
    let pitch = median(&mut pitches).unwrap_or(1.25 * size);
    Column { left, right, pitch }
}

/// Does the line `l` start a new paragraph after `p`?
fn breaks(lines: &[Ln], i: usize, col: &Column) -> bool {
    let (Some(p), Some(l)) = (i.checked_sub(1).and_then(|k| lines.get(k)), lines.get(i)) else { return true };
    let size = p.size.min(l.size).max(1.0);
    let pitch = 0.5 * (l.c0 + l.c1) - 0.5 * (p.c0 + p.c1);
    if pitch > 1.45 * col.pitch.max(size) || (l.size - p.size).abs() > 0.12 * size {
        return true;
    }
    // Heading and body text of one size differ in weight; a bold word inside a line does not make it heavy.
    if (l.heavy > 0.8) != (p.heavy > 0.8) || mark_of(&l.text).is_some() {
        return true;
    }
    // Two lines centred on the same line are one centred paragraph (a title that wraps), if the first is a long one.
    if (0.5 * (l.a0 + l.a1) - 0.5 * (p.a0 + p.a1)).abs() < 0.6 * size && (l.a0 - p.a0).abs() > size && p.a1 - p.a0 > 0.6 * (col.right - col.left) {
        return false;
    }
    // A line that ends well before the edge of the text around it (the lines near it that look like it) ends its paragraph.
    let near = lines.get(i.saturating_sub(5)..(i + 4).min(lines.len())).unwrap_or(&[]);
    let right = near.iter().filter(|x| (x.size - p.size).abs() < 0.1 * size && (x.a0 - p.a0).abs() < 4.0 * size).map(|x| x.a1).fold(p.a1, f32::max);
    if p.a1 < right - 2.0 * size {
        return true;
    }
    // A first line is indented: an indent that is new, not the hanging indent of a list item's later lines.
    l.a0 > p.a0 + size && mark_of(&p.text).is_none()
}

fn make_para(lines: &[Ln], col: &Column, before: f32, found: Option<(Mark, usize)>) -> Option<Para> {
    let first = lines.first()?;
    let last = lines.last()?;
    let mut runs = runs_of(first);
    for l in lines.iter().skip(1) {
        join(&mut runs, runs_of(l));
    }
    let (mut weight, mut size, mut heavy) = (0u32, 0.0f32, 0.0f32);
    for l in lines {
        weight += l.chars;
        size += l.size * l.chars as f32;
        heavy += l.heavy * l.chars as f32;
    }
    let w = weight.max(1) as f32;
    let size = round_size(size / w);
    let mut mark = None;
    let mut text_a0 = first.a0;
    if let Some((m, bytes)) = found {
        // Where the text after the marker starts.
        text_a0 = first.pieces.iter().find(|p| p.start as usize >= bytes).map_or(first.a0, |p| frame(&p.bbox, first.vertical)[0]);
        mark = Some(m);
        strip_front(&mut runs, bytes);
    }
    let (a0, a1) = lines.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(a, b), l| (a.min(l.a0), b.max(l.a1)));
    let rest_a0 = if lines.len() > 1 { median(&mut lines.iter().skip(1).map(|l| l.a0).collect::<Vec<_>>()).unwrap_or(first.a0) } else { first.a0 };
    let width = (col.right - col.left).max(1.0);
    let mut align = Align::Left;
    let (mut left, mut first_line) = ((rest_a0 - col.left).max(0.0), first.a0 - rest_a0);
    if lines.len() == 1 {
        let mid = 0.5 * (first.a0 + first.a1);
        if (mid - 0.5 * (col.left + col.right)).abs() <= (0.04 * width).max(2.0) && first.a0 - col.left > 1.5 * size && col.right - first.a1 > 1.5 * size {
            align = Align::Center;
        } else if col.right - first.a1 < size && first.a0 - col.left > 0.3 * width && mark.is_none() {
            align = Align::Right;
        } else if (size..=3.5 * size).contains(&(first.a0 - col.left)) {
            (left, first_line) = (0.0, first.a0 - col.left);
        }
    } else {
        let mid0 = 0.5 * (first.a0 + first.a1);
        // (A few lines centred on one line; a long paragraph is not centred however its lines are placed.)
        if lines.len() <= 4 && lines.iter().all(|l| (0.5 * (l.a0 + l.a1) - mid0).abs() <= 0.6 * size) && lines.iter().any(|l| (l.a0 - first.a0).abs() > size) {
            align = Align::Center;
        } else {
            let body = lines.get(..lines.len() - 1).unwrap_or(&[]);
            let edge = body.iter().map(|l| l.a1).fold(f32::NEG_INFINITY, f32::max);
            if !body.is_empty() && body.iter().all(|l| edge - l.a1 <= 0.6 * size) {
                align = Align::Both;
            }
        }
    }
    if left < 0.5 * size {
        left = 0.0;
    }
    if first_line.abs() < 0.5 * size {
        first_line = 0.0;
    }
    if mark.is_some() {
        // The marker hangs out to the left of the text.
        first_line = -(text_a0 - first.a0).max(0.0);
        left = (text_a0 - col.left).max(0.0);
    }
    Some(Para { runs, size, heavy: heavy / w > 0.8, lines: lines.len() as u32, chars: weight, align, left, first: first_line, before, pitch: col.pitch, mark, band: 0, a0, a1, c0: first.c0, c1: last.c1, level: 0, reaches: last.a1 >= col.right - 2.0 * size })
}

/// The paragraphs of lines read one after the other.
fn paragraphs(lines: &[Ln]) -> Vec<Para> {
    let col = column_of(lines);
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut start = 0usize;
    for i in 1..=lines.len() {
        if i == lines.len() || breaks(lines, i, &col) {
            ranges.push((start, i));
            start = i;
        }
    }
    let marks: Vec<Option<(Mark, usize)>> = ranges.iter().map(|&(s, _)| lines.get(s).and_then(|l| mark_of(&l.text))).collect();
    let number = |k: Option<usize>| match k.and_then(|k| marks.get(k)) {
        Some(Some((Mark::Number(n, _), _))) => Some(*n),
        _ => None,
    };
    let mid = |l: &Ln| 0.5 * (l.c0 + l.c1);
    let mut out = Vec::with_capacity(ranges.len());
    for (k, &(s, e)) in ranges.iter().enumerate() {
        let mut found = marks.get(k).copied().flatten();
        // A number only starts a list when the one before it is the number before it, or the one after is the number after it.
        if let Some((Mark::Number(n, _), _)) = found
            && number(k.checked_sub(1)) != Some(n.wrapping_sub(1))
            && number(Some(k + 1)) != Some(n + 1)
        {
            found = None;
        }
        // Space before: how far the distance from the paragraph before is beyond the usual distance between lines.
        let gap = match (s.checked_sub(1).and_then(|p| lines.get(p)), lines.get(s)) {
            (Some(prev), Some(first)) => mid(first) - mid(prev) - col.pitch,
            _ => 0.0,
        };
        if let Some(chunk) = lines.get(s..e)
            && let Some(p) = make_para(chunk, &col, if gap > 2.0 { gap.min(60.0) } else { 0.0 }, found)
        {
            out.push(p);
        }
    }
    out
}

// --- reading order -----------------------------------------------------------------------------------------------------------

/// The items of a page in reading order: groups of items that are read one after the other (groups of one column that follow
/// each other are one).
fn order(rects: &[Rect], gutter: f32) -> Vec<(Vec<usize>, bool)> {
    let mut groups: Vec<(Vec<usize>, u32)> = Vec::new();
    let mut next_id = 1u32;
    cut(rects, (0..rects.len()).collect(), gutter, 0, 0, &mut next_id, &mut groups);
    let mut out: Vec<(Vec<usize>, u32)> = Vec::new();
    for (idx, id) in groups {
        match out.last_mut() {
            Some((last, lid)) if *lid == id => last.extend(idx),
            _ => out.push((idx, id)),
        }
    }
    // (A group that is a column, not the whole width of the page.)
    out.into_iter().map(|(g, id)| (g, id != 0)).collect()
}

/// The widest free strip between the intervals `(from, to, item)` (not narrower than `at_least`): where it starts and ends.
fn widest_gap(items: &mut [(f32, f32, usize)], at_least: f32) -> Option<(f32, f32)> {
    sort(items, &mut |a, b| a.0 < b.0);
    let mut best: Option<(f32, f32)> = None;
    let mut reach = f32::NEG_INFINITY;
    for &(from, to, _) in items.iter() {
        if reach.is_finite() && from - reach >= at_least && best.is_none_or(|(s, e)| from - reach > e - s) {
            best = Some((reach, from));
        }
        reach = reach.max(to);
    }
    best
}

fn cut(rects: &[Rect], idx: Vec<usize>, gutter: f32, depth: u32, id: u32, next_id: &mut u32, out: &mut Vec<(Vec<usize>, u32)>) {
    let rect = |i: usize| rects.get(i).copied().unwrap_or_default();
    let leaf = |mut idx: Vec<usize>, out: &mut Vec<(Vec<usize>, u32)>| {
        sort(&mut idx, &mut |&x, &y| (rect(x)[1], rect(x)[0]) < (rect(y)[1], rect(y)[0]));
        out.push((idx, id));
    };
    if idx.len() <= 1 || depth >= MAX_DEPTH {
        return leaf(idx, out);
    }
    let split_at = |axis: usize, at: f32| -> (Vec<usize>, Vec<usize>) { idx.iter().partition(|&&i| rect(i).get(axis).zip(rect(i).get(axis + 2)).is_some_and(|(a, b)| 0.5 * (a + b) < at)) };
    // A strip down the page that no item crosses: columns.
    let mut along: Vec<(f32, f32, usize)> = idx.iter().map(|&i| (rect(i)[0], rect(i)[2], i)).collect();
    if let Some((s, e)) = widest_gap(&mut along, gutter) {
        let (left, right) = split_at(0, 0.5 * (s + e));
        if !left.is_empty() && !right.is_empty() {
            let (a, b) = (*next_id, *next_id + 1);
            *next_id += 2;
            cut(rects, left, gutter, depth + 1, a, next_id, out);
            cut(rects, right, gutter, depth + 1, b, next_id, out);
            return;
        }
    }
    // Otherwise the widest strip across the page.
    let mut across: Vec<(f32, f32, usize)> = idx.iter().map(|&i| (rect(i)[1], rect(i)[3], i)).collect();
    if let Some((s, e)) = widest_gap(&mut across, 0.1) {
        let (top, bottom) = split_at(1, 0.5 * (s + e));
        if !top.is_empty() && !bottom.is_empty() {
            cut(rects, top, gutter, depth + 1, id, next_id, out);
            cut(rects, bottom, gutter, depth + 1, id, next_id, out);
            return;
        }
    }
    leaf(idx, out)
}

// --- a page ------------------------------------------------------------------------------------------------------------------

/// The cost of the analysis of a page, for the work meter: units per character, line, picture and rule.
pub(super) fn cost(layout: &PageLayout) -> f64 {
    let chars: usize = layout.lines.iter().map(|l| l.text.len()).sum();
    chars as f64 * 150.0 + layout.lines.len() as f64 * 5_000.0 + layout.images.len() as f64 * 20_000.0 + layout.rules.len() as f64 * 4_000.0
}

/// The work of the cells of a table.
pub(super) fn table_cost(t: &Table) -> f64 {
    t.rows.iter().map(Vec::len).sum::<usize>() as f64 * 10_000.0
}

/// A page's blocks in reading order. `pics` are the pictures of the page, placed (already written to the file), by the index of
/// the image in `layout.images`.
pub(super) fn analyze(layout: &PageLayout, pics: &[Option<Pic>]) -> PageOut {
    let (w, h) = layout.size;
    let total: usize = layout.lines.iter().map(|l| l.text.len()).sum();
    let vertical_chars: usize = layout.lines.iter().filter(|l| l.vertical).map(|l| l.text.len()).sum();
    let vertical = total > 0 && vertical_chars * 2 > total;
    let (grids, crossings) = if vertical { (Vec::new(), 0) } else { table::find(&layout.rules, (w, h)) };

    let mut cells_of: Vec<Vec<Vec<Ln>>> = grids.iter().map(|g| vec![Vec::new(); g.rows() * g.cols()]).collect();
    let mut rects: Vec<Rect> = Vec::new();
    let mut items: Vec<Option<Item>> = Vec::new();
    let mut free: Vec<Ln> = Vec::new();
    for line in &layout.lines {
        let Some(full) = sub_line(line, 0, line.pieces.len(), vertical) else { continue };
        let centre = (0.5 * (full.a0 + full.a1), 0.5 * (full.c0 + full.c1));
        let inside = |g: &Grid| centre.0 >= g.bbox[0] - TOL_IN && centre.0 <= g.bbox[2] + TOL_IN && centre.1 >= g.bbox[1] - TOL_IN && centre.1 <= g.bbox[3] + TOL_IN;
        match grids.iter().position(inside) {
            // (Lines of a table: each character goes to the cell it is in.)
            Some(g) => split_into_cells(line, grids.get(g), cells_of.get_mut(g)),
            None => free.push(full),
        }
    }
    for l in merge_fragments(free) {
        rects.push([l.a0, l.c0, l.a1, l.c1]);
        items.push(Some(Item::Line(l)));
    }
    // (A picture comes with its box on the page as it is shown.)
    for pic in pics.iter().flatten() {
        let rect = frame(&pic.rect, vertical);
        rects.push(rect);
        items.push(Some(Item::Pic(Pic { rect, ..pic.clone() })));
    }
    for (g, grid) in grids.iter().enumerate() {
        let table = build_table(grid, cells_of.get_mut(g).map(std::mem::take).unwrap_or_default());
        rects.push(grid.bbox);
        items.push(Some(Item::Table(table)));
    }

    // A column gutter is as wide as most of a character of the page's usual size, and not less than a few points.
    let (mut usual, mut n) = (0.0f32, 0u32);
    for it in items.iter().flatten() {
        if let Item::Line(l) = it {
            usual += l.size * l.chars as f32;
            n += l.chars;
        }
    }
    let usual = if n > 0 { usual / n as f32 } else { 10.0 };
    let gutter = (0.8 * usual).max(MIN_GUTTER);
    let mut blocks = Vec::new();
    // Where a column starts after another, in the blocks.
    let (mut breaks_at, mut after_column) = (Vec::new(), false);
    for (group, columnar) in order(&rects, gutter) {
        if columnar && after_column {
            breaks_at.push(blocks.len());
        }
        after_column = columnar;
        let mut run: Vec<Ln> = Vec::new();
        let flush = |run: &mut Vec<Ln>, blocks: &mut Vec<Block>| {
            for mut p in paragraphs(run) {
                if !vertical {
                    p.band = if p.c1 < 0.1 * h {
                        1
                    } else if p.c0 > 0.9 * h {
                        2
                    } else {
                        0
                    };
                }
                blocks.push(Block::Para(p));
            }
            run.clear();
        };
        for i in group {
            match items.get_mut(i).and_then(Option::take) {
                Some(Item::Line(l)) => run.push(l),
                Some(Item::Pic(p)) => {
                    flush(&mut run, &mut blocks);
                    blocks.push(Block::Pic(p));
                }
                Some(Item::Table(t)) => {
                    flush(&mut run, &mut blocks);
                    blocks.push(Block::Table(t));
                }
                None => {}
            }
        }
        flush(&mut run, &mut blocks);
    }
    // A paragraph that runs on from the bottom of a column to the top of the next is one paragraph.
    for &k in breaks_at.iter().rev() {
        if let (Some(Block::Para(a)), Some(Block::Para(b))) = (k.checked_sub(1).and_then(|p| blocks.get(p)), blocks.get(k))
            && continues(a, b)
            && let Block::Para(b) = blocks.remove(k)
            && let Some(Block::Para(a)) = blocks.get_mut(k - 1)
        {
            a.append(b);
        }
    }
    // The page ends with its text, or is a picture (a scan) and ends with that.
    let bottom = blocks.iter().filter_map(|b| if let Block::Para(p) = b { (p.band != 2).then_some(p.c1) } else { None }).fold(0.0f32, f32::max);
    let filled = if vertical {
        1.0
    } else if bottom > 0.0 {
        bottom / h.max(1.0)
    } else if blocks.iter().any(|b| matches!(b, Block::Pic(_))) {
        0.0
    } else {
        1.0
    };
    PageOut { w, h, vertical, blocks, filled, crossings }
}

/// Put the pieces of a line into the cells of a table.
fn split_into_cells(line: &TextLine, grid: Option<&Grid>, cells: Option<&mut Vec<Vec<Ln>>>) {
    let (Some(grid), Some(cells)) = (grid, cells) else { return };
    let cols = grid.cols();
    let cell_of = |p: &Piece| grid.cell_at(0.5 * (p.bbox[0] + p.bbox[2]), 0.5 * (p.bbox[1] + p.bbox[3]));
    let mut at = 0usize;
    while let Some(cell) = line.pieces.get(at).map(cell_of) {
        let mut end = at + 1;
        while line.pieces.get(end).is_some_and(|p| cell_of(p) == cell) {
            end += 1;
        }
        if let Some(part) = sub_line(line, at, end, false)
            && let Some(slot) = cells.get_mut(cell.0 * cols + cell.1)
        {
            slot.push(part);
        }
        at = end;
    }
}

fn build_table(grid: &Grid, mut cells: Vec<Vec<Ln>>) -> Table {
    let (rows, cols) = (grid.rows(), grid.cols());
    let mut out_rows = Vec::with_capacity(rows);
    for r in 0..rows {
        let mut row = Vec::new();
        let mut c = 0;
        while c < cols {
            let (r0, c0) = grid.owner(r, c);
            let mut span = 1;
            while c + span < cols && grid.owner(r, c + span) == (r0, c0) {
                span += 1;
            }
            let mut down = 1;
            while r0 + down < rows && grid.owner(r0 + down, c0) == (r0, c0) {
                down += 1;
            }
            let vmerge = if r0 != r {
                2
            } else if down > 1 {
                1
            } else {
                0
            };
            let paras = if (r0, c0) == (r, c) {
                let mut lines = cells.get_mut(r * cols + c).map(std::mem::take).unwrap_or_default();
                sort(&mut lines, &mut |a, b| (a.c0, a.a0) < (b.c0, b.a0));
                paragraphs(&lines)
            } else {
                Vec::new()
            };
            let edges = [grid.h(r0, c0), grid.v(r0, c0), grid.h(r0 + down, c0), grid.v(r0, c0 + span)];
            row.push(Cell { span: span as u16, vmerge, edges, paras });
            c += span;
        }
        out_rows.push(row);
    }
    let cols = grid.xs.windows(2).map(|w| if let [a, b] = w { b - a } else { 0.0 }).collect();
    Table { cols, rows: out_rows, rect: grid.bbox }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_marks() {
        assert_eq!(mark_of("• item").map(|m| m.0), Some(Mark::Bullet('•')));
        assert_eq!(mark_of("- item").map(|m| m.0), Some(Mark::Bullet('•')));
        assert_eq!(mark_of("1. item").map(|m| m.0), Some(Mark::Number(1, NumFmt::Dot)));
        assert_eq!(mark_of("12) item").map(|m| m.0), Some(Mark::Number(12, NumFmt::Paren)));
        assert_eq!(mark_of("(3) item").map(|m| m.0), Some(Mark::Number(3, NumFmt::Round)));
        // The full-width forms are lists of their own, and a pair that does not match is no list at all.
        assert_eq!(mark_of("2． item").map(|m| m.0), Some(Mark::Number(2, NumFmt::DotWide)));
        assert_eq!(mark_of("（4） item").map(|m| m.0), Some(Mark::Number(4, NumFmt::RoundWide)));
        assert_eq!(mark_of("5） item").map(|m| m.0), Some(Mark::Number(5, NumFmt::ParenWide)));
        assert_eq!(mark_of("(4） item"), None);
        assert_eq!(mark_of("（4）项目"), None); // no space after it: left as text
        assert_eq!(mark_of("3.5 apples"), None);
        assert_eq!(mark_of("2024 was"), None);
        assert_eq!(mark_of("-5 degrees"), None);
        assert_eq!(mark_of("•"), None);
        assert_eq!(mark_of("1.  two spaces").map(|m| m.1), Some(4));
    }

    #[test]
    fn columns_are_read_one_after_the_other() {
        // Title across the page; two columns of two lines each.
        let rects = vec![[0.0, 0.0, 200.0, 10.0], [0.0, 30.0, 90.0, 40.0], [0.0, 45.0, 90.0, 55.0], [110.0, 30.0, 200.0, 40.0], [110.0, 45.0, 200.0, 55.0]];
        assert_eq!(order(&rects, 8.0), vec![(vec![0], false), (vec![1, 2], true), (vec![3, 4], true)]);
    }

    #[test]
    fn a_single_column_is_one_group() {
        let rects = vec![[0.0, 0.0, 200.0, 10.0], [0.0, 12.0, 150.0, 22.0], [0.0, 60.0, 190.0, 70.0]];
        assert_eq!(order(&rects, 8.0), vec![(vec![0, 1, 2], false)]);
    }

    #[test]
    fn hyphenated_words_are_joined_and_chinese_lines_too() {
        let run = |t: &str| vec![Run { text: t.to_string(), size: 10.0, style: 0 }];
        let text = |a: &[Run]| a.iter().map(|r| r.text.as_str()).collect::<String>();
        let mut a = run("compu-");
        join(&mut a, run("tation goes"));
        assert_eq!(text(&a), "computation goes");
        let mut b = run("这是第一行");
        join(&mut b, run("接着第二行"));
        assert_eq!(text(&b), "这是第一行接着第二行");
        let mut c = run("two");
        join(&mut c, run("words"));
        assert_eq!(text(&c), "two words");
    }
}
