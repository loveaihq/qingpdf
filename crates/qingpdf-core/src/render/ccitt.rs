//! CCITTFaxDecode (ISO 32000-1 7.4.6): Group 3 one-dimensional (`K = 0`), Group 3
//! two-dimensional (`K > 0`) and Group 4 (`K < 0`) fax data, after ITU-T T.4 and T.6
//! (the code tables are theirs; the PDF specification refers to them).
//!
//! The output is packed rows, most significant bit first, a row padded to a byte
//! (7.4.6): white pixels are 1 and black 0 unless `BlackIs1`.

use std::sync::OnceLock;

pub(crate) struct Params {
    pub k: i64,
    pub columns: usize,
    /// Rows to decode; 0: until the data ends.
    pub rows: usize,
    pub byte_align: bool,
    pub black_is_1: bool,
}

pub(crate) struct Decoded {
    pub data: Vec<u8>,
    /// Rows decoded (the data may stop before `rows`).
    pub rows: usize,
    /// The data stopped at a code that does not exist, or in the middle of a row.
    pub damaged: bool,
}

/// T.4 Table 2 and 3: terminating and make-up codes, white then black, as bit strings.
const WHITE: &[(&str, u16)] = &[
    ("00110101", 0), ("000111", 1), ("0111", 2), ("1000", 3), ("1011", 4), ("1100", 5), ("1110", 6), ("1111", 7),
    ("10011", 8), ("10100", 9), ("00111", 10), ("01000", 11), ("001000", 12), ("000011", 13), ("110100", 14), ("110101", 15),
    ("101010", 16), ("101011", 17), ("0100111", 18), ("0001100", 19), ("0001000", 20), ("0010111", 21), ("0000011", 22), ("0000100", 23),
    ("0101000", 24), ("0101011", 25), ("0010011", 26), ("0100100", 27), ("0011000", 28), ("00000010", 29), ("00000011", 30), ("00011010", 31),
    ("00011011", 32), ("00010010", 33), ("00010011", 34), ("00010100", 35), ("00010101", 36), ("00010110", 37), ("00010111", 38), ("00101000", 39),
    ("00101001", 40), ("00101010", 41), ("00101011", 42), ("00101100", 43), ("00101101", 44), ("00000100", 45), ("00000101", 46), ("00001010", 47),
    ("00001011", 48), ("01010010", 49), ("01010011", 50), ("01010100", 51), ("01010101", 52), ("00100100", 53), ("00100101", 54), ("01011000", 55),
    ("01011001", 56), ("01011010", 57), ("01011011", 58), ("01001010", 59), ("01001011", 60), ("00110010", 61), ("00110011", 62), ("00110100", 63),
    ("11011", 64), ("10010", 128), ("010111", 192), ("0110111", 256), ("00110110", 320), ("00110111", 384), ("01100100", 448), ("01100101", 512),
    ("01101000", 576), ("01100111", 640), ("011001100", 704), ("011001101", 768), ("011010010", 832), ("011010011", 896), ("011010100", 960),
    ("011010101", 1024), ("011010110", 1088), ("011010111", 1152), ("011011000", 1216), ("011011001", 1280), ("011011010", 1344),
    ("011011011", 1408), ("010011000", 1472), ("010011001", 1536), ("010011010", 1600), ("011000", 1664), ("010011011", 1728),
];

