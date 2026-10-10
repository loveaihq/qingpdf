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
    pub const SPACE: u32 = 0x20;
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
    pub const G: u32 = 0x47;
    pub const O: u32 = 0x4F;
    pub const R: u32 = 0x52;
    pub const NUMPAD0: u32 = 0x60;
    pub const ADD: u32 = 0x6B;
    pub const SUBTRACT: u32 = 0x6D;
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
    /// Ask for a line of text in a box of its own; the answer comes as [`Event::Text`] with the same `tag`. A
    /// `secret` is shown as dots, and what is typed is wiped from the window's memory once it has been handed over.
    AskText { tag: u32, title: String, prompt: String, secret: bool, ok: String, cancel: String },
    Message { title: String, text: String },
    Quit,
}

pub enum MenuEntry {
    Item { id: u32, label: String },
    Separator,
}

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
}
