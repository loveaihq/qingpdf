//! The text of the .docx (ISO/IEC 29500-1, WordprocessingML): what is decided over the whole document (running page headers and
//! footers, page numbers, headings, the usual font, sections), and the parts written from the pages' blocks.

use std::collections::HashMap;
use std::fmt::Write as _;

use qingpdf_base::text::font::{STYLE_BOLD, STYLE_CJK_SHIFT, STYLE_FAMILY_SHIFT, STYLE_ITALIC};

use crate::bookmarks::Heading;
use crate::idmap::IdMap;
use crate::xml::esc;
use super::flow::{Align, Block, Cell, Mark, NumFmt, PageOut, Para, Pic, Run, Table, continues, round_size};

/// Most heading levels used.
const MAX_LEVEL: usize = 4;
/// A page whose text stops above this share of its height ends a chapter or a section: the next page starts on a new page.
const SHORT_PAGE: f32 = 0.6;
/// Most different bullet characters kept apart (the others are the usual bullet).
const MAX_BULLETS: usize = 8;
/// The biggest page Word has: 22 inches (in points, and in twips). A bigger page is scaled down to it.
const MAX_PAGE_PT: f32 = 1584.0;
const MAX_PAGE_TWIPS: i32 = 31_680;
/// The size of text when the pages have no sane one (五号, the size of a Chinese document's body), and the smallest and biggest a
/// size can be in Word (points).
const DEFAULT_SIZE: f32 = 10.5;
const MIN_SIZE: f32 = 1.0;
const MAX_SIZE: f32 = 1638.0;
/// Most lines of text one running header (or footer) is made of; the lines past that stay in the body.
const MAX_BAND_LINES: usize = 12;
/// A size in half points as Word has it (`w:sz`): never 0, never more than Word takes; the usual size where the number is none.
fn half_points(pt: f32) -> u32 {
    if pt.is_finite() { (pt * 2.0).round().clamp(2.0 * MIN_SIZE, 2.0 * MAX_SIZE) as u32 } else { (2.0 * DEFAULT_SIZE) as u32 }
}

/// Points to twentieths of a point (twips).
fn tw(pt: f32) -> i32 {
    (pt * 20.0).round().clamp(-1_000_000.0, 1_000_000.0) as i32
}

/// What was found in the document, for the report.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub paragraphs: usize,
    pub headings: usize,
    pub tables: usize,
    pub images: usize,
}

/// The XML parts that depend on the whole document.
pub(super) struct Parts {
    pub document: String,
    pub styles: String,
    pub numbering: String,
    pub header: String,
    pub footer: String,
    pub counts: Counts,
}

// --- text --------------------------------------------------------------------------------------------------------------------

fn plain(p: &Para) -> String {
    p.runs.iter().map(|r| r.text.as_str()).collect()
}

/// A line of text with its numbers made alike, to tell the same running header on every page.
fn norm(text: &str) -> String {
    let mut s: String = text.chars().map(|c| if c.is_ascii_digit() { '#' } else { c }).collect();
    s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    s
}

fn is_page_number(text: &str) -> bool {
    let t = text.to_ascii_lowercase().replace("page", "").replace(" of ", " ");
    let (mut digits, mut roman, mut other) = (0usize, 0usize, 0usize);
    for c in t.chars() {
        if c.is_ascii_digit() {
            digits += 1;
        } else if "ivxlcdm".contains(c) {
            roman += 1;
        } else if !(c.is_whitespace() || "-–—·|./()[]{}<>•:第页共頁".contains(c)) {
            other += 1;
        }
    }
    other == 0 && ((digits > 0 && roman == 0 && digits <= 6) || (digits == 0 && (1..=6).contains(&roman)))
}

// --- running headers and footers ---------------------------------------------------------------------------------------------

/// One paragraph of a running header or footer: a line of text, or the page number.
struct Band {
    text: String,
    align: Align,
    number: bool,
}

