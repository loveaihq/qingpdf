//! Tier 1 of JPEG 2000: the decoding of a code-block's bit-planes (ISO/IEC 15444-1 Annex D). Three passes per
//! bit-plane (significance propagation, magnitude refinement, cleanup), arithmetic coded with the MQ decoder of
//! [`crate::render::mq`] or, in the bypass mode, raw bits; the five code-block style switches: bypass, context reset,
//! termination of every pass, vertically causal contexts, segmentation symbols (predictable termination only helps an
//! encoder's checks and is ignored).
//!
//! The state of a sample is a word of flags: which of its eight neighbours are significant, the signs of the four
//! straight ones, and its own state; when a sample becomes significant it tells its neighbours, so that the context of
//! a decision is a table lookup. Magnitudes are kept with one fractional bit: the bit that is last decoded of a sample
//! is followed by a half, the middle of what is not decoded (that is the reconstruction of E.1.1.2 and the reason
//! lossless data comes out exact: the half after plane 0 falls off).

use super::Ctx;
use crate::error::Result;
use crate::render::mq::{Mq, context};
use crate::render::work::cost;

pub(super) const BYPASS: u8 = 1;
pub(super) const RESET: u8 = 2;
pub(super) const TERMALL: u8 = 4;
pub(super) const CAUSAL: u8 = 8;
pub(super) const SEGSYM: u8 = 32;

// Neighbour bits.
const N: u16 = 1;
const S: u16 = 2;
const W: u16 = 4;
const E: u16 = 8;
const NW: u16 = 16;
const NE: u16 = 32;
const SW: u16 = 64;
const SE: u16 = 128;
const NB: u16 = 0xFF;
// Signs of the straight neighbours (set when negative).
const N_NEG: u16 = 256;
const S_NEG: u16 = 512;
const W_NEG: u16 = 1024;
const E_NEG: u16 = 2048;
// The sample's own state.
const SIG: u16 = 1 << 12;
const REFINED: u16 = 1 << 14;

/// The sign of a decoded sample in `T1::data`.
pub(super) const NEGATIVE: u32 = 1 << 31;

/// Contexts: 0-8 significance, 9-13 sign, 14-16 magnitude refinement, 17 run length, 18 uniform (Table D.7).
const CTX_RUN: usize = 17;
const CTX_UNI: usize = 18;

#[allow(clippy::indexing_slicing)] // evaluated at compile time: a mistake would not build
const SIG_LUT: [[u8; 256]; 4] = {
    let mut t = [[0u8; 256]; 4];
    let mut orient = 0;
    while orient < 4 {
        let mut nb = 0usize;
        while nb < 256 {
            let mut h = ((nb >> 2) & 1) + ((nb >> 3) & 1);
            let mut v = (nb & 1) + ((nb >> 1) & 1);
            let d = ((nb >> 4) & 1) + ((nb >> 5) & 1) + ((nb >> 6) & 1) + ((nb >> 7) & 1);
            let ctx: u8 = if orient == 3 {
                let hv = h + v;
                if d >= 3 {
                    8
                } else if d == 2 {
                    if hv >= 1 { 7 } else { 6 }
                } else if d == 1 {
                    if hv >= 2 {
                        5
                    } else if hv == 1 {
                        4
                    } else {
                        3
                    }
                } else if hv >= 2 {
                    2
                } else if hv == 1 {
                    1
                } else {
                    0
                }
            } else {
                if orient == 1 {
                    let x = h;
                    h = v;
                    v = x;
                }
                if h == 2 {
                    8
                } else if h == 1 {
                    if v >= 1 {
                        7
                    } else if d >= 1 {
                        6
                    } else {
                        5
                    }
                } else if v == 2 {
                    4
                } else if v == 1 {
                    3
                } else if d >= 2 {
                    2
                } else if d == 1 {
                    1
                } else {
                    0
                }
            };
            t[orient][nb] = ctx;
            nb += 1;
        }
        orient += 1;
    }
    t
};

