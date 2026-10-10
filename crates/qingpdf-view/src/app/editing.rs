//! Marking text, drawing, notes, taking annotations out, undo and redo, saving (4a). The reader only asks: the engine makes the
//! annotations, keeps the history and writes the file (`docs/view-api.md`); what is here is the tools, what is shown meanwhile
//! (a stroke being drawn, the annotation picked) and the questions about unsaved changes.

use std::path::PathBuf;

use qingpdf_core::view::{AnnotInfo, EditResult, EditStatus, MarkupKind, SaveStatus, line_rects};

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
/// The colour of the frame round the annotation picked.
const PICK_COLOR: u32 = 0x0066FF;
/// How far (in points) from an annotation a click still picks it.
const PICK_SLOP: f32 = 2.0;

/// What the pointer does on the pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tool {
    /// Select text, follow links, pick annotations.
    Select,
    /// Mark the text that is dragged over.
    Markup(MarkupKind),
    Ink,
    Note,
}

/// What is to be done once the changes are saved (or given up).
#[derive(Clone, Debug)]
pub(super) enum After {
    Quit,
    Open(PathBuf),
}

/// A stroke being drawn: the page and the points so far (points of the page as the engine tells them).
pub(super) struct InkDraft {
    page: u32,
    points: Vec<[f32; 2]>,
}

/// What the engine was asked and has not yet answered.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Asked {
    Add,
    Remove,
    Undo,
    Redo,
}

pub(super) struct EditState {
    pub tool: Tool,
    /// The colour the person chose (none yet: the tool's own).
    pub color: Option<usize>,
    pub width: usize,
    pub author: String,
    ink: Option<InkDraft>,
    /// The annotations of the pages near the window, and the engine's requests for them (request, page).
    annots: HashMap<u32, Vec<AnnotInfo>>,
    annots_asked: HashMap<u64, u32>,
    /// The annotation picked: its page and its place in that page's list.
    picked: Option<(u32, usize)>,
    asked: HashMap<u64, Asked>,
    pub can_undo: bool,
    pub can_redo: bool,
    pub dirty: bool,
    /// The structure of the file was damaged: it is written afresh, as a whole.
    rewrote: bool,
    /// The save that is going on (its request) and where it writes; what to do when it is done.
    saving: Option<(u64, PathBuf)>,
    after: Option<After>,
    /// The place where the pointer went down for a note, and where the note is to go while its text is asked for.
    note_press: Option<(u32, f32, f32, i64, i64)>,
    note_at: Option<(u32, f32, f32)>,
}

impl EditState {
    pub(super) fn new(author: String) -> EditState {
        EditState {
            tool: Tool::Select,
            color: None,
            width: DEFAULT_WIDTH,
            author,
            ink: None,
            annots: HashMap::new(),
            annots_asked: HashMap::new(),
            picked: None,
            asked: HashMap::new(),
            can_undo: false,
            can_redo: false,
            dirty: false,
            rewrote: false,
            saving: None,
            after: None,
            note_press: None,
            note_at: None,
        }
    }

    /// Forget everything about the document that is gone (the author and the tool stay).
    pub(super) fn reset(&mut self) {
        let (author, color, width) = (std::mem::take(&mut self.author), self.color, self.width);
        *self = EditState::new(author);
        self.color = color;
        self.width = width;
    }

    /// The window lost the pointer while it was down: a stroke or a note that was begun is given up.
    pub(super) fn give_up_stroke(&mut self) {
        self.ink = None;
        self.note_press = None;
    }

    pub(super) fn drop_page(&mut self, page: u32) {
        self.annots.remove(&page);
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
            _ => 4,
        })
    }
}

impl App {
    fn rgb(&self) -> [u8; 3] {
        COLORS.get(self.edit.current_color()).map_or([255, 235, 0], |c| c.1)
    }

    // --- tools -------------------------------------------------------------------------------------------------

    pub(super) fn set_tool(&mut self, tool: Tool) -> Vec<Action> {
        if self.doc.is_none() {
            return Vec::new();
        }
        if tool != Tool::Select && !self.rights.annotate {
            return self.say(t(self.lang, Msg::NoteAnnotDenied).to_string());
        }
        self.edit.ink = None;
        self.edit.picked = None;
        self.menus_dirty = true;
        // Text that is already selected is marked at once, and the pointer goes back to what it was.
        if let Tool::Markup(kind) = tool
            && self.selection.is_some()
        {
            self.edit.tool = Tool::Select;
            return self.apply_markup(kind);
        }
        self.edit.tool = tool;
        let hint = match tool {
            Tool::Select => None,
            Tool::Markup(_) => Some(Msg::NoteMarkHint),
            Tool::Ink => Some(Msg::NoteInkHint),
            Tool::Note => Some(Msg::NoteNoteHint),
        };
        match hint {
            Some(m) => self.say(t(self.lang, m).to_string()),
            None => vec![Action::Invalidate],
        }
    }

