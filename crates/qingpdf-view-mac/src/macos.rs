//! The macOS side of the reader: the counterpart of `win32.rs` (first milestone, 2026-11-03). One NSWindow with a view of our own,
//! a back buffer that is a CGBitmapContext (in pixels, like the Windows one: the window's size is told in pixels and the backing scale as
//! a dpi), text drawn with CoreText. No crates: the Objective-C runtime and the frameworks are declared here by hand, as `win32.rs`
//! declares Windows. `unsafe` is allowed in this module only (and in `win32.rs`).
//!
//! All of it runs on the main thread, except [`Waker::wake`] (any thread). The reader is never called while it is already running:
//! events that arrive meanwhile (a nested modal box, say) wait in a queue ([`dispatch`]).
//!
//! The Objective-C calls go through `sendN`, which are safe wrappers over `objc_msgSend`. Their contract: the receiver is nil or a live
//! object (always one got from AppKit or made here), and the selector and the argument and result types are written together at the
//! call, to fit the method (which is what the Apple headers say). Nothing is ever sent to an object after it has been released.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::ffi::{CStr, c_char, c_void};
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use qingpdf_view::ocr::{GrayImage, OcrError, Recognizer};
use qingpdf_view::placement::{self, Placement};
use qingpdf_core::bookmarks::Place;
use qingpdf_core::view::{MAX_WORDS_PER_PAGE, OcrLine, OcrWord};
use qingpdf_view::theme::Contrast;
use qingpdf_view::ui::*;

// --- the declarations ------------------------------------------------------------------------------------------------

type Id = *mut c_void;
type Sel = *const c_void;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CgSize {
    w: f64,
    h: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CgPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CgRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> Id;
    fn sel_registerName(name: *const c_char) -> Sel;
    fn objc_msgSend();
    fn objc_allocateClassPair(superclass: Id, name: *const c_char, extra: usize) -> Id;
    fn objc_registerClassPair(class: Id);
    fn class_addMethod(class: Id, sel: Sel, imp: *const c_void, types: *const c_char) -> bool;
    fn objc_autoreleasePoolPush() -> *mut c_void;
    fn class_getMethodImplementation(class: Id, sel: Sel) -> *const c_void;
    fn sel_getName(sel: Sel) -> *const c_char;
    fn objc_autoreleasePoolPop(pool: *mut c_void);
}

#[cfg(target_arch = "x86_64")]
#[link(name = "objc")]
unsafe extern "C" {
    fn objc_msgSend_stret();
}

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {
    static NSPasteboardTypeString: Id;
    static NSFontAttributeName: Id;
    static NSForegroundColorAttributeName: Id;
}

#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {
    static NSRunLoopCommonModes: Id;
    fn NSFullUserName() -> Id;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFTypeArrayCallBacks: [u8; 0];
    static kCFTypeDictionaryKeyCallBacks: [u8; 0];
    static kCFTypeDictionaryValueCallBacks: [u8; 0];
    static kCFBooleanTrue: Id;
    fn CFRelease(object: Id);
    fn CFRetain(object: Id) -> Id;
    fn CFStringCreateWithBytes(alloc: Id, bytes: *const u8, len: isize, encoding: u32, external: bool) -> Id;
    fn CFArrayCreate(alloc: Id, values: *const Id, count: isize, callbacks: *const c_void) -> Id;
    fn CFArrayGetCount(array: Id) -> isize;
    fn CFArrayGetValueAtIndex(array: Id, index: isize) -> Id;
    fn CFDictionaryCreate(alloc: Id, keys: *const Id, values: *const Id, count: isize, key_cb: *const c_void, value_cb: *const c_void) -> Id;
    fn CFAttributedStringCreate(alloc: Id, string: Id, attributes: Id) -> Id;
    fn CFLocaleCopyPreferredLanguages() -> Id;
    fn CFDataCreate(alloc: Id, bytes: *const u8, len: isize) -> Id;
    fn CFURLCreateFromFileSystemRepresentation(alloc: Id, bytes: *const u8, len: isize, directory: bool) -> Id;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    static kCGColorSpaceSRGB: Id;
    fn CGColorSpaceCreateWithName(name: Id) -> Id;
    fn CGColorSpaceRelease(space: Id);
    fn CGBitmapContextCreate(data: *mut c_void, w: usize, h: usize, bpc: usize, bpr: usize, space: Id, info: u32) -> Id;
    fn CGBitmapContextCreateImage(ctx: Id) -> Id;
    fn CGContextRelease(ctx: Id);
    fn CGImageRelease(image: Id);
    fn CGContextSetRGBFillColor(ctx: Id, r: f64, g: f64, b: f64, a: f64);
    fn CGContextSetRGBStrokeColor(ctx: Id, r: f64, g: f64, b: f64, a: f64);
    fn CGContextFillRect(ctx: Id, rect: CgRect);
    fn CGContextBeginPath(ctx: Id);
    fn CGContextMoveToPoint(ctx: Id, x: f64, y: f64);
    fn CGContextAddLineToPoint(ctx: Id, x: f64, y: f64);
    fn CGContextAddArcToPoint(ctx: Id, x1: f64, y1: f64, x2: f64, y2: f64, radius: f64);
    fn CGContextClosePath(ctx: Id);
    fn CGContextFillPath(ctx: Id);
    fn CGContextStrokePath(ctx: Id);
    fn CGContextSetLineWidth(ctx: Id, width: f64);
    fn CGContextSetLineCap(ctx: Id, cap: i32);
    fn CGContextSetLineJoin(ctx: Id, join: i32);
    fn CGContextSetInterpolationQuality(ctx: Id, quality: i32);
    fn CGContextDrawImage(ctx: Id, rect: CgRect, image: Id);
    fn CGContextSetTextPosition(ctx: Id, x: f64, y: f64);
    fn CGContextSaveGState(ctx: Id);
    fn CGContextRestoreGState(ctx: Id);
    fn CGContextClipToMask(ctx: Id, rect: CgRect, mask: Id);
    fn CGDataProviderCreateWithData(info: *mut c_void, data: *const c_void, size: usize, release: *const c_void) -> Id;
    fn CGDataProviderRelease(provider: Id);
    fn CGDataProviderCreateWithCFData(data: Id) -> Id;
    fn CGColorSpaceCreateDeviceGray() -> Id;
    fn CGPDFContextCreateWithURL(url: Id, media: *const CgRect, info: Id) -> Id;
    fn CGPDFContextBeginPage(ctx: Id, info: Id);
    fn CGPDFContextEndPage(ctx: Id);
    fn CGPDFContextClose(ctx: Id);
    fn CGImageCreate(
        w: usize,
        h: usize,
        bpc: usize,
        bpp: usize,
        bpr: usize,
        space: Id,
        info: u32,
        provider: Id,
        decode: *const f64,
        interpolate: bool,
        intent: i32,
    ) -> Id;
}

#[link(name = "CoreText", kind = "framework")]
unsafe extern "C" {
    static kCTFontAttributeName: Id;
    static kCTForegroundColorFromContextAttributeName: Id;
    fn CTFontCreateUIFontForLanguage(kind: u32, size: f64, language: Id) -> Id;
    fn CTFontGetAscent(font: Id) -> f64;
    fn CTFontGetDescent(font: Id) -> f64;
    fn CTFontGetLeading(font: Id) -> f64;
    fn CTLineCreateWithAttributedString(string: Id) -> Id;
    fn CTLineDraw(line: Id, ctx: Id);
    fn CTLineGetTypographicBounds(line: Id, ascent: *mut f64, descent: *mut f64, leading: *mut f64) -> f64;
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn PMSessionGetCurrentPrinter(session: *mut c_void, printer: *mut *mut c_void) -> i32;
    fn PMSetFirstPage(settings: *mut c_void, first: u32, lock: bool) -> i32;
    fn PMSetLastPage(settings: *mut c_void, last: u32, lock: bool) -> i32;
    fn PMPrinterPrintWithFile(printer: *mut c_void, settings: *mut c_void, format: *mut c_void, mime: Id, url: Id) -> i32;
}

#[link(name = "Vision", kind = "framework")]
unsafe extern "C" {}

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecRandomCopyBytes(random: *const c_void, count: usize, bytes: *mut u8) -> i32;
}

#[repr(C)]
struct Tm {
    sec: i32,
    min: i32,
    hour: i32,
    mday: i32,
    mon: i32,
    year: i32,
    wday: i32,
    yday: i32,
    isdst: i32,
    gmtoff: i64,
    zone: *const c_char,
}

unsafe extern "C" {
    fn time(t: *mut i64) -> i64;
    fn localtime_r(t: *const i64, out: *mut Tm) -> *mut Tm;
    fn pthread_main_np() -> i32;
    fn getpid() -> i32;
    fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut c_void) -> i32;
}

macro_rules! sender {
    ($name:ident; $(($t:ident, $a:ident)),*) => {
        #[allow(clippy::too_many_arguments)]
        fn $name<R $(, $t)*>(obj: Id, sel: &CStr $(, $a: $t)*) -> R {
            // SAFETY: see the head of this module: a nil or live receiver and a selector written with the types of its method; `objc_msgSend` is
            // the entry point of every message and is called through the function type that the method has.
            unsafe {
                let f = std::mem::transmute::<*const (), unsafe extern "C" fn(Id, Sel $(, $t)*) -> R>(objc_msgSend as *const ());
                f(obj, sel_registerName(sel.as_ptr()) $(, $a)*)
            }
        }
    };
}
sender!(send0;);
sender!(send1; (A, a));
sender!(send2; (A, a), (B, b));
sender!(send3; (A, a), (B, b), (C, c));
sender!(send4; (A, a), (B, b), (C, c), (D, d));
sender!(send5; (A, a), (B, b), (C, c), (D, d), (E, e));
sender!(send9; (A, a), (B, b), (C, c), (D, d), (E, e), (F, f), (G, g), (H, h), (I, i));

/// A message whose answer is a rectangle (a structure too big for the registers on Intel, which has its own entry point for it).
fn send_rect(obj: Id, sel: &CStr) -> CgRect {
    #[cfg(target_arch = "x86_64")]
    {
        let mut out = CgRect::default();
        // SAFETY: as for `sender`; `objc_msgSend_stret` takes the place the answer is written to first, and `out` is live through the call.
        unsafe {
            let f = std::mem::transmute::<*const (), unsafe extern "C" fn(*mut CgRect, Id, Sel)>(objc_msgSend_stret as *const ());
            f(&mut out, obj, sel_registerName(sel.as_ptr()));
        }
        out
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        send0(obj, sel)
    }
}

fn sel(name: &CStr) -> Sel {
    // SAFETY: the name is a zero-terminated string that lives through the call; the runtime copies what it keeps.
    unsafe { sel_registerName(name.as_ptr()) }
}

fn class(name: &CStr) -> Id {
    // SAFETY: the name is a zero-terminated string that lives through the call.
    unsafe { objc_getClass(name.as_ptr()) }
}

/// `[[Class alloc] init]` (the object is ours: one reference).
fn new_object(name: &CStr) -> Id {
    let allocated: Id = send0(class(name), c"alloc");
    send0(allocated, c"init")
}

fn release(object: Id) {
    if !object.is_null() {
        // SAFETY: an object we hold a reference to and do not use afterwards.
        unsafe { CFRelease(object) };
    }
}

/// A Core Foundation object that is released when it goes.
struct Cf(Id);

impl Drop for Cf {
    fn drop(&mut self) {
        release(self.0);
    }
}

/// A CFString (which is an NSString as well).
fn cfstr(s: &str) -> Cf {
    // SAFETY: the bytes are live and valid UTF-8 and are copied; a null allocator is the default one.
    Cf(unsafe { CFStringCreateWithBytes(null_mut(), s.as_ptr(), s.len() as isize, 0x0800_0100, false) })
}

fn cf_array(items: &[Id]) -> Cf {
    // SAFETY: `items` are live objects, which the array keeps (the standard callbacks retain them).
    Cf(unsafe { CFArrayCreate(null_mut(), items.as_ptr(), items.len() as isize, kCFTypeArrayCallBacks.as_ptr().cast()) })
}

fn cf_array_of_strings(items: &[String]) -> Cf {
    let strings: Vec<Cf> = items.iter().map(|s| cfstr(s)).collect();
    let ids: Vec<Id> = strings.iter().map(|s| s.0).collect();
    cf_array(&ids)
}

/// The Rust string of an NSString (empty for nil).
fn rust_string(ns: Id) -> String {
    if ns.is_null() {
        return String::new();
    }
    let p: *const c_char = send0(ns, c"UTF8String");
    if p.is_null() {
        return String::new();
    }
    // SAFETY: `UTF8String` is a zero-terminated string that lives as long as the NSString, which is live here.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

fn array_items(array: Id) -> Vec<Id> {
    if array.is_null() {
        return Vec::new();
    }
    // SAFETY: a live CFArray (an NSArray is one); indexes are held to its count.
    let count = unsafe { CFArrayGetCount(array) };
    // SAFETY: as above, `i` is below the count.
    (0..count).map(|i| unsafe { CFArrayGetValueAtIndex(array, i) }).collect()
}

fn rgb_f(color: Rgb) -> (f64, f64, f64) {
    (f64::from((color >> 16) & 0xFF) / 255.0, f64::from((color >> 8) & 0xFF) / 255.0, f64::from(color & 0xFF) / 255.0)
}

// --- the keys --------------------------------------------------------------------------------------------------------

/// The Windows virtual-key code for a macOS key code (the keys the reader knows; letters and digits as on a US keyboard).
fn vk_of(code: u16) -> Option<u32> {
    Some(match code {
        0 => vk::A,
        1 => vk::S,
        2 => vk::D,
        3 => vk::F,
        4 => 0x48,
        5 => vk::G,
        6 => vk::Z,
        7 => 0x58,
        8 => vk::C,
        9 => 0x56,
        11 => vk::B,
        12 => 0x51,
        13 => vk::W,
        14 => 0x45,
        15 => vk::R,
        16 => vk::Y,
        17 => 0x54,
        18 => vk::NUM1,
        19 => vk::NUM2,
        20 => 0x33,
        21 => 0x34,
        22 => 0x36,
        23 => 0x35,
        24 => vk::OEM_PLUS,
        25 => 0x39,
        26 => 0x37,
        27 => vk::OEM_MINUS,
        28 => 0x38,
        29 => vk::NUM0,
        30 => vk::OEM_6,
        31 => vk::O,
        32 => 0x55,
        33 => vk::OEM_4,
        34 => 0x49,
        35 => vk::P,
        36 | 76 => vk::RETURN,
        37 => 0x4C,
        38 => 0x4A,
        40 => 0x4B,
        45 => 0x4E,
        46 => 0x4D,
        48 => vk::TAB,
        49 => vk::SPACE,
        51 => vk::BACK,
        53 => vk::ESCAPE,
        69 => vk::ADD,
        78 => vk::SUBTRACT,
        82 => vk::NUMPAD0,
        83..=89 => vk::NUMPAD0 + u32::from(code - 82),
        91 => vk::NUMPAD0 + 8,
        92 => vk::NUMPAD0 + 9,
        96 => vk::F5,
        99 => vk::F3,
        103 => vk::F11,
        115 => vk::HOME,
        116 => vk::PRIOR,
        117 => vk::DELETE,
        118 => vk::F4,
        119 => vk::END,
        120 => vk::F2,
        121 => vk::NEXT,
        123 => vk::LEFT,
        124 => vk::RIGHT,
        125 => vk::DOWN,
        126 => vk::UP,
        _ => return None,
    })
}

// --- the state -------------------------------------------------------------------------------------------------------

/// The back buffer: pixels of the window, BGRA premultiplied, drawn into through a bitmap context.
struct Buffer {
    ctx: Id,
    w: i32,
    h: i32,
    /// The pixels the context draws into (it must outlive the context, which is released first, in `drop`).
    _data: Vec<u8>,
}

impl Drop for Buffer {
    fn drop(&mut self) {
        // SAFETY: a context made here and released once.
        unsafe { CGContextRelease(self.ctx) };
    }
}

#[derive(Default)]
struct Mac {
    window: usize,
    view: usize,
    delegate: usize,
    space: usize,
    scale: f64,
    /// The view's size in points.
    pts: (f64, f64),
    told_dpi: u32,
    buffer: Option<Buffer>,
    timers: HashMap<u32, usize>,
    /// The text boxes over the pages (made when first needed): the find box, the page pill's, and the ones of a form field (one line, several).
    search: usize,
    pill: usize,
    field_single: usize,
    field_multi: usize,
    field_class: usize,
    /// The box of a form field now in use (0: none) and its most characters.
    field_active: usize,
    field_max: usize,
    pill_open: bool,
    search_on: bool,
    /// The colours of the reader's theme: the words, the ground of a box typed in.
    theme: Option<(Rgb, Rgb)>,
    /// The bookmarks tree: its scroll view, the outline view in it and its one column (0: not made yet), and its colours (ground, words).
    scroll: usize,
    outline: usize,
    column: usize,
    tree_look: Option<(Rgb, Rgb)>,
    /// The window's place when it is neither maximized nor full screen (kept for the next run), and whether it is kept at all.
    normal: Option<Placement>,
    remember: bool,
}

/// What the callbacks need to know, copied out so that nothing is held while Cocoa is called.
#[derive(Clone, Copy, Default)]
struct Snap {
    window: usize,
    view: usize,
    delegate: usize,
    space: usize,
    scale: f64,
    pts: (f64, f64),
    ctx: usize,
    px: (i32, i32),
}

thread_local! {
    static MAC: RefCell<Mac> = RefCell::new(Mac::default());
    static HANDLER: RefCell<Option<Box<dyn Handler>>> = const { RefCell::new(None) };
    static QUEUE: RefCell<VecDeque<Event>> = const { RefCell::new(VecDeque::new()) };
    static BUSY: Cell<bool> = const { Cell::new(false) };
    static QUIT: Cell<bool> = const { Cell::new(false) };
}

fn snap() -> Snap {
    MAC.with(|m| {
        m.try_borrow()
            .map(|m| Snap {
                window: m.window,
                view: m.view,
                delegate: m.delegate,
                space: m.space,
                scale: m.scale,
                pts: m.pts,
                ctx: m.buffer.as_ref().map_or(0, |b| b.ctx as usize),
                px: m.buffer.as_ref().map_or((0, 0), |b| (b.w, b.h)),
            })
            .unwrap_or_default()
    })
}

fn with_mac(f: impl FnOnce(&mut Mac)) {
    MAC.with(|m| {
        if let Ok(mut m) = m.try_borrow_mut() {
            f(&mut m);
        }
    });
}

/// Gives the reader an event (or queues it if the reader is busy) and does what it answers.
fn dispatch(event: Event) {
    QUEUE.with(|q| q.borrow_mut().push_back(event));
    if BUSY.get() {
        return;
    }
    BUSY.set(true);
    while let Some(event) = QUEUE.with(|q| q.borrow_mut().pop_front()) {
        let Some(mut handler) = HANDLER.with(|h| h.borrow_mut().take()) else {
            QUEUE.with(|q| q.borrow_mut().push_front(event));
            break;
        };
        let actions = handler.event(event);
        HANDLER.with(|h| *h.borrow_mut() = Some(handler));
        for action in actions {
            perform(action);
        }
    }
    BUSY.set(false);
}

fn with_handler<R>(f: impl FnOnce(&mut dyn Handler) -> R) -> Option<R> {
    let mut handler = HANDLER.with(|h| h.borrow_mut().take())?;
    let result = f(handler.as_mut());
    HANDLER.with(|h| *h.borrow_mut() = Some(handler));
    Some(result)
}

fn set_needs_display() {
    let s = snap();
    if s.view != 0 {
        send1::<(), bool>(s.view as Id, c"setNeedsDisplay:", true);
    }
}

/// Reads the size of the view and the scale of the screen; tells the reader what changed.
fn sync_size() {
    let s = snap();
    if s.view == 0 || s.window == 0 {
        return;
    }
    place_buttons();
    let bounds = send_rect(s.view as Id, c"bounds");
    let scale: f64 = send0(s.window as Id, c"backingScaleFactor");
    let scale = if scale >= 1.0 { scale } else { 1.0 };
    let px = ((bounds.w * scale).round() as i32, (bounds.h * scale).round() as i32);
    let mut dpi_changed = false;
    let mut size_changed = false;
    with_mac(|m| {
        m.scale = scale;
        m.pts = (bounds.w, bounds.h);
        let dpi = (96.0 * scale).round() as u32;
        if m.told_dpi != dpi {
            m.told_dpi = dpi;
            dpi_changed = true;
        }
        if px.0 > 0 && px.1 > 0 && m.buffer.as_ref().is_none_or(|b| (b.w, b.h) != px) {
            m.buffer = make_buffer(m.space as Id, px.0, px.1);
            size_changed = m.buffer.is_some();
        }
    });
    if dpi_changed {
        dispatch(Event::Dpi((96.0 * scale).round() as u32));
    }
    if size_changed {
        dispatch(Event::Size { w: px.0, h: px.1 });
        set_needs_display();
    }
}

fn make_buffer(space: Id, w: i32, h: i32) -> Option<Buffer> {
    let (uw, uh) = (usize::try_from(w).ok()?, usize::try_from(h).ok()?);
    let len = uw.checked_mul(uh)?.checked_mul(4)?;
    if len > 1 << 31 {
        return None;
    }
    let mut data = vec![0u8; len];
    // 8194: premultiplied alpha first, 32-bit little endian = BGRA in memory.
    // SAFETY: `data` is `uh` rows of `uw * 4` bytes and lives as long as the context (see `Buffer`); the colour space is live.
    let ctx = unsafe { CGBitmapContextCreate(data.as_mut_ptr().cast(), uw, uh, 8, uw * 4, space, 8194) };
    if ctx.is_null() { None } else { Some(Buffer { ctx, w, h, _data: data }) }
}

/// Asks the reader to draw into the back buffer.
fn paint_now() {
    let s = snap();
    if s.ctx == 0 {
        return;
    }
    let mut painter = MacPainter::new(s.ctx as Id, s.space as Id, s.px.1, s.scale);
    let _ = with_handler(|h| h.paint(&mut painter));
}

/// Puts the back buffer on the screen (called while the view is being drawn).
fn present() {
    let s = snap();
    if s.ctx == 0 {
        return;
    }
    let current: Id = send0(class(c"NSGraphicsContext"), c"currentContext");
    let target: Id = send0(current, c"CGContext");
    if target.is_null() {
        return;
    }
    // SAFETY: the buffer's context is live (it is replaced only on this thread, outside drawing); the image is a copy made now and released.
    unsafe {
        let image = CGBitmapContextCreateImage(s.ctx as Id);
        if !image.is_null() {
            CGContextSetInterpolationQuality(target, 1);
            CGContextDrawImage(target, CgRect { x: 0.0, y: 0.0, w: s.pts.0, h: s.pts.1 }, image);
            CGImageRelease(image);
        }
    }
}

// --- the painter -----------------------------------------------------------------------------------------------------

struct MacPainter {
    scale: f64,
    ctx: Id,
    space: Id,
    h: i32,
    fonts: [Id; 4],
    attrs: [Cf; 4],
    font: usize,
}

/// The SF Symbol that stands for a letter of the icon font of Windows (Segoe Fluent Icons / Segoe MDL2 Assets), for every letter the reader draws.
fn symbol_for(ch: char) -> Option<&'static str> {
    Some(match ch {
        '\u{E70D}' => "chevron.down",
        '\u{E76C}' => "chevron.right",
        '\u{E8A0}' => "sidebar.left",
        '\u{E838}' => "folder",
        '\u{E74E}' => "square.and.arrow.down",
        '\u{E749}' => "printer",
        '\u{E8B0}' => "cursorarrow",
        '\u{E7E6}' => "highlighter",
        '\u{E70B}' => "note.text",
        '\u{EE56}' => "pencil.tip",
        '\u{E739}' => "square.on.circle",
        '\u{E8AC}' => "textbox",
        '\u{E8A8}' => "rectangle.dashed",
        '\u{E8DC}' => "underline",
        '\u{EDE0}' => "strikethrough",
        '\u{EDC6}' => "scribble.variable",
        '\u{E8EC}' => "seal",
        '\u{EA3A}' => "circle",
        '\u{E72A}' => "line.diagonal",
        '\u{E90A}' => "text.bubble",
        '\u{E9A6}' => "arrow.up.left.and.arrow.down.right",
        '\u{E721}' => "magnifyingglass",
        '\u{E712}' => "ellipsis",
        _ => return None,
    })
}

