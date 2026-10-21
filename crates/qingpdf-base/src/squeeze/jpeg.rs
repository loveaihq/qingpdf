//! A baseline JPEG encoder (ITU-T T.81, sequential DCT, Huffman coding, 8 bits a sample), for the pictures that compression shrinks:
//! one component (grey) or three (YCbCr, the chrominance halved in both directions, 4:2:0). The quantisation tables of Annex K.1 are
//! scaled by the quality the way libjpeg does it, and the Huffman tables are made for the picture itself (Annex K.2), so a picture
//! is worked through twice: once for the quantised blocks and the symbols they need, once to write them.
//!
//! The forward transform is the AAN float one (the scaling folded into the quantiser). Everything is checked against the decoders
//! (`zune-jpeg` here, PyMuPDF and Pillow in `tests/tools/compress_check.py`).

use crate::error::{Error, Result};

/// A picture: `comps` (1 or 3) interleaved 8-bit samples for each of `w` by `h` pixels.
pub struct Raster<'a> {
    pub w: usize,
    pub h: usize,
    pub comps: usize,
    pub data: &'a [u8],
}

/// Most pixels along a side: what libjpeg reads (`JPEG_MAX_DIMENSION`).
pub const MAX_SIDE: usize = 65_500;

/// Most pixels a picture may have here: its quantised blocks (3 bytes for each pixel of a colour picture) are held until it is written.
pub const MAX_PIXELS: usize = 24 << 20;

/// Annex K.1, Tables K.1 and K.2, in natural (row by row) order.
const LUMA: [u16; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56, 14, 17, 22, 29, 51, 87, 80, 62, 18,
    22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113, 92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
];
const CHROMA: [u16; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99, 99, 47, 66, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
];

/// Figure A.6: the place in a block (row by row) of the k-th coefficient of the zigzag order.
const ZIGZAG: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56,
    57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// The scale factors of the AAN transform for each of the eight frequencies.
const AAN: [f32; 8] = [1.0, 1.387_039_8, 1.306_563, 1.175_875_6, 1.0, 0.785_694_96, 0.541_196_1, 0.275_899_38];

/// A quantisation table for `quality` (1 to 100), in natural order: the scaling libjpeg has (`jpeg_quality_scaling`).
fn scaled(base: &[u16; 64], quality: u8) -> [u16; 64] {
    let q = u32::from(quality.clamp(1, 100));
    let scale = if q < 50 { 5000 / q } else { 200 - 2 * q };
    base.map(|b| ((u32::from(b) * scale + 50) / 100).clamp(1, 255) as u16)
}

/// What a coefficient is multiplied by to be quantised: the divisor, the scale of the transform and the factor 8 of its normalisation.
fn reciprocals(table: &[u16; 64]) -> [f32; 64] {
    let mut out = [0.0f32; 64];
    for (i, (slot, &q)) in out.iter_mut().zip(table).enumerate() {
        let (row, col) = (i / 8, i % 8);
        let s = AAN.get(row).copied().unwrap_or(1.0) * AAN.get(col).copied().unwrap_or(1.0) * 8.0;
        *slot = 1.0 / (f32::from(q) * s);
    }
    out
}

/// The AAN forward DCT of eight values (the scaled one of libjpeg's `jfdctflt`), in place.
fn dct8(v: &mut [f32; 8]) {
    let [d0, d1, d2, d3, d4, d5, d6, d7] = *v;
    let (t0, t7) = (d0 + d7, d0 - d7);
    let (t1, t6) = (d1 + d6, d1 - d6);
    let (t2, t5) = (d2 + d5, d2 - d5);
    let (t3, t4) = (d3 + d4, d3 - d4);
    // Even part.
    let (t10, t13) = (t0 + t3, t0 - t3);
    let (t11, t12) = (t1 + t2, t1 - t2);
    let z1 = (t12 + t13) * 0.707_106_77;
    // Odd part.
    let (o10, o11, o12) = (t4 + t5, t5 + t6, t6 + t7);
    let z5 = (o10 - o12) * 0.382_683_43;
    let z2 = 0.541_196_1 * o10 + z5;
    let z4 = 1.306_563 * o12 + z5;
    let z3 = o11 * 0.707_106_77;
    let (z11, z13) = (t7 + z3, t7 - z3);
    *v = [t10 + t11, z11 + z4, t13 + z1, z13 - z2, t10 - t11, z13 + z2, t13 - z1, z11 - z4];
}

/// The transform of a block of 64 samples (level shifted already), row by row in place.
fn dct_block(b: &mut [f32; 64]) {
    for row in b.as_chunks_mut::<8>().0 {
        dct8(row);
    }
    for col in 0..8 {
        let mut v = [0.0f32; 8];
        for (k, slot) in v.iter_mut().enumerate() {
            *slot = b.get(col + 8 * k).copied().unwrap_or(0.0);
        }
        dct8(&mut v);
        for (k, x) in v.into_iter().enumerate() {
            if let Some(slot) = b.get_mut(col + 8 * k) {
                *slot = x;
            }
        }
    }
}

