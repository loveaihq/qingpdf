//! Marking text, drawing, notes, taking annotations out, undo and redo, saving (4a). The reader only asks: the engine makes the
//! annotations, keeps the history and writes the file (`docs/view-api.md`); what is here is the tools, what is shown meanwhile
//! (a stroke being drawn, the annotation picked) and the questions about unsaved changes.

use std::path::PathBuf;

use qingpdf_core::idmap::IdMap;
use qingpdf_core::view::{ALL_PAGES, AnnotInfo, NO_PAGE, EditResult, EditStatus, MarkupKind, SaveStatus, ShapeKind, line_rects};

use super::annotedit::AnnotEdit;
use super::forms::FormState;
use super::*;
use crate::ui::SaveChoice;

/// The colours to choose from, as the menu names them.
pub(super) const COLORS: [(Msg, [u8; 3]); 6] = [
    (Msg::ColYellow, [255, 235, 0]),
    (Msg::ColGreen, [120, 220, 90]),
    (Msg::ColBlue, [90, 170, 255]),
    (Msg::ColPink, [255, 120, 190]),
    (Msg::ColRed, [230, 30, 30]),
    (Msg::ColBlack, [0, 0, 0]),
];
/// The line widths of a drawing, in points.
pub(super) const WIDTHS: [(Msg, f32); 4] = [(Msg::WidthThin, 1.0), (Msg::WidthMedium, 2.0), (Msg::WidthThick, 4.0), (Msg::WidthBold, 8.0)];
const DEFAULT_WIDTH: usize = 1;
/// Most points kept of one stroke while it is drawn (the engine thins a longer one).
const MAX_DRAFT_POINTS: usize = 20_000;

/// What the pointer does on the pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tool {
    /// Select text, follow links, pick annotations.
    Select,
    /// Mark the text that is dragged over.
    Markup(MarkupKind),
    Ink,
    Note,
    /// Drag out a box and type the text that goes in it (4b).
    TextBox,
    /// The frame of a picture is being placed (4b); the tool is set by choosing the picture.
    Stamp,
    /// Click a line of the page's text and type what it is to read instead (5a).
    EditText,
    /// Drag out a rectangle, an ellipse, a line or an arrow (2026-10-17).
    Shape(ShapeKind),
    /// Drag out the box of the page that is to be kept (2026-10-21).
    Crop,
}

/// What is to be done once the changes are saved (or given up).
#[derive(Clone, Debug)]
pub(super) enum After {
    /// The window is to close (the tabs with changes are asked about one after the other).
    Quit,
    /// The shown tab is to close.
    CloseTab,
    /// The box for compressing the file is shown (the changes were saved first).
    Compress,
}

/// A stroke being drawn: the page and the points so far (points of the page as the engine tells them).
pub(super) struct InkDraft {
    page: u32,
    points: Vec<[f32; 2]>,
}

/// What the engine was asked and has not yet answered.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Asked {
    Add,
    Fill,
    Remove,
    Undo,
    Redo,
    /// A line of the page's text was changed (5a).
    Line,
    /// Marks for redaction were made by a find (2026-10-20): the engine says how many.
    Mark,
    /// The text layer of a run of text recognition was written (5c): the run says how it went.
    Ocr,
    /// Pages were turned, taken out, moved, or put in from a file (2026-10-15).
    Rotate,
    Delete,
    Move,
    Insert,
    /// Page numbers, headers and footers were put on, or taken away.
    Pagenum,
    Unpagenum,
    /// An annotation was moved, resized, drawn again or given a text (2026-10-17).
    Change,
    /// The bookmarks were changed (2026-10-20).
    Bookmarks,
    /// Pages were cropped or uncropped, and the properties of the file were changed (2026-10-21).
    Crop,
    Uncrop,
    Meta,
}

impl Asked {
    /// What is said when the change to the pages is done.
    pub(super) fn pages_note(self) -> Option<Msg> {
        match self {
            Asked::Rotate => Some(Msg::NotePagesRotated),
            Asked::Delete => Some(Msg::NotePagesDeleted),
            Asked::Move => Some(Msg::NotePagesMoved),
            Asked::Insert => Some(Msg::NotePagesInserted),
            Asked::Crop => Some(Msg::NoteCropped),
            Asked::Uncrop => Some(Msg::NoteUncropped),
            _ => None,
        }
    }
}

pub(super) struct EditState {
    pub tool: Tool,
    /// The colour the person chose (none yet: the tool's own).
    pub color: Option<usize>,
    pub width: usize,
    pub author: String,
    pub(super) ink: Option<InkDraft>,
    /// The annotations of the pages near the window, and the engine's requests for them (request, page).
    pub(super) annots: IdMap<u32, Vec<AnnotInfo>>,
    annots_asked: IdMap<u64, u32>,
    /// The annotation picked: its page and its place in that page's list.
    pub(super) picked: Option<(u32, usize)>,
    pub(super) asked: IdMap<u64, Asked>,
    pub can_undo: bool,
    pub can_redo: bool,
    pub dirty: bool,
    /// The structure of the file was damaged: it is written afresh, as a whole. And the bookmarks were asked for again because of it (the
    /// objects have other numbers then, and the list the window has names the old ones).
    rewrote: bool,
    rewrote_listed: bool,
    /// The save that is going on (its request) and where it writes; what to do when it is done.
    pub(super) saving: Option<(u64, PathBuf, bool)>,
    /// The Word export (5b): the box that asks where to write it is open, and the request that is writing it.
    /// A name was asked for an export: to Excel (true) or to Word (false).
    export_asked: Option<bool>,
    pub(super) exporting: Option<u64>,
    /// The export going is to Excel.
    export_excel: bool,
    pub(super) after: Option<After>,
    /// The place where the pointer went down for a note, and where the note is to go while its text is asked for.
    note_press: Option<(u32, f32, f32, i64, i64)>,
    note_at: Option<(u32, f32, f32)>,
    /// Form fields, text boxes and pictures (4b).
    pub forms: FormState,
    /// Shapes, and the annotation picked held, nudged, drawn again (2026-10-17).
    pub(super) ae: AnnotEdit,
}

impl EditState {
    /// What the person has chosen about annotations (the name, the colour, the width of the line, the size of the text, the watermark) goes from tab
    /// to tab: taken from `from`, the state of the tab that was shown (its name is moved, not copied).
    pub(super) fn take_prefs(&mut self, from: &mut EditState) {
        std::mem::swap(&mut self.author, &mut from.author);
        (self.color, self.width, self.ae.fill, self.forms.text_size, self.forms.wm) = (from.color, from.width, from.ae.fill, from.forms.text_size, from.forms.wm);
    }

