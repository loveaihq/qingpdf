//! The Redact menu (功能清单第⑧项, 2026-10-20): marks for taking content out of the pages, and applying them. A mark is a `Redact` annotation that the
//! engine makes (dragged out as a box with the shape tool, or from the text that is selected, or from every place a word or a kind of number is
//! found); it is listed, picked, taken out and undone like the other annotations. Applying them is one box (the words on the boxes, a switch for
//! the metadata, a warning) and then where to write the new file, next to the open one with "-涂黑" added to its name; the engine makes it in the
//! background, and when it is done the new file is opened (in a tab of its own; in the same tab if it was written over the open file).

use qingpdf_core::view::{FindSpec, MarkupKind, Preset, RedactHow, SaveStatus, line_rects};

use super::editing::Asked;
use super::*;
use crate::ui::{FormAnswer, FormItem, FormKind::*, FormSpec};

pub(super) struct Redacting {
    /// The choice made in the box, waiting for the name of the file to write.
    waiting: Option<RedactHow>,
    /// The engine is making the file: the request and where it writes.
    running: Option<(u64, PathBuf)>,
}

impl Redacting {
    pub(super) fn new() -> Redacting {
        Redacting { waiting: None, running: None }
    }

    /// A redaction is waiting for the name of the file or is going on.
    pub(super) fn busy(&self) -> bool {
        self.running.is_some() || self.waiting.is_some()
    }

    /// The open file has gone: what was going on for it is given up (the engine is told).
    pub(super) fn reset(&mut self) -> Option<u64> {
        self.waiting = None;
        self.running.take().map(|(id, _)| id)
    }
}

/// The colour of the outline of a mark.
const REDACT_RED: [u8; 3] = [230, 30, 30];

fn label(lang: Lang, m: Msg, rect: (i32, i32, i32, i32)) -> FormItem {
    FormItem { kind: Label, text: t(lang, m).to_string(), rect, choices: Vec::new(), selected: 0 }
}

impl App {
    /// Redact > Mark the selected text: a mark for each line of it, on each page it covers.
    pub(super) fn redact_selection(&mut self) -> Vec<Action> {
        let Some(sel) = self.selection else { return self.say(t(self.lang, Msg::NoteRedactSelect).to_string()) };
        if !self.rights.annotate {
            return self.say(t(self.lang, Msg::NoteAnnotDenied).to_string());
        }
        if let Some(wait) = self.edit_wait() {
            return wait;
        }
        let (first, last) = sel.ordered();
        for page in first.page..=last.page {
            let Some(b) = self.boxes.get(&page) else { continue };
            let Some((a, z)) = sel.range_in(page, b.boxes.len()) else { continue };
            let rects = line_rects(b.boxes.get(a..z).unwrap_or(&[]));
            if rects.is_empty() {
                continue;
            }
            let id = self.new_id();
            self.engine.add_markup(self.doc_id, id, page, MarkupKind::Redact, rects, REDACT_RED, &self.edit.author);
            self.edit.asked.insert(id, Asked::Add);
        }
        self.selection = None;
        vec![Action::Invalidate]
    }

    /// Redact > Find and mark: the box asks for the words, or the kinds of number.
    pub(super) fn ask_redact_find(&mut self) -> Vec<Action> {
        let lang = self.lang;
        if self.doc.is_none() {
            return Vec::new();
        }
        if !self.rights.annotate {
            return self.say(t(lang, Msg::NoteAnnotDenied).to_string());
        }
        let check = |m: Msg, y: i32| FormItem { kind: Check, text: t(lang, m).to_string(), rect: (10, y, 460, 22), choices: Vec::new(), selected: 0 };
        let items = vec![
            label(lang, Msg::RdFindText, (10, 10, 460, 18)),
            FormItem { kind: Edit, text: String::new(), rect: (10, 30, 460, 24), choices: Vec::new(), selected: 0 },
            label(lang, Msg::RdKinds, (10, 68, 460, 18)),
            check(Msg::RdIdCard, 90),
            check(Msg::RdPhone, 114),
            check(Msg::RdBank, 138),
        ];
        vec![Action::AskForm(FormSpec { tag: TAG_REDACT_FIND, title: t(lang, Msg::RdFindTitle).to_string(), size: (480, 204), items, ok: t(lang, Msg::RdFindOk).to_string(), cancel: t(lang, Msg::Cancel).to_string(), preview: None })]
    }

