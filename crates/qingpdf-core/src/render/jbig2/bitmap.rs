//! Bitmaps of a JBIG2 image: one bit per pixel, rows padded to a byte, 1 is black (T.88 6.2.1: a bitmap is an
//! array of bits). Pixels outside the bitmap read as 0 (white), as the standard says for the pixels a context
//! template reaches outside the bitmap.

/// How a region is put on another bitmap (T.88 7.4.1.5, Table 6; the text-region operators 6.4.5 are the first
/// four).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Op {
    Or,
    And,
    Xor,
    Xnor,
    Replace,
}

impl Op {
    pub fn from_code(code: u32) -> Option<Op> {
        Some(match code {
            0 => Op::Or,
            1 => Op::And,
            2 => Op::Xor,
            3 => Op::Xnor,
            4 => Op::Replace,
            _ => return None,
        })
    }
}

#[cfg_attr(test, derive(Clone))]
pub(super) struct Bitmap {
    pub w: usize,
    pub h: usize,
    pub stride: usize,
    pub data: Vec<u8>,
}

impl Bitmap {
    /// A bitmap of `w` by `h` pixels, all black or all white. The caller has checked the size against its limits.
    pub fn new(w: usize, h: usize, black: bool) -> Bitmap {
        let stride = w.div_ceil(8);
        let mut bitmap = Bitmap { w, h, stride, data: vec![0; stride.saturating_mul(h)] };
        if black {
            bitmap.fill_black();
        }
        bitmap
    }

    fn fill_black(&mut self) {
        black_rows(&mut self.data, self.stride, self.w);
    }

    pub fn row(&self, y: usize) -> &[u8] {
        self.data.get(y * self.stride..(y + 1) * self.stride).unwrap_or(&[])
    }

    pub fn row_mut(&mut self, y: usize) -> &mut [u8] {
        let stride = self.stride;
        self.data.get_mut(y * stride..(y + 1) * stride).unwrap_or(&mut [])
    }

    /// The pixel at (`x`, `y`); 0 outside.
    #[inline]
    pub fn get(&self, x: i64, y: i64) -> u8 {
        if x < 0 || y < 0 || x >= self.w as i64 || y >= self.h as i64 {
            return 0;
        }
        let (x, y) = (x as usize, y as usize);
        self.data.get(y * self.stride + (x >> 3)).map_or(0, |b| (b >> (7 - (x & 7))) & 1)
    }

    /// Set the pixel (`x`, `y`) to `v` (0 or 1); nothing outside.
    #[inline]
    pub fn set(&mut self, x: usize, y: usize, v: u8) {
        if x >= self.w || y >= self.h {
            return;
        }
        let at = y * self.stride + (x >> 3);
        if let Some(b) = self.data.get_mut(at) {
            let bit = 0x80u8 >> (x & 7);
            if v != 0 {
                *b |= bit;
            } else {
                *b &= !bit;
            }
        }
    }

    /// How many pixels of a bitmap of `sw` by `sh` put with its corner at (`x`, `y`) fall on this one.
    pub fn overlap(&self, sw: usize, sh: usize, x: i64, y: i64) -> u64 {
        let w = x.saturating_add(sw as i64).min(self.w as i64) - x.max(0);
        let h = y.saturating_add(sh as i64).min(self.h as i64) - y.max(0);
        if w <= 0 || h <= 0 { 0 } else { w as u64 * h as u64 }
    }

    /// Set row `y` from one byte per pixel (0 or 1); `src` holds at least `w` pixels.
    pub fn pack_row(&mut self, y: usize, src: &[u8]) {
        let w = self.w;
        let row = self.row_mut(y);
        let src = src.get(..w).unwrap_or(&[]);
        let mut chunks = src.chunks_exact(8);
        for (byte, chunk) in row.iter_mut().zip(&mut chunks) {
            // Eight bytes of 0 or 1 gather into one: the multiplication puts byte i at bit 63 - i, the first pixel on top.
            let v = chunk.try_into().map_or(0, u64::from_le_bytes);
            *byte = (v.wrapping_mul(0x8040_2010_0804_0201) >> 56) as u8;
        }
        let rest = chunks.remainder();
        if let Some(byte) = row.get_mut(src.len() / 8)
            && !rest.is_empty()
        {
            *byte = rest.iter().enumerate().fold(0u8, |v, (k, &p)| v | (u8::from(p != 0) << (7 - k)));
        }
    }

