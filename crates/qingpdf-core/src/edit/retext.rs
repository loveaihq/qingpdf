//! Changing text that is already on a page (5a; ISO 32000-1 9.4, 7.8.2, 8.10): the reader's "edit a line", and the command line's find
//! and replace.
//!
//! The page is read once by `text::EditScanner`, which finds every character of the page's text (what `qingpdf text` and the reader's
//! search find) and, for each, the operator that shows it and its bytes in the string. A change is a *hit*: some characters that
//! are drawn one after the other, and the text that is to stand in their place. The old characters are taken out of the content
//! stream (a `Tj` or `TJ` is cut where they were; nothing is painted over them), and the new text is put where the first of them
//! was, as new strings of the same operator, in the same graphics state: the size, colour, character and word spacing, scaling,
//! rise and text matrix stay as they were. What is drawn after a replaced hit in the same operator stays where it was (a number
//! of a `TJ` makes up the difference in width); what an operator of its own draws follows the new text as it followed the old.
//! A marked-content sequence with an `/ActualText` that holds the hit gets the new text there when it holds nothing else, and
//! loses the `/ActualText` otherwise (extraction would read the old text from it).
//!
//! The new text is shown in the font the old text used when that font has a glyph for every character (the codes come from its
//! `/ToUnicode` or its encoding, and the font program is asked whether it holds the glyph, since a subset holds only what the page
//! used). Characters it lacks are put in a subset of a system font that looks like it (by name and `/Flags`; for Chinese, Song,
//! Hei, Kai or Fang), switched to with `Tf` just for them and back after. Where the old font is a Type 3 font (which has its own
//! `/FontMatrix`, often a flipped one) the substitute is a Type 3 font too, with the same matrix, its glyphs drawn from the outlines.
//!
//! Only the statements of the operators that change are written again; every other byte of a stream stays. A page's own stream that
//! changes, and a form XObject that changes (it is copied, and the copy is what the page draws: another page that shares the form
//! keeps the old one; pages that get the same change share one copy), become new objects; the original objects stay in the file.
//!
//! One line at a time: the characters of a hit are on one line of the page's text, and nothing is moved to make room.

use std::collections::HashMap;
use std::rc::Rc;

use miniz_oxide::deflate::compress_to_vec_zlib;

use super::appearance::num;
use super::content::{charge, check_content_allowed, contents_list};
use super::fontsub;
use super::ocr::OCR_MARK;
use super::{Ctx, TOO_BIG, commit, replay, save_section, text_string};
use crate::document::{Document, Page};
use crate::error::{Error, Result};
use crate::object::{Dict, Name, ObjRef, Object, Stream};
use crate::render::GlyphProbe;
use crate::render::sysfont::Request;
use crate::render::work::{PAGE_WORK, Work};
use crate::text::font::Font;
use crate::text::{ActualSpan, EditScanner, FontRec, Scan, ShowItem, ShowOp, Shown, Source};

/// Most hits on one page, and most replaced in one run of the command line.
pub(super) const MAX_HITS_PER_PAGE: usize = 20_000;
pub const MAX_REPLACE_HITS: usize = 500_000;
/// Most bytes one content stream may have after it is changed.
const MAX_STREAM_BYTES: usize = 64 << 20;
/// Most characters of a new text.
pub const MAX_TEXT_CHARS: usize = 4_000;
/// Most glyphs between the first and the last of one hit (a hit is a few lines of text at most).
const MAX_HIT_GLYPHS: usize = 16_384;
/// The work meter's charges, in units of about a nanosecond: a hit looked at, and a place where the text to find is, looked at; a
/// glyph between the first and the last of a hit; a character of the new text put in a hit; a character looked for in a font (its
/// codes found, its glyph asked for); a font's program read for the first time; a byte of content written again and compressed.
const HIT_COST: f64 = 5_000.0;
const MATCH_COST: f64 = 300.0;
const SPAN_COST: f64 = 20.0;
const CHAR_COST: f64 = 100.0;
const ENCODE_COST: f64 = 200_000.0;
const FONT_COST: f64 = 3_000_000.0;
const BYTE_COST: f64 = 14.0;
/// What a whole run of the command line may cost, in pages' worth of work (each page has its own meter for being read).
const RUN_PAGES: f64 = 200.0;
/// What the command line lets an edit take, in bytes.
const CLI_ROOM: usize = 512 << 20;
/// How far from a glyph (in its size) a click may be and still be on its line.
const REACH: f64 = 0.7;
const NOT_THERE: &str = "the text is not where it should be";
/// How many forms inside forms are followed when looking for a mark (the reader's own limit is 32).
const MAX_FORM_NESTING: usize = 32;

// --- the text of a line ------------------------------------------------------------------------------------------

/// Characters that are not allowed in the new text of a line (a change is one line, and a control character is no text).
fn bad_text(text: &str) -> Option<&'static str> {
    if text.chars().count() > MAX_TEXT_CHARS {
        Some("the text is too long")
    } else if text.chars().any(char::is_control) {
        Some("the text has a line end or another control character")
    } else {
        None
    }
}

/// A line of the page's text: its bytes in the text, and the marks (of `scan.marks`) in it.
struct LineAt {
    bytes: (usize, usize),
    marks: (usize, usize),
}

/// The index of the mark of each glyph (`u32::MAX`: it has none, its text is not in the page's text).
fn mark_of(scan: &Scan) -> Vec<u32> {
    let mut of = vec![u32::MAX; scan.glyphs.len()];
    for (i, m) in scan.marks.iter().enumerate() {
        if let Some(slot) = of.get_mut(m.glyph) {
            *slot = u32::try_from(i).unwrap_or(u32::MAX);
        }
    }
    of
}

fn distance(b: [f32; 4], (x, y): (f64, f64)) -> f64 {
    let dx = (f64::from(b[0]) - x).max(x - f64::from(b[2])).max(0.0);
    let dy = (f64::from(b[1]) - y).max(y - f64::from(b[3])).max(0.0);
    dx.hypot(dy)
}

/// The line of text a place (points of the page as shown) is on, or the nearest one within reach.
fn line_at(scan: &Scan, shown: &Shown, at: (f64, f64)) -> Option<LineAt> {
    let mut best: Option<(f64, usize)> = None;
    for m in &scan.marks {
        let g = scan.glyphs.get(m.glyph)?;
        let d = distance(shown.glyph_box(g, 0.0, 1.0), at);
        if d <= REACH * g.size.max(4.0) && best.is_none_or(|(b, _)| d < b) {
            best = Some((d, m.start));
        }
    }
    let at_byte = best?.1;
    let start = scan.text.get(..at_byte)?.rfind('\n').map_or(0, |i| i + 1);
    let end = scan.text.get(at_byte..)?.find('\n').map_or(scan.text.len(), |i| at_byte + i);
    let first = scan.marks.partition_point(|m| m.end <= start);
    let last = scan.marks.partition_point(|m| m.start < end);
    Some(LineAt { bytes: (start, end), marks: (first, last) })
}

/// Why the bytes `a..b` of the content of the stream `src` cannot be written again, if they cannot (the content stream they are in,
/// and the form, are looked at, not the font).
fn place_problem(scan: &Scan, src: u32, a: usize, b: usize) -> Option<&'static str> {
    let Some(source) = scan.sources.get(src as usize) else { return Some("the page has too many forms to follow") };
    if source.uses != 1 {
        return Some("the text is in a form that the page draws more than once");
    }
    stream_problem(source, a, b)
}

fn stream_problem(source: &Source, a: usize, b: usize) -> Option<&'static str> {
    if source.form.is_none() {
        let Some(i) = source.stream_at(a, b).filter(|&i| source.streams.get(i).is_some_and(|s| s.2.is_some())) else {
            return Some("the text is in a content stream that cannot be changed");
        };
        if source.repeated.get(i) == Some(&true) {
            return Some("the page uses one content stream twice");
        }
    }
    None
}

/// Is the content stream `src` a recognised text layer (5c), or a form inside one? The layer is a form with the mark of `ocr`.
fn in_ocr_layer(scan: &Scan, mut src: u32) -> bool {
    for _ in 0..MAX_FORM_NESTING {
        let Some(source) = scan.sources.get(src as usize) else { return false };
        if matches!(source.dict.get(OCR_MARK), Some(Object::Bool(true))) {
            return true;
        }
        if source.form.is_none() {
            return false;
        }
        src = source.parent;
    }
    false
}

