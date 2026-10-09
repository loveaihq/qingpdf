//! A JPEG 2000 encoder for the tests: it makes the streams the decoder is tried on that the encoders at hand (OpenJPEG
//! through Pillow) cannot: every code-block style switch, SOP and EPH markers, packed packet headers, progression order
//! changes, tile-parts, subsampled and signed components, and streams that are broken on purpose.
//!
//! It is not a codec: there is no forward wavelet transform. It takes the coefficients of the subbands as they are
//! (made up from a hash of where they are, so that they do not depend on how the picture is cut into tiles, precincts
//! or code-blocks) and codes them. With no decomposition levels the coefficients are the samples, so that what the
//! decoder must give is known; with levels, every way of packing the same coefficients must decode to the same
//! picture, and `JPX_DUMP_DIR=folder cargo test jpx` writes the streams (and what the decoder made of them) for
//! `tests/tools/check_jpx_dump.py`, which has OpenJPEG decode them. It follows ISO/IEC 15444-1 the plain way (the
//! contexts are worked out from the state of the neighbours each time, not from flags kept up to date), so that it
//! and the decoder check each other.

#![allow(clippy::too_many_arguments, clippy::type_complexity, clippy::needless_range_loop, clippy::manual_is_multiple_of)]

use crate::render::jbig2::MqEnc;
use crate::render::mq::context;

/// A hash of up to five numbers.
pub fn hash(seed: u64, a: u64, b: u64, c: u64, d: u64) -> u64 {
    let mut x = seed ^ 0x9E37_79B9_7F4A_7C15;
    for v in [a, b, c, d] {
        x = (x ^ v).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x ^= x >> 31;
        x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
        x ^= x >> 29;
    }
    x
}

#[derive(Clone, Copy)]
pub struct CompCfg {
    pub depth: u8,
    pub signed: bool,
    pub dx: u32,
    pub dy: u32,
}

#[derive(Clone)]
pub struct Poc {
    pub rs: u8,
    pub cs: u16,
    pub lye: u16,
    pub re: u8,
    pub ce: u16,
    pub prog: u8,
}

#[derive(Clone)]
pub struct Cfg {
    pub seed: u64,
    pub w: u32,
    pub h: u32,
    pub comps: Vec<CompCfg>,
    pub tile: (u32, u32),
    pub levels: u8,
    /// Code-block size exponents (width, height).
    pub cb: (u8, u8),
    pub cbstyle: u8,
    pub reversible: bool,
    pub mct: bool,
    pub layers: usize,
    pub prog: u8,
    /// Precinct size exponents per resolution (default 15).
    pub precincts: Option<Vec<(u8, u8)>>,
    pub sop: bool,
    pub eph: bool,
    pub ppm: bool,
    pub ppt: bool,
    pub tile_parts: usize,
    pub poc: Vec<Poc>,
    pub guard: u8,
    /// Region of interest shift (RGN) of component 0 (the coefficients are not shifted: it only tells the decoder).
    pub roi: u8,
    /// Is the progression order change in the tile-part headers instead of the main header?
    pub poc_in_tile: bool,
    /// Components with a decomposition level count of their own (COC): (component, levels).
    pub coc: Vec<(usize, u8)>,
    /// The coding style of the tiles is in their first tile-part header; the main header has another that is not it.
    pub tile_cod: bool,
}

impl Cfg {
    pub fn new(w: u32, h: u32, ncomp: usize) -> Cfg {
        Cfg {
            seed: 1,
            w,
            h,
            comps: vec![CompCfg { depth: 8, signed: false, dx: 1, dy: 1 }; ncomp],
            tile: (w, h),
            levels: 0,
            cb: (4, 4),
            cbstyle: 0,
            reversible: true,
            mct: false,
            layers: 1,
            prog: 0,
            precincts: None,
            sop: false,
            eph: false,
            ppm: false,
            ppt: false,
            tile_parts: 1,
            poc: Vec::new(),
            guard: 2,
            roi: 0,
            poc_in_tile: false,
            coc: Vec::new(),
            tile_cod: false,
        }
    }

    /// The decomposition levels of component `c`.
    pub fn levels_of(&self, c: usize) -> u8 {
        self.coc.iter().find(|(cc, _)| *cc == c).map_or(self.levels, |&(_, l)| l)
    }

    fn max_levels(&self) -> u8 {
        (0..self.ncomp()).map(|c| self.levels_of(c)).max().unwrap_or(0)
    }

    /// Bit-planes of band `b` of resolution `r` (the standard's Mb), for component `c`.
    fn eps(&self, c: usize, orient: u8) -> u32 {
        let gain = match orient {
            0 => 0,
            3 => 2,
            _ => 1,
        };
        u32::from(self.comps[c].depth) + gain
    }

    fn mb(&self, c: usize, orient: u8) -> u32 {
        u32::from(self.guard) + self.eps(c, orient) - 1
    }

    /// The coefficient at (x, y) of a band, in band coordinates: the same wherever the band is cut.
    pub fn coef(&self, c: usize, r: usize, orient: u8, x: i64, y: i64) -> i32 {
        let h = hash(self.seed, c as u64, (r as u64) << 4 | u64::from(orient), x as u64, y as u64);
        let mb = self.mb(c, orient).min(30);
        // Mostly small values, some up to the top bit-plane.
        let bits = if h >> 60 == 0 { (h >> 32) as u32 % mb } else { (h >> 32) as u32 % (mb / 2 + 1) };
        let mag = ((h as u32) & ((1u32 << bits) - 1)) as i32;
        let mag = if (h >> 50) & 0x3F == 0 { 0 } else { mag };
        if (h >> 63) == 1 { -mag } else { mag }
    }

