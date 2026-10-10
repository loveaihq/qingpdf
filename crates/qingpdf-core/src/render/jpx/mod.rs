//! `JPXDecode` (ISO 32000-1 7.4.9), after ISO/IEC 15444-1 (JPEG 2000 Part 1): a JP2 file or a bare codestream decoded
//! to 8-bit samples, at the resolution the page needs.
//!
//! The speed comes from not decoding what is not seen: a picture that is drawn at a quarter of its size is decoded at
//! a quarter of its size, which leaves out the code-blocks of the finest resolution levels (their packets are read
//! for their lengths, and no more), and a component the page does not use is not decoded at all.
//!
//! Layout: [`codestream`] reads the markers; [`tile`] builds a tile's resolutions, precincts and code-blocks and reads
//! its packets; [`recon`] decodes the code-blocks ([`t1`], with the MQ decoder of [`crate::render::mq`]), dequantizes,
//! applies the inverse wavelet transform ([`dwt`]) and the component transform; this file reads the JP2 boxes, walks
//! the tiles and turns the planes into pixels.
//!
//! Everything in the file is an untrusted request. What bounds it: caps on components, tiles, tile-parts, levels,
//! progression order changes, precincts, code-blocks, packets and passes, each checked with checked arithmetic before
//! anything sized by it is allocated; a memory cap ([`Ctx::take_memory`]) on every buffer; and the page's work meter
//! ([`Ctx::charge`]), which every header bit, tag-tree node, packet looked at, coding pass, decision, coefficient,
//! wavelet sample and pixel is charged to before it is done, whether or not it shows on the page.

mod codestream;
mod dwt;
mod recon;
mod t1;
mod tagtree;
mod tile;
#[cfg(test)]
mod test_enc;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_streams;
#[cfg(test)]
mod tests_hostile;

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use super::work::{Work, cost};
use crate::error::{Error, Result};
use codestream::Header;
use tile::{Part, TileDec, TileParams, ceil_shift, component_coding};

/// A fault of this image (not of the page's budget).
fn bad(m: &str) -> Error {
    Error::Invalid(format!("JPX: {m}"))
}

/// Bytes of buffers one image may hold at once: the planes, the tile being decoded, the structures of its packets.
/// (With the page itself, 64 MB at most, and the rest of the program, a page stays under 200 MB.)
const MAX_MEMORY: u64 = 120 * 1024 * 1024;
/// A tile whose three largest component buffers (4 bytes a sample) take more than this is decoded one component at a
/// time, the first three kept at two bytes a sample until the component transform needs them; a smaller one three
/// components at a time (their wavelet transforms run side by side), and exactly as the standard has it.
const GROUP_BYTES: u64 = 48 * 1024 * 1024;
/// Most colour channels an image has (a fifth, the opacity, is allowed on top).
const MAX_COLOUR: usize = 4;
/// Most boxes of a JP2 file looked at, and most entries of a palette (the standard's own limit).
const MAX_BOXES: usize = 1 << 12;
const MAX_PALETTE: usize = 1024;
/// A picture is decoded at a size that is at least this many percent of the size it is shown at.
const REDUCE_TOLERANCE: u64 = 97;
/// Most threads used, and the fewest samples worth waking them for.
const MAX_THREADS: usize = 8;
pub(super) const MIN_PARALLEL_SAMPLES: usize = 1 << 18;
/// Most warnings kept.
const MAX_WARNINGS: usize = 8;

#[cfg(test)]
thread_local! {
    /// Tests make `GROUP_BYTES` small to take the way of the big pictures with small ones.
    pub(super) static GROUP_BYTES_TEST: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

fn group_bytes() -> u64 {
    #[cfg(test)]
    if let Some(v) = GROUP_BYTES_TEST.with(std::cell::Cell::get) {
        return v;
    }
    GROUP_BYTES
}

/// The warnings of one image: the first few different ones, and how many more there were (one for every damaged tile
/// or tile-part would be as many strings as the file has tiles).
#[derive(Default)]
pub(super) struct Warnings {
    list: Vec<String>,
    more: usize,
}

impl Warnings {
    pub fn push(&mut self, m: String) {
        if self.list.contains(&m) {
            return;
        }
        if self.list.len() < MAX_WARNINGS {
            self.list.push(m);
        } else {
            self.more = self.more.saturating_add(1);
        }
    }

    fn pop(&mut self) -> Option<String> {
        self.list.pop()
    }

    fn text(&self) -> Option<String> {
        if self.list.is_empty() {
            return None;
        }
        let mut t = self.list.join("; ");
        if self.more > 0 {
            t.push_str(&format!("; and {} more", self.more));
        }
        Some(t)
    }
}

/// What every part of the decoder charges and takes memory from.
pub(super) struct Ctx<'a> {
    work: &'a Work,
    memory: AtomicU64,
    /// The most memory there is, [`MAX_MEMORY`] or what the page's work meter allows.
    cap: u64,
    /// The least memory there was left at any moment.
    #[cfg(test)]
    low: AtomicU64,
    /// Most threads the decode may use.
    threads: usize,
}

impl<'a> Ctx<'a> {
    fn new(work: &'a Work, threads: Option<usize>) -> Ctx<'a> {
        let threads = threads.unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |t| t.get()).min(MAX_THREADS));
        let cap = work.decoder_memory().min(MAX_MEMORY);
        Ctx { work, memory: AtomicU64::new(cap), cap, #[cfg(test)] low: AtomicU64::new(cap), threads: threads.max(1) }
    }

