//! Which glyph a character of a font is (ISO 32000-1 9.6.6 for simple fonts, 9.7.4 for composite
//! fonts), and its outline: from the font program the PDF carries, else from a system font.
//!
//! A [`GlyphSource`] is made when the font is met in the page and does no work until a glyph of the font
//! is drawn; then the program is read (and the system font opened) once. A program that cannot be read
//! is a warning and the system font takes over; without one the characters are boxes. The caches of
//! glyphs and programs are counted in bytes against budgets shared by all the fonts of a renderer; a page
//! also has a budget of outline work and of program bytes read, which it gets afresh at its start.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::{Arc, OnceLock};
use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};

use crate::document::Document;
use crate::object::{Dict, ObjRef, Object};
use crate::text::data::{self, Ordering};
use crate::text::font::{Font, Warnings};
use crate::text::fontprog;
use crate::text::interp::FxMap;

use super::cff::CffFont;
use super::glyf::{Cmap, TtFont};
use super::outline::{Res, path_bytes};
use super::sysfont;
use super::type1::T1Font;

/// The largest font program taken from a PDF, decoded.
const MAX_PROGRAM: usize = 32 << 20;
/// Bytes of font programs kept for one renderer, and of glyph outlines.
pub(crate) const MAX_PROGRAM_BYTES: usize = 64 << 20;
pub(crate) const MAX_GLYPH_BYTES: usize = 16 << 20;
/// Glyph outlines kept for one font before its cache starts again.
const MAX_FONT_GLYPH_BYTES: usize = 4 << 20;
/// Work (charstring operators and bytes of subroutines, points, segments) the outlines of one page may cost in all, whether
/// the glyphs come out or not: ordinary text spends a few hundred to a few thousand on each distinct glyph, a hostile
/// font up to 200,000 on one.
pub(crate) const MAX_PAGE_OUTLINE_WORK: usize = 20_000_000;
/// Bytes of font programs one page may have read (decoded and parsed) in all, loading the same program again included.
const MAX_PAGE_PROGRAM_READ: usize = 128 << 20;

static SERIAL: AtomicU32 = AtomicU32::new(1);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Cause {
    /// No font program and no system font to stand in.
    NoFont,
    /// The encoding does not say what character the code is.
    NoCharacter,
    /// The font has no glyph for the character.
    NotInFont,
}

pub(crate) struct Glyph {
    /// In em units, y up.
    pub path: tiny_skia::Path,
    /// The font's own advance in em units, 0 when not known.
    pub advance: f32,
    /// Names the glyph for the bitmap cache.
    pub id: u64,
}

#[derive(Clone)]
pub(crate) enum Lookup {
    Draw(Rc<Glyph>),
    /// The glyph has no outline (a space); the font's own advance in em units, 0 when not known.
    Blank(f32),
    /// The font has no glyph for the character (its .notdef is not drawn): nothing is drawn, and it is counted.
    Absent,
    /// The page has used up the work it may spend on outlines: the glyph is not made, nothing is drawn.
    Skipped,
    /// The character is drawn as a box.
    Box(Cause),
}

/// What the font reader says about a shown character.
pub(crate) struct CharInfo {
    pub code: u32,
    pub cid: u32,
    pub glyph_uni: u32,
}

/// Shared counters of the bytes the caches hold, what the page being drawn may still spend, and the programs alive.
#[derive(Clone, Default)]
pub(crate) struct Budget {
    pub programs: Rc<Cell<usize>>,
    pub glyphs: Rc<Cell<usize>>,
    /// Outline work and bytes of programs the page may still use (set by [`Budget::start_page`]).
    work: Rc<Cell<usize>>,
    read: Rc<Cell<usize>>,
    /// The programs alive, by the stream they were read from: fonts that share a FontFile share its parse.
    held: Rc<RefCell<FxMap<ObjRef, Weak<Held>>>>,
}

impl Budget {
    pub fn start_page(&self) {
        self.work.set(MAX_PAGE_OUTLINE_WORK);
        self.read.set(MAX_PAGE_PROGRAM_READ);
    }

