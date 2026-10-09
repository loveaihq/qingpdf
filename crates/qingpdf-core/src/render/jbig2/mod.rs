//! `JBIG2Decode` (ISO 32000-1 7.4.7), after ITU-T T.88: the segments of an embedded JBIG2 stream (no file header,
//! the global segments of `/JBIG2Globals` first) drawn on one page bitmap.
//!
//! Segments: symbol dictionaries ([`symbol`]), text regions ([`text`]), pattern dictionaries and halftone regions
//! ([`halftone`]), generic and generic refinement regions ([`generic`]; MMR coded ones through [`super::ccitt`]),
//! tables of custom Huffman codes ([`huffman`]), page information, end of stripe. Arithmetic coding is
//! [`super::mq`].
//!
//! Everything in the file is an untrusted request. What bounds it: the size of any bitmap and of the bitmaps an
//! image holds together ([`Ctx::bitmap`]), the number of symbols, segments, table lines and referred-to segments,
//! and the page's work meter ([`Ctx::charge`]), which every row decoded, symbol made, integer read and pixel put on
//! a bitmap is charged to before it is done, whether or not it shows on the page. Nothing here recurses: a
//! dictionary, a region or an aggregate symbol only refers to what was decoded before it, by number.

mod arith;
mod bitmap;
mod generic;
mod halftone;
mod huffman;
mod symbol;
mod text;
#[cfg(test)]
mod test_enc;
#[cfg(test)]
mod test_enc_text;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_budget;
#[cfg(test)]
mod tests_hostile;
#[cfg(test)]
mod tests_text;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use bitmap::{Bitmap, Op};
use huffman::Table;

use super::mq::Mq;
use super::work::{Work, cost};
use crate::error::{Error, Result};
use crate::object::ObjRef;

/// A fault of this image (not of the page's budget).
fn bad(m: &str) -> Error {
    Error::Invalid(format!("JBIG2: {m}"))
}

/// Widest and tallest bitmap (also the widest MMR row).
const MAX_DIM: usize = 1 << 20;
/// Most pixels of one bitmap, the same as of any image.
const MAX_PIXELS: u64 = super::image::MAX_IMAGE_PIXELS;
/// Bytes of bitmaps one image may make, all together (the page, regions, symbols, patterns).
const MAX_MEMORY: u64 = 192 * 1024 * 1024;
/// Most segments in the global and the page stream together, and most segments one segment may refer to.
const MAX_SEGMENTS: usize = 1 << 16;
const MAX_REFS: usize = 1 << 12;
/// What a bitmap takes besides its pixels: the `Rc` box, the struct, the allocator's share.
const BITMAP_OVERHEAD: u64 = 100;
/// Bytes of decoded globals a page keeps for the images that name the same stream.
const MAX_GLOBALS_CACHE: u64 = 32 * 1024 * 1024;

/// What every part of the decoder charges and takes memory from.
struct Ctx<'a> {
    work: &'a Work,
    memory: Cell<u64>,
}