/// The 64 quantised coefficients of a block, in zigzag order.
type Coefs = [i16; 64];

fn quantise(b: &[f32; 64], recip: &[f32; 64]) -> Coefs {
    let mut out = [0i16; 64];
    for (slot, &zz) in out.iter_mut().zip(&ZIGZAG) {
        let i = usize::from(zz);
        let x = b.get(i).copied().unwrap_or(0.0) * recip.get(i).copied().unwrap_or(0.0);
        *slot = x.round().clamp(-32767.0, 32767.0) as i16;
    }
    out
}

/// How many bits the magnitude of `v` takes (the category of Table F.1).
fn category(v: i16) -> u32 {
    16 - v.unsigned_abs().leading_zeros()
}

// --- the Huffman tables ---------------------------------------------------------------------------------------------

/// A table as DHT writes it: how many codes there are of each length 1 to 16, and the symbols in order of code length.
struct Spec {
    counts: [u8; 16],
    symbols: Vec<u8>,
}

/// How often each symbol is needed (the 257th is the reserved one of K.2).
type Freq = [u32; 257];

/// Annex K.2: the table that codes the symbols of `freq` in the fewest bits, with no code longer than 16 bits.
fn optimal(freq: &Freq) -> Spec {
    let mut f: [i64; 257] = [0; 257];
    for (slot, &n) in f.iter_mut().zip(freq) {
        *slot = i64::from(n);
    }
    if let Some(last) = f.get_mut(256) {
        *last = 1;
    }
    let mut size = [0u32; 257];
    let mut next = [-1i32; 257];
    loop {
        // The least used symbol (the highest number among equals) and the next least.
        let pick = |skip: Option<usize>| -> Option<usize> {
            let mut best: Option<usize> = None;
            for (i, &n) in f.iter().enumerate() {
                if n != 0 && Some(i) != skip && best.is_none_or(|b| n <= f.get(b).copied().unwrap_or(i64::MAX)) {
                    best = Some(i);
                }
            }
            best
        };
        let Some(c1) = pick(None) else { break };
        let Some(c2) = pick(Some(c1)) else { break };
        let merged = f.get(c1).copied().unwrap_or(0) + f.get(c2).copied().unwrap_or(0);
        if let Some(s) = f.get_mut(c1) {
            *s = merged;
        }
        if let Some(s) = f.get_mut(c2) {
            *s = 0;
        }
        for start in [c1, c2] {
            let mut at = start;
            loop {
                if let Some(s) = size.get_mut(at) {
                    *s += 1;
                }
                match next.get(at).copied().and_then(|n| usize::try_from(n).ok()) {
                    Some(n) => at = n,
                    None => break,
                }
            }
            if start == c1
                && let Some(n) = next.get_mut(at)
            {
                *n = c2 as i32;
            }
        }
    }
    // How many codes of each length (at most 32 long, so far).
    let mut bits = [0u32; 33];
    for &s in &size {
        if s != 0
            && let Some(b) = bits.get_mut(s.min(32) as usize)
        {
            *b += 1;
        }
    }
    // Shorten the codes longer than 16 bits: two of the longest give way to one a bit shorter and one that takes the place of a code
    // shorter than that.
    for i in (17..=32).rev() {
        while bits.get(i).copied().unwrap_or(0) > 0 {
            let mut j = i - 2;
            while bits.get(j).copied().unwrap_or(1) == 0 {
                j -= 1;
            }
            for (at, change) in [(i, -2i32), (i - 1, 1), (j + 1, 2), (j, -1)] {
                if let Some(b) = bits.get_mut(at) {
                    *b = b.saturating_add_signed(change);
                }
            }
        }
    }
    // The reserved symbol takes the longest code left; it is not one of the symbols.
    if let Some(i) = (1..=16).rev().find(|&i| bits.get(i).copied().unwrap_or(0) > 0)
        && let Some(b) = bits.get_mut(i)
    {
        *b -= 1;
    }
    let mut counts = [0u8; 16];
    for (slot, &b) in counts.iter_mut().zip(bits.iter().skip(1)) {
        *slot = b.min(255) as u8;
    }
    let mut symbols = Vec::new();
    for len in 1..=32u32 {
        for (sym, &s) in size.iter().enumerate().take(256) {
            if s == len {
                symbols.push(sym as u8);
            }
        }
    }
    Spec { counts, symbols }
}

/// The codes of a table: for each symbol its code and its length in bits (0: the symbol has none).
struct Codes {
    code: [u16; 256],
    len: [u8; 256],
}

