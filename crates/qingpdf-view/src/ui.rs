//! What the window and the reader say to each other, in plain types. The reader (`app.rs`) never calls Windows: it
//! is given [`Event`]s, answers with [`Action`]s and draws through a [`Painter`]; `win32.rs` is the other side of
//! all three. That keeps the reader testable without a window.

use std::path::PathBuf;

use qingpdf_core::bookmarks::Place;

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

/// The fonts the reader draws text in: the usual one, the same in bold, a big bold one (the name on the welcome page), and the font of the pictures of the
/// buttons (Segoe Fluent Icons, or Segoe MDL2 Assets).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Font {
    Body,
    Bold,
    Title,
    Icon,
}

/// Drawing on the window (a back buffer that is put on the screen when the reader has done).
pub trait Painter {
    fn fill_rect(&mut self, r: Rect, color: Rgb);
    /// `fill_rect` with the colour laid over what is there: `alpha` from 0 (nothing) to 255 (the colour).
    fn fill_rect_alpha(&mut self, r: Rect, color: Rgb, alpha: u8);
    /// A BGRA picture of `src_w` by `src_h` pixels (rows from the top, no padding) put into `dest`, stretched if the
    /// sizes differ. A picture whose bytes are fewer than its size says is not drawn.
    fn draw_bgra(&mut self, dest: Rect, src_w: i32, src_h: i32, bgra: &[u8]);
    /// Text with its top left corner at (x, y), in the window's font (see [`Painter::set_font`]).
    fn text(&mut self, x: i32, y: i32, s: &str, color: Rgb);
    fn text_width(&mut self, s: &str) -> i32;
    fn line_height(&self) -> i32;
    /// The font that text is drawn and measured in from now on.
    fn set_font(&mut self, _font: Font) {}
    /// A shape with smooth edges, filled: the rectangle `r` with the radii of its corners (top left, top right, bottom right, bottom left; a radius is held to
    /// half the shorter side), the colour laid over what is there with `alpha`. Without smooth edges it is the rectangle.
    fn fill_shape(&mut self, r: Rect, _radii: [i32; 4], color: Rgb, alpha: u8) {
        self.fill_rect_alpha(r, color, alpha);
    }
    /// The outline of a rounded rectangle, `width` pixels thick and inside the rectangle's edge.
    fn stroke_round(&mut self, _r: Rect, _radius: i32, _width: i32, _color: Rgb, _alpha: u8) {}
    /// A line through these points (pixels), `width` thick, with round ends and joins.
    fn polyline(&mut self, _points: &[(f32, f32)], _width: f32, _color: Rgb, _alpha: u8) {}
    /// A letter of the icon font, in the middle of `r`.
    fn glyph(&mut self, r: Rect, ch: char, color: Rgb) {
        self.set_font(Font::Icon);
        let s = ch.to_string();
        let (w, h) = (self.text_width(&s), self.line_height());
        self.text(r.x + (r.w - w) / 2, r.y + (r.h - h) / 2, &s, color);
        self.set_font(Font::Body);
    }
}

/// `s` cut short with an ellipsis so that it is at most `room` pixels wide (it is kept whole when it fits).
pub fn fit_text(p: &mut dyn Painter, s: &str, room: i32) -> String {
    if p.text_width(s) <= room {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let cut: String = chars.iter().take(mid).chain(std::iter::once(&'\u{2026}')).collect();
        if p.text_width(&cut) <= room { lo = mid } else { hi = mid - 1 }
    }
    chars.iter().take(lo).chain(std::iter::once(&'\u{2026}')).collect()
}

/// `v` held between `lo` and `hi`, and never a panic: `Ord::clamp` panics when `lo` is more than `hi` (a card wider than the window it is to stand in), and
/// here `lo` wins then.
pub fn clamp_to<T: Ord + Copy>(v: T, lo: T, hi: T) -> T {
    v.max(lo).min(hi.max(lo))
}

