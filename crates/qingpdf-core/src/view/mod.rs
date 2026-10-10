//! The reader engine (3d-1, 3d-2): what a viewer window talks to.
//!
//! One thread owns the open document. The window sends requests (open a file, draw this part of that page at this
//! size, find these words, forget a request) and gets results back, all through channels; it never waits for the engine.
//! Every type on this boundary is a plain one (integers, floats, strings, byte vectors, enums without data of Rust's
//! own), so a C interface or a HarmonyOS NAPI wrapper can be put on it later without changing the engine
//! (`docs/view-api.md`).
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

mod annots;
mod editing;
mod geometry;
mod links;
mod outline;
mod plan;
mod search;
mod server;
mod targets;
mod textcache;
#[cfg(test)]
mod reader_tests;
#[cfg(test)]
mod tests;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use crate::document::{Document, Page};
use crate::error::Error;
use crate::ops::has_owner_rights;
use crate::text;

pub use annots::{AnnotInfo, MAX_ANNOT_INFOS, MAX_CONTENTS_CHARS, MAX_PARTS};
pub use editing::{EditResult, EditStatus, MAX_UNDO_STEPS, SaveStatus};
pub use geometry::{CharIndex, is_blank, line_rects, nearest_char};
pub use links::{Link, LinkKind, MAX_LINKS, MAX_URI_BYTES, safe_uri};
pub use outline::{MAX_OUTLINE_DEPTH, MAX_OUTLINE_ITEMS, MAX_TITLE_CHARS, OutlineItem};
pub use crate::edit::{MAX_AUTHOR_CHARS, MAX_INK_POINTS, MAX_INK_STROKES, MAX_INK_WIDTH, MAX_NOTE_CHARS, MAX_QUADS, MIN_INK_WIDTH, MarkupKind};
pub use plan::{MAX_FILE_BYTES, MemoryPlan, TOTAL_BYTES, plan};
pub use search::MAX_QUERY_CHARS;
pub use server::{MAX_COPY_CHARS, MAX_COPY_PAGES, MAX_HITS_PER_PAGE, MAX_PRINT_DPI, MAX_PRINT_PAGES, MAX_SEARCH_HITS};

/// The priority from which a request to draw waits for the quick requests of the reader (bookmarks, links, boxes of
/// characters) instead of going before them: what is in view is drawn first, the rest of the page and the pages
/// round about after them.
pub const BACKGROUND_PRIORITY: u32 = 1000;

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
/// were made. A request below [`BACKGROUND_PRIORITY`] is drawn before the quick requests (bookmarks, links, boxes of
/// characters), one at or above it after them. `id` names the request for [`Engine::cancel`] and in the result; the caller makes the ids up and keeps
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

/// What the file's author allows whoever opened it to do, worked out like the `text` command does: a file that is not
/// encrypted allows everything; for an encrypted one, the owner password (or an owner with no password) lifts every
/// restriction, and otherwise the permission bits of the file decide (a file whose permission check value does not agree
/// with its flags is taken to allow nothing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rights {
    /// Text may be copied (selected text, [`Engine::copy_text`]); the same answer as `text::check_extraction_allowed`.
    pub copy: bool,
    /// The file may be printed.
    pub print: bool,
    /// ... at the best quality. Without it, printing is limited to [`LOW_QUALITY_PRINT_DPI`] dots per inch.
    pub print_high_quality: bool,
    /// Annotations may be added and taken out (permission bit 6, Table 22; the owner password lifts it).
    pub annotate: bool,
}

/// The most dots per inch a page is printed at when the file allows only low-quality printing (Table 22, bit 12).
pub const LOW_QUALITY_PRINT_DPI: f64 = 150.0;

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
    pub rights: Rights,
}

/// Whether text could be had.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum TextStatus {
    Ok = 0,
    /// The file's author did not allow copying text, and the file was not opened with the owner password. (For the boxes of the
    /// characters this is told only when annotating is not allowed either; see [`Engine::char_boxes`].)
    Denied = 1,
    /// The page could not be read for its text (`message` says why: the page asks for more work than is allowed, there is
    /// no such page).
    Failed = 2,
}

