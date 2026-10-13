//! Recognising the text of scanned pages (5c): the part that knows nothing of Windows.
//!
//! The work is a chain, one page at a time (so that only one picture of a page is held): the engine finds the pages to do
//! (`Engine::ocr_scan`), draws one as grey pixels (`ocr_image`), the picture goes to the recogniser on a thread of its own
//! ([`Worker`]; the engine thread and the window never wait for it), the words that come back are kept, and when the last page is
//! done (or the person stops the run) all of them are given to the engine as one invisible text layer (`ocr_layer`), which is one step
//! to undo. [`Run`] is that chain as a state machine: the window feeds it the engine's events and the worker's replies and shows what
//! it says; the hidden `--ocr-test` of the program feeds it the same way with no window. The recogniser is a [`Recognizer`], which
//! `win32.rs` makes from the Windows Runtime (`Windows.Media.Ocr`); tests make one that reads nothing.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use qingpdf_core::view::{Engine, Event, MAX_OCR_PAGES, MAX_WORDS_PER_RUN, OcrLine, OcrPage, RenderStatus};

/// How long the recogniser may take for one page before the page is given up (the operation is cancelled and closed).
pub const PAGE_TIMEOUT: Duration = Duration::from_secs(30);

/// Why a page, or the recogniser, did not work out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OcrError {
    /// The system has no recogniser for the language wanted (`chinese`: it is Chinese that is missing) or for any language;
    /// `available` are the languages it has, as language tags (`en-US`).
    NoLanguage { chinese: bool, available: Vec<String> },
    /// The recogniser took longer than [`PAGE_TIMEOUT`].
    TimedOut,
    /// The person stopped it.
    Cancelled,
    /// It failed (the system's reason, or what was wrong with the picture).
    Failed(String),
}

/// A page as grey pixels, 8 bits, 0 black, rows from the top, no padding.
pub struct GrayImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Something that reads words off a picture. It lives on the worker's thread (it is made there and never leaves it).
pub trait Recognizer {
    /// The language tag it recognises (`zh-Hans-CN`).
    fn language(&self) -> String;
    /// The most pixels it takes on a side.
    fn max_dimension(&self) -> u32;
    /// Read `image`. The lines are in reading order, the boxes in pixels of the picture. `cancel` is looked at while it works;
    /// after `timeout` the work is given up ([`OcrError::TimedOut`]).
    fn recognize(&mut self, image: &GrayImage, cancel: &AtomicBool, timeout: Duration) -> Result<Vec<OcrLine>, OcrError>;
}

/// Makes the recogniser on the worker's thread.
pub type Opener = Box<dyn FnOnce() -> Result<Box<dyn Recognizer>, OcrError> + Send>;

/// What the worker says.
pub enum Reply {
    /// The recogniser is made: its language and the most pixels it takes on a side.
    Ready { language: String, max_dimension: u32 },
    /// It could not be made.
    NotReady(OcrError),
    /// A page is read, or could not be (`millis`: how long it took).
    Page { page: u32, result: Result<Vec<OcrLine>, OcrError>, millis: u64 },
}

struct Job {
    page: u32,
    image: GrayImage,
}

/// The thread that owns the recogniser. Its replies are taken with [`Worker::poll`]; `wake` is called from the thread after each
/// reply, to wake the window's loop. Dropping the worker stops it: the page being read is given up and the thread ends by itself
/// (nothing waits for it).
pub struct Worker {
    jobs: Sender<Job>,
    replies: Receiver<Reply>,
    cancel: Arc<AtomicBool>,
}

impl Worker {
    pub fn start(opener: Opener, timeout: Duration, wake: Arc<dyn Fn() + Send + Sync>) -> Result<Worker, String> {
        let (jobs, job_rx) = mpsc::channel::<Job>();
        let (reply_tx, replies) = mpsc::channel::<Reply>();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        std::thread::Builder::new()
            .name("qingpdf-ocr".to_string())
            .spawn(move || {
                let say = |reply: Reply| {
                    let _ = reply_tx.send(reply);
                    wake();
                };
                let mut recognizer = match opener() {
                    Ok(r) => r,
                    Err(e) => {
                        say(Reply::NotReady(e));
                        return;
                    }
                };
                say(Reply::Ready { language: recognizer.language(), max_dimension: recognizer.max_dimension() });
                // (The loop ends when the worker is dropped: its sender goes.)
                while let Ok(job) = job_rx.recv() {
                    if flag.load(Ordering::SeqCst) {
                        break;
                    }
                    let started = Instant::now();
                    let result = recognizer.recognize(&job.image, &flag, timeout);
                    drop(job.image);
                    say(Reply::Page { page: job.page, result, millis: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX) });
                }
                // The recogniser is dropped here, on the thread that made it.
            })
            .map(|_| Worker { jobs, replies, cancel })
            .map_err(|e| e.to_string())
    }

    /// Give the worker a page to read.
    pub fn submit(&self, page: u32, image: GrayImage) {
        let _ = self.jobs.send(Job { page, image });
    }

    pub fn poll(&self) -> Option<Reply> {
        self.replies.try_recv().ok()
    }

    pub fn wait(&self, timeout: Duration) -> Option<Reply> {
        self.replies.recv_timeout(timeout).ok()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

// --- the chain ------------------------------------------------------------------------------------------------------------

/// Which pages to recognise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Every page with no text that is mostly pictures.
    Scans,
    /// This page (from 0), if it has no text.
    Page(u32),
    /// The scans among pages `first` to `last` (from 0).
    Range(u32, u32),
}

