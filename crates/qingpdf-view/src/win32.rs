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

use crate::clicks::ClickCounter;
use crate::copydata;
use crate::placement::{self, Placement};
use crate::cmdbar;
use crate::theme::Contrast;
use qingpdf_core::bookmarks::Place;

use crate::treedrop;
use crate::ui::{Action, Cursor, Event, FieldBox, FieldExit, Font, FormAnswer, FormKind, FormSpec, Handler, Menu, MenuEntry, NcHit, OutlineNode, Painter, PrintOp, PrinterPreset, PrinterSetup, Rect, Rgb, SaveChoice, SearchBox, ThemeSpec};
use tiny_skia::{FillRule, LineCap, LineJoin, Paint, Path, PathBuilder, PixmapMut, Stroke, Transform};

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

/// `COPYDATASTRUCT`: a number of the sender's choice, and some bytes.
#[repr(C)]
struct CopyData {
    id: usize,
    size: u32,
    data: *const c_void,
}

/// `TRACKMOUSEEVENT`.
#[repr(C)]
struct TrackMouse {
    size: u32,
    flags: u32,
    hwnd: isize,
    hover_time: u32,
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
struct MonitorInfo {
    size: u32,
    monitor: WinRect,
    work: WinRect,
    flags: u32,
}

/// A window's place (WINDOWPLACEMENT): how it is shown, where it is when it is minimized or maximized, and where it is when it is neither.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct WindowPlacement {
    length: u32,
    flags: u32,
    show: u32,
    min: Point,
    max: Point,
    normal: WinRect,
}

/// `MARGINS`: how far the frame of a window is extended into its client area.
#[repr(C)]
struct Margins {
    left: i32,
    right: i32,
    top: i32,
    bottom: i32,
}

/// `NCCALCSIZE_PARAMS`: the rectangles of WM_NCCALCSIZE (the first is the one that is changed) and a pointer that is not used.
#[repr(C)]
struct NcCalcSize {
    rects: [WinRect; 3],
    pos: *mut c_void,
}

/// `HIGHCONTRASTW`.
#[repr(C)]
struct HighContrastW {
    size: u32,
    flags: u32,
    scheme: *mut u16,
}

#[repr(C)]
#[derive(Default)]
struct LocalTime {
    year: u16,
    month: u16,
    weekday: u16,
    day: u16,
    rest: [u16; 4],
}

