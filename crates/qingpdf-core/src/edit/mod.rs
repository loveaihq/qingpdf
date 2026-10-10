//! Editing (layer 4, step 4a): annotations added to a file and taken out of it, saved as an incremental update.
//!
//! An edit is worked out against the document as it stands and gives the objects it makes or changes ([`apply`]): the new
//! annotations and their appearance streams (12.5.5), the page (or the annotation array, if that is an object of its own) with
//! the new list. They are put in front of the document's own objects, in memory ([`commit`]); the file is not touched. Saving
//! writes the final state of all of them as one incremental update ([`save_section`], [`incremental`]), a cross-reference section
//! that points back at the old one, and the saved file is then the document's file.
//!
//! Every place is in points on the page as it is shown, as the boxes of the characters are ([`crate::view::CharBoxes`]):
//! the origin at the top left, y downwards, the page's own `/Rotate` applied. They are turned into default user space here.

mod appearance;
pub(crate) mod incremental;
pub(crate) mod signature;

use std::collections::HashSet;
use std::rc::Rc;

use miniz_oxide::deflate::compress_to_vec_zlib;

use crate::document::{Document, Page};
use crate::error::{Error, Result};
use crate::object::{Dict, MAX_OBJECT_NUMBER, Name, ObjRef, Object, PdfString, Stream};
use crate::text::Shown;

use appearance::{Appearance, Quad};
/// Most points one drawing has (all its strokes together); a longer one is thinned out, its ends kept.
pub const MAX_INK_POINTS: usize = 8_192;
/// Most strokes in one drawing.
pub const MAX_INK_STROKES: usize = 64;
/// Most lines of text one markup covers.
pub const MAX_QUADS: usize = 4_000;
/// Most characters of the text of a note; the rest is cut off.
pub const MAX_NOTE_CHARS: usize = 4_000;
/// Most characters of an author's name.
pub const MAX_AUTHOR_CHARS: usize = 64;
/// The narrowest and the widest line of a drawing, in points.
pub const MIN_INK_WIDTH: f64 = 0.25;
pub const MAX_INK_WIDTH: f64 = 36.0;
/// Most entries of a page's `/Annots` an annotation is added to.
pub const MAX_ANNOTS_ON_PAGE: usize = 20_000;
/// The size of the box a note's text opens in, in points.
const POPUP_WIDTH: f64 = 200.0;
const POPUP_HEIGHT: f64 = 120.0;
/// The biggest number a place may have (points); a bigger one is not a place on a page.
const LIMIT: f64 = 1.0e7;

/// The four kinds of text markup (12.5.6.10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum MarkupKind {
    Highlight = 0,
    Underline = 1,
    StrikeOut = 2,
    Squiggly = 3,
}

impl MarkupKind {
    fn subtype(self) -> &'static str {
        match self {
            MarkupKind::Highlight => "Highlight",
            MarkupKind::Underline => "Underline",
            MarkupKind::StrikeOut => "StrikeOut",
            MarkupKind::Squiggly => "Squiggly",
        }
    }
}

/// One change to a file.
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    /// Markup over lines of text: one `[left, top, right, bottom]` for each line.
    Markup { page: u32, kind: MarkupKind, rects: Vec<[f32; 4]>, color: [u8; 3], author: String },
    /// A freehand drawing: strokes of points, `width` points wide.
    Ink { page: u32, strokes: Vec<Vec<[f32; 2]>>, color: [u8; 3], width: f32, author: String },
    /// A note whose icon has its top left corner at `at`, with `text`.
    Note { page: u32, at: [f32; 2], text: String, color: [u8; 3], author: String },
    /// Take an annotation out of the page: the `index`-th entry of its `/Annots`, which must be object number `num`
    /// (0 for an annotation written in the array itself). A popup that belongs to it goes with it.
    Delete { page: u32, index: u32, num: u32 },
}

impl Edit {
    pub fn page(&self) -> u32 {
        match self {
            Edit::Markup { page, .. } | Edit::Ink { page, .. } | Edit::Note { page, .. } | Edit::Delete { page, .. } => *page,
        }
    }
}

/// Now, as a PDF date (7.9.4): `D:YYYYMMDDHHmmSSZ`, in UTC.
pub fn pdf_date_now() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    pdf_date(secs)
}

/// A PDF date for `secs` seconds after 1970-01-01 00:00:00 UTC.
pub fn pdf_date(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rest = secs % 86_400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("D:{year:04}{month:02}{day:02}{:02}{:02}{:02}Z", rest / 3_600, rest % 3_600 / 60, rest % 60)
}

/// A text string (7.9.2.2): plain ASCII is PDFDocEncoding already; anything else goes as UTF-16BE behind its byte order
/// mark. Control characters other than the line ends and tab become spaces.
fn text_string(s: &str) -> Object {
    let clean: String = s.chars().map(|c| if c.is_control() && !matches!(c, '\n' | '\r' | '\t') { ' ' } else { c }).collect();
    if clean.is_ascii() {
        return Object::String(PdfString::literal(clean.into_bytes()));
    }
    let mut bytes = vec![0xFE, 0xFF];
    for unit in clean.encode_utf16() {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }
    Object::String(PdfString::hex(bytes))
}

fn reals(values: &[f64]) -> Object {
    Object::Array(values.iter().map(|&v| Object::Real(v)).collect())
}

fn unit(rgb: [u8; 3]) -> [f64; 3] {
    [f64::from(rgb[0]) / 255.0, f64::from(rgb[1]) / 255.0, f64::from(rgb[2]) / 255.0]
}

fn finite_place(v: f32) -> Result<f64> {
    let v = f64::from(v);
    if v.is_finite() && v.abs() <= LIMIT { Ok(v) } else { Err(Error::Invalid("a place is not a number on the page".to_string())) }
}

/// Points of a stroke closer than this to the one before are left out (points on the page as shown), and a point that lies
/// within `FLAT` of the line between its neighbours too.
const MIN_INK_STEP: f64 = 0.5;
const FLAT: f64 = 0.05;
/// Bit 8 of the flags of an annotation (Table 165), Locked: it is not to be taken out.
const FLAG_LOCKED: i64 = 128;
/// How many steps of replies to replies are followed when an annotation is taken out.
const MAX_REPLY_DEPTH: usize = 8;

/// What a change is worked out with: the document, the numbers taken for new objects and the objects to write.
struct Ctx<'a> {
    doc: &'a Document,
    next: u32,
    objects: Vec<(u32, Object)>,
}

