//! The form fields of a page that a person can fill in (4b): every widget annotation with the field it belongs to, its place on
//! the page as it is shown and its value. Signature fields and fields of no known kind are listed too (so that the reader can say
//! they are not filled in here); hidden widgets are not. The options of a combo box or list are not in the list: they are asked
//! for, for the one field being chosen from ([`options`]).

use crate::document::{Document, Page};
use crate::edit::{FieldInfo, forms};
use crate::object::Object;
use crate::render::work::{Work, cost};
use crate::text::Shown;

/// Most fields listed for a page; the rest are left out.
pub const MAX_FIELD_INFOS: usize = 3_000;
/// Most bytes one list of fields comes to (the texts of their names and values, and what a [`FieldInfo`] takes for each field); the
/// rest of the fields are left out and the list says so. The reader keeps the lists of the pages round about, so this is counted in
/// the memory plan ([`super::MemoryPlan::form_bytes`]).
pub const MAX_FIELDS_BYTES: usize = 256 * 1024;
/// What a field is counted as besides its texts.
const FIELD_BYTES: usize = std::mem::size_of::<FieldInfo>();
/// Most entries of a page's `/Annots` looked at.
const MAX_LOOKED_AT: usize = 20_000;

/// The fields of `page`, in the order of its `/Annots` (the order Tab goes in). `truncated`: there were more than the reader lists
/// ([`MAX_FIELD_INFOS`], [`MAX_FIELDS_BYTES`]), or the work ran out.
pub(super) fn read(doc: &Document, page: &Page, work: &Work) -> (Vec<FieldInfo>, bool) {
    let mut out = Vec::new();
    let Some(form) = forms::acroform(doc, work) else { return (out, false) };
    let env = forms::Env { doc, form: &form, work };
    let Some(held) = page.dict.get("Annots").and_then(|a| work.read(doc, a)) else { return (out, false) };
    let Some(annots) = held.as_array() else { return (out, false) };
    let shown = Shown::of(page);
    let mut truncated = false;
    let mut bytes = 0usize;
    for (index, item) in annots.iter().enumerate() {
        if index >= MAX_LOOKED_AT || out.len() >= MAX_FIELD_INFOS || bytes > MAX_FIELDS_BYTES || !work.charge(cost::ANNOT_VISIT) {
            truncated = true;
            break;
        }
        let Ok(Some(held)) = work.resolve(doc, item) else { continue };
        let Some(d) = held.as_dict() else { continue };
        if !d.get("Subtype").and_then(|s| work.read(doc, s)).is_some_and(|s| matches!(&*s, Object::Name(n) if n == "Widget")) {
            continue;
        }
        if d.get_int("F").is_some_and(|f| f & (2 | 32) != 0) {
            continue;
        }
        let Some([x0, y0, x1, y1]) = work.rectangle(doc, d.get("Rect")) else { continue };
        let corners = [shown.map(x0, y0), shown.map(x1, y0), shown.map(x0, y1), shown.map(x1, y1)];
        let rect = corners.iter().fold([f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY], |b, &(x, y)| [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)]);
        if !rect.iter().all(|v| v.is_finite()) || rect[2] <= rect[0] || rect[3] <= rect[1] {
            continue;
        }
        if !work.charge(cost::ANNOT_DRAW) {
            truncated = true;
            break;
        }
        let num = item.as_obj_ref().map_or(0, |r| r.num);
        let index = u32::try_from(index).unwrap_or(u32::MAX);
        if let Some(info) = forms::describe(env, num, d, [rect[0] as f32, rect[1] as f32, rect[2] as f32, rect[3] as f32], index) {
            bytes += FIELD_BYTES + info.value.len() + info.name.len();
            // (The one that goes over is the last: the list is cut after it.)
            out.push(info);
        }
    }
    (out, truncated)
}

/// The options of the choice field whose widget is the `index`-th entry of `page`'s `/Annots`, object `num`: (export value, shown
/// text) for as many as fit in [`forms::MAX_OPTION_BYTES`] of text, and whether some were left out. Nothing for a widget that is
/// not there any more or is no choice.
pub(super) fn options(doc: &Document, page: &Page, work: &Work, index: u32, num: u32) -> (Vec<(String, String)>, bool) {
    let Some(form) = forms::acroform(doc, work) else { return (Vec::new(), false) };
    let Some(held) = page.dict.get("Annots").and_then(|a| work.read(doc, a)) else { return (Vec::new(), false) };
    let Some(item) = held.as_array().and_then(|a| a.get(index as usize)) else { return (Vec::new(), false) };
    if item.as_obj_ref().is_none_or(|r| r.num != num) {
        return (Vec::new(), false);
    }
    let Ok(Some(widget)) = work.resolve(doc, item) else { return (Vec::new(), false) };
    let Some(d) = widget.as_dict() else { return (Vec::new(), false) };
    forms::options_of(forms::Env { doc, form: &form, work }, num, d)
}
