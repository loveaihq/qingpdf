//! A JBIG2 encoder for the tests: it makes the streams the decoder is tried on. It follows T.88 the plain way (each
//! context is read out of a table of the pixels it is made of, with `Bitmap::get`), not the way the decoder does it
//! with sliding windows, so that the two check each other; and the streams it makes are also given to PDFium (see
//! `docs/decisions.md`, 3c2-1) as a decoder written by somebody else.

#![allow(clippy::too_many_arguments, clippy::type_complexity, clippy::useless_vec, clippy::manual_is_multiple_of)]

use super::bitmap::{Bitmap, Op};
use super::huffman::Table;
use crate::render::mq::state_entry;

/// The MQ encoder (T.88 Annex E.2).
pub struct MqEnc {
    out: Vec<u8>,
    a: u32,
    c: u32,
    ct: u32,
}

impl MqEnc {
    pub fn new() -> MqEnc {
        // The first byte of `out` is the byte "before" the data; it is dropped at the end.
        MqEnc { out: vec![0], a: 0x8000, c: 0, ct: 12 }
    }

    fn byte_out(&mut self) {
        let b = *self.out.last().expect("a byte");
        if b == 0xFF {
            self.out.push((self.c >> 20) as u8);
            self.c &= 0xFFFFF;
            self.ct = 7;
        } else if self.c < 0x800_0000 {
            self.out.push((self.c >> 19) as u8);
            self.c &= 0x7FFFF;
            self.ct = 8;
        } else {
            *self.out.last_mut().expect("a byte") += 1;
            if *self.out.last().expect("a byte") == 0xFF {
                self.c &= 0x7FF_FFFF;
                self.out.push((self.c >> 20) as u8);
                self.c &= 0xFFFFF;
                self.ct = 7;
            } else {
                self.out.push((self.c >> 19) as u8);
                self.c &= 0x7FFFF;
                self.ct = 8;
            }
        }
    }

    fn renorm(&mut self) {
        loop {
            self.a <<= 1;
            self.c <<= 1;
            self.ct -= 1;
            if self.ct == 0 {
                self.byte_out();
            }
            if self.a & 0x8000 != 0 {
                break;
            }
        }
    }

    pub fn encode(&mut self, d: u32, cx: &mut u8) {
        let (qe, nmps, nlps, switch) = state_entry(*cx >> 1);
        let mut mps = u32::from(*cx & 1);
        let index;
        self.a -= qe;
        if d == mps {
            if self.a & 0x8000 == 0 {
                if self.a < qe {
                    self.a = qe;
                } else {
                    self.c += qe;
                }
                index = nmps;
                *cx = (index << 1) | mps as u8;
                self.renorm();
            } else {
                self.c += qe;
            }
        } else {
            if self.a < qe {
                self.c += qe;
            } else {
                self.a = qe;
            }
            if switch {
                mps = 1 - mps;
            }
            index = nlps;
            *cx = (index << 1) | mps as u8;
            self.renorm();
        }
    }

    /// FLUSH (E.2.9): the bytes, the marker `FF AC` last.
    pub fn finish(mut self) -> Vec<u8> {
        let temp = self.c + self.a;
        self.c |= 0xFFFF;
        if self.c >= temp {
            self.c -= 0x8000;
        }
        self.c <<= self.ct;
        self.byte_out();
        self.c <<= self.ct;
        self.byte_out();
        if *self.out.last().expect("a byte") != 0xFF {
            self.out.push(0xFF);
        }
        self.out.push(0xAC);
        self.out.remove(0);
        self.out
    }
}