const BLACK: &[(&str, u16)] = &[
    ("0000110111", 0), ("010", 1), ("11", 2), ("10", 3), ("011", 4), ("0011", 5), ("0010", 6), ("00011", 7),
    ("000101", 8), ("000100", 9), ("0000100", 10), ("0000101", 11), ("0000111", 12), ("00000100", 13), ("00000111", 14), ("000011000", 15),
    ("0000010111", 16), ("0000011000", 17), ("0000001000", 18), ("00001100111", 19), ("00001101000", 20), ("00001101100", 21), ("00000110111", 22),
    ("00000101000", 23), ("00000010111", 24), ("00000011000", 25), ("000011001010", 26), ("000011001011", 27), ("000011001100", 28),
    ("000011001101", 29), ("000001101000", 30), ("000001101001", 31), ("000001101010", 32), ("000001101011", 33), ("000011010010", 34),
    ("000011010011", 35), ("000011010100", 36), ("000011010101", 37), ("000011010110", 38), ("000011010111", 39), ("000001101100", 40),
    ("000001101101", 41), ("000011011010", 42), ("000011011011", 43), ("000001010100", 44), ("000001010101", 45), ("000001010110", 46),
    ("000001010111", 47), ("000001100100", 48), ("000001100101", 49), ("000001010010", 50), ("000001010011", 51), ("000000100100", 52),
    ("000000110111", 53), ("000000111000", 54), ("000000100111", 55), ("000000101000", 56), ("000001011000", 57), ("000001011001", 58),
    ("000000101011", 59), ("000000101100", 60), ("000001011010", 61), ("000001100110", 62), ("000001100111", 63),
    ("0000001111", 64), ("000011001000", 128), ("000011001001", 192), ("000001011011", 256), ("000000110011", 320), ("000000110100", 384),
    ("000000110101", 448), ("0000001101100", 512), ("0000001101101", 576), ("0000001001010", 640), ("0000001001011", 704),
    ("0000001001100", 768), ("0000001001101", 832), ("0000001110010", 896), ("0000001110011", 960), ("0000001110100", 1024),
    ("0000001110101", 1088), ("0000001110110", 1152), ("0000001110111", 1216), ("0000001010010", 1280), ("0000001010011", 1344),
    ("0000001010100", 1408), ("0000001010101", 1472), ("0000001011010", 1536), ("0000001011011", 1600), ("0000001100100", 1664),
    ("0000001100101", 1728),
];

/// Make-up codes 1792 to 2560, the same for both colours.
const EXTENDED: &[(&str, u16)] = &[
    ("00000001000", 1792), ("00000001100", 1856), ("00000001101", 1920), ("000000010010", 1984), ("000000010011", 2048),
    ("000000010100", 2112), ("000000010101", 2176), ("000000010110", 2240), ("000000010111", 2304), ("000000011100", 2368),
    ("000000011101", 2432), ("000000011110", 2496), ("000000011111", 2560),
];

const TABLE_BITS: u32 = 13;

struct Tables {
    /// Indexed by the next 13 bits: `len << 16 | run`, 0 for a bit pattern that is no code.
    white: Vec<u32>,
    black: Vec<u32>,
}

fn build(codes: &[&[(&str, u16)]]) -> Vec<u32> {
    let mut table = vec![0u32; 1 << TABLE_BITS];
    for list in codes {
        for &(bits, run) in *list {
            let len = bits.len() as u32;
            let value = bits.bytes().fold(0u32, |acc, b| (acc << 1) | u32::from(b == b'1'));
            let first = (value << (TABLE_BITS - len)) as usize;
            let count = 1usize << (TABLE_BITS - len);
            if let Some(slots) = table.get_mut(first..first + count) {
                slots.fill(len << 16 | u32::from(run));
            }
        }
    }
    table
}

fn tables() -> &'static Tables {
    static TABLES: OnceLock<Tables> = OnceLock::new();
    TABLES.get_or_init(|| Tables { white: build(&[WHITE, EXTENDED]), black: build(&[BLACK, EXTENDED]) })
}

struct Bits<'a> {
    data: &'a [u8],
    /// Position in bits.
    pos: usize,
}

impl Bits<'_> {
    /// The next `n` (at most 24) bits as a number; zeros past the end.
    #[inline]
    fn peek(&self, n: u32) -> u32 {
        let byte = self.pos / 8;
        let mut v: u32 = 0;
        for i in 0..4 {
            v = (v << 8) | u32::from(self.data.get(byte + i).copied().unwrap_or(0));
        }
        let shift = (self.pos % 8) as u32;
        // 32 bits loaded, `shift` of them already used.
        (v << shift) >> (32 - n)
    }

    #[inline]
    fn skip(&mut self, n: u32) {
        self.pos += n as usize;
    }

    fn at_end(&self) -> bool {
        self.pos >= self.data.len() * 8
    }

    fn align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }
}

