//! What the window and the reader say to each other, in plain types. The reader (`app.rs`) never calls Windows: it
//! is given [`Event`]s, answers with [`Action`]s and draws through a [`Painter`]; `win32.rs` is the other side of
//! all three. That keeps the reader testable without a window.

use std::path::PathBuf;

/// A rectangle in pixels of the window's client area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// A colour as 0xRRGGBB.
pub type Rgb = u32;

/// Drawing on the window (a back buffer that is put on the screen when the reader has done).
pub trait Painter {
    fn fill_rect(&mut self, r: Rect, color: Rgb);
    /// `fill_rect` with the colour laid over what is there: `alpha` from 0 (nothing) to 255 (the colour).
    fn fill_rect_alpha(&mut self, r: Rect, color: Rgb, alpha: u8);
    /// A BGRA picture of `src_w` by `src_h` pixels (rows from the top, no padding) put into `dest`, stretched if the
    /// sizes differ. A picture whose bytes are fewer than its size says is not drawn.
    fn draw_bgra(&mut self, dest: Rect, src_w: i32, src_h: i32, bgra: &[u8]);
    /// Text with its top left corner at (x, y), in the window's font.
    fn text(&mut self, x: i32, y: i32, s: &str, color: Rgb);
    fn text_width(&mut self, s: &str) -> i32;
    fn line_height(&self) -> i32;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bar {
    Vertical,
    Horizontal,
}

/// A scroll bar: positions from `min` to `max` (inclusive) with a thumb as long as `page`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScrollInfo {
    pub min: i32,
    pub max: i32,
    pub page: u32,
    pub pos: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollCode {
    LineUp,
    LineDown,
    PageUp,
    PageDown,
    Top,
    Bottom,
    /// The thumb is at this position.
    Thumb(i32),
}

/// Virtual-key codes the reader uses (winuser.h).
pub mod vk {
    pub const RETURN: u32 = 0x0D;
    pub const ESCAPE: u32 = 0x1B;
    pub const SPACE: u32 = 0x20;
    pub const DELETE: u32 = 0x2E;
    pub const PRIOR: u32 = 0x21;
    pub const NEXT: u32 = 0x22;
    pub const END: u32 = 0x23;
    pub const HOME: u32 = 0x24;
    pub const LEFT: u32 = 0x25;
    pub const UP: u32 = 0x26;
    pub const RIGHT: u32 = 0x27;
    pub const DOWN: u32 = 0x28;
    pub const NUM0: u32 = 0x30;
    pub const NUM1: u32 = 0x31;
    pub const NUM2: u32 = 0x32;
    pub const C: u32 = 0x43;
    pub const F: u32 = 0x46;
    pub const G: u32 = 0x47;
    pub const O: u32 = 0x4F;
    pub const P: u32 = 0x50;
    pub const R: u32 = 0x52;
    pub const S: u32 = 0x53;
    pub const Y: u32 = 0x59;
    pub const Z: u32 = 0x5A;
    pub const NUMPAD0: u32 = 0x60;
    pub const ADD: u32 = 0x6B;
    pub const SUBTRACT: u32 = 0x6D;
    pub const F3: u32 = 0x72;
    pub const F4: u32 = 0x73;
    pub const OEM_PLUS: u32 = 0xBB;
    pub const OEM_MINUS: u32 = 0xBD;
}

/// What happened, as the reader hears it. (No `Debug`: a [`Event::Text`] may be a password, which must not end up in
/// a log by accident.)
pub enum Event {
    /// The client area is this big now (pixels).
    Size { w: i32, h: i32 },
    /// The screen's dots per inch changed (and is told once at the start).
    Dpi(u32),
    Key { vk: u32, ctrl: bool, shift: bool },
    /// The wheel turned: `delta` is 120 for a click away from the user (up). `x`, `y` is the pointer.
    Wheel { delta: i32, ctrl: bool, shift: bool, x: i32, y: i32 },
    HWheel { delta: i32 },
    Scroll { bar: Bar, code: ScrollCode },
    /// A menu item (its id).
    Command(u32),
    /// Files dropped on the window.
    Drop(Vec<PathBuf>),
    /// The engine has something to say; look at it.
    Wake,
    /// A timer set with [`Action::SetTimer`].
    Timer(u32),
    /// The answer to [`Action::PickFile`]: the file chosen, or none.
    FilePicked(Option<PathBuf>),
    /// The answer to [`Action::AskText`]: what was typed, or none if it was cancelled.
    Text { tag: u32, text: Option<String> },
    /// The left button went down, moved (with it down) or came up, at this place in the client area.
    MouseDown { x: i32, y: i32, ctrl: bool, shift: bool },
    MouseMove { x: i32, y: i32 },
    MouseUp { x: i32, y: i32 },
    /// The window lost the mouse while the button was down (another window took it).
    MouseLost,
    /// The person asked to close the window (the close button, Alt+F4): it stays open unless the reader answers with
    /// [`Action::Quit`] (it may ask first whether to save).
    CloseRequested,
    /// The answer to [`Action::PickSavePath`]: the file to write, or none if it was cancelled.
    SavePicked(Option<PathBuf>),
    /// The answer to [`Action::AskSave`].
    SaveChoice { tag: u32, choice: SaveChoice },
    /// An item of the bookmarks was clicked (its number in the list given with [`Action::Outline`]).
    OutlineClick(usize),
    /// The text in the find box changed.
    FindText(String),
    /// The answer to [`Action::Confirm`].
    Confirmed { tag: u32, yes: bool },
    /// The answer to [`Action::ChoosePrinter`]: the printer chosen and what it can print, or none if it was cancelled or there is none.
    Printer(Option<PrinterSetup>),
    /// A band given with [`PrintOp::Band`] has been put on the page: the next may be asked for.
    PrintStepDone,
    /// The printer refused something (the text says what).
    PrintError(String),
    /// The answer to [`Action::SetClipboard`]: the text is on the clipboard (`true`), or it could not be put there.
    ClipboardSet(bool),
}

/// What a person answered to "save the changes?".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveChoice {
    Save,
    Discard,
    Cancel,
}