impl Ctx<'_> {
    fn alloc(&mut self) -> Result<ObjRef> {
        if self.next > MAX_OBJECT_NUMBER {
            return Err(Error::Limit("the file has too many objects".to_string()));
        }
        let r = ObjRef::new(self.next, 0);
        self.next += 1;
        Ok(r)
    }

    fn form(&mut self, a: &Appearance) -> Result<ObjRef> {
        let r = self.alloc()?;
        let mut dict = Dict::new();
        dict.set("Type", Object::from("XObject"));
        dict.set("Subtype", Object::from("Form"));
        dict.set("BBox", reals(&a.bbox));
        dict.set("Matrix", reals(&[1.0, 0.0, 0.0, 1.0, 0.0, 0.0]));
        let mut resources = Dict::new();
        if a.multiply {
            let mut state = Dict::new();
            state.set("Type", Object::from("ExtGState"));
            state.set("BM", Object::from("Multiply"));
            let mut states = Dict::new();
            states.set("GS0", Object::Dict(state));
            resources.set("ExtGState", Object::Dict(states));
        }
        dict.set("Resources", Object::Dict(resources));
        dict.set("Filter", Object::from("FlateDecode"));
        let data = compress_to_vec_zlib(a.content.as_bytes(), 6);
        self.objects.push((r.num, Object::Stream(Stream { dict, data })));
        Ok(r)
    }
}

/// The dictionary every annotation of ours starts from (Table 164): type, rectangle, colour, flags (printable), author,
/// modification date and creation date.
fn annotation(subtype: &str, rect: [f64; 4], rgb: [u8; 3], author: &str, now: &str) -> Dict {
    let mut d = Dict::new();
    d.set("Type", Object::from("Annot"));
    d.set("Subtype", Object::from(subtype));
    d.set("Rect", reals(&rect));
    d.set("C", reals(&unit(rgb)));
    d.set("F", Object::Integer(4));
    let author: String = author.chars().take(MAX_AUTHOR_CHARS).collect();
    if !author.is_empty() {
        d.set("T", text_string(&author));
    }
    d.set("M", Object::String(PdfString::literal(now.as_bytes().to_vec())));
    d.set("CreationDate", Object::String(PdfString::literal(now.as_bytes().to_vec())));
    d
}

fn with_appearance(d: &mut Dict, form: ObjRef) {
    let mut ap = Dict::new();
    ap.set("N", Object::Ref(form));
    d.set("AP", Object::Dict(ap));
}

/// Work out `edit` against `doc` (the document as it stands now, `pages` its pages): the objects it makes or changes, by number.
/// `now` is the date the new annotations get ([`pdf_date_now`]). Nothing is changed; [`commit`] puts the objects in the document.
pub fn apply(doc: &Document, pages: &[Page], edit: &Edit, now: &str) -> Result<Vec<(u32, Object)>> {
    let page_index = usize::try_from(edit.page()).unwrap_or(usize::MAX);
    let page = pages.get(page_index).ok_or_else(|| Error::Invalid("there is no such page".to_string()))?;
    let shown = Shown::of(page);
    let mut ctx = Ctx { doc, next: doc.next_free_number(), objects: Vec::new() };
    let change = match edit {
        Edit::Markup { kind, rects, color, author, .. } => AnnotsChange::Add(markup(&mut ctx, &shown, *kind, rects, *color, author, now)?),
        Edit::Ink { strokes, color, width, author, .. } => AnnotsChange::Add(ink(&mut ctx, &shown, strokes, *color, *width, author, now)?),
        Edit::Note { at, text, color, author, .. } => AnnotsChange::Add(note(&mut ctx, &shown, *at, text, *color, author, now)?),
        Edit::Delete { index, num, .. } => AnnotsChange::Remove { index: *index as usize, num: *num },
    };
    change_annots(&mut ctx, pages, page, change)?;
    Ok(ctx.objects)
}

fn markup(ctx: &mut Ctx<'_>, shown: &Shown, kind: MarkupKind, rects: &[[f32; 4]], color: [u8; 3], author: &str, now: &str) -> Result<Vec<ObjRef>> {
    if rects.is_empty() {
        return Err(Error::Invalid("nothing is selected".to_string()));
    }
    if rects.len() > MAX_QUADS {
        return Err(Error::Limit(format!("a markup covers at most {MAX_QUADS} lines")));
    }
    let mut quads: Vec<Quad> = Vec::with_capacity(rects.len());
    for r in rects {
        let (l, t, rt, b) = (finite_place(r[0])?, finite_place(r[1])?, finite_place(r[2])?, finite_place(r[3])?);
        let (l, rt) = (l.min(rt), l.max(rt));
        let (t, b) = (t.min(b), t.max(b));
        if rt <= l || b <= t {
            continue;
        }
        quads.push([shown.unmap(l, t), shown.unmap(rt, t), shown.unmap(l, b), shown.unmap(rt, b)]);
    }
    if quads.is_empty() {
        return Err(Error::Invalid("nothing is selected".to_string()));
    }
    let ap = appearance::markup(kind, &quads, unit(color));
    let form = ctx.form(&ap)?;
    let annot_ref = ctx.alloc()?;
    let mut d = annotation(kind.subtype(), ap.bbox, color, author, now);
    let points: Vec<f64> = quads.iter().flat_map(|q| q.iter().flat_map(|&(x, y)| [x, y])).collect();
    d.set("QuadPoints", reals(&points));
    d.set("Border", Object::Array(vec![Object::Integer(0), Object::Integer(0), Object::Integer(0)]));
    with_appearance(&mut d, form);
    ctx.objects.push((annot_ref.num, Object::Dict(d)));
    Ok(vec![annot_ref])
}

