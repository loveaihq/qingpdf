//! Tier 2 of JPEG 2000: a tile's components, resolutions, subbands, precincts and code-blocks (ISO/IEC 15444-1 Annex B),
//! and the packets that carry the code-blocks' bytes (B.9 to B.12): their headers, the five progression orders and
//! progression order changes. What this builds is a list of chunks of the codestream for every code-block; [`super::recon`]
//! decodes them.
//!
//! Everything sized from a header value is checked against a cap with checked arithmetic before it is allocated, and
//! the page's work meter is charged for every step, including packets that turn out empty or are not wanted.

use super::codestream::{Comp, Header, Poc, Quant, Style};
use super::t1;
use super::tagtree::{self, Bits, Node};
use super::{Ctx, bad};
use crate::error::{Error, Result};
use crate::render::work::cost;

/// Most precincts, code-blocks and packets of one tile.
const MAX_PRECINCTS: u64 = 1 << 19;
const MAX_BLOCKS: u64 = 1 << 19;
const MAX_PACKETS: u64 = 1 << 22;
/// Most coding passes of one code-block (the standard's Mb is at most 37: 109 passes).
const MAX_PASSES: u32 = 128;
const NONE: u32 = u32::MAX;

pub(super) fn ceil_shift(a: u64, s: u32) -> u64 {
    if s >= 63 { u64::from(a > 0) } else { (a + (1u64 << s) - 1) >> s }
}

pub(super) struct Band {
    /// 0 LL, 1 HL, 2 LH, 3 HH.
    pub orient: u8,
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
    /// Where the band's samples are in the tile-component's buffer (the layout of [`super::dwt`]).
    pub x_off: usize,
    pub y_off: usize,
    /// Bit-planes of a coefficient (with the region of interest shift), and the quantization step.
    pub planes: u32,
    pub step: f32,
}

pub(super) struct Res {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
    pub npw: u32,
    pub nph: u32,
    pub bands: Vec<Band>,
    /// The index in [`TileComp::pbands`] of the first precinct's first subband, and of the precinct in the tile's
    /// list of all precincts.
    pub first_pband: usize,
    pub pbase: usize,
    /// Is this resolution decoded? (Wanted, and not above the reduction.)
    pub keep: bool,
}

/// What one precinct has of one subband: a grid of code-blocks and the two tag trees over it.
pub(super) struct PBand {
    pub ncw: u32,
    pub nch: u32,
    pub first_block: u32,
    /// Offset in [`TileComp::nodes`] of the inclusion tree; the zero bit-plane tree follows it.
    pub nodes: u32,
    pub nn: u32,
}

pub(super) struct Block {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
    /// The first and last of its chunks, in the tile's list.
    pub head: u32,
    pub tail: u32,
    pub passes: u16,
    pub zbp: u8,
    pub lblock: u8,
    pub included: bool,
    /// The codeword segment being filled and the passes in it.
    pub nseg: u8,
    pub seg_passes: u8,
}

/// A piece of a code-block's bytes: a range of the codestream, from one packet, belonging to one codeword segment.
pub(super) struct Chunk {
    pub off: u32,
    pub len: u32,
    pub next: u32,
    pub seg: u8,
    pub passes: u8,
}

pub(super) struct TileComp {
    pub dx: u32,
    pub dy: u32,
    pub style: Style,
    pub depth: u8,
    pub roi: u8,
    /// Is this component decoded at all? And the highest resolution decoded.
    pub need: bool,
    pub keep: usize,
    pub res: Vec<Res>,
    pub pbands: Vec<PBand>,
    pub blocks: Vec<Block>,
    pub nodes: Vec<Node>,
}

/// The parameters that are the tile's, not a component's.
pub(super) struct TileParams {
    pub layers: u16,
}

pub(super) struct Part<'a> {
    pub body: &'a [u8],
    /// Where the body is in the codestream.
    pub body_off: usize,
    /// The packed packet headers of this tile-part, if there are any.
    pub hdr: Option<Vec<u8>>,
    pub bpos: usize,
    pub hpos: usize,
}

