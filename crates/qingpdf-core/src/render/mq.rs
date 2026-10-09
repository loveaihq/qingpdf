//! The MQ arithmetic decoder: ITU-T T.88 (JBIG2) Annex E, the same coder as ITU-T T.800 (JPEG 2000) Annex C.
//! JBIG2 ([`super::jbig2`]) uses it for generic and refinement regions, symbol dictionaries and text regions;
//! the JPEG 2000 decoder ([`super::jpx`]) uses it for the code-block passes.
//!
//! A context is one byte: `state index << 1 | MPS`, so a fresh context is `0` (state 0, MPS 0); JPEG 2000 starts a
//! few of its contexts in other states and says so with [`context`].
//!
//! The decoder never fails and never reads past its data: where the data ends (or at a marker, a byte `0xFF`
//! followed by more than `0x8F`) it goes on with 1-bits, as the standard says. [`Mq::past_end`] counts how many
//! bytes' worth of those it has taken, so that a caller can tell a stream that has run dry from one that is just
//! coded in few bits (a blank page is a few dozen bytes).

/// Probability estimate, next state after an MPS, next state after an LPS, and whether an LPS switches the MPS
/// (T.88 Table E.1).
#[derive(Clone, Copy)]
struct Entry {
    qe: u32,
    nmps: u8,
    nlps: u8,
    switch: bool,
}

const fn e(qe: u32, nmps: u8, nlps: u8, switch: u8) -> Entry {
    Entry { qe, nmps, nlps, switch: switch != 0 }
}

/// 47 states; the table is padded to 128 so that a state byte can index it without a check.
#[allow(clippy::indexing_slicing)] // evaluated at compile time: a mistake would not build
const TABLE: [Entry; 128] = {
    let states = [
        e(0x5601, 1, 1, 1),
        e(0x3401, 2, 6, 0),
        e(0x1801, 3, 9, 0),
        e(0x0AC1, 4, 12, 0),
        e(0x0521, 5, 29, 0),
        e(0x0221, 38, 33, 0),
        e(0x5601, 7, 6, 1),
        e(0x5401, 8, 14, 0),
        e(0x4801, 9, 14, 0),
        e(0x3801, 10, 14, 0),
        e(0x3001, 11, 17, 0),
        e(0x2401, 12, 18, 0),
        e(0x1C01, 13, 20, 0),
        e(0x1601, 29, 21, 0),
        e(0x5601, 15, 14, 1),
        e(0x5401, 16, 14, 0),
        e(0x5101, 17, 15, 0),
        e(0x4801, 18, 16, 0),
        e(0x3801, 19, 17, 0),
        e(0x3401, 20, 18, 0),
        e(0x3001, 21, 19, 0),
        e(0x2801, 22, 19, 0),
        e(0x2401, 23, 20, 0),
        e(0x2201, 24, 21, 0),
        e(0x1C01, 25, 22, 0),
        e(0x1801, 26, 23, 0),
        e(0x1601, 27, 24, 0),
        e(0x1401, 28, 25, 0),
        e(0x1201, 29, 26, 0),
        e(0x1101, 30, 27, 0),
        e(0x0AC1, 31, 28, 0),
        e(0x09C1, 32, 29, 0),
        e(0x08A1, 33, 30, 0),
        e(0x0521, 34, 31, 0),
        e(0x0441, 35, 32, 0),
        e(0x02A1, 36, 33, 0),
        e(0x0221, 37, 34, 0),
        e(0x0141, 38, 35, 0),
        e(0x0111, 39, 36, 0),
        e(0x0085, 40, 37, 0),
        e(0x0049, 41, 38, 0),
        e(0x0025, 42, 39, 0),
        e(0x0015, 43, 40, 0),
        e(0x0009, 44, 41, 0),
        e(0x0005, 45, 42, 0),
        e(0x0001, 45, 43, 0),
        e(0x5601, 46, 46, 0),
    ];
    let mut table = [states[46]; 128];
    let mut i = 0;
    while i < 47 {
        table[i] = states[i];
        i += 1;
    }
    table
};

/// The entry of state `index`: Qe, next state after an MPS, after an LPS, and whether an LPS switches the MPS (the
/// tests encode with the same table).
#[cfg(test)]
pub(crate) fn state_entry(index: u8) -> (u32, u8, u8, bool) {
    let e = TABLE.get(usize::from(index) & 127).copied().unwrap_or(TABLE[0]);
    (e.qe, e.nmps, e.nlps, e.switch)
}