    pub fn ncomp(&self) -> usize {
        self.comps.len()
    }
}

// --- geometry (B.5 to B.7), written out plainly ---------------------------------------------------------------------

fn cdiv(a: i64, b: i64) -> i64 {
    (a + b - 1).div_euclid(b)
}

#[derive(Clone)]
struct GBand {
    orient: u8,
    x0: i64,
    y0: i64,
    x1: i64,
    y1: i64,
}

struct GRes {
    x0: i64,
    y0: i64,
    ppx: u32,
    ppy: u32,
    npw: i64,
    nph: i64,
    bands: Vec<GBand>,
}

fn tile_rect(cfg: &Cfg, t: usize) -> [i64; 4] {
    let across = cdiv(i64::from(cfg.w), i64::from(cfg.tile.0)) as usize;
    let (p, q) = ((t % across) as i64, (t / across) as i64);
    let (tw, th) = (i64::from(cfg.tile.0), i64::from(cfg.tile.1));
    [p * tw, q * th, ((p + 1) * tw).min(i64::from(cfg.w)), ((q + 1) * th).min(i64::from(cfg.h))]
}

pub fn tile_count(cfg: &Cfg) -> usize {
    (cdiv(i64::from(cfg.w), i64::from(cfg.tile.0)) * cdiv(i64::from(cfg.h), i64::from(cfg.tile.1))) as usize
}

fn geometry(cfg: &Cfg, t: usize, c: usize) -> Vec<GRes> {
    let [tx0, ty0, tx1, ty1] = tile_rect(cfg, t);
    let comp = cfg.comps[c];
    let (x0, y0, x1, y1) = (cdiv(tx0, i64::from(comp.dx)), cdiv(ty0, i64::from(comp.dy)), cdiv(tx1, i64::from(comp.dx)), cdiv(ty1, i64::from(comp.dy)));
    let nl = i64::from(cfg.levels_of(c));
    let mut out = Vec::new();
    for r in 0..=nl {
        let d = nl - r;
        let (rx0, ry0, rx1, ry1) = (cdiv(x0, 1 << d), cdiv(y0, 1 << d), cdiv(x1, 1 << d), cdiv(y1, 1 << d));
        let (ppx, ppy) = cfg.precincts.as_ref().and_then(|p| p.get(r as usize)).map_or((15, 15), |&(a, b)| (u32::from(a), u32::from(b)));
        let npw = if rx1 > rx0 { cdiv(rx1, 1 << ppx) - rx0.div_euclid(1 << ppx) } else { 0 };
        let nph = if ry1 > ry0 { cdiv(ry1, 1 << ppy) - ry0.div_euclid(1 << ppy) } else { 0 };
        let mut bands = Vec::new();
        let kinds: &[u8] = if r == 0 { &[0] } else { &[1, 2, 3] };
        for &orient in kinds {
            let nb = if r == 0 { nl } else { nl - r + 1 };
            let (xob, yob) = (i64::from(orient & 1), i64::from(orient >> 1));
            let e = |v: i64, ob: i64| if nb == 0 { v } else { cdiv(v - (ob << (nb - 1)), 1 << nb) };
            bands.push(GBand { orient, x0: e(x0, xob), y0: e(y0, yob), x1: e(x1, xob), y1: e(y1, yob) });
        }
        out.push(GRes { x0: rx0, y0: ry0, ppx, ppy, npw, nph, bands });
    }
    out
}

/// The code-blocks of a precinct's part of a band: (x0, y0, x1, y1) in band coordinates, in raster order, and the
/// grid's width and height.
fn blocks_of(cfg: &Cfg, res: &GRes, r: usize, band: &GBand, k: i64) -> (Vec<[i64; 4]>, usize, usize) {
    let (px, py) = (k % res.npw, k / res.npw);
    let shift = i64::from(r > 0);
    let (pw, ph) = (1i64 << (res.ppx as i64 - shift), 1i64 << (res.ppy as i64 - shift));
    let px0 = ((res.x0.div_euclid(1 << res.ppx) + px) << res.ppx) >> shift;
    let py0 = ((res.y0.div_euclid(1 << res.ppy) + py) << res.ppy) >> shift;
    let (cx0, cy0, cx1, cy1) = (px0.max(band.x0), py0.max(band.y0), (px0 + pw).min(band.x1), (py0 + ph).min(band.y1));
    if cx1 <= cx0 || cy1 <= cy0 {
        return (Vec::new(), 0, 0);
    }
    let xcb = i64::from(cfg.cb.0).min(res.ppx as i64 - shift);
    let ycb = i64::from(cfg.cb.1).min(res.ppy as i64 - shift);
    let (i0, i1) = (cx0.div_euclid(1 << xcb), cdiv(cx1, 1 << xcb));
    let (j0, j1) = (cy0.div_euclid(1 << ycb), cdiv(cy1, 1 << ycb));
    let mut out = Vec::new();
    for j in j0..j1 {
        for i in i0..i1 {
            out.push([(i << xcb).max(cx0), (j << ycb).max(cy0), ((i + 1) << xcb).min(cx1), ((j + 1) << ycb).min(cy1)]);
        }
    }
    (out, (i1 - i0) as usize, (j1 - j0) as usize)
}

// --- tier 1 (Annex D) -----------------------------------------------------------------------------------------------

struct RawW {
    bytes: Vec<u8>,
    cur: u32,
    n: u32,
    limit: u32,
}

impl RawW {
    fn new() -> RawW {
        RawW { bytes: Vec::new(), cur: 0, n: 0, limit: 8 }
    }

