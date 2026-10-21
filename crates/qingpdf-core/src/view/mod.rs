//! The reader engine (3d-1, 3d-2): what a viewer window talks to.
//!
//! One thread owns the open documents, and serves one of them at a time (the tab that is shown); the others wait, with their file and
//! their edits that are not saved, until [`Engine::activate`] makes one of them the served one, or are let go of ([`Event::Dropped`]) to
//! make room for another file. The window sends requests (open a file, draw this part of that page at this
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
mod forms;
mod geometry;
mod links;
mod outline;
mod plan;
pub(crate) mod search;
mod server;
mod store;
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

pub use annots::{AnnotInfo, ListedAnnot, MAX_ANNOT_INFOS, MAX_CONTENTS_CHARS, MAX_FULL_TEXT_CHARS, MAX_LISTED_ANNOTS, MAX_LISTED_CHARS, MAX_PARTS};
pub use forms::{MAX_FIELD_INFOS, MAX_FIELDS_BYTES};
pub use crate::edit::MAX_OPTION_BYTES;
pub use crate::edit::{EditClass, FieldInfo, FieldKind, FillValue, MAX_FONT_SIZE, MAX_FREETEXT_CHARS, MAX_STAMP_FILE_BYTES, MAX_STAMP_PIXELS, MAX_VALUE_CHARS, MIN_FONT_SIZE};
pub use crate::edit::{LineView, MAX_TEXT_CHARS};
pub use crate::edit::{ALL_PAGES, NO_PAGE, MAX_MARGIN_MM, MAX_PAGINATION_PAGES, MAX_START, MAX_TEMPLATE_CHARS, MAX_WATERMARK_CHARS, MAX_WATERMARK_PAGES, Pagination, SLOTS, Watermark};
pub use crate::edit::{MAX_INSERT_FILE_BYTES, MAX_INSERT_PAGES, MAX_PAGES_AFTER, order_after_move};
pub use crate::edit::{MAX_IMAGE_PIXELS, MAX_OCR_PAGES, MAX_WORDS_PER_PAGE, MAX_WORDS_PER_RUN, MIN_OCR_DPI, OcrLine, OcrPage, OcrWord, TARGET_DPI};
pub use editing::{EditResult, EditStatus, MAX_UNDO_STEPS, SaveStatus};
pub use geometry::{CharIndex, is_blank, line_rects, nearest_char};
pub use links::{Link, LinkKind, MAX_LINKS, MAX_URI_BYTES, safe_uri};
pub use outline::{MAX_OUTLINE_DEPTH, MAX_OUTLINE_ITEMS, MAX_TITLE_CHARS, OutlineItem, read_bookmarks};
pub(crate) use outline::walk as walk_outline;
pub use crate::edit::{MAX_AUTHOR_CHARS, MAX_INK_POINTS, MAX_INK_STROKES, MAX_INK_WIDTH, MAX_NOTE_CHARS, MAX_QUADS, MIN_INK_WIDTH, MarkupKind, ShapeKind, keep_on_page};
pub use crate::edit::{FindSpec, Preset, REDACT_COLOR};
pub use plan::{ACTIVE_FLOOR, MAX_FILE_BYTES, MemoryPlan, THUMB_CACHE_BYTES, TOTAL_BYTES, plan};

/// The biggest file the reader compresses: the file, the new file and the new pictures are held at once, with the pictures being
/// worked on (`docs/decisions.md`, compressing); a bigger one is for `qingpdf compress`.
pub const MAX_COMPRESS_FILE: u64 = 30_000_000;
/// The biggest file the reader redacts (the same reasons; a bigger one is for `qingpdf redact`).
pub const MAX_REDACT_FILE: u64 = 40_000_000;
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
/// them different. A request for a document that is not the served one is dropped.
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
    /// The documents kept for the other tabs leave no room for this one (the file alone is small enough to open): none of them can be let go
    /// of, since each has edits that are not saved or was opened with a password. Save or close some, and try again.
    NoRoom = 5,
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
    /// Annotations (text boxes and stamps too) may be added and taken out (permission bit 6, Table 22; the owner password lifts it).
    pub annotate: bool,
    /// Form fields may be filled in (permission bit 9, or bit 6 of a file of revision 2; the owner password lifts it).
    pub fill_forms: bool,
    /// The content of pages may be changed: text, pictures and watermarks added (permission bit 4; the owner password lifts it).
    pub modify: bool,
    /// Pages may be turned, taken out, moved and put in (permission bit 11, "assemble", of revision 3 and later, bit 4 of revision 2; the
    /// owner password lifts it).
    pub assemble: bool,
}

/// How the open file is encrypted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cipher {
    Rc4,
    Aes128,
    Aes256,
    /// Only the embedded files are encrypted, or some mixture: the box says "other".
    Other,
}

/// How the open file is protected: what File > Passwords and permissions shows, and whether it can be changed there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Protected {
    /// How the file is encrypted; `None` for a file that is not.
    pub cipher: Option<Cipher>,
    /// A password is needed to open it (the empty one does not).
    pub needs_password: bool,
    /// What the file's flags allow (everything for a file that is not encrypted). Not what whoever opened it may do: the owner
    /// password lifts every restriction ([`Rights`]).
    pub permissions: crate::security::Permissions,
    /// Whoever opened it may change its protection: the file is not encrypted, or the owner password opened it, or it allows
    /// everything anyway (the rule of `decrypt`).
    pub may_change: bool,
    /// The file is signed or certified: writing it again would break the signature, so its protection is not changed.
    pub signed: bool,
}

/// What a protection request does with the file ([`Engine::protect`]).
pub enum ProtectChange {
    /// Encrypt it as this says, replacing any encryption it has.
    Set(Box<crate::security::Protection>),
    /// Take the encryption off.
    Remove,
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
    /// What the window may keep of thumbnails (small pictures of whole pages), besides `bitmap_cache_bytes`.
    pub thumb_cache_bytes: u64,
    /// The most pixels one request should ask for (a bigger page is asked for in pieces).
    pub max_tile_pixels: u32,
    pub rights: Rights,
    pub protection: Protected,
    /// How the file asks to have its pages laid out (`/PageLayout`): [`SPREAD_NONE`] for one column, one page after the other (also
    /// when it says nothing), [`SPREAD_FIRST_LEFT`] for two columns with the first page on the left (`TwoColumnLeft`, `TwoPageLeft`),
    /// [`SPREAD_FIRST_RIGHT`] for two columns with the first page alone on the right (`TwoColumnRight`, `TwoPageRight`).
    pub spread_hint: u8,
    /// The file says the pages read from right to left (`/ViewerPreferences` `/Direction` `/R2L`): in two columns the first page of a pair
    /// goes on the right.
    pub right_to_left: bool,
}

