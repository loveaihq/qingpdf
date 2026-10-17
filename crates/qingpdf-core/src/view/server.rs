//! The engine thread's serving loop: the one open document, the requests for pieces of pages to draw, and the other
//! requests (bookmarks, links, boxes of characters, search, copy, print).
//!
//! What is done first: the quick requests a person waits for (bookmarks, links, boxes of characters), then the pieces to
//! draw (the most urgent first), and only when nothing else is waiting one step of a long job (a page of a search, of a
//! copy, a band of a print). Every step of those is one page, charged to a work meter of its own that is cancelled by the
//! same flag as the drawing, so a hostile page cannot hold the thread for long.

use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use crate::idmap::IdMap;
use crate::document::{Document, Page};
use crate::edit::{self, EditClass, signature::Policy};
use crate::error::Error;
use crate::object::Object;
use crate::ops;
use crate::render::work::{Work, cost};
use crate::render::{Bitmap, MAX_DPI, MIN_DPI, MemoryLimits, Renderer};
use crate::text::{PageChars, TextExtractor};

use super::editing::{EditStatus, SaveStatus, Session, write_atomic};
use super::forms;
use super::targets::{Names, page_index};
use super::textcache::TextCache;
use super::{
    BACKGROUND_PRIORITY, CharBoxes, Command, Event, ListedAnnot, LOW_QUALITY_PRINT_DPI, OpenDoc, OpenFailure, Opened, PageInfo, PrintBand, PrintPage, PrintStatus, ProtectChange, ProtectJob, RenderRequest,
    MemoryPlan, RenderStatus, Rendered, Request, Rights, SearchHit, SearchStatus, Shared, TextStatus, annots, links, outline, page_info, reopen, search,
};
use crate::view::geometry::line_rects;

/// Most places one search reports; it stops there.
pub const MAX_SEARCH_HITS: usize = 10_000;
/// Most places reported from one page.
pub const MAX_HITS_PER_PAGE: usize = 2_000;
/// Most pages one copy reads, and most characters it returns.
pub const MAX_COPY_PAGES: usize = 100;
pub const MAX_COPY_CHARS: usize = 1_000_000;
/// Most pages one print job holds.
pub const MAX_PRINT_PAGES: usize = 100_000;
/// The most dots per inch a page is printed at.
pub const MAX_PRINT_DPI: f64 = 300.0;
/// How often a search says how far it is.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

struct SearchJob {
    id: u64,
    needle: Vec<char>,
    start: u32,
    done: u32,
    hits: u32,
    skipped: u32,
    announced: Instant,
}

struct CopyJob {
    id: u64,
    from: (u32, u32),
    to: (u32, u32),
    /// The next page to read.
    page: u32,
    pages_done: u32,
    text: String,
    chars: usize,
    skipped: u32,
    truncated: bool,
}

struct OcrScanJob {
    id: u64,
    only: Option<u32>,
    /// The next page to look at.
    next: u32,
    found: Vec<u32>,
    looked: u32,
    unreadable: u32,
    truncated: bool,
}

struct OcrImageJob {
    id: u64,
    page: u32,
    max_side: u32,
    /// The picture's size in pixels and its resolution, once worked out; the grey pixels drawn so far (a band at a time) and the next row.
    size: Option<(u32, u32, f64)>,
    y: u32,
    gray: Vec<u8>,
    /// Why the page was not drawn whole, if it was not.
    incomplete: String,
}

struct AnnotListJob {
    id: u64,
    /// The next page to read.
    next: u32,
    items: Vec<ListedAnnot>,
    /// A page had more annotations than are listed, the list is full, or the pages together had too many entries to look at.
    truncated: bool,
    /// The entries of the pages' `/Annots` looked at so far.
    visits: usize,
}

struct PrintJob {
    id: u64,
    pages: Vec<PrintPage>,
    /// The page being printed (an index into `pages`) and the next row of it to draw.
    at: usize,
    y: u32,
    /// A band has been sent and is not yet used.
    waiting: bool,
    checked: bool,
    printed: u32,
}

enum Job {
    Outline { id: u64 },
    Links { id: u64, page: u32 },
    CharBoxes { id: u64, page: u32 },
    Annotations { id: u64, page: u32 },
    AnnotText { id: u64, page: u32, index: u32, num: u32 },
    Fields { id: u64, page: u32 },
    Options { id: u64, page: u32, index: u32, num: u32 },
    TextLine { id: u64, page: u32, at: [f32; 2] },
    AnnotList(AnnotListJob),
    Search(SearchJob),
    Copy(CopyJob),
    Print(PrintJob),
    OcrScan(OcrScanJob),
    OcrImage(OcrImageJob),
}

impl Job {
    fn id(&self) -> u64 {
        match self {
            Job::Outline { id } | Job::Links { id, .. } | Job::CharBoxes { id, .. } | Job::Annotations { id, .. } | Job::AnnotText { id, .. } | Job::Fields { id, .. } | Job::Options { id, .. } | Job::TextLine { id, .. } => *id,
            Job::AnnotList(a) => a.id,
            Job::Search(s) => s.id,
            Job::Copy(c) => c.id,
            Job::Print(p) => p.id,
            Job::OcrScan(o) => o.id,
            Job::OcrImage(o) => o.id,
        }
    }

    /// A quick request that someone is waiting for.
    fn immediate(&self) -> bool {
        matches!(self, Job::Outline { .. } | Job::Links { .. } | Job::CharBoxes { .. } | Job::Annotations { .. } | Job::AnnotText { .. } | Job::Fields { .. } | Job::Options { .. } | Job::TextLine { .. })
    }

    /// Has something to do now (a print job whose band is not used yet waits).
    fn runnable(&self) -> bool {
        !matches!(self, Job::Print(p) if p.waiting)
    }

    fn new(id: u64, what: Request, pages: usize) -> Job {
        match what {
            Request::Outline => Job::Outline { id },
            Request::Links(page) => Job::Links { id, page },
            Request::CharBoxes(page) => Job::CharBoxes { id, page },
            Request::Annotations(page) => Job::Annotations { id, page },
            Request::AnnotText { page, index, num } => Job::AnnotText { id, page, index, num },
            Request::AnnotList => Job::AnnotList(AnnotListJob { id, next: 0, items: Vec::new(), truncated: false, visits: 0 }),
            Request::Fields(page) => Job::Fields { id, page },
            Request::Options { page, index, num } => Job::Options { id, page, index, num },
            Request::TextLine { page, at } => Job::TextLine { id, page, at },
            Request::Search { query, start } => Job::Search(SearchJob {
                id,
                needle: search::fold_query(&query),
                start: if (start as usize) < pages { start } else { 0 },
                done: 0,
                hits: 0,
                skipped: 0,
                announced: Instant::now(),
            }),
            Request::Copy { from, to } => {
                let (from, to) = if from <= to { (from, to) } else { (to, from) };
                Job::Copy(CopyJob { id, from, to, page: from.0, pages_done: 0, text: String::new(), chars: 0, skipped: 0, truncated: false })
            }
            Request::OcrScan { only } => Job::OcrScan(OcrScanJob {
                id,
                only,
                next: only.unwrap_or(0),
                found: Vec::new(),
                looked: 0,
                unreadable: 0,
                truncated: false,
            }),
            Request::OcrImage { page, max_side } => Job::OcrImage(OcrImageJob { id, page, max_side, size: None, y: 0, gray: Vec::new(), incomplete: String::new() }),
            Request::Print(mut list) => {
                list.truncate(MAX_PRINT_PAGES);
                Job::Print(PrintJob { id, pages: list, at: 0, y: 0, waiting: false, checked: false, printed: 0 })
            }
        }
    }
}

/// What came of one step of a job.
enum Step {
    /// Not finished: step it again.
    More,
    Done,
    /// The meter was cancelled (the request was withdrawn, or the flag was raised late for another).
    Cancelled,
}

/// What goes from one document to the edited one that replaces it: the requests that were waiting.
struct Carry {
    queue: Vec<RenderRequest>,
    jobs: VecDeque<Job>,
    edits: VecDeque<Command>,
}

/// How the document is to be replaced.
enum Pending {
    /// The file, written afresh (its cross-reference data was damaged), and the edit that waits for it.
    Rewrite { bytes: Vec<u8>, then: Command },
    /// The update that writes everything in front of the file (`edit::save_section`), to be appended to it; then the file is
    /// written to `path`.
    Save { section: Vec<u8>, doc: u64, id: u64, path: String },
}