    /// The program of a font file entry, if some font has it open.
    fn held_program(&self, entry: &Object) -> Option<Rc<Held>> {
        let Object::Ref(r) = entry else { return None };
        self.held.borrow().get(r).and_then(Weak::upgrade)
    }
}

pub(crate) struct LoadCtx<'a> {
    pub doc: &'a Document,
    pub warnings: &'a mut Warnings,
    pub budget: &'a Budget,
}

// --- the plan, made when the font is met --------------------------------------------------------------

struct SimplePlan {
    names: Vec<Option<Vec<u8>>>,
    table: &'static [u16; 256],
    named_base: bool,
    /// The Encoding, or its BaseEncoding, is MacRomanEncoding or WinAnsiEncoding.
    win_mac: bool,
}

struct Plan {
    name: String,
    raw_name: Vec<u8>,
    flags: i64,
    weight: f64,
    italic_angle: f64,
    ordering: Option<Ordering>,
    embedded: Option<Object>,
    cid_to_gid: Option<Object>,
    simple: Option<SimplePlan>,
}

enum State {
    Unloaded,
    Loaded(Rc<Loaded>),
    Failed,
}

pub(crate) struct GlyphSource {
    plan: Plan,
    state: RefCell<State>,
    /// A simple font without /Widths: the glyphs of a stand-in font set the spacing.
    pub no_widths: bool,
    /// A simple TrueType font with a GBK name and no program at all (no FontFile, FontFile2 or FontFile3): its strings are GBK text, a lead byte and a trail byte
    /// make one character (as PDFium reads them).
    pub gbk_pairs: bool,
}

fn resolve(doc: &Document, o: Option<&Object>) -> Option<Object> {
    match o? {
        Object::Null => None,
        o => doc.resolve(o).ok().filter(|o| !matches!(o, Object::Null)),
    }
}

fn dict_of(doc: &Document, d: &Dict, key: &str) -> Option<Dict> {
    match resolve(doc, d.get(key))? {
        Object::Dict(d) => Some(d),
        Object::Stream(s) => Some(s.dict),
        _ => None,
    }
}

impl GlyphSource {
    pub fn new(doc: &Document, dict: &Dict, font: &Font) -> GlyphSource {
        let composite = dict.get_name("Subtype").is_some_and(|n| n.as_bytes() == b"Type0");
        let subtype_is_truetype = dict.get_name("Subtype").is_some_and(|n| n.as_bytes() == b"TrueType");
        let raw_full_name = dict.get_name("BaseFont").map(|n| n.as_bytes().to_vec()).unwrap_or_default();
        let raw_name = dict.get_name("BaseFont").map(|n| n.as_bytes().to_vec()).unwrap_or_default();
        // The subset tag ("ABCDEF+") says nothing about the face.
        let raw_name = match raw_name.iter().position(|&b| b == b'+') {
            Some(6) if raw_name.iter().take(6).all(u8::is_ascii_uppercase) => raw_name.get(7..).unwrap_or_default().to_vec(),
            _ => raw_name,
        };
        let (font_dict, cid_font) = if composite {
            let desc = match resolve(doc, dict.get("DescendantFonts")) {
                Some(Object::Array(a)) => a.first().and_then(|o| resolve(doc, Some(o))),
                Some(o @ Object::Dict(_)) => Some(o),
                _ => None,
            };
            let d = desc.and_then(|o| o.as_dict().cloned()).unwrap_or_default();
            (d.clone(), Some(d))
        } else {
            (dict.clone(), None)
        };
        let descriptor = dict_of(doc, &font_dict, "FontDescriptor").unwrap_or_default();
        let flags = descriptor.get_int("Flags").unwrap_or(0);
        let num = |key: &str| descriptor.get(key).and_then(|o| resolve(doc, Some(o))).and_then(|o| o.as_f64()).unwrap_or(0.0);
        let embedded = ["FontFile", "FontFile2", "FontFile3"].iter().find_map(|k| descriptor.get(k).cloned());
        let embedded_is_none = embedded.is_none();
        let raw_name_copy = raw_full_name;
        let cid_to_gid = cid_font.as_ref().and_then(|d| d.get("CIDToGIDMap").cloned());
        let simple = font.simple_info().map(|i| SimplePlan { names: i.names.to_vec(), table: i.table, named_base: i.named_base, win_mac: i.win_mac });
        GlyphSource {
            plan: Plan {
                name: font.name.clone(),
                raw_name,
                flags,
                weight: num("FontWeight"),
                italic_angle: num("ItalicAngle"),
                ordering: font.ordering(),
                embedded,
                cid_to_gid,
                simple,
            },
            state: RefCell::new(State::Unloaded),
            no_widths: font.simple_info().is_some_and(|i| !i.has_widths),
            gbk_pairs: !composite && subtype_is_truetype && embedded_is_none && sysfont::is_gbk_font_name(&raw_name_copy),
        }
    }

