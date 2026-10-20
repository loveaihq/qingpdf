//! File > Compress: one box to choose how far the pictures are given up (three presets, or only what loses nothing), then where to
//! write the smaller file (next to the open one, with "-压缩" added to the name), and the engine makes it in the background, telling
//! how far it is in the status line; Esc stops it. When it is done the sizes are told and the person is asked whether to open the new
//! file. Changes not saved yet are saved first (the box asks), so that the file on the disk is what is compressed.

use qingpdf_core::compress::Preset;
use qingpdf_core::view::{CompressHow, SaveStatus};

use super::*;
use crate::ui::{FormAnswer, FormItem, FormKind::*, FormSpec};

pub(super) struct Squeeze {
    /// The choice made in the box, waiting for the name of the file to write.
    waiting: Option<CompressHow>,
    /// The engine is making the file: the request and where it writes.
    running: Option<(u64, PathBuf)>,
    /// The file that was made, while the person is asked whether to open it.
    made: Option<PathBuf>,
}

impl Squeeze {
    pub(super) fn new() -> Squeeze {
        Squeeze { waiting: None, running: None, made: None }
    }

    /// A compression is waiting for the name of the file or is going on.
    pub(super) fn busy(&self) -> bool {
        self.running.is_some() || self.waiting.is_some()
    }

    /// The open file has gone: what was going on for it is given up (the engine is told).
    pub(super) fn reset(&mut self) -> Option<u64> {
        self.waiting = None;
        self.made = None;
        self.running.take().map(|(id, _)| id)
    }
}

/// "5.2 MB", "340 KB": a size in bytes for a person.
fn size_text(bytes: u64) -> String {
    if bytes >= 1_000_000 { format!("{:.1} MB", bytes as f64 / 1_000_000.0) } else { format!("{} KB", bytes.div_ceil(1000)) }
}

impl App {
    /// File > Compress.
    pub(super) fn ask_compress(&mut self) -> Vec<Action> {
        // (A file that does not allow changing its content can still be compressed without loss: the box says so.)
        if self.doc.is_none() {
            return Vec::new();
        }
        if self.cz.running.is_some() || self.cz.waiting.is_some() || self.pt.is_busy() || self.print.is_some() || self.ocr.is_some() || self.edit.saving.is_some() || self.edit.exporting.is_some() {
            return self.say(t(self.lang, Msg::NoteBusy).to_string());
        }
        if self.edit.dirty {
            return vec![Action::Confirm { tag: TAG_COMPRESS_SAVE, title: t(self.lang, Msg::CzTitle).to_string(), text: t(self.lang, Msg::CzSaveAsk).to_string() }];
        }
        vec![Action::AskForm(self.compress_form())]
    }

    /// "Save the changes first?" was answered: yes saves, and the box comes when the file is saved.
    pub(super) fn compress_save_answered(&mut self, yes: bool) -> Vec<Action> {
        if !yes || self.doc.is_none() {
            return Vec::new();
        }
        self.edit.after = Some(After::Compress);
        let actions = self.save();
        if self.edit.saving.is_none() {
            self.edit.after = None;
        }
        actions
    }

    /// The box, with the balanced preset chosen; for a file whose content may not be changed, with "lossless only" ticked and a line
    /// that tells why.
    pub(super) fn compress_form(&self) -> FormSpec {
        let lang = self.lang;
        let locked = !self.rights.modify;
        let mut items = vec![
            FormItem { kind: Label, text: t(lang, Msg::CzIntro).to_string(), rect: (10, 6, 460, 56), choices: Vec::new(), selected: 0 },
            FormItem { kind: Label, text: t(lang, Msg::CzPictures).to_string(), rect: (10, 74, 60, 18), choices: Vec::new(), selected: 0 },
            FormItem {
                kind: Pick,
                text: String::new(),
                rect: (72, 70, 398, 110),
                choices: [Msg::CzHigh, Msg::CzBalanced, Msg::CzSmall].map(|m| t(lang, m).to_string()).to_vec(),
                selected: 1,
            },
            FormItem { kind: Check, text: t(lang, Msg::CzLossless).to_string(), rect: (10, 106, 460, 22), choices: Vec::new(), selected: usize::from(locked) },
        ];
        if locked {
            items.push(FormItem { kind: Label, text: t(lang, Msg::CzNoModify).to_string(), rect: (10, 132, 460, 40), choices: Vec::new(), selected: 0 });
        }
        FormSpec { tag: TAG_COMPRESS, title: t(lang, Msg::CzTitle).to_string(), size: (480, if locked { 216 } else { 176 }), items, ok: t(lang, Msg::CzOk).to_string(), cancel: t(lang, Msg::Cancel).to_string(), preview: None }
    }