    pub(super) fn set_color(&mut self, index: usize) -> Vec<Action> {
        if index < COLORS.len() {
            self.edit.color = Some(index);
            self.menus_dirty = true;
        }
        Vec::new()
    }

    pub(super) fn set_width(&mut self, index: usize) -> Vec<Action> {
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
        if self.edit.tool == Tool::Select && self.edit.ink.is_none() && self.edit.picked.is_none() {
            return None;
        }
        self.edit.tool = Tool::Select;
        self.edit.ink = None;
        self.edit.picked = None;
        self.menus_dirty = true;
        Some(vec![Action::Invalidate])
    }

    // --- the pointer -------------------------------------------------------------------------------------------

    /// The pointer went down with a tool that takes it: a stroke is started, or the place of a note noted. `None`: the tool leaves
    /// it to selecting.
    pub(super) fn edit_mouse_down(&mut self, x: i64, y: i64) -> Option<Vec<Action>> {
        match self.edit.tool {
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
    pub(super) fn edit_mouse_move(&mut self, x: i64, y: i64) -> Option<Vec<Action>> {
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
        if let Some(draft) = self.edit.ink.take() {
            if !self.rights.annotate {
                return Some(self.say(t(self.lang, Msg::NoteAnnotDenied).to_string()));
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
            ok: t(self.lang, Msg::Ok).to_string(),
            cancel: t(self.lang, Msg::Cancel).to_string(),
        }])
    }

    /// The text of a note was typed: the note is made where the pointer went down.
    pub(super) fn note_answered(&mut self, text: Option<String>) -> Vec<Action> {
        let Some((page, x, y)) = self.edit.note_at.take() else { return Vec::new() };
        let Some(text) = text.filter(|t| !t.trim().is_empty()) else { return Vec::new() };
        let id = self.new_id();
        self.engine.add_note(self.doc_id, id, page, [x, y], &text, self.rgb(), &self.edit.author);
        self.edit.asked.insert(id, Asked::Add);
        Vec::new()
    }

    /// A drag with a marking tool is over: mark the text that was dragged over.
    pub(super) fn drag_finished(&mut self) -> Vec<Action> {
        match self.edit.tool {
            Tool::Markup(kind) if self.selection.is_some() => self.apply_markup(kind),
            _ => Vec::new(),
        }
    }

    /// The point of `page` (points as the engine tells them) under a place of the window, held to the page.
    fn page_point(&self, page: u32, x: i64, y: i64) -> Option<(f32, f32)> {
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
            actions.extend(self.say(t(self.lang, Msg::NoteMarkHint).to_string()));
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
    }

    pub(super) fn on_annotations(&mut self, id: u64, page: u32, items: Vec<AnnotInfo>) -> Vec<Action> {
        if self.edit.annots_asked.remove(&id).is_some() {
            self.edit.annots.insert(page, items);
        }
        Vec::new()
    }

    /// Pick the annotation of `page` at the place (`x`, `y`) in points, if there is one (the last of them is on top).
    pub(super) fn pick_annotation(&mut self, page: u32, x: f64, y: f64) -> Vec<Action> {
        let (x, y) = (x as f32, y as f32);
        let hit = self.edit.annots.get(&page).and_then(|list| {
            list.iter().enumerate().rev().find(|(_, a)| {
                let rects: &[[f32; 4]] = if a.parts.is_empty() { std::slice::from_ref(&a.rect) } else { &a.parts };
                rects.iter().any(|r| r[0] - PICK_SLOP <= x && x <= r[2] + PICK_SLOP && r[1] - PICK_SLOP <= y && y <= r[3] + PICK_SLOP)
            })
        });
        let before = self.edit.picked;
        self.edit.picked = hit.map(|(i, _)| (page, i));
        let mut actions = Vec::new();
        if let Some(a) = self.edit.picked_info() {
            let mut text = a.subtype.clone();
            if !a.author.is_empty() {
                text.push_str(" - ");
                text.push_str(&a.author);
            }
            if !a.contents.is_empty() {
                text.push_str(": ");
                text.push_str(&a.contents);
            }
            actions.extend(self.say(text));
        }
        if before != self.edit.picked {
            self.menus_dirty = true;
            actions.push(Action::Invalidate);
        }
        actions
    }

    /// Is there an annotation under this place (so that the pointer can say so)?
    pub(super) fn annotation_cursor(&self) -> Option<Cursor> {
        match self.edit.tool {
            Tool::Ink | Tool::Note => Some(Cursor::Cross),
            Tool::Markup(_) => Some(Cursor::Text),
            Tool::Select => None,
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
        let id = self.new_id();
        self.engine.delete_annotation(self.doc_id, id, page, a.index, a.num);
        self.edit.asked.insert(id, Asked::Remove);
        Vec::new()
    }

    // --- undo, redo and the engine's answers ---------------------------------------------------------------------

    pub(super) fn undo(&mut self) -> Vec<Action> {
        if self.doc.is_none() {
            return Vec::new();
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
        let id = self.new_id();
        self.engine.redo(self.doc_id, id);
        self.edit.asked.insert(id, Asked::Redo);
        Vec::new()
    }

    pub(super) fn on_edited(&mut self, r: EditResult) -> Vec<Action> {
        let Some(what) = self.edit.asked.remove(&r.id) else { return Vec::new() };
        self.edit.can_undo = r.can_undo;
        self.edit.can_redo = r.can_redo;
        self.edit.dirty = r.dirty;
        self.menus_dirty = true;
        let lang = self.lang;
        let mut actions = vec![Action::SetTitle(self.title()), Action::Invalidate];
        match r.status {
            EditStatus::Done => {
                // What was drawn of the page is out of date; everything else is as it was.
                self.cache.remove_page(r.page);
                self.failed.retain(|k| k.page != r.page);
                self.notices.remove(&r.page);
                self.links.remove(&r.page);
                for id in self.links_asked.iter().filter(|(_, p)| **p == r.page).map(|(id, _)| *id).collect::<Vec<_>>() {
                    self.engine.cancel(id);
                    self.links_asked.remove(&id);
                }
                self.edit.drop_page(r.page);
                self.schedule();
                self.ask_for_links();
                self.ask_for_annots();
                // The engine tells once, with a message, that the file is signed and the signature will show it as changed.
                let note = if !r.message.is_empty() {
                    Some(Msg::NoteSigned)
                } else if r.rewrote && !self.edit.rewrote {
                    self.edit.rewrote = true;
                    Some(Msg::NoteRewrite)
                } else {
                    match what {
                        Asked::Add => Some(Msg::NoteAdded),
                        Asked::Remove => Some(Msg::NoteRemoved),
                        Asked::Undo | Asked::Redo => None,
                    }
                };
                if let Some(m) = note {
                    actions.extend(self.say(t(lang, m).to_string()));
                }
            }
            EditStatus::Denied => actions.extend(self.say(t(lang, Msg::NoteAnnotDenied).to_string())),
            EditStatus::Failed => actions.extend(self.say(format!("{} {}", t(lang, Msg::NoteEditFailed), r.message))),
            EditStatus::Nothing => {
                let m = if what == Asked::Redo { Msg::NoteNothingRedo } else { Msg::NoteNothingUndo };
                actions.extend(self.say(t(lang, m).to_string()));
            }
            EditStatus::Busy => actions.extend(self.say(t(lang, Msg::NoteBusy).to_string())),
        }
        actions
    }

    // --- saving --------------------------------------------------------------------------------------------------

    /// Save to the file the document was opened from.
    pub(super) fn save(&mut self) -> Vec<Action> {
        match self.path.clone() {
            Some(path) if self.doc.is_some() => self.start_save(path),
            _ => Vec::new(),
        }
    }

    pub(super) fn save_as(&mut self) -> Vec<Action> {
        let Some(path) = self.path.clone().filter(|_| self.doc.is_some()) else { return Vec::new() };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let filters = vec![(t(self.lang, Msg::FilterPdf).to_string(), "*.pdf".to_string())];
        vec![Action::PickSavePath { title: t(self.lang, Msg::SaveTitle).to_string(), filters, name }]
    }

    pub(super) fn save_picked(&mut self, path: Option<PathBuf>) -> Vec<Action> {
        match path {
            Some(p) if self.doc.is_some() => self.start_save(std::path::absolute(&p).unwrap_or(p)),
            _ => Vec::new(),
        }
    }

    fn start_save(&mut self, path: PathBuf) -> Vec<Action> {
        if self.edit.saving.is_some() {
            return Vec::new();
        }
        let id = self.new_id();
        self.engine.save(self.doc_id, id, &path.to_string_lossy());
        self.edit.saving = Some((id, path));
        Vec::new()
    }

    pub(super) fn on_saved(&mut self, id: u64, status: SaveStatus, message: &str) -> Vec<Action> {
        let Some((_, path)) = self.edit.saving.take().filter(|(i, _)| *i == id) else { return Vec::new() };
        let lang = self.lang;
        match status {
            SaveStatus::Done => {
                self.edit.dirty = false;
                self.menus_dirty = true;
                let renamed = self.path.as_ref() != Some(&path);
                self.path = Some(path);
                if renamed {
                    self.remember_position();
                }
                let mut actions = vec![Action::SetTitle(self.title()), Action::Invalidate];
                match self.edit.after.take() {
                    Some(After::Quit) => actions.extend(self.exit_now()),
                    Some(After::Open(p)) => actions.extend(self.open_path_now(&p, "")),
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
        Some(vec![Action::AskSave { tag: TAG_UNSAVED, title: t(self.lang, Msg::UnsavedTitle).to_string(), text: t(self.lang, Msg::UnsavedAsk).to_string() }])
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
            SaveChoice::Discard => {
                self.edit.dirty = false;
                match after {
                    After::Quit => self.exit_now(),
                    After::Open(p) => self.open_path_now(&p, ""),
                }
            }
            SaveChoice::Cancel => Vec::new(),
        }
    }

    pub(super) fn exit_now(&mut self) -> Vec<Action> {
        // A print job that is going is stopped first (AbortDoc, and the printer is let go of).
        let mut actions = self.cancel_print_for_new_document();
        self.remember_position();
        actions.push(Action::Quit);
        actions
    }

    // --- drawing over the pages ----------------------------------------------------------------------------------

    /// The stroke being drawn and the frame of the annotation picked, on a page whose top left corner is at (`left`, `top`).
    pub(super) fn draw_edit_marks(&self, p: &mut dyn Painter, page: usize, left: i64, top: i64) {
        let Some(doc) = self.doc.as_ref() else { return };
        let Some(map) = doc.layout.page_map(page) else { return };
        if let Some(draft) = self.edit.ink.as_ref().filter(|d| d.page as usize == page) {
            let width_pt = f64::from(WIDTHS.get(self.edit.width).map_or(2.0, |w| w.1));
            let side = (width_pt * doc.layout.scale()).round().max(1.0) as i64;
            let [r, g, b] = self.rgb();
            let color = (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
            let dot = |p: &mut dyn Painter, x: f64, y: f64| {
                p.fill_rect(Rect { x: clamp32(left + x.round() as i64 - side / 2), y: clamp32(top + y.round() as i64 - side / 2), w: clamp32(side), h: clamp32(side) }, color);
            };
            let mut last: Option<(f64, f64)> = None;
            for pt in &draft.points {
                let (x, y) = map.to_px(f64::from(pt[0]), f64::from(pt[1]));
                if let Some((lx, ly)) = last {
                    // Dots along the segment, as many as keep it unbroken.
                    let steps = ((x - lx).abs().max((y - ly).abs()) / (side as f64 / 2.0).max(1.0)).ceil().clamp(1.0, 4000.0) as i64;
                    for i in 1..=steps {
                        let t = i as f64 / steps as f64;
                        dot(p, lx + (x - lx) * t, ly + (y - ly) * t);
                    }
                } else {
                    dot(p, x, y);
                }
                last = Some((x, y));
            }
        }
        if let Some((pp, _)) = self.edit.picked
            && pp as usize == page
            && let Some(a) = self.edit.picked_info()
        {
            let rects: Vec<[f32; 4]> = if a.parts.is_empty() { vec![a.rect] } else { a.parts.clone() };
            for r in rects {
                let (x0, y0, x1, y1) = map.rect_to_px(r);
                let (x0, y0, x1, y1) = (left + x0.floor() as i64 - 1, top + y0.floor() as i64 - 1, left + x1.ceil() as i64 + 1, top + y1.ceil() as i64 + 1);
                let t = self.px(2).max(1);
                let rect = |x: i64, y: i64, w: i64, h: i64| Rect { x: clamp32(x), y: clamp32(y), w: clamp32(w.max(1)), h: clamp32(h.max(1)) };
                p.fill_rect(rect(x0, y0, x1 - x0, t), PICK_COLOR);
                p.fill_rect(rect(x0, y1 - t, x1 - x0, t), PICK_COLOR);
                p.fill_rect(rect(x0, y0, t, y1 - y0), PICK_COLOR);
                p.fill_rect(rect(x1 - t, y0, t, y1 - y0), PICK_COLOR);
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
        app.event(Event::MouseDown { x: at.0, y: at.1, ctrl: false, shift: false });
        let ask = app.event(Event::MouseUp { x: at.0, y: at.1 });
        assert!(ask.iter().any(|a| matches!(a, Action::AskText { tag: TAG_NOTE, .. })), "no question about the text of the note");
        app.event(Event::Text { tag: TAG_NOTE, text: Some("a note".to_string()) });
        pump(&mut app, "the note", |a| a.edit.dirty && a.edit.annots.get(&0).is_some_and(|l| l.iter().any(|i| i.subtype == "Text")));
        // Back to selecting, click on the icon: it is picked.
        app.event(Event::Command(CMD_TOOL_SELECT));
        app.event(Event::MouseDown { x: at.0 + 6, y: at.1 + 6, ctrl: false, shift: false });
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
        app.event(Event::MouseDown { x: 500, y: 300, ctrl: false, shift: false });
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
}