pub const SPREAD_NONE: u8 = 0;
pub const SPREAD_FIRST_LEFT: u8 = 1;
pub const SPREAD_FIRST_RIGHT: u8 = 2;

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
    /// The characters themselves, one `char` for each box and in the same order (a space the layout put between two pieces is a
    /// space, the end of a line is a line feed): what a double or triple click needs to find a word or a line. Empty for a file that
    /// may be annotated but not copied (the positions only, as for [`Engine::copy_text`]).
    pub text: String,
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
    /// A document the engine kept for a tab is served again (the answer to [`Engine::activate`]): what the window may keep of bitmaps and
    /// thumbnails now (the other documents kept take their share of the total, so it is not what it was), and the most pixels of a request.
    /// Whatever was asked of the document before is forgotten (see [`Engine::activate`]).
    Activated { doc: u64, bitmap_cache_bytes: u64, thumb_cache_bytes: u64, max_tile_pixels: u32 },
    /// The engine let go of a document it kept for a tab, to make room for another: it is not open any more, and the window opens the file
    /// again ([`Engine::open_file`]) when the tab is wanted. Only a document with no edits that are not saved and no password goes. Also the
    /// answer to [`Engine::activate`] for a document that is not there.
    Dropped { doc: u64 },
    Rendered(Rendered),
    /// The bookmarks (the answer to [`Engine::outline`]), in display order. `truncated`: there were more than the reader reads.
    Outline { doc: u64, id: u64, items: Vec<OutlineItem>, truncated: bool },
    /// The bookmarks made from the headings of the document (the answer to [`Engine::headings`]): `Done` with the list (`truncated`: there
    /// were more headings than bookmarks may be), or `Failed` (`message` says why, for instance that the file does not allow taking text out).
    Headings { doc: u64, id: u64, status: SaveStatus, message: String, items: Vec<OutlineItem>, truncated: bool },
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
    /// Every annotation of the document that can be picked, page by page in the order of each page's list (the answer to [`Engine::annot_list`]);
    /// `truncated`: some are left out (more than [`MAX_LISTED_ANNOTS`] in all, or more than [`MAX_ANNOT_INFOS`] on a page).
    AnnotList { doc: u64, id: u64, items: Vec<ListedAnnot>, truncated: bool },
    /// The whole text of an annotation (the answer to [`Engine::annotation_text`]); `found` is false when it is not there any more.
    AnnotText { doc: u64, id: u64, page: u32, index: u32, num: u32, found: bool, text: String },
    /// The form fields of a page (the answer to [`Engine::fields`]), in the order of the page's list (the order Tab goes in).
    Fields { doc: u64, id: u64, page: u32, items: Vec<FieldInfo>, truncated: bool },
    /// The line of text at a place (the answer to [`Engine::text_line`]): `line` is `None` when there is no text near it; `status`
    /// is [`TextStatus::Denied`] when the file does not allow changing its content, [`TextStatus::Failed`] when it is signed or the
    /// page could not be read (`message` says).
    TextLine { doc: u64, id: u64, page: u32, status: TextStatus, message: String, line: Option<LineView> },
    /// The options of a combo box or list (the answer to [`Engine::options`]): (export value, shown text), at most
    /// [`MAX_OPTION_BYTES`] of text; `truncated`: some were left out. Empty when the field is not there any more or is no choice.
    Options { doc: u64, id: u64, page: u32, index: u32, num: u32, items: Vec<(String, String)>, truncated: bool },
    /// The answer to an edit, an undo or a redo. After [`EditStatus::Done`] what was drawn of the page is out of date (the other
    /// pages are as they were), and the queued requests go on. The file is not changed until it is saved.
    Edited(EditResult),
    /// The answer to [`Engine::save`].
    Saved { doc: u64, id: u64, status: SaveStatus, message: String, bytes: u64 },
    /// The pages worth recognising (the answer to [`Engine::ocr_scan`]), in page order, at most [`MAX_OCR_PAGES`] (`truncated`: there
    /// were more). `looked` pages were looked at; `unreadable` of them could not be read for their text and are not in the list.
    OcrPages { doc: u64, id: u64, pages: Vec<u32>, looked: u32, unreadable: u32, truncated: bool },
    /// A page drawn for the recogniser (the answer to [`Engine::ocr_image`]): `width` by `height` pixels of grey (8 bits, 0 black, rows
    /// from the top, no padding) showing the whole page as it is shown, at `dpi` dots to the inch. `status` is [`RenderStatus::Failed`]
    /// (nothing drawn, `message` says why) or [`RenderStatus::Incomplete`] (the page stopped part-way, `message` says) or complete.
    OcrImage { doc: u64, id: u64, page: u32, status: RenderStatus, message: String, width: u32, height: u32, dpi: f64, gray: Vec<u8> },
    /// The answer to [`Engine::protect`]: the file at the path was written (`Done`, `bytes` long), or not (`Failed`, `message` says why).
    Protected { doc: u64, id: u64, status: SaveStatus, message: String, bytes: u64 },
    /// The answer to [`Engine::export_docx`] and [`Engine::export_xlsx`]: `Done` (`message` is what the conversion warned of, if
    /// anything; `pages` how many pages were converted; `tables` how many tables are in the file: a Word table or an Excel sheet, and 0
    /// for an Excel file made of the text of the pages) or `Failed` (`message` says why; no file was written).
    Exported { doc: u64, id: u64, status: SaveStatus, message: String, pages: u32, tables: u32, bytes: u64 },
    /// How far [`Engine::compress`] is, 0 to 100 (told every now and then while it runs).
    CompressProgress { doc: u64, id: u64, percent: u32 },
    /// The answer to [`Engine::compress`]: `Done` with the size of the file before and after (`after` is `before` and `kept` is true
    /// when the result would not have been smaller: no file was written), or `Failed` (`message` says why; no file was written).
    /// `message` also holds what the work warned of.
    Compressed { doc: u64, id: u64, status: SaveStatus, message: String, before: u64, after: u64, kept: bool },
    /// The answer to [`Engine::redact`]: `Done` (the file at the path was written, `bytes` long; `pages` pages had something taken out, for
    /// `marks` marks; `message` is what the work warned of) or `Failed` (`message` says why; no file was written).
    Redacted { doc: u64, id: u64, status: SaveStatus, message: String, bytes: u64, pages: u32, marks: u32 },
}

