//! The content stream interpreter that draws (ISO 32000-1 8.2 to 8.7, 8.9, 8.10, 9.3, 9.6.5):
//! the graphics state, paths, clipping, colours, images, form XObjects, Type 3 glyphs and,
//! for the other fonts, an outline box per character (the real glyphs come in step 3b).
//! Operators are read by the same scanner text extraction uses.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use tiny_skia::{
    BlendMode, Color, FillRule, FilterQuality, LineCap, LineJoin, Mask, Paint, PathBuilder, Pixmap, PixmapPaint, Stroke, StrokeDash, Transform,
};

use crate::document::Document;
use crate::error::{Error, Result};
use crate::object::{Dict, ObjRef, Object, Stream};
use crate::text::cmap::{CMap, Uni};
use crate::text::font::{Font, Warnings};
use crate::text::interp::{FxMap, Matrix, mul};
use crate::text::scan::{Item, Operand, Scanner};

use super::color::ColorSpace;
use super::func::Meter;
use super::image::{self, Loaded};

/// Most operators one page may run, forms and glyph procedures included.
pub(crate) const MAX_OPERATORS: usize = 2_000_000;
/// Forms (and Type 3 glyph procedures) nest at most this deep.
pub(crate) const MAX_FORM_DEPTH: usize = 24;
/// Deepest `q` nesting kept; further ones are ignored.
const MAX_GSTATE_DEPTH: usize = 256;
/// Most path segments one page may build, and one path may have.
pub(crate) const MAX_PAGE_SEGMENTS: usize = 8_000_000;
const MAX_PATH_SEGMENTS: usize = 1_000_000;
/// The most area, in pixels, the painting of one page may cover (the sum of the bounding boxes).
pub(crate) const MAX_PAINT_AREA: f64 = 6.0e9;
/// Most source pixels of images one page may decode.
pub(crate) const MAX_PAGE_IMAGE_PIXELS: u64 = 256 * 1024 * 1024;
/// Decoded form contents and glyph procedures kept for reuse, in bytes.
const MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;
/// Images kept for reuse, in bytes. (With the page itself, 64 MB at most, and the buffers of an image
/// being made, about 110 MB, a page stays under 200 MB.)
const MAX_IMAGE_CACHE_BYTES: usize = 24 * 1024 * 1024;
/// Bytes of clip masks alive at once (the `q` stack keeps one per nested non-rectangular clip, a page's
/// worth of bytes each); past it clips are made from their bounding boxes.
const MAX_LIVE_MASK_BYTES: usize = 64 * 1024 * 1024;
/// Dashes one stroke may be cut into, and one page in all; a longer pattern is drawn solid.
const MAX_STROKE_DASHES: f64 = 1_000_000.0;
const MAX_PAGE_DASHES: f64 = 4_000_000.0;

const IDENTITY: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

// Operators as numbers (their length and bytes), so that dispatching is a jump.
#[allow(clippy::indexing_slicing)] // `i < op.len()`
const fn key(op: &[u8]) -> u32 {
    let mut k = (op.len() as u32) << 24;
    let mut i = 0;
    while i < op.len() && i < 3 {
        k |= (op[i] as u32) << (8 * i);
        i += 1;
    }
    k
}

#[allow(clippy::indexing_slicing)] // `i < op.len()`
fn op_key(op: &[u8]) -> u32 {
    if op.len() > 3 {
        return 0;
    }
    key(op)
}

macro_rules! ops {
    ($($name:ident = $s:literal;)*) => { $(const $name: u32 = key($s);)* };
}
ops! {
    OP_Q_SAVE = b"q"; OP_Q_RESTORE = b"Q"; OP_CM = b"cm"; OP_W = b"w"; OP_J_CAP = b"J"; OP_J_JOIN = b"j"; OP_M_LIMIT = b"M";
    OP_D = b"d"; OP_GS = b"gs"; OP_M = b"m"; OP_L = b"l"; OP_C = b"c"; OP_V = b"v"; OP_Y = b"y"; OP_H = b"h"; OP_RE = b"re";
    OP_S = b"S"; OP_S_CLOSE = b"s"; OP_F = b"f"; OP_F_UPPER = b"F"; OP_F_STAR = b"f*"; OP_B = b"B"; OP_B_STAR = b"B*";
    OP_B_CLOSE = b"b"; OP_B_CLOSE_STAR = b"b*"; OP_N = b"n"; OP_CLIP = b"W"; OP_CLIP_STAR = b"W*";
    OP_G = b"g"; OP_G_UPPER = b"G"; OP_RG = b"rg"; OP_RG_UPPER = b"RG"; OP_K = b"k"; OP_K_UPPER = b"K";
    OP_CS = b"cs"; OP_CS_UPPER = b"CS"; OP_SC = b"sc"; OP_SC_UPPER = b"SC"; OP_SCN = b"scn"; OP_SCN_UPPER = b"SCN";
    OP_SH = b"sh"; OP_DO = b"Do"; OP_BT = b"BT"; OP_ET = b"ET";
    OP_TC = b"Tc"; OP_TW = b"Tw"; OP_TZ = b"Tz"; OP_TL = b"TL"; OP_TF = b"Tf"; OP_TR = b"Tr"; OP_TS = b"Ts";
    OP_TD = b"Td"; OP_TD_UPPER = b"TD"; OP_TM = b"Tm"; OP_T_STAR = b"T*"; OP_TJ = b"Tj"; OP_TJ_ARRAY = b"TJ";
    OP_QUOTE = b"'"; OP_DQUOTE = b"\""; OP_D0 = b"d0"; OP_D1 = b"d1";
}

fn finite(m: &Matrix) -> bool {
    m.iter().all(|v| v.is_finite())
}

fn apply(m: &Matrix, x: f64, y: f64) -> (f64, f64) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

fn numbers<const N: usize>(ops: &[Operand]) -> Option<[f64; N]> {
    let tail = ops.get(ops.len().checked_sub(N)?..)?;
    let mut out = [0.0; N];
    for (slot, o) in out.iter_mut().zip(tail) {
        match o {
            Operand::Num(v) => *slot = *v,
            _ => return None,
        }
    }
    Some(out)
}

// --- resources ----------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Cat {
    Font,
    XObject,
    ExtGState,
    ColorSpace,
}

/// A `/Resources` dictionary, its sub-dictionaries read when first needed.
pub(crate) struct Resources {
    dict: Dict,
    fonts: OnceCell<FxMap<Vec<u8>, Object>>,
    xobjects: OnceCell<FxMap<Vec<u8>, Object>>,
    ext_g_states: OnceCell<FxMap<Vec<u8>, Object>>,
    color_spaces: OnceCell<FxMap<Vec<u8>, Object>>,
    loaded_spaces: RefCell<FxMap<Vec<u8>, Option<Arc<ColorSpace>>>>,
}

impl Resources {
    pub fn new(doc: &Document, resources: Option<&Object>) -> Rc<Resources> {
        let dict = resources.and_then(|r| doc.resolve(r).ok()).and_then(|o| o.as_dict().cloned()).unwrap_or_default();
        Rc::new(Resources {
            dict,
            fonts: OnceCell::new(),
            xobjects: OnceCell::new(),
            ext_g_states: OnceCell::new(),
            color_spaces: OnceCell::new(),
            loaded_spaces: RefCell::new(FxMap::default()),
        })
    }

    fn table(&self, doc: &Document, cat: Cat) -> &FxMap<Vec<u8>, Object> {
        let (cell, key) = match cat {
            Cat::Font => (&self.fonts, "Font"),
            Cat::XObject => (&self.xobjects, "XObject"),
            Cat::ExtGState => (&self.ext_g_states, "ExtGState"),
            Cat::ColorSpace => (&self.color_spaces, "ColorSpace"),
        };
        cell.get_or_init(|| {
            let sub = self.dict.get(key).and_then(|o| doc.resolve(o).ok());
            match sub {
                Some(Object::Dict(d)) => d.into_pairs().into_iter().map(|(k, v)| (k.0, v)).collect(),
                _ => FxMap::default(),
            }
        })
    }

    fn get(&self, doc: &Document, cat: Cat, name: &[u8]) -> Option<Object> {
        self.table(doc, cat).get(name).cloned()
    }
}

// --- shared state -------------------------------------------------------------------------------------

/// A Type 3 font (9.6.5): glyph procedures by code.
struct Type3 {
    matrix: Matrix,
    /// Code to glyph name.
    names: Vec<Option<Vec<u8>>>,
    procs: FxMap<Vec<u8>, Object>,
    resources: Option<Rc<Resources>>,
    cache: RefCell<FxMap<u8, Option<Rc<Vec<u8>>>>>,
}

struct FontEntry {
    font: Font,
    type3: Option<Type3>,
}

struct Form {
    content: Vec<u8>,
    matrix: Matrix,
    bbox: Option<[f64; 4]>,
    resources: Option<Rc<Resources>>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct ImageKey {
    num: u32,
    generation: u16,
    target: (usize, usize),
    fill: [u8; 3],
}

/// A prepared image kept for reuse.
struct Prepared {
    pixmap: Pixmap,
    opaque: bool,
}

/// Where the unit square (an image) falls on the device.
struct Placement {
    /// The pixels it covers, when it is not turned: the outer rectangle (left, top, right, bottom).
    rect: Option<[i32; 4]>,
    flip_x: bool,
    flip_y: bool,
    /// How big the image should be to be drawn without resampling: the size of the rectangle, or the
    /// lengths of the two sides when it is turned.
    target: (usize, usize),
}

#[derive(Default)]
pub(crate) struct Shared {
    fonts: FxMap<ObjRef, Option<Rc<FontEntry>>>,
    pub cmaps: HashMap<String, Option<Arc<CMap>>>,
    pub warnings: Warnings,
    forms: FxMap<ObjRef, Option<Rc<Form>>>,
    cache_bytes: usize,
    images: HashMap<ImageKey, Rc<Prepared>>,
    image_bytes: usize,
}

// --- graphics state -----------------------------------------------------------------------------------

/// A clip mask that counts its bytes in `live` for as long as it exists.
struct MaskBuf {
    mask: Mask,
    live: Option<Rc<Cell<usize>>>,
}

impl MaskBuf {
    fn new(mask: Mask, live: Option<&Rc<Cell<usize>>>) -> MaskBuf {
        if let Some(l) = live {
            l.set(l.get().saturating_add(mask.data().len()));
        }
        MaskBuf { mask, live: live.cloned() }
    }
}

impl Drop for MaskBuf {
    fn drop(&mut self) {
        if let Some(l) = &self.live {
            l.set(l.get().saturating_sub(self.mask.data().len()));
        }
    }
}

/// The mask to hand to the rasterizer.
fn as_mask(m: &Option<Rc<MaskBuf>>) -> Option<&Mask> {
    m.as_ref().map(|m| &m.mask)
}

/// A page-sized mask that is set inside `rect` (left, top, right, bottom).
fn rect_mask(width: u32, height: u32, rect: [i32; 4]) -> Option<Mask> {
    let mut mask = Mask::new(width, height)?;
    let [x0, y0, x1, y1] = rect;
    let (x0, x1) = (x0.max(0) as usize, (x1.max(0) as usize).min(width as usize));
    let (y0, y1) = (y0.max(0) as usize, (y1.max(0) as usize).min(height as usize));
    for row in mask.data_mut().chunks_exact_mut(width as usize).take(y1).skip(y0) {
        if let Some(span) = row.get_mut(x0..x1) {
            span.fill(255);
        }
    }
    Some(mask)
}

/// The clipping region: an integer rectangle in device pixels, and, when the clip is not a
/// rectangle, a mask that already includes the rectangle.
struct Clip {
    rect: [i32; 4],
    mask: Option<Rc<MaskBuf>>,
    rect_mask: OnceCell<Rc<MaskBuf>>,
}

impl Clip {
    fn new(rect: [i32; 4], mask: Option<Rc<MaskBuf>>) -> Clip {
        Clip { rect, mask, rect_mask: OnceCell::new() }
    }

