//! Content stream interpreter, only as far as text needs it (ISO 32000-1 7.8,
//! 8.2 to 8.4, 9.2 to 9.4): graphics state (`q Q cm`), text state (`Tc Tw Tz TL
//! Tf Tr Ts`), text positioning and showing (`BT ET Td TD Tm T* Tj TJ ' "`),
//! form XObjects (`Do`) and inline images (skipped). Every shown character
//! becomes a [`Glyph`] with its position, writing direction and advance.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use crate::document::Document;
use crate::error::{Error, Result};
use crate::object::{ObjRef, Object};

use super::cmap::{CMap, Uni};
use super::font::{Font, Warnings};
use super::scan::{Item, Operand, Scanner};

/// Most operators one page may run, forms included (a form shown many times counts every time).
pub(crate) const MAX_OPERATORS: usize = 5_000_000;
/// Most characters one page may produce.
pub(crate) const MAX_GLYPHS: usize = 1_000_000;
/// Form XObjects nest at most this deep (7.8.3 sets no limit; ours).
pub(crate) const MAX_FORM_DEPTH: usize = 32;
/// Deepest `q` nesting kept; further ones are ignored.
const MAX_GSTATE_DEPTH: usize = 256;
/// Decoded form contents kept for reuse, in bytes.
const MAX_FORM_CACHE_BYTES: usize = 64 * 1024 * 1024;

/// A fast hasher for the small integer and short byte-string keys used here (no need for
/// the randomized one: the keys are not attacker-chosen in a way that matters, a bad
/// case only costs time inside the page's operator budget).
#[derive(Default, Clone, Copy)]
pub(crate) struct Fx(u64);

impl std::hash::Hasher for Fx {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(5) ^ u64::from(b)).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
        }
    }

    fn write_u32(&mut self, v: u32) {
        self.0 = (self.0.rotate_left(5) ^ u64::from(v)).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }

    fn write_u64(&mut self, v: u64) {
        self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }

    fn write_usize(&mut self, v: usize) {
        self.write_u64(v as u64);
    }
}

pub(crate) type FxMap<K, V> = HashMap<K, V, std::hash::BuildHasherDefault<Fx>>;

// Operators as numbers (their bytes and length), so that `operator` is a jump and not a
// chain of comparisons: it runs for every operator of every page.
const fn key1(a: u8) -> u32 {
    (a as u32) | (1 << 24)
}
const fn key2(a: u8, b: u8) -> u32 {
    (a as u32) | ((b as u32) << 8) | (2 << 24)
}
fn op_key(op: &[u8]) -> u32 {
    match op {
        [a] => key1(*a),
        [a, b] => key2(*a, *b),
        _ => 0,
    }
}
const OP_TJ: u32 = key2(b'T', b'j');
const OP_TJ_ARRAY: u32 = key2(b'T', b'J');
const OP_TD: u32 = key2(b'T', b'd');
const OP_TD_UPPER: u32 = key2(b'T', b'D');
const OP_Q_SAVE: u32 = key1(b'q');
const OP_Q_RESTORE: u32 = key1(b'Q');
const OP_CM: u32 = key2(b'c', b'm');
const OP_BT: u32 = key2(b'B', b'T');
const OP_TC: u32 = key2(b'T', b'c');
const OP_TW: u32 = key2(b'T', b'w');
const OP_TZ: u32 = key2(b'T', b'z');
const OP_TL: u32 = key2(b'T', b'L');
const OP_TS: u32 = key2(b'T', b's');
const OP_TF: u32 = key2(b'T', b'f');
const OP_TM: u32 = key2(b'T', b'm');
const OP_T_STAR: u32 = key2(b'T', b'*');
const OP_QUOTE: u32 = key1(39); // the single quote
const OP_DQUOTE: u32 = key1(34); // the double quote
const OP_DO: u32 = key2(b'D', b'o');


pub(crate) type Matrix = [f64; 6];
const IDENTITY: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// `a` then `b` (row vectors, 8.3.4): the matrix that does a's transformation first.
pub(crate) fn mul(a: &Matrix, b: &Matrix) -> Matrix {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
        a[4] * b[0] + a[5] * b[2] + b[4],
        a[4] * b[1] + a[5] * b[3] + b[5],
    ]
}

/// The length of the vector (x, y). (`f64::hypot` is exact but several times slower, and
/// this runs for every character.)
fn norm(x: f64, y: f64) -> f64 {
    (x * x + y * y).sqrt()
}

fn finite(m: &Matrix) -> bool {
    m.iter().all(|v| v.is_finite())
}