/// Take the running headers, footers and page numbers out of the pages; what is left of them for Word's header and footer: every
/// line that repeats (once, in the order the pages show them), and the page number where the first one was.
fn bands(pages: &mut [PageOut]) -> [Vec<Band>; 2] {
    // (band, normalised text) -> pages it is on
    let mut seen: HashMap<(u8, String), usize> = HashMap::new();
    for page in pages.iter() {
        let mut here: Vec<(u8, String)> = Vec::new();
        for b in &page.blocks {
            if let Block::Para(p) = b
                && p.band != 0
            {
                let key = (p.band, norm(&plain(p)));
                if !here.contains(&key) {
                    here.push(key);
                }
            }
        }
        for key in here {
            *seen.entry(key).or_insert(0) += 1;
        }
    }
    let total = pages.len();
    let repeated = |key: &(u8, String)| seen.get(key).is_some_and(|&n| n >= 2 && n * 2 >= total);
    let mut found: [Vec<Band>; 2] = [Vec::new(), Vec::new()];
    let mut taken: std::collections::HashSet<(u8, String)> = std::collections::HashSet::new();
    let mut numbered = [false; 2];
    for page in pages.iter_mut() {
        let width = page.w;
        let mut keep = Vec::with_capacity(page.blocks.len());
        for b in std::mem::take(&mut page.blocks) {
            let Block::Para(p) = &b else {
                keep.push(b);
                continue;
            };
            if p.band == 0 {
                keep.push(b);
                continue;
            }
            let text = plain(p);
            let which = usize::from(p.band - 1);
            let mid = 0.5 * (p.a0 + p.a1);
            let align = if (mid - 0.5 * width).abs() < 0.08 * width { Align::Center } else if mid > 0.62 * width { Align::Right } else { Align::Left };
            let key = (p.band, norm(&text));
            if is_page_number(&text) {
                if let (Some(done), Some(list)) = (numbered.get_mut(which), found.get_mut(which))
                    && !*done
                    && list.len() < MAX_BAND_LINES
                {
                    *done = true;
                    list.push(Band { text: String::new(), align, number: true });
                }
            } else if repeated(&key) {
                if taken.contains(&key) {
                    continue;
                }
                match found.get_mut(which) {
                    Some(list) if list.len() < MAX_BAND_LINES => {
                        list.push(Band { text, align, number: false });
                        taken.insert(key);
                    }
                    // (More lines than a header is made of: this one stays where it is.)
                    _ => keep.push(b),
                }
            } else {
                keep.push(b);
            }
        }
        page.blocks = keep;
    }
    found
}

/// A paragraph that fills its last line and goes on, in the same size and weight and without an indent, at the top of the next page
/// is one paragraph (unless the sentence ended where the page did).
fn join_pages(pages: &mut [PageOut]) {
    for i in 1..pages.len() {
        let (before, after) = pages.split_at_mut(i);
        let (Some(prev), Some(next)) = (before.last_mut(), after.first_mut()) else { continue };
        if prev.vertical != next.vertical || (prev.w - next.w).abs() > 2.0 {
            continue;
        }
        let ok = match (prev.blocks.last(), next.blocks.first()) {
            (Some(Block::Para(a)), Some(Block::Para(b))) => continues(a, b),
            _ => false,
        };
        if ok && !next.blocks.is_empty() {
            let Block::Para(b) = next.blocks.remove(0) else { continue };
            if let Some(Block::Para(a)) = prev.blocks.last_mut() {
                a.append(b);
            }
        }
    }
}

// --- the usual size, the usual font, headings --------------------------------------------------------------------------------

fn each_para<'a>(blocks: &'a [Block], f: &mut dyn FnMut(&'a Para, bool)) {
    for b in blocks {
        match b {
            Block::Para(p) => f(p, false),
            Block::Table(t) => t.rows.iter().flatten().flat_map(|c| &c.paras).for_each(|p| f(p, true)),
            Block::Pic(_) => {}
        }
    }
}

struct Look {
    /// Size of the body text, its font (the style bits of family and Chinese face), its line pitch.
    body: f32,
    style: u8,
    pitch: f32,
    /// Sizes of the heading levels 1 and up.
    heads: Vec<f32>,
}

fn look(pages: &[PageOut]) -> Look {
    let mut sizes: IdMap<u32, u64> = IdMap::new();
    let mut styles: IdMap<u8, u64> = IdMap::new();
    let mut pitches: IdMap<u32, u64> = IdMap::new();
    for page in pages {
        each_para(&page.blocks, &mut |p, _| {
            for r in &p.runs {
                let n = r.text.chars().count() as u64;
                if (MIN_SIZE..=MAX_SIZE).contains(&r.size) {
                    sizes.update((r.size * 2.0) as u32, 0, |v| *v += n);
                }
                styles.update(r.style & !(STYLE_BOLD | STYLE_ITALIC), 0, |v| *v += n);
            }
            if p.lines > 1 && (MIN_SIZE..=2.0 * MAX_SIZE).contains(&p.pitch) {
                pitches.update((p.pitch * 2.0) as u32, 0, |v| *v += u64::from(p.chars));
            }
        });
    }
    let top = |m: &IdMap<u32, u64>| m.iter().max_by_key(|&(k, n)| (*n, *k)).map(|(k, _)| *k as f32 / 2.0);
    let body = top(&sizes).filter(|b| (MIN_SIZE..=MAX_SIZE).contains(b)).unwrap_or(DEFAULT_SIZE);
    let style = styles.iter().max_by_key(|&(k, n)| (*n, *k)).map_or(0, |(k, _)| *k);
    Look { body, style, pitch: top(&pitches).unwrap_or(body * 1.25), heads: Vec::new() }
}