/// The symbols that are drawn small (the arrows of the menus and of the tree) and a little bolder.
fn symbol_is_small(name: &str) -> bool {
    name.starts_with("chevron.")
}

fn font_index(font: Font) -> usize {
    match font {
        Font::Body => 0,
        Font::Bold => 1,
        Font::Title => 2,
        Font::Icon => 3,
    }
}

impl MacPainter {
    fn new(ctx: Id, space: Id, h: i32, scale: f64) -> MacPainter {
        // The system font (kCTFontUIFontSystem = 2, emphasized = 3); CoreText falls back to other fonts for the letters it lacks (Chinese).
        // The icon font (Segoe Fluent Icons) has no counterpart: `glyph` draws SF Symbols (see `symbol_for`); this font is only the fallback.
        let sizes = [(2, 13.0), (3, 13.0), (3, 22.0), (2, 16.0)];
        let fonts = sizes.map(|(kind, size)| {
            // SAFETY: plain numbers and a null language (the default).
            unsafe { CTFontCreateUIFontForLanguage(kind, size * scale, null_mut()) }
        });
        let attrs = fonts.map(|font| {
            // SAFETY: the keys are CoreText's constants and the values live objects; the dictionary retains them.
            Cf(unsafe {
                let keys = [kCTFontAttributeName, kCTForegroundColorFromContextAttributeName];
                let values = [font, kCFBooleanTrue];
                CFDictionaryCreate(
                    null_mut(),
                    keys.as_ptr(),
                    values.as_ptr(),
                    2,
                    kCFTypeDictionaryKeyCallBacks.as_ptr().cast(),
                    kCFTypeDictionaryValueCallBacks.as_ptr().cast(),
                )
            })
        });
        MacPainter { scale, ctx, space, h, fonts, attrs, font: 0 }
    }

    /// The y of the bottom edge of a rectangle that has its top at `y`, in the context's own coordinates (y up).
    fn cg_rect(&self, r: Rect) -> CgRect {
        CgRect { x: f64::from(r.x), y: f64::from(self.h - r.y - r.h), w: f64::from(r.w), h: f64::from(r.h) }
    }

    /// An SF Symbol in the middle of `r`, in `color` (the symbol is a mask: its shape is filled with the colour). False if the system has no such symbol.
    fn draw_symbol(&self, r: Rect, name: &str, color: Rgb) -> bool {
        let title = cfstr(name);
        let image: Id = send2(class(c"NSImage"), c"imageWithSystemSymbolName:accessibilityDescription:", title.0, null_mut::<c_void>());
        if image.is_null() {
            return false;
        }
        // The size is in pixels of the back buffer (the symbol is made as big as the point size says, one unit to a pixel).
        let (points, weight) = if symbol_is_small(name) { (10.0, 0.4) } else { (15.0, 0.0) };
        let config: Id = send3::<Id, f64, f64, i64>(class(c"NSImageSymbolConfiguration"), c"configurationWithPointSize:weight:scale:", points * self.scale, weight, 2);
        let sized: Id = if config.is_null() { image } else { send1(image, c"imageWithSymbolConfiguration:", config) };
        let size: CgPoint = send0(if sized.is_null() { image } else { sized }, c"size");
        let (w, h) = (size.x.ceil(), size.y.ceil());
        if w < 1.0 || h < 1.0 || w > 512.0 || h > 512.0 {
            return false;
        }
        let mut proposed = CgRect { x: 0.0, y: 0.0, w, h };
        let cg: Id = send3::<Id, *mut CgRect, Id, Id>(if sized.is_null() { image } else { sized }, c"CGImageForProposedRect:context:hints:", &mut proposed, null_mut(), null_mut());
        if cg.is_null() {
            return false;
        }
        let dest = CgRect { x: f64::from(r.x) + (f64::from(r.w) - w) / 2.0, y: self.fy(f64::from(r.y) + (f64::from(r.h) + h) / 2.0), w, h };
        self.set_fill(color, 255);
        // SAFETY: a live context and image (the image belongs to the NSImage, which lives through this call); the state saved is the one restored.
        unsafe {
            CGContextSaveGState(self.ctx);
            CGContextClipToMask(self.ctx, dest, cg);
            CGContextFillRect(self.ctx, dest);
            CGContextRestoreGState(self.ctx);
        }
        true
    }

    fn fy(&self, y: f64) -> f64 {
        f64::from(self.h) - y
    }

    fn line(&self, s: &str) -> Cf {
        let text = cfstr(s);
        let attrs = self.attrs.get(self.font).map_or(null_mut(), |a| a.0);
        // SAFETY: a live string and attribute dictionary; the line keeps what it needs.
        unsafe {
            let attributed = Cf(CFAttributedStringCreate(null_mut(), text.0, attrs));
            Cf(CTLineCreateWithAttributedString(attributed.0))
        }
    }

    fn metrics(&self) -> (f64, f64, f64) {
        let font = self.fonts.get(self.font).copied().unwrap_or(null_mut());
        // SAFETY: a live font.
        unsafe { (CTFontGetAscent(font), CTFontGetDescent(font), CTFontGetLeading(font)) }
    }

    fn set_fill(&self, color: Rgb, alpha: u8) {
        let (r, g, b) = rgb_f(color);
        // SAFETY: a live context and plain numbers.
        unsafe { CGContextSetRGBFillColor(self.ctx, r, g, b, f64::from(alpha) / 255.0) };
    }

    /// The path of a rounded rectangle (radii: top left, top right, bottom right, bottom left), in the top-left coordinates of the window.
    fn round_path(&self, r: Rect, radii: [i32; 4]) -> bool {
        if r.w <= 0 || r.h <= 0 {
            return false;
        }
        let half = r.w.min(r.h) / 2;
        let [tl, tr, br, bl] = radii.map(|v| f64::from(v.clamp(0, half)));
        let (x, y, w, h) = (f64::from(r.x), f64::from(r.y), f64::from(r.w), f64::from(r.h));
        let f = |py: f64| self.fy(py);
        // SAFETY: a live context; plain numbers.
        unsafe {
            let c = self.ctx;
            CGContextBeginPath(c);
            CGContextMoveToPoint(c, x + tl, f(y));
            CGContextAddLineToPoint(c, x + w - tr, f(y));
            CGContextAddArcToPoint(c, x + w, f(y), x + w, f(y + tr), tr);
            CGContextAddLineToPoint(c, x + w, f(y + h - br));
            CGContextAddArcToPoint(c, x + w, f(y + h), x + w - br, f(y + h), br);
            CGContextAddLineToPoint(c, x + bl, f(y + h));
            CGContextAddArcToPoint(c, x, f(y + h), x, f(y + h - bl), bl);
            CGContextAddLineToPoint(c, x, f(y + tl));
            CGContextAddArcToPoint(c, x, f(y), x + tl, f(y), tl);
            CGContextClosePath(c);
        }
        true
    }
}

impl Drop for MacPainter {
    fn drop(&mut self) {
        for font in self.fonts {
            release(font);
        }
    }
}

impl Painter for MacPainter {
    fn fill_rect(&mut self, r: Rect, color: Rgb) {
        self.fill_rect_alpha(r, color, 255);
    }

    fn fill_rect_alpha(&mut self, r: Rect, color: Rgb, alpha: u8) {
        if r.w <= 0 || r.h <= 0 {
            return;
        }
        self.set_fill(color, alpha);
        // SAFETY: a live context and a plain rectangle.
        unsafe { CGContextFillRect(self.ctx, self.cg_rect(r)) };
    }

    fn draw_bgra(&mut self, dest: Rect, src_w: i32, src_h: i32, bgra: &[u8]) {
        if dest.w <= 0 || dest.h <= 0 || src_w <= 0 || src_h <= 0 {
            return;
        }
        let (sw, sh) = (src_w as usize, src_h as usize);
        let Some(needed) = sw.checked_mul(sh).and_then(|n| n.checked_mul(4)) else { return };
        if bgra.len() < needed {
            return;
        }
        // SAFETY: the provider reads `needed` bytes of `bgra`, which is live until the image is released at the end of this block; the colour
        // space and the context are live. 8198: the alpha byte is skipped (the picture is opaque), 32-bit little endian = BGRA.
        unsafe {
            let provider = CGDataProviderCreateWithData(null_mut(), bgra.as_ptr().cast(), needed, null());
            let image = CGImageCreate(sw, sh, 8, 32, sw * 4, self.space, 8198, provider, null(), true, 0);
            CGDataProviderRelease(provider);
            if !image.is_null() {
                CGContextSetInterpolationQuality(self.ctx, if dest.w == src_w && dest.h == src_h { 1 } else { 3 });
                CGContextDrawImage(self.ctx, self.cg_rect(dest), image);
                CGImageRelease(image);
            }
        }
    }

    fn text(&mut self, x: i32, y: i32, s: &str, color: Rgb) {
        if s.is_empty() {
            return;
        }
        let line = self.line(s);
        let (ascent, _, leading) = self.metrics();
        self.set_fill(color, 255);
        // SAFETY: a live context and line; the text position is set before the line is drawn.
        unsafe {
            CGContextSetTextPosition(self.ctx, f64::from(x), self.fy(f64::from(y) + ascent + leading / 2.0));
            CTLineDraw(line.0, self.ctx);
        }
    }

    fn text_width(&mut self, s: &str) -> i32 {
        if s.is_empty() {
            return 0;
        }
        let line = self.line(s);
        let (mut a, mut d, mut l) = (0.0, 0.0, 0.0);
        // SAFETY: a live line and three live numbers to write to.
        let width = unsafe { CTLineGetTypographicBounds(line.0, &mut a, &mut d, &mut l) };
        width.ceil() as i32
    }

    fn line_height(&self) -> i32 {
        let (a, d, l) = self.metrics();
        (a + d + l).ceil() as i32
    }

    fn set_font(&mut self, font: Font) {
        self.font = font_index(font);
    }

    fn glyph(&mut self, r: Rect, ch: char, color: Rgb) {
        if let Some(name) = symbol_for(ch)
            && self.draw_symbol(r, name, color)
        {
            return;
        }
        // No picture for this letter: its own text (a box, if the system font lacks it).
        self.set_font(Font::Icon);
        let s = ch.to_string();
        let (w, h) = (self.text_width(&s), self.line_height());
        self.text(r.x + (r.w - w) / 2, r.y + (r.h - h) / 2, &s, color);
        self.set_font(Font::Body);
    }

    fn fill_shape(&mut self, r: Rect, radii: [i32; 4], color: Rgb, alpha: u8) {
        if self.round_path(r, radii) {
            self.set_fill(color, alpha);
            // SAFETY: a live context holding the path just made.
            unsafe { CGContextFillPath(self.ctx) };
        }
    }

    fn stroke_round(&mut self, r: Rect, radius: i32, width: i32, color: Rgb, alpha: u8) {
        if width <= 0 {
            return;
        }
        // The line is centred on the path, so the path is held half a width inside the edge.
        let inner = Rect { x: r.x + width / 2, y: r.y + width / 2, w: r.w - width / 2 * 2, h: r.h - width / 2 * 2 };
        let rad = (radius - width / 2).max(0);
        if self.round_path(inner, [rad; 4]) {
            let (red, green, blue) = rgb_f(color);
            // SAFETY: a live context holding the path just made.
            unsafe {
                CGContextSetRGBStrokeColor(self.ctx, red, green, blue, f64::from(alpha) / 255.0);
                CGContextSetLineWidth(self.ctx, f64::from(width));
                CGContextStrokePath(self.ctx);
            }
        }
    }

    fn polyline(&mut self, points: &[(f32, f32)], width: f32, color: Rgb, alpha: u8) {
        let Some((first, rest)) = points.split_first() else { return };
        if rest.is_empty() {
            return;
        }
        let (red, green, blue) = rgb_f(color);
        // SAFETY: a live context; plain numbers.
        unsafe {
            let c = self.ctx;
            CGContextSetRGBStrokeColor(c, red, green, blue, f64::from(alpha) / 255.0);
            CGContextSetLineWidth(c, f64::from(width));
            CGContextSetLineCap(c, 1);
            CGContextSetLineJoin(c, 1);
            CGContextBeginPath(c);
            CGContextMoveToPoint(c, f64::from(first.0), self.fy(f64::from(first.1)));
            for p in rest {
                CGContextAddLineToPoint(c, f64::from(p.0), self.fy(f64::from(p.1)));
            }
            CGContextStrokePath(c);
        }
    }
}

// --- the classes -----------------------------------------------------------------------------------------------------

type Method = (&'static CStr, *const c_void, &'static CStr);

fn make_class(name: &CStr, superclass: &CStr, methods: &[Method]) -> Id {
    // SAFETY: names are zero-terminated strings; the class is made once, filled, then registered; each implementation is an `extern "C"` function whose
    // arguments and result are those the type string says.
    unsafe {
        let class = objc_allocateClassPair(objc_getClass(superclass.as_ptr()), name.as_ptr(), 0);
        if class.is_null() {
            return null_mut();
        }
        for (selector, imp, types) in methods {
            class_addMethod(class, sel_registerName(selector.as_ptr()), *imp, types.as_ptr());
        }
        objc_registerClassPair(class);
        class
    }
}

/// The pointer of the event in view points, as pixels of the window (y from the top).
fn pointer(event: Id) -> (i32, i32) {
    let s = snap();
    let p: CgPoint = send0(event, c"locationInWindow");
    (((p.x) * s.scale).round() as i32, ((s.pts.1 - p.y) * s.scale).round() as i32)
}

fn flags(event: Id) -> (bool, bool) {
    let f: u64 = send0(event, c"modifierFlags");
    let (shift, control, command) = (f & (1 << 17) != 0, f & (1 << 18) != 0, f & (1 << 20) != 0);
    (control || command, shift)
}

fn update_cursor(x: i32, y: i32) {
    let Some(cursor) = with_handler(|h| h.cursor_at(x, y)) else { return };
    let name: &CStr = match cursor {
        Cursor::Arrow => c"arrowCursor",
        Cursor::Hand => c"pointingHandCursor",
        Cursor::Cross | Cursor::SizeNWSE | Cursor::SizeNESW => c"crosshairCursor",
        Cursor::Text => c"IBeamCursor",
        Cursor::SizeWE => c"resizeLeftRightCursor",
        Cursor::Move => c"openHandCursor",
        Cursor::Hidden => {
            send1::<(), bool>(class(c"NSCursor"), c"setHiddenUntilMouseMoves:", true);
            return;
        }
    };
    let c: Id = send0(class(c"NSCursor"), name);
    send0::<()>(c, c"set");
}

extern "C" fn view_draw_rect(_this: Id, _sel: Sel, _dirty: CgRect) {
    sync_size();
    paint_now();
    present();
}

extern "C" fn view_true(_this: Id, _sel: Sel) -> i8 {
    1
}

extern "C" fn view_mouse_down(_this: Id, _sel: Sel, event: Id) {
    let (x, y) = pointer(event);
    let (ctrl, shift) = flags(event);
    let clicks: i64 = send0(event, c"clickCount");
    if with_handler(|h| h.nc_hit(x, y)) == Some(NcHit::Caption) {
        title_bar_click(event, clicks);
        return;
    }
    dispatch(Event::MouseDown { x, y, ctrl, shift, clicks: clicks.clamp(1, 3) as u8 });
    update_cursor(x, y);
}

/// A press on the empty part of the title strip: a double click does what the system is set to do with the title bar's (zoom, minimise or nothing), and a
/// single press drags the window.
fn title_bar_click(event: Id, clicks: i64) {
    let s = snap();
    if s.window == 0 {
        return;
    }
    let window = s.window as Id;
    if clicks >= 2 {
        let defaults: Id = send0(class(c"NSUserDefaults"), c"standardUserDefaults");
        let key = cfstr("AppleActionOnDoubleClick");
        let action = rust_string(send1(defaults, c"stringForKey:", key.0));
        match action.as_str() {
            "None" => {}
            "Minimize" => send1::<(), Id>(window, c"performMiniaturize:", null_mut()),
            _ => send1::<(), Id>(window, c"performZoom:", null_mut()),
        }
    } else {
        send1::<(), Id>(window, c"performWindowDragWithEvent:", event);
    }
}

/// How far the window's buttons reach from the left edge of the title strip, in points at 96 dots per inch (the reader keeps this much free).
pub const BUTTONS_ROOM: i64 = 80;

/// The height of the title strip, in points (the reader's `TITLE_H`).
const STRIP_H: f64 = 40.0;

/// Makes the window's title bar as tall as the reader's title strip and puts the three buttons in the middle of it. The system lays the buttons out
/// again (on a resize, in full screen), so this is done whenever the view is sized or drawn; it moves nothing that is already in place.
fn place_buttons() {
    let s = snap();
    if s.window == 0 {
        return;
    }
    let window = s.window as Id;
    let style: u64 = send0(window, c"styleMask");
    if style & (1 << 14) != 0 {
        return;
    }
    let frame = send_rect(window, c"frame");
    for kind in 0..3_i64 {
        let button: Id = send1(window, c"standardWindowButton:", kind);
        if button.is_null() {
            continue;
        }
        let bar: Id = send0(button, c"superview");
        let container: Id = if bar.is_null() { null_mut() } else { send0(bar, c"superview") };
        if kind == 0 && !container.is_null() {
            let want = CgRect { x: 0.0, y: frame.h - STRIP_H, w: frame.w, h: STRIP_H };
            let have = send_rect(container, c"frame");
            if (have.y - want.y).abs() > 0.5 || (have.h - want.h).abs() > 0.5 || (have.w - want.w).abs() > 0.5 {
                send1::<(), CgRect>(container, c"setFrame:", want);
            }
            let inner = CgRect { x: 0.0, y: 0.0, w: frame.w, h: STRIP_H };
            let have = send_rect(bar, c"frame");
            if (have.h - inner.h).abs() > 0.5 || (have.w - inner.w).abs() > 0.5 {
                send1::<(), CgRect>(bar, c"setFrame:", inner);
            }
        }
        let b = send_rect(button, c"frame");
        // As far from the left edge as from the top and bottom, then the system's 20 points from one button to the next.
        let y = ((STRIP_H - b.h) / 2.0).round();
        let x = y + 20.0 * kind as f64;
        if (b.y - y).abs() > 0.5 || (b.x - x).abs() > 0.5 {
            send1::<(), CgPoint>(button, c"setFrameOrigin:", CgPoint { x, y });
        }
    }
}

/// The reader's mark (the white 轻 on the rounded blue square) drawn into a new square buffer of `side` pixels.
fn render_mark(space: Id, side: i32) -> Option<Buffer> {
    let buffer = make_buffer(space, side, side)?;
    let mut painter = MacPainter::new(buffer.ctx, space, side, 1.0);
    // Apple's icon grid: the rounded square is about 80% of the canvas, centred, so that the mark is as big as the other icons in the Dock.
    let inner = (side as f32 * 0.805).round() as i32;
    let off = (side - inner) / 2;
    let r = Rect { x: off, y: off, w: inner, h: inner };
    let k = inner as f32 / 100.0;
    painter.fill_shape(r, [(qingpdf_view::logo::RADIUS * k).round() as i32; 4], qingpdf_view::accent::QING_LIGHT.base, 255);
    let o = off as f32;
    for stroke in qingpdf_view::logo::strokes(inner) {
        let points: Vec<(f32, f32)> = stroke.pts.iter().map(|&(x, y)| (o + x * k, o + y * k)).collect();
        painter.polyline(&points, stroke.width * k, 0xFFFFFF, 255);
    }
    drop(painter);
    Some(buffer)
}