    /// How many threads to use for jobs of `samples` samples in all: one when there are too few to be worth starting
    /// a thread for (starting one costs about as much as decoding a few thousand samples).
    pub fn threads_for(&self, samples: usize) -> usize {
        if samples < MIN_PARALLEL_SAMPLES { 1 } else { self.threads }
    }

    /// Spend `units` of the page's work before doing it. [`Error::Limit`]: the page has no more.
    pub fn charge(&self, units: f64) -> Result<()> {
        if self.work.charge(units) { Ok(()) } else { Err(Error::Limit("the page's work allowance is used up (JPX image)".to_string())) }
    }

    /// Spend `units` on work that is done already.
    pub fn spend(&self, units: f64) {
        self.work.spend(units);
    }

    pub fn take_memory(&self, bytes: u64) -> Result<()> {
        match self.memory.try_update(Ordering::Relaxed, Ordering::Relaxed, |left| left.checked_sub(bytes)) {
            Ok(_left) => {
                #[cfg(test)]
                self.low.fetch_min(_left.saturating_sub(bytes), Ordering::Relaxed);
                Ok(())
            }
            Err(_) => Err(Error::Limit("JPX: the image needs more memory than is allowed".to_string())),
        }
    }

    pub fn give_back(&self, bytes: u64) {
        let _ = self.memory.try_update(Ordering::Relaxed, Ordering::Relaxed, |left| Some(left.saturating_add(bytes).min(self.cap)));
    }

    #[cfg(test)]
    pub fn memory_left(&self) -> u64 {
        self.memory.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub fn memory_low(&self) -> u64 {
        self.low.load(Ordering::Relaxed)
    }
}

/// A component's samples at the resolution that is decoded: a byte each, or two when it has more than 8 bits.
pub(super) enum PlaneData {
    Bytes(Vec<u8>),
    Words(Vec<u16>),
}

pub(super) struct Plane {
    pub x0: u64,
    pub y0: u64,
    pub w: usize,
    pub h: usize,
    /// What the samples are before a tile covers them: the middle of the range.
    mid: u16,
    pub data: PlaneData,
}

impl Plane {
    /// A plane whose samples are not made yet (a picture of one tile needs the plane only when the tile is done).
    fn new(x0: u64, y0: u64, w: usize, h: usize, depth: u8) -> Plane {
        let data = if depth > 8 { PlaneData::Words(Vec::new()) } else { PlaneData::Bytes(Vec::new()) };
        Plane { x0, y0, w, h, mid: 1u16 << (depth - 1), data }
    }

    /// Make the samples, grey, if that is not done.
    pub fn ensure(&mut self, ctx: &Ctx) -> Result<()> {
        let n = self.w * self.h;
        let (made, per) = match &self.data {
            PlaneData::Bytes(d) => (!d.is_empty(), 1u64),
            PlaneData::Words(d) => (!d.is_empty(), 2u64),
        };
        if made || n == 0 {
            return Ok(());
        }
        ctx.take_memory(n as u64 * per)?;
        ctx.charge(n as f64 * per as f64 * cost::JPX_CLEAR_BYTE)?;
        self.data = if per == 2 { PlaneData::Words(vec![self.mid; n]) } else { PlaneData::Bytes(vec![self.mid as u8; n]) };
        Ok(())
    }

    /// Row `y`, as 16-bit samples (`tmp` is where a plane of bytes is widened to).
    fn row<'a>(&'a self, y: usize, tmp: &'a mut Vec<u16>) -> Option<&'a [u16]> {
        let range = y * self.w..(y + 1) * self.w;
        match &self.data {
            PlaneData::Words(d) => d.get(range),
            PlaneData::Bytes(d) => {
                tmp.clear();
                tmp.extend(d.get(range)?.iter().map(|&b| u16::from(b)));
                Some(tmp)
            }
        }
    }
}

/// What the PDF image dictionary says about how to use the picture.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Alpha {
    /// The file's opacity channel, if it has one, is not wanted.
    Ignore,
    /// `/SMaskInData 1`: the opacity channel is the image's alpha.
    Straight,
    /// `/SMaskInData 2`: and the colours are multiplied by it already.
    Premultiplied,
}

pub(crate) struct Options {
    /// About how big the picture is on the device: it is decoded at the smallest size that is not smaller.
    pub want: (usize, usize),
    /// How many colour channels the image dictionary's `/ColorSpace` has (it overrides the file's colour space).
    pub components: Option<usize>,
    /// The image dictionary's colour space is `Indexed`: the samples are indices, taken as they are, and the file's
    /// palette is not applied.
    pub indexed: bool,
    pub alpha: Alpha,
    /// Most threads to use (`None`: as many as the machine has, up to 8).
    pub threads: Option<usize>,
}

pub(crate) struct Decoded {
    pub w: usize,
    pub h: usize,
    /// Colour channels in `data`: 1, 3 or 4, interleaved, 8 bits each.
    pub ncomp: usize,
    pub data: Vec<u8>,
    /// The opacity of each pixel, if the image has it and it was asked for.
    pub alpha: Option<Vec<u8>>,
    /// The size of the picture at full resolution.
    pub full: (u64, u64),
    pub warning: Option<String>,
    /// Memory of the image's allowance that was not given back (what was left when it was done: the planes and the
    /// pixels are still counted).
    #[cfg(test)]
    pub memory_left: u64,
    /// The least memory of the allowance there was left at any moment of the decode.
    #[cfg(test)]
    pub memory_low: u64,
}

// --- the JP2 file -----------------------------------------------------------------------------------------------------

struct Palette {
    entries: usize,
    /// Bits of each column.
    bits: Vec<u8>,
    values: Vec<u16>,
}

