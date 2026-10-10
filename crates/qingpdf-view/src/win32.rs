//! Everything that calls Windows. This is the only module of the viewer that may use `unsafe`; every block says why
//! it is sound. The declarations are written here by hand (no `windows-sys`), only the `W` (Unicode) functions are
//! used, and nothing outside this module sees a handle or a pointer: the reader gets [`Event`]s and answers with
//! [`Action`]s (`ui.rs`).
//!
//! What the unsafe code relies on, in general:
//! - every pointer passed to Windows points to a live value of the type Windows expects, and Windows does not keep
//!   it after the call (except the window procedure and the user-data pointer, which are described where used);
//! - handles (`isize`) are only used with the functions that take that kind of handle, and a handle that fails to
//!   be made is checked before it is used;
//! - the window state is created by `run`, reached only from the window's thread, and freed once, by `run`, after the message
//!   loop has ended (not at `WM_NCDESTROY`: that can arrive while a box opened by a handler is still on the stack).

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::{OsString, c_void};
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::ui::{Action, Bar, Event, Handler, Menu, MenuEntry, Painter, Rect, Rgb, ScrollCode, ScrollInfo};

// --- declarations ------------------------------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Point {
    x: i32,
    y: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct WinRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
struct Size {
    cx: i32,
    cy: i32,
}

#[repr(C)]
struct Msg {
    hwnd: isize,
    message: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
    pt: Point,
    private: u32,
}

type WndProc = unsafe extern "system" fn(isize, u32, usize, isize) -> isize;

#[repr(C)]
struct WndClassExW {
    size: u32,
    style: u32,
    wnd_proc: Option<WndProc>,
    cls_extra: i32,
    wnd_extra: i32,
    instance: isize,
    icon: isize,
    cursor: isize,
    background: isize,
    menu_name: *const u16,
    class_name: *const u16,
    icon_small: isize,
}

#[repr(C)]
struct PaintStruct {
    hdc: isize,
    erase: i32,
    paint: WinRect,
    restore: i32,
    inc_update: i32,
    reserved: [u8; 32],
}

#[repr(C)]
struct ScrollInfoW {
    size: u32,
    mask: u32,
    min: i32,
    max: i32,
    page: u32,
    pos: i32,
    track_pos: i32,
}

#[repr(C)]
struct BitmapInfoHeader {
    size: u32,
    width: i32,
    height: i32,
    planes: u16,
    bit_count: u16,
    compression: u32,
    size_image: u32,
    x_pels: i32,
    y_pels: i32,
    clr_used: u32,
    clr_important: u32,
}

#[repr(C)]
struct OpenFileNameW {
    struct_size: u32,
    owner: isize,
    instance: isize,
    filter: *const u16,
    custom_filter: *mut u16,
    max_custom_filter: u32,
    filter_index: u32,
    file: *mut u16,
    max_file: u32,
    file_title: *mut u16,
    max_file_title: u32,
    initial_dir: *const u16,
    title: *const u16,
    flags: u32,
    file_offset: u16,
    file_extension: u16,
    def_ext: *const u16,
    cust_data: isize,
    hook: *const c_void,
    template_name: *const u16,
    reserved: *mut c_void,
    reserved_dw: u32,
    flags_ex: u32,
}

#[repr(C)]
struct MinMaxInfo {
    reserved: Point,
    max_size: Point,
    max_position: Point,
    min_track: Point,
    max_track: Point,
}

#[repr(C)]
struct InitCommonControlsEx {
    size: u32,
    icc: u32,
}

#[repr(C)]
struct ProcessMemoryCounters {
    cb: u32,
    page_fault_count: u32,
    peak_working_set: usize,
    working_set: usize,
    quota_peak_paged: usize,
    quota_paged: usize,
    quota_peak_non_paged: usize,
    quota_non_paged: usize,
    pagefile: usize,
    peak_pagefile: usize,
}

#[link(name = "user32")]
unsafe extern "system" {
    fn RegisterClassExW(class: *const WndClassExW) -> u16;
    fn CreateWindowExW(ex_style: u32, class: *const u16, name: *const u16, style: u32, x: i32, y: i32, w: i32, h: i32, parent: isize, menu: isize, instance: isize, param: *mut c_void) -> isize;
    fn DefWindowProcW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn GetMessageW(msg: *mut Msg, hwnd: isize, min: u32, max: u32) -> i32;
    fn TranslateMessage(msg: *const Msg) -> i32;
    fn DispatchMessageW(msg: *const Msg) -> isize;
    fn PostQuitMessage(code: i32);
    fn PostMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> i32;
    fn SendMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
    fn UpdateWindow(hwnd: isize) -> i32;
    fn DestroyWindow(hwnd: isize) -> i32;
    fn InvalidateRect(hwnd: isize, rect: *const WinRect, erase: i32) -> i32;
    fn BeginPaint(hwnd: isize, ps: *mut PaintStruct) -> isize;
    fn EndPaint(hwnd: isize, ps: *const PaintStruct) -> i32;
    fn GetClientRect(hwnd: isize, rect: *mut WinRect) -> i32;
    fn GetWindowRect(hwnd: isize, rect: *mut WinRect) -> i32;
    fn SetWindowTextW(hwnd: isize, text: *const u16) -> i32;
    fn GetWindowTextLengthW(hwnd: isize) -> i32;
    fn GetWindowTextW(hwnd: isize, text: *mut u16, max: i32) -> i32;
    fn SetScrollInfo(hwnd: isize, bar: i32, info: *const ScrollInfoW, redraw: i32) -> i32;
    fn GetScrollInfo(hwnd: isize, bar: i32, info: *mut ScrollInfoW) -> i32;
    fn SetTimer(hwnd: isize, id: usize, ms: u32, proc_: *const c_void) -> usize;
    fn KillTimer(hwnd: isize, id: usize) -> i32;
    fn LoadCursorW(instance: isize, name: *const u16) -> isize;
    fn GetKeyState(key: i32) -> i16;
    fn MessageBoxW(hwnd: isize, text: *const u16, caption: *const u16, kind: u32) -> i32;
    fn EnableWindow(hwnd: isize, enable: i32) -> i32;
    fn SetWindowLongPtrW(hwnd: isize, index: i32, value: isize) -> isize;
    fn GetWindowLongPtrW(hwnd: isize, index: i32) -> isize;
    fn CreateMenu() -> isize;
    fn CreatePopupMenu() -> isize;
    fn AppendMenuW(menu: isize, flags: u32, id: usize, text: *const u16) -> i32;
    fn SetMenu(hwnd: isize, menu: isize) -> i32;
    fn GetDpiForWindow(hwnd: isize) -> u32;
    fn GetDpiForSystem() -> u32;
    fn AdjustWindowRectExForDpi(rect: *mut WinRect, style: u32, menu: i32, ex_style: u32, dpi: u32) -> i32;
    fn SetWindowPos(hwnd: isize, after: isize, x: i32, y: i32, w: i32, h: i32, flags: u32) -> i32;
    fn SetFocus(hwnd: isize) -> isize;
    fn SetForegroundWindow(hwnd: isize) -> i32;
    fn IsDialogMessageW(dialog: isize, msg: *mut Msg) -> i32;
    fn FillRect(hdc: isize, rect: *const WinRect, brush: isize) -> i32;
    fn ScreenToClient(hwnd: isize, point: *mut Point) -> i32;
    fn SystemParametersInfoW(action: u32, param: u32, data: *mut c_void, flags: u32) -> i32;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn CreateCompatibleDC(hdc: isize) -> isize;
    fn CreateCompatibleBitmap(hdc: isize, w: i32, h: i32) -> isize;
    fn SelectObject(hdc: isize, object: isize) -> isize;
    fn DeleteObject(object: isize) -> i32;
    fn DeleteDC(hdc: isize) -> i32;
    fn BitBlt(dest: isize, x: i32, y: i32, w: i32, h: i32, src: isize, sx: i32, sy: i32, rop: u32) -> i32;
    fn StretchDIBits(hdc: isize, xd: i32, yd: i32, wd: i32, hd: i32, xs: i32, ys: i32, ws: i32, hs: i32, bits: *const c_void, info: *const BitmapInfoHeader, usage: u32, rop: u32) -> i32;
    fn SetStretchBltMode(hdc: isize, mode: i32) -> i32;
    #[allow(clippy::too_many_arguments)]
    fn CreateFontW(height: i32, width: i32, escapement: i32, orientation: i32, weight: i32, italic: u32, underline: u32, strike: u32, charset: u32, out_precision: u32, clip_precision: u32, quality: u32, pitch_family: u32, face: *const u16) -> isize;
    fn SetBkMode(hdc: isize, mode: i32) -> i32;
    fn SetTextColor(hdc: isize, color: u32) -> u32;
    fn TextOutW(hdc: isize, x: i32, y: i32, text: *const u16, len: i32) -> i32;
    fn GetTextExtentPoint32W(hdc: isize, text: *const u16, len: i32, size: *mut Size) -> i32;
    fn CreateSolidBrush(color: u32) -> isize;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> isize;
    fn AttachConsole(pid: u32) -> i32;
    fn GetStdHandle(which: u32) -> isize;
    fn SetStdHandle(which: u32, handle: isize) -> i32;
    fn CreateFileW(name: *const u16, access: u32, share: u32, security: *const c_void, disposition: u32, flags: u32, template: isize) -> isize;
    fn GetCurrentProcess() -> isize;
    fn K32GetProcessMemoryInfo(process: isize, counters: *mut ProcessMemoryCounters, cb: u32) -> i32;
    fn GetUserDefaultUILanguage() -> u16;
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn DragAcceptFiles(hwnd: isize, accept: i32);
    fn DragQueryFileW(drop: isize, index: u32, buffer: *mut u16, len: u32) -> u32;
    fn DragFinish(drop: isize);
}

