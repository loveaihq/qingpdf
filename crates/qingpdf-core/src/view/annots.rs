//! The annotations of a page that a person can pick, take out and change (4a, 2026-10-17): everything but links, form fields and popups,
//! which are part of the page's behaviour. Each has the rectangle it covers on the page as it is shown, for the window to hit-test and to
//! outline; the list of the whole document (`list`) is the same read for every page, with the text cut short.

use crate::document::{Document, Page};
use crate::edit::modify::{contents_of, shown_rect};
use crate::info::decode_text_string;
use crate::object::{Dict, Object, PdfString};
use crate::render::work::{Work, cost};
use crate::text::Shown;

/// Most annotations listed for a page; the rest are left out.
pub const MAX_ANNOT_INFOS: usize = 2_000;
/// Most entries of a page's `/Annots` looked at.
const MAX_LOOKED_AT: usize = 20_000;
/// Most characters of the text of an annotation that are kept.
pub const MAX_CONTENTS_CHARS: usize = 200;
/// Most pieces (lines of a text markup) kept for one annotation.
pub const MAX_PARTS: usize = 64;
/// Most annotations the list of the whole document holds, and the most characters of the first line of an annotation's text in it.
pub const MAX_LISTED_ANNOTS: usize = 20_000;
/// Most entries of `/Annots` the list of the whole document looks at, over all its pages (pages may share one array of thousands).
pub const MAX_LIST_VISITS: usize = 1_500_000;
pub const MAX_LISTED_CHARS: usize = 120;
/// Most characters of the full text of one annotation that [`text`] gives (a note's, a text box's).
pub const MAX_FULL_TEXT_CHARS: usize = 4_000;

/// Bits of the flags of an annotation (Table 165): ReadOnly (bit 7), Locked (bit 8), LockedContents (bit 10).
const FLAG_READ_ONLY: u32 = 64;
const FLAG_LOCKED: u32 = 128;
const FLAG_LOCKED_CONTENTS: u32 = 512;

/// One annotation of a page.
#[derive(Clone, Debug, PartialEq)]
pub struct AnnotInfo {
    /// Its place in the page's `/Annots` (from 0), and its object number (0 when it is written in the array itself): what
    /// [`crate::view::Engine::delete_annotation`] asks for.
    pub index: u32,
    pub num: u32,
    /// The name of its type (`Highlight`, `Ink`, `Text`, ...).
    pub subtype: String,
    /// Where it is: `[left, top, right, bottom]` in points on the page as it is shown.
    pub rect: [f32; 4],
    /// The pieces it is made of when it is more than its rectangle: a line of text for each quadrilateral of a text markup.
    pub parts: Vec<[f32; 4]>,
    /// The two ends of a `Line`, in the points of `rect` (for picking it by the line and not by its box).
    pub line: Option<[f32; 4]>,
    /// Its text (`/Contents`), cut to [`MAX_CONTENTS_CHARS`], and who wrote it (`/T`).
    pub contents: String,
    pub author: String,
    /// The day it was last changed (`/M`), as `2026-10-17`; what the file says if that is not a date, empty if it says nothing.
    pub modified: String,
    /// Its flags (`/F`).
    pub flags: u32,
}

impl AnnotInfo {
    /// Locked (or read-only): it is not moved, resized or recoloured, and not taken out.
    pub fn locked(&self) -> bool {
        self.flags & (FLAG_LOCKED | FLAG_READ_ONLY) != 0
    }

    /// Its text is not changed (Locked does not stop that: Table 165; read-only and LockedContents do).
    pub fn text_locked(&self) -> bool {
        self.flags & (FLAG_READ_ONLY | FLAG_LOCKED_CONTENTS) != 0
    }

    /// The reader can draw it again in another colour: text markup, drawings, shapes, notes and text boxes.
    pub fn restylable(&self) -> bool {
        matches!(self.subtype.as_str(), "Highlight" | "Underline" | "StrikeOut" | "Squiggly" | "Ink" | "Square" | "Circle" | "Line" | "Text" | "FreeText")
    }

    /// It has a line whose width can change.
    pub fn has_width(&self) -> bool {
        matches!(self.subtype.as_str(), "Ink" | "Square" | "Circle" | "Line")
    }

    /// It can be filled.
    pub fn fillable(&self) -> bool {
        matches!(self.subtype.as_str(), "Square" | "Circle")
    }

    /// Its size can change (a note's icon cannot).
    pub fn resizable(&self) -> bool {
        matches!(self.subtype.as_str(), "Square" | "Circle" | "Line" | "Ink" | "FreeText" | "Stamp" | "Polygon" | "PolyLine") && self.flags & 16 == 0
    }

    /// It has a text a person can type (a note, a text box).
    pub fn typed(&self) -> bool {
        matches!(self.subtype.as_str(), "Text" | "FreeText")
    }
}