    /// The glyph reader of the font, made now if it was not (once; a font that cannot be made stays so).
    pub fn loaded(&self, ctx: &mut LoadCtx<'_>) -> Result<Rc<Loaded>, Cause> {
        match &*self.state.borrow() {
            State::Loaded(l) => return Ok(l.clone()),
            State::Failed => return Err(Cause::NoFont),
            State::Unloaded => {}
        }
        // The page has read as many bytes of programs as it may (a few big fonts used in turn make the reader start again
        // and again): the font is not made, and is tried again on the next page.
        if let Some(entry) = &self.plan.embedded
            && ctx.budget.read.get() == 0
            && ctx.budget.held_program(entry).is_none()
        {
            ctx.warnings.add(format!("font {}: the page has read more font program data than a page may; its characters are drawn as boxes", self.plan.name));
            return Err(Cause::NoFont);
        }
        match Loaded::build(&self.plan, ctx) {
            Ok(l) => {
                let l = Rc::new(l);
                *self.state.borrow_mut() = State::Loaded(l.clone());
                Ok(l)
            }
            Err(cause) => {
                *self.state.borrow_mut() = State::Failed;
                Err(cause)
            }
        }
    }

    pub fn is_loaded(&self) -> bool {
        matches!(&*self.state.borrow(), State::Loaded(_))
    }

    /// Give back the memory of the reader (the budget is full); the next glyph reads the program again.
    pub fn unload(&self) {
        *self.state.borrow_mut() = State::Unloaded;
    }

    pub fn trim_glyphs(&self) {
        if let State::Loaded(l) = &*self.state.borrow() {
            l.clear_glyphs();
        }
    }
}

// --- font programs -------------------------------------------------------------------------------------

enum Program {
    Tt(Arc<TtFont>),
    Cff(Box<CffFont>),
    T1(Box<T1Font>),
}

/// A font program in memory, counted in the budget for as long as it lives.
struct Held {
    program: Program,
    bytes: usize,
    budget: Budget,
}

impl Held {
    fn new(program: Program, bytes: usize, budget: &Budget) -> Held {
        budget.programs.set(budget.programs.get().saturating_add(bytes));
        Held { program, bytes, budget: budget.clone() }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        let b = &self.budget;
        b.programs.set(b.programs.get().saturating_sub(self.bytes));
    }
}

fn parse_program(data: Vec<u8>) -> Res<Program> {
    if fontprog::is_sfnt(&data) {
        let data = Arc::new(data);
        if let Some((off, len)) = fontprog::sfnt_table_range(&data, 0, b"CFF ") {
            let cmap = fontprog::sfnt_table(&data, b"cmap").map(|c| Cmap::new(c.to_vec()));
            return Ok(Program::Cff(Box::new(CffFont::parse(data, off, len, cmap)?)));
        }
        return Ok(Program::Tt(Arc::new(TtFont::from_memory(data, 0)?)));
    }
    match data.first() {
        Some(1) if data.get(1) == Some(&0) => {
            let len = data.len();
            Ok(Program::Cff(Box::new(CffFont::parse(Arc::new(data), 0, len, None)?)))
        }
        Some(0x80) | Some(b'%') => Ok(Program::T1(Box::new(T1Font::parse(&data)?))),
        _ => Err("an unknown kind of font file"),
    }
}