/// What the straight neighbour whose significance bit is `bit` of `i` (its sign bit is 4 higher) adds to the sum of a
/// direction: 0 if it is not significant, else +1 or -1.
const fn contribution(i: usize, bit: usize) -> i32 {
    if (i >> bit) & 1 == 0 {
        0
    } else if (i >> (bit + 4)) & 1 == 1 {
        -1
    } else {
        1
    }
}

/// The sign contexts of Table D.3, by the significance bits (N, S, W, E) and sign bits of the straight neighbours:
/// the context, plus 16 when the decoded bit is to be flipped.
#[allow(clippy::indexing_slicing)] // evaluated at compile time
const SIGN_LUT: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut i = 0usize;
    while i < 256 {
        let hs = contribution(i, 2) + contribution(i, 3);
        let vs = contribution(i, 0) + contribution(i, 1);
        let h = if hs > 1 { 1 } else if hs < -1 { -1 } else { hs };
        let v = if vs > 1 { 1 } else if vs < -1 { -1 } else { vs };
        let (ctx, flip) = match (h, v) {
            (1, 1) => (13, 0),
            (1, 0) => (12, 0),
            (1, -1) => (11, 0),
            (0, 1) => (10, 0),
            (0, 0) => (9, 0),
            (0, -1) => (10, 16),
            (-1, 1) => (11, 16),
            (-1, 0) => (12, 16),
            _ => (13, 16),
        };
        t[i] = ctx + flip;
        i += 1;
    }
    t
};

/// What decodes the decisions of a pass: the MQ decoder, or raw bits (the bypass mode).
trait Coder {
    fn bit(&mut self, cx: &mut [u8; 19], i: usize) -> u32;
    /// The sign of a sample that has become significant: `true` for negative.
    fn sign(&mut self, cx: &mut [u8; 19], flags: u16) -> bool;
}

impl Coder for Mq<'_> {
    #[inline(always)]
    fn bit(&mut self, cx: &mut [u8; 19], i: usize) -> u32 {
        match cx.get_mut(i) {
            Some(c) => self.decode(c),
            None => 0,
        }
    }
    #[inline(always)]
    fn sign(&mut self, cx: &mut [u8; 19], flags: u16) -> bool {
        let entry = SIGN_LUT.get(usize::from((flags & 0xF) | ((flags >> 4) & 0xF0))).copied().unwrap_or(9);
        let bit = self.bit(cx, usize::from(entry & 15));
        (bit ^ u32::from(entry >> 4)) == 1
    }
}

/// Raw bits of a bypassed segment: bytes with the bit stuffing of packet headers (a byte after 0xFF has seven bits);
/// past the end, 1-bits.
struct Raw<'a> {
    data: &'a [u8],
    pos: usize,
    cur: u8,
    left: u8,
    prev_ff: bool,
}

impl<'a> Raw<'a> {
    fn new(data: &'a [u8]) -> Raw<'a> {
        Raw { data, pos: 0, cur: 0, left: 0, prev_ff: false }
    }

    fn next(&mut self) -> u32 {
        if self.left == 0 {
            let b = self.data.get(self.pos).copied();
            self.pos += 1;
            let b = b.unwrap_or(0xFF);
            self.left = if self.prev_ff && self.pos <= self.data.len() { 7 } else { 8 };
            self.prev_ff = b == 0xFF;
            self.cur = b;
        }
        self.left -= 1;
        u32::from((self.cur >> self.left) & 1)
    }
}

impl Coder for Raw<'_> {
    #[inline(always)]
    fn bit(&mut self, _cx: &mut [u8; 19], _i: usize) -> u32 {
        self.next()
    }
    #[inline(always)]
    fn sign(&mut self, _cx: &mut [u8; 19], _flags: u16) -> bool {
        self.next() == 1
    }
}

/// One codeword segment of a code-block: its bytes, and how many coding passes they hold.
pub(super) struct Seg<'a> {
    pub data: &'a [u8],
    pub passes: usize,
}

