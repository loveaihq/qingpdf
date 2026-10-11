//! Filling in forms, text boxes and picture stamps (4b). The reader only asks: the engine lists the fields of a page, fills them in,
//! makes the text boxes and the stamps (`docs/view-api.md`); what is here is the window's side: the box a field is typed in (the
//! system's edit box, so that the input method of Chinese works as everywhere), the list a choice is made from, Tab from field to
//! field, the frame of a picture that is being placed and the box that is being dragged out for a text box.

use std::collections::HashMap;
use std::path::Path;

use qingpdf_core::view::{FieldInfo, FieldKind, FillValue, MAX_FREETEXT_CHARS, MAX_STAMP_FILE_BYTES, Watermark};

use super::editing::{Asked, Tool};
use super::*;
use crate::ui::{FieldBox, FieldExit};

/// The sizes of text a text box may have, as the menu names them (points).
pub(super) const TEXT_SIZES: [(Msg, f32); 4] = [(Msg::SizeSmall, 10.0), (Msg::SizeMedium, 12.0), (Msg::SizeLarge, 16.0), (Msg::SizeHuge, 24.0)];
const DEFAULT_TEXT_SIZE: usize = 1;
/// The size of the box a click makes for a text box, and the width a picture starts with (points).
const BOX_W: f32 = 180.0;
const BOX_H: f32 = 48.0;
const STAMP_W: f32 = 160.0;
/// The smallest a picture may be made by dragging (points), and the side of the handles at its corners (pixels at 96 dpi).
const STAMP_MIN: f32 = 16.0;
const HANDLE: i64 = 9;
/// How far (pixels at 96 dpi) the pointer must go to drag a box out rather than click.
const BOX_DRAG: i64 = 6;
const FRAME_COLOR: u32 = 0x0066FF;
const FIELD_COLOR: u32 = 0x3390FF;

/// What is typed in the box over the page.
enum Editing {
    /// The `index`-th field of the list of `page`.
    Field { page: u32, index: usize },
    /// A text box that will be where `rect` is (points of the page).
    TextBox { page: u32, rect: [f32; 4] },
}

/// A combo box or list that is being chosen from. Its options are not in the list of fields: they are asked for when it is clicked
/// (`Engine::options`), and the list opens when they come.
struct Choosing {
    page: u32,
    index: usize,
    /// The request for the options, until they have come.
    asked: Option<u64>,
    /// (export value, shown text).
    options: Vec<(String, String)>,
}

/// A picture that is being placed: where it is going (points of the page, the shape of the picture) and the bytes of its file.
struct StampDraft {
    page: u32,
    rect: [f32; 4],
    image: Vec<u8>,
    /// Width over height.
    ratio: f32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Grab {
    Move,
    /// 0 top left, 1 top right, 2 bottom left, 3 bottom right.
    Corner(usize),
}

pub(super) struct FormState {
    /// The fields of the pages near the window, and the engine's requests for them (request, page).
    fields: HashMap<u32, Vec<FieldInfo>>,
    asked: HashMap<u64, u32>,
    editing: Option<Editing>,
    /// The choice list is open (or about to open) for this field.
    choosing: Option<Choosing>,
    /// A box being dragged out for a text box: the page and the two corners (points).
    dragging: Option<(u32, [f32; 2], [f32; 2])>,
    stamp: Option<StampDraft>,
    /// What the pointer holds of the stamp frame, where it went down (points) and what the frame was then.
    grab: Option<(Grab, [f32; 2], [f32; 4])>,
    /// The file box that is open is for a picture, not for a document.
    pub picking: bool,
    pub text_size: usize,
    /// The text box or picture being made is written into the page's content (the Insert menu, 4c), not made an annotation.
    pub(super) content: bool,
    pub wm: Watermarking,
}

/// What a watermark made from the Insert menu is like: which opacity and angle of the lists below, and the three switches.
#[derive(Clone, Copy)]
pub(super) struct Watermarking {
    pub opacity: usize,
    pub angle: usize,
    pub tile: bool,
    pub under: bool,
    pub page_only: bool,
}

/// The opacities (percent) and angles (degrees) to choose from.
pub(super) const WM_OPACITY: [u32; 3] = [20, 30, 50];
pub(super) const WM_ANGLE: [i32; 3] = [0, 45, 90];

impl FormState {
    pub(super) fn new() -> FormState {
        let wm = Watermarking { opacity: 1, angle: 0, tile: false, under: false, page_only: false };
        FormState { fields: HashMap::new(), asked: HashMap::new(), editing: None, choosing: None, dragging: None, stamp: None, grab: None, picking: false, text_size: DEFAULT_TEXT_SIZE, content: false, wm }
    }

    pub(super) fn drop_page(&mut self, page: u32) {
        self.fields.remove(&page);
    }

    pub(super) fn has_stamp(&self) -> bool {
        self.stamp.is_some()
    }

    /// Esc: whatever was being placed or dragged is given up. `true` if there was something.
    pub(super) fn give_up(&mut self) -> bool {
        let had = self.stamp.is_some() || self.dragging.is_some() || self.editing.is_some();
        // (A list that has not opened yet is not to open after Esc.)
        if self.choosing.as_ref().is_some_and(|c| c.asked.is_some()) {
            self.choosing = None;
        }
        self.stamp = None;
        self.dragging = None;
        self.grab = None;
        had
    }

    pub(super) fn give_up_drag(&mut self) {
        self.dragging = None;
        self.grab = None;
    }
}

fn contains(r: [f32; 4], x: f32, y: f32) -> bool {
    r[0] <= x && x <= r[2] && r[1] <= y && y <= r[3]
}

fn fillable(kind: FieldKind) -> bool {
    matches!(kind, FieldKind::Text | FieldKind::Combo | FieldKind::List | FieldKind::Check | FieldKind::Radio)
}

impl App {
    // --- the fields of the pages ----------------------------------------------------------------------------------