/// Writes a CGImage as a PNG file; false if it could not be.
fn write_png(image: Id, path: &str) -> bool {
    let allocated: Id = send0(class(c"NSBitmapImageRep"), c"alloc");
    let rep: Id = send1(allocated, c"initWithCGImage:", image);
    if rep.is_null() {
        return false;
    }
    // 4 is NSBitmapImageFileTypePNG.
    let data: Id = send2::<Id, u64, Id>(rep, c"representationUsingType:properties:", 4, null_mut());
    let name = cfstr(path);
    let ok = !data.is_null() && send2::<i8, Id, bool>(data, c"writeToFile:atomically:", name.0, true) != 0;
    release(rep);
    ok
}

/// The hidden `--write-icon`: the mark as a PNG of `side` pixels (the build script of the `.app` makes the icon from it).
pub fn write_icon(path: &str, side: i32) -> bool {
    // SAFETY: the pool is popped below, before the function ends; a Core Graphics constant for the colour space, released below.
    let (pool, space) = unsafe { (objc_autoreleasePoolPush(), CGColorSpaceCreateWithName(kCGColorSpaceSRGB)) };
    let mut ok = false;
    if let Some(buffer) = render_mark(space, side) {
        // SAFETY: the buffer's context is live until `buffer` goes at the end of this block; the image is a copy made now and released below.
        let image = unsafe { CGBitmapContextCreateImage(buffer.ctx) };
        if !image.is_null() {
            ok = write_png(image, path);
            // SAFETY: the image made above, released once.
            unsafe { CGImageRelease(image) };
        }
    }
    // SAFETY: the colour space made above and the pool pushed above, each once.
    unsafe {
        CGColorSpaceRelease(space);
        objc_autoreleasePoolPop(pool);
    }
    ok
}

/// The reader's mark as the application's icon, so that the Dock and the boxes (password, save) show it and not a generic folder.
fn set_app_icon() {
    const SIDE: i32 = 256;
    let s = snap();
    let Some(buffer) = render_mark(s.space as Id, SIDE) else { return };
    // SAFETY: the buffer's context is live until `buffer` goes at the end of this function; the image is a copy made now and released below.
    let image = unsafe { CGBitmapContextCreateImage(buffer.ctx) };
    if image.is_null() {
        return;
    }
    let allocated: Id = send0(class(c"NSImage"), c"alloc");
    let icon: Id = send2(allocated, c"initWithCGImage:size:", image, CgPoint { x: f64::from(SIDE), y: f64::from(SIDE) });
    // SAFETY: the image was made above and is no longer needed: the NSImage holds its own reference.
    unsafe { CGImageRelease(image) };
    if !icon.is_null() {
        let app: Id = send0(class(c"NSApplication"), c"sharedApplication");
        send1::<(), Id>(app, c"setApplicationIconImage:", icon);
        // Kept for the life of the program: the boxes set it as their icon (an NSAlert of a program with no bundle shows a folder otherwise).
        APP_ICON.store(icon as usize, std::sync::atomic::Ordering::Relaxed);
    }
}

/// The application's icon made by `set_app_icon` (0 until then); never released.
static APP_ICON: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Gives a box the application's icon.
fn set_alert_icon(alert: Id) {
    let icon = APP_ICON.load(std::sync::atomic::Ordering::Relaxed) as Id;
    if !icon.is_null() {
        send1::<(), Id>(alert, c"setIcon:", icon);
    }
}

extern "C" fn view_mouse_dragged(_this: Id, _sel: Sel, event: Id) {
    let (x, y) = pointer(event);
    dispatch(Event::MouseMove { x, y, shift: flags(event).1 });
    update_cursor(x, y);
}

extern "C" fn view_mouse_up(_this: Id, _sel: Sel, event: Id) {
    let (x, y) = pointer(event);
    dispatch(Event::MouseUp { x, y });
    update_cursor(x, y);
}

extern "C" fn view_mouse_moved(_this: Id, _sel: Sel, event: Id) {
    let (x, y) = pointer(event);
    dispatch(Event::Hover { x, y });
    update_cursor(x, y);
}

extern "C" fn view_mouse_exited(_this: Id, _sel: Sel, _event: Id) {
    dispatch(Event::HoverEnd);
}

extern "C" fn view_right_mouse_down(_this: Id, _sel: Sel, event: Id) {
    let (x, y) = pointer(event);
    dispatch(Event::ContextMenu { x, y });
}

extern "C" fn view_other_mouse_up(_this: Id, _sel: Sel, event: Id) {
    let button: i64 = send0(event, c"buttonNumber");
    if button == 2 {
        let (x, y) = pointer(event);
        dispatch(Event::MiddleClick { x, y });
    }
}

extern "C" fn view_scroll_wheel(_this: Id, _sel: Sel, event: Id) {
    let (x, y) = pointer(event);
    let (ctrl, shift) = flags(event);
    let (dx, dy): (f64, f64) = (send0(event, c"scrollingDeltaX"), send0(event, c"scrollingDeltaY"));
    let precise: i8 = send0(event, c"hasPreciseScrollingDeltas");
    // 120 is a notch of a Windows wheel: a trackpad's points count 3 to one unit, and a mouse wheel's lines 40.
    let k = if precise != 0 { 3.0 } else { 40.0 };
    if dy != 0.0 {
        dispatch(Event::Wheel { delta: (dy * k).round() as i32, ctrl, shift, x, y });
    }
    if dx != 0.0 {
        // (Windows' positive is to the right; content that moves right is a scroll to the left.)
        dispatch(Event::HWheel { delta: (-dx * k).round() as i32 });
    }
}

extern "C" fn view_magnify(_this: Id, _sel: Sel, event: Id) {
    let (x, y) = pointer(event);
    let m: f64 = send0(event, c"magnification");
    dispatch(Event::Wheel { delta: (m * 1200.0).round() as i32, ctrl: true, shift: false, x, y });
}

extern "C" fn view_key_down(_this: Id, _sel: Sel, event: Id) {
    let code: u16 = send0(event, c"keyCode");
    let (ctrl, shift) = flags(event);
    if let Some(vk) = vk_of(code) {
        dispatch(Event::Key { vk, ctrl, shift });
    }
}

extern "C" fn view_drag_entered(_this: Id, _sel: Sel, _sender: Id) -> u64 {
    1
}

extern "C" fn view_drop(_this: Id, _sel: Sel, sender: Id) -> i8 {
    let board: Id = send0(sender, c"draggingPasteboard");
    let kind = cfstr("NSFilenamesPboardType");
    let list: Id = send1(board, c"propertyListForType:", kind.0);
    let paths: Vec<PathBuf> = array_items(list).into_iter().map(|s| PathBuf::from(rust_string(s))).collect();
    if paths.is_empty() {
        return 0;
    }
    dispatch(Event::Drop(paths));
    1
}

extern "C" fn delegate_should_terminate(_this: Id, _sel: Sel, _app: Id) -> u64 {
    if QUIT.get() {
        return 1;
    }
    dispatch(Event::CloseRequested);
    // The reader quits at once when nothing is unsaved (or after its own boxes, which are modal and so are over by now): say yes then, so that
    // a Quit from the Dock, AppleScript or logging out is not reported as cancelled. Otherwise it is the reader's to finish later.
    u64::from(QUIT.get())
}

extern "C" fn delegate_should_close(_this: Id, _sel: Sel, _window: Id) -> i8 {
    if QUIT.get() {
        return 1;
    }
    dispatch(Event::CloseRequested);
    // Quitting asks the window first: a no here would be taken as the person cancelling the Quit (AppleScript gets "User cancelled" and
    // the program stays), so it is yes when the reader has quit.
    i8::from(QUIT.get())
}

extern "C" fn delegate_no(_this: Id, _sel: Sel, _app: Id) -> i8 {
    0
}

extern "C" fn delegate_resized(_this: Id, _sel: Sel, _note: Id) {
    sync_size();
    note_place();
}

extern "C" fn delegate_moved(_this: Id, _sel: Sel, _note: Id) {
    note_place();
}

extern "C" fn delegate_timer(_this: Id, _sel: Sel, timer: Id) {
    let info: Id = send0(timer, c"userInfo");
    let id: u32 = send0(info, c"unsignedIntValue");
    dispatch(Event::Timer(id));
}

extern "C" fn delegate_wake(_this: Id, _sel: Sel) {
    dispatch(Event::Wake);
}

extern "C" fn delegate_menu_item(_this: Id, _sel: Sel, item: Id) {
    let tag: i64 = send0(item, c"tag");
    if CHOOSING.get() {
        CHOSEN.set(u32::try_from(tag).ok());
        return;
    }
    // A shortcut that is shown in the menu and was pressed: the key goes to the reader as a key, as it would have if the menu had not shown it
    // (the same `Event::Key` the pages get). A key that only chose the item in a menu being tracked (Return, say) is no shortcut.
    let app: Id = send0(class(c"NSApplication"), c"sharedApplication");
    let event: Id = send0(app, c"currentEvent");
    let key: Id = send0(item, c"keyEquivalent");
    if !event.is_null() && !key.is_null() && send0::<u64>(event, c"type") == 10 && send0::<u64>(key, c"length") > 0 {
        let code: u16 = send0(event, c"keyCode");
        let (ctrl, shift) = flags(event);
        if let Some(vk) = vk_of(code).filter(|&v| ctrl || (vk::F2..=vk::F11).contains(&v)) {
            dispatch(Event::Key { vk, ctrl, shift });
            return;
        }
    }
    if let Ok(id) = u32::try_from(tag) {
        dispatch(Event::Command(id));
    }
}

fn register_classes() -> (Id, Id, Id) {
    let view = make_class(
        c"QingpdfView",
        c"NSView",
        &[
            (c"drawRect:", view_draw_rect as *const c_void, c"v@:{CGRect=dddd}"),
            (c"acceptsFirstResponder", view_true as *const c_void, c"c@:"),
            (c"mouseDown:", view_mouse_down as *const c_void, c"v@:@"),
            (c"mouseDragged:", view_mouse_dragged as *const c_void, c"v@:@"),
            (c"mouseUp:", view_mouse_up as *const c_void, c"v@:@"),
            (c"mouseMoved:", view_mouse_moved as *const c_void, c"v@:@"),
            (c"mouseExited:", view_mouse_exited as *const c_void, c"v@:@"),
            (c"rightMouseDown:", view_right_mouse_down as *const c_void, c"v@:@"),
            (c"otherMouseUp:", view_other_mouse_up as *const c_void, c"v@:@"),
            (c"scrollWheel:", view_scroll_wheel as *const c_void, c"v@:@"),
            (c"magnifyWithEvent:", view_magnify as *const c_void, c"v@:@"),
            (c"keyDown:", view_key_down as *const c_void, c"v@:@"),
            (c"draggingEntered:", view_drag_entered as *const c_void, c"Q@:@"),
            (c"draggingUpdated:", view_drag_entered as *const c_void, c"Q@:@"),
            (c"prepareForDragOperation:", view_true_arg as *const c_void, c"c@:@"),
            (c"performDragOperation:", view_drop as *const c_void, c"c@:@"),
        ],
    );
    let delegate = make_class(
        c"QingpdfDelegate",
        c"NSObject",
        &[
            (c"applicationShouldTerminate:", delegate_should_terminate as *const c_void, c"Q@:@"),
            (c"applicationShouldTerminateAfterLastWindowClosed:", delegate_no as *const c_void, c"c@:@"),
            (c"windowShouldClose:", delegate_should_close as *const c_void, c"c@:@"),
            (c"windowDidResize:", delegate_resized as *const c_void, c"v@:@"),
            (c"windowDidChangeBackingProperties:", delegate_resized as *const c_void, c"v@:@"),
            (c"timerFired:", delegate_timer as *const c_void, c"v@:@"),
            (c"wakeUp", delegate_wake as *const c_void, c"v@:"),
            (c"menuItem:", delegate_menu_item as *const c_void, c"v@:@"),
            (c"windowDidMove:", delegate_moved as *const c_void, c"v@:@"),
            (c"windowDidEnterFullScreen:", delegate_entered_full as *const c_void, c"v@:@"),
            (c"windowDidExitFullScreen:", delegate_left_full as *const c_void, c"v@:@"),
            (c"application:openFiles:", delegate_open_files as *const c_void, c"v@:@@"),
            (c"systemChanged:", delegate_system_changed as *const c_void, c"v@:@"),
            (c"systemLook", delegate_system_look as *const c_void, c"v@:"),
            (c"outlineView:numberOfChildrenOfItem:", tree_count as *const c_void, c"q@:@@"),
            (c"outlineView:child:ofItem:", tree_child as *const c_void, c"@@:@q@"),
            (c"outlineView:isItemExpandable:", tree_expandable as *const c_void, c"c@:@@"),
            (c"outlineView:objectValueForTableColumn:byItem:", tree_value as *const c_void, c"@@:@@@"),
            (c"outlineView:setObjectValue:forTableColumn:byItem:", tree_set_value as *const c_void, c"v@:@@@@"),
            (c"outlineView:shouldEditTableColumn:item:", tree_may_edit as *const c_void, c"c@:@@@"),
            (c"outlineViewSelectionDidChange:", tree_selected as *const c_void, c"v@:@"),
            (c"outlineViewItemDidExpand:", tree_expanded as *const c_void, c"v@:@"),
            (c"outlineViewItemDidCollapse:", tree_collapsed as *const c_void, c"v@:@"),
            (c"outlineView:writeItems:toPasteboard:", tree_write as *const c_void, c"c@:@@@"),
            (c"outlineView:validateDrop:proposedItem:proposedChildIndex:", tree_validate as *const c_void, c"Q@:@@@q"),
            (c"outlineView:acceptDrop:item:childIndex:", tree_accept as *const c_void, c"c@:@@@q"),
            (c"treeClicked:", tree_clicked as *const c_void, c"v@:@"),
            (c"control:textView:doCommandBySelector:", delegate_command as *const c_void, c"c@:@@:"),
            (c"controlTextDidChange:", delegate_text_changed as *const c_void, c"v@:@"),
            (c"controlTextDidEndEditing:", delegate_text_ended as *const c_void, c"v@:@"),
        ],
    );
    let field = make_class(c"QingpdfField", c"NSTextField", &[(c"becomeFirstResponder", field_become as *const c_void, c"c@:")]);
    (view, delegate, field)
}

extern "C" fn view_true_arg(_this: Id, _sel: Sel, _sender: Id) -> i8 {
    1
}

// --- the actions -----------------------------------------------------------------------------------------------------

/// The file extensions the filters name (`*.pdf;*.PDF` gives `pdf`); empty when any file is allowed.
fn extensions(filters: &[(String, String)]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (_, pattern) in filters {
        for part in pattern.split(';') {
            let part = part.trim();
            if part == "*" || part == "*.*" {
                return Vec::new();
            }
            if let Some(ext) = part.strip_prefix("*.") {
                let ext = ext.to_ascii_lowercase();
                if !ext.is_empty() && !out.contains(&ext) {
                    out.push(ext);
                }
            }
        }
    }
    out
}

fn path_of(url: Id) -> Option<PathBuf> {
    if url.is_null() {
        return None;
    }
    let path: Id = send0(url, c"path");
    Some(PathBuf::from(rust_string(path)))
}

fn run_panel(panel: Id) -> bool {
    let result: i64 = send0(panel, c"runModal");
    result == 1
}

fn open_panel(title: &str, filters: &[(String, String)], many: bool, folder: bool) -> Vec<PathBuf> {
    let panel: Id = send0(class(c"NSOpenPanel"), c"openPanel");
    let message = cfstr(title);
    send1::<(), Id>(panel, c"setMessage:", message.0);
    send1::<(), bool>(panel, c"setAllowsMultipleSelection:", many);
    send1::<(), bool>(panel, c"setCanChooseFiles:", !folder);
    send1::<(), bool>(panel, c"setCanChooseDirectories:", folder);
    if folder {
        send1::<(), bool>(panel, c"setCanCreateDirectories:", true);
    }
    let exts = extensions(filters);
    if !exts.is_empty() && !folder {
        let list = cf_array_of_strings(&exts);
        send1::<(), Id>(panel, c"setAllowedFileTypes:", list.0);
    }
    if !run_panel(panel) {
        return Vec::new();
    }
    let urls: Id = send0(panel, c"URLs");
    array_items(urls).into_iter().filter_map(path_of).collect()
}

fn save_panel(title: &str, filters: &[(String, String)], name: &str) -> Option<PathBuf> {
    let panel: Id = send0(class(c"NSSavePanel"), c"savePanel");
    let message = cfstr(title);
    send1::<(), Id>(panel, c"setMessage:", message.0);
    let file = cfstr(name);
    send1::<(), Id>(panel, c"setNameFieldStringValue:", file.0);
    let exts = extensions(filters);
    if !exts.is_empty() {
        let list = cf_array_of_strings(&exts);
        send1::<(), Id>(panel, c"setAllowedFileTypes:", list.0);
    }
    if !run_panel(panel) {
        return None;
    }
    let url: Id = send0(panel, c"URL");
    path_of(url)
}

/// A box with buttons; the number (from 0) of the one pressed.
/// The words of the buttons of the system's boxes, in the system's language (as Windows' own boxes are).
struct ButtonWords {
    ok: &'static str,
    cancel: &'static str,
    yes: &'static str,
    no: &'static str,
    save: &'static str,
    dont_save: &'static str,
}

fn button_words() -> ButtonWords {
    if qingpdf_view::i18n::Lang::from_langid(ui_language()) == qingpdf_view::i18n::Lang::Zh {
        ButtonWords { ok: "好", cancel: "取消", yes: "是", no: "否", save: "存储", dont_save: "不存储" }
    } else {
        ButtonWords { ok: "OK", cancel: "Cancel", yes: "Yes", no: "No", save: "Save", dont_save: "Don't Save" }
    }
}

fn alert(title: &str, text: &str, buttons: &[&str]) -> usize {
    let alert = new_object(c"NSAlert");
    set_alert_icon(alert);
    let (t, m) = (cfstr(title), cfstr(text));
    send1::<(), Id>(alert, c"setMessageText:", t.0);
    send1::<(), Id>(alert, c"setInformativeText:", m.0);
    let cancel = button_words().cancel;
    for button in buttons {
        let b = cfstr(button);
        let made: Id = send1(alert, c"addButtonWithTitle:", b.0);
        if *button == cancel {
            let escape = cfstr("\u{1b}");
            send1::<(), Id>(made, c"setKeyEquivalent:", escape.0);
        }
    }
    let pressed: i64 = send0(alert, c"runModal");
    release(alert);
    usize::try_from(pressed - 1000).unwrap_or(0)
}

fn perform(action: Action) {
    match action {
        Action::Invalidate | Action::InvalidateRect(_) => set_needs_display(),
        Action::SetTitle(title) => {
            let s = snap();
            let t = cfstr(&title);
            send1::<(), Id>(s.window as Id, c"setTitle:", t.0);
        }
        Action::SetTimer { id, ms } => {
            perform(Action::KillTimer(id));
            let s = snap();
            let info: Id = send1(class(c"NSNumber"), c"numberWithUnsignedInt:", id);
            let seconds = f64::from(ms.max(1)) / 1000.0;
            let timer: Id = send5::<Id, f64, Id, Sel, Id, bool>(
                class(c"NSTimer"),
                c"timerWithTimeInterval:target:selector:userInfo:repeats:",
                seconds,
                s.delegate as Id,
                sel(c"timerFired:"),
                info,
                true,
            );
            if !timer.is_null() {
                // SAFETY: a live timer, kept by us until it is stopped.
                unsafe { CFRetain(timer) };
                let run_loop: Id = send0(class(c"NSRunLoop"), c"currentRunLoop");
                // SAFETY: a Foundation constant.
                let mode = unsafe { NSRunLoopCommonModes };
                send2::<(), Id, Id>(run_loop, c"addTimer:forMode:", timer, mode);
                with_mac(|m| {
                    m.timers.insert(id, timer as usize);
                });
            }
        }
        Action::KillTimer(id) => {
            let mut old = 0;
            with_mac(|m| old = m.timers.remove(&id).unwrap_or(0));
            if old != 0 {
                send0::<()>(old as Id, c"invalidate");
                release(old as Id);
            }
        }
        Action::PickFile { title, filters } => dispatch(Event::FilePicked(open_panel(&title, &filters, false, false).into_iter().next())),
        Action::PickFiles { title, filters } => dispatch(Event::FilesPicked(open_panel(&title, &filters, true, false))),
        Action::PickFolder { title } => dispatch(Event::FolderPicked(open_panel(&title, &[], false, true).into_iter().next())),
        Action::PickSavePath { title, filters, name } => dispatch(Event::SavePicked(save_panel(&title, &filters, &name))),
        Action::Message { title, text } => {
            alert(&title, &text, &[button_words().ok]);
        }
        Action::Confirm { tag, title, text } => {
            // No is first, so that it is the button Return presses (as on Windows).
            let w = button_words();
            let yes = alert(&title, &text, &[w.no, w.yes]) == 1;
            dispatch(Event::Confirmed { tag, yes });
        }
        Action::AskSave { tag, title, text } => {
            let w = button_words();
            let choice = match alert(&title, &text, &[w.save, w.dont_save, w.cancel]) {
                0 => SaveChoice::Save,
                1 => SaveChoice::Discard,
                _ => SaveChoice::Cancel,
            };
            dispatch(Event::SaveChoice { tag, choice });
        }
        Action::SetClipboard(text) => {
            let board: Id = send0(class(c"NSPasteboard"), c"generalPasteboard");
            let _: i64 = send0(board, c"clearContents");
            let s = cfstr(&text);
            // SAFETY: an AppKit constant.
            let kind = unsafe { NSPasteboardTypeString };
            let ok: i8 = send2(board, c"setString:forType:", s.0, kind);
            dispatch(Event::ClipboardSet(ok != 0));
        }
        Action::OpenUri(uri) => {
            let s = cfstr(&uri);
            let url: Id = send1(class(c"NSURL"), c"URLWithString:", s.0);
            if !url.is_null() {
                let workspace: Id = send0(class(c"NSWorkspace"), c"sharedWorkspace");
                let _: i8 = send1(workspace, c"openURL:", url);
            }
        }
        Action::Quit => {
            save_placement();
            QUIT.set(true);
            let app: Id = send0(class(c"NSApplication"), c"sharedApplication");
            send1::<(), Id>(app, c"stop:", null_mut());
            // `stop:` is looked at after an event: this one makes the loop turn once more.
            let event: Id = send9::<Id, u64, CgPoint, u64, f64, i64, Id, i16, i64, i64>(
                class(c"NSEvent"),
                c"otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:subtype:data1:data2:",
                15,
                CgPoint::default(),
                0,
                0.0,
                0,
                null_mut(),
                0,
                0,
                0,
            );
            send2::<(), Id, bool>(app, c"postEvent:atStart:", event, true);
        }
        Action::SetMenus(menus) => install_menus_kept(menus),
        Action::AskText { tag, title, prompt, secret, initial, ok, cancel } => {
            let text = ask_text(&title, &prompt, secret, &initial, &ok, &cancel);
            dispatch(Event::Text { tag, text });
        }
        Action::SearchBox(b) => set_search_box(b),
        Action::Find(rect) => show_find(rect),
        Action::PillEdit(b) => set_pill_edit(b),
        Action::FieldEdit(Some(b)) => show_field(&b),
        Action::FieldEdit(None) => hide_field(),
        Action::FieldMove(r) => {
            let active = read_mac(|m| m.field_active).unwrap_or(0);
            if active != 0 {
                place(active as Id, r);
            }
        }
        Action::Theme(t) => apply_theme(t),
        Action::Outline { nodes, select } => fill_tree(&nodes, select),
        Action::OutlineRename(index) => start_rename(index),
        Action::Sidebar(r) => {
            // (A tree that was never shown has nothing to hide.)
            if (r.w > 0 && r.h > 0 || read_mac(|m| m.scroll).unwrap_or(0) != 0)
                && let Some((scroll, _)) = ensure_tree()
            {
                place(scroll, r);
            }
        }
        Action::ContextMenu { entries, at } => context_menu(&entries, at),
        Action::Choose { items, selected, at } => dispatch(Event::Chosen(choose(&items, selected, at))),
        Action::ShowMenu { at, .. } => show_menus(at),
        Action::HideCursor => send1::<(), bool>(class(c"NSCursor"), c"setHiddenUntilMouseMoves:", true),
        Action::FullScreen(on) => set_full_screen(on),
        Action::AskForm(spec) => {
            let answer = ask_form(&spec);
            dispatch(Event::Form { tag: spec.tag, answer });
        }
        Action::ChoosePrinter { max_page, preset } => match choose_printer(max_page, preset) {
            Ok(setup) => dispatch(Event::Printer(setup)),
            // No printer, or the panel would not open: the reader says so.
            Err(()) => dispatch(Event::PrintError(String::new())),
        },
        Action::Print(op) => match print_op(op) {
            Ok(true) => dispatch(Event::PrintStepDone),
            Ok(false) => {}
            Err(message) => dispatch(Event::PrintError(message)),
        },
    }
}

