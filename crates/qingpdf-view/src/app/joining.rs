//! File > Merge PDFs and File > Images to PDF: the person picks several files in the Windows box, puts them in order in the list box
//! (`filelist`), and a thread of its own reads them and makes the new file with the engine (`ops::merge`, `ops::images_to_pdf_with`), telling
//! how far it is in the status line; Esc stops it. A file that needs a password is asked for it by the thread (the box is the one used when a
//! file is opened); cancelling that box leaves the file out. The person says where to write the result once the files are read, and when it is
//! done is asked whether to open it. The open file is not touched, and a file that is open with changes not yet saved is joined as it is on the disk.

use std::borrow::Cow;
use std::io::Write;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};

use qingpdf_core::image::PageMode;
use qingpdf_core::ops::{self, Input, LoadedImage, Output};
use qingpdf_core::{Document, Error, export};

use super::filelist::{self, FileList};
use super::*;
use crate::ui::FormAnswer;

/// What is joined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Merge,
    Images,
}

/// Where the person and the work are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Idle,
    /// The Windows box that picks the files is up: the first time (cancelling it ends it all) or to add more (cancelling it goes back to the list).
    Picking { first: bool },
    /// The list box is up.
    Listing,
    /// The thread is reading the files (and asking for passwords).
    Reading,
    /// The box that asks where to write is up.
    Saving,
    /// The thread is making the file and writing it.
    Writing,
}

/// What the thread tells the reader.
enum Told {
    /// This file needs a password (`wrong`: the one given was not it).
    Password { name: String, wrong: bool },
    /// All the files are read: these are left (a file whose password was not given is not), and where to write is wanted.
    Ready(Vec<PathBuf>),
    Progress(u32),
    Done(Result<Made, Failure>),
}

struct Made {
    pages: usize,
    warnings: Vec<String>,
}

enum Failure {
    /// The new file would be written over one of the files that are joined.
    SameFile,
    Text(String),
}

/// What the reader tells the thread.
enum Asked {
    /// The password for the file (none: it was not given, leave the file out).
    Password(Option<String>),
    /// Write the file here.
    Go(PathBuf),
}

/// What the reader and the thread pass between them: what each has told the other that has not been taken yet (a plain queue with a lock, not a
/// channel: a channel for each kind of thing is code for each), and whether the work is to stop.
struct Pipe {
    told: Mutex<(VecDeque<Told>, VecDeque<Asked>)>,
    changed: Condvar,
    stop: AtomicBool,
}

impl Pipe {
    fn lock(&self) -> MutexGuard<'_, (VecDeque<Told>, VecDeque<Asked>)> {
        self.told.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Tell the thread to stop, and wake it if it is waiting.
    fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        drop(self.lock());
        self.changed.notify_all();
    }
}

pub(super) struct Join {
    kind: Kind,
    stage: Stage,
    list: FileList,
    /// The page size chosen for pictures: 0 is A4, 1 is the size of the picture.
    size: usize,
    helper: Option<Arc<Pipe>>,
    /// Where the file is written, and (once it is) the file that the person is asked whether to open.
    target: Option<PathBuf>,
    made: Option<PathBuf>,
}

impl Join {
    pub(super) fn new() -> Join {
        Join { kind: Kind::Merge, stage: Stage::Idle, list: FileList::default(), size: 0, helper: None, target: None, made: None }
    }

    /// Something is being chosen or made.
    pub(super) fn busy(&self) -> bool {
        self.stage != Stage::Idle
    }

    /// The thread is working (the status line tells how far it is).
    pub(super) fn working(&self) -> bool {
        matches!(self.stage, Stage::Reading | Stage::Writing)
    }

    /// Give up what was going on (the thread is told to stop).
    fn clear(&mut self) {
        if let Some(pipe) = self.helper.take() {
            pipe.stop();
        }
        self.stage = Stage::Idle;
        self.list = FileList::default();
        self.target = None;
    }
}

/// A file's name, for messages and for the engine.
fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(|| path.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned())
}

/// What the thread has to talk with.
struct Talk<'a> {
    pipe: &'a Pipe,
    wake: &'a (dyn Fn() + Send + Sync),
}