    /// Ask the engine for the fields of the pages in view.
    pub(super) fn ask_for_fields(&mut self) {
        let Some((first, last)) = self.visible() else { return };
        let last = last.min(first.saturating_add(8));
        let stale: Vec<u64> = self.edit.forms.asked.iter().filter(|(_, p)| !(first..=last).contains(p)).map(|(id, _)| *id).collect();
        for id in stale {
            self.engine.cancel(id);
            self.edit.forms.asked.remove(&id);
        }
        for page in first..=last {
            if self.edit.forms.fields.contains_key(&page) || self.edit.forms.asked.values().any(|&p| p == page) {
                continue;
            }
            let id = self.new_id();
            self.engine.fields(self.doc_id, id, page);
            self.edit.forms.asked.insert(id, page);
        }
        self.edit.forms.fields.retain(|&p, _| p.abs_diff(first) <= 12 && p.abs_diff(last) <= 12);
    }

    pub(super) fn on_fields(&mut self, id: u64, page: u32, items: Vec<FieldInfo>) -> Vec<Action> {
        if self.edit.forms.asked.remove(&id).is_some() {
            self.edit.forms.fields.insert(page, items);
        }
        Vec::new()
    }

    /// The field of `page` that holds the point (x, y) of the page in points (the last of them is on top).
    pub(super) fn field_at(&self, page: u32, x: f64, y: f64) -> Option<usize> {
        let (x, y) = (x as f32, y as f32);
        self.edit.forms.fields.get(&page)?.iter().rposition(|f| contains(f.rect, x, y))
    }

    /// The shape of the pointer over a field that can be filled in.
    pub(super) fn field_cursor(&self, x: i64, y: i64) -> Option<Cursor> {
        if self.edit.tool != Tool::Select || !self.rights.fill_forms {
            return None;
        }
        let (page, px, py) = self.page_at_point(x, y)?;
        let map = self.doc.as_ref()?.layout.page_map(page)?;
        let (xp, yp) = map.to_pt(px, py);
        let f = self.edit.forms.fields.get(&(page as u32))?.get(self.field_at(page as u32, xp, yp)?)?;
        match f.kind {
            FieldKind::Text => (!f.read_only).then_some(Cursor::Text),
            FieldKind::Combo | FieldKind::List | FieldKind::Check | FieldKind::Radio => (!f.read_only).then_some(Cursor::Hand),
            FieldKind::Push | FieldKind::Signature => None,
        }
    }

    /// A field was clicked: a text field opens its box, a check box or radio button changes, a choice opens its list.
    pub(super) fn activate_field(&mut self, page: u32, index: usize) -> Vec<Action> {
        let Some(f) = self.edit.forms.fields.get(&page).and_then(|l| l.get(index)).cloned() else { return Vec::new() };
        if f.kind == FieldKind::Signature {
            return self.say(t(self.lang, Msg::NoteSignatureField).to_string());
        }
        if !fillable(f.kind) {
            return Vec::new();
        }
        if !self.rights.fill_forms {
            return self.say(t(self.lang, Msg::NoteFillDenied).to_string());
        }
        if f.read_only {
            return self.say(t(self.lang, Msg::NoteReadOnlyField).to_string());
        }
        match f.kind {
            FieldKind::Text => self.open_editor(page, index),
            FieldKind::Combo if f.editable => self.open_editor(page, index),
            FieldKind::Combo | FieldKind::List => {
                let id = self.new_id();
                self.engine.options(self.doc_id, id, page, f.index, f.num);
                self.edit.forms.choosing = Some(Choosing { page, index, asked: Some(id), options: Vec::new() });
                Vec::new()
            }
            FieldKind::Check => self.send_fill(page, &f, FillValue::Checked(!f.checked)),
            FieldKind::Radio if !f.checked => self.send_fill(page, &f, FillValue::Checked(true)),
            _ => Vec::new(),
        }
    }

    /// The options of the combo box or list that was clicked have come: the list opens under the field.
    pub(super) fn on_options(&mut self, id: u64, options: Vec<(String, String)>) -> Vec<Action> {
        let Some((page, index)) = self.edit.forms.choosing.as_ref().filter(|c| c.asked == Some(id)).map(|c| (c.page, c.index)) else { return Vec::new() };
        let field = self.edit.forms.fields.get(&page).and_then(|l| l.get(index)).cloned();
        let rect = field.as_ref().and_then(|f| self.window_rect(page, f.rect));
        let (Some(f), Some(rect), false) = (field, rect, options.is_empty()) else {
            self.edit.forms.choosing = None;
            return Vec::new();
        };
        let selected = options.iter().position(|(export, shown)| *shown == f.value || *export == f.value);
        let items = options.iter().map(|(_, shown)| shown.clone()).collect();
        if let Some(c) = self.edit.forms.choosing.as_mut() {
            c.asked = None;
            c.options = options;
        }
        vec![Action::Choose { items, selected, at: (rect.x, rect.y + rect.h) }]
    }

    fn send_fill(&mut self, page: u32, f: &FieldInfo, value: FillValue) -> Vec<Action> {
        let id = self.new_id();
        self.engine.fill_field(self.doc_id, id, page, f.index, f.num, value);
        self.edit.asked.insert(id, Asked::Fill);
        Vec::new()
    }

    /// An item of the list of a combo box or list was chosen.
    pub(super) fn chosen(&mut self, item: Option<usize>) -> Vec<Action> {
        let Some(Choosing { page, index, asked: None, options }) = self.edit.forms.choosing.take() else { return Vec::new() };
        let Some(f) = self.edit.forms.fields.get(&page).and_then(|l| l.get(index)).cloned() else { return Vec::new() };
        match item.and_then(|i| options.get(i)) {
            Some((export, _)) => self.send_fill(page, &f, FillValue::Text(export.clone())),
            None => Vec::new(),
        }
    }

    // --- the box things are typed in -------------------------------------------------------------------------------

