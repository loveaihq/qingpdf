//! The reader: what is open, where the view is, what has been drawn and what is asked of the engine. It hears
//! [`Event`]s and answers with [`Action`]s; it never calls Windows, and it never waits for the engine.
//!
//! The parts of it that came with 3d-2 are in the modules below, each an `impl App` of its own: `outline` (the bookmarks, links
//! and moving to a place), `selecting` (the mouse, selected text, copying), `searching` (the find box), `printjob` (printing),
//! `menus` and `recents` (the menus; the files opened last), `ocr` (recognizing the text of scans), `sidebar` (the tabs, and the
//! thumbnails: the list is drawn and handled there). `looks` (2026-10-20) is how the pages go in rows, how the window looks and in what colours the pages are
//! shown; `screen` is full screen and the presentation.

#[cfg(test)]
mod annot_tests;
mod annotedit;
mod annotlist;
mod bookmarks;
mod chrome;
mod compress;
mod editing;
mod forms;
mod looks;
mod menus;
mod ocr;
mod outline;
mod pagenum;
mod pages;
mod printjob;
mod protect;
mod recents;
mod redact;
mod screen;
mod searching;
mod selecting;
mod sidebar;
mod tabbar;
mod tabset;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use qingpdf_core::idmap::IdMap;
use qingpdf_core::render::recolor::recolor;
use qingpdf_core::view::{Engine, Event as EngineEvent, Link, OpenFailure, Opened, OutlineItem, RenderRequest, RenderStatus, Rendered, Rights, TOTAL_BYTES};

use crate::bench::Bench;
use crate::cache::{BitmapCache, Entry, Focus, Key};
use crate::find::SearchState;
use crate::i18n::{self, Lang, LangChoice, Msg, t};
use crate::layout::{Arrangement, Layout, Spread};
use crate::places::{self, Place, Places, Stamp};
use crate::recent;
use crate::sched::{self, Scene, Want};
use crate::select::Selection;
use crate::tabs::Tabs;
use crate::theme::{self, Palette, ThemeChoice, Tint};
use crate::thumbs::{self, Tab};
use crate::tiles;
use crate::ui::{Action, Bar, Cursor, Event, Handler, Menu, Painter, Rect, ScrollCode, ScrollInfo, StatusText, ToolbarState, vk};
use qingpdf_core::view::{MarkupKind, ShapeKind};
use crate::zoom::{self, ZoomMode};

pub use printjob::PrintTest;
use printjob::PrintJob;
use bookmarks::BmWork;
use editing::{After, EditState, Tool};
use pages::PageWork;
use selecting::{PageBoxes, Press};

// Menu commands.
pub const CMD_OPEN: u32 = 1;
pub const CMD_EXIT: u32 = 2;
pub const CMD_PRINT: u32 = 3;
pub const CMD_CLEAR_RECENT: u32 = 4;
pub const CMD_ABOUT: u32 = 5;
pub const CMD_ZOOM_IN: u32 = 10;
pub const CMD_ZOOM_OUT: u32 = 11;
pub const CMD_FIT_WIDTH: u32 = 12;
pub const CMD_FIT_PAGE: u32 = 13;
pub const CMD_ACTUAL: u32 = 14;
pub const CMD_ROTATE: u32 = 15;
pub const CMD_GOTO: u32 = 16;
pub const CMD_BOOKMARKS: u32 = 17;
pub const CMD_FIND: u32 = 18;
pub const CMD_FIND_NEXT: u32 = 19;
pub const CMD_FIND_PREV: u32 = 20;
pub const CMD_COPY: u32 = 21;
/// File > Passwords and permissions.
pub const CMD_PROTECT: u32 = 22;
/// File > Compress.
pub const CMD_COMPRESS: u32 = 23;
/// Tools > Recognize text: in every scanned page, on this page.
pub const CMD_OCR_ALL: u32 = 26;
pub const CMD_OCR_PAGE: u32 = 27;
pub const CMD_SAVE: u32 = 30;
pub const CMD_SAVE_AS: u32 = 31;
/// Make a Word file of the document (5b), and an Excel file of its tables (5d).
pub const CMD_EXPORT_DOCX: u32 = 29;
pub const CMD_EXPORT_XLSX: u32 = 28;
pub const CMD_UNDO: u32 = 32;
pub const CMD_REDO: u32 = 33;
pub const CMD_REMOVE: u32 = 34;
pub const CMD_AUTHOR: u32 = 35;
/// Pick a picture to stamp on a page.
pub const CMD_STAMP: u32 = 36;
/// The Insert menu (4c): text and a picture written into the page, a watermark; the watermark's options (opacity and angle from
/// here, one for each choice, then the three switches).
pub const CMD_INS_TEXT: u32 = 37;
pub const CMD_INS_IMAGE: u32 = 38;
pub const CMD_INS_WM: u32 = 39;
/// Insert > page numbers, headers and footers.
pub const CMD_INS_PAGENUM: u32 = 91;
pub const CMD_WM_OPACITY_BASE: u32 = 80;
pub const CMD_WM_ANGLE_BASE: u32 = 84;
pub const CMD_WM_TILE: u32 = 88;
pub const CMD_WM_UNDER: u32 = 89;
pub const CMD_WM_PAGE: u32 = 90;
/// The tools of the Comment menu: select, the four markups, draw, note.
pub const CMD_TOOL_SELECT: u32 = 40;
pub const CMD_TOOL_HIGHLIGHT: u32 = 41;
pub const CMD_TOOL_UNDERLINE: u32 = 42;
pub const CMD_TOOL_STRIKE: u32 = 43;
pub const CMD_TOOL_SQUIGGLY: u32 = 44;
pub const CMD_TOOL_INK: u32 = 45;
pub const CMD_TOOL_NOTE: u32 = 46;
pub const CMD_TOOL_TEXTBOX: u32 = 47;
pub const CMD_TOOL_EDITTEXT: u32 = 48;
/// The shapes of the Comment menu (2026-10-17) and its fill switch.
pub const CMD_TOOL_RECT: u32 = 130;
pub const CMD_TOOL_ELLIPSE: u32 = 131;
pub const CMD_TOOL_LINE: u32 = 132;
pub const CMD_TOOL_ARROW: u32 = 133;
pub const CMD_FILL: u32 = 134;
/// The Redact menu (2026-10-20): the tool that drags out a box, the selected text, find and mark, apply.
pub const CMD_TOOL_REDACT: u32 = 180;
pub const CMD_REDACT_SELECTION: u32 = 181;
pub const CMD_REDACT_FIND: u32 = 182;
pub const CMD_REDACT_APPLY: u32 = 183;
/// The colours (from here, one for each) and the line widths.
pub const CMD_COLOR_BASE: u32 = 50;
pub const CMD_WIDTH_BASE: u32 = 60;
/// The sizes of the text of a text box.
pub const CMD_SIZE_BASE: u32 = 70;
/// The files in the list of recent ones are commands from here, one for each.
pub const CMD_RECENT_BASE: u32 = 100;
/// The page before and the page after the one in view, the toolbar, the languages, and the items of the menu on the page.
pub const CMD_PREV_PAGE: u32 = 110;
pub const CMD_NEXT_PAGE: u32 = 111;
pub const CMD_TOOLBAR: u32 = 112;
pub const CMD_LANG_SYSTEM: u32 = 113;
pub const CMD_LANG_ZH: u32 = 114;
pub const CMD_LANG_EN: u32 = 115;
pub const CMD_CTX_HIGHLIGHT: u32 = 116;
pub const CMD_CTX_NOTE: u32 = 117;
pub const CMD_CTX_SELECT_ALL: u32 = 118;
/// The menu on the list of thumbnails (2026-10-15): turn left and right, delete, put in pages from a file in front of or behind, extract.
pub const CMD_PG_LEFT: u32 = 120;
pub const CMD_PG_RIGHT: u32 = 121;
pub const CMD_PG_DELETE: u32 = 122;
pub const CMD_PG_INS_BEFORE: u32 = 123;
pub const CMD_PG_INS_AFTER: u32 = 124;
pub const CMD_PG_EXTRACT: u32 = 125;
/// The tabs (2026-10-17): close the one shown, show the next one, show the one before.
pub const CMD_CLOSE_TAB: u32 = 140;
pub const CMD_NEXT_TAB: u32 = 141;
pub const CMD_PREV_TAB: u32 = 142;
/// View (2026-10-20): the layout of the pages, full screen and the presentation, the look of the window, the colours of the pages.
pub const CMD_LAYOUT_SINGLE: u32 = 150;
pub const CMD_LAYOUT_SPREAD: u32 = 151;
pub const CMD_LAYOUT_COVER: u32 = 152;
pub const CMD_FULLSCREEN: u32 = 153;
pub const CMD_PRESENT: u32 = 154;
pub const CMD_THEME_SYSTEM: u32 = 155;
pub const CMD_THEME_LIGHT: u32 = 156;
pub const CMD_THEME_DARK: u32 = 157;
pub const CMD_TINT_ORIGINAL: u32 = 158;
pub const CMD_TINT_NIGHT: u32 = 159;
pub const CMD_TINT_SEPIA: u32 = 160;
/// The bookmarks (2026-10-20): add, type over the title, take out, one level deeper or shallower, lead to the top of the window, make from headings.
pub const CMD_BM_ADD: u32 = 170;
pub const CMD_BM_RENAME: u32 = 171;
pub const CMD_BM_DELETE: u32 = 172;
pub const CMD_BM_INDENT: u32 = 173;
pub const CMD_BM_OUTDENT: u32 = 174;
pub const CMD_BM_HERE: u32 = 175;
pub const CMD_BM_AUTO: u32 = 176;

// Questions asked of the user.
const TAG_PASSWORD: u32 = 1;
const TAG_GOTO: u32 = 2;
const TAG_LINK: u32 = 3;
const TAG_NOTE: u32 = 4;
const TAG_AUTHOR: u32 = 5;
const TAG_UNSAVED: u32 = 6;
const TAG_WATERMARK: u32 = 7;
const TAG_SRC_PASSWORD: u32 = 8;
const TAG_PAGENUM: u32 = 9;
const TAG_PROTECT: u32 = 10;
const TAG_COMPRESS: u32 = 11;
const TAG_COMPRESS_SAVE: u32 = 12;
const TAG_COMPRESS_OPEN: u32 = 13;
const TAG_NOTE_EDIT: u32 = 14;
const TAG_BM_AUTO: u32 = 15;
const TAG_REDACT_FIND: u32 = 16;
const TAG_REDACT_APPLY: u32 = 17;

