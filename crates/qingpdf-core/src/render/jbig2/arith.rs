//! The arithmetic integer decoding procedures of JBIG2 (T.88 Annex A) on top of the MQ decoder.

use crate::render::mq::Mq;

/// The 512 contexts of one integer procedure (IADH, IADW, IAFS, ...; A.2).
pub(super) struct IntCtx(Vec<u8>);

impl IntCtx {
    pub fn new() -> IntCtx {
        IntCtx(vec![0; 512])
    }

    /// Decode one integer (A.2); `None` is the out-of-band value.
    pub fn decode(&mut self, mq: &mut Mq<'_>) -> Option<i64> {
        let mut prev = 1usize;
        let mut bit = |mq: &mut Mq<'_>| -> u32 {
            let d = match self.0.get_mut(prev) {
                Some(cx) => mq.decode(cx),
                None => 0,
            };
            prev = if prev < 256 { (prev << 1) | d as usize } else { (((prev << 1) | d as usize) & 511) | 256 };
            d
        };
        let sign = bit(mq);
        let (nbits, offset) = if bit(mq) == 0 {
            (2, 0)
        } else if bit(mq) == 0 {
            (4, 4)
        } else if bit(mq) == 0 {
            (6, 20)
        } else if bit(mq) == 0 {
            (8, 84)
        } else if bit(mq) == 0 {
            (12, 340)
        } else {
            (32, 4436)
        };
        let mut v = 0i64;
        for _ in 0..nbits {
            v = (v << 1) | i64::from(bit(mq));
        }
        v += offset;
        match (sign, v) {
            (0, v) => Some(v),
            (_, 0) => None,
            (_, v) => Some(-v),
        }
    }
}

/// The symbol-ID procedure (A.3): `len` bits, with a context for each prefix. `cx` holds `2^(len + 1)` contexts.
pub(super) fn decode_iaid(mq: &mut Mq<'_>, cx: &mut [u8], len: u32) -> usize {
    let mut prev = 1usize;
    for _ in 0..len {
        let d = match cx.get_mut(prev) {
            Some(c) => mq.decode(c) as usize,
            None => 0,
        };
        prev = (prev << 1) | d;
    }
    prev - (1usize << len)
}
