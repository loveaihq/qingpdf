//! Packet header bits (ISO/IEC 15444-1 B.10.1: a 0xFF byte is followed by a byte of only seven bits) and tag trees
//! (B.10.2).

/// A reader of the bits of packet headers. `None` from every read: the data has ended.
pub(super) struct Bits<'a> {
    data: &'a [u8],
    pub pos: usize,
    cur: u8,
    left: u8,
    prev_ff: bool,
    /// Bits taken so far (the packet header's cost).
    pub taken: u64,
}

impl<'a> Bits<'a> {
    pub fn new(data: &'a [u8], pos: usize) -> Bits<'a> {
        Bits { data, pos, cur: 0, left: 0, prev_ff: false, taken: 0 }
    }

    #[inline]
    pub fn bit(&mut self) -> Option<u32> {
        if self.left == 0 {
            let b = *self.data.get(self.pos)?;
            self.pos += 1;
            self.left = if self.prev_ff { 7 } else { 8 };
            self.prev_ff = b == 0xFF;
            self.cur = b;
        }
        self.left -= 1;
        self.taken += 1;
        Some(u32::from((self.cur >> self.left) & 1))
    }

    /// `n` bits (at most 32), most significant first.
    pub fn bits(&mut self, n: u32) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.bit()?;
        }
        Some(v)
    }

    /// The end of the header: the rest of the byte is padding, and a 0xFF last byte is followed by a stuffed byte.
    pub fn align(&mut self) {
        self.left = 0;
        if self.prev_ff {
            self.pos += 1;
            self.prev_ff = false;
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Node {
    low: u16,
    value: u16,
}

const INF: u16 = u16::MAX;

impl Node {
    pub const FRESH: Node = Node { low: 0, value: INF };
}

/// Most levels a tag tree over at most 2^20 by 2^20 leaves has.
const MAX_LEVELS: usize = 22;

/// The number of nodes of a tag tree over `w` by `h` leaves (all levels), 0 for none.
pub(super) fn node_count(w: u64, h: u64) -> u64 {
    if w == 0 || h == 0 {
        return 0;
    }
    let (mut w, mut h, mut n) = (w, h, 0u64);
    loop {
        n = n.saturating_add(w.saturating_mul(h));
        if w <= 1 && h <= 1 {
            return n;
        }
        w = w.div_ceil(2);
        h = h.div_ceil(2);
    }
}

/// Is the value of the leaf (x, y) below `threshold`? Reads what is needed of the tree (B.10.2); `None` when the
/// data ends. `nodes` is the tree's nodes, leaves first, level by level; the tree is `w` by `h` leaves.
pub(super) fn below(nodes: &mut [Node], w: u32, h: u32, x: u32, y: u32, threshold: u32, bits: &mut Bits<'_>) -> Option<bool> {
    let mut offs = [0usize; MAX_LEVELS];
    let mut widths = [0usize; MAX_LEVELS];
    let (mut lw, mut lh, mut o, mut n) = (w as usize, h as usize, 0usize, 0usize);
    loop {
        *offs.get_mut(n)? = o;
        *widths.get_mut(n)? = lw;
        o = o.checked_add(lw.checked_mul(lh)?)?;
        n += 1;
        if lw <= 1 && lh <= 1 {
            break;
        }
        lw = lw.div_ceil(2);
        lh = lh.div_ceil(2);
    }
    let threshold = threshold.min(u32::from(INF)) as u16;
    let mut low = 0u16;
    for level in (0..n).rev() {
        let at = offs.get(level)? + (y as usize >> level) * widths.get(level)? + (x as usize >> level);
        let node = nodes.get_mut(at)?;
        if low > node.low {
            node.low = low;
        } else {
            low = node.low;
        }
        while low < threshold && low < node.value {
            if bits.bit()? == 1 {
                node.value = low;
            } else {
                low += 1;
            }
        }
        node.low = low;
        if level == 0 {
            return Some(node.value < threshold);
        }
    }
    None
}

/// The value of the leaf (x, y) once it is known to be below `limit` (the zero bit-plane count); `Some(None)`: it is
/// not below `limit`.
pub(super) fn value(nodes: &mut [Node], w: u32, h: u32, x: u32, y: u32, limit: u32, bits: &mut Bits<'_>) -> Option<Option<u32>> {
    for t in 1..=limit {
        if below(nodes, w, h, x, y, t, bits)? {
            return Some(Some(t - 1));
        }
    }
    Some(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_skip_the_stuffed_bit_after_ff() {
        // 0xFF then a byte whose top bit is stuffing: 8 + 7 bits, then padding.
        let data = [0xFF, 0x7F, 0xA0];
        let mut b = Bits::new(&data, 0);
        assert_eq!(b.bits(8), Some(0xFF));
        assert_eq!(b.bits(7), Some(0x7F));
        assert_eq!(b.bit(), Some(1));
        b.align();
        assert_eq!(b.pos, 3);
        assert_eq!(b.bit(), None);
        // A header that ends in 0xFF is followed by a stuffed byte.
        let data = [0x80, 0xFF, 0x00, 0x55];
        let mut b = Bits::new(&data, 0);
        b.bits(8);
        b.bits(8);
        b.align();
        assert_eq!(b.pos, 3);
    }

    #[test]
    fn node_counts() {
        assert_eq!(node_count(1, 1), 1);
        assert_eq!(node_count(2, 2), 5);
        assert_eq!(node_count(3, 2), 6 + 2 + 1);
        assert_eq!(node_count(0, 5), 0);
    }
}
