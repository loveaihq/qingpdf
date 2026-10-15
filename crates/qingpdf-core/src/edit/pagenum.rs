//! Page numbers, headers and footers: up to six pieces of text on each page of a run (left, middle and right of the head and of the foot),
//! each a template whose `{page}`, `{pages}`, `{date}` and `{file}` are filled in for every page.
//!
//! They are written into the page's own content with the machinery of 4c (`content::put_on_page`) and are marked as artifacts of the page
//! (ISO 32000-1 14.8.2.2, Table 330: `/Artifact` with `/Type /Pagination` and `/Subtype /Header` or `/Footer`), so that the text
//! extraction, the export to Word and Excel and the reader's search and selection leave them out, as they do a watermark. One subset of a
//! system font holds every character of the run; the pages share it (it goes into their resources under one name) and each has the
//! drawing of its own in a stream of its own. Places are the page as it is shown (its visible box turned by `/Rotate`): the head and the
//! foot are at the top and the bottom of what the person sees, and the text reads upright.
//!
//! The streams that draw them are listed under a private key of the page ([`TAG`]), which is how they are found again and taken out
//! ([`unpaginate`]); the page's own content is not touched in either direction.

use std::collections::{BTreeSet, HashSet};
use std::fmt::Write;

use super::appearance::num;
use super::boxes::{BOX_COST, MAX_FONT_SIZE, MIN_FONT_SIZE};
use super::content::{self, Placing, Resource, Shared};
use super::fontsub::{self, Look};
use super::Ctx;
use crate::document::Page;
use crate::error::{Error, Result};
use crate::layout::Face;
use crate::object::Object;
use crate::text::Shown;

/// The six places, in the order of [`Pagination::slots`]: head left, middle, right; foot left, middle, right.
pub const SLOTS: usize = 6;
/// Most characters of one template, and most pages one run goes on.
pub const MAX_TEMPLATE_CHARS: usize = 200;
pub const MAX_PAGINATION_PAGES: usize = 100_000;
/// The biggest number the first page may be given, and the biggest distance from the edge of the page, in millimetres.
pub const MAX_START: u32 = 1_000_000;
pub const MAX_MARGIN_MM: f32 = 500.0;
/// The private key of the page under which the streams that draw the numbers are listed.
pub(super) const TAG: &str = "QpPagination";
/// What a page costs the work meter besides what putting the drawing on it costs, and what a character of text does (a unit is about a
/// nanosecond).
const PAGE_COST: f64 = 4_000.0;
const CHAR_COST: f64 = 600.0;
/// Most characters of a file name that `{file}` is given, and of the date.
const MAX_FILE_CHARS: usize = 255;
const MAX_DATE_CHARS: usize = 32;
const MM: f64 = 72.0 / 25.4;

/// Page numbers, headers and footers on the pages `pages`.
#[derive(Clone, Debug, PartialEq)]
pub struct Pagination {
    /// The templates: head left, head middle, head right, foot left, foot middle, foot right (blank: nothing there). `{page}` is the
    /// number of the page, `{pages}` the number of the last page of the run, `{date}` the date of `date`, `{file}` the file's name;
    /// anything else, braces included, is written as it stands.
    pub slots: [String; SLOTS],
    /// Points ([`MIN_FONT_SIZE`] to [`MAX_FONT_SIZE`]).
    pub size: f32,
    pub color: [u8; 3],
    /// How far the text is from the edges of the page (the top for the head, the bottom for the foot, the sides for the left and the
    /// right ones), in millimetres.
    pub margin: f32,
    /// The pages, from 0. The first of them (the one with the lowest number) is the first of the run.
    pub pages: Vec<u32>,
    /// The number of the first page of the run; a later page has it plus its distance from the first.
    pub start: u32,
    /// On the pages that come second, fourth ... of the file, the left places and the right ones change sides (the number at the outer
    /// edge when the pages are printed on both sides).
    pub mirror: bool,
    /// The first page of the run gets nothing (it is still counted).
    pub skip_first: bool,
    /// The file's name, for `{file}`.
    pub file: String,
    /// The date, for `{date}` (the caller writes it as it wants it to read, `2026-10-15`).
    pub date: String,
}