/// The boxes of the characters of a page (the answer to [`Engine::char_boxes`]).
#[derive(Debug)]
pub struct CharBoxes {
    pub doc: u64,
    pub id: u64,
    pub page: u32,
    pub status: TextStatus,
    pub message: String,
    /// One `[left, top, right, bottom]` for each character of the page's text, in reading order, in points on the page as it
    /// is shown (turned by its own `/Rotate`, not by the window's rotation; origin at the top left). A space between pieces
    /// and the end of a line have a box with no area. At most [`crate::text::MAX_PAGE_CHARS`] characters.
    pub boxes: Vec<[f32; 4]>,
}

/// One place where the words were found on a page.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchHit {
    /// Where it starts in the page's characters (the order of [`CharBoxes::boxes`]) and how many characters it covers.
    pub start: u32,
    pub len: u32,
    /// What to colour: a rectangle for each line (or column) it covers, in the points of [`CharBoxes`].
    pub rects: Vec<[f32; 4]>,
}

/// How a search ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SearchStatus {
    /// Every page was looked at.
    Finished = 0,
    /// It stopped at [`MAX_SEARCH_HITS`] places.
    HitLimit = 1,
}

/// A page to print and how: it is drawn `dpi` dots to the inch, turned `rotation` degrees (0, 90, 180, 270) further than its own
/// turn. [`Engine::print`] sends it in bands.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PrintPage {
    pub page: u32,
    pub dpi: f64,
    pub rotation: u16,
}

/// A band of a page being printed: rows `y` to `y + height` of the page drawn whole at the dpi of the [`PrintPage`]
/// (`page_width` by `page_height` pixels), `width` pixels across (the whole page's width). BGRA, rows from the top, no padding.
/// The next band is not drawn until [`Engine::print_next`] says this one is done with.
#[derive(Debug)]
pub struct PrintBand {
    pub doc: u64,
    pub id: u64,
    /// Which page of the job this is (from 0, the order given to [`Engine::print`]).
    pub index: u32,
    pub page: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub page_width: u32,
    pub page_height: u32,
    /// The dots per inch the page was drawn at: what was asked for, held to [`MAX_PRINT_DPI`] (to [`LOW_QUALITY_PRINT_DPI`] for a
    /// file that allows only low quality printing) and to what the renderer draws. The window sets the page on the paper by
    /// this, not by what it asked for.
    pub dpi: f64,
    pub bgra: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PrintStatus {
    Finished = 0,
    /// Stopped: the file does not allow printing, or a page could not be drawn (`message` says).
    Failed = 1,
}

#[derive(Debug)]
pub enum Event {
    Opened(Opened),
    OpenFailed { doc: u64, failure: OpenFailure, message: String },
    Rendered(Rendered),
    /// The bookmarks (the answer to [`Engine::outline`]), in display order. `truncated`: there were more than the reader reads.
    Outline { doc: u64, id: u64, items: Vec<OutlineItem>, truncated: bool },
    /// The links of a page (the answer to [`Engine::links`]).
    Links { doc: u64, id: u64, page: u32, links: Vec<Link>, truncated: bool },
    CharBoxes(CharBoxes),
    /// Places found on a page (a page with none is not told); the pages come in the order they are looked at.
    SearchHits { doc: u64, id: u64, page: u32, hits: Vec<SearchHit> },
    /// How far a search is (told every now and then while it runs).
    SearchProgress { doc: u64, id: u64, pages_done: u32, pages_total: u32, hits: u32 },
    /// A search is over. `pages_skipped` pages could not be read for their text (or used up their allowance of work).
    SearchDone { doc: u64, id: u64, status: SearchStatus, pages_skipped: u32, hits: u32 },
    /// The text of a selection (the answer to [`Engine::copy_text`]); lines end with `\n`. `truncated`: the selection was
    /// longer than [`MAX_COPY_PAGES`] pages or [`MAX_COPY_CHARS`] characters and the rest is left out.
    Copied { doc: u64, id: u64, status: TextStatus, message: String, text: String, truncated: bool },
    PrintBand(PrintBand),
    PrintDone { doc: u64, id: u64, status: PrintStatus, pages_done: u32, message: String },
    /// The annotations of a page that can be picked (the answer to [`Engine::annotations`]), in the order of the page's list.
    Annotations { doc: u64, id: u64, page: u32, items: Vec<AnnotInfo>, truncated: bool },
    /// The answer to an edit, an undo or a redo. After [`EditStatus::Done`] what was drawn of the page is out of date (the other
    /// pages are as they were), and the queued requests go on. The file is not changed until it is saved.
    Edited(EditResult),
    /// The answer to [`Engine::save`].
    Saved { doc: u64, id: u64, status: SaveStatus, message: String, bytes: u64 },
}

/// What can be asked of the open document besides drawing.
pub(crate) enum Request {
    Outline,
    Links(u32),
    CharBoxes(u32),
    Search { query: String, start: u32 },
    Copy { from: (u32, u32), to: (u32, u32) },
    Print(Vec<PrintPage>),
    Annotations(u32),
}

pub(crate) enum Command {
    Open { doc: u64, source: Source, password: String, total: u64, view_pixels: u64 },
    Render(RenderRequest),
    Request { doc: u64, id: u64, what: Request },
    Edit { doc: u64, id: u64, edit: crate::edit::Edit },
    Undo { doc: u64, id: u64 },
    Redo { doc: u64, id: u64 },
    Save { doc: u64, id: u64, path: String },
    /// Inside the serving loop only: the document is to be replaced by an edited one.
    Swap,
    PrintNext(u64),
    Cancel(u64),
    Close,
    Quit,
}

pub(crate) enum Source {
    Path(String),
    Bytes(Vec<u8>),
}

/// What the window side and the engine thread share.
pub(crate) struct Shared {
    /// The id of the request being worked on (0: none).
    pub(crate) current: AtomicU64,
    /// Set to stop the request being worked on.
    pub(crate) stop: Arc<AtomicBool>,
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