    fn is_empty(&self) -> bool {
        self.rect[0] >= self.rect[2] || self.rect[1] >= self.rect[3]
    }

    /// A mask of the whole page that is set inside the rectangle. One kept with the clip counts in `live`
    /// (and is kept only while that stays under the cap); past the cap it is made again for each use.
    fn mask_for(&self, width: u32, height: u32, live: &Rc<Cell<usize>>) -> Option<Rc<MaskBuf>> {
        if let Some(m) = &self.mask {
            return Some(m.clone());
        }
        if let Some(m) = self.rect_mask.get() {
            return Some(m.clone());
        }
        let mask = rect_mask(width, height, self.rect)?;
        if live.get().saturating_add(mask.data().len()) > MAX_LIVE_MASK_BYTES {
            return Some(Rc::new(MaskBuf::new(mask, None)));
        }
        let rc = Rc::new(MaskBuf::new(mask, Some(live)));
        let _ = self.rect_mask.set(rc.clone());
        Some(rc)
    }
}

#[derive(Clone)]
struct Colour {
    space: Arc<ColorSpace>,
    rgb: [f32; 3],
    /// Nothing is painted with it (a pattern, which comes with step 3c, or a /None separation).
    none: bool,
}

#[derive(Clone)]
struct GState {
    ctm: Matrix,
    clip: Rc<Clip>,
    fill: Colour,
    stroke: Colour,
    fill_alpha: f32,
    stroke_alpha: f32,
    line_width: f64,
    cap: LineCap,
    join: LineJoin,
    miter: f64,
    dash: Option<Rc<(Vec<f32>, f32)>>,
    font: Option<Rc<FontEntry>>,
    size: f64,
    tc: f64,
    tw: f64,
    th: f64,
    tl: f64,
    rise: f64,
    mode: i64,
}

/// A path as the content stream gave it, in user space.
#[derive(Default)]
struct PathData {
    /// 0 move, 1 line, 2 curve, 3 close.
    verbs: Vec<u8>,
    pts: Vec<f64>,
}

impl PathData {
    fn clear(&mut self) {
        self.verbs.clear();
        self.pts.clear();
    }
}

/// What a shown character needs from the font.
struct Shown {
    code: u32,
    w0: f64,
    vertical: Option<(f64, f64, f64)>,
    is_space: bool,
    blank: bool,
}

pub(crate) struct Interp<'a> {
    doc: &'a Document,
    shared: &'a mut Shared,
    pub pixmap: Pixmap,
    base: Matrix,
    gs: GState,
    stack: Vec<GState>,
    path: PathData,
    cur: (f64, f64),
    start: (f64, f64),
    pending_clip: Option<FillRule>,
    tm: Matrix,
    tlm: Matrix,
    ops_left: usize,
    segs_left: usize,
    area_left: f64,
    image_pixels_left: u64,
    /// Steps the page may spend running tint functions.
    meter: Meter,
    /// Bytes of clip masks alive now.
    live_masks: Rc<Cell<usize>>,
    dashes_left: f64,
    in_progress: Vec<ObjRef>,
    /// The depth of `stack` a `Q` may not go below: what is under it belongs to whoever called a form or a glyph.
    stack_floor: usize,
    /// Inside a `d1` glyph: colours are the text's, whatever the glyph says.
    uncolored: bool,
    chars: Vec<Shown>,
    /// Characters drawn as boxes so far.
    pub boxed: usize,
    gray: Arc<ColorSpace>,
    rgb: Arc<ColorSpace>,
    cmyk: Arc<ColorSpace>,
}

