//! Redaction (功能清单第⑧项, 2026-10-20; ISO 32000-2 12.5.6.23): marks that say what is to be taken out of a file, and the taking out.
//!
//! **Marks** are `Redact` annotations: a red outline (their appearance) over the places, `/QuadPoints` for each line of text, `/IC` the colour of the box
//! that will cover them, `/OverlayText` the words on it. They are listed, taken out and undone like any annotation, and saved with the file (an
//! incremental update) for later, or for another program. A mark is made by dragging a rectangle, from the text that is selected (a rectangle for each
//! line), or from every place a word or a kind of number is found ([`FindSpec`]).
//!
//! **Applying** them (`redact_document`) takes the content out of the pages (`retext/redact.rs`) and writes a file afresh as a whole, with only what the
//! catalog reaches (the old bytes are not in it), the thumbnails and private data of pages left out, and, if asked, the information dictionary and the
//! metadata. The encryption is kept; a signed file is refused; the content of the pages may be changed only with permission bit 4 (or the owner).

use super::retext::redact::{Mark, Redactor, bounds_of, rect_of};
use super::{AnnotsChange, Ctx, MarkupKind, change_annots, markup};
use crate::document::{Document, Page};
use crate::error::{Error, Result};
use crate::idmap::IdSet;
use crate::info::decode_text_string;
use crate::object::{Dict, ObjRef, Object};
use crate::render::work::{PAGE_WORK, Work};
use crate::text::{EditScanner, Scan, Shown};
use crate::view::search::{find_all, fold, fold_query};

pub use super::retext::redact::{Counts, MAX_RECTS};

/// Most places a find may make marks of, in all the pages.
pub const MAX_FIND_MARKS: usize = 5_000;
/// Most words and kinds of number one find may look for.
pub const MAX_FIND_WORDS: usize = 64;
/// Most areas given by name (command line).
pub const MAX_AREAS: usize = 10_000;
/// What a whole redaction may cost the work meter, in units of about a nanosecond (the command line; the reader a part of it).
pub const ALLOWANCE: f64 = 2.0e11;
/// Pages' worth of work one page of a redaction may take.
const PAGE_ALLOWANCE: f64 = 4.0 * PAGE_WORK;
/// The colour of the outline that shows a mark, and the room the command line gives to what a redaction makes, in bytes.
pub const OUTLINE: [u8; 3] = [230, 30, 30];
const CLI_ROOM: usize = 768 << 20;
/// The colour a mark covers with unless it says another.
pub const DEFAULT_COLOR: [u8; 3] = [0, 0, 0];

// --- what to look for --------------------------------------------------------------------------------------------

/// A kind of number found by its shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    /// A citizen identity number of China: 18 characters, the first 17 digits and the last a digit or `X`.
    IdCard,
    /// A mobile phone number of China: `1`, one of `3` to `9`, nine more digits.
    Phone,
    /// A bank card number: 16 to 19 digits, which may stand in groups separated by a space or a hyphen.
    BankCard,
}

impl Preset {
    pub fn name(self) -> &'static str {
        match self {
            Preset::IdCard => "idcard",
            Preset::Phone => "phone",
            Preset::BankCard => "bankcard",
        }
    }

    pub fn parse(name: &str) -> Option<Preset> {
        [Preset::IdCard, Preset::Phone, Preset::BankCard].into_iter().find(|p| p.name() == name)
    }
}

/// Words and kinds of number to look for in the text of the pages.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FindSpec {
    pub words: Vec<String>,
    pub presets: Vec<Preset>,
}

/// An area asked for by place: `rect` is `[left, top, right, bottom]`, points of the page `page` (from 0) as shown.
#[derive(Clone, Debug, PartialEq)]
pub struct Area {
    pub page: usize,
    pub rect: [f32; 4],
}

fn digit(c: char) -> bool {
    c.is_ascii_digit()
}

/// Where the kind of number is in the (folded) text: (start, end) in characters, in order, not overlapping.
fn preset_ranges(hay: &[char], p: Preset) -> Vec<(usize, usize)> {
    let at = |i: usize| hay.get(i).copied().unwrap_or(' ');
    let mut out = Vec::new();
    let mut i = 0;
    while i < hay.len() {
        if !digit(at(i)) || (i > 0 && digit(at(i - 1))) {
            i += 1;
            continue;
        }
        let found = match p {
            Preset::IdCard => (at(i) != '0' && (1..17).all(|k| digit(at(i + k))) && (digit(at(i + 17)) || at(i + 17) == 'x') && !digit(at(i + 18))).then_some(i + 18),
            Preset::Phone => (at(i) == '1' && ('3'..='9').contains(&at(i + 1)) && (2..11).all(|k| digit(at(i + k))) && !digit(at(i + 11))).then_some(i + 11),
            Preset::BankCard => {
                // Digits, in groups that a single space or hyphen separates: the match ends at the last digit.
                let (mut j, mut digits, mut end) = (i, 0usize, i);
                while j < hay.len() {
                    if digit(at(j)) {
                        digits += 1;
                        end = j + 1;
                        j += 1;
                    } else if matches!(at(j), ' ' | '-') && digit(at(j + 1)) && digits > 0 {
                        j += 1;
                    } else {
                        break;
                    }
                }
                (16..=19).contains(&digits).then_some(end)
            }
        };
        match found {
            Some(end) => {
                out.push((i, end));
                i = end;
            }
            None => {
                // The whole run of digits (and the separators of a card number) is passed over: a number is not found inside a longer one.
                while i < hay.len() && digit(at(i)) {
                    i += 1;
                }
            }
        }
    }
    out
}