// --- the reader --------------------------------------------------------------------------------------------

enum Mode {
    /// The bool: the glyph is chosen through the encoding (9.6.6.4), not straight from the cmap.
    Simple(SimplePlan, bool),
    Cid(Option<Vec<u8>>),
    /// A system font: simple fonts through the encoding's characters, composite ones through the collection's.
    System { symbol: bool },
}

pub(crate) struct Loaded {
    program: Rc<Held>,
    mode: Mode,
    /// The font is not the PDF's own.
    pub substituted: bool,
    name: String,
    serial: u32,
    cache: RefCell<FxMap<u64, Lookup>>,
    cache_bytes: Cell<usize>,
    error: Cell<Option<&'static str>>,
    /// The font is a "tricky" TrueType font: its glyphs are made with their instructions run (3b2).
    hinted: bool,
    /// Why the instructions of a glyph could not be run (said once for the font; its glyphs are drawn without them).
    hint_error: Cell<Option<&'static str>>,
    hint_warned: Cell<bool>,
    budget: Budget,
}

impl Drop for Loaded {
    fn drop(&mut self) {
        let b = &self.budget;
        b.glyphs.set(b.glyphs.get().saturating_sub(self.cache_bytes.get()));
    }
}

enum Found {
    Gid(u32),
    Blank,
    Absent,
    Missing(Cause),
}

/// A character that stands for white space and needs no glyph.
fn is_space(cp: u32) -> bool {
    matches!(cp, 0x20 | 0xA0 | 0x2000..=0x200F | 0x202F | 0x205F | 0x3000 | 0xFEFF | 0x09 | 0x0A | 0x0D)
}

impl Loaded {
    fn build(plan: &Plan, ctx: &mut LoadCtx<'_>) -> Result<Loaded, Cause> {
        if let Some(entry) = &plan.embedded {
            match Loaded::embedded(plan, entry, ctx) {
                Ok(l) => return Ok(l),
                Err(why) => ctx.warnings.add(format!("font {}: the font program was not read ({why}); a system font stands in for it", plan.name)),
            }
        }
        let request = sysfont::request_for(&plan.raw_name, plan.flags, plan.weight, plan.italic_angle, plan.ordering);
        let Some(tt) = sysfont::load(request) else {
            ctx.warnings.add(format!("font {}: no system font stands in for it; its characters are drawn as boxes", plan.name));
            return Err(Cause::NoFont);
        };
        let held = Rc::new(Held::new(Program::Tt(tt), 0, ctx.budget));
        Ok(Loaded::new(held, Mode::System { symbol: request.kind == sysfont::Kind::Symbol }, true, plan, ctx.budget))
    }

    fn new(program: Rc<Held>, mode: Mode, substituted: bool, plan: &Plan, budget: &Budget) -> Loaded {
        // The tables decide for a system font; a font of the PDF is also known by the name the PDF gives it.
        let hinted = match &program.program {
            Program::Tt(tt) => tt.is_tricky() || (!substituted && TtFont::is_tricky_name(&plan.raw_name)),
            _ => false,
        };
        Loaded {
            program,
            mode,
            substituted,
            name: plan.name.clone(),
            serial: SERIAL.fetch_add(1, AtomicOrdering::Relaxed),
            cache: RefCell::new(FxMap::default()),
            cache_bytes: Cell::new(0),
            error: Cell::new(None),
            hinted,
            hint_error: Cell::new(None),
            hint_warned: Cell::new(false),
            budget: budget.clone(),
        }
    }