/// Why the serving of one document ended.
enum Exit {
    Done(Option<Command>),
    /// Go on with this document (the edited one, or the one before it if the change did not open) and the waiting requests.
    Again { open: Box<OpenDoc>, carry: Carry },
}

struct Server<'a> {
    doc_id: u64,
    document: &'a Document,
    /// The pages; the dictionary of a page that an edit changed is read again.
    pages: Vec<Page>,
    index: IdMap<u32, u32>,
    max_tile_pixels: u32,
    rights: Rights,
    renderer: Renderer<'a>,
    text: TextExtractor<'a>,
    /// The characters of the pages read lately, shared by search, selection and copy.
    chars: TextCache,
    names: Names<'a>,
    queue: Vec<RenderRequest>,
    jobs: VecDeque<Job>,
    commands: &'a Receiver<Command>,
    send: &'a dyn Fn(Event),
    shared: &'a Shared,
    /// The editing state (what can be undone, whether the file was saved).
    session: &'a mut Session,
    /// Edits, undos, redos and saves, in the order they came: done before anything else, one at a time.
    edits: VecDeque<Command>,
    /// Set when the document is to be replaced (the loop then ends with [`Command::Swap`]).
    swap: Option<Pending>,
    /// The plan the memory was shared by (what compressing lends its pictures, `Engine::compress`, is worked out from it), and the limits
    /// and the text budget the drawing's caches are made again with.
    plan: MemoryPlan,
    limits: MemoryLimits,
    text_budget: u64,
}

/// Serve one open document until a command ends it; that command is returned for the caller to carry out. An edit replaces the
/// document by the changed one and goes on with the requests that were waiting.
pub(super) fn serve(open: OpenDoc, session: &mut Session, commands: &Receiver<Command>, send: &dyn Fn(Event), shared: &Shared) -> Option<Command> {
    let mut open = open;
    let mut carry: Option<Carry> = None;
    loop {
        match serve_once(open, carry.take(), session, commands, send, shared) {
            Exit::Done(end) => return end,
            Exit::Again { open: next, carry: waiting } => {
                open = *next;
                carry = Some(waiting);
            }
        }
    }
}

fn serve_once(open: OpenDoc, carry: Option<Carry>, session: &mut Session, commands: &Receiver<Command>, send: &dyn Fn(Event), shared: &Shared) -> Exit {
    let OpenDoc { doc_id, mut document, plan, rights } = open;
    let pages = match document.pages() {
        Ok(p) => p,
        Err(e) => {
            send(Event::OpenFailed { doc: doc_id, failure: OpenFailure::Damaged, message: e.to_string() });
            return Exit::Done(None);
        }
    };
    if carry.is_none() {
        let infos: Vec<PageInfo> = pages.iter().map(page_info).collect();
        send(Event::Opened(Opened {
            doc: doc_id,
            pages: infos,
            file_bytes: plan.file_bytes,
            bitmap_cache_bytes: plan.bitmap_cache_bytes,
            thumb_cache_bytes: plan.thumb_cache_bytes,
            max_tile_pixels: plan.max_tile_pixels,
            rights,
            protection: super::protection_of(&document, session.password()),
        }));
    }
    let Carry { queue, jobs, edits } = carry.unwrap_or(Carry { queue: Vec::new(), jobs: VecDeque::new(), edits: VecDeque::new() });
    let to_usize = |v: u64| usize::try_from(v).unwrap_or(usize::MAX);
    let mut renderer = Renderer::new(&document, crate::forms::painter());
    let limits = MemoryLimits {
        caches: to_usize(plan.render_caches),
        layers: to_usize(plan.layers),
        masks: to_usize(plan.masks),
        decoders: to_usize(plan.decoders),
        image_slot: to_usize(plan.image_slot),
        canvas_pixels: u64::from(plan.max_tile_pixels),
    };
    renderer.set_memory_limits(limits);
    renderer.set_cancel(Some(shared.stop.clone()));
    let mut server = Server {
        doc_id,
        document: &document,
        index: page_index(&pages),
        pages,
        max_tile_pixels: plan.max_tile_pixels,
        rights,
        renderer,
        text: TextExtractor::new(&document),
        chars: TextCache::new(plan.text_cache),
        names: Names::new(&document),
        queue,
        jobs,
        commands,
        send,
        shared,
        session: &mut *session,
        edits,
        swap: None,
        text_budget: plan.text_cache,
        limits,
        plan,
    };
    let end = server.run();
    let Some(pending) = server.swap.take() else { return Exit::Done(end) };
    // (The number of pages now: a change to the pages made while this document was served has changed it.)
    let page_count = server.pages.len();
    if !matches!(end, Some(Command::Swap)) {
        return Exit::Done(end);
    }
    let mut carry = Carry { queue: std::mem::take(&mut server.queue), jobs: std::mem::take(&mut server.jobs), edits: std::mem::take(&mut server.edits) };
    drop(server);

    match pending {
        // The file as it will be, made as a copy so that the document it replaces is still there if the new one does not open.
        Pending::Rewrite { bytes, then } => match reopen(doc_id, plan, rights, bytes, session.password(), page_count) {
            Ok(next) => {
                session.rewritten();
                // The edit that waited for the new file is done first of all.
                carry.edits.push_front(then);
                Exit::Again { open: Box::new(next), carry }
            }
            Err(message) => {
                let message = format!("the changed file could not be opened again: {message}");
                // The edit that waited for the rewrite is given up as well.
                match &then {
                    Command::Edit { doc, id, edit } => send(Event::Edited(session.result(*doc, *id, EditStatus::Failed, &message, edit.page()))),
                    Command::Save { doc, id, .. } => send(Event::Saved { doc: *doc, id: *id, status: SaveStatus::Failed, message, bytes: 0 }),
                    _ => {}
                }
                Exit::Again { open: Box::new(OpenDoc { doc_id, document, plan, rights }), carry }
            }
        },
        // The update is appended to the file in memory, which is read again from its new end (the document is then the saved
        // file), and the file is written. If it cannot be written, the document is what it was meant to be written as and is
        // still changed: saving again writes it.
        Pending::Save { section, doc, id, path } => {
            session.before_saving(&document);
            match document.append_section(&section, page_count) {
                Ok(()) => {
                    let bytes = document.data();
                    match write_atomic(&path, bytes) {
                        Ok(()) => {
                            session.mark_saved();
                            send(Event::Saved { doc, id, status: SaveStatus::Done, message: String::new(), bytes: bytes.len() as u64 });
                        }
                        Err(e) => send(Event::Saved { doc, id, status: SaveStatus::Failed, message: e.to_string(), bytes: 0 }),
                    }
                }
                Err(e) => send(Event::Saved { doc, id, status: SaveStatus::Failed, message: e.to_string(), bytes: 0 }),
            }
            Exit::Again { open: Box::new(OpenDoc { doc_id, document, plan, rights }), carry }
        }
    }
}