    fn request(&self, doc: u64, id: u64, what: Request) {
        let _ = self.commands.send(Command::Request { doc, id, what });
    }

    /// Ask for the bookmarks of the file: answered by [`Event::Outline`] (no bookmarks: an empty list).
    pub fn outline(&self, doc: u64, id: u64) {
        self.request(doc, id, Request::Outline);
    }

    /// Ask for the links of a page: answered by [`Event::Links`].
    pub fn links(&self, doc: u64, id: u64, page: u32) {
        self.request(doc, id, Request::Links(page));
    }

    /// Ask for the boxes of the characters of a page, for selecting text: answered by [`Event::CharBoxes`]. A file that allows
    /// neither copying nor annotating answers [`TextStatus::Denied`]; one that allows annotating but not copying gives the boxes (the
    /// positions, not the text: selecting text to mark it works, [`Engine::copy_text`] still answers [`TextStatus::Denied`]).
    pub fn char_boxes(&self, doc: u64, id: u64, page: u32) {
        self.request(doc, id, Request::CharBoxes(page));
    }

    /// Look for `query` in the pages, starting with `start_page` and going on to the end and round to the page before it, one
    /// page at a time between the other work: [`Event::SearchHits`] for each page that has some, [`Event::SearchProgress`] now
    /// and then, and [`Event::SearchDone`] at the end. Capital and small letters are the same, so are full-width and
    /// half-width forms; a word may run over a line end. [`Engine::cancel`] stops it (and then nothing more is told).
    pub fn search(&self, doc: u64, id: u64, query: &str, start_page: u32) {
        self.request(doc, id, Request::Search { query: query.to_string(), start: start_page });
    }

    /// Ask for the text between two places, each a page (from 0) and a character of its characters (the order of
    /// [`CharBoxes::boxes`]): from the first up to, not including, the second. The places may be given in either order.
    /// Answered by [`Event::Copied`], the text laid out as the `text` command does (a file that does not allow copying
    /// answers [`TextStatus::Denied`]).
    pub fn copy_text(&self, doc: u64, id: u64, from: (u32, u32), to: (u32, u32)) {
        self.request(doc, id, Request::Copy { from, to });
    }

    /// Print `pages` (at most [`MAX_PRINT_PAGES`]): each is sent as bands ([`Event::PrintBand`]), one at a time; the next is
    /// drawn after [`Engine::print_next`]. [`Event::PrintDone`] at the end. [`Engine::cancel`] stops it. A file that does not
    /// allow printing ends at once with [`PrintStatus::Failed`]; one that allows only low quality is drawn at no more than
    /// [`LOW_QUALITY_PRINT_DPI`].
    pub fn print(&self, doc: u64, id: u64, pages: Vec<PrintPage>) {
        self.request(doc, id, Request::Print(pages));
    }