#[derive(Default)]
struct Jp2<'a> {
    codestream: &'a [u8],
    /// The enumerated colour space of the first `colr` box, or the channel count of its ICC profile.
    enumcs: Option<u32>,
    icc_channels: Option<usize>,
    has_colr: bool,
    palette: Option<Palette>,
    /// (component, mapping type, palette column)
    cmap: Vec<(u16, u8, u8)>,
    /// (channel, type, association)
    cdef: Vec<(u16, u16, u16)>,
}

fn be16(d: &[u8], at: usize) -> Option<u16> {
    let b: [u8; 2] = d.get(at..at.checked_add(2)?)?.try_into().ok()?;
    Some(u16::from_be_bytes(b))
}

fn be32(d: &[u8], at: usize) -> Option<u32> {
    let b: [u8; 4] = d.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_be_bytes(b))
}

/// The boxes in `data`: (type, payload).
fn boxes<'a>(ctx: &Ctx, data: &'a [u8]) -> Result<Vec<([u8; 4], &'a [u8])>> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + 8 <= data.len() {
        ctx.charge(cost::JPX_BOX)?;
        if out.len() >= MAX_BOXES {
            return Err(Error::Limit("JPX: too many boxes".to_string()));
        }
        let lbox = be32(data, pos).unwrap_or(0);
        let kind: [u8; 4] = data.get(pos + 4..pos + 8).and_then(|b| b.try_into().ok()).unwrap_or([0; 4]);
        let (header, len) = match lbox {
            0 => (8usize, data.len() - pos),
            1 => {
                let hi = u64::from(be32(data, pos + 8).unwrap_or(0));
                let lo = u64::from(be32(data, pos + 12).unwrap_or(0));
                (16usize, usize::try_from((hi << 32) | lo).unwrap_or(usize::MAX))
            }
            n => (8usize, n as usize),
        };
        if len < header {
            return Err(bad("a box shorter than its header"));
        }
        let end = pos.saturating_add(len).min(data.len());
        out.push((kind, data.get(pos + header..end).unwrap_or(&[])));
        pos = end;
    }
    Ok(out)
}