impl Talk<'_> {
    fn tell(&self, what: Told) {
        self.pipe.lock().0.push_back(what);
        (self.wake)();
    }

    fn stopped(&self) -> bool {
        self.pipe.stop.load(Ordering::Relaxed)
    }

    /// Wait for what the reader tells; `None` when the work is to stop.
    fn asked(&self) -> Option<Asked> {
        let mut queue = self.pipe.lock();
        loop {
            if let Some(what) = queue.1.pop_front() {
                return Some(what);
            }
            if self.stopped() {
                return None;
            }
            queue = self.pipe.changed.wait(queue).unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Wait for the reader to say where to write, `None` when it has gone or has given up.
    fn target(&self) -> Option<PathBuf> {
        match self.asked() {
            Some(Asked::Go(path)) => Some(path),
            _ => None,
        }
    }
}

/// Read the PDF files (asking for the passwords that are needed), then, once the reader has said where to write, join them.
fn merge_job(talk: &Talk<'_>, files: Vec<PathBuf>) {
    let total = files.len().max(1);
    let mut opened: Vec<(PathBuf, Document, String)> = Vec::with_capacity(files.len());
    for (i, path) in files.into_iter().enumerate() {
        if talk.stopped() {
            return;
        }
        talk.tell(Told::Progress((i * 50 / total) as u32));
        let (name, mut password, mut asked) = (name_of(&path), String::new(), false);
        let doc = loop {
            match Document::open_with_password(&path, &password) {
                Ok(d) if !d.is_locked() => break Some(d),
                Ok(_) | Err(Error::PasswordRequired | Error::WrongPassword) => {
                    talk.tell(Told::Password { name: name.clone(), wrong: asked });
                    asked = true;
                    match talk.asked() {
                        Some(Asked::Password(Some(p))) => password = p,
                        Some(Asked::Password(None)) => break None,
                        _ => return,
                    }
                }
                Err(e) => return talk.tell(Told::Done(Err(Failure::Text(format!("{name}\n{e}"))))),
            }
        };
        if let Some(doc) = doc {
            opened.push((path, doc, password));
        }
    }
    let kept: Vec<PathBuf> = opened.iter().map(|o| o.0.clone()).collect();
    talk.tell(Told::Ready(kept.clone()));
    let Some(out) = talk.target() else { return };
    if overwrites(&out, &kept) {
        return talk.tell(Told::Done(Err(Failure::SameFile)));
    }
    let names: Vec<String> = kept.iter().map(|p| name_of(p)).collect();
    let inputs: Vec<Input<'_>> = opened.iter().zip(&names).map(|((_, doc, password), name)| Input { name, doc, password }).collect();
    talk.tell(Told::Progress(60));
    write_result(talk, ops::merge(&inputs), &out);
}

/// Wait for the reader to say where to write, then make the PDF of the pictures, one picture in memory at a time.
fn images_job(talk: &Talk<'_>, files: &[PathBuf], mode: PageMode) {
    talk.tell(Told::Ready(files.to_vec()));
    let Some(out) = talk.target() else { return };
    if overwrites(&out, files) {
        return talk.tell(Told::Done(Err(Failure::SameFile)));
    }
    let mut load = |i: usize| -> Result<LoadedImage<'static>, Error> {
        if talk.stopped() {
            return Err(Error::Invalid("stopped".to_string()));
        }
        talk.tell(Told::Progress((i * 90 / files.len().max(1)) as u32));
        let path = files.get(i).ok_or_else(|| Error::Invalid("no such image".to_string()))?;
        let data = std::fs::read(path).map_err(|e| Error::Invalid(format!("{}: {e}", name_of(path))))?;
        Ok(LoadedImage { name: name_of(path), data: Cow::Owned(data) })
    };
    let made = ops::images_to_pdf_with(files.len(), &mut load, mode);
    write_result(talk, made, &out);
}

/// Write what the engine made (unless the person has stopped it) and tell how it went.
fn write_result(talk: &Talk<'_>, made: Result<Output, Error>, out: &Path) {
    if talk.stopped() {
        return;
    }
    talk.tell(Told::Progress(95));
    let done = made.and_then(|o| {
        export::write_file(out, |mut f| {
            f.write_all(&o.data)?;
            Ok(f)
        })?;
        Ok(Made { pages: o.pages, warnings: o.warnings.iter().map(ToString::to_string).collect() })
    });
    if !talk.stopped() {
        talk.tell(Told::Done(done.map_err(|e| Failure::Text(e.to_string()))));
    }
}

/// Is the file `out` one of `inputs` (the same file by any name)?
fn overwrites(out: &Path, inputs: &[PathBuf]) -> bool {
    let Ok(out) = std::fs::canonicalize(out) else { return false };
    inputs.iter().any(|i| std::fs::canonicalize(i).is_ok_and(|i| i == out))
}

impl App {
    // --- the person chooses ------------------------------------------------------------------------------------------

    /// File > Merge PDFs: the Windows box picks the files; the open file, if there is one, is the first of them.
    pub(super) fn ask_merge(&mut self) -> Vec<Action> {
        if self.jn.busy() {
            return self.say(t(self.lang, Msg::NoteBusy).to_string());
        }
        self.jn.kind = Kind::Merge;
        self.jn.list = FileList::default();
        if let Some(open) = self.path.clone().filter(|_| self.doc.is_some()) {
            self.jn.list.files.push(std::path::absolute(&open).unwrap_or(open));
        }
        self.pick_more(true)
    }

