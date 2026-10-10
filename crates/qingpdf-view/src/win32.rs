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
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::ui::{Action, Bar, Cursor, Event, Handler, Menu, MenuEntry, OutlineNode, Painter, PrintOp, PrinterPreset, PrinterSetup, Rect, Rgb, ScrollCode, ScrollInfo};

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

/// TVITEMW.
#[repr(C)]
struct TvItem {
    mask: u32,
    item: isize,
    state: u32,
    state_mask: u32,
    text: *mut u16,
    text_max: i32,
    image: i32,
    selected_image: i32,
    children: i32,
    param: isize,
}

/// TVITEMEXW (the item of a TVINSERTSTRUCTW is the extended form, the bigger of the two, so the control never reads past it).
#[repr(C)]
struct TvItemEx {
    item: TvItem,
    integral: i32,
    state_ex: u32,
    window: isize,
    expanded_image: i32,
    reserved: i32,
}

#[repr(C)]
struct TvInsert {
    parent: isize,
    after: isize,
    item: TvItemEx,
}

#[repr(C)]
struct TvHitTest {
    pt: Point,
    flags: u32,
    item: isize,
}

#[repr(C)]
struct NmHdr {
    from: isize,
    id: usize,
    code: u32,
}

#[repr(C)]
struct NmTreeView {
    hdr: NmHdr,
    action: u32,
    old: TvItem,
    new: TvItem,
    pt: Point,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PrintPageRange {
    from: u32,
    to: u32,
}

/// PRINTDLGEXW.
#[repr(C)]
struct PrintDlgEx {
    size: u32,
    owner: isize,
    devmode: isize,
    devnames: isize,
    dc: isize,
    flags: u32,
    flags2: u32,
    exclusion_flags: u32,
    page_range_count: u32,
    page_range_max: u32,
    page_ranges: *mut PrintPageRange,
    min_page: u32,
    max_page: u32,
    copies: u32,
    instance: isize,
    template_name: *const u16,
    callback: *const c_void,
    property_page_count: u32,
    property_pages: *const isize,
    start_page: u32,
    result_action: u32,
}

/// DOCINFOW.
#[repr(C)]
struct DocInfo {
    size: i32,
    doc_name: *const u16,
    output: *const u16,
    datatype: *const u16,
    kind: u32,
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
    fn GetFocus() -> isize;
    fn GetCursorPos(point: *mut Point) -> i32;
    fn SetCursor(cursor: isize) -> isize;
    fn SetCapture(hwnd: isize) -> isize;
    fn ReleaseCapture() -> i32;
    fn OpenClipboard(hwnd: isize) -> i32;
    fn CloseClipboard() -> i32;
    fn EmptyClipboard() -> i32;
    fn SetClipboardData(format: u32, mem: isize) -> isize;
    fn DestroyMenu(menu: isize) -> i32;
    fn DrawMenuBar(hwnd: isize) -> i32;
    fn GetMenu(hwnd: isize) -> isize;
    fn MoveWindow(hwnd: isize, x: i32, y: i32, w: i32, h: i32, repaint: i32) -> i32;
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
    fn GetDeviceCaps(hdc: isize, index: i32) -> i32;
    fn CreateDCW(driver: *const u16, device: *const u16, port: *const u16, devmode: *const c_void) -> isize;
    fn StartDocW(hdc: isize, info: *const DocInfo) -> i32;
    fn EndDoc(hdc: isize) -> i32;
    fn AbortDoc(hdc: isize) -> i32;
    fn StartPage(hdc: isize) -> i32;
    fn EndPage(hdc: isize) -> i32;
}

#[link(name = "msimg32")]
unsafe extern "system" {
    #[allow(clippy::too_many_arguments)]
    fn AlphaBlend(dest: isize, xd: i32, yd: i32, wd: i32, hd: i32, src: isize, xs: i32, ys: i32, ws: i32, hs: i32, blend: u32) -> i32;
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
    fn GlobalAlloc(flags: u32, bytes: usize) -> isize;
    fn GlobalLock(mem: isize) -> *mut c_void;
    fn GlobalUnlock(mem: isize) -> i32;
    fn GlobalFree(mem: isize) -> isize;
}

#[link(name = "ole32")]
unsafe extern "system" {
    fn CoInitializeEx(reserved: *const c_void, flags: u32) -> i32;
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn DragAcceptFiles(hwnd: isize, accept: i32);
    fn DragQueryFileW(drop: isize, index: u32, buffer: *mut u16, len: u32) -> u32;
    fn DragFinish(drop: isize);
    fn ShellExecuteW(hwnd: isize, verb: *const u16, file: *const u16, params: *const u16, dir: *const u16, show: i32) -> isize;
}

#[link(name = "comdlg32")]
unsafe extern "system" {
    fn GetOpenFileNameW(ofn: *mut OpenFileNameW) -> i32;
    fn PrintDlgExW(dialog: *mut PrintDlgEx) -> i32;
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
const WM_ENTERMENULOOP: u32 = 0x0211;
const WM_EXITMENULOOP: u32 = 0x0212;
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
const MF_CHECKED: u32 = 0x8;
const MF_GRAYED: u32 = 0x1;
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
const WM_SETREDRAW: u32 = 0x000B;
const WM_SETCURSOR: u32 = 0x0020;
const WM_NOTIFY: u32 = 0x004E;
const WM_MOUSEMOVE: u32 = 0x0200;
const WM_LBUTTONDOWN: u32 = 0x0201;
const WM_LBUTTONUP: u32 = 0x0202;
const WM_CAPTURECHANGED: u32 = 0x0215;
const WS_CLIPCHILDREN: u32 = 0x0200_0000;
const MK_LBUTTON: usize = 1;
const HTCLIENT: u32 = 1;
const IDC_HAND: usize = 32649;
const EN_CHANGE: u32 = 0x0300;
const EM_SETSEL: u32 = 0x00B1;
const ICC_TREEVIEW_CLASSES: u32 = 0x2;
const SW_HIDE: i32 = 0;
/// Child windows: the bookmarks tree and the find box.
const ID_TREE: isize = 101;
const ID_FIND: isize = 102;
const TVS_HASBUTTONS: u32 = 0x1;
const TVS_HASLINES: u32 = 0x2;
const TVS_LINESATROOT: u32 = 0x4;
const TVS_SHOWSELALWAYS: u32 = 0x20;
const TVM_DELETEITEM: u32 = 0x1101;
const TVM_EXPAND: u32 = 0x1102;
const TVM_HITTEST: u32 = 0x1111;
const TVM_ENSUREVISIBLE: u32 = 0x1114;
const TVM_INSERTITEMW: u32 = 0x1132;
const TVM_GETITEMW: u32 = 0x113E;
const TVIF_TEXT: u32 = 0x1;
const TVIF_PARAM: u32 = 0x4;
const TVIF_HANDLE: u32 = 0x10;
const TVE_EXPAND: usize = 2;
const TVI_ROOT: isize = -0x10000;
const TVI_LAST: isize = -0xFFFE;
const TVHT_ON_ITEM: u32 = 0x2 | 0x4;
const NM_CLICK: u32 = -2i32 as u32;
const TVN_SELCHANGEDW: u32 = -451i32 as u32;
const TVC_BYMOUSE: u32 = 1;
const TVC_BYKEYBOARD: u32 = 2;
const CF_UNICODETEXT: u32 = 13;
const GMEM_MOVEABLE: u32 = 2;
const MB_YESNO: u32 = 4;
const MB_ICONQUESTION: u32 = 0x20;
const MB_DEFBUTTON2: u32 = 0x100;
const IDYES: i32 = 6;
const PD_PAGENUMS: u32 = 0x2;
const PD_NOSELECTION: u32 = 0x4;
const PD_RETURNDC: u32 = 0x100;
const PD_USEDEVMODECOPIESANDCOLLATE: u32 = 0x4_0000;
const PD_NOCURRENTPAGE: u32 = 0x80_0000;
const PD_RESULT_PRINT: u32 = 1;
const START_PAGE_GENERAL: u32 = 0xFFFF_FFFF;
const MAX_PRINT_RANGES: usize = 16;
const HORZRES: i32 = 8;
const VERTRES: i32 = 10;
const LOGPIXELSX: i32 = 88;
const COINIT_APARTMENTTHREADED: u32 = 2;
/// The most characters of the find box that are read.
const MAX_FIND_CHARS: usize = 2000;
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
    /// The bookmarks tree and the find box, children of the window (0 if Windows would not make them).
    tree: Cell<isize>,
    find: Cell<isize>,
    /// The printer a job is going to (from the print box, or named by the hidden test), until the job ends.
    printer: RefCell<Option<Printer>>,
    /// A menu is open (between WM_ENTERMENULOOP and WM_EXITMENULOOP), and the menus that came meanwhile, to be put when it closes:
    /// destroying the menu bar under an open menu would close it under the person's hand.
    menu_open: Cell<bool>,
    menus_waiting: RefCell<Option<Vec<Menu>>>,
}

/// A printer's device context, and the file it writes if it was told to write one (zero-terminated UTF-16).
struct Printer {
    dc: isize,
    output: Option<Vec<u16>>,
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
            Action::Confirm { tag, title, text } => {
                let (t, c) = (wide(&text), wide(&title));
                // SAFETY: both strings are zero-terminated UTF-16 that outlive the call; the box is modal and returns when it
                // is closed. "No" is the button the Enter key presses.
                let answer = unsafe { MessageBoxW(hwnd, t.as_ptr(), c.as_ptr(), MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2) };
                queue.extend(deliver(state, Event::Confirmed { tag, yes: answer == IDYES }));
            }
            Action::SetMenus(menus) => set_menus(state, hwnd, menus),
            Action::Outline(nodes) => fill_tree(state.tree.get(), &nodes),
            Action::Sidebar(r) => place_child(state.tree.get(), r),
            Action::Find(rect) => show_find(state, hwnd, rect),
            Action::OpenUri(uri) => {
                // The reader has checked the address; it is checked again here, the last place before the system, so that nothing
                // that is not a plain http, https or mailto address (an environment variable like %USERNAME%, say) reaches it.
                if qingpdf_core::view::safe_uri(uri.as_bytes()).is_some() {
                    let (verb, file) = (wide("open"), wide(&uri));
                    // SAFETY: both strings are zero-terminated UTF-16 that outlive the call; no parameters or folder (null); the
                    // system opens the address with the program the user has chosen for its kind. ShellExecuteW takes no flags (it
                    // is not given SEE_MASK_DOENVSUBST, which ShellExecuteExW would need to expand environment variables), and the
                    // address has no '%' that does not begin a percent-escape.
                    unsafe { ShellExecuteW(hwnd, verb.as_ptr(), file.as_ptr(), null(), null(), SW_SHOWNORMAL) };
                }
            }
            Action::SetClipboard(text) => {
                let ok = set_clipboard(hwnd, &text);
                queue.extend(deliver(state, Event::ClipboardSet(ok)));
            }
            Action::ChoosePrinter { max_page, preset } => {
                match choose_printer(state, hwnd, max_page, preset) {
                    Ok(setup) => queue.extend(deliver(state, Event::Printer(setup))),
                    // No printer, or the box would not open: the reader says so.
                    Err(()) => queue.extend(deliver(state, Event::PrintError(String::new()))),
                }
            }
            Action::Print(op) => match print_op(state, op) {
                Ok(true) => queue.extend(deliver(state, Event::PrintStepDone)),
                Ok(false) => {}
                Err(message) => queue.extend(deliver(state, Event::PrintError(message))),
            },
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
            } else if lparam == state.find.get() && hiword(wparam) == EN_CHANGE {
                let text = window_text(lparam, MAX_FIND_CHARS);
                dispatch(state, Event::FindText(text));
            }
            return 0;
        }
        WM_LBUTTONDOWN => {
            // SAFETY: plain handle of this window; the pointer is captured so that a drag outside the window is still heard.
            unsafe {
                SetFocus(hwnd);
                SetCapture(hwnd);
            }
            let (x, y) = mouse_place(lparam);
            dispatch(state, Event::MouseDown { x, y, ctrl: modifier(0x11), shift: modifier(0x10) });
            return 0;
        }
        WM_MOUSEMOVE => {
            if wparam & MK_LBUTTON != 0 {
                let (x, y) = mouse_place(lparam);
                dispatch(state, Event::MouseMove { x, y });
            }
            return 0;
        }
        WM_LBUTTONUP => {
            // The reader hears the button come up first: giving the mouse back makes Windows say that the window lost it
            // (WM_CAPTURECHANGED), and the reader would take that for a drag cut short.
            let (x, y) = mouse_place(lparam);
            dispatch(state, Event::MouseUp { x, y });
            // SAFETY: gives the mouse back; fine to call when it was not captured.
            unsafe { ReleaseCapture() };
            return 0;
        }
        WM_CAPTURECHANGED => {
            // Another window took the mouse while the button was down: the drag is over.
            dispatch(state, Event::MouseLost);
            return 0;
        }
        WM_SETCURSOR => {
            // Only over the pages themselves (the children have cursors of their own, and so has the frame).
            if wparam as isize == hwnd && loword(lparam as usize) == HTCLIENT {
                let mut p = Point::default();
                // SAFETY: `p` is a live POINT that Windows fills in and rewrites; both calls only read the pointer's place.
                unsafe {
                    GetCursorPos(&mut p);
                    ScreenToClient(hwnd, &mut p);
                }
                let shape = match state.handler.try_borrow_mut() {
                    Ok(mut guard) => guard.as_mut().map_or(Cursor::Arrow, |h| h.cursor_at(p.x, p.y)),
                    Err(_) => Cursor::Arrow,
                };
                // SAFETY: LoadCursorW with a stock cursor id (a small integer in the pointer slot) gives a shared cursor that
                // is never freed; SetCursor takes it.
                unsafe { SetCursor(LoadCursorW(0, (if shape == Cursor::Hand { IDC_HAND } else { IDC_ARROW }) as *const u16)) };
                return 1;
            }
        }
        WM_NOTIFY => {
            // SAFETY: for WM_NOTIFY, lparam points to the NMHDR (or the bigger notification that starts with one) of the
            // notification, valid for the duration of this message; it is only read here.
            let hdr = unsafe { (lparam as *const NmHdr).as_ref() };
            if let Some(hdr) = hdr
                && hdr.from != 0
                && hdr.from == state.tree.get()
            {
                tree_notified(state, hwnd, hdr, lparam);
            }
            return 0;
        }
        WM_CLOSE => {
            // The reader keeps where it was before the window goes; then the default procedure closes it.
            dispatch(state, Event::Closing);
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
        WM_ENTERMENULOOP => {
            state.menu_open.set(true);
            return 0;
        }
        WM_EXITMENULOOP => {
            state.menu_open.set(false);
            let waiting = state.menus_waiting.borrow_mut().take();
            if let Some(menus) = waiting {
                set_menus(state, hwnd, menus);
            }
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

/// The place of the pointer in a mouse message (the low and high words of `lparam` are signed 16-bit numbers).
fn mouse_place(lparam: isize) -> (i32, i32) {
    (i32::from(loword(lparam as usize) as u16 as i16), i32::from(hiword(lparam as usize) as u16 as i16))
}

/// The text of a window (an edit box), at most `max` characters of it.
fn window_text(hwnd: isize, max: usize) -> String {
    // SAFETY: reads the length of the text of a window; a bad handle makes it 0.
    let n = unsafe { GetWindowTextLengthW(hwnd) }.max(0) as usize;
    let n = n.min(max);
    let mut buf = vec![0u16; n + 1];
    // SAFETY: `buf` has room for `n + 1` characters, which is what the call is told (it cuts the text there and ends it with a zero).
    let got = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) }.max(0) as usize;
    buf.truncate(got);
    String::from_utf16_lossy(&buf)
}