    fn embedded(plan: &Plan, entry: &Object, ctx: &mut LoadCtx<'_>) -> Res<Loaded> {
        let held = match ctx.budget.held_program(entry) {
            Some(shared) => shared,
            None => {
                let Some(Object::Stream(s)) = resolve(ctx.doc, Some(entry)) else { return Err("the font file is not a stream") };
                let data = ctx.doc.decode_stream_limited(&s, MAX_PROGRAM).map_err(|_| "the font file cannot be decoded")?;
                let bytes = data.len();
                if ctx.budget.programs.get().saturating_add(bytes) > MAX_PROGRAM_BYTES {
                    return Err("too many font programs are open");
                }
                // Reading costs the page its bytes, a program that turns out to be broken too.
                ctx.budget.read.set(ctx.budget.read.get().saturating_sub(bytes));
                let held = Rc::new(Held::new(parse_program(data)?, bytes, ctx.budget));
                if let Object::Ref(r) = entry {
                    let mut map = ctx.budget.held.borrow_mut();
                    if map.len() >= 256 {
                        map.retain(|_, w| w.strong_count() > 0);
                    }
                    map.insert(*r, Rc::downgrade(&held));
                }
                held
            }
        };
        let mode = match &plan.simple {
            Some(s) => {
                // 9.6.6.4: through the encoding when it names MacRoman or WinAnsi (and has no /Differences, as PDFium reads
                // it) or the font is nonsymbolic; otherwise the cmap is read as it is.
                let by_encoding = plan.flags & 32 != 0 || (s.win_mac && s.names.iter().all(Option::is_none));
                Mode::Simple(SimplePlan { names: s.names.clone(), table: s.table, named_base: s.named_base, win_mac: s.win_mac }, by_encoding)
            }
            None => {
                let map = match resolve(ctx.doc, plan.cid_to_gid.as_ref()) {
                    Some(Object::Stream(m)) => ctx.doc.decode_stream_limited(&m, 1 << 18).ok(),
                    _ => None,
                };
                Mode::Cid(map)
            }
        };
        Ok(Loaded::new(held, mode, false, plan, ctx.budget))
    }

    /// The first thing that went wrong with a glyph since the last call.
    pub fn take_error(&self) -> Option<String> {
        self.error.take().map(|why| format!("font {}: a glyph could not be read ({why}); it is not drawn", self.name)).or_else(|| {
            self.hint_error.take().map(|why| format!("font {}: its glyph instructions could not be run ({why}); its glyphs are drawn without them", self.name))
        })
    }

    pub fn clear_glyphs(&self) {
        self.cache.borrow_mut().clear();
        let b = &self.budget;
        b.glyphs.set(b.glyphs.get().saturating_sub(self.cache_bytes.replace(0)));
    }

    pub fn lookup(&self, ch: &CharInfo) -> Lookup {
        let key = (u64::from(ch.cid) << 32) | u64::from(ch.glyph_uni);
        if let Some(hit) = self.cache.borrow().get(&key) {
            return hit.clone();
        }
        let value = match self.find(ch) {
            Found::Gid(gid) => {
                // Not cached: the next page has work to spend again.
                if self.budget.work.get() == 0 {
                    return Lookup::Skipped;
                }
                self.make(gid)
            }
            Found::Blank => Lookup::Blank(0.0),
            Found::Absent => Lookup::Absent,
            Found::Missing(cause) => Lookup::Box(cause),
        };
        if self.cache_bytes.get() > MAX_FONT_GLYPH_BYTES {
            self.clear_glyphs();
        }
        let size = match &value {
            Lookup::Draw(g) => path_bytes(&g.path) + 96,
            _ => 48,
        };
        self.cache_bytes.set(self.cache_bytes.get() + size);
        self.budget.glyphs.set(self.budget.glyphs.get().saturating_add(size));
        self.cache.borrow_mut().insert(key, value.clone());
        value
    }

    // --- which glyph ---

