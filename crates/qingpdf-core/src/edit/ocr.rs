//! Recognised text for scanned pages (5c): an invisible text layer put over the picture of the page.
//!
//! The words come from a recogniser that looked at the page drawn as a picture ([`OcrPage`]: boxes in pixels of that picture; the
//! recogniser itself is the window's business, the engine never calls it). This module chooses the pages worth recognising
//! ([`scan_state`], [`wants_text`]), the resolution to draw them at ([`ocr_dpi`]), and writes the words into the pages as text drawn
//! with text rendering mode 3, which paints nothing (ISO 32000-1 9.3.6, Table 106) but is text for everything that reads a page:
//! other readers search it and copy it, and so do our search, selection and the Word export.
//!
//! One edit makes the layer of all the pages: one subset of one system font that has every character used (4b, `fontsub`), shared by
//! the pages, and for each page one form XObject that holds its text and is drawn over the page's own content (4c, `content`). The
//! form carries a private key ([`OCR_MARK`]): a page that has such a form is done ([`has_layer`], however few characters it holds),
//! and `retext` (5a) leaves the text in it alone (it is the words of the picture, and the picture does not change). Each
//! word is placed at its box: the font size is the height of the line the word is in, the baseline sits where the reader's own
//! character boxes put it (`Shown::glyph_box`: 0.2 em below the baseline, 0.8 above), and the horizontal scaling `Tz` (9.3.4) is
//! worked out for each word so that its natural width in the font fills the box. A word that is followed by another in the same line
//! has a space after it that reaches the next word, so that the characters of a line cover it without a gap and the text reads with
//! its spaces; Chinese, Japanese and Korean words get none (their lines are not spaced).
//!
//! Places are the page as it is shown (the crop box turned by `/Rotate`): the picture is that page, so a box of the picture is a box of
//! the shown page scaled by (page size) / (picture size), and `content::upright` carries the shown page onto user space, whatever the
//! `/Rotate`, the origin of the boxes and the offset of the crop box are.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt::Write;

use miniz_oxide::deflate::compress_to_vec_zlib;

use super::appearance::num;
use super::content::{self, Shared};
use super::{Ctx, fontsub};
use crate::document::{Document, Page};
use crate::error::{Error, Result};
use crate::layout::Face;
use crate::object::{Dict, Object, Stream};
use crate::text::{PageLayout, Shown};

/// Most pages one run recognises (and one edit gives text to).
pub const MAX_OCR_PAGES: usize = 500;
/// Most words and characters of one page, and words of a whole run; more is a recogniser gone wrong, and the memory the words take
/// until they are written is held to about 20 MB.
pub const MAX_WORDS_PER_PAGE: usize = 30_000;
pub const MAX_CHARS_PER_PAGE: usize = 60_000;
pub const MAX_WORDS_PER_RUN: usize = 400_000;
/// Most characters of one word.
pub const MAX_WORD_CHARS: usize = 200;
/// Most pixels of the picture a page is drawn as (an A4 page at 300 dpi is 8.7 million), and the resolution aimed at (the Windows recogniser reads best at
/// about 300 dots per inch) and the least that is worth it.
pub const MAX_IMAGE_PIXELS: u64 = 9_000_000;
pub const TARGET_DPI: f64 = 300.0;
pub const MIN_OCR_DPI: f64 = 36.0;
/// A page with fewer visible characters than this has no text for the purpose of choosing scans (a page number stamped on a scan does
/// not make it a text page), and a scan is a page of which at least this part is pictures.
pub const MAX_SCAN_TEXT_CHARS: usize = 10;
pub const MIN_IMAGE_COVER: f32 = 0.5;
/// Most different characters in the layer's font (a subset holds 3000 glyphs, components of composite glyphs included); the ones used
/// least are left out of a run that has more.
const MAX_GLYPH_CHARS: usize = 2_500;
/// The smallest and biggest font size, in points, and the range of the horizontal scaling, in percent.
const MIN_SIZE: f64 = 1.0;
const MAX_SIZE: f64 = 500.0;
const MIN_TZ: f64 = 5.0;
const MAX_TZ: f64 = 1_000.0;
/// What the work meter is charged: per character and per word written, and per page.
const CHAR_COST: f64 = 250.0;
const WORD_COST: f64 = 600.0;
/// The private key (`true`) of the form that holds a page's layer: the layer is ours, so a page that has one is done, and the text in
/// it is not text to change (`retext`). The forms are named `Qo` and their object number in the page's resources.
pub const OCR_MARK: &str = "QingpdfOCR";
const FORM_PREFIX: &[u8] = b"Qo";
/// Most forms of a page looked at for the mark (a page has one layer; more is a file that was made to waste time).
const MAX_FORMS_LOOKED_AT: usize = 16;