    /// A rectangle of the page (points) as a rectangle of the window (pixels).
    fn window_rect(&self, page: u32, r: [f32; 4]) -> Option<Rect> {
        let l = &self.doc.as_ref()?.layout;
        let map = l.page_map(page as usize)?;
        let (x0, y0, x1, y1) = map.rect_to_px(r);
        let (left, top) = (self.view_x + l.left(page as usize, self.view_w, self.scroll_x), l.top(page as usize) - self.scroll_y);
        let (x0, y0, x1, y1) = (left + x0.floor() as i64, top + y0.floor() as i64, left + x1.ceil() as i64, top + y1.ceil() as i64);
        Some(Rect { x: clamp32(x0), y: clamp32(y0), w: clamp32((x1 - x0).max(1)), h: clamp32((y1 - y0).max(1)) })
    }

    fn open_editor(&mut self, page: u32, index: usize) -> Vec<Action> {
        let Some(f) = self.edit.forms.fields.get(&page).and_then(|l| l.get(index)).cloned() else { return Vec::new() };
        let Some(rect) = self.window_rect(page, f.rect) else { return Vec::new() };
        self.edit.forms.editing = Some(Editing::Field { page, index });
        vec![Action::FieldEdit(Some(FieldBox { rect, text: f.value.clone(), multiline: f.multiline, max_len: f.max_len }))]
    }

    /// The page moved under the box (a scroll, a zoom): the box goes with it.
    pub(super) fn reposition_editor(&self) -> Vec<Action> {
        let rect = match self.edit.forms.editing.as_ref() {
            Some(Editing::Field { page, index }) => self.edit.forms.fields.get(page).and_then(|l| l.get(*index)).and_then(|f| self.window_rect(*page, f.rect)),
            Some(Editing::TextBox { page, rect }) => self.window_rect(*page, *rect),
            None => None,
        };
        rect.map(|r| vec![Action::FieldMove(r)]).unwrap_or_default()
    }

    /// The person has left the box: keep what was typed (unless it was Esc) and go on to the next field if Tab was pressed.
    pub(super) fn field_done(&mut self, text: String, exit: FieldExit) -> Vec<Action> {
        let Some(editing) = self.edit.forms.editing.take() else { return Vec::new() };
        let mut actions = Vec::new();
        match editing {
            Editing::Field { page, index } => {
                let Some(f) = self.edit.forms.fields.get(&page).and_then(|l| l.get(index)).cloned() else { return actions };
                if exit != FieldExit::Cancel && text != f.value {
                    actions.extend(self.send_fill(page, &f, FillValue::Text(text)));
                }
                if matches!(exit, FieldExit::Next | FieldExit::Prev)
                    && let Some(next) = self.next_editable(page, index, exit == FieldExit::Next)
                {
                    actions.extend(self.open_editor(page, next));
                }
            }
            Editing::TextBox { page, rect } => {
                if exit != FieldExit::Cancel && !text.trim().is_empty() {
                    let id = self.new_id();
                    let size = TEXT_SIZES.get(self.edit.forms.text_size).map_or(12.0, |s| s.1);
                    let text: String = text.chars().take(MAX_FREETEXT_CHARS).collect();
                    if self.edit.forms.content {
                        // The text starts at the corner of the box, its first line a little below it.
                        self.engine.add_text(self.doc_id, id, page, [rect[0], rect[1] + size * 0.85], &text, size, self.rgb());
                    } else {
                        self.engine.add_freetext(self.doc_id, id, page, rect, &text, size, self.rgb(), &self.edit.author);
                    }
                    self.edit.asked.insert(id, Asked::Add);
                }
            }
        }
        actions
    }

    /// The field that Tab goes to from the `index`-th of `page`: the next (or previous) one a person types in, round the end of the list.
    fn next_editable(&self, page: u32, index: usize, forward: bool) -> Option<usize> {
        let list = self.edit.forms.fields.get(&page)?;
        let n = list.len();
        let typed = |f: &FieldInfo| !f.read_only && (f.kind == FieldKind::Text || (f.kind == FieldKind::Combo && f.editable));
        (1..n).map(|k| if forward { (index + k) % n } else { (index + n - k) % n }).find(|&i| list.get(i).is_some_and(typed))
    }

    // --- text boxes ------------------------------------------------------------------------------------------------

    pub(super) fn set_text_size(&mut self, index: usize) -> Vec<Action> {
        if index < TEXT_SIZES.len() {
            self.edit.forms.text_size = index;
            self.menus_dirty = true;
        }
        Vec::new()
    }

    /// The pointer went down with the text box tool or the stamp tool: a box is begun, or the frame of the picture taken hold of.
    pub(super) fn forms_mouse_down(&mut self, x: i64, y: i64) -> Option<Vec<Action>> {
        match self.edit.tool {
            Tool::TextBox => {
                let (page, ..) = self.page_at_point(x, y)?;
                let (xp, yp) = self.page_point(page as u32, x, y)?;
                self.edit.forms.dragging = Some((page as u32, [xp, yp], [xp, yp]));
                self.selection = None;
                Some(Vec::new())
            }
            Tool::Stamp => {
                let (page, ..) = self.page_at_point(x, y)?;
                let draft = self.edit.forms.stamp.as_ref().filter(|d| d.page as usize == page)?;
                let (xp, yp) = self.page_point(draft.page, x, y)?;
                let rect = draft.rect;
                let r = self.window_rect(draft.page, rect)?;
                let near = self.px(HANDLE) + 2;
                let corners = [(r.x, r.y), (r.x + r.w, r.y), (r.x, r.y + r.h), (r.x + r.w, r.y + r.h)];
                let grab = match corners.iter().position(|&(cx, cy)| (x - i64::from(cx)).abs() <= near && (y - i64::from(cy)).abs() <= near) {
                    Some(c) => Grab::Corner(c),
                    None if contains(rect, xp, yp) => Grab::Move,
                    None => return Some(Vec::new()),
                };
                self.edit.forms.grab = Some((grab, [xp, yp], rect));
                Some(Vec::new())
            }
            _ => None,
        }
    }