/// `s` cut short in the middle with an ellipsis so that it is at most `room` pixels wide, keeping the start and the end (a path keeps its drive and its file
/// name); kept whole when it fits.
pub fn fit_middle(p: &mut dyn Painter, s: &str, room: i32) -> String {
    if p.text_width(s) <= room {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    // The most characters that fit with the ellipsis, shared between the two ends (the end gets the larger share).
    let (mut lo, mut hi) = (0usize, chars.len());
    let cut = |keep: usize| -> String {
        let (head, tail) = (keep / 3, keep - keep / 3);
        chars.iter().take(head).chain(std::iter::once(&'\u{2026}')).chain(chars.iter().skip(chars.len().saturating_sub(tail))).collect()
    };
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if p.text_width(&cut(mid)) <= room { lo = mid } else { hi = mid - 1 }
    }
    cut(lo)
}

/// Virtual-key codes the reader uses (winuser.h).
pub mod vk {
    pub const BACK: u32 = 0x08;
    pub const TAB: u32 = 0x09;
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
    pub const A: u32 = 0x41;
    pub const B: u32 = 0x42;
    pub const C: u32 = 0x43;
    pub const D: u32 = 0x44;
    pub const F: u32 = 0x46;
    pub const G: u32 = 0x47;
    pub const O: u32 = 0x4F;
    pub const P: u32 = 0x50;
    pub const R: u32 = 0x52;
    pub const S: u32 = 0x53;
    pub const Y: u32 = 0x59;
    pub const W: u32 = 0x57;
    pub const Z: u32 = 0x5A;
    pub const NUMPAD0: u32 = 0x60;
    pub const ADD: u32 = 0x6B;
    pub const SUBTRACT: u32 = 0x6D;
    pub const F2: u32 = 0x71;
    pub const F3: u32 = 0x72;
    pub const F4: u32 = 0x73;
    pub const F5: u32 = 0x74;
    pub const F11: u32 = 0x7A;
    pub const OEM_PLUS: u32 = 0xBB;
    pub const OEM_MINUS: u32 = 0xBD;
    /// `[` and `]` of a US keyboard.
    pub const OEM_4: u32 = 0xDB;
    pub const OEM_6: u32 = 0xDD;
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
    /// `clicks` says which click in a row this is (1, 2 or 3: a double click selects a word, a triple click a line).
    MouseDown { x: i32, y: i32, ctrl: bool, shift: bool, clicks: u8 },
    /// (`shift`: Shift is held, which keeps a shape being dragged out square, round or at a multiple of 45 degrees.)
    MouseMove { x: i32, y: i32, shift: bool },
    MouseUp { x: i32, y: i32 },
    /// The window lost the mouse while the button was down (another window took it).
    MouseLost,
    /// The pointer moved with no button down, or left the window (2026-10-17: the strip of tabs shows the path of the tab it is over).
    Hover { x: i32, y: i32 },
    HoverEnd,
    /// The middle button came up (it closes the tab under it).
    MiddleClick { x: i32, y: i32 },
    /// The person asked to close the window (the close button, Alt+F4): it stays open unless the reader answers with
    /// [`Action::Quit`] (it may ask first whether to save).
    CloseRequested,
    /// The answer to [`Action::PickSavePath`]: the file to write, or none if it was cancelled.
    SavePicked(Option<PathBuf>),
    /// The answer to [`Action::PickFolder`]: the folder chosen, or none if it was cancelled.
    FolderPicked(Option<PathBuf>),
    /// The answer to [`Action::AskSave`].
    SaveChoice { tag: u32, choice: SaveChoice },
    /// An item of the bookmarks was clicked (its number in the list given with [`Action::Outline`]).
    OutlineClick(usize),
    /// The title of an item was typed over in the tree (2026-10-20), an item was dragged to a place of another (`from` to `place` of `to`), an
    /// item was opened (`true`) or closed, and the right button was pressed (on an item, or on none) at this place of the pages' area.
    OutlineRename { index: usize, text: String },
    OutlineDrop { from: usize, to: usize, place: Place },
    OutlineExpand { index: usize, open: bool },
    OutlineMenu { index: Option<usize>, x: i32, y: i32 },
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
    /// The box of [`Action::FieldEdit`] is closed: what is in it, and how it was left.
    FieldDone { text: String, exit: FieldExit },
    /// The answer to [`Action::Choose`]: the number of the item chosen, or none if the list was dismissed.
    Chosen(Option<usize>),
    /// The right button was pressed (or the menu key) at this place of the page area: the reader may answer with an [`Action::ContextMenu`].
    ContextMenu { x: i32, y: i32 },
    /// The keyboard went into the search box of the command bar.
    FindFocus,
    /// The box over the page number or the zoom of the page pill ([`Action::PillEdit`]) is closed: what is in it when Enter closed it, none when it was left
    /// any other way (Esc, a click outside).
    PillEdit(Option<String>),
    /// The answer to [`Action::AskForm`]: what was filled in, or none if it was cancelled.
    Form { tag: u32, answer: Option<FormAnswer> },
    /// Windows is set to dark apps (`true`) or to light ones, as it was at the start or has become (the person changed it).
    SystemDark(bool),
    /// The colour Windows is set to as its accent (none if it could not be read), at the start and when it changes.
    SystemAccent(Option<Rgb>),
    /// Windows is set to a high-contrast theme (with its colours), or is not any more.
    HighContrast(Option<crate::theme::Contrast>),
    /// How the frame of the window is: `custom` when the reader draws the title strip (the system's buttons are drawn over its right end, `caption` pixels
    /// wide), not when the system's own title bar is above the window (a high-contrast theme).
    Frame { custom: bool, caption: i32 },
    /// Alt or F10 alone was pressed: the menus are asked for, to be used by the keyboard.
    MenuKey,
    /// Windows is set to show animations (`true`) or not, at the start and when that changes: the window moves things only when it is.
    Motion(bool),
}