    /// Something this tab has asked of the engine is not answered yet, or a save or an export is going: the tab is not to be left.
    pub(super) fn busy(&self) -> bool {
        self.saving.is_some() || self.exporting.is_some() || !self.asked.is_empty()
    }

    pub(super) fn new(author: String) -> EditState {
        EditState {
            tool: Tool::Select,
            color: None,
            width: DEFAULT_WIDTH,
            author,
            ink: None,
            annots: IdMap::new(),
            annots_asked: IdMap::new(),
            picked: None,
            asked: IdMap::new(),
            can_undo: false,
            can_redo: false,
            dirty: false,
            rewrote: false,
            rewrote_listed: false,
            saving: None,
            export_asked: None,
            exporting: None,
            export_excel: false,
            after: None,
            note_press: None,
            note_at: None,
            forms: FormState::new(),
            ae: AnnotEdit::new(),
        }
    }

    /// Forget everything about the document that is gone (the author and the tool stay).
    pub(super) fn reset(&mut self) {
        let (author, color, width, fill) = (std::mem::take(&mut self.author), self.color, self.width, self.ae.fill);
        *self = EditState::new(author);
        self.color = color;
        self.width = width;
        self.ae.fill = fill;
    }

    /// The window lost the pointer while it was down: a stroke or a note that was begun is given up.
    pub(super) fn give_up_stroke(&mut self) {
        self.ink = None;
        self.note_press = None;
        self.ae.give_up();
        self.forms.give_up_drag();
    }

    /// The pages are not the pages they were: everything kept by page goes. Gives the requests to take back; a box that is open for typing
    /// in a field is closed (`actions`).
    pub(super) fn forget_pages(&mut self, actions: &mut Vec<Action>) -> Vec<u64> {
        self.annots.clear();
        self.picked = None;
        self.ink = None;
        self.ae.forget_pages();
        let mut ids: Vec<u64> = self.annots_asked.drain().map(|(i, _)| i).collect();
        ids.extend(self.forms.forget_pages(actions));
        ids
    }

    pub(super) fn drop_page(&mut self, page: u32) {
        self.annots.remove(&page);
        self.forms.drop_page(page);
        if self.picked.is_some_and(|(p, _)| p == page) {
            self.picked = None;
        }
    }

    pub(super) fn picked_info(&self) -> Option<&AnnotInfo> {
        let (page, index) = self.picked?;
        self.annots.get(&page)?.get(index)
    }

    /// The colour a new annotation gets: the one chosen, else yellow for a highlight or a note and red for the rest.
    pub(super) fn current_color(&self) -> usize {
        self.color.unwrap_or(match self.tool {
            Tool::Markup(MarkupKind::Highlight) | Tool::Note | Tool::Select => 0,
            Tool::TextBox | Tool::Stamp | Tool::EditText => 5,
            _ => 4,
        })
    }
}

impl App {
    pub(super) fn rgb(&self) -> [u8; 3] {
        COLORS.get(self.edit.current_color()).map_or([255, 235, 0], |c| c.1)
    }

    // --- tools -------------------------------------------------------------------------------------------------

    pub(super) fn set_tool(&mut self, tool: Tool) -> Vec<Action> {
        if self.doc.is_none() {
            return Vec::new();
        }
        if tool != Tool::TextBox {
            self.edit.forms.content = tool == Tool::EditText;
        }
        if tool == Tool::Crop && !self.can_crop() {
            return self.say(t(self.lang, Msg::NotePagesDenied).to_string());
        }
        if tool != Tool::Select && tool != Tool::Crop && !self.can_make() {
            return self.say(t(self.lang, if self.edit.forms.content { Msg::NoteModifyDenied } else { Msg::NoteAnnotDenied }).to_string());
        }
        self.edit.ink = None;
        self.edit.ae.give_up();
        self.edit.picked = None;
        self.edit.forms.give_up();
        self.menus_dirty = true;
        // Text that is already selected is marked at once, and the pointer goes back to what it was.
        if let Tool::Markup(kind) = tool
            && self.selection.is_some()
        {
            self.edit.tool = Tool::Select;
            return self.apply_markup(kind);
        }
        self.edit.tool = tool;
        // The hint of the tool stays over the pages for as long as the tool is in use (`sticky.rs`).
        if let Some(m) = self.mode_hint() {
            self.note = Some(t(self.lang, m).to_string());
        }
        let mut actions = self.sync_sticky();
        actions.push(Action::Invalidate);
        actions
    }

    pub(super) fn set_color(&mut self, index: usize) -> Vec<Action> {
        // With an annotation picked, the colour is its; with none, the next ones'.
        if index < COLORS.len() && self.has_picked() && let Some(actions) = self.restyle_picked(Some(index), None, None) {
            return actions;
        }
        if index < COLORS.len() {
            self.edit.color = Some(index);
            self.menus_dirty = true;
        }
        Vec::new()
    }

    pub(super) fn set_width(&mut self, index: usize) -> Vec<Action> {
        if index < WIDTHS.len() && self.has_picked() && let Some(actions) = self.restyle_picked(None, Some(index), None) {
            return actions;
        }
        if index < WIDTHS.len() {
            self.edit.width = index;
            self.menus_dirty = true;
        }
        Vec::new()
    }

    pub(super) fn ask_author(&mut self) -> Vec<Action> {
        vec![Action::AskText {
            tag: TAG_AUTHOR,
            title: t(self.lang, Msg::AuthorTitle).to_string(),
            prompt: format!("{} ({})", t(self.lang, Msg::AuthorPrompt), self.edit.author),
            secret: false,
            initial: String::new(),
            ok: t(self.lang, Msg::Ok).to_string(),
            cancel: t(self.lang, Msg::Cancel).to_string(),
        }]
    }

    pub(super) fn author_answered(&mut self, text: Option<String>) -> Vec<Action> {
        if let Some(name) = text.and_then(|t| crate::settings::clean(&t)) {
            crate::settings::save(&name);
            self.edit.author = name;
        }
        Vec::new()
    }