    /// The box was closed: with OK the person is asked where to write the file.
    pub(super) fn compress_answered(&mut self, answer: Option<FormAnswer>) -> Vec<Action> {
        let Some(FormAnswer { values, .. }) = answer.filter(|a| a.values.len() == 2) else { return Vec::new() };
        let Some(path) = self.path.clone().filter(|_| self.doc.is_some()) else { return Vec::new() };
        let preset = match values.first().map(String::as_str) {
            Some("0") => Preset::High,
            Some("2") => Preset::Small,
            _ => Preset::Balanced,
        };
        // (Whatever the box says, a file that does not allow changing its content gets only the lossless part; the engine says no otherwise.)
        self.cz.waiting = Some(CompressHow { preset, lossless_only: !self.rights.modify || values.get(1).map(String::as_str) == Some("1") });
        // Next to the file, "-压缩" added to its name: the box starts in that folder with that name.
        let full = path.to_string_lossy();
        let name = format!("{}{}.pdf", full.strip_suffix(".pdf").or_else(|| full.strip_suffix(".PDF")).unwrap_or(&full), t(self.lang, Msg::CzSuffix));
        let filters = vec![(t(self.lang, Msg::FilterPdf).to_string(), "*.pdf".to_string())];
        vec![Action::PickSavePath { title: t(self.lang, Msg::CzPickTitle).to_string(), filters, name }]
    }

    /// The box that asks where to write was answered. `None`: it was not asked for this.
    pub(super) fn compress_picked(&mut self, path: Option<PathBuf>) -> Option<Vec<Action>> {
        let how = self.cz.waiting.take()?;
        let Some(path) = path.map(|p| std::path::absolute(&p).unwrap_or(p)).filter(|_| self.doc.is_some()) else { return Some(Vec::new()) };
        let id = self.new_id();
        self.engine.compress(self.doc_id, id, &path.to_string_lossy(), how);
        self.cz.running = Some((id, path));
        Some(self.say_working(0))
    }

    fn say_working(&mut self, percent: u32) -> Vec<Action> {
        let text = t(self.lang, Msg::CzWorking).replace("{n}", &percent.to_string());
        self.say(text)
    }

    pub(super) fn on_compress_progress(&mut self, id: u64, percent: u32) -> Vec<Action> {
        if self.cz.running.as_ref().is_none_or(|(i, _)| *i != id) {
            return Vec::new();
        }
        self.say_working(percent)
    }

    /// Esc while the file is being made: the engine is told to stop (it answers nothing). `None`: nothing is going on.
    pub(super) fn cancel_compress(&mut self) -> Option<Vec<Action>> {
        let (id, _) = self.cz.running.take()?;
        self.engine.cancel(id);
        Some(self.say(t(self.lang, Msg::CzCancelled).to_string()))
    }

    /// The engine has made the file (or not).
    pub(super) fn on_compressed(&mut self, id: u64, status: SaveStatus, message: &str, before: u64, after: u64, kept: bool) -> Vec<Action> {
        let Some((_, path)) = self.cz.running.take().filter(|(i, _)| *i == id) else { return Vec::new() };
        let lang = self.lang;
        match status {
            SaveStatus::Failed => vec![Action::Message { title: t(lang, Msg::Error).to_string(), text: format!("{}\n{message}", t(lang, Msg::CzFailed)) }],
            SaveStatus::Done if kept => vec![Action::Message { title: t(lang, Msg::CzTitle).to_string(), text: t(lang, Msg::CzKept).replace("{before}", &size_text(before)) }],
            SaveStatus::Done => {
                let pct = ((before - after.min(before)) as f64 * 100.0 / before.max(1) as f64).round();
                let text = t(lang, Msg::CzDone).replace("{before}", &size_text(before)).replace("{after}", &size_text(after)).replace("{pct}", &format!("{pct:.0}"));
                self.cz.made = Some(path);
                vec![Action::Confirm { tag: TAG_COMPRESS_OPEN, title: t(lang, Msg::CzTitle).to_string(), text }]
            }
        }
    }