/// Mark the paragraphs that are headings, and give the sizes of the levels.
fn headings(pages: &mut [PageOut], look: &mut Look) {
    let body = look.body;
    let key = |p: &Para| ((p.size * 2.0) as u32, p.heavy);
    // Paragraphs of one size and weight: how many, how many of them take more than one line, and how many characters they have. A
    // size that has paragraphs of several lines, or that has a quarter of the characters the most used size has, is text (the
    // references of a paper in a bigger size than the text are not headings); headings take one line and are few.
    let mut classes: HashMap<(u32, bool), (usize, usize, usize)> = HashMap::new();
    let mut paras = 0usize;
    for page in pages.iter() {
        for b in &page.blocks {
            if let Block::Para(p) = b
                && p.band == 0
                && p.mark.is_none()
            {
                let c = classes.entry(key(p)).or_insert((0, 0, 0));
                c.0 += 1;
                c.1 += usize::from(p.lines > 1);
                c.2 += p.chars as usize;
                paras += 1;
            }
        }
    }
    let most = classes.values().map(|c| c.2).max().unwrap_or(0);
    let text_like = |p: &Para| classes.get(&key(p)).is_some_and(|&(all, multi, chars)| (multi >= 3 && multi * 4 >= all) || (paras >= 20 && chars * 4 >= most));
    let candidate = |p: &Para, strict: bool| -> bool {
        if p.mark.is_some() || p.band != 0 || text_like(p) {
            return false;
        }
        let text = plain(p);
        let letters = text.chars().filter(|c| !c.is_whitespace()).count();
        if letters < 2 || (letters < 3 && !text.chars().any(super::flow::is_wide)) {
            return false;
        }
        let last = text.trim_end().chars().next_back().unwrap_or(' ');
        let sentence = ".。;；,，:：".contains(last);
        // Bigger than the text: a size or two up for one line, a title may take three.
        let big = p.size >= body * 1.08 && p.size >= body + 1.0 && (p.lines <= 3 && p.chars <= 120) && (p.lines == 1 || p.size >= body * 1.15);
        let bold = p.heavy && p.size >= body * 0.95 && p.lines == 1 && p.chars <= 80 && !sentence;
        // (If a lot of the document would be headings, the sizes say little: only what is a lot bigger than the text is one.)
        (if strict { p.size >= body * 1.15 && p.lines <= 3 && p.chars <= 120 } else { big || bold }) && !is_page_number(&text)
    };
    let cands = pages.iter().flat_map(|page| &page.blocks).filter(|b| matches!(b, Block::Para(p) if candidate(p, false))).count();
    let strict = paras >= 40 && cands * 6 > paras;
    // The classes of heading: a size, heavy or not, biggest first.
    let mut levels: Vec<(u32, bool)> = Vec::new();
    for page in pages.iter() {
        for b in &page.blocks {
            if let Block::Para(p) = b
                && candidate(p, strict)
                && !levels.contains(&key(p))
            {
                levels.push(key(p));
            }
        }
    }
    super::flow::sort(&mut levels, &mut |a, b| a > b);
    for page in pages.iter_mut() {
        for b in &mut page.blocks {
            if let Block::Para(p) = b
                && candidate(p, strict)
            {
                let k = key(p);
                p.level = levels.iter().position(|&c| c == k).map_or(MAX_LEVEL, |i| (i + 1).min(MAX_LEVEL)) as u8;
            }
        }
    }
    // (The classes past the fourth level are heading 4 as well.)
    look.heads = levels.iter().take(MAX_LEVEL).map(|c| c.0 as f32 / 2.0).collect();
}

/// The headings of the document (the paragraphs `assemble` makes headings of): the page, the level, the words and how far down the page the
/// first line starts (not for a page of vertical text). The pages' blocks stay.
pub(super) fn heading_list(pages: &mut [PageOut]) -> Vec<Heading> {
    let _ = bands(pages);
    join_pages(pages);
    let mut look = look(pages);
    headings(pages, &mut look);
    let mut found = Vec::new();
    for (index, page) in pages.iter().enumerate() {
        for b in &page.blocks {
            if let Block::Para(p) = b
                && p.level > 0
            {
                found.push(Heading { page: u32::try_from(index).unwrap_or(u32::MAX), level: p.level, text: plain(p), y: (!page.vertical).then_some(p.c0) });
            }
        }
    }
    found
}

// --- fonts and runs ----------------------------------------------------------------------------------------------------------

fn latin(style: u8) -> &'static str {
    match (style >> STYLE_FAMILY_SHIFT) & 3 {
        1 => "Arial",
        2 => "Courier New",
        _ => "Times New Roman",
    }
}

fn east(style: u8) -> &'static str {
    match (style >> STYLE_CJK_SHIFT) & 3 {
        1 => "黑体",
        2 => "楷体",
        3 => "仿宋",
        _ => "宋体",
    }
}

/// What the style a paragraph is in already says about its runs.
#[derive(Clone, Copy)]
struct Base {
    style: u8,
    size: f32,
    bold: bool,
}

