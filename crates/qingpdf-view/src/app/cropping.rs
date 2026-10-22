//! Cropping pages (2026-10-21): Page > Crop pages... (a box with the four margins in millimetres, or "the white margins", and the pages it is for),
//! Page > Crop by dragging a box (a rectangle dragged on a page fills the margins in), Page > Uncrop pages.... The engine writes the crop boxes and
//! keeps the history (`docs/view-api.md`, "裁剪"); the window asks, and finds the white margins by asking the engine to draw the pages small.
//! Which pages a choice means (`scope_pages`) is plain and has tests.

use qingpdf_core::edit::{PT_PER_MM, is_nearly_whole, margins_rect};
use qingpdf_core::ops::parse_page_ranges;
use qingpdf_core::view::CropOp;

use super::editing::Asked;
use super::*;
use crate::i18n::fill;
use crate::ui::{FormAnswer, FormItem, FormKind::*, FormSpec};

/// The pages a box with the "apply to" choice is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Scope {
    This,
    Chosen,
    All,
    Range,
    Odd,
    Even,
}

/// The choices in the order of the list in the box.
pub(super) const SCOPES: [(Scope, Msg); 6] = [
    (Scope::This, Msg::ScopeThis),
    (Scope::Chosen, Msg::ScopeChosen),
    (Scope::All, Msg::ScopeAll),
    (Scope::Range, Msg::ScopeRange),
    (Scope::Odd, Msg::ScopeOdd),
    (Scope::Even, Msg::ScopeEven),
];

/// The pages (from 0) a choice names, for a document of `count` pages whose page `current` is in view and whose thumbnails `chosen` are chosen. `None`:
/// the range written is not a list of pages of the document, or there are none.
#[inline(never)]
pub(super) fn scope_pages(scope: Scope, current: usize, chosen: &[u32], count: usize, range: &str) -> Option<Vec<u32>> {
    let n = u32::try_from(count).ok()?;
    let here = u32::try_from(current).ok()?;
    let pages: Vec<u32> = match scope {
        Scope::This => vec![here],
        Scope::Chosen if chosen.is_empty() => vec![here],
        Scope::Chosen => chosen.to_vec(),
        Scope::All => (0..n).collect(),
        // (Page 1 is odd: the first of the pages, the third ...)
        Scope::Odd => (0..n).step_by(2).collect(),
        Scope::Even => (1..n).step_by(2).collect(),
        Scope::Range => parse_page_ranges(range, count).ok()?.into_iter().filter_map(|p| u32::try_from(p).ok()).collect(),
    };
    (!pages.is_empty() && pages.iter().all(|&p| p < n)).then_some(pages)
}

/// A number of millimetres as the box writes it: at most one decimal, no trailing zero.
#[inline(never)]
fn mm_text(points: f64) -> String {
    let s = format!("{:.1}", points / PT_PER_MM);
    s.strip_suffix(".0").map_or(s.clone(), str::to_string)
}

/// A number the person typed (a comma may be the decimal mark), not negative.
#[inline(never)]
fn parse_mm(text: &str) -> Option<f64> {
    text.trim().replace(',', ".").parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0)
}

/// What the cropping asks of the engine and has not been answered: the pages being looked at for their white margins (the request).
pub(super) struct Cropping {
    looking: Option<u64>,
}

impl Cropping {
    pub(super) fn new() -> Cropping {
        Cropping { looking: None }
    }

    pub(super) fn busy(&self) -> bool {
        self.looking.is_some()
    }

    /// The document is gone: the request to take back.
    pub(super) fn reset(&mut self) -> Option<u64> {
        self.looking.take()
    }
}

impl App {
    /// Cropping may be asked for: a file is open that allows changing its pages (or its content).
    pub(super) fn can_crop(&self) -> bool {
        self.doc.is_some() && (self.rights.assemble || self.rights.modify)
    }

    /// The actions that say why pages cannot be cropped now; `None` when they can.
    #[inline(never)]
    fn crop_not_now(&mut self) -> Option<Vec<Action>> {
        if self.doc.is_none() {
            return Some(Vec::new());
        }
        if !self.can_crop() {
            return Some(self.say(t(self.lang, Msg::NotePagesDenied).to_string()));
        }
        if self.ocr.is_some() || self.print.is_some() || self.cr.busy() || self.px.busy() {
            return Some(self.say(t(self.lang, Msg::NoteBusy).to_string()));
        }
        self.pages_busy().then(Vec::new)
    }