// --- the boxes typed in ----------------------------------------------------------------------------------------------

fn read_mac<R>(f: impl FnOnce(&Mac) -> R) -> Option<R> {
    MAC.with(|m| m.try_borrow().ok().map(|m| f(&m)))
}

/// Asks for a line of text in a box with two buttons (the line is dots if `secret`); none if it was cancelled.
fn ask_text(title: &str, prompt: &str, secret: bool, initial: &str, ok: &str, cancel: &str) -> Option<String> {
    let alert = new_object(c"NSAlert");
    set_alert_icon(alert);
    let (t, m) = (cfstr(title), cfstr(prompt));
    send1::<(), Id>(alert, c"setMessageText:", t.0);
    send1::<(), Id>(alert, c"setInformativeText:", m.0);
    let (ok, cancel) = (cfstr(if ok.is_empty() { "OK" } else { ok }), cfstr(if cancel.is_empty() { "Cancel" } else { cancel }));
    let _: Id = send1(alert, c"addButtonWithTitle:", ok.0);
    let _: Id = send1(alert, c"addButtonWithTitle:", cancel.0);
    let allocated: Id = send0(class(if secret { c"NSSecureTextField" } else { c"NSTextField" }), c"alloc");
    let field: Id = send1(allocated, c"initWithFrame:", CgRect { x: 0.0, y: 0.0, w: 300.0, h: 24.0 });
    let start = cfstr(initial);
    send1::<(), Id>(field, c"setStringValue:", start.0);
    send1::<(), Id>(alert, c"setAccessoryView:", field);
    send0::<()>(alert, c"layout");
    let window: Id = send0(alert, c"window");
    send1::<(), Id>(window, c"setInitialFirstResponder:", field);
    let pressed: i64 = send0(alert, c"runModal");
    let answer = (pressed == 1000).then(|| rust_string(send0(field, c"stringValue")));
    // What was typed (a password) is wiped from the box before it goes.
    let empty = cfstr("");
    send1::<(), Id>(field, c"setStringValue:", empty.0);
    release(field);
    release(alert);
    answer
}

/// The text box of `kind` (0 the find box, 1 the page pill's, 2 a field of one line, 3 a field of several), made when first needed.
fn text_box(kind: usize) -> Id {
    let existing = read_mac(|m| [m.search, m.pill, m.field_single, m.field_multi].get(kind).copied().unwrap_or(0)).unwrap_or(0);
    if existing != 0 {
        return existing as Id;
    }
    let s = snap();
    let (class, theme) = (read_mac(|m| m.field_class).unwrap_or(0) as Id, read_mac(|m| m.theme).flatten());
    if class.is_null() || s.view == 0 {
        return null_mut();
    }
    let allocated: Id = send0(class, c"alloc");
    let field: Id = send1(allocated, c"initWithFrame:", CgRect::default());
    if field.is_null() {
        return null_mut();
    }
    let font: Id = send1::<Id, f64>(self::class(c"NSFont"), c"systemFontOfSize:", 13.0);
    send1::<(), Id>(field, c"setFont:", font);
    send1::<(), bool>(field, c"setBordered:", false);
    // No ring: the reader draws the box under this one.
    send1::<(), i64>(field, c"setFocusRingType:", 1);
    let boxed = kind >= 2;
    send1::<(), bool>(field, c"setDrawsBackground:", boxed);
    if let Some((text, ground)) = theme {
        send1::<(), Id>(field, c"setTextColor:", srgb_color(text));
        if boxed {
            send1::<(), Id>(field, c"setBackgroundColor:", srgb_color(ground));
        }
    }
    let cell: Id = send0(field, c"cell");
    if kind == 3 {
        send1::<(), bool>(cell, c"setWraps:", true);
        send1::<(), bool>(cell, c"setScrollable:", false);
    } else {
        send1::<(), bool>(cell, c"setWraps:", false);
        send1::<(), bool>(cell, c"setScrollable:", true);
    }
    send1::<(), Id>(field, c"setDelegate:", s.delegate as Id);
    send1::<(), bool>(field, c"setHidden:", true);
    send1::<(), Id>(s.view as Id, c"addSubview:", field);
    // The view holds the box now; this reference is the box's own for good.
    with_mac(|m| match kind {
        0 => m.search = field as usize,
        1 => m.pill = field as usize,
        2 => m.field_single = field as usize,
        _ => m.field_multi = field as usize,
    });
    field
}

fn srgb_color(color: Rgb) -> Id {
    let (r, g, b) = rgb_f(color);
    send4::<Id, f64, f64, f64, f64>(class(c"NSColor"), c"colorWithSRGBRed:green:blue:alpha:", r, g, b, 1.0)
}

/// Puts a box at a place of the window (pixels, y from the top); a rectangle with no size puts it away.
fn place(field: Id, r: Rect) {
    if r.w <= 0 || r.h <= 0 {
        send1::<(), bool>(field, c"setHidden:", true);
        return;
    }
    let s = snap();
    let k = 1.0 / s.scale.max(1.0);
    let frame = CgRect { x: f64::from(r.x) * k, y: s.pts.1 - f64::from(r.y + r.h) * k, w: f64::from(r.w) * k, h: f64::from(r.h) * k };
    send1::<(), CgRect>(field, c"setFrame:", frame);
    send1::<(), bool>(field, c"setHidden:", false);
}

fn is_editing(field: Id) -> bool {
    let editor: Id = send0(field, c"currentEditor");
    !editor.is_null()
}

/// The keyboard goes back to the pages.
fn focus_view() {
    let s = snap();
    if s.window != 0 && s.view != 0 {
        send1::<bool, Id>(s.window as Id, c"makeFirstResponder:", s.view as Id);
    }
}

fn set_text(field: Id, text: &str) {
    let t = cfstr(text);
    send1::<(), Id>(field, c"setStringValue:", t.0);
}

fn set_search_box(b: Option<SearchBox>) {
    match b {
        Some(b) => {
            let field = text_box(0);
            if field.is_null() {
                return;
            }
            with_mac(|m| m.search_on = true);
            let hint = cfstr(&b.hint);
            send1::<(), Id>(field, c"setPlaceholderString:", hint.0);
            send1::<(), bool>(field, c"setEnabled:", b.enabled);
            place(field, b.rect);
        }
        None => {
            with_mac(|m| m.search_on = false);
            let field = read_mac(|m| m.search).unwrap_or(0) as Id;
            if !field.is_null() {
                if is_editing(field) {
                    focus_view();
                }
                send1::<(), bool>(field, c"setHidden:", true);
            }
        }
    }
}

/// The find box gets the keyboard (with everything selected), at `rect` if the command bar's is not in use; or it is given up.
fn show_find(rect: Option<Rect>) {
    let on = read_mac(|m| m.search_on).unwrap_or(false);
    match rect {
        Some(r) => {
            let field = text_box(0);
            if field.is_null() {
                return;
            }
            if !on {
                place(field, r);
            }
            if !is_editing(field) {
                send1::<bool, Id>(field, c"selectText:", null_mut());
            }
        }
        None => {
            let field = read_mac(|m| m.search).unwrap_or(0) as Id;
            if field.is_null() {
                return;
            }
            if is_editing(field) {
                focus_view();
            }
            if !on {
                send1::<(), bool>(field, c"setHidden:", true);
            }
        }
    }
}

fn set_pill_edit(b: Option<(Rect, String)>) {
    match b {
        Some((r, text)) => {
            let field = text_box(1);
            if field.is_null() {
                return;
            }
            with_mac(|m| m.pill_open = true);
            set_text(field, &text);
            place(field, r);
            send1::<bool, Id>(field, c"selectText:", null_mut());
        }
        None => {
            let mut was_open = false;
            with_mac(|m| was_open = std::mem::replace(&mut m.pill_open, false));
            let field = read_mac(|m| m.pill).unwrap_or(0) as Id;
            if was_open && !field.is_null() {
                focus_view();
                send1::<(), bool>(field, c"setHidden:", true);
            }
        }
    }
}

/// The box over the page pill is left: Enter (`taken`) hands over what is in it, Esc gives it up; a click outside (`focus` false) gives it up too.
fn finish_pill(taken: bool, focus: bool) {
    let mut was_open = false;
    with_mac(|m| was_open = std::mem::replace(&mut m.pill_open, false));
    let field = read_mac(|m| m.pill).unwrap_or(0) as Id;
    if !was_open || field.is_null() {
        return;
    }
    let text = taken.then(|| rust_string(send0(field, c"stringValue")).chars().take(32).collect::<String>());
    if focus {
        focus_view();
    }
    send1::<(), bool>(field, c"setHidden:", true);
    dispatch(Event::PillEdit(text));
}

fn show_field(b: &FieldBox) {
    let field = text_box(if b.multiline { 3 } else { 2 });
    if field.is_null() {
        return;
    }
    let other = read_mac(|m| if b.multiline { m.field_single } else { m.field_multi }).unwrap_or(0) as Id;
    // The box that was in use before is put away without telling the reader: it has moved on to this one already.
    with_mac(|m| {
        m.field_active = field as usize;
        m.field_max = b.max_len as usize;
    });
    if !other.is_null() {
        send1::<(), bool>(other, c"setHidden:", true);
    }
    set_text(field, &b.text);
    place(field, b.rect);
    send1::<bool, Id>(field, c"selectText:", null_mut());
}

/// The box is put away, keeping nothing: the reader has asked for that.
fn hide_field() {
    let mut active = 0;
    with_mac(|m| active = std::mem::replace(&mut m.field_active, 0));
    if active != 0 {
        if is_editing(active as Id) {
            focus_view();
        }
        send1::<(), bool>(active as Id, c"setHidden:", true);
    }
}

/// The person has left the box: its text goes to the reader, with how it was left, and the box is put away.
fn finish_field(exit: FieldExit) {
    let mut active = 0;
    with_mac(|m| active = std::mem::replace(&mut m.field_active, 0));
    if active == 0 {
        return;
    }
    let text = rust_string(send0(active as Id, c"stringValue")).replace("\r\n", "\n").replace('\r', "\n");
    if exit != FieldExit::Blur {
        focus_view();
    }
    send1::<(), bool>(active as Id, c"setHidden:", true);
    dispatch(Event::FieldDone { text, exit });
}

/// Which box a control is: 0 the find box, 1 the page pill's, 2 the field of a form; none for another control.
fn which_box(control: Id) -> Option<usize> {
    if control.is_null() {
        return None;
    }
    let at = control as usize;
    read_mac(|m| {
        if at == m.search {
            Some(0)
        } else if at == m.pill {
            Some(1)
        } else if at != 0 && at == m.field_active {
            Some(2)
        } else {
            None
        }
    })
    .flatten()
}

/// Control and Shift held now (Command counts as Control), as the event being handled says.
fn current_flags() -> (bool, bool) {
    let app: Id = send0(class(c"NSApplication"), c"sharedApplication");
    let event: Id = send0(app, c"currentEvent");
    if event.is_null() { (false, false) } else { flags(event) }
}

/// What a box does with a command of the text system (Enter, Esc, Tab): true if it was taken.
fn box_command(which: usize, name: &str, multiline: bool, text_view: Id) -> bool {
    let (ctrl, shift) = current_flags();
    match which {
        // Enter and Esc go to the reader (it runs the search, or leaves the box).
        0 => match name {
            "insertNewline:" => {
                dispatch(Event::Key { vk: vk::RETURN, ctrl, shift });
                true
            }
            "cancelOperation:" => {
                dispatch(Event::Key { vk: vk::ESCAPE, ctrl, shift });
                true
            }
            _ => false,
        },
        1 => match name {
            "insertNewline:" => {
                finish_pill(true, true);
                true
            }
            "cancelOperation:" => {
                finish_pill(false, true);
                true
            }
            _ => false,
        },
        _ => match name {
            "cancelOperation:" => {
                finish_field(FieldExit::Cancel);
                true
            }
            "insertTab:" => {
                finish_field(FieldExit::Next);
                true
            }
            "insertBacktab:" => {
                finish_field(FieldExit::Prev);
                true
            }
            // A field of one line ends with Enter; one of several with Control+Enter, and Enter is a new line.
            "insertNewline:" if !multiline || ctrl => {
                finish_field(FieldExit::Commit);
                true
            }
            "insertNewline:" => {
                send1::<(), Id>(text_view, c"insertNewlineIgnoringFieldEditor:", null_mut());
                true
            }
            "insertLineBreak:" | "insertNewlineIgnoringFieldEditor:" if ctrl => {
                finish_field(FieldExit::Commit);
                true
            }
            _ => false,
        },
    }
}

extern "C" fn delegate_command(_this: Id, _sel: Sel, control: Id, text_view: Id, command: Sel) -> i8 {
    let Some(which) = which_box(control) else { return 0 };
    // SAFETY: the runtime gives the name of the selector as a zero-terminated string that lives as long as the selector does (for good).
    let name = unsafe { CStr::from_ptr(sel_getName(command)) }.to_str().unwrap_or("");
    let multiline = read_mac(|m| m.field_active == m.field_multi && m.field_multi != 0).unwrap_or(false);
    i8::from(box_command(which, name, multiline, text_view))
}

extern "C" fn delegate_text_changed(_this: Id, _sel: Sel, note: Id) {
    let control: Id = send0(note, c"object");
    match which_box(control) {
        Some(0) => dispatch(Event::FindText(rust_string(send0(control, c"stringValue")))),
        Some(2) => {
            let max = read_mac(|m| m.field_max).unwrap_or(0);
            if max > 0 {
                let text = rust_string(send0(control, c"stringValue"));
                if text.chars().count() > max {
                    set_text(control, &text.chars().take(max).collect::<String>());
                }
            }
        }
        _ => {}
    }
}

extern "C" fn delegate_text_ended(_this: Id, _sel: Sel, note: Id) {
    let control: Id = send0(note, c"object");
    match which_box(control) {
        // The keyboard went elsewhere (a click outside): the pill's text is not taken, the field's is kept.
        Some(1) => finish_pill(false, false),
        Some(2) => finish_field(FieldExit::Blur),
        _ => {}
    }
}

/// The find box has been given the keyboard (a click, or Tab).
extern "C" fn field_become(this: Id, command: Sel) -> i8 {
    // SAFETY: the method of the class we are made from, called with the arguments and the result type it has.
    let ok = unsafe {
        let imp = class_getMethodImplementation(class(c"NSTextField"), command);
        if imp.is_null() {
            return 0;
        }
        std::mem::transmute::<*const c_void, unsafe extern "C" fn(Id, Sel) -> i8>(imp)(this, command)
    };
    if ok != 0 && read_mac(|m| m.search == this as usize && this as usize != 0).unwrap_or(false) {
        dispatch(Event::FindFocus);
    }
    ok
}