    /// Esc: back to selecting, and no stroke or annotation picked.
    pub(super) fn escape_tool(&mut self) -> Option<Vec<Action>> {
        if self.edit.tool == Tool::Select && self.edit.ink.is_none() && self.edit.picked.is_none() && !self.edit.forms.has_stamp() && !self.edit.ae.holds() {
            return None;
        }
        self.edit.tool = Tool::Select;
        self.edit.ink = None;
        self.edit.ae.give_up();
        self.edit.picked = None;
        self.edit.forms.give_up();
        self.menus_dirty = true;
        Some(vec![Action::Invalidate])
    }

    // --- the pointer -------------------------------------------------------------------------------------------

    /// The pointer went down with a tool that takes it: a stroke is started, or the place of a note noted. `None`: the tool leaves
    /// it to selecting.
    pub(super) fn edit_mouse_down(&mut self, x: i64, y: i64, clicks: u8) -> Option<Vec<Action>> {
        if let Some(actions) = self.forms_mouse_down(x, y) {
            return Some(actions);
        }
        match self.edit.tool {
            Tool::Select => self.annot_mouse_down(x, y, clicks),
            Tool::Shape(_) | Tool::Crop => self.shape_mouse_down(x, y),
            Tool::Ink => {
                let (page, ..) = self.page_at_point(x, y)?;
                let (xp, yp) = self.page_point(page as u32, x, y)?;
                self.edit.ink = Some(InkDraft { page: page as u32, points: vec![[xp, yp]] });
                self.selection = None;
                Some(vec![Action::Invalidate])
            }
            Tool::Note => {
                let (page, ..) = self.page_at_point(x, y)?;
                let (xp, yp) = self.page_point(page as u32, x, y)?;
                self.edit.note_press = Some((page as u32, xp, yp, x, y));
                self.selection = None;
                Some(Vec::new())
            }
            _ => None,
        }
    }

    /// The pointer moved with the button down: `true` if a stroke took it.
    pub(super) fn edit_mouse_move(&mut self, x: i64, y: i64, shift: bool) -> Option<Vec<Action>> {
        if let Some(actions) = self.forms_mouse_move(x, y) {
            return Some(actions);
        }
        if let Some(actions) = self.annot_mouse_move(x, y, shift) {
            return Some(actions);
        }
        let page = self.edit.ink.as_ref()?.page;
        let (xp, yp) = self.page_point(page, x, y)?;
        let draft = self.edit.ink.as_mut()?;
        let far = draft.points.last().is_none_or(|p| (p[0] - xp).abs() >= 0.4 || (p[1] - yp).abs() >= 0.4);
        if far && draft.points.len() < MAX_DRAFT_POINTS {
            draft.points.push([xp, yp]);
            return Some(vec![Action::Invalidate]);
        }
        Some(Vec::new())
    }

    /// The button came up: a stroke is handed to the engine, or the text of a note asked for. `true` if a tool took it.
    pub(super) fn edit_mouse_up(&mut self, x: i64, y: i64) -> Option<Vec<Action>> {
        if let Some(actions) = self.forms_mouse_up(x, y) {
            return Some(actions);
        }
        if let Some(actions) = self.annot_mouse_up(x, y) {
            return Some(actions);
        }
        if let Some(draft) = self.edit.ink.take() {
            if !self.rights.annotate {
                return Some(self.say(t(self.lang, Msg::NoteAnnotDenied).to_string()));
            }
            if let Some(wait) = self.edit_wait() {
                return Some(wait);
            }
            let id = self.new_id();
            let width = WIDTHS.get(self.edit.width).map_or(2.0, |w| w.1);
            self.engine.add_ink(self.doc_id, id, draft.page, vec![draft.points], self.rgb(), width, &self.edit.author);
            self.edit.asked.insert(id, Asked::Add);
            return Some(vec![Action::Invalidate]);
        }
        let (page, xp, yp, sx, sy) = self.edit.note_press.take()?;
        let moved = (x - sx).abs().max((y - sy).abs()) >= self.px(4);
        if moved {
            return Some(Vec::new());
        }
        self.edit.note_at = Some((page, xp, yp));
        Some(vec![Action::AskText {
            tag: TAG_NOTE,
            title: t(self.lang, Msg::NoteTitle).to_string(),
            prompt: t(self.lang, Msg::NotePrompt).to_string(),
            secret: false,
            initial: String::new(),
            ok: t(self.lang, Msg::Ok).to_string(),
            cancel: t(self.lang, Msg::Cancel).to_string(),
        }])
    }

    /// The text of a note was typed: the note is made where the pointer went down.
    pub(super) fn note_answered(&mut self, text: Option<String>) -> Vec<Action> {
        let Some((page, x, y)) = self.edit.note_at.take() else { return Vec::new() };
        let Some(text) = text.filter(|t| !t.trim().is_empty()) else { return Vec::new() };
        if let Some(wait) = self.edit_wait() {
            return wait;
        }
        let id = self.new_id();
        self.engine.add_note(self.doc_id, id, page, [x, y], &text, self.rgb(), &self.edit.author);
        self.edit.asked.insert(id, Asked::Add);
        Vec::new()
    }

    /// The menu on the page: mark the selected text as highlighted.
    pub(super) fn highlight_selection(&mut self) -> Vec<Action> {
        self.apply_markup(MarkupKind::Highlight)
    }

    /// The menu on the page: a note where the menu was opened (the text is asked for, as when a note is put with the tool).
    pub(super) fn note_here(&mut self) -> Vec<Action> {
        let Some(at) = self.ctx_at.take() else { return Vec::new() };
        if !self.rights.annotate {
            return self.say(t(self.lang, Msg::NoteAnnotDenied).to_string());
        }
        self.edit.note_at = Some(at);
        vec![Action::AskText {
            tag: TAG_NOTE,
            title: t(self.lang, Msg::NoteTitle).to_string(),
            prompt: t(self.lang, Msg::NotePrompt).to_string(),
            secret: false,
            initial: String::new(),
            ok: t(self.lang, Msg::Ok).to_string(),
            cancel: t(self.lang, Msg::Cancel).to_string(),
        }]
    }

    /// A drag with a marking tool is over: mark the text that was dragged over.
    pub(super) fn drag_finished(&mut self) -> Vec<Action> {
        match self.edit.tool {
            Tool::Markup(kind) if self.selection.is_some() => self.apply_markup(kind),
            _ => Vec::new(),
        }
    }