    pub(super) fn forms_mouse_move(&mut self, x: i64, y: i64) -> Option<Vec<Action>> {
        if let Some((page, start, _)) = self.edit.forms.dragging {
            let (xp, yp) = self.page_point(page, x, y)?;
            self.edit.forms.dragging = Some((page, start, [xp, yp]));
            return Some(vec![Action::Invalidate]);
        }
        let (grab, start, rect0) = self.edit.forms.grab?;
        let page = self.edit.forms.stamp.as_ref()?.page;
        let ratio = self.edit.forms.stamp.as_ref()?.ratio;
        let (xp, yp) = self.page_point(page, x, y)?;
        let (pw, ph) = self.doc.as_ref()?.layout.page_size_pt(page as usize)?;
        let (pw, ph) = (pw as f32, ph as f32);
        let rect = match grab {
            Grab::Move => {
                let (w, h) = (rect0[2] - rect0[0], rect0[3] - rect0[1]);
                let l = (rect0[0] + xp - start[0]).clamp(0.0, (pw - w).max(0.0));
                let t = (rect0[1] + yp - start[1]).clamp(0.0, (ph - h).max(0.0));
                [l, t, l + w, t + h]
            }
            Grab::Corner(c) => {
                // The opposite corner stays; the width is the distance across, the height follows the shape of the picture.
                let (ax, ay) = (if c % 2 == 0 { rect0[2] } else { rect0[0] }, if c < 2 { rect0[3] } else { rect0[1] });
                let room_w = if c % 2 == 0 { ax } else { pw - ax };
                let room_h = if c < 2 { ay } else { ph - ay };
                let w = (xp - ax).abs().clamp(STAMP_MIN, room_w.min(room_h * ratio).max(STAMP_MIN));
                let h = w / ratio;
                let (l, r) = if c % 2 == 0 { (ax - w, ax) } else { (ax, ax + w) };
                let (t, b) = if c < 2 { (ay - h, ay) } else { (ay, ay + h) };
                [l, t, r, b]
            }
        };
        if let Some(d) = self.edit.forms.stamp.as_mut() {
            d.rect = rect;
        }
        Some(vec![Action::Invalidate])
    }

    pub(super) fn forms_mouse_up(&mut self, x: i64, y: i64) -> Option<Vec<Action>> {
        if self.edit.forms.grab.take().is_some() {
            return Some(vec![Action::Invalidate]);
        }
        let (page, start, now) = self.edit.forms.dragging.take()?;
        let now = self.page_point(page, x, y).map_or(now, |(a, b)| [a, b]);
        let (sx, sy) = self.window_rect(page, [start[0], start[1], start[0], start[1]]).map(|r| (i64::from(r.x), i64::from(r.y)))?;
        let dragged = (x - sx).abs().max((y - sy).abs()) >= self.px(BOX_DRAG);
        // A click makes a box of the usual size; a drag, the box dragged out.
        let rect = if dragged {
            [start[0].min(now[0]), start[1].min(now[1]), start[0].max(now[0]), start[1].max(now[1])]
        } else {
            [start[0], start[1], start[0] + BOX_W, start[1] + BOX_H]
        };
        let Some(window) = self.window_rect(page, rect) else { return Some(Vec::new()) };
        self.edit.forms.editing = Some(Editing::TextBox { page, rect });
        Some(vec![Action::FieldEdit(Some(FieldBox { rect: window, text: String::new(), multiline: true, max_len: MAX_FREETEXT_CHARS as u32 })), Action::Invalidate])
    }

    // --- picture stamps --------------------------------------------------------------------------------------------

    /// The menu item: ask which picture.
    pub(super) fn ask_for_stamp(&mut self, content: bool) -> Vec<Action> {
        if self.doc.is_none() {
            return Vec::new();
        }
        self.edit.forms.content = content;
        if !self.can_make() {
            return self.say(t(self.lang, if content { Msg::NoteModifyDenied } else { Msg::NoteAnnotDenied }).to_string());
        }
        self.edit.forms.picking = true;
        let filters = vec![(t(self.lang, Msg::FilterImages).to_string(), "*.png;*.jpg;*.jpeg".to_string()), (t(self.lang, Msg::FilterAll).to_string(), "*.*".to_string())];
        vec![Action::PickFile { title: t(self.lang, Msg::StampTitle).to_string(), filters }]
    }

    /// A picture was chosen: its frame is put in the middle of the window, to be moved and resized.
    pub(super) fn stamp_picked(&mut self, path: &Path) -> Vec<Action> {
        let bad = |this: &mut Self, why: String| this.say(format!("{} {why}", t(this.lang, Msg::NoteStampBad)));
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if size > MAX_STAMP_FILE_BYTES as u64 {
            return bad(self, format!("> {} MB", MAX_STAMP_FILE_BYTES >> 20));
        }
        let image = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => return bad(self, e.to_string()),
        };
        let (w, h) = match qingpdf_core::image::probe(&image) {
            Ok((w, h, _)) if w > 0 && h > 0 => (w as f32, h as f32),
            Ok(_) => return bad(self, String::new()),
            Err(e) => return bad(self, e.to_string()),
        };
        let page = self.current_page() as u32;
        let Some((pw, ph)) = self.doc.as_ref().and_then(|d| d.layout.page_size_pt(page as usize)) else { return Vec::new() };
        let (pw, ph) = (pw as f32, ph as f32);
        let ratio = w / h;
        let mut width = STAMP_W.min(pw * 0.6);
        if width / ratio > ph * 0.6 {
            width = ph * 0.6 * ratio;
        }
        let height = width / ratio;
        let (cx, cy) = self.page_point(page, self.view_x + self.view_w / 2, self.view_h / 2).unwrap_or((pw / 2.0, ph / 2.0));
        let l = (cx - width / 2.0).clamp(0.0, (pw - width).max(0.0));
        let top = (cy - height / 2.0).clamp(0.0, (ph - height).max(0.0));
        self.edit.forms.stamp = Some(StampDraft { page, rect: [l, top, l + width, top + height], image, ratio });
        self.edit.ink = None;
        self.edit.picked = None;
        self.edit.tool = Tool::Stamp;
        self.menus_dirty = true;
        let mut actions = self.say(t(self.lang, Msg::NoteStampHint).to_string());
        actions.push(Action::Invalidate);
        actions
    }