fn codes_of(spec: &Spec) -> Codes {
    let mut out = Codes { code: [0; 256], len: [0; 256] };
    let mut code = 0u32;
    let mut symbols = spec.symbols.iter();
    for (i, &n) in spec.counts.iter().enumerate() {
        for _ in 0..n {
            if let Some(&sym) = symbols.next()
                && let (Some(c), Some(l)) = (out.code.get_mut(usize::from(sym)), out.len.get_mut(usize::from(sym)))
            {
                *c = code as u16;
                *l = (i + 1) as u8;
            }
            code += 1;
        }
        code <<= 1;
    }
    out
}

// --- writing ----------------------------------------------------------------------------------------------------

/// The entropy-coded data: bits packed from the high end, with a zero byte after each 0xFF (B.1.1.5).
struct Bits {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bits {
    fn put(&mut self, code: u32, len: u32) {
        self.acc = (self.acc << len) | u64::from(code & ((1u32 << len) - 1));
        self.n += len;
        while self.n >= 8 {
            let byte = (self.acc >> (self.n - 8)) as u8;
            self.out.push(byte);
            if byte == 0xFF {
                self.out.push(0);
            }
            self.n -= 8;
        }
    }

    /// The last byte is filled up with ones (F.1.2.3).
    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            let pad = 8 - self.n;
            self.put((1 << pad) - 1, pad);
        }
        self.out
    }
}

/// The symbols one block needs: its DC difference category, and for each run of zeros and following value the run and category.
/// `each` is called with (is it AC, symbol, extra bits value, extra bits count).
fn symbols(block: &Coefs, pred: &mut i16, mut each: impl FnMut(bool, u8, u32, u32)) {
    let dc = block.first().copied().unwrap_or(0);
    let diff = dc.wrapping_sub(*pred);
    *pred = dc;
    let s = category(diff);
    let extra = if diff < 0 { u32::from((diff - 1) as u16) } else { u32::from(diff as u16) };
    each(false, s as u8, extra, s);
    let mut run = 0u32;
    let last = block.iter().rposition(|&v| v != 0).unwrap_or(0);
    for &v in block.iter().take(last + 1).skip(1) {
        if v == 0 {
            run += 1;
            continue;
        }
        while run > 15 {
            each(true, 0xF0, 0, 0);
            run -= 16;
        }
        let s = category(v);
        let extra = if v < 0 { u32::from((v - 1) as u16) } else { u32::from(v as u16) };
        each(true, ((run << 4) | s) as u8, extra, s);
        run = 0;
    }
    if last < 63 {
        each(true, 0x00, 0, 0);
    }
}

fn push_u16(out: &mut Vec<u8>, v: usize) {
    out.extend_from_slice(&u16::try_from(v).unwrap_or(u16::MAX).to_be_bytes());
}

fn dht(out: &mut Vec<u8>, class_and_id: u8, spec: &Spec) {
    out.extend_from_slice(&[0xFF, 0xC4]);
    push_u16(out, 2 + 1 + 16 + spec.symbols.len());
    out.push(class_and_id);
    out.extend_from_slice(&spec.counts);
    out.extend_from_slice(&spec.symbols);
}

fn dqt(out: &mut Vec<u8>, id: u8, table: &[u16; 64]) {
    out.extend_from_slice(&[0xFF, 0xDB]);
    push_u16(out, 2 + 1 + 64);
    out.push(id);
    out.extend(ZIGZAG.iter().map(|&z| table.get(usize::from(z)).copied().unwrap_or(1) as u8));
}

/// The samples of the pixel at (x, y), edges repeated outwards.
fn pixel<'a>(r: &'a Raster<'_>, x: usize, y: usize) -> &'a [u8] {
    let (x, y) = (x.min(r.w - 1), y.min(r.h - 1));
    let at = (y * r.w + x) * r.comps;
    r.data.get(at..at + r.comps).unwrap_or(&[])
}