/// One thing in a form box ([`Action::AskForm`]): what it is, what it says, and where it is in the box (`x, y, w, h` in units of a 96th of
/// an inch from the box's top left corner; the height of a `Combo` is that of its list when it is open).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormItem {
    pub kind: FormKind,
    pub text: String,
    pub rect: (i32, i32, i32, i32),
    /// The entries of a `Combo` or a `Pick`.
    pub choices: Vec<String>,
    /// The entry a `Pick` starts with; for a `Check`, 1 when it starts ticked.
    pub selected: usize,
}

impl FormItem {
    /// An item with nothing to pick from.
    #[inline(never)]
    pub fn new(kind: FormKind, text: &str, rect: (i32, i32, i32, i32)) -> FormItem {
        FormItem { kind, text: text.to_string(), rect, choices: Vec::new(), selected: 0 }
    }

    /// A list of choices with the one at `selected` chosen.
    #[inline(never)]
    pub fn pick(choices: Vec<String>, selected: usize, rect: (i32, i32, i32, i32)) -> FormItem {
        FormItem { kind: FormKind::Pick, text: String::new(), rect, choices, selected }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormKind {
    /// Words (`text`).
    Label,
    /// A frame round some of the others, with `text` for its title.
    Group,
    /// A line of text that starts as `text`.
    Edit,
    /// A line of text typed as dots, empty to start with: a password. The value is wiped from the window's memory once it is handed over.
    Secret,
    /// A line of text that starts as `text`, with a list of `choices` to pick one from (what is picked replaces the line).
    Combo,
    /// A list of `choices` to pick one from.
    Pick,
    /// A tick box with `text` beside it.
    Check,
    /// A button with `text` on it that closes the box ([`FormAnswer::button`] says which).
    Button,
}

/// A box with several things to fill in, in the place and the size the reader says; OK and Cancel are at its foot, right.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormSpec {
    pub tag: u32,
    pub title: String,
    /// The size of what is inside the box, in units of a 96th of an inch; the last 44 units are the foot with the buttons.
    pub size: (i32, i32),
    pub items: Vec<FormItem>,
    pub ok: String,
    pub cancel: String,
    /// A sketch of a page that shows what is typed, as it is typed.
    pub preview: Option<FormPreview>,
}

/// A white page in a form box with the text of six places on it: the text of the six `Combo` or `Edit` items it is given, with the fields
/// filled in (`{page}`, `{pages}`, `{date}`, `{file}` stand for `fields`, in that order), at the left, middle and right of the head and of
/// the foot. When the tick box `mirror` is ticked and the page is a `left_page`, the left and right places change sides.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormPreview {
    /// Where the sketch is (units of a 96th of an inch, like the items'): the page is as big as fits in it.
    pub rect: (i32, i32, i32, i32),
    /// The shape of the page: its width and its height, in any unit.
    pub shape: (i32, i32),
    /// The numbers (in `FormSpec::items`) of the six places' items, head left first, and of the tick box.
    pub slots: [usize; 6],
    pub mirror: usize,
    pub fields: [String; 4],
    pub left_page: bool,
}