/// A code-block to decode.
pub(super) struct Job<'a> {
    pub w: usize,
    pub h: usize,
    /// 0 LL, 1 HL, 2 LH, 3 HH.
    pub orient: u8,
    /// The bit-plane the first pass (a cleanup pass) codes; 0 is the least significant plane. At most 29.
    pub top_plane: i32,
    pub style: u8,
    pub segs: &'a [Seg<'a>],
}

/// The decoder's state; the buffers are kept from block to block.
///
/// Besides the flags of each sample there is a word for each column of a stripe (four samples one above the other):
/// four bits of "candidate" (not significant, and a neighbour is), four of "significant" and four of "visited in this
/// bit-plane's significance propagation pass". The passes read those words and only work on the samples they ask for,
/// so that a pass over a block that is mostly insignificant costs a look at each column, not at each sample.
pub(super) struct T1 {
    flags: Vec<u16>,
    cols: Vec<u16>,
    /// The magnitudes (one fractional bit) with [`NEGATIVE`] for the sign, `w` by `h`.
    pub data: Vec<u32>,
    cx: [u8; 19],
    w: usize,
    h: usize,
    stride: usize,
    lut: usize,
    causal: bool,
    /// Decisions made since they were last charged.
    n: u64,
}

fn fresh_contexts() -> [u8; 19] {
    let mut cx = [0u8; 19];
    if let Some(c) = cx.get_mut(0) {
        *c = context(4, 0);
    }
    if let Some(c) = cx.get_mut(CTX_RUN) {
        *c = context(3, 0);
    }
    if let Some(c) = cx.get_mut(CTX_UNI) {
        *c = context(46, 0);
    }
    cx
}

/// The parts of a column word.
const CAND: u16 = 0x000F;
const SIGN_COL: u16 = 0x00F0;
const VISIT_COL: u16 = 0x0F00;

impl T1 {
    pub fn new() -> T1 {
        T1 { flags: Vec::new(), cols: Vec::new(), data: Vec::new(), cx: fresh_contexts(), w: 0, h: 0, stride: 0, lut: 0, causal: false, n: 0 }
    }