/// `template` with its fields filled in. (The file's name goes in last, so that whatever braces it has are not read as fields.)
fn expand(template: &str, page: &str, pages: &str, date: &str, file: &str) -> String {
    template.replace("{page}", page).replace("{pages}", pages).replace("{date}", date).replace("{file}", file)
}

/// The characters the font of a run must have: what the templates say, the digits (of the page number and of the number of pages), the
/// date and the file's name.
fn font_chars(templates: &[String], date: &str, file: &str) -> BTreeSet<char> {
    const DIGITS: &str = "0123456789";
    let mut chars: BTreeSet<char> = BTreeSet::new();
    for t in templates {
        chars.extend(expand(t, DIGITS, DIGITS, date, file).chars());
    }
    chars
}

fn width(face: &dyn Face, line: &str) -> f64 {
    line.chars().map(|c| face.width(c)).sum::<f64>() / 1000.0
}

/// The drawing for one page of `w` by `h` points as it is shown: the head and the foot, each a marked sequence (14.8.2.2). `texts` are the
/// six lines (empty: none there). `font` is the name of the font in the page's resources.
fn drawing(face: &dyn Face, font: &str, spec: &Style, w: f64, h: f64, texts: &[String]) -> String {
    let (asc, desc) = face.ascent_descent();
    let (size, margin) = (spec.size, spec.margin);
    let mut out = String::new();
    // The head hangs from the top margin, the foot stands on the bottom one (the lines are as high as the font's ascent and descent).
    for ((kind, y), group) in [("Header", h - margin - asc * size), ("Footer", margin - desc * size)].into_iter().zip(texts.chunks(3)) {
        let mut body = String::new();
        for (k, text) in group.iter().enumerate().filter(|(_, t)| !t.is_empty()) {
            let len = width(face, text) * size;
            let x = match k {
                0 => margin,
                1 => (w - len) / 2.0,
                _ => w - margin - len,
            };
            let _ = writeln!(body, "1 0 0 1 {} {} Tm {} Tj", num(x.min(w - len).max(0.0)), num(y), face.encode(text));
        }
        if !body.is_empty() {
            let _ = write!(out, "/Artifact <</Type /Pagination /Subtype /{kind}>> BDC\nBT\n/{font} {} Tf\n{}\n{body}ET\nEMC\n", num(size), spec.color);
        }
    }
    out
}

/// The run: what decides the six lines of each page of it.
struct Run<'a> {
    templates: &'a [String],
    start: u64,
    /// The first and the last page of the run (from 0).
    first: u32,
    last: u32,
    mirror: bool,
    skip_first: bool,
    date: &'a str,
    file: &'a str,
}

impl Run<'_> {
    /// The six lines of the page with this index, or none when nothing is written on it (the first page of the run, when it is left out).
    fn texts(&self, index: u32) -> Option<Vec<String>> {
        if self.skip_first && index == self.first {
            return None;
        }
        let number = (self.start + u64::from(index.saturating_sub(self.first))).to_string();
        let total = (self.start + u64::from(self.last.saturating_sub(self.first))).to_string();
        let mut texts: Vec<String> = self.templates.iter().map(|t| expand(t, &number, &total, self.date, self.file)).collect();
        // Printed on both sides, the second, fourth ... page of the file is a left-hand page: the left and the right places swap.
        if self.mirror && index % 2 == 1 {
            texts.swap(0, 2);
            texts.swap(3, 5);
        }
        Some(texts)
    }
}

/// What every page is drawn with: the size and the margin in points, the colour as `rg` has it.
struct Style {
    size: f64,
    margin: f64,
    color: String,
}