/// A notification from the bookmarks tree: a click on an item (a new item is selected by the click, and the same one can be clicked again),
/// or the selection moved by the keyboard.
fn tree_notified(state: &WindowState, hwnd: isize, hdr: &NmHdr, lparam: isize) {
    let tree = state.tree.get();
    match hdr.code {
        NM_CLICK => {
            let mut hit = TvHitTest { pt: Point::default(), flags: 0, item: 0 };
            // SAFETY: `hit.pt` is a live POINT filled in and rewritten in place.
            unsafe {
                GetCursorPos(&mut hit.pt);
                ScreenToClient(tree, &mut hit.pt);
            }
            // SAFETY: TVM_HITTEST takes a pointer to a live TVHITTESTINFO, which it fills in and does not keep.
            unsafe { SendMessageW(tree, TVM_HITTEST, 0, (&raw mut hit) as isize) };
            if hit.item != 0 && hit.flags & TVHT_ON_ITEM != 0 {
                let mut item = tv_item(TVIF_PARAM | TVIF_HANDLE, null_mut(), -1);
                item.item = hit.item;
                // SAFETY: TVM_GETITEMW takes a pointer to a live TVITEMW and fills in the fields the mask names; nothing is kept.
                unsafe { SendMessageW(tree, TVM_GETITEMW, 0, (&raw mut item) as isize) };
                if let Ok(index) = usize::try_from(item.param) {
                    dispatch(state, Event::OutlineClick(index));
                    // The pages get the keyboard back, so that the keys scroll them.
                    // SAFETY: plain handle of the main window.
                    unsafe { SetFocus(hwnd) };
                }
            }
        }
        TVN_SELCHANGEDW => {
            // SAFETY: for TVN_SELCHANGEDW, lparam points to an NMTREEVIEWW valid for the duration of this message; it is only read.
            let nm = unsafe { (lparam as *const NmTreeView).as_ref() };
            if let Some(nm) = nm
                && (nm.action == TVC_BYKEYBOARD || nm.action == TVC_BYMOUSE)
                && let Ok(index) = usize::try_from(nm.new.param)
            {
                dispatch(state, Event::OutlineClick(index));
            }
        }
        _ => {}
    }
}