    /// File > Images to PDF (or pictures dropped on the window, which fill the list before the box is shown).
    pub(super) fn ask_images(&mut self, dropped: Vec<PathBuf>) -> Vec<Action> {
        if self.jn.busy() {
            return self.say(t(self.lang, Msg::NoteBusy).to_string());
        }
        self.jn.kind = Kind::Images;
        self.jn.size = 0;
        self.jn.list = FileList::default();
        if dropped.is_empty() {
            return self.pick_more(true);
        }
        let mut dropped = dropped;
        filelist::sort_naturally(&mut dropped);
        self.jn.list.add(dropped);
        self.jn.list.chosen = 0;
        self.show_list()
    }

    /// The Windows box that picks several files of the kind that is joined.
    fn pick_more(&mut self, first: bool) -> Vec<Action> {
        let lang = self.lang;
        self.jn.stage = Stage::Picking { first };
        let (title, kind, pattern) = match self.jn.kind {
            Kind::Merge => (Msg::MgPickTitle, Msg::FilterPdf, "*.pdf"),
            Kind::Images => (Msg::ImPickTitle, Msg::FilterImages, "*.jpg;*.jpeg;*.png"),
        };
        vec![Action::PickFiles { title: t(lang, title).to_string(), filters: vec![(t(lang, kind).to_string(), pattern.to_string())] }]
    }

    fn show_list(&mut self) -> Vec<Action> {
        self.jn.stage = Stage::Listing;
        let (title, intro, ok) = match self.jn.kind {
            Kind::Merge => (Msg::MgTitle, Msg::MgIntro, Msg::MgOk),
            Kind::Images => (Msg::ImTitle, Msg::ImIntro, Msg::ImOk),
        };
        vec![Action::AskForm(filelist::form(self.lang, TAG_JOIN, title, intro, ok, &self.jn.list, (self.jn.kind == Kind::Images).then_some(self.jn.size)))]
    }

    /// The Windows box that picks files was answered.
    pub(super) fn join_files_picked(&mut self, mut files: Vec<PathBuf>) -> Vec<Action> {
        let Stage::Picking { first } = self.jn.stage else { return Vec::new() };
        files = files.into_iter().map(|f| std::path::absolute(&f).unwrap_or(f)).collect();
        if self.jn.kind == Kind::Images {
            filelist::sort_naturally(&mut files);
        } else {
            // (The file that is open is in the list already.)
            files.retain(|f| !(first && self.jn.list.files.contains(f)));
        }
        if files.is_empty() && first {
            self.jn.clear();
            return Vec::new();
        }
        self.jn.list.add(files);
        if first && self.jn.kind == Kind::Merge {
            self.jn.list.chosen = 0;
        }
        self.show_list()
    }

    /// The list box was closed: a button changes the list and shows the box again, OK starts the work.
    pub(super) fn join_answered(&mut self, answer: Option<FormAnswer>) -> Vec<Action> {
        if self.jn.stage != Stage::Listing {
            return Vec::new();
        }
        let images = self.jn.kind == Kind::Images;
        let Some(FormAnswer { values, button }) = answer.filter(|a| a.values.len() == 1 + usize::from(images)) else {
            self.jn.clear();
            return Vec::new();
        };
        let number = |k: usize| values.get(k).and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
        self.jn.list.chosen = number(0).min(self.jn.list.files.len().saturating_sub(1));
        self.jn.size = number(1).min(1);
        match button {
            Some(filelist::ADD) => return self.pick_more(false),
            Some(filelist::UP) => self.jn.list.up(),
            Some(filelist::DOWN) => self.jn.list.down(),
            Some(filelist::REMOVE) => self.jn.list.remove(),
            _ => return self.join_start(),
        }
        self.show_list()
    }

    /// OK in the list box: with enough files, the thread starts reading them.
    fn join_start(&mut self) -> Vec<Action> {
        let need = if self.jn.kind == Kind::Merge { 2 } else { 1 };
        if self.jn.list.files.len() < need {
            let mut actions = vec![Action::Message { title: t(self.lang, Msg::Error).to_string(), text: t(self.lang, Msg::JnTooFew).replace("{n}", &need.to_string()) }];
            actions.extend(self.show_list());
            return actions;
        }
        let pipe = Arc::new(Pipe { told: Mutex::new((VecDeque::new(), VecDeque::new())), changed: Condvar::new(), stop: AtomicBool::new(false) });
        let (kind, files, wake, thread_pipe) = (self.jn.kind, self.jn.list.files.clone(), self.wake.clone(), pipe.clone());
        let mode = if self.jn.size == 1 { PageMode::Fit } else { PageMode::A4 };
        let started = std::thread::Builder::new().name("qingpdf-join".to_string()).spawn(move || {
            let talk = Talk { pipe: &thread_pipe, wake: &*wake };
            match kind {
                Kind::Merge => merge_job(&talk, files),
                Kind::Images => images_job(&talk, &files, mode),
            }
        });
        if started.is_err() {
            self.jn.clear();
            return self.fail(t(self.lang, Msg::JnFailed).to_string());
        }
        self.jn.helper = Some(pipe);
        self.jn.stage = Stage::Reading;
        self.say_joining(0)
    }