#[link(name = "comdlg32")]
unsafe extern "system" {
    fn GetOpenFileNameW(ofn: *mut OpenFileNameW) -> i32;
}

#[link(name = "comctl32")]
unsafe extern "system" {
    fn InitCommonControlsEx(init: *const InitCommonControlsEx) -> i32;
}

// --- constants ---------------------------------------------------------------------------------------------------

const WM_SIZE: u32 = 0x0005;
const WM_PAINT: u32 = 0x000F;
const WM_CLOSE: u32 = 0x0010;
const WM_ERASEBKGND: u32 = 0x0014;
const WM_SETFONT: u32 = 0x0030;
const WM_GETMINMAXINFO: u32 = 0x0024;
const WM_NCDESTROY: u32 = 0x0082;
const WM_KEYDOWN: u32 = 0x0100;
const WM_COMMAND: u32 = 0x0111;
const WM_TIMER: u32 = 0x0113;
const WM_HSCROLL: u32 = 0x0114;
const WM_VSCROLL: u32 = 0x0115;
const WM_MOUSEWHEEL: u32 = 0x020A;
const WM_MOUSEHWHEEL: u32 = 0x020E;
const WM_DPICHANGED: u32 = 0x02E0;
const WM_DROPFILES: u32 = 0x0233;
const WM_DESTROY: u32 = 0x0002;
/// The engine thread posts this to say it has something to hand over.
const WM_WAKE: u32 = 0x8001;

const WS_OVERLAPPEDWINDOW: u32 = 0x00CF_0000;
const WS_POPUP: u32 = 0x8000_0000;
const WS_CAPTION: u32 = 0x00C0_0000;
const WS_SYSMENU: u32 = 0x0008_0000;
const WS_CHILD: u32 = 0x4000_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const WS_TABSTOP: u32 = 0x0001_0000;
const WS_VSCROLL: u32 = 0x0020_0000;
const WS_HSCROLL: u32 = 0x0010_0000;
const WS_EX_ACCEPTFILES: u32 = 0x0000_0010;
const WS_EX_DLGMODALFRAME: u32 = 0x0000_0001;
const WS_EX_CLIENTEDGE: u32 = 0x0000_0200;
const CW_USEDEFAULT: i32 = i32::MIN;
const SW_SHOWNORMAL: i32 = 1;
const SW_SHOW: i32 = 5;
const SB_HORZ: i32 = 0;
const SB_VERT: i32 = 1;
const SIF_RANGE: u32 = 0x1;
const SIF_PAGE: u32 = 0x2;
const SIF_POS: u32 = 0x4;
const SIF_DISABLENOSCROLL: u32 = 0x8;
const SIF_TRACKPOS: u32 = 0x10;
const SRCCOPY: u32 = 0x00CC_0020;
const COLORONCOLOR: i32 = 3;
const TRANSPARENT: i32 = 1;
const GWLP_USERDATA: i32 = -21;
const MF_STRING: u32 = 0;
const MF_SEPARATOR: u32 = 0x800;
const MF_POPUP: u32 = 0x10;
const MB_OK: u32 = 0;
const MB_ICONINFORMATION: u32 = 0x40;
const MB_ICONERROR: u32 = 0x10;
const MB_TASKMODAL: u32 = 0x2000;
const ES_AUTOHSCROLL: u32 = 0x80;
const ES_PASSWORD: u32 = 0x20;
const BS_PUSHBUTTON: u32 = 0;
const BS_DEFPUSHBUTTON: u32 = 1;
const IDOK: usize = 1;
const IDCANCEL: usize = 2;
const SWP_NOZOMORDER: u32 = 0x4;
const SWP_NOACTIVATE: u32 = 0x10;
const SWP_NOMOVE: u32 = 0x2;
const OFN_PATHMUSTEXIST: u32 = 0x800;
const OFN_FILEMUSTEXIST: u32 = 0x1000;
const OFN_NOCHANGEDIR: u32 = 0x8;
const OFN_EXPLORER: u32 = 0x8_0000;
const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
const STD_ERROR_HANDLE: u32 = -12i32 as u32;
const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
const INVALID_HANDLE_VALUE: isize = -1;
const IDC_ARROW: usize = 32512;
const ICC_STANDARD_CLASSES: u32 = 0x4000;
const SPI_GETWORKAREA: u32 = 0x0030;
const DEFAULT_WIDTH: i32 = 1000;
const DEFAULT_HEIGHT: i32 = 800;