/// What can be asked of the open document besides drawing.
pub(crate) enum Request {
    Outline,
    Headings(Option<u32>),
    Links(u32),
    CharBoxes(u32),
    Search { query: String, start: u32 },
    Copy { from: (u32, u32), to: (u32, u32) },
    Print(Vec<PrintPage>),
    Annotations(u32),
    AnnotList,
    AnnotText { page: u32, index: u32, num: u32 },
    Fields(u32),
    Options { page: u32, index: u32, num: u32 },
    TextLine { page: u32, at: [f32; 2] },
    OcrScan { only: Option<u32> },
    OcrImage { page: u32, max_side: u32 },
}

pub(crate) enum Command {
    Open { doc: u64, source: Source, password: String, total: u64, view_pixels: u64 },
    Activate { doc: u64, total: u64, view_pixels: u64 },
    CloseDoc { doc: u64 },
    Render(RenderRequest),
    Request { doc: u64, id: u64, what: Request },
    Edit { doc: u64, id: u64, edit: crate::edit::Edit },
    Undo { doc: u64, id: u64 },
    Redo { doc: u64, id: u64 },
    Save { doc: u64, id: u64, path: String, whole: bool },
    Export { doc: u64, id: u64, path: String, excel: bool },
    Extract { doc: u64, id: u64, pages: Vec<u32>, path: String },
    Protect { doc: u64, id: u64, path: String, job: Box<ProtectJob> },
    Compress { doc: u64, id: u64, path: String, how: CompressHow },
    Redact { doc: u64, id: u64, path: String, how: RedactHow },
    /// Inside the serving loop only: the document is to be replaced by an edited one.
    Swap,
    PrintNext(u64),
    Cancel(u64),
    /// Every document.
    Close,
    Quit,
}

/// What [`Engine::compress`] asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompressHow {
    pub preset: crate::compress::Preset,
    /// Only what loses nothing: the pictures are not changed.
    pub lossless_only: bool,
}

/// What [`Engine::redact`] asks for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RedactHow {
    /// The words on the boxes that have none of their own (empty: none).
    pub label: String,
    /// Leave the information dictionary and the metadata out of the new file.
    pub clear_metadata: bool,
    /// Take the attached files and the scripts out of the new file.
    pub remove_active: bool,
}

/// What [`Engine::protect`] asks for, and the seed of the random bytes it needs.
pub(crate) struct ProtectJob {
    change: ProtectChange,
    seed: [u8; 32],
}

pub(crate) enum Source {
    Path(String),
    #[cfg(test)]
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

    /// Open the file at `path` (read by the engine thread, so the caller does not wait for the disk) and serve it. `doc` names the
    /// document in everything that follows (a number not used before). The document that was served waits: the request being drawn is
    /// stopped and its requests are dropped, but the document, with its edits that are not saved, is kept for [`Engine::activate`] (a
    /// document with the same number is closed). `password` is tried after the empty one (it is used once and not kept).
    /// `total` is the memory goal in bytes ([`TOTAL_BYTES`]) for all the documents together; `view_pixels` the most pixels the window can
    /// show (0: not known): the window's bitmaps are sized from it and the engine's caches give way when the total is short. The
    /// documents that wait take their bytes off the total; when there is not enough left, the ones that can be let go of go
    /// ([`Event::Dropped`], the least recently served first), and when that is not enough the answer is [`OpenFailure::NoRoom`]. A file that
    /// does not open leaves no document served until [`Engine::activate`] is called for one.
    pub fn open_file(&self, doc: u64, path: &str, password: &str, total: u64, view_pixels: u64) {
        let _ = self.commands.send(Command::Open { doc, source: Source::Path(path.to_string()), password: password.to_string(), total, view_pixels });
        // The command goes first: the engine that finds itself stopped looks for it.
        self.shared.stop.store(true, Ordering::SeqCst);
    }

    /// [`Engine::open_file`] for a file already in memory (the engine keeps the vector).
    #[cfg(test)]
    pub fn open_bytes(&self, doc: u64, bytes: Vec<u8>, password: &str, total: u64, view_pixels: u64) {
        let _ = self.commands.send(Command::Open { doc, source: Source::Bytes(bytes), password: password.to_string(), total, view_pixels });
        self.shared.stop.store(true, Ordering::SeqCst);
    }

    /// Serve the document `doc` that waits (it was opened and then another was opened or activated), the served one waiting instead: the
    /// request being drawn is stopped and the requests of the document that stops being served are dropped, so the window asks for what it
    /// wants of `doc` after [`Event::Activated`] (which says what the plan is now; requests sent before it are fine, they are served in
    /// order). A document that is being served already is answered again, nothing else happens. One that is not there (let go of: the answer
    /// is [`Event::Dropped`]) must be opened again. Edits, saves and exports asked of the served document are done before it stops being
    /// served; searches, prints and the like are not (the window waits for them or cancels them first).
    pub fn activate(&self, doc: u64, total: u64, view_pixels: u64) {
        let _ = self.commands.send(Command::Activate { doc, total, view_pixels });
        self.shared.stop.store(true, Ordering::SeqCst);
    }