    /// Decode the block into `self.data`. `Ok(true)`: the data was damaged (a segmentation symbol that is wrong, or
    /// the arithmetic decoder ran dry); what was decoded is kept. `Err`: the page's work is used up.
    pub fn decode(&mut self, ctx: &Ctx, job: &Job<'_>) -> Result<bool> {
        let (w, h) = (job.w, job.h);
        self.w = w;
        self.h = h;
        self.stride = w + 2;
        self.lut = usize::from(job.orient & 3);
        self.causal = job.style & CAUSAL != 0;
        self.flags.clear();
        self.flags.resize((w + 2) * (h + 2), 0);
        self.cols.clear();
        self.cols.resize(h.div_ceil(4) * w, 0);
        self.data.clear();
        self.data.resize(w * h, 0);
        self.cx = fresh_contexts();
        self.n = 0;
        let samples = (w * h) as f64;
        let mut damaged = false;
        let mut pass = 0usize;
        'segments: for seg in job.segs {
            let first_kind = kind_of(pass);
            let raw = job.style & BYPASS != 0 && pass >= 10 && first_kind != 2;
            let mut mq = Mq::new(seg.data);
            let mut rawr = Raw::new(seg.data);
            for _ in 0..seg.passes {
                let plane = job.top_plane - pass.div_ceil(3) as i32;
                if plane < 0 {
                    break 'segments;
                }
                let p = plane as u32;
                ctx.charge(cost::JPX_PASS + samples * cost::JPX_PASS_SAMPLE)?;
                match (kind_of(pass), raw) {
                    (0, true) => self.sigprop(&mut rawr, p),
                    (1, true) => self.magref(&mut rawr, p),
                    (0, false) => self.sigprop(&mut mq, p),
                    (1, false) => self.magref(&mut mq, p),
                    _ => {
                        self.cleanup(&mut mq, p);
                        if job.style & SEGSYM != 0 {
                            let mut v = 0;
                            for _ in 0..4 {
                                v = (v << 1) | mq.bit(&mut self.cx, CTX_UNI);
                                self.n += 1;
                            }
                            if v != 0xA {
                                damaged = true;
                            }
                        }
                    }
                }
                ctx.spend(self.n as f64 * cost::JPX_DECISION);
                self.n = 0;
                if job.style & RESET != 0 {
                    self.cx = fresh_contexts();
                }
                pass += 1;
                if !raw && mq.exhausted() {
                    damaged = true;
                    break 'segments;
                }
            }
        }
        Ok(damaged)
    }

    /// A sample has become significant: tell its neighbours (their flags, and their column words).
    #[inline]
    fn set_significant(&mut self, x: usize, y: usize, neg: bool) {
        let (s, w, h) = (self.stride, self.w, self.h);
        let fi = (y + 1) * s + x + 1;
        let sign = |bit: u16| if neg { bit } else { 0 };
        if let Some(f) = self.flags.get_mut(fi) {
            *f |= SIG;
        }
        if let Some(f) = self.flags.get_mut(fi - 1) {
            *f |= E | sign(E_NEG);
        }
        if let Some(f) = self.flags.get_mut(fi + 1) {
            *f |= W | sign(W_NEG);
        }
        if let Some([a, b, c]) = self.flags.get_mut(fi + s - 1..fi + s + 2) {
            *a |= NE;
            *b |= N | sign(N_NEG);
            *c |= NW;
        }
        // The last row of the stripe above must not see what is in the stripe below, in the causal mode.
        let above = !(self.causal && y & 3 == 0);
        if above && let Some([a, b, c]) = self.flags.get_mut(fi - s - 1..fi - s + 2) {
            *a |= SE;
            *b |= S | sign(S_NEG);
            *c |= SW;
        }
        // The column words: this sample is significant now, and each neighbour it can be seen from is a candidate.
        if let Some(col) = self.cols.get_mut((y / 4) * w + x) {
            *col = (*col | (1 << (4 + (y & 3)))) & !(1 << (y & 3));
        }
        let (x0, x1) = (x.saturating_sub(1), (x + 1).min(w - 1));
        let y0 = if y > 0 && above { y - 1 } else { y };
        for ny in y0..=(y + 1).min(h - 1) {
            for nx in x0..=x1 {
                if (nx, ny) != (x, y)
                    && let Some(col) = self.cols.get_mut((ny / 4) * w + nx)
                {
                    *col |= 1 << (ny & 3);
                }
            }
        }
    }

    /// Code a sample that is not significant with the context its neighbours give.
    #[inline(always)]
    fn code_sample<C: Coder>(&mut self, c: &mut C, x: usize, y: usize, p: u32) {
        let fi = (y + 1) * self.stride + x + 1;
        let f = self.flags.get(fi).copied().unwrap_or(0);
        let ctx = SIG_LUT.get(self.lut).and_then(|t| t.get(usize::from(f & NB))).copied().unwrap_or(0);
        self.n += 1;
        if c.bit(&mut self.cx, usize::from(ctx)) == 1 {
            self.n += 1;
            let neg = c.sign(&mut self.cx, f);
            if let Some(d) = self.data.get_mut(y * self.w + x) {
                *d = (3u32 << p) | if neg { NEGATIVE } else { 0 };
            }
            self.set_significant(x, y, neg);
        }
    }

    fn sigprop<C: Coder>(&mut self, c: &mut C, p: u32) {
        let (w, h) = (self.w, self.h);
        for stripe in 0..h.div_ceil(4) {
            for x in 0..w {
                let ci = stripe * w + x;
                let mut from = 0u16;
                loop {
                    // The samples that are candidates, not significant, not visited: read again after each one, since a
                    // sample that becomes significant makes the ones below it candidates.
                    let m = self.cols.get(ci).copied().unwrap_or(0);
                    let todo = m & CAND & !(m >> 4) & !(m >> 8) & (0xF << from);
                    if todo == 0 {
                        break;
                    }
                    let k = todo.trailing_zeros() as u16;
                    self.code_sample(c, x, stripe * 4 + usize::from(k), p);
                    if let Some(col) = self.cols.get_mut(ci) {
                        *col |= 1 << (8 + k);
                    }
                    from = k + 1;
                }
            }
        }
    }

    fn magref<C: Coder>(&mut self, c: &mut C, p: u32) {
        let (w, h, s) = (self.w, self.h, self.stride);
        for stripe in 0..h.div_ceil(4) {
            for x in 0..w {
                let m = self.cols.get(stripe * w + x).copied().unwrap_or(0);
                let mut todo = (m >> 4) & 0xF & !(m >> 8);
                while todo != 0 {
                    let k = todo.trailing_zeros() as usize;
                    todo &= todo - 1;
                    let y = stripe * 4 + k;
                    let fi = (y + 1) * s + x + 1;
                    let f = self.flags.get(fi).copied().unwrap_or(0);
                    let ctx = if f & REFINED != 0 {
                        16
                    } else if f & NB != 0 {
                        15
                    } else {
                        14
                    };
                    self.n += 1;
                    let bit = c.bit(&mut self.cx, ctx);
                    if let Some(d) = self.data.get_mut(y * w + x) {
                        let half = 1u32 << p;
                        *d = if bit == 1 { d.wrapping_add(half) } else { d.wrapping_sub(half) };
                    }
                    if let Some(f) = self.flags.get_mut(fi) {
                        *f |= REFINED;
                    }
                }
            }
        }
    }

    fn cleanup(&mut self, mq: &mut Mq<'_>, p: u32) {
        let (w, h) = (self.w, self.h);
        for stripe in 0..h.div_ceil(4) {
            let rows = (h - stripe * 4).min(4);
            let valid = (1u16 << rows) - 1;
            for x in 0..w {
                let m = self.cols.get(stripe * w + x).copied().unwrap_or(0);
                // The samples to code: not significant, not visited.
                let todo = !((m >> 4) | (m >> 8)) & valid;
                if todo == 0 {
                    continue;
                }
                let y0 = stripe * 4;
                let mut start = 0usize;
                // A whole stripe column that is insignificant, unvisited and without significant neighbours is coded in
                // one decision (the run-length mode, D.3.4).
                if rows == 4 && m & (CAND | SIGN_COL | VISIT_COL) == 0 {
                    self.n += 1;
                    if mq.bit(&mut self.cx, CTX_RUN) == 0 {
                        continue;
                    }
                    self.n += 2;
                    let r = ((mq.bit(&mut self.cx, CTX_UNI) << 1) | mq.bit(&mut self.cx, CTX_UNI)) as usize;
                    let y = y0 + r;
                    let fi = (y + 1) * self.stride + x + 1;
                    let f = self.flags.get(fi).copied().unwrap_or(0);
                    self.n += 1;
                    let neg = mq.sign(&mut self.cx, f);
                    if let Some(d) = self.data.get_mut(y * w + x) {
                        *d = (3u32 << p) | if neg { NEGATIVE } else { 0 };
                    }
                    self.set_significant(x, y, neg);
                    start = r + 1;
                }
                for k in start..rows {
                    if todo & (1 << k) != 0 {
                        self.code_sample(mq, x, y0 + k, p);
                    }
                }
            }
        }
        for col in &mut self.cols {
            *col &= !VISIT_COL;
        }
    }
}

/// 0 significance propagation, 1 magnitude refinement, 2 cleanup: the first pass of a block is a cleanup pass.
fn kind_of(pass: usize) -> u8 {
    if pass == 0 { 2 } else { ((pass - 1) % 3) as u8 }
}