/// What was filled in: the value of each `Edit` and `Combo` (its text), `Pick` (the number of the entry) and `Check` (`1` or `0`), in
/// the order of the items; and which `Button` closed the box (counting the buttons only), or none when it was OK.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormAnswer {
    pub values: Vec<String>,
    pub button: Option<usize>,
}

/// How the person left the box a form field or a text box is typed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldExit {
    /// Enter (single line) or Ctrl+Enter.
    Commit,
    /// Tab and Shift+Tab: what is typed is kept and the next (or the previous) field is next.
    Next,
    Prev,
    /// Esc: what is typed is thrown away.
    Cancel,
    /// The keyboard went elsewhere (a click outside): what is typed is kept.
    Blur,
}

/// The box a form field or a text box is typed in, over the page.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldBox {
    pub rect: Rect,
    pub text: String,
    pub multiline: bool,
    /// Most characters (0: no limit beyond the engine's).
    pub max_len: u32,
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
    /// Over the edge of the bookmarks, which can be dragged to make them wider or narrower.
    SizeWE,
    /// Over an annotation that is picked (it is moved by dragging), and over its corners (it is resized by dragging them).
    Move,
    SizeNWSE,
    SizeNESW,
    /// No pointer at all (the presentation hides it when it has been still for a while).
    Hidden,
}