/// An integer (A.2): `None` is the out-of-band value.
pub fn enc_int(mq: &mut MqEnc, cx: &mut [u8], v: Option<i64>) {
    let mut prev = 1usize;
    let mut emit = |mq: &mut MqEnc, bit: u32| {
        mq.encode(bit, &mut cx[prev]);
        prev = if prev < 256 { (prev << 1) | bit as usize } else { (((prev << 1) | bit as usize) & 511) | 256 };
    };
    let (sign, mag) = match v {
        None => (1, 0),
        Some(v) if v < 0 => (1, -v),
        Some(v) => (0, v),
    };
    emit(mq, sign);
    let (prefix, nbits, offset): (&[u32], u32, i64) = if mag < 4 {
        (&[0], 2, 0)
    } else if mag < 20 {
        (&[1, 0], 4, 4)
    } else if mag < 84 {
        (&[1, 1, 0], 6, 20)
    } else if mag < 340 {
        (&[1, 1, 1, 0], 8, 84)
    } else if mag < 4436 {
        (&[1, 1, 1, 1, 0], 12, 340)
    } else {
        (&[1, 1, 1, 1, 1], 32, 4436)
    };
    for &b in prefix {
        emit(mq, b);
    }
    let rest = mag - offset;
    for i in (0..nbits).rev() {
        emit(mq, ((rest >> i) & 1) as u32);
    }
}

/// A symbol number with `len` bits (A.3).
pub fn enc_iaid(mq: &mut MqEnc, cx: &mut [u8], id: usize, len: u32) {
    let mut prev = 1usize;
    for i in (0..len).rev() {
        let bit = (id >> i) & 1;
        mq.encode(bit as u32, &mut cx[prev]);
        prev = (prev << 1) | bit;
    }
}

/// Bits for Huffman coded data.
#[derive(Default)]
pub struct BitWriter {
    pub bytes: Vec<u8>,
    bits: usize,
}

impl BitWriter {
    pub fn bit(&mut self, b: u32) {
        if self.bits % 8 == 0 {
            self.bytes.push(0);
        }
        if b != 0 {
            *self.bytes.last_mut().expect("a byte") |= 0x80 >> (self.bits % 8);
        }
        self.bits += 1;
    }

    pub fn bits(&mut self, v: u64, n: u32) {
        for i in (0..n).rev() {
            self.bit(((v >> i) & 1) as u32);
        }
    }

    pub fn align(&mut self) {
        self.bits = self.bits.div_ceil(8) * 8;
    }

    pub fn put_bytes(&mut self, data: &[u8]) {
        self.align();
        self.bytes.extend_from_slice(data);
        self.bits = self.bytes.len() * 8;
    }

    pub fn value(&mut self, t: &Table, v: Option<i64>) {
        let (code, len, extra, extra_len) = t.encode(v).expect("a value the table can code");
        self.bits(u64::from(code), len);
        self.bits(extra, extra_len);
    }
}

// ---- generic regions --------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum P {
    Fixed(i64, i64),
    At(usize),
}

fn template_pixels(t: u8) -> Vec<P> {
    use P::{At, Fixed};
    match t {
        0 => vec![
            At(3), Fixed(-1, -2), Fixed(0, -2), Fixed(1, -2), At(2), At(1), Fixed(-2, -1), Fixed(-1, -1), Fixed(0, -1), Fixed(1, -1), Fixed(2, -1), At(0), Fixed(-4, 0),
            Fixed(-3, 0), Fixed(-2, 0), Fixed(-1, 0),
        ],
        1 => vec![
            Fixed(-1, -2), Fixed(0, -2), Fixed(1, -2), Fixed(2, -2), Fixed(-2, -1), Fixed(-1, -1), Fixed(0, -1), Fixed(1, -1), Fixed(2, -1), At(0), Fixed(-3, 0),
            Fixed(-2, 0), Fixed(-1, 0),
        ],
        2 => vec![Fixed(-1, -2), Fixed(0, -2), Fixed(1, -2), Fixed(-2, -1), Fixed(-1, -1), Fixed(0, -1), Fixed(1, -1), At(0), Fixed(-2, 0), Fixed(-1, 0)],
        _ => vec![Fixed(-3, -1), Fixed(-2, -1), Fixed(-1, -1), Fixed(0, -1), Fixed(1, -1), At(0), Fixed(-4, 0), Fixed(-3, 0), Fixed(-2, 0), Fixed(-1, 0)],
    }
}

pub const SLTP: [usize; 4] = [0x9B25, 0x0795, 0x00E5, 0x0195];