    fn bit(&mut self, b: u32) {
        self.cur = (self.cur << 1) | b;
        self.n += 1;
        if self.n == self.limit {
            self.bytes.push(self.cur as u8);
            self.limit = if self.cur == 0xFF { 7 } else { 8 };
            self.cur = 0;
            self.n = 0;
        }
    }

    fn finish(mut self) -> Vec<u8> {
        while self.n != 0 {
            self.bit(0);
        }
        if self.bytes.last() == Some(&0xFF) {
            self.bytes.push(0);
        }
        self.bytes
    }
}

enum Coder {
    Mq(MqEnc),
    Raw(RawW),
}

struct BlockEnc<'a> {
    w: usize,
    h: usize,
    orient: u8,
    mag: &'a [u32],
    neg: &'a [bool],
    causal: bool,
    sig: Vec<bool>,
    visit: Vec<bool>,
    refined: Vec<bool>,
    cx: [u8; 19],
}

fn fresh() -> [u8; 19] {
    let mut cx = [0u8; 19];
    cx[0] = context(4, 0);
    cx[17] = context(3, 0);
    cx[18] = context(46, 0);
    cx
}

impl BlockEnc<'_> {
    /// Is the neighbour (x, y) significant, as seen from row `from`? (Not, in the causal mode, when it is in the stripe below.)
    fn sg(&self, x: i64, y: i64, from: usize) -> bool {
        if x < 0 || y < 0 || x >= self.w as i64 || y >= self.h as i64 {
            return false;
        }
        if self.causal && (y as usize) / 4 > from / 4 {
            return false;
        }
        self.sig[y as usize * self.w + x as usize]
    }

    fn counts(&self, x: usize, y: usize) -> (u32, u32, u32) {
        let (xi, yi) = (x as i64, y as i64);
        let s = |dx: i64, dy: i64| u32::from(self.sg(xi + dx, yi + dy, y));
        (s(-1, 0) + s(1, 0), s(0, -1) + s(0, 1), s(-1, -1) + s(1, -1) + s(-1, 1) + s(1, 1))
    }

    fn sig_ctx(&self, x: usize, y: usize) -> usize {
        let (mut h, mut v, d) = self.counts(x, y);
        if self.orient == 1 {
            std::mem::swap(&mut h, &mut v);
        }
        if self.orient == 3 {
            let hv = h + v;
            return match d {
                0 => [0, 1, 2][hv.min(2) as usize],
                1 => [3, 4, 5][hv.min(2) as usize],
                2 => [6, 7, 7][hv.min(2) as usize],
                _ => 8,
            };
        }
        match (h, v) {
            (2, _) => 8,
            (1, 0) if d == 0 => 5,
            (1, 0) => 6,
            (1, _) => 7,
            (0, 2) => 4,
            (0, 1) => 3,
            _ => [0, 1, 2][d.min(2) as usize],
        }
    }

    fn sign_ctx(&self, x: usize, y: usize) -> (usize, u32) {
        let (xi, yi) = (x as i64, y as i64);
        let c = |dx: i64, dy: i64| -> i32 {
            let (nx, ny) = (xi + dx, yi + dy);
            if self.sg(nx, ny, y) {
                if self.neg[ny as usize * self.w + nx as usize] { -1 } else { 1 }
            } else {
                0
            }
        };
        let h = (c(-1, 0) + c(1, 0)).clamp(-1, 1);
        let v = (c(0, -1) + c(0, 1)).clamp(-1, 1);
        match (h, v) {
            (1, 1) => (13, 0),
            (1, 0) => (12, 0),
            (1, -1) => (11, 0),
            (0, 1) => (10, 0),
            (0, 0) => (9, 0),
            (0, -1) => (10, 1),
            (-1, 1) => (11, 1),
            (-1, 0) => (12, 1),
            _ => (13, 1),
        }
    }

    fn put(&mut self, coder: &mut Coder, bit: u32, cx: usize) {
        match coder {
            Coder::Mq(mq) => mq.encode(bit, &mut self.cx[cx]),
            Coder::Raw(raw) => raw.bit(bit),
        }
    }

    fn put_sign(&mut self, coder: &mut Coder, x: usize, y: usize) {
        let neg = u32::from(self.neg[y * self.w + x]);
        match coder {
            Coder::Mq(mq) => {
                let (cx, flip) = self.sign_ctx(x, y);
                mq.encode(neg ^ flip, &mut self.cx[cx]);
            }
            Coder::Raw(raw) => raw.bit(neg),
        }
    }

    fn bit_of(&self, x: usize, y: usize, p: u32) -> u32 {
        (self.mag[y * self.w + x] >> p) & 1
    }

    fn sigprop(&mut self, coder: &mut Coder, p: u32) {
        for y0 in (0..self.h).step_by(4) {
            for x in 0..self.w {
                for y in y0..(y0 + 4).min(self.h) {
                    if self.sig[y * self.w + x] || self.sig_ctx(x, y) == 0 {
                        continue;
                    }
                    let bit = self.bit_of(x, y, p);
                    let cx = self.sig_ctx(x, y);
                    self.put(coder, bit, cx);
                    if bit == 1 {
                        self.put_sign(coder, x, y);
                        self.sig[y * self.w + x] = true;
                    }
                    self.visit[y * self.w + x] = true;
                }
            }
        }
    }

    fn magref(&mut self, coder: &mut Coder, p: u32) {
        for y0 in (0..self.h).step_by(4) {
            for x in 0..self.w {
                for y in y0..(y0 + 4).min(self.h) {
                    let i = y * self.w + x;
                    if !self.sig[i] || self.visit[i] {
                        continue;
                    }
                    let cx = if self.refined[i] {
                        16
                    } else {
                        let (xi, yi) = (x as i64, y as i64);
                        let any = (-1..=1).any(|dy| (-1..=1).any(|dx| (dx != 0 || dy != 0) && self.sg(xi + dx, yi + dy, y)));
                        if any { 15 } else { 14 }
                    };
                    let bit = self.bit_of(x, y, p);
                    self.put(coder, bit, cx);
                    self.refined[i] = true;
                }
            }
        }
    }

    fn cleanup(&mut self, coder: &mut Coder, p: u32, segsym: bool) {
        for y0 in (0..self.h).step_by(4) {
            for x in 0..self.w {
                let mut start = y0;
                if y0 + 4 <= self.h && (y0..y0 + 4).all(|y| !self.sig[y * self.w + x] && !self.visit[y * self.w + x] && self.sig_ctx(x, y) == 0) {
                    match (y0..y0 + 4).find(|&y| self.bit_of(x, y, p) == 1) {
                        None => {
                            self.put(coder, 0, 17);
                            continue;
                        }
                        Some(y) => {
                            self.put(coder, 1, 17);
                            let r = (y - y0) as u32;
                            self.put(coder, r >> 1, 18);
                            self.put(coder, r & 1, 18);
                            self.put_sign(coder, x, y);
                            self.sig[y * self.w + x] = true;
                            start = y + 1;
                        }
                    }
                }
                for y in start..(y0 + 4).min(self.h) {
                    let i = y * self.w + x;
                    if self.sig[i] || self.visit[i] {
                        continue;
                    }
                    let bit = self.bit_of(x, y, p);
                    let cx = self.sig_ctx(x, y);
                    self.put(coder, bit, cx);
                    if bit == 1 {
                        self.put_sign(coder, x, y);
                        self.sig[i] = true;
                    }
                }
            }
        }
        self.visit.iter_mut().for_each(|v| *v = false);
        if segsym {
            for b in [1, 0, 1, 0] {
                self.put(coder, b, 18);
            }
        }
    }
}