/// The rectangles (one for each line) that cover the characters `a..b` (bytes of the page's text), from the glyphs' boxes.
fn rects_of(scan: &Scan, shown: &Shown, a: usize, b: usize) -> Vec<[f32; 4]> {
    let first = scan.marks.partition_point(|m| m.end <= a);
    let last = scan.marks.partition_point(|m| m.start < b);
    let boxes: Vec<[f32; 4]> = scan.marks.get(first..last).unwrap_or(&[]).iter().filter_map(|m| scan.glyphs.get(m.glyph)).map(|g| shown.glyph_box(g, 0.0, 1.0)).collect();
    crate::view::line_rects(&boxes)
}

/// The places of `spec` in `text`, as (start, end) in bytes, in order, those that overlap made one. `matched` has one flag for each word and then each
/// kind of number of the spec; the ones found here are set.
fn ranges_of(text: &str, spec: &FindSpec, matched: &mut [bool], work: &Work) -> Result<Vec<(usize, usize)>> {
    let (hay, origin) = fold(text);
    let mut byte_of: Vec<usize> = text.char_indices().map(|(b, _)| b).collect();
    byte_of.push(text.len());
    let mut places: Vec<(usize, usize)> = Vec::new();
    let mut spent = false;
    // The words typed (folded like the text) and then the kinds of number.
    let jobs: Vec<std::result::Result<Vec<char>, Preset>> = spec.words.iter().map(|w| Ok(fold_query(w))).chain(spec.presets.iter().map(|&p| Err(p))).collect();
    for (k, job) in jobs.iter().enumerate() {
        let ranges = match job {
            Ok(w) => find_all(&hay, w, MAX_FIND_MARKS, &mut |n| {
                spent |= !work.charge(n as f64 * 4.0);
                !spent
            }),
            Err(p) => preset_ranges(&hay, *p),
        };
        if spent {
            return Err(Error::Limit("searching the page took too much work".to_string()));
        }
        if let Some(flag) = matched.get_mut(k) {
            *flag |= !ranges.is_empty();
        }
        for (s, e) in ranges {
            let (Some(&cs), Some(&ce)) = (origin.get(s), e.checked_sub(1).and_then(|l| origin.get(l))) else { continue };
            if let (Some(&bs), Some(&be)) = (byte_of.get(cs as usize), byte_of.get(ce as usize + 1)) {
                places.push((bs, be));
            }
        }
    }
    // Places that overlap are one place.
    crate::order::sort_by_key(&mut places, |p| (p.0 as u64) << 32 | p.1 as u64);
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for p in places {
        match merged.last_mut() {
            Some(l) if p.0 < l.1 => l.1 = l.1.max(p.1),
            _ => merged.push(p),
        }
    }
    Ok(merged)
}

/// The places of `spec` on a page read as `scan`: for each, the rectangles of its lines.
fn find_in(scan: &Scan, shown: &Shown, spec: &FindSpec, matched: &mut [bool], work: &Work) -> Result<Vec<Vec<[f32; 4]>>> {
    Ok(ranges_of(&scan.text, spec, matched, work)?.into_iter().map(|(a, b)| rects_of(scan, shown, a, b)).filter(|r| !r.is_empty()).collect())
}

/// What an annotation says in words: its contents, rich text, tool tip, subject and the value of a field (when they are strings), and for a field or a free
/// text note the words its appearance draws.
fn annot_text(doc: &Document, scanner: &mut EditScanner<'_>, d: &Dict, work: &Work) -> String {
    let mut out = String::new();
    for key in ["Contents", "RC", "V", "DV", "TU", "Subj"] {
        if let Some(Object::String(s)) = d.get(key).and_then(|o| doc.resolve(o).ok()) {
            out.push_str(&decode_text_string(&s.bytes));
            out.push('\n');
        }
    }
    if matches!(d.get_name("Subtype"), Some(n) if n == "Widget" || n == "FreeText")
        && let Some(Object::Dict(ap)) = d.get("AP").and_then(|a| doc.resolve(a).ok())
        && let Some(Object::Ref(n)) = ap.get("N")
        && let Ok(Object::Stream(form)) = doc.get(*n)
    {
        let mut page = Dict::new();
        page.set("Contents", Object::Ref(*n));
        if let Some(r) = form.dict.get("Resources") {
            page.set("Resources", r.clone());
        }
        if let Ok(scan) = scanner.scan(&Page::new(*n, page), work) {
            out.push_str(&scan.text);
        }
    }
    out
}

