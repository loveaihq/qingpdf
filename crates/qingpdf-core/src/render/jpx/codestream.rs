//! The codestream's markers (ISO/IEC 15444-1 Annex A): the main header, and the tile-parts found by their SOT markers
//! with the parameters their headers add.

use std::collections::HashMap;

use super::{Ctx, Warnings, bad};
use crate::error::{Error, Result};
use crate::render::work::cost;

/// Most components (the standard allows 16384; a page's picture has 1 to 4).
pub(super) const MAX_COMPONENTS: usize = 256;
/// Widest and tallest image area, and the most pixels of the whole area (the area is only ever decoded at a size that
/// fits the memory cap, but it is walked in tiles).
const MAX_SIDE: u64 = 1 << 24;
const MAX_AREA: u64 = 1 << 36;
/// Most tiles (the SOT marker has 16 bits for the number) and tile-parts.
const MAX_TILES: u64 = 65_535;
const MAX_TILE_PARTS: usize = 1 << 17;
/// Most progression order changes of the main header and of a tile together.
const MAX_POC: usize = 1 << 12;
/// Most bits for a sample (the standard allows 38).
pub(super) const MAX_DEPTH: u8 = 16;

pub(super) struct Comp {
    pub depth: u8,
    pub dx: u32,
    pub dy: u32,
}

pub(super) struct Siz {
    pub x1: u32,
    pub y1: u32,
    pub x0: u32,
    pub y0: u32,
    pub tw: u32,
    pub th: u32,
    pub tx0: u32,
    pub ty0: u32,
    pub comps: Vec<Comp>,
}

impl Siz {
    pub fn tiles_across(&self) -> u64 {
        (u64::from(self.x1) - u64::from(self.tx0)).div_ceil(u64::from(self.tw))
    }

    pub fn tiles_down(&self) -> u64 {
        (u64::from(self.y1) - u64::from(self.ty0)).div_ceil(u64::from(self.th))
    }

    /// The tile `t`'s rectangle on the reference grid (B.3, equations B-5 to B-8).
    pub fn tile_rect(&self, t: u64) -> [u32; 4] {
        let across = self.tiles_across().max(1);
        let (p, q) = (t % across, t / across);
        let x0 = (u64::from(self.tx0) + p * u64::from(self.tw)).max(u64::from(self.x0));
        let y0 = (u64::from(self.ty0) + q * u64::from(self.th)).max(u64::from(self.y0));
        let x1 = (u64::from(self.tx0) + (p + 1) * u64::from(self.tw)).min(u64::from(self.x1));
        let y1 = (u64::from(self.ty0) + (q + 1) * u64::from(self.th)).min(u64::from(self.y1));
        [x0 as u32, y0 as u32, x1 as u32, y1 as u32]
    }
}

/// How a component is transformed and cut into code-blocks (COD, COC).
#[derive(Clone)]
pub(super) struct Style {
    pub levels: u8,
    pub xcb: u8,
    pub ycb: u8,
    pub cbstyle: u8,
    pub reversible: bool,
    /// Per resolution: the precinct width exponent in the low nibble, the height exponent in the high one.
    pub precincts: Vec<u8>,
}

#[derive(Clone)]
pub(super) struct Cod {
    pub prog: u8,
    pub layers: u16,
    pub mct: u8,
    pub style: Style,
}

/// Quantization (QCD, QCC): the style (0 none, 1 derived, 2 expounded), the guard bits, the exponent and mantissa of
/// each subband (one only for the derived style).
#[derive(Clone)]
pub(super) struct Quant {
    pub style: u8,
    pub guard: u8,
    pub steps: Vec<(u8, u16)>,
}