/// Why the text of a glyph cannot be changed, if it cannot.
fn not_editable(scan: &Scan, g: usize) -> Option<&'static str> {
    let rec = scan.chars.get(g)?;
    let Some(show) = scan.parts.get(rec.part as usize).and_then(|p| scan.shows.get(p.show as usize)) else { return Some(NOT_THERE) };
    // The recognised text of a scan is the picture's words, not the picture: changing it would leave the picture as it is. Other
    // invisible text (text rendering mode 3, 9.3.6) is the same thing, from another program.
    if in_ocr_layer(scan, show.src) {
        return Some("the text is the recognised text of a scanned page; the picture it stands for is not changed");
    }
    if show.render == 3 {
        return Some("the text is invisible (text rendering mode 3, such as the text layer of a scan)");
    }
    if scan.fonts.get(show.font as usize).is_none() {
        return Some("the text is in a font that was not found");
    }
    place_problem(scan, show.src, show.start as usize, show.end as usize)
}

/// What the reader shows of a line to be edited.
#[derive(Clone, Debug, PartialEq)]
pub struct LineView {
    pub text: String,
    /// `[left, top, right, bottom]`, points of the page as shown.
    pub rect: [f32; 4],
    /// Why it cannot be changed (empty when it can).
    pub why: String,
}

/// The line of text at `at` on `page` (points of the page as shown).
pub fn text_line(doc: &Document, page: &Page, at: [f32; 2], work: &Work) -> Result<Option<LineView>> {
    let mut scanner = EditScanner::new(doc);
    let scan = {
        let _budget = doc.page_decode_budget();
        scanner.scan(page, work)?
    };
    let shown = Shown::of(page);
    let Some(line) = line_at(&scan, &shown, (f64::from(at[0]), f64::from(at[1]))) else { return Ok(None) };
    let text = scan.text.get(line.bytes.0..line.bytes.1).unwrap_or("").to_string();
    let mut rect = [f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY];
    let mut why = "";
    for m in scan.marks.get(line.marks.0..line.marks.1).unwrap_or(&[]) {
        if let Some(g) = scan.glyphs.get(m.glyph) {
            let b = shown.glyph_box(g, 0.0, 1.0);
            rect = [rect[0].min(b[0]), rect[1].min(b[1]), rect[2].max(b[2]), rect[3].max(b[3])];
        }
        if why.is_empty() {
            why = not_editable(&scan, m.glyph).unwrap_or("");
        }
    }
    Ok(Some(LineView { text, rect, why: why.to_string() }))
}

// --- hits --------------------------------------------------------------------------------------------------------

/// Glyphs to take out (drawn one after the other) and where the new text goes: before the first of them, or (when none are taken
/// out) before or after the glyph `anchor`.
struct Hit {
    del: Vec<usize>,
    anchor: usize,
    after: bool,
}

/// The hit that takes the glyphs of the marks `first..last` out, if nothing else of the page's text is drawn between them (and they
/// are not spread over more than [`MAX_HIT_GLYPHS`] glyphs). What it costs is charged.
fn hit_of(ctx: &Ctx<'_>, scan: &Scan, marked: &[u32], first: usize, last: usize) -> Result<Option<Hit>> {
    let Some(marks) = scan.marks.get(first..last) else { return Ok(None) };
    let (Some(lo), Some(hi)) = (marks.iter().map(|m| m.glyph).min(), marks.iter().map(|m| m.glyph).max()) else { return Ok(None) };
    if hi - lo >= MAX_HIT_GLYPHS {
        return Ok(None);
    }
    charge(ctx, SPAN_COST * (marks.len() + hi - lo + 1) as f64)?;
    let mut member = vec![false; hi - lo + 1];
    for m in marks {
        match member.get_mut(m.glyph - lo) {
            Some(slot) => *slot = true,
            None => return Ok(None),
        }
    }
    for (k, g) in (lo..=hi).enumerate() {
        if marked.get(g).is_some_and(|&m| m != u32::MAX) && !member.get(k).copied().unwrap_or(false) {
            return Ok(None);
        }
    }
    Ok(Some(Hit { del: (lo..=hi).collect(), anchor: lo, after: false }))
}

/// The marks of `scan` inside the bytes `a..b` of its text, if none is cut by them.
fn marks_in(scan: &Scan, a: usize, b: usize) -> Option<(usize, usize)> {
    let first = scan.marks.partition_point(|m| m.end <= a);
    let last = scan.marks.partition_point(|m| m.start < b);
    scan.marks.get(first..last)?.iter().all(|m| m.start >= a && m.end <= b).then_some((first, last))
}

// --- the fonts ---------------------------------------------------------------------------------------------------

/// A character in the page's own font: the bytes of its string, and how far it advances (in text space units per size: the width, or
/// the vertical displacement). A space the font draws with no width has no bytes: `adv` is the width it is given instead.
#[derive(Clone)]
struct Enc {
    bytes: Vec<u8>,
    adv: f64,
    space: bool,
}

/// Things by a key (a character, a font): a map for the command line, where there can be thousands, a short list for the reader,
/// where a line of text has few (and the map is not in it).
struct ByKey<K, V, const MANY: bool> {
    map: HashMap<K, V>,
    list: Vec<(K, V)>,
}

impl<K: std::hash::Hash + Eq + Copy, V, const MANY: bool> ByKey<K, V, MANY> {
    fn new() -> Self {
        ByKey { map: HashMap::new(), list: Vec::new() }
    }

    fn get(&self, k: K) -> Option<&V> {
        if MANY { self.map.get(&k) } else { self.list.iter().find(|e| e.0 == k).map(|e| &e.1) }
    }

    fn insert(&mut self, k: K, v: V) {
        if MANY {
            self.map.insert(k, v);
        } else {
            self.list.push((k, v));
        }
    }
}

struct FontCtx<const MANY: bool> {
    font: Rc<Font>,
    entry: Object,
    probe: Option<GlyphProbe>,
    tried: bool,
    /// What the substitute font is made like: the request and, for a Type 3 font, its `/FontMatrix`.
    request: Option<Request>,
    matrix: Option<[f64; 6]>,
    known: ByKey<char, Option<Enc>, MANY>,
    /// The characters of the new text that the font lacks: an index of `Run::missing_sets`.
    missing: Option<usize>,
}

/// What a substitute font is made for: the look it is to have, vertical or not, the matrix of a Type 3 font, and the characters
/// (an index of `Run::missing_sets`).
type FallbackKey = (Request, bool, Option<[u64; 6]>, usize);

/// A system font subset put in the file for characters the page's font lacks: for each character, the bytes that show it and its
/// advance in ems (the vertical one, 1 down, for vertical writing).
struct Fallback<const MANY: bool> {
    key: FallbackKey,
    font: ObjRef,
    codes: ByKey<char, (Vec<u8>, f64), MANY>,
}

impl<const MANY: bool> Fallback<MANY> {
    fn name(&self) -> Vec<u8> {
        let mut name = b"Qf".to_vec();
        name.extend_from_slice(self.font.num.to_string().as_bytes());
        name
    }
}

enum Frag {
    Orig(Vec<u8>),
    Fall(usize, Vec<u8>),
    /// A space the page's font has no usable glyph for: the number of a `TJ` that moves over it.
    Gap(f64),
    /// The number of a `TJ` that makes up for the new text being wider or narrower than the old, so that what follows it in the
    /// operator stays where it was; written only when something does follow.
    Keep(f64),
}

/// What is left of an operator's contents once the edits are made.
enum Piece<'f> {
    Bytes(Vec<u8>),
    Num(f64),
    New(&'f [Frag]),
    /// Characters taken out (by the hit of that number).
    Cut(u32),
}

/// An item of the array of a `TJ`: a string or a number.
enum Item {
    Str(Vec<u8>),
    Num(f64),
}

fn keep(pieces: &mut Vec<Piece<'_>>, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    match pieces.last_mut() {
        Some(Piece::Bytes(b)) => b.extend_from_slice(bytes),
        _ => pieces.push(Piece::Bytes(bytes.to_vec())),
    }
}

/// What the edits of a page are, in lists (sorted by [`PageEdits::seal`] when the page is written).
#[derive(Default)]
struct PageEdits {
    /// The characters taken out: (string, offset, bytes, the hit they are of).
    deleted: Vec<(u32, u32, u8, u32)>,
    /// New text, before the character at (string, offset).
    inserted: Vec<(u32, u32, Vec<Frag>)>,
    /// The operators that change (indices of `scan.shows`).
    shows: Vec<u32>,
    /// (content stream, fallback font) that are used.
    fallbacks: Vec<(u32, usize)>,
    /// The marked-content sequences with an `/ActualText` that change (`scan.spans`) and what the text becomes (`None`: it goes).
    spans: Vec<(u32, Option<String>)>,
    hits: usize,
    /// About how many bytes `inserted` takes.
    bytes: usize,
}