    /// The point of `page` (points as the engine tells them) under a place of the window, held to the page.
    pub(super) fn page_point(&self, page: u32, x: i64, y: i64) -> Option<(f32, f32)> {
        let l = &self.doc.as_ref()?.layout;
        let map = l.page_map(page as usize)?;
        let (px, py) = ((x - self.view_x - l.left(page as usize, self.view_w, self.scroll_x)) as f64, (y + self.scroll_y - l.top(page as usize)) as f64);
        let (xp, yp) = map.to_pt(px, py);
        let (w, h) = l.page_size_pt(page as usize)?;
        Some((xp.clamp(0.0, w) as f32, yp.clamp(0.0, h) as f32))
    }

    /// Mark the selected text, a markup for each page it covers, and let go of the selection.
    fn apply_markup(&mut self, kind: MarkupKind) -> Vec<Action> {
        let Some(sel) = self.selection else { return Vec::new() };
        if !self.rights.annotate {
            return self.say(t(self.lang, Msg::NoteAnnotDenied).to_string());
        }
        if let Some(wait) = self.edit_wait() {
            return wait;
        }
        let (first, last) = sel.ordered();
        let color = self.rgb();
        let mut sent = false;
        for page in first.page..=last.page {
            let Some(b) = self.boxes.get(&page) else { continue };
            let Some((a, z)) = sel.range_in(page, b.boxes.len()) else { continue };
            let rects = line_rects(b.boxes.get(a..z).unwrap_or(&[]));
            if rects.is_empty() {
                continue;
            }
            let id = self.new_id();
            self.engine.add_markup(self.doc_id, id, page, kind, rects, color, &self.edit.author);
            self.edit.asked.insert(id, Asked::Add);
            sent = true;
        }
        self.selection = None;
        let mut actions = vec![Action::Invalidate];
        if !sent {
            // Nothing in what was selected to mark: the marking tool is in use now (what its hint says), and the hint stays until Esc ends it.
            if self.edit.tool == Tool::Select {
                self.edit.tool = Tool::Markup(kind);
                self.menus_dirty = true;
            }
            self.note = Some(t(self.lang, Msg::NoteMarkHint).to_string());
            actions.extend(self.sync_sticky());
        }
        actions
    }

    // --- annotations of the pages --------------------------------------------------------------------------------

    /// Ask the engine for the annotations of the pages in view.
    pub(super) fn ask_for_annots(&mut self) {
        if self.bench.is_some() {
            return;
        }
        let Some((first, last)) = self.visible() else { return };
        let last = last.min(first.saturating_add(8));
        let stale: Vec<u64> = self.edit.annots_asked.iter().filter(|(_, p)| !(first..=last).contains(p)).map(|(id, _)| *id).collect();
        for id in stale {
            self.engine.cancel(id);
            self.edit.annots_asked.remove(&id);
        }
        for page in first..=last {
            if self.edit.annots.contains_key(&page) || self.edit.annots_asked.values().any(|&p| p == page) {
                continue;
            }
            let id = self.new_id();
            self.engine.annotations(self.doc_id, id, page);
            self.edit.annots_asked.insert(id, page);
        }
        self.edit.annots.retain(|&p, _| p.abs_diff(first) <= 12 && p.abs_diff(last) <= 12);
        self.ask_for_fields();
    }

    pub(super) fn on_annotations(&mut self, id: u64, page: u32, items: Vec<AnnotInfo>) -> Vec<Action> {
        if self.edit.annots_asked.remove(&id).is_some() {
            self.edit.annots.insert(page, items);
            self.resolve_want_pick(page);
        }
        vec![Action::Invalidate]
    }

    /// Pick the annotation of `page` at the place (`x`, `y`) in points, if there is one (the last of them is on top).
    pub(super) fn pick_annotation(&mut self, page: u32, x: f64, y: f64) -> Vec<Action> {
        let hit = self.annotation_at(page, x as f32, y as f32);
        let before = self.edit.picked;
        self.edit.picked = hit.map(|i| (page, i));
        let mut actions = self.describe_picked();
        if before != self.edit.picked {
            self.menus_dirty = true;
            actions.push(Action::Invalidate);
        }
        actions
    }

    /// Is there an annotation under this place (so that the pointer can say so)?
    pub(super) fn annotation_cursor(&self) -> Option<Cursor> {
        match self.edit.tool {
            Tool::Ink | Tool::Note | Tool::TextBox | Tool::Shape(_) | Tool::Crop => Some(Cursor::Cross),
            Tool::Markup(_) | Tool::EditText => Some(Cursor::Text),
            Tool::Select | Tool::Stamp => None,
        }
    }

    pub(super) fn has_picked(&self) -> bool {
        self.edit.picked_info().is_some()
    }

    pub(super) fn remove_picked(&mut self) -> Vec<Action> {
        let Some(a) = self.edit.picked_info().cloned() else { return Vec::new() };
        let Some((page, _)) = self.edit.picked else { return Vec::new() };
        if !self.rights.annotate {
            return self.say(t(self.lang, Msg::NoteAnnotDenied).to_string());
        }
        if a.locked() {
            return self.say(t(self.lang, Msg::NoteLocked).to_string());
        }
        if let Some(wait) = self.edit_wait() {
            return wait;
        }
        let id = self.new_id();
        self.engine.delete_annotation(self.doc_id, id, page, a.index, a.num);
        self.edit.asked.insert(id, Asked::Remove);
        Vec::new()
    }

    // --- undo, redo and the engine's answers ---------------------------------------------------------------------

    /// An undo or a redo while pages are being written as pictures or looked at for their margins would change the pages under the work: said, not done.
    fn history_not_now(&mut self) -> Option<Vec<Action>> {
        (self.px.busy() || self.cr.busy()).then(|| self.say(t(self.lang, Msg::NoteBusy).to_string()))
    }

    pub(super) fn undo(&mut self) -> Vec<Action> {
        if self.doc.is_none() {
            return Vec::new();
        }
        if let Some(said) = self.history_not_now() {
            return said;
        }
        let id = self.new_id();
        self.engine.undo(self.doc_id, id);
        self.edit.asked.insert(id, Asked::Undo);
        Vec::new()
    }

    pub(super) fn redo(&mut self) -> Vec<Action> {
        if self.doc.is_none() {
            return Vec::new();
        }
        if let Some(said) = self.history_not_now() {
            return said;
        }
        let id = self.new_id();
        self.engine.redo(self.doc_id, id);
        self.edit.asked.insert(id, Asked::Redo);
        Vec::new()
    }