impl Quant {
    /// The exponent and mantissa of subband `index` (0 is the lowest LL; resolution `r` has 3r-2 to 3r) of a
    /// component with `levels` decompositions (E.1.1, E-5).
    pub fn step(&self, index: usize, levels: usize) -> Option<(u8, u16)> {
        if self.style == 1 {
            let (e0, m0) = *self.steps.first()?;
            let r = if index == 0 { 0 } else { index.div_ceil(3) };
            let drop = if r == 0 { 0 } else { r - 1 };
            let _ = levels;
            Some((e0.saturating_sub(drop.min(255) as u8), m0))
        } else {
            self.steps.get(index).copied()
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Poc {
    pub rs: u8,
    pub cs: u16,
    pub lye: u16,
    pub re: u16,
    pub ce: u16,
    pub prog: u8,
}

/// The coding parameters of the main header or of one tile; a tile's override the main header's.
#[derive(Default)]
pub(super) struct Coding {
    pub cod: Option<Cod>,
    pub coc: HashMap<usize, Style>,
    pub qcd: Option<Quant>,
    pub qcc: HashMap<usize, Quant>,
    /// Region of interest shift (RGN), by component.
    pub rgn: HashMap<usize, u8>,
    pub poc: Vec<Poc>,
}

/// A tile-part: where its data is, and where its packet headers are if they are packed elsewhere.
pub(super) struct Part {
    pub body: (usize, usize),
    /// The tile-part's share of the main header's packed packet headers (PPM), as a range of [`Header::ppm`].
    pub ppm: Option<(usize, usize)>,
}

#[derive(Default)]
pub(super) struct Tile {
    pub coding: Coding,
    pub parts: Vec<Part>,
    /// Packed packet headers of the tile (PPT): index of the marker, range in the codestream.
    pub ppt: Vec<(u8, usize, usize)>,
}

pub(super) struct Header {
    pub siz: Siz,
    pub main: Coding,
    pub ppm: Vec<u8>,
    pub tiles: HashMap<u64, Tile>,
    pub warnings: Warnings,
}

fn be16(d: &[u8], at: usize) -> Option<u16> {
    let b: [u8; 2] = d.get(at..at.checked_add(2)?)?.try_into().ok()?;
    Some(u16::from_be_bytes(b))
}

fn be32(d: &[u8], at: usize) -> Option<u32> {
    let b: [u8; 4] = d.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_be_bytes(b))
}

fn short() -> Error {
    bad("a marker segment ends too soon")
}

fn parse_siz(b: &[u8]) -> Result<Siz> {
    let get = |at: usize| be32(b, at).ok_or_else(short);
    let (x1, y1, x0, y0) = (get(2)?, get(6)?, get(10)?, get(14)?);
    let (tw, th, tx0, ty0) = (get(18)?, get(22)?, get(26)?, get(30)?);
    let n = usize::from(be16(b, 34).ok_or_else(short)?);
    if n == 0 {
        return Err(bad("an image without components"));
    }
    if n > MAX_COMPONENTS {
        return Err(Error::Limit(format!("JPX: {n} components")));
    }
    if x0 >= x1 || y0 >= y1 || tw == 0 || th == 0 || tx0 > x0 || ty0 > y0 || u64::from(tx0) + u64::from(tw) <= u64::from(x0) || u64::from(ty0) + u64::from(th) <= u64::from(y0) {
        return Err(bad("an image area that does not add up"));
    }
    if u64::from(x1 - x0) > MAX_SIDE || u64::from(y1 - y0) > MAX_SIDE || u64::from(x1 - x0) * u64::from(y1 - y0) > MAX_AREA {
        return Err(Error::Limit("JPX: image too large".to_string()));
    }
    let mut comps = Vec::with_capacity(n);
    for i in 0..n {
        let at = 36 + i * 3;
        let s = *b.get(at).ok_or_else(short)?;
        let dx = u32::from(*b.get(at + 1).ok_or_else(short)?);
        let dy = u32::from(*b.get(at + 2).ok_or_else(short)?);
        let depth = (s & 0x7F) + 1;
        if dx == 0 || dy == 0 {
            return Err(bad("a component with no sampling step"));
        }
        if depth > MAX_DEPTH {
            return Err(Error::Unsupported(format!("JPX: {depth} bits per sample")));
        }
        comps.push(Comp { depth, dx, dy });
    }
    let siz = Siz { x1, y1, x0, y0, tw, th, tx0, ty0, comps };
    if siz.tiles_across().saturating_mul(siz.tiles_down()) > MAX_TILES {
        return Err(Error::Limit("JPX: too many tiles".to_string()));
    }
    Ok(siz)
}

/// SPcod / SPcoc: the five parameters and the precinct sizes (A.6.1, A.6.2).
fn parse_style(b: &[u8], precincts_given: bool) -> Result<Style> {
    let levels = *b.first().ok_or_else(short)?;
    let xcb = b.get(1).ok_or_else(short)? & 15;
    let ycb = b.get(2).ok_or_else(short)? & 15;
    let cbstyle = *b.get(3).ok_or_else(short)?;
    let transform = *b.get(4).ok_or_else(short)?;
    if levels > 32 {
        return Err(bad("more than 32 decomposition levels"));
    }
    let (xcb, ycb) = (xcb + 2, ycb + 2);
    if xcb > 10 || ycb > 10 || xcb + ycb > 12 {
        return Err(bad("a code-block size that is not allowed"));
    }
    if cbstyle & 0xC0 != 0 {
        return Err(Error::Unsupported("JPX: high-throughput code-blocks".to_string()));
    }
    if transform > 1 {
        return Err(Error::Unsupported("JPX: a wavelet transform of Part 2".to_string()));
    }
    let n = usize::from(levels) + 1;
    let mut precincts = Vec::with_capacity(n);
    for r in 0..n {
        let v = if precincts_given { *b.get(5 + r).ok_or_else(short)? } else { 0xFF };
        if r > 0 && (v & 15 == 0 || v >> 4 == 0) {
            return Err(bad("a precinct of size 1 above the lowest resolution"));
        }
        precincts.push(v);
    }
    Ok(Style { levels, xcb, ycb, cbstyle, reversible: transform == 1, precincts })
}

fn parse_cod(b: &[u8]) -> Result<Cod> {
    let scod = *b.first().ok_or_else(short)?;
    let prog = *b.get(1).ok_or_else(short)?;
    let layers = be16(b, 2).ok_or_else(short)?;
    let mct = *b.get(4).ok_or_else(short)?;
    if prog > 4 {
        return Err(bad("a progression order that does not exist"));
    }
    if layers == 0 {
        return Err(bad("no layers"));
    }
    let style = parse_style(b.get(5..).ok_or_else(short)?, scod & 1 != 0)?;
    Ok(Cod { prog, layers, mct, style })
}

fn comp_index(b: &[u8], ncomp: usize) -> Result<(usize, usize)> {
    if ncomp < 257 {
        Ok((usize::from(*b.first().ok_or_else(short)?), 1))
    } else {
        Ok((usize::from(be16(b, 0).ok_or_else(short)?), 2))
    }
}

fn parse_quant(b: &[u8]) -> Result<Quant> {
    let s = *b.first().ok_or_else(short)?;
    let (style, guard) = (s & 0x1F, s >> 5);
    let rest = b.get(1..).ok_or_else(short)?;
    let steps: Vec<(u8, u16)> = match style {
        0 => rest.iter().map(|&v| (v >> 3, 0)).collect(),
        1 | 2 => rest.chunks_exact(2).map(|c| c.first().zip(c.get(1)).map_or((0, 0), |(&a, &b)| { let v = u16::from_be_bytes([a, b]); ((v >> 11) as u8, v & 0x7FF) })).collect(),
        _ => return Err(bad("a quantization style that does not exist")),
    };
    if steps.is_empty() {
        return Err(short());
    }
    Ok(Quant { style, guard, steps })
}

fn parse_poc(b: &[u8], ncomp: usize) -> Result<Vec<Poc>> {
    let wide = ncomp >= 257;
    let size = if wide { 9 } else { 7 };
    let mut out = Vec::new();
    for e in b.chunks_exact(size) {
        let mut at = 0usize;
        let mut take = |n: usize| -> Option<u16> {
            let v = e.get(at..at + n)?.iter().fold(0u16, |a, &b| (a << 8) | u16::from(b));
            at += n;
            Some(v)
        };
        let (Some(rs), Some(cs), Some(lye), Some(re), Some(ce), Some(prog)) = (take(1), take(if wide { 2 } else { 1 }), take(2), take(1), take(if wide { 2 } else { 1 }), take(1)) else {
            return Err(short());
        };
        if prog > 4 {
            return Err(bad("a progression order that does not exist"));
        }
        out.push(Poc { rs: rs as u8, cs, lye, re, ce: if ce == 0 { 256 } else { ce }, prog: prog as u8 });
    }
    Ok(out)
}

impl Coding {
    /// Take in a marker segment that sets coding parameters. `Ok(false)`: it is not one of those.
    fn marker(&mut self, marker: u16, b: &[u8], ncomp: usize) -> Result<bool> {
        match marker {
            0xFF52 => self.cod = Some(parse_cod(b)?),
            0xFF53 => {
                let (c, n) = comp_index(b, ncomp)?;
                let scoc = *b.get(n).ok_or_else(short)?;
                let style = parse_style(b.get(n + 1..).ok_or_else(short)?, scoc & 1 != 0)?;
                self.coc.insert(c, style);
            }
            0xFF5C => self.qcd = Some(parse_quant(b)?),
            0xFF5D => {
                let (c, n) = comp_index(b, ncomp)?;
                self.qcc.insert(c, parse_quant(b.get(n..).ok_or_else(short)?)?);
            }
            0xFF5E => {
                let (c, n) = comp_index(b, ncomp)?;
                let shift = *b.get(n + 1).ok_or_else(short)?;
                self.rgn.insert(c, shift);
            }
            0xFF5F => {
                let more = parse_poc(b, ncomp)?;
                if self.poc.len() + more.len() > MAX_POC {
                    return Err(Error::Limit("JPX: too many progression order changes".to_string()));
                }
                self.poc.extend(more);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}

/// What the parsed form of a marker segment of `len` bytes takes, at most (a quantization step is 4 bytes for each byte of
/// the marker that holds it): the coding parameters of every tile are kept until the decode is done.
fn parsed_bytes(len: usize) -> u64 {
    len as u64 * 4 + 128
}

/// Read the main header and find the tile-parts.
pub(super) fn parse(ctx: &Ctx, data: &[u8]) -> Result<Header> {
    if be16(data, 0) != Some(0xFF4F) || be16(data, 2) != Some(0xFF51) {
        return Err(bad("not a codestream (no SOC and SIZ)"));
    }
    let siz_len = usize::from(be16(data, 4).ok_or_else(short)?);
    let siz = parse_siz(data.get(6..4 + siz_len).ok_or_else(short)?)?;
    let ncomp = siz.comps.len();
    let mut main = Coding::default();
    let mut warnings = Warnings::default();
    let mut ppm_parts: Vec<(u8, usize, usize)> = Vec::new();
    let mut pos = 4 + siz_len;
    // The main header, up to the first SOT.
    loop {
        let marker = be16(data, pos).ok_or_else(|| bad("the main header ends too soon"))?;
        if marker == 0xFF90 || marker == 0xFFD9 {
            break;
        }
        if marker < 0xFF00 {
            return Err(bad("garbage in the main header"));
        }
        if (0xFF30..=0xFF3F).contains(&marker) {
            pos += 2;
            continue;
        }
        let len = usize::from(be16(data, pos + 2).ok_or_else(short)?);
        if len < 2 {
            return Err(bad("a marker segment shorter than its length field"));
        }
        let body = data.get(pos + 4..pos + 2 + len).ok_or_else(short)?;
        ctx.charge(len as f64 * cost::JPX_HEADER_BYTE + cost::JPX_MARKER)?;
        if marker == 0xFF60 {
            let z = *body.first().ok_or_else(short)?;
            ppm_parts.push((z, pos + 5, pos + 2 + len));
        } else {
            if main.marker(marker, body, ncomp)? {
                ctx.take_memory(parsed_bytes(len))?;
            }
        }
        pos += 2 + len;
    }
    // Packed packet headers: the pieces in order of their index, joined.
    ppm_parts.sort_by_key(|p| p.0);
    let mut ppm = Vec::new();
    for (_, a, b) in &ppm_parts {
        ppm.extend_from_slice(data.get(*a..*b).unwrap_or(&[]));
    }
    ctx.take_memory(ppm.len() as u64)?;
    let mut ppm_pos = 0usize;
    let mut tiles: HashMap<u64, Tile> = HashMap::new();
    let ntiles = siz.tiles_across().saturating_mul(siz.tiles_down());
    let mut count = 0usize;
    // The tile-parts.
    while be16(data, pos) == Some(0xFF90) {
        ctx.charge(cost::JPX_TILE_PART)?;
        count += 1;
        if count > MAX_TILE_PARTS {
            return Err(Error::Limit("JPX: too many tile-parts".to_string()));
        }
        let (Some(lsot), Some(isot), Some(psot)) = (be16(data, pos + 2), be16(data, pos + 4), be32(data, pos + 6)) else {
            warnings.push("the codestream ends inside a tile-part header".to_string());
            break;
        };
        if lsot != 10 {
            warnings.push("a tile-part header of the wrong length".to_string());
            break;
        }
        let mut end = if psot == 0 {
            let mut e = data.len();
            if e >= 2 && be16(data, e - 2) == Some(0xFFD9) {
                e -= 2;
            }
            e
        } else {
            (pos + psot as usize).min(data.len())
        };
        if psot != 0 && (psot < 14 || pos + 14 > data.len()) {
            warnings.push("a tile-part shorter than its header".to_string());
            break;
        }
        end = end.max(pos + 12);
        let t = u64::from(isot);
        // Its packed headers, if the main header packed them.
        let mut ppm_chunk = None;
        if !ppm.is_empty() {
            if let Some(n) = be32(&ppm, ppm_pos) {
                let a = ppm_pos + 4;
                let b = a.saturating_add(n as usize).min(ppm.len());
                ppm_chunk = Some((a.min(b), b));
                ppm_pos = b;
            } else {
                ppm_pos = ppm.len();
            }
        }
        // The tile-part header, up to SOD.
        let mut p = pos + 12;
        let mut tile_coding = Coding::default();
        let mut tile_ppt = Vec::new();
        let mut body_start = None;
        while p + 2 <= end {
            let Some(marker) = be16(data, p) else { break };
            if marker == 0xFF93 {
                body_start = Some(p + 2);
                break;
            }
            let len = usize::from(be16(data, p + 2).unwrap_or(0));
            let Some(body) = data.get(p + 4..p + 2 + len).filter(|_| len >= 2) else { break };
            ctx.charge(len as f64 * cost::JPX_HEADER_BYTE + cost::JPX_MARKER)?;
            if marker == 0xFF61 {
                if let Some(&z) = body.first() {
                    tile_ppt.push((z, p + 5, p + 2 + len));
                }
            } else {
                match tile_coding.marker(marker, body, ncomp) {
                    Ok(true) => ctx.take_memory(parsed_bytes(len))?,
                    Ok(false) => {}
                    Err(e @ Error::Limit(_)) => return Err(e),
                    Err(other) => warnings.push(format!("a tile-part header is damaged ({other})")),
                }
            }
            p += 2 + len;
        }
        if t < ntiles {
            let Some(body_start) = body_start else {
                warnings.push("a tile-part without data".to_string());
                pos = end;
                continue;
            };
            if !tiles.contains_key(&t) {
                ctx.take_memory(std::mem::size_of::<Tile>() as u64 + 64)?;
            }
            let tile = tiles.entry(t).or_default();
            merge(&mut tile.coding, tile_coding);
            if tile.coding.poc.len() > MAX_POC {
                return Err(Error::Limit("JPX: too many progression order changes".to_string()));
            }
            tile.ppt.extend(tile_ppt);
            ctx.take_memory(std::mem::size_of::<Part>() as u64)?;
            tile.parts.push(Part { body: (body_start.min(end), end), ppm: ppm_chunk });
        } else {
            warnings.push("a tile-part of a tile that does not exist".to_string());
        }
        if end <= pos {
            break;
        }
        pos = end;
    }
    if tiles.is_empty() {
        return Err(bad("the codestream has no tile data"));
    }
    Ok(Header { siz, main, ppm, tiles, warnings })
}

/// Add the parameters a tile-part header set to the tile's.
fn merge(into: &mut Coding, from: Coding) {
    if from.cod.is_some() {
        into.cod = from.cod;
    }
    into.coc.extend(from.coc);
    if from.qcd.is_some() {
        into.qcd = from.qcd;
    }
    into.qcc.extend(from.qcc);
    into.rgn.extend(from.rgn);
    into.poc.extend(from.poc);
}