/// A panic is being reported: the window answers nothing any more (its state may be half way through a change).
static PANICKED: AtomicBool = AtomicBool::new(false);
/// The speed test asks for a window of a given size, which may be bigger than the screen: let Windows allow it.
static ALLOW_BIG_WINDOW: AtomicBool = AtomicBool::new(false);

// --- small safe wrappers -----------------------------------------------------------------------------------------

/// A string as UTF-16 with the closing zero.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn colorref(c: Rgb) -> u32 {
    ((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF)
}

fn loword(v: usize) -> u32 {
    (v & 0xFFFF) as u32
}

fn hiword(v: usize) -> u32 {
    ((v >> 16) & 0xFFFF) as u32
}

/// Wakes the window's loop from any thread (the engine's thread uses it).
#[derive(Clone, Copy)]
pub struct Waker(isize);

impl Waker {
    pub fn wake(self) {
        // SAFETY: PostMessageW may be called from any thread with any window handle; for a window that is gone it
        // fails and does nothing. The message carries no pointers.
        unsafe {
            PostMessageW(self.0, WM_WAKE, 0, 0);
        }
    }
}

/// The Windows language id of the user interface.
pub fn ui_language() -> u16 {
    // SAFETY: takes no arguments and only reads a system setting.
    unsafe { GetUserDefaultUILanguage() }
}

/// The most memory this process has had in use, in bytes (0 if Windows will not say).
pub fn peak_working_set() -> u64 {
    let mut c = ProcessMemoryCounters {
        cb: std::mem::size_of::<ProcessMemoryCounters>() as u32,
        page_fault_count: 0,
        peak_working_set: 0,
        working_set: 0,
        quota_peak_paged: 0,
        quota_paged: 0,
        quota_peak_non_paged: 0,
        quota_non_paged: 0,
        pagefile: 0,
        peak_pagefile: 0,
    };
    // SAFETY: GetCurrentProcess returns a pseudo handle that needs no closing; `c` is a live struct of the size
    // given in `cb`, which Windows fills in and does not keep.
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
    if ok == 0 { 0 } else { c.peak_working_set as u64 }
}

/// For the speed test, which prints its numbers: a program of the windows subsystem has no console, so use the one
/// of the program that started it (if any) unless the output goes to a file or a pipe already.
pub fn attach_console() {
    // SAFETY: GetStdHandle only reads the process's standard handle.
    let out = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    if out != 0 && out != INVALID_HANDLE_VALUE {
        return;
    }
    // SAFETY: AttachConsole takes a process id (here "the parent") and changes only the console of this process.
    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
        return;
    }
    let name = wide("CONOUT$");
    // SAFETY: `name` is a zero-terminated UTF-16 string that outlives the call; the null pointer is "no security
    // attributes"; the handle returned is checked before it is used, and kept open for the life of the process.
    let console = unsafe { CreateFileW(name.as_ptr(), 0xC000_0000, 3, null(), 3, 0, 0) };
    if console != 0 && console != INVALID_HANDLE_VALUE {
        // SAFETY: `console` is a valid open handle (checked above) that is never closed, so it stays valid.
        unsafe {
            SetStdHandle(STD_OUTPUT_HANDLE, console);
            SetStdHandle(STD_ERROR_HANDLE, console);
        }
    }
}

fn client_size(hwnd: isize) -> (i32, i32) {
    let mut r = WinRect::default();
    // SAFETY: `r` is a live RECT that Windows fills in and does not keep; a bad handle makes the call fail and
    // leaves `r` zero.
    unsafe { GetClientRect(hwnd, &mut r) };
    (r.right - r.left, r.bottom - r.top)
}

fn set_text(hwnd: isize, text: &str) {
    let w = wide(text);
    // SAFETY: `w` is a zero-terminated UTF-16 string that outlives the call; Windows copies it.
    unsafe { SetWindowTextW(hwnd, w.as_ptr()) };
}

fn invalidate(hwnd: isize) {
    // SAFETY: a null rectangle means the whole client area; nothing is kept by Windows.
    unsafe { InvalidateRect(hwnd, null(), 0) };
}

fn make_font(dpi: u32) -> isize {
    let face = wide("Segoe UI");
    let height = -((9 * dpi as i32 + 36) / 72);
    // SAFETY: all arguments are plain numbers except `face`, a zero-terminated UTF-16 string that outlives the
    // call (Windows copies it). The font returned is owned by the caller; 0 means it failed.
    unsafe { CreateFontW(height, 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 5, 0, face.as_ptr()) }
}

// --- the window --------------------------------------------------------------------------------------------------

/// The back buffer: the picture is drawn here and put on the window in one go.
#[derive(Default)]
struct Back {
    dc: isize,
    bitmap: isize,
    w: i32,
    h: i32,
}

struct WindowState {
    hwnd: Cell<isize>,
    /// The window is being destroyed (set at WM_DESTROY): the actions still queued are dropped and the reader hears no more.
    closing: Cell<bool>,
    handler: RefCell<Option<Box<dyn Handler>>>,
    back: RefCell<Back>,
    font: Cell<isize>,
    dpi: Cell<u32>,
}

fn state_of<'a>(hwnd: isize) -> Option<&'a WindowState> {
    // SAFETY: the user-data slot holds either 0 or the pointer made by `Box::into_raw` in `run`, which is freed
    // by `run` after the message loop has ended (the slot is set to 0 at WM_NCDESTROY, so nothing finds it after that).
    // Everything runs on the window's one thread, so no
    // other reference can be used while this one is. The reference is used within one message only.
    unsafe {
        let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const WindowState;
        p.as_ref()
    }
}

/// Give the reader an event; what it asks for is done once it has finished (so that a box it asks for, which
/// handles messages of its own, can call the reader again).
fn dispatch(state: &WindowState, event: Event) {
    let actions = deliver(state, event);
    execute(state, actions);
}

fn deliver(state: &WindowState, event: Event) -> Vec<Action> {
    if state.closing.get() {
        return Vec::new();
    }
    match state.handler.try_borrow_mut() {
        Ok(mut guard) => match guard.as_mut() {
            Some(h) => h.event(event),
            None => Vec::new(),
        },
        // The reader is busy (a message arrived while it was handling one): the event is dropped, not queued.
        Err(_) => Vec::new(),
    }
}