pub(super) struct TileDec<'a> {
    pub ctx: &'a Ctx<'a>,
    pub params: TileParams,
    pub comps: Vec<TileComp>,
    pub chunks: Vec<Chunk>,
    pub parts: Vec<Part<'a>>,
    cur: usize,
    next_layer: Vec<u16>,
    /// The tile's packed packet headers (PPT) and how far they are read.
    pub ppt: Option<Vec<u8>>,
    ppt_pos: usize,
    /// The tile's rectangle on the reference grid.
    pub rect: [u32; 4],
    /// Memory taken for this tile, to give back.
    pub held: u64,
    /// The packets have stopped (the data ended, or is damaged).
    pub done: bool,
    pub damaged: Option<String>,
    /// Scratch for a packet: the code-blocks named in the header and what they bring.
    pending: Vec<(u32, u8, u8, u32)>,
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Flow {
    More,
    End,
}

/// The caps a tile's structures are counted against while they are built.
#[derive(Default)]
struct Totals {
    precincts: u64,
    blocks: u64,
}

impl<'a> TileDec<'a> {
    pub fn new(ctx: &'a Ctx<'a>, params: TileParams, rect: [u32; 4]) -> TileDec<'a> {
        TileDec { ctx, params, rect, comps: Vec::new(), chunks: Vec::new(), parts: Vec::new(), cur: 0, next_layer: Vec::new(), ppt: None, ppt_pos: 0, held: 0, done: false, damaged: None, pending: Vec::new() }
    }

    pub fn take(&mut self, bytes: u64) -> Result<()> {
        self.ctx.take_memory(bytes)?;
        self.held += bytes;
        Ok(())
    }

    /// Build the structure of one component of the tile. `reduce`: the resolution levels left out.
    pub fn add_component(&mut self, c: &Comp, style: &Style, quant: &Quant, roi: u8, need: bool, reduce: usize) -> Result<()> {
        let ctx = self.ctx;
        ctx.charge(cost::JPX_TILE_COMP)?;
        let (dx, dy) = (u64::from(c.dx), u64::from(c.dy));
        let [tx0, ty0, tx1, ty1] = self.rect.map(u64::from);
        let (x0, y0, x1, y1) = (tx0.div_ceil(dx), ty0.div_ceil(dy), tx1.div_ceil(dx), ty1.div_ceil(dy));
        let levels = usize::from(style.levels);
        let nres = levels + 1;
        if need && levels < reduce {
            return Err(bad("a tile with fewer decomposition levels than are left out"));
        }
        let keep = if need { levels - reduce } else { 0 };
        let mut comp = TileComp {
            dx: c.dx,
            dy: c.dy,
            style: style.clone(),
            depth: c.depth,
            roi,
            need,
            keep,
            res: Vec::new(),
            pbands: Vec::new(),
            blocks: Vec::new(),
            nodes: Vec::new(),
        };
        let mut totals = Totals::default();
        for tc in &self.comps {
            for r in &tc.res {
                totals.precincts += u64::from(r.npw) * u64::from(r.nph);
            }
            totals.blocks += tc.blocks.len() as u64;
        }
        let mut pbase = self.next_layer.len();
        let mut prev_dims = (0usize, 0usize);
        for r in 0..nres {
            ctx.charge(cost::JPX_RES)?;
            let d = (levels - r) as u32;
            let (rx0, ry0, rx1, ry1) = (ceil_shift(x0, d), ceil_shift(y0, d), ceil_shift(x1, d), ceil_shift(y1, d));
            let pp = style.precincts.get(r).copied().unwrap_or(0xFF);
            let (ppx, ppy) = (u32::from(pp & 15), u32::from(pp >> 4));
            let npw = if rx1 > rx0 { ceil_shift(rx1, ppx) - (rx0 >> ppx) } else { 0 };
            let nph = if ry1 > ry0 { ceil_shift(ry1, ppy) - (ry0 >> ppy) } else { 0 };
            let np = npw.checked_mul(nph).ok_or_else(|| Error::Limit("JPX: too many precincts".to_string()))?;
            totals.precincts = totals.precincts.saturating_add(np);
            if totals.precincts > MAX_PRECINCTS || totals.precincts.saturating_mul(u64::from(self.params.layers)) > MAX_PACKETS {
                return Err(Error::Limit("JPX: too many precincts or packets".to_string()));
            }
            let res_keep = need && r <= keep;
            let (w, h) = ((rx1 - rx0) as usize, (ry1 - ry0) as usize);
            // The bands.
            let mut bands = Vec::new();
            let band_kinds: &[u8] = if r == 0 { &[0] } else { &[1, 2, 3] };
            for (bi, &orient) in band_kinds.iter().enumerate() {
                let nb = if r == 0 { levels as u32 } else { (levels - r + 1) as u32 };
                let (xob, yob) = (u64::from(orient & 1), u64::from(orient >> 1));
                let edge = |v: u64, ob: u64| -> u64 {
                    if nb == 0 {
                        return v;
                    }
                    let t = v as i64 - ((ob as i64) << (nb - 1));
                    ((t + (1i64 << nb) - 1) >> nb).max(0) as u64
                };
                let (bx0, by0, bx1, by1) = (edge(x0, xob), edge(y0, yob), edge(x1, xob), edge(y1, yob));
                let index = if r == 0 { 0 } else { 3 * (r - 1) + 1 + bi };
                let (eps, mu) = quant.step(index, levels).ok_or_else(|| bad("the quantization has too few steps"))?;
                let gain = match orient {
                    0 => 0,
                    3 => 2,
                    _ => 1,
                };
                let planes = (u32::from(quant.guard) + u32::from(eps)).saturating_sub(1) + u32::from(roi);
                if res_keep && planes > 30 {
                    return Err(bad("more bit-planes than a coefficient can have"));
                }
                let rb = i32::from(c.depth) + gain;
                let step = 2f32.powi(rb - i32::from(eps)) * (1.0 + f32::from(mu) / 2048.0);
                let (x_off, y_off) = (if orient & 1 != 0 { prev_dims.0 } else { 0 }, if orient & 2 != 0 { prev_dims.1 } else { 0 });
                bands.push(Band { orient, x0: bx0 as u32, y0: by0 as u32, x1: bx1 as u32, y1: by1 as u32, x_off, y_off, planes: planes.min(30), step });
            }
            let first_pband = comp.pbands.len();
            // The precincts and their code-blocks.
            for py in 0..nph {
                for px in 0..npw {
                    ctx.charge(cost::JPX_PRECINCT)?;
                    for band in &bands {
                        ctx.charge(cost::JPX_PBAND)?;
                        let shift = u32::from(r > 0);
                        let (px0, py0) = ((((rx0 >> ppx) + px) << ppx) >> shift, (((ry0 >> ppy) + py) << ppy) >> shift);
                        let (px1, py1) = (px0 + (1u64 << (ppx - shift)), py0 + (1u64 << (ppy - shift)));
                        let (cx0, cy0) = (px0.max(u64::from(band.x0)), py0.max(u64::from(band.y0)));
                        let (cx1, cy1) = (px1.min(u64::from(band.x1)), py1.min(u64::from(band.y1)));
                        let (xcb, ycb) = (u32::from(style.xcb).min(ppx - shift), u32::from(style.ycb).min(ppy - shift));
                        let (ncw, nch) = if cx1 > cx0 && cy1 > cy0 { (ceil_shift(cx1, xcb) - (cx0 >> xcb), ceil_shift(cy1, ycb) - (cy0 >> ycb)) } else { (0, 0) };
                        let n = ncw * nch;
                        totals.blocks = totals.blocks.saturating_add(n);
                        if totals.blocks > MAX_BLOCKS {
                            return Err(Error::Limit("JPX: too many code-blocks".to_string()));
                        }
                        let nn = tagtree::node_count(ncw, nch);
                        self.take(n * std::mem::size_of::<Block>() as u64 + nn * 2 * std::mem::size_of::<Node>() as u64 + std::mem::size_of::<PBand>() as u64)?;
                        ctx.charge(n as f64 * cost::JPX_BLOCK_INIT + nn as f64 * 2.0 * cost::JPX_TREE_NODE)?;
                        let first_block = comp.blocks.len() as u32;
                        let nodes = comp.nodes.len() as u32;
                        comp.pbands.push(PBand { ncw: ncw as u32, nch: nch as u32, first_block, nodes, nn: nn as u32 });
                        comp.nodes.resize(comp.nodes.len() + 2 * nn as usize, Node::FRESH);
                        for j in 0..nch {
                            for i in 0..ncw {
                                let bx = (cx0 >> xcb) + i;
                                let by = (cy0 >> ycb) + j;
                                comp.blocks.push(Block {
                                    x0: (bx << xcb).max(cx0) as u32,
                                    y0: (by << ycb).max(cy0) as u32,
                                    x1: ((bx + 1) << xcb).min(cx1) as u32,
                                    y1: ((by + 1) << ycb).min(cy1) as u32,
                                    head: NONE,
                                    tail: NONE,
                                    passes: 0,
                                    zbp: 0,
                                    lblock: 3,
                                    included: false,
                                    nseg: 0,
                                    seg_passes: 0,
                                });
                            }
                        }
                    }
                }
            }
            self.take(np * 2 + 256)?;
            comp.res.push(Res { x0: rx0 as u32, y0: ry0 as u32, x1: rx1 as u32, y1: ry1 as u32, npw: npw as u32, nph: nph as u32, bands, first_pband, pbase, keep: res_keep });
            pbase += np as usize;
            prev_dims = (w, h);
        }
        self.next_layer.resize(pbase, 0);
        self.comps.push(comp);
        Ok(())
    }

    /// Read the packets of the tile in the progression order(s), filling in the chunks of its code-blocks.
    pub fn read_packets(&mut self, main_poc: &[Poc], tile_poc: &[Poc], default_prog: u8) -> Result<()> {
        let ncomp = self.comps.len();
        let max_res = self.comps.iter().map(|c| c.res.len()).max().unwrap_or(0);
        let layers = usize::from(self.params.layers);
        let mut entries: Vec<Poc> = if !tile_poc.is_empty() {
            tile_poc.to_vec()
        } else if !main_poc.is_empty() {
            main_poc.to_vec()
        } else {
            vec![Poc { rs: 0, cs: 0, lye: self.params.layers, re: max_res as u16, ce: ncomp as u16, prog: default_prog }]
        };
        // Whatever an entry says, it only reaches what there is.
        for e in &mut entries {
            e.lye = e.lye.min(layers as u16);
            e.re = e.re.min(max_res as u16);
            e.ce = e.ce.min(ncomp as u16);
        }
        for e in &entries {
            self.ctx.charge(cost::JPX_PACKET)?;
            if self.run(e)? == Flow::End {
                break;
            }
        }
        Ok(())
    }

    /// The packets of one progression order change (or of the tile's one order).
    fn run(&mut self, e: &Poc) -> Result<Flow> {
        let (rs, re, cs, ce, lye) = (usize::from(e.rs), usize::from(e.re), usize::from(e.cs), usize::from(e.ce), usize::from(e.lye));
        match e.prog {
            0 => {
                // (Every layer and every resolution of it is charged, even when nothing is in the range of components:
                // the loops are as long as the layers and resolutions the entry names, whatever is in them.)
                for l in 0..lye {
                    self.ctx.charge(cost::JPX_PACKET)?;
                    for r in rs..re {
                        self.ctx.charge(cost::JPX_PACKET)?;
                        for c in cs..ce {
                            if self.visit_precincts(c, r, l)? == Flow::End {
                                return Ok(Flow::End);
                            }
                        }
                    }
                }
            }
            1 => {
                for r in rs..re {
                    self.ctx.charge(cost::JPX_PACKET)?;
                    for l in 0..lye {
                        self.ctx.charge(cost::JPX_PACKET)?;
                        for c in cs..ce {
                            if self.visit_precincts(c, r, l)? == Flow::End {
                                return Ok(Flow::End);
                            }
                        }
                    }
                }
            }
            prog => {
                // The position-based orders: every precinct of the range, with its place on the reference grid,
                // sorted by the order's key; then the precinct's layers.
                let mut list: Vec<(u128, u32)> = Vec::new();
                let mut taken = 0u64;
                for c in cs..ce {
                    let Some(comp) = self.comps.get(c) else { continue };
                    for r in rs..re.min(comp.res.len()) {
                        let Some(res) = comp.res.get(r) else { continue };
                        let np = u64::from(res.npw) * u64::from(res.nph);
                        self.ctx.charge(np as f64 * cost::JPX_ORDER + cost::JPX_PACKET)?;
                        self.ctx.take_memory(np * 32)?;
                        taken += np * 32;
                        let pp = comp.style.precincts.get(r).copied().unwrap_or(0xFF);
                        let (ppx, ppy) = (u32::from(pp & 15), u32::from(pp >> 4));
                        let d = (comp.res.len() - 1 - r) as u32;
                        let (tx0, ty0) = (u64::from(self.rect[0]), u64::from(self.rect[1]));
                        let pos = |first: u64, p: u64, pp: u32, t0: u64, step: u64| -> u64 {
                            if p == 0 && first & ((1u64 << pp) - 1) != 0 { t0 } else { (((first >> pp) + p) << pp).saturating_mul(1u64 << d).saturating_mul(step) }
                        };
                        for k in 0..np {
                            let (px, py) = (k % u64::from(res.npw), k / u64::from(res.npw));
                            let x = u128::from(pos(u64::from(res.x0), px, ppx, tx0, u64::from(comp.dx)).min(u64::from(u32::MAX)));
                            let y = u128::from(pos(u64::from(res.y0), py, ppy, ty0, u64::from(comp.dy)).min(u64::from(u32::MAX)));
                            let (cc, rr, kk) = (c as u128, r as u128, u128::from(k));
                            let key = match prog {
                                2 => (rr << 112) | (y << 80) | (x << 48) | (cc << 32) | kk,
                                3 => (y << 88) | (x << 56) | (cc << 40) | (rr << 32) | kk,
                                _ => (cc << 104) | (y << 72) | (x << 40) | (rr << 32) | kk,
                            };
                            list.push((key, ((c as u32) << 8) | r as u32));
                        }
                    }
                }
                list.sort_unstable_by_key(|e| e.0);
                let mut flow = Flow::More;
                'all: for (key, cr) in &list {
                    let (c, r, k) = ((cr >> 8) as usize, (cr & 0xFF) as usize, (key & 0xFFFF_FFFF) as usize);
                    for l in 0..lye {
                        if self.visit(c, r, k, l)? == Flow::End {
                            flow = Flow::End;
                            break 'all;
                        }
                    }
                }
                self.ctx.give_back(taken);
                return Ok(flow);
            }
        }
        Ok(Flow::More)
    }

    /// The packets of every precinct of resolution `r` of component `c` for layer `l`.
    fn visit_precincts(&mut self, c: usize, r: usize, l: usize) -> Result<Flow> {
        self.ctx.charge(cost::JPX_PACKET)?;
        let Some(res) = self.comps.get(c).and_then(|comp| comp.res.get(r)) else { return Ok(Flow::More) };
        let np = u64::from(res.npw) * u64::from(res.nph);
        for k in 0..np as usize {
            if self.visit(c, r, k, l)? == Flow::End {
                return Ok(Flow::End);
            }
        }
        Ok(Flow::More)
    }

    /// The packet of precinct `k`, resolution `r`, component `c` and layer `l`, unless an earlier order change had it.
    fn visit(&mut self, c: usize, r: usize, k: usize, l: usize) -> Result<Flow> {
        self.ctx.charge(cost::JPX_PACKET)?;
        if self.done {
            return Ok(Flow::End);
        }
        let Some(base) = self.comps.get(c).and_then(|comp| comp.res.get(r)).map(|res| res.pbase) else { return Ok(Flow::More) };
        let Some(next) = self.next_layer.get_mut(base + k) else { return Ok(Flow::More) };
        if l < usize::from(*next) {
            return Ok(Flow::More);
        }
        *next = (l + 1).min(usize::from(u16::MAX)) as u16;
        self.packet(c, r, k, l)
    }

    fn fail(&mut self, why: &str) -> Flow {
        self.done = true;
        if self.damaged.is_none() {
            self.damaged = Some(why.to_string());
        }
        Flow::End
    }

    /// One packet (B.10): its header, then its code-block bytes.
    fn packet(&mut self, c: usize, r: usize, k: usize, l: usize) -> Result<Flow> {
        // The tile-part the packet is in.
        let ppt_mode = self.ppt.is_some();
        let ppt_left = self.ppt.as_ref().map(|p| self.ppt_pos < p.len());
        if ppt_left == Some(false) {
            return Ok(self.fail("the packed packet headers end before the last packet"));
        }
        loop {
            let Some(part) = self.parts.get(self.cur) else { return Ok(self.fail("the tile's data ends before its last packet")) };
            let used_up = match &part.hdr {
                Some(h) if self.ppt.is_none() => part.hpos >= h.len(),
                _ => part.bpos >= part.body.len(),
            };
            if !used_up {
                break;
            }
            // With the headers packed for the whole tile, the packets after the last body's end may be empty ones.
            if ppt_mode && self.cur + 1 >= self.parts.len() {
                break;
            }
            self.cur += 1;
        }
        let ctx = self.ctx;
        let TileDec { comps, chunks, parts, pending, cur, held, ppt, ppt_pos, .. } = self;
        let (Some(part), Some(comp)) = (parts.get_mut(*cur), comps.get_mut(c)) else { return Ok(Flow::End) };
        // A start-of-packet marker (A.8.1) is in the body wherever the headers are; it can only be a marker.
        if part.body.get(part.bpos..part.bpos + 2) == Some(&[0xFF, 0x91][..]) {
            part.bpos += 6;
        }
        let packed = part.hdr.is_some() || ppt.is_some();
        let body: &[u8] = part.body;
        let (src, start): (&[u8], usize) = match (&*ppt, &part.hdr) {
            (Some(p), _) => (p.as_slice(), *ppt_pos),
            (None, Some(h)) => (h.as_slice(), part.hpos),
            (None, None) => (body, part.bpos),
        };
        let mut bits = Bits::new(src, start);
        pending.clear();
        let TileComp { res, pbands, blocks, nodes, style, need, .. } = comp;
        let Some(res) = res.get(r) else { return Ok(Flow::More) };
        let store = *need && res.keep;
        let mut failure: Option<&str> = None;
        'header: {
            let Some(first) = bits.bit() else {
                failure = Some("a packet header ends too soon");
                break 'header;
            };
            if first == 0 {
                break 'header;
            }
            let nbands = res.bands.len();
            for bi in 0..nbands {
                let Some(pb) = pbands.get(res.first_pband + k * nbands + bi) else { break 'header };
                let (ncw, nch) = (pb.ncw, pb.nch);
                let n = u64::from(ncw) * u64::from(nch);
                if n == 0 {
                    continue;
                }
                let Some(both) = nodes.get_mut(pb.nodes as usize..pb.nodes as usize + 2 * pb.nn as usize) else { break 'header };
                let (incl, zbp) = both.split_at_mut(pb.nn as usize);
                for i in 0..n as u32 {
                    ctx.charge(cost::JPX_BLOCK_HDR)?;
                    let bidx = pb.first_block + i;
                    let Some(blk) = blocks.get_mut(bidx as usize) else { break 'header };
                    let (bx, by) = (i % ncw, i / ncw);
                    let included = if blk.included { bits.bit() } else { tagtree::below(incl, ncw, nch, bx, by, l as u32 + 1, &mut bits).map(u32::from) };
                    match included {
                        None => {
                            failure = Some("a packet header ends too soon");
                            break 'header;
                        }
                        Some(0) => continue,
                        Some(_) => {}
                    }
                    if !blk.included {
                        match tagtree::value(zbp, ncw, nch, bx, by, 64, &mut bits) {
                            Some(Some(z)) => blk.zbp = z as u8,
                            Some(None) => {
                                failure = Some("a code-block with an impossible number of zero bit-planes");
                                break 'header;
                            }
                            None => {
                                failure = Some("a packet header ends too soon");
                                break 'header;
                            }
                        }
                        blk.included = true;
                        blk.lblock = 3;
                    }
                    let Some(mut left) = num_passes(&mut bits) else {
                        failure = Some("a packet header ends too soon");
                        break 'header;
                    };
                    if u32::from(blk.passes) + left > MAX_PASSES {
                        failure = Some("a code-block with too many coding passes");
                        break 'header;
                    }
                    loop {
                        match bits.bit() {
                            Some(1) => {
                                blk.lblock += 1;
                                if blk.lblock > 32 {
                                    failure = Some("a length that needs more than 32 bits");
                                    break 'header;
                                }
                            }
                            Some(_) => break,
                            None => {
                                failure = Some("a packet header ends too soon");
                                break 'header;
                            }
                        }
                    }
                    while left > 0 {
                        ctx.charge(cost::JPX_SEG)?;
                        let max = segment_passes(style.cbstyle, blk.nseg);
                        let take = left.min(max - u32::from(blk.seg_passes));
                        let nbits = u32::from(blk.lblock) + (31 - take.leading_zeros());
                        let Some(len) = (if nbits > 32 { None } else { bits.bits(nbits) }) else {
                            failure = Some("a packet header ends too soon");
                            break 'header;
                        };
                        pending.push((bidx, blk.nseg, take as u8, len));
                        blk.seg_passes += take as u8;
                        blk.passes += take as u16;
                        left -= take;
                        if u32::from(blk.seg_passes) == max {
                            blk.nseg = blk.nseg.saturating_add(1);
                            blk.seg_passes = 0;
                        }
                    }
                }
            }
        }
        bits.align();
        // An end-of-packet-header marker (A.8.2) can only be a marker, too.
        if failure.is_none() && src.get(bits.pos..bits.pos + 2) == Some(&[0xFF, 0x92][..]) {
            bits.pos += 2;
        }
        let end_pos = bits.pos;
        ctx.charge(bits.taken as f64 * cost::JPX_HEADER_BIT)?;
        if let Some(why) = failure {
            // The blocks named so far are not given their bytes.
            self.done = true;
            if self.damaged.is_none() {
                self.damaged = Some(why.to_string());
            }
            return Ok(Flow::End);
        }
        let mut bpos = if packed { part.bpos } else { end_pos };
        if ppt.is_some() {
            *ppt_pos = end_pos;
        } else if packed {
            part.hpos = end_pos;
        }
        let mut cut = false;
        let mut added = 0u64;
        for &(bidx, seg, passes, len) in pending.iter() {
            let avail = body.len().saturating_sub(bpos);
            let take = (len as usize).min(avail);
            if take < len as usize {
                cut = true;
            }
            if store {
                ctx.take_memory(std::mem::size_of::<Chunk>() as u64)?;
                added += std::mem::size_of::<Chunk>() as u64;
                let id = chunks.len() as u32;
                chunks.push(Chunk { off: (part.body_off + bpos) as u32, len: take as u32, next: NONE, seg, passes });
                if let Some(blk) = blocks.get_mut(bidx as usize) {
                    if blk.tail == NONE {
                        blk.head = id;
                    } else if let Some(prev) = chunks.get_mut(blk.tail as usize) {
                        prev.next = id;
                    }
                    blk.tail = id;
                }
            }
            bpos += take;
        }
        part.bpos = bpos;
        *held += added;
        if cut {
            self.done = true;
            if self.damaged.is_none() {
                self.damaged = Some("a packet's data is cut short".to_string());
            }
            return Ok(Flow::End);
        }
        Ok(Flow::More)
    }
}

/// The number of coding passes (Table B.4).
fn num_passes(bits: &mut Bits<'_>) -> Option<u32> {
    if bits.bit()? == 0 {
        return Some(1);
    }
    if bits.bit()? == 0 {
        return Some(2);
    }
    let v = bits.bits(2)?;
    if v < 3 {
        return Some(3 + v);
    }
    let v = bits.bits(5)?;
    if v < 31 {
        return Some(6 + v);
    }
    Some(37 + bits.bits(7)?)
}

/// How many passes codeword segment `index` of a code-block holds at most (D.6, Table D.9): one with every pass
/// terminated, 10 then 2, 1, 2, 1, ... with the arithmetic coding bypass, no end otherwise.
fn segment_passes(cbstyle: u8, index: u8) -> u32 {
    if cbstyle & t1::TERMALL != 0 {
        1
    } else if cbstyle & t1::BYPASS != 0 {
        match index {
            0 => 10,
            i if i % 2 == 1 => 2,
            _ => 1,
        }
    } else {
        1 << 20
    }
}

/// Which style, quantization and region of interest shift component `c` of a tile has (A.6.2 to A.6.4: a tile-part's
/// COC, then its COD, then the main header's COC, then its COD).
pub(super) fn component_coding<'h>(h: &'h Header, tile: &'h super::codestream::Coding, c: usize) -> Result<(&'h Style, &'h Quant, u8)> {
    let style = tile
        .coc
        .get(&c)
        .or_else(|| tile.cod.as_ref().map(|d| &d.style))
        .or_else(|| h.main.coc.get(&c))
        .or_else(|| h.main.cod.as_ref().map(|d| &d.style))
        .ok_or_else(|| bad("no COD"))?;
    let quant = tile.qcc.get(&c).or(tile.qcd.as_ref()).or_else(|| h.main.qcc.get(&c)).or(h.main.qcd.as_ref()).ok_or_else(|| bad("no QCD"))?;
    let roi = tile.rgn.get(&c).or_else(|| h.main.rgn.get(&c)).copied().unwrap_or(0);
    Ok((style, quant, roi))
}
