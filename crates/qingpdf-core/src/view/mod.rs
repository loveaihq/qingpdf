//! The reader engine (3d-1): what a viewer window talks to.
//!
//! One thread owns the open document. The window sends requests (open a file, draw this part of that page at this
//! size, forget a request) and gets results back, all through channels; it never waits for the engine. Every type
//! on this boundary is a plain one (integers, floats, strings, byte vectors, enums without data of Rust's own), so
//! a C interface or a HarmonyOS NAPI wrapper can be put on it later without changing the engine (`docs/view-api.md`).
//!
//! ```no_run
//! # use qingpdf_core::view::{Engine, Event, RenderRequest, TOTAL_BYTES};
//! let engine = Engine::start(Box::new(|| {}));          // the closure is called when a result is waiting
//! engine.open_file(1, "a.pdf", "", TOTAL_BYTES, 1920 * 1080);
//! if let Some(Event::Opened(o)) = engine.wait(5000) {
//!     engine.render(RenderRequest { doc: 1, id: 1, page: 0, dpi: 96.0, rotation: 0, x: 0, y: 0, width: 0, height: 0, priority: 0 });
//!     let _ = o.pages.len();
//! }
//! ```

mod plan;
#[cfg(test)]
mod tests;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use crate::document::{Document, Page};
use crate::error::Error;
use crate::render::{MemoryLimits, Renderer};
use crate::text;

pub use plan::{MAX_FILE_BYTES, MemoryPlan, TOTAL_BYTES, plan};

/// The size of a page as it is shown (the crop box, turned by `/Rotate`), in points (1/72 inch).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageInfo {
    pub width: f64,
    pub height: f64,
}

/// A request to draw part of a page.
///
/// The page is drawn at `dpi` dots per inch (a point is 1/72 inch), turned `rotation` degrees (0, 90, 180 or 270)
/// further than the page's own `/Rotate`. `x`, `y`, `width`, `height` pick a piece of the whole page bitmap at that
/// size, in pixels from its top left corner (a `width` or `height` of 0 asks for all of the page). The result has
/// the size of the piece, cut to the page.
///
/// `priority`: the smaller the number, the sooner it is drawn; requests of the same priority go in the order they
/// were made. `id` names the request for [`Engine::cancel`] and in the result; the caller makes the ids up and keeps
/// them different. A request for a document that is not the open one is dropped.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderRequest {
    pub doc: u64,
    pub id: u64,
    pub page: u32,
    pub dpi: f64,
    pub rotation: u16,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub priority: u32,
}

/// How a piece was drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RenderStatus {
    /// Drawn (things in the page that could not be read were left out, as the command line tool does).
    Complete = 0,
    /// The bitmap is valid but the page stopped early: it asked for more work than is allowed, or its content stream
    /// went wrong part-way. What was drawn before is there.
    Incomplete = 1,
    /// Nothing could be drawn (the bitmap is empty); `message` says why.
    Failed = 2,
}

/// The result of a [`RenderRequest`]. The pixels are 8-bit BGRA (blue first), rows from the top, no padding, and
/// opaque.
#[derive(Debug)]
pub struct Rendered {
    pub doc: u64,
    pub id: u64,
    pub page: u32,
    pub status: RenderStatus,
    /// In English, for a log or a tooltip; empty for [`RenderStatus::Complete`].
    pub message: String,
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

/// Why a file was not opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum OpenFailure {
    /// Encrypted and the password given (none, or the empty one) does not open it: ask for one and open again.
    NeedsPassword = 0,
    /// A password was given and it does not open the file.
    WrongPassword = 1,
    /// The file is bigger than the reader can hold ([`MAX_FILE_BYTES`]).
    TooLarge = 2,
    /// The file cannot be read at all (`message` says what the system reported).
    Unreadable = 3,
    /// Not a PDF, or too damaged to read, or it asks for more than is safe.
    Damaged = 4,
}