/// One shown character: where it starts, which way the line runs, how far it
/// advances (all in default user space) and how big it is.
#[derive(Clone, Debug)]
pub(crate) struct Glyph {
    pub uni: Uni,
    pub x: f64,
    pub y: f64,
    /// Unit vector of the writing direction (left to right for horizontal
    /// text, top to bottom for vertical text, after the text and page matrices).
    pub dx: f64,
    pub dy: f64,
    /// Advance along the direction, including character and word spacing.
    pub adv: f64,
    pub size: f64,
}

/// Resources of a page or form that text needs: the font and XObject entries by name
/// (a page may have hundreds of fonts, and every `Tf` looks one up), and the fonts
/// already loaded for them.
pub(crate) struct Scope {
    fonts: FxMap<Vec<u8>, Object>,
    xobjects: FxMap<Vec<u8>, Object>,
    loaded: RefCell<FxMap<Vec<u8>, Option<Rc<Font>>>>,
}

impl Scope {
    pub fn new(doc: &Document, resources: Option<&Object>) -> Scope {
        let res = resources.and_then(|r| doc.resolve(r).ok());
        let sub = |key: &str| -> FxMap<Vec<u8>, Object> {
            let dict = res
                .as_ref()
                .and_then(Object::as_dict)
                .and_then(|d| d.get(key))
                .and_then(|o| doc.resolve(o).ok())
                .and_then(|o| match o {
                    Object::Dict(d) => Some(d),
                    _ => None,
                })
                .unwrap_or_default();
            // `Dict` keeps the last of a repeated key (decisions.md), so the pairs are distinct.
            dict.into_pairs().into_iter().map(|(k, v)| (k.0, v)).collect()
        };
        Scope { fonts: sub("Font"), xobjects: sub("XObject"), loaded: RefCell::new(FxMap::default()) }
    }
}

pub(crate) struct Form {
    content: Vec<u8>,
    matrix: Matrix,
    /// `None`: the form has no `/Resources` and uses its caller's (7.8.3).
    scope: Option<Rc<Scope>>,
}

/// What lives as long as one text extraction: caches shared by all pages.
#[derive(Default)]
pub(crate) struct Shared {
    pub fonts: FxMap<ObjRef, Option<Rc<Font>>>,
    pub forms: FxMap<ObjRef, Rc<Form>>,
    pub form_bytes: usize,
    pub not_forms: HashSet<ObjRef, std::hash::BuildHasherDefault<Fx>>,
    pub cmaps: HashMap<String, Option<Arc<CMap>>>,
    pub warnings: Warnings,
}

#[derive(Clone)]
struct GState {
    ctm: Matrix,
    font: Option<Rc<Font>>,
    size: f64,
    tc: f64,
    tw: f64,
    th: f64,
    tl: f64,
    rise: f64,
}

/// What the matrices give for every character of a string: the linear part of the text
/// matrix times the CTM, the lengths of its columns, the two writing directions and
/// the glyph size. Kept from one string to the next while the matrices' linear parts
/// and the font size stay the same (a `Td` only moves the origin).
#[derive(Clone, Copy)]
struct Basis {
    key: [f64; 9],
    /// The c and d of the matrix (the y axis), for the rise.
    m2: f64,
    m3: f64,
    norm_ab: f64,
    norm_cd: f64,
    dir_h: (f64, f64),
    dir_v: (f64, f64),
    size_h: f64,
    size_v: f64,
}

impl Basis {
    fn new(tm: &Matrix, ctm: &Matrix, size: f64) -> Basis {
        let m0 = tm[0] * ctm[0] + tm[1] * ctm[2];
        let m1 = tm[0] * ctm[1] + tm[1] * ctm[3];
        let m2 = tm[2] * ctm[0] + tm[3] * ctm[2];
        let m3 = tm[2] * ctm[1] + tm[3] * ctm[3];
        let norm_ab = norm(m0, m1);
        let norm_cd = norm(m2, m3);
        Basis {
            key: [tm[0], tm[1], tm[2], tm[3], ctm[0], ctm[1], ctm[2], ctm[3], size],
            m2,
            m3,
            norm_ab,
            norm_cd,
            dir_h: unit(m0, m1, (1.0, 0.0)),
            dir_v: unit(-m2, -m3, (0.0, -1.0)),
            size_h: size.abs() * norm_cd,
            size_v: size.abs() * norm_ab,
        }
    }

    fn matches(&self, tm: &Matrix, ctm: &Matrix, size: f64) -> bool {
        self.key == [tm[0], tm[1], tm[2], tm[3], ctm[0], ctm[1], ctm[2], ctm[3], size]
    }
}