/// A word the recogniser found: its text and its box `[left, top, right, bottom]` in pixels of the picture, from the top left.
#[derive(Clone, Debug, PartialEq)]
pub struct OcrWord {
    pub text: String,
    pub rect: [f32; 4],
}

/// The words of one line of the recogniser's, in reading order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OcrLine {
    pub words: Vec<OcrWord>,
}

/// What was recognised on one page: the picture was `width` by `height` pixels and showed the whole page as it is shown.
#[derive(Clone, Debug, PartialEq)]
pub struct OcrPage {
    pub page: u32,
    pub width: u32,
    pub height: u32,
    pub lines: Vec<OcrLine>,
}

impl OcrPage {
    /// How many words the page has.
    pub fn word_count(&self) -> usize {
        self.lines.iter().map(|l| l.words.len()).sum()
    }
}

// --- which pages, how big ----------------------------------------------------------------------------------------------

/// What decides whether a page is a scan.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScanState {
    /// Characters of the page's text that are not white space.
    pub text_chars: usize,
    /// The part of the page that pictures cover (0 to 1; pictures that overlap are counted twice, up to 1).
    pub image_cover: f32,
}

/// Look at what a page shows (a [`PageLayout`] of the text extractor).
pub fn scan_state(layout: &PageLayout) -> ScanState {
    let text_chars = layout.lines.iter().map(|l| l.text.chars().filter(|c| !c.is_whitespace()).count()).sum();
    let (w, h) = (f64::from(layout.size.0), f64::from(layout.size.1));
    let area = w * h;
    let covered: f64 = layout
        .images
        .iter()
        .map(|i| {
            let [l, t, r, b] = i.bbox.map(f64::from);
            (r.min(w) - l.max(0.0)).max(0.0) * (b.min(h) - t.max(0.0)).max(0.0)
        })
        .sum();
    let image_cover = if area > 0.0 && covered.is_finite() { (covered / area).min(1.0) as f32 } else { 0.0 };
    ScanState { text_chars, image_cover }
}

/// Is the page one to recognise? It has no text; and, when `needs_picture` (recognising every scanned page of a file), it is mostly
/// pictures. A page the person asked for by itself needs only to have no text. (A page that has our own layer is done whatever its
/// text: [`has_layer`].)
pub fn wants_text(state: &ScanState, needs_picture: bool) -> bool {
    state.text_chars < MAX_SCAN_TEXT_CHARS && (!needs_picture || state.image_cover >= MIN_IMAGE_COVER)
}

/// Does the page already have the layer of a run of ours (a form with the mark [`OCR_MARK`] among its resources)? However few
/// characters it holds: a page whose layer has three characters is done, and a second run would put a second layer over it.
pub fn has_layer(doc: &Document, page: &Page) -> bool {
    let Some(Object::Dict(resources)) = page.resources().and_then(|r| doc.resolve(r).ok()) else { return false };
    let Some(Object::Dict(forms)) = resources.get("XObject").and_then(|x| doc.resolve(x).ok()) else { return false };
    forms
        .iter()
        .filter(|(name, _)| name.0.starts_with(FORM_PREFIX))
        .take(MAX_FORMS_LOOKED_AT)
        .any(|(_, form)| matches!(doc.resolve(form), Ok(Object::Stream(s)) if matches!(s.dict.get(OCR_MARK), Some(Object::Bool(true)))))
}