/// A statement to write again: (content stream, start, end, bytes).
type Change = (u32, usize, usize, Vec<u8>);

/// The statements in the order of their places. For a few of them (the reader changes one line); [`order_many`] is for the
/// command line, where there can be tens of thousands, and is not linked into the reader.
fn order_few(list: &mut [Change]) {
    for i in 1..list.len() {
        let mut j = i;
        while j > 0 && list.get(j - 1).map(|c| (c.0, c.1)) > list.get(j).map(|c| (c.0, c.1)) {
            list.swap(j - 1, j);
            j -= 1;
        }
    }
}

fn order_many(list: &mut Vec<Change>) {
    crate::order::sort_by(list, |a, b| (a.0, a.1).cmp(&(b.0, b.1)));
}

impl PageEdits {
    /// The lists of one hit are in order already but for the fonts, of which there are few; take out repeats.
    fn seal_one(&mut self) {
        self.shows.dedup();
        for i in 1..self.fallbacks.len() {
            let mut j = i;
            while j > 0 && self.fallbacks.get(j - 1) > self.fallbacks.get(j) {
                self.fallbacks.swap(j - 1, j);
                j -= 1;
            }
        }
        self.fallbacks.dedup();
    }

    /// Put the lists in order (what came first stays first among equals) and take out repeats.
    fn seal(&mut self) {
        crate::order::sort_by_key(&mut self.deleted, |e| u64::from(e.0) << 32 | u64::from(e.1));
        self.deleted.dedup_by_key(|e| (e.0, e.1));
        crate::order::sort_by_key(&mut self.inserted, |e| u64::from(e.0) << 32 | u64::from(e.1));
        crate::order::sort_by_key(&mut self.shows, |&s| u64::from(s));
        self.shows.dedup();
        crate::order::sort_by(&mut self.fallbacks, |a, b| a.cmp(b));
        self.fallbacks.dedup();
        crate::order::sort_by_key(&mut self.spans, |e| u64::from(e.0));
        let mut merged: Vec<(u32, Option<String>)> = Vec::with_capacity(self.spans.len());
        for e in std::mem::take(&mut self.spans) {
            match merged.last_mut() {
                Some(last) if last.0 == e.0 => last.1 = None,
                _ => merged.push(e),
            }
        }
        self.spans = merged;
    }
}

/// The entries of a list sorted by their first number that have it equal to `key`.
fn span<T>(list: &[T], key: u32, of: impl Fn(&T) -> u32) -> &[T] {
    let (a, b) = (list.partition_point(|e| of(e) < key), list.partition_point(|e| of(e) <= key));
    list.get(a..b).unwrap_or(&[])
}

/// What a run of changes keeps between pages. `MANY`: the command line's, over many hits and pages (copies of forms and resources
/// are shared between pages, a substitute font that could not be made is remembered); the reader's changes one line, and that
/// code is not in it.
struct Run<'a, const MANY: bool> {
    scanner: EditScanner<'a>,
    fonts: Vec<FontCtx<MANY>>,
    font_index: ByKey<usize, usize, MANY>,
    fallbacks: Vec<Fallback<MANY>>,
    /// Substitute fonts that could not be made, and why.
    failed: Vec<(FallbackKey, String)>,
    /// The characters of the new text.
    new_chars: std::collections::BTreeSet<char>,
    /// The sets of those that a page font lacks (a substitute font is made for them only).
    missing_sets: Vec<Vec<char>>,
    /// Copies made of resources that pages share (or inherit): (inherited, address or number) -> the copy.
    res_copies: Vec<((bool, usize), ObjRef)>,
    /// Copies made of forms, by the form, a hash of what the copy holds and how long that is. A form that many pages draw and get
    /// the same change in is copied once.
    form_copies: Vec<((ObjRef, u64, usize), ObjRef)>,
    done: Vec<u32>,
    hits: usize,
    /// Places the text to find was met, whether or not it could be changed.
    tried: usize,
    skipped: Vec<(String, usize)>,
    wider: (usize, f64),
}

/// Why a hit cannot be made if the substitute font cannot (a limit of the font, not of the room: that one ends the run).
fn font_trouble(e: Error) -> std::result::Result<String, Error> {
    match e {
        Error::Invalid(m) => Ok(m),
        Error::Limit(m) if m != TOO_BIG => Ok(m),
        e => Err(e),
    }
}