/// One codeword segment of a code-block: its bytes and the number of coding passes in it.
#[derive(Clone)]
pub struct Seg {
    pub bytes: Vec<u8>,
    pub passes: usize,
}

/// The bit-planes `nplanes` of a block, as codeword segments for the style switches.
fn encode_block(w: usize, h: usize, orient: u8, mag: &[u32], neg: &[bool], nplanes: u32, style: u8) -> Vec<Seg> {
    let mut e = BlockEnc { w, h, orient, mag, neg, causal: style & 8 != 0, sig: vec![false; w * h], visit: vec![false; w * h], refined: vec![false; w * h], cx: fresh() };
    let (bypass, reset, termall, segsym) = (style & 1 != 0, style & 2 != 0, style & 4 != 0, style & 32 != 0);
    let total = 1 + 3 * (nplanes as usize - 1);
    let mut segs = Vec::new();
    let mut coder: Option<Coder> = None;
    let mut in_seg = 0usize;
    for pass in 0..total {
        let kind = if pass == 0 { 2 } else { (pass - 1) % 3 };
        let plane = nplanes - 1 - pass.div_ceil(3) as u32;
        let raw = bypass && pass >= 10 && kind != 2;
        let c = coder.get_or_insert_with(|| if raw { Coder::Raw(RawW::new()) } else { Coder::Mq(MqEnc::new()) });
        match kind {
            0 => e.sigprop(c, plane),
            1 => e.magref(c, plane),
            _ => e.cleanup(c, plane, segsym),
        }
        in_seg += 1;
        if reset {
            e.cx = fresh();
        }
        // Where a segment ends: every pass with termall; with the bypass, after pass 9 and then after each raw pair and
        // each cleanup pass; and the last pass.
        let ends = termall || pass + 1 == total || (bypass && (pass == 9 || (pass >= 10 && (kind == 1 || kind == 2))));
        if ends {
            let bytes = match coder.take() {
                Some(Coder::Mq(mq)) => {
                    let mut b = mq.finish();
                    // The marker the JBIG2 flavour appends, and a 0xFF the decoder would supply itself.
                    b.truncate(b.len() - 2);
                    b
                }
                Some(Coder::Raw(r)) => r.finish(),
                None => Vec::new(),
            };
            segs.push(Seg { bytes, passes: in_seg });
            in_seg = 0;
        }
    }
    segs
}

// --- tier 2 (Annex B) -------------------------------------------------------------------------------------------------

struct Bits {
    bytes: Vec<u8>,
    cur: u32,
    n: u32,
    limit: u32,
}

impl Bits {
    fn new() -> Bits {
        Bits { bytes: Vec::new(), cur: 0, n: 0, limit: 8 }
    }

    fn bit(&mut self, b: u32) {
        self.cur = (self.cur << 1) | (b & 1);
        self.n += 1;
        if self.n == self.limit {
            self.bytes.push(self.cur as u8);
            self.limit = if self.cur == 0xFF { 7 } else { 8 };
            self.cur = 0;
            self.n = 0;
        }
    }