/// A menu label as the menu bar shows it: no `&` mnemonics (`&Open`, `打开(&O)`) and no accelerator text (after the tab).
fn clean_label(label: &str) -> String {
    let head = label.split('\t').next().unwrap_or("");
    let mut out = String::new();
    let mut chars = head.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '(' && chars.peek() == Some(&'&') {
            // `(&O)`: skip to the closing bracket.
            for d in chars.by_ref() {
                if d == ')' {
                    break;
                }
            }
        } else if c == '&' {
            if chars.peek() == Some(&'&') {
                chars.next();
                out.push('&');
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The shortcut a menu label shows after its tab (`Ctrl+Shift+O`, `F2`, `Ctrl++`) as a key equivalent: the key and the modifier flags (Ctrl is Command).
/// Only keys that cannot be a letter typed into a box get one: with Command (or Control for Tab) or a function key. The menu then swallows the key,
/// and `delegate_menu_item` gives it to the reader as the `Event::Key` it would have been. Delete, Tab and the like have no key equivalent
/// (a menu would take them from a text box too); they are written after the title (see `build_menu`). Ctrl+C is left to the text boxes' own Copy.
fn shortcut(label: &str) -> Option<(String, u64)> {
    let (_, text) = label.rsplit_once('\t')?;
    let (mut rest, mut mask, mut ctrl) = (text.trim(), 0u64, false);
    loop {
        if let Some(r) = rest.strip_prefix("Ctrl+").filter(|r| !r.is_empty()) {
            (rest, ctrl) = (r, true);
        } else if let Some(r) = rest.strip_prefix("Shift+").filter(|r| !r.is_empty()) {
            (rest, mask) = (r, mask | 1 << 17);
        } else {
            break;
        }
    }
    let function = rest.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()).filter(|n| (1..=12).contains(n));
    let key: String = if let Some(n) = function {
        char::from_u32(0xF704 + n - 1)?.to_string()
    } else if rest == "Tab" && ctrl {
        // Control, not Command: Command+Tab is the system's.
        mask |= 1 << 18;
        return Some(("\t".to_string(), mask));
    } else if ctrl && rest.chars().count() == 1 && !rest.eq_ignore_ascii_case("c") {
        rest.to_lowercase()
    } else {
        return None;
    };
    if ctrl {
        mask |= 1 << 20;
    }
    Some((key, mask))
}

fn build_menu(title: &str, entries: &[MenuEntry], target: Id) -> Id {
    let t = cfstr(&clean_label(title));
    let allocated: Id = send0(class(c"NSMenu"), c"alloc");
    let menu: Id = send1(allocated, c"initWithTitle:", t.0);
    send1::<(), bool>(menu, c"setAutoenablesItems:", false);
    for entry in entries {
        let item: Id = match entry {
            MenuEntry::Separator => {
                let separator: Id = send0(class(c"NSMenuItem"), c"separatorItem");
                send1::<(), Id>(menu, c"addItem:", separator);
                continue;
            }
            MenuEntry::Item { id, label, checked, enabled, .. } => {
                let equivalent = shortcut(label);
                // A shortcut that is not a key equivalent is still shown, after the title.
                let shown = match (&equivalent, label.rsplit_once('\t')) {
                    (None, Some((_, text))) if !text.trim().is_empty() => format!("{}    {}", clean_label(label), text.trim()),
                    _ => clean_label(label),
                };
                let (l, key) = (cfstr(&shown), cfstr(equivalent.as_ref().map_or("", |e| e.0.as_str())));
                let allocated: Id = send0(class(c"NSMenuItem"), c"alloc");
                let item: Id = send3(allocated, c"initWithTitle:action:keyEquivalent:", l.0, sel(c"menuItem:"), key.0);
                if let Some((_, mask)) = equivalent {
                    send1::<(), u64>(item, c"setKeyEquivalentModifierMask:", mask);
                }
                send1::<(), i64>(item, c"setTag:", i64::from(*id));
                send1::<(), Id>(item, c"setTarget:", target);
                send1::<(), bool>(item, c"setEnabled:", *enabled);
                send1::<(), i64>(item, c"setState:", i64::from(*checked));
                item
            }
            MenuEntry::Submenu { title, entries } => {
                let (l, key) = (cfstr(&clean_label(title)), cfstr(""));
                let allocated: Id = send0(class(c"NSMenuItem"), c"alloc");
                let item: Id = send3(allocated, c"initWithTitle:action:keyEquivalent:", l.0, null::<c_void>(), key.0);
                let sub = build_menu(title, entries, target);
                send1::<(), Id>(item, c"setSubmenu:", sub);
                release(sub);
                item
            }
        };
        send1::<(), Id>(menu, c"addItem:", item);
        release(item);
    }
    menu
}

/// The menus of the reader, kept for the pop-up (`Action::ShowMenu`).
fn install_menus_kept(menus: Vec<Menu>) {
    install_menus(&menus);
    MENUS.with(|m| *m.borrow_mut() = menus);
}

/// The menu bar: the application's own menu (Quit), then the reader's menus. The shortcuts are shown (see `shortcut`); the keys reach the reader as `Event::Key`.
fn install_menus(menus: &[Menu]) {
    let s = snap();
    let bar = build_menu("", &[], s.delegate as Id);
    let item_class = class(c"NSMenuItem");
    // The application menu.
    let app_menu = build_menu("qingpdf", &[], s.delegate as Id);
    let (quit_title, quit_key) = (cfstr("Quit qingpdf"), cfstr("q"));
    let allocated: Id = send0(item_class, c"alloc");
    let quit: Id = send3(allocated, c"initWithTitle:action:keyEquivalent:", quit_title.0, sel(c"terminate:"), quit_key.0);
    send1::<(), Id>(app_menu, c"addItem:", quit);
    release(quit);
    // Cut, copy, paste and select all for the system's text boxes (password, search, page number, form fields): they go to whatever box has the
    // keyboard (no target), so they work only there; over the page no one takes them, the items are off and the keys go to the reader as
    // `Event::Key` as before. They are hidden (the reader's own Edit menu is what people see) but keep their keys.
    for (title, action, key) in [("Cut", c"cut:", "x"), ("Copy", c"copy:", "c"), ("Paste", c"paste:", "v"), ("Select All", c"selectAll:", "a")] {
        let (title, key) = (cfstr(title), cfstr(key));
        let allocated: Id = send0(item_class, c"alloc");
        let item: Id = send3(allocated, c"initWithTitle:action:keyEquivalent:", title.0, sel(action), key.0);
        if send1::<bool, Sel>(item, c"respondsToSelector:", sel(c"setAllowsKeyEquivalentWhenHidden:")) {
            send1::<(), bool>(item, c"setAllowsKeyEquivalentWhenHidden:", true);
            send1::<(), bool>(item, c"setHidden:", true);
        }
        send1::<(), Id>(app_menu, c"addItem:", item);
        release(item);
    }
    let (name, empty) = (cfstr("qingpdf"), cfstr(""));
    let allocated: Id = send0(item_class, c"alloc");
    let app_item: Id = send3(allocated, c"initWithTitle:action:keyEquivalent:", name.0, null::<c_void>(), empty.0);
    send1::<(), Id>(app_item, c"setSubmenu:", app_menu);
    send1::<(), Id>(bar, c"addItem:", app_item);
    release(app_item);
    release(app_menu);
    for menu in menus {
        let sub = build_menu(&menu.title, &menu.entries, s.delegate as Id);
        let title = cfstr(&clean_label(&menu.title));
        let allocated: Id = send0(item_class, c"alloc");
        let item: Id = send3(allocated, c"initWithTitle:action:keyEquivalent:", title.0, null::<c_void>(), empty.0);
        send1::<(), Id>(item, c"setSubmenu:", sub);
        send1::<(), Id>(bar, c"addItem:", item);
        release(item);
        release(sub);
    }
    let app: Id = send0(class(c"NSApplication"), c"sharedApplication");
    send1::<(), Id>(app, c"setMainMenu:", bar);
    release(bar);
}

// --- the bookmarks tree ----------------------------------------------------------------------------------------------

/// The bookmarks as the tree shows them. An item of the tree is an NSNumber holding the index of its node; the numbers and the titles are ours (retained)
/// for as long as the tree shows this list.
#[derive(Default)]
struct Tree {
    items: Vec<Cf>,
    names: Vec<Cf>,
    children: Vec<Vec<usize>>,
    parent: Vec<Option<usize>>,
    roots: Vec<usize>,
}

thread_local! {
    static TREE: RefCell<Tree> = RefCell::new(Tree::default());
    /// The tree is being filled by the reader (what it tells then is not the person's doing).
    static FILLING: Cell<bool> = const { Cell::new(false) };
    /// The reader asked for a title to be typed over (the tree's own slow click is not allowed to).
    static RENAMING: Cell<bool> = const { Cell::new(false) };
    /// The node being dragged.
    static DRAGGED: Cell<Option<usize>> = const { Cell::new(None) };
    /// A drop-down list is open: the item chosen goes to `CHOSEN` and not to the reader as a command.
    static CHOOSING: Cell<bool> = const { Cell::new(false) };
    static CHOSEN: Cell<Option<u32>> = const { Cell::new(None) };
    /// The reader's menus, for `Action::ShowMenu`.
    static MENUS: RefCell<Vec<Menu>> = const { RefCell::new(Vec::new()) };
    /// Whether the reader thinks the window is full screen.
    static FULL_ASKED: Cell<bool> = const { Cell::new(false) };
}

fn tree_read<R>(f: impl FnOnce(&Tree) -> R) -> Option<R> {
    TREE.with(|t| t.try_borrow().ok().map(|t| f(&t)))
}

/// The node an item of the tree stands for.
fn node_of(item: Id) -> Option<usize> {
    if item.is_null() {
        return None;
    }
    usize::try_from(send0::<i64>(item, c"longLongValue")).ok()
}

fn item_of(node: usize) -> Id {
    tree_read(|t| t.items.get(node).map_or(null_mut(), |c| c.0)).unwrap_or(null_mut())
}

extern "C" fn tree_count(_this: Id, _sel: Sel, _outline: Id, item: Id) -> i64 {
    let n = if item.is_null() { tree_read(|t| t.roots.len()) } else { node_of(item).and_then(|i| tree_read(|t| t.children.get(i).map_or(0, Vec::len))) };
    i64::try_from(n.unwrap_or(0)).unwrap_or(0)
}

extern "C" fn tree_child(_this: Id, _sel: Sel, _outline: Id, index: i64, item: Id) -> Id {
    let parent = node_of(item);
    tree_read(|t| {
        let list = if item.is_null() { Some(&t.roots) } else { parent.and_then(|p| t.children.get(p)) };
        let node = list.and_then(|l| l.get(usize::try_from(index).ok()?))?;
        t.items.get(*node).map(|c| c.0)
    })
    .flatten()
    .unwrap_or(null_mut())
}

extern "C" fn tree_expandable(_this: Id, _sel: Sel, _outline: Id, item: Id) -> i8 {
    i8::from(node_of(item).and_then(|i| tree_read(|t| t.children.get(i).is_some_and(|c| !c.is_empty()))).unwrap_or(false))
}

extern "C" fn tree_value(_this: Id, _sel: Sel, _outline: Id, _column: Id, item: Id) -> Id {
    node_of(item).and_then(|i| tree_read(|t| t.names.get(i).map(|c| c.0))).flatten().unwrap_or(null_mut())
}

extern "C" fn tree_set_value(_this: Id, _sel: Sel, _outline: Id, value: Id, _column: Id, item: Id) {
    if let Some(index) = node_of(item) {
        dispatch(Event::OutlineRename { index, text: rust_string(value) });
        focus_view();
    }
}

extern "C" fn tree_may_edit(_this: Id, _sel: Sel, _outline: Id, _column: Id, _item: Id) -> i8 {
    i8::from(RENAMING.get())
}

/// The person went up or down the tree with the keys: that is a click on the bookmark. (A mouse click is `tree_clicked`; the reader's own selecting is not told.)
extern "C" fn tree_selected(_this: Id, _sel: Sel, note: Id) {
    if FILLING.get() {
        return;
    }
    let app: Id = send0(class(c"NSApplication"), c"sharedApplication");
    let event: Id = send0(app, c"currentEvent");
    if event.is_null() || send0::<u64>(event, c"type") != 10 {
        return;
    }
    let outline: Id = send0(note, c"object");
    let row: i64 = send0(outline, c"selectedRow");
    if row < 0 {
        return;
    }
    let item: Id = send1(outline, c"itemAtRow:", row);
    if let Some(index) = node_of(item) {
        dispatch(Event::OutlineClick(index));
    }
}

/// A click on a bookmark. The pages get the keyboard back, so that the keys scroll them (not while a title is being typed over).
extern "C" fn tree_clicked(_this: Id, _sel: Sel, outline: Id) {
    let row: i64 = send0(outline, c"clickedRow");
    if row < 0 {
        return;
    }
    let item: Id = send1(outline, c"itemAtRow:", row);
    if let Some(index) = node_of(item) {
        dispatch(Event::OutlineClick(index));
        if send0::<i64>(outline, c"editedRow") < 0 {
            focus_view();
        }
    }
}

fn tree_opened(note: Id, open: bool) {
    if FILLING.get() {
        return;
    }
    let info: Id = send0(note, c"userInfo");
    let key = cfstr("NSObject");
    let item: Id = send1(info, c"objectForKey:", key.0);
    if let Some(index) = node_of(item) {
        dispatch(Event::OutlineExpand { index, open });
    }
}

extern "C" fn tree_expanded(_this: Id, _sel: Sel, note: Id) {
    tree_opened(note, true);
}

extern "C" fn tree_collapsed(_this: Id, _sel: Sel, note: Id) {
    tree_opened(note, false);
}

extern "C" fn tree_write(_this: Id, _sel: Sel, _outline: Id, items: Id, board: Id) -> i8 {
    let Some(node) = array_items(items).first().and_then(|&i| node_of(i)) else { return 0 };
    DRAGGED.set(Some(node));
    // SAFETY: an AppKit constant.
    let kind = unsafe { NSPasteboardTypeString };
    let kinds = cf_array(&[kind]);
    let _: i64 = send2(board, c"declareTypes:owner:", kinds.0, null_mut::<c_void>() as Id);
    let text = cfstr(&node.to_string());
    let ok: i8 = send2(board, c"setString:forType:", text.0, kind);
    ok
}

/// Where a drop at `index` of the children of `item` (nil: the top; -1: on the item itself) would put the node: next to a node, or inside one.
fn drop_target(item: Id, index: i64) -> Option<(usize, Place)> {
    let parent = node_of(item);
    tree_read(|t| {
        let list = if item.is_null() { &t.roots } else { t.children.get(parent?)? };
        if index < 0 {
            return parent.map(|p| (p, Place::Inside));
        }
        match list.get(usize::try_from(index).ok()?) {
            Some(&n) => Some((n, Place::Before)),
            None => list.last().map(|&n| (n, Place::After)).or(parent.map(|p| (p, Place::Inside))),
        }
    })
    .flatten()
}

/// Is `node` the node `from` or below it (a branch cannot be dropped into itself)?
fn within(node: usize, from: usize) -> bool {
    tree_read(|t| {
        let mut at = Some(node);
        for _ in 0..t.parent.len() + 1 {
            match at {
                Some(n) if n == from => return true,
                Some(n) => at = t.parent.get(n).copied().flatten(),
                None => return false,
            }
        }
        false
    })
    .unwrap_or(false)
}

extern "C" fn tree_validate(_this: Id, _sel: Sel, _outline: Id, _info: Id, item: Id, index: i64) -> u64 {
    let (Some(from), Some((to, _))) = (DRAGGED.get(), drop_target(item, index)) else { return 0 };
    // 16 is NSDragOperationMove.
    if within(to, from) { 0 } else { 16 }
}

extern "C" fn tree_accept(_this: Id, _sel: Sel, _outline: Id, _info: Id, item: Id, index: i64) -> i8 {
    let (Some(from), Some((to, place))) = (DRAGGED.take(), drop_target(item, index)) else { return 0 };
    dispatch(Event::OutlineDrop { from, to, place });
    1
}

/// A right click on the tree: the bookmark under the pointer is the one the menu is for; the reader makes the menu (`Action::ContextMenu`).
extern "C" fn tree_menu_for_event(this: Id, _sel: Sel, event: Id) -> Id {
    let (x, y) = pointer(event);
    let local: CgPoint = send0(event, c"locationInWindow");
    let at: CgPoint = send2(this, c"convertPoint:fromView:", local, null_mut::<c_void>() as Id);
    let row: i64 = send1(this, c"rowAtPoint:", at);
    let item: Id = if row >= 0 { send1(this, c"itemAtRow:", row) } else { null_mut() };
    if row >= 0 {
        let rows: Id = send1(class(c"NSIndexSet"), c"indexSetWithIndex:", row as u64);
        FILLING.set(true);
        send2::<(), Id, bool>(this, c"selectRowIndexes:byExtendingSelection:", rows, false);
        FILLING.set(false);
    }
    dispatch(Event::OutlineMenu { index: node_of(item), x, y });
    null_mut()
}

/// The scroll view and the outline view of the bookmarks, made when first needed (hidden until the reader says where they go).
fn ensure_tree() -> Option<(Id, Id)> {
    if let Some((scroll, outline)) = read_mac(|m| (m.scroll, m.outline)).filter(|p| p.0 != 0) {
        return Some((scroll as Id, outline as Id));
    }
    let s = snap();
    if s.view == 0 {
        return None;
    }
    let outline_class = make_class(c"QingpdfOutline", c"NSOutlineView", &[(c"menuForEvent:", tree_menu_for_event as *const c_void, c"@@:@")]);
    if outline_class.is_null() {
        return None;
    }
    let allocated: Id = send0(outline_class, c"alloc");
    let outline: Id = send1(allocated, c"initWithFrame:", CgRect::default());
    let allocated: Id = send0(class(c"NSTableColumn"), c"alloc");
    let name = cfstr("title");
    let column: Id = send1(allocated, c"initWithIdentifier:", name.0);
    let allocated: Id = send0(class(c"NSScrollView"), c"alloc");
    let scroll: Id = send1(allocated, c"initWithFrame:", CgRect::default());
    if outline.is_null() || column.is_null() || scroll.is_null() {
        return None;
    }
    send1::<(), bool>(column, c"setEditable:", true);
    // NSTableColumnAutoresizingMask: the column is as wide as the tree.
    send1::<(), u64>(column, c"setResizingMask:", 1);
    let cell: Id = send0(column, c"dataCell");
    let font: Id = send1::<Id, f64>(class(c"NSFont"), c"systemFontOfSize:", 13.0);
    send1::<(), Id>(cell, c"setFont:", font);
    // NSLineBreakByTruncatingTail.
    send1::<(), u64>(cell, c"setLineBreakMode:", 4);
    send1::<(), Id>(outline, c"addTableColumn:", column);
    send1::<(), Id>(outline, c"setOutlineTableColumn:", column);
    send1::<(), Id>(outline, c"setHeaderView:", null_mut());
    send1::<(), u64>(outline, c"setColumnAutoresizingStyle:", 1);
    send1::<(), f64>(outline, c"setRowHeight:", 24.0);
    send1::<(), f64>(outline, c"setIndentationPerLevel:", 14.0);
    send1::<(), i64>(outline, c"setFocusRingType:", 1);
    send1::<(), Id>(outline, c"setDataSource:", s.delegate as Id);
    send1::<(), Id>(outline, c"setDelegate:", s.delegate as Id);
    send1::<(), Id>(outline, c"setTarget:", s.delegate as Id);
    send1::<(), Sel>(outline, c"setAction:", sel(c"treeClicked:"));
    // SAFETY: an AppKit constant.
    let kind = unsafe { NSPasteboardTypeString };
    let kinds = cf_array(&[kind]);
    send1::<(), Id>(outline, c"registerForDraggedTypes:", kinds.0);
    send2::<(), u64, bool>(outline, c"setDraggingSourceOperationMask:forLocal:", 16, true);
    send1::<(), Id>(scroll, c"setDocumentView:", outline);
    send1::<(), bool>(scroll, c"setHasVerticalScroller:", true);
    send1::<(), bool>(scroll, c"setAutohidesScrollers:", true);
    send1::<(), u64>(scroll, c"setBorderType:", 0);
    send1::<(), bool>(scroll, c"setHidden:", true);
    send1::<(), Id>(s.view as Id, c"addSubview:", scroll);
    with_mac(|m| {
        m.scroll = scroll as usize;
        m.outline = outline as usize;
        m.column = column as usize;
    });
    apply_tree_look();
    Some((scroll, outline))
}

/// The colours of the reader's theme on the tree.
fn apply_tree_look() {
    let Some((scroll, outline, column, Some((ground, text)))) = read_mac(|m| (m.scroll as Id, m.outline as Id, m.column as Id, m.tree_look)) else { return };
    if scroll.is_null() {
        return;
    }
    send1::<(), Id>(outline, c"setBackgroundColor:", srgb_color(ground));
    send1::<(), Id>(scroll, c"setBackgroundColor:", srgb_color(ground));
    let cell: Id = send0(column, c"dataCell");
    send1::<(), Id>(cell, c"setTextColor:", srgb_color(text));
    send1::<(), bool>(outline, c"setNeedsDisplay:", true);
}

/// Fill the tree with the bookmarks (what was in it goes): the nodes open that the list says are open, `select` selected and brought into view, and
/// without one the tree is as far scrolled as it was.
fn fill_tree(nodes: &[OutlineNode], select: Option<usize>) {
    let Some((scroll, outline)) = ensure_tree() else { return };
    let clip: Id = send0(scroll, c"contentView");
    let top = send_rect(clip, c"bounds").y;
    let mut tree = Tree::default();
    // The latest node at each depth of the branch being filled: a node goes under the latest one a level up.
    let mut path: Vec<usize> = Vec::new();
    for (i, node) in nodes.iter().enumerate() {
        let depth = (node.depth as usize).min(path.len());
        let parent = if depth == 0 { None } else { path.get(depth - 1).copied() };
        tree.parent.push(parent);
        tree.children.push(Vec::new());
        match parent.and_then(|p| tree.children.get_mut(p)) {
            Some(list) => list.push(i),
            None => tree.roots.push(i),
        }
        path.truncate(depth);
        path.push(i);
        tree.names.push(cfstr(&node.title));
        let number: Id = send1::<Id, u64>(class(c"NSNumber"), c"numberWithUnsignedLongLong:", i as u64);
        // SAFETY: a live object (autoreleased); the retain is the one the `Cf` releases.
        tree.items.push(Cf(unsafe { CFRetain(number) }));
    }
    FILLING.set(true);
    let old = TREE.with(|t| t.replace(tree));
    send0::<()>(outline, c"reloadData");
    drop(old);
    for (i, node) in nodes.iter().enumerate() {
        if node.open {
            send1::<(), Id>(outline, c"expandItem:", item_of(i));
        }
    }
    let row: i64 = match select {
        Some(i) => send1(outline, c"rowForItem:", item_of(i)),
        None => -1,
    };
    if row >= 0 {
        let rows: Id = send1(class(c"NSIndexSet"), c"indexSetWithIndex:", row as u64);
        send2::<(), Id, bool>(outline, c"selectRowIndexes:byExtendingSelection:", rows, false);
        send1::<(), i64>(outline, c"scrollRowToVisible:", row);
    } else {
        send0::<()>(outline, c"deselectAll:");
        send1::<(), CgPoint>(clip, c"scrollToPoint:", CgPoint { x: 0.0, y: top });
        send1::<(), Id>(scroll, c"reflectScrolledClipView:", clip);
    }
    FILLING.set(false);
}

/// A title is typed over (the reader asked: the file allows it).
fn start_rename(index: usize) {
    let Some((_, outline)) = read_mac(|m| (m.scroll, m.outline)).filter(|p| p.0 != 0) else { return };
    let outline = outline as Id;
    let row: i64 = send1(outline, c"rowForItem:", item_of(index));
    if row < 0 {
        return;
    }
    RENAMING.set(true);
    send4::<(), i64, i64, Id, bool>(outline, c"editColumn:row:withEvent:select:", 0, row, null_mut(), true);
    RENAMING.set(false);
}

// --- the pop-up menus ------------------------------------------------------------------------------------------------

/// Shows a menu at a place of the window (pixels, y from the top) and returns when it is closed; the chosen item has been sent to `menu_item` by then.
fn pop_up(menu: Id, at: (i32, i32), right_aligned: bool) {
    let s = snap();
    if s.view == 0 {
        return;
    }
    let k = 1.0 / s.scale.max(1.0);
    let width = if right_aligned { send0::<CgPoint>(menu, c"size").x } else { 0.0 };
    let point = CgPoint { x: f64::from(at.0) * k - width, y: s.pts.1 - f64::from(at.1) * k };
    send3::<i8, Id, CgPoint, Id>(menu, c"popUpMenuPositioningItem:atLocation:inView:", null_mut(), point, s.view as Id);
}

fn context_menu(entries: &[MenuEntry], at: (i32, i32)) {
    let s = snap();
    let menu = build_menu("", entries, s.delegate as Id);
    pop_up(menu, at, false);
    release(menu);
}

/// A drop-down list: the number of the item chosen, or none.
fn choose(items: &[String], selected: Option<usize>, at: (i32, i32)) -> Option<usize> {
    let s = snap();
    let entries: Vec<MenuEntry> = items
        .iter()
        .take(512)
        .enumerate()
        .map(|(i, item)| MenuEntry::Item { id: i as u32 + 1, label: item.replace('&', "&&"), checked: selected == Some(i), radio: false, enabled: true })
        .collect();
    let menu = build_menu("", &entries, s.delegate as Id);
    CHOSEN.set(None);
    CHOOSING.set(true);
    pop_up(menu, at, false);
    CHOOSING.set(false);
    release(menu);
    // The item's number is its place plus one.
    CHOSEN.take().and_then(|n| usize::try_from(n).ok()).and_then(|n| n.checked_sub(1))
}

/// The menu bar's menus as one pop-up (for the menu button of the command bar), the menus as submenus.
fn show_menus(at: (i32, i32)) {
    let s = snap();
    let root = build_menu("", &[], s.delegate as Id);
    let item_class = class(c"NSMenuItem");
    MENUS.with(|menus| {
        for menu in menus.borrow().iter() {
            let (title, empty) = (cfstr(&clean_label(&menu.title)), cfstr(""));
            let allocated: Id = send0(item_class, c"alloc");
            let item: Id = send3(allocated, c"initWithTitle:action:keyEquivalent:", title.0, null::<c_void>(), empty.0);
            let sub = build_menu(&menu.title, &menu.entries, s.delegate as Id);
            send1::<(), Id>(item, c"setSubmenu:", sub);
            send1::<(), Id>(root, c"addItem:", item);
            release(item);
            release(sub);
        }
    });
    pop_up(root, at, true);
    release(root);
}

// --- the window ------------------------------------------------------------------------------------------------------

/// The height of the main screen, in points: what the places of windows (from the top) are counted from.
fn main_screen_height() -> f64 {
    let screens: Id = send0(class(c"NSScreen"), c"screens");
    array_items(screens).first().map_or(0.0, |&s| send_rect(s, c"frame").h)
}

/// The work areas of the screens (less the menu bar and the Dock), the main one first, in points from the top left of the main screen.
fn work_areas() -> Vec<Rect> {
    let screens: Id = send0(class(c"NSScreen"), c"screens");
    let height = main_screen_height();
    array_items(screens)
        .into_iter()
        .map(|s| {
            let v = send_rect(s, c"visibleFrame");
            Rect { x: v.x.round() as i32, y: (height - v.y - v.h).round() as i32, w: v.w.round() as i32, h: v.h.round() as i32 }
        })
        .collect()
}

/// Notes where the window is, if it is in its usual state (not full screen): that is what is kept for the next run.
fn note_place() {
    let s = snap();
    if s.window == 0 {
        return;
    }
    let window = s.window as Id;
    let style: u64 = send0(window, c"styleMask");
    if style & (1 << 14) != 0 {
        return;
    }
    let zoomed = send0::<i8>(window, c"isZoomed") != 0;
    let f = send_rect(window, c"frame");
    let height = main_screen_height();
    with_mac(|m| {
        if zoomed {
            // Maximized: the place it goes back to is the one it had before.
            if let Some(p) = m.normal.as_mut() {
                p.maximized = true;
            }
        } else {
            m.normal = Some(Placement { x: f.x.round() as i32, y: (height - f.y - f.h).round() as i32, w: f.w.round() as i32, h: f.h.round() as i32, maximized: false });
        }
    });
}

/// Keeps the window's place for the next run (not for the speed test or the other hidden tests).
fn save_placement() {
    note_place();
    if let Some(Some(place)) = read_mac(|m| m.normal.filter(|_| m.remember)) {
        qingpdf_view::settings::update(|s| s.window = Some(place));
    }
}

fn set_full_screen(on: bool) {
    FULL_ASKED.set(on);
    let s = snap();
    if s.window == 0 {
        return;
    }
    let style: u64 = send0(s.window as Id, c"styleMask");
    if (style & (1 << 14) != 0) != on {
        send1::<(), Id>(s.window as Id, c"toggleFullScreen:", null_mut());
    }
}

/// The person went full screen or back with the system's button or keys: the reader is told with its own key for it.
/// Whether the window is in full screen (the window's buttons are gone there, so the reader keeps no room for them).
pub fn full_screen() -> bool {
    FULL_ASKED.get()
}

fn full_screen_changed(on: bool) {
    if FULL_ASKED.replace(on) != on {
        dispatch(Event::Key { vk: vk::F11, ctrl: false, shift: false });
    }
}

extern "C" fn delegate_entered_full(_this: Id, _sel: Sel, _note: Id) {
    full_screen_changed(true);
}

extern "C" fn delegate_left_full(_this: Id, _sel: Sel, _note: Id) {
    full_screen_changed(false);
    place_buttons();
}

/// The reader's colours on the window: the strip's colour behind the title bar, light or dark controls (menus, boxes, the scroll bars of the tree).
fn apply_theme(spec: ThemeSpec) {
    with_mac(|m| {
        m.theme = Some((spec.text, spec.field_bg));
        m.tree_look = Some((spec.tree_bg, spec.text));
    });
    let s = snap();
    if s.window != 0 {
        let window = s.window as Id;
        let name = cfstr(if spec.dark { "NSAppearanceNameDarkAqua" } else { "NSAppearanceNameAqua" });
        let appearance: Id = send1(class(c"NSAppearance"), c"appearanceNamed:", name.0);
        send1::<(), Id>(window, c"setAppearance:", appearance);
        send1::<(), Id>(window, c"setBackgroundColor:", srgb_color(spec.strip));
    }
    apply_tree_look();
}

// --- what the system says --------------------------------------------------------------------------------------------

/// Files opened with the program from Finder or the Dock while it runs (or that started it): they open as the files dropped on the window do.
extern "C" fn delegate_open_files(_this: Id, _sel: Sel, app: Id, files: Id) {
    let paths: Vec<PathBuf> = array_items(files).into_iter().map(|s| PathBuf::from(rust_string(s))).collect();
    if !paths.is_empty() {
        dispatch(Event::Drop(paths));
    }
    // NSApplicationDelegateReplySuccess.
    send1::<(), u64>(app, c"replyToOpenOrPrint:", 0);
}

/// The system's look changed (dark or light, the accent colour, more contrast): the system is asked again a moment later, when it has settled.
extern "C" fn delegate_system_changed(this: Id, _sel: Sel, _note: Id) {
    let look = sel(c"systemLook");
    send3::<(), Id, Sel, Id>(class(c"NSObject"), c"cancelPreviousPerformRequestsWithTarget:selector:object:", this, look, null_mut());
    send3::<(), Sel, Id, f64>(this, c"performSelector:withObject:afterDelay:", look, null_mut(), 0.3);
}

extern "C" fn delegate_system_look(_this: Id, _sel: Sel) {
    dispatch(Event::SystemDark(system_uses_dark()));
    dispatch(Event::SystemAccent(system_accent()));
    dispatch(Event::HighContrast(high_contrast()));
}

/// Asks to be told when the system's look changes.
fn observe_system(delegate: Id) {
    let note = sel(c"systemChanged:");
    let distributed: Id = send0(class(c"NSDistributedNotificationCenter"), c"defaultCenter");
    for name in ["AppleInterfaceThemeChangedNotification", "AppleColorPreferencesChangedNotification"] {
        let name = cfstr(name);
        send4::<(), Id, Sel, Id, Id>(distributed, c"addObserver:selector:name:object:", delegate, note, name.0, null_mut());
    }
    let workspace: Id = send0(class(c"NSWorkspace"), c"sharedWorkspace");
    let center: Id = send0(workspace, c"notificationCenter");
    let name = cfstr("NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification");
    send4::<(), Id, Sel, Id, Id>(center, c"addObserver:selector:name:object:", delegate, note, name.0, null_mut());
}

// --- starting up -----------------------------------------------------------------------------------------------------

/// Wakes the window's loop from any thread (the engine's thread uses it). Holds the delegate, which lives as long as the program.
#[derive(Clone, Copy)]
pub struct Waker(usize);

impl Waker {
    pub fn wake(self) {
        // SAFETY: the pool is pushed and popped around the call, as a thread that Cocoa did not start must do.
        let pool = unsafe { objc_autoreleasePoolPush() };
        send3::<(), Sel, Id, bool>(self.0 as Id, c"performSelectorOnMainThread:withObject:waitUntilDone:", sel(c"wakeUp"), null_mut(), false);
        // SAFETY: the pool just pushed, popped once.
        unsafe { objc_autoreleasePoolPop(pool) };
    }
}

/// Make the window and run it until it is closed (see `win32::run`). The window's place (in points from the top left of the main screen) is
/// put back from `saved` where it can be seen, and kept at `Action::Quit` if `remember`.
pub fn run(title: &str, client: Option<(i32, i32)>, saved: Option<Placement>, remember: bool, make: impl FnOnce(Waker) -> Box<dyn Handler>) -> i32 {
    // SAFETY: the pool lives to the end of this function, which is where the program's main thread leaves its work.
    let pool = unsafe { objc_autoreleasePoolPush() };
    let app: Id = send0(class(c"NSApplication"), c"sharedApplication");
    send1::<i8, i64>(app, c"setActivationPolicy:", 0);
    // The reader's View menu has its own "Full screen"; without this AppKit adds a second "Enter Full Screen" item to it.
    let defaults: Id = send0(class(c"NSUserDefaults"), c"standardUserDefaults");
    send2::<(), bool, Id>(defaults, c"setBool:forKey:", false, cfstr("NSFullScreenMenuItemEverywhere").0);
    // The reader reads its own command line; without this AppKit also opens every argument that is not a switch as a file, and the
    // output file of `--print-test` (not there yet) ends in an alert that waits for ever.
    let no = cfstr("NO");
    send2::<(), Id, Id>(defaults, c"setObject:forKey:", no.0, cfstr("NSTreatUnknownArgumentsAsOpen").0);
    let (view_class, delegate_class, field_class) = register_classes();
    if view_class.is_null() || delegate_class.is_null() || field_class.is_null() {
        return 1;
    }
    let delegate: Id = send0(send0(delegate_class, c"alloc"), c"init");
    send1::<(), Id>(app, c"setDelegate:", delegate);

    let screen: Id = send0(class(c"NSScreen"), c"mainScreen");
    let screen_scale: f64 = send0(screen, c"backingScaleFactor");
    let screen_scale = if screen_scale >= 1.0 { screen_scale } else { 1.0 };
    let visible = send_rect(screen, c"visibleFrame");
    let (cw, ch) = match client {
        Some((w, h)) => (f64::from(w) / screen_scale, f64::from(h) / screen_scale),
        None => ((visible.w * 0.8).max(480.0), (visible.h * 0.8).max(360.0)),
    };
    let frame = CgRect { x: 0.0, y: 0.0, w: cw, h: ch };
    // Titled, closable, miniaturizable, resizable; buffered.
    let allocated: Id = send0(class(c"NSWindow"), c"alloc");
    // Titled, closable, miniaturizable, resizable and full size content (1 << 15): the reader's tab strip is the title bar, the window's buttons sit in it.
    let window: Id = send4::<Id, CgRect, u64, u64, bool>(allocated, c"initWithContentRect:styleMask:backing:defer:", frame, 15 | (1 << 15), 2, false);
    let allocated: Id = send0(view_class, c"alloc");
    let view: Id = send1(allocated, c"initWithFrame:", frame);
    let name = cfstr(title);
    send1::<(), Id>(window, c"setTitle:", name.0);
    send1::<(), bool>(window, c"setTitlebarAppearsTransparent:", true);
    // NSWindowTitleHidden; the title is still set for Mission Control and the Window menu.
    send1::<(), i64>(window, c"setTitleVisibility:", 1);
    send1::<(), Id>(window, c"setDelegate:", delegate);
    send1::<(), Id>(window, c"setContentView:", view);
    send1::<(), bool>(window, c"setAcceptsMouseMovedEvents:", true);
    send1::<(), bool>(window, c"setReleasedWhenClosed:", false);
    // Mouse moves and leaving, whatever the window's state.
    let allocated: Id = send0(class(c"NSTrackingArea"), c"alloc");
    let area: Id = send4::<Id, CgRect, u64, Id, Id>(allocated, c"initWithRect:options:owner:userInfo:", CgRect::default(), 0x283, view, null_mut());
    send1::<(), Id>(view, c"addTrackingArea:", area);
    release(area);
    let files = cfstr("NSFilenamesPboardType");
    let kinds = cf_array(&[files.0]);
    send1::<(), Id>(view, c"registerForDraggedTypes:", kinds.0);
    send1::<(), u64>(window, c"setCollectionBehavior:", 1 << 7);
    // Where the window was when it was last closed, if that can still be seen; else the middle of the screen.
    let mut maximized = false;
    match saved.filter(|_| client.is_none()).and_then(|p| placement::restore(p, &work_areas())) {
        Some(at) => {
            let whole = CgRect { x: f64::from(at.x), y: main_screen_height() - f64::from(at.y + at.h), w: f64::from(at.w), h: f64::from(at.h) };
            send2::<(), CgRect, bool>(window, c"setFrame:display:", whole, false);
            maximized = at.maximized;
        }
        None => send0::<()>(window, c"center"),
    }
    // SAFETY: a Core Graphics constant.
    let space = unsafe { CGColorSpaceCreateWithName(kCGColorSpaceSRGB) };
    with_mac(|m| {
        m.window = window as usize;
        m.view = view as usize;
        m.delegate = delegate as usize;
        m.space = space as usize;
        m.scale = screen_scale;
        m.field_class = field_class as usize;
        m.remember = remember;
    });
    note_place();
    if maximized {
        with_mac(|m| m.normal = saved.map(|p| Placement { maximized: true, ..p }).or(m.normal));
    }
    observe_system(delegate);
    install_menus(&[]);

    let handler = make(Waker(delegate as usize));
    HANDLER.with(|h| *h.borrow_mut() = Some(handler));
    let scale: f64 = send0(window, c"backingScaleFactor");
    dispatch(Event::Dpi((96.0 * scale.max(1.0)).round() as u32));
    dispatch(Event::Motion(true));
    // The reader draws the title strip; the window's buttons are the system's, at the left (kept free by `BUTTONS_ROOM`), none at the right.
    dispatch(Event::Frame { custom: true, caption: 0 });
    set_app_icon();
    with_mac(|m| m.told_dpi = (96.0 * scale.max(1.0)).round() as u32);
    sync_size();
    place_buttons();

    send1::<(), Id>(window, c"makeKeyAndOrderFront:", null_mut());
    send1::<(), Id>(window, c"makeFirstResponder:", view);
    if maximized {
        send1::<(), Id>(window, c"zoom:", null_mut());
    }
    send1::<(), bool>(app, c"activateIgnoringOtherApps:", true);
    send0::<()>(app, c"run");

    // The reader is dropped before the pool and the window go.
    drop(HANDLER.with(|h| h.borrow_mut().take()));
    // SAFETY: the pool pushed at the start; the colour space made above.
    unsafe {
        CGColorSpaceRelease(space);
        objc_autoreleasePoolPop(pool);
    }
    0
}

// --- the small ones --------------------------------------------------------------------------------------------------

/// The language of the person as a Windows language id (0x0804 Chinese, 0x0409 anything else): what `Lang::from_langid` tells Chinese by.
pub fn ui_language() -> u16 {
    // SAFETY: returns a new array (or null), released below.
    let languages = Cf(unsafe { CFLocaleCopyPreferredLanguages() });
    let first = array_items(languages.0).first().map(|&s| rust_string(s)).unwrap_or_default();
    if first.starts_with("zh") { 0x0804 } else { 0x0409 }
}

/// The full name of the user: the author of the annotations until the person says another.
pub fn user_name() -> String {
    // SAFETY: returns an autoreleased string (the pool of `run` holds it).
    rust_string(unsafe { NSFullUserName() })
}

pub fn random_bytes(buffer: &mut [u8]) -> bool {
    // SAFETY: `buffer` is live and writable for its whole length, which is what is told; a null generator is the default one (0 is success).
    unsafe { SecRandomCopyBytes(null(), buffer.len(), buffer.as_mut_ptr()) == 0 }
}

fn civil(utc_secs: i64, local: bool) -> String {
    let mut tm = Tm { sec: 0, min: 0, hour: 0, mday: 0, mon: 0, year: 0, wday: 0, yday: 0, isdst: 0, gmtoff: 0, zone: null() };
    // SAFETY: `tm` is live and the size `localtime_r` writes; the time is read from a live number.
    let ok = unsafe { !localtime_r(&utc_secs, &mut tm).is_null() };
    if !ok || !local {
        return String::new();
    }
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", tm.year + 1900, tm.mon + 1, tm.mday, tm.hour, tm.min, tm.sec)
}

/// Today's date in the person's time zone, as `2026-10-15`.
pub fn local_date() -> String {
    // SAFETY: a null place to also store the time in is allowed.
    let now = unsafe { time(null_mut()) };
    civil(now, true).chars().take(10).collect()
}

/// A time (seconds since 1970, UTC) in the person's time zone, as `2026-10-15 14:03:09`.
pub fn local_time(utc_secs: i64) -> String {
    civil(utc_secs, true)
}

/// The most memory this process has had in use, in bytes (0 if the system will not say).
pub fn peak_working_set() -> u64 {
    // The most physical memory the program has had to itself (`ri_lifetime_max_phys_footprint` of `rusage_info_v4`, what Activity
    // Monitor shows). Not `ru_maxrss`: that counts every page of the system's shared libraries the program has touched, which on
    // macOS is some 400 MB that every program shares.
    let mut info = RusageInfo { uuid: [0; 16], fields: [0; 40] };
    // SAFETY: `info` is live and bigger than a `rusage_info_v4` (16 bytes and 35 64-bit fields), which is all flavour 4 writes; the
    // process is this one. 0 is success.
    let ok = unsafe { proc_pid_rusage(getpid(), 4, (&raw mut info).cast()) } == 0;
    if ok { info.fields[28] } else { 0 }
}

#[repr(C)]
struct RusageInfo {
    uuid: [u8; 16],
    fields: [u64; 40],
}

/// A macOS program started from a terminal has its console already.
pub fn attach_console() {}

/// The identifier of the bundle this program runs from (empty when it is not in an `.app`, as when run from the build folder).
fn bundle_id() -> String {
    let bundle: Id = send0(class(c"NSBundle"), c"mainBundle");
    rust_string(send0(bundle, c"bundleIdentifier"))
}

/// One window for all the files, as on Windows. For the `.app` Launch Services is what makes it one program (opening a file in a running copy sends it
/// `application:openFiles:`); this is for the program started by hand from inside the bundle: it looks for another copy by the bundle identifier. A program
/// that is not in a bundle has none to look for: it is always the first.
pub fn first_copy() -> bool {
    let id = bundle_id();
    if id.is_empty() {
        return true;
    }
    let name = cfstr(&id);
    let apps: Id = send1(class(c"NSRunningApplication"), c"runningApplicationsWithBundleIdentifier:", name.0);
    // SAFETY: no arguments; the id of this process.
    let me = unsafe { getpid() };
    !array_items(apps).into_iter().any(|a| send0::<i32>(a, c"processIdentifier") != me)
}

/// Gives the files to the copy that is running (Launch Services sends it the same message a Finder double click does); false if there is none.
pub fn hand_over(paths: &[PathBuf]) -> bool {
    let id = bundle_id();
    if id.is_empty() || first_copy() {
        return false;
    }
    let urls: Vec<Cf> = paths
        .iter()
        .map(|p| {
            let s = cfstr(&p.to_string_lossy());
            let url: Id = send1(class(c"NSURL"), c"fileURLWithPath:", s.0);
            // SAFETY: a live (autoreleased) object; the retain is the one the `Cf` releases.
            Cf(unsafe { CFRetain(url) })
        })
        .collect();
    let list = cf_array(&urls.iter().map(|u| u.0).collect::<Vec<_>>());
    let name = cfstr(&id);
    let workspace: Id = send0(class(c"NSWorkspace"), c"sharedWorkspace");
    // 0x10000 is NSWorkspaceLaunchAsync; no extra descriptor and no launch identifiers wanted.
    send5::<i8, Id, Id, u64, Id, *mut Id>(workspace, c"openURLs:withAppBundleIdentifier:options:additionalEventParamDescriptor:launchIdentifiers:", list.0, name.0, 0x10000, null_mut(), null_mut()) != 0
}

pub fn system_uses_dark() -> bool {
    // What the application looks like now (it follows the system, also when it changes by itself at night).
    let app: Id = send0(class(c"NSApplication"), c"sharedApplication");
    let appearance: Id = if app.is_null() { null_mut() } else { send0(app, c"effectiveAppearance") };
    if !appearance.is_null() {
        let names = cf_array_of_strings(&["NSAppearanceNameAqua".to_string(), "NSAppearanceNameDarkAqua".to_string()]);
        let best = rust_string(send1(appearance, c"bestMatchFromAppearancesWithNames:", names.0));
        if !best.is_empty() {
            return best.contains("Dark");
        }
    }
    let defaults: Id = send0(class(c"NSUserDefaults"), c"standardUserDefaults");
    let key = cfstr("AppleInterfaceStyle");
    let style: Id = send1(defaults, c"stringForKey:", key.0);
    rust_string(style) == "Dark"
}

pub fn system_accent() -> Option<Rgb> {
    let color: Id = send0(class(c"NSColor"), c"controlAccentColor");
    let space: Id = send0(class(c"NSColorSpace"), c"sRGBColorSpace");
    let srgb: Id = send1(color, c"colorUsingColorSpace:", space);
    if srgb.is_null() {
        return None;
    }
    let (mut r, mut g, mut b, mut a) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    send4::<(), *mut f64, *mut f64, *mut f64, *mut f64>(srgb, c"getRed:green:blue:alpha:", &mut r, &mut g, &mut b, &mut a);
    let byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    Some(byte(r) << 16 | byte(g) << 8 | byte(b))
}

/// "Increase contrast" of the system's accessibility settings: black on white, or white on black when the system is dark, with the accent for the selection.
pub fn high_contrast() -> Option<Contrast> {
    let workspace: Id = send0(class(c"NSWorkspace"), c"sharedWorkspace");
    if send0::<i8>(workspace, c"accessibilityDisplayShouldIncreaseContrast") == 0 {
        return None;
    }
    let accent = system_accent().unwrap_or(0x0A84FF);
    Some(if system_uses_dark() {
        Contrast { window: 0x000000, text: 0xFFFFFF, gray: 0xB0B0B0, highlight: accent, highlight_text: 0x000000, face: 0x000000 }
    } else {
        Contrast { window: 0xFFFFFF, text: 0x000000, gray: 0x505050, highlight: accent, highlight_text: 0xFFFFFF, face: 0xFFFFFF }
    })
}

/// The size of the part of the main screen the windows may use, in pixels.
pub fn work_area_size() -> (i32, i32) {
    let screen: Id = send0(class(c"NSScreen"), c"mainScreen");
    if screen.is_null() {
        return (1920, 1080);
    }
    let scale: f64 = send0(screen, c"backingScaleFactor");
    let visible = send_rect(screen, c"visibleFrame");
    ((visible.w * scale).round() as i32, (visible.h * scale).round() as i32)
}

/// The box shown when the reader has failed in a way it cannot go on from.
pub fn fatal_box(title: &str, text: &str) {
    eprintln!("{title}: {text}");
    // SAFETY: no arguments; says whether this is the main thread, the only one that may show a box.
    if unsafe { pthread_main_np() } != 0 {
        let _: Id = send0(class(c"NSApplication"), c"sharedApplication");
        alert(title, text, &["OK"]);
    }
}

// --- the form box (`Action::AskForm`) -------------------------------------------------------------------------------
//
// An NSAlert (its own modal loop, the reader's icon, OK and Cancel at its foot) whose accessory view holds the system's controls at the
// places `dlgstyle::lay_out` gives (a 96th of an inch is a point). A `Button` item closes the box with `stopModalWithCode:` (2000 and
// its number among the buttons). The sketch of a page is a view of ours that reads the controls when it draws; a timer in the common
// run-loop modes (which the modal loop is in) draws it again five times a second, so it follows what is typed.

/// What the sketch of the open form box draws from (the controls are the box's, alive while it is open).
struct FormSketch {
    shape: (i32, i32),
    slots: [usize; 6],
    mirror: usize,
    fields: [String; 4],
    left_page: bool,
}

thread_local! {
    static SKETCH: RefCell<Option<FormSketch>> = const { RefCell::new(None) };
}

/// The most characters taken from a line of a form box (as `win32.rs`).
const FORM_MAX_TEXT: usize = 4_000;
/// What `runModal` answers for a `Button` item (plus its number).
const FORM_BUTTON_CODE: i64 = 2000;

/// The class `name`, made with `methods` the first time.
fn class_once(name: &CStr, superclass: &CStr, methods: &[Method]) -> Id {
    let existing = class(name);
    if existing.is_null() { make_class(name, superclass, methods) } else { existing }
}

extern "C" fn flipped_yes(_this: Id, _sel: Sel) -> i8 {
    1
}

extern "C" fn form_button_pressed(_this: Id, _sel: Sel, sender: Id) {
    let tag: i64 = send0(sender, c"tag");
    let app: Id = send0(class(c"NSApplication"), c"sharedApplication");
    send1::<(), i64>(app, c"stopModalWithCode:", FORM_BUTTON_CODE + tag);
}

extern "C" fn sketch_tick(this: Id, _sel: Sel, _timer: Id) {
    send1::<(), bool>(this, c"setNeedsDisplay:", true);
}

/// The text of a control (at most `FORM_MAX_TEXT` characters).
fn control_text(control: usize) -> String {
    if control == 0 {
        return String::new();
    }
    rust_string(send0(control as Id, c"stringValue")).chars().take(FORM_MAX_TEXT).collect()
}

/// Width and height of `text` in the system font of `size`.
fn text_attributes(size: f64, color: Rgb) -> Id {
    let font: Id = send1::<Id, f64>(class(c"NSFont"), c"systemFontOfSize:", size);
    // SAFETY: the two keys are AppKit's constant strings, alive for the whole program.
    let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    let values = [font, srgb_color(color)];
    send3::<Id, *const Id, *const Id, usize>(class(c"NSDictionary"), c"dictionaryWithObjects:forKeys:count:", values.as_ptr(), keys.as_ptr(), 2)
}

extern "C" fn sketch_draw(this: Id, _sel: Sel, _dirty: CgRect) {
    let bounds = send_rect(this, c"bounds");
    let graphics: Id = send0(class(c"NSGraphicsContext"), c"currentContext");
    let ctx: Id = send0(graphics, c"CGContext");
    if ctx.is_null() {
        return;
    }
    SKETCH.with(|s| {
        let Ok(s) = s.try_borrow() else { return };
        let Some(s) = s.as_ref() else { return };
        let (rw, rh) = (bounds.w, bounds.h);
        let (pw, ph) = (f64::from(s.shape.0.max(1)), f64::from(s.shape.1.max(1)));
        let (w, h) = if rw * ph <= rh * pw { (rw, rw * ph / pw) } else { (rh * pw / ph, rh) };
        let page = CgRect { x: (rw - w) / 2.0, y: (rh - h) / 2.0, w, h };
        // SAFETY: the view's own drawing context, alive through `drawRect:`; plain rectangles.
        unsafe {
            CGContextSetRGBFillColor(ctx, 0.5, 0.5, 0.5, 1.0);
            CGContextFillRect(ctx, page);
            CGContextSetRGBFillColor(ctx, 1.0, 1.0, 1.0, 1.0);
            CGContextFillRect(ctx, CgRect { x: page.x + 1.0, y: page.y + 1.0, w: page.w - 2.0, h: page.h - 2.0 });
        }
        let mirrored = s.left_page && s.mirror != 0 && send0::<i64>(s.mirror as Id, c"state") == 1;
        let attributes = text_attributes(9.0, 0x303030);
        let f = &s.fields;
        for i in 0..6 {
            let at = if mirrored && i % 3 != 1 { i / 3 * 3 + 2 - i % 3 } else { i };
            let text = control_text(s.slots.get(at).copied().unwrap_or(0)).replace("{page}", &f[0]).replace("{pages}", &f[1]).replace("{date}", &f[2]).replace("{file}", &f[3]);
            if text.is_empty() {
                continue;
            }
            let t = cfstr(&text);
            let size: CgSize = send1(t.0, c"sizeWithAttributes:", attributes);
            let x = match i % 3 {
                0 => page.x + 6.0,
                1 => page.x + (page.w - size.w) / 2.0,
                _ => page.x + page.w - 6.0 - size.w,
            };
            let y = if i < 3 { page.y + 6.0 } else { page.y + page.h - 6.0 - size.h };
            send2::<(), CgPoint, Id>(t.0, c"drawAtPoint:withAttributes:", CgPoint { x, y }, attributes);
        }
    });
}

/// Ask for several things (`Action::AskForm`); none if it was cancelled.
fn ask_form(spec: &FormSpec) -> Option<FormAnswer> {
    let flipped = class_once(c"QingpdfFlipped", c"NSView", &[(c"isFlipped", flipped_yes as *const c_void, c"c@:")]);
    let sketch_class = class_once(
        c"QingpdfSketch",
        c"NSView",
        &[(c"isFlipped", flipped_yes as *const c_void, c"c@:"), (c"drawRect:", sketch_draw as *const c_void, c"v@:{CGRect=dddd}"), (c"tick:", sketch_tick as *const c_void, c"v@:@")],
    );
    let target_class = class_once(c"QingpdfFormTarget", c"NSObject", &[(c"pressed:", form_button_pressed as *const c_void, c"v@:@")]);
    if flipped.is_null() || sketch_class.is_null() || target_class.is_null() {
        return None;
    }
    let items: Vec<(FormKind, (i32, i32, i32, i32))> = spec.items.iter().map(|i| (i.kind, if i.kind == FormKind::Label && i.text.is_empty() { (i.rect.0, i.rect.1, i.rect.2, 0) } else { i.rect })).collect();
    let laid = qingpdf_view::dlgstyle::lay_out(&items, spec.preview.as_ref().map(|p| p.rect), &spec.ok, &spec.cancel);
    // The accessory view holds what is above the foot (NSAlert has its own margins and buttons).
    let pad = qingpdf_view::dlgstyle::PAD;
    let top = laid.rects.iter().chain(laid.preview.iter()).filter(|r| r.3 > 0).map(|r| r.1).min().unwrap_or(pad);
    let bottom = laid.rects.iter().chain(laid.preview.iter()).map(|r| r.1 + r.3).max().unwrap_or(top);
    let width = (laid.size.0 - 2 * pad).max(200);
    let frame = |r: (i32, i32, i32, i32), h: Option<f64>| {
        let full = f64::from(r.3);
        let h = h.map_or(full, |h| h.min(full.max(h)));
        CgRect { x: f64::from(r.0 - pad), y: f64::from(r.1 - top) + (full - h).max(0.0) / 2.0, w: f64::from(r.2), h }
    };
    let content: Id = send1(send0(flipped, c"alloc"), c"initWithFrame:", CgRect { x: 0.0, y: 0.0, w: f64::from(width), h: f64::from((bottom - top).max(1)) });
    let target = new_object_of(target_class);
    let mut made: Vec<(FormKind, usize)> = Vec::with_capacity(spec.items.len());
    let (mut buttons, mut focus): (i64, Id) = (0, null_mut());
    for (k, item) in spec.items.iter().enumerate() {
        let r = laid.rects.get(k).copied().unwrap_or(item.rect);
        let text = cfstr(&item.text);
        let control: Id = match item.kind {
            FormKind::Label | FormKind::Group => {
                let label: Id = if r.3 > qingpdf_view::dlgstyle::LABEL_H {
                    send1(class(c"NSTextField"), c"wrappingLabelWithString:", text.0)
                } else {
                    send1(class(c"NSTextField"), c"labelWithString:", text.0)
                };
                // The layout is made for the 12-pixel font of the Windows boxes; the system's 13 points would cut words like "Middle".
                let font: Id = send1::<Id, f64>(class(c"NSFont"), if item.kind == FormKind::Group { c"boldSystemFontOfSize:" } else { c"systemFontOfSize:" }, 12.0);
                send1::<(), Id>(label, c"setFont:", font);
                send1::<(), bool>(label, c"setAllowsDefaultTighteningForTruncation:", true);
                // A line of words wider than its place is set a little smaller (down to 10 points) rather than cut.
                let fits: CgSize = send0(label, c"fittingSize");
                if r.3 <= qingpdf_view::dlgstyle::LABEL_H && fits.w > f64::from(r.2) && fits.w > 0.0 {
                    let size = (12.0 * f64::from(r.2) / fits.w).max(10.0);
                    let smaller: Id = send1::<Id, f64>(class(c"NSFont"), if item.kind == FormKind::Group { c"boldSystemFontOfSize:" } else { c"systemFontOfSize:" }, size);
                    send1::<(), Id>(label, c"setFont:", smaller);
                }
                send1::<(), CgRect>(label, c"setFrame:", frame(r, None));
                label
            }
            FormKind::Edit | FormKind::Secret => {
                let c: Id = send1(send0(class(if item.kind == FormKind::Secret { c"NSSecureTextField" } else { c"NSTextField" }), c"alloc"), c"initWithFrame:", frame(r, Some(22.0)));
                if item.kind == FormKind::Edit {
                    send1::<(), Id>(c, c"setStringValue:", text.0);
                }
                let _: Id = send0(c, c"autorelease");
                c
            }
            FormKind::Combo => {
                let c: Id = send1(send0(class(c"NSComboBox"), c"alloc"), c"initWithFrame:", frame(r, Some(26.0)));
                let choices = cf_array_of_strings(&item.choices);
                send1::<(), Id>(c, c"addItemsWithObjectValues:", choices.0);
                send1::<(), Id>(c, c"setStringValue:", text.0);
                let _: Id = send0(c, c"autorelease");
                c
            }
            FormKind::Pick => {
                let c: Id = send2(send0(class(c"NSPopUpButton"), c"alloc"), c"initWithFrame:pullsDown:", frame(r, Some(26.0)), false);
                let choices = cf_array_of_strings(&item.choices);
                send1::<(), Id>(c, c"addItemsWithTitles:", choices.0);
                if item.selected < item.choices.len() {
                    send1::<(), i64>(c, c"selectItemAtIndex:", item.selected as i64);
                }
                let _: Id = send0(c, c"autorelease");
                c
            }
            FormKind::Check => {
                let c: Id = send3(class(c"NSButton"), c"checkboxWithTitle:target:action:", text.0, null_mut::<c_void>(), null::<c_void>());
                send1::<(), i64>(c, c"setState:", i64::from(item.selected == 1));
                send1::<(), CgRect>(c, c"setFrame:", frame(r, Some(18.0)));
                c
            }
            FormKind::Button => {
                let c: Id = send3(class(c"NSButton"), c"buttonWithTitle:target:action:", text.0, target, sel(c"pressed:"));
                send1::<(), i64>(c, c"setTag:", buttons);
                buttons += 1;
                send1::<(), CgRect>(c, c"setFrame:", frame(r, Some(28.0)));
                c
            }
        };
        if !control.is_null() {
            send1::<(), Id>(content, c"addSubview:", control);
            if focus.is_null() && matches!(item.kind, FormKind::Edit | FormKind::Secret | FormKind::Combo) {
                focus = control;
            }
        }
        made.push((item.kind, control as usize));
    }
    let mut timer: Id = null_mut();
    if let (Some(p), Some(r)) = (&spec.preview, laid.preview) {
        let handle = |k: usize| made.get(k).map_or(0, |m| m.1);
        SKETCH.with(|s| *s.borrow_mut() = Some(FormSketch { shape: p.shape, slots: p.slots.map(handle), mirror: handle(p.mirror), fields: p.fields.clone(), left_page: p.left_page }));
        let view: Id = send1(send0(sketch_class, c"alloc"), c"initWithFrame:", frame(r, None));
        send1::<(), Id>(content, c"addSubview:", view);
        release(view);
        timer = send5::<Id, f64, Id, Sel, Id, bool>(class(c"NSTimer"), c"timerWithTimeInterval:target:selector:userInfo:repeats:", 0.2, view, sel(c"tick:"), null_mut(), true);
        let run_loop: Id = send0(class(c"NSRunLoop"), c"currentRunLoop");
        // SAFETY: Foundation's constant string, alive for the whole program.
        send2::<(), Id, Id>(run_loop, c"addTimer:forMode:", timer, unsafe { NSRunLoopCommonModes });
    }

    let alert = new_object(c"NSAlert");
    set_alert_icon(alert);
    let title = cfstr(&spec.title);
    send1::<(), Id>(alert, c"setMessageText:", title.0);
    let (ok, cancel) = (cfstr(if spec.ok.is_empty() { "OK" } else { &spec.ok }), cfstr(if spec.cancel.is_empty() { "Cancel" } else { &spec.cancel }));
    let _: Id = send1(alert, c"addButtonWithTitle:", ok.0);
    let cancel_button: Id = send1(alert, c"addButtonWithTitle:", cancel.0);
    let escape = cfstr("\u{1b}");
    send1::<(), Id>(cancel_button, c"setKeyEquivalent:", escape.0);
    send1::<(), Id>(alert, c"setAccessoryView:", content);
    send0::<()>(alert, c"layout");
    if !focus.is_null() {
        let window: Id = send0(alert, c"window");
        send1::<(), Id>(window, c"setInitialFirstResponder:", focus);
    }
    let pressed: i64 = send0(alert, c"runModal");
    if !timer.is_null() {
        send0::<()>(timer, c"invalidate");
    }
    SKETCH.with(|s| *s.borrow_mut() = None);

    let button = (pressed >= FORM_BUTTON_CODE).then(|| (pressed - FORM_BUTTON_CODE) as usize);
    let accepted = pressed == 1000 || button.is_some();
    let mut values = Vec::new();
    for &(kind, control) in &made {
        let c = control as Id;
        match kind {
            FormKind::Edit | FormKind::Combo if accepted => values.push(control_text(control)),
            FormKind::Secret => {
                if accepted {
                    values.push(control_text(control));
                }
                // What was typed (a password) is wiped from the box before it goes.
                let empty = cfstr("");
                send1::<(), Id>(c, c"setStringValue:", empty.0);
            }
            FormKind::Pick if accepted => values.push(send0::<i64>(c, c"indexOfSelectedItem").max(0).to_string()),
            FormKind::Check if accepted => values.push(if send0::<i64>(c, c"state") == 1 { "1" } else { "0" }.to_string()),
            _ => {}
        }
    }
    release(alert);
    release(content);
    release(target);
    accepted.then_some(FormAnswer { values, button })
}

fn new_object_of(class: Id) -> Id {
    let allocated: Id = send0(class, c"alloc");
    send0(allocated, c"init")
}

// --- printing (`Action::ChoosePrinter`, `Action::Print`) ------------------------------------------------------------------
//
// The reader prints in bands of printer pixels, as Windows' GDI wants. Here the bands are drawn into a PDF file page by page
// (CGPDFContext, the paper's size, the bands placed in the part that can be printed on); at the end the file is given to the printer the
// person chose in the system's print panel (PrintCore's `PMPrinterPrintWithFile`, with the panel's settings: copies, paper, two-sided)
// and then deleted. The hidden `--print-test` (a preset) writes the PDF to its output file and prints nothing.

/// Dots per inch the reader is asked to draw at (the PDF is the same size whatever it is; this is the sharpness of the pictures).
const PRINT_DPI: i32 = 300;
/// A4 in points, for the preset (no printer is asked).
const A4_POINTS: (f64, f64) = (595.0, 842.0);

struct PrintJob {
    /// The NSPrintInfo the panel filled in (ours: one reference), or nil for a preset.
    info: Id,
    /// The file the PDF is written to, and whether it is the preset's own (kept) rather than a passing one (printed, then deleted).
    path: PathBuf,
    keep: bool,
    paper: (f64, f64),
    /// The part of the paper that can be printed on, in points from the bottom left.
    area: CgRect,
    /// The PDF being written (once the job has started).
    ctx: Id,
}

thread_local! {
    static PRINT: RefCell<Option<PrintJob>> = const { RefCell::new(None) };
}

fn cf_url(path: &std::path::Path) -> Cf {
    let bytes = path.as_os_str().as_encoded_bytes();
    // SAFETY: the bytes are live through the call and copied; a null allocator is the default one.
    Cf(unsafe { CFURLCreateFromFileSystemRepresentation(null_mut(), bytes.as_ptr(), bytes.len() as isize, false) })
}

/// Stop the job there is (its PDF is closed; the passing file is deleted unless it is printed: `print`).
fn end_print(print: bool) -> Result<(), String> {
    let Some(job) = PRINT.with(|p| p.borrow_mut().take()) else { return Ok(()) };
    if !job.ctx.is_null() {
        // SAFETY: the PDF context made by `Start`, closed and released once.
        unsafe {
            CGPDFContextClose(job.ctx);
            CGContextRelease(job.ctx);
        }
    }
    let mut result = Ok(());
    if print && !job.keep && !job.info.is_null() && !job.ctx.is_null() {
        let (session, settings, format): (*mut c_void, *mut c_void, *mut c_void) = (send0(job.info, c"PMPrintSession"), send0(job.info, c"PMPrintSettings"), send0(job.info, c"PMPageFormat"));
        let mut printer: *mut c_void = null_mut();
        let url = cf_url(&job.path);
        let mime = cfstr("application/pdf");
        // SAFETY: the session, settings and page format belong to `info`, which is alive; `printer` is written once and belongs to the
        // session; the URL and the type are live CF objects. The file holds only the pages asked for, so the settings' page range is
        // set to all of them. 0 is success.
        let status = unsafe {
            let mut s = PMSessionGetCurrentPrinter(session, &mut printer);
            if s == 0 {
                PMSetFirstPage(settings, 1, false);
                PMSetLastPage(settings, u32::MAX, false);
                s = PMPrinterPrintWithFile(printer, settings, format, mime.0, url.0);
            }
            s
        };
        if status != 0 {
            result = Err(format!("the printer would not take the job ({status})"));
        }
    }
    if !job.keep || (!print && !job.ctx.is_null()) {
        let _ = std::fs::remove_file(&job.path);
    }
    release(job.info);
    result
}

/// Which printer (the system's print panel, with pages and copies), or the preset's file. `Err` if there is no printer.
fn choose_printer(max_page: u32, preset: Option<PrinterPreset>) -> Result<Option<PrinterSetup>, ()> {
    let _ = end_print(false);
    let setup = |name: String, area: CgRect, ranges: Vec<(u32, u32)>| PrinterSetup {
        name,
        dpi: PRINT_DPI,
        width: (area.w * f64::from(PRINT_DPI) / 72.0).floor() as i32,
        height: (area.h * f64::from(PRINT_DPI) / 72.0).floor() as i32,
        ranges,
    };
    if let Some(preset) = preset {
        let area = CgRect { x: 0.0, y: 0.0, w: A4_POINTS.0, h: A4_POINTS.1 };
        PRINT.with(|p| *p.borrow_mut() = Some(PrintJob { info: null_mut(), path: preset.output.clone(), keep: true, paper: A4_POINTS, area, ctx: null_mut() }));
        return Ok(Some(setup(preset.name, area, preset.ranges)));
    }
    let names: Id = send0(class(c"NSPrinter"), c"printerNames");
    if array_items(names).is_empty() {
        return Err(());
    }
    let shared: Id = send0(class(c"NSPrintInfo"), c"sharedPrintInfo");
    let info: Id = send0(shared, c"copy");
    let panel: Id = send0(class(c"NSPrintPanel"), c"printPanel");
    // Copies, page range, paper size, orientation.
    send1::<(), u64>(panel, c"setOptions:", 1 | 2 | 4 | 8);
    let dictionary: Id = send0(info, c"dictionary");
    let number = |n: u32| -> Id { send1(class(c"NSNumber"), c"numberWithUnsignedInt:", n) };
    let (first_key, last_key, all_key) = (cfstr("NSFirstPage"), cfstr("NSLastPage"), cfstr("NSAllPages"));
    send2::<(), Id, Id>(dictionary, c"setObject:forKey:", number(1), first_key.0);
    send2::<(), Id, Id>(dictionary, c"setObject:forKey:", number(max_page.max(1)), last_key.0);
    let answer: i64 = send1(panel, c"runModalWithPrintInfo:", info);
    if answer != 1 {
        release(info);
        return Ok(None);
    }
    let all = send0::<i8>(send1(dictionary, c"objectForKey:", all_key.0), c"boolValue") != 0;
    let page = |key: &Cf| -> u32 { send0::<i64>(send1(dictionary, c"objectForKey:", key.0), c"integerValue").clamp(1, i64::from(max_page.max(1))) as u32 };
    let (first, last) = (page(&first_key), page(&last_key));
    let ranges = if all || (first <= 1 && last >= max_page) { Vec::new() } else { vec![(first.min(last), first.max(last))] };
    let printer: Id = send0(info, c"printer");
    let name = rust_string(send0(printer, c"name"));
    let paper: CgSize = send0(info, c"paperSize");
    let area = send_rect(info, c"imageablePageBounds");
    if paper.w <= 0.0 || paper.h <= 0.0 || area.w <= 0.0 || area.h <= 0.0 {
        release(info);
        return Err(());
    }
    let path = std::env::temp_dir().join(format!("qingpdf-print-{}.pdf", std::process::id()));
    PRINT.with(|p| *p.borrow_mut() = Some(PrintJob { info, path, keep: false, paper: (paper.w, paper.h), area, ctx: null_mut() }));
    Ok(Some(setup(name, area, ranges)))
}

/// One step of a print job. `Ok(true)`: a band was put on the page. `Err`: it could not be done.
fn print_op(op: PrintOp) -> Result<bool, String> {
    match op {
        PrintOp::End => return end_print(true).map(|()| false),
        PrintOp::Abort => return end_print(false).map(|()| false),
        _ => {}
    }
    PRINT.with(|p| {
        let mut slot = p.borrow_mut();
        // A job that was stopped is not carried on.
        let Some(job) = slot.as_mut() else { return Ok(false) };
        match op {
            PrintOp::Start { .. } => {
                let url = cf_url(&job.path);
                let media = CgRect { x: 0.0, y: 0.0, w: job.paper.0, h: job.paper.1 };
                // SAFETY: the URL is live and the box is read during the call; no extra information (null). The context is ours.
                job.ctx = unsafe { CGPDFContextCreateWithURL(url.0, &media, null_mut()) };
                if job.ctx.is_null() {
                    return Err("the printer would not start the job".to_string());
                }
                Ok(false)
            }
            PrintOp::StartPage | PrintOp::EndPage if job.ctx.is_null() => Err("the printer would not start a page".to_string()),
            PrintOp::StartPage => {
                // SAFETY: the live PDF context of the job; no page information (null).
                unsafe { CGPDFContextBeginPage(job.ctx, null_mut()) };
                Ok(false)
            }
            PrintOp::EndPage => {
                // SAFETY: the live PDF context, with a page begun.
                unsafe { CGPDFContextEndPage(job.ctx) };
                Ok(false)
            }
            PrintOp::Band { x, y, w, h, src_w, src_h, bgra } => {
                let needed = (src_w.max(0) as usize).saturating_mul(src_h.max(0) as usize).saturating_mul(4);
                if job.ctx.is_null() || src_w <= 0 || src_h <= 0 || w <= 0 || h <= 0 || bgra.len() < needed {
                    return Ok(true);
                }
                let k = 72.0 / f64::from(PRINT_DPI);
                let rect = CgRect { x: job.area.x + f64::from(x) * k, y: job.area.y + job.area.h - f64::from(y + h) * k, w: f64::from(w) * k, h: f64::from(h) * k };
                // SAFETY: `needed` bytes of `bgra` (checked) are copied into the CFData; the provider and the colour space are released
                // after the image has taken its own references; the image is src_w x src_h, 8 bits a component, 4 bytes a pixel in the
                // order B G R x (32-bit little-endian, the first byte skipped), which is what a band is; it is drawn into the live PDF
                // context and released.
                let ok = unsafe {
                    let data = CFDataCreate(null_mut(), bgra.as_ptr(), needed as isize);
                    let provider = if data.is_null() { null_mut() } else { CGDataProviderCreateWithCFData(data) };
                    if !data.is_null() {
                        CFRelease(data);
                    }
                    if provider.is_null() {
                        false
                    } else {
                        let space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
                        let image = CGImageCreate(src_w as usize, src_h as usize, 8, 32, src_w as usize * 4, space, 0x2000 | 6, provider, null(), true, 0);
                        CGColorSpaceRelease(space);
                        CGDataProviderRelease(provider);
                        if !image.is_null() {
                            CGContextDrawImage(job.ctx, rect, image);
                            CGImageRelease(image);
                        }
                        !image.is_null()
                    }
                };
                if !ok {
                    return Err("the printer would not take a part of the page".to_string());
                }
                Ok(true)
            }
            PrintOp::End | PrintOp::Abort => Ok(false),
        }
    })
}

// --- text recognition (5c): Vision -------------------------------------------------------------------------------------
//
// `VNRecognizeTextRequest` (macOS 10.15; the list of its languages needs 12) at the accurate level, run synchronously on the
// recogniser's worker thread (`qingpdf_view::ocr::Worker`), which is the only thread that touches these objects, except that a
// watcher thread of `recognize` may send `cancel` to the request, which Vision allows from any thread. Each call has its own
// autorelease pool. Vision gives lines (observations) with a string; the words are cut out of it here (a Han, kana or Hangul
// character is a word of its own, as the Windows recogniser gives Chinese; other text is cut at white space) and each word's box is
// asked of Vision (`boundingBoxForRange:error:`), or, when Vision has none, is the word's share of the line's box.

/// The largest side of a picture given to Vision (no limit is documented; this holds an A4 page at 350 dpi, and the memory of one picture
/// to 16 MB).
const VISION_MAX_SIDE: u32 = 4096;
/// Most lines taken from one page (as `win32.rs`).
const VISION_MAX_LINES: usize = 10_000;

#[repr(C)]
#[derive(Clone, Copy)]
struct NsRange {
    location: usize,
    length: usize,
}

/// The objects made in it are let go when it goes.
struct Pool(*mut c_void);

impl Pool {
    fn new() -> Pool {
        // SAFETY: no arguments; popped once, by `drop`, on the same thread.
        Pool(unsafe { objc_autoreleasePoolPush() })
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        // SAFETY: the pool pushed by `new` on this thread, popped once; each
        // lives in one scope (so they are popped in reverse order of pushing).
        unsafe { objc_autoreleasePoolPop(self.0) };
    }
}

struct VisionRecognizer {
    /// The languages asked for, best first (an NSArray of NSStrings).
    languages: Cf,
    language: String,
}

/// The languages Vision recognises at the accurate level, as tags (`zh-Hans`, `en-US`).
fn vision_languages() -> Result<Vec<String>, OcrError> {
    if class(c"VNRecognizeTextRequest").is_null() {
        return Err(OcrError::Failed("the text recogniser of macOS (Vision) is not there".to_string()));
    }
    let request = new_object(c"VNRecognizeTextRequest");
    send1::<(), i64>(request, c"setRecognitionLevel:", 0);
    let list: Id = if send1::<i8, Sel>(request, c"respondsToSelector:", sel(c"supportedRecognitionLanguagesAndReturnError:")) != 0 {
        send1(request, c"supportedRecognitionLanguagesAndReturnError:", null_mut::<Id>())
    } else {
        null_mut()
    };
    let languages: Vec<String> = array_items(list).into_iter().map(rust_string).filter(|s| !s.is_empty()).collect();
    release(request);
    Ok(languages)
}

/// What to ask Vision for: Chinese (Simplified first) with English when `chinese`, else the person's own languages that Vision has.
fn pick_languages(chinese: bool, available: &[String], preferred: &[String]) -> Vec<String> {
    let has = |tag: &str| available.iter().any(|a| a == tag);
    let mut out: Vec<String> = Vec::new();
    if chinese {
        if let Some(zh) = ["zh-Hans", "zh-Hant"].into_iter().find(|t| has(t)) {
            out.push(zh.to_string());
            if has("en-US") {
                out.push("en-US".to_string());
            }
        }
        return out;
    }
    let primary = |tag: &str| tag.split('-').next().unwrap_or("").to_ascii_lowercase();
    for p in preferred {
        // The same tag, else one of the same language with the script the person's tag names (zh-Hans-AU is zh-Hans), else the first
        // of the same language (en-AU is en-US).
        let found = available
            .iter()
            .find(|a| *a == p)
            .or_else(|| available.iter().find(|a| p.starts_with(&format!("{a}-"))))
            .or_else(|| available.iter().find(|a| primary(a) == primary(p) && !a.starts_with("zh") && !a.starts_with("yue")));
        if let Some(a) = found
            && !out.contains(a)
        {
            out.push(a.clone());
        }
    }
    out
}

/// Make the recogniser: for Chinese when `chinese` (and say so if Vision has none), else for the languages the person uses.
pub fn open_recognizer(chinese: bool) -> Result<Box<dyn Recognizer>, OcrError> {
    let _pool = Pool::new();
    let available = vision_languages()?;
    // SAFETY: no arguments; the array is ours (a copy) and `Cf` releases it.
    let preferred = Cf(unsafe { CFLocaleCopyPreferredLanguages() });
    let preferred: Vec<String> = array_items(preferred.0).into_iter().map(rust_string).collect();
    let languages = pick_languages(chinese, &available, &preferred);
    let Some(first) = languages.first().cloned() else {
        return Err(OcrError::NoLanguage { chinese, available });
    };
    Ok(Box::new(VisionRecognizer { languages: cf_array_of_strings(&languages), language: first }))
}

/// A Han, kana, Hangul or full-width character: a word of its own.
fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x2E80..=0x9FFF | 0xA960..=0xA97F | 0xAC00..=0xD7FF | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFFEF | 0x20000..=0x3FFFF)
}