    /// The box was closed: with OK every place the words or the kinds of number are found is marked.
    pub(super) fn redact_find_answered(&mut self, answer: Option<FormAnswer>) -> Vec<Action> {
        let Some(FormAnswer { values, .. }) = answer.filter(|a| a.values.len() == 4) else { return Vec::new() };
        if self.doc.is_none() {
            return Vec::new();
        }
        let ticked = |i: usize| values.get(i).map(String::as_str) == Some("1");
        let words: Vec<String> = values.first().map(|w| w.trim().to_string()).filter(|w| !w.is_empty()).into_iter().collect();
        let presets: Vec<Preset> = [Preset::IdCard, Preset::Phone, Preset::BankCard].into_iter().enumerate().filter(|(i, _)| ticked(i + 1)).map(|(_, p)| p).collect();
        if words.is_empty() && presets.is_empty() {
            return Vec::new();
        }
        if let Some(wait) = self.edit_wait() {
            return wait;
        }
        let id = self.new_id();
        self.engine.redact_find(self.doc_id, id, FindSpec { words, presets }, &self.edit.author);
        self.edit.asked.insert(id, Asked::Mark);
        Vec::new()
    }

    /// Redact > Apply: the box with the warning.
    pub(super) fn ask_redact_apply(&mut self) -> Vec<Action> {
        let lang = self.lang;
        if self.doc.is_none() {
            return Vec::new();
        }
        if !self.rights.modify {
            return self.say(t(lang, Msg::NoteModifyDenied).to_string());
        }
        if self.rd.busy() || self.cz.busy() || self.pt.is_busy() || self.print.is_some() || self.ocr.is_some() || self.edit.saving.is_some() || self.edit.exporting.is_some() {
            return self.say(t(lang, Msg::NoteBusy).to_string());
        }
        let items = vec![
            label(lang, Msg::RdWarn, (10, 6, 460, 92)),
            label(lang, Msg::RdLabel, (10, 108, 460, 18)),
            FormItem { kind: Edit, text: String::new(), rect: (10, 128, 460, 24), choices: Vec::new(), selected: 0 },
            FormItem { kind: Check, text: t(lang, Msg::RdClear).to_string(), rect: (10, 164, 460, 22), choices: Vec::new(), selected: 0 },
            FormItem { kind: Check, text: t(lang, Msg::RdAttach).to_string(), rect: (10, 188, 460, 22), choices: Vec::new(), selected: 0 },
        ];
        vec![Action::AskForm(FormSpec { tag: TAG_REDACT_APPLY, title: t(lang, Msg::RdApplyTitle).to_string(), size: (480, 254), items, ok: t(lang, Msg::RdApplyOk).to_string(), cancel: t(lang, Msg::Cancel).to_string(), preview: None })]
    }

    /// The box was closed: with OK the person is asked where to write the file (next to the open one, with "-涂黑" added to its name).
    pub(super) fn redact_apply_answered(&mut self, answer: Option<FormAnswer>) -> Vec<Action> {
        let Some(FormAnswer { values, .. }) = answer.filter(|a| a.values.len() == 3) else { return Vec::new() };
        let Some(path) = self.path.clone().filter(|_| self.doc.is_some()) else { return Vec::new() };
        let ticked = |i: usize| values.get(i).map(String::as_str) == Some("1");
        self.rd.waiting = Some(RedactHow { label: values.first().map(|l| l.trim().to_string()).unwrap_or_default(), clear_metadata: ticked(1), remove_active: ticked(2) });
        let full = path.to_string_lossy();
        let name = format!("{}{}.pdf", full.strip_suffix(".pdf").or_else(|| full.strip_suffix(".PDF")).unwrap_or(&full), t(self.lang, Msg::RdSuffix));
        let filters = vec![(t(self.lang, Msg::FilterPdf).to_string(), "*.pdf".to_string())];
        vec![Action::PickSavePath { title: t(self.lang, Msg::RdPickTitle).to_string(), filters, name }]
    }