    /// The band last sent for print job `id` has been used: draw the next.
    pub fn print_next(&self, id: u64) {
        let _ = self.commands.send(Command::PrintNext(id));
    }

    /// Ask for the annotations of a page that can be picked and taken out (everything but links, form fields and popups):
    /// answered by [`Event::Annotations`].
    pub fn annotations(&self, doc: u64, id: u64, page: u32) {
        self.request(doc, id, Request::Annotations(page));
    }

    /// Mark text: one `[left, top, right, bottom]` for each line, in the points of [`CharBoxes`] (the rectangles of
    /// [`line_rects`] for the selected characters). Answered by [`Event::Edited`]. `author` is who is told to have made it
    /// (at most [`MAX_AUTHOR_CHARS`] characters). Edits are done in the order they are asked for; none while a print job runs.
    #[allow(clippy::too_many_arguments)]
    pub fn add_markup(&self, doc: u64, id: u64, page: u32, kind: MarkupKind, rects: Vec<[f32; 4]>, color: [u8; 3], author: &str) {
        let edit = crate::edit::Edit::Markup { page, kind, rects, color, author: author.to_string() };
        let _ = self.commands.send(Command::Edit { doc, id, edit });
    }

    /// Draw freehand: strokes of points in the points of [`CharBoxes`], `width` points wide (between [`MIN_INK_WIDTH`] and
    /// [`MAX_INK_WIDTH`]). At most [`MAX_INK_STROKES`] strokes; more than [`MAX_INK_POINTS`] points in all are thinned out.
    /// Answered by [`Event::Edited`].
    #[allow(clippy::too_many_arguments)]
    pub fn add_ink(&self, doc: u64, id: u64, page: u32, strokes: Vec<Vec<[f32; 2]>>, color: [u8; 3], width: f32, author: &str) {
        let edit = crate::edit::Edit::Ink { page, strokes, color, width, author: author.to_string() };
        let _ = self.commands.send(Command::Edit { doc, id, edit });
    }

    /// Put a note (an icon that opens `text`, at most [`MAX_NOTE_CHARS`] characters) with its top left corner at `at`.
    /// Answered by [`Event::Edited`].
    #[allow(clippy::too_many_arguments)]
    pub fn add_note(&self, doc: u64, id: u64, page: u32, at: [f32; 2], text: &str, color: [u8; 3], author: &str) {
        let edit = crate::edit::Edit::Note { page, at, text: text.to_string(), color, author: author.to_string() };
        let _ = self.commands.send(Command::Edit { doc, id, edit });
    }

    /// Take an annotation out: the one that [`Event::Annotations`] told as `index` and `num` of the page. Answered by
    /// [`Event::Edited`].
    pub fn delete_annotation(&self, doc: u64, id: u64, page: u32, index: u32, num: u32) {
        let _ = self.commands.send(Command::Edit { doc, id, edit: crate::edit::Edit::Delete { page, index, num } });
    }

    /// Undo the last edit (up to [`MAX_UNDO_STEPS`] of them are kept): answered by [`Event::Edited`].
    pub fn undo(&self, doc: u64, id: u64) {
        let _ = self.commands.send(Command::Undo { doc, id });
    }

    /// Do again what was undone: answered by [`Event::Edited`].
    pub fn redo(&self, doc: u64, id: u64) {
        let _ = self.commands.send(Command::Redo { doc, id });
    }

    /// Write the file, with all the edits, to `path` (the file it was opened from, or a new one): into a file of its own that
    /// replaces `path` only when it is whole. Answered by [`Event::Saved`]. A file that was not changed is written as it is.
    pub fn save(&self, doc: u64, id: u64, path: &str) {
        let _ = self.commands.send(Command::Save { doc, id, path: path.to_string() });
    }

    /// Withdraw a request: dropped if it has not started, stopped at its next operator if it is being worked on. A
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
                Ok((opened, mut session)) => next = server::serve(opened, &mut session, commands, &send, shared),
                Err((failure, message)) => send(Event::OpenFailed { doc, failure, message }),
            },
            // Requests with no document open.
            Command::Render(_)
            | Command::Request { .. }
            | Command::Edit { .. }
            | Command::Undo { .. }
            | Command::Redo { .. }
            | Command::Save { .. }
            | Command::Swap
            | Command::PrintNext(_)
            | Command::Cancel(_)
            | Command::Close => {}
        }
    }
}

