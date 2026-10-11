//! Huffman tables of JBIG2 (T.88 Annex B): the fifteen standard tables, custom tables from table segments, and
//! the bit reader they and the Huffman-coded regions read with.

use std::rc::Rc;
use std::sync::OnceLock;

use super::Ctx;
use crate::error::{Error, Result};
use crate::render::work::cost;

fn bad(m: &str) -> Error {
    Error::Invalid(format!("JBIG2: {m}"))
}

/// Bits of a Huffman-coded stream, most significant first.
pub(super) struct BitReader<'a> {
    data: &'a [u8],
    /// Position in bits.
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> BitReader<'a> {
        BitReader { data, pos: 0 }
    }

    pub fn bit(&mut self) -> Result<u32> {
        let b = self.data.get(self.pos >> 3).ok_or_else(|| bad("Huffman data ends too soon"))?;
        let v = (b >> (7 - (self.pos & 7))) & 1;
        self.pos += 1;
        Ok(u32::from(v))
    }

    /// `n` (at most 32) bits as a number.
    pub fn bits(&mut self, n: u32) -> Result<u32> {
        let mut v = 0u64;
        for _ in 0..n {
            v = (v << 1) | u64::from(self.bit()?);
        }
        Ok(v as u32)
    }

    /// Skip to the next byte border.
    pub fn align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }

    /// Byte position (the reader is aligned first).
    pub fn byte_pos(&mut self) -> usize {
        self.align();
        self.pos / 8
    }

    /// Move to byte `p`.
    pub fn seek(&mut self, p: usize) {
        self.pos = p.saturating_mul(8);
    }

    pub fn data(&self) -> &'a [u8] {
        self.data
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Normal,
    /// The line for everything below the lowest range: the value is `low` minus 32 more bits.
    Lower,
    /// Everything above the highest: `low` plus 32 more bits.
    Upper,
    Oob,
}

#[derive(Clone, Copy, Debug)]
struct Line {
    low: i64,
    preflen: u8,
    rangelen: u8,
    kind: Kind,
}

/// The longest prefix we accept (the standard allows what fits `HTPS` bits, 255; no table uses more than 32).
const MAX_PREFIX: usize = 32;

pub(super) struct Table {
    lines: Vec<Line>,
    /// Indices of the lines that have a code, by length of code, then by place in the table.
    order: Vec<u32>,
    /// By length of code: the first code, how many codes, and where they start in `order`.
    first_code: [u32; MAX_PREFIX + 1],
    count: [u32; MAX_PREFIX + 1],
    first_index: [usize; MAX_PREFIX + 1],
}

impl Table {
    /// Assign the prefix codes (T.88 B.3) and set up decoding.
    fn build(lines: Vec<Line>) -> Result<Table> {
        let mut count = [0u32; MAX_PREFIX + 1];
        for l in &lines {
            let len = usize::from(l.preflen);
            if len > MAX_PREFIX {
                return Err(bad("a Huffman prefix is longer than 32 bits"));
            }
            if len > 0
                && let Some(c) = count.get_mut(len)
            {
                *c += 1;
            }
        }
        let mut first_code = [0u32; MAX_PREFIX + 1];
        let mut first_index = [0usize; MAX_PREFIX + 1];
        let mut code = 0u64;
        let mut index = 0usize;
        for len in 1..=MAX_PREFIX {
            let before = count.get(len - 1).copied().unwrap_or(0);
            code = (code + if len == 1 { 0 } else { u64::from(before) }) << 1;
            let n = u64::from(count.get(len).copied().unwrap_or(0));
            // More codes of this length than there are bit patterns of it: not a prefix code.
            if code + n > (1u64 << len) {
                return Err(bad("a Huffman table has more codes than its prefix lengths allow"));
            }
            if let (Some(f), Some(i)) = (first_code.get_mut(len), first_index.get_mut(len)) {
                *f = code as u32;
                *i = index;
            }
            index += n as usize;
        }
        // The lines that have a code, by length of code and then by place in the table: one pass, each line put at the
        // next free place of its length.
        let mut order = vec![0u32; index];
        let mut next = first_index;
        for (i, l) in lines.iter().enumerate() {
            if let Some(slot) = next.get_mut(usize::from(l.preflen)).filter(|_| l.preflen > 0) {
                if let Some(o) = order.get_mut(*slot) {
                    *o = i as u32;
                }
                *slot += 1;
            }
        }
        Ok(Table { lines, order, first_code, count, first_index })
    }