fn ink(ctx: &mut Ctx<'_>, shown: &Shown, strokes: &[Vec<[f32; 2]>], color: [u8; 3], width: f32, author: &str, now: &str) -> Result<Vec<ObjRef>> {
    if strokes.len() > MAX_INK_STROKES {
        return Err(Error::Limit(format!("a drawing has at most {MAX_INK_STROKES} strokes")));
    }
    let width = f64::from(width);
    if !width.is_finite() {
        return Err(Error::Invalid("the line width is not a number".to_string()));
    }
    let width = width.clamp(MIN_INK_WIDTH, MAX_INK_WIDTH);
    // On the page as shown: every point is checked, the ones that add nothing are left out, and then what is still over the
    // limit is thinned out evenly (the ends of every stroke stay).
    let mut shown_strokes: Vec<Vec<(f64, f64)>> = Vec::new();
    for stroke in strokes {
        let mut points: Vec<(f64, f64)> = Vec::with_capacity(stroke.len());
        for p in stroke {
            points.push((finite_place(p[0])?, finite_place(p[1])?));
        }
        let points = smooth(points);
        if !points.is_empty() {
            shown_strokes.push(points);
        }
    }
    if shown_strokes.is_empty() {
        return Err(Error::Invalid("the drawing is empty".to_string()));
    }
    thin(&mut shown_strokes, MAX_INK_POINTS);
    let out: Vec<Vec<(f64, f64)>> = shown_strokes.iter().map(|s| s.iter().map(|&(x, y)| shown.unmap(x, y)).collect()).collect();
    let ap = appearance::ink(&out, unit(color), width);
    let form = ctx.form(&ap)?;
    let annot_ref = ctx.alloc()?;
    let mut d = annotation("Ink", ap.bbox, color, author, now);
    let list: Vec<Object> = out.iter().map(|s| reals(&s.iter().flat_map(|&(x, y)| [x, y]).collect::<Vec<f64>>())).collect();
    d.set("InkList", Object::Array(list));
    let mut border = Dict::new();
    border.set("W", Object::Real(width));
    border.set("S", Object::from("S"));
    d.set("BS", Object::Dict(border));
    with_appearance(&mut d, form);
    ctx.objects.push((annot_ref.num, Object::Dict(d)));
    Ok(vec![annot_ref])
}

fn distance_squared(a: (f64, f64), b: (f64, f64)) -> f64 {
    (a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)
}

/// Is `b` within [`FLAT`] of the straight line from `a` to `c`, between them?
fn on_the_line(a: (f64, f64), b: (f64, f64), c: (f64, f64)) -> bool {
    let (ac, ab) = ((c.0 - a.0, c.1 - a.1), (b.0 - a.0, b.1 - a.1));
    let length_squared = ac.0 * ac.0 + ac.1 * ac.1;
    if length_squared <= 0.0 {
        return false;
    }
    let along = (ab.0 * ac.0 + ab.1 * ac.1) / length_squared;
    let cross = ab.0 * ac.1 - ab.1 * ac.0;
    (0.0..=1.0).contains(&along) && cross * cross <= FLAT * FLAT * length_squared
}

/// A stroke without the points that add nothing to it: those closer than [`MIN_INK_STEP`] to the one before (the last point of
/// the stroke is kept, in place of the one before it), and those on the straight line between their neighbours.
fn smooth(points: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    let n = points.len();
    let mut kept: Vec<(f64, f64)> = Vec::with_capacity(n);
    for (i, p) in points.into_iter().enumerate() {
        if let Some(&q) = kept.last()
            && distance_squared(p, q) < MIN_INK_STEP * MIN_INK_STEP
        {
            if i + 1 == n && kept.len() >= 2 {
                kept.pop();
            } else {
                continue;
            }
        }
        while let [.., a, b] = kept[..] {
            if on_the_line(a, b, p) {
                kept.pop();
            } else {
                break;
            }
        }
        kept.push(p);
    }
    kept
}

/// Thin the strokes out evenly to `max` points in all, exactly: the first and last point of each stroke stay (a stroke of one
/// point is one), and the rest of the room is shared out by length.
fn thin(strokes: &mut [Vec<(f64, f64)>], max: usize) {
    let total: usize = strokes.iter().map(Vec::len).sum();
    if total <= max {
        return;
    }
    let fixed: usize = strokes.iter().map(|s| s.len().min(2)).sum();
    let spare = max.saturating_sub(fixed);
    let movable = (total - fixed).max(1);
    for stroke in strokes.iter_mut() {
        let len = stroke.len();
        let own = len.min(2);
        let keep = own + (len - own) * spare / movable;
        if keep >= len || keep < 2 {
            continue;
        }
        let picked: Vec<(f64, f64)> = (0..keep).filter_map(|i| stroke.get(i * (len - 1) / (keep - 1)).copied()).collect();
        *stroke = picked;
    }
}

fn note(ctx: &mut Ctx<'_>, shown: &Shown, at: [f32; 2], text: &str, color: [u8; 3], author: &str, now: &str) -> Result<Vec<ObjRef>> {
    let text: String = text.chars().take(MAX_NOTE_CHARS).collect();
    if text.trim().is_empty() {
        return Err(Error::Invalid("the note is empty".to_string()));
    }
    let (page_w, page_h) = shown.size();
    let size = appearance::NOTE_SIZE;
    let x = finite_place(at[0])?.clamp(0.0, (page_w - size).max(0.0));
    let y = finite_place(at[1])?.clamp(0.0, (page_h - size).max(0.0));
    let user_rect = |l: f64, t: f64, r: f64, b: f64| {
        let (a, c) = (shown.unmap(l, t), shown.unmap(r, b));
        [a.0.min(c.0), a.1.min(c.1), a.0.max(c.0), a.1.max(c.1)]
    };
    // The icon is not turned with the page (NoRotate, below): such an annotation hangs from the upper left corner of its rectangle
    // in user space, so that corner is where the icon's own upper left corner is on the page as shown, and the rectangle goes
    // down and to the right of it in user space.
    let (ux, uy) = shown.unmap(x, y);
    let icon = [ux, uy - size, ux + size, uy];
    // The box the text opens in sits to the right of the icon, or to its left when the page ends there.
    let px = if x + size + 4.0 + POPUP_WIDTH <= page_w { x + size + 4.0 } else { (x - 4.0 - POPUP_WIDTH).max(0.0) };
    let py = y.min((page_h - POPUP_HEIGHT).max(0.0));
    let popup_rect = user_rect(px, py, px + POPUP_WIDTH, py + POPUP_HEIGHT);

    let ap = appearance::note(unit(color));
    let form = ctx.form(&ap)?;
    let note_ref = ctx.alloc()?;
    let popup_ref = ctx.alloc()?;
    let mut d = annotation("Text", icon, color, author, now);
    // Printable, and neither scaled nor turned with the page (Table 165, bits 4 and 5): the icon stays upright and its size.
    d.set("F", Object::Integer(28));
    d.set("Contents", text_string(&text));
    d.set("Name", Object::from("Comment"));
    d.set("Open", Object::Bool(false));
    d.set("Popup", Object::Ref(popup_ref));
    with_appearance(&mut d, form);
    ctx.objects.push((note_ref.num, Object::Dict(d)));

    let mut p = Dict::new();
    p.set("Type", Object::from("Annot"));
    p.set("Subtype", Object::from("Popup"));
    p.set("Rect", reals(&popup_rect));
    p.set("Parent", Object::Ref(note_ref));
    p.set("Open", Object::Bool(false));
    p.set("F", Object::Integer(28));
    ctx.objects.push((popup_ref.num, Object::Dict(p)));
    Ok(vec![note_ref, popup_ref])
}