    fn bits(&mut self, v: u32, n: u32) {
        for i in (0..n).rev() {
            self.bit(v >> i);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        while self.n != 0 {
            self.bit(0);
        }
        if self.bytes.last() == Some(&0xFF) {
            self.bytes.push(0);
        }
        self.bytes
    }
}

/// A tag tree over a grid (B.10.2), encoder side.
struct Tree {
    dims: Vec<(usize, usize, usize)>,
    value: Vec<u32>,
    low: Vec<u32>,
    known: Vec<bool>,
}

impl Tree {
    fn new(w: usize, h: usize, leaves: &[u32]) -> Tree {
        let mut dims = Vec::new();
        let (mut lw, mut lh, mut off) = (w, h, 0);
        loop {
            dims.push((lw, lh, off));
            off += lw * lh;
            if lw <= 1 && lh <= 1 {
                break;
            }
            lw = lw.div_ceil(2);
            lh = lh.div_ceil(2);
        }
        let mut value = vec![u32::MAX; off];
        value[..w * h].copy_from_slice(leaves);
        for l in 1..dims.len() {
            let (cw, ch, coff) = dims[l - 1];
            let (pw, ph, poff) = dims[l];
            for y in 0..ph {
                for x in 0..pw {
                    let mut m = u32::MAX;
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let (cx, cy) = (2 * x + dx, 2 * y + dy);
                        if cx < cw && cy < ch {
                            m = m.min(value[coff + cy * cw + cx]);
                        }
                    }
                    value[poff + y * pw + x] = m;
                }
            }
        }
        Tree { low: vec![0; off], known: vec![false; off], value, dims }
    }

    fn encode(&mut self, bits: &mut Bits, x: usize, y: usize, threshold: u32) {
        let mut low = 0;
        for l in (0..self.dims.len()).rev() {
            let (w, _, off) = self.dims[l];
            let i = off + (y >> l) * w + (x >> l);
            if low > self.low[i] {
                self.low[i] = low;
            } else {
                low = self.low[i];
            }
            while low < threshold {
                if low >= self.value[i] {
                    if !self.known[i] {
                        bits.bit(1);
                        self.known[i] = true;
                    }
                    break;
                }
                bits.bit(0);
                low += 1;
            }
            self.low[i] = low;
        }
    }
}

struct CodedBlock {
    zbp: u32,
    segs: Vec<Seg>,
    /// The layer each segment goes in (not decreasing); the first one is the layer of inclusion.
    layer_of: Vec<usize>,
    lblock: u32,
    sent: usize,
}

struct PBandEnc {
    ncw: usize,
    blocks: Vec<CodedBlock>,
    incl: Tree,
    zbp: Tree,
}

struct PrecinctEnc {
    bands: Vec<PBandEnc>,
}

fn nplanes_of(mag: &[u32]) -> u32 {
    32 - mag.iter().fold(0u32, |a, &m| a | m).leading_zeros()
}

fn make_precinct(cfg: &Cfg, t: usize, c: usize, r: usize, k: i64, res: &GRes) -> PrecinctEnc {
    let mut bands = Vec::new();
    for band in &res.bands {
        let (rects, ncw, nch) = blocks_of(cfg, res, r, band, k);
        let mb = cfg.mb(c, band.orient) + if c == 0 { u32::from(cfg.roi) } else { 0 };
        let mut blocks = Vec::new();
        for (bi, b) in rects.iter().enumerate() {
            let (w, h) = ((b[2] - b[0]) as usize, (b[3] - b[1]) as usize);
            let mut mag = Vec::with_capacity(w * h);
            let mut neg = Vec::with_capacity(w * h);
            for y in b[1]..b[3] {
                for x in b[0]..b[2] {
                    let v = cfg.coef(c, r, band.orient, x, y);
                    // Where there is a region of interest its coefficients are shifted up (H.1).
                    let up = if c == 0 && cfg.roi > 0 && (x + y) % 3 == 0 { u32::from(cfg.roi) } else { 0 };
                    mag.push(v.unsigned_abs() << up);
                    neg.push(v < 0);
                }
            }
            let np = nplanes_of(&mag);
            let (segs, zbp) = if np == 0 { (Vec::new(), 0) } else { (encode_block(w, h, band.orient, &mag, &neg, np, cfg.cbstyle), mb.saturating_sub(np)) };
            // Layers: the segments, in order, get layers that do not decrease.
            let mut layer = (hash(cfg.seed, t as u64, (c * 1000 + r) as u64, (k as u64) << 8 | bi as u64, 77) % cfg.layers as u64) as usize;
            let mut layer_of = Vec::new();
            for si in 0..segs.len() {
                if si > 0 && cfg.layers > 1 && hash(cfg.seed, t as u64, c as u64, bi as u64, si as u64) % 2 == 0 {
                    layer = (layer + 1).min(cfg.layers - 1);
                }
                layer_of.push(layer);
            }
            blocks.push(CodedBlock { zbp, segs, layer_of, lblock: 3, sent: 0 });
        }
        let incl_leaves: Vec<u32> = blocks.iter().map(|b| b.layer_of.first().map_or(u32::MAX, |&l| l as u32)).collect();
        let zbp_leaves: Vec<u32> = blocks.iter().map(|b| b.zbp).collect();
        bands.push(PBandEnc { ncw, incl: Tree::new(ncw, nch, &incl_leaves), zbp: Tree::new(ncw, nch, &zbp_leaves), blocks });
    }
    PrecinctEnc { bands }
}

fn floor_log2(v: usize) -> u32 {
    usize::BITS - 1 - v.leading_zeros()
}