/// The annotations of a page whose words hold a place of `spec`: for each, its rectangle (as the place to take out: the annotation goes with it).
fn find_annots(doc: &Document, scanner: &mut EditScanner<'_>, page: &Page, shown: &Shown, spec: &FindSpec, matched: &mut [bool], work: &Work) -> Result<Vec<Vec<[f32; 4]>>> {
    let Some(Object::Array(items)) = page.dict.get("Annots").and_then(|a| doc.resolve(a).ok()) else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    for item in &items {
        let Ok(Object::Dict(d)) = doc.resolve(item) else { continue };
        let Some(r) = rect_of(doc, &d).filter(|_| !d.get_name("Subtype").is_some_and(|n| n == "Redact")) else { continue };
        let text = annot_text(doc, scanner, &d, work);
        if text.trim().is_empty() || ranges_of(&text, spec, matched, work)?.is_empty() {
            continue;
        }
        let b = bounds_of([(r[0], r[1]), (r[2], r[1]), (r[2], r[3]), (r[0], r[3])].map(|(x, y)| shown.map(x, y)));
        if b.iter().all(|v| v.is_finite()) {
            out.push(vec![[b[0] as f32, b[1] as f32, b[2] as f32, b[3] as f32]]);
        }
    }
    Ok(out)
}

// --- the marks ---------------------------------------------------------------------------------------------------

/// A mark over every place the words and kinds of number of `spec` are found, on every page: one mark for each place. One step.
pub(super) fn mark_found(ctx: &mut Ctx<'_>, pages: &[Page], spec: &FindSpec, author: &str, now: &str) -> Result<()> {
    check_spec(spec)?;
    let mut scanner = EditScanner::new(ctx.doc);
    let mut matched = vec![false; spec.words.len() + spec.presets.len()];
    let mut total = 0usize;
    let mut missed: Vec<usize> = Vec::new();
    for (number, page) in pages.iter().enumerate() {
        let own = Work::with_allowance(PAGE_WORK);
        let scan = {
            let _budget = ctx.doc.page_decode_budget();
            scanner.scan(page, &own)
        };
        ctx.work.spend(own.used());
        let scan = match scan {
            Ok(s) => s,
            // A page too heavy to read is passed over, as a search does (and said).
            Err(Error::Limit(_)) => {
                missed.push(number + 1);
                continue;
            }
            Err(e) => return Err(e),
        };
        if scan.unreadable > 0 || unread(ctx.doc, page, &scan) {
            missed.push(number + 1);
        }
        let shown = Shown::of(page);
        let mut places = find_in(&scan, &shown, spec, &mut matched, ctx.work)?;
        places.extend(find_annots(ctx.doc, &mut scanner, page, &shown, spec, &mut matched, ctx.work)?);
        if places.is_empty() {
            continue;
        }
        total += places.len();
        if total > MAX_FIND_MARKS {
            return Err(Error::Limit(format!("more than {MAX_FIND_MARKS} places found: look for something more particular")));
        }
        let mut made = Vec::with_capacity(places.len());
        for rects in &places {
            made.extend(markup(ctx, &shown, MarkupKind::Redact, rects, OUTLINE, author, now)?.pop());
        }
        change_annots(ctx, pages, page, AnnotsChange::Add(made))?;
    }
    if total == 0 {
        return Err(Error::Invalid("nothing was found".to_string()));
    }
    if !missed.is_empty() {
        let list: Vec<String> = missed.iter().take(20).map(ToString::to_string).collect();
        ctx.notes.push(format!("Pages {}{} were not read completely (too heavy, or text that cannot be read): places on them may be missed.", list.join(", "), if missed.len() > 20 { ", ..." } else { "" }));
    }
    ctx.notes.push(format!("{total}"));
    Ok(())
}

/// Is a content stream of the page one that cannot be read (a filter that is not known)? What it shows is then not seen.
fn unread(doc: &Document, page: &Page, scan: &Scan) -> bool {
    scan.sources.first().is_some_and(|s| s.streams.len() < super::content::contents_list(doc, page.dict.get("Contents")).len())
}

fn check_spec(spec: &FindSpec) -> Result<()> {
    if spec.words.len() + spec.presets.len() > MAX_FIND_WORDS {
        return Err(Error::Limit(format!("at most {MAX_FIND_WORDS} words and kinds of number at a time")));
    }
    if spec.words.iter().any(|w| fold_query(w).is_empty()) {
        return Err(Error::Invalid("there is nothing to look for".to_string()));
    }
    if spec.words.is_empty() && spec.presets.is_empty() {
        return Err(Error::Invalid("there is nothing to look for".to_string()));
    }
    Ok(())
}

// --- applying them -----------------------------------------------------------------------------------------------

/// What to redact.
#[derive(Clone, Debug, Default)]
pub struct RedactSpec {
    pub find: FindSpec,
    pub areas: Vec<Area>,
    /// The colour of the boxes that have none of their own, and the words on those boxes (empty: none).
    pub color: [u8; 3],
    pub label: String,
    /// Apply the `Redact` annotations of the file.
    pub use_marks: bool,
    /// Leave the information dictionary and the metadata out of the new file.
    pub clear_metadata: bool,
    /// Take the attachments and the scripts (JavaScript) out of the new file.
    pub remove_active: bool,
}