    /// A table whose lines are the numbers `0..lengths.len()` with those prefix lengths, no extra bits (the symbol
    /// codes of a Huffman-coded text region, 7.4.3.1.7). A length of 0 is a number that has no code.
    pub fn from_lengths(lengths: &[u8]) -> Result<Table> {
        let lines = lengths.iter().enumerate().map(|(i, &l)| Line { low: i as i64, preflen: l, rangelen: 0, kind: Kind::Normal }).collect();
        Table::build(lines)
    }

    /// Read a value: `None` for the out-of-band value.
    pub fn decode(&self, r: &mut BitReader<'_>) -> Result<Option<i64>> {
        let mut code = 0u32;
        for len in 1..=MAX_PREFIX {
            code = (code << 1) | r.bit()?;
            let n = self.count.get(len).copied().unwrap_or(0);
            let first = self.first_code.get(len).copied().unwrap_or(0);
            if n > 0 && code >= first && code - first < n {
                let at = self.first_index.get(len).copied().unwrap_or(0) + (code - first) as usize;
                let line = self.order.get(at).and_then(|&i| self.lines.get(i as usize)).ok_or_else(|| bad("Huffman table"))?;
                return Ok(match line.kind {
                    Kind::Oob => None,
                    Kind::Normal => Some(line.low + i64::from(r.bits(u32::from(line.rangelen))?)),
                    Kind::Lower => Some(line.low - i64::from(r.bits(32)?)),
                    Kind::Upper => Some(line.low + i64::from(r.bits(32)?)),
                });
            }
        }
        Err(bad("a Huffman code that is in no table"))
    }

    /// Like [`Table::decode`] for a table that has no out-of-band value, or where out-of-band is an error.
    pub fn decode_value(&self, r: &mut BitReader<'_>) -> Result<i64> {
        self.decode(r)?.ok_or_else(|| bad("an out-of-band value where there must be a number"))
    }

    /// The bytes a table of `lines` lines holds: the lines, the order of the ones that have a code, the fixed arrays.
    pub fn bytes_for(lines: usize) -> u64 {
        (lines as u64).saturating_mul((std::mem::size_of::<Line>() + std::mem::size_of::<u32>()) as u64) + std::mem::size_of::<Table>() as u64 + 64
    }

    /// The bytes this table holds (for giving them back).
    pub fn held(&self) -> u64 {
        Table::bytes_for(self.lines.len())
    }

    /// The work of making a table of `lines` lines (counting and placing them), apart from reading the lines.
    pub fn build_work(lines: usize) -> f64 {
        lines as f64 * cost::JB2_TABLE_BUILD
    }

    /// A table segment, read from a throwaway meter (tests).
    #[cfg(test)]
    pub fn parse_segment(data: &[u8]) -> Result<Table> {
        let work = crate::render::work::Work::new();
        Table::parse_charged(&Ctx::new(&work), data)
    }