/// The resolution to draw a page of `width_pt` by `height_pt` points at for recognition: [`TARGET_DPI`], less when the picture would
/// be longer than `max_side` pixels (what the recogniser takes) or have more than [`MAX_IMAGE_PIXELS`]. `None`: the page is so big
/// that it would be drawn below [`MIN_OCR_DPI`].
pub fn ocr_dpi(width_pt: f64, height_pt: f64, max_side: u32) -> Option<f64> {
    if !(width_pt > 0.0 && height_pt > 0.0 && width_pt.is_finite() && height_pt.is_finite()) {
        return None;
    }
    // (A little less than the limits, because the sides of the picture are rounded up.)
    let by_side = f64::from(max_side.max(1)) * 72.0 / width_pt.max(height_pt) * 0.995;
    let by_area = (MAX_IMAGE_PIXELS as f64 / (width_pt * height_pt)).sqrt() * 72.0 * 0.995;
    let dpi = TARGET_DPI.min(by_side).min(by_area);
    (dpi >= MIN_OCR_DPI).then_some(dpi)
}

// --- the words, in points of the shown page ------------------------------------------------------------------------------

/// A word on the shown page: `[left, top, right, bottom]` in points from the top left.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Word {
    pub text: String,
    pub rect: [f64; 4],
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Line {
    pub words: Vec<Word>,
}

/// Is `c` a character of a script that is written without spaces between words?
fn is_unspaced(c: char) -> bool {
    matches!(u32::from(c), 0x2E80..=0x9FFF | 0xA000..=0xA4CF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFFEF | 0x20000..=0x3FFFF)
}

/// The recogniser's words turned into points of the shown page of `width` by `height` points: control characters and white space
/// inside a word are left out, empty words and words with no area or no place (not numbers) are dropped, boxes are cut to the page.
pub(crate) fn tidy(page: &OcrPage, width: f64, height: f64) -> Result<Vec<Line>> {
    if page.width == 0 || page.height == 0 || !(width > 0.0 && height > 0.0) {
        return Err(Error::Invalid("the picture that was recognised has no size".to_string()));
    }
    if page.word_count() > MAX_WORDS_PER_PAGE {
        return Err(Error::Limit(format!("a page has at most {MAX_WORDS_PER_PAGE} recognised words")));
    }
    let (sx, sy) = (width / f64::from(page.width), height / f64::from(page.height));
    let (mut lines, mut chars) = (Vec::with_capacity(page.lines.len()), 0usize);
    for l in &page.lines {
        let mut words = Vec::with_capacity(l.words.len());
        for w in &l.words {
            let text: String = w.text.chars().filter(|c| !c.is_control() && !c.is_whitespace() && *c != '\u{FFFD}').take(MAX_WORD_CHARS).collect();
            let [a, b, c, d] = w.rect.map(f64::from);
            if text.is_empty() || ![a, b, c, d].iter().all(|v| v.is_finite()) {
                continue;
            }
            let (left, right) = ((a.min(c) * sx).clamp(0.0, width), (a.max(c) * sx).clamp(0.0, width));
            let (top, bottom) = ((b.min(d) * sy).clamp(0.0, height), (b.max(d) * sy).clamp(0.0, height));
            if right - left <= 0.0 || bottom - top <= 0.0 {
                continue;
            }
            chars += text.chars().count();
            if chars > MAX_CHARS_PER_PAGE {
                return Err(Error::Limit(format!("a page has at most {MAX_CHARS_PER_PAGE} recognised characters")));
            }
            words.push(Word { text, rect: [left, top, right, bottom] });
        }
        if !words.is_empty() {
            lines.push(Line { words });
        }
    }
    Ok(lines)
}

/// The top and bottom of a line (the words' boxes together) and the font size it is shown at: its height.
fn line_extent(line: &Line) -> (f64, f64, f64) {
    let (top, bottom) = line.words.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(t, b), w| (t.min(w.rect[1]), b.max(w.rect[3])));
    (top, bottom, (bottom - top).clamp(MIN_SIZE, MAX_SIZE))
}

/// The word that follows `w` in its line if a space is shown between them: not when either side is a script written without spaces,
/// and not when the gap is wider than a few ems (a column: the position says it).
fn space_before<'a>(w: &Word, next: Option<&'a Word>, size: f64) -> Option<&'a Word> {
    next.filter(|n| !(w.text.chars().next_back().is_some_and(is_unspaced) || n.text.chars().next().is_some_and(is_unspaced)) && n.rect[0] - w.rect[2] < 3.0 * size)
}

