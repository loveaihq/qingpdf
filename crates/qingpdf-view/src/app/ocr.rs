//! Tools > Recognize text (5c): the window's side of a run of recognition (`crate::ocr::Run` does the chain; this starts it, feeds it what
//! the engine and the worker say, shows how far it is, stops it with Esc and tells how it went).

use qingpdf_core::view::{EditStatus, Event as EngineEvent};

use super::*;
use crate::ocr::{self, OcrError, Opener, Progress, Run, Scope};

/// How many skipped pages a message lists.
const LISTED_SKIPS: usize = 8;

impl App {
    /// Run `f` with the engine and a source of request numbers (the window's own counter goes on from where `f` left it).
    fn with_ids<T>(&mut self, f: impl FnOnce(&Engine, &mut dyn FnMut() -> u64) -> T) -> T {
        let mut next = self.next_id;
        let mut give = || {
            let id = next;
            next += 1;
            id
        };
        let out = f(&self.engine, &mut give);
        self.next_id = next;
        out
    }

    /// The recogniser for a run: Windows' own, or the one a test has put in.
    fn ocr_opener(&mut self) -> Opener {
        match self.ocr_test_opener.take() {
            Some(opener) => opener,
            None => {
                let chinese = self.lang == Lang::Zh;
                Box::new(move || crate::win32::open_recognizer(chinese))
            }
        }
    }

    /// Tools > Recognize text in all scanned pages / on this page.
    pub(super) fn start_ocr(&mut self, current_page_only: bool) -> Vec<Action> {
        if self.doc.is_none() {
            return Vec::new();
        }
        if self.ocr.is_some() {
            return self.say(t(self.lang, Msg::OcrBusy).to_string());
        }
        if let Some(wait) = self.edit_wait() {
            return wait;
        }
        let scope = if current_page_only { Scope::Page(u32::try_from(self.current_page()).unwrap_or(0)) } else { Scope::Scans };
        let (doc, wake, opener) = (self.doc_id, self.wake.clone(), self.ocr_opener());
        let run = self.with_ids(|engine, ids| Run::start(engine, doc, scope, opener, ocr::PAGE_TIMEOUT, wake, ids));
        self.ocr = Some(run);
        self.menus_dirty = true;
        self.say_progress(t(self.lang, Msg::OcrLooking).to_string())
    }

    /// A message over the pages (what text recognition has got to; it is said again as the run goes on, and takes the place of the one showing).
    fn say_progress(&mut self, text: String) -> Vec<Action> {
        self.say_job(sticky::Job::Ocr, text)
    }

    /// Esc while text is being recognized: stop; the pages done so far get their text. `None`: nothing is running.
    pub(super) fn stop_ocr(&mut self) -> Option<Vec<Action>> {
        let mut run = self.ocr.take()?;
        let progress = self.with_ids(|engine, ids| run.stop(engine, ids));
        self.ocr = Some(run);
        Some(self.ocr_progress(progress))
    }

    /// An answer of the engine that may be the run's.
    pub(super) fn on_ocr_event(&mut self, event: EngineEvent) -> Vec<Action> {
        let Some(mut run) = self.ocr.take() else { return Vec::new() };
        let progress = self.with_ids(|engine, ids| run.on_event(engine, event, ids));
        self.ocr = Some(run);
        self.ocr_progress(progress)
    }

    /// What the worker has said since last looked.
    pub(super) fn pump_ocr(&mut self) -> Vec<Action> {
        let Some(mut run) = self.ocr.take() else { return Vec::new() };
        let progress = self.with_ids(|engine, ids| run.poll_worker(engine, ids));
        self.ocr = Some(run);
        progress.into_iter().flat_map(|p| self.ocr_progress(p)).collect()
    }

    /// The engine answered the request to write the layer.
    pub(super) fn ocr_layer_answered(&mut self, id: u64, status: EditStatus, message: &str) -> Vec<Action> {
        let Some(mut run) = self.ocr.take() else { return Vec::new() };
        let progress = run.on_layer(id, status == EditStatus::Done, message);
        self.ocr = Some(run);
        self.ocr_progress(progress)
    }

    /// Is `id` the request that writes the layer of the run that is going on?
    pub(super) fn is_ocr_layer(&self, id: u64) -> bool {
        self.ocr.as_ref().is_some_and(|r| r.is_layer_request(id))
    }

    /// Let go of the run (the document is closed or another is opened): what it waits for is withdrawn.
    pub(super) fn drop_ocr(&mut self) {
        if let Some(run) = self.ocr.take() {
            if let Some(id) = run.waiting_for() {
                self.engine.cancel(id);
            }
            self.menus_dirty = true;
        }
    }