    /// A table segment (T.88 7.4.13 and B.2). Every line read and built and the memory the table
    /// holds are taken from `ctx` before they are spent, so that the table segments of a stream cost what they cost
    /// whether or not anything uses them; the caller gives the memory back (`held`) if it lets the table go.
    pub fn parse_charged(ctx: &Ctx<'_>, data: &[u8]) -> Result<Table> {
        let flags = *data.first().ok_or_else(|| bad("empty table segment"))?;
        let has_oob = flags & 1 != 0;
        let ps = u32::from((flags >> 1) & 7) + 1;
        let rs = u32::from((flags >> 4) & 7) + 1;
        let int = |i: usize| -> Result<i64> {
            let b: [u8; 4] = data.get(i..i + 4).and_then(|s| s.try_into().ok()).ok_or_else(|| bad("table segment too short"))?;
            Ok(i64::from(i32::from_be_bytes(b)))
        };
        let (low, high) = (int(1)?, int(5)?);
        if low >= high {
            return Err(bad("a table whose lowest value is not below its highest"));
        }
        let mut r = BitReader::new(data.get(9..).unwrap_or(&[]));
        let mut lines = Vec::new();
        let mut cur = low;
        while cur < high {
            // Every line costs at least two bits of the data, so the data bounds the loop; the cap bounds memory.
            if lines.len() >= MAX_TABLE_LINES {
                return Err(bad("a custom Huffman table has too many lines"));
            }
            ctx.charge(cost::JB2_TABLE_LINE)?;
            let preflen = r.bits(ps)? as u8;
            let rangelen = r.bits(rs)? as u8;
            if rangelen > 32 {
                return Err(bad("a Huffman table line with more than 32 range bits"));
            }
            lines.push(Line { low: cur, preflen, rangelen, kind: Kind::Normal });
            cur += 1i64 << rangelen;
        }
        let lower = r.bits(ps)? as u8;
        lines.push(Line { low: low - 1, preflen: lower, rangelen: 32, kind: Kind::Lower });
        let upper = r.bits(ps)? as u8;
        lines.push(Line { low: high, preflen: upper, rangelen: 32, kind: Kind::Upper });
        if has_oob {
            let oob = r.bits(ps)? as u8;
            lines.push(Line { low: 0, preflen: oob, rangelen: 0, kind: Kind::Oob });
        }
        lines.shrink_to_fit();
        ctx.charge(Table::build_work(lines.len()))?;
        let bytes = Table::bytes_for(lines.len());
        ctx.take_memory(bytes)?;
        Table::build(lines).inspect_err(|_| ctx.give_back(bytes))
    }

    /// The code of a value (the tests encode with the tables the decoder reads with): the code, its length, the extra
    /// bits and how many they are.
    #[cfg(test)]
    pub fn encode(&self, v: Option<i64>) -> Option<(u32, u32, u64, u32)> {
        for (idx, line) in self.lines.iter().enumerate() {
            if line.preflen == 0 {
                continue;
            }
            let hit = match (v, line.kind) {
                (None, Kind::Oob) => true,
                (Some(v), Kind::Normal) => v >= line.low && v < line.low + (1i64 << line.rangelen),
                (Some(v), Kind::Lower) => v <= line.low,
                (Some(v), Kind::Upper) => v >= line.low,
                _ => false,
            };
            if !hit {
                continue;
            }
            let len = usize::from(line.preflen);
            let pos = self.order.get(self.first_index[len]..)?.iter().position(|&i| i as usize == idx)?;
            let code = self.first_code[len] + pos as u32;
            let (extra, extra_len) = match (v, line.kind) {
                (Some(v), Kind::Normal) => ((v - line.low) as u64, u32::from(line.rangelen)),
                (Some(v), Kind::Lower) => ((line.low - v) as u64, 32),
                (Some(v), Kind::Upper) => ((v - line.low) as u64, 32),
                _ => (0, 0),
            };
            return Some((code, len as u32, extra, extra_len));
        }
        None
    }

    /// The table a selector field chooses: `options[sel]` is the number of a standard table, 0 for the next custom
    /// table of the segment, -1 for a selector that means nothing.
    pub fn select<'a>(custom: &mut std::slice::Iter<'a, Rc<Table>>, sel: u16, options: &[i8]) -> Result<&'a Table> {
        match options.get(usize::from(sel)).copied() {
            Some(n) if n > 0 => Table::standard(n as usize),
            Some(0) => custom.next().map(|t| &**t).ok_or_else(|| bad("a segment needs a custom Huffman table it was not given")),
            _ => Err(bad("a Huffman table selector that means nothing")),
        }
    }

    /// One of the standard tables B.1 to B.15.
    pub fn standard(n: usize) -> Result<&'static Table> {
        static TABLES: OnceLock<Vec<Table>> = OnceLock::new();
        let tables = TABLES.get_or_init(|| STANDARD.iter().filter_map(|t| Table::build(t.lines()).ok()).collect());
        n.checked_sub(1).and_then(|i| tables.get(i)).ok_or_else(|| bad("a standard Huffman table that does not exist"))
    }
}