    /// "Open the new file?" was answered.
    pub(super) fn compress_open_answered(&mut self, yes: bool) -> Vec<Action> {
        match self.cz.made.take() {
            Some(path) if yes => self.open_path_now(&path, ""),
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::i18n::Lang;
    use crate::ui::{Event, FormKind};

    /// One page of 200 x 200 points with a 600 x 600 colour picture (smooth, not coded) drawn 100 points wide: 432 dots per inch.
    fn page_with_a_picture() -> Vec<u8> {
        let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
        let mut offsets = std::collections::BTreeMap::new();
        let mut raw = Vec::with_capacity(600 * 600 * 3);
        for y in 0..600u32 {
            for x in 0..600u32 {
                raw.extend_from_slice(&[(128.0 + 100.0 * (f64::from(x) / 95.0).sin()) as u8, (f64::from(y) / 3.0) as u8, ((x + y) / 5) as u8]);
            }
        }
        let content = b"q 100 0 0 100 50 50 cm /Im1 Do Q".to_vec();
        let objects: Vec<(u32, Vec<u8>)> = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /XObject << /Im1 5 0 R >> >> >>".to_vec()),
            (4, [format!("<< /Length {} >>\nstream\n", content.len()).into_bytes(), content, b"\nendstream".to_vec()].concat()),
            (
                5,
                [
                    format!("<< /Type /XObject /Subtype /Image /Width 600 /Height 600 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length {} >>\nstream\n", raw.len()).into_bytes(),
                    raw,
                    b"\nendstream".to_vec(),
                ]
                .concat(),
            ),
        ];
        for (n, body) in &objects {
            offsets.insert(*n, out.len());
            out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let at = out.len();
        out.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
        for n in 1..6 {
            out.extend_from_slice(format!("{:010} 00000 n \n", offsets[&n]).as_bytes());
        }
        out.extend_from_slice(format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{at}\n%%EOF\n").as_bytes());
        out
    }

    /// Let the engine work until some event gives the actions `found` looks for (the actions of all the events are kept).
    fn pump_for(app: &mut App, what: &str, found: impl Fn(&App, &[Action]) -> bool) -> Vec<Action> {
        let mut all = Vec::new();
        for _ in 0..3000 {
            all.extend(app.event(Event::Wake));
            if found(app, &all) {
                return all;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for {what}");
    }

    #[test]
    fn the_box_asks_for_a_preset_the_file_is_written_next_to_the_old_one_and_the_sizes_are_told() {
        let dir = std::env::temp_dir().join(format!("qingpdf-view-compress-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("picture.pdf");
        std::fs::write(&file, page_with_a_picture()).unwrap();
        let before = std::fs::metadata(&file).unwrap().len();
        let mut app = App::new(Lang::En, Arc::new(|| {}), Instant::now(), 1920 * 1080, "Tester".to_string());
        app.event(Event::Size { w: 900, h: 700 });
        // Nothing open: nothing happens.
        assert!(app.event(Event::Command(CMD_COMPRESS)).is_empty());
        app.open_path(&file, "");
        pump_for(&mut app, "the file to open", |a, _| a.has_document());
        assert!(app.menus().iter().any(|m| m.entries.iter().any(|e| matches!(e, crate::ui::MenuEntry::Item { id, enabled: true, .. } if *id == CMD_COMPRESS))));

        // The box: the three presets (the balanced one chosen) and the tick box for lossless only.
        let asked = app.event(Event::Command(CMD_COMPRESS));
        let [Action::AskForm(form)] = &asked[..] else { panic!("{asked:?}") };
        assert_eq!(form.tag, TAG_COMPRESS);
        let picks: Vec<&FormItem> = form.items.iter().filter(|i| i.kind == FormKind::Pick).collect();
        assert_eq!((picks.len(), picks[0].choices.len(), picks[0].selected), (1, 3, 1));
        assert!(picks[0].choices[0].contains("200") && picks[0].choices[1].contains("150") && picks[0].choices[2].contains("100"));
        assert_eq!(form.items.iter().filter(|i| i.kind == FormKind::Check).count(), 1);
        assert!(form.items.iter().all(|i| i.rect.0 >= 0 && i.rect.0 + i.rect.2 <= form.size.0 && (i.kind == FormKind::Pick || i.rect.1 + i.rect.3 <= form.size.1)));
        // Cancelled, or answered with something that is not the box's values: nothing happens.
        assert!(app.event(Event::Form { tag: TAG_COMPRESS, answer: None }).is_empty());
        assert!(app.event(Event::Form { tag: TAG_COMPRESS, answer: Some(FormAnswer { values: vec!["1".to_string()], button: None }) }).is_empty());

        // OK with the smallest preset: where to write, next to the file with "-compressed" added; cancelling that does nothing.
        let answer = |v: &[&str]| Event::Form { tag: TAG_COMPRESS, answer: Some(FormAnswer { values: v.iter().map(ToString::to_string).collect(), button: None }) };
        let asked = app.event(answer(&["2", "0"]));
        let [Action::PickSavePath { name, .. }] = &asked[..] else { panic!("{asked:?}") };
        assert!(std::path::Path::new(name).is_absolute() && name.ends_with("picture-compressed.pdf"), "{name}");
        assert!(app.event(Event::SavePicked(None)).is_empty() && app.cz.running.is_none() && app.cz.waiting.is_none());
        // Again, and this time a name is given.
        app.event(answer(&["2", "0"]));
        let target = dir.join("small.pdf");
        let started = app.event(Event::SavePicked(Some(target.clone())));
        assert!(app.cz.running.is_some() && app.note.as_deref().is_some_and(|n| n.contains("Compressing") && n.contains("Esc")), "{started:?} {:?}", app.note);
        // Another request while it runs: busy.
        assert!(app.event(Event::Command(CMD_COMPRESS)).iter().all(|a| !matches!(a, Action::AskForm(_))));
        let done = pump_for(&mut app, "the answer", |_, a| a.iter().any(|x| matches!(x, Action::Confirm { tag: TAG_COMPRESS_OPEN, .. })));
        let Some(Action::Confirm { text, .. }) = done.iter().find(|x| matches!(x, Action::Confirm { .. })) else { panic!() };
        assert!(text.contains("smaller") && (text.contains("MB") || text.contains("KB")), "{text}");
        assert!(app.cz.running.is_none());
        let after = std::fs::metadata(&target).unwrap().len();
        assert!(after > 0 && after < before / 20, "{after} of {before}");
        assert_eq!(std::fs::metadata(&file).unwrap().len(), before, "the open file is not touched");
        // Not opened when the person says no; opened when the person says yes.
        assert!(app.event(Event::Confirmed { tag: TAG_COMPRESS_OPEN, yes: false }).is_empty() && app.cz.made.is_none());
        app.cz.made = Some(target.clone());
        app.event(Event::Confirmed { tag: TAG_COMPRESS_OPEN, yes: true });
        pump_for(&mut app, "the new file to open", |a, _| a.path.as_deref() == Some(target.as_path()) && a.has_document());

        // Esc while it works: the engine is told, the note says so, and no answer is waited for.
        app.event(answer(&["1", "0"]));
        app.event(Event::SavePicked(Some(dir.join("other.pdf"))));
        assert!(app.cz.running.is_some());
        app.event(Event::Key { vk: crate::ui::vk::ESCAPE, ctrl: false, shift: false });
        assert!(app.cz.running.is_none() && app.note.as_deref() == Some("Compression cancelled."), "{:?}", app.note);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn changes_not_saved_are_saved_first_when_the_person_says_so() {
        let dir = std::env::temp_dir().join(format!("qingpdf-view-compress-dirty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("picture.pdf");
        std::fs::write(&file, page_with_a_picture()).unwrap();
        let mut app = App::new(Lang::En, Arc::new(|| {}), Instant::now(), 1920 * 1080, "Tester".to_string());
        app.event(Event::Size { w: 900, h: 700 });
        app.open_path(&file, "");
        pump_for(&mut app, "the file to open", |a, _| a.has_document());
        app.edit.dirty = true;
        // (The flag was set by hand, so the window is told the new title along with the question.)
        let asked: Vec<Action> = app.event(Event::Command(CMD_COMPRESS)).into_iter().filter(|a| !matches!(a, Action::SetTitle(_))).collect();
        let [Action::Confirm { tag, text, .. }] = &asked[..] else { panic!("{asked:?}") };
        assert_eq!(*tag, TAG_COMPRESS_SAVE);
        assert!(text.contains("not saved"));
        // No: nothing happens, the changes stay.
        assert!(app.event(Event::Confirmed { tag: TAG_COMPRESS_SAVE, yes: false }).is_empty() && app.edit.dirty);
        // Yes: the file is saved, and then the box comes.
        app.event(Event::Confirmed { tag: TAG_COMPRESS_SAVE, yes: true });
        let shown = pump_for(&mut app, "the box after the save", |_, a| a.iter().any(|x| matches!(x, Action::AskForm(f) if f.tag == TAG_COMPRESS)));
        assert!(!app.edit.dirty && shown.iter().all(|a| !matches!(a, Action::Message { .. })));
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn a_file_that_may_not_be_changed_still_gets_the_box_with_only_the_lossless_part() {
        let dir = std::env::temp_dir().join(format!("qingpdf-view-compress-locked-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("picture.pdf");
        std::fs::write(&file, page_with_a_picture()).unwrap();
        let mut app = App::new(Lang::En, Arc::new(|| {}), Instant::now(), 1920 * 1080, "Tester".to_string());
        app.event(Event::Size { w: 900, h: 700 });
        app.open_path(&file, "");
        pump_for(&mut app, "the file to open", |a, _| a.has_document());
        app.rights.modify = false;
        // The menu entry stays enabled (what loses nothing is a plain rewrite).
        assert!(app.menus().iter().any(|m| m.entries.iter().any(|e| matches!(e, crate::ui::MenuEntry::Item { id, enabled: true, .. } if *id == CMD_COMPRESS))));
        // The box has "lossless only" ticked and says why; whatever it is answered with, only the lossless part is asked for.
        let asked = app.event(Event::Command(CMD_COMPRESS));
        let [Action::AskForm(form)] = &asked[..] else { panic!("{asked:?}") };
        let check = form.items.iter().find(|i| i.kind == FormKind::Check).unwrap();
        assert_eq!(check.selected, 1);
        assert!(form.items.iter().any(|i| i.kind == FormKind::Label && i.text.contains("did not allow")));
        assert!(form.items.iter().all(|i| i.rect.0 >= 0 && i.rect.0 + i.rect.2 <= form.size.0 && (i.kind == FormKind::Pick || i.rect.1 + i.rect.3 <= form.size.1)));
        let answer = Event::Form { tag: TAG_COMPRESS, answer: Some(FormAnswer { values: vec!["2".to_string(), "0".to_string()], button: None }) };
        let asked = app.event(answer);
        assert!(matches!(&asked[..], [Action::PickSavePath { .. }]));
        assert!(app.cz.waiting.as_ref().is_some_and(|how| how.lossless_only));
        // A file that allows it gets the box without the line, unticked.
        app.cz.waiting = None;
        app.rights.modify = true;
        let asked = app.event(Event::Command(CMD_COMPRESS));
        let [Action::AskForm(form)] = &asked[..] else { panic!("{asked:?}") };
        assert_eq!(form.items.iter().find(|i| i.kind == FormKind::Check).unwrap().selected, 0);
        assert!(form.items.iter().all(|i| !i.text.contains("did not allow")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