/// The text of a page of words the way the layer reads: lines one under another (ends of lines are \n), the words of a line
/// separated by a space where `build_text` shows one.
#[cfg(test)]
pub(crate) fn joined(lines: &[Line]) -> String {
    let mut out = String::new();
    for line in lines {
        let size = line_extent(line).2;
        for (k, w) in line.words.iter().enumerate() {
            out.push_str(&w.text);
            if space_before(w, line.words.get(k + 1), size).is_some() {
                out.push(' ');
            }
        }
        out.push('\n');
    }
    out
}

// --- the text object ------------------------------------------------------------------------------------------------

/// The content of the page's layer: one text object that shows every word invisibly (`3 Tr`) in the box the recogniser gave it.
/// `page_h` is the height of the shown page, whose space the content is in (origin at the lower left, y up). Characters the font
/// does not have are left out by the caller; the face's widths are those of the characters shown.
pub(crate) fn build_text(face: &dyn Face, lines: &[Line], page_h: f64) -> String {
    let mut out = String::from("BT\n3 Tr\n");
    let mut current_size = f64::NAN;
    let mut current_tz = f64::NAN;
    let advance = |text: &str, size: f64| text.chars().map(|c| face.width(c)).sum::<f64>() * size / 1000.0;
    let space = face.width(' ');
    for line in lines {
        let (top, bottom, size) = line_extent(line);
        if !(top.is_finite() && bottom.is_finite()) {
            continue;
        }
        // The cell of a character reaches 0.8 em above the baseline and 0.2 below (`Shown::glyph_box`): the middle of the cell is
        // 0.3 em above it. In the space of the shown page, y up.
        let baseline = page_h - ((top + bottom) / 2.0 + 0.3 * size);
        for (k, w) in line.words.iter().enumerate() {
            let (left, right) = (w.rect[0], w.rect[2]);
            // A space after the word reaches the next one, so that the characters of the line cover it without a gap.
            let trailing = if space > 0.0 { space_before(w, line.words.get(k + 1), size) } else { None };
            let mut shown = w.text.clone();
            let extent = match (trailing, line.words.get(k + 1)) {
                (Some(n), _) => {
                    shown.push(' ');
                    (n.rect[0] - left).max(0.3 * (right - left))
                }
                // No space, but the next word is close (Chinese words, or a word and a punctuation mark): the word reaches it, so
                // that the line has no gaps that another reader would take for spaces.
                (None, Some(n)) if n.rect[0] > right && n.rect[0] - right < 3.0 * size => n.rect[0] - left,
                _ => right - left,
            };
            let natural = advance(&shown, size);
            let tz = if natural > 1.0e-6 { (extent / natural * 100.0).clamp(MIN_TZ, MAX_TZ) } else { 100.0 };
            if size != current_size {
                let _ = writeln!(out, "/F0 {} Tf", num(size));
                current_size = size;
            }
            if tz != current_tz {
                let _ = writeln!(out, "{} Tz", num(tz));
                current_tz = tz;
            }
            let _ = writeln!(out, "1 0 0 1 {} {} Tm {} Tj", num(left), num(baseline), face.encode(&shown));
        }
    }
    out.push_str("ET\n");
    out
}

// --- the edit ---------------------------------------------------------------------------------------------------------------

