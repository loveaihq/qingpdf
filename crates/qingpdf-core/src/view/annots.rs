//! The annotations of a page that a person can pick and take out (4a): everything but links, form fields and popups, which
//! are part of the page's behaviour. Each has the rectangle it covers on the page as it is shown, for the window to hit-test and to
//! outline.

use crate::document::{Document, Page};
use crate::info::decode_text_string;
use crate::object::{Object, PdfString};
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
    /// Its text (`/Contents`), cut to [`MAX_CONTENTS_CHARS`], and who wrote it (`/T`).
    pub contents: String,
    pub author: String,
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
        if d.get_int("F").is_some_and(|f| f & (2 | 32) != 0) {
            continue;
        }
        let Some([x0, y0, x1, y1]) = work.rectangle(doc, d.get("Rect")) else { continue };
        let bounds = |points: &[(f64, f64)]| {
            let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
            for &(x, y) in points {
                let (px, py) = shown.map(x, y);
                b = [b[0].min(px), b[1].min(py), b[2].max(px), b[3].max(py)];
            }
            b
        };
        let mut rect = bounds(&[(x0, y0), (x0, y1), (x1, y0), (x1, y1)]);
        // NoRotate (bit 5): drawn upright, from where the rectangle's upper left corner (of user space) is on the page as shown.
        if d.get_int("F").is_some_and(|f| f & 16 != 0) {
            let (px, py) = shown.map(x0, y1);
            rect = [px, py, px + (x1 - x0), py + (y1 - y0)];
        }
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
        let num = item.as_obj_ref().map_or(0, |r| r.num);
        out.push(AnnotInfo {
            index: u32::try_from(index).unwrap_or(u32::MAX),
            num,
            subtype: String::from_utf8_lossy(subtype.as_bytes()).into_owned(),
            rect: to32(rect),
            parts,
            contents: text_of(d.get("Contents"), work, doc),
            author: text_of(d.get("T"), work, doc),
        });
    }
    (out, truncated)
}