impl<'a> Interp<'a> {
    pub fn new(doc: &'a Document, shared: &'a mut Shared, pixmap: Pixmap, base: Matrix, meter: Meter) -> Interp<'a> {
        let (w, h) = (pixmap.width() as i32, pixmap.height() as i32);
        let gray = Arc::new(ColorSpace::Gray);
        let black = Colour { space: gray.clone(), rgb: [0.0; 3], none: false };
        Interp {
            doc,
            shared,
            pixmap,
            base,
            gs: GState {
                ctm: IDENTITY,
                clip: Rc::new(Clip::new([0, 0, w, h], None)),
                fill: black.clone(),
                stroke: black,
                fill_alpha: 1.0,
                stroke_alpha: 1.0,
                line_width: 1.0,
                cap: LineCap::Butt,
                join: LineJoin::Miter,
                miter: 10.0,
                dash: None,
                font: None,
                size: 0.0,
                tc: 0.0,
                tw: 0.0,
                th: 1.0,
                tl: 0.0,
                rise: 0.0,
                mode: 0,
            },
            stack: Vec::new(),
            path: PathData::default(),
            cur: (0.0, 0.0),
            start: (0.0, 0.0),
            pending_clip: None,
            tm: IDENTITY,
            tlm: IDENTITY,
            ops_left: MAX_OPERATORS,
            segs_left: MAX_PAGE_SEGMENTS,
            area_left: MAX_PAINT_AREA,
            image_pixels_left: MAX_PAGE_IMAGE_PIXELS,
            meter,
            live_masks: Rc::new(Cell::new(0)),
            dashes_left: MAX_PAGE_DASHES,
            in_progress: Vec::new(),
            stack_floor: 0,
            uncolored: false,
            chars: Vec::new(),
            boxed: 0,
            gray,
            rgb: Arc::new(ColorSpace::Rgb),
            cmyk: Arc::new(ColorSpace::Cmyk),
        }
    }

    /// Run one content stream.
    pub fn run(&mut self, content: &[u8], res: &Rc<Resources>, depth: usize) -> Result<()> {
        let mut sc = Scanner::new(content);
        let mut ops: Vec<Operand> = Vec::new();
        loop {
            let Item::Operator(op) = sc.next(&mut ops) else { return Ok(()) };
            self.charge_op()?;
            if op == b"BI" {
                self.inline_image(&mut sc, &mut ops, res)?;
                continue;
            }
            self.operator(op_key(op), &ops, &sc, res, depth)?;
        }
    }

    fn warn(&mut self, message: impl Into<String>) {
        self.shared.warnings.add(message);
    }

    /// Count one operator (or one glyph shown) against the page's allowance.
    fn charge_op(&mut self) -> Result<()> {
        if self.ops_left == 0 {
            return Err(Error::Limit(format!("more than {MAX_OPERATORS} content stream operators and glyphs on one page")));
        }
        self.ops_left -= 1;
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn operator(&mut self, op: u32, ops: &[Operand], sc: &Scanner<'_>, res: &Rc<Resources>, depth: usize) -> Result<()> {
        match op {
            OP_Q_SAVE => {
                if self.stack.len() < MAX_GSTATE_DEPTH {
                    self.stack.push(self.gs.clone());
                }
            }
            OP_Q_RESTORE => {
                if self.stack.len() > self.stack_floor
                    && let Some(g) = self.stack.pop()
                {
                    self.gs = g;
                }
            }
            OP_CM => {
                if let Some(m) = numbers::<6>(ops) {
                    let m = mul(&m, &self.gs.ctm);
                    if finite(&m) {
                        self.gs.ctm = m;
                    }
                }
            }
            OP_W => {
                if let Some([w]) = numbers::<1>(ops) {
                    self.gs.line_width = w.abs();
                }
            }
            OP_J_CAP => {
                if let Some([v]) = numbers::<1>(ops) {
                    self.gs.cap = cap_of(v as i64);
                }
            }
            OP_J_JOIN => {
                if let Some([v]) = numbers::<1>(ops) {
                    self.gs.join = join_of(v as i64);
                }
            }
            OP_M_LIMIT => {
                if let Some([v]) = numbers::<1>(ops) {
                    self.gs.miter = v.max(1.0);
                }
            }
            OP_D => self.set_dash(ops),
            OP_GS => {
                if let Some(Operand::Name(n)) = ops.last() {
                    self.ext_g_state(sc.bytes(*n), res);
                }
            }
            // --- paths ---
            OP_M => {
                if let Some([x, y]) = numbers::<2>(ops) {
                    self.add_seg(0, &[x, y]);
                    self.cur = (x, y);
                    self.start = (x, y);
                }
            }
            OP_L => {
                if let Some([x, y]) = numbers::<2>(ops) {
                    if self.path.verbs.is_empty() {
                        self.add_seg(0, &[x, y]);
                        self.start = (x, y);
                    } else {
                        self.add_seg(1, &[x, y]);
                    }
                    self.cur = (x, y);
                }
            }
            OP_C => {
                if let Some(p) = numbers::<6>(ops) {
                    self.curve(p);
                }
            }
            OP_V => {
                if let Some([x2, y2, x3, y3]) = numbers::<4>(ops) {
                    let (x1, y1) = self.cur;
                    self.curve([x1, y1, x2, y2, x3, y3]);
                }
            }
            OP_Y => {
                if let Some([x1, y1, x3, y3]) = numbers::<4>(ops) {
                    self.curve([x1, y1, x3, y3, x3, y3]);
                }
            }
            OP_H => self.close(),
            OP_RE => {
                if let Some([x, y, w, h]) = numbers::<4>(ops) {
                    self.add_seg(0, &[x, y]);
                    self.add_seg(1, &[x + w, y]);
                    self.add_seg(1, &[x + w, y + h]);
                    self.add_seg(1, &[x, y + h]);
                    self.add_seg(3, &[]);
                    self.cur = (x, y);
                    self.start = (x, y);
                }
            }
            OP_S => self.paint(false, false, true, FillRule::Winding)?,
            OP_S_CLOSE => self.paint(true, false, true, FillRule::Winding)?,
            OP_F | OP_F_UPPER => self.paint(false, true, false, FillRule::Winding)?,
            OP_F_STAR => self.paint(false, true, false, FillRule::EvenOdd)?,
            OP_B => self.paint(false, true, true, FillRule::Winding)?,
            OP_B_STAR => self.paint(false, true, true, FillRule::EvenOdd)?,
            OP_B_CLOSE => self.paint(true, true, true, FillRule::Winding)?,
            OP_B_CLOSE_STAR => self.paint(true, true, true, FillRule::EvenOdd)?,
            OP_N => self.paint(false, false, false, FillRule::Winding)?,
            OP_CLIP => self.pending_clip = Some(FillRule::Winding),
            OP_CLIP_STAR => self.pending_clip = Some(FillRule::EvenOdd),
            // --- colour ---
            OP_G => {
                if let Some([g]) = numbers::<1>(ops) {
                    self.set_device(true, self.gray.clone(), &[g]);
                }
            }
            OP_G_UPPER => {
                if let Some([g]) = numbers::<1>(ops) {
                    self.set_device(false, self.gray.clone(), &[g]);
                }
            }
            OP_RG => {
                if let Some(c) = numbers::<3>(ops) {
                    self.set_device(true, self.rgb.clone(), &c);
                }
            }
            OP_RG_UPPER => {
                if let Some(c) = numbers::<3>(ops) {
                    self.set_device(false, self.rgb.clone(), &c);
                }
            }
            OP_K => {
                if let Some(c) = numbers::<4>(ops) {
                    self.set_device(true, self.cmyk.clone(), &c);
                }
            }
            OP_K_UPPER => {
                if let Some(c) = numbers::<4>(ops) {
                    self.set_device(false, self.cmyk.clone(), &c);
                }
            }
            OP_CS | OP_CS_UPPER => {
                if let Some(Operand::Name(n)) = ops.last() {
                    let name = sc.bytes(*n).to_vec();
                    if let Some(space) = self.load_space(&name, res) {
                        let initial = space.initial();
                        self.set_device(op == OP_CS, space, &initial);
                    }
                }
            }
            OP_SC | OP_SC_UPPER | OP_SCN | OP_SCN_UPPER => self.set_color(op == OP_SC || op == OP_SCN, ops),
            OP_SH => self.warn("shading fills (sh) are not drawn yet"),
            // --- XObjects ---
            OP_DO => {
                if let Some(Operand::Name(n)) = ops.last() {
                    let name = sc.bytes(*n).to_vec();
                    self.do_xobject(&name, res, depth)?;
                }
            }
            // --- text ---
            OP_BT => {
                self.tm = IDENTITY;
                self.tlm = IDENTITY;
            }
            OP_ET => {}
            OP_TC => {
                if let Some([v]) = numbers::<1>(ops) {
                    self.gs.tc = v;
                }
            }
            OP_TW => {
                if let Some([v]) = numbers::<1>(ops) {
                    self.gs.tw = v;
                }
            }
            OP_TZ => {
                if let Some([v]) = numbers::<1>(ops) {
                    self.gs.th = v / 100.0;
                }
            }
            OP_TL => {
                if let Some([v]) = numbers::<1>(ops) {
                    self.gs.tl = v;
                }
            }
            OP_TS => {
                if let Some([v]) = numbers::<1>(ops) {
                    self.gs.rise = v;
                }
            }
            OP_TR => {
                if let Some([v]) = numbers::<1>(ops) {
                    self.gs.mode = v as i64;
                }
            }
            OP_TF => {
                if let [.., Operand::Name(name), Operand::Num(size)] = ops {
                    self.gs.size = *size;
                    let name = sc.bytes(*name).to_vec();
                    self.gs.font = self.font(&name, res);
                }
            }
            OP_TD => {
                if let Some([tx, ty]) = numbers::<2>(ops) {
                    self.move_line(tx, ty);
                }
            }
            OP_TD_UPPER => {
                if let Some([tx, ty]) = numbers::<2>(ops) {
                    self.gs.tl = -ty;
                    self.move_line(tx, ty);
                }
            }
            OP_TM => {
                if let Some(m) = numbers::<6>(ops)
                    && finite(&m)
                {
                    self.tm = m;
                    self.tlm = m;
                }
            }
            OP_T_STAR => self.move_line(0.0, -self.gs.tl),
            OP_TJ => {
                if let Some(Operand::Str(s)) = ops.last() {
                    self.show(&[sc.bytes(*s)], res, depth)?;
                }
            }
            OP_TJ_ARRAY => {
                // Strings and adjustments in order; runs of strings are shown together.
                for item in ops {
                    match item {
                        Operand::Str(s) => self.show(&[sc.bytes(*s)], res, depth)?,
                        Operand::Num(n) => self.adjust(*n),
                        _ => {}
                    }
                }
            }
            OP_QUOTE => {
                self.move_line(0.0, -self.gs.tl);
                if let Some(Operand::Str(s)) = ops.last() {
                    self.show(&[sc.bytes(*s)], res, depth)?;
                }
            }
            OP_DQUOTE => {
                if let [.., Operand::Num(aw), Operand::Num(ac), Operand::Str(s)] = ops {
                    self.gs.tw = *aw;
                    self.gs.tc = *ac;
                    self.move_line(0.0, -self.gs.tl);
                    self.show(&[sc.bytes(*s)], res, depth)?;
                }
            }
            // --- Type 3 glyph procedures (9.6.5) ---
            OP_D0 => self.uncolored = false,
            OP_D1 => self.uncolored = true,
            _ => {}
        }
        Ok(())
    }

    // --- device transform ------------------------------------------------------------------------------

    /// User space to device pixels.
    fn full(&self) -> Matrix {
        mul(&self.gs.ctm, &self.base)
    }

    // --- paths -----------------------------------------------------------------------------------------

    fn add_seg(&mut self, verb: u8, pts: &[f64]) {
        if self.segs_left == 0 || self.path.verbs.len() >= MAX_PATH_SEGMENTS {
            return;
        }
        self.segs_left -= 1;
        self.path.verbs.push(verb);
        self.path.pts.extend_from_slice(pts);
    }

    fn curve(&mut self, p: [f64; 6]) {
        if self.path.verbs.is_empty() {
            self.add_seg(0, &[p[0], p[1]]);
            self.start = (p[0], p[1]);
        }
        self.add_seg(2, &p);
        self.cur = (p[4], p[5]);
    }

    fn close(&mut self) {
        if !self.path.verbs.is_empty() {
            self.add_seg(3, &[]);
            self.cur = self.start;
        }
    }

    /// The path in device space, `None` when it is empty or not finite.
    fn device_path(&self, m: &Matrix) -> Option<tiny_skia::Path> {
        let mut pb = PathBuilder::new();
        let mut pts = self.path.pts.chunks_exact(2);
        let next = |pts: &mut std::slice::ChunksExact<'_, f64>| -> Option<(f32, f32)> {
            let p = pts.next()?;
            let (x, y) = apply(m, *p.first()?, *p.get(1)?);
            Some((x.clamp(-1e7, 1e7) as f32, y.clamp(-1e7, 1e7) as f32))
        };
        for &verb in &self.path.verbs {
            match verb {
                0 => {
                    let (x, y) = next(&mut pts)?;
                    pb.move_to(x, y);
                }
                1 => {
                    let (x, y) = next(&mut pts)?;
                    pb.line_to(x, y);
                }
                2 => {
                    let (x1, y1) = next(&mut pts)?;
                    let (x2, y2) = next(&mut pts)?;
                    let (x3, y3) = next(&mut pts)?;
                    pb.cubic_to(x1, y1, x2, y2, x3, y3);
                }
                _ => pb.close(),
            }
        }
        pb.finish()
    }

    /// Paint the current path (8.5.3) and apply a pending clip (8.5.4).
    fn paint(&mut self, close: bool, fill: bool, stroke: bool, rule: FillRule) -> Result<()> {
        if close {
            self.close();
        }
        let clip = self.pending_clip.take();
        if self.path.verbs.is_empty() {
            return Ok(());
        }
        let m = self.full();
        let invertible = finite(&m) && (m[0] * m[3] - m[1] * m[2]).abs() > 1e-12;
        if invertible {
            if fill
                && !self.gs.fill.none
                && let Some(p) = self.device_path(&m)
            {
                let color = self.gs.fill.rgb;
                self.fill_device(&p, rule, color, self.gs.fill_alpha)?;
            }
            if stroke && !self.gs.stroke.none {
                self.stroke_path(&m)?;
            }
        }
        if let Some(rule) = clip {
            // 8.5.4: a path that has collapsed (a singular matrix, a single point) has nothing inside it, so
            // nothing is left of the clip either.
            match invertible.then(|| self.device_path(&m)).flatten() {
                Some(p) => self.clip_device(&p, rule)?,
                None => self.gs.clip = Rc::new(Clip::new([0, 0, 0, 0], None)),
            }
        }
        self.path.clear();
        Ok(())
    }

    fn charge_area(&mut self, area: f64) -> Result<()> {
        self.area_left -= area;
        if self.area_left < 0.0 {
            return Err(Error::Limit("the page asks for more painting than is allowed".to_string()));
        }
        Ok(())
    }

    /// The part of `b` (grown by `grow` pixels) that can be seen, as integers, and whether all of it is
    /// inside the clip rectangle; `None` when nothing can be seen.
    fn visible(&self, b: tiny_skia::Rect, grow: f32) -> Option<([i32; 4], bool)> {
        let c = self.gs.clip.rect;
        let (fx0, fy0) = ((b.left() - grow).floor() as i32, (b.top() - grow).floor() as i32);
        let (fx1, fy1) = ((b.right() + grow).ceil() as i32, (b.bottom() + grow).ceil() as i32);
        let area = [fx0.max(c[0]), fy0.max(c[1]), fx1.min(c[2]), fy1.min(c[3])];
        let inside = fx0 >= c[0] && fy0 >= c[1] && fx1 <= c[2] && fy1 <= c[3];
        (area[0] < area[2] && area[1] < area[3]).then_some((area, inside))
    }

    /// The mask to draw with: none when the clip is a rectangle that contains everything drawn.
    fn mask_for(&self, inside: bool) -> Option<Rc<MaskBuf>> {
        let clip = &self.gs.clip;
        if clip.mask.is_none() && inside {
            return None;
        }
        clip.mask_for(self.pixmap.width(), self.pixmap.height(), &self.live_masks)
    }

    fn paint_of(&self, color: [f32; 3], alpha: f32) -> Paint<'static> {
        let mut paint = Paint::default();
        paint.set_color(Color::from_rgba(color[0].clamp(0.0, 1.0), color[1].clamp(0.0, 1.0), color[2].clamp(0.0, 1.0), alpha.clamp(0.0, 1.0)).unwrap_or(Color::BLACK));
        paint.anti_alias = true;
        paint
    }

    fn fill_device(&mut self, path: &tiny_skia::Path, rule: FillRule, color: [f32; 3], alpha: f32) -> Result<()> {
        if self.gs.clip.is_empty() {
            return Ok(());
        }
        let Some((area, inside)) = self.visible(path.bounds(), 1.0) else { return Ok(()) };
        self.charge_area(f64::from(area[2] - area[0]) * f64::from(area[3] - area[1]))?;
        let mask = self.mask_for(inside);
        let paint = self.paint_of(color, alpha);
        self.pixmap.fill_path(path, &paint, rule, Transform::identity(), as_mask(&mask));
        Ok(())
    }

    fn stroke_path(&mut self, m: &Matrix) -> Result<()> {
        if self.gs.clip.is_empty() {
            return Ok(());
        }
        let det = (m[0] * m[3] - m[1] * m[2]).abs();
        let scale = det.sqrt();
        // Uniform scale (a rotation and a zoom): stroke in device space. Otherwise the pen is an
        // ellipse in device space: stroke in user space and let the rasterizer transform it.
        let uniform = (m[0] * m[0] + m[1] * m[1] - (m[2] * m[2] + m[3] * m[3])).abs() <= 1e-6 * (m[0] * m[0] + m[1] * m[1]).max(1e-12)
            && (m[0] * m[2] + m[1] * m[3]).abs() <= 1e-6 * det.max(1e-12);
        let (path, transform, unit) = if uniform {
            (self.device_path(m), Transform::identity(), scale)
        } else {
            let user = self.device_path(&IDENTITY);
            (user, Transform::from_row(m[0] as f32, m[1] as f32, m[2] as f32, m[3] as f32, m[4] as f32, m[5] as f32), 1.0)
        };
        let Some(path) = path else { return Ok(()) };
        // A line is at least one device pixel wide (what the other viewers do).
        let min_width = if uniform { 1.0 } else { 1.0 / scale.max(1e-9) };
        let width = if uniform { (self.gs.line_width * unit).max(min_width) } else { self.gs.line_width.max(min_width) };
        let dash_scale = if uniform { unit as f32 } else { 1.0 };
        let mut stroke = Stroke { width: width as f32, miter_limit: self.gs.miter as f32, line_cap: self.gs.cap, line_join: self.gs.join, dash: None };
        if let Some(d) = self.gs.dash.clone() {
            let scaled: Vec<f32> = d.0.iter().map(|v| v * dash_scale).collect();
            // A short pattern on a long path is a great many dashes (and the rasterizer makes them all): count
            // them first, and draw the line solid when they are too many.
            let period: f64 = scaled.iter().map(|&v| f64::from(v)).sum();
            let count = path_length(&path) / period * scaled.len() as f64;
            if count <= MAX_STROKE_DASHES && count <= self.dashes_left {
                self.dashes_left -= count + 1.0;
                stroke.dash = StrokeDash::new(scaled, d.1 * dash_scale);
            } else {
                self.warn("a dashed line would have more dashes than is allowed; it is drawn solid");
            }
        }
        // Where it can reach, in device pixels.
        let device_bounds = if uniform {
            path.bounds()
        } else {
            let Some(p) = path.clone().transform(transform) else { return Ok(()) };
            p.bounds()
        };
        let reach = (width * if uniform { 1.0 } else { scale }) as f32 * stroke.miter_limit.clamp(1.0, 10.0) * 0.5 + 1.0;
        let Some((area, inside)) = self.visible(device_bounds, reach) else { return Ok(()) };
        self.charge_area(f64::from(area[2] - area[0]) * f64::from(area[3] - area[1]))?;
        let mask = self.mask_for(inside);
        let paint = self.paint_of(self.gs.stroke.rgb, self.gs.stroke_alpha);
        self.pixmap.stroke_path(&path, &paint, &stroke, transform, as_mask(&mask));
        Ok(())
    }

    // --- clipping --------------------------------------------------------------------------------------

    fn clip_rect_user(&mut self, r: [f64; 4]) -> Result<()> {
        let m = self.full();
        let corners = [apply(&m, r[0], r[1]), apply(&m, r[2], r[1]), apply(&m, r[2], r[3]), apply(&m, r[0], r[3])];
        let mut pb = PathBuilder::new();
        for (i, (x, y)) in corners.iter().enumerate() {
            let (x, y) = (x.clamp(-1e7, 1e7) as f32, y.clamp(-1e7, 1e7) as f32);
            if i == 0 {
                pb.move_to(x, y);
            } else {
                pb.line_to(x, y);
            }
        }
        pb.close();
        match pb.finish() {
            Some(p) => self.clip_device(&p, FillRule::Winding),
            None => {
                self.gs.clip = Rc::new(Clip::new([0, 0, 0, 0], None));
                Ok(())
            }
        }
    }

    /// Intersect the clip with a device-space path.
    fn clip_device(&mut self, path: &tiny_skia::Path, rule: FillRule) -> Result<()> {
        let old = self.gs.clip.clone();
        let b = path.bounds();
        let rect_like = axis_aligned_rect(path);
        let r = match rect_like {
            Some([x0, y0, x1, y1]) => [x0.round() as i32, y0.round() as i32, x1.round() as i32, y1.round() as i32],
            None => [b.left().floor() as i32, b.top().floor() as i32, b.right().ceil() as i32, b.bottom().ceil() as i32],
        };
        let rect = [r[0].max(old.rect[0]), r[1].max(old.rect[1]), r[2].min(old.rect[2]), r[3].min(old.rect[3])];
        if rect[0] >= rect[2] || rect[1] >= rect[3] {
            self.gs.clip = Rc::new(Clip::new([0, 0, 0, 0], None));
            return Ok(());
        }
        let (w, h) = (self.pixmap.width(), self.pixmap.height());
        let bytes = w as usize * h as usize;
        // Every mask the `q` stack keeps is a page's worth of bytes: past the cap the clip is its bounding box.
        let over_cap = self.live_masks.get().saturating_add(bytes) > MAX_LIVE_MASK_BYTES;
        if rect_like.is_some() {
            // A rectangle: only the bounds change; a mask the clip already has is cut to it.
            let mask = match &old.mask {
                Some(_) if over_cap => {
                    self.warn("clip masks use more memory than is allowed; some clips are approximated by their bounding boxes");
                    None
                }
                Some(m) => {
                    self.charge_area(f64::from(w) * f64::from(h))?;
                    let mut cut = m.mask.clone();
                    let width = w as usize;
                    for (y, row) in cut.data_mut().chunks_exact_mut(width).enumerate() {
                        let inside_y = (y as i32) >= rect[1] && (y as i32) < rect[3];
                        for (x, v) in row.iter_mut().enumerate() {
                            if !(inside_y && (x as i32) >= rect[0] && (x as i32) < rect[2]) {
                                *v = 0;
                            }
                        }
                    }
                    Some(Rc::new(MaskBuf::new(cut, Some(&self.live_masks))))
                }
                None => None,
            };
            self.gs.clip = Rc::new(Clip::new(rect, mask));
            return Ok(());
        }
        if over_cap {
            self.warn("clip masks use more memory than is allowed; some clips are approximated by their bounding boxes");
            self.gs.clip = Rc::new(Clip::new(rect, None));
            return Ok(());
        }
        self.charge_area(2.0 * f64::from(w) * f64::from(h))?;
        // The old clip's mask, or its rectangle, with the path cut out of it.
        let base = match &old.mask {
            Some(m) => Some(m.mask.clone()),
            None => rect_mask(w, h, old.rect),
        };
        let Some(mut mask) = base else { return Ok(()) };
        mask.intersect_path(path, rule, true, Transform::identity());
        self.gs.clip = Rc::new(Clip::new(rect, Some(Rc::new(MaskBuf::new(mask, Some(&self.live_masks))))));
        Ok(())
    }

    // --- colour ----------------------------------------------------------------------------------------

    fn set_device(&mut self, fill: bool, space: Arc<ColorSpace>, comps: &[f64]) {
        if self.uncolored {
            return;
        }
        let pattern = matches!(*space, ColorSpace::Pattern);
        let colour = Colour { rgb: if pattern { [0.0; 3] } else { space.to_rgb(comps, &self.meter) }, none: pattern || space.is_none(), space };
        if fill {
            self.gs.fill = colour;
        } else {
            self.gs.stroke = colour;
        }
    }

    fn set_color(&mut self, fill: bool, ops: &[Operand]) {
        if self.uncolored {
            return;
        }
        let space = if fill { self.gs.fill.space.clone() } else { self.gs.stroke.space.clone() };
        if matches!(*space, ColorSpace::Pattern) {
            self.warn("pattern fills and strokes (tiling patterns and shadings) are not drawn yet");
            return;
        }
        let n = space.components();
        let comps: Vec<f64> = ops.iter().filter_map(|o| if let Operand::Num(v) = o { Some(*v) } else { None }).collect();
        if comps.len() < n {
            return;
        }
        let tail = comps.get(comps.len() - n..).unwrap_or(&[]);
        self.set_device(fill, space, tail);
    }

    fn load_space(&mut self, name: &[u8], res: &Rc<Resources>) -> Option<Arc<ColorSpace>> {
        match name {
            b"DeviceGray" | b"G" => return Some(self.gray.clone()),
            b"DeviceRGB" | b"RGB" => return Some(self.rgb.clone()),
            b"DeviceCMYK" | b"CMYK" => return Some(self.cmyk.clone()),
            b"Pattern" => return Some(Arc::new(ColorSpace::Pattern)),
            _ => {}
        }
        if let Some(hit) = res.loaded_spaces.borrow().get(name) {
            return hit.clone();
        }
        let doc = self.doc;
        let lookup = |n: &[u8]| res.get(doc, Cat::ColorSpace, n);
        let loaded = lookup(name).and_then(|o| ColorSpace::load(doc, &o, &lookup, &self.meter));
        if loaded.is_none() {
            self.warn(format!("colour space /{} could not be read; the colour is left as it was", String::from_utf8_lossy(name)));
        }
        res.loaded_spaces.borrow_mut().insert(name.to_vec(), loaded.clone());
        loaded
    }

    // --- graphics state dictionary (8.4.5) -------------------------------------------------------------

    fn set_dash(&mut self, ops: &[Operand]) {
        // [array] phase d
        let mut values: Vec<f32> = Vec::new();
        let mut inside = false;
        let mut phase = 0.0;
        for o in ops {
            match o {
                Operand::ArrayStart => {
                    inside = true;
                    values.clear();
                }
                Operand::ArrayEnd => inside = false,
                Operand::Num(v) if inside => values.push(*v as f32),
                Operand::Num(v) => phase = *v as f32,
                _ => {}
            }
        }
        self.gs.dash = dash_of(values, phase).map(Rc::new);
    }

    fn ext_g_state(&mut self, name: &[u8], res: &Rc<Resources>) {
        let doc = self.doc;
        let Some(entry) = res.get(doc, Cat::ExtGState, name) else { return };
        let Ok(Object::Dict(d)) = doc.resolve(&entry) else { return };
        let num = |k: &str| d.get(k).and_then(|o| doc.resolve(o).ok()).and_then(|o| o.as_f64());
        if let Some(v) = num("LW") {
            self.gs.line_width = v.abs();
        }
        if let Some(v) = num("LC") {
            self.gs.cap = cap_of(v as i64);
        }
        if let Some(v) = num("LJ") {
            self.gs.join = join_of(v as i64);
        }
        if let Some(v) = num("ML") {
            self.gs.miter = v.max(1.0);
        }
        if let Some(v) = num("CA") {
            self.gs.stroke_alpha = v.clamp(0.0, 1.0) as f32;
        }
        if let Some(v) = num("ca") {
            self.gs.fill_alpha = v.clamp(0.0, 1.0) as f32;
        }
        if let Some(Object::Array(a)) = d.get("D").and_then(|o| doc.resolve(o).ok()) {
            let list = a.first().and_then(|o| doc.resolve(o).ok());
            let phase = a.get(1).and_then(|o| doc.resolve(o).ok()).and_then(|o| o.as_f64()).unwrap_or(0.0) as f32;
            if let Some(Object::Array(items)) = list {
                let values: Vec<f32> = items.iter().filter_map(|o| doc.resolve(o).ok().and_then(|o| o.as_f64())).map(|v| v as f32).collect();
                self.gs.dash = dash_of(values, phase).map(Rc::new);
            }
        }
        if let Some(Object::Array(a)) = d.get("Font").and_then(|o| doc.resolve(o).ok())
            && let (Some(Object::Ref(r)), Some(size)) = (a.first(), a.get(1).and_then(|o| doc.resolve(o).ok()).and_then(|o| o.as_f64()))
        {
            self.gs.font = self.font_from_ref(*r);
            self.gs.size = size;
        }
        let blend = d.get("BM").and_then(|o| doc.resolve(o).ok());
        let soft_mask = d.get("SMask").and_then(|o| doc.resolve(o).ok());
        if matches!(&blend, Some(Object::Name(n)) if n.as_bytes() != b"Normal" && n.as_bytes() != b"Compatible")
            || matches!(&soft_mask, Some(Object::Dict(_)))
        {
            self.warn("blend modes and soft masks in graphics state dictionaries are not drawn yet");
        }
    }

    // --- text ------------------------------------------------------------------------------------------

    fn move_line(&mut self, tx: f64, ty: f64) {
        let m = mul(&[1.0, 0.0, 0.0, 1.0, tx, ty], &self.tlm);
        if finite(&m) {
            self.tlm = m;
            self.tm = m;
        }
    }

    /// A number in a `TJ` array (9.4.3).
    fn adjust(&mut self, n: f64) {
        let vertical = self.gs.font.as_ref().is_some_and(|f| f.font.vertical);
        if vertical {
            let ty = -n / 1000.0 * self.gs.size;
            self.tm[4] += ty * self.tm[2];
            self.tm[5] += ty * self.tm[3];
        } else {
            let tx = -n / 1000.0 * self.gs.size * self.gs.th;
            self.tm[4] += tx * self.tm[0];
            self.tm[5] += tx * self.tm[1];
        }
        if !finite(&self.tm) {
            self.tm = IDENTITY;
        }
    }

    fn font(&mut self, name: &[u8], res: &Rc<Resources>) -> Option<Rc<FontEntry>> {
        let Some(entry) = res.get(self.doc, Cat::Font, name) else {
            self.warn(format!("font /{} is not in the resources", String::from_utf8_lossy(name)));
            return None;
        };
        match entry {
            Object::Ref(r) => self.font_from_ref(r),
            Object::Dict(d) => Some(Rc::new(self.load_font(&d))),
            _ => None,
        }
    }

    fn font_from_ref(&mut self, r: ObjRef) -> Option<Rc<FontEntry>> {
        if let Some(hit) = self.shared.fonts.get(&r) {
            return hit.clone();
        }
        let loaded = match self.doc.get(r) {
            Ok(Object::Dict(d)) => Some(Rc::new(self.load_font(&d))),
            Ok(_) => None,
            Err(e) => {
                self.warn(format!("font object {} {} R could not be read: {e}", r.num, r.generation));
                None
            }
        };
        self.shared.fonts.insert(r, loaded.clone());
        loaded
    }

    fn load_font(&mut self, d: &Dict) -> FontEntry {
        let doc = self.doc;
        let font = Font::load(doc, d, &mut self.shared.cmaps, &mut self.shared.warnings);
        let type3 = if d.get_name("Subtype").is_some_and(|n| n.as_bytes() == b"Type3") { self.load_type3(d) } else { None };
        FontEntry { font, type3 }
    }

    fn load_type3(&mut self, d: &Dict) -> Option<Type3> {
        let doc = self.doc;
        let matrix = match d.get("FontMatrix").and_then(|o| doc.resolve(o).ok()) {
            Some(Object::Array(a)) if a.len() == 6 => {
                let mut m = [0.001, 0.0, 0.0, 0.001, 0.0, 0.0];
                for (slot, v) in m.iter_mut().zip(&a) {
                    *slot = doc.resolve(v).ok().and_then(|o| o.as_f64()).filter(|v| v.is_finite()).unwrap_or(*slot);
                }
                m
            }
            _ => [0.001, 0.0, 0.0, 0.001, 0.0, 0.0],
        };
        let mut names: Vec<Option<Vec<u8>>> = vec![None; 256];
        if let Some(Object::Dict(enc)) = d.get("Encoding").and_then(|o| doc.resolve(o).ok())
            && let Some(Object::Array(items)) = enc.get("Differences").and_then(|o| doc.resolve(o).ok())
        {
            let mut code: Option<usize> = None;
            for item in &items {
                match doc.resolve(item).ok() {
                    Some(Object::Integer(n)) => code = usize::try_from(n).ok(),
                    Some(Object::Name(n)) => {
                        if let Some(c) = code {
                            if let Some(slot) = names.get_mut(c) {
                                *slot = Some(n.as_bytes().to_vec());
                            }
                            code = Some(c + 1);
                        }
                    }
                    _ => {}
                }
            }
        }
        let procs = match d.get("CharProcs").and_then(|o| doc.resolve(o).ok()) {
            Some(Object::Dict(p)) => p.into_pairs().into_iter().map(|(k, v)| (k.0, v)).collect(),
            _ => return None,
        };
        let resources = d.get("Resources").map(|r| Resources::new(doc, Some(r)));
        Some(Type3 { matrix, names, procs, resources, cache: RefCell::new(FxMap::default()) })
    }

    /// Show strings (9.4.4): a Type 3 glyph is drawn, any other character as an outline box.
    fn show(&mut self, strings: &[&[u8]], res: &Rc<Resources>, depth: usize) -> Result<()> {
        let Some(entry) = self.gs.font.clone() else { return Ok(()) };
        let (size, tc, tw, th, rise) = (self.gs.size, self.gs.tc, self.gs.tw, self.gs.th, self.gs.rise);
        // The outline boxes stand in for glyphs, which neither show in mode 3 nor in mode 7 (which clips instead);
        // a Type 3 glyph is only hidden by mode 3 (9.3.6: no other mode has any effect on it).
        let visible = !matches!(self.gs.mode, 3 | 7);
        let type3_visible = self.gs.mode != 3;
        let full = self.full();
        let mut boxes = PathBuilder::new();
        let mut any_box = false;
        for s in strings {
            let mut chars = std::mem::take(&mut self.chars);
            chars.clear();
            entry.font.show(s, |ch| {
                chars.push(Shown {
                    code: ch.code,
                    w0: ch.w0,
                    vertical: ch.vertical,
                    is_space: ch.is_space_code,
                    blank: is_blank(&ch.uni),
                });
            });
            for ch in &chars {
                // A glyph is work even if it draws nothing (a Type 3 glyph may show strings of its own).
                self.charge_op()?;
                let tx_ty = match ch.vertical {
                    None => {
                        let tx = (ch.w0 * size + tc + if ch.is_space { tw } else { 0.0 }) * th;
                        (tx * self.tm[0], tx * self.tm[1])
                    }
                    Some((w1, _, _)) => {
                        let ty = w1 * size + tc + if ch.is_space { tw } else { 0.0 };
                        (ty * self.tm[2], ty * self.tm[3])
                    }
                };
                if let Some(t3) = &entry.type3 {
                    if type3_visible {
                        self.type3_glyph(t3, ch.code, res, depth)?;
                    }
                } else if visible && !ch.blank && ch.w0 > 0.0 && size != 0.0 {
                    // The box of the character in text space: its width, from a bit below the
                    // baseline to a bit above the x-height of an average font.
                    let (x0, x1, y0, y1) = match ch.vertical {
                        None => (0.0, ch.w0 * size * th, rise - 0.2 * size, rise + 0.8 * size),
                        Some((_, vx, vy)) => (-vx * size, (-vx + ch.w0) * size, rise - vy * size - 0.2 * size, rise - vy * size + 0.8 * size),
                    };
                    let tm_ctm = mul(&self.tm, &full);
                    let pts = [apply(&tm_ctm, x0, y0), apply(&tm_ctm, x1, y0), apply(&tm_ctm, x1, y1), apply(&tm_ctm, x0, y1)];
                    self.boxed += 1;
                    // Text that runs straight across the page: the box is drawn right here, pixel by pixel.
                    let upright = tm_ctm[1].abs() <= 1e-6 * tm_ctm[0].abs().max(1e-9) && tm_ctm[2].abs() <= 1e-6 * tm_ctm[3].abs().max(1e-9);
                    if !(upright && self.draw_box_fast(&pts)) {
                        for (i, (x, y)) in pts.iter().enumerate() {
                            let (x, y) = (x.clamp(-1e7, 1e7) as f32, y.clamp(-1e7, 1e7) as f32);
                            if i == 0 {
                                boxes.move_to(x, y);
                            } else {
                                boxes.line_to(x, y);
                            }
                        }
                        boxes.close();
                        any_box = true;
                    }
                }
                self.tm[4] += tx_ty.0;
                self.tm[5] += tx_ty.1;
            }
            self.chars = chars;
        }
        if !finite(&self.tm) {
            self.tm = IDENTITY;
        }
        if any_box && let Some(path) = boxes.finish() {
            self.stroke_boxes(&path)?;
        }
        Ok(())
    }

    /// One pixel wide outline of an upright box, written straight into the page; false when the
    /// clip is not a rectangle that holds the whole box (then it is drawn as a path).
    fn draw_box_fast(&mut self, pts: &[(f64, f64); 4]) -> bool {
        if self.gs.mode == 3 || self.gs.mode == 7 || self.gs.clip.mask.is_some() {
            return false;
        }
        let colour = if matches!(self.gs.mode, 1 | 5) { &self.gs.stroke } else { &self.gs.fill };
        if colour.none {
            return true;
        }
        let alpha = if matches!(self.gs.mode, 1 | 5) { self.gs.stroke_alpha } else { self.gs.fill_alpha };
        let rgb = colour.rgb;
        let (mut lx, mut ty, mut rx, mut by) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for &(x, y) in pts {
            lx = lx.min(x);
            ty = ty.min(y);
            rx = rx.max(x);
            by = by.max(y);
        }
        if !(lx.is_finite() && ty.is_finite() && rx.is_finite() && by.is_finite()) || lx < -1e6 || ty < -1e6 || rx > 1e6 || by > 1e6 {
            return false;
        }
        let (x0, y0) = (lx.round() as i32, ty.round() as i32);
        let (x1, y1) = ((rx.round() as i32).max(x0 + 1), (by.round() as i32).max(y0 + 1));
        let c = self.gs.clip.rect;
        if x0 < c[0] || y0 < c[1] || x1 > c[2] || y1 > c[3] {
            return false;
        }
        if self.charge_area(f64::from(2 * (x1 - x0 + y1 - y0))).is_err() {
            return false;
        }
        let w = self.pixmap.width() as usize;
        let a = alpha.clamp(0.0, 1.0);
        let src = [rgb[0], rgb[1], rgb[2]].map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32);
        let data = self.pixmap.data_mut();
        let mut put = |x: i32, y: i32| {
            let i = (y as usize * w + x as usize) * 4;
            if let Some(px) = data.get_mut(i..i + 4) {
                if a >= 0.999 {
                    px.copy_from_slice(&[src[0] as u8, src[1] as u8, src[2] as u8, 255]);
                } else {
                    for (slot, s) in px.iter_mut().zip([src[0], src[1], src[2], 255]) {
                        *slot = (f32::from(*slot) * (1.0 - a) + s as f32 * a + 0.5).min(255.0) as u8;
                    }
                }
            }
        };
        for x in x0..x1 {
            put(x, y0);
            put(x, y1 - 1);
        }
        for y in y0 + 1..y1 - 1 {
            put(x0, y);
            put(x1 - 1, y);
        }
        true
    }

    fn stroke_boxes(&mut self, path: &tiny_skia::Path) -> Result<()> {
        if self.gs.clip.is_empty() {
            return Ok(());
        }
        let (colour, alpha) = if matches!(self.gs.mode, 1 | 5) { (self.gs.stroke.clone(), self.gs.stroke_alpha) } else { (self.gs.fill.clone(), self.gs.fill_alpha) };
        if colour.none {
            return Ok(());
        }
        let Some((area, inside)) = self.visible(path.bounds(), 1.0) else { return Ok(()) };
        self.charge_area(f64::from(area[2] - area[0]) * f64::from(area[3] - area[1]))?;
        let mask = self.mask_for(inside);
        let paint = self.paint_of(colour.rgb, alpha);
        let stroke = Stroke { width: 1.0, ..Stroke::default() };
        self.pixmap.stroke_path(path, &paint, &stroke, Transform::identity(), as_mask(&mask));
        Ok(())
    }

    fn type3_glyph(&mut self, t3: &Type3, code: u32, res: &Rc<Resources>, depth: usize) -> Result<()> {
        let Ok(code) = u8::try_from(code) else { return Ok(()) };
        let cached = t3.cache.borrow().get(&code).cloned();
        let content = match cached {
            Some(c) => c,
            None => {
                let loaded = self.load_glyph_proc(t3, code)?;
                t3.cache.borrow_mut().insert(code, loaded.clone());
                loaded
            }
        };
        let Some(content) = content else { return Ok(()) };
        if depth + 1 > MAX_FORM_DEPTH {
            self.warn(format!("Type 3 glyphs nested more than {MAX_FORM_DEPTH} deep; the deeper ones are skipped"));
            return Ok(());
        }
        let (size, th, rise) = (self.gs.size, self.gs.th, self.gs.rise);
        let m = mul(&mul(&mul(&t3.matrix, &[size * th, 0.0, 0.0, size, 0.0, rise]), &self.tm), &self.gs.ctm);
        if !finite(&m) {
            return Ok(());
        }
        let saved = (self.gs.clone(), self.stack.len(), self.tm, self.tlm, self.uncolored);
        let floor = std::mem::replace(&mut self.stack_floor, self.stack.len());
        self.gs.ctm = m;
        self.uncolored = false;
        let inner = t3.resources.clone().unwrap_or_else(|| res.clone());
        let path_saved = std::mem::take(&mut self.path);
        let result = self.run(&content, &inner, depth + 1);
        self.path = path_saved;
        self.stack_floor = floor;
        self.stack.truncate(saved.1);
        self.gs = saved.0;
        self.tm = saved.2;
        self.tlm = saved.3;
        self.uncolored = saved.4;
        result
    }

    fn load_glyph_proc(&mut self, t3: &Type3, code: u8) -> Result<Option<Rc<Vec<u8>>>> {
        let Some(Some(name)) = t3.names.get(usize::from(code)) else { return Ok(None) };
        let Some(entry) = t3.procs.get(name) else { return Ok(None) };
        let obj = match self.doc.resolve(entry) {
            Ok(o) => o,
            Err(Error::Limit(m)) => return Err(Error::Limit(m)),
            Err(_) => return Ok(None),
        };
        let Object::Stream(s) = obj else { return Ok(None) };
        match self.doc.decode_stream(&s) {
            Ok(data) => {
                self.shared.cache_bytes = self.shared.cache_bytes.saturating_add(data.len());
                Ok(Some(Rc::new(data)))
            }
            Err(Error::Limit(m)) => Err(Error::Limit(m)),
            Err(e) => {
                self.warn(format!("a Type 3 glyph procedure is skipped: {e}"));
                Ok(None)
            }
        }
    }

    // --- XObjects --------------------------------------------------------------------------------------

    fn do_xobject(&mut self, name: &[u8], res: &Rc<Resources>, depth: usize) -> Result<()> {
        let doc = self.doc;
        let Some(entry) = res.get(doc, Cat::XObject, name) else { return Ok(()) };
        let r = entry.as_obj_ref();
        // An image already prepared at this size.
        if let Some(r) = r {
            if let Some(place) = self.placement() {
                let key = self.image_key(r, place.target);
                if let Some(hit) = self.shared.images.get(&key).cloned() {
                    return self.draw_prepared(&hit, &place);
                }
            }
            if let Some(Some(form)) = self.shared.forms.get(&r) {
                let form = form.clone();
                return self.run_form(r, &form, res, depth);
            }
        }
        let obj = match doc.resolve(&entry) {
            Ok(o) => o,
            Err(Error::Limit(m)) => return Err(Error::Limit(m)),
            Err(e) => {
                self.warn(format!("XObject /{} could not be read: {e}", String::from_utf8_lossy(name)));
                return Ok(());
            }
        };
        let Object::Stream(stream) = obj else { return Ok(()) };
        let subtype = stream.dict.get_name("Subtype").map(|n| n.as_bytes().to_vec());
        match subtype.as_deref() {
            Some(b"Image") => self.draw_image(&stream, r, res),
            Some(b"Form") => self.draw_form(&stream, r, res, depth),
            None if stream.dict.contains_key("BBox") => self.draw_form(&stream, r, res, depth),
            _ => Ok(()),
        }
    }

    fn draw_form(&mut self, stream: &Stream, r: Option<ObjRef>, res: &Rc<Resources>, depth: usize) -> Result<()> {
        let Some(form) = self.load_form(stream, r)? else { return Ok(()) };
        self.run_form(r.unwrap_or(ObjRef::new(0, 0)), &form, res, depth)
    }

    fn image_key(&self, r: ObjRef, target: (usize, usize)) -> ImageKey {
        let fill = self.gs.fill.rgb.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
        ImageKey { num: r.num, generation: r.generation, target, fill }
    }

    /// Where the unit square lands on the device. An image that is not turned is put on whole pixels
    /// (the outer rectangle of where it falls), as the other viewers do: no half-covered row or column at
    /// its edges, and no seams between neighbours.
    fn placement(&self) -> Option<Placement> {
        let m = self.full();
        if !finite(&m) {
            return None;
        }
        if m[1].abs() <= 1e-6 * m[0].abs().max(1e-9) && m[2].abs() <= 1e-6 * m[3].abs().max(1e-9) && m[0] != 0.0 && m[3] != 0.0 {
            let span = |origin: f64, step: f64| -> (i32, i32) {
                let (lo, hi) = if step > 0.0 { (origin, origin + step) } else { (origin + step, origin) };
                let lo = (lo + 1e-3).floor().clamp(-1e7, 1e7) as i32;
                let hi = ((hi - 1e-3).ceil().clamp(-1e7, 1e7) as i32).max(lo + 1);
                (lo, hi)
            };
            let (x0, x1) = span(m[4], m[0]);
            let (y0, y1) = span(m[5], m[3]);
            let target = (usize::try_from(x1 - x0).unwrap_or(1), usize::try_from(y1 - y0).unwrap_or(1));
            return Some(Placement { rect: Some([x0, y0, x1, y1]), flip_x: m[0] < 0.0, flip_y: m[3] > 0.0, target });
        }
        let side = |a: f64, b: f64| (a.hypot(b).ceil().clamp(1.0, 1e7)) as usize;
        Some(Placement { rect: None, flip_x: false, flip_y: false, target: (side(m[0], m[1]), side(m[2], m[3])) })
    }

    fn load_form(&mut self, stream: &Stream, r: Option<ObjRef>) -> Result<Option<Rc<Form>>> {
        let doc = self.doc;
        let content = match doc.decode_stream(stream) {
            Ok(c) => c,
            Err(Error::Limit(m)) => return Err(Error::Limit(m)),
            Err(e) => {
                self.warn(format!("a form is skipped: {e}"));
                return Ok(None);
            }
        };
        let matrix = match stream.dict.get("Matrix").and_then(|o| doc.resolve(o).ok()) {
            Some(Object::Array(a)) if a.len() == 6 => {
                let mut m = IDENTITY;
                for (slot, v) in m.iter_mut().zip(&a) {
                    *slot = doc.resolve(v).ok().and_then(|o| o.as_f64()).unwrap_or(*slot);
                }
                if finite(&m) { m } else { IDENTITY }
            }
            _ => IDENTITY,
        };
        let bbox = crate::document::rectangle(stream.dict.get("BBox").and_then(|o| doc.resolve(o).ok()).as_ref());
        let resources = stream.dict.get("Resources").map(|res| Resources::new(doc, Some(res)));
        let form = Rc::new(Form { content, matrix, bbox, resources });
        if let Some(r) = r
            && self.shared.cache_bytes.saturating_add(form.content.len()) <= MAX_CACHE_BYTES
        {
            self.shared.cache_bytes += form.content.len();
            self.shared.forms.insert(r, Some(form.clone()));
        }
        Ok(Some(form))
    }

    fn run_form(&mut self, r: ObjRef, form: &Rc<Form>, res: &Rc<Resources>, depth: usize) -> Result<()> {
        if depth + 1 > MAX_FORM_DEPTH {
            self.warn(format!("forms are nested more than {MAX_FORM_DEPTH} deep; the deeper ones are skipped"));
            return Ok(());
        }
        if r.num != 0 && self.in_progress.contains(&r) {
            self.warn(format!("form {} {} R contains itself; the repeat is skipped", r.num, r.generation));
            return Ok(());
        }
        let saved = (self.gs.clone(), self.stack.len(), self.tm, self.tlm);
        let floor = std::mem::replace(&mut self.stack_floor, self.stack.len());
        let m = mul(&form.matrix, &self.gs.ctm);
        if finite(&m) {
            self.gs.ctm = m;
        }
        if let Some(b) = form.bbox
            && let Err(Error::Limit(m)) = self.clip_rect_user(b)
        {
            // No budget left to clip it: the form is not drawn (nothing of it may fall outside its box).
            self.warn(format!("{m}; a form is skipped"));
            self.gs.clip = Rc::new(Clip::new([0, 0, 0, 0], None));
        }
        let path_saved = std::mem::take(&mut self.path);
        self.in_progress.push(r);
        let inner = form.resources.clone().unwrap_or_else(|| res.clone());
        let result = if self.gs.clip.is_empty() { Ok(()) } else { self.run(&form.content, &inner, depth + 1) };
        self.in_progress.pop();
        self.path = path_saved;
        self.stack_floor = floor;
        self.stack.truncate(saved.1);
        self.gs = saved.0;
        self.tm = saved.2;
        self.tlm = saved.3;
        result
    }

    // --- images ----------------------------------------------------------------------------------------

    fn draw_image(&mut self, stream: &Stream, r: Option<ObjRef>, res: &Rc<Resources>) -> Result<()> {
        if self.gs.clip.is_empty() {
            return Ok(());
        }
        let doc = self.doc;
        let Some(place) = self.placement() else { return Ok(()) };
        let lookup = |n: &[u8]| res.get(doc, Cat::ColorSpace, n);
        // The loader counts the pixels it decodes (the image's, its mask's, a JPEG's own size) against the page.
        let pixels_left = Cell::new(self.image_pixels_left);
        let env = image::Env { doc, lookup: &lookup, fill: self.gs.fill.rgb, target: place.target, meter: &self.meter, pixels_left: &pixels_left };
        let loaded = image::load(&env, stream, &mut self.shared.warnings);
        self.image_pixels_left = pixels_left.get();
        let loaded = match loaded {
            Ok(l) => l,
            Err(Error::Limit(m)) => return Err(Error::Limit(m)),
            Err(e) => {
                self.warn(format!("an image is skipped: {e}"));
                return Ok(());
            }
        };
        match loaded {
            Loaded::Placeholder => self.draw_placeholder(),
            Loaded::Image { pixmap, opaque } => {
                let prepared = Rc::new(Prepared { pixmap, opaque });
                let bytes = prepared.pixmap.data().len();
                if let Some(r) = r
                    && bytes <= MAX_IMAGE_CACHE_BYTES / 4
                {
                    if self.shared.image_bytes + bytes > MAX_IMAGE_CACHE_BYTES {
                        self.shared.images.clear();
                        self.shared.image_bytes = 0;
                    }
                    self.shared.image_bytes += bytes;
                    let key = self.image_key(r, place.target);
                    self.shared.images.insert(key, prepared.clone());
                }
                self.draw_prepared(&prepared, &place)
            }
        }
    }

    /// Fill the unit square with a grey, for an image we cannot decode.
    fn draw_placeholder(&mut self) -> Result<()> {
        let saved = std::mem::take(&mut self.path);
        for (verb, pts) in [(0u8, [0.0, 0.0]), (1, [1.0, 0.0]), (1, [1.0, 1.0]), (1, [0.0, 1.0])] {
            self.add_seg(verb, &pts);
        }
        self.add_seg(3, &[]);
        let m = self.full();
        let result = match self.device_path(&m) {
            Some(p) => self.fill_device(&p, FillRule::Winding, [0.75; 3], 1.0),
            None => Ok(()),
        };
        self.path = saved;
        result
    }

    /// Draw a prepared image over the unit square.
    fn draw_prepared(&mut self, prepared: &Prepared, place: &Placement) -> Result<()> {
        if self.gs.clip.is_empty() {
            return Ok(());
        }
        let pixmap = &prepared.pixmap;
        let (iw, ih) = (f64::from(pixmap.width()), f64::from(pixmap.height()));
        // Exactly the size of its place: put its pixels there as they are.
        if let Some(rect) = place.rect
            && usize::try_from(pixmap.width()).ok() == Some(place.target.0)
            && usize::try_from(pixmap.height()).ok() == Some(place.target.1)
            && self.gs.fill_alpha >= 0.999
        {
            return self.blit(pixmap, prepared.opaque, rect, place.flip_x, place.flip_y);
        }
        // Image pixels to the unit square (the first row is at the top), then to the device.
        let mut full = self.full();
        if let Some([x0, y0, x1, y1]) = place.rect {
            let (w, h) = (f64::from(x1 - x0), f64::from(y1 - y0));
            full = [
                if place.flip_x { -w } else { w },
                0.0,
                0.0,
                if place.flip_y { h } else { -h },
                f64::from(if place.flip_x { x1 } else { x0 }),
                f64::from(if place.flip_y { y0 } else { y1 }),
            ];
        }
        let m = mul(&[1.0 / iw, 0.0, 0.0, -1.0 / ih, 0.0, 1.0], &full);
        if !finite(&m) {
            return Ok(());
        }
        let corners = [apply(&m, 0.0, 0.0), apply(&m, iw, 0.0), apply(&m, iw, ih), apply(&m, 0.0, ih)];
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for (x, y) in corners {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
        let Some(bounds) = tiny_skia::Rect::from_ltrb(
            x0.clamp(-1e7, 1e7) as f32,
            y0.clamp(-1e7, 1e7) as f32,
            x1.clamp(-1e7, 1e7) as f32,
            y1.clamp(-1e7, 1e7) as f32,
        ) else {
            return Ok(());
        };
        let Some((area, inside)) = self.visible(bounds, 1.0) else { return Ok(()) };
        self.charge_area(f64::from(area[2] - area[0]) * f64::from(area[3] - area[1]))?;
        let scale = m[0].hypot(m[1]).max(m[2].hypot(m[3]));
        let quality = if scale > 1.0 { FilterQuality::Nearest } else { FilterQuality::Bilinear };
        let paint = PixmapPaint { opacity: self.gs.fill_alpha, blend_mode: BlendMode::SourceOver, quality };
        let transform = Transform::from_row(m[0] as f32, m[1] as f32, m[2] as f32, m[3] as f32, m[4] as f32, m[5] as f32);
        let mask = self.mask_for(inside);
        self.pixmap.draw_pixmap(0, 0, pixmap.as_ref(), &paint, transform, as_mask(&mask));
        Ok(())
    }

    /// Put an image on the page pixel for pixel (it is as big as `rect`): a copy when it is opaque and
    /// nothing but the clip rectangle cuts it, else source-over with the clip mask.
    fn blit(&mut self, image: &Pixmap, opaque: bool, rect: [i32; 4], flip_x: bool, flip_y: bool) -> Result<()> {
        let clip = self.gs.clip.clone();
        let c = clip.rect;
        let area = [rect[0].max(c[0]), rect[1].max(c[1]), rect[2].min(c[2]), rect[3].min(c[3])];
        if area[0] >= area[2] || area[1] >= area[3] {
            return Ok(());
        }
        self.charge_area(f64::from(area[2] - area[0]) * f64::from(area[3] - area[1]))?;
        let page_w = self.pixmap.width() as usize;
        let (dw, dh) = ((rect[2] - rect[0]) as usize, (rect[3] - rect[1]) as usize);
        let src = image.data();
        let mask = clip.mask.as_ref().map(|m| m.mask.data());
        let dst = self.pixmap.data_mut();
        let (x0, x1) = (area[0] as usize, area[2] as usize);
        for y in area[1]..area[3] {
            let row = (y - rect[1]) as usize;
            let sy = if flip_y { dh - 1 - row } else { row };
            let line = y as usize * page_w;
            let (Some(src_row), Some(dst_row)) = (src.get(sy * dw * 4..(sy + 1) * dw * 4), dst.get_mut((line + x0) * 4..(line + x1) * 4)) else {
                continue;
            };
            let mask_row = mask.and_then(|m| m.get(line + x0..line + x1));
            let first = (area[0] - rect[0]) as usize;
            if opaque && mask_row.is_none() && !flip_x {
                if let Some(from) = src_row.get(first * 4..first * 4 + (x1 - x0) * 4) {
                    dst_row.copy_from_slice(from);
                }
                continue;
            }
            for (i, d) in dst_row.chunks_exact_mut(4).enumerate() {
                let sx = if flip_x { dw - 1 - (first + i) } else { first + i };
                let Some(&[r, g, b, a]) = src_row.get(sx * 4..sx * 4 + 4).and_then(|p| <&[u8; 4]>::try_from(p).ok()) else { continue };
                let k = u32::from(mask_row.and_then(|m| m.get(i)).copied().unwrap_or(255));
                let scale = |v: u8| -> u32 { if k == 255 { u32::from(v) } else { (u32::from(v) * k + 127) / 255 } };
                let (r, g, b, a) = (scale(r), scale(g), scale(b), scale(a));
                if a == 255 {
                    d.copy_from_slice(&[r as u8, g as u8, b as u8, 255]);
                } else if a != 0 {
                    for (slot, v) in d.iter_mut().zip([r, g, b, a]) {
                        *slot = (v + (u32::from(*slot) * (255 - a) + 127) / 255).min(255) as u8;
                    }
                }
            }
        }
        Ok(())
    }

    /// `BI` (8.9.7): the dictionary up to `ID`, the data up to `EI`.
    fn inline_image(&mut self, sc: &mut Scanner<'_>, ops: &mut Vec<Operand>, res: &Rc<Resources>) -> Result<()> {
        let mut dict = Dict::new();
        // The key/value pairs arrive as the operands of `ID`, `true`, `false` and `null` (which the
        // scanner calls operators) among them.
        let mut flat: Vec<Object> = Vec::new();
        let mut found_id = false;
        for _ in 0..64 {
            let keyword = match sc.next(ops) {
                Item::Operator(k) => k,
                Item::End => return Ok(()),
            };
            push_operands(ops, sc, &mut flat);
            match keyword {
                b"ID" => {
                    found_id = true;
                    break;
                }
                b"true" => flat.push(Object::Bool(true)),
                b"false" => flat.push(Object::Bool(false)),
                b"null" => flat.push(Object::Null),
                _ => {}
            }
        }
        if !found_id {
            return Ok(());
        }
        for pair in flat.chunks_exact(2) {
            if let [Object::Name(k), v] = pair {
                dict.set(k.clone(), v.clone());
            }
        }
        let dict = image::expand_inline(&dict);
        let w = dict.get("Width").and_then(Object::as_int).unwrap_or(0).max(0) as usize;
        let h = dict.get("Height").and_then(Object::as_int).unwrap_or(0).max(0) as usize;
        // Without a filter the data has a known length.
        let known = if dict.contains_key("Filter") {
            None
        } else {
            let bpc = if matches!(dict.get("ImageMask"), Some(Object::Bool(true))) { 1 } else { dict.get("BitsPerComponent").and_then(Object::as_int).unwrap_or(8).max(1) as usize };
            let comps = if matches!(dict.get("ImageMask"), Some(Object::Bool(true))) {
                1
            } else {
                match dict.get("ColorSpace") {
                    Some(Object::Name(n)) => match n.as_bytes() {
                        b"DeviceGray" | b"CalGray" => 1,
                        b"DeviceCMYK" => 4,
                        b"DeviceRGB" | b"CalRGB" => 3,
                        _ => 0,
                    },
                    Some(Object::Array(_)) => 1,
                    _ => 0,
                }
            };
            // (the numbers come from the file: no overflow may panic or wrap)
            w.checked_mul(comps).and_then(|n| n.checked_mul(bpc)).filter(|_| comps > 0).map(|bits| bits.div_ceil(8).saturating_mul(h))
        };
        let data = sc.inline_image_data(known).to_vec();
        if self.gs.clip.is_empty() {
            return Ok(());
        }
        let stream = Stream { dict, data };
        self.draw_image(&stream, None, res)
    }
}