pub fn nominal_at(t: u8) -> [(i8, i8); 4] {
    if t == 0 { [(3, -1), (-3, -1), (2, -2), (-2, -2)] } else if t == 1 { [(3, -1), (0, 0), (0, 0), (0, 0)] } else { [(2, -1), (0, 0), (0, 0), (0, 0)] }
}

fn generic_context(t: u8, at: &[(i8, i8); 4], bm: &Bitmap, x: i64, y: i64) -> usize {
    template_pixels(t).iter().fold(0usize, |c, p| {
        let (dx, dy) = match *p {
            P::Fixed(dx, dy) => (dx, dy),
            P::At(i) => (i64::from(at[i].0), i64::from(at[i].1)),
        };
        (c << 1) | usize::from(bm.get(x + dx, y + dy))
    })
}

/// Encode `bm` as the pixels of a generic region (6.2).
pub fn encode_generic(mq: &mut MqEnc, cx: &mut [u8], bm: &Bitmap, template: u8, tpgdon: bool, at: [(i8, i8); 4], skip: Option<&Bitmap>) {
    let mut ltp = false;
    for y in 0..bm.h as i64 {
        if tpgdon {
            let same = (0..bm.w as i64).all(|x| bm.get(x, y) == bm.get(x, y - 1));
            mq.encode(u32::from(same ^ ltp), &mut cx[SLTP[usize::from(template)]]);
            ltp = same;
            if same {
                continue;
            }
        }
        for x in 0..bm.w as i64 {
            if skip.is_some_and(|s| s.get(x, y) != 0) {
                assert_eq!(bm.get(x, y), 0, "a skipped pixel must be white");
                continue;
            }
            let c = generic_context(template, &at, bm, x, y);
            mq.encode(u32::from(bm.get(x, y)), &mut cx[c]);
        }
    }
}

fn refine_context(template: u8, at: [(i8, i8); 2], cur: &Bitmap, r: &Bitmap, x: i64, y: i64, dx: i64, dy: i64) -> usize {
    let c = |ox: i64, oy: i64| usize::from(cur.get(x + ox, y + oy));
    let g = |ox: i64, oy: i64| usize::from(r.get(x - dx + ox, y - dy + oy));
    let bits: Vec<usize> = if template == 0 {
        vec![
            c(0, -1), c(1, -1), c(-1, 0), c(i64::from(at[0].0), i64::from(at[0].1)), g(0, -1), g(1, -1), g(-1, 0), g(0, 0), g(1, 0), g(-1, 1), g(0, 1), g(1, 1),
            g(i64::from(at[1].0), i64::from(at[1].1)),
        ]
    } else {
        vec![c(-1, -1), c(0, -1), c(1, -1), c(-1, 0), g(0, -1), g(-1, 0), g(0, 0), g(1, 0), g(0, 1), g(1, 1)]
    };
    bits.iter().fold(0, |a, &b| (a << 1) | b)
}

/// Encode `bm` as a refinement of `r` (6.3).
#[allow(clippy::too_many_arguments)]
pub fn encode_refinement(mq: &mut MqEnc, cx: &mut [u8], bm: &Bitmap, r: &Bitmap, dx: i64, dy: i64, template: u8, tpgron: bool, at: [(i8, i8); 2]) {
    let sltp = if template == 0 { 0x20 } else { 0x08 };
    let mut ltp = false;
    for y in 0..bm.h as i64 {
        // The pixel that is given by its neighbourhood in the reference, if there is one.
        let implied = |x: i64| -> Option<u8> {
            let v = r.get(x - dx, y - dy);
            (-1..=1).all(|oy| (-1..=1).all(|ox| r.get(x - dx + ox, y - dy + oy) == v)).then_some(v)
        };
        let typical = tpgron && (0..bm.w as i64).all(|x| implied(x).is_none_or(|v| v == bm.get(x, y)));
        if tpgron {
            mq.encode(u32::from(typical ^ ltp), &mut cx[sltp]);
            ltp = typical;
        }
        for x in 0..bm.w as i64 {
            if ltp && implied(x).is_some() {
                continue;
            }
            let c = refine_context(template, at, bm, r, x, y, dx, dy);
            mq.encode(u32::from(bm.get(x, y)), &mut cx[c]);
        }
    }
}