/// The blocks of the picture in the order the scan has them (a grey picture: row by row; a colour one: four luma blocks, a Cb and a Cr
/// for each 16 by 16 pixels), quantised.
fn blocks(r: &Raster<'_>, luma: &[f32; 64], chroma: &[f32; 64]) -> Vec<Coefs> {
    let mut out = Vec::new();
    if r.comps == 1 {
        let (bw, bh) = (r.w.div_ceil(8), r.h.div_ceil(8));
        out.reserve(bw * bh);
        for by in 0..bh {
            for bx in 0..bw {
                let mut b = [0.0f32; 64];
                for (i, slot) in b.iter_mut().enumerate() {
                    *slot = f32::from(pixel(r, bx * 8 + i % 8, by * 8 + i / 8).first().copied().unwrap_or(0)) - 128.0;
                }
                dct_block(&mut b);
                out.push(quantise(&b, luma));
            }
        }
        return out;
    }
    let (mw, mh) = (r.w.div_ceil(16), r.h.div_ceil(16));
    out.reserve(mw * mh * 6);
    for my in 0..mh {
        for mx in 0..mw {
            let mut y = [[0.0f32; 64]; 4];
            let mut cb = [0.0f32; 64];
            let mut cr = [0.0f32; 64];
            for i in 0..256 {
                let (px, py) = (mx * 16 + i % 16, my * 16 + i / 16);
                let (rr, gg, bb) = match pixel(r, px, py) {
                    [a, b, c, ..] => (f32::from(*a), f32::from(*b), f32::from(*c)),
                    _ => (0.0, 0.0, 0.0),
                };
                let block = (i / 128) * 2 + (i % 16) / 8;
                let at = ((i / 16) % 8) * 8 + i % 8;
                if let Some(slot) = y.get_mut(block).and_then(|b| b.get_mut(at)) {
                    *slot = 0.299 * rr + 0.587 * gg + 0.114 * bb - 128.0;
                }
                // Each of the 64 chroma samples is the average of the four pixels over it.
                let c = ((i / 32) * 8) + (i % 16) / 2;
                if let (Some(u), Some(v)) = (cb.get_mut(c), cr.get_mut(c)) {
                    *u += (-0.168_736 * rr - 0.331_264 * gg + 0.5 * bb) * 0.25;
                    *v += (0.5 * rr - 0.418_688 * gg - 0.081_312 * bb) * 0.25;
                }
            }
            for mut b in y {
                dct_block(&mut b);
                out.push(quantise(&b, luma));
            }
            for mut b in [cb, cr] {
                dct_block(&mut b);
                out.push(quantise(&b, chroma));
            }
        }
    }
    out
}

