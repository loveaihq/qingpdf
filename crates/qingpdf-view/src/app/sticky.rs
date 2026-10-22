//! The message that stays (2026-10-22): what a job that is running says ("Exporting... (Esc cancels)") and the hint of the tool that is in use ("Drag over the text
//! to mark it (Esc ends)") are shown over the pages for as long as the job or the tool lasts, not for a few seconds. Both are read off the reader's state: a job's
//! words are kept with the job they belong to and are dropped the moment that job is not running, and the hint is that of the tool now chosen, so that nothing has
//! to remember to take a message away (Esc, a finished job, another tab, a closed file, all end it by changing the state). A failure is not a message that fades
//! but a box that has to be answered ([`App::fail`]). The words of ordinary messages are in `toast.rs`.

use super::*;

/// The jobs that say how far they have got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Job {
    Print,
    Export,
    Compress,
    Pictures,
    Crop,
    Ocr,
    Redact,
    Headings,
}

impl App {
    /// Is the job going on? (What the job says is shown only while it is.)
    fn job_running(&self, job: Job) -> bool {
        match job {
            Job::Print => self.print.is_some(),
            Job::Export => self.edit.exporting.is_some(),
            Job::Compress => self.cz.busy(),
            Job::Pictures => self.px.busy(),
            Job::Crop => self.cr.busy(),
            Job::Ocr => self.ocr.is_some(),
            Job::Redact => self.rd.busy(),
            Job::Headings => self.bm_searching(),
        }
    }

    /// The hint of the tool that is chosen (none for the pointer that selects, and for a picture that is not yet placed).
    pub(super) fn mode_hint(&self) -> Option<Msg> {
        if self.edit.forms.has_stamp() {
            return Some(Msg::NoteStampHint);
        }
        match self.edit.tool {
            Tool::Select | Tool::Stamp => None,
            Tool::Markup(_) => Some(Msg::NoteMarkHint),
            Tool::Ink => Some(Msg::NoteInkHint),
            Tool::Note => Some(Msg::NoteNoteHint),
            Tool::TextBox => Some(Msg::NoteTextBoxHint),
            Tool::EditText => Some(Msg::NoteEditTextHint),
            Tool::Shape(ShapeKind::Redact) => Some(Msg::NoteRedactHint),
            Tool::Crop => Some(Msg::NoteCropHint),
            Tool::Shape(_) => Some(Msg::NoteShapeHint),
        }
    }

    /// What a job that is running says now: it stays over the pages until the job is over.
    pub(super) fn say_job(&mut self, job: Job, text: String) -> Vec<Action> {
        self.note = Some(text.clone());
        self.job_note = Some((job, text));
        self.sync_sticky()
    }

    /// What should stay over the pages: the words of the job that is running, else the hint of the tool in use.
    fn sticky_text(&mut self) -> Option<String> {
        if self.job_note.as_ref().is_some_and(|(job, _)| !self.job_running(*job)) {
            self.job_note = None;
        }
        match &self.job_note {
            Some((_, words)) => Some(words.clone()),
            None => self.mode_hint().map(|m| t(self.lang, m).to_string()),
        }
    }

    /// Make the message that stays say what it should (after every event: the state may have changed in any of them). The actions draw the room of the messages
    /// again when it changed.
    pub(super) fn sync_sticky(&mut self) -> Vec<Action> {
        let want = self.sticky_text();
        if want.as_deref() == self.toasts.sticky() {
            return Vec::new();
        }
        let now = self.now_ms();
        self.toasts.set_sticky(want, now);
        // (The fast clock is for a message that fades; the words of one that stands changing, as a percentage does, need only a drawing.)
        let mut actions = if self.toasts.moving(now) { self.start_anim() } else { Vec::new() };
        actions.push(self.toast_invalid());
        actions
    }

    /// Something went wrong and the person has to read what: a box with the whole of the text (it is also what was last said), not a message that goes by itself in
    /// a few seconds and joins others of the same kind.
    pub(super) fn fail(&mut self, text: String) -> Vec<Action> {
        self.note = Some(text.clone());
        vec![Action::Message { title: t(self.lang, Msg::Error).to_string(), text }]
    }
}

#[cfg(test)]
mod tests {
    use super::super::sidebar::tests::{Rec, open_pages};
    use super::*;
    use crate::ui::vk;

    fn app(name: &str) -> App {
        let mut app = open_pages(name, &[(612, 792, 0); 3], false);
        app.bare = false;
        app.event(Event::Size { w: 1400, h: 900 });
        app
    }

    fn texts(app: &mut App) -> Vec<String> {
        let mut rec = Rec::default();
        app.paint(&mut rec);
        rec.texts.into_iter().map(|(_, _, s)| s).collect()
    }