/// A date of the file, `D:20261017143000Z`, as `2026-10-17` (the day only: the file's time is in UTC or in a zone the file names, and the list is
/// not the place to turn it into the person's own); anything else as it is, cut short.
pub fn show_date(s: &str) -> String {
    let d = s.strip_prefix("D:").unwrap_or("");
    let n = d.bytes().take_while(u8::is_ascii_digit).count();
    let at = |a: usize, b: usize| d.get(a..b).unwrap_or("");
    if n >= 8 { format!("{}-{}-{}", at(0, 4), at(4, 6), at(6, 8)) } else { s.chars().take(24).collect() }
}

/// The flags of an annotation (`/F`), read as the renderer reads them: they may be an indirect object or a real.
pub(super) fn flags_in(doc: &Document, d: &Dict, work: &Work) -> i64 {
    d.get("F").and_then(|f| work.read(doc, f)).map_or(0, |f| f.as_flags())
}

fn text_of(o: Option<&Object>, work: &Work, doc: &Document) -> String {
    match o.and_then(|o| work.read(doc, o)).as_deref() {
        Some(Object::String(PdfString { bytes, .. })) => decode_text_string(bytes).chars().take(MAX_CONTENTS_CHARS).collect(),
        _ => String::new(),
    }
}

/// The annotations of `page`. `truncated`: there were more than the reader lists, or the work ran out.
pub(super) fn read(doc: &Document, page: &Page, work: &Work) -> (Vec<AnnotInfo>, bool) {
    let mut out = Vec::new();
    let Some(held) = page.dict.get("Annots").and_then(|a| work.read(doc, a)) else { return (out, false) };
    let Some(annots) = held.as_array() else { return (out, false) };
    let shown = Shown::of(page);
    let mut truncated = false;
    for (index, item) in annots.iter().enumerate() {
        if index >= MAX_LOOKED_AT || out.len() >= MAX_ANNOT_INFOS || !work.charge(cost::ANNOT_VISIT) {
            truncated = true;
            break;
        }
        let Ok(Some(held)) = work.resolve(doc, item) else { continue };
        let Some(d) = held.as_dict() else { continue };
        let Some(Object::Name(subtype)) = d.get("Subtype").and_then(|s| work.read(doc, s)).as_deref().cloned() else { continue };
        if matches!(subtype.as_bytes(), b"Link" | b"Widget" | b"Popup") {
            continue;
        }
        // Hidden (bit 2) and not-to-be-viewed (bit 6) annotations are not there for the reader.
        let flags = flags_in(doc, d, work);
        if flags & (2 | 32) != 0 {
            continue;
        }
        let Some(user) = work.rectangle(doc, d.get("Rect")) else { continue };
        let bounds = |points: &[(f64, f64)]| {
            let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
            for &(x, y) in points {
                let (px, py) = shown.map(x, y);
                b = [b[0].min(px), b[1].min(py), b[2].max(px), b[3].max(py)];
            }
            b
        };
        // (NoRotate, bit 5: drawn upright, from where the upper left corner of the rectangle of user space is on the page as shown.)
        let rect = shown_rect(&shown, flags, user);
        if !rect.iter().all(|v| v.is_finite()) || rect[2] <= rect[0] || rect[3] <= rect[1] {
            continue;
        }
        let to32 = |b: [f64; 4]| [b[0] as f32, b[1] as f32, b[2] as f32, b[3] as f32];
        let mut parts: Vec<[f32; 4]> = Vec::new();
        if matches!(subtype.as_bytes(), b"Highlight" | b"Underline" | b"StrikeOut" | b"Squiggly")
            && let Some(q) = d.get("QuadPoints").and_then(|q| work.read(doc, q))
            && let Some(values) = q.as_array()
        {
            let numbers: Vec<f64> = values.iter().filter_map(Object::as_f64).collect();
            for quad in numbers.as_chunks::<8>().0.iter().take(MAX_PARTS) {
                let points: Vec<(f64, f64)> = quad.as_chunks::<2>().0.iter().map(|p| (p[0], p[1])).collect();
                let b = bounds(&points);
                if b.iter().all(|v| v.is_finite()) && b[2] > b[0] && b[3] > b[1] {
                    parts.push(to32(b));
                }
            }
        }
        let line = (subtype.as_bytes() == b"Line")
            .then(|| d.get("L").and_then(|l| work.read(doc, l)))
            .flatten()
            .and_then(|l| {
                let v: Vec<f64> = l.as_array()?.iter().filter_map(|n| work.read(doc, n).and_then(|n| n.as_f64())).collect();
                let [x0, y0, x1, y1] = v.as_slice() else { return None };
                let (a, b) = (shown.map(*x0, *y0), shown.map(*x1, *y1));
                Some([a.0 as f32, a.1 as f32, b.0 as f32, b.1 as f32])
            });
        let num = item.as_obj_ref().map_or(0, |r| r.num);
        out.push(AnnotInfo {
            index: u32::try_from(index).unwrap_or(u32::MAX),
            num,
            subtype: String::from_utf8_lossy(subtype.as_bytes()).into_owned(),
            rect: to32(rect),
            parts,
            line,
            contents: text_of(d.get("Contents"), work, doc),
            author: text_of(d.get("T"), work, doc),
            modified: show_date(&text_of(d.get("M"), work, doc)),
            flags: u32::try_from(flags).unwrap_or(0),
        });
    }
    (out, truncated)
}