/// Everything of the file up to the entropy-coded data: the markers, the quantisation tables (two for a colour picture), the frame, the Huffman
/// tables (one pair for each class: luminance, and chrominance for a colour picture) and the header of the scan.
fn header(out: &mut Vec<u8>, (w, h): (usize, usize), comps: usize, (qy, qc): (&[u16; 64], &[u16; 64]), (dc_spec, ac_spec): (&[Spec], &[Spec])) {
    let colour = comps == 3;
    out.extend_from_slice(&[0xFF, 0xD8]);
    // JFIF 1.01, no units, 1 by 1 and no thumbnail: what tells the decoders that three components are YCbCr.
    out.extend_from_slice(&[0xFF, 0xE0, 0, 16, b'J', b'F', b'I', b'F', 0, 1, 1, 0, 0, 1, 0, 1, 0, 0]);
    dqt(out, 0, qy);
    if colour {
        dqt(out, 1, qc);
    }
    out.extend_from_slice(&[0xFF, 0xC0]);
    push_u16(out, 8 + 3 * comps);
    out.push(8);
    push_u16(out, h);
    push_u16(out, w);
    out.push(comps as u8);
    if colour {
        out.extend_from_slice(&[1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
    } else {
        out.extend_from_slice(&[1, 0x11, 0]);
    }
    for (c, (d, a)) in dc_spec.iter().zip(ac_spec).enumerate() {
        dht(out, c as u8, d);
        dht(out, 0x10 | c as u8, a);
    }
    out.extend_from_slice(&[0xFF, 0xDA]);
    push_u16(out, 6 + 2 * comps);
    out.push(comps as u8);
    if colour {
        out.extend_from_slice(&[1, 0x00, 2, 0x11, 3, 0x11]);
    } else {
        out.extend_from_slice(&[1, 0x00]);
    }
    out.extend_from_slice(&[0, 63, 0]);
}

/// The picture as a baseline JPEG file, at `quality` (1 to 100: 75 is the usual one, 100 the best). `Err` for a picture with no pixels,
/// one with more than [`MAX_SIDE`] on a side or [`MAX_PIXELS`] in all, or other than one or three samples to a pixel.
pub fn encode(r: &Raster<'_>, quality: u8) -> Result<Vec<u8>> {
    let bad = |m: &str| Error::Invalid(format!("JPEG encoder: {m}"));
    // (65,500 is what libjpeg, which PDFium and most programs use, reads at the most; the format itself has room up to 65,535.)
    if r.w == 0 || r.h == 0 || r.w > MAX_SIDE || r.h > MAX_SIDE {
        return Err(bad("the picture has no pixels or is too wide or too high"));
    }
    if !matches!(r.comps, 1 | 3) {
        return Err(bad("one or three samples to a pixel only"));
    }
    if r.w.saturating_mul(r.h) > MAX_PIXELS || r.data.len() != r.w * r.h * r.comps {
        return Err(bad("the picture is too big or its samples are not as many as it says"));
    }
    let colour = r.comps == 3;
    let (qy, qc) = (scaled(&LUMA, quality), scaled(&CHROMA, quality));
    let all = blocks(r, &reciprocals(&qy), &reciprocals(&qc));
    // The first pass: which symbols there are. Per MCU the blocks are four luma and two chroma (or one luma).
    let per = if colour { 6 } else { 1 };
    let class = |i: usize| usize::from(colour && i % per >= 4);
    let comp_of = |i: usize| if colour { [0usize, 0, 0, 0, 1, 2].get(i % 6).copied().unwrap_or(0) } else { 0 };
    let mut dc: [Freq; 2] = [[0; 257], [0; 257]];
    let mut ac: [Freq; 2] = [[0; 257], [0; 257]];
    let mut pred = [0i16; 3];
    for (i, b) in all.iter().enumerate() {
        let (Some(dcf), Some(acf), Some(pr)) = (dc.get_mut(class(i)), ac.get_mut(class(i)), pred.get_mut(comp_of(i))) else { continue };
        symbols(b, pr, |is_ac, sym, _, _| {
            let f = if is_ac { &mut *acf } else { &mut *dcf };
            if let Some(n) = f.get_mut(usize::from(sym)) {
                *n += 1;
            }
        });
    }
    let nclass = if colour { 2 } else { 1 };
    let dc_spec: Vec<Spec> = dc.iter().take(nclass).map(optimal).collect();
    let ac_spec: Vec<Spec> = ac.iter().take(nclass).map(optimal).collect();
    let dc_codes: Vec<Codes> = dc_spec.iter().map(codes_of).collect();
    let ac_codes: Vec<Codes> = ac_spec.iter().map(codes_of).collect();

    // The file.
    let mut out = Vec::with_capacity(all.len() * 24 + 700);
    header(&mut out, (r.w, r.h), r.comps, (&qy, &qc), (&dc_spec, &ac_spec));

    // The second pass: the blocks, coded.
    let mut bits = Bits { out, acc: 0, n: 0 };
    let mut pred = [0i16; 3];
    for (i, b) in all.iter().enumerate() {
        let (Some(dcc), Some(acc), Some(pr)) = (dc_codes.get(class(i)), ac_codes.get(class(i)), pred.get_mut(comp_of(i))) else { continue };
        symbols(b, pr, |is_ac, sym, extra, n| {
            let t = if is_ac { acc } else { dcc };
            let (code, len) = (t.code.get(usize::from(sym)).copied().unwrap_or(0), t.len.get(usize::from(sym)).copied().unwrap_or(0));
            bits.put(u32::from(code), u32::from(len));
            if n > 0 {
                bits.put(extra, n);
            }
        });
    }
    let mut out = bits.finish();
    out.extend_from_slice(&[0xFF, 0xD9]);
    Ok(out)
}

/// The tables of a picture that is coded a band at a time: the frequencies are not known before the whole picture is seen, so the codes are made for
/// frequencies that fall away the way those of real pictures do (the end of a block, 0x00, most often, then the short runs of small values, the long
/// runs of large ones last). The file comes out a few per cent bigger than the one the real frequencies give ([`encode`]).
fn assumed(colour: bool) -> (Vec<Spec>, Vec<Spec>) {
    let mut dc: Freq = [0; 257];
    for (s, slot) in dc.iter_mut().enumerate().take(12) {
        *slot = 1 + ((1u32 << 12) >> s.abs_diff(2).min(12));
    }
    let mut ac: Freq = [0; 257];
    for (sym, slot) in ac.iter_mut().enumerate().take(256) {
        let (run, size) = (sym >> 4, sym & 15);
        // The symbols are the sizes 1 to 10 after each run of 0 to 15 zeros, and 0x00 and 0xF0.
        if (1..=10).contains(&size) {
            *slot = 1 + ((1u32 << 16) >> (size + run).min(16));
        }
    }
    if let Some(end) = ac.get_mut(0x00) {
        *end = 1 << 17;
    }
    if let Some(zeros) = ac.get_mut(0xF0) {
        *zeros = 8;
    }
    let classes = if colour { 2 } else { 1 };
    ((0..classes).map(|_| optimal(&dc)).collect(), (0..classes).map(|_| optimal(&ac)).collect())
}

/// A picture coded a band of rows at a time and written to `sink` as it goes, so that a picture of hundreds of megabytes is never held: only the
/// quantised blocks of one band and the compressed bytes not yet written. The Huffman codes are the same all through ([`assumed`]).
pub struct Banded<W: std::io::Write> {
    sink: W,
    w: usize,
    rows_left: usize,
    comps: usize,
    luma: [f32; 64],
    chroma: [f32; 64],
    dc: Vec<Codes>,
    ac: Vec<Codes>,
    bits: Bits,
    pred: [i16; 3],
}

impl<W: std::io::Write> Banded<W> {
    /// Start a picture of `w` by `h` pixels (`comps` samples a pixel, 1 or 3) at `quality`; the header is written now. The limits are those of
    /// [`encode`] on the sides (not on the pixels in all).
    pub fn new(mut sink: W, (w, h): (usize, usize), comps: usize, quality: u8) -> Result<Banded<W>> {
        if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE || !matches!(comps, 1 | 3) {
            return Err(Error::Invalid("JPEG encoder: the picture has no pixels, is too wide or too high, or has other than one or three samples to a pixel".to_string()));
        }
        let (qy, qc) = (scaled(&LUMA, quality), scaled(&CHROMA, quality));
        let (dc_spec, ac_spec) = assumed(comps == 3);
        let mut head = Vec::with_capacity(700);
        header(&mut head, (w, h), comps, (&qy, &qc), (&dc_spec, &ac_spec));
        sink.write_all(&head)?;
        Ok(Banded {
            sink,
            w,
            rows_left: h,
            comps,
            luma: reciprocals(&qy),
            chroma: reciprocals(&qc),
            dc: dc_spec.iter().map(codes_of).collect(),
            ac: ac_spec.iter().map(codes_of).collect(),
            bits: Bits { out: Vec::new(), acc: 0, n: 0 },
            pred: [0; 3],
        })
    }

    /// The next rows of the picture: `data` is whole rows of `comps` samples a pixel. Every band but the last has a number of rows that is a
    /// multiple of 16 (a colour picture) or of 8; anything else is an error.
    pub fn band(&mut self, data: &[u8]) -> Result<()> {
        let stride = self.w * self.comps;
        let rows = data.len() / stride.max(1);
        let unit = if self.comps == 3 { 16 } else { 8 };
        if data.len() != rows * stride || rows == 0 || rows > self.rows_left || (rows < self.rows_left && !rows.is_multiple_of(unit)) {
            return Err(Error::Invalid("JPEG encoder: a band is whole rows, a multiple of the block height unless it is the last".to_string()));
        }
        self.rows_left -= rows;
        let all = blocks(&Raster { w: self.w, h: rows, comps: self.comps, data }, &self.luma, &self.chroma);
        let per = if self.comps == 3 { 6 } else { 1 };
        for (i, b) in all.iter().enumerate() {
            let class = usize::from(self.comps == 3 && i % per >= 4);
            let comp = if self.comps == 3 { [0usize, 0, 0, 0, 1, 2].get(i % 6).copied().unwrap_or(0) } else { 0 };
            let (Some(dcc), Some(acc), Some(pr)) = (self.dc.get(class), self.ac.get(class), self.pred.get_mut(comp)) else { continue };
            let bits = &mut self.bits;
            symbols(b, pr, |is_ac, sym, extra, n| {
                let t = if is_ac { acc } else { dcc };
                bits.put(u32::from(t.code.get(usize::from(sym)).copied().unwrap_or(0)), u32::from(t.len.get(usize::from(sym)).copied().unwrap_or(0)));
                if n > 0 {
                    bits.put(extra, n);
                }
            });
        }
        self.sink.write_all(&self.bits.out)?;
        self.bits.out.clear();
        Ok(())
    }

    /// The end of the picture; the sink is given back. Rows missing: an error.
    pub fn finish(mut self) -> Result<W> {
        if self.rows_left != 0 {
            return Err(Error::Invalid("JPEG encoder: the picture was not all given".to_string()));
        }
        let mut tail = self.bits.finish();
        tail.extend_from_slice(&[0xFF, 0xD9]);
        self.sink.write_all(&tail)?;
        Ok(self.sink)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::image::jpeg_samples;

    /// Decode with the engine's decoder to the colour space of `comps`; `(width, height, samples)`.
    fn decode(jpeg: &[u8], comps: usize) -> (usize, usize, Vec<u8>) {
        jpeg_samples(jpeg, comps, usize::MAX).unwrap()
    }

    fn psnr(a: &[u8], b: &[u8]) -> f64 {
        let mse: f64 = a.iter().zip(b).map(|(&x, &y)| (f64::from(x) - f64::from(y)).powi(2)).sum::<f64>() / a.len() as f64;
        if mse == 0.0 { 99.0 } else { 10.0 * (255.0f64 * 255.0 / mse).log10() }
    }

    /// A smooth "photograph": gradients, a soft shape and a little noise (a fixed generator, so the numbers do not move).
    fn photo(w: usize, h: usize, comps: usize) -> Vec<u8> {
        let mut seed = 12345u32;
        let mut noise = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            f64::from(seed >> 24) / 255.0 - 0.5
        };
        let mut out = Vec::with_capacity(w * h * comps);
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f64 / w as f64, y as f64 / h as f64);
                let base = 110.0 + 70.0 * (fx * 6.0).sin() * (fy * 4.0).cos() + 40.0 * fx;
                let blob = 60.0 * (-(((fx - 0.6).powi(2) + (fy - 0.4).powi(2)) * 30.0)).exp();
                for c in 0..comps {
                    let v = base + blob * (1.0 + c as f64 * 0.3) + noise() * 6.0 + c as f64 * 15.0;
                    out.push(v.clamp(0.0, 255.0) as u8);
                }
            }
        }
        out
    }

    #[test]
    fn a_colour_picture_comes_back_at_the_same_size_and_close_to_what_it_was() {
        let (w, h) = (203, 157);
        let data = photo(w, h, 3);
        let jpeg = encode(&Raster { w, h, comps: 3, data: &data }, 75).unwrap();
        assert!(jpeg.starts_with(&[0xFF, 0xD8]) && jpeg.ends_with(&[0xFF, 0xD9]));
        let (dw, dh, back) = decode(&jpeg, 3);
        assert_eq!((dw, dh), (w, h));
        let p = psnr(&data, &back);
        assert!(p > 33.0, "PSNR {p}");
        assert!(jpeg.len() < data.len() / 4, "{} of {}", jpeg.len(), data.len());
    }

    #[test]
    fn a_grey_picture_comes_back_close_and_better_quality_is_closer_and_bigger() {
        let (w, h) = (96, 80);
        let data = photo(w, h, 1);
        let mut last = (0.0, 0usize);
        for q in [20u8, 50, 75, 95] {
            let jpeg = encode(&Raster { w, h, comps: 1, data: &data }, q).unwrap();
            let (dw, dh, back) = decode(&jpeg, 1);
            assert_eq!((dw, dh), (w, h));
            let p = psnr(&data, &back);
            assert!(p > last.0 && jpeg.len() > last.1, "q {q}: PSNR {p} ({} bytes) after {last:?}", jpeg.len());
            last = (p, jpeg.len());
        }
        assert!(last.0 > 38.0, "{last:?}");
    }

    #[test]
    fn odd_and_extreme_sizes_are_all_right() {
        for (w, h) in [(1, 1), (1, 65_500), (65_500, 1), (2, 3), (7, 9), (8, 8), (9, 17), (16, 16), (17, 15), (31, 1), (1, 33), (65, 66)] {
            for comps in [1, 3] {
                let data: Vec<u8> = (0..w * h * comps).map(|i| (i * 37 % 251) as u8).collect();
                let jpeg = encode(&Raster { w, h, comps, data: &data }, 75).unwrap();
                let (dw, dh, back) = decode(&jpeg, comps);
                assert_eq!((dw, dh, back.len()), (w, h, data.len()), "{w} by {h}, {comps}");
            }
        }
    }

    #[test]
    fn a_flat_picture_is_tiny_and_exact_and_the_extremes_do_not_overflow() {
        for v in [0u8, 128, 255] {
            let data = vec![v; 64 * 64 * 3];
            let jpeg = encode(&Raster { w: 64, h: 64, comps: 3, data: &data }, 90).unwrap();
            assert!(jpeg.len() < 700, "{}", jpeg.len());
            let (_, _, back) = decode(&jpeg, 3);
            assert!(back.iter().all(|&b| b.abs_diff(v) <= 2), "{v}");
        }
        // Black and white stripes at every size of period: the largest coefficients there are.
        let data: Vec<u8> = (0..64 * 64).map(|i| if (i % 64 + i / 64) % 2 == 0 { 0 } else { 255 }).collect();
        for q in [1u8, 50, 100] {
            let jpeg = encode(&Raster { w: 64, h: 64, comps: 1, data: &data }, q).unwrap();
            let (w, h, _) = decode(&jpeg, 1);
            assert_eq!((w, h), (64, 64));
        }
    }

    #[test]
    fn a_picture_coded_in_bands_comes_back_close_and_not_much_bigger_than_the_best_coding() {
        for (w, h, comps) in [(203usize, 157usize, 3usize), (320, 240, 1), (256, 200, 3), (33, 70, 1)] {
            let data = photo(w, h, comps);
            let best = encode(&Raster { w, h, comps, data: &data }, 85).unwrap();
            // Bands of 16 rows (32 for the second), the last one whatever is left.
            let unit = if comps == 3 { 16 } else { 8 };
            let mut banded = Banded::new(Vec::new(), (w, h), comps, 85).unwrap();
            let mut row = 0;
            while row < h {
                let rows = (unit * 2).min(h - row);
                banded.band(&data[row * w * comps..(row + rows) * w * comps]).unwrap();
                row += rows;
            }
            let jpeg = banded.finish().unwrap();
            let (dw, dh, back) = decode(&jpeg, comps);
            assert_eq!((dw, dh), (w, h));
            let (p, p_best) = (psnr(&data, &back), psnr(&data, &decode(&best, comps).2));
            assert!((p - p_best).abs() < 0.01, "{w}x{h}x{comps}: PSNR {p} against {p_best}");
            assert!(jpeg.len() < if w * h > 10_000 { best.len() * 13 / 10 } else { best.len() * 2 }, "{w}x{h}x{comps}: {} bytes against {}", jpeg.len(), best.len());
        }
    }

    #[test]
    fn bands_that_are_not_whole_blocks_or_not_all_there_are_refused() {
        let data = vec![9u8; 40 * 40 * 3];
        let mut b = Banded::new(Vec::new(), (40, 40), 3, 75).unwrap();
        assert!(b.band(&data[..40 * 3 * 10]).is_err(), "10 rows is not a multiple of 16");
        assert!(b.band(&data[..40 * 3 * 3 + 1]).is_err(), "not whole rows");
        assert!(b.band(&data[..0]).is_err());
        assert!(b.band(&vec![0u8; 40 * 3 * 48]).is_err(), "more rows than there are");
        b.band(&data[..40 * 3 * 16]).unwrap();
        assert!(b.finish().is_err(), "24 rows are missing");
        assert!(Banded::new(Vec::new(), (0, 5), 3, 75).is_err() && Banded::new(Vec::new(), (5, 5), 2, 75).is_err() && Banded::new(Vec::new(), (65_501, 5), 1, 75).is_err());
    }

    #[test]
    fn the_assumed_tables_have_every_symbol_a_block_can_need() {
        let (dc, ac) = assumed(true);
        assert_eq!((dc.len(), ac.len()), (2, 2));
        for spec in dc.iter().chain(&ac) {
            let kraft: f64 = spec.counts.iter().enumerate().map(|(i, &c)| f64::from(c) / 2f64.powi(i as i32 + 1)).sum();
            assert!(kraft < 1.0 && spec.counts.iter().map(|&c| usize::from(c)).sum::<usize>() == spec.symbols.len());
        }
        let has = |spec: &Spec, s: usize| spec.symbols.contains(&(s as u8));
        assert!((0..=11).all(|s| has(&dc[0], s)));
        assert!((0..16).all(|run| (1..=10).all(|size| has(&ac[0], run << 4 | size))) && has(&ac[0], 0) && has(&ac[0], 0xF0));
        assert_eq!(assumed(false).0.len(), 1);
    }

    #[test]
    fn what_cannot_be_encoded_is_refused_not_a_panic() {
        let one = [0u8; 3];
        assert!(encode(&Raster { w: 0, h: 1, comps: 1, data: &[] }, 75).is_err());
        assert!(encode(&Raster { w: 1, h: 0, comps: 1, data: &[] }, 75).is_err());
        assert!(encode(&Raster { w: 65_536, h: 1, comps: 1, data: &[] }, 75).is_err());
        // (More than libjpeg reads, though the format allows it.)
        assert!(encode(&Raster { w: 65_501, h: 1, comps: 1, data: &vec![0; 65_501] }, 75).is_err());
        assert!(encode(&Raster { w: 65_500, h: 1, comps: 1, data: &vec![0; 65_500] }, 75).is_ok());
        assert!(encode(&Raster { w: 1, h: 1, comps: 2, data: &one }, 75).is_err());
        assert!(encode(&Raster { w: 1, h: 1, comps: 3, data: &one[..2] }, 75).is_err());
        assert!(encode(&Raster { w: 8, h: 8, comps: 1, data: &one }, 75).is_err());
        // A quality outside 1 to 100 is taken as the nearest one.
        assert!(encode(&Raster { w: 1, h: 1, comps: 3, data: &one }, 0).is_ok());
        assert!(encode(&Raster { w: 1, h: 1, comps: 3, data: &one }, 255).is_ok());
    }

    #[test]
    fn the_huffman_tables_are_valid_codes_of_at_most_sixteen_bits() {
        // Frequencies that make a very deep tree (Fibonacci), then a flat one, then one symbol only.
        let mut fib: Freq = [0; 257];
        let (mut a, mut b) = (1u32, 1u32);
        for slot in fib.iter_mut().take(40) {
            *slot = a;
            (a, b) = (b, a.saturating_add(b).min(1 << 30));
        }
        let flat: Freq = {
            let mut f = [0; 257];
            f.iter_mut().take(256).for_each(|s| *s = 10);
            f
        };
        let mut single: Freq = [0; 257];
        single[7] = 1000;
        for f in [&fib, &flat, &single] {
            let spec = optimal(f);
            let n: usize = spec.counts.iter().map(|&c| usize::from(c)).sum();
            assert_eq!(n, spec.symbols.len());
            // Kraft: the codes fit in the code space.
            let kraft: f64 = spec.counts.iter().enumerate().map(|(i, &c)| f64::from(c) / 2f64.powi(i as i32 + 1)).sum();
            assert!(kraft < 1.0 + 1e-12, "{kraft}");
            let wanted = f.iter().take(256).filter(|&&x| x > 0).count();
            assert_eq!(spec.symbols.len(), wanted);
        }
    }
}