fn run_xml(out: &mut String, r: &Run, base: &Base) {
    if r.text.is_empty() {
        return;
    }
    out.push_str("<w:r>");
    let mut props = String::new();
    let family = |s: u8| s & (0xF << STYLE_FAMILY_SHIFT);
    if family(r.style) != family(base.style) {
        let (l, e) = (latin(r.style), east(r.style));
        let _ = write!(props, "<w:rFonts w:ascii=\"{l}\" w:hAnsi=\"{l}\" w:eastAsia=\"{e}\" w:cs=\"{l}\"/>");
    }
    let bold = r.style & STYLE_BOLD != 0;
    // (A heading is bold by its style; a run of a Hei face in it, which is heavy without being called bold, keeps that.)
    if bold && !base.bold {
        props.push_str("<w:b/><w:bCs/>");
    }
    if r.style & STYLE_ITALIC != 0 {
        props.push_str("<w:i/><w:iCs/>");
    }
    if half_points(r.size) != half_points(base.size) {
        let half = half_points(r.size);
        let _ = write!(props, "<w:sz w:val=\"{half}\"/><w:szCs w:val=\"{half}\"/>");
    }
    if !props.is_empty() {
        let _ = write!(out, "<w:rPr>{props}</w:rPr>");
    }
    for (i, piece) in r.text.split('\t').enumerate() {
        if i > 0 {
            out.push_str("<w:tab/>");
        }
        if !piece.is_empty() {
            out.push_str("<w:t xml:space=\"preserve\">");
            esc(out, piece);
            out.push_str("</w:t>");
        }
    }
    out.push_str("</w:r>");
}

// --- the numbering -----------------------------------------------------------------------------------------------------------

#[derive(Default)]
struct Lists {
    /// The numbered lists: the format and the number the list starts at; `numId` is the index + 1 (after the bullets').
    numbered: Vec<(NumFmt, u32)>,
    /// For each format: the list that is going (index into `numbered`) and the number it got to.
    going: [Option<(usize, u32)>; FORMATS],
    bullets: Vec<char>,
}

/// The ways of writing a number: what `lvlText` of the abstract numberings 0 up says; the bullets' come after them.
const FORMATS: usize = 6;
const NUMBER_TEXT: [&str; FORMATS] = ["%1.", "%1)", "(%1)", "%1．", "%1）", "（%1）"];

fn fmt_index(f: NumFmt) -> usize {
    match f {
        NumFmt::Dot => 0,
        NumFmt::Paren => 1,
        NumFmt::Round => 2,
        NumFmt::DotWide => 3,
        NumFmt::ParenWide => 4,
        NumFmt::RoundWide => 5,
    }
}

impl Lists {
    /// The `numId` for a list item. Bullets come first (ids 1 to 8), then the numbered lists.
    fn id(&mut self, m: Mark) -> usize {
        match m {
            Mark::Bullet(c) => {
                let at = self.bullets.iter().position(|&b| b == c).unwrap_or_else(|| {
                    if self.bullets.len() < MAX_BULLETS {
                        self.bullets.push(c);
                        self.bullets.len() - 1
                    } else {
                        0
                    }
                });
                at + 1
            }
            Mark::Number(n, f) => {
                let i = fmt_index(f);
                if let Some((list, last)) = self.going.get(i).copied().flatten()
                    && n == last + 1
                {
                    if let Some(g) = self.going.get_mut(i) {
                        *g = Some((list, n));
                    }
                    return MAX_BULLETS + list + 1;
                }
                self.numbered.push((f, n));
                if let Some(g) = self.going.get_mut(i) {
                    *g = Some((self.numbered.len() - 1, n));
                }
                MAX_BULLETS + self.numbered.len()
            }
        }
    }

    fn xml(&self) -> String {
        let mut s = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:numbering xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">");
        for (i, text) in NUMBER_TEXT.iter().enumerate() {
            let _ = write!(s, "<w:abstractNum w:abstractNumId=\"{i}\"><w:multiLevelType w:val=\"singleLevel\"/><w:lvl w:ilvl=\"0\"><w:start w:val=\"1\"/><w:numFmt w:val=\"decimal\"/><w:lvlText w:val=\"{text}\"/><w:lvlJc w:val=\"left\"/><w:pPr><w:ind w:left=\"420\" w:hanging=\"420\"/></w:pPr></w:lvl></w:abstractNum>");
        }
        for i in 0..MAX_BULLETS {
            let c = self.bullets.get(i).copied().unwrap_or('•');
            let mut text = String::new();
            esc(&mut text, &c.to_string());
            let _ = write!(s, "<w:abstractNum w:abstractNumId=\"{}\"><w:multiLevelType w:val=\"singleLevel\"/><w:lvl w:ilvl=\"0\"><w:start w:val=\"1\"/><w:numFmt w:val=\"bullet\"/><w:lvlText w:val=\"{text}\"/><w:lvlJc w:val=\"left\"/><w:pPr><w:ind w:left=\"420\" w:hanging=\"420\"/></w:pPr></w:lvl></w:abstractNum>", FORMATS + i);
        }
        for i in 0..MAX_BULLETS {
            let _ = write!(s, "<w:num w:numId=\"{}\"><w:abstractNumId w:val=\"{}\"/></w:num>", i + 1, FORMATS + i);
        }
        for (i, (f, start)) in self.numbered.iter().enumerate() {
            let _ = write!(s, "<w:num w:numId=\"{}\"><w:abstractNumId w:val=\"{}\"/><w:lvlOverride w:ilvl=\"0\"><w:startOverride w:val=\"{start}\"/></w:lvlOverride></w:num>", MAX_BULLETS + i + 1, fmt_index(*f));
        }
        s.push_str("</w:numbering>");
        s
    }
}