/// The header and the body of the packet of layer `l` of a precinct.
fn packet(p: &mut PrecinctEnc, l: usize) -> (Vec<u8>, Vec<u8>) {
    let mut bits = Bits::new();
    let mut body = Vec::new();
    let any = p.bands.iter().any(|b| b.blocks.iter().any(|blk| blk.layer_of.contains(&l)));
    if !any {
        bits.bit(0);
        return (bits.finish(), body);
    }
    bits.bit(1);
    for band in &mut p.bands {
        let ncw = band.ncw;
        for i in 0..band.blocks.len() {
            let (x, y) = (i % ncw, i / ncw);
            let blk = &mut band.blocks[i];
            let mine: Vec<usize> = (0..blk.segs.len()).filter(|&s| blk.layer_of[s] == l).collect();
            let first = blk.sent == 0;
            if first {
                band.incl.encode(&mut bits, x, y, l as u32 + 1);
            } else {
                bits.bit(u32::from(!mine.is_empty()));
            }
            if mine.is_empty() {
                continue;
            }
            if first {
                band.zbp.encode(&mut bits, x, y, blk.zbp + 1);
            }
            let passes: usize = mine.iter().map(|&s| blk.segs[s].passes).sum();
            match passes {
                1 => bits.bit(0),
                2 => bits.bits(0b10, 2),
                3..=5 => bits.bits(0b1100 | (passes as u32 - 3), 4),
                6..=36 => bits.bits((0b1111 << 5) | (passes as u32 - 6), 9),
                _ => bits.bits((0b1_1111_1111 << 7) | (passes as u32 - 37), 16),
            }
            // Lblock: enough for the longest length.
            let need = mine.iter().map(|&s| (32 - (blk.segs[s].bytes.len() as u32).leading_zeros()).saturating_sub(floor_log2(blk.segs[s].passes))).max().unwrap_or(0);
            let up = need.saturating_sub(blk.lblock);
            for _ in 0..up {
                bits.bit(1);
            }
            bits.bit(0);
            blk.lblock += up;
            for &s in &mine {
                let seg = &blk.segs[s];
                bits.bits(seg.bytes.len() as u32, blk.lblock + floor_log2(seg.passes));
                body.extend_from_slice(&seg.bytes);
            }
            blk.sent += mine.len();
        }
    }
    (bits.finish(), body)
}

// --- the codestream (Annex A) ----------------------------------------------------------------------------------------

fn put16(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&(x as u16).to_be_bytes());
}

fn put32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_be_bytes());
}

fn marker(out: &mut Vec<u8>, code: u16, body: &[u8]) {
    out.extend_from_slice(&code.to_be_bytes());
    out.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(body);
}

/// A packet's place in a progression: component, resolution, precinct, layer.
type Key = (usize, usize, i64, usize);

