//! Annotation appearances (ISO 32000-1 12.5). After the page's content, each annotation's normal appearance stream
//! (`/AP /N`; when that is a dictionary of states, the one `/AS` names) is drawn as a form XObject, put on the
//! annotation's `/Rect` by Algorithm 8.1 (12.5.5): the form's `/BBox` is taken through its `/Matrix`, and the box
//! this makes is scaled and moved onto `/Rect`. Annotations that are `Hidden` or `NoView` (12.5.3), `Popup`s, and those
//! whose optional content is off are not drawn.
//!
//! Everything is metered, in proportion to the real cost, whether or not the annotation shows: at most
//! [`MAX_ANNOTS`] entries of `/Annots` are looked at; each costs [`cost::ANNOT_VISIT`] and the bytes of the
//! dictionaries read for it; each one drawn costs [`cost::ANNOT_DRAW`] and then what its content costs. An appearance
//! stream that many annotations share is read once (the forms kept for the page) and drawn each time; one that
//! cannot be kept is charged for each read. Every object the file points to is read through [`Work::resolve`]: charged
//! by its size the first time and kept (failures too), so that `/Subtype 6 0 R` with 6 a huge array costs one parse for
//! the page, not one for each annotation. A page that has no work left stops drawing annotations and keeps what it
//! has. The `/Parent` chain followed for an inherited `/V` is at most [`MAX_PARENTS`] long and stops at a repeat.

use super::super::work::{Held, cost};
use super::*;
use crate::document::Page;

/// Most entries of `/Annots` looked at; the rest are not drawn.
pub(crate) const MAX_ANNOTS: usize = 20_000;
/// Most levels of `/Parent` followed to find an inherited `/V` (12.7.3.1).
const MAX_PARENTS: usize = 32;

/// Annotation flags (12.5.3, Table 165).
const FLAG_INVISIBLE: i64 = 1;
const FLAG_HIDDEN: i64 = 1 << 1;
const FLAG_NO_VIEW: i64 = 1 << 5;

/// The standard annotation types (12.5.6, Table 169): the `Invisible` flag is about the others.
const STANDARD: [&[u8]; 26] = [
    b"Text",
    b"Link",
    b"FreeText",
    b"Line",
    b"Square",
    b"Circle",
    b"Polygon",
    b"PolyLine",
    b"Highlight",
    b"Underline",
    b"Squiggly",
    b"StrikeOut",
    b"Stamp",
    b"Caret",
    b"Ink",
    b"Popup",
    b"FileAttachment",
    b"Sound",
    b"Movie",
    b"Widget",
    b"Screen",
    b"PrinterMark",
    b"TrapNet",
    b"Watermark",
    b"3D",
    b"Redact",
];

/// What looking for an annotation's appearance found.
enum Found {
    /// The form to draw, and the object it is.
    Form(ObjRef, Rc<Form>),
    /// There is no appearance stream to look at (no `/AP`, or no `/N` in it).
    Missing,
    /// There is one in principle, but none for now: no stream for the state in force, optional content that is off, a stream
    /// that cannot be read.
    Nothing,
}