enum Mode {
    Pass,
    Horizontal,
    Vertical(i64),
}

fn read_mode(bits: &mut Bits<'_>) -> Option<Mode> {
    let v = bits.peek(7);
    let (mode, len) = if v >> 6 == 1 {
        (Mode::Vertical(0), 1)
    } else if v >> 4 == 0b011 {
        (Mode::Vertical(1), 3)
    } else if v >> 4 == 0b010 {
        (Mode::Vertical(-1), 3)
    } else if v >> 4 == 0b001 {
        (Mode::Horizontal, 3)
    } else if v >> 3 == 0b0001 {
        (Mode::Pass, 4)
    } else if v >> 1 == 0b000011 {
        (Mode::Vertical(2), 6)
    } else if v >> 1 == 0b000010 {
        (Mode::Vertical(-2), 6)
    } else if v == 0b0000011 {
        (Mode::Vertical(3), 7)
    } else if v == 0b0000010 {
        (Mode::Vertical(-3), 7)
    } else {
        // 0000001xxx would be the extension to uncompressed mode; 0000000 is an EOL or garbage.
        return None;
    };
    bits.skip(len);
    Some(mode)
}

/// A run length of one colour: make-up codes, then a terminating code.
fn read_run(bits: &mut Bits<'_>, black: bool, t: &Tables) -> Option<usize> {
    let table = if black { &t.black } else { &t.white };
    let mut total = 0usize;
    for _ in 0..64 {
        let entry = table.get(bits.peek(TABLE_BITS) as usize).copied().unwrap_or(0);
        if entry == 0 {
            return None;
        }
        bits.skip(entry >> 16);
        let run = (entry & 0xFFFF) as usize;
        total += run;
        if run < 64 {
            return Some(total);
        }
    }
    None
}

/// Record a change of colour at `pos`; two changes at one place cancel (a run of length 0).
fn push(cur: &mut Vec<u32>, pos: usize, columns: usize) {
    if pos >= columns {
        return;
    }
    let pos = pos as u32;
    if cur.last() == Some(&pos) {
        cur.pop();
    } else {
        cur.push(pos);
    }
}

fn row_1d(bits: &mut Bits<'_>, cur: &mut Vec<u32>, columns: usize, t: &Tables) -> Option<()> {
    cur.clear();
    let mut a0 = 0usize;
    let mut black = false;
    while a0 < columns {
        a0 += read_run(bits, black, t)?;
        push(cur, a0, columns);
        black = !black;
    }
    Some(())
}

fn row_2d(bits: &mut Bits<'_>, refl: &[u32], cur: &mut Vec<u32>, columns: usize, t: &Tables) -> Option<()> {
    cur.clear();
    let width = columns as i64;
    let mut a0: i64 = -1;
    let mut black = false;
    let mut bi = 0usize;
    while a0 < width {
        // b1: the first change on the reference line right of a0 to the opposite colour of a0's.
        while refl.get(bi).is_some_and(|&p| i64::from(p) <= a0) {
            bi += 1;
        }
        let mut i = bi;
        if (i % 2 == 1) != black {
            i += 1;
        }
        let b1 = refl.get(i).map_or(width, |&p| i64::from(p));
        let b2 = refl.get(i + 1).map_or(width, |&p| i64::from(p));
        match read_mode(bits)? {
            Mode::Pass => a0 = b2,
            Mode::Horizontal => {
                let start = a0.max(0) as usize;
                let r1 = read_run(bits, black, t)?;
                let r2 = read_run(bits, !black, t)?;
                let a1 = start + r1;
                let a2 = a1 + r2;
                push(cur, a1, columns);
                push(cur, a2, columns);
                a0 = a2 as i64;
            }
            Mode::Vertical(d) => {
                let a1 = b1 + d;
                if a1 < a0.max(0) || a1 > width {
                    return None;
                }
                push(cur, a1 as usize, columns);
                black = !black;
                a0 = a1;
            }
        }
    }
    Some(())
}