enum AnnotsChange {
    Add(Vec<ObjRef>),
    Remove { index: usize, num: u32 },
}

/// Where the page's list of annotations is written: in the page's own dictionary, or in the array object it points to.
enum Holder {
    Page,
    Array(u32),
}

/// Does another page point to the array object `array`, or might it (a chain of references too long to follow)?
fn shared_with_other_page(doc: &Document, pages: &[Page], this: &Page, array: u32) -> bool {
    let reaches = |first: ObjRef| {
        let mut at = first;
        for _ in 0..=8 {
            if at.num == array {
                return true;
            }
            match doc.get(at) {
                Ok(Object::Ref(next)) => at = next,
                _ => return false,
            }
        }
        true
    };
    pages.iter().any(|p| p.obj_ref != this.obj_ref && matches!(p.dict.get("Annots"), Some(Object::Ref(r)) if reaches(*r)))
}

/// The page's annotation list and where it is to be written back (12.5.2). An array that is an object of its own is written
/// where it is, unless another page points to it too (or one might): a change to the list of one page must not be a change to
/// the list of another, so the page gets a copy of its own, in its own dictionary.
fn locate(doc: &Document, pages: &[Page], page: &Page, page_dict: &Dict) -> Result<(Vec<Object>, Holder)> {
    match page_dict.get("Annots").cloned() {
        Some(Object::Array(a)) => Ok((a, Holder::Page)),
        Some(Object::Ref(first)) => {
            // Follow references to the object that holds the array.
            let mut at = first;
            let mut hops = 0;
            loop {
                match doc.get(at)? {
                    Object::Array(a) => {
                        let own = hops == 0 && !shared_with_other_page(doc, pages, page, at.num);
                        return Ok((a, if own { Holder::Array(at.num) } else { Holder::Page }));
                    }
                    Object::Ref(next) if hops < 8 => {
                        at = next;
                        hops += 1;
                    }
                    // Not an array (null, a dictionary, damage): the page gets an array of its own.
                    _ => return Ok((Vec::new(), Holder::Page)),
                }
            }
        }
        _ => Ok((Vec::new(), Holder::Page)),
    }
}

fn is_locked(doc: &Document, d: &Dict) -> bool {
    d.get("F").and_then(|f| doc.resolve(f).ok()).and_then(|f| f.as_int()).is_some_and(|f| f & FLAG_LOCKED != 0)
}

/// If the popup of annotation `d` (number `owner`) says it belongs to it, put the popup's number in `gone`. A popup that names
/// another annotation as its parent is not this one's to take out (12.5.6.14).
fn add_own_popup(doc: &Document, d: &Dict, owner: u32, gone: &mut HashSet<u32>) {
    let Some(Object::Ref(popup)) = d.get("Popup") else { return };
    let belongs = match doc.get(*popup) {
        Ok(Object::Dict(p)) => p.get("Parent").and_then(Object::as_obj_ref).is_some_and(|parent| parent.num == owner),
        _ => false,
    };
    if belongs {
        gone.insert(popup.num);
    }
}

/// Write the page's annotation list again with `change` made: the page object when the list is in it (or there is none, or
/// the array it points to is shared), the array object when that is the page's own (12.5.2; the page itself is then left alone).
///
/// Taking an annotation out takes with it its popup (if the popup names it as its parent) and the replies to it (annotations
/// whose `/IRT` points to it, and replies to those, 12.5.6.2): a reply left without what it answers would be a note about
/// nothing. A locked annotation (flag bit 8) or a locked reply is not taken out.
fn change_annots(ctx: &mut Ctx<'_>, pages: &[Page], page: &Page, change: AnnotsChange) -> Result<()> {
    let page_dict = match ctx.doc.get(page.obj_ref)? {
        Object::Dict(d) => d,
        _ => return Err(Error::Invalid("the page is not a dictionary".to_string())),
    };
    let (mut items, holder) = locate(ctx.doc, pages, page, &page_dict)?;
    match change {
        AnnotsChange::Add(new) => {
            if items.len().saturating_add(new.len()) > MAX_ANNOTS_ON_PAGE {
                return Err(Error::Limit(format!("a page may have {MAX_ANNOTS_ON_PAGE} annotations at most")));
            }
            items.extend(new.into_iter().map(Object::Ref));
        }
        AnnotsChange::Remove { index, num } => {
            let gone_error = || Error::Invalid("the annotation is not there any more".to_string());
            let item = items.get(index).cloned().ok_or_else(gone_error)?;
            let same = match &item {
                Object::Ref(r) => r.num == num,
                Object::Dict(_) => num == 0,
                _ => false,
            };
            if !same {
                return Err(gone_error());
            }
            let resolved = ctx.doc.resolve(&item)?;
            let Some(d) = resolved.as_dict() else {
                return Err(Error::Invalid("the annotation is not a dictionary".to_string()));
            };
            if d.get_name("Subtype").is_some_and(|n| n == "Link" || n == "Widget" || n == "Popup") {
                return Err(Error::Invalid("this kind of annotation is part of the page and is not taken out here".to_string()));
            }
            if is_locked(ctx.doc, d) {
                return Err(Error::Invalid("the annotation is locked: its author does not allow it to be taken out".to_string()));
            }
            let mut gone: HashSet<u32> = HashSet::new();
            if num != 0 {
                gone.insert(num);
                add_own_popup(ctx.doc, d, num, &mut gone);
                for _ in 0..MAX_REPLY_DEPTH {
                    let mut grew = false;
                    for other in &items {
                        let Some(r) = other.as_obj_ref().filter(|r| !gone.contains(&r.num)) else { continue };
                        let Ok(Object::Dict(reply)) = ctx.doc.get(r) else { continue };
                        if !reply.get("IRT").and_then(Object::as_obj_ref).is_some_and(|to| gone.contains(&to.num)) {
                            continue;
                        }
                        if is_locked(ctx.doc, &reply) {
                            return Err(Error::Invalid("a reply to the annotation is locked: it is not taken out".to_string()));
                        }
                        gone.insert(r.num);
                        add_own_popup(ctx.doc, &reply, r.num, &mut gone);
                        grew = true;
                    }
                    if !grew {
                        break;
                    }
                }
            }
            items = items
                .into_iter()
                .enumerate()
                .filter(|(i, o)| *i != index && !o.as_obj_ref().is_some_and(|r| gone.contains(&r.num)))
                .map(|(_, o)| o)
                .collect();
        }
    }
    match holder {
        Holder::Page => {
            let mut dict = page_dict;
            dict.set(Name::from("Annots"), Object::Array(items));
            ctx.objects.push((page.obj_ref.num, Object::Dict(dict)));
        }
        Holder::Array(num) => ctx.objects.push((num, Object::Array(items))),
    }
    Ok(())
}