impl<'a> Ctx<'a> {
    fn new(work: &'a Work) -> Ctx<'a> {
        Ctx { work, memory: Cell::new(MAX_MEMORY) }
    }

    /// Spend `units` of the page's work before doing it. [`Error::Limit`]: the page has no more.
    pub fn charge(&self, units: f64) -> Result<()> {
        if self.work.charge(units) { Ok(()) } else { Err(Error::Limit("the page's work allowance is used up (JBIG2 image)".to_string())) }
    }

    /// Spend `units` on work that is done already (the decoder of a row knows how much of it was run and how much
    /// pixel by pixel only when it has finished). When it takes all that is left, the next charge says no.
    pub fn spend(&self, units: f64) {
        self.work.spend(units);
    }

    /// Memory still free (tests).
    #[cfg(test)]
    pub fn memory_left(&self) -> u64 {
        self.memory.get()
    }

    /// Give back what a bitmap took when it is dropped: the limit is on what an image holds at once, not on all it ever
    /// made (a text region makes a bitmap for each refined symbol it draws).
    pub fn release(&self, b: &Bitmap) {
        self.give_back(b.data.len() as u64 + BITMAP_OVERHEAD);
    }

    pub fn give_back(&self, bytes: u64) {
        self.memory.set(self.memory.get().saturating_add(bytes).min(MAX_MEMORY));
    }

    /// Take `bytes` of the memory an image may hold.
    pub fn take_memory(&self, bytes: u64) -> Result<()> {
        match self.memory.get().checked_sub(bytes) {
            Some(left) => {
                self.memory.set(left);
                Ok(())
            }
            None => Err(bad("the image holds more bitmaps than is allowed")),
        }
    }

    /// A new bitmap, all white (or black): its size is checked, its memory taken, clearing it charged.
    pub fn bitmap(&self, w: usize, h: usize, black: bool) -> Result<Bitmap> {
        if w > MAX_DIM || h > MAX_DIM || (w as u64) * (h as u64) > MAX_PIXELS {
            return Err(bad("a bitmap that is too large"));
        }
        let bytes = w.div_ceil(8) as u64 * h as u64 + BITMAP_OVERHEAD;
        self.take_memory(bytes)?;
        self.charge(bytes as f64 * cost::JB2_CLEAR_BYTE)?;
        Ok(Bitmap::new(w, h, black))
    }
}

/// A segment already decoded that later segments may refer to.
enum Stored {
    Symbols(Rc<symbol::SymbolDict>),
    Patterns(Rc<halftone::PatternDict>),
    Table(Rc<Table>),
    /// An intermediate region.
    Region(Rc<Bitmap>),
}

impl Stored {
    /// The memory of the image this segment holds (to give back when a segment of the same number replaces it).
    fn held(&self) -> u64 {
        match self {
            Stored::Symbols(d) => d.held,
            Stored::Patterns(p) => p.held(),
            Stored::Table(t) => t.held(),
            Stored::Region(b) => b.data.len() as u64 + BITMAP_OVERHEAD,
        }
    }
}

/// What a segment refers to: the symbols of the dictionaries (all of them, in order), the custom tables (in order),
/// and the last dictionary (whose retained contexts a new one may start from).
struct Referred {
    symbols: Vec<Rc<Bitmap>>,
    tables: Vec<Rc<Table>>,
    last: Option<Rc<symbol::SymbolDict>>,
    /// The memory the list of symbols takes until the segment is decoded.
    held: u64,
}

struct Header {
    number: u32,
    kind: u8,
    refs: Vec<u32>,
    /// Where the data starts, and how long it is (`None`: not told, an immediate generic region found by its end marker).
    start: usize,
    length: Option<usize>,
}

fn be32(data: &[u8], at: usize) -> Result<u32> {
    let b: [u8; 4] = data.get(at..at + 4).and_then(|s| s.try_into().ok()).ok_or_else(|| bad("a segment ends too soon"))?;
    Ok(u32::from_be_bytes(b))
}

/// A segment header (T.88 7.2).
fn parse_header(data: &[u8], pos: usize) -> Result<Header> {
    let number = be32(data, pos)?;
    let flags = *data.get(pos + 4).ok_or_else(|| bad("a segment header ends too soon"))?;
    let kind = flags & 0x3F;
    let first = *data.get(pos + 5).ok_or_else(|| bad("a segment header ends too soon"))?;
    let mut p = pos + 5;
    let count = if first >> 5 == 7 {
        let n = (be32(data, p)? & 0x1FFF_FFFF) as usize;
        p += 4 + (n + 1).div_ceil(8);
        n
    } else if first >> 5 <= 4 {
        p += 1;
        usize::from(first >> 5)
    } else {
        return Err(bad("a segment that refers to 5 or 6 segments by the short form"));
    };
    if count > MAX_REFS {
        return Err(bad("a segment refers to too many segments"));
    }
    let width = if number <= 256 { 1 } else if number <= 65536 { 2 } else { 4 };
    let mut refs = Vec::with_capacity(count);
    for _ in 0..count {
        let bytes = data.get(p..p + width).ok_or_else(|| bad("a segment header ends too soon"))?;
        refs.push(bytes.iter().fold(0u32, |a, &b| (a << 8) | u32::from(b)));
        p += width;
    }
    p += if flags & 0x40 != 0 { 4 } else { 1 };
    let length = be32(data, p)?;
    p += 4;
    let length = if length == u32::MAX { None } else { Some(length as usize) };
    Ok(Header { number, kind, refs, start: p, length })
}

/// The length of an immediate generic region whose header does not say (7.2.7): up to its end marker and the row
/// count after it. The region's flags tell whether the data is MMR coded.
fn unknown_length(data: &[u8], start: usize) -> Result<usize> {
    let flags = *data.get(start + 17).ok_or_else(|| bad("a segment ends too soon"))?;
    let mmr = flags & 1 != 0;
    let marker: [u8; 2] = if mmr { [0x00, 0x00] } else { [0xFF, 0xAC] };
    // The data starts after the flags and the adaptive pixels.
    let head = 18 + if mmr { 0 } else if (flags >> 1) & 3 == 0 { 8 } else { 2 };
    let body = data.get(start + head..).unwrap_or(&[]);
    let at = body.windows(2).position(|w| w == marker).ok_or_else(|| bad("a generic region without its end marker"))?;
    Ok(head + at + 2 + 4)
}

/// The region segment information field (7.4.1).
struct Region {
    w: usize,
    h: usize,
    x: i64,
    y: i64,
    op: Op,
}

fn region_info(data: &[u8]) -> Result<Region> {
    let (w, h, x, y) = (be32(data, 0)? as usize, be32(data, 4)? as usize, be32(data, 8)?, be32(data, 12)?);
    let op = Op::from_code(u32::from(data.get(16).copied().ok_or_else(|| bad("a region ends too soon"))? & 7)).ok_or_else(|| bad("a region with an operator that does not exist"))?;
    if w > MAX_DIM || h > MAX_DIM || (w as u64) * (h as u64) > MAX_PIXELS {
        return Err(bad("a region that is too large"));
    }
    Ok(Region { w, h, x: i64::from(x as i32), y: i64::from(y as i32), op })
}

/// Pairs of signed bytes (adaptive template pixels).
fn at_pairs<const N: usize>(data: &[u8], pos: usize) -> Result<[(i8, i8); N]> {
    let mut out = [(0i8, 0i8); N];
    for (i, o) in out.iter_mut().enumerate() {
        let (a, b) = (data.get(pos + 2 * i), data.get(pos + 2 * i + 1));
        let (Some(&a), Some(&b)) = (a, b) else { return Err(bad("adaptive pixels cut off")) };
        *o = (a as i8, b as i8);
    }
    Ok(out)
}

struct PageState {
    bitmap: Bitmap,
    default_black: bool,
    unknown_height: bool,
}

/// The segments of a `/JBIG2Globals` stream, decoded: the images of a page that name the same stream share them
/// (they would otherwise decode it again each, and a page of many crops of one scan would use its work up on that).
pub(crate) struct Globals {
    segments: HashMap<u32, Stored>,
    /// Segments, to count them against the limit of the image.
    count: usize,
    warnings: Vec<String>,
    /// The memory of the image the segments hold: an image that uses them starts with that much taken.
    held: u64,
}

/// The globals a page has decoded, by the object that is their stream. Bounded by the bytes they hold.
#[derive(Default)]
pub(crate) struct GlobalsCache {
    entries: RefCell<HashMap<ObjRef, Rc<Globals>>>,
    bytes: Cell<u64>,
}

impl GlobalsCache {
    fn get(&self, key: ObjRef) -> Option<Rc<Globals>> {
        self.entries.borrow().get(&key).cloned()
    }