/// BROWSEINFOW.
#[repr(C)]
struct BrowseInfo {
    owner: isize,
    root: *const c_void,
    display_name: *mut u16,
    title: *const u16,
    flags: u32,
    callback: *const c_void,
    param: isize,
    image: i32,
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

/// NMTVDISPINFOW: what the tree says when a title starts or stops being typed over.
#[repr(C)]
struct NmTvDispInfo {
    hdr: NmHdr,
    item: TvItem,
}

/// Where a drop of an item of the tree would go: the item it is in relation to (its place in the reader's list), where in relation to it,
/// and the item of the tree the mark is put on.
#[derive(Clone, Copy, PartialEq)]
struct DropMark {
    to: usize,
    place: Place,
    on: isize,
}

/// An item of the tree that is being dragged: its handle and its place in the reader's list, and where it would land.
#[derive(Clone, Copy)]
struct TreeDrag {
    item: isize,
    index: usize,
    mark: Option<DropMark>,
    /// Where the pointer was last, in the tree's client area (the drag is followed by the messages the window gets, not by asking where the
    /// pointer is now).
    at: Point,
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
    fn SetTimer(hwnd: isize, id: usize, ms: u32, proc_: *const c_void) -> usize;
    fn KillTimer(hwnd: isize, id: usize) -> i32;
    fn LoadCursorW(instance: isize, name: *const u16) -> isize;
    fn GetKeyState(key: i32) -> i16;
    fn MessageBoxW(hwnd: isize, text: *const u16, caption: *const u16, kind: u32) -> i32;
    fn EnableWindow(hwnd: isize, enable: i32) -> i32;
    fn SetWindowLongPtrW(hwnd: isize, index: i32, value: isize) -> isize;
    fn GetWindowLongPtrW(hwnd: isize, index: i32) -> isize;
    fn CreatePopupMenu() -> isize;
    fn AppendMenuW(menu: isize, flags: u32, id: usize, text: *const u16) -> i32;
    fn GetDpiForWindow(hwnd: isize) -> u32;
    fn GetDpiForSystem() -> u32;
    fn AdjustWindowRectExForDpi(rect: *mut WinRect, style: u32, menu: i32, ex_style: u32, dpi: u32) -> i32;
    fn SetWindowPos(hwnd: isize, after: isize, x: i32, y: i32, w: i32, h: i32, flags: u32) -> i32;
    fn SetFocus(hwnd: isize) -> isize;
    fn SetForegroundWindow(hwnd: isize) -> i32;
    fn IsDialogMessageW(dialog: isize, msg: *mut Msg) -> i32;
    fn FillRect(hdc: isize, rect: *const WinRect, brush: isize) -> i32;
    fn ScreenToClient(hwnd: isize, point: *mut Point) -> i32;
    fn ClientToScreen(hwnd: isize, point: *mut Point) -> i32;
    fn TrackPopupMenu(menu: isize, flags: u32, x: i32, y: i32, reserved: i32, hwnd: isize, rect: *const WinRect) -> i32;
    fn SystemParametersInfoW(action: u32, param: u32, data: *mut c_void, flags: u32) -> i32;
    fn GetFocus() -> isize;
    fn GetCursorPos(point: *mut Point) -> i32;
    fn GetMessagePos() -> u32;
    fn SetCursor(cursor: isize) -> isize;
    fn SetCapture(hwnd: isize) -> isize;
    fn ReleaseCapture() -> i32;
    fn OpenClipboard(hwnd: isize) -> i32;
    fn CloseClipboard() -> i32;
    fn EmptyClipboard() -> i32;
    fn SetClipboardData(format: u32, mem: isize) -> isize;
    fn DestroyMenu(menu: isize) -> i32;
    fn MoveWindow(hwnd: isize, x: i32, y: i32, w: i32, h: i32, repaint: i32) -> i32;
    fn ShutdownBlockReasonCreate(hwnd: isize, reason: *const u16) -> i32;
    fn ShutdownBlockReasonDestroy(hwnd: isize) -> i32;
    fn GetSystemMetricsForDpi(index: i32, dpi: u32) -> i32;
    fn GetSystemMetrics(index: i32) -> i32;
    fn GetDoubleClickTime() -> u32;
    fn GetMessageTime() -> i32;
    fn LoadImageW(instance: isize, name: *const u16, kind: u32, cx: i32, cy: i32, flags: u32) -> isize;
    fn GetSysColorBrush(index: i32) -> isize;
    fn WindowFromDC(hdc: isize) -> isize;
    fn EnumDisplayMonitors(hdc: isize, clip: *const WinRect, callback: unsafe extern "system" fn(isize, isize, *const WinRect, isize) -> i32, data: isize) -> i32;
    fn GetMonitorInfoW(monitor: isize, info: *mut MonitorInfo) -> i32;
    fn IsZoomed(hwnd: isize) -> i32;
    fn IsIconic(hwnd: isize) -> i32;
    fn FindWindowW(class: *const u16, title: *const u16) -> isize;
    fn SendMessageTimeoutW(hwnd: isize, msg: u32, wparam: usize, lparam: isize, flags: u32, timeout: u32, result: *mut usize) -> isize;
    fn GetWindowThreadProcessId(hwnd: isize, pid: *mut u32) -> u32;
    fn AllowSetForegroundWindow(pid: u32) -> i32;
    fn TrackMouseEvent(track: *mut TrackMouse) -> i32;
    fn GetWindowPlacement(hwnd: isize, place: *mut WindowPlacement) -> i32;
    fn SetWindowPlacement(hwnd: isize, place: *const WindowPlacement) -> i32;
    fn MonitorFromWindow(hwnd: isize, flags: u32) -> isize;
    fn RedrawWindow(hwnd: isize, rect: *const WinRect, region: isize, flags: u32) -> i32;
    fn GetSysColor(index: i32) -> u32;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn CreateCompatibleDC(hdc: isize) -> isize;
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
    fn CreateDIBSection(hdc: isize, info: *const BitmapInfoHeader, usage: u32, bits: *mut *mut c_void, section: isize, offset: u32) -> isize;
    fn GdiFlush() -> i32;
    fn GetClipBox(hdc: isize, rect: *mut WinRect) -> i32;
    fn GetTextFaceW(hdc: isize, count: i32, buffer: *mut u16) -> i32;
    fn SetBkColor(hdc: isize, color: u32) -> u32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> isize;
    fn CreateMutexW(attributes: *const c_void, owner: i32, name: *const u16) -> isize;
    fn GetLastError() -> u32;
    fn GetLocalTime(time: *mut LocalTime);
    fn SystemTimeToTzSpecificLocalTime(zone: *const c_void, utc: *const LocalTime, local: *mut LocalTime) -> i32;
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
    fn CoTaskMemFree(memory: *mut c_void);
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn DragAcceptFiles(hwnd: isize, accept: i32);
    fn DragQueryFileW(drop: isize, index: u32, buffer: *mut u16, len: u32) -> u32;
    fn DragFinish(drop: isize);
    fn ShellExecuteW(hwnd: isize, verb: *const u16, file: *const u16, params: *const u16, dir: *const u16, show: i32) -> isize;
    fn SHBrowseForFolderW(info: *mut BrowseInfo) -> *mut c_void;
    fn SHGetPathFromIDListW(list: *const c_void, path: *mut u16) -> i32;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn GetUserNameW(buffer: *mut u16, size: *mut u32) -> i32;
    /// Reads one value of the registry (only read: nothing in it is ever written).
    fn RegGetValueW(key: isize, sub_key: *const u16, value: *const u16, flags: u32, kind: *mut u32, data: *mut c_void, size: *mut u32) -> i32;
}

#[link(name = "dwmapi")]
unsafe extern "system" {
    fn DwmSetWindowAttribute(hwnd: isize, attribute: u32, value: *const c_void, size: u32) -> i32;
    fn DwmGetWindowAttribute(hwnd: isize, attribute: u32, value: *mut c_void, size: u32) -> i32;
    fn DwmExtendFrameIntoClientArea(hwnd: isize, margins: *const Margins) -> i32;
    fn DwmDefWindowProc(hwnd: isize, msg: u32, wparam: usize, lparam: isize, result: *mut isize) -> i32;
    fn DwmGetColorizationColor(color: *mut u32, opaque: *mut i32) -> i32;
}

#[link(name = "uxtheme")]
unsafe extern "system" {
    /// Gives a window the look of another theme ("Explorer": the light controls of the file windows; "DarkMode_Explorer": the same dark).
    fn SetWindowTheme(hwnd: isize, sub_app_name: *const u16, sub_id_list: *const u16) -> i32;
}

#[link(name = "bcrypt")]
unsafe extern "system" {
    /// `NTSTATUS BCryptGenRandom(BCRYPT_ALG_HANDLE algorithm, PUCHAR buffer, ULONG size, ULONG flags)`: 0 for success.
    fn BCryptGenRandom(algorithm: *mut c_void, buffer: *mut u8, size: u32, flags: u32) -> i32;
}

#[link(name = "comdlg32")]
unsafe extern "system" {
    fn GetOpenFileNameW(ofn: *mut OpenFileNameW) -> i32;
    fn GetSaveFileNameW(ofn: *mut OpenFileNameW) -> i32;
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
const WM_QUERYENDSESSION: u32 = 0x0011;
const WM_ENDSESSION: u32 = 0x0016;
/// lParam of WM_QUERYENDSESSION: the shutdown cannot be held up.
const ENDSESSION_CRITICAL: isize = 0x4000_0000;
const WM_ERASEBKGND: u32 = 0x0014;
const WM_SETFONT: u32 = 0x0030;
const WM_GETMINMAXINFO: u32 = 0x0024;
const WM_NCDESTROY: u32 = 0x0082;
const WM_KEYDOWN: u32 = 0x0100;
const WM_COMMAND: u32 = 0x0111;
const WM_TIMER: u32 = 0x0113;
const WM_MOUSEWHEEL: u32 = 0x020A;
const WM_MOUSEHWHEEL: u32 = 0x020E;
const WM_DPICHANGED: u32 = 0x02E0;
const WM_DROPFILES: u32 = 0x0233;
const WM_COPYDATA: u32 = 0x004A;
const WM_MBUTTONUP: u32 = 0x0208;
const WM_MOUSELEAVE: u32 = 0x02A3;
const TME_LEAVE: u32 = 2;
const SMTO_ABORTIFHUNG: u32 = 2;
const SW_RESTORE: i32 = 9;
const ERROR_ALREADY_EXISTS: u32 = 183;
/// The class of the window (found by a second copy of the program, to hand it files).
const WINDOW_CLASS: &str = "QingpdfView";
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
const WM_VSCROLL: u32 = 0x0115;
const WS_EX_ACCEPTFILES: u32 = 0x0000_0010;
const WS_EX_DLGMODALFRAME: u32 = 0x0000_0001;
const WS_EX_CLIENTEDGE: u32 = 0x0000_0200;
const CW_USEDEFAULT: i32 = i32::MIN;
const SW_SHOWNORMAL: i32 = 1;
const SW_SHOW: i32 = 5;
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
/// A label shows an ampersand as it is (a file name or a title is not a menu item).
const SS_NOPREFIX: u32 = 0x80;
const ES_PASSWORD: u32 = 0x20;
const ES_MULTILINE: u32 = 0x4;
const ES_AUTOVSCROLL: u32 = 0x40;
const ES_WANTRETURN: u32 = 0x1000;
const BS_PUSHBUTTON: u32 = 0;
const BS_DEFPUSHBUTTON: u32 = 1;
const BS_AUTOCHECKBOX: u32 = 3;
const BS_GROUPBOX: u32 = 7;
const CBS_DROPDOWN: u32 = 2;
const CBS_DROPDOWNLIST: u32 = 3;
const CBS_AUTOHSCROLL: u32 = 0x40;
const CB_ADDSTRING: u32 = 0x0143;
const CB_GETCURSEL: u32 = 0x0147;
const CB_SETCURSEL: u32 = 0x014E;
const BM_GETCHECK: u32 = 0x00F0;
const BM_SETCHECK: u32 = 0x00F1;
const IDOK: usize = 1;
const IDCANCEL: usize = 2;
const SWP_NOZOMORDER: u32 = 0x4;
const SWP_NOACTIVATE: u32 = 0x10;
const SWP_NOMOVE: u32 = 0x2;
const OFN_OVERWRITEPROMPT: u32 = 0x2;
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
const MK_SHIFT: usize = 0x0004;
const MK_LBUTTON: usize = 1;
const HTCLIENT: u32 = 1;
const IDC_HAND: usize = 32649;
const EN_CHANGE: u32 = 0x0300;
const EN_KILLFOCUS: u32 = 0x0200;
const EM_LIMITTEXT: u32 = 0x00C5;
/// TrackPopupMenu: answer with the item chosen instead of posting a command, left and top aligned.
const TPM_RETURNCMD_NONOTIFY: u32 = 0x0100 | 0x0080;
const EM_SETSEL: u32 = 0x00B1;
const ICC_TREEVIEW_CLASSES: u32 = 0x2;
const SW_HIDE: i32 = 0;
/// Child windows: the bookmarks tree and the find box.
const ID_TREE: isize = 101;
const ID_FIND: isize = 102;
/// The boxes a form field or a text box is typed in (one line, several lines).
const ID_FIELD1: isize = 103;
const ID_FIELD2: isize = 104;
const ID_PAGE_BOX: isize = 109;
const ID_SEARCH_BOX: isize = 110;
const TVS_HASBUTTONS: u32 = 0x1;
const TVS_SHOWSELALWAYS: u32 = 0x20;
const TVS_EDITLABELS: u32 = 0x8;
const TVM_DELETEITEM: u32 = 0x1101;
const TVM_EXPAND: u32 = 0x1102;
const TVM_GETITEMRECT: u32 = 0x1104;
const TVM_GETNEXTITEM: u32 = 0x110A;
const TVM_SELECTITEM: u32 = 0x110B;
const TVM_GETEDITCONTROL: u32 = 0x110F;
const TVM_HITTEST: u32 = 0x1111;
const TVM_SETINSERTMARK: u32 = 0x111A;
const TVM_GETITEMSTATE: u32 = 0x1127;
const TVM_EDITLABELW: u32 = 0x1141;
const TVGN_FIRSTVISIBLE: usize = 5;
const TVGN_PARENT: usize = 3;
const TVGN_CHILD: usize = 4;
const TVGN_DROPHILITE: usize = 8;
const TVGN_CARET: usize = 9;
const TVIS_EXPANDED: isize = 0x20;
const EM_SETLIMITTEXT: u32 = 0x00C5;
/// The tree says a title was typed over (posted by the tree's notification, so that the reader hears it after it), and the timer that scrolls
/// the tree while an item is dragged to its edge.
const WM_RENAMED: u32 = 0x8003;
const TIMER_TREE_DRAG: usize = 0x7001;
/// How far (pixels) from the top or bottom of the tree a dragged item has to be for the tree to scroll.
const TREE_EDGE: i32 = 22;
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
const NM_RCLICK: u32 = -5i32 as u32;
const TVN_SELCHANGEDW: u32 = -451i32 as u32;
const TVN_ITEMEXPANDEDW: u32 = -455i32 as u32;
const TVN_BEGINDRAGW: u32 = -456i32 as u32;
const TVN_BEGINLABELEDITW: u32 = -459i32 as u32;
const TVN_ENDLABELEDITW: u32 = -460i32 as u32;
const TVC_BYMOUSE: u32 = 1;
const TVC_BYKEYBOARD: u32 = 2;
const CF_UNICODETEXT: u32 = 13;
const GMEM_MOVEABLE: u32 = 2;
const MB_YESNO: u32 = 4;
const MB_YESNOCANCEL: u32 = 3;
const IDNO: i32 = 7;
const IDC_CROSS: usize = 32515;
const IDC_IBEAM: usize = 32513;
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
const WM_MOVE: u32 = 0x0003;
const WM_SETICON: u32 = 0x0080;
const WM_CONTEXTMENU: u32 = 0x007B;
/// Posted to the window to select all the text of a box of the toolbar once the click that gave it the keyboard is done.
const WM_SELECT_BOX: u32 = 0x8002;
const EN_SETFOCUS: u32 = 0x0100;
const ES_CENTER: u32 = 0x1;
const EM_SETCUEBANNER: u32 = 0x1501;
const SM_CXICON: i32 = 11;
const SM_CYICON: i32 = 12;
const SM_CXDOUBLECLK: i32 = 36;
const SM_CYDOUBLECLK: i32 = 37;
const SM_CXSMICON: i32 = 49;
const SM_CYSMICON: i32 = 50;
const IMAGE_ICON: u32 = 1;
const LR_SHARED: u32 = 0x8000;
/// The id of the program's icon in its resources (the group icon `build.rs` writes).
const ICON_ID: usize = 1;
const COLOR_BTNFACE: i32 = 15;
const SW_SHOWMAXIMIZED: i32 = 3;
const IDC_SIZEWE: usize = 32644;
const IDC_SIZEALL: usize = 32646;
const IDC_SIZENWSE: usize = 32642;
const IDC_SIZENESW: usize = 32643;
const TPM_RIGHTBUTTON: u32 = 0x2;
const DEFAULT_CHARSET: u32 = 1;
const ANTIALIASED_QUALITY: u32 = 4;
/// The look of the window (2026-10-20): the attribute of the title bar that makes it dark (DWMWA_USE_IMMERSIVE_DARK_MODE), the messages and styles that colour
/// the tree of bookmarks, the status bar and the toolbar, and what a window is told when it is drawn by its owner.
const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
/// The frame the reader draws the title strip in (2026-10-21): the messages of the non-client area, what a hit test answers, the attributes of Mica and of the
/// system's buttons, the sizes of the frame, and the high-contrast settings.
const WM_NCCALCSIZE: u32 = 0x0083;
const WM_NCHITTEST: u32 = 0x0084;
const WM_NCMOUSEMOVE: u32 = 0x00A0;
const WM_NCLBUTTONDOWN: u32 = 0x00A1;
const WM_NCLBUTTONUP: u32 = 0x00A2;
const WM_NCMOUSELEAVE: u32 = 0x02A2;
const WM_SYSCOMMAND: u32 = 0x0112;
const WM_SYSCOLORCHANGE: u32 = 0x0015;
const WM_DWMCOLORIZATIONCOLORCHANGED: u32 = 0x0320;
const SC_KEYMENU: usize = 0xF100;
const HTCAPTION: isize = 2;
const HTLEFT: isize = 10;
const HTRIGHT: isize = 11;
const HTTOP: isize = 12;
const HTTOPLEFT: isize = 13;
const HTTOPRIGHT: isize = 14;
const HTBOTTOM: isize = 15;
const HTBOTTOMLEFT: isize = 16;
const HTBOTTOMRIGHT: isize = 17;
const DWMWA_CAPTION_BUTTON_BOUNDS: u32 = 5;
const DWMWA_SYSTEMBACKDROP_TYPE: u32 = 38;
const DWMSBT_MAINWINDOW: i32 = 2;
const SM_CXSIZEFRAME: i32 = 32;
const SM_CYSIZEFRAME: i32 = 33;
const SM_CXPADDEDBORDER: i32 = 92;
const SPI_GETHIGHCONTRAST: u32 = 0x0042;
const SPI_SETHIGHCONTRAST: usize = 0x0043;
const HCF_HIGHCONTRASTON: u32 = 1;
const COLOR_WINDOW: i32 = 5;
const COLOR_WINDOWTEXT: i32 = 8;
const COLOR_HIGHLIGHT: i32 = 13;
const COLOR_HIGHLIGHTTEXT: i32 = 14;
const COLOR_GRAYTEXT: i32 = 17;
const TPM_RIGHTALIGN: u32 = 0x8;
const VK_DOWN: usize = 0x28;
/// The least size of the window at 96 dpi (so that the command bar has room).
const MIN_WIDTH: i32 = 640;
const MIN_HEIGHT: i32 = 420;
const TVM_SETBKCOLOR: u32 = 0x111D;
const TVM_SETEXTENDEDSTYLE: u32 = 0x112C;
const TVS_EX_DOUBLEBUFFER: usize = 0x4;
const TVM_SETTEXTCOLOR: u32 = 0x111E;
const TVM_SETLINECOLOR: u32 = 0x1128;
const WM_CTLCOLOREDIT: u32 = 0x0133;
const WM_CTLCOLORSTATIC: u32 = 0x0138;
const WM_SETTINGCHANGE: u32 = 0x001A;
const RDW_INVALIDATE_ALL: u32 = 0x1 | 0x4 | 0x80;
const HKEY_CURRENT_USER: isize = 0x8000_0001u32 as i32 as isize;
const RRF_RT_REG_DWORD: u32 = 0x10;
/// Full screen: the style of a window's frame, the monitor nearest a window, and what SetWindowPos is told to keep.
const GWL_STYLE: i32 = -16;
const MONITOR_DEFAULTTONEAREST: u32 = 2;
const SWP_NOSIZE: u32 = 0x1;
const SWP_NOOWNERZORDER: u32 = 0x200;
const SWP_FRAMECHANGED: u32 = 0x20;
/// Most characters of the box of a form field or text box, and most items of a list of choices shown.
const MAX_FIELD_CHARS: usize = 8000;
const MAX_CHOICES: usize = 1000;
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

/// The name of the Windows user (empty if Windows will not say): the author of the annotations until the person says another.
pub fn user_name() -> String {
    let mut buf = vec![0u16; 257];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` has room for `len` UTF-16 units, which is what the call is told; Windows writes the name and its length (with
    // the closing zero) into them and keeps nothing.
    let ok = unsafe { GetUserNameW(buf.as_mut_ptr(), &mut len) } != 0;
    if !ok {
        return String::new();
    }
    let n = (len as usize).saturating_sub(1).min(buf.len());
    String::from_utf16_lossy(buf.get(..n).unwrap_or(&[]))
}

/// Fill `buffer` with random bytes from the system's cryptographic generator (what an encryption is seeded with). `false`: Windows gave
/// none, and nothing weaker is ever used instead.
pub fn random_bytes(buffer: &mut [u8]) -> bool {
    /// Use the system's preferred generator; the algorithm handle must then be null.
    const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x0000_0002;
    let Ok(size) = u32::try_from(buffer.len()) else { return false };
    // SAFETY: `buffer` is a live, writable slice of exactly `size` bytes, which is what Windows is told; the algorithm handle is null as the
    // flag requires; Windows writes into the buffer during the call and keeps no pointer to it. A status of 0 is success.
    unsafe { BCryptGenRandom(null_mut(), buffer.as_mut_ptr(), size, BCRYPT_USE_SYSTEM_PREFERRED_RNG) == 0 }
}

/// Today's date in the person's time zone, as `2026-10-15` (what a page number's `{date}` is).
pub fn local_date() -> String {
    let mut t = LocalTime::default();
    // SAFETY: `t` is a live SYSTEMTIME of the size Windows expects (eight 16-bit numbers) that Windows fills in and keeps no pointer to.
    unsafe { GetLocalTime(&mut t) };
    format!("{:04}-{:02}-{:02}", t.year, t.month, t.day)
}

/// A time (seconds since 1970, UTC) in the person's time zone, as `2026-10-15 14:03:09` (the dates in a file's properties).
pub fn local_time(utc_secs: i64) -> String {
    let (year, month, day, hour, minute, second) = qingpdf_core::props::civil(utc_secs);
    let utc = LocalTime { year: year.clamp(1601, 30_827) as u16, month: month as u16, weekday: 0, day: day as u16, rest: [hour as u16, minute as u16, second as u16, 0] };
    let mut local = LocalTime::default();
    // SAFETY: both are live SYSTEMTIMEs of the size Windows expects; a null zone means the one Windows is set to; Windows fills in `local`
    // and keeps no pointer. If it fails, `local` stays empty and the time is told as it came.
    let ok = unsafe { SystemTimeToTzSpecificLocalTime(null(), &utc, &mut local) } != 0;
    let l = if ok { local } else { utc };
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", l.year, l.month, l.day, l.rest[0], l.rest[1], l.rest[2])
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

// --- the window --------------------------------------------------------------------------------------------------

/// The back buffer: the picture is drawn here (the memory of a bitmap of 32-bit pixels, which `Gdi` writes in) and put on the window in one go.
#[derive(Default)]
struct Back {
    dc: isize,
    bitmap: isize,
    bits: usize,
    w: i32,
    h: i32,
}

struct WindowState {
    hwnd: Cell<isize>,
    /// The window is being destroyed (set at WM_DESTROY): the actions still queued are dropped and the reader hears no more.
    closing: Cell<bool>,
    handler: RefCell<Option<Box<dyn Handler>>>,
    back: RefCell<Back>,
    /// The font of the text, the same in bold, the big one and the one of the pictures, in the size of the screen and the language.
    font: Cell<isize>,
    fonts: [Cell<isize>; 3],
    /// The words are Chinese (the fonts follow the language).
    zh: Cell<bool>,
    dpi: Cell<u32>,
    /// The bookmarks tree and the find box, children of the window (0 if Windows would not make them).
    tree: Cell<isize>,
    find: Cell<isize>,
    /// The tree's items by their place in the reader's list. The tree is being filled (what it says then is not the person's doing), a title
    /// may be typed over (only when the reader asked), the title that was typed and not yet handed on, and the item that is being dragged.
    tree_items: RefCell<Vec<isize>>,
    tree_filling: Cell<bool>,
    tree_rename: Cell<bool>,
    renamed: RefCell<Option<(usize, String)>>,
    tree_drag: Cell<Option<TreeDrag>>,
    /// The boxes a form field or a text box is typed in, and the one that is in use (0: none).
    field_single: Cell<isize>,
    field_multi: Cell<isize>,
    field_active: Cell<isize>,
    /// The printer a job is going to (from the print box, or named by the hidden test), until the job ends.
    printer: RefCell<Option<Printer>>,
    /// The menus the reader last gave (shown as a pop-up menu when the person asks).
    menus: RefCell<Vec<Menu>>,
    /// The box the words to find are typed in, over the search pill of the command bar (and whether it is shown), and the box over the page number or the zoom of
    /// the page pill (and whether it is in use): children of the window (0 if Windows would not make them).
    tb_search: Cell<isize>,
    tb_page: Cell<isize>,
    search_on: Cell<bool>,
    pill_open: Cell<bool>,
    /// The frame is drawn by the reader (not in a high-contrast theme), Windows can show Mica (the answer to asking for it), the colour of the title strip where
    /// the window is transparent, the size of the client area and the frame as the reader was last told.
    custom: Cell<bool>,
    mica: Cell<bool>,
    key: Cell<Rgb>,
    size: Cell<(i32, i32)>,
    frame_told: Cell<Option<(bool, i32)>>,
    /// The window's place when it is neither maximized nor minimized, and whether it is kept for the next run.
    normal: Cell<Option<WinRect>>,
    remember_window: bool,
    clicks: RefCell<ClickCounter>,
    /// How many calls of `execute` are on the stack (more than one: a box is open), the files that came from outside meanwhile, and whether
    /// the system is told when the pointer leaves the window.
    depth: Cell<u32>,
    deferred: RefCell<Vec<PathBuf>>,
    tracking: Cell<bool>,
    /// How the reader last said the window looks (none before it has), and the brushes of the ground of the window and of its boxes.
    theme: Cell<Option<ThemeSpec>>,
    face_brush: Cell<isize>,
    field_brush: Cell<isize>,
    /// The window is full screen, and what it was before (put back exactly when it is not).
    fullscreen: Cell<bool>,
    full_saved: RefCell<Option<FullSaved>>,
}

/// A window as it was before full screen: its place and its style.
struct FullSaved {
    place: WindowPlacement,
    style: u32,
    /// The dots per inch of the screen then (the place is in the pixels of that).
    dpi: u32,
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
    try_deliver(state, event).unwrap_or_default()
}

/// [`deliver`], saying whether the reader took the event: it does not when the window is closing, or when the reader is busy (a message arrived
/// while it was handling one): the event is dropped then, not queued.
fn try_deliver(state: &WindowState, event: Event) -> Option<Vec<Action>> {
    if state.closing.get() {
        return None;
    }
    let mut guard = state.handler.try_borrow_mut().ok()?;
    Some(guard.as_mut()?.event(event))
}

fn execute(state: &WindowState, actions: Vec<Action>) {
    state.depth.set(state.depth.get() + 1);
    run_actions(state, actions);
    state.depth.set(state.depth.get().saturating_sub(1));
    // Files that came from outside while a box was open are opened now that it is closed.
    if state.depth.get() == 0 {
        let waiting = std::mem::take(&mut *state.deferred.borrow_mut());
        if !waiting.is_empty() {
            dispatch(state, Event::Drop(waiting));
        }
    }
}

fn run_actions(state: &WindowState, actions: Vec<Action>) {
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
            Action::SetTimer { id, ms } => {
                // SAFETY: a null timer procedure means the timer posts WM_TIMER to the window; plain numbers only.
                unsafe { SetTimer(hwnd, id as usize, ms, null()) };
            }
            Action::KillTimer(id) => {
                // SAFETY: plain numbers; killing a timer that is not set does nothing.
                unsafe { KillTimer(hwnd, id as usize) };
            }
            Action::PickFile { title, filters } => {
                let picked = file_dialog(hwnd, &title, &filters, None);
                queue.extend(deliver(state, Event::FilePicked(picked)));
            }
            Action::PickSavePath { title, filters, name } => {
                let picked = file_dialog(hwnd, &title, &filters, Some(&name));
                queue.extend(deliver(state, Event::SavePicked(picked)));
            }
            Action::PickFolder { title } => {
                let picked = folder_dialog(hwnd, &title);
                queue.extend(deliver(state, Event::FolderPicked(picked)));
            }
            Action::AskSave { tag, title, text } => {
                let (t, c) = (wide(&text), wide(&title));
                // SAFETY: both strings are zero-terminated UTF-16 that outlive the call; the box is modal and returns when it is
                // closed. Yes is the button the Enter key presses; closing the box with the cross is Cancel.
                let answer = unsafe { MessageBoxW(hwnd, t.as_ptr(), c.as_ptr(), MB_YESNOCANCEL | MB_ICONQUESTION) };
                let choice = match answer {
                    IDYES => SaveChoice::Save,
                    IDNO => SaveChoice::Discard,
                    _ => SaveChoice::Cancel,
                };
                queue.extend(deliver(state, Event::SaveChoice { tag, choice }));
            }
            Action::AskText { tag, title, prompt, secret, initial, ok, cancel } => {
                let text = input_dialog(hwnd, state.dpi.get(), state.font.get(), &title, &prompt, secret, &initial, &ok, &cancel);
                queue.extend(deliver(state, Event::Text { tag, text }));
            }
            Action::AskForm(spec) => {
                let answer = form_dialog(hwnd, state.dpi.get(), state.font.get(), &spec);
                queue.extend(deliver(state, Event::Form { tag: spec.tag, answer }));
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
            Action::SetMenus(menus) => set_menus(state, menus),
            Action::Outline { nodes, select } => fill_tree(state, &nodes, select),
            Action::OutlineRename(index) => start_rename(state, index),
            Action::Sidebar(r) => {
                // (The tree stands a few pixels in from the window's edge, where the window is sized.)
                let inset = if r.w > 0 { scale(state.dpi.get(), 4) } else { 0 };
                place_child(state.tree.get(), Rect { x: r.x + inset, w: r.w - inset, ..r });
            }
            Action::Find(rect) => show_find(state, hwnd, rect),
            Action::FieldEdit(Some(b)) => show_field(state, hwnd, b),
            Action::FieldEdit(None) => hide_field(state, hwnd),
            Action::FieldMove(r) => {
                let edit = state.field_active.get();
                if edit != 0 {
                    // SAFETY: plain handle of a child window of ours and plain numbers.
                    unsafe { MoveWindow(edit, r.x, r.y, r.w.max(1), r.h.max(1), 1) };
                }
            }
            Action::Choose { items, selected, at } => {
                let chosen = choose(hwnd, &items, selected, at);
                queue.extend(deliver(state, Event::Chosen(chosen)));
            }
            Action::ContextMenu { entries, at } => {
                let menu = build_popup(&entries);
                if menu != 0 {
                    let mut p = Point { x: at.0, y: at.1 };
                    // SAFETY: `p` is a live POINT turned into screen coordinates; the menu belongs to this window and is destroyed after
                    // use; TPM_RETURNCMD answers with the item's command (0 if the menu was dismissed) instead of posting a message.
                    let chosen = unsafe {
                        ClientToScreen(hwnd, &mut p);
                        let chosen = TrackPopupMenu(menu, TPM_RETURNCMD_NONOTIFY | TPM_RIGHTBUTTON, p.x, p.y, 0, hwnd, null());
                        DestroyMenu(menu);
                        chosen
                    };
                    if let Ok(cmd) = u32::try_from(chosen)
                        && cmd > 0
                    {
                        queue.extend(deliver(state, Event::Command(cmd)));
                    }
                }
            }
            Action::SearchBox(b) => set_search_box(state, hwnd, b),
            Action::PillEdit(b) => set_pill_edit(state, hwnd, b),
            Action::ShowMenu { at, keyboard } => {
                if let Some(cmd) = show_menu(state, hwnd, at, keyboard) {
                    queue.extend(deliver(state, Event::Command(cmd)));
                }
            }
            Action::Theme(spec) => apply_theme(state, spec),
            Action::FullScreen(on) => set_full_screen(state, on),
            Action::HideCursor => {
                // SAFETY: a null cursor hides the pointer, until it moves and Windows asks for a cursor again.
                unsafe { SetCursor(0) };
            }
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
                save_placement(state);
                // SAFETY: destroys the window made by `run`; posts WM_DESTROY, which ends the loop. The window state is not
                // freed by this (that is done after the loop), so `state` stays valid for the rest of this function.
                unsafe { DestroyWindow(hwnd) };
                // Nothing after a Quit is carried out.
                break;
            }
        }
    }
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
        // SAFETY: for WM_GETMINMAXINFO, lparam points to a MINMAXINFO that is valid and ours to change for the duration of this message.
        if let Some(info) = unsafe { (lparam as *mut MinMaxInfo).as_mut() } {
            if ALLOW_BIG_WINDOW.load(Ordering::Relaxed) {
                info.max_track = Point { x: 16384, y: 16384 };
            } else {
                // SAFETY: plain handle; reads the screen's dots per inch for the window (0 for a window that is not there yet).
                let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
                info.min_track = Point { x: scale(dpi, MIN_WIDTH), y: scale(dpi, MIN_HEIGHT) };
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
    // The frame the reader draws the title strip in: not in full screen nor with a high-contrast theme (the system's own frame then).
    let own_frame = state.custom.get() && !state.fullscreen.get();
    match msg {
        WM_NCCALCSIZE if own_frame => return nc_calc_size(hwnd, wparam, lparam),
        WM_NCHITTEST if own_frame => return nc_hit_test(state, hwnd, wparam, lparam),
        // The buttons the system draws over the title strip are pressed and hovered through Windows' own procedure.
        WM_NCMOUSEMOVE | WM_NCLBUTTONDOWN | WM_NCLBUTTONUP | WM_NCMOUSELEAVE if own_frame => {
            let mut result = 0isize;
            // SAFETY: DwmDefWindowProc takes the message as this procedure got it and writes its answer to `result`.
            if unsafe { DwmDefWindowProc(hwnd, msg, wparam, lparam, &mut result) } != 0 {
                return result;
            }
        }
        // Alt or F10 alone: the menus, for the keyboard (Alt+Space is the system menu and is left to the system).
        WM_SYSCOMMAND if wparam & 0xFFF0 == SC_KEYMENU && lparam != isize::from(b' ') => {
            dispatch(state, Event::MenuKey);
            return 0;
        }
        WM_ERASEBKGND => {
            // The pages are drawn over their area in one go, with no erasing first (that would flicker). What a child window asks this
            // window to draw behind it (the toolbar does, to show the colour of a window) is filled in the colour of a window.
            let dc = wparam as isize;
            let mut r = WinRect::default();
            // SAFETY: `dc` is the device context that came with the message; `r` is a live RECT that Windows fills in; the brush is the
            // system's own, shared and never freed, or the window's own face (a dark window). A DC that is this window's own is left alone.
            unsafe {
                if WindowFromDC(dc) != hwnd && GetClipBox(dc, &mut r) != 0 {
                    FillRect(dc, &r, face_brush(state));
                }
            }
            return 1;
        }
        WM_PAINT => {
            paint(state, hwnd);
            return 0;
        }
        WM_SIZE => {
            if wparam != 1 {
                remember_normal(state);
                apply_layout(state);
            }
            return 0;
        }
        WM_MOVE => {
            remember_normal(state);
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
        WM_COMMAND => {
            let code = hiword(wparam);
            if lparam == 0 {
                dispatch(state, Event::Command(loword(wparam)));
            } else if (lparam == state.find.get() || lparam == state.tb_search.get()) && code == EN_CHANGE {
                let text = window_text(lparam, MAX_FIND_CHARS);
                dispatch(state, Event::FindText(text));
            } else if lparam == state.tb_search.get() && code == EN_SETFOCUS {
                dispatch(state, Event::FindFocus);
                // SAFETY: posts a message to this window; plain numbers (the box is selected once the click that gave it the keyboard is done).
                unsafe { PostMessageW(hwnd, WM_SELECT_BOX, lparam as usize, 0) };
            } else if lparam == state.tb_page.get() && code == EN_KILLFOCUS && state.pill_open.replace(false) {
                // The keyboard went elsewhere (a click outside the box): what was typed is not taken.
                place_child(lparam, Rect { x: 0, y: 0, w: 0, h: 0 });
                dispatch(state, Event::PillEdit(None));
            } else if lparam == state.field_active.get() && hiword(wparam) == EN_KILLFOCUS {
                // The keyboard went elsewhere (a click outside the box): what is typed is kept.
                finish_field(state, FieldExit::Blur);
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
            let clicks = click_count(state, x, y);
            dispatch(state, Event::MouseDown { x, y, ctrl: modifier(0x11), shift: modifier(0x10), clicks });
            return 0;
        }
        WM_MOUSEMOVE => {
            // An item of the tree is being dragged: the pointer is the tree's.
            if state.tree_drag.get().is_some() {
                tree_drag_over(state, in_tree(state, lparam));
                return 0;
            }
            let (x, y) = mouse_place(lparam);
            if wparam & MK_LBUTTON != 0 {
                dispatch(state, Event::MouseMove { x, y, shift: wparam & MK_SHIFT != 0 });
            } else {
                // (The system says when the pointer leaves the window, once, after this.)
                if !state.tracking.replace(true) {
                    let mut track = TrackMouse { size: std::mem::size_of::<TrackMouse>() as u32, flags: TME_LEAVE, hwnd, hover_time: 0 };
                    // SAFETY: `track` is a live TRACKMOUSEEVENT of the size it declares; the window is ours.
                    unsafe { TrackMouseEvent(&mut track) };
                }
                dispatch(state, Event::Hover { x, y });
            }
            return 0;
        }
        WM_MOUSELEAVE => {
            state.tracking.set(false);
            dispatch(state, Event::HoverEnd);
            return 0;
        }
        WM_MBUTTONUP => {
            let (x, y) = mouse_place(lparam);
            dispatch(state, Event::MiddleClick { x, y });
            return 0;
        }
        WM_COPYDATA => {
            // Another program sent this (the other copy of the reader does, with files to open; any program can): it is read as a list of paths
            // of PDF files and nothing else, and thrown away whole if it is anything but that (`copydata.rs`).
            // SAFETY: for WM_COPYDATA, lparam points to a COPYDATASTRUCT that is valid for the duration of this message; it is only read.
            let sent = unsafe { (lparam as *const CopyData).as_ref() };
            let paths = sent.and_then(|cd| {
                let len = usize::try_from(cd.size).ok().filter(|&n| n <= copydata::MAX_BYTES && !cd.data.is_null())?;
                // SAFETY: the sender of a WM_COPYDATA says `size` bytes at `data` are there, and the system keeps them valid until this message
                // is answered; `len` is held to the limit above before they are read, and `decode` copies what it keeps.
                let bytes = unsafe { std::slice::from_raw_parts(cd.data.cast::<u8>(), len) };
                copydata::decode(cd.id, bytes)
            });
            let Some(paths) = paths else { return 0 };
            // (The sender is told whether the files were taken: if not, it opens them in a window of its own.)
            if !open_from_outside(state, paths) {
                return 0;
            }
            bring_to_front(hwnd);
            return 1;
        }
        WM_LBUTTONUP => {
            // The item goes where the pointer is as the button comes up (the place in this message).
            if state.tree_drag.get().is_some() {
                tree_drag_end(state, Some(in_tree(state, lparam)));
                return 0;
            }
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
            if state.tree_drag.get().is_some() {
                tree_drag_end(state, None);
                return 0;
            }
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
                if shape == Cursor::Hidden {
                    // SAFETY: a null cursor hides the pointer.
                    unsafe { SetCursor(0) };
                    return 1;
                }
                let id = match shape {
                    Cursor::SizeWE => IDC_SIZEWE,
                    Cursor::Move => IDC_SIZEALL,
                    Cursor::SizeNWSE => IDC_SIZENWSE,
                    Cursor::SizeNESW => IDC_SIZENESW,
                    Cursor::Hand => IDC_HAND,
                    Cursor::Cross => IDC_CROSS,
                    Cursor::Text => IDC_IBEAM,
                    Cursor::Arrow => IDC_ARROW,
                    Cursor::Hidden => IDC_ARROW,
                };
                // SAFETY: LoadCursorW with a stock cursor id (a small integer in the pointer slot) gives a shared cursor that
                // is never freed; SetCursor takes it.
                unsafe { SetCursor(LoadCursorW(0, id as *const u16)) };
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
                return tree_notified(state, hwnd, hdr, lparam);
            }
            return 0;
        }
        WM_CONTEXTMENU => {
            // The right button (or the menu key) over the pages: the reader may have a menu for the place. Over a child window the
            // child's own menu is its business.
            if wparam as isize == hwnd {
                let (w, h) = state.size.get();
                let mut p = Point { x: i32::from(loword(lparam as usize) as u16 as i16), y: i32::from(hiword(lparam as usize) as u16 as i16) };
                if lparam as i32 == -1 {
                    p = Point { x: w / 2, y: h / 2 };
                } else {
                    // SAFETY: `p` is a live POINT that Windows rewrites in place.
                    unsafe { ScreenToClient(hwnd, &mut p) };
                }
                if p.x >= 0 && p.y >= 0 && p.x < w && p.y < h {
                    dispatch(state, Event::ContextMenu { x: p.x, y: p.y });
                }
                return 0;
            }
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC => {
            // The boxes of the command bar and the pill are in the colour of the pill; the others are dark when the window is.
            if let Some(t) = state.theme.get().filter(|t| t.dark || lparam == state.tb_search.get() || lparam == state.tb_page.get()) {
                // SAFETY: `wparam` is the device context of the box, valid for this message; plain colours.
                unsafe {
                    SetTextColor(wparam as isize, colorref(t.text));
                    SetBkColor(wparam as isize, colorref(t.field_bg));
                }
                return state.field_brush.get();
            }
        }
        WM_SETTINGCHANGE => {
            // The high-contrast setting changed.
            if wparam == SPI_SETHIGHCONTRAST {
                refresh_contrast(state);
            }
            // "ImmersiveColorSet" is what the system says when Windows was set to light or dark apps (or another accent); anything else it says is not for the reader.
            if lparam != 0 {
                let name: Vec<u16> = (0..32usize)
                    .map_while(|i| {
                        // SAFETY: when a WM_SETTINGCHANGE has a name, lparam points to a zero-terminated UTF-16 string that is valid for this message; the units
                        // are read one by one and no further than the zero, and at most 32 of them.
                        let c = unsafe { *(lparam as *const u16).add(i) };
                        (c != 0).then_some(c)
                    })
                    .collect();
                if name.iter().copied().eq("ImmersiveColorSet".encode_utf16()) {
                    dispatch(state, Event::SystemDark(system_uses_dark()));
                    dispatch(state, Event::SystemAccent(system_accent()));
                }
            }
        }
        WM_DWMCOLORIZATIONCOLORCHANGED => {
            dispatch(state, Event::SystemAccent(system_accent()));
            return 0;
        }
        WM_SYSCOLORCHANGE => {
            refresh_contrast(state);
            return 0;
        }
        WM_SELECT_BOX => {
            let edit = wparam as isize;
            if edit != 0 && (edit == state.tb_page.get() || edit == state.tb_search.get()) {
                // SAFETY: plain handle of a box of ours; the whole text is selected (a start of 0 and an end of -1).
                unsafe { SendMessageW(edit, EM_SETSEL, 0, -1) };
            }
            return 0;
        }
        WM_CLOSE => {
            // The reader decides: it answers with a Quit when the window may go (not at once if there are changes to save).
            dispatch(state, Event::CloseRequested);
            return 0;
        }
        WM_QUERYENDSESSION => {
            // The system is about to log off, restart or shut down. With changes not saved the session is not ended yet: the system
            // is given the reason (it shows it beside this program in the list of what is holding it up), and the question that
            // closing the window asks is asked, after this message is answered. A shutdown that cannot be held up is let through.
            if lparam & ENDSESSION_CRITICAL != 0 {
                return 1;
            }
            let reason = match state.handler.try_borrow() {
                Ok(guard) => guard.as_ref().and_then(|h| h.unsaved_reason()),
                // The reader is in the middle of something: do not risk losing what it holds.
                Err(_) => Some("Unsaved changes".to_string()),
            };
            let Some(text) = reason else { return 1 };
            let text = wide(&text);
            // SAFETY: `text` is a zero-terminated UTF-16 string that outlives the call (Windows copies it); the handle is this window's.
            unsafe { ShutdownBlockReasonCreate(hwnd, text.as_ptr()) };
            // SAFETY: posts WM_CLOSE to this window; plain numbers, nothing is borrowed.
            unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) };
            return 0;
        }
        WM_ENDSESSION => {
            // `wparam` is zero when the session was not ended after all: what held it up is let go of.
            if wparam == 0 {
                // SAFETY: takes this window's handle; does nothing if it has no reason.
                unsafe { ShutdownBlockReasonDestroy(hwnd) };
            }
            return 0;
        }
        WM_TIMER => {
            if wparam == TIMER_TREE_DRAG {
                tree_drag_tick(state);
                return 0;
            }
            dispatch(state, Event::Timer(wparam as u32));
            return 0;
        }
        WM_RENAMED => {
            let typed = state.renamed.borrow_mut().take();
            if let Some((index, text)) = typed {
                dispatch(state, Event::OutlineRename { index, text });
            }
            return 0;
        }
        WM_WAKE => {
            dispatch(state, Event::Wake);
            return 0;
        }
        WM_DROPFILES => {
            let files = dropped_files(wparam as isize);
            open_from_outside(state, files);
            return 0;
        }
        WM_DPICHANGED => {
            let dpi = loword(wparam);
            // SAFETY: for WM_DPICHANGED, lparam points to a RECT (the suggested new window rectangle) that is
            // valid for the duration of this message; it is only read here.
            let suggested = unsafe { (lparam as *const WinRect).as_ref() }.copied();
            if state.fullscreen.get() {
                // Full screen is the monitor, whatever Windows suggests for a window of the old size.
                fit_monitor(hwnd);
            } else if let Some(r) = suggested {
                // SAFETY: plain numbers and the handle of this window.
                unsafe { SetWindowPos(hwnd, 0, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZOMORDER | SWP_NOACTIVATE) };
            }
            new_dpi(state, dpi);
            dispatch(state, Event::Dpi(dpi));
            return 0;
        }
        WM_DESTROY => {
            state.closing.set(true);
            // SAFETY: takes this window's handle; does nothing if it has no reason.
            unsafe { ShutdownBlockReasonDestroy(hwnd) };
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

/// Files that came from outside (dropped on the window, handed over by another copy of the program): opened at once, or, with a box open on
/// the window, when it is closed.
/// `false`: they were not taken (the reader is busy, the window is closing, or too many wait already).
fn open_from_outside(state: &WindowState, files: Vec<PathBuf>) -> bool {
    if state.depth.get() > 0 {
        copydata::keep_waiting(&mut state.deferred.borrow_mut(), files, crate::tabs::MAX_DROPPED)
    } else {
        match try_deliver(state, Event::Drop(files)) {
            Some(actions) => {
                execute(state, actions);
                true
            }
            None => false,
        }
    }
}

/// Show the window to the person (out of the task bar if it was minimised, in front of the others).
fn bring_to_front(hwnd: isize) {
    // SAFETY: plain handle of our window and plain numbers.
    unsafe {
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        SetForegroundWindow(hwnd);
    }
}

/// Is this the first copy of the program on this desktop? (The first one takes a name that the others find taken; the handle is kept for as long
/// as the program runs.)
pub fn first_copy() -> bool {
    let name = wide("Local\\QingpdfViewFirstCopy");
    // SAFETY: a null security descriptor, no initial owner and a zero-terminated name that outlives the call; the handle is never closed.
    let handle = unsafe { CreateMutexW(null(), 0, name.as_ptr()) };
    // SAFETY: reads the last error of this thread, set by the call just made.
    handle != 0 && unsafe { GetLastError() } != ERROR_ALREADY_EXISTS
}

/// Hand these files to the copy of the program that is running (its window is found by its class): `true` if it took them (they are what
/// `copydata::decode` accepts), and it comes to the front.
pub fn hand_over(paths: &[PathBuf]) -> bool {
    let data = copydata::encode(paths);
    let class = wide(WINDOW_CLASS);
    // SAFETY: the class is a zero-terminated string that outlives the call; no window title (null).
    let hwnd = unsafe { FindWindowW(class.as_ptr(), null()) };
    let Ok(size) = u32::try_from(data.len()) else { return false };
    if hwnd == 0 || size == 0 {
        return false;
    }
    let (mut pid, mut result) = (0u32, 0usize);
    let message = CopyData { id: copydata::ID, size, data: data.as_ptr().cast() };
    // SAFETY: `pid` is a live u32 that Windows fills in; `message` is a live COPYDATASTRUCT whose data points to `size` bytes of `data`, which
    // outlive the call (SendMessageTimeoutW does not return before the other window has answered or the time is out, and the system copies
    // the bytes for the other process); a window that does not answer in 3 seconds, or hangs, is given up on.
    let sent = unsafe {
        GetWindowThreadProcessId(hwnd, &mut pid);
        AllowSetForegroundWindow(pid);
        SendMessageTimeoutW(hwnd, WM_COPYDATA, 0, (&raw const message) as isize, SMTO_ABORTIFHUNG, 3000, &mut result)
    };
    sent != 0 && result == 1
}

fn dropped_files(drop: isize) -> Vec<PathBuf> {
    // SAFETY: `drop` is the HDROP of a WM_DROPFILES message, valid until DragFinish. 0xFFFFFFFF asks for the count.
    let count = unsafe { DragQueryFileW(drop, u32::MAX, null_mut(), 0) };
    // (The first few dozen only: the rest of a very long list is not looked at.)
    let count = count.min(crate::tabs::MAX_DROPPED as u32);
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

/// The place in the reader's list of an item of the tree.
fn tree_param(tree: isize, handle: isize) -> Option<usize> {
    let mut item = tv_item(TVIF_PARAM | TVIF_HANDLE, null_mut(), -1);
    item.item = handle;
    // SAFETY: TVM_GETITEMW takes a pointer to a live TVITEMW and fills in the fields the mask names; nothing is kept.
    unsafe { SendMessageW(tree, TVM_GETITEMW, 0, (&raw mut item) as isize) };
    usize::try_from(item.param).ok()
}

/// Where the pointer was when the message being handled was sent, in the client area of `window` (not where it is now: a click that is handled
/// late was made where it was made, and the pointer may have moved since).
fn message_point(window: isize) -> Point {
    // SAFETY: gives the place that goes with the message being handled; no arguments.
    let at = unsafe { GetMessagePos() };
    let (x, y) = mouse_place(at as isize);
    let mut p = Point { x, y };
    // SAFETY: `p` is a live POINT, turned from screen coordinates into the window's in place.
    unsafe { ScreenToClient(window, &mut p) };
    p
}

/// A place of the main window's client area (a mouse message's `lparam`) in the tree's own client area.
fn in_tree(state: &WindowState, lparam: isize) -> Point {
    let (x, y) = mouse_place(lparam);
    let mut p = Point { x, y };
    // SAFETY: `p` is a live POINT, turned from the window's coordinates into the screen's and into the tree's in place; both are windows of ours.
    unsafe {
        ClientToScreen(state.hwnd.get(), &mut p);
        ScreenToClient(state.tree.get(), &mut p);
    }
    p
}

/// What the tree has at `pt` (a place in its client area): the item and what part of it, or none.
fn tree_hit(tree: isize, pt: Point) -> TvHitTest {
    let mut hit = TvHitTest { pt, flags: 0, item: 0 };
    // SAFETY: TVM_HITTEST takes a pointer to a live TVHITTESTINFO, which it fills in and does not keep.
    unsafe { SendMessageW(tree, TVM_HITTEST, 0, (&raw mut hit) as isize) };
    hit
}

/// A notification from the bookmarks tree: a click on an item (a new item is selected by the click, and the same one can be clicked again),
/// the selection moved by the keyboard, an item opened or closed, the right button, a title typed over, an item dragged. The answer is what
/// the tree is to be told (for a title that starts to be typed over: whether it may; for one that stops: whether the tree takes it).
fn tree_notified(state: &WindowState, hwnd: isize, hdr: &NmHdr, lparam: isize) -> isize {
    let tree = state.tree.get();
    match hdr.code {
        NM_CLICK => {
            let hit = tree_hit(tree, message_point(tree));
            if hit.item != 0
                && hit.flags & TVHT_ON_ITEM != 0
                && let Some(index) = tree_param(tree, hit.item)
            {
                dispatch(state, Event::OutlineClick(index));
                // The pages get the keyboard back, so that the keys scroll them; not while a title is being typed over (the click may have started
                // that, and the box would lose the keyboard and close with the title as it was).
                // SAFETY: plain handle of the main window, and a message to the tree that answers the box it has over a title (0: none).
                unsafe {
                    if SendMessageW(tree, TVM_GETEDITCONTROL, 0, 0) == 0 {
                        SetFocus(hwnd);
                    }
                }
            }
        }
        NM_RCLICK => {
            let at = message_point(tree);
            let hit = tree_hit(tree, at);
            let item = (hit.item != 0 && hit.flags & (TVHT_ON_ITEM | 0x20) != 0).then_some(hit.item);
            // The item under the pointer is the one the menu is for (it is selected without the reader going to it).
            if let Some(h) = item {
                // SAFETY: plain messages to the tree with a handle it gave.
                unsafe { SendMessageW(tree, TVM_SELECTITEM, TVGN_CARET, h) };
            }
            let mut p = at;
            // SAFETY: `p` is a live POINT, turned from the tree's coordinates into the window's in place.
            unsafe {
                ClientToScreen(tree, &mut p);
                ScreenToClient(hwnd, &mut p);
            }
            dispatch(state, Event::OutlineMenu { index: item.and_then(|h| tree_param(tree, h)), x: p.x, y: p.y });
            // (Nothing more is done by the tree itself.)
            return 1;
        }
        TVN_ITEMEXPANDEDW => {
            // SAFETY: for TVN_ITEMEXPANDEDW, lparam points to an NMTREEVIEWW valid for the duration of this message; it is only read.
            let nm = unsafe { (lparam as *const NmTreeView).as_ref() };
            if let Some(nm) = nm
                && !state.tree_filling.get()
                && let Ok(index) = usize::try_from(nm.new.param)
            {
                dispatch(state, Event::OutlineExpand { index, open: nm.action == TVE_EXPAND as u32 });
            }
        }
        TVN_BEGINLABELEDITW => {
            // Only the reader starts a title being typed over (the file may not allow it): the tree's own slow click is not allowed.
            if !state.tree_rename.replace(false) {
                return 1;
            }
            // SAFETY: plain messages to the tree and to the box it made for the title (a null handle does nothing).
            unsafe {
                let edit = SendMessageW(tree, TVM_GETEDITCONTROL, 0, 0);
                SendMessageW(edit, EM_SETLIMITTEXT, qingpdf_core::view::MAX_TITLE_CHARS, 0);
            }
            return 0;
        }
        TVN_ENDLABELEDITW => {
            // SAFETY: for TVN_ENDLABELEDITW, lparam points to an NMTVDISPINFOW valid for the duration of this message; it is only read, and its
            // text (null when the typing was given up) is a zero-terminated string that is copied before the message ends.
            let nm = unsafe { (lparam as *const NmTvDispInfo).as_ref() };
            if let Some(nm) = nm
                && !nm.item.text.is_null()
                && let Ok(index) = usize::try_from(nm.item.param)
            {
                // SAFETY: the pointer is the string the box held, zero-terminated; at most 1024 characters are read.
                let units: Vec<u16> = (0..1024usize).map_while(|i| Some(unsafe { nm.item.text.add(i).read() }).filter(|&u| u != 0)).collect();
                *state.renamed.borrow_mut() = Some((index, String::from_utf16_lossy(&units)));
                // SAFETY: plain handle and message of this window; the reader hears the title when this notification is over.
                unsafe { PostMessageW(hwnd, WM_RENAMED, 0, 0) };
            }
            // (The tree is not given the text: what it shows is what the reader's list says, put there when the change is made.)
            return 0;
        }
        TVN_BEGINDRAGW => {
            // SAFETY: for TVN_BEGINDRAGW, lparam points to an NMTREEVIEWW valid for the duration of this message; it is only read.
            let nm = unsafe { (lparam as *const NmTreeView).as_ref() };
            if let Some(nm) = nm
                && let Ok(index) = usize::try_from(nm.new.param)
            {
                state.tree_drag.set(Some(TreeDrag { item: nm.new.item, index, mark: None, at: nm.pt }));
                // SAFETY: plain handle of the main window: it takes the pointer so that the drag is heard wherever the pointer goes, and a timer
                // that scrolls the tree when the item is held at its edge.
                unsafe {
                    SetCapture(hwnd);
                    SetTimer(hwnd, TIMER_TREE_DRAG, 60, null());
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
    0
}

/// Mark where a dragged item would land (or take the marks away): a line before or after an item, or the item itself lit for "inside".
fn tree_mark(tree: isize, mark: Option<DropMark>) {
    let (after, line, lit) = match mark {
        Some(DropMark { place: Place::Inside, on, .. }) => (false, 0, on),
        Some(DropMark { place, on, .. }) => (place == Place::After, on, 0),
        None => (false, 0, 0),
    };
    // SAFETY: plain messages to the tree with handles it gave (0 takes a mark away).
    unsafe {
        SendMessageW(tree, TVM_SETINSERTMARK, usize::from(after), line);
        SendMessageW(tree, TVM_SELECTITEM, TVGN_DROPHILITE, lit);
    }
}

/// Where a drop at `pt` (a place in the tree's client area) would put the dragged item: the top third of a row is before it, the bottom third
/// after it (before its first child when it is open), the middle inside it (`treedrop`). Not on the item itself or on what is under it.
fn tree_drop_target(tree: isize, dragged: isize, pt: Point) -> Option<DropMark> {
    let hit = tree_hit(tree, pt);
    if hit.item == 0 || hit.flags & (TVHT_ON_ITEM | 0x20 | 0x40) == 0 {
        return None;
    }
    let mut up = hit.item;
    for _ in 0..64 {
        if up == dragged {
            return None;
        }
        // SAFETY: a plain message to the tree with a handle it gave; 0 is no parent.
        up = unsafe { SendMessageW(tree, TVM_GETNEXTITEM, TVGN_PARENT, up) };
        if up == 0 {
            break;
        }
    }
    let to = tree_param(tree, hit.item)?;
    // The rectangle of the row: the tree takes the handle where the rectangle will be.
    let h = hit.item as u64;
    let mut row = WinRect { left: h as i32, top: (h >> 32) as i32, right: 0, bottom: 0 };
    // SAFETY: TVM_GETITEMRECT takes a pointer to a RECT that holds the handle (the size of two numbers) and is rewritten with the rectangle.
    if unsafe { SendMessageW(tree, TVM_GETITEMRECT, 0, (&raw mut row) as isize) } == 0 {
        return None;
    }
    // SAFETY: plain messages to the tree with handles it gave.
    let (open, child) = unsafe { (SendMessageW(tree, TVM_GETITEMSTATE, hit.item as usize, TVIS_EXPANDED) & TVIS_EXPANDED != 0, SendMessageW(tree, TVM_GETNEXTITEM, TVGN_CHILD, hit.item)) };
    let (place, first_child) = treedrop::landing(treedrop::zone(pt.y, row.top, row.bottom), open && child != 0);
    if !first_child {
        return Some(DropMark { to, place, on: hit.item });
    }
    // The line goes between the row and its first child, and the item cannot be put in front of itself.
    (child != dragged).then_some(())?;
    Some(DropMark { to: tree_param(tree, child)?, place, on: child })
}

/// The pointer moved to `pt` (a place in the tree's client area), or the tree scrolled, while an item is dragged: the marks follow.
fn tree_drag_over(state: &WindowState, pt: Point) {
    let Some(mut drag) = state.tree_drag.get() else { return };
    let tree = state.tree.get();
    let now = tree_drop_target(tree, drag.item, pt);
    if now != drag.mark {
        tree_mark(tree, now);
        drag.mark = now;
    }
    drag.at = pt;
    state.tree_drag.set(Some(drag));
}

/// The timer of a drag: the tree scrolls when the item is held at its top or bottom edge.
fn tree_drag_tick(state: &WindowState) {
    let Some(drag) = state.tree_drag.get() else { return };
    let tree = state.tree.get();
    let mut r = WinRect::default();
    // SAFETY: `r` is a live RECT that is filled in; the tree is a window of ours.
    unsafe { GetClientRect(tree, &mut r) };
    if let Some(down) = treedrop::scroll_toward(drag.at.y, r.bottom, TREE_EDGE) {
        // SAFETY: a scroll message (line up is 0, line down 1) to the tree; the bar is the tree's own.
        unsafe { SendMessageW(tree, WM_VSCROLL, usize::from(down), 0) };
        tree_drag_over(state, drag.at);
    }
}

/// The drag is over: the marks go, the pointer is given back, and if the button came up (`drop` is the place, in the tree's client area) over a
/// place that takes the item the reader hears where it went.
fn tree_drag_end(state: &WindowState, drop: Option<Point>) {
    let Some(drag) = state.tree_drag.take() else { return };
    tree_mark(state.tree.get(), None);
    // SAFETY: stops the timer made for the drag and gives the pointer back; fine to call when it was not captured.
    unsafe {
        KillTimer(state.hwnd.get(), TIMER_TREE_DRAG);
        ReleaseCapture();
    }
    if let Some(pt) = drop
        && let Some(DropMark { to, place, .. }) = tree_drop_target(state.tree.get(), drag.item, pt)
    {
        dispatch(state, Event::OutlineDrop { from: drag.index, to, place });
    }
}

/// Start typing over the title of the item at this place of the reader's list, in the tree where it is.
fn start_rename(state: &WindowState, index: usize) {
    let tree = state.tree.get();
    let handle = state.tree_items.borrow().get(index).copied().unwrap_or(0);
    if tree == 0 || handle == 0 {
        return;
    }
    state.tree_rename.set(true);
    // SAFETY: plain handle of the tree and a message with a handle it gave: the tree takes the keyboard and puts a box over the title.
    unsafe {
        SetFocus(tree);
        SendMessageW(tree, TVM_EDITLABELW, 0, handle);
    }
    state.tree_rename.set(false);
}

fn tv_item(mask: u32, text: *mut u16, param: isize) -> TvItem {
    TvItem { mask, item: 0, state: 0, state_mask: 0, text, text_max: 0, image: 0, selected_image: 0, children: 0, param }
}

/// Fill the tree with the bookmarks (what was in it goes): the items open that the list says are open, `select` selected and brought into view,
/// and without one the item that was at the top still at the top.
fn fill_tree(state: &WindowState, nodes: &[OutlineNode], select: Option<usize>) {
    let tree = state.tree.get();
    if tree == 0 {
        return;
    }
    // (A drag that is going on is given up: the items it has the handles of are not there after this.)
    tree_drag_end(state, None);
    state.tree_filling.set(true);
    // SAFETY: plain messages to the tree control: the first item in view is asked for (to be put at the top again), no redraw while it is
    // filled, and all its items deleted.
    let top = unsafe {
        let first = SendMessageW(tree, TVM_GETNEXTITEM, TVGN_FIRSTVISIBLE, 0);
        SendMessageW(tree, WM_SETREDRAW, 0, 0);
        first
    };
    let top = (top != 0).then(|| tree_param(tree, top)).flatten();
    // SAFETY: as above.
    unsafe { SendMessageW(tree, TVM_DELETEITEM, 0, TVI_ROOT) };
    let mut handles: Vec<isize> = Vec::with_capacity(nodes.len());
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
        handles.push(handle);
        if handle == 0 {
            continue;
        }
        if first == 0 {
            first = handle;
        }
        path.truncate(depth);
        path.push(handle);
        if node.open {
            to_open.push(handle);
        }
    }
    let handle_of = |i: Option<usize>| i.and_then(|i| handles.get(i)).copied().unwrap_or(0);
    // SAFETY: plain messages to the tree control with handles it gave: open the items the file shows open, show the item selected (else the
    // one that was at the top, else the first), and let it draw again.
    unsafe {
        for h in to_open {
            SendMessageW(tree, TVM_EXPAND, TVE_EXPAND, h);
        }
        let (selected, kept_top) = (handle_of(select), handle_of(top));
        if selected != 0 {
            SendMessageW(tree, TVM_SELECTITEM, TVGN_CARET, selected);
            SendMessageW(tree, TVM_ENSUREVISIBLE, 0, selected);
        } else if kept_top != 0 {
            SendMessageW(tree, TVM_SELECTITEM, TVGN_FIRSTVISIBLE, kept_top);
        } else if first != 0 {
            SendMessageW(tree, TVM_ENSUREVISIBLE, 0, first);
        }
        SendMessageW(tree, WM_SETREDRAW, 1, 0);
    }
    *state.tree_items.borrow_mut() = handles;
    state.tree_filling.set(false);
    invalidate(tree);
}

fn new_dpi(state: &WindowState, dpi: u32) {
    state.dpi.set(dpi);
    rebuild_fonts(state);
    set_icons(state.hwnd.get(), dpi);
    apply_frame(state);
    apply_layout(state);
}

/// Is there a font with this name on this computer (when there is not, Windows gives another and says its name)?
fn has_font(dc: isize, face: &str) -> bool {
    let name = wide(face);
    // SAFETY: plain numbers and `name`, a zero-terminated UTF-16 string that outlives the call (Windows copies it); the font is owned here.
    let font = unsafe { CreateFontW(-16, 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET, 0, 0, ANTIALIASED_QUALITY, 0, name.as_ptr()) };
    if font == 0 {
        return false;
    }
    let mut buf = [0u16; 64];
    // SAFETY: `dc` is a device context of ours; the font is selected only for the call that asks for its name, the old one is put back
    // and the font deleted after; `buf` has room for the 64 characters the call is told.
    let n = unsafe {
        let old = SelectObject(dc, font);
        let n = GetTextFaceW(dc, buf.len() as i32, buf.as_mut_ptr());
        SelectObject(dc, old);
        DeleteObject(font);
        n
    };
    // The count includes the closing zero.
    let got = String::from_utf16_lossy(buf.get(..usize::try_from(n).unwrap_or(1).saturating_sub(1)).unwrap_or(&[]));
    got.eq_ignore_ascii_case(face)
}

/// The first of these faces that this computer has (the last if none is found).
fn first_face(dc: isize, faces: &[&'static str]) -> &'static str {
    faces.iter().copied().find(|f| has_font(dc, f)).unwrap_or(faces.last().copied().unwrap_or("Segoe UI"))
}

/// A font of the face, `px` high (a negative height is the height of the letters, not of the line), `weight` heavy, smoothed as the screen is set to.
fn make_font(face: &str, px: i32, weight: i32, quality: u32, charset: u32) -> isize {
    let name = wide(face);
    // SAFETY: all arguments are plain numbers except `name`, a zero-terminated UTF-16 string that outlives the call (Windows copies it). The font
    // returned is owned by the caller; 0 means it failed.
    unsafe { CreateFontW(-px, 0, 0, 0, weight, 0, 0, 0, charset, 0, 0, quality, 0, name.as_ptr()) }
}

/// Make the fonts again for the screen's dots per inch and the language (Chinese: Microsoft YaHei UI; else Segoe UI Variable Text, or Segoe UI where that
/// is not there): the text, the same in bold, the big name of the welcome page, and the pictures of the buttons (Segoe Fluent Icons or Segoe MDL2 Assets).
fn rebuild_fonts(state: &WindowState) {
    let dpi = state.dpi.get() as i32;
    // SAFETY: a null device context gives one of the screen's kind; it is deleted below.
    let dc = unsafe { CreateCompatibleDC(0) };
    if dc == 0 {
        return;
    }
    let text_face = if state.zh.get() { first_face(dc, &["Microsoft YaHei UI", "Segoe UI"]) } else { first_face(dc, &["Segoe UI Variable Text", "Segoe UI"]) };
    let icon_face = cmdbar::pick_icon_font(has_font(dc, cmdbar::FLUENT));
    // SAFETY: the device context made above, not used again.
    unsafe { DeleteDC(dc) };
    let body = (9 * dpi + 36) / 72;
    let made = [
        make_font(text_face, body, 400, 5, 1),
        make_font(text_face, body, 700, 5, 1),
        make_font(text_face, 28 * dpi / 96, 700, 5, 1),
        make_font(icon_face, cmdbar::ICON_PX * dpi / 96, 400, ANTIALIASED_QUALITY, DEFAULT_CHARSET),
    ];
    let old = [state.font.replace(made[0]), state.fonts[0].replace(made[1]), state.fonts[1].replace(made[2]), state.fonts[2].replace(made[3])];
    if state.font.get() != 0 {
        for child in [state.tree.get(), state.find.get(), state.field_single.get(), state.field_multi.get(), state.tb_page.get(), state.tb_search.get()] {
            if child != 0 {
                // SAFETY: WM_SETFONT takes a font handle (the one just made) and a redraw flag; `child` is a window of ours.
                unsafe { SendMessageW(child, WM_SETFONT, state.font.get() as usize, 1) };
            }
        }
    }
    // The back buffer's device context lets go of the old font (a font that is selected in a device context cannot be deleted).
    if let Ok(back) = state.back.try_borrow()
        && back.dc != 0
        && made[0] != 0
    {
        // SAFETY: the memory DC of the back buffer and the font just made.
        unsafe { SelectObject(back.dc, made[0]) };
    }
    for font in old.into_iter().filter(|&f| f != 0) {
        // SAFETY: a font made by `make_font`; no device context has it selected (see above; a paint selects its fonts itself) and no control uses it any more.
        unsafe { DeleteObject(font) };
    }
    invalidate(state.hwnd.get());
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

/// The corners' radii of a rounded rectangle's path are held to half its shorter side.
const KAPPA: f32 = 0.552_284_8;

/// The path of the rectangle `r` with its corners rounded by `radii` (top left, top right, bottom right, bottom left); none for a rectangle with no area.
fn round_path(r: Rect, radii: [i32; 4]) -> Option<Path> {
    if r.w <= 0 || r.h <= 0 {
        return None;
    }
    let (x, y, w, h) = (r.x as f32, r.y as f32, r.w as f32, r.h as f32);
    let most = w.min(h) / 2.0;
    let [tl, tr, br, bl] = radii.map(|v| (v as f32).clamp(0.0, most));
    let mut pb = PathBuilder::new();
    pb.move_to(x + tl, y);
    pb.line_to(x + w - tr, y);
    pb.cubic_to(x + w - tr + KAPPA * tr, y, x + w, y + tr - KAPPA * tr, x + w, y + tr);
    pb.line_to(x + w, y + h - br);
    pb.cubic_to(x + w, y + h - br + KAPPA * br, x + w - br + KAPPA * br, y + h, x + w - br, y + h);
    pb.line_to(x + bl, y + h);
    pb.cubic_to(x + bl - KAPPA * bl, y + h, x, y + h - bl + KAPPA * bl, x, y + h - bl);
    pb.line_to(x, y + tl);
    pb.cubic_to(x, y + tl - KAPPA * tl, x + tl - KAPPA * tl, y, x + tl, y);
    pb.close();
    pb.finish()
}

/// A paint of this colour and opacity for the rasterizer. The pixels of the back buffer are blue, green, red, alpha in memory and the rasterizer reads red,
/// green, blue, alpha: the two are swapped on the way in, so that what comes out is the colour that was asked for.
fn skia_paint(color: Rgb, alpha: u8) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color_rgba8((color & 0xFF) as u8, ((color >> 8) & 0xFF) as u8, ((color >> 16) & 0xFF) as u8, alpha);
    paint.anti_alias = true;
    paint
}

/// Draws on a memory device context whose bitmap is a section of 32-bit pixels (top row first): rectangles and the shapes with smooth edges are drawn into
/// the memory itself (the rasterizer of the engine does the edges), the text and the pictures of the pages by GDI (ClearType). What GDI has queued is
/// finished before the memory is touched.
struct Gdi {
    dc: isize,
    bits: usize,
    w: i32,
    h: i32,
    /// The fonts (the text, bold, the big one, the icons) and the height of a line of each, and the one in use.
    fonts: [isize; 4],
    lines: [i32; 4],
    font: usize,
    /// GDI has drawn since the memory was last brought up to date.
    pending: bool,
}

impl Gdi {
    /// The pixels as bytes (blue, green, red, alpha), after what GDI has drawn is done; `None` when there is no memory.
    fn pixels(&mut self) -> Option<&mut [u8]> {
        if self.bits == 0 || self.w <= 0 || self.h <= 0 {
            return None;
        }
        if std::mem::take(&mut self.pending) {
            // SAFETY: takes nothing; it finishes the drawing GDI has queued for this thread, so that the memory says what was drawn.
            unsafe { GdiFlush() };
        }
        let len = self.w as usize * self.h as usize * 4;
        // SAFETY: `bits` is the address CreateDIBSection gave for a bitmap of `w` by `h` 32-bit pixels, which is `len` bytes, valid for as long as the bitmap
        // lives (the whole of this paint); GDI is not drawing into it now (it was flushed above and no GDI call is made while the slice is in use), nothing
        // else points to the memory, and it is all on one thread.
        Some(unsafe { std::slice::from_raw_parts_mut(self.bits as *mut u8, len) })
    }

    fn pixmap(&mut self) -> Option<PixmapMut<'_>> {
        let (w, h) = (u32::try_from(self.w).ok()?, u32::try_from(self.h).ok()?);
        PixmapMut::from_bytes(self.pixels()?, w, h)
    }

    /// Where the rectangle is on the picture: the left, top, right and bottom edges, held to the picture; none if nothing of it is on.
    fn clip(&self, r: Rect) -> Option<(usize, usize, usize, usize)> {
        let (x0, y0) = (r.x.max(0), r.y.max(0));
        let (x1, y1) = (r.x.saturating_add(r.w).min(self.w), r.y.saturating_add(r.h).min(self.h));
        (x0 < x1 && y0 < y1).then_some((x0 as usize, y0 as usize, x1 as usize, y1 as usize))
    }

    /// Make the strip's colour transparent over the first `rows` rows, and every other pixel there opaque (the window shows what is behind it, Mica, where it
    /// has that colour).
    fn key_out(&mut self, key: Rgb, rows: i32) {
        let (w, rows) = (self.w as usize, rows.clamp(0, self.h) as usize);
        let want = [key as u8, (key >> 8) as u8, (key >> 16) as u8];
        let Some(buf) = self.pixels() else { return };
        for px in buf.as_chunks_mut::<4>().0.iter_mut().take(w * rows) {
            *px = if px[..3] == want { [0; 4] } else { [px[0], px[1], px[2], 255] };
        }
    }

    /// What GDI queued is finished (before the picture is put on the screen).
    fn finish(&mut self) {
        let _ = self.pixels();
    }
}

impl Painter for Gdi {
    fn fill_rect_alpha(&mut self, r: Rect, color: Rgb, alpha: u8) {
        match alpha {
            0 => return,
            255 => return self.fill_rect(r, color),
            _ => {}
        }
        let Some((x0, y0, x1, y1)) = self.clip(r) else { return };
        let (a, w) = (u32::from(alpha), self.w as usize);
        let src = [color & 0xFF, (color >> 8) & 0xFF, (color >> 16) & 0xFF].map(|c| c * a + 127);
        let Some(buf) = self.pixels() else { return };
        for row in buf.chunks_exact_mut(w * 4).skip(y0).take(y1 - y0) {
            let Some(cells) = row.as_chunks_mut::<4>().0.get_mut(x0..x1) else { continue };
            for px in cells {
                for (d, s) in px.iter_mut().zip(src) {
                    *d = ((s + u32::from(*d) * (255 - a)) / 255) as u8;
                }
                px[3] = 255;
            }
        }
    }

    fn fill_rect(&mut self, r: Rect, color: Rgb) {
        if self.bits == 0 {
            // A painter over a window's own device context (the sketch of a page in a box): GDI fills it.
            let rc = WinRect { left: r.x, top: r.y, right: r.x.saturating_add(r.w), bottom: r.y.saturating_add(r.h) };
            // SAFETY: `rc` is a live RECT; the brush is made here, checked, and deleted after use; `self.dc` is a valid device context.
            unsafe {
                let brush = CreateSolidBrush(colorref(color));
                if brush != 0 {
                    FillRect(self.dc, &rc, brush);
                    DeleteObject(brush);
                }
            }
            return;
        }
        let Some((x0, y0, x1, y1)) = self.clip(r) else { return };
        let (px, stride) = ([color as u8, (color >> 8) as u8, (color >> 16) as u8, 255], self.w as usize * 4);
        let Some(buf) = self.pixels() else { return };
        // The first row is filled a pixel at a time and the others are copies of it: a copy is fast whatever this crate is optimized for (it is built for size, and
        // a loop of its own over two million pixels would take twice as long as the pages do).
        let (start, end) = (y0 * stride + x0 * 4, y0 * stride + x1 * 4);
        if let Some(first) = buf.get_mut(start..end) {
            first.as_chunks_mut::<4>().0.fill(px);
        }
        for row in 1..y1 - y0 {
            if end + row * stride <= buf.len() {
                buf.copy_within(start..end, start + row * stride);
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
        self.pending = true;
        // SAFETY: `bgra` has at least `src_w * src_h * 4` bytes (checked above), which is all the call reads of
        // the pixels; `header` is a live BITMAPINFOHEADER for a 32-bit uncompressed picture, which needs no
        // colour table; `self.dc` is the valid memory DC of the back buffer. Windows keeps neither pointer.
        unsafe {
            StretchDIBits(self.dc, dest.x, dest.y, dest.w, dest.h, 0, 0, src_w, src_h, bgra.as_ptr().cast(), &header, 0, SRCCOPY);
        }
    }

    fn text(&mut self, x: i32, y: i32, s: &str, color: Rgb) {
        let w: Vec<u16> = s.encode_utf16().collect();
        self.pending = true;
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
        self.lines.get(self.font).copied().unwrap_or(16)
    }

    fn set_font(&mut self, font: Font) {
        self.font = font as usize;
        if let Some(&f) = self.fonts.get(self.font).filter(|&&f| f != 0) {
            // SAFETY: `f` is a font of the window and `self.dc` the valid memory DC of the back buffer.
            unsafe { SelectObject(self.dc, f) };
        }
    }

    fn fill_shape(&mut self, r: Rect, radii: [i32; 4], color: Rgb, alpha: u8) {
        if alpha == 0 {
            return;
        }
        let Some(path) = round_path(r, radii) else { return };
        if let Some(mut pm) = self.pixmap() {
            pm.fill_path(&path, &skia_paint(color, alpha), FillRule::Winding, Transform::identity(), None);
        }
    }

    fn stroke_round(&mut self, r: Rect, radius: i32, width: i32, color: Rgb, alpha: u8) {
        if alpha == 0 || width <= 0 {
            return;
        }
        // The line is centred on the path: the path is the rectangle taken in by half the line's width.
        let half = width as f32 / 2.0;
        let inner = Rect { x: r.x, y: r.y, w: r.w, h: r.h };
        let (x, y, w, h) = (inner.x as f32 + half, inner.y as f32 + half, inner.w as f32 - width as f32, inner.h as f32 - width as f32);
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let rad = (radius as f32 - half).clamp(0.0, w.min(h) / 2.0);
        let mut pb = PathBuilder::new();
        pb.move_to(x + rad, y);
        pb.line_to(x + w - rad, y);
        pb.cubic_to(x + w - rad + KAPPA * rad, y, x + w, y + rad - KAPPA * rad, x + w, y + rad);
        pb.line_to(x + w, y + h - rad);
        pb.cubic_to(x + w, y + h - rad + KAPPA * rad, x + w - rad + KAPPA * rad, y + h, x + w - rad, y + h);
        pb.line_to(x + rad, y + h);
        pb.cubic_to(x + rad - KAPPA * rad, y + h, x, y + h - rad + KAPPA * rad, x, y + h - rad);
        pb.line_to(x, y + rad);
        pb.cubic_to(x, y + rad - KAPPA * rad, x + rad - KAPPA * rad, y, x + rad, y);
        pb.close();
        let Some(path) = pb.finish() else { return };
        let stroke = Stroke { width: width as f32, ..Stroke::default() };
        if let Some(mut pm) = self.pixmap() {
            pm.stroke_path(&path, &skia_paint(color, alpha), &stroke, Transform::identity(), None);
        }
    }

    fn polyline(&mut self, points: &[(f32, f32)], width: f32, color: Rgb, alpha: u8) {
        let (Some(&(x0, y0)), true) = (points.first(), alpha > 0 && width > 0.0) else { return };
        let mut pb = PathBuilder::new();
        pb.move_to(x0, y0);
        for &(x, y) in points.iter().skip(1) {
            pb.line_to(x, y);
        }
        let Some(path) = pb.finish() else { return };
        let stroke = Stroke { width, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Stroke::default() };
        if let Some(mut pm) = self.pixmap() {
            pm.stroke_path(&path, &skia_paint(color, alpha), &stroke, Transform::identity(), None);
        }
    }
}

fn paint(state: &WindowState, hwnd: isize) {
    let mut ps = PaintStruct { hdc: 0, erase: 0, paint: WinRect::default(), restore: 0, inc_update: 0, reserved: [0; 32] };
    // SAFETY: `ps` is a live PAINTSTRUCT; BeginPaint and EndPaint are paired below on every path.
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
    if hdc != 0 {
        paint_to(state, hdc);
    }
    // SAFETY: pairs the BeginPaint above with the same window and the same PAINTSTRUCT.
    unsafe { EndPaint(hwnd, &ps) };
}

/// Draw the reader on the back buffer (the whole client area) and put that on the window's device context `hdc`.
fn paint_to(state: &WindowState, hdc: isize) {
    let (w, h) = client_size(state.hwnd.get());
    if w <= 0 || h <= 0 {
        return;
    }
    let Ok(mut back) = state.back.try_borrow_mut() else { return };
    if back.dc == 0 || back.w != w || back.h != h {
        resize_back(&mut back, hdc, w, h);
    }
    if back.dc == 0 || back.bitmap == 0 || back.bits == 0 {
        return;
    }
    let fonts = [state.font.get(), state.fonts[0].get(), state.fonts[1].get(), state.fonts[2].get()];
    let mut gdi = Gdi { dc: back.dc, bits: back.bits, w, h, fonts, lines: [16; 4], font: 0, pending: false };
    for (i, &f) in fonts.iter().enumerate() {
        if f != 0 {
            // SAFETY: the memory DC of the back buffer and a font of the window.
            unsafe { SelectObject(back.dc, f) };
            if let Some(line) = gdi.lines.get_mut(i) {
                *line = line_height(back.dc);
            }
        }
    }
    gdi.set_font(Font::Body);
    let mut present = true;
    if let Ok(mut guard) = state.handler.try_borrow_mut()
        && let Some(handler) = guard.as_mut()
    {
        present = handler.paint(&mut gdi);
    }
    if present {
        gdi.finish();
        // Where the title strip is the colour of the strip, the window shows Mica through it.
        if state.mica.get() && state.custom.get() && !state.fullscreen.get() {
            gdi.key_out(state.key.get(), scale(state.dpi.get(), crate::tabs::TITLE_H as i32));
        }
        // SAFETY: both DCs are valid and the memory DC holds a bitmap of w by h pixels.
        unsafe { BitBlt(hdc, 0, 0, w, h, back.dc, 0, 0, SRCCOPY) };
    }
}

/// Make the back buffer `w` by `h` (and its device context the first time): a section of 32-bit pixels, top row first, that is drawn in directly.
fn resize_back(back: &mut Back, hdc: isize, w: i32, h: i32) {
    let info = BitmapInfoHeader { size: std::mem::size_of::<BitmapInfoHeader>() as u32, width: w, height: -h, planes: 1, bit_count: 32, compression: 0, size_image: 0, x_pels: 0, y_pels: 0, clr_used: 0, clr_important: 0 };
    let mut bits: *mut c_void = null_mut();
    // SAFETY: `hdc` is the valid DC of this paint; `info` is a live header of a 32-bit top-down picture; `bits` is a live pointer Windows fills with the address
    // of the picture's memory (w by h by 4 bytes), which lives until the bitmap is deleted. The old bitmap is deselected (by selecting the new one in the
    // same DC) before it is deleted; handles are checked before use.
    unsafe {
        if back.dc == 0 {
            back.dc = CreateCompatibleDC(hdc);
            if back.dc != 0 {
                SetStretchBltMode(back.dc, COLORONCOLOR);
            }
        }
        let bitmap = if back.dc != 0 { CreateDIBSection(hdc, &info, 0, &mut bits, 0, 0) } else { 0 };
        if back.dc != 0 && bitmap != 0 && !bits.is_null() {
            SelectObject(back.dc, bitmap);
            if back.bitmap != 0 {
                DeleteObject(back.bitmap);
            }
            back.bitmap = bitmap;
            back.bits = bits as usize;
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

/// The box that picks a file: to open (`save_name` is `None`) or to write (`Some(name)`, the name the box starts with; it asks before
/// it replaces a file, and a name without an extension gets `.pdf`, or `.docx` when the name it starts with ends in that).
fn file_dialog(owner: isize, title: &str, filters: &[(String, String)], save_name: Option<&str>) -> Option<PathBuf> {
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
    if let Some(name) = save_name {
        // The name it starts with (cut to leave room for the closing zero).
        for (slot, unit) in file.iter_mut().zip(name.encode_utf16().take(32_000)) {
            *slot = unit;
        }
    }
    // (A name without an extension gets the one the box started with: .docx for a Word file, else .pdf.)
    let def_ext = wide(if save_name.is_some_and(|n| n.to_ascii_lowercase().ends_with(".docx")) { "docx" } else { "pdf" });
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
        flags: if save_name.is_some() { OFN_PATHMUSTEXIST | OFN_OVERWRITEPROMPT | OFN_NOCHANGEDIR | OFN_EXPLORER } else { OFN_PATHMUSTEXIST | OFN_FILEMUSTEXIST | OFN_NOCHANGEDIR | OFN_EXPLORER },
        file_offset: 0,
        file_extension: 0,
        def_ext: if save_name.is_some() { def_ext.as_ptr() } else { null() },
        cust_data: 0,
        hook: null(),
        template_name: null(),
        reserved: null_mut(),
        reserved_dw: 0,
        flags_ex: 0,
    };
    // SAFETY: `ofn` is a live OPENFILENAMEW of the size it declares; the strings it points to (`filter`, `title`, `def_ext`)
    // and the buffer `file` (of the length declared in `max_file`) outlive the call; the box is modal and Windows
    // keeps none of the pointers after it returns.
    let chosen = unsafe { if save_name.is_some() { GetSaveFileNameW(&mut ofn) } else { GetOpenFileNameW(&mut ofn) } } != 0;
    if !chosen {
        return None;
    }
    let len = file.iter().position(|&c| c == 0).unwrap_or(file.len());
    Some(PathBuf::from(OsString::from_wide(file.get(..len).unwrap_or(&[]))))
}

/// The box that picks a folder (the shell's tree of folders, with a button to make a new one).
fn folder_dialog(owner: isize, title: &str) -> Option<PathBuf> {
    /// BIF_RETURNONLYFSDIRS and BIF_NEWDIALOGSTYLE.
    const FLAGS: u32 = 0x1 | 0x40;
    let title = wide(title);
    let mut name = [0u16; 260];
    let mut info = BrowseInfo { owner, root: null(), display_name: name.as_mut_ptr(), title: title.as_ptr(), flags: FLAGS, callback: null(), param: 0, image: 0 };
    // SAFETY: `info` is a live BROWSEINFOW whose pointers (`name`, which has the 260 units Windows writes at the most, and `title`) outlive the call; the box
    // is modal and Windows keeps none of them. It returns a list of items to be freed with CoTaskMemFree, or null if it was cancelled.
    let list = unsafe { SHBrowseForFolderW(&mut info) };
    if list.is_null() {
        return None;
    }
    let mut path = [0u16; 260];
    // SAFETY: `list` came from SHBrowseForFolderW and is freed once, after this; `path` has the 260 units MAX_PATH calls for.
    let got = unsafe { SHGetPathFromIDListW(list, path.as_mut_ptr()) } != 0;
    // SAFETY: `list` is the item list the shell allocated, not used again.
    unsafe { CoTaskMemFree(list) };
    let len = path.iter().position(|&c| c == 0).unwrap_or(path.len());
    got.then(|| PathBuf::from(OsString::from_wide(path.get(..len).unwrap_or(&[])))).filter(|p| !p.as_os_str().is_empty())
}

struct DialogState {
    done: Cell<bool>,
    accepted: Cell<bool>,
    /// The button of a form box that closed it, counting from 1 (0: it was closed with OK or Cancel).
    pressed: Cell<usize>,
    /// The sketch of a page a form box draws, if it has one.
    sketch: RefCell<Option<Sketch>>,
}

/// What a form box draws as a sketch of a page: where, the shape of the page, the controls that hold the six places and the tick box that
/// mirrors them, what the fields stand for, and the font and line height to draw with.
struct Sketch {
    rect: Rect,
    shape: (i32, i32),
    slots: [isize; 6],
    mirror: isize,
    fields: [String; 4],
    left_page: bool,
    font: isize,
    line: i32,
}

/// Draw the sketch (a white page with the text of the six places on it, each as typed with its fields filled in).
fn paint_sketch(hwnd: isize, sketch: &Sketch) {
    let mut ps = PaintStruct { hdc: 0, erase: 0, paint: WinRect::default(), restore: 0, inc_update: 0, reserved: [0; 32] };
    // SAFETY: `ps` is a live PAINTSTRUCT; BeginPaint and EndPaint are paired below on every path.
    let dc = unsafe { BeginPaint(hwnd, &mut ps) };
    if dc != 0 {
        // SAFETY: selects the dialog's font into the paint DC (and back below); plain handles.
        let old = unsafe { SelectObject(dc, sketch.font) };
        let mut g = Gdi { dc, bits: 0, w: 0, h: 0, fonts: [0; 4], lines: [sketch.line; 4], font: 0, pending: false };
        let (r, (pw, ph)) = (sketch.rect, sketch.shape);
        // The page: as big as fits, in the middle, with a grey edge.
        let (w, h) = if r.w * ph <= r.h * pw { (r.w, r.w * ph / pw.max(1)) } else { (r.h * pw / ph.max(1), r.h) };
        let page = Rect { x: r.x + (r.w - w) / 2, y: r.y + (r.h - h) / 2, w, h };
        g.fill_rect(page, 0x808080);
        g.fill_rect(Rect { x: page.x + 1, y: page.y + 1, w: page.w - 2, h: page.h - 2 }, 0xFFFFFF);
        // The six lines. (The left and the right ones change sides on a left-hand page when that is asked.)
        // SAFETY: a plain query message to the tick box, which is alive while the box is.
        let mirrored = sketch.left_page && unsafe { SendMessageW(sketch.mirror, BM_GETCHECK, 0, 0) } == 1;
        let f = &sketch.fields;
        for i in 0..6 {
            let at = if mirrored && i % 3 != 1 { i / 3 * 3 + 2 - i % 3 } else { i };
            let text = window_text(sketch.slots.get(at).copied().unwrap_or(0), 400).replace("{page}", &f[0]).replace("{pages}", &f[1]).replace("{date}", &f[2]).replace("{file}", &f[3]);
            let tw = g.text_width(&text);
            let x = match i % 3 {
                0 => page.x + 6,
                1 => page.x + (page.w - tw) / 2,
                _ => page.x + page.w - 6 - tw,
            };
            g.text(x, if i < 3 { page.y + 6 } else { page.y + page.h - 6 - sketch.line }, &text, 0x303030);
        }
        // SAFETY: puts the DC's font back.
        unsafe { SelectObject(dc, old) };
    }
    // SAFETY: pairs the BeginPaint above with the same window and the same PAINTSTRUCT.
    unsafe { EndPaint(hwnd, &ps) };
}

/// The control ids in a form box: the number of an item from `FORM_ITEM` (`IDOK` and `IDCANCEL` are the buttons at the foot), the buttons of
/// the form from `FORM_BUTTON`.
const FORM_ITEM: usize = 100;
const FORM_BUTTON: usize = 1000;

unsafe extern "system" fn dialog_proc(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize {
    // SAFETY: the user-data slot holds 0 or the pointer to the `DialogState` on the stack of `make_dialog`'s caller, which
    // outlives the dialog window (it is destroyed before that function returns); used on the same thread only.
    let state = unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const DialogState).as_ref() };
    if let Some(state) = state {
        match msg {
            WM_PAINT if state.sketch.borrow().is_some() => {
                if let Some(sketch) = state.sketch.borrow().as_ref() {
                    paint_sketch(hwnd, sketch);
                }
                return 0;
            }
            // Something in a form box was typed in or changed: the sketch is drawn again.
            WM_COMMAND if (FORM_ITEM..FORM_BUTTON).contains(&(loword(wparam) as usize)) => {
                if let Some(sketch) = state.sketch.borrow().as_ref() {
                    let r = WinRect { left: sketch.rect.x, top: sketch.rect.y, right: sketch.rect.x + sketch.rect.w, bottom: sketch.rect.y + sketch.rect.h };
                    // SAFETY: `r` is a live RECT; the window is the box itself.
                    unsafe { InvalidateRect(hwnd, &r, 0) };
                }
            }
            WM_COMMAND if loword(wparam) as usize >= FORM_BUTTON && wparam >> 16 == 0 => {
                state.pressed.set(loword(wparam) as usize - FORM_BUTTON + 1);
                state.accepted.set(true);
                state.done.set(true);
                return 0;
            }
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

/// A box of its own over `owner`, whose inside is `w` by `h` units of a 96th of an inch: its window and the module. `state` is what the
/// box's procedure reads; the caller keeps it alive until it has called `end_modal`.
fn make_dialog(owner: isize, dpi: u32, title: &str, (w, h): (i32, i32), state: &DialogState) -> Option<(isize, isize)> {
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
        icon: load_icon(instance, dpi, false),
        // SAFETY: a small integer resource id in the pointer slot is how LoadCursorW names a stock cursor.
        cursor: unsafe { LoadCursorW(0, IDC_ARROW as *const u16) },
        background: 16, // COLOR_BTNFACE + 1: the same grey as the labels and buttons on it
        menu_name: null(),
        class_name: class.as_ptr(),
        icon_small: load_icon(instance, dpi, true),
    };
    // SAFETY: `wc` is a live WNDCLASSEXW whose strings outlive the call; registering twice fails harmlessly.
    unsafe { RegisterClassExW(&wc) };

    let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
    let mut frame = WinRect { left: 0, top: 0, right: s(w), bottom: s(h) };
    // SAFETY: `frame` is a live RECT that is rewritten in place.
    unsafe { AdjustWindowRectExForDpi(&mut frame, style, 0, WS_EX_DLGMODALFRAME, dpi) };
    let (mut fw, mut fh) = (frame.right - frame.left, frame.bottom - frame.top);
    // A box is never bigger than the work area of the screen it opens on, and it is moved in until all of it is there: its buttons are at its foot.
    let work = monitor_work(owner);
    if let Some(w) = &work {
        fw = fw.min(w.right - w.left);
        fh = fh.min(w.bottom - w.top);
    }
    let mut at = WinRect::default();
    // SAFETY: `at` is a live RECT that Windows fills in.
    unsafe { GetWindowRect(owner, &mut at) };
    let (mut x, mut y) = (at.left + ((at.right - at.left) - fw) / 2, at.top + ((at.bottom - at.top) - fh) / 3);
    if let Some(w) = &work {
        x = x.clamp(w.left, (w.right - fw).max(w.left));
        y = y.clamp(w.top, (w.bottom - fh).max(w.top));
    }
    let title_w = wide(title);
    // SAFETY: the strings are zero-terminated UTF-16 that outlive the call; `owner` is the main window; no
    // creation parameter is passed. A handle of 0 means failure and is checked.
    let dialog = unsafe { CreateWindowExW(WS_EX_DLGMODALFRAME, class.as_ptr(), title_w.as_ptr(), style, x, y, fw, fh, owner, 0, instance, null_mut()) };
    if dialog == 0 {
        return None;
    }
    // SAFETY: stores the address of `state` for `dialog_proc`; the caller keeps `state` alive until after the window is destroyed
    // (`end_modal`, which clears the slot), and the slot is only read on this thread.
    unsafe { SetWindowLongPtrW(dialog, GWLP_USERDATA, std::ptr::from_ref(state) as isize) };
    Some((dialog, instance))
}

/// The height of the inside of a box made by `make_dialog`, in units of a 96th of an inch (less than it was asked for when the box was cut to the work
/// area of the screen).
fn client_units(dialog: isize, dpi: u32) -> i32 {
    let mut r = WinRect::default();
    // SAFETY: `r` is a live RECT that Windows fills in for a window that is alive.
    let ok = unsafe { GetClientRect(dialog, &mut r) } != 0;
    if ok && r.bottom > 0 { r.bottom * 96 / dpi.max(1) as i32 } else { i32::MAX }
}

/// A control inside a box made by `make_dialog`, at `(x, y, w, h)` units of a 96th of an inch, in the font of the program; 0 if it was not made.
#[allow(clippy::too_many_arguments)]
fn dialog_child(dialog: isize, instance: isize, dpi: u32, font: isize, class: &str, text: &str, style: u32, ex: u32, (x, y, w, h): (i32, i32, i32, i32), id: usize) -> isize {
    let s = |v: i32| v * dpi as i32 / 96;
    let (c, t) = (wide(class), wide(text));
    // SAFETY: the strings are zero-terminated UTF-16 that outlive the call; `dialog` is the parent window; the
    // control id goes in the menu slot as Windows defines for child windows. 0 means failure and is checked.
    let child = unsafe { CreateWindowExW(ex, c.as_ptr(), t.as_ptr(), WS_CHILD | WS_VISIBLE | style, s(x), s(y), s(w), s(h), dialog, id as isize, instance, null_mut()) };
    if child != 0 && font != 0 {
        // SAFETY: WM_SETFONT takes a font handle, which `font` is, and a redraw flag.
        unsafe { SendMessageW(child, WM_SETFONT, font as usize, 1) };
    }
    child
}

/// Show the box and handle its messages until it is closed (the owner is disabled meanwhile); `focus` is the control the keyboard starts in.
fn modal_loop(owner: isize, dialog: isize, state: &DialogState, focus: isize) {
    // SAFETY: plain handles of windows made by the caller; the owner is enabled again by `end_modal` on every path.
    unsafe {
        EnableWindow(owner, 0);
        ShowWindow(dialog, SW_SHOW);
        if focus != 0 {
            SetFocus(focus);
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
}

/// Enable the owner again and destroy the box (its user-data slot is cleared first, so that nothing can reach the state after this).
fn end_modal(owner: isize, dialog: isize) {
    // SAFETY: plain handles; enables the owner again, and destroys the dialog window, which makes the state (still alive in the
    // caller) unreachable: its slot is cleared first.
    unsafe {
        EnableWindow(owner, 1);
        SetWindowLongPtrW(dialog, GWLP_USERDATA, 0);
        DestroyWindow(dialog);
        SetForegroundWindow(owner);
    }
}

/// A box with a line of text to fill in. Modal: it handles messages itself until it is closed.
#[allow(clippy::too_many_arguments)]
fn input_dialog(owner: isize, dpi: u32, font: isize, title: &str, prompt: &str, secret: bool, initial: &str, ok: &str, cancel: &str) -> Option<String> {
    let state = DialogState { done: Cell::new(false), accepted: Cell::new(false), pressed: Cell::new(0), sketch: RefCell::new(None) };
    let (dialog, instance) = make_dialog(owner, dpi, title, (380, 138), &state)?;
    let child = |class: &str, text: &str, style: u32, ex: u32, at: (i32, i32, i32, i32), id: usize| dialog_child(dialog, instance, dpi, font, class, text, style, ex, at, id);
    child("STATIC", prompt, 0, 0, (16, 14, 348, 20), 100);
    let edit = child("EDIT", initial, WS_TABSTOP | ES_AUTOHSCROLL | if secret { ES_PASSWORD } else { 0 }, WS_EX_CLIENTEDGE, (16, 38, 348, 24), 101);
    child("BUTTON", ok, WS_TABSTOP | BS_DEFPUSHBUTTON, 0, (176, 88, 90, 28), IDOK);
    child("BUTTON", cancel, WS_TABSTOP | BS_PUSHBUTTON, 0, (274, 88, 90, 28), IDCANCEL);
    modal_loop(owner, dialog, &state, edit);
    let text = if state.accepted.get() && edit != 0 {
        let mut buf = text_units(edit);
        let text = String::from_utf16_lossy(&buf);
        // The typed characters are overwritten (they may be a password).
        buf.fill(0);
        std::hint::black_box(&buf);
        Some(text)
    } else {
        None
    };
    // SAFETY: clears the box's text before it goes (the edit control is alive).
    unsafe {
        if edit != 0 {
            SetWindowTextW(edit, [0u16].as_ptr());
        }
    }
    end_modal(owner, dialog);
    text
}

/// The text of a control, as UTF-16 (without the closing zero), for text that is wiped after it is used.
fn text_units(control: isize) -> Vec<u16> {
    // SAFETY: the control is alive; `buf` has room for the length asked for plus the closing zero, which is what the call is told; the
    // length is read just before.
    unsafe {
        let n = GetWindowTextLengthW(control).max(0) as usize;
        let mut buf = vec![0u16; n + 1];
        let got = GetWindowTextW(control, buf.as_mut_ptr(), buf.len() as i32).max(0) as usize;
        buf.truncate(got);
        buf
    }
}

/// A box with several things to fill in, laid out as `spec` says (`Action::AskForm`). Modal. `None` if it was cancelled or could not be made.
fn form_dialog(owner: isize, dpi: u32, font: isize, spec: &FormSpec) -> Option<FormAnswer> {
    let state = DialogState { done: Cell::new(false), accepted: Cell::new(false), pressed: Cell::new(0), sketch: RefCell::new(None) };
    let (dialog, instance) = make_dialog(owner, dpi, &spec.title, spec.size, &state)?;
    let child = |class: &str, text: &str, style: u32, ex: u32, at: (i32, i32, i32, i32), id: usize| dialog_child(dialog, instance, dpi, font, class, text, style, ex, at, id);
    let mut made: Vec<(FormKind, isize)> = Vec::with_capacity(spec.items.len());
    let (mut buttons, mut focus) = (0usize, 0isize);
    for (k, item) in spec.items.iter().enumerate() {
        let (class, style, ex) = match item.kind {
            FormKind::Label => ("STATIC", SS_NOPREFIX, 0),
            FormKind::Group => ("BUTTON", BS_GROUPBOX, 0),
            FormKind::Edit => ("EDIT", WS_TABSTOP | ES_AUTOHSCROLL, WS_EX_CLIENTEDGE),
            FormKind::Secret => ("EDIT", WS_TABSTOP | ES_AUTOHSCROLL | ES_PASSWORD, WS_EX_CLIENTEDGE),
            FormKind::Combo => ("COMBOBOX", WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWN | CBS_AUTOHSCROLL, 0),
            FormKind::Pick => ("COMBOBOX", WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST, 0),
            FormKind::Check => ("BUTTON", WS_TABSTOP | BS_AUTOCHECKBOX, 0),
            FormKind::Button => ("BUTTON", WS_TABSTOP | BS_PUSHBUTTON, 0),
        };
        let id = if item.kind == FormKind::Button { FORM_BUTTON + buttons } else { FORM_ITEM + k };
        buttons += usize::from(item.kind == FormKind::Button);
        let text = if matches!(item.kind, FormKind::Pick) { "" } else { item.text.as_str() };
        let h = child(class, text, style, ex, item.rect, id);
        if h != 0 {
            if focus == 0 && matches!(item.kind, FormKind::Edit | FormKind::Secret | FormKind::Combo) {
                focus = h;
            }
            let units: Vec<Vec<u16>> = item.choices.iter().map(|c| wide(c)).collect();
            for entry in units.iter().filter(|_| matches!(item.kind, FormKind::Combo | FormKind::Pick)) {
                // SAFETY: CB_ADDSTRING takes a zero-terminated UTF-16 string, which `wide` makes and which outlives the call.
                unsafe { SendMessageW(h, CB_ADDSTRING, 0, entry.as_ptr() as isize) };
            }
            // SAFETY: plain messages to a control that was just made: the entry a list starts at, a tick box ticked or not.
            unsafe {
                match item.kind {
                    FormKind::Pick => SendMessageW(h, CB_SETCURSEL, item.selected, 0),
                    FormKind::Check => SendMessageW(h, BM_SETCHECK, item.selected.min(1), 0),
                    _ => 0,
                };
            }
        }
        made.push((item.kind, h));
    }
    if let Some(p) = &spec.preview {
        let s = |v: i32| v * dpi as i32 / 96;
        let handle = |k: usize| made.get(k).map_or(0, |m| m.1);
        let (x, y, w, h) = p.rect;
        *state.sketch.borrow_mut() = Some(Sketch {
            rect: Rect { x: s(x), y: s(y), w: s(w), h: s(h) },
            shape: p.shape,
            slots: p.slots.map(handle),
            mirror: handle(p.mirror),
            fields: p.fields.clone(),
            left_page: p.left_page,
            font,
            line: s(16),
        });
    }
    // (The buttons are at the foot of the inside the box has, which is less than it was asked for when the box was cut to the screen.)
    let foot = client_units(dialog, dpi).min(spec.size.1) - 36;
    child("BUTTON", &spec.ok, WS_TABSTOP | BS_DEFPUSHBUTTON, 0, (spec.size.0 - 202, foot, 90, 28), IDOK);
    child("BUTTON", &spec.cancel, WS_TABSTOP | BS_PUSHBUTTON, 0, (spec.size.0 - 104, foot, 90, 28), IDCANCEL);
    modal_loop(owner, dialog, &state, focus);
    let answer = state.accepted.get().then(|| {
        // SAFETY: plain query messages (the entry a list is at, whether a tick box is ticked) to a control that is alive; they take no arguments.
        let ask = |h: isize, msg: u32| unsafe { SendMessageW(h, msg, 0, 0) };
        let mut values = Vec::new();
        for &(kind, h) in &made {
            match kind {
                FormKind::Edit | FormKind::Combo => values.push(window_text(h, 4_000)),
                FormKind::Secret => {
                    let mut units = text_units(h);
                    values.push(String::from_utf16_lossy(&units));
                    // The typed characters are overwritten (they are a password).
                    units.fill(0);
                    std::hint::black_box(&units);
                }
                FormKind::Pick => values.push(ask(h, CB_GETCURSEL).max(0).to_string()),
                FormKind::Check => values.push(String::from(if ask(h, BM_GETCHECK) == 1 { "1" } else { "0" })),
                FormKind::Label | FormKind::Group | FormKind::Button => {}
            }
        }
        FormAnswer { values, button: state.pressed.get().checked_sub(1) }
    });
    // What is left in a password box goes before the box does.
    for &(_, h) in made.iter().filter(|m| m.0 == FormKind::Secret) {
        // SAFETY: clears the text of an edit control that is alive.
        unsafe { SetWindowTextW(h, [0u16].as_ptr()) };
    }
    end_modal(owner, dialog);
    answer
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

/// Show the find box at a place and give it the keyboard (with its text selected), or hide it and take the keyboard back.
fn show_find(state: &WindowState, hwnd: isize, rect: Option<Rect>) {
    let edit = find_edit(state);
    if edit == 0 {
        return;
    }
    let in_toolbar = edit == state.tb_search.get();
    // The box over the pages is put away when the toolbar's is the one in use.
    if in_toolbar {
        place_child(state.find.get(), Rect { x: 0, y: 0, w: 0, h: 0 });
    }
    match rect {
        Some(r) => {
            let had_focus = {
                // SAFETY: reads which window has the keyboard.
                unsafe { GetFocus() == edit }
            };
            if !in_toolbar {
                place_child(edit, r);
            }
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
            if !in_toolbar {
                place_child(edit, Rect { x: 0, y: 0, w: 0, h: 0 });
            }
        }
    }
}

/// Show the box a form field or text box is typed in at a place, with this text (selected) and the keyboard. The box for one line
/// or the one for several is used as `b` says.
fn show_field(state: &WindowState, hwnd: isize, b: FieldBox) {
    let (edit, other) = if b.multiline { (state.field_multi.get(), state.field_single.get()) } else { (state.field_single.get(), state.field_multi.get()) };
    if edit == 0 {
        return;
    }
    // The box that was in use before is put away without telling the reader: it has moved on to this one already.
    state.field_active.set(0);
    let text = wide(&b.text.replace("\r\n", "\n").replace('\n', "\r\n"));
    let limit = if b.max_len > 0 { (b.max_len as usize).min(MAX_FIELD_CHARS) } else { MAX_FIELD_CHARS };
    // SAFETY: plain handles of child windows of ours; `text` is zero-terminated UTF-16 that outlives the call; EM_LIMITTEXT takes a
    // number of characters; the whole text is selected (a start of 0 and an end of -1).
    unsafe {
        if other != 0 {
            ShowWindow(other, SW_HIDE);
        }
        SendMessageW(edit, EM_LIMITTEXT, limit, 0);
        SetWindowTextW(edit, text.as_ptr());
    }
    place_child(edit, b.rect);
    state.field_active.set(edit);
    // SAFETY: plain handle of the edit box just placed.
    unsafe {
        SetFocus(edit);
        SendMessageW(edit, EM_SETSEL, 0, -1);
    }
    let _ = hwnd;
}

/// Put the box away, keeping nothing: the reader has asked for that.
fn hide_field(state: &WindowState, hwnd: isize) {
    let edit = state.field_active.replace(0);
    if edit != 0 {
        // SAFETY: plain handles; the keyboard goes back to the main window, and the box is hidden.
        unsafe {
            if GetFocus() == edit {
                SetFocus(hwnd);
            }
            ShowWindow(edit, SW_HIDE);
        }
    }
}

/// The person has left the box: its text goes to the reader, with how it was left, and the box is put away.
fn finish_field(state: &WindowState, exit: FieldExit) {
    let edit = state.field_active.replace(0);
    if edit == 0 {
        return;
    }
    let text = window_text(edit, MAX_FIELD_CHARS).replace("\r\n", "\n").replace('\r', "\n");
    let hwnd = state.hwnd.get();
    // SAFETY: plain handles; the keyboard goes back to the main window and the box is hidden (the notification that follows is
    // ignored: no box is in use any more).
    unsafe {
        if GetFocus() == edit {
            SetFocus(hwnd);
        }
        ShowWindow(edit, SW_HIDE);
    }
    dispatch(state, Event::FieldDone { text, exit });
}

/// A list of choices at a place of the client area; the number of the one chosen.
fn choose(hwnd: isize, items: &[String], selected: Option<usize>, at: (i32, i32)) -> Option<usize> {
    // SAFETY: creates an empty pop-up menu; the handle is checked.
    let menu = unsafe { CreatePopupMenu() };
    if menu == 0 {
        return None;
    }
    for (i, item) in items.iter().take(MAX_CHOICES).enumerate() {
        let label = wide(&item.replace('&', "&&"));
        let flags = MF_STRING | if selected == Some(i) { MF_CHECKED } else { 0 };
        // SAFETY: `menu` is the menu just made; the item's number is its place plus one (0 is "nothing chosen"); `label` is
        // zero-terminated UTF-16 that outlives the call (the menu copies it).
        unsafe { AppendMenuW(menu, flags, i + 1, label.as_ptr()) };
    }
    let mut at = Point { x: at.0, y: at.1 };
    // SAFETY: `at` is a live POINT turned into screen coordinates; the menu belongs to this window and is destroyed after use;
    // TPM_RETURNCMD answers with the item's number (0 if the menu was dismissed) instead of posting a message.
    let answer = unsafe {
        ClientToScreen(hwnd, &mut at);
        let answer = TrackPopupMenu(menu, TPM_RETURNCMD_NONOTIFY, at.x, at.y, 0, hwnd, null());
        DestroyMenu(menu);
        answer
    };
    usize::try_from(answer).ok().filter(|&n| n > 0).map(|n| n - 1)
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

// --- the layout, the frame and the look --------------------------------------------------------------------------------

fn scale(dpi: u32, v: i32) -> i32 {
    (i64::from(v) * i64::from(dpi) / 96) as i32
}

fn metric(index: i32, dpi: u32) -> i32 {
    // SAFETY: takes two plain numbers and reads a system setting.
    unsafe { GetSystemMetricsForDpi(index, dpi) }
}

/// The width of the system's buttons (minimize, maximize, close) at the right end of the window's top, which Windows draws over the title strip.
fn caption_width(state: &WindowState) -> i32 {
    let mut r = WinRect::default();
    // SAFETY: DWMWA_CAPTION_BUTTON_BOUNDS fills a RECT, which is the 16 bytes told; `r` is live and Windows keeps no pointer to it.
    let hr = unsafe { DwmGetWindowAttribute(state.hwnd.get(), DWMWA_CAPTION_BUTTON_BOUNDS, (&raw mut r).cast(), 16) };
    let w = r.right - r.left;
    if hr == 0 && w > 0 { w } else { scale(state.dpi.get(), 138) }
}

/// Tell the reader how big the client area is (all of it: the reader puts the title strip, the command bar and the pages in it) and how the frame is, when that
/// has changed.
fn apply_layout(state: &WindowState) {
    let (cw, ch) = client_size(state.hwnd.get());
    if cw <= 0 || ch <= 0 {
        return;
    }
    let frame = (state.custom.get() && !state.fullscreen.get(), caption_width(state));
    if state.frame_told.replace(Some(frame)) != Some(frame) {
        dispatch(state, Event::Frame { custom: frame.0, caption: frame.1 });
    }
    if state.size.get() != (cw, ch) {
        state.size.set((cw, ch));
        dispatch(state, Event::Size { w: cw, h: ch });
    }
}

/// Extend the frame of the window over the title strip (so that Windows draws its buttons there, and Mica where the strip is transparent), or not at all in
/// full screen and with a high-contrast theme (the system's own title bar).
fn apply_frame(state: &WindowState) {
    let strip = if state.custom.get() && !state.fullscreen.get() { scale(state.dpi.get(), crate::tabs::TITLE_H as i32) } else { 0 };
    let margins = Margins { left: 0, right: 0, top: strip, bottom: 0 };
    // SAFETY: `margins` is a live MARGINS that Windows reads and does not keep; the window is ours.
    unsafe { DwmExtendFrameIntoClientArea(state.hwnd.get(), &margins) };
}

/// Windows is set to a high-contrast theme, or is not any more: the frame is the system's own (with its title bar above the window) in a high-contrast
/// theme, and the reader is told the colours.
fn refresh_contrast(state: &WindowState) {
    let colors = high_contrast();
    let custom = colors.is_none();
    if state.custom.replace(custom) != custom {
        apply_frame(state);
        // SAFETY: plain numbers and the handle of this window; the frame is calculated again.
        unsafe { SetWindowPos(state.hwnd.get(), 0, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZOMORDER | SWP_NOOWNERZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED) };
    }
    dispatch(state, Event::HighContrast(colors));
    apply_layout(state);
}

/// The brush a window is filled with where a child asks the window for its ground (the colour of the title strip), or the system's.
fn face_brush(state: &WindowState) -> isize {
    match state.face_brush.get() {
        0 => {
            // SAFETY: the system's own brush, shared and never freed.
            unsafe { GetSysColorBrush(COLOR_BTNFACE) }
        }
        brush => brush,
    }
}

/// A brush of a colour, or 0 if there is none to make.
fn brush_of(color: Rgb) -> isize {
    // SAFETY: plain number; the brush is the caller's, deleted by whoever replaces it.
    unsafe { CreateSolidBrush(colorref(color)) }
}

/// The tree takes the light or the dark form of the look of the file windows (its scroll bars and its chevrons follow).
fn set_tree_theme(tree: isize, dark: bool) {
    if tree == 0 {
        return;
    }
    let name = wide(if dark { "DarkMode_Explorer" } else { "Explorer" });
    // SAFETY: SetWindowTheme takes a window of ours and a zero-terminated UTF-16 name that outlives the call (no sub id list); a system without that theme answers
    // with an error, which leaves the tree as it is. TVM_SETEXTENDEDSTYLE takes a mask and the styles (TVS_EX_DOUBLEBUFFER) and keeps nothing.
    unsafe {
        SetWindowTheme(tree, name.as_ptr(), null());
        SendMessageW(tree, TVM_SETEXTENDEDSTYLE, TVS_EX_DOUBLEBUFFER, TVS_EX_DOUBLEBUFFER as isize);
    }
}

/// Take how the reader says the window looks (View > Interface, the accent, the language): the frame is made light or dark, the boxes and the tree of bookmarks
/// have the colours, the fonts are chosen by the language, and the colour of the strip is the one the window is transparent in. What stays as the system has
/// it: the dialogs and the menus.
fn apply_theme(state: &WindowState, spec: ThemeSpec) {
    let hwnd = state.hwnd.get();
    let zh_changed = state.zh.replace(spec.zh) != spec.zh;
    state.theme.set(Some(spec));
    state.key.set(spec.strip);
    for (cell, new) in [(&state.face_brush, brush_of(spec.strip)), (&state.field_brush, brush_of(spec.field_bg))] {
        let old = cell.replace(new);
        if old != 0 {
            // SAFETY: a brush made by `brush_of` for this window; the window is not drawn with it any more (it is redrawn below).
            unsafe { DeleteObject(old) };
        }
    }
    let on = i32::from(spec.dark);
    // SAFETY: DwmSetWindowAttribute takes this window, the number of an attribute and a pointer to a value of the size told (a 4-byte BOOL that lives
    // through the call); a system that does not know the attribute answers with an error, which does not matter.
    unsafe { DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, (&raw const on).cast(), 4) };
    set_tree_theme(state.tree.get(), spec.dark);
    for (message, color) in [(TVM_SETBKCOLOR, colorref(spec.tree_bg)), (TVM_SETTEXTCOLOR, colorref(spec.text)), (TVM_SETLINECOLOR, colorref(spec.tree_line))] {
        // SAFETY: plain messages to the tree of bookmarks, with a colour; a window that is not there is not an error.
        unsafe { SendMessageW(state.tree.get(), message, 0, color as isize) };
    }
    if zh_changed {
        rebuild_fonts(state);
    }
    // SAFETY: this window and everything in it is invalidated and erased (no rectangle, no region).
    unsafe { RedrawWindow(hwnd, null(), 0, RDW_INVALIDATE_ALL) };
}

/// Is Windows set to dark apps? Read from the registry (`AppsUseLightTheme` of the current user's Personalize key), never written.
pub fn system_uses_dark() -> bool {
    let (key, name) = (wide("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"), wide("AppsUseLightTheme"));
    let (mut data, mut size) = (1u32, 4u32);
    // SAFETY: RegGetValueW takes a predefined key, two zero-terminated UTF-16 strings that outlive the call, the kind of value allowed (a DWORD), no kind
    // out, and a pointer to 4 bytes with their size, which `data` and `size` are; it only reads.
    let err = unsafe { RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), RRF_RT_REG_DWORD, null_mut(), (&raw mut data).cast(), &mut size) };
    err == 0 && data == 0
}

/// The colour Windows is set to as its accent: the `AccentColor` of the current user's DWM key (read only), else the colour of the title bars that Windows says.
pub fn system_accent() -> Option<Rgb> {
    let (key, name) = (wide("Software\\Microsoft\\Windows\\DWM"), wide("AccentColor"));
    let (mut data, mut size) = (0u32, 4u32);
    // SAFETY: as in `system_uses_dark`: a read of one DWORD of the registry into 4 bytes that live through the call.
    let err = unsafe { RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), RRF_RT_REG_DWORD, null_mut(), (&raw mut data).cast(), &mut size) };
    if err == 0 {
        return Some(crate::accent::rgb_of_abgr(data));
    }
    let (mut argb, mut opaque) = (0u32, 0i32);
    // SAFETY: DwmGetColorizationColor writes a colour and a flag to the two live values and keeps nothing.
    (unsafe { DwmGetColorizationColor(&mut argb, &mut opaque) } == 0).then(|| crate::accent::rgb_of_argb(argb))
}

/// The colours of the high-contrast theme Windows is set to; `None` when it is set to none.
pub fn high_contrast() -> Option<Contrast> {
    let mut scheme = [0u16; 256];
    let mut hc = HighContrastW { size: std::mem::size_of::<HighContrastW>() as u32, flags: 0, scheme: scheme.as_mut_ptr() };
    // SAFETY: SPI_GETHIGHCONTRAST fills a HIGHCONTRASTW of the size it says in `size` and writes the name of the scheme into the buffer `scheme` points to
    // (256 units, which is more than Windows writes); both are live through the call.
    let ok = unsafe { SystemParametersInfoW(SPI_GETHIGHCONTRAST, hc.size, (&raw mut hc).cast(), 0) } != 0;
    if !ok || hc.flags & HCF_HIGHCONTRASTON == 0 {
        return None;
    }
    let color = |index: i32| {
        // SAFETY: reads one colour of the system's; takes and gives plain numbers.
        let v = unsafe { GetSysColor(index) };
        ((v & 0xFF) << 16) | (v & 0xFF00) | ((v >> 16) & 0xFF)
    };
    Some(Contrast { window: color(COLOR_WINDOW), text: color(COLOR_WINDOWTEXT), gray: color(COLOR_GRAYTEXT), highlight: color(COLOR_HIGHLIGHT), highlight_text: color(COLOR_HIGHLIGHTTEXT), face: color(COLOR_BTNFACE) })
}

/// WM_NCCALCSIZE with the reader's own title strip: the client area is the whole window, except that a maximized window is taken in by the frame it
/// overhangs the screen by (so that nothing is drawn off the screen). Returns the answer for the message.
fn nc_calc_size(hwnd: isize, wparam: usize, lparam: isize) -> isize {
    // SAFETY: for WM_NCCALCSIZE with a true `wparam`, lparam points to an NCCALCSIZE_PARAMS that is valid, and ours to change, for the duration of this message
    // (with a false one it points to a RECT, and is not used); it is read only as far as its first rectangle.
    let params = if wparam != 0 { unsafe { (lparam as *mut NcCalcSize).as_mut() } } else { None };
    if let Some(r) = params.and_then(|p| p.rects.first_mut()) {
        // SAFETY: plain handle of this window; reads its state.
        let zoomed = unsafe { IsZoomed(hwnd) } != 0;
        if zoomed {
            // SAFETY: plain handle; reads the screen's dots per inch for the window.
            let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
            let pad = metric(SM_CXPADDEDBORDER, dpi);
            let (fx, fy) = (metric(SM_CXSIZEFRAME, dpi) + pad, metric(SM_CYSIZEFRAME, dpi) + pad);
            (r.left, r.right, r.top, r.bottom) = (r.left + fx, r.right - fx, r.top + fy, r.bottom - fy);
        }
    }
    0
}

/// WM_NCHITTEST with the reader's own title strip: the system's buttons first (it knows where it drew them, and Windows 11 shows its layouts for snapping a
/// window by the maximize button), then the edges where the window is sized, then what the reader says (the empty part of the strip is the title bar).
fn nc_hit_test(state: &WindowState, hwnd: isize, wparam: usize, lparam: isize) -> isize {
    let mut result = 0isize;
    // SAFETY: DwmDefWindowProc takes the message as the window procedure got it and writes its answer to `result`.
    if unsafe { DwmDefWindowProc(hwnd, WM_NCHITTEST, wparam, lparam, &mut result) } != 0 {
        return result;
    }
    let screen = Point { x: i32::from(loword(lparam as usize) as u16 as i16), y: i32::from(hiword(lparam as usize) as u16 as i16) };
    let mut window = WinRect::default();
    // SAFETY: `window` is a live RECT that Windows fills in and does not keep.
    unsafe { GetWindowRect(hwnd, &mut window) };
    // SAFETY: plain handle; reads whether the window is maximized.
    if unsafe { IsZoomed(hwnd) } == 0 {
        let b = scale(state.dpi.get(), 5);
        let (x, y) = (screen.x - window.left, screen.y - window.top);
        let (left, right, top, bottom) = (x < b, x >= window.right - window.left - b, y < b, y >= window.bottom - window.top - b);
        let code = match (top, bottom, left, right) {
            (true, _, true, _) => HTTOPLEFT,
            (true, _, _, true) => HTTOPRIGHT,
            (_, true, true, _) => HTBOTTOMLEFT,
            (_, true, _, true) => HTBOTTOMRIGHT,
            (true, ..) => HTTOP,
            (_, true, ..) => HTBOTTOM,
            (_, _, true, _) => HTLEFT,
            (_, _, _, true) => HTRIGHT,
            _ => 0,
        };
        if code != 0 {
            return code;
        }
    }
    let mut client = screen;
    // SAFETY: `client` is a live POINT that Windows rewrites in place.
    unsafe { ScreenToClient(hwnd, &mut client) };
    let caption = state.handler.try_borrow_mut().ok().and_then(|mut guard| guard.as_mut().map(|h| h.nc_hit(client.x, client.y)));
    if caption == Some(NcHit::Caption) { HTCAPTION } else { HTCLIENT as isize }
}

/// A popup menu of the window's menus: each of them is an item that opens its own. 0 if Windows would not make it.
fn build_top_menu(menus: &[Menu]) -> isize {
    // SAFETY: creates an empty popup menu; the handle is checked.
    let root = unsafe { CreatePopupMenu() };
    if root == 0 {
        return 0;
    }
    for menu in menus {
        let popup = build_popup(&menu.entries);
        if popup == 0 {
            continue;
        }
        let w = wide(&menu.title);
        // SAFETY: `popup` becomes a submenu of `root` (and is destroyed with it); `w` outlives the call.
        unsafe { AppendMenuW(root, MF_POPUP, popup as usize, w.as_ptr()) };
    }
    root
}

/// Show the menus as a pop-up menu with its top right corner at `at` (a place of the client area); the command of the item chosen. With `keyboard` its first
/// item is chosen to begin with (a key press is posted to the menu's loop), as a menu opened by Alt is.
fn show_menu(state: &WindowState, hwnd: isize, at: (i32, i32), keyboard: bool) -> Option<u32> {
    let root = build_top_menu(&state.menus.borrow());
    if root == 0 {
        return None;
    }
    let mut p = Point { x: at.0, y: at.1 };
    // SAFETY: `p` is a live POINT turned into screen coordinates; the menu belongs to this window and is destroyed after use; TPM_RETURNCMD answers with the
    // item's command (0 if the menu was dismissed) instead of posting a message; the key press is posted with no pointers in it.
    let chosen = unsafe {
        ClientToScreen(hwnd, &mut p);
        if keyboard {
            PostMessageW(hwnd, WM_KEYDOWN, VK_DOWN, 0);
        }
        let chosen = TrackPopupMenu(root, TPM_RETURNCMD_NONOTIFY | TPM_RIGHTBUTTON | TPM_RIGHTALIGN, p.x, p.y, 0, hwnd, null());
        DestroyMenu(root);
        chosen
    };
    u32::try_from(chosen).ok().filter(|&c| c > 0)
}

/// Put these menus by (for the pop-up).
fn set_menus(state: &WindowState, menus: Vec<Menu>) {
    *state.menus.borrow_mut() = menus;
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

/// The box the words to find are typed in, over the search pill of the command bar (or hide it).
fn set_search_box(state: &WindowState, hwnd: isize, b: Option<SearchBox>) {
    let edit = state.tb_search.get();
    if edit == 0 {
        return;
    }
    state.search_on.set(b.is_some());
    match b {
        Some(b) => {
            let w = wide(&b.hint);
            // SAFETY: EM_SETCUEBANNER takes a flag (0: the words go once the box has the keyboard) and a zero-terminated UTF-16 string that outlives the call (the
            // box copies it); EnableWindow takes a plain handle and a flag.
            unsafe {
                SendMessageW(edit, EM_SETCUEBANNER, 0, w.as_ptr() as isize);
                EnableWindow(edit, i32::from(b.enabled));
            }
            place_child(edit, b.rect);
        }
        None => {
            // SAFETY: reads which window has the keyboard, and gives it to the main window if it was the box.
            unsafe {
                if GetFocus() == edit {
                    SetFocus(hwnd);
                }
            }
            place_child(edit, Rect { x: 0, y: 0, w: 0, h: 0 });
        }
    }
}

/// The box over the page number or the zoom of the page pill, with the text selected and the keyboard (or put it away).
fn set_pill_edit(state: &WindowState, hwnd: isize, b: Option<(Rect, String)>) {
    let edit = state.tb_page.get();
    if edit == 0 {
        return;
    }
    match b {
        Some((r, text)) => {
            state.pill_open.set(true);
            set_text(edit, &text);
            place_child(edit, r);
            // SAFETY: plain handle of a box of ours; the whole text is selected (a start of 0 and an end of -1) once the click that gave it the keyboard is done.
            unsafe {
                SetFocus(edit);
                PostMessageW(hwnd, WM_SELECT_BOX, edit as usize, 0);
            }
        }
        None => {
            if state.pill_open.replace(false) {
                // SAFETY: reads which window has the keyboard, and gives it to the main window if it was the box.
                unsafe {
                    if GetFocus() == edit {
                        SetFocus(hwnd);
                    }
                }
                place_child(edit, Rect { x: 0, y: 0, w: 0, h: 0 });
            }
        }
    }
}

/// The work area (the monitor less the taskbar) of the monitor the window is on (nearest it), if Windows says.
fn monitor_work(hwnd: isize) -> Option<WinRect> {
    let mut info = MonitorInfo { size: std::mem::size_of::<MonitorInfo>() as u32, monitor: WinRect::default(), work: WinRect::default(), flags: 0 };
    // SAFETY: `info` is a live struct of the size it declares, which Windows fills in; the handles are this window's and the monitor nearest it.
    let ok = unsafe { GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut info) } != 0;
    ok.then_some(info.work)
}

/// The rectangle of the monitor the window is on (nearest it), if Windows says.
fn monitor_rect(hwnd: isize) -> Option<WinRect> {
    let mut info = MonitorInfo { size: std::mem::size_of::<MonitorInfo>() as u32, monitor: WinRect::default(), work: WinRect::default(), flags: 0 };
    // SAFETY: `info` is a live struct of the size it declares, which Windows fills in; the handles are this window's and the monitor nearest it.
    let ok = unsafe { GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut info) } != 0;
    ok.then_some(info.monitor)
}

/// The full-screen window is as big as its monitor (it was made so, and again when the monitor's dots per inch changed).
fn fit_monitor(hwnd: isize) {
    if let Some(m) = monitor_rect(hwnd) {
        // SAFETY: plain numbers and the handle of this window; no restack, no focus.
        unsafe { SetWindowPos(hwnd, 0, m.left, m.top, m.right - m.left, m.bottom - m.top, SWP_NOZOMORDER | SWP_NOOWNERZORDER | SWP_NOACTIVATE) };
    }
}

/// The window goes full screen on the monitor it is on, or comes back as it was. Full screen: no frame and no title strip (the frame is not extended
/// over the window); the client area is the monitor. Coming back: the style and the place (also maximized or not) are put back.
fn set_full_screen(state: &WindowState, on: bool) {
    let hwnd = state.hwnd.get();
    if on && !state.fullscreen.get() {
        let mut place = WindowPlacement { length: std::mem::size_of::<WindowPlacement>() as u32, ..WindowPlacement::default() };
        // SAFETY: `place` is a live struct of the size it declares, which Windows fills in; the handle is this window's. The style is read to be put back.
        let style = unsafe {
            GetWindowPlacement(hwnd, &mut place);
            GetWindowLongPtrW(hwnd, GWL_STYLE) as u32
        };
        let Some(m) = monitor_rect(hwnd) else { return };
        *state.full_saved.borrow_mut() = Some(FullSaved { place, style, dpi: state.dpi.get() });
        state.fullscreen.set(true);
        apply_frame(state);
        // SAFETY: plain numbers and the handle of this window: the frame is taken off, and the window is made as big as the monitor. The keyboard goes to the
        // pages: the box or the tree that had it is hidden now, and keys sent to a hidden window go nowhere.
        unsafe {
            SetWindowLongPtrW(hwnd, GWL_STYLE, (style & !WS_OVERLAPPEDWINDOW) as isize);
            SetWindowPos(hwnd, 0, m.left, m.top, m.right - m.left, m.bottom - m.top, SWP_NOOWNERZORDER | SWP_FRAMECHANGED);
            SetFocus(hwnd);
        }
        apply_layout(state);
    } else if !on && let Some(saved) = state.full_saved.borrow_mut().take() {
        state.fullscreen.set(false);
        apply_frame(state);
        // The place was kept in the pixels of the screen as it was: with another number of dots per inch now (it changed while the window was full screen)
        // the window is as many pixels bigger or smaller as the dots are more or fewer, in the same place; a maximized one is maximized again as it was.
        let (now, then) = (i64::from(state.dpi.get()), i64::from(saved.dpi.max(1)));
        let n = saved.place.normal;
        let scaled = |v: i32| (i64::from(v) * now / then) as i32;
        // SAFETY: plain numbers and the handle of this window: the style comes back, the place is put as it was (and, when the dots per inch changed, the size is
        // put right), and the frame is drawn again.
        unsafe {
            SetWindowLongPtrW(hwnd, GWL_STYLE, saved.style as isize);
            SetWindowPlacement(hwnd, &saved.place);
            if now != then && IsZoomed(hwnd) == 0 {
                SetWindowPos(hwnd, 0, n.left, n.top, scaled(n.right - n.left), scaled(n.bottom - n.top), SWP_NOZOMORDER | SWP_NOOWNERZORDER | SWP_FRAMECHANGED);
            } else {
                SetWindowPos(hwnd, 0, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZOMORDER | SWP_NOOWNERZORDER | SWP_FRAMECHANGED);
            }
        }
        apply_layout(state);
    }
}

/// Remember where the window is while it is neither maximized nor minimized (that is the place that is kept for the next run).
fn remember_normal(state: &WindowState) {
    let hwnd = state.hwnd.get();
    // (Full screen is no place to come back to: the one from before it is in the saved state.)
    if state.fullscreen.get() {
        return;
    }
    // SAFETY: plain handle of this window; reads its state.
    if unsafe { IsZoomed(hwnd) != 0 || IsIconic(hwnd) != 0 } {
        return;
    }
    let mut r = WinRect::default();
    // SAFETY: `r` is a live RECT that Windows fills in and does not keep.
    if unsafe { GetWindowRect(hwnd, &mut r) } != 0 {
        state.normal.set(Some(r));
    }
}

/// Keep the window's place for the next run (not for the speed test or the other hidden tests).
fn save_placement(state: &WindowState) {
    let Some(r) = state.normal.get().filter(|_| state.remember_window) else { return };
    // SAFETY: plain handle of this window; reads its state.
    let maximized = unsafe { IsZoomed(state.hwnd.get()) } != 0;
    let place = Placement { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top, maximized };
    crate::settings::update(|s| s.window = Some(place));
}

unsafe extern "system" fn collect_monitor(monitor: isize, _dc: isize, _rect: *const WinRect, data: isize) -> i32 {
    // SAFETY: `data` is the address of the Vec that `work_areas` hands to EnumDisplayMonitors, which calls this only during that call and
    // on the same thread; nothing else uses the Vec meanwhile.
    let Some(list) = (unsafe { (data as *mut Vec<(bool, Rect)>).as_mut() }) else { return 0 };
    let mut info = MonitorInfo { size: std::mem::size_of::<MonitorInfo>() as u32, monitor: WinRect::default(), work: WinRect::default(), flags: 0 };
    // SAFETY: `info` is a live MONITORINFO of the size it declares; Windows fills it in and does not keep it.
    if unsafe { GetMonitorInfoW(monitor, &mut info) } != 0 {
        let w = info.work;
        list.push((info.flags & 1 != 0, Rect { x: w.left, y: w.top, w: w.right - w.left, h: w.bottom - w.top }));
    }
    1
}

/// The work areas of the screens (each less its task bar), the main screen first.
fn work_areas() -> Vec<Rect> {
    let mut list: Vec<(bool, Rect)> = Vec::new();
    // SAFETY: a null device context and clip rectangle ask for every monitor; the callback is `collect_monitor`, given the address of
    // `list`, which lives through the call.
    unsafe { EnumDisplayMonitors(0, null(), collect_monitor, (&raw mut list) as isize) };
    list.iter().filter(|(main, _)| *main).chain(list.iter().filter(|(main, _)| !*main)).map(|&(_, r)| r).collect()
}

/// The program's icon in the size the screen asks for (0 when the program has none: it is a resource that `build.rs` of the program writes).
fn load_icon(instance: isize, dpi: u32, small: bool) -> isize {
    let (cx, cy) = if small { (metric(SM_CXSMICON, dpi), metric(SM_CYSMICON, dpi)) } else { (metric(SM_CXICON, dpi), metric(SM_CYICON, dpi)) };
    // SAFETY: a small integer resource id in the pointer slot is how LoadImageW names a resource of the program; LR_SHARED gives an icon
    // that the system keeps and that is never destroyed by the caller, of the size asked or the nearest the file has.
    unsafe { LoadImageW(instance, ICON_ID as *const u16, IMAGE_ICON, cx, cy, LR_SHARED) }
}

/// Give the window the icon (big for the task switcher, small for the title bar and task bar) at the size of its screen.
fn set_icons(hwnd: isize, dpi: u32) {
    // SAFETY: a null module name means the module of this program.
    let instance = unsafe { GetModuleHandleW(null()) };
    for (which, small) in [(1usize, false), (0, true)] {
        let icon = load_icon(instance, dpi, small);
        if icon != 0 {
            // SAFETY: WM_SETICON takes the kind (big or small) and an icon handle, which is a shared icon of the system's.
            unsafe { SendMessageW(hwnd, WM_SETICON, which, icon) };
        }
    }
}

/// Which click in a row (1, 2, 3) the button that has just gone down is, by the system's own measure of a double click.
fn click_count(state: &WindowState, x: i32, y: i32) -> u8 {
    // SAFETY: these read system settings and the time of the message being handled; they take and give plain numbers.
    let (ms, w, h, time) = unsafe { (GetDoubleClickTime(), GetSystemMetrics(SM_CXDOUBLECLK), GetSystemMetrics(SM_CYDOUBLECLK), GetMessageTime()) };
    state.clicks.borrow_mut().click(time as u32, x, y, ms, w / 2, h / 2)
}

/// The edit box that is the find box: the command bar's when it is shown, else the one over the pages.
fn find_edit(state: &WindowState) -> isize {
    if state.search_on.get() && state.tb_search.get() != 0 { state.tb_search.get() } else { state.find.get() }
}

// --- starting up -------------------------------------------------------------------------------------------------

/// Make the window and run it until it is closed. `make` is given the means to wake the window from another thread
/// and builds the reader. `client`: a size for the window's client area, in pixels (the speed test uses it; it may be bigger
/// than the screen); otherwise the window is most of the work area, or where it was (`saved`, put back where it can be seen). With
/// `remember` the window's place is kept for the next run when it is closed.
pub fn run(title: &str, client: Option<(i32, i32)>, saved: Option<Placement>, remember: bool, make: impl FnOnce(Waker) -> Box<dyn Handler>) -> i32 {
    let icc = InitCommonControlsEx { size: std::mem::size_of::<InitCommonControlsEx>() as u32, icc: ICC_STANDARD_CLASSES | ICC_TREEVIEW_CLASSES };
    // SAFETY: `icc` is a live struct of the size it declares; this makes the buttons and boxes use the new look.
    unsafe { InitCommonControlsEx(&icc) };
    // SAFETY: the print box needs COM on its thread (a single-threaded apartment); a failure only means the box may not open.
    unsafe { CoInitializeEx(null(), COINIT_APARTMENTTHREADED) };
    let class = wide(WINDOW_CLASS);
    // SAFETY: a null module name means the module of this program.
    let instance = unsafe { GetModuleHandleW(null()) };
    // SAFETY: no arguments; reads a system setting.
    let system_dpi = unsafe { GetDpiForSystem() }.max(96);
    let wc = WndClassExW {
        size: std::mem::size_of::<WndClassExW>() as u32,
        style: 0,
        wnd_proc: Some(wnd_proc),
        cls_extra: 0,
        wnd_extra: 0,
        instance,
        icon: load_icon(instance, system_dpi, false),
        // SAFETY: a small integer resource id in the pointer slot is how LoadCursorW names a stock cursor.
        cursor: unsafe { LoadCursorW(0, IDC_ARROW as *const u16) },
        background: 0,
        menu_name: null(),
        class_name: class.as_ptr(),
        icon_small: load_icon(instance, system_dpi, true),
    };
    // SAFETY: `wc` is a live WNDCLASSEXW whose strings outlive the call (Windows copies what it needs).
    if unsafe { RegisterClassExW(&wc) } == 0 {
        return 1;
    }
    // With the reader's own title strip the window is its client area; with the system's own frame (a high-contrast theme) the frame is added to the size.
    let plain_frame = high_contrast().is_some();
    let style = WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN;
    let mut frame = WinRect { left: 0, top: 0, right: DEFAULT_WIDTH * system_dpi as i32 / 96, bottom: DEFAULT_HEIGHT * system_dpi as i32 / 96 };
    if plain_frame {
        // SAFETY: `frame` is a live RECT that is rewritten in place.
        unsafe { AdjustWindowRectExForDpi(&mut frame, style, 0, WS_EX_ACCEPTFILES, system_dpi) };
    }
    // The window goes in the middle of the screen's work area (not under the task bar) and is no bigger than most of it.
    let mut work = WinRect::default();
    // SAFETY: SPI_GETWORKAREA fills in the RECT that the pointer points to; `work` is a live RECT.
    let have_work = unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, (&raw mut work).cast(), 0) } != 0;
    let (mut x, mut y) = (CW_USEDEFAULT, CW_USEDEFAULT);
    let (mut w, mut h) = (frame.right - frame.left, frame.bottom - frame.top);
    if let Some((cw, ch)) = client {
        ALLOW_BIG_WINDOW.store(true, Ordering::Relaxed);
        frame = WinRect { left: 0, top: 0, right: cw.clamp(100, 16000), bottom: ch.clamp(100, 16000) };
        if plain_frame {
            // SAFETY: `frame` is a live RECT that is rewritten in place.
            unsafe { AdjustWindowRectExForDpi(&mut frame, style, 0, WS_EX_ACCEPTFILES, system_dpi) };
        }
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
    // Where the window was when it was last closed, if that can still be seen.
    let mut maximized = false;
    if client.is_none()
        && let Some(at) = saved.and_then(|p| placement::restore(p, &work_areas()))
    {
        (x, y, w, h, maximized) = (at.x, at.y, at.w, at.h, at.maximized);
    }
    let name = wide(title);
    // SAFETY: the strings are zero-terminated UTF-16 that outlive the call; no parent, menu or creation parameter.
    let hwnd = unsafe { CreateWindowExW(WS_EX_ACCEPTFILES, class.as_ptr(), name.as_ptr(), style, x, y, w, h, 0, 0, instance, null_mut()) };
    if hwnd == 0 {
        return 1;
    }
    let state = Box::new(WindowState {
        hwnd: Cell::new(hwnd),
        closing: Cell::new(false),
        handler: RefCell::new(None),
        back: RefCell::new(Back::default()),
        font: Cell::new(0),
        fonts: [Cell::new(0), Cell::new(0), Cell::new(0)],
        zh: Cell::new(false),
        dpi: Cell::new(96),
        tree: Cell::new(0),
        find: Cell::new(0),
        tree_items: RefCell::new(Vec::new()),
        tree_filling: Cell::new(false),
        tree_rename: Cell::new(false),
        renamed: RefCell::new(None),
        tree_drag: Cell::new(None),
        field_single: Cell::new(0),
        field_multi: Cell::new(0),
        field_active: Cell::new(0),
        printer: RefCell::new(None),
        menus: RefCell::new(Vec::new()),
        tb_search: Cell::new(0),
        tb_page: Cell::new(0),
        search_on: Cell::new(false),
        pill_open: Cell::new(false),
        custom: Cell::new(!plain_frame),
        mica: Cell::new(false),
        key: Cell::new(0),
        size: Cell::new((-1, -1)),
        frame_told: Cell::new(None),
        normal: Cell::new(None),
        remember_window: remember && client.is_none(),
        clicks: RefCell::new(ClickCounter::default()),
        depth: Cell::new(0),
        deferred: RefCell::new(Vec::new()),
        tracking: Cell::new(false),
        theme: Cell::new(None),
        face_brush: Cell::new(0),
        field_brush: Cell::new(0),
        fullscreen: Cell::new(false),
        full_saved: RefCell::new(None),
    });
    let raw = Box::into_raw(state);
    // SAFETY: stores the pointer from `Box::into_raw`; it is freed once, by `free_state` below, after the message loop.
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, raw as isize) };
    let Some(state) = state_of(hwnd) else { return 1 };
    // The bookmarks tree and the find box: children of the window, hidden until the reader says where to put them.
    // The tree of bookmarks has the look of the file windows (chevrons, no lines, no sunken edge).
    state.tree.set(make_child(hwnd, instance, "SysTreeView32", 0, WS_TABSTOP | TVS_HASBUTTONS | TVS_SHOWSELALWAYS | TVS_EDITLABELS, ID_TREE));
    set_tree_theme(state.tree.get(), false);
    state.find.set(make_child(hwnd, instance, "EDIT", WS_EX_CLIENTEDGE, WS_TABSTOP | ES_AUTOHSCROLL, ID_FIND));
    state.field_single.set(make_child(hwnd, instance, "EDIT", WS_EX_CLIENTEDGE, WS_TABSTOP | ES_AUTOHSCROLL, ID_FIELD1));
    state.field_multi.set(make_child(hwnd, instance, "EDIT", WS_EX_CLIENTEDGE, WS_TABSTOP | ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN | WS_VSCROLL, ID_FIELD2));
    // The box the words to find are typed in (over the search pill of the command bar) and the box over the page number or the zoom (over the page pill):
    // no border, the colour of the pill.
    state.tb_page.set(make_child(hwnd, instance, "EDIT", 0, WS_TABSTOP | ES_AUTOHSCROLL | ES_CENTER, ID_PAGE_BOX));
    state.tb_search.set(make_child(hwnd, instance, "EDIT", 0, WS_TABSTOP | ES_AUTOHSCROLL, ID_SEARCH_BOX));
    // Windows 11 can show Mica behind the title strip; where it cannot, asking is an error, and the strip is its own colour.
    let backdrop = DWMSBT_MAINWINDOW;
    // SAFETY: DwmSetWindowAttribute takes this window, the number of an attribute and a pointer to a value of the size told (a 4-byte integer that lives through
    // the call); a system that does not know the attribute answers with an error, which is the answer wanted here.
    let mica = unsafe { DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, (&raw const backdrop).cast(), 4) } == 0;
    state.mica.set(mica && !plain_frame);
    // The frame is calculated again now that the window has its state (what `WM_NCCALCSIZE` answers depends on it).
    // SAFETY: plain numbers and the handle of the window just made.
    unsafe { SetWindowPos(hwnd, 0, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZOMORDER | SWP_NOOWNERZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED) };

    if let Some((cw, ch)) = client {
        // The page area is to be as wide as asked and as tall as asked less what the status bar used to be (26 pixels at 96 dpi), which is what the speed test has
        // always measured; it has no title strip, command bar or scroll bars. The window makes room for the rest.
        let (cw, ch) = (cw, ch - scale(system_dpi, 26));
        let (have_w, have_h) = client_size(hwnd);
        if (have_w, have_h) != (cw, ch) {
            // SAFETY: plain numbers and the handle of the window just made; no move, no restack.
            unsafe { SetWindowPos(hwnd, 0, 0, 0, w + (cw - have_w), h + (ch - have_h), SWP_NOZOMORDER | SWP_NOACTIVATE | SWP_NOMOVE) };
        }
    }

    // SAFETY: plain handle; 1 means "accept dropped files".
    unsafe { DragAcceptFiles(hwnd, 1) };
    *state.handler.borrow_mut() = Some(make(Waker(hwnd)));

    // SAFETY: plain handle of the window just made.
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    set_icons(hwnd, dpi);
    new_dpi(state, dpi);
    dispatch(state, Event::Dpi(dpi));
    // The reader has not heard of the size yet (what was told before it was made is lost): it is told now.
    state.size.set((-1, -1));
    state.frame_told.set(None);
    apply_layout(state);
    // The place it has now is the one kept for when it is closed (it is not shown yet, so it is not maximized).
    remember_normal(state);
    // SAFETY: plain handle of the window just made.
    unsafe {
        ShowWindow(hwnd, if maximized { SW_SHOWMAXIMIZED } else { SW_SHOWNORMAL });
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
    // The box of a form field or text box: Esc leaves it (and throws what is typed away), Tab and Shift+Tab go to the next field and
    // the one before, Enter ends a line of a field of one line (Ctrl+Enter ends the several lines of a field or text box).
    let active = state.field_active.get();
    if msg.hwnd != 0 && msg.hwnd == active {
        let (ctrl, shift) = (modifier(0x11), modifier(0x10));
        let exit = match msg.wparam as u32 {
            0x1B => Some(FieldExit::Cancel),
            0x09 => Some(if shift { FieldExit::Prev } else { FieldExit::Next }),
            0x0D if ctrl || active == state.field_single.get() => Some(FieldExit::Commit),
            _ => None,
        };
        if let Some(exit) = exit {
            finish_field(state, exit);
            return true;
        }
        return false;
    }
    // The box over the page pill: Enter takes what was typed, Esc gives it up; either gives the keyboard to the pages.
    let page = state.tb_page.get();
    if msg.hwnd != 0 && msg.hwnd == page {
        let (taken, exit) = match msg.wparam as u32 {
            0x0D => (true, true),
            0x1B => (false, true),
            _ => (false, false),
        };
        if exit {
            let text = if taken { Some(window_text(page, 32)) } else { None };
            if state.pill_open.replace(false) {
                place_child(page, Rect { x: 0, y: 0, w: 0, h: 0 });
                // SAFETY: plain handle of the main window.
                unsafe { SetFocus(state.hwnd.get()) };
                dispatch(state, Event::PillEdit(text));
            }
        }
        return exit;
    }
    let search = state.tb_search.get();
    if msg.hwnd == 0 || (msg.hwnd != tree && msg.hwnd != find && msg.hwnd != search) {
        return false;
    }
    let in_find = msg.hwnd == find || msg.hwnd == search;
    let vk = msg.wparam as u32;
    let (ctrl, shift) = (modifier(0x11), modifier(0x10));
    // Esc while an item of the tree is dragged gives the drag up.
    if vk == 0x1B && state.tree_drag.get().is_some() {
        tree_drag_end(state, None);
        return true;
    }
    let wanted = match vk {
        0x0D => in_find,
        // In the tree F2, Delete and Tab are the bookmarks' (type over the title, take out, a level deeper).
        0x71 | 0x2E | 0x09 if msg.hwnd == tree && !ctrl => true,
        0x1B | 0x72..=0x74 | 0x7A => true,
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
    for font in [state.font.get(), state.fonts[0].get(), state.fonts[1].get(), state.fonts[2].get()].into_iter().filter(|&f| f != 0) {
        // SAFETY: a font made by `make_font` and the device context that had it selected is gone.
        unsafe { DeleteObject(font) };
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

// --- Windows text recognition (5c) ---------------------------------------------------------------------------------------
//
// `Windows.Media.Ocr` is a Windows Runtime class: objects are COM interface pointers (a vtable of function pointers behind the
// pointer), made by the factory of their class (`RoGetActivationFactory`), and strings are HSTRINGs. Everything here is declared by
// hand: the interfaces below are called by the slot of the method in the vtable (the three methods of IUnknown, three more of
// IInspectable, then the interface's own, in the order of the Windows SDK's metadata), and no object of ours is ever given to
// Windows. What the unsafe code relies on:
// - an `Obj` holds one reference to a live interface pointer (never null) and gives it back in `Drop`, so every interface is
//   released on every path, early returns and errors included;
// - a method is called only through a function pointer type that is its signature (the `get_*` helpers name the signature and the
//   caller names the slot and the interface it belongs to);
// - an out-parameter is a live local that Windows writes once, and what it hands back (an interface pointer or an HSTRING) is
//   owned by us from then on and released exactly once;
// - the thread that makes a `Recogniser` initialises the Windows Runtime for itself (`RoGuard`) and the recogniser is dropped
//   on that thread, so the objects never cross threads.

use std::time::Duration;

use crate::ocr::{GrayImage, OcrError, Recognizer};
use qingpdf_core::view::{MAX_WORDS_PER_PAGE, OcrLine, OcrWord};

#[repr(C)]
#[derive(Clone, Copy)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

/// `Windows.Media.Ocr.IOcrEngineStatics`
const IID_OCR_ENGINE_STATICS: Guid = Guid { data1: 0x5BFF_A85A, data2: 0x3384, data3: 0x3540, data4: [0x99, 0x40, 0x69, 0x91, 0x20, 0xD4, 0x28, 0xA8] };
/// `Windows.Foundation.IAsyncInfo`
const IID_ASYNC_INFO: Guid = Guid { data1: 0x0000_0036, data2: 0x0000, data3: 0x0000, data4: [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46] };
/// `Windows.Graphics.Imaging.ISoftwareBitmapStatics`
const IID_SOFTWARE_BITMAP_STATICS: Guid = Guid { data1: 0xDF03_85DB, data2: 0x672F, data3: 0x4A9D, data4: [0x80, 0x6E, 0xC2, 0x44, 0x2F, 0x34, 0x3E, 0x86] };
/// `Windows.Security.Cryptography.ICryptographicBufferStatics`
const IID_CRYPTO_BUFFER_STATICS: Guid = Guid { data1: 0x320B_7E22, data2: 0x3CB0, data3: 0x4CDF, data4: [0x86, 0x63, 0x1D, 0x28, 0x91, 0x00, 0x65, 0xEB] };

/// `BitmapPixelFormat::Gray8`
const PIXEL_FORMAT_GRAY8: i32 = 62;
/// `RO_INIT_MULTITHREADED`, and the answer when the thread is a single-threaded apartment already.
const RO_INIT_MULTITHREADED: i32 = 1;
const RPC_E_CHANGED_MODE: i32 = 0x8001_0106_u32 as i32;
const E_POINTER: i32 = 0x8000_4003_u32 as i32;
const E_FAIL: i32 = 0x8000_4005_u32 as i32;
/// `AsyncStatus`: Started, Completed, Canceled, Error.
const ASYNC_STARTED: u32 = 0;
const ASYNC_COMPLETED: u32 = 1;
const ASYNC_CANCELED: u32 = 2;
const ASYNC_ERROR: u32 = 3;
/// Most lines read off a page.
const MAX_OCR_LINES: u32 = 10_000;

#[link(name = "runtimeobject")]
unsafe extern "system" {
    fn RoInitialize(kind: i32) -> i32;
    fn RoUninitialize();
    fn RoGetActivationFactory(class_id: isize, iid: *const Guid, factory: *mut *mut c_void) -> i32;
    fn WindowsCreateString(source: *const u16, length: u32, string: *mut isize) -> i32;
    fn WindowsDeleteString(string: isize) -> i32;
    fn WindowsGetStringRawBuffer(string: isize, length: *mut u32) -> *const u16;
}

/// An HSTRING we made, deleted when dropped.
struct HString(isize);

impl HString {
    fn new(s: &str) -> Result<HString, i32> {
        let units: Vec<u16> = s.encode_utf16().collect();
        let mut h = 0isize;
        // SAFETY: `units` holds `length` UTF-16 units that live through the call (Windows copies them); `h` is a live isize that
        // receives the new string handle.
        let hr = unsafe { WindowsCreateString(units.as_ptr(), units.len() as u32, &mut h) };
        if hr < 0 { Err(hr) } else { Ok(HString(h)) }
    }
}

impl Drop for HString {
    fn drop(&mut self) {
        // SAFETY: the handle was made by WindowsCreateString and is deleted only here.
        unsafe { WindowsDeleteString(self.0) };
    }
}

/// Take the text of an HSTRING that Windows gave us, and delete it.
fn take_hstring(h: isize) -> String {
    let mut len = 0u32;
    // SAFETY: `h` is a handle Windows gave us (0 is allowed: it gives a null pointer and a length of 0); the buffer it returns is
    // valid until the handle is deleted, which is after the copy below; `len` is a live u32.
    let text = unsafe {
        let p = WindowsGetStringRawBuffer(h, &mut len);
        if p.is_null() { String::new() } else { String::from_utf16_lossy(std::slice::from_raw_parts(p, len as usize)) }
    };
    // SAFETY: `h` is ours (the out-parameter of a method that gives its caller the string) and is deleted once, here.
    unsafe { WindowsDeleteString(h) };
    text
}

/// The thread's Windows Runtime initialisation, undone when dropped (a thread that was a single-threaded apartment already is not
/// undone, it was not ours).
struct RoGuard(bool);

impl RoGuard {
    fn new() -> Result<RoGuard, i32> {
        // SAFETY: takes a plain number and changes only the apartment state of this thread.
        let hr = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
        if hr >= 0 {
            Ok(RoGuard(true))
        } else if hr == RPC_E_CHANGED_MODE {
            Ok(RoGuard(false))
        } else {
            Err(hr)
        }
    }
}

impl Drop for RoGuard {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful RoInitialize of this thread; every object of the thread is released before (the guard
            // is the last field of what holds them).
            unsafe { RoUninitialize() };
        }
    }
}

/// One reference to a Windows Runtime object (an interface pointer, never null).
struct Obj(*mut c_void);

type GetObj = unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32;
type GetU32 = unsafe extern "system" fn(*mut c_void, *mut u32) -> i32;
type GetI32 = unsafe extern "system" fn(*mut c_void, *mut i32) -> i32;
type GetHString = unsafe extern "system" fn(*mut c_void, *mut isize) -> i32;
type GetRect = unsafe extern "system" fn(*mut c_void, *mut [f32; 4]) -> i32;
type ObjWithObj = unsafe extern "system" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> i32;
type ObjAt = unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32;
type Call0 = unsafe extern "system" fn(*mut c_void) -> i32;
type Release = unsafe extern "system" fn(*mut c_void) -> u32;
type Query = unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32;
type CreateFromBytes = unsafe extern "system" fn(*mut c_void, u32, *const u8, *mut *mut c_void) -> i32;
type CreateBitmap = unsafe extern "system" fn(*mut c_void, *mut c_void, i32, i32, i32, *mut *mut c_void) -> i32;

fn check(hr: i32) -> Result<(), i32> {
    if hr < 0 { Err(hr) } else { Ok(()) }
}

impl Obj {
    /// Own the interface pointer `p`; an error when it is null.
    ///
    /// # Safety
    /// A non-null `p` must be a pointer to a live interface of a runtime object, of which one reference is given to the new `Obj`
    /// (it is released when the `Obj` is dropped).
    unsafe fn new(p: *mut c_void) -> Result<Obj, i32> {
        if p.is_null() { Err(E_POINTER) } else { Ok(Obj(p)) }
    }

    /// The function in slot `n` of the vtable, as the function pointer type `F`.
    ///
    /// # Safety
    /// The object must have a vtable with at least `n + 1` slots, and slot `n` must be a method of the signature `F` (a function
    /// pointer type).
    unsafe fn method<F: Copy>(&self, n: usize) -> F {
        debug_assert!(std::mem::size_of::<F>() == std::mem::size_of::<usize>());
        // SAFETY: an interface pointer points to a pointer to the vtable, an array of function addresses; the caller promises
        // that slot `n` is there and has the signature `F`, a function pointer, which is the size of an address.
        unsafe {
            let vtable = *(self.0 as *const *const usize);
            std::mem::transmute_copy::<usize, F>(&*vtable.add(n))
        }
    }

    // The methods below take a slot of the vtable (or are for one interface only), and a wrong one is not a mistake the compiler
    // can find: every one of them is `unsafe`, and its caller says why the slot is right for the object.

    /// `HRESULT Method([out] Interface** value)` in slot `n`.
    ///
    /// # Safety
    /// Slot `n` of the object must be a getter of an interface pointer, with that signature.
    unsafe fn get_obj(&self, n: usize) -> Result<Obj, i32> {
        let mut out = null_mut();
        // SAFETY: the caller promises that slot `n` is a getter of an interface pointer; `out` is a live pointer that is written
        // once.
        let hr = unsafe { self.method::<GetObj>(n)(self.0, &mut out) };
        check(hr)?;
        // SAFETY: the call succeeded, so `out` is a new reference to an interface (or null, which `new` refuses).
        unsafe { Obj::new(out) }
    }

    /// `HRESULT Method([out] UINT32* value)` in slot `n`.
    ///
    /// # Safety
    /// Slot `n` of the object must be a getter of a 32-bit number, with that signature.
    unsafe fn get_u32(&self, n: usize) -> Result<u32, i32> {
        let mut out = 0u32;
        // SAFETY: the caller promises that slot `n` is a getter of a 32-bit number; `out` is a live u32.
        check(unsafe { self.method::<GetU32>(n)(self.0, &mut out) })?;
        Ok(out)
    }

    /// `HRESULT Method([out] HSTRING* value)` in slot `n`.
    ///
    /// # Safety
    /// Slot `n` of the object must be a getter of an HSTRING, with that signature.
    unsafe fn get_string(&self, n: usize) -> Result<String, i32> {
        let mut out = 0isize;
        // SAFETY: the caller promises that slot `n` is a getter of an HSTRING; `out` is a live handle that is written once, and it
        // is ours when the call succeeds.
        check(unsafe { self.method::<GetHString>(n)(self.0, &mut out) })?;
        Ok(take_hstring(out))
    }

    /// `HRESULT BoundingRect([out] Rect* value)` in slot `n` (four floats: x, y, width, height).
    ///
    /// # Safety
    /// Slot `n` of the object must be the getter of a `Windows.Foundation.Rect`.
    unsafe fn get_rect(&self, n: usize) -> Result<[f32; 4], i32> {
        let mut out = [0f32; 4];
        // SAFETY: the caller promises that slot `n` is the getter of a `Windows.Foundation.Rect`, four 32-bit floats; `out` is a
        // live array of four.
        check(unsafe { self.method::<GetRect>(n)(self.0, &mut out) })?;
        Ok(out)
    }

    /// `HRESULT Method([in] Interface* argument, [out] Interface** value)` in slot `n`.
    ///
    /// # Safety
    /// Slot `n` of the object must take one interface pointer (of the kind `argument` is) and give one.
    unsafe fn call_with(&self, n: usize, argument: &Obj) -> Result<Obj, i32> {
        let mut out = null_mut();
        // SAFETY: the caller promises that slot `n` takes one interface pointer and gives one; `argument` is live for the call;
        // `out` is written once.
        check(unsafe { self.method::<ObjWithObj>(n)(self.0, argument.0, &mut out) })?;
        // SAFETY: the call succeeded, so `out` is a new reference to an interface (or null, which `new` refuses).
        unsafe { Obj::new(out) }
    }

    /// `HRESULT GetAt([in] UINT32 index, [out] T* value)` of an `IVectorView` of runtime objects (slot 6).
    ///
    /// # Safety
    /// The object must be an `IVectorView` of runtime objects.
    unsafe fn at(&self, index: u32) -> Result<Obj, i32> {
        let mut out = null_mut();
        // SAFETY: the caller promises that this is an IVectorView, whose slot 6 is GetAt(index, out); `out` is written once.
        check(unsafe { self.method::<ObjAt>(6)(self.0, index, &mut out) })?;
        // SAFETY: the call succeeded, so `out` is a new reference to an interface (or null, which `new` refuses).
        unsafe { Obj::new(out) }
    }

    /// `HRESULT Method()` in slot `n` (Cancel and Close of IAsyncInfo).
    ///
    /// # Safety
    /// Slot `n` of the object must be a method with no argument besides the object.
    unsafe fn call0(&self, n: usize) -> i32 {
        // SAFETY: the caller promises that slot `n` takes no argument besides the object.
        unsafe { self.method::<Call0>(n)(self.0) }
    }

    /// QueryInterface for another interface of the object.
    fn query(&self, iid: &Guid) -> Result<Obj, i32> {
        let mut out = null_mut();
        // SAFETY: slot 0 of every interface is QueryInterface(iid, out); `iid` is a live GUID; `out` is written once.
        check(unsafe { self.method::<Query>(0)(self.0, iid, &mut out) })?;
        // SAFETY: the call succeeded, so `out` is a new reference to an interface (or null, which `new` refuses).
        unsafe { Obj::new(out) }
    }
}

/// An operation of the recogniser (`IAsyncInfo`) that is being waited for. However it is left, it is cancelled if it may still be
/// running, and closed (an operation may be closed only when it is not running).
struct Pending(Obj);

impl Pending {
    /// The state of the operation (`ASYNC_*`).
    fn status(&self) -> Result<u32, i32> {
        // SAFETY: slot 7 of IAsyncInfo is get_Status(out AsyncStatus), a 32-bit number.
        unsafe { self.0.get_u32(7) }
    }

    /// The error code of a failed operation (IAsyncInfo, slot 8).
    fn error_code(&self) -> i32 {
        let mut hr = 0i32;
        // SAFETY: slot 8 of IAsyncInfo is get_ErrorCode(out HRESULT); `hr` is a live i32.
        unsafe { self.0.method::<GetI32>(8)(self.0.0, &mut hr) };
        hr
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        // A state that is not known counts as running.
        if !matches!(self.status(), Ok(ASYNC_COMPLETED | ASYNC_CANCELED | ASYNC_ERROR)) {
            // SAFETY: slot 9 of IAsyncInfo is Cancel().
            unsafe { self.0.call0(9) };
            for _ in 0..40 {
                match self.status() {
                    Ok(ASYNC_STARTED) => std::thread::sleep(Duration::from_millis(5)),
                    _ => break,
                }
            }
        }
        // SAFETY: slot 10 of IAsyncInfo is Close().
        unsafe { self.0.call0(10) };
    }
}

impl Drop for Obj {
    fn drop(&mut self) {
        // SAFETY: slot 2 of every interface is Release, and this reference is given back once.
        unsafe { self.method::<Release>(2)(self.0) };
    }
}

/// The factory of a runtime class, as the interface `iid` (its statics).
fn factory(class: &str, iid: &Guid) -> Result<Obj, i32> {
    let name = HString::new(class)?;
    let mut out = null_mut();
    // SAFETY: `name` is a live HSTRING; `iid` is a live GUID; `out` is written once and is ours when the call succeeds.
    check(unsafe { RoGetActivationFactory(name.0, iid, &mut out) })?;
    // SAFETY: the call succeeded, so `out` is a new reference to the factory (or null, which `new` refuses).
    unsafe { Obj::new(out) }
}

/// The text recogniser of Windows, made on the thread that uses it.
struct Recogniser {
    engine: Obj,
    bitmaps: Obj,
    buffers: Obj,
    language: String,
    max_dimension: u32,
    // (Dropped last: after every object of the thread is released.)
    _runtime: RoGuard,
}

fn failed(what: &str, hr: i32) -> OcrError {
    OcrError::Failed(format!("{what} (error 0x{:08X})", hr as u32))
}

/// Make the recogniser: for Chinese when `chinese` (and say so if Windows has none), else for the languages the person uses.
pub fn open_recognizer(chinese: bool) -> Result<Box<dyn Recognizer>, OcrError> {
    let runtime = RoGuard::new().map_err(|hr| failed("the Windows Runtime could not be started", hr))?;
    let statics = factory("Windows.Media.Ocr.OcrEngine", &IID_OCR_ENGINE_STATICS).map_err(|hr| failed("the text recogniser of Windows is not there", hr))?;
    // SAFETY: `statics` is an IOcrEngineStatics; its slot 6 is get_MaxImageDimension (a UINT32), 7 AvailableRecognizerLanguages (an
    // IVectorView<Language>), 9 TryCreateFromLanguage(language, out engine), 10 TryCreateFromUserProfileLanguages(out engine). An
    // IVectorView has Size in slot 7 (a UINT32) and GetAt in slot 6; an ILanguage has LanguageTag in slot 6 (an HSTRING).
    let max_dimension = unsafe { statics.get_u32(6) }.map_err(|hr| failed("the recogniser did not say how big a picture it takes", hr))?;
    // The languages Windows can recognise (IOcrEngineStatics.AvailableRecognizerLanguages, a view of Language objects).
    // SAFETY: as above.
    let languages = unsafe { statics.get_obj(7) }.map_err(|hr| failed("the recogniser did not list its languages", hr))?;
    // SAFETY: `languages` is an IVectorView.
    let count = unsafe { languages.get_u32(7) }.unwrap_or(0).min(256);
    let mut found: Vec<(String, Obj)> = Vec::new();
    for i in 0..count {
        // SAFETY: `languages` is an IVectorView of Language objects, and a Language is an ILanguage.
        let Ok(language) = (unsafe { languages.at(i) }) else { continue };
        // SAFETY: as above (slot 6 of ILanguage is LanguageTag, an HSTRING).
        if let Ok(tag) = unsafe { language.get_string(6) } {
            found.push((tag, language));
        }
    }
    let tags = |found: &[(String, Obj)]| found.iter().map(|(tag, _)| tag.clone()).collect::<Vec<_>>();
    let (engine, language) = if chinese {
        // A Simplified Chinese recogniser first, any Chinese one after.
        let pick = found.iter().find(|(tag, _)| tag.starts_with("zh-Hans")).or_else(|| found.iter().find(|(tag, _)| tag.starts_with("zh")));
        let Some((tag, language)) = pick else { return Err(OcrError::NoLanguage { chinese: true, available: tags(&found) }) };
        // SAFETY: slot 9 of IOcrEngineStatics takes a Language and gives an OcrEngine.
        let engine = unsafe { statics.call_with(9, language) }.map_err(|_| OcrError::NoLanguage { chinese: true, available: tags(&found) })?;
        (engine, tag.clone())
    } else {
        // The languages of the person's profile (IOcrEngineStatics.TryCreateFromUserProfileLanguages), else the first there is.
        // SAFETY: slot 10 of IOcrEngineStatics gives an OcrEngine.
        match unsafe { statics.get_obj(10) } {
            Ok(engine) => {
                // SAFETY: slot 7 of IOcrEngine is get_RecognizerLanguage (a Language), whose slot 6 is the tag (an HSTRING).
                let tag = unsafe { engine.get_obj(7).and_then(|l| l.get_string(6)) }.unwrap_or_default();
                (engine, tag)
            }
            Err(_) => {
                let Some((tag, language)) = found.first() else { return Err(OcrError::NoLanguage { chinese: false, available: tags(&found) }) };
                // SAFETY: slot 9 of IOcrEngineStatics takes a Language and gives an OcrEngine.
                let engine = unsafe { statics.call_with(9, language) }.map_err(|_| OcrError::NoLanguage { chinese: false, available: tags(&found) })?;
                (engine, tag.clone())
            }
        }
    };
    let bitmaps = factory("Windows.Graphics.Imaging.SoftwareBitmap", &IID_SOFTWARE_BITMAP_STATICS).map_err(|hr| failed("the bitmaps of Windows are not there", hr))?;
    let buffers = factory("Windows.Security.Cryptography.CryptographicBuffer", &IID_CRYPTO_BUFFER_STATICS).map_err(|hr| failed("the buffers of Windows are not there", hr))?;
    Ok(Box::new(Recogniser { engine, bitmaps, buffers, language, max_dimension, _runtime: runtime }))
}

impl Recogniser {
    /// The picture as a `SoftwareBitmap` (Gray8): the bytes are copied into a buffer and the buffer into the bitmap, so nothing
    /// refers to the pixels after this returns.
    fn bitmap(&self, image: &GrayImage) -> Result<Obj, OcrError> {
        let mut buffer = null_mut();
        // SAFETY: slot 9 of ICryptographicBufferStatics is CreateFromByteArray(length, bytes, out); the pixels are `length` bytes
        // and Windows copies them during the call; `buffer` is written once.
        let hr = unsafe { self.buffers.method::<CreateFromBytes>(9)(self.buffers.0, image.pixels.len() as u32, image.pixels.as_ptr(), &mut buffer) };
        check(hr).map_err(|hr| failed("the picture could not be put in a buffer", hr))?;
        // SAFETY: the call succeeded, so `buffer` is a new reference to an IBuffer (or null, which `new` refuses).
        let buffer = unsafe { Obj::new(buffer) }.map_err(|hr| failed("the picture could not be put in a buffer", hr))?;
        let mut bitmap = null_mut();
        // SAFETY: slot 9 of ISoftwareBitmapStatics is CreateCopyFromBuffer(buffer, format, width, height, out); `buffer` is live
        // and holds width x height bytes of Gray8; the bitmap gets a copy; `bitmap` is written once.
        let hr = unsafe { self.bitmaps.method::<CreateBitmap>(9)(self.bitmaps.0, buffer.0, PIXEL_FORMAT_GRAY8, image.width as i32, image.height as i32, &mut bitmap) };
        check(hr).map_err(|hr| failed("the picture could not be made into a bitmap", hr))?;
        // SAFETY: the call succeeded, so `bitmap` is a new reference to a SoftwareBitmap (or null, which `new` refuses).
        unsafe { Obj::new(bitmap) }.map_err(|hr| failed("the picture could not be made into a bitmap", hr))
    }
}

impl Recognizer for Recogniser {
    fn language(&self) -> String {
        self.language.clone()
    }

    fn max_dimension(&self) -> u32 {
        self.max_dimension
    }

    fn recognize(&mut self, image: &GrayImage, cancel: &AtomicBool, timeout: Duration) -> Result<Vec<OcrLine>, OcrError> {
        if image.width == 0 || image.height == 0 || image.width > self.max_dimension || image.height > self.max_dimension || image.pixels.len() != image.width as usize * image.height as usize {
            return Err(OcrError::Failed("the picture is not a size the recogniser takes".to_string()));
        }
        let bitmap = self.bitmap(image)?;
        // RecognizeAsync(bitmap) -> IAsyncOperation<OcrResult> (IOcrEngine slot 6).
        // SAFETY: slot 6 of IOcrEngine is RecognizeAsync(SoftwareBitmap) and gives an IAsyncOperation<OcrResult>.
        let operation = unsafe { self.engine.call_with(6, &bitmap) }.map_err(|hr| failed("the recogniser did not start", hr))?;
        drop(bitmap);
        // From here on every way out cancels the operation if it still runs and closes it (`Pending`).
        let info = Pending(operation.query(&IID_ASYNC_INFO).map_err(|hr| failed("the recogniser's operation cannot be watched", hr))?);
        let started = std::time::Instant::now();
        let mut pause = 2;
        loop {
            match info.status() {
                Ok(ASYNC_COMPLETED) => break,
                Ok(ASYNC_STARTED) => {}
                Ok(_) => {
                    let code = info.error_code();
                    return Err(failed("the recogniser failed", if code < 0 { code } else { E_FAIL }));
                }
                Err(hr) => return Err(failed("the recogniser's state cannot be read", hr)),
            }
            if cancel.load(Ordering::SeqCst) {
                return Err(OcrError::Cancelled);
            }
            if started.elapsed() >= timeout {
                return Err(OcrError::TimedOut);
            }
            std::thread::sleep(Duration::from_millis(pause));
            pause = (pause * 2).min(25);
        }
        // IAsyncOperation<OcrResult>.GetResults (slot 8), then the lines and their words.
        // SAFETY: slot 8 of IAsyncOperation<OcrResult> is GetResults() and gives the OcrResult; the operation is completed.
        let result = unsafe { operation.get_obj(8) }.map_err(|hr| failed("the recogniser gave no result", hr))?;
        // SAFETY: the OcrResult's slot 6 is get_Lines (an IVectorView<OcrLine>), whose Size is slot 7 (a UINT32) and GetAt slot 6; an
        // OcrLine has Words in slot 6 (an IVectorView<OcrWord>) and an OcrWord has BoundingRect in slot 6 (a Rect) and Text in slot 7
        // (an HSTRING).
        let lines = unsafe { result.get_obj(6) }.map_err(|hr| failed("the recogniser gave no lines", hr))?;
        // SAFETY: `lines` is an IVectorView.
        let count = unsafe { lines.get_u32(7) }.map_err(|hr| failed("the recogniser's lines cannot be counted", hr))?.min(MAX_OCR_LINES);
        let (mut out, mut words_in_all) = (Vec::new(), 0usize);
        for i in 0..count {
            // SAFETY: as above.
            let line = unsafe { lines.at(i) }.map_err(|hr| failed("a line cannot be read", hr))?;
            // SAFETY: as above.
            let words = unsafe { line.get_obj(6) }.map_err(|hr| failed("the words of a line cannot be read", hr))?;
            // SAFETY: as above.
            let n = unsafe { words.get_u32(7) }.map_err(|hr| failed("the words of a line cannot be counted", hr))?;
            let mut line_words = Vec::new();
            for j in 0..n {
                words_in_all += 1;
                if words_in_all > MAX_WORDS_PER_PAGE {
                    return Err(OcrError::Failed("the page has too many words".to_string()));
                }
                // SAFETY: as above.
                let word = unsafe { words.at(j) }.map_err(|hr| failed("a word cannot be read", hr))?;
                // SAFETY: as above.
                let text = unsafe { word.get_string(7) }.map_err(|hr| failed("the text of a word cannot be read", hr))?;
                // SAFETY: as above.
                let [x, y, w, h] = unsafe { word.get_rect(6) }.map_err(|hr| failed("the box of a word cannot be read", hr))?;
                line_words.push(OcrWord { text, rect: [x, y, x + w, y + h] });
            }
            if !line_words.is_empty() {
                out.push(OcrLine { words: line_words });
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn the_structures_have_the_layout_windows_expects() {
        // The 64-bit sizes of NMHDR, MONITORINFO, MARGINS, NCCALCSIZE_PARAMS and HIGHCONTRASTW (commctrl.h, winuser.h, dwmapi.h).
        assert_eq!(std::mem::size_of::<NmHdr>(), 24);
        assert_eq!(std::mem::size_of::<MonitorInfo>(), 40);
        assert_eq!(std::mem::size_of::<Margins>(), 16);
        assert_eq!(std::mem::size_of::<NcCalcSize>(), 56);
        assert_eq!(std::mem::size_of::<HighContrastW>(), 16);
    }

    /// A painter over a memory device context of its own, with a section of 32-bit pixels `w` by `h` (no window needed).
    struct Canvas {
        gdi: Gdi,
        dc: isize,
        bitmap: isize,
        old: isize,
    }

    impl Canvas {
        fn new(w: i32, h: i32) -> Canvas {
            // SAFETY: a null DC gives a memory device context of the screen's kind; the section is made for it, checked, and both are freed in `drop`.
            let (dc, bitmap, bits, old) = unsafe {
                let dc = CreateCompatibleDC(0);
                let info = BitmapInfoHeader { size: std::mem::size_of::<BitmapInfoHeader>() as u32, width: w, height: -h, planes: 1, bit_count: 32, compression: 0, size_image: 0, x_pels: 0, y_pels: 0, clr_used: 0, clr_important: 0 };
                let mut bits: *mut c_void = null_mut();
                let bitmap = CreateDIBSection(dc, &info, 0, &mut bits, 0, 0);
                let old = SelectObject(dc, bitmap);
                (dc, bitmap, bits as usize, old)
            };
            assert!(dc != 0 && bitmap != 0 && bits != 0);
            let font = make_font("Segoe UI", 12, 400, 5, 1);
            // SAFETY: the DC and the font just made.
            unsafe { SelectObject(dc, font) };
            Canvas { gdi: Gdi { dc, bits, w, h, fonts: [font, font, font, font], lines: [16; 4], font: 0, pending: false }, dc, bitmap, old }
        }

        /// The pixel at (x, y) as 0xRRGGBB and its alpha.
        fn at(&mut self, x: i32, y: i32) -> (u32, u8) {
            let w = self.gdi.w;
            let px = self.gdi.pixels().and_then(|b| b.get(((y * w + x) * 4) as usize..((y * w + x) * 4 + 4) as usize).map(|p| [p[0], p[1], p[2], p[3]])).unwrap();
            ((u32::from(px[2]) << 16) | (u32::from(px[1]) << 8) | u32::from(px[0]), px[3])
        }
    }

    impl Drop for Canvas {
        fn drop(&mut self) {
            // SAFETY: the bitmap is taken out of the DC before it is deleted, and the DC after; the fonts (each once, they may be one) are deleted last; all of
            // them were made by the canvas.
            unsafe {
                SelectObject(self.dc, self.old);
                DeleteObject(self.bitmap);
                DeleteDC(self.dc);
                let mut seen: Vec<isize> = Vec::new();
                for f in self.gdi.fonts {
                    if f != 0 && !seen.contains(&f) {
                        seen.push(f);
                        DeleteObject(f);
                    }
                }
            }
        }
    }

    #[test]
    fn a_rectangle_comes_out_in_the_colour_that_was_asked_for_and_a_shape_has_smooth_corners() {
        let mut c = Canvas::new(80, 60);
        c.gdi.fill_rect(Rect { x: 0, y: 0, w: 80, h: 60 }, 0x102030);
        assert_eq!(c.at(5, 5), (0x102030, 255));
        // Red is red (the pixels are blue, green, red in memory, and the rasterizer is told red as blue).
        c.gdi.fill_shape(Rect { x: 10, y: 10, w: 40, h: 30 }, [10; 4], 0xFF0000, 255);
        assert_eq!(c.at(30, 25), (0xFF0000, 255));
        // The corner is cut: the pixel in the very corner is what was there, a pixel on the curve is a mix of the two.
        assert_eq!(c.at(10, 10).0, 0x102030);
        let on_curve = c.at(12, 13).0;
        assert!(on_curve != 0x102030 && on_curve != 0xFF0000, "{on_curve:06X}");
        // Half transparent: half way between.
        c.gdi.fill_rect(Rect { x: 0, y: 0, w: 80, h: 60 }, 0x000000);
        c.gdi.fill_shape(Rect { x: 0, y: 0, w: 80, h: 60 }, [0; 4], 0xFFFFFF, 128);
        let (v, _) = c.at(40, 30);
        assert!((0x7F..=0x81).contains(&(v & 0xFF)) && (0x7F..=0x81).contains(&((v >> 8) & 0xFF)), "{v:06X}");
        c.gdi.fill_rect_alpha(Rect { x: 0, y: 0, w: 10, h: 10 }, 0xFFFFFF, 128);
        let (v, a) = c.at(2, 2);
        assert!((0xBF..=0xC1).contains(&(v & 0xFF)) && a == 255, "{v:06X}");
        // Outside the picture nothing is drawn and nothing breaks.
        c.gdi.fill_rect(Rect { x: -50, y: -50, w: 20, h: 20 }, 1);
        c.gdi.fill_rect(Rect { x: 70, y: 50, w: 100, h: 100 }, 0x00FF00);
        c.gdi.fill_shape(Rect { x: -100, y: -100, w: 50, h: 50 }, [5; 4], 0xFF0000, 255);
        assert_eq!(c.at(79, 59), (0x00FF00, 255));
    }

    #[test]
    fn lines_and_outlines_are_drawn_with_round_ends_and_inside_their_rectangle() {
        let mut c = Canvas::new(60, 40);
        c.gdi.fill_rect(Rect { x: 0, y: 0, w: 60, h: 40 }, 0xFFFFFF);
        c.gdi.polyline(&[(10.0, 20.0), (50.0, 20.0)], 4.0, 0x0000FF, 255);
        assert_eq!(c.at(30, 20), (0x0000FF, 255));
        assert_eq!(c.at(30, 25).0, 0xFFFFFF);
        // The ends are round and reach half a width past the points.
        assert!(c.at(9, 20).0 != 0xFFFFFF && c.at(7, 20).0 == 0xFFFFFF);
        c.gdi.stroke_round(Rect { x: 5, y: 5, w: 30, h: 20 }, 6, 2, 0x00FF00, 255);
        assert_eq!(c.at(20, 5), (0x00FF00, 255), "the top edge, in the rectangle");
        assert_eq!(c.at(20, 4).0, 0xFFFFFF, "nothing outside it");
        assert_eq!(c.at(20, 15).0, 0xFFFFFF, "and nothing inside");
        c.gdi.polyline(&[], 4.0, 0, 255);
        c.gdi.polyline(&[(1.0, 1.0)], 4.0, 0, 255);
    }

    #[test]
    fn the_strip_colour_is_made_transparent_and_everything_else_in_the_strip_opaque() {
        let mut c = Canvas::new(20, 10);
        c.gdi.fill_rect(Rect { x: 0, y: 0, w: 20, h: 10 }, 0xE8ECEB);
        c.gdi.fill_rect(Rect { x: 5, y: 0, w: 5, h: 10 }, 0x1D9E75);
        c.gdi.key_out(0xE8ECEB, 4);
        assert_eq!(c.at(1, 1), (0, 0), "the strip's own colour shows what is behind it");
        assert_eq!(c.at(6, 1), (0x1D9E75, 255), "anything else in the strip stays");
        assert_eq!(c.at(1, 6), (0xE8ECEB, 255), "below the strip nothing changes");
    }

    #[test]
    fn a_rounded_rectangle_has_the_bounds_of_its_rectangle_and_none_without_an_area() {
        let path = round_path(Rect { x: 3, y: 4, w: 50, h: 30 }, [8, 8, 0, 0]).unwrap();
        let b = path.bounds();
        assert_eq!((b.left(), b.top(), b.right(), b.bottom()), (3.0, 4.0, 53.0, 34.0));
        assert!(round_path(Rect { x: 0, y: 0, w: 0, h: 5 }, [1; 4]).is_none());
        // A radius bigger than the rectangle is held to half of its shorter side.
        assert!(round_path(Rect { x: 0, y: 0, w: 10, h: 10 }, [500; 4]).is_some());
    }

    #[test]
    fn the_chrome_is_painted_in_the_time_it_has() {
        // A rough check, not a measurement: a title strip, a command bar and a pill's worth of shapes at 1920 by 1080 take a few milliseconds at most even in
        // a loaded test run.
        let mut c = Canvas::new(1920, 1080);
        let started = std::time::Instant::now();
        for _ in 0..20 {
            c.gdi.fill_rect(Rect { x: 0, y: 0, w: 1920, h: 124 }, 0xE8ECEB);
            for i in 0..12 {
                c.gdi.fill_shape(Rect { x: 40 + i * 40, y: 50, w: 28, h: 28 }, [6; 4], 0xE1F5EE, 255);
                c.gdi.polyline(&[(50.0 + 40.0 * i as f32, 60.0), (60.0 + 40.0 * i as f32, 70.0)], 1.5, 0x1E2422, 255);
            }
            c.gdi.fill_shape(Rect { x: 1500, y: 50, w: 240, h: 28 }, [14; 4], 0xEFF2F1, 255);
            c.gdi.stroke_round(Rect { x: 1500, y: 50, w: 240, h: 28 }, 14, 1, 0xD6DBD9, 255);
        }
        let per = started.elapsed().as_secs_f64() * 1000.0 / 20.0;
        assert!(per < 20.0, "{per} ms");
    }


    impl Canvas {
        /// A canvas whose fonts are the window's (the face of the language, bold, big, the icons) for the dots per inch.
        fn with_fonts(w: i32, h: i32, dpi: i32, zh: bool) -> Canvas {
            let mut c = Canvas::new(w, h);
            let body = (9 * dpi + 36) / 72;
            let face = if zh { "Microsoft YaHei UI" } else { "Segoe UI" };
            let fonts = [make_font(face, body, 400, 5, 1), make_font(face, body, 700, 5, 1), make_font(face, 28 * dpi / 96, 700, 5, 1), make_font("Segoe Fluent Icons", cmdbar::ICON_PX * dpi / 96, 400, ANTIALIASED_QUALITY, DEFAULT_CHARSET)];
            // The font `Canvas::new` made is given back with the others at the end (it is the first of the old ones).
            // SAFETY: the DC and the font made by `new`, which is no longer selected once the first of the new ones is.
            unsafe {
                SelectObject(c.dc, fonts[0]);
                DeleteObject(c.gdi.fonts[0]);
            }
            c.gdi.fonts = fonts;
            for (i, &f) in fonts.iter().enumerate() {
                // SAFETY: the memory DC and a font just made.
                unsafe { SelectObject(c.dc, f) };
                c.gdi.lines[i] = line_height(c.dc);
            }
            c.gdi.set_font(Font::Body);
            c
        }

        /// The picture as the bytes of a 32-bit BMP file (rows from the bottom), for looking at.
        fn bmp(&mut self) -> Vec<u8> {
            let (w, h) = (self.gdi.w as usize, self.gdi.h as usize);
            let pixels = self.gdi.pixels().map(|b| b.to_vec()).unwrap_or_default();
            let mut out = Vec::with_capacity(54 + pixels.len());
            out.extend_from_slice(b"BM");
            out.extend_from_slice(&((54 + pixels.len()) as u32).to_le_bytes());
            out.extend_from_slice(&[0; 4]);
            out.extend_from_slice(&54u32.to_le_bytes());
            out.extend_from_slice(&40u32.to_le_bytes());
            out.extend_from_slice(&(w as i32).to_le_bytes());
            out.extend_from_slice(&(h as i32).to_le_bytes());
            out.extend_from_slice(&1u16.to_le_bytes());
            out.extend_from_slice(&32u16.to_le_bytes());
            out.extend_from_slice(&[0; 24]);
            for row in pixels.chunks_exact(w * 4).rev() {
                out.extend_from_slice(row);
            }
            out
        }
    }

    /// The real reader, with the file open and the window of this size (the engine runs on its thread as it does in the window).
    fn real_reader(w: i32, h: i32, dpi: u32, zh: bool, file: Option<&str>) -> crate::app::App {
        let mut app = crate::app::App::new(if zh { crate::i18n::Lang::Zh } else { crate::i18n::Lang::En }, std::sync::Arc::new(|| {}), std::time::Instant::now(), 1920 * 1080, "Tester".to_string());
        app.persist = false;
        app.set_bare(false);
        app.event(Event::Dpi(dpi));
        if let Some(name) = file {
            app.open_when_ready(std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/corpus/public/xref-classic/").to_string() + name));
        }
        app.event(Event::Size { w, h });
        let started = std::time::Instant::now();
        while file.is_some() && !app.has_document() && started.elapsed() < std::time::Duration::from_secs(30) {
            app.event(Event::Wake);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        for _ in 0..40 {
            app.event(Event::Wake);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        app
    }

    #[test]
    fn the_window_is_painted_by_the_real_reader_in_a_few_milliseconds() {
        let mut app = real_reader(1920, 1080, 96, false, Some("hello_world_2_pages.pdf"));
        let mut c = Canvas::with_fonts(1920, 1080, 96, false);
        // A first picture (it makes what is made once), then the average of twenty.
        app.paint(&mut c.gdi);
        let started = std::time::Instant::now();
        for _ in 0..20 {
            app.paint(&mut c.gdi);
        }
        let per = started.elapsed().as_secs_f64() * 1000.0 / 20.0;
        // The whole picture, pages' area and all, at 1920 by 1080: the chrome is a small part of it. (A loose guard, since a test may run on a busy machine: the
        // numbers that are kept are measured on a quiet one and are printed here.)
        // The chrome alone: the title strip, the command bar and what lies over the pages.
        let started = std::time::Instant::now();
        for _ in 0..20 {
            app.paint_chrome_only(&mut c.gdi);
        }
        let chrome = started.elapsed().as_secs_f64() * 1000.0 / 20.0;
        println!("paint at 1920x1080: {per:.2} ms, of which the chrome {chrome:.2} ms");
        assert!(chrome < if cfg!(debug_assertions) { 1000.0 } else { 12.0 }, "the chrome takes {chrome} ms");
        // (The test build without optimization draws a hundred times slower than the release build: the check is for the release.)
        assert!(per < if cfg!(debug_assertions) { 1000.0 } else { 40.0 }, "{per} ms");
        // The title strip is the strip's colour and the command bar the surface's, to the pixel.
        assert_eq!(c.at(900, 5), (crate::theme::LIGHT.strip_bg, 255));
        assert_eq!(c.at(900, 60), (crate::theme::LIGHT.surface, 255));
        // A whole picture filled with one colour (what the pages' area is before the pages are drawn on it).
        let started = std::time::Instant::now();
        for _ in 0..20 {
            c.gdi.fill_rect(Rect { x: 0, y: 0, w: 1920, h: 1080 }, 0xEBECEC);
        }
        let fill = started.elapsed().as_secs_f64() * 1000.0 / 20.0;
        println!("a whole picture filled in {fill:.2} ms");
        assert!(fill < if cfg!(debug_assertions) { 1000.0 } else { 12.0 }, "{fill} ms");
    }

    /// Pictures of the window in several states, written to the folder `QINGPDF_DUMP_DIR` names (as .bmp), for looking at: the pointer on a button (its tooltip),
    /// on a tab, on the pill, a message, the welcome page; light and dark, in both languages. `cargo test --release dump_the_states -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn dump_the_states_of_the_window() {
        let Some(dir) = std::env::var_os("QINGPDF_DUMP_DIR") else { return };
        let dir = std::path::PathBuf::from(dir);
        // The dots per inch can be asked for (`QINGPDF_DUMP_DPI`, 96 to 192): the window is as big in inches as the one of 1400 by 900 at 120.
        let dpi: u32 = std::env::var("QINGPDF_DUMP_DPI").ok().and_then(|v| v.parse().ok()).unwrap_or(120);
        let (w, h) = (1400 * dpi as i32 / 120, 900 * dpi as i32 / 120);
        let s = |v: i32| v * dpi as i32 / 96;
        let shot = |app: &mut crate::app::App, zh: bool, name: &str| {
            let mut c = Canvas::with_fonts(w, h, dpi as i32, zh);
            app.paint(&mut c.gdi);
            let _ = std::fs::write(dir.join(format!("{name}-{dpi}.bmp")), c.bmp());
        };
        for (zh, dark) in [(true, false), (false, false), (true, true)] {
            let tag = format!("{}-{}", if zh { "zh" } else { "en" }, if dark { "dark" } else { "light" });
            let mut app = real_reader(w, h, dpi, zh, Some("hello_world_2_pages.pdf"));
            app.event(Event::Command(if dark { crate::app::CMD_THEME_DARK } else { crate::app::CMD_THEME_LIGHT }));
            let l = cmdbar::layout(w, dpi);
            let print = l.buttons.iter().find(|(i, _)| cmdbar::BUTTONS[*i].cmd == crate::app::CMD_PRINT).map(|(_, r)| *r).unwrap_or(Rect { x: 0, y: 0, w: 0, h: 0 });
            let top = s(crate::tabs::TITLE_H as i32);
            // The pointer on the print button, and its tooltip after a moment.
            app.event(Event::Hover { x: print.x + 5, y: top + print.y + 5 });
            app.event(Event::Timer(8));
            shot(&mut app, zh, &format!("tip-{tag}"));
            // On the tool in use; on a tab and its cross; on the pill's zoom button.
            app.event(Event::Command(crate::app::CMD_TOOL_INK));
            let ink = l.buttons.iter().find(|(i, _)| cmdbar::BUTTONS[*i].cmd == crate::app::CMD_TOOL_INK).map(|(_, r)| *r).unwrap_or(Rect { x: 0, y: 0, w: 0, h: 0 });
            app.event(Event::Hover { x: ink.x + 5, y: top + ink.y + 5 });
            shot(&mut app, zh, &format!("hover-tool-{tag}"));
            app.event(Event::Hover { x: s(150), y: s(20) });
            shot(&mut app, zh, &format!("hover-tab-{tag}"));
            let pill = crate::pill::layout(Rect { x: 0, y: 0, w, h: h - s(84) }, dpi, s(60), s(40));
            let zoom_in = pill.parts.iter().find(|(p, _)| *p == crate::pill::Part::ZoomIn).map(|(_, r)| *r).unwrap_or(Rect { x: 0, y: 0, w: 0, h: 0 });
            app.event(Event::Hover { x: zoom_in.x + 5, y: s(84) + zoom_in.y + 5 });
            shot(&mut app, zh, &format!("hover-pill-{tag}"));
        }
        // The welcome page, with a list.
        for (zh, dark) in [(true, false), (false, false)] {
            let tag = format!("{}-{}", if zh { "zh" } else { "en" }, if dark { "dark" } else { "light" });
            let mut app = real_reader(w, h, dpi, zh, None);
            let _ = &mut app;
            shot(&mut app, zh, &format!("welcome-{tag}"));
        }
    }
}