    /// Enter: the picture is stamped where its frame is.
    pub(super) fn stamp_place(&mut self) -> Option<Vec<Action>> {
        let draft = self.edit.forms.stamp.take()?;
        self.edit.tool = Tool::Select;
        self.menus_dirty = true;
        let id = self.new_id();
        if self.edit.forms.content {
            self.engine.add_image(self.doc_id, id, draft.page, draft.rect, draft.image);
        } else {
            self.engine.add_stamp(self.doc_id, id, draft.page, draft.rect, draft.image, &self.edit.author);
        }
        self.edit.asked.insert(id, Asked::Add);
        Some(vec![Action::Invalidate])
    }

    // --- the Insert menu (4c) ----------------------------------------------------------------------------------------

    /// Whether the text box or picture about to be made may be: a page's content needs the right to change it, an annotation the
    /// right to annotate.
    pub(super) fn can_make(&self) -> bool {
        if self.edit.forms.content { self.rights.modify } else { self.rights.annotate }
    }

    /// The text box tool: its text goes into an annotation (the Comment menu) or into the page (the Insert menu).
    pub(super) fn text_tool(&mut self, content: bool) -> Vec<Action> {
        self.edit.forms.content = content;
        self.set_tool(Tool::TextBox)
    }

    pub(super) fn ask_watermark(&mut self) -> Vec<Action> {
        if self.doc.is_none() {
            return Vec::new();
        }
        if !self.rights.modify {
            return self.say(t(self.lang, Msg::NoteModifyDenied).to_string());
        }
        vec![Action::AskText {
            tag: TAG_WATERMARK,
            title: t(self.lang, Msg::InsertWatermark).replace('&', ""),
            prompt: t(self.lang, Msg::WatermarkPrompt).to_string(),
            secret: false,
            ok: t(self.lang, Msg::Ok).to_string(),
            cancel: t(self.lang, Msg::Cancel).to_string(),
        }]
    }

    /// The text of the watermark was typed: it goes on every page, or the one in view, as the menu says.
    pub(super) fn watermark_answered(&mut self, text: Option<String>) -> Vec<Action> {
        let Some(text) = text.filter(|t| !t.trim().is_empty()) else { return Vec::new() };
        let wm = self.edit.forms.wm;
        let pages: Vec<u32> = if wm.page_only { vec![self.current_page() as u32] } else { (0..self.page_count() as u32).collect() };
        let opacity = WM_OPACITY.get(wm.opacity).map_or(30, |&p| p) as f32 / 100.0;
        let angle = WM_ANGLE.get(wm.angle).map_or(0, |&a| a) as f32;
        let mark = Watermark { text, image: Vec::new(), opacity, angle, tile: wm.tile, under: wm.under, pages };
        let id = self.new_id();
        self.engine.add_watermark(self.doc_id, id, mark);
        self.edit.asked.insert(id, Asked::Add);
        Vec::new()
    }

    pub(super) fn set_watermark_option(&mut self, cmd: u32) -> Vec<Action> {
        let wm = &mut self.edit.forms.wm;
        match cmd {
            CMD_WM_TILE => wm.tile = !wm.tile,
            CMD_WM_UNDER => wm.under = !wm.under,
            CMD_WM_PAGE => wm.page_only = !wm.page_only,
            c if c >= CMD_WM_ANGLE_BASE => wm.angle = ((c - CMD_WM_ANGLE_BASE) as usize).min(WM_ANGLE.len() - 1),
            c => wm.opacity = ((c - CMD_WM_OPACITY_BASE) as usize).min(WM_OPACITY.len() - 1),
        }
        self.menus_dirty = true;
        Vec::new()
    }

    // --- drawing -----------------------------------------------------------------------------------------------------