/// Write the numbers, headers and footers of `spec` into `pages`.
pub(super) fn paginate(ctx: &mut Ctx<'_>, pages: &[Page], spec: &Pagination) -> Result<()> {
    let size = f64::from(spec.size);
    let margin = f64::from(spec.margin);
    // (The limits are written out in the messages: a test keeps them with the constants.)
    if !size.is_finite() || !margin.is_finite() || !(0.0..=f64::from(MAX_MARGIN_MM)).contains(&margin) {
        return Err(Error::Invalid("the size is not a number, or the margin is not from 0 to 500 millimetres".to_string()));
    }
    if spec.start > MAX_START {
        return Err(Error::Limit("the first page may be given a number up to 1000000".to_string()));
    }
    if spec.slots.iter().any(|t| t.chars().count() > MAX_TEMPLATE_CHARS) {
        return Err(Error::Limit("a template has at most 200 characters".to_string()));
    }
    let templates: Vec<String> = spec.slots.iter().map(|t| content::clean(t, true, MAX_TEMPLATE_CHARS)).collect();
    if templates.iter().all(|t| t.trim().is_empty()) {
        return Err(Error::Invalid("there is nothing to write on the pages".to_string()));
    }
    if spec.pages.is_empty() {
        return Err(Error::Invalid("there are no pages to number".to_string()));
    }
    if spec.pages.len() > MAX_PAGINATION_PAGES {
        return Err(Error::Limit("page numbers go on at most 100000 pages at once".to_string()));
    }
    let chosen: Vec<(u32, &Page)> = spec.pages.iter().filter_map(|&i| pages.get(i as usize).map(|p| (i, p))).collect();
    if chosen.len() != spec.pages.len() {
        return Err(Error::Invalid("there is no such page".to_string()));
    }
    let (first, last) = chosen.iter().fold((u32::MAX, 0u32), |(lo, hi), &(i, _)| (lo.min(i), hi.max(i)));
    let (date, file) = (content::clean(&spec.date, true, MAX_DATE_CHARS), content::clean(&spec.file, true, MAX_FILE_CHARS));
    content::charge(ctx, BOX_COST + CHAR_COST * templates.iter().map(|t| t.chars().count()).sum::<usize>() as f64)?;

    // One font for the run.
    let chars = font_chars(&templates, &date, &file);
    let font = fontsub::choose(&chars, Look::Sans)?;
    let embedded = fontsub::embed(&font, &chars, ctx.work)?;
    let from = ctx.objects.len();
    let font_ref = fontsub::add_objects(ctx, &embedded)?;
    ctx.tally(from)?;

    let name = format!("Qn{}", font_ref.num);
    let [r, g, b] = spec.color.map(|c| num(f64::from(c) / 255.0));
    let look = Style { size: size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE), margin: margin * MM, color: format!("{r} {g} {b} rg") };
    let how = Placing { resource: Resource { category: "Font", wanted: &name, object: font_ref }, under: false, tag: Some(TAG) };
    let run = Run { templates: &templates, start: u64::from(spec.start), first, last, mirror: spec.mirror, skip_first: spec.skip_first, date: &date, file: &file };
    let mut shared = Shared::default();
    let mut seen: HashSet<u32> = HashSet::new();
    let mut written = 0usize;
    for (index, page) in chosen {
        // (A page named twice, or listed twice by the page tree, is done once.)
        let Some(texts) = run.texts(index).filter(|_| seen.insert(page.obj_ref.num)) else { continue };
        content::charge(ctx, PAGE_COST + CHAR_COST * texts.iter().map(|t| t.chars().count()).sum::<usize>() as f64)?;
        let shown = Shown::of(page);
        let (w, h) = shown.size();
        let to_user = content::cm(content::upright(&shown));
        content::put_on_page(ctx, &mut shared, page, &how, &|font| format!("q {to_user}\n{}Q\n", drawing(&embedded, font, &look, w, h, &texts)))?;
        written += 1;
    }
    if written == 0 {
        return Err(Error::Invalid("the only page was to be left out".to_string()));
    }
    Ok(())
}

/// Is this (the decoded bytes of a stream) what [`paginate`] writes for a page: `q`, the matrix, a line end, the marked sequence of a head
/// or a foot (or nothing), and the `Q` that ends it? A key of a page that lists a stream that is not is not believed.
fn is_ours(data: &[u8]) -> bool {
    let Some(end) = data.iter().position(|&b| b == b'\n') else { return false };
    let (first, rest) = data.split_at(end + 1);
    let head = |kind: &str| rest.starts_with(format!("/Artifact <</Type /Pagination /Subtype /{kind}>> BDC\n").as_bytes());
    first.starts_with(b"q ") && first.ends_with(b" cm\n") && (rest == b"Q\n" || ((head("Header") || head("Footer")) && data.ends_with(b"EMC\nQ\n")))
}