    /// The first `count` choices of pages, in words; and the one a box starts with: the chosen thumbnails when there are several, else the page in view.
    #[inline(never)]
    pub(super) fn scope_choices(&self, count: usize) -> (Vec<String>, usize) {
        (SCOPES.iter().take(count).map(|(_, m)| t(self.lang, *m).to_string()).collect(), usize::from(self.pg.chosen.len() > 1))
    }

    /// The pages the box's choice names, or a word to the person when the range is not right.
    #[inline(never)]
    pub(super) fn pages_of(&mut self, which: usize, range: &str) -> Result<Vec<u32>, Vec<Action>> {
        let scope = SCOPES.get(which).map_or(Scope::This, |s| s.0);
        let chosen = self.pg.chosen.pages();
        scope_pages(scope, self.thumb_current(), &chosen, self.page_count(), range).ok_or_else(|| self.say(t(self.lang, Msg::NoteCropBad).to_string()))
    }

    /// Page > Crop pages...
    #[inline(never)]
    pub(super) fn ask_crop(&mut self) -> Vec<Action> {
        if let Some(actions) = self.crop_not_now() {
            return actions;
        }
        let (_, scope) = self.scope_choices(0);
        vec![Action::AskForm(self.crop_form(["0", "0", "0", "0"].map(String::from), scope))]
    }

    /// The box, with these margins (millimetres as text: top, right, bottom, left) and this choice of pages.
    #[inline(never)]
    pub(super) fn crop_form(&self, margins: [String; 4], scope: usize) -> FormSpec {
        let lang = self.lang;
        let mut items = vec![FormItem::new(Label, t(lang, Msg::CropIntro), (12, 8, 456, 34))];
        let names = [Msg::CropTop, Msg::CropRight, Msg::CropBottom, Msg::CropLeft];
        for (i, (name, value)) in names.iter().zip(&margins).enumerate() {
            let (x, y) = (12 + (i as i32 % 2) * 232, 50 + (i as i32 / 2) * 28);
            items.push(FormItem::new(Label, t(lang, *name), (x, y + 3, 56, 18)));
            items.push(FormItem::new(Edit, value, (x + 58, y, 90, 22)));
        }
        items.push(FormItem::new(Check, t(lang, Msg::CropAuto), (12, 110, 456, 20)));
        items.push(FormItem::new(Label, t(lang, Msg::ApplyTo), (12, 140, 90, 18)));
        items.push(FormItem::pick(self.scope_choices(6).0, scope, (104, 136, 364, 200)));
        items.push(FormItem::new(Label, t(lang, Msg::ScopeRangeLabel), (12, 170, 130, 18)));
        items.push(FormItem::new(Edit, "", (144, 167, 324, 22)));
        FormSpec { tag: TAG_CROP, title: t(lang, Msg::CropTitle).to_string(), size: (480, 244), items, ok: t(lang, Msg::CropOk).to_string(), cancel: t(lang, Msg::Cancel).to_string(), preview: None }
    }

    /// The box was closed: with OK, the margins or the white margins are asked for.
    #[inline(never)]
    pub(super) fn crop_answered(&mut self, answer: Option<FormAnswer>) -> Vec<Action> {
        // (Four margins, the tick, the choice of pages and the pages written.)
        let Some(FormAnswer { values, .. }) = answer.filter(|a| a.values.len() == 7) else { return Vec::new() };
        if self.crop_not_now().is_some_and(|a| !a.is_empty()) || self.doc.is_none() {
            return Vec::new();
        }
        let which = values.get(5).and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
        let pages = match self.pages_of(which, values.get(6).map_or("", String::as_str)) {
            Ok(p) => p,
            Err(actions) => return actions,
        };
        if values.get(4).map(String::as_str) == Some("1") {
            // The white margins: the engine draws the pages small and looks; the crop is asked for when it has.
            let id = self.new_id();
            self.engine.auto_crop(self.doc_id, id, pages);
            self.cr.looking = Some(id);
            return self.say_job(sticky::Job::Crop, fill(t(self.lang, Msg::NoteCropLooking), &[("n", "0")]));
        }
        let mut cut = [0.0f64; 4];
        for (slot, v) in cut.iter_mut().zip(&values) {
            match parse_mm(v) {
                Some(mm) => *slot = mm * PT_PER_MM,
                None => return self.say(t(self.lang, Msg::NoteCropBad).to_string()),
            }
        }
        let mut set = Vec::with_capacity(pages.len());
        for &p in &pages {
            let size = self.doc.as_ref().and_then(|d| d.layout.page_size_pt(p as usize));
            match size.and_then(|s| margins_rect(s, cut).ok()) {
                Some(r) => set.push((p, r.map(|v| v as f32))),
                None => return self.say(t(self.lang, Msg::NoteCropBad).to_string()),
            }
        }
        self.send_crop(CropOp::Set(set))
    }