/// What a redaction did.
#[derive(Debug)]
pub struct Redacted {
    /// The new file.
    pub data: Vec<u8>,
    /// For each page that had something to take out (from 0): what was taken.
    pub pages: Vec<(usize, Counts)>,
    /// How many marks were applied, and which of the words and kinds of number asked for were found nowhere.
    pub marks: usize,
    pub unfound: Vec<String>,
    pub warnings: Vec<String>,
    /// What the work meter was charged.
    pub work_used: f64,
}

fn rgb_of(doc: &Document, o: Option<&Object>) -> Option<[u8; 3]> {
    let Object::Array(a) = doc.resolve(o?).ok()? else { return None };
    let n: Vec<f64> = a.iter().filter_map(|o| doc.resolve(o).ok().and_then(|o| o.as_f64())).map(|v| v.clamp(0.0, 1.0)).collect();
    let byte = |v: f64| (v * 255.0).round() as u8;
    match n.as_slice() {
        [g] => Some([byte(*g); 3]),
        [r, g, b] => Some([byte(*r), byte(*g), byte(*b)]),
        [c, m, y, k] => Some([byte((1.0 - c) * (1.0 - k)), byte((1.0 - m) * (1.0 - k)), byte((1.0 - y) * (1.0 - k))]),
        _ => None,
    }
}

/// The `Redact` annotations of a page as marks.
fn marks_of_page(doc: &Document, page: &Page, shown: &Shown, spec: &RedactSpec) -> Vec<Mark> {
    let Some(Object::Array(items)) = page.dict.get("Annots").and_then(|a| doc.resolve(a).ok()) else { return Vec::new() };
    let mut out = Vec::new();
    for item in &items {
        let Ok(Object::Dict(d)) = doc.resolve(item) else { continue };
        if !d.get_name("Subtype").is_some_and(|n| n == "Redact") {
            continue;
        }
        let numbers = |key: &str| -> Vec<f64> {
            match d.get(key).and_then(|o| doc.resolve(o).ok()) {
                Some(Object::Array(a)) => a.iter().filter_map(|o| doc.resolve(o).ok().and_then(|o| o.as_f64())).collect(),
                _ => Vec::new(),
            }
        };
        let to_shown = |pts: &[(f64, f64)]| -> Option<[f32; 4]> {
            let b = pts.iter().map(|&(x, y)| shown.map(x, y)).fold([f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY], |b, (x, y)| [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)]);
            b.iter().all(|v| v.is_finite()).then_some([b[0] as f32, b[1] as f32, b[2] as f32, b[3] as f32])
        };
        let quads = numbers("QuadPoints");
        let mut rects: Vec<[f32; 4]> = quads.as_chunks::<8>().0.iter().filter_map(|q| to_shown(&[(q[0], q[1]), (q[2], q[3]), (q[4], q[5]), (q[6], q[7])])).collect();
        if rects.is_empty()
            && let [x0, y0, x1, y1] = numbers("Rect").as_slice()
            && let Some(r) = to_shown(&[(*x0, *y0), (*x1, *y0), (*x1, *y1), (*x0, *y1)])
        {
            rects.push(r);
        }
        if rects.is_empty() {
            continue;
        }
        let label = match d.get("OverlayText").and_then(|o| doc.resolve(o).ok()) {
            Some(Object::String(s)) => decode_text_string(&s.bytes),
            _ => spec.label.clone(),
        };
        out.push(Mark { rects, color: rgb_of(doc, d.get("IC")).unwrap_or(spec.color), label });
    }
    out
}

