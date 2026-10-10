//! The engine thread's serving loop: the one open document, the requests for pieces of pages to draw, and the other
//! requests (bookmarks, links, boxes of characters, search, copy, print).
//!
//! What is done first: the quick requests a person waits for (bookmarks, links, boxes of characters), then the pieces to
//! draw (the most urgent first), and only when nothing else is waiting one step of a long job (a page of a search, of a
//! copy, a band of a print). Every step of those is one page, charged to a work meter of its own that is cancelled by the
//! same flag as the drawing, so a hostile page cannot hold the thread for long.

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use crate::document::{Document, Page};
use crate::error::Error;
use crate::render::work::{Work, cost};
use crate::render::{Bitmap, MAX_DPI, MIN_DPI, MemoryLimits, Renderer};
use crate::text::{PageChars, TextExtractor};

use super::targets::{Names, page_index};
use super::textcache::TextCache;
use super::{
    BACKGROUND_PRIORITY, CharBoxes, Command, Event, LOW_QUALITY_PRINT_DPI, OpenDoc, OpenFailure, Opened, PageInfo, PrintBand, PrintPage, PrintStatus, RenderRequest,
    RenderStatus, Rendered, Request, Rights, SearchHit, SearchStatus, Shared, TextStatus, links, outline, page_info, search,
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
    Search(SearchJob),
    Copy(CopyJob),
    Print(PrintJob),
}

impl Job {
    fn id(&self) -> u64 {
        match self {
            Job::Outline { id } | Job::Links { id, .. } | Job::CharBoxes { id, .. } => *id,
            Job::Search(s) => s.id,
            Job::Copy(c) => c.id,
            Job::Print(p) => p.id,
        }
    }

    /// A quick request that someone is waiting for.
    fn immediate(&self) -> bool {
        matches!(self, Job::Outline { .. } | Job::Links { .. } | Job::CharBoxes { .. })
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

struct Server<'a> {
    doc_id: u64,
    document: &'a Document,
    pages: &'a [Page],
    index: HashMap<u32, u32>,
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
}

/// Serve one open document until a command ends it; that command is returned for the caller to carry out.
pub(super) fn serve(open: OpenDoc, commands: &Receiver<Command>, send: &dyn Fn(Event), shared: &Shared) -> Option<Command> {
    let OpenDoc { doc_id, document, plan, rights } = open;
    let pages = match document.pages() {
        Ok(p) => p,
        Err(e) => {
            send(Event::OpenFailed { doc: doc_id, failure: OpenFailure::Damaged, message: e.to_string() });
            return None;
        }
    };
    let infos: Vec<PageInfo> = pages.iter().map(page_info).collect();
    send(Event::Opened(Opened {
        doc: doc_id,
        pages: infos,
        file_bytes: plan.file_bytes,
        bitmap_cache_bytes: plan.bitmap_cache_bytes,
        max_tile_pixels: plan.max_tile_pixels,
        rights,
    }));
    let to_usize = |v: u64| usize::try_from(v).unwrap_or(usize::MAX);
    let mut renderer = Renderer::new(&document);
    renderer.set_memory_limits(MemoryLimits {
        caches: to_usize(plan.render_caches),
        layers: to_usize(plan.layers),
        masks: to_usize(plan.masks),
        decoders: to_usize(plan.decoders),
        image_slot: to_usize(plan.image_slot),
        canvas_pixels: u64::from(plan.max_tile_pixels),
    });
    renderer.set_cancel(Some(shared.stop.clone()));
    let mut server = Server {
        doc_id,
        document: &document,
        pages: &pages,
        index: page_index(&pages),
        max_tile_pixels: plan.max_tile_pixels,
        rights,
        renderer,
        text: TextExtractor::new(&document),
        chars: TextCache::new(plan.text_cache),
        names: Names::new(&document),
        queue: Vec::new(),
        jobs: VecDeque::new(),
        commands,
        send,
        shared,
    };
    server.run()
}

impl Server<'_> {
    fn run(&mut self) -> Option<Command> {
        loop {
            // Take what has been asked for: wait only when there is nothing to do.
            if self.queue.is_empty() && !self.jobs.iter().any(Job::runnable) {
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
        match draw(&mut self.renderer, self.pages, &request) {
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
            Job::Search(s) => self.step_search(s),
            Job::Copy(c) => self.step_copy(c),
            Job::Print(p) => self.step_print(p),
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
            let c = Rc::new(c);
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
        let found = outline::read(self.document, self.pages, &self.index, &self.names, &work);
        if work.was_cancelled() {
            return Step::Cancelled;
        }
        self.tell(Event::Outline { doc: self.doc_id, id, items: found.items, truncated: found.truncated });
        Step::Done
    }

    fn links(&mut self, id: u64, page: u32) -> Step {
        let work = self.meter();
        let (links, truncated) = match self.pages.get(page as usize) {
            Some(p) => links::read(self.document, p, self.pages, &self.index, &self.names, &work),
            None => (Vec::new(), false),
        };
        if work.was_cancelled() {
            return Step::Cancelled;
        }
        self.tell(Event::Links { doc: self.doc_id, id, page, links, truncated });
        Step::Done
    }

    fn char_boxes(&mut self, id: u64, page: u32) -> Step {
        let doc = self.doc_id;
        let reply = move |status: TextStatus, message: &str, boxes: Vec<[f32; 4]>| {
            Event::CharBoxes(CharBoxes { doc, id, page, status, message: message.to_string(), boxes })
        };
        if !self.rights.copy {
            self.tell(reply(TextStatus::Denied, DENIED_COPY, Vec::new()));
            return Step::Done;
        }
        let work = self.meter();
        match self.page_chars(page, &work, false) {
            None => {
                self.tell(reply(TextStatus::Failed, "there is no such page", Vec::new()));
                Step::Done
            }
            Some(Err(Error::Cancelled)) => Step::Cancelled,
            Some(Err(e)) => {
                self.tell(reply(TextStatus::Failed, &e.to_string(), Vec::new()));
                Step::Done
            }
            Some(Ok(chars)) => {
                self.tell(reply(TextStatus::Ok, "", chars.boxes.clone()));
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

const DENIED_COPY: &str = "the file's author did not allow copying text, and the file was not opened with the owner password";

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
            let (status, message) = match (&partial.error, partial.work_over) {
                (Some(e), _) => (RenderStatus::Incomplete, e.to_string()),
                (None, true) => (
                    RenderStatus::Incomplete,
                    warnings.iter().find(|w| w.contains("more work")).cloned().unwrap_or_else(|| "the page asks for more work than is allowed".to_string()),
                ),
                (None, false) => (RenderStatus::Complete, String::new()),
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
        let high = Rights { copy: true, print: true, print_high_quality: true };
        let low = Rights { print_high_quality: false, ..high };
        assert_eq!((print_dpi(1000.0, high), print_dpi(1000.0, low)), (300.0, 150.0));
        assert_eq!((print_dpi(200.0, high), print_dpi(200.0, low)), (200.0, 150.0));
        assert_eq!((print_dpi(100.0, high), print_dpi(100.0, low)), (100.0, 100.0));
        assert_eq!(print_dpi(0.0, low), MIN_DPI);
    }
}