/// What a byte of a stream that is read to see whether it is ours costs the work meter, and the most bytes of one that are read (what
/// [`paginate`] writes for a page is far less).
const READ_COST: f64 = 10.0;
const MAX_READ: usize = 4 << 20;

/// Take away the numbers, headers and footers that [`paginate`] wrote into `pages`: the streams listed under the page's private key
/// are taken out of its `/Contents` (the font stays in the resources), each one only after it has been read and found to be what
/// [`paginate`] writes (a page's key is the file's word, and may name a stream of the page's own text). A page whose list of streams is not
/// what [`paginate`] made of it (it was changed since) is left as it is.
pub(super) fn unpaginate(ctx: &mut Ctx<'_>, pages: &[Page]) -> Result<()> {
    let mut seen: HashSet<u32> = HashSet::new();
    let mut taken = 0usize;
    for page in pages {
        if !seen.insert(page.obj_ref.num) {
            continue;
        }
        content::charge(ctx, PAGE_COST)?;
        let Ok(Object::Dict(mut dict)) = ctx.doc.get(page.obj_ref) else { continue };
        let Some(Object::Array(ours)) = dict.get(TAG).cloned() else { continue };
        let Some(Object::Array(list)) = dict.get("Contents").cloned() else { continue };
        content::charge(ctx, CHAR_COST * (ours.len() + list.len()) as f64)?;
        let ours: HashSet<u32> = ours.iter().filter_map(|o| o.as_obj_ref().map(|r| r.num)).collect();
        // The listed streams that are ours (each is read once).
        let mut checked: Vec<(u32, bool)> = Vec::new();
        let mut kept: Vec<Object> = Vec::with_capacity(list.len());
        for o in &list {
            let Some(r) = o.as_obj_ref().filter(|r| ours.contains(&r.num)) else {
                kept.push(o.clone());
                continue;
            };
            let known = match checked.iter().find(|(n, _)| *n == r.num) {
                Some(&(_, yes)) => yes,
                None => {
                    let yes = match ctx.doc.get(r) {
                        Ok(Object::Stream(stream)) => {
                            content::charge(ctx, READ_COST * stream.data.len() as f64)?;
                            let limit = (ctx.work.remaining() / READ_COST).min(MAX_READ as f64) as usize;
                            let _own_budget = ctx.doc.page_decode_budget();
                            ctx.doc.decode_stream_limited(&stream, limit).is_ok_and(|data| is_ours(&data))
                        }
                        _ => false,
                    };
                    checked.push((r.num, yes));
                    yes
                }
            };
            if !known {
                kept.push(o.clone());
            }
        }
        if kept.len() == list.len() {
            continue;
        }
        dict.remove(TAG);
        dict.set("Contents", Object::Array(kept));
        ctx.spend(dict.approx_size() + 48)?;
        ctx.objects.push((page.obj_ref.num, Object::Dict(dict)));
        taken += 1;
    }
    if taken == 0 {
        return Err(Error::Invalid("there are no page numbers, headers or footers made by qingpdf on these pages".to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run<'a>(templates: &'a [String], start: u64, first: u32, last: u32) -> Run<'a> {
        Run { templates, start, first, last, mirror: false, skip_first: false, date: "2026-10-15", file: "a{page}.pdf" }
    }

    fn six(foot_middle: &str, foot_right: &str) -> Vec<String> {
        ["", "", "", "", foot_middle, foot_right].iter().map(ToString::to_string).collect()
    }

    #[test]
    fn the_limits_in_the_messages_are_the_limits() {
        const { assert!(MAX_MARGIN_MM == 500.0 && MAX_START == 1_000_000 && MAX_TEMPLATE_CHARS == 200 && MAX_PAGINATION_PAGES == 100_000) };
    }

    #[test]
    fn the_fields_are_filled_in_and_what_is_not_a_field_stays() {
        assert_eq!(expand("第 {page} 页 共 {pages} 页", "3", "9", "d", "f"), "第 3 页 共 9 页");
        assert_eq!(expand("{date} {file} {page}/{pages} {x} {{page}}", "3", "9", "2026-10-15", "a.pdf"), "2026-10-15 a.pdf 3/9 {x} {3}");
        // A file name with a field in it is not read as one (it goes in last), and neither is a date.
        assert_eq!(expand("{file}|{page}", "3", "9", "d", "{page}{pages}.pdf"), "{page}{pages}.pdf|3");
        assert_eq!(expand("{page", "3", "9", "d", "f"), "{page");
    }

    #[test]
    fn numbers_start_where_they_are_told_count_from_the_first_page_of_the_run_and_the_last_is_the_total() {
        let t = six("{page}/{pages}", "");
        // The whole file, from 1: page 5 of 9.
        let all = run(&t, 1, 0, 8);
        assert_eq!(all.texts(4).unwrap()[4], "5/9");
        // Pages 3 to 11 of a file of 11 pages, the first of them reading 1: page 3 is 1, page 11 is 9 of 9.
        let from_three = run(&t, 1, 2, 10);
        assert_eq!(from_three.texts(2).unwrap()[4], "1/9");
        assert_eq!(from_three.texts(10).unwrap()[4], "9/9");
        // Starting at 0 and at 100.
        assert_eq!(run(&t, 0, 0, 3).texts(0).unwrap()[4], "0/3");
        assert_eq!(run(&t, 100, 0, 3).texts(3).unwrap()[4], "103/103");
        // Pages that are not next to each other count from the first (page 5 of a run that starts at page 1 reads 5).
        assert_eq!(run(&t, 1, 0, 4).texts(4).unwrap()[4], "5/5");
    }

    #[test]
    fn the_first_page_can_be_left_out_and_is_still_counted_and_the_sides_change_on_the_even_pages() {
        let t: Vec<String> = ["hl", "hc", "hr", "fl", "fc", "fr"].iter().map(ToString::to_string).collect();
        let r = Run { skip_first: true, mirror: true, ..run(&t, 1, 0, 3) };
        assert_eq!(r.texts(0), None);
        // Page 2 (index 1) is a left-hand page: left and right change places, the middle stays.
        assert_eq!(r.texts(1).unwrap(), ["hr", "hc", "hl", "fr", "fc", "fl"]);
        assert_eq!(r.texts(2).unwrap(), ["hl", "hc", "hr", "fl", "fc", "fr"]);
        // The first page of the run, not of the file, is the one that is left out.
        let later = Run { skip_first: true, ..run(&t, 1, 2, 5) };
        assert!(later.texts(2).is_none() && later.texts(3).is_some() && later.texts(0).is_some_and(|_| true));
        let n = six("{page}", "");
        assert_eq!(Run { skip_first: true, ..run(&n, 1, 2, 5) }.texts(3).unwrap()[4], "2");
    }

    #[test]
    fn the_drawing_is_two_marked_sequences_with_the_text_where_the_places_are() {
        // A face with a fixed width of half an em, 0.8 up and 0.2 down: a line of n characters is n / 2 ems wide.
        struct Fixed;
        impl Face for Fixed {
            fn width(&self, _: char) -> f64 {
                500.0
            }
            fn ascent_descent(&self) -> (f64, f64) {
                (0.8, -0.2)
            }
            fn encode(&self, text: &str) -> String {
                format!("<{text}>")
            }
        }
        let style = Style { size: 10.0, margin: 20.0, color: "0 0 0 rg".to_string() };
        let texts: Vec<String> = ["hl", "", "hr", "", "fc", ""].iter().map(ToString::to_string).collect();
        let d = drawing(&Fixed, "Qn9", &style, 200.0, 300.0, &texts);
        // The head: its baseline 0.8 em below the line that is 20 points from the top; the left one 20 points from the left, the right one
        // ends 20 points from the right (it is 10 points wide).
        assert!(d.contains("/Artifact <</Type /Pagination /Subtype /Header>> BDC\nBT\n/Qn9 10 Tf\n0 0 0 rg\n1 0 0 1 20 272 Tm <hl> Tj\n1 0 0 1 170 272 Tm <hr> Tj\nET\nEMC\n"), "{d}");
        // The foot: the baseline 0.2 em above the line that is 20 points from the bottom; the middle is in the middle (10 wide).
        assert!(d.contains("/Subtype /Footer>> BDC\nBT\n/Qn9 10 Tf\n0 0 0 rg\n1 0 0 1 95 22 Tm <fc> Tj\nET\nEMC\n"), "{d}");
        assert_eq!(d.matches("BDC").count(), 2);
        // A line wider than the page starts at the left edge, one that is nowhere is not drawn.
        let wide: Vec<String> = ["", &"x".repeat(100), "", "", "", ""].iter().map(ToString::to_string).collect();
        assert!(drawing(&Fixed, "F", &style, 200.0, 300.0, &wide).contains("1 0 0 1 0 272 Tm"));
        assert_eq!(drawing(&Fixed, "F", &style, 200.0, 300.0, &vec![String::new(); 6]), "");
    }

    #[test]
    fn the_font_has_all_ten_digits_for_the_page_number_and_for_the_number_of_pages_whichever_of_them_the_templates_use() {
        let digits: BTreeSet<char> = "0123456789".chars().collect();
        let chars = |t: &str| font_chars(&[t.to_string()], "", "");
        // `{pages}` alone (the number of pages is any number), `{page}` alone, both: every digit; neither: none.
        for t in ["of {pages}", "{page}", "{page}/{pages}"] {
            assert!(digits.is_subset(&chars(t)), "{t}");
        }
        assert_eq!(chars("of {pages}").len(), 10 + 3, "the digits and the letters o, f and the space");
        assert!(chars("Page").is_disjoint(&digits));
        // The date and the file's name have their own characters.
        let all = font_chars(&["{date} {file}".to_string()], "2026-10-15", "a-é.pdf");
        assert!(["-", "é", ".", "p", "d", "f", " "].iter().all(|c| all.contains(&c.chars().next().unwrap())));
    }

    #[test]
    fn only_what_the_run_writes_for_a_page_is_taken_for_its_own() {
        struct Fixed;
        impl Face for Fixed {
            fn width(&self, _: char) -> f64 {
                500.0
            }
            fn ascent_descent(&self) -> (f64, f64) {
                (0.8, -0.2)
            }
            fn encode(&self, text: &str) -> String {
                format!("<{text}>")
            }
        }
        let style = Style { size: 10.0, margin: 20.0, color: "0 0 0 rg".to_string() };
        let texts: Vec<String> = ["hl", "", "", "", "fc", ""].iter().map(ToString::to_string).collect();
        let written = format!("q 1 0 0 1 0 0 cm\n{}Q\n", drawing(&Fixed, "Qn9", &style, 200.0, 300.0, &texts));
        assert!(is_ours(written.as_bytes()), "{written}");
        // A page with nothing to say is the matrix and the `Q`.
        assert!(is_ours(b"q 1 0 0 1 0 0 cm\nQ\n"));
        // What the page itself has is not, even if it looks a little like it.
        for body in [
            "BT /F1 12 Tf 20 20 Td (Body) Tj ET\n",
            "q 1 0 0 1 0 0 cm\nBT /F1 12 Tf (Body) Tj ET\nQ\n",
            "q 1 0 0 1 0 0 cm\n/Artifact <</Type /Pagination /Subtype /Watermark>> BDC\nBT ET\nEMC\nQ\n",
            "q 1 0 0 1 0 0 cm\n/Artifact <</Type /Pagination /Subtype /Footer>> BDC\nBT (body text) Tj ET\nEMC\n0 0 m\n",
            "/Artifact <</Type /Pagination /Subtype /Footer>> BDC\nBT ET\nEMC\nQ\n",
            "",
            "q",
        ] {
            assert!(!is_ours(body.as_bytes()), "{body}");
        }
    }
}