/// The context byte for state `index` (0 to 46) with `mps` as the more probable symbol.
pub(crate) fn context(index: u8, mps: u8) -> u8 {
    (index.min(46) << 1) | (mps & 1)
}

/// How many bytes' worth of fill (past the end of the data, or at a marker) a decoder may take before the data is
/// called used up. A well-formed stream needs two or three; a long run of nearly certain decisions needs none.
pub(crate) const MAX_PAST_END: u32 = 24;

pub(crate) struct Mq<'a> {
    data: &'a [u8],
    bp: usize,
    c: u32,
    a: u32,
    ct: u32,
    past_end: u32,
}

impl<'a> Mq<'a> {
    /// INITDEC (T.88 E.3.5).
    pub fn new(data: &'a [u8]) -> Mq<'a> {
        let mut mq = Mq { data, bp: 0, c: 0, a: 0x8000, ct: 0, past_end: 0 };
        mq.c = u32::from(mq.at(0)) << 16;
        mq.byte_in();
        mq.c <<= 7;
        mq.ct = mq.ct.saturating_sub(7);
        mq
    }

    #[inline(always)]
    fn at(&self, i: usize) -> u8 {
        self.data.get(i).copied().unwrap_or(0xFF)
    }

    /// BYTEIN (E.3.4).
    #[inline(never)]
    fn byte_in(&mut self) {
        if self.at(self.bp) == 0xFF {
            let next = self.at(self.bp + 1);
            if next > 0x8F {
                // A marker or the end of the data: 1-bits for ever.
                self.c += 0xFF00;
                self.ct = 8;
                self.past_end += 1;
            } else {
                self.bp += 1;
                self.c += u32::from(next) << 9;
                self.ct = 7;
            }
        } else {
            self.bp += 1;
            self.c += u32::from(self.at(self.bp)) << 8;
            self.ct = 8;
        }
    }

    /// Has the decoder taken more fill than a stream that is complete needs? (See [`MAX_PAST_END`].)
    #[inline]
    pub fn exhausted(&self) -> bool {
        self.past_end > MAX_PAST_END
    }

    /// Bytes of fill taken so far.
    #[allow(dead_code)]
    pub fn past_end(&self) -> u32 {
        self.past_end
    }

    /// DECODE (E.3.2): one decision with the context `cx`.
    #[inline(always)]
    pub fn decode(&mut self, cx: &mut u8) -> u32 {
        let state = *cx;
        let index;
        let mut mps = u32::from(state & 1);
        let entry = TABLE.get(usize::from(state >> 1) & 127).copied().unwrap_or(TABLE[0]);
        let qe = entry.qe;
        let mut a = self.a.wrapping_sub(qe);
        let d;
        if (self.c >> 16) < qe {
            // LPS exchange.
            if a < qe {
                d = mps;
                index = entry.nmps;
            } else {
                d = 1 ^ mps;
                if entry.switch {
                    mps = d;
                }
                index = entry.nlps;
            }
            a = qe;
        } else {
            self.c = self.c.wrapping_sub(qe << 16);
            if a & 0x8000 != 0 {
                self.a = a;
                return mps;
            }
            // MPS exchange.
            if a < qe {
                d = 1 ^ mps;
                if entry.switch {
                    mps = d;
                }
                index = entry.nlps;
            } else {
                d = mps;
                index = entry.nmps;
            }
        }
        // RENORMD (E.3.3).
        loop {
            if self.ct == 0 {
                self.byte_in();
            }
            a <<= 1;
            self.c <<= 1;
            self.ct -= 1;
            if a & 0x8000 != 0 {
                break;
            }
        }
        self.a = a;
        *cx = (index << 1) | mps as u8;
        d
    }
}

impl Mq<'_> {
    /// Decode up to `max` decisions in the context `cx`, stopping after the first that is not `value` (0 or 1).
    /// Returns how many decisions came out as `value`, and whether the decision after them was decoded too (it was
    /// not `value`; false when `max` was reached first). Gives what `decode` would, decision by decision, but a long
    /// run of the more probable symbol in a context where it is very probable takes one step: while the coder does
    /// not have to renormalise, each such decision only takes `Qe` off the interval and off the code register.
    pub fn decode_run(&mut self, cx: &mut u8, value: u32, max: usize) -> (usize, bool) {
        let mut n = 0usize;
        while n < max {
            if u32::from(*cx & 1) == value {
                let qe = TABLE.get(usize::from(*cx >> 1) & 127).map_or(1, |e| e.qe);
                // Decision j (1, 2, ...) stays on the fast path while `Chigh >= j * Qe` and `A - j * Qe >= 0x8000`.
                let k = ((self.c >> 16) / qe).min((self.a - 0x8000) / qe) as usize;
                let k = k.min(max - n);
                if k > 0 {
                    let taken = k as u32 * qe;
                    self.c -= taken << 16;
                    self.a -= taken;
                    n += k;
                    continue;
                }
            }
            if self.decode(cx) != value {
                return (n, true);
            }
            n += 1;
        }
        (n, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// T.88 H.2: a test sequence for the arithmetic coder, 256 decisions in one context.
    #[test]
    fn the_standard_test_sequence_decodes() {
        let coded: [u8; 30] = [
            0x84, 0xC7, 0x3B, 0xFC, 0xE1, 0xA1, 0x43, 0x04, 0x02, 0x20, 0x00, 0x00, 0x41, 0x0D, 0xBB, 0x86, 0xF4, 0x31, 0x7F, 0xFF, 0x88, 0xFF, 0x37, 0x47, 0x1A,
            0xDB, 0x6A, 0xDF, 0xFF, 0xAC,
        ];
        let expected: [u8; 32] = [
            0x00, 0x02, 0x00, 0x51, 0x00, 0x00, 0x00, 0xC0, 0x03, 0x52, 0x87, 0x2A, 0xAA, 0xAA, 0xAA, 0xAA, 0x82, 0xC0, 0x20, 0x00, 0xFC, 0xD7, 0x9E, 0xF6, 0xBF,
            0x7F, 0xED, 0x90, 0x4F, 0x46, 0xA3, 0xBF,
        ];
        let mut mq = Mq::new(&coded);
        let mut cx = 0u8;
        let mut out = [0u8; 32];
        for i in 0..256 {
            let bit = mq.decode(&mut cx) as u8;
            out[i / 8] |= bit << (7 - i % 8);
        }
        assert_eq!(out, expected);
        assert!(!mq.exhausted());
    }

    #[test]
    fn a_run_is_the_same_as_the_decisions_one_by_one() {
        // Data of every sort, contexts in every state: the run and the single decisions give the same bits, leave the
        // decoder and the context in the same state.
        let mut seed = 5u32;
        let mut next = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 16) as u8
        };
        for round in 0..400 {
            let len = 4 + round % 40;
            let mut data: Vec<u8> = (0..len).map(|_| next()).collect();
            // Some data that is mostly the same, as coded blank space is.
            if round % 3 == 0 {
                for b in data.iter_mut().skip(2) {
                    *b = if next() < 8 { next() } else { 0x00 };
                }
            }
            let mut a = Mq::new(&data);
            let mut b = Mq::new(&data);
            let mut ca = [context((round % 47) as u8, (round % 2) as u8), 0];
            let mut cb = ca;
            let value = (round / 2 % 2) as u32;
            let max = 1 + round * 7 % 300;
            let mut singles = Vec::new();
            let (n, odd) = a.decode_run(&mut ca[0], value, max);
            // The same with single decisions.
            let mut m = 0;
            let mut odd2 = false;
            while m < max {
                let d = b.decode(&mut cb[0]);
                if d != value {
                    odd2 = true;
                    break;
                }
                m += 1;
                singles.push(d);
            }
            assert_eq!((n, odd), (m, odd2), "round {round}");
            assert_eq!((a.a, a.c, a.ct, a.bp, ca), (b.a, b.c, b.ct, b.bp, cb), "round {round}");
        }
    }

    #[test]
    fn data_of_all_ones_decodes_for_ever_and_runs_dry() {
        // 0xFF forever: a marker straight away; every decision is made from 1-bits, none of it fails, and the decoder
        // says it has run dry once it needs more than a stream that is complete does (fresh contexts need many bits).
        let data = [0xFFu8; 64];
        let mut mq = Mq::new(&data);
        let mut fresh = vec![0u8; 100_000];
        for cx in fresh.iter_mut() {
            mq.decode(cx);
        }
        assert!(mq.exhausted());
        // And no data at all.
        let mut mq = Mq::new(&[]);
        let mut fresh = vec![0u8; 100_000];
        for cx in fresh.iter_mut() {
            mq.decode(cx);
        }
        assert!(mq.exhausted());
        // Contexts that settle into near certainty need no more bits: those decisions go on without end and without harm.
        let mut mq = Mq::new(&data);
        let mut cx = [0u8; 16];
        for i in 0..100_000 {
            mq.decode(&mut cx[i % 16]);
        }
    }

}