    pub(super) fn on_edited(&mut self, mut r: EditResult) -> Vec<Action> {
        let Some(what) = self.edit.asked.remove(&r.id) else { return Vec::new() };
        self.edit.can_undo = r.can_undo;
        self.edit.can_redo = r.can_redo;
        self.edit.dirty = r.dirty;
        // A change that was not made leaves nothing to pick again.
        if what == Asked::Change && r.status != EditStatus::Done {
            self.edit.ae.want_pick = None;
        }
        self.menus_dirty = true;
        let lang = self.lang;
        let mut actions = vec![Action::SetTitle(self.title()), Action::Invalidate];
        match r.status {
            EditStatus::Done => {
                // The list of pages changed (pages turned, taken out, moved, put in; or the undo of one): everything is by the new pages.
                let listed = r.pages.take();
                if let Some(infos) = listed.clone() {
                    actions.extend(self.pages_changed(infos, r.id));
                }
                // What was drawn of the page is out of date (of every page, for a watermark); everything else is as it was.
                let changed = if listed.is_some() || r.page == NO_PAGE {
                    0..0
                } else if r.page == ALL_PAGES {
                    0..self.page_count() as u32
                } else {
                    r.page..r.page + 1
                };
                for page in changed {
                    self.cache.remove_page(page);
                    self.forget_thumb(page);
                    self.failed.retain(|k| k.page != page);
                    self.notices.remove(&page);
                    self.links.remove(&page);
                    // (The boxes of the characters: a page that was recognized has some now.)
                    self.boxes.remove(&page);
                    for id in self.links_asked.iter().filter(|(_, p)| **p == page).map(|(id, _)| *id).collect::<Vec<_>>() {
                        self.engine.cancel(id);
                        self.links_asked.remove(&id);
                    }
                    self.edit.drop_page(page);
                }
                // A change to the bookmarks, or the undo or redo of one: the engine is asked for them again. So are they when the file was written
                // afresh by this change (a damaged file's first change): its objects are numbered anew.
                if r.rewrote && !std::mem::replace(&mut self.edit.rewrote_listed, true) && !self.outline.is_empty() && r.page != NO_PAGE {
                    self.bm.reload = true;
                    actions.extend(self.ask_for_outline());
                }
                if r.page == NO_PAGE {
                    self.bm.reload = true;
                    actions.extend(self.ask_for_outline());
                }
                self.schedule();
                self.ask_for_links();
                self.ask_for_annots();
                self.annots_stale();
                // The engine tells once, with a message, that the file is signed and the signature will show it as changed.
                let mut extra = String::new();
                let note = if what == Asked::Mark {
                    // (The engine's message is what it has to say, and the number of places last.)
                    let (said, n) = r.message.rsplit_once(' ').unwrap_or(("", &r.message));
                    actions.extend(self.say(t(lang, Msg::NoteMarked).replace("{n}", n)));
                    if !said.is_empty() {
                        actions.push(Action::Message { title: t(lang, Msg::RdFindTitle).to_string(), text: said.to_string() });
                    }
                    None
                } else if what == Asked::Line {
                    extra = r.message.clone();
                    Some(Msg::NoteLineChanged)
                } else if what == Asked::Ocr {
                    None
                } else if let Some(m) = what.pages_note() {
                    extra = r.message.clone();
                    Some(m)
                } else if what == Asked::Meta {
                    let xmp = if r.message.is_empty() { "" } else { t(lang, Msg::NotePropsXmp) };
                    actions.extend(self.say(format!("{} {xmp}", t(lang, Msg::NoteProps)).trim_end().to_string()));
                    None
                } else if !r.message.is_empty() {
                    Some(Msg::NoteSigned)
                } else if r.rewrote && !self.edit.rewrote {
                    self.edit.rewrote = true;
                    Some(Msg::NoteRewrite)
                } else {
                    match what {
                        Asked::Add => Some(Msg::NoteAdded),
                        Asked::Change => Some(Msg::NoteChanged),
                        Asked::Fill => Some(Msg::NoteFilled),
                        Asked::Remove => Some(Msg::NoteRemoved),
                        Asked::Pagenum => Some(Msg::NotePagenum),
                        Asked::Unpagenum => Some(Msg::NoteUnpagenum),
                        Asked::Undo | Asked::Redo | Asked::Line | Asked::Mark | Asked::Ocr | Asked::Rotate | Asked::Delete | Asked::Move | Asked::Insert | Asked::Bookmarks | Asked::Crop | Asked::Uncrop | Asked::Meta => None,
                    }
                };
                if let Some(m) = note {
                    actions.extend(self.say(format!("{} {extra}", t(lang, m)).trim_end().to_string()));
                }
            }
            EditStatus::Denied if what == Asked::Ocr => {}
            EditStatus::Failed if what == Asked::Ocr => {}
            EditStatus::Denied => {
                let m = if what.pages_note().is_some() {
                    Msg::NotePagesDenied
                } else if what == Asked::Bookmarks {
                    Msg::NoteBmDenied
                } else if what == Asked::Meta {
                    Msg::PropNoEdit
                } else if what == Asked::Fill {
                    Msg::NoteFillDenied
                } else {
                    Msg::NoteAnnotDenied
                };
                actions.extend(self.say(t(lang, m).to_string()));
            }
            // The file the pages are to come from needs a password (or the one given was wrong).
            EditStatus::NeedsPassword | EditStatus::WrongPassword if what == Asked::Insert => {
                actions.extend(self.ask_source_password(r.status == EditStatus::WrongPassword));
            }
            EditStatus::NeedsPassword | EditStatus::WrongPassword => {}
            EditStatus::TooBig => {
                let mb = (qingpdf_core::view::MAX_INSERT_FILE_BYTES >> 20).to_string();
                actions.extend(self.say(t(lang, Msg::NoteInsertTooBig).replace("{n}", &mb)));
            }
            EditStatus::Failed => actions.extend(self.fail(format!("{} {}", t(lang, Msg::NoteEditFailed), r.message))),
            EditStatus::Nothing => {
                let m = if what == Asked::Redo { Msg::NoteNothingRedo } else { Msg::NoteNothingUndo };
                actions.extend(self.say(t(lang, m).to_string()));
            }
            EditStatus::Busy => actions.extend(self.say(t(lang, Msg::NoteBusy).to_string())),
        }
        if what == Asked::Ocr {
            actions.extend(self.ocr_layer_answered(r.id, r.status, &r.message));
        }
        // A change to the bookmarks that was not made: the tree is as the list has it.
        if what == Asked::Bookmarks && r.status != EditStatus::Done {
            actions.extend(self.bm_failed());
        }
        actions
    }