/// The full text of the annotation at place `index` of the page's list, which must be object `num`: `None` if it is not there (any more), or if it has
/// more than [`MAX_FULL_TEXT_CHARS`] characters (the box would show the first of them, and saving it would lose the rest).
pub(super) fn text(doc: &Document, page: &Page, index: u32, num: u32, work: &Work) -> Option<String> {
    let held = page.dict.get("Annots").and_then(|a| work.read(doc, a))?;
    let item = held.as_array()?.get(index as usize)?;
    if item.as_obj_ref().map_or(0, |r| r.num) != num || num == 0 {
        return None;
    }
    let d: Dict = doc.resolve(item).ok()?.as_dict()?.clone();
    let text = contents_of(doc, &d, MAX_FULL_TEXT_CHARS + 1);
    (work.charge(cost::ANNOT_VISIT) && text.chars().count() <= MAX_FULL_TEXT_CHARS).then_some(text)
}

/// How many entries of the page's `/Annots` [`read`] looks at (what the list of the document counts against its visits).
pub(super) fn looked_at(doc: &Document, page: &Page, work: &Work) -> usize {
    page.dict.get("Annots").and_then(|a| work.read(doc, a)).and_then(|a| a.as_array().map(|a| a.len().min(MAX_LOOKED_AT))).unwrap_or(0)
}

/// An annotation of the list of the whole document: where it is and what [`AnnotInfo`] says of it (without the pieces, with the text cut to its first line).
#[derive(Clone, Debug, PartialEq)]
pub struct ListedAnnot {
    pub page: u32,
    pub info: AnnotInfo,
}

/// Cut the text of an annotation for the list: its first line, at most [`MAX_LISTED_CHARS`] characters.
pub(super) fn listed(page: u32, mut info: AnnotInfo) -> ListedAnnot {
    info.parts = Vec::new();
    info.contents = info.contents.split(['\r', '\n']).find(|l| !l.trim().is_empty()).unwrap_or("").trim().chars().take(MAX_LISTED_CHARS).collect();
    ListedAnnot { page, info }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_shown_as_a_day_and_anything_else_as_it_is() {
        assert_eq!(show_date("D:20261017143005Z"), "2026-10-17");
        assert_eq!(show_date("D:2026101714+08'00'"), "2026-10-17");
        assert_eq!(show_date("D:20261017"), "2026-10-17");
        assert_eq!(show_date(""), "");
        assert_eq!(show_date("last Tuesday"), "last Tuesday");
        assert_eq!(show_date("D:2026"), "D:2026");
    }

    #[test]
    fn flags_are_read_as_the_renderer_reads_them() {
        let mut b = crate::testutil::PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] /Annots [10 0 R 11 0 R 12 0 R 13 0 R] >>");
        // Hidden as a real; hidden by an indirect number; locked as a real; plain.
        b.obj(10, "<< /Type /Annot /Subtype /Square /Rect [10 10 60 60] /F 130.0 >>");
        b.obj(11, "<< /Type /Annot /Subtype /Square /Rect [10 70 60 120] /F 20 0 R >>");
        b.obj(12, "<< /Type /Annot /Subtype /Square /Rect [10 130 60 180] /F 128.0 >>");
        b.obj(13, "<< /Type /Annot /Subtype /Square /Rect [10 190 60 240] >>");
        b.obj(20, "2");
        let doc = Document::from_bytes(b.finish_classic(30, "/Root 1 0 R")).unwrap();
        let pages = doc.pages().unwrap();
        let (list, _) = read(&doc, &pages[0], &Work::new());
        assert_eq!(list.iter().map(|a| a.num).collect::<Vec<_>>(), [12, 13]);
        assert!(list[0].locked() && !list[1].locked());
    }

    #[test]
    fn a_listed_text_is_its_first_line_cut_short() {
        let info = AnnotInfo {
            index: 0,
            num: 7,
            subtype: "Text".to_string(),
            rect: [0.0; 4],
            parts: vec![[0.0; 4]],
            line: None,
            contents: format!("\r\n  first line {}\nsecond", "x".repeat(300)),
            author: String::new(),
            modified: String::new(),
            flags: 0,
        };
        let l = listed(3, info);
        assert!(l.info.parts.is_empty() && l.page == 3);
        assert!(l.info.contents.starts_with("first line x") && l.info.contents.chars().count() == MAX_LISTED_CHARS && !l.info.contents.contains("second"));
    }
}