/// What a run has done, shown while it goes on and said at the end.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    /// Pages found to do, pages done, words and characters recognised.
    pub pages_found: u32,
    pub pages_done: u32,
    pub words: u64,
    pub chars: u64,
    /// The language used.
    pub language: String,
    /// Pages given up, with the reason (page from 0, what was wrong).
    pub skipped: Vec<(u32, String)>,
    /// Pages that were recognised from a picture that is not the whole page (the page stopped drawing early), with the reason; the
    /// words of what was drawn are kept, and some of the page's text may be missing.
    pub incomplete: Vec<(u32, String)>,
    /// Pages that were found but not done because there were more than a run does.
    pub too_many: bool,
    /// What the engine said about the layer it wrote (a note, or why it could not).
    pub layer_note: String,
    /// The run was stopped by the person; the pages done so far were given to the engine.
    pub stopped: bool,
    /// For each page read: the page (from 0), the milliseconds from asking for its picture to its words, the milliseconds the
    /// recogniser took, and the words found.
    pub page_log: Vec<(u32, u64, u64, usize)>,
}

/// What a step of a run comes to.
#[derive(Debug, PartialEq)]
pub enum Progress {
    /// Nothing for the person to be told.
    Quiet,
    /// The run is going on: this page (the position among the pages found, from 1) of that many.
    Working { position: u32, total: u32 },
    /// The scan of the file found no page to recognise (`looked` pages were looked at).
    NothingToDo { looked: u32 },
    /// The recogniser could not be made.
    NoRecogniser(OcrError),
    /// The words are given to the engine: waiting for its answer.
    Writing,
    /// The run is over. The layer was written (`Ok`) or not (`Err`, with why; the pages' words were lost).
    Finished(Result<(), String>),
    /// The run failed as a whole (the engine could not draw or the like).
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    /// Waiting for the list of pages.
    Scanning,
    /// Waiting for the recogniser to be made.
    Starting,
    /// Waiting for the picture of a page.
    Drawing,
    /// Waiting for the recogniser to read a page.
    Reading,
    /// Waiting for the engine to write the layer.
    Writing,
    Over,
}

/// One run of recognition over a document.
pub struct Run {
    doc: u64,
    phase: Phase,
    /// The request being waited for.
    waiting: u64,
    queue: VecDeque<u32>,
    /// The page being drawn or read.
    current: Option<u32>,
    results: Vec<OcrPage>,
    pub summary: Summary,
    worker: Option<Worker>,
    max_side: u32,
    /// The size of the picture of the page being read (the answer carries boxes in its pixels).
    size: (u32, u32),
    timeout: Duration,
    wake: Arc<dyn Fn() + Send + Sync>,
    opener: Option<Opener>,
    scope: Scope,
    pub started: Instant,
    /// When the picture of the page in hand was asked for.
    page_started: Instant,
}

impl Run {
    /// Begin: ask the engine for the pages to do. `next_id` gives the numbers of the requests.
    pub fn start(engine: &Engine, doc: u64, scope: Scope, opener: Opener, timeout: Duration, wake: Arc<dyn Fn() + Send + Sync>, next_id: &mut dyn FnMut() -> u64) -> Run {
        let id = next_id();
        match scope {
            Scope::Scans => engine.ocr_scan(doc, id, None),
            Scope::Page(p) => engine.ocr_scan(doc, id, Some(p)),
            Scope::Range(..) => engine.ocr_scan(doc, id, None),
        }
        Run {
            doc,
            phase: Phase::Scanning,
            waiting: id,
            queue: VecDeque::new(),
            current: None,
            results: Vec::new(),
            summary: Summary::default(),
            worker: None,
            max_side: 0,
            size: (0, 0),
            timeout,
            wake,
            opener: Some(opener),
            scope,
            started: Instant::now(),
            page_started: Instant::now(),
        }
    }

    pub fn is_over(&self) -> bool {
        self.phase == Phase::Over
    }

    /// The id of the engine request the run is waiting for, if it is waiting for one (the window cancels it when the run is stopped).
    pub fn waiting_for(&self) -> Option<u64> {
        matches!(self.phase, Phase::Scanning | Phase::Drawing).then_some(self.waiting)
    }