    // --- saving --------------------------------------------------------------------------------------------------

    /// Save to the file the document was opened from.
    pub(super) fn save(&mut self) -> Vec<Action> {
        match self.path.clone() {
            Some(path) if self.doc.is_some() => self.start_save(path, false),
            _ => Vec::new(),
        }
    }

    pub(super) fn save_as(&mut self) -> Vec<Action> {
        let Some(path) = self.path.clone().filter(|_| self.doc.is_some()) else { return Vec::new() };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let filters = vec![(t(self.lang, Msg::FilterPdf).to_string(), "*.pdf".to_string())];
        vec![Action::PickSavePath { title: t(self.lang, Msg::SaveTitle).to_string(), filters, name }]
    }

    /// File > Export to Word (or Excel): ask where to write the .docx (.xlsx).
    pub(super) fn export_file(&mut self, excel: bool) -> Vec<Action> {
        let Some(path) = self.path.clone().filter(|_| self.doc.is_some() && self.edit.exporting.is_none()) else { return Vec::new() };
        let ext = if excel { "xlsx" } else { "docx" };
        let name = path.with_extension(ext).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let filters = vec![(t(self.lang, if excel { Msg::FilterXlsx } else { Msg::FilterDocx }).to_string(), format!("*.{ext}"))];
        self.edit.export_asked = Some(excel);
        vec![Action::PickSavePath { title: t(self.lang, if excel { Msg::ExportExcelTitle } else { Msg::ExportTitle }).to_string(), filters, name }]
    }

    fn export_picked(&mut self, path: Option<PathBuf>, excel: bool) -> Vec<Action> {
        let Some(p) = path.filter(|_| self.doc.is_some()) else { return Vec::new() };
        let id = self.new_id();
        let to = std::path::absolute(&p).unwrap_or(p).to_string_lossy().into_owned();
        if excel {
            self.engine.export_xlsx(self.doc_id, id, &to);
        } else {
            self.engine.export_docx(self.doc_id, id, &to);
        }
        self.edit.exporting = Some(id);
        self.edit.export_excel = excel;
        self.say_job(sticky::Job::Export, t(self.lang, Msg::NoteExporting).to_string())
    }

    /// Esc while the export is going: the engine is told to stop at the next page (it answers nothing), the temporary file goes,
    /// and the old file is as it was. `None`: no export is going.
    pub(super) fn cancel_export(&mut self) -> Option<Vec<Action>> {
        let id = self.edit.exporting.take()?;
        self.engine.cancel(id);
        Some(self.say(t(self.lang, Msg::NoteExportCancelled).to_string()))
    }