impl Server<'_> {
    fn run(&mut self) -> Option<Command> {
        loop {
            // Take what has been asked for: wait only when there is nothing to do.
            if self.queue.is_empty() && self.edits.is_empty() && !self.jobs.iter().any(Job::runnable) {
                match self.commands.recv() {
                    Ok(c) => {
                        if let Some(end) = self.take(c) {
                            return Some(end);
                        }
                    }
                    Err(_) => return Some(Command::Quit),
                }
            }
            while let Ok(c) = self.commands.try_recv() {
                if let Some(end) = self.take(c) {
                    return Some(end);
                }
            }
            // An edit, an undo or a save is done as soon as it is asked for, before the rest, one at a time: a person is waiting
            // for the page to change. (No job is in hand here, so a swap of the document loses none of them.)
            if let Some(command) = self.edits.pop_front() {
                if let Some(end) = self.do_edit(command) {
                    return Some(end);
                }
                continue;
            }
            // What is in view is drawn first, then the quick requests, then the rest of the drawing, and only when nothing
            // else is waiting a step of a long job. The quick requests do not hold up what is in view (3d-2 review).
            // The most urgent request; of equal ones, the oldest (the queue is in the order they came).
            let urgent = self.queue.iter().enumerate().min_by_key(|(i, r)| (r.priority, *i)).map(|(i, r)| (i, r.priority));
            if urgent.is_none_or(|(_, priority)| priority >= BACKGROUND_PRIORITY)
                && let Some(pos) = self.jobs.iter().position(Job::immediate)
            {
                if let Some(job) = self.jobs.remove(pos)
                    && let Some(end) = self.work_on(job)
                {
                    return Some(end);
                }
                continue;
            }
            if let Some((index, _)) = urgent {
                let request = self.queue.remove(index);
                if let Some(end) = self.draw_request(request) {
                    return Some(end);
                }
                continue;
            }
            if let Some(pos) = self.jobs.iter().position(Job::runnable)
                && let Some(job) = self.jobs.remove(pos)
                && let Some(end) = self.work_on(job)
            {
                return Some(end);
            }
        }
    }

    /// Carry out a command that is not about drawing, or queue the request; a command that ends the document is returned.
    fn take(&mut self, command: Command) -> Option<Command> {
        match command {
            Command::Render(r) => {
                if r.doc == self.doc_id {
                    self.queue.push(r);
                }
                None
            }
            Command::Request { doc, id, what } => {
                if doc == self.doc_id {
                    self.jobs.push_back(Job::new(id, what, self.pages.len()));
                }
                None
            }
            c @ (Command::Edit { .. } | Command::Undo { .. } | Command::Redo { .. } | Command::Save { .. } | Command::Export { .. } | Command::Extract { .. } | Command::Protect { .. } | Command::Compress { .. }) => {
                let doc = match &c {
                    Command::Edit { doc, .. }
                    | Command::Undo { doc, .. }
                    | Command::Redo { doc, .. }
                    | Command::Save { doc, .. }
                    | Command::Export { doc, .. }
                    | Command::Extract { doc, .. }
                    | Command::Protect { doc, .. }
                    | Command::Compress { doc, .. } => *doc,
                    _ => 0,
                };
                if doc == self.doc_id {
                    self.edits.push_back(c);
                }
                None
            }
            Command::Swap => None,
            Command::PrintNext(id) => {
                for job in &mut self.jobs {
                    if let Job::Print(p) = job
                        && p.id == id
                    {
                        p.waiting = false;
                    }
                }
                None
            }
            Command::Cancel(id) => {
                self.queue.retain(|r| r.id != id);
                self.jobs.retain(|j| j.id() != id);
                None
            }
            end @ (Command::Open { .. } | Command::Close | Command::Quit) => Some(end),
        }
    }

    /// Start work on request `id`: clear the stop flag, name the request, and look at the commands that came in meanwhile
    /// (a cancel that came between taking the commands and now would find nothing being worked on). `Err`: a command ended
    /// the document. `Ok(false)`: the request was cancelled.
    fn begin(&mut self, id: u64) -> Result<bool, Command> {
        self.shared.stop.store(false, Ordering::SeqCst);
        self.shared.current.store(id, Ordering::SeqCst);
        let mut cancelled = false;
        while let Ok(c) = self.commands.try_recv() {
            match c {
                Command::Cancel(i) if i == id => cancelled = true,
                other => {
                    if let Some(end) = self.take(other) {
                        self.shared.current.store(0, Ordering::SeqCst);
                        return Err(end);
                    }
                }
            }
        }
        Ok(!cancelled)
    }

    /// The work on `id` was stopped by the flag: was it withdrawn, or was the flag raised late for the one before it? The cancel
    /// command was sent before the flag, so it is there to be found if it was. `Err`: a command ended the document.
    fn really_cancelled(&mut self, id: u64) -> Result<bool, Command> {
        let mut really = false;
        while let Ok(c) = self.commands.try_recv() {
            match c {
                Command::Cancel(i) if i == id => really = true,
                other => {
                    if let Some(end) = self.take(other) {
                        self.shared.current.store(0, Ordering::SeqCst);
                        return Err(end);
                    }
                }
            }
        }
        Ok(really)
    }

    fn draw_request(&mut self, request: RenderRequest) -> Option<Command> {
        match self.begin(request.id) {
            Err(end) => return Some(end),
            Ok(false) => {
                self.shared.current.store(0, Ordering::SeqCst);
                return None;
            }
            Ok(true) => {}
        }
        match draw(&mut self.renderer, &self.pages, &request) {
            Some(result) => {
                self.shared.current.store(0, Ordering::SeqCst);
                (self.send)(Event::Rendered(result));
            }
            None => {
                // Stopped: by the cancel of this request, or by a flag raised late for the one before it.
                match self.really_cancelled(request.id) {
                    Err(end) => return Some(end),
                    Ok(true) => {}
                    Ok(false) => self.queue.insert(0, request),
                }
            }
        }
        self.shared.current.store(0, Ordering::SeqCst);
        None
    }

    /// One step of a job.
    fn work_on(&mut self, mut job: Job) -> Option<Command> {
        let id = job.id();
        match self.begin(id) {
            Err(end) => return Some(end),
            Ok(false) => {
                self.shared.current.store(0, Ordering::SeqCst);
                return None;
            }
            Ok(true) => {}
        }
        let step = match &mut job {
            Job::Outline { id } => self.outline(*id),
            Job::Links { id, page } => self.links(*id, *page),
            Job::CharBoxes { id, page } => self.char_boxes(*id, *page),
            Job::Annotations { id, page } => self.annotations(*id, *page),
            Job::AnnotText { id, page, index, num } => self.annot_text(*id, *page, *index, *num),
            Job::AnnotList(a) => self.step_annot_list(a),
            Job::Fields { id, page } => self.fields(*id, *page),
            Job::Options { id, page, index, num } => self.options(*id, *page, *index, *num),
            Job::TextLine { id, page, at } => self.text_line(*id, *page, *at),
            Job::Search(s) => self.step_search(s),
            Job::Copy(c) => self.step_copy(c),
            Job::Print(p) => self.step_print(p),
            Job::OcrScan(o) => self.step_ocr_scan(o),
            Job::OcrImage(o) => self.step_ocr_image(o),
        };
        self.shared.current.store(0, Ordering::SeqCst);
        match step {
            Step::More => self.jobs.push_back(job),
            Step::Done => {}
            Step::Cancelled => match self.really_cancelled(id) {
                Err(end) => return Some(end),
                Ok(true) => {}
                Ok(false) => self.jobs.push_front(job),
            },
        }
        None
    }

    /// The characters of page `page_no` (the page must exist), from the cache or read now (charged to `work`). `pass`: the page
    /// is one of many read by a search or a copy.
    fn page_chars(&mut self, page_no: u32, work: &Work, pass: bool) -> Option<Result<Rc<PageChars>, Error>> {
        if let Some(known) = self.chars.get(page_no) {
            return Some(Ok(known));
        }
        let page = self.pages.get(page_no as usize)?;
        Some(self.text.page_chars(page, work).map(|c| {
            // A page with no text of its own has the text recognised for it in this session, if there is some (5c: a file that may
            // not be written to keeps it in memory).
            let c = match self.session.kept_text(page_no) {
                Some(kept) if c.text.trim().is_empty() => kept,
                _ => Rc::new(c),
            };
            self.chars.put(page_no, c.clone(), pass);
            c
        }))
    }

    /// A work meter for one page of a job, cancelled by the flag that stops the request being worked on.
    fn meter(&self) -> Work {
        Work::new().with_cancel(Some(self.shared.stop.clone()))
    }

    fn tell(&self, event: Event) {
        (self.send)(event);
    }

    // --- bookmarks, links, boxes of characters --------------------------------------------------------------------

    fn outline(&mut self, id: u64) -> Step {
        let work = self.meter();
        let found = outline::read(self.document, &self.pages, &self.index, &self.names, &work);
        if work.was_cancelled() {
            return Step::Cancelled;
        }
        self.tell(Event::Outline { doc: self.doc_id, id, items: found.items, truncated: found.truncated });
        Step::Done
    }

    fn links(&mut self, id: u64, page: u32) -> Step {
        let work = self.meter();
        let (links, truncated) = match self.pages.get(page as usize) {
            Some(p) => links::read(self.document, p, &self.pages, &self.index, &self.names, &work),
            None => (Vec::new(), false),
        };
        if work.was_cancelled() {
            return Step::Cancelled;
        }
        self.tell(Event::Links { doc: self.doc_id, id, page, links, truncated });
        Step::Done
    }

    fn annotations(&mut self, id: u64, page: u32) -> Step {
        let work = self.meter();
        let (items, truncated) = match self.pages.get(page as usize) {
            Some(p) => annots::read(self.document, p, &work),
            None => (Vec::new(), false),
        };
        if work.was_cancelled() {
            return Step::Cancelled;
        }
        self.tell(Event::Annotations { doc: self.doc_id, id, page, items, truncated });
        Step::Done
    }

    fn annot_text(&mut self, id: u64, page: u32, index: u32, num: u32) -> Step {
        let work = self.meter();
        let text = self.pages.get(page as usize).and_then(|p| annots::text(self.document, p, index, num, &work));
        if work.was_cancelled() {
            return Step::Cancelled;
        }
        self.tell(Event::AnnotText { doc: self.doc_id, id, page, index, num, found: text.is_some(), text: text.unwrap_or_default() });
        Step::Done
    }

    /// One page of the list of the document's annotations.
    fn step_annot_list(&mut self, j: &mut AnnotListJob) -> Step {
        let work = self.meter();
        let page = self.pages.get(j.next as usize);
        let looked = page.map_or(0, |p| annots::looked_at(self.document, p, &work));
        let over = j.visits.saturating_add(looked) > annots::MAX_LIST_VISITS;
        let Some(page) = page.filter(|_| !over && j.items.len() < annots::MAX_LISTED_ANNOTS) else {
            j.truncated |= (over || j.items.len() >= annots::MAX_LISTED_ANNOTS) && (j.next as usize) < self.pages.len();
            self.tell(Event::AnnotList { doc: self.doc_id, id: j.id, items: std::mem::take(&mut j.items), truncated: j.truncated });
            return Step::Done;
        };
        j.visits += looked;
        let (infos, cut) = annots::read(self.document, page, &work);
        if work.was_cancelled() {
            return Step::Cancelled;
        }
        j.truncated |= cut;
        let room = annots::MAX_LISTED_ANNOTS.saturating_sub(j.items.len());
        j.truncated |= infos.len() > room;
        j.items.extend(infos.into_iter().take(room).map(|i| annots::listed(j.next, i)));
        j.next += 1;
        Step::More
    }

    fn fields(&mut self, id: u64, page: u32) -> Step {
        let work = self.meter();
        let (items, truncated) = match self.pages.get(page as usize) {
            Some(p) => forms::read(self.document, p, &work),
            None => (Vec::new(), false),
        };
        if work.was_cancelled() {
            return Step::Cancelled;
        }
        self.tell(Event::Fields { doc: self.doc_id, id, page, items, truncated });
        Step::Done
    }

    /// The line of text under a click, for editing it (5a): its text, where it is, and why it cannot be changed if it cannot.
    fn text_line(&mut self, id: u64, page: u32, at: [f32; 2]) -> Step {
        let doc = self.doc_id;
        let says = |this: &Self, status: TextStatus, message: &str, line: Option<edit::LineView>| {
            this.tell(Event::TextLine { doc, id, page, status, message: message.to_string(), line });
        };
        if !self.rights.modify {
            says(self, TextStatus::Denied, DENIED_MODIFY, None);
            return Step::Done;
        }
        if let Policy::Refuse(why) = self.session.policy(self.document, EditClass::Content) {
            says(self, TextStatus::Failed, why, None);
            return Step::Done;
        }
        let work = self.meter();
        let Some(p) = self.pages.get(page as usize) else {
            says(self, TextStatus::Failed, "there is no such page", None);
            return Step::Done;
        };
        match edit::text_line(self.document, p, at, &work) {
            Ok(line) => says(self, TextStatus::Ok, "", line),
            Err(Error::Cancelled) => return Step::Cancelled,
            Err(e) => says(self, TextStatus::Failed, &e.to_string(), None),
        }
        Step::Done
    }

    fn options(&mut self, id: u64, page: u32, index: u32, num: u32) -> Step {
        let work = self.meter();
        let (items, truncated) = match self.pages.get(page as usize) {
            Some(p) => forms::options(self.document, p, &work, index, num),
            None => (Vec::new(), false),
        };
        if work.was_cancelled() {
            return Step::Cancelled;
        }
        self.tell(Event::Options { doc: self.doc_id, id, page, index, num, items, truncated });
        Step::Done
    }

    // --- editing --------------------------------------------------------------------------------------------------

    /// Keep the text recognised for pages (5c) in the session, for a file that may not be written to; gives how many pages were
    /// kept and how many had words.
    fn keep_recognised(&mut self, recognised: &[edit::OcrPage]) -> (usize, usize) {
        let (mut kept, mut asked) = (0usize, 0usize);
        for o in recognised.iter().take(edit::MAX_OCR_PAGES) {
            let Some(page) = self.pages.get(o.page as usize) else { continue };
            let (w, h) = crate::text::Shown::of(page).size();
            let Ok(chars) = edit::ocr_page_chars(o, w, h) else { continue };
            if chars.boxes.is_empty() {
                continue;
            }
            asked += 1;
            if self.session.keep_text(o.page, Rc::new(chars)) {
                self.chars.forget(o.page);
                kept += 1;
            }
        }
        (kept, asked)
    }

    /// Read the dictionary of page `page_no` again, after an edit (or an undo) changed it, and forget what is kept of the page.
    /// Nothing else is kept by page: what the renderer keeps is by object, and an edit only makes objects with numbers that were
    /// never used before.
    fn refresh_page(&mut self, page_no: u32) {
        if page_no == edit::ALL_PAGES {
            // Every page (a watermark).
            for (n, p) in self.pages.iter_mut().enumerate() {
                if let Ok(Object::Dict(mut dict)) = self.document.get(p.obj_ref) {
                    dict.remove("Parent");
                    p.dict = Rc::new(dict);
                }
                self.chars.forget(n as u32);
            }
            return;
        }
        let Some(page_ref) = self.pages.get(page_no as usize).map(|p| p.obj_ref) else { return };
        if let Ok(Object::Dict(mut dict)) = self.document.get(page_ref) {
            dict.remove("Parent");
            let dict = Rc::new(dict);
            for p in self.pages.iter_mut().filter(|p| p.obj_ref == page_ref) {
                p.dict = Rc::clone(&dict);
            }
        }
        self.chars.forget(page_no);
    }

    /// The list of pages has changed (a change to the pages, or the undo or redo of one): read it again. `fits` says whether the number of
    /// pages is the one it should be; if it is not, or the pages cannot be read, nothing here is changed and the reason is returned.
    /// What is kept by page (the characters, the recognised text) is let go of; what the renderer keeps is by object and stays.
    fn refresh_structure(&mut self, fits: impl Fn(usize) -> bool) -> Result<Vec<PageInfo>, String> {
        let pages = self.document.pages().map_err(|e| format!("the pages cannot be read after the change: {e}"))?;
        if !fits(pages.len()) {
            return Err("the page tree did not come out as the change meant it to".to_string());
        }
        self.index = page_index(&pages);
        self.pages = pages;
        self.chars.clear();
        self.session.forget_memory_text();
        Ok(self.pages.iter().map(page_info).collect())
    }

    /// Carry out an edit, an undo, a redo or a save. `Some(Command::Swap)`: the document is to be replaced (`self.swap` says how).
    fn do_edit(&mut self, command: Command) -> Option<Command> {
        let said = |this: &Self, doc: u64, id: u64, status: EditStatus, message: &str, page: u32| {
            this.tell(Event::Edited(this.session.result(doc, id, status, message, page)));
        };
        let printing = self.jobs.iter().any(|j| matches!(j, Job::Print(_)));
        const BUSY: &str = "a print job is running";
        match command {
            Command::Edit { doc, id, edit } => {
                let page = edit.page();
                let class = edit.class();
                let (allowed, denied) = match class {
                    EditClass::Fill => (self.rights.fill_forms, DENIED_FILL),
                    EditClass::Content => (self.rights.modify, DENIED_MODIFY),
                    EditClass::Annotate => (self.rights.annotate, DENIED_ANNOTATE),
                    EditClass::Assemble => (self.rights.assemble, DENIED_ASSEMBLE),
                };
                // Recognised text for a file that may not be changed is used in this window and not written (5c).
                if let edit::Edit::OcrLayer(recognised) = &edit
                    && (!allowed || matches!(self.session.policy(self.document, class), Policy::Refuse(_)))
                {
                    let (kept, asked) = self.keep_recognised(recognised);
                    let message = if kept == asked { MEMORY_ONLY.to_string() } else { format!("{MEMORY_ONLY} Only {kept} of the {asked} pages fit in memory.") };
                    said(self, doc, id, if kept > 0 { EditStatus::Done } else { EditStatus::Failed }, if kept > 0 { &message } else { "the recognised text is too big to keep in memory" }, page);
                    return None;
                }
                if !allowed {
                    said(self, doc, id, EditStatus::Denied, denied, page);
                    return None;
                }
                if printing {
                    said(self, doc, id, EditStatus::Busy, BUSY, page);
                    return None;
                }
                // A signed file is added to only as far as its signatures allow (12.8).
                let policy = self.session.policy(self.document, class);
                if let Policy::Refuse(why) = policy {
                    said(self, doc, id, EditStatus::Failed, why, page);
                    return None;
                }
                // A file whose cross-reference data was damaged cannot be added to: it is written afresh first, as a whole (not
                // a signed one: that would break the signature).
                if self.document.was_repaired() || self.document.leading_junk() > 0 {
                    if policy != Policy::Free {
                        said(self, doc, id, EditStatus::Failed, SIGNED_AND_DAMAGED, page);
                        return None;
                    }
                    return match ops::copy_all(self.document) {
                        Ok(out) => {
                            self.swap = Some(Pending::Rewrite { bytes: out.data, then: Command::Edit { doc, id, edit } });
                            Some(Command::Swap)
                        }
                        Err(e) => {
                            said(self, doc, id, EditStatus::Failed, &format!("the file is damaged and cannot be written again: {e}"), page);
                            None
                        }
                    };
                }
                // An edit may use a page's worth of work, and all of a session's together twenty pages' worth.
                let work = Work::with_allowance(self.session.work_left().min(crate::render::work::PAGE_WORK));
                // (What the edit may take is what is left of the memory set aside, after what is in front of the file already.)
                let room = self.session.budget().saturating_sub(self.document.overlay_bytes());
                let made = edit::apply_any(self.document, &self.pages, &edit, &edit::pdf_date_now(), &work, room);
                self.session.add_work(work.used());
                let (objects, notes) = match made {
                    Ok(done) => done,
                    // Pages from a file that do not fit: they cannot be put in in parts, so it is said with a status of its own (the reader
                    // has the words for it).
                    Err(Error::Limit(m)) if m == edit::TOO_BIG && matches!(&edit, edit::Edit::Pages(op) if matches!(**op, edit::PageOp::Insert { .. })) => {
                        let message = format!("those pages do not fit in the {} MB set aside for what is not saved: save the file first, or put in a smaller file", edit::MAX_INSERT_FILE_BYTES >> 20);
                        said(self, doc, id, EditStatus::TooBig, &message, page);
                        return None;
                    }
                    Err(Error::Limit(m)) if m == edit::TOO_BIG => {
                        let advice = if self.document.has_overlay() { "save the file first, or do it in parts" } else { "do it in parts (fewer pages at a time)" };
                        said(self, doc, id, EditStatus::Failed, &format!("{m}: {advice}"), page);
                        return None;
                    }
                    Err(Error::PasswordRequired) => {
                        said(self, doc, id, EditStatus::NeedsPassword, "the file the pages come from needs a password", page);
                        return None;
                    }
                    Err(Error::WrongPassword) => {
                        said(self, doc, id, EditStatus::WrongPassword, "that password does not open the file the pages come from", page);
                        return None;
                    }
                    Err(e) => {
                        said(self, doc, id, EditStatus::Failed, &e.to_string(), page);
                        return None;
                    }
                };
                let extra = objects.iter().fold(0usize, |sum, (_, o)| sum.saturating_add(o.approx_size()).saturating_add(48));
                if !self.session.make_room(self.document, extra) {
                    said(self, doc, id, EditStatus::Failed, TOO_MANY_CHANGES, page);
                    return None;
                }
                let changes = edit::commit(self.document, objects);
                let signed = if policy == Policy::Warn && self.session.take_warning() { SIGNED_WARNING } else { "" };
                let message = std::iter::once(signed.to_string()).chain(notes).filter(|m| !m.is_empty()).collect::<Vec<_>>().join(" ");
                if let edit::Edit::Pages(op) = &edit {
                    // The list of pages: read again from the document, and only if it came out as the change meant is the step kept.
                    let before = self.pages.len();
                    let expected = op.count_after(before);
                    let infos = match self.refresh_structure(|n| expected.map_or(n > before, |e| e == n)) {
                        Ok(infos) => infos,
                        Err(why) => {
                            edit::replay(self.document, &changes, false);
                            said(self, doc, id, EditStatus::Failed, &why, page);
                            return None;
                        }
                    };
                    self.session.push_pages(changes);
                    let mut result = self.session.result(doc, id, EditStatus::Done, &message, edit::ALL_PAGES);
                    result.pages = Some(infos);
                    self.tell(Event::Edited(result));
                    return None;
                }
                let page = self.session.push(page, changes);
                self.refresh_page(page);
                said(self, doc, id, EditStatus::Done, &message, page);
                None
            }
            Command::Undo { doc, id } | Command::Redo { doc, id } => {
                let undo = matches!(command, Command::Undo { .. });
                if printing {
                    said(self, doc, id, EditStatus::Busy, BUSY, 0);
                    return None;
                }
                let done = if undo { self.session.undo(self.document) } else { self.session.redo(self.document) };
                match done {
                    None => said(self, doc, id, EditStatus::Nothing, "", 0),
                    Some((page, true)) => match self.refresh_structure(|_| true) {
                        Ok(infos) => {
                            let mut result = self.session.result(doc, id, EditStatus::Done, "", page);
                            result.pages = Some(infos);
                            self.tell(Event::Edited(result));
                        }
                        Err(why) => said(self, doc, id, EditStatus::Failed, &why, page),
                    },
                    Some((page, false)) => {
                        self.refresh_page(page);
                        said(self, doc, id, EditStatus::Done, "", page);
                    }
                }
                None
            }
            Command::Export { doc, id, path, excel } => {
                let reply = |this: &Self, status: SaveStatus, message: String, pages: u32, tables: u32, bytes: u64| this.tell(Event::Exported { doc, id, status, message, pages, tables, bytes });
                if printing {
                    reply(self, SaveStatus::Failed, BUSY.to_string(), 0, 0, 0);
                    return None;
                }
                // The conversion looks at the stop flag between pages (and the work of a page at its next step), so that `cancel(id)`, or
                // closing the document, ends it; a withdrawn request gets no answer. A flag raised late for the request before this one
                // stops it too, and then it is started again.
                let result = loop {
                    match self.begin(id) {
                        Err(end) => return Some(end),
                        Ok(false) => {
                            self.shared.current.store(0, Ordering::SeqCst);
                            return None;
                        }
                        Ok(true) => {}
                    }
                    let (to, stop) = (std::path::Path::new(&path), Some(self.shared.stop.clone()));
                    // (What the two conversions tell, alike: pages, tables, bytes, and what they warned of.)
                    let result = if excel {
                        crate::xlsx::export_to_path(self.document, self.session.password(), None, to, stop).map(|r| (r.pages, r.tables, r.bytes, r.warnings))
                    } else {
                        crate::docx::export_to_path(self.document, self.session.password(), None, to, stop).map(|r| (r.pages, r.counts.tables, r.bytes, r.warnings))
                    };
                    self.shared.current.store(0, Ordering::SeqCst);
                    if matches!(result, Err(Error::Cancelled)) {
                        match self.really_cancelled(id) {
                            Err(end) => return Some(end),
                            Ok(true) => return None,
                            Ok(false) => continue,
                        }
                    }
                    break result;
                };
                match result {
                    Ok((pages, tables, bytes, warnings)) => reply(self, SaveStatus::Done, warnings.join("; "), u32::try_from(pages).unwrap_or(u32::MAX), u32::try_from(tables).unwrap_or(u32::MAX), bytes),
                    Err(e) => reply(self, SaveStatus::Failed, e.to_string(), 0, 0, 0),
                }
                None
            }
            Command::Extract { doc, id, pages, path } => {
                let reply = |this: &Self, status: SaveStatus, message: String, count: u32, bytes: u64| this.tell(Event::Exported { doc, id, status, message, pages: count, tables: 0, bytes });
                if printing {
                    reply(self, SaveStatus::Failed, BUSY.to_string(), 0, 0);
                    return None;
                }
                if !self.rights.copy {
                    reply(self, SaveStatus::Failed, DENIED_EXTRACT.to_string(), 0, 0);
                    return None;
                }
                let wanted: Vec<usize> = edit::unique_pages(&pages, self.pages.len()).into_iter().map(|p| p as usize).collect();
                if wanted.is_empty() || pages.iter().any(|&p| p as usize >= self.pages.len()) {
                    reply(self, SaveStatus::Failed, "there is no such page".to_string(), 0, 0);
                    return None;
                }
                match ops::extract_pages(self.document, &wanted) {
                    Ok(out) => match write_atomic(&path, &out.data) {
                        Ok(()) => {
                            let message = out.warnings.iter().map(|w| w.0.as_str()).collect::<Vec<_>>().join("; ");
                            reply(self, SaveStatus::Done, message, u32::try_from(out.pages).unwrap_or(u32::MAX), out.data.len() as u64);
                        }
                        Err(e) => reply(self, SaveStatus::Failed, e.to_string(), 0, 0),
                    },
                    Err(e) => reply(self, SaveStatus::Failed, e.to_string(), 0, 0),
                }
                None
            }
            Command::Protect { doc, id, path, job } => {
                let reply = |this: &Self, status: SaveStatus, message: String, bytes: u64| this.tell(Event::Protected { doc, id, status, message, bytes });
                if printing {
                    reply(self, SaveStatus::Failed, BUSY.to_string(), 0);
                    return None;
                }
                let ProtectJob { change, mut seed } = *job;
                // The document as it is, with the edits not saved: written afresh under the new encryption (or none).
                let result = match &change {
                    ProtectChange::Set(how) => ops::protect(self.document, self.session.password(), how, &seed),
                    ProtectChange::Remove => ops::decrypt(self.document, self.session.password()),
                };
                seed.fill(0);
                std::hint::black_box(&seed);
                match result {
                    Ok(out) => match write_atomic(&path, &out.data) {
                        Ok(()) => reply(self, SaveStatus::Done, String::new(), out.data.len() as u64),
                        Err(e) => reply(self, SaveStatus::Failed, e.to_string(), 0),
                    },
                    Err(e) => reply(self, SaveStatus::Failed, e.to_string(), 0),
                }
                None
            }
            Command::Compress { doc, id, path, how } => {
                let reply = |this: &Self, status: SaveStatus, message: String, before: u64, after: u64, kept: bool| this.tell(Event::Compressed { doc, id, status, message, before, after, kept });
                if printing {
                    reply(self, SaveStatus::Failed, BUSY.to_string(), 0, 0, false);
                    return None;
                }
                // (Only what changes the pictures needs the right to change the content: written again as it is, the file is a plain copy.)
                if !how.lossless_only && !self.rights.modify {
                    reply(self, SaveStatus::Failed, DENIED_MODIFY.to_string(), 0, 0, false);
                    return None;
                }
                if self.document.data().len() as u64 > super::MAX_COMPRESS_FILE {
                    reply(self, SaveStatus::Failed, format!("the file is bigger than {} MB, the most this reader compresses; use the command line: qingpdf compress", super::MAX_COMPRESS_FILE / 1_000_000), 0, 0, false);
                    return None;
                }
                // What the window keeps, the file and its copies must fit the total (the pictures are lent what is left): in a window big
                // enough to keep most of it for its bitmaps, a file that is not small is left to the command line.
                let Some(room) = self.plan.compress_room(self.document.overlay_bytes() as u64) else {
                    reply(self, SaveStatus::Failed, "the window and the file leave this reader too little memory to compress it; use the command line: qingpdf compress".to_string(), 0, 0, false);
                    return None;
                };
                // The drawing's caches (fonts, pictures, forms, the text of the pages read) are let go of: the memory is for the
                // compressing, and they fill up again as pages are drawn.
                self.renderer = Renderer::new(self.document, crate::forms::painter());
                self.renderer.set_memory_limits(self.limits);
                self.renderer.set_cancel(Some(self.shared.stop.clone()));
                self.text = TextExtractor::new(self.document);
                self.chars = TextCache::new(self.text_budget);
                // The document as it is, with the edits not saved. The stop flag stops it at its next step; a flag raised late for the
                // request before this one stops it too, and then it is started again.
                let result = loop {
                    match self.begin(id) {
                        Err(end) => return Some(end),
                        Ok(false) => {
                            self.shared.current.store(0, Ordering::SeqCst);
                            return None;
                        }
                        Ok(true) => {}
                    }
                    let last = std::cell::Cell::new(u32::MAX);
                    let progress = |percent: u32| {
                        if last.replace(percent) != percent {
                            self.tell(Event::CompressProgress { doc, id, percent });
                        }
                    };
                    let control = crate::compress::Control { stop: Some(self.shared.stop.clone()), progress: &progress };
                    let options = crate::compress::Options { preset: how.preset, lossless_only: how.lossless_only, picture_memory: room, threads: 0 };
                    let result = crate::compress::compress(self.document, self.session.password(), &options, &control);
                    self.shared.current.store(0, Ordering::SeqCst);
                    if matches!(result, Err(Error::Cancelled)) {
                        match self.really_cancelled(id) {
                            Err(end) => return Some(end),
                            Ok(true) => return None,
                            Ok(false) => continue,
                        }
                    }
                    break result;
                };
                match result {
                    Ok(packed) => {
                        let message = packed.warnings.iter().map(|w| w.0.as_str()).collect::<Vec<_>>().join("; ");
                        match packed.data {
                            Some(data) => match write_atomic(&path, &data) {
                                Ok(()) => reply(self, SaveStatus::Done, message, packed.before, data.len() as u64, false),
                                Err(e) => reply(self, SaveStatus::Failed, e.to_string(), 0, 0, false),
                            },
                            None => reply(self, SaveStatus::Done, message, packed.before, packed.before, true),
                        }
                    }
                    Err(e) => reply(self, SaveStatus::Failed, e.to_string(), 0, 0, false),
                }
                None
            }
            Command::Save { doc, id, path, whole } => {
                let failed = |this: &Self, message: &str| this.tell(Event::Saved { doc, id, status: SaveStatus::Failed, message: message.to_string(), bytes: 0 });
                // Save as: the whole file again (a signed file is saved as it is, an update: written afresh it would not be signed).
                if whole && self.session.policy(self.document, EditClass::Annotate) == Policy::Free {
                    if printing {
                        failed(self, BUSY);
                        return None;
                    }
                    // (The document becomes the new file, which is then written as it is.)
                    return match ops::copy_all(self.document) {
                        Ok(out) => {
                            self.swap = Some(Pending::Rewrite { bytes: out.data, then: Command::Save { doc, id, path, whole: false } });
                            Some(Command::Swap)
                        }
                        Err(e) => {
                            failed(self, &e.to_string());
                            None
                        }
                    };
                }
                if !self.document.has_overlay() {
                    // Nothing was changed since the file was read or saved: it is written as it is.
                    let bytes = self.document.data();
                    match write_atomic(&path, bytes) {
                        Ok(()) => {
                            self.session.mark_saved();
                            self.tell(Event::Saved { doc, id, status: SaveStatus::Done, message: String::new(), bytes: bytes.len() as u64 });
                        }
                        Err(e) => failed(self, &e.to_string()),
                    }
                    return None;
                }
                if printing {
                    failed(self, BUSY);
                    return None;
                }
                if self.document.was_repaired() || self.document.leading_junk() > 0 {
                    failed(self, "the file's structure is damaged; it cannot be added to");
                    return None;
                }
                match edit::save_section(self.document) {
                    Ok(section) => {
                        self.swap = Some(Pending::Save { section, doc, id, path });
                        Some(Command::Swap)
                    }
                    Err(e) => {
                        failed(self, &e.to_string());
                        None
                    }
                }
            }
            _ => None,
        }
    }

    fn char_boxes(&mut self, id: u64, page: u32) -> Step {
        let doc = self.doc_id;
        let reply = move |status: TextStatus, message: &str, boxes: Vec<[f32; 4]>, text: String| {
            Event::CharBoxes(CharBoxes { doc, id, page, status, message: message.to_string(), boxes, text })
        };
        // Where the characters are, without what they are, is also what marking text needs: a file that forbids copying and allows
        // annotating gives the positions (copying stays refused).
        if !self.rights.copy && !self.rights.annotate {
            self.tell(reply(TextStatus::Denied, DENIED_COPY, Vec::new(), String::new()));
            return Step::Done;
        }
        let work = self.meter();
        match self.page_chars(page, &work, false) {
            None => {
                self.tell(reply(TextStatus::Failed, "there is no such page", Vec::new(), String::new()));
                Step::Done
            }
            Some(Err(Error::Cancelled)) => Step::Cancelled,
            Some(Err(e)) => {
                self.tell(reply(TextStatus::Failed, &e.to_string(), Vec::new(), String::new()));
                Step::Done
            }
            Some(Ok(chars)) => {
                // The characters go with the boxes only where copying is allowed.
                let text = if self.rights.copy { chars.text.clone() } else { String::new() };
                self.tell(reply(TextStatus::Ok, "", chars.boxes.clone(), text));
                Step::Done
            }
        }
    }

    // --- search ---------------------------------------------------------------------------------------------------

    fn step_search(&mut self, s: &mut SearchJob) -> Step {
        let total = u32::try_from(self.pages.len()).unwrap_or(u32::MAX);
        if s.needle.is_empty() || s.done >= total {
            self.finish_search(s, SearchStatus::Finished);
            return Step::Done;
        }
        let page_no = (s.start + s.done) % total;
        let work = self.meter();
        let mut hits: Vec<SearchHit> = Vec::new();
        match self.page_chars(page_no, &work, true) {
            Some(Err(Error::Cancelled)) => return Step::Cancelled,
            None | Some(Err(_)) => s.skipped += 1,
            Some(Ok(chars)) => {
                let (hay, origin) = search::fold(&chars.text);
                let room = MAX_SEARCH_HITS.saturating_sub(s.hits as usize).min(MAX_HITS_PER_PAGE);
                if work.charge(hay.len() as f64 * cost::SEARCH_CHAR) {
                    let found = search::find_all(&hay, &s.needle, room, &mut |n| work.charge(n as f64 * cost::SEARCH_STEP));
                    for (a, b) in found {
                        // From the first character of the match to the last, as the page's text has them.
                        let (Some(&first), Some(&last)) = (origin.get(a), origin.get(b.saturating_sub(1))) else { continue };
                        let (first, end) = (first as usize, last as usize + 1);
                        let rects = chars.boxes.get(first..end).map(line_rects).unwrap_or_default();
                        hits.push(SearchHit { start: first as u32, len: (end - first) as u32, rects });
                    }
                }
                if work.was_cancelled() {
                    return Step::Cancelled;
                }
                if work.charge(0.0) {
                    // The meter held to the end.
                } else {
                    s.skipped += 1;
                }
            }
        }
        s.done += 1;
        if !hits.is_empty() {
            s.hits += hits.len() as u32;
            self.tell(Event::SearchHits { doc: self.doc_id, id: s.id, page: page_no, hits });
        }
        if s.hits as usize >= MAX_SEARCH_HITS {
            self.finish_search(s, SearchStatus::HitLimit);
            return Step::Done;
        }
        if s.done >= total {
            self.finish_search(s, SearchStatus::Finished);
            return Step::Done;
        }
        if s.announced.elapsed() >= PROGRESS_EVERY {
            s.announced = Instant::now();
            self.tell(Event::SearchProgress { doc: self.doc_id, id: s.id, pages_done: s.done, pages_total: total, hits: s.hits });
        }
        Step::More
    }

    fn finish_search(&self, s: &SearchJob, status: SearchStatus) {
        self.tell(Event::SearchDone { doc: self.doc_id, id: s.id, status, pages_skipped: s.skipped, hits: s.hits });
    }

    // --- copy -----------------------------------------------------------------------------------------------------

    fn step_copy(&mut self, c: &mut CopyJob) -> Step {
        let reply = |this: &Self, status: TextStatus, message: String, text: String, truncated: bool| {
            this.tell(Event::Copied { doc: this.doc_id, id: c.id, status, message, text, truncated });
        };
        if !self.rights.copy {
            reply(self, TextStatus::Denied, DENIED_COPY.to_string(), String::new(), false);
            return Step::Done;
        }
        let total = u32::try_from(self.pages.len()).unwrap_or(u32::MAX);
        if c.truncated || c.page > c.to.0 || c.page >= total || c.pages_done as usize >= MAX_COPY_PAGES {
            let truncated = c.truncated || (c.page <= c.to.0 && c.page < total);
            let message = if c.skipped > 0 { format!("{} pages could not be read", c.skipped) } else { String::new() };
            reply(self, TextStatus::Ok, message, std::mem::take(&mut c.text), truncated);
            return Step::Done;
        }
        let work = self.meter();
        match self.page_chars(c.page, &work, true) {
            Some(Err(Error::Cancelled)) => return Step::Cancelled,
            None | Some(Err(_)) => c.skipped += 1,
            Some(Ok(chars)) => {
                let from = if c.page == c.from.0 { c.from.1 as usize } else { 0 };
                let to = (if c.page == c.to.0 { c.to.1 as usize } else { usize::MAX }).min(chars.boxes.len());
                if !c.text.is_empty() && !c.text.ends_with('\n') {
                    c.text.push('\n');
                    c.chars += 1;
                }
                if from < to {
                    let room = MAX_COPY_CHARS.saturating_sub(c.chars);
                    let wanted = to - from;
                    let piece: String = chars.text.chars().skip(from).take(wanted.min(room)).collect();
                    c.chars += piece.chars().count();
                    c.text.push_str(&piece);
                    if wanted > room {
                        c.truncated = true;
                    }
                }
            }
        }
        c.page += 1;
        c.pages_done += 1;
        Step::More
    }

    // --- print ----------------------------------------------------------------------------------------------------

    // --- recognising scans (5c) -----------------------------------------------------------------------------------------

    /// One page of the search for scans: its text and its pictures decide.
    fn step_ocr_scan(&mut self, s: &mut OcrScanJob) -> Step {
        let total = u32::try_from(self.pages.len()).unwrap_or(u32::MAX);
        let end = s.only.map_or(total, |p| p.saturating_add(1).min(total));
        if s.next >= end || s.truncated {
            self.tell(Event::OcrPages { doc: self.doc_id, id: s.id, pages: std::mem::take(&mut s.found), looked: s.looked, unreadable: s.unreadable, truncated: s.truncated });
            return Step::Done;
        }
        let work = self.meter();
        let Some(page) = self.pages.get(s.next as usize) else { return Step::Done };
        match self.text.page_layout(page, &work) {
            Ok(layout) => {
                // (A page that has our own layer is done, whatever the number of characters in it.)
                if edit::wants_text(&edit::scan_state(&layout), s.only.is_none()) && !edit::has_ocr_layer(self.document, page) {
                    if s.found.len() >= edit::MAX_OCR_PAGES {
                        s.truncated = true;
                        return Step::More;
                    }
                    s.found.push(s.next);
                }
            }
            Err(Error::Cancelled) => return Step::Cancelled,
            Err(_) => s.unreadable += 1,
        }
        let _ = self.text.take_warnings();
        s.looked += 1;
        s.next += 1;
        Step::More
    }

    /// One band of the picture of a page for the recogniser: grey pixels, the page as it is shown.
    fn step_ocr_image(&mut self, j: &mut OcrImageJob) -> Step {
        let (doc, id, page_no) = (self.doc_id, j.id, j.page);
        let fail = |this: &Self, message: &str| {
            this.tell(Event::OcrImage { doc, id, page: page_no, status: RenderStatus::Failed, message: message.to_string(), width: 0, height: 0, dpi: 0.0, gray: Vec::new() });
            Step::Done
        };
        let Some(page) = self.pages.get(j.page as usize) else { return fail(self, "there is no such page") };
        let (w, h, dpi) = match j.size {
            Some(known) => known,
            None => {
                let (pw, ph) = crate::text::Shown::of(page).size();
                let Some(dpi) = edit::ocr_dpi(pw, ph, j.max_side) else { return fail(self, "the page is too large to be recognised at a useful resolution") };
                let (w, h) = Renderer::page_pixels(page, dpi, 0);
                if w > u64::from(j.max_side) || h > u64::from(j.max_side) || w.saturating_mul(h) > edit::MAX_IMAGE_PIXELS {
                    return fail(self, "the page is too large to be recognised at a useful resolution");
                }
                let (w, h) = (u32::try_from(w).unwrap_or(u32::MAX), u32::try_from(h).unwrap_or(u32::MAX));
                j.gray = vec![255u8; w as usize * h as usize];
                j.size = Some((w, h, dpi));
                (w, h, dpi)
            }
        };
        let rows = (self.max_tile_pixels / w.max(1)).clamp(1, h.saturating_sub(j.y).max(1));
        match self.renderer.render_partial(page, dpi, 0, Some([0, j.y, w, rows])) {
            Err(Error::Cancelled) => Step::Cancelled,
            Err(e) => fail(self, &e.to_string()),
            Ok(partial) => {
                let warnings = self.renderer.take_warnings();
                if j.incomplete.is_empty() {
                    match (&partial.error, partial.work_over, &partial.incomplete) {
                        (Some(e), ..) => j.incomplete = e.to_string(),
                        (None, true, _) => j.incomplete = warnings.iter().find(|m| m.contains("more work")).cloned().unwrap_or_else(|| "the page asks for more work than is allowed".to_string()),
                        (None, false, Some(why)) => j.incomplete = why.clone(),
                        (None, false, None) => {}
                    }
                }
                if partial.bitmap.width != w || partial.bitmap.height != rows {
                    return fail(self, "the page was drawn at a size other than the one asked for");
                }
                let from = j.y as usize * w as usize;
                let (pixels, _) = partial.bitmap.rgba.as_chunks::<4>();
                if let Some(band) = j.gray.get_mut(from..from + pixels.len()) {
                    for (g, px) in band.iter_mut().zip(pixels) {
                        // (Weights of Rec. 601, in 256ths.)
                        *g = ((77 * u32::from(px[0]) + 150 * u32::from(px[1]) + 29 * u32::from(px[2])) >> 8) as u8;
                    }
                }
                j.y += rows;
                if j.y < h {
                    return Step::More;
                }
                let status = if j.incomplete.is_empty() { RenderStatus::Complete } else { RenderStatus::Incomplete };
                self.tell(Event::OcrImage { doc, id, page: page_no, status, message: std::mem::take(&mut j.incomplete), width: w, height: h, dpi, gray: std::mem::take(&mut j.gray) });
                Step::Done
            }
        }
    }

    fn step_print(&mut self, p: &mut PrintJob) -> Step {
        let fail = |this: &Self, p: &PrintJob, message: &str| {
            this.tell(Event::PrintDone { doc: this.doc_id, id: p.id, status: PrintStatus::Failed, pages_done: p.printed, message: message.to_string() });
            Step::Done
        };
        if !p.checked {
            p.checked = true;
            if !self.rights.print {
                return fail(self, p, "the file's author did not allow printing, and the file was not opened with the owner password");
            }
        }
        let Some(&spec) = p.pages.get(p.at) else {
            self.tell(Event::PrintDone { doc: self.doc_id, id: p.id, status: PrintStatus::Finished, pages_done: p.printed, message: String::new() });
            return Step::Done;
        };
        let Some(page) = self.pages.get(spec.page as usize) else { return fail(self, p, "there is no such page") };
        if !spec.rotation.is_multiple_of(90) || !spec.dpi.is_finite() {
            return fail(self, p, "the page is to be turned by something that is not a multiple of 90 degrees");
        }
        let dpi = print_dpi(spec.dpi, self.rights);
        let (w, h) = Renderer::page_pixels(page, dpi, spec.rotation);
        // As many rows as the canvas of the plan holds: the band is the width of the page.
        let rows = (u64::from(self.max_tile_pixels) / w.max(1)).clamp(1, h.max(1));
        let y = u64::from(p.y).min(h.saturating_sub(1));
        let band_h = rows.min(h - y).max(1);
        let region = [0u32, u32::try_from(y).unwrap_or(0), u32::try_from(w).unwrap_or(u32::MAX), u32::try_from(band_h).unwrap_or(u32::MAX)];
        match self.renderer.render_partial(page, dpi, spec.rotation, Some(region)) {
            Err(Error::Cancelled) => Step::Cancelled,
            Err(e) => fail(self, p, &e.to_string()),
            Ok(partial) => {
                // Whatever stopped the page part-way, what was drawn is printed.
                let _ = self.renderer.take_warnings();
                let (width, height) = (partial.bitmap.width, partial.bitmap.height);
                let bgra = to_bgra(partial.bitmap);
                self.tell(Event::PrintBand(PrintBand {
                    doc: self.doc_id,
                    id: p.id,
                    index: u32::try_from(p.at).unwrap_or(u32::MAX),
                    page: spec.page,
                    y: u32::try_from(y).unwrap_or(0),
                    width,
                    height,
                    page_width: u32::try_from(w).unwrap_or(u32::MAX),
                    page_height: u32::try_from(h).unwrap_or(u32::MAX),
                    dpi,
                    bgra,
                }));
                p.waiting = true;
                let next = y + band_h;
                if next >= h {
                    p.at += 1;
                    p.y = 0;
                    p.printed += 1;
                } else {
                    p.y = u32::try_from(next).unwrap_or(u32::MAX);
                }
                Step::More
            }
        }
    }
}