/// The words of a line: their text and where they are in it, in UTF-16 units (what `NSRange` counts), and in characters.
fn split_words(line: &str) -> Vec<(String, NsRange, (usize, usize))> {
    let mut words = Vec::new();
    let (mut start16, mut pos16, mut start_ch) = (0usize, 0usize, 0usize);
    let mut current = String::new();
    let flush = |current: &mut String, start16: usize, end16: usize, start_ch: usize, end_ch: usize, words: &mut Vec<_>| {
        if !current.is_empty() {
            words.push((std::mem::take(current), NsRange { location: start16, length: end16 - start16 }, (start_ch, end_ch)));
        }
    };
    for (i, c) in line.chars().enumerate() {
        let len16 = c.len_utf16();
        if c.is_whitespace() {
            flush(&mut current, start16, pos16, start_ch, i, &mut words);
        } else if is_cjk(c) {
            flush(&mut current, start16, pos16, start_ch, i, &mut words);
            words.push((c.to_string(), NsRange { location: pos16, length: len16 }, (i, i + 1)));
        } else {
            if current.is_empty() {
                start16 = pos16;
                start_ch = i;
            }
            current.push(c);
        }
        pos16 += len16;
    }
    let chars = line.chars().count();
    flush(&mut current, start16, pos16, start_ch, chars, &mut words);
    words
}