// ---- pictures and segments ---------------------------------------------------------------------------------

/// A bitmap that has blobs, lines and speckle in it, the same for the same `seed`.
pub fn picture(w: usize, h: usize, seed: u32) -> Bitmap {
    let mut b = Bitmap::new(w, h, false);
    let mut s = seed.wrapping_mul(2_654_435_761).wrapping_add(12345);
    let mut next = move |n: usize| -> usize {
        s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (s >> 8) as usize % n.max(1)
    };
    for _ in 0..(w * h / 60).max(3) {
        let (x, y, rw, rh) = (next(w), next(h), 1 + next(7), 1 + next(5));
        for yy in y..(y + rh).min(h) {
            for xx in x..(x + rw).min(w) {
                b.set(xx, yy, 1);
            }
        }
    }
    for _ in 0..(h / 8).max(1) {
        let y = next(h);
        for xx in 0..w {
            b.set(xx, y, 1);
        }
    }
    for _ in 0..(w * h / 50) {
        let (x, y) = (next(w), next(h));
        b.set(x, y, u8::from(next(2) == 0));
    }
    b
}

pub fn same(a: &Bitmap, b: &Bitmap) -> bool {
    a.w == b.w && a.h == b.h && (0..a.h).all(|y| (0..a.w).all(|x| a.get(x as i64, y as i64) == b.get(x as i64, y as i64)))
}

pub fn segment(number: u32, kind: u8, refs: &[u32], data: &[u8]) -> Vec<u8> {
    let mut v = number.to_be_bytes().to_vec();
    v.push(kind);
    assert!(refs.len() <= 4);
    v.push((refs.len() as u8) << 5);
    let width = if number <= 256 { 1 } else if number <= 65536 { 2 } else { 4 };
    for r in refs {
        v.extend(&r.to_be_bytes()[4 - width..]);
    }
    v.push(1);
    v.extend((data.len() as u32).to_be_bytes());
    v.extend_from_slice(data);
    v
}

pub fn page_info(w: u32, h: u32) -> Vec<u8> {
    let mut d = w.to_be_bytes().to_vec();
    d.extend(h.to_be_bytes());
    d.extend([0; 8]);
    d.extend([0, 0, 0]);
    d
}

pub fn region_info(w: usize, h: usize, x: i64, y: i64, op: Op) -> Vec<u8> {
    let mut d = (w as u32).to_be_bytes().to_vec();
    d.extend((h as u32).to_be_bytes());
    d.extend((x as i32).to_be_bytes());
    d.extend((y as i32).to_be_bytes());
    d.push(match op {
        Op::Or => 0,
        Op::And => 1,
        Op::Xor => 2,
        Op::Xnor => 3,
        Op::Replace => 4,
    });
    d
}

/// A generic region segment's data.
pub fn generic_region(bm: &Bitmap, x: i64, y: i64, op: Op, template: u8, tpgdon: bool, at: [(i8, i8); 4]) -> Vec<u8> {
    let mut d = region_info(bm.w, bm.h, x, y, op);
    d.push((template << 1) | (u8::from(tpgdon) << 3));
    let n = if template == 0 { 4 } else { 1 };
    for a in at.iter().take(n) {
        d.push(a.0 as u8);
        d.push(a.1 as u8);
    }
    let mut mq = MqEnc::new();
    let mut cx = vec![0u8; 1 << 16];
    encode_generic(&mut mq, &mut cx, bm, template, tpgdon, at, None);
    d.extend(mq.finish());
    d
}

/// A whole stream: page information, then the segments.
pub fn stream(w: u32, h: u32, segs: Vec<Vec<u8>>) -> Vec<u8> {
    let mut v = segment(0, 48, &[], &page_info(w, h));
    for s in segs {
        v.extend(s);
    }
    v
}