// Timers.
/// The view has stopped moving.
const TIMER_SETTLE: u32 = 1;
const SETTLE_MS: u32 = 30;
/// The words in the find box have been left alone for a moment: look for them.
const TIMER_FIND: u32 = 2;
const FIND_MS: u32 = 250;
/// The note in the status line has been there long enough.
const TIMER_NOTE: u32 = 3;
const NOTE_MS: u32 = 6000;
/// The pointer is dragging a selection: scroll if it is outside the pages.
const TIMER_DRAG: u32 = 4;
const DRAG_MS: u32 = 50;
/// The pointer is dragging thumbnails: scroll the list if it is held at an edge.
const TIMER_THUMB_DRAG: u32 = 5;
/// The arrow keys have been left alone for a moment: the nudges are sent as one move.
const TIMER_NUDGE: u32 = 6;
/// The pointer has been still for two seconds in the presentation: it is hidden.
const TIMER_CURSOR: u32 = 7;
const CURSOR_MS: u32 = 2000;

// Sizes at 96 dpi.
const GAP: i64 = 10;
const MARGIN: i64 = 12;
/// The strip between the sidebar and the pages that can be dragged to change the sidebar's width (at 96 dpi; the usual width is `sidebar::DEFAULT_WIDTH`).
const SPLIT_GAP: i64 = 6;
const WHEEL_STEP: i64 = 96;
const LINE_STEP: i64 = 48;
/// The find box: the edit box, the room for the count to its left, the gap to the top and right edges.
const FIND_EDIT_W: i64 = 220;
const FIND_EDIT_H: i64 = 24;
const FIND_LABEL_W: i64 = 190;
const FIND_MARGIN: i64 = 10;

// Colours of the marks over the pages (the colours of the parts of the window are in `theme.rs`).
const HIT_COLOR: u32 = 0xFFD400;
const HIT_ALPHA: u8 = 120;
const CURRENT_HIT_COLOR: u32 = 0xFF7A00;
const CURRENT_HIT_ALPHA: u8 = 150;
const SELECTION_COLOR: u32 = 0x3390FF;
const SELECTION_ALPHA: u8 = 100;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Notice {
    Incomplete,
    Failed,
}

/// The document that is open.
struct Doc {
    layout: Layout,
    max_tile_pixels: u32,
}