    fn working(&self) -> Progress {
        Progress::Working { position: (self.summary.pages_done + u32::try_from(self.summary.skipped.len()).unwrap_or(u32::MAX) + 1).min(self.summary.pages_found), total: self.summary.pages_found }
    }

    /// An event of the engine (taken by value: the picture goes on to the worker without being copied). Events that are not the run's
    /// are ignored.
    pub fn on_event(&mut self, engine: &Engine, event: Event, next_id: &mut dyn FnMut() -> u64) -> Progress {
        match event {
            Event::OcrPages { doc, id, pages, looked, truncated, .. } if doc == self.doc && id == self.waiting && self.phase == Phase::Scanning => {
                let mut pages = pages;
                if let Scope::Range(first, last) = self.scope {
                    pages.retain(|p| (first..=last).contains(p));
                }
                self.summary.too_many = truncated;
                self.summary.pages_found = u32::try_from(pages.len()).unwrap_or(u32::MAX);
                if pages.is_empty() {
                    self.phase = Phase::Over;
                    return Progress::NothingToDo { looked };
                }
                self.queue = pages.into();
                let Some(opener) = self.opener.take() else { return Progress::Failed("the recogniser was started twice".to_string()) };
                match Worker::start(opener, self.timeout, self.wake.clone()) {
                    Ok(w) => self.worker = Some(w),
                    Err(e) => {
                        self.phase = Phase::Over;
                        return Progress::Failed(e);
                    }
                }
                self.phase = Phase::Starting;
                Progress::Quiet
            }
            Event::OcrImage { doc, id, page, status, message, width, height, gray, .. } if doc == self.doc && id == self.waiting && self.phase == Phase::Drawing => {
                if status == RenderStatus::Failed || width == 0 || gray.len() != width as usize * height as usize {
                    self.summary.skipped.push((page, if message.is_empty() { "the page could not be drawn".to_string() } else { message }));
                    self.current = None;
                    return self.next_page(engine, next_id);
                }
                if status == RenderStatus::Incomplete {
                    self.summary.incomplete.push((page, if message.is_empty() { "the page could not be drawn whole".to_string() } else { message }));
                }
                self.size = (width, height);
                self.phase = Phase::Reading;
                if let Some(w) = &self.worker {
                    w.submit(page, GrayImage { width, height, pixels: gray });
                }
                self.working()
            }
            _ => Progress::Quiet,
        }
    }

    /// The engine answered the request to write the layer (an edit: `done` or not, with its message).
    pub fn on_layer(&mut self, id: u64, done: bool, message: &str) -> Progress {
        if self.phase != Phase::Writing || id != self.waiting {
            return Progress::Quiet;
        }
        self.phase = Phase::Over;
        self.summary.layer_note = message.to_string();
        Progress::Finished(if done { Ok(()) } else { Err(message.to_string()) })
    }

    /// Is `id` the request to write the layer?
    pub fn is_layer_request(&self, id: u64) -> bool {
        self.phase == Phase::Writing && id == self.waiting
    }

    /// Take what the worker has said and carry on; the progress of each reply, in order.
    pub fn poll_worker(&mut self, engine: &Engine, next_id: &mut dyn FnMut() -> u64) -> Vec<Progress> {
        let mut out = Vec::new();
        while let Some(reply) = self.worker.as_ref().and_then(Worker::poll) {
            out.push(self.on_reply(engine, reply, next_id));
        }
        out
    }

    /// For programs with no window (the hidden test, the tests): wait a moment for the engine, take what it and the worker have said,
    /// and carry on; the progress of each step.
    pub fn pump(&mut self, engine: &Engine, next_id: &mut dyn FnMut() -> u64) -> Vec<Progress> {
        let mut progress = Vec::new();
        if let Some(event) = engine.wait(5) {
            if let Event::Edited(r) = &event {
                progress.push(self.on_layer(r.id, r.status == qingpdf_core::view::EditStatus::Done, &r.message));
            } else {
                progress.push(self.on_event(engine, event, next_id));
            }
        }
        progress.extend(self.poll_worker(engine, next_id));
        progress
    }