// --- blocks ------------------------------------------------------------------------------------------------------------------

struct Writer<'a> {
    out: String,
    look: &'a Look,
    lists: Lists,
    counts: Counts,
    next_pic: u32,
    /// Width and height of the text area of the section being written, points.
    area: (f32, f32),
}

impl Writer<'_> {
    fn base(&self, p: &Para) -> Base {
        match p.level {
            0 => Base { style: self.look.style, size: self.look.body, bold: false },
            n => Base { style: self.look.style, size: self.look.heads.get(usize::from(n) - 1).copied().unwrap_or(self.look.body), bold: true },
        }
    }

    fn para(&mut self, p: &Para, page_break: bool, section: Option<&str>) {
        let base = self.base(p);
        let mut ppr = String::new();
        if p.level > 0 {
            let _ = write!(ppr, "<w:pStyle w:val=\"Heading{}\"/><w:keepNext/>", p.level);
        }
        if page_break {
            ppr.push_str("<w:pageBreakBefore/>");
        }
        if let Some(m) = p.mark {
            let id = self.lists.id(m);
            let _ = write!(ppr, "<w:numPr><w:ilvl w:val=\"0\"/><w:numId w:val=\"{id}\"/></w:numPr>");
        }
        let mut spacing = String::new();
        if p.before > 2.0 {
            let _ = write!(spacing, " w:before=\"{}\"", tw(p.before));
        }
        if p.lines > 1 && p.level == 0 && (p.pitch - self.look.pitch).abs() > 1.5 {
            let _ = write!(spacing, " w:line=\"{}\" w:lineRule=\"atLeast\"", tw(p.pitch));
        }
        if !spacing.is_empty() {
            let _ = write!(ppr, "<w:spacing{spacing}/>");
        }
        if p.left > 0.0 || p.first != 0.0 {
            let mut ind = format!(" w:left=\"{}\"", tw(p.left));
            if p.first < 0.0 {
                let _ = write!(ind, " w:hanging=\"{}\"", tw(-p.first));
            } else if p.first > 0.0 {
                let _ = write!(ind, " w:firstLine=\"{}\"", tw(p.first));
            }
            let _ = write!(ppr, "<w:ind{ind}/>");
        }
        match p.align {
            Align::Center => ppr.push_str("<w:jc w:val=\"center\"/>"),
            Align::Right => ppr.push_str("<w:jc w:val=\"right\"/>"),
            Align::Both => ppr.push_str("<w:jc w:val=\"both\"/>"),
            Align::Left => {}
        }
        if let Some(s) = section {
            ppr.push_str(s);
        }
        self.out.push_str("<w:p>");
        if !ppr.is_empty() {
            let _ = write!(self.out, "<w:pPr>{ppr}</w:pPr>");
        }
        for r in &p.runs {
            run_xml(&mut self.out, r, &base);
        }
        self.out.push_str("</w:p>");
        self.counts.paragraphs += 1;
        self.counts.headings += usize::from(p.level > 0);
    }

    fn empty_para(&mut self, inner: &str) {
        let _ = write!(self.out, "<w:p><w:pPr><w:spacing w:before=\"0\" w:after=\"0\" w:line=\"20\" w:lineRule=\"exact\"/>{inner}</w:pPr></w:p>");
    }

    fn pic(&mut self, p: &Pic) {
        let (mut w, mut h) = (p.w.max(1.0), p.h.max(1.0));
        let k = (self.area.0 / w).min(self.area.1 / h).min(1.0);
        if k.is_finite() && k > 0.0 {
            (w, h) = (w * k, h * k);
        }
        // (Never more than the biggest page of Word, whatever the numbers of the file are.)
        let emu = |pt: f32| (pt * 12_700.0).clamp(12_700.0, MAX_PAGE_PT * 12_700.0) as u64;
        let (cx, cy) = (emu(w), emu(h));
        self.next_pic += 1;
        let id = self.next_pic;
        let jc = match p.align {
            Align::Center => "<w:pPr><w:jc w:val=\"center\"/></w:pPr>",
            Align::Right => "<w:pPr><w:jc w:val=\"right\"/></w:pPr>",
            _ => "",
        };
        let flips = format!("{}{}", if p.flip_h { " flipH=\"1\"" } else { "" }, if p.flip_v { " flipV=\"1\"" } else { "" });
        let rid = 100 + p.index;
        let _ = write!(
            self.out,
            "<w:p>{jc}<w:r><w:drawing><wp:inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\"><wp:extent cx=\"{cx}\" cy=\"{cy}\"/><wp:docPr id=\"{id}\" name=\"Picture {id}\"/><wp:cNvGraphicFramePr/><a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><pic:pic><pic:nvPicPr><pic:cNvPr id=\"{id}\" name=\"image{}.{}\"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed=\"rId{rid}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm{flips}><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>",
            p.index, p.ext
        );
        self.counts.images += 1;
    }

    fn table(&mut self, t: &Table) {
        let total: f32 = t.cols.iter().sum();
        let _ = write!(self.out, "<w:tbl><w:tblPr><w:tblW w:w=\"{}\" w:type=\"dxa\"/><w:tblLayout w:type=\"fixed\"/></w:tblPr><w:tblGrid>", tw(total));
        for w in &t.cols {
            let _ = write!(self.out, "<w:gridCol w:w=\"{}\"/>", tw(*w));
        }
        self.out.push_str("</w:tblGrid>");
        for row in &t.rows {
            self.out.push_str("<w:tr>");
            let mut col = 0usize;
            for cell in row {
                self.cell(cell, &t.cols, col);
                col += usize::from(cell.span);
            }
            self.out.push_str("</w:tr>");
        }
        self.out.push_str("</w:tbl>");
        // (A table is followed by a paragraph: two tables one after the other would be one, and a cell or the body cannot end in a table.)
        self.empty_para("");
        self.counts.tables += 1;
    }

    fn cell(&mut self, c: &Cell, cols: &[f32], first: usize) {
        let width: f32 = cols.iter().skip(first).take(usize::from(c.span)).sum();
        let _ = write!(self.out, "<w:tc><w:tcPr><w:tcW w:w=\"{}\" w:type=\"dxa\"/>", tw(width));
        if c.span > 1 {
            let _ = write!(self.out, "<w:gridSpan w:val=\"{}\"/>", c.span);
        }
        match c.vmerge {
            1 => self.out.push_str("<w:vMerge w:val=\"restart\"/>"),
            2 => self.out.push_str("<w:vMerge/>"),
            _ => {}
        }
        self.out.push_str("<w:tcBorders>");
        for (name, on) in ["top", "left", "bottom", "right"].iter().zip(c.edges) {
            let _ = if on { write!(self.out, "<w:{name} w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"000000\"/>") } else { write!(self.out, "<w:{name} w:val=\"nil\"/>") };
        }
        self.out.push_str("</w:tcBorders></w:tcPr>");
        if c.paras.is_empty() {
            self.out.push_str("<w:p/>");
        }
        for p in &c.paras {
            self.para(p, false, None);
        }
        self.out.push_str("</w:tc>");
    }
}