/// The fields of the reader, in two lists: the ones that belong to the window (`global`) and the ones that belong to the file that is shown
/// (`tab`, each with the value a tab starts with). A tab that is not shown keeps its `tab` fields in a [`TabState`]; showing a tab is
/// swapping them (`App::exchange`). One list, so that a field cannot be in one place and forgotten in the other.
macro_rules! app_fields {
    (
        global { $( $(#[$gm:meta])* $gv:vis $gf:ident : $gt:ty, )* }
        tab { $( $(#[$tm:meta])* $tv:vis $tf:ident : $tt:ty = $ti:expr, )* }
    ) => {
        pub struct App {
            $( $(#[$gm])* $gv $gf: $gt, )*
            $( $(#[$tm])* $tv $tf: $tt, )*
        }

        /// What the window holds apart from the tabs.
        struct Globals {
            $( $gf: $gt, )*
        }

        /// What the reader keeps of the file that is shown: all of it for a tab that is not shown, a blank one for a tab not yet made.
        pub(crate) struct TabState {
            $( $tf: $tt, )*
        }

        impl TabState {
            fn blank() -> TabState {
                TabState { $( $tf: $ti, )* }
            }
        }

        impl App {
            fn assemble(g: Globals) -> App {
                App { $( $gf: g.$gf, )* $( $tf: $ti, )* }
            }

            /// Swap the fields of the file that is shown with those of `other`.
            fn swap_tab(&mut self, other: &mut TabState) {
                $( std::mem::swap(&mut self.$tf, &mut other.$tf); )*
            }
        }
    };
}

app_fields! {
    global {
        lang: Lang,
        engine: Engine,
        /// The most pixels the window can show (its screen's work area), for sizing the bitmaps' share of the memory.
        max_view_pixels: u64,
        /// The scale the pieces in view are drawn at: the zoom's own, or a smaller one when they would not fit the cache.
        draw_scale: u64,
        /// The whole client area's width and height; the pages are `view_w` wide, from `view_x` (the bookmarks are to their left), and
        /// `view_h` high, from `bar_h()` down (the strip of tabs is above them when there are two files or more).
        client_w: i64,
        client_h: i64,
        view_x: i64,
        view_w: i64,
        view_h: i64,
        dpi: u32,
        next_id: u64,
        /// The number the next document opened gets (a number is never used twice), and the number of changes of tab so far.
        next_doc: u64,
        left_count: u64,
        /// The memory goal handed to the engine for all the files together (the tests make it small to see files let go of).
        pub(crate) total: u64,
        /// What the scroll bars show now, so that they are set only when it changes.
        bars: (Option<ScrollInfo>, Option<ScrollInfo>),
        /// Files to open, one after the other (those named on the command line, dropped on the window, handed over by another copy of
        /// the program), and whether the window has its size yet (nothing is opened before).
        queue: VecDeque<PathBuf>,
        ready: bool,
        /// Wakes the window's loop (from any thread).
        wake: Arc<dyn Fn() + Send + Sync>,
        first_bitmap: Option<Instant>,
        last_sharp: Option<Instant>,
        pub bench: Option<Bench>,
        pub(crate) started: Instant,
        /// Where the pointer was last, in the client area.
        mouse: (i64, i64),
        find_open: bool,
        /// What is in the find box now.
        find_text: String,
        /// A line for the status bar, between the page number and the zoom.
        note: Option<String>,
        recent: Vec<recent::Entry>,
        recent_path: Option<PathBuf>,
        /// An address of a link, waiting for the person to say yes.
        pending_uri: Option<String>,
        /// The menus have to be put again (what is greyed, checked or listed has changed).
        menus_dirty: bool,
        /// The hidden `--print-test`: print to a printer that writes a file, with no box, and quit.
        print_test: Option<PrintTest>,
        pub(crate) ocr_test_opener: Option<crate::ocr::Opener>,
        /// What the person chose in View > Language, and the language of Windows (what "follow the system" means).
        lang_choice: LangChoice,
        system_lang: Lang,
        /// The toolbar is shown (View > Toolbar); it is told to the window with the next answer when `toolbar_dirty`.
        toolbar_on: bool,
        toolbar_dirty: bool,
        /// What the toolbar and the status bar were last told to say, so that they are told only when it changes.
        last_toolbar: Option<ToolbarState>,
        last_status: Option<StatusText>,
        /// The width of the bookmarks the person has made them (at 96 dpi), and the start of a drag of their edge: where the pointer was
        /// and the width then.
        sidebar_pref: i64,
        split_drag: Option<(i64, i64)>,
        /// Whether changes to the settings are written to the file (the tests do not).
        pub(crate) persist: bool,
        /// The speed test shows the sidebar on the thumbnails (`--bench-thumbs`).
        bench_sidebar: bool,
        /// What the box of Insert > Page numbers... holds.
        pn: pagenum::Pagenum,
        /// The tabs (2026-10-17): the files that are open, and the state of those that are not shown.
        tabs: Tabs<TabState>,
        /// Where the reader was in the last files (`remember_place`: the person turned that off with `remember_place=0` in the settings),
        /// where they are kept, and whether they have changed since they were written.
        places: Places,
        places_path: Option<PathBuf>,
        places_dirty: bool,
        remember_place: bool,
        /// The pointer is over this tab of the strip, and a click that began in the strip is not over yet.
        tab_hover: Option<usize>,
        strip_press: bool,
        /// A document that waited was asked for: nothing is drawn of it until the engine says what the plan is now.
        activating: bool,
        /// The tab asked for while a file was being read again (it is shown when that is done).
        pending_tab: Option<usize>,
        /// The title the window was last given.
        last_title: String,
        /// The documents whose changes the person gave up in the round of questions that is going on before the window is closed.
        discarded: Vec<u64>,
        // What the shown file has going or has drawn that is let go of when the tab is left (`release`) or that cannot be going when it is
        // (`tab_busy`), so that it is the window's and not each tab's:
        /// Scrolling or zooming goes on: nothing sharp is asked for until it stops.
        moving: bool,
        /// The requests the engine has: by the piece, and by the request.
        in_flight: HashMap<Key, u64>,
        asked: IdMap<u64, Want>,
        /// Pieces the engine could not draw: not asked for again.
        failed: HashSet<Key>,
        notices: IdMap<u32, Notice>,
        wrong_password: bool,
        /// Whether the last picture drawn had every piece of the pages in view at the zoom they are seen at.
        sharp_on_screen: bool,
        /// The links of the pages near the window, and the engine's requests for them (request, page).
        links: IdMap<u32, Vec<Link>>,
        links_asked: IdMap<u64, u32>,
        /// The boxes of the characters of the pages the pointer has been on, and the requests for them.
        boxes: IdMap<u32, PageBoxes>,
        boxes_asked: IdMap<u64, u32>,
        /// The left button is down: where, and on what.
        press: Option<Press>,
        /// The request for the text of the selection that is being copied, and what to say when the clipboard has taken it.
        copy_asked: Option<u64>,
        copy_note: Option<String>,
        search: Option<SearchState>,
        print: Option<PrintJob>,
        /// The page and the place on it (points) the menu on the page was opened at, for "add note here" and "select all on the page".
        ctx_at: Option<(u32, f32, f32)>,
        /// A double click, a triple click or "select all" that waits for the characters of its page.
        pending_select: Option<chrome::PendingSelect>,
        /// The run of text recognition that is going on, if one is.
        ocr: Option<crate::ocr::Run>,
        /// The rows of the thumbnails, where they are scrolled to, the pictures and the requests for them.
        thumbs: thumbs::Thumbs,
        /// The keys belong to the list of thumbnails (it was clicked last), and its bar is being dragged; or to the list of annotations.
        thumb_focus: bool,
        annots_focus: bool,
        /// Editing the bookmarks (2026-10-20): the item selected, the keys, the changes waiting for the engine.
        bm: BmWork,
        al: annotlist::AnnotList,
        thumb_drag: bool,
        /// The page the list of thumbnails moved to (it is the page shown as current while it is in view), and whether that move is going on.
        thumb_target: Option<usize>,
        thumb_nav: bool,
        cz: compress::Squeeze,
        /// Redaction (2026-10-20): the box that asks where to write is answered, the engine is making the file.
        rd: redact::Redacting,
        /// The look of the window (2026-10-20): what the person chose in View > Interface, whether Windows is set to dark apps, the colours in use, and
        /// whether the window is to be told them (`Action::Theme`) with the next answer.
        theme_choice: ThemeChoice,
        system_dark: bool,
        pal: &'static Palette,
        theme_dirty: bool,
        /// The colours of the pages (View > Page colours), and how the pages go in rows for a file nothing is kept about (the last choice made).
        tint: Tint,
        default_spread: Spread,
        /// The window is full screen (F11); the presentation that is going on (F5); the page to put at the top once the window has its old size back.
        full: bool,
        present: Option<screen::Present>,
        restore_page: Option<usize>,
        /// The speed test goes through the pages in the presentation (`--bench-present`).
        bench_present: bool,
    }
    tab {
        doc_id: u64 = 0,
        path: Option<PathBuf> = None,
        /// The size and time of the file when it was read, to tell later whether it is the same file.
        stamp: Option<Stamp> = None,
        /// Where to put the view when the file is read again (a tab the engine let go of is wanted).
        restore: Option<Place> = None,
        opening: bool = false,
        doc: Option<Doc> = None,
        mode: ZoomMode = ZoomMode::FitWidth,
        zoom: u32 = zoom::ACTUAL_SIZE,
        /// Turns of the view in degrees, added to each page's own.
        rotation: u16 = 0,
        scroll_x: i64 = 0,
        scroll_y: i64 = 0,
        cache: BitmapCache = BitmapCache::new(0),
        // --- 3d-2 ---
        /// What the file allows (all of it when none is open).
        rights: Rights = Rights { copy: true, print: true, print_high_quality: true, annotate: true, fill_forms: true, modify: true, assemble: true },
        /// The bookmarks of the open file, and whether the sidebar is wanted (it is shown when there are bookmarks, or when it is open on the thumbnails).
        outline: Vec<OutlineItem> = Vec::new(),
        show_sidebar: bool = true,
        selection: Option<Selection> = None,
        // --- 4a ---
        /// The tool, the annotations, the history of the changes and the saving.
        edit: EditState = EditState::new(String::new()),
        // --- the thumbnails (2026-10-14) ---
        /// The tab of the sidebar the person chose, and whether the sidebar is open on the thumbnails for a file without bookmarks.
        tab: Tab = Tab::Bookmarks,
        thumbs_alone: bool = false,
        // --- the pages (2026-10-15) ---
        /// Which thumbnails are chosen, the drag of them, and the pages put in or extracted.
        pg: PageWork = PageWork::new(),
        // --- passwords and permissions (2026-10-16) ---
        /// How the open file is protected, and the box that changes it.
        pt: protect::Protect = protect::Protect::new(),
        // --- the looks (2026-10-20) ---
        /// How the pages go in rows, whether they read from the right (the file's own word), and the colours the pictures in `cache` were made in.
        spread: Spread = Spread::Single,
        r2l: bool = false,
        cache_tint: Tint = Tint::Original,
    }
}

impl App {
    pub fn new(lang: Lang, wake: Arc<dyn Fn() + Send + Sync>, started: Instant, max_view_pixels: u64, author: String) -> App {
        let for_engine = wake.clone();
        let recent_path = recent::file_path();
        let recent = recent_path.as_deref().map(recent::load).unwrap_or_default();
        let places_path = places::file_path();
        let places = places_path.as_deref().map(places::load).unwrap_or_default();
        let globals = Globals {
            lang,
            engine: Engine::start(Box::new(move || for_engine())),
            max_view_pixels,
            draw_scale: sched::scale_key(zoom::ACTUAL_SIZE, 96),
            client_w: 0,
            client_h: 0,
            view_x: 0,
            view_w: 0,
            view_h: 0,
            dpi: 96,
            next_id: 1,
            next_doc: 1,
            left_count: 0,
            total: TOTAL_BYTES,
            bars: (None, None),
            queue: VecDeque::new(),
            ready: false,
            wake,
            first_bitmap: None,
            last_sharp: None,
            bench: None,
            started,
            mouse: (0, 0),
            find_open: false,
            find_text: String::new(),
            note: None,
            recent,
            recent_path,
            pending_uri: None,
            menus_dirty: true,
            print_test: None,
            ocr_test_opener: None,
            lang_choice: LangChoice::System,
            system_lang: lang,
            toolbar_on: true,
            toolbar_dirty: true,
            last_toolbar: None,
            last_status: None,
            sidebar_pref: sidebar::DEFAULT_WIDTH,
            split_drag: None,
            persist: true,
            bench_sidebar: false,
            pn: pagenum::Pagenum::new(),
            tabs: Tabs::new(),
            places,
            places_path,
            places_dirty: false,
            remember_place: true,
            tab_hover: None,
            strip_press: false,
            activating: false,
            pending_tab: None,
            last_title: String::new(),
            discarded: Vec::new(),
            moving: false,
            in_flight: HashMap::new(),
            asked: IdMap::new(),
            failed: HashSet::new(),
            notices: IdMap::new(),
            wrong_password: false,
            sharp_on_screen: false,
            links: IdMap::new(),
            links_asked: IdMap::new(),
            boxes: IdMap::new(),
            boxes_asked: IdMap::new(),
            press: None,
            copy_asked: None,
            copy_note: None,
            search: None,
            print: None,
            ctx_at: None,
            pending_select: None,
            ocr: None,
            thumbs: thumbs::Thumbs::new(),
            thumb_focus: false,
            annots_focus: false,
            bm: BmWork::default(),
            al: annotlist::AnnotList::new(),
            thumb_drag: false,
            thumb_target: None,
            thumb_nav: false,
            cz: compress::Squeeze::new(),
            rd: redact::Redacting::new(),
            theme_choice: ThemeChoice::System,
            system_dark: false,
            pal: &theme::LIGHT,
            theme_dirty: true,
            tint: Tint::Original,
            default_spread: Spread::Single,
            full: false,
            present: None,
            restore_page: None,
            bench_present: false,
        };
        let mut app = App::assemble(globals);
        app.edit = EditState::new(author);
        app
    }

    /// Open this file once the window has its size.
    pub fn open_when_ready(&mut self, path: PathBuf) {
        self.queue.push_back(path);
    }

    /// The memory goal handed to the engine for all the files together, in bytes (200 MB by default; the speed test of the tabs makes it small, to see
    /// files let go of).
    pub fn set_memory_goal(&mut self, bytes: u64) {
        self.total = bytes;
    }

    /// Open these files, each in a tab, once the window has its size (those named on the command line).
    pub fn open_all_when_ready(&mut self, paths: Vec<PathBuf>) {
        self.queue.extend(paths);
    }

    fn px(&self, at96: i64) -> i64 {
        at96 * i64::from(self.dpi) / 96
    }

    fn new_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    // --- opening ---------------------------------------------------------------------------------------------

    fn path_text(&self) -> String {
        self.path.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// Let go of the open document and everything that belongs to it. The actions that undo what the window shows of it.
    fn drop_document(&mut self) -> Vec<Action> {
        // What is being drawn for the old file is not wanted: stop it (opening another file stops it too, but the
        // file may fail to open, and the old requests are not to go on).
        // (A presentation is of the document that goes: it ends first.)
        let mut actions = self.end_present(true);
        actions.extend(self.release());
        self.cache.clear();
        actions.extend(self.cancel_print_for_new_document());
        self.drop_ocr();
        self.pg.reset();
        self.thumb_focus = false;
        self.annots_focus = false;
        self.edit.reset();
        self.pt.reset();
        if let Some(id) = self.cz.reset() {
            self.engine.cancel(id);
        }
        if let Some(id) = self.rd.reset() {
            self.engine.cancel(id);
        }
        let had_outline = !self.outline.is_empty();
        self.doc = None;
        self.outline.clear();
        self.selection = None;
        self.note = None;
        self.rights = Rights { copy: true, print: true, print_high_quality: true, annotate: true, fill_forms: true, modify: true, assemble: true };
        self.scroll_x = 0;
        self.scroll_y = 0;
        self.sharp_on_screen = false;
        self.menus_dirty = true;
        self.bm = BmWork::default();
        if had_outline {
            actions.push(Action::Outline { nodes: Vec::new(), select: None });
        }
        // The pages have the width of the window again.
        self.view_x = 0;
        self.view_w = self.client_w;
        actions.extend(self.chrome());
        actions
    }

    fn title(&self) -> String {
        match &self.path {
            Some(p) => format!("{}{} - qingpdf", if self.edit.dirty { "* " } else { "" }, p.file_name().map_or_else(|| p.to_string_lossy(), |n| n.to_string_lossy())),
            None => "qingpdf".to_string(),
        }
    }

    fn on_opened(&mut self, o: Opened) -> Vec<Action> {
        if o.doc != self.doc_id {
            return Vec::new();
        }
        self.opening = false;
        self.rights = o.rights;
        self.pt.info = Some(o.protection);
        let sizes = o.pages.iter().map(|p| (p.width, p.height)).collect();
        self.doc = Some(Doc { layout: Layout::new(sizes), max_tile_pixels: o.max_tile_pixels });
        self.cache = BitmapCache::new(o.bitmap_cache_bytes);
        self.thumbs.reset(o.thumb_cache_bytes);
        self.mode = ZoomMode::FitWidth;
        self.rotation = 0;
        self.scroll_x = 0;
        self.scroll_y = 0;
        // How the pages go in rows: what the file asks for, else what was last chosen (a place that was kept says so itself, below).
        self.r2l = o.right_to_left;
        self.spread = Spread::from_hint(o.spread_hint).unwrap_or(self.default_spread);
        self.cache_tint = self.tint;
        let mut actions = vec![Action::SetTitle(self.title())];
        // The sidebar is there from the start when it is open on the thumbnails (the bookmarks, if there are any, come later).
        self.view_x = self.sidebar_width();
        self.view_w = (self.client_w - self.view_x).max(0);
        self.relayout(0, 0, None);
        // (The speed test of the presentation starts it before anything is asked for.)
        if std::mem::take(&mut self.bench_present) {
            actions.extend(self.start_present());
        }
        // Where the reader was: in this tab before the file was read again, else in this file last time (if it is the same file).
        let restore = self.restore.take();
        let start = restore.or_else(|| self.stored_place());
        if let Some(place) = start {
            self.apply_place(&place);
        }
        actions.extend(self.after_view_change(false));
        actions.extend(self.chrome());
        self.remember_position();
        self.flush_places();
        self.menus_dirty = true;
        if self.bench.is_none() {
            actions.extend(self.ask_for_outline());
        }
        self.ask_for_annot_list();
        actions.extend(self.start_print_test());
        actions.extend(self.open_queued());
        if let Some(i) = self.pending_tab.take() {
            actions.extend(self.show_tab(i));
        }
        actions
    }

    fn on_open_failed(&mut self, doc: u64, failure: OpenFailure, message: &str) -> Vec<Action> {
        if doc != self.doc_id {
            return Vec::new();
        }
        self.opening = false;
        let lang = self.lang;
        let mut actions = vec![Action::Invalidate];
        match failure {
            OpenFailure::NeedsPassword | OpenFailure::WrongPassword => {
                let wrong = failure == OpenFailure::WrongPassword;
                self.wrong_password = wrong;
                let prompt = t(lang, if wrong { Msg::PasswordWrongPrompt } else { Msg::PasswordPrompt });
                actions.push(Action::AskText {
                    tag: TAG_PASSWORD,
                    title: t(lang, Msg::PasswordTitle).to_string(),
                    prompt: prompt.to_string(),
                    secret: true,
                    initial: String::new(),
                    ok: t(lang, Msg::Ok).to_string(),
                    cancel: t(lang, Msg::Cancel).to_string(),
                });
            }
            OpenFailure::TooLarge => actions.push(Action::Message { title: t(lang, Msg::Error).to_string(), text: t(lang, Msg::TooLarge).to_string() }),
            OpenFailure::Unreadable => {
                actions.push(Action::Message { title: t(lang, Msg::Error).to_string(), text: format!("{}\n{message}", t(lang, Msg::CannotRead)) });
                // A file that is gone is not kept in the list.
                let key = self.path_text();
                recent::forget(&mut self.recent, &key);
                self.save_recent();
                self.menus_dirty = true;
            }
            OpenFailure::Damaged => actions.push(Action::Message { title: t(lang, Msg::Error).to_string(), text: t(lang, Msg::NotPdf).to_string() }),
            OpenFailure::NoRoom => actions.push(Action::Message { title: t(lang, Msg::Error).to_string(), text: t(lang, Msg::NoRoomTabs).to_string() }),
        }
        if failure != OpenFailure::NeedsPassword && failure != OpenFailure::WrongPassword {
            // The tab made for the file goes again (the one before it is shown), and the next file that waits is opened.
            self.pending_tab = None;
            actions.extend(self.close_current());
            actions.extend(self.open_queued());
        }
        actions
    }

    // --- the view --------------------------------------------------------------------------------------------

    /// Lay the pages out again for the zoom, the rotation and the size of the window. The place that is `anchor_y`
    /// pixels down the window (and `anchor_x` across) stays where it is. `mode`: set the zoom from a fit first.
    fn relayout(&mut self, anchor_x: i64, anchor_y: i64, zoom_to: Option<(ZoomMode, u32)>) {
        // The presentation shows one page at a time, fitted to the screen, with no margin and a gap as high as the screen so that the pages
        // next to it are never in view; the page is kept by its middle (the anchor is the middle of the screen).
        let presenting = self.present.is_some();
        let (dpi, rotation) = (self.dpi, self.rotation);
        let (gap, margin) = if presenting { (self.view_h.max(1), 0) } else { (self.px(GAP), self.px(MARGIN)) };
        let (anchor_x, anchor_y) = if presenting { (0, self.view_h / 2) } else { (anchor_x, anchor_y) };
        let (view_w, view_h) = (self.view_w, self.view_h);
        let arrangement = self.arrangement();
        let Some(doc) = self.doc.as_mut() else { return };
        let l = &mut doc.layout;
        let (page, fraction) = l.locate(self.scroll_y + anchor_y);
        let old_w = l.content_width().max(1);
        let x_fraction = (self.scroll_x + anchor_x) as f64 / old_w as f64;
        // The rotation and the rows first: the fit works on the pages as they are shown.
        l.arrange(arrangement);
        l.set(l.scale(), rotation, gap, margin);
        if presenting {
            self.mode = ZoomMode::FitPage;
        }
        if let Some((mode, z)) = zoom_to {
            self.mode = mode;
            self.zoom = zoom::clamp(z);
        }
        if self.mode != ZoomMode::Custom {
            self.zoom = fit_zoom(l, self.mode, page, (view_w, view_h), margin, dpi, presenting);
        }
        l.set(zoom::scale(self.zoom, dpi), rotation, gap, margin);
        self.scroll_y = l.y_of(page, fraction) - anchor_y;
        self.scroll_x = (x_fraction * l.content_width() as f64).round() as i64 - anchor_x;
        if presenting {
            let ph = l.size_px(page).1;
            (self.scroll_x, self.scroll_y) = (0, l.scroll_to_page(page, (view_h - ph) / 2));
        }
        self.clamp_scroll();
    }

    fn clamp_scroll(&mut self) {
        let Some(doc) = &self.doc else { return };
        // (The presentation is put where it is by `presented_at`, in the middle of the screen: that may be above the top of the document.)
        if self.present.is_some() {
            return;
        }
        let (mx, my) = doc.layout.max_scroll(self.view_w, self.view_h);
        self.scroll_x = self.scroll_x.clamp(0, mx);
        self.scroll_y = self.scroll_y.clamp(0, my);
    }

    /// The pages in view (first, last).
    fn visible(&self) -> Option<(u32, u32)> {
        let doc = self.doc.as_ref()?;
        let r = doc.layout.visible(self.scroll_y, self.scroll_y + self.view_h);
        (!r.is_empty()).then(|| (r.start as u32, (r.end - 1) as u32))
    }

    fn focus(&self) -> Focus {
        let (first, last) = self.visible().unwrap_or((0, 0));
        Focus { first, last, rotation: self.rotation, scale: self.draw_scale }
    }

    /// The page at the middle of the window (1-based for people).
    pub fn current_page(&self) -> usize {
        self.doc.as_ref().map_or(0, |d| d.layout.page_at(self.scroll_y + self.view_h / 2))
    }

    pub fn page_count(&self) -> usize {
        self.doc.as_ref().map_or(0, |d| d.layout.len())
    }

    /// After the view has moved or changed: bars, what to ask the engine, and a new picture. `moving`: the view is
    /// still being scrolled (a timer says when it has stopped).
    fn after_view_change(&mut self, moving: bool) -> Vec<Action> {
        let mut actions = Vec::new();
        self.moving = moving;
        if moving {
            actions.push(Action::SetTimer { id: TIMER_SETTLE, ms: SETTLE_MS });
        } else {
            actions.push(Action::KillTimer(TIMER_SETTLE));
        }
        // The list of thumbnails keeps its own place unless the page changed (then it shows the page); a page chosen in the list stays the
        // current one while it is in view, but any other way of moving forgets it.
        if !self.thumb_nav {
            self.thumb_target = None;
            // One page chosen is the page in view, and follows it; several chosen stay.
            if self.pg.chosen.len() <= 1 {
                self.pg.chosen.clear();
            }
        }
        self.sync_thumbs();
        self.sync_bars(&mut actions);
        self.schedule();
        self.ask_for_links();
        self.ask_for_annots();
        actions.extend(self.reposition_editor());
        actions.push(Action::Invalidate);
        actions
    }

    fn sync_bars(&mut self, actions: &mut Vec<Action>) {
        let Some(doc) = &self.doc else {
            let none = ScrollInfo { min: 0, max: 0, page: 0, pos: 0 };
            if self.bars != (Some(none), None) {
                self.bars = (Some(none), None);
                actions.push(Action::SetScroll { bar: Bar::Vertical, info: none });
                actions.push(Action::SetScroll { bar: Bar::Horizontal, info: none });
            }
            return;
        };
        let l = &doc.layout;
        // The bar counts in whole numbers up to 2^31: a very long document is counted in bigger steps.
        let step = (l.content_height() / 2_000_000_000 + 1).max(1);
        let v = ScrollInfo {
            min: 0,
            max: (l.content_height() / step - 1).max(0) as i32,
            page: (self.view_h / step).max(0) as u32,
            pos: (self.scroll_y / step) as i32,
        };
        let h = ScrollInfo { min: 0, max: (l.content_width() - 1).clamp(0, i64::from(i32::MAX)) as i32, page: self.view_w.max(0) as u32, pos: self.scroll_x.clamp(0, i64::from(i32::MAX)) as i32 };
        if self.bars.0 != Some(v) {
            self.bars.0 = Some(v);
            actions.push(Action::SetScroll { bar: Bar::Vertical, info: v });
        }
        if self.bars.1 != Some(h) {
            self.bars.1 = Some(h);
            actions.push(Action::SetScroll { bar: Bar::Horizontal, info: h });
        }
    }

    /// What the window shows, for the scheduler.
    fn scene(&self) -> Option<Scene<'_>> {
        let doc = self.doc.as_ref()?;
        Some(Scene {
            layout: &doc.layout,
            scroll_x: self.scroll_x,
            scroll_y: self.scroll_y,
            view_w: self.view_w,
            view_h: self.view_h,
            zoom: self.zoom,
            dpi: self.dpi,
            moving: self.moving,
            max_tile_pixels: doc.max_tile_pixels,
            budget: self.cache.budget(),
            present: self.present.is_some(),
        })
    }

    /// Ask the engine for what is missing and take back what is not wanted any more.
    fn schedule(&mut self) {
        // (A document that waited and was asked for is not drawn until the engine says what the plan is now.)
        let Some(scene) = self.scene().filter(|_| !self.activating) else { return };
        let scale = sched::draw_scale(&scene);
        let mut wanted = sched::wanted(&scene, &self.cache);
        self.draw_scale = scale;
        wanted.retain(|w| !self.failed.contains(&w.key));
        let plan = sched::plan(&wanted, &self.cache, &self.in_flight);
        for id in plan.cancel {
            self.engine.cancel(id);
            if let Some(w) = self.asked.remove(&id) {
                self.in_flight.remove(&w.key);
            }
        }
        // What the pages need comes before the thumbnails: the ones being drawn are taken back (they are asked for again when the pages are in).
        if !plan.issue.is_empty() {
            self.cancel_thumbs();
        }
        for w in plan.issue {
            let id = self.new_id();
            self.engine.render(RenderRequest {
                doc: self.doc_id,
                id,
                page: w.key.page,
                dpi: sched::dpi_of(w.key.scale),
                rotation: w.key.rotation,
                x: w.x.clamp(0, i64::from(u32::MAX)) as u32,
                y: w.y.clamp(0, i64::from(u32::MAX)) as u32,
                width: w.w.clamp(0, i64::from(u32::MAX)) as u32,
                height: w.h.clamp(0, i64::from(u32::MAX)) as u32,
                priority: w.priority,
            });
            self.in_flight.insert(w.key, id);
            self.asked.insert(id, w);
        }
        self.schedule_thumbs();
    }

    // --- the engine's answers --------------------------------------------------------------------------------

    fn pump_engine(&mut self) -> Vec<Action> {
        let mut actions = Vec::new();
        let mut redraw = false;
        while let Some(event) = self.engine.poll() {
            match event {
                EngineEvent::Opened(o) => actions.extend(self.on_opened(o)),
                EngineEvent::OpenFailed { doc, failure, message } => actions.extend(self.on_open_failed(doc, failure, &message)),
                EngineEvent::Activated { doc, bitmap_cache_bytes, thumb_cache_bytes, max_tile_pixels } => actions.extend(self.on_activated(doc, bitmap_cache_bytes, thumb_cache_bytes, max_tile_pixels)),
                EngineEvent::Dropped { doc } => actions.extend(self.on_dropped(doc)),
                EngineEvent::Rendered(r) => redraw |= self.on_rendered(r),
                other => actions.extend(self.on_other_event(other)),
            }
        }
        self.schedule_thumbs();
        if redraw {
            actions.push(Action::Invalidate);
        }
        actions.extend(self.pump_ocr());
        actions
    }

    /// A piece came back. `true`: it is in view, so the window needs a new picture.
    fn on_rendered(&mut self, mut r: Rendered) -> bool {
        if r.doc != self.doc_id {
            return false;
        }
        if let Some(page) = self.thumbs.asked.remove(&r.id) {
            return self.on_thumb_rendered(page, r);
        }
        let Some(want) = self.asked.remove(&r.id) else { return false };
        self.in_flight.remove(&want.key);
        let page = want.key.page;
        if r.status == RenderStatus::Failed {
            self.failed.insert(want.key);
            if !want.preview {
                self.notices.insert(page, Notice::Failed);
            }
            return self.page_in_view(page);
        }
        if r.status == RenderStatus::Incomplete && !want.preview {
            self.notices.insert(page, Notice::Incomplete);
        }
        // The picture is kept in the colours it is shown in (View > Page colours): working them out now means scrolling costs nothing more.
        recolor(&mut r.bgra, self.tint);
        let entry = Entry {
            key: want.key,
            width: r.width,
            height: r.height,
            bgra: r.bgra,
            preview: want.preview,
            x: want.x,
            y: want.y,
            page_w: want.page_w,
            page_h: want.page_h,
        };
        let focus = self.focus();
        self.cache.insert(entry, &focus);
        // The page is whole at this zoom now: what was drawn for another zoom can go.
        if !want.preview && self.page_is_sharp(page) {
            self.cache.drop_stale(page, focus.rotation, focus.scale);
        }
        self.page_in_view(page)
    }

    fn page_in_view(&self, page: u32) -> bool {
        self.visible().is_some_and(|(a, b)| (a..=b).contains(&page))
    }

    /// The size of a page in pixels at the scale the pieces in view are drawn at.
    fn page_px_at_draw_scale(&self, page: usize) -> (i64, i64) {
        let Some(doc) = &self.doc else { return (0, 0) };
        if self.draw_scale == sched::scale_key(self.zoom, self.dpi) {
            return doc.layout.size_px(page);
        }
        doc.layout.size_pt(page).map_or((0, 0), |(w, h)| sched::page_px(w, h, self.draw_scale))
    }

    /// Does the cache hold every tile of the page at the scale it is drawn at?
    fn page_is_sharp(&self, page: u32) -> bool {
        let Some(doc) = &self.doc else { return false };
        let (pw, ph) = self.page_px_at_draw_scale(page as usize);
        let have = self.cache.page(page).filter(|e| !e.preview && e.key.rotation == self.rotation && e.key.scale == self.draw_scale).count() as u64;
        have >= tiles::count(pw, ph, doc.max_tile_pixels)
    }

    /// Is everything in view drawn sharp (the pieces in view, at the scale they are drawn at)? Used by the speed test.
    pub fn visible_ready(&self) -> bool {
        let Some(scene) = self.scene() else { return false };
        let visible = scene.layout.visible(self.scroll_y, self.scroll_y + self.view_h);
        !visible.is_empty() && sched::in_view(&scene).0.iter().all(|w| self.cache.contains(&w.key) || self.failed.contains(&w.key))
    }

    /// Is the engine asked for nothing (everything wanted has come in)? Used by the speed test.
    pub fn is_idle(&self) -> bool {
        self.in_flight.is_empty()
    }

    /// Are the pieces in view drawn smaller than the zoom, and stretched (they would not fit the cache)?
    pub fn reduced(&self) -> bool {
        self.draw_scale != sched::scale_key(self.zoom, self.dpi)
    }

    /// When the first picture with any page bitmap in it was drawn.
    pub fn first_bitmap_painted(&self) -> Option<Instant> {
        self.first_bitmap
    }

    /// When the last picture was drawn that had every piece in view sharp (none since `clear_last_sharp_paint`).
    pub fn last_sharp_paint(&self) -> Option<Instant> {
        self.last_sharp
    }

    pub fn clear_last_sharp_paint(&mut self) {
        self.last_sharp = None;
    }

    pub fn has_document(&self) -> bool {
        self.doc.is_some()
    }

    /// The size of the area the pages are shown in, in pixels (the window less the toolbar, the status bar and the sidebar).
    pub fn page_area(&self) -> (i64, i64) {
        (self.view_w, self.view_h)
    }

    /// Is the sidebar beside the pages?
    pub fn shows_sidebar(&self) -> bool {
        self.view_x > 0
    }

    /// For the speed test: let go of every bitmap and every request.
    pub fn forget_bitmaps(&mut self) {
        for (id, _) in self.asked.drain() {
            self.engine.cancel(id);
        }
        self.in_flight.clear();
        self.cache.clear();
        self.notices.clear();
        self.failed.clear();
    }

    // --- moving about ----------------------------------------------------------------------------------------

    /// Scroll to a place (document pixels), clamped; `moving`: more is coming.
    pub fn scroll_to(&mut self, x: i64, y: i64, moving: bool) -> Vec<Action> {
        self.scroll_x = x;
        self.scroll_y = y;
        self.clamp_scroll();
        self.after_view_change(moving)
    }

    pub fn go_to_page(&mut self, page: usize) -> Vec<Action> {
        if self.present.is_some() {
            return self.present_goto(page);
        }
        let Some(doc) = &self.doc else { return Vec::new() };
        let page = page.min(doc.layout.len().saturating_sub(1));
        let y = doc.layout.scroll_to_page(page, self.px(MARGIN) / 2);
        self.scroll_to(self.scroll_x, y, false)
    }

    fn set_zoom(&mut self, mode: ZoomMode, zoom_value: u32, anchor: Option<(i64, i64)>) -> Vec<Action> {
        if self.doc.is_none() {
            return Vec::new();
        }
        let (ax, ay) = anchor.unwrap_or((self.view_w / 2, self.view_h / 2));
        self.relayout(ax, ay, Some((mode, zoom_value)));
        self.after_view_change(true)
    }

    fn rotate(&mut self, quarter_turns: u16) -> Vec<Action> {
        if self.doc.is_none() {
            return Vec::new();
        }
        self.rotation = (self.rotation + 90 * quarter_turns) % 360;
        // The page in the middle of the window stays there.
        let (ax, ay) = (self.view_w / 2, self.view_h / 2);
        self.relayout(ax, ay, None);
        self.after_view_change(false)
    }

    fn scroll_by(&mut self, dx: i64, dy: i64, moving: bool) -> Vec<Action> {
        self.scroll_to(self.scroll_x + dx, self.scroll_y + dy, moving)
    }

    fn command(&mut self, cmd: u32) -> Vec<Action> {
        match cmd {
            CMD_OPEN => {
                let filters = vec![(t(self.lang, Msg::FilterPdf).to_string(), "*.pdf".to_string()), (t(self.lang, Msg::FilterAll).to_string(), "*.*".to_string())];
                vec![Action::PickFile { title: t(self.lang, Msg::OpenTitle).to_string(), filters }]
            }
            CMD_EXIT => self.quit_asked(),
            CMD_CLOSE_TAB => self.close_tab_command(),
            CMD_NEXT_TAB => self.step_tab(true),
            CMD_PREV_TAB => self.step_tab(false),
            CMD_SAVE => self.save(),
            CMD_SAVE_AS => self.save_as(),
            CMD_EXPORT_DOCX | CMD_EXPORT_XLSX => self.export_file(cmd == CMD_EXPORT_XLSX),
            CMD_OCR_ALL | CMD_OCR_PAGE => self.start_ocr(cmd == CMD_OCR_PAGE),
            CMD_UNDO => self.undo(),
            CMD_REDO => self.redo(),
            CMD_REMOVE => self.remove_picked(),
            CMD_AUTHOR => self.ask_author(),
            CMD_STAMP | CMD_INS_IMAGE => self.ask_for_stamp(cmd == CMD_INS_IMAGE),
            CMD_TOOL_TEXTBOX | CMD_INS_TEXT => self.text_tool(cmd == CMD_INS_TEXT),
            CMD_INS_WM => self.ask_watermark(),
            CMD_INS_PAGENUM => self.ask_pagenum(),
            CMD_PROTECT => self.ask_protect(),
            CMD_COMPRESS => self.ask_compress(),
            CMD_TOOL_REDACT => self.toggle_tool(Tool::Shape(ShapeKind::Redact)),
            CMD_REDACT_SELECTION => self.redact_selection(),
            CMD_REDACT_FIND => self.ask_redact_find(),
            CMD_REDACT_APPLY => self.ask_redact_apply(),
            CMD_WM_OPACITY_BASE..=CMD_WM_PAGE => self.set_watermark_option(cmd),
            c if (CMD_SIZE_BASE..CMD_SIZE_BASE + 4).contains(&c) => self.set_text_size((c - CMD_SIZE_BASE) as usize),
            CMD_TOOL_SELECT => self.set_tool(Tool::Select),
            CMD_TOOL_HIGHLIGHT => self.toggle_tool(Tool::Markup(MarkupKind::Highlight)),
            CMD_TOOL_UNDERLINE => self.set_tool(Tool::Markup(MarkupKind::Underline)),
            CMD_TOOL_STRIKE => self.set_tool(Tool::Markup(MarkupKind::StrikeOut)),
            CMD_TOOL_SQUIGGLY => self.set_tool(Tool::Markup(MarkupKind::Squiggly)),
            CMD_TOOL_INK => self.toggle_tool(Tool::Ink),
            CMD_TOOL_NOTE => self.toggle_tool(Tool::Note),
            CMD_TOOL_EDITTEXT => self.set_tool(Tool::EditText),
            CMD_TOOL_RECT => self.toggle_tool(Tool::Shape(ShapeKind::Rectangle)),
            CMD_TOOL_ELLIPSE => self.toggle_tool(Tool::Shape(ShapeKind::Ellipse)),
            CMD_TOOL_LINE => self.toggle_tool(Tool::Shape(ShapeKind::Line)),
            CMD_TOOL_ARROW => self.toggle_tool(Tool::Shape(ShapeKind::Arrow)),
            CMD_FILL => self.toggle_fill(),
            c if (CMD_COLOR_BASE..CMD_COLOR_BASE + 6).contains(&c) => self.set_color((c - CMD_COLOR_BASE) as usize),
            c if (CMD_WIDTH_BASE..CMD_WIDTH_BASE + 4).contains(&c) => self.set_width((c - CMD_WIDTH_BASE) as usize),
            CMD_PRINT => self.print_command(),
            CMD_CLEAR_RECENT => self.clear_recent(),
            CMD_ABOUT => vec![Action::Message { title: t(self.lang, Msg::AboutTitle).to_string(), text: i18n::about(self.lang) }],
            CMD_ZOOM_IN => self.set_zoom(ZoomMode::Custom, zoom::step_in(self.zoom), None),
            CMD_ZOOM_OUT => self.set_zoom(ZoomMode::Custom, zoom::step_out(self.zoom), None),
            CMD_FIT_WIDTH => self.set_zoom(ZoomMode::FitWidth, self.zoom, None),
            CMD_FIT_PAGE => self.set_zoom(ZoomMode::FitPage, self.zoom, None),
            CMD_ACTUAL => self.set_zoom(ZoomMode::Custom, zoom::ACTUAL_SIZE, None),
            CMD_ROTATE => self.rotate(1),
            CMD_LAYOUT_SINGLE => self.set_spread(Spread::Single),
            CMD_LAYOUT_SPREAD => self.set_spread(Spread::Pair),
            CMD_LAYOUT_COVER => self.set_spread(Spread::Cover),
            CMD_FULLSCREEN => self.toggle_full(),
            CMD_PRESENT => self.start_present(),
            CMD_THEME_SYSTEM => self.set_theme(ThemeChoice::System),
            CMD_THEME_LIGHT => self.set_theme(ThemeChoice::Light),
            CMD_THEME_DARK => self.set_theme(ThemeChoice::Dark),
            CMD_TINT_ORIGINAL => self.set_tint(Tint::Original),
            CMD_TINT_NIGHT => self.set_tint(Tint::Night),
            CMD_TINT_SEPIA => self.set_tint(Tint::Sepia),
            CMD_PREV_PAGE => self.step_page(false),
            CMD_NEXT_PAGE => self.step_page(true),
            CMD_TOOLBAR => self.toggle_toolbar(),
            CMD_LANG_SYSTEM => self.set_language(LangChoice::System),
            CMD_LANG_ZH => self.set_language(LangChoice::Zh),
            CMD_LANG_EN => self.set_language(LangChoice::En),
            CMD_CTX_HIGHLIGHT => self.highlight_selection(),
            CMD_CTX_NOTE => self.note_here(),
            CMD_CTX_SELECT_ALL => self.select_all_here(),
            CMD_PG_LEFT => self.rotate_pages(-90),
            CMD_PG_RIGHT => self.rotate_pages(90),
            CMD_PG_DELETE => self.delete_pages(),
            CMD_PG_INS_BEFORE | CMD_PG_INS_AFTER => self.ask_insert_file(cmd == CMD_PG_INS_AFTER),
            CMD_PG_EXTRACT => self.ask_extract(),
            CMD_BOOKMARKS => self.toggle_sidebar(),
            CMD_BM_ADD => self.bm_add(),
            CMD_BM_RENAME => self.bm_rename(),
            CMD_BM_DELETE => self.bm_delete(),
            CMD_BM_INDENT | CMD_BM_OUTDENT => self.bm_shift(cmd == CMD_BM_INDENT),
            CMD_BM_HERE => self.bm_here(),
            CMD_BM_AUTO => self.bm_auto(),
            CMD_COPY => self.copy_selection(),
            CMD_GOTO if self.doc.is_some() => {
                let title = t(self.lang, Msg::GoToTitle).to_string();
                let prompt = format!("{} (1-{})", t(self.lang, Msg::GoToTitle), self.page_count());
                let (ok, cancel) = (t(self.lang, Msg::Ok).to_string(), t(self.lang, Msg::Cancel).to_string());
                vec![Action::AskText { tag: TAG_GOTO, title, prompt, secret: false, initial: String::new(), ok, cancel }]
            }
            CMD_FIND => self.open_find(),
            CMD_FIND_NEXT => self.find_step(true),
            CMD_FIND_PREV => self.find_step(false),
            c if (CMD_RECENT_BASE..CMD_RECENT_BASE + recent::MAX_RECENT as u32).contains(&c) => self.open_recent((c - CMD_RECENT_BASE) as usize),
            _ => Vec::new(),
        }
    }

    fn key(&mut self, key: u32, ctrl: bool, shift: bool) -> Vec<Action> {
        // In the presentation the keys turn the pages, and nothing else is done by them.
        if self.present.is_some() {
            return self.present_key(key, shift);
        }
        // The arrow keys nudge the annotation that is picked; anything else ends the nudging, which is sent as one move first.
        if !ctrl && matches!(key, vk::LEFT | vk::RIGHT | vk::UP | vk::DOWN) && let Some(actions) = self.nudge_key(key, shift) {
            return actions;
        }
        let mut actions = self.flush_nudge();
        actions.extend(self.key_now(key, ctrl, shift));
        actions
    }

    fn key_now(&mut self, key: u32, ctrl: bool, shift: bool) -> Vec<Action> {
        // The list of thumbnails has the keyboard: the arrow keys move from page to page.
        if !ctrl && self.thumb_focus && self.thumbs_shown() && let Some(list_key) = sidebar::list_key(key) {
            return self.thumb_key(list_key);
        }
        // ... and so has the list of annotations.
        if !ctrl && self.annots_focus && self.annots_shown() && let Some(list_key) = sidebar::list_key(key) {
            return self.annots_key(list_key);
        }
        // ... and the tree of bookmarks: F2 types over the title, Delete takes the bookmark out, Tab and Shift+Tab move it a level.
        if !ctrl && let Some(actions) = self.bm_key(key, shift) {
            return actions;
        }
        let page_step = (self.view_h * 9 / 10).max(1);
        let line = self.px(LINE_STEP);
        if ctrl {
            return match key {
                vk::O => self.command(CMD_OPEN),
                vk::B => self.command(CMD_BM_ADD),
                vk::TAB => self.command(if shift { CMD_PREV_TAB } else { CMD_NEXT_TAB }),
                vk::W | vk::F4 => self.command(CMD_CLOSE_TAB),
                vk::G => self.command(CMD_GOTO),
                vk::F => self.command(CMD_FIND),
                vk::P => self.command(CMD_PRINT),
                vk::C => self.command(CMD_COPY),
                vk::A if self.thumb_focus && self.thumbs_shown() => {
                    self.pg.chosen.all(self.page_count());
                    vec![Action::Invalidate]
                }
                vk::OEM_4 => self.command(CMD_PG_LEFT),
                vk::OEM_6 => self.command(CMD_PG_RIGHT),
                vk::S => self.command(if shift { CMD_SAVE_AS } else { CMD_SAVE }),
                vk::Z => self.command(CMD_UNDO),
                vk::Y => self.command(CMD_REDO),
                vk::R => {
                    if shift {
                        self.rotate(3)
                    } else {
                        self.rotate(1)
                    }
                }
                vk::OEM_PLUS | vk::ADD => self.command(CMD_ZOOM_IN),
                vk::OEM_MINUS | vk::SUBTRACT => self.command(CMD_ZOOM_OUT),
                vk::NUM0 | vk::NUMPAD0 => self.command(CMD_ACTUAL),
                vk::NUM1 => self.command(CMD_FIT_WIDTH),
                vk::NUM2 => self.command(CMD_FIT_PAGE),
                vk::PRIOR => self.command(CMD_PREV_PAGE),
                vk::NEXT => self.command(CMD_NEXT_PAGE),
                _ => Vec::new(),
            };
        }
        match key {
            vk::ESCAPE => self.escape(),
            vk::RETURN => match self.stamp_place() {
                Some(actions) => actions,
                None => self.find_enter(shift),
            },
            vk::F3 => self.find_step(!shift),
            vk::F4 => self.command(CMD_BOOKMARKS),
            vk::F5 => self.command(CMD_PRESENT),
            vk::F11 => self.command(CMD_FULLSCREEN),
            // In the list of thumbnails Delete takes pages out; over the pages it takes the annotation picked out.
            vk::DELETE if self.thumb_focus && self.thumbs_shown() => self.command(CMD_PG_DELETE),
            vk::DELETE if self.annots_focus && self.annots_shown() => self.annots_delete(),
            vk::DELETE => self.command(CMD_REMOVE),
            vk::NEXT => self.scroll_by(0, page_step, false),
            vk::PRIOR => self.scroll_by(0, -page_step, false),
            vk::SPACE => self.scroll_by(0, if shift { -page_step } else { page_step }, false),
            vk::DOWN => self.scroll_by(0, line, false),
            vk::UP => self.scroll_by(0, -line, false),
            vk::RIGHT => self.scroll_by(line, 0, false),
            vk::LEFT => self.scroll_by(-line, 0, false),
            vk::HOME => self.scroll_to(self.scroll_x, 0, false),
            vk::END => self.scroll_to(self.scroll_x, i64::MAX / 4, false),
            _ => Vec::new(),
        }
    }

    fn scrolled_by_bar(&mut self, bar: Bar, code: ScrollCode) -> Vec<Action> {
        let line = self.px(LINE_STEP);
        let page_step = (self.view_h * 9 / 10).max(1);
        let Some(doc) = &self.doc else { return Vec::new() };
        let step = (doc.layout.content_height() / 2_000_000_000 + 1).max(1);
        match (bar, code) {
            (Bar::Vertical, ScrollCode::LineUp) => self.scroll_by(0, -line, false),
            (Bar::Vertical, ScrollCode::LineDown) => self.scroll_by(0, line, false),
            (Bar::Vertical, ScrollCode::PageUp) => self.scroll_by(0, -page_step, false),
            (Bar::Vertical, ScrollCode::PageDown) => self.scroll_by(0, page_step, false),
            (Bar::Vertical, ScrollCode::Top) => self.scroll_to(self.scroll_x, 0, false),
            (Bar::Vertical, ScrollCode::Bottom) => self.scroll_to(self.scroll_x, i64::MAX / 4, false),
            (Bar::Vertical, ScrollCode::Thumb(p)) => self.scroll_to(self.scroll_x, i64::from(p) * step, true),
            (Bar::Horizontal, ScrollCode::LineUp) => self.scroll_by(-line, 0, false),
            (Bar::Horizontal, ScrollCode::LineDown) => self.scroll_by(line, 0, false),
            (Bar::Horizontal, ScrollCode::PageUp) => self.scroll_by(-self.view_w * 9 / 10, 0, false),
            (Bar::Horizontal, ScrollCode::PageDown) => self.scroll_by(self.view_w * 9 / 10, 0, false),
            (Bar::Horizontal, ScrollCode::Top) => self.scroll_to(0, self.scroll_y, false),
            (Bar::Horizontal, ScrollCode::Bottom) => self.scroll_to(i64::MAX / 4, self.scroll_y, false),
            (Bar::Horizontal, ScrollCode::Thumb(p)) => self.scroll_to(i64::from(p), self.scroll_y, true),
        }
    }

    // --- a line in the status bar ------------------------------------------------------------------------------

    /// Say something in the status bar for a few seconds.
    fn say(&mut self, text: String) -> Vec<Action> {
        self.note = Some(text);
        vec![Action::SetTimer { id: TIMER_NOTE, ms: NOTE_MS }, Action::Invalidate]
    }

    // --- drawing ---------------------------------------------------------------------------------------------

    /// Draw a page; `true` if some of its bitmaps are on it.
    fn draw_page(&self, p: &mut dyn Painter, page: usize) -> bool {
        let Some(doc) = &self.doc else { return false };
        let l = &doc.layout;
        let (pw, ph) = l.size_px(page);
        let (left, top) = (l.left(page, self.view_w, self.scroll_x) + self.view_x, l.top(page) - self.scroll_y);
        let rect = |x: i64, y: i64, w: i64, h: i64| Rect { x: clamp32(x), y: clamp32(y), w: clamp32(w), h: clamp32(h) };
        p.fill_rect(rect(left - 1, top - 1, pw + 2, ph + 2), self.page_border());
        p.fill_rect(rect(left, top, pw, ph), self.tint.paper());
        let want = self.draw_scale;
        let mut entries: Vec<&Entry> = self.cache.page(page as u32).filter(|e| e.key.rotation == self.rotation).collect();
        let exact = entries.iter().filter(|e| !e.preview && e.key.scale == want).count() as u64;
        let (dw, dh) = self.page_px_at_draw_scale(page);
        let whole = exact >= tiles::count(dw, dh, doc.max_tile_pixels);
        if whole {
            entries.retain(|e| !e.preview && e.key.scale == want);
        }
        // Coarse first, so that the sharp pieces come over them.
        let rank = |e: &&Entry| (e.key.scale == want && !e.preview, e.key.scale, e.key.ty, e.key.tx);
        qingpdf_core::order::sort_by(&mut entries, |a, b| rank(a).cmp(&rank(b)));
        let drew = !entries.is_empty();
        for e in entries {
            // The piece is put where the same part of the page is now: one drawn at the page's own size lands on its
            // pixels, a smaller one (another zoom, a preview, a page drawn smaller to fit the cache) is stretched.
            let (sx, sy) = (pw as f64 / e.page_w.max(1) as f64, ph as f64 / e.page_h.max(1) as f64);
            let (x0, y0) = ((e.x as f64 * sx).floor() as i64, (e.y as f64 * sy).floor() as i64);
            let (x1, y1) = (((e.x + i64::from(e.width)) as f64 * sx).ceil() as i64, ((e.y + i64::from(e.height)) as f64 * sy).ceil() as i64);
            p.draw_bgra(rect(left + x0, top + y0, x1 - x0, y1 - y0), e.width as i32, e.height as i32, &e.bgra);
        }
        // Found words and selected text lie over the page.
        self.draw_marks(p, page, left, top);
        self.draw_edit_marks(p, page, left, top);
        match self.notices.get(&(page as u32)) {
            Some(Notice::Incomplete) => {
                let strip = i64::from(p.line_height()) + 8;
                p.fill_rect(rect(left, top, pw, strip), self.pal.notice_bg);
                p.text(clamp32(left + 8), clamp32(top + 4), t(self.lang, Msg::NoticeIncomplete), self.pal.notice_fg);
            }
            Some(Notice::Failed) => {
                let s = t(self.lang, Msg::NoticeFailed);
                let w = i64::from(p.text_width(s));
                p.text(clamp32(left + (pw - w) / 2), clamp32(top + 24), s, self.pal.notice_fg);
            }
            None => {}
        }
        drew
    }
}

fn clamp32(v: i64) -> i32 {
    v.clamp(-100_000_000, 100_000_000) as i32
}

/// The zoom that fits the pages into the window: across, the widest page (so that no page is wider than the window
/// and the window never needs a horizontal bar; the narrower ones have room at the sides); for a whole page, the
/// height of page `page` too.
fn fit_zoom(l: &Layout, mode: ZoomMode, page: usize, (view_w, view_h): (i64, i64), margin: i64, dpi: u32, alone: bool) -> u32 {
    let Some((w_pt, _)) = l.size_pt(page) else { return zoom::ACTUAL_SIZE };
    // (In a spread it is the widest row, with the gap between its pages, and the height of the row the page is in; for a page shown alone, its own.)
    let (widest, between) = if alone { (w_pt, 0) } else { l.widest_pt() };
    if alone && mode == ZoomMode::FitPage && margin == 0 {
        return zoom::fit_screen(w_pt, l.row_height_pt(page), view_w, view_h, dpi);
    }
    let across = zoom::fit((view_w - 2 * margin - between) as f64, widest, dpi);
    match mode {
        ZoomMode::FitWidth => across,
        ZoomMode::FitPage => across.min(zoom::fit((view_h - 2 * margin) as f64, l.row_height_pt(page), dpi)),
        ZoomMode::Custom => zoom::ACTUAL_SIZE,
    }
}

/// Move the places in an action that the window puts in its client area down by the height of the strip of tabs (the reader thinks in the
/// coordinates of the pages' area, which is below the strip).
fn below_strip(action: &mut Action, dy: i32) {
    match action {
        Action::Sidebar(r) | Action::FieldMove(r) | Action::Find(Some(r)) => r.y = r.y.saturating_add(dy),
        Action::FieldEdit(Some(b)) => b.rect.y = b.rect.y.saturating_add(dy),
        Action::Choose { at, .. } | Action::ContextMenu { at, .. } => at.1 = at.1.saturating_add(dy),
        _ => {}
    }
}

impl App {
    /// An event as the strip of tabs sees it: `Err` with the actions when the strip took it (a click on a tab, the pointer moving over it); `Ok`
    /// with the event in the coordinates of the pages' area, which is below the strip, when it is for the pages.
    fn route(&mut self, event: Event) -> Result<Event, Vec<Action>> {
        let bar = self.bar_h();
        let dy = clamp32(bar);
        // A click that began in the strip is not for the pages, nor is what follows it.
        match event {
            Event::MouseMove { .. } if self.strip_press => return Err(Vec::new()),
            Event::MouseUp { .. } if std::mem::take(&mut self.strip_press) => return Err(Vec::new()),
            Event::HoverEnd => return Err(self.strip_hover(None)),
            _ => {}
        }
        if bar == 0 {
            return match event {
                Event::Hover { .. } | Event::MiddleClick { .. } => Err(self.strip_hover(None)),
                other => Ok(other),
            };
        }
        match event {
            Event::MouseDown { x, y, .. } if y < dy => Err(self.strip_down(i64::from(x))),
            Event::MouseDown { x, y, ctrl, shift, clicks } => Ok(Event::MouseDown { x, y: y - dy, ctrl, shift, clicks }),
            Event::MouseMove { x, y, shift } => Ok(Event::MouseMove { x, y: y - dy, shift }),
            Event::MouseUp { x, y } => Ok(Event::MouseUp { x, y: y - dy }),
            Event::Wheel { y, .. } | Event::ContextMenu { y, .. } if y < dy => Err(Vec::new()),
            Event::Wheel { delta, ctrl, shift, x, y } => Ok(Event::Wheel { delta, ctrl, shift, x, y: y - dy }),
            Event::ContextMenu { x, y } => Ok(Event::ContextMenu { x, y: y - dy }),
            Event::OutlineMenu { index, x, y } => Ok(Event::OutlineMenu { index, x, y: y - dy }),
            Event::Hover { x, y } => Err(self.strip_hover((y < dy).then_some(i64::from(x)))),
            Event::MiddleClick { x, y } if y < dy => Err(self.strip_middle(i64::from(x))),
            Event::MiddleClick { .. } => Err(Vec::new()),
            other => Ok(other),
        }
    }

    /// Whatever the event was, the engine may have something waiting; the speed test may have its next step.
    fn finish(&mut self, mut actions: Vec<Action>) -> Vec<Action> {
        self.sync_tab();
        // The window's title is always the shown file's (whatever else was said about it).
        let title = self.title();
        if title != self.last_title {
            actions.push(Action::SetTitle(title.clone()));
            self.last_title = title;
        }
        if self.menus_dirty {
            self.menus_dirty = false;
            actions.push(Action::SetMenus(self.menus()));
        }
        actions.extend(self.chrome_updates());
        if let Some(mut bench) = self.bench.take() {
            actions.extend(bench.step(self));
            self.bench = Some(bench);
        }
        actions
    }
}

impl Handler for App {
    fn event(&mut self, event: Event) -> Vec<Action> {
        // In the presentation the pointer moving is what keeps it on the screen.
        if let Event::Hover { x, y } = &event
            && self.present.is_some()
        {
            return self.present_pointer(i64::from(*x), i64::from(*y));
        }
        // The pointer moving over the window is heard all the time: only the strip of tabs has anything to do with it.
        if matches!(event, Event::Hover { .. } | Event::HoverEnd) {
            return self.route(event).err().unwrap_or_default();
        }
        let mut actions = match self.route(event) {
            Ok(event) => self.handle(event),
            Err(actions) => self.finish(actions),
        };
        // (The strip may have come or gone while the event was handled: what the reader says is by the pages' area as it is now.)
        let dy = clamp32(self.bar_h());
        if dy > 0 {
            actions.iter_mut().for_each(|a| below_strip(a, dy));
        }
        actions
    }

    fn paint(&mut self, p: &mut dyn Painter) -> bool {
        let dy = clamp32(self.bar_h());
        if dy == 0 {
            return self.paint_pages(p);
        }
        let drawn = self.paint_pages(&mut tabbar::Shifted { inner: &mut *p, dy });
        self.draw_tabbar(p);
        drawn
    }

    fn cursor_at(&mut self, x: i32, y: i32) -> Cursor {
        if self.pointer_hidden_at(i64::from(x), i64::from(y)) {
            return Cursor::Hidden;
        }
        let dy = clamp32(self.bar_h());
        if y < dy {
            return Cursor::Arrow;
        }
        let (x, y) = (i64::from(x), i64::from(y - dy));
        match self.cursor_over(x, y) {
            Cursor::Arrow => self.text_cursor(x, y),
            shape => shape,
        }
    }

    fn unsaved_reason(&self) -> Option<String> {
        let shown = self.tabs.active();
        let any = self.tabs.iter().enumerate().any(|(i, t)| if Some(i) == shown { self.edit.dirty } else { t.dirty });
        ((self.edit.dirty && self.doc.is_some()) || any).then(|| t(self.lang, Msg::ShutdownReason).to_string())
    }
}

impl App {
    fn handle(&mut self, event: Event) -> Vec<Action> {
        let actions = match event {
            Event::Size { w, h } => {
                self.client_w = i64::from(w);
                self.client_h = i64::from(h).max(0);
                self.ready = true;
                self.view_x = self.sidebar_width();
                self.view_w = (i64::from(w) - self.view_x).max(0);
                self.view_h = (self.client_h - self.bar_h()).max(0);
                let (ax, ay) = (self.view_w / 2, self.view_h / 2);
                self.relayout(ax, ay, None);
                self.clamp_annots_scroll();
                let mut actions = match self.restore_page.take() {
                    Some(page) => self.go_to_page(page),
                    None => self.after_view_change(false),
                };
                actions.extend(self.chrome());
                actions.extend(self.open_queued());
                actions
            }
            Event::Dpi(dpi) => {
                self.dpi = dpi.clamp(48, 960);
                // The pages are the same size in inches, the pixels change: draw them all again.
                self.forget_bitmaps();
                self.view_x = self.sidebar_width();
                self.view_w = (self.client_w - self.view_x).max(0);
                self.view_h = (self.client_h - self.bar_h()).max(0);
                let (ax, ay) = (self.view_w / 2, self.view_h / 2);
                self.relayout(ax, ay, None);
                let mut actions = self.after_view_change(false);
                actions.extend(self.chrome());
                actions
            }
            Event::Key { vk, ctrl, shift } => self.key(vk, ctrl, shift),
            Event::Wheel { delta, ctrl, shift, x, y } => {
                if self.present.is_some() {
                    self.present_wheel(delta)
                } else if !ctrl && !shift && self.over_thumbs(i64::from(x), i64::from(y)) {
                    self.thumb_wheel(delta)
                } else if !ctrl && !shift && self.over_annots(i64::from(x), i64::from(y)) {
                    self.annots_wheel(delta)
                } else if ctrl {
                    let z = zoom::wheel(self.zoom, f64::from(delta) / 120.0);
                    self.set_zoom(ZoomMode::Custom, z, Some((i64::from(x) - self.view_x, i64::from(y))))
                } else if shift {
                    self.scroll_by(-i64::from(delta) * self.px(WHEEL_STEP) / 120, 0, true)
                } else {
                    self.scroll_by(0, -i64::from(delta) * self.px(WHEEL_STEP) / 120, true)
                }
            }
            Event::HWheel { delta } => self.scroll_by(i64::from(delta) * self.px(WHEEL_STEP) / 120, 0, true),
            Event::Scroll { bar, code } => self.scrolled_by_bar(bar, code),
            Event::Command(cmd) => {
                if cmd != CMD_BOOKMARKS && !pages::is_page_command(cmd) {
                    self.thumb_focus = false;
                }
                if !(CMD_BM_ADD..=CMD_BM_AUTO).contains(&cmd) {
                    self.bm.focus = false;
                }
                let mut actions = self.flush_nudge();
                actions.extend(self.command(cmd));
                actions
            }
            Event::Drop(files) => {
                // (A file that comes in the presentation ends it.)
                let mut actions = self.end_present(false);
                actions.extend(self.dropped(files));
                actions
            }
            Event::Wake => self.pump_engine(),
            Event::SystemDark(dark) => self.set_system_dark(dark),
            Event::Timer(TIMER_SETTLE) => {
                self.moving = false;
                self.schedule();
                vec![Action::KillTimer(TIMER_SETTLE)]
            }
            Event::Timer(TIMER_FIND) => self.find_timer(),
            Event::Timer(TIMER_NOTE) => {
                self.note = None;
                vec![Action::KillTimer(TIMER_NOTE), Action::Invalidate]
            }
            Event::Timer(TIMER_DRAG) => self.drag_timer(),
            Event::Timer(TIMER_THUMB_DRAG) => self.thumb_drag_tick(),
            Event::Timer(TIMER_NUDGE) => self.flush_nudge(),
            Event::Timer(TIMER_CURSOR) => self.present_idle(),
            Event::Timer(_) => Vec::new(),
            Event::FilePicked(path) if self.pg.insert_pending() => self.insert_picked(path).unwrap_or_default(),
            Event::FilePicked(Some(path)) => {
                if std::mem::take(&mut self.edit.forms.picking) {
                    self.stamp_picked(&path)
                } else {
                    self.open_path(&path, "")
                }
            }
            Event::FilePicked(None) => {
                self.edit.forms.picking = false;
                Vec::new()
            }
            Event::FieldDone { text, exit } => self.field_done(text, exit),
            Event::Chosen(item) => self.chosen(item),
            Event::Text { tag: TAG_PASSWORD, text } => match (text, self.path.is_some()) {
                // (The tab made for the file is still there: it is read again with the password.)
                (Some(mut pw), true) => {
                    let actions = self.begin_open(&pw);
                    // The password has gone to the engine; overwrite our copy.
                    let mut bytes = std::mem::take(&mut pw).into_bytes();
                    bytes.fill(0);
                    std::hint::black_box(&bytes);
                    actions
                }
                _ => {
                    let mut actions = self.close_current();
                    actions.extend(self.open_queued());
                    actions
                }
            },
            Event::Text { tag: TAG_SRC_PASSWORD, text } => self.source_password_answered(text),
            Event::Text { tag: TAG_NOTE, text } => self.note_answered(text),
            Event::Text { tag: TAG_NOTE_EDIT, text } => self.note_edit_answered(text),
            Event::Text { tag: TAG_AUTHOR, text } => self.author_answered(text),
            Event::Text { tag: TAG_WATERMARK, text } => self.watermark_answered(text),
            Event::Text { tag: TAG_GOTO, text } => match text.and_then(|s| s.trim().parse::<usize>().ok()) {
                Some(n) if n >= 1 => self.go_to_page(n - 1),
                _ => Vec::new(),
            },
            Event::Text { .. } => Vec::new(),
            Event::Form { tag: TAG_PAGENUM, answer } => self.pagenum_answered(answer),
            Event::Form { tag: TAG_PROTECT, answer } => self.protect_answered(answer),
            Event::Form { tag: TAG_COMPRESS, answer } => self.compress_answered(answer),
            Event::Form { tag: TAG_REDACT_FIND, answer } => self.redact_find_answered(answer),
            Event::Form { tag: TAG_REDACT_APPLY, answer } => self.redact_apply_answered(answer),
            Event::Form { .. } => Vec::new(),
            Event::MouseDown { x, y, .. } if self.present.is_some() => self.present_click(i64::from(x), i64::from(y)),
            Event::MouseDown { x, y, ctrl, shift, clicks } => self.mouse_down(i64::from(x), i64::from(y), ctrl, shift, clicks),
            // (In the presentation the right button goes back a page.)
            Event::ContextMenu { .. } if self.present.is_some() => self.present_turn(-1),
            Event::ContextMenu { x, y } => self.context_menu(i64::from(x), i64::from(y)),
            Event::FindFocus => self.find_focused(),
            Event::PageBox(text) => match crate::toolbar::parse_page_box(&text, self.page_count()) {
                Some(page) => self.go_to_page(page),
                None => Vec::new(),
            },
            Event::MouseMove { x, y, .. } if self.present.is_some() => self.present_pointer(i64::from(x), i64::from(y)),
            Event::MouseUp { .. } if self.present.is_some() => Vec::new(),
            Event::MouseMove { x, y, shift } => self.mouse_move(i64::from(x), i64::from(y), shift),
            Event::MouseUp { x, y } => self.mouse_up(i64::from(x), i64::from(y)),
            Event::MouseLost => self.mouse_lost(),
            Event::CloseRequested => self.quit_asked(),
            Event::Hover { .. } | Event::HoverEnd | Event::MiddleClick { .. } => Vec::new(),
            Event::SavePicked(path) => self.save_picked(path),
            Event::SaveChoice { tag: TAG_UNSAVED, choice } => self.unsaved_answered(choice),
            Event::SaveChoice { .. } => Vec::new(),
            Event::OutlineClick(i) => self.outline_click(i),
            Event::OutlineRename { index, text } => self.bm_renamed(index, &text),
            Event::OutlineDrop { from, to, place } => self.bm_moved(from, to, place),
            Event::OutlineExpand { index, open } => self.bm_expanded(index, open),
            Event::OutlineMenu { index, x, y } => self.bm_menu(index, x, y),
            Event::FindText(text) => self.find_text_changed(text),
            Event::Confirmed { tag: TAG_LINK, yes } => self.link_confirmed(yes),
            Event::Confirmed { tag: TAG_BM_AUTO, yes } => self.bm_auto_answered(yes),
            Event::Confirmed { tag: TAG_COMPRESS_SAVE, yes } => self.compress_save_answered(yes),
            Event::Confirmed { tag: TAG_COMPRESS_OPEN, yes } => self.compress_open_answered(yes),
            Event::Confirmed { .. } => Vec::new(),
            Event::Printer(setup) => self.printer_chosen(setup),
            Event::PrintStepDone => self.print_step_done(),
            Event::PrintError(message) => self.print_error(&message),
            Event::ClipboardSet(ok) => self.on_clipboard(ok),
        };
        self.finish(actions)
    }

    /// Draw the pages' area (below the strip of tabs, when it is there).
    fn paint_pages(&mut self, p: &mut dyn Painter) -> bool {
        let (full_w, full_h) = (clamp32(self.client_w.max(self.view_x + self.view_w)), clamp32(self.view_h));
        p.fill_rect(Rect { x: 0, y: 0, w: full_w, h: full_h }, self.page_background());
        if self.view_x > 0 {
            // The edge of the bookmarks, which can be dragged: a thin line in the middle of its strip.
            let line = clamp32(self.px(1).max(1));
            p.fill_rect(Rect { x: clamp32(self.view_x - self.px(SPLIT_GAP) / 2), y: 0, w: line, h: full_h }, self.pal.split);
        }
        self.draw_sidebar(p);
        let mut drew = false;
        match &self.doc {
            Some(doc) => {
                for page in doc.layout.visible(self.scroll_y, self.scroll_y + self.view_h) {
                    drew |= self.draw_page(p, page);
                }
            }
            None => {
                let s = t(self.lang, if self.opening { Msg::Opening } else { Msg::Hint });
                let w = p.text_width(s);
                p.text(clamp32(self.view_x) + (clamp32(self.view_w) - w) / 2, (clamp32(self.view_h) - p.line_height()) / 2, s, self.pal.hint_fg);
            }
        }
        self.draw_find_panel(p);
        let now = Instant::now();
        if drew && self.first_bitmap.is_none() {
            self.first_bitmap = Some(now);
        }
        self.sharp_on_screen = self.visible_ready();
        if self.sharp_on_screen {
            self.last_sharp = Some(now);
        }
        if self.bench.is_some() {
            // The speed test looks at what was drawn after this picture is on the screen.
            (self.wake)();
        }
        // The speed test stops its clock when the page is drawn; it does not put the pictures before that on the
        // screen (Windows sometimes holds a BitBlt to the screen for half a second, whatever is in it).
        self.bench.is_none() || self.sharp_on_screen
    }

}