/// A file that is open.
#[derive(Debug)]
pub struct Opened {
    pub doc: u64,
    pub pages: Vec<PageInfo>,
    pub file_bytes: u64,
    /// What the window may keep of drawn bitmaps, so that the file, the engine and the bitmaps stay within the total.
    pub bitmap_cache_bytes: u64,
    /// The most pixels one request should ask for (a bigger page is asked for in pieces).
    pub max_tile_pixels: u32,
}

#[derive(Debug)]
pub enum Event {
    Opened(Opened),
    OpenFailed { doc: u64, failure: OpenFailure, message: String },
    Rendered(Rendered),
}

enum Command {
    Open { doc: u64, source: Source, password: String, total: u64, view_pixels: u64 },
    Render(RenderRequest),
    Cancel(u64),
    Close,
    Quit,
}

enum Source {
    Path(String),
    Bytes(Vec<u8>),
}

/// What the window side and the engine thread share.
struct Shared {
    /// The id of the request being drawn (0: none).
    current: AtomicU64,
    /// Set to stop the request being drawn.
    stop: Arc<AtomicBool>,
}

/// The reader engine: a thread that owns the open document, and the channels to it.
pub struct Engine {
    commands: Sender<Command>,
    events: Receiver<Event>,
    shared: Arc<Shared>,
    /// Why the engine thread could not be started, if it could not.
    start_error: Option<String>,
}

impl Engine {
    /// Start the engine thread. `notify` is called (from the engine thread) each time an event is waiting to be
    /// taken with [`Engine::poll`]; it should only wake the window up (post a message), not do work.
    pub fn start(notify: Box<dyn Fn() + Send + 'static>) -> Engine {
        let (commands, command_rx) = mpsc::channel();
        let (event_tx, events) = mpsc::channel();
        let shared = Arc::new(Shared { current: AtomicU64::new(0), stop: Arc::new(AtomicBool::new(false)) });
        let for_thread = shared.clone();
        let started = std::thread::Builder::new().name("qingpdf-engine".to_string()).spawn(move || run(&command_rx, &event_tx, &*notify, &for_thread));
        // No thread: the commands go nowhere, every later call does nothing, and [`Engine::start_error`] says why.
        let start_error = started.err().map(|e| e.to_string());
        Engine { commands, events, shared, start_error }
    }

    /// Why the engine thread could not be started (the system refused a thread), if it could not: then nothing is
    /// ever answered, and the caller should say so instead of waiting.
    pub fn start_error(&self) -> Option<&str> {
        self.start_error.as_deref()
    }

    /// Open the file at `path` (read by the engine thread, so the caller does not wait for the disk). `doc` names the
    /// document in everything that follows; a document that was open is closed first, the request being drawn is
    /// stopped, and its requests are dropped. `password` is tried after the empty one (it is used once and not kept).
    /// `total` is the memory goal in bytes ([`TOTAL_BYTES`]); `view_pixels` the most pixels the window can show (0: not
    /// known): the window's bitmaps are sized from it and the engine's caches give way when the total is short.
    pub fn open_file(&self, doc: u64, path: &str, password: &str, total: u64, view_pixels: u64) {
        let _ = self.commands.send(Command::Open { doc, source: Source::Path(path.to_string()), password: password.to_string(), total, view_pixels });
        // The command goes first: the engine that finds itself stopped looks for it.
        self.shared.stop.store(true, Ordering::SeqCst);
    }

    /// [`Engine::open_file`] for a file already in memory (the engine keeps the vector).
    pub fn open_bytes(&self, doc: u64, bytes: Vec<u8>, password: &str, total: u64, view_pixels: u64) {
        let _ = self.commands.send(Command::Open { doc, source: Source::Bytes(bytes), password: password.to_string(), total, view_pixels });
        self.shared.stop.store(true, Ordering::SeqCst);
    }

    /// Ask for a piece of a page to be drawn (see [`RenderRequest`]).
    pub fn render(&self, request: RenderRequest) {
        let _ = self.commands.send(Command::Render(request));
    }

    /// Withdraw a request: dropped if it has not started, stopped at its next operator if it is being drawn. A
    /// cancelled request gets no result (one that was finished already may still arrive; ignore it).
    pub fn cancel(&self, id: u64) {
        // The command goes first: the engine that finds itself stopped looks for it.
        let _ = self.commands.send(Command::Cancel(id));
        if self.shared.current.load(Ordering::SeqCst) == id {
            self.shared.stop.store(true, Ordering::SeqCst);
        }
    }