fn execute(state: &WindowState, actions: Vec<Action>) {
    let mut queue: VecDeque<Action> = actions.into();
    while let Some(action) = queue.pop_front() {
        // A box that was up while the window was closed (the task bar, Alt+F4 on the owner) leaves the window gone: what is
        // still queued is not carried out.
        if state.closing.get() {
            break;
        }
        let hwnd = state.hwnd.get();
        match action {
            Action::Invalidate => invalidate(hwnd),
            Action::SetTitle(t) => set_text(hwnd, &t),
            Action::SetScroll { bar, info } => set_scroll(hwnd, bar, info),
            Action::SetTimer { id, ms } => {
                // SAFETY: a null timer procedure means the timer posts WM_TIMER to the window; plain numbers only.
                unsafe { SetTimer(hwnd, id as usize, ms, null()) };
            }
            Action::KillTimer(id) => {
                // SAFETY: plain numbers; killing a timer that is not set does nothing.
                unsafe { KillTimer(hwnd, id as usize) };
            }
            Action::PickFile { title, filters } => {
                let picked = open_file_dialog(hwnd, &title, &filters);
                queue.extend(deliver(state, Event::FilePicked(picked)));
            }
            Action::AskText { tag, title, prompt, secret, ok, cancel } => {
                let text = input_dialog(hwnd, state.dpi.get(), state.font.get(), &title, &prompt, secret, &ok, &cancel);
                queue.extend(deliver(state, Event::Text { tag, text }));
            }
            Action::Message { title, text } => {
                let (t, c) = (wide(&text), wide(&title));
                // SAFETY: both strings are zero-terminated UTF-16 that outlive the call; the box is modal and
                // returns when it is closed.
                unsafe { MessageBoxW(hwnd, t.as_ptr(), c.as_ptr(), MB_OK | MB_ICONINFORMATION) };
            }
            Action::Quit => {
                // SAFETY: destroys the window made by `run`; posts WM_DESTROY, which ends the loop. The window state is not
                // freed by this (that is done after the loop), so `state` stays valid for the rest of this function.
                unsafe { DestroyWindow(hwnd) };
                // Nothing after a Quit is carried out.
                break;
            }
        }
    }
}

fn set_scroll(hwnd: isize, bar: Bar, info: ScrollInfo) {
    let (which, extra) = match bar {
        // The vertical bar stays (greyed when there is nothing to scroll) so that the window does not change its
        // width when a page is just too short; the horizontal one comes and goes.
        Bar::Vertical => (SB_VERT, SIF_DISABLENOSCROLL),
        Bar::Horizontal => (SB_HORZ, 0),
    };
    let si = ScrollInfoW { size: std::mem::size_of::<ScrollInfoW>() as u32, mask: SIF_RANGE | SIF_PAGE | SIF_POS | extra, min: info.min, max: info.max, page: info.page, pos: info.pos, track_pos: 0 };
    // SAFETY: `si` is a live SCROLLINFO of the size given in `size`; Windows copies it.
    unsafe { SetScrollInfo(hwnd, which, &si, 1) };
}

fn track_pos(hwnd: isize, which: i32) -> i32 {
    let mut si = ScrollInfoW { size: std::mem::size_of::<ScrollInfoW>() as u32, mask: SIF_TRACKPOS, min: 0, max: 0, page: 0, pos: 0, track_pos: 0 };
    // SAFETY: `si` is a live SCROLLINFO of the size given in `size`; Windows fills it in and does not keep it.
    unsafe { GetScrollInfo(hwnd, which, &mut si) };
    si.track_pos
}

fn modifier(key: i32) -> bool {
    // SAFETY: reads the state of one key; takes and returns plain numbers.
    unsafe { GetKeyState(key) < 0 }
}