    fn find(&self, ch: &CharInfo) -> Found {
        match (&self.mode, &self.program.program) {
            (Mode::System { symbol }, Program::Tt(tt)) => self.find_system(tt, ch, *symbol),
            (Mode::System { .. }, _) => Found::Missing(Cause::NoFont),
            (Mode::Simple(sel, by_encoding), Program::Tt(tt)) => self.find_truetype(tt, sel, *by_encoding, ch),
            (Mode::Simple(sel, _), Program::Cff(cff)) => self.find_named(ch, sel, |n| cff.gid_of_name(n), |u| cff.gid_of_unicode(u), |c| cff.gid_of_code(c)),
            (Mode::Simple(sel, _), Program::T1(t1)) => self.find_named(ch, sel, |n| t1.gid_of_name(n), |u| t1.gid_of_unicode(u), |c| t1.gid_of_code(c)),
            (Mode::Cid(map), Program::Tt(_)) => {
                let gid = match map {
                    Some(m) => fontprog::u16_at(m, (ch.cid as usize).saturating_mul(2)).map(u32::from),
                    None => Some(ch.cid),
                };
                self.gid_or_blank(gid, ch)
            }
            (Mode::Cid(_), Program::Cff(cff)) => self.gid_or_blank(cff.gid_of_cid(ch.cid), ch),
            (Mode::Cid(_), Program::T1(_)) => Found::Missing(Cause::NotInFont),
        }
    }

    /// A glyph number of 0 is the font's .notdef: nothing is drawn for it.
    fn gid_or_blank(&self, gid: Option<u32>, ch: &CharInfo) -> Found {
        match gid {
            Some(g) if g != 0 => Found::Gid(g),
            _ if is_space(ch.glyph_uni) => Found::Blank,
            _ => Found::Absent,
        }
    }

    /// 9.6.6.4: the glyph of a character of a simple TrueType font, through the encoding's glyph names (`by_encoding`) or
    /// straight from the cmap.
    fn find_truetype(&self, tt: &TtFont, sel: &SimplePlan, by_encoding: bool, ch: &CharInfo) -> Found {
        let Some(cm) = tt.cmap.as_ref() else { return Found::Gid(ch.code) };
        let code = ch.code;
        let uni = ch.glyph_uni;
        let mut gid = None;
        if by_encoding {
            if uni != 0 {
                gid = cm.unicode(uni);
                if gid.is_none()
                    && let Some(mac) = data::MAC_ROMAN.iter().position(|&u| u32::from(u) == uni)
                {
                    gid = cm.mac(mac as u32);
                }
            }
            if gid.is_none()
                && let Some(Some(name)) = usize::try_from(code).ok().and_then(|c| sel.names.get(c))
            {
                gid = tt.glyph_by_name(&String::from_utf8_lossy(name));
            }
            gid = gid.or_else(|| cm.symbol(code)).or_else(|| cm.mac(code));
        } else {
            gid = cm.symbol(code).or_else(|| cm.mac(code));
            if gid.is_none() {
                gid = cm.unicode(code).or_else(|| (uni != 0).then(|| cm.unicode(uni)).flatten());
            }
            // A broken file: the cmap has nothing for the code but /Differences names a glyph the `post` table has.
            if gid.is_none()
                && let Some(Some(name)) = usize::try_from(code).ok().and_then(|c| sel.names.get(c))
            {
                gid = tt.glyph_by_name(&String::from_utf8_lossy(name));
            }
        }
        self.gid_or_blank(gid, ch)
    }

    /// Fonts whose glyphs have names (Type 1, CFF): 9.6.6.2.
    fn find_named(
        &self,
        ch: &CharInfo,
        sel: &SimplePlan,
        by_name: impl Fn(&str) -> Option<u32>,
        by_unicode: impl Fn(u32) -> Option<u32>,
        by_code: impl Fn(u8) -> Option<u32>,
    ) -> Found {
        let code = u8::try_from(ch.code).unwrap_or(0);
        let named = sel.names.get(usize::from(code)).and_then(Option::as_ref);
        let mut gid = None;
        if let Some(name) = named {
            let name = String::from_utf8_lossy(name);
            gid = by_name(&name).or_else(|| data::glyph_name_to_unicode(&name).first().and_then(|&u| by_unicode(u)));
        } else if sel.named_base {
            if ch.glyph_uni != 0 {
                gid = by_unicode(ch.glyph_uni);
            }
            gid = gid.or_else(|| by_code(code));
        } else {
            gid = by_code(code);
            if gid.is_none() && ch.glyph_uni != 0 {
                gid = by_unicode(ch.glyph_uni);
            }
        }
        self.gid_or_blank(gid, ch)
    }