/// One bookmark for the tree beside the pages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutlineNode {
    pub title: String,
    /// 0 for the top level.
    pub depth: u32,
    /// Shown with its children open.
    pub open: bool,
}

/// What the chosen printer can print and what was asked for.
#[derive(Clone, Debug, PartialEq)]
pub struct PrinterSetup {
    pub name: String,
    /// Dots per inch, across.
    pub dpi: i32,
    /// The part of the paper that can be printed on, in printer pixels.
    pub width: i32,
    pub height: i32,
    /// The page ranges chosen (first and last, from 1); none is all pages.
    pub ranges: Vec<(u32, u32)>,
}

/// A printer to use without asking (the hidden `--print-test` of the program): its name, the file it writes (for a printer that
/// makes a file) and the pages.
#[derive(Clone, Debug, PartialEq)]
pub struct PrinterPreset {
    pub name: String,
    pub output: PathBuf,
    pub ranges: Vec<(u32, u32)>,
}

/// One step of a print job.
#[derive(Debug)]
pub enum PrintOp {
    /// Start the job (a document called `name`).
    Start { name: String },
    StartPage,
    /// Put a BGRA picture of `src_w` by `src_h` pixels at `x, y` on the page, stretched to `w` by `h` printer pixels.
    Band { x: i32, y: i32, w: i32, h: i32, src_w: i32, src_h: i32, bgra: Vec<u8> },
    EndPage,
    /// The job is finished.
    End,
    /// The job is stopped.
    Abort,
}

/// The shape of the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cursor {
    Arrow,
    /// Over a link.
    Hand,
    /// Drawing or putting a note.
    Cross,
    /// Marking text.
    Text,
}

/// What the reader asks the window to do (after it has finished with an event).
#[derive(Debug)]
pub enum Action {
    /// Draw the window again.
    Invalidate,
    SetTitle(String),
    SetScroll { bar: Bar, info: ScrollInfo },
    SetTimer { id: u32, ms: u32 },
    KillTimer(u32),
    /// Show the open-file box; the answer comes as [`Event::FilePicked`]. `filters`: (name, pattern) pairs.
    PickFile { title: String, filters: Vec<(String, String)> },
    /// Show the save-as box (asking before it replaces a file); the answer comes as [`Event::SavePicked`]. `name` is the file name it
    /// starts with.
    PickSavePath { title: String, filters: Vec<(String, String)>, name: String },
    /// Ask "save the changes?" with Yes, No and Cancel; the answer is [`Event::SaveChoice`] with the same `tag` (Cancel unless the
    /// person says otherwise).
    AskSave { tag: u32, title: String, text: String },
    /// Ask for a line of text in a box of its own; the answer comes as [`Event::Text`] with the same `tag`. A
    /// `secret` is shown as dots, and what is typed is wiped from the window's memory once it has been handed over.
    AskText { tag: u32, title: String, prompt: String, secret: bool, ok: String, cancel: String },
    Message { title: String, text: String },
    /// Ask a question that is answered yes or no (the answer is [`Event::Confirmed`] with the same `tag`); the answer is no
    /// unless the person says yes.
    Confirm { tag: u32, title: String, text: String },
    Quit,
    /// Put the menus again (what is checked or greyed, the files in the list of recent ones, have changed).
    SetMenus(Vec<Menu>),
    /// The bookmarks to show in the tree (an empty list clears it) ...
    Outline(Vec<OutlineNode>),
    /// ... and the place of the tree in the window (a width of 0 hides it).
    Sidebar(Rect),
    /// Show the find box at this place (and put the keyboard in it), or hide it.
    Find(Option<Rect>),
    /// Open an address with the system's browser or mail program. The reader has checked it.
    OpenUri(String),
    /// Put text on the clipboard; the answer is [`Event::ClipboardSet`].
    SetClipboard(String),
    /// Ask which printer, which pages and how many copies (with the system's box), or use `preset` without asking. The answer is
    /// [`Event::Printer`]. `max_page` is the number of pages of the document.
    ChoosePrinter { max_page: u32, preset: Option<PrinterPreset> },
    Print(PrintOp),
}

#[derive(Debug)]
pub enum MenuEntry {
    Item { id: u32, label: String, checked: bool, enabled: bool },
    Submenu { title: String, entries: Vec<MenuEntry> },
    Separator,
}

#[derive(Debug)]
pub struct Menu {
    pub title: String,
    pub entries: Vec<MenuEntry>,
}

/// The reader as the window sees it.
pub trait Handler {
    fn event(&mut self, event: Event) -> Vec<Action>;
    /// Draw the window. The answer says whether the picture is to be put on the screen: it always is, except in the
    /// speed test, which does not present the pictures that are not the one it waits for (see `bench.rs`).
    fn paint(&mut self, painter: &mut dyn Painter) -> bool;
    /// The shape of the pointer over this place of the client area.
    fn cursor_at(&mut self, x: i32, y: i32) -> Cursor;
    /// Why the system should not end the session (log off, restart, shut down) while this window has something unsaved, worded for
    /// the person; `None` when it may end. The window then holds the shutdown up and asks, as it does when it is closed.
    fn unsaved_reason(&self) -> Option<String> {
        None
    }
}