impl<'a, const MANY: bool> Run<'a, MANY> {
    fn new(doc: &'a Document, new_text: &str) -> Run<'a, MANY> {
        Run {
            scanner: EditScanner::new(doc),
            fonts: Vec::new(),
            font_index: ByKey::new(),
            fallbacks: Vec::new(),
            failed: Vec::new(),
            new_chars: crate::order::set_of(new_text.chars()),
            missing_sets: Vec::new(),
            res_copies: Vec::new(),
            form_copies: Vec::new(),
            done: Vec::new(),
            hits: 0,
            tried: 0,
            skipped: Vec::new(),
            wider: (0, 0.0),
        }
    }

    fn skip(&mut self, why: &str) {
        match self.skipped.iter_mut().find(|s| s.0 == why) {
            Some(s) => s.1 += 1,
            None => self.skipped.push((why.to_string(), 1)),
        }
    }

    fn font_ctx(&mut self, rec: &FontRec) -> usize {
        let key = Rc::as_ptr(&rec.font) as usize;
        if let Some(&i) = self.font_index.get(key) {
            return i;
        }
        self.fonts.push(FontCtx { font: Rc::clone(&rec.font), entry: rec.entry.clone(), probe: None, tried: false, request: None, matrix: None, known: ByKey::new(), missing: None });
        self.font_index.insert(key, self.fonts.len() - 1);
        self.fonts.len() - 1
    }

    /// `c` as the page's font shows it, if the font has a glyph for it.
    fn encode(&mut self, ctx: &Ctx<'_>, fi: usize, c: char) -> Result<Option<Enc>> {
        let Some(f) = self.fonts.get_mut(fi) else { return Ok(None) };
        if let Some(known) = f.known.get(c) {
            return Ok(known.clone());
        }
        charge(ctx, ENCODE_COST)?;
        if !f.tried {
            f.tried = true;
            charge(ctx, FONT_COST)?;
            if let Ok(Object::Dict(d)) = ctx.doc.resolve(&f.entry) {
                let probe = GlyphProbe::new(ctx.doc, &d, &f.font);
                f.request = Some(probe.request());
                if f.font.type3 {
                    f.matrix = Some(type3_matrix(ctx.doc, &d));
                }
                f.probe = Some(probe);
            }
        }
        let mut found = None;
        for (bytes, coded) in f.font.codes_for(c) {
            let adv = coded.vertical.map_or(coded.w0, |v| v.0);
            // A Type 3 font has the glyph when it gives the code a width; the others are asked.
            let has = if f.font.type3 { adv.abs() > 1.0e-6 } else { f.probe.as_mut().is_some_and(|p| p.has(ctx.doc, &coded, c)) };
            // (A space in a font made for TeX, say, has a code and no width: it moves nothing.)
            if has && (!c.is_whitespace() || adv.abs() > 1.0e-6) {
                found = Some(Enc { adv, space: bytes == [32], bytes });
                break;
            }
        }
        if found.is_none() && c.is_whitespace() {
            found = Some(Enc { bytes: Vec::new(), adv: if u32::from(c) == 0x3000 { 1.0 } else { 0.25 }, space: false });
        }
        f.known.insert(c, found.clone());
        Ok(found)
    }

    /// Which characters of the new text the page's font `fi` lacks (an index of `missing_sets`).
    fn missing_of(&mut self, ctx: &Ctx<'_>, fi: usize) -> Result<usize> {
        if let Some(id) = self.fonts.get(fi).and_then(|f| f.missing) {
            return Ok(id);
        }
        let wanted: Vec<char> = self.new_chars.iter().copied().collect();
        let mut lacks: Vec<char> = Vec::new();
        for c in wanted {
            if self.encode(ctx, fi, c)?.is_none() {
                lacks.push(c);
            }
        }
        let id = match self.missing_sets.iter().position(|m| *m == lacks) {
            Some(i) => i,
            None => {
                self.missing_sets.push(lacks);
                self.missing_sets.len() - 1
            }
        };
        if let Some(f) = self.fonts.get_mut(fi) {
            f.missing = Some(id);
        }
        Ok(id)
    }

    /// The subset of a system font that looks like the page's font `fi` and holds the characters it lacks (`missing`), made on the
    /// first call. A font that cannot be made is said so for the other hits that want it.
    fn fallback(&mut self, ctx: &mut Ctx<'_>, fi: usize, vertical: bool, missing: usize) -> Result<usize> {
        let (request, matrix) = match self.fonts.get(fi) {
            Some(f) => (f.request.unwrap_or_else(|| crate::render::sysfont::request_for(b"", 0, 0.0, 0.0, None)), f.matrix),
            None => return Err(Error::Invalid(NOT_THERE.to_string())),
        };
        let key = (request, vertical, matrix.map(|m| m.map(f64::to_bits)), missing);
        if let Some(i) = self.fallbacks.iter().position(|f| f.key == key) {
            return Ok(i);
        }
        if MANY && let Some((_, why)) = self.failed.iter().find(|(k, _)| *k == key) {
            return Err(Error::Invalid(why.clone()));
        }
        charge(ctx, FONT_COST)?;
        let chars: std::collections::BTreeSet<char> = self.missing_sets.get(missing).map(|m| crate::order::set_of(m.iter().copied())).unwrap_or_default();
        let (from, next) = (ctx.objects.len(), ctx.next);
        match Self::make_fallback(ctx, &chars, request, matrix, vertical) {
            Ok((font_ref, codes)) => {
                ctx.tally(from)?;
                self.fallbacks.push(Fallback { key, font: font_ref, codes });
                Ok(self.fallbacks.len() - 1)
            }
            Err(e) => {
                // (Whatever was made before it went wrong is not kept.)
                ctx.objects.truncate(from);
                ctx.next = next;
                let why = font_trouble(e)?;
                if MANY {
                    self.failed.push((key, why.clone()));
                }
                Err(Error::Invalid(why))
            }
        }
    }

    #[allow(clippy::type_complexity)]
    fn make_fallback(ctx: &mut Ctx<'_>, chars: &std::collections::BTreeSet<char>, request: Request, matrix: Option<[f64; 6]>, vertical: bool) -> Result<(ObjRef, ByKey<char, (Vec<u8>, f64), MANY>)> {
        let font = fontsub::choose_like(chars, request)?;
        match matrix {
            Some(m) => {
                let (font_ref, codes) = fontsub::add_type3(ctx, &font, chars, m)?;
                let mut by = ByKey::new();
                for (c, b, a) in codes {
                    by.insert(c, (b, a));
                }
                Ok((font_ref, by))
            }
            None => {
                let emb = fontsub::embed(&font, chars, ctx.work)?;
                let font_ref = fontsub::add_objects_in(ctx, &emb, vertical)?;
                let mut by = ByKey::new();
                for (&c, &cid) in &emb.cid {
                    by.insert(c, (cid.to_be_bytes().to_vec(), if vertical { -1.0 } else { emb.width(c) / 1000.0 }));
                }
                Ok((font_ref, by))
            }
        }
    }

    /// Plan one hit: take its characters out and put `text` where they were. `Some(reason)`: it cannot be done, and nothing was
    /// planned.
    fn plan(&mut self, ctx: &mut Ctx<'_>, scan: &Scan, edits: &mut PageEdits, hit: &Hit, text: &str) -> Result<Option<String>> {
        charge(ctx, HIT_COST + 150.0 * hit.del.len() as f64 + CHAR_COST * text.chars().count() as f64)?;
        if let Some(why) = hit.del.iter().chain([&hit.anchor]).find_map(|&g| not_editable(scan, g)) {
            return Ok(Some(why.to_string()));
        }
        let at = hit.del.first().copied().unwrap_or(hit.anchor);
        let (Some(rec), Some(glyph)) = (scan.chars.get(at), scan.glyphs.get(at)) else { return Ok(Some(NOT_THERE.to_string())) };
        let Some(show) = scan.parts.get(rec.part as usize).and_then(|p| scan.shows.get(p.show as usize)) else { return Ok(Some(NOT_THERE.to_string())) };
        let Some(font_rec) = scan.fonts.get(show.font as usize) else { return Ok(Some(NOT_THERE.to_string())) };
        let spans = match span_changes(scan, hit, text) {
            Ok(s) => s,
            Err(why) => return Ok(Some(why)),
        };
        let fi = self.font_ctx(font_rec);
        let vertical = glyph.vertical;
        // The new text, in the page's font where it has the glyph and in a fallback font where it does not.
        let mut frags: Vec<Frag> = Vec::new();
        let mut used: Vec<usize> = Vec::new();
        // How far it runs: in points of the page (for telling the person), and in text space (for what follows it).
        let (mut new_width, mut new_tx) = (0.0f64, 0.0f64);
        let step = |adv: f64, space: bool| {
            let sp = if space { show.tw } else { 0.0 };
            if vertical { adv * show.size + show.tc + sp } else { (adv * show.size + show.tc + sp) * show.th }
        };
        for c in text.chars() {
            let t = match self.encode(ctx, fi, c)? {
                Some(enc) if enc.bytes.is_empty() => {
                    frags.push(Frag::Gap(enc.adv * 1000.0 * if vertical { 1.0 } else { -1.0 }));
                    enc.adv * show.size * if vertical { 1.0 } else { show.th }
                }
                Some(enc) => {
                    let t = step(enc.adv, enc.space);
                    match frags.last_mut() {
                        Some(Frag::Orig(b)) => b.extend_from_slice(&enc.bytes),
                        _ => frags.push(Frag::Orig(enc.bytes)),
                    }
                    t
                }
                None => {
                    let missing = self.missing_of(ctx, fi)?;
                    let fb = match self.fallback(ctx, fi, vertical, missing) {
                        Ok(i) => i,
                        Err(e) => return font_trouble(e).map(Some),
                    };
                    let Some((code, adv)) = self.fallbacks.get(fb).and_then(|f| f.codes.get(c)) else {
                        return Ok(Some("the substitute font lacks a glyph".to_string()));
                    };
                    match frags.last_mut() {
                        Some(Frag::Fall(i, b)) if *i == fb => b.extend_from_slice(code),
                        _ => frags.push(Frag::Fall(fb, code.clone())),
                    }
                    if !used.contains(&fb) {
                        used.push(fb);
                    }
                    step(*adv, false)
                }
            };
            new_width += t.abs() * show.scale.abs();
            new_tx += t;
        }
        let old_width: f64 = hit.del.iter().filter_map(|&g| scan.glyphs.get(g)).map(|g| g.adv.abs()).sum();
        if new_width > old_width + 1.0 {
            self.wider = (self.wider.0 + 1, self.wider.1.max(new_width - old_width));
        }
        // What follows the replaced text in the operator stays where it was (not for vertical writing, nor where the widths of the
        // font are a guess, as for a standard font without /Widths: the text would then stand where the guess puts it).
        if !vertical && !font_rec.font.guessed_widths() && let Some(n) = keep_number(scan, hit, new_tx) {
            frags.push(Frag::Keep(n));
        }
        // The text is kept until the page is written: it counts against the room and the size of a stream from here on.
        let held = 48 + frags.iter().map(|f| match f { Frag::Orig(b) | Frag::Fall(_, b) => b.len() + 32, _ => 16 }).sum::<usize>();
        edits.bytes = edits.bytes.saturating_add(held);
        if edits.bytes > MAX_STREAM_BYTES {
            return Err(Error::Limit("the content stream would be too big".to_string()));
        }
        ctx.spend(held)?;
        // Where the characters were, and where the new text goes: before the first, or next to the anchor.
        let id = u32::try_from(edits.hits).unwrap_or(u32::MAX);
        if let Some(p) = scan.parts.get(rec.part as usize) {
            edits.shows.push(p.show);
        }
        for r in hit.del.iter().filter_map(|&g| scan.chars.get(g)) {
            edits.deleted.push((r.part, r.off, r.len, id));
            if let Some(p) = scan.parts.get(r.part as usize) {
                edits.shows.push(p.show);
            }
        }
        if !frags.is_empty() {
            let off = if hit.after { rec.off + u32::from(rec.len) } else { rec.off };
            edits.inserted.push((rec.part, off, frags));
        }
        edits.fallbacks.extend(used.into_iter().map(|fb| (show.src, fb)));
        edits.spans.extend(spans);
        edits.hits += 1;
        Ok(None)
    }

    /// The statement that takes the place of the operator `si` once the page's edits are made.
    fn render_show(&self, scan: &Scan, si: u32, edits: &PageEdits) -> Vec<u8> {
        let Some(show) = scan.shows.get(si as usize) else { return Vec::new() };
        let mut pieces: Vec<Piece<'_>> = Vec::new();
        for item in &show.items {
            match item {
                ShowItem::Num(n) => pieces.push(Piece::Num(*n)),
                ShowItem::Str(p) => {
                    let Some(part) = scan.parts.get(*p as usize) else { continue };
                    let mut pos = 0usize;
                    let (dels, inss) = (span(&edits.deleted, *p, |d| d.0), span(&edits.inserted, *p, |d| d.0));
                    let (mut d, mut i) = (0usize, 0usize);
                    loop {
                        let (nd, ni) = (dels.get(d).map(|e| e.1 as usize), inss.get(i).map(|e| e.1 as usize));
                        let Some(at) = nd.into_iter().chain(ni).min() else { break };
                        keep(&mut pieces, part.bytes.get(pos..at).unwrap_or(&[]));
                        pos = pos.max(at);
                        if ni == Some(at) {
                            if let Some((_, _, frags)) = inss.get(i) {
                                pieces.push(Piece::New(frags));
                            }
                            i += 1;
                        }
                        if nd == Some(at) {
                            let (len, hit) = dels.get(d).map_or((0, 0), |e| (usize::from(e.2), e.3));
                            pos = at + len;
                            // (The numbers between two cut places of one hit, the spacing inside the text that went, go with it.)
                            let nums = pieces.iter().rev().take_while(|p| matches!(p, Piece::Num(_))).count();
                            if matches!(pieces.len().checked_sub(nums + 1).and_then(|k| pieces.get(k)), Some(Piece::Cut(h)) if *h == hit) {
                                pieces.truncate(pieces.len() - nums);
                            } else {
                                pieces.push(Piece::Cut(hit));
                            }
                            d += 1;
                        }
                    }
                    keep(&mut pieces, part.bytes.get(pos..).unwrap_or(&[]));
                }
            }
        }
        // Whether text of the operator follows each piece (the number that keeps it in place is for that text).
        let mut follow: Vec<bool> = Vec::with_capacity(pieces.len());
        let mut seen = false;
        for piece in pieces.iter().rev() {
            follow.push(seen);
            seen |= match piece {
                Piece::Bytes(_) => true,
                Piece::New(frags) => frags.iter().any(|f| !matches!(f, Frag::Keep(_))),
                _ => false,
            };
        }
        follow.reverse();

        let font = scan.fonts.get(show.font as usize);
        let tf = |out: &mut Vec<u8>, name: &[u8], size: f64| {
            put_name(out, name);
            out.push(b' ');
            out.extend_from_slice(num(size).as_bytes());
            out.extend_from_slice(b" Tf\n");
        };
        let mut out: Vec<u8> = Vec::new();
        match show.op {
            ShowOp::Quote => out.extend_from_slice(b"T*\n"),
            ShowOp::DoubleQuote => {
                if let Some((aw, ac)) = show.spacing {
                    for (v, op) in [(aw, " Tw "), (ac, " Tc ")] {
                        out.extend_from_slice(num(v).as_bytes());
                        out.extend_from_slice(op.as_bytes());
                    }
                    out.extend_from_slice(b"T*\n");
                }
            }
            _ => {}
        }
        let mut items: Vec<Item> = Vec::new();
        let push_string = |items: &mut Vec<Item>, bytes: &[u8]| match items.last_mut() {
            Some(Item::Str(b)) => b.extend_from_slice(bytes),
            _ => items.push(Item::Str(bytes.to_vec())),
        };
        let flush = |out: &mut Vec<u8>, items: &mut Vec<Item>| {
            match items.as_slice() {
                [] => return,
                [Item::Str(s)] => {
                    put_string(out, s);
                    out.extend_from_slice(b" Tj\n");
                }
                _ => {
                    out.push(b'[');
                    for (k, item) in items.iter().enumerate() {
                        if k > 0 {
                            out.push(b' ');
                        }
                        match item {
                            Item::Str(s) => put_string(out, s),
                            Item::Num(n) => out.extend_from_slice(num(*n).as_bytes()),
                        }
                    }
                    out.extend_from_slice(b"] TJ\n");
                }
            }
            items.clear();
        };
        for (piece, &follows) in pieces.iter().zip(&follow) {
            match piece {
                Piece::Bytes(b) => push_string(&mut items, b),
                Piece::Num(n) => items.push(Item::Num(*n)),
                Piece::Cut(_) => {}
                Piece::New(frags) => {
                    for frag in *frags {
                        match frag {
                            Frag::Gap(n) => items.push(Item::Num(*n)),
                            Frag::Keep(n) => {
                                if follows {
                                    items.push(Item::Num(*n));
                                }
                            }
                            Frag::Orig(b) => push_string(&mut items, b),
                            Frag::Fall(fb, b) => {
                                flush(&mut out, &mut items);
                                if let Some(f) = self.fallbacks.get(*fb) {
                                    tf(&mut out, &f.name(), show.size);
                                    put_string(&mut out, b);
                                    out.extend_from_slice(b" Tj\n");
                                    if let Some(rec) = font {
                                        tf(&mut out, &rec.name, show.size);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        flush(&mut out, &mut items);
        if out.last() == Some(&b'\n') {
            out.pop();
        }
        out
    }

    /// Write the page's edits: new streams for what changed, the copies of forms, resources, and the page itself.
    fn write_page(&mut self, ctx: &mut Ctx<'_>, page: &Page, scan: &Scan, edits: &PageEdits) -> Result<()> {
        // The new statements: (content stream, start, end, bytes). They count against the room and the size of a stream as they are
        // made.
        let mut changes: Vec<Change> = Vec::new();
        let hold = |ctx: &mut Ctx<'_>, bytes: usize| -> Result<()> { ctx.spend(bytes + 64) };
        for &si in &edits.shows {
            if let Some(show) = scan.shows.get(si as usize) {
                let bytes = self.render_show(scan, si, edits);
                hold(ctx, bytes.len())?;
                changes.push((show.src, show.start as usize, show.end as usize, bytes));
            }
        }
        for (id, text) in &edits.spans {
            if let Some(sequence) = scan.spans.get(*id as usize) {
                let bytes = render_span(scan, sequence, text.as_deref())?;
                hold(ctx, bytes.len())?;
                changes.push((sequence.src, sequence.start as usize, sequence.end as usize, bytes));
            }
        }
        if MANY {
            order_many(&mut changes);
        } else {
            order_few(&mut changes);
        }
        let splice = |ctx: &Ctx<'_>, old: &[u8], from: usize, to: usize, list: &[Change]| -> Result<Vec<u8>> {
            let mut out = Vec::with_capacity(old.len());
            let mut pos = from;
            for (_, a, b, bytes) in list {
                if *a < pos || *b < *a || *b > to {
                    return Err(Error::Invalid("two changes of the page overlap".to_string()));
                }
                out.extend_from_slice(old.get(pos..*a).unwrap_or(&[]));
                out.extend_from_slice(bytes);
                pos = *b;
                if out.len() > MAX_STREAM_BYTES {
                    return Err(Error::Limit("the content stream would be too big".to_string()));
                }
            }
            out.extend_from_slice(old.get(pos..to).unwrap_or(&[]));
            charge(ctx, BYTE_COST * out.len() as f64)?;
            Ok(out)
        };
        let compress = |ctx: &mut Ctx<'_>, dict: &mut Dict, data: &[u8]| -> Result<Vec<u8>> {
            dict.set("Filter", Object::from("FlateDecode"));
            let packed = compress_to_vec_zlib(data, 6);
            ctx.spend(packed.len() + dict.approx_size() + 96)?;
            Ok(packed)
        };
        let fonts_of = |this: &Self, src: u32| -> Vec<(Vec<u8>, ObjRef)> {
            span(&edits.fallbacks, src, |e| e.0).iter().filter_map(|&(_, fb)| this.fallbacks.get(fb)).map(|f| (f.name(), f.font)).collect()
        };

        // The forms that change, the last drawn first (a form is drawn by one made before it): a copy of each, and the entry in
        // the resources of what draws it that points at the copy.
        let n = scan.sources.len();
        let mut dirty = vec![false; n];
        for c in &changes {
            if let Some(d) = dirty.get_mut(c.0 as usize) {
                *d = true;
            }
        }
        for i in (1..n).rev() {
            if dirty.get(i) == Some(&true)
                && let Some(p) = scan.sources.get(i).and_then(|s| dirty.get_mut(s.parent as usize))
            {
                *p = true;
            }
        }
        let mut copies: Vec<Option<ObjRef>> = vec![None; n];
        let drawn_by = |copies: &[Option<ObjRef>], i: usize| -> Vec<(Vec<u8>, ObjRef)> {
            (i + 1..n)
                .filter(|&j| scan.sources.get(j).is_some_and(|c| c.parent as usize == i))
                .filter_map(|j| Some((scan.sources.get(j)?.name.clone(), copies.get(j).copied().flatten()?)))
                .collect()
        };
        for i in (1..n).rev() {
            let Some(src) = scan.sources.get(i).filter(|_| dirty.get(i) == Some(&true)) else { continue };
            let data = splice(ctx, &src.content, 0, src.content.len(), span(&changes, i as u32, |c| c.0))?;
            let mut dict = src.dict.clone();
            for k in ["Length", "Filter", "DecodeParms", "DL"] {
                dict.remove(k);
            }
            // Its resources get the substitute fonts it uses and the copies of the forms it draws.
            let (fonts, forms) = (fonts_of(self, i as u32), drawn_by(&copies, i));
            // A copy that does not depend on the page (the form has resources of its own, or gets none) is the same for every page
            // that makes it the same: made once.
            let key = src.form.filter(|_| MANY && (src.dict.get("Resources").is_some() || (fonts.is_empty() && forms.is_empty()))).map(|form| {
                let names: Vec<(&[u8], u32)> = fonts.iter().chain(&forms).map(|(n, r)| (n.as_slice(), r.num)).collect();
                let mut h = fingerprint(&data, 0xcbf2_9ce4_8422_2325);
                for (name, num) in names {
                    h = fingerprint(&num.to_le_bytes(), fingerprint(name, h));
                }
                (form, h, data.len())
            });
            if let Some(k) = key
                && let Some(shared) = self.form_copies.get(self.form_copies.partition_point(|e| e.0 < k)).filter(|e| e.0 == k).map(|e| e.1)
            {
                if let Some(slot) = copies.get_mut(i) {
                    *slot = Some(shared);
                }
                continue;
            }
            if !fonts.is_empty() || !forms.is_empty() {
                let mut res = effective_resources(ctx.doc, page, scan, i);
                charge(ctx, BYTE_COST * res.approx_size() as f64)?;
                add_entries(ctx.doc, &mut res, "Font", &fonts)?;
                add_entries(ctx.doc, &mut res, "XObject", &forms)?;
                dict.set("Resources", Object::Dict(res));
            }
            let packed = compress(ctx, &mut dict, &data)?;
            let r = ctx.alloc()?;
            ctx.objects.push((r.num, Object::Stream(Stream { dict, data: packed })));
            if let Some(slot) = copies.get_mut(i) {
                *slot = Some(r);
            }
            if let Some(k) = key {
                let at = self.form_copies.partition_point(|e| e.0 < k);
                self.form_copies.insert(at, (k, r));
            }
        }

        // The page's own streams.
        let Object::Dict(mut pdict) = ctx.doc.get(page.obj_ref)? else { return Err(Error::Invalid("the page is not a dictionary".to_string())) };
        charge(ctx, BYTE_COST * pdict.approx_size() as f64)?;
        let mut replaced: Vec<(ObjRef, ObjRef)> = Vec::new();
        if let Some(src) = scan.sources.first() {
            let own = span(&changes, 0, |c| c.0);
            for &(from, to, old) in &src.streams {
                let Some(old) = old else { continue };
                let list = own.get(own.partition_point(|c| c.1 < from)..own.partition_point(|c| c.1 < to)).unwrap_or(&[]);
                if list.is_empty() {
                    continue;
                }
                let data = splice(ctx, &src.content, from, to, list)?;
                let mut dict = Dict::new();
                let packed = compress(ctx, &mut dict, &data)?;
                let r = ctx.alloc()?;
                ctx.objects.push((r.num, Object::Stream(Stream { dict, data: packed })));
                replaced.insert(replaced.partition_point(|e| e.0 < old), (old, r));
            }
        }
        let list: Vec<Object> = contents_list(ctx.doc, pdict.get("Contents"))
            .into_iter()
            .map(|o| match o.as_obj_ref().and_then(|r| replaced.get(replaced.partition_point(|e| e.0 < r)).filter(|e| e.0 == r)) {
                Some(&(_, new)) => Object::Ref(new),
                None => o,
            })
            .collect();
        let single = matches!(pdict.get("Contents"), Some(Object::Ref(r)) if ctx.doc.is_stream(*r));
        match (single, list.as_slice()) {
            (true, [one]) => pdict.set("Contents", one.clone()),
            _ => pdict.set("Contents", Object::Array(list)),
        }

        // The page's resources: the substitute fonts it uses itself and the copies of the forms it draws.
        let (fonts, forms) = (fonts_of(self, 0), drawn_by(&copies, 0));
        self.page_resources(ctx, page, &mut pdict, &fonts, &forms)?;
        ctx.spend(pdict.approx_size() + 48)?;
        ctx.objects.push((page.obj_ref.num, Object::Dict(pdict)));
        Ok(())
    }

    /// Add the fonts and form copies to the page's resources. A dictionary of the page's own is changed where it is; one that is an
    /// object, or that the page tree gives the page, is not (other pages use it): the page gets a copy, one for all the pages that
    /// need only fonts added to the same resources.
    fn page_resources(&mut self, ctx: &mut Ctx<'_>, page: &Page, pdict: &mut Dict, fonts: &[(Vec<u8>, ObjRef)], forms: &[(Vec<u8>, ObjRef)]) -> Result<()> {
        if fonts.is_empty() && forms.is_empty() {
            return Ok(());
        }
        if let Some(Object::Dict(res)) = pdict.get_mut("Resources") {
            add_entries(ctx.doc, res, "Font", fonts)?;
            return add_entries(ctx.doc, res, "XObject", forms);
        }
        let key = match pdict.get("Resources") {
            Some(Object::Ref(r)) => Some((false, r.num as usize)),
            Some(_) => None,
            None => page.inherited("Resources").map(|a| (true, a.id)),
        };
        let shareable = (MANY && forms.is_empty()).then_some(key).flatten();
        if let Some(k) = shareable
            && let Some(&(_, copy)) = self.res_copies.iter().find(|(have, _)| *have == k)
        {
            if let Some((_, Object::Dict(res))) = ctx.objects.iter_mut().find(|o| o.0 == copy.num) {
                add_entries(ctx.doc, res, "Font", fonts)?;
            }
            pdict.set("Resources", Object::Ref(copy));
            return Ok(());
        }
        let mut res = match pdict.get("Resources").cloned().or_else(|| page.resources().cloned()) {
            Some(o) => resolve_dict(ctx.doc, &o),
            None => Dict::new(),
        };
        charge(ctx, BYTE_COST * res.approx_size() as f64)?;
        add_entries(ctx.doc, &mut res, "Font", fonts)?;
        add_entries(ctx.doc, &mut res, "XObject", forms)?;
        ctx.spend(res.approx_size() + 48)?;
        match shareable {
            Some(k) => {
                let r = ctx.alloc()?;
                ctx.objects.push((r.num, Object::Dict(res)));
                self.res_copies.push((k, r));
                pdict.set("Resources", Object::Ref(r));
            }
            None => pdict.set("Resources", Object::Dict(res)),
        }
        Ok(())
    }

    /// Plan and write the hits of a page. The number of changes made.
    fn edit_page(&mut self, ctx: &mut Ctx<'_>, page: &Page, scan: &Scan, hits: &[Hit], text: &str) -> Result<usize> {
        if self.done.binary_search(&page.obj_ref.num).is_ok() {
            return Ok(0);
        }
        let mut edits = PageEdits::default();
        for hit in hits {
            if edits.hits >= MAX_HITS_PER_PAGE || self.hits.saturating_add(edits.hits) >= MAX_REPLACE_HITS {
                return Err(Error::Limit(format!("more than {MAX_HITS_PER_PAGE} changes on a page, or {MAX_REPLACE_HITS} in all")));
            }
            if let Some(why) = self.plan(ctx, scan, &mut edits, hit, text)? {
                self.skip(&why);
            }
        }
        if edits.hits == 0 {
            return Ok(0);
        }
        edits.seal();
        self.write_page(ctx, page, scan, &edits)?;
        let at = self.done.partition_point(|&d| d < page.obj_ref.num);
        if self.done.get(at) != Some(&page.obj_ref.num) {
            self.done.insert(at, page.obj_ref.num);
        }
        self.hits += edits.hits;
        Ok(edits.hits)
    }
}

// --- helpers -------------------------------------------------------------------------------------------------------

/// The `/FontMatrix` of a Type 3 font (the usual one when it has none).
fn type3_matrix(doc: &Document, font: &Dict) -> [f64; 6] {
    let mut m = [0.001, 0.0, 0.0, 0.001, 0.0, 0.0];
    if let Some(Object::Array(a)) = font.get("FontMatrix").and_then(|o| doc.resolve(o).ok())
        && a.len() == 6
    {
        for (slot, v) in m.iter_mut().zip(&a) {
            *slot = doc.resolve(v).ok().and_then(|v| v.as_f64()).filter(|v| v.is_finite()).unwrap_or(*slot);
        }
    }
    m
}

/// FNV-1a over `bytes`, going on from `h`.
fn fingerprint(bytes: &[u8], h: u64) -> u64 {
    bytes.iter().fold(h, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

fn put_name(out: &mut Vec<u8>, name: &[u8]) {
    out.push(b'/');
    for &b in name {
        if b <= b' ' || b >= 0x7F || b"#()<>[]{}/%".contains(&b) {
            out.extend_from_slice(format!("#{b:02X}").as_bytes());
        } else {
            out.push(b);
        }
    }
}

/// The marked-content sequences with an `/ActualText` that hold a hit, from the innermost out: (the sequence, the new text for its
/// `/ActualText` when the sequence holds the hit and nothing else; `None`: the entry goes, extraction would read the old text from
/// it). `Err(reason)`: one of them cannot be written again.
fn span_changes(scan: &Scan, hit: &Hit, text: &str) -> std::result::Result<Vec<(u32, Option<String>)>, String> {
    if scan.spans.is_empty() {
        return Ok(Vec::new());
    }
    let (lo, hi) = match (hit.del.first(), hit.del.last()) {
        (Some(&a), Some(&b)) => (a, b),
        _ => (hit.anchor, hit.anchor),
    };
    let mut ids: Vec<u32> = Vec::new();
    let mut previous = None;
    for g in lo..=hi {
        let here = scan.span_at(g);
        if here == previous {
            continue;
        }
        previous = here;
        let mut at = here;
        while let Some(i) = at {
            ids.push(u32::try_from(i).unwrap_or(u32::MAX));
            at = scan.spans.get(i).map(|s| s.parent as usize).filter(|&p| p < scan.spans.len());
        }
    }
    crate::order::sort_by_key(&mut ids, |&i| u64::from(i));
    ids.dedup();
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let Some(sequence) = scan.spans.get(id as usize) else { continue };
        if let Some(why) = place_problem(scan, sequence.src, sequence.start as usize, sequence.end as usize) {
            return Err(format!("the text is in a marked-content sequence with an /ActualText that cannot be changed: {why}"));
        }
        let exact = !hit.del.is_empty() && sequence.first as usize == lo && sequence.last as usize == hi + 1;
        out.push((id, (exact && !text.is_empty()).then(|| text.to_string())));
    }
    Ok(out)
}

/// The `BDC` statement of a sequence once its `/ActualText` is `text` (`None`: taken out; a sequence left with no properties is
/// a `BMC`).
fn render_span(scan: &Scan, sequence: &ActualSpan, text: Option<&str>) -> Result<Vec<u8>> {
    let old = scan.sources.get(sequence.src as usize).and_then(|s| s.content.get(sequence.start as usize..sequence.end as usize)).unwrap_or_default();
    let mut parser = crate::parser::Parser::new(old, 0);
    let (Ok(Object::Name(tag)), Ok(Object::Dict(mut props))) = (parser.parse_object(), parser.parse_object()) else { return Ok(old.to_vec()) };
    // (Not the sequence that was thought: one that has no /ActualText stays as it is.)
    if props.remove("ActualText").is_none() {
        return Ok(old.to_vec());
    }
    if let Some(t) = text {
        props.set("ActualText", text_string(t));
    }
    let mut out = Vec::new();
    put_name(&mut out, &tag.0);
    if props.is_empty() {
        out.extend_from_slice(b" BMC");
    } else {
        out.push(b' ');
        crate::writer::write_object(&mut out, &Object::Dict(props))?;
        out.extend_from_slice(b" BDC");
    }
    Ok(out)
}

/// The number of a `TJ` that, put after new text which runs `new_tx` (in text space, with the horizontal scaling) in the place of the
/// glyphs of `hit`, leaves what follows where it was: the difference of the two, in thousandths of the size. Only for glyphs of one
/// operator (their way is then the glyphs' own); `None` when there is nothing to make up for.
fn keep_number(scan: &Scan, hit: &Hit, new_tx: f64) -> Option<f64> {
    let (lo, hi) = (*hit.del.first()?, *hit.del.last()?);
    let (a, b) = (scan.glyphs.get(lo)?, scan.glyphs.get(hi)?);
    let show_of = |g: usize| scan.chars.get(g).and_then(|r| scan.parts.get(r.part as usize)).map(|p| p.show);
    let show = scan.shows.get(show_of(lo).filter(|&s| Some(s) == show_of(hi))? as usize)?;
    let along = (b.x + b.adv * b.dx - a.x) * a.dx + (b.y + b.adv * b.dy - a.y) * a.dy;
    let per = show.size * show.th;
    if show.scale <= 0.0 || per == 0.0 {
        return None;
    }
    let n = (new_tx - along / show.scale) * 1000.0 / per;
    (n.is_finite() && n.abs() > 1.0e-3).then_some(n)
}

fn put_string(out: &mut Vec<u8>, bytes: &[u8]) {
    if bytes.iter().all(|b| (0x20..0x7F).contains(b)) {
        out.push(b'(');
        for &b in bytes {
            if matches!(b, b'(' | b')' | b'\\') {
                out.push(b'\\');
            }
            out.push(b);
        }
        out.push(b')');
    } else {
        out.push(b'<');
        for b in bytes {
            out.extend(b"0123456789ABCDEF".get(usize::from(b >> 4)).into_iter().chain(b"0123456789ABCDEF".get(usize::from(b & 15))));
        }
        out.push(b'>');
    }
}

fn resolve_dict(doc: &Document, o: &Object) -> Dict {
    match doc.resolve(o) {
        Ok(Object::Dict(d)) => d,
        _ => Dict::new(),
    }
}

/// The resources the content of source `i` is drawn with: its own, else those of what draws it.
fn effective_resources(doc: &Document, page: &Page, scan: &Scan, i: usize) -> Dict {
    let mut at = i;
    for _ in 0..64 {
        if at == 0 {
            return page.resources().map(|r| resolve_dict(doc, r)).unwrap_or_default();
        }
        let Some(src) = scan.sources.get(at) else { break };
        if let Some(own) = src.dict.get("Resources") {
            return resolve_dict(doc, own);
        }
        at = src.parent as usize;
    }
    Dict::new()
}

/// Put `entries` in the dictionary `category` of `res` (made, or read out of its object, when it is not there as a dictionary).
fn add_entries(doc: &Document, res: &mut Dict, category: &str, entries: &[(Vec<u8>, ObjRef)]) -> Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    let mut sub = match res.get(category) {
        Some(Object::Dict(d)) => d.clone(),
        Some(o) => resolve_dict(doc, o),
        None => Dict::new(),
    };
    for (name, r) in entries {
        if let Some(have) = sub.iter().find(|(k, _)| k.0 == *name).map(|(_, v)| v)
            && *have != Object::Ref(*r)
            && category == "Font"
        {
            return Err(Error::Invalid("the page's resources already use the name of the substitute font".to_string()));
        }
        sub.set(Name(name.clone()), Object::Ref(*r));
    }
    res.set(category, Object::Dict(sub));
    Ok(())
}

// --- the reader: change a line -----------------------------------------------------------------------------------------

/// Change the line of text at `at` (points of the page as shown), which reads `old`, into `text`.
pub(super) fn replace_line(ctx: &mut Ctx<'_>, page: &Page, at: [f32; 2], old: &str, text: &str) -> Result<()> {
    if let Some(why) = bad_text(text) {
        return Err(Error::Invalid(why.to_string()));
    }
    let mut run = Run::<false>::new(ctx.doc, text);
    let scan = {
        let _budget = ctx.doc.page_decode_budget();
        run.scanner.scan(page, ctx.work)?
    };
    let shown = Shown::of(page);
    let stale = || Error::Invalid("the text of the line is not what it was; click the line again".to_string());
    let line = line_at(&scan, &shown, (f64::from(at[0]), f64::from(at[1]))).ok_or_else(stale)?;
    let (ls, le) = line.bytes;
    let current = scan.text.get(ls..le).ok_or_else(stale)?;
    if current != old {
        return Err(stale());
    }
    if current == text {
        return Err(Error::Invalid("the text is the same".to_string()));
    }
    // What stays at both ends of the line is left alone; what is between is replaced, cut at the borders of the marks.
    let (o, n): (Vec<char>, Vec<char>) = (current.chars().collect(), text.chars().collect());
    let prefix = o.iter().zip(&n).take_while(|(a, b)| a == b).count();
    let suffix = o.iter().rev().zip(n.iter().rev()).take(o.len().min(n.len()) - prefix).take_while(|(a, b)| a == b).count();
    let byte_at = |chars: usize| ls + current.char_indices().nth(chars).map_or(current.len(), |(i, _)| i);
    let (mut a, mut b) = (byte_at(prefix), byte_at(o.len() - suffix));
    for m in scan.marks.get(line.marks.0..line.marks.1).unwrap_or(&[]) {
        if m.start < a && a < m.end {
            a = m.start;
        }
        if m.start < b && b < m.end {
            b = m.end;
        }
    }
    let keep_head = scan.text.get(ls..a).map_or(0, |s| s.chars().count());
    let keep_tail = scan.text.get(b..le).map_or(0, |s| s.chars().count());
    let middle: String = n.get(keep_head..n.len().saturating_sub(keep_tail)).unwrap_or(&[]).iter().collect();
    let marked = mark_of(&scan);
    let hit = match marks_in(&scan, a, b) {
        Some((first, last)) if first < last => hit_of(ctx, &scan, &marked, first, last)?,
        Some((first, _)) => {
            // Nothing is taken out: the new text goes before the mark that starts the rest of the line, or after the last one.
            let end = line.marks.1;
            match scan.marks.get(first).filter(|_| first < end) {
                Some(m) => Some(Hit { del: Vec::new(), anchor: m.glyph, after: false }),
                None => scan.marks.get(end.saturating_sub(1)).map(|m| Hit { del: Vec::new(), anchor: m.glyph, after: true }),
            }
        }
        None => None,
    };
    let Some(hit) = hit else { return Err(Error::Invalid("this part of the line cannot be changed on its own".to_string())) };
    if middle.is_empty() && hit.del.is_empty() {
        return Err(Error::Invalid("the text is the same".to_string()));
    }
    run.new_chars = crate::order::set_of(middle.chars());
    let mut edits = PageEdits::default();
    if let Some(why) = run.plan(ctx, &scan, &mut edits, &hit, &middle)? {
        return Err(Error::Invalid(why));
    }
    edits.seal_one();
    run.write_page(ctx, page, &scan, &edits)?;
    if run.wider.0 > 0 {
        ctx.notes.push(format!("the new text is wider than the old by {:.0} points; it may run into what follows", run.wider.1));
    }
    Ok(())
}

// --- the command line: find and replace --------------------------------------------------------------------------------

/// What `qingpdf replace` did.
#[derive(Debug)]
pub struct Replaced {
    /// The bytes of the new file (the whole original, then one incremental update); nothing when `count` is 0.
    pub data: Vec<u8>,
    /// The file's structure was damaged, so it was written afresh first and the original bytes are not in the result.
    pub rewritten: bool,
    /// How many places were changed, on how many pages.
    pub count: usize,
    pub pages: usize,
    /// Places found that could not be changed, and why.
    pub skipped: Vec<(String, usize)>,
    pub warnings: Vec<String>,
    /// What the work meter was charged, in units of about a nanosecond.
    pub work_used: f64,
}

/// Replace `find` by `with` on `pages` (from 0; all of them when `None`) of `doc`: the text of a line of the page as `qingpdf text`
/// gives it, whole characters, case as written.
pub fn replace_text(doc: &Document, password: &str, find: &str, with: &str, pages: Option<&[usize]>) -> Result<Replaced> {
    if find.is_empty() {
        return Err(Error::Invalid("there is nothing to find".to_string()));
    }
    if let Some(why) = bad_text(find).or_else(|| bad_text(with)) {
        return Err(Error::Invalid(why.to_string()));
    }
    if find == with {
        return Err(Error::Invalid("the text to find and the text to put there are the same".to_string()));
    }
    check_content_allowed(doc, password)?;
    let rewritten = doc.was_repaired() || doc.leading_junk() > 0;
    let fresh = if rewritten { Some(Document::from_bytes_with_password(crate::ops::copy_all(doc)?.data, password)?) } else { None };
    let doc = fresh.as_ref().unwrap_or(doc);
    let all = doc.pages()?;
    let chosen: Vec<usize> = match pages {
        Some(list) => list.to_vec(),
        None => (0..all.len()).collect(),
    };
    if let Some(bad) = chosen.iter().find(|&&p| p >= all.len()) {
        return Err(Error::Invalid(format!("there is no page {} (the file has {})", bad + 1, all.len())));
    }
    let total = Work::with_allowance(RUN_PAGES * PAGE_WORK);
    let mut ctx = Ctx { doc, next: doc.next_free_number(), objects: Vec::new(), work: &total, room: CLI_ROOM, used: 0, notes: Vec::new() };
    let mut run = Run::<true>::new(doc, with);
    let (mut count, mut changed) = (0usize, 0usize);
    let mut warnings: Vec<String> = Vec::new();
    for &index in &chosen {
        let Some(page) = all.get(index) else { continue };
        let own = Work::with_allowance(PAGE_WORK);
        let scan = {
            let _budget = doc.page_decode_budget();
            run.scanner.scan(page, &own)
        };
        total.spend(own.used());
        let scan = match scan {
            Ok(s) => s,
            Err(Error::Limit(m)) => {
                warnings.push(format!("page {} was not searched: {m}", index + 1));
                continue;
            }
            Err(e) => return Err(e),
        };
        if !scan.text.contains(find) {
            continue;
        }
        let marked = mark_of(&scan);
        let mut hits: Vec<Hit> = Vec::new();
        let mut tried = 0usize;
        for (a, _) in scan.text.match_indices(find) {
            // Every place the text is met counts, whether or not it can be changed.
            tried += 1;
            if tried > MAX_HITS_PER_PAGE || run.tried.saturating_add(tried) > MAX_REPLACE_HITS {
                return Err(Error::Limit(format!("more than {MAX_HITS_PER_PAGE} places to change on page {}, or {MAX_REPLACE_HITS} in all", index + 1)));
            }
            charge(&ctx, MATCH_COST)?;
            let found = match marks_in(&scan, a, a + find.len()).filter(|(first, last)| first < last) {
                Some((first, last)) => hit_of(&ctx, &scan, &marked, first, last)?,
                None => None,
            };
            match found {
                Some(h) => hits.push(h),
                None => run.skip("the text is cut by a glyph that stands for several characters, or is not drawn in one go"),
            }
        }
        run.tried += tried;
        let done = run.edit_page(&mut ctx, page, &scan, &hits, with)?;
        count += done;
        changed += usize::from(done > 0);
    }
    let skipped = std::mem::take(&mut run.skipped);
    if run.wider.0 > 0 {
        warnings.push(format!("{} new texts are wider than the old ones (the widest by {:.0} points); they may run into what follows", run.wider.0, run.wider.1));
    }
    if count == 0 {
        return Ok(Replaced { data: Vec::new(), rewritten, count, pages: 0, skipped, warnings, work_used: total.used() });
    }
    let changes = commit(doc, std::mem::take(&mut ctx.objects));
    let section = save_section(doc);
    replay(doc, &changes, false);
    let section = section?;
    let mut data = Vec::with_capacity(doc.data().len() + section.len());
    data.extend_from_slice(doc.data());
    data.extend_from_slice(&section);
    Ok(Replaced { data, rewritten, count, pages: changed, skipped, warnings, work_used: total.used() })
}