    /// A reply of the worker.
    pub fn on_reply(&mut self, engine: &Engine, reply: Reply, next_id: &mut dyn FnMut() -> u64) -> Progress {
        match reply {
            Reply::NotReady(e) => {
                self.phase = Phase::Over;
                Progress::NoRecogniser(e)
            }
            Reply::Ready { language, max_dimension } => {
                self.summary.language = language;
                self.max_side = max_dimension;
                self.next_page(engine, next_id)
            }
            Reply::Page { page, result, millis } => {
                if self.phase != Phase::Reading || self.current != Some(page) {
                    return Progress::Quiet;
                }
                self.current = None;
                let total = u64::try_from(self.page_started.elapsed().as_millis()).unwrap_or(u64::MAX);
                match result {
                    Ok(lines) => {
                        let o = OcrPage { page, width: self.size.0, height: self.size.1, lines };
                        let words = o.word_count();
                        self.summary.page_log.push((page, total, millis, words));
                        let total_words: usize = self.results.iter().map(OcrPage::word_count).sum();
                        if total_words + words > MAX_WORDS_PER_RUN {
                            self.summary.too_many = true;
                            self.queue.clear();
                        } else {
                            self.summary.pages_done += 1;
                            self.summary.words += words as u64;
                            self.summary.chars += o.lines.iter().flat_map(|l| &l.words).map(|w| w.text.chars().count() as u64).sum::<u64>();
                            if words > 0 {
                                self.results.push(o);
                            }
                        }
                    }
                    Err(OcrError::Cancelled) => {}
                    Err(e) => self.summary.skipped.push((page, describe(&e))),
                }
                self.next_page(engine, next_id)
            }
        }
    }

    /// Ask for the next page's picture, or, when there is none left, write the layer.
    fn next_page(&mut self, engine: &Engine, next_id: &mut dyn FnMut() -> u64) -> Progress {
        if let Some(page) = self.queue.pop_front() {
            let id = next_id();
            engine.ocr_image(self.doc, id, page, self.max_side);
            self.page_started = Instant::now();
            self.waiting = id;
            self.current = Some(page);
            self.phase = Phase::Drawing;
            return self.working();
        }
        self.finish(engine, next_id)
    }

    /// No more pages: give the engine what was recognised.
    fn finish(&mut self, engine: &Engine, next_id: &mut dyn FnMut() -> u64) -> Progress {
        self.worker = None;
        if self.results.is_empty() {
            self.phase = Phase::Over;
            return Progress::Finished(Ok(()));
        }
        let id = next_id();
        engine.ocr_layer(self.doc, id, std::mem::take(&mut self.results));
        self.waiting = id;
        self.phase = Phase::Writing;
        Progress::Writing
    }

    /// The person stopped the run: the page being drawn or read is given up, and the pages done are written.
    pub fn stop(&mut self, engine: &Engine, next_id: &mut dyn FnMut() -> u64) -> Progress {
        self.summary.stopped = true;
        match self.phase {
            Phase::Over | Phase::Writing => return Progress::Quiet,
            Phase::Scanning => {
                engine.cancel(self.waiting);
                self.phase = Phase::Over;
                return Progress::Finished(Ok(()));
            }
            Phase::Drawing => engine.cancel(self.waiting),
            Phase::Starting | Phase::Reading => {}
        }
        if let Some(w) = &self.worker {
            w.cancel.store(true, Ordering::SeqCst);
        }
        self.queue.clear();
        self.current = None;
        self.finish(engine, next_id)
    }
}

/// Words for a page that was given up.
pub fn describe(e: &OcrError) -> String {
    match e {
        OcrError::TimedOut => format!("it took more than {} seconds", PAGE_TIMEOUT.as_secs()),
        OcrError::Cancelled => "stopped".to_string(),
        OcrError::Failed(why) => why.clone(),
        OcrError::NoLanguage { .. } => "no recogniser".to_string(),
    }
}

/// Most pages a run does (the engine's limit).
pub const MAX_PAGES: usize = MAX_OCR_PAGES;

// --- the hidden speed and accuracy test --------------------------------------------------------------------------------------