/// A box of Vision's (from 0 to 1, from the bottom left) in pixels of a `width` by `height` picture from the top left.
fn vision_box(r: CgRect, width: u32, height: u32) -> [f32; 4] {
    let (w, h) = (f64::from(width), f64::from(height));
    let clamp = |v: f64| v.clamp(0.0, 1.0);
    [(clamp(r.x) * w) as f32, (clamp(1.0 - r.y - r.h) * h) as f32, (clamp(r.x + r.w) * w) as f32, (clamp(1.0 - r.y) * h) as f32]
}

/// The picture as a grey CGImage that owns a copy of the pixels (ours: one reference).
fn gray_cgimage(image: &GrayImage) -> Id {
    // SAFETY: the pixels are live and `len` long and are copied into the CFData; the provider and the colour space are released here
    // after the image has taken its own references; the image is width x height of 8-bit grey with `width` bytes a row, no alpha, which
    // is what `GrayImage` holds (checked by the caller).
    unsafe {
        let data = CFDataCreate(null_mut(), image.pixels.as_ptr(), image.pixels.len() as isize);
        if data.is_null() {
            return null_mut();
        }
        let provider = CGDataProviderCreateWithCFData(data);
        CFRelease(data);
        if provider.is_null() {
            return null_mut();
        }
        let space = CGColorSpaceCreateDeviceGray();
        let w = image.width as usize;
        let cg = CGImageCreate(w, image.height as usize, 8, 8, w, space, 0, provider, null(), false, 0);
        CGColorSpaceRelease(space);
        CGDataProviderRelease(provider);
        cg
    }
}