    /// Show what a step of the run came to.
    fn ocr_progress(&mut self, progress: Progress) -> Vec<Action> {
        let lang = self.lang;
        match progress {
            Progress::Quiet => Vec::new(),
            Progress::Working { position, total } => {
                let text = t(lang, Msg::OcrWorking).replace("{n}", &position.to_string()).replace("{total}", &total.to_string());
                self.say_progress(text)
            }
            Progress::Writing => self.say_progress(t(lang, Msg::OcrWriting).to_string()),
            Progress::NothingToDo { .. } => {
                self.ocr = None;
                self.menus_dirty = true;
                self.say(t(lang, Msg::OcrNothing).to_string())
            }
            Progress::NoRecogniser(e) => {
                self.ocr = None;
                self.menus_dirty = true;
                let text = match &e {
                    OcrError::NoLanguage { chinese, available } => no_language_text(lang, *chinese, available),
                    other => format!("{}\n{}", t(lang, Msg::OcrFailed), ocr::describe(other)),
                };
                vec![Action::Message { title: t(lang, Msg::OcrTitle).to_string(), text }, Action::Invalidate]
            }
            Progress::Failed(why) => {
                self.ocr = None;
                self.menus_dirty = true;
                vec![Action::Message { title: t(lang, Msg::OcrTitle).to_string(), text: format!("{}\n{why}", t(lang, Msg::OcrFailed)) }, Action::Invalidate]
            }
            Progress::Finished(written) => {
                let Some(run) = self.ocr.take() else { return Vec::new() };
                self.menus_dirty = true;
                let s = run.summary;
                let mut line = match &written {
                    Ok(()) if s.stopped && s.chars == 0 => t(lang, Msg::OcrStoppedNone).to_string(),
                    Ok(()) if s.chars == 0 => t(lang, Msg::OcrNoText).to_string(),
                    Ok(()) => t(lang, if s.stopped { Msg::OcrStopped } else { Msg::OcrDone }).replace("{pages}", &s.pages_done.to_string()).replace("{chars}", &s.chars.to_string()),
                    Err(why) => format!("{} {why}", t(lang, Msg::OcrNotWritten)),
                };
                if s.layer_note.contains("window only") {
                    line.push(' ');
                    line.push_str(t(lang, Msg::OcrMemoryOnly));
                }
                if s.too_many {
                    line.push(' ');
                    line.push_str(t(lang, Msg::OcrTooMany));
                }
                if !s.skipped.is_empty() {
                    line.push(' ');
                    line.push_str(&t(lang, Msg::OcrSkipped).replace("{n}", &s.skipped.len().to_string()));
                }
                if !s.incomplete.is_empty() {
                    line.push(' ');
                    line.push_str(&t(lang, Msg::OcrIncomplete).replace("{n}", &s.incomplete.len().to_string()));
                }
                let mut actions = self.say(line);
                // One box: the pages given up, and the pages drawn in part, each with its reason.
                let mut text = String::new();
                for (pages, head) in [(&s.skipped, Msg::OcrSkippedWhy), (&s.incomplete, Msg::OcrIncompleteWhy)] {
                    if pages.is_empty() {
                        continue;
                    }
                    if !text.is_empty() {
                        text.push_str("\n\n");
                    }
                    text.push_str(t(lang, head));
                    for (page, why) in pages.iter().take(LISTED_SKIPS) {
                        text.push_str(&format!("\n{}: {why}", t(lang, Msg::OcrPageN).replace("{n}", &(page + 1).to_string())));
                    }
                    if pages.len() > LISTED_SKIPS {
                        text.push_str("\n...");
                    }
                }
                if !text.is_empty() {
                    actions.push(Action::Message { title: t(lang, Msg::OcrTitle).to_string(), text });
                }
                actions
            }
        }
    }
}