/// A character that is white space draws no box (an unknown one does).
fn is_blank(u: &Uni) -> bool {
    match u {
        Uni::None => false,
        Uni::One(c) => c.is_whitespace(),
        Uni::Many(t) => t.chars().all(char::is_whitespace),
    }
}

/// Operands as objects: arrays are made, a dictionary is parsed from its bytes.
fn push_operands(ops: &[Operand], sc: &Scanner<'_>, flat: &mut Vec<Object>) {
    let mut open: Vec<Vec<Object>> = Vec::new();
    let mut current = std::mem::take(flat);
    for op in ops {
        let value = match op {
            Operand::Num(v) => Some(if v.fract() == 0.0 && v.abs() < 1e15 { Object::Integer(*v as i64) } else { Object::Real(*v) }),
            Operand::Name(s) => Some(Object::Name(crate::object::Name::new(sc.bytes(*s).to_vec()))),
            Operand::Str(s) => Some(Object::String(crate::object::PdfString::literal(sc.bytes(*s).to_vec()))),
            Operand::Dict(s) => {
                let bytes = sc.bytes(*s).to_vec();
                crate::parser::Parser::new(&bytes, 0).parse_object().ok()
            }
            Operand::ArrayStart => {
                if open.len() < 16 {
                    open.push(std::mem::take(&mut current));
                }
                None
            }
            Operand::ArrayEnd => open.pop().map(|outer| Object::Array(std::mem::replace(&mut current, outer))),
        };
        if let Some(v) = value {
            current.push(v);
        }
    }
    // An array left open is closed at the end.
    while let Some(outer) = open.pop() {
        let items = std::mem::replace(&mut current, outer);
        current.push(Object::Array(items));
    }
    *flat = current;
}