pub(crate) struct Interp<'a> {
    doc: &'a Document,
    shared: &'a mut Shared,
    pub glyphs: Vec<Glyph>,
    ops_left: usize,
    in_progress: Vec<ObjRef>,
    gs: GState,
    stack: Vec<GState>,
    tm: Matrix,
    tlm: Matrix,
    basis: Option<Basis>,
    /// Characters shown that have no Unicode value, per font name.
    pub unmapped: HashMap<String, usize>,
}

impl<'a> Interp<'a> {
    pub fn new(doc: &'a Document, shared: &'a mut Shared) -> Interp<'a> {
        Interp {
            doc,
            shared,
            glyphs: Vec::new(),
            ops_left: MAX_OPERATORS,
            in_progress: Vec::new(),
            gs: GState { ctm: IDENTITY, font: None, size: 0.0, tc: 0.0, tw: 0.0, th: 1.0, tl: 0.0, rise: 0.0 },
            stack: Vec::new(),
            tm: IDENTITY,
            tlm: IDENTITY,
            basis: None,
            unmapped: HashMap::new(),
        }
    }

    /// Run one content stream (a page's, or a form's with `depth` > 0).
    pub fn run(&mut self, content: &[u8], scope: &Rc<Scope>, depth: usize) -> Result<()> {
        let mut sc = Scanner::new(content);
        let mut ops: Vec<Operand> = Vec::new();
        loop {
            // The end, or a stream cut off in the middle of a string: what came before stands.
            let Item::Operator(op) = sc.next(&mut ops) else { return Ok(()) };
            if self.ops_left == 0 {
                return Err(Error::Limit(format!("more than {MAX_OPERATORS} content stream operators on one page")));
            }
            self.ops_left -= 1;
            if op == b"BI" {
                // 8.9.7: the image's dictionary entries, `ID`, binary data, `EI`: nothing for text.
                for _ in 0..4096 {
                    match sc.next(&mut ops) {
                        Item::Operator(b"ID") => {
                            sc.skip_inline_image_data();
                            break;
                        }
                        Item::Operator(_) => {}
                        Item::End => return Ok(()),
                    }
                }
                continue;
            }
            self.operator(op, &ops, &sc, scope, depth)?;
        }
    }

    fn operator(&mut self, op: &[u8], ops: &[Operand], sc: &Scanner<'_>, scope: &Rc<Scope>, depth: usize) -> Result<()> {
        match op_key(op) {
            OP_TJ => {
                if let Some(Operand::Str(s)) = ops.last() {
                    self.show(sc.bytes(*s))?;
                }
            }
            OP_TJ_ARRAY => {
                for item in ops {
                    match item {
                        Operand::Str(s) => self.show(sc.bytes(*s))?,
                        Operand::Num(n) => self.adjust(*n),
                        _ => {}
                    }
                }
            }
            OP_TD => {
                if let Some([tx, ty]) = numbers::<2>(ops) {
                    self.move_line(tx, ty);
                }
            }
            OP_Q_SAVE => {
                if self.stack.len() < MAX_GSTATE_DEPTH {
                    self.stack.push(self.gs.clone());
                }
            }
            OP_Q_RESTORE => {
                if let Some(g) = self.stack.pop() {
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
            OP_BT => {
                self.tm = IDENTITY;
                self.tlm = IDENTITY;
            }
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
            OP_TF => {
                if let [.., Operand::Name(name), Operand::Num(size)] = ops {
                    self.gs.size = *size;
                    self.gs.font = self.font(sc.bytes(*name), scope);
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
            OP_QUOTE => {
                self.move_line(0.0, -self.gs.tl);
                if let Some(Operand::Str(s)) = ops.last() {
                    self.show(sc.bytes(*s))?;
                }
            }
            OP_DQUOTE => {
                if let [.., Operand::Num(aw), Operand::Num(ac), Operand::Str(s)] = ops {
                    self.gs.tw = *aw;
                    self.gs.tc = *ac;
                    self.move_line(0.0, -self.gs.tl);
                    self.show(sc.bytes(*s))?;
                }
            }
            OP_DO => {
                if let Some(Operand::Name(name)) = ops.last() {
                    self.do_xobject(sc.bytes(*name), scope, depth)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// `Td`: start the next line, offset from the start of this one (9.4.2).
    fn move_line(&mut self, tx: f64, ty: f64) {
        let m = mul(&[1.0, 0.0, 0.0, 1.0, tx, ty], &self.tlm);
        if finite(&m) {
            self.tlm = m;
            self.tm = m;
        }
    }

    /// A number in a `TJ` array: move back by that thousandths of the font size (9.4.3).
    fn adjust(&mut self, n: f64) {
        let vertical = self.gs.font.as_ref().is_some_and(|f| f.vertical);
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

    // --- fonts -------------------------------------------------------------------------------

    fn font(&mut self, name: &[u8], scope: &Rc<Scope>) -> Option<Rc<Font>> {
        if let Some(hit) = scope.loaded.borrow().get(name) {
            return hit.clone();
        }
        let loaded = self.load_font(name, scope);
        scope.loaded.borrow_mut().insert(name.to_vec(), loaded.clone());
        loaded
    }

    fn load_font(&mut self, name: &[u8], scope: &Rc<Scope>) -> Option<Rc<Font>> {
        let Some(entry) = scope.fonts.get(name) else {
            self.shared.warnings.add(format!("font /{} is not in the resources", String::from_utf8_lossy(name)));
            return None;
        };
        if let Object::Ref(r) = entry {
            if let Some(hit) = self.shared.fonts.get(r) {
                return hit.clone();
            }
            let loaded = match self.doc.get(*r) {
                Ok(Object::Dict(d)) => Some(Rc::new(Font::load(self.doc, &d, &mut self.shared.cmaps, &mut self.shared.warnings))),
                Ok(_) => None,
                Err(e) => {
                    self.shared.warnings.add(format!("font object {} {} R could not be read: {e}", r.num, r.generation));
                    None
                }
            };
            self.shared.fonts.insert(*r, loaded.clone());
            return loaded;
        }
        match entry {
            Object::Dict(d) => Some(Rc::new(Font::load(self.doc, d, &mut self.shared.cmaps, &mut self.shared.warnings))),
            _ => None,
        }
    }

    // --- showing text ------------------------------------------------------------------------

    fn show(&mut self, s: &[u8]) -> Result<()> {
        let Some(font) = self.gs.font.clone() else { return Ok(()) };
        let (size, tc, tw, th, rise) = (self.gs.size, self.gs.tc, self.gs.tw, self.gs.th, self.gs.rise);
        let ctm = self.gs.ctm;
        let mut tm = self.tm;
        let basis = match self.basis {
            Some(b) if b.matches(&tm, &ctm, size) => b,
            _ => {
                let b = Basis::new(&tm, &ctm, size);
                self.basis = Some(b);
                b
            }
        };
        let mut failed: Option<Error> = None;
        font.show(s, |ch| {
            if failed.is_some() {
                return;
            }
            // The text space origin of the glyph, through the text matrix and the CTM (8.3.4).
            let ox = tm[4] * ctm[0] + tm[5] * ctm[2] + ctm[4];
            let oy = tm[4] * ctm[1] + tm[5] * ctm[3] + ctm[5];
            let (x, y) = (rise * basis.m2 + ox, rise * basis.m3 + oy);
            let (glyph_dir, glyph_size, adv, step);
            match ch.vertical {
                None => {
                    // 9.4.4: tx = ((w0 * Tfs) + Tc + Tw) * Th
                    let tx = (ch.w0 * size + tc + if ch.is_space_code { tw } else { 0.0 }) * th;
                    glyph_dir = basis.dir_h;
                    glyph_size = basis.size_h;
                    adv = tx * basis.norm_ab;
                    step = (tx * tm[0], tx * tm[1]);
                }
                Some((w1, _vx, _vy)) => {
                    // ty = w1 * Tfs + Tc + Tw (the glyph moves down the column)
                    let ty = w1 * size + tc + if ch.is_space_code { tw } else { 0.0 };
                    glyph_dir = basis.dir_v;
                    glyph_size = basis.size_v;
                    adv = -ty * basis.norm_cd;
                    step = (ty * tm[2], ty * tm[3]);
                }
            }
            if ch.uni.is_none() && !ch.is_space_code {
                match self.unmapped.get_mut(&font.name) {
                    Some(n) => *n += 1,
                    None => {
                        self.unmapped.insert(font.name.clone(), 1);
                    }
                }
            }
            if self.glyphs.len() >= MAX_GLYPHS {
                failed = Some(Error::Limit(format!("more than {MAX_GLYPHS} characters on one page")));
                return;
            }
            let glyph = Glyph {
                uni: ch.uni,
                x: if x.is_finite() { x } else { 0.0 },
                y: if y.is_finite() { y } else { 0.0 },
                dx: glyph_dir.0,
                dy: glyph_dir.1,
                adv: if adv.is_finite() { adv } else { 0.0 },
                size: if glyph_size.is_finite() { glyph_size } else { 0.0 },
            };
            self.glyphs.push(glyph);
            tm[4] += step.0;
            tm[5] += step.1;
        });
        if !finite(&tm) {
            tm = IDENTITY;
        }
        self.tm = tm;
        match failed {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    // --- form XObjects -----------------------------------------------------------------------

    fn do_xobject(&mut self, name: &[u8], scope: &Rc<Scope>, depth: usize) -> Result<()> {
        let Some(Object::Ref(r)) = scope.xobjects.get(name) else { return Ok(()) };
        let r = *r;
        if self.shared.not_forms.contains(&r) {
            return Ok(());
        }
        let form = match self.shared.forms.get(&r) {
            Some(f) => f.clone(),
            None => {
                let Some(form) = self.load_form(r)? else { return Ok(()) };
                let form = Rc::new(form);
                if self.shared.form_bytes.saturating_add(form.content.len()) <= MAX_FORM_CACHE_BYTES {
                    self.shared.form_bytes += form.content.len();
                    self.shared.forms.insert(r, form.clone());
                }
                form
            }
        };
        if depth + 1 > MAX_FORM_DEPTH {
            self.shared.warnings.add(format!("forms are nested more than {MAX_FORM_DEPTH} deep; the deeper ones are skipped"));
            return Ok(());
        }
        if self.in_progress.contains(&r) {
            self.shared.warnings.add(format!("form {} {} R contains itself; the repeat is skipped", r.num, r.generation));
            return Ok(());
        }
        let saved_gs = self.gs.clone();
        let saved_depth = self.stack.len();
        let (saved_tm, saved_tlm) = (self.tm, self.tlm);
        let m = mul(&form.matrix, &self.gs.ctm);
        if finite(&m) {
            self.gs.ctm = m;
        }
        self.in_progress.push(r);
        let inner = form.scope.clone().unwrap_or_else(|| scope.clone());
        let result = self.run(&form.content, &inner, depth + 1);
        self.in_progress.pop();
        self.stack.truncate(saved_depth);
        self.gs = saved_gs;
        self.tm = saved_tm;
        self.tlm = saved_tlm;
        result
    }

    /// Read a form XObject; `None` (and remembered) when it is not a form or cannot be read.
    fn load_form(&mut self, r: ObjRef) -> Result<Option<Form>> {
        let obj = match self.doc.get(r) {
            Ok(o) => o,
            Err(Error::Limit(m)) => return Err(Error::Limit(m)),
            Err(e) => {
                self.shared.warnings.add(format!("object {} {} R could not be read: {e}", r.num, r.generation));
                self.shared.not_forms.insert(r);
                return Ok(None);
            }
        };
        let Object::Stream(s) = obj else {
            self.shared.not_forms.insert(r);
            return Ok(None);
        };
        let is_form = match s.dict.get_name("Subtype") {
            Some(n) => n == "Form",
            None => s.dict.contains_key("BBox"),
        };
        if !is_form {
            self.shared.not_forms.insert(r);
            return Ok(None);
        }
        let content = match self.doc.decode_stream(&s) {
            Ok(c) => c,
            Err(Error::Limit(m)) => return Err(Error::Limit(m)),
            Err(e) => {
                self.shared.warnings.add(format!("form {} {} R is skipped: {e}", r.num, r.generation));
                self.shared.not_forms.insert(r);
                return Ok(None);
            }
        };
        let matrix = match s.dict.get("Matrix").and_then(Object::as_array) {
            Some(a) if a.len() == 6 => {
                let mut m = IDENTITY;
                for (slot, v) in m.iter_mut().zip(a) {
                    *slot = v.as_f64().unwrap_or(*slot);
                }
                if finite(&m) { m } else { IDENTITY }
            }
            _ => IDENTITY,
        };
        let scope = s.dict.get("Resources").map(|res| Rc::new(Scope::new(self.doc, Some(res))));
        Ok(Some(Form { content, matrix, scope }))
    }
}

/// The last `N` operands as numbers.
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

/// A unit vector in the direction (x, y); `fallback` for a zero vector.
fn unit(x: f64, y: f64, fallback: (f64, f64)) -> (f64, f64) {
    let len = norm(x, y);
    if len > 0.0 && len.is_finite() { (x / len, y / len) } else { fallback }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_order() {
        // scale by 2, then translate by (10, 0)
        let m = mul(&[2.0, 0.0, 0.0, 2.0, 0.0, 0.0], &[1.0, 0.0, 0.0, 1.0, 10.0, 0.0]);
        assert_eq!(m, [2.0, 0.0, 0.0, 2.0, 10.0, 0.0]);
    }
}