/// The dots per inch a page is printed at when `asked` for: held to what the renderer draws and to [`MAX_PRINT_DPI`], or to
/// [`LOW_QUALITY_PRINT_DPI`] when the file allows only low quality printing.
fn print_dpi(asked: f64, rights: Rights) -> f64 {
    let cap = if rights.print_high_quality { MAX_PRINT_DPI } else { LOW_QUALITY_PRINT_DPI };
    asked.clamp(MIN_DPI, cap.min(MAX_DPI))
}

const DENIED_FILL: &str = "the file's author did not allow filling in form fields, and the file was not opened with the owner password";

const DENIED_ASSEMBLE: &str = "the file's author did not allow inserting, deleting, moving or turning pages, and the file was not opened with the owner password";

const DENIED_EXTRACT: &str = "the file's author did not allow copying or extracting, and the file was not opened with the owner password";

const DENIED_MODIFY: &str = "the file's author did not allow changing its content, and the file was not opened with the owner password";
const DENIED_ANNOTATE: &str = "the file's author did not allow adding annotations, and the file was not opened with the owner password";

const DENIED_COPY: &str = "the file's author did not allow copying text, and the file was not opened with the owner password";

const MEMORY_ONLY: &str = "the file may not be changed, so the recognised text is used in this window only and is not saved.";

