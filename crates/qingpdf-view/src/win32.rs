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
use crate::placement::{self, Placement};
use crate::toolbar::{self, IconFont};
use crate::ui::{Action, Bar, Cursor, Event, FieldBox, FieldExit, FormAnswer, FormKind, FormSpec, Handler, Menu, MenuEntry, OutlineNode, Painter, PrintOp, PrinterPreset, PrinterSetup, Rect, Rgb, SaveChoice, ScrollCode, ScrollInfo, StatusText, ToolItem, ToolbarSpec, ToolbarState};

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
struct MonitorInfo {
    size: u32,
    monitor: WinRect,
    work: WinRect,
    flags: u32,
}

/// A button of a toolbar (the 64-bit layout of TBBUTTON).
#[repr(C)]
struct TbButton {
    bitmap: i32,
    command: i32,
    state: u8,
    style: u8,
    reserved: [u8; 6],
    data: usize,
    string: isize,
}

/// What TB_SETBUTTONINFOW is given (the 64-bit layout of TBBUTTONINFOW).
#[repr(C)]
struct TbButtonInfo {
    size: u32,
    mask: u32,
    command: i32,
    image: i32,
    state: u8,
    style: u8,
    width: u16,
    param: usize,
    text: *mut u16,
    text_max: i32,
}

/// The ToolTip's request for the words of a tip (NMTTDISPINFOW).
#[repr(C)]
struct NmTtDispInfo {
    hdr: NmHdr,
    text: *mut u16,
    buf: [u16; 80],
    instance: isize,
    flags: u32,
    param: isize,
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
    fn ClientToScreen(hwnd: isize, point: *mut Point) -> i32;
    fn TrackPopupMenu(menu: isize, flags: u32, x: i32, y: i32, reserved: i32, hwnd: isize, rect: *const WinRect) -> i32;
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
    fn CreateDIBSection(hdc: isize, info: *const BitmapInfoHeader, usage: u32, bits: *mut *mut c_void, section: isize, offset: u32) -> isize;
    fn GdiFlush() -> i32;
    fn GetClipBox(hdc: isize, rect: *mut WinRect) -> i32;
    fn GetTextFaceW(hdc: isize, count: i32, buffer: *mut u16) -> i32;
}