/// The box that says Windows cannot recognise the language: how to add it, and what there is.
fn no_language_text(lang: Lang, chinese: bool, available: &[String]) -> String {
    let list = if available.is_empty() { t(lang, Msg::OcrNoneAvailable).to_string() } else { available.join(", ") };
    format!("{}\n\n{} {list}", t(lang, if chinese { Msg::OcrNoChinese } else { Msg::OcrNoLanguage }), t(lang, Msg::OcrAvailable))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::ocr::tests::{fake, scan_file};
    use crate::ui::MenuEntry;
    use qingpdf_core::view::{OcrLine, OcrWord};

    fn pump(app: &mut App, what: &str, mut until: impl FnMut(&App) -> bool) -> Vec<Action> {
        let mut all = Vec::new();
        for _ in 0..3000 {
            all.extend(app.event(Event::Wake));
            if until(app) {
                return all;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for {what}");
    }

    fn opened(name: &str, pages: usize, with_text: &[usize], lang: Lang) -> App {
        let file = scan_file(name, pages, with_text);
        let mut app = App::new(lang, Arc::new(|| {}), Instant::now(), 1920 * 1080, "Tester".to_string());
        app.event(Event::Size { w: 900, h: 700 });
        app.open_path(&file, "");
        pump(&mut app, "the file to open", |a| a.has_document());
        app
    }

    fn one_line() -> Vec<OcrLine> {
        let w = |t: &str, l: f32, r: f32| OcrWord { text: t.to_string(), rect: [l, 100.0, r, 140.0] };
        vec![OcrLine { words: vec![w("Hello", 100.0, 300.0), w("world", 320.0, 520.0)] }]
    }

    #[test]
    fn tools_recognize_text_runs_in_the_background_says_how_far_and_adds_one_step_to_undo() {
        let mut app = opened("app-run.pdf", 3, &[1], Lang::En);
        assert!(app.menus().iter().any(|m| m.title == "&Tools"));
        app.ocr_test_opener = Some(fake(10_000, |_| Ok(one_line())));
        let started = app.event(Event::Command(CMD_OCR_ALL));
        assert!(app.ocr.is_some() && started.iter().any(|a| matches!(a, Action::Invalidate | Action::InvalidateRect(_))));
        assert_eq!(app.note.as_deref(), Some("Looking for scanned pages... (Esc to stop)"));
        // While it runs the menu items are greyed, and a second start only says so.
        assert!(app.start_ocr(false).iter().any(|a| matches!(a, Action::Invalidate | Action::InvalidateRect(_))));
        assert_eq!(app.note.as_deref(), Some("Text is being recognized already (Esc stops it)."));
        let mut notes = Vec::new();
        pump(&mut app, "the run to end", |a| {
            if let Some(n) = &a.note {
                notes.push(n.clone());
            }
            a.ocr.is_none()
        });
        assert!(notes.iter().any(|n| n.starts_with("Recognizing text: page 1 of 2")), "{notes:?}");
        assert_eq!(app.note.as_deref(), Some("Recognized 2 page(s), 20 characters. They can be searched and copied; Undo takes the text out."));
        assert!(app.edit.can_undo && app.edit.dirty);
        // One undo takes the layer out of both pages.
        app.event(Event::Command(CMD_UNDO));
        pump(&mut app, "the undo", |a| !a.edit.can_undo);
        assert!(app.menus().iter().flat_map(|m| m.entries.iter()).any(|e| matches!(e, MenuEntry::Item { id, enabled: true, .. } if *id == CMD_OCR_ALL)));
    }

    #[test]
    fn esc_stops_the_run_and_keeps_the_pages_done_and_this_page_needs_a_scan() {
        let mut app = opened("app-stop.pdf", 6, &[], Lang::En);
        // Slow enough that Esc comes in the middle: each page takes a moment.
        app.ocr_test_opener = Some(fake(10_000, |_| {
            std::thread::sleep(Duration::from_millis(60));
            Ok(one_line())
        }));
        app.event(Event::Command(CMD_OCR_ALL));
        pump(&mut app, "two pages", |a| a.ocr.as_ref().is_some_and(|r| r.summary.pages_done >= 2));
        app.event(Event::Key { vk: crate::ui::vk::ESCAPE, ctrl: false, shift: false });
        pump(&mut app, "the run to end", |a| a.ocr.is_none());
        let note = app.note.clone().unwrap_or_default();
        assert!(note.starts_with("Stopped. ") && note.contains("page(s) were recognized"), "{note}");
        assert!(app.edit.can_undo, "the pages done have their text");
        // The page in view has text now: asking for it again finds nothing to do.
        app.ocr_test_opener = Some(fake(10_000, |_| Ok(one_line())));
        app.event(Event::Command(CMD_OCR_PAGE));
        pump(&mut app, "nothing to do", |a| a.ocr.is_none());
        assert!(app.note.as_deref().unwrap_or("").starts_with("No page to recognize"), "{:?}", app.note);
    }

    #[test]
    fn no_chinese_recogniser_is_said_in_a_box_with_the_languages_there_are_and_nothing_is_changed() {
        for (lang, chinese, expect) in [(Lang::Zh, true, "添加中文语言包"), (Lang::En, false, "Add a language")] {
            let mut app = opened("app-nolang.pdf", 1, &[], lang);
            app.ocr_test_opener = Some(Box::new(move || Err(crate::ocr::OcrError::NoLanguage { chinese, available: vec!["en-US".to_string(), "en-GB".to_string()] })));
            app.event(Event::Command(CMD_OCR_ALL));
            let actions = pump(&mut app, "the box", |a| a.ocr.is_none());
            let text = actions.iter().find_map(|a| if let Action::Message { text, .. } = a { Some(text.clone()) } else { None }).expect("a box");
            assert!(text.contains(expect) && text.contains("en-US, en-GB"), "{text}");
            assert!(!app.edit.dirty);
        }
    }
}