    /// The frame of the picture being placed and the box being dragged out, on a page whose top left corner is at (`left`, `top`).
    pub(super) fn draw_form_marks(&self, p: &mut dyn Painter, page: usize, left: i64, top: i64) {
        let Some(map) = self.doc.as_ref().and_then(|d| d.layout.page_map(page)) else { return };
        let to_window = |r: [f32; 4]| {
            let (x0, y0, x1, y1) = map.rect_to_px(r);
            (left + x0.floor() as i64, top + y0.floor() as i64, left + x1.ceil() as i64, top + y1.ceil() as i64)
        };
        let rect = |x: i64, y: i64, w: i64, h: i64| Rect { x: clamp32(x), y: clamp32(y), w: clamp32(w.max(1)), h: clamp32(h.max(1)) };
        let outline = |p: &mut dyn Painter, (x0, y0, x1, y1): (i64, i64, i64, i64), color: u32, t: i64| {
            p.fill_rect(rect(x0, y0, x1 - x0, t), color);
            p.fill_rect(rect(x0, y1 - t, x1 - x0, t), color);
            p.fill_rect(rect(x0, y0, t, y1 - y0), color);
            p.fill_rect(rect(x1 - t, y0, t, y1 - y0), color);
        };
        let t = self.px(2).max(1);
        if let Some((pg, a, b)) = self.edit.forms.dragging.filter(|d| d.0 as usize == page) {
            let _ = pg;
            outline(p, to_window([a[0].min(b[0]), a[1].min(b[1]), a[0].max(b[0]), a[1].max(b[1])]), FRAME_COLOR, t);
        }
        if let Some(d) = self.edit.forms.stamp.as_ref().filter(|d| d.page as usize == page) {
            let (x0, y0, x1, y1) = to_window(d.rect);
            p.fill_rect_alpha(rect(x0, y0, x1 - x0, y1 - y0), FRAME_COLOR, 40);
            outline(p, (x0, y0, x1, y1), FRAME_COLOR, t);
            let h = self.px(HANDLE);
            for (cx, cy) in [(x0, y0), (x1, y0), (x0, y1), (x1, y1)] {
                p.fill_rect(rect(cx - h / 2, cy - h / 2, h, h), 0xFFFFFF);
                outline(p, (cx - h / 2, cy - h / 2, cx - h / 2 + h, cy - h / 2 + h), FRAME_COLOR, t.min(2));
            }
        }
        // The fields a person can fill in show faintly when the pointer is over the page with nothing else going on.
        if self.edit.tool == Tool::Select
            && self.rights.fill_forms
            && let Some(list) = self.edit.forms.fields.get(&(page as u32))
        {
            for f in list.iter().filter(|f| fillable(f.kind) && !f.read_only) {
                let (x0, y0, x1, y1) = to_window(f.rect);
                p.fill_rect_alpha(rect(x0, y0, x1 - x0, y1 - y0), FIELD_COLOR, 22);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::i18n::Lang;

    fn pump(app: &mut App, what: &str, until: impl Fn(&App) -> bool) {
        for _ in 0..1000 {
            app.event(Event::Wake);
            if until(app) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for {what}");
    }

    fn pdf(objects: &[(u32, String)]) -> Vec<u8> {
        let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
        let mut offsets = std::collections::BTreeMap::new();
        for (n, body) in objects {
            offsets.insert(*n, out.len());
            out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
        }
        let size = objects.iter().map(|o| o.0).max().unwrap_or(0) + 1;
        let at = out.len();
        out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        for n in 0..size {
            match offsets.get(&n) {
                Some(o) => out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes()),
                None => out.extend_from_slice(b"0000000000 65535 f \n"),
            }
        }
        out.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{at}\n%%EOF\n").as_bytes());
        out
    }

    /// A PNG of `w` by `h` pixels, every pixel half see-through red (the deflate blocks are stored, not compressed).
    fn png(w: u32, h: u32) -> Vec<u8> {
        fn crc(data: &[u8]) -> u32 {
            let mut c = 0xFFFF_FFFFu32;
            for &b in data {
                c ^= u32::from(b);
                for _ in 0..8 {
                    c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
                }
            }
            !c
        }
        fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let mut body = kind.to_vec();
            body.extend_from_slice(data);
            out.extend_from_slice(&body);
            out.extend_from_slice(&crc(&body).to_be_bytes());
        }
        let mut raw = Vec::new();
        for _ in 0..h {
            raw.extend_from_slice(&[0]);
            for _ in 0..w {
                raw.extend_from_slice(&[255, 0, 0, 128]);
            }
        }
        let mut z = vec![0x78, 0x01];
        for (i, block) in raw.chunks(65_535).enumerate() {
            let last = (i + 1) * 65_535 >= raw.len();
            z.push(u8::from(last));
            z.extend_from_slice(&(block.len() as u16).to_le_bytes());
            z.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
            z.extend_from_slice(block);
        }
        let (mut a, mut b) = (1u32, 0u32);
        for &x in &raw {
            a = (a + u32::from(x)) % 65_521;
            b = (b + a) % 65_521;
        }
        z.extend_from_slice(&((b << 16) | a).to_be_bytes());
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        let mut header = Vec::new();
        header.extend_from_slice(&w.to_be_bytes());
        header.extend_from_slice(&h.to_be_bytes());
        header.extend_from_slice(&[8, 6, 0, 0, 0]);
        chunk(&mut out, b"IHDR", &header);
        chunk(&mut out, b"IDAT", &z);
        chunk(&mut out, b"IEND", &[]);
        out
    }

    fn centre(app: &App, index: usize) -> (i32, i32) {
        let f = &app.edit.forms.fields[&0][index];
        let r = app.window_rect(0, f.rect).expect("the field is on the window");
        (r.x + r.w / 2, r.y + r.h / 2)
    }

    fn click(app: &mut App, at: (i32, i32)) -> Vec<Action> {
        app.event(Event::MouseDown { x: at.0, y: at.1, ctrl: false, shift: false });
        app.event(Event::MouseUp { x: at.0, y: at.1 })
    }

    fn field_value(app: &App, index: usize) -> String {
        app.edit.forms.fields.get(&0).and_then(|l| l.get(index)).map(|f| f.value.clone()).unwrap_or_default()
    }

    #[test]
    fn a_form_is_filled_by_clicks_and_tab_and_gets_a_text_box_and_a_stamp() {
        let objects = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [10 0 R 11 0 R 12 0 R 13 0 R] /DA (/Helv 12 Tf 0 g) >> >>".to_string()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Annots [10 0 R 11 0 R 12 0 R 13 0 R] >>".to_string()),
            (10, "<< /Type /Annot /Subtype /Widget /FT /Tx /T (a) /Rect [20 240 200 260] /MK << /BC [0 0 0] >> >>".to_string()),
            (11, "<< /Type /Annot /Subtype /Widget /FT /Tx /T (b) /V (old) /Rect [20 200 200 220] /MK << /BC [0 0 0] >> >>".to_string()),
            (12, "<< /Type /Annot /Subtype /Widget /FT /Btn /T (c) /Rect [20 150 40 170] /MK << /BC [0 0 0] >> >>".to_string()),
            (13, "<< /Type /Annot /Subtype /Widget /FT /Ch /Ff 131072 /T (d) /Rect [20 100 200 120] /Opt [(One) (Two) (Three)] /MK << /BC [0 0 0] >> >>".to_string()),
        ];
        let dir = std::env::temp_dir().join(format!("qingpdf-view-forms-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("form.pdf");
        std::fs::write(&file, pdf(&objects)).unwrap();
        let picture = dir.join("sig.png");
        std::fs::write(&picture, png(40, 20)).unwrap();

        let mut app = App::new(Lang::En, Arc::new(|| {}), Instant::now(), 1920 * 1080, "Tester".to_string());
        app.event(Event::Size { w: 900, h: 700 });
        app.open_path(&file, "");
        pump(&mut app, "the file to open", |a| a.has_document());
        pump(&mut app, "the fields", |a| a.edit.forms.fields.contains_key(&0));
        assert_eq!(app.edit.forms.fields[&0].len(), 4);

        // A click on the first field opens the box for one line, empty; Tab keeps what was typed and opens the next field's box, with
        // what that has in it.
        let opened = { let at = centre(&app, 0); click(&mut app, at) };
        assert!(opened.iter().any(|a| matches!(a, Action::FieldEdit(Some(b)) if !b.multiline && b.text.is_empty())), "{opened:?}");
        let tab = app.event(Event::FieldDone { text: "张三".to_string(), exit: FieldExit::Next });
        assert!(tab.iter().any(|a| matches!(a, Action::FieldEdit(Some(b)) if b.text == "old")), "{tab:?}");
        pump(&mut app, "the first field to be filled", |a| field_value(a, 0) == "张三");
        assert!(app.edit.dirty);
        // Esc throws away what was typed.
        app.event(Event::FieldDone { text: "never".to_string(), exit: FieldExit::Cancel });
        assert_eq!(field_value(&app, 1), "old");

        // The check box turns on at a click; the combo box (not typed in) opens a list, and the choice is made from it.
        { let at = centre(&app, 2); click(&mut app, at) };
        pump(&mut app, "the check box", |a| a.edit.forms.fields.get(&0).and_then(|l| l.get(2)).is_some_and(|f| f.checked));
        // (The options are asked for when the field is clicked; the list opens when they come.)
        { let at = centre(&app, 3); click(&mut app, at) };
        let mut list = Vec::new();
        for _ in 0..1000 {
            list.extend(app.event(Event::Wake));
            if !list.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(list.iter().any(|a| matches!(a, Action::Choose { items, .. } if items.len() == 3)), "{list:?}");
        app.event(Event::Chosen(Some(2)));
        pump(&mut app, "the choice", |a| field_value(a, 3) == "Three");

        // A text box: drag out a box, type Chinese, leave it by clicking outside.
        app.event(Event::Command(CMD_TOOL_TEXTBOX));
        let r = app.window_rect(0, [150.0, 30.0, 150.0, 30.0]).unwrap();
        app.event(Event::MouseDown { x: r.x, y: r.y, ctrl: false, shift: false });
        app.event(Event::MouseMove { x: r.x + 60, y: r.y + 30 });
        let boxed = app.event(Event::MouseUp { x: r.x + 120, y: r.y + 60 });
        assert!(boxed.iter().any(|a| matches!(a, Action::FieldEdit(Some(b)) if b.multiline && b.rect.w >= 100)), "{boxed:?}");
        app.event(Event::FieldDone { text: "审批意见：同意".to_string(), exit: FieldExit::Blur });
        pump(&mut app, "the text box", |a| a.edit.annots.get(&0).is_some_and(|l| l.iter().any(|i| i.subtype == "FreeText" && i.contents.contains("审批"))));
        app.event(Event::Command(CMD_TOOL_SELECT));

        // A stamp: the picture is asked for, its frame appears in the middle, is moved and made smaller (with the shape of the picture),
        // and Enter puts it.
        let asked = app.event(Event::Command(CMD_STAMP));
        assert!(asked.iter().any(|a| matches!(a, Action::PickFile { .. })));
        app.event(Event::FilePicked(Some(picture.clone())));
        let (page, before) = {
            let d = app.edit.forms.stamp.as_ref().expect("a frame for the picture");
            (d.page, d.rect)
        };
        assert!(((before[2] - before[0]) / (before[3] - before[1]) - 2.0).abs() < 0.01, "the frame has the shape of the picture: {before:?}");
        let inside = app.window_rect(page, before).map(|r| (r.x + r.w / 2, r.y + r.h / 2)).unwrap();
        app.event(Event::MouseDown { x: inside.0, y: inside.1, ctrl: false, shift: false });
        app.event(Event::MouseMove { x: inside.0 + 40, y: inside.1 + 30 });
        app.event(Event::MouseUp { x: inside.0 + 40, y: inside.1 + 30 });
        let moved = app.edit.forms.stamp.as_ref().unwrap().rect;
        assert!(moved[0] > before[0] && moved[1] > before[1] && ((moved[2] - moved[0]) - (before[2] - before[0])).abs() < 0.01, "{before:?} -> {moved:?}");
        let corner = app.window_rect(page, moved).map(|r| (r.x + r.w, r.y + r.h)).unwrap();
        app.event(Event::MouseDown { x: corner.0, y: corner.1, ctrl: false, shift: false });
        app.event(Event::MouseMove { x: corner.0 - 30, y: corner.1 - 5 });
        app.event(Event::MouseUp { x: corner.0 - 30, y: corner.1 - 5 });
        let smaller = app.edit.forms.stamp.as_ref().unwrap().rect;
        let (w, h) = (smaller[2] - smaller[0], smaller[3] - smaller[1]);
        assert!(w < moved[2] - moved[0] && (w / h - 2.0).abs() < 0.01 && smaller[0] == moved[0], "{moved:?} -> {smaller:?}");
        app.event(Event::Key { vk: vk::RETURN, ctrl: false, shift: false });
        pump(&mut app, "the stamp", |a| a.edit.annots.get(&0).is_some_and(|l| l.iter().any(|i| i.subtype == "Stamp")));
        assert!(app.edit.forms.stamp.is_none());

        // Everything saved, and a picture that is no picture only says so.
        app.event(Event::Command(CMD_SAVE));
        pump(&mut app, "the save", |a| !a.edit.dirty && a.edit.saving.is_none());
        let doc = qingpdf_core::Document::open(&file).unwrap();
        assert!(doc.page_count().unwrap() == 1 && std::fs::metadata(&file).unwrap().len() > 3000);
        let junk = dir.join("junk.png");
        std::fs::write(&junk, b"not a picture at all").unwrap();
        app.event(Event::Command(CMD_STAMP));
        app.event(Event::FilePicked(Some(junk)));
        assert!(app.edit.forms.stamp.is_none() && app.note.as_deref().is_some_and(|n| n.contains("PNG or JPEG")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn menu_items(menus: &[Menu]) -> Vec<(u32, bool, bool)> {
        fn walk(entries: &[crate::ui::MenuEntry], out: &mut Vec<(u32, bool, bool)>) {
            for e in entries {
                match e {
                    crate::ui::MenuEntry::Item { id, checked, enabled, .. } => out.push((*id, *checked, *enabled)),
                    crate::ui::MenuEntry::Submenu { entries, .. } => walk(entries, out),
                    crate::ui::MenuEntry::Separator => {}
                }
            }
        }
        let mut out = Vec::new();
        menus.iter().for_each(|m| walk(&m.entries, &mut out));
        out
    }

    #[test]
    fn the_insert_menu_writes_text_a_picture_and_a_watermark_into_the_pages() {
        if !["msyh.ttc", "simsun.ttc", "simhei.ttf"].iter().any(|f| std::path::Path::new("C:/Windows/Fonts").join(f).is_file()) {
            println!("SKIPPED: no Chinese TrueType font on this machine");
            return;
        }
        let objects = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".to_string()),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] >>".to_string()),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] >>".to_string()),
        ];
        let dir = std::env::temp_dir().join(format!("qingpdf-view-insert-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("two.pdf");
        std::fs::write(&file, pdf(&objects)).unwrap();
        let picture = dir.join("logo.png");
        std::fs::write(&picture, png(40, 20)).unwrap();

        let mut app = App::new(Lang::En, Arc::new(|| {}), Instant::now(), 1920 * 1080, "Tester".to_string());
        app.event(Event::Size { w: 900, h: 700 });
        app.open_path(&file, "");
        pump(&mut app, "the file to open", |a| a.has_document());
        let items = menu_items(&app.menus());
        assert!([CMD_INS_TEXT, CMD_INS_IMAGE, CMD_INS_WM, CMD_WM_TILE].iter().all(|c| items.iter().any(|i| i.0 == *c && i.2)), "the Insert menu is enabled: {items:?}");

        // Text: the text box tool, a click, Chinese typed; it goes into the page, not into an annotation.
        app.event(Event::Command(CMD_INS_TEXT));
        let r = app.window_rect(0, [40.0, 60.0, 40.0, 60.0]).unwrap();
        app.event(Event::MouseDown { x: r.x, y: r.y, ctrl: false, shift: false });
        app.event(Event::MouseUp { x: r.x, y: r.y });
        app.event(Event::FieldDone { text: "你好，世界".to_string(), exit: FieldExit::Blur });
        pump(&mut app, "the text", |a| a.edit.can_undo);
        app.event(Event::Command(CMD_TOOL_SELECT));
        // A picture: asked for, framed, put with Enter (into the page, not as a stamp).
        let asked = app.event(Event::Command(CMD_INS_IMAGE));
        assert!(asked.iter().any(|a| matches!(a, Action::PickFile { .. })));
        app.event(Event::FilePicked(Some(picture)));
        assert!(app.edit.forms.stamp.is_some());
        app.event(Event::Key { vk: vk::RETURN, ctrl: false, shift: false });
        pump(&mut app, "the picture", |a| a.edit.forms.stamp.is_none() && a.edit.asked.is_empty());
        // The watermark: its options are chosen from the menu (tiled, turned 45 degrees, 50 per cent, under the content), its text typed.
        for cmd in [CMD_WM_TILE, CMD_WM_UNDER, CMD_WM_OPACITY_BASE + 2, CMD_WM_ANGLE_BASE + 1] {
            app.event(Event::Command(cmd));
        }
        let items = menu_items(&app.menus());
        let checked = |c: u32| items.iter().any(|i| i.0 == c && i.1);
        assert!(checked(CMD_WM_TILE) && checked(CMD_WM_UNDER) && checked(CMD_WM_OPACITY_BASE + 2) && checked(CMD_WM_ANGLE_BASE + 1) && !checked(CMD_WM_PAGE));
        let ask = app.event(Event::Command(CMD_INS_WM));
        assert!(ask.iter().any(|a| matches!(a, Action::AskText { tag: TAG_WATERMARK, .. })), "{ask:?}");
        app.event(Event::Text { tag: TAG_WATERMARK, text: Some("机密".to_string()) });
        pump(&mut app, "the watermark", |a| a.edit.asked.is_empty());
        assert!(app.edit.dirty && app.edit.can_undo && app.note.as_deref().is_some_and(|n| n.contains("added")), "{:?}", app.note);
        // Nothing is an annotation: no list of annotations has a thing in it.
        assert!(app.edit.annots.values().all(Vec::is_empty));

        // Saved: the Chinese text is in the content of page 1, the watermark in both pages, and the picture is a drawn image.
        app.event(Event::Command(CMD_SAVE));
        pump(&mut app, "the save", |a| !a.edit.dirty && a.edit.saving.is_none());
        let doc = qingpdf_core::Document::open(&file).unwrap();
        let pages = doc.pages().unwrap();
        let mut extractor = qingpdf_core::text::TextExtractor::new(&doc);
        let first = extractor.page_text(&pages[0]).unwrap();
        assert!(first.contains("你好，世界") && !first.contains("机密"), "the added text is text, the watermark is not: {first:?}");
        assert!(!extractor.page_text(&pages[1]).unwrap().contains("机密"));
        // The watermark is on both pages, drawn as an artifact (14.8.2.2).
        for (n, page) in pages.iter().enumerate() {
            let Ok(qingpdf_core::Object::Dict(dict)) = doc.get(page.obj_ref) else { panic!("page {n}") };
            let Some(qingpdf_core::Object::Array(list)) = dict.get("Contents") else { panic!("page {n}: {dict:?}") };
            let marked = list.iter().filter_map(|o| o.as_obj_ref()).any(|r| match doc.get(r) {
                Ok(qingpdf_core::Object::Stream(s)) => String::from_utf8_lossy(&doc.decode_stream(&s).unwrap()).contains("/Subtype /Watermark>> BDC"),
                _ => false,
            });
            assert!(marked, "page {n} has no watermark");
        }
        assert!(!String::from_utf8_lossy(&std::fs::read(&file).unwrap()).contains("/Subtype /Stamp"));
        // A file whose author does not allow changing it: the menu is greyed and a try is told why.
        app.rights.modify = false;
        app.menus_dirty = true;
        assert!(menu_items(&app.menus()).iter().all(|i| ![CMD_INS_TEXT, CMD_INS_WM].contains(&i.0) || !i.2));
        assert!(app.event(Event::Command(CMD_INS_WM)).iter().all(|a| !matches!(a, Action::AskText { .. })));
        assert!(app.note.as_deref().is_some_and(|n| n.contains("content")), "{:?}", app.note);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