fn tv_item(mask: u32, text: *mut u16, param: isize) -> TvItem {
    TvItem { mask, item: 0, state: 0, state_mask: 0, text, text_max: 0, image: 0, selected_image: 0, children: 0, param }
}

/// Fill the tree with the bookmarks (what was in it goes).
fn fill_tree(tree: isize, nodes: &[OutlineNode]) {
    if tree == 0 {
        return;
    }
    // SAFETY: plain messages to the tree control: no redraw while it is filled, and all its items deleted.
    unsafe {
        SendMessageW(tree, WM_SETREDRAW, 0, 0);
        SendMessageW(tree, TVM_DELETEITEM, 0, TVI_ROOT);
    }
    // The handle of the latest item at each depth of the branch being filled; an item goes under the latest one a level up,
    // and after the latest one of its own level (that is quick, where "last" would walk the whole row).
    let mut path: Vec<isize> = Vec::new();
    let mut to_open: Vec<isize> = Vec::new();
    let mut first: isize = 0;
    for (i, node) in nodes.iter().enumerate() {
        let depth = (node.depth as usize).min(path.len());
        let parent = if depth == 0 { TVI_ROOT } else { path.get(depth - 1).copied().unwrap_or(TVI_ROOT) };
        let after = path.get(depth).copied().unwrap_or(TVI_LAST);
        let mut text = wide(&node.title);
        let mut insert = TvInsert { parent, after, item: TvItemEx { item: tv_item(TVIF_TEXT | TVIF_PARAM, text.as_mut_ptr(), i as isize), integral: 0, state_ex: 0, window: 0, expanded_image: 0, reserved: 0 } };
        // SAFETY: TVM_INSERTITEMW takes a pointer to a live TVINSERTSTRUCTW (here with the bigger extended item) whose text
        // pointer is to a zero-terminated string that outlives the call; the control copies what it needs. 0 means failure.
        let handle = unsafe { SendMessageW(tree, TVM_INSERTITEMW, 0, (&raw mut insert) as isize) };
        if handle == 0 {
            continue;
        }
        if first == 0 {
            first = handle;
        }
        path.truncate(depth);
        path.push(handle);
        if node.open && node.depth < 2 {
            to_open.push(handle);
        }
    }
    // SAFETY: plain messages to the tree control with handles it gave: open the items the file shows open, show the first one,
    // and let it draw again.
    unsafe {
        for h in to_open {
            SendMessageW(tree, TVM_EXPAND, TVE_EXPAND, h);
        }
        if first != 0 {
            SendMessageW(tree, TVM_ENSUREVISIBLE, 0, first);
        }
        SendMessageW(tree, WM_SETREDRAW, 1, 0);
    }
    invalidate(tree);
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
    for child in [state.tree.get(), state.find.get()] {
        if child != 0 && state.font.get() != 0 {
            // SAFETY: WM_SETFONT takes a font handle (the one just made) and a redraw flag; `child` is a window of ours.
            unsafe { SendMessageW(child, WM_SETFONT, state.font.get() as usize, 1) };
        }
    }
    if old != 0 {
        // SAFETY: `old` was a font made by `make_font`; it is no longer selected into a device context or set as a control's font.
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

/// A memory device context of one pixel, to lay a colour over the picture with (made the first time it is needed).
struct Tint {
    dc: isize,
    bitmap: isize,
    old: isize,
}

/// Draws on a memory device context.
struct Gdi {
    dc: isize,
    line: i32,
    tint: Option<Tint>,
}

impl Drop for Gdi {
    fn drop(&mut self) {
        if let Some(t) = self.tint.take() {
            // SAFETY: the DC and bitmap were made by `fill_rect_alpha` and are only here; the bitmap is taken out of the DC
            // (the old one selected back) before it is deleted, and the DC after.
            unsafe {
                SelectObject(t.dc, t.old);
                DeleteObject(t.bitmap);
                DeleteDC(t.dc);
            }
        }
    }
}

impl Painter for Gdi {
    fn fill_rect_alpha(&mut self, r: Rect, color: Rgb, alpha: u8) {
        if r.w <= 0 || r.h <= 0 || alpha == 0 {
            return;
        }
        if alpha == 255 {
            self.fill_rect(r, color);
            return;
        }
        if self.tint.is_none() {
            // SAFETY: makes a DC and a one-pixel bitmap compatible with the back buffer's; each handle is checked, and what
            // was made is freed at once if the other failed.
            unsafe {
                let dc = CreateCompatibleDC(self.dc);
                let bitmap = if dc != 0 { CreateCompatibleBitmap(self.dc, 1, 1) } else { 0 };
                if dc != 0 && bitmap != 0 {
                    let old = SelectObject(dc, bitmap);
                    self.tint = Some(Tint { dc, bitmap, old });
                } else {
                    if bitmap != 0 {
                        DeleteObject(bitmap);
                    }
                    if dc != 0 {
                        DeleteDC(dc);
                    }
                }
            }
        }
        let Some(t) = &self.tint else {
            return;
        };
        let one = WinRect { left: 0, top: 0, right: 1, bottom: 1 };
        // SAFETY: `t.dc` holds the one-pixel bitmap; the brush is made, checked and deleted here; AlphaBlend stretches that pixel
        // over the rectangle with the constant alpha asked for (the blend function is packed: operation 0 = AC_SRC_OVER, no flags,
        // the alpha in the third byte, no per-pixel alpha). Windows keeps nothing.
        unsafe {
            let brush = CreateSolidBrush(colorref(color));
            if brush != 0 {
                FillRect(t.dc, &one, brush);
                DeleteObject(brush);
                AlphaBlend(self.dc, r.x, r.y, r.w, r.h, t.dc, 0, 0, 1, 1, u32::from(alpha) << 16);
            }
        }
    }

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
    let mut gdi = Gdi { dc: back.dc, line: line_height(back.dc), tint: None };
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

// --- menus, child windows, the clipboard ------------------------------------------------------------------------

/// A popup menu with these entries (submenus inside it are made too), or 0.
fn build_popup(entries: &[MenuEntry]) -> isize {
    // SAFETY: creates an empty popup menu; the handle is checked.
    let popup = unsafe { CreatePopupMenu() };
    if popup == 0 {
        return 0;
    }
    for entry in entries {
        match entry {
            MenuEntry::Item { id, label, checked, enabled } => {
                let w = wide(label);
                let flags = MF_STRING | if *checked { MF_CHECKED } else { 0 } | if *enabled { 0 } else { MF_GRAYED };
                // SAFETY: `w` is a zero-terminated UTF-16 string that outlives the call (Windows copies it).
                unsafe { AppendMenuW(popup, flags, *id as usize, w.as_ptr()) };
            }
            MenuEntry::Submenu { title, entries } => {
                let sub = build_popup(entries);
                if sub != 0 {
                    let w = wide(title);
                    // SAFETY: `sub` becomes a submenu of `popup` (and is destroyed with it); `w` outlives the call.
                    unsafe { AppendMenuW(popup, MF_POPUP, sub as usize, w.as_ptr()) };
                }
            }
            // SAFETY: a separator takes no text; the arguments are plain.
            MenuEntry::Separator => unsafe {
                AppendMenuW(popup, MF_SEPARATOR, 0, null());
            },
        }
    }
    popup
}

fn build_menu(menus: &[Menu]) -> isize {
    // SAFETY: creates an empty menu bar; the handle is checked below.
    let bar = unsafe { CreateMenu() };
    if bar == 0 {
        return 0;
    }
    for menu in menus {
        let popup = build_popup(&menu.entries);
        if popup == 0 {
            continue;
        }
        let w = wide(&menu.title);
        // SAFETY: `popup` becomes a submenu of `bar` (and is destroyed with it); `w` outlives the call.
        unsafe { AppendMenuW(bar, MF_POPUP, popup as usize, w.as_ptr()) };
    }
    bar
}

/// Put these menus on the window in place of the ones it has; while a menu is open, menus that have changed wait until it is
/// closed (the newest wins).
fn set_menus(state: &WindowState, hwnd: isize, menus: Vec<Menu>) {
    if state.menu_open.get() {
        *state.menus_waiting.borrow_mut() = Some(menus);
        return;
    }
    let menu = build_menu(&menus);
    if menu == 0 {
        return;
    }
    // SAFETY: `menu` is a menu bar just made; the window takes it over, and the one it had (a menu of ours) is destroyed after
    // it is replaced, with everything under it.
    unsafe {
        let old = GetMenu(hwnd);
        SetMenu(hwnd, menu);
        if old != 0 {
            DestroyMenu(old);
        }
        DrawMenuBar(hwnd);
    }
}

/// Move a child window to a rectangle of the client area and show it, or hide it when the rectangle has no area.
fn place_child(child: isize, r: Rect) {
    if child == 0 {
        return;
    }
    // SAFETY: plain handle of a child window of ours and plain numbers.
    unsafe {
        if r.w > 0 && r.h > 0 {
            MoveWindow(child, r.x, r.y, r.w, r.h, 1);
            ShowWindow(child, SW_SHOW);
        } else {
            ShowWindow(child, SW_HIDE);
        }
    }
}

/// Show the find box at a place and give it the keyboard (with its text selected), or hide it and take the keyboard back.
fn show_find(state: &WindowState, hwnd: isize, rect: Option<Rect>) {
    let edit = state.find.get();
    if edit == 0 {
        return;
    }
    match rect {
        Some(r) => {
            let had_focus = {
                // SAFETY: reads which window has the keyboard.
                unsafe { GetFocus() == edit }
            };
            place_child(edit, r);
            if !had_focus {
                // SAFETY: plain handle of the edit box; the whole text is selected (a start of 0 and an end of -1).
                unsafe {
                    SetFocus(edit);
                    SendMessageW(edit, EM_SETSEL, 0, -1);
                }
            }
        }
        None => {
            // SAFETY: reads which window has the keyboard, and gives it to the main window if it was the edit box.
            unsafe {
                if GetFocus() == edit {
                    SetFocus(hwnd);
                }
            }
            place_child(edit, Rect { x: 0, y: 0, w: 0, h: 0 });
        }
    }
}

/// Put text on the clipboard (as UTF-16, the line ends as the system writes them). `false`: it could not be done (the memory
/// could not be had, or another program holds the clipboard); what was on the clipboard before stays then.
fn set_clipboard(hwnd: isize, text: &str) -> bool {
    let mut w: Vec<u16> = Vec::with_capacity(text.len() + 1);
    for c in text.replace("\r\n", "\n").replace('\n', "\r\n").encode_utf16() {
        w.push(c);
    }
    w.push(0);
    let bytes = w.len() * 2;
    // SAFETY: the memory block is allocated moveable, locked for the copy of exactly `w.len()` characters it was sized for, and
    // unlocked, all before the clipboard is touched, so that a failure to get memory leaves the old contents alone. The clipboard
    // is opened for this window and closed on every path after it was opened. If the clipboard takes the block (SetClipboardData
    // succeeds) it belongs to the system, otherwise it is freed here. Every handle and pointer is checked.
    unsafe {
        let mem = GlobalAlloc(GMEM_MOVEABLE, bytes);
        if mem == 0 {
            return false;
        }
        let p = GlobalLock(mem).cast::<u16>();
        if p.is_null() {
            GlobalFree(mem);
            return false;
        }
        std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
        GlobalUnlock(mem);
        if OpenClipboard(hwnd) == 0 {
            GlobalFree(mem);
            return false;
        }
        EmptyClipboard();
        let taken = SetClipboardData(CF_UNICODETEXT, mem) != 0;
        if !taken {
            GlobalFree(mem);
        }
        CloseClipboard();
        taken
    }
}

// --- printing ----------------------------------------------------------------------------------------------------

/// What a printer's device context can print on, and the pages asked for.
fn printer_setup(dc: isize, name: String, ranges: Vec<(u32, u32)>) -> PrinterSetup {
    // SAFETY: reads three capabilities of a printer device context that is valid (made by the caller and checked).
    let (dpi, width, height) = unsafe { (GetDeviceCaps(dc, LOGPIXELSX), GetDeviceCaps(dc, HORZRES), GetDeviceCaps(dc, VERTRES)) };
    PrinterSetup { name, dpi, width, height, ranges }
}

/// End the job that is going (if any) and let go of the printer: stopped (`abort`) or finished.
fn end_printer(state: &WindowState, abort: bool) {
    let Some(p) = state.printer.borrow_mut().take() else { return };
    // SAFETY: `p.dc` is a printer device context made by `choose_printer` and used nowhere else; the job is ended or stopped
    // (both are harmless when none was started) and the context deleted once.
    unsafe {
        if abort {
            AbortDoc(p.dc);
        } else {
            EndDoc(p.dc);
        }
        DeleteDC(p.dc);
    }
}

/// Which printer to print to: the one named by the hidden test (no box), or the one the person chooses in the system's print
/// box (with the pages and copies). `None` if there is none, or the person cancelled.
fn choose_printer(state: &WindowState, hwnd: isize, max_page: u32, preset: Option<PrinterPreset>) -> Result<Option<PrinterSetup>, ()> {
    end_printer(state, true);
    if let Some(preset) = preset {
        let (driver, name) = (wide("WINSPOOL"), wide(&preset.name));
        // SAFETY: both strings are zero-terminated UTF-16 that outlive the call; no port and no device mode (null). 0 is failure.
        let dc = unsafe { CreateDCW(driver.as_ptr(), name.as_ptr(), null(), null()) };
        if dc == 0 {
            return Err(());
        }
        let mut output: Vec<u16> = preset.output.as_os_str().encode_wide().collect();
        output.push(0);
        *state.printer.borrow_mut() = Some(Printer { dc, output: Some(output) });
        return Ok(Some(printer_setup(dc, preset.name, preset.ranges)));
    }
    let mut ranges = [PrintPageRange { from: 1, to: max_page.max(1) }; MAX_PRINT_RANGES];
    let mut pd = PrintDlgEx {
        size: std::mem::size_of::<PrintDlgEx>() as u32,
        owner: hwnd,
        devmode: 0,
        devnames: 0,
        dc: 0,
        flags: PD_RETURNDC | PD_USEDEVMODECOPIESANDCOLLATE | PD_NOSELECTION | PD_NOCURRENTPAGE,
        flags2: 0,
        exclusion_flags: 0,
        page_range_count: 1,
        page_range_max: MAX_PRINT_RANGES as u32,
        page_ranges: ranges.as_mut_ptr(),
        min_page: 1,
        max_page: max_page.max(1),
        copies: 1,
        instance: 0,
        template_name: null(),
        callback: null(),
        property_page_count: 0,
        property_pages: null(),
        start_page: START_PAGE_GENERAL,
        result_action: 0,
    };
    // SAFETY: `pd` is a live PRINTDLGEXW of the size it declares; the page range array it points to has the room it says and
    // outlives the call; the box is modal and returns when closed (0 is S_OK). Windows keeps none of the pointers.
    let hr = unsafe { PrintDlgExW(&mut pd) };
    // SAFETY: the device mode and device names the box returns are global memory blocks we own and no longer need.
    unsafe {
        if pd.devmode != 0 {
            GlobalFree(pd.devmode);
        }
        if pd.devnames != 0 {
            GlobalFree(pd.devnames);
        }
    }
    if hr != 0 {
        // The box did not open (there is no printer, say).
        return Err(());
    }
    if pd.result_action != PD_RESULT_PRINT || pd.dc == 0 {
        if pd.dc != 0 {
            // SAFETY: the context the box made and returned is ours to delete.
            unsafe { DeleteDC(pd.dc) };
        }
        return Ok(None);
    }
    let chosen: Vec<(u32, u32)> = if pd.flags & PD_PAGENUMS != 0 {
        let n = (pd.page_range_count as usize).min(MAX_PRINT_RANGES);
        ranges.iter().take(n).map(|r| (r.from, r.to)).collect()
    } else {
        Vec::new()
    };
    *state.printer.borrow_mut() = Some(Printer { dc: pd.dc, output: None });
    Ok(Some(printer_setup(pd.dc, String::new(), chosen)))
}

/// One step of a print job. `Ok(true)`: a band was put on the page. `Err`: the printer refused.
fn print_op(state: &WindowState, op: PrintOp) -> Result<bool, String> {
    if let PrintOp::End | PrintOp::Abort = op {
        end_printer(state, matches!(op, PrintOp::Abort));
        return Ok(false);
    }
    let slot = state.printer.borrow();
    // A job that was stopped is not carried on.
    let Some(printer) = slot.as_ref() else { return Ok(false) };
    let dc = printer.dc;
    match op {
        PrintOp::Start { name } => {
            let doc = wide(&name);
            let info = DocInfo {
                size: std::mem::size_of::<DocInfo>() as i32,
                doc_name: doc.as_ptr(),
                output: printer.output.as_ref().map_or(null(), |o| o.as_ptr()),
                datatype: null(),
                kind: 0,
            };
            // SAFETY: `info` is a live DOCINFOW of the size it declares; the strings it points to (the name, and the output file of
            // a printer that writes one) are zero-terminated UTF-16 that outlive the call; `dc` is the valid printer context.
            // A result of 0 or less is failure.
            if unsafe { StartDocW(dc, &info) } <= 0 {
                return Err("the printer would not start the job".to_string());
            }
            Ok(false)
        }
        PrintOp::StartPage => {
            // SAFETY: `dc` is the valid printer context of a job that was started.
            if unsafe { StartPage(dc) } <= 0 {
                return Err("the printer would not start a page".to_string());
            }
            Ok(false)
        }
        PrintOp::Band { x, y, w, h, src_w, src_h, bgra } => {
            let needed = (src_w.max(0) as usize).saturating_mul(src_h.max(0) as usize).saturating_mul(4);
            if src_w <= 0 || src_h <= 0 || w <= 0 || h <= 0 || bgra.len() < needed {
                return Ok(true);
            }
            let header = BitmapInfoHeader {
                size: std::mem::size_of::<BitmapInfoHeader>() as u32,
                width: src_w,
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
            // SAFETY: `bgra` has at least `src_w * src_h * 4` bytes (checked above), all the call reads of the pixels; `header` is a
            // live BITMAPINFOHEADER for a 32-bit uncompressed top-down picture, which needs no colour table; `dc` is the valid
            // printer context. Windows keeps neither pointer. A result of 0 is failure.
            let lines = unsafe {
                SetStretchBltMode(dc, COLORONCOLOR);
                StretchDIBits(dc, x, y, w, h, 0, 0, src_w, src_h, bgra.as_ptr().cast(), &header, 0, SRCCOPY)
            };
            if lines == 0 {
                return Err("the printer would not take a part of the page".to_string());
            }
            Ok(true)
        }
        PrintOp::EndPage => {
            // SAFETY: `dc` is the valid printer context with a page started.
            if unsafe { EndPage(dc) } <= 0 {
                return Err("the printer would not end a page".to_string());
            }
            Ok(false)
        }
        PrintOp::End | PrintOp::Abort => Ok(false),
    }
}

// --- starting up -------------------------------------------------------------------------------------------------

/// Make the window and run it until it is closed. `make` is given the means to wake the window from another thread
/// and builds the reader. `client`: a size for the window's client area, in pixels (the speed test uses it; it may be bigger
/// than the screen); otherwise the window is most of the work area.
pub fn run(title: &str, menus: &[Menu], client: Option<(i32, i32)>, make: impl FnOnce(Waker) -> Box<dyn Handler>) -> i32 {
    let icc = InitCommonControlsEx { size: std::mem::size_of::<InitCommonControlsEx>() as u32, icc: ICC_STANDARD_CLASSES | ICC_TREEVIEW_CLASSES };
    // SAFETY: `icc` is a live struct of the size it declares; this makes the buttons and boxes use the new look.
    unsafe { InitCommonControlsEx(&icc) };
    // SAFETY: the print box needs COM on its thread (a single-threaded apartment); a failure only means the box may not open.
    unsafe { CoInitializeEx(null(), COINIT_APARTMENTTHREADED) };
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
    let style = WS_OVERLAPPEDWINDOW | WS_VSCROLL | WS_HSCROLL | WS_CLIPCHILDREN;
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
    let state = Box::new(WindowState { hwnd: Cell::new(hwnd), closing: Cell::new(false), handler: RefCell::new(None), back: RefCell::new(Back::default()), font: Cell::new(0), dpi: Cell::new(96), tree: Cell::new(0), find: Cell::new(0), printer: RefCell::new(None), menu_open: Cell::new(false), menus_waiting: RefCell::new(None) });
    let raw = Box::into_raw(state);
    // SAFETY: stores the pointer from `Box::into_raw`; it is freed once, by `free_state` below, after the message loop.
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, raw as isize) };
    let Some(state) = state_of(hwnd) else { return 1 };
    // The bookmarks tree and the find box: children of the window, hidden until the reader says where to put them.
    state.tree.set(make_child(hwnd, instance, "SysTreeView32", WS_EX_CLIENTEDGE, WS_TABSTOP | TVS_HASBUTTONS | TVS_HASLINES | TVS_LINESATROOT | TVS_SHOWSELALWAYS, ID_TREE));
    state.find.set(make_child(hwnd, instance, "EDIT", WS_EX_CLIENTEDGE, WS_TABSTOP | ES_AUTOHSCROLL, ID_FIND));
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
        // Some keys pressed in the find box or the tree are for the reader (Enter and Esc in the box, F3, Ctrl+F ...).
        if msg.message == WM_KEYDOWN && forward_key(state, &msg) {
            continue;
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

/// A child window (a control) of the main window, not shown, or 0 if Windows would not make it.
fn make_child(parent: isize, instance: isize, class: &str, ex_style: u32, style: u32, id: isize) -> isize {
    let (c, t) = (wide(class), wide(""));
    // SAFETY: the strings are zero-terminated UTF-16 that outlive the call; `parent` is the main window; the control's id goes in the
    // menu slot as Windows defines for child windows; no creation parameter. 0 means failure and is checked by the caller.
    unsafe { CreateWindowExW(ex_style, c.as_ptr(), t.as_ptr(), WS_CHILD | style, 0, 0, 0, 0, parent, id, instance, null_mut()) }
}

/// A key pressed while the find box or the tree had the keyboard: is it one the reader wants? Then it is given to the reader
/// (and the box or tree never sees it). Enter and Esc in the box, Esc and F3 and F4 anywhere, and Ctrl with a letter or digit
/// the reader uses (not the editing keys of the box: Ctrl+C, V, X, A, Z).
fn forward_key(state: &WindowState, msg: &Msg) -> bool {
    let (tree, find) = (state.tree.get(), state.find.get());
    if msg.hwnd == 0 || (msg.hwnd != tree && msg.hwnd != find) {
        return false;
    }
    let in_find = msg.hwnd == find;
    let vk = msg.wparam as u32;
    let (ctrl, shift) = (modifier(0x11), modifier(0x10));
    let wanted = match vk {
        0x0D => in_find,
        0x1B | 0x72 | 0x73 => true,
        _ if ctrl => !(in_find && matches!(vk, 0x43 | 0x56 | 0x58 | 0x41 | 0x5A)),
        _ => false,
    };
    if wanted {
        dispatch(state, Event::Key { vk, ctrl, shift });
    }
    wanted
}

/// Free the window state made by `run`: the reader (and with it the engine), the back buffer and the font.
fn free_state(raw: *mut WindowState) {
    // SAFETY: `raw` is the pointer from `Box::into_raw` in `run`; this is the only place it is turned back into a box, called
    // once, after the message loop (the only thing that gives the state to window procedures) has ended. The user-data slot
    // was cleared at WM_NCDESTROY, or the window is destroyed with the process.
    let state = unsafe { Box::from_raw(raw) };
    // A print job that is still going (the window was closed during it) is stopped and the printer let go of.
    end_printer(&state, true);
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