    fn send_crop(&mut self, op: CropOp) -> Vec<Action> {
        let id = self.new_id();
        let asked = if matches!(op, CropOp::Reset(_)) { Asked::Uncrop } else { Asked::Crop };
        self.engine.crop_pages(self.doc_id, id, op);
        self.edit.asked.insert(id, asked);
        Vec::new()
    }

    /// The pages were looked at: the rectangles of their content are the crop.
    #[inline(never)]
    pub(super) fn on_auto_crop(&mut self, id: u64, boxes: Vec<(u32, Option<[f32; 4]>)>, failed: &[u32]) -> Vec<Action> {
        if self.cr.looking != Some(id) {
            return Vec::new();
        }
        self.cr.looking = None;
        let lang = self.lang;
        let blank: Vec<String> = boxes.iter().filter(|b| b.1.is_none()).map(|b| b.0).chain(failed.iter().copied()).map(|p| (p + 1).to_string()).collect();
        // A page whose content fills it to the edges has no white margin to cut: it is left as it is, and said so.
        let (mut set, mut whole) = (Vec::new(), Vec::new());
        for (p, r) in boxes.into_iter().filter_map(|(p, r)| r.map(|r| (p, r))) {
            let size = self.doc.as_ref().and_then(|d| d.layout.page_size_pt(p as usize));
            if size.is_some_and(|s| is_nearly_whole(r.map(f64::from), s)) {
                whole.push((p + 1).to_string());
            } else {
                set.push((p, r));
            }
        }
        let mut notes = Vec::new();
        if !blank.is_empty() {
            notes.push(fill(t(lang, Msg::NoteCropBlank), &[("list", &blank.join(", "))]));
        }
        if !whole.is_empty() {
            notes.push(fill(t(lang, Msg::NoteCropWhole), &[("list", &whole.join(", "))]));
        }
        if set.is_empty() {
            notes.push(t(lang, Msg::NoteCropNothing).to_string());
            return vec![Action::Message { title: t(lang, Msg::CropTitle).to_string(), text: notes.join("\n") }];
        }
        let mut actions = Vec::new();
        if !notes.is_empty() {
            actions.push(Action::Message { title: t(lang, Msg::CropTitle).to_string(), text: notes.join("\n") });
        }
        if self.crop_not_now().is_none() {
            actions.extend(self.send_crop(CropOp::Set(set)));
        }
        actions
    }

    /// Esc while the pages are being looked at. `None`: they are not.
    pub(super) fn cancel_crop_look(&mut self) -> Option<Vec<Action>> {
        let id = self.cr.looking.take()?;
        self.engine.cancel(id);
        Some(self.say(t(self.lang, Msg::NoteStopped).to_string()))
    }

    /// How far the looking is.
    #[inline(never)]
    pub(super) fn on_crop_progress(&mut self, id: u64, percent: u32) -> Option<Vec<Action>> {
        (self.cr.looking == Some(id)).then(|| self.say_job(sticky::Job::Crop, fill(t(self.lang, Msg::NoteCropLooking), &[("n", &percent.to_string())])))
    }

    // --- the pointer: a box dragged out on a page ------------------------------------------------------------------

    /// A box was dragged out on `page` from `from` to `to` (points of the page as shown): the margins of the page are what is outside it, and the
    /// box asks which pages.
    #[inline(never)]
    pub(super) fn crop_dragged(&mut self, page: u32, from: [f32; 2], to: [f32; 2]) -> Vec<Action> {
        if let Some(actions) = self.crop_not_now() {
            return actions;
        }
        let Some((w, h)) = self.doc.as_ref().and_then(|d| d.layout.page_size_pt(page as usize)) else { return Vec::new() };
        let (x0, x1) = (f64::from(from[0].min(to[0])).clamp(0.0, w), f64::from(from[0].max(to[0])).clamp(0.0, w));
        let (y0, y1) = (f64::from(from[1].min(to[1])).clamp(0.0, h), f64::from(from[1].max(to[1])).clamp(0.0, h));
        if x1 - x0 < 8.0 || y1 - y0 < 8.0 {
            return Vec::new();
        }
        self.edit.tool = Tool::Select;
        self.menus_dirty = true;
        vec![Action::AskForm(self.crop_form([y0, w - x1, h - y1, x0].map(mm_text), 0)), Action::Invalidate]
    }