/// The window procedure.
unsafe extern "system" fn wnd_proc(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize {
    if msg == WM_GETMINMAXINFO {
        // SAFETY: the default procedure takes the same arguments as this one; it fills in the usual limits.
        let result = unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
        if ALLOW_BIG_WINDOW.load(Ordering::Relaxed) {
            // SAFETY: for WM_GETMINMAXINFO, lparam points to a MINMAXINFO that is valid and ours to change for the
            // duration of this message.
            if let Some(info) = unsafe { (lparam as *mut MinMaxInfo).as_mut() } {
                info.max_track = Point { x: 16384, y: 16384 };
            }
        }
        return result;
    }
    // A panic is being reported on this thread by a box that handles messages: answer nothing, touch nothing.
    if PANICKED.load(Ordering::Relaxed) {
        // SAFETY: the default procedure takes the same arguments as this one.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let Some(state) = state_of(hwnd) else {
        // SAFETY: the default procedure takes the same arguments as this one.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    match msg {
        WM_ERASEBKGND => return 1,
        WM_PAINT => {
            paint(state, hwnd);
            return 0;
        }
        WM_SIZE => {
            if wparam != 1 {
                let (w, h) = (loword(lparam as usize) as i32, hiword(lparam as usize) as i32);
                dispatch(state, Event::Size { w, h });
            }
            return 0;
        }
        WM_KEYDOWN => {
            dispatch(state, Event::Key { vk: wparam as u32, ctrl: modifier(0x11), shift: modifier(0x10) });
            return 0;
        }
        WM_MOUSEWHEEL => {
            let delta = i32::from(hiword(wparam) as u16 as i16);
            let keys = loword(wparam);
            let mut p = Point { x: i32::from(loword(lparam as usize) as u16 as i16), y: i32::from(hiword(lparam as usize) as u16 as i16) };
            // SAFETY: `p` is a live POINT that Windows rewrites in place.
            unsafe { ScreenToClient(hwnd, &mut p) };
            dispatch(state, Event::Wheel { delta, ctrl: keys & 0x8 != 0, shift: keys & 0x4 != 0, x: p.x, y: p.y });
            return 0;
        }
        WM_MOUSEHWHEEL => {
            dispatch(state, Event::HWheel { delta: i32::from(hiword(wparam) as u16 as i16) });
            return 0;
        }
        WM_VSCROLL | WM_HSCROLL => {
            let (bar, which) = if msg == WM_VSCROLL { (Bar::Vertical, SB_VERT) } else { (Bar::Horizontal, SB_HORZ) };
            let code = match loword(wparam) {
                0 => Some(ScrollCode::LineUp),
                1 => Some(ScrollCode::LineDown),
                2 => Some(ScrollCode::PageUp),
                3 => Some(ScrollCode::PageDown),
                4 | 5 => Some(ScrollCode::Thumb(track_pos(hwnd, which))),
                6 => Some(ScrollCode::Top),
                7 => Some(ScrollCode::Bottom),
                _ => None,
            };
            if let Some(code) = code {
                dispatch(state, Event::Scroll { bar, code });
            }
            return 0;
        }
        WM_COMMAND => {
            if lparam == 0 {
                dispatch(state, Event::Command(loword(wparam)));
            }
            return 0;
        }
        WM_TIMER => {
            dispatch(state, Event::Timer(wparam as u32));
            return 0;
        }
        WM_WAKE => {
            dispatch(state, Event::Wake);
            return 0;
        }
        WM_DROPFILES => {
            let files = dropped_files(wparam as isize);
            dispatch(state, Event::Drop(files));
            return 0;
        }
        WM_DPICHANGED => {
            let dpi = loword(wparam);
            // SAFETY: for WM_DPICHANGED, lparam points to a RECT (the suggested new window rectangle) that is
            // valid for the duration of this message; it is only read here.
            let suggested = unsafe { (lparam as *const WinRect).as_ref() }.copied();
            if let Some(r) = suggested {
                // SAFETY: plain numbers and the handle of this window.
                unsafe { SetWindowPos(hwnd, 0, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZOMORDER | SWP_NOACTIVATE) };
            }
            new_dpi(state, dpi);
            dispatch(state, Event::Dpi(dpi));
            return 0;
        }
        WM_DESTROY => {
            state.closing.set(true);
            // SAFETY: ends the message loop of this thread.
            unsafe { PostQuitMessage(0) };
            return 0;
        }
        WM_NCDESTROY => {
            // The state is not freed here: this message can arrive inside a call that still holds a reference to it (a
            // box that was up when the window was closed). `run` frees it once the message loop has ended. The slot is
            // cleared so that nothing finds it through the window any more.
            // SAFETY: plain numbers and the handle of this window; this is the last message it gets.
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
            return 0;
        }
        _ => {}
    }
    // SAFETY: the default procedure takes the same arguments as this one.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn dropped_files(drop: isize) -> Vec<PathBuf> {
    // SAFETY: `drop` is the HDROP of a WM_DROPFILES message, valid until DragFinish. 0xFFFFFFFF asks for the count.
    let count = unsafe { DragQueryFileW(drop, u32::MAX, null_mut(), 0) };
    let mut files = Vec::new();
    for i in 0..count {
        // SAFETY: with a null buffer the call returns the length in characters without writing anything.
        let len = unsafe { DragQueryFileW(drop, i, null_mut(), 0) } as usize;
        let mut buf = vec![0u16; len + 1];
        // SAFETY: `buf` has room for `len + 1` characters, which is what the call is told.
        let got = unsafe { DragQueryFileW(drop, i, buf.as_mut_ptr(), (len + 1) as u32) } as usize;
        files.push(PathBuf::from(OsString::from_wide(buf.get(..got.min(len)).unwrap_or(&[]))));
    }
    // SAFETY: releases the HDROP, which is not used again.
    unsafe { DragFinish(drop) };
    files
}

fn new_dpi(state: &WindowState, dpi: u32) {
    state.dpi.set(dpi);
    let old = state.font.replace(make_font(dpi));
    let back = state.back.borrow();
    if back.dc != 0 && state.font.get() != 0 {
        // SAFETY: both handles are valid (the memory DC of the back buffer, the font just made). The old font is
        // deleted only after the new one is selected in its place.
        unsafe { SelectObject(back.dc, state.font.get()) };
    }
    if old != 0 {
        // SAFETY: `old` was a font made by `make_font` and is no longer selected into any device context.
        unsafe { DeleteObject(old) };
    }
}

fn free_back(state: &WindowState) {
    let mut b = state.back.borrow_mut();
    // SAFETY: the DC and bitmap were made by `paint`; the bitmap is deleted after the DC that held it, both once.
    unsafe {
        if b.dc != 0 {
            DeleteDC(b.dc);
        }
        if b.bitmap != 0 {
            DeleteObject(b.bitmap);
        }
    }
    *b = Back::default();
}

/// Draws on a memory device context.
struct Gdi {
    dc: isize,
    line: i32,
}

impl Painter for Gdi {
    fn fill_rect(&mut self, r: Rect, color: Rgb) {
        if r.w <= 0 || r.h <= 0 {
            return;
        }
        let rc = WinRect { left: r.x, top: r.y, right: r.x.saturating_add(r.w), bottom: r.y.saturating_add(r.h) };
        // SAFETY: `rc` is a live RECT; the brush is made here, checked, and deleted after use; `self.dc` is the
        // valid memory DC of the back buffer.
        unsafe {
            let brush = CreateSolidBrush(colorref(color));
            if brush != 0 {
                FillRect(self.dc, &rc, brush);
                DeleteObject(brush);
            }
        }
    }

    fn draw_bgra(&mut self, dest: Rect, src_w: i32, src_h: i32, bgra: &[u8]) {
        if dest.w <= 0 || dest.h <= 0 || src_w <= 0 || src_h <= 0 {
            return;
        }
        let needed = (src_w as usize).saturating_mul(src_h as usize).saturating_mul(4);
        if bgra.len() < needed {
            return;
        }
        let header = BitmapInfoHeader {
            size: std::mem::size_of::<BitmapInfoHeader>() as u32,
            width: src_w,
            // A negative height: the rows are from the top.
            height: -src_h,
            planes: 1,
            bit_count: 32,
            compression: 0,
            size_image: 0,
            x_pels: 0,
            y_pels: 0,
            clr_used: 0,
            clr_important: 0,
        };
        // SAFETY: `bgra` has at least `src_w * src_h * 4` bytes (checked above), which is all the call reads of
        // the pixels; `header` is a live BITMAPINFOHEADER for a 32-bit uncompressed picture, which needs no
        // colour table; `self.dc` is the valid memory DC of the back buffer. Windows keeps neither pointer.
        unsafe {
            StretchDIBits(self.dc, dest.x, dest.y, dest.w, dest.h, 0, 0, src_w, src_h, bgra.as_ptr().cast(), &header, 0, SRCCOPY);
        }
    }

    fn text(&mut self, x: i32, y: i32, s: &str, color: Rgb) {
        let w: Vec<u16> = s.encode_utf16().collect();
        // SAFETY: `w` outlives the call and its length is passed; `self.dc` is the valid memory DC.
        unsafe {
            SetBkMode(self.dc, TRANSPARENT);
            SetTextColor(self.dc, colorref(color));
            TextOutW(self.dc, x, y, w.as_ptr(), w.len() as i32);
        }
    }

    fn text_width(&mut self, s: &str) -> i32 {
        let w: Vec<u16> = s.encode_utf16().collect();
        let mut size = Size { cx: 0, cy: 0 };
        // SAFETY: `w` outlives the call and its length is passed; `size` is a live SIZE that is filled in.
        unsafe { GetTextExtentPoint32W(self.dc, w.as_ptr(), w.len() as i32, &mut size) };
        size.cx
    }

    fn line_height(&self) -> i32 {
        self.line
    }
}

fn paint(state: &WindowState, hwnd: isize) {
    let mut ps = PaintStruct { hdc: 0, erase: 0, paint: WinRect::default(), restore: 0, inc_update: 0, reserved: [0; 32] };
    // SAFETY: `ps` is a live PAINTSTRUCT; BeginPaint and EndPaint are paired below on every path.
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
    if hdc != 0 {
        paint_to(state, hwnd, hdc);
    }
    // SAFETY: pairs the BeginPaint above with the same window and the same PAINTSTRUCT.
    unsafe { EndPaint(hwnd, &ps) };
}

/// Draw the reader on the back buffer and put that on the window's device context `hdc`.
fn paint_to(state: &WindowState, hwnd: isize, hdc: isize) {
    let (w, h) = client_size(hwnd);
    if w <= 0 || h <= 0 {
        return;
    }
    let Ok(mut back) = state.back.try_borrow_mut() else { return };
    if back.dc == 0 || back.w != w || back.h != h {
        resize_back(state, &mut back, hdc, w, h);
    }
    if back.dc == 0 || back.bitmap == 0 {
        return;
    }
    let mut gdi = Gdi { dc: back.dc, line: line_height(back.dc) };
    let mut present = true;
    if let Ok(mut guard) = state.handler.try_borrow_mut()
        && let Some(handler) = guard.as_mut()
    {
        present = handler.paint(&mut gdi);
    }
    if present {
        // SAFETY: both DCs are valid and the memory DC holds a bitmap of at least w by h pixels.
        unsafe { BitBlt(hdc, 0, 0, w, h, back.dc, 0, 0, SRCCOPY) };
    }
}

/// Make the back buffer `w` by `h` (and its device context the first time).
fn resize_back(state: &WindowState, back: &mut Back, hdc: isize, w: i32, h: i32) {
    // SAFETY: `hdc` is the valid DC of this paint. The old bitmap is deselected (by selecting the new one in the same
    // DC) before it is deleted; handles are checked before use.
    unsafe {
        if back.dc == 0 {
            back.dc = CreateCompatibleDC(hdc);
            if state.font.get() != 0 && back.dc != 0 {
                SelectObject(back.dc, state.font.get());
            }
            if back.dc != 0 {
                SetStretchBltMode(back.dc, COLORONCOLOR);
            }
        }
        let bitmap = CreateCompatibleBitmap(hdc, w, h);
        if back.dc != 0 && bitmap != 0 {
            SelectObject(back.dc, bitmap);
            if back.bitmap != 0 {
                DeleteObject(back.bitmap);
            }
            back.bitmap = bitmap;
            back.w = w;
            back.h = h;
        } else if bitmap != 0 {
            DeleteObject(bitmap);
        }
    }
}

fn line_height(dc: isize) -> i32 {
    let mut size = Size { cx: 0, cy: 0 };
    let probe = [b'M' as u16, b'g' as u16];
    // SAFETY: `probe` is a live array of two characters (its length is passed); `size` is filled in.
    unsafe { GetTextExtentPoint32W(dc, probe.as_ptr(), 2, &mut size) };
    size.cy.max(8)
}

// --- boxes -------------------------------------------------------------------------------------------------------

fn open_file_dialog(owner: isize, title: &str, filters: &[(String, String)]) -> Option<PathBuf> {
    let mut filter: Vec<u16> = Vec::new();
    for (name, pattern) in filters {
        filter.extend(name.encode_utf16());
        filter.push(0);
        filter.extend(pattern.encode_utf16());
        filter.push(0);
    }
    filter.push(0);
    let title = wide(title);
    let mut file = vec![0u16; 32_768];
    let mut ofn = OpenFileNameW {
        struct_size: std::mem::size_of::<OpenFileNameW>() as u32,
        owner,
        instance: 0,
        filter: filter.as_ptr(),
        custom_filter: null_mut(),
        max_custom_filter: 0,
        filter_index: 1,
        file: file.as_mut_ptr(),
        max_file: file.len() as u32,
        file_title: null_mut(),
        max_file_title: 0,
        initial_dir: null(),
        title: title.as_ptr(),
        flags: OFN_PATHMUSTEXIST | OFN_FILEMUSTEXIST | OFN_NOCHANGEDIR | OFN_EXPLORER,
        file_offset: 0,
        file_extension: 0,
        def_ext: null(),
        cust_data: 0,
        hook: null(),
        template_name: null(),
        reserved: null_mut(),
        reserved_dw: 0,
        flags_ex: 0,
    };
    // SAFETY: `ofn` is a live OPENFILENAMEW of the size it declares; the strings it points to (`filter`, `title`)
    // and the buffer `file` (of the length declared in `max_file`) outlive the call; the box is modal and Windows
    // keeps none of the pointers after it returns.
    let chosen = unsafe { GetOpenFileNameW(&mut ofn) } != 0;
    if !chosen {
        return None;
    }
    let len = file.iter().position(|&c| c == 0).unwrap_or(file.len());
    Some(PathBuf::from(OsString::from_wide(file.get(..len).unwrap_or(&[]))))
}

struct DialogState {
    done: Cell<bool>,
    accepted: Cell<bool>,
}

unsafe extern "system" fn dialog_proc(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize {
    // SAFETY: the user-data slot holds 0 or the pointer to the `DialogState` on the stack of `input_dialog`, which
    // outlives the dialog window (it is destroyed before that function returns); used on the same thread only.
    let state = unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const DialogState).as_ref() };
    if let Some(state) = state {
        match msg {
            WM_COMMAND if loword(wparam) as usize == IDOK || loword(wparam) as usize == IDCANCEL => {
                state.accepted.set(loword(wparam) as usize == IDOK);
                state.done.set(true);
                return 0;
            }
            WM_CLOSE => {
                state.done.set(true);
                return 0;
            }
            _ => {}
        }
    }
    // SAFETY: the default procedure takes the same arguments as this one.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// A box with a line of text to fill in. Modal: it handles messages itself until it is closed.
#[allow(clippy::too_many_arguments)]
fn input_dialog(owner: isize, dpi: u32, font: isize, title: &str, prompt: &str, secret: bool, ok: &str, cancel: &str) -> Option<String> {
    let s = |v: i32| v * dpi as i32 / 96;
    let class = wide("QingpdfInput");
    // SAFETY: a null module name means the module of this program.
    let instance = unsafe { GetModuleHandleW(null()) };
    let wc = WndClassExW {
        size: std::mem::size_of::<WndClassExW>() as u32,
        style: 0,
        wnd_proc: Some(dialog_proc),
        cls_extra: 0,
        wnd_extra: 0,
        instance,
        icon: 0,
        // SAFETY: a small integer resource id in the pointer slot is how LoadCursorW names a stock cursor.
        cursor: unsafe { LoadCursorW(0, IDC_ARROW as *const u16) },
        background: 16, // COLOR_BTNFACE + 1: the same grey as the labels and buttons on it
        menu_name: null(),
        class_name: class.as_ptr(),
        icon_small: 0,
    };
    // SAFETY: `wc` is a live WNDCLASSEXW whose strings outlive the call; registering twice fails harmlessly.
    unsafe { RegisterClassExW(&wc) };

    let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
    let mut frame = WinRect { left: 0, top: 0, right: s(380), bottom: s(138) };
    // SAFETY: `frame` is a live RECT that is rewritten in place.
    unsafe { AdjustWindowRectExForDpi(&mut frame, style, 0, WS_EX_DLGMODALFRAME, dpi) };
    let (fw, fh) = (frame.right - frame.left, frame.bottom - frame.top);
    let mut at = WinRect::default();
    // SAFETY: `at` is a live RECT that Windows fills in.
    unsafe { GetWindowRect(owner, &mut at) };
    let (x, y) = (at.left + ((at.right - at.left) - fw) / 2, at.top + ((at.bottom - at.top) - fh) / 3);
    let title_w = wide(title);
    // SAFETY: the strings are zero-terminated UTF-16 that outlive the call; `owner` is the main window; no
    // creation parameter is passed. A handle of 0 means failure and is checked.
    let dialog = unsafe { CreateWindowExW(WS_EX_DLGMODALFRAME, class.as_ptr(), title_w.as_ptr(), style, x, y, fw, fh, owner, 0, instance, null_mut()) };
    if dialog == 0 {
        return None;
    }
    let state = DialogState { done: Cell::new(false), accepted: Cell::new(false) };
    // SAFETY: stores the address of `state` for `dialog_proc`; `state` lives until after the window is destroyed
    // at the end of this function, and the slot is only read on this thread.
    unsafe { SetWindowLongPtrW(dialog, GWLP_USERDATA, std::ptr::addr_of!(state) as isize) };

    let child = |class: &str, text: &str, style: u32, ex: u32, x: i32, y: i32, w: i32, h: i32, id: usize| -> isize {
        let (c, t) = (wide(class), wide(text));
        // SAFETY: the strings are zero-terminated UTF-16 that outlive the call; `dialog` is the parent window; the
        // control id goes in the menu slot as Windows defines for child windows. 0 means failure and is checked.
        let h = unsafe { CreateWindowExW(ex, c.as_ptr(), t.as_ptr(), WS_CHILD | WS_VISIBLE | style, s(x), s(y), s(w), s(h), dialog, id as isize, instance, null_mut()) };
        if h != 0 && font != 0 {
            // SAFETY: WM_SETFONT takes a font handle, which `font` is, and a redraw flag.
            unsafe { SendMessageW(h, WM_SETFONT, font as usize, 1) };
        }
        h
    };
    child("STATIC", prompt, 0, 0, 16, 14, 348, 20, 100);
    let edit = child("EDIT", "", WS_TABSTOP | ES_AUTOHSCROLL | if secret { ES_PASSWORD } else { 0 }, WS_EX_CLIENTEDGE, 16, 38, 348, 24, 101);
    child("BUTTON", ok, WS_TABSTOP | BS_DEFPUSHBUTTON, 0, 176, 88, 90, 28, IDOK);
    child("BUTTON", cancel, WS_TABSTOP | BS_PUSHBUTTON, 0, 274, 88, 90, 28, IDCANCEL);

    // SAFETY: plain handles of windows made above; the owner is disabled while the box is up and enabled again
    // below on every path.
    unsafe {
        EnableWindow(owner, 0);
        ShowWindow(dialog, SW_SHOW);
        if edit != 0 {
            SetFocus(edit);
        }
    }
    let mut msg = Msg { hwnd: 0, message: 0, wparam: 0, lparam: 0, time: 0, pt: Point::default(), private: 0 };
    while !state.done.get() {
        // SAFETY: `msg` is a live MSG that is filled in; the other arguments are plain. A result of 0 is WM_QUIT
        // (posted again so the main loop sees it), -1 an error: either way the box is left.
        let got = unsafe { GetMessageW(&mut msg, 0, 0, 0) };
        if got <= 0 {
            if got == 0 {
                // SAFETY: re-posts the quit request that ended this loop.
                unsafe { PostQuitMessage(msg.wparam as i32) };
            }
            break;
        }
        // SAFETY: `msg` was filled in by GetMessageW; IsDialogMessageW handles Tab, Enter and Escape for the box.
        unsafe {
            if IsDialogMessageW(dialog, &mut msg) == 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
    let text = if state.accepted.get() && edit != 0 {
        // SAFETY: the edit control is alive; `buf` has room for the length asked for plus the closing zero, which
        // is what the call is told; the length is read just before.
        let mut buf = unsafe {
            let n = GetWindowTextLengthW(edit).max(0) as usize;
            let mut buf = vec![0u16; n + 1];
            let got = GetWindowTextW(edit, buf.as_mut_ptr(), buf.len() as i32).max(0) as usize;
            buf.truncate(got);
            buf
        };
        let text = String::from_utf16_lossy(&buf);
        // The typed characters are overwritten (they may be a password).
        buf.fill(0);
        std::hint::black_box(&buf);
        Some(text)
    } else {
        None
    };
    // SAFETY: clears the box's text before it goes, enables the owner again, and destroys the dialog window, which
    // makes `state` (still alive here) unreachable: its slot is cleared first.
    unsafe {
        if edit != 0 {
            SetWindowTextW(edit, [0u16].as_ptr());
        }
        EnableWindow(owner, 1);
        SetWindowLongPtrW(dialog, GWLP_USERDATA, 0);
        DestroyWindow(dialog);
        SetForegroundWindow(owner);
    }
    text
}

// --- starting up -------------------------------------------------------------------------------------------------

fn build_menu(menus: &[Menu]) -> isize {
    // SAFETY: creates an empty menu bar; the handle is checked below.
    let bar = unsafe { CreateMenu() };
    if bar == 0 {
        return 0;
    }
    for menu in menus {
        // SAFETY: creates an empty popup menu; the handle is checked.
        let popup = unsafe { CreatePopupMenu() };
        if popup == 0 {
            continue;
        }
        for entry in &menu.entries {
            match entry {
                MenuEntry::Item { id, label } => {
                    let w = wide(label);
                    // SAFETY: `w` is a zero-terminated UTF-16 string that outlives the call (Windows copies it).
                    unsafe { AppendMenuW(popup, MF_STRING, *id as usize, w.as_ptr()) };
                }
                // SAFETY: a separator takes no text; the arguments are plain.
                MenuEntry::Separator => unsafe {
                    AppendMenuW(popup, MF_SEPARATOR, 0, null());
                },
            }
        }
        let w = wide(&menu.title);
        // SAFETY: `popup` becomes a submenu of `bar` (and is destroyed with it); `w` outlives the call.
        unsafe { AppendMenuW(bar, MF_POPUP, popup as usize, w.as_ptr()) };
    }
    bar
}

/// Make the window and run it until it is closed. `make` is given the means to wake the window from another thread
/// and builds the reader. `client`: a size for the window's client area, in pixels (the speed test uses it; it may be bigger
/// than the screen); otherwise the window is most of the work area.
pub fn run(title: &str, menus: &[Menu], client: Option<(i32, i32)>, make: impl FnOnce(Waker) -> Box<dyn Handler>) -> i32 {
    let icc = InitCommonControlsEx { size: std::mem::size_of::<InitCommonControlsEx>() as u32, icc: ICC_STANDARD_CLASSES };
    // SAFETY: `icc` is a live struct of the size it declares; this makes the buttons and boxes use the new look.
    unsafe { InitCommonControlsEx(&icc) };
    let class = wide("QingpdfView");
    // SAFETY: a null module name means the module of this program.
    let instance = unsafe { GetModuleHandleW(null()) };
    let wc = WndClassExW {
        size: std::mem::size_of::<WndClassExW>() as u32,
        style: 0,
        wnd_proc: Some(wnd_proc),
        cls_extra: 0,
        wnd_extra: 0,
        instance,
        icon: 0,
        // SAFETY: a small integer resource id in the pointer slot is how LoadCursorW names a stock cursor.
        cursor: unsafe { LoadCursorW(0, IDC_ARROW as *const u16) },
        background: 0,
        menu_name: null(),
        class_name: class.as_ptr(),
        icon_small: 0,
    };
    // SAFETY: `wc` is a live WNDCLASSEXW whose strings outlive the call (Windows copies what it needs).
    if unsafe { RegisterClassExW(&wc) } == 0 {
        return 1;
    }
    // SAFETY: no arguments; reads a system setting.
    let system_dpi = unsafe { GetDpiForSystem() }.max(96);
    let style = WS_OVERLAPPEDWINDOW | WS_VSCROLL | WS_HSCROLL;
    let mut frame = WinRect { left: 0, top: 0, right: DEFAULT_WIDTH * system_dpi as i32 / 96, bottom: DEFAULT_HEIGHT * system_dpi as i32 / 96 };
    // SAFETY: `frame` is a live RECT that is rewritten in place.
    unsafe { AdjustWindowRectExForDpi(&mut frame, style, 1, WS_EX_ACCEPTFILES, system_dpi) };
    // The window goes in the middle of the screen's work area (not under the task bar) and is no bigger than most of it.
    let mut work = WinRect::default();
    // SAFETY: SPI_GETWORKAREA fills in the RECT that the pointer points to; `work` is a live RECT.
    let have_work = unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, (&raw mut work).cast(), 0) } != 0;
    let (mut x, mut y) = (CW_USEDEFAULT, CW_USEDEFAULT);
    let (mut w, mut h) = (frame.right - frame.left, frame.bottom - frame.top);
    if let Some((cw, ch)) = client {
        ALLOW_BIG_WINDOW.store(true, Ordering::Relaxed);
        frame = WinRect { left: 0, top: 0, right: cw.clamp(100, 16000), bottom: ch.clamp(100, 16000) };
        // SAFETY: `frame` is a live RECT that is rewritten in place.
        unsafe { AdjustWindowRectExForDpi(&mut frame, style, 1, WS_EX_ACCEPTFILES, system_dpi) };
        (w, h) = (frame.right - frame.left, frame.bottom - frame.top);
        if have_work {
            (x, y) = (work.left, work.top);
        }
    } else if have_work && work.right > work.left && work.bottom > work.top {
        let (aw, ah) = (work.right - work.left, work.bottom - work.top);
        w = w.min(aw * 95 / 100);
        h = h.min(ah * 95 / 100);
        x = work.left + (aw - w) / 2;
        y = work.top + (ah - h) / 2;
    }
    let name = wide(title);
    // SAFETY: the strings are zero-terminated UTF-16 that outlive the call; no parent, menu or creation parameter.
    let hwnd = unsafe { CreateWindowExW(WS_EX_ACCEPTFILES, class.as_ptr(), name.as_ptr(), style, x, y, w, h, 0, 0, instance, null_mut()) };
    if hwnd == 0 {
        return 1;
    }
    let state = Box::new(WindowState { hwnd: Cell::new(hwnd), closing: Cell::new(false), handler: RefCell::new(None), back: RefCell::new(Back::default()), font: Cell::new(0), dpi: Cell::new(96) });
    let raw = Box::into_raw(state);
    // SAFETY: stores the pointer from `Box::into_raw`; it is freed once, by `free_state` below, after the message loop.
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, raw as isize) };
    let Some(state) = state_of(hwnd) else { return 1 };
    if let Some((cw, ch)) = client {
        // The scroll bars and the like take some of the window: make the client area the size asked for.
        let (have_w, have_h) = client_size(hwnd);
        if (have_w, have_h) != (cw, ch) {
            // SAFETY: plain numbers and the handle of the window just made; no move, no restack.
            unsafe { SetWindowPos(hwnd, 0, 0, 0, w + (cw - have_w), h + (ch - have_h), SWP_NOZOMORDER | SWP_NOACTIVATE | SWP_NOMOVE) };
        }
    }

    let menu = build_menu(menus);
    if menu != 0 {
        // SAFETY: `menu` is a menu bar just made; the window takes it over and destroys it with itself.
        unsafe { SetMenu(hwnd, menu) };
    }
    // SAFETY: plain handle; 1 means "accept dropped files".
    unsafe { DragAcceptFiles(hwnd, 1) };
    *state.handler.borrow_mut() = Some(make(Waker(hwnd)));

    // SAFETY: plain handle of the window just made.
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    new_dpi(state, dpi);
    dispatch(state, Event::Dpi(dpi));
    let (w, h) = client_size(hwnd);
    dispatch(state, Event::Size { w, h });
    // SAFETY: plain handle of the window just made.
    unsafe {
        ShowWindow(hwnd, SW_SHOWNORMAL);
        UpdateWindow(hwnd);
    }

    let mut msg = Msg { hwnd: 0, message: 0, wparam: 0, lparam: 0, time: 0, pt: Point::default(), private: 0 };
    loop {
        // SAFETY: `msg` is a live MSG that is filled in; 0 is WM_QUIT, -1 an error, both end the loop.
        let got = unsafe { GetMessageW(&mut msg, 0, 0, 0) };
        if got <= 0 {
            break;
        }
        // SAFETY: `msg` was filled in by GetMessageW and is passed on unchanged.
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    let code = msg.wparam as i32;
    // The loop has ended: no message is being handled, so nothing holds the state any more.
    free_state(raw);
    code
}