/// What the reader asks the window to do (after it has finished with an event).
#[derive(Debug)]
pub enum Action {
    /// Draw the window again.
    Invalidate,
    /// Draw this part of the window again (in the window's coordinates); the rest is left as it is. What fades or grows asks for the room it is in.
    InvalidateRect(Rect),
    SetTitle(String),
    SetTimer { id: u32, ms: u32 },
    KillTimer(u32),
    /// Show the open-file box; the answer comes as [`Event::FilePicked`]. `filters`: (name, pattern) pairs.
    PickFile { title: String, filters: Vec<(String, String)> },
    /// Show the save-as box (asking before it replaces a file); the answer comes as [`Event::SavePicked`]. `name` is the file name it
    /// starts with.
    PickSavePath { title: String, filters: Vec<(String, String)>, name: String },
    /// Show the box that picks a folder; the answer comes as [`Event::FolderPicked`].
    PickFolder { title: String },
    /// Ask "save the changes?" with Yes, No and Cancel; the answer is [`Event::SaveChoice`] with the same `tag` (Cancel unless the
    /// person says otherwise).
    AskSave { tag: u32, title: String, text: String },
    /// Ask for a line of text in a box of its own; the answer comes as [`Event::Text`] with the same `tag`. A
    /// `secret` is shown as dots, and what is typed is wiped from the window's memory once it has been handed over. The line starts as `initial`
    /// (selected, so that typing replaces it).
    AskText { tag: u32, title: String, prompt: String, secret: bool, initial: String, ok: String, cancel: String },
    /// Ask for several things in a box of its own; the answer comes as [`Event::Form`] with the same tag.
    AskForm(FormSpec),
    Message { title: String, text: String },
    /// Ask a question that is answered yes or no (the answer is [`Event::Confirmed`] with the same `tag`); the answer is no
    /// unless the person says yes.
    Confirm { tag: u32, title: String, text: String },
    Quit,
    /// Put the menus again (what is checked or greyed, the files in the list of recent ones, have changed).
    SetMenus(Vec<Menu>),
    /// The bookmarks to show in the tree (an empty list clears it), and the item to select and bring into view ...
    Outline { nodes: Vec<OutlineNode>, select: Option<usize> },
    /// ... the title of this item is typed over in the tree, where it is ...
    OutlineRename(usize),
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
    /// Show a box to type in over the page (or take it away): the text starts as given and is selected; the answer is
    /// [`Event::FieldDone`] when the person leaves it.
    FieldEdit(Option<FieldBox>),
    /// The box of [`Action::FieldEdit`] moves to this place (the page was scrolled or zoomed), keeping what was typed.
    FieldMove(Rect),
    /// Show a list of choices at this place of the client area; the answer is [`Event::Chosen`].
    Choose { items: Vec<String>, selected: Option<usize>, at: (i32, i32) },
    /// Show a menu at this place of the page area; the item chosen comes as [`Event::Command`].
    ContextMenu { entries: Vec<MenuEntry>, at: (i32, i32) },
    /// The box the words to find are typed in, over the search pill of the command bar (none when the command bar is not shown); the pill itself is drawn by the
    /// reader.
    SearchBox(Option<SearchBox>),
    /// A box to type the page or the zoom in, over the page pill, with this text selected (or take it away); the answer is [`Event::PillEdit`].
    PillEdit(Option<(Rect, String)>),
    /// Show the menus as a pop-up menu: the menus of the window's top level are its items, each opening its own. `at` is where its top right corner goes;
    /// with `keyboard` the first item is selected, as it is when it was opened by Alt or F10.
    ShowMenu { at: (i32, i32), keyboard: bool },
    /// How the window looks (View > Interface): the parts that are Windows' own take the colours.
    Theme(ThemeSpec),
    /// Make the window full screen, on the monitor it is on, with no frame or title strip; or give it back its place exactly as it was.
    FullScreen(bool),
    /// Hide the pointer until it moves.
    HideCursor,
}

/// How the window looks to Windows' own parts of it (the frame, the boxes typed in, the tree of bookmarks) and to the text it draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemeSpec {
    pub dark: bool,
    /// The words are Chinese (the fonts are chosen by the language).
    pub zh: bool,
    /// The colour of the title strip: where the window is that colour, it shows what is behind it instead (Mica), when Windows can do that.
    pub strip: Rgb,
    /// The words in the boxes and in the tree, the boxes the person types in, the tree of bookmarks with the lines in it.
    pub text: Rgb,
    pub field_bg: Rgb,
    pub tree_bg: Rgb,
    pub tree_line: Rgb,
    /// The colours of the menus and of the boxes the reader opens, which it draws itself.
    pub skin: Skin,
}

/// The colours that menus and boxes are drawn in: the ground, the ink and its paler partners (for a key, for what is off), the hairline, the ground of a box
/// that is typed in, the accent with its four partners, and the ink laid over the ground to show the pointer on a button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Skin {
    pub surface: Rgb,
    pub text: Rgb,
    pub dim: Rgb,
    pub off: Rgb,
    pub line: Rgb,
    pub field: Rgb,
    pub accent: Rgb,
    pub accent_hover: Rgb,
    pub tint: Rgb,
    pub on_tint: Rgb,
    pub on_accent: Rgb,
    pub hover: Rgb,
}

/// The search box of the command bar: where it is, what it says while it is empty, and whether it can be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchBox {
    pub rect: Rect,
    pub hint: String,
    pub enabled: bool,
}