    #[test]
    fn the_hint_of_a_tool_stays_over_the_pages_until_the_tool_ends() {
        let mut app = app("hint");
        assert_eq!(app.toasts.sticky(), None);
        app.event(Event::Command(CMD_TOOL_INK));
        assert_eq!(app.toasts.sticky(), Some(t(app.lang, Msg::NoteInkHint)));
        // It is not an ordinary message: it does not go by itself, however long it is there, and nothing asks for the fast clock.
        let later = app.now_ms() + 600_000;
        app.toasts.advance(later);
        assert_eq!(app.toasts.view(later).map(|v| v.alpha), Some(255));
        assert!(!app.toasts.moving(later) && app.toasts.next_change(later).is_none());
        assert!(texts(&mut app).iter().any(|s| s == t(app.lang, Msg::NoteInkHint)));
        // Esc is the tool's: the first one ends it (it is not eaten by the message), and the message goes with it.
        app.event(Event::Key { vk: vk::ESCAPE, ctrl: false, shift: false });
        assert_eq!(app.edit.tool, Tool::Select);
        assert_eq!(app.toasts.sticky(), None);
        let after = app.now_ms() + 700;
        app.toasts.advance(after);
        assert!(app.toasts.view(after).is_none());
    }

    #[test]
    fn the_hint_follows_the_tool_that_is_chosen_and_goes_with_the_pointer_that_selects() {
        let mut app = app("marking");
        // Choosing another tool changes the hint; choosing the pointer takes it away.
        app.event(Event::Command(CMD_TOOL_INK));
        app.event(Event::Command(CMD_TOOL_NOTE));
        assert_eq!(app.toasts.sticky(), Some(t(app.lang, Msg::NoteNoteHint)));
        app.event(Event::Command(CMD_TOOL_SELECT));
        assert_eq!(app.toasts.sticky(), None);
    }

    #[test]
    fn what_a_running_job_says_stays_until_the_job_is_over_and_then_goes_even_if_nobody_said_it_was_over() {
        let mut app = app("job");
        app.edit.exporting = Some(77);
        app.say_job(Job::Export, "Exporting... (Esc cancels)".to_string());
        assert_eq!(app.toasts.sticky(), Some("Exporting... (Esc cancels)"));
        assert_eq!(app.note.as_deref(), Some("Exporting... (Esc cancels)"));
        // A long time later it is still there (and the clock is not running for it).
        let later = app.now_ms() + 120_000;
        app.toasts.advance(later);
        assert_eq!(app.toasts.view(later).map(|v| v.alpha), Some(255));
        assert!(!app.toasts.moving(later));
        // The job ends (here: the state says so; whatever code ended it): the next event takes the message away.
        app.edit.exporting = None;
        let actions = app.event(Event::Timer(TIMER_ANIM));
        assert_eq!(app.toasts.sticky(), None, "{actions:?}");
        assert!(app.job_note.is_none());
    }

    #[test]
    fn an_ordinary_message_comes_first_and_the_message_that_stays_comes_back_after_it() {
        let mut app = app("both");
        app.event(Event::Command(CMD_TOOL_INK));
        let hint = t(app.lang, Msg::NoteInkHint).to_string();
        std::thread::sleep(std::time::Duration::from_millis(30));
        app.say("Copied 3 characters".to_string());
        let t0 = app.now_ms();
        // (The hint gives way: it fades out, then the message shows, and when that has gone the hint is back.)
        app.toasts.advance(t0 + crate::toast::FADE_OUT + 1);
        let shown = app.toasts.view(t0 + crate::toast::FADE_OUT + 1).map(|v| v.text.to_string());
        assert_eq!(shown.as_deref(), Some("Copied 3 characters"));
        let over = t0 + crate::toast::FADE_OUT + 1 + crate::toast::FADE_IN + crate::toast::HOLD + crate::toast::FADE_OUT + 1;
        app.toasts.advance(over);
        assert_eq!(app.toasts.view(over).map(|v| v.text.to_string()), Some(hint));
    }

    #[test]
    fn a_failure_is_a_box_with_the_whole_text_and_never_a_message_that_fades() {
        let mut app = app("fail");
        let long = "The change was not made: the engine said that the object 12 0 R could not be written because its stream is damaged".to_string();
        let actions = app.fail(long.clone());
        assert!(matches!(&actions[..], [Action::Message { text, .. }] if *text == long), "{actions:?}");
        assert_eq!(app.note.as_deref(), Some(long.as_str()));
        assert!(!app.toasts.busy(), "nothing is put over the pages to fade");
        // Two failures that differ only in a number are two boxes (not one that is changed).
        let a = app.fail("Could not write page 3".to_string());
        let b = app.fail("Could not write page 4".to_string());
        assert!(matches!(&a[..], [Action::Message { .. }]) && matches!(&b[..], [Action::Message { .. }]));
    }
}