    pub(super) fn on_exported(&mut self, id: u64, status: SaveStatus, message: &str, tables: u32) -> Vec<Action> {
        if self.edit.exporting != Some(id) {
            return Vec::new();
        }
        self.edit.exporting = None;
        match status {
            SaveStatus::Done if !self.edit.export_excel => self.say(t(self.lang, Msg::NoteExported).to_string()),
            SaveStatus::Done => self.say(t(self.lang, if tables == 0 { Msg::NoteExportedNoTables } else { Msg::NoteExportedExcel }).to_string()),
            SaveStatus::Failed => vec![Action::Message { title: t(self.lang, Msg::Error).to_string(), text: format!("{}
{message}", t(self.lang, Msg::NoteExportFailed)) }],
        }
    }

    pub(super) fn save_picked(&mut self, path: Option<PathBuf>) -> Vec<Action> {
        if let Some(actions) = self.protect_picked(path.clone()) {
            return actions;
        }
        if let Some(actions) = self.compress_picked(path.clone()) {
            return actions;
        }
        if let Some(actions) = self.redact_picked(path.clone()) {
            return actions;
        }
        if let Some(actions) = self.extract_picked(path.clone()) {
            return actions;
        }
        if let Some(excel) = self.edit.export_asked.take() {
            return self.export_picked(path, excel);
        }
        match path {
            Some(p) if self.doc.is_some() => self.start_save(std::path::absolute(&p).unwrap_or(p), true),
            _ => Vec::new(),
        }
    }

    fn start_save(&mut self, path: PathBuf, whole: bool) -> Vec<Action> {
        if self.edit.saving.is_some() {
            return Vec::new();
        }
        let id = self.new_id();
        if whole {
            self.engine.save_as(self.doc_id, id, &path.to_string_lossy());
        } else {
            self.engine.save(self.doc_id, id, &path.to_string_lossy());
        }
        self.edit.saving = Some((id, path, whole));
        Vec::new()
    }

    pub(super) fn on_saved(&mut self, id: u64, status: SaveStatus, message: &str) -> Vec<Action> {
        let Some((_, path, whole)) = self.edit.saving.take().filter(|(i, ..)| *i == id) else { return Vec::new() };
        let lang = self.lang;
        match status {
            SaveStatus::Done => {
                self.edit.dirty = false;
                self.menus_dirty = true;
                // (A file written again as a whole has no history to go back through.)
                (self.edit.can_undo, self.edit.can_redo) = (self.edit.can_undo && !whole, self.edit.can_redo && !whole);
                // The file is what was just written: later it is told from a file that was changed by another program by this (and where the
                // reader was in it is noted with it).
                self.stamp = crate::places::stamp_of(&path);
                self.path = Some(path);
                self.remember_position();
                let mut actions = vec![Action::SetTitle(self.title()), Action::Invalidate];
                // A file written again as a whole has its objects numbered anew: the bookmarks are asked for again, with the numbers the file has now.
                if whole && !self.outline.is_empty() {
                    self.bm.reload = true;
                    actions.extend(self.ask_for_outline());
                }
                match self.edit.after.take() {
                    Some(After::Quit) => actions.extend(self.quit()),
                    Some(After::CloseTab) => actions.extend(self.close_current()),
                    Some(After::Compress) => actions.push(Action::AskForm(self.compress_form())),
                    None => actions.extend(self.say(t(lang, Msg::NoteSaved).to_string())),
                }
                actions
            }
            SaveStatus::Failed => {
                self.edit.after = None;
                vec![Action::Message { title: t(lang, Msg::Error).to_string(), text: format!("{}\n{message}", t(lang, Msg::NoteSaveFailed)) }]
            }
        }
    }

    // --- unsaved changes -----------------------------------------------------------------------------------------

    /// Ask what to do about the changes before `then` is done; `false` when there is nothing to ask (do it now).
    pub(super) fn ask_unsaved(&mut self, then: After) -> Option<Vec<Action>> {
        if !self.edit.dirty || self.doc.is_none() {
            return None;
        }
        self.edit.after = Some(then);
        // (With several files open, which one is asked about is said.)
        let name = self.path.as_deref().map(crate::tabs::label).unwrap_or_default();
        Some(vec![Action::AskSave { tag: TAG_UNSAVED, title: t(self.lang, Msg::UnsavedTitle).to_string(), text: format!("{}

{name}", t(self.lang, Msg::UnsavedAsk)) }])
    }

    pub(super) fn unsaved_answered(&mut self, choice: SaveChoice) -> Vec<Action> {
        let Some(after) = self.edit.after.take() else { return Vec::new() };
        match choice {
            SaveChoice::Save => {
                self.edit.after = Some(after);
                let actions = self.save();
                if self.edit.saving.is_none() {
                    self.edit.after = None;
                }
                actions
            }
            // (The changes stay where they are until the tab or the window is gone: the person may still cancel at the next question, and
            // then the tab has its changes and its mark. For this round of questions the tab is not asked about again.)
            SaveChoice::Discard => match after {
                After::Quit => {
                    self.discarded.push(self.doc_id);
                    self.quit()
                }
                After::CloseTab => self.close_current(),
                After::Compress => Vec::new(),
            },
            SaveChoice::Cancel => {
                self.discarded.clear();
                Vec::new()
            }
        }
    }

    pub(super) fn exit_now(&mut self) -> Vec<Action> {
        // A print job that is going is stopped first (AbortDoc, and the printer is let go of).
        let mut actions = self.cancel_print_for_new_document();
        self.remember_position();
        self.flush_places();
        actions.push(Action::Quit);
        actions
    }

    // --- drawing over the pages ----------------------------------------------------------------------------------

    /// The stroke being drawn and the frame of the annotation picked, on a page whose top left corner is at (`left`, `top`).
    pub(super) fn draw_edit_marks(&self, p: &mut dyn Painter, page: usize, left: i64, top: i64) {
        self.draw_form_marks(p, page, left, top);
        let Some(doc) = self.doc.as_ref() else { return };
        let Some(map) = doc.layout.page_map(page) else { return };
        if let Some(draft) = self.edit.ink.as_ref().filter(|d| d.page as usize == page) {
            let width_pt = f64::from(WIDTHS.get(self.edit.width).map_or(2.0, |w| w.1));
            let side = (width_pt * doc.layout.scale()).round().max(1.0) as i64;
            let [r, g, b] = self.rgb();
            let color = (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
            let points: Vec<(f64, f64)> = draft.points.iter().map(|pt| map.to_px(f64::from(pt[0]), f64::from(pt[1]))).collect();
            self.draw_dots(p, &points, left, top, side, color);
        }
        self.draw_annot_marks(p, page, left, top);
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

    #[test]
    fn a_note_is_put_picked_taken_out_undone_and_saved_through_the_reader() {
        let corpus = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/corpus/public/xref-classic/hello_world.pdf");
        let dir = std::env::temp_dir().join(format!("qingpdf-view-edit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.pdf");
        std::fs::copy(corpus, &file).unwrap();
        let mut app = App::new(Lang::En, Arc::new(|| {}), Instant::now(), 1920 * 1080, "Tester".to_string());
        app.event(Event::Size { w: 900, h: 700 });
        app.open_path(&file, "");
        pump(&mut app, "the file to open", |a| a.has_document());
        pump(&mut app, "the annotations of the page", |a| !a.edit.annots_asked.is_empty() || a.edit.annots.contains_key(&0));
        pump(&mut app, "the annotation list", |a| a.edit.annots.contains_key(&0));
        // A note where the page is, with text.
        app.event(Event::Command(CMD_TOOL_NOTE));
        let at = (300, 200);
        app.event(Event::MouseDown { x: at.0, y: at.1, ctrl: false, shift: false, clicks: 1 });
        let ask = app.event(Event::MouseUp { x: at.0, y: at.1 });
        assert!(ask.iter().any(|a| matches!(a, Action::AskText { tag: TAG_NOTE, .. })), "no question about the text of the note");
        app.event(Event::Text { tag: TAG_NOTE, text: Some("a note".to_string()) });
        pump(&mut app, "the note", |a| a.edit.dirty && a.edit.annots.get(&0).is_some_and(|l| l.iter().any(|i| i.subtype == "Text")));
        // Back to selecting, click on the icon: it is picked.
        app.event(Event::Command(CMD_TOOL_SELECT));
        app.event(Event::MouseDown { x: at.0 + 6, y: at.1 + 6, ctrl: false, shift: false, clicks: 1 });
        app.event(Event::MouseUp { x: at.0 + 6, y: at.1 + 6 });
        assert!(app.has_picked(), "the note under the pointer was not picked");
        // Taken out, undone (it is back), taken out again, saved (the file is then written and the title is clean).
        app.event(Event::Command(CMD_REMOVE));
        pump(&mut app, "the removal", |a| a.edit.annots.get(&0).is_some_and(|l| l.iter().all(|i| i.subtype != "Text")) && a.edit.can_undo);
        app.event(Event::Command(CMD_UNDO));
        pump(&mut app, "the undo", |a| a.edit.annots.get(&0).is_some_and(|l| l.iter().any(|i| i.subtype == "Text")));
        app.event(Event::Command(CMD_SAVE));
        pump(&mut app, "the save", |a| !a.edit.dirty && a.edit.saving.is_none());
        let saved = std::fs::read(&file).unwrap();
        assert!(saved.starts_with(&std::fs::read(corpus).unwrap()) && saved.len() > std::fs::metadata(corpus).unwrap().len() as usize);
        assert!(!app.title().starts_with('*'));
        // Closing with nothing unsaved quits at once; with a change it asks first.
        let quit = app.event(Event::CloseRequested);
        assert!(quit.iter().any(|a| matches!(a, Action::Quit)));
        app.event(Event::Command(CMD_TOOL_NOTE));
        app.event(Event::MouseDown { x: 500, y: 300, ctrl: false, shift: false, clicks: 1 });
        app.event(Event::MouseUp { x: 500, y: 300 });
        app.event(Event::Text { tag: TAG_NOTE, text: Some("second".to_string()) });
        pump(&mut app, "the second note", |a| a.edit.dirty);
        let ask = app.event(Event::CloseRequested);
        assert!(ask.iter().any(|a| matches!(a, Action::AskSave { .. })) && !ask.iter().any(|a| matches!(a, Action::Quit)));
        let cancelled = app.event(Event::SaveChoice { tag: TAG_UNSAVED, choice: SaveChoice::Cancel });
        assert!(!cancelled.iter().any(|a| matches!(a, Action::Quit)));
        let discarded = app.event(Event::CloseRequested);
        assert!(discarded.iter().any(|a| matches!(a, Action::AskSave { .. })));
        let quit = app.event(Event::SaveChoice { tag: TAG_UNSAVED, choice: SaveChoice::Discard });
        assert!(quit.iter().any(|a| matches!(a, Action::Quit)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn export_to_word_asks_for_a_name_writes_the_file_and_says_so() {
        let corpus = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/corpus/public/xref-classic/hello_world.pdf");
        let dir = std::env::temp_dir().join(format!("qingpdf-view-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.pdf");
        std::fs::copy(corpus, &file).unwrap();
        let mut app = App::new(Lang::En, Arc::new(|| {}), Instant::now(), 1920 * 1080, "Tester".to_string());
        // Nothing open: nothing to ask.
        assert!(!app.event(Event::Command(CMD_EXPORT_DOCX)).iter().any(|a| matches!(a, Action::PickSavePath { .. })));
        app.event(Event::Size { w: 900, h: 700 });
        app.open_path(&file, "");
        pump(&mut app, "the file to open", |a| a.has_document());
        // The box asks for a .docx file, starting from the name of the file.
        let asked = app.event(Event::Command(CMD_EXPORT_DOCX));
        let Some(Action::PickSavePath { filters, name, .. }) = asked.iter().find(|a| matches!(a, Action::PickSavePath { .. })) else { panic!("no question: {asked:?}") };
        assert_eq!((name.as_str(), filters.first().map(|f| f.1.as_str())), ("a.docx", Some("*.docx")));
        // Cancelled: nothing is written, and the file name box was for this and not for a PDF save.
        app.event(Event::SavePicked(None));
        assert!(app.edit.exporting.is_none() && !dir.join("a.docx").exists());
        // Asked again and answered: the engine writes the file.
        app.event(Event::Command(CMD_EXPORT_DOCX));
        let target = dir.join("out.docx");
        let said = app.event(Event::SavePicked(Some(target.clone())));
        assert!(!said.iter().any(|a| matches!(a, Action::PickSavePath { .. })));
        pump(&mut app, "the export", |a| a.edit.exporting.is_none() && a.note.as_deref() == Some("Exported to Word."));
        assert!(std::fs::read(&target).unwrap().starts_with(b"PK\x03\x04"));
        // The PDF is untouched and is still not changed.
        assert!(!app.edit.dirty && !app.title().starts_with('*'));
        // A name that cannot be written says so in a message box.
        let bad = dir.join("no-such-folder").join("x.docx");
        app.event(Event::Command(CMD_EXPORT_DOCX));
        app.event(Event::SavePicked(Some(bad)));
        let mut shown = Vec::new();
        for _ in 0..1000 {
            shown.extend(app.event(Event::Wake));
            if shown.iter().any(|a| matches!(a, Action::Message { .. })) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(shown.iter().any(|a| matches!(a, Action::Message { text, .. } if text.contains("could not be exported"))), "{shown:?}");
        // Esc while it is going stops it: nothing is said but that it was stopped, and no message box comes after it.
        app.event(Event::Command(CMD_EXPORT_DOCX));
        let stopped = dir.join("stopped.docx");
        app.event(Event::SavePicked(Some(stopped.clone())));
        let said = app.event(Event::Key { vk: crate::ui::vk::ESCAPE, ctrl: false, shift: false });
        assert!(app.edit.exporting.is_none(), "{said:?}");
        let mut shown = Vec::new();
        for _ in 0..30 {
            shown.extend(app.event(Event::Wake));
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!shown.iter().any(|a| matches!(a, Action::Message { .. })), "{shown:?}");
        // (Either it was stopped in time, or it was done before the key: a whole file, never half of one.)
        if stopped.exists() {
            assert!(std::fs::read(&stopped).unwrap().starts_with(b"PK\x03\x04"));
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn export_to_excel_asks_for_an_xlsx_name_and_says_when_the_file_has_no_table() {
        let corpus = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/corpus/public/xref-classic/hello_world.pdf");
        let dir = std::env::temp_dir().join(format!("qingpdf-view-export-xlsx-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.pdf");
        std::fs::copy(corpus, &file).unwrap();
        let mut app = App::new(Lang::En, Arc::new(|| {}), Instant::now(), 1920 * 1080, "Tester".to_string());
        assert!(!app.event(Event::Command(CMD_EXPORT_XLSX)).iter().any(|a| matches!(a, Action::PickSavePath { .. })));
        app.event(Event::Size { w: 900, h: 700 });
        app.open_path(&file, "");
        pump(&mut app, "the file to open", |a| a.has_document());
        let asked = app.event(Event::Command(CMD_EXPORT_XLSX));
        let Some(Action::PickSavePath { filters, name, .. }) = asked.iter().find(|a| matches!(a, Action::PickSavePath { .. })) else { panic!("no question: {asked:?}") };
        assert_eq!((name.as_str(), filters.first().map(|f| f.1.as_str())), ("a.xlsx", Some("*.xlsx")));
        // Answered: the engine writes the workbook; the page has no table, and the note says what the sheet has.
        let target = dir.join("out.xlsx");
        app.event(Event::SavePicked(Some(target.clone())));
        pump(&mut app, "the export", |a| a.edit.exporting.is_none() && a.note.as_deref().is_some_and(|n| n.starts_with("Exported to Excel: no table")));
        let bytes = std::fs::read(&target).unwrap();
        assert!(bytes.starts_with(b"PK") && bytes.windows(15).any(|w| w == b"xl/workbook.xml"));
        // Esc while it is going stops it, as for Word.
        app.event(Event::Command(CMD_EXPORT_XLSX));
        app.event(Event::SavePicked(Some(dir.join("stopped.xlsx"))));
        app.event(Event::Key { vk: crate::ui::vk::ESCAPE, ctrl: false, shift: false });
        assert!(app.edit.exporting.is_none());
        // (The engine may still be at it for a moment; the folder goes when it is done.)
        for _ in 0..30 {
            app.event(Event::Wake);
            std::thread::sleep(Duration::from_millis(10));
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