    /// The box that asks where to write was answered. `None`: it was not asked for this.
    pub(super) fn redact_picked(&mut self, path: Option<PathBuf>) -> Option<Vec<Action>> {
        let how = self.rd.waiting.take()?;
        let Some(path) = path.map(|p| std::path::absolute(&p).unwrap_or(p)).filter(|_| self.doc.is_some()) else { return Some(Vec::new()) };
        let id = self.new_id();
        self.engine.redact(self.doc_id, id, &path.to_string_lossy(), how);
        self.rd.running = Some((id, path));
        Some(self.say(t(self.lang, Msg::RdWorking).to_string()))
    }

    /// The engine has made the file (or not): the new file is opened.
    pub(super) fn on_redacted(&mut self, id: u64, status: SaveStatus, message: &str, pages: u32, marks: u32) -> Vec<Action> {
        let Some((_, path)) = self.rd.running.take().filter(|(i, _)| *i == id) else { return Vec::new() };
        let lang = self.lang;
        match status {
            SaveStatus::Failed => vec![Action::Message { title: t(lang, Msg::Error).to_string(), text: format!("{}\n{message}", t(lang, Msg::RdFailed)) }],
            SaveStatus::Done => {
                // (Written over the open file, the tab shows the new one; else the new file has a tab of its own.)
                let mut actions = if self.path.as_deref() == Some(path.as_path()) { self.open_path_now(&path, "") } else { self.open_path(&path, "") };
                actions.extend(self.say(t(lang, Msg::RdDone).replace("{marks}", &marks.to_string()).replace("{pages}", &pages.to_string())));
                // What the engine found that the person must know (the warnings: text that is left in places, a page that could not be read, ...).
                if !message.is_empty() {
                    actions.push(Action::Message { title: t(lang, Msg::RdApplyTitle).to_string(), text: format!("{}\n\n{message}", t(lang, Msg::RdNotes)) });
                }
                actions
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::i18n::Lang;
    use crate::ui::Event;

    fn pump(app: &mut App, what: &str, until: impl Fn(&App) -> bool) {
        for _ in 0..1500 {
            app.event(Event::Wake);
            if until(app) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for {what}");
    }

    fn marks(app: &App) -> usize {
        app.edit.annots.get(&0).map_or(0, |l| l.iter().filter(|a| a.subtype == "Redact").count())
    }

    fn answer(tag: u32, v: &[&str]) -> Event {
        Event::Form { tag, answer: Some(FormAnswer { values: v.iter().map(ToString::to_string).collect(), button: None }) }
    }

    #[test]
    fn marks_are_found_dragged_listed_and_applied_and_the_new_file_is_opened() {
        let corpus = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/corpus/public/xref-classic/hello_world.pdf");
        let dir = std::env::temp_dir().join(format!("qingpdf-view-redact-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("hello.pdf");
        std::fs::copy(corpus, &file).unwrap();
        let mut app = App::new(Lang::En, Arc::new(|| {}), Instant::now(), 1920 * 1080, "Tester".to_string());
        app.event(Event::Size { w: 900, h: 700 });
        // Nothing open: nothing to ask.
        assert!(app.event(Event::Command(CMD_REDACT_FIND)).is_empty() && app.event(Event::Command(CMD_REDACT_APPLY)).is_empty());
        app.open_path(&file, "");
        pump(&mut app, "the file to open", |a| a.has_document());
        pump(&mut app, "the annotations", |a| a.edit.annots.contains_key(&0));
        // Find and mark: the box asks for the words and three kinds of number.
        let asked = app.event(Event::Command(CMD_REDACT_FIND));
        let [Action::AskForm(form)] = &asked[..] else { panic!("{asked:?}") };
        assert_eq!(form.tag, TAG_REDACT_FIND);
        assert_eq!((form.items.iter().filter(|i| i.kind == Edit).count(), form.items.iter().filter(|i| i.kind == Check).count()), (1, 3));
        assert!(form.items.iter().all(|i| i.rect.0 >= 0 && i.rect.0 + i.rect.2 <= form.size.0 && i.rect.1 + i.rect.3 <= form.size.1 - 44));
        // Nothing typed and nothing ticked: nothing is asked of the engine; cancelled likewise.
        assert!(app.event(answer(TAG_REDACT_FIND, &["  ", "0", "0", "0"])).is_empty() && app.edit.asked.is_empty());
        assert!(app.event(Event::Form { tag: TAG_REDACT_FIND, answer: None }).is_empty());
        app.event(answer(TAG_REDACT_FIND, &["Goodbye", "0", "0", "0"]));
        pump(&mut app, "the mark", |a| a.edit.dirty && a.edit.annots.get(&0).is_some_and(|l| l.iter().any(|i| i.subtype == "Redact")));
        assert!(app.note.as_deref().is_some_and(|n| n.contains("Marked 1 place")), "{:?}", app.note);
        // The mark is in the list of annotations with its own name.
        assert_eq!(app.kind_name("Redact"), "Redaction mark");
        // The tool: a box dragged out is a second mark; the menu has it checked.
        app.event(Event::Command(CMD_TOOL_REDACT));
        assert_eq!(app.tool_command(), CMD_TOOL_REDACT);
        app.event(Event::MouseDown { x: 200, y: 200, ctrl: false, shift: false, clicks: 1 });
        app.event(Event::MouseMove { x: 260, y: 230, shift: false });
        app.event(Event::MouseUp { x: 260, y: 230 });
        pump(&mut app, "the second mark", |a| marks(a) == 2);
        app.event(Event::Command(CMD_TOOL_SELECT));
        // The box that applies them: a warning, the words on the boxes, the metadata switch; then where to write (next to the file, "-redacted").
        let asked = app.event(Event::Command(CMD_REDACT_APPLY));
        let [Action::AskForm(form)] = &asked[..] else { panic!("{asked:?}") };
        assert_eq!(form.tag, TAG_REDACT_APPLY);
        assert!(form.items.iter().any(|i| i.kind == Label && i.text.contains("cannot be undone")));
        assert_eq!(form.items.iter().filter(|i| i.kind == Check).count(), 2);
        assert!(app.event(Event::Form { tag: TAG_REDACT_APPLY, answer: None }).is_empty());
        let asked = app.event(answer(TAG_REDACT_APPLY, &["X", "1", "0"]));
        let [Action::PickSavePath { name, .. }] = &asked[..] else { panic!("{asked:?}") };
        assert!(std::path::Path::new(name).is_absolute() && name.ends_with("hello-redacted.pdf"), "{name}");
        assert!(app.event(Event::SavePicked(None)).is_empty() && !app.rd.busy());
        app.event(answer(TAG_REDACT_APPLY, &["X", "1", "1"]));
        assert_eq!(app.rd.waiting.as_ref().map(|h| (h.clear_metadata, h.remove_active)), Some((true, true)));
        let target = dir.join("new.pdf");
        app.event(Event::SavePicked(Some(target.clone())));
        assert!(app.rd.busy() && app.note.as_deref() == Some("Redacting..."), "{:?}", app.note);
        // Done: the new file is open in a tab of its own, the old one is as it was, the new one has no mark and not the word.
        pump(&mut app, "the new file", |a| a.path.as_deref() == Some(target.as_path()) && a.has_document() && !a.rd.busy());
        let bytes = std::fs::read(&target).unwrap();
        assert!(!bytes.windows(7).any(|w| w == b"Goodbye") && std::fs::read(&file).unwrap() == std::fs::read(corpus).unwrap());
        let doc = qingpdf_core::Document::from_bytes(bytes).unwrap();
        let text = qingpdf_core::text::TextExtractor::new(&doc).page_text(&doc.pages().unwrap()[0]).unwrap();
        assert!(text.contains("Hello") && !text.contains("Goodbye"), "{text}");
        assert_eq!(app.tabs.len(), 2);
        // What the engine had to say is shown (the lines, in a box); nothing to say, no box.
        let said = |app: &mut App, id: u64, message: &str| {
            app.rd.running = Some((id, target.clone()));
            app.on_redacted(id, SaveStatus::Done, message, 1, 2)
        };
        let actions = said(&mut app, 7, "The file has 1 attachment(s)\nThe removed text is still in the file, in bookmarks");
        assert!(actions.iter().any(|a| matches!(a, Action::Message { text, .. } if text.contains("1 attachment(s)") && text.contains("bookmarks"))), "{actions:?}");
        assert!(!said(&mut app, 8, "").iter().any(|a| matches!(a, Action::Message { .. })));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