/// `qingpdf-view --ocr-test in.pdf out.pdf [first-last]`: recognise the scans among the pages (all of them, or `first` to `last`,
/// from 1) with the same chain the window uses, save the file with the text layer, and print how it went. `chinese`: the Chinese
/// recogniser (else the languages of the person). The exit code is 0 when the file was written, 1 when there was nothing to
/// recognise, 2 for a file that did not open or save, 3 when there is no recogniser.
pub fn ocr_test(input: &str, output: &str, pages: Option<(u32, u32)>, chinese: bool) -> i32 {
    use qingpdf_core::view::TOTAL_BYTES;
    let engine = Engine::start(Box::new(|| {}));
    engine.open_file(1, input, "", TOTAL_BYTES, 1920 * 1080);
    loop {
        match engine.wait(60_000) {
            Some(Event::Opened(o)) => {
                println!("opened {input}: {} pages", o.pages.len());
                break;
            }
            Some(Event::OpenFailed { failure, message, .. }) => {
                println!("not opened: {failure:?} {message}");
                return 2;
            }
            Some(_) => {}
            None => {
                println!("not opened: the engine said nothing for a minute");
                return 2;
            }
        }
    }
    let mut counter = 100u64;
    let mut next_id = move || {
        counter += 1;
        counter
    };
    let scope = pages.map_or(Scope::Scans, |(a, b)| Scope::Range(a.saturating_sub(1), b.saturating_sub(1)));
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
    let mut run = Run::start(&engine, 1, scope, Box::new(move || crate::win32::open_recognizer(chinese)), PAGE_TIMEOUT, wake, &mut next_id);
    let finished = loop {
        let progress = run.pump(&engine, &mut next_id);
        let mut result = None;
        for p in progress {
            match p {
                Progress::Working { position, total } => println!("page {position}/{total} ({:.1} s)", run.started.elapsed().as_secs_f64()),
                Progress::NothingToDo { looked } => {
                    println!("nothing to recognise ({looked} pages looked at)");
                    result = Some(1);
                }
                Progress::NoRecogniser(e) => {
                    println!("no recogniser: {e:?}");
                    result = Some(3);
                }
                Progress::Failed(why) => {
                    println!("failed: {why}");
                    result = Some(2);
                }
                Progress::Finished(Err(why)) => {
                    println!("the text layer was not written: {why}");
                    result = Some(2);
                }
                Progress::Finished(Ok(())) => result = Some(0),
                Progress::Writing | Progress::Quiet => {}
            }
        }
        if let Some(code) = result {
            break code;
        }
        if run.started.elapsed() > Duration::from_secs(3_600) {
            println!("gave up after an hour");
            break 2;
        }
    };
    let s = &run.summary;
    println!("language {}, {} pages found, {} done, {} words, {} characters, {:.2} s in all", s.language, s.pages_found, s.pages_done, s.words, s.chars, run.started.elapsed().as_secs_f64());
    for (page, total, ocr, words) in &s.page_log {
        println!("  page {}: {:.2} s (recogniser {:.2} s), {words} words", page + 1, *total as f64 / 1000.0, *ocr as f64 / 1000.0);
    }
    for (page, why) in &s.skipped {
        println!("  page {} skipped: {why}", page + 1);
    }
    for (page, why) in &s.incomplete {
        println!("  page {} was drawn only in part: {why}", page + 1);
    }
    if !s.layer_note.is_empty() {
        println!("note: {}", s.layer_note);
    }
    if finished != 0 {
        return finished;
    }
    let id = next_id();
    engine.save(1, id, output);
    loop {
        match engine.wait(60_000) {
            Some(Event::Saved { id: got, status, message, bytes, .. }) if got == id => {
                println!("saved {output}: {status:?} {bytes} bytes {message}");
                break;
            }
            Some(_) => {}
            None => {
                println!("saving: the engine said nothing for a minute");
                return 2;
            }
        }
    }
    println!("memory peak {:.1} MB", crate::win32::peak_working_set() as f64 / 1_048_576.0);
    0
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use qingpdf_core::view::{OcrWord, TOTAL_BYTES};
    use std::path::PathBuf;

    /// A file of A6 pages (298 by 420 points): each is a picture over the whole page, and the ones in `with_text` have a line of text too.
    pub(crate) fn scan_file(name: &str, count: usize, with_text: &[usize]) -> PathBuf {
        let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        let mut add = |out: &mut Vec<u8>, n: usize, body: String| {
            offsets.push((n, out.len()));
            out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
        };
        let kids: String = (0..count).map(|i| format!("{} 0 R ", 10 + i)).collect();
        add(&mut out, 1, "<< /Type /Catalog /Pages 2 0 R >>".to_string());
        add(&mut out, 2, format!("<< /Type /Pages /Kids [{kids}] /Count {count} /MediaBox [0 0 298 420] >>"));
        let hex = format!("{}>", "FF".repeat(64));
        add(&mut out, 3, format!("<< /Type /XObject /Subtype /Image /Width 8 /Height 8 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /ASCIIHexDecode /Length {} >>\nstream\n{hex}\nendstream", hex.len()));
        add(&mut out, 4, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string());
        for i in 0..count {
            let text = if with_text.contains(&i) { "BT /F 12 Tf 20 300 Td (There is text on this page already) Tj ET" } else { "" };
            let body = format!("q 298 0 0 420 0 0 cm /Im Do Q {text}");
            add(&mut out, 100 + i, format!("<< /Length {} >>\nstream\n{body}\nendstream", body.len()));
            add(&mut out, 10 + i, format!("<< /Type /Page /Parent 2 0 R /Resources << /XObject << /Im 3 0 R >> /Font << /F 4 0 R >> >> /Contents {} 0 R >>", 100 + i));
        }
        let at = out.len();
        let size = 100 + count;
        out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        for n in 0..size {
            match offsets.iter().find(|(m, _)| *m == n) {
                Some((_, o)) => out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes()),
                None => out.extend_from_slice(b"0000000000 65535 f \n"),
            }
        }
        out.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{at}\n%%EOF\n").as_bytes());
        let dir = std::env::temp_dir().join(format!("qingpdf-view-ocr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, out).unwrap();
        path
    }

    /// A recogniser that reads nothing off the picture: it says what it is told for each page (the pages come in order, from 0).
    pub(crate) struct Fake {
        pub max: u32,
        pub says: Box<dyn FnMut(usize) -> Result<Vec<OcrLine>, OcrError> + Send>,
        pub calls: usize,
    }

    impl Recognizer for Fake {
        fn language(&self) -> String {
            "xx-XX".to_string()
        }
        fn max_dimension(&self) -> u32 {
            self.max
        }
        fn recognize(&mut self, image: &GrayImage, _cancel: &AtomicBool, _timeout: Duration) -> Result<Vec<OcrLine>, OcrError> {
            assert_eq!(image.pixels.len(), image.width as usize * image.height as usize);
            assert!(image.width <= self.max && image.height <= self.max, "{} x {}", image.width, image.height);
            let n = self.calls;
            self.calls += 1;
            (self.says)(n)
        }
    }

    pub(crate) fn fake(max: u32, says: impl FnMut(usize) -> Result<Vec<OcrLine>, OcrError> + Send + 'static) -> Opener {
        Box::new(move || Ok(Box::new(Fake { max, says: Box::new(says), calls: 0 }) as Box<dyn Recognizer>))
    }

    /// A line "Hello number N" as a recogniser would find it on a picture of 1000 pixels across.
    fn line(n: usize) -> Vec<OcrLine> {
        let w = |t: &str, l: f32, r: f32| OcrWord { text: t.to_string(), rect: [l, 100.0, r, 140.0] };
        vec![OcrLine { words: vec![w("Hello", 100.0, 300.0), w("number", 320.0, 560.0), w(&n.to_string(), 580.0, 640.0)] }]
    }

    fn open(path: &std::path::Path) -> Engine {
        let engine = Engine::start(Box::new(|| {}));
        engine.open_file(1, &path.to_string_lossy(), "", TOTAL_BYTES, 1920 * 1080);
        match engine.wait(20_000) {
            Some(Event::Opened(_)) => engine,
            other => panic!("not opened: {other:?}"),
        }
    }

    /// Run `run` to its end; everything it said that is not `Quiet`.
    fn drive(engine: &Engine, run: &mut Run, ids: &mut dyn FnMut() -> u64) -> Vec<Progress> {
        let mut said = Vec::new();
        let started = Instant::now();
        while !run.is_over() {
            assert!(started.elapsed() < Duration::from_secs(60), "the run did not end: {said:?}");
            said.extend(run.pump(engine, ids).into_iter().filter(|p| *p != Progress::Quiet));
        }
        said
    }

    fn start(engine: &Engine, scope: Scope, opener: Opener, ids: &mut dyn FnMut() -> u64) -> Run {
        Run::start(engine, 1, scope, opener, PAGE_TIMEOUT, Arc::new(|| {}), ids)
    }

    /// The text of a page as the engine gives it.
    fn text_of(engine: &Engine, page: u32) -> String {
        let id = 9_000 + u64::from(page);
        engine.copy_text(1, id, (page, 0), (page, 10_000));
        loop {
            match engine.wait(20_000) {
                Some(Event::Copied { id: i, text, .. }) if i == id => return text,
                Some(_) => {}
                None => panic!("no answer"),
            }
        }
    }

    #[test]
    fn a_run_recognises_the_scans_one_page_at_a_time_and_writes_one_layer() {
        let file = scan_file("run.pdf", 4, &[1]);
        let engine = open(&file);
        let mut counter = 100u64;
        let mut ids = move || {
            counter += 1;
            counter
        };
        let mut run = start(&engine, Scope::Scans, fake(10_000, |n| Ok(line(n))), &mut ids);
        let said = drive(&engine, &mut run, &mut ids);
        assert_eq!(said.last(), Some(&Progress::Finished(Ok(()))), "{said:?}");
        assert!(said.contains(&Progress::Working { position: 1, total: 3 }) && said.contains(&Progress::Working { position: 3, total: 3 }) && said.contains(&Progress::Writing), "{said:?}");
        let s = &run.summary;
        assert_eq!((s.pages_found, s.pages_done, s.words, s.chars), (3, 3, 9, 36), "{s:?}");
        assert!(s.skipped.is_empty() && s.language == "xx-XX" && s.page_log.len() == 3);
        // The scans have the text, the page that had text has what it had.
        assert_eq!(text_of(&engine, 0), "Hello number 0\n");
        assert_eq!(text_of(&engine, 2), "Hello number 1\n");
        assert_eq!(text_of(&engine, 3), "Hello number 2\n");
        assert!(text_of(&engine, 1).starts_with("There is text"));
        std::fs::remove_file(&file).ok();
    }

    #[test]
    fn a_page_for_itself_a_range_and_a_file_with_nothing_to_do() {
        let file = scan_file("scope.pdf", 4, &[0, 3]);
        let engine = open(&file);
        let mut counter = 100u64;
        let mut ids = move || {
            counter += 1;
            counter
        };
        let mut run = start(&engine, Scope::Page(2), fake(10_000, |n| Ok(line(n))), &mut ids);
        assert_eq!(drive(&engine, &mut run, &mut ids).last(), Some(&Progress::Finished(Ok(()))));
        assert_eq!(run.summary.pages_found, 1);
        // Page 1 (from 0) has no text: of the range 0 to 1 only it is done.
        let mut run = start(&engine, Scope::Range(0, 1), fake(10_000, |_| Ok(line(7))), &mut ids);
        drive(&engine, &mut run, &mut ids);
        assert_eq!(run.summary.pages_found, 1);
        assert_eq!(text_of(&engine, 1), "Hello number 7\n");
        // Nothing left that is a scan in the range 0 to 0.
        let mut run = start(&engine, Scope::Page(0), fake(10_000, |_| Ok(line(1))), &mut ids);
        assert!(matches!(drive(&engine, &mut run, &mut ids).last(), Some(Progress::NothingToDo { .. })));
        std::fs::remove_file(&file).ok();
    }

    #[test]
    fn a_page_that_was_drawn_only_in_part_is_still_recognised_and_is_reported_with_the_reason() {
        let file = scan_file("partial.pdf", 2, &[]);
        let engine = open(&file);
        let mut counter = 100u64;
        let mut ids = move || {
            counter += 1;
            counter
        };
        let mut run = start(&engine, Scope::Page(0), fake(10_000, |n| Ok(line(n))), &mut ids);
        // The engine's answers are made up here (the real ones that come later are not for the step the run is in, and are ignored),
        // so that the picture of the page is an incomplete one.
        let found = Event::OcrPages { doc: 1, id: run.waiting, pages: vec![0], looked: 1, unreadable: 0, truncated: false };
        run.on_event(&engine, found, &mut ids);
        let began = Instant::now();
        while run.phase != Phase::Drawing {
            assert!(began.elapsed() < Duration::from_secs(20), "the recogniser was not made");
            run.poll_worker(&engine, &mut ids);
            std::thread::sleep(Duration::from_millis(5));
        }
        let why = "the page asks for more work than is allowed";
        let picture = Event::OcrImage { doc: 1, id: run.waiting, page: 0, status: RenderStatus::Incomplete, message: why.to_string(), width: 1000, height: 1400, dpi: 300.0, gray: vec![255; 1000 * 1400] };
        run.on_event(&engine, picture, &mut ids);
        let said = drive(&engine, &mut run, &mut ids);
        assert_eq!(said.last(), Some(&Progress::Finished(Ok(()))), "{said:?}");
        let s = &run.summary;
        assert_eq!((s.pages_done, s.skipped.len()), (1, 0), "{s:?}");
        assert_eq!(s.incomplete, vec![(0, why.to_string())]);
        assert!(text_of(&engine, 0).contains("Hello number 0"), "the words of what was drawn are kept");
    }

    #[test]
    fn a_page_the_recogniser_fails_on_or_is_too_slow_for_is_skipped_and_the_rest_is_done() {
        let file = scan_file("skips.pdf", 4, &[]);
        let engine = open(&file);
        let mut counter = 100u64;
        let mut ids = move || {
            counter += 1;
            counter
        };
        let says = |n: usize| match n {
            1 => Err(OcrError::TimedOut),
            2 => Err(OcrError::Failed("broken".to_string())),
            _ => Ok(line(n)),
        };
        let mut run = start(&engine, Scope::Scans, fake(10_000, says), &mut ids);
        assert_eq!(drive(&engine, &mut run, &mut ids).last(), Some(&Progress::Finished(Ok(()))));
        let s = &run.summary;
        assert_eq!((s.pages_done, s.skipped.len()), (2, 2), "{s:?}");
        assert_eq!(s.skipped[0].0, 1);
        assert!(s.skipped[0].1.contains("30 seconds") && s.skipped[1].1 == "broken", "{:?}", s.skipped);
        assert_eq!(text_of(&engine, 0), "Hello number 0\n");
        assert_eq!(text_of(&engine, 1), "");
        assert_eq!(text_of(&engine, 3), "Hello number 3\n");
        std::fs::remove_file(&file).ok();
    }

    #[test]
    fn stopping_keeps_the_pages_done_and_a_missing_recogniser_is_said() {
        let file = scan_file("stop.pdf", 5, &[]);
        let engine = open(&file);
        let mut counter = 100u64;
        let mut ids = move || {
            counter += 1;
            counter
        };
        let mut run = start(&engine, Scope::Scans, fake(10_000, |n| Ok(line(n))), &mut ids);
        // Go on until two pages are done, then stop.
        let started = Instant::now();
        while run.summary.pages_done < 2 {
            assert!(started.elapsed() < Duration::from_secs(60));
            run.pump(&engine, &mut ids);
        }
        let said = run.stop(&engine, &mut ids);
        assert_eq!(said, Progress::Writing);
        let said = drive(&engine, &mut run, &mut ids);
        assert_eq!(said.last(), Some(&Progress::Finished(Ok(()))));
        assert!(run.summary.stopped && run.summary.pages_done >= 2 && run.summary.pages_done < 5, "{:?}", run.summary);
        assert_eq!(text_of(&engine, 0), "Hello number 0\n");
        assert_eq!(text_of(&engine, 4), "");
        // No recogniser: the run says why and ends.
        let none: Opener = Box::new(|| Err(OcrError::NoLanguage { chinese: true, available: vec!["en-US".to_string()] }));
        let mut run = start(&engine, Scope::Scans, none, &mut ids);
        let said = drive(&engine, &mut run, &mut ids);
        assert_eq!(said, vec![Progress::NoRecogniser(OcrError::NoLanguage { chinese: true, available: vec!["en-US".to_string()] })]);
        std::fs::remove_file(&file).ok();
    }

    #[test]
    fn the_size_the_recogniser_takes_decides_the_size_of_the_picture() {
        let file = scan_file("size.pdf", 1, &[]);
        let engine = open(&file);
        let mut counter = 100u64;
        let mut ids = move || {
            counter += 1;
            counter
        };
        // (The fake asserts the picture is within what it takes.) 420 points at 300 dpi is 1750 pixels; this one takes 800.
        let mut run = start(&engine, Scope::Scans, fake(800, |n| Ok(line(n))), &mut ids);
        assert_eq!(drive(&engine, &mut run, &mut ids).last(), Some(&Progress::Finished(Ok(()))));
        assert_eq!(run.summary.pages_done, 1);
        std::fs::remove_file(&file).ok();
    }

    /// The real recogniser of Windows, where it is there: it reads a page that our own renderer drew from text in a PDF.
    #[test]
    fn windows_reads_a_page_drawn_by_the_engine() {
        let Ok(mut recognizer) = crate::win32::open_recognizer(false) else { return };
        let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        let body = "BT /F 28 Tf 40 300 Td (Hello World 2026) Tj 0 -50 Td (The quick brown fox) Tj ET";
        for (n, text) in [
            (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 420 400] >>".to_string()),
            (3, "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F 4 0 R >> >> /Contents 5 0 R >>".to_string()),
            (4, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string()),
            (5, format!("<< /Length {} >>\nstream\n{body}\nendstream", body.len())),
        ] {
            offsets.push(out.len());
            out.extend_from_slice(format!("{n} 0 obj\n{text}\nendobj\n").as_bytes());
        }
        let at = out.len();
        out.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
        for o in &offsets {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{at}\n%%EOF\n").as_bytes());
        let path = std::env::temp_dir().join(format!("qingpdf-view-win-ocr-{}.pdf", std::process::id()));
        std::fs::write(&path, out).unwrap();
        let engine = open(&path);
        engine.ocr_image(1, 77, 0, recognizer.max_dimension());
        let image = loop {
            match engine.wait(20_000) {
                Some(Event::OcrImage { id: 77, width, height, gray, status, .. }) => {
                    assert_ne!(status, RenderStatus::Failed);
                    break GrayImage { width, height, pixels: gray };
                }
                Some(_) => {}
                None => panic!("no picture"),
            }
        };
        let lines = recognizer.recognize(&image, &AtomicBool::new(false), PAGE_TIMEOUT).unwrap();
        let text: Vec<String> = lines.iter().map(|l| l.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")).collect();
        assert_eq!(text, ["Hello World 2026", "The quick brown fox"], "{lines:?}");
        // The boxes are in pixels of the picture, where the words are: the first line is at 40 points from the left and 300 up of 400.
        let first = &lines[0].words[0].rect;
        let px = f64::from(image.width) / 420.0;
        assert!((f64::from(first[0]) - 40.0 * px).abs() < 20.0 && (f64::from(first[3]) - 100.0 * px).abs() < 12.0, "{first:?} at {px} pixels a point");
        // A picture that is cancelled, or a size it does not take, is an error and not a hang.
        let started = Instant::now();
        assert_eq!(recognizer.recognize(&image, &AtomicBool::new(true), PAGE_TIMEOUT), Err(OcrError::Cancelled));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(recognizer.recognize(&GrayImage { width: 0, height: 0, pixels: Vec::new() }, &AtomicBool::new(false), PAGE_TIMEOUT).is_err());
        let slow = recognizer.recognize(&image, &AtomicBool::new(false), Duration::from_millis(0));
        assert!(matches!(slow, Err(OcrError::TimedOut)) || slow.is_ok(), "{slow:?}");
        std::fs::remove_file(&path).ok();
    }
}