    // --- the thread works --------------------------------------------------------------------------------------------

    fn say_joining(&mut self, percent: u32) -> Vec<Action> {
        let msg = if self.jn.kind == Kind::Merge { Msg::MgWorking } else { Msg::ImWorking };
        let text = t(self.lang, msg).replace("{n}", &percent.to_string());
        self.say_job(sticky::Job::Join, text)
    }

    /// What the thread has told since the last time.
    pub(super) fn pump_join(&mut self) -> Vec<Action> {
        let mut actions = Vec::new();
        while let Some(told) = self.jn.helper.as_ref().and_then(|pipe| pipe.lock().0.pop_front()) {
            actions.extend(match told {
                Told::Password { name, wrong } => self.join_ask_password(&name, wrong),
                Told::Ready(files) => self.join_ready(files),
                Told::Progress(percent) => self.say_joining(percent),
                Told::Done(result) => self.join_done(result),
            });
        }
        actions
    }

    fn tell_thread(&self, what: Asked) {
        if let Some(pipe) = &self.jn.helper {
            pipe.lock().1.push_back(what);
            pipe.changed.notify_all();
        }
    }

    fn join_ask_password(&mut self, name: &str, wrong: bool) -> Vec<Action> {
        let lang = self.lang;
        vec![Action::AskText {
            tag: TAG_JOIN_PASSWORD,
            title: t(lang, Msg::PasswordTitle).to_string(),
            prompt: format!("{name}\n{}", t(lang, if wrong { Msg::PasswordWrongPrompt } else { Msg::PasswordPrompt })),
            secret: true,
            initial: String::new(),
            ok: t(lang, Msg::Ok).to_string(),
            cancel: t(lang, Msg::Cancel).to_string(),
        }]
    }

    /// The password box was answered: the thread goes on with it (without one, the file is left out).
    pub(super) fn join_password_answered(&mut self, text: Option<String>) -> Vec<Action> {
        self.tell_thread(Asked::Password(text));
        Vec::new()
    }

    /// The files are read: the person says where to write (the thread waits).
    fn join_ready(&mut self, files: Vec<PathBuf>) -> Vec<Action> {
        let need = if self.jn.kind == Kind::Merge { 2 } else { 1 };
        let Some(first) = files.first().filter(|_| files.len() >= need) else {
            self.jn.clear();
            return self.say(t(self.lang, Msg::JnTooFew).replace("{n}", &need.to_string()));
        };
        let lang = self.lang;
        let merge = self.jn.kind == Kind::Merge;
        let name = filelist::default_name(first, if merge { t(lang, Msg::MgSuffix) } else { "" });
        let title = t(lang, if merge { Msg::MgSaveTitle } else { Msg::ImSaveTitle }).to_string();
        self.jn.stage = Stage::Saving;
        vec![Action::PickSavePath { title, filters: vec![(t(lang, Msg::FilterPdf).to_string(), "*.pdf".to_string())], name }]
    }

    /// The box that asks where to write was answered. `None`: it was not asked for this.
    pub(super) fn join_picked(&mut self, path: Option<PathBuf>) -> Option<Vec<Action>> {
        if self.jn.stage != Stage::Saving {
            return None;
        }
        let Some(path) = path.map(|p| std::path::absolute(&p).unwrap_or(p)) else {
            self.jn.clear();
            return Some(Vec::new());
        };
        self.tell_thread(Asked::Go(path.clone()));
        self.jn.target = Some(path);
        self.jn.stage = Stage::Writing;
        Some(self.say_joining(60))
    }

    fn join_done(&mut self, result: Result<Made, Failure>) -> Vec<Action> {
        let lang = self.lang;
        let target = self.jn.target.take();
        self.jn.clear();
        match (result, target) {
            (Ok(made), Some(path)) => {
                let mut warnings: Vec<String> = made.warnings.iter().take(6).cloned().collect();
                if made.warnings.len() > warnings.len() {
                    warnings.push("\u{2026}".to_string());
                }
                let warnings = if warnings.is_empty() { String::new() } else { format!("{}\n\n", warnings.join("\n")) };
                let text = t(lang, Msg::JnDone).replace("{n}", &made.pages.to_string()).replace("{warnings}", &warnings);
                self.jn.made = Some(path);
                // The percentage is taken off before the question (which waits for the person), not after it.
                let mut actions = self.sync_sticky();
                actions.push(Action::Confirm { tag: TAG_JOIN_OPEN, title: t(lang, if self.jn.kind == Kind::Merge { Msg::MgTitle } else { Msg::ImTitle }).to_string(), text });
                actions
            }
            (Err(Failure::SameFile), _) => self.fail(t(lang, Msg::JnSame).to_string()),
            (Err(Failure::Text(m)), _) => self.fail(format!("{}\n{m}", t(lang, Msg::JnFailed))),
            (Ok(_), None) => Vec::new(),
        }
    }