/// What putting an edit's objects in front of the document's own replaced: for each object, what was there before (`None`: the
/// file's own object) and what is there now.
#[derive(Clone)]
pub(crate) struct Change {
    pub num: u32,
    pub before: Option<Rc<Object>>,
    pub after: Option<Rc<Object>>,
}

/// Put `objects` (from [`apply`]) in front of the document's own, in memory. Returns what they replaced, to undo it.
pub(crate) fn commit(doc: &Document, objects: Vec<(u32, Object)>) -> Vec<Change> {
    objects
        .into_iter()
        .map(|(num, object)| {
            let after = Rc::new(object);
            let before = doc.overlay_set(num, Some(Rc::clone(&after)));
            Change { num, before, after: Some(after) }
        })
        .collect()
}

/// Do a change again (`forward`) or take it back, in reverse order of the changes.
pub(crate) fn replay(doc: &Document, changes: &[Change], forward: bool) {
    let mut each = |c: &Change| {
        doc.overlay_set(c.num, if forward { c.after.clone() } else { c.before.clone() });
    };
    if forward {
        changes.iter().for_each(&mut each);
    } else {
        changes.iter().rev().for_each(&mut each);
    }
}

/// The incremental update (7.5.6) that writes everything put in front of the document's file, as one section.
pub(crate) fn save_section(doc: &Document) -> Result<Vec<u8>> {
    incremental::section(doc, &doc.overlay_entries())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::PdfBuilder;

    const NOW: &str = "D:20260101000000Z";

    #[test]
    fn dates_are_utc_and_right_across_leap_years() {
        assert_eq!(pdf_date(0), "D:19700101000000Z");
        assert_eq!(pdf_date(951_782_400), "D:20000229000000Z");
        assert_eq!(pdf_date(1_760_000_000), "D:20251009085320Z");
        assert_eq!(pdf_date(4_102_444_799), "D:20991231235959Z");
    }

    #[test]
    fn text_is_ascii_or_utf16() {
        assert_eq!(text_string("abc"), Object::String(PdfString::literal(b"abc".to_vec())));
        assert_eq!(text_string("a\u{7}b"), Object::String(PdfString::literal(b"a b".to_vec())));
        let Object::String(s) = text_string("\u{4E2D}a") else { panic!("not a string") };
        assert_eq!(s.bytes, vec![0xFE, 0xFF, 0x4E, 0x2D, 0x00, 0x61]);
    }

    /// `base` opened, `edits` made in memory one after the other, and the one update that writes them appended and read again.
    fn changed_all(base: &[u8], edits: &[Edit]) -> (Vec<u8>, Document) {
        let doc = Document::from_bytes(base.to_vec()).unwrap();
        for edit in edits {
            let pages = doc.pages().unwrap();
            commit(&doc, apply(&doc, &pages, edit, NOW).unwrap());
        }
        let section = save_section(&doc).unwrap();
        let mut bytes = base.to_vec();
        bytes.extend_from_slice(&section);
        let again = Document::from_bytes(bytes).unwrap();
        assert!(!again.was_repaired(), "{}", String::from_utf8_lossy(&section));
        (section, again)
    }

    fn changed(base: &[u8], edit: &Edit) -> (Vec<u8>, Document) {
        changed_all(base, std::slice::from_ref(edit))
    }

    fn annots_of(doc: &Document, page: usize) -> Vec<Dict> {
        let pages = doc.pages().unwrap();
        let d = doc.get(pages[page].obj_ref).unwrap();
        let list = doc.resolve(d.as_dict().unwrap().get("Annots").unwrap_or(&Object::Null)).unwrap();
        list.as_array().unwrap_or(&[]).iter().map(|a| doc.resolve(a).unwrap().as_dict().unwrap().clone()).collect()
    }

    fn subtypes(doc: &Document, page: usize) -> Vec<String> {
        annots_of(doc, page).iter().map(|d| String::from_utf8_lossy(d.get_name("Subtype").unwrap().as_bytes()).into_owned()).collect()
    }

    fn rect_of(d: &Dict) -> Vec<f64> {
        d.get("Rect").and_then(Object::as_array).unwrap().iter().map(|o| o.as_f64().unwrap()).collect()
    }

    fn page_pdf(page_entries: &str, trailer: &str, extra: impl FnOnce(&mut PdfBuilder)) -> Vec<u8> {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, &format!("<< /Type /Page /Parent 2 0 R /Resources << >> {page_entries} >>"));
        extra(&mut b);
        b.finish_classic(20, trailer)
    }

    fn two_pages(annots: &str, extra: impl FnOnce(&mut PdfBuilder)) -> Vec<u8> {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>");
        b.obj(3, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> /Annots {annots} >>"));
        b.obj(4, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> /Annots {annots} >>"));
        extra(&mut b);
        b.finish_classic(20, "/Root 1 0 R")
    }

    fn highlight(page: u32) -> Edit {
        Edit::Markup { page, kind: MarkupKind::Highlight, rects: vec![[10.0, 10.0, 80.0, 24.0]], color: [255, 255, 0], author: "me".to_string() }
    }

    #[test]
    fn places_on_a_turned_page_with_a_crop_box_land_where_the_page_shows_them() {
        // Media box 300 x 300, crop box [50 50 250 250] (shown 200 x 200), turned 90 degrees.
        let base = page_pdf("/MediaBox [0 0 300 300] /CropBox [50 50 250 250] /Rotate 90", "/Root 1 0 R", |_| {});
        let edit = Edit::Markup { page: 0, kind: MarkupKind::Highlight, rects: vec![[10.0, 20.0, 60.0, 40.0]], color: [255, 255, 0], author: String::new() };
        let (_, doc) = changed(&base, &edit);
        let a = &annots_of(&doc, 0)[0];
        // Display (x, y) is user (y + 50, x + 50) for a page turned 90 degrees.
        assert_eq!(rect_of(a), vec![70.0, 60.0, 90.0, 110.0]);
        let Some(Object::Array(q)) = a.get("QuadPoints") else { panic!("no quad points") };
        let q: Vec<f64> = q.iter().map(|o| o.as_f64().unwrap()).collect();
        // Upper left, upper right, lower left, lower right of what the reader sees.
        assert_eq!(q, vec![70.0, 60.0, 70.0, 110.0, 90.0, 60.0, 90.0, 110.0]);
        // The same place on a page that is not turned, and on pages turned 180 and 270 degrees.
        for (rotate, want) in [(0, [60.0, 210.0, 110.0, 230.0]), (180, [190.0, 70.0, 240.0, 90.0]), (270, [210.0, 190.0, 230.0, 240.0])] {
            let base = page_pdf(&format!("/MediaBox [0 0 300 300] /CropBox [50 50 250 250] /Rotate {rotate}"), "/Root 1 0 R", |_| {});
            let (_, doc) = changed(&base, &edit);
            assert_eq!(rect_of(&annots_of(&doc, 0)[0]), want.to_vec(), "rotate {rotate}");
        }
    }

    #[test]
    fn an_array_of_its_own_is_written_again_and_the_page_is_left_alone() {
        let base = page_pdf("/MediaBox [0 0 200 100] /Annots 7 0 R", "/Root 1 0 R", |b| {
            b.obj(7, "[ ]");
        });
        let edit = Edit::Ink { page: 0, strokes: vec![vec![[10.0, 10.0], [50.0, 50.0]]], color: [255, 0, 0], width: 2.0, author: "A".to_string() };
        let (section, doc) = changed(&base, &edit);
        let text = String::from_utf8_lossy(&section);
        assert!(text.contains("\n7 0 obj\n[") && !text.contains("/Type /Page"), "{text}");
        assert_eq!(annots_of(&doc, 0).len(), 1);
        // The file says /Size 20: the new objects start at 20.
        assert!(text.contains("\n20 0 obj"), "{text}");
    }

    #[test]
    fn an_array_that_two_pages_share_is_copied_for_the_page_that_changes() {
        let base = two_pages("5 0 R", |b| {
            b.obj(5, "[ 6 0 R ]");
            b.obj(6, "<< /Type /Annot /Subtype /Square /Rect [10 10 40 40] >>");
        });
        // Adding to page 0 leaves page 1's list alone, and the shared array is not written again.
        let (section, doc) = changed(&base, &highlight(0));
        assert_eq!((annots_of(&doc, 0).len(), annots_of(&doc, 1).len()), (2, 1));
        assert!(!String::from_utf8_lossy(&section).contains("\n5 0 obj"), "the shared array was rewritten");
        // Taking the square out of page 1 leaves it on page 0, and the other way round.
        let (_, doc) = changed(&base, &Edit::Delete { page: 1, index: 0, num: 6 });
        assert_eq!((annots_of(&doc, 0).len(), annots_of(&doc, 1).len()), (1, 0));
        let (_, doc) = changed(&base, &Edit::Delete { page: 0, index: 0, num: 6 });
        assert_eq!((annots_of(&doc, 0).len(), annots_of(&doc, 1).len()), (0, 1));
        // A chain of references to the array cannot be told to be one page's own: the page gets a copy.
        let chain = two_pages("8 0 R", |b| {
            b.obj(8, "5 0 R");
            b.obj(5, "[ ]");
        });
        let (section, doc) = changed(&chain, &highlight(1));
        assert_eq!((annots_of(&doc, 0).len(), annots_of(&doc, 1).len()), (0, 1));
        assert!(!String::from_utf8_lossy(&section).contains("\n5 0 obj"));
        // A page that is the only one to point to its array keeps writing it where it is.
        let own = page_pdf("/MediaBox [0 0 200 100] /Annots 7 0 R", "/Root 1 0 R", |b| {
            b.obj(7, "[ ]");
        });
        let (section, _) = changed(&own, &highlight(0));
        assert!(String::from_utf8_lossy(&section).contains("\n7 0 obj"));
    }

    #[test]
    fn a_size_that_is_too_small_does_not_make_new_objects_collide() {
        // The table has entries up to object 15, but the trailer says /Size 4 (the later /Size wins).
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << >> >>");
        b.obj(15, "<< /Marker true >>");
        let base = b.finish_classic(16, "/Root 1 0 R /Size 4");
        let edit = Edit::Note { page: 0, at: [10.0, 10.0], text: "hi".to_string(), color: [255, 215, 0], author: String::new() };
        let doc = Document::from_bytes(base).unwrap();
        let pages = doc.pages().unwrap();
        commit(&doc, apply(&doc, &pages, &edit, NOW).unwrap());
        let section = String::from_utf8_lossy(&save_section(&doc).unwrap()).into_owned();
        assert!(section.contains("\n16 0 obj"), "{section}");
        assert!(!section.contains("\n4 0 obj") && !section.contains("\n15 0 obj"), "{section}");
    }

    #[test]
    fn an_object_keeps_its_generation_and_the_id_keeps_its_first_string() {
        // Object 3 (the page) is generation 2.
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 2 R] /Count 1 >>");
        let at = b.raw_obj(3, b"3 2 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << >> >>\nendobj\n");
        let x = b.len();
        let mut table = format!("xref\n0 4\n0000000000 65535 f \n{:010} 00000 n \n{:010} 00000 n \n{at:010} 00002 n \n", b.offset_of(1), b.offset_of(2));
        table.push_str("trailer\n<< /Size 4 /Root 1 0 R /ID [<00112233445566778899AABBCCDDEEFF> <00112233445566778899AABBCCDDEEFF>] >>\n");
        b.raw(table.as_bytes());
        b.startxref(x);
        let base = b.finish();
        let edit = Edit::Markup { page: 0, kind: MarkupKind::Underline, rects: vec![[10.0, 10.0, 80.0, 24.0]], color: [0, 0, 0], author: String::new() };
        let (section, doc) = changed(&base, &edit);
        let text = String::from_utf8_lossy(&section).into_owned();
        assert!(text.contains("\n3 2 obj\n") && text.contains("00002 n"), "{text}");
        assert_eq!(annots_of(&doc, 0).len(), 1);
        let Some(Object::Array(id)) = doc.trailer().get("ID") else { panic!("no ID") };
        let (Object::String(first), Object::String(second)) = (&id[0], &id[1]) else { panic!("ID is not strings") };
        assert_eq!(first.bytes, vec![0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
        assert_ne!(first.bytes, second.bytes, "the second string was not changed");
    }

    #[test]
    fn an_encrypted_file_without_an_id_is_not_given_one() {
        let base = crate::testutil::encrypted_rc4_without_id();
        let plain = Document::from_bytes(base.clone()).unwrap();
        assert!(plain.is_encrypted() && !plain.is_locked() && plain.trailer().get("ID").is_none());
        let edit = Edit::Note { page: 0, at: [10.0, 10.0], text: "secret".to_string(), color: [255, 215, 0], author: "me".to_string() };
        let (section, doc) = changed(&base, &edit);
        assert!(doc.is_encrypted() && !doc.is_locked(), "the edited file does not open");
        assert!(doc.trailer().get("ID").is_none(), "an ID was added: {}", String::from_utf8_lossy(&section));
        let note = annots_of(&doc, 0).into_iter().find(|d| d.get_name("Subtype").is_some_and(|n| n == "Text")).unwrap();
        let Some(Object::String(contents)) = note.get("Contents") else { panic!("no text") };
        assert_eq!(contents.bytes, b"secret");
        assert!(!String::from_utf8_lossy(&section).contains("secret"), "the text was written in the clear");
    }

    #[test]
    fn a_direct_annotation_and_a_note_with_its_popup_can_be_taken_out() {
        let base = page_pdf(
            "/MediaBox [0 0 200 100] /Annots [ << /Type /Annot /Subtype /Square /Rect [10 10 40 40] >> 8 0 R 9 0 R << /Type /Annot /Subtype /Link /Rect [0 0 5 5] >> ]",
            "/Root 1 0 R",
            |b| {
                b.obj(8, "<< /Type /Annot /Subtype /Text /Rect [50 50 70 70] /Contents (x) /Popup 9 0 R >>");
                b.obj(9, "<< /Type /Annot /Subtype /Popup /Rect [80 50 180 90] /Parent 8 0 R >>");
            },
        );
        let (_, doc) = changed(&base, &Edit::Delete { page: 0, index: 0, num: 0 });
        assert_eq!(annots_of(&doc, 0).len(), 3);
        // The note takes its popup with it.
        let (_, doc) = changed(&base, &Edit::Delete { page: 0, index: 1, num: 8 });
        assert_eq!(subtypes(&doc, 0), ["Square", "Link"]);
        // A popup, a link, a number that is not the one in the list, a place that is not in the list, a page that is not there.
        let d = Document::from_bytes(base).unwrap();
        let pages = d.pages().unwrap();
        for bad in [
            Edit::Delete { page: 0, index: 2, num: 9 },
            Edit::Delete { page: 0, index: 3, num: 0 },
            Edit::Delete { page: 0, index: 9, num: 0 },
            Edit::Delete { page: 5, index: 0, num: 0 },
            Edit::Delete { page: 0, index: 0, num: 7 },
        ] {
            assert!(apply(&d, &pages, &bad, NOW).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_popup_that_belongs_to_another_annotation_stays() {
        // Notes A (6) and B (7) both name popup 8, whose parent is B: taking A out must leave B its popup.
        let base = page_pdf("/MediaBox [0 0 200 200] /Annots [6 0 R 7 0 R 8 0 R]", "/Root 1 0 R", |b| {
            b.obj(6, "<< /Type /Annot /Subtype /Text /Rect [10 10 30 30] /Contents (A) /Popup 8 0 R >>");
            b.obj(7, "<< /Type /Annot /Subtype /Text /Rect [50 50 70 70] /Contents (B) /Popup 8 0 R >>");
            b.obj(8, "<< /Type /Annot /Subtype /Popup /Rect [80 50 180 90] /Parent 7 0 R >>");
        });
        let (_, doc) = changed(&base, &Edit::Delete { page: 0, index: 0, num: 6 });
        assert_eq!(subtypes(&doc, 0), ["Text", "Popup"]);
        let (_, doc) = changed(&base, &Edit::Delete { page: 0, index: 1, num: 7 });
        assert_eq!(subtypes(&doc, 0), ["Text"]);
    }

    #[test]
    fn a_locked_annotation_stays_and_the_replies_to_one_that_goes_go_with_it() {
        let base = page_pdf("/MediaBox [0 0 200 200] /Annots [6 0 R 7 0 R 8 0 R 9 0 R 10 0 R]", "/Root 1 0 R", |b| {
            b.obj(6, "<< /Type /Annot /Subtype /Text /Rect [10 10 30 30] /Contents (root) /F 128 >>");
            b.obj(7, "<< /Type /Annot /Subtype /Text /Rect [50 50 70 70] /Contents (a note) /Popup 10 0 R >>");
            b.obj(8, "<< /Type /Annot /Subtype /Text /Rect [50 50 70 70] /Contents (reply) /IRT 7 0 R /RT /R >>");
            b.obj(9, "<< /Type /Annot /Subtype /Text /Rect [50 50 70 70] /Contents (reply to the reply) /IRT 8 0 R >>");
            b.obj(10, "<< /Type /Annot /Subtype /Popup /Rect [80 50 180 90] /Parent 7 0 R >>");
        });
        let d = Document::from_bytes(base.clone()).unwrap();
        let pages = d.pages().unwrap();
        let locked = apply(&d, &pages, &Edit::Delete { page: 0, index: 0, num: 6 }, NOW);
        assert!(locked.is_err_and(|e| e.to_string().contains("locked")));
        // The note, its popup and both replies go.
        let (_, doc) = changed(&base, &Edit::Delete { page: 0, index: 1, num: 7 });
        assert_eq!(subtypes(&doc, 0), ["Text"]);
        // A reply alone goes with the replies to it only.
        let (_, doc) = changed(&base, &Edit::Delete { page: 0, index: 2, num: 8 });
        assert_eq!(subtypes(&doc, 0), ["Text", "Text", "Popup"]);
        // A locked reply keeps the whole thing from going.
        let locked_reply = page_pdf("/MediaBox [0 0 200 200] /Annots [7 0 R 8 0 R]", "/Root 1 0 R", |b| {
            b.obj(7, "<< /Type /Annot /Subtype /Text /Rect [50 50 70 70] /Contents (a note) >>");
            b.obj(8, "<< /Type /Annot /Subtype /Text /Rect [50 50 70 70] /IRT 7 0 R /F 132 >>");
        });
        let d = Document::from_bytes(locked_reply).unwrap();
        let pages = d.pages().unwrap();
        assert!(apply(&d, &pages, &Edit::Delete { page: 0, index: 0, num: 7 }, NOW).is_err());
    }

    #[test]
    fn edits_are_made_in_memory_one_after_the_other_and_written_as_one_update() {
        let base = page_pdf("/MediaBox [0 0 200 100]", "/Root 1 0 R", |_| {});
        let edits: Vec<Edit> = (0..50).map(|i| Edit::Note { page: 0, at: [i as f32, 10.0], text: format!("note {i}"), color: [255, 215, 0], author: String::new() }).collect();
        let (section, doc) = changed_all(&base, &edits);
        assert_eq!(annots_of(&doc, 0).len(), 100, "50 notes and their popups");
        // The page is written once, not once for each edit, and there is a single cross-reference section.
        let text = String::from_utf8_lossy(&section);
        assert_eq!(text.matches("/Type /Page ").count(), 1);
        assert_eq!(text.matches("\nstartxref\n").count(), 1);
    }

    #[test]
    fn taking_a_change_back_and_doing_it_again_restores_what_was_there() {
        let base = page_pdf("/MediaBox [0 0 200 100]", "/Root 1 0 R", |_| {});
        let doc = Document::from_bytes(base).unwrap();
        let pages = doc.pages().unwrap();
        let first = commit(&doc, apply(&doc, &pages, &highlight(0), NOW).unwrap());
        let second = commit(&doc, apply(&doc, &pages, &highlight(0), NOW).unwrap());
        assert_eq!(annots_of(&doc, 0).len(), 2);
        replay(&doc, &second, false);
        assert_eq!(annots_of(&doc, 0).len(), 1);
        replay(&doc, &first, false);
        assert_eq!(annots_of(&doc, 0).len(), 0);
        assert!(!doc.has_overlay(), "what was new is gone with the change");
        replay(&doc, &first, true);
        replay(&doc, &second, true);
        assert_eq!(annots_of(&doc, 0).len(), 2);
        // A number given out once is not given out again after an undo.
        let top = doc.next_free_number();
        replay(&doc, &second, false);
        replay(&doc, &first, false);
        assert!(doc.next_free_number() >= top);
    }

    #[test]
    fn a_stroke_is_smoothed_and_the_points_of_a_drawing_are_held_to_the_limit_exactly() {
        // Points closer than half a point, and points on a straight line, go; the ends stay.
        let line: Vec<(f64, f64)> = (0..=100).map(|i| (f64::from(i) * 0.1, 0.0)).chain((1..=10).map(|i| (10.0 + f64::from(i), 0.0))).collect();
        assert_eq!(smooth(line), vec![(0.0, 0.0), (20.0, 0.0)]);
        let corner = smooth(vec![(0.0, 0.0), (5.0, 0.0), (10.0, 0.0), (10.0, 5.0), (10.0, 10.0)]);
        assert_eq!(corner, vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)]);
        assert_eq!(smooth(vec![(3.0, 3.0), (3.1, 3.0), (3.2, 3.1)]), vec![(3.0, 3.0)], "a dot stays a dot");
        // A wave is kept; then it is thinned to exactly the limit, from several strokes of different length.
        let wave = |n: usize, shift: f64| -> Vec<(f64, f64)> { (0..n).map(|i| (i as f64, shift + 3.0 * (i as f64 * 0.9).sin())).collect() };
        let mut strokes = vec![wave(20_000, 0.0), wave(5_000, 50.0), vec![(1.0, 1.0)], wave(2, 80.0)];
        let ends: Vec<_> = strokes.iter().map(|s| (s[0], *s.last().unwrap())).collect();
        thin(&mut strokes, MAX_INK_POINTS);
        let total: usize = strokes.iter().map(Vec::len).sum();
        assert!(total <= MAX_INK_POINTS && total > MAX_INK_POINTS - 8, "{total} points");
        for (s, (first, last)) in strokes.iter().zip(ends) {
            assert_eq!((s[0], *s.last().unwrap()), (first, last));
        }
        // Through the whole edit, a drawing of many wiggles ends with no more than the limit.
        let base = page_pdf("/MediaBox [0 0 2000 1000]", "/Root 1 0 R", |_| {});
        let strokes: Vec<Vec<[f32; 2]>> = (0..64).map(|k| (0..500).map(|i| [i as f32 * 3.0, 10.0 + k as f32 * 10.0 + 4.0 * (i as f32).sin()]).collect()).collect();
        let (_, doc) = changed(&base, &Edit::Ink { page: 0, strokes, color: [0; 3], width: 1.0, author: String::new() });
        let ink = &annots_of(&doc, 0)[0];
        let Some(Object::Array(list)) = ink.get("InkList") else { panic!("no ink list") };
        let points: usize = list.iter().map(|s| s.as_array().unwrap().len() / 2).sum();
        assert!(points <= MAX_INK_POINTS && points > MAX_INK_POINTS - 70, "{points} points");
    }

    #[test]
    fn places_that_are_not_numbers_and_too_many_pieces_are_refused() {
        let base = page_pdf("/MediaBox [0 0 200 100]", "/Root 1 0 R", |_| {});
        let d = Document::from_bytes(base).unwrap();
        let pages = d.pages().unwrap();
        let now = NOW;
        let mark = |rects: Vec<[f32; 4]>| Edit::Markup { page: 0, kind: MarkupKind::Highlight, rects, color: [0; 3], author: String::new() };
        assert!(apply(&d, &pages, &mark(vec![[f32::NAN, 0.0, 10.0, 10.0]]), now).is_err());
        assert!(apply(&d, &pages, &mark(vec![[0.0, 0.0, f32::INFINITY, 10.0]]), now).is_err());
        assert!(apply(&d, &pages, &mark(vec![[0.0, 0.0, 1.0e9, 10.0]]), now).is_err());
        assert!(apply(&d, &pages, &mark(vec![]), now).is_err());
        assert!(apply(&d, &pages, &mark(vec![[5.0, 5.0, 5.0, 9.0]]), now).is_err(), "a rectangle with no area marks nothing");
        assert!(apply(&d, &pages, &mark(vec![[0.0, 0.0, 10.0, 10.0]; MAX_QUADS + 1]), now).is_err());
        let ink = |strokes: Vec<Vec<[f32; 2]>>, width: f32| Edit::Ink { page: 0, strokes, color: [0; 3], width, author: String::new() };
        assert!(apply(&d, &pages, &ink(vec![], 1.0), now).is_err());
        assert!(apply(&d, &pages, &ink(vec![vec![[1.0, 1.0]]], f32::NAN), now).is_err());
        assert!(apply(&d, &pages, &ink(vec![vec![[1.0, 1.0]]; MAX_INK_STROKES + 1], 1.0), now).is_err());
        assert!(apply(&d, &pages, &ink(vec![vec![[1.0, 1.0]]], 1.0e9), now).is_ok(), "a huge width is held to the widest");
        assert!(apply(&d, &pages, &ink(vec![vec![[1.0, 1.0], [f32::NAN, 1.0], [5.0, 5.0]]], 1.0), now).is_err(), "every point is looked at");
        let note = |text: &str| Edit::Note { page: 0, at: [5.0, 5.0], text: text.to_string(), color: [0; 3], author: String::new() };
        assert!(apply(&d, &pages, &note("   "), now).is_err());
        assert!(apply(&d, &pages, &note("ok"), now).is_ok());
    }
}