    /// Keep `g` unless it is more than the cache may hold; when it does not fit beside the others, they go.
    fn put(&self, key: ObjRef, g: Rc<Globals>) {
        if g.held > MAX_GLOBALS_CACHE {
            return;
        }
        let mut entries = self.entries.borrow_mut();
        if self.bytes.get() + g.held > MAX_GLOBALS_CACHE {
            entries.clear();
            self.bytes.set(0);
        }
        self.bytes.set(self.bytes.get() + g.held);
        entries.insert(key, g);
    }
}

/// Where the globals of an image come from.
pub(crate) struct GlobalsSource<'a> {
    /// The stream's object when it has one (it is the key of the cache); a stream with none is decoded for this image
    /// alone.
    pub key: Option<ObjRef>,
    pub cache: &'a GlobalsCache,
    /// Reads the stream (its own filters undone); not called when the cache has the globals.
    pub load: &'a dyn Fn() -> Option<Vec<u8>>,
}

struct Decoder<'a> {
    ctx: &'a Ctx<'a>,
    segments: HashMap<u32, Stored>,
    /// The decoded globals, if the image has any that were kept; their segments are looked up after its own.
    globals: Option<Rc<Globals>>,
    page: Option<PageState>,
    /// The size the PDF says the image has: a page that is not told is made that size.
    want: (usize, usize),
    stripe_end: Option<u64>,
    count: usize,
    warnings: Vec<String>,
    done: bool,
}