/// Write the layers of `ocr` into the pages: one font for all, one form for each page that has words.
pub(super) fn layer(ctx: &mut Ctx<'_>, pages: &[Page], ocr: &[OcrPage]) -> Result<()> {
    if ocr.is_empty() {
        return Err(Error::Invalid("there is no recognised text".to_string()));
    }
    if ocr.len() > MAX_OCR_PAGES {
        return Err(Error::Limit(format!("a run gives text to at most {MAX_OCR_PAGES} pages")));
    }
    if ocr.iter().map(OcrPage::word_count).fold(0usize, usize::saturating_add) > MAX_WORDS_PER_RUN {
        return Err(Error::Limit(format!("a run has at most {MAX_WORDS_PER_RUN} recognised words")));
    }
    // The words of each page in points; a page named twice is done once, a page that does not exist is an error, and a page that has
    // a layer already is left as it is (a second layer would be a second copy of the text).
    let mut seen = HashSet::new();
    let mut work: Vec<(&Page, Shown, Vec<Line>)> = Vec::with_capacity(ocr.len());
    let mut had_layer = 0usize;
    for o in ocr {
        let page = pages.get(o.page as usize).ok_or_else(|| Error::Invalid("there is no such page".to_string()))?;
        if !seen.insert(page.obj_ref.num) {
            continue;
        }
        if has_layer(ctx.doc, page) {
            had_layer += 1;
            continue;
        }
        let shown = Shown::of(page);
        let (w, h) = shown.size();
        let lines = tidy(o, w, h)?;
        let (chars, words) = lines.iter().fold((0usize, 0usize), |(c, n), l| (c + l.words.iter().map(|w| w.text.chars().count()).sum::<usize>(), n + l.words.len()));
        if !ctx.work.charge(CHAR_COST * chars as f64 + WORD_COST * words as f64) {
            return Err(Error::Limit("the change took too much work".to_string()));
        }
        work.push((page, shown, lines));
    }

    if work.is_empty() {
        return Err(Error::Invalid("the pages already have recognised text".to_string()));
    }
    // One font for all the pages: the characters used, the most used kept when there are too many.
    let mut counts: BTreeMap<char, u32> = BTreeMap::new();
    for (.., lines) in &work {
        for w in lines.iter().flat_map(|l| &l.words) {
            for c in w.text.chars() {
                *counts.entry(c).or_insert(0) += 1;
            }
        }
    }
    if counts.is_empty() {
        return Err(Error::Invalid("no text was recognised".to_string()));
    }
    counts.insert(' ', u32::MAX);
    if counts.len() > MAX_GLYPH_CHARS {
        let mut by_use: Vec<(char, u32)> = counts.iter().map(|(&c, &n)| (c, n)).collect();
        crate::order::sort_by_key(&mut by_use, |&(c, n)| u64::from(u32::MAX - n) << 32 | u64::from(c));
        by_use.truncate(MAX_GLYPH_CHARS);
        counts.clear();
        counts.extend(by_use);
    }
    let (font, usable) = fontsub::choose_covering(&counts)?;
    let embedded = fontsub::embed(&font, &usable, ctx.work)?;
    let from = ctx.objects.len();
    let font_ref = fontsub::add_objects(ctx, &embedded)?;
    ctx.tally(from)?;

    // The pages: words with a character the font lacks lose that character (a word of nothing else goes), and each page gets its form.
    let mut left_out = 0usize;
    let mut shared = Shared::default();
    let mut any = false;
    for (page, shown, mut lines) in work {
        for l in &mut lines {
            for w in &mut l.words {
                let before = w.text.chars().count();
                w.text.retain(|c| usable.contains(&c));
                left_out += before - w.text.chars().count();
            }
            l.words.retain(|w| !w.text.is_empty());
        }
        lines.retain(|l| !l.words.is_empty());
        if lines.is_empty() {
            continue;
        }
        any = true;
        let (w, h) = shown.size();
        let text = build_text(&embedded, &lines, h);
        let mut fonts = Dict::new();
        fonts.set("F0", Object::Ref(font_ref));
        let mut resources = Dict::new();
        resources.set("Font", Object::Dict(fonts));
        let form = ctx.alloc()?;
        let mut dict = Dict::new();
        dict.set("Type", Object::from("XObject"));
        dict.set("Subtype", Object::from("Form"));
        dict.set(OCR_MARK, Object::Bool(true));
        dict.set("BBox", super::reals(&[0.0, 0.0, w, h]));
        dict.set("Resources", Object::Dict(resources));
        dict.set("Filter", Object::from("FlateDecode"));
        let data = compress_to_vec_zlib(text.as_bytes(), 6);
        ctx.spend(data.len() + dict.approx_size() + 48)?;
        ctx.objects.push((form.num, Object::Stream(Stream { dict, data })));
        if !ctx.work.charge(6.0 * text.len() as f64) {
            return Err(Error::Limit("the change took too much work".to_string()));
        }
        let to_user = content::upright(&shown);
        let name = format!("Qo{}", form.num);
        let how = content::Placing { resource: content::Resource { category: "XObject", wanted: &name, object: form }, under: false, tag: None };
        content::put_on_page(ctx, &mut shared, page, &how, &|n| format!("q {} /{n} Do Q\n", content::cm(to_user)))?;
    }
    if !any {
        return Err(Error::Invalid("no text was recognised".to_string()));
    }
    if left_out > 0 {
        ctx.notes.push(format!("{left_out} recognised characters are in no installed font and were left out of the text layer."));
    }
    if had_layer > 0 {
        ctx.notes.push(format!("{had_layer} pages already had recognised text and were left as they are."));
    }
    Ok(())
}