impl Interp<'_> {
    /// Draw the normal appearances of the page's annotations on top of what is drawn. Nothing here fails the page: what
    /// cannot be drawn is skipped, and past a limit the rest of the annotations are left out, both with a warning.
    pub fn draw_annotations(&mut self, page: &Page, res: &Rc<Resources>) {
        let held: Held<'_>;
        let items: &[Object] = match page.dict.get("Annots") {
            Some(Object::Array(a)) => a,
            Some(o @ Object::Ref(_)) => match self.work.resolve(self.doc, o) {
                Ok(Some(h)) => {
                    held = h;
                    match held.as_array() {
                        Some(a) => a,
                        None => return,
                    }
                }
                Err(Error::Limit(m)) => {
                    self.warn(format!("{m}; the annotations are not drawn"));
                    return;
                }
                _ => return,
            },
            _ => return,
        };
        if items.is_empty() {
            return;
        }
        // The array is copied and walked whatever it holds: charged for all of it, drawn or not.
        if !self.work.charge(items.len() as f64 * cost::ANNOT_ENTRY) {
            return;
        }
        self.start_annotations();
        for (i, item) in items.iter().enumerate() {
            if i >= MAX_ANNOTS {
                self.warn(format!("the page has more than {MAX_ANNOTS} annotations; the rest are not drawn"));
                break;
            }
            if !self.work.charge(cost::ANNOT_VISIT) {
                break;
            }
            if let Err(e) = self.annotation(item, res) {
                self.warn(format!("{e}; the annotations from here on are not drawn"));
                break;
            }
        }
    }

    /// The page's content is over: whatever state it left (open marked content, a deep `q` stack, a clip) is not the
    /// annotations'. They start from the initial graphics state.
    fn start_annotations(&mut self) {
        self.gs = self.annotation_state();
        self.stack.clear();
        self.stack_floor = 0;
        self.path.clear();
        self.pending_clip = None;
        self.tm = IDENTITY;
        self.tlm = IDENTITY;
        self.text_clip = None;
        self.text_clip_segs = 0;
        self.hidden = 0;
        self.mc_stack.clear();
        self.mc_floor = 0;
        self.uncolored = false;
        self.in_progress.clear();
        self.pattern_base = self.base;
    }

    fn annotation_state(&self) -> GState {
        initial_gstate(self.pixmap.width() as i32, self.pixmap.height() as i32, &self.gray)
    }

    /// One entry of `/Annots`. `Err` only for a limit that ends the drawing of annotations.
    fn annotation(&mut self, item: &Object, res: &Rc<Resources>) -> Result<()> {
        let doc = self.doc;
        // (An annotation written in place was paid for with the array it is in.)
        let held = self.work.resolve(doc, item)?;
        let Some(Object::Dict(d)) = held.as_deref() else { return Ok(()) };
        let subtype = self.name_of(d.get("Subtype"));
        // A popup is the viewer's window for another annotation, not part of the page (12.5.6.14).
        if subtype.as_deref() == Some(b"Popup") {
            return Ok(());
        }
        let flags = d.get("F").and_then(|o| self.work.read(doc, o)).and_then(|o| o.as_f64()).map_or(0, |v| v as i64);
        if flags & (FLAG_HIDDEN | FLAG_NO_VIEW) != 0 {
            return Ok(());
        }
        // 12.5.3: `Invisible` hides the annotations that are not of a standard type.
        if flags & FLAG_INVISIBLE != 0 && !subtype.as_deref().is_some_and(|s| STANDARD.contains(&s)) {
            return Ok(());
        }
        let Some(rect) = self.work.rectangle(doc, d.get("Rect")) else { return Ok(()) };
        // Off the page, or too large to place: nothing of it can show.
        let (w, h) = (f64::from(self.pixmap.width()), f64::from(self.pixmap.height()));
        let corners = [
            apply(&self.base, rect[0], rect[1]),
            apply(&self.base, rect[2], rect[1]),
            apply(&self.base, rect[2], rect[3]),
            apply(&self.base, rect[0], rect[3]),
        ];
        if corners.iter().any(|(x, y)| !x.is_finite() || !y.is_finite()) {
            return Ok(());
        }
        let (min_x, max_x) = corners.iter().fold((f64::MAX, f64::MIN), |(lo, hi), (x, _)| (lo.min(*x), hi.max(*x)));
        let (min_y, max_y) = corners.iter().fold((f64::MAX, f64::MIN), |(lo, hi), (_, y)| (lo.min(*y), hi.max(*y)));
        if max_x < 0.0 || min_x > w || max_y < 0.0 || min_y > h {
            return Ok(());
        }
        // 8.11.3.3: the annotation's own optional content.
        if let Some(o) = d.get("OC")
            && let Some(oc) = self.shared.oc.clone()
            && oc.is_present()
            && !oc.visible(doc, o, None, &mut self.work)
        {
            return Ok(());
        }
        let (r, form) = match self.normal_appearance(d)? {
            Found::Form(r, form) => (r, form),
            Found::Missing => {
                if subtype.as_deref() == Some(b"Widget") {
                    self.warn("a form field has no appearance stream; it is not drawn (NeedAppearances is not handled)");
                }
                return Ok(());
            }
            Found::Nothing => return Ok(()),
        };
        // Algorithm 8.1 (12.5.5): the form's box through its matrix, then onto the annotation's rectangle.
        let Some(bbox) = form.bbox else { return Ok(()) };
        let t = [
            apply(&form.matrix, bbox[0], bbox[1]),
            apply(&form.matrix, bbox[2], bbox[1]),
            apply(&form.matrix, bbox[2], bbox[3]),
            apply(&form.matrix, bbox[0], bbox[3]),
        ];
        let (tx0, tx1) = t.iter().fold((f64::MAX, f64::MIN), |(lo, hi), (x, _)| (lo.min(*x), hi.max(*x)));
        let (ty0, ty1) = t.iter().fold((f64::MAX, f64::MIN), |(lo, hi), (_, y)| (lo.min(*y), hi.max(*y)));
        let (tw, th, rw, rh) = (tx1 - tx0, ty1 - ty0, rect[2] - rect[0], rect[3] - rect[1]);
        // A rectangle without area has nothing to show. A box that is flat in one direction (a horizontal line's) is not
        // scaled in that direction, as PDFium does (the scale is 1 for a side under 0.001).
        if !(rw > 0.0 && rh > 0.0 && tw.is_finite() && th.is_finite()) {
            return Ok(());
        }
        let (sx, sy) = (if tw < 1e-3 { 1.0 } else { rw / tw }, if th < 1e-3 { 1.0 } else { rh / th });
        let a = [sx, 0.0, 0.0, sy, rect[0] - tx0 * sx, rect[1] - ty0 * sy];
        if !finite(&a) || !finite(&mul(&a, &self.base)) {
            return Ok(());
        }
        if !self.work.charge(cost::ANNOT_DRAW) {
            return Ok(());
        }
        self.gs = self.annotation_state();
        self.gs.ctm = a;
        // (A stream that has no resources of its own is given the page's, as the other viewers do.)
        self.run_form(r, &form, res, 0)
    }

    /// The appearance stream an annotation shows (12.5.5): `/AP /N`, or, when that is a dictionary of states, the
    /// one `/AS` names (an annotation without `/AS` is in the state its `/V` or an ancestor field's `/V` names when
    /// the dictionary has it, else in `Off`, as the other viewers read it).
    fn normal_appearance(&mut self, d: &Dict) -> Result<Found> {
        let doc = self.doc;
        let hold_n: Held<'_>;
        let Some(ap_entry) = d.get("AP") else { return Ok(Found::Missing) };
        let ap_held = self.work.resolve(doc, ap_entry)?;
        let Some(Object::Dict(ap)) = ap_held.as_deref() else { return Ok(Found::Missing) };
        let Some(n) = ap.get("N") else { return Ok(Found::Missing) };
        let states: &Dict = match n {
            Object::Dict(s) => {
                // Looking a state up goes through the entries one by one (a dictionary read by reference is charged
                // for its entries when it is read).
                if !self.work.charge(s.len() as f64 * cost::ANNOT_ENTRY) {
                    return Ok(Found::Nothing);
                }
                s
            }
            Object::Ref(r) => {
                if !self.xobject_shown(*r) {
                    return Ok(Found::Nothing);
                }
                if let Some(hit) = self.kept_form(*r) {
                    return Ok(hit);
                }
                let Some(h) = self.work.resolve(doc, n)? else { return Ok(Found::Nothing) };
                hold_n = h;
                match &*hold_n {
                    Object::Stream(s) => return self.load_appearance(*r, s),
                    Object::Dict(s) => s,
                    _ => {
                        self.shared.forms.insert(*r, None);
                        return Ok(Found::Missing);
                    }
                }
            }
            _ => return Ok(Found::Missing),
        };
        let Some(state) = self.state_in_force(d, states)? else { return Ok(Found::Nothing) };
        let Some((_, value)) = states.iter().find(|(k, _)| k.as_bytes() == state.as_slice()) else { return Ok(Found::Nothing) };
        let Object::Ref(r) = value else { return Ok(Found::Nothing) };
        if !self.xobject_shown(*r) {
            return Ok(Found::Nothing);
        }
        if let Some(hit) = self.kept_form(*r) {
            return Ok(hit);
        }
        let Some(target) = self.work.resolve(doc, value)? else { return Ok(Found::Nothing) };
        match &*target {
            Object::Stream(s) => self.load_appearance(*r, s),
            _ => {
                self.shared.forms.insert(*r, None);
                Ok(Found::Nothing)
            }
        }
    }

    /// The name of the state the annotation is in: `/AS`, or, without one, the `/V` it or a field above it has (the
    /// `/Parent` chain is followed at most [`MAX_PARENTS`] levels and not past a repeat) when `states` has that state,
    /// else `Off`.
    fn state_in_force(&mut self, d: &Dict, states: &Dict) -> Result<Option<Vec<u8>>> {
        let doc = self.doc;
        if d.get("AS").is_some() {
            return Ok(self.name_of(d.get("AS")));
        }
        let mut value = self.name_of(d.get("V"));
        let mut next = d.get("Parent").and_then(Object::as_obj_ref);
        let mut seen: Vec<ObjRef> = Vec::new();
        while value.is_none()
            && let Some(r) = next
            && seen.len() < MAX_PARENTS
            && !seen.contains(&r)
        {
            seen.push(r);
            // (A field with many widgets under it is read once, then shared by them; one that is no dictionary is
            // remembered as well.)
            let parent_ref = Object::Ref(r);
            let Some(parent) = self.work.resolve(doc, &parent_ref)? else { break };
            let Object::Dict(p) = &*parent else { break };
            value = self.name_of(p.get("V"));
            next = p.get("Parent").and_then(Object::as_obj_ref);
        }
        let named = value.filter(|v| states.iter().any(|(k, _)| k.as_bytes() == v.as_slice()));
        Ok(Some(named.unwrap_or_else(|| b"Off".to_vec())))
    }

    /// An appearance stream already read for this page (or one that was found unreadable).
    fn kept_form(&self, r: ObjRef) -> Option<Found> {
        match self.shared.forms.get(&r) {
            Some(Some(form)) => Some(Found::Form(r, form.clone())),
            Some(None) => Some(Found::Nothing),
            None => None,
        }
    }

    /// Read an appearance stream as a form. The form is kept for the next annotation that shows it; when it cannot be kept (the
    /// forms kept have used up their room) it is read again each time, and each time is charged for. A stream that is
    /// no form, or cannot be read, is remembered as such.
    fn load_appearance(&mut self, r: ObjRef, s: &Stream) -> Result<Found> {
        if s.dict.get_name("Subtype").is_some_and(|t| t.as_bytes() != b"Form") {
            self.shared.forms.insert(r, None);
            return Ok(Found::Nothing);
        }
        if !self.work.charge(s.data.len() as f64 * cost::ANNOT_LOAD_BYTE) {
            return Ok(Found::Nothing);
        }
        match self.load_form(s, Some(r))? {
            Some(form) => {
                self.work.spend(form.content.len() as f64 * cost::DECODE_BYTE);
                Ok(Found::Form(r, form))
            }
            None => {
                self.shared.forms.insert(r, None);
                Ok(Found::Nothing)
            }
        }
    }

    /// The name `o` is or refers to.
    fn name_of(&self, o: Option<&Object>) -> Option<Vec<u8>> {
        match &*self.work.read(self.doc, o?)? {
            Object::Name(n) => Some(n.as_bytes().to_vec()),
            _ => None,
        }
    }
}