/// Take the form widgets `removed` out of the form: out of the field tree (a field none of whose widgets is left goes too, one that keeps some
/// loses its value), out of the calculation order, and the XFA data (which holds the values) with them.
fn unlink_fields(ctx: &mut Ctx<'_>, removed: &[ObjRef]) -> Result<()> {
    if removed.is_empty() {
        return Ok(());
    }
    let doc = ctx.doc;
    let mut catalog = doc.catalog()?;
    let root = doc.trailer().get("Root").and_then(Object::as_obj_ref);
    let Some(entry) = catalog.get("AcroForm").cloned() else { return Ok(()) };
    let (acro_ref, mut acro) = match (&entry, doc.resolve(&entry)) {
        (Object::Ref(r), Ok(Object::Dict(d))) => (Some(*r), d),
        (Object::Dict(d), _) => (None, d.clone()),
        _ => return Ok(()),
    };
    let gone: &[ObjRef] = removed;
    let mut changed: Vec<(u32, Dict)> = Vec::new();
    let mut budget = 200_000usize;

    fn prune(doc: &Document, list: Vec<Object>, gone: &[ObjRef], depth: usize, changed: &mut Vec<(u32, Dict)>, budget: &mut usize) -> Result<(Vec<Object>, bool)> {
        let mut out = Vec::with_capacity(list.len());
        let mut below = false;
        for item in list {
            *budget = budget.checked_sub(1).ok_or_else(|| Error::Limit("the form has too many fields".to_string()))?;
            if item.as_obj_ref().is_some_and(|r| gone.contains(&r)) {
                below = true;
                continue;
            }
            if depth < 32
                && let Ok(Object::Dict(mut d)) = doc.resolve(&item)
                && let Some(Object::Array(kids)) = d.get("Kids").and_then(|k| doc.resolve(k).ok())
            {
                let (kept, removed_below) = prune(doc, kids, gone, depth + 1, changed, budget)?;
                if removed_below {
                    below = true;
                    if kept.is_empty() {
                        continue;
                    }
                    d.set("Kids", Object::Array(kept));
                    d.remove("V");
                    d.remove("DV");
                    if let Some(r) = item.as_obj_ref() {
                        changed.push((r.num, d));
                    }
                }
            }
            out.push(item);
        }
        Ok((out, below))
    }

    let (fields_ref, items) = match acro.get("Fields").cloned() {
        Some(Object::Ref(r)) => (Some(r), match doc.get(r) {
            Ok(Object::Array(a)) => a,
            _ => Vec::new(),
        }),
        Some(Object::Array(a)) => (None, a),
        _ => (None, Vec::new()),
    };
    let (kept, below) = prune(doc, items, gone, 0, &mut changed, &mut budget)?;
    if !below {
        return Ok(());
    }
    match fields_ref {
        Some(r) => ctx.objects.push((r.num, Object::Array(kept))),
        None => acro.set("Fields", Object::Array(kept)),
    }
    if let Some(Object::Array(co)) = acro.get("CO").and_then(|c| doc.resolve(c).ok()) {
        acro.set("CO", Object::Array(co.into_iter().filter(|o| !o.as_obj_ref().is_some_and(|r| gone.contains(&r))).collect()));
    }
    acro.remove("XFA");
    match (acro_ref, root) {
        (Some(r), _) => ctx.objects.push((r.num, Object::Dict(acro))),
        (None, Some(root)) => {
            catalog.set("AcroForm", Object::Dict(acro));
            ctx.objects.push((root.num, Object::Dict(catalog)));
        }
        (None, None) => return Err(Error::Invalid("the file has no catalog to write the form into".to_string())),
    }
    for (num, d) in changed {
        ctx.objects.push((num, Object::Dict(d)));
    }
    Ok(())
}

/// Redact the file `doc` as `spec` says: the pages' content that lies under the marks is taken out, and the new file is written as a whole. `password` is the
/// one the file was opened with; `work` pays for it; `room` is how many bytes the new objects of the pages may take together.
pub fn redact_document(doc: &Document, password: &str, spec: &RedactSpec, work: &Work, room: usize) -> Result<Redacted> {
    super::content::check_content_allowed(doc, password)?;
    crate::ops::ensure_unsigned(doc)?;
    if !spec.find.words.is_empty() || !spec.find.presets.is_empty() {
        check_spec(&spec.find)?;
    }
    if spec.areas.len() > MAX_AREAS {
        return Err(Error::Limit(format!("at most {MAX_AREAS} areas")));
    }
    let pages = doc.pages()?;
    if let Some(a) = spec.areas.iter().find(|a| a.page >= pages.len()) {
        return Err(Error::Invalid(format!("there is no page {} (the file has {})", a.page + 1, pages.len())));
    }
    let finding = !spec.find.words.is_empty() || !spec.find.presets.is_empty();
    let mut ctx = Ctx { doc, next: doc.next_free_number(), objects: Vec::new(), work, room, used: 0, notes: Vec::new() };
    let mut redactor = Redactor::new(doc);
    let mut reports: Vec<(usize, Counts)> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut matched = vec![false; spec.find.words.len() + spec.find.presets.len()];
    let mut total_marks = 0usize;
    // The pages that were applied to.
    let mut applied: Vec<ObjRef> = Vec::new();
    for (index, page) in pages.iter().enumerate() {
        let shown = Shown::of(page);
        let mut marks: Vec<Mark> = if spec.use_marks { marks_of_page(doc, page, &shown, spec) } else { Vec::new() };
        for a in spec.areas.iter().filter(|a| a.page == index) {
            marks.push(Mark { rects: vec![a.rect], color: spec.color, label: spec.label.clone() });
        }
        if marks.is_empty() && !finding {
            continue;
        }
        let own = Work::with_allowance(PAGE_ALLOWANCE.min(work.remaining()));
        let scan = redactor.scan(doc, page, &own);
        work.spend(own.used());
        let scan = match scan {
            Ok(s) => s,
            Err(Error::Limit(m)) if marks.is_empty() => {
                warnings.push(format!("page {} was not searched: {m}", index + 1));
                continue;
            }
            Err(e) if matches!(e, Error::Limit(_)) => return Err(Error::Limit(format!("page {}: {e}", index + 1))),
            Err(e) => return Err(e),
        };
        if finding {
            let found = find_in(&scan, &shown, &spec.find, &mut matched, work)?.into_iter().chain(find_annots(doc, redactor.scanner(), page, &shown, &spec.find, &mut matched, work)?);
            marks.extend(found.map(|rects| Mark { rects, color: spec.color, label: spec.label.clone() }));
            if marks.is_empty() && (scan.unreadable > 0 || unread(doc, page, &scan)) {
                warnings.push(format!("page {} has text that cannot be read, so it was not searched", index + 1));
            }
        }
        if marks.is_empty() {
            continue;
        }
        total_marks += marks.len();
        let counts = redactor.apply(&mut ctx, index, page, &scan, &marks).map_err(|e| match e {
            Error::Invalid(m) => Error::Invalid(format!("page {}: {m}", index + 1)),
            other => other,
        })?;
        reports.push((index, counts));
        applied.push(page.obj_ref);
    }
    if total_marks == 0 {
        return Err(Error::Invalid("nothing to redact: no place was found".to_string()));
    }
    unlink_fields(&mut ctx, &redactor.widgets)?;
    strip_structure(&mut ctx, &applied);
    // The tables that a font has for copying text from it (`/ToUnicode`) still list the characters that were taken out.
    if redactor.touched.iter().any(|f| matches!(doc.get(*f), Ok(Object::Dict(d)) if d.contains_key("ToUnicode"))) {
        warnings.push("The fonts that lost text still list the removed characters in their /ToUnicode tables: a short word can be read back as a set of characters.".to_string());
    }
    let (files, scripts, _) = count_active(doc, &pages, 2_000_000);
    if files + scripts > 0 {
        let what = format!("{files} attachment(s) and {scripts} script(s) (JavaScript)");
        if spec.remove_active {
            remove_active(&mut ctx, &pages);
            warnings.push(format!("Took out {what}."));
        } else {
            warnings.push(format!("The file has {what}, which can hold the removed text; ask for them to be taken out."));
        }
    }
    warnings.extend(redactor.warnings());
    warnings.append(&mut redactor.notes);
    let unfound: Vec<String> = spec
        .find
        .words
        .iter()
        .cloned()
        .chain(spec.find.presets.iter().map(|p| p.name().to_string()))
        .zip(&matched)
        .filter(|(_, m)| !**m)
        .map(|(w, _)| w)
        .collect();
    // The redaction is put in front of the document's objects to be written, and taken off again: the document is as it was.
    let changes = super::commit(doc, std::mem::take(&mut ctx.objects));
    let out = crate::ops::rewrite_redacted(doc, spec.clear_metadata);
    // (What is looked at is the file as it is to be written.)
    let needles: Vec<String> = redactor.removed.split('\n').map(|r| r.trim().chars().take(100).collect::<String>()).chain(spec.find.words.iter().cloned()).collect();
    let left = if out.is_ok() { residue(doc, &needles, spec.clear_metadata) } else { None };
    super::replay(doc, &changes, false);
    let out = out?;
    warnings.extend(out.warnings.iter().map(|w| w.0.clone()));
    warnings.extend(left);
    Ok(Redacted { data: out.data, pages: reports, marks: total_marks, unfound, warnings, work_used: work.used() })
}