#[link(name = "msimg32")]
unsafe extern "system" {
    #[allow(clippy::too_many_arguments)]
    fn AlphaBlend(dest: isize, xd: i32, yd: i32, wd: i32, hd: i32, src: isize, xs: i32, ys: i32, ws: i32, hs: i32, blend: u32) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> isize;
    fn GetLocalTime(time: *mut LocalTime);
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

#[link(name = "advapi32")]
unsafe extern "system" {
    fn GetUserNameW(buffer: *mut u16, size: *mut u32) -> i32;
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
    fn ImageList_Create(cx: i32, cy: i32, flags: u32, initial: i32, grow: i32) -> isize;
    fn ImageList_Add(list: isize, bitmap: isize, mask: isize) -> i32;
    fn ImageList_Destroy(list: isize) -> i32;
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
const WS_EX_ACCEPTFILES: u32 = 0x0000_0010;
const WS_EX_DLGMODALFRAME: u32 = 0x0000_0001;
const WS_EX_CLIENTEDGE: u32 = 0x0000_0200;
const CW_USEDEFAULT: i32 = i32::MIN;
const SW_SHOWNORMAL: i32 = 1;
const SW_SHOW: i32 = 5;
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
const ID_TOOLBAR: isize = 105;
const ID_STATUS: isize = 106;
const ID_VBAR: isize = 107;
const ID_HBAR: isize = 108;
const ID_PAGE_BOX: isize = 109;
const ID_SEARCH_BOX: isize = 110;
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
const WM_USER: u32 = 0x0400;
/// Posted to the window to select all the text of a box of the toolbar once the click that gave it the keyboard is done.
const WM_SELECT_BOX: u32 = 0x8002;
const EN_SETFOCUS: u32 = 0x0100;
const ES_CENTER: u32 = 0x1;
const EM_SETCUEBANNER: u32 = 0x1501;
const ICC_BAR_CLASSES: u32 = 0x4;
const SBS_HORZ: u32 = 0;
const SBS_VERT: u32 = 1;
const SB_CTL: i32 = 2;
const SB_ENDSCROLL: u32 = 8;
const SM_CXVSCROLL: i32 = 2;
const SM_CYHSCROLL: i32 = 3;
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
const TPM_RIGHTBUTTON: u32 = 0x2;
const CCS_NORESIZE: u32 = 0x4;
const CCS_NOPARENTALIGN: u32 = 0x8;
const CCS_NODIVIDER: u32 = 0x40;
const TBSTYLE_TOOLTIPS: u32 = 0x100;
const TBSTYLE_FLAT: u32 = 0x800;
const BTNS_SEP: u8 = 0x1;
const BTNS_CHECK: u8 = 0x2;
const BTNS_AUTOSIZE: u8 = 0x10;
const BTNS_SHOWTEXT: u8 = 0x40;
const TBSTATE_ENABLED: u8 = 0x4;
const I_IMAGENONE: i32 = -2;
const TB_ENABLEBUTTON: u32 = WM_USER + 1;
const TB_CHECKBUTTON: u32 = WM_USER + 2;
const TB_BUTTONCOUNT: u32 = WM_USER + 24;
const TB_DELETEBUTTON: u32 = WM_USER + 22;
const TB_GETITEMRECT: u32 = WM_USER + 29;
const TB_BUTTONSTRUCTSIZE: u32 = WM_USER + 30;
const TB_SETBUTTONSIZE: u32 = WM_USER + 31;
const TB_AUTOSIZE: u32 = WM_USER + 33;
const TB_SETIMAGELIST: u32 = WM_USER + 48;
const TB_SETDISABLEDIMAGELIST: u32 = WM_USER + 54;
const TB_GETBUTTONSIZE: u32 = WM_USER + 58;
const TB_ADDBUTTONSW: u32 = WM_USER + 68;
const TB_SETBUTTONINFOW: u32 = WM_USER + 64;
const TBIF_SIZE: u32 = 0x40;
/// The commands of the two buttons that hold the places of the boxes of the toolbar (no menu command is so big).
const BOX_COMMAND: i32 = 0x7F01;
const SB_SETPARTS: u32 = WM_USER + 4;
const SB_SETTEXTW: u32 = WM_USER + 11;
const TTN_GETDISPINFOW: u32 = -530i32 as u32;
const ILC_COLOR32: u32 = 0x20;
const DEFAULT_CHARSET: u32 = 1;
const ANTIALIASED_QUALITY: u32 = 4;
/// The height of the status bar at 96 dpi.
const STATUS_HEIGHT: i32 = 26;
/// The size of a toolbar picture at 96 dpi, and the room the buttons have round it.
const TOOL_ICON: i32 = 16;
/// The widths at 96 dpi of the gap between groups of buttons and of the page and search boxes.
const TOOL_GAP: i32 = 10;
const PAGE_BOX_W: i32 = 88;
const SEARCH_BOX_W: i32 = 200;
const BOX_H: i32 = 24;
/// The colours of the pictures: ink, and the same greyed.
const ICON_INK: u32 = 0x1F1F1F;
const ICON_GREY: u32 = 0xA0A0A0;
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

/// Today's date in the person's time zone, as `2026-10-15` (what a page number's `{date}` is).
pub fn local_date() -> String {
    let mut t = LocalTime::default();
    // SAFETY: `t` is a live SYSTEMTIME of the size Windows expects (eight 16-bit numbers) that Windows fills in and keeps no pointer to.
    unsafe { GetLocalTime(&mut t) };
    format!("{:04}-{:02}-{:02}", t.year, t.month, t.day)
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
    /// The boxes a form field or a text box is typed in, and the one that is in use (0: none).
    field_single: Cell<isize>,
    field_multi: Cell<isize>,
    field_active: Cell<isize>,
    /// The printer a job is going to (from the print box, or named by the hidden test), until the job ends.
    printer: RefCell<Option<Printer>>,
    /// A menu is open (between WM_ENTERMENULOOP and WM_EXITMENULOOP), and the menus that came meanwhile, to be put when it closes:
    /// destroying the menu bar under an open menu would close it under the person's hand.
    menu_open: Cell<bool>,
    menus_waiting: RefCell<Option<Vec<Menu>>>,
    /// The toolbar, the boxes that lie over it (the page and the words to find), the status bar and the two scroll bars: children of
    /// the window (0 if Windows would not make them). The scroll bars are controls and not the window's own so that the vertical one
    /// ends above the status bar.
    toolbar: Cell<isize>,
    tb_page: Cell<isize>,
    tb_search: Cell<isize>,
    status: Cell<isize>,
    vbar: Cell<isize>,
    hbar: Cell<isize>,
    /// The toolbar as the reader last described it, how it was told to look, and where its boxes are in the list of its buttons.
    tb_spec: RefCell<Option<ToolbarSpec>>,
    tb_state: RefCell<ToolbarState>,
    tb_visible: Cell<bool>,
    tb_height: Cell<i32>,
    tb_boxes: Cell<(i32, i32)>,
    /// The two image lists of the toolbar's pictures (inked and greyed), and the font they were drawn from (found the first time).
    tb_lists: Cell<(isize, isize)>,
    icon_font: Cell<Option<IconFont>>,
    /// The horizontal scroll bar is shown (there is more across than the window shows).
    hbar_on: Cell<bool>,
    /// The page area as the reader was told: its top, width and height (the window less the toolbar, the scroll bar, the status bar).
    content: Cell<(i32, i32, i32)>,
    /// The window's place when it is neither maximized nor minimized, and whether it is kept for the next run.
    normal: Cell<Option<WinRect>>,
    remember_window: bool,
    clicks: RefCell<ClickCounter>,
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
            Action::SetScroll { bar, info } => set_scroll(state, bar, info),
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
            Action::AskText { tag, title, prompt, secret, ok, cancel } => {
                let text = input_dialog(hwnd, state.dpi.get(), state.font.get(), &title, &prompt, secret, &ok, &cancel);
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
            Action::SetMenus(menus) => set_menus(state, hwnd, menus),
            Action::Outline(nodes) => fill_tree(state.tree.get(), &nodes),
            Action::Sidebar(r) => place_child(state.tree.get(), to_client(state, r)),
            Action::Find(rect) => show_find(state, hwnd, rect.map(|r| to_client(state, r))),
            Action::FieldEdit(Some(b)) => show_field(state, hwnd, FieldBox { rect: to_client(state, b.rect), ..b }),
            Action::FieldEdit(None) => hide_field(state, hwnd),
            Action::FieldMove(r) => {
                let edit = state.field_active.get();
                if edit != 0 {
                    let r = to_client(state, r);
                    // SAFETY: plain handle of a child window of ours and plain numbers.
                    unsafe { MoveWindow(edit, r.x, r.y, r.w.max(1), r.h.max(1), 1) };
                }
            }
            Action::Choose { items, selected, at } => {
                let chosen = choose(hwnd, &items, selected, (at.0, at.1.saturating_add(content_top(state))));
                queue.extend(deliver(state, Event::Chosen(chosen)));
            }
            Action::ContextMenu { entries, at } => {
                let menu = build_popup(&entries);
                if menu != 0 {
                    let mut p = Point { x: at.0, y: at.1.saturating_add(content_top(state)) };
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
            Action::Toolbar(spec) => set_toolbar(state, spec),
            Action::ToolbarState(st) => {
                *state.tb_state.borrow_mut() = st;
                apply_toolbar_state(state);
            }
            Action::Status(st) => set_status(state, &st),
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

fn set_scroll(state: &WindowState, bar: Bar, info: ScrollInfo) {
    let (ctl, extra) = match bar {
        // The vertical bar stays (greyed when there is nothing to scroll) so that the window does not change its
        // width when a page is just too short; the horizontal one comes and goes.
        Bar::Vertical => (state.vbar.get(), SIF_DISABLENOSCROLL),
        Bar::Horizontal => (state.hbar.get(), 0),
    };
    if ctl == 0 {
        return;
    }
    let si = ScrollInfoW { size: std::mem::size_of::<ScrollInfoW>() as u32, mask: SIF_RANGE | SIF_PAGE | SIF_POS | extra, min: info.min, max: info.max, page: info.page, pos: info.pos, track_pos: 0 };
    // SAFETY: `si` is a live SCROLLINFO of the size given in `size`; Windows copies it. The bar is a scroll bar control of ours.
    unsafe { SetScrollInfo(ctl, SB_CTL, &si, 1) };
    if bar == Bar::Horizontal {
        // As a window's own bar would, the horizontal one is shown when there is more across than the window shows.
        let more = info.max > info.min && i64::from(info.max) - i64::from(info.min) + 1 > i64::from(info.page);
        if more != state.hbar_on.get() {
            state.hbar_on.set(more);
            apply_layout(state);
        }
    }
}

fn track_pos(ctl: isize) -> i32 {
    let mut si = ScrollInfoW { size: std::mem::size_of::<ScrollInfoW>() as u32, mask: SIF_TRACKPOS, min: 0, max: 0, page: 0, pos: 0, track_pos: 0 };
    // SAFETY: `si` is a live SCROLLINFO of the size given in `size`; Windows fills it in and does not keep it. The bar is a scroll bar
    // control of ours.
    unsafe { GetScrollInfo(ctl, SB_CTL, &mut si) };
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
        WM_ERASEBKGND => {
            // The pages are drawn over their area in one go, with no erasing first (that would flicker). What a child window asks this
            // window to draw behind it (the toolbar does, to show the colour of a window) is filled in the colour of a window.
            let dc = wparam as isize;
            let mut r = WinRect::default();
            // SAFETY: `dc` is the device context that came with the message; `r` is a live RECT that Windows fills in; the brush is the
            // system's own, shared and never freed. A DC that is this window's own is left alone.
            unsafe {
                if WindowFromDC(dc) != hwnd && GetClipBox(dc, &mut r) != 0 {
                    FillRect(dc, &r, GetSysColorBrush(COLOR_BTNFACE));
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
            dispatch(state, Event::Wheel { delta, ctrl: keys & 0x8 != 0, shift: keys & 0x4 != 0, x: p.x, y: p.y - content_top(state) });
            return 0;
        }
        WM_MOUSEHWHEEL => {
            dispatch(state, Event::HWheel { delta: i32::from(hiword(wparam) as u16 as i16) });
            return 0;
        }
        WM_VSCROLL | WM_HSCROLL => {
            // The scroll bars are controls: lparam is the one that was used.
            let (bar, ctl) = if msg == WM_VSCROLL { (Bar::Vertical, state.vbar.get()) } else { (Bar::Horizontal, state.hbar.get()) };
            if ctl == 0 || lparam != ctl {
                return 0;
            }
            let code = match loword(wparam) {
                0 => Some(ScrollCode::LineUp),
                1 => Some(ScrollCode::LineDown),
                2 => Some(ScrollCode::PageUp),
                3 => Some(ScrollCode::PageDown),
                4 | 5 => Some(ScrollCode::Thumb(track_pos(ctl))),
                6 => Some(ScrollCode::Top),
                7 => Some(ScrollCode::Bottom),
                _ => None,
            };
            if let Some(code) = code {
                dispatch(state, Event::Scroll { bar, code });
            }
            if loword(wparam) == SB_ENDSCROLL {
                // The pages get the keyboard back, so that the keys scroll them.
                // SAFETY: plain handle of the main window.
                unsafe { SetFocus(hwnd) };
            }
            return 0;
        }
        WM_COMMAND => {
            let code = hiword(wparam);
            if lparam == 0 {
                dispatch(state, Event::Command(loword(wparam)));
            } else if lparam == state.toolbar.get() {
                // A button of the toolbar.
                dispatch(state, Event::Command(loword(wparam)));
                // The pages get the keyboard back, so that the keys scroll them.
                // SAFETY: plain handle of the main window.
                unsafe { SetFocus(hwnd) };
            } else if (lparam == state.find.get() || lparam == state.tb_search.get()) && code == EN_CHANGE {
                let text = window_text(lparam, MAX_FIND_CHARS);
                dispatch(state, Event::FindText(text));
            } else if lparam == state.tb_search.get() && code == EN_SETFOCUS {
                dispatch(state, Event::FindFocus);
                // SAFETY: posts a message to this window; plain numbers (the box is selected once the click that gave it the keyboard is done).
                unsafe { PostMessageW(hwnd, WM_SELECT_BOX, lparam as usize, 0) };
            } else if lparam == state.tb_page.get() && code == EN_SETFOCUS {
                // SAFETY: as above.
                unsafe { PostMessageW(hwnd, WM_SELECT_BOX, lparam as usize, 0) };
            } else if lparam == state.tb_page.get() && code == EN_KILLFOCUS {
                // What was typed and not sent is not kept: the box says the page again.
                set_text(lparam, &state.tb_state.borrow().page_text);
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
            dispatch(state, Event::MouseDown { x, y: y - content_top(state), ctrl: modifier(0x11), shift: modifier(0x10), clicks });
            return 0;
        }
        WM_MOUSEMOVE => {
            if wparam & MK_LBUTTON != 0 {
                let (x, y) = mouse_place(lparam);
                dispatch(state, Event::MouseMove { x, y: y - content_top(state) });
            }
            return 0;
        }
        WM_LBUTTONUP => {
            // The reader hears the button come up first: giving the mouse back makes Windows say that the window lost it
            // (WM_CAPTURECHANGED), and the reader would take that for a drag cut short.
            let (x, y) = mouse_place(lparam);
            dispatch(state, Event::MouseUp { x, y: y - content_top(state) });
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
                    Ok(mut guard) => guard.as_mut().map_or(Cursor::Arrow, |h| h.cursor_at(p.x, p.y - content_top(state))),
                    Err(_) => Cursor::Arrow,
                };
                let id = match shape {
                    Cursor::SizeWE => IDC_SIZEWE,
                    Cursor::Hand => IDC_HAND,
                    Cursor::Cross => IDC_CROSS,
                    Cursor::Text => IDC_IBEAM,
                    Cursor::Arrow => IDC_ARROW,
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
                tree_notified(state, hwnd, hdr, lparam);
            } else if let Some(hdr) = hdr
                && hdr.code == TTN_GETDISPINFOW
            {
                // SAFETY: for TTN_GETDISPINFOW, lparam points to an NMTTDISPINFOW that is valid, and ours to fill in, for the duration of
                // this message; the one reference made is used within it.
                if let Some(info) = unsafe { (lparam as *mut NmTtDispInfo).as_mut() } {
                    tooltip_text(state, info);
                }
            }
            return 0;
        }
        WM_CONTEXTMENU => {
            // The right button (or the menu key) over the pages: the reader may have a menu for the place. Over a child window the
            // child's own menu is its business.
            if wparam as isize == hwnd {
                let (top, w, h) = state.content.get();
                let mut p = Point { x: i32::from(loword(lparam as usize) as u16 as i16), y: i32::from(hiword(lparam as usize) as u16 as i16) };
                if lparam as i32 == -1 {
                    p = Point { x: w / 2, y: top.max(0) + h / 2 };
                } else {
                    // SAFETY: `p` is a live POINT that Windows rewrites in place.
                    unsafe { ScreenToClient(hwnd, &mut p) };
                }
                let (x, y) = (p.x, p.y - top.max(0));
                if x >= 0 && y >= 0 && x < w && y < h {
                    dispatch(state, Event::ContextMenu { x, y });
                }
                return 0;
            }
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
            // SAFETY: takes this window's handle; does nothing if it has no reason.
            unsafe { ShutdownBlockReasonDestroy(hwnd) };
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
    for child in [state.tree.get(), state.find.get(), state.field_single.get(), state.field_multi.get(), state.tb_page.get(), state.tb_search.get(), state.status.get()] {
        if child != 0 && state.font.get() != 0 {
            // SAFETY: WM_SETFONT takes a font handle (the one just made) and a redraw flag; `child` is a window of ours.
            unsafe { SendMessageW(child, WM_SETFONT, state.font.get() as usize, 1) };
        }
    }
    if old != 0 {
        // SAFETY: `old` was a font made by `make_font`; it is no longer selected into a device context or set as a control's font.
        unsafe { DeleteObject(old) };
    }
    // The pictures of the toolbar are drawn again at the size of the new screen, and everything is laid out again.
    set_icons(state.hwnd.get(), dpi);
    if state.tb_visible.get() {
        rebuild_toolbar(state);
    }
    apply_layout(state);
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
        paint_to(state, hdc);
    }
    // SAFETY: pairs the BeginPaint above with the same window and the same PAINTSTRUCT.
    unsafe { EndPaint(hwnd, &ps) };
}

/// Draw the reader on the back buffer and put that on the window's device context `hdc`.
fn paint_to(state: &WindowState, hdc: isize) {
    let (top, w, h) = state.content.get();
    if w <= 0 || h <= 0 {
        return;
    }
    let top = top.max(0);
    if state.hbar_on.get() {
        // Where the two scroll bars meet, nothing is drawn by either: the colour of the window.
        let (vw, hh) = (metric(SM_CXVSCROLL, state.dpi.get()), metric(SM_CYHSCROLL, state.dpi.get()));
        let corner = WinRect { left: w, top: top + h, right: w + vw, bottom: top + h + hh };
        // SAFETY: `hdc` is the valid DC of this paint; `corner` is a live RECT; the brush is the system's own, shared and never freed.
        unsafe { FillRect(hdc, &corner, GetSysColorBrush(COLOR_BTNFACE)) };
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
        unsafe { BitBlt(hdc, 0, top, w, h, back.dc, 0, 0, SRCCOPY) };
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
        let mut g = Gdi { dc, line: sketch.line, tint: None };
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
    // SAFETY: stores the address of `state` for `dialog_proc`; the caller keeps `state` alive until after the window is destroyed
    // (`end_modal`, which clears the slot), and the slot is only read on this thread.
    unsafe { SetWindowLongPtrW(dialog, GWLP_USERDATA, std::ptr::from_ref(state) as isize) };
    Some((dialog, instance))
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
fn input_dialog(owner: isize, dpi: u32, font: isize, title: &str, prompt: &str, secret: bool, ok: &str, cancel: &str) -> Option<String> {
    let state = DialogState { done: Cell::new(false), accepted: Cell::new(false), pressed: Cell::new(0), sketch: RefCell::new(None) };
    let (dialog, instance) = make_dialog(owner, dpi, title, (380, 138), &state)?;
    let child = |class: &str, text: &str, style: u32, ex: u32, at: (i32, i32, i32, i32), id: usize| dialog_child(dialog, instance, dpi, font, class, text, style, ex, at, id);
    child("STATIC", prompt, 0, 0, (16, 14, 348, 20), 100);
    let edit = child("EDIT", "", WS_TABSTOP | ES_AUTOHSCROLL | if secret { ES_PASSWORD } else { 0 }, WS_EX_CLIENTEDGE, (16, 38, 348, 24), 101);
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
            FormKind::Label => ("STATIC", 0, 0),
            FormKind::Group => ("BUTTON", BS_GROUPBOX, 0),
            FormKind::Edit => ("EDIT", WS_TABSTOP | ES_AUTOHSCROLL, WS_EX_CLIENTEDGE),
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
            if focus == 0 && matches!(item.kind, FormKind::Edit | FormKind::Combo) {
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
    let foot = spec.size.1 - 36;
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
                FormKind::Pick => values.push(ask(h, CB_GETCURSEL).max(0).to_string()),
                FormKind::Check => values.push(String::from(if ask(h, BM_GETCHECK) == 1 { "1" } else { "0" })),
                FormKind::Label | FormKind::Group | FormKind::Button => {}
            }
        }
        FormAnswer { values, button: state.pressed.get().checked_sub(1) }
    });
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

// --- the toolbar, the status bar, the scroll bars and the layout --------------------------------------------------------

fn scale(dpi: u32, v: i32) -> i32 {
    (i64::from(v) * i64::from(dpi) / 96) as i32
}

fn metric(index: i32, dpi: u32) -> i32 {
    // SAFETY: takes two plain numbers and reads a system setting.
    unsafe { GetSystemMetricsForDpi(index, dpi) }
}

/// The top of the page area in the client area: the toolbar is above it when it is shown.
fn content_top(state: &WindowState) -> i32 {
    state.content.get().0.max(0)
}

/// A rectangle of the page area (as the reader says it) in the window's client area.
fn to_client(state: &WindowState, r: Rect) -> Rect {
    Rect { y: r.y.saturating_add(content_top(state)), ..r }
}

/// Move a child window and show it, or hide it.
fn put(child: isize, x: i32, y: i32, w: i32, h: i32, show: bool) {
    if child == 0 {
        return;
    }
    // SAFETY: plain handle of a child window of ours and plain numbers.
    unsafe {
        MoveWindow(child, x, y, w.max(0), h.max(0), 1);
        ShowWindow(child, if show { SW_SHOW } else { SW_HIDE });
    }
}

/// Put the toolbar, the scroll bars and the status bar in their places; what is left of the window is the page area, and the reader
/// is told its size when that has changed.
fn apply_layout(state: &WindowState) {
    let hwnd = state.hwnd.get();
    let dpi = state.dpi.get();
    let (cw, ch) = client_size(hwnd);
    if cw <= 0 || ch <= 0 {
        return;
    }
    let tb_h = state.tb_height.get();
    let top = if state.tb_visible.get() { tb_h } else { 0 };
    let (vw, hh) = (metric(SM_CXVSCROLL, dpi), metric(SM_CYHSCROLL, dpi));
    let sb_h = scale(dpi, STATUS_HEIGHT);
    let hbar_h = if state.hbar_on.get() { hh } else { 0 };
    let (w, h) = ((cw - vw).max(0), (ch - top - sb_h - hbar_h).max(0));
    put(state.toolbar.get(), 0, 0, cw, tb_h, state.tb_visible.get());
    put(state.vbar.get(), w, top, vw, h, true);
    put(state.hbar.get(), 0, top + h, w, hh, state.hbar_on.get());
    put(state.status.get(), 0, ch - sb_h, cw, sb_h, true);
    set_status_parts(state, cw, dpi);
    place_tool_boxes(state, dpi);
    let now = (top, w, h);
    if state.content.get() != now {
        state.content.set(now);
        dispatch(state, Event::Size { w, h });
    }
}

/// The three parts of the status bar: the page, the note (what is left), the zoom.
fn set_status_parts(state: &WindowState, cw: i32, dpi: u32) {
    let bar = state.status.get();
    if bar == 0 {
        return;
    }
    let (left, right) = (scale(dpi, 200).min(cw / 3), scale(dpi, 90).min(cw / 3));
    let edges: [i32; 3] = [left, (cw - right).max(left), -1];
    // SAFETY: SB_SETPARTS takes the number of parts and a pointer to that many right edges, which `edges` is (the control copies them).
    unsafe { SendMessageW(bar, SB_SETPARTS, edges.len(), edges.as_ptr() as isize) };
}

fn set_status(state: &WindowState, st: &StatusText) {
    let bar = state.status.get();
    if bar == 0 {
        return;
    }
    // Two tabs right-align the text of the last part.
    let zoom = if st.zoom.is_empty() { String::new() } else { format!("\t\t{}", st.zoom) };
    for (part, text) in [&st.page, &st.note, &zoom].into_iter().enumerate() {
        let w = wide(text);
        // SAFETY: SB_SETTEXTW takes the part in the low word (drawn as it is) and a pointer to a zero-terminated UTF-16 string, which
        // `w` outlives (the control copies it).
        unsafe { SendMessageW(bar, SB_SETTEXTW, part, w.as_ptr() as isize) };
    }
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

/// The font the toolbar's pictures come from (looked for the first time it is asked).
fn icon_font(state: &WindowState) -> IconFont {
    if let Some(font) = state.icon_font.get() {
        return font;
    }
    // SAFETY: a null window gives a device context of the screen's kind; it is deleted below.
    let dc = unsafe { CreateCompatibleDC(0) };
    let font = if dc == 0 {
        IconFont::Words
    } else {
        let found = toolbar::pick_icon_font(has_font(dc, toolbar::FLUENT), has_font(dc, toolbar::MDL2));
        // SAFETY: the device context made above, not used again.
        unsafe { DeleteDC(dc) };
        found
    };
    state.icon_font.set(Some(font));
    font
}

/// An image list of the pictures of `glyphs` from the font `face`, `px` wide and high, in the colour `ink` (0xRRGGBB); 0 if it could
/// not be made. Each letter is drawn white on black with smooth edges on a 32-bit picture, and its brightness is made its opacity.
fn icon_list(face: &str, glyphs: &[char], px: i32, ink: Rgb) -> isize {
    if px <= 0 || glyphs.is_empty() {
        return 0;
    }
    // SAFETY: plain numbers; the list is checked, and destroyed by whoever replaces it.
    let list = unsafe { ImageList_Create(px, px, ILC_COLOR32, glyphs.len() as i32, 1) };
    if list == 0 {
        return 0;
    }
    // SAFETY: a null window gives a device context of the screen's kind; it is deleted below.
    let dc = unsafe { CreateCompatibleDC(0) };
    if dc == 0 {
        // SAFETY: the list made above, not used again.
        unsafe { ImageList_Destroy(list) };
        return 0;
    }
    let name = wide(face);
    // SAFETY: plain numbers and `name`, a zero-terminated UTF-16 string that outlives the call; the font is owned here and deleted below.
    let font = unsafe { CreateFontW(-px, 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET, 0, 0, ANTIALIASED_QUALITY, 0, name.as_ptr()) };
    // SAFETY: both handles are ours (a failed font is 0, and nothing is selected then).
    let old_font = if font != 0 { unsafe { SelectObject(dc, font) } } else { 0 };
    let (ink_r, ink_g, ink_b) = ((ink >> 16) & 0xFF, (ink >> 8) & 0xFF, ink & 0xFF);
    for &glyph in glyphs {
        let info = BitmapInfoHeader { size: std::mem::size_of::<BitmapInfoHeader>() as u32, width: px, height: -px, planes: 1, bit_count: 32, compression: 0, size_image: 0, x_pels: 0, y_pels: 0, clr_used: 0, clr_important: 0 };
        let mut bits: *mut c_void = null_mut();
        // SAFETY: `info` is a live header of a 32-bit top-down picture; `bits` is a live pointer that Windows fills with the address of
        // the picture's memory (px by px by 4 bytes, zeroed), which lives until the bitmap is deleted.
        let bitmap = unsafe { CreateDIBSection(dc, &info, 0, &mut bits, 0, 0) };
        if bitmap == 0 || bits.is_null() {
            if bitmap != 0 {
                // SAFETY: the bitmap made above, not selected anywhere.
                unsafe { DeleteObject(bitmap) };
            }
            continue;
        }
        let unit = [glyph as u16];
        let mut size = Size { cx: 0, cy: 0 };
        // SAFETY: the bitmap is selected into the DC only for the drawing and put out again before it is read or deleted; `unit` is a
        // live array of one character (its length is passed); `size` is filled in; GdiFlush makes sure the drawing is done.
        unsafe {
            let old = SelectObject(dc, bitmap);
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, 0x00FF_FFFF);
            GetTextExtentPoint32W(dc, unit.as_ptr(), 1, &mut size);
            TextOutW(dc, (px - size.cx) / 2, (px - size.cy) / 2, unit.as_ptr(), 1);
            GdiFlush();
            SelectObject(dc, old);
        }
        // SAFETY: `bits` points to px * px * 4 bytes of the bitmap's memory, which stay until the DeleteObject below; the bitmap is out of
        // the DC and its drawing is flushed, so nothing else uses them.
        let pixels = unsafe { std::slice::from_raw_parts_mut(bits.cast::<u8>(), (px as usize) * (px as usize) * 4) };
        for [b, g, r, a] in pixels.as_chunks_mut::<4>().0 {
            let alpha = u32::from((*b).max(*g).max(*r));
            // The picture is kept premultiplied: the colour is scaled by the opacity.
            *b = (ink_b * alpha / 255) as u8;
            *g = (ink_g * alpha / 255) as u8;
            *r = (ink_r * alpha / 255) as u8;
            *a = alpha as u8;
        }
        // SAFETY: the list and the bitmap are ours; the list copies the picture, and the bitmap is deleted after.
        unsafe {
            ImageList_Add(list, bitmap, 0);
            DeleteObject(bitmap);
        }
    }
    // SAFETY: the font is put out of the DC before it is deleted, and the DC after; all three are ours and not used again.
    unsafe {
        if old_font != 0 {
            SelectObject(dc, old_font);
        }
        if font != 0 {
            DeleteObject(font);
        }
        DeleteDC(dc);
    }
    list
}

fn make_long(low: i32, high: i32) -> isize {
    (((high as u32) << 16) | (low as u32 & 0xFFFF)) as isize
}

fn separator(width: i32) -> TbButton {
    TbButton { bitmap: width, command: 0, state: TBSTATE_ENABLED, style: BTNS_SEP, reserved: [0; 6], data: 0, string: 0 }
}

/// Make the toolbar's buttons from `tb_spec`: the pictures are drawn at the size the screen asks for (or the buttons say their names
/// when there is no icon font), and the boxes of the page and the search get a gap of their own in the row.
fn rebuild_toolbar(state: &WindowState) {
    let bar = state.toolbar.get();
    let spec_ref = state.tb_spec.borrow();
    let Some(spec) = spec_ref.as_ref() else { return };
    if bar == 0 {
        return;
    }
    let dpi = state.dpi.get();
    // SAFETY: plain messages to the toolbar: the size of its button structure is told first, then every button is taken out.
    unsafe {
        SendMessageW(bar, TB_BUTTONSTRUCTSIZE, std::mem::size_of::<TbButton>(), 0);
        for _ in 0..SendMessageW(bar, TB_BUTTONCOUNT, 0, 0).max(0) {
            SendMessageW(bar, TB_DELETEBUTTON, 0, 0);
        }
    }
    let glyphs: Vec<char> = spec.items.iter().filter_map(|i| if let ToolItem::Button { glyph, .. } = i { Some(*glyph) } else { None }).collect();
    let px = scale(dpi, TOOL_ICON).max(8);
    let (mut normal, mut grey) = match icon_font(state).face() {
        Some(face) => (icon_list(face, &glyphs, px, ICON_INK), icon_list(face, &glyphs, px, ICON_GREY)),
        None => (0, 0),
    };
    if normal == 0 || grey == 0 {
        // Both or neither: a button greyed with no picture of its own would show the other kind.
        for list in [normal, grey].into_iter().filter(|&l| l != 0) {
            // SAFETY: a list made just above and not given to anyone.
            unsafe { ImageList_Destroy(list) };
        }
        (normal, grey) = (0, 0);
    }
    let words = normal == 0;
    // SAFETY: TB_SETIMAGELIST and TB_SETDISABLEDIMAGELIST take the id of a set of lists (0) and a list (or 0 for none) and answer with the
    // list that was there; those are ours and nobody uses them now, so they are destroyed. The new lists are kept to be destroyed at the end.
    unsafe {
        let old_normal = SendMessageW(bar, TB_SETIMAGELIST, 0, normal);
        let old_grey = SendMessageW(bar, TB_SETDISABLEDIMAGELIST, 0, grey);
        for list in [old_normal, old_grey].into_iter().filter(|&l| l != 0) {
            ImageList_Destroy(list);
        }
    }
    state.tb_lists.set((normal, grey));
    // Without pictures the names go into the toolbar's string pool, and a button points at its name by its number.
    let mut first_string = 0;
    if words {
        let mut pool: Vec<u16> = Vec::new();
        for item in &spec.items {
            if let ToolItem::Button { label, .. } = item {
                pool.extend(label.encode_utf16().filter(|&c| c != 0));
                pool.push(0);
            }
        }
        pool.push(0);
        // SAFETY: TB_ADDSTRINGW with no module takes a pointer to strings that each end with a zero and the last with two; `pool` is that and
        // outlives the call (the toolbar copies it). The answer is the number of the first string.
        first_string = unsafe { SendMessageW(bar, WM_USER + 77, 0, pool.as_ptr() as isize) };
    }
    let (gap, mut image, mut name) = (scale(dpi, TOOL_GAP), 0, first_string);
    let mut boxes = (-1, -1);
    let mut buttons: Vec<TbButton> = Vec::new();
    for item in &spec.items {
        let index = buttons.len() as i32;
        match item {
            ToolItem::Button { cmd, toggle, .. } => {
                let mut style = if *toggle { BTNS_CHECK } else { 0 };
                let (bitmap, string) = if words {
                    style |= BTNS_SHOWTEXT | BTNS_AUTOSIZE;
                    name += 1;
                    (I_IMAGENONE, name - 1)
                } else {
                    image += 1;
                    (image - 1, 0)
                };
                buttons.push(TbButton { bitmap, command: *cmd as i32, state: TBSTATE_ENABLED, style, reserved: [0; 6], data: 0, string });
            }
            ToolItem::Separator => buttons.push(separator(gap)),
            // The place of a box is a button that has no picture, is greyed and is as wide as the box (a wide separator would draw a
            // stray mark at its top); its width is set below, once it is in the toolbar.
            ToolItem::PageBox => {
                boxes.0 = index;
                buttons.push(TbButton { bitmap: I_IMAGENONE, command: BOX_COMMAND, state: 0, style: BTNS_AUTOSIZE, reserved: [0; 6], data: 0, string: 0 });
            }
            ToolItem::SearchBox { .. } => {
                boxes.1 = index;
                buttons.push(TbButton { bitmap: I_IMAGENONE, command: BOX_COMMAND + 1, state: 0, style: BTNS_AUTOSIZE, reserved: [0; 6], data: 0, string: 0 });
            }
        }
    }
    state.tb_boxes.set(boxes);
    // SAFETY: TB_ADDBUTTONSW takes the number of buttons and a pointer to that many TBBUTTON structures, which `buttons` is (the toolbar
    // copies them).
    unsafe {
        SendMessageW(bar, TB_ADDBUTTONSW, buttons.len(), buttons.as_ptr() as isize);
    }
    for (command, width) in [(BOX_COMMAND, PAGE_BOX_W), (BOX_COMMAND + 1, SEARCH_BOX_W)] {
        let info = TbButtonInfo { size: std::mem::size_of::<TbButtonInfo>() as u32, mask: TBIF_SIZE, command: 0, image: 0, state: 0, style: 0, width: scale(dpi, width) as u16, param: 0, text: null_mut(), text_max: 0 };
        // SAFETY: TB_SETBUTTONINFOW takes the command of a button and a pointer to a live TBBUTTONINFOW of the size it declares, here
        // setting only the width; a command the toolbar has no button for (a box that is not in this toolbar) is not an error.
        unsafe { SendMessageW(bar, TB_SETBUTTONINFOW, command as usize, (&raw const info) as isize) };
    }
    // SAFETY: plain messages to the toolbar: the size of a button is the picture and the room round it (TB_SETBUTTONSIZE takes the width
    // in the low word and the height in the high); TB_AUTOSIZE lets the toolbar settle its own size.
    unsafe {
        if !words {
            SendMessageW(bar, TB_SETBUTTONSIZE, 0, make_long(px + scale(dpi, 16), px + scale(dpi, 14)));
        }
        SendMessageW(bar, TB_AUTOSIZE, 0, 0);
    }
    // SAFETY: asks the toolbar for the size of a button: width in the low word, height in the high.
    let size = unsafe { SendMessageW(bar, TB_GETBUTTONSIZE, 0, 0) } as usize;
    state.tb_height.set(hiword(size) as i32 + scale(dpi, 6));
    drop(spec_ref);
    apply_toolbar_state(state);
}

/// Grey or press the buttons as the reader says, enable the boxes, and put the page in its box (unless it is being typed in).
fn apply_toolbar_state(state: &WindowState) {
    let bar = state.toolbar.get();
    let st = state.tb_state.borrow();
    if bar != 0 {
        for &(cmd, enabled, checked) in &st.buttons {
            // SAFETY: plain messages to the toolbar: the button of this command is enabled or not, pressed or not (a command it has no
            // button for is not an error).
            unsafe {
                SendMessageW(bar, TB_ENABLEBUTTON, cmd as usize, isize::from(enabled));
                SendMessageW(bar, TB_CHECKBUTTON, cmd as usize, isize::from(checked));
            }
        }
    }
    for edit in [state.tb_page.get(), state.tb_search.get()] {
        if edit != 0 {
            // SAFETY: plain handle of a box of ours.
            unsafe { EnableWindow(edit, i32::from(st.boxes_enabled)) };
        }
    }
    let page = state.tb_page.get();
    // SAFETY: reads which window has the keyboard.
    if page != 0 && unsafe { GetFocus() } != page {
        set_text(page, &st.page_text);
    }
}

/// Lay the page box and the search box over their gaps in the toolbar's row.
fn place_tool_boxes(state: &WindowState, dpi: u32) {
    let bar = state.toolbar.get();
    let show = bar != 0 && state.tb_visible.get();
    let (page_index, search_index) = state.tb_boxes.get();
    let h = scale(dpi, BOX_H);
    let (y, inset) = (((state.tb_height.get() - h) / 2).max(0), scale(dpi, 2));
    for (edit, index) in [(state.tb_page.get(), page_index), (state.tb_search.get(), search_index)] {
        if edit == 0 {
            continue;
        }
        let mut r = WinRect::default();
        // SAFETY: TB_GETITEMRECT takes a button's number and a pointer to a live RECT, which it fills in (a number it has no button for
        // fails and leaves the RECT zero).
        let found = show && index >= 0 && unsafe { SendMessageW(bar, TB_GETITEMRECT, index as usize, (&raw mut r) as isize) } != 0;
        if found {
            put(edit, r.left + inset, y, r.right - r.left - 2 * inset, h, true);
        } else {
            put(edit, 0, 0, 0, 0, false);
        }
    }
}

/// Take the toolbar the reader describes (its buttons, its words, whether it is shown).
fn set_toolbar(state: &WindowState, spec: ToolbarSpec) {
    state.tb_visible.set(spec.visible && state.toolbar.get() != 0);
    if let Some(ToolItem::SearchBox { hint }) = spec.items.iter().find(|i| matches!(i, ToolItem::SearchBox { .. })) {
        let w = wide(hint);
        // SAFETY: EM_SETCUEBANNER takes a flag (0: the words go once the box has the keyboard) and a zero-terminated UTF-16 string that
        // outlives the call (the box copies it).
        unsafe { SendMessageW(state.tb_search.get(), EM_SETCUEBANNER, 0, w.as_ptr() as isize) };
    }
    *state.tb_spec.borrow_mut() = Some(spec);
    // A toolbar that is not shown is not drawn (the speed test has none); it is made when it is asked to be shown.
    if state.tb_visible.get() {
        rebuild_toolbar(state);
    }
    apply_layout(state);
}

/// The ToolTip control asks for the words of a tip: the toolbar button of that command has them in the toolbar's description.
fn tooltip_text(state: &WindowState, info: &mut NmTtDispInfo) {
    fill_tip(state.tb_spec.borrow().as_ref(), info);
}

/// Put the tip of the button whose command is `info.hdr.id` into the ToolTip's request (nothing if there is no such button).
fn fill_tip(spec: Option<&ToolbarSpec>, info: &mut NmTtDispInfo) {
    // When the tool is named by a window (flag 1) and not by a command, it is not one of ours.
    if info.flags & 1 != 0 {
        return;
    }
    let cmd = info.hdr.id as u32;
    let tip = spec.and_then(|s| {
        s.items.iter().find_map(|i| match i {
            ToolItem::Button { cmd: c, tip, .. } if *c == cmd => Some(tip),
            _ => None,
        })
    });
    let Some(tip) = tip else { return };
    info.buf = [0; 80];
    for (to, unit) in info.buf.iter_mut().take(79).zip(tip.encode_utf16()) {
        *to = unit;
    }
    info.text = info.buf.as_mut_ptr();
}

/// Remember where the window is while it is neither maximized nor minimized (that is the place that is kept for the next run).
fn remember_normal(state: &WindowState) {
    let hwnd = state.hwnd.get();
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

/// The edit box that is the find box: the toolbar's when the toolbar is shown, else the one over the pages.
fn find_edit(state: &WindowState) -> isize {
    if state.tb_visible.get() && state.tb_search.get() != 0 { state.tb_search.get() } else { state.find.get() }
}

// --- starting up -------------------------------------------------------------------------------------------------

/// Make the window and run it until it is closed. `make` is given the means to wake the window from another thread
/// and builds the reader. `client`: a size for the window's client area, in pixels (the speed test uses it; it may be bigger
/// than the screen); otherwise the window is most of the work area, or where it was (`saved`, put back where it can be seen). With
/// `remember` the window's place is kept for the next run when it is closed.
pub fn run(title: &str, menus: &[Menu], client: Option<(i32, i32)>, saved: Option<Placement>, remember: bool, make: impl FnOnce(Waker) -> Box<dyn Handler>) -> i32 {
    let icc = InitCommonControlsEx { size: std::mem::size_of::<InitCommonControlsEx>() as u32, icc: ICC_STANDARD_CLASSES | ICC_TREEVIEW_CLASSES | ICC_BAR_CLASSES };
    // SAFETY: `icc` is a live struct of the size it declares; this makes the buttons and boxes use the new look.
    unsafe { InitCommonControlsEx(&icc) };
    // SAFETY: the print box needs COM on its thread (a single-threaded apartment); a failure only means the box may not open.
    unsafe { CoInitializeEx(null(), COINIT_APARTMENTTHREADED) };
    let class = wide("QingpdfView");
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
    // The scroll bars are controls of their own (see `apply_layout`), not the window's.
    let style = WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN;
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
        dpi: Cell::new(96),
        tree: Cell::new(0),
        find: Cell::new(0),
        field_single: Cell::new(0),
        field_multi: Cell::new(0),
        field_active: Cell::new(0),
        printer: RefCell::new(None),
        menu_open: Cell::new(false),
        menus_waiting: RefCell::new(None),
        toolbar: Cell::new(0),
        tb_page: Cell::new(0),
        tb_search: Cell::new(0),
        status: Cell::new(0),
        vbar: Cell::new(0),
        hbar: Cell::new(0),
        tb_spec: RefCell::new(None),
        tb_state: RefCell::new(ToolbarState::default()),
        tb_visible: Cell::new(false),
        tb_height: Cell::new(0),
        tb_boxes: Cell::new((-1, -1)),
        tb_lists: Cell::new((0, 0)),
        icon_font: Cell::new(None),
        hbar_on: Cell::new(false),
        content: Cell::new((-1, -1, -1)),
        normal: Cell::new(None),
        remember_window: remember && client.is_none(),
        clicks: RefCell::new(ClickCounter::default()),
    });
    let raw = Box::into_raw(state);
    // SAFETY: stores the pointer from `Box::into_raw`; it is freed once, by `free_state` below, after the message loop.
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, raw as isize) };
    let Some(state) = state_of(hwnd) else { return 1 };
    // The bookmarks tree and the find box: children of the window, hidden until the reader says where to put them.
    state.tree.set(make_child(hwnd, instance, "SysTreeView32", WS_EX_CLIENTEDGE, WS_TABSTOP | TVS_HASBUTTONS | TVS_HASLINES | TVS_LINESATROOT | TVS_SHOWSELALWAYS, ID_TREE));
    state.find.set(make_child(hwnd, instance, "EDIT", WS_EX_CLIENTEDGE, WS_TABSTOP | ES_AUTOHSCROLL, ID_FIND));
    state.field_single.set(make_child(hwnd, instance, "EDIT", WS_EX_CLIENTEDGE, WS_TABSTOP | ES_AUTOHSCROLL, ID_FIELD1));
    state.field_multi.set(make_child(hwnd, instance, "EDIT", WS_EX_CLIENTEDGE, WS_TABSTOP | ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN | WS_VSCROLL, ID_FIELD2));
    // The toolbar (and, made after it so that they lie over it, the boxes of the page and the search), the status bar and the two scroll
    // bars. The toolbar and the status bar are put where they go by `apply_layout`, not by Windows.
    state.toolbar.set(make_child(hwnd, instance, "ToolbarWindow32", 0, TBSTYLE_TOOLTIPS | TBSTYLE_FLAT | CCS_NODIVIDER | CCS_NORESIZE | CCS_NOPARENTALIGN, ID_TOOLBAR));
    state.tb_page.set(make_child(hwnd, instance, "EDIT", WS_EX_CLIENTEDGE, WS_TABSTOP | ES_AUTOHSCROLL | ES_CENTER, ID_PAGE_BOX));
    state.tb_search.set(make_child(hwnd, instance, "EDIT", WS_EX_CLIENTEDGE, WS_TABSTOP | ES_AUTOHSCROLL, ID_SEARCH_BOX));
    state.status.set(make_child(hwnd, instance, "msctls_statusbar32", 0, CCS_NORESIZE | CCS_NOPARENTALIGN, ID_STATUS));
    state.vbar.set(make_child(hwnd, instance, "SCROLLBAR", 0, SBS_VERT, ID_VBAR));
    state.hbar.set(make_child(hwnd, instance, "SCROLLBAR", 0, SBS_HORZ, ID_HBAR));

    if let Some((cw, ch)) = client {
        // The page area is to be as wide as asked (the vertical scroll bar is beside it) and as tall as asked less the status bar, which
        // is what the speed test has always measured; it has no toolbar. The window makes room for the rest.
        let (cw, ch) = (cw + metric(SM_CXVSCROLL, system_dpi), ch);
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
    set_icons(hwnd, dpi);
    new_dpi(state, dpi);
    dispatch(state, Event::Dpi(dpi));
    // The reader has not heard of the size yet (what was told before it was made is lost): it is told now.
    state.content.set((-1, -1, -1));
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
    // The page box of the toolbar: Enter goes to the page typed, Esc gives up what was typed; either gives the keyboard to the pages.
    let page = state.tb_page.get();
    if msg.hwnd != 0 && msg.hwnd == page {
        let exit = match msg.wparam as u32 {
            0x0D => {
                dispatch(state, Event::PageBox(window_text(page, 32)));
                true
            }
            0x1B => true,
            _ => false,
        };
        if exit {
            // SAFETY: plain handle of the main window.
            unsafe { SetFocus(state.hwnd.get()) };
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
    for list in [state.tb_lists.get().0, state.tb_lists.get().1] {
        if list != 0 {
            // SAFETY: the lists were made by `icon_list`; the toolbar that used them is gone with its window (or is no longer asked for them).
            unsafe { ImageList_Destroy(list) };
        }
    }
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
    use crate::i18n::Lang;

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn the_structures_have_the_layout_windows_expects() {
        // The 64-bit sizes of TBBUTTON, NMHDR, NMTTDISPINFOW and MONITORINFO (commctrl.h, winuser.h).
        assert_eq!(std::mem::size_of::<TbButton>(), 32);
        assert_eq!(std::mem::size_of::<NmHdr>(), 24);
        assert_eq!(std::mem::size_of::<NmTtDispInfo>(), 216);
        assert_eq!(std::mem::size_of::<MonitorInfo>(), 40);
        assert_eq!(std::mem::size_of::<TbButtonInfo>(), 48);
        assert_eq!(std::mem::offset_of!(NmTtDispInfo, buf), 32);
        assert_eq!(std::mem::offset_of!(TbButton, string), 24);
    }

    fn request(cmd: u32) -> NmTtDispInfo {
        NmTtDispInfo { hdr: NmHdr { from: 0, id: cmd as usize, code: TTN_GETDISPINFOW }, text: null_mut(), buf: [0; 80], instance: 0, flags: 0, param: 0 }
    }

    #[test]
    fn a_tooltip_asked_for_by_its_command_gets_the_words_of_its_button() {
        let spec = toolbar::spec(Lang::Zh, true);
        let mut info = request(crate::app::CMD_PRINT);
        fill_tip(Some(&spec), &mut info);
        let end = info.buf.iter().position(|&u| u == 0).unwrap();
        assert_eq!(String::from_utf16(&info.buf[..end]).unwrap(), "打印 (Ctrl+P)");
        assert_eq!(info.text, info.buf.as_mut_ptr());
        // A command with no button, a tip named by a window, and no toolbar: nothing is written.
        for (cmd, flags, has_spec) in [(9999, 0, true), (crate::app::CMD_PRINT, 1, true), (crate::app::CMD_PRINT, 0, false)] {
            let mut info = request(cmd);
            info.flags = flags;
            fill_tip(has_spec.then_some(&spec), &mut info);
            assert!(info.text.is_null() && info.buf.iter().all(|&u| u == 0), "{cmd}");
        }
    }
}