/// Most lines a custom table may have.
const MAX_TABLE_LINES: usize = 1 << 16;

/// A standard table as printed in T.88 Annex B: lines of (lowest value, prefix length, range length), the lines for
/// values below and above those (lowest value of the upper line / highest of the lower, prefix length) when there
/// are any, and the prefix length of the out-of-band value.
struct Std {
    lines: &'static [(i32, u8, u8)],
    lower: Option<(i32, u8)>,
    upper: Option<(i32, u8)>,
    oob: Option<u8>,
}

impl Std {
    fn lines(&self) -> Vec<Line> {
        let mut v: Vec<Line> = self.lines.iter().map(|&(low, preflen, rangelen)| Line { low: i64::from(low), preflen, rangelen, kind: Kind::Normal }).collect();
        if let Some((low, preflen)) = self.lower {
            v.push(Line { low: i64::from(low), preflen, rangelen: 32, kind: Kind::Lower });
        }
        if let Some((low, preflen)) = self.upper {
            v.push(Line { low: i64::from(low), preflen, rangelen: 32, kind: Kind::Upper });
        }
        if let Some(preflen) = self.oob {
            v.push(Line { low: 0, preflen, rangelen: 0, kind: Kind::Oob });
        }
        v
    }
}

const STANDARD: [Std; 15] = [
    // B.1
    Std { lines: &[(0, 1, 4), (16, 2, 8), (272, 3, 16)], lower: None, upper: Some((65808, 3)), oob: None },
    // B.2
    Std { lines: &[(0, 1, 0), (1, 2, 0), (2, 3, 0), (3, 4, 3), (11, 5, 6)], lower: None, upper: Some((75, 6)), oob: Some(6) },
    // B.3
    Std { lines: &[(-256, 8, 8), (0, 1, 0), (1, 2, 0), (2, 3, 0), (3, 4, 3), (11, 5, 6)], lower: Some((-257, 8)), upper: Some((75, 7)), oob: Some(6) },
    // B.4
    Std { lines: &[(1, 1, 0), (2, 2, 0), (3, 3, 0), (4, 4, 3), (12, 5, 6)], lower: None, upper: Some((76, 5)), oob: None },
    // B.5
    Std { lines: &[(-255, 7, 8), (1, 1, 0), (2, 2, 0), (3, 3, 0), (4, 4, 3), (12, 5, 6)], lower: Some((-256, 7)), upper: Some((76, 6)), oob: None },
    // B.6
    Std {
        lines: &[(-2048, 5, 10), (-1024, 4, 9), (-512, 4, 8), (-256, 4, 7), (-128, 5, 6), (-64, 5, 5), (-32, 4, 5), (0, 2, 7), (128, 3, 7), (256, 3, 8), (512, 4, 9), (1024, 4, 10)],
        lower: Some((-2049, 6)),
        upper: Some((2048, 6)),
        oob: None,
    },
    // B.7
    Std {
        lines: &[(-1024, 4, 9), (-512, 3, 8), (-256, 4, 7), (-128, 5, 6), (-64, 5, 5), (-32, 4, 5), (0, 4, 5), (32, 5, 5), (64, 5, 6), (128, 4, 7), (256, 3, 8), (512, 3, 9), (1024, 3, 10)],
        lower: Some((-1025, 5)),
        upper: Some((2048, 5)),
        oob: None,
    },
    // B.8
    Std {
        lines: &[
            (-15, 8, 3), (-7, 9, 1), (-5, 8, 1), (-3, 9, 0), (-2, 7, 0), (-1, 4, 0), (0, 2, 1), (2, 5, 0), (3, 6, 0), (4, 3, 4), (20, 6, 1), (22, 4, 4), (38, 4, 5),
            (70, 5, 6), (134, 5, 7), (262, 6, 7), (390, 7, 8), (646, 6, 10),
        ],
        lower: Some((-16, 9)),
        upper: Some((1670, 9)),
        oob: Some(2),
    },
    // B.9
    Std {
        lines: &[
            (-31, 8, 4), (-15, 9, 2), (-11, 8, 2), (-7, 9, 1), (-5, 7, 1), (-3, 4, 1), (-1, 3, 1), (1, 3, 1), (3, 5, 1), (5, 6, 1), (7, 3, 5), (39, 6, 2), (43, 4, 5),
            (75, 4, 6), (139, 5, 7), (267, 5, 8), (523, 6, 8), (779, 7, 9), (1291, 6, 11),
        ],
        lower: Some((-32, 9)),
        upper: Some((3339, 9)),
        oob: Some(2),
    },
    // B.10
    Std {
        lines: &[
            (-21, 7, 4), (-5, 8, 0), (-4, 7, 0), (-3, 5, 0), (-2, 2, 2), (2, 5, 0), (3, 6, 0), (4, 7, 0), (5, 8, 0), (6, 2, 6), (70, 5, 5), (102, 6, 5), (134, 6, 6),
            (198, 6, 7), (326, 6, 8), (582, 6, 9), (1094, 6, 10), (2118, 7, 11),
        ],
        lower: Some((-22, 8)),
        upper: Some((4166, 8)),
        oob: Some(2),
    },
    // B.11
    Std {
        lines: &[(1, 1, 0), (2, 2, 1), (4, 4, 0), (5, 4, 1), (7, 5, 1), (9, 5, 2), (13, 6, 2), (17, 7, 2), (21, 7, 3), (29, 7, 4), (45, 7, 5), (77, 7, 6)],
        lower: None,
        upper: Some((141, 7)),
        oob: None,
    },
    // B.12
    Std {
        lines: &[(1, 1, 0), (2, 2, 0), (3, 3, 1), (5, 5, 0), (6, 5, 1), (8, 6, 1), (10, 7, 0), (11, 7, 1), (13, 7, 2), (17, 7, 3), (25, 7, 4), (41, 8, 5)],
        lower: None,
        upper: Some((73, 8)),
        oob: None,
    },
    // B.13
    Std {
        lines: &[(1, 1, 0), (2, 3, 0), (3, 4, 0), (4, 5, 0), (5, 4, 1), (7, 3, 3), (15, 6, 1), (17, 6, 2), (21, 6, 3), (29, 6, 4), (45, 6, 5), (77, 7, 6)],
        lower: None,
        upper: Some((141, 7)),
        oob: None,
    },
    // B.14
    Std { lines: &[(-2, 3, 0), (-1, 3, 0), (0, 1, 0), (1, 3, 0), (2, 3, 0)], lower: None, upper: None, oob: None },
    // B.15
    Std {
        lines: &[(-24, 7, 4), (-8, 6, 2), (-4, 5, 1), (-2, 4, 0), (-1, 3, 0), (0, 1, 0), (1, 3, 0), (2, 4, 0), (3, 5, 1), (5, 6, 2), (9, 7, 4)],
        lower: Some((-25, 7)),
        upper: Some((25, 7)),
        oob: None,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_standard_table_is_a_complete_prefix_code_over_contiguous_ranges() {
        for (n, t) in STANDARD.iter().enumerate() {
            // Kraft: the code space is used exactly.
            let all = t.lines();
            let space: f64 = all.iter().filter(|l| l.preflen > 0).map(|l| 0.5f64.powi(i32::from(l.preflen))).sum();
            assert!((space - 1.0).abs() < 1e-12, "B.{}: code space {space}", n + 1);
            // The ranges follow each other without gaps and the outer lines start where they end.
            let mut next = t.lines.first().map(|l| i64::from(l.0)).unwrap_or(0);
            for &(low, _, rangelen) in t.lines {
                assert_eq!(i64::from(low), next, "B.{}", n + 1);
                next += 1i64 << rangelen;
            }
            if let Some((low, _)) = t.upper {
                assert_eq!(i64::from(low), next, "B.{}", n + 1);
            }
            if let Some((low, _)) = t.lower {
                assert_eq!(i64::from(low), i64::from(t.lines.first().map(|l| l.0).unwrap_or(0)) - 1, "B.{}", n + 1);
            }
            assert!(Table::standard(n + 1).is_ok());
        }
    }

    fn read(table: &Table, bits: &str) -> Option<i64> {
        let mut bytes = vec![0u8; bits.len().div_ceil(8) + 8];
        for (i, c) in bits.bytes().enumerate() {
            if c == b'1' {
                bytes[i / 8] |= 0x80 >> (i % 8);
            }
        }
        table.decode(&mut BitReader::new(&bytes)).unwrap()
    }

    #[test]
    fn codes_of_table_b1_to_b3_are_the_ones_in_the_standard() {
        // B.1: 0, 10, 110, 111 with 4, 8, 16 and 32 more bits.
        let b1 = Table::standard(1).unwrap();
        assert_eq!(read(b1, "00101"), Some(5));
        assert_eq!(read(b1, "1000000001"), Some(17));
        // B.2: OOB is 111111, 0 is the code 0.
        let b2 = Table::standard(2).unwrap();
        assert_eq!(read(b2, "111111"), None);
        assert_eq!(read(b2, "0"), Some(0));
        assert_eq!(read(b2, "1110011"), Some(6));
        // B.3: -256..-1 is 11111110 + 8 bits, OOB 111110, the lower line 11111111.
        let b3 = Table::standard(3).unwrap();
        assert_eq!(read(b3, "1111111000000101"), Some(-251));
        assert_eq!(read(b3, "111110"), None);
        assert_eq!(read(b3, &format!("11111111{}", "0".repeat(31) + "1")), Some(-258));
    }

    #[test]
    fn a_custom_table_and_bad_ones() {
        // HTOOB 1, HTPS 3 bits, HTRS 3 bits: values 0..8 as one line of 3 range bits, lower, upper, OOB.
        // flags = 1 | (2 << 1) | (2 << 4) = 0x25; low 0, high 8; line: preflen 1, rangelen 3; lower 2; upper 3; oob 3.
        let mut seg = vec![0x25u8];
        seg.extend(0i32.to_be_bytes());
        seg.extend(8i32.to_be_bytes());
        // bits: 001 011 | 010 | 011 | 011  -> 001011 010 011 011
        seg.extend([0b0010_1101, 0b0011_0110]);
        let t = Table::parse_segment(&seg).unwrap();
        assert_eq!(read(&t, "0101"), Some(5));
        assert_eq!(read(&t, "111"), None);
        // Low not below high; too many codes for the prefix lengths; a prefix longer than 32.
        let mut s2 = vec![0x25u8];
        s2.extend(8i32.to_be_bytes());
        s2.extend(8i32.to_be_bytes());
        assert!(Table::parse_segment(&s2).is_err());
        assert!(Table::from_lengths(&[1, 1, 1]).is_err());
        assert!(Table::from_lengths(&[33]).is_err());
        assert!(Table::from_lengths(&[1, 2, 0, 3, 3]).is_ok());
    }

    #[test]
    fn a_table_that_asks_for_billions_of_lines_stops() {
        // low -2^31, high 2^31 - 1 with rangelen 0 lines: every line costs 2 bits; the data runs out first.
        let mut seg = vec![0x00u8];
        seg.extend(i32::MIN.to_be_bytes());
        seg.extend(i32::MAX.to_be_bytes());
        seg.extend([0x55u8; 64]);
        assert!(Table::parse_segment(&seg).is_err());
        // Plenty of data: the cap stops it.
        let mut seg = vec![0x00u8];
        seg.extend(i32::MIN.to_be_bytes());
        seg.extend(i32::MAX.to_be_bytes());
        seg.extend(vec![0x55u8; 1 << 20]);
        assert!(Table::parse_segment(&seg).is_err());
    }
}