#[derive(Debug)]
pub enum MenuEntry {
    /// An item; `radio`: it is one of a list of which one is chosen (shown with a dot, not a tick).
    Item { id: u32, label: String, checked: bool, radio: bool, enabled: bool },
    Submenu { title: String, entries: Vec<MenuEntry> },
    Separator,
}

#[derive(Debug)]
pub struct Menu {
    pub title: String,
    pub entries: Vec<MenuEntry>,
}

/// What a place of the window is to its frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NcHit {
    Client,
    Caption,
}

/// The reader as the window sees it.
pub trait Handler {
    fn event(&mut self, event: Event) -> Vec<Action>;
    /// Draw the window. The answer says whether the picture is to be put on the screen: it always is, except in the
    /// speed test, which does not present the pictures that are not the one it waits for (see `bench.rs`).
    fn paint(&mut self, painter: &mut dyn Painter) -> bool;
    /// Draw only the part of the window whose top left corner is at (`x`, `y`): `painter` is a picture of that part alone (its corner is its origin). Gives
    /// what `paint` gives.
    fn paint_part(&mut self, painter: &mut dyn Painter, x: i32, y: i32) -> bool {
        let _ = (painter, x, y);
        true
    }
    /// The shape of the pointer over this place of the client area.
    fn cursor_at(&mut self, x: i32, y: i32) -> Cursor;
    /// What this place of the client area is to the frame: part of the title bar (the window is dragged there, and a double click makes it big or small) or
    /// the reader's own.
    fn nc_hit(&mut self, _x: i32, _y: i32) -> NcHit {
        NcHit::Client
    }
    /// Why the system should not end the session (log off, restart, shut down) while this window has something unsaved, worded for
    /// the person; `None` when it may end. The window then holds the shutdown up and asks, as it does when it is closed.
    fn unsaved_reason(&self) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Ruler;

    impl Painter for Ruler {
        fn fill_rect(&mut self, _r: Rect, _c: Rgb) {}
        fn fill_rect_alpha(&mut self, _r: Rect, _c: Rgb, _a: u8) {}
        fn draw_bgra(&mut self, _d: Rect, _w: i32, _h: i32, _b: &[u8]) {}
        fn text(&mut self, _x: i32, _y: i32, _s: &str, _c: Rgb) {}
        fn text_width(&mut self, s: &str) -> i32 {
            7 * s.chars().count() as i32
        }
        fn line_height(&self) -> i32 {
            16
        }
    }

    #[test]
    fn a_clamp_that_has_no_room_gives_the_low_end_and_never_panics() {
        assert_eq!(clamp_to(50, 4, 100), 50);
        assert_eq!(clamp_to(-5, 4, 100), 4);
        assert_eq!(clamp_to(500, 4, 100), 100);
        // The window is narrower than the card: the card stands at the low end.
        assert_eq!(clamp_to(50, 4, -20), 4);
        assert_eq!(clamp_to(50, 4, 3), 4);
        assert_eq!(clamp_to(7i64, 7, 7), 7);
    }

    #[test]
    fn words_are_cut_with_an_ellipsis_at_the_end_or_in_the_middle_and_kept_whole_when_they_fit() {
        let mut p = Ruler;
        assert_eq!(fit_text(&mut p, "short", 100), "short");
        assert_eq!(fit_middle(&mut p, "short", 100), "short");
        let long = "C:\\one\\two\\three\\four\\five\\six\\seven\\file.pdf";
        let end = fit_text(&mut p, long, 140);
        assert!(end.ends_with('\u{2026}') && p.text_width(&end) <= 140, "{end}");
        let mid = fit_middle(&mut p, long, 140);
        assert!(p.text_width(&mid) <= 140 && mid.contains('\u{2026}') && mid.ends_with("file.pdf") && mid.starts_with("C:"), "{mid}");
        // No room at all: nothing that is too wide comes back.
        let (none_mid, none_end) = (fit_middle(&mut p, long, 0), fit_text(&mut p, long, 0));
        assert!(p.text_width(&none_mid) <= 7 && p.text_width(&none_end) <= 7, "{none_mid} {none_end}");
    }
}