    /// Close a document, the served one or one that waits, with whatever it holds (its edits that are not saved too). Nothing is told. If
    /// it is the served one no document is served until [`Engine::activate`].
    pub fn close_doc(&self, doc: u64) {
        let _ = self.commands.send(Command::CloseDoc { doc });
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

    /// Ask for the bookmarks that the headings of the document make (or of the one page `only`, whose bookmarks all lead to it): answered by
    /// [`Event::Headings`]. Reads every page for its text (cancelled by [`Engine::cancel`]).
    pub fn headings(&self, doc: u64, id: u64, only: Option<u32>) {
        self.request(doc, id, Request::Headings(only));
    }

    /// Make the bookmarks of the file the ones in `items` (the list as [`Event::Outline`] gave it, changed: each item still has the `id` it
    /// had, a new one has 0): answered by [`Event::Edited`] like every edit; undone and redone with the others, and written when the file is
    /// saved. The edit is to no page ([`NO_PAGE`]): after it, and after the undo or redo of one, ask for the bookmarks again.
    pub fn set_bookmarks(&self, doc: u64, id: u64, items: Vec<OutlineItem>) {
        let _ = self.commands.send(Command::Edit { doc, id, edit: crate::edit::Edit::Outline(items) });
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

    /// Ask for every annotation of the document that can be picked (not links, form fields, popups or hidden ones), page after page, at most
    /// [`MAX_LISTED_ANNOTS`]: answered by one [`Event::AnnotList`], worked out a page at a time between the other work.
    pub fn annot_list(&self, doc: u64, id: u64) {
        self.request(doc, id, Request::AnnotList);
    }

    /// Ask for the whole text (`/Contents`, at most [`MAX_FULL_TEXT_CHARS`] characters) of an annotation, to edit it: answered by [`Event::AnnotText`].
    pub fn annotation_text(&self, doc: u64, id: u64, page: u32, index: u32, num: u32) {
        self.request(doc, id, Request::AnnotText { page, index, num });
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

    /// Ask for the form fields of a page: answered by [`Event::Fields`].
    pub fn fields(&self, doc: u64, id: u64, page: u32) {
        self.request(doc, id, Request::Fields(page));
    }

    /// Ask for the line of text at `at` (points of the page as shown), to edit it: answered by [`Event::TextLine`]. A line is what the
    /// reader selects and searches as one: the characters drawn one after the other along one baseline.
    pub fn text_line(&self, doc: u64, id: u64, page: u32, at: [f32; 2]) {
        self.request(doc, id, Request::TextLine { page, at });
    }

    /// Change the line of text at `at` on the page, which reads `old` (what [`Engine::text_line`] said; if the page says something
    /// else now the change is refused), into `text`: the old letters are taken out of the page's content and the new ones drawn where
    /// they were, in the same size, colour and spacing (the font of the old text where it has the glyphs, else a system font that
    /// looks like it). At most [`MAX_TEXT_CHARS`] characters, no line end. Needs [`Rights::modify`] and a file without signatures.
    /// Answered by [`Event::Edited`]; its `message` says when the new text is wider than the old.
    pub fn replace_line(&self, doc: u64, id: u64, page: u32, at: [f32; 2], old: &str, text: &str) {
        let edit = crate::edit::Edit::ReplaceLine { page, at, old: old.to_string(), text: text.to_string() };
        let _ = self.commands.send(Command::Edit { doc, id, edit });
    }

    /// Ask for the options of a combo box or list, when a person is about to choose from them: the field that [`Event::Fields`] told
    /// as `index` and `num` of the page. Answered by [`Event::Options`]. (The lists of fields do not carry options: a form whose
    /// fields share one long list would be sent that list for every field.)
    pub fn options(&self, doc: u64, id: u64, page: u32, index: u32, num: u32) {
        self.request(doc, id, Request::Options { page, index, num });
    }

    /// Fill in a form field: the one that [`Event::Fields`] told as `index` and `num` of the page, with `value` (text for a text
    /// field, the export value or the shown text of one of the choices of a combo box or list, on or off for a check box or
    /// radio button). Answered by [`Event::Edited`]; needs the right to fill in forms ([`Rights::fill_forms`]).
    pub fn fill_field(&self, doc: u64, id: u64, page: u32, index: u32, num: u32, value: FillValue) {
        let _ = self.commands.send(Command::Edit { doc, id, edit: crate::edit::Edit::Fill { page, index, num, value } });
    }

    /// Put a text box on the page: `rect` is `[left, top, right, bottom]` in the points of [`CharBoxes`]; the text is `size`
    /// points high (between [`MIN_FONT_SIZE`] and [`MAX_FONT_SIZE`]), at most [`MAX_FREETEXT_CHARS`] characters; the box grows
    /// downwards if the text needs more room. Answered by [`Event::Edited`].
    #[allow(clippy::too_many_arguments)]
    pub fn add_freetext(&self, doc: u64, id: u64, page: u32, rect: [f32; 4], text: &str, size: f32, color: [u8; 3], author: &str) {
        let edit = crate::edit::Edit::FreeText { page, rect, text: text.to_string(), size, color, author: author.to_string() };
        let _ = self.commands.send(Command::Edit { doc, id, edit });
    }

    /// Stamp a picture (the bytes of a PNG or JPEG file; `crate::image::probe` tells its size) on the page, as big as fits in `rect`
    /// with the picture's shape. At most [`MAX_STAMP_PIXELS`] pixels and [`MAX_STAMP_FILE_BYTES`] bytes. Answered by
    /// [`Event::Edited`].
    pub fn add_stamp(&self, doc: u64, id: u64, page: u32, rect: [f32; 4], image: Vec<u8>, author: &str) {
        let edit = crate::edit::Edit::Stamp { page, rect, image, author: author.to_string() };
        let _ = self.commands.send(Command::Edit { doc, id, edit });
    }

    /// Write text into the page's own content (4c, not an annotation): the first line's baseline starts at `at` (points of the page as
    /// shown, from the top left), `size` points high, the lines of `text` one under another. Needs [`Rights::modify`] and a file
    /// without signatures. Answered by [`Event::Edited`].
    #[allow(clippy::too_many_arguments)]
    pub fn add_text(&self, doc: u64, id: u64, page: u32, at: [f32; 2], text: &str, size: f32, color: [u8; 3]) {
        let edit = crate::edit::Edit::AddText { page, at, text: text.to_string(), size, color };
        let _ = self.commands.send(Command::Edit { doc, id, edit });
    }

    /// Draw a picture (the bytes of a PNG or JPEG file) into the page's own content, as big as fits in `rect` with the picture's
    /// shape. Like [`Engine::add_text`] in what it needs and how it is answered.
    pub fn add_image(&self, doc: u64, id: u64, page: u32, rect: [f32; 4], image: Vec<u8>) {
        let edit = crate::edit::Edit::AddImage { page, rect, image };
        let _ = self.commands.send(Command::Edit { doc, id, edit });
    }

    /// Put a watermark on the pages `mark.pages` (their own content). Like [`Engine::add_text`] in what it needs; answered by one
    /// [`Event::Edited`] with `page` [`ALL_PAGES`] (every page's picture is out of date), one step to undo.
    pub fn add_watermark(&self, doc: u64, id: u64, mark: Watermark) {
        let _ = self.commands.send(Command::Edit { doc, id, edit: crate::edit::Edit::Watermark(Box::new(mark)) });
    }

    /// Put page numbers, headers and footers on the pages `spec.pages` (their own content, marked as artifacts of the page). Like
    /// [`Engine::add_watermark`] in what it needs and how it is answered, one step to undo.
    pub fn add_pagination(&self, doc: u64, id: u64, spec: Pagination) {
        let _ = self.commands.send(Command::Edit { doc, id, edit: crate::edit::Edit::Pagination(Box::new(spec)) });
    }

    /// Take away the page numbers, headers and footers that [`Engine::add_pagination`] wrote (on every page, whenever it was done; the
    /// ones other programs wrote are not touched). Answered by one [`Event::Edited`] with `page` [`ALL_PAGES`]; `Failed` when there are none.
    pub fn remove_pagination(&self, doc: u64, id: u64) {
        let _ = self.commands.send(Command::Edit { doc, id, edit: crate::edit::Edit::Unpaginate });
    }

    /// Turn pages (from 0, each once, any order) clockwise by `degrees`, a multiple of 90 (a negative number turns them the other way): the
    /// pages' `/Rotate`. Needs [`Rights::assemble`] and a file without signatures. Answered by [`Event::Edited`] with `page` [`ALL_PAGES`] and
    /// `pages`, the new list of pages (here and in the three calls below, which change the list; one step to undo each).
    pub fn rotate_pages(&self, doc: u64, id: u64, pages: Vec<u32>, degrees: i32) {
        self.page_op(doc, id, crate::edit::PageOp::Rotate { pages, degrees });
    }

    /// Take pages out (not all of them). Their form fields go with them; bookmarks and links to them lead nowhere. The pages stay in the
    /// older part of a saved file until [`Engine::save_as`].
    pub fn delete_pages(&self, doc: u64, id: u64, pages: Vec<u32>) {
        self.page_op(doc, id, crate::edit::PageOp::Delete { pages });
    }

    /// Put pages (in their order) in front of page `to`, counted as the pages are now (the number of pages: after the last).
    pub fn move_pages(&self, doc: u64, id: u64, pages: Vec<u32>, to: u32) {
        self.page_op(doc, id, crate::edit::PageOp::Move { pages, to });
    }

    /// Put all the pages of the file at `path` in front of page `at` (the number of pages: after the last), with their annotations, form
    /// fields (with names changed where this document has them already), links and named destinations; not its bookmarks. A file that needs
    /// a password is answered with [`EditStatus::NeedsPassword`] (or [`EditStatus::WrongPassword`] for one that does not open it): ask for
    /// one and call again. A file that restricts what may be done with it is refused unless `password` is the owner's. At most
    /// [`MAX_INSERT_PAGES`] pages and [`MAX_INSERT_FILE_BYTES`] bytes; the document may not come to more than [`MAX_PAGES_AFTER`] pages.
    pub fn insert_pages(&self, doc: u64, id: u64, at: u32, path: &str, password: &str) {
        self.page_op(doc, id, crate::edit::PageOp::Insert { at, path: path.to_string(), password: crate::edit::Password::new(password) });
    }

    fn page_op(&self, doc: u64, id: u64, op: crate::edit::PageOp) {
        let _ = self.commands.send(Command::Edit { doc, id, edit: crate::edit::Edit::Pages(Box::new(op)) });
    }

    /// Write the pages (from 0; each once, in page order) as a file of their own at `path`, the document as it is with the edits not yet
    /// saved, written afresh as a whole (what `qingpdf split` writes, with the same encryption). The open file is not touched. Answered by
    /// [`Event::Exported`] (`pages` is how many pages are in it, `tables` is 0). Needs [`Rights::copy`]; done in the order of the edits.
    pub fn extract_pages(&self, doc: u64, id: u64, pages: Vec<u32>, path: &str) {
        let _ = self.commands.send(Command::Extract { doc, id, pages, path: path.to_string() });
    }

    /// Write the document, with the edits not saved yet, as a file of its own at `path` (into a file of its own that replaces `path` only
    /// when it is whole; `path` may be the open file) under the encryption `change` says: a new one (passwords, permissions, AES-256 or
    /// AES-128; see `ops::protect` for the rules) or none. The open file is not touched. `seed` is 32 bytes from the system's cryptographic
    /// random generator, new for this call (the engine draws no random numbers of its own); it is overwritten when used. Answered by
    /// [`Event::Protected`]; a file that is signed, or that is encrypted and was not opened with the owner password and does not
    /// allow everything ([`Protected::may_change`]), answers `Failed`. Done in the order of the edits.
    pub fn protect(&self, doc: u64, id: u64, path: &str, change: ProtectChange, seed: [u8; 32]) {
        let job = Box::new(ProtectJob { change, seed });
        let _ = self.commands.send(Command::Protect { doc, id, path: path.to_string(), job });
    }

    /// Write the document, with the edits not saved yet, as a smaller file of its own at `path` (into a file of its own that replaces
    /// `path` only when it is whole; see [`crate::compress`]): objects that repeat merged, the file packed, the pictures made smaller as
    /// `how` says. The open file is not touched. Told by [`Event::CompressProgress`] as it goes and answered by [`Event::Compressed`];
    /// a file that is signed is refused, and so is one whose author did not allow changing its content and that was not opened with the
    /// owner password, unless `how.lossless_only` (a plain rewrite). [`Engine::cancel`] stops it (it gets no answer then). Done at once, one at a time, before the drawing; the pictures are
    /// worked on by several threads that borrow the memory the drawing uses. A file bigger than [`MAX_COMPRESS_FILE`] is refused; the pictures are lent what is left of the total once what the window
    /// keeps and the file's own copies are taken off.
    pub fn compress(&self, doc: u64, id: u64, path: &str, how: CompressHow) {
        let _ = self.commands.send(Command::Compress { doc, id, path: path.to_string(), how });
    }

    /// Apply the marks of the document (its `Redact` annotations, with the edits not saved yet): what lies under them is taken out of the pages for real
    /// (`edit::redact_document`) and the file is written afresh as a whole at `path` (into a file of its own that replaces `path` only when it is
    /// whole; `path` may be the open file), the document's encryption kept. The open file is not touched. Answered by [`Event::Redacted`]; a file that is
    /// signed, or whose author did not allow changing its content and that was not opened with the owner password, answers `Failed`, and so does one
    /// bigger than [`MAX_REDACT_FILE`]. Done in the order of the edits.
    pub fn redact(&self, doc: u64, id: u64, path: &str, how: RedactHow) {
        let _ = self.commands.send(Command::Redact { doc, id, path: path.to_string(), how });
    }

    /// (A mark for redaction over text or a box is [`Engine::add_markup`] of [`MarkupKind::Redact`], or [`Engine::add_shape`] of [`ShapeKind::Redact`].)
    /// Mark every place the words and kinds of number of `spec` are found, on every page, one mark for each place: answered by one [`Event::Edited`] with
    /// `page` [`ALL_PAGES`] whose `message` is the number of places (or `Failed`: nothing found, or too many places).
    pub fn redact_find(&self, doc: u64, id: u64, spec: FindSpec, author: &str) {
        let edit = crate::edit::Edit::RedactFind { spec: Box::new(spec), author: author.to_string() };
        let _ = self.commands.send(Command::Edit { doc, id, edit });
    }

    /// Draw a shape: a rectangle, an ellipse, a line or an arrow, with its line `width` points wide (between [`MIN_INK_WIDTH`] and [`MAX_INK_WIDTH`]) in
    /// `color`; `fill` fills a rectangle or an ellipse with the colour lightened. `from` and `to` are the points of [`CharBoxes`] the person dragged
    /// from and to (opposite corners, or the ends of the line). Answered by [`Event::Edited`].
    #[allow(clippy::too_many_arguments)]
    pub fn add_shape(&self, doc: u64, id: u64, page: u32, kind: ShapeKind, from: [f32; 2], to: [f32; 2], color: [u8; 3], width: f32, fill: bool, author: &str) {
        let edit = crate::edit::Edit::Shape { page, kind, from, to, color, width, fill, author: author.to_string() };
        let _ = self.commands.send(Command::Edit { doc, id, edit });
    }

    /// Move an annotation (the one [`Event::Annotations`] told as `index` and `num` of the page, any kind but links, form fields and popups; not a
    /// locked one) by `delta` points of the page as shown. Answered by [`Event::Edited`].
    pub fn move_annotation(&self, doc: u64, id: u64, page: u32, index: u32, num: u32, delta: [f32; 2]) {
        let _ = self.commands.send(Command::Edit { doc, id, edit: crate::edit::Edit::Move { page, index, num, delta } });
    }

    /// Put an annotation in `rect` (`[left, top, right, bottom]` of the page as shown): squares, circles, lines, drawings, text boxes, pictures and
    /// polygons can be resized, notes and text markup cannot. Answered by [`Event::Edited`].
    pub fn resize_annotation(&self, doc: u64, id: u64, page: u32, index: u32, num: u32, rect: [f32; 4]) {
        let _ = self.commands.send(Command::Edit { doc, id, edit: crate::edit::Edit::Resize { page, index, num, rect } });
    }

    /// Draw an annotation again in `color`, with its line `width` points wide, filled or not (`None`: as it is): text markup, drawings, shapes, notes
    /// and text boxes; other kinds are refused. Answered by [`Event::Edited`].
    #[allow(clippy::too_many_arguments)]
    pub fn restyle_annotation(&self, doc: u64, id: u64, page: u32, index: u32, num: u32, color: Option<[u8; 3]>, width: Option<f32>, fill: Option<bool>) {
        let _ = self.commands.send(Command::Edit { doc, id, edit: crate::edit::Edit::Restyle { page, index, num, color, width, fill } });
    }

    /// Give an annotation another text (at most [`MAX_FULL_TEXT_CHARS`] characters; a text box is drawn again with it, a blank one is refused).
    /// Answered by [`Event::Edited`].
    pub fn set_annotation_text(&self, doc: u64, id: u64, page: u32, index: u32, num: u32, text: &str) {
        let _ = self.commands.send(Command::Edit { doc, id, edit: crate::edit::Edit::SetText { page, index, num, text: text.to_string() } });
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
        let _ = self.commands.send(Command::Save { doc, id, path: path.to_string(), whole: false });
    }

    /// [`Engine::save`] for "save as": the file is written afresh as a whole (a file with only what is reachable from the page
    /// tree, renumbered, with the same encryption), not as the original bytes and an update: what was taken out of a page (the old
    /// text of a line that was changed) is not in it, in any object or stream. The document is then that new file (what was
    /// changed cannot be undone any more). A signed file is saved as `save` does, so as not to break its signature.
    pub fn save_as(&self, doc: u64, id: u64, path: &str) {
        let _ = self.commands.send(Command::Save { doc, id, path: path.to_string(), whole: true });
    }

    /// Look for the pages to recognise (5c): the ones with no text that are mostly pictures (`only` is `None`), or the one page `only`
    /// names, which needs only to have no text. One page at a time between the other work; answered by [`Event::OcrPages`].
    /// [`Engine::cancel`] stops it (and then nothing is told).
    pub fn ocr_scan(&self, doc: u64, id: u64, only: Option<u32>) {
        self.request(doc, id, Request::OcrScan { only });
    }

    /// Draw `page` as grey pixels for a recogniser that takes pictures of up to `max_side` pixels on a side: the whole page as it is
    /// shown, at [`TARGET_DPI`] or less (see [`crate::edit::ocr_dpi`]), at most [`MAX_IMAGE_PIXELS`]. Drawn a band at a time between the
    /// other work; answered by [`Event::OcrImage`]; only that one picture is held, so ask for the next page when this one is used.
    pub fn ocr_image(&self, doc: u64, id: u64, page: u32, max_side: u32) {
        self.request(doc, id, Request::OcrImage { page, max_side });
    }

    /// Give the pages the text a recogniser found in them (`pages`: at most [`MAX_OCR_PAGES`], each [`OcrPage`] with the boxes in the
    /// pixels of the picture [`Engine::ocr_image`] drew): an invisible text layer in the page's own content (text rendering mode 3), one
    /// font for all the pages. Needs [`Rights::modify`] and a file without signatures; one step to undo; answered by [`Event::Edited`]
    /// with `page` [`ALL_PAGES`]. A file that does not allow it is not changed.
    pub fn ocr_layer(&self, doc: u64, id: u64, pages: Vec<OcrPage>) {
        let _ = self.commands.send(Command::Edit { doc, id, edit: crate::edit::Edit::OcrLayer(pages) });
    }

    /// Write the whole document, with the edits not yet saved, as a Word file (`.docx`) to `path` (5b; `qingpdf_core::docx`): into a
    /// file of its own that replaces `path` only when it is whole. Answered by [`Event::Exported`]. A file whose author did not allow
    /// copying text is refused unless the owner password opened it. Done at once, one at a time, before the drawing; [`Engine::cancel`]
    /// stops it (it gets no result then).
    pub fn export_docx(&self, doc: u64, id: u64, path: &str) {
        let _ = self.commands.send(Command::Export { doc, id, path: path.to_string(), excel: false });
    }

    /// Write the tables of the whole document, with the edits not yet saved, as an Excel file (`.xlsx`) to `path` (5d;
    /// `qingpdf_core::xlsx`), a sheet for each table; a file with no table gets one sheet with its text. The same as
    /// [`Engine::export_docx`] in every other way (one answer, [`Event::Exported`]; copying text must be allowed; [`Engine::cancel`]
    /// stops it).
    pub fn export_xlsx(&self, doc: u64, id: u64, path: &str) {
        let _ = self.commands.send(Command::Export { doc, id, path: path.to_string(), excel: true });
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

    /// Close every document and drop every request.
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

/// The engine thread: wait for a file to open or a document to serve, serve it until another command ends it (the document then waits in
/// the store), repeat.
fn run(commands: &Receiver<Command>, events: &Sender<Event>, notify: &(dyn Fn() + Send), shared: &Shared) {
    let send = |event: Event| {
        if events.send(event).is_ok() {
            notify();
        }
    };
    let mut store = store::Store::default();
    let mut next: Option<Command> = None;
    loop {
        let command = match next.take() {
            Some(c) => c,
            None => match commands.recv() {
                Ok(c) => c,
                Err(_) => return,
            },
        };
        let to_serve = match command {
            Command::Quit => return,
            Command::Open { doc, source, password, total, view_pixels } => {
                // (A document with this number is gone: it is replaced.)
                store.remove(doc);
                match open(doc, source, password, total, view_pixels, &mut store, &send) {
                    Ok((opened, session)) => Some((opened, session, server::Start::Opened)),
                    Err((failure, message)) => {
                        send(Event::OpenFailed { doc, failure, message });
                        None
                    }
                }
            }
            Command::Activate { doc, total, view_pixels } => match store.take(doc) {
                Some(waiting) => Some(wake_up(waiting, total, view_pixels, &store)),
                None => {
                    send(Event::Dropped { doc });
                    None
                }
            },
            Command::CloseDoc { doc } => {
                store.remove(doc);
                None
            }
            Command::Close => {
                store.clear();
                None
            }
            // Requests with no document served.
            Command::Render(_)
            | Command::Request { .. }
            | Command::Edit { .. }
            | Command::Undo { .. }
            | Command::Redo { .. }
            | Command::Save { .. }
            | Command::Export { .. }
            | Command::Extract { .. }
            | Command::Protect { .. }
            | Command::Compress { .. }
            | Command::Redact { .. }
            | Command::Swap
            | Command::PrintNext(_)
            | Command::Cancel(_) => None,
        };
        if let Some((opened, session, start)) = to_serve {
            let (end, kept) = server::serve(opened, session, start, &mut store, commands, &send, shared);
            if let Some((opened, session)) = kept {
                store.park(opened, session);
            }
            next = end;
        }
    }
}

/// A document that waited, to be served: it gets the plan the total allows now, with the other documents that wait taking their bytes.
fn wake_up(waiting: store::Parked, total: u64, view_pixels: u64, others: &store::Store) -> (OpenDoc, editing::Session, server::Start) {
    let store::Parked { mut open, session, .. } = waiting;
    let size = open.document.data().len() as u64;
    // (A plan always exists when the documents were let in as they should be; if not, one with the least room, over the total, rather than none.)
    open.plan = plan(total.saturating_sub(others.bytes()), size, view_pixels).or_else(|| plan(size.saturating_add(ACTIVE_FLOOR), size, view_pixels)).unwrap_or(open.plan);
    open.document.set_object_stream_cache_limit(usize::try_from(open.plan.object_streams).unwrap_or(usize::MAX));
    (open, session, server::Start::Activated)
}

pub(crate) struct OpenDoc {
    pub(crate) doc_id: u64,
    pub(crate) document: Document,
    pub(crate) plan: MemoryPlan,
    pub(crate) rights: Rights,
}

fn open(doc_id: u64, source: Source, mut password: String, total: u64, view_pixels: u64, store: &mut store::Store, send: &dyn Fn(Event)) -> Result<(OpenDoc, editing::Session), (OpenFailure, String)> {
    let result = open_inner(doc_id, source, &password, total, view_pixels, store, send);
    // The editing session keeps the password for as long as the file is open (to open the edited file again), and wipes it
    // when it ends; this copy is used and gone: overwrite it before it is freed.
    let budget = result.as_ref().map_or(0, |o| o.plan.edit_bytes);
    let session = editing::Session::new(&password, budget);
    wipe(&mut password);
    result.map(|opened| (opened, session))
}

/// Make room for a file of `size` bytes among the documents that wait: if the total does not hold it with them (and the least the one
/// that is served needs, `active_floor`), the ones that can be let go of go, the least recently served first, and the window is told
/// ([`Event::Dropped`]). `TooLarge`: the file is too big for the total on its own; `NoRoom`: it is not, but the documents that cannot be let
/// go of leave too little.
fn make_room(store: &mut store::Store, send: &dyn Fn(Event), total: u64, size: u64, view_pixels: u64) -> Result<(), (OpenFailure, String)> {
    if plan(total, size, view_pixels).is_none() {
        return Err((OpenFailure::TooLarge, format!("the file is bigger than {} MB, the most this reader opens", MAX_FILE_BYTES / 1_000_000)));
    }
    let need = store::short_by(total, store.bytes(), size.saturating_add(store::guess_for(size)), ACTIVE_FLOOR);
    if need == 0 {
        return Ok(());
    }
    let Some(drops) = store::pick_drops(&store.droppable(), need) else {
        return Err((OpenFailure::NoRoom, "the other open files leave no room for this one: save or close some of them".to_string()));
    };
    for doc in drops {
        store.remove(doc);
        send(Event::Dropped { doc });
    }
    Ok(())
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

fn open_inner(doc_id: u64, source: Source, password: &str, total: u64, view_pixels: u64, store: &mut store::Store, send: &dyn Fn(Event)) -> Result<OpenDoc, (OpenFailure, String)> {
    let too_large = || (OpenFailure::TooLarge, format!("the file is bigger than {} MB, the most this reader opens", MAX_FILE_BYTES / 1_000_000));
    let bytes = match source {
        Source::Path(path) => {
            let unreadable = |e: std::io::Error| (OpenFailure::Unreadable, e.to_string());
            let size = std::fs::metadata(&path).map_err(unreadable)?.len();
            make_room(store, send, total, size, view_pixels)?;
            // With room after the file for the update that saving appends, so that appending does not copy the file.
            let mut file = std::fs::File::open(&path).map_err(unreadable)?;
            let mut bytes = Vec::new();
            bytes.try_reserve_exact(usize::try_from(size).unwrap_or(0).saturating_add(SPARE_AFTER_FILE)).map_err(|e| (OpenFailure::Unreadable, e.to_string()))?;
            std::io::Read::read_to_end(&mut file, &mut bytes).map_err(unreadable)?;
            bytes
        }
        #[cfg(test)]
        Source::Bytes(b) => {
            make_room(store, send, total, b.len() as u64, view_pixels)?;
            b
        }
    };
    // (What is left of the total once the documents that wait have their bytes.)
    let plan = plan(total.saturating_sub(store.bytes()), bytes.len() as u64, view_pixels).ok_or_else(too_large)?;
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

/// How the file is protected, and whether whoever opened it with `password` may change that (see [`Protected`]).
fn protection_of(document: &Document, password: &str) -> Protected {
    let Some(encryption) = document.encryption() else {
        return Protected { cipher: None, needs_password: false, permissions: crate::security::Permissions::all(), may_change: true, signed: crate::edit::signature::look(document).any() };
    };
    use crate::security::Method;
    let cipher = match (encryption.stream_method, encryption.string_method) {
        (Method::AesV3, Method::AesV3) => Cipher::Aes256,
        (Method::AesV2, Method::AesV2) => Cipher::Aes128,
        (Method::Rc4, Method::Rc4) => Cipher::Rc4,
        _ => Cipher::Other,
    };
    Protected {
        cipher: Some(cipher),
        needs_password: encryption.needs_password(),
        permissions: encryption.permissions,
        may_change: has_owner_rights(document, &encryption, password) || encryption.permissions.allows_everything(),
        signed: crate::edit::signature::look(document).any(),
    }
}

/// What the file asks for in laying out its pages: `(spread_hint, right_to_left)` of [`Opened`] (spec 12.2, Table 28 `/PageLayout`, Table 150
/// `/ViewerPreferences` `/Direction`). A name that is not one of the known ones, or an entry of another type, asks for nothing.
fn layout_hint_of(document: &Document) -> (u8, bool) {
    use crate::object::{Dict, Object};
    let Ok(catalog) = document.catalog() else { return (SPREAD_NONE, false) };
    let name = |d: &Dict, key: &str| match d.get(key).and_then(|o| document.resolve(o).ok()) {
        Some(Object::Name(n)) => n.as_bytes().to_vec(),
        _ => Vec::new(),
    };
    let spread = match name(&catalog, "PageLayout").as_slice() {
        b"TwoColumnLeft" | b"TwoPageLeft" => SPREAD_FIRST_LEFT,
        b"TwoColumnRight" | b"TwoPageRight" => SPREAD_FIRST_RIGHT,
        _ => SPREAD_NONE,
    };
    let right_to_left = match catalog.get("ViewerPreferences").and_then(|o| document.resolve(o).ok()) {
        Some(Object::Dict(prefs)) => name(&prefs, "Direction") == b"R2L",
        _ => false,
    };
    (spread, right_to_left)
}

/// What the file allows, for whoever opened it with `password` (see [`Rights`]).
fn rights_of(document: &Document, password: &str) -> Rights {
    let copy = text::check_extraction_allowed(document, password).is_ok();
    let Some(encryption) = document.encryption() else {
        return Rights { copy, print: true, print_high_quality: true, annotate: true, fill_forms: true, modify: true, assemble: true };
    };
    if has_owner_rights(document, &encryption, password) {
        return Rights { copy, print: true, print_high_quality: true, annotate: true, fill_forms: true, modify: true, assemble: true };
    }
    Rights {
        copy,
        print: encryption.permissions.print,
        print_high_quality: encryption.permissions.print_high_quality,
        annotate: encryption.permissions.annotate,
        fill_forms: encryption.permissions.fill_forms,
        modify: encryption.permissions.modify,
        assemble: encryption.permissions.assemble,
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