    // --- uncropping ------------------------------------------------------------------------------------------------

    /// Page > Uncrop pages...
    #[inline(never)]
    pub(super) fn ask_uncrop(&mut self) -> Vec<Action> {
        if let Some(actions) = self.crop_not_now() {
            return actions;
        }
        let lang = self.lang;
        let (choices, scope) = self.scope_choices(6);
        let items = vec![
            FormItem::new(Label, t(lang, Msg::ApplyTo), (12, 12, 90, 18)),
            FormItem::pick(choices, scope, (104, 8, 304, 200)),
            FormItem::new(Label, t(lang, Msg::ScopeRangeLabel), (12, 44, 130, 18)),
            FormItem::new(Edit, "", (144, 41, 264, 22)),
        ];
        vec![Action::AskForm(FormSpec { tag: TAG_UNCROP, title: t(lang, Msg::UncropTitle).to_string(), size: (420, 112), items, ok: t(lang, Msg::UncropTitle).to_string(), cancel: t(lang, Msg::Cancel).to_string(), preview: None })]
    }

    #[inline(never)]
    pub(super) fn uncrop_answered(&mut self, answer: Option<FormAnswer>) -> Vec<Action> {
        let Some(FormAnswer { values, .. }) = answer.filter(|a| a.values.len() == 2) else { return Vec::new() };
        if self.crop_not_now().is_some_and(|a| !a.is_empty()) || self.doc.is_none() {
            return Vec::new();
        }
        let which = values.first().and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
        match self.pages_of(which, values.get(1).map_or("", String::as_str)) {
            Ok(pages) => self.send_crop(CropOp::Reset(pages)),
            Err(actions) => actions,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_choice_names_its_pages_and_a_range_that_is_not_the_documents_names_none() {
        let pages = |s, current, chosen: &[u32], range: &str| scope_pages(s, current, chosen, 6, range);
        assert_eq!(pages(Scope::This, 3, &[], ""), Some(vec![3]));
        assert_eq!(pages(Scope::Chosen, 3, &[], ""), Some(vec![3]), "none chosen: the page in view");
        assert_eq!(pages(Scope::Chosen, 3, &[1, 4], ""), Some(vec![1, 4]));
        assert_eq!(pages(Scope::All, 0, &[], ""), Some(vec![0, 1, 2, 3, 4, 5]));
        // Odd and even are of the page numbers people read: page 1 is odd.
        assert_eq!(pages(Scope::Odd, 0, &[], ""), Some(vec![0, 2, 4]));
        assert_eq!(pages(Scope::Even, 0, &[], ""), Some(vec![1, 3, 5]));
        assert_eq!(pages(Scope::Range, 0, &[], "2-3,6"), Some(vec![1, 2, 5]));
        for bad in ["", "7", "0", "3-1x", "a"] {
            assert_eq!(pages(Scope::Range, 0, &[], bad), None, "{bad:?}");
        }
        // One page: no even pages at all.
        assert_eq!(scope_pages(Scope::Even, 0, &[], 1, ""), None);
        assert_eq!(scope_pages(Scope::Odd, 0, &[], 1, ""), Some(vec![0]));
        assert_eq!(scope_pages(Scope::All, 0, &[], 0, ""), None);
        // Pages that the document does not have are not pages (the list of thumbnails of a document that has changed).
        assert_eq!(scope_pages(Scope::Chosen, 0, &[2, 9], 6, ""), None);
    }

    #[test]
    fn millimetres_are_written_short_and_read_with_a_comma_too() {
        assert_eq!(mm_text(0.0), "0");
        assert_eq!(mm_text(PT_PER_MM * 12.0), "12");
        assert_eq!(mm_text(PT_PER_MM * 12.34), "12.3");
        assert_eq!(parse_mm("12,5"), Some(12.5));
        assert_eq!(parse_mm(" 3 "), Some(3.0));
        for bad in ["", "x", "-1", "1e999", "NaN"] {
            assert_eq!(parse_mm(bad), None, "{bad:?}");
        }
    }
}