// --- sections ----------------------------------------------------------------------------------------------------------------

struct Section {
    first: usize,
    last: usize,
    w: f32,
    h: f32,
    vertical: bool,
    margins: [f32; 4],
}

fn sections(pages: &[PageOut]) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    for (i, page) in pages.iter().enumerate() {
        let same = out.last().is_some_and(|s| (s.w - page.w).abs() < 2.0 && (s.h - page.h).abs() < 2.0 && s.vertical == page.vertical);
        if !same {
            out.push(Section { first: i, last: i, w: page.w, h: page.h, vertical: page.vertical, margins: [f32::INFINITY, f32::INFINITY, f32::INFINITY, f32::INFINITY] });
        }
        let Some(s) = out.last_mut() else { continue };
        s.last = i;
        // The margins are the page's text area: from the left-most to the right-most text (in the section's pages), and so on.
        for b in &page.blocks {
            let r = b.rect();
            let (x0, y0, x1, y1) = if page.vertical { (-r[3], r[0], -r[1], r[2]) } else { (r[0], r[1], r[2], r[3]) };
            let m = [y0, page.w - x1, page.h - y1, x0];
            for (slot, v) in s.margins.iter_mut().zip(m) {
                *slot = slot.min(v);
            }
        }
    }
    for s in &mut out {
        let ok = s.margins.iter().all(|m| m.is_finite());
        // (`clamp` would panic on a page smaller than 45 points.)
        let fit = |m: f32, dim: f32| m.max(18.0).min((0.4 * dim).max(0.0));
        s.margins = if ok {
            [fit(s.margins[0], s.h), fit(s.margins[1], s.w), fit(s.margins[2], s.h), fit(s.margins[3], s.w)]
        } else {
            [72.0, 72.0, 72.0, 72.0]
        };
    }
    out
}

fn sect_pr(s: &Section) -> String {
    let (w, h) = (tw(s.w).clamp(1_440, MAX_PAGE_TWIPS), tw(s.h).clamp(1_440, MAX_PAGE_TWIPS));
    // (A margin is less than half of the page, so that a text area is left.)
    let [t, r, b, l] = [(s.margins[0], h), (s.margins[1], w), (s.margins[2], h), (s.margins[3], w)].map(|(m, dim)| tw(m).clamp(0, (dim / 2 - 1).max(0)));
    let head = (t / 2).min(720);
    let foot = (b / 2).min(720);
    format!(
        "<w:sectPr><w:headerReference w:type=\"default\" r:id=\"rId4\"/><w:footerReference w:type=\"default\" r:id=\"rId5\"/><w:pgSz w:w=\"{w}\" w:h=\"{h}\"{}/><w:pgMar w:top=\"{t}\" w:right=\"{r}\" w:bottom=\"{b}\" w:left=\"{l}\" w:header=\"{head}\" w:footer=\"{foot}\" w:gutter=\"0\"/>{}</w:sectPr>",
        if w > h { " w:orient=\"landscape\"" } else { "" },
        if s.vertical { "<w:textDirection w:val=\"tbRl\"/>" } else { "" }
    )
}