/// Set the pixels `from..to` of a packed row to `value` (0 or 1).
fn fill_bits(row: &mut [u8], from: usize, to: usize, value: bool) {
    if from >= to {
        return;
    }
    let (first, last) = (from / 8, (to - 1) / 8);
    let head = 0xFFu8 >> (from % 8);
    let tail = 0xFFu8 << (7 - (to - 1) % 8);
    let apply = |byte: &mut u8, mask: u8| {
        if value {
            *byte |= mask;
        } else {
            *byte &= !mask;
        }
    };
    if first == last {
        if let Some(b) = row.get_mut(first) {
            apply(b, head & tail);
        }
        return;
    }
    if let Some(b) = row.get_mut(first) {
        apply(b, head);
    }
    if let Some(mid) = row.get_mut(first + 1..last) {
        mid.fill(if value { 0xFF } else { 0 });
    }
    if let Some(b) = row.get_mut(last) {
        apply(b, tail);
    }
}

/// Decode `data`. At most `max_bytes` of output; more rows than that are not decoded.
pub(crate) fn decode(data: &[u8], p: &Params, max_bytes: usize) -> Decoded {
    let t = tables();
    let columns = p.columns.max(1);
    let row_bytes = columns.div_ceil(8);
    let mut max_rows = max_bytes.checked_div(row_bytes).unwrap_or(0);
    if p.rows > 0 {
        max_rows = max_rows.min(p.rows);
    }
    // Room for what the data can give, not for what the header asks for: a few bytes of data can ask for rows
    // enough to fill `max_bytes`. The output grows as rows come out.
    let first_room = row_bytes.saturating_mul(max_rows.min(64)).min(data.len().saturating_mul(32).max(4096)).min(max_bytes);
    let mut out: Vec<u8> = Vec::with_capacity(first_room);
    let mut bits = Bits { data, pos: 0 };
    let mut refl: Vec<u32> = Vec::new();
    let mut cur: Vec<u32> = Vec::new();
    let (white_value, black_value) = (!p.black_is_1, p.black_is_1);
    let mut rows = 0usize;
    let mut damaged = false;
    let mut next_is_2d = p.k < 0;
    // Group 3 may begin with an EOL; with K > 0 a tag bit follows each one.
    let mut first = true;
    while rows < max_rows {
        if p.byte_align {
            bits.align();
        }
        if bits.at_end() {
            break;
        }
        if p.k < 0 {
            // EOFB: two EOLs.
            if bits.peek(12) == 1 {
                break;
            }
        } else {
            while bits.peek(12) == 0 && !bits.at_end() {
                bits.skip(1);
            }
            let mut saw_eol = false;
            if bits.peek(12) == 1 {
                bits.skip(12);
                saw_eol = true;
            }
            if p.k > 0 {
                next_is_2d = bits.peek(1) == 0;
                bits.skip(1);
            }
            // Return to control: another EOL straight after.
            if (saw_eol || !first) && bits.peek(12) == 1 {
                break;
            }
            if bits.at_end() {
                break;
            }
        }
        first = false;
        let ok = if next_is_2d {
            row_2d(&mut bits, &refl, &mut cur, columns, t)
        } else {
            row_1d(&mut bits, &mut cur, columns, t)
        };
        if ok.is_none() {
            damaged = true;
            break;
        }
        let start = out.len();
        out.resize(start + row_bytes, if white_value { 0xFF } else { 0 });
        if let Some(row) = out.get_mut(start..) {
            let mut from = None;
            for &pos in &cur {
                match from.take() {
                    None => from = Some(pos as usize),
                    Some(f) => fill_bits(row, f, pos as usize, black_value),
                }
            }
            if let Some(f) = from {
                fill_bits(row, f, columns, black_value);
            }
        }
        rows += 1;
        std::mem::swap(&mut refl, &mut cur);
        if p.k == 0 {
            next_is_2d = false;
        } else if p.k < 0 {
            next_is_2d = true;
        }
    }
    Decoded { data: out, rows, damaged }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_few_bytes_do_not_reserve_the_output_they_ask_for() {
        // Three bytes of data, a header that asks for a hundred thousand rows: room for what the data can give.
        let p = Params { k: -1, columns: 1000, rows: 100_000, byte_align: false, black_is_1: false };
        let d = decode(&[0x00, 0x10, 0x01], &p, 48 * 1024 * 1024);
        assert!(d.data.capacity() < 64 * 1024, "{}", d.data.capacity());
    }

    #[test]
    fn code_tables_are_prefix_free() {
        // No code may start another one, in either colour (the extended make-up codes count for both).
        for list in [WHITE, BLACK] {
            let all: Vec<&str> = list.iter().chain(EXTENDED).map(|c| c.0).collect();
            for (i, a) in all.iter().enumerate() {
                for (j, b) in all.iter().enumerate() {
                    assert!(i == j || !b.starts_with(a), "{a} is a prefix of {b}");
                }
            }
        }
        // White: 64 terminating + 27 make-up; black the same; 13 extended each.
        assert_eq!(WHITE.len(), 91);
        assert_eq!(BLACK.len(), 91);
        // Both colours' tables fill the code space exactly but for the unused EOL / extension prefixes.
        for table in [&tables().white, &tables().black] {
            let unused = table.iter().filter(|&&e| e == 0).count();
            // 0000000xxxxxx... (EOL and fill) is 1/128 of the space and 0000001xxx (a few black make-ups
            // overlap it) is not free either; a table error would leave a lot more holes.
            assert!(unused < (1 << TABLE_BITS) / 4, "{unused}");
        }
    }

    fn pack(rows: &[&str]) -> Vec<u8> {
        let width = rows.first().map_or(0, |r| r.len());
        let mut out = Vec::new();
        for r in rows {
            let mut bytes = vec![0xFFu8; width.div_ceil(8)];
            for (i, c) in r.bytes().enumerate() {
                if c == b'#'
                    && let Some(b) = bytes.get_mut(i / 8)
                {
                    *b &= !(0x80 >> (i % 8));
                }
            }
            out.extend(bytes);
        }
        out
    }

    /// Bits written out as a string: "1 011 ..." with spaces ignored.
    fn bits_to_bytes(s: &str) -> Vec<u8> {
        let digits: Vec<u8> = s.bytes().filter(|b| *b == b'0' || *b == b'1').collect();
        digits
            .chunks(8)
            .map(|c| c.iter().enumerate().fold(0u8, |acc, (i, &b)| acc | (u8::from(b == b'1') << (7 - i))))
            .collect()
    }

    #[test]
    fn group_4_modes() {
        // Row 1 (reference all white): "..###..." = horizontal: white 2, black 3, then V0 to the end.
        //   horizontal 001, white run 2 = 0111, black run 3 = 10, then V0 = 1 (b1 = end of line)
        // Row 2 equals row 1: V0 V0 V0 (b1 at 2, at 5, at 8)  = 1 1 1
        // Then EOFB.
        let data = bits_to_bytes("001 0111 10 1  1 1 1  000000000001 000000000001");
        let got = decode(&data, &Params { k: -1, columns: 8, rows: 0, byte_align: false, black_is_1: false }, 1 << 20);
        assert!(!got.damaged);
        assert_eq!(got.rows, 2);
        assert_eq!(got.data, pack(&["..###...", "..###..."]));
    }

    #[test]
    fn group_3_one_dimensional_with_eols() {
        // Two rows of 8: "..###..." = white 2 (0111), black 3 (10), white 3 (1000); preceded by EOL each.
        let data = bits_to_bytes("000000000001 0111 10 1000  000000000001 00110101 0000110111 ");
        // second row: white 0 (00110101), black 0?? invalid since the row needs 8 pixels: it is damaged.
        let got = decode(&data, &Params { k: 0, columns: 8, rows: 0, byte_align: false, black_is_1: false }, 1 << 20);
        assert_eq!(got.rows, 1);
        assert!(got.damaged);
        assert_eq!(got.data, pack(&["..###..."]));
        // BlackIs1 flips the bits.
        let flipped = decode(&data, &Params { k: 0, columns: 8, rows: 0, byte_align: false, black_is_1: true }, 1 << 20);
        assert_eq!(flipped.data, vec![0b0011_1000]);
    }

    #[test]
    fn nonsense_and_limits() {
        let junk: Vec<u8> = (0..4000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
        for k in [-1, 0, 3] {
            let got = decode(&junk, &Params { k, columns: 1728, rows: 0, byte_align: k == 0, black_is_1: false }, 1 << 16);
            assert!(got.data.len() <= (1 << 16) + 216);
        }
        // Zero columns and empty data.
        let got = decode(&[], &Params { k: -1, columns: 0, rows: 5, byte_align: false, black_is_1: false }, 1 << 16);
        assert_eq!(got.rows, 0);
    }

    #[test]
    fn fill_bits_edges() {
        let mut row = vec![0xFFu8; 3];
        fill_bits(&mut row, 3, 20, false);
        assert_eq!(row, vec![0b1110_0000, 0, 0b0000_1111]);
        fill_bits(&mut row, 4, 6, true);
        assert_eq!(row, vec![0b1110_1100, 0, 0b0000_1111]);
    }

    /// Rows made by libtiff (through Pillow): a 2600 pixel wide bitmap whose rows are one white and one
    /// black run, with every run length that has a code of its own (0 to 130, and around each multiple of 64
    /// up to the extended make-up codes), in both colours. The rows are in the order of this function.
    /// (libtiff codes the 0 bits of the data as white, whatever the file says about black and white, so
    /// the tests decode with `BlackIs1`: the bits then come out as they went in.)
    fn runs_rows() -> Vec<(usize, usize)> {
        let mut lens: Vec<usize> = (0..=130).collect();
        for k in 1..=40usize {
            for d in [-1i64, 0, 1, 62, 63] {
                let v = 64 * k as i64 + d;
                if (0..=2600).contains(&v) {
                    lens.push(v as usize);
                }
            }
        }
        lens.sort_unstable();
        lens.dedup();
        lens.into_iter().flat_map(|l| [(l, 2600 - l), (2600 - l, l)]).collect()
    }

    fn runs_expected() -> Vec<u8> {
        let mut out = Vec::new();
        for (w, b) in runs_rows() {
            let mut row = vec![0xFFu8; 2600usize.div_ceil(8)];
            fill_bits(&mut row, w, w + b, false);
            out.extend(row);
        }
        out
    }

    #[test]
    fn libtiff_group_4() {
        let data = include_bytes!("../../tests/render_fixtures/ccitt_runs_g4.bin");
        let rows = runs_rows().len();
        let got = decode(data, &Params { k: -1, columns: 2600, rows, byte_align: false, black_is_1: true }, 1 << 26);
        assert!(!got.damaged);
        assert_eq!(got.rows, rows);
        let want = runs_expected();
        let stride = 2600usize.div_ceil(8);
        for (i, ((a, b), (w, bl))) in got.data.chunks(stride).zip(want.chunks(stride)).zip(runs_rows()).enumerate() {
            assert!(a == b, "group 4 row {i} (white {w}, black {bl}) differs");
        }
        assert!(got.data == want, "group 4 rows differ");
    }

    #[test]
    fn libtiff_group_3_one_dimensional() {
        let data = include_bytes!("../../tests/render_fixtures/ccitt_runs_g3.bin");
        let rows = runs_rows().len();
        let got = decode(data, &Params { k: 0, columns: 2600, rows, byte_align: false, black_is_1: true }, 1 << 26);
        assert!(!got.damaged);
        assert_eq!(got.rows, rows);
        let want = runs_expected();
        let stride = 2600usize.div_ceil(8);
        for (i, ((a, b), (w, bl))) in got.data.chunks(stride).zip(want.chunks(stride)).zip(runs_rows()).enumerate() {
            assert!(a == b, "group 3 row {i} (white {w}, black {bl}) differs");
        }
        assert!(got.data == want, "group 3 rows differ");
        // Without the row count the end of the data (or an RTC) stops it at the same place.
        let all = decode(data, &Params { k: 0, columns: 2600, rows: 0, byte_align: false, black_is_1: true }, 1 << 26);
        assert_eq!(all.rows, rows);
    }
}