pub(crate) struct OpenDoc {
    pub(crate) doc_id: u64,
    pub(crate) document: Document,
    pub(crate) plan: MemoryPlan,
    pub(crate) rights: Rights,
}

fn open(doc_id: u64, source: Source, mut password: String, total: u64, view_pixels: u64) -> Result<(OpenDoc, editing::Session), (OpenFailure, String)> {
    let result = open_inner(doc_id, source, &password, total, view_pixels);
    // The editing session keeps the password for as long as the file is open (to open the edited file again), and wipes it
    // when it ends; this copy is used and gone: overwrite it before it is freed.
    let budget = result.as_ref().map_or(0, |o| o.plan.edit_bytes);
    let session = editing::Session::new(&password, budget);
    wipe(&mut password);
    result.map(|opened| (opened, session))
}

/// Open the bytes of an edited file (the same document with changes appended or cut off) again. `pages` is how many pages it
/// must have. The error is the reason, in English.
pub(crate) fn reopen(doc_id: u64, plan: MemoryPlan, rights: Rights, bytes: Vec<u8>, password: &str, pages: usize) -> Result<OpenDoc, String> {
    let document = Document::from_bytes_with_password(bytes, password).map_err(|e| e.to_string())?;
    if document.is_locked() {
        return Err("the changed file cannot be opened".to_string());
    }
    // What was appended has to be read as it is written: a file that needs its table rebuilt after the change was broken by it.
    if document.was_repaired() {
        return Err("its cross-reference data cannot be read".to_string());
    }
    if document.page_count().map_err(|e| e.to_string())? != pages {
        return Err("the changed file has other pages".to_string());
    }
    document.set_object_stream_cache_limit(usize::try_from(plan.object_streams).unwrap_or(usize::MAX));
    Ok(OpenDoc { doc_id, document, plan, rights })
}

fn open_inner(doc_id: u64, source: Source, password: &str, total: u64, view_pixels: u64) -> Result<OpenDoc, (OpenFailure, String)> {
    let too_large = || (OpenFailure::TooLarge, format!("the file is bigger than {} MB, the most this reader opens", MAX_FILE_BYTES / 1_000_000));
    let bytes = match source {
        Source::Path(path) => {
            let unreadable = |e: std::io::Error| (OpenFailure::Unreadable, e.to_string());
            let size = std::fs::metadata(&path).map_err(unreadable)?.len();
            if plan(total, size, view_pixels).is_none() {
                return Err(too_large());
            }
            // With room after the file for the update that saving appends, so that appending does not copy the file.
            let mut file = std::fs::File::open(&path).map_err(unreadable)?;
            let mut bytes = Vec::new();
            bytes.try_reserve_exact(usize::try_from(size).unwrap_or(0).saturating_add(SPARE_AFTER_FILE)).map_err(|e| (OpenFailure::Unreadable, e.to_string()))?;
            std::io::Read::read_to_end(&mut file, &mut bytes).map_err(unreadable)?;
            bytes
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
    let rights = rights_of(&document, password);
    Ok(OpenDoc { doc_id, document, plan, rights })
}

/// What the file allows, for whoever opened it with `password` (see [`Rights`]).
fn rights_of(document: &Document, password: &str) -> Rights {
    let copy = text::check_extraction_allowed(document, password).is_ok();
    let Some(encryption) = document.encryption() else {
        return Rights { copy, print: true, print_high_quality: true, annotate: true };
    };
    if has_owner_rights(document, &encryption, password) {
        return Rights { copy, print: true, print_high_quality: true, annotate: true };
    }
    Rights {
        copy,
        print: encryption.permissions.print,
        print_high_quality: encryption.permissions.print_high_quality,
        annotate: encryption.permissions.annotate,
    }
}

/// Overwrite a string's bytes with zeros.
fn wipe(s: &mut String) {
    let mut bytes = std::mem::take(s).into_bytes();
    bytes.fill(0);
    std::hint::black_box(&bytes);
}

/// Room left after a file read from the disk for what saving appends to it (an incremental update is mostly a few kilobytes).
const SPARE_AFTER_FILE: usize = 1 << 20;

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