    /// Put `src` on this bitmap with its top left corner at (`x`, `y`), clipped to this bitmap.
    pub fn combine(&mut self, src: &Bitmap, x: i64, y: i64, op: Op) {
        self.combine_part(src, 0, src.w, x, y, op);
    }

    /// Put the columns `sx` to `sx + sw` of `src` on this bitmap with the top left corner of that part at (`x`, `y`).
    pub fn combine_part(&mut self, src: &Bitmap, sx: usize, sw: usize, x: i64, y: i64, op: Op) {
        let sw = sw.min(src.w.saturating_sub(sx));
        let (x0, y0) = (x.max(0), y.max(0));
        let x1 = x.saturating_add(sw as i64).min(self.w as i64);
        let y1 = y.saturating_add(src.h as i64).min(self.h as i64);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let n = (x1 - x0) as usize;
        let from = sx + (x0 - x) as usize;
        for dy in y0..y1 {
            let srow = src.row((dy - y) as usize);
            blit_row(self.row_mut(dy as usize), x0 as usize, srow, from, n, op);
        }
    }

    /// Add white (or black) rows at the bottom until the bitmap is `h` rows high.
    pub fn grow_to(&mut self, h: usize, black: bool) {
        if h <= self.h {
            return;
        }
        let old = self.data.len();
        self.data.resize(self.stride.saturating_mul(h), 0);
        self.h = h;
        if black && let Some(new) = self.data.get_mut(old..) {
            black_rows(new, self.stride, self.w);
        }
    }
}

/// Whole rows of `w` pixels (`stride` bytes each) made black, the padding bits of each row left clear.
fn black_rows(data: &mut [u8], stride: usize, w: usize) {
    data.fill(0xFF);
    let pad = stride * 8 - w;
    if pad > 0 {
        let mask = 0xFFu8 << pad;
        for row in data.chunks_exact_mut(stride.max(1)) {
            if let Some(last) = row.last_mut() {
                *last &= mask;
            }
        }
    }
}

/// `n` pixels of `src` from pixel `sx` on, put on `dst` from pixel `dx` on.
fn blit_row(dst: &mut [u8], dx: usize, src: &[u8], sx: usize, n: usize, op: Op) {
    match op {
        Op::Or => blit_with(dst, dx, src, sx, n, |d, s, m| d | (s & m)),
        Op::And => blit_with(dst, dx, src, sx, n, |d, s, m| d & (s | !m)),
        Op::Xor => blit_with(dst, dx, src, sx, n, |d, s, m| d ^ (s & m)),
        Op::Xnor => blit_with(dst, dx, src, sx, n, |d, s, m| d ^ (!s & m)),
        Op::Replace => blit_with(dst, dx, src, sx, n, |d, s, m| (d & !m) | (s & m)),
    }
}