// --- what is left in the file ------------------------------------------------------------------------------------

/// Every object reachable from `root`, once each (not through a page or a parent, and so not into the page lists that an action points at), told to `f`
/// as it is. The strings of bookmarks, names and actions, and what attachments hold, are in them.
fn walk(doc: &Document, root: &Object, seen: &mut IdSet<ObjRef>, budget: &mut usize, f: &mut dyn FnMut(&Object)) {
    let mut stack = vec![root.clone()];
    while let Some(o) = stack.pop() {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        if let Object::Ref(r) = &o
            && !seen.insert(*r)
        {
            continue;
        }
        let Ok(v) = doc.resolve(&o) else { continue };
        let dict = match &v {
            Object::Dict(d) => Some(d),
            Object::Stream(s) => Some(&s.dict),
            _ => None,
        };
        if dict.is_some_and(|d| d.get_name("Type").is_some_and(|t| matches!(t.as_bytes(), b"Page" | b"Pages" | b"Catalog"))) {
            continue;
        }
        f(&v);
        match (&v, dict) {
            (Object::Array(a), _) => stack.extend(a.iter().cloned()),
            (_, Some(d)) => stack.extend(d.iter().filter(|(k, _)| !matches!(k.0.as_slice(), b"Parent" | b"P" | b"Prev" | b"Pg" | b"Obj" | b"ParentTree")).map(|(_, v)| v.clone())),
            _ => {}
        }
    }
}