const SIGNED_WARNING: &str = "this file is signed: after the annotations are saved, the signature will show the file as changed";

const SIGNED_AND_DAMAGED: &str = "the file is signed and its structure is damaged: it cannot be written afresh without breaking the signature, so it is not edited";

const TOO_MANY_CHANGES: &str = "there are too many unsaved changes: save the file first";

/// RGBA to BGRA, in place.
fn to_bgra(bitmap: Bitmap) -> Vec<u8> {
    let mut bgra = bitmap.rgba;
    let (pixels, _) = bgra.as_chunks_mut::<4>();
    for px in pixels {
        px.swap(0, 2);
    }
    bgra
}

/// Draw one request. `None`: it was cancelled while it was drawn.
fn draw(renderer: &mut Renderer<'_>, pages: &[Page], request: &RenderRequest) -> Option<Rendered> {
    let result = |status: RenderStatus, message: String, width: u32, height: u32, bgra: Vec<u8>| Rendered {
        doc: request.doc,
        id: request.id,
        page: request.page,
        status,
        message,
        width,
        height,
        bgra,
    };
    let Some(page) = pages.get(request.page as usize) else {
        return Some(result(RenderStatus::Failed, "there is no such page".to_string(), 0, 0, Vec::new()));
    };
    let region = (request.width > 0 && request.height > 0).then_some([request.x, request.y, request.width, request.height]);
    let drawn = renderer.render_partial(page, request.dpi, request.rotation, region);
    // What the page said is not kept from one request to the next.
    let warnings = renderer.take_warnings();
    match drawn {
        Err(Error::Cancelled) => None,
        Err(e) => Some(result(RenderStatus::Failed, e.to_string(), 0, 0, Vec::new())),
        Ok(partial) => {
            let (status, message) = match (&partial.error, partial.work_over, &partial.incomplete) {
                (Some(e), ..) => (RenderStatus::Incomplete, e.to_string()),
                (None, true, _) => (
                    RenderStatus::Incomplete,
                    warnings.iter().find(|w| w.contains("more work")).cloned().unwrap_or_else(|| "the page asks for more work than is allowed".to_string()),
                ),
                (None, false, Some(why)) => (RenderStatus::Incomplete, why.clone()),
                (None, false, None) => (RenderStatus::Complete, String::new()),
            };
            let (width, height) = (partial.bitmap.width, partial.bitmap.height);
            let bgra = to_bgra(partial.bitmap);
            Some(result(status, message, width, height, bgra))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_that_allows_only_low_quality_printing_is_drawn_at_150_dpi_at_most() {
        let high = Rights { copy: true, print: true, print_high_quality: true, annotate: true, fill_forms: true, modify: true, assemble: true };
        let low = Rights { print_high_quality: false, ..high };
        assert_eq!((print_dpi(1000.0, high), print_dpi(1000.0, low)), (300.0, 150.0));
        assert_eq!((print_dpi(200.0, high), print_dpi(200.0, low)), (200.0, 150.0));
        assert_eq!((print_dpi(100.0, high), print_dpi(100.0, low)), (100.0, 100.0));
        assert_eq!(print_dpi(0.0, low), MIN_DPI);
    }
}