    /// Close the document and drop every request.
    pub fn close(&self) {
        let _ = self.commands.send(Command::Close);
        self.shared.stop.store(true, Ordering::SeqCst);
    }

    /// The next event if there is one; never waits.
    pub fn poll(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    /// The next event, waiting up to `milliseconds` for it (for programs with no event loop: tests, batch tools).
    pub fn wait(&self, milliseconds: u64) -> Option<Event> {
        self.events.recv_timeout(Duration::from_millis(milliseconds)).ok()
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // The thread stops at its next operator and then at the closed channel; it is not waited for.
        let _ = self.commands.send(Command::Quit);
        self.shared.stop.store(true, Ordering::SeqCst);
    }
}

/// The engine thread: wait for a file to open, serve it until another command ends it, repeat.
fn run(commands: &Receiver<Command>, events: &Sender<Event>, notify: &(dyn Fn() + Send), shared: &Shared) {
    let send = |event: Event| {
        if events.send(event).is_ok() {
            notify();
        }
    };
    let mut next: Option<Command> = None;
    loop {
        let command = match next.take() {
            Some(c) => c,
            None => match commands.recv() {
                Ok(c) => c,
                Err(_) => return,
            },
        };
        match command {
            Command::Quit => return,
            Command::Open { doc, source, password, total, view_pixels } => match open(doc, source, password, total, view_pixels) {
                Ok(opened) => next = serve(opened, commands, &send, shared),
                Err((failure, message)) => send(Event::OpenFailed { doc, failure, message }),
            },
            // Requests with no document open.
            Command::Render(_) | Command::Cancel(_) | Command::Close => {}
        }
    }
}

struct OpenDoc {
    doc_id: u64,
    document: Document,
    plan: MemoryPlan,
}

fn open(doc_id: u64, source: Source, mut password: String, total: u64, view_pixels: u64) -> Result<OpenDoc, (OpenFailure, String)> {
    let result = open_inner(doc_id, source, &password, total, view_pixels);
    // The password is used and gone: overwrite it before it is freed.
    wipe(&mut password);
    result
}

fn open_inner(doc_id: u64, source: Source, password: &str, total: u64, view_pixels: u64) -> Result<OpenDoc, (OpenFailure, String)> {
    let too_large = || (OpenFailure::TooLarge, format!("the file is bigger than {} MB, the most this reader opens", MAX_FILE_BYTES / 1_000_000));
    let bytes = match source {
        Source::Path(path) => {
            let size = std::fs::metadata(&path).map_err(|e| (OpenFailure::Unreadable, e.to_string()))?.len();
            if plan(total, size, view_pixels).is_none() {
                return Err(too_large());
            }
            std::fs::read(&path).map_err(|e| (OpenFailure::Unreadable, e.to_string()))?
        }
        Source::Bytes(b) => b,
    };
    let plan = plan(total, bytes.len() as u64, view_pixels).ok_or_else(too_large)?;
    let document = Document::from_bytes_with_password(bytes, password).map_err(|e| match e {
        Error::PasswordRequired => (OpenFailure::NeedsPassword, e.to_string()),
        Error::WrongPassword => (OpenFailure::WrongPassword, e.to_string()),
        other => (OpenFailure::Damaged, other.to_string()),
    })?;
    if document.is_locked() {
        return Err((OpenFailure::NeedsPassword, Error::PasswordRequired.to_string()));
    }
    document.set_object_stream_cache_limit(usize::try_from(plan.object_streams).unwrap_or(usize::MAX));
    Ok(OpenDoc { doc_id, document, plan })
}

/// Overwrite a string's bytes with zeros.
fn wipe(s: &mut String) {
    let mut bytes = std::mem::take(s).into_bytes();
    bytes.fill(0);
    std::hint::black_box(&bytes);
}

/// The most a side of a page is told to be, in points (Annex C of the spec: implementation limit of 14,400 units; `/UserUnit`
/// is not applied by this engine). A page with a bigger box is shown cut at this size, from its top left corner; a box of
/// `[0 0 1e300 1e300]` would otherwise overflow the window's arithmetic.
pub const MAX_PAGE_UNITS: f64 = 14_400.0;

/// The size of each page as shown.
fn page_info(page: &Page) -> PageInfo {
    let [x0, y0, x1, y1] = text::visible_box(page).unwrap_or([0.0, 0.0, 612.0, 792.0]);
    let (w, h) = ((x1 - x0).min(MAX_PAGE_UNITS), (y1 - y0).min(MAX_PAGE_UNITS));
    if page.rotate().rem_euclid(360) % 180 == 90 { PageInfo { width: h, height: w } } else { PageInfo { width: w, height: h } }
}

/// Serve one open document until a command ends it; that command is returned for the caller to carry out.
fn serve(open: OpenDoc, commands: &Receiver<Command>, send: &dyn Fn(Event), shared: &Shared) -> Option<Command> {
    let OpenDoc { doc_id, document, plan } = open;
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
    let mut queue: Vec<RenderRequest> = Vec::new();
    loop {
        // Take what has been asked for: wait only when there is nothing to draw.
        if queue.is_empty() {
            match commands.recv() {
                Ok(c) => {
                    if let Some(end) = take(c, doc_id, &mut queue) {
                        return Some(end);
                    }
                }
                Err(_) => return Some(Command::Quit),
            }
        }
        while let Ok(c) = commands.try_recv() {
            if let Some(end) = take(c, doc_id, &mut queue) {
                return Some(end);
            }
        }
        // The most urgent request; of equal ones, the oldest (the queue is in the order they came).
        let Some(index) = queue.iter().enumerate().min_by_key(|(i, r)| (r.priority, *i)).map(|(i, _)| i) else { continue };
        let request = queue.remove(index);
        shared.stop.store(false, Ordering::SeqCst);
        shared.current.store(request.id, Ordering::SeqCst);
        // A cancel that came between taking the commands and now would find nothing being drawn: look again.
        let mut cancelled = false;
        while let Ok(c) = commands.try_recv() {
            match c {
                Command::Cancel(id) if id == request.id => cancelled = true,
                other => {
                    if let Some(end) = take(other, doc_id, &mut queue) {
                        shared.current.store(0, Ordering::SeqCst);
                        return Some(end);
                    }
                }
            }
        }
        if !cancelled {
            match draw(&mut renderer, &pages, &request) {
                Some(result) => {
                    shared.current.store(0, Ordering::SeqCst);
                    send(Event::Rendered(result));
                }
                None => {
                    // Stopped: by the cancel of this request, or by a flag raised late for the one before it. The
                    // cancel command was sent before the flag, so it is there to be found.
                    let mut really = false;
                    while let Ok(c) = commands.try_recv() {
                        match c {
                            Command::Cancel(id) if id == request.id => really = true,
                            other => {
                                if let Some(end) = take(other, doc_id, &mut queue) {
                                    shared.current.store(0, Ordering::SeqCst);
                                    return Some(end);
                                }
                            }
                        }
                    }
                    if !really {
                        queue.insert(0, request);
                    }
                }
            }
        }
        shared.current.store(0, Ordering::SeqCst);
    }
}

/// Carry out a command that is not about drawing, or queue the request; a command that ends the document is returned.
fn take(command: Command, doc_id: u64, queue: &mut Vec<RenderRequest>) -> Option<Command> {
    match command {
        Command::Render(r) => {
            if r.doc == doc_id {
                queue.push(r);
            }
            None
        }
        Command::Cancel(id) => {
            queue.retain(|r| r.id != id);
            None
        }
        end @ (Command::Open { .. } | Command::Close | Command::Quit) => Some(end),
    }
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
            let mut bgra = partial.bitmap.rgba;
            // RGBA to BGRA, in place.
            let (pixels, _) = bgra.as_chunks_mut::<4>();
            for px in pixels {
                px.swap(0, 2);
            }
            Some(result(status, message, width, height, bgra))
        }
    }
}