/// Where a file holds what it says and does besides its pages' content: each entry of the catalog (what it leads to), the document information, and the actions
/// and notes of the pages, each with a name for saying where.
fn places(doc: &Document, pages: &[Page]) -> Vec<(&'static str, Object)> {
    let mut out: Vec<(&'static str, Object)> = Vec::new();
    for (k, v) in doc.catalog().unwrap_or_default().iter() {
        let name = match k.0.as_slice() {
            b"Pages" | b"Type" => continue,
            b"Outlines" => "bookmarks",
            b"Names" | b"Dests" => "names (of destinations, attachments, scripts)",
            b"Metadata" => "the document metadata",
            b"StructTreeRoot" => "the structure tree",
            _ => "the catalog (form, actions)",
        };
        out.push((name, v.clone()));
    }
    out.extend(doc.info_entry().map(|i| ("the document information", i)));
    for page in pages {
        out.extend(page.dict.get("AA").map(|a| ("actions", a.clone())));
        if let Some(Object::Array(items)) = page.dict.get("Annots").and_then(|a| doc.resolve(a).ok()) {
            out.extend(items.into_iter().map(|a| ("the notes of the pages", a)));
        }
    }
    out
}

fn is_script(o: &Object) -> bool {
    matches!(o, Object::Dict(d) if d.get_name("S").is_some_and(|s| s == "JavaScript"))
}

/// How many attachments (file specifications that hold a file) and scripts (JavaScript actions) the file has, looking at `budget` objects at the most
/// (the third is whether all were looked at).
pub(crate) fn count_active(doc: &Document, pages: &[Page], mut budget: usize) -> (usize, usize, bool) {
    let (mut files, mut scripts) = (0usize, 0usize);
    let mut seen = IdSet::new();
    for (_, root) in places(doc, pages) {
        walk(doc, &root, &mut seen, &mut budget, &mut |o| {
            files += usize::from(matches!(o, Object::Dict(d) if d.contains_key("EF")));
            scripts += usize::from(is_script(o));
        });
    }
    (files, scripts, budget > 0)
}

/// Does what `o` is (or leads to) have a script?
fn has_script(doc: &Document, o: &Object) -> bool {
    let mut found = false;
    walk(doc, o, &mut IdSet::new(), &mut 100_000, &mut |v| found |= is_script(v));
    found
}

/// Change the dictionary of object `r` as it is to be written (the copy that is to be written, if there is one, else the file's); `f` says if it changed it.
fn edit_dict(ctx: &mut Ctx<'_>, r: ObjRef, f: &mut dyn FnMut(&mut Dict) -> bool) {
    let at = ctx.objects.iter().rposition(|o| o.0 == r.num);
    let mut d = match at.and_then(|i| ctx.objects.get(i)) {
        Some((_, Object::Dict(d))) => d.clone(),
        _ => match ctx.doc.get(r) {
            Ok(Object::Dict(d)) => d,
            _ => return,
        },
    };
    if !f(&mut d) {
        return;
    }
    match at.and_then(|i| ctx.objects.get_mut(i)) {
        Some(slot) => *slot = (r.num, Object::Dict(d)),
        None => ctx.objects.push((r.num, Object::Dict(d))),
    }
}

/// Take the attachments and the scripts out: the name trees, the actions of the document and the pages and the annotations, the associated files; an
/// annotation that holds a file becomes an empty one.
fn remove_active(ctx: &mut Ctx<'_>, pages: &[Page]) {
    let doc = ctx.doc;
    let names_out = |n: &mut Dict| {
        let a = n.remove("EmbeddedFiles").is_some();
        n.remove("JavaScript").is_some() | a
    };
    let actions_out = |d: &mut Dict, keys: &[&str]| keys.iter().fold(false, |any, k| if d.get(k).is_some_and(|o| has_script(doc, o)) { d.remove(k).is_some() | any } else { any });
    if let Some(root) = doc.trailer().get("Root").and_then(Object::as_obj_ref) {
        let names_ref = doc.catalog().ok().and_then(|c| c.get("Names").and_then(Object::as_obj_ref));
        edit_dict(ctx, root, &mut |c| {
            let mut changed = actions_out(c, &["OpenAction", "AA"]) | c.remove("AF").is_some();
            if let Some(Object::Dict(n)) = c.get_mut("Names") {
                changed |= names_out(n);
            }
            changed
        });
        if let Some(n) = names_ref {
            edit_dict(ctx, n, &mut |d| names_out(d));
        }
    }
    for page in pages {
        edit_dict(ctx, page.obj_ref, &mut |p| actions_out(p, &["AA"]) | p.remove("AF").is_some());
        let Some(Object::Array(items)) = page.dict.get("Annots").and_then(|a| doc.resolve(a).ok()) else { continue };
        for r in items.iter().filter_map(Object::as_obj_ref) {
            if matches!(doc.get(r), Ok(Object::Dict(a)) if a.get_name("Subtype").is_some_and(|s| s == "FileAttachment")) {
                let mut stub = Dict::new();
                stub.set("Type", Object::from("Annot"));
                stub.set("Subtype", Object::from("Text"));
                stub.set("F", Object::Integer(2));
                ctx.objects.push((r.num, Object::Dict(stub)));
            } else {
                edit_dict(ctx, r, &mut |a| actions_out(a, &["A", "AA"]));
            }
        }
    }
}

/// Where the text that was taken out still is, in what the redaction does not change (bookmarks, names, attachments, scripts, the structure tree, the notes
/// and fields of the pages, the document's information and metadata). One line, or none.
fn residue(doc: &Document, needles: &[String], clear_metadata: bool) -> Option<String> {
    let needles: Vec<(&String, Vec<char>)> = needles.iter().map(|n| (n, fold_query(n))).filter(|(_, f)| f.len() >= 2).collect();
    let pages = doc.pages().unwrap_or_default();
    let mut found: Vec<(&str, Vec<&String>)> = Vec::new();
    let mut budget = 1_000_000usize;
    for (label, root) in places(doc, &pages).iter().filter(|_| !needles.is_empty()) {
        if clear_metadata && label.starts_with("the document") {
            continue;
        }
        let mut hit: Vec<&String> = Vec::new();
        walk(doc, root, &mut IdSet::new(), &mut budget, &mut |o| {
            let text = match o {
                Object::String(s) => decode_text_string(&s.bytes),
                Object::Stream(s) if s.data.len() < 8 << 20 && !s.dict.get_name("Subtype").is_some_and(|t| t == "Image" || t == "Form") => String::from_utf8_lossy(&doc.decode_stream(s).unwrap_or_default()).into_owned(),
                _ => return,
            };
            let (hay, _) = fold(&text);
            for (n, f) in &needles {
                if !hit.contains(n) && !find_all(&hay, f, 1, &mut |_| true).is_empty() {
                    hit.push(*n);
                }
            }
        });
        match found.iter_mut().find(|(l, _)| l == label) {
            Some((_, have)) => {
                for n in hit {
                    if !have.contains(&n) {
                        have.push(n);
                    }
                }
            }
            None if !hit.is_empty() => found.push((*label, hit)),
            None => {}
        }
    }
    let list: Vec<String> = found.iter().map(|(l, n)| format!("{l} (\"{}\")", n.iter().take(3).map(|s| s.chars().take(24).collect::<String>()).collect::<Vec<_>>().join("\", \""))).collect();
    (!list.is_empty()).then(|| format!("The removed text is still in the file, in {}; those places are not changed.", list.join("; ")))
}

/// The words that the structure tree's elements for the content of `pages` carry (`/Alt`, `/ActualText`, `/E`) are read out as the page's: they are taken out.
fn strip_structure(ctx: &mut Ctx<'_>, pages: &[ObjRef]) {
    let doc = ctx.doc;
    let mut pages = pages.to_vec();
    crate::order::sort_by_key(&mut pages, |r| u64::from(r.num));
    let Some(root) = doc.catalog().ok().and_then(|c| c.get("StructTreeRoot").cloned()) else { return };
    let (mut seen, mut budget): (IdSet<ObjRef>, usize) = (IdSet::new(), 400_000);
    let mut stack: Vec<(Object, Option<ObjRef>)> = vec![(root, None)];
    while let Some((node, inherited)) = stack.pop() {
        budget = match budget.checked_sub(1) {
            Some(b) => b,
            None => return,
        };
        let me = node.as_obj_ref();
        if me.is_some_and(|r| !seen.insert(r)) {
            continue;
        }
        match doc.resolve(&node) {
            Ok(Object::Array(a)) => stack.extend(a.into_iter().map(|o| (o, inherited))),
            Ok(Object::Dict(mut d)) => {
                let pg = d.get("Pg").and_then(Object::as_obj_ref).or(inherited);
                if pg.is_some_and(|p| pages.binary_search_by_key(&p.num, |r| r.num).is_ok())
                    && ["Alt", "ActualText", "E"].map(|k| d.remove(k).is_some()).contains(&true)
                    && let Some(r) = me
                {
                    ctx.objects.push((r.num, Object::Dict(d.clone())));
                }
                stack.extend(d.get("K").map(|k| (k.clone(), pg)));
            }
            _ => {}
        }
    }
}

/// [`redact_document`] for the command line: its own meter and room.
pub fn redact_file(doc: &Document, password: &str, spec: &RedactSpec) -> Result<Redacted> {
    redact_document(doc, password, spec, &Work::with_allowance(ALLOWANCE), CLI_ROOM)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn identity_numbers_are_eighteen_characters_and_not_part_of_a_longer_number() {
        // (The text is folded before it is looked in: lower case, as the search does.)
        let text = fold("号码 11010519491231002X 和 110105194912310021, 1101051949123100212 不是").0;
        let cut: Vec<String> = preset_ranges(&text, Preset::IdCard).iter().map(|&(a, b)| text[a..b].iter().collect()).collect();
        assert_eq!(cut, ["11010519491231002x", "110105194912310021"]);
        // Not for a leading zero, and not for a letter in the middle.
        assert!(preset_ranges(&chars("010105194912310021 11010519491x310021"), Preset::IdCard).is_empty());
    }

    #[test]
    fn phone_numbers_start_with_one_and_three_to_nine_and_have_eleven_digits() {
        let text = chars("13812345678, 19987654321 12345678901 138123456789");
        assert_eq!(preset_ranges(&text, Preset::Phone), vec![(0, 11), (13, 24)]);
    }

    #[test]
    fn bank_cards_are_sixteen_to_nineteen_digits_in_groups() {
        let text = chars("卡 6222 0200 1234 5678 901 ，另 6222-0200-1234-5678 和 123456789012345 以及 12345678901234567890");
        let found = preset_ranges(&text, Preset::BankCard);
        let cut: Vec<String> = found.iter().map(|&(a, b)| text[a..b].iter().collect()).collect();
        assert_eq!(cut, ["6222 0200 1234 5678 901", "6222-0200-1234-5678"]);
    }
}