    /// A system font: by the character the code stands for.
    fn find_system(&self, tt: &TtFont, ch: &CharInfo, symbol: bool) -> Found {
        let Some(cm) = tt.cmap.as_ref() else { return Found::Missing(Cause::NotInFont) };
        if symbol && let Some(g) = cm.symbol(ch.code) {
            return Found::Gid(g);
        }
        let uni = ch.glyph_uni;
        if uni == 0 {
            return Found::Missing(Cause::NoCharacter);
        }
        if let Some(g) = cm.unicode(uni) {
            return Found::Gid(g);
        }
        // Kangxi radicals and compatibility ideographs (what a collection's table gives for some CIDs) are the
        // unified ideograph the fonts have.
        let unified = data::normalize_cjk(uni);
        if unified != uni && let Some(g) = cm.unicode(unified) {
            return Found::Gid(g);
        }
        if is_space(uni) {
            return Found::Blank;
        }
        // A few characters stand in for others.
        let near = match uni {
            0x2010 | 0x2011 | 0x2212 | 0xAD => Some(0x2D),
            0x2018 | 0x2019 => Some(0x27),
            0x201C | 0x201D => Some(0x22),
            0x2022 => Some(0xB7),
            _ => None,
        };
        match near.and_then(|n| cm.unicode(n)) {
            Some(g) => Found::Gid(g),
            None => Found::Missing(Cause::NotInFont),
        }
    }

    // --- the outline ---

    fn make(&self, gid: u32) -> Lookup {
        // What the reader did is charged to the page whether a glyph came out of it or not.
        let (built, advance, spent) = match &self.program.program {
            Program::Tt(tt) => {
                let advance = if self.substituted { tt.advance(gid).unwrap_or(0.0) } else { 0.0 };
                let mut spent = 0;
                let mut hinted = None;
                if self.hinted {
                    // A tricky font: its instructions are run, once, at a large size; a font whose instructions fail is drawn without them.
                    let mut b = super::outline::Builder::new(tt.hinted_matrix());
                    let done = tt.outline_hinted(gid, &mut b);
                    spent = b.work();
                    match done {
                        Ok(()) => hinted = Some(b.finish()),
                        Err(why) => {
                            if !self.hint_warned.replace(true) {
                                self.hint_error.set(Some(why));
                            }
                        }
                    }
                }
                match hinted {
                    Some(path) => (Ok(path), advance, spent),
                    None => {
                        let mut b = super::outline::Builder::new(tt.em_matrix());
                        let done = tt.outline(gid, &mut b);
                        spent += b.work();
                        (done.map(|()| b.finish()), advance, spent)
                    }
                }
            }
            Program::Cff(cff) => {
                let mut b = cff.builder(gid);
                let done = cff.outline(gid, &mut b);
                let spent = b.work();
                (done.map(|()| b.finish()), 0.0, spent)
            }
            Program::T1(t1) => {
                let mut b = t1.builder();
                let done = t1.outline(gid, &mut b);
                let spent = b.work();
                (done.map(|()| b.finish()), 0.0, spent)
            }
        };
        self.budget.work.set(self.budget.work.get().saturating_sub(spent.max(1)));
        match built {
            Ok(Some(path)) => Lookup::Draw(Rc::new(Glyph { path, advance, id: (u64::from(self.serial) << 32) | u64::from(gid) })),
            Ok(None) => Lookup::Blank(advance),
            Err(why) => {
                if self.error.get().is_none() {
                    self.error.set(Some(why));
                }
                Lookup::Blank(0.0)
            }
        }
    }
}

/// The Unicode value of a GBK code (lead byte, trail byte), 0 if the code is none.
pub(crate) fn gbk_to_unicode(code: u32) -> u32 {
    static CMAP: OnceLock<Option<Arc<crate::text::cmap::CMap>>> = OnceLock::new();
    let cmap = CMAP.get_or_init(|| data::find_predefined("GBK-EUC-H"));
    let cid = cmap.as_ref().and_then(|c| c.cid(code, 2));
    cid.and_then(|c| Ordering::Gb1.cid_to_unicode(c)).map_or(0, data::normalize_cjk)
}