fn cap_of(v: i64) -> LineCap {
    match v {
        1 => LineCap::Round,
        2 => LineCap::Square,
        _ => LineCap::Butt,
    }
}

fn join_of(v: i64) -> LineJoin {
    match v {
        1 => LineJoin::Round,
        2 => LineJoin::Bevel,
        _ => LineJoin::Miter,
    }
}

/// A dash pattern (8.4.3.6); an odd count repeats to make it even. `None`: a solid line.
fn dash_of(mut values: Vec<f32>, phase: f32) -> Option<(Vec<f32>, f32)> {
    values.truncate(64);
    if values.is_empty() || values.iter().any(|v| !v.is_finite() || *v < 0.0) || values.iter().sum::<f32>() <= 0.0 {
        return None;
    }
    if values.len() % 2 == 1 {
        let copy = values.clone();
        values.extend(copy);
    }
    Some((values, if phase.is_finite() { phase } else { 0.0 }))
}

/// The corners (left, top, right, bottom) of a path that is one axis-aligned rectangle.
fn axis_aligned_rect(path: &tiny_skia::Path) -> Option<[f32; 4]> {
    use tiny_skia::PathSegment;
    let mut pts: Vec<(f32, f32)> = Vec::with_capacity(5);
    for seg in path.segments() {
        match seg {
            PathSegment::MoveTo(p) => {
                if !pts.is_empty() {
                    return None;
                }
                pts.push((p.x, p.y));
            }
            PathSegment::LineTo(p) => pts.push((p.x, p.y)),
            PathSegment::Close => {}
            _ => return None,
        }
        if pts.len() > 5 {
            return None;
        }
    }
    if pts.len() == 5 && pts.first() == pts.get(4) {
        pts.pop();
    }
    let [a, b, c, d] = pts[..] else { return None };
    let eps = 1e-3;
    let near = |x: f32, y: f32| (x - y).abs() <= eps;
    let horizontal_first = near(a.1, b.1) && near(b.0, c.0) && near(c.1, d.1) && near(d.0, a.0);
    let vertical_first = near(a.0, b.0) && near(b.1, c.1) && near(c.0, d.0) && near(d.1, a.1);
    if !(horizontal_first || vertical_first) {
        return None;
    }
    Some([a.0.min(c.0), a.1.min(c.1), a.0.max(c.0), a.1.max(c.1)])
}

/// About how long a path is (the control polygon of each curve, so a little more than the curve).
fn path_length(path: &tiny_skia::Path) -> f64 {
    use tiny_skia::PathSegment;
    let (mut start, mut at) = ((0.0f64, 0.0f64), (0.0f64, 0.0f64));
    let mut total = 0.0f64;
    let mut to = |at: &mut (f64, f64), x: f32, y: f32| {
        let (x, y) = (f64::from(x), f64::from(y));
        total += (x - at.0).hypot(y - at.1);
        *at = (x, y);
    };
    for seg in path.segments() {
        match seg {
            PathSegment::MoveTo(p) => {
                at = (f64::from(p.x), f64::from(p.y));
                start = at;
            }
            PathSegment::LineTo(p) => to(&mut at, p.x, p.y),
            PathSegment::QuadTo(c, p) => {
                to(&mut at, c.x, c.y);
                to(&mut at, p.x, p.y);
            }
            PathSegment::CubicTo(c1, c2, p) => {
                to(&mut at, c1.x, c1.y);
                to(&mut at, c2.x, c2.y);
                to(&mut at, p.x, p.y);
            }
            PathSegment::Close => to(&mut at, start.0 as f32, start.1 as f32),
        }
    }
    total
}