/// The characters of a page of recognised words as the reader reads a page (`PageChars`): the text of [`joined`], and for each
/// character the box it covers, in points of the shown page. The characters of a word share its box in proportion to their widths
/// (a Chinese word, which is one character in most cases, has the whole box).
pub fn page_chars(page: &OcrPage, width: f64, height: f64) -> Result<crate::text::PageChars> {
    let lines = tidy(page, width, height)?;
    let mut text = String::new();
    let mut boxes: Vec<[f32; 4]> = Vec::new();
    for line in &lines {
        let (top, bottom, size) = line_extent(line);
        for (k, w) in line.words.iter().enumerate() {
            let weights: Vec<f64> = w.text.chars().map(|c| if c.is_ascii() { 0.5 + if c.is_ascii_alphanumeric() { 0.1 } else { -0.15 } } else { 1.0 }).collect();
            let total: f64 = weights.iter().sum();
            let mut x = w.rect[0];
            for (c, wt) in w.text.chars().zip(&weights) {
                let step = (w.rect[2] - w.rect[0]) * wt / total.max(1.0e-9);
                text.push(c);
                boxes.push([x as f32, top as f32, (x + step) as f32, bottom as f32]);
                x += step;
            }
            if space_before(w, line.words.get(k + 1), size).is_some() {
                text.push(' ');
                let p = w.rect[2] as f32;
                boxes.push([p, bottom as f32, p, bottom as f32]);
            }
        }
        text.push('\n');
        let end = line.words.last().map_or(0.0, |w| w.rect[2]) as f32;
        boxes.push([end, bottom as f32, end, bottom as f32]);
    }
    Ok(crate::text::PageChars { text, boxes })
}