/// The bytes of the run, `f(destination byte, the source bits that line up with it, mask of the run in the byte)`.
#[inline(always)]
fn blit_with(dst: &mut [u8], dx: usize, src: &[u8], sx: usize, n: usize, f: impl Fn(u8, u8, u8) -> u8) {
    let (first, last) = (dx / 8, (dx + n - 1) / 8);
    let Some(window) = dst.get_mut(first..=last) else { return };
    // The source bit that lines up with bit 0 of the first destination byte (before the run when `dx` is not on a
    // byte border); each byte on, the source moves one byte, the shift stays.
    let skew = sx as i64 - (dx % 8) as i64;
    let (mut at, sh) = (skew.div_euclid(8), skew.rem_euclid(8) as u32);
    let byte = |i: i64| -> u16 { usize::try_from(i).ok().and_then(|i| src.get(i)).map_or(0, |&b| u16::from(b)) };
    let count = window.len();
    for (k, d) in window.iter_mut().enumerate() {
        let lo = if k == 0 { dx % 8 } else { 0 };
        let hi = if k + 1 == count { (dx + n - 1) % 8 + 1 } else { 8 };
        let mask = ((0xFFu16 >> lo) & !(0xFFu16 >> hi)) as u8;
        let s = (((byte(at) << 8) | byte(at + 1)) >> (8 - sh)) as u8;
        *d = f(*d, s, mask);
        at += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_rows(rows: &[&str]) -> Bitmap {
        let mut b = Bitmap::new(rows.first().map_or(0, |r| r.len()), rows.len(), false);
        for (y, r) in rows.iter().enumerate() {
            let px: Vec<u8> = r.bytes().map(|c| u8::from(c == b'#')).collect();
            b.pack_row(y, &px);
        }
        b
    }

    fn rows(b: &Bitmap) -> Vec<String> {
        (0..b.h).map(|y| (0..b.w).map(|x| if b.get(x as i64, y as i64) == 1 { '#' } else { '.' }).collect()).collect()
    }

    #[test]
    fn combine_clips_and_shifts_across_byte_borders() {
        let src = from_rows(&["#########", "#.......#", "#########"]);
        for (x, y) in [(0i64, 0i64), (3, 1), (7, 2), (-2, -1), (13, 4), (-20, 0)] {
            let mut fast = Bitmap::new(20, 6, false);
            fast.combine(&src, x, y, Op::Or);
            let mut slow = Bitmap::new(20, 6, false);
            for sy in 0..3i64 {
                for sx in 0..9i64 {
                    if src.get(sx, sy) == 1 && (0..20).contains(&(x + sx)) && (0..6).contains(&(y + sy)) {
                        slow.set((x + sx) as usize, (y + sy) as usize, 1);
                    }
                }
            }
            assert_eq!(rows(&fast), rows(&slow), "at {x},{y}");
        }
    }

    #[test]
    fn the_five_operators() {
        let src = from_rows(&["##..", "#.#."]);
        let base = from_rows(&["#.#.", "##.."]);
        let expect = [
            (Op::Or, ["###.", "###."]),
            (Op::And, ["#...", "#..."]),
            (Op::Xor, [".##.", ".##."]),
            (Op::Xnor, ["#..#", "#..#"]),
            (Op::Replace, ["##..", "#.#."]),
        ];
        for (op, want) in expect {
            let mut b = Bitmap::new(4, 2, false);
            b.combine(&base, 0, 0, Op::Replace);
            b.combine(&src, 0, 0, op);
            assert_eq!(rows(&b), want.to_vec(), "{op:?}");
        }
    }

    #[test]
    fn packing_a_row_puts_the_first_pixel_on_top() {
        // Every pattern of eight pixels, in a row of sixteen and a row of nineteen.
        for v in 0..=255u32 {
            let px: Vec<u8> = (0..8).map(|k| ((v >> (7 - k)) & 1) as u8).collect();
            for w in [8usize, 16, 19] {
                let mut b = Bitmap::new(w, 1, false);
                let mut row = px.clone();
                row.resize(w, 1);
                b.pack_row(0, &row);
                assert_eq!(b.row(0).first().copied(), Some(v as u8), "pattern {v:08b} in {w}");
                if w == 19 {
                    // The three pixels after the sixteenth: 1 1 1 at the top of the last byte.
                    assert_eq!(b.row(0).get(2).copied(), Some(0xE0));
                }
            }
        }
    }

    #[test]
    fn black_fill_keeps_the_padding_clear() {
        let b = Bitmap::new(10, 2, true);
        assert_eq!(b.row(0), &[0xFF, 0xC0]);
        let mut c = Bitmap::new(10, 1, true);
        c.grow_to(3, true);
        assert_eq!(c.row(2), &[0xFF, 0xC0]);
    }
}