// --- the whole -----------------------------------------------------------------------------------------------------------------

fn band_xml(root: &str, style: &str, lines: &[Band]) -> String {
    let mut s = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:{root} xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">");
    for b in lines {
        let _ = write!(s, "<w:p><w:pPr><w:pStyle w:val=\"{style}\"/>");
        match b.align {
            Align::Center => s.push_str("<w:jc w:val=\"center\"/>"),
            Align::Right => s.push_str("<w:jc w:val=\"right\"/>"),
            _ => {}
        }
        s.push_str("</w:pPr>");
        if b.number {
            s.push_str("<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r><w:r><w:instrText xml:space=\"preserve\"> PAGE </w:instrText></w:r><w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:t>1</w:t></w:r><w:r><w:fldChar w:fldCharType=\"end\"/></w:r>");
        } else if !b.text.is_empty() {
            s.push_str("<w:r><w:t xml:space=\"preserve\">");
            esc(&mut s, &b.text);
            s.push_str("</w:t></w:r>");
        }
        s.push_str("</w:p>");
    }
    if lines.is_empty() {
        let _ = write!(s, "<w:p><w:pPr><w:pStyle w:val=\"{style}\"/></w:pPr></w:p>");
    }
    let _ = write!(s, "</w:{root}>");
    s
}

fn styles_xml(look: &Look) -> String {
    let half = half_points;
    let (l, e) = (latin(look.style), east(look.style));
    let mut s = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii=\"{l}\" w:hAnsi=\"{l}\" w:eastAsia=\"{e}\" w:cs=\"{l}\"/><w:sz w:val=\"{0}\"/><w:szCs w:val=\"{0}\"/><w:lang w:val=\"en-US\" w:eastAsia=\"zh-CN\" w:bidi=\"ar-SA\"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:before=\"0\" w:after=\"0\" w:line=\"{1}\" w:lineRule=\"atLeast\"/></w:pPr></w:pPrDefault></w:docDefaults>",
        half(look.body),
        tw(look.pitch)
    );
    s.push_str("<w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/><w:qFormat/></w:style>");
    s.push_str("<w:style w:type=\"character\" w:default=\"1\" w:styleId=\"DefaultParagraphFont\"><w:name w:val=\"Default Paragraph Font\"/><w:uiPriority w:val=\"1\"/><w:semiHidden/></w:style>");
    s.push_str("<w:style w:type=\"table\" w:default=\"1\" w:styleId=\"TableNormal\"><w:name w:val=\"Normal Table\"/><w:uiPriority w:val=\"99\"/><w:semiHidden/><w:tblPr><w:tblInd w:w=\"0\" w:type=\"dxa\"/><w:tblCellMar><w:top w:w=\"0\" w:type=\"dxa\"/><w:left w:w=\"108\" w:type=\"dxa\"/><w:bottom w:w=\"0\" w:type=\"dxa\"/><w:right w:w=\"108\" w:type=\"dxa\"/></w:tblCellMar></w:tblPr></w:style>");
    for n in 1..=MAX_LEVEL {
        let size = look.heads.get(n - 1).copied().unwrap_or(look.body);
        let _ = write!(
            s,
            "<w:style w:type=\"paragraph\" w:styleId=\"Heading{n}\"><w:name w:val=\"heading {n}\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:uiPriority w:val=\"9\"/><w:qFormat/><w:pPr><w:keepNext/><w:outlineLvl w:val=\"{}\"/></w:pPr><w:rPr><w:b/><w:bCs/><w:sz w:val=\"{1}\"/><w:szCs w:val=\"{1}\"/></w:rPr></w:style>",
            n - 1,
            half(size)
        );
    }
    for (id, name) in [("Header", "header"), ("Footer", "footer")] {
        let _ = write!(s, "<w:style w:type=\"paragraph\" w:styleId=\"{id}\"><w:name w:val=\"{name}\"/><w:basedOn w:val=\"Normal\"/><w:uiPriority w:val=\"99\"/></w:style>");
    }
    s.push_str("</w:styles>");
    s
}

pub(super) const W_NS: &str = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"";