/// Free the window state made by `run`: the reader (and with it the engine), the back buffer and the font.
fn free_state(raw: *mut WindowState) {
    // SAFETY: `raw` is the pointer from `Box::into_raw` in `run`; this is the only place it is turned back into a box, called
    // once, after the message loop (the only thing that gives the state to window procedures) has ended. The user-data slot
    // was cleared at WM_NCDESTROY, or the window is destroyed with the process.
    let state = unsafe { Box::from_raw(raw) };
    free_back(&state);
    if state.font.get() != 0 {
        // SAFETY: the font was made by `make_font` and the device context that had it selected is gone.
        unsafe { DeleteObject(state.font.get()) };
    }
}

/// The size in pixels of the work area of the main screen (the screen less the task bar): the most a window can show
/// without being bigger than the screen. `(0, 0)` if Windows will not say.
pub fn work_area_size() -> (i32, i32) {
    let mut work = WinRect::default();
    // SAFETY: SPI_GETWORKAREA fills in the RECT that the pointer points to; `work` is a live RECT.
    let ok = unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, (&raw mut work).cast(), 0) } != 0;
    if ok { ((work.right - work.left).max(0), (work.bottom - work.top).max(0)) } else { (0, 0) }
}

/// Show a box about a fatal error, for the panic hook. It may be called from any thread, with the program in any state: it
/// allocates a little, calls Windows once, and touches nothing of the reader. From then on the main window answers no
/// message (the box handles messages while it is up), and only the first box of a run is shown.
pub fn fatal_box(title: &str, text: &str) {
    static SHOWN: AtomicBool = AtomicBool::new(false);
    PANICKED.store(true, Ordering::SeqCst);
    if SHOWN.swap(true, Ordering::SeqCst) {
        return;
    }
    let (t, c) = (wide(text), wide(title));
    // SAFETY: both strings are zero-terminated UTF-16 that outlive the call; a null owner makes the box a window of its
    // own; it is modal to the thread and returns when it is closed.
    unsafe { MessageBoxW(0, t.as_ptr(), c.as_ptr(), MB_OK | MB_ICONERROR | MB_TASKMODAL) };
}