/// The pages of a layer that is to be used in memory only (a file that is signed, or whose author did not allow changing it): the
/// number of the pages with words, so that the reader can say how many pages the text is for.
pub fn pages_with_words(pages: &[OcrPage]) -> usize {
    pages.iter().filter(|p| p.word_count() > 0).map(|p| p.page).fold(BTreeSet::new(), |mut set, page| {
        set.insert(page);
        set
    }).len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::{ImagePlace, TextLine};

    /// A face whose letters are all 500 thousandths wide, and CJK ones 1000.
    struct Fixed;

    impl Face for Fixed {
        fn width(&self, c: char) -> f64 {
            if is_unspaced(c) { 1000.0 } else { 500.0 }
        }
        fn ascent_descent(&self) -> (f64, f64) {
            (0.9, -0.2)
        }
        fn encode(&self, text: &str) -> String {
            format!("<{}>", text.chars().map(|c| format!("{:04X}", c as u32)).collect::<String>())
        }
    }

    fn word(text: &str, rect: [f32; 4]) -> OcrWord {
        OcrWord { text: text.to_string(), rect }
    }

    fn page(words: Vec<Vec<OcrWord>>) -> OcrPage {
        OcrPage { page: 0, width: 1000, height: 2000, lines: words.into_iter().map(|words| OcrLine { words }).collect() }
    }

    #[test]
    fn the_resolution_is_300_dpi_less_for_a_page_too_big_for_the_recogniser_and_none_for_a_huge_one() {
        // A4: 300 dpi is 2480 x 3508.
        assert_eq!(ocr_dpi(595.0, 842.0, 10_000), Some(300.0));
        // A recogniser that takes 2000 pixels at most: 2000 pixels over the 842 points of the long side.
        let dpi = ocr_dpi(595.0, 842.0, 2_000).unwrap();
        assert!(dpi < 171.0 && dpi > 168.0, "{dpi}");
        assert!((842.0f64 * dpi / 72.0).ceil() <= 2_000.0);
        // A poster is held to the pixel cap: 9 million pixels.
        let dpi = ocr_dpi(2_400.0, 3_400.0, 10_000).unwrap();
        let (w, h) = ((2_400.0f64 * dpi / 72.0).ceil(), (3_400.0f64 * dpi / 72.0).ceil());
        assert!(w * h <= MAX_IMAGE_PIXELS as f64 && w <= 10_000.0 && h <= 10_000.0, "{w} x {h}");
        // 14400-point pages (200 inches) would be drawn at 24 dpi or so: not worth it.
        assert_eq!(ocr_dpi(14_400.0, 14_400.0, 10_000), None);
        assert_eq!(ocr_dpi(0.0, 100.0, 10_000), None);
        assert_eq!(ocr_dpi(f64::NAN, 100.0, 10_000), None);
    }

    #[test]
    fn a_scan_has_hardly_any_text_and_is_mostly_pictures() {
        let image = |bbox: [f32; 4]| ImagePlace { obj: crate::object::ObjRef::new(5, 0), bbox, flip_h: false, flip_v: false };
        let text = |s: &str| TextLine { text: s.to_string(), pieces: Vec::new(), vertical: false };
        let layout = |lines: Vec<TextLine>, images: Vec<ImagePlace>| PageLayout { size: (100.0, 200.0), lines, images, rules: Vec::new() };
        let scan = scan_state(&layout(vec![], vec![image([0.0, 0.0, 100.0, 200.0])]));
        assert_eq!((scan.text_chars, scan.image_cover), (0, 1.0));
        assert!(wants_text(&scan, true) && wants_text(&scan, false));
        // A page number is not text enough; a paragraph is.
        let stamped = scan_state(&layout(vec![text(" 12 ")], vec![image([0.0, 0.0, 100.0, 200.0])]));
        assert_eq!(stamped.text_chars, 2);
        assert!(wants_text(&stamped, true));
        let text_page = scan_state(&layout(vec![text("A paragraph of real text")], vec![image([0.0, 0.0, 100.0, 200.0])]));
        assert!(!wants_text(&text_page, true) && !wants_text(&text_page, false));
        // A page with a small picture and no text: wanted by itself (the person asked), not among "every scan".
        let small = scan_state(&layout(vec![], vec![image([10.0, 10.0, 50.0, 60.0])]));
        assert!((small.image_cover - 0.1).abs() < 1e-6);
        assert!(!wants_text(&small, true) && wants_text(&small, false));
        // Pictures off the page count for the part that is on it; none at all is no scan.
        assert!((scan_state(&layout(vec![], vec![image([50.0, 0.0, 250.0, 200.0])])).image_cover - 0.5).abs() < 1e-6);
        assert_eq!(scan_state(&layout(vec![], vec![])).image_cover, 0.0);
    }

    #[test]
    fn boxes_are_scaled_cut_to_the_page_and_nonsense_is_dropped() {
        // The picture is 1000 x 2000 pixels of a 500 x 400 point page: x counts 0.5 point a pixel, y 0.2.
        let p = page(vec![vec![
            word("ab", [100.0, 200.0, 300.0, 400.0]),
            word(" c d\u{7}", [400.0, 200.0, 2_000.0, 400.0]),
            word("", [0.0, 0.0, 10.0, 10.0]),
            word("nan", [f32::NAN, 0.0, 10.0, 10.0]),
            word("flat", [10.0, 10.0, 10.0, 50.0]),
            word("off", [1_500.0, 0.0, 1_600.0, 50.0]),
        ]]);
        let lines = tidy(&p, 500.0, 400.0).unwrap();
        assert_eq!(lines.len(), 1);
        let words = &lines[0].words;
        assert_eq!(words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(), ["ab", "cd"]);
        assert_eq!(words[0].rect, [50.0, 40.0, 150.0, 80.0]);
        assert_eq!(words[1].rect, [200.0, 40.0, 500.0, 80.0]);
        // A picture with no size, and a page with too many words, are refused.
        assert!(tidy(&OcrPage { width: 0, ..p.clone() }, 500.0, 400.0).is_err());
        let many = page(vec![(0..=MAX_WORDS_PER_PAGE).map(|_| word("x", [0.0, 0.0, 1.0, 1.0])).collect()]);
        assert!(matches!(tidy(&many, 500.0, 400.0), Err(Error::Limit(_))));
    }

    #[test]
    fn a_word_is_placed_in_its_box_and_scaled_to_fill_it() {
        // Two words in a line 20 points high (top 100, bottom 120) on a page 300 points high: the font is 20 points, the baseline is
        // 0.2 em above the bottom of the line, 300 - 120 + 4 = 184 above the foot of the page.
        let lines = vec![Line { words: vec![Word { text: "ab".to_string(), rect: [10.0, 100.0, 50.0, 120.0] }, Word { text: "cd".to_string(), rect: [60.0, 102.0, 100.0, 118.0] }] }];
        let text = build_text(&Fixed, &lines, 300.0);
        assert!(text.starts_with("BT\n3 Tr\n") && text.ends_with("ET\n"), "{text}");
        // "ab" and the space after it must fill 10 to 60: natural width 3 x 500 x 20 / 1000 = 30, so 50 / 30 = 166.6667 %.
        assert!(text.contains("/F0 20 Tf\n166.6667 Tz\n1 0 0 1 10 184 Tm <006100620020> Tj\n"), "{text}");
        // The last word fills its box: natural 2 x 10 = 20 for 40 points, 200 %.
        assert!(text.contains("200 Tz\n1 0 0 1 60 184 Tm <00630064> Tj\n"), "{text}");
        assert_eq!(text.matches("Tf").count(), 1, "the size is set once for the line");
    }

    #[test]
    fn chinese_words_get_no_space_and_a_far_word_gets_none_either() {
        let w = |t: &str, l: f64, r: f64| Word { text: t.to_string(), rect: [l, 0.0, r, 12.0] };
        let chinese = vec![Line { words: vec![w("你好", 0.0, 24.0), w("世界", 24.0, 48.0)] }];
        let text = build_text(&Fixed, &chinese, 100.0);
        assert!(text.contains("<4F60597D> Tj") && text.contains("<4E16754C> Tj"), "{text}");
        assert!(!text.contains("0020"), "{text}");
        assert_eq!(joined(&chinese), "你好世界\n");
        // Latin next to Latin, near: the space is shown and joined says so; Latin next to Chinese: none; 388 points away: none.
        let mixed = vec![Line { words: vec![w("go", 0.0, 12.0), w("to", 14.0, 26.0), w("北京", 28.0, 52.0), w("far", 440.0, 470.0)] }];
        assert_eq!(joined(&mixed), "go to北京far\n");
        let far = vec![Line { words: vec![w("go", 0.0, 12.0), w("far", 400.0, 430.0)] }];
        assert_eq!(joined(&far), "gofar\n");
        assert!(!build_text(&Fixed, &far, 100.0).contains("0020"));
    }

    #[test]
    fn a_space_does_not_make_a_font_cover_text_it_does_not_have() {
        // Nearly every font has a space; a run of characters that no font has is an error that says so, not a layer of spaces.
        let counts: BTreeMap<char, u32> = [(' ', u32::MAX), ('\u{10FFFD}', 3)].into_iter().collect();
        match fontsub::choose_covering(&counts) {
            Err(Error::Invalid(why)) => assert!(why.contains("U+10FFFD") || why.contains("font"), "{why}"),
            Err(other) => panic!("{other:?}"),
            Ok((_, has)) => panic!("a font was found for U+10FFFD: it has {has:?}"),
        }
    }

    #[test]
    fn the_text_of_a_layer_has_a_box_for_each_character() {
        let p = page(vec![vec![word("ab", [0.0, 0.0, 200.0, 100.0]), word("北", [300.0, 0.0, 400.0, 100.0])], vec![word("cd", [0.0, 500.0, 200.0, 600.0])]]);
        let chars = page_chars(&p, 500.0, 1000.0).unwrap();
        assert_eq!(chars.text, "ab北\ncd\n");
        assert_eq!(chars.boxes.len(), chars.text.chars().count());
        // "ab" fills 0..100 of the 500 point page (200 px at 0.5), the letters share it equally; the Chinese character fills its box.
        assert_eq!(chars.boxes[0], [0.0, 0.0, 50.0, 50.0]);
        assert_eq!(chars.boxes[1], [50.0, 0.0, 100.0, 50.0]);
        assert_eq!(chars.boxes[2], [150.0, 0.0, 200.0, 50.0]);
        // The line end has no area.
        assert_eq!(chars.boxes[3][0], chars.boxes[3][2]);
        assert_eq!(pages_with_words(&[p]), 1);
    }
}