    /// "Open the new file?" was answered.
    pub(super) fn join_open_answered(&mut self, yes: bool) -> Vec<Action> {
        match self.jn.made.take() {
            Some(path) if yes => self.open_path_now(&path, ""),
            _ => Vec::new(),
        }
    }

    /// Esc while the files are read or the file is made: the thread is told to stop. `None`: nothing is going on.
    pub(super) fn cancel_join(&mut self) -> Option<Vec<Action>> {
        if !self.jn.working() {
            return None;
        }
        self.jn.clear();
        Some(self.say(t(self.lang, Msg::JnCancelled).to_string()))
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::i18n::Lang;
    use crate::ui::{Event, FormSpec};

    /// A PDF of `pages` pages of 200 x 200 points, the first page's text being `label`.
    fn pdf(pages: usize, label: &str) -> Vec<u8> {
        let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        let kids: String = (0..pages).map(|i| format!("{} 0 R ", 4 + i)).collect();
        let mut objects: Vec<String> = vec!["<< /Type /Catalog /Pages 2 0 R >>".to_string(), format!("<< /Type /Pages /Kids [{kids}] /Count {pages} >>"), "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string()];
        for i in 0..pages {
            objects.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents {} 0 R /Resources << /Font << /F1 3 0 R >> >> >>", 4 + pages + i));
        }
        for i in 0..pages {
            let content = format!("BT /F1 12 Tf 20 100 Td ({label} {i}) Tj ET");
            objects.push(format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()));
        }
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
        }
        let at = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
        for o in &offsets {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{at}\n%%EOF\n", objects.len() + 1).as_bytes());
        out
    }

    /// A one-by-one pixel PNG (red).
    fn png() -> Vec<u8> {
        fn chunk(kind: &[u8], data: &[u8]) -> Vec<u8> {
            let mut body = kind.to_vec();
            body.extend_from_slice(data);
            let mut crc = 0xFFFF_FFFFu32;
            for &b in &body {
                crc ^= u32::from(b);
                for _ in 0..8 {
                    crc = if crc & 1 != 0 { 0xEDB8_8320 ^ (crc >> 1) } else { crc >> 1 };
                }
            }
            let mut out = (data.len() as u32).to_be_bytes().to_vec();
            out.extend_from_slice(&body);
            out.extend_from_slice(&(!crc).to_be_bytes());
            out
        }
        // The data of the one pixel: a filter byte and three colour bytes, as a zlib stream of a stored block.
        let raw = [0u8, 255, 0, 0];
        let (mut a, mut b) = (1u32, 0u32);
        for &x in &raw {
            a = (a + u32::from(x)) % 65_521;
            b = (b + a) % 65_521;
        }
        let mut z = vec![0x78, 0x01, 0x01, 4, 0, 0xFB, 0xFF];
        z.extend_from_slice(&raw);
        z.extend_from_slice(&((b << 16) | a).to_be_bytes());
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        out.extend(chunk(b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0]));
        out.extend(chunk(b"IDAT", &z));
        out.extend(chunk(b"IEND", &[]));
        out
    }

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

