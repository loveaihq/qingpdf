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

use qingpdf_view::ocr::{OcrError, Recognizer};
use qingpdf_view::placement::Placement;
use qingpdf_view::theme::Contrast;
use qingpdf_view::ui::*;

// --- the declarations ------------------------------------------------------------------------------------------------

type Id = *mut c_void;
type Sel = *const c_void;

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
    fn CGDataProviderCreateWithData(info: *mut c_void, data: *const c_void, size: usize, release: *const c_void) -> Id;
    fn CGDataProviderRelease(provider: Id);
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

/// `struct rusage` up to the field that is used (the two `timeval`s come first, then `ru_maxrss`, in bytes on macOS), and the rest as padding.
#[repr(C)]
struct Rusage {
    times: [i64; 4],
    maxrss: i64,
    rest: [i64; 13],
}

unsafe extern "C" {
    fn time(t: *mut i64) -> i64;
    fn localtime_r(t: *const i64, out: *mut Tm) -> *mut Tm;
    fn getrusage(who: i32, out: *mut Rusage) -> i32;
    fn pthread_main_np() -> i32;
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
    ctx: Id,
    space: Id,
    h: i32,
    fonts: [Id; 4],
    attrs: [Cf; 4],
    font: usize,
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
        // TODO(macos): the icon font (Segoe Fluent Icons) has no counterpart; the buttons' pictures need drawing as shapes or SF Symbols.
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
        MacPainter { ctx, space, h, fonts, attrs, font: 0 }
    }

    /// The y of the bottom edge of a rectangle that has its top at `y`, in the context's own coordinates (y up).
    fn cg_rect(&self, r: Rect) -> CgRect {
        CgRect { x: f64::from(r.x), y: f64::from(self.h - r.y - r.h), w: f64::from(r.w), h: f64::from(r.h) }
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
    dispatch(Event::MouseDown { x, y, ctrl, shift, clicks: clicks.clamp(1, 3) as u8 });
    update_cursor(x, y);
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
    0
}

extern "C" fn delegate_should_close(_this: Id, _sel: Sel, _window: Id) -> i8 {
    dispatch(Event::CloseRequested);
    0
}

extern "C" fn delegate_no(_this: Id, _sel: Sel, _app: Id) -> i8 {
    0
}

extern "C" fn delegate_resized(_this: Id, _sel: Sel, _note: Id) {
    sync_size();
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
    if let Ok(id) = u32::try_from(tag) {
        dispatch(Event::Command(id));
    }
}

fn register_classes() -> (Id, Id) {
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
        ],
    );
    (view, delegate)
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
fn alert(title: &str, text: &str, buttons: &[&str]) -> usize {
    let alert = new_object(c"NSAlert");
    let (t, m) = (cfstr(title), cfstr(text));
    send1::<(), Id>(alert, c"setMessageText:", t.0);
    send1::<(), Id>(alert, c"setInformativeText:", m.0);
    for button in buttons {
        let b = cfstr(button);
        let _: Id = send1(alert, c"addButtonWithTitle:", b.0);
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
            alert(&title, &text, &["OK"]);
        }
        Action::Confirm { tag, title, text } => {
            // TODO(macos): the words of the buttons are English whatever the language of the reader is.
            let yes = alert(&title, &text, &["Yes", "No"]) == 0;
            dispatch(Event::Confirmed { tag, yes });
        }
        Action::AskSave { tag, title, text } => {
            let choice = match alert(&title, &text, &["Save", "Don't Save", "Cancel"]) {
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
        Action::SetMenus(menus) => install_menus(&menus),
        // TODO(macos): the rest of the actions: AskText, AskForm, Outline*, Sidebar, Find, SearchBox, PillEdit, ChoosePrinter, Print, FieldEdit,
        // FieldMove, Choose, ContextMenu, ShowMenu, Theme, FullScreen, HideCursor.
        _ => {}
    }
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
                let (l, key) = (cfstr(&clean_label(label)), cfstr(""));
                let allocated: Id = send0(class(c"NSMenuItem"), c"alloc");
                let item: Id = send3(allocated, c"initWithTitle:action:keyEquivalent:", l.0, sel(c"menuItem:"), key.0);
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

/// The menu bar: the application's own menu (Quit), then the reader's menus. TODO(macos): accelerators are not shown (the keys work through `Event::Key`).
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

/// Make the window and run it until it is closed (see `win32::run`). The saved place and `remember` are not used yet.
/// TODO(macos): keep the window's place between runs.
pub fn run(title: &str, client: Option<(i32, i32)>, _saved: Option<Placement>, _remember: bool, make: impl FnOnce(Waker) -> Box<dyn Handler>) -> i32 {
    // SAFETY: the pool lives to the end of this function, which is where the program's main thread leaves its work.
    let pool = unsafe { objc_autoreleasePoolPush() };
    let app: Id = send0(class(c"NSApplication"), c"sharedApplication");
    send1::<i8, i64>(app, c"setActivationPolicy:", 0);
    let (view_class, delegate_class) = register_classes();
    if view_class.is_null() || delegate_class.is_null() {
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
    let window: Id = send4::<Id, CgRect, u64, u64, bool>(allocated, c"initWithContentRect:styleMask:backing:defer:", frame, 15, 2, false);
    let allocated: Id = send0(view_class, c"alloc");
    let view: Id = send1(allocated, c"initWithFrame:", frame);
    let name = cfstr(title);
    send1::<(), Id>(window, c"setTitle:", name.0);
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
    send0::<()>(window, c"center");
    // SAFETY: a Core Graphics constant.
    let space = unsafe { CGColorSpaceCreateWithName(kCGColorSpaceSRGB) };
    with_mac(|m| {
        m.window = window as usize;
        m.view = view as usize;
        m.delegate = delegate as usize;
        m.space = space as usize;
        m.scale = screen_scale;
    });
    install_menus(&[]);

    let handler = make(Waker(delegate as usize));
    HANDLER.with(|h| *h.borrow_mut() = Some(handler));
    let scale: f64 = send0(window, c"backingScaleFactor");
    dispatch(Event::Dpi((96.0 * scale.max(1.0)).round() as u32));
    dispatch(Event::Motion(true));
    dispatch(Event::Frame { custom: false, caption: 0 });
    with_mac(|m| m.told_dpi = (96.0 * scale.max(1.0)).round() as u32);
    sync_size();

    send1::<(), Id>(window, c"makeKeyAndOrderFront:", null_mut());
    send1::<(), Id>(window, c"makeFirstResponder:", view);
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
    let mut usage = Rusage { times: [0; 4], maxrss: 0, rest: [0; 13] };
    // SAFETY: `usage` is live and as big as a `struct rusage` (144 bytes); 0 is RUSAGE_SELF.
    let ok = unsafe { getrusage(0, &mut usage) } == 0;
    if ok { u64::try_from(usage.maxrss).unwrap_or(0) } else { 0 }
}

/// A macOS program started from a terminal has its console already.
pub fn attach_console() {}

/// TODO(macos): one window for all the files (the Windows reader finds the first copy by a mutex and hands files over by a message).
pub fn first_copy() -> bool {
    true
}

pub fn hand_over(_paths: &[PathBuf]) -> bool {
    false
}

pub fn system_uses_dark() -> bool {
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

pub fn high_contrast() -> Option<Contrast> {
    None
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

/// TODO(macos): text recognition (Vision) is not there yet.
pub fn open_recognizer(_chinese: bool) -> Result<Box<dyn Recognizer>, OcrError> {
    Err(OcrError::Failed("text recognition is not available on macOS yet".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

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