/// The packets of a tile in the order of one progression (B.12), by the standard's own loops over positions.
fn order(cfg: &Cfg, t: usize, geo: &[Vec<GRes>], e: &Poc) -> Vec<Key> {
    let mut out = Vec::new();
    let [tx0, ty0, tx1, ty1] = tile_rect(cfg, t);
    let nc = cfg.ncomp().min(usize::from(e.ce));
    let nres = (cfg.max_levels() as usize + 1).min(usize::from(e.re));
    let lye = usize::from(e.lye).min(cfg.layers);
    let np = |c: usize, r: usize| geo[c].get(r).map_or(0, |g| g.npw * g.nph);
    match e.prog {
        0 => {
            for l in 0..lye {
                for r in usize::from(e.rs)..nres {
                    for c in usize::from(e.cs)..nc {
                        for k in 0..np(c, r) {
                            out.push((c, r, k, l));
                        }
                    }
                }
            }
        }
        1 => {
            for r in usize::from(e.rs)..nres {
                for l in 0..lye {
                    for c in usize::from(e.cs)..nc {
                        for k in 0..np(c, r) {
                            out.push((c, r, k, l));
                        }
                    }
                }
            }
        }
        _ => {
            // The precinct of component c, resolution r that position (x, y) starts, if it does.
            let at = |c: usize, r: usize, x: i64, y: i64| -> Option<i64> {
                let g = geo[c].get(r)?;
                if g.npw == 0 || g.nph == 0 {
                    return None;
                }
                let comp = cfg.comps[c];
                let d = i64::from(cfg.levels_of(c)) - r as i64;
                let (sx, sy) = (i64::from(comp.dx) << (d + g.ppx as i64), i64::from(comp.dy) << (d + g.ppy as i64));
                let x_ok = x % sx == 0 || (x == tx0 && (g.x0 << d) % (1 << (d + g.ppx as i64)) != 0);
                let y_ok = y % sy == 0 || (y == ty0 && (g.y0 << d) % (1 << (d + g.ppy as i64)) != 0);
                if !x_ok || !y_ok {
                    return None;
                }
                let px = cdiv(x, i64::from(comp.dx) << d).div_euclid(1 << g.ppx) - g.x0.div_euclid(1 << g.ppx);
                let py = cdiv(y, i64::from(comp.dy) << d).div_euclid(1 << g.ppy) - g.y0.div_euclid(1 << g.ppy);
                (px >= 0 && py >= 0 && px < g.npw && py < g.nph).then_some(py * g.npw + px)
            };
            let positions: Vec<(i64, i64)> = (ty0..ty1).flat_map(|y| (tx0..tx1).map(move |x| (x, y))).collect();
            match e.prog {
                2 => {
                    for r in usize::from(e.rs)..nres {
                        for &(x, y) in &positions {
                            for c in usize::from(e.cs)..nc {
                                if let Some(k) = at(c, r, x, y) {
                                    for l in 0..lye {
                                        out.push((c, r, k, l));
                                    }
                                }
                            }
                        }
                    }
                }
                3 => {
                    for &(x, y) in &positions {
                        for c in usize::from(e.cs)..nc {
                            for r in usize::from(e.rs)..nres {
                                if let Some(k) = at(c, r, x, y) {
                                    for l in 0..lye {
                                        out.push((c, r, k, l));
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {
                    for c in usize::from(e.cs)..nc {
                        for &(x, y) in &positions {
                            for r in usize::from(e.rs)..nres {
                                if let Some(k) = at(c, r, x, y) {
                                    for l in 0..lye {
                                        out.push((c, r, k, l));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

pub struct Encoded {
    pub codestream: Vec<u8>,
}

fn siz(cfg: &Cfg) -> Vec<u8> {
    let mut b = Vec::new();
    put16(&mut b, 0);
    for v in [cfg.w, cfg.h, 0, 0, cfg.tile.0, cfg.tile.1, 0, 0] {
        put32(&mut b, v);
    }
    put16(&mut b, cfg.ncomp() as u32);
    for c in &cfg.comps {
        b.extend_from_slice(&[(c.depth - 1) | if c.signed { 0x80 } else { 0 }, c.dx as u8, c.dy as u8]);
    }
    b
}

fn qcd(cfg: &Cfg, c: usize) -> Vec<u8> {
    let mut b = Vec::new();
    let nb = 3 * cfg.levels_of(c) as usize + 1;
    let orients = |i: usize| if i == 0 { 0 } else { [1u8, 2, 3][(i - 1) % 3] };
    if cfg.reversible {
        b.push(cfg.guard << 5);
        for i in 0..nb {
            b.push((cfg.eps(c, orients(i)) as u8) << 3);
        }
    } else {
        b.push((cfg.guard << 5) | 2);
        for i in 0..nb {
            let v = ((cfg.eps(c, orients(i)) as u16) << 11) | (hash(cfg.seed, i as u64, 0, 0, 5) % 2048) as u16;
            b.extend_from_slice(&v.to_be_bytes());
        }
    }
    b
}

fn cod_style(cfg: &Cfg, levels: u8) -> Vec<u8> {
    let mut b = vec![levels, cfg.cb.0 - 2, cfg.cb.1 - 2, cfg.cbstyle, u8::from(cfg.reversible)];
    if let Some(p) = &cfg.precincts {
        for r in 0..=levels as usize {
            let (x, y) = p.get(r).copied().unwrap_or((15, 15));
            b.push(x | (y << 4));
        }
    }
    b
}

fn poc_body(cfg: &Cfg, list: &[Poc]) -> Vec<u8> {
    let mut b = Vec::new();
    for e in list {
        b.push(e.rs);
        if cfg.ncomp() >= 257 {
            put16(&mut b, u32::from(e.cs));
        } else {
            b.push(e.cs as u8);
        }
        put16(&mut b, u32::from(e.lye));
        b.push(e.re);
        if cfg.ncomp() >= 257 {
            put16(&mut b, u32::from(e.ce));
        } else {
            b.push(e.ce as u8);
        }
        b.push(e.prog);
    }
    b
}

/// The codestream of `cfg`.
pub fn encode(cfg: &Cfg) -> Encoded {
    let mut main = vec![0xFF, 0x4F];
    marker(&mut main, 0xFF51, &siz(cfg));
    // The coding style: COD (what the tiles use, or, with `tile_cod`, something else that the tiles' own COD overrides),
    // COC for the components with levels of their own, QCD for component 0 and QCC for the components that need one.
    let cod_body = |prog: u8, layers: usize, mct: bool| -> Vec<u8> {
        let mut cod = vec![u8::from(cfg.precincts.is_some()) | (u8::from(cfg.sop) << 1) | (u8::from(cfg.eph) << 2), prog];
        put16(&mut cod, layers as u32);
        cod.push(u8::from(mct));
        cod.extend(cod_style(cfg, cfg.levels));
        cod
    };
    let real_cod = cod_body(cfg.prog, cfg.layers, cfg.mct);
    if cfg.tile_cod {
        marker(&mut main, 0xFF52, &cod_body((cfg.prog + 1) % 5, 1, !cfg.mct));
    } else {
        marker(&mut main, 0xFF52, &real_cod);
    }
    for &(c, levels) in &cfg.coc {
        let mut b = vec![c as u8, u8::from(cfg.precincts.is_some())];
        b.extend(cod_style(cfg, levels));
        marker(&mut main, 0xFF53, &b);
    }
    marker(&mut main, 0xFF5C, &qcd(cfg, 0));
    for c in 1..cfg.ncomp() {
        if cfg.comps[c].depth != cfg.comps[0].depth || cfg.levels_of(c) != cfg.levels_of(0) {
            let mut b = vec![c as u8];
            b.extend(qcd(cfg, c));
            marker(&mut main, 0xFF5D, &b);
        }
    }
    if cfg.roi > 0 {
        marker(&mut main, 0xFF5E, &[0, 0, cfg.roi]);
    }
    if !cfg.poc.is_empty() && !cfg.poc_in_tile {
        marker(&mut main, 0xFF5F, &poc_body(cfg, &cfg.poc));
    }
    // The tiles' packets.
    struct TilePart {
        tile: usize,
        headers: Vec<u8>,
        body: Vec<u8>,
        packets: usize,
    }
    let mut parts: Vec<(usize, Vec<TilePart>)> = Vec::new();
    for t in 0..tile_count(cfg) {
        let geo: Vec<Vec<GRes>> = (0..cfg.ncomp()).map(|c| geometry(cfg, t, c)).collect();
        let mut precincts: std::collections::HashMap<(usize, usize, i64), PrecinctEnc> = std::collections::HashMap::new();
        for c in 0..cfg.ncomp() {
            for (r, res) in geo[c].iter().enumerate() {
                for k in 0..res.npw * res.nph {
                    precincts.insert((c, r, k), make_precinct(cfg, t, c, r, k, res));
                }
            }
        }
        let entries: Vec<Poc> = if cfg.poc.is_empty() {
            vec![Poc { rs: 0, cs: 0, lye: cfg.layers as u16, re: cfg.max_levels() + 1, ce: cfg.ncomp() as u16, prog: cfg.prog }]
        } else {
            cfg.poc.clone()
        };
        let mut done: std::collections::HashSet<Key> = std::collections::HashSet::new();
        let mut keys = Vec::new();
        for e in &entries {
            for key in order(cfg, t, &geo, e) {
                if done.insert(key) {
                    keys.push(key);
                }
            }
        }
        let mut seq = 0u32;
        let mut packets: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        for (c, r, k, l) in keys {
            let p = precincts.get_mut(&(c, r, k)).expect("precinct");
            let (mut header, body) = packet(p, l);
            if cfg.eph {
                header.extend_from_slice(&[0xFF, 0x92]);
            }
            let mut sop = Vec::new();
            if cfg.sop {
                sop.extend_from_slice(&[0xFF, 0x91, 0, 4]);
                put16(&mut sop, seq);
                seq += 1;
            }
            // (SOP, header) travel together as "header" here and are split below.
            let mut h = sop;
            h.extend(header);
            packets.push((h, body));
        }
        // Cut into tile-parts.
        let n = cfg.tile_parts.max(1);
        let per = packets.len().div_ceil(n).max(1);
        let mut tps = Vec::new();
        for chunk in packets.chunks(per).take(n) {
            let mut tp = TilePart { tile: t, headers: Vec::new(), body: Vec::new(), packets: chunk.len() };
            for (h, b) in chunk {
                if cfg.ppm || cfg.ppt {
                    // The SOP marker stays with the data, the header goes to the packed headers.
                    let (sop, rest) = if cfg.sop { h.split_at(6) } else { h.split_at(0) };
                    tp.body.extend_from_slice(sop);
                    tp.headers.extend_from_slice(rest);
                } else {
                    tp.body.extend_from_slice(h);
                }
                tp.body.extend_from_slice(b);
            }
            tps.push(tp);
        }
        if tps.is_empty() {
            tps.push(TilePart { tile: t, headers: Vec::new(), body: Vec::new(), packets: 0 });
        }
        parts.push((t, tps));
    }
    if cfg.ppm {
        // One block of packed headers per tile-part, in the order of the tile-parts in the file.
        let mut stream = Vec::new();
        for (_, tps) in &parts {
            for tp in tps {
                put32(&mut stream, tp.headers.len() as u32);
                stream.extend_from_slice(&tp.headers);
            }
        }
        // Split over markers of at most 60000 bytes (a block may run over from one to the next).
        for (z, chunk) in stream.chunks(60_000).enumerate() {
            let mut b = vec![z as u8];
            b.extend_from_slice(chunk);
            marker(&mut main, 0xFF60, &b);
        }
    }
    let mut out = main;
    for (t, tps) in &parts {
        let n = tps.len();
        for (i, tp) in tps.iter().enumerate() {
            let mut head = Vec::new();
            if cfg.ppt {
                let mut b = vec![i as u8];
                b.extend_from_slice(&tp.headers);
                marker(&mut head, 0xFF61, &b);
            }
            if cfg.tile_cod && i == 0 {
                marker(&mut head, 0xFF52, &real_cod);
                marker(&mut head, 0xFF5C, &qcd(cfg, 0));
            }
            if cfg.poc_in_tile && i == 0 && !cfg.poc.is_empty() {
                marker(&mut head, 0xFF5F, &poc_body(cfg, &cfg.poc));
            }
            let psot = 12 + head.len() + 2 + tp.body.len();
            out.extend_from_slice(&[0xFF, 0x90, 0, 10]);
            put16(&mut out, *t as u32);
            put32(&mut out, psot as u32);
            out.push(i as u8);
            out.push(n as u8);
            out.extend(head);
            out.extend_from_slice(&[0xFF, 0x93]);
            out.extend_from_slice(&tp.body);
            let _ = (tp.tile, tp.packets);
        }
    }
    out.extend_from_slice(&[0xFF, 0xD9]);
    Encoded { codestream: out }
}

/// A JP2 file around a codestream: the signature, the file type, a header with `ihdr`, the colour specification
/// (`enumcs`) and any `extra` header boxes, then the codestream box.
pub fn jp2(cfg: &Cfg, codestream: &[u8], enumcs: u32, extra: &[Vec<u8>]) -> Vec<u8> {
    fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }
    let mut out = boxed(b"jP  ", &[0x0D, 0x0A, 0x87, 0x0A]);
    out.extend(boxed(b"ftyp", b"jp2 \0\0\0\0jp2 "));
    let mut ihdr = Vec::new();
    put32(&mut ihdr, cfg.h);
    put32(&mut ihdr, cfg.w);
    put16(&mut ihdr, cfg.ncomp() as u32);
    ihdr.extend_from_slice(&[7, 7, 0, 0]);
    let mut header = boxed(b"ihdr", &ihdr);
    let mut colr = vec![1, 0, 0];
    put32(&mut colr, enumcs);
    header.extend(boxed(b"colr", &colr));
    for e in extra {
        header.extend_from_slice(e);
    }
    out.extend(boxed(b"jp2h", &header));
    out.extend(boxed(b"jp2c", codestream));
    out
}

/// A box (for [`jp2`]'s `extra`).
pub fn jp2_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(kind);
    v.extend_from_slice(body);
    v
}