/// Everything that depends on the whole document, from the pages (their blocks are used up).
pub(super) fn assemble(pages: &mut [PageOut]) -> Parts {
    // A page bigger than Word's biggest is made smaller as a whole, so that it is the same page on a smaller sheet.
    for page in pages.iter_mut() {
        let big = page.w.max(page.h).min(f32::MAX);
        if big > MAX_PAGE_PT {
            page.scale(MAX_PAGE_PT / big);
        }
    }
    let [header, footer] = bands(pages);
    join_pages(pages);
    let mut look = look(pages);
    headings(pages, &mut look);
    let sects = sections(pages);
    let mut w = Writer { out: String::new(), look: &look, lists: Lists::default(), counts: Counts::default(), next_pic: 0, area: (0.0, 0.0) };
    let _ = write!(w.out, "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:document {W_NS}><w:body>");
    let mut prev_filled = 1.0f32;
    for (si, sect) in sects.iter().enumerate() {
        w.area = (sect.w - sect.margins[1] - sect.margins[3], sect.h - sect.margins[0] - sect.margins[2]);
        if sect.vertical {
            w.area = (w.area.1, w.area.0);
        }
        let last_section = si + 1 == sects.len();
        let end = sect_pr(sect);
        for pi in sect.first..=sect.last {
            let Some(page) = pages.get_mut(pi) else { continue };
            let mut first = true;
            // (The blocks of a page are let go of when they are written, so that the text is not in memory twice.)
            let blocks = std::mem::take(&mut page.blocks);
            let n = blocks.len();
            let page_break = pi > 0 && prev_filled < SHORT_PAGE;
            let page_filled = page.filled;
            for (bi, b) in blocks.iter().enumerate() {
                // The section ends with the last block of its last page, in that block when it is a paragraph.
                let ends = pi == sect.last && bi + 1 == n && !last_section;
                let marker = ends.then(|| end.clone());
                let brk = first && page_break;
                match b {
                    Block::Para(p) => w.para(p, brk, marker.as_deref()),
                    Block::Table(t) => {
                        if brk {
                            w.out.push_str("<w:p><w:r><w:br w:type=\"page\"/></w:r></w:p>");
                        }
                        w.table(t);
                        if let Some(m) = &marker {
                            w.empty_para(m);
                        }
                    }
                    Block::Pic(p) => {
                        if brk {
                            w.out.push_str("<w:p><w:r><w:br w:type=\"page\"/></w:r></w:p>");
                        }
                        w.pic(p);
                        if let Some(m) = &marker {
                            w.empty_para(m);
                        }
                    }
                }
                first = false;
            }
            if n == 0 && pi == sect.last && !last_section {
                w.empty_para(&end);
            }
            // How far down the page the text got decides whether the next page starts afresh. A page that is a picture does.
            prev_filled = page_filled;
        }
    }
    if sects.is_empty() {
        w.out.push_str("<w:p/>");
    }
    let last = sects.last().map_or_else(|| sect_pr(&Section { first: 0, last: 0, w: 595.0, h: 842.0, vertical: false, margins: [72.0; 4] }), sect_pr);
    w.out.push_str(&last);
    w.out.push_str("</w:body></w:document>");
    let _ = round_size(0.0);
    Parts {
        document: std::mem::take(&mut w.out),
        styles: styles_xml(&look),
        numbering: w.lists.xml(),
        header: band_xml("hdr", "Header", &header),
        footer: band_xml("ftr", "Footer", &footer),
        counts: w.counts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_escaped_and_what_xml_does_not_allow_is_dropped() {
        let mut out = String::new();
        esc(&mut out, "a<b>&c\u{1}\u{b}d\u{7f}\u{85}e\u{fffe}\u{ffff}\u{fdd0}f\u{e000}g\u{1f600}h\t\n");
        assert_eq!(out, "a&lt;b&gt;&amp;cdef\u{e000}g\u{1f600}h\t\n");
        let mut out = String::new();
        esc(&mut out, "中文，全角：標點");
        assert_eq!(out, "中文，全角：標點");
    }

    #[test]
    fn page_numbers_are_told_from_text() {
        for yes in ["12", "- 3 -", "Page 4", "Page 4 of 10", "第 5 页", "共 10 页", "iv", "— 7 —", "12 / 30"] {
            assert!(is_page_number(yes), "{yes}");
        }
        for no in ["Introduction", "2024 Annual Report", "Table 1", "Chapter 2", ""] {
            assert!(!is_page_number(no), "{no}");
        }
        assert_eq!(norm("Page  12   of 40"), "Page ## of ##");
    }

    #[test]
    fn numbers_give_one_list_when_they_follow_one_another() {
        let mut l = Lists::default();
        let a = l.id(Mark::Number(1, NumFmt::Dot));
        assert_eq!(l.id(Mark::Number(2, NumFmt::Dot)), a);
        assert_ne!(l.id(Mark::Number(7, NumFmt::Dot)), a);
        assert_ne!(l.id(Mark::Number(1, NumFmt::Paren)), a);
        let xml = l.xml();
        assert!(xml.contains("<w:startOverride w:val=\"7\"/>") && xml.matches("<w:lvlOverride").count() == 3);
        assert_ne!(l.id(Mark::Number(1, NumFmt::DotWide)), a);
        assert!(l.xml().contains("<w:lvlText w:val=\"%1．\"/>"));
        assert_eq!(l.id(Mark::Bullet('■')), 1);
        assert_eq!(l.id(Mark::Bullet('•')), 2);
        assert_eq!(l.id(Mark::Bullet('■')), 1);
        assert!(l.xml().contains("w:lvlText w:val=\"■\""));
    }
}