impl Decoder<'_> {
    fn find(&self, number: u32) -> Option<&Stored> {
        self.segments.get(&number).or_else(|| self.globals.as_ref().and_then(|g| g.segments.get(&number)))
    }

    /// Keep a segment. One of the same number that was kept before goes, and what it held is free again.
    fn store(&mut self, number: u32, s: Stored) {
        if let Some(old) = self.segments.insert(number, s) {
            self.ctx.give_back(old.held());
        }
    }

    /// Start from the globals: the kept ones, or the stream decoded now (and kept, when it can be).
    fn use_globals(&mut self, source: &GlobalsSource<'_>) -> Result<()> {
        if let Some(g) = source.key.and_then(|k| source.cache.get(k)) {
            self.ctx.take_memory(g.held)?;
            self.count = g.count;
            self.warnings.extend(g.warnings.iter().cloned());
            self.globals = Some(g);
            return Ok(());
        }
        let Some(bytes) = (source.load)() else { return Ok(()) };
        self.stream(&bytes)?;
        // End-of-page in the globals would be odd; the page stream is read anyway.
        self.done = false;
        // Globals that drew on the page are the page's, not something to share.
        if let Some(key) = source.key
            && self.page.is_none()
            && self.stripe_end.is_none()
        {
            let held = MAX_MEMORY.saturating_sub(self.ctx.memory.get());
            let g = Rc::new(Globals { segments: std::mem::take(&mut self.segments), count: self.count, warnings: self.warnings.clone(), held });
            source.cache.put(key, g.clone());
            self.globals = Some(g);
        }
        Ok(())
    }

    /// Decode the segments of one stream. A segment with a fault ends the stream (what the earlier ones drew stays);
    /// a fault of the page's budget ends the image.
    fn stream(&mut self, data: &[u8]) -> Result<()> {
        let mut pos = 0usize;
        while pos < data.len() && !self.done {
            self.count += 1;
            if self.count > MAX_SEGMENTS {
                self.warnings.push("too many segments".to_string());
                return Ok(());
            }
            self.ctx.charge(cost::JB2_SEGMENT)?;
            let step = parse_header(data, pos).and_then(|h| {
                let length = match h.length {
                    Some(n) => n,
                    // Only an immediate generic region may leave its length out (7.2.7).
                    None if matches!(h.kind, 36 | 38 | 39) => unknown_length(data, h.start)?,
                    None => return Err(bad("a segment without a length that is not a generic region")),
                };
                let body = data.get(h.start..h.start.saturating_add(length)).ok_or_else(|| bad("a segment is longer than the data"))?;
                Ok((h, body))
            });
            let (h, body) = match step {
                Ok(x) => x,
                Err(e @ Error::Limit(_)) => return Err(e),
                Err(e) => {
                    self.warnings.push(e.to_string());
                    return Ok(());
                }
            };
            pos = h.start + body.len();
            match self.segment(&h, body) {
                Ok(()) => {}
                Err(e @ Error::Limit(_)) => return Err(e),
                Err(e) => {
                    self.warnings.push(e.to_string());
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    fn page_mut(&mut self) -> Result<&mut PageState> {
        if self.page.is_none() {
            // No page information: the page is as big as the image says.
            let (w, h) = self.want;
            self.page = Some(PageState { bitmap: self.ctx.bitmap(w, h, false)?, default_black: false, unknown_height: false });
        }
        self.page.as_mut().ok_or_else(|| bad("no page"))
    }

    fn segment(&mut self, h: &Header, data: &[u8]) -> Result<()> {
        // Looking up what the segment refers to is work of its own (a header can name thousands).
        self.ctx.charge(h.refs.len() as f64 * 8.0)?;
        match h.kind {
            0 => {
                let r = self.referred(h)?;
                let held = r.held;
                let dict = symbol::decode_dictionary(self.ctx, data, r.symbols, &r.tables, r.last.as_deref())?;
                self.ctx.give_back(held);
                self.store(h.number, Stored::Symbols(Rc::new(dict)));
            }
            4 | 6 | 7 => {
                let r = self.referred(h)?;
                let held = r.held;
                let info = region_info(data)?;
                let bitmap = text::decode_region(self.ctx, data, &info, r.symbols, &r.tables)?;
                self.ctx.give_back(held);
                self.region_done(h, &info, bitmap)?;
            }
            16 => {
                let dict = halftone::decode_patterns(self.ctx, data)?;
                self.store(h.number, Stored::Patterns(Rc::new(dict)));
            }
            20 | 22 | 23 => {
                let info = region_info(data)?;
                let dict = h.refs.iter().find_map(|r| match self.find(*r) {
                    Some(Stored::Patterns(p)) => Some(p.clone()),
                    _ => None,
                });
                let dict = dict.ok_or_else(|| bad("a halftone region without a pattern dictionary"))?;
                let bitmap = halftone::decode_region(self.ctx, data, &info, &dict)?;
                self.region_done(h, &info, bitmap)?;
            }
            36 | 38 | 39 => {
                let info = region_info(data)?;
                let bitmap = self.generic_region(data, &info, h.length.is_none())?;
                self.region_done(h, &info, bitmap)?;
            }
            40 | 42 | 43 => {
                let info = region_info(data)?;
                let bitmap = self.refinement_region(h, data, &info)?;
                self.region_done(h, &info, bitmap)?;
            }
            48 => self.page_info(data)?,
            49 | 51 => self.done = true,
            50 => {
                let y = be32(data, 0)?;
                self.stripe_end = Some(self.stripe_end.map_or(u64::from(y), |s| s.max(u64::from(y))));
            }
            53 => {
                let table = Table::parse_charged(self.ctx, data)?;
                self.store(h.number, Stored::Table(Rc::new(table)));
            }
            // Profiles and extensions say things to people, not to the decoder.
            52 | 62 => {}
            k => {
                let w = format!("a segment of type {k} is skipped");
                if !self.warnings.contains(&w) {
                    self.warnings.push(w);
                }
            }
        }
        Ok(())
    }

    /// What a segment refers to.
    fn referred(&self, h: &Header) -> Result<Referred> {
        let mut symbols: Vec<Rc<Bitmap>> = Vec::new();
        let mut tables = Vec::new();
        let mut last = None;
        // The list of symbols is memory of the image until the segment is decoded.
        let mut held = 0u64;
        for r in &h.refs {
            match self.find(*r) {
                Some(Stored::Symbols(d)) => {
                    if symbols.len() + d.exported.len() > symbol::MAX_SYMBOLS {
                        return Err(bad("the dictionaries a segment refers to have too many symbols"));
                    }
                    let bytes = d.exported.len() as u64 * symbol::PTR;
                    self.ctx.take_memory(bytes)?;
                    held += bytes;
                    symbols.extend(d.exported.iter().cloned());
                    last = Some(d.clone());
                }
                Some(Stored::Table(t)) => tables.push(t.clone()),
                _ => {}
            }
        }
        self.ctx.charge(symbols.len() as f64 * cost::JB2_RC)?;
        Ok(Referred { symbols, tables, last, held })
    }

    fn page_info(&mut self, data: &[u8]) -> Result<()> {
        let w = be32(data, 0)? as usize;
        let h = be32(data, 4)?;
        let flags = *data.get(16).ok_or_else(|| bad("page information ends too soon"))?;
        let unknown = h == u32::MAX;
        let h = if unknown { 0 } else { h as usize };
        let black = flags & 4 != 0;
        if w == 0 {
            return Err(bad("a page without width"));
        }
        // A new page replaces the old one: its bitmap is free before the new one is made.
        if let Some(old) = self.page.take() {
            self.ctx.release(&old.bitmap);
        }
        let bitmap = self.ctx.bitmap(w, h, black)?;
        self.page = Some(PageState { bitmap, default_black: black, unknown_height: unknown });
        Ok(())
    }

    /// A finished region: kept for later segments to refine (intermediate), or put on the page.
    fn region_done(&mut self, h: &Header, info: &Region, bitmap: Bitmap) -> Result<()> {
        if matches!(h.kind, 4 | 20 | 36 | 40) {
            // Kept as it is; its rows are charged as those of a region put on the page are.
            self.ctx.charge(bitmap.h as f64 * cost::JB2_ROW)?;
            self.store(h.number, Stored::Region(Rc::new(bitmap)));
            return Ok(());
        }
        let ctx = self.ctx;
        let page = self.page_mut()?;
        if page.unknown_height {
            // A striped page grows with its regions.
            let need = usize::try_from(info.y.saturating_add(bitmap.h as i64)).unwrap_or(0);
            if need > page.bitmap.h {
                if need > MAX_DIM || (page.bitmap.w as u64) * (need as u64) > MAX_PIXELS {
                    return Err(bad("a page that grows too large"));
                }
                let add = (need - page.bitmap.h) * page.bitmap.stride;
                ctx.take_memory(add as u64)?;
                ctx.charge(add as f64 * cost::JB2_CLEAR_BYTE)?;
                let black = page.default_black;
                page.bitmap.grow_to(need, black);
            }
        }
        ctx.charge(page.bitmap.overlap(bitmap.w, bitmap.h, info.x, info.y) as f64 * cost::JB2_BLIT_PIXEL + bitmap.h as f64 * cost::JB2_BLIT_ROW)?;
        page.bitmap.combine(&bitmap, info.x, info.y, info.op);
        ctx.release(&bitmap);
        Ok(())
    }

    /// A generic region segment (7.4.6).
    fn generic_region(&mut self, data: &[u8], info: &Region, unknown_length: bool) -> Result<Bitmap> {
        let flags = *data.get(17).ok_or_else(|| bad("a generic region ends too soon"))?;
        let mmr = flags & 1 != 0;
        let template = (flags >> 1) & 3;
        if flags & 0x10 != 0 {
            return Err(Error::Unsupported("JBIG2: a generic region with the extended template".to_string()));
        }
        let mut pos = 18;
        let mut at = [(0i8, 0i8); 4];
        if !mmr {
            if template == 0 {
                at = at_pairs::<4>(data, pos)?;
                pos += 8;
            } else {
                let one = at_pairs::<1>(data, pos)?;
                at[0] = one[0];
                pos += 2;
            }
        }
        let mut height = info.h;
        let mut end = data.len();
        if unknown_length {
            // The last four bytes are the number of rows there are.
            end = end.saturating_sub(4);
            height = height.min(be32(data, end)? as usize);
        }
        let body = data.get(pos..end).ok_or_else(|| bad("a generic region ends too soon"))?;
        if mmr {
            let (bitmap, _, complete) = generic::decode_mmr(self.ctx, body, info.w, height)?;
            if !complete {
                self.warnings.push("an MMR coded region is damaged".to_string());
            }
            return Ok(bitmap);
        }
        let mut mq = Mq::new(body);
        let mut cx = vec![0u8; generic::GENERIC_CONTEXTS];
        self.ctx.charge(cx.len() as f64 * cost::JB2_CLEAR_BYTE)?;
        let params = generic::GenericParams { template, tpgdon: flags & 8 != 0, at, skip: None };
        let (bitmap, complete) = generic::decode_generic(self.ctx, &mut mq, &mut cx, info.w, height, &params)?;
        if !complete {
            self.warnings.push("a generic region is cut short; the rest is white".to_string());
        }
        Ok(bitmap)
    }

    /// A generic refinement region segment (7.4.7).
    fn refinement_region(&mut self, h: &Header, data: &[u8], info: &Region) -> Result<Bitmap> {
        let flags = *data.get(17).ok_or_else(|| bad("a refinement region ends too soon"))?;
        let template = flags & 1;
        let mut pos = 18;
        let mut at = [(0i8, 0i8); 2];
        if template == 0 {
            at = at_pairs::<2>(data, pos)?;
            pos += 4;
        }
        let reference = h.refs.iter().find_map(|r| match self.find(*r) {
            Some(Stored::Region(b)) => Some(b.clone()),
            _ => None,
        });
        let from_page = reference.is_none();
        let reference = match reference {
            Some(b) => b,
            None => {
                // No region to refine: the part of the page under this one.
                let ctx = self.ctx;
                let mut part = ctx.bitmap(info.w, info.h, false)?;
                let page = self.page_mut()?;
                ctx.charge(page.bitmap.overlap(info.w, info.h, info.x, info.y) as f64 * cost::JB2_BLIT_PIXEL + info.h as f64 * cost::JB2_BLIT_ROW)?;
                part.combine(&page.bitmap, -info.x, -info.y, Op::Replace);
                Rc::new(part)
            }
        };
        let body = data.get(pos..).unwrap_or(&[]);
        let mut mq = Mq::new(body);
        let mut cx = vec![0u8; generic::REFINE_CONTEXTS];
        let params = generic::RefineParams { template, tpgron: flags & 2 != 0, at, reference: &reference, dx: 0, dy: 0 };
        let (bitmap, complete) = generic::decode_refinement(self.ctx, &mut mq, &mut cx, info.w, info.h, &params)?;
        if from_page {
            self.ctx.release(&reference);
        }
        if !complete {
            self.warnings.push("a refinement region is cut short".to_string());
        }
        Ok(bitmap)
    }
}

/// A decoded page, in the polarity of the PDF filter output: one bit per pixel, rows padded to a byte, 0 black.
pub(crate) struct Page {
    pub data: Vec<u8>,
    /// Pixels the JBIG2 page itself has (the image dictionary may say another number).
    pub page_pixels: u64,
    /// What went wrong, if anything did; the page is what could be decoded.
    pub warning: Option<String>,
}

/// Decode a JBIG2 stream (`globals`: where the stream of `/JBIG2Globals` comes from) into a page of `want` pixels (the
/// size the image dictionary says; the JBIG2 page is cut or padded with white to it). The work is charged to `work`.
///
/// `Err(Error::Limit)`: the page's work is used up. `Err(Error::Invalid)`: there is nothing to show (no segment
/// could be decoded into a page); the caller draws a placeholder.
pub(crate) fn decode_with_globals(data: &[u8], globals: Option<GlobalsSource<'_>>, want: (usize, usize), work: &Work) -> Result<Page> {
    let ctx = Ctx::new(work);
    run(&ctx, data, globals, want)
}

/// [`decode_with_globals`] with the globals as bytes (tests).
#[cfg(test)]
pub(crate) fn decode(data: &[u8], globals: Option<&[u8]>, want: (usize, usize), work: &Work) -> Result<Page> {
    let cache = GlobalsCache::default();
    let load = || globals.map(<[u8]>::to_vec);
    decode_with_globals(data, globals.map(|_| GlobalsSource { key: None, cache: &cache, load: &load }), want, work)
}

fn run<'a>(ctx: &'a Ctx<'a>, data: &[u8], globals: Option<GlobalsSource<'_>>, want: (usize, usize)) -> Result<Page> {
    let work = ctx.work;
    let mut d = Decoder { ctx, segments: HashMap::new(), globals: None, page: None, want, stripe_end: None, count: 0, warnings: Vec::new(), done: false };
    if let Some(source) = &globals {
        d.use_globals(source)?;
    }
    d.stream(data)?;
    let Some(mut page) = d.page.take() else {
        return Err(d.warnings.pop().map_or_else(|| bad("no page"), Error::Invalid));
    };
    if page.unknown_height
        && let Some(end) = d.stripe_end
    {
        let h = usize::try_from(end.saturating_add(1)).unwrap_or(0).min(page.bitmap.h);
        page.bitmap.h = h;
        page.bitmap.data.truncate(h * page.bitmap.stride);
    }
    let bitmap = page.bitmap;
    let (w, h) = want;
    let stride = w.div_ceil(8);
    let total = stride.checked_mul(h).ok_or_else(|| bad("an image that is too large"))?;
    work.spend(total as f64 * cost::JB2_CLEAR_BYTE);
    // Black is 1 in JBIG2 and 0 in the PDF's samples; what the page does not cover is white.
    let mut out = vec![0xFFu8; total];
    for (y, row) in out.chunks_exact_mut(stride.max(1)).enumerate().take(bitmap.h) {
        for (o, &b) in row.iter_mut().zip(bitmap.row(y)) {
            *o = !b;
        }
    }
    let warning = (!d.warnings.is_empty()).then(|| d.warnings.join("; "));
    Ok(Page { data: out, page_pixels: bitmap.w as u64 * bitmap.h as u64, warning })
}