impl Recognizer for VisionRecognizer {
    fn language(&self) -> String {
        self.language.clone()
    }

    fn max_dimension(&self) -> u32 {
        VISION_MAX_SIDE
    }

    fn recognize(&mut self, image: &GrayImage, cancel: &AtomicBool, timeout: Duration) -> Result<Vec<OcrLine>, OcrError> {
        if image.width == 0 || image.height == 0 || image.width > VISION_MAX_SIDE || image.height > VISION_MAX_SIDE || image.pixels.len() != image.width as usize * image.height as usize {
            return Err(OcrError::Failed("the picture is not a size the recogniser takes".to_string()));
        }
        let _pool = Pool::new();
        let cg = gray_cgimage(image);
        if cg.is_null() {
            return Err(OcrError::Failed("the picture could not be made into an image".to_string()));
        }
        let options = new_object(c"NSDictionary");
        let handler: Id = send2(send0(class(c"VNImageRequestHandler"), c"alloc"), c"initWithCGImage:options:", cg, options);
        // SAFETY: the image is ours; the handler keeps its own reference.
        unsafe { CGImageRelease(cg) };
        release(options);
        if handler.is_null() {
            return Err(OcrError::Failed("the recogniser did not take the picture".to_string()));
        }
        let request = new_object(c"VNRecognizeTextRequest");
        send1::<(), i64>(request, c"setRecognitionLevel:", 0);
        send1::<(), Id>(request, c"setRecognitionLanguages:", self.languages.0);
        send1::<(), bool>(request, c"setUsesLanguageCorrection:", true);
        let requests = cf_array(&[request]);

        // The watcher cancels the request when the person stops the run or the time is up; it ends as soon as the work does, and the
        // scope waits for it, so the request is still live whenever it is sent `cancel`.
        let done = AtomicBool::new(false);
        let timed_out = AtomicBool::new(false);
        let mut error: Id = null_mut();
        let ok = std::thread::scope(|scope| {
            let request_addr = request as usize;
            let (done, timed_out) = (&done, &timed_out);
            scope.spawn(move || {
                let start = Instant::now();
                while !done.load(Ordering::Acquire) {
                    let late = start.elapsed() >= timeout;
                    if late || cancel.load(Ordering::Relaxed) {
                        timed_out.store(late, Ordering::Relaxed);
                        let _pool = Pool::new();
                        send0::<()>(request_addr as Id, c"cancel");
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            });
            let ok = send2::<i8, Id, *mut Id>(handler, c"performRequests:error:", requests.0, &mut error) != 0;
            done.store(true, Ordering::Release);
            ok
        });
        let result = if cancel.load(Ordering::Relaxed) {
            Err(OcrError::Cancelled)
        } else if timed_out.load(Ordering::Relaxed) {
            Err(OcrError::TimedOut)
        } else if !ok {
            let why = rust_string(send0(error, c"localizedDescription"));
            Err(OcrError::Failed(if why.is_empty() { "the recogniser failed".to_string() } else { why }))
        } else {
            read_observations(send0(request, c"results"), image.width, image.height)
        };
        drop(requests);
        release(request);
        release(handler);
        result
    }
}

/// The lines and words of Vision's observations, in the order Vision gives them.
fn read_observations(results: Id, width: u32, height: u32) -> Result<Vec<OcrLine>, OcrError> {
    let (mut out, mut words_in_all) = (Vec::new(), 0usize);
    for observation in array_items(results).into_iter().take(VISION_MAX_LINES) {
        let candidates: Id = send1(observation, c"topCandidates:", 1usize);
        let Some(&best) = array_items(candidates).first() else { continue };
        let text = rust_string(send0(best, c"string"));
        let line_box = send_rect(observation, c"boundingBox");
        let chars = text.chars().count().max(1) as f64;
        let mut line_words = Vec::new();
        for (word, range, (from, to)) in split_words(&text) {
            words_in_all += 1;
            if words_in_all > MAX_WORDS_PER_PAGE {
                return Err(OcrError::Failed("the page has too many words".to_string()));
            }
            let found: Id = send2(best, c"boundingBoxForRange:error:", range, null_mut::<Id>());
            let r = if found.is_null() {
                let (a, b) = (from as f64 / chars, to as f64 / chars);
                CgRect { x: line_box.x + line_box.w * a, y: line_box.y, w: line_box.w * (b - a), h: line_box.h }
            } else {
                send_rect(found, c"boundingBox")
            };
            line_words.push(OcrWord { text: word, rect: vision_box(r, width, height) });
        }
        if !line_words.is_empty() {
            out.push(OcrLine { words: line_words });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_of_vision_is_cut_into_words_with_their_utf16_places() {
        let words: Vec<(String, usize, usize, (usize, usize))> = split_words("PDF \u{4e2d}\u{6587}ok \u{1F600}x").into_iter().map(|(w, r, c)| (w, r.location, r.length, c)).collect();
        assert_eq!(
            words,
            vec![
                ("PDF".to_string(), 0, 3, (0, 3)),
                ("\u{4e2d}".to_string(), 4, 1, (4, 5)),
                ("\u{6587}".to_string(), 5, 1, (5, 6)),
                ("ok".to_string(), 6, 2, (6, 8)),
                ("\u{1F600}x".to_string(), 9, 3, (9, 11)),
            ]
        );
        assert!(split_words("  ").is_empty());
    }

    #[test]
    fn the_languages_asked_of_vision_are_chinese_with_english_or_the_persons_own() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let available = s(&["en-US", "fr-FR", "zh-Hans", "zh-Hant", "yue-Hans"]);
        assert_eq!(pick_languages(true, &available, &s(&["en-AU"])), s(&["zh-Hans", "en-US"]));
        assert_eq!(pick_languages(true, &s(&["en-US"]), &s(&["en-AU"])), Vec::<String>::new());
        assert_eq!(pick_languages(false, &available, &s(&["en-AU", "zh-Hant-TW", "fr-CA", "de-DE"])), s(&["en-US", "zh-Hant", "fr-FR"]));
    }

    #[test]
    fn a_box_of_vision_is_turned_into_pixels_from_the_top_left() {
        assert_eq!(vision_box(CgRect { x: 0.25, y: 0.5, w: 0.5, h: 0.25 }, 400, 200), [100.0, 50.0, 300.0, 100.0]);
    }

    #[test]
    fn every_letter_of_the_icon_font_the_reader_draws_has_a_symbol() {
        // The buttons of the command bar, and the arrows of the menus, the tree and the drop-down boxes.
        for ch in qingpdf_view::cmdbar::BUTTONS.iter().map(|d| d.glyph).chain(['\u{E70D}', '\u{E76C}', '\u{E8DC}', '\u{EDE0}', '\u{EDC6}', '\u{E8EC}', '\u{EA3A}', '\u{E72A}', '\u{E90A}', '\u{E9A6}', '\u{E721}', '\u{E712}']) {
            assert!(symbol_for(ch).is_some(), "{ch:?}");
        }
        assert_eq!(symbol_for('a'), None);
        assert!(symbol_is_small("chevron.down") && !symbol_is_small("folder"));
    }

    #[test]
    fn mac_key_codes_become_the_windows_ones() {
        assert_eq!(vk_of(0), Some(vk::A));
        assert_eq!(vk_of(35), Some(vk::P));
        assert_eq!(vk_of(36), Some(vk::RETURN));
        assert_eq!(vk_of(53), Some(vk::ESCAPE));
        assert_eq!(vk_of(123), Some(vk::LEFT));
        assert_eq!(vk_of(126), Some(vk::UP));
        assert_eq!(vk_of(121), Some(vk::NEXT));
        assert_eq!(vk_of(83), Some(vk::NUMPAD0 + 1));
        assert_eq!(vk_of(92), Some(vk::NUMPAD0 + 9));
        assert_eq!(vk_of(200), None);
    }

    #[test]
    fn menu_labels_lose_their_mnemonics_and_accelerators() {
        assert_eq!(clean_label("&Open...\tCtrl+O"), "Open...");
        assert_eq!(clean_label("打开(&O)...\tCtrl+O"), "打开...");
        assert_eq!(clean_label("Fish && &Chips"), "Fish & Chips");
    }

    #[test]
    fn shortcuts_become_key_equivalents_only_where_a_box_cannot_lose_a_key() {
        assert_eq!(shortcut("&Open...\tCtrl+O"), Some(("o".to_string(), 1 << 20)));
        assert_eq!(shortcut("Zoom &in\tCtrl++"), Some(("+".to_string(), 1 << 20)));
        assert_eq!(shortcut("Find p&revious\tShift+F3"), Some(("\u{F706}".to_string(), 1 << 17)));
        assert_eq!(shortcut("Show &sidebar\tF4"), Some(("\u{F707}".to_string(), 0)));
        assert_eq!(shortcut("N&ext tab\tCtrl+Tab"), Some(("\t".to_string(), 1 << 18)));
        assert_eq!(shortcut("&Delete\tDel"), None);
        assert_eq!(shortcut("&Copy\tCtrl+C"), None);
        assert_eq!(shortcut("Move &Up a Level\tShift+Tab"), None);
        assert_eq!(shortcut("No shortcut"), None);
    }

    #[test]
    fn filters_become_extensions() {
        let pdf = vec![("PDF".to_string(), "*.pdf;*.PDF".to_string())];
        assert_eq!(extensions(&pdf), vec!["pdf".to_string()]);
        assert!(extensions(&[("All".to_string(), "*.*".to_string())]).is_empty());
    }

    #[test]
    fn the_date_and_the_time_have_their_shapes() {
        assert_eq!(local_date().len(), 10);
        assert_eq!(local_time(0).len(), 19);
        let mut bytes = [0u8; 16];
        assert!(random_bytes(&mut bytes));
    }
}