fn parse_jp2<'a>(ctx: &Ctx, data: &'a [u8]) -> Result<Jp2<'a>> {
    if data.starts_with(&[0xFF, 0x4F, 0xFF, 0x51]) {
        return Ok(Jp2 { codestream: data, ..Jp2::default() });
    }
    if !data.starts_with(&[0, 0, 0, 12, b'j', b'P', b' ', b' ']) {
        return Err(bad("neither a JP2 file nor a codestream"));
    }
    let mut jp2 = Jp2::default();
    let mut found = false;
    for (kind, payload) in boxes(ctx, data)? {
        match &kind {
            b"jp2c" => {
                jp2.codestream = payload;
                found = true;
                break;
            }
            b"jp2h" => {
                for (k, p) in boxes(ctx, payload)? {
                    match &k {
                        b"colr" if !jp2.has_colr => {
                            jp2.has_colr = true;
                            match p.first() {
                                Some(1) => jp2.enumcs = be32(p, 3),
                                Some(2) => {
                                    jp2.icc_channels = match p.get(3 + 16..3 + 20) {
                                        Some(b"RGB ") => Some(3),
                                        Some(b"GRAY") => Some(1),
                                        Some(b"CMYK") => Some(4),
                                        _ => None,
                                    }
                                }
                                _ => {}
                            }
                        }
                        b"pclr" => jp2.palette = parse_palette(p),
                        b"cmap" => {
                            jp2.cmap = p.chunks_exact(4).take(MAX_PALETTE).filter_map(|c| Some((be16(c, 0)?, *c.get(2)?, *c.get(3)?))).collect();
                        }
                        b"cdef" => {
                            let n = usize::from(be16(p, 0).unwrap_or(0));
                            jp2.cdef = (0..n).filter_map(|i| Some((be16(p, 2 + i * 6)?, be16(p, 4 + i * 6)?, be16(p, 6 + i * 6)?))).collect();
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    if !found {
        return Err(bad("no codestream box"));
    }
    Ok(jp2)
}

/// The palette box (I.5.3.4): entries, columns, the bits of each column, then the table.
fn parse_palette(p: &[u8]) -> Option<Palette> {
    let entries = usize::from(be16(p, 0)?);
    let cols = usize::from(*p.get(2)?);
    if entries == 0 || entries > MAX_PALETTE || cols == 0 {
        return None;
    }
    let bits: Vec<u8> = p.get(3..3 + cols)?.iter().map(|b| (b & 0x7F) + 1).collect();
    if bits.iter().any(|&b| b > 16) {
        return None;
    }
    let widths: Vec<usize> = bits.iter().map(|&b| usize::from(b).div_ceil(8)).collect();
    let row: usize = widths.iter().sum();
    let table = p.get(3 + cols..)?;
    if table.len() < row.checked_mul(entries)? {
        return None;
    }
    let mut values = Vec::with_capacity(entries * cols);
    let mut at = 0usize;
    for _ in 0..entries {
        for &w in &widths {
            values.push(table.get(at..at + w)?.iter().fold(0u16, |a, &b| (a << 8) | u16::from(b)));
            at += w;
        }
    }
    Some(Palette { entries, bits, values })
}

// --- channels ---------------------------------------------------------------------------------------------------------

/// Where a channel of the result comes from: a component, perhaps through a column of the palette.
#[derive(Clone, Copy)]
struct Chan {
    comp: usize,
    column: Option<usize>,
}

struct Plan {
    colour: Vec<Chan>,
    alpha: Option<Chan>,
    sycc: bool,
}

fn plan_channels(jp2: &Jp2<'_>, ncomp: usize, siz: &codestream::Siz, opts: &Options, warnings: &mut Warnings) -> Result<Plan> {
    let use_palette = jp2.palette.is_some() && !jp2.cmap.is_empty() && !opts.indexed;
    let channels: Vec<Chan> = if use_palette {
        jp2.cmap.iter().map(|&(comp, kind, column)| Chan { comp: usize::from(comp), column: (kind == 1).then_some(usize::from(column)) }).collect()
    } else {
        (0..ncomp).map(|comp| Chan { comp, column: None }).collect()
    };
    if channels.iter().any(|c| c.comp >= ncomp || c.column.is_some_and(|k| jp2.palette.as_ref().is_none_or(|p| k >= p.bits.len()))) {
        return Err(bad("a palette mapping that names what is not there"));
    }
    let mut colour = Vec::new();
    let mut alpha = None;
    if !jp2.cdef.is_empty() {
        // (A box can name 65534 channels. A channel is a colour once, and the colour space has as many as it has: the
        // image dictionary's, or four at most; what is named past that is not used.)
        let cap = opts.components.unwrap_or(MAX_COLOUR).clamp(1, channels.len().max(1));
        let mut named: Vec<(u16, usize)> = Vec::new();
        let mut used = vec![false; channels.len()];
        let mut skipped = false;
        for &(ch, typ, asoc) in &jp2.cdef {
            let ch = usize::from(ch);
            match typ {
                0 if (1..=0xFFFE).contains(&asoc) && ch < channels.len() => {
                    if used.get(ch).copied().unwrap_or(true) || named.len() >= cap {
                        skipped = true;
                    } else {
                        named.push((asoc, ch));
                        if let Some(u) = used.get_mut(ch) {
                            *u = true;
                        }
                    }
                }
                1 | 2 if asoc == 0 && alpha.is_none() && ch < channels.len() => alpha = Some(ch),
                _ => {}
            }
        }
        if skipped {
            warnings.push(format!("the channel definitions name more colour channels than the colour space has; {cap} are used"));
        }
        named.sort();
        colour = named.into_iter().map(|(_, ch)| ch).collect();
    }
    if colour.is_empty() {
        let natural = match jp2.enumcs {
            Some(16 | 18) => Some(3),
            Some(17) => Some(1),
            Some(12) => Some(4),
            _ => jp2.icc_channels,
        };
        // The dictionary's colour space comes first (7.4.9); then what the file says; then a guess from the count.
        let n = opts.components.or(natural).unwrap_or(match channels.len() {
            0 | 1 => 1,
            2 => 1,
            3 => 3,
            _ => 4,
        });
        let n = n.min(channels.len()).max(1);
        colour = (0..n).collect();
        if alpha.is_none() && channels.len() > n && opts.alpha != Alpha::Ignore {
            alpha = Some(n);
        }
    }
    let to_chan = |i: usize| channels.get(i).copied();
    let colour: Vec<Chan> = colour.into_iter().filter_map(to_chan).collect();
    let alpha = alpha.and_then(to_chan).filter(|_| opts.alpha != Alpha::Ignore);
    if colour.is_empty() {
        return Err(bad("no colour channel"));
    }
    // sYCC: the file says so, or a bare codestream has chroma at lower resolution than luma. (When the image dictionary
    // has a /ColorSpace, the samples are in it and the colour space of the file is not looked at, 7.4.9.)
    let c1_sub = siz.comps.get(1).zip(siz.comps.first()).is_some_and(|(b, a)| b.dx != a.dx || b.dy != a.dy);
    let sycc = opts.components.is_none() && colour.len() == 3 && !use_palette && (jp2.enumcs == Some(18) || (!jp2.has_colr && c1_sub));
    Ok(Plan { colour, alpha, sycc })
}

// --- the walk over the tiles ------------------------------------------------------------------------------------------

/// The resolution levels left out: as many as keep the picture at least as big as it is shown, and as many more as
/// the memory cap needs.
fn choose_reduction(ctx: &Ctx, h: &Header, needed: &[bool], opts: &Options, ncol: usize) -> Result<usize> {
    let siz = &h.siz;
    let mut r_max = usize::MAX;
    for (c, &n) in needed.iter().enumerate() {
        if n {
            let levels = h.main.coc.get(&c).map(|s| s.levels).or_else(|| h.main.cod.as_ref().map(|d| d.style.levels)).ok_or_else(|| bad("no COD"))?;
            r_max = r_max.min(usize::from(levels));
        }
    }
    let r_max = if r_max == usize::MAX { 0 } else { r_max };
    let (w, h_) = (u64::from(siz.x1 - siz.x0), u64::from(siz.y1 - siz.y0));
    let (tw, th) = (opts.want.0 as u64, opts.want.1 as u64);
    let mut r = 0usize;
    if tw > 0 && th > 0 {
        // (A picture a few percent smaller than it is shown, as a page at 150 dpi is by a pixel or two of rounding, is no
        // reason to decode four times the pixels.)
        while r < r_max && ceil_shift(w, r as u32 + 1) * 100 >= tw * REDUCE_TOLERANCE && ceil_shift(h_, r as u32 + 1) * 100 >= th * REDUCE_TOLERANCE {
            r += 1;
        }
    }
    // What it would hold at once at this reduction. A tile: the planes (not yet made when the picture is one tile),
    // and the buffers of the tile being decoded, which are three components' at a time, or, for a big tile, one
    // component's, with the first three kept at two bytes a sample for the component transform. The pixels: the
    // planes, the pixels made from them, and a row of columns.
    let single = siz.tiles_across().saturating_mul(siz.tiles_down()) <= 1;
    let first3 = siz.comps.len() >= 3 && needed.iter().take(3).any(|&n| n);
    let mct3 = first3 && h.main.cod.as_ref().is_some_and(|d| d.mct != 0);
    // (Two bytes a sample hold the first three components when they are integers of at most 14 bits, or numbers of at
    // most 8.)
    let packs = (0..3).all(|c| {
        let reversible = h.main.coc.get(&c).map(|s| s.reversible).or_else(|| h.main.cod.as_ref().map(|d| d.style.reversible)).unwrap_or(false);
        siz.comps.get(c).is_none_or(|comp| comp.depth <= if reversible { 14 } else { 8 })
    });
    let kept: u64 = if packs { 2 } else { 4 };
    let estimate = |r: usize| -> u64 {
        let d = r as u32;
        let (ow, oh) = (ceil_shift(w, d), ceil_shift(h_, d));
        let mut planes = 0u64;
        let mut wanted = 0u64;
        for (c, comp) in siz.comps.iter().enumerate() {
            if needed.get(c).copied().unwrap_or(false) {
                let bytes = if comp.depth > 8 { 2 } else { 1 };
                planes += ow.div_ceil(u64::from(comp.dx)) * oh.div_ceil(u64::from(comp.dy)) * bytes;
                wanted += 1;
            }
        }
        let in_flight = if first3 { 3 } else { wanted.clamp(1, 3) };
        let (tile_w, tile_h) = (ceil_shift(u64::from(siz.tw).min(w), d), ceil_shift(u64::from(siz.th).min(h_), d));
        let samples = tile_w * tile_h;
        let tile = if samples * 12 <= group_bytes() {
            planes + samples * 4 * in_flight
        } else if mct3 {
            let planes_now = if single { 0 } else { planes };
            (planes_now + samples * (2 * kept + 4)).max(planes + samples * 3 * kept).max(planes + samples * 4)
        } else {
            planes + samples * 4
        };
        let pixels = planes + ow * oh * (ncol as u64 + 1) + ow * 4 * ncol as u64;
        tile.max(pixels) + (4 << 20)
    };
    while r < r_max && estimate(r) > ctx.memory_budget() {
        r += 1;
    }
    if estimate(r) > ctx.memory_budget() {
        return Err(Error::Limit("JPX: the image is too large to decode".to_string()));
    }
    Ok(r)
}

impl Ctx<'_> {
    fn memory_budget(&self) -> u64 {
        self.memory.load(Ordering::Relaxed)
    }
}

struct Walk<'a> {
    ctx: &'a Ctx<'a>,
    data: &'a [u8],
    h: &'a Header,
    needed: &'a [bool],
    reduce: usize,
    warnings: Warnings,
}

impl<'a> Walk<'a> {
    fn warn(&mut self, m: String) {
        self.warnings.push(m);
    }

    /// Decode tile `t` onto the planes.
    fn tile(&mut self, t: u64, planes: &mut [Option<Plane>]) -> Result<()> {
        let ctx = self.ctx;
        let h = self.h;
        let siz = &h.siz;
        let Some(entry) = h.tiles.get(&t) else { return Ok(()) };
        ctx.charge(cost::JPX_TILE)?;
        let rect = siz.tile_rect(t);
        let cod = entry.coding.cod.as_ref().or(h.main.cod.as_ref()).ok_or_else(|| bad("no COD"))?;
        let ncomp = siz.comps.len();
        let mct = cod.mct != 0 && ncomp >= 3;
        let mut td = TileDec::new(ctx, TileParams { layers: cod.layers }, rect);
        let result = self.tile_inner(&mut td, t, planes, cod.prog, mct);
        let held = td.held;
        if let Some(why) = td.damaged.take() {
            self.warn(format!("a tile is damaged ({why}); what could be read is drawn"));
        }
        drop(td);
        ctx.give_back(held);
        result
    }

    fn tile_inner(&mut self, td: &mut TileDec<'a>, t: u64, planes: &mut [Option<Plane>], prog: u8, mct: bool) -> Result<()> {
        let h = self.h;
        let siz = &h.siz;
        let Some(entry) = h.tiles.get(&t) else { return Ok(()) };
        let ncomp = siz.comps.len();
        let any_first = self.needed.iter().take(3).any(|&n| n);
        for (c, comp) in siz.comps.iter().enumerate() {
            let (style, quant, roi) = component_coding(h, &entry.coding, c)?;
            let need = self.needed.get(c).copied().unwrap_or(false) || (mct && c < 3 && any_first);
            td.add_component(comp, style, quant, roi, need, self.reduce)?;
        }
        // The tile-parts' bodies, and where the packet headers are if they are not in them.
        let mut ppt: Vec<(u8, usize, usize)> = entry.ppt.clone();
        ppt.sort_by_key(|p| p.0);
        for part in &entry.parts {
            let (a, b) = part.body;
            let hdr = part.ppm.map(|(x, y)| h.ppm.get(x..y).unwrap_or(&[]).to_vec());
            td.parts.push(Part { body: self.data.get(a..b).unwrap_or(&[]), body_off: a, hdr, bpos: 0, hpos: 0 });
        }
        if !ppt.is_empty() {
            let mut packed = Vec::new();
            for (_, a, b) in ppt {
                packed.extend_from_slice(self.data.get(a..b).unwrap_or(&[]));
            }
            td.take(packed.len() as u64)?;
            td.ppt = Some(packed);
        }
        td.read_packets(&h.main.poc, &entry.coding.poc, prog)?;
        // Decode, transform and put on the planes, three components at a time (so that their wavelet transforms and
        // their writing can run side by side); the first three together are what the component transform needs. A tile
        // that is big is done one component at a time instead (three buffers of it at 4 bytes a sample are too much):
        // the first three are kept at two bytes a sample until the component transform has all of them.
        let mut biggest = 0u64;
        for comp in td.comps.iter().filter(|c| c.need) {
            if let Some(top) = comp.res.get(comp.keep) {
                biggest = biggest.max(u64::from(top.x1 - top.x0) * u64::from(top.y1 - top.y0) * 4);
            }
        }
        let big = biggest.saturating_mul(3) > group_bytes();
        let first = if big && mct {
            self.transform_packed(td, planes)?;
            3
        } else {
            0
        };
        let order: Vec<usize> = (first..ncomp).collect();
        for group in order.chunks(if big { 1 } else { 3 }) {
            let mut recs: Vec<recon::Recon> = Vec::new();
            for &c in group {
                if let Some(rec) = td.decode_blocks(c, self.data, &mut self.warnings)? {
                    recs.push(rec);
                }
                td.free_coding(c);
            }
            if group.last() == Some(&(ncomp - 1)) {
                td.free_chunks();
            }
            if recs.is_empty() {
                continue;
            }
            td.transform(&mut recs.iter_mut().collect::<Vec<_>>())?;
            if mct
                && group.first() == Some(&0)
                && let [a, b, c] = recs.as_mut_slice()
                && !recon::component_transform(a, b, c)
            {
                self.warn("the component transform does not fit the first three components; skipped".to_string());
            }
            self.put_all(td, &recs, planes)?;
            for rec in &recs {
                td.release(rec);
            }
        }
        Ok(())
    }

    /// The first three components of a big tile with the component transform: each is decoded and transformed alone,
    /// then kept at two bytes a sample (when it can be; else as it is) until all three are there, and the component
    /// transform and the writing on the planes are done a few rows at a time.
    fn transform_packed(&mut self, td: &mut TileDec<'a>, planes: &mut [Option<Plane>]) -> Result<()> {
        let mut packed: Vec<recon::Packed> = Vec::new();
        for c in 0..3 {
            let Some(mut rec) = td.decode_blocks(c, self.data, &mut self.warnings)? else { continue };
            td.free_coding(c);
            td.transform(&mut [&mut rec])?;
            packed.push(td.pack(rec)?);
        }
        if td.comps.len() <= 3 {
            td.free_chunks();
        }
        let depths: Vec<u8> = packed.iter().map(|p| td.comps.get(p.comp).map_or(8, |c| c.depth)).collect();
        if !recon::store_packed(self.ctx, &packed, &depths, planes, packed.len() == 3)? && packed.len() == 3 {
            self.warn("the component transform does not fit the first three components; skipped".to_string());
        }
        for p in &packed {
            td.release_packed(p);
        }
        Ok(())
    }

    /// Put reconstructions on the planes of their components, one thread for each when there are several.
    fn put_all(&self, td: &TileDec<'a>, recs: &[recon::Recon], planes: &mut [Option<Plane>]) -> Result<()> {
        let mut jobs: Vec<Mutex<(&recon::Recon, &mut Plane, u8)>> = Vec::new();
        for (c, slot) in planes.iter_mut().enumerate() {
            if let (Some(plane), Some(rec), Some(comp)) = (slot.as_mut(), recs.iter().find(|r| r.comp == c), td.comps.get(c)) {
                jobs.push(Mutex::new((rec, plane, comp.depth)));
            }
        }
        let ctx = self.ctx;
        let samples: usize = recs.iter().map(|r| r.w * r.h).sum();
        let results = recon::par_map(
            jobs.len(),
            ctx.threads_for(samples).min(jobs.len()).max(1),
            || (),
            |_, i| match jobs.get(i) {
                Some(job) => {
                    let mut g = job.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    let (rec, plane, depth) = &mut *g;
                    recon::store(ctx, rec, *depth, plane)
                }
                None => Ok(()),
            },
            |r| r.is_err(),
        );
        for r in results {
            r.ok_or_else(|| bad("a worker was lost"))??;
        }
        Ok(())
    }
}

/// Decode a JP2 file or codestream. `Err(Error::Limit)`: the page's work (or this image's memory cap) is used up.
/// `Err(Error::Invalid)`: nothing could be decoded; the caller draws a placeholder.
pub(crate) fn decode(data: &[u8], opts: &Options, work: &Work) -> Result<Decoded> {
    let ctx = Ctx::new(work, opts.threads);
    let jp2 = parse_jp2(&ctx, data)?;
    let mut hdr = codestream::parse(&ctx, jp2.codestream)?;
    let mut warnings = std::mem::take(&mut hdr.warnings);
    let siz = &hdr.siz;
    let ncomp = siz.comps.len();
    let plan = plan_channels(&jp2, ncomp, siz, opts, &mut warnings)?;
    let mut needed = vec![false; ncomp];
    for ch in plan.colour.iter().chain(plan.alpha.iter()) {
        if let Some(n) = needed.get_mut(ch.comp) {
            *n = true;
        }
    }
    let ncol = plan.colour.len();
    let reduce = choose_reduction(&ctx, &hdr, &needed, opts, ncol)?;
    let d = reduce as u32;
    let (ow, oh) = ((ceil_shift(u64::from(siz.x1), d) - ceil_shift(u64::from(siz.x0), d)) as usize, (ceil_shift(u64::from(siz.y1), d) - ceil_shift(u64::from(siz.y0), d)) as usize);
    if ow == 0 || oh == 0 {
        return Err(bad("nothing of the picture is left at this size"));
    }
    // The planes, grey until a tile covers them (made when the first tile is put on them, or at once when there are
    // several tiles).
    let several = siz.tiles_across().saturating_mul(siz.tiles_down()) > 1;
    let mut planes: Vec<Option<Plane>> = Vec::with_capacity(ncomp);
    for (c, comp) in siz.comps.iter().enumerate() {
        if !needed.get(c).copied().unwrap_or(false) {
            planes.push(None);
            continue;
        }
        let (dx, dy) = (u64::from(comp.dx), u64::from(comp.dy));
        let (cx0, cx1) = (u64::from(siz.x0).div_ceil(dx), u64::from(siz.x1).div_ceil(dx));
        let (cy0, cy1) = (u64::from(siz.y0).div_ceil(dy), u64::from(siz.y1).div_ceil(dy));
        let (px0, py0) = (ceil_shift(cx0, d), ceil_shift(cy0, d));
        let (pw, ph) = ((ceil_shift(cx1, d) - px0) as usize, (ceil_shift(cy1, d) - py0) as usize);
        pw.checked_mul(ph).ok_or_else(|| Error::Limit("JPX: image too large".to_string()))?;
        let mut plane = Plane::new(px0, py0, pw, ph, comp.depth);
        if several {
            plane.ensure(&ctx)?;
        }
        planes.push(Some(plane));
    }
    let mut walk = Walk { ctx: &ctx, data: jp2.codestream, h: &hdr, needed: &needed, reduce, warnings };
    let mut order: Vec<u64> = hdr.tiles.keys().copied().collect();
    order.sort_unstable();
    let mut good = 0usize;
    for t in order {
        match walk.tile(t, &mut planes) {
            Ok(()) => good += 1,
            Err(Error::Limit(m)) => return Err(Error::Limit(m)),
            Err(e) => walk.warn(format!("a tile cannot be decoded ({e})")),
        }
    }
    if good == 0 {
        return Err(walk.warnings.pop().map_or_else(|| bad("no tile could be decoded"), Error::Invalid));
    }
    let tiles = siz.tiles_across() * siz.tiles_down();
    if (hdr.tiles.len() as u64) < tiles {
        walk.warn(format!("{} of {tiles} tiles have no data", tiles - hdr.tiles.len() as u64));
    }
    let warnings = std::mem::take(&mut walk.warnings);
    drop(walk);
    // (A plane no tile reached is grey.)
    for plane in planes.iter_mut().flatten() {
        plane.ensure(&ctx)?;
    }
    let mut out = assemble(&ctx, &jp2, &plan, siz, planes, (ow, oh), opts)?;
    out.full = (u64::from(siz.x1 - siz.x0), u64::from(siz.y1 - siz.y0));
    out.warning = warnings.text();
    Ok(out)
}

/// A sample of `bits` bits as 8 bits.
fn to8(v: u32, bits: u8) -> u8 {
    match bits {
        8 => v as u8,
        b if b > 8 => ((v + (1 << (b - 9))) >> (b - 8)).min(255) as u8,
        b => {
            let max = (1u32 << b) - 1;
            ((v.min(max) * 255 + max / 2) / max) as u8
        }
    }
}

/// Tables of samples to 8 bits, by (component, palette column).
type Tables = Vec<((usize, Option<usize>), Vec<u8>)>;

/// The table that turns a sample of channel `ch` into 8 bits (through the palette if there is one): its place in `tables`.
/// A table is made once for each component and palette column, however many channels name it, and what it takes is
/// counted (a 16-bit component has 65536 entries).
fn table_of(ctx: &Ctx, jp2: &Jp2<'_>, siz: &codestream::Siz, opts: &Options, tables: &mut Tables, ch: &Chan) -> Result<usize> {
    if let Some(i) = tables.iter().position(|(key, _)| *key == (ch.comp, ch.column)) {
        return Ok(i);
    }
    let comp = siz.comps.get(ch.comp).ok_or_else(|| bad("a channel of a component that is not there"))?;
    let size = 1usize << comp.depth;
    ctx.take_memory(size as u64)?;
    ctx.charge(size as f64 * cost::JPX_PIXEL)?;
    let lut: Vec<u8> = match (ch.column, jp2.palette.as_ref()) {
        (Some(col), Some(p)) => {
            let bits = p.bits.get(col).copied().unwrap_or(8);
            (0..size)
                .map(|i| {
                    let e = i.min(p.entries - 1);
                    to8(u32::from(p.values.get(e * p.bits.len() + col).copied().unwrap_or(0)), bits)
                })
                .collect()
        }
        _ if opts.indexed => (0..size).map(|i| i.min(255) as u8).collect(),
        _ => (0..size).map(|i| to8(i as u32, comp.depth)).collect(),
    };
    tables.push(((ch.comp, ch.column), lut));
    Ok(tables.len() - 1)
}

/// For each of `w` pixels of a row, the column of the plane it is taken from (a component at a lower resolution has
/// fewer).
fn columns(plane: &Plane, w: usize) -> Vec<u32> {
    (0..w).map(|x| ((x as u64 * plane.w as u64) / (w as u64).max(1)).min(plane.w.saturating_sub(1) as u64) as u32).collect()
}

/// The planes as pixels: palette applied, channels in order, 8 bits, sYCC turned into RGB. A plane is let go when its
/// last channel is made.
fn assemble(ctx: &Ctx, jp2: &Jp2<'_>, plan: &Plan, siz: &codestream::Siz, mut planes: Vec<Option<Plane>>, (w, h): (usize, usize), opts: &Options) -> Result<Decoded> {
    let ncol = plan.colour.len();
    let n = w.checked_mul(h).ok_or_else(|| Error::Limit("JPX: image too large".to_string()))?;
    let total = n.checked_mul(ncol).ok_or_else(|| Error::Limit("JPX: image too large".to_string()))?;
    ctx.take_memory(total as u64 + if plan.alpha.is_some() { n as u64 } else { 0 })?;
    ctx.charge((total + n) as f64 * cost::JPX_PIXEL)?;
    let mut tables = Tables::new();
    let mut data = vec![0u8; total];
    // Bands of rows, each made by a thread of its own when the picture is big.
    let threads = ctx.threads_for(n);
    let rows_per = h.div_ceil(threads * 4).max(1);
    let alpha_wanted = plan.alpha.filter(|_| opts.alpha != Alpha::Ignore);
    // One channel at a time, so that its plane can go once nothing else needs it.
    for (k, ch) in plan.colour.iter().enumerate() {
        let table = table_of(ctx, jp2, siz, opts, &mut tables, ch)?;
        let lut: &[u8] = tables.get(table).map_or(&[], |t| t.1.as_slice());
        if let Some(Some(plane)) = planes.get(ch.comp) {
            let xs_bytes = w as u64 * 4;
            ctx.take_memory(xs_bytes)?;
            let xs = columns(plane, w);
            let bands: Vec<Mutex<&mut [u8]>> = data.chunks_mut(rows_per * w.max(1) * ncol).map(Mutex::new).collect();
            recon::par_map(
                bands.len(),
                threads,
                Vec::<u16>::new,
                |tmp, i| {
                    let Some(band) = bands.get(i) else { return };
                    let mut band = band.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    for (r, dst) in band.chunks_exact_mut(w * ncol).enumerate() {
                        let y = i * rows_per + r;
                        let sy = ((y as u64 * plane.h as u64) / (h as u64).max(1)).min(plane.h.saturating_sub(1) as u64) as usize;
                        let Some(row) = plane.row(sy, tmp) else { continue };
                        for (px, &sx) in dst.chunks_exact_mut(ncol).zip(&xs) {
                            if let (Some(d), Some(&v)) = (px.get_mut(k), row.get(sx as usize)) {
                                *d = lut.get(usize::from(v)).copied().unwrap_or(0);
                            }
                        }
                    }
                },
                |_| false,
            );
            drop(bands);
            drop(xs);
            ctx.give_back(xs_bytes);
        }
        let later = plan.colour.iter().skip(k + 1).chain(alpha_wanted.iter()).any(|c| c.comp == ch.comp);
        if !later && let Some(slot) = planes.get_mut(ch.comp) {
            *slot = None;
        }
    }
    if plan.sycc && ncol == 3 {
        let bands: Vec<Mutex<&mut [u8]>> = data.chunks_mut(rows_per * w.max(1) * ncol).map(Mutex::new).collect();
        recon::par_map(
            bands.len(),
            threads,
            || (),
            |_, i| {
                let Some(band) = bands.get(i) else { return };
                let mut band = band.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                for px in band.chunks_exact_mut(3) {
                    if let [y, cb, cr] = px {
                        let (yy, pb, pr) = (f32::from(*y), f32::from(*cb) - 128.0, f32::from(*cr) - 128.0);
                        *y = (yy + 1.402 * pr + 0.5).clamp(0.0, 255.0) as u8;
                        *cb = (yy - 0.344_136 * pb - 0.714_136 * pr + 0.5).clamp(0.0, 255.0) as u8;
                        *cr = (yy + 1.772 * pb + 0.5).clamp(0.0, 255.0) as u8;
                    }
                }
            },
            |_| false,
        );
    }
    let alpha = match alpha_wanted {
        Some(ch) => {
            let table = table_of(ctx, jp2, siz, opts, &mut tables, &Chan { column: None, ..ch })?;
            let lut: &[u8] = tables.get(table).map_or(&[], |t| t.1.as_slice());
            let mut a = vec![255u8; n];
            if let Some(Some(plane)) = planes.get(ch.comp) {
                let xs_bytes = w as u64 * 4;
                ctx.take_memory(xs_bytes)?;
                let xs = columns(plane, w);
                let mut tmp = Vec::new();
                for (y, dst) in a.chunks_exact_mut(w.max(1)).enumerate() {
                    let sy = ((y as u64 * plane.h as u64) / (h as u64).max(1)).min(plane.h.saturating_sub(1) as u64) as usize;
                    let Some(row) = plane.row(sy, &mut tmp) else { continue };
                    for (d, &sx) in dst.iter_mut().zip(&xs) {
                        if let Some(&v) = row.get(sx as usize) {
                            *d = lut.get(usize::from(v)).copied().unwrap_or(255);
                        }
                    }
                }
                ctx.give_back(xs_bytes);
            }
            if opts.alpha == Alpha::Premultiplied {
                for (px, &al) in data.chunks_exact_mut(ncol).zip(&a) {
                    if al != 0 && al != 255 {
                        for c in px {
                            *c = ((u32::from(*c) * 255 + u32::from(al) / 2) / u32::from(al)).min(255) as u8;
                        }
                    }
                }
            }
            Some(a)
        }
        None => None,
    };
    let table_bytes: u64 = tables.iter().map(|t| t.1.len() as u64).sum();
    ctx.give_back(table_bytes);
    Ok(Decoded { w, h, ncomp: ncol, data, alpha, full: (0, 0), warning: None, #[cfg(test)] memory_left: ctx.memory_left(), #[cfg(test)] memory_low: ctx.memory_low() })
}