    fn new_app() -> App {
        let mut app = App::new(Lang::En, Arc::new(|| {}), Instant::now(), 1920 * 1080, "Tester".to_string());
        app.event(Event::Size { w: 900, h: 700 });
        app
    }

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("qingpdf-view-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Where qpdf is, if it is on this machine: it checks what the tests write.
    fn qpdf() -> Option<PathBuf> {
        let exe = if cfg!(windows) { "qpdf.exe" } else { "qpdf" };
        let mut places = vec![std::env::var_os("QINGPDF_QPDF").map(PathBuf::from).unwrap_or_default(), Path::new("C:/Dev/tools/qpdf/bin").join(exe)];
        places.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).map(|d| d.join(exe)));
        places.into_iter().find(|p| p.is_file())
    }

    /// `qpdf --check` accepts the file and it has `pages` pages.
    fn assert_qpdf_accepts(file: &Path, pages: usize) {
        let Some(qpdf) = qpdf() else {
            // (Without qpdf the engine reads it back.)
            assert_eq!(Document::open(file).unwrap().page_count().unwrap(), pages);
            return;
        };
        let check = std::process::Command::new(&qpdf).arg("--check").arg(file).output().unwrap();
        assert!(check.status.success(), "{}{}", String::from_utf8_lossy(&check.stdout), String::from_utf8_lossy(&check.stderr));
        let count = std::process::Command::new(&qpdf).arg("--show-npages").arg(file).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&count.stdout).trim(), pages.to_string());
    }

    fn list_answer(chosen: usize, size: Option<usize>, button: Option<usize>) -> Event {
        let mut values = vec![chosen.to_string()];
        values.extend(size.map(|s| s.to_string()));
        Event::Form { tag: TAG_JOIN, answer: Some(FormAnswer { values, button }) }
    }

    #[test]
    fn files_picked_one_after_the_other_are_put_in_order_and_merged_into_a_file_that_qpdf_accepts() {
        let dir = folder("merge");
        let (a, b, c) = (dir.join("a.pdf"), dir.join("b.pdf"), dir.join("c.pdf"));
        std::fs::write(&a, pdf(2, "a")).unwrap();
        std::fs::write(&b, pdf(1, "b")).unwrap();
        std::fs::write(&c, pdf(3, "c")).unwrap();
        let mut app = new_app();
        // The open file is first in the list.
        app.open_path(&a, "");
        pump_for(&mut app, "the file to open", |x, _| x.has_document());
        assert!(app.menus().iter().any(|m| m.entries.iter().any(|e| matches!(e, crate::ui::MenuEntry::Item { id, enabled: true, .. } if *id == CMD_MERGE))));
        let asked = app.event(Event::Command(CMD_MERGE));
        assert!(matches!(&asked[..], [Action::PickFiles { .. }]), "{asked:?}");
        // Another request while it goes on: busy.
        assert!(app.event(Event::Command(CMD_MERGE)).iter().all(|x| !matches!(x, Action::PickFiles { .. })));
        // The box gives the open file again, c and b: the open file is not listed twice.
        let shown = app.event(Event::FilesPicked(vec![a.clone(), c.clone(), b.clone()]));
        let [Action::AskForm(form)] = &shown[..] else { panic!("{shown:?}") };
        assert_eq!(form.tag, TAG_JOIN);
        assert_eq!(app.jn.list.files, vec![a.clone(), c.clone(), b.clone()]);
        assert_eq!(app.jn.list.chosen, 0);
        // Move c to the end: a, b, c.
        app.event(list_answer(1, None, Some(filelist::DOWN)));
        assert_eq!((app.jn.list.files.clone(), app.jn.list.chosen), (vec![a.clone(), b.clone(), c.clone()], 2));
        // Cancelling the box that adds files goes back to the list.
        assert!(matches!(&app.event(list_answer(2, None, Some(filelist::ADD)))[..], [Action::PickFiles { .. }]));
        assert!(matches!(&app.event(Event::FilesPicked(Vec::new()))[..], [Action::AskForm(_)]));
        // OK: the files are read and the person is asked where to write, next to the first with "-merged" added.
        app.event(list_answer(2, None, None));
        assert!(app.jn.working());
        let asked = pump_for(&mut app, "the question where to write", |_, x| x.iter().any(|y| matches!(y, Action::PickSavePath { .. })));
        let Some(Action::PickSavePath { name, .. }) = asked.iter().find(|y| matches!(y, Action::PickSavePath { .. })) else { panic!() };
        assert_eq!(name, &dir.join("a-merged.pdf").to_string_lossy().into_owned());
        let target = dir.join("all.pdf");
        let started = app.event(Event::SavePicked(Some(target.clone())));
        assert!(app.note.as_deref().is_some_and(|n| n.contains("Merging") && n.contains("Esc")), "{started:?} {:?}", app.note);
        let done = pump_for(&mut app, "the answer", |_, x| x.iter().any(|y| matches!(y, Action::Confirm { tag: TAG_JOIN_OPEN, .. }) || matches!(y, Action::Message { .. })));
        let Some(Action::Confirm { text, .. }) = done.iter().find(|y| matches!(y, Action::Confirm { .. })) else { panic!("{done:?}") };
        assert!(text.contains("6 page"), "{text}");
        assert!(!app.jn.busy());
        assert_qpdf_accepts(&target, 6);
        // The files that were joined are as they were.
        assert_eq!(std::fs::read(&b).unwrap(), pdf(1, "b"));
        // Yes opens the new file.
        app.event(Event::Confirmed { tag: TAG_JOIN_OPEN, yes: true });
        pump_for(&mut app, "the new file to open", |x, _| x.path.as_deref() == Some(target.as_path()) && x.has_document());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_new_file_may_not_replace_a_file_that_is_joined_and_too_few_files_are_not_joined() {
        let dir = folder("merge-same");
        let (a, b) = (dir.join("a.pdf"), dir.join("b.pdf"));
        std::fs::write(&a, pdf(1, "a")).unwrap();
        std::fs::write(&b, pdf(1, "b")).unwrap();
        let mut app = new_app();
        app.event(Event::Command(CMD_MERGE));
        // Nothing picked and nothing open: it ends there.
        assert!(app.event(Event::FilesPicked(Vec::new())).is_empty() && !app.jn.busy());
        app.event(Event::Command(CMD_MERGE));
        app.event(Event::FilesPicked(vec![a.clone()]));
        // One file is not enough: said, and the list is shown again.
        let said = app.event(list_answer(0, None, None));
        assert!(matches!(&said[..], [Action::Message { text, .. }, Action::AskForm(_)] if text.contains('2')), "{said:?}");
        // Cancelling the list ends it.
        assert!(app.event(Event::Form { tag: TAG_JOIN, answer: None }).is_empty() && !app.jn.busy());
        app.event(Event::Command(CMD_MERGE));
        app.event(Event::FilesPicked(vec![a.clone(), b.clone()]));
        app.event(list_answer(0, None, None));
        pump_for(&mut app, "the question where to write", |_, x| x.iter().any(|y| matches!(y, Action::PickSavePath { .. })));
        app.event(Event::SavePicked(Some(b.clone())));
        let done = pump_for(&mut app, "the answer", |_, x| x.iter().any(|y| matches!(y, Action::Message { .. })));
        assert!(done.iter().any(|y| matches!(y, Action::Message { text, .. } if text.contains("replace"))), "{done:?}");
        assert_eq!(std::fs::read(&b).unwrap(), pdf(1, "b"));
        // Cancelling the box that asks where to write ends it too.
        app.event(Event::Command(CMD_MERGE));
        app.event(Event::FilesPicked(vec![a.clone(), b.clone()]));
        app.event(list_answer(0, None, None));
        pump_for(&mut app, "the question where to write", |_, x| x.iter().any(|y| matches!(y, Action::PickSavePath { .. })));
        assert!(app.event(Event::SavePicked(None)).is_empty() && !app.jn.busy());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_needs_a_password_is_asked_for_it_again_when_it_is_wrong_and_left_out_when_the_box_is_cancelled() {
        let dir = folder("merge-password");
        let a = dir.join("a.pdf");
        std::fs::write(&a, pdf(1, "a")).unwrap();
        // A file with a user password (from the corpus), copied here under the name the box will show.
        let source = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/corpus/public/encrypted/qpdf-generated/rects.r6-aes256-user-long.pdf");
        let password = "correct horse battery staple, a very long password of more than thirty-two characters";
        let locked = dir.join("locked.pdf");
        std::fs::copy(source, &locked).unwrap();
        let locked_pages = Document::open_with_password(&locked, password).unwrap().page_count().unwrap();
        let asked_password = |x: &[Action]| x.iter().any(|y| matches!(y, Action::AskText { tag: TAG_JOIN_PASSWORD, .. }));
        let mut app = new_app();
        app.event(Event::Command(CMD_MERGE));
        app.event(Event::FilesPicked(vec![a.clone(), locked.clone()]));
        app.event(list_answer(0, None, None));
        let asked = pump_for(&mut app, "the password box", |_, x| asked_password(x));
        let Some(Action::AskText { prompt, secret, .. }) = asked.iter().find(|y| matches!(y, Action::AskText { .. })) else { panic!() };
        assert!(*secret && prompt.contains("locked.pdf") && prompt.contains("needs a password"), "{prompt}");
        // A wrong password: asked again, in other words.
        app.event(Event::Text { tag: TAG_JOIN_PASSWORD, text: Some("nope".to_string()) });
        let again = pump_for(&mut app, "the box again", |_, x| asked_password(x));
        assert!(again.iter().any(|y| matches!(y, Action::AskText { prompt, .. } if prompt.contains("Try again"))), "{again:?}");
        // Cancelled: the file is left out, and one file is too few to merge.
        app.event(Event::Text { tag: TAG_JOIN_PASSWORD, text: None });
        let left = pump_for(&mut app, "the end", |x, _| !x.jn.busy());
        assert!(app.note.as_deref().is_some_and(|n| n.contains("at least 2")), "{:?} {left:?}", app.note);
        // The right password: both files are joined.
        app.event(Event::Command(CMD_MERGE));
        app.event(Event::FilesPicked(vec![a.clone(), locked.clone()]));
        app.event(list_answer(0, None, None));
        pump_for(&mut app, "the password box", |_, x| asked_password(x));
        app.event(Event::Text { tag: TAG_JOIN_PASSWORD, text: Some(password.to_string()) });
        pump_for(&mut app, "the question where to write", |_, x| x.iter().any(|y| matches!(y, Action::PickSavePath { .. })));
        let target = dir.join("both.pdf");
        app.event(Event::SavePicked(Some(target.clone())));
        pump_for(&mut app, "the answer", |_, x| x.iter().any(|y| matches!(y, Action::Confirm { .. } | Action::Message { .. })));
        assert_qpdf_accepts(&target, 1 + locked_pages);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pictures_dropped_on_the_window_fill_the_list_in_natural_order_and_become_a_pdf_that_qpdf_accepts() {
        let dir = folder("images");
        let names = ["p10.png", "p2.png", "p1.png"];
        for n in names {
            std::fs::write(dir.join(n), png()).unwrap();
        }
        let mut app = new_app();
        // Pictures only: the list box comes at once, in the natural order, with the page size to choose.
        let shown = app.event(Event::Drop(names.iter().map(|n| dir.join(n)).collect()));
        let forms: Vec<&FormSpec> = shown.iter().filter_map(|a| if let Action::AskForm(f) = a { Some(f) } else { None }).collect();
        let [form] = &forms[..] else { panic!("{shown:?}") };
        assert_eq!(app.jn.list.files, vec![dir.join("p1.png"), dir.join("p2.png"), dir.join("p10.png")]);
        assert_eq!(form.items.iter().filter(|i| i.kind == crate::ui::FormKind::Pick).count(), 2);
        // The picture is moved to the front, the page size is the picture's; OK asks where to write, in the folder of the first picture.
        app.event(list_answer(2, Some(1), Some(filelist::UP)));
        assert_eq!((app.jn.size, app.jn.list.chosen), (1, 1));
        app.event(list_answer(1, Some(1), None));
        let asked = pump_for(&mut app, "the question where to write", |_, x| x.iter().any(|y| matches!(y, Action::PickSavePath { .. })));
        let Some(Action::PickSavePath { name, .. }) = asked.iter().find(|y| matches!(y, Action::PickSavePath { .. })) else { panic!() };
        assert_eq!(name, &dir.join("p1.pdf").to_string_lossy().into_owned());
        let target = dir.join("album.pdf");
        app.event(Event::SavePicked(Some(target.clone())));
        pump_for(&mut app, "the answer", |_, x| x.iter().any(|y| matches!(y, Action::Confirm { .. } | Action::Message { .. })));
        assert_qpdf_accepts(&target, 3);
        // A picture that cannot be read is told, with its name.
        app.event(Event::Command(CMD_IMAGES_PDF));
        let bad = dir.join("bad.png");
        std::fs::write(&bad, b"not a picture").unwrap();
        app.event(Event::FilesPicked(vec![bad.clone()]));
        app.event(list_answer(0, Some(0), None));
        pump_for(&mut app, "the question where to write", |_, x| x.iter().any(|y| matches!(y, Action::PickSavePath { .. })));
        app.event(Event::SavePicked(Some(dir.join("never.pdf"))));
        let failed = pump_for(&mut app, "the answer", |_, x| x.iter().any(|y| matches!(y, Action::Message { .. })));
        assert!(failed.iter().any(|y| matches!(y, Action::Message { text, .. } if text.contains("bad.png"))), "{failed:?}");
        assert!(!dir.join("never.pdf").exists());
        // Files that are not all pictures are opened as before, not joined.
        let mixed = app.event(Event::Drop(vec![dir.join("p1.png"), dir.join("album.pdf")]));
        assert!(mixed.iter().all(|a| !matches!(a, Action::AskForm(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn esc_stops_the_work_and_the_thread_writes_nothing() {
        let dir = folder("join-esc");
        let (a, b) = (dir.join("a.pdf"), dir.join("b.pdf"));
        std::fs::write(&a, pdf(1, "a")).unwrap();
        std::fs::write(&b, pdf(1, "b")).unwrap();
        let mut app = new_app();
        app.event(Event::Command(CMD_MERGE));
        app.event(Event::FilesPicked(vec![a.clone(), b.clone()]));
        app.event(list_answer(0, None, None));
        pump_for(&mut app, "the question where to write", |_, x| x.iter().any(|y| matches!(y, Action::PickSavePath { .. })));
        // (Esc cannot reach the window while the Windows box is up; it stops what goes on after that.)
        let target = dir.join("never.pdf");
        app.event(Event::SavePicked(Some(target.clone())));
        app.event(Event::Key { vk: crate::ui::vk::ESCAPE, ctrl: false, shift: false });
        assert!(!app.jn.busy() && app.note.as_deref().is_some_and(|n| n.contains("Stopped")), "{:?}", app.note);
        std::thread::sleep(Duration::from_millis(300));
        app.event(Event::Wake);
        assert!(!target.exists() || Document::open(&target).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
